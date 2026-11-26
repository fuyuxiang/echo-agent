use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::auth::error::RefreshTokenFailedReason;
use crate::auth::manager::RefreshReason;
use crate::auth::oidc::OidcRefreshResult;

use super::{AuthSnapshot, DiagnosticUploader, RefreshOutcome, TokenRefresher};

#[cfg(test)]
use crate::auth::manager::AuthManager;

/// Escalate to `PermanentFailure` after this many consecutive transient
/// failures (then `PERMANENT_FAILURE_TTL` allows recovery). OIDC tolerates more
/// blips than `ExternalBinaryRefresher` (1) since network refreshes flake more
/// than a local binary. Kept above `try_recover_unauthorized`'s per-recovery
/// attempt budget so one 401 recovery cannot alone escalate.
const MAX_CONSECUTIVE_TRANSIENT_FAILURES: u32 = 5;

/// Consecutive transient-failure budget, scoped to the credential it accrued
/// against. Held under one lock so the credential check, reset, and increment
/// are a single atomic step.
#[derive(Default)]
struct TransientBudget {
    /// Credential the count belongs to. A different credential (e.g. after
    /// re-login on this long-lived refresher) re-arms the budget so a fresh,
    /// valid token never inherits a dead one's escalation.
    key: Option<String>,
    count: u32,
}

pub(crate) struct OidcRefresher {
    auth: Arc<dyn AuthSnapshot>,
    diagnostic_uploader: Option<DiagnosticUploader>,
    upload_in_flight: Arc<AtomicBool>,
    transient_budget: parking_lot::Mutex<TransientBudget>,
}

impl OidcRefresher {
    pub(crate) fn new(auth: Arc<dyn AuthSnapshot>) -> Self {
        Self {
            auth,
            diagnostic_uploader: None,
            upload_in_flight: Arc::new(AtomicBool::new(false)),
            transient_budget: parking_lot::Mutex::new(TransientBudget::default()),
        }
    }

    pub(crate) fn with_diagnostic_upload(mut self, uploader: DiagnosticUploader) -> Self {
        self.diagnostic_uploader = Some(uploader);
        self
    }

    /// Clear the transient-blip budget on refresh progress (a fresh token or an
    /// adopted sibling token), so later blips start from a full budget.
    fn note_refresh_progress(&self) {
        *self.transient_budget.lock() = TransientBudget::default();
    }

    fn record_transient_failure(
        &self,
        message: String,
        tried_key: Option<String>,
        network_unreachable: bool,
    ) -> RefreshOutcome {
        // Never reached the IdP → proves nothing about the credential: don't
        // consume the escalation budget (and don't reset it — only real
        // refresh progress does). See `OidcRefreshResult::Failed`.
        if network_unreachable {
            tracing::debug!(%message, "auth: transient refresh failure (network unreachable), not counted toward escalation");
            return RefreshOutcome::transient(message);
        }
        let escalate = {
            let mut budget = self.transient_budget.lock();
            // Re-arm when the credential changes so a fresh token never inherits
            // a prior credential's accrued blips.
            if budget.key != tried_key {
                budget.key = tried_key.clone();
                budget.count = 0;
            }
            budget.count += 1;
            let escalate = budget.count >= MAX_CONSECUTIVE_TRANSIENT_FAILURES;
            // On escalation reset the count so the next TTL window gets the full
            // budget (the verdict gates refresh() meanwhile). The key is left in
            // place; a same-key retry resumes from zero, a new key re-arms.
            if escalate {
                budget.count = 0;
            }
            escalate
        };
        if escalate {
            tracing::warn!(%message, "auth: escalating consecutive transient failures to permanent");
            RefreshOutcome::permanent(RefreshTokenFailedReason::Other, tried_key)
        } else {
            RefreshOutcome::transient(message)
        }
    }

    /// One-shot retry with disk's RT after `invalid_grant`.
    ///
    /// If disk already has a valid (unexpired) AT with a different key,
    /// adopt it directly, without consuming the disk's RT in another IdP
    /// call. This prevents cascading `invalid_grant` when a sibling
    /// already refreshed and wrote a valid token.
    async fn retry_with_fresh_disk_token(
        &self,
        tried: &crate::auth::EchoAgentAuth,
    ) -> Option<RefreshOutcome> {
        let disk_now = self.auth.read_disk_auth()?;

        // If disk has a valid AT that differs from what we tried,
        // a sibling already refreshed. Adopt directly — no IdP call.
        if !crate::auth::is_expired(&disk_now) && disk_now.key != tried.key {
            crate::unified_log::info(
                "oidc refresh: disk has valid AT, adopting instead of consuming RT",
                None,
                Some(serde_json::json!({
                    "disk_key_prefix": echo_agent_auth::bearer_suffix(&disk_now.key),
                    "tried_key_prefix": echo_agent_auth::bearer_suffix(&tried.key),
                })),
            );
            self.note_refresh_progress();
            return Some(RefreshOutcome::success(disk_now));
        }

        if disk_now.refresh_token.is_none()
            || disk_now.refresh_token.as_deref() == tried.refresh_token.as_deref()
        {
            return None;
        }

        crate::unified_log::info(
            "oidc refresh retrying with disk token",
            None,
            Some(serde_json::json!({
                "tried_rt_prefix": tried
                    .refresh_token
                    .as_deref()
                    .map(echo_agent_auth::bearer_suffix),
                "disk_rt_prefix": disk_now
                    .refresh_token
                    .as_deref()
                    .map(echo_agent_auth::bearer_suffix),
            })),
        );

        match crate::auth::oidc::oidc_token_exchange(&disk_now).await {
            OidcRefreshResult::Success(new_auth) => {
                self.note_refresh_progress();
                Some(RefreshOutcome::Success(new_auth))
            }
            OidcRefreshResult::TerminalError { reason } => {
                crate::unified_log::warn(
                    "oidc refresh disk retry exhausted",
                    None,
                    Some(serde_json::json!({ "reason": format!("{reason:?}") })),
                );
                Some(RefreshOutcome::permanent_for(reason, &disk_now))
            }
            OidcRefreshResult::Failed { .. } => {
                Some(RefreshOutcome::transient("OIDC disk-retry refresh failed"))
            }
        }
    }
}

#[async_trait::async_trait]
impl TokenRefresher for OidcRefresher {
    async fn refresh(&self, reason: RefreshReason) -> RefreshOutcome {
        crate::unified_log::debug(
            "oidc refresh enter",
            None,
            Some(serde_json::json!({
                "reason": format!("{reason:?}"),
                "has_current": self.auth.current().is_some(),
                "is_expired": self.auth.is_expired(),
            })),
        );

        let disk_auth = self.auth.read_disk_auth();

        // Short-circuit: if disk has a valid unexpired AT that differs
        // from in-memory, a sibling refreshed between refresh_chain
        // step 2 (disk check under lock) and here. Adopt it directly,
        // no IdP call needed.
        if let Some(ref d) = disk_auth
            && !crate::auth::is_expired(d)
            && self.auth.current().map(|a| a.key).as_deref() != Some(&d.key)
        {
            crate::unified_log::info(
                "oidc refresh: sibling refreshed, adopting valid disk AT",
                None,
                Some(serde_json::json!({
                    "disk_key_prefix": echo_agent_auth::bearer_suffix(&d.key),
                })),
            );
            self.note_refresh_progress();
            return RefreshOutcome::success(d.clone());
        }

        let auth = super::resolve_refresh_credential(self.auth.as_ref(), disk_auth, reason);

        let Some(auth) = auth else {
            crate::unified_log::warn(
                "oidc refresh no token available",
                None,
                Some(serde_json::json!({ "reason": format!("{reason:?}") })),
            );
            return RefreshOutcome::transient("no token with refresh_token available");
        };

        crate::unified_log::info(
            "oidc refresh attempting idp",
            None,
            Some(serde_json::json!({
                "has_rt": auth.refresh_token.is_some(),
                "issuer": auth.oidc_issuer,
                "client_id": auth.oidc_client_id,
                "expires_at": auth.expires_at.map(|e| e.to_rfc3339()),
            })),
        );

        // Snapshot for diagnostic upload on failure (user id, never email).
        let pre_token = echo_agent_auth::bearer_suffix(&auth.key).to_owned();
        let pre_user_id = if auth.user_id.is_empty() {
            "unknown".into()
        } else {
            auth.user_id.clone()
        };

        match crate::auth::oidc::oidc_token_exchange(&auth).await {
            OidcRefreshResult::Success(new_auth) => {
                self.note_refresh_progress();
                RefreshOutcome::Success(new_auth)
            }
            OidcRefreshResult::TerminalError { reason } => {
                // Sibling-rotation race: disk may hold a
                // fresher RT than the one we tried. One-shot retry.
                if reason == RefreshTokenFailedReason::RefreshTokenRejected
                    && let Some(retry_outcome) = self.retry_with_fresh_disk_token(&auth).await
                {
                    return retry_outcome;
                }

                if let Some(uploader) = &self.diagnostic_uploader {
                    spawn_diagnostic_upload(
                        uploader,
                        pre_token,
                        pre_user_id,
                        &self.upload_in_flight,
                    );
                }
                RefreshOutcome::permanent_for(reason, &auth)
            }
            OidcRefreshResult::Failed {
                network_unreachable,
            } => {
                tracing::warn!(
                    refresh_reason = ?reason,
                    user_id = %auth.user_id,
                    has_refresh_token = auth.refresh_token.is_some(),
                    network_unreachable,
                    issuer = ?auth.oidc_issuer,
                    client_id = ?auth.oidc_client_id,
                    expires_at = ?auth.expires_at,
                    "auth: OIDC token refresh failed"
                );
                crate::unified_log::error(
                    "oidc refresh failed",
                    None,
                    Some(serde_json::json!({
                        "has_refresh_token": auth.refresh_token.is_some(),
                        "auth_mode": format!("{:?}", auth.auth_mode),
                        "network_unreachable": network_unreachable,
                        "issuer": auth.oidc_issuer,
                        "client_id": auth.oidc_client_id,
                        "expires_at": auth.expires_at.map(|e| e.to_rfc3339()),
                    })),
                );
                if let Some(uploader) = &self.diagnostic_uploader {
                    spawn_diagnostic_upload(
                        uploader,
                        pre_token,
                        pre_user_id,
                        &self.upload_in_flight,
                    );
                }
                self.record_transient_failure(
                    "OIDC token refresh failed".into(),
                    Some(auth.key.clone()),
                    network_unreachable,
                )
            }
        }
    }
}

/// Fire-and-forget diagnostic log upload. Guarded against concurrent spawns.
/// `user_id` is the GCS path segment (never email).
fn spawn_diagnostic_upload(
    uploader: &DiagnosticUploader,
    auth_token: String,
    user_id: String,
    in_flight: &Arc<AtomicBool>,
) {
    if in_flight
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        tracing::debug!("auth: diagnostic upload already in flight, skipping");
        return;
    }

    let in_flight = in_flight.clone();
    let uploader = uploader.clone();

    tokio::spawn(async move {
        // snapshot_log() holds a mutex, flushes, and reads up to 5 MB —
        // run it on a blocking thread to avoid stalling the tokio executor.
        let log_bytes = match tokio::task::spawn_blocking(crate::unified_log::snapshot_log).await {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                crate::unified_log::debug("diagnostic snapshot empty", None, None);
                in_flight.store(false, Ordering::Release);
                return;
            }
            Err(e) => {
                tracing::debug!(error = %e, "auth: snapshot_log task failed");
                crate::unified_log::error(
                    "diagnostic snapshot failed",
                    None,
                    Some(serde_json::json!({ "error": format!("{e}") })),
                );
                in_flight.store(false, Ordering::Release);
                return;
            }
        };

        uploader(log_bytes, auth_token, user_id).await;
        in_flight.store(false, Ordering::Release);
    });
}

#[cfg(test)]
#[path = "oidc_refresher_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "auth_backend_contract_tests.rs"]
mod auth_backend_contract_tests;
