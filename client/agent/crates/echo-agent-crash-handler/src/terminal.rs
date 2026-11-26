//! Terminal restore sequences for signal handler context.
//!
//! See <https://invisible-island.net/xterm/ctlseqs/ctlseqs.html> (DEC
//! Private Mode Reset / "Mouse Tracking" section) for the full spec.

// -----------------------------------------------------------------------
// Canonical list of DEC private modes we enable.
//
// Every mode the pager enables must appear here so that *all* teardown
// paths (normal exit, panic hook, signal handler) disable the same set.
//
//   Mode    Purpose                                          Enabled by
//   ----    -------                                          ----------
//   ?1000   Normal mouse tracking (X11 press/release)        EnableMouseCapture
//   ?1002   Button-event mouse tracking (cell-motion held)   EnableMouseCapture
//   ?1003   All-motion mouse tracking (any movement)         EnableMouseCapture
//   ?1015   RXVT extended mouse reporting (coords >223)      EnableMouseCapture
//   ?1006   SGR extended mouse reporting format (preferred)  EnableMouseCapture
//   ?2004   Bracketed paste mode                             EnableBracketedPaste
//   ?1004   Focus reporting (focus in/out events)            EnableFocusChange
//   ?25     Cursor visibility (show)                         cursor::Hide
//   ?1049   Alternate screen buffer                          EnterAlternateScreen
//   ?2026   Synchronized update                              BeginSynchronizedUpdate
//   CSI<u   Kitty keyboard protocol pop                      PushKeyboardEnhancementFlags
// -----------------------------------------------------------------------

/// Raw CSI sequences to disable every mouse-tracking mode the pager enables
/// (`?1000/?1002/?1003/?1015/?1006`) — the mouse subset of [`MOUSE_PASTE_RESET`],
/// without the bracketed-paste (`?2004l`) reset.
///
/// Use this to assert mouse tracking OFF without disturbing paste — e.g. to
/// clear a terminal left reporting by a prior run (crossterm's Windows
/// `DisableMouseCapture` is winapi-only and never emits this ANSI reset, so an
/// ANSI terminal such as JediTerm keeps reporting until it receives these bytes).
pub const MOUSE_TRACKING_RESET: &[u8] = b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1015l\x1b[?1006l";

/// Raw CSI sequences to disable mouse tracking and bracketed paste.
pub const MOUSE_PASTE_RESET: &[u8] =
    b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1015l\x1b[?1006l\x1b[?2004l";

/// Full escape sequence to restore the terminal to a sane state.
///
/// The kitty CSI-u pop precedes `?1049l` per spec (the protocol stack
/// is per-screen).
pub const RESTORE_SEQ: &[u8] =
    b"\x1b[?2026l\x1b[?25h\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1015l\x1b[?1006l\x1b[?2004l\x1b[?1004l\x1b[<u\x1b[?1049l";

/// Write terminal restore sequences to stderr using raw `libc::write`.
///
/// This is async-signal-safe: it only calls `write(2)` on fd 2 (stderr).
/// Called from the signal handler after writing the crash blob.
#[cfg(unix)]
pub fn restore_in_signal_handler() {
    unsafe {
        libc::write(
            2, // stderr
            RESTORE_SEQ.as_ptr() as *const libc::c_void,
            RESTORE_SEQ.len(),
        );
    }
}

#[cfg(windows)]
pub fn restore_in_signal_handler() {
    unsafe {
        let stderr = windows_sys::Win32::System::Console::GetStdHandle(
            windows_sys::Win32::System::Console::STD_ERROR_HANDLE,
        );
        if !stderr.is_null() && stderr != -1isize as *mut std::ffi::c_void {
            let mut written: u32 = 0;
            windows_sys::Win32::Storage::FileSystem::WriteFile(
                stderr,
                RESTORE_SEQ.as_ptr(),
                RESTORE_SEQ.len() as u32,
                &mut written,
                std::ptr::null_mut(),
            );
        }
    }
}

#[cfg(not(any(unix, windows)))]
pub fn restore_in_signal_handler() {
    // No-op on unsupported platforms.
}

#[cfg(test)]
mod tests {
    use super::*;

    fn position_of(needle: &[u8]) -> usize {
        RESTORE_SEQ
            .windows(needle.len())
            .position(|w| w == needle)
            .unwrap_or_else(|| {
                panic!(
                    "RESTORE_SEQ must contain {:?}",
                    std::str::from_utf8(needle).unwrap_or("<binary>")
                )
            })
    }

    #[test]
    fn restore_seq_pops_kitty_before_alt_screen_leave() {
        assert!(position_of(b"\x1b[<u") < position_of(b"\x1b[?1049l"));
    }

    #[test]
    fn restore_seq_includes_all_modes() {
        for needle in [
            b"\x1b[?2026l".as_slice(),
            b"\x1b[?25h".as_slice(),
            b"\x1b[?1000l".as_slice(),
            b"\x1b[?1002l".as_slice(),
            b"\x1b[?1003l".as_slice(),
            b"\x1b[?1015l".as_slice(),
            b"\x1b[?1006l".as_slice(),
            b"\x1b[?2004l".as_slice(),
            b"\x1b[?1004l".as_slice(),
            b"\x1b[<u".as_slice(),
            b"\x1b[?1049l".as_slice(),
        ] {
            position_of(needle);
        }
    }

    #[test]
    fn restore_seq_ends_synchronized_update_first() {
        // Multiplexers (zellij/tmux) must stop buffering before subsequent
        // resets arrive, otherwise they get batched onto the wrong screen.
        let end_sync = b"\x1b[?2026l";
        assert_eq!(&RESTORE_SEQ[..end_sync.len()], end_sync);
    }
}
