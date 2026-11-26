//! End-to-end coverage of the out-of-process Mermaid render path.
//!
//! Spawns the **real** built pager binary as the hidden `__mermaid-render`
//! child via [`render_via_subprocess`] — the exact function the render worker
//! uses in production — and asserts:
//!   * a valid diagram (the cyclic login-flow) renders to a decodable PNG;
//!   * an oversized / invalid diagram is *contained*: the child exits non-zero,
//!     the parent returns `Err`, and no PNG is written;
//!   * a tight timeout makes the parent kill the child and return `Err` quickly
//!     (a real, process-killable timeout — the crash/timeout containment gate).
//!
//! These exercise the cross-platform `Command` + `current_exe()`/`Child::kill`
//! machinery against the actual binary, which the in-process worker unit tests
//! (under `cargo test`, where the harness binary is not the pager) cannot.
//!
//! Every test is `#[ignore]` (like `pty_e2e`): it needs the built binary, so
//! `cargo test` skips it by default and CI opts in via `-- --ignored`. The
//! binary path is resolved at runtime by [`pager_binary`], so the file still
//! compiles where `CARGO_BIN_EXE_*` is unset (e.g. Bazel), where it is skipped.

use std::time::{Duration, Instant};

use echo_agent_pager::app::mermaid_worker::render_via_subprocess;
use echo_agent_pager::scrollback::blocks::mermaid_content::MermaidRenderQuality;
use echo_agent_pager_pty_harness::pager_binary;

/// A cyclic login-flow whose back-edge (`Attempts -->|No| Enter`) routes back
/// into the cycle — the tricky case for flowchart edge routing.
const LOGIN_FLOW: &str = "flowchart TD\n\
    Start([User visits login page]) --> Enter[Enter username & password]\n\
    Enter --> Submit[Submit credentials]\n\
    Submit --> Validate{Credentials valid?}\n\
    Validate -->|No| Fail[Show error message]\n\
    Fail --> Attempts{Too many failed attempts?}\n\
    Attempts -->|Yes| Lock[Lock account]\n\
    Attempts -->|No| Enter\n\
    Validate -->|Yes| Session[Create session]";

#[test]
#[ignore = "spawns the built pager binary; run with cargo test -- --ignored"]
fn child_renders_login_flow_to_png() {
    let bin = pager_binary().expect("resolve pager binary");
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("login.png");

    let result = render_via_subprocess(
        &bin,
        LOGIN_FLOW,
        false,
        1024,
        MermaidRenderQuality::Open,
        &out,
        Duration::from_secs(30),
    );

    assert!(
        result.is_ok(),
        "the login-flow must render via the __mermaid-render child: {result:?}"
    );
    assert!(out.exists(), "the child wrote the PNG to the out-path");
    let bytes = std::fs::read(&out).expect("read PNG");
    let img = image::load_from_memory(&bytes).expect("output is a decodable PNG");
    assert!(img.width() > 0 && img.height() > 0);
}

#[test]
#[ignore = "spawns the built pager binary; run with cargo test -- --ignored"]
fn oversized_source_is_contained() {
    // Source over the 64 KiB cap: the child rejects it and exits non-zero, so
    // the parent returns Err and no PNG is produced (degrades to the fallback).
    let bin = pager_binary().expect("resolve pager binary");
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("huge.png");
    let huge = format!("flowchart TD\n{}", "A-->B\n".repeat(100_000));

    let result = render_via_subprocess(
        &bin,
        &huge,
        false,
        1024,
        MermaidRenderQuality::Terminal,
        &out,
        Duration::from_secs(30),
    );

    assert!(
        result.is_err(),
        "oversized source must be contained: {result:?}"
    );
    assert!(!out.exists(), "a contained (failed) child writes no PNG");
}

#[test]
#[ignore = "spawns the built pager binary; run with cargo test -- --ignored"]
fn invalid_diagram_is_contained() {
    // An unrenderable diagram: the child's render errors and it exits non-zero;
    // the parent returns Err, no PNG. This is the same containment path a child
    // panic (abort -> non-success exit) would take.
    let bin = pager_binary().expect("resolve pager binary");
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("bad.png");

    let result = render_via_subprocess(
        &bin,
        "this is not a mermaid diagram at all",
        false,
        1024,
        MermaidRenderQuality::Terminal,
        &out,
        Duration::from_secs(30),
    );

    assert!(
        result.is_err(),
        "an invalid diagram must be contained: {result:?}"
    );
    assert!(!out.exists(), "no PNG for an unrenderable diagram");
}

#[test]
#[ignore = "spawns the built pager binary; run with cargo test -- --ignored"]
fn tight_timeout_kills_child_and_returns_err() {
    // A 1 ms budget cannot cover spawning + rendering, so the parent must kill
    // and reap the child and return Err, then return promptly (not block on the
    // child finishing). That the kill actually terminates the child's process
    // group is asserted directly (and without the heavy binary) by the
    // `echo_agent_mermaid::subprocess` `reap_terminates_the_process` unit test;
    // here the loose ceiling just guards against the parent blocking on a child
    // that outlived its budget, while tolerating slow-CI spawn of the real
    // binary.
    let bin = pager_binary().expect("resolve pager binary");
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("slow.png");

    let started = Instant::now();
    let result = render_via_subprocess(
        &bin,
        LOGIN_FLOW,
        false,
        1024,
        MermaidRenderQuality::Open,
        &out,
        Duration::from_millis(1),
    );
    let elapsed = started.elapsed();

    assert!(result.is_err(), "a 1ms budget must time out: {result:?}");
    assert!(
        elapsed < Duration::from_secs(10),
        "the parent must return at the deadline (real kill), took {elapsed:?}",
    );
    assert!(!out.exists(), "a killed child leaves no PNG");
}
