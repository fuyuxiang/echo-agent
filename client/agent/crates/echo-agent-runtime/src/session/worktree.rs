//! Git worktree operations: create, list, remove, apply.
//!
//! Core worktree lifecycle logic lives in [`echo_agent_workspace::worktree`].
//! This module re-exports everything from there and adds session-aware
//! functions that depend on shell-specific infrastructure (persistence,
//! auth, registry client, storage client, session restore).
use crate::util::config::WorktreeType as ShellWorktreeType;
use anyhow::{Context, Result};
use echo_agent_workspace::session::git::find_git_root_from_path;
pub use echo_agent_workspace::worktree::*;
use std::path::Path;
const WORKTREE_LOG: &str = "echo_agent_worktree";
/// Resume always consults the grove gate with `remote = None` (fail closed).
pub(crate) fn resume_grove_worktree_flag() -> Option<bool> {
    Some(crate::util::config::grove_worktree_enabled(None))
}
impl From<ShellWorktreeType> for WorktreeType {
    fn from(t: ShellWorktreeType) -> Self {
        match t {
            ShellWorktreeType::Linked => WorktreeType::Linked,
            ShellWorktreeType::Standalone => WorktreeType::Standalone,
            ShellWorktreeType::Git => WorktreeType::Git,
        }
    }
}
impl From<WorktreeType> for ShellWorktreeType {
    fn from(t: WorktreeType) -> Self {
        match t {
            WorktreeType::Linked => ShellWorktreeType::Linked,
            WorktreeType::Standalone => ShellWorktreeType::Standalone,
            WorktreeType::Git => ShellWorktreeType::Git,
        }
    }
}
/// Create a worktree for the resume-session flow, detecting jj vs git automatically.
///
/// When `git_ref` is set, forces a clean checkout of that ref (same as the
/// manual `create_from_worktree_sync` path used by `echoagent -w --ref`).
async fn create_worktree_for_resume(
    source_cwd: &str,
    copy_mode: WorktreeCopyMode,
    worktree_type: ShellWorktreeType,
    git_ref: Option<String>,
) -> Result<CreateWorktreeFromWorktreeResponse> {
    let copy_mode = if git_ref.is_some() {
        WorktreeCopyMode::Clean
    } else {
        copy_mode
    };
    let wt_req = CreateWorktreeFromWorktreeRequest {
        source_worktree_path: source_cwd.to_owned(),
        new_session_id: uuid::Uuid::now_v7().to_string(),
        copy_mode,
        git_ref,
        worktree_type: Some(WorktreeType::from(worktree_type)),
        label: None,
        grove_worktree: resume_grove_worktree_flag(),
        cancellation_token: None,
        resolved_dest_path: None,
    };
    let source = std::path::Path::new(source_cwd);
    if find_git_root_from_path(source)
        .ok()
        .is_some_and(|root| echo_agent_workspace::session::git::detect_vcs_kind(&root).is_jj())
    {
        create_jj_workspace(&wt_req).await
    } else {
        create_worktree_from_worktree_sync(&wt_req).await
    }
}
/// Best-effort cleanup of a worktree created during a failed resume flow.
async fn cleanup_worktree_on_failure(source_cwd: &str, worktree_path: &str) {
    let wt = std::path::Path::new(worktree_path);
    if !wt.exists() {
        return;
    }
    let is_jj = find_git_root_from_path(std::path::Path::new(source_cwd))
        .ok()
        .is_some_and(|root| echo_agent_workspace::session::git::detect_vcs_kind(&root).is_jj());
    if is_jj {
        if let Err(e) = remove_jj_workspace(worktree_path).await {
            tracing::warn!(error = %e, "failed to clean up jj workspace after failure");
        }
    } else {
        let wt_path = wt.to_path_buf();
        match tokio::task::spawn_blocking(move || {
            echo_agent_fast_worktree::remove_worktree(&wt_path)
        })
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "fast remove_worktree failed during cleanup, trying rm");
                let _ = tokio::fs::remove_dir_all(wt).await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "remove_worktree task panicked during cleanup, trying rm");
                let _ = tokio::fs::remove_dir_all(wt).await;
            }
        }
        if let Ok(root) = find_git_root_from_path(std::path::Path::new(source_cwd)) {
            let wt_path = wt.to_path_buf();
            let _ = tokio::task::spawn_blocking(move || {
                echo_agent_fast_worktree::remove_stale_worktree_registration(&root, &wt_path)
            })
            .await;
        }
    }
}
/// Check out a persisted HEAD commit in a worktree, with fetch fallback.
///
/// Always stashes any dirty state (the worktree may carry copies of the
/// source's uncommitted changes under `copy_mode: dirty`) before invoking
/// `git checkout` so the caller can surface the stash ref to the user.
pub(crate) async fn checkout_persisted_head_in_worktree(
    worktree_path: &str,
    head_commit: Option<&str>,
    session_id: &str,
) -> echo_agent_workspace::session::git::CheckoutSessionOutcome {
    let sha = match head_commit {
        Some(s) if !s.is_empty() => s,
        _ => return echo_agent_workspace::session::git::CheckoutSessionOutcome::default(),
    };
    echo_agent_workspace::session::git::checkout_session_commit(
        Path::new(worktree_path),
        sha,
        true,
        session_id,
    )
    .await
}
/// Decision returned to the worktree restore caller.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct WorktreeRestoreDecision {
    pub code_restored: bool,
    pub restore_summary: Option<String>,
    pub restore_degree: Option<echo_agent_workspace::session::git::RestoreDegree>,
}
/// Thin wire-format adapter over the shared
/// [`echo_agent_workspace::session::git::build_restore_decision`] helper.
pub(crate) fn build_worktree_restore_outcome(
    head_commit: Option<&str>,
    outcome: &echo_agent_workspace::session::git::CheckoutSessionOutcome,
    kind: echo_agent_workspace::session::git::RestoreKind,
) -> WorktreeRestoreDecision {
    let d = echo_agent_workspace::session::git::build_restore_decision(head_commit, outcome, kind);
    WorktreeRestoreDecision {
        code_restored: d.restored,
        restore_summary: d.summary,
        restore_degree: d.degree,
    }
}
use crate::session::persistence::{ResolvedLocalSession, resolve_local_session_for_repo};
/// Combined backend helper: resolve a session across all worktree roots
/// belonging to the same repo as `current_cwd`.
pub(crate) fn resolve_session_repo_wide(
    session_id: &str,
    current_cwd: &std::path::Path,
) -> Result<Option<ResolvedLocalSession>> {
    let candidates = candidate_worktree_cwds_for_same_repo(current_cwd)?;
    let refs: Vec<&str> = candidates.iter().map(String::as_str).collect();
    Ok(resolve_local_session_for_repo(session_id, &refs))
}
/// Whether remote worktree resume should fetch/apply the repository snapshot.
pub(crate) fn remote_worktree_restores_codebase(
    restore_code: Option<bool>,
    restore_code_default: bool,
) -> bool {
    restore_code.unwrap_or(restore_code_default)
}
/// Orchestrate the full "resume session in worktree" flow.
///
/// Shell-side orchestration: composes client ops (session persistence,
/// auth, registry) with server ops (worktree creation, git, fetch+extract)
/// dispatched through `WorkspaceOps`.
pub(crate) async fn resume_session_in_worktree(
    req: &ResumeSessionInWorktreeRequest,
    ops: &echo_agent_workspace::WorkspaceOps,
    worktree_type_default: ShellWorktreeType,
    restore_code_default: bool,
    registry_client: Option<&crate::agent::session_registry_client::SessionRegistryClient>,
    auth_manager: Option<std::sync::Arc<crate::auth::AuthManager>>,
    agent_id: &str,
) -> Result<ResumeSessionInWorktreeResponse> {
    use echo_agent_workspace::session::git::effective_worktree_path;
    tracing::info!(
        target: WORKTREE_LOG,
        session_id = %req.session_id,
        restore_code = ?req.restore_code,
        restore_code_default,
        effective_restore_code = req.restore_code.unwrap_or(restore_code_default),
        "RESTORE_CODE_DEBUG: resume_session_in_worktree entry"
    );
    let cwd_path = std::path::Path::new(req.source_cwd.as_str());
    let local_resolution = resolve_session_repo_wide(&req.session_id, cwd_path);
    if let Ok(Some(resolved)) = local_resolution {
        tracing::info!(
            target: WORKTREE_LOG,
            session_id = %req.session_id,
            resolved_cwd = %resolved.cwd,
            kind = ?resolved.resolution_kind,
            "RESUME_LOCAL_RESOLVED: session found via repo-wide lookup"
        );
        return resume_local_session_in_worktree(
            req,
            ops,
            &resolved.session_id,
            &resolved.cwd,
            worktree_type_default,
            restore_code_default,
            registry_client,
            auth_manager,
            agent_id,
        )
        .await;
    }
    let client = registry_client.ok_or_else(|| {
        anyhow::anyhow!(
            "Session {} not found locally and session registry is not available \
             (auth may be missing or registry is disabled)",
            req.session_id,
        )
    })?;
    tracing::info!(
        session_id = %req.session_id,
        "Restoring remote session: creating worktree first to keep source clean"
    );
    let worktree_type = req
        .worktree_type
        .map(ShellWorktreeType::from)
        .unwrap_or(worktree_type_default);
    let wt_resp = create_worktree_for_resume(
        &req.source_cwd,
        req.copy_mode,
        worktree_type,
        req.git_ref.clone(),
    )
    .await?;
    let record = client
        .get_session(&req.session_id)
        .await
        .context("fetching session record for remote restore")?;
    let turn = crate::session::restore::resolve_restore_turn(&record, None);
    let restore_code = remote_worktree_restores_codebase(req.restore_code, restore_code_default);
    let memory_dl_future = crate::session::restore::download_to_tempfile(
        client,
        &req.session_id,
        "memory.tar.gz",
        turn,
    );
    let state_dl_future = async {
        Err(anyhow::anyhow!(
            "session-state archive restore unavailable in this build"
        ))
    };
    let (memory_dl, state_dl) = {
        let _ = restore_code;
        tokio::join!(memory_dl_future, state_dl_future)
    };
    let codebase_ok = false;
    let _memory_result =
        crate::session::restore::apply_memory_download(memory_dl, &wt_resp.worktree_path).await;
    let (session_state_result, local_session_id) =
        crate::session::restore::apply_session_state_download(
            state_dl,
            &req.session_id,
            &wt_resp.worktree_path,
        )
        .await;
    if session_state_result.is_skipped() {
        cleanup_worktree_on_failure(&req.source_cwd, &wt_resp.worktree_path).await;
        anyhow::bail!(
            "Session {} session-state archive was unavailable -- \
             conversation history cannot be recovered. Retry in a few moments.",
            req.session_id,
        );
    }
    let worktree_root = std::path::Path::new(&wt_resp.worktree_path);
    let source_path = std::path::Path::new(&req.source_cwd);
    let source_git_root = wt_resp.source_git_root.as_deref().map(std::path::Path::new);
    let effective_cwd = effective_worktree_path(worktree_root, source_path, source_git_root)
        .to_string_lossy()
        .to_string();
    let restore_summary = None;
    let restore_degree = if codebase_ok {
        Some(echo_agent_workspace::session::git::RestoreDegree::Full)
    } else {
        None
    };
    Ok(ResumeSessionInWorktreeResponse {
        session_id: local_session_id,
        worktree_path: wt_resp.worktree_path,
        effective_cwd,
        remote_restored: true,
        parent_session_id: req.session_id.clone(),
        chat_messages_copied: session_state_result.files_copied as usize,
        updates_copied: if session_state_result.updates_restored {
            1
        } else {
            0
        },
        code_restored: codebase_ok,
        restore_summary,
        restore_degree,
    })
}
/// Local-session resume: create worktree from source, fork session into it.
async fn resume_local_session_in_worktree(
    req: &ResumeSessionInWorktreeRequest,
    #[allow(unused_variables)] ops: &echo_agent_workspace::WorkspaceOps,
    resolved_session_id: &str,
    resolved_source_cwd: &str,
    worktree_type_default: ShellWorktreeType,
    restore_code_default: bool,
    registry_client: Option<&crate::agent::session_registry_client::SessionRegistryClient>,
    auth_manager: Option<std::sync::Arc<crate::auth::AuthManager>>,
    agent_id: &str,
) -> Result<ResumeSessionInWorktreeResponse> {
    use crate::session::fork::{ForkSessionRequest, fork_session};
    use echo_agent_workspace::session::git::effective_worktree_path;
    let worktree_type = req
        .worktree_type
        .map(ShellWorktreeType::from)
        .unwrap_or(worktree_type_default);
    let wt_resp = create_worktree_for_resume(
        resolved_source_cwd,
        req.copy_mode,
        worktree_type,
        req.git_ref.clone(),
    )
    .await?;
    tracing::info!(
        target: WORKTREE_LOG,
        restore_code = ?req.restore_code,
        restore_code_default,
        effective = req.restore_code.unwrap_or(restore_code_default),
        git_ref = req.git_ref.as_deref(),
        resolved_session_id,
        worktree_path = %wt_resp.worktree_path,
        "RESTORE_CODE_DEBUG: resume_local_session_in_worktree, about to check restore_code"
    );
    let mut decision = WorktreeRestoreDecision {
        code_restored: false,
        restore_summary: None,
        restore_degree: None,
    };
    if req.restore_code.unwrap_or(restore_code_default) {
        let is_jj = find_git_root_from_path(std::path::Path::new(resolved_source_cwd))
            .ok()
            .is_some_and(|root| echo_agent_workspace::session::git::detect_vcs_kind(&root).is_jj());
        if !is_jj {
            if echo_agent_workspace::session::git::should_warn_registry_disabled(
                is_jj,
                registry_client.is_some(),
            ) {
                echo_agent_workspace::session::git::warn_registry_disabled_restore(
                    resolved_session_id,
                );
            }
            let info = crate::session::info::Info {
                id: agent_client_protocol::SessionId::new(resolved_session_id.to_owned()),
                cwd: resolved_source_cwd.to_owned(),
            };
            let summary_path = crate::session::persistence::session_dir(&info).join("summary.json");
            let head_commit = std::fs::read_to_string(&summary_path)
                .ok()
                .and_then(|raw| {
                    serde_json::from_str::<crate::session::persistence::Summary>(&raw).ok()
                })
                .and_then(|s| s.head_commit);
            tracing::info!(
                target: WORKTREE_LOG,
                head_commit = ?head_commit,
                summary_path = %summary_path.display(),
                "RESTORE_CODE_DEBUG: loaded head_commit from summary"
            );
            let outcome = checkout_persisted_head_in_worktree(
                &wt_resp.worktree_path,
                head_commit.as_deref(),
                resolved_session_id,
            )
            .await;
            use echo_agent_workspace::session::git::RestoreKind;
            let kind = if !outcome.checked_out {
                RestoreKind::CheckoutFailed
            } else {
                match registry_client {
                    None => RestoreKind::RegistryOff,
                    Some(client) => {
                        let _ = (client, ops);
                        RestoreKind::RegistryOff
                    }
                }
            };
            decision = build_worktree_restore_outcome(head_commit.as_deref(), &outcome, kind);
        }
    }
    let WorktreeRestoreDecision {
        code_restored,
        restore_summary,
        restore_degree,
    } = decision;
    let worktree_root = std::path::Path::new(&wt_resp.worktree_path);
    let source_path = std::path::Path::new(resolved_source_cwd);
    let source_git_root = wt_resp.source_git_root.as_deref().map(std::path::Path::new);
    let effective_cwd = effective_worktree_path(worktree_root, source_path, source_git_root)
        .to_string_lossy()
        .to_string();
    let fork_req = ForkSessionRequest {
        source_session_id: resolved_session_id.to_owned(),
        source_cwd: resolved_source_cwd.to_owned(),
        new_cwd: effective_cwd.clone(),
        session_kind: Some("worktree".to_string()),
        source_workspace_dir: Some(resolved_source_cwd.to_owned()),
        ..Default::default()
    };
    let fork_resp = match fork_session(fork_req, agent_id, auth_manager).await {
        Ok(r) => r,
        Err(e) => {
            cleanup_worktree_on_failure(resolved_source_cwd, &wt_resp.worktree_path).await;
            return Err(anyhow::anyhow!("Failed to fork session into worktree: {e}"));
        }
    };
    Ok(ResumeSessionInWorktreeResponse {
        session_id: fork_resp.new_session_id,
        worktree_path: wt_resp.worktree_path,
        effective_cwd,
        remote_restored: false,
        parent_session_id: resolved_session_id.to_owned(),
        chat_messages_copied: fork_resp.chat_messages_copied,
        updates_copied: fork_resp.updates_copied,
        code_restored,
        restore_summary,
        restore_degree,
    })
}
/// Orchestrate session rehydration: recreate the git worktree at the exact
/// path and restore all session state using the original session ID.
///
pub(crate) async fn rehydrate_session_in_worktree(
    req: &RehydrateSessionRequest,
    #[allow(unused_variables)] ops: &echo_agent_workspace::WorkspaceOps,
    registry_client: Option<&crate::agent::session_registry_client::SessionRegistryClient>,
) -> Result<RehydrateSessionResponse> {
    let worktree_path_str = req.worktree_path.as_deref().unwrap_or(&req.source_cwd);
    let repo_root = Path::new(&req.repo_root);
    let worktree_path = Path::new(worktree_path_str);
    if !repo_root.exists() {
        anyhow::bail!(
            "Repository root '{}' does not exist. \
             Ensure the repo is cloned before calling rehydrate.",
            req.repo_root
        );
    }
    let session_summary_exists = crate::util::echo_agent_home::sessions_cwd_dir(&req.source_cwd)
        .join(&req.session_id)
        .join("summary.json")
        .exists();
    if worktree_path.exists() && session_summary_exists {
        tracing::info!(
            session_id = %req.session_id,
            worktree_path = %worktree_path_str,
            "rehydrate: worktree and session state already exist, skipping"
        );
        return Ok(RehydrateSessionResponse {
            session_id: req.session_id.clone(),
            worktree_path: worktree_path_str.to_string(),
            effective_cwd: req.source_cwd.clone(),
            codebase_restored: false,
            session_state_restored: false,
            memory_restored: false,
            warnings: vec![],
        });
    }
    if !worktree_path.exists() {
        tracing::info!(session_id = %req.session_id, %worktree_path_str, "rehydrate: creating worktree");
        if let Some(parent) = worktree_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let source = req.repo_root.clone();
        let dest = worktree_path_str.to_string();
        let session_id = req.session_id.clone();
        let btrfs_delegate = btrfs_delegate_from_env();
        tokio::task::spawn_blocking(move || {
            use echo_agent_fast_worktree::{
                CreationMode, IgnoredFilesMode, WorkingTreeMode, WorktreeBuilder,
            };
            let mut builder = WorktreeBuilder::new(&source, &dest)
                .working_tree_mode(WorkingTreeMode::CleanAll)
                .ignored_files_mode(IgnoredFilesMode::Skip)
                .creation_mode(CreationMode::Linked)
                .worktree_kind(echo_agent_fast_worktree::WorktreeKind::Fork)
                .session_id(session_id);
            if let Some(delegate) = btrfs_delegate {
                builder = builder.btrfs_delegate(delegate);
            }
            builder.create()
        })
        .await
        .map_err(|e| anyhow::anyhow!("worktree creation task failed: {e}"))??;
    }
    let client = registry_client.ok_or_else(|| {
        anyhow::anyhow!(
            "Session registry client is required for rehydration \
             (auth may be missing or registry is disabled)"
        )
    })?;
    let record = client
        .get_session(&req.session_id)
        .await
        .context("fetching session record for rehydration")?;
    let turn = crate::session::restore::resolve_restore_turn(&record, None);
    let memory_dl_future = crate::session::restore::download_to_tempfile(
        client,
        &req.session_id,
        "memory.tar.gz",
        turn,
    );
    let state_dl_future = async {
        Err(anyhow::anyhow!(
            "session-state archive restore unavailable in this build"
        ))
    };
    let (memory_dl, state_dl) = tokio::join!(memory_dl_future, state_dl_future);
    let _ = ops;
    let mut warnings: Vec<String> = Vec::new();
    let codebase_restored = false;
    let memory_result =
        crate::session::restore::apply_memory_download(memory_dl, &req.source_cwd).await;
    let session_state_result = crate::session::restore::apply_session_state_in_place(
        state_dl,
        &req.session_id,
        &req.source_cwd,
    )
    .await;
    if session_state_result.is_skipped() {
        warnings.push(
            "Session state archive was unavailable; conversation history not restored.".to_string(),
        );
    }
    Ok(RehydrateSessionResponse {
        session_id: req.session_id.clone(),
        worktree_path: worktree_path_str.to_string(),
        effective_cwd: req.source_cwd.clone(),
        codebase_restored,
        session_state_restored: !session_state_result.is_skipped(),
        memory_restored: memory_result.sessions_copied > 0,
        warnings,
    })
}
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serial_test::serial;
    #[test]
    #[serial]
    fn resume_grove_worktree_flag_runs_gate_fail_closed() {
        unsafe { std::env::set_var("ECHO_AGENT_WORKTREE_TYPE", "grove") };
        let flag = resume_grove_worktree_flag();
        unsafe { std::env::remove_var("ECHO_AGENT_WORKTREE_TYPE") };
        assert_eq!(
            flag,
            Some(false),
            "resume must call the grove gate; remote=None is fail-closed even when env asked for grove"
        );
    }
    #[test]
    fn resume_request_deserializes_with_defaults() {
        let json = r#"{"sessionId":"s1","sourceCwd":"/project"}"#;
        let req: ResumeSessionInWorktreeRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.session_id, "s1");
        assert_eq!(req.source_cwd, "/project");
        assert!(matches!(req.copy_mode, WorktreeCopyMode::Dirty));
        assert!(req.worktree_type.is_none());
        assert!(req.git_ref.is_none());
    }
    #[test]
    fn resume_request_deserializes_explicit_fields() {
        let json = r#"{
            "sessionId": "abc",
            "sourceCwd": "/work",
            "copyMode": "clean",
            "worktreeType": "standalone",
            "gitRef": "main"
        }"#;
        let req: ResumeSessionInWorktreeRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.session_id, "abc");
        assert!(matches!(req.copy_mode, WorktreeCopyMode::Clean));
        assert_eq!(req.worktree_type, Some(WorktreeType::Standalone));
        assert_eq!(req.git_ref.as_deref(), Some("main"));
    }
    #[test]
    fn resume_response_round_trips() {
        let resp = ResumeSessionInWorktreeResponse {
            session_id: "forked-id".into(),
            worktree_path: "/wt/root".into(),
            effective_cwd: "/wt/root/sub".into(),
            remote_restored: true,
            parent_session_id: "original-id".into(),
            chat_messages_copied: 42,
            updates_copied: 100,
            code_restored: true,
            restore_summary: Some(
                "checked out abc12345, staged: true, unstaged: false, untracked: 3".into(),
            ),
            restore_degree: Some(echo_agent_workspace::session::git::RestoreDegree::Full),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let deser: ResumeSessionInWorktreeResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.session_id, "forked-id");
        assert_eq!(deser.effective_cwd, "/wt/root/sub");
        assert!(deser.remote_restored);
        assert_eq!(deser.parent_session_id, "original-id");
        assert_eq!(deser.chat_messages_copied, 42);
        assert_eq!(deser.updates_copied, 100);
        assert!(deser.code_restored);
        assert_eq!(
            deser.restore_summary.as_deref(),
            Some("checked out abc12345, staged: true, unstaged: false, untracked: 3")
        );
        assert_eq!(
            deser.restore_degree,
            Some(echo_agent_workspace::session::git::RestoreDegree::Full)
        );
    }
    fn ck_outcome(
        checked_out: bool,
        stash_ref: Option<&str>,
        skipped: Option<&str>,
    ) -> echo_agent_workspace::session::git::CheckoutSessionOutcome {
        echo_agent_workspace::session::git::CheckoutSessionOutcome {
            checked_out,
            stash_ref: stash_ref.map(str::to_owned),
            stash_skipped_reason: skipped.map(str::to_owned),
        }
    }
    /// When checkout failed AND stash was skipped (e.g. in-progress
    /// merge), the meta still surfaces the skipped reason rather than
    /// going silent.
    #[test]
    fn worktree_restore_outcome_checkout_failed_surfaces_stash_skipped_reason() {
        use echo_agent_workspace::session::git::RestoreKind;
        let d = build_worktree_restore_outcome(
            Some("0123456789abcdef"),
            &ck_outcome(false, None, Some("MERGE_HEAD present")),
            RestoreKind::RegistryOff,
        );
        assert!(!d.code_restored);
        assert!(d.restore_degree.is_none());
        let s = d.restore_summary.unwrap();
        assert!(s.contains("restore aborted"));
        assert!(s.contains("; stash skipped: MERGE_HEAD present"));
    }
    #[test]
    fn worktree_restore_outcome_surfaces_stash_skipped_reason_on_success() {
        use echo_agent_workspace::session::git::RestoreKind;
        let d = build_worktree_restore_outcome(
            Some("0123456789abcdef"),
            &ck_outcome(true, None, Some("MERGE_HEAD present")),
            RestoreKind::RegistryOff,
        );
        let s = d.restore_summary.unwrap();
        assert!(s.contains("; stash skipped: MERGE_HEAD present"));
    }
    #[test]
    fn resume_response_serializes_degree_when_set() {
        let resp = ResumeSessionInWorktreeResponse {
            session_id: "s".into(),
            worktree_path: "w".into(),
            effective_cwd: "e".into(),
            remote_restored: false,
            parent_session_id: "p".into(),
            chat_messages_copied: 0,
            updates_copied: 0,
            code_restored: true,
            restore_summary: Some("checked out abc".into()),
            restore_degree: Some(echo_agent_workspace::session::git::RestoreDegree::HeadOnly),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"restoreDegree\":\"head_only\""));
        assert!(json.contains("\"restoreSummary\":\"checked out abc\""));
        let deser: ResumeSessionInWorktreeResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deser.restore_degree,
            Some(echo_agent_workspace::session::git::RestoreDegree::HeadOnly)
        );
        assert_eq!(deser.restore_summary.as_deref(), Some("checked out abc"));
    }
    /// Unknown degree strings must fail deserialisation rather than
    /// silently round-tripping as a typo.
    #[test]
    fn resume_response_rejects_unknown_degree_string() {
        let json = r#"{
            "sessionId": "s",
            "worktreePath": "w",
            "effectiveCwd": "e",
            "remoteRestored": false,
            "parentSessionId": "p",
            "chatMessagesCopied": 0,
            "updatesCopied": 0,
            "codeRestored": true,
            "restoreDegree": "full_"
        }"#;
        let r: Result<ResumeSessionInWorktreeResponse, _> = serde_json::from_str(json);
        assert!(r.is_err(), "typo \"full_\" must fail to deserialise");
    }
    #[test]
    fn resume_response_camel_case_keys() {
        let resp = ResumeSessionInWorktreeResponse {
            session_id: "s".into(),
            worktree_path: "w".into(),
            effective_cwd: "e".into(),
            remote_restored: false,
            parent_session_id: "p".into(),
            chat_messages_copied: 0,
            updates_copied: 0,
            code_restored: false,
            restore_summary: None,
            restore_degree: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("sessionId"));
        assert!(json.contains("worktreePath"));
        assert!(json.contains("effectiveCwd"));
        assert!(json.contains("remoteRestored"));
        assert!(json.contains("parentSessionId"));
        assert!(json.contains("chatMessagesCopied"));
        assert!(json.contains("updatesCopied"));
        assert!(json.contains("codeRestored"));
        assert!(!json.contains("restoreSummary"));
        assert!(!json.contains("restoreDegree"));
        assert!(!json.contains("session_id"));
        assert!(!json.contains("worktree_path"));
    }
    #[test]
    fn test_dirty_summary_serialization() {
        let summary = DirtyStateSummary {
            staged_count: 3,
            modified_count: 5,
            deleted_count: 1,
            untracked_count: 12,
            has_partially_staged: true,
            skipped_dirs: vec!["node_modules".to_string(), "target".to_string()],
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert!(json.contains("\"stagedCount\":3"));
        assert!(json.contains("\"modifiedCount\":5"));
        assert!(json.contains("\"hasPartiallyStaged\":true"));
        assert!(json.contains("\"skippedDirs\":["));
    }
    #[test]
    fn test_copy_mode_default_is_dirty() {
        let mode: WorktreeCopyMode = Default::default();
        assert_eq!(mode, WorktreeCopyMode::Dirty);
    }
    #[test]
    fn test_copy_mode_deserialization() {
        let clean: WorktreeCopyMode = serde_json::from_str("\"clean\"").unwrap();
        assert_eq!(clean, WorktreeCopyMode::Clean);
        let dirty: WorktreeCopyMode = serde_json::from_str("\"dirty\"").unwrap();
        assert_eq!(dirty, WorktreeCopyMode::Dirty);
    }
    #[test]
    fn test_created_status_without_copied_changes() {
        let status = WorktreeStatus::Created {
            session_id: "test-123".to_string(),
            worktree_path: "/path/to/worktree".to_string(),
            commit: "abc123".to_string(),
            source_git_root: None,
            copied_changes: None,
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(!json.contains("copiedChanges"));
        assert!(json.contains("\"status\":\"created\""));
        assert!(json.contains("\"sessionId\":\"test-123\""));
    }
    #[test]
    fn test_created_status_with_copied_changes() {
        let status = WorktreeStatus::Created {
            session_id: "test-123".to_string(),
            worktree_path: "/path/to/worktree".to_string(),
            commit: "abc123".to_string(),
            source_git_root: Some("/path/to/source".to_string()),
            copied_changes: Some(CopiedChangesSummary {
                staged_copied: 3,
                modified_copied: 5,
                untracked_copied: 12,
                deletions_applied: 1,
                warnings: vec![],
            }),
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("copiedChanges"));
        assert!(json.contains("\"stagedCopied\":3"));
    }
    #[test]
    fn remove_request_legacy_worktree_path_only() {
        let json = r#"{"worktreePath": "/path/to/wt", "force": true}"#;
        let req: RemoveWorktreeRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.worktree_path.as_deref(), Some("/path/to/wt"));
        assert!(req.id_or_path.is_none());
        assert!(req.force);
        assert!(!req.dry_run);
    }
    #[test]
    fn remove_request_new_id_or_path_only() {
        let json = r#"{"idOrPath": "wt-abc123", "dryRun": true}"#;
        let req: RemoveWorktreeRequest = serde_json::from_str(json).unwrap();
        assert!(req.worktree_path.is_none());
        assert_eq!(req.id_or_path.as_deref(), Some("wt-abc123"));
        assert!(!req.force);
        assert!(req.dry_run);
    }
    #[test]
    fn remove_request_both_fields_deserializes_but_handler_rejects() {
        let json = r#"{"worktreePath": "/explicit", "idOrPath": "fallback"}"#;
        let req: RemoveWorktreeRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.worktree_path.as_deref(), Some("/explicit"));
        assert_eq!(req.id_or_path.as_deref(), Some("fallback"));
    }
    #[test]
    fn remove_response_omits_resolved_path_when_none() {
        let resp = RemoveWorktreeResponse {
            removed: true,
            resolved_path: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(!json.contains("resolvedPath"));
    }
    #[test]
    fn remove_response_includes_resolved_path_when_present() {
        let resp = RemoveWorktreeResponse {
            removed: true,
            resolved_path: Some("/resolved/path".into()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"resolvedPath\":\"/resolved/path\""));
    }
    #[test]
    #[serial]
    fn resolve_worktree_by_id_or_path_nonexistent_returns_none() {
        let result = resolve_worktree_by_id_or_path("/nonexistent/path/xyz123").unwrap();
        assert!(result.is_none());
    }
    #[test]
    #[serial]
    fn resolve_worktree_by_id_or_path_existing_dir_returns_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().to_str().unwrap();
        let result = resolve_worktree_by_id_or_path(path).unwrap();
        assert_eq!(result.unwrap(), tmp.path());
    }
    fn make_wt_record(path: &str, source_repo: &str) -> echo_agent_fast_worktree::WorktreeRecord {
        echo_agent_fast_worktree::WorktreeRecord {
            id: format!("wt-{}", path.replace('/', "-")),
            path: std::path::PathBuf::from(path),
            source_repo: std::path::PathBuf::from(source_repo),
            repo_name: "repo".into(),
            kind: echo_agent_fast_worktree::WorktreeKind::Session,
            creation_mode: "linked".into(),
            git_ref: None,
            head_commit: None,
            session_id: None,
            creator_pid: None,
            created_at: 0,
            last_accessed_at: None,
            status: echo_agent_fast_worktree::WorktreeStatus::Alive,
            metadata: None,
        }
    }
    #[test]
    fn build_candidate_list_cwd_equals_main_root() {
        let result = build_candidate_list("/repo/main", "/repo/main", &[], &[]);
        assert_eq!(result, vec!["/repo/main"]);
    }
    #[test]
    fn build_candidate_list_cwd_differs_from_main_root() {
        let result = build_candidate_list("/repo/wt-1", "/repo/main", &[], &[]);
        assert_eq!(result, vec!["/repo/wt-1", "/repo/main"]);
    }
    #[test]
    fn build_candidate_list_includes_db_records_sorted() {
        let records = vec![
            make_wt_record("/repo/wt-c", "/repo/main"),
            make_wt_record("/repo/wt-a", "/repo/main"),
            make_wt_record("/repo/wt-b", "/repo/main"),
        ];
        let result = build_candidate_list("/repo/main", "/repo/main", &records, &[]);
        assert_eq!(
            result,
            vec!["/repo/main", "/repo/wt-a", "/repo/wt-b", "/repo/wt-c"]
        );
    }
    #[test]
    fn build_candidate_list_dedupes_cwd_and_main_root() {
        let records = vec![
            make_wt_record("/repo/main", "/repo/main"),
            make_wt_record("/repo/wt-1", "/repo/main"),
            make_wt_record("/repo/wt-1", "/repo/main"),
        ];
        let result = build_candidate_list("/repo/wt-1", "/repo/main", &records, &[]);
        assert_eq!(result, vec!["/repo/wt-1", "/repo/main"]);
    }
    #[test]
    fn repo_wide_resolve_finds_session_in_sibling_cwd_skipping_remote() {
        use crate::session::persistence::LocalSessionResolutionKind;
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let exact_cwd = "/project/main";
        let sibling_cwd = "/project/worktree-1";
        let encoded = crate::util::echo_agent_home::encode_cwd_dirname(sibling_cwd);
        let session_dir = root.join(&encoded).join("sess-remote-123");
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(session_dir.join("summary.json"), b"{}").unwrap();
        let candidates: &[&str] = &[exact_cwd, sibling_cwd];
        let result = crate::session::persistence::resolve_local_session_for_repo_in_root(
            "sess-remote-123",
            candidates,
            root,
        );
        let resolved = result.expect("should find session in sibling cwd");
        assert_eq!(resolved.session_id, "sess-remote-123");
        assert_eq!(resolved.cwd, sibling_cwd);
        assert_eq!(
            resolved.resolution_kind,
            LocalSessionResolutionKind::SameRepoDifferentCwd
        );
    }
    #[test]
    fn remote_worktree_codebase_follows_request_then_default() {
        assert!(!remote_worktree_restores_codebase(Some(false), true));
        assert!(!remote_worktree_restores_codebase(None, false));
        assert!(remote_worktree_restores_codebase(None, true));
        assert!(remote_worktree_restores_codebase(Some(true), false));
    }
    #[tokio::test]
    async fn resume_in_worktree_falls_through_to_remote_when_not_found_locally() {
        let req = ResumeSessionInWorktreeRequest {
            session_id: "nonexistent-session-id".to_string(),
            source_cwd: "/tmp/definitely-not-a-repo".to_string(),
            copy_mode: WorktreeCopyMode::Dirty,
            worktree_type: None,
            restore_code: Some(true),
            git_ref: None,
        };
        let ops = echo_agent_workspace::WorkspaceOps::for_test();
        let result = resume_session_in_worktree(
            &req,
            &ops,
            ShellWorktreeType::Linked,
            false,
            None,
            None,
            "test-agent",
        )
        .await;
        let err = result.expect_err("should fail when session not found and no registry");
        let msg = err.to_string();
        assert!(
            msg.contains("not found locally") && msg.contains("registry"),
            "expected registry-unavailable error, got: {msg}"
        );
    }
    /// Test helper: Initialize a git repo at the given path
    fn init_git_repo(path: &std::path::Path) {
        crate::test_support::ensure_hermetic_git_on_path();
        std::process::Command::new("git")
            .current_dir(path)
            .args(["init"])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .current_dir(path)
            .args(["config", "user.email", "test@test.com"])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .current_dir(path)
            .args(["config", "user.name", "Test"])
            .output()
            .unwrap();
    }
    /// Test helper: Stage and commit all files
    fn git_commit_all(path: &std::path::Path, message: &str) {
        std::process::Command::new("git")
            .current_dir(path)
            .args(["add", "."])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .current_dir(path)
            .args(["commit", "-m", message])
            .output()
            .unwrap();
    }
    #[tokio::test]
    async fn create_worktree_for_resume_produces_independent_worktree() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_path = tmp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        init_git_repo(&repo_path);
        std::fs::write(repo_path.join("file.txt"), "original").unwrap();
        git_commit_all(&repo_path, "initial");
        let source_cwd = repo_path.to_string_lossy().to_string();
        let wt_resp = create_worktree_for_resume(
            &source_cwd,
            WorktreeCopyMode::Clean,
            ShellWorktreeType::Linked,
            None,
        )
        .await
        .expect("worktree creation should succeed");
        let wt_path = std::path::Path::new(&wt_resp.worktree_path);
        assert!(wt_path.exists(), "worktree directory should exist");
        std::fs::write(wt_path.join("worktree_only.txt"), "from worktree").unwrap();
        assert!(
            !repo_path.join("worktree_only.txt").exists(),
            "file written in worktree must not appear in source repo",
        );
        assert!(wt_path.join("file.txt").exists());
        assert!(repo_path.join("file.txt").exists());
    }
    #[tokio::test]
    async fn create_worktree_for_resume_honors_git_ref() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_path = tmp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        init_git_repo(&repo_path);
        std::process::Command::new("git")
            .current_dir(&repo_path)
            .args(["checkout", "-b", "main"])
            .output()
            .unwrap();
        std::fs::write(repo_path.join("file.txt"), "on-main").unwrap();
        git_commit_all(&repo_path, "initial");
        std::process::Command::new("git")
            .current_dir(&repo_path)
            .args(["checkout", "-b", "feature"])
            .output()
            .unwrap();
        std::fs::write(repo_path.join("file.txt"), "on-feature").unwrap();
        git_commit_all(&repo_path, "feature commit");
        std::fs::write(repo_path.join("dirty.txt"), "uncommitted").unwrap();
        let source_cwd = repo_path.to_string_lossy().to_string();
        let wt_resp = create_worktree_for_resume(
            &source_cwd,
            WorktreeCopyMode::Dirty,
            ShellWorktreeType::Linked,
            Some("main".into()),
        )
        .await
        .expect("worktree creation with git_ref should succeed");
        let wt_path = std::path::Path::new(&wt_resp.worktree_path);
        let contents = std::fs::read_to_string(wt_path.join("file.txt")).unwrap();
        assert_eq!(
            contents, "on-main",
            "worktree should be checked out at the requested ref"
        );
        assert!(
            !wt_path.join("dirty.txt").exists(),
            "dirty overlay must not apply when git_ref is set"
        );
    }
    #[tokio::test]
    async fn cleanup_worktree_on_failure_removes_created_worktree() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_path = tmp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        init_git_repo(&repo_path);
        std::fs::write(repo_path.join("file.txt"), "original").unwrap();
        git_commit_all(&repo_path, "initial");
        let source_cwd = repo_path.to_string_lossy().to_string();
        let wt_resp = create_worktree_for_resume(
            &source_cwd,
            WorktreeCopyMode::Clean,
            ShellWorktreeType::Linked,
            None,
        )
        .await
        .expect("worktree creation should succeed");
        let wt_path = std::path::Path::new(&wt_resp.worktree_path);
        assert!(wt_path.exists(), "worktree should exist before cleanup");
        cleanup_worktree_on_failure(&source_cwd, &wt_resp.worktree_path).await;
        assert!(
            !wt_path.exists(),
            "worktree directory should be removed after cleanup",
        );
        assert!(repo_path.join("file.txt").exists());
    }
    #[test]
    fn worktree_base_dir_extracts_repo_name() {
        let base = worktree_base_dir(Path::new("/home/user/projects/my-repo"));
        assert!(base.ends_with("worktrees/projects-my-repo"));
    }
    /// Helper: get HEAD commit SHA from a git repo.
    fn git_head_sha(path: &std::path::Path) -> String {
        let out = std::process::Command::new("git")
            .current_dir(path)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }
    #[tokio::test]
    async fn checkout_persisted_head_checks_out_older_commit() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_path = tmp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        init_git_repo(&repo_path);
        std::fs::write(repo_path.join("a.txt"), "first").unwrap();
        git_commit_all(&repo_path, "first commit");
        let first_sha = git_head_sha(&repo_path);
        std::fs::write(repo_path.join("b.txt"), "second").unwrap();
        git_commit_all(&repo_path, "second commit");
        let second_sha = git_head_sha(&repo_path);
        assert_ne!(first_sha, second_sha);
        let wt_path = tmp.path().join("wt");
        std::process::Command::new("git")
            .current_dir(&repo_path)
            .args(["worktree", "add", "--detach", wt_path.to_str().unwrap()])
            .output()
            .unwrap();
        assert_eq!(git_head_sha(&wt_path), second_sha);
        let wt_str = wt_path.to_string_lossy().to_string();
        let result =
            checkout_persisted_head_in_worktree(&wt_str, Some(&first_sha), "sess-ck").await;
        assert!(result.checked_out, "should have performed checkout");
        assert_eq!(git_head_sha(&wt_path), first_sha);
    }
    #[tokio::test]
    async fn checkout_persisted_head_noop_when_none() {
        let outcome = checkout_persisted_head_in_worktree("/tmp/irrelevant", None, "sess").await;
        assert!(!outcome.checked_out, "None head_commit should be a no-op",);
    }
    #[tokio::test]
    async fn checkout_persisted_head_noop_on_empty_string() {
        let outcome =
            checkout_persisted_head_in_worktree("/tmp/irrelevant", Some(""), "sess").await;
        assert!(
            !outcome.checked_out,
            "empty string head_commit should be a no-op",
        );
    }
    /// Integration: a worktree with seeded dirty state must surface a
    /// stash ref AND end up clean after `checkout_persisted_head_in_worktree`.
    /// Mirrors `copy_mode: dirty` worktree creation where the worktree
    /// inherits the source's uncommitted changes.
    #[tokio::test]
    async fn checkout_persisted_head_stashes_dirty_worktree_state() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_path = tmp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        init_git_repo(&repo_path);
        std::fs::write(repo_path.join("a.txt"), "first").unwrap();
        git_commit_all(&repo_path, "first commit");
        let first_sha = git_head_sha(&repo_path);
        std::fs::write(repo_path.join("b.txt"), "second").unwrap();
        git_commit_all(&repo_path, "second commit");
        let wt_path = tmp.path().join("wt");
        std::process::Command::new("git")
            .current_dir(&repo_path)
            .args(["worktree", "add", "--detach", wt_path.to_str().unwrap()])
            .output()
            .unwrap();
        std::fs::write(wt_path.join("a.txt"), "dirty mod").unwrap();
        std::fs::write(wt_path.join("untracked.txt"), "new").unwrap();
        let wt_str = wt_path.to_string_lossy().to_string();
        let outcome =
            checkout_persisted_head_in_worktree(&wt_str, Some(&first_sha), "sess-dirty-wt").await;
        assert!(outcome.checked_out);
        assert!(
            outcome.stash_ref.is_some(),
            "dirty worktree must produce stash ref"
        );
        assert!(outcome.stash_skipped_reason.is_none());
        let porcelain = std::process::Command::new("git")
            .current_dir(&wt_path)
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&porcelain.stdout).trim().is_empty(),
            "working tree should be clean after stash"
        );
        assert!(!wt_path.join("untracked.txt").exists());
        let stash_list = std::process::Command::new("git")
            .current_dir(&wt_path)
            .args(["stash", "list"])
            .output()
            .unwrap();
        let list_out = String::from_utf8_lossy(&stash_list.stdout).into_owned();
        assert!(
            list_out.contains("echoagent: pre-restore-code sess-dirty-wt"),
            "stash list missing session label: {list_out}"
        );
    }
    #[test]
    fn test_background_copy_guard_registers_and_unregisters() {
        let context = BackgroundCopyContext::new();
        let worktree_path = "/test/worktree/guard-test".to_string();
        let cancellation_token = tokio_util::sync::CancellationToken::new();
        {
            let _guard = BackgroundCopyGuard::new(
                context.clone(),
                worktree_path.clone(),
                cancellation_token.clone(),
            );
        }
        let was_cancelled = context.cancel(&worktree_path);
        assert!(!was_cancelled);
    }
    #[test]
    fn test_background_copy_context_cancel_via_context() {
        let context = BackgroundCopyContext::new();
        let worktree_path = "/test/worktree/cancel-test".to_string();
        let cancellation_token = tokio_util::sync::CancellationToken::new();
        let _guard = BackgroundCopyGuard::new(
            context.clone(),
            worktree_path.clone(),
            cancellation_token.clone(),
        );
        assert!(!cancellation_token.is_cancelled());
        let was_cancelled = context.cancel(&worktree_path);
        assert!(was_cancelled);
        assert!(cancellation_token.is_cancelled());
    }
}
