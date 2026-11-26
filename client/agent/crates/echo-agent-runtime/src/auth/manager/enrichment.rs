//! Background `/user` enrichment spawned by `AuthManager::update()`.

use std::sync::Arc;
use std::time::Duration as StdDuration;

use super::AuthManager;
use super::lock::{Heartbeat, try_lock_auth_file_async};
use crate::auth::manager::AUTH_LOCK_TIMEOUT;
use crate::auth::model::{EchoAgentAuth, UserInfo, lookup_auth};
use crate::auth::storage::{read_auth_json, write_auth_json};

/// `/user` fetch budget, shared by the inline (login) and background paths.
const USER_FETCH_TIMEOUT: StdDuration = StdDuration::from_secs(10);

/// Logs `auth update enrichment dropped` if the task is cancelled
/// mid-flight. Disarmed on normal completion.
pub(super) struct EnrichmentExitGuard {
    pub(super) started: std::time::Instant,
    pub(super) armed: bool,
}

impl EnrichmentExitGuard {
    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for EnrichmentExitGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        echo_agent_telemetry::unified_log::warn(
            "auth update enrichment dropped",
            None,
            Some(serde_json::json!({
                "elapsed_ms": self.started.elapsed().as_millis() as u64,
            })),
        );
    }
}

pub(super) fn spawn(manager: Arc<AuthManager>, auth: EchoAgentAuth) {
    tokio::spawn(async move {
        let mut exit_guard = EnrichmentExitGuard {
            started: std::time::Instant::now(),
            armed: true,
        };
        run_user_info_enrichment(&manager, auth).await;
        exit_guard.disarm();
    });
}

async fn fetch_user_info(manager: &AuthManager, key: &str, log_label: &str) -> Option<UserInfo> {
    let user_url = format!("{}/user", manager.proxy_base_url);
    let token_header = &manager.echo_agent_com_config.token_header;
    let started = std::time::Instant::now();
    let http_client = crate::http::shared_client();
    let response = http_client
        .get(&user_url)
        .timeout(USER_FETCH_TIMEOUT)
        .header("Authorization", format!("Bearer {}", key))
        .header("X-Echo-Agent-Token-Auth", token_header.as_str())
        .header("x-echo-agent-client-version", echo_agent_version::VERSION)
        .header(
            crate::http::CLIENT_MODE_HEADER,
            crate::http::process_client_mode(),
        )
        .send()
        .await;

    match response {
        Ok(resp) if resp.status().is_success() => match resp.json::<UserInfo>().await {
            Ok(ui) if !ui.user_id.is_empty() => Some(ui),
            Ok(_) => {
                echo_agent_telemetry::unified_log::warn(
                    &format!("{log_label} skipped"),
                    None,
                    Some(serde_json::json!({
                        "reason": "empty_user_id",
                        "elapsed_ms": started.elapsed().as_millis() as u64,
                    })),
                );
                None
            }
            Err(e) => {
                echo_agent_telemetry::unified_log::warn(
                    &format!("{log_label} failed"),
                    None,
                    Some(serde_json::json!({
                        "reason": "parse",
                        "error": e.to_string(),
                        "elapsed_ms": started.elapsed().as_millis() as u64,
                    })),
                );
                None
            }
        },
        Ok(resp) => {
            echo_agent_telemetry::unified_log::warn(
                &format!("{log_label} failed"),
                None,
                Some(serde_json::json!({
                    "reason": "http_status",
                    "http_status": resp.status().as_u16(),
                    "elapsed_ms": started.elapsed().as_millis() as u64,
                })),
            );
            None
        }
        Err(e) => {
            echo_agent_telemetry::unified_log::warn(
                &format!("{log_label} failed"),
                None,
                Some(serde_json::json!({
                    "reason": if e.is_timeout() { "timeout" } else { "transport" },
                    "error": e.to_string(),
                    "elapsed_ms": started.elapsed().as_millis() as u64,
                })),
            );
            None
        }
    }
}

/// Blocking login-time enrichment: merge `/user` fields before the first save.
pub(super) async fn enrich_inline(manager: &AuthManager, auth: &mut EchoAgentAuth) {
    let Some(ui) = fetch_user_info(manager, &auth.key, "auth login enrichment").await else {
        return;
    };
    apply_user_info_enrichment(auth, ui);
}

async fn run_user_info_enrichment(manager: &AuthManager, auth: EchoAgentAuth) {
    let started = std::time::Instant::now();
    let Some(user_info) = fetch_user_info(manager, &auth.key, "auth update enrichment").await
    else {
        return;
    };
    let user_elapsed_ms = started.elapsed().as_millis() as u64;

    // R-M-W file lock. On timeout, drop the write rather than proceed
    // unlocked: an unlocked R-M-W can silently revert a freshly rotated
    // AT/RT on disk (a rolled-back RT is a future `invalid_grant` → forced
    // re-login). Enrichment is cosmetic; it re-runs on the next refresh.
    let lock_started = std::time::Instant::now();
    let lock_guard = try_lock_auth_file_async(&manager.path, AUTH_LOCK_TIMEOUT, Heartbeat::Skip)
        .await
        .into_guard();
    let lock_wait_ms = lock_started.elapsed().as_millis() as u64;
    let Some(_lock_guard) = lock_guard else {
        echo_agent_telemetry::unified_log::warn(
            "auth update enrichment skipped",
            None,
            Some(serde_json::json!({
                "reason": "lock_timeout",
                "lock_wait_ms": lock_wait_ms,
            })),
        );
        return;
    };

    let Ok(mut map) = read_auth_json(&manager.path) else {
        echo_agent_telemetry::unified_log::warn(
            "auth update enrichment skipped",
            None,
            Some(serde_json::json!({ "reason": "read_disk_failed" })),
        );
        return;
    };
    let Some(mut disk) = lookup_auth(&map, &manager.scope) else {
        echo_agent_telemetry::unified_log::info(
            "auth update enrichment skipped",
            None,
            Some(serde_json::json!({ "reason": "no_disk_auth" })),
        );
        return;
    };
    // Sibling-stomp guard. If either the access token or refresh
    // token on disk differs from the one we wrote, a sibling process
    // rotated tokens since our update(). Skip enrichment to avoid
    // writing stale profile data over the sibling's fresher entry.
    //
    // OR logic (not AND): a single-field rotation (key changes, RT
    // stays) is the common case during concurrent refresh. The old
    // AND logic required ALL three fields to differ, letting
    // single-field rotations through.
    //
    // Team-login transitions (placeholder→real user_id) don't rotate
    // tokens, so OR correctly allows enrichment for that case.
    if disk.key != auth.key || disk.refresh_token != auth.refresh_token {
        echo_agent_telemetry::unified_log::info(
            "auth update enrichment skipped",
            None,
            Some(serde_json::json!({
                "reason": "sibling_rotated",
                "written_key_prefix": echo_agent_auth::bearer_suffix(&auth.key),
                "disk_key_prefix": echo_agent_auth::bearer_suffix(&disk.key),
            })),
        );
        return;
    }

    apply_user_info_enrichment(&mut disk, user_info);

    map.insert(manager.scope.clone(), disk.clone());
    let write_started = std::time::Instant::now();
    if let Err(e) = write_auth_json(&manager.path, &map) {
        echo_agent_telemetry::unified_log::error(
            "auth update enrichment write failed",
            None,
            Some(serde_json::json!({
                "error": e.to_string(),
                "user_ms": user_elapsed_ms,
                "lock_wait_ms": lock_wait_ms,
                "write_ms": write_started.elapsed().as_millis() as u64,
            })),
        );
        return;
    }
    manager.with_inner_write(|inner| *inner = Some(disk));
    echo_agent_telemetry::unified_log::info(
        "auth update enrichment done",
        None,
        Some(serde_json::json!({
            "user_ms": user_elapsed_ms,
            "lock_wait_ms": lock_wait_ms,
            "write_ms": write_started.elapsed().as_millis() as u64,
            "total_ms": started.elapsed().as_millis() as u64,
        })),
    );
}

/// Merge enrichment fields into disk auth. Does not touch token fields.
pub(super) fn apply_user_info_enrichment(disk: &mut EchoAgentAuth, user_info: UserInfo) {
    disk.user_id = user_info.user_id;
    disk.first_name = user_info.first_name.or(disk.first_name.take());
    disk.last_name = user_info.last_name.or(disk.last_name.take());
    disk.profile_image_asset_id = user_info
        .profile_image_asset_id
        .or(disk.profile_image_asset_id.take());
    disk.principal_type = user_info.principal_type.or(disk.principal_type.take());
    disk.principal_id = user_info.principal_id.or(disk.principal_id.take());
    disk.team_id = user_info.team_id.or(disk.team_id.take());
    disk.team_name = user_info.team_name.or(disk.team_name.take());
    disk.team_role = user_info.team_role.or(disk.team_role.take());
    disk.organization_id = user_info.organization_id.or(disk.organization_id.take());
    disk.organization_name = user_info
        .organization_name
        .or(disk.organization_name.take());
    disk.organization_role = user_info
        .organization_role
        .or(disk.organization_role.take());
    disk.user_blocked_reason = user_info
        .user_blocked_reason
        .or(disk.user_blocked_reason.take());
    if let Some(reasons) = user_info.team_blocked_reasons {
        disk.team_blocked_reasons = reasons;
    }
    if let Some(opt_out) = user_info.coding_data_retention_opt_out {
        disk.coding_data_retention_opt_out = opt_out;
    }
    if let Some(ref email) = user_info.email
        && !email.is_empty()
    {
        disk.email = user_info.email;
    }
}
