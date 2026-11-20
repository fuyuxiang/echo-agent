//! `echo.agent/rewind/*` extension handlers.
//!
//! - `rewind/execute`: rewind a session to a target prompt index, optionally
//!   forcing past in-flight prompts and choosing a `RewindMode`.
//! - `rewind/points`: list the prompt indices that can be rewound to.
//!
//! Local mode dispatches [`handle`]. In gateway-bridge mode the agent's
//! routing hook calls [`handle_bridge`], which composes the server's
//! conversation rewind with the local file half so the pager-facing ACP
//! response is identical either way.
use super::{ExtResult, parse_params, to_raw_response};
use crate::agent::MvpAgent;
use crate::session::handle::SessionHandle;
use crate::session::{RewindFileGuard, RewindMode, RewindRequest, SessionCommand};
use agent_client_protocol as acp;
use serde::Deserialize;
use tokio::sync::oneshot;
#[tracing::instrument(skip_all, fields(method = %args.method))]
pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    tracing::info!("handling rewind request: {}", args.method);
    match args.method.as_ref() {
        "echo.agent/rewind/execute" => handle_execute(agent, args).await,
        "echo.agent/rewind/points" => handle_points(agent, args).await,
        _ => Err(acp::Error::method_not_found()),
    }
}
#[derive(Deserialize)]
struct RewindSessionRequest {
    #[serde(alias = "sessionId")]
    session_id: String,
    #[serde(default, alias = "targetPromptIndex")]
    target_prompt_index: Option<usize>,
    #[serde(default, alias = "targetResponseId")]
    target_response_id: Option<String>,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    mode: Option<RewindMode>,
    #[serde(default, alias = "fileGuard")]
    file_guard: Option<RewindFileGuard>,
}
impl RewindSessionRequest {
    fn prompt_index_for_local(&self) -> Result<usize, acp::Error> {
        if let Some(idx) = self.target_prompt_index {
            return Ok(idx);
        }
        if response_id_from_req(self).is_some() {
            return Err(
                acp::Error::invalid_params()
                    .data(
                        "targetResponseId rewind requires a chat/bridge session (use targetPromptIndex for local)",
                    ),
            );
        }
        Err(acp::Error::invalid_params().data("targetPromptIndex or targetResponseId is required"))
    }
}
#[derive(Deserialize)]
struct RewindPointsRequest {
    #[serde(alias = "sessionId")]
    session_id: String,
}
/// Look up a `SessionHandle` by id string, or return a `resource_not_found`
/// `acp::Error`. Used by both arms below.
fn lookup_session(agent: &MvpAgent, session_id: String) -> Result<SessionHandle, acp::Error> {
    agent
        .resident_handle(&acp::SessionId::new(session_id))
        .ok_or_else(|| acp::Error::resource_not_found(Some("session not found".into())))
}
async fn handle_execute(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let request: RewindSessionRequest = parse_params(args)?;
    let target_prompt_index = request.prompt_index_for_local()?;
    let handle = lookup_session(agent, request.session_id)?;
    let (tx, rx) = oneshot::channel();
    handle
        .cmd_tx
        .send(SessionCommand::Rewind {
            request: RewindRequest {
                target_prompt_index,
                force: request.force,
                mode: request.mode.unwrap_or(RewindMode::All),
                file_guard: request.file_guard,
            },
            respond_to: tx,
        })
        .map_err(|_| acp::Error::internal_error().data("failed to send rewind command"))?;
    let result = rx
        .await
        .map_err(|_| acp::Error::internal_error().data("session failed to respond"))?
        .map_err(|e| acp::Error::internal_error().data(format!("Rewind failed: {:?}", e)))?;
    to_raw_response(&result)
}
async fn handle_points(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let request: RewindPointsRequest = parse_params(args)?;
    let handle = lookup_session(agent, request.session_id)?;
    let (tx, rx) = oneshot::channel();
    handle
        .cmd_tx
        .send(SessionCommand::GetRewindPoints { respond_to: tx })
        .map_err(|_| acp::Error::internal_error().data("failed to send command"))?;
    let result = rx
        .await
        .map_err(|_| acp::Error::internal_error().data("session failed to respond"))?;
    to_raw_response(&result)
}
fn response_id_from_req(req: &RewindSessionRequest) -> Option<&str> {
    req.target_response_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
}
