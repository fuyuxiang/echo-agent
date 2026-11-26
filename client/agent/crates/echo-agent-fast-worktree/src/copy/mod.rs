//! Filesystem replication engine used by fast worktree creation.

pub(crate) mod cow;
pub(crate) mod engine;
pub(crate) mod gitdir;
pub(crate) mod shard;
pub(crate) mod skip;
pub(crate) mod standalone;
pub(crate) mod types;
pub(crate) mod worker;

pub(crate) use engine::copy_parallel;
pub(crate) use skip::collect_unignored_paths;
pub use types::CopyStats;
pub use types::DirtyFilesReport;
pub(crate) use types::ParallelCopyConfig;
