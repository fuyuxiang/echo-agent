//! Startup connect budget: the default agent-ready wait and its env override.

use std::time::Duration;

/// Single source for the env-var name so the runnable Try-row form below
/// cannot drift from it.
macro_rules! connect_ui_timeout_env {
    () => {
        "ECHO_AGENT_CONNECT_UI_TIMEOUT_SECS"
    };
}

pub(super) const CONNECT_UI_TIMEOUT_ENV: &str = connect_ui_timeout_env!();
/// Runnable form for the startup-failure `Try` row, which keeps commands out
/// of wrappable prose.
pub(super) const CONNECT_UI_TIMEOUT_TRY_COMMAND: &str =
    concat!(connect_ui_timeout_env!(), "=60 echoagent");
pub(super) const DEFAULT_CONNECT_UI_TIMEOUT: Duration = Duration::from_secs(30);
// Floor: the bounded no-mint startup auth alone can take ~5s.
const MIN_CONNECT_UI_TIMEOUT_SECS: u64 = 5;

/// `None`, empty, and unparsable values map to the default; parsed values
/// clamp to the 5s floor. `0` also maps to the default: a zero budget would
/// time out every startup instantly. No ceiling — a large budget is the
/// user's informed choice.
pub(super) fn resolve(env: Option<&str>) -> Duration {
    match env.map(str::trim).and_then(|v| v.parse::<u64>().ok()) {
        None | Some(0) => DEFAULT_CONNECT_UI_TIMEOUT,
        Some(secs) => Duration::from_secs(secs.max(MIN_CONNECT_UI_TIMEOUT_SECS)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_cases() {
        assert_eq!(resolve(None), DEFAULT_CONNECT_UI_TIMEOUT);
        assert_eq!(resolve(Some("")), DEFAULT_CONNECT_UI_TIMEOUT);
        assert_eq!(resolve(Some(" 45 ")), Duration::from_secs(45));
        assert_eq!(resolve(Some("0")), DEFAULT_CONNECT_UI_TIMEOUT);
        assert_eq!(resolve(Some("garbage")), DEFAULT_CONNECT_UI_TIMEOUT);
        assert_eq!(resolve(Some("-5")), DEFAULT_CONNECT_UI_TIMEOUT);
        assert_eq!(resolve(Some("1e3")), DEFAULT_CONNECT_UI_TIMEOUT);
        assert_eq!(resolve(Some("1")), Duration::from_secs(5));
        assert_eq!(resolve(Some("9999")), Duration::from_secs(9999));
    }
}
