//! Handler for echo.agent/git/worktree/* extension methods.

use agent_client_protocol as acp;
use echo_agent_acp::AcpAgentGatewaySender as GatewaySender;

use crate::agent::mvp_agent::MvpAgent;
use crate::session::ExtMethodResult;
use crate::session::persistence::LocalSessionResolutionKind;
use crate::session::worktree::{
    ApplyWorktreeRequest, CreateWorktreeFromWorktreeRequest, CreateWorktreeRequest,
    CreateWorktreeResponse, RehydrateSessionRequest, RemoveWorktreeRequest,
    ResumeSessionInWorktreeRequest, WorktreeNotificationSender, WorktreeStatus, WorktreeType,
    create_jj_workspace, create_worktree_async, create_worktree_from_worktree_async,
    rehydrate_session_in_worktree, resolve_session_repo_wide, resume_session_in_worktree,
};

type ExtResult = Result<acp::ExtResponse, acp::Error>;

const WORKTREE_EXT_LOG: &str = "echo_agent_worktree";

/// Wrapper to send worktree progress notifications via gateway.
#[derive(Clone)]
struct GatewayWorktreeNotifier {
    gateway: GatewaySender,
}

#[async_trait::async_trait]
impl WorktreeNotificationSender for GatewayWorktreeNotifier {
    async fn send_worktree_status(&self, progress: WorktreeStatus) {
        let params = match serde_json::value::to_raw_value(&progress) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("Failed to serialize worktree progress: {}", e);
                return;
            }
        };
        let notification =
            acp::ExtNotification::new("echo.agent/git/worktree/status", params.into());
        if let Err(e) = self.gateway.send(notification).await {
            tracing::warn!("Failed to send worktree progress notification: {}", e);
        }
    }
}

fn to_response<T: serde::Serialize>(result: anyhow::Result<T>) -> ExtResult {
    ExtMethodResult::from_result(result)
        .to_ext_response()
        .map_err(|e| acp::Error::internal_error().data(e.to_string()))
}

/// Extract the worktree path from a `Creating` response for pinning.
fn extract_creating_path(resp: &anyhow::Result<CreateWorktreeResponse>) -> Option<String> {
    if let Ok(CreateWorktreeResponse::Creating { worktree_path, .. }) = resp {
        Some(worktree_path.clone())
    } else {
        None
    }
}

// ── ACP request types for worktree management ──────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListWorktreeRequest {
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub r#type: Vec<String>,
    #[serde(default)]
    pub include_all: bool,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ShowWorktreeRequest {
    pub id_or_path: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GcWorktreeRequest {
    #[serde(default)]
    pub dry_run: bool,
    /// Duration string like "7d", "24h", "30m", "60s".
    #[serde(default)]
    pub max_age: Option<String>,
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeDbPathResponse {
    pub path: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResolveLocalForWorktreeResumeRequest {
    pub session_id: String,
    pub cwd: String,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResolveLocalForWorktreeResumeResponse {
    pub found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution_kind: Option<LocalSessionResolutionKind>,
}
fn parse_duration(s: &str) -> Result<i64, acp::Error> {
    let s = s.trim();
    let (num, mult) = if let Some(n) = s.strip_suffix('d') {
        (n, 86400i64)
    } else if let Some(n) = s.strip_suffix('h') {
        (n, 3600)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1)
    } else {
        return Err(acp::Error::invalid_params().data(format!(
            "invalid duration: {s} (expected e.g. 7d, 24h, 30m, 60s)"
        )));
    };
    num.parse::<i64>()
        .map(|v| v * mult)
        .map_err(|_| acp::Error::invalid_params().data(format!("invalid number in duration: {s}")))
}

fn log_effective_worktree_type(
    method: &str,
    request_worktree_type: Option<WorktreeType>,
    agent_default_worktree_type: crate::util::config::WorktreeType,
    effective_worktree_type: WorktreeType,
) {
    tracing::info!(
        target: WORKTREE_EXT_LOG,
        method,
        request_worktree_type = ?request_worktree_type,
        agent_default_worktree_type = ?agent_default_worktree_type,
        effective_worktree_type = ?effective_worktree_type,
        "WORKTREE_REQUEST_SHELL: resolved effective worktree type"
    );
}
pub async fn handle(
    agent: &MvpAgent,
    ops: &echo_agent_workspace::WorkspaceOps,
    args: &acp::ExtRequest,
) -> ExtResult {
    let worktree_type_default = agent.worktree_type;
    let restore_code_default = agent.restore_code;

    match args.method.as_ref() {
        "echo.agent/git/worktree/create" => {
            let mut req = serde_json::from_str::<CreateWorktreeRequest>(args.params.get())?;
            // Pre-dispatch: apply worktree_type default
            let request_worktree_type = req.worktree_type;
            if req.worktree_type.is_none() {
                req.worktree_type = Some(worktree_type_default.into());
            }
            apply_grove_worktree_flag(agent, &mut req.grove_worktree);
            log_effective_worktree_type(
                "echo.agent/git/worktree/create",
                request_worktree_type,
                worktree_type_default,
                req.worktree_type.unwrap_or(worktree_type_default.into()),
            );
            let result = ops
                .dispatch(&req, None)
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
            // Post-dispatch: spawn async task for Creating variant.
            if let Ok(resp) = serde_json::from_value::<CreateWorktreeResponse>(result.clone())
                && matches!(&resp, CreateWorktreeResponse::Creating { .. })
            {
                req.worktree_path = extract_creating_path(&Ok(resp));
                let notifier = GatewayWorktreeNotifier {
                    gateway: agent.gateway.clone(),
                };
                let copy_context = agent.background_copy_context();
                tokio::task::spawn_local(async move {
                    create_worktree_async(req, notifier, copy_context).await;
                });
            }
            to_response(Ok(result))
        }
        "echo.agent/git/worktree/remove" => {
            let req = serde_json::from_str::<RemoveWorktreeRequest>(args.params.get())?;
            let result = ops
                .dispatch(&req, None)
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
            to_response(Ok(result))
        }
        "echo.agent/git/worktree/apply" => {
            let req = serde_json::from_str::<ApplyWorktreeRequest>(args.params.get())?;
            let result = ops
                .dispatch(&req, None)
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
            to_response(Ok(result))
        }
        // Create a worktree from an existing worktree (used during session fork)
        "echo.agent/git/worktree/create_from_worktree" => {
            let mut req =
                serde_json::from_str::<CreateWorktreeFromWorktreeRequest>(args.params.get())?;
            let request_worktree_type = req.worktree_type;
            // Apply default if not explicitly set in request
            if req.worktree_type.is_none() {
                req.worktree_type = Some(worktree_type_default.into());
            }
            apply_grove_worktree_flag(agent, &mut req.grove_worktree);
            log_effective_worktree_type(
                "echo.agent/git/worktree/create_from_worktree",
                request_worktree_type,
                worktree_type_default,
                req.worktree_type.unwrap_or(worktree_type_default.into()),
            );
            // Dispatch prepare through workspace
            let result = ops
                .dispatch(
                    &echo_agent_workspace::workspace_ops::PrepareWorktreeFromWorktreeReq {
                        inner: req.clone(),
                    },
                    None,
                )
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;

            // Convert the serialized response back
            if let Some(err) = result.error {
                return Err(acp::Error::internal_error().data(err));
            }
            let response_value = result.response.unwrap_or(serde_json::Value::Null);
            let response: CreateWorktreeResponse =
                serde_json::from_value(response_value).map_err(|e| {
                    acp::Error::internal_error()
                        .data(format!("failed to deserialize response: {e}"))
                })?;

            if result.spawn_task {
                // Pin the resolved path so the async task reuses it instead of
                // generating a new UUID via auto_label().
                req.resolved_dest_path = extract_creating_path(&Ok(response.clone()));
                let notifier = GatewayWorktreeNotifier {
                    gateway: agent.gateway.clone(),
                };
                tokio::task::spawn_local(async move {
                    create_worktree_from_worktree_async(req, notifier).await;
                });
            }

            to_response(Ok(response))
        }
        // Synchronous variant - waits for worktree creation to complete
        "echo.agent/git/worktree/create_from_worktree_sync" => {
            let mut req =
                serde_json::from_str::<CreateWorktreeFromWorktreeRequest>(args.params.get())?;

            // For jj repos, use jj workspace add instead of git worktree
            let source_path = std::path::Path::new(&req.source_worktree_path);
            let resolved_root = ops
                .dispatch(
                    &echo_agent_workspace::workspace_ops::GitResolveRootReq {
                        cwd: source_path.to_path_buf(),
                    },
                    None,
                )
                .await
                .ok()
                .flatten();
            if let Some(git_root) = resolved_root {
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
                    tracing::info!("using jj workspace for subagent isolation");
                    return to_response(create_jj_workspace(&req).await);
                }
            }

            let request_worktree_type = req.worktree_type;
            // Apply default if not explicitly set in request
            if req.worktree_type.is_none() {
                req.worktree_type = Some(worktree_type_default.into());
            }
            apply_grove_worktree_flag(agent, &mut req.grove_worktree);
            log_effective_worktree_type(
                "echo.agent/git/worktree/create_from_worktree_sync",
                request_worktree_type,
                worktree_type_default,
                req.worktree_type.unwrap_or(worktree_type_default.into()),
            );
            let result = ops
                .dispatch(
                    &echo_agent_workspace::workspace_ops::CreateWorktreeFromWorktreeSyncReq {
                        inner: req.into_wire(),
                    },
                    None,
                )
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
            to_response(Ok(result))
        }
        // Resume a session in a fresh worktree.
        "echo.agent/git/worktree/resume_session" => {
            let req = serde_json::from_str::<ResumeSessionInWorktreeRequest>(args.params.get())?;
            log_effective_worktree_type(
                "echo.agent/git/worktree/resume_session",
                req.worktree_type,
                worktree_type_default,
                req.worktree_type.unwrap_or(worktree_type_default.into()),
            );
            let registry_client = agent.session_registry_client();
            let agent_id = echo_agent_telemetry::id::agent_id();

            to_response(
                resume_session_in_worktree(
                    &req,
                    ops,
                    worktree_type_default,
                    restore_code_default,
                    registry_client.as_ref(),
                    Some(agent.auth_manager.clone()),
                    &agent_id,
                )
                .await,
            )
        }
        // ── Repo-wide session resolution ─────────────────────────────────
        "echo.agent/session/resolve_local_for_worktree_resume" => {
            let req =
                serde_json::from_str::<ResolveLocalForWorktreeResumeRequest>(args.params.get())?;
            let result = resolve_session_repo_wide(&req.session_id, std::path::Path::new(&req.cwd));
            match result {
                Ok(Some(resolved)) => to_response(Ok(ResolveLocalForWorktreeResumeResponse {
                    found: true,
                    resolved_session_id: Some(resolved.session_id),
                    resolved_cwd: Some(resolved.cwd),
                    resolution_kind: Some(resolved.resolution_kind),
                })),
                Ok(None) => to_response(Ok(ResolveLocalForWorktreeResumeResponse {
                    found: false,
                    resolved_session_id: None,
                    resolved_cwd: None,
                    resolution_kind: None,
                })),
                Err(e) => {
                    Err(acp::Error::internal_error()
                        .data(format!("repo-wide resolution failed: {e}")))
                }
            }
        }
        // ── Session rehydration (devbox recovery) ─────────────────────────
        "echo.agent/session/rehydrate" => {
            let req = serde_json::from_str::<RehydrateSessionRequest>(args.params.get())?;
            let registry_client = agent.session_registry_client();

            to_response(rehydrate_session_in_worktree(&req, ops, registry_client.as_ref()).await)
        }
        // ── Worktree management methods ──────────────────────────────────
        "echo.agent/git/worktree/list" => {
            let req: echo_agent_workspace::workspace_ops::WorktreeListReq =
                serde_json::from_str(args.params.get())
                    .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
            let result = ops
                .dispatch(&req, None)
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
            to_response(Ok(result))
        }
        "echo.agent/git/worktree/show" => {
            let req = serde_json::from_str::<ShowWorktreeRequest>(args.params.get())?;
            let op = echo_agent_workspace::workspace_ops::WorktreeShowReq {
                id_or_path: req.id_or_path,
            };
            let result = ops
                .dispatch(&op, None)
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
            to_response(Ok(result))
        }
        "echo.agent/git/worktree/gc" => {
            let req = serde_json::from_str::<GcWorktreeRequest>(args.params.get())?;
            let max_age_secs = req.max_age.as_deref().map(parse_duration).transpose()?;
            let op = echo_agent_workspace::workspace_ops::WorktreeGcReq {
                dry_run: req.dry_run,
                max_age_secs,
                force: req.force,
            };
            let result = ops
                .dispatch(&op, None)
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
            to_response(Ok(result))
        }
        "echo.agent/git/worktree/db/stats" => {
            let result = ops
                .dispatch(
                    &echo_agent_workspace::workspace_ops::WorktreeDbStatsReq {},
                    None,
                )
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
            to_response(Ok(result))
        }
        "echo.agent/git/worktree/db/rebuild" => {
            let result = ops
                .dispatch(
                    &echo_agent_workspace::workspace_ops::WorktreeDbRebuildReq {},
                    None,
                )
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
            to_response(Ok(result))
        }
        "echo.agent/git/worktree/db/path" => {
            let result = ops
                .dispatch(
                    &echo_agent_workspace::workspace_ops::WorktreeDbPathReq {},
                    None,
                )
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
            to_response(Ok(result))
        }
        "echo.agent/git/worktree/detach" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct DetachReq {
                id_or_path: String,
                #[serde(default)]
                allow_copy: bool,
            }
            let req = serde_json::from_str::<DetachReq>(args.params.get())?;
            let result = ops
                .dispatch(
                    &echo_agent_workspace::workspace_ops::WorktreeDetachReq {
                        id_or_path: req.id_or_path,
                        allow_copy: req.allow_copy,
                    },
                    None,
                )
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
            to_response(Ok(result))
        }
        "echo.agent/git/worktree/salvage" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct SalvageReq {
                id_or_path: String,
                out: String,
            }
            let req = serde_json::from_str::<SalvageReq>(args.params.get())?;
            let result = ops
                .dispatch(
                    &echo_agent_workspace::workspace_ops::WorktreeSalvageReq {
                        id_or_path: req.id_or_path,
                        out: req.out,
                    },
                    None,
                )
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
            to_response(Ok(result))
        }
        "echo.agent/git/worktree/clean-artifacts" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct CleanReq {
                id_or_path: String,
            }
            let req = serde_json::from_str::<CleanReq>(args.params.get())?;
            let result = ops
                .dispatch(
                    &echo_agent_workspace::workspace_ops::WorktreeCleanArtifactsReq {
                        id_or_path: req.id_or_path,
                    },
                    None,
                )
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
            to_response(Ok(result))
        }
        _ => Err(acp::Error::method_not_found()),
    }
}

fn apply_grove_worktree_flag(agent: &MvpAgent, slot: &mut Option<bool>) {
    let root = crate::config::load_effective_config()
        .unwrap_or_else(|_| toml::Value::Table(toml::map::Map::new()));
    let cfg = agent.cfg.borrow();
    apply_grove_worktree_gate(slot, &root, cfg.remote_settings.as_ref());
}

/// Always run the grove gate, even when `slot` is already `Some`. Kill switch last.
pub(crate) fn apply_grove_worktree_gate(
    slot: &mut Option<bool>,
    root: &toml::Value,
    remote: Option<&crate::util::config::RemoteSettings>,
) {
    let (enabled, src) = crate::util::config::gate_grove_worktree(*slot, root, remote);
    tracing::info!(
        target: WORKTREE_EXT_LOG,
        grove_worktree = enabled,
        source = src,
        "WORKTREE_REQUEST_SHELL: resolved grove materialize strategy"
    );
    *slot = Some(enabled);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_request_all_defaults() {
        let req: ListWorktreeRequest = serde_json::from_str("{}").unwrap();
        assert!(req.repo.is_none());
        assert!(req.r#type.is_empty());
        assert!(!req.include_all);
    }

    #[test]
    fn list_request_with_filters() {
        let json = r#"{"repo": "echoagent", "type": ["session", "fork"], "includeAll": true}"#;
        let req: ListWorktreeRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.repo.as_deref(), Some("echoagent"));
        assert_eq!(req.r#type, vec!["session", "fork"]);
        assert!(req.include_all);
    }

    #[test]
    fn show_request_deserializes() {
        let json = r#"{"idOrPath": "wt-abc"}"#;
        let req: ShowWorktreeRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.id_or_path, "wt-abc");
    }

    #[test]
    fn gc_request_all_defaults() {
        let req: GcWorktreeRequest = serde_json::from_str("{}").unwrap();
        assert!(!req.dry_run);
        assert!(req.max_age.is_none());
        assert!(!req.force);
    }

    #[test]
    fn gc_request_with_all_fields() {
        let json = r#"{"dryRun": true, "maxAge": "7d", "force": true}"#;
        let req: GcWorktreeRequest = serde_json::from_str(json).unwrap();
        assert!(req.dry_run);
        assert_eq!(req.max_age.as_deref(), Some("7d"));
        assert!(req.force);
    }

    #[test]
    fn apply_grove_worktree_gate_runs_when_slot_already_some() {
        let root: toml::Value = toml::from_str("[cli]\ngrove_worktree = true").unwrap();
        let remote = crate::util::config::RemoteSettings {
            grove_worktree: Some(false),
            ..Default::default()
        };
        let mut slot = Some(true);
        apply_grove_worktree_gate(&mut slot, &root, Some(&remote));
        assert_eq!(
            slot,
            Some(false),
            "ACP wrapper must re-run the gate when the client already set Some(true)"
        );
        let mut slot = Some(true);
        apply_grove_worktree_gate(&mut slot, &root, None);
        assert_eq!(
            slot,
            Some(false),
            "ACP wrapper must fail closed when remote settings are unavailable"
        );
    }

    #[test]
    fn parse_duration_valid_values() {
        assert_eq!(parse_duration("7d").unwrap(), 7 * 86400);
        assert_eq!(parse_duration("24h").unwrap(), 24 * 3600);
        assert_eq!(parse_duration("30m").unwrap(), 30 * 60);
        assert_eq!(parse_duration("60s").unwrap(), 60);
    }

    #[test]
    fn parse_duration_rejects_invalid() {
        assert!(parse_duration("bad").is_err());
        assert!(parse_duration("").is_err());
        assert!(parse_duration("7x").is_err());
        assert!(parse_duration("abcd").is_err());
    }

    #[test]
    fn db_path_response_serializes() {
        let resp = WorktreeDbPathResponse {
            path: "/home/user/.echo-agent/worktrees.db".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"path\":\"/home/user/.echo-agent/worktrees.db\""));
    }

    #[test]
    fn remove_request_rejects_both_fields_set() {
        use crate::session::worktree::{
            BackgroundCopyContext, RemoveWorktreeRequest, remove_worktree,
        };

        let req = RemoveWorktreeRequest {
            worktree_path: Some("/a".into()),
            id_or_path: Some("b".into()),
            force: false,
            dry_run: false,
        };
        let ctx = BackgroundCopyContext::new();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = rt.block_on(remove_worktree(&req, &ctx));
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("exactly one"), "unexpected error: {msg}");
    }

    #[test]
    fn remove_request_rejects_neither_field_set() {
        use crate::session::worktree::{
            BackgroundCopyContext, RemoveWorktreeRequest, remove_worktree,
        };

        let req = RemoveWorktreeRequest {
            worktree_path: None,
            id_or_path: None,
            force: false,
            dry_run: false,
        };
        let ctx = BackgroundCopyContext::new();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = rt.block_on(remove_worktree(&req, &ctx));
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("either worktreePath or idOrPath"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn db_rebuild_response_carries_report_not_null() {
        // Regression: forwarding `()` instead of the report yields `result: null`,
        // which the CLI rejects with "ACP response missing result field".
        let report = serde_json::json!({
            "discovered": 5,
            "registered": 3,
            "already_tracked": 2,
        });
        let resp = to_response(Ok(report)).expect("rebuild response should serialize");
        let wire: serde_json::Value = serde_json::from_str(resp.0.get()).unwrap();

        let result = wire
            .get("result")
            .expect("envelope must carry a result field");
        assert!(!result.is_null(), "rebuild result must not be null");
        assert_eq!(result["discovered"], 5);
        assert_eq!(result["registered"], 3);
        assert_eq!(result["already_tracked"], 2);
        assert!(wire.get("error").is_none() || wire["error"].is_null());
    }

    // === Tests for repo-wide session resolution ACP types ===

    #[test]
    fn resolve_local_request_deserializes() {
        let json = r#"{"sessionId": "sess-abc", "cwd": "/repo/main"}"#;
        let req: ResolveLocalForWorktreeResumeRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.session_id, "sess-abc");
        assert_eq!(req.cwd, "/repo/main");
    }

    #[test]
    fn resolve_local_response_found_serializes() {
        use crate::session::persistence::LocalSessionResolutionKind;
        let resp = ResolveLocalForWorktreeResumeResponse {
            found: true,
            resolved_session_id: Some("sess-123".into()),
            resolved_cwd: Some("/repo/wt-1".into()),
            resolution_kind: Some(LocalSessionResolutionKind::SameRepoDifferentCwd),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"found\":true"));
        assert!(json.contains("\"resolvedSessionId\":\"sess-123\""));
        assert!(json.contains("\"resolvedCwd\":\"/repo/wt-1\""));
        assert!(json.contains("\"resolutionKind\":"));
    }

    #[test]
    fn resolve_local_response_not_found_omits_optional_fields() {
        let resp = ResolveLocalForWorktreeResumeResponse {
            found: false,
            resolved_session_id: None,
            resolved_cwd: None,
            resolution_kind: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"found\":false"));
        assert!(!json.contains("resolvedSessionId"));
        assert!(!json.contains("resolvedCwd"));
        assert!(!json.contains("resolutionKind"));
    }
}
