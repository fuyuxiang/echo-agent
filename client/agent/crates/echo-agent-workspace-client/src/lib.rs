#![allow(
    unused_imports,
    unused_variables,
    unused_mut,
    unreachable_code,
    dead_code
)]
//! Typed client for hub-proxied `workspace.*` RPC methods — the single
//! transport for the `workspace_rpc` channel, shared by `WorkspaceOps`
//! proxy mode and by consumers that cannot depend on
//! `echo-agent-workspace`. Wire types live in
//! `echo_agent_workspace_types::rpc`; this crate adds the connected-state
//! latch, the generic [`WorkspaceClient::rpc`] core, and error mapping.
//!
//! No deadline is imposed by default ([`WorkspaceClient::with_deadline`]
//! opts in), preserving `WorkspaceOps::rpc_raw` semantics where callers
//! own their timeouts.
use echo_agent_computer_hub_sdk::harness::ToolHarness;
use echo_agent_tool_runtime::{ToolCallContext, ToolStreamItem, TypedToolOutput};
use echo_agent_workspace_types::rpc::agents_md::{AgentConfigFile, DiscoverAgentsMdReq};
use echo_agent_workspace_types::rpc::code_nav::{
    CodeFindDefinitionsReq, CodeFindReferencesReq, CodeGotoDefinitionReq, CodeGotoReferencesReq,
    CodeIndexStatusReq, CodeIndexStatusResponse, CodeNavResponse,
};
use echo_agent_workspace_types::rpc::export_github::{ExportGithubReq, ExportGithubResponse};
use echo_agent_workspace_types::rpc::fs::{
    FsDeleteFileReq, FsExistsData, FsExistsReq, FsListData, FsListReq, FsReadFileData,
    FsReadFileReq, FsWriteFileReq, GetFilesReq, GetFilesRes, PutFilesReq, PutFilesRes,
};
use echo_agent_workspace_types::rpc::git::{
    CheckoutCommitResponse, CommitResult, DetectVcsKindReq, GitBranchInfoReq, GitBranchListData,
    GitBranchesReq, GitCheckoutCommitReq, GitCheckoutReq, GitCollectChangesReq,
    GitCollectChangesResponse, GitCommitReq, GitCurrentCommitReq, GitDiffReq, GitDiffsData,
    GitDiscardReq, GitFilesReq, GitInfoData, GitInfoReq, GitMetadataReq, GitReadFilesData,
    GitResolveRootReq, GitStageContentReq, GitStageReq, GitStashReq, GitStatusExtReq,
    GitStatusExtResponse, GitStatusReq, GitSyncBaseReq, GitSyncBaseResult, GitUnstageReq,
    StageData, VcsKind,
};
use echo_agent_workspace_types::rpc::hunks::{
    BulkHunkActionResponse, FileSummary, HunkActionResponse, HunkAllActionReq, HunkFileActionReq,
    HunkGetFileSummariesReq, HunkGetStagedFilesReq, HunkSingleActionReq, HunkTurnActionReq,
};
use echo_agent_workspace_types::rpc::search::{
    ContentSearchData, ContentSearchRequest, FuzzyChangeReq, FuzzyCloseReq, FuzzyOpenReq,
    FuzzyStatusReq,
};
use echo_agent_workspace_types::rpc::session::{
    BeginPromptReq, EndPromptReq, FileRewindResponse, RewindToReq,
};
use echo_agent_workspace_types::rpc::skills::{DiscoverPluginsReq, DiscoverSkillsReq, SkillInfo};
use echo_agent_workspace_types::rpc::workspace::{
    ConfigureMcpReq, DropSessionReq, InstallPluginReq, LoadEnvrcReq, LoadPermissionsReq,
    LoadProjectConfigReq, RefreshPluginsReq, ResolveFileReferencesReq, ToolDefinitionsReq,
    UpdateToolConfigReq, WorkspaceInfo, WorkspaceInfoReq,
};
use echo_agent_workspace_types::rpc::worktree::{
    ApplyWorktreeRequest, CreateWorktreeRequest, RemoveWorktreeRequest, WorktreeCreateSyncReq,
    WorktreeDbPathReq, WorktreeDbPathResponse, WorktreeDbRebuildReq, WorktreeDbStatsReq,
    WorktreeGcReq, WorktreeListReq, WorktreeShowReq,
};
use echo_agent_workspace_types::rpc::{RpcEnvelope, RpcError, WORKSPACE_RPC_TOOL_ID, WorkspaceRpc};
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceClientError {
    /// A previous call observed a fatal transport error and no
    /// reconnect has been signalled since.
    #[error("hub connection lost (previously disconnected)")]
    NotConnected,
    #[error("rpc failed: {0}")]
    Transport(String),
    #[error("{method} timed out after {after:?}")]
    Timeout { method: String, after: Duration },
    #[error("{method}: response decode: {source}")]
    Decode {
        method: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("workspace hibernated; it revives on the next bind; do not retry")]
    WorkspaceHibernated,
    /// The server returned an error envelope.
    #[error("workspace rpc error: {0}")]
    Rpc(RpcError),
}
/// Consume a `ToolStream<TypedToolOutput>` to its terminal item,
/// discarding progress frames.
///
/// Returns the terminal result, or a `ToolError::NetworkError` if the
/// stream ended without producing a terminal item.
pub async fn consume_stream_terminal(
    stream: &mut echo_agent_tool_runtime::ToolStream<TypedToolOutput>,
) -> Result<TypedToolOutput, echo_agent_tool_runtime::ToolError> {
    loop {
        let item = std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await;
        match item {
            Some(ToolStreamItem::Progress(_)) => {}
            Some(ToolStreamItem::Terminal(result)) => return result,
            None => {
                return Err(echo_agent_tool_runtime::ToolError::network_error(
                    "stream ended without terminal item",
                ));
            }
        }
    }
}
/// Feature-gate check over a reported workspace-server version: it parses as
/// semver and is `>= baseline`. Absent or unparseable versions return `false`
/// — an unproven version is older than any gated feature.
pub fn server_version_at_least(version: Option<&str>, baseline: &semver::Version) -> bool {
    version
        .and_then(|v| semver::Version::parse(v).ok())
        .is_some_and(|v| v >= *baseline)
}
/// Check whether a [`ToolError`](echo_agent_tool_runtime::ToolError) indicates
/// a fatal transport failure that should mark the hub as disconnected.
///
/// Returns `true` for:
/// - `NetworkError` — direct transport failure (socket dropped, stream
///   ended without terminal item, etc.)
/// - `Custom` with `details.code == "protocol_error"` — half-closed
///   WebSocket producing malformed frames
pub fn is_transport_fatal(err: &echo_agent_tool_runtime::ToolError) -> bool {
    match err.kind {
        echo_agent_tool_runtime::ToolErrorKind::NetworkError => true,
        echo_agent_tool_runtime::ToolErrorKind::Custom => err
            .details
            .as_ref()
            .and_then(|d| d.get("code"))
            .and_then(|c| c.as_str())
            .is_some_and(|c| c == "protocol_error"),
        _ => false,
    }
}
/// True when the hub's `workspace_unavailable` details carry `retryable: false`, the contract
/// that blind retries cannot succeed until the workspace is revived.
fn is_non_retryable_workspace_unavailable(err: &echo_agent_tool_runtime::ToolError) -> bool {
    if !matches!(err.kind, echo_agent_tool_runtime::ToolErrorKind::Custom) {
        return false;
    }
    err.details
        .as_ref()
        .and_then(|d| {
            use serde::Deserialize as _;
            echo_agent_tool_protocol::WorkspaceUnavailableDetails::deserialize(d).ok()
        })
        .is_some_and(|d| {
            d.code == echo_agent_tool_protocol::WORKSPACE_UNAVAILABLE_SUBCODE && !d.retryable
        })
}
/// Typed client over a bound [`ToolHarness`] for `workspace.*` RPCs.
///
/// Clones share the harness and the connected latch, which fast-fails
/// calls after a fatal transport error until
/// [`mark_connected`](Self::mark_connected) resets it (e.g. from an SDK
/// `on_reconnect` callback sharing the flag via
/// [`with_connected_flag`](Self::with_connected_flag)).
#[derive(Clone)]
pub struct WorkspaceClient {
    harness: ToolHarness,
    connected: Arc<AtomicBool>,
    deadline: Option<Duration>,
}
impl std::fmt::Debug for WorkspaceClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceClient")
            .field("connected", &self.is_connected())
            .field("deadline", &self.deadline)
            .finish_non_exhaustive()
    }
}
impl WorkspaceClient {
    pub fn new(harness: ToolHarness) -> Self {
        Self {
            harness,
            connected: Arc::new(AtomicBool::new(true)),
            deadline: None,
        }
    }
    /// Shares a pre-created connected flag, so an SDK `on_reconnect`
    /// callback holding the same `Arc` can reset it.
    pub fn with_connected_flag(harness: ToolHarness, connected: Arc<AtomicBool>) -> Self {
        Self {
            harness,
            connected,
            deadline: None,
        }
    }
    /// Set a per-call deadline covering dispatch and stream consumption.
    pub fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = Some(deadline);
        self
    }
    pub fn harness(&self) -> &ToolHarness {
        &self.harness
    }
    /// Server binary version from the hub bind report, without an RPC
    /// round-trip. `None` before the first bind or against servers that
    /// predate the field.
    pub fn server_binary_version(&self) -> Option<String> {
        self.harness
            .last_bind_report()
            .and_then(|report| report.binary_version.clone())
    }
    /// Whether the hub connection is believed to be alive.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }
    pub fn mark_disconnected(&self) {
        self.connected.store(false, Ordering::Relaxed);
    }
    /// Reset after an SDK reconnect.
    pub fn mark_connected(&self) {
        self.connected.store(true, Ordering::Relaxed);
    }
    /// Untyped RPC call: `{"method": .., "params": ..}` through the
    /// `workspace_rpc` hub tool, returning the raw envelope value.
    pub async fn rpc_raw(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, WorkspaceClientError> {
        if !self.is_connected() {
            return Err(WorkspaceClientError::NotConnected);
        }
        let tool_id = echo_agent_tool_protocol::ToolId::new(WORKSPACE_RPC_TOOL_ID)
            .expect("constant tool id is valid");
        let args = serde_json::json!({ "method": method, "params": params });
        tracing::debug!(method, "WorkspaceClient::rpc");
        let fut = async {
            let mut stream = self
                .harness
                .call(tool_id, args, ToolCallContext::default())
                .await;
            consume_stream_terminal(&mut stream).await
        };
        let result = match self.deadline {
            Some(deadline) => match tokio::time::timeout(deadline, fut).await {
                Ok(r) => r,
                Err(_) => {
                    return Err(WorkspaceClientError::Timeout {
                        method: method.to_owned(),
                        after: deadline,
                    });
                }
            },
            None => fut.await,
        };
        let typed = result.map_err(|e| {
            if is_non_retryable_workspace_unavailable(&e) {
                return WorkspaceClientError::WorkspaceHibernated;
            }
            if is_transport_fatal(&e) {
                self.mark_disconnected();
            }
            WorkspaceClientError::Transport(e.to_string())
        })?;
        Ok(typed.value)
    }
    /// Typed RPC call: derives the method and response type from the
    /// request type's [`WorkspaceRpc`] impl and decodes the envelope.
    pub async fn rpc<R: WorkspaceRpc>(&self, req: &R) -> Result<R::Response, WorkspaceClientError> {
        let params = serde_json::to_value(req).map_err(|e| WorkspaceClientError::Decode {
            method: R::METHOD.to_owned(),
            source: e,
        })?;
        let raw = self.rpc_raw(R::METHOD, params).await?;
        let envelope: RpcEnvelope<R::Response> =
            serde_json::from_value(raw).map_err(|e| WorkspaceClientError::Decode {
                method: R::METHOD.to_owned(),
                source: e,
            })?;
        envelope.into_result().map_err(WorkspaceClientError::Rpc)
    }
    /// `workspace.info`, decoded into the typed shape
    /// (`WorkspaceInfoReq::Response` is the raw `Value` for
    /// `WorkspaceOps` compat).
    pub async fn info(&self) -> Result<WorkspaceInfo, WorkspaceClientError> {
        let raw = self.rpc(&WorkspaceInfoReq {}).await?;
        serde_json::from_value(raw).map_err(|e| WorkspaceClientError::Decode {
            method: WorkspaceInfoReq::METHOD.to_owned(),
            source: e,
        })
    }
    /// `workspace.git_status` (JSON string value, ~1 KB server-side cap).
    pub async fn git_status(&self) -> Result<Value, WorkspaceClientError> {
        self.rpc(&GitStatusReq {}).await
    }
    pub async fn discover_skills(&self) -> Result<Vec<SkillInfo>, WorkspaceClientError> {
        self.rpc(&DiscoverSkillsReq {}).await
    }
    pub async fn discover_agents_md(&self) -> Result<Vec<AgentConfigFile>, WorkspaceClientError> {
        self.rpc(&DiscoverAgentsMdReq {}).await
    }
    pub async fn git_status_ext(
        &self,
        req: &GitStatusExtReq,
    ) -> Result<GitStatusExtResponse, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn git_files(
        &self,
        req: &GitFilesReq,
    ) -> Result<GitReadFilesData, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn git_diff(&self, req: &GitDiffReq) -> Result<GitDiffsData, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn git_stage(&self, req: &GitStageReq) -> Result<StageData, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn git_stage_content(
        &self,
        req: &GitStageContentReq,
    ) -> Result<(), WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn git_unstage(&self, req: &GitUnstageReq) -> Result<(), WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn git_discard(&self, req: &GitDiscardReq) -> Result<(), WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn git_commit(
        &self,
        req: &GitCommitReq,
    ) -> Result<CommitResult, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn git_sync_base(
        &self,
        req: &GitSyncBaseReq,
    ) -> Result<GitSyncBaseResult, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn git_checkout(&self, req: &GitCheckoutReq) -> Result<(), WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn git_stash(&self, req: &GitStashReq) -> Result<(), WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn git_info(&self, req: &GitInfoReq) -> Result<GitInfoData, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn git_branches(
        &self,
        req: &GitBranchesReq,
    ) -> Result<GitBranchListData, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn git_resolve_root(
        &self,
        req: &GitResolveRootReq,
    ) -> Result<Option<std::path::PathBuf>, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn git_current_commit(
        &self,
        req: &GitCurrentCommitReq,
    ) -> Result<Option<String>, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn detect_vcs_kind(
        &self,
        req: &DetectVcsKindReq,
    ) -> Result<VcsKind, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn git_checkout_commit(
        &self,
        req: &GitCheckoutCommitReq,
    ) -> Result<CheckoutCommitResponse, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn git_branch_info(&self) -> Result<Option<GitInfoData>, WorkspaceClientError> {
        self.rpc(&GitBranchInfoReq {}).await
    }
    pub async fn git_metadata(&self) -> Result<Value, WorkspaceClientError> {
        self.rpc(&GitMetadataReq {}).await
    }
    /// `workspace.git_collect_changes` — collect repository changes for serialization.
    pub async fn git_collect_changes(
        &self,
        req: &GitCollectChangesReq,
    ) -> Result<GitCollectChangesResponse, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn put_files(&self, req: &PutFilesReq) -> Result<PutFilesRes, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn get_files(&self, req: &GetFilesReq) -> Result<GetFilesRes, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn export_github(
        &self,
        req: &ExportGithubReq,
    ) -> Result<ExportGithubResponse, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn fs_list(&self, req: &FsListReq) -> Result<FsListData, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn fs_exists(&self, req: &FsExistsReq) -> Result<FsExistsData, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn fs_read_file(
        &self,
        req: &FsReadFileReq,
    ) -> Result<FsReadFileData, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn fs_write_file(&self, req: &FsWriteFileReq) -> Result<(), WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn fs_delete_file(&self, req: &FsDeleteFileReq) -> Result<(), WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn hunk_action(
        &self,
        req: &HunkSingleActionReq,
    ) -> Result<HunkActionResponse, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn hunk_file_action(
        &self,
        req: &HunkFileActionReq,
    ) -> Result<BulkHunkActionResponse, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn hunk_turn_action(
        &self,
        req: &HunkTurnActionReq,
    ) -> Result<BulkHunkActionResponse, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn hunk_all_action(
        &self,
        req: &HunkAllActionReq,
    ) -> Result<BulkHunkActionResponse, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn hunk_get_staged_files(&self) -> Result<Vec<String>, WorkspaceClientError> {
        self.rpc(&HunkGetStagedFilesReq {}).await
    }
    pub async fn hunk_get_file_summaries(&self) -> Result<Vec<FileSummary>, WorkspaceClientError> {
        self.rpc(&HunkGetFileSummariesReq {}).await
    }
    pub async fn code_goto_definition(
        &self,
        req: &CodeGotoDefinitionReq,
    ) -> Result<CodeNavResponse, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn code_goto_references(
        &self,
        req: &CodeGotoReferencesReq,
    ) -> Result<CodeNavResponse, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn code_find_definitions(
        &self,
        req: &CodeFindDefinitionsReq,
    ) -> Result<CodeNavResponse, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn code_find_references(
        &self,
        req: &CodeFindReferencesReq,
    ) -> Result<CodeNavResponse, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn code_index_status(
        &self,
        req: &CodeIndexStatusReq,
    ) -> Result<CodeIndexStatusResponse, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn ripgrep(
        &self,
        req: &ContentSearchRequest,
    ) -> Result<ContentSearchData, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn fuzzy_open(&self, req: &FuzzyOpenReq) -> Result<String, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn fuzzy_change(&self, req: &FuzzyChangeReq) -> Result<bool, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn fuzzy_close(&self, req: &FuzzyCloseReq) -> Result<bool, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn fuzzy_status(&self, req: &FuzzyStatusReq) -> Result<Value, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn create_worktree(
        &self,
        req: &CreateWorktreeRequest,
    ) -> Result<Value, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn worktree_create_sync(
        &self,
        req: &WorktreeCreateSyncReq,
    ) -> Result<Value, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn remove_worktree(
        &self,
        req: &RemoveWorktreeRequest,
    ) -> Result<Value, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn apply_worktree(
        &self,
        req: &ApplyWorktreeRequest,
    ) -> Result<Value, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn worktree_list(
        &self,
        req: &WorktreeListReq,
    ) -> Result<Value, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn worktree_show(
        &self,
        req: &WorktreeShowReq,
    ) -> Result<Value, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn worktree_gc(&self, req: &WorktreeGcReq) -> Result<Value, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn worktree_db_rebuild(&self) -> Result<Value, WorkspaceClientError> {
        self.rpc(&WorktreeDbRebuildReq {}).await
    }
    pub async fn worktree_db_path(&self) -> Result<WorktreeDbPathResponse, WorkspaceClientError> {
        self.rpc(&WorktreeDbPathReq {}).await
    }
    pub async fn worktree_db_stats(&self) -> Result<Value, WorkspaceClientError> {
        self.rpc(&WorktreeDbStatsReq {}).await
    }
    pub async fn begin_prompt(&self, req: &BeginPromptReq) -> Result<(), WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn end_prompt(&self, req: &EndPromptReq) -> Result<(), WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn rewind_to(
        &self,
        req: &RewindToReq,
    ) -> Result<FileRewindResponse, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn load_project_config(&self) -> Result<Value, WorkspaceClientError> {
        self.rpc(&LoadProjectConfigReq {}).await
    }
    pub async fn load_permissions(&self) -> Result<Value, WorkspaceClientError> {
        self.rpc(&LoadPermissionsReq {}).await
    }
    pub async fn load_envrc(&self) -> Result<Value, WorkspaceClientError> {
        self.rpc(&LoadEnvrcReq {}).await
    }
    pub async fn tool_definitions(
        &self,
        req: &ToolDefinitionsReq,
    ) -> Result<Value, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn resolve_file_references(
        &self,
        req: &ResolveFileReferencesReq,
    ) -> Result<Value, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn update_tool_config(
        &self,
        req: &UpdateToolConfigReq,
    ) -> Result<Value, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn drop_session(&self, req: &DropSessionReq) -> Result<Value, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn configure_mcp(
        &self,
        req: &ConfigureMcpReq,
    ) -> Result<Value, WorkspaceClientError> {
        self.rpc(req).await
    }
    pub async fn install_plugin(&self) -> Result<Value, WorkspaceClientError> {
        self.rpc(&InstallPluginReq {}).await
    }
    pub async fn refresh_plugins(&self) -> Result<Value, WorkspaceClientError> {
        self.rpc(&RefreshPluginsReq {}).await
    }
    pub async fn discover_plugins(&self) -> Result<Vec<Value>, WorkspaceClientError> {
        self.rpc(&DiscoverPluginsReq {}).await
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_computer_hub_sdk::harness::LocalRegistry;
    use echo_agent_tool_protocol::{SessionId, ToolId};
    use echo_agent_tool_runtime::{Tool, ToolError};
    use echo_agent_tool_types::ToolDescription;
    use echo_agent_workspace_types::rpc::RpcActivityClass;
    use echo_agent_workspace_types::rpc::skills::SkillScope;
    use schemars::JsonSchema;
    use serde::Deserialize;
    #[derive(Debug, Deserialize, JsonSchema)]
    struct RpcArgs {
        method: String,
        params: serde_json::Value,
    }
    #[derive(Debug, serde::Serialize)]
    #[serde(transparent)]
    struct RawOut(serde_json::Value);
    impl echo_agent_tool_runtime::ToolOutput for RawOut {}
    #[derive(Debug)]
    struct FakeWorkspaceRpc;
    impl Tool for FakeWorkspaceRpc {
        type Args = RpcArgs;
        type Output = RawOut;
        fn id(&self) -> ToolId {
            ToolId::new(WORKSPACE_RPC_TOOL_ID).unwrap()
        }
        fn description(
            &self,
            _ctx: &::echo_agent_tool_runtime::ListToolsContext,
        ) -> ToolDescription {
            ToolDescription::new(WORKSPACE_RPC_TOOL_ID, "fake workspace rpc")
        }
        async fn run(&self, _ctx: ToolCallContext, args: Self::Args) -> Result<RawOut, ToolError> {
            let ok = |v: serde_json::Value| Ok(RawOut(serde_json::json!({ "ok": v })));
            match args.method.as_str() {
                "workspace.info" => ok(serde_json::json!({
                    "os": "linux", "shell": "bash", "cwd": "/workspace",
                    "version": "1.2.3",
                })),
                "workspace.git_status" => ok(serde_json::json!("On branch main")),
                "workspace.discover_skills" => ok(serde_json::json!([{
                    "name": "my-skill",
                    "description": "A test skill",
                    "path": "/workspace/.echo-agent/skills/my-skill/SKILL.md",
                    "scope": "local",
                }])),
                "workspace.discover_agents_md" => ok(serde_json::json!([{
                    "file_name": "AGENTS.md",
                    "file_path": "/workspace/AGENTS.md",
                    "content": "# Project instructions",
                }])),
                "workspace.echo_params" => ok(args.params),
                "workspace.err" => Ok(RawOut(serde_json::json!({
                    "err": { "code": "session_not_found", "message": "ghost" },
                }))),
                "workspace.malformed" => Ok(RawOut(serde_json::json!({ "neither": true }))),
                "workspace.slow" => {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    ok(serde_json::Value::Null)
                }
                "workspace.netfail" => Err(ToolError::network_error("socket dropped")),
                "workspace.toolfail" => Err(ToolError::custom("some_code", "boom")),
                "workspace.hibernated" => Err(workspace_gone_tool_error(
                    echo_agent_tool_protocol::WorkspaceGoneReason::Hibernated,
                    echo_agent_tool_protocol::WorkspaceGonePhase::RouteMissing,
                )),
                "workspace.gone_retryable" => Err(workspace_gone_tool_error(
                    echo_agent_tool_protocol::WorkspaceGoneReason::NotBound,
                    echo_agent_tool_protocol::WorkspaceGonePhase::RouteMissing,
                )),
                other => panic!("unexpected method {other}"),
            }
        }
    }
    fn workspace_gone_tool_error(
        reason: echo_agent_tool_protocol::WorkspaceGoneReason,
        phase: echo_agent_tool_protocol::WorkspaceGonePhase,
    ) -> ToolError {
        let echo_agent_tool_protocol::ToolErrorWire::Custom {
            subcode,
            message,
            details,
        } = echo_agent_tool_protocol::workspace_unavailable_wire(reason, phase)
        else {
            panic!("workspace_unavailable_wire builds Custom");
        };
        ToolError::custom(subcode, message).with_details(details.expect("details always present"))
    }
    fn client() -> WorkspaceClient {
        let registry = LocalRegistry::new();
        registry.register(FakeWorkspaceRpc);
        let harness = ToolHarness::local_only_with(
            registry,
            SessionId::new("test").unwrap(),
            Default::default(),
        );
        WorkspaceClient::new(harness)
    }
    #[test]
    fn version_gating_treats_absent_and_unparseable_as_old() {
        let base = semver::Version::new(1, 2, 300);
        assert!(!server_version_at_least(None, &base));
        assert!(!server_version_at_least(Some("unknown"), &base));
        assert!(!server_version_at_least(Some("1.2.299"), &base));
        assert!(!server_version_at_least(Some("1.2.300-dev"), &base));
        assert!(server_version_at_least(Some("1.2.300"), &base));
        assert!(server_version_at_least(Some("1.3.0"), &base));
    }
    #[tokio::test]
    async fn info_decodes_typed_response() {
        let info = client().info().await.unwrap();
        assert_eq!(
            info,
            WorkspaceInfo {
                os: "linux".into(),
                shell: "bash".into(),
                cwd: "/workspace".into(),
                version: Some("1.2.3".into()),
            }
        );
    }
    #[tokio::test]
    async fn git_status_returns_raw_value() {
        let v = client().git_status().await.unwrap();
        assert_eq!(v, serde_json::json!("On branch main"));
    }
    #[tokio::test]
    async fn discover_skills_decodes_typed_list() {
        let skills = client().discover_skills().await.unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "my-skill");
        assert_eq!(skills[0].scope, SkillScope::Local);
    }
    #[tokio::test]
    async fn discover_agents_md_decodes_typed_list() {
        let files = client().discover_agents_md().await.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].file_name, "AGENTS.md");
        assert_eq!(files[0].file_path, "/workspace/AGENTS.md");
    }
    #[tokio::test]
    async fn typed_request_params_round_trip() {
        #[derive(Debug, serde::Serialize)]
        struct EchoReq {
            flag: bool,
            n: u32,
        }
        impl WorkspaceRpc for EchoReq {
            const METHOD: &'static str = "workspace.echo_params";
            const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
            type Response = Value;
        }
        let echoed = client().rpc(&EchoReq { flag: true, n: 7 }).await.unwrap();
        assert_eq!(echoed, serde_json::json!({ "flag": true, "n": 7 }));
    }
    #[tokio::test]
    async fn err_envelope_maps_to_rpc_error() {
        let c = client();
        let raw = c.rpc_raw("workspace.err", Value::Null).await.unwrap();
        assert!(raw.get("err").is_some());
        #[derive(Debug, serde::Serialize)]
        struct ErrReq;
        impl WorkspaceRpc for ErrReq {
            const METHOD: &'static str = "workspace.err";
            const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
            type Response = Value;
        }
        let err = c.rpc(&ErrReq).await.unwrap_err();
        match err {
            WorkspaceClientError::Rpc(e) => {
                assert_eq!(e.code, "session_not_found");
                assert_eq!(e.message, "ghost");
            }
            other => panic!("expected Rpc, got {other:?}"),
        }
        assert!(c.is_connected(), "envelope errors must not trip the latch");
    }
    #[tokio::test]
    async fn malformed_envelope_maps_to_decode() {
        let c = client();
        #[derive(Debug, serde::Serialize)]
        struct MalformedReq;
        impl WorkspaceRpc for MalformedReq {
            const METHOD: &'static str = "workspace.malformed";
            const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
            type Response = Value;
        }
        let err = c.rpc(&MalformedReq).await.unwrap_err();
        assert!(
            matches!(err, WorkspaceClientError::Decode { .. }),
            "{err:?}"
        );
        assert!(c.is_connected());
    }
    #[tokio::test]
    async fn network_error_trips_latch_and_fast_fails() {
        let c = client();
        let err = c
            .rpc_raw("workspace.netfail", Value::Null)
            .await
            .unwrap_err();
        assert!(matches!(err, WorkspaceClientError::Transport(_)), "{err:?}");
        assert!(!c.is_connected(), "network error must trip the latch");
        let err = c.rpc_raw("workspace.info", Value::Null).await.unwrap_err();
        assert!(matches!(err, WorkspaceClientError::NotConnected));
        c.mark_connected();
        assert!(c.rpc_raw("workspace.info", Value::Null).await.is_ok());
    }
    #[tokio::test]
    async fn non_fatal_tool_error_leaves_latch_up() {
        let c = client();
        let err = c
            .rpc_raw("workspace.toolfail", Value::Null)
            .await
            .unwrap_err();
        assert!(matches!(err, WorkspaceClientError::Transport(_)), "{err:?}");
        assert!(
            c.is_connected(),
            "non-fatal tool errors must not trip the latch"
        );
    }
    #[tokio::test]
    async fn hibernated_route_miss_maps_to_workspace_hibernated() {
        let c = client();
        let err = c
            .rpc_raw("workspace.hibernated", Value::Null)
            .await
            .unwrap_err();
        assert!(
            matches!(err, WorkspaceClientError::WorkspaceHibernated),
            "{err:?}"
        );
        assert!(c.is_connected(), "hibernation must not trip the latch");
    }
    #[tokio::test]
    async fn retryable_workspace_unavailable_stays_transport() {
        let c = client();
        let err = c
            .rpc_raw("workspace.gone_retryable", Value::Null)
            .await
            .unwrap_err();
        assert!(matches!(err, WorkspaceClientError::Transport(_)), "{err:?}");
        assert!(c.is_connected());
    }
    #[tokio::test(start_paused = true)]
    async fn deadline_times_out_slow_calls() {
        let c = client().with_deadline(Duration::from_secs(3));
        let err = c.rpc_raw("workspace.slow", Value::Null).await.unwrap_err();
        match err {
            WorkspaceClientError::Timeout { method, after } => {
                assert_eq!(method, "workspace.slow");
                assert_eq!(after, Duration::from_secs(3));
            }
            other => panic!("expected Timeout, got {other:?}"),
        }
        assert!(c.is_connected(), "timeouts must not trip the latch");
    }
    #[tokio::test(start_paused = true)]
    async fn no_deadline_by_default_waits_out_slow_calls() {
        let v = client()
            .rpc_raw("workspace.slow", Value::Null)
            .await
            .unwrap();
        assert!(v.get("ok").is_some());
    }
    #[tokio::test]
    async fn shared_connected_flag_is_externally_controllable() {
        let registry = LocalRegistry::new();
        registry.register(FakeWorkspaceRpc);
        let harness = ToolHarness::local_only_with(
            registry,
            SessionId::new("test").unwrap(),
            Default::default(),
        );
        let flag = Arc::new(AtomicBool::new(true));
        let c = WorkspaceClient::with_connected_flag(harness, flag.clone());
        flag.store(false, Ordering::Relaxed);
        let err = c.rpc_raw("workspace.info", Value::Null).await.unwrap_err();
        assert!(matches!(err, WorkspaceClientError::NotConnected));
        flag.store(true, Ordering::Relaxed);
        assert!(c.rpc_raw("workspace.info", Value::Null).await.is_ok());
    }
    #[tokio::test]
    async fn clones_share_the_latch() {
        let a = client();
        let b = a.clone();
        a.mark_disconnected();
        assert!(!b.is_connected());
    }
    #[tokio::test]
    async fn consume_stream_terminal_returns_ok() {
        let value = serde_json::json!({"result": "hello"});
        let typed = TypedToolOutput::from_value(ToolId::new("t").unwrap(), value.clone());
        let mut stream = echo_agent_tool_runtime::terminal_only(Ok(typed));
        assert_eq!(
            consume_stream_terminal(&mut stream).await.unwrap().value,
            value
        );
    }
    #[tokio::test]
    async fn consume_stream_terminal_returns_err() {
        let mut stream: echo_agent_tool_runtime::ToolStream<TypedToolOutput> =
            echo_agent_tool_runtime::terminal_only(Err(ToolError::network_error("oops")));
        let err = consume_stream_terminal(&mut stream).await.unwrap_err();
        assert!(err.to_string().contains("oops"));
    }
    #[tokio::test]
    async fn consume_stream_terminal_exhausted_stream_is_network_error() {
        let typed = TypedToolOutput::from_value(ToolId::new("t").unwrap(), Value::Null);
        let mut stream = echo_agent_tool_runtime::terminal_only::<TypedToolOutput>(Ok(typed));
        let _ = consume_stream_terminal(&mut stream).await;
        let err = consume_stream_terminal(&mut stream).await.unwrap_err();
        assert!(
            err.to_string()
                .contains("stream ended without terminal item")
        );
        assert!(is_transport_fatal(&err));
    }
}
