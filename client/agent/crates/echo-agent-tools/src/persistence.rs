//! Background persistence for tool state.
//!
//! [`ResourcesPersistence`] persists `Resources` state (the new architecture).
//! Old `ToolStatePersistence` and `PersistenceLayer` have been deleted.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::AsyncWriteExt;

use crate::types::resources::Resources;

/// Background persistence for `Resources` state/params.
///
/// Same pattern as `ToolStatePersistence` — debounced background writes with
/// atomic rename. Takes a `serde_json::Value` from `Resources::serialize()`
/// and writes it to disk. On load, parses the JSON and feeds it to
/// `Resources::load_from()`.
///
/// This replaces the old `ToolStatePersistence` pipeline for the new
/// architecture. During migration both coexist; once all tools are migrated,
/// `ToolStatePersistence` will be deleted.
pub struct ResourcesPersistence {
    /// `None` means this handle reads and writes nothing.
    state_path: Option<PathBuf>,
    /// Channel to send serialized state to the background writer
    tx: tokio::sync::mpsc::UnboundedSender<ResourcesPersistenceCommand>,
}

#[cfg(test)]
pub(crate) type ControlledSave = (
    serde_json::Value,
    tokio::sync::oneshot::Sender<io::Result<()>>,
);

enum ResourcesPersistenceCommand {
    /// Write this serialized Resources value to disk
    Save(serde_json::Value),
    SaveAndFlush {
        snapshot: serde_json::Value,
        respond_to: tokio::sync::oneshot::Sender<io::Result<()>>,
    },
    /// Flush pending writes and notify when done
    Flush(tokio::sync::oneshot::Sender<()>),
}

impl ResourcesPersistence {
    /// A handle that reads and writes nothing.
    /// For tests, and for sessions with no state directory, which keep their resources in memory for the life of the session.
    pub fn noop() -> Self {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            state_path: None,
            tx,
        }
    }

    #[cfg(test)]
    pub(crate) fn controlled() -> (Self, tokio::sync::mpsc::UnboundedReceiver<ControlledSave>) {
        let (tx, mut commands) =
            tokio::sync::mpsc::unbounded_channel::<ResourcesPersistenceCommand>();
        let (observed_tx, observed_rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(command) = commands.recv().await {
                match command {
                    ResourcesPersistenceCommand::Save(_) => {}
                    ResourcesPersistenceCommand::SaveAndFlush {
                        snapshot,
                        respond_to,
                    } => {
                        let _ = observed_tx.send((snapshot, respond_to));
                    }
                    ResourcesPersistenceCommand::Flush(done) => {
                        let _ = done.send(());
                    }
                }
            }
        });
        (
            Self {
                state_path: Some(PathBuf::from("/dev/null")),
                tx,
            },
            observed_rx,
        )
    }

    /// Create a new persistence handle and spawn the background writer task.
    pub fn new(state_path: PathBuf) -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let writer_path = state_path.clone();

        tokio::spawn(async move {
            Self::writer_loop(rx, writer_path).await;
        });

        Self {
            state_path: Some(state_path),
            tx,
        }
    }

    /// Load existing Resources state from disk, if the file exists.
    ///
    /// Reads the JSON, parses it into the nested `HashMap<String, HashMap<String, Value>>`
    /// shape that `Resources::load_from()` expects, and applies it to the given resources.
    ///
    /// Returns `true` if state was loaded, `false` if there is no path, no file, or a parse error.
    pub fn load(&self, resources: &mut Resources) -> bool {
        let Some(state_path) = self.state_path.as_ref() else {
            return false;
        };

        let json = match std::fs::read_to_string(state_path) {
            Ok(s) => s,
            Err(_) => return false,
        };

        let top: serde_json::Value = match serde_json::from_str(&json) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("Failed to parse resources state from {state_path:?}: {e}");
                return false;
            }
        };

        let data = match Self::value_to_nested_map(top) {
            Some(m) => m,
            None => {
                tracing::warn!("Resources state file {state_path:?} has unexpected shape");
                return false;
            }
        };

        resources.load_from(data);
        true
    }

    /// Save the current Resources state (non-blocking).
    /// Sends a serialized snapshot to the background writer.
    pub fn save(&self, resources: &Resources) {
        if self.state_path.is_none() {
            return;
        }
        let snapshot = resources.serialize();
        let _ = self.tx.send(ResourcesPersistenceCommand::Save(snapshot));
    }

    /// Replace pending snapshots, write this snapshot, and acknowledge the result.
    pub fn enqueue_save_and_flush(
        &self,
        snapshot: serde_json::Value,
    ) -> io::Result<tokio::sync::oneshot::Receiver<io::Result<()>>> {
        if self.state_path.is_none() {
            let (respond_to, response) = tokio::sync::oneshot::channel();
            let _ = respond_to.send(Ok(()));
            return Ok(response);
        }
        let (respond_to, response) = tokio::sync::oneshot::channel();
        self.tx
            .send(ResourcesPersistenceCommand::SaveAndFlush {
                snapshot,
                respond_to,
            })
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "resources persistence writer stopped",
                )
            })?;
        Ok(response)
    }

    /// Await an acknowledgement returned by [`Self::enqueue_save_and_flush`].
    pub async fn await_save_and_flush(
        response: tokio::sync::oneshot::Receiver<io::Result<()>>,
    ) -> io::Result<()> {
        response.await.map_err(|_| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "resources persistence writer dropped acknowledgement",
            )
        })?
    }

    /// Replace pending snapshots, write this snapshot, and await the result.
    pub async fn save_and_flush(&self, snapshot: serde_json::Value) -> io::Result<()> {
        Self::await_save_and_flush(self.enqueue_save_and_flush(snapshot)?).await
    }

    /// `None` when this handle writes nothing.
    pub fn state_path(&self) -> Option<&std::path::Path> {
        self.state_path.as_deref()
    }

    /// Flush pending writes. Call on graceful shutdown.
    pub async fn flush(&self) {
        if self.state_path.is_none() {
            return;
        }
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let _ = self.tx.send(ResourcesPersistenceCommand::Flush(done_tx));
        let _ = done_rx.await;
    }

    /// Convert the `serde_json::Value` (from serialize()) into the nested
    /// HashMap structure that `load_from()` expects.
    fn value_to_nested_map(
        val: serde_json::Value,
    ) -> Option<
        std::collections::HashMap<String, std::collections::HashMap<String, serde_json::Value>>,
    > {
        let top = val.as_object()?;
        let mut result = std::collections::HashMap::new();
        for (cat_key, cat_val) in top {
            let inner_obj = cat_val.as_object()?;
            let mut inner = std::collections::HashMap::new();
            for (k, v) in inner_obj {
                inner.insert(k.clone(), v.clone());
            }
            result.insert(cat_key.clone(), inner);
        }
        Some(result)
    }

    async fn writer_loop(
        mut rx: tokio::sync::mpsc::UnboundedReceiver<ResourcesPersistenceCommand>,
        state_path: PathBuf,
    ) {
        let mut pending: Option<serde_json::Value> = None;
        let mut debounce = tokio::time::interval(Duration::from_millis(500));
        debounce.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                cmd = rx.recv() => {
                    match cmd {
                        Some(ResourcesPersistenceCommand::Save(snapshot)) => {
                            pending = Some(snapshot);
                        }
                        Some(ResourcesPersistenceCommand::SaveAndFlush {
                            snapshot,
                            respond_to,
                        }) => {
                            pending = None;
                            let result = Self::write_json_durable(&state_path, &snapshot).await;
                            let _ = respond_to.send(result);
                        }
                        Some(ResourcesPersistenceCommand::Flush(done)) => {
                            if let Some(snapshot) = pending.take()
                                && let Err(error) = Self::write_json(&state_path, &snapshot).await
                            {
                                tracing::warn!(
                                    ?error,
                                    ?state_path,
                                    "Failed to flush resources state"
                                );
                            }
                            let _ = done.send(());
                        }
                        None => {
                            if let Some(snapshot) = pending.take()
                                && let Err(error) = Self::write_json(&state_path, &snapshot).await
                            {
                                tracing::warn!(
                                    ?error,
                                    ?state_path,
                                    "Failed to flush resources state"
                                );
                            }
                            break;
                        }
                    }
                }
                _ = debounce.tick() => {
                    if let Some(snapshot) = pending.take()
                        && let Err(error) = Self::write_json(&state_path, &snapshot).await
                    {
                        tracing::warn!(
                            ?error,
                            ?state_path,
                            "Failed to save resources state"
                        );
                    }
                }
            }
        }
    }

    async fn write_json(path: &Path, value: &serde_json::Value) -> io::Result<()> {
        let (tmp_path, json) = Self::prepare_write(path, value)?;
        tokio::fs::write(&tmp_path, json).await?;
        Self::replace_state_path(path, &tmp_path).await
    }

    async fn write_json_durable(path: &Path, value: &serde_json::Value) -> io::Result<()> {
        let (tmp_path, json) = Self::prepare_write(path, value)?;
        let result = async {
            let mut file = tokio::fs::File::create(&tmp_path).await?;
            file.write_all(&json).await?;
            file.sync_all().await?;
            drop(file);
            Self::publish_durable(path, &tmp_path).await
        }
        .await;
        Self::cleanup_temp_on_error(&tmp_path, result).await
    }

    async fn cleanup_temp_on_error(tmp_path: &Path, result: io::Result<()>) -> io::Result<()> {
        if result.is_err() {
            let _ = tokio::fs::remove_file(tmp_path).await;
        }
        result
    }

    fn prepare_write(path: &Path, value: &serde_json::Value) -> io::Result<(PathBuf, Vec<u8>)> {
        let json = serde_json::to_vec_pretty(value)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        Ok((path.with_extension("json.tmp"), json))
    }

    async fn replace_state_path(path: &Path, tmp_path: &Path) -> io::Result<()> {
        if path.is_dir() {
            tracing::warn!(
                "Resources state path {:?} is a directory — removing before write",
                path
            );
            tokio::fs::remove_dir_all(path).await?;
        }
        tokio::fs::rename(tmp_path, path).await
    }

    #[cfg(not(windows))]
    async fn publish_durable(path: &Path, tmp_path: &Path) -> io::Result<()> {
        // A bare filename has an empty parent, so the write would land in the server's own directory, shared by every session.
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "resources state has no parent directory",
                )
            })?;

        Self::replace_state_path(path, tmp_path).await?;
        tokio::fs::File::open(parent).await?.sync_all().await
    }

    #[cfg(windows)]
    async fn publish_durable(path: &Path, tmp_path: &Path) -> io::Result<()> {
        use windows::Win32::Storage::FileSystem::MoveFileExW;
        use windows::core::PCWSTR;
        if path.is_dir() {
            tokio::fs::remove_dir_all(path).await?;
        }
        let from = Self::windows_extended_path(tmp_path)?;
        let to = Self::windows_extended_path(path)?;
        unsafe {
            MoveFileExW(
                PCWSTR(from.as_ptr()),
                PCWSTR(to.as_ptr()),
                Self::WINDOWS_MOVE_FLAGS,
            )
        }
        .map_err(io::Error::other)
    }

    #[cfg(windows)]
    const WINDOWS_MOVE_FLAGS: windows::Win32::Storage::FileSystem::MOVE_FILE_FLAGS =
        windows::Win32::Storage::FileSystem::MOVE_FILE_FLAGS(1 | 8);

    #[cfg(windows)]
    fn windows_extended_path(path: &Path) -> io::Result<Vec<u16>> {
        use std::os::windows::ffi::OsStrExt;
        let path = std::path::absolute(path)?;
        let mut wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
        if wide.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path contains NUL",
            ));
        }
        let unc = wide.starts_with(&[92, 92]);
        let mut result = if unc { r"\\?\UNC\" } else { r"\\?\" }
            .encode_utf16()
            .collect::<Vec<_>>();
        if unc {
            wide.drain(..2);
        }
        result.extend(wide);
        result.push(0);
        Ok(result)
    }
}

// Old `PersistenceLayer` / `PersistenceRunner` deleted.
// ToolState persistence replaced by Resources persistence via `ResourcesPersistence`.

#[cfg(test)]
mod tests {
    use super::*;

    // ResourcesPersistence tests
    // -----------------------------------------------------------------------

    use crate::types::resources::{Resources, State, WebCitationCounter};

    #[tokio::test]
    async fn resources_save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("resources_state.json");

        let persistence = ResourcesPersistence::new(state_path);

        // Build resources with registered state types
        let mut resources = Resources::new();
        resources.register_state::<WebCitationCounter>();

        // Populate WebCitationCounter
        {
            let counter = resources.get_or_default::<State<WebCitationCounter>>();
            counter.counter = 7;
        }

        // Save and flush
        persistence.save(&resources);
        persistence.flush().await;

        // Load into fresh resources (with same registrations)
        let mut restored = Resources::new();
        restored.register_state::<WebCitationCounter>();
        assert!(persistence.load(&mut restored));

        // Verify WebCitationCounter roundtripped
        let counter = restored.get::<State<WebCitationCounter>>().unwrap();
        assert_eq!(counter.counter, 7);
    }

    #[tokio::test]
    async fn resources_load_returns_false_when_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("nonexistent.json");

        let persistence = ResourcesPersistence::new(state_path);
        let mut resources = Resources::new();
        assert!(!persistence.load(&mut resources));
    }

    #[tokio::test]
    async fn resources_load_returns_false_on_corrupt_json() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("resources_state.json");
        std::fs::write(&state_path, "{ this is not valid json }").unwrap();

        let persistence = ResourcesPersistence::new(state_path);
        let mut resources = Resources::new();
        assert!(!persistence.load(&mut resources));
    }

    /// Atomic-rename guarantee: a concurrent reader hammering the path while
    /// the writer streams 200 snapshots must never observe torn JSON.
    #[tokio::test]
    async fn writer_atomic_rename_never_exposes_partial_json() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("resources_state.json");
        let persistence = ResourcesPersistence::new(state_path.clone());

        let mut resources = Resources::new();
        resources.register_state::<WebCitationCounter>();

        let done = std::sync::Arc::new(AtomicBool::new(false));
        let reader_done = done.clone();
        let reader_path = state_path.clone();
        let reader = tokio::spawn(async move {
            while !reader_done.load(Ordering::Relaxed) {
                if let Ok(s) = tokio::fs::read_to_string(&reader_path).await {
                    assert!(
                        serde_json::from_str::<serde_json::Value>(&s).is_ok(),
                        "reader observed a torn/partial write (atomic-rename violated): {s:?}"
                    );
                }
                tokio::task::yield_now().await;
            }
        });

        for i in 0..200u64 {
            {
                let counter = resources.get_or_default::<State<WebCitationCounter>>();
                counter.counter = i as u32;
            }
            persistence.save(&resources);
            persistence.flush().await;
        }

        done.store(true, Ordering::Relaxed);
        reader.await.unwrap();

        // Final state is intact and reflects the last write.
        let content = std::fs::read_to_string(&state_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(parsed["state"]["echo_agent_build.WebCitation"].is_object());
    }

    #[tokio::test]
    async fn resources_flush_writes_pending() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("resources_state.json");

        let persistence = ResourcesPersistence::new(state_path.clone());

        let mut resources = Resources::new();
        resources.register_state::<WebCitationCounter>();
        {
            let counter = resources.get_or_default::<State<WebCitationCounter>>();
            counter.counter = 42;
        }

        persistence.save(&resources);
        persistence.flush().await;

        // File should exist with correct structure
        assert!(state_path.exists());
        let content = std::fs::read_to_string(&state_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        // Should have "state" category with "echo_agent_build.WebCitation" key
        assert!(parsed["state"]["echo_agent_build.WebCitation"].is_object());
    }

    #[tokio::test]
    async fn save_and_flush_supersedes_older_pending_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("resources_state.json");
        let persistence = ResourcesPersistence::new(state_path.clone());
        persistence.flush().await;

        let mut resources = Resources::new();
        resources.register_state::<WebCitationCounter>();
        resources
            .get_or_default::<State<WebCitationCounter>>()
            .counter = 1;
        persistence.save(&resources);

        resources
            .get_or_default::<State<WebCitationCounter>>()
            .counter = 2;
        persistence
            .save_and_flush(resources.serialize())
            .await
            .unwrap();
        persistence.flush().await;

        let content = std::fs::read_to_string(state_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(
            parsed["state"]["echo_agent_build.WebCitation"]["counter"],
            2
        );
    }

    #[tokio::test]
    async fn save_and_flush_error_can_be_retried() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("missing");
        let state_path = parent.join("resources_state.json");
        let persistence = ResourcesPersistence::new(state_path.clone());

        let mut resources = Resources::new();
        resources.register_state::<WebCitationCounter>();
        resources
            .get_or_default::<State<WebCitationCounter>>()
            .counter = 7;
        let snapshot = resources.serialize();

        assert!(persistence.save_and_flush(snapshot.clone()).await.is_err());

        std::fs::create_dir(parent).unwrap();
        persistence.save_and_flush(snapshot).await.unwrap();

        let content = std::fs::read_to_string(state_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(
            parsed["state"]["echo_agent_build.WebCitation"]["counter"],
            7
        );
    }

    #[tokio::test]
    async fn enqueued_acknowledged_save_precedes_a_newer_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("resources_state.json");
        let persistence = ResourcesPersistence::new(state_path.clone());
        persistence.flush().await;

        let mut resources = Resources::new();
        resources.register_state::<WebCitationCounter>();
        resources
            .get_or_default::<State<WebCitationCounter>>()
            .counter = 1;
        let acknowledgement = persistence
            .enqueue_save_and_flush(resources.serialize())
            .unwrap();

        resources
            .get_or_default::<State<WebCitationCounter>>()
            .counter = 2;
        persistence.save(&resources);

        ResourcesPersistence::await_save_and_flush(acknowledgement)
            .await
            .unwrap();
        persistence.flush().await;

        let content = std::fs::read_to_string(state_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(
            parsed["state"]["echo_agent_build.WebCitation"]["counter"],
            2
        );
    }

    #[tokio::test]
    async fn post_create_failure_cleans_temp_and_allows_retry() {
        let dir = tempfile::tempdir().unwrap();
        let tmp_path = dir.path().join("resources_state.json.tmp");
        std::fs::write(&tmp_path, "partial").unwrap();
        let error = io::Error::other("publish failed");
        let returned = ResourcesPersistence::cleanup_temp_on_error(&tmp_path, Err(error))
            .await
            .unwrap_err();
        assert_eq!(returned.to_string(), "publish failed");
        assert!(!tmp_path.exists());
        std::fs::write(&tmp_path, "retry").unwrap();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_publish_supports_long_paths_and_legacy_directory() {
        use windows::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        };
        assert_eq!(
            ResourcesPersistence::WINDOWS_MOVE_FLAGS.0,
            MOVEFILE_REPLACE_EXISTING.0 | MOVEFILE_WRITE_THROUGH.0
        );
        let long = PathBuf::from(format!(r"C:\{}", "long\\".repeat(60)));
        let wide = ResourcesPersistence::windows_extended_path(&long).unwrap();
        assert!(wide.len() > 260 && String::from_utf16_lossy(&wide).starts_with(r"\\?\"));
        let unc =
            ResourcesPersistence::windows_extended_path(Path::new(r"\\server\share\state.json"))
                .unwrap();
        assert!(String::from_utf16_lossy(&unc).starts_with(r"\\?\UNC\"));
        assert!(ResourcesPersistence::windows_extended_path(Path::new("bad\0path")).is_err());

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("state.json");
        std::fs::create_dir(&target).unwrap();
        let temp = dir.path().join("state.json.tmp");
        std::fs::write(&temp, "new").unwrap();
        ResourcesPersistence::publish_durable(&target, &temp)
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(target).unwrap(), "new");
    }

    /// A bare filename would land in the server's own directory, shared by every session.
    #[cfg(not(windows))]
    #[tokio::test]
    async fn durable_write_refuses_a_path_with_no_directory() {
        let error = ResourcesPersistence::publish_durable(
            Path::new("resources_state.json"),
            Path::new("resources_state.json.tmp"),
        )
        .await
        .expect_err("a bare filename must not publish");

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn noop_persistence_neither_reads_nor_writes() {
        let noop = ResourcesPersistence::noop();

        noop.save_and_flush(serde_json::json!({"state": {}}))
            .await
            .unwrap();

        assert!(noop.state_path().is_none());
        assert!(!noop.load(&mut Resources::new()));
    }
}
