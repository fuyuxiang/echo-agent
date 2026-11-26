// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

const CONSENT_TITLE: &str = "ZZCONSENTTITLE";
const CONSENT_BODY: &str = "ZZCONSENTBODY";
const ACCEPT_LABEL: &str = "ZZACCEPT";

fn consent_settings() -> serde_json::Value {
    serde_json::json!({
        "allow_access": true,
        "consent_gate": {
            "id": "pty-consent",
            "version": 1,
            "title": CONSENT_TITLE,
            "body": format!("{CONSENT_BODY} [Terms](https://echo-agent.invalid/legal/tos)."),
            "accept_label": ACCEPT_LABEL,
        },
    })
}

/// Settings must be in place before the spawn: the gate is seeded from the startup prefetch.
fn spawn_gated(content: &ContentController, oauth_user: &str) -> PtyHarness {
    spawn_serving(content, oauth_user, consent_settings())
}

fn spawn_serving(
    content: &ContentController,
    oauth_user: &str,
    settings: serde_json::Value,
) -> PtyHarness {
    content.server().set_settings(settings);
    seed_fake_oauth(content, oauth_user);

    let binary = pager_binary().expect("resolve pager binary");
    PtyHarness::spawn_with_content_env_ops_in_dir(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        content,
        &[],
        &oauth_credential_ops(),
        Some(content.home()),
    )
    .expect("spawn pager against the served settings")
}

/// `Ctrl+N` is the case worth pinning: it bypasses the welcome input interceptor entirely, so only
/// the `session_startup_allowed()` chokepoint stops it.
/// The sibling folder-trust test guards the same leak for its own gate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn consent_gate_renders_and_blocks_new_session() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} past the notice."));
    let mut harness = spawn_gated(&content, "pty-consent-blocks");

    harness
        .wait_for_text(CONSENT_TITLE, WELCOME_TIMEOUT)
        .expect("consent notice renders before any session");

    assert!(
        harness.contains_text(CONSENT_BODY),
        "the body must paint, not just the title\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.inject_keys(b"\x0e").expect("inject Ctrl+N"); // arm
    harness.inject_keys(b"\x0e").expect("inject Ctrl+N"); // confirm
    harness.update(Duration::from_millis(400));

    assert!(
        harness.contains_text(CONSENT_TITLE),
        "Ctrl+N must not start a session while consent is Pending\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}

/// No served notice means no gate, which is the fail-open path every launch without settings takes:
/// the client must stay fully usable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn absent_consent_gate_shows_no_notice() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} no notice."));
    let mut harness = spawn_serving(
        &content,
        "pty-consent-absent",
        serde_json::json!({ "allow_access": true }),
    );

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome renders");

    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit a prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("no gate was served, so startup must not be blocked");

    assert!(
        !harness.contains_text(CONSENT_TITLE),
        "no notice was served, so none must paint\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}

/// Answer once and the same notice never asks again; a bumped version asks again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn an_answered_notice_does_not_ask_again() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} past the notice."));
    let mut first = spawn_gated(&content, "pty-consent-relaunch");

    first
        .wait_for_text(ACCEPT_LABEL, WELCOME_TIMEOUT)
        .expect("notice renders");

    first.inject_keys(b"a").expect("inject a");
    first
        .wait_for_text_absent(CONSENT_TITLE, WELCOME_TIMEOUT)
        .expect("notice clears");

    // The write is a spawned task, so wait for it rather than assume it landed before exit.
    let config = content.sandbox().echo_agent_home().join("config.toml");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if std::fs::read_to_string(&config).is_ok_and(|c| c.contains("pty-consent")) {
            break;
        }
        first.update(Duration::from_millis(100));
    }

    let recorded = std::fs::read_to_string(&config).unwrap_or_default();
    assert!(
        recorded.contains("[consent.answers"),
        "the answer must reach config.toml:\n{recorded}"
    );

    first.quit().expect("clean quit");

    let mut second = spawn_gated(&content, "pty-consent-relaunch");

    second
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome renders");

    second
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit a prompt");
    second
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("an answered notice must not block the next launch");

    assert!(
        !second.contains_text(CONSENT_TITLE),
        "an answered notice must not ask again\nscreen:\n{}",
        second.screen_contents()
    );

    second.quit().expect("clean quit");

    // A new version is a new question.
    let mut settings = consent_settings();
    settings["consent_gate"]["version"] = serde_json::json!(2);
    let mut third = spawn_serving(&content, "pty-consent-relaunch", settings);

    third
        .wait_for_text(CONSENT_TITLE, WELCOME_TIMEOUT)
        .expect("a bumped version must ask again");

    third.quit().expect("clean quit");
}
