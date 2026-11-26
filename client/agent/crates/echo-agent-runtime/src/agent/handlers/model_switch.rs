//! Applies a model switch to a session — the ungated path. `set_session_model`
//! enforces the `allowed_models` gate before delegating here; internal callers
//! (`new_session`, `load_session`) call `apply` directly.
use crate::agent::config;
use crate::agent::mvp_agent::reasoning_effort::EffortTarget;
use crate::agent::mvp_agent::{
    MvpAgent, agent_name_after_model_switch, harnesses_are_compatible, resolve_required_agent_type,
};
use crate::session::SessionCommand;
use agent_client_protocol::{self as acp};
use echo_agent_sampling_types::ReasoningEffort;
use tokio::sync::oneshot;
/// Apply a model switch to a session (no gate — `set_session_model` gates first).
pub(crate) async fn apply(
    agent: &MvpAgent,
    args: acp::SetSessionModelRequest,
    effort_override: Option<ReasoningEffort>,
) -> Result<acp::SetSessionModelResponse, acp::Error> {
    tracing::info!("Received set session model request {args:?}");
    echo_agent_telemetry::unified_log::info(
        "model changed",
        Some(args.session_id.0.as_ref()),
        Some(serde_json::json!({"model": args.model_id.0.as_ref()})),
    );
    tracing::debug!("session_session_model::mvp_agent: {:?}", &args);
    let acp::SetSessionModelRequest {
        session_id,
        model_id,
        ..
    } = args;
    let handle = agent
        .session_handle_waiting_for_load(&session_id)
        .await
        .ok_or_else(|| acp::Error::invalid_params().data("unknown session id"))?;
    let model = agent.resolve_model_id(&model_id)?;
    let use_concise = model.info().use_concise;
    let session_default = handle
        .session_default_agent_profile
        .as_deref()
        .unwrap_or(&handle.agent_name);
    let required_agent_type =
        resolve_required_agent_type(Some(model.info().agent_type.as_str()), session_default);
    let previous_model_id = handle.model_id.0.clone();
    let model_unchanged = previous_model_id == model_id.0;
    let is_family_switch = {
        let models = agent.models_manager.models();
        let old_family = config::find_model_by_id(&models, &previous_model_id)
            .and_then(|e| e.info.model_family.as_deref());
        matches!(
            (old_family, model.info().model_family.as_deref()),
            (Some(a), Some(b)) if a != b
        )
    };
    let mut pending_rebuild_definition: Option<echo_agent_core::AgentDefinition> = None;
    {
        let required = &required_agent_type;
        let turn_count = handle
            .signals_handle
            .snapshot()
            .await
            .map(|s| s.turn_count)
            .unwrap_or(0);
        let (agent_tx, agent_rx) = oneshot::channel();
        let _ = handle.cmd_tx.send(SessionCommand::GetActiveAgent {
            responds_to: agent_tx,
        });
        let active_agent_type = agent_rx.await.ok().flatten();
        let is_mismatch = active_agent_type.as_ref().is_some_and(|active| {
            // Automation modes install their own profile over the already
            // selected stock model. Restoring/reapplying that exact model
            // must keep the profile and its prompt. A different model or a
            // newly strict harness requirement still uses the normal gate.
            let automation_reapply = model_unchanged
                && matches!(active.as_str(), "browser-use" | "computer-use")
                && !echo_agent_core::config::is_strict_harness_agent_type(required);
            !automation_reapply && !harnesses_are_compatible(active, required)
        });
        tracing::info!(
            session_id = %session_id.0,
            model_id = %model_id.0,
            ?required_agent_type,
            ?active_agent_type,
            turn_count,
            is_mismatch,
            "set_session_model: agent type compatibility check"
        );
        if is_mismatch && turn_count > 0 {
            tracing::warn!(
                session_id = %session_id.0,
                model_id = %model_id.0,
                active_agent = ?active_agent_type,
                required_agent = %required,
                turn_count,
                "set_session_model: agent type mismatch rejected"
            );
            echo_agent_telemetry::session_ctx::log_event(
                echo_agent_telemetry::events::ModelSwitched {
                    session_id: session_id.0.to_string(),
                    previous_model_id: previous_model_id.to_string(),
                    new_model_id: model_id.0.to_string(),
                    success: false,
                    error_code: Some(config::MODEL_SWITCH_INCOMPATIBLE_AGENT.to_string()),
                    required_agent_type: Some(required.clone()),
                    current_agent_type: active_agent_type.clone(),
                },
            );
            let err_payload = config::ModelSwitchIncompatibleAgentError {
                code: config::MODEL_SWITCH_INCOMPATIBLE_AGENT.to_string(),
                active_agent_type: active_agent_type.unwrap_or_else(|| "unknown".to_owned()),
                required_agent_type: required.clone(),
                model_id: model_id.0.to_string(),
                suggestion: "start_new_session".to_string(),
            };
            return Err(err_payload.into_acp_error());
        }
        if is_mismatch && turn_count == 0 {
            let cwd = handle.tool_context.cwd.as_path();
            let resolved = echo_agent_core::discovery::by_name_in_cwd_with_plugins(
                required,
                cwd,
                agent.plugin_registry_handle.snapshot().as_deref(),
            );
            match resolved {
                Some(def) => {
                    tracing::info!(
                        session_id = %session_id.0,
                        model_id = %model_id.0,
                        required_agent_type = %required,
                        agent_def_name = %def.name,
                        "set_session_model: zero-turn harness switch — queued agent rebuild"
                    );
                    pending_rebuild_definition = Some(def);
                }
                None => {
                    tracing::warn!(
                        session_id = %session_id.0,
                        model_id = %model_id.0,
                        required_agent_type = %required,
                        "set_session_model: zero-turn harness switch — could not resolve agent definition; proceeding with stale harness"
                    );
                }
            }
        }
    }
    let mut model_sampling =
        agent.prepare_sampling_config_for_model(&model, handle.origin_client.clone());
    agent.models_manager.apply_supported_effort(
        &mut model_sampling,
        effort_override,
        &session_id,
        EffortTarget::ModelSwitch,
    );
    let applied_effort = model_sampling.reasoning_effort;
    let gate_closed = !handle
        .gateway_enabled
        .load(std::sync::atomic::Ordering::Relaxed);
    let apply_prompt_override = !gate_closed;
    if gate_closed {
        tracing::info!(
            session_id = %session_id.0,
            model_id = %model_id.0,
            "set_session_model: gateway gate closed, prompt override suppressed"
        );
        pending_rebuild_definition = None;
    }
    let did_rebuild = if let Some(def) = pending_rebuild_definition {
        let (rebuild_tx, rebuild_rx) = oneshot::channel();
        let _ = handle
            .cmd_tx
            .send(SessionCommand::RebuildAgentForDefinition {
                definition: def,
                responds_to: rebuild_tx,
            });
        let rebuild_result = rebuild_rx
            .await
            .map_err(|_| acp::Error::internal_error().data("rebuild_agent: actor closed"))?;
        match rebuild_result {
            Ok(()) => true,
            Err(e) => {
                tracing::error!(
                    session_id = %session_id.0,
                    model_id = %model_id.0,
                    error = ?e,
                    "set_session_model: zero-turn harness rebuild failed; aborting model switch"
                );
                echo_agent_telemetry::session_ctx::log_event(
                    echo_agent_telemetry::events::ModelSwitched {
                        session_id: session_id.0.to_string(),
                        previous_model_id: previous_model_id.to_string(),
                        new_model_id: model_id.0.to_string(),
                        success: false,
                        error_code: Some(config::MODEL_SWITCH_REBUILD_FAILED.to_string()),
                        required_agent_type: Some(required_agent_type.clone()),
                        current_agent_type: None,
                    },
                );
                return Err(e);
            }
        }
    } else {
        false
    };
    let new_threshold = {
        let cfg = agent.cfg.borrow();
        let models = agent.models_manager.models();
        let model = config::find_model_by_id(&models, model_sampling.model.as_str());
        crate::util::config::resolve_auto_compact_threshold_percent(
            &cfg,
            model_sampling.model.as_str(),
            model.map(|e| &e.info),
        )
    };
    let (tx, rx) = oneshot::channel();
    let _ = handle.cmd_tx.send(SessionCommand::SetSessionModel {
        model_id: model_id.clone(),
        sampling_config: model_sampling,
        use_concise,
        is_family_switch,
        apply_prompt_override,
        skip_prompt_rewrite: did_rebuild || model_unchanged,
        auto_compact_threshold_percent: new_threshold,
        responds_to: tx,
    });
    let updated_model = rx
        .await
        .map_err(|_| acp::Error::internal_error().data("failed to set session model"))?;
    agent.with_resident_mut(&session_id, |handle| {
        handle.model_id = model_id.clone();
        handle.reasoning_effort = applied_effort;
        handle.agent_name =
            agent_name_after_model_switch(did_rebuild, &required_agent_type, &handle.agent_name);
    });
    broadcast_model_changed(
        agent,
        &session_id,
        model_id.0.as_ref(),
        applied_effort.map(|eff| eff.to_string()),
    );
    echo_agent_telemetry::session_ctx::log_event(echo_agent_telemetry::events::ModelSwitched {
        session_id: session_id.0.to_string(),
        previous_model_id: previous_model_id.to_string(),
        new_model_id: model_id.0.to_string(),
        success: true,
        error_code: None,
        required_agent_type: Some(required_agent_type.clone()),
        current_agent_type: None,
    });
    if agent.cfg.borrow().mode != config::AgentMode::Leader {
        agent.models_manager.set_current_model_id(model_id.clone());
        agent
            .models_manager
            .set_current_reasoning_effort(applied_effort);
    }
    agent.sync_process_static_api_key(Some(model_id.0.as_ref()));
    Ok(acp::SetSessionModelResponse::new().meta(
        serde_json::json!({
            "model": updated_model,
        })
        .as_object()
        .cloned(),
    ))
}
/// Broadcast a `ModelChanged` to every client subscribed to this session so
/// followers mirror the new model. The originating client ignores its own echo
/// (gated by `model_switch_pending`). Broadcast-only — no eventId, not persisted.
fn broadcast_model_changed(
    agent: &MvpAgent,
    session_id: &acp::SessionId,
    model_id: &str,
    reasoning_effort: Option<String>,
) {
    let notification = crate::extensions::notification::SessionNotification {
        session_id: session_id.clone(),
        update: crate::extensions::notification::SessionUpdate::ModelChanged {
            model_id: model_id.to_owned(),
            reasoning_effort,
        },
        meta: None,
    };
    if let Ok(params) = serde_json::value::to_raw_value(&notification) {
        agent
            .gateway
            .forward_fire_and_forget(acp::ExtNotification::new(
                "echo.agent/session_notification",
                params.into(),
            ));
    }
}
