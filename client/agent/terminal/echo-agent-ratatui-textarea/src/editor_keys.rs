use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{EditCommand, Movement, WordStyle};

/// Resolve a key event into a cursor [`Movement`]; `None` for non-movement keys.
pub(crate) fn resolve_movement(event: &KeyEvent) -> Option<Movement> {
    match event.code {
        // Super matches by `contains` so a stray reported bit (META/HYPER) can't degrade the chord.
        KeyCode::Left if event.modifiers.contains(KeyModifiers::SUPER) => {
            return Some(Movement::VisualRowStart);
        }
        KeyCode::Right if event.modifiers.contains(KeyModifiers::SUPER) => {
            return Some(Movement::VisualRowEnd);
        }
        KeyCode::Home => return Some(Movement::LogicalLineStart),
        KeyCode::End => return Some(Movement::LogicalLineEnd),
        KeyCode::Up => return Some(Movement::VisualRowUp),
        KeyCode::Down => return Some(Movement::VisualRowDown),
        KeyCode::Char('p') if event.modifiers == KeyModifiers::CONTROL => {
            return Some(Movement::VisualRowUp);
        }
        KeyCode::Char('n') if event.modifiers == KeyModifiers::CONTROL => {
            return Some(Movement::VisualRowDown);
        }
        _ => {}
    }
    // Directional (Navigation) commands are exactly the ones with a collapse edge.
    let command = classify_key_event(event)?;
    let edge = command.selection_collapse_edge()?;
    Some(Movement::Command(command, edge))
}

pub fn classify_key_event(event: &KeyEvent) -> Option<EditCommand> {
    match event {
        // Some terminals encode Ctrl-B/Ctrl-F as bare C0 characters.
        KeyEvent {
            code: KeyCode::Char('\u{0002}'),
            modifiers: KeyModifiers::NONE,
            ..
        } => Some(EditCommand::MoveGraphemeLeft),
        KeyEvent {
            code: KeyCode::Char('\u{0006}'),
            modifiers: KeyModifiers::NONE,
            ..
        } => Some(EditCommand::MoveGraphemeRight),
        KeyEvent {
            code: KeyCode::Char('h'),
            modifiers,
            ..
        } if *modifiers == (KeyModifiers::CONTROL | KeyModifiers::ALT) => {
            Some(EditCommand::DeleteWordBackward(WordStyle::Small))
        }
        // Kitty protocol loss can surface Backspace as raw BS or DEL; modifiers are unreliable.
        KeyEvent {
            code: KeyCode::Char('\u{0008}' | '\u{007f}'),
            ..
        } => Some(EditCommand::DeleteGraphemeBackward),
        KeyEvent {
            code: KeyCode::Backspace,
            modifiers,
            ..
        } => Some(backspace_command(*modifiers)),
        KeyEvent {
            code: KeyCode::Delete,
            modifiers,
            ..
        } => Some(delete_command(*modifiers)),
        KeyEvent {
            code: KeyCode::Char('w'),
            modifiers: KeyModifiers::CONTROL,
            ..
        } => Some(EditCommand::DeleteWordBackward(
            WordStyle::WhitespaceDelimited,
        )),
        KeyEvent {
            code: KeyCode::Left,
            modifiers,
            ..
        } if modifiers.intersects(KeyModifiers::ALT | KeyModifiers::CONTROL) => {
            Some(EditCommand::MoveWordLeft(WordStyle::Small))
        }
        KeyEvent {
            code: KeyCode::Right,
            modifiers,
            ..
        } if modifiers.intersects(KeyModifiers::ALT | KeyModifiers::CONTROL) => {
            Some(EditCommand::MoveWordRight(WordStyle::Small))
        }
        KeyEvent {
            code: KeyCode::Char('a'),
            modifiers: KeyModifiers::CONTROL,
            ..
        } => Some(EditCommand::MoveLogicalLineStart),
        KeyEvent {
            code: KeyCode::Char('e'),
            modifiers: KeyModifiers::CONTROL,
            ..
        } => Some(EditCommand::MoveLogicalLineEnd),
        KeyEvent {
            code: KeyCode::Left,
            modifiers: KeyModifiers::NONE,
            ..
        }
        | KeyEvent {
            code: KeyCode::Char('b'),
            modifiers: KeyModifiers::CONTROL,
            ..
        } => Some(EditCommand::MoveGraphemeLeft),
        KeyEvent {
            code: KeyCode::Right,
            modifiers: KeyModifiers::NONE,
            ..
        }
        | KeyEvent {
            code: KeyCode::Char('f'),
            modifiers: KeyModifiers::CONTROL,
            ..
        } => Some(EditCommand::MoveGraphemeRight),
        KeyEvent {
            code: KeyCode::Char('b'),
            modifiers: KeyModifiers::ALT,
            ..
        } => Some(EditCommand::MoveWordLeft(WordStyle::Small)),
        KeyEvent {
            code: KeyCode::Char('f'),
            modifiers: KeyModifiers::ALT,
            ..
        } => Some(EditCommand::MoveWordRight(WordStyle::Small)),
        KeyEvent {
            code: KeyCode::Char('u'),
            modifiers: KeyModifiers::CONTROL,
            ..
        } => Some(EditCommand::DeleteToLineStart),
        KeyEvent {
            code: KeyCode::Char('k'),
            modifiers: KeyModifiers::CONTROL,
            ..
        } => Some(EditCommand::DeleteToLineEnd),
        KeyEvent {
            code: KeyCode::Char('h'),
            modifiers: KeyModifiers::CONTROL,
            ..
        } => Some(EditCommand::DeleteGraphemeBackward),
        KeyEvent {
            code: KeyCode::Char('d'),
            modifiers: KeyModifiers::CONTROL,
            ..
        } => Some(EditCommand::DeleteGraphemeForward),
        KeyEvent {
            code: KeyCode::Char('d'),
            modifiers,
            ..
        } if modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SUPER) => {
            Some(EditCommand::DeleteWordForward(WordStyle::Small))
        }
        KeyEvent {
            code: KeyCode::Char(character),
            modifiers: KeyModifiers::NONE | KeyModifiers::SHIFT,
            ..
        } if !character.is_control() => {
            let character = if event.modifiers.contains(KeyModifiers::SHIFT) {
                shifted_char(*character)
            } else {
                *character
            };
            Some(EditCommand::Insert(character))
        }
        KeyEvent {
            code: KeyCode::Char(character),
            modifiers,
            ..
        } if crate::is_altgr(*modifiers) && !character.is_control() => {
            Some(EditCommand::Insert(*character))
        }
        _ => None,
    }
}

fn shifted_char(character: char) -> char {
    if character.is_ascii_lowercase() {
        character.to_ascii_uppercase()
    } else {
        character
    }
}

fn backspace_command(modifiers: KeyModifiers) -> EditCommand {
    // Backspace preserves exact historical chords; extra modifiers fall back to grapheme delete.
    match modifiers {
        KeyModifiers::ALT | KeyModifiers::CONTROL => {
            EditCommand::DeleteWordBackward(WordStyle::Small)
        }
        KeyModifiers::SUPER => EditCommand::DeleteToLineStart,
        _ => EditCommand::DeleteGraphemeBackward,
    }
}

fn delete_command(modifiers: KeyModifiers) -> EditCommand {
    // Delete accepts Shift in addition to a word modifier because enhanced protocols retain it.
    if modifiers.intersects(KeyModifiers::ALT | KeyModifiers::CONTROL | KeyModifiers::SUPER) {
        EditCommand::DeleteWordForward(WordStyle::Small)
    } else {
        EditCommand::DeleteGraphemeForward
    }
}
