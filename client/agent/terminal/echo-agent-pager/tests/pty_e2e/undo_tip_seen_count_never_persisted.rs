// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// 17. **No persistence — the seen count never lands on disk.**
/// After triggering the tip (the path the old code wrote
/// `[hints] undo_tip_shown_count` to `config.toml` from), the persisted config
/// must not carry the tip key.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn undo_tip_seen_count_never_persisted() {
    let content = ContentController::start().await.expect("start content");
    let binary = pager_binary().expect("resolve pager binary");
    // Contextual hints ship default-OFF; opt in explicitly so the tip shows.
    let env_refs = CONTEXTUAL_HINTS_ENV;
    let mut harness = PtyHarness::spawn_with_content_env(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &[],
        env_refs,
    )
    .expect("spawn");
    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome");

    wipe_substantial_draft(&mut harness);
    harness
        .wait_for_text(UNDO_TIP_SENTINEL, Duration::from_secs(10))
        .expect("tip shows");
    harness.update(Duration::from_millis(500));
    harness.quit().expect("quit");

    // Old behavior wrote `[hints] undo_tip_shown_count`; the in-memory cap
    // writes nothing. A missing config.toml (read_to_string → "") also proves
    // no persist happened, since the old code would have created it.
    let config = content.home().join(".echo-agent").join("config.toml");
    let body = std::fs::read_to_string(&config).unwrap_or_default();
    assert!(
        !body.contains("undo_tip_shown_count"),
        "the undo-tip seen count must never be persisted to config.toml:\n{body}"
    );
}
