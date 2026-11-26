//! End-to-end tests for the lock-free concurrent-updater convergence model
//! (the "double download" fix): updaters key staleness off the on-disk
//! install, so a binary another process already installed is never
//! downloaded again — and the accepted same-instant residual race is
//! genuinely harmless thanks to per-attempt download temp names.
//!
//! Production has three independent downloader paths that can race around a
//! release:
//!
//! 1. TUI startup: `check_update_background` spawns a detached `echoagent update`
//!    (the Ctrl+U path now adopts this child instead of spawning a second).
//! 2. Explicit `echoagent update` (incl. the Ctrl+U fallback when there is no
//!    live child).
//! 3. Leader mode: the hourly checker runs `ensure_latest_on_disk`
//!    in-process.
//!
//! Two layers are exercised here:
//!
//! - **Convergence** (`ensure_latest_on_disk`, `run_update`): a sequential
//!   updater finds the target already on disk and skips the download. The
//!   artifact server / fake `gh` count downloads so the skip is asserted,
//!   not assumed.
//! - **Race integrity** (`install_internal_from_base` run concurrently): the
//!   same-instant race is accepted as rare; these tests pin the property
//!   that makes it acceptable — concurrent installs (same or *different*
//!   versions) never corrupt the active binary. Before the per-attempt
//!   temp-name fix, every `0.1.x` download shared one `echo-agent-0.1.tmp`
//!   (`with_extension("tmp")` eats everything after the last dot), so racer
//!   A could atomically rename racer B's half-written file into place.

#![cfg(unix)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use serial_test::serial;

use common::artifact_server::ArtifactServer;
use common::{
    FakeBinGuard, can_exec_shell_scripts, host_platform, make_update_config, reset_home,
    set_test_version, small_good_artifact, test_home,
};
use echo_agent_update::auto_update::{
    CliUpdateTrigger, ensure_latest_on_disk, install_internal_from_base, run_update,
};
use echo_agent_update::version::installed_on_disk_version;

/// Assert the active `~/.echo-agent/bin/echoagent` resolves to the expected versioned
/// binary, actually runs, and has exactly the expected content (the content
/// check is what catches a cross-racer temp-file corruption).
fn assert_active_binary(home: &Path, version: &str, platform: &str, expected_content: &[u8]) {
    let link = home.join("bin").join("echoagent");
    assert!(link.is_symlink(), "echoagent must be a symlink");
    let resolved = dunce::canonicalize(&link)
        .unwrap_or_else(|e| panic!("active echoagent symlink does not resolve: {e}"));
    assert_eq!(
        resolved.file_name().unwrap().to_string_lossy(),
        format!("echo-agent-{version}-{platform}"),
        "active echoagent must be the expected version"
    );
    assert_eq!(
        std::fs::read(&resolved).unwrap(),
        expected_content,
        "active binary content must be exactly the served artifact (no \
         partial/interleaved writes from a racing updater)"
    );
    let ran_ok = std::process::Command::new(&resolved)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(ran_ok, "active echoagent must pass the smoke-test");
}

/// Lay down a managed-install layout in the test ECHO_AGENT_HOME:
/// `bin/echoagent -> ../downloads/echo-agent-<version>-<platform>` (what
/// `install_internal_from_base` produces).
fn fake_managed_install(version: &str) {
    let home = test_home();
    let downloads = home.join("downloads");
    let bin = home.join("bin");
    std::fs::create_dir_all(&downloads).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    let name = format!("echo-agent-{version}-{}", host_platform());
    std::fs::write(downloads.join(&name), small_good_artifact()).unwrap();
    std::fs::set_permissions(
        downloads.join(&name),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        std::path::Path::new("../downloads").join(&name),
        bin.join("echoagent"),
    )
    .unwrap();
}

/// Fake `gh` that logs argv to `<dir>/gh-args.log`, answers
/// `release list --exclude-pre-releases` from `<dir>/gh-stable-only-stdout`,
/// and for `release download ... --output <path>` writes a smoke-passing
/// artifact to the output path.
fn fake_gh_serving_releases(dir: &std::path::Path) -> String {
    let dq = format!("'{}'", dir.to_string_lossy().replace('\'', "'\\''"));
    format!(
        r#"#!/bin/sh
echo "$@" >> {dq}/gh-args.log
case "$*" in
  *"release list"*)
    if [ -f {dq}/gh-stable-only-stdout ]; then cat {dq}/gh-stable-only-stdout; fi
    ;;
  *"release download"*)
    out=""
    prev=""
    for a in "$@"; do
      if [ "$prev" = "--output" ]; then out="$a"; fi
      prev="$a"
    done
    if [ -n "$out" ]; then
      printf '#!/bin/sh\nexit 0\n' > "$out"
      chmod +x "$out"
    fi
    ;;
esac
exit 0
"#
    )
}

/// Count `release download` invocations in the fake gh's argv log.
fn gh_download_count(g: &FakeBinGuard) -> usize {
    g.args_log()
        .iter()
        .filter(|l| l.contains("release download"))
        .count()
}

fn setup_gh_release(running_version: &str) -> FakeBinGuard {
    let _ = test_home();
    reset_home();
    set_test_version(running_version);
    // SAFETY: serial_test ensures no race; reset_home clears this between tests.
    unsafe { std::env::set_var("ECHO_AGENT_INSTALLER", "gh-release") };
    FakeBinGuard::install("gh", fake_gh_serving_releases)
}

// ─────────────────────────────────────────────────────────────────────────────
// Convergence: ensure_latest_on_disk downloads once, then every subsequent
// pass (the leader's hourly re-entry) converges without re-downloading.
// This is the e2e companion to the decision-level tests in
// test_downgrade_matrix.rs — it asserts on actual download invocations.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn ensure_latest_downloads_once_then_converges_without_redownload() {
    if !can_exec_shell_scripts() {
        eprintln!("skipping: shell scripts cannot execute in this sandbox");
        return;
    }
    let g = setup_gh_release("0.2.5");
    g.set_stable_only_stdout("v0.2.7\n");
    let cfg = make_update_config("stable");

    // Pass 1: disk is empty → downloads and installs.
    let first = ensure_latest_on_disk(&cfg).await.unwrap();
    assert_eq!(first.installed.as_deref(), Some("0.2.7"));
    assert!(first.relaunch_needed, "running 0.2.5 < disk 0.2.7");
    assert_eq!(gh_download_count(&g), 1, "first pass downloads");
    assert_eq!(installed_on_disk_version().as_deref(), Some("0.2.7"));

    // Pass 2 (the pre-fix hourly re-download): disk already current →
    // no download, but the stale running process still gets the relaunch
    // signal.
    let second = ensure_latest_on_disk(&cfg).await.unwrap();
    assert_eq!(second.installed, None, "second pass must not re-download");
    assert!(second.relaunch_needed, "still running 0.2.5 < disk 0.2.7");
    assert_eq!(
        gh_download_count(&g),
        1,
        "hourly re-entry must not download again"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Convergence: explicit `echoagent update` (the Ctrl+U fallback path) finds the
// binary another process already installed and skips the download — while
// still returning the target version so stale leaders get signalled.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn run_update_skips_download_when_disk_already_current() {
    if !can_exec_shell_scripts() {
        eprintln!("skipping: shell scripts cannot execute in this sandbox");
        return;
    }
    let g = setup_gh_release("0.2.5");
    g.set_stable_only_stdout("v0.2.7\n");
    // Another process (TUI background download) already installed 0.2.7.
    fake_managed_install("0.2.7");
    let mut cfg = make_update_config("stable");

    let result = run_update(false, None, None, &mut cfg, CliUpdateTrigger::UserCommand)
        .await
        .unwrap();

    assert_eq!(
        result.as_deref(),
        Some("0.2.7"),
        "run_update must still report the on-disk target so the caller \
         signals stale leaders to relaunch"
    );
    assert_eq!(
        gh_download_count(&g),
        0,
        "a binary someone else installed must not be downloaded again"
    );
}

#[tokio::test]
#[serial]
async fn run_update_force_still_redownloads_when_disk_current() {
    if !can_exec_shell_scripts() {
        eprintln!("skipping: shell scripts cannot execute in this sandbox");
        return;
    }
    let g = setup_gh_release("0.2.7");
    g.set_stable_only_stdout("v0.2.7\n");
    fake_managed_install("0.2.7");
    let mut cfg = make_update_config("stable");

    let result = run_update(true, None, None, &mut cfg, CliUpdateTrigger::UserCommand)
        .await
        .unwrap();

    assert_eq!(result.as_deref(), Some("0.2.7"));
    assert_eq!(
        gh_download_count(&g),
        1,
        "--force must bypass the disk-current skip and reinstall"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Installer gating: the disk-version probe must only be trusted for
// installers that actually maintain the managed `~/.echo-agent/bin/echoagent` symlink
// (internal, gh-release). For npm, a symlink left over from a previous
// internal install LIES about the npm install's version — and in the worst
// direction (leftover "newer" than the registry) it would silently suppress
// npm updates forever.
// ─────────────────────────────────────────────────────────────────────────────

fn setup_npm(running_version: &str) -> FakeBinGuard {
    let _ = test_home();
    reset_home();
    set_test_version(running_version);
    // SAFETY: serial_test ensures no race; reset_home clears this between tests.
    unsafe { std::env::set_var("ECHO_AGENT_INSTALLER", "npm") };
    FakeBinGuard::install_npm()
}

#[tokio::test]
#[serial]
async fn npm_update_not_suppressed_by_leftover_newer_internal_symlink() {
    if !can_exec_shell_scripts() {
        eprintln!("skipping: shell scripts cannot execute in this sandbox");
        return;
    }
    let g = setup_npm("0.2.5");
    g.set_stdout("\"0.2.7\"\n");
    // Leftover symlink from a previous internal install, claiming to be
    // NEWER than the npm registry. It says nothing about the npm-managed
    // global install and must be ignored for npm staleness decisions.
    fake_managed_install("0.2.9");
    let mut cfg = make_update_config("stable");

    let result = run_update(false, None, None, &mut cfg, CliUpdateTrigger::UserCommand)
        .await
        .unwrap();

    assert_eq!(
        result.as_deref(),
        Some("0.2.7"),
        "npm update must proceed despite the lying leftover symlink"
    );
    assert!(
        g.args_log().iter().any(|l| l.contains("i -g")),
        "npm install must actually run: {:?}",
        g.args_log()
    );
}

#[tokio::test]
#[serial]
async fn ensure_latest_npm_ignores_leftover_internal_symlink() {
    if !can_exec_shell_scripts() {
        eprintln!("skipping: shell scripts cannot execute in this sandbox");
        return;
    }
    let g = setup_npm("0.2.5");
    g.set_stdout("\"0.2.7\"\n");
    fake_managed_install("0.2.9");
    let cfg = make_update_config("stable");

    let outcome = ensure_latest_on_disk(&cfg).await.unwrap();

    assert_eq!(
        outcome.installed.as_deref(),
        Some("0.2.7"),
        "npm leader pass must install despite the lying leftover symlink"
    );
    assert!(
        outcome.relaunch_needed,
        "running 0.2.5 < freshly installed 0.2.7"
    );
    assert!(
        g.args_log().iter().any(|l| l.contains("i -g")),
        "npm install must actually run: {:?}",
        g.args_log()
    );
}

#[tokio::test]
#[serial]
async fn disk_probe_preserves_prerelease_versions() {
    let _ = test_home();
    reset_home();
    // An alpha install must read back as the full pre-release version —
    // truncating to "0.1.220" would mask the alpha → stable update.
    fake_managed_install("0.1.220-alpha.4");
    assert_eq!(
        installed_on_disk_version().as_deref(),
        Some("0.1.220-alpha.4")
    );
}

#[tokio::test]
#[serial]
async fn disk_probe_rejects_dangling_symlink() {
    // If the symlink survives but its target binary was deleted (manual
    // ~/.echo-agent/downloads cleanup), the probe must report None — otherwise
    // every updater would claim "already up to date" forever while no
    // runnable binary exists, and nothing would ever repair the install.
    let home = test_home();
    reset_home();
    let platform = host_platform();
    fake_managed_install("0.2.7");
    assert_eq!(installed_on_disk_version().as_deref(), Some("0.2.7"));

    std::fs::remove_file(
        home.join("downloads")
            .join(format!("echo-agent-0.2.7-{platform}")),
    )
    .unwrap();

    assert_eq!(
        installed_on_disk_version(),
        None,
        "a dangling symlink must not report an installed version"
    );
}

#[tokio::test]
#[serial]
async fn ensure_latest_repairs_dangling_symlink_by_downloading() {
    if !can_exec_shell_scripts() {
        eprintln!("skipping: shell scripts cannot execute in this sandbox");
        return;
    }
    // Dangling symlink + stale running process: the probe returns None, so
    // the decision falls back to the running version and the download runs,
    // repairing the install instead of wedging on "already up to date".
    let g = setup_gh_release("0.2.5");
    g.set_stable_only_stdout("v0.2.7\n");
    let home = test_home();
    let platform = host_platform();
    fake_managed_install("0.2.7");
    std::fs::remove_file(
        home.join("downloads")
            .join(format!("echo-agent-0.2.7-{platform}")),
    )
    .unwrap();
    let cfg = make_update_config("stable");

    let outcome = ensure_latest_on_disk(&cfg).await.unwrap();

    assert_eq!(
        outcome.installed.as_deref(),
        Some("0.2.7"),
        "dangling symlink must be repaired by an actual download"
    );
    assert_eq!(gh_download_count(&g), 1);
    assert_eq!(
        installed_on_disk_version().as_deref(),
        Some("0.2.7"),
        "probe healthy again after the repair install"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Race integrity: the accepted same-instant race must stay harmless. Two (or
// three) installers running concurrently — even for DIFFERENT versions —
// must never leave a corrupt active binary. Pre-fix, all 0.1.x downloads
// shared one `echo-agent-0.1.tmp`, so a concurrent racer could atomically rename a
// half-written file into place.
// ─────────────────────────────────────────────────────────────────────────────

async fn run_concurrent_installs(
    server: &ArtifactServer,
    versions: &[&str],
) -> Vec<anyhow::Result<String>> {
    let base = server.uri();
    let mut tasks = Vec::new();
    for version in versions {
        let base = base.clone();
        let version = version.to_string();
        tasks.push(tokio::spawn(async move {
            let cfg = make_update_config("stable");
            install_internal_from_base(Some(&version), &cfg, &base).await
        }));
    }
    let mut results = Vec::new();
    for t in tasks {
        results.push(t.await.expect("install task must not panic"));
    }
    results
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn concurrent_same_version_installs_leave_valid_active_binary() {
    if !can_exec_shell_scripts() {
        eprintln!("skipping: shell scripts cannot execute in this sandbox");
        return;
    }
    let home = test_home();
    reset_home();
    let platform = host_platform();
    let artifact = small_good_artifact();
    let server = ArtifactServer::start(artifact.clone());
    // Hold responses open so the racers genuinely overlap mid-download.
    server.set_slow(true);

    let results = run_concurrent_installs(&server, &["0.1.181", "0.1.181", "0.1.181"]).await;
    for r in results {
        r.expect("every racing install must succeed (atomic swap, last writer wins)");
    }

    // Lock-free model: concurrent racers may each download (accepted waste);
    // the invariant is integrity, not the count.
    assert!(server.request_count() >= 1);
    assert_active_binary(home, "0.1.181", &platform, &artifact);
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn concurrent_different_version_installs_do_not_corrupt_each_other() {
    if !can_exec_shell_scripts() {
        eprintln!("skipping: shell scripts cannot execute in this sandbox");
        return;
    }
    let home = test_home();
    reset_home();
    let platform = host_platform();
    let artifact = small_good_artifact();
    let server = ArtifactServer::start(artifact.clone());
    server.set_slow(true);

    // Pre-fix, BOTH of these wrote to downloads/echo-agent-0.1.tmp concurrently
    // (with_extension("tmp") truncates at the last dot), so one racer could
    // rename the other's partial file into its own versioned path.
    let results = run_concurrent_installs(&server, &["0.1.181", "0.1.182"]).await;
    for r in results {
        r.expect("both racing installs must succeed");
    }

    // Both versioned binaries must exist with full, uncorrupted content.
    for version in ["0.1.181", "0.1.182"] {
        let path = home
            .join("downloads")
            .join(format!("echo-agent-{version}-{platform}"));
        assert_eq!(
            std::fs::read(&path).unwrap(),
            artifact,
            "binary {version} must contain exactly the served artifact"
        );
    }

    // The active symlink points at whichever racer swapped last; it must
    // resolve and run regardless.
    let resolved = dunce::canonicalize(home.join("bin").join("echoagent")).unwrap();
    assert_eq!(std::fs::read(&resolved).unwrap(), artifact);
    let name = resolved.file_name().unwrap().to_string_lossy().to_string();
    assert!(
        !name.contains(".tmp"),
        "active echoagent must never be a temp file: {name}"
    );

    // No stray shared temp file left behind (the pre-fix collision name).
    assert!(
        !home.join("downloads").join("echo-agent-0.1.tmp").exists(),
        "the pre-fix shared temp name must not exist"
    );
}
