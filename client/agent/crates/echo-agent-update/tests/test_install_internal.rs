//! End-to-end tests for `install_internal` — the GCS-bucket installer used
//! when `installer = "internal"` is configured.
//!
//! Wires together a wiremock-mocked GCS bucket + an isolated `ECHO_AGENT_HOME`
//! tempdir so we can verify the full install pipeline:
//!   fetch version → download echoagent binary → chmod → atomic symlink →
//!   cleanup_old_downloads → persist installer config.
//!
//! The function reads `echo_agent_home()` (a process-wide `OnceLock`), so all
//! tests in this binary share a single `ECHO_AGENT_HOME` and run serially via
//! `#[serial]`.

#![cfg(unix)]

mod common;

use serial_test::serial;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::{reset_home, test_home};
use echo_agent_telemetry::events::CliUpdateErrorKind;
use echo_agent_update::UpdateConfig;
use echo_agent_update::auto_update::{
    classify_install_error, install_internal_from_base, install_internal_from_bases,
};

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

/// Mount GCS endpoints for a given version. Returns the `MockServer`.
async fn mount_gcs(version: &str, platform: &str) -> MockServer {
    let server = MockServer::start().await;

    // Channel pointer: stable returns this version.
    Mock::given(method("GET"))
        .and(path("/stable"))
        .respond_with(ResponseTemplate::new(200).set_body_string(version))
        .mount(&server)
        .await;

    // Main echoagent binary download.
    Mock::given(method("GET"))
        .and(path(format!("/echo-agent-{version}-{platform}")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"#!/bin/sh\nexit 0\n".to_vec()))
        .mount(&server)
        .await;

    server
}

// ─────────────────────────────────────────────────────────────────────────────
// Happy-path
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn install_internal_pinned_version_writes_binary_and_symlink() {
    let _ = test_home();
    reset_home();
    let platform = host_platform();
    let server = mount_gcs("0.1.181", &platform).await;
    let cfg = make_config("stable");

    install_internal_from_base(Some("0.1.181"), &cfg, &server.uri())
        .await
        .unwrap();

    let home = test_home();
    let downloaded = home
        .join("downloads")
        .join(format!("echo-agent-0.1.181-{platform}"));
    assert!(downloaded.exists(), "binary downloaded: {downloaded:?}");
    assert_eq!(std::fs::read(&downloaded).unwrap(), b"#!/bin/sh\nexit 0\n");

    let symlink = home.join("bin").join("echoagent");
    assert!(symlink.is_symlink(), "echoagent symlink created");
    let target = std::fs::read_link(&symlink).unwrap();
    assert_eq!(
        target.file_name().unwrap(),
        format!("echo-agent-0.1.181-{platform}").as_str()
    );

    // `echoagent` and `agent` move together — see `swap_managed_bin_links`.
    let agent_link = home.join("bin").join("agent");
    assert!(agent_link.is_symlink(), "agent symlink created");
    let agent_target = std::fs::read_link(&agent_link).unwrap();
    assert_eq!(
        agent_target, target,
        "agent and echoagent point at same target"
    );
}

/// Regression: pre-existing `agent` symlink from a prior install must be
/// swapped to the new version, not left stale (the original bug).
#[tokio::test]
#[serial]
async fn install_internal_updates_stale_agent_symlink_to_new_version() {
    let _ = test_home();
    reset_home();
    let platform = host_platform();
    let server = mount_gcs("0.1.181", &platform).await;
    let cfg = make_config("stable");

    // Prior install: both links point at an older versioned binary.
    let home = test_home();
    let bin_dir = home.join("bin");
    let download_dir = home.join("downloads");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&download_dir).unwrap();
    let old_binary = download_dir.join(format!("echo-agent-0.1.180-{platform}"));
    std::fs::write(&old_binary, b"#!/bin/sh\nexit 0\n").unwrap();
    let rel_old = std::path::Path::new("..")
        .join("downloads")
        .join(format!("echo-agent-0.1.180-{platform}"));
    std::os::unix::fs::symlink(&rel_old, bin_dir.join("echoagent")).unwrap();
    std::os::unix::fs::symlink(&rel_old, bin_dir.join("agent")).unwrap();

    install_internal_from_base(Some("0.1.181"), &cfg, &server.uri())
        .await
        .unwrap();

    let agent_link = bin_dir.join("agent");
    let agent_target = std::fs::read_link(&agent_link).unwrap();
    assert_eq!(
        agent_target.file_name().unwrap(),
        format!("echo-agent-0.1.181-{platform}").as_str(),
        "agent symlink must swap to the new version, not stay on old"
    );
}

/// Rollback regression: if `agent` swap fails after `echoagent` succeeded,
/// `echoagent` must roll back to its prior target (all-or-nothing).
#[tokio::test]
#[serial]
async fn install_internal_rolls_back_echo_agent_when_agent_swap_fails() {
    let _ = test_home();
    reset_home();
    let platform = host_platform();
    let server = mount_gcs("0.1.181", &platform).await;
    let cfg = make_config("stable");

    let home = test_home();
    let bin_dir = home.join("bin");
    let download_dir = home.join("downloads");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&download_dir).unwrap();
    let old_binary = download_dir.join(format!("echo-agent-0.1.180-{platform}"));
    std::fs::write(&old_binary, b"#!/bin/sh\nexit 0\n").unwrap();
    let rel_old = std::path::Path::new("..")
        .join("downloads")
        .join(format!("echo-agent-0.1.180-{platform}"));
    std::os::unix::fs::symlink(&rel_old, bin_dir.join("echoagent")).unwrap();

    // Sabotage the agent swap: non-empty directory → rename fails with EISDIR.
    let agent_dir = bin_dir.join("agent");
    std::fs::create_dir(&agent_dir).unwrap();
    std::fs::write(agent_dir.join("blocker"), b"x").unwrap();

    let err = install_internal_from_base(Some("0.1.181"), &cfg, &server.uri())
        .await
        .expect_err("agent swap must fail when target is a non-empty dir");
    drop(err);

    // echoagent must be rolled back to the prior version.
    let echo_agent_target = std::fs::read_link(bin_dir.join("echoagent")).unwrap();
    assert_eq!(
        echo_agent_target.file_name().unwrap(),
        format!("echo-agent-0.1.180-{platform}").as_str(),
        "echoagent must be rolled back when agent swap fails"
    );
}

/// Absent-prior rollback regression: fresh install (no prior `echoagent` /
/// `agent`), sabotaged `agent` swap must *remove* the just-created `echoagent`
/// link so we don't leave it on the new binary while `agent` is absent.
#[tokio::test]
#[serial]
async fn install_internal_rollback_removes_absent_prior_echo_agent_link() {
    let _ = test_home();
    reset_home();
    let platform = host_platform();
    let server = mount_gcs("0.1.181", &platform).await;
    let cfg = make_config("stable");

    let home = test_home();
    let bin_dir = home.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();

    // No prior `echoagent`. Sabotage `agent` swap: non-empty directory → EISDIR.
    let agent_dir = bin_dir.join("agent");
    std::fs::create_dir(&agent_dir).unwrap();
    std::fs::write(agent_dir.join("blocker"), b"x").unwrap();
    assert!(
        !bin_dir.join("echoagent").exists() && !bin_dir.join("echoagent").is_symlink(),
        "precondition: echoagent must not exist before install",
    );

    let err = install_internal_from_base(Some("0.1.181"), &cfg, &server.uri())
        .await
        .expect_err("agent swap must fail when target is a non-empty dir");
    drop(err);

    let echo_agent_path = bin_dir.join("echoagent");
    assert!(
        !echo_agent_path.is_symlink() && !echo_agent_path.exists(),
        "echoagent must be removed on rollback when there was no prior link",
    );
}

#[tokio::test]
#[serial]
async fn install_internal_chmods_binary_executable() {
    use std::os::unix::fs::PermissionsExt;
    let _ = test_home();
    reset_home();
    let platform = host_platform();
    let server = mount_gcs("0.1.181", &platform).await;
    let cfg = make_config("stable");

    install_internal_from_base(Some("0.1.181"), &cfg, &server.uri())
        .await
        .unwrap();

    let home = test_home();
    let binary = home
        .join("downloads")
        .join(format!("echo-agent-0.1.181-{platform}"));
    let mode = std::fs::metadata(&binary).unwrap().permissions().mode();
    assert!(mode & 0o111 != 0, "binary must be executable, got {mode:o}");
}

#[tokio::test]
#[serial]
async fn install_internal_cleans_up_stale_pager_symlink() {
    // Old installations shipped a separate echo-agent-pager binary. Verify the
    // update removes the stale symlink from ~/.echo-agent/bin/.
    let _ = test_home();
    reset_home();
    let platform = host_platform();
    let server = mount_gcs("0.1.181", &platform).await;
    let cfg = make_config("stable");

    let home = test_home();
    let bin_dir = home.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let pager_link = bin_dir.join("echo-agent-pager");
    std::os::unix::fs::symlink("/tmp/fake-old-pager", &pager_link).unwrap();
    assert!(
        pager_link.is_symlink(),
        "precondition: stale symlink exists"
    );

    install_internal_from_base(Some("0.1.181"), &cfg, &server.uri())
        .await
        .unwrap();

    assert!(
        !pager_link.exists() && !pager_link.is_symlink(),
        "stale echo-agent-pager symlink should be removed"
    );
}

#[tokio::test]
#[serial]
async fn install_internal_persists_installer_config() {
    let _ = test_home();
    reset_home();
    let platform = host_platform();
    let server = mount_gcs("0.1.181", &platform).await;
    let cfg = make_config("stable");

    install_internal_from_base(Some("0.1.181"), &cfg, &server.uri())
        .await
        .unwrap();

    let home = test_home();
    let cfg_body = std::fs::read_to_string(home.join("config.toml")).unwrap();
    assert!(
        cfg_body.contains("installer = \"internal\""),
        "config should set installer = internal: {cfg_body}"
    );
}

#[tokio::test]
#[serial]
async fn install_internal_resolves_version_via_channel_pointer_when_no_target() {
    let _ = test_home();
    reset_home();
    let platform = host_platform();
    let server = mount_gcs("0.1.181", &platform).await;
    let cfg = make_config("stable");

    // No pinned version → must fetch /stable pointer to resolve.
    install_internal_from_base(None, &cfg, &server.uri())
        .await
        .unwrap();

    let home = test_home();
    assert!(
        home.join("downloads")
            .join(format!("echo-agent-0.1.181-{platform}"))
            .exists(),
        "binary at version from /stable pointer"
    );
}

#[tokio::test]
#[serial]
async fn install_internal_alpha_channel_resolves_max_of_alpha_and_stable() {
    let _ = test_home();
    reset_home();
    let platform = host_platform();
    let server = MockServer::start().await;

    // Stable points to 0.1.181, alpha points to 0.1.180-alpha.5 — stable wins.
    Mock::given(method("GET"))
        .and(path("/stable"))
        .respond_with(ResponseTemplate::new(200).set_body_string("0.1.181"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/alpha"))
        .respond_with(ResponseTemplate::new(200).set_body_string("0.1.180-alpha.5"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/echo-agent-0.1.181-{platform}")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"#!/bin/sh\nexit 0\n".to_vec()))
        .mount(&server)
        .await;

    let cfg = make_config("alpha");
    install_internal_from_base(None, &cfg, &server.uri())
        .await
        .unwrap();

    let home = test_home();
    assert!(
        home.join("downloads")
            .join(format!("echo-agent-0.1.181-{platform}"))
            .exists()
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Failure paths
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn install_internal_fails_on_echo_agent_binary_404() {
    let _ = test_home();
    reset_home();
    let platform = host_platform();
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/stable"))
        .respond_with(ResponseTemplate::new(200).set_body_string("0.1.181"))
        .mount(&server)
        .await;
    // Main binary returns 404 — must propagate as error.
    Mock::given(method("GET"))
        .and(path(format!("/echo-agent-0.1.181-{platform}")))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let cfg = make_config("stable");
    let err = install_internal_from_base(Some("0.1.181"), &cfg, &server.uri())
        .await
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("Download failed"), "msg: {msg}");
    // A real download failure classifies as Download end to end.
    assert_eq!(classify_install_error(&err), CliUpdateErrorKind::Download);
}

#[tokio::test]
#[serial]
async fn install_internal_rejects_invalid_pinned_version() {
    let _ = test_home();
    reset_home();
    let server = MockServer::start().await;
    let cfg = make_config("stable");

    let err = install_internal_from_base(Some("not-a-version"), &cfg, &server.uri())
        .await
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("invalid version format"), "msg: {msg}");
}

// ─────────────────────────────────────────────────────────────────────────────
// Cleanup integration: install v1, then v2, verify N-1 retention.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn install_internal_cleans_up_old_versions_keeping_n_minus_one() {
    let _ = test_home();
    reset_home();
    let platform = host_platform();

    // Install v1, v2, v3 sequentially. After v3, only v3 (current) and v2
    // (N-1) should remain on disk; v1 should be deleted.
    for v in ["0.1.179", "0.1.180", "0.1.181"] {
        // Age earlier installs: cleanup never deletes freshly-written
        // binaries (concurrent-racer protection), so retention assertions
        // need the previous installs to look like old leftovers.
        common::backdate_downloads();
        let server = mount_gcs(v, &platform).await;
        let cfg = make_config("stable");
        install_internal_from_base(Some(v), &cfg, &server.uri())
            .await
            .unwrap();
    }

    let home = test_home();
    let downloads = home.join("downloads");
    assert!(
        downloads
            .join(format!("echo-agent-0.1.181-{platform}"))
            .exists(),
        "current"
    );
    assert!(
        downloads
            .join(format!("echo-agent-0.1.180-{platform}"))
            .exists(),
        "N-1 retained"
    );
    assert!(
        !downloads
            .join(format!("echo-agent-0.1.179-{platform}"))
            .exists(),
        "oldest deleted"
    );

    // Symlink updated to latest.
    let target = std::fs::read_link(home.join("bin").join("echoagent")).unwrap();
    assert!(
        target
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("0.1.181"),
        "symlink points to latest: {target:?}"
    );
}

#[tokio::test]
#[serial]
async fn install_internal_idempotent_for_same_version() {
    // Re-installing the same version should not error and should leave the
    // binary at the same path with the same content.
    let _ = test_home();
    reset_home();
    let platform = host_platform();
    let server = mount_gcs("0.1.181", &platform).await;
    let cfg = make_config("stable");

    install_internal_from_base(Some("0.1.181"), &cfg, &server.uri())
        .await
        .unwrap();
    let first = std::fs::read(
        test_home()
            .join("downloads")
            .join(format!("echo-agent-0.1.181-{platform}")),
    )
    .unwrap();

    install_internal_from_base(Some("0.1.181"), &cfg, &server.uri())
        .await
        .unwrap();
    let second = std::fs::read(
        test_home()
            .join("downloads")
            .join(format!("echo-agent-0.1.181-{platform}")),
    )
    .unwrap();

    assert_eq!(first, second);
    let target = std::fs::read_link(test_home().join("bin").join("echoagent")).unwrap();
    assert!(target.to_string_lossy().contains("0.1.181"));
}

#[tokio::test]
#[serial]
async fn install_internal_creates_echo_agent_home_subdirs_if_missing() {
    let _ = test_home();
    reset_home();
    // Explicitly delete bin/ and downloads/ so install must create them.
    let _ = std::fs::remove_dir_all(test_home().join("bin"));
    let _ = std::fs::remove_dir_all(test_home().join("downloads"));

    let platform = host_platform();
    let server = mount_gcs("0.1.181", &platform).await;
    let cfg = make_config("stable");

    install_internal_from_base(Some("0.1.181"), &cfg, &server.uri())
        .await
        .unwrap();

    assert!(test_home().join("bin").is_dir());
    assert!(test_home().join("downloads").is_dir());
}

// ─────────────────────────────────────────────────────────────────────────────
// Multi-base URL fallback: install_internal_from_bases tries each base in
// preference order, falling through to the next on failure.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn install_internal_from_bases_falls_back_to_secondary_when_primary_fails() {
    // Primary server returns 500 on every endpoint (CDN outage simulation);
    // fallback server serves the install successfully. Result: install
    // succeeds via fallback.
    let _ = test_home();
    reset_home();
    let platform = host_platform();

    let primary = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&primary)
        .await;

    let fallback = mount_gcs("0.1.181", &platform).await;
    let cfg = make_config("stable");

    install_internal_from_bases(
        Some("0.1.181"),
        &cfg,
        &[primary.uri().as_str(), fallback.uri().as_str()],
    )
    .await
    .unwrap();

    assert!(
        test_home()
            .join("downloads")
            .join(format!("echo-agent-0.1.181-{platform}"))
            .exists(),
        "fallback should produce a downloaded binary"
    );
}

#[tokio::test]
#[serial]
async fn install_internal_from_bases_does_not_fallback_on_smoke_failure() {
    // A --version failure is a property of the artifact, not the CDN. The
    // fallback base must not be contacted (a dead fallback would fail
    // differently if we retried).
    let _ = test_home();
    reset_home();
    let platform = host_platform();

    let primary = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/stable"))
        .respond_with(ResponseTemplate::new(200).set_body_string("0.1.181"))
        .mount(&primary)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/echo-agent-0.1.181-{platform}")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"#!/bin/sh\nexit 1\n".to_vec()))
        .mount(&primary)
        .await;

    // Live fallback so we can assert it was never contacted.
    let fallback = MockServer::start().await;

    let cfg = make_config("stable");
    let err = install_internal_from_bases(
        Some("0.1.181"),
        &cfg,
        &[primary.uri().as_str(), fallback.uri().as_str()],
    )
    .await
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("failed to run") || msg.contains("exited"),
        "expected smoke failure, got: {msg}"
    );
    assert!(
        fallback
            .received_requests()
            .await
            .expect("request recording enabled")
            .is_empty(),
        "smoke failure must not trigger a fallback-base retry"
    );
}

#[tokio::test]
#[serial]
async fn install_internal_from_bases_uses_primary_when_it_works() {
    // Both bases work; the install must use the primary (first one) and
    // never touch the fallback. Verified by tearing down the fallback
    // server immediately after configuration — if the install reached for
    // it, the request would fail.
    let _ = test_home();
    reset_home();
    let platform = host_platform();

    let primary = mount_gcs("0.1.181", &platform).await;
    let cfg = make_config("stable");

    install_internal_from_bases(
        Some("0.1.181"),
        &cfg,
        &[primary.uri().as_str(), "http://127.0.0.1:1"],
    )
    .await
    .unwrap();

    assert!(
        test_home()
            .join("downloads")
            .join(format!("echo-agent-0.1.181-{platform}"))
            .exists()
    );
}

#[tokio::test]
#[serial]
async fn install_internal_from_bases_propagates_last_error_when_all_fail() {
    // Every base returns 500 — the install must fail, surfacing the final
    // base's error rather than silently succeeding.
    let _ = test_home();
    reset_home();

    let bad1 = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&bad1)
        .await;

    let bad2 = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&bad2)
        .await;

    let cfg = make_config("stable");
    let err = install_internal_from_bases(
        Some("0.1.181"),
        &cfg,
        &[bad1.uri().as_str(), bad2.uri().as_str()],
    )
    .await
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("Download failed"), "msg: {msg}");
}

/// Regression: a local failure after a successful download (sabotaged
/// `agent` swap) must fail the install immediately — the fallback base must
/// never be contacted for a pointless re-download.
#[tokio::test]
#[serial]
async fn install_internal_from_bases_does_not_redownload_on_local_swap_failure() {
    let _ = test_home();
    reset_home();
    let platform = host_platform();

    let primary = mount_gcs("0.1.181", &platform).await;
    let fallback = mount_gcs("0.1.181", &platform).await;
    let cfg = make_config("stable");

    let home = test_home();
    let bin_dir = home.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    // Sabotage activation: agent as a non-empty dir fails the swap's
    // rollback capture (read_link on a directory) before any rename.
    let agent_dir = bin_dir.join("agent");
    std::fs::create_dir(&agent_dir).unwrap();
    std::fs::write(agent_dir.join("blocker"), b"x").unwrap();

    let err = install_internal_from_bases(
        Some("0.1.181"),
        &cfg,
        &[primary.uri().as_str(), fallback.uri().as_str()],
    )
    .await
    .expect_err("swap failure must fail the install");
    // A real activation failure classifies as Activate end to end.
    assert_eq!(classify_install_error(&err), CliUpdateErrorKind::Activate);

    let fallback_requests = fallback
        .received_requests()
        .await
        .expect("request recording is enabled on MockServer::start()");
    assert!(
        fallback_requests.is_empty(),
        "local swap failure must not fall through to the next base: {} request(s)",
        fallback_requests.len()
    );
}
