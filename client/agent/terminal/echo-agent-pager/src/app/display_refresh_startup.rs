//! Display-refresh probe + motion cadence at TUI startup.
//!
//! Owns env cadence knobs, fail-closed probe planning (sync only when auto can
//! change a clock), paint-clock resolution, and the terminal + display-refresh
//! telemetry `spawn_blocking`. Keeps the event loop free of this policy glue.

use std::time::Duration;

use echo_agent_runtime::util::config::{
    DISPLAY_REFRESH_DEFAULT_CADENCE_MS, MotionCadence, RemoteSettings, resolve_display_refresh,
    resolve_motion_cadence,
};
use toml::Value as TomlValue;

/// Inclusive bounds for motion cadence env knobs (`ECHO_AGENT_MIN_DRAW_MS`,
/// `ECHO_AGENT_SCROLL_CADENCE_MS`).
const CADENCE_ENV_MIN_MS: u64 = 1;
const CADENCE_ENV_MAX_MS: u64 = 100;

/// Paint clocks resolved once at startup (fixed for the event-loop lifetime).
#[derive(Debug, Clone, Copy)]
pub struct MotionClocks {
    pub min_draw_interval: Duration,
    pub scroll_cadence: Duration,
}

/// Pure parse for cadence ms: trim/empty/invalid → `default_ms`, clamp 1..=100.
fn parse_cadence_ms(raw: Option<&str>, default_ms: u64) -> u64 {
    raw.and_then(|v| {
        let t = v.trim();
        if t.is_empty() {
            None
        } else {
            t.parse::<u64>().ok()
        }
    })
    .unwrap_or(default_ms)
    .clamp(CADENCE_ENV_MIN_MS, CADENCE_ENV_MAX_MS)
}

/// Read a cadence env knob: `(set, ms)`. `set` is true when the var is present
/// (empty/invalid still counts as set → `default_ms` after clamp 1..=100).
fn cadence_ms_from_env(name: &str, default_ms: u64) -> (bool, u64) {
    match std::env::var(name) {
        Ok(raw) => (true, parse_cadence_ms(Some(&raw), default_ms)),
        Err(_) => (false, default_ms),
    }
}

/// How to obtain the display-refresh probe for telemetry (and optionally cadence).
enum ProbePlan {
    /// Kill switch: no FFI; emit skipped/disabled.
    Disabled,
    /// Auto-cadence needs Hz before paint clocks pin — probe on the main path.
    Sync(crate::host::DisplayRefreshProbeResult),
    /// Telemetry-only: probe in the background task (default path).
    Async,
}

struct StartupTel {
    plan: ProbePlan,
    auto_cadence_enabled: bool,
    cadence: MotionCadence,
}

/// Resolve policy, pin motion clocks, and spawn terminal + display-refresh
/// telemetry off the first-paint path.
pub fn start(
    requirements: Option<&TomlValue>,
    user: Option<&TomlValue>,
    managed: Option<&TomlValue>,
    remote: Option<&RemoteSettings>,
) -> MotionClocks {
    let policy = resolve_display_refresh(requirements, user, managed, remote);
    let default_cadence_ms = DISPLAY_REFRESH_DEFAULT_CADENCE_MS;
    let (min_draw_env_set, min_draw_env_ms) =
        cadence_ms_from_env("ECHO_AGENT_MIN_DRAW_MS", default_cadence_ms);
    let (scroll_env_set, scroll_env_ms) =
        cadence_ms_from_env("ECHO_AGENT_SCROLL_CADENCE_MS", default_cadence_ms);
    let both_env_cadence = min_draw_env_set && scroll_env_set;
    let need_probe_for_cadence =
        policy.probe_enabled && policy.auto_cadence_enabled && !both_env_cadence;
    let plan = if !policy.probe_enabled {
        ProbePlan::Disabled
    } else if need_probe_for_cadence {
        ProbePlan::Sync(crate::host::probe_display_refresh())
    } else {
        ProbePlan::Async
    };
    let probe_hz = match &plan {
        ProbePlan::Sync(p) => p.hz,
        ProbePlan::Disabled | ProbePlan::Async => None,
    };
    let cadence = resolve_motion_cadence(
        &policy,
        probe_hz,
        min_draw_env_set.then_some(min_draw_env_ms),
        scroll_env_set.then_some(scroll_env_ms),
    );
    let clocks = MotionClocks {
        min_draw_interval: Duration::from_millis(cadence.min_draw_ms),
        scroll_cadence: Duration::from_millis(cadence.scroll_ms),
    };

    let tel = StartupTel {
        plan,
        auto_cadence_enabled: policy.auto_cadence_enabled,
        cadence,
    };
    spawn_terminal_and_display_refresh_telemetry(tel);
    clocks
}

fn spawn_terminal_and_display_refresh_telemetry(tel: StartupTel) {
    tokio::task::spawn_blocking(move || {
        let t = crate::terminal::terminal_context().telemetry_snapshot();
        let _span = tracing::info_span!(
            "terminal.detect",
            terminal.brand = %t.brand,
            terminal.multiplexer = %t.multiplexer,
            terminal.is_ssh = t.is_ssh,
            terminal.is_byobu = t.is_byobu,
            terminal.tmux_version = %t.tmux_version,
            terminal.term_var = %t.term_var,
            terminal.xtversion = %t.xtversion,
            terminal.term_version = %t.term_version,
            terminal.term_version_source = %t.term_version_source,
            terminal.kitty_event_types_withheld = t.kitty_event_types_withheld,
        )
        .entered();
        tracing::info!("terminal environment detected");
        echo_agent_telemetry::session_ctx::log_event(t.clone());

        let (outcome, hz, source, skip_reason, duration_ms) = match tel.plan {
            ProbePlan::Disabled => ("skipped", None, "none".into(), "disabled".into(), 0_u64),
            ProbePlan::Sync(p) => (
                p.outcome(),
                p.hz,
                p.source.to_string(),
                p.skip_reason.to_string(),
                p.duration_ms,
            ),
            ProbePlan::Async => {
                let p = crate::host::probe_display_refresh();
                (
                    p.outcome(),
                    p.hz,
                    p.source.to_string(),
                    p.skip_reason.to_string(),
                    p.duration_ms,
                )
            }
        };
        let c = tel.cadence;
        // OTLP path: numerics as i64 (u32/u64 fall back to strings and get dropped).
        let duration_ms_i = duration_ms as i64;
        let min_draw_i = c.min_draw_ms as i64;
        let scroll_i = c.scroll_ms as i64;
        let hz_i = hz.map(i64::from);
        tracing::info!(
            outcome,
            hz = hz_i,
            source = %source,
            skip_reason = %skip_reason,
            duration_ms = duration_ms_i,
            auto_cadence_enabled = tel.auto_cadence_enabled,
            auto_cadence_applied = c.auto_applied,
            effective_min_draw_ms = min_draw_i,
            effective_scroll_cadence_ms = scroll_i,
            auto_cadence_reason = c.reason,
            "display refresh probed"
        );
        echo_agent_telemetry::session_ctx::log_event(
            echo_agent_telemetry::events::DisplayRefreshProbe {
                terminal: t,
                outcome: outcome.to_string(),
                hz: hz_i,
                source,
                skip_reason,
                duration_ms: duration_ms_i,
                auto_cadence_enabled: tel.auto_cadence_enabled,
                auto_cadence_applied: c.auto_applied,
                effective_min_draw_ms: min_draw_i,
                effective_scroll_cadence_ms: scroll_i,
                auto_cadence_reason: c.reason.to_string(),
            },
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cadence_ms_matrix() {
        assert_eq!(parse_cadence_ms(None, 16), 16, "unset → default");
        assert_eq!(parse_cadence_ms(Some(""), 16), 16, "empty → default");
        assert_eq!(parse_cadence_ms(Some("  "), 16), 16, "whitespace → default");
        assert_eq!(parse_cadence_ms(Some("nope"), 16), 16, "invalid → default");
        assert_eq!(parse_cadence_ms(Some("0"), 16), 1, "below min → clamp 1");
        assert_eq!(
            parse_cadence_ms(Some("101"), 16),
            100,
            "above max → clamp 100"
        );
        assert_eq!(parse_cadence_ms(Some("8"), 16), 8, "in range");
        assert_eq!(parse_cadence_ms(Some(" 8 "), 16), 8, "trim + in range");
        assert_eq!(parse_cadence_ms(Some("1"), 16), 1, "min inclusive");
        assert_eq!(parse_cadence_ms(Some("100"), 16), 100, "max inclusive");
    }
}
