// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Dirty-death e2e: when the wrapped child dies with DEC private modes still
/// latched, `echoagent wrap` must emit the matching resets so the outer terminal is
/// not left broken.
///
/// The child SIGKILLs itself after enabling alt screen, all-motion mouse
/// tracking, SGR mouse encoding, bracketed paste, and cursor hide. From wrap's
/// perspective a child SIGKILL is byte-identical to the local ssh client dying
/// on a transport drop (laptop sleep, `~.`, remote host gone): the PTY hits
/// EOF with the enables' reset bytes never having arrived. Only wrap survives
/// to clean up the local terminal.
#[test]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
#[cfg(unix)]
fn wrap_child_killed_with_latched_modes_restores_terminal() {
    let (code, raw) = run_wrap(
        &[
            "/bin/sh",
            "-c",
            r"printf '\033[?1049h\033[?1003h\033[?1006h\033[?2004h\033[?25l'; kill -KILL $$",
        ],
        &[],
    );
    assert!(
        code.is_some(),
        "wrap must exit after the child is killed\nraw:\n{raw:?}"
    );

    // All resets must appear after the last enable (the cursor hide): they can
    // only have come from wrap's own restore path, not from the dead child.
    let last_enable = raw
        .rfind("\x1b[?25l")
        .unwrap_or_else(|| panic!("child's mode enables must pass through\nraw:\n{raw:?}"));
    let reset_pos = |needle: &str| -> usize {
        match raw.rfind(needle) {
            Some(pos) if pos > last_enable => pos,
            Some(_) => {
                panic!("reset {needle:?} must appear after the child's enables\nraw:\n{raw:?}")
            }
            None => panic!(
                "wrap must emit {needle:?} for a mode the dead child left latched\nraw:\n{raw:?}"
            ),
        }
    };

    let alt_screen_leave = reset_pos("\x1b[?1049l");
    for needle in ["\x1b[?1003l", "\x1b[?1006l", "\x1b[?2004l", "\x1b[?25h"] {
        let pos = reset_pos(needle);
        assert!(
            pos < alt_screen_leave,
            "alt-screen leave must come last among the resets ({needle:?} at {pos} vs \
             ?1049l at {alt_screen_leave})\nraw:\n{raw:?}"
        );
    }
}
