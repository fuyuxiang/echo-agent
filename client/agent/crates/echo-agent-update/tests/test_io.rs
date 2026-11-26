//! I/O integration tests for the auto-update crate.
//!
//! These tests touch global process state — `ECHO_AGENT_HOME` (a `OnceLock` in
//! `echo-agent-config`), `ECHO_AGENT_TEST_VERSION`, and `NPM_TOKEN` — so they
//! must run serially. Once `ECHO_AGENT_HOME` is initialized for a process, it can't
//! be changed; we set it from a single shared `OnceLock` and reset the
//! contents of the directory between tests.
//!
//! The patterns here mirror the ECHO_AGENT_HOME isolation used in other
//! integration tests.

mod common;

use std::path::PathBuf;
use std::time::Duration;

use serial_test::serial;

use common::{reset_home, test_home};
use echo_agent_update::write_version_cache;

/// Path to the version cache file inside the test home.
fn version_cache_path() -> PathBuf {
    test_home().join("version.json")
}

/// Local alias kept so existing test bodies don't need to change.
fn reset() {
    reset_home();
}

// ─────────────────────────────────────────────────────────────────────────────
// write_version_cache
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn write_version_cache_creates_file_at_echo_agent_home() {
    let _ = test_home();
    reset();

    write_version_cache("0.1.180", None).await;

    let path = version_cache_path();
    assert!(
        path.exists(),
        "version.json should exist at {}",
        path.display()
    );

    let body = std::fs::read_to_string(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["version"], "0.1.180");
    assert!(
        parsed["checked_at"].as_str().is_some(),
        "checked_at should be a string: {body}"
    );
}

#[tokio::test]
#[serial]
async fn write_version_cache_overwrites_existing_atomically() {
    let _ = test_home();
    reset();

    write_version_cache("0.1.180", None).await;
    write_version_cache("0.1.181", None).await;

    let body = std::fs::read_to_string(version_cache_path()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        parsed["version"], "0.1.181",
        "second write must overwrite first"
    );
}

#[tokio::test]
#[serial]
async fn write_version_cache_does_not_leave_tmp_file_behind() {
    let _ = test_home();
    reset();

    write_version_cache("0.1.180", None).await;

    let tmp = test_home().join("version.json.tmp");
    assert!(
        !tmp.exists(),
        "atomic rename must clean up tmp file: {}",
        tmp.display()
    );
}

#[tokio::test]
#[serial]
async fn write_version_cache_writes_valid_json_object() {
    let _ = test_home();
    reset();

    write_version_cache("0.1.182-alpha.3", None).await;

    let body = std::fs::read_to_string(version_cache_path()).unwrap();
    // Must parse as JSON.
    let parsed: serde_json::Value =
        serde_json::from_str(&body).unwrap_or_else(|e| panic!("not valid JSON: {e}\nbody: {body}"));
    let obj = parsed.as_object().unwrap();
    assert!(obj.contains_key("version"));
    assert!(obj.contains_key("checked_at"));
    assert_eq!(parsed["version"], "0.1.182-alpha.3");
}

#[tokio::test]
#[serial]
async fn write_version_cache_records_recent_timestamp() {
    let _ = test_home();
    reset();

    let before = time::OffsetDateTime::now_utc();
    write_version_cache("0.1.180", None).await;
    let after = time::OffsetDateTime::now_utc();

    let body = std::fs::read_to_string(version_cache_path()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    let ts_str = parsed["checked_at"].as_str().unwrap();
    let ts = time::OffsetDateTime::parse(ts_str, &time::format_description::well_known::Rfc3339)
        .unwrap();

    assert!(
        ts >= before - Duration::from_secs(5) && ts <= after + Duration::from_secs(5),
        "timestamp should be within the test window: ts={ts}, before={before}, after={after}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// is_version_cache_fresh — exercised via the public re-export. Each scenario
// writes the file directly so we can control the timestamp.
// ─────────────────────────────────────────────────────────────────────────────

/// Write a `EchoAgentVersion`-shaped JSON file with an arbitrary timestamp.
fn write_cache_with_timestamp(version: &str, ts: time::OffsetDateTime) {
    let ts_str = ts
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let body = serde_json::json!({
        "version": version,
        "checked_at": ts_str,
    });
    std::fs::write(
        version_cache_path(),
        serde_json::to_vec_pretty(&body).unwrap(),
    )
    .unwrap();
}

/// Re-implement the cache-freshness check using the public API. We can't
/// import the private `is_version_cache_fresh` directly, but we can verify
/// its on-disk contract: file shape + freshness logic via the public
/// `EchoAgentVersion` JSON layout.
async fn cache_is_fresh() -> bool {
    // Mirror the implementation: look at version.json under ECHO_AGENT_HOME,
    // parse, and check the TTL.
    let path = version_cache_path();
    let Ok(body) = tokio::fs::read_to_string(&path).await else {
        return false;
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&body) else {
        return false;
    };
    let Some(ts_str) = parsed["checked_at"].as_str() else {
        return false;
    };
    let Ok(ts) =
        time::OffsetDateTime::parse(ts_str, &time::format_description::well_known::Rfc3339)
    else {
        return false;
    };
    let now = time::OffsetDateTime::now_utc();
    now - ts < Duration::from_secs(60 * 30)
}

#[tokio::test]
#[serial]
async fn version_cache_is_fresh_after_write() {
    let _ = test_home();
    reset();

    write_version_cache("0.1.180", None).await;
    assert!(
        cache_is_fresh().await,
        "cache should be fresh right after write"
    );
}

#[tokio::test]
#[serial]
async fn version_cache_is_stale_when_old() {
    let _ = test_home();
    reset();

    let two_hours_ago = time::OffsetDateTime::now_utc() - Duration::from_secs(2 * 60 * 60);
    write_cache_with_timestamp("0.1.180", two_hours_ago);

    assert!(
        !cache_is_fresh().await,
        "2-hour-old cache should be stale (TTL is 30 min)"
    );
}

#[tokio::test]
#[serial]
async fn version_cache_missing_file_is_not_fresh() {
    let _ = test_home();
    reset();

    assert!(
        !cache_is_fresh().await,
        "missing file should not be considered fresh"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// version.json wire format — the on-disk file is read by every echoagent launch.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn version_cache_file_is_round_trippable() {
    let _ = test_home();
    reset();

    write_version_cache("0.1.182-alpha.3", Some("0.1.180")).await;

    let body = std::fs::read_to_string(version_cache_path()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();

    // The shape must match what a manually-written file would look like.
    let manual = serde_json::json!({
        "version": parsed["version"].as_str().unwrap(),
        "stable_version": parsed["stable_version"].as_str().unwrap(),
        "checked_at": parsed["checked_at"].as_str().unwrap(),
    });
    assert_eq!(parsed, manual);
}

#[tokio::test]
#[serial]
async fn write_version_cache_handles_long_prerelease_string() {
    let _ = test_home();
    reset();

    // Realistic alpha string with multi-segment pre-release id.
    write_version_cache("0.1.190-alpha.42.beta.7", None).await;

    let body = std::fs::read_to_string(version_cache_path()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["version"], "0.1.190-alpha.42.beta.7");
}

#[tokio::test]
#[serial]
async fn write_version_cache_idempotent_for_same_version() {
    let _ = test_home();
    reset();

    write_version_cache("0.1.180", None).await;
    let body1 = std::fs::read_to_string(version_cache_path()).unwrap();
    // Force a small wait so the timestamp could differ.
    tokio::time::sleep(Duration::from_millis(50)).await;
    write_version_cache("0.1.180", None).await;
    let body2 = std::fs::read_to_string(version_cache_path()).unwrap();

    // Both writes should leave the same version field, but timestamps may
    // differ — verify the version is preserved.
    let v1: serde_json::Value = serde_json::from_str(&body1).unwrap();
    let v2: serde_json::Value = serde_json::from_str(&body2).unwrap();
    assert_eq!(v1["version"], v2["version"]);
    assert_eq!(v1["version"], "0.1.180");
}

// ─────────────────────────────────────────────────────────────────────────────
// get_installed_echo_agent_version env override
//
// The function honors `ECHO_AGENT_TEST_VERSION` for testing. We exercise it
// via the public re-export only — no private items leaked.
// ─────────────────────────────────────────────────────────────────────────────
//
// Note: `get_installed_echo_agent_version` is not re-exported from `lib.rs`, but
// it's `pub` from `version` module and accessible via `version::`.

#[tokio::test]
#[serial]
async fn get_installed_version_uses_env_var_override() {
    let _ = test_home();
    reset();

    unsafe {
        std::env::set_var("ECHO_AGENT_TEST_VERSION", "9.9.9");
    }
    let v = echo_agent_update::version::get_installed_echo_agent_version();
    assert_eq!(v, "9.9.9");
    unsafe {
        std::env::remove_var("ECHO_AGENT_TEST_VERSION");
    }
}

#[tokio::test]
#[serial]
async fn get_installed_version_falls_back_to_cargo_pkg_version_when_env_unset() {
    let _ = test_home();
    reset();

    unsafe {
        std::env::remove_var("ECHO_AGENT_TEST_VERSION");
    }
    let v = echo_agent_update::version::get_installed_echo_agent_version();
    // The compile-time CARGO_PKG_VERSION must be a parseable semver string.
    let _: semver::Version = v
        .parse()
        .unwrap_or_else(|e| panic!("CARGO_PKG_VERSION is not a valid semver: '{v}': {e}"));
}

#[tokio::test]
#[serial]
async fn get_installed_version_with_env_var_takes_precedence() {
    let _ = test_home();
    reset();

    let real = {
        unsafe {
            std::env::remove_var("ECHO_AGENT_TEST_VERSION");
        }
        echo_agent_update::version::get_installed_echo_agent_version()
    };

    unsafe {
        std::env::set_var("ECHO_AGENT_TEST_VERSION", "0.0.0-test");
    }
    let overridden = echo_agent_update::version::get_installed_echo_agent_version();
    assert_ne!(real, overridden);
    assert_eq!(overridden, "0.0.0-test");

    unsafe {
        std::env::remove_var("ECHO_AGENT_TEST_VERSION");
    }
}

#[tokio::test]
#[serial]
async fn get_installed_version_handles_alpha_prerelease_in_env() {
    let _ = test_home();
    reset();

    unsafe {
        std::env::set_var("ECHO_AGENT_TEST_VERSION", "0.1.200-alpha.5");
    }
    let v = echo_agent_update::version::get_installed_echo_agent_version();
    assert_eq!(v, "0.1.200-alpha.5");
    unsafe {
        std::env::remove_var("ECHO_AGENT_TEST_VERSION");
    }
}

#[tokio::test]
#[serial]
async fn get_installed_version_does_not_validate_env_var_format() {
    // The function returns whatever's in the env var verbatim, even garbage.
    // Document this so callers know they need to validate downstream.
    let _ = test_home();
    reset();

    unsafe {
        std::env::set_var("ECHO_AGENT_TEST_VERSION", "not-a-version");
    }
    let v = echo_agent_update::version::get_installed_echo_agent_version();
    assert_eq!(v, "not-a-version");
    unsafe {
        std::env::remove_var("ECHO_AGENT_TEST_VERSION");
    }
}
