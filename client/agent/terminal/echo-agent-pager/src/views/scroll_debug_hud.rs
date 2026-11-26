//! Scroll-diagnostics HUD — the in-pager "scroll playground".
//!
//! A compact top-right overlay painting a per-frame snapshot of the scroll
//! state machine ([`MouseScrollState::debug_snapshot`]) plus the active
//! scrollback's viewport facts, inside a REAL session with the REAL event
//! loop. Recipe: `ECHO_AGENT_FPS=1 ECHO_AGENT_SCROLL_DEBUG=1 echoagent --resume <session>`,
//! then flip `scroll_mode` / `scroll_lines` / `invert_scroll` /
//! `scroll_speed` in `/settings` to compare variants live. For event-exact
//! capture beyond this per-frame sampling, add `ECHO_AGENT_SCROLL_LOG=1` — the
//! JSONL flight recorder ([`crate::input::scroll_log`]).
//!
//! Invariant: the HUD must never affect scroll behavior. The snapshot is
//! read-only (`&self`, caller-supplied `now`), taken in the draw path after
//! all input/tick state updates for the frame, and rendering only paints
//! buffer cells. Disabled cost is a single bool check per frame.
//!
//! Unlike the FPS overlay (`render::frame_metrics`, debug/dev builds only), this
//! compiles into release builds behind its runtime gate (the hidden-command
//! precedent, e.g. `/gboom`): dev instrumentation alters the frame pipeline
//! (phase timings through `draw_frame`), so a dev-only HUD could not probe
//! the production render path — defeating the zero-fidelity-gap goal — and
//! the pty e2e suite runs against production-featured binaries.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::input::mouse::ScrollDebugSnapshot;

/// Panel width in cells; each line is padded/truncated to this.
const PANEL_WIDTH: u16 = 46;

/// Runtime enablement for the HUD. Mirrors `FrameMetrics`' env machinery:
/// `ECHO_AGENT_SCROLL_DEBUG` (nonempty and not `"0"`) enables at startup, and the
/// hidden `/scroll-debug` command toggles it live. Deliberately NOT a
/// settings-registry entry: it is a diagnostic, not a preference to persist.
pub struct ScrollDebugHud {
    enabled: bool,
}

impl Default for ScrollDebugHud {
    fn default() -> Self {
        Self::new()
    }
}

impl ScrollDebugHud {
    pub fn new() -> Self {
        let env_on =
            std::env::var("ECHO_AGENT_SCROLL_DEBUG").is_ok_and(|v| !v.is_empty() && v != "0");
        Self { enabled: env_on }
    }

    /// Whether the HUD is currently enabled.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// `/scroll-debug` runtime toggle.
    pub fn toggle(&mut self) {
        self.enabled = !self.enabled;
    }
}

/// Scrollback-side facts for the `view:` row (`None` off agent views).
#[derive(Clone, Copy, Debug)]
pub struct ViewportDebug {
    pub scroll_offset: usize,
    pub max_offset: usize,
    pub total_height: usize,
    pub follow_mode: bool,
    pub at_bottom: bool,
}

/// Owned per-frame render params, assembled by `AppView::draw` BEFORE the
/// frame closure (after all scroll-state updates; borrow-splitting keeps the
/// closure free of `self.scroll_state`).
pub struct ScrollDebugPanel {
    pub snapshot: ScrollDebugSnapshot,
    pub view: Option<ViewportDebug>,
    /// Rows left free for FPS overlays stacked above (the dev `ECHO_AGENT_FPS`
    /// line and/or the release-safe `/debug fps` HUD).
    pub top_offset: u16,
}

impl ScrollDebugPanel {
    /// Paint the panel in the top-right corner of `area`. Per-frame
    /// formatting is fine for a debug tool; nothing here outlives the frame.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        let s = &self.snapshot;
        let yn = |b: bool| if b { "y" } else { "n" };
        let ctx = crate::terminal::terminal_context();

        let mut lines: Vec<String> = Vec::with_capacity(10);
        lines.push("scroll debug  (/scroll-debug)".to_string());
        lines.push(format!("term:{} mux:{}", ctx.brand, ctx.multiplexer));
        lines.push(format!(
            "mode:{} inv:{} speed:x{:.2}",
            s.mode.label(),
            yn(s.invert),
            s.speed_multiplier
        ));
        lines.push(format!(
            "ept:{} lpt:{}/{} vp:{} cap:{} cad:{}ms",
            s.events_per_tick,
            s.wheel_lines_per_tick,
            s.trackpad_lines_per_tick,
            s.viewport_height,
            s.flush_cap,
            s.cadence_ms
        ));
        match &s.stream {
            Some(st) => {
                lines.push(format!(
                    "stream:live kind:{}{} ev:{}",
                    st.kind,
                    if st.promoted { "*" } else { "" },
                    st.events
                ));
                lines.push(format!(
                    "avg:{} accel:x{:.2} gap:{}ms",
                    st.avg_interval_ms
                        .map_or_else(|| "-".to_string(), |ms| format!("{ms:.1}ms")),
                    st.accel,
                    st.gap_remaining_ms
                ));
                lines.push(format!(
                    "desired:{:+.1} applied:{:+} backlog:{:+}",
                    st.desired_lines, st.applied_lines, st.backlog
                ));
            }
            None => {
                lines.push("stream:- kind:- ev:-".to_string());
                lines.push("avg:- accel:- gap:-".to_string());
                lines.push("desired:- applied:- backlog:-".to_string());
            }
        }
        lines.push(format!(
            "carry:{:+.2} flush:{}ms clock:{}",
            s.carry_lines,
            s.ms_since_flush,
            s.next_deadline_ms
                .map_or_else(|| "-".to_string(), |ms| format!("{ms}ms")),
        ));
        lines.push(s.last_stream.as_ref().map_or_else(
            || "last:-".to_string(),
            |l| format!("last:{} ev:{} ln:{:+}", l.kind, l.events, l.applied_lines),
        ));
        if let Some(v) = &self.view {
            lines.push(format!(
                "view:{}/{} h:{} follow:{} bot:{}",
                v.scroll_offset,
                v.max_offset,
                v.total_height,
                yn(v.follow_mode),
                yn(v.at_bottom)
            ));
        }

        let line_refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        super::debug_style::render_panel(area, buf, self.top_offset, PANEL_WIDTH, &line_refs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Modifier, Style};

    fn snapshot() -> ScrollDebugSnapshot {
        ScrollDebugSnapshot {
            stream: None,
            last_stream: None,
            carry_lines: 0.0,
            ms_since_flush: 0,
            next_deadline_ms: None,
            mode: crate::input::mouse::ScrollInputMode::Auto,
            events_per_tick: 3,
            wheel_lines_per_tick: 3,
            trackpad_lines_per_tick: 1,
            invert: false,
            speed_multiplier: 1.0,
            viewport_height: 40,
            flush_cap: 120,
            cadence_ms: 16,
        }
    }

    /// The Oscura Midnight regression: the panel must paint the explicit
    /// debug chrome — bg black, white/yellow fg, no inherited modifiers —
    /// on EVERY cell of its rect, trailing padding included, regardless of
    /// the themed cells underneath.
    #[test]
    fn panel_paints_theme_agnostic_style_over_every_cell() {
        let area = Rect::new(0, 0, 60, 14);
        let mut buf = Buffer::empty(area);
        // Mimic a themed frame: near-black RGB bg (Oscura Midnight base is
        // #030304), tinted fg, and a modifier on every cell — everything
        // the overlay must override.
        let theme = Style::default()
            .fg(Color::Rgb(228, 228, 228))
            .bg(Color::Rgb(3, 3, 4))
            .add_modifier(Modifier::ITALIC);
        buf.set_style(area, theme);

        let panel = ScrollDebugPanel {
            snapshot: snapshot(),
            view: Some(ViewportDebug {
                scroll_offset: 5,
                max_offset: 10,
                total_height: 50,
                follow_mode: true,
                at_bottom: false,
            }),
            top_offset: 0,
        };
        panel.render(area, &mut buf);

        // 10 lines with a `view:` row; the panel hugs the right edge.
        let x0 = area.width - PANEL_WIDTH;
        for y in 0..10u16 {
            for x in x0..area.width {
                let cell = &buf[(x, y)];
                assert_eq!(
                    cell.bg,
                    Color::Black,
                    "cell ({x},{y}) bg must be explicit black, got {:?}",
                    cell.bg
                );
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
        // The overlay is rect-scoped: cells outside it keep the theme.
        assert_eq!(buf[(0, 0)].bg, Color::Rgb(3, 3, 4));
        assert_eq!(buf[(x0 - 1, 3)].modifier, Modifier::ITALIC);
    }
}
