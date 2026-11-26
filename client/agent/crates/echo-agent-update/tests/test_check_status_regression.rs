//! End-to-end regression tests for `check_update_status` that lock in the
//! exact JSON shape produced by `echoagent update --check --json` for the failure
//! modes that real users have hit in the wild.
//!
//! Seen when a user is behind a corporate npm registry mirror:
//!
//! ```text
//! # Mirror returns 403 for the @echo-agent-official scope
//! { "currentVersion": "0.1.181", "latestVersion": null,
//!   "updateAvailable": false, "installer": "npm", "channel": "stable",
//!   "autoUpdate": true,
//!   "error": "npm view @latest failed: npm error code E403 ..." }
//!
//! # npm falls back to the public registry which has a stale 0.1.4
//! { "currentVersion": "0.1.181", "latestVersion": "0.1.4",
//!   "updateAvailable": false, "installer": "npm", "channel": "stable",
//!   "autoUpdate": true, "error": null }
//! ```
//!
//! The first case produces `error != null`, the second produces
//! `error == null` but `updateAvailable == false`. Both result in zero
//! visible change for an interactive user — the in-process auto-update
//! check (`run_update_if_available`) silently swallows the same error and
//! the same "already current" outcome.
//!
//! These tests verify the JSON contract so any refactor to `UpdateStatus`,
//! `check_update_status`, or the npm dispatch path will surface a diff.

#![cfg(unix)]

mod common;

use serial_test::serial;

use common::{FakeBinGuard, reset_home, set_test_version, test_home};
use echo_agent_update::UpdateConfig;
use echo_agent_update::auto_update::check_update_status;

/// Set up a fake `npm` on PATH, set `ECHO_AGENT_INSTALLER=npm` so the auto-update
/// code dispatches to npm without consulting config, and pin the installed
/// version to `0.1.181` (matches the user's report).
fn setup() -> FakeBinGuard {
    let _ = test_home();
    reset_home();
    set_test_version("0.1.181");
    // SAFETY: serial_test ensures no race; reset_home will clear this between
    // tests.
    unsafe { std::env::set_var("ECHO_AGENT_INSTALLER", "npm") };
    FakeBinGuard::install_npm()
}

fn make_update_config() -> UpdateConfig {
    UpdateConfig {
        proxy_base_url: "http://test.invalid/v1".to_string(),
        auth_scope: "test".to_string(),
        deployment_key: None,
        alpha_test_key: None,
        channel: "stable".to_string(),
        npm_registry: None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Scenario A: corporate registry 403.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn check_status_surfaces_npm_403_in_error_field() {
    let g = setup();

    // Mimic a corporate registry-mirror 403 response shape (npm exits non-zero,
    // writes the error message to stderr).
    g.set_exit_code(1);
    g.set_stderr(
        "npm error code E403\n\
         npm error 403 403 Forbidden - GET https://registry-mirror.example.invalid/api/npm/js-virtual/@echo-agent-official%2fechoagent\n\
         npm error 403 In most cases, you or one of your dependencies are requesting\n\
         npm error 403 a package version that is forbidden by your security policy",
    );

    let cfg = make_update_config();
    let status = check_update_status(&cfg).await;

    assert_eq!(status.current_version, "0.1.181");
    assert_eq!(status.latest_version, None, "no version when fetch fails");
    assert!(!status.update_available, "no update when fetch fails");
    assert_eq!(status.installer.as_deref(), Some("npm"));
    assert_eq!(status.channel, "stable");
    let err = status
        .error
        .as_deref()
        .expect("error must be populated when npm fails");
    assert!(
        err.contains("npm view") && err.contains("failed"),
        "error must say what failed: {err}"
    );
    assert!(
        err.contains("403") || err.contains("E403") || err.contains("Forbidden"),
        "error must include the underlying HTTP detail: {err}"
    );
}

#[tokio::test]
#[serial]
async fn check_status_npm_403_serializes_to_user_visible_json() {
    // Verify the public JSON shape matches what the user saw in their terminal.
    let g = setup();

    g.set_exit_code(1);
    g.set_stderr("npm error code E403\nnpm error 403 Forbidden");

    let cfg = make_update_config();
    let status = check_update_status(&cfg).await;
    let json = serde_json::to_value(&status).unwrap();

    // Lock in every key the user's tooling depends on.
    assert_eq!(json["currentVersion"], "0.1.181");
    assert!(json["latestVersion"].is_null());
    assert_eq!(json["updateAvailable"], false);
    assert_eq!(json["installer"], "npm");
    assert_eq!(json["channel"], "stable");
    let err = json["error"]
        .as_str()
        .expect("error key must be a string when fetch fails");
    assert!(
        err.contains("E403") || err.contains("403"),
        "error must include 403: {err}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Scenario B: public registry returns stale 0.1.4.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn check_status_returns_no_update_when_registry_has_older_version() {
    // The public registry returns 0.1.4 (much older than installed 0.1.181).
    // `needs_update("0.1.181", "0.1.4", "stable")` returns Some(false), so
    // `updateAvailable` is false and `error` is null. From the user's
    // perspective: silent no-op, even though their preferred upgrade lane
    // (corporate mirror) was unreachable. There's nothing the auto-update
    // code can do here without knowing about scoped registries — but we want
    // to lock in this exact shape so a future change doesn't accidentally
    // present a downgrade as an upgrade.
    let g = setup();
    g.set_stdout("\"0.1.4\"");

    let cfg = make_update_config();
    let status = check_update_status(&cfg).await;

    assert_eq!(status.current_version, "0.1.181");
    assert_eq!(status.latest_version.as_deref(), Some("0.1.4"));
    assert!(
        !status.update_available,
        "older latest must NOT be reported as update available"
    );
    assert_eq!(status.installer.as_deref(), Some("npm"));
    assert!(status.error.is_none(), "no error on successful fetch");
}

#[tokio::test]
#[serial]
async fn check_status_stale_version_serializes_to_user_visible_json() {
    let g = setup();
    g.set_stdout("\"0.1.4\"");

    let cfg = make_update_config();
    let status = check_update_status(&cfg).await;
    let json = serde_json::to_value(&status).unwrap();

    assert_eq!(json["currentVersion"], "0.1.181");
    assert_eq!(json["latestVersion"], "0.1.4");
    assert_eq!(json["updateAvailable"], false);
    assert_eq!(json["installer"], "npm");
    assert_eq!(json["channel"], "stable");
    assert!(json["error"].is_null());
}

// ─────────────────────────────────────────────────────────────────────────────
// Sanity: when npm returns a NEWER version, we DO report an update.
// (Anti-regression: the silent-skip paths must only fire on actual no-op
//  conditions, not collapse into "always returns no update".)
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn check_status_reports_update_when_registry_has_newer_version() {
    let g = setup();
    g.set_stdout("\"0.1.182\"");

    let cfg = make_update_config();
    let status = check_update_status(&cfg).await;

    assert_eq!(status.current_version, "0.1.181");
    assert_eq!(status.latest_version.as_deref(), Some("0.1.182"));
    assert!(status.update_available, "newer version must be reported");
    assert!(status.error.is_none());
}

// ─────────────────────────────────────────────────────────────────────────────
// npm rollback safety: npm must NEVER report a downgrade as an update.
// Stale registries / misconfigured Artifactories returning old versions is a
// known failure mode — the auto-updater must ignore them rather than
// downgrading the user.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn check_status_npm_never_reports_downgrade_as_update() {
    // Verify that the npm path still refuses to report a lower version as
    // an available update, even after the allow_downgrade feature was added
    // for GCS/internal installers. This is the key safety property.
    let g = setup();
    // Simulate a moderate rollback (not a wildly stale version).
    g.set_stdout("\"0.1.179\"");

    let cfg = make_update_config();
    let status = check_update_status(&cfg).await;

    assert_eq!(status.current_version, "0.1.181");
    assert_eq!(status.latest_version.as_deref(), Some("0.1.179"));
    assert!(
        !status.update_available,
        "npm must NOT report a downgrade as update available — stale registries \
         would force-downgrade users to ancient versions"
    );
    assert_eq!(status.installer.as_deref(), Some("npm"));
    assert!(status.error.is_none());
}
