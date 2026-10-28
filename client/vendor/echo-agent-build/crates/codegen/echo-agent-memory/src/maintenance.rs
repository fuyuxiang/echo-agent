//! Session-owned embedding maintenance. Searches enqueue work without owning
//! its network lifetime; repeated requests for a database share one worker.
use std::{collections::HashMap, path::PathBuf, sync::Mutex};

use crate::{MemoryIndex, MemoryStorage, embedding::ApiEmbeddingProvider};
use tokio::{sync::watch, task::JoinHandle};

#[derive(Clone)]
struct Work {
    db: PathBuf,
    storage: MemoryStorage,
    provider: ApiEmbeddingProvider,
    dimensions: usize,
}

struct Worker {
    updates: watch::Sender<Work>,
    task: JoinHandle<()>,
}

#[derive(Default)]
struct State {
    stopping: bool,
    workers: HashMap<PathBuf, Worker>,
}

/// Shared by a session's tool, injection and recovery backends. Dropping the
/// final owner aborts workers; shutdown also joins cancellation before disk ACK.
#[derive(Default)]
pub struct EmbeddingMaintenance(Mutex<State>);

impl EmbeddingMaintenance {
    pub(crate) fn schedule(
        &self,
        db: PathBuf,
        storage: MemoryStorage,
        provider: ApiEmbeddingProvider,
        dimensions: usize,
    ) {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if state.stopping {
            return;
        }
        let work = Work {
            db: db.clone(),
            storage,
            provider,
            dimensions,
        };
        if let Some(worker) = state.workers.get(&db)
            && !worker.task.is_finished()
        {
            worker.updates.send_replace(work);
            return;
        }
        let (updates, receiver) = watch::channel(work);
        let task = tokio::spawn(run(receiver));
        state.workers.insert(db, Worker { updates, task });
    }

    pub async fn shutdown(&self) {
        let workers = {
            let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
            state.stopping = true;
            std::mem::take(&mut state.workers)
        };
        for worker in workers.values() {
            worker.task.abort();
        }
        for (_, worker) in workers {
            let _ = worker.task.await;
        }
    }
}

impl Drop for EmbeddingMaintenance {
    fn drop(&mut self) {
        let state = self.0.get_mut().unwrap_or_else(|error| error.into_inner());
        for worker in state.workers.values() {
            worker.task.abort();
        }
    }
}

async fn run(mut updates: watch::Receiver<Work>) {
    let mut failures = 0usize;
    loop {
        let work = updates.borrow_and_update().clone();
        if run_pass(&work).await {
            // Retry service failures even when no new chat/search arrives.
            // Never multiply retries for every foreground search.
            let seconds = [5, 15, 60, 300][failures.min(3)];
            failures = failures.saturating_add(1);
            tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
        } else {
            failures = 0;
            if updates.changed().await.is_err() {
                return;
            }
        }
    }
}

async fn run_pass(work: &Work) -> bool {
    if !work.storage.enabled() {
        return false;
    }
    // No SQLite connection/reference crosses an await. Preserve a newer
    // model's dimensions and let fingerprint checks reject stale responses.
    let candidates = (|| {
        let index = MemoryIndex::open_or_create_preserving_dimensions(
            &work.db,
            work.storage.clone(),
            Default::default(),
            work.dimensions,
        )?;
        index.embedding_candidates()
    })();
    let candidates = match candidates {
        Ok(candidates) => candidates,
        Err(error) => {
            tracing::warn!(target: crate::MEMORY_LOG_TARGET, %error, "memory maintenance index unavailable");
            return true;
        }
    };
    for batch in candidates.chunks(32) {
        let progress =
            crate::embedding_sync::embed_candidates(&work.storage, &work.provider, batch).await;
        let retry = progress.retry_needed;
        let committed = (|| {
            let index = MemoryIndex::open_or_create_preserving_dimensions(
                &work.db,
                work.storage.clone(),
                Default::default(),
                work.dimensions,
            )?;
            Ok::<_, rusqlite::Error>(crate::embedding_sync::commit_embeddings(
                &index,
                &work.provider,
                progress,
            ))
        })();
        if let Err(error) = committed {
            tracing::warn!(target: crate::MEMORY_LOG_TARGET, %error, "memory maintenance commit unavailable");
            return true;
        }
        if retry {
            return true;
        }
    }
    false
}
