//! Overlay-on-FUSE worktree support.
//!
//! When the source repo is on a FUSE+overlayfs stack (repo-fuse), we can
//! create worktrees via a new overlay mount that shares the same FUSE lower
//! layer, with a btrfs snapshot of the current upper dir as the new upper.
//! This gives O(1) worktree creation without any file copies.

pub(crate) mod detect;
pub(crate) mod snapshot;

pub(crate) use detect::{OverlayInfo, detect_fuse_overlay};
pub(crate) use snapshot::{
    cleanup_orphaned_overlay_snapshots, create_overlay_worktree, remove_overlay_worktree,
    try_remove_from_metadata, try_remove_from_mountinfo,
};
