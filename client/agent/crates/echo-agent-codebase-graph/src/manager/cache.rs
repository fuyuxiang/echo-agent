//! Index caching for fast loading.
//!
//! Uses a custom binary format with magic bytes "SGIX" for the new interned format.
//! Automatically detects and skips legacy bincode format (returns error so caller can rebuild).

use std::path::Path;

use crate::scope_graph::ScopeGraphIndex;

/// Default cache file name.
pub const CACHE_FILE_NAME: &str = ".goto_index.bin";

/// Error type for cache operations.
#[derive(Debug)]
pub enum CacheError {
    /// IO error.
    IoError(std::io::Error),
    /// Serialization error.
    SerializeError(String),
    /// Deserialization error.
    DeserializeError(String),
    /// Legacy format detected (caller should rebuild).
    LegacyFormat,
}

impl std::fmt::Display for CacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CacheError::IoError(e) => write!(f, "IO error: {}", e),
            CacheError::SerializeError(msg) => write!(f, "Serialization error: {}", msg),
            CacheError::DeserializeError(msg) => write!(f, "Deserialization error: {}", msg),
            CacheError::LegacyFormat => write!(f, "Legacy cache format detected"),
        }
    }
}

impl std::error::Error for CacheError {}

impl From<std::io::Error> for CacheError {
    fn from(e: std::io::Error) -> Self {
        CacheError::IoError(e)
    }
}

/// Result type for cache operations.
pub type Result<T> = std::result::Result<T, CacheError>;

/// Get the default cache path for a repository.
pub fn get_cache_path(root_path: &Path) -> std::path::PathBuf {
    root_path.join(CACHE_FILE_NAME)
}

/// Load an index from cache.
///
/// Uses the new binary format with magic bytes "SGIX".
/// Returns `CacheError::LegacyFormat` if the file uses the old bincode format,
/// signaling to the caller that a rebuild is needed.
pub fn load_index(cache_path: &Path) -> Result<ScopeGraphIndex> {
    if !cache_path.exists() {
        return Err(CacheError::IoError(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Cache file not found",
        )));
    }

    // Use ScopeGraphIndex::load which handles format detection
    match ScopeGraphIndex::load(cache_path) {
        Ok(Some(index)) => Ok(index),
        Ok(None) => {
            // None means legacy format was detected
            tracing::info!(
                cache_path = %cache_path.display(),
                "Legacy cache format detected, will rebuild"
            );
            Err(CacheError::LegacyFormat)
        }
        Err(e) => Err(CacheError::IoError(e)),
    }
}

/// Save an index to cache using the new binary format.
pub fn save_index(cache_path: &Path, index: &ScopeGraphIndex) -> Result<()> {
    index.save(cache_path).map_err(CacheError::IoError)
}

/// Save an index to cache asynchronously (in a background thread).
///
/// Returns immediately and spawns a thread to do the actual saving.
/// Useful for saving the index without blocking the main thread.
pub fn save_index_async(cache_path: std::path::PathBuf, index: ScopeGraphIndex) {
    std::thread::spawn(move || {
        if let Err(e) = save_index(&cache_path, &index) {
            tracing::warn!("Failed to save index cache: {}", e);
        }
    });
}

/// Check if a cache exists and return its metadata.
pub fn cache_exists(cache_path: &Path) -> bool {
    cache_path.exists()
}

/// Get cache file size in bytes.
pub fn cache_size(cache_path: &Path) -> Option<u64> {
    std::fs::metadata(cache_path).ok().map(|m| m.len())
}
