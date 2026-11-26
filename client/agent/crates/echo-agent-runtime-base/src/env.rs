//! EchoAgentBuildEnvironment configuration for the shell crate family.
//!
//! The environment presets (per-environment endpoint URLs, the staging
//! trust check, `EnvVarGuard`) live in the [`echo_agent_env`] leaf crate so
//! sibling crates (telemetry, tools, workspace) can share them without
//! depending on this crate. This module re-exports them and hosts the
//! shell-specific gateway-bridge env vars.
//!
//! # Gateway-bridge mode (env-only)
//! - `ECHO_AGENT_GATEWAY_URL` — when set to a valid URL, `MvpAgent` spawns a
//!   per-session gateway bridge actor and routes prompts through
//!   it. Unset → falls back to [`EchoAgentBuildEnvironment::gateway_ws_url`] for
//!   sessions created in gateway mode; otherwise local-mode (unchanged).
pub use echo_agent_env::{
    EchoAgentBuildEnvironment, PROD_ASSET_SERVER_URL, PROD_CLI_CHAT_PROXY_BASE_URL,
    PROD_GATEWAY_WS_URL, PROD_RELAY_WS_URL, PROD_WS_ORIGIN,
};
/// Public Computer Hub WebSocket URL used by the local-workspace supervisor
/// (`workspace_server --hub-url`) when `agent_config.hub.url` is unset.
/// Extracted from the former private leader const (same literal).
pub const PROD_COMPUTER_HUB_WS_URL: &str = "wss://computer-hub.echo-agent.invalid/v1/tools";
#[cfg(any(test, feature = "test-support"))]
pub use echo_agent_env::EnvVarGuard;
/// Env var that opts a process into gateway-bridge mode. When set to
/// a parseable URL, `session/new` / `session/load` spawns a per-session
/// `gateway_bridge` actor in the shell; unset → local-mode (unchanged).
pub const ECHO_AGENT_GATEWAY_URL_ENV: &str = "ECHO_AGENT_GATEWAY_URL";
/// Client kill switch for the gateway-bridge custom-method passthrough.
/// Set to `1` / `true` to force every `custom_method` call back onto
/// agent-local dispatch regardless of the routing table or negotiated
/// capability — an instant revert without a redeploy if the channel
/// misbehaves. Unset/`0`/`false` → normal routing.
pub const ECHO_AGENT_DISABLE_CUSTOM_BRIDGE_ENV: &str = "ECHO_AGENT_DISABLE_CUSTOM_BRIDGE";
/// `true` when the custom-method bridge passthrough is force-disabled via
/// [`ECHO_AGENT_DISABLE_CUSTOM_BRIDGE_ENV`]. Accepts `1`/`true` (case-insensitive).
pub fn custom_bridge_disabled() -> bool {
    std::env::var(ECHO_AGENT_DISABLE_CUSTOM_BRIDGE_ENV)
        .map(|v| {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true")
        })
        .unwrap_or(false)
}
/// Parse `ECHO_AGENT_GATEWAY_URL` into a [`url::Url`].
///
/// This build ignores the variable and stays in local mode (warns if set).
pub fn parse_gateway_url() -> Option<url::Url> {
    let raw = std::env::var(ECHO_AGENT_GATEWAY_URL_ENV).ok()?;
    if raw.is_empty() {
        return None;
    }
    tracing::warn!(
        env = ECHO_AGENT_GATEWAY_URL_ENV,
        "ECHO_AGENT_GATEWAY_URL is set but this build does not support gateway-bridge mode; staying in local mode"
    );
    None
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_gateway_url_returns_none_when_unset() {
        let _env = EnvVarGuard::remove(ECHO_AGENT_GATEWAY_URL_ENV);
        assert!(parse_gateway_url().is_none());
    }
    #[test]
    fn parse_gateway_url_returns_none_when_empty() {
        let _env = EnvVarGuard::set(ECHO_AGENT_GATEWAY_URL_ENV, "");
        assert!(parse_gateway_url().is_none());
    }
    #[test]
    fn parse_gateway_url_hard_off_without_gateway_bridge() {
        let _env = EnvVarGuard::set(ECHO_AGENT_GATEWAY_URL_ENV, "wss://gateway.example.com/ws");
        assert!(
            parse_gateway_url().is_none(),
            "valid URL must still be ignored in this build"
        );
    }
    #[test]
    fn custom_bridge_disabled_defaults_false_when_unset() {
        let _env = EnvVarGuard::remove(ECHO_AGENT_DISABLE_CUSTOM_BRIDGE_ENV);
        assert!(!custom_bridge_disabled());
    }
    #[test]
    fn custom_bridge_disabled_true_for_one_and_true() {
        for v in ["1", "true", "TRUE", " true "] {
            let _env = EnvVarGuard::set(ECHO_AGENT_DISABLE_CUSTOM_BRIDGE_ENV, v);
            assert!(
                custom_bridge_disabled(),
                "{v:?} must disable the custom bridge"
            );
        }
    }
    #[test]
    fn custom_bridge_disabled_false_for_zero_and_garbage() {
        for v in ["0", "false", "", "no"] {
            let _env = EnvVarGuard::set(ECHO_AGENT_DISABLE_CUSTOM_BRIDGE_ENV, v);
            assert!(
                !custom_bridge_disabled(),
                "{v:?} must leave the custom bridge enabled"
            );
        }
    }
}
