use crate::util::config::RemoteSettings;
use toml::Value as TomlValue;

/// Env override for the **remember tool approvals** permission-panel gate.
pub(crate) const ENV_REMEMBER_TOOL_APPROVALS: &str = "ECHO_AGENT_REMEMBER_TOOL_APPROVALS";

/// Default for the `remember_tool_approvals` gate when no layer sets it.
/// Shared with the pager settings modal so the displayed default cannot
/// drift from the resolver.
pub const DEFAULT_REMEMBER_TOOL_APPROVALS: bool = true;

/// Extract the user knob `[ui] remember_tool_approvals` from one TOML layer.
fn remember_tool_approvals_from_toml(v: Option<&TomlValue>) -> Option<bool> {
    v?.get("ui")?.get("remember_tool_approvals")?.as_bool()
}

/// Precedence core shared by the typed resolver and the disk reader so they
/// can't drift: requirement > env > config > managed > remote > default
/// [`DEFAULT_REMEMBER_TOOL_APPROVALS`] (`true`).
fn resolve_remember_tool_approvals_layers(
    requirement: Option<bool>,
    config: Option<bool>,
    managed: Option<bool>,
    feature_flag: Option<bool>,
) -> crate::agent::config::Resolved<bool> {
    use crate::agent::config::BoolFlag;
    BoolFlag::env(ENV_REMEMBER_TOOL_APPROVALS)
        .requirement(requirement)
        .config(config)
        .managed(managed)
        .feature_flag(feature_flag)
        .default(DEFAULT_REMEMBER_TOOL_APPROVALS)
        .resolve()
}

/// Resolve whether the granular per-tool "Always allow …" prompt options are
/// shown. Precedence: requirements > env (`ECHO_AGENT_REMEMBER_TOOL_APPROVALS`) >
/// `[ui].remember_tool_approvals` > managed > remote settings > default `true`
/// ([`DEFAULT_REMEMBER_TOOL_APPROVALS`]).
pub fn resolve_remember_tool_approvals(
    requirements: Option<&TomlValue>,
    user: Option<&TomlValue>,
    managed: Option<&TomlValue>,
    remote: Option<&RemoteSettings>,
) -> crate::agent::config::Resolved<bool> {
    resolve_remember_tool_approvals_layers(
        remember_tool_approvals_from_toml(requirements),
        remember_tool_approvals_from_toml(user),
        remember_tool_approvals_from_toml(managed),
        remote.and_then(|r| r.remember_tool_approvals),
    )
}

/// Process-global cache of the remote tier, read by
/// [`remember_tool_approvals_from_disk`] at spawn (no live `RemoteSettings`
/// there). Fail-safe to `None` on lock poisoning.
static REMOTE_REMEMBER_TOOL_APPROVALS: std::sync::RwLock<Option<bool>> =
    std::sync::RwLock::new(None);

/// Record the remote settings value; called when the agent applies `RemoteSettings`
/// (`agent::init` at startup, `MvpAgent` on refresh).
pub(crate) fn cache_remote_remember_tool_approvals(value: Option<bool>) {
    if let Ok(mut guard) = REMOTE_REMEMBER_TOOL_APPROVALS.write() {
        *guard = value;
    }
}

fn cached_remote_remember_tool_approvals() -> Option<bool> {
    REMOTE_REMEMBER_TOOL_APPROVALS.read().ok().and_then(|g| *g)
}

fn remember_tool_approvals_from_layers(
    layers: &crate::config::ConfigLayers,
    requirements: Option<&TomlValue>,
    remote: Option<bool>,
) -> bool {
    let crate::config::ConfigLayers {
        system_managed,
        managed,
        user,
        env_overlay: _,
        user_requirements: _,
        system_requirements: _,
        mdm_requirements: _,
        campaigns: _,
    } = layers;
    resolve_remember_tool_approvals_layers(
        remember_tool_approvals_from_toml(requirements),
        remember_tool_approvals_from_toml(Some(user)),
        remember_tool_approvals_from_toml(Some(managed))
            .or_else(|| remember_tool_approvals_from_toml(Some(system_managed))),
        remote,
    )
    .value
}

/// Disk form of the gate (overlay-free). Defaults `true`
/// ([`DEFAULT_REMEMBER_TOOL_APPROVALS`]).
pub(crate) fn remember_tool_approvals_from_disk() -> bool {
    let requirements = crate::config::load_merged_requirements();
    let layers = match crate::config::ConfigLayers::load() {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(error = %e, "remember_tool_approvals: failed to load config layers");
            crate::config::ConfigLayers::default()
        }
    };
    remember_tool_approvals_from_layers(
        &layers,
        requirements.as_ref(),
        cached_remote_remember_tool_approvals(),
    )
}

#[cfg(test)]
mod remember_tool_approvals_gate_tests {
    use super::*;
    use crate::agent::config::ConfigSource;

    // `ECHO_AGENT_REMEMBER_TOOL_APPROVALS` is process-global; serialize and force it
    // unset at the top of each test so a developer's shell value can't make
    // these flaky.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn guard() -> std::sync::MutexGuard<'static, ()> {
        let g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe { std::env::remove_var(ENV_REMEMBER_TOOL_APPROVALS) };
        g
    }

    fn toml_ui(v: bool) -> TomlValue {
        toml::from_str(&format!("[ui]\nremember_tool_approvals = {v}\n")).unwrap()
    }

    fn remote(v: Option<bool>) -> RemoteSettings {
        RemoteSettings {
            remember_tool_approvals: v,
            ..RemoteSettings::default()
        }
    }

    #[test]
    fn defaults_on_when_nothing_set() {
        let _g = guard();
        let r = resolve_remember_tool_approvals(None, None, None, None);
        assert!(r.value, "gate must default ON");
        assert_eq!(r.source, ConfigSource::Default);
    }

    #[test]
    fn each_layer_can_turn_it_off() {
        let _g = guard();
        let off = toml_ui(false);
        // requirement
        let r = resolve_remember_tool_approvals(Some(&off), None, None, None);
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Requirement);
        // config (user)
        let r = resolve_remember_tool_approvals(None, Some(&off), None, None);
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Config);
        // managed
        let r = resolve_remember_tool_approvals(None, None, Some(&off), None);
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::ManagedConfig);
        // remote settings
        let r = resolve_remember_tool_approvals(None, None, None, Some(&remote(Some(false))));
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Remote);
    }

    #[test]
    fn explicit_layer_reports_its_source() {
        let _g = guard();
        let on = toml_ui(true);
        // An explicit `true` matches the default but must still resolve with
        // the layer's provenance, not `Default`.
        let r = resolve_remember_tool_approvals(None, Some(&on), None, None);
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::Config);
    }

    #[test]
    fn remote_kill_switch_reads_struct_field() {
        let _g = guard();
        let r = resolve_remember_tool_approvals(None, None, None, Some(&remote(Some(false))));
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Remote);
        // An absent remote tier falls through to the ON default.
        let r = resolve_remember_tool_approvals(None, None, None, Some(&remote(None)));
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::Default);
    }

    #[test]
    fn precedence_config_beats_managed_beats_remote() {
        let _g = guard();
        let off = toml_ui(false);
        let on = toml_ui(true);
        let r =
            resolve_remember_tool_approvals(None, Some(&off), Some(&on), Some(&remote(Some(true))));
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Config);
        let r = resolve_remember_tool_approvals(None, None, Some(&off), Some(&remote(Some(true))));
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::ManagedConfig);
    }

    #[test]
    fn env_overrides_config_and_remote() {
        let _g = guard();
        unsafe { std::env::set_var(ENV_REMEMBER_TOOL_APPROVALS, "1") };
        let off = toml_ui(false);
        let r = resolve_remember_tool_approvals(None, Some(&off), None, Some(&remote(Some(false))));
        assert!(r.value, "env must override config + remote");
        assert_eq!(r.source, ConfigSource::Env);
        unsafe { std::env::remove_var(ENV_REMEMBER_TOOL_APPROVALS) };
    }

    #[test]
    fn requirement_beats_env() {
        let _g = guard();
        unsafe { std::env::set_var(ENV_REMEMBER_TOOL_APPROVALS, "1") };
        let off = toml_ui(false);
        let r = resolve_remember_tool_approvals(Some(&off), None, None, None);
        assert!(!r.value, "requirement (managed/MDM floor) must beat env");
        assert_eq!(r.source, ConfigSource::Requirement);
        unsafe { std::env::remove_var(ENV_REMEMBER_TOOL_APPROVALS) };
    }

    #[test]
    fn remote_cache_round_trips() {
        let _g = guard();
        cache_remote_remember_tool_approvals(Some(true));
        assert_eq!(cached_remote_remember_tool_approvals(), Some(true));
        cache_remote_remember_tool_approvals(Some(false));
        assert_eq!(cached_remote_remember_tool_approvals(), Some(false));
        cache_remote_remember_tool_approvals(None);
        assert_eq!(cached_remote_remember_tool_approvals(), None);
    }
}
