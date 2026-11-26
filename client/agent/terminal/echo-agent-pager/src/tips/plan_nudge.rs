//! Plan-nudge trigger: detects planning keywords typed into the prompt so the
//! pager can hint that Shift+Tab cycles into plan mode first.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::EphemeralTip;
use crate::theme::Theme;

/// Ephemeral-tip dedup key for the plan-mode nudge.
pub(crate) const PLAN_NUDGE_KEY: &str = "plan_nudge";

/// Key into the per-session in-memory seen-count map
/// (`AppView::tip_seen_counts`) for the plan nudge. Not persisted to disk.
pub(crate) const PLAN_NUDGE_SEEN_KEY: &str = "plan_nudge_shown_count";

/// The tip stops showing after this many shows within a single session.
const PLAN_NUDGE_SEEN_CAP: u32 = 3;

/// Tight, false-positive-averse allowlist of planning intents (ASCII
/// lowercase). Matched as whole words (see [`prompt_mentions_planning`]) so
/// "explain"/"explanation"/"planet" never trip the "plan" entry.
const PLANNING_KEYWORDS: &[&str] = &[
    "plan",
    "planning",
    "design",
    "architect",
    "step by step",
    "break this down",
    "lay out",
    "approach",
    "strategy",
];

/// Plan-mode chord for the tip copy: always `shift+tab`. Derived from the real
/// `CycleMode` binding (not a literal) — `shift_tab_keys()[0]` is one of the
/// encodings [`crate::input::key::is_shift_tab`] accepts — so it can't drift.
fn plan_chord_label() -> String {
    crate::input::key::shift_tab_keys()[0]
        .display()
        .to_ascii_lowercase()
}

/// Build the "Planning? Check out plan mode via {chord}" tip, seen-gated to
/// [`PLAN_NUDGE_SEEN_CAP`] shows per session (in-memory).
pub fn plan_nudge_tip() -> EphemeralTip {
    let theme = Theme::current();
    let dim = Style::default().fg(theme.gray);
    // Key chord styled like the shortcuts bar (bold secondary on dim text).
    let chord = Style::default()
        .fg(theme.text_secondary)
        .add_modifier(Modifier::BOLD);
    EphemeralTip::new(
        PLAN_NUDGE_KEY,
        Line::from(vec![
            Span::styled("Planning? Check out plan mode via ", dim),
            Span::styled(plan_chord_label(), chord),
        ]),
    )
    .with_session_seen_cap(PLAN_NUDGE_SEEN_KEY, PLAN_NUDGE_SEEN_CAP)
}

/// Whether `text` mentions a planning intent from the tight [`PLANNING_KEYWORDS`]
/// allowlist. Case-insensitive and matched on whole-word boundaries so near
/// neighbours ("explain", "explanation", "planet", "redesign") never match.
/// Non-allocating — runs on the prompt edit hot path.
pub fn prompt_mentions_planning(text: &str) -> bool {
    PLANNING_KEYWORDS
        .iter()
        .any(|kw| contains_whole_word_ci(text, kw))
}

/// Whether ASCII-lowercase `needle` occurs in `haystack` matched
/// case-insensitively and bordered by non-word bytes (or string ends). No
/// allocation: scans bytes and lowercases each candidate byte in place. A byte
/// `>= 0x80` (any UTF-8 multibyte) counts as a word byte, so a keyword touching
/// a non-ASCII letter is conservatively rejected.
fn contains_whole_word_ci(haystack: &str, needle: &str) -> bool {
    let hay = haystack.as_bytes();
    let need = needle.as_bytes();
    if need.is_empty() || hay.len() < need.len() {
        return false;
    }
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b >= 0x80;
    let last_start = hay.len() - need.len();
    for start in 0..=last_start {
        let matches = hay[start..start + need.len()]
            .iter()
            .zip(need)
            .all(|(h, n)| h.to_ascii_lowercase() == *n);
        if !matches {
            continue;
        }
        let before_ok = start == 0 || !is_word(hay[start - 1]);
        let end = start + need.len();
        let after_ok = end >= hay.len() || !is_word(hay[end]);
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_planning_keywords_as_whole_words() {
        assert!(prompt_mentions_planning("let's plan the refactor"));
        assert!(prompt_mentions_planning("Plan it first"));
        assert!(prompt_mentions_planning("some planning before we code"));
        assert!(prompt_mentions_planning("design the system"));
        assert!(prompt_mentions_planning("architect this module"));
        assert!(prompt_mentions_planning(
            "walk me through this step by step"
        ));
        assert!(prompt_mentions_planning("break this down for me"));
        assert!(prompt_mentions_planning("lay out the migration"));
        assert!(prompt_mentions_planning("what's your approach?"));
        assert!(prompt_mentions_planning("pick a strategy"));
    }

    #[test]
    fn does_not_match_near_neighbours() {
        // The headline false positive: "explain" must NOT match "plan".
        assert!(!prompt_mentions_planning("explain this code"));
        assert!(!prompt_mentions_planning("can you explain the explanation"));
        assert!(!prompt_mentions_planning("the planet is round"));
        assert!(!prompt_mentions_planning("redesigned the airplane wing"));
        assert!(!prompt_mentions_planning("fix the bug in main.rs"));
        assert!(!prompt_mentions_planning(""));
    }

    #[test]
    fn plan_nudge_builder_applies_seen_gating() {
        // The wiring to pin is that the builder opts into the per-session seen
        // gate at all (echoing key/cap would be tautological).
        assert_eq!(
            plan_nudge_tip().session_seen.map(|(key, _cap)| key),
            Some(PLAN_NUDGE_SEEN_KEY)
        );
    }

    #[test]
    fn plan_nudge_chord_is_shift_tab() {
        // Always shift+tab — derived from the real binding so it can't drift.
        assert_eq!(plan_chord_label(), "shift+tab");
        // The advertised chord is genuinely one is_shift_tab accepts.
        assert!(crate::input::key::is_shift_tab(
            &crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::BackTab,
                crossterm::event::KeyModifiers::NONE,
            )
        ));
    }
}
