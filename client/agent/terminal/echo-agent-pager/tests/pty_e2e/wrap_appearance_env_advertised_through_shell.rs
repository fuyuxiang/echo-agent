// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Single whitespace-containing argv → `$SHELL -i -c` (same hop as OSC 52).
const PRINT_APPEARANCE: &str =
    "printf 'echoagent=%s lc=%s\\n' \"$ECHO_AGENT_APPEARANCE\" \"$LC_ECHO_AGENT_APPEARANCE\"";

fn parse_printed_appearance(raw: &str) -> Option<(String, String)> {
    let line = raw.lines().find(|l| l.starts_with("echoagent="))?;
    let rest = line.strip_prefix("echoagent=")?;
    let (echoagent, lc) = rest.split_once(" lc=")?;
    Some((echoagent.to_owned(), lc.to_owned()))
}

/// Appearance stamp e2e through the interactive shell hop.
///
/// Parent pins ECHO_AGENT/LC empty. `COLORFGBG` is a dark hint `detect()` would
/// honor, so a wrap that invented polarity from it would stamp `dark`.
/// Do not call `detect_desktop()` here — two live portal probes can disagree.
#[test]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
#[cfg(unix)]
fn wrap_appearance_env_advertised_through_shell() {
    let (code, raw) = run_wrap(
        &[PRINT_APPEARANCE],
        &[
            ("SHELL", "/bin/sh"),
            ("COLORFGBG", "15;0"),
            ("ECHO_AGENT_APPEARANCE", ""),
            ("LC_ECHO_AGENT_APPEARANCE", ""),
        ],
    );
    let (echoagent, lc) = parse_printed_appearance(&raw)
        .unwrap_or_else(|| panic!("missing echoagent=/lc= line\nraw:\n{raw}"));
    match (echoagent.as_str(), lc.as_str()) {
        ("", "") => {}
        ("dark", "dark") | ("light", "light") => {}
        _ => panic!(
            "ECHO_AGENT and LC must agree and not invent from COLORFGBG; echoagent={echoagent:?} lc={lc:?}\nraw:\n{raw}"
        ),
    }
    assert_eq!(
        code,
        Some(0),
        "shell-routed printf must exit 0\nraw:\n{raw}"
    );
}

/// Parent `ECHO_AGENT_APPEARANCE=light` with LC pinned empty: desktop Some overrides
/// both names to the same polarity; desktop None inherits ECHO_AGENT and must not
/// invent LC. No second live desktop probe.
#[test]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
#[cfg(unix)]
fn wrap_appearance_env_desktop_none_does_not_restamp_parent_echo_agent() {
    let (code, raw) = run_wrap(
        &[PRINT_APPEARANCE],
        &[
            ("SHELL", "/bin/sh"),
            ("ECHO_AGENT_APPEARANCE", "light"),
            ("LC_ECHO_AGENT_APPEARANCE", ""),
        ],
    );
    let (echoagent, lc) = parse_printed_appearance(&raw)
        .unwrap_or_else(|| panic!("missing echoagent=/lc= line\nraw:\n{raw}"));
    match (echoagent.as_str(), lc.as_str()) {
        ("light", "") => {}
        ("dark", "dark") | ("light", "light") => {}
        _ => panic!(
            "expected inherit echoagent=light with empty lc, or a matching desktop stamp; echoagent={echoagent:?} lc={lc:?}\nraw:\n{raw}"
        ),
    }
    assert_eq!(
        code,
        Some(0),
        "shell-routed printf must exit 0\nraw:\n{raw}"
    );
}
