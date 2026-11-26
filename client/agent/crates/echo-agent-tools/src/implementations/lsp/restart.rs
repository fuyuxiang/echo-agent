//! Monitors LSP servers for crashes and auto-restarts them.

use std::path::PathBuf;
use std::sync::{Arc, Weak};

use async_lsp::lsp_types::Url;

use super::client::LspClient;
use super::config::LspServerConfig;
use super::manager::LspManager;
use super::{DiagnosticsNotify, file_uri};
use crate::util::ProcessScope;

/// Waits for the current lifecycle to exit. Returns `None` (stop monitoring)
/// when the manager is gone or the server's client has been removed.
///
/// See `restart_monitor` for the Weak/lifetime argument.
async fn wait_for_crashed_lifecycle(
    lsp_manager: &Weak<tokio::sync::Mutex<LspManager>>,
    server_name: &str,
    poll_interval: std::time::Duration,
) -> Option<u64> {
    loop {
        tokio::time::sleep(poll_interval).await;
        let mgr_arc = lsp_manager.upgrade()?;
        let mgr = mgr_arc.lock().await;
        match mgr.clients.get(server_name) {
            Some(client) if client.main_loop.is_finished() => return Some(client.lifecycle_id),
            Some(_) => continue,
            None => return None,
        }
    }
}

/// Replays tracked documents, returning each URI with the document version its
/// replay was sent as — what the manager needs to tell a verdict on the replay
/// from a leftover one. Documents the fresh server was never told about are
/// left out, so nothing waits on a verdict that was never asked for.
pub(super) fn replay_tracked_documents(
    restarted_client: &mut LspClient,
    tracked_docs: &[(String, String)],
) -> Vec<(Url, i32)> {
    tracked_docs
        .iter()
        .filter_map(|(uri_str, lang_id)| {
            let path = uri_str
                .strip_prefix("file://")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(uri_str));
            let content = std::fs::read_to_string(&path).ok()?;
            let uri = file_uri(&path).ok()?;
            let version = restarted_client.notify_file_change(&path, &content, lang_id)?;
            Some((uri, version))
        })
        .collect()
}

/// Removes the crashed client if it is still current and returns restart state.
async fn take_crashed_client_if_current(
    lsp_manager: &Arc<tokio::sync::Mutex<LspManager>>,
    server_name: &str,
    crashed_lifecycle_id: u64,
) -> Option<RestartContext> {
    let mut mgr = lsp_manager.lock().await;
    let current = mgr.clients.get(server_name)?;
    if current.lifecycle_id != crashed_lifecycle_id {
        return None;
    }

    let tracked_docs = mgr
        .clients
        .remove(server_name)
        .map(|client| client.tracked_documents())
        .unwrap_or_default();
    let server_config = mgr.servers.get(server_name).cloned()?;
    let next_lifecycle_id = mgr.alloc_lifecycle_id();
    Some(RestartContext {
        next_lifecycle_id,
        server_config,
        workspace_root: mgr.workspace_root.clone(),
        diagnostics_notify: mgr.diagnostics_ready.clone(),
        tracked_docs,
        process_scope: mgr.process_scope.clone(),
    })
}

/// Removes the crashed client if it is still current.
async fn discard_crashed_client_if_current(
    lsp_manager: &Arc<tokio::sync::Mutex<LspManager>>,
    server_name: &str,
    crashed_lifecycle_id: u64,
) -> bool {
    let mut mgr = lsp_manager.lock().await;
    let Some(current) = mgr.clients.get(server_name) else {
        return false;
    };
    if current.lifecycle_id != crashed_lifecycle_id {
        return false;
    }
    mgr.clients.remove(server_name);
    true
}

/// Installs a restarted client unless shutdown has begun.
async fn install_restarted_client(
    lsp_manager: &Arc<tokio::sync::Mutex<LspManager>>,
    server_name: &str,
    restarted_client: LspClient,
    replayed_uris: Vec<(Url, i32)>,
) -> Result<(), LspClient> {
    let mut mgr = lsp_manager.lock().await;
    if mgr.shutting_down {
        return Err(restarted_client);
    }
    let lifecycle_id = restarted_client.lifecycle_id;
    for (uri, version) in replayed_uris {
        mgr.mark_uri_pending_diagnostics(server_name, lifecycle_id, uri, version);
    }
    mgr.clients
        .insert(server_name.to_string(), restarted_client);
    Ok(())
}

struct RestartContext {
    next_lifecycle_id: u64,
    server_config: LspServerConfig,
    workspace_root: PathBuf,
    diagnostics_notify: DiagnosticsNotify,
    tracked_docs: Vec<(String, String)>,
    /// Snapshotted under the same manager lock as the rest of the context so
    /// each restart enrolls without an extra lock roundtrip.
    process_scope: Option<ProcessScope>,
}

enum RestartOutcome {
    Restarted {
        attempts: u32,
        lifecycle_id: u64,
        replayed_doc_count: usize,
    },
    Exhausted {
        attempts: u32,
        error: String,
    },
    Shutdown,
}

async fn send_failed_notification(
    lsp_manager: &Arc<tokio::sync::Mutex<LspManager>>,
    server_name: &str,
    error: String,
    attempts: u32,
) {
    lsp_manager
        .lock()
        .await
        .notification_handle
        .send_lsp_failed(crate::notification::LspServerFailed {
            server_name: server_name.to_string(),
            error,
            attempts,
        });
}

async fn alloc_lifecycle_id_if_running(
    lsp_manager: &Arc<tokio::sync::Mutex<LspManager>>,
) -> Option<u64> {
    let mut mgr = lsp_manager.lock().await;
    if mgr.shutting_down {
        None
    } else {
        Some(mgr.alloc_lifecycle_id())
    }
}

async fn restart_lsp_with_retries(
    lsp_manager: &Arc<tokio::sync::Mutex<LspManager>>,
    server_name: &str,
    restart_ctx: RestartContext,
    attempts: &mut u32,
    max_restarts: u32,
    backoff: &mut std::time::Duration,
    max_backoff: std::time::Duration,
) -> RestartOutcome {
    let RestartContext {
        mut next_lifecycle_id,
        server_config,
        workspace_root,
        diagnostics_notify,
        tracked_docs,
        process_scope,
    } = restart_ctx;

    loop {
        *attempts += 1;
        tokio::time::sleep(*backoff).await;

        lsp_manager
            .lock()
            .await
            .notification_handle
            .send_lsp_starting(crate::notification::LspServerStarting {
                server_name: server_name.to_string(),
                command: server_config.command.clone(),
            });

        match LspClient::start(
            server_name.to_string(),
            next_lifecycle_id,
            server_config.clone(),
            &workspace_root,
            diagnostics_notify.clone(),
        )
        .await
        {
            Ok(mut restarted_client) => {
                if !restarted_client.enroll(process_scope.as_ref()) {
                    // Closed scope == session teardown killed the child at
                    // registration; drop the client rather than install a dead
                    // server and churn through further respawns.
                    tracing::info!(server = %server_name, "session scope closed, dropping restarted server");
                    return RestartOutcome::Shutdown;
                }
                let replayed_doc_count = tracked_docs.len();
                let replayed_uris = replay_tracked_documents(&mut restarted_client, &tracked_docs);
                // Re-check after the replay window: a `kill_all` between enroll
                // and install has already SIGKILLed the enrolled child, and
                // `install` only checks `shutting_down` (never set by
                // `kill_all`) — installing here would mark a dead server ready.
                if process_scope.as_ref().is_some_and(|s| s.is_closed()) {
                    tracing::info!(server = %server_name, "session scope closed during restart, dropping restarted server");
                    return RestartOutcome::Shutdown;
                }
                if let Err(restarted_client) = install_restarted_client(
                    lsp_manager,
                    server_name,
                    restarted_client,
                    replayed_uris,
                )
                .await
                {
                    tracing::info!(server = %server_name, "session shutting down, dropping restarted server");
                    restarted_client.shutdown().await;
                    return RestartOutcome::Shutdown;
                }
                *backoff = std::time::Duration::from_secs(1);
                return RestartOutcome::Restarted {
                    attempts: *attempts,
                    lifecycle_id: next_lifecycle_id,
                    replayed_doc_count,
                };
            }
            Err(e) => {
                if *attempts >= max_restarts {
                    tracing::warn!(
                        server = %server_name,
                        attempt = *attempts,
                        lifecycle_id = next_lifecycle_id,
                        error = %e,
                        "LSP server restart failed, max restarts exceeded - giving up"
                    );
                    return RestartOutcome::Exhausted {
                        attempts: *attempts,
                        error: e.to_string(),
                    };
                }

                tracing::warn!(
                    server = %server_name,
                    attempt = *attempts,
                    lifecycle_id = next_lifecycle_id,
                    error = %e,
                    next_backoff_ms = ((*backoff) * 2).min(max_backoff).as_millis() as u64,
                    "LSP server restart failed, will retry"
                );
                *backoff = ((*backoff) * 2).min(max_backoff);
                let Some(allocated_lifecycle_id) = alloc_lifecycle_id_if_running(lsp_manager).await
                else {
                    return RestartOutcome::Shutdown;
                };
                next_lifecycle_id = allocated_lifecycle_id;
            }
        }
    }
}

/// Monitors one server entry and replaces crashed lifecycles.
///
/// Takes a `Weak` to the manager so the monitor never keeps the `LspManager`
/// (and its child processes) alive past the owning session: it upgrades only
/// briefly per poll and for the duration of a single restart. When the manager
/// is dropped at session teardown, the next upgrade fails and the monitor exits.
pub async fn restart_monitor(
    lsp_manager: Weak<tokio::sync::Mutex<LspManager>>,
    server_name: String,
) {
    // Lifetime restart budget for this server monitor. Successful restarts
    // reset backoff but do not reset the attempt counter.
    let mut attempts: u32 = 0;
    let mut backoff = std::time::Duration::from_secs(1);
    const MAX_BACKOFF: std::time::Duration = std::time::Duration::from_secs(30);
    const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

    loop {
        let Some(crashed_lifecycle_id) =
            wait_for_crashed_lifecycle(&lsp_manager, &server_name, POLL_INTERVAL).await
        else {
            return;
        };

        // Upgrade to a strong ref only for this restart; stop if the manager was
        // dropped between crash detection and here.
        let Some(lsp_manager) = lsp_manager.upgrade() else {
            return;
        };

        let max_restarts = {
            let mgr = lsp_manager.lock().await;
            mgr.servers
                .get(&server_name)
                .map(|c| c.max_restarts())
                .unwrap_or(3)
        };

        if attempts >= max_restarts {
            if !discard_crashed_client_if_current(&lsp_manager, &server_name, crashed_lifecycle_id)
                .await
            {
                continue;
            }
            tracing::warn!(
                server = %server_name,
                attempts,
                max = max_restarts,
                lifecycle_id = crashed_lifecycle_id,
                "LSP server crashed, max restarts exceeded - giving up"
            );
            send_failed_notification(
                &lsp_manager,
                &server_name,
                "process exited".into(),
                attempts,
            )
            .await;
            return;
        }

        tracing::info!(
            server = %server_name,
            next_attempt = attempts + 1,
            max = max_restarts,
            lifecycle_id = crashed_lifecycle_id,
            backoff_ms = backoff.as_millis() as u64,
            "LSP server crashed, restarting after backoff"
        );
        lsp_manager
            .lock()
            .await
            .notification_handle
            .send_lsp_crashed(crate::notification::LspServerCrashed {
                server_name: server_name.clone(),
            });
        lsp_manager
            .lock()
            .await
            .notification_handle
            .send_lsp_retrying(crate::notification::LspServerRetrying {
                server_name: server_name.clone(),
                attempt: attempts + 1,
                max_restarts,
                backoff_ms: backoff.as_millis() as u64,
            });

        let Some(restart) =
            take_crashed_client_if_current(&lsp_manager, &server_name, crashed_lifecycle_id).await
        else {
            continue;
        };

        match restart_lsp_with_retries(
            &lsp_manager,
            &server_name,
            restart,
            &mut attempts,
            max_restarts,
            &mut backoff,
            MAX_BACKOFF,
        )
        .await
        {
            RestartOutcome::Restarted {
                attempts,
                lifecycle_id,
                replayed_doc_count,
            } => {
                tracing::info!(
                    server = %server_name,
                    attempt = attempts,
                    lifecycle_id,
                    replayed_docs = replayed_doc_count,
                    "LSP server ready after restart"
                );
                lsp_manager.lock().await.notification_handle.send_lsp_ready(
                    crate::notification::LspServerReady {
                        server_name: server_name.clone(),
                    },
                );
            }
            RestartOutcome::Exhausted { attempts, error } => {
                send_failed_notification(&lsp_manager, &server_name, error, attempts).await;
                return;
            }
            RestartOutcome::Shutdown => return,
        }
    }
}
