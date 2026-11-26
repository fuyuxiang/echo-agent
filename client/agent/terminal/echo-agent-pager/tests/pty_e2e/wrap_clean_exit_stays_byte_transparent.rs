// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Transparency e2e: when the wrapped child balances every mode it enables and
/// exits cleanly, `echoagent wrap` must add zero reset bytes of its own. Blindly
/// blasting resets on exit would be visible here (duplicate disables, and a
/// kitty pop that could corrupt an enclosing context's keyboard stack); the
/// mode tracker keeps clean exits byte-for-byte transparent.
#[test]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
#[cfg(unix)]
fn wrap_clean_exit_stays_byte_transparent() {
    let (code, raw) = run_wrap(
        &[
            "/bin/sh",
            "-c",
            concat!(
                r"printf '\033[?1049h\033[?1003h\033[?1006h\033[?2004h\033[?25l'; ",
                r"printf '\033[?25h\033[?2004l\033[?1006l\033[?1003l\033[?1049l'",
            ),
        ],
        &[],
    );
    assert_eq!(
        code,
        Some(0),
        "clean child exit must propagate\nraw:\n{raw:?}"
    );

    // Exactly the child's own disables — one occurrence each. A second copy
    // means wrap injected resets on a clean exit.
    for needle in [
        "\x1b[?1003l",
        "\x1b[?1006l",
        "\x1b[?2004l",
        "\x1b[?25h",
        "\x1b[?1049l",
    ] {
        assert_eq!(
            raw.matches(needle).count(),
            1,
            "clean exit must stay byte-transparent: expected exactly the child's own \
             {needle:?}\nraw:\n{raw:?}"
        );
    }
    assert!(
        !raw.contains("\x1b[<u"),
        "wrap must not pop a kitty keyboard stack the child never pushed\nraw:\n{raw:?}"
    );
}
