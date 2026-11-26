// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// Env var under test; the TestSandbox `env_clear` guarantees it is unset
/// unless a case sets it explicitly.
const CONNECT_UI_TIMEOUT_ENV: &str = "ECHO_AGENT_CONNECT_UI_TIMEOUT_SECS";

/// Unified-log message the pager writes directly (pre-connect, bypassing the
/// ACP forwarder) whenever the env var is set — including rejected values,
/// so the resolution is observable on the startups that fail inside it.
const ENV_BUDGET_LOG_MSG: &str = "startup connect budget from env";

/// Poll the sandbox unified log until it contains `needle` or `timeout`
/// elapses, returning the last read. The pager's write is immediate, but
/// this test races the child process's startup, and a read can catch a line
/// mid-append — so poll on a needle from the entry's LAST field (`ctx`), not
/// its first.
fn read_unified_log_until(
    harness: &mut PtyHarness,
    path: &Path,
    needle: &str,
    timeout: Duration,
) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let unified = std::fs::read_to_string(path).unwrap_or_default();
        if unified.contains(needle) || Instant::now() >= deadline {
            return unified;
        }
        harness.update(Duration::from_millis(200));
    }
}

/// Spawn the pager with the env var set, wait for the welcome screen, then
/// return the unified-log line recording the budget resolution (polled on
/// `ctx_needle`, a fragment of the entry's last-serialized field).
async fn boot_and_read_budget_line(env_value: &str, ctx_needle: &str) -> String {
    let content = ContentController::start().await.expect("start content");

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness = PtyHarness::spawn_with_content_env_ops(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &content,
        &[],
        &[EnvOp::set(CONNECT_UI_TIMEOUT_ENV, env_value)],
    )
    .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");

    let log_path = unified_log_path(&content);
    let unified =
        read_unified_log_until(&mut harness, &log_path, ctx_needle, Duration::from_secs(30));
    let line = unified
        .lines()
        .find(|line| line.contains(ctx_needle))
        .unwrap_or_else(|| {
            panic!(
                "unified log must record the env budget resolution\nlog path: {}\nlog:\n{unified}",
                log_path.display()
            )
        })
        .to_owned();

    harness.quit().expect("clean quit");
    line
}

/// **Valid override boots and is observable.** With the env set to `45`,
/// startup completes under the raised budget and the pager records the raw
/// value plus what it resolved to — proof the override resolved from the env
/// end-to-end, durable even though this run happened to succeed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn connect_ui_timeout_env_override_logs_and_boots() {
    let line = boot_and_read_budget_line("45", "\"timeout_secs\":45").await;
    assert!(
        line.contains(&format!("\"msg\":\"{ENV_BUDGET_LOG_MSG}\"")),
        "resolution entry must carry the budget message\nentry: {line}"
    );
    assert!(
        line.contains("\"raw\":\"45\""),
        "resolution entry must carry the raw env value\nentry: {line}"
    );
}

/// **Garbage falls back loudly.** An unparsable value resolves to the default
/// 30s (startup still completes) and the entry records the rejected input
/// next to the default it resolved to — a presence check, with no ordering
/// argument about shared buffers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn connect_ui_timeout_env_garbage_logs_default_and_boots() {
    let line = boot_and_read_budget_line("garbage", "\"timeout_secs\":30").await;
    assert!(
        line.contains(&format!("\"msg\":\"{ENV_BUDGET_LOG_MSG}\"")),
        "resolution entry must carry the budget message\nentry: {line}"
    );
    assert!(
        line.contains("\"raw\":\"garbage\""),
        "the rejected input must be recorded next to the default it resolved to\nentry: {line}"
    );
}
