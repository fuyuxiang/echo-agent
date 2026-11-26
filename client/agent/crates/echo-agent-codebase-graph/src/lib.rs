//! # echo-agent-codebase-graph
//!
//! High-performance code graph generation using tree-sitter queries.
//!
//! This crate provides:
//! - **Go-to-definitions**: Find where symbols are defined
//! - **Go-to-references**: Find where symbols are used
//! - **Initial repository indexing**: Build the full index from scratch
//! - **Incremental reindexing**: Update the index based on file system events
//! - **Parallel processing**: Uses rayon for fast parallel parsing
//! - **Memory-mapped I/O**: Zero-copy file reading and fast index caching
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use std::path::Path;
//! use echo_agent_codebase_graph::{IndexBuilder, load_index, save_index, get_cache_path, Navigator};
//!
//! let repo_path = Path::new("/path/to/repo");
//! let cache_path = get_cache_path(repo_path);
//!
//! // Try loading from cache first, otherwise build fresh
//! let index = match load_index(&cache_path) {
//!     Ok(index) => index,
//!     Err(_) => {
//!         let index = IndexBuilder::new()
//!             .with_threads(8)
//!             .build(repo_path)?;
//!         save_index(&cache_path, &index)?;
//!         index
//!     }
//! };
//!
//! // Create a navigator for location-based operations
//! let navigator = Navigator::new(index);
//!
//! // Go to definition at a specific position (row and col are 1-indexed)
//! let result = navigator.goto_definition(Path::new("src/main.rs"), 10, 15)?;
//! for loc in result.locations {
//!     println!("{}:{}", loc.path.display(), loc.line);
//! }
//! ```
//!
//! ## Channel-Based Incremental Updates
//!
//! `IndexManagerHandle` exposes direct query commands that answer in-place
//! without cloning the full index.  Prefer these over `get_snapshot()` in
//! hot paths.
//!
//! ```rust,ignore
//! use std::path::PathBuf;
//! use echo_agent_codebase_graph::{IndexManager, IndexManagerConfig, FileEvent};
//!
//! // Create the manager with config
//! let config = IndexManagerConfig::new("/path/to/repo".into())
//!     .with_cache_path("/tmp/index.bin".into());
//!
//! let handle = IndexManager::spawn(config);
//!
//! // Send file events as they come from FSNotify
//! handle.send_event(FileEvent::modified("src/main.rs".into()))?;
//!
//! // Query directly — no full-index clone needed
//! let file = PathBuf::from("src/main.rs");
//! let result = handle.goto_definition_blocking(file, 10, 15)??;
//! for loc in result.locations {
//!     println!("{}:{}", loc.path, loc.line);
//! }
//!
//! // Lightweight stats — also no clone
//! let file_count = handle.get_file_count();
//! let exists     = handle.has_definition_blocking("MyStruct");
//! ```

pub mod index_manager;
pub mod interner;
pub mod languages;
pub mod manager;
pub mod navigation;
pub mod scope_graph;
pub mod types;

// Re-exports for convenient access
pub use index_manager::{
    FileEvent, FileEventKind, IndexCommand, IndexManager, IndexManagerConfig, IndexManagerHandle,
    MAX_INDEXABLE_FILE_SIZE, QueryError, QueryResult, SymbolLocation, is_binary_content,
};
pub use languages::{LanguageRegistry, TSLanguageConfig};
pub use manager::{
    CACHE_FILE_NAME, CacheError, IndexBuilder, IndexError, IndexOperation, LockResult,
    WorkspaceLockGuard, cache_exists, cache_size, get_cache_path, is_operation_in_progress,
    load_index, save_index, save_index_async, try_lock,
};
pub use navigation::{Location, NavigationError, NavigationResult, Navigator};
pub use scope_graph::{
    LocalDef, LocalImport, LocalScope, NodeKind, QueryVersion, Reference, ScopeGraph,
    ScopeGraphIndex, ScopeGraphResult, Symbol, SymbolId, build_scope_graph, extract_symbols_fast,
};
pub use types::{FileMeta, IndexStats, Position, Range, SymbolAlias, SymbolOccurrence};

// String interning for memory-efficient storage
pub use interner::{StringId, StringInterner};
