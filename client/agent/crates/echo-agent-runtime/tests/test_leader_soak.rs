//! Leader soak: a real `MvpAgent` behind an in-process leader, churned by
//! clients until `LEADER_SOAK_SECS` expires. Each cycle closes its sessions,
//! so the bounds measure what teardown reclaims.
//!
//! ```bash
//! LEADER_SOAK_SECS=1200 cargo test -p echo-agent-runtime --features test-support \
//!     --test test_leader_soak -- --ignored --nocapture
//! ```

#![cfg(unix)]

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static DHAT_ALLOC: dhat::Alloc = dhat::Alloc;

/// Warmup so the window measures steady state, not first-session cost.
#[cfg(feature = "dhat-heap")]
const HEAP_WARMUP_CYCLES: u64 = 2;

use std::sync::Arc;
use std::time::Duration;

use echo_agent_runtime::leader::{
    ClientCapabilities, ClientMode, LeaderClient, LeaderServerControlState, LeaderServerMetadata,
    run_leader_server,
};
use echo_agent_test_support::resources::ResourceSnapshot;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// `leader.response.send_failed` entries written by THIS process.
fn send_failed_count() -> usize {
    let Some(bytes) = echo_agent_telemetry::unified_log::snapshot_log() else {
        return 0;
    };
    String::from_utf8_lossy(&bytes)
        .lines()
        .filter(|line| {
            serde_json::from_str::<serde_json::Value>(line).is_ok_and(|entry| {
                entry["msg"] == "leader.response.send_failed" && entry["pid"] == std::process::id()
            })
        })
        .count()
}

/// Send one JSON-RPC request through a `LeaderClient` and await the response
/// with the matching id, skipping interleaved notifications.
async fn rpc(client: &mut LeaderClient, payload: String, id: u64, what: &str) -> serde_json::Value {
    client
        .send(payload)
        .unwrap_or_else(|e| panic!("{what}: send failed: {e}"));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .unwrap_or_else(|| panic!("{what}: timed out waiting for response id {id}"));
        let msg = tokio::time::timeout(remaining, client.recv())
            .await
            .unwrap_or_else(|_| panic!("{what}: timed out waiting for response id {id}"))
            .unwrap_or_else(|| panic!("{what}: connection closed awaiting response id {id}"));
        let json: serde_json::Value = match serde_json::from_str(&msg) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if json["id"] == id && (json.get("result").is_some() || json.get("error").is_some()) {
            assert!(
                json.get("error").is_none(),
                "{what}: error response: {json}"
            );
            return json;
        }
    }
}

/// Per-registry entry counts. `echo.agent/debug/agent` answers under the extension
/// envelope's own `result`, nested inside the JSON-RPC `result`.
async fn registry_counts(client: &mut LeaderClient, id: u64) -> serde_json::Value {
    let resp = rpc(
        client,
        format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"_echo.agent/debug/agent","params":{{}}}}"#
        ),
        id,
        "echo.agent/debug/agent",
    )
    .await;
    let counts = resp["result"]["result"]["registries"].clone();
    assert!(
        counts.is_object(),
        "echo.agent/debug/agent returned no registries: {resp}"
    );
    counts
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "leader soak; run with --ignored (LEADER_SOAK_SECS bounds the duration)"]
async fn leader_soak_churning_clients_no_leaks_no_zombies() {
    echo_agent_extra_ca::ensure_default_crypto_provider();

    let server = echo_agent_test_support::MockInferenceServer::start()
        .await
        .unwrap();
    // Measure the leader, not the harness's copy of every conversation.
    server.set_keep_requests(false);
    let echo_agent_home = TempDir::new().unwrap();
    let workdir = TempDir::new().unwrap();

    // SAFETY: single-threaded current-thread runtime; set before any agent
    // code reads these process-globals (same pattern as session_load_perf).
    unsafe {
        std::env::set_var("ECHO_AGENT_HOME", echo_agent_home.path());
        std::env::set_var("ECHO_AGENT_CLI_CHAT_PROXY_BASE_URL", server.url());
        std::env::set_var("ECHO_AGENT_API_BASE_URL", server.url());
        std::env::set_var("ECHO_AGENT_API_KEY", "test-key-for-ci");
        std::env::set_var("ECHO_AGENT_TELEMETRY_ENABLED", "false");
        std::env::set_var("ECHO_AGENT_FEEDBACK_ENABLED", "false");
        std::env::set_var("ECHO_AGENT_TRACE_UPLOAD", "false");
    }

    let sock_path = echo_agent_home.path().join("leader-soak.sock");
    let soak_secs = env_u64("LEADER_SOAK_SECS", 10);
    let max_growth_mb = env_u64("LEADER_SOAK_MAX_RSS_GROWTH_MB", 1024);
    let max_thread_growth = env_u64("LEADER_SOAK_MAX_THREAD_GROWTH", 64) as usize;
    let send_failed_before = send_failed_count();

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (acp_tx, acp_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let (response_tx, response_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let cancel = CancellationToken::new();
            let client_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let control_state = LeaderServerControlState::new(LeaderServerMetadata {
                pid: std::process::id(),
                socket_path: sock_path.clone(),
                lock_path: sock_path.with_extension("lock"),
                ws_url_suffix: String::new(),
                leader_binary_version: env!("CARGO_PKG_VERSION").to_string(),
            });
            let cancel_for_server = cancel.clone();
            let sock_for_server = sock_path.clone();
            let client_count_for_server = client_count.clone();
            tokio::task::spawn_local(async move {
                let _ = run_leader_server(
                    sock_for_server,
                    acp_tx,
                    response_rx,
                    cancel_for_server,
                    true,
                    client_count_for_server,
                    Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    echo_agent_runtime::agent::activity::AgentActivity::default(),
                    tokio::sync::watch::channel(true).1,
                    tokio::sync::watch::channel(false).0,
                    tokio::sync::watch::channel(echo_agent_runtime::leader::ShutdownReason::Manual).0,
                    None,
                    control_state,
                )
                .await;
            });

            // Hold a sender for the whole soak: the leader's response channel
            // must not close when the agent's output ends.
            echo_agent_runtime::leader::in_process::spawn_agent(acp_rx, response_tx.clone());

            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            while !sock_path.exists() && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert!(sock_path.exists(), "leader socket never bound");

            let mut bootstrap = LeaderClient::connect(
                sock_path.clone(),
                "soak-bootstrap",
                ClientMode::Stdio,
                ClientCapabilities::default(),
            )
            .await
            .expect("bootstrap connect");
            rpc(
                &mut bootstrap,
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false},"_meta":{"startupHints":{"nonInteractive":true,"skipGitStatus":true,"skipProjectLayout":true},"clientType":"soak","clientVersion":"0.0.0-test"}}}"#.to_string(),
                1,
                "initialize",
            )
            .await;
            rpc(
                &mut bootstrap,
                r#"{"jsonrpc":"2.0","id":2,"method":"authenticate","params":{"methodId":"echoagent.api_key","_meta":{"headless":true}}}"#.to_string(),
                2,
                "authenticate",
            )
            .await;

            eprintln!(
                "[soak] budgets: {soak_secs}s, rss {max_growth_mb} MB, threads {max_thread_growth}"
            );
            #[cfg(feature = "dhat-heap")]
            let mut heap_window: Option<(dhat::Profiler, dhat::HeapStats, u64)> = None;
            let rss_before = ResourceSnapshot::capture();
            let soak_deadline = tokio::time::Instant::now() + Duration::from_secs(soak_secs);
            let workdir_str = workdir.path().to_string_lossy().to_string();
            let mut cycles: u64 = 0;
            let mut turns: u64 = 0;
            let mut baseline: Option<serde_json::Value> = None;

            while tokio::time::Instant::now() < soak_deadline {
                cycles += 1;
                let mut clients = Vec::new();
                for i in 0..10u64 {
                    let client = LeaderClient::connect(
                        sock_path.clone(),
                        "soak-client",
                        ClientMode::Stdio,
                        ClientCapabilities::default(),
                    )
                    .await
                    .unwrap_or_else(|e| panic!("cycle {cycles} client {i} connect: {e}"));
                    clients.push(client);
                }

                for (i, client) in clients.iter_mut().enumerate() {
                    for s in 0..2u64 {
                        let new_id = 100 + s;
                        let resp = rpc(
                            client,
                            format!(
                                r#"{{"jsonrpc":"2.0","id":{new_id},"method":"session/new","params":{{"cwd":"{workdir_str}","mcpServers":[]}}}}"#
                            ),
                            new_id,
                            "session/new",
                        )
                        .await;
                        let sid = resp["result"]["sessionId"]
                            .as_str()
                            .unwrap_or_else(|| panic!("no sessionId in {resp}"))
                            .to_string();

                        let prompt_id = 200 + s;
                        rpc(
                            client,
                            format!(
                                r#"{{"jsonrpc":"2.0","id":{prompt_id},"method":"session/prompt","params":{{"sessionId":"{sid}","prompt":[{{"type":"text","text":"soak c{i} s{s} cycle {cycles}"}}]}}}}"#
                            ),
                            prompt_id,
                            "session/prompt",
                        )
                        .await;
                        turns += 1;

                        // Disconnecting leaves sessions resident for a
                        // reconnect; `_` is the wire form for a custom method.
                        let close_id = 300 + s;
                        rpc(
                            client,
                            format!(
                                r#"{{"jsonrpc":"2.0","id":{close_id},"method":"_echo.agent/session/close","params":{{"sessionId":"{sid}"}}}}"#
                            ),
                            close_id,
                            "echo.agent/session/close",
                        )
                        .await;
                    }
                }

                for client in clients {
                    client.cancel();
                }
                let drain_deadline = tokio::time::Instant::now() + Duration::from_secs(30);
                while client_count.load(std::sync::atomic::Ordering::Relaxed) > 1 {
                    assert!(
                        tokio::time::Instant::now() < drain_deadline,
                        "cycle {cycles}: roster kept {} zombie clients after churn",
                        client_count.load(std::sync::atomic::Ordering::Relaxed)
                    );
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }

                // An entry that never drains names itself here, one cycle
                // after it leaks.
                let counts = registry_counts(&mut bootstrap, 1000 + cycles).await;
                assert_eq!(
                    counts["sessions"], 0,
                    "cycle {cycles}: sessions outlived their close: {counts}"
                );
                match baseline.as_ref() {
                    None => baseline = Some(counts),
                    Some(first) => assert_eq!(
                        &counts, first,
                        "cycle {cycles}: registry counts left their baseline"
                    ),
                }

                #[cfg(feature = "dhat-heap")]
                if cycles == HEAP_WARMUP_CYCLES {
                    let profiler = dhat::Profiler::builder()
                        // The 10-frame default never reaches our own code.
                        .trim_backtraces(Some(48))
                        .file_name(
                            std::env::var("LEADER_SOAK_DHAT_OUT")
                                .unwrap_or_else(|_| "dhat-leader-soak.json".to_string()),
                        )
                        .build();
                    heap_window = Some((profiler, dhat::HeapStats::get(), cycles));
                }

                // Linear in cycles is a leak; flattening is the allocator.
                if let Some(rss) = ResourceSnapshot::capture().rss {
                    eprintln!(
                        "[soak] cycle {cycles}: rss {:.1} MB",
                        rss as f64 / (1024.0 * 1024.0)
                    );
                }
            }

            // Retained heap is a leak; retained pages alone are the allocator.
            #[cfg(feature = "dhat-heap")]
            if let Some((profiler, before, start_cycle)) = heap_window.take() {
                let after = dhat::HeapStats::get();
                drop(profiler);
                let measured = cycles.saturating_sub(start_cycle).max(1);
                let net_bytes = after.curr_bytes as i64 - before.curr_bytes as i64;
                let net_blocks = after.curr_blocks as i64 - before.curr_blocks as i64;
                let per_cycle = net_bytes / measured as i64;
                eprintln!(
                    "[soak] heap over {measured} cycles: {net_bytes} bytes, {net_blocks} blocks \
                     ({:.2} MB per cycle)",
                    net_bytes as f64 / measured as f64 / (1024.0 * 1024.0)
                );
                let max_per_cycle = env_u64("LEADER_SOAK_MAX_HEAP_BYTES_PER_CYCLE", 1 << 20) as i64;
                assert!(
                    per_cycle <= max_per_cycle,
                    "leader retained {per_cycle} heap bytes per cycle (bound {max_per_cycle})"
                );
            }

            eprintln!("[soak] {cycles} cycles, {turns} turns in {soak_secs}s budget");
            assert!(cycles > 0, "soak budget too small to complete one cycle");

            assert_eq!(
                client_count.load(std::sync::atomic::Ordering::Relaxed),
                1,
                "roster must converge to the bootstrap client after churn"
            );
            let resp = rpc(
                &mut bootstrap,
                format!(
                    r#"{{"jsonrpc":"2.0","id":900,"method":"session/new","params":{{"cwd":"{workdir_str}","mcpServers":[]}}}}"#
                ),
                900,
                "post-soak session/new",
            )
            .await;
            assert!(resp["result"]["sessionId"].is_string());

            assert_eq!(
                send_failed_count(),
                send_failed_before,
                "leader.response.send_failed must not occur during the soak"
            );

            let rss_after = ResourceSnapshot::capture();
            let growth = rss_after.growth_from(&rss_before);
            if let (Some(before), Some(after), Some(growth_bytes)) =
                (rss_before.rss, rss_after.rss, growth.rss)
            {
                let growth_mb = growth_bytes as f64 / (1024.0 * 1024.0);
                eprintln!(
                    "[soak] rss: {:.1} MB -> {:.1} MB (growth {growth_mb:.1} MB)",
                    before as f64 / (1024.0 * 1024.0),
                    after as f64 / (1024.0 * 1024.0),
                );
                assert!(
                    growth_mb <= max_growth_mb as f64,
                    "leader RSS grew {growth_mb:.1} MB over the soak (bound {max_growth_mb} MB)"
                );
            } else {
                panic!("memory sample unavailable; the soak cannot bound it");
            }

            // A missing sample means the probe failed, which would silently
            // retire the nightly budget. macOS samples threads too now, but
            // the bound is tuned against Linux nightlies, so a macOS run
            // logs the growth without enforcing it.
            match growth.threads {
                Some(thread_growth) => {
                    eprintln!("[soak] threads: growth {thread_growth}");
                    if cfg!(target_os = "linux") {
                        assert!(
                            thread_growth <= max_thread_growth,
                            "leader threads grew by {thread_growth} over the soak \
                             (bound {max_thread_growth})"
                        );
                    }
                }
                None if cfg!(target_os = "linux") => {
                    panic!("thread growth sample unavailable; the soak cannot bound it")
                }
                None => {}
            }

            bootstrap.cancel();
            cancel.cancel();
        })
        .await;
}
