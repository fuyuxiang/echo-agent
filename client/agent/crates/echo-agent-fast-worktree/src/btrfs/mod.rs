//! BTRFS snapshot support for fast worktree creation.
//!
//! On Linux systems where the source repo is on a BTRFS subvolume,
//! we can use BTRFS snapshots for O(1) worktree creation instead of
//! file-by-file CoW cloning.
//!
//! The snapshot creates a complete standalone git repository (not a
//! git worktree), which is immediately usable without any fixup.
//!
//! This module also handles the case where the BTRFS subvolume is accessed
//! via a bind mount (e.g. when the working-tree path is not itself on BTRFS).
//! In such cases, snapshots are created inside the BTRFS mount point and
//! exposed at the expected destination via a symlink.

pub mod detect;
pub mod snapshot;

pub use detect::{BtrfsInfo, is_btrfs, is_btrfs_subvolume};
pub use snapshot::{
    BTRFS_META_SUFFIX, BTRFS_SNAPSHOT_SUBDIRS, BtrfsSnapshotMetadata, SnapshotMetaState,
    btrfs_meta_path, create_snapshot, create_snapshot_with_symlink, create_worktree_symlink,
    delete_snapshot, is_safe_snapshot_delete_target, remove_btrfs_metadata, snapshot_dest_path,
    snapshot_meta_state, snapshot_meta_targets, write_btrfs_metadata,
};
