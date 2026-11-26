// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// 1a. **Embedded mode (`--no-leader`) boots without hanging on a blocked backend.**
///
/// Enterprise deployments set `[cli] use_leader = false` and point at their own
/// backend, often with the cloud.echo-agent.invalid proxy blocked. A TCP listener that accepts
/// but never replies stands in for that endpoint, so every startup HTTP call
/// stalls until the client's own bounded timeout fires. The welcome screen must
/// render anyway; a hang here means some boot path went unbounded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn embedded_mode_boots_without_hanging_on_blocked_backend() {
    // Accept connections but never respond, holding the streams open.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming() {
            match stream {
                Ok(s) => held.push(s),
                Err(_) => break,
            }
        }
    });
    let base = format!("http://{addr}/v1");

    let home = tempfile::tempdir().expect("home");
    let echo_agent_home = home.path().join(".echo-agent");
    std::fs::create_dir_all(&echo_agent_home).unwrap();
    let env = [
        ("HOME", home.path().to_str().unwrap()),
        ("ECHO_AGENT_HOME", echo_agent_home.to_str().unwrap()),
        ("ECHO_AGENT_API_KEY", "test-key-for-ci"),
        ("ECHO_AGENT_CLI_CHAT_PROXY_BASE_URL", base.as_str()),
        ("ECHO_AGENT_API_BASE_URL", base.as_str()),
        ("ECHO_AGENT_TELEMETRY_ENABLED", "false"),
        ("ECHO_AGENT_FEEDBACK_ENABLED", "false"),
        ("ECHO_AGENT_TRACE_UPLOAD", "false"),
    ];

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness = PtyHarness::new_inherited_env(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &["--no-leader"],
        &env,
        None,
    )
    .expect("spawn pager");

    // 30s exceeds the sum of the bounded startup fetches (auth + settings +
    // catalog, ~5s each), so a timeout here means an unbounded wait, not a
    // slow one.
    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, Duration::from_secs(30))
        .expect("embedded welcome must render despite the blocked backend (boot went unbounded)");
    let screen = harness.screen_contents();
    assert!(!screen.contains("panicked"), "panic on screen:\n{screen}");

    harness.quit().expect("clean quit");
}
