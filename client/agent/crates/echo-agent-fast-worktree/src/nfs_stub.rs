//! Windows stand-in for `nfs/`. Grove worktrees are FUSE/NFS-only.
//!
//! Public builder types stay so `--features grove` still type-checks; every
//! arm declines and removal is a no-op.
#![allow(dead_code)]
use crate::RemoveReport;
use crate::worktree::CreateWorktreeResult;
use crate::worktree::plan::WorktreePlan;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
#[path = "nfs/create_latency_stamp.rs"]
pub mod create_latency_stamp;
pub const WORKTREE_BACKING_DIR: &str = "worktree-backing";
#[derive(Clone, Debug)]
pub struct NfsWorktreeOpts {
    pub enabled: bool,
    pub control_sock: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
    pub runtime_dir: Option<PathBuf>,
    pub ping_timeout: Duration,
    pub create_timeout: Duration,
    pub query_timeout: Duration,
    pub query_interval: Duration,
}
impl Default for NfsWorktreeOpts {
    fn default() -> Self {
        Self {
            enabled: false,
            control_sock: None,
            data_dir: None,
            runtime_dir: None,
            ping_timeout: Duration::from_millis(250),
            create_timeout: Duration::from_secs(180),
            query_timeout: Duration::from_secs(30),
            query_interval: Duration::from_millis(50),
        }
    }
}
#[derive(Debug, Clone)]
pub struct NfsAdopted {
    pub dest: PathBuf,
    pub mount_id: String,
    pub port: u16,
    pub transport: String,
}
#[derive(Debug)]
pub enum NfsCreateDecision {
    Adopted(NfsAdopted),
    Fallback,
}
#[derive(Debug)]
pub struct NfsWorktreeClient;
impl NfsWorktreeClient {
    #[must_use]
    pub fn from_opts(_opts: &NfsWorktreeOpts) -> Self {
        Self
    }
    pub fn detach_worktree(&self, _dest: &Path, _allow_copy: bool) -> Result<DetachReply> {
        anyhow::bail!("not available on this platform")
    }
    pub fn salvage_worktree(&self, _dest: &Path, _out: &Path) -> Result<SalvageReply> {
        anyhow::bail!("not available on this platform")
    }
    pub fn clean_artifacts(&self, _dest: &Path) -> Result<CleanArtifactsReply> {
        anyhow::bail!("not available on this platform")
    }
    pub fn status_for_dir(&self, _dest: &Path) -> Option<NfsStatusView> {
        None
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetachReply {
    pub phase: String,
    pub same_device: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SalvageReply {
    pub virtual_remaining: Vec<String>,
    pub gitdir_copied: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanArtifactsReply {
    pub purged_entries: u64,
    pub no_escapes: bool,
}
#[derive(Debug, Clone)]
pub struct NfsStatusView {
    pub hydration_percent: Option<f64>,
    pub raw: Option<serde_json::Value>,
    pub port: Option<u16>,
    pub mount_id: Option<String>,
    pub transport: Option<String>,
}
pub fn try_nfs_remove(_worktree_path: &Path) -> Result<Option<RemoveReport>> {
    Ok(None)
}
pub(crate) fn is_safe_worktree_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('.')
        && !id.contains('/')
        && !id.contains('\\')
        && !id.contains('\0')
}
pub(crate) fn try_grove_worktree(_plan: &WorktreePlan) -> Result<Option<CreateWorktreeResult>> {
    Ok(None)
}
pub(crate) fn nfs_error_blocks_fallback(_err: &anyhow::Error) -> bool {
    false
}
pub(crate) fn default_grove_creation_mode() -> &'static str {
    crate::worktree::STRATEGY_GROVE_NFS
}
pub fn dest_is_known_unmounted(_path: &Path) -> bool {
    true
}
pub fn dest_is_mountpoint(_path: &Path) -> bool {
    false
}
pub fn dest_is_nfs_mount(_path: &Path) -> bool {
    false
}
pub fn dest_is_projected_mount(_path: &Path) -> bool {
    false
}
pub(crate) fn dest_paths_equivalent(a: &Path, b: &Path) -> bool {
    a == b
}
pub(crate) fn dest_path_contains(parent: &Path, child: &Path) -> bool {
    child.starts_with(parent)
}
#[cfg(feature = "metadata")]
mod metadata {
    use crate::db::WorktreeRecord;
    use anyhow::Result;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    pub const RANK_DB: u8 = 0;
    #[derive(Debug, Clone)]
    pub struct NfsIdentity {
        pub worktree_id: String,
        pub dest: Option<PathBuf>,
        pub source_repo: Option<PathBuf>,
        pub pin_ref: Option<String>,
        pub backing: Option<PathBuf>,
        pub mount_id: Option<i64>,
        pub rank: u8,
        pub phase: Option<String>,
    }
    #[derive(Debug, Default)]
    pub struct PinGcReport {
        pub examined: u64,
        pub pruned: u64,
        pub deferred_grace: u64,
        pub kept_live: u64,
        pub pruned_ids: Vec<String>,
    }
    pub fn candidate_data_dirs() -> Vec<PathBuf> {
        Vec::new()
    }
    pub fn nfs_record_is_dead(_dest: &Path, _backing: Option<&Path>) -> bool {
        true
    }
    pub fn identities_from_worktree_records(_recs: &[WorktreeRecord]) -> Vec<NfsIdentity> {
        Vec::new()
    }
    pub fn collect_identities(
        _data_dir: &Path,
        _worktrees: &[NfsIdentity],
    ) -> HashMap<String, NfsIdentity> {
        HashMap::new()
    }
    pub fn merge_nfs_identities(
        _into: &mut HashMap<String, NfsIdentity>,
        _src: impl IntoIterator<Item = NfsIdentity>,
    ) {
    }
    pub fn gc_orphan_pins(
        _data_dir: &Path,
        _worktrees: &[NfsIdentity],
        _now: i64,
        _dry_run: bool,
    ) -> Result<PinGcReport> {
        Ok(PinGcReport::default())
    }
}
#[cfg(feature = "metadata")]
pub use metadata::*;
