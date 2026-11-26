#![allow(
    unused_imports,
    unused_variables,
    unused_mut,
    unreachable_code,
    dead_code
)]
//! Backend environment presets for the EchoAgent CLI crate family: endpoint URL
//! defaults, environment selection, and env-var test support.
//!
//! EchoAgent Desktop injects its provider endpoints at runtime. Standalone
//! builds can override the reserved defaults with `ECHO_AGENT_PRODUCTION_*`
//! environment variables.
/// The endpoint set for one backend environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EchoAgentBuildEndpoints {
    pub cli_chat_proxy_base_url: &'static str,
    pub asset_server_url: &'static str,
    pub relay_ws_url: &'static str,
    pub gateway_ws_url: &'static str,
    pub ws_origin: &'static str,
}
// RFC 2606 `.invalid` endpoints fail closed when a standalone build has not
// been connected to an EchoAgent deployment.
const PRODUCTION_ENDPOINTS: EchoAgentBuildEndpoints = EchoAgentBuildEndpoints {
    cli_chat_proxy_base_url: "https://chat-proxy.echo-agent.invalid/v1",
    asset_server_url: "https://assets.echo-agent.invalid",
    relay_ws_url: "wss://relay.echo-agent.invalid/ws/code-agent",
    gateway_ws_url: "wss://cloud.echo-agent.invalid/ws/gw/",
    ws_origin: "https://cloud.echo-agent.invalid",
};
pub const PROD_CLI_CHAT_PROXY_BASE_URL: &str = PRODUCTION_ENDPOINTS.cli_chat_proxy_base_url;
pub const PROD_ASSET_SERVER_URL: &str = PRODUCTION_ENDPOINTS.asset_server_url;
pub const PROD_RELAY_WS_URL: &str = PRODUCTION_ENDPOINTS.relay_ws_url;
pub const PROD_GATEWAY_WS_URL: &str = PRODUCTION_ENDPOINTS.gateway_ws_url;
pub const PROD_WS_ORIGIN: &str = PRODUCTION_ENDPOINTS.ws_origin;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EchoAgentBuildEnvironment {
    #[default]
    Production,
}
impl EchoAgentBuildEnvironment {
    pub fn from_flags(_dev: bool, _staging: bool) -> Self {
        EchoAgentBuildEnvironment::Production
    }
    /// Indicator string for display; `None` for Production.
    pub fn indicator(&self) -> Option<&'static str> {
        match self {
            EchoAgentBuildEnvironment::Production => None,
        }
    }
    pub fn is_production(&self) -> bool {
        matches!(self, EchoAgentBuildEnvironment::Production)
    }
    fn env_prefix(&self) -> &'static str {
        match self {
            EchoAgentBuildEnvironment::Production => "ECHO_AGENT_PRODUCTION",
        }
    }
    /// Compiled endpoint set for this environment (production by default).
    pub fn endpoints(&self) -> EchoAgentBuildEndpoints {
        match self {
            EchoAgentBuildEnvironment::Production => PRODUCTION_ENDPOINTS,
        }
    }
    /// Env-var override when set, else the compiled endpoint.
    fn resolve(&self, var_suffix: &str, compiled: &'static str) -> String {
        std::env::var(format!("{}{var_suffix}", self.env_prefix()))
            .unwrap_or_else(|_| compiled.to_string())
    }
    pub fn cli_chat_proxy_base_url(&self) -> String {
        self.resolve(
            "_CLI_CHAT_PROXY_BASE_URL",
            self.endpoints().cli_chat_proxy_base_url,
        )
    }
    pub fn ws_origin(&self) -> String {
        self.resolve("_WS_ORIGIN", self.endpoints().ws_origin)
    }
    pub fn asset_server_url(&self) -> String {
        self.resolve("_ASSET_SERVER_URL", self.endpoints().asset_server_url)
    }
    /// The relay WebSocket URL (Web Frontend at `cloud.echo-agent.invalid/code` driving a
    /// local agent). Not the cloud-sandbox gateway ([`Self::gateway_ws_url`]);
    /// the two speak different protocols.
    pub fn relay_ws_url(&self) -> String {
        self.resolve("_WS_URL", self.endpoints().relay_ws_url)
    }
    /// The gateway WebSocket URL for `/cloud new` sandboxes. The shell's
    /// `ECHO_AGENT_GATEWAY_URL` opt-in takes precedence.
    pub fn gateway_ws_url(&self) -> String {
        self.resolve("_GATEWAY_WS_URL", self.endpoints().gateway_ws_url)
    }
}
impl std::fmt::Display for EchoAgentBuildEnvironment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EchoAgentBuildEnvironment::Production => write!(f, "production"),
        }
    }
}
/// Serializes env-var mutation across tests; `std::env` is process-global.
#[cfg(any(test, feature = "test-support"))]
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[cfg(any(test, feature = "test-support"))]
thread_local! {
    /// Set while this thread owns [`ENV_LOCK`]. `ENV_LOCK` is a plain
    /// `std::sync::Mutex` and is therefore not reentrant, so without this a
    /// second guard on one thread blocks forever on the first guard's lock.
    static ENV_LOCK_HELD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
#[cfg(any(test, feature = "test-support"))]
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    assert!(
        !ENV_LOCK_HELD.get(),
        "EnvVarGuard: this thread already holds a live guard. Stacking guards \
         self-deadlocks on the non-reentrant ENV_LOCK; chain the extra keys \
         onto the first guard with `and_set`/`and_remove` instead."
    );
    let lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    ENV_LOCK_HELD.set(true);
    lock
}
/// RAII env-var override for tests: constructors snapshot the prior value
/// under [`ENV_LOCK`], `Drop` restores it, panics included.
///
/// A guard owns [`ENV_LOCK`] for its whole lifetime, so one thread can only
/// ever hold one. To override several keys at once, chain
/// [`Self::and_set`] / [`Self::and_remove`] onto a single guard.
#[cfg(any(test, feature = "test-support"))]
pub struct EnvVarGuard {
    /// The constructor's key; [`Self::set_value`] targets it.
    key: &'static str,
    /// Every overridden key with its pre-guard value, restored in reverse.
    restore: Vec<(&'static str, Option<String>)>,
    _lock: std::sync::MutexGuard<'static, ()>,
}
#[cfg(any(test, feature = "test-support"))]
impl EnvVarGuard {
    pub fn set(key: &'static str, value: &str) -> Self {
        Self::acquire(key).override_var(key, Some(value))
    }
    pub fn remove(key: &'static str) -> Self {
        Self::acquire(key).override_var(key, None)
    }
    /// Override a further key under this guard's existing lock.
    #[must_use]
    pub fn and_set(self, key: &'static str, value: &str) -> Self {
        self.override_var(key, Some(value))
    }
    /// Unset a further key under this guard's existing lock.
    #[must_use]
    pub fn and_remove(self, key: &'static str) -> Self {
        self.override_var(key, None)
    }
    fn acquire(key: &'static str) -> Self {
        Self {
            key,
            restore: Vec::new(),
            _lock: env_lock(),
        }
    }
    fn override_var(mut self, key: &'static str, value: Option<&str>) -> Self {
        self.restore.push((key, std::env::var(key).ok()));
        match value {
            Some(value) => unsafe { std::env::set_var(key, value) },
            None => unsafe { std::env::remove_var(key) },
        }
        self
    }
    /// Update the value while still holding the env lock.
    pub fn set_value(&self, value: &str) {
        unsafe { std::env::set_var(self.key, value) };
    }
}
#[cfg(any(test, feature = "test-support"))]
impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        for (key, prev) in self.restore.drain(..).rev() {
            match prev {
                Some(prev) => unsafe { std::env::set_var(key, prev) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
        ENV_LOCK_HELD.set(false);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    /// The env-var prefixes are an operator interface; do not rename.
    #[test]
    fn test_env_prefix() {
        assert_eq!(
            EchoAgentBuildEnvironment::Production.env_prefix(),
            "ECHO_AGENT_PRODUCTION"
        );
    }
    #[test]
    fn env_var_guard_set_value_updates_then_restores_on_drop() {
        const KEY: &str = "ECHO_AGENT_ENV_VAR_GUARD_SET_VALUE_PROBE";
        let before = std::env::var(KEY).ok();
        {
            let guard = EnvVarGuard::set(KEY, "initial");
            assert_eq!(std::env::var(KEY).ok().as_deref(), Some("initial"));
            guard.set_value("updated");
            assert_eq!(
                std::env::var(KEY).ok().as_deref(),
                Some("updated"),
                "set_value must update the env var while the guard is live"
            );
        }
        assert_eq!(
            std::env::var(KEY).ok(),
            before,
            "Drop must restore the pre-guard snapshot (was {before:?})"
        );
    }
    #[test]
    fn env_var_guard_chains_keys_under_one_lock_and_restores_all() {
        const A: &str = "ECHO_AGENT_ENV_VAR_GUARD_CHAIN_A_PROBE";
        const B: &str = "ECHO_AGENT_ENV_VAR_GUARD_CHAIN_B_PROBE";
        {
            let _guard = EnvVarGuard::set(A, "first")
                .and_set(B, "b")
                .and_set(A, "second")
                .and_remove(B);
            assert_eq!(std::env::var(A).ok().as_deref(), Some("second"));
            assert!(std::env::var(B).is_err());
        }
        assert!(
            std::env::var(A).is_err(),
            "a re-overridden key must restore to its pre-guard value, not to `first`"
        );
        assert!(std::env::var(B).is_err());
    }
    /// Stacking two guards on one thread used to block forever on the
    /// non-reentrant `ENV_LOCK`, which surfaced only as a CI test timeout.
    #[test]
    #[should_panic(expected = "this thread already holds a live guard")]
    fn env_var_guard_rejects_a_second_guard_on_the_same_thread() {
        const KEY: &str = "ECHO_AGENT_ENV_VAR_GUARD_REENTRANCY_PROBE";
        let _first = EnvVarGuard::set(KEY, "first");
        let _second = EnvVarGuard::set(KEY, "second");
    }
    /// Guards against conflating the relay and gateway endpoints (a relay
    /// loop mistakenly connecting to `wss://cloud.echo-agent.invalid/ws/gw/`).
    #[test]
    fn relay_and_gateway_urls_are_distinct() {
        assert_ne!(
            EchoAgentBuildEnvironment::Production.relay_ws_url(),
            EchoAgentBuildEnvironment::Production.gateway_ws_url(),
        );
    }
    #[test]
    fn test_from_flags() {
        assert_eq!(
            EchoAgentBuildEnvironment::from_flags(false, false),
            EchoAgentBuildEnvironment::Production
        );
    }
}
