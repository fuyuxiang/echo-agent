//! Vim-style key notation parser.
//!
//! Converts strings like `"hello<CR>"`, `"<C-c>"`, `"<Esc>:wq<CR>"` into
//! raw terminal byte sequences using the `terminput` crate.

use std::io;

use anyhow::{Result, bail};
use terminput::{Encoding, Event, KeyCode, KeyEvent, KeyModifiers};

/// Parse a vim-notation key string into raw terminal bytes.
///
/// # Examples
/// - `"hello"` -> literal bytes for h, e, l, l, o
/// - `"<CR>"` or `"<Enter>"` -> `\r`
/// - `"<C-c>"` -> Ctrl+C (0x03)
/// - `"<Esc>:wq<CR>"` -> ESC, :, w, q, CR
/// - `"<Up><Up><CR>"` -> up arrow, up arrow, CR
pub fn parse_keys(input: &str) -> Result<Vec<u8>> {
    let events = parse_to_events(input)?;
    let mut bytes = Vec::new();
    let mut buf = [0u8; 64];

    for event in events {
        let ev = Event::Key(event);
        match ev.encode(&mut buf, Encoding::Xterm) {
            Ok(n) => bytes.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::Unsupported => {
                // Some keys may not be encodable; skip them.
            }
            Err(e) => return Err(e.into()),
        }
    }

    Ok(bytes)
}

/// Parse vim notation into a sequence of `KeyEvent`s.
fn parse_to_events(input: &str) -> Result<Vec<KeyEvent>> {
    let mut events = Vec::new();
    let mut chars = input.chars().peekable();

    while let Some(&ch) = chars.peek() {
        if ch == '<' {
            // Try to parse a special key notation.
            let start_pos: String = chars.clone().collect();
            if let Some(end) = start_pos.find('>') {
                let notation = &start_pos[1..end]; // between < and >
                // Consume chars including the >.
                for _ in 0..=end {
                    chars.next();
                }
                let event = parse_special(notation)?;
                events.push(event);
            } else {
                // No closing '>', treat '<' as literal.
                chars.next();
                events.push(key(KeyCode::Char('<'), KeyModifiers::NONE));
            }
        } else {
            chars.next();
            events.push(key(KeyCode::Char(ch), KeyModifiers::NONE));
        }
    }

    Ok(events)
}

/// Helper to build a KeyEvent with modifiers.
fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    let mut ev = KeyEvent::new(code);
    ev.modifiers = modifiers;
    ev
}

/// Parse the content between `<` and `>` as a special key.
fn parse_special(notation: &str) -> Result<KeyEvent> {
    let lower = notation.to_lowercase();

    // Parse modifiers: C-, M-/A-, S- (in any order).
    let mut modifiers = KeyModifiers::NONE;
    let mut remaining = lower.as_str();

    loop {
        if let Some(rest) = remaining.strip_prefix("c-") {
            modifiers |= KeyModifiers::CTRL;
            remaining = rest;
        } else if let Some(rest) = remaining
            .strip_prefix("m-")
            .or(remaining.strip_prefix("a-"))
        {
            modifiers |= KeyModifiers::ALT;
            remaining = rest;
        } else if let Some(rest) = remaining.strip_prefix("s-") {
            modifiers |= KeyModifiers::SHIFT;
            remaining = rest;
        } else {
            break;
        }
    }

    let code = match remaining {
        "cr" | "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "bs" | "backspace" => KeyCode::Backspace,
        "tab" => KeyCode::Tab,
        "space" | "spc" => KeyCode::Char(' '),
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" => KeyCode::PageDown,
        "insert" | "ins" => KeyCode::Insert,
        "delete" | "del" => KeyCode::Delete,
        "f1" => KeyCode::F(1),
        "f2" => KeyCode::F(2),
        "f3" => KeyCode::F(3),
        "f4" => KeyCode::F(4),
        "f5" => KeyCode::F(5),
        "f6" => KeyCode::F(6),
        "f7" => KeyCode::F(7),
        "f8" => KeyCode::F(8),
        "f9" => KeyCode::F(9),
        "f10" => KeyCode::F(10),
        "f11" => KeyCode::F(11),
        "f12" => KeyCode::F(12),
        "lt" => KeyCode::Char('<'),
        "gt" => KeyCode::Char('>'),
        "bar" => KeyCode::Char('|'),
        "bslash" => KeyCode::Char('\\'),
        s if s.len() == 1 => {
            let c = s.chars().next().unwrap();
            // For Shift+letter with no other modifiers, uppercase it (vim behavior).
            if modifiers == KeyModifiers::SHIFT && c.is_ascii_alphabetic() {
                modifiers = KeyModifiers::NONE;
                KeyCode::Char(c.to_ascii_uppercase())
            } else {
                KeyCode::Char(c)
            }
        }
        _ => bail!("unknown key notation: <{notation}>"),
    };

    Ok(key(code, modifiers))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_literal_text() {
        let bytes = parse_keys("hello").unwrap();
        assert_eq!(bytes, b"hello");
    }

    #[test]
    fn test_enter() {
        let bytes = parse_keys("<CR>").unwrap();
        assert_eq!(bytes, b"\r");
    }

    #[test]
    fn test_escape() {
        let bytes = parse_keys("<Esc>").unwrap();
        assert_eq!(bytes, b"\x1b");
    }

    #[test]
    fn test_ctrl_c() {
        let bytes = parse_keys("<C-c>").unwrap();
        assert_eq!(bytes, b"\x03");
    }

    #[test]
    fn test_mixed() {
        let bytes = parse_keys("hello<CR>").unwrap();
        assert_eq!(&bytes[..5], b"hello");
        assert_eq!(bytes[5], b'\r');
    }

    #[test]
    fn test_case_insensitive() {
        let a = parse_keys("<cr>").unwrap();
        let b = parse_keys("<CR>").unwrap();
        let c = parse_keys("<Cr>").unwrap();
        assert_eq!(a, b);
        assert_eq!(b, c);
    }
}
