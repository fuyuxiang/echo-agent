//! `echo.agent/memory/flush`, `echo.agent/memory/dream`, `echo.agent/memory/rewrite`, and `echo.agent/compact_conversation`
//! extension handlers.
//!
//! - `compact_conversation`: trigger an on-demand compaction for a session.
//! - `memory/flush`: trigger an on-demand memory flush for a session.
//! - `memory/dream`: consolidate session summaries into long-term memory.
//! - `memory/rewrite`: rewrite a raw memory note into structured markdown via
//!   a one-shot LLM call.

use agent_client_protocol as acp;
use serde::Deserialize;
use tokio::sync::oneshot;

use super::{ExtResult, parse_params, to_ext_response, to_raw_response};
use crate::agent::MvpAgent;
use crate::session::{CompactConversationRequest, CompactConversationResponse, SessionCommand};

#[tracing::instrument(skip_all, fields(method = %args.method))]
pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        m if m.starts_with("echo.agent/compact_conversation") => handle_compact(agent, args).await,
        "echo.agent/memory/flush" => handle_flush(agent, args).await,
        "echo.agent/memory/dream" => handle_dream(agent, args).await,
        "echo.agent/memory/rewrite" => handle_rewrite(agent, args).await,
        _ => Err(acp::Error::method_not_found()),
    }
}

async fn handle_compact(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let req: CompactConversationRequest = parse_params(args)?;
    // send over the compact query here properly
    let sid: acp::SessionId = req.session_id.into();
    let session_handle = agent.resident_handle(&sid);
    let (tx, rx) = oneshot::channel();
    if let Some(session) = session_handle {
        let _ = session.cmd_tx.send(SessionCommand::CompactSession {
            user_context: req.user_context,
            respond_to: tx,
        });
    }
    rx.await
        .map_err(|_| acp::Error::internal_error().data("session failed to respond"))?
        .map_err(|e| acp::Error::internal_error().data(format!("Internal error: {:?}", e)))?;
    to_raw_response(&CompactConversationResponse {})
}

async fn handle_flush(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    #[derive(Deserialize)]
    struct MemoryFlushRequest {
        session_id: String,
    }

    let req: MemoryFlushRequest = parse_params(args)?;
    let not_found_err = format!("session not found: {}", req.session_id);
    let sid: acp::SessionId = req.session_id.into();
    let Some(session) = agent.resident_handle(&sid) else {
        return Err(acp::Error::invalid_params().data(not_found_err));
    };
    let (tx, rx) = oneshot::channel();
    let _ = session
        .cmd_tx
        .send(SessionCommand::FlushMemory { respond_to: tx });
    let completed = rx
        .await
        .map_err(|_| acp::Error::internal_error().data("session failed to respond"))?
        .map_err(|e| acp::Error::internal_error().data(format!("{:?}", e)))?;
    to_ext_response(Ok(serde_json::json!({ "completed": completed })))
}

async fn handle_dream(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    #[derive(Deserialize)]
    struct MemoryDreamRequest {
        session_id: String,
    }

    let req: MemoryDreamRequest = parse_params(args)?;
    let not_found_err = format!("session not found: {}", req.session_id);
    let sid: acp::SessionId = req.session_id.into();
    let Some(session) = agent.resident_handle(&sid) else {
        return Err(acp::Error::invalid_params().data(not_found_err));
    };
    let (tx, rx) = oneshot::channel();
    let _ = session
        .cmd_tx
        .send(SessionCommand::DreamMemory { respond_to: tx });
    let changed = rx
        .await
        .map_err(|_| acp::Error::internal_error().data("session failed to respond"))?
        .map_err(|error| acp::Error::internal_error().data(format!("{error:?}")))?;
    to_ext_response(Ok(serde_json::json!({ "changed": changed })))
}

async fn handle_rewrite(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct RewriteRequest {
        session_id: String,
        raw_text: String,
        context_summary: String,
    }

    let req: RewriteRequest = parse_params(args)?;
    let not_found_err = format!("session not found: {}", req.session_id);
    let sid: acp::SessionId = req.session_id.into();
    let Some(session) = agent.resident_handle(&sid) else {
        return Err(acp::Error::invalid_params().data(not_found_err));
    };
    let (tx, rx) = oneshot::channel();
    let _ = session.cmd_tx.send(SessionCommand::RewriteMemoryNote {
        raw_text: req.raw_text,
        context_summary: req.context_summary,
        respond_to: tx,
    });
    let rewritten = rx
        .await
        .map_err(|_| acp::Error::internal_error().data("session failed to respond"))?
        .map_err(|e| acp::Error::internal_error().data(e))?;
    to_raw_response(&serde_json::json!({ "rewritten": rewritten }))
}
