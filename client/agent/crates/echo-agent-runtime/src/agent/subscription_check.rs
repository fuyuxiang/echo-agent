//! Subscription check for paywall gate lift.
//!
//! Provides `single_check()` which queries `GET /user?include=subscription`
//! for the live subscription tier from the backend, independent of the JWT.
//! If a qualifying tier is detected, does a best-effort JWT refresh and
//! returns an `UnblockResult` so the agent can re-fetch settings and lift
//! the gate through its own settings seam.
//!
//! The pager drives the polling via `echo.agent/auth/check_subscription`: the 5s
//! paywall chain, the free-tier watch, the refocus check, and
//! verify-before-paywall gate deferral (see the pager's `app::subscription`
//! module).
use crate::auth::AuthManager;
use crate::auth::UserInfo;
use crate::auth::manager::{BEST_EFFORT_REFRESH_TIMEOUT, BoundedRefresh, RefreshReason};
use crate::auth::token_type::TokenType;
use std::sync::Arc;
use std::time::Duration;
/// Whether a `/user?include=subscription` tier qualifies for EchoAgent Build
/// access. Any active subscription qualifies -- the proxy only returns a
/// tier when an active subscription exists (`None` otherwise), and the
/// access gate in remote settings controls which tiers are actually
/// allowed. The `"Free"` guard is defense-in-depth should the proxy ever
/// start stamping free users explicitly.
fn is_qualifying_tier(tier: &str) -> bool {
    !tier.is_empty() && tier != "Free"
}
/// Successful subscription check result: a confirmed qualifying tier.
pub(crate) struct UnblockResult {
    pub(crate) new_tier: String,
    /// The proxy-canonical `userId` from the `/user` response that confirmed
    /// the tier — resolved with the live bearer, so it names the same account
    /// the check started with. The caller's identity guard accepts it
    /// alongside the started user_id: the mint below spawns a `/user`
    /// enrichment that can rewrite a seeded/stale user_id to this canonical
    /// value mid-check, and that normalization is not an account switch.
    pub(crate) canonical_user_id: String,
    /// True when the best-effort refresh below hit its bounded deadline with
    /// the exchange still in flight (spawn-don't-drop). The caller must not
    /// force a second mint then — it would only queue behind the detached
    /// exchange for up to another full budget, holding the gate lift past the
    /// documented single budget while the subscription is already confirmed.
    pub(crate) refresh_deadline_hit: bool,
}
/// Fetch `/user?include=subscription` and return the parsed `UserInfo`.
async fn fetch_user_info(
    http_client: &reqwest::Client,
    url: &str,
    auth: &crate::auth::EchoAgentAuth,
    auth_manager: &AuthManager,
    alpha_test_key: Option<&str>,
) -> Result<UserInfo, &'static str> {
    let request = http_client
        .get(url)
        .timeout(Duration::from_secs(10))
        .header("Authorization", format!("Bearer {}", auth.key))
        .header(
            "X-Echo-Agent-Token-Auth",
            auth_manager.echo_agent_com_config().token_header.as_str(),
        )
        .header("x-echo-agent-client-version", echo_agent_version::VERSION)
        .header(
            crate::http::CLIENT_MODE_HEADER,
            crate::http::process_client_mode(),
        );
    let _ = alpha_test_key;
    match request.send().await {
        Ok(resp) if resp.status().is_success() => {
            resp.json::<UserInfo>().await.map_err(|_| "parse")
        }
        Ok(_resp) => Err("http_status"),
        Err(e) if e.is_timeout() => Err("timeout"),
        Err(_) => Err("transport"),
    }
}
/// Single-shot subscription check. Called by the pager every 5s while
/// the paywall is shown (`echo.agent/auth/check_subscription`).
///
/// Queries `/user?include=subscription` for the live tier. If a qualifying
/// tier is found, does a best-effort JWT refresh and returns
/// `Some(UnblockResult)`. Returns `None` if no qualifying subscription
/// exists or the request fails.
#[tracing::instrument(name = "paywall_check", skip_all, fields(user_id = %user_id))]
pub(crate) async fn single_check(
    auth_manager: Arc<AuthManager>,
    proxy_base_url: &str,
    alpha_test_key: Option<&str>,
    user_id: &str,
) -> Option<UnblockResult> {
    let user_url = format!("{}/user?include=subscription", proxy_base_url);
    let http_client = crate::http::shared_client();
    let auth = auth_manager.current()?;
    let user_info = match fetch_user_info(
        &http_client,
        &user_url,
        &auth,
        &auth_manager,
        alpha_test_key,
    )
    .await
    {
        Ok(ui) => ui,
        Err(kind) => {
            echo_agent_telemetry::unified_log::warn(
                "paywall_check_error",
                None,
                Some(serde_json::json!({ "user_id": user_id, "kind": kind })),
            );
            return None;
        }
    };
    echo_agent_telemetry::unified_log::info(
        "paywall_check_result",
        None,
        Some(serde_json::json!({
            "user_id": user_id,
            "subscription_tier": user_info.subscription_tier,
        })),
    );
    let new_tier = match &user_info.subscription_tier {
        Some(tier) if !tier.is_empty() => tier.clone(),
        _ => return None,
    };
    if !is_qualifying_tier(&new_tier) {
        return None;
    }
    echo_agent_telemetry::unified_log::info(
        "paywall_check_subscription_detected",
        None,
        Some(serde_json::json!({
            "user_id": user_id,
            "new_tier": new_tier,
        })),
    );
    let refresh_deadline_hit = match auth_manager
        .refresh_chain_bounded_outcome(
            TokenType::OidcSession,
            RefreshReason::ServerRejected,
            BEST_EFFORT_REFRESH_TIMEOUT,
        )
        .await
    {
        BoundedRefresh::Resolved(result) => {
            if let Err(e) = *result {
                echo_agent_telemetry::unified_log::warn(
                    "paywall_check_error",
                    None,
                    Some(serde_json::json!({
                        "user_id": user_id,
                        "kind": "refresh_failed",
                        "detail": e.to_string(),
                    })),
                );
            }
            false
        }
        BoundedRefresh::DeadlineElapsed => {
            echo_agent_telemetry::unified_log::warn(
                "paywall_check_error",
                None,
                Some(serde_json::json!({
                    "user_id": user_id,
                    "kind": "refresh_deadline",
                    "detail": "bounded refresh deadline elapsed; mint continues in background",
                })),
            );
            true
        }
    };
    echo_agent_telemetry::unified_log::info(
        "paywall_check_unblocked",
        None,
        Some(serde_json::json!({ "user_id": user_id, "new_tier": new_tier })),
    );
    Some(UnblockResult {
        new_tier,
        canonical_user_id: user_info.user_id,
        refresh_deadline_hit,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_paid_tiers_qualify() {
        for tier in &[
            "EchoAgentPro",
            "EchoAgentPlus",
            "EchoAgentStandard",
            "EchoAgentLite",
            "EchoAgentPremiumPlus",
            "EchoAgentPremium",
            "EchoAgentBasic",
        ] {
            assert!(is_qualifying_tier(tier), "{tier} must qualify");
        }
    }
    #[test]
    fn free_and_empty_tiers_are_not_qualifying() {
        assert!(!is_qualifying_tier("Free"));
        assert!(!is_qualifying_tier(""));
    }
}
