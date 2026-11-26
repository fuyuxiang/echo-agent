use crate::util::config::RemoteSettings;
use toml::Value as TomlValue;

/// Env override for the full crash-handler install gate.
pub(crate) const ENV_CRASH_HANDLER: &str = "ECHO_AGENT_CRASH_HANDLER";

/// Extract `[diagnostics] crash_handler` from one TOML layer.
fn crash_handler_from_toml(v: Option<&TomlValue>) -> Option<bool> {
    v?.get("diagnostics")?.get("crash_handler")?.as_bool()
}

/// Precedence core shared by the typed resolver and the disk reader so they
/// can't drift: requirement > env > config > managed > remote > default `false`.
fn resolve_crash_handler_enabled_layers(
    requirement: Option<bool>,
    config: Option<bool>,
    managed: Option<bool>,
    feature_flag: Option<bool>,
) -> crate::agent::config::Resolved<bool> {
    use crate::agent::config::BoolFlag;
    BoolFlag::env(ENV_CRASH_HANDLER)
        .requirement(requirement)
        .config(config)
        .managed(managed)
        .feature_flag(feature_flag)
        .resolve()
}

/// Resolve whether the full crash handler should be installed.
/// Precedence: requirements > env (`ECHO_AGENT_CRASH_HANDLER`) >
/// user `[diagnostics] crash_handler` > managed > remote settings
/// `crash_handler_enabled` > default `false`.
pub fn resolve_crash_handler_enabled(
    requirements: Option<&TomlValue>,
    user: Option<&TomlValue>,
    managed: Option<&TomlValue>,
    remote: Option<&RemoteSettings>,
) -> crate::agent::config::Resolved<bool> {
    resolve_crash_handler_enabled_layers(
        crash_handler_from_toml(requirements),
        crash_handler_from_toml(user),
        crash_handler_from_toml(managed),
        remote.and_then(|r| r.crash_handler_enabled),
    )
}

/// Process-global cache of the remote tier, read by
/// [`load_crash_handler_enabled_sync`] at pre-Tokio install (no live
/// `RemoteSettings` there). Fail-safe to `None` on lock poisoning.
static REMOTE_CRASH_HANDLER_ENABLED: std::sync::RwLock<Option<bool>> = std::sync::RwLock::new(None);

/// Record the remote settings value; called when the agent applies `RemoteSettings`.
pub(crate) fn cache_remote_crash_handler_enabled(value: Option<bool>) {
    if let Ok(mut guard) = REMOTE_CRASH_HANDLER_ENABLED.write() {
        *guard = value;
    }
}

fn cached_remote_crash_handler_enabled() -> Option<bool> {
    REMOTE_CRASH_HANDLER_ENABLED.read().ok().and_then(|g| *g)
}

/// Merge system-managed policy (`/etc/echoagent`) under home `managed_config.toml`
/// so MDM/system layers still reach the managed BoolFlag tier.
fn load_managed_toml_layers() -> Option<TomlValue> {
    let system = crate::config::load_system_managed_config().ok();
    let managed = crate::config::load_managed_config().ok();
    match (system, managed) {
        (None, None) => None,
        (Some(s), None) => Some(s),
        (None, Some(m)) => Some(m),
        (Some(mut s), Some(m)) => {
            echo_agent_config::deep_merge_toml(&mut s, &m);
            Some(s)
        }
    }
}

/// Free-function form of [`resolve_crash_handler_enabled`] for the pager-bin
/// install path (no live `RemoteSettings`): env + requirements + user +
/// system/home managed from disk plus the cached remote tier. Defaults
/// `false`.
pub fn load_crash_handler_enabled_sync() -> bool {
    let requirements = crate::config::load_merged_requirements();
    let user = crate::config::load_from_disk().ok();
    let managed = load_managed_toml_layers();
    resolve_crash_handler_enabled_layers(
        crash_handler_from_toml(requirements.as_ref()),
        crash_handler_from_toml(user.as_ref()),
        crash_handler_from_toml(managed.as_ref()),
        cached_remote_crash_handler_enabled(),
    )
    .value
}

#[cfg(test)]
mod crash_handler_gate_tests {
    use super::*;
    use crate::agent::config::ConfigSource;

    // `ECHO_AGENT_CRASH_HANDLER` is process-global; serialize and force it unset at
    // the top of each test so a developer's shell value can't make these flaky.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn guard() -> std::sync::MutexGuard<'static, ()> {
        let g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe { std::env::remove_var(ENV_CRASH_HANDLER) };
        g
    }

    fn toml_diag(v: bool) -> TomlValue {
        toml::from_str(&format!("[diagnostics]\ncrash_handler = {v}\n")).unwrap()
    }

    fn remote(v: Option<bool>) -> RemoteSettings {
        RemoteSettings {
            crash_handler_enabled: v,
            ..RemoteSettings::default()
        }
    }

    #[test]
    fn defaults_off_when_nothing_set() {
        let _g = guard();
        let r = resolve_crash_handler_enabled(None, None, None, None);
        assert!(!r.value, "gate must default OFF");
        assert_eq!(r.source, ConfigSource::Default);
    }

    #[test]
    fn each_layer_can_turn_it_on() {
        let _g = guard();
        let on = toml_diag(true);
        let r = resolve_crash_handler_enabled(Some(&on), None, None, None);
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::Requirement);
        let r = resolve_crash_handler_enabled(None, Some(&on), None, None);
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::Config);
        let r = resolve_crash_handler_enabled(None, None, Some(&on), None);
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::ManagedConfig);
        let r = resolve_crash_handler_enabled(None, None, None, Some(&remote(Some(true))));
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::Remote);
    }

    #[test]
    fn each_layer_can_force_disable() {
        let _g = guard();
        let off = toml_diag(false);
        let r = resolve_crash_handler_enabled(Some(&off), None, None, Some(&remote(Some(true))));
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Requirement);
        let r = resolve_crash_handler_enabled(None, Some(&off), None, Some(&remote(Some(true))));
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Config);
        let r = resolve_crash_handler_enabled(None, None, Some(&off), Some(&remote(Some(true))));
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::ManagedConfig);
    }

    #[test]
    fn remote_kill_switch_reads_struct_field() {
        let _g = guard();
        let r = resolve_crash_handler_enabled(None, None, None, Some(&remote(Some(false))));
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Remote);
        let r = resolve_crash_handler_enabled(None, None, None, Some(&remote(None)));
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Default);
    }

    #[test]
    fn precedence_config_beats_managed_beats_remote() {
        let _g = guard();
        let off = toml_diag(false);
        let on = toml_diag(true);
        let r =
            resolve_crash_handler_enabled(None, Some(&off), Some(&on), Some(&remote(Some(true))));
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Config);
        let r = resolve_crash_handler_enabled(None, None, Some(&off), Some(&remote(Some(true))));
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::ManagedConfig);
    }

    #[test]
    fn env_overrides_config_and_remote() {
        let _g = guard();
        unsafe { std::env::set_var(ENV_CRASH_HANDLER, "1") };
        let off = toml_diag(false);
        let r = resolve_crash_handler_enabled(None, Some(&off), None, Some(&remote(Some(false))));
        assert!(r.value, "env must override config + remote");
        assert_eq!(r.source, ConfigSource::Env);
        unsafe { std::env::remove_var(ENV_CRASH_HANDLER) };
    }

    #[test]
    fn env_can_force_disable_over_config() {
        let _g = guard();
        unsafe { std::env::set_var(ENV_CRASH_HANDLER, "0") };
        let on = toml_diag(true);
        let r = resolve_crash_handler_enabled(None, Some(&on), None, Some(&remote(Some(true))));
        assert!(!r.value, "env=0 must override config + remote");
        assert_eq!(r.source, ConfigSource::Env);
        unsafe { std::env::remove_var(ENV_CRASH_HANDLER) };
    }

    #[test]
    fn requirement_beats_env() {
        let _g = guard();
        unsafe { std::env::set_var(ENV_CRASH_HANDLER, "1") };
        let off = toml_diag(false);
        let r = resolve_crash_handler_enabled(Some(&off), None, None, None);
        assert!(!r.value, "requirement must beat env");
        assert_eq!(r.source, ConfigSource::Requirement);
        unsafe { std::env::remove_var(ENV_CRASH_HANDLER) };
    }

    #[test]
    fn remote_cache_round_trips() {
        let _g = guard();
        cache_remote_crash_handler_enabled(Some(true));
        assert_eq!(cached_remote_crash_handler_enabled(), Some(true));
        cache_remote_crash_handler_enabled(Some(false));
        assert_eq!(cached_remote_crash_handler_enabled(), Some(false));
        cache_remote_crash_handler_enabled(None);
        assert_eq!(cached_remote_crash_handler_enabled(), None);
    }
}
