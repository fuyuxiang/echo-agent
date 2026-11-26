//! Local-filesystem event source. Single causal stream of wire-ready
//! [`FsEvent`]s on one broadcast channel. The `echo-agent-workspace` layer
//! translates these into `WorkspaceEvent`s with git-enrichment I/O.
//!
//! Single workspace root only; multi-root composition (parent + worktrees)
//! lives in the workspace layer.

mod checkout;
mod error;
mod event;
mod handle;
mod install;
mod merge;
mod paths;
mod registry;
mod selection;
mod source;
mod state;
mod vcs;
mod watcher;

pub use checkout::watch_root_covers;
pub use error::FsNotifyError;
pub use event::{FsEvent, FsEventKind, GitMetaKind};
pub use registry::{FsWatcherStats, STATS_TARGET, set_runtime_handle, shared, stats};
pub use source::{FsConfig, FsEventSource};
pub use state::SETTLE_MS;
