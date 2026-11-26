// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// OSC 52 sink advertisement e2e: the wrapped child must see
/// `ECHO_AGENT_OSC52_SINK=1` (the signal an inner `echoagent` uses to trust OSC 52 over
/// SSH — see `run_wrapped_command`), including through the `$SHELL -i -c` hop.
/// The parent env pins the var to `0` so a pass can only come from the wrap
/// layer's own override, not from an inherited value.
#[test]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
#[cfg(unix)]
fn wrap_osc52_sink_env_advertised_through_shell() {
    let (code, raw) = run_wrap(
        &["echo sink=$ECHO_AGENT_OSC52_SINK"],
        &[("SHELL", "/bin/sh"), ("ECHO_AGENT_OSC52_SINK", "0")],
    );

    assert!(
        raw.contains("sink=1"),
        "wrapped child must see ECHO_AGENT_OSC52_SINK=1 through the shell route\nraw:\n{raw}"
    );
    assert!(
        !raw.contains("sink=0"),
        "the wrap layer must override the inherited value\nraw:\n{raw}"
    );
    assert_eq!(code, Some(0), "shell-routed echo must exit 0\nraw:\n{raw}");
}
