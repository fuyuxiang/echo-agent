//! OS-level rescue for the modified-Enter chord.
//!
//! Apple Terminal can't deliver Shift/Opt/Cmd + Enter modifier flags via
//! crossterm. We side-channel through the same OS probe used by
//! [`super::keyboard_normalizer`] and gate on
//! [`crate::terminal::KeyboardCapabilities::enter_needs_rescue`] so the
//! per-brand truth lives in one place.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::terminal::terminal_context;

/// Returns `true` when the user is holding a modifier that should turn a
/// bare `Enter` into a newline, and the active terminal is classified as
/// dropping that information.
pub fn is_apple_terminal_newline_modifier_held() -> bool {
    let ctx = terminal_context();
    if !ctx.keyboard_capabilities().enter_needs_rescue() {
        return false;
    }
    os_any_newline_modifier_held()
}

/// Shift/Alt+Enter, or bare Enter while a newline modifier is held and the
/// terminal drops those flags ([`is_apple_terminal_newline_modifier_held`]).
/// Always requires `KeyCode::Enter` so Shift+Tab / Shift+letters never match.
///
/// SUPER/Cmd is not included: on most terminals Cmd+Enter is fullscreen or
/// split. Apple Terminal Cmd+Enter is rescued via CoreGraphics on bare Enter
/// ([`is_apple_terminal_newline_modifier_held`]), not the SUPER flag.
pub fn is_mod_enter(key: &KeyEvent) -> bool {
    key.code == KeyCode::Enter
        && (key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT)
            || is_apple_terminal_newline_modifier_held())
}

#[cfg(target_os = "macos")]
fn os_any_newline_modifier_held() -> bool {
    let s = super::macos_modifiers::snapshot();
    s.shift || s.option || s.command
}

#[cfg(not(target_os = "macos"))]
fn os_any_newline_modifier_held() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn is_mod_enter_requires_enter_code() {
        assert!(is_mod_enter(&KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::SHIFT
        )));
        assert!(is_mod_enter(&KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::ALT
        )));
        // SUPER/Cmd is not a product-wide newline chord (fullscreen/split on
        // many terminals). Apple Terminal Cmd+Enter is rescued via CoreGraphics
        // on bare Enter, not via the SUPER flag here.
        assert!(!is_mod_enter(&KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::SUPER
        )));
        assert!(!is_mod_enter(&KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE
        )));
        assert!(!is_mod_enter(&KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::CONTROL
        )));
        // Shift+Tab must never match (BackTab or Tab+SHIFT).
        assert!(!is_mod_enter(&KeyEvent::new(
            KeyCode::BackTab,
            KeyModifiers::NONE
        )));
        assert!(!is_mod_enter(&KeyEvent::new(
            KeyCode::BackTab,
            KeyModifiers::SHIFT
        )));
        assert!(!is_mod_enter(&KeyEvent::new(
            KeyCode::Tab,
            KeyModifiers::SHIFT
        )));
        assert!(!is_mod_enter(&KeyEvent::new(
            KeyCode::Char('a'),
            KeyModifiers::SHIFT
        )));
    }
}
