//! ACP [session setup]: the four methods that create, attach to, and free a
//! session. Split from `acp_agent.rs`, whose trait impl delegates all four.
//!
//! [session setup]: https://agentclientprotocol.com/protocol/v1/session-setup
use super::reasoning_effort::{
    EffortTarget, NewSessionEffort, resolve_new_session_effort_hint, split_new_session_effort,
};
use super::*;
use crate::agent::session_metrics::SessionStartKind;
/// Refusals resume must give verbatim, so a test cannot mistake some other
/// `invalid_params` for the guard it is pinning.
pub(super) const RESUME_REFUSES_CHAT: &str =
    "session/resume is not supported for chat sessions; use session/load";
pub(super) const RESUME_REFUSES_EXTRA_DIRS: &str =
    "session/resume does not support additionalDirectories";
const TOOL_OVERRIDES_ECHO_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);
async fn read_applied_tool_overrides(
    cmd_tx: &tokio::sync::mpsc::UnboundedSender<SessionCommand>,
) -> Option<echo_agent_sampling_types::ToolOverrides> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    if cmd_tx
        .send(SessionCommand::GetToolOverrides { respond_to: tx })
        .is_err()
    {
        tracing::warn!("tool-overrides echo: session actor command channel closed");
        return None;
    }
    match tokio::time::timeout(TOOL_OVERRIDES_ECHO_BUDGET, rx).await {
        Ok(Ok(overrides)) => overrides,
        Ok(Err(_)) => {
            tracing::warn!("tool-overrides echo: session actor dropped the response channel");
            None
        }
        Err(_) => {
            tracing::warn!("tool-overrides echo exceeded its budget; continuing without echo");
            None
        }
    }
}
fn insert_applied_tool_overrides(
    meta: &mut serde_json::Map<String, serde_json::Value>,
    echo: Option<&echo_agent_sampling_types::ToolOverrides>,
) {
    if let Some(overrides) = echo {
        meta.insert(
            "toolOverrides".to_string(),
            serde_json::to_value(overrides).expect("ToolOverrides is always serializable"),
        );
    }
}
/// Per-client capabilities for one session. Leader mode injects these per
/// request, so they belong to the request rather than to the agent.
struct ClientCaps {
    code_nav: bool,
    terminal: bool,
    fs_read: bool,
    fs_write: bool,
}
/// What an attach recovers from disk before the plan mode moves into the actor:
/// telemetry counters, and the parked approval the rebuilt actor has to re-ask.
struct RestoredSignals {
    compaction_count: u64,
    turn_count: u64,
    tool_call_count: u64,
    plan_mode_state: echo_agent_telemetry::events::PlanModeState,
    /// A parked `exit_plan_mode` approval must be re-issued once a client is back.
    awaiting_plan_approval: bool,
}
impl RestoredSignals {
    fn read(
        signals: Option<&crate::session::signals::SessionSignals>,
        plan_mode: Option<&crate::session::plan_mode::PlanModeSnapshot>,
    ) -> Self {
        use crate::session::plan_mode::PlanModeState;
        use echo_agent_telemetry::events::PlanModeState as Reported;
        Self {
            compaction_count: signals.map(|s| s.compaction_count as u64).unwrap_or(0),
            turn_count: signals.map(|s| s.turn_count as u64).unwrap_or(0),
            tool_call_count: signals.map(|s| s.tool_call_count as u64).unwrap_or(0),
            plan_mode_state: match plan_mode.map(|s| s.state) {
                Some(PlanModeState::Pending) => Reported::Pending,
                Some(PlanModeState::Active | PlanModeState::ExitPending) => Reported::Active,
                Some(PlanModeState::Inactive) | None => Reported::Inactive,
            },
            awaiting_plan_approval: plan_mode.is_some_and(|s| s.awaiting_plan_approval),
        }
    }
}
/// Which ACP method is attaching to an existing session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AttachOperation {
    Load,
    Resume,
}
impl AttachOperation {
    pub(super) fn start_kind(self) -> SessionStartKind {
        match self {
            Self::Load => SessionStartKind::Load,
            Self::Resume => SessionStartKind::Resume,
        }
    }
}
/// What the two attach methods do differently, decided in one exhaustive match
/// so a branch further down cannot quietly skip [`AttachOperation`]. Not in the
/// request's `_meta`, where resume used to write it: that lets a client spoof it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AttachPolicy {
    /// Skip the transcript replay before responding.
    pub(super) no_replay: bool,
    /// Check the session's persisted HEAD out into the caller's `cwd`.
    pub(super) restore_code: bool,
}
impl AttachPolicy {
    pub(super) fn resolve(
        op: AttachOperation,
        meta: Option<&acp::Meta>,
        agent_restore_code: bool,
    ) -> Self {
        let explicit_restore_code = meta
            .and_then(|m| m.get("echo.agent/restore_code"))
            .and_then(|v| v.as_bool());
        match op {
            AttachOperation::Load => Self {
                no_replay: parse_no_replay(meta),
                restore_code: explicit_restore_code.unwrap_or(agent_restore_code),
            },
            AttachOperation::Resume => Self {
                no_replay: true,
                restore_code: false,
            },
        }
    }
}
/// Client-supplied routing an attach's replay must echo: the `echo.agent/persist`
/// blob, the leader unicast target, and the reconnect cursor. All ride the
/// load request's `_meta`.
struct ReplayRouting<'a> {
    persist_data: Option<&'a serde_json::Value>,
    target_client_id: Option<&'a serde_json::Value>,
    cursor: Option<&'a str>,
}
/// The trusted workspace a session will run in.
struct SessionWorkspace {
    cwd: AbsPathBuf,
    remote_settings: Option<crate::util::config::RemoteSettings>,
    mcp_meta_config_map: McpMetaConfigMap,
}
fn session_info_for(session_id: &acp::SessionId, cwd: &AbsPathBuf) -> SessionInfo {
    SessionInfo {
        id: session_id.clone(),
        cwd: cwd.as_str().to_owned(),
    }
}
fn log_session_started(
    session_id: &acp::SessionId,
    kind: SessionStartKind,
    setup_duration: std::time::Duration,
    restored_from_disk: bool,
) {
    echo_agent_telemetry::session_ctx::log_session_event(
        crate::agent::session_metrics::SessionStarted::new(
            session_id.0.to_string(),
            kind,
            setup_duration,
            restored_from_disk,
        ),
    );
}
impl MvpAgent {
    /// Read this client's capabilities, falling back to the agent's own state
    /// where the request says nothing.
    fn resolve_client_caps(
        &self,
        meta: Option<&acp::Meta>,
        init: &acp::InitializeRequest,
    ) -> ClientCaps {
        let (terminal, fs_read, fs_write) = Self::resolve_client_io_caps(meta, init);
        ClientCaps {
            code_nav: meta
                .and_then(|m| m.get("codeNavEnabled"))
                .and_then(|v| v.as_bool())
                .unwrap_or_else(|| self.code_nav_enabled.get()),
            terminal,
            fs_read,
            fs_write,
        }
    }
    /// Resolve the workspace both pipelines run in. Folder trust is recorded
    /// before the MCP merge so an untrusted workspace's repo-local servers
    /// are dropped before anything spawns against them.
    fn resolve_workspace(
        &self,
        cwd: &std::path::Path,
        meta: Option<&acp::Meta>,
    ) -> Result<SessionWorkspace, acp::Error> {
        let cwd = AbsPathBuf::new(cwd.to_path_buf())
            .map_err(|e| acp::Error::invalid_params().data(e.to_string()))?;
        let remote_settings = self.cfg.borrow().remote_settings.clone();
        folder_trust::resolve_and_record(cwd.as_path(), remote_settings.as_ref(), false);
        Ok(SessionWorkspace {
            cwd,
            remote_settings,
            mcp_meta_config_map: parse_mcp_meta_config(meta),
        })
    }
    /// Start the relay mirror for a session and forward its connection state
    /// to the client. `None` when relay is not configured.
    fn start_relay_sync(
        &self,
        session_id: &acp::SessionId,
        session_info: &crate::session::info::Info,
    ) -> Option<crate::relay::RelaySync> {
        let sync = self.create_relay_sync(&session_id.0, session_info)?;
        Self::spawn_relay_state_forwarder(
            sync.subscribe_state(),
            sync.session_id().to_owned(),
            self.gateway.clone(),
        );
        Some(sync)
    }
    /// Where generated titles are pushed, suppressed for ZDR teams.
    fn registry_title_sync(
        &self,
    ) -> Option<crate::session::persistence::RegistryGeneratedTitleSync> {
        self.session_registry_client().map(|client| {
            crate::session::persistence::RegistryGeneratedTitleSync {
                client,
                suppress_for_zdr: self
                    .auth_manager
                    .current_or_expired()
                    .is_some_and(|a| a.is_zdr_team()),
            }
        })
    }
}
impl MvpAgent {
    pub(super) async fn new_session_inner(
        &self,
        arguments: acp::NewSessionRequest,
    ) -> Result<acp::NewSessionResponse, acp::Error> {
        let session_started_at = std::time::Instant::now();
        reject_chat_kind_without_feature(arguments.meta.as_ref())?;
        tracing::debug!(config = ?self.sampling_config, "Received new session request {arguments:?}");
        let init = self.initialize_request.get().ok_or_else(|| {
            acp::Error::invalid_params().data("initialize must be called before new_session")
        })?;
        self.seed_client_config_auth_if_available();
        self.spawn_settings_reapply();
        let SessionWorkspace {
            cwd,
            remote_settings,
            mcp_meta_config_map,
        } = self.resolve_workspace(&arguments.cwd, arguments.meta.as_ref())?;
        let client_session_id = arguments
            .meta
            .as_ref()
            .and_then(|m| m.get("sessionId"))
            .and_then(|v| v.as_str());
        let custom_model_id = arguments
            .meta
            .as_ref()
            .and_then(|m| m.get("modelId").and_then(|v| v.as_str()))
            .filter(|s| !s.is_empty());
        #[cfg(all(feature = "local-workspace", unix))]
        let pending_local_workspace = self
            .start_own_local_workspace_if_needed(&mut session_meta_for_stamp, cwd.as_path())
            .await?;
        #[cfg(all(feature = "local-workspace", not(unix)))]
        {
            use crate::gateway_bridge::local_workspace_supervisor::LocalWorkspaceIntent;
            use crate::gateway_bridge::local_workspace_supervisor::SupervisorError;
            use crate::gateway_bridge::local_workspace_supervisor::parse_local_workspace_intent;
            if matches!(
                parse_local_workspace_intent(session_meta_for_stamp.as_ref()),
                Some(LocalWorkspaceIntent::Own { .. })
            ) {
                return Err(SupervisorError::UnsupportedPlatform.into_acp_error());
            }
        }
        #[allow(unused_variables)]
        let session_computer_sessions = resolve_session_computer_sessions(arguments.meta.as_ref())?;
        let is_chat_kind =
            ChatKindClaim::from_meta(arguments.meta.as_ref()).declared() == SessionKind::Chat;
        let session_yolo_mode = arguments
            .meta
            .as_ref()
            .and_then(|m| m.get("yoloMode"))
            .and_then(|v| v.as_bool())
            .unwrap_or(self.default_yolo_mode);
        let session_auto_mode = resolve_session_auto_mode(
            arguments.meta.as_ref(),
            self.default_auto_mode,
            session_yolo_mode,
        );
        let session_id = match client_session_id {
            Some(s) => {
                uuid::Uuid::try_parse(s).map_err(|e| {
                    acp::Error::invalid_params().data(format!(
                        "Invalid UUID format for _meta.sessionId '{}': {}",
                        s, e
                    ))
                })?;
                acp::SessionId::new(s.to_string())
            }
            None => acp::SessionId::new(uuid::Uuid::now_v7().to_string()),
        };
        #[cfg(all(feature = "local-workspace", unix))]
        let mut local_ws_reap_guard =
            self.new_local_workspace_reap_guard(session_id.clone(), false);
        #[cfg(all(feature = "local-workspace", unix))]
        if let Some(handle) = pending_local_workspace {
            self.register_local_workspace_supervisor(session_id.clone(), handle);
            local_ws_reap_guard = self.new_local_workspace_reap_guard(session_id.clone(), true);
        }
        let mut session_timer = crate::instrumentation_timer!("session.new_session");
        session_timer.with_field("session_id", session_id.0.as_ref());
        session_timer.with_field("cwd", cwd.as_str());
        let client_identifier = arguments
            .meta
            .as_ref()
            .and_then(|m| m.get("clientIdentifier"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                self.initialize_request
                    .get()
                    .and_then(|req| req.meta.as_ref())
                    .and_then(|m| m.get("clientIdentifier"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            });
        let session_info = session_info_for(&session_id, &cwd);
        let mut model_agent_type: Option<String> = None;
        let mut session_sampling_override: Option<SamplingConfig> = None;
        let mut unavailable_custom: Option<String> = None;
        let session_initial_model = chat_initial_model(is_chat_kind, custom_model_id);
        let build_custom_model_id = if is_chat_kind { None } else { custom_model_id };
        let campaign_nudge = if is_chat_kind {
            None
        } else {
            crate::util::config::campaign_driven_models_default().filter(|c| {
                build_custom_model_id.is_none()
                    || build_custom_model_id == c.pre_campaign.as_deref()
                    || build_custom_model_id == Some(c.value.as_str())
            })
        };
        let campaign_nudged = campaign_nudge.is_some();
        if let Some(c) = &campaign_nudge {
            tracing::info!(
                model = %c.value,
                requested = ?custom_model_id,
                "new_session: applying campaign-driven default model"
            );
        }
        let build_custom_model_id: Option<String> = campaign_nudge
            .map(|c| c.value)
            .or_else(|| build_custom_model_id.map(str::to_owned));
        let resolved_custom_model = build_custom_model_id
            .as_deref()
            .and_then(|custom_model| match self
                .resolve_model_id(&acp::ModelId::new(custom_model))
            {
                Ok(model) if model.info.user_selectable => {
                    model_agent_type = Some(model.info().agent_type.clone());
                    let origin_client = self
                        .origin_client_info_from_meta(arguments.meta.as_ref());
                    session_sampling_override = Some(
                        self.prepare_sampling_config_for_model(&model, origin_client),
                    );
                    Some(custom_model)
                }
                Ok(_) => {
                    tracing::warn!(
                        requested_model = custom_model,
                        "Requested model not allowed by allowed_models"
                    );
                    if !campaign_nudged {
                        unavailable_custom = Some(format!(
                            "Requested model \"{custom_model}\" is not selectable under the current allowed_models configuration"
                        ));
                    }
                    None
                }
                Err(error) => {
                    tracing::warn!(
                        requested_model = custom_model,
                        error = ?error,
                        "Requested model not found"
                    );
                    if !campaign_nudged {
                        unavailable_custom = Some(format!(
                            "Requested model \"{custom_model}\" is not available in the runtime model catalog"
                        ));
                    }
                    None
                }
            });
        // An explicit client model is a contract, not a preference. Silently
        // falling back can bind a newly created session to a built-in model
        // with different credentials, while the client records the requested
        // id. Fail before the session is persisted so the caller can reload
        // the catalog or choose another configured model.
        if let Some(reason) = unavailable_custom {
            return Err(acp::Error::invalid_params().data(reason));
        }
        if model_agent_type.is_none()
            && custom_model_id.is_none()
            && let Ok(default_model) =
                self.resolve_model_id(&self.models_manager.current_model_id())
        {
            model_agent_type = Some(default_model.info().agent_type.clone());
        } else if model_agent_type.is_none() && custom_model_id.is_some() {
            tracing::debug!(
                custom_model = ?custom_model_id,
                current_model_id = %self.models_manager.current_model_id().0,
                "Skipping current_model_id agent_type fallback: custom model was requested, \
                 avoiding cross-client agent_type contamination in leader mode"
            );
        }
        let origin_client = self.origin_client_info_from_meta(arguments.meta.as_ref());
        let mut session_sampling = session_sampling_override.unwrap_or_else(|| {
            self.resolve_sampling_config_for_model(
                &self.models_manager.current_model_id(),
                origin_client.clone(),
            )
        });
        let effort_route = split_new_session_effort(
            resolved_custom_model,
            resolve_new_session_effort_hint(
                parse_reasoning_effort_meta(arguments.meta.as_ref()),
                self.models_manager.current_reasoning_effort(),
            ),
        );
        let spawn_effort = match effort_route {
            NewSessionEffort::Spawn(effort) => Some(effort),
            NewSessionEffort::Switch(_) | NewSessionEffort::None => None,
        };
        self.models_manager.apply_supported_effort(
            &mut session_sampling,
            spawn_effort.or_else(|| self.models_manager.current_reasoning_effort()),
            &session_id,
            EffortTarget::SummaryClient,
        );
        let (summary_client, summary_model) = self.build_summary_client(&session_sampling)?;
        let relay_sync = self.start_relay_sync(&session_id, &session_info);
        let model_id = match &session_initial_model {
            Some(chat_model) => acp::ModelId::new(chat_model.clone()),
            None => resolved_custom_model
                .map(acp::ModelId::new)
                .unwrap_or_else(|| self.models_manager.current_model_id()),
        };
        let session_model_id = model_id.clone();
        let persistence = if is_chat_kind {
            crate::session::persistence::PersistenceHandle::noop()
        } else {
            let _timer = crate::instrumentation_timer!("session.persistence_init");
            let registry_title_sync = self.registry_title_sync();
            crate::session::persistence::new(
                &session_info,
                model_id,
                crate::session::persistence::SessionDeps {
                    sampling_client: summary_client,
                    storage_mode: self.storage_mode.get(),
                    auth_manager: Some(self.auth_manager.clone()),
                    relay_sync,
                    gateway: Some(self.gateway.clone()),
                    session_summary_model: summary_model,
                    registry_title_sync,
                    search_index: self.search_index_cell(),
                },
            )
            .await
            .map_err(|e| crate::session::persistence::io_error_to_acp(&e))?
        };
        self.set_turn_number(&session_id, 0u64);
        let chat_history = vec![];
        let ClientCaps {
            code_nav: client_code_nav_enabled,
            terminal: client_terminal,
            fs_read: client_fs_read,
            fs_write: client_fs_write,
        } = self.resolve_client_caps(arguments.meta.as_ref(), init);
        let spawn_res = {
            let mut timer = crate::instrumentation_timer!("session.spawn_session_actor");
            timer.with_field("session_id", session_id.0.as_ref());
            // Resolve MCPs only after earlier async preparation. A global
            // toggle must see the resident actor or precede its config read.
            let _mcp_lifecycle = self.mcp_session_lifecycle.read().await;
            let spawn_opts = if is_chat_kind {
                chat_session_spawn_options(
                    session_info.clone(),
                    cwd.clone(),
                    arguments.meta.as_ref(),
                    model_agent_type.as_deref(),
                    session_model_id,
                    session_yolo_mode,
                )
            } else {
                let (initial_client_mcp_servers, mcp_servers) = self
                    .resolve_mcp_servers(arguments.mcp_servers, cwd.as_path())
                    .await;
                SessionSpawnOptions {
                    session_info: session_info.clone(),
                    cwd: cwd.clone(),
                    mcp_servers,
                    initial_client_mcp_servers,
                    mcp_meta_config_map,
                    persistence,
                    chat_history,
                    rewind_points_file_path: None,
                    initial_total_tokens: 0,
                    origin_client: origin_client.clone(),
                    client_code_nav_enabled,
                    client_terminal,
                    client_fs_read,
                    client_fs_write,
                    envrc: None,
                    persisted_signals: None,
                    persisted_plan_mode: None,
                    persisted_goal_mode: None,
                    persisted_workflow_runs: Vec::new(),
                    persisted_announcement_state: None,
                    session_meta: arguments.meta.as_ref(),
                    model_agent_type: model_agent_type.as_deref(),
                    session_model_id,
                    initial_reasoning_effort: spawn_effort,
                    session_yolo_mode,
                    session_auto_mode: session_auto_mode && !session_yolo_mode,
                    prompt_display_cwd: None,
                    is_chat_kind: false,
                }
            };
            let mut spawn_timer = crate::instrumentation_timer!("session.spawn");
            spawn_timer.with_field("session_id", session_id.0.as_ref());
            spawn_timer.with_subphase(echo_agent_telemetry::startup::Subphase::SessionSpawn);
            self.spawn_and_register_session(init, spawn_opts).await
        };
        #[cfg(all(feature = "local-workspace", unix))]
        if spawn_res.is_err() {
            self.shutdown_gateway_bridge(&session_id);
        }
        spawn_res?;
        tracing::debug!(session_id = %session_id.0, "new_session: spawn_session_actor");
        #[cfg(feature = "local-workspace")]
        if local_workspace_intent_present(arguments.meta.as_ref()) {
            self.mark_local_workspace_bound(session_id.clone());
        }
        self.maybe_spawn_interactive_trust_prompt(
            &session_id,
            cwd.as_path(),
            remote_settings.as_ref(),
        );
        let bridge_attach = BridgeAttach::NotAttached;
        let product_analytics = self.product_analytics_enabled();
        if product_analytics || echo_agent_telemetry::external::is_active() {
            let sid = session_id.0.to_string();
            let ci = client_identifier.clone();
            let cv = self.client_version();
            let cwd_str = cwd.as_str().to_owned();
            let perm = if session_yolo_mode {
                echo_agent_telemetry::enums::PermissionMode::AlwaysApprove
            } else if session_auto_mode
                && crate::util::config::auto_permission_mode_enabled_from_disk()
            {
                echo_agent_telemetry::enums::PermissionMode::Auto
            } else {
                echo_agent_telemetry::enums::PermissionMode::Ask
            };
            tokio::spawn(async move {
                let git = echo_agent_telemetry::context::collect_git_context(&cwd_str);
                let ev = echo_agent_telemetry::events::SessionNew {
                    session_id: sid,
                    client_identifier: ci,
                    client_version: cv,
                    is_git_repo: git.is_git_repo,
                    permission_mode: perm,
                };
                echo_agent_telemetry::session_ctx::log_event_dual(product_analytics, ev);
            });
        }
        if let Some(model_id) = resolved_custom_model {
            let switch_effort = match effort_route {
                NewSessionEffort::Switch(effort) => Some(effort),
                NewSessionEffort::Spawn(_) | NewSessionEffort::None => None,
            };
            let switched = crate::timed!(log: "new_session: set_session_model", {
                crate::agent::handlers::model_switch::apply(
                    self,
                    acp::SetSessionModelRequest::new(session_id.clone(), acp::ModelId::new(model_id)),
                    switch_effort,
                )
                .await
            });
            match switched {
                Ok(_) => {
                    tracing::debug!(session_id = %session_id.0, "new_session: set_session_model")
                }
                Err(err) => {
                    tracing::warn!(
                        session_id = %session_id.0,
                        error = ?err,
                        requested_effort = ?switch_effort,
                        "new_session: set_session_model failed; session keeps spawn defaults"
                    )
                }
            }
        }
        let indexed_roots = self.indexed_roots_for(cwd.as_path());
        let (git_root, is_git_repo, discovery_failed) =
            match echo_agent_workspace::session::git::discover_git_root(cwd.as_path()) {
                GitDiscoveryResult::Found(root) => {
                    let root_str = root.to_string_lossy().trim_end_matches('/').to_string();
                    (Some(root_str), true, false)
                }
                GitDiscoveryResult::NotARepo => {
                    tracing::debug!("new_session: not a git repository");
                    (None, false, false)
                }
                GitDiscoveryResult::DiscoveryFailed(e) => {
                    tracing::warn!(
                        error = %e,
                        cwd = %cwd.as_str(),
                        "new_session: git repo discovery failed unexpectedly"
                    );
                    (None, false, true)
                }
            };
        let (show_non_git_warning, feedback_enabled) = {
            let cfg = self.cfg.borrow();
            let show_non_git_warning = !is_git_repo
                && !discovery_failed
                && cfg
                    .remote_settings
                    .as_ref()
                    .and_then(|s| s.non_git_warning)
                    .unwrap_or(cfg.features.non_git_warning);
            let feedback_enabled = cfg.is_feedback_enabled();
            (show_non_git_warning, feedback_enabled)
        };
        echo_agent_telemetry::unified_log::info(
            "session created",
            Some(session_id.0.as_ref()),
            Some(serde_json::json!({"cwd": cwd.as_str()})),
        );
        let models = if is_chat_kind {
            chat_new_session_model_state(
                self.chat_modes.model_state().await,
                session_initial_model.filter(|_| matches!(bridge_attach, BridgeAttach::Spawned)),
            )
        } else {
            self.model_state(Some(&session_id))
        };
        let applied_tool_overrides = match self.session_handle_waiting_for_load(&session_id).await {
            Some(handle) => read_applied_tool_overrides(&handle.cmd_tx).await,
            None => {
                tracing::warn!(
                    session_id = %session_id.0,
                    "session/new toolOverrides echo: session handle not found"
                );
                None
            }
        };
        let mut meta = serde_json::json!({
            "currentWorkingDirectory": cwd.as_str().to_owned(),
            "codebaseIndexed": indexed_roots,
            "isGitRepo": is_git_repo,
            "gitRoot": git_root,
            "showNonGitWarning": show_non_git_warning,
            "feedbackEnabled": feedback_enabled,
        });
        if let Some(obj) = meta.as_object_mut() {
            self.insert_session_config_meta(
                obj,
                &session_id,
                cwd.as_str().to_owned(),
                None,
                &models,
            );
            insert_applied_tool_overrides(obj, applied_tool_overrides.as_ref());
        }
        self.attach_status_line(&session_id, arguments.meta.as_ref(), init);
        #[cfg(all(feature = "local-workspace", unix))]
        local_ws_reap_guard.disarm();
        log_session_started(
            &session_id,
            SessionStartKind::New,
            session_started_at.elapsed(),
            false,
        );
        Ok(acp::NewSessionResponse::new(session_id)
            .models(Some(models))
            .meta(meta.as_object().cloned()))
    }
    pub(super) async fn load_session_inner(
        &self,
        arguments: acp::LoadSessionRequest,
    ) -> Result<acp::LoadSessionResponse, acp::Error> {
        self.attach_session(arguments, AttachOperation::Load).await
    }
    async fn attach_session(
        &self,
        arguments: acp::LoadSessionRequest,
        op: AttachOperation,
    ) -> Result<acp::LoadSessionResponse, acp::Error> {
        let attach_started_at = std::time::Instant::now();
        let _load_guard = self.begin_session_load(&arguments.session_id);
        reject_chat_kind_without_feature(arguments.meta.as_ref())?;
        self.sweep_dead_sessions();
        if !self.is_resident(&arguments.session_id) {
            self.drain_old_session_thread(&arguments.session_id).await;
        }
        tracing::debug!("Received load session request {arguments:?}");
        let init = self.initialize_request.get().ok_or_else(|| {
            acp::Error::invalid_params().data("initialize must be called before load_session")
        })?;
        self.seed_client_config_auth_if_available();
        let persist_data = arguments
            .meta
            .as_ref()
            .and_then(|m| m.get("echo.agent/persist"))
            .cloned();
        let target_client_id = arguments
            .meta
            .as_ref()
            .and_then(|m| m.get("echo.agent/leaderClientId"))
            .cloned();
        let acp::LoadSessionRequest {
            session_id,
            cwd,
            mcp_servers: client_mcp_servers,
            meta: request_meta,
            ..
        } = arguments;
        let policy = AttachPolicy::resolve(op, request_meta.as_ref(), self.restore_code);
        let SessionWorkspace {
            cwd,
            remote_settings,
            mcp_meta_config_map,
        } = self.resolve_workspace(&cwd, request_meta.as_ref())?;
        let mut load_timer = crate::instrumentation_timer!("session.load_session");
        load_timer.with_field("session_id", session_id.0.as_ref());
        load_timer.with_field("cwd", cwd.as_str());
        let git_root =
            echo_agent_workspace::session::git::find_git_root_from_path(cwd.as_path()).ok();
        if let Some(root) = git_root {
            tokio::task::spawn_blocking(move || {
                crate::session::worktree_pool::cleanup_stale_pool_worktrees(Some(&root));
            });
        }
        let session_info = session_info_for(&session_id, &cwd);
        let current_session_dir = crate::session::persistence::session_dir(&session_info);
        tokio::task::spawn_blocking(move || {
            crate::session::persistence::cleanup_stale_sessions(Some(&current_session_dir));
        });
        let session_exists = self.is_resident(&session_id);
        let no_replay = policy.no_replay;
        if session_exists {
            tracing::info!(
                session_id = %session_id.0,
                "Reconnect detected: flushing persistence buffer before replay"
            );
            if !no_replay && let Some(handle) = self.resident_handle(&session_id) {
                handle
                    .gateway_enabled
                    .store(false, std::sync::atomic::Ordering::Relaxed);
            }
            let mut flush_timer = crate::instrumentation_timer!("session.reconnect_flush");
            flush_timer.with_field("session_id", session_id.0.as_ref());
            if let Err(reason) = self.flush_session(&session_id).await {
                tracing::warn!(
                    session_id = %session_id.0,
                    reason,
                    "Reconnect flush failed"
                );
            }
            drop(flush_timer);
        }
        let initial_reasoning_effort = parse_reasoning_effort_meta(request_meta.as_ref());
        let origin_client = self.origin_client_info_from_meta(request_meta.as_ref());
        let mut load_session_sampling = self.resolve_sampling_config_for_model(
            &self.models_manager.current_model_id(),
            origin_client.clone(),
        );
        self.models_manager.apply_supported_effort(
            &mut load_session_sampling,
            initial_reasoning_effort,
            &session_id,
            EffortTarget::SummaryClient,
        );
        let (summary_client, summary_model) = self.build_summary_client(&load_session_sampling)?;
        let relay_sync = self.start_relay_sync(&session_id, &session_info);
        let mut persistence_timer = crate::instrumentation_timer!("session.load");
        persistence_timer.with_field("session_id", session_id.0.as_ref());
        persistence_timer.with_subphase(echo_agent_telemetry::startup::Subphase::SessionLoad);
        let backend = if self.build_registry_config().is_some() {
            Some(crate::remote::BackendClient::new().with_auth_manager(self.auth_manager.clone()))
        } else {
            None
        };
        let registry_title_sync = self.registry_title_sync();
        let (persistence_info, persistence) = crate::session::persistence::load_light(
            &session_info,
            backend.as_ref(),
            crate::session::persistence::SessionDeps {
                sampling_client: summary_client,
                storage_mode: self.storage_mode.get(),
                auth_manager: Some(self.auth_manager.clone()),
                relay_sync,
                gateway: Some(self.gateway.clone()),
                session_summary_model: summary_model,
                registry_title_sync,
                search_index: self.search_index_cell(),
            },
        )
        .await
        .map_err(|e| crate::session::persistence::io_error_to_acp(&e))?;
        drop(persistence_timer);
        let crate::session::persistence::PersistedInfoLight {
            summary,
            chat_history,
            plan_state: _,
            plan_mode_state: persisted_plan_mode,
            updates_file_path,
            rewind_points_file_path,
            signals: persisted_signals,
            announcement_state: persisted_announcement_state,
            goal_mode_state: _persisted_goal_mode,
            workflow_runs: persisted_workflow_runs,
        } = persistence_info;
        let restored =
            RestoredSignals::read(persisted_signals.as_ref(), persisted_plan_mode.as_ref());
        self.set_turn_number(&session_id, summary.next_trace_turn);
        tracing::info!(
            session_id = %session_id.0,
            next_trace_turn = summary.next_trace_turn,
            "Loaded session telemetry turn counter from persistence"
        );
        let cursor = request_meta
            .as_ref()
            .and_then(|m| m.get("cursor"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let session_yolo_mode = request_meta
            .as_ref()
            .and_then(|m| m.get("yoloMode"))
            .and_then(|v| v.as_bool())
            .unwrap_or(self.default_yolo_mode);
        let session_auto_mode = resolve_session_auto_mode(
            request_meta.as_ref(),
            self.default_auto_mode,
            session_yolo_mode,
        );
        #[allow(unused_variables)]
        let session_computer_sessions = resolve_session_computer_sessions(request_meta.as_ref())?;
        let code_restore_info = self
            .restore_session_code(&session_id, &cwd, &summary, policy.restore_code)
            .await;
        let load_envrc = {
            let skip_envrc = request_meta
                .as_ref()
                .and_then(|m| m.get("echo.agent/skip_envrc"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if skip_envrc {
                false
            } else {
                self.cfg.borrow().session.load_envrc.unwrap_or(true)
            }
        };
        let envrc = if !load_envrc {
            Some(echo_agent_workspace::envrc::spawn_envrc_load(
                cwd.as_path().to_path_buf(),
                false,
            ))
        } else if session_exists {
            None
        } else {
            Some(echo_agent_workspace::envrc::spawn_envrc_load(
                cwd.as_path().to_path_buf(),
                folder_trust::project_scope_allowed(cwd.as_path()),
            ))
        };
        let (initial_total_tokens, unfinished_subagents) = self
            .replay_transcript_gate(
                &session_id,
                &cwd,
                &updates_file_path,
                ReplayRouting {
                    persist_data: persist_data.as_ref(),
                    target_client_id: target_client_id.as_ref(),
                    cursor: cursor.as_deref(),
                },
                no_replay,
            )
            .await?;
        self.attach_status_line(&session_id, request_meta.as_ref(), init);
        let ClientCaps {
            code_nav: client_code_nav_enabled,
            terminal: client_terminal,
            fs_read: client_fs_read,
            fs_write: client_fs_write,
        } = self.resolve_client_caps(request_meta.as_ref(), init);
        let prompt_display_cwd = request_meta
            .as_ref()
            .and_then(|m| m.get("echo.agent/display_cwd"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| summary.prompt_display_cwd.clone());
        let mcp_lifecycle = self.mcp_session_lifecycle.read().await;
        let (initial_client_mcp_servers, mcp_servers) =
            self.resolve_mcp_servers(client_mcp_servers, cwd.as_path()).await;
        let restored_from_disk = if !self.is_resident(&session_id) {
            tracing::info!(
                session_id = %session_id.0,
                "load_session: spawning new session actor (session not in memory)"
            );
            let mut spawn_timer = crate::instrumentation_timer!("session.spawn");
            spawn_timer.with_field("session_id", session_id.0.as_ref());
            spawn_timer.with_subphase(echo_agent_telemetry::startup::Subphase::SessionSpawn);
            let persisted_agent_name: Option<String> = summary.agent_name.clone().or_else(|| {
                self.resolve_model_id(&summary.current_model_id)
                    .ok()
                    .map(|m| m.info().agent_type.clone())
            });
            self.spawn_and_register_session(
                init,
                SessionSpawnOptions {
                    session_info,
                    cwd: cwd.clone(),
                    mcp_servers,
                    initial_client_mcp_servers,
                    mcp_meta_config_map,
                    persistence,
                    chat_history,
                    rewind_points_file_path,
                    initial_total_tokens,
                    origin_client: origin_client.clone(),
                    client_code_nav_enabled,
                    client_terminal,
                    client_fs_read,
                    client_fs_write,
                    envrc,
                    persisted_signals,
                    persisted_plan_mode,
                    persisted_goal_mode: _persisted_goal_mode,
                    persisted_workflow_runs,
                    persisted_announcement_state,
                    session_meta: request_meta.as_ref(),
                    model_agent_type: persisted_agent_name.as_deref(),
                    session_model_id: summary.current_model_id.clone(),
                    initial_reasoning_effort: None,
                    session_yolo_mode,
                    session_auto_mode: session_auto_mode && !session_yolo_mode,
                    prompt_display_cwd,
                    is_chat_kind: false,
                },
            )
            .await?;
            drop(spawn_timer);
            true
        } else {
            tracing::info!(
                session_id = %session_id.0,
                mcp_server_count = mcp_servers.len(),
                "load_session: reconnecting to existing session, updating MCP servers"
            );
            let attach_hints = explicit_startup_hints(request_meta.as_ref());
            self.with_resident_mut(&session_id, |handle| {
                handle.initial_client_mcp_servers = initial_client_mcp_servers;
                if let Some(hints) = attach_hints {
                    let _ =
                        handle
                            .cmd_tx
                            .send(crate::session::SessionCommand::UpdateAttachPolicy {
                                startup_hints: Box::new(hints),
                            });
                }
                let (tx, _rx) = tokio::sync::oneshot::channel();
                let _ = handle
                    .cmd_tx
                    .send(crate::session::SessionCommand::UpdateMcpServers {
                        mcp_servers,
                        respond_to: tx,
                    });
            });
            false
        };
        drop(mcp_lifecycle);
        {
            let init_meta = self
                .initialize_request
                .get()
                .and_then(|init| init.meta.as_ref());
            if let Some(handle) = self.resident_handle(&session_id) {
                enqueue_replace_system_prompt_override(
                    &handle.cmd_tx,
                    request_meta.as_ref(),
                    init_meta,
                );
            }
        }
        if let Some(hooks) = crate::extensions::hooks::reconnect_client_hooks(request_meta.as_ref())
            && let Some(handle) = self.resident_handle(&session_id)
        {
            handle.set_client_hooks(hooks);
        }
        #[allow(unused_variables)]
        let local_transcript_rendered = !no_replay
            && updates_file_path
                .as_ref()
                .and_then(|p| std::fs::metadata(p).ok())
                .is_some_and(|m| m.len() > 0);
        self.refresh_reconnect_session_state(
            &session_id,
            client_code_nav_enabled,
            session_yolo_mode,
            session_auto_mode,
        );
        self.maybe_spawn_interactive_trust_prompt(
            &session_id,
            cwd.as_path(),
            remote_settings.as_ref(),
        );
        self.heal_orphaned_subagents(&session_id, &unfinished_subagents)
            .await;
        self.restore_persisted_model(&session_id, &summary, initial_reasoning_effort)
            .await;
        let (model_state, response_meta) = self
            .build_attach_response_meta(&session_id, &summary, persist_data, code_restore_info)
            .await;
        echo_agent_telemetry::unified_log::info("session loaded", Some(session_id.0.as_ref()), None);
        let response = acp::LoadSessionResponse::new()
            .models(Some(model_state))
            .meta(response_meta.as_object().cloned());
        if let Some(handle) = self.resident_handle(&session_id) {
            let _ = handle.cmd_tx.send(SessionCommand::AdvertiseCommands);
            if restored.awaiting_plan_approval {
                let _ = handle.cmd_tx.send(SessionCommand::RestorePlanApproval);
            }
        }
        if self.product_analytics_enabled() {
            log_event(echo_agent_telemetry::events::SessionLoad {
                session_id: session_id.0.to_string(),
                compaction_count: restored.compaction_count,
                turn_count: restored.turn_count,
                tool_call_count: restored.tool_call_count,
                plan_mode_state: restored.plan_mode_state,
                permission_mode: if session_yolo_mode {
                    echo_agent_telemetry::enums::PermissionMode::AlwaysApprove
                } else if session_auto_mode
                    && crate::util::config::auto_permission_mode_enabled_from_disk()
                {
                    echo_agent_telemetry::enums::PermissionMode::Auto
                } else {
                    echo_agent_telemetry::enums::PermissionMode::Ask
                },
                model_id: summary.current_model_id.0.to_string(),
                restored_from_disk: true,
            });
        }
        log_session_started(
            &session_id,
            op.start_kind(),
            attach_started_at.elapsed(),
            restored_from_disk,
        );
        Ok(response)
    }
    /// Restore-code phase: check the persisted HEAD out into `cwd`, then
    /// worktree nor the session's own, so it cannot detach a real checkout.
    async fn restore_session_code(
        &self,
        session_id: &acp::SessionId,
        cwd: &AbsPathBuf,
        summary: &crate::session::persistence::Summary,
        restore_code_requested: bool,
    ) -> Option<serde_json::Value> {
        let registry_client_for_restore = self.session_registry_client();
        if restore_code_requested && registry_client_for_restore.is_none() {
            echo_agent_workspace::session::git::warn_registry_disabled_restore(session_id.0.as_ref());
        }
        let restore_checkout_allowed =
            echo_agent_workspace::session::git::restore_code_checkout_allowed(
                cwd.as_path(),
                Some(summary.info.cwd.as_str()),
            );
        if restore_code_requested
            && !restore_checkout_allowed
            && let Some(ref target_sha) = summary.head_commit
        {
            tracing::warn!(
                target: echo_agent_workspace::session::git::RESTORE_CODE_LOG,
                session_id = %session_id.0,
                supplied_cwd = %cwd.as_str(),
                persisted_cwd = %summary.info.cwd,
                target_sha = %target_sha,
                "restore_code: skipping session HEAD checkout — supplied cwd is neither a echoagent worktree nor the session's persisted cwd (refusing to detach the source repo)"
            );
            echo_agent_telemetry::unified_log::warn(
                "restore_code: skipped session HEAD checkout (unsafe cwd)",
                Some(session_id.0.as_ref()),
                Some(serde_json::json!({
                    "supplied_cwd": cwd.as_str(),
                    "persisted_cwd": summary.info.cwd,
                    "target_sha": target_sha,
                })),
            );
        }
        let mut code_restore_info: Option<serde_json::Value> = None;
        if restore_code_requested
            && restore_checkout_allowed
            && let Some(ref target_sha) = summary.head_commit
        {
            use echo_agent_workspace::session::git::RestoreKind;
            let outcome = echo_agent_workspace::session::git::checkout_session_commit(
                cwd.as_path(),
                target_sha,
                true,
                session_id.0.as_ref(),
            )
            .await;
            let kind = if !outcome.checked_out {
                RestoreKind::CheckoutFailed
            } else {
                match registry_client_for_restore {
                    None => RestoreKind::RegistryOff,
                    Some(registry_client) => {
                        let _ = registry_client;
                        RestoreKind::RegistryOff
                    }
                }
            };
            code_restore_info =
                crate::agent::restore_code::build_code_restore_meta(target_sha, &outcome, kind);
        }
        code_restore_info
    }
    /// Replay-gate phase: replay the transcript (unless `no_replay`), reopen
    /// the live-output gate, drain deltas so replay precedes the response.
    /// Stale-task reconciliation runs even under `no_replay`: it corrects state.
    async fn replay_transcript_gate(
        &self,
        session_id: &acp::SessionId,
        cwd: &AbsPathBuf,
        updates_file_path: &Option<PathBuf>,
        routing: ReplayRouting<'_>,
        no_replay: bool,
    ) -> Result<(u64, Vec<(String, String)>), acp::Error> {
        let session_id = session_id.clone();
        let cwd = cwd.clone();
        let updates_file_path = updates_file_path.clone();
        let persist_data = routing.persist_data.cloned();
        let target_client_id = routing.target_client_id.cloned();
        let cursor = routing.cursor.map(str::to_string);
        let (initial_total_tokens, delta_completions, unfinished_subagents) = if no_replay {
            tracing::info!(
                session_id = %session_id.0,
                "Skipping session replay (session/resume, or a noReplay load)"
            );
            (
                Self::extract_initial_tokens_from_updates(&updates_file_path),
                Vec::new(),
                Vec::new(),
            )
        } else {
            let (tokens, replay_end_offset, unfinished_subagents) = self
                .replay_session_updates(
                    &session_id,
                    &cwd,
                    &updates_file_path,
                    persist_data.as_ref(),
                    target_client_id.as_ref(),
                    cursor.as_deref(),
                )
                .await?;
            let cursor_mark_replay = cursor.is_none();
            let _timer = crate::instrumentation_timer!("session.delta_flush_replay");
            let completions = match self.flush_session(&session_id).await {
                Ok(()) => self.replay_session_updates_from_offset_enqueue(
                    &session_id,
                    &updates_file_path,
                    replay_end_offset,
                    persist_data.as_ref(),
                    target_client_id.as_ref(),
                    cursor_mark_replay,
                ),
                Err(reason) => {
                    tracing::warn!(
                        session_id = %session_id.0,
                        reason,
                        "Post-replay flush failed, skipping delta replay"
                    );
                    Vec::new()
                }
            };
            (tokens, completions, unfinished_subagents)
        };
        if let Some(handle) = self.resident_handle(&session_id) {
            handle
                .gateway_enabled
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        for rx in delta_completions {
            let _ = rx.await;
        }
        let reconcile_completions = {
            let _timer = crate::instrumentation_timer!("session.reconcile_stale_tasks");
            self.reconcile_stale_background_tasks(&session_id, &updates_file_path)
        };
        for rx in reconcile_completions {
            let _ = rx.await;
        }
        Ok((initial_total_tokens, unfinished_subagents))
    }
    /// Reconnect phase: re-apply per-client capability and permission state
    /// to the resident handle, which still reflects the client that spawned it.
    fn refresh_reconnect_session_state(
        &self,
        session_id: &acp::SessionId,
        client_code_nav_enabled: bool,
        session_yolo_mode: bool,
        session_auto_mode: bool,
    ) {
        let session_id = session_id.clone();
        self.with_resident_mut(&session_id, |handle| {
            handle.code_nav_enabled = client_code_nav_enabled;
            if session_yolo_mode && !handle.yolo_mode {
                tracing::debug!(
                    session_id = %session_id.0,
                    "Setting YOLO mode on reconnect from load_session request metadata"
                );
                handle.yolo_mode = true;
                let _ = handle
                    .cmd_tx
                    .send(SessionCommand::SetYoloMode { enabled: true });
            }
            if session_auto_mode
                && !session_yolo_mode
                && crate::util::config::auto_permission_mode_enabled_from_disk()
            {
                tracing::debug!(
                    session_id = %session_id.0,
                    "Setting auto mode on reconnect from load_session request metadata"
                );
                handle.yolo_mode = false;
                let _ = handle
                    .cmd_tx
                    .send(SessionCommand::SetAutoMode { enabled: true });
            }
        });
    }
    /// Heal crash-orphaned subagents from both sources (replayed spawns with
    /// no finish, on-disk `running` metas), keyed by id so a double orphan
    /// heals once. Runs under `noReplay` too, and persists: skipping corrupts disk.
    async fn heal_orphaned_subagents(
        &self,
        session_id: &acp::SessionId,
        unfinished_subagents: &[(String, String)],
    ) {
        let session_id = session_id.clone();
        let orphan_parent = self
            .resident_handle(&session_id)
            .map(|handle| (handle.cmd_tx.clone(), handle.info.cwd.clone()));
        if let Some((parent_cmd_tx, session_cwd)) = orphan_parent {
            let session_dir = crate::session::persistence::session_dir(&SessionInfo {
                id: session_id.clone(),
                cwd: session_cwd,
            });
            crate::agent::subagent::reconcile_orphaned_subagents_with_backend(
                unfinished_subagents,
                &echo_agent_tools::implementations::echo_agent_build::task::backend::ChannelBackend::new(
                    self.subagent_event_tx.clone(),
                ),
                &session_dir,
                session_id.0.as_ref(),
                &self.gateway,
                Some(&parent_cmd_tx),
                crate::agent::subagent::ORPHAN_RECONCILE_REASON,
                self.session_registry.live_orphan_heal_lock(&session_id),
            )
            .await;
        }
    }
    /// Model-restore phase: point the actor at the persisted model without
    /// writing the global `current_model_id` (shared across leader clients).
    /// A vanished model falls back within its family, or blocks prompts.
    pub(super) async fn restore_persisted_model(
        &self,
        session_id: &acp::SessionId,
        summary: &crate::session::persistence::Summary,
        initial_reasoning_effort: Option<ReasoningEffort>,
    ) {
        let session_id = session_id.clone();
        let persisted_model = summary.current_model_id.clone();
        let models = self.models_manager.models();
        let available = self.models_manager.available();
        self.session_registry.take_unavailable_model(&session_id);
        let resolved_catalog_key = resolve_catalog_key(&models, &persisted_model);
        tracing::debug!(
            session_id = %session_id.0,
            persisted = %persisted_model.0,
            resolved_catalog_key = ?resolved_catalog_key.as_ref().map(|k| k.0.as_ref()),
            available_count = available.len(),
            contains_persisted = available.contains_key(&persisted_model),
            available_keys = ?available.keys().take(10).collect::<Vec<_>>(),
            "load_session: restoring persisted model (debug)"
        );
        let is_echo_agent_build = persisted_model.0.starts_with("echo-agent-build");
        let same_family_fallback = if is_echo_agent_build {
            available
                .keys()
                .find(|id| id.0.starts_with("echo-agent-build"))
                .cloned()
        } else {
            available
                .keys()
                .find(|id| !id.0.starts_with("echo-agent-build"))
                .cloned()
        };
        let selectable_catalog_key =
            selectable_catalog_key_for_persisted(&models, &available, &persisted_model);
        let model_id = if let Some(catalog_key) = selectable_catalog_key {
            if catalog_key != persisted_model {
                tracing::info!(
                    session_id = %session_id.0,
                    persisted = %persisted_model.0,
                    catalog_key = %catalog_key.0,
                    "load_session: mapped persisted routing slug to catalog key"
                );
                echo_agent_telemetry::unified_log::info(
                    "load_session: mapped persisted routing slug to catalog key",
                    Some(session_id.0.as_ref()),
                    Some(serde_json::json!({
                        "persisted_model": persisted_model.0.as_ref(),
                        "catalog_key": catalog_key.0.as_ref(),
                    })),
                );
            }
            catalog_key
        } else if available.is_empty() {
            tracing::warn!(
                session_id = %session_id.0,
                persisted = %persisted_model.0,
                "load_session: model catalog empty at load; keeping persisted model unverified (catalog fetch may still be in flight)"
            );
            echo_agent_telemetry::unified_log::warn(
                "load_session: model catalog empty, keeping persisted model unverified",
                Some(session_id.0.as_ref()),
                Some(serde_json::json!({
                    "persisted_model": persisted_model.0.as_ref(),
                })),
            );
            persisted_model
        } else if let Some(fallback) = same_family_fallback {
            tracing::warn!(
                session_id = %session_id.0,
                previous = %persisted_model.0,
                new = %fallback.0,
                "Persisted model no longer available, auto-switching within family"
            );
            let reason = format!(
                "Model \"{}\" is no longer available for your account.",
                persisted_model.0,
            );
            self.send_model_auto_switched(&session_id, &persisted_model, &fallback, &reason)
                .await;
            fallback
        } else {
            let fallback = available
                .keys()
                .next()
                .cloned()
                .unwrap_or_else(|| persisted_model.clone());
            tracing::warn!(
                session_id = %session_id.0,
                previous = %persisted_model.0,
                fallback = %fallback.0,
                available_count = available.len(),
                available_keys = ?available.keys().take(10).collect::<Vec<_>>(),
                "Persisted model no longer available, no same-family fallback — blocking prompts for this session"
            );
            echo_agent_telemetry::unified_log::warn(
                "load_session: persisted model unavailable, no same-family fallback",
                Some(session_id.0.as_ref()),
                Some(serde_json::json!({
                    "persisted_model": persisted_model.0.as_ref(),
                    "fallback_model": fallback.0.as_ref(),
                    "available_count": available.len(),
                })),
            );
            let reason = format!(
                "Model \"{}\" is no longer available. Please start a new session.",
                persisted_model.0,
            );
            let empty_id = acp::ModelId::new(String::new());
            self.send_model_auto_switched(&session_id, &persisted_model, &empty_id, &reason)
                .await;
            self.session_registry
                .set_unavailable_model(&session_id, persisted_model.clone());
            fallback
        };
        tracing::debug!(
            session_id = %session_id.0,
            final_model_id = %model_id.0,
            "load_session: resolved final model_id for set_session_model"
        );
        {
            let _timer = crate::instrumentation_timer!("session.restore_model");
            let restore_effort = initial_reasoning_effort.or(summary.reasoning_effort);
            if let Err(err) = crate::agent::handlers::model_switch::apply(
                self,
                acp::SetSessionModelRequest::new(session_id.to_owned(), model_id),
                restore_effort,
            )
            .await
            {
                tracing::warn!(
                    session_id = %session_id.0,
                    error = ?err,
                    "load_session: restoring persisted model/effort failed; session keeps spawn defaults"
                );
            }
        }
    }
    /// Response phase: assemble the attach `_meta`, including the running
    /// prompt id a mid-turn loader adopts to pass the `session/update` gate.
    async fn build_attach_response_meta(
        &self,
        session_id: &acp::SessionId,
        summary: &crate::session::persistence::Summary,
        persist_data: Option<serde_json::Value>,
        code_restore_info: Option<serde_json::Value>,
    ) -> (acp::SessionModelState, serde_json::Value) {
        let session_id = session_id.clone();
        let mut response_meta_map = serde_json::Map::new();
        response_meta_map.insert("sessionId".to_string(), serde_json::json!(session_id));
        if let Some(persist) = persist_data {
            response_meta_map.insert("echo.agent/persist".to_string(), persist);
        }
        let session_cwd = self
            .resident_handle(&session_id)
            .map(|h| h.info.cwd.clone());
        let indexed_roots = session_cwd
            .as_deref()
            .map(|c| self.indexed_roots_for(std::path::Path::new(c)))
            .unwrap_or_default();
        response_meta_map.insert(
            "codebaseIndexed".to_string(),
            serde_json::json!(indexed_roots),
        );
        if summary.head_commit.is_some()
            && let Some(ref cwd) = session_cwd
            && summary.git_root_dir.as_deref().is_none_or(|root| {
                echo_agent_workspace::session::git::find_git_root_from_path(std::path::Path::new(
                    cwd.as_str(),
                ))
                .ok()
                .is_some_and(|current_root| current_root == std::path::Path::new(root))
            })
        {
            let mut git_scan_timer = crate::instrumentation_timer!("session.git_divergence");
            git_scan_timer.with_subphase(echo_agent_telemetry::startup::Subphase::SessionGitScan);
            let cwd_path = std::path::Path::new(cwd.as_str());
            let current_head =
                echo_agent_workspace::session::git::git_cli(cwd_path, &["rev-parse", "HEAD"])
                    .await
                    .ok();
            if let Some(divergence) = echo_agent_workspace::session::git::detect_head_divergence(
                summary.head_commit.as_deref(),
                summary.head_branch.as_deref(),
                current_head.as_deref(),
            ) {
                response_meta_map
                    .insert("gitDivergence".to_string(), serde_json::json!(divergence));
            }
        }
        if let Some(info) = code_restore_info {
            response_meta_map.insert("codeRestore".to_string(), info);
        }
        if let Some(running_prompt_id) = self
            .resident_handle(&session_id)
            .and_then(|h| h.current_prompt_id.lock().ok().and_then(|g| g.clone()))
        {
            response_meta_map.insert(
                "echo.agent/runningPromptId".to_string(),
                serde_json::json!(running_prompt_id),
            );
        }
        let model_state = self.model_state(Some(&session_id));
        self.insert_session_config_meta(
            &mut response_meta_map,
            &session_id,
            session_cwd.clone().unwrap_or_default(),
            summary.display_title_opt(),
            &model_state,
        );
        let applied_tool_overrides = {
            let cmd_tx = self
                .resident_handle(&session_id)
                .map(|handle| handle.cmd_tx.clone());
            match cmd_tx {
                Some(cmd_tx) => read_applied_tool_overrides(&cmd_tx).await,
                None => {
                    tracing::warn!(
                        session_id = %session_id.0,
                        "session/load toolOverrides echo: session handle not found"
                    );
                    None
                }
            }
        };
        insert_applied_tool_overrides(&mut response_meta_map, applied_tool_overrides.as_ref());
        let response_meta = serde_json::Value::Object(response_meta_map);
        (model_state, response_meta)
    }
    pub(super) async fn resume_session_inner(
        &self,
        args: acp::ResumeSessionRequest,
    ) -> Result<acp::ResumeSessionResponse, acp::Error> {
        tracing::info!(session_id = %args.session_id.0, "session/resume");
        if !args.additional_directories.is_empty() {
            return Err(acp::Error::invalid_params().data(RESUME_REFUSES_EXTRA_DIRS));
        }
        if crate::agent::chat_modes::process_chat_mode_enabled()
            || ChatKindClaim::from_meta(args.meta.as_ref()).resolve(self, &args.session_id)
                == SessionKind::Chat
        {
            return Err(acp::Error::invalid_params().data(RESUME_REFUSES_CHAT));
        }
        let loaded = self
            .attach_session(load_request_for_resume(args), AttachOperation::Resume)
            .await?;
        Ok(acp::ResumeSessionResponse::new()
            .modes(loaded.modes)
            .models(loaded.models)
            .config_options(loaded.config_options)
            .meta(loaded.meta))
    }
    /// Closing an inactive session succeeds: the spec permits either, and
    /// closes race disconnect-driven eviction routinely.
    pub(super) async fn close_session_inner(
        &self,
        args: acp::CloseSessionRequest,
    ) -> Result<acp::CloseSessionResponse, acp::Error> {
        let outcome = self.close_active_session(&args.session_id).await;
        tracing::info!(
            session_id = %args.session_id.0,
            ?outcome,
            "session/close"
        );
        let mut meta = acp::Meta::new();
        meta.insert(
            "echo.agent/closeOutcome".to_string(),
            serde_json::json!(outcome.wire_str()),
        );
        Ok(acp::CloseSessionResponse::new().meta(meta))
    }
}
/// Reshape a resume into the load request that backs it. Policy is not encoded
/// here: it rides on [`AttachOperation::Resume`], so a client cannot spoof it
/// and a reader does not have to trace a `_meta` key to find it.
pub(super) fn load_request_for_resume(args: acp::ResumeSessionRequest) -> acp::LoadSessionRequest {
    let acp::ResumeSessionRequest {
        session_id,
        cwd,
        mcp_servers,
        meta,
        ..
    } = args;
    acp::LoadSessionRequest::new(session_id, cwd)
        .mcp_servers(mcp_servers)
        .meta(meta.unwrap_or_default())
}
