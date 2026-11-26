//! Worktree lifecycle methods (`workspace.create_worktree`,
//! `workspace.remove_worktree`, `workspace.apply_worktree`,
//! `workspace.worktree_*`).
use super::git::{ChangeType, GitFileChange};
use super::{RpcActivityClass, WorkspaceRpc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
/// Worktree creation strategy.
///
/// Mirrors `echo_agent_fast_worktree::CreationMode` but uses config-friendly naming
/// (lowercase strings in TOML / JSON).
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WorktreeType {
    /// Linked worktree via `git worktree add --no-checkout` + parallel CoW copy.
    #[default]
    Linked,
    /// Standalone repository copy with independent `.git/` directory.
    Standalone,
    /// Plain `git worktree add` with full checkout.
    Git,
}
impl std::str::FromStr for WorktreeType {
    type Err = ();
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "linked" => Ok(Self::Linked),
            "standalone" => Ok(Self::Standalone),
            "git" => Ok(Self::Git),
            _ => Err(()),
        }
    }
}
/// Summary of source worktree's dirty state
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirtyStateSummary {
    pub staged_count: u32,
    pub modified_count: u32,
    pub deleted_count: u32,
    pub untracked_count: u32,
    pub has_partially_staged: bool,
    pub skipped_dirs: Vec<String>,
}
/// Summary of changes copied to the new worktree
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopiedChangesSummary {
    pub staged_copied: u32,
    pub modified_copied: u32,
    pub untracked_copied: u32,
    pub deletions_applied: u32,
    pub warnings: Vec<String>,
}
/// Copy mode for worktree creation
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WorktreeCopyMode {
    /// Only committed files at HEAD (original behavior)
    Clean,
    /// Copy dirty files, skip large untracked dirs (recommended)
    #[default]
    Dirty,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateWorktreeRequest {
    pub session_id: String,
    pub source_path: String,
    #[serde(default)]
    pub worktree_path: Option<String>,
    #[serde(default = "default_copy_mode")]
    pub copy_mode: WorktreeCopyMode,
    /// Git ref (branch, tag, or commit SHA) to checkout in the worktree.
    /// If not specified, defaults to HEAD of the source repository.
    #[serde(default)]
    pub git_ref: Option<String>,
    /// Whether to copy ignored files in the background after creation
    #[serde(default)]
    pub copy_ignored_in_background: bool,
    /// Patterns to skip when copying ignored files (e.g., "*.log", ".cache/**")
    #[serde(default)]
    pub ignored_skip_patterns: Vec<String>,
    /// Worktree creation type: "linked", "standalone", or "git".
    /// If not specified, the agent's config default will be used.
    #[serde(default)]
    pub worktree_type: Option<WorktreeType>,
    /// Human-readable label for the worktree directory name.
    /// When absent, an automatic `YYYY-MM-DD-<uuid>` label is generated.
    #[serde(default)]
    pub label: Option<String>,
    /// When `Some(true)`, enable the grove worktree arm on the builder.
    /// Absent/false → copy. `nfsWorktree` / `nfs_worktree` are deserialize aliases.
    #[serde(default, alias = "nfsWorktree", alias = "nfs_worktree")]
    pub grove_worktree: Option<bool>,
}
impl WorkspaceRpc for CreateWorktreeRequest {
    const METHOD: &'static str = "workspace.create_worktree";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Mutation;
    type Response = Value;
}
fn default_copy_mode() -> WorktreeCopyMode {
    WorktreeCopyMode::Dirty
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum CreateWorktreeResponse {
    #[serde(rename = "creating")]
    Creating {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "worktreePath")]
        worktree_path: String,
        /// Working directory root of the source repo/worktree (via `workdir()`).
        /// Clients strip this prefix from `source_path` to compute the
        /// subdirectory offset inside the new worktree.
        #[serde(rename = "sourceGitRoot", skip_serializing_if = "Option::is_none")]
        source_git_root: Option<String>,
    },
    #[serde(rename = "exists")]
    Exists {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "worktreePath")]
        worktree_path: String,
        commit: String,
        /// Working directory root of the source repo/worktree (via `workdir()`).
        /// Clients strip this prefix from `source_path` to compute the
        /// subdirectory offset inside the new worktree.
        #[serde(rename = "sourceGitRoot", skip_serializing_if = "Option::is_none")]
        source_git_root: Option<String>,
    },
}
/// `workspace.worktree_create_sync` — synchronous worktree creation; the
/// params are a [`CreateWorktreeRequest`] (transparent).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorktreeCreateSyncReq(pub CreateWorktreeRequest);
impl WorkspaceRpc for WorktreeCreateSyncReq {
    const METHOD: &'static str = "workspace.worktree_create_sync";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Mutation;
    type Response = Value;
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveWorktreeRequest {
    /// Legacy field: direct path to the worktree directory.
    #[serde(default)]
    pub worktree_path: Option<String>,
    /// New field: worktree ID or filesystem path, resolved via DB first.
    #[serde(default)]
    pub id_or_path: Option<String>,
    #[serde(default)]
    pub force: bool,
    #[serde(default)]
    pub dry_run: bool,
}
impl WorkspaceRpc for RemoveWorktreeRequest {
    const METHOD: &'static str = "workspace.remove_worktree";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Mutation;
    type Response = Value;
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveWorktreeResponse {
    pub removed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_path: Option<String>,
}
/// Response from creating a worktree from another worktree.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateWorktreeFromWorktreeResponse {
    pub status: String,
    pub new_session_id: String,
    pub worktree_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copied_changes: Option<CopiedChangesSummary>,
    /// Working directory root of the source repo/worktree (via `workdir()`).
    /// Clients strip this prefix from `source_worktree_path` to compute the
    /// subdirectory offset inside the new worktree.
    #[serde(rename = "sourceGitRoot", skip_serializing_if = "Option::is_none")]
    pub source_git_root: Option<String>,
}
/// Wire mirror of the heavy crate's `CreateWorktreeFromWorktreeRequest`.
///
/// Drops the two `#[serde(skip)]` runtime-only fields
/// (`cancellation_token: tokio_util::sync::CancellationToken` and
/// `resolved_dest_path`) so this lean crate avoids a `tokio_util` dependency.
/// Those fields are already absent from the wire, so the serde shape is
/// byte-identical; the server re-adds them as `None` when converting back.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateWorktreeFromWorktreeRequestWire {
    pub source_worktree_path: String,
    pub new_session_id: String,
    #[serde(default = "default_copy_mode")]
    pub copy_mode: WorktreeCopyMode,
    #[serde(default)]
    pub git_ref: Option<String>,
    #[serde(default)]
    pub worktree_type: Option<WorktreeType>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default, alias = "nfsWorktree", alias = "nfs_worktree")]
    pub grove_worktree: Option<bool>,
}
/// `workspace.worktree_create_from_worktree_sync` — synchronous worktree fork.
///
/// Unlike [`WorktreeCreateSyncReq`] this is **not** `#[serde(transparent)]`, so
/// the wire form keeps the `{ "inner": { … } }` wrapper.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateWorktreeFromWorktreeSyncReq {
    pub inner: CreateWorktreeFromWorktreeRequestWire,
}
impl WorkspaceRpc for CreateWorktreeFromWorktreeSyncReq {
    const METHOD: &'static str = "workspace.worktree_create_from_worktree_sync";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Mutation;
    type Response = CreateWorktreeFromWorktreeResponse;
}
/// Serializable version of `PrepareWorktreeResult` for wire transport.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrepareWorktreeFromWorktreeResponse {
    pub spawn_task: bool,
    /// Serialized `CreateWorktreeResponse` on success.
    pub response: Option<serde_json::Value>,
    pub error: Option<String>,
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ApplyMode {
    #[default]
    Overwrite,
    Merge,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyWorktreeRequest {
    pub session_id: String,
    pub worktree_path: String,
    #[serde(default)]
    pub mode: ApplyMode,
}
impl WorkspaceRpc for ApplyWorktreeRequest {
    const METHOD: &'static str = "workspace.apply_worktree";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Mutation;
    type Response = Value;
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileConflict {
    pub path: String,
    #[serde(rename = "type")]
    pub change_type: ChangeType,
    pub base: Option<String>,
    pub ours: Option<String>,
    pub theirs: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum ApplyWorktreeResponse {
    #[serde(rename = "success")]
    Success {
        files: Vec<GitFileChange>,
        #[serde(rename = "gitRoot")]
        git_root: String,
    },
    #[serde(rename = "conflicts")]
    Conflicts {
        files: Vec<GitFileChange>,
        conflicts: Vec<FileConflict>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeShowReq {
    pub id_or_path: String,
}
impl WorkspaceRpc for WorktreeShowReq {
    const METHOD: &'static str = "workspace.worktree_show";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = Value;
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeGcReq {
    #[serde(default)]
    pub dry_run: bool,
    pub max_age_secs: Option<i64>,
    #[serde(default)]
    pub force: bool,
}
impl WorkspaceRpc for WorktreeGcReq {
    const METHOD: &'static str = "workspace.worktree_gc";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = Value;
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorktreeListReq {
    pub repo: Option<String>,
    #[serde(default, rename = "type")]
    pub types: Vec<String>,
    #[serde(default)]
    pub include_all: bool,
}
impl WorkspaceRpc for WorktreeListReq {
    const METHOD: &'static str = "workspace.worktree_list";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = Value;
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeDbRebuildReq {}
impl WorkspaceRpc for WorktreeDbRebuildReq {
    const METHOD: &'static str = "workspace.worktree_db_rebuild";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = Value;
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeDbPathReq {}
impl WorkspaceRpc for WorktreeDbPathReq {
    const METHOD: &'static str = "workspace.worktree_db_path";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = WorktreeDbPathResponse;
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeDbPathResponse {
    pub path: Option<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorktreeDbStatsReq {}
impl WorkspaceRpc for WorktreeDbStatsReq {
    const METHOD: &'static str = "workspace.worktree_db_stats";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = Value;
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeDetachReq {
    pub id_or_path: String,
    #[serde(default)]
    pub allow_copy: bool,
}
impl WorkspaceRpc for WorktreeDetachReq {
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Mutation;
    const METHOD: &'static str = "workspace.worktree_detach";
    type Response = Value;
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeSalvageReq {
    pub id_or_path: String,
    pub out: String,
}
impl WorkspaceRpc for WorktreeSalvageReq {
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Mutation;
    const METHOD: &'static str = "workspace.worktree_salvage";
    type Response = Value;
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeCleanArtifactsReq {
    pub id_or_path: String,
}
impl WorkspaceRpc for WorktreeCleanArtifactsReq {
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Mutation;
    const METHOD: &'static str = "workspace.worktree_clean_artifacts";
    type Response = Value;
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn method_constants() {
        assert_eq!(CreateWorktreeRequest::METHOD, "workspace.create_worktree");
        assert_eq!(
            WorktreeCreateSyncReq::METHOD,
            "workspace.worktree_create_sync"
        );
        assert_eq!(RemoveWorktreeRequest::METHOD, "workspace.remove_worktree");
        assert_eq!(ApplyWorktreeRequest::METHOD, "workspace.apply_worktree");
        assert_eq!(WorktreeShowReq::METHOD, "workspace.worktree_show");
        assert_eq!(WorktreeGcReq::METHOD, "workspace.worktree_gc");
        assert_eq!(WorktreeListReq::METHOD, "workspace.worktree_list");
        assert_eq!(
            WorktreeDbRebuildReq::METHOD,
            "workspace.worktree_db_rebuild"
        );
        assert_eq!(WorktreeDbPathReq::METHOD, "workspace.worktree_db_path");
        assert_eq!(WorktreeDbStatsReq::METHOD, "workspace.worktree_db_stats");
        assert_eq!(
            CreateWorktreeFromWorktreeSyncReq::METHOD,
            "workspace.worktree_create_from_worktree_sync"
        );
    }
    #[test]
    fn create_worktree_from_worktree_sync_req_keeps_inner_wrapper() {
        let req = CreateWorktreeFromWorktreeSyncReq {
            inner: CreateWorktreeFromWorktreeRequestWire {
                source_worktree_path: "/src".into(),
                new_session_id: "s2".into(),
                copy_mode: WorktreeCopyMode::Dirty,
                git_ref: None,
                worktree_type: None,
                label: None,
                grove_worktree: None,
            },
        };
        let json = serde_json::to_value(&req).unwrap();
        let inner = json.get("inner").expect("inner wrapper present");
        assert_eq!(inner["sourceWorktreePath"], "/src");
        assert_eq!(inner["copyMode"], "dirty");
        assert!(inner.get("cancellationToken").is_none());
        assert!(inner.get("resolvedDestPath").is_none());
    }
    #[test]
    fn worktree_create_sync_req_is_transparent() {
        let req = WorktreeCreateSyncReq(CreateWorktreeRequest {
            session_id: "s1".into(),
            source_path: "/repo".into(),
            worktree_path: None,
            copy_mode: WorktreeCopyMode::Dirty,
            git_ref: None,
            copy_ignored_in_background: false,
            ignored_skip_patterns: vec![],
            worktree_type: None,
            label: None,
            grove_worktree: None,
        });
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["sessionId"], "s1");
        assert_eq!(json["sourcePath"], "/repo");
        assert!(json.get("inner").is_none());
    }
    #[test]
    fn worktree_type_from_str_round_trip() {
        use std::str::FromStr;
        assert_eq!(WorktreeType::from_str("linked"), Ok(WorktreeType::Linked));
        assert_eq!(
            WorktreeType::from_str("standalone"),
            Ok(WorktreeType::Standalone)
        );
        assert_eq!(WorktreeType::from_str("git"), Ok(WorktreeType::Git));
        assert_eq!(WorktreeType::from_str("bogus"), Err(()));
    }
    #[test]
    fn create_worktree_response_status_tagged() {
        let resp = CreateWorktreeResponse::Creating {
            session_id: "s1".into(),
            worktree_path: "/wt".into(),
            source_git_root: None,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["status"], "creating");
        assert_eq!(json["sessionId"], "s1");
        assert_eq!(json["worktreePath"], "/wt");
        assert!(json.get("sourceGitRoot").is_none());
    }
}
