//! Timeline sidebar: a tick rail (one tick per turn) that replaces the
//! scrollbar in its gutter while enabled. Tick position encodes conversation
//! order, not scroll proportion.
//!
//! Geometry is computed once per frame into a [`TimelineRail`] consumed by
//! both the renderer and mouse hit-testing, so they cannot drift.

use std::ops::Range;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::theme::Theme;

/// Columns reserved for the rail (widest tick).
pub const RAIL_WIDTH: u16 = 2;

/// Terminals narrower than this hide the rail (the transcript needs the
/// columns more than the navigator).
pub const MIN_TERMINAL_WIDTH: u16 = 60;

/// Minimum turns before the rail appears (a 1-turn timeline is noise).
pub const MIN_TURNS: usize = 2;

/// Per-frame rail geometry: where the ticks and chevrons landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineRail {
    /// Full rail rect (hit target), spanning the scrollback rows.
    pub rect: Rect,
    /// Turn indices currently shown as ticks (windowed around the active
    /// turn when the conversation has more turns than rows).
    pub window: Range<usize>,
    /// First tick row.
    pub ticks_y: u16,
    /// Active turn (viewport top), if any.
    pub active: Option<usize>,
    /// The ▲ target: nearest turn strictly above the viewport top
    /// ([`ScrollbackState::turn_above_viewport_top`]), NOT `active - 1` —
    /// stepping from `active` could target trailing turns that no scroll
    /// can bring to the top (stuck ▲).
    ///
    /// [`ScrollbackState::turn_above_viewport_top`]:
    /// crate::scrollback::ScrollbackState::turn_above_viewport_top
    pub up_target: Option<usize>,
    /// The ▼ target: nearest turn below the viewport top
    /// ([`ScrollbackState::turn_below_viewport_top`]), so ▼ anchors it to the
    /// top exactly like clicking its tick (both go through `jump_to_turn`,
    /// which over-scrolls trailing turns rather than dimming). `None` only
    /// when the last turn already owns the top.
    ///
    /// [`ScrollbackState::turn_below_viewport_top`]:
    /// crate::scrollback::ScrollbackState::turn_below_viewport_top
    pub down_target: Option<usize>,
    /// Chevron rows.
    pub up_y: u16,
    pub down_y: u16,
}

/// What part of the rail a screen position lands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineHit {
    /// A turn tick (turn index).
    Tick(usize),
    /// The ▲ chevron (previous turn).
    Up,
    /// The ▼ chevron (next turn).
    Down,
}

/// Columns to reserve for the rail this frame — the single eligibility
/// policy (setting, view kind, terminal width, turn count). Geometry
/// feasibility (enough rows) stays in [`compute_rail`].
pub fn rail_width(
    show_timeline: bool,
    is_subagent_view: bool,
    area_width: u16,
    turn_count: usize,
) -> u16 {
    if show_timeline
        && !is_subagent_view
        && area_width >= MIN_TERMINAL_WIDTH
        && turn_count >= MIN_TURNS
    {
        RAIL_WIDTH
    } else {
        0
    }
}

/// The viewport-derived turn state the rail is built from, gathered once
/// per frame from `ScrollbackState`. Bundled so [`compute_rail`] takes one
/// argument instead of four adjacent `Option<usize>` / `bool` positionals.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RailViewport {
    /// Turn at the viewport top (the highlighted tick), if any.
    pub active: Option<usize>,
    /// ▲ target: nearest turn strictly above the viewport top.
    pub up_target: Option<usize>,
    /// ▼ target: nearest turn below the viewport top.
    pub down_target: Option<usize>,
    /// Viewport is scrolled to the bottom — pins the tick window to the tail.
    pub at_bottom: bool,
}

/// Compute rail geometry for this frame, or `None` when the rail should
/// not render (too few turns / no room for chevrons + at least one tick).
pub fn compute_rail(
    scrollback_area: Rect,
    rail_x: u16,
    turn_count: usize,
    vp: RailViewport,
) -> Option<TimelineRail> {
    if turn_count < MIN_TURNS {
        return None;
    }
    let height = scrollback_area.height as usize;
    // Chevrons take 2 rows; require at least 1 tick row.
    let max_ticks = height.checked_sub(2)?;
    if max_ticks == 0 {
        return None;
    }

    let window = if turn_count <= max_ticks {
        0..turn_count
    } else {
        // More turns than rows: slide a window that keeps the active tick
        // visible. At the bottom, prefer the tail so the newest ticks stay
        // on screen — but never exclude the viewport-top (active) turn,
        // or no tick would highlight.
        let tail_start = turn_count - max_ticks;
        let start = if vp.at_bottom {
            match vp.active {
                Some(a) => a.min(tail_start),
                None => tail_start,
            }
        } else {
            vp.active
                .unwrap_or(turn_count - 1)
                .saturating_sub(max_ticks / 2)
                .min(tail_start)
        };
        start..start + max_ticks
    };

    // Center the chevron + tick stack vertically, like the web rail.
    let total_rows = window.len() + 2;
    let top = scrollback_area.y + ((height - total_rows) / 2) as u16;
    let ticks_y = top + 1;
    let down_y = ticks_y + window.len() as u16;

    Some(TimelineRail {
        rect: Rect {
            x: rail_x,
            y: scrollback_area.y,
            width: RAIL_WIDTH,
            height: scrollback_area.height,
        },
        window,
        ticks_y,
        active: vp.active,
        up_target: vp.up_target,
        down_target: vp.down_target,
        up_y: top,
        down_y,
    })
}

/// The turn a rail interaction jumps to, derived from the rail's own
/// fields — the same state that dims the chevrons, so display and action
/// cannot disagree. `None` = end stop (dim chevron, click is a no-op).
///
/// ▼ steps to `down_target` even at the bottom: `jump_to_turn` over-scrolls
/// a trailing turn to the top (identical to clicking its tick), so the
/// chevron matches the click instead of sitting dead.
pub fn chevron_target(rail: &TimelineRail, hit: TimelineHit) -> Option<usize> {
    match hit {
        TimelineHit::Tick(turn_idx) => Some(turn_idx),
        TimelineHit::Up => rail.up_target,
        TimelineHit::Down => rail.down_target,
    }
}

impl TimelineRail {
    /// Hit-test a screen position. The whole rail width is the target.
    pub fn hit(&self, col: u16, row: u16) -> Option<TimelineHit> {
        if !self.rect.contains((col, row).into()) {
            return None;
        }
        if row == self.up_y {
            return Some(TimelineHit::Up);
        }
        if row == self.down_y {
            return Some(TimelineHit::Down);
        }
        if row >= self.ticks_y {
            let rel = (row - self.ticks_y) as usize;
            if rel < self.window.len() {
                return Some(TimelineHit::Tick(self.window.start + rel));
            }
        }
        None
    }
}

/// Render the rail: chevrons + one tick row per windowed turn. The rail
/// draws directly on the scrollback background (no dark track strip — it
/// read as an awkward empty band, especially with few ticks).
pub fn render_rail(
    buf: &mut Buffer,
    rail: &TimelineRail,
    hovered: Option<TimelineHit>,
    theme: &Theme,
) {
    let dim = Style::default().fg(theme.gray_dim);
    let normal = Style::default().fg(theme.gray);
    let bright = Style::default().fg(theme.text_primary);

    // Chevron dim state derives from the same function the click handler
    // uses — a dim chevron is guaranteed to be a no-op.
    let up_enabled = chevron_target(rail, TimelineHit::Up).is_some();
    let down_enabled = chevron_target(rail, TimelineHit::Down).is_some();
    let up_style = if hovered == Some(TimelineHit::Up) && up_enabled {
        bright
    } else if up_enabled {
        normal
    } else {
        dim
    };
    let down_style = if hovered == Some(TimelineHit::Down) && down_enabled {
        bright
    } else if down_enabled {
        normal
    } else {
        dim
    };
    let chevron_x = rail.rect.x + RAIL_WIDTH - 1;
    buf.set_span(
        chevron_x,
        rail.up_y,
        &Span::styled(crate::glyphs::timeline_chevron_up(), up_style),
        1,
    );
    buf.set_span(
        chevron_x,
        rail.down_y,
        &Span::styled(crate::glyphs::timeline_chevron_down(), down_style),
        1,
    );

    for (row, turn_idx) in rail.window.clone().enumerate() {
        let y = rail.ticks_y + row as u16;
        let is_active = rail.active == Some(turn_idx);
        let is_hovered = hovered == Some(TimelineHit::Tick(turn_idx));

        let (text, style) = if is_active {
            (crate::glyphs::timeline_tick_active(), bright)
        } else if is_hovered {
            (crate::glyphs::timeline_tick_hover(), bright)
        } else {
            // Short dim tick in the rightmost cell (precomposed pad + light).
            (" \u{2500}", dim)
        };
        buf.set_span(rail.rect.x, y, &Span::styled(text, style), RAIL_WIDTH);
    }
}

/// Floating preview card for a hovered tick, anchored left of the rail.
///
/// Shrink-to-fit, in the house popup chrome (clear + dark base fill +
/// rounded `Block`, like the pickers and /btw panel). The interior must
/// stay `bg_base`: border glyphs draw mid-cell, so any lighter fill
/// bleeds a half-cell past the border line.
pub fn render_tick_hover_popup(
    buf: &mut Buffer,
    rail: &TimelineRail,
    scrollback_area: Rect,
    turn_idx: usize,
    preview: &str,
    theme: &Theme,
) {
    if !rail.window.contains(&turn_idx) {
        return;
    }
    let tick_y = rail.ticks_y + (turn_idx - rail.window.start) as u16;

    // Wrap to at most 2 lines by display width; ellipsize the last.
    let max_text = ((scrollback_area.width / 2).clamp(16, 32)) as usize;
    let mut lines: Vec<String> = Vec::new();
    let mut rest: &str = preview.trim();
    while !rest.is_empty() && lines.len() < 2 {
        if lines.len() == 1 {
            lines.push(crate::render::line_utils::truncate_str(rest, max_text));
            rest = "";
        } else {
            let end = crate::render::line_utils::byte_offset_at_width(rest, max_text);
            lines.push(rest[..end].to_string());
            rest = rest[end..].trim_start();
        }
    }
    if lines.is_empty() {
        return;
    }

    let text_w = lines
        .iter()
        .map(|l| unicode_width::UnicodeWidthStr::width(l.as_str()))
        .max()
        .unwrap_or(0) as u16;
    let card_w = text_w + 4;
    let card_h = lines.len() as u16 + 2;
    // Too short a terminal to place the card without painting over the
    // panes above/below — skip it.
    if card_h > scrollback_area.height {
        return;
    }
    let card_x = rail
        .rect
        .x
        .saturating_sub(card_w + 1)
        .max(scrollback_area.x);
    // Vertically centered on the tick row, clamped to the scrollback rows.
    let card_y = tick_y
        .saturating_sub(card_h / 2)
        .max(scrollback_area.y)
        .min(
            (scrollback_area.y + scrollback_area.height)
                .saturating_sub(card_h)
                .min(buf.area.height.saturating_sub(card_h)),
        );
    let card_area = Rect::new(card_x, card_y, card_w, card_h);

    let bg = theme.bg_base;
    ratatui::widgets::Widget::render(ratatui::widgets::Clear, card_area, buf);
    buf.set_style(card_area, Style::default().bg(bg));
    let block = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(Style::default().fg(theme.gray).bg(bg));
    let inner = block.inner(card_area);
    ratatui::widgets::Widget::render(block, card_area, buf);

    let text_style = Style::default().fg(theme.text_primary).bg(bg);
    for (i, line) in lines.iter().enumerate() {
        let y = inner.y + i as u16;
        if y >= inner.y + inner.height {
            break;
        }
        buf.set_line(
            inner.x + 1,
            y,
            &Line::from(Span::styled(line.clone(), text_style)),
            text_w,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn area() -> Rect {
        Rect {
            x: 0,
            y: 2,
            width: 80,
            height: 20,
        }
    }

    /// `compute_rail` with adjacent targets for an off-bottom prompt row.
    fn rail(turn_count: usize, active: Option<usize>) -> Option<TimelineRail> {
        let vp = RailViewport {
            active,
            up_target: active.and_then(|a| a.checked_sub(1)),
            down_target: active.and_then(|a| (a + 1 < turn_count).then_some(a + 1)),
            at_bottom: false,
        };
        compute_rail(area(), 76, turn_count, vp)
    }

    #[test]
    fn rail_hidden_below_min_turns_or_tiny_area() {
        assert!(rail(1, Some(0)).is_none());
        let tiny = Rect {
            height: 2,
            ..area()
        };
        let vp = RailViewport {
            active: Some(0),
            down_target: Some(1),
            ..RailViewport::default()
        };
        assert!(compute_rail(tiny, 76, 5, vp).is_none());
    }

    #[test]
    fn small_conversation_shows_all_ticks_centered() {
        let rail = rail(4, Some(1)).unwrap();
        assert_eq!(rail.window, 0..4);
        // 4 ticks + 2 chevrons = 6 rows centered in 20: top = 2 + 7 = 9.
        assert_eq!(rail.up_y, 9);
        assert_eq!(rail.ticks_y, 10);
        assert_eq!(rail.down_y, 14);
    }

    #[test]
    fn overflow_windows_around_active() {
        // 50 turns, 18 tick rows (20 - 2 chevrons).
        let rail = rail(50, Some(25)).unwrap();
        assert_eq!(rail.window.len(), 18);
        assert!(rail.window.contains(&25));
        // Window is roughly centered on the active turn, and tick rows map
        // to window-relative turn indices.
        assert_eq!(rail.window.start, 25 - 9);
        assert_eq!(
            rail.hit(76, rail.ticks_y),
            Some(TimelineHit::Tick(rail.window.start))
        );

        // Active at the end clamps the window to the tail.
        let rail = self::rail(50, Some(49)).unwrap();
        assert_eq!(rail.window, 32..50);

        // No active turn anchors to the newest.
        let rail = self::rail(50, None).unwrap();
        assert_eq!(rail.window, 32..50);

        // At the bottom the window prefers the tail, but still includes the
        // viewport-top (active) turn so a tick stays highlighted.
        let rail = compute_rail(
            area(),
            76,
            50,
            RailViewport {
                active: Some(25),
                up_target: Some(24),
                down_target: Some(26),
                at_bottom: true,
            },
        )
        .unwrap();
        assert_eq!(rail.window, 25..43);
        assert!(rail.window.contains(&25));

        // Active already in the tail → pin to the newest ticks.
        let rail = compute_rail(
            area(),
            76,
            50,
            RailViewport {
                active: Some(40),
                up_target: Some(39),
                down_target: Some(41),
                at_bottom: true,
            },
        )
        .unwrap();
        assert_eq!(rail.window, 32..50);
        assert!(rail.window.contains(&40));
    }

    #[test]
    fn hit_maps_chevrons_and_ticks() {
        let rail = rail(4, Some(1)).unwrap();
        // Outside the rail columns (width 2: cols 76-77).
        assert_eq!(rail.hit(75, rail.ticks_y), None);
        assert_eq!(rail.hit(78, rail.ticks_y), None);
        // Chevrons.
        assert_eq!(rail.hit(77, rail.up_y), Some(TimelineHit::Up));
        assert_eq!(rail.hit(77, rail.down_y), Some(TimelineHit::Down));
        // Ticks map window-relative rows to turn indices.
        assert_eq!(rail.hit(76, rail.ticks_y), Some(TimelineHit::Tick(0)));
        assert_eq!(rail.hit(77, rail.ticks_y + 3), Some(TimelineHit::Tick(3)));
        // Rows between chevrons/ticks and rail edges miss.
        assert_eq!(rail.hit(76, rail.up_y - 1), None);
    }

    #[test]
    fn chevron_targets_follow_the_rail_state() {
        use TimelineHit::{Down, Tick, Up};
        let mid = rail(10, Some(3)).unwrap();
        // Ticks jump to themselves.
        assert_eq!(chevron_target(&mid, Tick(7)), Some(7));
        // Chevrons take the rail's viewport-derived targets verbatim.
        assert_eq!(chevron_target(&mid, Up), Some(2));
        assert_eq!(chevron_target(&mid, Down), Some(4));
        // End stops are no-ops (the dim chevrons).
        assert_eq!(chevron_target(&rail(10, Some(0)).unwrap(), Up), None);
        assert_eq!(chevron_target(&rail(10, Some(9)).unwrap(), Down), None);
        // Pre-turn content focuses the first tick, but Down still enters
        // that first turn rather than skipping to the second.
        let pre = compute_rail(
            area(),
            76,
            10,
            RailViewport {
                active: Some(0),
                down_target: Some(0),
                ..RailViewport::default()
            },
        )
        .unwrap();
        assert_eq!(chevron_target(&pre, Down), Some(0));
        assert_eq!(chevron_target(&pre, Up), None);
        // At the bottom ▼ still steps to the next turn (jump_to_turn
        // over-scrolls it to the top, matching a tick click); ▲ steps up.
        let bottom = compute_rail(
            area(),
            76,
            10,
            RailViewport {
                active: Some(4),
                up_target: Some(3),
                down_target: Some(5),
                at_bottom: true,
            },
        )
        .unwrap();
        assert_eq!(chevron_target(&bottom, Up), Some(3));
        assert_eq!(chevron_target(&bottom, Down), Some(5));
        // ▼ dims only when the last turn already owns the top.
        let last = compute_rail(
            area(),
            76,
            10,
            RailViewport {
                active: Some(9),
                up_target: Some(8),
                down_target: None,
                at_bottom: true,
            },
        )
        .unwrap();
        assert_eq!(chevron_target(&last, Down), None);
    }

    #[test]
    fn rail_width_gates_eligibility() {
        // All conditions met → rail columns reserved.
        assert_eq!(rail_width(true, false, 80, 5), RAIL_WIDTH);
        // Setting off / subagent view / narrow terminal / too few turns.
        assert_eq!(rail_width(false, false, 80, 5), 0);
        assert_eq!(rail_width(true, true, 80, 5), 0);
        assert_eq!(rail_width(true, false, MIN_TERMINAL_WIDTH - 1, 5), 0);
        assert_eq!(rail_width(true, false, 80, 1), 0);
    }
}
