//! Index management: building, caching, locking, and updating.

mod builder;
pub mod cache;
pub mod lock;

pub use builder::{IndexBuilder, IndexError, Result};
pub use cache::{
    CACHE_FILE_NAME, CacheError, cache_exists, cache_size, get_cache_path, load_index, save_index,
    save_index_async,
};
pub use lock::{
    IndexOperation, LockResult, WorkspaceLockGuard, is_operation_in_progress, try_lock,
};
