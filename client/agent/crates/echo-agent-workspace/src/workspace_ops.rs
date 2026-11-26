//! [`WorkspaceOps`] — dual-mode workspace operations handle.
//!
//! Two modes:
//!
//! - **`Local`** — extensions dispatch through [`WorkspaceHandle`]; tool
//!   calls dispatch through the workspace session's [`FinalizedToolset`].
//!   The toolset is installed via [`WorkspaceOps::bind_local_session`]
//!   after the agent is built.
//!
//! - **`Proxy`** — everything routes through hub WebSocket to a remote
//!   workspace server.
//!
//! ## Type safety
//!
//! Each RPC method has a corresponding request struct that implements
//! [`WorkspaceRpc`]. The struct carries a `METHOD` constant and derives
//! `Serialize + Deserialize`. Both the proxy client (`WorkspaceOps`) and
//! the server (`WorkspaceRpcHandler::dispatch`) use the same struct —
//! add/rename a field and the compiler catches both sides.
use crate::error::{WorkspaceError, WorkspaceResult};
use crate::file_system::ContentSearchRequest;
use crate::handle::WorkspaceHandle;
use crate::worktree::{ApplyWorktreeRequest, CreateWorktreeRequest, RemoveWorktreeRequest};
use async_trait::async_trait;
use echo_agent_computer_hub_sdk::ToolHarness;
use echo_agent_tools::types::output::ToolRunResult;
use echo_agent_workspace_client::{WorkspaceClient, is_transport_fatal};
pub use echo_agent_workspace_types::rpc::agents_md::DiscoverAgentsMdReq;
pub use echo_agent_workspace_types::rpc::code_nav::{
    CodeFindDefinitionsReq, CodeFindReferencesReq, CodeGotoDefinitionReq, CodeGotoReferencesReq,
    CodeIndexStats, CodeIndexStatusReq, CodeIndexStatusResponse, CodeNavLocation, CodeNavResponse,
};
pub use echo_agent_workspace_types::rpc::export_github::ExportGithubReq;
pub use echo_agent_workspace_types::rpc::fs::{
    ClientFsListNode, ClientFsListReq, ClientFsListRes, ClientFsReadFileReq, ClientFsReadFileRes,
    ClientFsStatReq, ClientFsStatRes, GetFileEntry, GetFileResult, GetFilesReq, GetFilesRes,
    PutFileEntry, PutFileResult, PutFilesReq, PutFilesRes,
};
pub use echo_agent_workspace_types::rpc::git::{
    BinaryFileInfoData, CheckoutCommitResponse, CommitWithPatchData, DetectVcsKindReq,
    DiffStatsSummary, GitBranchesReq, GitCheckoutCommitReq, GitCheckoutReq, GitCollectChangesReq,
    GitCollectChangesResponse, GitCommitReq, GitCurrentCommitReq, GitDiffReq, GitDiscardReq,
    GitEnsureBindingReq, GitFilesReq, GitInfoReq, GitMergeToMainReq, GitPushReq, GitResolveRootReq,
    GitStageContentReq, GitStageReq, GitStashReq, GitStatusExtReq, GitStatusExtResponse,
    GitStatusFormat, GitStatusReq, GitSyncBaseReq, GitUnstageReq, IdentityData, PublicBaseData,
    RepoInfo, UNTRACKED_CONTENT_THRESHOLD, UncommittedChangesData, UntrackedFileData,
};
pub use echo_agent_workspace_types::rpc::hooks::{
    HookEventNameWire, HookRegistryReq, HookRegistryWire, HookSpecWire,
};
pub use echo_agent_workspace_types::rpc::hunks::{
    BulkHunkActionResponse, FileContentEntryWire, FileContentStatusWire, FileContentViewWire,
    FileSummary, FilteredHunksResponse, HunkActionKind, HunkActionReq, HunkActionResponse,
    HunkAllActionReq, HunkFileActionReq, HunkGetAllFileContentsReq, HunkGetAllHunksReq,
    HunkGetFileSummariesReq, HunkGetFilteredHunksReq, HunkGetSessionSummaryReq,
    HunkGetStagedFilesReq, HunkLineInfoWire, HunkSingleActionReq, HunkSourceWire,
    HunkTurnActionReq, HunkWire, SessionStatsWire, SessionSummaryWire, TurnSummaryWire,
};
pub use echo_agent_workspace_types::rpc::repos::{
    ProvisionedRepo, RepoManifest, ReposListReq, ReposListResponse,
};
pub use echo_agent_workspace_types::rpc::search::{FuzzyChangeReq, FuzzyCloseReq, FuzzyOpenReq};
pub use echo_agent_workspace_types::rpc::session::{BeginPromptReq, EndPromptReq, RewindToReq};
pub use echo_agent_workspace_types::rpc::skills::DiscoverSkillsReq;
pub use echo_agent_workspace_types::rpc::workspace::WorkspaceInfoReq;
pub use echo_agent_workspace_types::rpc::worktree::{
    CreateWorktreeFromWorktreeRequestWire, CreateWorktreeFromWorktreeSyncReq,
    PrepareWorktreeFromWorktreeResponse, WorktreeCleanArtifactsReq, WorktreeDbPathReq,
    WorktreeDbPathResponse, WorktreeDbRebuildReq, WorktreeDbStatsReq, WorktreeDetachReq,
    WorktreeGcReq, WorktreeListReq, WorktreeSalvageReq, WorktreeShowReq,
};
pub use echo_agent_workspace_types::rpc::{RpcActivityClass, WorkspaceRpc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
/// Implements [`WorkspaceRpc`] for request types whose responses
/// reference crate-internal types and so cannot live in the types crate.
/// The activity class is a required argument for the same reason the trait
/// const has no default: every method's author must decide.
macro_rules! workspace_rpc {
    ($ty:ty, $method:literal, $resp:ty, $activity:ident) => {
        impl crate::workspace_ops::WorkspaceRpc for $ty {
            const METHOD: &'static str = $method;
            const ACTIVITY: crate::workspace_ops::RpcActivityClass =
                crate::workspace_ops::RpcActivityClass::$activity;
            type Response = $resp;
        }
    };
}
/// Typed workspace operation: the wire contract (`METHOD`, `Response`)
/// comes from the [`WorkspaceRpc`] supertrait; this adds local-mode
/// `execute()`. In proxy mode the op is serialized through the server RPC.
#[async_trait]
pub trait WorkspaceOp: WorkspaceRpc + DeserializeOwned + Send + Sync {
    /// Execute the operation locally against the workspace handle.
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response>;
}
/// Prepare a worktree fork from an existing worktree (validation + path resolution).
/// Returns a serialized result with `spawn_task` flag and the response.
fn hub_transfer_client() -> WorkspaceResult<reqwest::Client> {
    echo_agent_extra_ca::build_reqwest_client(|builder| {
        builder.timeout(std::time::Duration::from_secs(600))
    })
    .map_err(|e| WorkspaceError::HubError(format!("failed to create HTTP client: {e}")))
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrepareWorktreeFromWorktreeReq {
    pub inner: crate::worktree::CreateWorktreeFromWorktreeRequest,
}
#[async_trait]
impl WorkspaceOp for ExportGithubReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        if std::path::Path::new(&self.project_dir).is_absolute() {
            return Err(WorkspaceError::HubError(
                "project_dir must be relative to the workspace root".into(),
            ));
        }
        let canonical_root = ws.canonical_root().await?;
        let project_dir = ws
            .resolve_service_path(&self.project_dir, &canonical_root)
            .await?;
        crate::export_github::run_export(crate::export_github::ExportGithubParams {
            project_dir: &project_dir,
            repo_full_name: self.repo_full_name.as_deref(),
            remote_url_base: "https://github.com",
            web_url_base: "https://github.com",
            branch: self.branch.as_deref(),
            commit_message: self.commit_message.as_deref(),
        })
        .await
        .map_err(|failure| WorkspaceError::ExportGithub {
            kind: failure.kind,
            message: failure.message,
        })
    }
}
/// Get all rewind points for the session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetRewindPointsReq {
    pub session_id: String,
}
impl WorkspaceRpc for GetRewindPointsReq {
    const METHOD: &'static str = "workspace.get_rewind_points";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = Vec<crate::session::file_state::RewindPoint>;
}
fn hunk_line_info_to_wire(info: &echo_agent_hunk_tracker::types::HunkLineInfo) -> HunkLineInfoWire {
    HunkLineInfoWire {
        old_start: info.old_start,
        old_count: info.old_count,
        new_start: info.new_start,
        new_count: info.new_count,
    }
}
fn hunk_source_to_wire(source: echo_agent_hunk_tracker::types::HunkSource) -> HunkSourceWire {
    use echo_agent_hunk_tracker::types::HunkSource as S;
    match source {
        S::AgentEdit { prompt_index } => HunkSourceWire::AgentEdit { prompt_index },
        S::ExternalEditOnAgentFile => HunkSourceWire::ExternalEditOnAgentFile,
        S::External => HunkSourceWire::External,
    }
}
fn hunk_to_wire(hunk: &echo_agent_hunk_tracker::types::Hunk) -> HunkWire {
    HunkWire {
        id: hunk.id.as_str().to_owned(),
        path: hunk.path.clone(),
        line_info: hunk_line_info_to_wire(&hunk.line_info),
        source: hunk_source_to_wire(hunk.source),
        old_text: hunk.old_text.clone(),
        new_text: hunk.new_text.clone(),
        patch: hunk.patch.clone(),
        created_at: hunk.created_at,
    }
}
fn file_content_status_to_wire(
    status: echo_agent_hunk_tracker::types::FileContentStatus,
) -> FileContentStatusWire {
    use echo_agent_hunk_tracker::types::FileContentStatus as S;
    match status {
        S::Missing => FileContentStatusWire::Missing,
        S::Binary => FileContentStatusWire::Binary,
        S::TooLarge => FileContentStatusWire::TooLarge,
        S::LfsPointer => FileContentStatusWire::LfsPointer,
        S::Symlink => FileContentStatusWire::Symlink,
        S::Full => FileContentStatusWire::Full,
    }
}
fn file_content_view_to_wire(
    view: echo_agent_hunk_tracker::types::FileContentView,
) -> FileContentViewWire {
    FileContentViewWire {
        status: file_content_status_to_wire(view.status),
        byte_len: view.byte_len,
        content: view.content,
    }
}
fn file_content_entry_to_wire(
    entry: echo_agent_hunk_tracker::FileContentEntry,
) -> FileContentEntryWire {
    FileContentEntryWire {
        path: entry.path,
        baseline: file_content_view_to_wire(entry.baseline),
        current: file_content_view_to_wire(entry.current),
        is_agent_file: entry.is_agent_file,
        staged: entry.staged,
    }
}
fn session_stats_to_wire(stats: &echo_agent_hunk_tracker::types::SessionStats) -> SessionStatsWire {
    SessionStatsWire {
        accepted_hunks: stats.accepted_hunks,
        rejected_hunks: stats.rejected_hunks,
        accepted_lines_added: stats.accepted_lines_added,
        accepted_lines_removed: stats.accepted_lines_removed,
        rejected_lines_added: stats.rejected_lines_added,
        rejected_lines_removed: stats.rejected_lines_removed,
    }
}
fn turn_summary_to_wire(turn: echo_agent_hunk_tracker::types::TurnSummary) -> TurnSummaryWire {
    TurnSummaryWire {
        prompt_index: turn.prompt_index,
        files: turn.files,
        pending_hunks: turn.pending_hunks.iter().map(|h| hunk_to_wire(h)).collect(),
        lines_added: turn.lines_added,
        lines_removed: turn.lines_removed,
    }
}
fn session_summary_to_wire(summary: echo_agent_hunk_tracker::SessionSummary) -> SessionSummaryWire {
    SessionSummaryWire {
        stats: session_stats_to_wire(&summary.stats),
        turns: summary
            .turns
            .into_iter()
            .map(turn_summary_to_wire)
            .collect(),
        files_modified: summary.files_modified,
        files_with_pending: summary.files_with_pending,
        pending_hunks: summary.pending_hunks,
        pending_lines_added: summary.pending_lines_added,
        pending_lines_removed: summary.pending_lines_removed,
        unattributed_pending: summary.unattributed_pending,
    }
}
/// Convert a wire [`HunkActionKind`] to the hunk-tracker crate's `HunkAction`.
fn tracker_action(kind: HunkActionKind) -> echo_agent_hunk_tracker::types::HunkAction {
    match kind {
        HunkActionKind::Accept => echo_agent_hunk_tracker::types::HunkAction::Accept,
        HunkActionKind::Reject => echo_agent_hunk_tracker::types::HunkAction::Reject,
    }
}
/// Access the per-session hunk tracker; the op must carry a session.
fn session_tracker(
    ws: &WorkspaceHandle,
    session_id: Option<&str>,
) -> WorkspaceResult<echo_agent_hunk_tracker::HunkTrackerHandle> {
    let sid = session_id
        .ok_or_else(|| WorkspaceError::HubError("per-session hunk op requires a session".into()))?;
    let session = ws
        .session(sid)
        .ok_or_else(|| WorkspaceError::SessionNotFound(sid.to_owned()))?;
    Ok(session.hunk_tracker().clone())
}
/// Ancestor hop budget when locating `.echo-agent/repos.json`.
///
/// Grove rewrite is one hop (`/workspace/app` → `/workspace`). Desktop
/// workspaces can sit deeper than that; this is a backstop only. Primary
/// bounds are the sandbox root (`/workspace`) and the user-global echoagent home.
const REPOS_MANIFEST_MAX_ANCESTOR_HOPS: usize = 16;
/// Directories to probe for [`REPOS_MANIFEST_RELATIVE_PATH`], starting at
/// `root_cwd` (post-grove-rewrite agent cwd) and walking up.
///
/// Does not escape the sandbox workspace or load `~/.echo-agent/repos.json` /
/// `$ECHO_AGENT_HOME/repos.json` (user-global, not a provisioned workspace).
fn repos_manifest_search_dirs(start: &std::path::Path) -> Vec<std::path::PathBuf> {
    let rel = echo_agent_workspace_types::rpc::repos::REPOS_MANIFEST_RELATIVE_PATH;
    #[allow(deprecated)]
    let home = std::env::home_dir();
    let mut global_manifests = Vec::with_capacity(2);
    if let Some(v) = std::env::var_os("ECHO_AGENT_HOME")
        && !v.is_empty()
    {
        global_manifests.push(std::path::PathBuf::from(v).join("repos.json"));
    }
    if let Some(user_home) = echo_agent_config::user_echo_agent_home() {
        global_manifests.push(user_home.join("repos.json"));
    }
    let mut out = Vec::new();
    let mut dir = Some(start);
    for _ in 0..=REPOS_MANIFEST_MAX_ANCESTOR_HOPS {
        let Some(d) = dir else {
            break;
        };
        let manifest = d.join(rel);
        if global_manifests.iter().any(|g| g == &manifest) {
            break;
        }
        if home.as_deref() == Some(d) || crate::trust::is_home_dir(d) {
            break;
        }
        out.push(d.to_path_buf());
        if d == std::path::Path::new("/workspace") {
            break;
        }
        dir = d.parent();
    }
    out
}
#[async_trait]
impl WorkspaceOp for ReposListReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let rel = echo_agent_workspace_types::rpc::repos::REPOS_MANIFEST_RELATIVE_PATH;
        let start = ws.root_cwd()?;
        let mut manifest = RepoManifest::new(Vec::new());
        for d in repos_manifest_search_dirs(&start) {
            let path = d.join(rel);
            match tokio::fs::read(&path).await {
                Ok(bytes) => {
                    manifest = RepoManifest::from_json_bytes(&bytes).map_err(|e| {
                        WorkspaceError::HubError(format!(
                            "invalid repos manifest at {}: {e}",
                            path.display()
                        ))
                    })?;
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(WorkspaceError::HubError(format!(
                        "failed to read repos manifest at {}: {e}",
                        path.display()
                    )));
                }
            }
        }
        Ok(ReposListResponse {
            version: manifest.version,
            repos: manifest.repos,
        })
    }
}
/// Resolve the directory a git op runs in: the explicit `git_root` when the
/// caller provides one (the per-session repo, which the desktop sends per
/// window), else the workspace root. Without this, every session's git
/// queries/mutations would target the workspace launch directory's repo.
pub(crate) async fn git_op_cwd(
    ws: &WorkspaceHandle,
    git_root: &Option<std::path::PathBuf>,
) -> WorkspaceResult<std::path::PathBuf> {
    match git_root {
        Some(root) => Ok(root.clone()),
        None => ws.root_cwd(),
    }
}
/// Every provisioned mount (or the workspace root when none). Prompt, graph,
/// fs-notify, and turn-commit walk this list so multi-repo is not primary-only.
pub(crate) async fn materialized_git_roots(
    ws: &WorkspaceHandle,
) -> WorkspaceResult<Vec<std::path::PathBuf>> {
    let root = ws.root_cwd()?;
    let manifest = load_repos_manifest(&root).await?;
    Ok(manifest.materialized_mounts(&root))
}
async fn load_repos_manifest(root: &std::path::Path) -> WorkspaceResult<RepoManifest> {
    let path = root.join(echo_agent_workspace_types::rpc::repos::REPOS_MANIFEST_RELATIVE_PATH);
    match tokio::fs::read(&path).await {
        Ok(bytes) => RepoManifest::from_json_bytes(&bytes)
            .map_err(|e| WorkspaceError::HubError(e.to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(RepoManifest::new(Vec::new())),
        Err(e) => Err(WorkspaceError::HubError(e.to_string())),
    }
}
#[async_trait]
impl WorkspaceOp for GitStatusExtReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let cwd = git_op_cwd(ws, &self.git_root).await?;
        match self.format {
            GitStatusFormat::Structured => {
                let data = crate::session::git::status(
                    &cwd,
                    self.include_untracked,
                    self.include_stats,
                    self.ignore_submodules,
                    self.include_patches,
                )
                .await
                .map_err(|e| WorkspaceError::HubError(e.to_string()))?;
                Ok(GitStatusExtResponse::structured(data))
            }
            GitStatusFormat::Prompt => {
                let result = crate::file_system::git_status(cwd)
                    .await
                    .map_err(|e| WorkspaceError::HubError(e.to_string()))?;
                Ok(GitStatusExtResponse::prompt(result))
            }
        }
    }
}
#[async_trait]
impl WorkspaceOp for GitFilesReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let cwd = git_op_cwd(ws, &self.git_root).await?;
        crate::session::git::read_files(&cwd, &self.paths, &self.version)
            .await
            .map_err(|e| WorkspaceError::HubError(e.to_string()))
    }
}
#[async_trait]
impl WorkspaceOp for GitDiffReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let cwd = git_op_cwd(ws, &self.git_root).await?;
        crate::session::git::diffs(
            &cwd,
            self.paths.as_deref(),
            &self.from,
            &self.to,
            self.include_patch,
            self.include_content,
            self.merge_base,
        )
        .await
        .map_err(|e| WorkspaceError::HubError(e.to_string()))
    }
}
#[async_trait]
impl WorkspaceOp for GitStageReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let cwd = git_op_cwd(ws, &self.git_root).await?;
        crate::session::git::stage(&cwd, self.paths.clone())
            .await
            .map_err(|e| WorkspaceError::HubError(e.to_string()))
    }
}
#[async_trait]
impl WorkspaceOp for GitStageContentReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let cwd = git_op_cwd(ws, &self.git_root).await?;
        crate::session::git::stage_content(&cwd, &self.path, &self.content)
            .await
            .map_err(|e| WorkspaceError::HubError(e.to_string()))
    }
}
#[async_trait]
impl WorkspaceOp for GitUnstageReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let cwd = git_op_cwd(ws, &self.git_root).await?;
        crate::session::git::unstage(&cwd, self.paths.clone())
            .await
            .map_err(|e| WorkspaceError::HubError(e.to_string()))
    }
}
#[async_trait]
impl WorkspaceOp for GitDiscardReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let cwd = git_op_cwd(ws, &self.git_root).await?;
        crate::session::git::discard(&cwd, self.paths.clone(), self.scope, self.include_untracked)
            .await
            .map_err(|e| WorkspaceError::HubError(e.to_string()))
    }
}
#[async_trait]
impl WorkspaceOp for GitCommitReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let cwd = git_op_cwd(ws, &self.git_root).await?;
        crate::session::git::commit(&cwd, self)
            .await
            .map_err(|e| WorkspaceError::HubError(e.to_string()))
    }
}
#[async_trait]
impl WorkspaceOp for GitSyncBaseReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let cwd = git_op_cwd(ws, &self.git_root).await?;
        crate::session::git::sync_base(
            &cwd,
            self.base_ref.as_deref(),
            self.abort,
            self.expected_branch.as_deref(),
        )
        .await
        .map_err(|e| WorkspaceError::HubError(e.to_string()))
    }
}
#[async_trait]
impl WorkspaceOp for GitCheckoutReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let cwd = git_op_cwd(ws, &self.git_root).await?;
        crate::session::git::checkout_branch(&cwd, &self.branch, self.create)
            .await
            .map_err(|e| WorkspaceError::HubError(e.to_string()))
    }
}
#[async_trait]
impl WorkspaceOp for GitEnsureBindingReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let cwd = git_op_cwd(ws, &self.git_root).await?;
        crate::session::git::ensure_binding(&cwd, &self.session_branch, &self.base_ref)
            .await
            .map_err(|e| WorkspaceError::HubError(e.to_string()))
    }
}
#[async_trait]
impl WorkspaceOp for GitMergeToMainReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let cwd = git_op_cwd(ws, &self.git_root).await?;
        let target = self.required_target_branch().ok_or_else(|| {
            WorkspaceError::HubError(
                "target_branch is required; omit/empty is rejected (no silent main default)"
                    .to_string(),
            )
        })?;
        crate::session::git::merge_to_main(&cwd, &self.session_branch, target, self.push)
            .await
            .map_err(|e| WorkspaceError::HubError(e.to_string()))
    }
}
#[async_trait]
impl WorkspaceOp for GitPushReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let cwd = git_op_cwd(ws, &self.git_root).await?;
        crate::session::git::push_branch(&cwd, self.branch.as_deref())
            .await
            .map_err(|e| WorkspaceError::HubError(e.to_string()))
    }
}
#[async_trait]
impl WorkspaceOp for GitStashReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let cwd = git_op_cwd(ws, &self.git_root).await?;
        crate::session::git::stash(&cwd, self.include_untracked)
            .await
            .map_err(|e| WorkspaceError::HubError(e.to_string()))
    }
}
#[async_trait]
impl WorkspaceOp for GitInfoReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let cwd = git_op_cwd(ws, &self.git_root).await?;
        crate::session::git::git_info(&cwd)
            .await
            .map_err(|e| WorkspaceError::HubError(e.to_string()))
    }
}
#[async_trait]
impl WorkspaceOp for GitBranchesReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let cwd = git_op_cwd(ws, &self.git_root).await?;
        crate::session::git::list_branches(&cwd)
            .await
            .map_err(|e| WorkspaceError::HubError(e.to_string()))
    }
}
#[async_trait]
impl WorkspaceOp for GitCollectChangesReq {
    async fn execute(
        &self,
        _ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        {
            return Err(WorkspaceError::HubError(
                "git collect changes is unavailable in this build".to_string(),
            ));
        }
    }
}
#[async_trait]
impl WorkspaceOp for GitResolveRootReq {
    async fn execute(
        &self,
        _ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        Ok(crate::session::git::find_git_root_from_path(&self.cwd).ok())
    }
}
#[async_trait]
impl WorkspaceOp for GitCurrentCommitReq {
    async fn execute(
        &self,
        _ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        Ok(crate::session::git::get_current_commit(&self.git_root).await)
    }
}
#[async_trait]
impl WorkspaceOp for DetectVcsKindReq {
    async fn execute(
        &self,
        _ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        Ok(crate::session::git::detect_vcs_kind(&self.path))
    }
}
#[async_trait]
impl WorkspaceOp for GitCheckoutCommitReq {
    async fn execute(
        &self,
        _ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        Ok(crate::session::git::checkout_commit_with_fetch(
            &self.git_root,
            &self.head_commit,
            self.stash_if_dirty,
        )
        .await)
    }
}
workspace_rpc!(
    PrepareWorktreeFromWorktreeReq,
    "workspace.prepare_worktree_from_worktree",
    PrepareWorktreeFromWorktreeResponse,
    // Validation + path resolution only; the fork itself is the (Mutation)
    // `worktree_create_from_worktree_sync` that follows.
    Read
);
#[async_trait]
impl WorkspaceOp for PrepareWorktreeFromWorktreeReq {
    async fn execute(
        &self,
        _ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let result = crate::worktree::prepare_worktree_from_worktree(&self.inner).await;
        match result.response {
            Ok(resp) => Ok(PrepareWorktreeFromWorktreeResponse {
                spawn_task: result.spawn_task,
                response: Some(
                    serde_json::to_value(&resp)
                        .map_err(|e| WorkspaceError::HubError(e.to_string()))?,
                ),
                error: None,
            }),
            Err(e) => Ok(PrepareWorktreeFromWorktreeResponse {
                spawn_task: false,
                response: None,
                error: Some(e.to_string()),
            }),
        }
    }
}
#[async_trait]
impl WorkspaceOp for CreateWorktreeFromWorktreeSyncReq {
    async fn execute(
        &self,
        _ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let req = crate::worktree::CreateWorktreeFromWorktreeRequest::from(self.inner.clone());
        crate::worktree::create_worktree_from_worktree_sync(&req)
            .await
            .map_err(|e| WorkspaceError::HubError(e.to_string()))
    }
}
#[async_trait]
impl WorkspaceOp for WorktreeDbRebuildReq {
    async fn execute(
        &self,
        _ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let report = crate::worktree::worktree_db_rebuild()
            .map_err(|e| WorkspaceError::HubError(e.to_string()))?;
        serde_json::to_value(report).map_err(|e| WorkspaceError::HubError(e.to_string()))
    }
}
#[async_trait]
impl WorkspaceOp for WorktreeDbPathReq {
    async fn execute(
        &self,
        _ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let path = crate::worktree::worktree_db_path()
            .map_err(|e| WorkspaceError::HubError(e.to_string()))?;
        Ok(WorktreeDbPathResponse {
            path: Some(path.display().to_string()),
        })
    }
}
#[async_trait]
impl WorkspaceOp for HunkSingleActionReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let hunk_id =
            echo_agent_hunk_tracker::types::HunkId::from_string(self.action.hunk_id.clone());
        let hunk_action = tracker_action(self.action.action);
        session_tracker(ws, session_id)?
            .hunk_action(hunk_id, hunk_action)
            .await
            .map_err(|e| WorkspaceError::HunkActionFailed(e.to_string()))?;
        Ok(HunkActionResponse {})
    }
}
#[async_trait]
impl WorkspaceOp for HunkFileActionReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let hunk_action = tracker_action(self.action);
        let affected = session_tracker(ws, session_id)?
            .file_action(std::path::PathBuf::from(&self.path), hunk_action)
            .await
            .map_err(|e| WorkspaceError::HunkActionFailed(e.to_string()))?;
        Ok(BulkHunkActionResponse {
            affected: affected.iter().map(|id| id.as_str().to_owned()).collect(),
        })
    }
}
#[async_trait]
impl WorkspaceOp for HunkTurnActionReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let hunk_action = tracker_action(self.action);
        let affected = session_tracker(ws, session_id)?
            .turn_action(self.prompt_index, hunk_action)
            .await
            .map_err(|e| WorkspaceError::HunkActionFailed(e.to_string()))?;
        Ok(BulkHunkActionResponse {
            affected: affected.iter().map(|id| id.as_str().to_owned()).collect(),
        })
    }
}
#[async_trait]
impl WorkspaceOp for HunkAllActionReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let hunk_action = tracker_action(self.action);
        let affected = session_tracker(ws, session_id)?
            .all_action(hunk_action)
            .await
            .map_err(|e| WorkspaceError::HunkActionFailed(e.to_string()))?;
        Ok(BulkHunkActionResponse {
            affected: affected.iter().map(|id| id.as_str().to_owned()).collect(),
        })
    }
}
#[async_trait]
impl WorkspaceOp for HunkGetAllFileContentsReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        Ok(session_tracker(ws, session_id)?
            .get_all_file_contents()
            .await
            .into_iter()
            .map(file_content_entry_to_wire)
            .collect())
    }
}
#[async_trait]
impl WorkspaceOp for HunkGetSessionSummaryReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        Ok(session_summary_to_wire(
            session_tracker(ws, session_id)?.get_session_summary().await,
        ))
    }
}
#[async_trait]
impl WorkspaceOp for HunkGetAllHunksReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let hunks = session_tracker(ws, session_id)?.get_all_hunks().await;
        Ok(hunks.iter().map(|arc| hunk_to_wire(arc)).collect())
    }
}
#[async_trait]
impl WorkspaceOp for HunkGetStagedFilesReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let files = session_tracker(ws, session_id)?.get_staged_files().await;
        Ok(files
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect())
    }
}
#[async_trait]
impl WorkspaceOp for HunkGetFilteredHunksReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let all_hunks = session_tracker(ws, session_id)?.get_all_hunks().await;
        let filtered: Vec<HunkWire> = all_hunks
            .into_iter()
            .filter(|h| {
                if let Some(ref path) = self.path
                    && !h.path.to_string_lossy().contains(path.as_str())
                {
                    return false;
                }
                if let Some(ref source) = self.source {
                    let source_str = format!("{:?}", h.source);
                    if !source_str.to_lowercase().contains(&source.to_lowercase()) {
                        return false;
                    }
                }
                true
            })
            .map(|arc| hunk_to_wire(&arc))
            .collect();
        let total = filtered.len();
        Ok(FilteredHunksResponse {
            hunks: filtered,
            total,
        })
    }
}
#[async_trait]
impl WorkspaceOp for HunkGetFileSummariesReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let all_hunks = session_tracker(ws, session_id)?.get_all_hunks().await;
        let mut file_map: std::collections::HashMap<String, (usize, bool)> =
            std::collections::HashMap::new();
        for h in &all_hunks {
            let path_str = h.path.to_string_lossy().to_string();
            let is_agent = matches!(
                h.source,
                echo_agent_hunk_tracker::types::HunkSource::AgentEdit { .. }
            );
            let entry = file_map.entry(path_str).or_insert((0, false));
            entry.0 += 1;
            if is_agent {
                entry.1 = true;
            }
        }
        Ok(file_map
            .into_iter()
            .map(|(path, (hunk_count, is_agent_file))| FileSummary {
                path,
                hunk_count,
                is_agent_file,
            })
            .collect())
    }
}
#[async_trait]
impl WorkspaceOp for FuzzyOpenReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let search_id = ws
            .fuzzy_open(
                self.root.as_deref(),
                self.request_id.clone(),
                self.hidden,
                self.session_id.clone(),
                self.target_client_id.clone(),
            )
            .await;
        Ok(search_id)
    }
}
#[async_trait]
impl WorkspaceOp for FuzzyChangeReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let Some((min_generation, has_query, query_version)) = ws
            .fuzzy_change(&self.search_id, &self.query, self.dirs_only)
            .await
        else {
            return Ok(false);
        };
        let ws = ws.clone();
        let search_id = self.search_id.clone();
        let limit = self.limit.unwrap_or(100);
        tokio::spawn(async move {
            ws.run_fuzzy_notifications(search_id, min_generation, has_query, query_version, limit)
                .await;
        });
        Ok(true)
    }
}
#[async_trait]
impl WorkspaceOp for FuzzyCloseReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        Ok(ws.fuzzy_close(&self.search_id).await)
    }
}
#[async_trait]
impl WorkspaceOp for ContentSearchRequest {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let cwd = match &self.cwd {
            Some(c) => c.clone(),
            None => ws.root_cwd()?,
        };
        let context_id = self
            .context_id
            .clone()
            .unwrap_or_else(|| "agent".to_string());
        let params: crate::file_system::ContentSearchParams = self.clone().into();
        ws.run_content_search(cwd, context_id, params).await
    }
}
/// Convert `HookRegistry` to its wire mirror. The `hooks` map is private, so a
/// serde round-trip stands in for field-by-field construction.
fn hook_registry_to_wire(
    registry: &echo_agent_hooks::discovery::HookRegistry,
) -> WorkspaceResult<HookRegistryWire> {
    let value =
        serde_json::to_value(registry).map_err(|e| WorkspaceError::HubError(e.to_string()))?;
    serde_json::from_value(value).map_err(|e| WorkspaceError::HubError(e.to_string()))
}
/// Inverse of [`hook_registry_to_wire`]. Unknown event keys (a newer peer) are
/// dropped so one can't fail the whole decode, and matchers are recompiled
/// fail-closed after the hop.
fn wire_to_hook_registry(
    wire: &HookRegistryWire,
) -> WorkspaceResult<echo_agent_hooks::discovery::HookRegistry> {
    let dropped: Vec<&str> = wire
        .hooks
        .keys()
        .filter_map(|event| match event {
            HookEventNameWire::Unknown(name) => Some(name.as_str()),
            _ => None,
        })
        .collect();
    if !dropped.is_empty() {
        tracing::debug!(
            dropped_count = dropped.len(),
            dropped_events = ?dropped,
            "dropping unknown hook event keys from peer wire registry"
        );
    }
    let known = HookRegistryWire {
        hooks: wire
            .hooks
            .iter()
            .filter(|(event, _)| !matches!(event, HookEventNameWire::Unknown(_)))
            .map(|(event, specs)| (event.clone(), specs.clone()))
            .collect(),
    };
    let value =
        serde_json::to_value(&known).map_err(|e| WorkspaceError::HubError(e.to_string()))?;
    let mut registry: echo_agent_hooks::discovery::HookRegistry =
        serde_json::from_value(value).map_err(|e| WorkspaceError::HubError(e.to_string()))?;
    registry.recompile_matchers();
    Ok(registry)
}
#[async_trait]
impl WorkspaceOp for HookRegistryReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        hook_registry_to_wire(&ws.hook_registry())
    }
}
#[async_trait]
impl WorkspaceOp for PutFilesReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        ws.put_files(session_id, self.files.clone()).await
    }
}
#[async_trait]
impl WorkspaceOp for GetFilesReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        ws.get_files(session_id, self.files.clone()).await
    }
}
#[async_trait]
impl WorkspaceOp for ClientFsListReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        crate::file_system::client_fs::list(ws, session_id, self).await
    }
}
#[async_trait]
impl WorkspaceOp for ClientFsStatReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        crate::file_system::client_fs::stat(ws, session_id, self).await
    }
}
#[async_trait]
impl WorkspaceOp for ClientFsReadFileReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        crate::file_system::client_fs::read_file(ws, session_id, self).await
    }
}
/// Resolve the index root for a code-nav op. Prefers the explicit per-session
/// `root` (the cwd the client sends per window), else the workspace root.
/// Without this, code nav in a non-primary window would query the launch
/// directory's index instead of the session's own repo.
fn index_root_for(
    ws: &WorkspaceHandle,
    root: Option<&std::path::Path>,
) -> WorkspaceResult<std::path::PathBuf> {
    let cwd = match root {
        Some(r) => r.to_path_buf(),
        None => ws.root_cwd()?,
    };
    Ok(crate::session::git::find_git_root_from_path(&cwd).unwrap_or(cwd))
}
fn resolve_index_for_workspace(
    ws: &WorkspaceHandle,
    root: Option<&std::path::Path>,
) -> WorkspaceResult<(
    std::sync::Arc<echo_agent_codebase_graph::IndexManagerHandle>,
    std::path::PathBuf,
)> {
    let index_root = index_root_for(ws, root)?;
    let (handle, _was_new) = ws.get_or_create_codebase_index(index_root.clone());
    Ok((handle, index_root))
}
fn resolve_index_for_file(
    ws: &WorkspaceHandle,
    root: Option<&std::path::Path>,
    file: &str,
) -> WorkspaceResult<(
    std::sync::Arc<echo_agent_codebase_graph::IndexManagerHandle>,
    std::path::PathBuf,
)> {
    if root.is_none() {
        let file_path = std::path::Path::new(file);
        if let Some(handle) = ws.get_covering_codebase_index(file_path) {
            return Ok((handle, file_path.to_path_buf()));
        }
    }
    resolve_index_for_workspace(ws, root)
}
#[async_trait]
impl WorkspaceOp for CodeGotoDefinitionReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let (handle, _root) = resolve_index_for_file(ws, self.root.as_deref(), &self.file)?;
        let result = handle
            .goto_definition(std::path::PathBuf::from(&self.file), self.line, self.col)
            .await
            .map_err(|e| WorkspaceError::HubError(format!("index channel closed: {e}")))?;
        Ok(query_result_to_response(result))
    }
}
#[async_trait]
impl WorkspaceOp for CodeGotoReferencesReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let (handle, _root) = resolve_index_for_file(ws, self.root.as_deref(), &self.file)?;
        let result = handle
            .goto_references(
                std::path::PathBuf::from(&self.file),
                self.line,
                self.col,
                self.include_definition,
            )
            .await
            .map_err(|e| WorkspaceError::HubError(format!("index channel closed: {e}")))?;
        Ok(query_result_to_response(result))
    }
}
#[async_trait]
impl WorkspaceOp for CodeFindDefinitionsReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let (handle, _root) = resolve_index_for_workspace(ws, self.root.as_deref())?;
        let result = handle
            .find_definitions(
                self.symbol.clone(),
                self.context_file.as_ref().map(std::path::PathBuf::from),
            )
            .await
            .map_err(|e| WorkspaceError::HubError(format!("index channel closed: {e}")))?;
        Ok(symbol_locations_to_response(result))
    }
}
#[async_trait]
impl WorkspaceOp for CodeFindReferencesReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let (handle, _root) = resolve_index_for_workspace(ws, self.root.as_deref())?;
        let result = handle
            .find_references(
                self.symbol.clone(),
                self.context_file.as_ref().map(std::path::PathBuf::from),
            )
            .await
            .map_err(|e| WorkspaceError::HubError(format!("index channel closed: {e}")))?;
        Ok(symbol_locations_to_response(result))
    }
}
#[async_trait]
impl WorkspaceOp for CodeIndexStatusReq {
    async fn execute(
        &self,
        ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let index_root = index_root_for(ws, self.root.as_deref())?;
        let handle = ws.get_codebase_index(&index_root);
        match handle {
            Some(h) => {
                let file_count = h.get_file_count();
                let stats = h.get_stats().map(|s| CodeIndexStats {
                    files: s.files,
                    definitions: s.definitions,
                    references: s.references,
                });
                Ok(CodeIndexStatusResponse {
                    active: true,
                    file_count,
                    stats,
                })
            }
            None => Ok(CodeIndexStatusResponse {
                active: false,
                file_count: None,
                stats: None,
            }),
        }
    }
}
fn query_result_to_response(
    result: Result<echo_agent_codebase_graph::QueryResult, echo_agent_codebase_graph::QueryError>,
) -> CodeNavResponse {
    match result {
        Ok(qr) => CodeNavResponse {
            locations: qr
                .locations
                .into_iter()
                .map(|loc| CodeNavLocation {
                    path: loc.path,
                    line: loc.line,
                    symbol: loc.matched_symbol,
                })
                .collect(),
        },
        Err(_) => CodeNavResponse { locations: vec![] },
    }
}
fn symbol_locations_to_response(
    locations: Vec<echo_agent_codebase_graph::SymbolLocation>,
) -> CodeNavResponse {
    CodeNavResponse {
        locations: locations
            .into_iter()
            .map(|loc| CodeNavLocation {
                path: loc.path,
                line: loc.line,
                symbol: loc.matched_symbol,
            })
            .collect(),
    }
}
#[async_trait]
impl WorkspaceOp for CreateWorktreeRequest {
    async fn execute(
        &self,
        _ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let result = crate::worktree::prepare_worktree_creation(self).await;
        match result.response {
            Ok(resp) => {
                serde_json::to_value(resp).map_err(|e| WorkspaceError::HubError(e.to_string()))
            }
            Err(e) => Err(WorkspaceError::HubError(e.to_string())),
        }
    }
}
#[async_trait]
impl WorkspaceOp for RemoveWorktreeRequest {
    async fn execute(
        &self,
        _ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let copy_ctx = crate::worktree::BackgroundCopyContext::new();
        let result = crate::worktree::remove_worktree(self, &copy_ctx)
            .await
            .map_err(|e| WorkspaceError::HubError(e.to_string()))?;
        serde_json::to_value(result).map_err(|e| WorkspaceError::HubError(e.to_string()))
    }
}
#[async_trait]
impl WorkspaceOp for ApplyWorktreeRequest {
    async fn execute(
        &self,
        _ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let result = crate::worktree::apply_worktree(self)
            .await
            .map_err(|e| WorkspaceError::HubError(e.to_string()))?;
        serde_json::to_value(result).map_err(|e| WorkspaceError::HubError(e.to_string()))
    }
}
#[async_trait]
impl WorkspaceOp for WorktreeListReq {
    async fn execute(
        &self,
        _ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let records =
            crate::worktree::list_worktrees(self.repo.as_deref(), &self.types, self.include_all)
                .map_err(|e| WorkspaceError::HubError(e.to_string()))?;
        serde_json::to_value(records).map_err(|e| WorkspaceError::HubError(e.to_string()))
    }
}
#[async_trait]
impl WorkspaceOp for WorktreeShowReq {
    async fn execute(
        &self,
        _ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let record = crate::worktree::show_worktree(&self.id_or_path)
            .map_err(|e| WorkspaceError::HubError(e.to_string()))?;
        serde_json::to_value(record).map_err(|e| WorkspaceError::HubError(e.to_string()))
    }
}
#[async_trait]
impl WorkspaceOp for WorktreeGcReq {
    async fn execute(
        &self,
        _ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let (dry_run, max_age_secs, force) = (self.dry_run, self.max_age_secs, self.force);
        let report = tokio::task::spawn_blocking(move || {
            crate::worktree::gc_worktrees_mgmt(dry_run, max_age_secs, force)
        })
        .await
        .map_err(|e| WorkspaceError::HubError(e.to_string()))?
        .map_err(|e| WorkspaceError::HubError(e.to_string()))?;
        serde_json::to_value(report).map_err(|e| WorkspaceError::HubError(e.to_string()))
    }
}
#[async_trait]
impl WorkspaceOp for WorktreeDetachReq {
    async fn execute(
        &self,
        _ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let id = self.id_or_path.clone();
        let allow_copy = self.allow_copy;
        let report = tokio::task::spawn_blocking(move || {
            crate::worktree::detach_worktree_mgmt(&id, allow_copy)
        })
        .await
        .map_err(|e| WorkspaceError::HubError(e.to_string()))?
        .map_err(|e| WorkspaceError::HubError(e.to_string()))?;
        serde_json::to_value(report).map_err(|e| WorkspaceError::HubError(e.to_string()))
    }
}
#[async_trait]
impl WorkspaceOp for WorktreeSalvageReq {
    async fn execute(
        &self,
        _ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let id = self.id_or_path.clone();
        let out = self.out.clone();
        let report =
            tokio::task::spawn_blocking(move || crate::worktree::salvage_worktree_mgmt(&id, &out))
                .await
                .map_err(|e| WorkspaceError::HubError(e.to_string()))?
                .map_err(|e| WorkspaceError::HubError(e.to_string()))?;
        serde_json::to_value(report).map_err(|e| WorkspaceError::HubError(e.to_string()))
    }
}
#[async_trait]
impl WorkspaceOp for WorktreeCleanArtifactsReq {
    async fn execute(
        &self,
        _ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let id = self.id_or_path.clone();
        let report =
            tokio::task::spawn_blocking(move || crate::worktree::clean_artifacts_mgmt(&id))
                .await
                .map_err(|e| WorkspaceError::HubError(e.to_string()))?
                .map_err(|e| WorkspaceError::HubError(e.to_string()))?;
        serde_json::to_value(report).map_err(|e| WorkspaceError::HubError(e.to_string()))
    }
}
#[async_trait]
impl WorkspaceOp for WorktreeDbStatsReq {
    async fn execute(
        &self,
        _ws: &WorkspaceHandle,
        _session_id: Option<&str>,
    ) -> WorkspaceResult<Self::Response> {
        let stats = crate::worktree::worktree_db_stats()
            .map_err(|e| WorkspaceError::HubError(e.to_string()))?;
        serde_json::to_value(stats).map_err(|e| WorkspaceError::HubError(e.to_string()))
    }
}
/// Dual-mode workspace operations handle.
///
/// - **`Local`** — wraps a [`WorkspaceHandle`]. Extensions dispatch
///   through the handle; tool calls dispatch through the workspace
///   session's [`FinalizedToolset`](echo_agent_tools::registry::types::FinalizedToolset).
///   Call [`bind_local_session`](Self::bind_local_session) after building
///   the agent to install the toolset on the workspace session.
///
/// - **`Proxy`** — wraps a [`WorkspaceClient`] connected to a remote hub.
///   Everything routes through hub WebSocket to a remote workspace server.
#[derive(Clone)]
pub enum WorkspaceOps {
    /// Local in-process mode — extensions through the handle, tool calls
    /// through the workspace session's toolset.
    Local { handle: WorkspaceHandle },
    /// Proxy mode — routes through hub RPC.
    Proxy { client: WorkspaceClient },
}
impl WorkspaceOps {
    /// Construct a local-mode ops handle.
    ///
    /// Extensions dispatch through the handle immediately. Tool calls
    /// require a workspace session — call [`bind_local_session`](Self::bind_local_session)
    /// after building the agent to install the toolset.
    pub fn local(handle: WorkspaceHandle) -> Self {
        Self::Local { handle }
    }
    /// Construct a proxy-mode ops handle.
    pub fn proxy(harness: Arc<ToolHarness>) -> Self {
        Self::Proxy {
            client: WorkspaceClient::new((*harness).clone()),
        }
    }
    /// Construct a proxy-mode ops handle sharing a pre-created connected
    /// flag. The same `Arc<AtomicBool>` should be wired into the harness
    /// builder's `on_reconnect` callback so reconnects reset the flag.
    pub fn proxy_with_connected(harness: Arc<ToolHarness>, connected: Arc<AtomicBool>) -> Self {
        Self::Proxy {
            client: WorkspaceClient::with_connected_flag((*harness).clone(), connected),
        }
    }
    /// Whether this handle routes through the server (proxy mode).
    pub fn is_proxy(&self) -> bool {
        matches!(self, Self::Proxy { .. })
    }
    /// Access the underlying workspace RPC client (proxy mode only).
    pub fn client(&self) -> Option<&WorkspaceClient> {
        match self {
            Self::Proxy { client } => Some(client),
            Self::Local { .. } => None,
        }
    }
    /// Access the underlying workspace handle (local mode only).
    pub fn workspace_handle(&self) -> Option<&WorkspaceHandle> {
        match self {
            Self::Local { handle } => Some(handle),
            Self::Proxy { .. } => None,
        }
    }
    /// Create the workspace session and bind the agent's toolset for local mode.
    ///
    /// Creates the session (if absent) reusing the agent's per-session
    /// `hunk_tracker` rooted at `cwd`, so workspace-routed hunk queries resolve
    /// the same tracker the agent feeds rather than a duplicate rooted at the
    /// launch directory. Then replaces the session's toolset. `cwd` and
    /// `hunk_tracker` are only used on first create; a re-bind (e.g. after an
    /// agent rebuild) just replaces the toolset.
    ///
    /// The installed toolset keeps the shell's own terminal backend; the
    /// session-owned backend minted at create stays idle and is what
    /// `drop_session`/evict cancel — deliberately never adopted from the
    /// external toolset, or teardown would SIGKILL a backend the shell shares.
    ///
    /// No-op in proxy mode (the workspace server owns sessions).
    pub fn bind_local_session(
        &self,
        session_id: &str,
        cwd: std::path::PathBuf,
        hunk_tracker: echo_agent_hunk_tracker::HunkTrackerHandle,
        toolset: Arc<echo_agent_tools::registry::types::FinalizedToolset>,
        viewer_ctx: Option<echo_agent_tool_runtime::WorkspaceViewerContext>,
    ) -> WorkspaceResult<()> {
        let Self::Local { handle } = self else {
            return Ok(());
        };
        if handle.session(session_id).is_none() {
            handle.create_session_with_tracker_and_viewer_ctx(
                session_id,
                cwd,
                hunk_tracker,
                None,
                crate::capability::CapabilityMode::All,
                viewer_ctx,
                false,
            )?;
        }
        let session = handle
            .session(session_id)
            .ok_or_else(|| WorkspaceError::SessionNotFound(session_id.to_owned()))?;
        session.replace(session.effective_tool_config(), toolset);
        Ok(())
    }
    /// Release the workspace session. No-op in proxy mode.
    pub fn end_local_session(&self, session_id: &str) {
        let Self::Local { handle } = self else {
            return;
        };
        handle.on_session_ended(session_id);
        if let Err(e) = handle.drop_session(session_id, session_id) {
            tracing::debug!(%session_id, error = %e, "end_local_session: drop_session failed (expected if never bound)");
        }
    }
    pub async fn on_before_turn(
        &self,
        session_id: &str,
        payload: &echo_agent_tool_protocol::turn_hook::BeforeTurnPayload,
    ) {
        match self {
            Self::Local { handle } => {
                handle.on_before_turn(session_id, payload).await;
            }
            Self::Proxy { .. } => {
                tracing::debug!("on_before_turn called on Proxy WorkspaceOps (no-op)");
            }
        }
    }
    pub async fn on_after_turn(
        &self,
        session_id: &str,
        payload: &echo_agent_tool_protocol::turn_hook::AfterTurnPayload,
    ) {
        match self {
            Self::Local { handle } => {
                handle.on_after_turn(session_id, payload).await;
            }
            Self::Proxy { .. } => {
                tracing::debug!("on_after_turn called on Proxy WorkspaceOps (no-op)");
            }
        }
    }
    pub async fn rpc_raw(&self, method: &str, params: Value) -> WorkspaceResult<Value> {
        let client = match self {
            Self::Proxy { client } => client,
            Self::Local { .. } => {
                return Err(WorkspaceError::HubError(
                    "rpc not available in local mode".into(),
                ));
            }
        };
        client
            .rpc_raw(method, params)
            .await
            .map_err(|e| WorkspaceError::HubError(e.to_string()))
    }
    async fn rpc<R: WorkspaceRpc>(&self, req: &R) -> WorkspaceResult<R::Response> {
        let params = serde_json::to_value(req)
            .map_err(|e| WorkspaceError::HubError(format!("serialize failed: {e}")))?;
        let terminal = self.rpc_raw(R::METHOD, params).await?;
        let envelope: crate::rpc_envelope::RpcEnvelope<R::Response> =
            serde_json::from_value(terminal)
                .map_err(|e| WorkspaceError::HubError(format!("envelope parse failed: {e}")))?;
        envelope
            .into_result()
            .map_err(crate::rpc_envelope::rpc_error_to_workspace)
    }
    /// Dispatch a typed operation in either local or proxy mode.
    ///
    /// - **Local mode**: calls `op.execute(handle, session_id)` directly.
    /// - **Proxy mode**: serializes the op and routes through the server RPC.
    ///   The server handler owns session context, so `session_id` is only
    ///   needed for local `execute()`.
    pub async fn dispatch<Op: WorkspaceOp>(
        &self,
        op: &Op,
        session_id: Option<&str>,
    ) -> WorkspaceResult<Op::Response> {
        let mode = match self {
            Self::Local { .. } => "local",
            Self::Proxy { .. } => "proxy",
        };
        tracing::debug!(method = Op::METHOD, mode, "WorkspaceOps::dispatch");
        match self {
            Self::Local { handle } => op.execute(handle, session_id).await,
            Self::Proxy { .. } => self.rpc(op).await,
        }
    }
    pub async fn workspace_info(&self) -> WorkspaceResult<Value> {
        self.rpc(&WorkspaceInfoReq {}).await
    }
    /// Server binary version without an RPC round-trip: own version in
    /// local mode, the hub bind report in proxy mode (`None` before the
    /// first bind or against servers predating the field).
    pub fn server_version(&self) -> Option<String> {
        match self {
            Self::Local { .. } => Some(echo_agent_version::VERSION.to_owned()),
            Self::Proxy { client } => client.server_binary_version(),
        }
    }
    /// **DEPRECATED**: Use [`Self::git_status_ext`] with `format: GitStatusFormat::Prompt`
    /// instead. This method will be removed in a future release.
    pub async fn git_status(&self) -> WorkspaceResult<Value> {
        self.rpc(&GitStatusReq {}).await
    }
    /// Get git status with configurable output format.
    ///
    /// `GitStatusExtReq` implements `WorkspaceOp`, so this is dispatched
    /// (local execute or proxy RPC) rather than being proxy-only.
    ///
    /// Use `format: GitStatusFormat::Prompt` for compact JSON string output
    /// (the replacement for the deprecated `git_status()` method).
    /// Use `format: GitStatusFormat::Structured` (default) for structured
    /// `GitStatusData` output.
    pub async fn git_status_ext(
        &self,
        req: &GitStatusExtReq,
    ) -> WorkspaceResult<GitStatusExtResponse> {
        self.dispatch(req, None).await
    }
    /// List provisioned repos from the in-sandbox manifest.
    pub async fn repos_list(&self) -> WorkspaceResult<ReposListResponse> {
        self.dispatch(&ReposListReq {}, None).await
    }
    pub async fn hook_registry(
        &self,
    ) -> WorkspaceResult<echo_agent_hooks::discovery::HookRegistry> {
        let wire = self.dispatch(&HookRegistryReq {}, None).await?;
        wire_to_hook_registry(&wire)
    }
    pub async fn begin_prompt(&self, session_id: &str, prompt_index: usize) -> WorkspaceResult<()> {
        self.rpc(&BeginPromptReq {
            session_id: session_id.to_owned(),
            prompt_index,
        })
        .await
    }
    pub async fn end_prompt(&self, session_id: &str, prompt_index: usize) -> WorkspaceResult<()> {
        self.rpc(&EndPromptReq {
            session_id: session_id.to_owned(),
            prompt_index,
        })
        .await
    }
    pub async fn get_rewind_points(
        &self,
        session_id: &str,
    ) -> WorkspaceResult<Vec<crate::session::file_state::RewindPoint>> {
        self.rpc(&GetRewindPointsReq {
            session_id: session_id.to_owned(),
        })
        .await
    }
    pub async fn rewind_to(
        &self,
        session_id: &str,
        target_prompt_index: usize,
    ) -> WorkspaceResult<crate::session::file_state::FileRewindResponse> {
        self.rpc(&RewindToReq {
            session_id: session_id.to_owned(),
            target_prompt_index,
        })
        .await
    }
    pub async fn put_files(&self, req: PutFilesReq) -> WorkspaceResult<PutFilesRes> {
        self.dispatch(&req, None).await
    }
    pub async fn get_files(&self, req: GetFilesReq) -> WorkspaceResult<GetFilesRes> {
        self.dispatch(&req, None).await
    }
    /// Dispatch a tool call through the workspace.
    ///
    /// - **Local**: dispatches through the workspace session's
    ///   [`FinalizedToolset`](echo_agent_tools::registry::types::FinalizedToolset)
    ///   (in-process). Requires `session_id` to look up the session.
    /// - **Proxy**: routes through the server `ToolHarness` (remote).
    pub async fn call_tool(
        &self,
        name: &str,
        args: Value,
        call_id: &str,
        session_id: Option<&str>,
    ) -> Result<ToolRunResult, echo_agent_tool_runtime::ToolError> {
        match self {
            Self::Local { handle } => {
                let session_id = session_id.ok_or_else(|| {
                    echo_agent_tool_runtime::ToolError::custom(
                        "missing_session",
                        "session_id required for local tool dispatch",
                    )
                })?;
                let session = handle.session(session_id).ok_or_else(|| {
                    echo_agent_tool_runtime::ToolError::custom(
                        "session_not_found",
                        format!(
                            "workspace session not found: {session_id} \
                             — call bind_local_session() first"
                        ),
                    )
                })?;
                session.toolset().call(name, args, call_id, None).await
            }
            Self::Proxy { client } => {
                if !client.is_connected() {
                    return Err(echo_agent_tool_runtime::ToolError::network_error(
                        "The workspace server connection was lost. \
                         Please restart your session to reconnect.",
                    ));
                }
                let tool_id = echo_agent_tool_protocol::ToolId::new(name).map_err(|e| {
                    echo_agent_tool_runtime::ToolError::custom(
                        "hub_proxy_error",
                        format!("invalid tool name: {e}"),
                    )
                })?;
                let mut ctx = echo_agent_tool_runtime::ToolCallContext::default();
                ctx.call_id = echo_agent_tool_protocol::ToolCallId::new(call_id.to_owned())
                    .unwrap_or(ctx.call_id);
                let mut stream = client.harness().call(tool_id, args, ctx).await;
                let typed = crate::hub_channel::consume_stream_terminal(&mut stream)
                    .await
                    .inspect_err(|e| {
                        if is_transport_fatal(e) {
                            client.mark_disconnected();
                        }
                    })?;
                serde_json::from_value::<ToolRunResult>(typed.value).map_err(|e| {
                    echo_agent_tool_runtime::ToolError::custom(
                        "tool_result_deserialize",
                        format!("tool result deserialization failed: {e}"),
                    )
                })
            }
        }
    }
}
#[cfg(any(test, feature = "test-support"))]
impl WorkspaceOps {
    /// Test variant backed by a temp dir.
    ///
    /// Supports extension dispatch (`dispatch()`). Tool calls via
    /// `call_tool()` require a workspace session — call
    /// `bind_local_session()` with a test toolset first.
    pub fn for_test() -> Self {
        Self::Local {
            handle: WorkspaceHandle::for_test(),
        }
    }
    /// Like [`Self::for_test`] but rooted at `root`.
    pub fn for_test_in(root: &std::path::Path) -> Self {
        Self::Local {
            handle: WorkspaceHandle::for_test_in(root),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    /// Drift pin for these workspace methods' `workspace.*` wire names. The
    /// request types are defined in `echo-agent-workspace-types` (and re-exported
    /// from this module for existing call sites) so the gateway's typed dispatch
    /// in `workspace_typed/` can consume them without depending on this crate;
    /// this test pins the `::METHOD` strings so a rename can't silently change
    /// the wire contract.
    #[test]
    fn pinned_workspace_method_wire_names() {
        assert_eq!(ReposListReq::METHOD, "workspace.repos_list");
        assert_eq!(HookRegistryReq::METHOD, "workspace.hook_registry");
        assert_eq!(HunkGetAllHunksReq::METHOD, "workspace.get_all_hunks");
        assert_eq!(
            HunkGetSessionSummaryReq::METHOD,
            "workspace.get_session_summary"
        );
        assert_eq!(
            HunkGetAllFileContentsReq::METHOD,
            "workspace.hunk_get_all_file_contents"
        );
        assert_eq!(
            HunkGetFilteredHunksReq::METHOD,
            "workspace.hunk_get_filtered_hunks"
        );
        assert_eq!(
            CreateWorktreeFromWorktreeSyncReq::METHOD,
            "workspace.worktree_create_from_worktree_sync"
        );
    }
    /// The reported bug: every window's git queries ran against the workspace
    /// launch directory. `git_op_cwd` must return the per-session repo the
    /// client sends, and only fall back to the workspace root when none is given.
    #[tokio::test]
    async fn git_op_cwd_uses_explicit_git_root_per_window() {
        let ops = WorkspaceOps::for_test();
        let WorkspaceOps::Local { handle } = &ops else {
            unreachable!("for_test builds a local handle");
        };
        let workspace_root = handle.root_cwd().unwrap();
        let window_a = std::path::PathBuf::from("/repos/echo-agent-main");
        let window_b = std::path::PathBuf::from("/repos/echo-agent-main-2");
        assert_eq!(
            git_op_cwd(handle, &Some(window_a.clone())).await.unwrap(),
            window_a
        );
        assert_eq!(
            git_op_cwd(handle, &Some(window_b.clone())).await.unwrap(),
            window_b
        );
        assert_eq!(git_op_cwd(handle, &None).await.unwrap(), workspace_root);
    }
    #[tokio::test]
    async fn repos_list_reads_real_manifest_empty_one_and_many() {
        let tmp = tempfile::tempdir().unwrap();
        let ops = WorkspaceOps::for_test_in(tmp.path());
        let empty = ops.repos_list().await.expect("empty list");
        assert!(empty.repos.is_empty(), "missing manifest → empty list");
        assert_eq!(
            empty.version,
            echo_agent_workspace_types::rpc::repos::REPOS_MANIFEST_VERSION
        );
        let one = RepoManifest::new(vec![ProvisionedRepo {
            name: "app".into(),
            repository: "acme/app".into(),
            mount_path: "/workspace/app".into(),
            base_branch: "main".into(),
            session_branch: "conv/1".into(),
        }]);
        std::fs::create_dir_all(tmp.path().join(".echo-agent")).unwrap();
        std::fs::write(
            tmp.path()
                .join(echo_agent_workspace_types::rpc::repos::REPOS_MANIFEST_RELATIVE_PATH),
            one.to_json_bytes().unwrap(),
        )
        .unwrap();
        let listed = ops.repos_list().await.expect("one repo");
        assert_eq!(one.repos, listed.repos);
        assert_eq!(one.version, listed.version);
        let many = RepoManifest::new(vec![
            ProvisionedRepo {
                name: "app".into(),
                repository: "acme/app".into(),
                mount_path: "/workspace/app".into(),
                base_branch: "main".into(),
                session_branch: "conv/1".into(),
            },
            ProvisionedRepo {
                name: "lib".into(),
                repository: "acme/lib".into(),
                mount_path: "/workspace/lib".into(),
                base_branch: "develop".into(),
                session_branch: "feat/x".into(),
            },
        ]);
        std::fs::write(
            tmp.path()
                .join(echo_agent_workspace_types::rpc::repos::REPOS_MANIFEST_RELATIVE_PATH),
            many.to_json_bytes().unwrap(),
        )
        .unwrap();
        let listed = ops.repos_list().await.expect("many repos");
        assert_eq!(many.repos, listed.repos);
        assert_eq!(many.version, listed.version);
    }
    #[tokio::test]
    async fn repos_list_walks_up_from_rewritten_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let sandbox_ws = tmp.path();
        let agent_cwd = sandbox_ws.join("app");
        std::fs::create_dir_all(&agent_cwd).unwrap();
        let one = RepoManifest::new(vec![ProvisionedRepo {
            name: "app".into(),
            repository: "acme/app".into(),
            mount_path: "/workspace/app".into(),
            base_branch: "".into(),
            session_branch: "conv/1".into(),
        }]);
        std::fs::create_dir_all(sandbox_ws.join(".echo-agent")).unwrap();
        std::fs::write(
            sandbox_ws.join(echo_agent_workspace_types::rpc::repos::REPOS_MANIFEST_RELATIVE_PATH),
            one.to_json_bytes().unwrap(),
        )
        .unwrap();
        let ops = WorkspaceOps::for_test_in(&agent_cwd);
        let listed = ops.repos_list().await.expect("walk up to sandbox ws");
        assert_eq!(one.repos, listed.repos);
    }
    #[test]
    fn repos_manifest_search_dirs_stops_at_sandbox_workspace_root() {
        let _lock = crate::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let _home = crate::TestEnvGuard::set("HOME", home.path());
        let _unset_echo_agent = crate::TestEnvGuard::unset("ECHO_AGENT_HOME");
        let dirs = repos_manifest_search_dirs(std::path::Path::new("/workspace/app"));
        assert_eq!(
            dirs,
            vec![
                std::path::PathBuf::from("/workspace/app"),
                std::path::PathBuf::from("/workspace"),
            ]
        );
    }
    #[test]
    fn repos_manifest_search_dirs_keeps_sandbox_root_when_home_unset() {
        let _lock = crate::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _home = crate::TestEnvGuard::unset("HOME");
        let _unset_echo_agent = crate::TestEnvGuard::unset("ECHO_AGENT_HOME");
        let dirs = repos_manifest_search_dirs(std::path::Path::new("/workspace/app"));
        assert!(
            dirs.contains(&std::path::PathBuf::from("/workspace/app")),
            "agent cwd must still be probed: {dirs:?}"
        );
        assert!(
            dirs.contains(&std::path::PathBuf::from("/workspace")),
            "sandbox root manifest must not be treated as user-global when HOME is unset: {dirs:?}"
        );
    }
    #[test]
    fn repos_manifest_search_dirs_skips_user_global_echo_agent_home() {
        let _lock = crate::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let _home = crate::TestEnvGuard::set("HOME", home.path());
        let _unset_echo_agent = crate::TestEnvGuard::unset("ECHO_AGENT_HOME");
        let start = home.path().join("src").join("org").join("app");
        let dirs = repos_manifest_search_dirs(&start);
        assert!(dirs.contains(&start));
        assert!(dirs.contains(&home.path().join("src").join("org")));
        assert!(dirs.contains(&home.path().join("src")));
        assert!(
            !dirs.iter().any(|d| d == home.path()),
            "must not probe $HOME/.echo-agent/repos.json: {dirs:?}"
        );
    }
    /// Sync + `block_on` so `ENV_TEST_LOCK` is not held across `.await`
    /// (clippy `await_holding_lock`).
    #[test]
    fn repos_list_does_not_load_user_global_manifest() {
        let _lock = crate::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let _home = crate::TestEnvGuard::set("HOME", home.path());
        let _unset_echo_agent = crate::TestEnvGuard::unset("ECHO_AGENT_HOME");
        let global = RepoManifest::new(vec![ProvisionedRepo {
            name: "global".into(),
            repository: "acme/global".into(),
            mount_path: "/unrelated".into(),
            base_branch: "main".into(),
            session_branch: "x".into(),
        }]);
        std::fs::create_dir_all(home.path().join(".echo-agent")).unwrap();
        std::fs::write(
            home.path().join(".echo-agent").join("repos.json"),
            global.to_json_bytes().unwrap(),
        )
        .unwrap();
        let agent_cwd = home.path().join("src").join("app");
        std::fs::create_dir_all(&agent_cwd).unwrap();
        let ops = WorkspaceOps::for_test_in(&agent_cwd);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let listed = rt.block_on(ops.repos_list()).expect("list");
        assert!(
            listed.repos.is_empty(),
            "missing workspace manifest must not fall back to ~/.echo-agent/repos.json: {:?}",
            listed.repos
        );
    }
    /// Regression: a long-lived (leader) workspace must reclaim the per-session
    /// `FinalizedToolset` — and the MCP tools / `McpState` / `events.jsonl`
    /// `EventWriter` it transitively pins — when a session ends.
    /// `bind_local_session` installs the toolset on a leader-level workspace
    /// session; without `end_local_session` that session (and everything it
    /// holds) leaks for the life of the process.
    #[tokio::test]
    async fn end_local_session_drops_bound_toolset() {
        let ops = WorkspaceOps::for_test();
        let WorkspaceOps::Local { handle } = &ops else {
            unreachable!("for_test builds a local handle");
        };
        let sid = "sess-teardown";
        let toolset = std::sync::Arc::new(
            echo_agent_tools::registry::types::FinalizedToolset::empty_for_test(),
        );
        let weak = std::sync::Arc::downgrade(&toolset);
        ops.bind_local_session(
            sid,
            handle.root_cwd().unwrap(),
            echo_agent_hunk_tracker::HunkTrackerHandle::noop(),
            toolset,
            None,
        )
        .expect("bind should succeed");
        assert!(handle.session(sid).is_some(), "session must be bound");
        assert!(
            weak.upgrade().is_some(),
            "workspace session must hold the toolset"
        );
        ops.end_local_session(sid);
        assert!(
            handle.session(sid).is_none(),
            "end_local_session must remove the workspace session"
        );
        assert!(
            weak.upgrade().is_none(),
            "end_local_session must drop the toolset (no leaked holder)"
        );
    }
    /// Round-trip serde for HunkActionResponse.
    #[test]
    fn hunk_action_response_round_trip() {
        let resp = HunkActionResponse {};
        let json = serde_json::to_value(&resp).unwrap();
        let recovered: HunkActionResponse = serde_json::from_value(json).unwrap();
        assert_eq!(format!("{recovered:?}"), "HunkActionResponse");
    }
    /// Round-trip serde for BulkHunkActionResponse.
    #[test]
    fn bulk_hunk_action_response_round_trip() {
        let resp = BulkHunkActionResponse {
            affected: vec!["hunk-1".into(), "hunk-2".into()],
        };
        let json = serde_json::to_value(&resp).unwrap();
        let recovered: BulkHunkActionResponse = serde_json::from_value(json).unwrap();
        assert_eq!(recovered.affected, vec!["hunk-1", "hunk-2"]);
    }
    /// Round-trip serde for FilteredHunksResponse (empty).
    #[test]
    fn filtered_hunks_response_round_trip_empty() {
        let resp = FilteredHunksResponse {
            hunks: vec![],
            total: 0,
        };
        let json = serde_json::to_value(&resp).unwrap();
        let recovered: FilteredHunksResponse = serde_json::from_value(json).unwrap();
        assert!(recovered.hunks.is_empty());
        assert_eq!(recovered.total, 0);
    }
    /// Round-trip serde for FileSummary.
    #[test]
    fn file_summary_round_trip() {
        let summary = FileSummary {
            path: "src/main.rs".into(),
            hunk_count: 3,
            is_agent_file: true,
        };
        let json = serde_json::to_value(&summary).unwrap();
        let recovered: FileSummary = serde_json::from_value(json).unwrap();
        assert_eq!(recovered.path, "src/main.rs");
        assert_eq!(recovered.hunk_count, 3);
        assert!(recovered.is_agent_file);
    }
    /// BulkHunkActionResponse default is empty.
    #[test]
    fn bulk_hunk_action_response_default() {
        let resp = BulkHunkActionResponse::default();
        assert!(resp.affected.is_empty());
    }
    /// FilteredHunksResponse default is empty.
    #[test]
    fn filtered_hunks_response_default() {
        let resp = FilteredHunksResponse::default();
        assert!(resp.hunks.is_empty());
        assert_eq!(resp.total, 0);
    }
    /// A `Hunk`'s wire mirror serializes byte-for-byte like the heavy type.
    #[test]
    fn hunk_to_wire_serializes_identically() {
        use echo_agent_hunk_tracker::types::{Hunk, HunkSource};
        let mut hunk = Hunk::file_created(
            std::path::PathBuf::from("/repo/a.rs"),
            "new\n".to_string(),
            HunkSource::AgentEdit { prompt_index: 2 },
        );
        hunk.old_text = Some("old\n".to_string());
        hunk.patch = Some("@@ -1 +1 @@\n".to_string());
        hunk.selected = true;
        assert_eq!(
            serde_json::to_value(&hunk).unwrap(),
            serde_json::to_value(hunk_to_wire(&hunk)).unwrap()
        );
    }
    /// A `FileContentEntry`'s wire mirror serializes identically (incl. the
    /// `skip_serializing_if` handling on absent baseline content).
    #[test]
    fn file_content_entry_to_wire_serializes_identically() {
        use echo_agent_hunk_tracker::FileContentEntry;
        use echo_agent_hunk_tracker::types::FileContentView;
        let entry = FileContentEntry {
            path: std::path::PathBuf::from("/repo/a.rs"),
            baseline: FileContentView::missing(),
            current: FileContentView::full("x\n".to_string()),
            is_agent_file: true,
            staged: false,
        };
        assert_eq!(
            serde_json::to_value(&entry).unwrap(),
            serde_json::to_value(file_content_entry_to_wire(entry)).unwrap()
        );
    }
    /// A `SessionSummary` (with a turn carrying a hunk) mirrors identically.
    #[test]
    fn session_summary_to_wire_serializes_identically() {
        use echo_agent_hunk_tracker::SessionSummary;
        use echo_agent_hunk_tracker::types::{Hunk, HunkSource, TurnSummary};
        use std::sync::Arc;
        let hunk = Hunk::file_created(
            std::path::PathBuf::from("/repo/a.rs"),
            "x\n".to_string(),
            HunkSource::AgentEdit { prompt_index: 1 },
        );
        let summary = SessionSummary {
            files_modified: 2,
            pending_hunks: 1,
            turns: vec![TurnSummary {
                prompt_index: 1,
                files: vec![std::path::PathBuf::from("/repo/a.rs")],
                pending_hunks: vec![Arc::new(hunk)],
                lines_added: 1,
                lines_removed: 0,
            }],
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_value(&summary).unwrap(),
            serde_json::to_value(session_summary_to_wire(summary)).unwrap()
        );
    }
    /// `HookRegistry` round-trips through the wire mirror in both directions
    /// (heavy → wire serializes identically; wire → heavy is the inverse).
    #[test]
    fn hook_registry_wire_round_trip_both_directions() {
        let spec = echo_agent_hooks::config::HookSpec {
            name: "global/safety".to_string(),
            event: echo_agent_hooks::event::HookEventName::PreToolUse,
            handler_type: echo_agent_hooks::config::HandlerType::Command,
            configured_matcher: Some("Bash".to_string()),
            matcher: None,
            enabled: true,
            command: Some(std::path::PathBuf::from("/bin/check.sh")),
            command_raw: Some("${X}/check.sh".to_string()),
            url: None,
            url_raw: None,
            timeout_ms: 5000,
            source_dir: std::path::PathBuf::from("/home/u/.echo-agent/hooks"),
            extra_env: std::collections::HashMap::from([("FOO".to_string(), "bar".to_string())]),
            layer: echo_agent_hooks::config::HookProvenance::File,
        };
        let mut registry = echo_agent_hooks::discovery::HookRegistry::default();
        registry.append_specs(vec![spec]);
        let wire = hook_registry_to_wire(&registry).expect("heavy → wire");
        assert_eq!(
            serde_json::to_value(&registry).unwrap(),
            serde_json::to_value(&wire).unwrap()
        );
        let back = wire_to_hook_registry(&wire).expect("wire → heavy");
        assert_eq!(
            serde_json::to_value(&back).unwrap(),
            serde_json::to_value(&registry).unwrap()
        );
    }
    /// Compile-time drift guard: the lean `HookEventNameWire` can't depend on
    /// upstream `HookEventName`, and `hook_registry_to_wire`/`wire_to_hook_registry`
    /// only couple them at runtime. This exhaustive `match` (no wildcard) fails to
    /// compile if upstream adds a variant, forcing the wire mirror to be updated
    /// before the serde round-trip could silently start erroring. The assertion
    /// also pins that each variant's serialized key is byte-identical on both sides.
    #[test]
    fn hook_event_name_wire_covers_all_upstream_variants() {
        use echo_agent_hooks::event::HookEventName as E;
        fn to_wire(e: E) -> HookEventNameWire {
            match e {
                E::SessionStart => HookEventNameWire::SessionStart,
                E::SessionEnd => HookEventNameWire::SessionEnd,
                E::Stop => HookEventNameWire::Stop,
                E::StopFailure => HookEventNameWire::StopFailure,
                E::StopCancelled => HookEventNameWire::StopCancelled,
                E::PreToolUse => HookEventNameWire::PreToolUse,
                E::PostToolUse => HookEventNameWire::PostToolUse,
                E::PostToolUseFailure => HookEventNameWire::PostToolUseFailure,
                E::PermissionDenied => HookEventNameWire::PermissionDenied,
                E::UserPromptSubmit => HookEventNameWire::UserPromptSubmit,
                E::Notification => HookEventNameWire::Notification,
                E::SubagentStart => HookEventNameWire::SubagentStart,
                E::SubagentStop => HookEventNameWire::SubagentStop,
                E::SubagentEnd => HookEventNameWire::SubagentEnd,
                E::PreCompact => HookEventNameWire::PreCompact,
                E::PostCompact => HookEventNameWire::PostCompact,
            }
        }
        for e in [
            E::SessionStart,
            E::SessionEnd,
            E::Stop,
            E::StopFailure,
            E::StopCancelled,
            E::PreToolUse,
            E::PostToolUse,
            E::PostToolUseFailure,
            E::PermissionDenied,
            E::UserPromptSubmit,
            E::Notification,
            E::SubagentStart,
            E::SubagentStop,
            E::SubagentEnd,
            E::PreCompact,
            E::PostCompact,
        ] {
            assert_eq!(
                serde_json::to_value(e).unwrap(),
                serde_json::to_value(to_wire(e)).unwrap(),
                "wire key drifted for {e:?}"
            );
        }
    }
    /// Compile-time drift guard for `HookSpecWire`, the struct analog of
    /// `hook_event_name_wire_covers_all_upstream_variants`. The lean types crate
    /// can't depend on `echo-agent-hooks`, and `hook_registry_to_wire` only couples
    /// the two via a serde round-trip, so a new serialized field on upstream
    /// `HookSpec` would otherwise be dropped on the wire silently. The exhaustive
    /// destructuring below (no `..`) fails to compile when upstream adds or renames
    /// a field, and rebuilding `HookSpecWire` from those bindings catches wire-side
    /// drift; the assertion pins that both serde shapes stay byte-identical. The
    /// compiled `matcher` is `#[serde(skip)]` and is the only field intentionally
    /// absent from the wire.
    #[test]
    fn hook_spec_wire_covers_all_upstream_fields() {
        use echo_agent_hooks::config::HookSpec;
        fn to_wire(spec: HookSpec) -> HookSpecWire {
            let HookSpec {
                name,
                event,
                handler_type,
                configured_matcher,
                matcher: _,
                enabled,
                command,
                command_raw,
                url,
                url_raw,
                timeout_ms,
                source_dir,
                extra_env,
                layer,
            } = spec;
            let event = serde_json::from_value(serde_json::to_value(event).unwrap()).unwrap();
            HookSpecWire {
                name,
                event,
                handler_type: handler_type.as_str().to_string(),
                configured_matcher,
                enabled,
                command,
                command_raw,
                url,
                url_raw,
                timeout_ms,
                source_dir,
                extra_env,
                layer: layer.as_str().to_string(),
            }
        }
        let spec = HookSpec {
            name: "global/safety".to_string(),
            event: echo_agent_hooks::event::HookEventName::PreToolUse,
            handler_type: echo_agent_hooks::config::HandlerType::Command,
            configured_matcher: Some("Bash".to_string()),
            matcher: None,
            enabled: true,
            command: Some(std::path::PathBuf::from("/bin/check.sh")),
            command_raw: Some("${X}/check.sh".to_string()),
            url: None,
            url_raw: None,
            timeout_ms: 5000,
            source_dir: std::path::PathBuf::from("/home/u/.echo-agent/hooks"),
            extra_env: std::collections::HashMap::from([("FOO".to_string(), "bar".to_string())]),
            layer: echo_agent_hooks::config::HookProvenance::Managed,
        };
        assert_eq!(
            serde_json::to_value(&spec).unwrap(),
            serde_json::to_value(to_wire(spec.clone())).unwrap(),
            "HookSpecWire serde shape drifted from upstream HookSpec"
        );
    }
    /// The worktree-fork request projects onto / rebuilds from its wire mirror;
    /// the two `#[serde(skip)]` runtime fields never ride the wire.
    #[test]
    fn create_worktree_from_worktree_request_wire_round_trip() {
        let req = crate::worktree::CreateWorktreeFromWorktreeRequest {
            source_worktree_path: "/src".to_string(),
            new_session_id: "s2".to_string(),
            copy_mode: crate::worktree::WorktreeCopyMode::Dirty,
            git_ref: Some("main".to_string()),
            worktree_type: Some(crate::worktree::WorktreeType::Linked),
            label: None,
            grove_worktree: None,
            cancellation_token: None,
            resolved_dest_path: None,
        };
        assert_eq!(
            serde_json::to_value(&req).unwrap(),
            serde_json::to_value(req.clone().into_wire()).unwrap()
        );
        let back: crate::worktree::CreateWorktreeFromWorktreeRequest = req.into_wire().into();
        assert_eq!(back.source_worktree_path, "/src");
        assert!(back.cancellation_token.is_none());
        assert!(back.resolved_dest_path.is_none());
    }
    use crate::handle::tests::make_handle;
    #[tokio::test]
    async fn execute_hunk_get_all_file_contents_returns_empty_for_fresh_tracker() {
        let handle = make_handle();
        let op_result = HunkGetAllFileContentsReq {}
            .execute(&handle, Some("main"))
            .await
            .expect("execute should succeed");
        assert!(op_result.is_empty());
    }
    #[tokio::test]
    async fn execute_hunk_get_session_summary_returns_value() {
        let handle = make_handle();
        let op_result = HunkGetSessionSummaryReq {}
            .execute(&handle, Some("main"))
            .await
            .expect("execute should succeed");
        let json = serde_json::to_value(&op_result).unwrap();
        assert!(json.is_object() || json.is_null());
    }
    #[tokio::test]
    async fn execute_hunk_get_all_hunks_returns_empty_for_fresh_tracker() {
        let handle = make_handle();
        let op_result = HunkGetAllHunksReq {}
            .execute(&handle, Some("main"))
            .await
            .expect("execute should succeed");
        assert!(op_result.is_empty());
    }
    #[tokio::test]
    async fn execute_hunk_get_staged_files_returns_empty_for_fresh_tracker() {
        let handle = make_handle();
        let op_result = HunkGetStagedFilesReq {}
            .execute(&handle, Some("main"))
            .await
            .expect("execute should succeed");
        assert!(op_result.is_empty());
    }
    #[tokio::test]
    async fn execute_hunk_get_filtered_hunks_returns_empty_for_fresh_tracker() {
        let handle = make_handle();
        let op_result = HunkGetFilteredHunksReq::default()
            .execute(&handle, Some("main"))
            .await
            .expect("execute should succeed");
        assert!(op_result.hunks.is_empty());
        assert_eq!(op_result.total, 0);
    }
    #[tokio::test]
    async fn execute_hunk_get_file_summaries_returns_empty_for_fresh_tracker() {
        let handle = make_handle();
        let op_result = HunkGetFileSummariesReq {}
            .execute(&handle, Some("main"))
            .await
            .expect("execute should succeed");
        assert!(op_result.is_empty());
    }
    fn test_fuzzy_open_req() -> FuzzyOpenReq {
        FuzzyOpenReq {
            root: None,
            request_id: None,
            hidden: false,
            session_id: None,
            target_client_id: crate::file_system::TargetClientId::None,
        }
    }
    #[tokio::test]
    async fn execute_fuzzy_open_returns_search_id() {
        let handle = make_handle();
        let search_id = test_fuzzy_open_req()
            .execute(&handle, None)
            .await
            .expect("execute should succeed");
        assert!(!search_id.is_empty());
    }
    #[tokio::test]
    async fn execute_fuzzy_close_nonexistent_returns_false() {
        let handle = make_handle();
        let closed = FuzzyCloseReq {
            search_id: "nonexistent".into(),
        }
        .execute(&handle, None)
        .await
        .expect("execute should succeed");
        assert!(!closed);
    }
    #[tokio::test]
    async fn execute_fuzzy_open_close_parity() {
        let handle = make_handle();
        let search_id = test_fuzzy_open_req()
            .execute(&handle, None)
            .await
            .expect("open");
        let closed = FuzzyCloseReq {
            search_id: search_id.clone(),
        }
        .execute(&handle, None)
        .await
        .expect("close");
        assert!(closed, "search we just opened should close");
        let closed_again = FuzzyCloseReq { search_id }
            .execute(&handle, None)
            .await
            .expect("close2");
        assert!(!closed_again, "second close should return false");
    }
    #[tokio::test]
    async fn execute_fuzzy_change_nonexistent_returns_false() {
        let handle = make_handle();
        let found = FuzzyChangeReq {
            search_id: "nonexistent".into(),
            query: "test".into(),
            dirs_only: false,
            limit: None,
        }
        .execute(&handle, None)
        .await
        .expect("execute should succeed");
        assert!(!found);
    }
    /// PutFileEntry serde round-trip with defaults.
    #[test]
    fn put_file_entry_defaults() {
        let json = serde_json::json!({
            "path": "src/main.rs",
            "content": "fn main() {}"
        });
        let entry: PutFileEntry = serde_json::from_value(json).unwrap();
        assert_eq!(entry.path, "src/main.rs");
        assert_eq!(entry.content, "fn main() {}");
        assert!(entry.create_dirs, "create_dirs should default to true");
        assert!(!entry.append, "append should default to false");
    }
    /// PutFilesReq round-trip.
    #[test]
    fn put_files_req_round_trip() {
        let req = PutFilesReq {
            files: vec![PutFileEntry {
                path: "a.txt".into(),
                content: "hello".into(),
                create_dirs: false,
                append: true,
            }],
        };
        let json = serde_json::to_value(&req).unwrap();
        let recovered: PutFilesReq = serde_json::from_value(json).unwrap();
        assert_eq!(recovered.files.len(), 1);
        assert_eq!(recovered.files[0].path, "a.txt");
        assert!(!recovered.files[0].create_dirs);
        assert!(recovered.files[0].append);
    }
    /// PutFilesReq METHOD constant.
    #[test]
    fn put_files_req_method() {
        assert_eq!(<PutFilesReq as WorkspaceRpc>::METHOD, "workspace.put_files");
    }
    /// PutFileResult serialization skips None fields.
    #[test]
    fn put_file_result_skip_none() {
        let result = PutFileResult {
            path: "a.txt".into(),
            ok: true,
            error: None,
            hash: Some("abc123".into()),
        };
        let json = serde_json::to_value(&result).unwrap();
        assert!(!json.as_object().unwrap().contains_key("error"));
        assert_eq!(json["hash"], "abc123");
    }
    /// GetFileEntry serde round-trip with defaults.
    #[test]
    fn get_file_entry_defaults() {
        let json = serde_json::json!({ "path": "lib.rs" });
        let entry: GetFileEntry = serde_json::from_value(json).unwrap();
        assert_eq!(entry.path, "lib.rs");
        assert!(entry.if_none_match.is_none());
        assert!(entry.offset.is_none());
        assert!(entry.length.is_none());
    }
    /// GetFilesReq round-trip with all optional fields.
    #[test]
    fn get_files_req_round_trip() {
        let req = GetFilesReq {
            files: vec![GetFileEntry {
                path: "data.bin".into(),
                if_none_match: Some("sha256-abc".into()),
                offset: Some(100),
                length: Some(200),
            }],
        };
        let json = serde_json::to_value(&req).unwrap();
        let recovered: GetFilesReq = serde_json::from_value(json).unwrap();
        assert_eq!(
            recovered.files[0].if_none_match.as_deref(),
            Some("sha256-abc")
        );
        assert_eq!(recovered.files[0].offset, Some(100));
        assert_eq!(recovered.files[0].length, Some(200));
    }
    /// GetFilesReq METHOD constant.
    #[test]
    fn get_files_req_method() {
        assert_eq!(<GetFilesReq as WorkspaceRpc>::METHOD, "workspace.get_files");
    }
    /// GetFileResult serialization skips None fields, defaults matched to false.
    #[test]
    fn get_file_result_defaults_and_skip() {
        let json = serde_json::json!({
            "path": "a.txt",
            "exists": true,
        });
        let result: GetFileResult = serde_json::from_value(json).unwrap();
        assert!(!result.matched, "matched should default to false");
        assert!(result.content.is_none());
        assert!(result.hash.is_none());
        assert!(result.size.is_none());
        assert!(result.error.is_none());
        let serialized = serde_json::to_value(&result).unwrap();
        let obj = serialized.as_object().unwrap();
        assert!(!obj.contains_key("content"));
        assert!(!obj.contains_key("hash"));
        assert!(!obj.contains_key("size"));
        assert!(!obj.contains_key("error"));
    }
    /// PutFilesRes / GetFilesRes round-trip.
    #[test]
    fn put_get_files_res_round_trip() {
        let put_res = PutFilesRes {
            results: vec![PutFileResult {
                path: "x.rs".into(),
                ok: false,
                error: Some("permission denied".into()),
                hash: None,
            }],
        };
        let json = serde_json::to_value(&put_res).unwrap();
        let recovered: PutFilesRes = serde_json::from_value(json).unwrap();
        assert!(!recovered.results[0].ok);
        assert_eq!(
            recovered.results[0].error.as_deref(),
            Some("permission denied")
        );
        let get_res = GetFilesRes {
            results: vec![GetFileResult {
                path: "y.rs".into(),
                exists: true,
                content: Some("contents".into()),
                hash: Some("deadbeef".into()),
                matched: false,
                size: Some(8),
                error: None,
            }],
        };
        let json = serde_json::to_value(&get_res).unwrap();
        let recovered: GetFilesRes = serde_json::from_value(json).unwrap();
        assert_eq!(recovered.results[0].size, Some(8));
        assert_eq!(recovered.results[0].content.as_deref(), Some("contents"));
    }
    /// Code-nav must resolve its index at the per-session root the client
    /// sends, not the shared workspace root — otherwise a second window would
    /// query the first window's index.
    #[tokio::test]
    async fn index_root_for_uses_explicit_per_window_root() {
        let handle = make_handle();
        let window_a = std::path::Path::new("/nonexistent/window-a");
        let window_b = std::path::Path::new("/nonexistent/window-b");
        assert_eq!(index_root_for(&handle, Some(window_a)).unwrap(), window_a);
        assert_eq!(index_root_for(&handle, Some(window_b)).unwrap(), window_b);
        assert_ne!(index_root_for(&handle, None).unwrap(), window_a);
    }
}
