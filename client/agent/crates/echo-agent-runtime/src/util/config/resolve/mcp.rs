use toml::Value as TomlValue;

/// Resolve `mcp.liveness_watchers` for a session.
///
/// Thin wrapper around the canonical
/// [`crate::agent::config::resolve_mcp_liveness_watchers`], which
/// unifies the two previous implementations so they can't drift.
///
/// Pulls each layer from its appropriate TOML / runtime source:
///
/// | Layer        | Source                                                          |
/// |--------------|-----------------------------------------------------------------|
/// | requirement  | `[features] mcp_liveness_watchers` in `requirements.toml`       |
/// | cli          | (none — no CLI flag)                                            |
/// | env          | `ECHO_AGENT_MCP_LIVENESS_WATCHERS` (handled by `BoolFlag::env`)       |
/// | config       | `[features] mcp_liveness_watchers` in `~/.echo-agent/config.toml`     |
/// | managed      | `[features] mcp_liveness_watchers` in `managed_config.toml`     |
/// | feature_flag | (none yet — remote settings plumbing TBD)                            |
/// | default      | `true`                                                          |
///
/// Returns the resolved boolean (the `Resolved::source` is discarded
/// for this call site — session-actor only needs the value).
pub(crate) fn resolve_mcp_liveness_watchers(
    requirements: Option<&TomlValue>,
    user: Option<&TomlValue>,
    managed: Option<&TomlValue>,
) -> bool {
    fn from_toml(v: Option<&TomlValue>) -> Option<bool> {
        v?.get("features")?.get("mcp_liveness_watchers")?.as_bool()
    }
    crate::agent::config::resolve_mcp_liveness_watchers(
        from_toml(requirements),
        /* cli          */ None,
        from_toml(user),
        from_toml(managed),
        /* feature_flag */ None,
    )
    .value
}

/// Resolve `mcp.auto_restart` for a session.
///
/// Thin wrapper around the canonical
/// [`crate::agent::config::resolve_mcp_auto_restart`]. Mirrors
/// [`resolve_mcp_liveness_watchers`].
///
/// Pulls each layer from its appropriate TOML / runtime source:
///
/// | Layer        | Source                                                          |
/// |--------------|-----------------------------------------------------------------|
/// | requirement  | `[features] mcp_auto_restart` in `requirements.toml`            |
/// | cli          | (none — no CLI flag)                                            |
/// | env          | `ECHO_AGENT_MCP_AUTO_RESTART` (handled by `BoolFlag::env`)            |
/// | config       | `[features] mcp_auto_restart` in `~/.echo-agent/config.toml`          |
/// | managed      | `[features] mcp_auto_restart` in `managed_config.toml`          |
/// | feature_flag | (none yet — remote settings plumbing TBD)                            |
/// | default      | `true`                                                          |
///
/// Returns the resolved boolean (the `Resolved::source` is discarded
/// for this call site — session-actor only needs the value).
pub(crate) fn resolve_mcp_auto_restart(
    requirements: Option<&TomlValue>,
    user: Option<&TomlValue>,
    managed: Option<&TomlValue>,
) -> bool {
    fn from_toml(v: Option<&TomlValue>) -> Option<bool> {
        v?.get("features")?.get("mcp_auto_restart")?.as_bool()
    }
    crate::agent::config::resolve_mcp_auto_restart(
        from_toml(requirements),
        /* cli          */ None,
        from_toml(user),
        from_toml(managed),
        /* feature_flag */ None,
    )
    .value
}

/// Resolve `mcp.push_server_status` for a session.
///
/// Thin wrapper around the canonical
/// [`crate::agent::config::resolve_mcp_push_server_status`] that
/// mirrors [`resolve_mcp_liveness_watchers`].
///
/// Pulls each layer from its TOML / runtime source:
///
/// | Layer        | Source                                                          |
/// |--------------|-----------------------------------------------------------------|
/// | requirement  | `[features] mcp_push_server_status` in `requirements.toml`      |
/// | cli          | (none — no CLI flag)                                            |
/// | env          | `ECHO_AGENT_MCP_PUSH_SERVER_STATUS` (handled by `BoolFlag::env`)      |
/// | config       | `[features] mcp_push_server_status` in `~/.echo-agent/config.toml`    |
/// | managed      | `[features] mcp_push_server_status` in `managed_config.toml`    |
/// | feature_flag | (none yet — remote settings plumbing TBD)                            |
/// | default      | `true`                                                          |
///
/// Returns the resolved boolean.
pub fn resolve_mcp_push_server_status(
    requirements: Option<&TomlValue>,
    user: Option<&TomlValue>,
    managed: Option<&TomlValue>,
) -> bool {
    fn from_toml(v: Option<&TomlValue>) -> Option<bool> {
        v?.get("features")?.get("mcp_push_server_status")?.as_bool()
    }
    crate::agent::config::resolve_mcp_push_server_status(
        from_toml(requirements),
        /* cli          */ None,
        from_toml(user),
        from_toml(managed),
        /* feature_flag */ None,
    )
    .value
}

/// Resolve `mcp.recursive_config_watch` for the leader's
/// `ConfigFileWatcher` spawn path.
///
/// Thin wrapper around the canonical
/// [`crate::agent::config::resolve_mcp_recursive_config_watch`] —
/// mirrors the same wrapper pattern as the other MCP resolvers so the
/// two implementations can't drift.
///
/// Pulls each layer from its TOML / runtime source:
///
/// | Layer        | Source                                                              |
/// |--------------|---------------------------------------------------------------------|
/// | requirement  | `[features] mcp_recursive_config_watch` in `requirements.toml`      |
/// | cli          | (none — no CLI flag)                                                |
/// | env          | `ECHO_AGENT_MCP_RECURSIVE_CONFIG_WATCH` (handled by `BoolFlag::env`)      |
/// | config       | `[features] mcp_recursive_config_watch` in `~/.echo-agent/config.toml`    |
/// | managed      | `[features] mcp_recursive_config_watch` in `managed_config.toml`    |
/// | feature_flag | (none yet — remote settings plumbing TBD)                                |
/// | default      | `true`                                                              |
///
/// Returns the resolved boolean (the `Resolved::source` is discarded
/// for this call site — the leader's watcher-spawn only needs the
/// value).
pub(crate) fn resolve_mcp_recursive_config_watch(
    requirements: Option<&TomlValue>,
    user: Option<&TomlValue>,
    managed: Option<&TomlValue>,
) -> bool {
    fn from_toml(v: Option<&TomlValue>) -> Option<bool> {
        v?.get("features")?
            .get("mcp_recursive_config_watch")?
            .as_bool()
    }
    crate::agent::config::resolve_mcp_recursive_config_watch(
        from_toml(requirements),
        /* cli          */ None,
        from_toml(user),
        from_toml(managed),
        /* feature_flag */ None,
    )
    .value
}

/// Default MCP startup-handshake timeout (seconds) when nothing overrides it.
/// Kept in sync with `echo_agent_mcp::servers`'s standalone fallback.
pub const DEFAULT_MCP_STARTUP_TIMEOUT_SECS: u64 = 30;

/// Env override for the MCP startup timeout, in milliseconds (shared with
/// common third-party tooling, so an existing setting carries over).
const ENV_MCP_TIMEOUT_MS: &str = "MCP_TIMEOUT";
/// Env override for the MCP startup timeout, in seconds (echo-agent-native).
const ENV_MCP_STARTUP_TIMEOUT_SECS: &str = "ECHO_AGENT_MCP_STARTUP_TIMEOUT_SECS";

/// Cached remote settings `mcp_startup_timeout_secs` (`0` = unset). MCP servers start
/// from free functions with no handle to the live `RemoteSettings`, so the
/// remote tier is cached here when settings are applied.
static REMOTE_MCP_STARTUP_TIMEOUT_SECS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Record the remote settings `mcp_startup_timeout_secs` for the free-function
/// resolver. Call wherever `RemoteSettings` is applied. `0` is treated as unset.
pub(crate) fn cache_remote_mcp_startup_timeout_secs(value: Option<u64>) {
    REMOTE_MCP_STARTUP_TIMEOUT_SECS.store(value.unwrap_or(0), std::sync::atomic::Ordering::Relaxed);
}

fn cached_remote_mcp_startup_timeout_secs() -> Option<u64> {
    match REMOTE_MCP_STARTUP_TIMEOUT_SECS.load(std::sync::atomic::Ordering::Relaxed) {
        0 => None,
        secs => Some(secs),
    }
}

/// Global default MCP startup-handshake timeout (seconds), applying the cached
/// remote tier. Global fallback only — a per-server
/// `startup_timeout_sec` / `_meta.startupTimeoutMs` still wins (see
/// `session::mcp_servers`).
pub(crate) fn resolved_mcp_startup_timeout_secs() -> u64 {
    resolve_mcp_startup_timeout_secs(cached_remote_mcp_startup_timeout_secs())
}

/// Resolve the global MCP startup-handshake timeout (seconds). Precedence:
/// requirements.toml `[mcp].startup_timeout_sec` > env (`MCP_TIMEOUT` ms /
/// `ECHO_AGENT_MCP_STARTUP_TIMEOUT_SECS` secs) > effective `config.toml [mcp]` >
/// remote settings `remote` > [`DEFAULT_MCP_STARTUP_TIMEOUT_SECS`].
pub(crate) fn resolve_mcp_startup_timeout_secs(remote: Option<u64>) -> u64 {
    fn extract(v: &toml::Value) -> Option<u64> {
        v.get("mcp")?
            .get("startup_timeout_sec")?
            .as_integer()
            .and_then(|n| u64::try_from(n).ok())
            .filter(|n| *n > 0)
    }
    let requirements = crate::config::load_merged_requirements()
        .as_ref()
        .and_then(extract);
    let config = crate::config::load_effective_config()
        .ok()
        .as_ref()
        .and_then(extract);
    resolve_mcp_startup_timeout_precedence(
        requirements,
        mcp_startup_timeout_from_env(),
        config,
        remote,
    )
}

/// `MCP_TIMEOUT` (ms, rounded up so a sub-second value never becomes 0s) >
/// `ECHO_AGENT_MCP_STARTUP_TIMEOUT_SECS` (secs). Unparseable/zero values are ignored.
fn mcp_startup_timeout_from_env() -> Option<u64> {
    if let Some(ms) = std::env::var(ENV_MCP_TIMEOUT_MS)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
    {
        return Some(ms.div_ceil(1000));
    }
    std::env::var(ENV_MCP_STARTUP_TIMEOUT_SECS)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
}

/// Pure precedence for [`resolve_mcp_startup_timeout_secs`] (tiers injected so it
/// is unit-testable without touching env/disk).
fn resolve_mcp_startup_timeout_precedence(
    requirements: Option<u64>,
    env: Option<u64>,
    config: Option<u64>,
    remote: Option<u64>,
) -> u64 {
    requirements
        .or(env)
        .or(config)
        .or(remote)
        .unwrap_or(DEFAULT_MCP_STARTUP_TIMEOUT_SECS)
}

#[cfg(test)]
mod mcp_startup_timeout_tests {
    use super::{DEFAULT_MCP_STARTUP_TIMEOUT_SECS, resolve_mcp_startup_timeout_precedence as r};

    #[test]
    fn precedence_requirements_env_config_remote_default() {
        assert_eq!(r(None, None, None, None), DEFAULT_MCP_STARTUP_TIMEOUT_SECS);
        assert_eq!(r(Some(5), Some(6), Some(7), Some(8)), 5); // requirements highest
        assert_eq!(r(None, Some(6), Some(7), Some(8)), 6); // env
        assert_eq!(r(None, None, Some(7), Some(8)), 7); // config
        assert_eq!(r(None, None, None, Some(8)), 8); // remote
    }
}

// ── MCP max output bytes (inline tool-result cap) ───────────────────────────
//
// Full multi-tier resolve lives only here (shell can read config/requirements).
// Tools holds a single effective atomic: we resolve once on apply and push the
// result via `set_mcp_max_output_bytes` so free-function truncation sees it.

/// Default MCP tool-result inline cap (bytes).
pub const DEFAULT_MAX_MCP_OUTPUT_BYTES: usize = echo_agent_tools::MCP_MAX_OUTPUT_BYTES;

/// Resolve the full stack for `remote` and seed the tools-crate effective limit.
///
/// Call wherever `RemoteSettings` is applied (same sites as
/// [`cache_remote_mcp_startup_timeout_secs`]). Unlike that helper — which only
/// caches the remote tier for a free-function resolver still living in shell —
/// this pushes the *fully resolved* value into tools (tools cannot re-read
/// config/requirements on every use).
pub(crate) fn cache_remote_max_mcp_output_bytes(remote: Option<u64>) {
    echo_agent_tools::set_mcp_max_output_bytes(resolve_max_mcp_output_bytes(remote));
}

/// Extract `[mcp] max_output_bytes` from one TOML root. Positive integers only.
fn max_mcp_output_bytes_from_toml(v: &toml::Value) -> Option<usize> {
    let raw = v.get("mcp")?.get("max_output_bytes")?.as_integer()?;
    u64::try_from(raw)
        .ok()
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| *n > 0)
}

/// Resolve the MCP tool-result inline cap (bytes) — **global / atomic path**
/// (no cwd, so no project tier; see [`resolve_max_mcp_output_bytes_for_cwd`]).
///
/// Precedence (highest first):
///   1. requirements.toml `[mcp] max_output_bytes`
///   2. env `ECHO_AGENT_MAX_MCP_OUTPUT_BYTES` / `MAX_MCP_OUTPUT_BYTES`
///      (EchoAgent-native wins when both set)
///   3. effective `config.toml [mcp] max_output_bytes`
///   4. remote settings `RemoteSettings.max_mcp_output_bytes`
///   5. [`DEFAULT_MAX_MCP_OUTPUT_BYTES`] (20_000)
pub(crate) fn resolve_max_mcp_output_bytes(remote: Option<u64>) -> usize {
    let remote_usize = remote
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| *n > 0);
    let requirements = crate::config::load_merged_requirements()
        .as_ref()
        .and_then(max_mcp_output_bytes_from_toml);
    let config = crate::config::load_effective_config()
        .ok()
        .as_ref()
        .and_then(max_mcp_output_bytes_from_toml);
    resolve_max_mcp_output_bytes_precedence(
        requirements,
        echo_agent_tools::mcp_max_output_bytes_from_env(),
        None, // project tier needs a cwd — see resolve_max_mcp_output_bytes_for_cwd
        config,
        remote_usize,
    )
}

/// Project tier of the MCP output cap: `[mcp] max_output_bytes` from the
/// `.echo-agent/config.toml` chain (`cwd` → git root), deepest file wins.
///
/// Folder-trust-gated: an untrusted checkout must not raise (context-stuffing
/// / cost vector) or lower the cap, matching how project plugin paths and
/// repo env contributions are gated.
fn project_max_mcp_output_bytes(cwd: &std::path::Path) -> Option<usize> {
    if !crate::agent::folder_trust::project_scope_allowed(cwd) {
        return None;
    }
    let mut value = None;
    // Repo-root-first → cwd-last: later (deeper) files overwrite.
    for config_path in crate::config::find_project_configs(cwd) {
        if let Ok(toml_val) = echo_agent_config::load_config_file(&config_path)
            && let Some(v) = max_mcp_output_bytes_from_toml(&toml_val)
        {
            value = Some(v);
        }
    }
    value
}

/// Session-scoped MCP output cap: `Some(bytes)` **only when the project tier
/// wins** the full precedence stack for `cwd`; `None` otherwise.
///
/// The caller seeds `Some` values into the session's `TruncationCfg` resource
/// (consulted by MCP truncation *before* the process-global atomic). Returning
/// `None` when any higher- or lower-priority tier would win keeps the atomic
/// authoritative for those — including live remote settings refresh — so sessions
/// without a repo-level value behave exactly as before.
///
/// The project tier only wins when requirements and env are absent (it sits
/// above user config / remote settings / default), so `Some` here is simply
/// "requirements and env unset, project value present".
pub(crate) fn resolve_max_mcp_output_bytes_for_cwd(cwd: &std::path::Path) -> Option<usize> {
    let requirements = crate::config::load_merged_requirements()
        .as_ref()
        .and_then(max_mcp_output_bytes_from_toml);
    if requirements.is_some() || echo_agent_tools::mcp_max_output_bytes_from_env().is_some() {
        return None;
    }
    project_max_mcp_output_bytes(cwd)
}

/// Pure precedence for [`resolve_max_mcp_output_bytes`] (tiers injected so it is
/// unit-testable without env/disk).
///
/// `requirements` > `env` > `project` > `config` > `remote` > default.
pub(crate) fn resolve_max_mcp_output_bytes_precedence(
    requirements: Option<usize>,
    env: Option<usize>,
    project: Option<usize>,
    config: Option<usize>,
    remote: Option<usize>,
) -> usize {
    requirements
        .or(env)
        .or(project)
        .or(config)
        .or(remote)
        .unwrap_or(DEFAULT_MAX_MCP_OUTPUT_BYTES)
}

#[cfg(test)]
mod max_mcp_output_bytes_tests {
    use super::{
        DEFAULT_MAX_MCP_OUTPUT_BYTES, max_mcp_output_bytes_from_toml,
        resolve_max_mcp_output_bytes_precedence as r,
    };

    #[test]
    fn precedence_requirements_env_project_config_remote_default() {
        assert_eq!(
            r(None, None, None, None, None),
            DEFAULT_MAX_MCP_OUTPUT_BYTES
        );
        assert_eq!(r(Some(1), Some(2), Some(9), Some(3), Some(4)), 1); // requirements highest
        assert_eq!(r(None, Some(2), Some(9), Some(3), Some(4)), 2); // env beats project
        assert_eq!(r(None, None, Some(9), Some(3), Some(4)), 9); // project beats user config
        assert_eq!(r(None, None, None, Some(3), Some(4)), 3); // user config beats remote
        assert_eq!(r(None, None, None, None, Some(4)), 4); // remote beats default
    }

    #[test]
    fn toml_extractor_rejects_non_positive_and_wrong_types() {
        let ok: toml::Value = toml::from_str("[mcp]\nmax_output_bytes = 40000").unwrap();
        assert_eq!(max_mcp_output_bytes_from_toml(&ok), Some(40_000));
        for bad in [
            "[mcp]\nmax_output_bytes = 0",
            "[mcp]\nmax_output_bytes = -5",
            "[mcp]\nmax_output_bytes = \"big\"",
            "[other]\nmax_output_bytes = 5",
        ] {
            let v: toml::Value = toml::from_str(bad).unwrap();
            assert_eq!(max_mcp_output_bytes_from_toml(&v), None, "input: {bad}");
        }
    }

    /// The project-tier walk: repo-root-first, deepest file wins; files
    /// without the key leave the running value untouched.
    ///
    /// Uses the pure chain logic via tempdirs + `find_project_configs`
    /// ordering (repo root → cwd), mirroring `project_max_mcp_output_bytes`
    /// without the trust gate (exercised separately — trust is inert in
    /// dev/test builds, see `folder_trust_inert`).
    #[test]
    fn project_chain_deepest_file_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Make it a git repo so the chain walks subdir → root.
        git2::Repository::init(root).unwrap();
        let sub = root.join("crates").join("thing");
        std::fs::create_dir_all(sub.join(".echo-agent")).unwrap();
        std::fs::create_dir_all(root.join(".echo-agent")).unwrap();
        std::fs::write(
            root.join(".echo-agent/config.toml"),
            "[mcp]\nmax_output_bytes = 30000\n",
        )
        .unwrap();

        // Only the repo root sets the key → root value applies at the subdir.
        assert_eq!(super::project_max_mcp_output_bytes(&sub), Some(30_000));

        // The subdir sets it too → deeper file wins.
        std::fs::write(
            sub.join(".echo-agent/config.toml"),
            "[mcp]\nmax_output_bytes = 50000\n",
        )
        .unwrap();
        assert_eq!(super::project_max_mcp_output_bytes(&sub), Some(50_000));

        // A deeper file *without* the key does not mask the root value.
        std::fs::write(
            sub.join(".echo-agent/config.toml"),
            "[ui]\nvim_mode = true\n",
        )
        .unwrap();
        assert_eq!(super::project_max_mcp_output_bytes(&sub), Some(30_000));

        // No .echo-agent files with the key anywhere → None.
        std::fs::remove_file(root.join(".echo-agent/config.toml")).unwrap();
        assert_eq!(super::project_max_mcp_output_bytes(&sub), None);
    }
}
