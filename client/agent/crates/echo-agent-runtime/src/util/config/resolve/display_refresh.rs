//! Display-refresh probe + auto-cadence policy resolve and pure cadence derivation.

use crate::util::config::RemoteSettings;
use echo_agent_config_types::DisplayRefreshSettings;
use serde::Deserialize;
use toml::Value as TomlValue;

pub const ENV_DISPLAY_REFRESH_PROBE_ENABLED: &str = "ECHO_AGENT_DISPLAY_REFRESH_PROBE_ENABLED";
pub const ENV_DISPLAY_REFRESH_AUTO_CADENCE: &str = "ECHO_AGENT_DISPLAY_REFRESH_AUTO_CADENCE";

/// Default motion paint cadence (~60 Hz) when env and auto-cadence do not apply.
pub const DISPLAY_REFRESH_DEFAULT_CADENCE_MS: u64 = 16;

/// Client defaults for [`DisplayRefreshPolicy`].
/// Auto on + max_hz 240: `display_refresh_probe` telemetry showed ~0.4% error
/// and common 120/144/165/180/240 Hz rates that clamp cleanly to 8–16 ms.
pub const DISPLAY_REFRESH_DEFAULT_PROBE_ENABLED: bool = true;
pub const DISPLAY_REFRESH_DEFAULT_AUTO_CADENCE_ENABLED: bool = true;
pub const DISPLAY_REFRESH_DEFAULT_FLOOR_MS: u32 = 8;
pub const DISPLAY_REFRESH_DEFAULT_CEILING_MS: u32 = 16;
pub const DISPLAY_REFRESH_DEFAULT_MIN_HZ: u32 = 55;
pub const DISPLAY_REFRESH_DEFAULT_MAX_HZ: u32 = 240;

/// Same band as env cadence knobs (`ECHO_AGENT_MIN_DRAW_MS` / `ECHO_AGENT_SCROLL_CADENCE_MS`).
const CADENCE_MS_MIN: u32 = 1;
const CADENCE_MS_MAX: u32 = 100;

#[cfg(test)]
static DISPLAY_REFRESH_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Effective display-refresh policy after layered resolve (compiled defaults applied).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayRefreshPolicy {
    pub probe_enabled: bool,
    pub auto_cadence_enabled: bool,
    pub floor_ms: u32,
    pub ceiling_ms: u32,
    pub min_hz: u32,
    pub max_hz: u32,
}

impl Default for DisplayRefreshPolicy {
    fn default() -> Self {
        Self {
            probe_enabled: DISPLAY_REFRESH_DEFAULT_PROBE_ENABLED,
            auto_cadence_enabled: DISPLAY_REFRESH_DEFAULT_AUTO_CADENCE_ENABLED,
            floor_ms: DISPLAY_REFRESH_DEFAULT_FLOOR_MS,
            ceiling_ms: DISPLAY_REFRESH_DEFAULT_CEILING_MS,
            min_hz: DISPLAY_REFRESH_DEFAULT_MIN_HZ,
            max_hz: DISPLAY_REFRESH_DEFAULT_MAX_HZ,
        }
    }
}

/// Pure auto-cadence decision from policy + probe Hz (ignores env cadence knobs).
///
/// `ms = clamp(round(1000/hz), floor_ms, ceiling_ms)` when
/// `auto_cadence_enabled` and `hz` is in `[min_hz, max_hz]`; otherwise no auto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AutoCadenceDecision {
    /// Derived cadence when auto applies; `None` when gated off / fail-closed.
    pub ms: Option<u64>,
    /// Stable reason token for telemetry: `flag_off` | `disabled` |
    /// `probe_skip` | `hz_out_of_range` | `applied`.
    pub reason: &'static str,
}

/// Effective min-draw + scroll cadence after auto-cadence + env merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MotionCadence {
    pub min_draw_ms: u64,
    pub scroll_ms: u64,
    /// Derived auto ms used on at least one clock.
    pub auto_applied: bool,
    /// Auto reason, or `env_override` when both env knobs are set.
    pub reason: &'static str,
}

/// One TOML layer: nested `[ui.display_refresh]` via canonical tolerant type.
#[derive(Debug, Clone, Default, PartialEq)]
struct DisplayRefreshLayer {
    settings: DisplayRefreshSettings,
}

impl DisplayRefreshLayer {
    fn from_toml(root: Option<&TomlValue>) -> Self {
        let Some(ui) = root.and_then(|v| v.get("ui")) else {
            return Self::default();
        };
        let settings = ui
            .get("display_refresh")
            .cloned()
            .and_then(|v| DisplayRefreshSettings::deserialize(v).ok())
            .unwrap_or_default();
        Self { settings }
    }
}

/// Priority: 0 requirements (highest) … 4 default (lowest).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Picked {
    value: u32,
    prio: u8,
}

fn pick_u32(
    requirements: Option<u32>,
    user: Option<u32>,
    managed: Option<u32>,
    remote: Option<u32>,
    default: u32,
) -> Picked {
    if let Some(v) = requirements {
        return Picked { value: v, prio: 0 };
    }
    if let Some(v) = user {
        return Picked { value: v, prio: 1 };
    }
    if let Some(v) = managed {
        return Picked { value: v, prio: 2 };
    }
    if let Some(v) = remote {
        return Picked { value: v, prio: 3 };
    }
    Picked {
        value: default,
        prio: 4,
    }
}

fn clamp_cadence_ms(v: u32) -> u32 {
    v.clamp(CADENCE_MS_MIN, CADENCE_MS_MAX)
}

/// When bounds invert, keep the higher-priority bound and collapse the lower
/// tier onto it (same-priority invert → compiled defaults).
fn order_bounds(lo: Picked, hi: Picked, def_lo: u32, def_hi: u32) -> (u32, u32) {
    if lo.value <= hi.value {
        (lo.value, hi.value)
    } else if lo.prio < hi.prio {
        (lo.value, lo.value)
    } else if hi.prio < lo.prio {
        (hi.value, hi.value)
    } else {
        (def_lo, def_hi)
    }
}

/// Resolve display-refresh probe + auto-cadence policy.
///
/// Precedence per field: requirements > env (bools only) > user TOML >
/// managed > remote `display_refresh` object > compiled defaults.
///
/// TOML/remote use tolerant [`DisplayRefreshSettings`]. Floor/ceiling clamp
/// `1..=100`; inverted bounds keep the higher-priority side.
pub fn resolve_display_refresh(
    requirements: Option<&TomlValue>,
    user: Option<&TomlValue>,
    managed: Option<&TomlValue>,
    remote: Option<&RemoteSettings>,
) -> DisplayRefreshPolicy {
    use crate::agent::config::BoolFlag;

    let req = DisplayRefreshLayer::from_toml(requirements);
    let usr = DisplayRefreshLayer::from_toml(user);
    let mng = DisplayRefreshLayer::from_toml(managed);

    let remote_obj = remote.and_then(|r| r.display_refresh.as_ref());
    let remote_probe = remote_obj.and_then(|d| d.probe_enabled);
    let remote_auto = remote_obj.and_then(|d| d.auto_cadence_enabled);

    let probe_enabled = BoolFlag::env(ENV_DISPLAY_REFRESH_PROBE_ENABLED)
        .requirement(req.settings.probe_enabled)
        .config(usr.settings.probe_enabled)
        .managed(mng.settings.probe_enabled)
        .feature_flag(remote_probe)
        .default(DISPLAY_REFRESH_DEFAULT_PROBE_ENABLED)
        .resolve()
        .value;

    let auto_cadence_enabled = BoolFlag::env(ENV_DISPLAY_REFRESH_AUTO_CADENCE)
        .requirement(req.settings.auto_cadence_enabled)
        .config(usr.settings.auto_cadence_enabled)
        .managed(mng.settings.auto_cadence_enabled)
        .feature_flag(remote_auto)
        .default(DISPLAY_REFRESH_DEFAULT_AUTO_CADENCE_ENABLED)
        .resolve()
        .value;

    let floor = pick_u32(
        req.settings.floor_ms,
        usr.settings.floor_ms,
        mng.settings.floor_ms,
        remote_obj.and_then(|d| d.floor_ms),
        DISPLAY_REFRESH_DEFAULT_FLOOR_MS,
    );
    let ceiling = pick_u32(
        req.settings.ceiling_ms,
        usr.settings.ceiling_ms,
        mng.settings.ceiling_ms,
        remote_obj.and_then(|d| d.ceiling_ms),
        DISPLAY_REFRESH_DEFAULT_CEILING_MS,
    );
    let floor = Picked {
        value: clamp_cadence_ms(floor.value),
        prio: floor.prio,
    };
    let ceiling = Picked {
        value: clamp_cadence_ms(ceiling.value),
        prio: ceiling.prio,
    };
    let (floor_ms, ceiling_ms) = order_bounds(
        floor,
        ceiling,
        DISPLAY_REFRESH_DEFAULT_FLOOR_MS,
        DISPLAY_REFRESH_DEFAULT_CEILING_MS,
    );

    let min_hz = pick_u32(
        req.settings.min_hz,
        usr.settings.min_hz,
        mng.settings.min_hz,
        remote_obj.and_then(|d| d.min_hz),
        DISPLAY_REFRESH_DEFAULT_MIN_HZ,
    );
    let max_hz = pick_u32(
        req.settings.max_hz,
        usr.settings.max_hz,
        mng.settings.max_hz,
        remote_obj.and_then(|d| d.max_hz),
        DISPLAY_REFRESH_DEFAULT_MAX_HZ,
    );
    let (min_hz, max_hz) = order_bounds(
        min_hz,
        max_hz,
        DISPLAY_REFRESH_DEFAULT_MIN_HZ,
        DISPLAY_REFRESH_DEFAULT_MAX_HZ,
    );

    DisplayRefreshPolicy {
        probe_enabled,
        auto_cadence_enabled,
        floor_ms,
        ceiling_ms,
        min_hz,
        max_hz,
    }
}

/// Pure auto-cadence derivation from policy + probe Hz.
///
/// - `policy.probe_enabled == false` → reason `disabled` (no Hz, no auto)
/// - `auto_cadence_enabled == false` → reason `flag_off`
/// - no `hz` → reason `probe_skip`
/// - `hz` outside `[min_hz, max_hz]` → reason `hz_out_of_range`
/// - else `ms = clamp(round(1000/hz), floor, ceiling)`, reason `applied`
pub(crate) fn decide_auto_cadence(
    policy: &DisplayRefreshPolicy,
    probe_hz: Option<u32>,
) -> AutoCadenceDecision {
    if !policy.probe_enabled {
        return AutoCadenceDecision {
            ms: None,
            reason: "disabled",
        };
    }
    if !policy.auto_cadence_enabled {
        return AutoCadenceDecision {
            ms: None,
            reason: "flag_off",
        };
    }
    let Some(hz) = probe_hz else {
        return AutoCadenceDecision {
            ms: None,
            reason: "probe_skip",
        };
    };
    if hz < policy.min_hz || hz > policy.max_hz {
        return AutoCadenceDecision {
            ms: None,
            reason: "hz_out_of_range",
        };
    }
    let raw = ((1000.0_f64 / f64::from(hz)).round() as u64).max(1);
    let lo = u64::from(policy.floor_ms);
    let hi = u64::from(policy.ceiling_ms);
    let ms = raw.clamp(lo, hi);
    AutoCadenceDecision {
        ms: Some(ms),
        reason: "applied",
    }
}

/// Merge auto-cadence with optional env cadence overrides.
///
/// Env knobs always win when present (`Some(ms)` even if parse-defaulted).
/// When env is `None`, auto `ms` is used if `Some`, else `default_ms`.
///
/// `reason` is `env_override` when **both** env knobs are set and auto is not
/// gated off (`flag_off` / `disabled`) — including when the probe was skipped
/// for cadence because env already pins both clocks.
pub(crate) fn merge_motion_cadence(
    auto: AutoCadenceDecision,
    min_draw_env: Option<u64>,
    scroll_env: Option<u64>,
    default_ms: u64,
) -> MotionCadence {
    let auto_ms = auto.ms.unwrap_or(default_ms);
    let min_draw_ms = min_draw_env.unwrap_or(auto_ms);
    let scroll_ms = scroll_env.unwrap_or(auto_ms);
    let auto_applied = auto.ms.is_some() && (min_draw_env.is_none() || scroll_env.is_none());
    let both_env = min_draw_env.is_some() && scroll_env.is_some();
    let reason = if both_env && auto.reason != "flag_off" && auto.reason != "disabled" {
        "env_override"
    } else {
        auto.reason
    };
    MotionCadence {
        min_draw_ms,
        scroll_ms,
        auto_applied,
        reason,
    }
}

/// Decide auto-cadence from policy + probe, then merge optional env overrides.
pub fn resolve_motion_cadence(
    policy: &DisplayRefreshPolicy,
    probe_hz: Option<u32>,
    min_draw_env: Option<u64>,
    scroll_env: Option<u64>,
) -> MotionCadence {
    let auto = decide_auto_cadence(policy, probe_hz);
    merge_motion_cadence(
        auto,
        min_draw_env,
        scroll_env,
        DISPLAY_REFRESH_DEFAULT_CADENCE_MS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard() -> std::sync::MutexGuard<'static, ()> {
        let g = DISPLAY_REFRESH_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        unsafe {
            std::env::remove_var(ENV_DISPLAY_REFRESH_PROBE_ENABLED);
            std::env::remove_var(ENV_DISPLAY_REFRESH_AUTO_CADENCE);
        }
        g
    }

    fn toml_nested(body: &str) -> TomlValue {
        toml::from_str(&format!("[ui.display_refresh]\n{body}\n")).unwrap()
    }

    fn remote_object(settings: DisplayRefreshSettings) -> RemoteSettings {
        RemoteSettings {
            display_refresh: Some(settings),
            ..RemoteSettings::default()
        }
    }

    #[test]
    fn defaults_probe_on_auto_on() {
        let _g = guard();
        let p = resolve_display_refresh(None, None, None, None);
        assert_eq!(p, DisplayRefreshPolicy::default());
        assert!(p.probe_enabled);
        assert!(p.auto_cadence_enabled);
        assert_eq!(p.floor_ms, 8);
        assert_eq!(p.ceiling_ms, 16);
        assert_eq!(p.min_hz, 55);
        assert_eq!(p.max_hz, 240);
    }

    #[test]
    fn nested_toml_and_remote_probe_kill() {
        let _g = guard();
        let off = toml_nested("probe_enabled = false\n");
        assert!(!resolve_display_refresh(None, Some(&off), None, None).probe_enabled);
        let remote = remote_object(DisplayRefreshSettings {
            probe_enabled: Some(false),
            auto_cadence_enabled: Some(true),
            ..Default::default()
        });
        let p = resolve_display_refresh(None, None, None, Some(&remote));
        assert!(!p.probe_enabled);
        assert!(p.auto_cadence_enabled);
    }

    #[test]
    fn nested_auto_and_knobs_from_toml_and_remote() {
        let _g = guard();
        let user = toml_nested("auto_cadence_enabled = true\nfloor_ms = 7\nmin_hz = 50\n");
        let remote = remote_object(DisplayRefreshSettings {
            ceiling_ms: Some(12),
            max_hz: Some(200),
            ..Default::default()
        });
        let p = resolve_display_refresh(None, Some(&user), None, Some(&remote));
        assert!(p.auto_cadence_enabled);
        assert_eq!(p.floor_ms, 7);
        assert_eq!(p.ceiling_ms, 12);
        assert_eq!(p.min_hz, 50);
        assert_eq!(p.max_hz, 200);
    }

    #[test]
    fn user_toml_beats_remote_for_auto() {
        let _g = guard();
        let user = toml_nested("auto_cadence_enabled = false\n");
        let remote = remote_object(DisplayRefreshSettings {
            auto_cadence_enabled: Some(true),
            ..Default::default()
        });
        assert!(
            !resolve_display_refresh(None, Some(&user), None, Some(&remote)).auto_cadence_enabled
        );
    }

    #[test]
    fn env_overrides_probe_and_auto() {
        let _g = guard();
        unsafe {
            std::env::set_var(ENV_DISPLAY_REFRESH_PROBE_ENABLED, "0");
            std::env::set_var(ENV_DISPLAY_REFRESH_AUTO_CADENCE, "1");
        }
        let on = toml_nested("probe_enabled = true\nauto_cadence_enabled = false\n");
        let remote = remote_object(DisplayRefreshSettings {
            probe_enabled: Some(true),
            auto_cadence_enabled: Some(false),
            ..Default::default()
        });
        let p = resolve_display_refresh(None, Some(&on), None, Some(&remote));
        assert!(!p.probe_enabled);
        assert!(p.auto_cadence_enabled);
        unsafe {
            std::env::remove_var(ENV_DISPLAY_REFRESH_PROBE_ENABLED);
            std::env::remove_var(ENV_DISPLAY_REFRESH_AUTO_CADENCE);
        }
    }

    #[test]
    fn requirement_beats_env() {
        let _g = guard();
        unsafe {
            std::env::set_var(ENV_DISPLAY_REFRESH_PROBE_ENABLED, "0");
            std::env::set_var(ENV_DISPLAY_REFRESH_AUTO_CADENCE, "0");
        }
        let req = toml_nested("probe_enabled = true\nauto_cadence_enabled = true\n");
        let p = resolve_display_refresh(Some(&req), None, None, None);
        assert!(p.probe_enabled);
        assert!(p.auto_cadence_enabled);
        unsafe {
            std::env::remove_var(ENV_DISPLAY_REFRESH_PROBE_ENABLED);
            std::env::remove_var(ENV_DISPLAY_REFRESH_AUTO_CADENCE);
        }
    }

    #[test]
    fn floor_ceiling_clamp_and_inverted_same_layer_defaults() {
        let _g = guard();
        // same-layer inverted → compiled defaults
        let user = toml_nested("floor_ms = 20\nceiling_ms = 10\n");
        let p = resolve_display_refresh(None, Some(&user), None, None);
        assert_eq!(p.floor_ms, DISPLAY_REFRESH_DEFAULT_FLOOR_MS);
        assert_eq!(p.ceiling_ms, DISPLAY_REFRESH_DEFAULT_CEILING_MS);

        // 0 → clamp to 1
        let zero = toml_nested("floor_ms = 0\nceiling_ms = 0\n");
        let p = resolve_display_refresh(None, Some(&zero), None, None);
        assert_eq!(p.floor_ms, 1);
        assert_eq!(p.ceiling_ms, 1);

        // above env band → clamp to 100
        let hi = toml_nested("floor_ms = 200\nceiling_ms = 500\n");
        let p = resolve_display_refresh(None, Some(&hi), None, None);
        assert_eq!(p.floor_ms, 100);
        assert_eq!(p.ceiling_ms, 100);
    }

    #[test]
    fn higher_priority_bound_wins_when_inverted() {
        let _g = guard();
        // requirements min_hz=100 beats remote max_hz=90 → keep 100..=100
        let req = toml_nested("min_hz = 100\n");
        let remote = remote_object(DisplayRefreshSettings {
            max_hz: Some(90),
            ..Default::default()
        });
        let p = resolve_display_refresh(Some(&req), None, None, Some(&remote));
        assert_eq!(p.min_hz, 100);
        assert_eq!(p.max_hz, 100);
    }

    #[test]
    fn wrong_typed_field_does_not_drop_siblings() {
        let _g = guard();
        let toml = toml::from_str(
            r#"
            [ui.display_refresh]
            probe_enabled = false
            floor_ms = "bad"
            auto_cadence_enabled = true
            "#,
        )
        .unwrap();
        let p = resolve_display_refresh(None, Some(&toml), None, None);
        assert!(!p.probe_enabled);
        assert!(p.auto_cadence_enabled);
        assert_eq!(p.floor_ms, DISPLAY_REFRESH_DEFAULT_FLOOR_MS);
    }

    #[test]
    fn both_env_reports_env_override_without_probe_hz() {
        let policy = DisplayRefreshPolicy::default();
        let m = resolve_motion_cadence(&policy, None, Some(8), Some(8));
        assert_eq!(m.min_draw_ms, 8);
        assert_eq!(m.scroll_ms, 8);
        assert!(!m.auto_applied);
        assert_eq!(m.reason, "env_override");
    }

    fn policy_auto_off() -> DisplayRefreshPolicy {
        DisplayRefreshPolicy {
            auto_cadence_enabled: false,
            ..DisplayRefreshPolicy::default()
        }
    }

    #[test]
    fn decide_applied_by_default() {
        let d = decide_auto_cadence(&DisplayRefreshPolicy::default(), Some(120));
        assert_eq!(d.ms, Some(8));
        assert_eq!(d.reason, "applied");
    }

    #[test]
    fn decide_flag_off_when_auto_disabled() {
        let d = decide_auto_cadence(&policy_auto_off(), Some(120));
        assert_eq!(d.ms, None);
        assert_eq!(d.reason, "flag_off");
    }

    #[test]
    fn decide_disabled_when_probe_off() {
        let p = DisplayRefreshPolicy {
            probe_enabled: false,
            ..DisplayRefreshPolicy::default()
        };
        let d = decide_auto_cadence(&p, Some(120));
        assert_eq!(d.ms, None);
        assert_eq!(d.reason, "disabled");
    }

    #[test]
    fn decide_probe_skip_without_hz() {
        let d = decide_auto_cadence(&DisplayRefreshPolicy::default(), None);
        assert_eq!(d.ms, None);
        assert_eq!(d.reason, "probe_skip");
    }

    #[test]
    fn decide_hz_out_of_range() {
        let d = decide_auto_cadence(&DisplayRefreshPolicy::default(), Some(30));
        assert_eq!(d.ms, None);
        assert_eq!(d.reason, "hz_out_of_range");
        let d = decide_auto_cadence(&DisplayRefreshPolicy::default(), Some(360));
        assert_eq!(d.reason, "hz_out_of_range");
    }

    #[test]
    fn decide_applied_clamps_round_1000_over_hz() {
        let p = DisplayRefreshPolicy::default();
        let d = decide_auto_cadence(&p, Some(120));
        assert_eq!(d.ms, Some(8));
        assert_eq!(d.reason, "applied");
        let d = decide_auto_cadence(&p, Some(60));
        assert_eq!(d.ms, Some(16));
        let d = decide_auto_cadence(&p, Some(144));
        assert_eq!(d.ms, Some(8));
        let d = decide_auto_cadence(&p, Some(55));
        assert_eq!(d.reason, "applied");
        let d = decide_auto_cadence(&p, Some(165));
        assert_eq!(d.reason, "applied");
        let d = decide_auto_cadence(&p, Some(180));
        assert_eq!(d.ms, Some(8));
        assert_eq!(d.reason, "applied");
        let d = decide_auto_cadence(&p, Some(240));
        assert_eq!(d.ms, Some(8));
        assert_eq!(d.reason, "applied");
    }

    #[test]
    fn merge_uses_auto_when_env_unset() {
        let auto = AutoCadenceDecision {
            ms: Some(8),
            reason: "applied",
        };
        let c = merge_motion_cadence(auto, None, None, 16);
        assert_eq!((c.min_draw_ms, c.scroll_ms), (8, 8));
        assert!(c.auto_applied);
        assert_eq!(c.reason, "applied");
    }

    #[test]
    fn merge_env_wins_per_clock() {
        let auto = AutoCadenceDecision {
            ms: Some(8),
            reason: "applied",
        };
        let c = merge_motion_cadence(auto, Some(10), None, 16);
        assert_eq!(c.min_draw_ms, 10);
        assert_eq!(c.scroll_ms, 8);
        assert!(c.auto_applied);
        assert_eq!(c.reason, "applied");
    }

    #[test]
    fn merge_both_env_override_reason() {
        let auto = AutoCadenceDecision {
            ms: Some(8),
            reason: "applied",
        };
        let c = merge_motion_cadence(auto, Some(10), Some(12), 16);
        assert_eq!((c.min_draw_ms, c.scroll_ms), (10, 12));
        assert!(!c.auto_applied);
        assert_eq!(c.reason, "env_override");
    }

    #[test]
    fn merge_defaults_when_auto_off() {
        let auto = AutoCadenceDecision {
            ms: None,
            reason: "flag_off",
        };
        let c = merge_motion_cadence(auto, None, None, 16);
        assert_eq!((c.min_draw_ms, c.scroll_ms), (16, 16));
        assert!(!c.auto_applied);
        assert_eq!(c.reason, "flag_off");
    }

    #[test]
    fn resolve_motion_cadence_folds_decide_and_merge() {
        let p = DisplayRefreshPolicy::default();
        let c = resolve_motion_cadence(&p, Some(120), None, None);
        assert_eq!(c.min_draw_ms, 8);
        assert_eq!(c.scroll_ms, 8);
        assert!(c.auto_applied);
        assert_eq!(c.reason, "applied");

        let c = resolve_motion_cadence(&p, Some(120), Some(10), Some(12));
        assert_eq!((c.min_draw_ms, c.scroll_ms), (10, 12));
        assert!(!c.auto_applied);
        assert_eq!(c.reason, "env_override");
    }

    #[test]
    fn layer_deserializes_partial_object() {
        let toml = toml_nested("auto_cadence_enabled = true\nfloor_ms = 7\n");
        let layer = DisplayRefreshLayer::from_toml(Some(&toml));
        assert_eq!(layer.settings.auto_cadence_enabled, Some(true));
        assert_eq!(layer.settings.floor_ms, Some(7));
        assert_eq!(layer.settings.probe_enabled, None);
    }
}
