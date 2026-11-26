//! Shortcuts bar — renders keyboard hints.
//!
//! Accepts `&[HintItem]` from any source — action registry, prompt widget,
//! scrollback state, etc. Each view builds its own hints dynamically.
//!
//! When a `PendingAction` is active (double-press confirmation),
//! the bar replaces all hints with "press again to {label}".

use std::borrow::Cow;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthStr;

use crate::input::key::KeyShortcut;
use crate::theme::Theme;

/// A single hint for the shortcuts bar.
///
/// Carries semantic key data — the bar handles rendering.
/// Views build these dynamically from the registry, widget keymaps, or local state.
#[derive(Debug, Clone)]
pub struct HintItem {
    /// Keys to display. Multiple keys are shown joined with "/" (e.g., j/k).
    pub keys: Vec<KeyShortcut>,
    /// Short label for the bottom bar (e.g., "send", "nav", "cancel").
    pub label: Cow<'static, str>,
    /// Optional custom key text for single-key bar paint (and cheatsheet).
    /// Multi-key bar paint (`keys.len() > 1`) ignores this and builds from
    /// structured keys (shared-mod compact or full-form join).
    pub custom_display: Option<&'static str>,
    /// Longer description for the all-shortcuts cheatsheet (e.g.,
    /// "Send prompt to agent"). When `None`, falls back to `label`.
    pub description: Option<Cow<'static, str>>,
    /// When true, the hint survives compact-mode truncation — it is always
    /// rendered regardless of `max_visible`. Use for hints that should be
    /// discoverable in every scrollback context (e.g. nav, turn, mode).
    pub pinned: bool,
}

impl HintItem {
    /// Single-key hint.
    pub fn new(key: KeyShortcut, label: impl Into<Cow<'static, str>>) -> Self {
        Self {
            keys: vec![key],
            label: label.into(),
            custom_display: None,
            description: None,
            pinned: false,
        }
    }

    /// Paired-key hint (e.g., j/k for nav, h/l for turn).
    pub fn paired(a: KeyShortcut, b: KeyShortcut, label: impl Into<Cow<'static, str>>) -> Self {
        Self {
            keys: vec![a, b],
            label: label.into(),
            custom_display: None,
            description: None,
            pinned: false,
        }
    }

    /// Mark this hint as pinned — it will always be shown in the compact
    /// shortcuts bar, even when the hint list exceeds `max_visible`.
    pub fn pinned(mut self) -> Self {
        self.pinned = true;
        self
    }

    fn bar_key_display(&self) -> String {
        bar_key_segments(self)
            .into_iter()
            .map(|seg| seg.text)
            .collect()
    }
}

struct BarKeySeg {
    text: String,
    is_join: bool,
}

fn bar_key_segments(hint: &HintItem) -> Vec<BarKeySeg> {
    if hint.keys.len() <= 1 {
        let text = if let Some(d) = hint.custom_display {
            d.to_string()
        } else if let Some(k) = hint.keys.first() {
            k.display()
        } else {
            String::new()
        };
        if text.is_empty() {
            return Vec::new();
        }
        return vec![BarKeySeg {
            text,
            is_join: false,
        }];
    }

    let shared = hint
        .keys
        .iter()
        .all(|k| k.modifiers == hint.keys[0].modifiers);
    let mut segs = Vec::new();
    if shared {
        let prefix = hint.keys[0].modifiers_prefix();
        if !prefix.is_empty() {
            segs.push(BarKeySeg {
                text: prefix,
                is_join: false,
            });
        }
        for (i, k) in hint.keys.iter().enumerate() {
            if i > 0 {
                segs.push(BarKeySeg {
                    text: "/".into(),
                    is_join: true,
                });
            }
            segs.push(BarKeySeg {
                text: k.code_display(),
                is_join: false,
            });
        }
    } else {
        for (i, k) in hint.keys.iter().enumerate() {
            if i > 0 {
                segs.push(BarKeySeg {
                    text: "/".into(),
                    is_join: true,
                });
            }
            segs.push(BarKeySeg {
                text: k.display(),
                is_join: false,
            });
        }
    }
    segs
}

/// Shortcuts bar widget. Renders a list of `HintItem`s.
pub struct ShortcutsBar<'a> {
    hints: &'a [HintItem],
    /// If set, replaces all hints with "press again to {label}".
    pending_confirmation: Option<PendingHint>,
    /// Right-aligned text (e.g. team name).
    right_text: Option<&'a str>,
    /// Compact mode config: render only the first `max_visible` hints from
    /// `hints`, then always append `help_hint` (e.g. the "all shortcuts"
    /// modal trigger). When None, all hints are rendered.
    compact: Option<CompactConfig>,
}

/// Compact-mode configuration for the shortcuts bar.
pub struct CompactConfig {
    /// Maximum number of items to render from the hint list before the
    /// trailing help hint.
    pub max_visible: usize,
    /// The trailing help hint (typically the binding for the all-shortcuts
    /// modal). Always rendered when set, even if the hint list is empty.
    pub help_hint: Option<HintItem>,
}

/// Info needed to render the "press again" hint.
#[derive(Clone, Copy)]
pub struct PendingHint {
    pub shortcut: KeyShortcut,
    pub label: &'static str,
}

impl<'a> ShortcutsBar<'a> {
    /// Create from a pre-built list of hints.
    pub fn new(hints: &'a [HintItem]) -> Self {
        Self {
            hints,
            pending_confirmation: None,
            right_text: None,
            compact: None,
        }
    }

    /// Render only the first `max_visible` hints, then append `help_hint`
    /// (typically the binding that opens the all-shortcuts modal).
    pub fn compact(mut self, max_visible: usize, help_hint: Option<HintItem>) -> Self {
        self.compact = Some(CompactConfig {
            max_visible,
            help_hint,
        });
        self
    }

    /// Set the pending confirmation hint (replaces all normal hints).
    pub fn with_pending(mut self, pending: Option<PendingHint>) -> Self {
        self.pending_confirmation = pending;
        self
    }

    /// Set right-aligned text (e.g. team name).
    pub fn with_right_text(mut self, text: Option<&'a str>) -> Self {
        self.right_text = text;
        self
    }
}

impl Widget for ShortcutsBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            return;
        }

        let theme = Theme::current();

        let bg_style = Style::default()
            .bg(theme.bg_base)
            .fg(theme.gray)
            .remove_modifier(Modifier::all());
        // Clear area content and style — set_style only patches style, leaving
        // old text from previous renders. Fill with spaces to clear.
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell_mut((x, area.y)) {
                cell.reset();
                cell.set_style(bg_style);
            }
        }

        let key_style = Style::default()
            .fg(theme.text_secondary)
            .bg(theme.bg_base)
            .add_modifier(Modifier::BOLD);

        let action_style = Style::default()
            .fg(theme.gray)
            .bg(theme.bg_base)
            .remove_modifier(Modifier::BOLD | Modifier::DIM);

        // If pending confirmation, show only "press again to {label}"
        if let Some(pending) = &self.pending_confirmation {
            let key_text = pending.shortcut.display();
            let label = format!("press again to {}", pending.label);

            let mut x = area.x;

            let key_span = Span::styled(&key_text, key_style);
            let key_width = key_text.width() as u16;
            buf.set_span(x, area.y, &key_span, key_width);
            x += key_width;

            let colon = Span::styled(":", action_style);
            buf.set_span(x, area.y, &colon, 1);
            x += 1;

            let action_span = Span::styled(&label, action_style);
            let action_width = label.width() as u16;
            buf.set_span(x, area.y, &action_span, action_width);
            let _ = x + action_width; // suppress unused
            return;
        }

        let sep_style = Style::default()
            .fg(theme.gray)
            .bg(theme.bg_base)
            .add_modifier(Modifier::DIM)
            .remove_modifier(Modifier::BOLD);

        let mut x = area.x;

        // Build the effective hint list (compact-aware).
        let effective = compute_effective_hints(self.hints, self.compact.as_ref());

        for (i, hint) in effective.iter().enumerate() {
            if i > 0 {
                let sep = Span::styled("  │  ", sep_style);
                let sep_width = 5u16;
                if x + sep_width > area.x + area.width {
                    break;
                }
                buf.set_span(x, area.y, &sep, sep_width);
                x += sep_width;
            }

            let key_width = hint.bar_key_display().width() as u16;
            if x + key_width > area.x + area.width {
                break;
            }
            x = paint_hint_keys(buf, x, area.y, hint, key_style, action_style);

            let colon = Span::styled(":", action_style);
            if x + 1 > area.x + area.width {
                break;
            }
            buf.set_span(x, area.y, &colon, 1);
            x += 1;

            let action_span = Span::styled(hint.label.as_ref(), action_style);
            let action_width = hint.label.width() as u16;
            if x + action_width > area.x + area.width {
                break;
            }
            buf.set_span(x, area.y, &action_span, action_width);
            x += action_width;
        }

        // Right-aligned text (team name etc.)
        if let Some(text) = self.right_text {
            let right_style = Style::default().fg(theme.gray).bg(theme.bg_base);
            let display = format!("{text} ");
            let rw = display.width() as u16;
            if rw > 0 && rw < area.width {
                let rx = area.x + area.width.saturating_sub(rw);
                if rx > x + 1 {
                    let right_span = Span::styled(display, right_style);
                    buf.set_span(rx, area.y, &right_span, rw);
                }
            }
        }
    }
}

fn paint_key_run(buf: &mut Buffer, x: u16, y: u16, text: &str, style: Style) -> u16 {
    if text.is_empty() {
        return x;
    }
    let w = text.width() as u16;
    let span = Span::styled(text, style);
    buf.set_span(x, y, &span, w);
    x + w
}

fn paint_hint_keys(
    buf: &mut Buffer,
    mut x: u16,
    y: u16,
    hint: &HintItem,
    key_style: Style,
    sep_style: Style,
) -> u16 {
    for seg in bar_key_segments(hint) {
        let style = if seg.is_join { sep_style } else { key_style };
        x = paint_key_run(buf, x, y, &seg.text, style);
    }
    x
}

/// Compute the hint list the bar will actually render.
///
/// Without `compact`: returns every hint from the input slice.
/// With `compact`: pinned hints are always included; the remaining
/// `max_visible − pinned_count` slots are filled with unpinned hints in
/// their original order. The trailing `help_hint` is unconditionally
/// appended so users always see how to discover the rest.
pub fn compute_effective_hints<'a>(
    hints: &'a [HintItem],
    compact: Option<&'a CompactConfig>,
) -> Vec<&'a HintItem> {
    if let Some(cfg) = compact {
        let pinned_count = hints.iter().filter(|h| h.pinned).count();
        let unpinned_budget = cfg.max_visible.saturating_sub(pinned_count);
        let mut unpinned_used = 0;
        let mut v: Vec<&HintItem> = hints
            .iter()
            .filter(|h| {
                if h.pinned {
                    true
                } else if unpinned_used < unpinned_budget {
                    unpinned_used += 1;
                    true
                } else {
                    false
                }
            })
            .collect();
        if let Some(ref h) = cfg.help_hint {
            v.push(h);
        }
        v
    } else {
        hints.iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key;

    fn h(label: &'static str, k: crate::input::key::KeyShortcut) -> HintItem {
        HintItem::new(k, label)
    }

    #[test]
    fn full_mode_returns_all_hints() {
        let hints = vec![h("a", key!('a')), h("b", key!('b')), h("c", key!('c'))];
        let out = compute_effective_hints(&hints, None);
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn compact_takes_first_n_then_appends_help() {
        let hints = vec![h("a", key!('a')), h("b", key!('b')), h("c", key!('c'))];
        let help = h("shortcuts", key!('/', CONTROL));
        let cfg = CompactConfig {
            max_visible: 2,
            help_hint: Some(help),
        };
        let out = compute_effective_hints(&hints, Some(&cfg));
        assert_eq!(out.len(), 3); // 2 + help
        assert_eq!(out[0].label, "a");
        assert_eq!(out[1].label, "b");
        assert_eq!(out[2].label, "shortcuts");
    }

    #[test]
    fn compact_help_hint_renders_even_with_empty_hint_list() {
        let hints: Vec<HintItem> = vec![];
        let help = h("shortcuts", key!('/', CONTROL));
        let cfg = CompactConfig {
            max_visible: 2,
            help_hint: Some(help),
        };
        let out = compute_effective_hints(&hints, Some(&cfg));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].label, "shortcuts");
    }

    #[test]
    fn compact_without_help_just_truncates() {
        let hints = vec![h("a", key!('a')), h("b", key!('b')), h("c", key!('c'))];
        let cfg = CompactConfig {
            max_visible: 2,
            help_hint: None,
        };
        let out = compute_effective_hints(&hints, Some(&cfg));
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn compact_max_visible_larger_than_input_is_safe() {
        let hints = vec![h("a", key!('a'))];
        let cfg = CompactConfig {
            max_visible: 10,
            help_hint: None,
        };
        let out = compute_effective_hints(&hints, Some(&cfg));
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn compact_pinned_hints_always_included() {
        // 5 hints: a, b, c are unpinned; d, e are pinned.
        // max_visible=3 → budget for unpinned = 3-2 = 1.
        // Result: a (unpinned slot 1), d (pinned), e (pinned) = 3 items.
        let hints = vec![
            h("a", key!('a')),
            h("b", key!('b')),
            h("c", key!('c')),
            h("d", key!('d')).pinned(),
            h("e", key!('e')).pinned(),
        ];
        let cfg = CompactConfig {
            max_visible: 3,
            help_hint: None,
        };
        let out = compute_effective_hints(&hints, Some(&cfg));
        let labels: Vec<&str> = out.iter().map(|h| h.label.as_ref()).collect();
        assert_eq!(labels, vec!["a", "d", "e"]);
    }

    #[test]
    fn compact_pinned_preserves_original_order() {
        // Pinned hint appears between unpinned ones — order is preserved.
        let hints = vec![
            h("a", key!('a')),
            h("nav", key!('j')).pinned(),
            h("b", key!('b')),
            h("c", key!('c')),
        ];
        let cfg = CompactConfig {
            max_visible: 3,
            help_hint: None,
        };
        let out = compute_effective_hints(&hints, Some(&cfg));
        let labels: Vec<&str> = out.iter().map(|h| h.label.as_ref()).collect();
        // 1 pinned + budget 2 unpinned: a, nav, b
        assert_eq!(labels, vec!["a", "nav", "b"]);
    }

    #[test]
    fn compact_all_pinned_exceeding_max_visible() {
        // More pinned hints than max_visible — all pinned still shown.
        let hints = vec![
            h("a", key!('a')).pinned(),
            h("b", key!('b')).pinned(),
            h("c", key!('c')).pinned(),
            h("d", key!('d')),
        ];
        let cfg = CompactConfig {
            max_visible: 2,
            help_hint: None,
        };
        let out = compute_effective_hints(&hints, Some(&cfg));
        let labels: Vec<&str> = out.iter().map(|h| h.label.as_ref()).collect();
        // All 3 pinned, 0 budget for unpinned.
        assert_eq!(labels, vec!["a", "b", "c"]);
    }

    fn render_hints(hints: &[HintItem]) -> Buffer {
        use ratatui::layout::Rect;
        use ratatui::widgets::Widget;

        let area = Rect::new(0, 0, 48, 1);
        let mut buf = Buffer::empty(area);
        ShortcutsBar::new(hints).render(area, &mut buf);
        buf
    }

    fn leading_text(buf: &Buffer, n: usize) -> String {
        let mut text = String::new();
        for col in 0..n {
            text.push_str(buf.cell((col as u16, 0)).expect("cell").symbol());
        }
        text
    }

    #[test]
    fn multi_key_shared_mod_compact_join_slash_uses_label_style() {
        use ratatui::style::Modifier;

        let theme = Theme::current();
        let key_fg = theme.text_secondary;
        let action_fg = theme.gray;

        let hints = [HintItem::paired(
            key!('[', CONTROL),
            key!(']', CONTROL),
            "prev/next agent",
        )];
        let buf = render_hints(&hints);

        assert!(
            leading_text(&buf, 9).starts_with("Ctrl+[/]:"),
            "expected compact Ctrl+[/]:, got {:?}",
            leading_text(&buf, 12)
        );

        // "Ctrl+[/]:prev/next agent" — join slash at col 6
        let slash = buf.cell((6, 0)).expect("slash cell");
        assert_eq!(slash.symbol(), "/");
        assert_eq!(
            slash.style().fg,
            Some(action_fg),
            "join slash must match label gray, got {:?}",
            slash.style().fg
        );
        assert!(
            !slash.style().add_modifier.contains(Modifier::BOLD),
            "join slash must not be bold key style"
        );

        let open = buf.cell((5, 0)).expect("open bracket");
        assert_eq!(open.symbol(), "[");
        assert_eq!(open.style().fg, Some(key_fg));
        assert!(open.style().add_modifier.contains(Modifier::BOLD));

        let close = buf.cell((7, 0)).expect("close bracket");
        assert_eq!(close.symbol(), "]");
        assert_eq!(close.style().fg, Some(key_fg));
        assert!(close.style().add_modifier.contains(Modifier::BOLD));

        // "Ctrl+[/]:" = 9 cols, then "prev" = 4, "/" at col 13.
        let label_slash = buf.cell((13, 0)).expect("label slash cell");
        assert_eq!(label_slash.symbol(), "/");
        assert_eq!(label_slash.style().fg, Some(action_fg));
    }

    #[test]
    fn multi_key_ignores_custom_display() {
        use ratatui::style::Modifier;

        let theme = Theme::current();
        let action_fg = theme.gray;

        let mut item = HintItem::paired(key!('[', CONTROL), key!(']', CONTROL), "cycle");
        item.custom_display = Some("WRONG");
        let buf = render_hints(&[item]);

        let text = leading_text(&buf, 12);
        assert!(
            text.starts_with("Ctrl+[/]:"),
            "multi-key must ignore custom_display, got {text:?}"
        );
        assert!(
            !text.contains("WRONG"),
            "custom_display must not appear for multi-key, got {text:?}"
        );

        let join = buf.cell((6, 0)).expect("join");
        assert_eq!(join.symbol(), "/");
        assert_eq!(join.style().fg, Some(action_fg));
        assert!(!join.style().add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn custom_display_with_slash_is_fully_key_styled() {
        use ratatui::style::Modifier;

        let theme = Theme::current();
        let key_fg = theme.text_secondary;

        let mut item = HintItem::new(key!(Null), "nav");
        item.custom_display = Some("\u{2191}/\u{2193}");
        let buf = render_hints(&[item]);

        // "↑/↓:nav" — all three key cells must be key-styled, including `/`.
        for col in 0..3 {
            let cell = buf.cell((col, 0)).expect("key cell");
            assert_eq!(
                cell.style().fg,
                Some(key_fg),
                "col {col} ({}) must be key-styled",
                cell.symbol()
            );
            assert!(cell.style().add_modifier.contains(Modifier::BOLD));
        }
        assert_eq!(buf.cell((0, 0)).expect("up").symbol(), "\u{2191}");
        assert_eq!(buf.cell((1, 0)).expect("slash").symbol(), "/");
        assert_eq!(buf.cell((2, 0)).expect("down").symbol(), "\u{2193}");
        assert_eq!(buf.cell((3, 0)).expect("colon").symbol(), ":");
    }

    #[test]
    fn bare_slash_key_is_fully_key_styled() {
        use ratatui::style::Modifier;

        let theme = Theme::current();
        let key_fg = theme.text_secondary;

        let hints = [HintItem::new(key!('/'), "search")];
        let buf = render_hints(&hints);

        let slash = buf.cell((0, 0)).expect("slash cell");
        assert_eq!(slash.symbol(), "/");
        assert_eq!(slash.style().fg, Some(key_fg));
        assert!(
            slash.style().add_modifier.contains(Modifier::BOLD),
            "bare / must be key-styled, not a join separator"
        );
    }

    #[test]
    fn ctrl_slash_key_is_fully_key_styled() {
        use ratatui::style::Modifier;

        let theme = Theme::current();
        let key_fg = theme.text_secondary;

        let hints = [HintItem::new(key!('/', CONTROL), "search")];
        let buf = render_hints(&hints);

        // "Ctrl+/" — every cell key-styled; the trailing / is not a join.
        for col in 0..6 {
            let cell = buf.cell((col, 0)).expect("key cell");
            assert_eq!(
                cell.style().fg,
                Some(key_fg),
                "col {col} ({}) must be key-styled",
                cell.symbol()
            );
            assert!(cell.style().add_modifier.contains(Modifier::BOLD));
        }
        assert_eq!(buf.cell((5, 0)).expect("slash").symbol(), "/");
        assert_eq!(buf.cell((6, 0)).expect("colon").symbol(), ":");
    }

    #[test]
    fn multi_key_different_mods_uses_full_forms() {
        use ratatui::style::Modifier;

        let theme = Theme::current();
        let key_fg = theme.text_secondary;
        let action_fg = theme.gray;

        let hints = [HintItem::paired(key!(' ', CONTROL), key!(F(8)), "toggle")];
        let buf = render_hints(&hints);

        // "Ctrl+Space/F8:toggle"
        //  0123456789...
        // join / at col 10
        let mut text = String::new();
        for col in 0..20 {
            text.push_str(buf.cell((col, 0)).expect("cell").symbol());
        }
        assert!(
            text.starts_with("Ctrl+Space/F8:"),
            "expected full forms, got {text:?}"
        );

        let join = buf.cell((10, 0)).expect("join slash");
        assert_eq!(join.symbol(), "/");
        assert_eq!(join.style().fg, Some(action_fg));
        assert!(!join.style().add_modifier.contains(Modifier::BOLD));

        let f = buf.cell((11, 0)).expect("F");
        assert_eq!(f.symbol(), "F");
        assert_eq!(f.style().fg, Some(key_fg));
        assert!(f.style().add_modifier.contains(Modifier::BOLD));
    }
}
