//! Git operations used by fast worktree creation.

pub(crate) mod checkout;
pub(crate) mod dirs;
pub(crate) mod discovery;
pub(crate) mod index;
pub(crate) mod probe;
pub(crate) mod reason;
pub(crate) mod reclaimed;
pub(crate) mod safety;
pub(crate) mod status;
pub(crate) mod worktree;

pub(crate) use checkout::checkout_ref;
pub(crate) use checkout::{git_clean_fd, git_reset_hard_command};
// Only consumed by the Linux-only snapshot finalize path.
#[cfg(target_os = "linux")]
pub(crate) use checkout::{has_staged_changes, worktree_at_ref, worktree_has_tracked_changes};
pub(crate) use discovery::{find_worktree_root, get_head_commit};
pub(crate) use index::{copy_git_index, update_index_stats};
// Consumed only by the metadata-gated GC path (api/gc.rs); reclaimed_tests reach
// the definitions via `super::`, so the re-export is unused without the feature.
#[cfg(feature = "metadata")]
pub(crate) use reclaimed::{RECLAIMED_LIFETIME, collect_reclaimed_names, reclaimable_within};
pub use reclaimed::{Reclaim, reclaimable_after_snapshot};
pub use safety::KeepReason;
// `Safety` is reached in production via `super::safety::Safety`; this re-export
// serves only `test_support` (test + metadata).
#[cfg(all(test, feature = "metadata"))]
pub(crate) use safety::Safety;
#[cfg(test)]
pub(crate) use safety::safe_to_delete_worktree;
pub(crate) use status::get_modified_files;
pub(crate) use worktree::{
    normalized_for_match, registration_worktree_path, worktree_add_no_checkout,
};
pub use worktree::{remove_stale_worktree_registration, remove_stale_worktree_registrations_under};
