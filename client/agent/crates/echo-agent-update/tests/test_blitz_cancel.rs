//! Blitz harness: hammer the download + install lifecycle while injecting a
//! truncation / corruption / cancel at every point, and after every iteration
//! assert the single invariant that makes the brick impossible:
//!
//! > `~/.echo-agent/bin/echoagent` resolves to a binary that passes the smoke-test, OR it
//! > is still the previous-good binary. It is never a broken/partial binary,
//! > and a `.tmp` never masquerades as the active binary.
//!
//! The invariant is checked by RE-RESOLVING the symlink and RE-RUNNING the
//! binary from disk every time — never by re-reading a value the harness set.
//!
//! A controllable raw HTTP/1.1 server serves a real executable ("good")
//! artifact and can truncate the body, close the connection early, serve a
//! right-length-but-garbage body, or hang mid-transfer — for both the parallel
//! byte-range path and the single-connection path.

#![cfg(unix)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serial_test::serial;

use common::artifact_server::{ArtifactServer, Mode};
use common::{
    can_exec_shell_scripts, host_platform, make_update_config, reset_home, small_good_artifact,
    test_home,
};
use echo_agent_update::auto_update::install_internal_from_base;

// ─────────────────────────────────────────────────────────────────────────────
// Artifacts + fixtures
// ─────────────────────────────────────────────────────────────────────────────

/// A real executable larger than the 16 MiB parallel threshold (at least 2
/// chunks), so the parallel byte-range path is exercised. The shell exits on
/// line 2, never reading the newline padding.
fn large_good_artifact() -> Vec<u8> {
    let mut v = b"#!/bin/sh\nexit 0\n".to_vec();
    v.resize(33 * 1024 * 1024, b'\n');
    v
}

/// Seed a previous-good versioned binary + both managed symlinks
/// (`echoagent` and `agent` — see `swap_managed_bin_links`). Returns the
/// absolute path of the seeded binary.
fn seed_previous_good(home: &Path, version: &str, platform: &str) -> PathBuf {
    let downloads = home.join("downloads");
    let bin = home.join("bin");
    std::fs::create_dir_all(&downloads).unwrap();
    std::fs::create_dir_all(&bin).unwrap();

    let prev = downloads.join(format!("echo-agent-{version}-{platform}"));
    std::fs::write(&prev, small_good_artifact()).unwrap();
    std::fs::set_permissions(&prev, std::fs::Permissions::from_mode(0o755)).unwrap();

    let rel = format!("../downloads/echo-agent-{version}-{platform}");
    for name in ["echoagent", "agent"] {
        let link = bin.join(name);
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&rel, &link).unwrap();
    }
    dunce::canonicalize(&prev).unwrap()
}

/// What the active `echoagent` should resolve to after an install attempt.
#[derive(Clone, Copy, PartialEq)]
enum Expect {
    /// The new version was installed and activated.
    NewBinary,
    /// The install was rejected/cancelled; the previous-good binary stays live.
    PreviousGood,
}

/// THE invariant. Re-resolves the on-disk symlink and RE-EXECUTES the resolved
/// binary; never inspects a harness-held value. Guarantees the active managed
/// link is always runnable and is never a `.tmp` or a partial file. Applied
/// to both `echoagent` and `agent` — `swap_managed_bin_links` moves them together.
fn assert_invariant(home: &Path, prev_good: &Path, new_binary: &Path, expect: Expect) {
    for name in ["echoagent", "agent"] {
        assert_link_invariant(home, name, prev_good, new_binary, expect);
    }
}

fn assert_link_invariant(
    home: &Path,
    name: &str,
    prev_good: &Path,
    new_binary: &Path,
    expect: Expect,
) {
    let link = home.join("bin").join(name);
    assert!(link.is_symlink(), "{name} must remain a symlink");

    // Resolve from disk. canonicalize fails on a dangling link — that alone
    // would be a brick.
    let resolved = dunce::canonicalize(&link)
        .unwrap_or_else(|e| panic!("active {name} symlink does not resolve: {e}"));

    // A `.tmp` file must never be the live target.
    let resolved_name = resolved.file_name().unwrap().to_string_lossy().to_string();
    assert!(
        !resolved_name.contains(".tmp"),
        "active {name} must not be a temp file: {resolved_name}"
    );

    // Re-run the resolved binary from disk: the active link must always run.
    let ran_ok = std::process::Command::new(&resolved)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(
        ran_ok,
        "active {name} must pass the smoke-test, but {} did not run",
        resolved.display()
    );

    match expect {
        Expect::NewBinary => assert_eq!(
            resolved,
            dunce::canonicalize(new_binary).unwrap(),
            "expected the newly-installed binary to be active for {name}"
        ),
        Expect::PreviousGood => assert_eq!(
            resolved, prev_good,
            "expected the previous-good binary to stay active for {name} after a rejected install"
        ),
    }
}

/// Run one install attempt against `server` in `mode`, optionally cancelling it
/// after `cancel_after`, then assert the invariant.
async fn run_one(
    server: &ArtifactServer,
    mode: Mode,
    version: &str,
    cancel_after: Option<Duration>,
) {
    let home = test_home();
    reset_home();
    let platform = host_platform();
    let prev_good = seed_previous_good(home, "0.1.100", &platform);
    let new_binary = home
        .join("downloads")
        .join(format!("echo-agent-{version}-{platform}"));
    let cfg = make_update_config("stable");

    server.set_mode(mode);

    let base = server.uri();
    let install = install_internal_from_base(Some(version), &cfg, &base);
    let expect = match (mode, cancel_after) {
        (Mode::Full, None) => {
            install.await.expect("full artifact install should succeed");
            Expect::NewBinary
        }
        (_, Some(deadline)) => {
            // Cancel mid-flight by dropping the future at the timeout.
            let _ = tokio::time::timeout(deadline, install).await;
            Expect::PreviousGood
        }
        _ => {
            let result = install.await;
            assert!(
                result.is_err(),
                "corrupt artifact ({mode:?}) must not install successfully"
            );
            Expect::PreviousGood
        }
    };

    assert_invariant(home, &prev_good, &new_binary, expect);
}

// ─────────────────────────────────────────────────────────────────────────────
// Deterministic matrix — single-connection path (small artifact)
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn blitz_single_connection_matrix() {
    if !can_exec_shell_scripts() {
        eprintln!("skipping: shell scripts cannot execute in this sandbox");
        return;
    }
    let server = ArtifactServer::start(small_good_artifact());
    let len = small_good_artifact().len();

    // Happy path first so we know the symlink CAN move to the new binary.
    run_one(&server, Mode::Full, "0.1.181", None).await;

    // Right-length garbage — caught by the smoke-test (Layer 2).
    run_one(&server, Mode::Garbage, "0.1.181", None).await;

    // Premature EOF at several offsets — caught by the length/transport checks.
    for k in [0usize, 1, len / 2, len.saturating_sub(1)] {
        run_one(&server, Mode::Truncate(k), "0.1.181", None).await;
    }

    // Cancel mid-transfer at several offsets (incl. before any byte and before
    // the HEAD completes), each dropping the in-flight future.
    for k in [0usize, len / 2, len.saturating_sub(1)] {
        run_one(
            &server,
            Mode::Hang(k),
            "0.1.181",
            Some(Duration::from_millis(300)),
        )
        .await;
    }

    // A clean serve still succeeds after the failure matrix. NOTE: run_one
    // calls reset_home() at the start of every case, so this checks the happy
    // path stays reachable — not recovery over a dirty dir. The genuine
    // recovery-without-reset assertion lives in
    // integrity_failure_is_clean_keeps_previous_good_and_emits_telemetry.
    run_one(&server, Mode::Full, "0.1.182", None).await;
}

// ─────────────────────────────────────────────────────────────────────────────
// Deterministic matrix — parallel byte-range path (>= 16 MiB artifact)
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn blitz_parallel_path_matrix() {
    if !can_exec_shell_scripts() {
        eprintln!("skipping: shell scripts cannot execute in this sandbox");
        return;
    }
    let body = large_good_artifact();
    let len = body.len();
    let server = ArtifactServer::start(body);

    // Happy path through the parallel reassembly.
    run_one(&server, Mode::Full, "0.1.181", None).await;

    // Right-length garbage reassembled from range chunks — smoke-test catches.
    run_one(&server, Mode::Garbage, "0.1.181", None).await;

    // Short chunk inside the range / set_len zero region. With Content-Length
    // present (the blitz server always sends it), a premature close surfaces as
    // a reqwest stream error that rejects the chunk; the download_range
    // byte-count check is the belt-and-suspenders for the rarer close-delimited
    // (Content-Length-absent) case. The parallel path falls back to single-
    // connection, which classifies the same truncation as DownloadIncomplete.
    for k in [0usize, 1024, len / 3, len - 4096] {
        run_one(&server, Mode::Truncate(k), "0.1.181", None).await;
    }

    // Cancel mid-chunk.
    run_one(
        &server,
        Mode::Hang(len / 4),
        "0.1.181",
        Some(Duration::from_millis(400)),
    )
    .await;

    // Clean serve recovers.
    run_one(&server, Mode::Full, "0.1.182", None).await;
}

// ─────────────────────────────────────────────────────────────────────────────
// Smoke-test rejects garbage and keeps previous-good
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn smoke_test_rejects_garbage_and_keeps_previous_good() {
    if !can_exec_shell_scripts() {
        eprintln!("skipping: shell scripts cannot execute in this sandbox");
        return;
    }
    let server = ArtifactServer::start(small_good_artifact());
    let home = test_home();
    reset_home();
    let platform = host_platform();
    let prev_good = seed_previous_good(home, "0.1.100", &platform);
    let cfg = make_update_config("stable");

    server.set_mode(Mode::Garbage);
    let base = server.uri();
    let result = install_internal_from_base(Some("0.1.181"), &cfg, &base).await;
    let err = result.expect_err("garbage artifact must not install");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("failed to run") || msg.contains("could not start"),
        "smoke failure should be specific, got: {msg}"
    );

    let new_binary = home
        .join("downloads")
        .join(format!("echo-agent-0.1.181-{platform}"));
    assert_invariant(home, &prev_good, &new_binary, Expect::PreviousGood);

    // A subsequent clean serve must succeed.
    server.set_mode(Mode::Full);
    let base = server.uri();
    install_internal_from_base(Some("0.1.181"), &cfg, &base)
        .await
        .expect("clean serve after a failure should succeed");
    assert_invariant(home, &prev_good, &new_binary, Expect::NewBinary);
}

// ─────────────────────────────────────────────────────────────────────────────
// Bounded randomized fuzz (CI) + ignored stress (1e5+ iterations).
// ─────────────────────────────────────────────────────────────────────────────

/// Cheap deterministic PRNG so the fuzz needs no extra dependency.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

async fn fuzz_loop(iterations: usize, seed: u64) {
    let server = ArtifactServer::start(small_good_artifact());
    let len = small_good_artifact().len();
    let mut rng = Rng(seed);

    for i in 0..iterations {
        let version = if i % 2 == 0 { "0.1.181" } else { "0.1.182" };
        // Periodically verify a clean serve still installs (recovery), but keep
        // the bulk on the fast corruption/cancel paths so the loop stays cheap
        // enough for high iteration counts.
        if i % 10 == 9 {
            run_one(&server, Mode::Full, version, None).await;
            continue;
        }
        match rng.below(3) {
            0 => run_one(&server, Mode::Garbage, version, None).await,
            1 => {
                // k in [0, len): always strictly truncating (k == len would be
                // a complete transfer).
                let k = rng.below(len);
                run_one(&server, Mode::Truncate(k), version, None).await;
            }
            _ => {
                // k in [0, len): Hang holds the socket after k bytes without
                // ever meeting Content-Length, so the client always cancels
                // mid-flight. k == len would transmit the whole body, letting
                // the install complete and the swap land before the deadline —
                // contradicting run_one's PreviousGood expectation (the same
                // reason the Truncate branch above uses rng.below(len)).
                let k = rng.below(len);
                run_one(
                    &server,
                    Mode::Hang(k),
                    version,
                    Some(Duration::from_millis(80)),
                )
                .await;
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn blitz_fuzz_bounded() {
    if !can_exec_shell_scripts() {
        eprintln!("skipping: shell scripts cannot execute in this sandbox");
        return;
    }
    // Kept bounded so CI stays fast; the exhaustive run is the ignored test
    // below. Every iteration still re-resolves and re-runs the on-disk binary.
    fuzz_loop(120, 0x9E3779B97F4A7C15).await;
}

/// The "test it a million times, cancelling at every point" stress run. Gated
/// behind `#[ignore]`; invoke via `just blitz-stress` or
/// `cargo nextest run -p echo-agent-update --run-ignored all`.
#[tokio::test(flavor = "multi_thread")]
#[serial]
#[ignore = "stress: 100k iterations, run via `just blitz-stress`"]
async fn blitz_fuzz_stress() {
    if !can_exec_shell_scripts() {
        eprintln!("skipping: shell scripts cannot execute in this sandbox");
        return;
    }
    let iterations: usize = std::env::var("ECHO_AGENT_BLITZ_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100_000);
    fuzz_loop(iterations, 0xDEADBEEFCAFEF00D).await;
}
