//! Reconciles delivered key events with physical input state for
//! terminal/OS pairs that drop modifier bits the application needs.
//! [`KeyboardNormalizer`] pairs an OS-level [`ModifierProbe`] with a
//! [`ModifierDelivery`] classification and rewrites incoming
//! `KeyEvent`s in place so every downstream surface sees the canonical
//! form.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

use crate::terminal::ModifierDelivery;

/// Snapshot of physically-held modifier keys at a single point in time.
/// Future probes can populate more bits; consumers should only read what
/// they need.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct ModifierState {
    pub command: bool,
    pub option: bool,
    pub shift: bool,
    pub control: bool,
}

/// OS-level probe of physical modifier state. One snapshot per call.
pub trait ModifierProbe {
    fn snapshot(&self) -> ModifierState;
}

/// Production probe: macOS reads CoreGraphics, other OSes return all-false.
#[derive(Debug, Default, Clone, Copy)]
pub struct OsModifierProbe;

impl ModifierProbe for OsModifierProbe {
    #[cfg(target_os = "macos")]
    fn snapshot(&self) -> ModifierState {
        super::macos_modifiers::snapshot()
    }

    #[cfg(not(target_os = "macos"))]
    fn snapshot(&self) -> ModifierState {
        ModifierState::default()
    }
}

/// Reusable normalizer that upgrades incoming key events with modifiers
/// the terminal failed to encode.
///
/// One instance lives on [`crate::app::AppView`] and is invoked at the
/// top of `handle_input`, so every downstream surface sees the rescued
/// event. Construct with [`KeyboardNormalizer::from_terminal_context`].
#[derive(Debug, Clone, Copy)]
pub struct KeyboardNormalizer<P: ModifierProbe = OsModifierProbe> {
    probe: P,
    delivery: ModifierDelivery,
}

impl<P: ModifierProbe> KeyboardNormalizer<P> {
    #[cfg(test)]
    pub(crate) fn new(probe: P, delivery: ModifierDelivery) -> Self {
        Self { probe, delivery }
    }

    /// Upgrade a [`KeyEvent`] when a modifier is held but absent from the
    /// event. Returns `Some` only if the delivered event changed.
    pub fn rescue_key(&self, key: KeyEvent) -> Option<KeyEvent> {
        if key.code == KeyCode::Char('\u{0002}') && key.modifiers.is_empty() {
            let mut out = key;
            out.code = KeyCode::Char('b');
            out.modifiers = KeyModifiers::CONTROL;
            return Some(out);
        }
        if !self.delivery.benefits_from_rescue() {
            return None;
        }
        if !key.modifiers.is_empty() {
            return None;
        }
        if !matches!(key.code, KeyCode::Backspace | KeyCode::Delete) {
            return None;
        }
        let state = self.probe.snapshot();
        // Cmd wins per macOS convention: Cmd+Backspace (line-kill) is the
        // stronger action; almost no one holds Cmd+Opt simultaneously.
        let added = match (
            state.command && self.delivery.cmd.benefits_from_rescue(),
            state.option && self.delivery.opt.benefits_from_rescue(),
        ) {
            (true, _) => KeyModifiers::SUPER,
            (false, true) => KeyModifiers::ALT,
            _ => return None,
        };
        tracing::debug!(
            key.code = ?key.code,
            added.modifier = ?added,
            "key event rescued via OS modifier probe"
        );
        let mut out = key;
        out.modifiers |= added;
        Some(out)
    }

    /// Upgrade an [`Event`] in place, owning a fresh `Event::Key` only
    /// when a rescue actually fires.
    pub fn rescue<'a>(&self, ev: &'a Event) -> std::borrow::Cow<'a, Event> {
        if let Event::Key(k) = ev
            && let Some(upgraded) = self.rescue_key(*k)
        {
            return std::borrow::Cow::Owned(Event::Key(upgraded));
        }
        std::borrow::Cow::Borrowed(ev)
    }
}

impl KeyboardNormalizer<OsModifierProbe> {
    pub fn from_terminal_context() -> Self {
        Self {
            probe: OsModifierProbe,
            delivery: crate::terminal::terminal_context()
                .keyboard_capabilities()
                .modifier_delivery,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::ModifierFate;

    #[derive(Debug, Default, Clone, Copy)]
    struct MockProbe(ModifierState);

    impl ModifierProbe for MockProbe {
        fn snapshot(&self) -> ModifierState {
            self.0
        }
    }

    fn drops_both() -> ModifierDelivery {
        ModifierDelivery::new_for_test(ModifierFate::Dropped, ModifierFate::Dropped)
    }

    fn make(state: ModifierState, delivery: ModifierDelivery) -> KeyboardNormalizer<MockProbe> {
        KeyboardNormalizer::new(MockProbe(state), delivery)
    }

    fn bare(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn raw_ctrl_b_canonicalizes_without_modifier_probe() {
        let native = ModifierDelivery::new_for_test(ModifierFate::Native, ModifierFate::Native);
        let n = make(ModifierState::default(), native);
        let raw = KeyEvent::new(KeyCode::Char('\u{0002}'), KeyModifiers::NONE);
        let out = n.rescue_key(raw).expect("raw Ctrl+B must canonicalize");
        assert_eq!(out.code, KeyCode::Char('b'));
        assert_eq!(out.modifiers, KeyModifiers::CONTROL);
        assert!(!crate::input::key::is_text_input_key(&out));
        assert!(crate::key!('b', CONTROL).matches(&out));
    }

    #[test]
    fn cmd_backspace_upgrades_to_super() {
        let n = make(
            ModifierState {
                command: true,
                ..Default::default()
            },
            drops_both(),
        );
        let out = n.rescue_key(bare(KeyCode::Backspace)).unwrap();
        assert_eq!(out.modifiers, KeyModifiers::SUPER);
        assert_eq!(out.code, KeyCode::Backspace);
    }

    #[test]
    fn opt_backspace_upgrades_to_alt() {
        let n = make(
            ModifierState {
                option: true,
                ..Default::default()
            },
            drops_both(),
        );
        let out = n.rescue_key(bare(KeyCode::Backspace)).unwrap();
        assert_eq!(out.modifiers, KeyModifiers::ALT);
    }

    #[test]
    fn cmd_delete_upgrades_to_super() {
        let n = make(
            ModifierState {
                command: true,
                ..Default::default()
            },
            drops_both(),
        );
        let out = n.rescue_key(bare(KeyCode::Delete)).unwrap();
        assert_eq!(out.modifiers, KeyModifiers::SUPER);
    }

    #[test]
    fn cmd_takes_precedence_over_opt_when_both_held() {
        let n = make(
            ModifierState {
                command: true,
                option: true,
                ..Default::default()
            },
            drops_both(),
        );
        let out = n.rescue_key(bare(KeyCode::Backspace)).unwrap();
        assert_eq!(out.modifiers, KeyModifiers::SUPER);
    }

    #[test]
    fn no_modifier_held_skips_rescue() {
        let n = make(ModifierState::default(), drops_both());
        assert!(n.rescue_key(bare(KeyCode::Backspace)).is_none());
    }

    #[test]
    fn already_modified_event_skips_rescue() {
        let n = make(
            ModifierState {
                command: true,
                option: true,
                ..Default::default()
            },
            drops_both(),
        );
        let key = KeyEvent::new(KeyCode::Backspace, KeyModifiers::SHIFT);
        assert!(n.rescue_key(key).is_none());
    }

    #[test]
    fn non_deletion_keys_skip_rescue() {
        let n = make(
            ModifierState {
                command: true,
                option: true,
                ..Default::default()
            },
            drops_both(),
        );
        for code in [
            KeyCode::Char('a'),
            KeyCode::Enter,
            KeyCode::Esc,
            KeyCode::Tab,
            KeyCode::Up,
            KeyCode::Char('v'),
        ] {
            assert!(
                n.rescue_key(bare(code)).is_none(),
                "rescue should not fire for {code:?}"
            );
        }
    }

    #[test]
    fn rescue_only_adds_modifier_for_dropped_axis() {
        // Cmd is Native, only Opt is Dropped → bare Backspace + Cmd held
        // must NOT be rescued (we'd be claiming a modifier the terminal
        // would have delivered).
        let opt_only = ModifierDelivery::new_for_test(ModifierFate::Native, ModifierFate::Dropped);
        let n = make(
            ModifierState {
                command: true,
                ..Default::default()
            },
            opt_only,
        );
        assert!(n.rescue_key(bare(KeyCode::Backspace)).is_none());
        // But Opt held should still rescue.
        let n = make(
            ModifierState {
                option: true,
                ..Default::default()
            },
            opt_only,
        );
        assert_eq!(
            n.rescue_key(bare(KeyCode::Backspace)).unwrap().modifiers,
            KeyModifiers::ALT
        );
    }
}
