//! SSH tip shown over an unwrapped remote session.
//!
//! Shown once per run, at the first stable agent-view draw — the welcome
//! screen has no ephemeral-tip row, so the first agent render is the
//! earliest surface that can paint it (see
//! `AppView::maybe_trigger_ssh_wrap_tip`). Environment shape comes from
//! `diagnostics::ssh_wrap_hint`; the per-tip config gate is
//! `[ui.contextual_hints].ssh_wrap`.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::EphemeralTip;
use crate::theme::Theme;

/// Ephemeral-tip dedup key for the SSH doctor hint.
pub(crate) const SSH_WRAP_TIP_KEY: &str = "ssh_wrap_tip";

/// Key into the per-session in-memory seen-count map for this tip.
pub(crate) const SSH_WRAP_TIP_SEEN_KEY: &str = "ssh_wrap_tip_shown_count";

/// Stop showing after this many shows within a single session.
const SSH_WRAP_TIP_SEEN_CAP: u32 = 1;

/// Tip lifetime (~10 s at the 30 fps animation cadence). The default ~3 s
/// window suits glanceable notices; this one carries a command the user is
/// meant to read and act on, so it gets a longer window. Ambient bounds it:
/// the TTL pauses while occluded instead of burning off-screen.
pub(crate) const SSH_WRAP_TIP_TICKS: u16 = 300;

/// Build the `/doctor` discovery notice, seen-gated to
/// [`SSH_WRAP_TIP_SEEN_CAP`] show per session. It is about the transport, not
/// the draft, so submitting a prompt right after session load must not
/// retire it, and occlusion pauses (not burns) its TTL.
pub fn ssh_wrap_tip() -> EphemeralTip {
    let theme = Theme::current();
    let dim = Style::default().fg(theme.gray);
    let command = Style::default()
        .fg(theme.text_secondary)
        .add_modifier(Modifier::BOLD);
    EphemeralTip {
        ticks_remaining: SSH_WRAP_TIP_TICKS,
        ..EphemeralTip::new(
            SSH_WRAP_TIP_KEY,
            Line::from(vec![
                Span::styled("Run ", dim),
                Span::styled("/doctor", command),
                Span::styled(" for details and fixes.", dim),
            ]),
        )
        .with_session_seen_cap(SSH_WRAP_TIP_SEEN_KEY, SSH_WRAP_TIP_SEEN_CAP)
        .ambient()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_wrap_tip_builder_applies_seen_gating() {
        assert_eq!(
            ssh_wrap_tip().session_seen,
            Some((SSH_WRAP_TIP_SEEN_KEY, SSH_WRAP_TIP_SEEN_CAP))
        );
    }

    #[test]
    fn ssh_wrap_tip_points_to_doctor() {
        let tip = ssh_wrap_tip();
        assert_eq!(tip.key, SSH_WRAP_TIP_KEY);
        let text: String = tip.line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "Run /doctor for details and fixes.");
    }

    #[test]
    fn ssh_wrap_tip_outlives_default_ttl() {
        let tip = ssh_wrap_tip();
        assert_eq!(tip.ticks_remaining, SSH_WRAP_TIP_TICKS);
        // Read-and-act copy needs more than the glanceable default window.
        assert!(
            tip.ticks_remaining > super::super::DEFAULT_TIP_TICKS,
            "ssh wrap tip must outlive the default TTL"
        );
    }

    #[test]
    fn ssh_wrap_tip_is_ambient() {
        // Must survive prompt submission and pause TTL under occlusion —
        // a session-load tip would otherwise blink away under the first
        // submit or permission ask.
        assert!(ssh_wrap_tip().ambient);
    }
}
