use pretty_assertions::assert_eq;

use super::{ModeSupport, Remedy};
use crate::app::ScreenMode;

const FULLSCREEN_ONLY: ModeSupport = ModeSupport::FullscreenOnly(Remedy::SwitchMode {
    why: "minimal is single-session",
});
const MINIMAL_ONLY: ModeSupport =
    ModeSupport::MinimalOnly(Remedy::UseInstead("press → on the block"));

#[test]
fn inline_counts_as_fullscreen() {
    for mode in [ScreenMode::Fullscreen, ScreenMode::Inline] {
        assert!(ModeSupport::Both.supports(mode));
        assert!(FULLSCREEN_ONLY.supports(mode));
        assert!(!MINIMAL_ONLY.supports(mode));
    }

    assert!(ModeSupport::Both.supports(ScreenMode::Minimal));
    assert!(!FULLSCREEN_ONLY.supports(ScreenMode::Minimal));
    assert!(MINIMAL_ONLY.supports(ScreenMode::Minimal));
}

#[test]
fn supported_modes_have_no_refusal() {
    assert_eq!(
        ModeSupport::Both.refusal("theme", ScreenMode::Minimal),
        None
    );
    assert_eq!(
        FULLSCREEN_ONLY.refusal("theme", ScreenMode::Inline),
        None,
        "inline is not minimal, so a fullscreen-only command runs"
    );
    assert_eq!(MINIMAL_ONLY.refusal("expand", ScreenMode::Minimal), None);
}

#[test]
fn switch_mode_refusal_names_the_current_mode_and_the_way_out() {
    assert_eq!(
        FULLSCREEN_ONLY.refusal("theme", ScreenMode::Minimal),
        Some(
            "/theme isn't available in minimal mode (minimal is single-session). \
             Run /fullscreen to switch this session."
                .to_string()
        )
    );
    assert_eq!(
        ModeSupport::MinimalOnly(Remedy::SwitchMode {
            why: "the full TUI prints nothing to re-print"
        })
        .refusal("expand", ScreenMode::Fullscreen),
        Some(
            "/expand isn't available in fullscreen mode \
             (the full TUI prints nothing to re-print). \
             Run /minimal to switch this session."
                .to_string()
        )
    );
}

#[test]
fn use_instead_refusal_names_the_substitute_not_a_relaunch() {
    let refusal = MINIMAL_ONLY
        .refusal("expand", ScreenMode::Fullscreen)
        .expect("minimal-only command is refused in fullscreen");
    assert_eq!(
        refusal,
        "/expand isn't available in fullscreen mode: press → on the block."
    );
    assert!(
        !refusal.contains("/minimal"),
        "suggesting a relaunch contradicts the substitute: {refusal:?}"
    );
}

#[test]
fn already_in_mode_refusal_is_a_plain_statement() {
    assert_eq!(
        ModeSupport::FullscreenOnly(Remedy::AlreadyInMode).refusal("minimal", ScreenMode::Minimal),
        Some("You're already in minimal mode.".to_string())
    );
    assert_eq!(
        ModeSupport::MinimalOnly(Remedy::AlreadyInMode).refusal("fullscreen", ScreenMode::Inline),
        Some("You're already in fullscreen mode.".to_string())
    );
}

/// Pinned on the composed sentence, not the variant, so a remedy that reads
/// wrong to a user lands in the diff rather than only in the code.
#[test]
fn mode_specific_builtin_refusals_are_pinned() {
    let commands = crate::slash::commands::builtin_commands();
    let mut actual: Vec<(&str, String)> = commands
        .iter()
        .filter_map(|command| {
            let refusal = [ScreenMode::Minimal, ScreenMode::Fullscreen]
                .into_iter()
                .find_map(|mode| command.mode_support().refusal(command.name(), mode))?;
            Some((command.name(), refusal))
        })
        .collect();
    actual.sort_unstable();

    assert_eq!(
        actual,
        vec![
            (
                "dashboard",
                "/dashboard isn't available in minimal mode (minimal is single-session). \
                 Run /fullscreen to switch this session."
                    .to_string()
            ),
            (
                "expand",
                "/expand isn't available in fullscreen mode: press Tab to focus the \
                 scrollback, then → on the block."
                    .to_string()
            ),
            (
                "find",
                "/find isn't available in minimal mode (minimal has no scrollback pane: \
                 use your terminal's own search). Run /fullscreen to switch this session."
                    .to_string()
            ),
            (
                "fullscreen",
                "You're already in fullscreen mode.".to_string()
            ),
            (
                "jump",
                "/jump isn't available in minimal mode \
                 (minimal scrolls with your terminal's native scrollback). \
                 Run /fullscreen to switch this session."
                    .to_string()
            ),
            ("minimal", "You're already in minimal mode.".to_string()),
            (
                "theme",
                "/theme isn't available in minimal mode \
                 (minimal renders with your terminal's own palette). \
                 Run /fullscreen to switch this session."
                    .to_string()
            ),
            (
                "timeline",
                "/timeline isn't available in minimal mode \
                 (the timeline rail needs the interactive scrollback pane). \
                 Run /fullscreen to switch this session."
                    .to_string()
            ),
            (
                "tutorial",
                "/tutorial isn't available in minimal mode \
                 (the tutorial overlay needs fullscreen). \
                 Run /fullscreen to switch this session."
                    .to_string()
            ),
        ]
    );
}
