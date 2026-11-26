//! Git extension API layer.
//!
//! Routing: prefers explicit `gitRoot`, falls back to session lookup via `sessionId`.
//! Business logic delegated to `session::git::*` pure functions.
//!
//! **Phase 4 design note**: Git/JJ functions (`git_cli`, `status`,
//! `detect_vcs_kind`, `find_git_root_from_path`, etc.) are stateless
//! utilities that take a `&Path` and shell out to `git`/`jj`. They do
//! not access workspace state and therefore remain direct calls rather
//! than routing through `WorkspaceChannel`. The channel's VCS stubs
//! (`git_status`, `git_diff`, etc.) are reserved for future stateful
//! operations (e.g. cached VCS state, cross-session conflict detection).
use super::{Empty, ExtResult, parse_params, to_ext_response, to_ext_response_partial};
use crate::agent::MvpAgent;
use crate::session::ExtMethodResult;
use agent_client_protocol as acp;
use echo_agent_workspace::session::git::{
    self, DiscardScope, GitDiffsData, check_diff_size_limits,
};
use echo_agent_workspace::workspace_ops::{
    GitBranchesReq, GitCheckoutCommitReq, GitCheckoutReq, GitCommitReq, GitCurrentCommitReq,
    GitDiffReq, GitDiscardReq, GitFilesReq, GitInfoReq, GitStageContentReq, GitStageReq,
    GitStashReq, GitStatusExtReq, GitStatusFormat, GitUnstageReq,
};
use serde::Deserialize;
use std::path::PathBuf;
fn default_head() -> String {
    "HEAD".to_string()
}
fn default_working() -> String {
    "working".to_string()
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitStatusRequest {
    #[serde(default)]
    pub session_id: Option<acp::SessionId>,
    #[serde(default)]
    pub git_root: Option<String>,
    pub include_untracked: Option<bool>,
    pub include_stats: Option<bool>,
    pub ignore_submodules: Option<bool>,
    pub include_patches: Option<bool>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitFilesRequest {
    #[serde(default)]
    pub session_id: Option<acp::SessionId>,
    #[serde(default)]
    pub git_root: Option<String>,
    pub paths: Vec<String>,
    #[serde(default = "default_head")]
    pub version: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitDiffsRequest {
    #[serde(default)]
    pub session_id: Option<acp::SessionId>,
    #[serde(default)]
    pub git_root: Option<String>,
    #[serde(default)]
    pub paths: Option<Vec<String>>,
    #[serde(default = "default_head")]
    pub from: String,
    #[serde(default = "default_working")]
    pub to: String,
    #[serde(default)]
    pub include_patch: bool,
    #[serde(default)]
    pub include_content: bool,
    #[serde(default)]
    pub max_patch_bytes: Option<usize>,
    #[serde(default)]
    pub max_patch_lines: Option<usize>,
    #[serde(default)]
    pub merge_base: bool,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitStageRequest {
    #[serde(default)]
    pub session_id: Option<acp::SessionId>,
    #[serde(default)]
    pub git_root: Option<String>,
    pub paths: Option<Vec<String>>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitStageContentRequest {
    #[serde(default)]
    pub session_id: Option<acp::SessionId>,
    #[serde(default)]
    pub git_root: Option<String>,
    pub path: String,
    pub content: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitUnstageRequest {
    #[serde(default)]
    pub session_id: Option<acp::SessionId>,
    #[serde(default)]
    pub git_root: Option<String>,
    pub paths: Option<Vec<String>>,
}
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GitDiscardScope {
    Working,
    Staged,
    #[default]
    Both,
}
impl From<GitDiscardScope> for DiscardScope {
    fn from(s: GitDiscardScope) -> Self {
        match s {
            GitDiscardScope::Working => DiscardScope::Working,
            GitDiscardScope::Staged => DiscardScope::Staged,
            GitDiscardScope::Both => DiscardScope::Both,
        }
    }
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitDiscardRequest {
    #[serde(default)]
    pub session_id: Option<acp::SessionId>,
    #[serde(default)]
    pub git_root: Option<String>,
    pub paths: Option<Vec<String>>,
    #[serde(default)]
    pub include_untracked: bool,
    #[serde(default)]
    scope: GitDiscardScope,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitCommitRequest {
    #[serde(default)]
    pub session_id: Option<acp::SessionId>,
    #[serde(default)]
    pub git_root: Option<String>,
    pub message: String,
    #[serde(default)]
    pub amend: bool,
    #[serde(default)]
    pub signoff: bool,
    #[serde(default)]
    pub push: bool,
    #[serde(default)]
    pub sync: bool,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitStashRequest {
    #[serde(default)]
    pub session_id: Option<acp::SessionId>,
    #[serde(default)]
    pub git_root: Option<String>,
    #[serde(default)]
    pub include_untracked: bool,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitCheckoutRequest {
    #[serde(default)]
    pub session_id: Option<acp::SessionId>,
    #[serde(default)]
    pub git_root: Option<String>,
    pub branch: String,
    #[serde(default)]
    pub create: bool,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CheckoutSessionHeadRequest {
    pub session_id: acp::SessionId,
    #[serde(default)]
    pub git_root: Option<String>,
    #[serde(default)]
    pub stash_if_dirty: bool,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitInfoRequest {
    #[serde(default)]
    pub session_id: Option<acp::SessionId>,
    #[serde(default)]
    pub git_root: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitBranchesRequest {
    #[serde(default)]
    pub session_id: Option<acp::SessionId>,
    #[serde(default)]
    pub git_root: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitCurrentCommitRequest {
    #[serde(default)]
    pub session_id: Option<acp::SessionId>,
    #[serde(default)]
    pub git_root: Option<String>,
}
/// Request for echo.agent/git/checkout_commit extension method.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitCheckoutCommitRequest {
    #[serde(default)]
    pub session_id: Option<acp::SessionId>,
    #[serde(default)]
    pub git_root: Option<String>,
    /// Commit hash or ref to checkout.
    pub commit: String,
    #[serde(default)]
    pub stash_if_dirty: bool,
}
/// Resolve git_root from explicit value or session lookup via [`WorkspaceOps`].
async fn resolve_git_root(
    agent: &MvpAgent,
    ops: &echo_agent_workspace::WorkspaceOps,
    git_root: Option<String>,
    session_id: Option<&acp::SessionId>,
) -> Result<PathBuf, acp::Error> {
    if let Some(root) = git_root {
        return Ok(PathBuf::from(root));
    }
    if let Some(sid) = session_id {
        if let Some(cwd) = agent.get_session_cwd(sid) {
            let result = ops
                .dispatch(
                    &echo_agent_workspace::workspace_ops::GitResolveRootReq { cwd },
                    None,
                )
                .await
                .map_err(|e| {
                    acp::Error::invalid_params()
                        .data(format!("cannot find git root from session cwd: {}", e))
                })?;
            return result.ok_or_else(|| {
                acp::Error::invalid_params()
                    .data("cannot find git root from session cwd: not a git repository")
            });
        }
        return Err(acp::Error::invalid_params().data(format!("session not found: {}", sid.0)));
    }
    Err(acp::Error::invalid_params().data("either gitRoot or sessionId is required"))
}
/// Try to extract a git_root from the request params (best-effort, for jj routing).
async fn try_resolve_git_root(
    agent: &MvpAgent,
    ops: &echo_agent_workspace::WorkspaceOps,
    args: &acp::ExtRequest,
) -> Option<PathBuf> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Probe {
        git_root: Option<String>,
        session_id: Option<agent_client_protocol::SessionId>,
    }
    let probe: Probe = serde_json::from_str(args.params.get()).ok()?;
    if let Some(root) = probe.git_root {
        return Some(PathBuf::from(root));
    }
    if let Some(sid) = &probe.session_id
        && let Some(cwd) = agent.get_session_cwd(sid)
    {
        return ops
            .dispatch(
                &echo_agent_workspace::workspace_ops::GitResolveRootReq { cwd },
                None,
            )
            .await
            .ok()
            .flatten();
    }
    None
}
pub async fn handle(
    agent: &MvpAgent,
    ops: &echo_agent_workspace::WorkspaceOps,
    args: &acp::ExtRequest,
) -> ExtResult {
    if let Some(git_root) = try_resolve_git_root(agent, ops, args).await {
        let vcs_kind = ops
            .dispatch(
                &echo_agent_workspace::workspace_ops::DetectVcsKindReq {
                    path: git_root.clone(),
                },
                None,
            )
            .await
            .unwrap_or(echo_agent_workspace::session::git::VcsKind::Git);
        if vcs_kind.is_jj()
            && let Some(result) =
                super::jj::try_handle(args.method.as_ref(), &git_root, &args.params).await
        {
            return result;
        }
    }
    match args.method.as_ref() {
        "echo.agent/git/git_repo_root" => {
            let req: git::GitRepoRequest = parse_params(args)?;
            let response = git::is_git_repo(&req).await?;
            super::to_raw_response(&response)
        }
        "echo.agent/git/serialize_changes" => {
            let _ = (args, ops);
            to_ext_response::<()>(Err(anyhow::anyhow!(
                "git serialize_changes is unavailable in this build"
            )))
        }
        "echo.agent/git/status" => {
            let req = parse_params::<GitStatusRequest>(args)?;
            let include_untracked = req.include_untracked.unwrap_or(false);
            let include_stats = req.include_stats.unwrap_or(false);
            let ignore_submodules = req.ignore_submodules.unwrap_or(true);
            let include_patches = req.include_patches.unwrap_or(false);
            let git_root = resolve_git_root(agent, ops, req.git_root, req.session_id.as_ref())
                .await
                .ok();
            let op = GitStatusExtReq {
                git_root,
                include_untracked,
                include_stats,
                ignore_submodules,
                include_patches,
                format: GitStatusFormat::Structured,
            };
            let response = ops
                .dispatch(&op, None)
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
            let result = response.data.ok_or_else(|| {
                acp::Error::internal_error().data("git_status_ext returned no structured data")
            })?;
            to_ext_response(Ok(result))
        }
        "echo.agent/git/files" => {
            let req = parse_params::<GitFilesRequest>(args)?;
            let git_root = resolve_git_root(agent, ops, req.git_root, req.session_id.as_ref())
                .await
                .ok();
            let op = GitFilesReq {
                git_root,
                paths: req.paths.clone(),
                version: req.version.clone(),
            };
            let result = ops
                .dispatch(&op, None)
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
            to_ext_response(Ok(result))
        }
        "echo.agent/git/diffs" => {
            let req = parse_params::<GitDiffsRequest>(args)?;
            let max_bytes = req.max_patch_bytes;
            let max_lines = req.max_patch_lines;
            let git_root = resolve_git_root(agent, ops, req.git_root, req.session_id.as_ref())
                .await
                .ok();
            let op = GitDiffReq {
                git_root,
                paths: req.paths.clone(),
                from: req.from.clone(),
                to: req.to.clone(),
                include_patch: req.include_patch,
                include_content: req.include_content,
                merge_base: req.merge_base,
            };
            let data = ops
                .dispatch(&op, None)
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
            if let Err(err) = check_diff_size_limits(&data, max_bytes, max_lines) {
                let ext_result = ExtMethodResult::<GitDiffsData>::failure(err.message());
                ext_result
                    .to_ext_response()
                    .map_err(|e| acp::Error::internal_error().data(e.to_string()))
            } else {
                to_ext_response(Ok(data))
            }
        }
        "echo.agent/git/stage" => {
            let req = parse_params::<GitStageRequest>(args)?;
            let git_root = resolve_git_root(agent, ops, req.git_root, req.session_id.as_ref())
                .await
                .ok();
            let op = GitStageReq {
                git_root: git_root.clone(),
                paths: req.paths,
            };
            let result = ops
                .dispatch(&op, None)
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
            to_ext_response(Ok(result))
        }
        "echo.agent/git/stage/content" => {
            let req = parse_params::<GitStageContentRequest>(args)?;
            let git_root = resolve_git_root(agent, ops, req.git_root, req.session_id.as_ref())
                .await
                .ok();
            let op = GitStageContentReq {
                git_root: git_root.clone(),
                path: req.path.clone(),
                content: req.content.clone(),
            };
            ops.dispatch(&op, None)
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
            to_ext_response(Ok(Empty {}))
        }
        "echo.agent/git/unstage" => {
            let req = parse_params::<GitUnstageRequest>(args)?;
            let git_root = resolve_git_root(agent, ops, req.git_root, req.session_id.as_ref())
                .await
                .ok();
            let op = GitUnstageReq {
                git_root: git_root.clone(),
                paths: req.paths,
            };
            ops.dispatch(&op, None)
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
            to_ext_response(Ok(Empty {}))
        }
        "echo.agent/git/discard" => {
            let req = parse_params::<GitDiscardRequest>(args)?;
            let git_root = resolve_git_root(agent, ops, req.git_root, req.session_id.as_ref())
                .await
                .ok();
            let op = GitDiscardReq {
                git_root: git_root.clone(),
                paths: req.paths,
                scope: req.scope.into(),
                include_untracked: req.include_untracked,
            };
            ops.dispatch(&op, None)
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
            to_ext_response(Ok(Empty {}))
        }
        "echo.agent/git/commit" => {
            let req = parse_params::<GitCommitRequest>(args)?;
            let git_root = resolve_git_root(agent, ops, req.git_root, req.session_id.as_ref())
                .await
                .ok();
            let op = GitCommitReq {
                git_root: git_root.clone(),
                message: req.message.clone(),
                amend: req.amend,
                signoff: req.signoff,
                push: req.push,
                sync: req.sync,
                ..Default::default()
            };
            let commit_result = ops
                .dispatch(&op, None)
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
            to_ext_response_partial(Ok(commit_result.data), commit_result.warning)
        }
        "echo.agent/git/checkout" => {
            let req = parse_params::<GitCheckoutRequest>(args)?;
            let git_root = resolve_git_root(agent, ops, req.git_root, req.session_id.as_ref())
                .await
                .ok();
            let op = GitCheckoutReq {
                git_root: git_root.clone(),
                branch: req.branch.clone(),
                create: req.create,
            };
            ops.dispatch(&op, None)
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
            to_ext_response(Ok(Empty {}))
        }
        "echo.agent/git/stash" => {
            let req = parse_params::<GitStashRequest>(args)?;
            let git_root = resolve_git_root(agent, ops, req.git_root, req.session_id.as_ref())
                .await
                .ok();
            let op = GitStashReq {
                git_root: git_root.clone(),
                include_untracked: req.include_untracked,
            };
            ops.dispatch(&op, None)
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
            to_ext_response(Ok(Empty {}))
        }
        "echo.agent/git/info" => {
            let req = parse_params::<GitInfoRequest>(args)?;
            let git_root = resolve_git_root(agent, ops, req.git_root, req.session_id.as_ref())
                .await
                .ok();
            let result = ops
                .dispatch(&GitInfoReq { git_root }, None)
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
            to_ext_response(Ok(result))
        }
        "echo.agent/git/branches" => {
            let req = parse_params::<GitBranchesRequest>(args)?;
            let git_root = resolve_git_root(agent, ops, req.git_root, req.session_id.as_ref())
                .await
                .ok();
            let result = ops
                .dispatch(&GitBranchesReq { git_root }, None)
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
            to_ext_response(Ok(result))
        }
        "echo.agent/git/current_commit" => {
            let req = parse_params::<GitCurrentCommitRequest>(args)?;
            let result = match resolve_git_root(agent, ops, req.git_root, req.session_id.as_ref())
                .await
                .ok()
            {
                Some(git_root) => ops
                    .dispatch(&GitCurrentCommitReq { git_root }, None)
                    .await
                    .map_err(|e| acp::Error::internal_error().data(e.to_string()))?,
                None => None,
            };
            to_ext_response(Ok(result))
        }
        "echo.agent/git/checkout_session_head" => {
            let req = parse_params::<CheckoutSessionHeadRequest>(args)?;
            let git_root =
                resolve_git_root(agent, ops, req.git_root, Some(&req.session_id)).await?;
            let vcs_kind = ops
                .dispatch(
                    &echo_agent_workspace::workspace_ops::DetectVcsKindReq {
                        path: git_root.clone(),
                    },
                    None,
                )
                .await
                .unwrap_or(echo_agent_workspace::session::git::VcsKind::Git);
            if vcs_kind.is_jj() {
                return Err(acp::Error::invalid_request()
                    .data("checkout_session_head is not supported in jj repositories"));
            }
            let summary =
                crate::session::persistence::find_summary_by_session_id(&req.session_id.0)
                    .ok_or_else(|| {
                        acp::Error::invalid_params()
                            .data(format!("session {} not found", req.session_id.0))
                    })?;
            let head_commit = summary.head_commit.ok_or_else(|| {
                acp::Error::invalid_params().data(format!(
                    "session {} has no persisted HEAD commit",
                    req.session_id.0
                ))
            })?;
            let result = ops
                .dispatch(
                    &echo_agent_workspace::workspace_ops::GitCheckoutCommitReq {
                        git_root: git_root.clone(),
                        head_commit,
                        head_branch: summary.head_branch,
                        stash_if_dirty: req.stash_if_dirty,
                    },
                    None,
                )
                .await
                .map_err(|e| acp::Error::internal_error().data(format!("checkout failed: {e}")))?;
            super::to_raw_response(&result)
        }
        "echo.agent/git/checkout_commit" => {
            let req = parse_params::<GitCheckoutCommitRequest>(args)?;
            let git_root =
                resolve_git_root(agent, ops, req.git_root, req.session_id.as_ref()).await?;
            let vcs_kind = ops
                .dispatch(
                    &echo_agent_workspace::workspace_ops::DetectVcsKindReq {
                        path: git_root.clone(),
                    },
                    None,
                )
                .await
                .unwrap_or(echo_agent_workspace::session::git::VcsKind::Git);
            if vcs_kind.is_jj() {
                return Err(acp::Error::invalid_request().data(
                    "checkout_commit is not supported in jj repos; use `jj new` or `jj edit`",
                ));
            }
            let result = ops
                .dispatch(
                    &GitCheckoutCommitReq {
                        git_root: git_root.clone(),
                        head_commit: req.commit,
                        head_branch: None,
                        stash_if_dirty: req.stash_if_dirty,
                    },
                    None,
                )
                .await
                .map_err(|e| acp::Error::internal_error().data(format!("checkout failed: {e}")))?;
            super::to_raw_response(&result)
        }
        _ => Err(acp::Error::method_not_found()),
    }
}
