//! Stub for builds without the devbox auth feature.
//!
//! Compiled instead of `devbox_login.rs` when the devbox auth feature is
//! off, so the remote devbox login helper is not reached. The API
//! mirrors the real module: `is_devbox_environment()` is always `false`, which
//! short-circuits every auto-recovery/migration call site, and the entry
//! points that can still be reached directly (`echoagent login --devbox`) return a
//! descriptive error.

use super::manager::AuthManager;
use super::model::EchoAgentAuth;

const UNAVAILABLE: &str =
    "devbox login is not available in this build (compiled without the `devbox-login` feature)";

/// Always `false` without the devbox auth feature; callers treat the
/// process as running outside a devbox environment.
pub(crate) fn is_devbox_environment() -> bool {
    false
}

/// Unreachable in practice (guarded by [`is_devbox_environment`]); errors
/// defensively if called.
pub(crate) async fn mint_devbox_auth(_auth_manager: &AuthManager) -> anyhow::Result<EchoAgentAuth> {
    anyhow::bail!(UNAVAILABLE)
}

/// Unreachable in practice (guarded by [`is_devbox_environment`]); errors
/// defensively if called.
pub(super) async fn mint_devbox_auth_raw() -> anyhow::Result<EchoAgentAuth> {
    anyhow::bail!(UNAVAILABLE)
}

/// `echoagent login --devbox` entry point: always errors in this build.
pub(crate) async fn run_devbox_login(
    _config: &crate::agent::config::Config,
) -> anyhow::Result<EchoAgentAuth> {
    anyhow::bail!(UNAVAILABLE)
}
