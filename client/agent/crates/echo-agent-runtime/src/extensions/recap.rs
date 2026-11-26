//! `echo.agent/recap` extension handler.
//!
//! Triggers generation of a session recap — a short "where was I" summary of
//! the session so far — via [`SessionCommand::Recap`]. This is fire-and-forget:
//! the recap is delivered asynchronously to every attached client as a
//! [`SessionUpdate::SessionRecap`](crate::extensions::notification::SessionUpdate::SessionRecap)
//! notification, so the handler returns as soon as the command is queued rather
//! than blocking on the model call.
//!
//! Invoked on demand via the `/recap` slash command (`auto = false`) and
//! automatically when the user returns to the terminal after being away
//! (`auto = true`).

use agent_client_protocol as acp;

use super::{ExtResult, parse_params, to_ext_response};
use crate::agent::MvpAgent;
use crate::session::SessionCommand;

#[tracing::instrument(skip_all)]
pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct RecapRequest {
        session_id: String,
        #[serde(default)]
        auto: bool,
    }

    let req: RecapRequest = parse_params(args)?;
    tracing::info!(auto = req.auto, "handling /recap request");

    // Feature gate: remote setting / `[features] session_recap`
    // config.toml key / `ECHO_AGENT_SESSION_RECAP` env (default ON). Gates both the
    // manual `/recap` and the automatic recap.
    if !agent.cfg.borrow().is_session_recap_enabled() {
        tracing::debug!("session recap disabled by config/feature flag; ignoring request");
        return to_ext_response(Ok(serde_json::json!({ "ok": true, "disabled": true })));
    }

    let sid: acp::SessionId = req.session_id.clone().into();
    // Load-race-tolerant: an automatic recap fires on return-from-away, when a
    // leader restart may have a reconnect-replayed `session/load` still in
    // flight. Wait for that load instead of failing with "session not found".
    let Some(session) = agent.session_handle_waiting_for_load(&sid).await else {
        return Err(
            acp::Error::invalid_params().data(format!("session not found: {}", req.session_id))
        );
    };

    // Fire-and-forget: the recap is emitted later as a SessionRecap
    // notification. We only ack that the request was accepted.
    let _ = session
        .cmd_tx
        .send(SessionCommand::Recap { auto: req.auto });

    to_ext_response(Ok(serde_json::json!({ "ok": true })))
}
