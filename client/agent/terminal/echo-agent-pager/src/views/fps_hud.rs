//! Release-safe FPS readout — `/debug fps`, `ECHO_AGENT_FPS` on release builds.
//!
//! The full frame profiler (`render::frame_metrics`, `ECHO_AGENT_FPS`) is compiled
//! only in debug/dev builds because it threads per-phase timings
//! through `draw_frame`. This HUD measures the one thing that needs no
//! pipeline change — the wall-clock duration of the whole `draw_frame` call
//! (render + flush + writer handoff) — so it compiles into release builds
//! behind a runtime toggle (the scroll-debug HUD precedent) and profiles the
//! production render path with zero fidelity gap.
//!
//! `ECHO_AGENT_FPS` ownership: in debug/dev builds the env feeds `FrameMetrics` as
//! always and this HUD stays toggle-only (no double overlay); on release
//! binaries — where that overlay does not exist — the same env enables this
//! HUD from startup, so `ECHO_AGENT_FPS=1` is never a silent no-op
//! ([`HONORS_ECHO_AGENT_FPS_ENV`]).
//!
//! "fps" here is render throughput (1 / mean frame cost), not paint
//! frequency: the pager draws on demand, so an idle UI paints nothing and a
//! busy one is bounded by this number.
//!
//! Invariant (shared with the scroll HUD): pure observation. Disabled cost
//! is one bool check per frame; rendering only paints buffer cells.
use super::debug_style;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use std::collections::VecDeque;
use std::time::{Duration, Instant};
/// Frame-duration ring buffer capacity (~4s at 30fps).
const SAMPLE_CAP: usize = 120;
/// Overlay text refresh cadence; avoids re-sorting/formatting every frame.
const REFRESH: Duration = Duration::from_millis(250);
/// Panel width in cells; each line is padded/truncated to this.
const PANEL_WIDTH: u16 = 32;
/// Whether this HUD owns the `ECHO_AGENT_FPS` env gate: only where the dev
/// `FrameMetrics` overlay is compiled out. In debug/dev builds the env keeps
/// feeding that overlay alone.
const HONORS_ECHO_AGENT_FPS_ENV: bool = true;
/// Runtime state for the FPS HUD. `ECHO_AGENT_FPS` enables it at startup on
/// release binaries ([`HONORS_ECHO_AGENT_FPS_ENV`]); `/debug fps` toggles it
/// live everywhere. Deliberately NOT a settings-registry entry: it is a
/// diagnostic, not a preference to persist.
pub struct FpsHud {
    enabled: bool,
    samples: VecDeque<Duration>,
    /// Cached stats line, rewritten at most every [`REFRESH`].
    body: String,
    last_refresh: Option<Instant>,
}
impl Default for FpsHud {
    fn default() -> Self {
        Self::new()
    }
}
impl FpsHud {
    pub fn new() -> Self {
        Self::with_env(std::env::var("ECHO_AGENT_FPS").ok())
    }
    /// `env` is the raw `ECHO_AGENT_FPS` value; the truthiness rule (nonempty and
    /// not `"0"`) matches `FrameMetrics` and `ECHO_AGENT_SCROLL_DEBUG`.
    fn with_env(env: Option<String>) -> Self {
        let env_on = HONORS_ECHO_AGENT_FPS_ENV && env.is_some_and(|v| !v.is_empty() && v != "0");
        Self {
            enabled: env_on,
            samples: VecDeque::with_capacity(SAMPLE_CAP),
            body: String::new(),
            last_refresh: None,
        }
    }
    /// Whether the HUD is currently enabled.
    pub fn enabled(&self) -> bool {
        self.enabled
    }
    /// `/debug fps` runtime toggle.
    pub fn toggle(&mut self) {
        self.enabled = !self.enabled;
        self.samples.clear();
        self.body.clear();
        self.last_refresh = None;
    }
    /// Rows the overlay occupies when enabled (for stacking overlays).
    pub fn overlay_height(&self) -> u16 {
        if self.enabled { 2 } else { 0 }
    }
    /// Record one frame's `draw_frame` wall duration.
    pub fn record(&mut self, frame: Duration) {
        if !self.enabled {
            return;
        }
        if self.samples.len() >= SAMPLE_CAP {
            self.samples.pop_front();
        }
        self.samples.push_back(frame);
    }
    /// Owned per-frame render params (`None` unless enabled), assembled by
    /// `AppView::draw` BEFORE the frame closure — the `ScrollDebugPanel`
    /// pattern. `top_offset` leaves rows for overlays stacked above.
    pub fn overlay(&mut self, top_offset: u16) -> Option<FpsOverlay> {
        if !self.enabled {
            return None;
        }
        if self.last_refresh.is_none_or(|at| at.elapsed() >= REFRESH) {
            self.body = format_stats(&self.samples);
            self.last_refresh = Some(Instant::now());
        }
        Some(FpsOverlay {
            body: self.body.clone(),
            top_offset,
        })
    }
}
/// Mean/percentile line from the ring buffer; placeholder before samples.
fn format_stats(samples: &VecDeque<Duration>) -> String {
    if samples.is_empty() {
        return "fps:- p50:- p95:-".to_string();
    }
    let mut ms: Vec<f64> = samples.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
    ms.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mean_ms = ms.iter().sum::<f64>() / ms.len() as f64;
    let fps = if mean_ms > 1e-6 {
        1000.0 / mean_ms
    } else {
        0.0
    };
    let p50 = percentile(&ms, 50.0);
    let p95 = percentile(&ms, 95.0);
    format!("fps:{fps:.0} p50:{p50:.1}ms p95:{p95:.1}ms")
}
/// Linear-interpolation percentile from a sorted slice.
fn percentile(sorted: &[f64], pct: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (pct / 100.0) * (sorted.len() - 1) as f64;
    let lower = rank.floor() as usize;
    if lower + 1 >= sorted.len() {
        return sorted[lower];
    }
    let frac = rank - lower as f64;
    sorted[lower] + (sorted[lower + 1] - sorted[lower]) * frac
}
/// Owned render params for one frame (title + stats line, top-right).
pub struct FpsOverlay {
    body: String,
    /// Rows left free for overlays above (the dev `ECHO_AGENT_FPS` line).
    pub top_offset: u16,
}
impl FpsOverlay {
    /// Paint the two-line panel in the top-right corner of `area`, in the
    /// shared theme-agnostic debug chrome (every cell, padding included).
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        debug_style::render_panel(
            area,
            buf,
            self.top_offset,
            PANEL_WIDTH,
            &["fps debug  (/debug fps)", self.body.as_str()],
        );
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Modifier, Style};
    #[test]
    fn disabled_by_default_and_toggle_round_trips() {
        let mut hud = FpsHud::with_env(None);
        assert!(!hud.enabled());
        assert_eq!(hud.overlay_height(), 0);
        assert!(hud.overlay(0).is_none());
        hud.toggle();
        assert!(hud.enabled());
        assert_eq!(hud.overlay_height(), 2);
        assert!(hud.overlay(0).is_some());
        hud.toggle();
        assert!(!hud.enabled());
        assert!(hud.overlay(0).is_none());
    }
    /// Default test builds compile without dev instrumentation — release-shaped
    /// for this gate — so a truthy env must construct enabled. A
    /// debug/dev test build hands the env to `FrameMetrics` instead;
    /// asserting against [`HONORS_ECHO_AGENT_FPS_ENV`] keeps the test true under
    /// both cfgs (the dev half is pinned by the constant's shape, the same
    /// limitation as the `/debug` visibility test).
    #[test]
    fn echo_agent_fps_env_enables_hud_where_dev_overlay_absent() {
        for truthy in ["1", "full", " "] {
            assert_eq!(
                FpsHud::with_env(Some(truthy.into())).enabled(),
                HONORS_ECHO_AGENT_FPS_ENV,
                "ECHO_AGENT_FPS={truthy:?} must track the env-gate owner"
            );
        }
        for falsy in [None, Some(String::new()), Some("0".into())] {
            assert!(!FpsHud::with_env(falsy).enabled());
        }
    }
    #[test]
    fn record_caps_ring_buffer_and_toggle_clears_stale_samples() {
        let mut hud = FpsHud::with_env(None);
        hud.toggle();
        for _ in 0..SAMPLE_CAP + 30 {
            hud.record(Duration::from_millis(10));
        }
        assert_eq!(hud.samples.len(), SAMPLE_CAP);
        hud.toggle();
        hud.toggle();
        assert!(hud.samples.is_empty());
        assert!(
            hud.overlay(0).unwrap().body.contains("fps:-"),
            "fresh enablement must show placeholders"
        );
    }
    #[test]
    fn stats_line_reports_mean_fps_and_percentiles() {
        let mut hud = FpsHud::with_env(None);
        hud.toggle();
        for _ in 0..100 {
            hud.record(Duration::from_millis(10));
        }
        let overlay = hud.overlay(0).expect("enabled");
        assert_eq!(overlay.body, "fps:100 p50:10.0ms p95:10.0ms");
    }
    #[test]
    fn record_is_a_noop_while_disabled() {
        let mut hud = FpsHud::with_env(None);
        hud.record(Duration::from_millis(10));
        assert!(hud.samples.is_empty());
    }
    /// Every cell of the panel rect — trailing padding included — must carry
    /// the explicit debug chrome, not the theme style underneath.
    #[test]
    fn render_paints_theme_agnostic_style_over_every_panel_cell() {
        let area = Rect::new(0, 0, 60, 6);
        let mut buf = Buffer::empty(area);
        let theme = Style::default()
            .fg(Color::Rgb(228, 228, 228))
            .bg(Color::Rgb(3, 3, 4))
            .add_modifier(Modifier::ITALIC);
        buf.set_style(area, theme);
        let overlay = FpsOverlay {
            body: "fps:100 p50:10.0ms p95:10.0ms".to_string(),
            top_offset: 1,
        };
        overlay.render(area, &mut buf);
        let x0 = area.width - PANEL_WIDTH;
        for y in 1..3u16 {
            for x in x0..area.width {
                let cell = &buf[(x, y)];
                assert_eq!(cell.bg, Color::Black, "cell ({x},{y}) bg");
                assert!(
                    cell.fg == Color::White || cell.fg == Color::Yellow,
                    "cell ({x},{y}) fg must be debug chrome, got {:?}",
                    cell.fg
                );
                assert_eq!(
                    cell.modifier,
                    Modifier::empty(),
                    "cell ({x},{y}) must shed themed modifiers"
                );
            }
        }
        assert_eq!(buf[(0, 1)].bg, Color::Rgb(3, 3, 4));
        assert_eq!(buf[(area.width - 1, 0)].bg, Color::Rgb(3, 3, 4));
    }
}
