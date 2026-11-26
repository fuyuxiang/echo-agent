//! Shared editor infrastructure used by the default echo_agent_build tools.
pub mod file_operation_lock;
pub use file_operation_lock::{FileOperationLockGuard, FileOperationLockManager};
/// Default `block_until_ms` when the model omits the field on blocking shell
/// tools. Shared so ACP UI labels and tool implementations stay aligned.
pub const DEFAULT_BLOCK_UNTIL_MS: u64 = 30_000;
