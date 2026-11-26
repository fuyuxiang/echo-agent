// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Fail-fast e2e: an explicit path (contains `/`) never routes through the
/// shell, so a nonexistent one must keep the old precise failure — PTY spawn
/// fails, the exec fallback engages ("wrapped mode failed"), exec ENOENTs
/// ("failed to exec"), and echoagent exits 1. Guards against the shell route ever
/// swallowing path typos into a confusing shell error.
#[test]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
#[cfg(unix)]
fn wrap_explicit_path_not_found_fails_fast() {
    let (code, raw) = run_wrap(&["/nonexistent-echo-agent-wrap-e2e/prog", "arg"], &[]);

    assert!(
        raw.contains("wrapped mode failed"),
        "PTY spawn failure must fall back to exec with the notice\nraw:\n{raw}"
    );
    assert!(
        raw.contains("failed to exec"),
        "exec fallback must report the precise spawn failure\nraw:\n{raw}"
    );
    assert_eq!(
        code,
        Some(1),
        "a not-found explicit path must exit 1\nraw:\n{raw}"
    );
}
