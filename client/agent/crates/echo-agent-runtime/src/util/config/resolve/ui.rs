use crate::util::config::RemoteSettings;
use toml::Value as TomlValue;

/// Env override for showing agent thinking blocks in the TUI.
pub const ENV_SHOW_THINKING_BLOCKS: &str = "ECHO_AGENT_SHOW_THINKING_BLOCKS";

#[cfg(test)]
static SHOW_THINKING_BLOCKS_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Shared precedence core for `[ui]` bool flags: requirement > env >
/// `[ui].<ui_key>` config > managed > remote (already extracted) > `default`.
fn resolve_ui_bool(
    env_var: &str,
    ui_key: &str,
    default: bool,
    requirements: Option<&TomlValue>,
    user: Option<&TomlValue>,
    managed: Option<&TomlValue>,
    remote_value: Option<bool>,
) -> crate::agent::config::Resolved<bool> {
    use crate::agent::config::BoolFlag;
    let from_toml =
        |v: Option<&TomlValue>| -> Option<bool> { v?.get("ui")?.get(ui_key)?.as_bool() };
    BoolFlag::env(env_var)
        .requirement(from_toml(requirements))
        .config(from_toml(user))
        .managed(from_toml(managed))
        .feature_flag(remote_value)
        .default(default)
        .resolve()
}

/// Resolve whether the TUI should show agent thinking/reasoning blocks.
///
/// Precedence: requirements > env (`ECHO_AGENT_SHOW_THINKING_BLOCKS`) >
/// `[ui] show_thinking_blocks` > managed > remote settings > default `true`.
pub fn resolve_show_thinking_blocks(
    requirements: Option<&TomlValue>,
    user: Option<&TomlValue>,
    managed: Option<&TomlValue>,
    remote: Option<&RemoteSettings>,
) -> crate::agent::config::Resolved<bool> {
    resolve_ui_bool(
        ENV_SHOW_THINKING_BLOCKS,
        "show_thinking_blocks",
        true,
        requirements,
        user,
        managed,
        remote.and_then(|r| r.show_thinking_blocks),
    )
}

/// Env override for grouping consecutive non-destructive tool calls in the TUI.
pub const ENV_GROUP_TOOL_VERBS: &str = "ECHO_AGENT_GROUP_TOOL_VERBS";

#[cfg(test)]
static GROUP_TOOL_VERBS_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Resolve whether the TUI folds runs of consecutive non-destructive tool
/// calls (reads/searches/lists) into one transcript row.
///
/// Precedence: requirements > env (`ECHO_AGENT_GROUP_TOOL_VERBS`) >
/// `[ui] group_tool_verbs` > managed > remote settings > default `true`
/// (remote `Some(false)` is the kill switch).
pub fn resolve_group_tool_verbs(
    requirements: Option<&TomlValue>,
    user: Option<&TomlValue>,
    managed: Option<&TomlValue>,
    remote: Option<&RemoteSettings>,
) -> crate::agent::config::Resolved<bool> {
    resolve_ui_bool(
        ENV_GROUP_TOOL_VERBS,
        "group_tool_verbs",
        true,
        requirements,
        user,
        managed,
        remote.and_then(|r| r.group_tool_verbs),
    )
}

/// Env override for the collapsed-Edit-blocks default in the TUI.
pub const ENV_COLLAPSED_EDIT_BLOCKS: &str = "ECHO_AGENT_COLLAPSED_EDIT_BLOCKS";

#[cfg(test)]
static COLLAPSED_EDIT_BLOCKS_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Resolve whether the TUI shows Edit tool calls as a collapsed one-line
/// `+N/-M` diffstat summary by default (expand for the diff).
///
/// Precedence: requirements > env (`ECHO_AGENT_COLLAPSED_EDIT_BLOCKS`) >
/// `[ui] collapsed_edit_blocks` > managed > remote (GrowthBook) > default
/// `false` (rollout flag: off keeps the legacy expanded-diff view).
pub fn resolve_collapsed_edit_blocks(
    requirements: Option<&TomlValue>,
    user: Option<&TomlValue>,
    managed: Option<&TomlValue>,
    remote: Option<&RemoteSettings>,
) -> crate::agent::config::Resolved<bool> {
    resolve_ui_bool(
        ENV_COLLAPSED_EDIT_BLOCKS,
        "collapsed_edit_blocks",
        false,
        requirements,
        user,
        managed,
        remote.and_then(|r| r.collapsed_edit_blocks),
    )
}

/// Resolve the opt-in mouse-reporting toggle shortcut flag.
///
/// When enabled, the pager registers `Ctrl+R` (scrollback-focused only) so the
/// user can flip terminal mouse capture and hand selection back to the terminal
/// for native click-drag copy/paste.
///
/// Precedence: `ECHO_AGENT_MOUSE_REPORTING_TOGGLE` env > `[ui] mouse_reporting_toggle`
/// in effective config > the parsed [`UiConfig`] field (defends against a
/// partial deserialize) > default (`false`). Returns [`Resolved`] so callers can
/// log the winning source.
///
/// [`UiConfig`]: crate::agent::config::UiConfig
/// [`Resolved`]: crate::agent::config::Resolved
pub fn resolve_mouse_reporting_toggle(
    effective_config: Option<&TomlValue>,
    ui: &crate::agent::config::UiConfig,
) -> crate::agent::config::Resolved<bool> {
    use crate::agent::config::BoolFlag;
    let from_effective = effective_config
        .and_then(|c| c.get("ui"))
        .and_then(|ui| ui.get("mouse_reporting_toggle"))
        .and_then(|v| v.as_bool());
    BoolFlag::env("ECHO_AGENT_MOUSE_REPORTING_TOGGLE")
        .config(from_effective.or(ui.mouse_reporting_toggle))
        .resolve()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Assumes ECHO_AGENT_MOUSE_REPORTING_TOGGLE is unset in the test env.
    #[test]
    fn resolve_mouse_reporting_toggle_defaults_off() {
        use crate::agent::config::{ConfigSource, UiConfig};
        let resolved = resolve_mouse_reporting_toggle(None, &UiConfig::default());
        assert!(!resolved.value);
        assert_eq!(resolved.source, ConfigSource::Default);
    }

    #[test]
    fn resolve_mouse_reporting_toggle_reads_effective_config() {
        use crate::agent::config::{ConfigSource, UiConfig};
        let effective: TomlValue = toml::from_str("[ui]\nmouse_reporting_toggle = true\n").unwrap();
        let resolved = resolve_mouse_reporting_toggle(Some(&effective), &UiConfig::default());
        assert!(resolved.value);
        assert_eq!(resolved.source, ConfigSource::Config);
    }

    #[test]
    fn resolve_mouse_reporting_toggle_falls_back_to_ui_struct() {
        use crate::agent::config::UiConfig;
        let ui = UiConfig {
            mouse_reporting_toggle: Some(true),
            ..UiConfig::default()
        };
        // No effective config → the parsed struct field is the fallback layer.
        let resolved = resolve_mouse_reporting_toggle(None, &ui);
        assert!(resolved.value);
    }
}

#[cfg(test)]
mod show_thinking_blocks_tests {
    use super::*;
    use crate::agent::config::ConfigSource;

    fn guard() -> std::sync::MutexGuard<'static, ()> {
        let g = super::SHOW_THINKING_BLOCKS_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        unsafe { std::env::remove_var(ENV_SHOW_THINKING_BLOCKS) };
        g
    }

    fn toml_ui(v: bool) -> TomlValue {
        toml::from_str(&format!("[ui]\nshow_thinking_blocks = {v}\n")).unwrap()
    }

    fn remote(v: Option<bool>) -> RemoteSettings {
        RemoteSettings {
            show_thinking_blocks: v,
            ..RemoteSettings::default()
        }
    }

    #[test]
    fn defaults_on_when_nothing_set() {
        let _g = guard();
        let r = resolve_show_thinking_blocks(None, None, None, None);
        assert!(r.value, "thinking blocks must default ON");
        assert_eq!(r.source, ConfigSource::Default);
    }

    #[test]
    fn each_layer_can_turn_it_off() {
        let _g = guard();
        let off = toml_ui(false);
        let r = resolve_show_thinking_blocks(Some(&off), None, None, None);
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Requirement);
        let r = resolve_show_thinking_blocks(None, Some(&off), None, None);
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Config);
        let r = resolve_show_thinking_blocks(None, None, Some(&off), None);
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::ManagedConfig);
        let r = resolve_show_thinking_blocks(None, None, None, Some(&remote(Some(false))));
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Remote);
    }

    #[test]
    fn env_overrides_config_and_remote() {
        let _g = guard();
        unsafe { std::env::set_var(ENV_SHOW_THINKING_BLOCKS, "0") };
        let on = toml_ui(true);
        let r = resolve_show_thinking_blocks(None, Some(&on), None, Some(&remote(Some(true))));
        assert!(!r.value, "env must override config + remote");
        assert_eq!(r.source, ConfigSource::Env);
        unsafe { std::env::remove_var(ENV_SHOW_THINKING_BLOCKS) };
    }

    #[test]
    fn requirement_beats_env() {
        let _g = guard();
        unsafe { std::env::set_var(ENV_SHOW_THINKING_BLOCKS, "0") };
        let on = toml_ui(true);
        let r = resolve_show_thinking_blocks(Some(&on), None, None, None);
        assert!(r.value, "requirement must beat env");
        assert_eq!(r.source, ConfigSource::Requirement);
        unsafe { std::env::remove_var(ENV_SHOW_THINKING_BLOCKS) };
    }

    #[test]
    fn config_beats_managed_beats_remote() {
        let _g = guard();
        let off = toml_ui(false);
        let on = toml_ui(true);
        let r =
            resolve_show_thinking_blocks(None, Some(&off), Some(&on), Some(&remote(Some(true))));
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Config);
        let r = resolve_show_thinking_blocks(None, None, Some(&off), Some(&remote(Some(true))));
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::ManagedConfig);
    }
}

#[cfg(test)]
mod group_tool_verbs_tests {
    use super::*;
    use crate::agent::config::ConfigSource;

    fn guard() -> std::sync::MutexGuard<'static, ()> {
        let g = super::GROUP_TOOL_VERBS_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        unsafe { std::env::remove_var(ENV_GROUP_TOOL_VERBS) };
        g
    }

    fn toml_ui(v: bool) -> TomlValue {
        toml::from_str(&format!("[ui]\ngroup_tool_verbs = {v}\n")).unwrap()
    }

    fn remote(v: Option<bool>) -> RemoteSettings {
        RemoteSettings {
            group_tool_verbs: v,
            ..RemoteSettings::default()
        }
    }

    #[test]
    fn defaults_on_when_nothing_set() {
        let _g = guard();
        let r = resolve_group_tool_verbs(None, None, None, None);
        assert!(r.value, "tool-verb grouping must default ON");
        assert_eq!(r.source, ConfigSource::Default);
    }

    #[test]
    fn each_layer_can_turn_it_off() {
        let _g = guard();
        let off = toml_ui(false);
        let r = resolve_group_tool_verbs(Some(&off), None, None, None);
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Requirement);
        unsafe { std::env::set_var(ENV_GROUP_TOOL_VERBS, "0") };
        let r = resolve_group_tool_verbs(None, None, None, None);
        assert!(!r.value, "env disable must beat the true default");
        assert_eq!(r.source, ConfigSource::Env);
        unsafe { std::env::remove_var(ENV_GROUP_TOOL_VERBS) };
        let r = resolve_group_tool_verbs(None, Some(&off), None, None);
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Config);
        let r = resolve_group_tool_verbs(None, None, Some(&off), None);
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::ManagedConfig);
        let r = resolve_group_tool_verbs(None, None, None, Some(&remote(Some(false))));
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Remote);
    }

    #[test]
    fn env_overrides_config_and_remote() {
        let _g = guard();
        unsafe { std::env::set_var(ENV_GROUP_TOOL_VERBS, "0") };
        let on = toml_ui(true);
        let r = resolve_group_tool_verbs(None, Some(&on), None, Some(&remote(Some(true))));
        assert!(!r.value, "env must override config + remote");
        assert_eq!(r.source, ConfigSource::Env);
        unsafe { std::env::remove_var(ENV_GROUP_TOOL_VERBS) };
    }

    #[test]
    fn requirement_beats_env() {
        let _g = guard();
        unsafe { std::env::set_var(ENV_GROUP_TOOL_VERBS, "0") };
        let on = toml_ui(true);
        let r = resolve_group_tool_verbs(Some(&on), None, None, None);
        assert!(r.value, "requirement must beat env");
        assert_eq!(r.source, ConfigSource::Requirement);
        unsafe { std::env::remove_var(ENV_GROUP_TOOL_VERBS) };
    }

    #[test]
    fn config_beats_managed_beats_remote() {
        let _g = guard();
        let off = toml_ui(false);
        let on = toml_ui(true);
        let r = resolve_group_tool_verbs(None, Some(&off), Some(&on), Some(&remote(Some(true))));
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Config);
        let r = resolve_group_tool_verbs(None, None, Some(&off), Some(&remote(Some(true))));
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::ManagedConfig);
    }
}

#[cfg(test)]
mod collapsed_edit_blocks_tests {
    use super::*;
    use crate::agent::config::ConfigSource;

    fn guard() -> std::sync::MutexGuard<'static, ()> {
        let g = super::COLLAPSED_EDIT_BLOCKS_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        unsafe { std::env::remove_var(ENV_COLLAPSED_EDIT_BLOCKS) };
        g
    }

    fn toml_ui(v: bool) -> TomlValue {
        toml::from_str(&format!("[ui]\ncollapsed_edit_blocks = {v}\n")).unwrap()
    }

    fn remote(v: Option<bool>) -> RemoteSettings {
        RemoteSettings {
            collapsed_edit_blocks: v,
            ..RemoteSettings::default()
        }
    }

    #[test]
    fn defaults_off_when_nothing_set() {
        let _g = guard();
        let r = resolve_collapsed_edit_blocks(None, None, None, None);
        assert!(!r.value, "collapsed edit blocks must default OFF");
        assert_eq!(r.source, ConfigSource::Default);
    }

    #[test]
    fn each_layer_can_turn_it_on() {
        let _g = guard();
        let on = toml_ui(true);
        let r = resolve_collapsed_edit_blocks(Some(&on), None, None, None);
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::Requirement);
        unsafe { std::env::set_var(ENV_COLLAPSED_EDIT_BLOCKS, "1") };
        let r = resolve_collapsed_edit_blocks(None, None, None, None);
        assert!(r.value, "env enable must beat the false default");
        assert_eq!(r.source, ConfigSource::Env);
        unsafe { std::env::remove_var(ENV_COLLAPSED_EDIT_BLOCKS) };
        let r = resolve_collapsed_edit_blocks(None, Some(&on), None, None);
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::Config);
        let r = resolve_collapsed_edit_blocks(None, None, Some(&on), None);
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::ManagedConfig);
        let r = resolve_collapsed_edit_blocks(None, None, None, Some(&remote(Some(true))));
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::Remote);
    }

    #[test]
    fn env_overrides_config_and_remote() {
        let _g = guard();
        unsafe { std::env::set_var(ENV_COLLAPSED_EDIT_BLOCKS, "0") };
        let on = toml_ui(true);
        let r = resolve_collapsed_edit_blocks(None, Some(&on), None, Some(&remote(Some(true))));
        assert!(!r.value, "env must override config + remote");
        assert_eq!(r.source, ConfigSource::Env);
        unsafe { std::env::remove_var(ENV_COLLAPSED_EDIT_BLOCKS) };
    }

    #[test]
    fn requirement_beats_env() {
        let _g = guard();
        unsafe { std::env::set_var(ENV_COLLAPSED_EDIT_BLOCKS, "1") };
        let off = toml_ui(false);
        let r = resolve_collapsed_edit_blocks(Some(&off), None, None, None);
        assert!(!r.value, "requirement must beat env");
        assert_eq!(r.source, ConfigSource::Requirement);
        unsafe { std::env::remove_var(ENV_COLLAPSED_EDIT_BLOCKS) };
    }

    #[test]
    fn config_beats_managed_beats_remote() {
        let _g = guard();
        let off = toml_ui(false);
        let on = toml_ui(true);
        let r =
            resolve_collapsed_edit_blocks(None, Some(&on), Some(&off), Some(&remote(Some(false))));
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::Config);
        let r = resolve_collapsed_edit_blocks(None, None, Some(&on), Some(&remote(Some(false))));
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::ManagedConfig);
    }
}
