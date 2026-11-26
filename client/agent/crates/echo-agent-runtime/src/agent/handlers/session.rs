//! Session meta-information handlers.
//!
//! Router pattern: single `handle()` dispatches by method name.
//! Business logic delegates to pure functions or MvpAgent methods.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use agent_client_protocol::{self as acp};
use serde::Deserialize;

use super::super::mvp_agent::MvpAgent;
use crate::session::persistence::{Summary, list_recent_summaries, list_summaries};
use crate::session::{
    AllSessionOverviewRequest, AllSessionOverviewResponse, ContextInfo, ExtMethodResult,
    SessionCommand, SessionInfoData, SessionInfoResponse, SessionListRequest, SessionListResponse,
};

/// Mirrors the display title (`generated_title`, else `session_summary`) into
/// `session_summary` so clients that only read that field show the same title
/// as `display_title()` — including after a `/rename` that updated only
/// `generated_title`. Mutates the response copy only; never persisted.
fn backfill_session_summary(summary: &mut Summary) {
    let display = summary.display_title().to_owned();
    if !display.is_empty() && display != summary.session_summary {
        summary.session_summary = display;
    }
}

/// Router for echo.agent/session/* and echo.agent/session_summaries/* methods.
pub(crate) async fn handle(
    agent: &MvpAgent,
    args: &acp::ExtRequest,
) -> Result<acp::ExtResponse, acp::Error> {
    match args.method.as_ref() {
        "echo.agent/session/info" => handle_session_info(agent, args).await,
        "echo.agent/session/close" => handle_session_close(agent, args).await,
        "echo.agent/session/list" => handle_session_list(agent, args).await,
        "echo.agent/sessions/list" => handle_roster_list(agent, args).await,
        m if m.starts_with("echo.agent/session_summaries/") => {
            handle_session_summaries(agent, args).await
        }
        _ => Err(acp::Error::method_not_found()),
    }
}

/// `echo.agent/sessions/list` — the FleetView roster. Returns every
/// resident session plus recently-touched on-disk `Dormant` sessions. Clients
/// poll this while the dashboard is open and reconcile against the
/// `echo.agent/sessions/changed` broadcast.
async fn handle_roster_list(
    agent: &MvpAgent,
    _args: &acp::ExtRequest,
) -> Result<acp::ExtResponse, acp::Error> {
    let sessions = agent.build_roster().await;
    ExtMethodResult::success(crate::agent::roster::RosterListResponse { sessions })
        .to_ext_response()
        .map_err(|e| acp::Error::internal_error().data(e.to_string()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionInfoRequest {
    session_id: Option<String>,
}

#[derive(Deserialize)]
struct RecentSessionsRequest {
    limit: usize,
}

async fn handle_session_info(
    agent: &MvpAgent,
    args: &acp::ExtRequest,
) -> Result<acp::ExtResponse, acp::Error> {
    let req: SessionInfoRequest = serde_json::from_str(args.params.get())
        .map_err(|e| acp::Error::invalid_params().data(format!("invalid params: {e}")))?;

    let session_id = req.session_id.or_else(|| {
        agent
            .resident_ids()
            .into_iter()
            .next()
            .map(|id| id.0.to_string())
    });

    let Some(session_id) = session_id else {
        return ExtMethodResult::success(serde_json::json!({}))
            .to_ext_response()
            .map_err(|e| acp::Error::internal_error().data(e.to_string()));
    };

    let sid = acp::SessionId::new(session_id.clone());
    let Some(session) = agent.resident_handle(&sid) else {
        return ExtMethodResult::success(serde_json::json!({}))
            .to_ext_response()
            .map_err(|e| acp::Error::internal_error().data(e.to_string()));
    };

    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = session
        .cmd_tx
        .send(SessionCommand::GetSessionInfo { responds_to: tx });
    let info = rx.await.ok();

    // Construct display data for `/session-info`.
    let mut data = info.unwrap_or_else(|| SessionInfoData {
        agent_name: None,
        model: None,
        model_display_name: None,
        resolved_model_id: None,
        model_fingerprint: None,
        show_model_fingerprint: false,
        api_backend: None,
        conversation_id: None,
        turns: 0,
        turn_index: 0,
        context: ContextInfo {
            auto_compact_threshold_percent:
                crate::util::config::DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
            ..ContextInfo::default()
        },
    });

    // Session identity follows the configured catalog entry. The sampler's
    // upstream model name can be shared by different providers or accounts.
    data.model = Some(session.model_id.0.to_string());
    // Calculate the model's display name.
    data.model_display_name = agent
        .models_manager
        .models()
        .get(session.model_id.0.as_ref())
        .and_then(|entry| entry.info.name.clone());

    // Construct `SessionInfoResponse`.
    let response = SessionInfoResponse {
        session_id,
        cwd: session.info.cwd.clone(),
        data,
    };

    // Wrap `SessionInfoResponse` in `ExtMethodResult` and return it.
    ExtMethodResult::success(serde_json::to_value(&response).unwrap_or_default())
        .to_ext_response()
        .map_err(|e| acp::Error::internal_error().data(e.to_string()))
}

async fn handle_session_close(
    agent: &MvpAgent,
    args: &acp::ExtRequest,
) -> Result<acp::ExtResponse, acp::Error> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct CloseRequest {
        session_id: String,
    }

    let req: CloseRequest = serde_json::from_str(args.params.get())
        .map_err(|e| acp::Error::invalid_params().data(format!("invalid params: {e}")))?;

    let sid = acp::SessionId::new(req.session_id);
    let outcome = agent.close_active_session(&sid).await;
    tracing::info!(
        session_id = %sid.0,
        ?outcome,
        "echo.agent/session/close"
    );

    // `success` stays for existing callers; `outcome` says what the close
    // actually did (`closed` / `notResident` / `superseded`).
    ExtMethodResult::success(serde_json::json!({
        "success": true,
        "outcome": outcome.wire_str(),
    }))
    .to_ext_response()
    .map_err(|e| acp::Error::internal_error().data(e.to_string()))
}

async fn handle_session_summaries(
    _agent: &MvpAgent,
    args: &acp::ExtRequest,
) -> Result<acp::ExtResponse, acp::Error> {
    match args.method.as_ref() {
        "echo.agent/session_summaries/session_list" => {
            let req = serde_json::from_str::<SessionListRequest>(args.params.get())?;
            let cwd = req.workspace_directory.to_string_lossy().to_string();

            let _timer = crate::instrumentation_timer!("session.list_sessions_for_workspace");

            let mut summaries = list_summaries(Some(&cwd)).await.map_err(|e| {
                acp::Error::internal_error().data(format!("failed to list sessions: {e}"))
            })?;
            for s in &mut summaries {
                backfill_session_summary(s);
            }

            let value = serde_json::to_value(SessionListResponse {
                session_summaries: summaries,
            })
            .map(|v| serde_json::value::to_raw_value(&v).map(Arc::from))
            .expect("to work")
            .expect("to work");

            Ok(acp::ExtResponse::new(value))
        }
        "echo.agent/session_summaries/workspace_list" => {
            tracing::debug!("echoagent/session_summaries/workspace_list is working");
            let _req = serde_json::from_str::<AllSessionOverviewRequest>(args.params.get())?;

            let _timer = crate::instrumentation_timer!("session.list_sessions_for_load");

            let summaries = list_summaries(None).await.map_err(|e| {
                acp::Error::internal_error().data(format!("failed to list workspaces: {e}"))
            })?;

            summaries_to_overview_response(summaries)
        }
        "echo.agent/session_summaries/workspace_list_recent" => {
            let req = serde_json::from_str::<RecentSessionsRequest>(args.params.get())?;

            let _timer = crate::instrumentation_timer!("session.list_sessions_recent");

            let limit = req.limit.min(10_000);
            let mut summaries = list_recent_summaries(limit).await.map_err(|e| {
                acp::Error::internal_error().data(format!("failed to list workspaces: {e}"))
            })?;
            for s in &mut summaries {
                backfill_session_summary(s);
            }

            let value = serde_json::to_value(&summaries)
                .map(|v| serde_json::value::to_raw_value(&v).map(Arc::from))
                .expect("to work")
                .expect("to work");

            Ok(acp::ExtResponse::new(value))
        }
        _ => Err(acp::Error::method_not_found()),
    }
}

/// Group summaries by cwd and serialize into an [`AllSessionOverviewResponse`].
fn summaries_to_overview_response(summaries: Vec<Summary>) -> Result<acp::ExtResponse, acp::Error> {
    let mut by_cwd: BTreeMap<String, Vec<Summary>> = Default::default();
    for mut s in summaries {
        backfill_session_summary(&mut s);
        by_cwd.entry(s.info.cwd.clone()).or_default().push(s);
    }

    let value = serde_json::to_value(AllSessionOverviewResponse {
        all_sessions: by_cwd
            .into_iter()
            .map(|(k, v)| (PathBuf::from(k), v))
            .collect(),
    })
    .map(|v| serde_json::value::to_raw_value(&v).map(Arc::from))
    .expect("to work")
    .expect("to work");

    Ok(acp::ExtResponse::new(value))
}
// ── Merged session list (local + remote) ─────────────────────────────

async fn handle_session_list(
    agent: &MvpAgent,
    args: &acp::ExtRequest,
) -> Result<acp::ExtResponse, acp::Error> {
    use crate::session::unified_list;

    // Under chat mode `parse_list_req` force-rewrites `kind` to conversations
    // unless `local-workspace` is compiled in and the client sent chat/build.
    let req = unified_list::parse_list_req(args.params.get())
        .map_err(|e| acp::Error::invalid_params().data(format!("invalid params: {e}")))?;
    tracing::debug!(
        chat_mode_forced_kind = crate::agent::chat_modes::process_chat_mode_enabled(),
        "session/list"
    );

    let registry_client = agent.session_registry_client();
    let conversations_client = agent.conversations_client();
    let result = unified_list::build_unified_list(
        registry_client.as_ref(),
        conversations_client.as_ref(),
        req,
    )
    .await;

    ExtMethodResult::success(unified_list::ext_list_response(result))
        .to_ext_response()
        .map_err(|e| acp::Error::internal_error().data(e.to_string()))
}

/// Build sessions in exactly the requested directory. A page walk cannot reach
/// past `over_fetch(limit)` rows per cwd: the local lane re-scans that window
/// each page instead of seeking to the cursor.
pub(crate) async fn handle_list_sessions(
    agent: &MvpAgent,
    args: acp::ListSessionsRequest,
) -> Result<acp::ListSessionsResponse, acp::Error> {
    use crate::session::unified_list;

    // Kept in step with the capability, withheld under chat mode.
    if crate::agent::chat_modes::process_chat_mode_enabled() {
        return Err(acp::Error::method_not_found());
    }

    let additional_directories = args.additional_directories.len();
    let cwd = args.cwd.map(|p| p.to_string_lossy().into_owned());
    let mut req = unified_list::ListReq {
        cwd: cwd.clone(),
        cursor: args.cursor,
        meta: args.meta.map(serde_json::Value::Object),
        cwd_scope: unified_list::CwdScope::Only,
        ..Default::default()
    };
    unified_list::force_kind(&mut req, unified_list::SessionKind::Build);

    let registry_client = agent.session_registry_client();
    let conversations_client = agent.conversations_client();
    let result = unified_list::build_unified_list(
        registry_client.as_ref(),
        conversations_client.as_ref(),
        req,
    )
    .await;

    let meta = unified_list::acp_response_meta(&result);
    // `CwdScope::Only` already dropped rows the schema cannot represent, so this
    // conversion discards nothing and the page keeps the size it was given.
    let sessions: Vec<acp::SessionInfo> = result
        .rows
        .into_iter()
        .filter_map(|row| row.into_session_info().try_into().ok())
        .collect();

    tracing::debug!(
        cwd = cwd.as_deref().unwrap_or("<all>"),
        paged = result.next_cursor.is_some(),
        returned = sessions.len(),
        additional_directories,
        "session/list"
    );

    Ok(acp::ListSessionsResponse::new(sessions)
        .next_cursor(result.next_cursor)
        .meta(meta))
}
