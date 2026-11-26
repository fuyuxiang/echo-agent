//! Single source of truth for the echoagent home directory: `$ECHO_AGENT_HOME` or
//! `<home>/.echo-agent`. Shared by `echo-agent-config` and `echo-agent-fast-worktree`.
//!
//! Which function to call:
//! - [`echo_agent_home`]: the usual choice, a cached, created path to build on.
//! - [`user_echo_agent_home`]: `None` instead of a cwd fallback when no home resolves.
//! - [`default_echo_agent_home`]: the `<home>/.echo-agent` default, ignoring `$ECHO_AGENT_HOME`, so callers can detect an override.
//! - [`resolve_echo_agent_home`]: a fresh, uncached resolve.
//!
//! TODO: collapse these getters by threading the path through config as an
//! explicit value.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// `<home>/.echo-agent`, canonicalized via `dunce` (not `std::fs::canonicalize`,
/// which yields Windows `\\?\` verbatim paths).
fn echo_agent_home_in(home: &Path) -> PathBuf {
    dunce::canonicalize(home)
        .unwrap_or_else(|_| home.to_path_buf())
        .join(".echo-agent")
}

/// `$ECHO_AGENT_HOME` verbatim when non-empty, else `<home>/.echo-agent`. The env value is
/// used as-is (not canonicalized) so it stays stable and comparable: callers do
/// literal prefix checks against it, and downstream symlink guards must still see
/// its original components.
fn resolve_echo_agent_home_from(
    echo_agent_home_env: Option<&OsStr>,
    os_home: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(env) = echo_agent_home_env.filter(|env| !env.is_empty()) {
        return Some(PathBuf::from(env));
    }
    os_home.map(echo_agent_home_in)
}

/// Resolve the echoagent home from the environment (fresh, no cache); `None` if neither resolves.
pub fn resolve_echo_agent_home() -> Option<PathBuf> {
    resolve_echo_agent_home_from(
        std::env::var_os("ECHO_AGENT_HOME").as_deref(),
        dirs::home_dir().as_deref(),
    )
}

/// The default `<home>/.echo-agent`, used when `$ECHO_AGENT_HOME` is unset.
pub fn default_echo_agent_home() -> PathBuf {
    echo_agent_home_in(&dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
}

/// The echoagent home, created if missing and cached for the process; falls back to
/// [`default_echo_agent_home`] when neither `$ECHO_AGENT_HOME` nor a home resolves.
pub fn echo_agent_home() -> PathBuf {
    static ECHO_AGENT_HOME: OnceLock<PathBuf> = OnceLock::new();
    ECHO_AGENT_HOME
        .get_or_init(|| {
            let home = resolve_echo_agent_home().unwrap_or_else(default_echo_agent_home);
            if let Err(err) = std::fs::create_dir_all(&home) {
                tracing::warn!(path = %home.display(), %err, "failed to create echoagent home");
            }
            home
        })
        .clone()
}

/// Like [`echo_agent_home`], but `None` when no home resolves (no cwd fallback).
pub fn user_echo_agent_home() -> Option<PathBuf> {
    resolve_echo_agent_home().is_some().then(echo_agent_home)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::ffi::OsString;

    #[test]
    fn env_wins_over_os_home() {
        let resolved = resolve_echo_agent_home_from(
            Some(OsStr::new("/custom/home")),
            Some(Path::new("/home/u")),
        );
        assert_eq!(resolved, Some(PathBuf::from("/custom/home")));
    }

    #[test]
    fn env_used_verbatim_even_when_it_exists() {
        // A real, existing dir whose canonical form differs (macOS symlinks
        // `/var` -> `/private/var`): the env value must come back unchanged.
        let tmp = tempfile::tempdir().unwrap();
        let resolved = resolve_echo_agent_home_from(Some(tmp.path().as_os_str()), None);
        assert_eq!(resolved, Some(tmp.path().to_path_buf()));
    }

    #[test]
    fn empty_env_falls_through_to_os_home() {
        let tmp = tempfile::tempdir().unwrap();
        let resolved = resolve_echo_agent_home_from(Some(&OsString::new()), Some(tmp.path()));
        assert_eq!(
            resolved,
            Some(dunce::canonicalize(tmp.path()).unwrap().join(".echo-agent"))
        );
    }

    #[test]
    fn default_echo_agent_home_has_no_verbatim_prefix() {
        // The reason we canonicalize via dunce: std::fs::canonicalize yields
        // `\\?\` verbatim paths on Windows that break git and byte-exact
        // comparisons. No-op assertion on Unix.
        let home = default_echo_agent_home();
        assert!(!home.to_string_lossy().starts_with(r"\\?\"));
        assert!(home.ends_with(".echo-agent"));
    }

    #[test]
    fn none_when_nothing_resolves() {
        assert_eq!(
            resolve_echo_agent_home_from(
                /* echo_agent_home_env */ None, /* os_home */ None
            ),
            None
        );
    }
}

/// OS home lookup for compatibility discovery. Callers that isolate acceptance
/// profiles must apply their controlled-root gate before invoking this fallback.
pub fn os_home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}
