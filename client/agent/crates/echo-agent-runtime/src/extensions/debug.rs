//! `echo.agent/debug/*` extension handlers for local client testing.
//!
//! These methods bypass heuristics, sampling, cooldowns, and enabled checks
//! so client engineers can exercise notification → response flows without
//! needing real experiments, real sessions, or real model inference.
//!
//! - `trigger_feedback`: fire a synthetic `FeedbackRequestNotification`.
//! - `arm_auto_compact`: arm the next turn to unconditionally trigger
//!   auto-compaction, regardless of context window usage.
//! - `agent`: agent-process diagnostics (registry counts).

use agent_client_protocol as acp;

use super::{ExtResult, parse_params};
use crate::agent::MvpAgent;
use crate::session::{ExtMethodResult, SessionCommand};

#[tracing::instrument(skip_all, fields(method = %args.method))]
pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        "echo.agent/debug/trigger_feedback" => {
            tracing::info!("debug: triggering test feedback request");
            handle_trigger_feedback(agent, args).await
        }
        "echo.agent/debug/arm_auto_compact" => handle_arm_auto_compact(agent, args),
        "echo.agent/debug/agent" => handle_agent(agent).await,
        _ => Err(acp::Error::method_not_found()),
    }
}

async fn handle_agent(agent: &MvpAgent) -> ExtResult {
    let registries = agent.registry_snapshot().await;
    ExtMethodResult::success(serde_json::json!({ "registries": registries }))
        .to_ext_response()
        .map_err(|e| acp::Error::internal_error().data(e.to_string()))
}

async fn handle_trigger_feedback(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    use crate::session::feedback::{FeedbackMode, FeedbackTier};

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct DebugTriggerParams {
        #[serde(alias = "session_id")]
        session_id: String,
        /// "tier1" | "tier2" | "tier3" (default: "tier1")
        #[serde(default)]
        tier: Option<String>,
        /// "thumbs" | "stars" | "text" | "thumbs_text" | "stars_text" (default: "thumbs_text")
        #[serde(default)]
        mode: Option<String>,
    }

    let params: DebugTriggerParams = parse_params(args)?;

    let tier = match params.tier.as_deref() {
        Some("tier2") => FeedbackTier::Tier2,
        Some("tier3") => FeedbackTier::Tier3,
        Some("tier1") | None => FeedbackTier::Tier1,
        Some(other) => {
            return Err(acp::Error::invalid_params().data(format!(
                "unknown tier: {other:?} (expected tier1/tier2/tier3)"
            )));
        }
    };

    let mode = match params.mode.as_deref() {
        Some("thumbs") => FeedbackMode::Thumbs,
        Some("stars") => FeedbackMode::Stars,
        Some("text") => FeedbackMode::Text,
        Some("stars_text") => FeedbackMode::StarsText,
        Some("thumbs_text") | None => FeedbackMode::ThumbsText,
        Some(other) => {
            return Err(acp::Error::invalid_params().data(format!(
                "unknown mode: {other:?} (expected thumbs/stars/text/thumbs_text/stars_text)"
            )));
        }
    };

    let session_id = acp::SessionId::new(params.session_id.clone());
    let handle = agent.resident_handle(&session_id).ok_or_else(|| {
        acp::Error::invalid_params().data(format!("session not found: {}", params.session_id))
    })?;

    let (tx, rx) = tokio::sync::oneshot::channel();
    handle
        .cmd_tx
        .send(SessionCommand::TriggerTestFeedback {
            tier,
            mode,
            respond_to: tx,
        })
        .map_err(|_| {
            acp::Error::internal_error().data("failed to dispatch debug trigger to session")
        })?;

    rx.await
        .map_err(|_| acp::Error::internal_error().data("session failed to respond"))?
        .map_err(|e| acp::Error::internal_error().data(format!("Internal error: {e:?}")))
}

fn handle_arm_auto_compact(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let params: serde_json::Value = parse_params(args)?;

    let session_id_str = params["sessionId"]
        .as_str()
        .or_else(|| params["session_id"].as_str())
        .ok_or_else(|| acp::Error::invalid_params().data("sessionId required"))?;
    let session_id = acp::SessionId::new(session_id_str);

    let handle = agent
        .resident_handle(&session_id)
        .ok_or_else(|| acp::Error::invalid_params().data("unknown session id"))?;

    handle
        .force_compact
        .store(true, std::sync::atomic::Ordering::Relaxed);

    tracing::info!(
        session_id = %session_id_str,
        "debug: armed auto-compact for next turn"
    );

    ExtMethodResult::success(serde_json::json!({ "armed": true }))
        .to_ext_response()
        .map_err(|e| acp::Error::internal_error().data(e.to_string()))
}
