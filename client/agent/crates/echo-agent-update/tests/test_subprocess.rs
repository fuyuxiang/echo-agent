//! Subprocess-based integration tests using fake `npm` / `gh` shell scripts
//! placed first on `PATH`.
//!
//! `auto_update::install_npm` and `version::fetch_npm_tag` spawn `npm` by
//! bare name (`Command::new("npm")`). To test them without touching the real
//! npm registry, we install a tempdir-resident shell script named `npm`
//! that logs its args and prints canned stdout, then prepend that tempdir
//! to `PATH` for the duration of the test.
//!
//! Same pattern for `gh` for the `gh-release` installer paths.
//!
//! All tests in this file mutate `PATH` (global), so they're serialized with
//! `#[serial]`.

#![cfg(unix)]

mod common;

use std::time::Duration;

use serial_test::serial;

use common::FakeBinGuard;
use echo_agent_update::auto_update::install_npm_for_test;
use echo_agent_update::version::{
    fetch_gh_release_version, fetch_npm_tag_for_test, fetch_npm_version_for_test,
};

// ─────────────────────────────────────────────────────────────────────────────
// fetch_npm_tag — reads a single dist-tag from `npm view`.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn fetch_npm_tag_returns_string_response() {
    let g = FakeBinGuard::install_npm();
    g.set_stdout("\"0.1.181\"\n");

    let v = fetch_npm_tag_for_test("latest", None).await.unwrap();
    assert_eq!(v, "0.1.181");
}

#[tokio::test]
#[serial]
async fn fetch_npm_tag_returns_array_response_picks_last() {
    // npm view sometimes returns an array of versions for ambiguous specs.
    // The implementation picks the LAST one (rev().find_map).
    let g = FakeBinGuard::install_npm();
    g.set_stdout(r#"["0.1.179", "0.1.180", "0.1.181"]"#);

    let v = fetch_npm_tag_for_test("latest", None).await.unwrap();
    assert_eq!(v, "0.1.181");
}

#[tokio::test]
#[serial]
async fn fetch_npm_tag_passes_pkg_and_tag_to_npm() {
    let g = FakeBinGuard::install_npm();
    g.set_stdout("\"0.1.181\"");

    let _ = fetch_npm_tag_for_test("latest", None).await.unwrap();
    let log = g.args_log();
    assert_eq!(log.len(), 1, "exactly one npm invocation");
    let args = &log[0];
    assert!(args.contains("view"), "args: {args}");
    // For "latest" tag, no `@latest` suffix is appended in pkg_spec.
    assert!(
        args.contains("@echo-agent-official/echoagent"),
        "args: {args}"
    );
    assert!(!args.contains("@latest"), "args: {args}");
    assert!(args.contains("--json"), "args: {args}");
}

#[tokio::test]
#[serial]
async fn fetch_npm_tag_alpha_appends_at_alpha_suffix() {
    let g = FakeBinGuard::install_npm();
    g.set_alpha_stdout("\"0.1.181-alpha.1\"");

    let v = fetch_npm_tag_for_test("alpha", None).await.unwrap();
    assert_eq!(v, "0.1.181-alpha.1");

    let log = g.args_log();
    assert!(
        log[0].contains("@echo-agent-official/echoagent@alpha"),
        "args: {}",
        log[0]
    );
}

#[tokio::test]
#[serial]
async fn fetch_npm_tag_passes_registry_flag_when_set() {
    let g = FakeBinGuard::install_npm();
    g.set_stdout("\"0.1.181\"");

    let _ = fetch_npm_tag_for_test("latest", Some("https://npm.example.com"))
        .await
        .unwrap();
    let log = g.args_log();
    assert!(
        log[0].contains("--registry=https://npm.example.com"),
        "args: {}",
        log[0]
    );
}

#[tokio::test]
#[serial]
async fn fetch_npm_tag_no_registry_flag_when_unset() {
    let g = FakeBinGuard::install_npm();
    g.set_stdout("\"0.1.181\"");

    let _ = fetch_npm_tag_for_test("latest", None).await.unwrap();
    let log = g.args_log();
    assert!(!log[0].contains("--registry"), "args: {}", log[0]);
}

#[tokio::test]
#[serial]
async fn fetch_npm_tag_propagates_npm_failure() {
    let g = FakeBinGuard::install_npm();
    g.set_exit_code(1);

    let err = fetch_npm_tag_for_test("latest", None).await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("npm view"), "msg: {msg}");
    assert!(msg.contains("failed"), "msg: {msg}");
}

#[tokio::test]
#[serial]
async fn fetch_npm_tag_invalid_json_returns_err() {
    let g = FakeBinGuard::install_npm();
    g.set_stdout("not valid json {");

    let err = fetch_npm_tag_for_test("latest", None).await.unwrap_err();
    // serde_json should error on this.
    let msg = format!("{err:#}");
    assert!(!msg.is_empty());
}

#[tokio::test]
#[serial]
async fn fetch_npm_tag_unexpected_json_shape_returns_err() {
    // npm view can return null, an object, etc. The function expects string
    // or array of strings — anything else is an error.
    let g = FakeBinGuard::install_npm();
    g.set_stdout("42");

    let err = fetch_npm_tag_for_test("latest", None).await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("unexpected JSON"), "msg: {msg}");
}

#[tokio::test]
#[serial]
async fn fetch_npm_tag_empty_array_returns_err() {
    let g = FakeBinGuard::install_npm();
    g.set_stdout("[]");

    let err = fetch_npm_tag_for_test("latest", None).await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("empty"), "msg: {msg}");
}

// ─────────────────────────────────────────────────────────────────────────────
// fetch_npm_version — alpha channel calls both tags and returns the max.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn fetch_npm_version_stable_calls_only_latest() {
    let g = FakeBinGuard::install_npm();
    g.set_stdout("\"0.1.181\"");

    let v = fetch_npm_version_for_test("stable", None).await.unwrap();
    assert_eq!(v, "0.1.181");
    assert_eq!(g.args_log().len(), 1, "stable should make one call");
}

#[tokio::test]
#[serial]
async fn fetch_npm_version_alpha_returns_max_of_alpha_and_latest_when_alpha_higher() {
    let g = FakeBinGuard::install_npm();
    g.set_stdout("\"0.1.181\""); // latest tag → stable
    g.set_alpha_stdout("\"0.1.182-alpha.1\""); // alpha tag

    let v = fetch_npm_version_for_test("alpha", None).await.unwrap();
    assert_eq!(v, "0.1.182-alpha.1");
    assert_eq!(g.args_log().len(), 2, "alpha should make two calls");
}

#[tokio::test]
#[serial]
async fn fetch_npm_version_alpha_returns_stable_when_higher() {
    // Common case: stable shipped after a stale alpha tag — must not strand
    // alpha users on the older alpha.
    let g = FakeBinGuard::install_npm();
    g.set_stdout("\"0.1.182\"");
    g.set_alpha_stdout("\"0.1.181-alpha.1\"");

    let v = fetch_npm_version_for_test("alpha", None).await.unwrap();
    assert_eq!(v, "0.1.182");
}

// ─────────────────────────────────────────────────────────────────────────────
// install_npm — spawns `npm i -g @pkg@version`.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn install_npm_calls_npm_with_version_arg() {
    let g = FakeBinGuard::install_npm();
    // No stdout/exit setup → succeeds with empty stdout.

    install_npm_for_test(Some("0.1.181"), "stable", None).unwrap();
    let log = g.args_log();
    assert_eq!(log.len(), 1, "exactly one npm invocation");
    let args = &log[0];
    assert!(args.contains("i -g"), "args: {args}");
    assert!(
        args.contains("@echo-agent-official/echoagent@0.1.181"),
        "args: {args}"
    );
}

#[tokio::test]
#[serial]
async fn install_npm_falls_back_to_dist_tag_on_no_target() {
    let g = FakeBinGuard::install_npm();

    install_npm_for_test(None, "stable", None).unwrap();
    let log = g.args_log();
    assert!(
        log[0].contains("@echo-agent-official/echoagent@latest"),
        "stable channel uses @latest dist-tag: {}",
        log[0]
    );
}

#[tokio::test]
#[serial]
async fn install_npm_falls_back_to_alpha_dist_tag_on_alpha_channel() {
    let g = FakeBinGuard::install_npm();

    install_npm_for_test(None, "alpha", None).unwrap();
    let log = g.args_log();
    assert!(
        log[0].contains("@echo-agent-official/echoagent@alpha"),
        "alpha channel uses @alpha dist-tag: {}",
        log[0]
    );
}

#[tokio::test]
#[serial]
async fn install_npm_passes_registry_flag_when_set() {
    let g = FakeBinGuard::install_npm();

    install_npm_for_test(Some("0.1.181"), "stable", Some("https://npm.example.com")).unwrap();
    let log = g.args_log();
    assert!(
        log[0].contains("--registry=https://npm.example.com"),
        "args: {}",
        log[0]
    );
}

#[tokio::test]
#[serial]
async fn install_npm_no_registry_flag_when_unset() {
    let g = FakeBinGuard::install_npm();

    install_npm_for_test(Some("0.1.181"), "stable", None).unwrap();
    let log = g.args_log();
    assert!(!log[0].contains("--registry"), "args: {}", log[0]);
}

#[tokio::test]
#[serial]
async fn install_npm_returns_err_on_npm_failure() {
    let g = FakeBinGuard::install_npm();
    g.set_exit_code(1);

    let err = install_npm_for_test(Some("0.1.181"), "stable", None).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("npm install failed"), "msg: {msg}");
}

#[tokio::test]
#[serial]
async fn install_npm_with_token_passes_userconfig() {
    // SAFETY: serial_test ensures no other thread touches NPM_TOKEN.
    unsafe { std::env::set_var("NPM_TOKEN", "secrettoken") };
    let g = FakeBinGuard::install_npm();

    install_npm_for_test(Some("0.1.181"), "stable", None).unwrap();
    let log = g.args_log();
    assert!(
        log[0].contains("--userconfig="),
        "with NPM_TOKEN, must pass --userconfig: {}",
        log[0]
    );
    // The userconfig path should be cleaned up afterwards.
    let userconfig_arg = log[0]
        .split_whitespace()
        .find(|a| a.starts_with("--userconfig="))
        .unwrap()
        .trim_start_matches("--userconfig=");
    assert!(
        !std::path::Path::new(userconfig_arg).exists(),
        "userconfig file should be cleaned up: {userconfig_arg}"
    );
    unsafe { std::env::remove_var("NPM_TOKEN") };
}

#[tokio::test]
#[serial]
async fn install_npm_no_token_no_userconfig() {
    unsafe { std::env::remove_var("NPM_TOKEN") };
    let g = FakeBinGuard::install_npm();

    install_npm_for_test(Some("0.1.181"), "stable", None).unwrap();
    let log = g.args_log();
    assert!(!log[0].contains("--userconfig"), "args: {}", log[0]);
}

// ─────────────────────────────────────────────────────────────────────────────
// fetch_gh_release_version — exercises the `gh release list` shell-out.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn fetch_gh_release_stable_returns_tag_stripped() {
    let g = FakeBinGuard::install_gh();
    // For stable channel, only the `--exclude-pre-releases` invocation is made.
    g.set_stable_only_stdout("v0.1.181\n");

    let v = fetch_gh_release_version("stable").await.unwrap();
    assert_eq!(v, "0.1.181");

    let log = g.args_log();
    assert_eq!(log.len(), 1);
    assert!(
        log[0].contains("--exclude-pre-releases"),
        "args: {}",
        log[0]
    );
}

#[tokio::test]
#[serial]
async fn fetch_gh_release_stable_handles_tag_without_v_prefix() {
    let g = FakeBinGuard::install_gh();
    g.set_stable_only_stdout("0.1.181");

    let v = fetch_gh_release_version("stable").await.unwrap();
    assert_eq!(v, "0.1.181");
}

#[tokio::test]
#[serial]
async fn fetch_gh_release_alpha_returns_max_of_pre_and_stable() {
    // Alpha channel makes two `gh release list` calls (with and without
    // --exclude-pre-releases) and returns the semver-max.
    let g = FakeBinGuard::install_gh();
    g.set_with_pre_stdout("v0.1.182-alpha.1");
    g.set_stable_only_stdout("v0.1.181");

    let v = fetch_gh_release_version("alpha").await.unwrap();
    assert_eq!(v, "0.1.182-alpha.1");
    assert_eq!(g.args_log().len(), 2);
}

#[tokio::test]
#[serial]
async fn fetch_gh_release_alpha_returns_stable_when_higher() {
    let g = FakeBinGuard::install_gh();
    g.set_with_pre_stdout("v0.1.180-alpha.5");
    g.set_stable_only_stdout("v0.1.181");

    let v = fetch_gh_release_version("alpha").await.unwrap();
    assert_eq!(v, "0.1.181");
}

#[tokio::test]
#[serial]
async fn fetch_gh_release_propagates_gh_failure() {
    let g = FakeBinGuard::install_gh();
    g.set_exit_code(1);

    let err = fetch_gh_release_version("stable").await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("gh release list"), "msg: {msg}");
    assert!(msg.contains("failed"), "msg: {msg}");
}

#[tokio::test]
#[serial]
async fn fetch_gh_release_empty_response_returns_err() {
    let g = FakeBinGuard::install_gh();
    g.set_stable_only_stdout("");

    let err = fetch_gh_release_version("stable").await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("No releases found"), "msg: {msg}");
}

#[tokio::test]
#[serial]
async fn fetch_gh_release_passes_repo_flag() {
    let g = FakeBinGuard::install_gh();
    g.set_stable_only_stdout("v0.1.181");

    let _ = fetch_gh_release_version("stable").await.unwrap();
    let log = g.args_log();
    assert!(log[0].contains("--repo"), "args: {}", log[0]);
    assert!(
        log[0].contains("fuyuxiang/echo-agent-desktop"),
        "args: {}",
        log[0]
    );
}

#[tokio::test]
#[serial]
async fn fetch_gh_release_uses_jq_to_extract_tag() {
    // The function constructs `gh release list --json tagName --jq '.[0].tagName'`
    // — we verify the args include the jq filter so a refactor doesn't accidentally
    // drop it.
    let g = FakeBinGuard::install_gh();
    g.set_stable_only_stdout("v0.1.181");

    let _ = fetch_gh_release_version("stable").await.unwrap();
    let log = g.args_log();
    assert!(log[0].contains("--json"), "args: {}", log[0]);
    assert!(log[0].contains("--jq"), "args: {}", log[0]);
}

#[tokio::test]
#[serial]
async fn fetch_gh_release_does_not_hang_on_quick_responses() {
    // Sanity: every call should return well under our test timeout.
    let g = FakeBinGuard::install_gh();
    g.set_stable_only_stdout("v0.1.181");

    let res =
        tokio::time::timeout(Duration::from_secs(5), fetch_gh_release_version("stable")).await;
    assert!(res.is_ok(), "should not hang");
}
