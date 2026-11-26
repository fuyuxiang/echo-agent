// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// 2a-park. **Upload queue parks on storage 401 and drains after recovery.**
///
/// The production "chat works, storage 401s" signature: chat completes
/// against the mock while `/v1/storage` rejects the bearer. The trace
/// artifact must survive the outage (parked, quiescent) and land once the
/// gate heals — pre-park it was permanently dropped after one refresh retry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn storage_upload_parks_on_401_and_drains_after_recovery() {
    let content = ContentController::start().await.expect("start content");
    content.set_response(format!("{MOCK_RESPONSE_SENTINEL} park e2e response."));
    // Storage auth outage from the start; chat endpoints stay healthy.
    content.set_storage_unauthorized(true);

    // Trace uploads are gated on first-party EchoAgent OAuth (`is_echo_agent_auth()`:
    // AuthMode::Oidc + EchoAgent issuer); the harness's ECHO_AGENT_API_KEY is ApiKey mode
    // and never uploads. Seed a fake OAuth entry instead — the mock accepts
    // any bearer, and its failing refresh_token is exactly the parked state
    // under test.
    seed_fake_oauth(&content, "pty-park-e2e");

    // Explicit overrides win over the sandbox defaults. Disable only the fake
    // API-key credential so seeded OAuth remains active.
    let overrides = [
        oauth_credential_ops()[0],
        EnvOp::set("ECHO_AGENT_TRACE_UPLOAD", "true"),
        EnvOp::set("ECHO_AGENT_TELEMETRY_TRACE_UPLOAD", "true"),
        EnvOp::set("ECHO_AGENT_UPLOAD_QUEUE_AUTH_PROBE_SECS", "2"),
    ];

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness = PtyHarness::spawn_with_content_env_ops(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &[],
        &overrides,
    )
    .expect("spawn pager with storage-401 mock");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");
    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("chat response on screen while storage is 401ing");

    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while content.storage_request_count() < 2 && std::time::Instant::now() < deadline {
        harness.update(Duration::from_millis(200));
    }
    assert!(
        content.storage_request_count() >= 2,
        "storage saw the initial attempt + refresh retry; requests: {:?}",
        content.requests()
    );
    assert!(
        content.storage_uploads().is_empty(),
        "no upload may be accepted while the 401 gate is closed"
    );

    // Bounded-probe check, not strict quiescence (that's unit-tested with the
    // production probe interval).
    //
    // Accounting (echo-agent-file-utils upload queue):
    // - `DEFAULT_MAX_CONCURRENT` = 8 workers
    // - each post-park wire attempt may do a probe + credential refresh retry
    //   → 2 storage requests per wake
    // - `AUTH_PARK_WAIT_INTERVAL` is 5s, so a single wait-slice timeout should
    //   not fire inside this 3s window; with `ECHO_AGENT_UPLOAD_QUEUE_AUTH_PROBE_SECS=2`
    //   and `has_usable_credential()` still true for a seeded OAuth entry that
    //   storage rejects, the probe path can still wake workers early.
    //
    // Allow two effective wake cycles of headroom (8 × 2 × 2 = 32). CI has
    // observed 7 → 26 on amd64-local under that path — still an order of
    // magnitude below a busy-loop (hundreds). Keep the bound tight enough that
    // a per-slice retry storm still fails.
    // u32 to match `ContentController::storage_request_count`.
    const MAX_PARKED_WORKERS: u32 = 8;
    const REQUESTS_PER_WAKE: u32 = 2;
    const WAKE_CYCLE_HEADROOM: u32 = 2;
    const MAX_EXTRA_WHILE_PARKED: u32 =
        MAX_PARKED_WORKERS * REQUESTS_PER_WAKE * WAKE_CYCLE_HEADROOM;

    let parked_count = content.storage_request_count();
    harness.update(Duration::from_secs(3));
    let after = content.storage_request_count();
    assert!(
        after <= parked_count + MAX_EXTRA_WHILE_PARKED,
        "parked queue must not spam storage: {parked_count} -> {after} \
         (allowed +{MAX_EXTRA_WHILE_PARKED})"
    );
    assert!(
        harness.is_running().expect("poll pager liveness"),
        "pager stays healthy while parked"
    );

    content.set_storage_unauthorized(false);
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while content.storage_uploads().is_empty() && std::time::Instant::now() < deadline {
        harness.update(Duration::from_millis(200));
    }
    let uploads = content.storage_uploads();
    assert!(
        !uploads.is_empty(),
        "parked artifact uploads after recovery; storage requests: {}",
        content.storage_request_count()
    );
    assert!(
        uploads.iter().all(|u| u.size > 0),
        "uploaded artifacts are non-empty: {uploads:?}"
    );

    harness.quit().expect("clean quit");
}
