//! Durable memory extraction checkpoints, independent of the model lifetime.
use echo_agent_sampling_types::ConversationItem;
#[derive(serde::Serialize, serde::Deserialize)]
pub struct PendingMemory {
    pub source_id: String,
    pub history: Vec<ConversationItem>,
    #[serde(default)]
    pub conversation_len: usize,
    #[serde(default)]
    pub queued_at: i64,
}

impl PendingMemory {
    pub fn identity(&self) -> std::io::Result<String> {
        let bytes = serde_json::to_vec(&(&self.source_id, self.conversation_len, &self.history))
            .map_err(std::io::Error::other)?;
        Ok(blake3::hash(&bytes).to_hex().to_string())
    }
}

/// Content-addressed jobs retain earlier unprocessed turns and deduplicate
/// repeated exit attempts. Never replace an in-flight job with a newer turn.
pub fn checkpoint(
    storage: &echo_agent_memory::MemoryStorage,
    job: &PendingMemory,
) -> std::io::Result<()> {
    let name = job.identity()?;
    let path = storage
        .workspace_dir()
        .join("pending")
        .join(format!("{name}.json"));
    let _guard = checkpoint_lock()
        .lock()
        .map_err(|_| std::io::Error::other("memory checkpoint lock poisoned"))?;
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(std::io::Error::other("invalid memory checkpoint file"));
            }
            let same = if metadata.len() <= MAX_CHECKPOINT_BYTES {
                let bytes = std::fs::read(&path)?;
                serde_json::from_slice::<PendingMemory>(&bytes)
                    .is_ok_and(|saved| saved.identity().ok().as_deref() == Some(name.as_str()))
            } else {
                false
            };
            if same {
                return Ok(());
            }
            quarantine(storage, &path)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    echo_agent_memory::records::write_json(&path, job)
}

const MAX_CHECKPOINT_BYTES: u64 = 8 * 1024 * 1024;

fn checkpoint_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    &LOCK
}

/// Move the original atomically into a private, uniquely reserved file. A
/// failed isolation remains a real save failure; no corrupt bytes are dropped.
/// Caller holds checkpoint_lock and has checked that path is a regular file.
fn quarantine(
    storage: &echo_agent_memory::MemoryStorage,
    path: &std::path::Path,
) -> std::io::Result<()> {
    let pending = storage.workspace_dir().join("pending");
    let directory = pending.join("quarantine");
    std::fs::create_dir_all(&directory)?;
    for dir in [&pending, &directory] {
        let metadata = std::fs::symlink_metadata(dir)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(std::io::Error::other("invalid memory checkpoint directory"));
        }
    }
    let target = tempfile::Builder::new()
        .prefix("checkpoint-")
        .suffix(".json")
        .tempfile_in(&directory)?;
    // Close the reservation before replacement so Windows can rename it too.
    let (reservation, target_path) = target.keep().map_err(|error| error.error)?;
    drop(reservation);
    if let Err(error) = std::fs::rename(path, &target_path) {
        let _ = std::fs::remove_file(&target_path);
        return Err(error);
    }
    #[cfg(unix)]
    {
        std::fs::File::open(&directory)?.sync_all()?;
        std::fs::File::open(&pending)?.sync_all()?;
    }
    Ok(())
}

/// Durable checkpoint traversal is independent of a model/provider failure.
pub async fn recover_pending_jobs<F, Fut>(
    storage: &echo_agent_memory::MemoryStorage,
    allowed: impl Fn() -> bool,
    mut run: F,
) where
    F: FnMut(PendingMemory) -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    use echo_agent_memory::records;
    records::recover_extraction_activity(storage);
    let mut attempted = std::collections::HashSet::new();
    for _pass in 0..3 {
        let entries = match std::fs::read_dir(storage.workspace_dir().join("pending")) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => {
                tracing::warn!(%error, "memory checkpoint directory unavailable");
                return;
            }
        };
        let mut processed = 0;
        for entry in entries.flatten() {
            if !allowed() {
                return;
            }
            let path = entry.path();
            if !entry.file_type().is_ok_and(|kind| kind.is_file())
                || path.extension().is_none_or(|v| v != "json")
            {
                continue;
            }
            let (bytes, identity, job) = {
                let Ok(_guard) = checkpoint_lock().lock() else {
                    return;
                };
                let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                    continue;
                };
                if !metadata.is_file() || metadata.file_type().is_symlink() {
                    continue;
                }
                let bytes = if metadata.len() <= MAX_CHECKPOINT_BYTES {
                    match std::fs::read(&path) {
                        Ok(bytes) => bytes,
                        Err(_) => continue,
                    }
                } else {
                    if let Err(error) = quarantine(storage, &path) {
                        tracing::warn!(%error, "could not isolate oversized memory checkpoint");
                    }
                    continue;
                };
                let identity = (path.clone(), blake3::hash(&bytes));
                if attempted.contains(&identity) {
                    continue;
                }
                let job = match serde_json::from_slice::<PendingMemory>(&bytes) {
                    Ok(job) => job,
                    Err(error) => {
                        attempted.insert(identity);
                        tracing::warn!(%error, path = %path.display(), "invalid memory checkpoint isolated");
                        if let Err(error) = quarantine(storage, &path) {
                            records::record_extraction_recovery_error(
                                storage,
                                format!("损坏待办未能隔离，原件已保留：{error}"),
                            );
                        }
                        continue;
                    }
                };
                (bytes, identity, job)
            };
            if processed >= 32 {
                break;
            }
            processed += 1;
            attempted.insert(identity);
            if run(job).await {
                let Ok(_guard) = checkpoint_lock().lock() else {
                    return;
                };
                if std::fs::read(&path).is_ok_and(|current| current == bytes) {
                    // Completion cannot delete a newer turn's replacement checkpoint.
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
}

#[cfg(test)]
mod pending_recovery_tests {
    use super::*;
    #[tokio::test]
    async fn corrupt_and_failed_jobs_do_not_starve_valid_or_newer_checkpoints() {
        let dir = tempfile::tempdir().unwrap();
        let storage = echo_agent_memory::MemoryStorage::new_flat(dir.path(), dir.path());
        let pending = dir.path().join("pending");
        std::fs::create_dir_all(&pending).unwrap();
        for n in 0..40 {
            std::fs::write(pending.join(format!("corrupt-{n}.json")), b"invalid").unwrap();
        }
        for source in ["failed", "success", "updated"] {
            std::fs::write(
                pending.join(format!("{source}.json")),
                serde_json::to_vec(&PendingMemory {
                    source_id: source.into(),
                    history: vec![],
                    conversation_len: 0,
                    queued_at: 1,
                })
                .unwrap(),
            )
            .unwrap();
        }
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        recover_pending_jobs(
            &storage,
            || true,
            |job| {
                seen.lock().unwrap().push(job.source_id.clone());
                if job.source_id == "updated" {
                    std::fs::write(pending.join("updated.json"), b"new checkpoint").unwrap();
                }
                async move { job.source_id != "failed" }
            },
        )
        .await;
        let seen = seen.lock().unwrap();
        assert_eq!(seen.iter().filter(|s| *s == "failed").count(), 1);
        assert!(seen.contains(&"success".to_string()));
        assert!(!pending.join("success.json").exists());
        assert!(pending.join("failed.json").exists());
        assert_eq!(
            std::fs::read_dir(pending.join("quarantine"))
                .unwrap()
                .count(),
            41
        );
        assert!(!pending.join("updated.json").exists());
        assert!(
            std::fs::read_dir(pending.join("quarantine"))
                .unwrap()
                .flatten()
                .any(|entry| std::fs::read(entry.path()).unwrap() == b"new checkpoint")
        );
    }
}
