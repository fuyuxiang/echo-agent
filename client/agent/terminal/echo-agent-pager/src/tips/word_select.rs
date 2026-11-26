//! Tip after double-clicking scrollback while text-selection mode is fold/nav:
//! advertise Settings → Text selection → Word select.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::EphemeralTip;
use crate::theme::Theme;

/// Ephemeral-tip dedup key for the word-select settings hint.
pub(crate) const WORD_SELECT_TIP_KEY: &str = "word_select_tip";

/// Key into the per-session in-memory seen-count map for this tip.
pub(crate) const WORD_SELECT_TIP_SEEN_KEY: &str = "word_select_tip_shown_count";

/// Stop showing after this many shows within a single session.
const WORD_SELECT_TIP_SEEN_CAP: u32 = 3;

/// Tip lifetime: ~20s at the 30fps animation cadence (vs the ~3s default).
/// This tip is a call to action (read → decide → press the chord), not a
/// glanceable notice, so it gets a much longer window. Ambient + the
/// retire-on-prompt-edit hook bound the window: it pauses while occluded and
/// dies the moment the user starts doing something else.
pub(crate) const WORD_SELECT_TIP_TICKS: u16 = 600;

/// The accept chord advertised by the tip: pressing it while the tip is on
/// screen flips `keep_text_selection` to `word_select` (see
/// `Action::AcceptWordSelectTip`). Tip-scoped — outside the tip's TTL the
/// chord keeps its normal meaning (prompt yank), and any prompt edit retires
/// the tip so the long TTL cannot shadow a kill→yank sequence.
pub(crate) const WORD_SELECT_ACCEPT_CHORD: &str = "Ctrl+Y";

/// Build "Want double-click to select? /settings → Text selection · Ctrl+Y:
/// enable now", seen-gated to [`WORD_SELECT_TIP_SEEN_CAP`] shows per session
/// (in-memory).
///
/// Fires when double-click runs the fold/nav path (default `flash` / `hold`)
/// so users who expected terminal-like word highlight learn about the setting
/// — or flip it on the spot with the advertised chord.
pub fn word_select_tip() -> EphemeralTip {
    let theme = Theme::current();
    let dim = Style::default().fg(theme.gray);
    let key_style = Style::default()
        .fg(theme.text_secondary)
        .add_modifier(Modifier::BOLD);
    EphemeralTip {
        ticks_remaining: WORD_SELECT_TIP_TICKS,
        ..EphemeralTip::new(
            WORD_SELECT_TIP_KEY,
            Line::from(vec![
                Span::styled("Want double-click to select? ", dim),
                Span::styled("/settings", key_style),
                Span::styled(" → Text selection · ", dim),
                Span::styled(WORD_SELECT_ACCEPT_CHORD, key_style),
                Span::styled(": enable now", dim),
            ]),
        )
        .with_session_seen_cap(WORD_SELECT_TIP_SEEN_KEY, WORD_SELECT_TIP_SEEN_CAP)
        // Ambient: not about the draft being edited — an unrelated submit
        // keeps it, and occlusion (permission ask, modal) pauses the TTL
        // instead of burning the decision window off-screen.
        .ambient()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_select_tip_builder_applies_seen_gating() {
        assert_eq!(
            word_select_tip().session_seen.map(|(key, _cap)| key),
            Some(WORD_SELECT_TIP_SEEN_KEY)
        );
        assert_eq!(
            word_select_tip().session_seen.map(|(_, cap)| cap),
            Some(WORD_SELECT_TIP_SEEN_CAP)
        );
    }

    /// The CTA window: long TTL + ambient (pauses while occluded, survives an
    /// unrelated submit). Retire-on-prompt-edit bounds it — see the
    /// `PromptEvent::Edited` hook in `agent_view/prompt.rs`.
    #[test]
    fn word_select_tip_has_long_ambient_window() {
        let tip = word_select_tip();
        assert_eq!(tip.ticks_remaining, WORD_SELECT_TIP_TICKS);
        assert!(
            tip.ticks_remaining > super::super::DEFAULT_TIP_TICKS,
            "CTA tip must outlive the glanceable default"
        );
        assert!(tip.ambient, "occlusion must pause, not burn, the window");
    }

    #[test]
    fn word_select_tip_advertises_settings_and_accept_chord() {
        let tip = word_select_tip();
        let text: String = tip.line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.contains("double-click to select")
                && text.contains("/settings")
                && text.contains("Text selection")
                && text.contains(WORD_SELECT_ACCEPT_CHORD),
            "expected settings path + accept chord copy, got {text:?}"
        );
    }
}
