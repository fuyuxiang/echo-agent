// Per-test-case module for the `pty_e2e` integration test crate.
//
// End-to-end coverage for free→paid subscription auto-detection
// (`src/app/subscription.rs`).
#[allow(unused_imports)]
use super::common::*;

/// Distinctive gate copy (unlikely to collide with welcome chrome).
const GATE_MSG: &str = "ZZSUBGATEMSG";

/// A tier in the shell's `QUALIFYING_TIERS` list.
const PAID_TIER: &str = "EchoAgentPro";

/// Display name delivered via `/settings` `subscription_tier_display`.
const PAID_TIER_DISPLAY: &str = "EchoAgent Pro";

/// Count of live subscription checks the client made against the mock
/// (`GET /v1/user?include=subscription`). Plain `/v1/user` enrichment
/// fetches are deliberately excluded.
fn user_check_count(content: &ContentController) -> usize {
    content
        .requests()
        .iter()
        .filter(|e| e.path.starts_with("/v1/user?") && e.path.contains("include=subscription"))
        .count()
}

/// Count of `GET /v1/settings` fetches (the qualifying-tier check refetches
/// settings, so a post-upgrade increase marks detection completing).
fn settings_count(content: &ContentController) -> usize {
    content
        .requests()
        .iter()
        .filter(|e| e.path == "/v1/settings")
        .count()
}

/// Count of `GET /v1/models` catalog fetches. Post-upgrade the shell must
/// re-fetch so tier-targeted models land without restart.
fn models_count(content: &ContentController) -> usize {
    content
        .requests()
        .iter()
        .filter(|e| e.path == "/v1/models" || e.path.starts_with("/v1/models?"))
        .count()
}

/// Paid-only model id used to prove the post-unblock catalog actually
/// replaced the free list in the picker (not merely that `/v1/models` was hit).
const PAID_ONLY_MODEL: &str = "composer-paid-only";

/// Minimal unsigned JWT with a `tier` claim matching [`PAID_TIER`].
///
/// Proto `prod_auth.SubscriptionTier`: 5 = `echoagent_heavy` = live
/// `/user` string `EchoAgentPro`. Must match
/// `jwt_claim_matches_user_subscription_tier` or post-unblock catalog
/// refresh treats the claim as stale and never re-fetches `/v1/models`
/// within the test timeout.
fn paid_tier_jwt() -> String {
    use base64::Engine;
    let enc = |v: &serde_json::Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string().as_bytes())
    };
    let header = enc(&json!({"alg": "none", "typ": "JWT"}));
    let payload = enc(&json!({"sub": "pty-subwatch", "exp": 2_000_000_000u64, "tier": 5}));
    format!("{header}.{payload}.sig")
}

/// Bind the fixed local-dev OIDC issuer (`http://localhost:22255`) and return a
/// paid-tier JWT on refresh. Call **after** free-tier watch polling so early
/// checks still see connection-refused (hermetic free path) and only the
/// post-upgrade refresh succeeds with a paid token.
///
/// Minimal raw HTTP (no axum dep in this crate): discovery + token only.
async fn start_local_oidc_paid_refresh() -> tokio::task::JoinHandle<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:22255")
        .await
        .expect("bind local OIDC issuer on :22255 for hermetic paid refresh");
    let paid_jwt = paid_tier_jwt();
    let discovery_body = serde_json::to_vec(&json!({
        "authorization_endpoint": "http://localhost:22255/authorize",
        "token_endpoint": "http://localhost:22255/token",
    }))
    .expect("discovery json");
    let token_body = serde_json::to_vec(&json!({
        "access_token": paid_jwt,
        "refresh_token": "pty-test-refresh-token-rotated",
        "expires_in": 3600,
    }))
    .expect("token json");

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let discovery_body = discovery_body.clone();
            let token_body = token_body.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let n = match socket.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                let req = String::from_utf8_lossy(&buf[..n]);
                let (status, body): (&str, &[u8]) =
                    if req.starts_with("GET /.well-known/openid-configuration") {
                        ("200 OK", discovery_body.as_slice())
                    } else if req.starts_with("POST /token") {
                        ("200 OK", token_body.as_slice())
                    } else {
                        ("404 Not Found", br"{}")
                    };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(resp.as_bytes()).await;
                let _ = socket.write_all(body).await;
            });
        }
    })
}

/// Pump the PTY until `cond` holds or `timeout` elapses (panics with a
/// screen dump on timeout).
fn pump_until(
    harness: &mut PtyHarness,
    timeout: Duration,
    mut cond: impl FnMut() -> bool,
    what: &str,
) {
    let deadline = Instant::now() + timeout;
    while !cond() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}\nscreen:\n{}",
            harness.screen_contents()
        );
        harness.update(Duration::from_millis(100));
    }
}

/// Like [`seed_fake_oauth`], but under the `ECHO_AGENT_LOCAL_AUTH` dev issuer
/// (`http://localhost:22255`). Two reasons: `is_echo_agent_oauth2_issuer()` accepts
/// the local issuer, so the subscription gate applies (an enterprise/unknown
/// issuer bypasses it); and the qualifying-tier JWT refresh then hits
/// `localhost:22255` — instant connection-refused instead of a real network
/// call to auth.echo-agent.invalid (hermetic, no CI-network flake). Pair with
/// `ECHO_AGENT_LOCAL_AUTH=1` in the spawn env so the shell's scope-key lookup
/// resolves this entry.
fn seed_fake_oauth_local_issuer(content: &ContentController, user: &str) {
    let echo_agent_home = content.home().join(".echo-agent");
    std::fs::create_dir_all(&echo_agent_home).expect("create temp .echo-agent");
    std::fs::write(
        echo_agent_home.join("auth.json"),
        format!(
            r#"{{
  "http://localhost:22255::b1a00492-073a-47ea-816f-4c329264a828": {{
    "key": "pty-test-oauth-token",
    "auth_mode": "oidc",
    "create_time": "2026-01-01T00:00:00Z",
    "user_id": "{user}",
    "email": "{user}@test.invalid",
    "expires_at": "2030-01-01T00:00:00Z",
    "refresh_token": "pty-test-refresh-token",
    "oidc_issuer": "http://localhost:22255",
    "oidc_client_id": "b1a00492-073a-47ea-816f-4c329264a828"
  }}
}}"#
        ),
    )
    .expect("seed fake local-issuer oauth auth.json");
}

/// Spawn the pager with local-issuer session auth (see
/// [`seed_fake_oauth_local_issuer`]) plus `extra_env`. Does NOT wait for the
/// welcome screen — gate tests assert on the very first paint.
fn spawn_subscription_pager(
    content: &ContentController,
    oauth_user: &str,
    extra_env: &[EnvOp<'_>],
) -> PtyHarness {
    seed_fake_oauth_local_issuer(content, oauth_user);
    let mut overrides = Vec::from(oauth_credential_ops());
    overrides.push(EnvOp::set("ECHO_AGENT_LOCAL_AUTH", "1"));
    overrides.extend_from_slice(extra_env);

    let binary = pager_binary().expect("resolve pager binary");
    PtyHarness::spawn_with_content_env_ops_in_dir(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        content,
        &[],
        &overrides,
        Some(content.home()),
    )
    .expect("spawn pager with subscription session auth")
}

/// [`spawn_subscription_pager`] driven into a live session
/// (welcome → prompt → mock response).
fn spawn_subscription_session(
    content: &ContentController,
    oauth_user: &str,
    extra_env: &[EnvOp<'_>],
) -> PtyHarness {
    let mut harness = spawn_subscription_pager(content, oauth_user, extra_env);
    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt to enter session");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("session response");
    harness
}

/// Watch cadence while free, upgrade detection, then dormancy once paid.
///
/// Also covers W-17: after free→paid unblock the shell refreshes the model
/// catalog with a **paid** JWT (mock IdP on `:22255`) and the paid-only model
/// id appears in the `/model` picker — not merely that `/v1/models` was called.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn subscription_watch_polls_free_tier_then_goes_dormant_after_upgrade() {
    // Start free-targeted (no paid-only model); swap after upgrade.
    // OIDC mock is started only after free-phase polling so early refresh
    // still connection-refuses (keeps the free watch path hermetic).
    let content = ContentController::start_with_models(vec![MockModel::new("echo-agent-3")])
        .await
        .expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} watch cadence."));
    // Free tier: the mock's /v1/user returns no subscription tier by default.
    let mut harness = spawn_subscription_session(
        &content,
        "pty-subwatch",
        &[EnvOp::set(
            "ECHO_AGENT_SUBSCRIPTION_WATCH_INTERVAL_SECS",
            "1",
        )],
    );

    // While free, the watch fires repeatedly at the (test-shrunk) cadence.
    pump_until(
        &mut harness,
        Duration::from_secs(30),
        || user_check_count(&content) >= 3,
        ">=3 live subscription checks while on the free tier",
    );

    // Now enable hermetic paid JWT refresh for the post-unblock catalog path.
    let oidc = start_local_oidc_paid_refresh().await;

    // Server-side upgrade: the live tier flips to a qualifying value and
    // settings now carry the paid display tier. Swap the model catalog *before*
    // recording models_before so an in-flight free fetch cannot falsely satisfy
    // the post-upgrade re-fetch wait.
    let settings_before = settings_count(&content);
    content.server().set_user_subscription_tier(Some(PAID_TIER));
    content.server().set_settings(json!({
        "allow_access": true,
        "subscription_tier_display": PAID_TIER_DISPLAY,
    }));
    content.server().set_models(vec![
        MockModel::new("echo-agent-3"),
        MockModel::new(PAID_ONLY_MODEL),
    ]);
    let models_before = models_count(&content);

    // Detection: the qualifying check refetches /v1/settings (that's how the
    // paid display tier reaches the client and disarms the watch).
    pump_until(
        &mut harness,
        Duration::from_secs(30),
        || settings_count(&content) > settings_before,
        "settings refetch after the qualifying tier was detected",
    );

    // W-17: after gate lift + successful paid JWT refresh the shell must
    // re-fetch /v1/models (fire-and-forget `on_auth_changed`).
    pump_until(
        &mut harness,
        Duration::from_secs(30),
        || models_count(&content) > models_before,
        "model catalog re-fetch after free→paid subscription unblock",
    );

    // Stronger than a GET count: switch to the paid-only model id. Status bar
    // shows it on success (same pattern as same_agent_type_switch_no_modal).
    harness
        .inject_keys(format!("/model {PAID_ONLY_MODEL}\r").as_bytes())
        .expect("switch to paid-only model");
    harness
        .wait_for_text(PAID_ONLY_MODEL, Duration::from_secs(20))
        .expect("paid-only model applied after upgrade catalog refresh");

    // Dormancy: once the paid tier lands, a full 6s quiet window (>=6
    // would-be ticks at the 1s cadence) passes with zero new checks.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let base = user_check_count(&content);
        let window_end = Instant::now() + Duration::from_secs(6);
        while Instant::now() < window_end {
            harness.update(Duration::from_millis(200));
        }
        if user_check_count(&content) == base {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "watch never went dormant after the upgrade (checks still firing)\nscreen:\n{}",
            harness.screen_contents()
        );
    }

    harness.quit().expect("clean quit");
    oidc.abort();
}

/// A genuinely-free user gated at startup still gets the paywall — but only
/// after a live subscription check confirmed the block (the gate is never
/// painted straight from the stale source).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn startup_gate_shows_paywall_for_free_user_after_live_check() {
    let content = ContentController::start().await.expect("start content");
    // Explicit deny + gate copy. Absent allow_access now fails open.
    content.server().set_settings(json!({
        "allow_access": false,
        "gate_message": GATE_MSG,
        "gate_url": "https://cloud.echo-agent.invalid/echoagent?referrer=echo-agent-build",
        "gate_label": "Subscribe",
    }));

    let mut harness = spawn_subscription_pager(&content, "pty-subgate-free", &[]);

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    // The verified gate renders. Normally the check response resolves the
    // deferral within seconds; the budget also covers the 30s hung-check
    // safety net under full-suite contention.
    harness
        .wait_for_text(GATE_MSG, Duration::from_secs(45))
        .expect("gate copy renders for a genuinely-free user");

    assert!(
        user_check_count(&content) >= 1,
        "a live subscription check must run before the paywall is shown; requests: {:?}",
        content
            .requests()
            .iter()
            .map(|e| e.path.clone())
            .collect::<Vec<_>>()
    );

    harness.quit().expect("clean quit");
}

/// Verify-before-paywall: a user who ALREADY subscribed never sees a
/// paywall flash when a stale gated settings snapshot reaches the client.
///
/// The stale snapshot is delivered via the `/new` settings refresh — the
/// only active `/v1/settings` consumer at that point (watch disabled via
/// env, announcements poll at its 5-min default, gate poll only runs while
/// gated, startup fetches settled). Queueing it at startup instead races
/// the shell's concurrent startup fetches: a slow gated fetch landing after
/// the verify check stores fresh settings can legitimately re-carry the
/// gate — a time-travel artifact of the scripted one-shot, not a client
/// bug (observed as a flake).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn stale_gate_push_never_flashes_paywall_for_subscribed_user() {
    let content = ContentController::start().await.expect("start content");
    // Live tier: already paid; steady settings allow access.
    content.server().set_user_subscription_tier(Some(PAID_TIER));
    content.server().set_settings(json!({
        "allow_access": true,
        "subscription_tier_display": PAID_TIER_DISPLAY,
    }));
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} paid path."));

    // Watch disabled so the deferral's own VerifyPendingGate is the only
    // subscription-check traffic (deferral does not depend on the watch).
    let mut harness = spawn_subscription_session(
        &content,
        "pty-subgate-paid",
        &[EnvOp::set(
            "ECHO_AGENT_SUBSCRIPTION_WATCH_INTERVAL_SECS",
            "0",
        )],
    );

    // Let startup fetches fully settle so the scripted one-shot below can
    // only be consumed by the /new refresh.
    harness.update(Duration::from_secs(2));
    let checks_before = user_check_count(&content);

    // One stale gated snapshot: the "remote settings stale moment".
    content.enqueue_response(
        "/v1/settings",
        ScriptedResponse::json(
            200,
            json!({ "allow_access": false, "gate_message": GATE_MSG }),
        ),
    );
    harness.inject_keys(b"/new\r").expect("run /new");

    // Sample the screen across the deferral window: the gate copy must
    // never appear. The deferred gate could only surface via a gated check
    // result (impossible — the live tier is paid and the fresh settings
    // allow) or the 30s hung-check net, which the resolving check disarms.
    let end = Instant::now() + Duration::from_secs(8);
    while Instant::now() < end {
        harness.update(Duration::from_millis(150));
        assert!(
            !harness.contains_text(GATE_MSG),
            "paywall flashed for an already-subscribed user\nscreen:\n{}",
            harness.screen_contents()
        );
    }

    // Positive anchors: the deferral's live check actually ran, and the
    // session is fully usable (prompt round-trips).
    assert!(
        user_check_count(&content) > checks_before,
        "expected a live subscription check for the deferred gate; requests: {:?}",
        content
            .requests()
            .iter()
            .map(|e| e.path.clone())
            .collect::<Vec<_>>()
    );
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} still usable."));
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("session usable for the subscribed user");

    harness.quit().expect("clean quit");
}
