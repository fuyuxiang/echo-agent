//! PTY e2e: permission Auto mode is distinct on the real pager screen.
//!
//! Uses `echo-agent-pager-pty-harness` (`PtyHarness`) + Shift+Tab (CSI Z,
//! compatible with `echo-agent-pty` key injection) to cycle Normal → Plan → Auto
//! and assert the mode banner / status shows Auto without conflating
//! Always-Approve.
//!
//! Auth: seeds `HOME/.echo-agent/auth.json` from `ECHO_AGENT_AUTH_JSON` (path) or the
//! developer's `~/.echo-agent/auth.json` so the pager skips device-login when
//! credentials exist. Without auth the test records an environmental
//! failure (login screen) and still asserts the harness API surface.
//!
//! Run with:
//! `cargo test -p echo-agent-pager --test pty_auto_mode -- --ignored --nocapture`

use std::path::PathBuf;
use std::time::Duration;

use echo_agent_pager_pty_harness::{PtyHarness, pager_binary};
use echo_agent_test_support::TestSandbox;

const ROWS: u16 = 40;
const COLS: u16 = 120;
const WELCOME_TIMEOUT: Duration = Duration::from_secs(25);
const WELCOME_SCREEN_SENTINEL: &str = "Quit";

/// Back-tab / Shift+Tab (CSI Z) — pager binds this to CycleMode.
const SHIFT_TAB: &[u8] = b"\x1b[Z";

/// Prefer explicit path, else the user's real `~/.echo-agent/auth.json`.
fn auth_json_source() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("ECHO_AGENT_AUTH_JSON") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    dirs_next_home()
        .map(|h| h.join(".echo-agent/auth.json"))
        .filter(|p| p.is_file())
}

fn dirs_next_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Sandbox HOME + optional auth.json seed (no secrets logged), with the
/// auto-permission-mode feature gate pinned explicitly via `gate_on` so each
/// test is self-contained and deterministic regardless of the runner's shell.
fn prepare_sandbox(sandbox: &mut TestSandbox, gate_on: bool) -> Vec<(String, String)> {
    // Remove rather than empty the fake API key so seeded OIDC remains authoritative.
    sandbox.remove_env("ECHO_AGENT_API_KEY");

    let home = sandbox.home();
    let echoagent = sandbox.echo_agent_home();
    let _ = std::fs::create_dir_all(echoagent);
    if let Some(src) = auth_json_source() {
        let dest = echoagent.join("auth.json");
        if let Err(e) = std::fs::copy(&src, &dest) {
            eprintln!("pty_auto_mode: could not copy auth.json ({e}); login may block mode cycle");
        } else {
            eprintln!(
                "pty_auto_mode: seeded auth from {} ({} bytes)",
                src.display(),
                std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0)
            );
        }
    } else {
        eprintln!("pty_auto_mode: no ~/.echo-agent/auth.json — may hit device login");
    }

    let home_s = home.display().to_string();
    let mut env = vec![
        ("XDG_CONFIG_HOME".into(), format!("{home_s}/.config")),
        ("XDG_DATA_HOME".into(), format!("{home_s}/.local/share")),
        ("XDG_CACHE_HOME".into(), format!("{home_s}/.cache")),
        ("TERM".into(), "xterm-256color".into()),
        ("COLORTERM".into(), "truecolor".into()),
        ("NO_COLOR".into(), "0".into()),
        ("TERM_PROGRAM".into(), "".into()),
        ("TMUX".into(), "".into()),
    ];
    // Pin the feature gate explicitly so the cycle is deterministic regardless
    // of the developer's shell. `ECHO_AGENT_AUTO_PERMISSION_MODE` is the highest gate
    // layer below requirements; "1"/"0" parse to on/off (echo_agent_config::
    // env_bool), and portable-pty merges this over the inherited environment —
    // so an exported value can't flip the result (Auto is present in the ring
    // with the gate on, skipped with it off).
    env.push((
        "ECHO_AGENT_AUTO_PERMISSION_MODE".into(),
        if gate_on { "1" } else { "0" }.into(),
    ));
    env
}

fn is_login_screen(screen: &str) -> bool {
    screen.contains("Waiting for approval")
        || screen.contains("Approve in your browser")
        || screen.contains("finish signing in")
}

/// Whether the caller expects seeded auth (CI / a deliberate e2e run). When set,
/// hitting the login screen is a real failure (broken auth seeding), not an
/// environmental skip — so the test hard-fails instead of passing vacuously.
fn require_auth() -> bool {
    std::env::var("ECHO_AGENT_PTY_REQUIRE_AUTH").is_ok_and(|v| v == "1" || v == "true")
}

/// Cycle into Auto on the welcome / pre-session path and assert screen text
/// shows Auto (mode banner) while not stuck on Always-Approve alone.
#[test]
#[ignore = "spawns real pager PTY; run with cargo test -- --ignored"]
fn pty_shift_tab_cycles_to_auto_mode_banner() {
    let binary = match pager_binary() {
        Ok(b) => b,
        Err(e) => panic!("resolve pager binary via harness env: {e:#}"),
    };
    let mut sandbox = TestSandbox::new();
    let env_owned = prepare_sandbox(&mut sandbox, true);
    let env_refs: Vec<(&str, &str)> = env_owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    // Pin the starting mode: with the gate ON, an empty sandbox home would
    // launch straight into Auto, and this test cycles the ring from Normal.
    let mut harness = PtyHarness::new_in_sandbox(
        &binary,
        ROWS,
        COLS,
        &["--permission-mode", "default"],
        &sandbox,
        &env_refs,
        None,
    )
    .expect("spawn pager in PTY (echo-agent-pager-pty-harness)");

    // Drain startup; welcome or agent chrome.
    let _ = harness.wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT);
    let early = harness.screen_contents();
    if is_login_screen(&early) {
        assert!(
            !require_auth(),
            "ECHO_AGENT_PTY_REQUIRE_AUTH set but pager hit the login screen — auth seeding broke"
        );
        // Auth still blocking (expired token / no network). Honest env failure:
        // the UI-ring guarantee is covered by the dispatch-level unit tests; save
        // the screen for debugging.
        if let Ok(dump) = std::env::var("PTY_AUTO_MODE_SCREEN_DUMP") {
            let _ = std::fs::write(&dump, &early);
        }
        eprintln!(
            "pty_auto_mode: login/device-auth screen blocked Shift+Tab cycle \
             (seeded auth may be expired). Screen saved; treating as env limit — \
             see the permission_auto_mode SessionActor wire tests for coverage."
        );
        // Still prove we exercised PtyHarness spawn (not a no-op).
        let running = harness.is_running().expect("poll pager liveness");
        assert!(
            running || !early.is_empty(),
            "pager must have produced output even on login screen"
        );
        let _ = harness.inject_keys(b"\x11"); // ctrl+q if bound
        return;
    }

    // Normal → Plan
    harness
        .inject_keys(SHIFT_TAB)
        .expect("inject Shift+Tab (Plan)");
    let _ = harness.wait_for_text("Plan", Duration::from_secs(8));

    // Plan → Auto
    harness
        .inject_keys(SHIFT_TAB)
        .expect("inject Shift+Tab (Auto)");
    let saw_auto = harness
        .wait_for_text("Auto", Duration::from_secs(12))
        .is_ok();
    let screen = harness.screen_contents();

    if let Ok(dump) = std::env::var("PTY_AUTO_MODE_SCREEN_DUMP") {
        let _ = std::fs::write(&dump, &screen);
    }

    if is_login_screen(&screen) {
        eprintln!("pty_auto_mode: landed on login after key inject; env auth limit");
        return;
    }

    assert!(
        saw_auto || screen.contains("Auto") || screen.contains("Switched to mode: Auto"),
        "after Plan → Auto cycle, screen must show Auto (distinct mode). screen=\n{screen}"
    );
    if screen.contains("Always-Approve") && !screen.contains("Auto") {
        panic!("landed on Always-Approve without Auto — cycle skipped Auto mode");
    }

    let _ = harness.inject_keys(b"q");
}

/// Gate OFF (the shipped default): the Shift+Tab ring must SKIP Auto entirely
/// (Normal → Plan → Always-Approve → Normal), so the feature is inert and the
/// classifier never launches. Negative companion to the gate-ON cycle test;
/// proves the gate governs the UI ring, not just the engine.
#[test]
#[ignore = "spawns real pager PTY; run with cargo test -- --ignored"]
fn pty_shift_tab_skips_auto_when_gate_off() {
    let binary = match pager_binary() {
        Ok(b) => b,
        Err(e) => panic!("resolve pager binary via harness env: {e:#}"),
    };
    let mut sandbox = TestSandbox::new();
    let env_owned = prepare_sandbox(&mut sandbox, false);
    let env_refs: Vec<(&str, &str)> = env_owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let mut harness =
        PtyHarness::new_in_sandbox(&binary, ROWS, COLS, &[], &sandbox, &env_refs, None)
            .expect("spawn pager in PTY (echo-agent-pager-pty-harness)");

    let _ = harness.wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT);
    let early = harness.screen_contents();
    if is_login_screen(&early) {
        assert!(
            !require_auth(),
            "ECHO_AGENT_PTY_REQUIRE_AUTH set but pager hit the login screen — auth seeding broke"
        );
        eprintln!(
            "pty_auto_mode(gate off): login/device-auth screen blocked cycle; env auth limit"
        );
        let running = harness.is_running().expect("poll pager liveness");
        assert!(
            running || !early.is_empty(),
            "pager must have produced output even on login screen"
        );
        let _ = harness.inject_keys(b"\x11");
        return;
    }

    // Cycle the full ring (4 presses returns to Normal). With the gate off the
    // ring is Normal → Plan → Always-Approve → Normal: Auto must never appear,
    // while Plan and Always-Approve still must (the ring otherwise works).
    let mut saw_plan = false;
    let mut saw_always = false;
    let mut saw_auto = false;
    for _ in 0..4 {
        harness.inject_keys(SHIFT_TAB).expect("inject Shift+Tab");
        // Drain output so the NEW mode banner renders before we read. A
        // `wait_for_text("Switched to mode:")` would hit its fast-path and
        // return instantly on the prior press's banner still on screen, so we
        // explicitly pump for a fixed window instead (the screen tracker keeps
        // only the latest frame, so each read reflects the current mode).
        harness.update(Duration::from_secs(2));
        let s = harness.screen_contents();
        if is_login_screen(&s) {
            eprintln!("pty_auto_mode(gate off): landed on login after key inject; env auth limit");
            return;
        }
        if s.contains("Switched to mode: Plan") {
            saw_plan = true;
        }
        if s.contains("Switched to mode: Always-Approve") {
            saw_always = true;
        }
        if s.contains("Switched to mode: Auto") {
            saw_auto = true;
        }
    }

    if let Ok(dump) = std::env::var("PTY_AUTO_MODE_SCREEN_DUMP") {
        let _ = std::fs::write(&dump, harness.screen_contents());
    }

    assert!(
        !saw_auto,
        "gate OFF: Shift+Tab ring must skip Auto, but the Auto banner appeared"
    );
    assert!(
        saw_plan && saw_always,
        "gate OFF: ring should still cycle Plan and Always-Approve \
         (saw_plan={saw_plan}, saw_always={saw_always})"
    );

    let _ = harness.inject_keys(b"q");
}

/// Structural: harness crate exports used by this e2e (fails compile if removed).
#[test]
fn pty_harness_api_surface_for_auto_mode_e2e() {
    let _ = std::any::type_name::<PtyHarness>();
    assert_eq!(SHIFT_TAB, b"\x1b[Z");
}
