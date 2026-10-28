//! `SessionMemory` — memory subsystem state for the session actor.
//!
//! Groups storage, flush config, injection state, and telemetry counters
//! that were previously scattered across 15 fields on `SessionActor`.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64};

/// Memory subsystem state for a session.
pub(crate) struct SessionMemory {
    pub jobs: crate::session::memory::background::MemoryJobs,
    /// Memory storage handle for writing flush output (None when memory disabled).
    /// Wrapped in `RefCell` to allow `/memory on|off` toggle from `&Arc<SessionActor>`.
    pub storage: RefCell<Option<crate::session::memory::MemoryStorage>>,
    /// Whether to write a session summary to memory on session end.
    pub save_on_end: bool,
    /// Shared params for building a fully-configured memory backend.
    /// `None` when memory is disabled.
    pub backend_params: Option<crate::session::memory::MemoryBackendParams>,
    /// First-turn memory injection behavior resolved from local + remote config.
    pub initial_injection_config: crate::config::MemoryInitialInjectionConfig,
    /// Per-process latch: the first-turn injection decision already ran in
    /// this session segment. Cross-segment idempotency comes from
    /// `conversation_has_memory_context`, not this flag.
    pub context_injected: AtomicBool,
    /// Reuse search results within one user turn; validate against live files before use.
    pub cached_context: RefCell<
        Option<(
            String,
            Vec<echo_agent_tools::types::memory_backend::MemorySearchResult>,
        )>,
    >,
    /// Memory flush configuration (from MemoryConfig).
    pub flush_config: crate::config::MemoryFlushConfig,
    /// When `true`, auto-compact checks are suppressed during memory flush.
    pub is_flushing: AtomicBool,
    /// The compaction count at which the last flush ran (once-per-cycle guard).
    pub last_flush_compaction: AtomicU64,
    /// Number of flushes executed in this session.
    pub flush_count: AtomicU64,
    /// Content from the most recent successful flush, used for delta prompts.
    /// Wrapped in `RefCell` because `SessionActor` is single-threaded (LocalSet).
    pub last_flush_content: RefCell<Option<String>>,
    /// Number of successful flushes.
    pub flush_success_count: AtomicU64,
    /// Number of failed flushes.
    pub flush_error_count: AtomicU64,
    /// Counts model-initiated `memory_search` tool calls.
    /// Wrapped in `RefCell` to allow `/memory on|off` toggle from `&Arc<SessionActor>`.
    pub search_counter: RefCell<Option<Arc<AtomicU64>>>,
    /// Counts first-turn memory context injections.
    pub injection_count: AtomicU64,
    /// Counts post-compaction memory re-injection searches.
    pub compaction_recovery_count: AtomicU64,
    /// Total memory chunks added across all sources.
    pub chunks_added: Arc<AtomicU64>,
    /// autoDream consolidation config.
    pub dream_config: crate::config::MemoryDreamConfig,
    /// Number of dream consolidations attempted.
    pub dream_count: AtomicU64,
    /// Number of successful dream consolidations.
    pub dream_success_count: AtomicU64,
    /// Number of failed dream consolidations.
    pub dream_error_count: AtomicU64,
}

impl SessionMemory {
    pub(crate) async fn stop_jobs(&self) {
        self.jobs.stop().await;
        if let Some(params) = &self.backend_params {
            params.embedding_maintenance.shutdown().await;
        }
    }

    /// Whether memory is enabled for this session.
    pub(crate) fn is_enabled(&self) -> bool {
        !self.jobs.is_stopping()
            && self
                .storage
                .borrow()
                .as_ref()
                .is_some_and(|storage| storage.enabled())
    }

    pub(crate) fn setting(
        &self,
        section: &str,
        child: Option<&str>,
        key: &str,
        fallback: bool,
    ) -> bool {
        self.storage
            .borrow()
            .as_ref()
            .is_some_and(|storage| storage.setting(section, child, key, fallback))
    }

    /// Clone the storage out of the `RefCell`, dropping the borrow immediately.
    pub(crate) fn storage(&self) -> Option<crate::session::memory::MemoryStorage> {
        self.storage.borrow().clone()
    }

    /// Attempt to acquire the flush lock. Returns `true` if acquired,
    /// `false` if another flush is already in progress.
    pub(crate) fn try_acquire_flush_lock(&self) -> bool {
        self.is_flushing
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
            )
            .is_ok()
    }

    /// Release the flush lock.
    pub(crate) fn release_flush_lock(&self) {
        self.is_flushing
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }

    /// Record a flush result and increment the appropriate counter.
    ///
    /// Matches the original three-way logic: "written" increments success,
    /// "error" increments error, anything else ("nothing_to_store", "rejected")
    /// increments only the total flush count.
    pub(crate) fn record_flush_result(&self, outcome: &str) {
        self.flush_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        match outcome {
            "written" => {
                self.flush_success_count
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            "error" => {
                self.flush_error_count
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            _ => {}
        }
    }

    /// Record a dream consolidation result.
    pub(crate) fn record_dream_result(&self, success: bool) {
        use std::sync::atomic::Ordering::Relaxed;
        self.dream_count.fetch_add(1, Relaxed);
        if success {
            self.dream_success_count.fetch_add(1, Relaxed);
        } else {
            self.dream_error_count.fetch_add(1, Relaxed);
        }
    }

    /// Record a neutral dream outcome (nothing to consolidate / skipped).
    pub(crate) fn record_dream_neutral(&self) {
        self.dream_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Open (or create) the memory index for the current workspace.
    ///
    /// Shared helper that extracts embed dimensions from `backend_params`
    /// and opens the index at `<workspace_dir>/index.sqlite`.
    pub(crate) fn open_index(
        &self,
        storage: &crate::session::memory::MemoryStorage,
    ) -> Option<crate::session::memory::MemoryIndex> {
        let embed_dims = self
            .backend_params
            .as_ref()
            .and_then(|p| p.embed_config.as_ref())
            .map_or(1024, |c| c.dimensions);
        let db_path = storage.workspace_dir().join("index.sqlite");
        crate::session::memory::MemoryIndex::open_or_create(
            &db_path,
            storage.clone(),
            Default::default(),
            embed_dims,
        )
        .ok()
    }

    /// Reindex a file and embed new chunks when embedding is configured.
    pub(crate) async fn reindex_and_embed(&self, path: &std::path::Path, source: &str) {
        if !self.is_enabled() {
            return;
        }
        let Some(storage) = self.storage.borrow().clone() else {
            return;
        };
        if let Some(mut index) = self.open_index(&storage) {
            let _ = index.reindex_file(path, source);
            if let Some(ref params) = self.backend_params
                && let Some(provider) = params.make_embedding_provider().await
            {
                crate::session::memory::embed_missing_chunks(&index, &provider).await;
            }
        }
    }

    /// Remove chunks for the given file paths from the search index.
    ///
    /// Used after dream consolidation deletes processed session files so
    /// that stale chunks don't linger in the index. Best-effort: errors
    /// are logged but don't propagate.
    pub(crate) fn delete_paths_from_index(&self, paths: &[std::path::PathBuf]) {
        if paths.is_empty() {
            return;
        }
        let Some(storage) = self.storage.borrow().clone() else {
            return;
        };
        if let Some(mut index) = self.open_index(&storage) {
            let mut total_removed = 0usize;
            for path in paths {
                match index.delete_path(path) {
                    Ok(n) => total_removed += n,
                    Err(e) => {
                        tracing::warn!(
                            target: echo_agent_telemetry::memory_log::TARGET,
                            path = %path.display(),
                            error = %e,
                            "DREAM_CLEANUP: failed to remove chunks from index"
                        );
                    }
                }
            }
            if total_removed > 0 {
                tracing::info!(
                    target: echo_agent_telemetry::memory_log::TARGET,
                    chunks_removed = total_removed,
                    files = paths.len(),
                    "DREAM_CLEANUP: removed stale chunks from index"
                );
            }
        }
    }

    /// Collect telemetry counters for session-end summary.
    pub(crate) fn telemetry_snapshot(&self) -> MemoryTelemetry {
        use std::sync::atomic::Ordering::Relaxed;
        MemoryTelemetry {
            flush_count: self.flush_count.load(Relaxed),
            flush_success_count: self.flush_success_count.load(Relaxed),
            flush_error_count: self.flush_error_count.load(Relaxed),
            tool_search_count: self
                .search_counter
                .borrow()
                .as_ref()
                .map_or(0, |c| c.load(Relaxed)),
            injection_count: self.injection_count.load(Relaxed),
            compaction_recovery_count: self.compaction_recovery_count.load(Relaxed),
            chunks_added: self.chunks_added.load(Relaxed),
            dream_count: self.dream_count.load(Relaxed),
            dream_success_count: self.dream_success_count.load(Relaxed),
            dream_error_count: self.dream_error_count.load(Relaxed),
        }
    }
}

/// Snapshot of memory telemetry counters for session-end logging.
pub(crate) struct MemoryTelemetry {
    pub flush_count: u64,
    pub flush_success_count: u64,
    pub flush_error_count: u64,
    pub tool_search_count: u64,
    pub injection_count: u64,
    pub compaction_recovery_count: u64,
    pub chunks_added: u64,
    pub dream_count: u64,
    pub dream_success_count: u64,
    pub dream_error_count: u64,
}

#[derive(Debug)]
pub(crate) struct MemoryRequestPolicy {
    pub storage: crate::session::memory::MemoryStorage,
    pub extraction: bool,
}
impl echo_agent_sampling_types::ConversationRequestPreflight for MemoryRequestPolicy {
    fn apply(
        &self,
        request: &mut echo_agent_sampling_types::ConversationRequest,
    ) -> Result<(), &'static str> {
        let enabled = self.storage.enabled();
        if self.extraction && !enabled {
            return Err("Memory was disabled before the request could be sent");
        }
        for item in &mut request.items {
            if let echo_agent_sampling_types::ConversationItem::System(system) = item {
                let mut cursor = 0;
                while let Some(offset) = system.content[cursor..].find("<memory-context>") {
                    let start = cursor + offset;
                    let end = system.content[start..]
                        // Treat embedded closing markers as memory data too.
                        // An early marker must not leave private text outside
                        // the span removed when memory is disabled/forgotten.
                        .rfind("</memory-context>")
                        .map(|offset| start + offset + "</memory-context>".len())
                        .unwrap_or(system.content.len());
                    if enabled
                        && echo_agent_memory::records::permitted(
                            &self.storage,
                            &system.content[start..end],
                        )
                        .unwrap_or(false)
                    {
                        cursor = end;
                    } else {
                        system.content =
                            format!("{}{}", &system.content[..start], &system.content[end..])
                                .into();
                        cursor = start;
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod live_policy_tests {
    use super::*;
    use echo_agent_sampling_types::{
        ConversationItem, ConversationRequest, ConversationRequestPreflight,
    };
    #[test]
    fn disabled_personal_memory_is_removed_at_submission_without_affecting_independent_roots() {
        if std::env::var_os("ECHO_MEMORY_PREFLIGHT_TEST").is_none() {
            let dir = tempfile::tempdir().unwrap();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "session::memory_state::live_policy_tests::disabled_personal_memory_is_removed_at_submission_without_affecting_independent_roots"])
                .env("ECHO_AGENT_HOME",dir.path()).env("ECHO_MEMORY_PREFLIGHT_TEST","1").status().unwrap();
            assert!(status.success());
            return;
        }
        let home = std::path::PathBuf::from(std::env::var_os("ECHO_AGENT_HOME").unwrap());
        let storage = crate::session::memory::MemoryStorage::new(
            std::path::Path::new("/fixture"),
            Some(&home.join("memory")),
        );
        let policy = MemoryRequestPolicy {
            storage: storage.clone(),
            extraction: false,
        };
        std::fs::write(home.join("config.toml"), "[memory]\nenabled = true\n").unwrap();
        let original = ConversationRequest {
            items: vec![ConversationItem::system(
                "instructions<memory-context>private fact</memory-context>keep",
            )],
            ..Default::default()
        };
        assert!(
            storage.enabled(),
            "enabled config must be visible to the request policy"
        );
        assert!(echo_agent_memory::records::permitted(&storage, "private fact").unwrap());
        let mut initial = original.clone();
        policy.apply(&mut initial).unwrap();
        assert!(
            initial.items[0].text_content().contains("private fact"),
            "enabled request: {:?}",
            initial.items
        );
        let record = echo_agent_memory::records::remember(
            &storage,
            echo_agent_memory::records::RecordInput {
                scope: "global".into(),
                kind: "user".into(),
                key: "privacy".into(),
                content: "private fact".into(),
                ..Default::default()
            },
            None,
            false,
        )
        .unwrap()
        .unwrap();
        echo_agent_memory::records::retire(
            &storage,
            "global",
            &record.id,
            Some(record.revision),
            false,
        )
        .unwrap();
        let mut forgotten = original.clone();
        policy.apply(&mut forgotten).unwrap();
        assert_eq!(forgotten.items[0].text_content(), "instructionskeep");
        std::fs::write(home.join("config.toml"), "[memory]\nenabled = false\n").unwrap();
        let mut retry = original.clone();
        policy.apply(&mut retry).unwrap();
        assert_eq!(retry.items[0].text_content(), "instructionskeep");
        let mut embedded_marker = ConversationRequest {
            items: vec![ConversationItem::system(
                "before<memory-context>first</memory-context>private tail</memory-context>after",
            )],
            ..Default::default()
        };
        policy.apply(&mut embedded_marker).unwrap();
        assert_eq!(embedded_marker.items[0].text_content(), "beforeafter");
        let extraction = MemoryRequestPolicy {
            storage: storage.clone(),
            extraction: true,
        };
        assert!(extraction.apply(&mut original.clone()).is_err());
        let independent = MemoryRequestPolicy {
            storage: storage.with_personal_policy(false),
            extraction: false,
        };
        let mut group = original;
        group.items = vec![ConversationItem::system(
            "<memory-context>independent fact</memory-context>",
        )];
        independent.apply(&mut group).unwrap();
        assert!(group.items[0].text_content().contains("independent fact"));
    }
}
