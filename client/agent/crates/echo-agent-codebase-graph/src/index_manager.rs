//! Channel-based IndexManager for incremental reindexing.
//!
//! This module provides a clean, channel-based architecture for managing
//! the code graph index with support for incremental updates from file system events.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────┐     FileEvent     ┌─────────────────┐
//! │   FSNotify      │ ──────────────────▶│  IndexManager   │
//! │   (debounced)   │                   │   (owns index)  │
//! └─────────────────┘                   └────────┬────────┘
//!                                                │
//!                                                ▼
//!                                       ┌─────────────────┐
//!                                       │ ScopeGraphIndex │
//!                                       │   (mutations)   │
//!                                       └─────────────────┘
//! ```
//!
//! The IndexManager runs in its own task and processes events sequentially,
//! eliminating the need for Arc<Mutex> around the index.
//!
//! **Note**: Debouncing is handled externally by notify-debouncer-full (FSEvents).
//! Events arriving here are already debounced, so we process them immediately.
//!
//! ## Deduplication
//!
//! The module ensures that:
//! - At most one `IndexManager` exists per workspace per process (via `ACTIVE_MANAGERS`)
//! - Concurrent index operations are coordinated via locks (see `manager::lock`)
//! - Background refresh operations are deduplicated across processes

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Instant;

/// Maximum file size we'll attempt to index (5 MB).
/// Files larger than this are skipped to avoid pathological memory usage
/// from tree-sitter AST construction on huge or binary files.
pub const MAX_INDEXABLE_FILE_SIZE: u64 = 5 * 1024 * 1024;

use crossbeam::channel::{self, Receiver, Sender};
use dashmap::DashMap;
use echo_agent_paths::to_relative_path;
use once_cell::sync::Lazy;

use crate::languages::LanguageRegistry;
use crate::manager::IndexBuilder;
use crate::scope_graph::ScopeGraphIndex;
use crate::types::{FileMeta, IndexStats};

/// Global registry of active IndexManager handles per workspace.
///
/// This ensures that at most one IndexManager exists per workspace per process.
/// Uses `Weak` references so handles are automatically cleaned up when dropped.
static ACTIVE_MANAGERS: Lazy<DashMap<PathBuf, Weak<IndexManagerHandle>>> = Lazy::new(DashMap::new);

/// File system event kind - maps from notify::EventKind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileEventKind {
    /// File was created
    Created,
    /// File was modified
    Modified,
    /// File was deleted
    Removed,
    /// File was renamed (old path in the event, new path in paths[1] if available)
    Renamed,
}

/// A file system event that triggers index updates.
#[derive(Debug, Clone)]
pub struct FileEvent {
    /// The affected file path(s)
    pub paths: Vec<PathBuf>,
    /// The kind of event
    pub kind: FileEventKind,
}

impl FileEvent {
    /// Create a new file event.
    pub fn new(paths: Vec<PathBuf>, kind: FileEventKind) -> Self {
        Self { paths, kind }
    }

    /// Create a "file created" event.
    pub fn created(path: PathBuf) -> Self {
        Self::new(vec![path], FileEventKind::Created)
    }

    /// Create a "file modified" event.
    pub fn modified(path: PathBuf) -> Self {
        Self::new(vec![path], FileEventKind::Modified)
    }

    /// Create a "file removed" event.
    pub fn removed(path: PathBuf) -> Self {
        Self::new(vec![path], FileEventKind::Removed)
    }

    /// Create a "file renamed" event.
    pub fn renamed(from: PathBuf, to: PathBuf) -> Self {
        Self::new(vec![from, to], FileEventKind::Renamed)
    }
}

/// Commands that can be sent to the IndexManager.
pub enum IndexCommand {
    /// Process a file event
    FileEvent(FileEvent),
    /// Process a batch of file events (more efficient)
    FileEventBatch(Vec<FileEvent>),
    /// Rebuild the entire index
    Rebuild,
    /// Get a shared snapshot of the current index (response sent via oneshot).
    /// Returns an `Arc` — cloning the sender is zero-cost when no mutations
    /// are in flight.
    GetSnapshot(tokio::sync::oneshot::Sender<Arc<ScopeGraphIndex>>),
    /// Go to definition query
    GotoDefinition {
        file_path: PathBuf,
        row: usize,
        col: usize,
        response_tx: tokio::sync::oneshot::Sender<Result<QueryResult, QueryError>>,
    },
    /// Go to references query
    GotoReferences {
        file_path: PathBuf,
        row: usize,
        col: usize,
        include_definition: bool,
        response_tx: tokio::sync::oneshot::Sender<Result<QueryResult, QueryError>>,
    },
    /// Find definitions by symbol name
    FindDefinitions {
        symbol: String,
        context_file: Option<PathBuf>,
        response_tx: tokio::sync::oneshot::Sender<Vec<SymbolLocation>>,
    },
    /// Find references by symbol name
    FindReferences {
        symbol: String,
        context_file: Option<PathBuf>,
        response_tx: tokio::sync::oneshot::Sender<Vec<SymbolLocation>>,
    },
    /// Background refresh: reindex stale/new files discovered in background
    BackgroundRefresh {
        /// Files that need reindexing (stale or new)
        stale_files: Vec<String>,
        /// Files that were deleted
        deleted_files: Vec<String>,
    },
    /// Get the number of indexed files (lightweight, no clone)
    GetFileCount(tokio::sync::oneshot::Sender<usize>),
    /// Get index statistics (lightweight, no clone)
    GetStats(tokio::sync::oneshot::Sender<IndexStats>),
    /// Get the query version stamp of the current index (lightweight, no clone)
    GetQueryVersion(tokio::sync::oneshot::Sender<crate::QueryVersion>),
    /// Check whether a symbol has any definitions (lightweight, no clone)
    HasDefinition {
        symbol: String,
        response_tx: tokio::sync::oneshot::Sender<bool>,
    },
    /// Shutdown the manager
    Shutdown,
}

/// Result of a query operation.
#[derive(Debug, Clone)]
pub struct QueryResult {
    /// The symbol that was found at the query position.
    pub symbol: String,
    /// List of locations where the symbol is defined/referenced.
    pub locations: Vec<SymbolLocation>,
}

/// A symbol location in a file.
///
/// `path` is stored as **relative** to the index root_path (for portability across machines/sessions).
/// Use `to_relative_path` helper when creating from absolute paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolLocation {
    /// Path to the file (relative to index root_path).
    pub path: String,
    /// 1-indexed line number.
    pub line: usize,
    /// Optional: the matched symbol name (useful for aliases).
    pub matched_symbol: Option<String>,
}

impl SymbolLocation {
    /// Create a new location with relative path (to index root_path).
    pub fn new(path: impl Into<String>, line: usize) -> Self {
        Self {
            path: path.into(),
            line,
            matched_symbol: None,
        }
    }

    /// Create a new location with relative path and matched symbol.
    pub fn with_symbol(path: impl Into<String>, line: usize, symbol: String) -> Self {
        Self {
            path: path.into(),
            line,
            matched_symbol: Some(symbol),
        }
    }

    /// Get the path as a Path reference (relative to index root_path).
    pub fn as_path(&self) -> &Path {
        Path::new(&self.path)
    }
}

/// Error type for query operations.
#[derive(Debug, Clone)]
pub enum QueryError {
    /// File not found or could not be read.
    FileNotFound(PathBuf),
    /// No symbol found at the given position.
    NoSymbolAtPosition { row: usize, col: usize },
    /// Language not supported.
    UnsupportedLanguage(String),
    /// Parse error.
    ParseError(String),
}

/// Handle for sending commands to the IndexManager.
#[derive(Clone)]
pub struct IndexManagerHandle {
    command_tx: Sender<IndexCommand>,
    /// Test-only: true once the actor thread exits.
    #[cfg(test)]
    exit_signal: Arc<std::sync::atomic::AtomicBool>,
}

impl IndexManagerHandle {
    /// Test-only: has the actor thread's `run_loop` returned yet?
    #[cfg(test)]
    pub(crate) fn has_run_loop_exited(&self) -> bool {
        self.exit_signal.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Send a file event to the manager.
    pub fn send_event(&self, event: FileEvent) -> Result<(), channel::SendError<IndexCommand>> {
        self.command_tx.send(IndexCommand::FileEvent(event))
    }

    /// Send a batch of file events (more efficient than sending one at a time).
    pub fn send_events(
        &self,
        events: Vec<FileEvent>,
    ) -> Result<(), channel::SendError<IndexCommand>> {
        if events.is_empty() {
            return Ok(());
        }
        self.command_tx.send(IndexCommand::FileEventBatch(events))
    }

    /// Request a full rebuild of the index.
    pub fn rebuild(&self) -> Result<(), channel::SendError<IndexCommand>> {
        self.command_tx.send(IndexCommand::Rebuild)
    }

    /// Get a shared snapshot of the current index (blocking).
    ///
    /// Returns an `Arc<ScopeGraphIndex>` — the clone is zero-cost when no
    /// mutation is in flight inside the manager.
    pub fn get_snapshot(&self) -> Result<Arc<ScopeGraphIndex>, channel::SendError<IndexCommand>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.command_tx.send(IndexCommand::GetSnapshot(tx))?;
        Ok(rx
            .blocking_recv()
            .expect("IndexManager dropped before responding"))
    }

    /// Get a shared snapshot of the current index (async version).
    pub async fn get_snapshot_async(
        &self,
    ) -> Result<Arc<ScopeGraphIndex>, channel::SendError<IndexCommand>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.command_tx.send(IndexCommand::GetSnapshot(tx))?;
        Ok(rx.await.expect("IndexManager dropped before responding"))
    }

    /// Get the number of indexed files without cloning the entire index.
    pub fn get_file_count(&self) -> Option<usize> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.command_tx.send(IndexCommand::GetFileCount(tx)).ok()?;
        rx.blocking_recv().ok()
    }

    /// Get index statistics without cloning the entire index.
    pub fn get_stats(&self) -> Option<IndexStats> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.command_tx.send(IndexCommand::GetStats(tx)).ok()?;
        rx.blocking_recv().ok()
    }

    /// Get the query version stamp of the current index without cloning.
    pub fn get_query_version(&self) -> Option<crate::QueryVersion> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.command_tx
            .send(IndexCommand::GetQueryVersion(tx))
            .ok()?;
        rx.blocking_recv().ok()
    }

    /// Check if the index contains at least one definition for `symbol` (blocking).
    ///
    /// Returns `Some(true/false)` on success, or `None` if the manager is
    /// unavailable.  Prefer this over `get_snapshot()` + `has_definition()`
    /// when you only need a boolean existence check.
    pub fn has_definition_blocking(&self, symbol: &str) -> Option<bool> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.command_tx
            .send(IndexCommand::HasDefinition {
                symbol: symbol.to_string(),
                response_tx: tx,
            })
            .ok()?;
        rx.blocking_recv().ok()
    }

    /// Shutdown the IndexManager.
    pub fn shutdown(&self) -> Result<(), channel::SendError<IndexCommand>> {
        self.command_tx.send(IndexCommand::Shutdown)
    }

    // ========== Async Query APIs ==========

    /// Go to definition at the given position (async).
    ///
    /// # Arguments
    /// * `file_path` - Path to the file
    /// * `row` - 1-indexed line number
    /// * `col` - 1-indexed column number
    pub async fn goto_definition(
        &self,
        file_path: PathBuf,
        row: usize,
        col: usize,
    ) -> Result<Result<QueryResult, QueryError>, channel::SendError<IndexCommand>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.command_tx.send(IndexCommand::GotoDefinition {
            file_path,
            row,
            col,
            response_tx: tx,
        })?;
        Ok(rx.await.expect("IndexManager dropped before responding"))
    }

    /// Go to references at the given position (async).
    ///
    /// # Arguments
    /// * `file_path` - Path to the file
    /// * `row` - 1-indexed line number
    /// * `col` - 1-indexed column number
    /// * `include_definition` - Whether to include definition locations in results
    pub async fn goto_references(
        &self,
        file_path: PathBuf,
        row: usize,
        col: usize,
        include_definition: bool,
    ) -> Result<Result<QueryResult, QueryError>, channel::SendError<IndexCommand>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.command_tx.send(IndexCommand::GotoReferences {
            file_path,
            row,
            col,
            include_definition,
            response_tx: tx,
        })?;
        Ok(rx.await.expect("IndexManager dropped before responding"))
    }

    /// Find definitions by symbol name (async).
    ///
    /// # Arguments
    /// * `symbol` - The symbol name to look up
    /// * `context_file` - Optional file path for context-aware ranking
    pub async fn find_definitions(
        &self,
        symbol: String,
        context_file: Option<PathBuf>,
    ) -> Result<Vec<SymbolLocation>, channel::SendError<IndexCommand>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.command_tx.send(IndexCommand::FindDefinitions {
            symbol,
            context_file,
            response_tx: tx,
        })?;
        Ok(rx.await.expect("IndexManager dropped before responding"))
    }

    /// Find references by symbol name (async).
    ///
    /// # Arguments
    /// * `symbol` - The symbol name to look up
    /// * `context_file` - Optional file path for context-aware ranking
    pub async fn find_references(
        &self,
        symbol: String,
        context_file: Option<PathBuf>,
    ) -> Result<Vec<SymbolLocation>, channel::SendError<IndexCommand>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.command_tx.send(IndexCommand::FindReferences {
            symbol,
            context_file,
            response_tx: tx,
        })?;
        Ok(rx.await.expect("IndexManager dropped before responding"))
    }

    // ========== Blocking Query APIs ==========

    /// Go to definition at the given position (blocking).
    pub fn goto_definition_blocking(
        &self,
        file_path: PathBuf,
        row: usize,
        col: usize,
    ) -> Result<Result<QueryResult, QueryError>, channel::SendError<IndexCommand>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.command_tx.send(IndexCommand::GotoDefinition {
            file_path,
            row,
            col,
            response_tx: tx,
        })?;
        Ok(rx
            .blocking_recv()
            .expect("IndexManager dropped before responding"))
    }

    /// Go to references at the given position (blocking).
    pub fn goto_references_blocking(
        &self,
        file_path: PathBuf,
        row: usize,
        col: usize,
        include_definition: bool,
    ) -> Result<Result<QueryResult, QueryError>, channel::SendError<IndexCommand>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.command_tx.send(IndexCommand::GotoReferences {
            file_path,
            row,
            col,
            include_definition,
            response_tx: tx,
        })?;
        Ok(rx
            .blocking_recv()
            .expect("IndexManager dropped before responding"))
    }

    /// Find definitions by symbol name (blocking).
    pub fn find_definitions_blocking(
        &self,
        symbol: String,
        context_file: Option<PathBuf>,
    ) -> Result<Vec<SymbolLocation>, channel::SendError<IndexCommand>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.command_tx.send(IndexCommand::FindDefinitions {
            symbol,
            context_file,
            response_tx: tx,
        })?;
        Ok(rx
            .blocking_recv()
            .expect("IndexManager dropped before responding"))
    }

    /// Find references by symbol name (blocking).
    pub fn find_references_blocking(
        &self,
        symbol: String,
        context_file: Option<PathBuf>,
    ) -> Result<Vec<SymbolLocation>, channel::SendError<IndexCommand>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.command_tx.send(IndexCommand::FindReferences {
            symbol,
            context_file,
            response_tx: tx,
        })?;
        Ok(rx
            .blocking_recv()
            .expect("IndexManager dropped before responding"))
    }
}

/// Configuration for the IndexManager.
#[derive(Debug, Clone)]
pub struct IndexManagerConfig {
    /// Root path to index
    pub root_path: PathBuf,
    /// Optional custom cache path
    pub cache_path: Option<PathBuf>,
    /// Whether to load from cache on startup
    pub load_from_cache: bool,
    /// Whether to save to cache on changes
    pub save_to_cache: bool,
}

impl IndexManagerConfig {
    /// Create a new config with just the root path.
    pub fn new(root_path: PathBuf) -> Self {
        Self {
            root_path,
            cache_path: None,
            load_from_cache: true,
            save_to_cache: true,
        }
    }

    /// Set the cache path.
    pub fn with_cache_path(mut self, path: PathBuf) -> Self {
        self.cache_path = Some(path);
        self
    }

    /// Disable cache loading on startup.
    pub fn without_cache_load(mut self) -> Self {
        self.load_from_cache = false;
        self
    }

    /// Disable cache saving.
    pub fn without_cache_save(mut self) -> Self {
        self.save_to_cache = false;
        self
    }
}

/// Test-only RAII guard: sets a shared `AtomicBool` on drop so tests can
/// confirm the actor thread actually exited (not just that the Weak stopped upgrading).
#[cfg(test)]
struct ExitBeacon(Arc<std::sync::atomic::AtomicBool>);

#[cfg(test)]
impl Drop for ExitBeacon {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// The IndexManager owns and manages the ScopeGraphIndex.
///
/// It processes file events through a channel and updates the index incrementally.
/// This design avoids Arc<Mutex> by having a single owner of the index.
pub struct IndexManager {
    /// The index being managed. Wrapped in `Arc` so that `GetSnapshot` can
    /// hand out a shared reference without cloning.  Mutations use
    /// `Arc::make_mut` which is zero-cost when no snapshot is alive and
    /// performs a single clone (COW) when one is.
    index: Arc<ScopeGraphIndex>,
    /// Language registry for parsing
    registry: LanguageRegistry,
    /// Configuration
    config: IndexManagerConfig,
    /// Command receiver
    command_rx: Receiver<IndexCommand>,
    /// Stats: number of updates processed
    updates_processed: usize,
    /// Cached parsers by language ID (avoids recreating on each file)
    parser_cache: HashMap<String, tree_sitter::Parser>,
    /// Cached queries by language ID (avoids recompiling on each file)
    query_cache: HashMap<String, tree_sitter::Query>,
}

impl IndexManager {
    /// Spawn an IndexManager in a background thread, returning immediately.
    ///
    /// Unlike `new()`, this never blocks the caller. The index is loaded/built
    /// in a background thread. Queries will wait in the channel until the index
    /// is ready, then get processed.
    ///
    /// This is the recommended API for integrations that need non-blocking startup.
    ///
    /// ## Deduplication
    ///
    /// This function ensures that at most one `IndexManager` exists per workspace
    /// per process. If an active manager already exists for the workspace, its
    /// handle is returned instead of creating a new one.
    pub fn spawn(config: IndexManagerConfig) -> Arc<IndexManagerHandle> {
        let canonical_root =
            dunce::canonicalize(&config.root_path).unwrap_or_else(|_| config.root_path.clone());

        // Test beacon: shared with the actor thread's ExitBeacon guard.
        #[cfg(test)]
        let exit_signal = Arc::new(std::sync::atomic::AtomicBool::new(false));

        // Use entry API for atomic get-or-create
        // Loop handles the case where we find a dead weak ref and need to retry
        let (handle, command_tx, command_rx) = loop {
            match ACTIVE_MANAGERS.entry(canonical_root.clone()) {
                dashmap::mapref::entry::Entry::Occupied(entry) => {
                    // Check if the existing weak reference is still valid
                    if let Some(strong) = entry.get().upgrade() {
                        tracing::debug!(
                            root = %canonical_root.display(),
                            "Reusing existing IndexManager"
                        );
                        return strong;
                    }
                    // Weak reference is dead, remove it and retry
                    entry.remove();
                    continue;
                }
                dashmap::mapref::entry::Entry::Vacant(entry) => {
                    // No existing manager - create a new one
                    let (command_tx, command_rx) = channel::unbounded();
                    let handle = Arc::new(IndexManagerHandle {
                        command_tx: command_tx.clone(),
                        #[cfg(test)]
                        exit_signal: exit_signal.clone(),
                    });
                    entry.insert(Arc::downgrade(&handle));
                    break (handle, command_tx, command_rx);
                }
            }
        };

        // Weak ref so the bg thread can't keep the channel alive past teardown.
        // If every handle drops, the channel disconnects and the actor exits --
        // the bg result is discarded.
        let bg_handle_weak = Arc::downgrade(&handle);

        // Capture the current tracing dispatcher so the background thread can use it
        let dispatcher = tracing::dispatcher::get_default(|d| d.clone());

        let root_path_for_log = config.root_path.clone();
        std::thread::Builder::new()
            .name("index-manager".to_string())
            .spawn(move || {
                // Set the tracing dispatcher for this thread
                let _guard = tracing::dispatcher::set_default(&dispatcher);

                tracing::info!(
                    root = %root_path_for_log.display(),
                    "IndexManager thread started, loading/building index..."
                );
                let start = std::time::Instant::now();

                let registry = LanguageRegistry::new();
                let current_query_version = registry.compute_query_hash();

                // Load or build the index (this is the slow part)
                let (index, needs_background_validation) = if config.load_from_cache {
                    let cache_path = config
                        .cache_path
                        .clone()
                        .unwrap_or_else(|| crate::manager::get_cache_path(&config.root_path));

                    match crate::manager::load_index(&cache_path) {
                        Ok(idx) => {
                            // Check if query version matches
                            if idx.needs_query_rebuild(current_query_version) {
                                tracing::info!(
                                    cache_path = %cache_path.display(),
                                    cached_version = ?idx.query_version,
                                    current_version = current_query_version,
                                    "Query version mismatch, rebuilding index"
                                );
                                let idx = Self::build_fresh_index(&config.root_path);
                                let (files, defs, refs) = idx.stats();
                                tracing::info!(
                                    files = files,
                                    definitions = defs,
                                    references = refs,
                                    elapsed_ms = start.elapsed().as_millis() as u64,
                                    "Fresh index build complete (query version changed)"
                                );
                                (idx, false)
                            } else {
                                let (files, defs, refs) = idx.stats();
                                tracing::info!(
                                    cache_path = %cache_path.display(),
                                    files = files,
                                    definitions = defs,
                                    references = refs,
                                    elapsed_ms = start.elapsed().as_millis() as u64,
                                    "Loaded index from cache (will validate in background)"
                                );
                                (idx, true)
                            }
                        }
                        Err(e) => {
                            let error = format!("{:?}", e);
                            tracing::info!(
                                root = %config.root_path.display(),
                                error = %error,
                                "No cache found, building fresh index..."
                            );
                            let idx = Self::build_fresh_index(&config.root_path);
                            let (files, defs, refs) = idx.stats();
                            tracing::info!(
                                files = files,
                                definitions = defs,
                                references = refs,
                                elapsed_ms = start.elapsed().as_millis() as u64,
                                "Fresh index build complete"
                            );
                            (idx, false)
                        }
                    }
                } else {
                    tracing::info!("Cache disabled, building index");
                    let idx = Self::build_fresh_index(&config.root_path);
                    (idx, false)
                };

                let mut manager = Self {
                    index: Arc::new(index),
                    registry,
                    config: config.clone(),
                    command_rx,
                    updates_processed: 0,
                    parser_cache: HashMap::new(),
                    query_cache: HashMap::new(),
                };

                // Save fresh build to cache immediately (don't wait for shutdown)
                if !needs_background_validation {
                    manager.save_cache();
                }

                // Bg thread gets a Weak -- can't pin the actor past teardown.
                if needs_background_validation {
                    let bg_handle = bg_handle_weak;
                    let root_path = config.root_path.clone();
                    // Collect file paths and metadata for background validation
                    let cached_data: Vec<(String, FileMeta)> = manager
                        .index
                        .file_paths_with_meta()
                        .map(|(path, meta)| (path.to_string(), *meta))
                        .collect();

                    // Capture dispatcher for the background thread
                    let bg_dispatcher = tracing::dispatcher::get_default(|d| d.clone());

                    std::thread::Builder::new()
                        .name("index-bg-refresh".to_string())
                        .spawn(move || {
                            let _guard = tracing::dispatcher::set_default(&bg_dispatcher);
                            background_index_refresh(root_path, cached_data, bg_handle);
                        })
                        .expect("Failed to spawn background refresh thread");
                }

                // Drop our sender so the channel can disconnect when all handles drop.
                drop(command_tx);

                // Test beacon: fires on run_loop return.
                #[cfg(test)]
                let _exit_beacon = ExitBeacon(exit_signal);

                // Run the event loop - queries queued during load will now be processed
                manager.run_loop();
            })
            .expect("Failed to spawn index manager thread");

        handle
    }

    /// Build a fresh index from scratch.
    fn build_fresh_index(root_path: &Path) -> ScopeGraphIndex {
        let start = Instant::now();
        match IndexBuilder::new().build(root_path) {
            Ok(index) => {
                let (files, defs, refs) = index.stats();
                tracing::info!(
                    files = files,
                    definitions = defs,
                    references = refs,
                    elapsed_ms = start.elapsed().as_millis() as u64,
                    "Built index"
                );
                index
            }
            Err(e) => {
                tracing::error!(error = %e, "Failed to build index");
                ScopeGraphIndex::new()
            }
        }
    }

    /// Run the manager, processing commands until shutdown.
    ///
    /// This should be called in a dedicated thread.
    /// Event loop. Drains and coalesces pending file events before processing.
    fn run_loop(&mut self) {
        let (files, defs, refs) = self.index.stats();
        tracing::info!(
            files = files,
            definitions = defs,
            references = refs,
            "IndexManager ready, processing queries"
        );

        let mut last_cache_save = Instant::now();

        loop {
            match self.command_rx.recv() {
                Ok(cmd) => {
                    if !self.process_command_coalesced(cmd, &mut last_cache_save) {
                        break;
                    }
                }
                Err(_) => {
                    tracing::debug!("Channel closed, shutting down");
                    break;
                }
            }
        }

        // Save cache on shutdown
        self.save_cache();
        tracing::info!(
            updates_processed = self.updates_processed,
            "IndexManager shutdown complete"
        );
    }

    /// Process a command with event coalescing and time-throttled cache saving.
    ///
    /// For file events: drains pending events from the channel and coalesces
    /// by path (last-writer-wins) before processing. A `Removed` after a
    /// `Created`/`Modified` cancels both. A `Created`/`Modified` after
    /// `Removed` processes as `Created` (file was replaced).
    ///
    /// Returns false if shutdown was requested.
    fn process_command_coalesced(
        &mut self,
        command: IndexCommand,
        last_cache_save: &mut Instant,
    ) -> bool {
        match command {
            IndexCommand::FileEvent(event) => {
                let mut coalesced = CoalescedEvents::new();
                coalesced.add(event);
                // Drain any pending file events from the channel
                while let Ok(cmd) = self.command_rx.try_recv() {
                    match cmd {
                        IndexCommand::FileEvent(e) => coalesced.add(e),
                        IndexCommand::FileEventBatch(batch) => {
                            for e in batch {
                                coalesced.add(e);
                            }
                        }
                        other => {
                            // Non-file command: flush coalesced events, then handle.
                            // Depth-1 recursion: `other` is never a file event.
                            self.apply_coalesced(coalesced, last_cache_save);
                            return self.process_command_coalesced(other, last_cache_save);
                        }
                    }
                }
                self.apply_coalesced(coalesced, last_cache_save);
                true
            }
            IndexCommand::FileEventBatch(events) => {
                let mut coalesced = CoalescedEvents::new();
                for e in events {
                    coalesced.add(e);
                }
                self.apply_coalesced(coalesced, last_cache_save);
                true
            }
            IndexCommand::Rebuild => {
                self.rebuild_index();
                true
            }
            IndexCommand::GetSnapshot(response_tx) => {
                let _ = response_tx.send(Arc::clone(&self.index));
                true
            }
            IndexCommand::GotoDefinition {
                file_path,
                row,
                col,
                response_tx,
            } => {
                let result = self.handle_goto_definition(&file_path, row, col);
                let _ = response_tx.send(result);
                true
            }
            IndexCommand::GotoReferences {
                file_path,
                row,
                col,
                include_definition,
                response_tx,
            } => {
                let result = self.handle_goto_references(&file_path, row, col, include_definition);
                let _ = response_tx.send(result);
                true
            }
            IndexCommand::FindDefinitions {
                symbol,
                context_file,
                response_tx,
            } => {
                let result = self.handle_find_definitions(&symbol, context_file.as_deref());
                let _ = response_tx.send(result);
                true
            }
            IndexCommand::FindReferences {
                symbol,
                context_file,
                response_tx,
            } => {
                let result = self.handle_find_references(&symbol, context_file.as_deref());
                let _ = response_tx.send(result);
                true
            }
            IndexCommand::BackgroundRefresh {
                stale_files,
                deleted_files,
            } => {
                self.process_background_refresh(stale_files, deleted_files);
                true
            }
            IndexCommand::GetFileCount(response_tx) => {
                let _ = response_tx.send(self.index.file_count());
                true
            }
            IndexCommand::GetStats(response_tx) => {
                let _ = response_tx.send(self.stats());
                true
            }
            IndexCommand::GetQueryVersion(response_tx) => {
                let _ = response_tx.send(self.index.query_version.clone());
                true
            }
            IndexCommand::HasDefinition {
                symbol,
                response_tx,
            } => {
                let _ = response_tx.send(self.index.has_definition(&symbol));
                true
            }
            IndexCommand::Shutdown => {
                tracing::debug!("Shutdown requested");
                false
            }
        }
    }

    /// Handle goto_definition query.
    fn handle_goto_definition(
        &mut self,
        file_path: &Path,
        row: usize,
        col: usize,
    ) -> Result<QueryResult, QueryError> {
        // Get symbol at position
        let symbol = self.get_symbol_at_position(file_path, row, col)?;

        // Look up definitions
        let defs =
            self.index
                .find_definitions_smart(&symbol, Some(file_path), Some(&self.registry));

        let locations = defs
            .into_iter()
            .map(|(path, line)| SymbolLocation::new(path, line))
            .collect();

        Ok(QueryResult { symbol, locations })
    }

    /// Handle goto_references query.
    fn handle_goto_references(
        &mut self,
        file_path: &Path,
        row: usize,
        col: usize,
        include_definition: bool,
    ) -> Result<QueryResult, QueryError> {
        // Get symbol at position
        let symbol = self.get_symbol_at_position(file_path, row, col)?;

        // Look up references
        let refs = self
            .index
            .find_references_smart(&symbol, Some(file_path), Some(&self.registry));

        let mut locations: Vec<SymbolLocation> = refs
            .into_iter()
            .map(|(sym, path, line)| SymbolLocation::with_symbol(path, line, sym))
            .collect();

        // Optionally include definitions
        if include_definition {
            let defs =
                self.index
                    .find_definitions_smart(&symbol, Some(file_path), Some(&self.registry));

            for (path, line) in defs {
                let loc = SymbolLocation::new(path.clone(), line);
                if !locations
                    .iter()
                    .any(|l| l.path == loc.path && l.line == loc.line)
                {
                    locations.insert(0, loc);
                }
            }
        }

        Ok(QueryResult { symbol, locations })
    }

    /// Handle find_definitions query.
    fn handle_find_definitions(
        &self,
        symbol: &str,
        context_file: Option<&Path>,
    ) -> Vec<SymbolLocation> {
        self.index
            .find_definitions_smart(symbol, context_file, Some(&self.registry))
            .into_iter()
            .map(|(path, line)| SymbolLocation::new(path, line))
            .collect()
    }

    /// Handle find_references query.
    fn handle_find_references(
        &self,
        symbol: &str,
        context_file: Option<&Path>,
    ) -> Vec<SymbolLocation> {
        self.index
            .find_references_smart(symbol, context_file, Some(&self.registry))
            .into_iter()
            .map(|(sym, path, line)| SymbolLocation::with_symbol(path, line, sym))
            .collect()
    }

    /// Get the symbol at a file position.
    fn get_symbol_at_position(
        &mut self,
        file_path: &Path,
        row: usize,
        col: usize,
    ) -> Result<String, QueryError> {
        if row == 0 || col == 0 {
            return Err(QueryError::NoSymbolAtPosition { row, col });
        }

        let content = std::fs::read(file_path)
            .map_err(|_| QueryError::FileNotFound(file_path.to_path_buf()))?;

        let lang_config = self.registry.for_file_path(file_path).ok_or_else(|| {
            QueryError::UnsupportedLanguage(
                file_path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("unknown")
                    .to_string(),
            )
        })?;

        let lang_id = lang_config.primary_language_id().to_string();
        let lang = lang_config.language();

        let parser = self.parser_cache.entry(lang_id).or_insert_with(|| {
            let mut p = tree_sitter::Parser::new();
            let _ = p.set_language(&lang);
            p
        });

        let tree = parser
            .parse(&content, None)
            .ok_or_else(|| QueryError::ParseError("Failed to parse file".to_string()))?;

        let point = tree_sitter::Point::new(row.saturating_sub(1), col.saturating_sub(1));
        let node = find_smallest_named_node_at_point(tree.root_node(), point);

        match node {
            Some(n) => {
                let text = std::str::from_utf8(&content[n.byte_range()])
                    .map_err(|_| QueryError::ParseError("Invalid UTF-8".to_string()))?;
                Ok(text.to_string())
            }
            None => Err(QueryError::NoSymbolAtPosition { row, col }),
        }
    }

    fn should_index(&self, path: &Path, root: &Path) -> bool {
        self.registry.is_supported(path) && !is_under_hidden_dir(&to_relative_path(root, path))
    }

    /// Process background refresh results.
    fn process_background_refresh(&mut self, stale_files: Vec<String>, deleted_files: Vec<String>) {
        let start = Instant::now();
        let stale_count = stale_files.len();
        let deleted_count = deleted_files.len();

        // Remove deleted files
        for path in deleted_files {
            Arc::make_mut(&mut self.index).remove_file(Path::new(&path));
        }

        // Reindex stale files
        for path in stale_files {
            let path_ref = Path::new(&path);
            if self.registry.is_supported(path_ref) {
                self.reindex_file(path_ref);
            }
        }

        tracing::info!(
            stale = stale_count,
            deleted = deleted_count,
            elapsed_ms = start.elapsed().as_millis() as u64,
            "Background refresh complete"
        );

        // Save cache after background refresh
        self.save_cache();
    }

    /// Minimum interval between cache saves (avoid blocking during event floods).
    const CACHE_SAVE_INTERVAL_SECS: u64 = 60;

    /// Apply a set of coalesced file events and optionally save cache.
    fn apply_coalesced(&mut self, coalesced: CoalescedEvents, last_cache_save: &mut Instant) {
        let root = self.config.root_path.clone();

        for (path, kind) in coalesced.events {
            match kind {
                FileEventKind::Created | FileEventKind::Modified => {
                    if self.should_index(&path, &root) {
                        self.reindex_file(&path);
                        self.updates_processed += 1;
                    }
                }
                FileEventKind::Removed => {
                    // Gated by should_index: files under hidden dirs / unsupported
                    // extensions were never indexed, so there's nothing to remove.
                    if self.should_index(&path, &root) {
                        self.remove_file(&path);
                        self.updates_processed += 1;
                    }
                }
                FileEventKind::Renamed => {
                    // Renames are passed through without coalescing since they
                    // need both from/to paths which are handled at the event level.
                    if self.should_index(&path, &root) {
                        self.reindex_file(&path);
                        self.updates_processed += 1;
                    }
                }
            }
        }

        // Time-throttled cache save: at most once per CACHE_SAVE_INTERVAL_SECS
        if self.config.save_to_cache
            && self.updates_processed > 0
            && last_cache_save.elapsed().as_secs() >= Self::CACHE_SAVE_INTERVAL_SECS
        {
            self.save_cache();
            *last_cache_save = Instant::now();
        }
    }

    /// Reindex a single file: read, parse, and intern symbols directly.
    ///
    /// Unlike the parallel builder path, this interns symbol names directly
    /// from `&content[byte_range]` into the index's `StringInterner` — zero
    /// intermediate `Arc<str>` allocations.
    ///
    /// Old entries are removed before re-extraction. If the file can't be
    /// read/parsed (transient error, binary, oversized), it stays absent
    /// from the index until the next successful reindex.
    fn reindex_file(&mut self, path: &Path) {
        let rel_path = to_relative_path(&self.config.root_path, path);
        let rel_str = rel_path.to_string_lossy();

        Arc::make_mut(&mut self.index).remove_file(Path::new(rel_str.as_ref()));

        let Some(lang_config) = self.registry.for_file_path(path) else {
            return;
        };
        let lang_id_str = lang_config.primary_language_id();

        let metadata = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(_) => return,
        };
        if metadata.len() == 0 || metadata.len() > MAX_INDEXABLE_FILE_SIZE {
            return;
        }

        // Check binary using a small prefix read to avoid loading large files
        // into memory just to discover they contain null bytes.
        if is_binary_file(path) {
            return;
        }

        let content = match std::fs::read(path) {
            Ok(c) => c,
            Err(_) => return,
        };

        // Populate parser/query caches on miss
        if !self.parser_cache.contains_key(lang_id_str) {
            let mut p = tree_sitter::Parser::new();
            let _ = p.set_language(&lang_config.language());
            self.parser_cache.insert(lang_id_str.to_string(), p);
        }
        if !self.query_cache.contains_key(lang_id_str) {
            let q = lang_config.compile_query().unwrap_or_else(|_| {
                tree_sitter::Query::new(&lang_config.language(), "").expect("empty query")
            });
            self.query_cache.insert(lang_id_str.to_string(), q);
        }

        let Some(parser) = self.parser_cache.get_mut(lang_id_str) else {
            return;
        };
        let Some(tree) = parser.parse(&content, None) else {
            return;
        };
        let Some(query) = self.query_cache.get(lang_id_str) else {
            return;
        };

        // Intern path once, then extract and intern symbols directly from
        // &content[byte_range] — no Arc<str> intermediaries.
        let idx = Arc::make_mut(&mut self.index);
        let path_id = idx.intern(&rel_str);
        intern_symbols_directly(query, tree.root_node(), &content, path_id, idx);

        // Use already-obtained metadata (avoids re-stat and relative path CWD issues)
        Arc::make_mut(&mut self.index).set_file_meta(&rel_str, FileMeta::from_metadata(&metadata));
    }

    /// Remove a file from the index.
    fn remove_file(&mut self, path: &Path) {
        let rel_path = to_relative_path(&self.config.root_path, path);
        Arc::make_mut(&mut self.index).remove_file(&rel_path);
    }

    /// Rebuild the entire index.
    fn rebuild_index(&mut self) {
        tracing::info!("Rebuilding entire index...");
        self.index = Arc::new(Self::build_fresh_index(&self.config.root_path));
        self.save_cache();
    }

    /// Save the index to cache.
    fn save_cache(&self) {
        if !self.config.save_to_cache {
            return;
        }

        let cache_path = self
            .config
            .cache_path
            .clone()
            .unwrap_or_else(|| crate::manager::get_cache_path(&self.config.root_path));

        let start = Instant::now();
        if let Err(e) = crate::manager::save_index(&cache_path, &self.index) {
            tracing::error!(error = %e, "Failed to save cache");
        } else {
            tracing::debug!(
                elapsed_ms = start.elapsed().as_millis() as u64,
                "Cache saved"
            );
        }
    }

    /// Get the current index (consumes the manager).
    pub fn into_index(self) -> Arc<ScopeGraphIndex> {
        self.index
    }

    /// Get stats about the index.
    pub fn stats(&self) -> IndexStats {
        let (files, defs, refs) = self.index.stats();
        IndexStats::new(files, defs, refs)
    }
}

/// Background index refresh: validates cached files and discovers new ones.
///
/// This runs in a background thread and sends a BackgroundRefresh command
/// when complete. Uses parallel processing for fast stat operations.
///
/// ## Deduplication
///
/// This function acquires an exclusive lock before starting. If another
/// process is already performing background refresh on this workspace,
/// this call returns early without doing any work.
fn background_index_refresh(
    root_path: PathBuf,
    cached_data: Vec<(String, FileMeta)>,
    handle: Weak<IndexManagerHandle>,
) {
    use crate::manager::lock::{IndexOperation, LockResult, try_lock};
    use rayon::prelude::*;
    use std::collections::HashSet;

    // Test seam: delay file keeps this bg thread alive to prove the actor
    // still exits promptly on handle-drop.
    #[cfg(test)]
    if let Ok(s) = std::fs::read_to_string(root_path.join(".bg_refresh_test_delay_ms"))
        && let Ok(ms) = s.trim().parse::<u64>()
    {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }

    // Try to acquire exclusive lock for background refresh
    let _lock_guard = match try_lock(&root_path, IndexOperation::BackgroundRefresh) {
        LockResult::Acquired(guard) => guard,
        LockResult::Busy {
            operation,
            holder_pid,
        } => {
            tracing::info!(
                root = %root_path.display(),
                blocking_operation = %operation,
                blocking_pid = ?holder_pid,
                "Skipping background refresh - another operation in progress"
            );
            return;
        }
    };

    let start = Instant::now();
    tracing::info!(
        cached_files = cached_data.len(),
        "Starting background index validation"
    );

    // Phase 1: Parallel stat check on cached files to find stale/deleted
    let (stale_from_cache, deleted): (Vec<_>, Vec<_>) = cached_data
        .par_iter()
        .filter_map(|(path, cached_meta)| {
            let path_ref = Path::new(path);
            if cached_meta.is_stale(path_ref) {
                // Check if file exists or is deleted
                if path_ref.exists() {
                    Some((Some(path.clone()), None)) // Stale
                } else {
                    Some((None, Some(path.clone()))) // Deleted
                }
            } else {
                None // Up to date
            }
        })
        .fold(
            || (Vec::new(), Vec::new()),
            |(mut stale, mut deleted), item| {
                if let Some(p) = item.0 {
                    stale.push(p);
                }
                if let Some(p) = item.1 {
                    deleted.push(p);
                }
                (stale, deleted)
            },
        )
        .reduce(
            || (Vec::new(), Vec::new()),
            |(mut stale_a, mut deleted_a), (stale_b, deleted_b)| {
                stale_a.extend(stale_b);
                deleted_a.extend(deleted_b);
                (stale_a, deleted_a)
            },
        );

    let stat_elapsed = start.elapsed();
    tracing::debug!(
        stale = stale_from_cache.len(),
        deleted = deleted.len(),
        elapsed_ms = stat_elapsed.as_millis() as u64,
        "Stat check complete"
    );

    // Phase 2: Walk filesystem to find new files (not in cache)
    let cached_set: HashSet<&str> = cached_data.iter().map(|(p, _)| p.as_str()).collect();
    let registry = crate::languages::LanguageRegistry::new();

    let new_files: Vec<String> = ignore::WalkBuilder::new(&root_path)
        .hidden(true) // Skip hidden files/dirs
        .git_ignore(true) // Respect .gitignore
        .git_global(true)
        .git_exclude(true)
        .build()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
        .filter(|e| registry.is_supported(e.path()))
        .filter(|e| !cached_set.contains(e.path().to_string_lossy().as_ref()))
        .map(|e| e.path().to_string_lossy().into_owned())
        .collect();

    let walk_elapsed = start.elapsed() - stat_elapsed;
    tracing::debug!(
        new_files = new_files.len(),
        elapsed_ms = walk_elapsed.as_millis() as u64,
        "Filesystem walk complete"
    );

    // Combine stale and new files
    let mut stale_files = stale_from_cache;
    stale_files.extend(new_files);

    let total_elapsed = start.elapsed();
    tracing::info!(
        stale = stale_files.len(),
        deleted = deleted.len(),
        total_elapsed_ms = total_elapsed.as_millis() as u64,
        "Background validation complete, sending refresh command"
    );

    // Send result only if the manager is still leased (Weak upgrades).
    // Otherwise discard -- the actor already exited.
    if (!stale_files.is_empty() || !deleted.is_empty())
        && let Some(handle) = handle.upgrade()
    {
        let _ = handle.command_tx.send(IndexCommand::BackgroundRefresh {
            stale_files,
            deleted_files: deleted,
        });
    }
}

/// Extract symbols from a parsed tree and intern them directly into the index.
///
/// This is the zero-alloc alternative to `extract_symbols_inline` for the
/// incremental reindex path. Instead of creating `Arc<str>` for each symbol
/// and collecting into Vecs, it interns symbol names directly from
/// `&src[byte_range]` into the index's `StringInterner` and adds
/// definition/reference entries inline.
///
/// Invalid UTF-8 byte ranges are skipped (indicates binary content in that
/// region of the file — no lossy replacement needed).
fn intern_symbols_directly(
    query: &tree_sitter::Query,
    root_node: tree_sitter::Node<'_>,
    src: &[u8],
    path_id: crate::interner::StringId,
    index: &mut ScopeGraphIndex,
) {
    use tree_sitter::StreamingIterator;

    let capture_names = query.capture_names();
    let mut is_def = vec![false; capture_names.len()];
    let mut is_ref = vec![false; capture_names.len()];
    let mut alias_original_idx: Option<usize> = None;
    let mut alias_name_idx: Option<usize> = None;

    for (i, name) in capture_names.iter().enumerate() {
        if name.starts_with("name.definition.") {
            is_def[i] = true;
        } else if name.starts_with("name.reference.") {
            is_ref[i] = true;
        } else if *name == "alias.original" {
            alias_original_idx = Some(i);
        } else if *name == "alias.name" {
            alias_name_idx = Some(i);
        }
    }

    let mut cursor = tree_sitter::QueryCursor::new();
    let mut matches = cursor.matches(query, root_node, src);

    while let Some(m) = matches.next() {
        let mut alias_original: Option<&[u8]> = None;
        let mut alias_name: Option<&[u8]> = None;

        for capture in m.captures {
            let idx = capture.index as usize;
            let node = capture.node;
            let byte_range = node.byte_range();
            let line = node.start_position().row + 1;

            let bytes = &src[byte_range];
            // Skip non-UTF-8 ranges (binary artifact) instead of lossy replacement
            let Ok(text) = std::str::from_utf8(bytes) else {
                continue;
            };

            if is_def.get(idx).copied().unwrap_or(false) {
                index.add_definition_with_path_id(text, path_id, line);
            } else if is_ref.get(idx).copied().unwrap_or(false) {
                index.add_reference_with_path_id(text, path_id, line);
            } else if Some(idx) == alias_original_idx {
                alias_original = Some(bytes);
            } else if Some(idx) == alias_name_idx {
                alias_name = Some(bytes);
            }
        }

        if let (Some(original), Some(alias)) = (alias_original, alias_name)
            && let (Ok(orig_str), Ok(alias_str)) =
                (std::str::from_utf8(original), std::str::from_utf8(alias))
        {
            index.add_alias(alias_str, orig_str);
        }
    }
}

/// Event coalescing state: deduplicates file events by path.
///
/// Semantics:
/// - `Created` + `Modified` → `Modified` (already exists, just reindex)
/// - `Created/Modified` + `Removed` → cancelled (nothing to do)
/// - `Removed` + `Created/Modified` → `Created` (file was replaced)
/// - Multiple `Modified` → single `Modified`
struct CoalescedEvents {
    events: HashMap<PathBuf, FileEventKind>,
}

impl CoalescedEvents {
    fn new() -> Self {
        Self {
            events: HashMap::new(),
        }
    }

    fn add(&mut self, event: FileEvent) {
        // Renames are special: they carry two paths. Process the "to" path
        // as Created (it needs indexing) and the "from" as Removed.
        if event.kind == FileEventKind::Renamed && event.paths.len() >= 2 {
            self.insert(event.paths[0].clone(), FileEventKind::Removed);
            self.insert(event.paths[1].clone(), FileEventKind::Created);
            return;
        }

        for path in event.paths {
            self.insert(path, event.kind);
        }
    }

    fn insert(&mut self, path: PathBuf, kind: FileEventKind) {
        use std::collections::hash_map::Entry;
        match self.events.entry(path) {
            Entry::Vacant(e) => {
                e.insert(kind);
            }
            Entry::Occupied(mut e) => {
                let prev = *e.get();
                match (prev, kind) {
                    // Created/Modified then Removed → cancel both
                    (FileEventKind::Created | FileEventKind::Modified, FileEventKind::Removed) => {
                        e.remove();
                    }
                    // Removed then Created/Modified → file replaced, treat as Created
                    (FileEventKind::Removed, FileEventKind::Created | FileEventKind::Modified) => {
                        e.insert(FileEventKind::Created);
                    }
                    // Same or compatible kinds → last writer wins
                    _ => {
                        e.insert(kind);
                    }
                }
            }
        }
    }
}

/// Check if content appears to be binary by scanning for null bytes.
/// Uses the same heuristic as git (check first 8000 bytes).
pub fn is_binary_content(content: &[u8]) -> bool {
    let check_len = content.len().min(8000);
    content[..check_len].contains(&0)
}

/// Check if a file appears to be binary by reading only the first 8KB.
///
/// Unlike `is_binary_content` (which takes an already-loaded buffer), this
/// reads a small prefix from disk — avoiding loading the entire file into
/// memory just to discover it contains null bytes.
fn is_binary_file(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut buf = [0u8; 8000];
    let Ok(n) = f.read(&mut buf) else {
        return false;
    };
    buf[..n].contains(&0)
}

/// Check if a path is under a hidden directory (component starting with `.`).
///
/// Returns `true` for paths like `.claude/worktrees/x/src/main.rs` or
/// `.echo-agent/worktrees/repo/lib.rs`, which should not be indexed since they
/// are typically tool-managed worktrees or caches.
fn is_under_hidden_dir(path: &Path) -> bool {
    path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|s| s.starts_with('.') && s.len() > 1)
    })
}

/// Find the smallest named node that contains the given point.
fn find_smallest_named_node_at_point(
    node: tree_sitter::Node<'_>,
    point: tree_sitter::Point,
) -> Option<tree_sitter::Node<'_>> {
    // Check if point is within this node
    if point < node.start_position() || point > node.end_position() {
        return None;
    }

    // Try to find a smaller child node that contains the point
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(found) = find_smallest_named_node_at_point(child, point) {
            // Prefer named nodes that look like identifiers
            if is_identifier_like(&found) {
                return Some(found);
            }
            // Keep searching for a better match
            if is_identifier_like(&child) {
                return Some(child);
            }
            return Some(found);
        }
    }

    // No smaller child contains the point, return this node if it's identifier-like
    if is_identifier_like(&node) {
        Some(node)
    } else {
        None
    }
}

/// Check if a node looks like an identifier.
fn is_identifier_like(node: &tree_sitter::Node<'_>) -> bool {
    let kind = node.kind();
    kind == "identifier"
        || kind == "type_identifier"
        || kind == "property_identifier"
        || kind == "field_identifier"
        || kind == "shorthand_property_identifier"
        || kind == "shorthand_property_identifier_pattern"
        || kind == "attribute" // Python
        || kind == "package_identifier" // Go
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_index_manager_basic() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.rs");

        fs::write(&file_path, "fn hello() {}\nfn world() {}").unwrap();

        let config = IndexManagerConfig::new(dir.path().to_path_buf())
            .without_cache_load()
            .without_cache_save();

        let handle = IndexManager::spawn(config);

        assert!(handle.has_definition_blocking("hello").unwrap());
        assert!(handle.has_definition_blocking("world").unwrap());

        // Add a new file
        let new_file = dir.path().join("new.rs");
        fs::write(&new_file, "fn new_func() {}").unwrap();
        handle
            .send_event(FileEvent::created(new_file.clone()))
            .unwrap();

        // Events are processed in order so the next query sees the updated index
        assert!(handle.has_definition_blocking("new_func").unwrap());

        // Shutdown
        handle.shutdown().unwrap();
    }

    #[test]
    fn test_query_hash_is_consistent() {
        let registry1 = LanguageRegistry::new();
        let registry2 = LanguageRegistry::new();

        // Same registry should produce the same hash
        let hash1 = registry1.compute_query_hash();
        let hash2 = registry2.compute_query_hash();

        assert_eq!(
            hash1, hash2,
            "Query hash should be consistent across instances"
        );

        // Hash should be non-zero (extremely unlikely to be zero with real queries)
        assert_ne!(hash1, 0, "Query hash should be non-zero");
    }

    #[test]
    fn test_query_version_set_and_check() {
        use crate::QueryVersion;

        let mut index = ScopeGraphIndex::new();

        // Initially Legacy
        assert_eq!(index.query_version, QueryVersion::Legacy);

        // After setting, should be Version
        index.set_query_version(12345);
        assert_eq!(index.query_version, QueryVersion::Version(12345));

        // needs_query_rebuild should return false for same version
        assert!(!index.needs_query_rebuild(12345));

        // needs_query_rebuild should return true for different version
        assert!(index.needs_query_rebuild(67890));
    }

    #[test]
    fn test_query_version_legacy_triggers_rebuild() {
        use crate::QueryVersion;

        // Index without query version (like old cached indexes)
        let index = ScopeGraphIndex::new();
        assert_eq!(index.query_version, QueryVersion::Legacy);

        // Should force rebuild since we don't know what queries were used
        assert!(
            index.needs_query_rebuild(12345),
            "Legacy indexes should trigger rebuild"
        );
    }

    #[test]
    fn test_index_includes_query_version() {
        use crate::QueryVersion;

        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.rs");
        fs::write(&file_path, "fn hello() {}").unwrap();

        let config = IndexManagerConfig::new(dir.path().to_path_buf())
            .without_cache_load()
            .without_cache_save();

        let handle = IndexManager::spawn(config);

        let qv = handle.get_query_version().unwrap();
        assert!(
            matches!(qv, QueryVersion::Version(_)),
            "Fresh index should have query version set"
        );

        let registry = LanguageRegistry::new();
        assert_eq!(
            qv,
            QueryVersion::Version(registry.compute_query_hash()),
            "Index query version should match current registry hash"
        );

        handle.shutdown().unwrap();
    }

    #[test]
    fn test_is_binary_content_detects_null_bytes() {
        assert!(is_binary_content(b"hello\x00world"));
        assert!(is_binary_content(&[0u8; 100]));
    }

    #[test]
    fn test_is_binary_content_passes_text() {
        assert!(!is_binary_content(b"fn main() {}"));
        assert!(!is_binary_content(b""));
    }

    #[test]
    fn test_is_under_hidden_dir_filters_dotdirs() {
        assert!(is_under_hidden_dir(Path::new(
            ".claude/worktrees/abc/src/main.rs"
        )));
        assert!(is_under_hidden_dir(Path::new(
            ".echo-agent/worktrees/repo/lib.rs"
        )));
        assert!(is_under_hidden_dir(Path::new("src/.hidden/file.rs")));
    }

    #[test]
    fn test_is_under_hidden_dir_passes_normal() {
        assert!(!is_under_hidden_dir(Path::new("src/main.rs")));
        assert!(!is_under_hidden_dir(Path::new("lib/utils/mod.rs")));
    }

    #[test]
    fn test_extract_skips_binary_file() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("binary.py");
        // Write binary content (urandom-like with null bytes)
        let mut content = vec![0u8; 1024];
        content[0] = b'x';
        content[100] = 0;
        fs::write(&file_path, &content).unwrap();

        let config = IndexManagerConfig::new(dir.path().to_path_buf())
            .without_cache_load()
            .without_cache_save();

        let handle = IndexManager::spawn(config);

        // Send event for the binary file
        handle.send_event(FileEvent::created(file_path)).unwrap();

        assert_eq!(handle.get_file_count().unwrap_or(0), 0);

        handle.shutdown().unwrap();
    }

    #[test]
    fn test_extract_skips_oversized_file() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("huge.rs");
        // Write a file larger than MAX_INDEXABLE_FILE_SIZE
        let content = "fn a() {}\n".repeat(600_000); // ~6MB
        fs::write(&file_path, &content).unwrap();

        let config = IndexManagerConfig::new(dir.path().to_path_buf())
            .without_cache_load()
            .without_cache_save();

        let handle = IndexManager::spawn(config);

        handle.send_event(FileEvent::created(file_path)).unwrap();

        assert_eq!(handle.get_file_count().unwrap_or(0), 0);

        handle.shutdown().unwrap();
    }

    #[test]
    fn test_process_event_skips_hidden_dir() {
        let dir = tempdir().unwrap();
        // Create a file under a hidden directory
        let hidden_dir = dir.path().join(".claude").join("worktrees").join("abc");
        fs::create_dir_all(&hidden_dir).unwrap();
        let file_path = hidden_dir.join("main.rs");
        fs::write(&file_path, "fn main() {}").unwrap();

        // Also create a normal file
        let normal_path = dir.path().join("lib.rs");
        fs::write(&normal_path, "fn lib_func() {}").unwrap();

        let config = IndexManagerConfig::new(dir.path().to_path_buf())
            .without_cache_load()
            .without_cache_save();

        let handle = IndexManager::spawn(config);

        handle.send_event(FileEvent::created(file_path)).unwrap();
        handle.send_event(FileEvent::created(normal_path)).unwrap();

        // Only the normal file should be indexed
        assert!(handle.has_definition_blocking("lib_func").unwrap());
        assert!(!handle.has_definition_blocking("main").unwrap());

        handle.shutdown().unwrap();
    }

    #[test]
    fn test_builder_skips_binary_and_oversized_files() {
        use crate::manager::IndexBuilder;

        let dir = tempdir().unwrap();

        // Normal file — should be indexed
        fs::write(dir.path().join("good.rs"), "fn good() {}").unwrap();

        // Binary file with supported extension — should be skipped
        let mut binary = vec![0u8; 1024];
        binary[0] = b'f';
        binary[10] = 0;
        fs::write(dir.path().join("binary.rs"), &binary).unwrap();

        // Oversized file — should be skipped
        let big = "fn big() {}\n".repeat(500_000); // ~6MB
        fs::write(dir.path().join("huge.rs"), &big).unwrap();

        let index = IndexBuilder::new().build(dir.path()).unwrap();
        assert!(index.has_definition("good"));
        assert!(!index.has_definition("big"));
    }

    #[test]
    fn test_get_file_count_lightweight() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "fn alpha() {}").unwrap();
        fs::write(dir.path().join("b.rs"), "fn beta() {}").unwrap();

        let config = IndexManagerConfig::new(dir.path().to_path_buf())
            .without_cache_load()
            .without_cache_save();

        let handle = IndexManager::spawn(config);

        let count = handle.get_file_count().unwrap();
        assert_eq!(count, 2);

        handle.shutdown().unwrap();
    }

    #[test]
    fn test_get_stats_lightweight() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("test.rs"), "fn hello() {}\nfn world() {}").unwrap();

        let config = IndexManagerConfig::new(dir.path().to_path_buf())
            .without_cache_load()
            .without_cache_save();

        let handle = IndexManager::spawn(config);

        let stats = handle.get_stats().unwrap();
        assert_eq!(stats.files, 1);
        assert!(stats.definitions >= 2); // hello + world

        handle.shutdown().unwrap();
    }

    // ========== CoalescedEvents tests ==========

    #[test]
    fn test_coalesce_create_then_remove_cancels() {
        let mut c = CoalescedEvents::new();
        c.add(FileEvent::created("/a.rs".into()));
        c.add(FileEvent::removed("/a.rs".into()));
        assert!(c.events.is_empty());
    }

    #[test]
    fn test_coalesce_modify_then_remove_cancels() {
        let mut c = CoalescedEvents::new();
        c.add(FileEvent::modified("/a.rs".into()));
        c.add(FileEvent::removed("/a.rs".into()));
        assert!(c.events.is_empty());
    }

    #[test]
    fn test_coalesce_remove_then_create_is_created() {
        let mut c = CoalescedEvents::new();
        c.add(FileEvent::removed("/a.rs".into()));
        c.add(FileEvent::created("/a.rs".into()));
        assert_eq!(c.events.len(), 1);
        assert_eq!(c.events[&PathBuf::from("/a.rs")], FileEventKind::Created);
    }

    #[test]
    fn test_coalesce_remove_then_modify_is_created() {
        let mut c = CoalescedEvents::new();
        c.add(FileEvent::removed("/a.rs".into()));
        c.add(FileEvent::modified("/a.rs".into()));
        assert_eq!(c.events.len(), 1);
        assert_eq!(c.events[&PathBuf::from("/a.rs")], FileEventKind::Created);
    }

    #[test]
    fn test_coalesce_multiple_modify_is_single() {
        let mut c = CoalescedEvents::new();
        c.add(FileEvent::modified("/a.rs".into()));
        c.add(FileEvent::modified("/a.rs".into()));
        c.add(FileEvent::modified("/a.rs".into()));
        assert_eq!(c.events.len(), 1);
        assert_eq!(c.events[&PathBuf::from("/a.rs")], FileEventKind::Modified);
    }

    #[test]
    fn test_coalesce_rename_decomposes() {
        let mut c = CoalescedEvents::new();
        c.add(FileEvent::renamed("/old.rs".into(), "/new.rs".into()));
        assert_eq!(c.events.len(), 2);
        assert_eq!(c.events[&PathBuf::from("/old.rs")], FileEventKind::Removed);
        assert_eq!(c.events[&PathBuf::from("/new.rs")], FileEventKind::Created);
    }

    #[test]
    fn test_coalesce_different_paths_independent() {
        let mut c = CoalescedEvents::new();
        c.add(FileEvent::created("/a.rs".into()));
        c.add(FileEvent::modified("/b.rs".into()));
        assert_eq!(c.events.len(), 2);
    }

    #[test]
    fn test_coalesce_rename_then_remove_target_cancels_target() {
        let mut c = CoalescedEvents::new();
        c.add(FileEvent::renamed("/a.rs".into(), "/b.rs".into()));
        c.add(FileEvent::removed("/b.rs".into()));
        // /a.rs should still be Removed, /b.rs Created+Removed = cancelled
        assert_eq!(c.events.len(), 1);
        assert_eq!(c.events[&PathBuf::from("/a.rs")], FileEventKind::Removed);
    }

    #[test]
    fn test_coalesce_rename_then_modify_target() {
        let mut c = CoalescedEvents::new();
        c.add(FileEvent::renamed("/a.rs".into(), "/b.rs".into()));
        c.add(FileEvent::modified("/b.rs".into()));
        // /a.rs Removed, /b.rs Created+Modified → Modified (last writer wins)
        assert_eq!(c.events.len(), 2);
        assert_eq!(c.events[&PathBuf::from("/a.rs")], FileEventKind::Removed);
        assert_eq!(c.events[&PathBuf::from("/b.rs")], FileEventKind::Modified);
    }

    // Verify has_definition_blocking agrees with get_snapshot().has_definition()
    // for both present and absent symbols.
    #[test]
    fn test_has_definition_blocking_agrees_with_snapshot() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "fn present() {}").unwrap();

        let config = IndexManagerConfig::new(dir.path().to_path_buf())
            .without_cache_load()
            .without_cache_save();

        let handle = IndexManager::spawn(config);

        let snapshot = handle.get_snapshot().unwrap();

        assert!(snapshot.has_definition("present"));
        assert_eq!(handle.has_definition_blocking("present"), Some(true));

        assert!(!snapshot.has_definition("absent"));
        assert_eq!(handle.has_definition_blocking("absent"), Some(false));

        handle.shutdown().unwrap();
    }

    // COW isolation: an Arc snapshot taken before a mutation must
    // continue to reflect the original index contents (COW isolation), while
    // a snapshot taken after the mutation must reflect the updated index.
    // Also verifies that the two snapshots no longer share the same backing
    // allocation once a mutation has caused Arc::make_mut to detach.
    #[test]
    fn test_snapshot_isolation_across_mutation() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("target.rs");
        fs::write(&file_path, "fn before() {}").unwrap();

        let config = IndexManagerConfig::new(dir.path().to_path_buf())
            .without_cache_load()
            .without_cache_save();

        let handle = IndexManager::spawn(config);

        // Snapshot A: sees the original index.
        let snapshot_before = handle.get_snapshot().unwrap();
        assert!(snapshot_before.has_definition("before"));
        assert!(!snapshot_before.has_definition("after"));

        // Mutate: replace the file's symbol. While snapshot_before is alive
        // the manager's Arc refcount is >1, so Arc::make_mut clones before
        // mutating — that is the COW path we are testing.
        fs::write(&file_path, "fn after() {}").unwrap();
        handle
            .send_event(FileEvent::modified(file_path.clone()))
            .unwrap();

        // Snapshot B: sees the mutated index.
        let snapshot_after = handle.get_snapshot().unwrap();
        assert!(!snapshot_after.has_definition("before"));
        assert!(snapshot_after.has_definition("after"));

        // Snapshot A is unchanged despite the mutation.
        assert!(snapshot_before.has_definition("before"));
        assert!(!snapshot_before.has_definition("after"));

        // The two snapshots must not share the same backing allocation:
        // Arc::make_mut on the manager's index detached it from snapshot_before.
        assert!(
            !std::sync::Arc::ptr_eq(&snapshot_before, &snapshot_after),
            "snapshots should point to distinct backing allocations after a mutation"
        );

        handle.shutdown().unwrap();
    }

    /// Actor thread exits when the last handle drops.
    #[test]
    fn test_run_loop_thread_exits_when_last_handle_dropped() {
        use std::sync::atomic::Ordering;
        use std::time::{Duration, Instant};

        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "fn alpha() {}").unwrap();

        let config = IndexManagerConfig::new(dir.path().to_path_buf())
            .without_cache_load()
            .without_cache_save();
        let handle = IndexManager::spawn(config);
        // Actor must be alive before we test the drop.
        assert!(
            handle.has_definition_blocking("alpha").unwrap(),
            "index should contain the seeded definition"
        );
        assert!(
            !handle.has_run_loop_exited(),
            "run_loop must still be running while a handle is alive"
        );

        let exit_signal = std::sync::Arc::clone(&handle.exit_signal);
        drop(handle);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !exit_signal.load(Ordering::SeqCst) {
            assert!(
                Instant::now() < deadline,
                "run_loop thread did not exit within 5s after the last handle dropped — \
                 the actor is leaking (actor must drop its own sender before run_loop)"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Actor exits on handle-drop even while bg refresh is still running
    /// (bg holds Weak, not a sender).
    #[test]
    fn test_actor_exits_while_background_refresh_still_running() {
        use std::sync::atomic::Ordering;
        use std::time::{Duration, Instant};

        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "fn alpha() {}").unwrap();

        // Build + cache, then fully reap so pass 2 hits the cache-load path.
        {
            let h1 = IndexManager::spawn(IndexManagerConfig::new(dir.path().to_path_buf()));
            assert!(h1.has_definition_blocking("alpha").unwrap());
            let beacon1 = std::sync::Arc::clone(&h1.exit_signal);
            drop(h1);
            let deadline = Instant::now() + Duration::from_secs(5);
            while !beacon1.load(Ordering::SeqCst) {
                assert!(Instant::now() < deadline, "pass-1 actor did not exit");
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        // Make this root's background refresh sleep 3s so it is provably still
        // running when we drop the handle below.
        fs::write(dir.path().join(".bg_refresh_test_delay_ms"), "3000").unwrap();

        // Pass 2: cache-load triggers a bg refresh (sleeping 3s via the marker).
        let h2 = IndexManager::spawn(IndexManagerConfig::new(dir.path().to_path_buf()));
        assert!(
            h2.has_definition_blocking("alpha").unwrap(),
            "cache reload should still answer queries"
        );
        assert!(!h2.has_run_loop_exited());
        let beacon2 = std::sync::Arc::clone(&h2.exit_signal);
        let dropped_at = Instant::now();
        drop(h2);

        // Must exit well under 3s — proves the bg holds a Weak, not a sender.
        let deadline = Instant::now() + Duration::from_millis(1500);
        while !beacon2.load(Ordering::SeqCst) {
            assert!(
                Instant::now() < deadline,
                "actor thread did not exit within 1.5s of handle-drop while a 3s background \
                 refresh was running — the bg thread is pinning the command channel (regression \
                 of the #8 Weak-sender fix)"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            dropped_at.elapsed() < Duration::from_secs(3),
            "actor exited only after the bg delay elapsed — bg is still pinning the channel"
        );
    }
}
