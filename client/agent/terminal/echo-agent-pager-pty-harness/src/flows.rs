//! Cross-suite e2e flow helpers over [`PtyHarness`] / [`ContentController`].
//!
//! The single canonical home for driving/seeding helpers shared by the
//! pager's `pty_e2e` and `leader_pty_e2e` test targets (both depend on this
//! crate); suite-local constants (sizes, sentinels, timeouts) stay in each
//! suite's `common.rs`.

use std::time::{Duration, Instant};

use crate::{ContentController, PtyHarness};

/// Pump PTY output until every label is absent from the visible screen.
pub fn wait_for_labels_absent(h: &mut PtyHarness, labels: &[&str], timeout: Duration) {
    let _ = h.wait_until("screen labels to disappear", timeout, |h| {
        labels.iter().all(|label| !h.contains_text(label))
    });
}

/// Submit `prompt` from `h`, then keep re-pressing Enter until the turn
/// actually starts streaming (`sentinel` appears) or `timeout` elapses.
///
/// In a heavy multi-client leader cluster the driver's submit Enter can be
/// dropped when it races the other client attaching / replaying on the shared
/// leader: the typed prompt is left sitting unsubmitted in the composer, the
/// turn never starts, and a plain `wait_for_text` then times out (the observed
/// `leader_two_clients_shared_session` flake — A idle with `again` still in the
/// composer at 75s). Re-pressing Enter is safe and idempotent: submitting takes
/// the composer draft synchronously (`std::mem::take` in `dispatch`), so once a
/// turn has really been sent the composer is empty and an extra Enter is a
/// no-op. It can only submit a still-stuck prompt, never double-submit a sent
/// one (which would break exactly-once scrollback asserts).
pub fn submit_turn(h: &mut PtyHarness, prompt: &str, sentinel: &str, timeout: Duration) {
    h.inject_keys(format!("{prompt}\r").as_bytes())
        .expect("inject prompt submit");
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        // Per-attempt sub-budget, generous enough that a genuinely in-flight
        // submit resolves before we re-nudge (so the re-nudge only ever fires
        // on an empty composer, where it is a no-op).
        if h.wait_for_text(sentinel, Duration::from_secs(10).min(remaining))
            .is_ok()
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out after {timeout:?} waiting for {sentinel:?}\nscreen:\n{}",
            h.screen_contents()
        );
        let _ = h.inject_keys(b"\r");
    }
}

/// Count only inference requests (chat completions / responses / messages),
/// ignoring incidental GETs like /v1/models and /v1/settings, so a replay
/// invariant means "no turn was re-driven" rather than "no HTTP at all".
pub fn inference_request_count(content: &ContentController) -> usize {
    content
        .requests()
        .iter()
        .filter(|e| {
            e.path.contains("/chat/completions")
                || e.path.contains("/responses")
                || e.path.contains("/messages")
        })
        .count()
}

/// Seed a fake EchoAgent OAuth entry into the isolated home's `auth.json` so the
/// shell has session auth (the harness's `ECHO_AGENT_API_KEY` is ApiKey/BYOK mode
/// and never enters the auth manager). Load-bearing details: the scope key
/// must be `<issuer>::<client_id>`, `auth_mode` must be `oidc`,
/// `expires_at` must be far-future so no network refresh is attempted, and
/// `coding_data_retention_opt_out` must be `false` so collection/upload-path
/// e2es (e.g. storage park-on-401) still enqueue traces — missing that field
/// now deserializes as opted-out via
/// `default_coding_data_retention_opt_out()`. The mock server accepts any
/// bearer. Pair with [`oauth_credential_ops`].
pub fn seed_fake_oauth(content: &ContentController, user: &str) {
    seed_fake_oauth_with_opt_out(content, user, false);
}

/// Like [`seed_fake_oauth`], but with `coding_data_retention_opt_out: true` —
/// the auth-side precondition for the coding-data privacy upsell banner.
pub fn seed_fake_oauth_coding_data_opted_out(content: &ContentController, user: &str) {
    seed_fake_oauth_with_opt_out(content, user, true);
}

/// Like [`seed_fake_oauth_coding_data_opted_out`], but on a Zero Data
/// Retention team (`team_blocked_reasons` carries `BLOCKED_REASON_NO_LOGS`,
/// the shell's `EchoAgentAuth::is_zdr_team` trigger) — locks the settings modal's
/// `coding_data_sharing` row to `ZDR` and suppresses the privacy banner.
pub fn seed_fake_oauth_zdr_team(content: &ContentController, user: &str) {
    seed_fake_oauth_raw(
        content,
        user,
        true,
        ",\n    \"team_name\": \"PTY ZDR Team\",\n    \"team_role\": \"MEMBER\",\n    \
         \"team_blocked_reasons\": [\"BLOCKED_REASON_NO_LOGS\"]",
    );
}

/// Like [`seed_fake_oauth_coding_data_opted_out`], but as a non-admin member
/// of a (non-ZDR) team — locks the settings modal's `coding_data_sharing`
/// row to `Opt out · Admin Managed` and suppresses the privacy banner.
pub fn seed_fake_oauth_team_member(content: &ContentController, user: &str) {
    seed_fake_oauth_raw(
        content,
        user,
        true,
        ",\n    \"team_name\": \"PTY Team\",\n    \"team_role\": \"MEMBER\"",
    );
}

fn seed_fake_oauth_with_opt_out(content: &ContentController, user: &str, opted_out: bool) {
    seed_fake_oauth_raw(content, user, opted_out, "");
}

/// Shared auth.json template writer. `team_fields` is a raw JSON fragment
/// spliced after `coding_data_retention_opt_out` (empty = no team; field
/// names must match the shell's `EchoAgentAuth` serde names in
/// `echo-agent-runtime/src/auth/model.rs`).
fn seed_fake_oauth_raw(
    content: &ContentController,
    user: &str,
    opted_out: bool,
    team_fields: &str,
) {
    let echo_agent_home = content.home().join(".echo-agent");
    std::fs::create_dir_all(&echo_agent_home).expect("create temp .echo-agent");
    std::fs::write(
        echo_agent_home.join("auth.json"),
        format!(
            r#"{{
  "https://auth.echo-agent.invalid::b1a00492-073a-47ea-816f-4c329264a828": {{
    "key": "pty-test-oauth-token",
    "auth_mode": "oidc",
    "create_time": "2026-01-01T00:00:00Z",
    "user_id": "{user}",
    "email": "{user}@test.invalid",
    "expires_at": "2030-01-01T00:00:00Z",
    "refresh_token": "pty-test-refresh-token",
    "oidc_issuer": "https://auth.echo-agent.invalid",
    "oidc_client_id": "b1a00492-073a-47ea-816f-4c329264a828",
    "coding_data_retention_opt_out": {opted_out}{team_fields}
  }}
}}"#
        ),
    )
    .expect("seed fake oauth auth.json");
}

/// Remove only the sandbox's fake API-key credential, allowing the `auth.json`
/// entry written by [`seed_fake_oauth`] to determine the advertised auth method.
pub fn oauth_credential_ops() -> [crate::EnvOp<'static>; 1] {
    [crate::EnvOp::remove("ECHO_AGENT_API_KEY")]
}

/// Drive `/new` until `model` shows on screen. Campaigns apply to **new
/// sessions only** and the pager's settings prefetch is deliberately 2s-capped,
/// so on a loaded runner the first session can legitimately open pre-campaign;
/// each `/new` after the settings fetch lands re-resolves with the campaign.
pub fn wait_for_model_via_new_sessions(h: &mut PtyHarness, model: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if h.contains_text(model) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        let _ = h.inject_keys(b"/new\r");
        h.update(Duration::from_millis(3000));
    }
}
