//! Session-administration extension handlers.
//!
//! Methods grouped here are operational/admin endpoints that mutate
//! persistent or shared agent state but are not part of the per-turn prompt
//! lifecycle:
//!
//! - `echo.agent/session/rename`                  rename a session locally + remote
//! - `echo.agent/session/delete`                  delete a session locally + remote
//! - `echo.agent/session/update_mcp_servers`      mid-session MCP server swap
//! - `echo.agent/session/add_local_workspace`     mid-session local workspace add-only (chat)
//! - `echo.agent/session/fork`                    fork a session into a new one
//! - `echo.agent/internal/reload_all_mcp_servers` config hot-reload, all sessions
//! - `echo.agent/internal/reload_project_mcp_servers` config hot-reload, cwd-scoped
//! - `echo.agent/internal/reload_skills`          skills file watcher fan-out
//! - `echo.agent/internal/reload_models`          model list hot-reload from config.toml
//! - `echo.agent/internal/reload_models_cache`    model catalog hot-reload from disk cache
//! - `echo.agent/internal/auth_cleared`           auth hot-clear cleanup
//! - `echo.agent/plugins/reload`                  rebuild shared plugin registry
//! - `echo.agent/commands/list`                   list slash commands

use std::path::Path;
use std::sync::Arc;

use agent_client_protocol as acp;
use agent_client_protocol::Client as _;
use serde::Deserialize;

use super::{ExtResult, parse_params, to_raw_response};
use crate::agent::MvpAgent;
use crate::leader::protocol::InternalMethod;
use crate::session::persistence::{
    MAX_TITLE_BYTES, MAX_TITLE_SCALARS, PersistenceMsg, list_summaries, sanitize_rename_title,
};
use crate::session::storage::StorageAdapter;
use crate::session::storage::jsonl::JsonlStorageAdapter;
use crate::session::unified_list::SessionKind;
use crate::session::{ExtMethodResult, SessionCommand};
use echo_agent_telemetry::id::agent_id;

#[tracing::instrument(skip_all, fields(method = %args.method))]
pub(crate) async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    if let Some(method) = InternalMethod::from_name(args.method.as_ref()) {
        return handle_internal(agent, args, method).await;
    }
    match args.method.as_ref() {
        "echo.agent/session/rename" => handle_session_rename(agent, args).await,
        "echo.agent/session/delete" => handle_session_delete(agent, args).await,
        "echo.agent/session/update_mcp_servers" => handle_update_mcp_servers(agent, args).await,
        #[cfg(feature = "local-workspace")]
        "echo.agent/session/add_local_workspace" => handle_add_local_workspace(agent, args).await,
        "echo.agent/session/fork" => handle_session_fork(agent, args).await,
        "echo.agent/plugins/reload" => handle_plugins_reload(agent).await,
        "echo.agent/commands/list" => handle_commands_list(agent, args).await,
        _ => Err(acp::Error::method_not_found()),
    }
}

/// Exhaustive, so a new [`InternalMethod`] cannot compile without a handler.
async fn handle_internal(
    agent: &MvpAgent,
    args: &acp::ExtRequest,
    method: InternalMethod,
) -> ExtResult {
    match method {
        InternalMethod::ReloadAllMcpServers => handle_reload_all_mcp_servers(agent).await,
        InternalMethod::ReloadProjectMcpServers => {
            handle_reload_project_mcp_servers(agent, args).await
        }
        InternalMethod::ReloadSkills => handle_reload_skills(agent).await,
        InternalMethod::ReloadWorkflows => handle_reload_workflows(agent),
        InternalMethod::ReloadModels => handle_reload_models(agent),
        InternalMethod::ReloadModelsCache => handle_reload_models_cache(agent),
        InternalMethod::AuthCleared => handle_auth_cleared(agent),
        // Arrives as a notification, so it never reaches this request path.
        InternalMethod::EvictSessions => Err(acp::Error::method_not_found()),
    }
}

// session/rename

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionRenameRequest {
    session_id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    kind: SessionKind,
    #[serde(default)]
    reset_to_auto: bool,
}

/// Handles renaming a session.
///
/// Relay-registered sessions (sidebar titles): the relay REST
/// endpoint remains the sole title authority. This ACP method does not
/// write through `relay_sync`; a rename that never reaches the relay
/// reverts on the next sidebar refetch. Clients that own a relay lane
/// must rename through the relay REST endpoint. Unpin (`resetToAuto`)
/// has the same gap and cannot clear a relay sidebar title (the relay
/// REST API can only set a title).
async fn handle_session_rename(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let mut req: SessionRenameRequest = parse_params(args)?;
    if req.reset_to_auto {
        if req.kind == SessionKind::Chat {
            return Err(acp::Error::invalid_request()
                .data("chat conversations have no auto-title to restore"));
        }
        // Mixed-version: a new pager sends `{title:"", resetToAuto:true}` so
        // an old shell (unknown field ignored) hits the blank-title rejection
        // instead of silently renaming. Non-empty here is a client bug.
        if !sanitize_rename_title(&req.title).is_empty() {
            return Err(
                acp::Error::invalid_request().data("title must be empty when resetToAuto is set")
            );
        }
        return reset_session_title_to_auto(agent, &req.session_id, req.cwd.as_deref()).await;
    }
    // Manual titles must be non-blank: `Summary.title_is_manual` binds to a
    // real `generated_title`, so reject whitespace-only input at the boundary.
    // Strip C0/C1 controls here (single authority) so a `/rename` OSC/CSI
    // payload cannot persist on RemoteSync.
    if req.title.len() > MAX_TITLE_BYTES {
        return Err(acp::Error::invalid_request().data("title too large"));
    }
    req.title = sanitize_rename_title(&req.title).into_owned();
    if req.title.is_empty() {
        return Err(acp::Error::invalid_request().data("title must not be blank"));
    }
    if req.title.chars().count() > MAX_TITLE_SCALARS {
        return Err(acp::Error::invalid_request().data(format!(
            "title too long (max {MAX_TITLE_SCALARS} characters after removing control characters)"
        )));
    }

    if req.kind == SessionKind::Chat {
        return rename_chat_conversation(agent, &req.session_id, &req.title).await;
    }

    let session_id = acp::SessionId::new(Arc::from(req.session_id.as_str()));

    // Find the session info, scoping to cwd if provided
    let summaries = list_summaries(req.cwd.as_deref())
        .await
        .map_err(|e| acp::Error::internal_error().data(format!("failed to list sessions: {e}")))?;

    let summary = summaries
        .iter()
        .find(|s| s.info.id == session_id)
        .ok_or_else(|| {
            acp::Error::invalid_request().data(format!("session not found: {}", req.session_id))
        })?;

    let info = summary.info.clone();

    // Update the session title in local storage
    let storage = JsonlStorageAdapter::default();
    storage
        .update_session_title(&info, req.title.clone())
        .await
        .map_err(|e| {
            acp::Error::internal_error().data(format!("failed to update session title: {e}"))
        })?;

    // Resident sessions own RemoteSync inside the persistence actor. Route
    // via that FIFO channel so a racing auto-title SetTitle cannot overtake.
    if let Some(handle) = agent.resident_handle(&session_id) {
        let _ = handle
            .persistence_tx
            .send(PersistenceMsg::ManualTitleRenamed(req.title.clone()));
        // Freeze the auto title refresh so an in-flight one can't flip the
        // user's title and no later refresh fights it (the actor persists the
        // frozen watermark).
        let _ = handle
            .cmd_tx
            .send(crate::session::commands::SessionCommand::TitleRenamed { manual: true });
    } else {
        // Dormant session: no actor to freeze it, so persist the frozen
        // watermark directly for the next resume.
        crate::session::helpers::session_summary::save_title_refresh_watermark(
            &crate::session::persistence::session_dir(&info),
            crate::session::helpers::session_summary::TITLE_REFRESH_TURNS.len(),
        );
    }

    // Update session search index with new title
    crate::session::storage::search::notify_session_updated(
        agent.search_index().writer(),
        &info.id.to_string(),
        &info.cwd,
    );

    // Send a SessionSummaryGenerated notification so the TUI updates its title
    notify_session_title(agent, session_id, &req.title).await;

    if agent.is_writeback_storage()
        && let Some(auth) = agent.current_auth()
        && !auth.is_zdr_team()
    {
        use crate::remote::client::BackendClient;
        use crate::session::export::ExportedMetadata;

        let mut metadata = ExportedMetadata::from_summary(summary);
        metadata.title = Some(req.title.clone());
        metadata.title_is_manual = Some(true);
        metadata.updated_at = Some(chrono::Utc::now().to_rfc3339());
        if let Err(e) = BackendClient::new()
            .with_auth_manager(agent.auth_manager.clone())
            .save_session_data(&req.session_id, &[], Some(&metadata))
            .await
        {
            tracing::warn!(?e, session_id = %req.session_id, "failed to sync renamed title to backend");
        }
    }

    // Hook 2: update session replica with summary (fire-and-forget)
    spawn_registry_title_update(
        agent,
        &req.session_id,
        registry_title_for_agent(agent, Some(req.title.clone())),
    );

    tracing::info!(session_id = %req.session_id, title = %req.title, "Session renamed");

    to_raw_response(&serde_json::json!({ "success": true }))
}

/// `/rename --auto`: clear the local manual pin under the summary lock.
/// When a pin was actually cleared, enqueue `ResetTitleToAuto` (generator
/// reset + remote cache clear) and fan out `titleIsManual: false`. Re-indexes
/// either way when this process keeps an index.
async fn reset_session_title_to_auto(
    agent: &MvpAgent,
    session_id: &str,
    cwd: Option<&str>,
) -> ExtResult {
    let session_id_acp = acp::SessionId::new(Arc::from(session_id));

    let summaries = list_summaries(cwd)
        .await
        .map_err(|e| acp::Error::internal_error().data(format!("failed to list sessions: {e}")))?;

    let summary = summaries
        .iter()
        .find(|s| s.info.id == session_id_acp)
        .ok_or_else(|| {
            acp::Error::invalid_request().data(format!("session not found: {session_id}"))
        })?;

    let info = summary.info.clone();

    let storage = JsonlStorageAdapter::default();
    let cleared = storage.reset_title_to_auto(&info).await.map_err(|e| {
        acp::Error::internal_error().data(format!("failed to reset session title: {e}"))
    })?;

    if cleared {
        if let Some(handle) = agent.resident_handle(&session_id_acp) {
            let _ = handle.persistence_tx.send(PersistenceMsg::ResetTitleToAuto);
            // Reopen the auto title refresh so it can re-title from the whole
            // conversation now that the manual pin is gone (the actor persists
            // the reopened watermark).
            let _ = handle
                .cmd_tx
                .send(crate::session::commands::SessionCommand::TitleRenamed { manual: false });
        } else {
            // Dormant session: persist the reopened watermark directly so the
            // whole-conversation retitle can run on the next resume.
            crate::session::helpers::session_summary::save_title_refresh_watermark(
                &crate::session::persistence::session_dir(&info),
                0,
            );
        }
        // Non-resident sessions have no persistence actor / RemoteSync.
        // Mirror the rename writeback path so a dormant unpin cannot leave
        // the remote pin for a later pull to restore.
        if agent.is_writeback_storage()
            && let Some(auth) = agent.current_auth()
            && !auth.is_zdr_team()
        {
            use crate::remote::client::BackendClient;
            use crate::session::export::ExportedMetadata;

            let mut metadata = ExportedMetadata::from_summary(summary);
            metadata.title = Some(String::new());
            metadata.title_is_manual = Some(false);
            metadata.updated_at = Some(chrono::Utc::now().to_rfc3339());
            if let Err(e) = BackendClient::new()
                .with_auth_manager(agent.auth_manager.clone())
                .save_session_data(session_id, &[], Some(&metadata))
                .await
            {
                tracing::warn!(
                    ?e,
                    session_id = %session_id,
                    "failed to clear remote title on unpin"
                );
            }
        }
        // `titleIsManual: false` is distinct from absent meta (absent =
        // racing auto title).
        notify_session_title_unpinned(agent, session_id_acp).await;
        // Empty string, not None: `UpdateRequest.summary` omits `None` and
        // the replica would keep advertising the old manual title.
        spawn_registry_title_update(
            agent,
            session_id,
            registry_title_for_agent(agent, Some(String::new())),
        );
    }

    crate::session::storage::search::notify_session_updated(
        agent.search_index().writer(),
        &info.id.to_string(),
        &info.cwd,
    );

    tracing::info!(session_id = %session_id, cleared, "Session title reset to auto");

    to_raw_response(&serde_json::json!({ "success": true }))
}

/// ZDR teams omit the title on the wire (`None`); everyone else sends `title`.
fn registry_title_for_agent(agent: &MvpAgent, title: Option<String>) -> Option<String> {
    if agent
        .auth_manager
        .current_or_expired()
        .is_some_and(|a| a.is_zdr_team())
    {
        None
    } else {
        title
    }
}

/// Fire-and-forget session-registry summary update. `title = None` omits
/// the field (no-op on a merge replica); `Some("")` clears a prior pin.
fn spawn_registry_title_update(agent: &MvpAgent, session_id: &str, title: Option<String>) {
    let Some(client) = agent.session_registry_client() else {
        return;
    };
    let sid = session_id.to_string();
    tokio::spawn(async move {
        let update = crate::agent::session_registry_client::UpdateRequest {
            summary: title,
            first_prompt: None,
            last_turn_number: None,
            repo_head_at_end: None,
            restorable_turn_number: None,
        };
        if let Err(e) = client.update(&sid, &update).await {
            tracing::warn!(error = %e, "session registry summary update failed (non-fatal)");
        }
    });
}

/// Unpin fan-out: `SessionSummaryGenerated` with empty text +
/// `_meta.echo.agent/titleIsManual: false` so followers drop `display_name`
/// without treating this as a racing auto title.
async fn notify_session_title_unpinned(agent: &MvpAgent, session_id: acp::SessionId) {
    use crate::extensions::notification::{
        SessionNotification, SessionUpdate, title_is_unpinned_meta,
    };

    let notification = SessionNotification {
        session_id: session_id.clone(),
        update: SessionUpdate::SessionSummaryGenerated {
            session_summary: String::new(),
        },
        meta: Some(title_is_unpinned_meta()),
    };
    if let Ok(params) = serde_json::value::to_raw_value(&notification) {
        let ext_notification =
            acp::ExtNotification::new("echo.agent/session_notification", params.into());
        let _ = agent.gateway.ext_notification(ext_notification).await;
    }

    if agent.is_resident(&session_id) {
        agent.gateway.forward_fire_and_forget(
            crate::session::summary::session_info_update_unpinned(session_id),
        );
    }
}

/// Notify connected clients of a session's new title via
/// `SessionSummaryGenerated`. Manual-rename fan-out stamps
/// `_meta.echo.agent/titleIsManual` so followers can set `display_name`.
async fn notify_session_title(agent: &MvpAgent, session_id: acp::SessionId, title: &str) {
    use crate::extensions::notification::{
        SessionNotification, SessionUpdate, title_is_manual_meta,
    };

    let notification = SessionNotification {
        session_id: session_id.clone(),
        update: SessionUpdate::SessionSummaryGenerated {
            session_summary: title.to_owned(),
        },
        meta: Some(title_is_manual_meta()),
    };
    if let Ok(params) = serde_json::value::to_raw_value(&notification) {
        let ext_notification =
            acp::ExtNotification::new("echo.agent/session_notification", params.into());
        let _ = agent.gateway.ext_notification(ext_notification).await;
    }

    agent.notify_session_info_update(&session_id, title);
}

async fn rename_chat_conversation(
    agent: &MvpAgent,
    conversation_id: &str,
    title: &str,
) -> ExtResult {
    use crate::remote::{ConvError, UpdateConversationBody};

    let Some(client) = agent.conversations_client() else {
        return Err(acp::Error::invalid_request()
            .data("chat session rename requires the conversations lane (OIDC + chat feature)"));
    };

    let body = UpdateConversationBody {
        title: Some(title.to_owned()),
        starred: None,
    };
    client
        .update_conversation(conversation_id, &body)
        .await
        .map_err(|e| match e {
            ConvError::NoOauth => acp::Error::invalid_request()
                .data("chat session rename requires EchoAgent OAuth credentials"),
            ConvError::Http { status: 404 } => acp::Error::invalid_request()
                .data(format!("conversation not found: {conversation_id}")),
            other => acp::Error::internal_error()
                .data(format!("chat conversation rename failed: {other}")),
        })?;

    // If this conversation is open live, notify clients of the new title.
    let session_id = acp::SessionId::new(Arc::from(conversation_id));
    if agent.is_resident(&session_id) {
        notify_session_title(agent, session_id, title).await;
    }

    tracing::info!(
        session_id = %conversation_id,
        title = %title,
        "Chat conversation renamed"
    );

    to_raw_response(&serde_json::json!({ "success": true }))
}

// session/delete

/// Delete a session from history.
async fn handle_session_delete(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct DeleteRequest {
        session_id: String,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        kind: SessionKind,
    }

    let req: DeleteRequest = parse_params(args)?;

    if req.kind == SessionKind::Chat {
        return soft_delete_chat_conversation(agent, &req.session_id).await;
    }

    let session_id = acp::SessionId::new(Arc::from(req.session_id.as_str()));

    // For writeback storage (non-ZDR): remote delete is authoritative for
    // the cloud history and runs first; on failure no local bits are
    // touched so the pager does not remove the row or toast success.
    let needs_remote =
        agent.is_writeback_storage() && agent.current_auth().is_some_and(|a| !a.is_zdr_team());

    // Always drain: even a non-resident session can still have coordinator
    // children finishing after an earlier fire-and-forget TeardownSession
    // (e.g. idle unload). hard_stop / kill_all no-op when not resident.
    agent.teardown_live_session_before_delete(&session_id).await;

    // Shared delete: remote-first, then local disk + FTS eviction.
    // Mirrored by the `echoagent sessions delete <id>` CLI path.
    crate::session::persistence::delete_session_history(
        &req.session_id,
        req.cwd.as_deref(),
        needs_remote,
        agent.auth_manager.clone(),
        agent.search_index().writer(),
    )
    .await
    .map_err(|e| {
        if let crate::session::persistence::DeleteSessionError::Remote(_) = &e {
            tracing::warn!(?e, session_id = %req.session_id, "failed to delete remote session data");
        }
        acp::Error::internal_error().data(e.to_string())
    })?;

    tracing::info!(session_id = %req.session_id, "Session deleted");

    to_raw_response(&serde_json::json!({ "success": true }))
}

async fn soft_delete_chat_conversation(agent: &MvpAgent, conversation_id: &str) -> ExtResult {
    use crate::remote::ConvError;

    let Some(client) = agent.conversations_client() else {
        return Err(acp::Error::invalid_request()
            .data("chat session delete requires the conversations lane (OIDC + chat feature)"));
    };

    client
        .soft_delete_conversation(conversation_id)
        .await
        .map_err(|e| match e {
            ConvError::NoOauth => acp::Error::invalid_request()
                .data("chat session delete requires EchoAgent OAuth credentials"),
            other => acp::Error::internal_error()
                .data(format!("chat conversation soft-delete failed: {other}")),
        })?;

    let session_id = acp::SessionId::new(Arc::from(conversation_id));
    if agent.is_resident(&session_id) {
        agent.request_session_shutdown(&session_id);
        agent.remove_session(&session_id);
    }

    tracing::info!(session_id = %conversation_id, "Chat conversation soft-deleted");

    to_raw_response(&serde_json::json!({ "success": true }))
}

// session/update_mcp_servers

async fn handle_update_mcp_servers(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Params {
        session_id: acp::SessionId,
        mcp_servers: Vec<acp::McpServer>,
    }

    let params: Params = parse_params(args)?;

    let (handle, cwd) = {
        let h = agent
            .resident_handle(&params.session_id)
            .ok_or_else(|| acp::Error::invalid_params().data("unknown session id"))?;
        let cwd = std::path::PathBuf::from(&h.info.cwd);
        (h, cwd)
    };

    let compat = agent.cfg.borrow().compat_resolved;
    let admitted =
        crate::session::managed_mcp::admit_client_mcp_servers(params.mcp_servers, &cwd, &compat);
    let merged = crate::session::managed_mcp::merge_managed_mcp_servers(
        admitted.clone(),
        &cwd,
        agent.plugin_registry_handle().snapshot().as_deref(),
        &compat,
    );

    let (tx, rx) = tokio::sync::oneshot::channel();
    handle
        .cmd_tx
        .send(SessionCommand::UpdateMcpServers {
            mcp_servers: merged,
            respond_to: tx,
        })
        .map_err(|_| acp::Error::internal_error().data("session closed"))?;

    // Wait for the session actor to finish MCP re-initialization.
    rx.await
        .map_err(|_| acp::Error::internal_error().data("session closed"))?
        .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;

    // Store the admitted (not raw) client set: hot-reloads re-merge from this
    // seed, and a raw list would re-spawn a previously rejected vendor server
    // once on-disk attribution vanishes.
    agent.with_resident_mut(&params.session_id, |h| {
        h.initial_client_mcp_servers = admitted;
    });

    ExtMethodResult::success(serde_json::json!({ "ok": true }))
        .to_ext_response()
        .map_err(|e| acp::Error::internal_error().data(e.to_string()))
}

// session/add_local_workspace (add-only; local-workspace feature)

#[cfg(feature = "local-workspace")]
async fn handle_add_local_workspace(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Params {
        session_id: acp::SessionId,
        #[serde(default)]
        meta: Option<acp::Meta>,
    }

    let params: Params = parse_params(args)?;
    let cwd = {
        let h = agent
            .resident_handle(&params.session_id)
            .ok_or_else(|| acp::Error::invalid_params().data("unknown session id"))?;
        std::path::PathBuf::from(&h.info.cwd)
    };
    // Gate on actual chat kind — not `requires_gateway` (true for non-chat
    // GatewayAttach; false for unknown ids).
    if !agent.is_chat_kind_session(&params.session_id) {
        return Err(acp::Error::invalid_params().data(serde_json::json!({
            "code": "local_workspace_chat_only",
            "message": "echo.agent/session/add_local_workspace is only available on chat-kind sessions",
        })));
    }

    let result = agent
        .add_local_workspace_mid_session(&params.session_id, params.meta, &cwd)
        .await?;
    ExtMethodResult::success(result)
        .to_ext_response()
        .map_err(|e| acp::Error::internal_error().data(e.to_string()))
}

// internal/reload_skills

/// Reload skills for ALL active sessions. Called by the skills file watcher
async fn handle_reload_skills(agent: &MvpAgent) -> ExtResult {
    let reloaded = agent
        .reload_skills_all_sessions_checked()
        .await
        .map_err(|error| acp::Error::internal_error().data(error))?;
    ExtMethodResult::success(serde_json::json!({ "reloaded": reloaded }))
        .to_ext_response()
        .map_err(|e| acp::Error::internal_error().data(e.to_string()))
}

fn handle_reload_workflows(agent: &MvpAgent) -> ExtResult {
    let reloaded = agent.advertise_commands_all_sessions();
    ExtMethodResult::success(serde_json::json!({ "reloaded": reloaded }))
        .to_ext_response()
        .map_err(|e| acp::Error::internal_error().data(e.to_string()))
}

// internal/reload_all_mcp_servers

/// Reload MCP servers for ALL active sessions. Called by the config
/// hot-reload watcher when `[mcp_servers]` changes in config.toml.
async fn handle_reload_all_mcp_servers(agent: &MvpAgent) -> ExtResult {
    let session_ids = agent.resident_ids();

    if session_ids.is_empty() {
        return ExtMethodResult::success(serde_json::json!({ "updated": 0 }))
            .to_ext_response()
            .map_err(|e| acp::Error::internal_error().data(e.to_string()));
    }

    let mut updated = 0u32;
    for session_id in &session_ids {
        let Some(handle) = agent.resident_handle(session_id) else {
            continue;
        };
        let cwd = std::path::PathBuf::from(&handle.info.cwd);
        let compat = agent.cfg.borrow().compat_resolved;
        // Re-seed the merge with the session's original client-provided MCP
        // servers (e.g. a client session binding injected at `session/new`).
        // `merge_managed_mcp_servers` already re-reads every disk source
        // (config.toml, plugins, ~/.claude.json, ~/.cursor/mcp.json, .mcp.json)
        // internally, so passing `load_mcp_servers()` output here was redundant
        // — and silently dropped client servers that exist in no on-disk config,
        // tearing them down on every config hot-reload.
        if crate::session::managed_mcp::merge_and_send_managed_mcp_update(
            &handle.cmd_tx,
            &cwd,
            handle.initial_client_mcp_servers.clone(),
            agent.plugin_registry_handle().snapshot().as_deref(),
            &compat,
        ) {
            updated += 1;
        }
    }

    tracing::info!(
        updated,
        total = session_ids.len(),
        "reloaded MCP servers for active sessions"
    );
    ExtMethodResult::success(serde_json::json!({ "updated": updated }))
        .to_ext_response()
        .map_err(|e| acp::Error::internal_error().data(e.to_string()))
}

// internal/reload_project_mcp_servers

/// Reload MCP servers for sessions whose `cwd` matches (or sits beneath)
/// the project root passed in `params.cwd`. Called by the config
/// hot-reload watcher when `<cwd>/.echo-agent/config.toml`,
/// `<cwd>/.mcp.json`, or `<cwd>/.claude.json` changes.
///
/// Sessions in unrelated cwds are intentionally NOT touched — that is
/// the whole point of [`crate::config::reloader::ConfigUpdate::
/// ProjectMcpServersChanged`] being a per-cwd variant. The legacy
/// [`handle_reload_all_mcp_servers`] is still the fan-out for global
/// `~/.echo-agent/config.toml` edits.
async fn handle_reload_project_mcp_servers(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    #[derive(Deserialize)]
    struct Params {
        cwd: String,
    }

    let params: Params = parse_params(args)?;
    let target_cwd = std::path::PathBuf::from(&params.cwd);

    // Collect (session_id, cwd) pairs once so we don't hold the
    // `sessions` RefCell borrow across `.await` points.
    let mut session_ids = Vec::new();
    agent.for_each_resident(|sid, h| {
        session_ids.push((sid.clone(), std::path::PathBuf::from(&h.info.cwd)));
    });
    session_ids.retain(|(_, cwd)| cwd_matches(cwd, &target_cwd));

    if session_ids.is_empty() {
        return ExtMethodResult::success(serde_json::json!({ "updated": 0 }))
            .to_ext_response()
            .map_err(|e| acp::Error::internal_error().data(e.to_string()));
    }

    let mut updated = 0u32;
    for (session_id, cwd) in &session_ids {
        let Some(handle) = agent.resident_handle(session_id) else {
            continue;
        };
        // See `handle_reload_all_mcp_servers`: seed with the session's
        // client-provided servers, not `load_mcp_servers()` — the merge
        // re-reads all disk sources itself, and client-provided servers
        // (session bindings) must survive config hot-reloads.
        let merged = crate::session::managed_mcp::merge_managed_mcp_servers(
            handle.initial_client_mcp_servers.clone(),
            cwd,
            agent.plugin_registry_handle().snapshot().as_deref(),
            &agent.cfg.borrow().compat_resolved,
        );

        let (tx, _rx) = tokio::sync::oneshot::channel();
        if handle
            .cmd_tx
            .send(SessionCommand::UpdateMcpServers {
                mcp_servers: merged,
                respond_to: tx,
            })
            .is_ok()
        {
            updated += 1;
        }
    }

    tracing::info!(
        updated,
        total = session_ids.len(),
        cwd = %target_cwd.display(),
        "reloaded project MCP servers for matching sessions"
    );
    ExtMethodResult::success(serde_json::json!({ "updated": updated }))
        .to_ext_response()
        .map_err(|e| acp::Error::internal_error().data(e.to_string()))
}

/// Returns `true` iff `session_cwd` equals `target_cwd` or sits
/// beneath it (so a `<repo>/` edit reloads `<repo>/subdir/` sessions
/// too).
///
/// This uses `Path::starts_with`, which is
/// **component-aware** — `/repo-test` does NOT match `/repo` even
/// though the byte prefix matches. That is the desired behavior
/// (component-aware avoids the `/foo-bar` ⊂ `/foo` foot-gun). Paths
/// come from `SessionInfo::cwd` (always absolute) and the watcher's
/// emitted path (also absolute), so no canonicalization is needed
/// here. The `==` short-circuit is redundant (`Path::starts_with` is
/// reflexive) but kept for an explicit zero-allocation fast path.
fn cwd_matches(session_cwd: &std::path::Path, target_cwd: &std::path::Path) -> bool {
    session_cwd == target_cwd || session_cwd.starts_with(target_cwd)
}

// internal/reload_models

/// Re-resolve the agent model list from config.toml. Called by the config
/// hot-reload watcher when `[model.*]` or `[models]` changes.
///
/// Re-reads config from disk, re-runs the same resolution logic as
/// `new_with_models()` for user TOML config entries, and swaps the model list
/// in-place. Prefetched (API) and default models are NOT re-fetched -- only
/// BYOK entries from config are updated.
fn handle_reload_models(agent: &MvpAgent) -> ExtResult {
    let disk_config = crate::config::load_effective_config()
        .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;

    let toml_config = crate::agent::config::Config::new_from_toml_cfg(&disk_config)
        .map_err(|e| acp::Error::internal_error().data(e))?;

    // Merge TOML-derived model fields into the agent's in-memory config so
    // runtime-only fields (#[serde(skip)]: remote_settings, endpoints, CLI
    // flags) are preserved. Only model-related TOML fields are refreshed.
    {
        let agent_config = agent.cfg.borrow();
        let overrides = crate::config::ModelOverrideConfig::resolve(
            agent_config.web_search_model_override.as_deref(),
            agent_config.session_summary_model_override.as_deref(),
            &disk_config,
            agent_config.remote_settings.as_ref(),
        );
        drop(agent_config);
        let mut agent_config = agent.cfg.borrow_mut();
        agent_config.models = toml_config.models.clone();
        agent_config.config_models = toml_config.config_models.clone();
        agent_config.model_providers = toml_config.model_providers.clone();
        agent_config.web_search_model = overrides.web_search;
        agent_config.session_summary_model = overrides.session_summary;
        agent_config.image_description_model = overrides.image_description;
        agent_config.prompt_suggest_model_pin = overrides.prompt_suggestion;
    }
    // Recompute the campaign overlay + `pre_campaign_default` (the catalog-miss
    // fallback) so reload matches spawn; `new_from_toml_cfg` reset it to None.
    {
        let mut agent_config = agent.cfg.borrow_mut();
        crate::util::config::sync_campaign_fields(&mut agent_config);
    }
    let merged_config = agent.cfg.borrow().clone();

    agent.models_manager.apply_config(merged_config);
    agent.sync_process_static_api_key(None);
    let auth_method_id = agent.reconcile_auth_method_after_model_reload();

    let count = agent.models_manager.models().len();
    let auth_ready = auth_method_id.is_some();
    tracing::info!(
        count,
        auth_method_id,
        auth_ready,
        "model list reloaded from config.toml"
    );
    ExtMethodResult::success(serde_json::json!({
        "models": count,
        "authMethodId": auth_method_id,
        "authReady": auth_ready,
    }))
    .to_ext_response()
    .map_err(|e| acp::Error::internal_error().data(e.to_string()))
}

// internal/reload_models_cache

/// Hot-reload the model catalog from `~/.echo-agent/models_cache.json` after an
/// external write detected by the config watcher.
///
/// Routed through the agent's ACP stream (injected by the
/// `ConfigUpdate::ModelsCacheChanged` arm in `agent/app.rs`) instead of being
/// applied directly on the manager from the config-update task: stream
/// requests are processed in order, so when `config.toml` and
/// `models_cache.json` change in the same watcher batch this runs strictly
/// after `reload_models`' `apply_config` accepted or rejected the new config,
/// rather than rebuilding the catalog and notifying clients mid-flight.
fn handle_reload_models_cache(agent: &MvpAgent) -> ExtResult {
    agent.models_manager.reload_from_disk_cache();
    agent.sync_process_static_api_key(None);
    ExtMethodResult::success(serde_json::json!({ "reloaded": true }))
        .to_ext_response()
        .map_err(|e| acp::Error::internal_error().data(e.to_string()))
}

fn handle_auth_cleared(agent: &MvpAgent) -> ExtResult {
    agent.disable_managed_gateway_tools_and_refresh_sessions();
    ExtMethodResult::success(serde_json::json!({ "ok": true }))
        .to_ext_response()
        .map_err(|e| acp::Error::internal_error().data(e.to_string()))
}

// plugins/reload

async fn handle_plugins_reload(agent: &MvpAgent) -> ExtResult {
    // Rebuild the shared registry so future/new sessions clone the latest.
    let session_cwd = agent.resident_ids().into_iter().next().and_then(|id| {
        agent
            .resident_handle(&id)
            .map(|h| std::path::PathBuf::from(&h.info.cwd))
    });
    let mut plugins = agent.cfg.borrow().plugins.clone();
    plugins.merge_claude_enabled_plugins(session_cwd.as_deref());
    let disk_cfg = plugins.to_discovery_config();
    // Folder-trust gates repo-local project plugins (hooks/MCP). Resolve and
    // record the verdict for this cwd (honoring the real remote), then gate
    // plugins on it.
    let project_trusted = session_cwd.as_deref().is_some_and(|c| {
        let remote_settings = agent.cfg.borrow().remote_settings.clone();
        crate::agent::folder_trust::resolve_and_record(c, remote_settings.as_ref(), false)
    });
    // Explicit desktop `echo.agent/plugins/reload`: force a full local-install re-copy.
    agent
        .plugin_registry_handle()
        .reload(session_cwd.as_deref(), &disk_cfg, project_trusted, true);

    // Eagerly fan out the new registry to every live session: each adopts a
    // cwd-correct snapshot (hooks + MCP + skills + client slash-command
    // catalog), the same refresh the originating session of a reload gets.
    agent.broadcast_plugin_registry_to_sessions(None);

    super::to_ext_response(Ok(serde_json::json!({"ok": true})))
}

// commands/list

async fn handle_commands_list(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let req: crate::session::slash_commands::ListCommandsRequest = parse_params(args)?;

    let availability = agent.command_availability();
    let skills_config = agent.cfg.borrow().skills.clone();
    let compat = agent.cfg.borrow().compat_resolved;

    // Chat product catalog never uses cwd plugin discovery / folder-trust.
    // Prefer kind=chat even when sessionId is set so the pull matches ACU.
    if req.kind.as_deref() == Some("chat") {
        let response = crate::session::slash_commands::list_commands(
            None,
            &skills_config,
            None,
            availability,
            compat,
            false,
            Some("chat"),
            Some(agent.auth_manager.clone()),
        )
        .await?;
        return Ok(acp::ExtResponse::new(Arc::from(
            serde_json::value::to_raw_value(&response)?,
        )));
    }

    if let Some(session_id) = req.session_id.as_ref() {
        let Some(handle) = agent.session_handle_waiting_for_load(session_id).await else {
            return Err(
                acp::Error::invalid_request().data(format!("unknown session id: {}", session_id.0))
            );
        };
        let response = handle.list_available_commands().await;
        return Ok(acp::ExtResponse::new(Arc::from(
            serde_json::value::to_raw_value(&response)?,
        )));
    }

    // For a given cwd, compute the plugin registry the same way a session would
    // at spawn time (via build_for_cwd) and the same way reload_plugins_impl does
    // (ancestor project config walk + vendor compat merge). This is required so
    // that `echo.agent/commands/list` (the pull used by echo-agent-desktop after session
    // start) returns plugin-provided slash commands for the target cwd.
    //
    // The shared snapshot is only populated at agent boot (using process CWD)
    // and by explicit reloads. In desktop<->docker (and ssh) setups the agent's
    // launch CWD is unrelated to the user's chosen workspace dir, so relying on
    // snapshot() alone meant the post-start pull returned no project plugin
    // skills until the user manually reloaded.
    let plugin_reg = if let Some(cwd_str) = &req.cwd {
        let cwd = Path::new(cwd_str);

        // Folder-trust gates repo-local project plugins (hooks/MCP). Resolve and
        // record the verdict for this cwd (honoring the real remote) BEFORE the
        // plugins-config read below: that read gates its project-paths merge on
        // the recorded verdict, and a cold cwd (client-supplied, no session
        // resolve yet) must not first take the gate's remote-less backstop —
        // that would record a kill-switch-blind deny no later resolve can lift.
        let remote_settings = agent.cfg.borrow().remote_settings.clone();
        let project_trusted =
            crate::agent::folder_trust::resolve_and_record(cwd, remote_settings.as_ref(), false);

        // Effective [plugins] config (global + ancestor project configs +
        // vendor compat merge), shared with reload_plugins_impl and the eager
        // fan-out so the menu agrees with each session's registry for this cwd.
        let disk_cfg = crate::config::resolve_effective_plugins_config(cwd).to_discovery_config();

        // Fresh discovery for *this* cwd (includes .echo-agent/plugins under it, plus
        // the cli --plugin-dir dirs). Does not mutate the shared snapshot.
        agent
            .plugin_registry_handle()
            .build_for_cwd(cwd, &disk_cfg, &[], project_trusted)
    } else {
        // No cwd: global/user skills only (pre-session case). Use the boot snapshot.
        agent.plugin_registry_handle().snapshot()
    };

    let response = crate::session::slash_commands::list_commands(
        req.cwd.as_deref(),
        &skills_config,
        plugin_reg.as_deref(),
        availability,
        compat,
        false,
        req.kind.as_deref(),
        Some(agent.auth_manager.clone()),
    )
    .await?;
    Ok(acp::ExtResponse::new(Arc::from(
        serde_json::value::to_raw_value(&response)?,
    )))
}

// session/fork

async fn handle_session_fork(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    use crate::session::fork::{ForkSessionRequest, fork_session};

    let request: ForkSessionRequest = parse_params(args)?;
    if request.fresh_continuation {
        if let Some(handle) =
            agent.resident_handle(&acp::SessionId::new(request.source_session_id.clone()))
        {
            if handle.is_busy().await {
                return Err(
                    acp::Error::invalid_request().data("请等待当前执行结束后再创建接续对话")
                );
            }
            // A completed reply can still be queued in the persistence actor.
            // Copy only after its conversation and replay log reach disk.
            let (respond_to, ack) = tokio::sync::oneshot::channel();
            handle
                .persistence_tx
                .send(PersistenceMsg::FlushAndAck { respond_to })
                .map_err(|_| acp::Error::internal_error().data("无法保存原会话，请稍后重试"))?;
            tokio::time::timeout(std::time::Duration::from_secs(15), ack)
                .await
                .map_err(|_| acp::Error::internal_error().data("保存原会话超时，请稍后重试"))?
                .map_err(|_| acp::Error::internal_error().data("原会话保存已中断"))?
                .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
        }
    }

    let agent_id = agent_id();
    let response = fork_session(request, &agent_id, Some(agent.auth_manager.clone()))
        .await
        .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;

    to_raw_response(&response)
}

#[cfg(test)]
mod sanitize_rename_title_tests {
    use std::borrow::Cow;

    use crate::session::persistence::sanitize_rename_title;

    #[test]
    fn strips_ascii_controls_and_trims() {
        assert_eq!(
            sanitize_rename_title("  Hello\u{1b}[31mWorld\u{07}  ").as_ref(),
            "Hello[31mWorld"
        );
    }

    #[test]
    fn strips_c1_controls() {
        assert_eq!(
            sanitize_rename_title("ok\u{9b}[31m\u{9c}done").as_ref(),
            "ok[31mdone"
        );
    }

    #[test]
    fn strips_bidi_overrides() {
        assert_eq!(
            sanitize_rename_title("deploy-prod\u{202e}gnp.hs").as_ref(),
            "deploy-prodgnp.hs"
        );
    }

    #[test]
    fn control_only_title_becomes_blank() {
        assert!(sanitize_rename_title("\u{1b}\u{07}\n\t\u{9b}").is_empty());
    }

    #[test]
    fn already_clean_title_is_borrowed() {
        let s = "Fix Login Bug";
        let out = sanitize_rename_title(s);
        assert_eq!(out.as_ref(), s);
        assert!(matches!(out, Cow::Borrowed(_)));

        let padded = "  hello  ";
        let out = sanitize_rename_title(padded);
        assert_eq!(out.as_ref(), "hello");
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn dirty_title_is_owned() {
        let out = sanitize_rename_title("a\u{1b}b");
        assert_eq!(out.as_ref(), "ab");
        assert!(matches!(out, Cow::Owned(_)));
    }

    #[test]
    fn length_is_counted_in_scalars_after_strip() {
        use crate::session::persistence::MAX_TITLE_SCALARS;
        let exact: String = "é".repeat(MAX_TITLE_SCALARS);
        assert_eq!(
            sanitize_rename_title(&exact).chars().count(),
            MAX_TITLE_SCALARS
        );
        let with_controls = format!("\u{1b}{} ", "👍".repeat(MAX_TITLE_SCALARS));
        assert_eq!(
            sanitize_rename_title(&with_controls).chars().count(),
            MAX_TITLE_SCALARS
        );
    }

    #[test]
    fn rename_request_reset_to_auto_deserializes_empty_title_convention() {
        let unpin: super::SessionRenameRequest = serde_json::from_value(serde_json::json!({
            "sessionId": "sid",
            "title": "",
            "cwd": "/repo",
            "resetToAuto": true,
        }))
        .unwrap();
        assert!(unpin.reset_to_auto);
        assert!(unpin.title.is_empty());
        assert_eq!(unpin.kind, crate::session::unified_list::SessionKind::Build);

        let absent_title: super::SessionRenameRequest = serde_json::from_value(serde_json::json!({
            "sessionId": "sid",
            "resetToAuto": true,
        }))
        .unwrap();
        assert!(absent_title.reset_to_auto);
        assert!(absent_title.title.is_empty());

        let ordinary: super::SessionRenameRequest = serde_json::from_value(serde_json::json!({
            "sessionId": "sid",
            "title": "Fix Login Bug",
            "kind": "chat",
        }))
        .unwrap();
        assert!(!ordinary.reset_to_auto);
        assert_eq!(ordinary.title, "Fix Login Bug");
        assert_eq!(
            ordinary.kind,
            crate::session::unified_list::SessionKind::Chat
        );
    }

    #[test]
    fn rename_fanout_meta_serializes_title_is_manual() {
        use crate::extensions::notification::{
            SessionNotification, SessionUpdate, TITLE_IS_MANUAL_META_KEY, title_is_manual_meta,
        };
        let n = SessionNotification {
            session_id: agent_client_protocol::SessionId::new("s"),
            update: SessionUpdate::SessionSummaryGenerated {
                session_summary: "raw & title".into(),
            },
            meta: Some(title_is_manual_meta()),
        };
        let v = serde_json::to_value(&n).unwrap();
        assert_eq!(v["_meta"][TITLE_IS_MANUAL_META_KEY], true);
        assert_eq!(v["update"]["session_summary"], "raw & title");
    }
}
