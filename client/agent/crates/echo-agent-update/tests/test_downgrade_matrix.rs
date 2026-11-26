//! Invariant matrix tests for the rollback/downgrade feature.
//!
//! Covers every combination of:
//!   - user's current version vs. channel pointer target
//!   - installer type (internal, npm, gh-release)
//!   - channel (stable, alpha, enterprise)
//!   - pointer-flip scenarios (stable bumped after user upgraded, alpha
//!     pointer rolled back, etc.)
//!
//! Also includes wiremock-based installation tests that verify the GCS
//! internal installer actually downloads and symlinks an older binary
//! when the stable pointer is rolled back.

#![cfg(unix)]

mod common;

use serial_test::serial;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::{FakeBinGuard, reset_home, set_test_version, test_home};
use echo_agent_update::UpdateConfig;
use echo_agent_update::auto_update::{
    auto_update_target, check_update_status, ensure_latest_on_disk, install_internal_from_base,
};
use echo_agent_update::version::installed_on_disk_version;

fn host_platform() -> String {
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        panic!("unsupported test platform");
    };
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        panic!("unsupported test arch");
    };
    format!("{os}-{arch}")
}

fn make_config(channel: &str) -> UpdateConfig {
    UpdateConfig {
        proxy_base_url: "http://test.invalid/v1".to_string(),
        auth_scope: "test".to_string(),
        deployment_key: None,
        alpha_test_key: None,
        channel: channel.to_string(),
        npm_registry: None,
    }
}

async fn mount_gcs_with_channels(
    stable_version: &str,
    alpha_version: Option<&str>,
    binary_version: &str,
    platform: &str,
) -> MockServer {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/stable"))
        .respond_with(ResponseTemplate::new(200).set_body_string(stable_version))
        .mount(&server)
        .await;

    if let Some(alpha_v) = alpha_version {
        Mock::given(method("GET"))
            .and(path("/alpha"))
            .respond_with(ResponseTemplate::new(200).set_body_string(alpha_v))
            .mount(&server)
            .await;
    }

    Mock::given(method("GET"))
        .and(path(format!("/echo-agent-{binary_version}-{platform}")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"#!/bin/sh\nexit 0\n".to_vec()))
        .mount(&server)
        .await;

    server
}

// ─────────────────────────────────────────────────────────────────────────────
// Scenario matrix: GCS internal installer — downgrade via install
//
// Each test simulates a user on version X, with the stable/alpha pointer
// now pointing to version Y. The internal installer should install Y
// regardless of whether Y < X (rollback) or Y > X (upgrade).
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn internal_install_stable_rollback_0_2_7_to_0_2_5() {
    // User was on 0.2.7, stable pointer rolled back to 0.2.5.
    let _ = test_home();
    reset_home();
    let platform = host_platform();
    let server = mount_gcs_with_channels("0.2.5", None, "0.2.5", &platform).await;
    let cfg = make_config("stable");

    install_internal_from_base(Some("0.2.5"), &cfg, &server.uri())
        .await
        .unwrap();

    let home = test_home();
    let downloaded = home
        .join("downloads")
        .join(format!("echo-agent-0.2.5-{platform}"));
    assert!(downloaded.exists(), "rolled-back binary must be downloaded");

    let symlink = home.join("bin").join("echoagent");
    let target = std::fs::read_link(&symlink).unwrap();
    assert!(
        target.to_string_lossy().contains("0.2.5"),
        "symlink must point to rolled-back version: {target:?}"
    );
}

#[tokio::test]
#[serial]
async fn internal_install_stable_upgrade_0_2_5_to_0_2_7() {
    // Normal upgrade path: user on 0.2.5, pointer at 0.2.7.
    let _ = test_home();
    reset_home();
    let platform = host_platform();
    let server = mount_gcs_with_channels("0.2.7", None, "0.2.7", &platform).await;
    let cfg = make_config("stable");

    install_internal_from_base(Some("0.2.7"), &cfg, &server.uri())
        .await
        .unwrap();

    let symlink = test_home().join("bin").join("echoagent");
    let target = std::fs::read_link(&symlink).unwrap();
    assert!(target.to_string_lossy().contains("0.2.7"));
}

#[tokio::test]
#[serial]
async fn internal_install_rollback_then_upgrade_sequence() {
    // Simulates: install 0.2.7 → rollback to 0.2.5 → fix ships as 0.2.8.
    // All three installs must succeed sequentially.
    let _ = test_home();
    reset_home();
    let platform = host_platform();

    for version in ["0.2.7", "0.2.5", "0.2.8"] {
        // Age the previous installs: cleanup deliberately never deletes a
        // freshly-written binary (it may be a concurrent racer's just-renamed
        // download), so the retention assertions below need the earlier
        // installs to look like real leftovers from past releases.
        common::backdate_downloads();
        let server = mount_gcs_with_channels(version, None, version, &platform).await;
        let cfg = make_config("stable");
        install_internal_from_base(Some(version), &cfg, &server.uri())
            .await
            .unwrap();
    }

    let target = std::fs::read_link(test_home().join("bin").join("echoagent")).unwrap();
    assert!(
        target.to_string_lossy().contains("0.2.8"),
        "final symlink must point to 0.2.8: {target:?}"
    );

    // Cleanup retains current + highest-semver non-current (N-1 by version, not install order).
    let downloads = test_home().join("downloads");
    assert!(
        downloads
            .join(format!("echo-agent-0.2.8-{platform}"))
            .exists(),
        "current"
    );
    assert!(
        downloads
            .join(format!("echo-agent-0.2.7-{platform}"))
            .exists(),
        "N-1 by semver"
    );
    assert!(
        !downloads
            .join(format!("echo-agent-0.2.5-{platform}"))
            .exists(),
        "lowest cleaned up"
    );
}

#[tokio::test]
#[serial]
async fn internal_install_alpha_rollback_pointer_resolves_correctly() {
    // Alpha user on 0.2.8-alpha.3. Alpha pointer rolled back to 0.2.8-alpha.1,
    // stable pointer is 0.2.7. Alpha channel returns max(alpha, stable) = 0.2.8-alpha.1.
    let _ = test_home();
    reset_home();
    let platform = host_platform();
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/stable"))
        .respond_with(ResponseTemplate::new(200).set_body_string("0.2.7"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/alpha"))
        .respond_with(ResponseTemplate::new(200).set_body_string("0.2.8-alpha.1"))
        .mount(&server)
        .await;
    // The resolved version is max(0.2.7, 0.2.8-alpha.1) = 0.2.8-alpha.1.
    // Note: semver considers 0.2.8-alpha.1 < 0.2.8 but > 0.2.7.
    Mock::given(method("GET"))
        .and(path(format!("/echo-agent-0.2.8-alpha.1-{platform}")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"#!/bin/sh\nexit 0\n".to_vec()))
        .mount(&server)
        .await;

    let cfg = make_config("alpha");
    install_internal_from_base(None, &cfg, &server.uri())
        .await
        .unwrap();

    let downloaded = test_home()
        .join("downloads")
        .join(format!("echo-agent-0.2.8-alpha.1-{platform}"));
    assert!(
        downloaded.exists(),
        "alpha rollback target must be installed"
    );
}

#[tokio::test]
#[serial]
async fn internal_install_alpha_user_gets_newer_stable_after_stable_passes_alpha() {
    // Alpha user on 0.2.6-alpha.2. Stable ships 0.2.7 (higher than alpha).
    // Alpha channel returns max(alpha=0.2.6-alpha.2, stable=0.2.7) = 0.2.7.
    let _ = test_home();
    reset_home();
    let platform = host_platform();
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/stable"))
        .respond_with(ResponseTemplate::new(200).set_body_string("0.2.7"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/alpha"))
        .respond_with(ResponseTemplate::new(200).set_body_string("0.2.6-alpha.2"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/echo-agent-0.2.7-{platform}")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"#!/bin/sh\nexit 0\n".to_vec()))
        .mount(&server)
        .await;

    let cfg = make_config("alpha");
    install_internal_from_base(None, &cfg, &server.uri())
        .await
        .unwrap();

    assert!(
        test_home()
            .join("downloads")
            .join(format!("echo-agent-0.2.7-{platform}"))
            .exists(),
        "alpha user should get the newer stable"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Scenario matrix: check_update_status across installer × version direction
//
// Uses check_update_status end-to-end with fake npm/gh binaries.
// The internal (GCS) path can't be end-to-end tested via check_update_status
// (hardcoded URLs), so its update-detection logic is covered by the
// needs_update unit tests and the install tests above.
// ─────────────────────────────────────────────────────────────────────────────

fn setup_npm(current_version: &str) -> FakeBinGuard {
    let _ = test_home();
    reset_home();
    set_test_version(current_version);
    // SAFETY: serial_test ensures no race; reset_home clears this between tests.
    unsafe { std::env::set_var("ECHO_AGENT_INSTALLER", "npm") };
    FakeBinGuard::install_npm()
}

fn setup_gh(current_version: &str) -> FakeBinGuard {
    let _ = test_home();
    reset_home();
    set_test_version(current_version);
    // SAFETY: serial_test ensures no race; reset_home clears this between tests.
    unsafe { std::env::set_var("ECHO_AGENT_INSTALLER", "gh-release") };
    FakeBinGuard::install_gh()
}

// ── npm: never downgrades ──

#[tokio::test]
#[serial]
async fn npm_upgrade_reports_update() {
    let g = setup_npm("0.2.5");
    g.set_stdout("\"0.2.7\"");

    let status = check_update_status(&make_config("stable")).await;
    assert!(status.update_available);
    assert_eq!(status.latest_version.as_deref(), Some("0.2.7"));
}

#[tokio::test]
#[serial]
async fn npm_same_version_no_update() {
    let g = setup_npm("0.2.7");
    g.set_stdout("\"0.2.7\"");

    let status = check_update_status(&make_config("stable")).await;
    assert!(!status.update_available);
}

#[tokio::test]
#[serial]
async fn npm_rollback_does_not_report_update() {
    // Stable pointer rolled back 0.2.7 → 0.2.5. npm user on 0.2.7 must NOT
    // see an update — stale registries make this path unsafe.
    let g = setup_npm("0.2.7");
    g.set_stdout("\"0.2.5\"");

    let status = check_update_status(&make_config("stable")).await;
    assert!(
        !status.update_available,
        "npm must never report a downgrade: current={} latest={:?}",
        status.current_version, status.latest_version
    );
}

#[tokio::test]
#[serial]
async fn npm_drastically_old_registry_does_not_report_update() {
    // Corporate registry returns ancient version.
    let g = setup_npm("0.2.7");
    g.set_stdout("\"0.1.4\"");

    let status = check_update_status(&make_config("stable")).await;
    assert!(!status.update_available);
}

// ── gh-release: --check is upgrade-only; rollback handled by auto-install ──

#[tokio::test]
#[serial]
async fn gh_release_upgrade_reports_update() {
    let g = setup_gh("0.2.5");
    g.set_stable_only_stdout("v0.2.7\n");

    let status = check_update_status(&make_config("stable")).await;
    assert!(status.update_available);
    assert_eq!(status.latest_version.as_deref(), Some("0.2.7"));
}

#[tokio::test]
#[serial]
async fn gh_release_rollback_not_advertised_by_check() {
    // `update --check` advertises upgrades only; a rollback still converges via
    // the auto-install path (covered by the internal_install_* tests), not here.
    let g = setup_gh("0.2.7");
    g.set_stable_only_stdout("v0.2.5\n");

    let status = check_update_status(&make_config("stable")).await;
    assert!(
        !status.update_available,
        "gh-release rollback must not be advertised by --check: current={} latest={:?}",
        status.current_version, status.latest_version
    );
    assert_eq!(status.latest_version.as_deref(), Some("0.2.5"));
}

#[tokio::test]
#[serial]
async fn gh_release_same_version_no_update() {
    let g = setup_gh("0.2.7");
    g.set_stable_only_stdout("v0.2.7\n");

    let status = check_update_status(&make_config("stable")).await;
    assert!(!status.update_available);
}

// ─────────────────────────────────────────────────────────────────────────────
// auto_update_target: the leader/background auto-install decision
//
// Unlike the upgrade-only `check_update_status` report, this is the
// downgrade-aware convergence decision. It gates on the installer, so
// authoritative installers (gh-release/internal) follow a rolled-back pointer
// while npm never downgrades. `fetch_latest_version` keeps these hermetic.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn auto_update_target_gh_release_rollback_returns_older() {
    let g = setup_gh("0.2.26");
    g.set_stable_only_stdout("v0.2.22\n");

    assert_eq!(
        auto_update_target(&make_config("stable")).await,
        Some(("gh-release", "0.2.22".to_string())),
        "authoritative installer must converge down on a rolled-back pointer"
    );
}

#[tokio::test]
#[serial]
async fn auto_update_target_gh_release_upgrade_returns_newer() {
    let g = setup_gh("0.2.5");
    g.set_stable_only_stdout("v0.2.7\n");

    assert_eq!(
        auto_update_target(&make_config("stable")).await,
        Some(("gh-release", "0.2.7".to_string()))
    );
}

#[tokio::test]
#[serial]
async fn auto_update_target_gh_release_same_version_returns_none() {
    let g = setup_gh("0.2.7");
    g.set_stable_only_stdout("v0.2.7\n");

    assert_eq!(auto_update_target(&make_config("stable")).await, None);
}

#[tokio::test]
#[serial]
async fn auto_update_target_npm_rollback_returns_none() {
    // npm registries can serve stale versions — never downgrade npm installs.
    let g = setup_npm("0.2.26");
    g.set_stdout("\"0.2.22\"");

    assert_eq!(
        auto_update_target(&make_config("stable")).await,
        None,
        "npm must never be downgraded even when the registry reports an older version"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Disk-aware convergence: ensure_latest_on_disk + installed_on_disk_version
//
// Concurrent updaters (TUI background download, leader hourly checker,
// explicit `echoagent update`) must decide staleness from the on-disk install, not
// their own compiled-in version — a binary another process already installed
// is never downloaded a second time, but a stale running process still gets
// the relaunch signal.
// ─────────────────────────────────────────────────────────────────────────────

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
    std::fs::write(downloads.join(&name), b"#!/bin/sh\nexit 0\n").unwrap();
    std::os::unix::fs::symlink(
        std::path::Path::new("../downloads").join(&name),
        bin.join("echoagent"),
    )
    .unwrap();
}

#[tokio::test]
#[serial]
async fn installed_on_disk_version_reads_symlink_target() {
    let _ = test_home();
    reset_home();
    assert_eq!(installed_on_disk_version(), None, "no install yet");

    fake_managed_install("0.2.7");
    assert_eq!(installed_on_disk_version().as_deref(), Some("0.2.7"));
}

#[tokio::test]
#[serial]
async fn ensure_latest_skips_download_when_disk_current_but_still_relaunches() {
    // Running 0.2.5, pointer 0.2.7, disk already at 0.2.7 (another process
    // downloaded it): no download, but the stale running process must relaunch.
    let g = setup_gh("0.2.5");
    g.set_stable_only_stdout("v0.2.7\n");
    fake_managed_install("0.2.7");

    let outcome = ensure_latest_on_disk(&make_config("stable")).await.unwrap();
    assert_eq!(outcome.installed, None, "must not re-download");
    assert!(outcome.relaunch_needed, "running 0.2.5 < disk 0.2.7");
    assert!(
        !g.args_log().iter().any(|l| l.contains("release download")),
        "no gh download invocation expected, got: {:?}",
        g.args_log()
    );
}

#[tokio::test]
#[serial]
async fn ensure_latest_noop_when_running_and_disk_current() {
    let g = setup_gh("0.2.7");
    g.set_stable_only_stdout("v0.2.7\n");
    fake_managed_install("0.2.7");

    let outcome = ensure_latest_on_disk(&make_config("stable")).await.unwrap();
    assert_eq!(outcome.installed, None);
    assert!(!outcome.relaunch_needed);
}

#[tokio::test]
#[serial]
async fn ensure_latest_relaunches_onto_rolled_back_disk() {
    // Pointer rolled back to 0.2.22 and the disk already converged; a running
    // 0.2.26 leader must relaunch onto the older binary (gh-release is an
    // authoritative installer → downgrades allowed).
    let g = setup_gh("0.2.26");
    g.set_stable_only_stdout("v0.2.22\n");
    fake_managed_install("0.2.22");

    let outcome = ensure_latest_on_disk(&make_config("stable")).await.unwrap();
    assert_eq!(outcome.installed, None, "disk already at pointer");
    assert!(outcome.relaunch_needed, "downgrade relaunch expected");
}

// ─────────────────────────────────────────────────────────────────────────────
// Pointer-flip timing scenarios
//
// These test the race between a user opening echoagent (which caches the version)
// and a pointer flip happening. The 30-min TTL means the user won't see the
// new pointer until the cache expires, but once it does, the correct behavior
// must kick in.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn npm_user_upgraded_then_stable_rolled_back_stays_on_newer() {
    // User ran `echoagent update` and got 0.2.7. Then stable was rolled back to
    // 0.2.5. Next check_update_status sees 0.2.5 from npm. npm installer
    // must NOT report a downgrade.
    let g = setup_npm("0.2.7");
    g.set_stdout("\"0.2.5\"");

    let status = check_update_status(&make_config("stable")).await;
    assert!(!status.update_available);
    assert_eq!(status.latest_version.as_deref(), Some("0.2.5"));
}

#[tokio::test]
#[serial]
async fn gh_release_user_ahead_of_pointer_check_reports_no_update() {
    // User manually installed 0.2.26 (ahead of the stable pointer 0.2.22);
    // `update --check` must not present the older pointer as a new version.
    let g = setup_gh("0.2.26");
    g.set_stable_only_stdout("v0.2.22\n");

    let status = check_update_status(&make_config("stable")).await;
    assert!(
        !status.update_available,
        "ahead-of-pointer must not be advertised as an update: current={} latest={:?}",
        status.current_version, status.latest_version
    );
    assert_eq!(status.latest_version.as_deref(), Some("0.2.22"));
}

#[tokio::test]
#[serial]
async fn npm_alpha_user_upgrade_after_stable_surpasses_alpha() {
    // Alpha user on 0.2.6-alpha.2. Stable ships 0.2.7. npm returns 0.2.7
    // for the @latest tag. User should upgrade.
    let g = setup_npm("0.2.6-alpha.2");
    g.set_stdout("\"0.2.7\"");

    let status = check_update_status(&make_config("stable")).await;
    // Pre-release current on stable channel forces install.
    assert!(
        status.update_available,
        "alpha user should upgrade to stable when stable surpasses alpha"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Double-rollback scenario
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn internal_install_double_rollback() {
    // Ship 0.2.7 → rollback to 0.2.5 → rollback further to 0.2.3.
    // The installer must handle multiple sequential downgrades.
    let _ = test_home();
    reset_home();
    let platform = host_platform();

    for version in ["0.2.7", "0.2.5", "0.2.3"] {
        let server = mount_gcs_with_channels(version, None, version, &platform).await;
        let cfg = make_config("stable");
        install_internal_from_base(Some(version), &cfg, &server.uri())
            .await
            .unwrap();

        let target = std::fs::read_link(test_home().join("bin").join("echoagent")).unwrap();
        assert!(
            target.to_string_lossy().contains(version),
            "symlink must point to {version} after install: {target:?}"
        );
    }
}
