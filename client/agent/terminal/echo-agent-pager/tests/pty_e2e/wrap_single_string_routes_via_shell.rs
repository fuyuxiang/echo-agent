// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Raw-string route e2e — the original field regression (`echoagent wrap "mycli ssh
/// host"`): ONE argv element containing spaces must be handed to `$SHELL -i -c`
/// for word-splitting instead of being spawned as a program literally named
/// `echo wrap-e2e one two`. `SHELL` is pinned to `/bin/sh` for determinism.
#[test]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
#[cfg(unix)]
fn wrap_single_string_routes_via_shell() {
    let (code, raw) = run_wrap(&["echo wrap-e2e one two"], &[("SHELL", "/bin/sh")]);
    assert!(
        raw.contains("wrap-e2e one two"),
        "the quoted command line must be word-split and run by the shell\nraw:\n{raw}"
    );
    assert_eq!(code, Some(0), "shell-routed run must exit 0\nraw:\n{raw}");
}
