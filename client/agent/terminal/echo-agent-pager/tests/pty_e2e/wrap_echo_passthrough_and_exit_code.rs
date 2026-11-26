// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Direct-route e2e: `echoagent wrap` with a resolvable explicit path runs the
/// command inside the wrap PTY with output passing through, and propagates the
/// child's exit code — both for success and for a nonzero exit.
#[test]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
#[cfg(unix)]
fn wrap_echo_passthrough_and_exit_code() {
    let (code, raw) = run_wrap(&["/bin/echo", "wrap-e2e-hello"], &[]);
    assert!(
        raw.contains("wrap-e2e-hello"),
        "wrapped echo output must pass through the wrap PTY\nraw:\n{raw}"
    );
    assert_eq!(
        code,
        Some(0),
        "echo's exit code must propagate\nraw:\n{raw}"
    );

    let (code, raw) = run_wrap(&["/bin/sh", "-c", "exit 7"], &[]);
    assert_eq!(
        code,
        Some(7),
        "a nonzero child exit must propagate as echoagent wrap's own exit\nraw:\n{raw}"
    );
}
