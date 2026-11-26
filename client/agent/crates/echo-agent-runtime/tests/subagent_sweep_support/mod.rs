//! Shared harness for the subagent latency sweep and the bootstrap-cost
//! regression tier. Included by BOTH test binaries via `#[path]`: the
//! regression tier lives in its own binary because the waterfall sink and
//! env latch once per process.
#![allow(dead_code)]

use std::time::{Duration, Instant};

use agent_client_protocol::{self as acp, Agent as _};
use echo_agent_runtime::waterfall;
use echo_agent_test_support::{
    InferenceEndpoint, InferenceRequestMatcher, MockInferenceServer, ResourceSnapshot, RssSampler,
    ScriptedResponse, SseEvent,
};
use echo_agent_test_utils::env::env_usize;
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::acp_harness;
use crate::perf_harness::{PerfRecorder, spawn_agent_thread};

pub fn chat_chunk(delta: Value, finish_reason: Value) -> SseEvent {
    SseEvent::data(
        json!({
            "id": "chatcmpl-sweep",
            "object": "chat.completion.chunk",
            "created": 1234567890,
            "model": "test-model",
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish_reason }]
        })
        .to_string(),
    )
}

pub fn probe_id(n: usize, i: usize) -> String {
    format!("swp-{n}-{i:03}")
}

/// One assistant response with N parallel task calls, one delta chunk per
/// call; the script speaks wire names (`spawn_subagent`, `background`).
pub fn burst_tool_calls_sse(n: usize, isolation: &str) -> ScriptedResponse {
    let mut events = Vec::with_capacity(n + 2);
    for i in 0..n {
        let mut args = json!({
            "description": format!("latency probe {i:03}"),
            "prompt": format!("Reply with the word done and nothing else ({i:03})."),
            "subagent_type": "general-purpose",
            "background": true,
            "task_id": probe_id(n, i),
        });
        if isolation == "worktree" {
            args["isolation"] = json!("worktree");
        }
        events.push(chat_chunk(
            json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "index": i,
                    "id": format!("call_task_{i:03}"),
                    "type": "function",
                    "function": { "name": "spawn_subagent", "arguments": args.to_string() }
                }]
            }),
            Value::Null,
        ));
    }
    events.push(chat_chunk(json!({}), json!("tool_calls")));
    events.push(SseEvent::data("[DONE]"));
    ScriptedResponse::sse(events)
}

pub fn build_repo(files: usize) -> TempDir {
    let dir = TempDir::new().expect("repo tempdir");
    let wd = dir.path();
    let git = |args: &[&str]| echo_agent_test_utils::git::run_git(wd, args);
    git(&["init"]);
    git(&["config", "user.name", "Sweep User"]);
    git(&["config", "user.email", "sweep@test.com"]);
    echo_agent_test_utils::git::write_fanout_tree(wd, files, 100);
    git(&["add", "."]);
    git(&["commit", "-m", "populate tree"]);
    dir
}

pub struct Row {
    pub id: String,
    pub spawn_ms: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub task_ms: i64,
    pub status: String,
    pub agent_ms: i64,
}

pub fn pctl(sorted: &[i64], p: f64) -> i64 {
    if sorted.is_empty() {
        return -1;
    }
    let rank = ((p / 100.0) * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

pub fn offset_ms(t0: Instant, at: Instant) -> i64 {
    at.duration_since(t0).as_millis() as i64
}

/// Map a wall-clock instant onto the marks' monotonic timeline.
pub fn mono_us(at: std::time::SystemTime) -> u128 {
    let now_mono = waterfall::now_us();
    let behind = std::time::SystemTime::now()
        .duration_since(at)
        .map(|d| d.as_micros())
        .unwrap_or(0);
    now_mono.saturating_sub(behind)
}

/// A cold listener stalls the burst's earliest simultaneous connections.
pub async fn warm_mock(origin: &str, k: usize) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let addr = origin.trim_start_matches("http://").to_string();
    let t = Instant::now();
    let mut handles = Vec::with_capacity(k);
    for _ in 0..k {
        let addr = addr.clone();
        handles.push(tokio::task::spawn_local(async move {
            let Ok(Ok(mut s)) = tokio::time::timeout(
                Duration::from_secs(5),
                tokio::net::TcpStream::connect(&addr),
            )
            .await
            else {
                return false;
            };
            let _ = s
                .write_all(b"GET /v1/models HTTP/1.1\r\nHost: mock\r\nConnection: close\r\n\r\n")
                .await;
            let mut buf = Vec::new();
            let _ = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut buf)).await;
            !buf.is_empty()
        }));
    }
    let mut ok = 0usize;
    for h in handles {
        if matches!(h.await, Ok(true)) {
            ok += 1;
        }
    }
    eprintln!("WARMUP k={k} ok={ok} ms={}", t.elapsed().as_millis());
}

/// Emit one `MOCK_REQ` mark per probe: for each probe id, stamp the first mock
/// chat request whose last message is that probe's user prompt, which skips
/// tool-result follow-ups and the auto-wake echo.
pub fn emit_mock_request_marks(server: &MockInferenceServer, n: usize) {
    let mut seen: std::collections::HashSet<String> = Default::default();
    for e in server.requests() {
        if e.method != "POST" || !e.path.contains("chat/completions") {
            continue;
        }
        let Some(body) = &e.body else { continue };
        let Some(last) = body
            .get("messages")
            .and_then(|m| m.as_array())
            .and_then(|a| a.last())
        else {
            continue;
        };
        if last.get("role").and_then(|r| r.as_str()) != Some("user") {
            continue;
        }
        let content = last
            .get("content")
            .map(|c| c.to_string())
            .unwrap_or_default();
        for i in 0..n {
            if content.contains(&format!("nothing else ({i:03}).")) {
                let id = probe_id(n, i);
                if seen.insert(id.clone()) {
                    eprintln!(
                        "{} id={id} stage={} t_us={}",
                        waterfall::LINE_PREFIX,
                        waterfall::stage::MOCK_REQ,
                        mono_us(e.at)
                    );
                }
                break;
            }
        }
    }
}

pub struct BurstOutcome {
    pub rows: Vec<Row>,
    pub wall_ms: i64,
    pub prompt_ms: i64,
    pub failures: usize,
    pub rss_end_mb: f64,
    pub rss_peak_mb: f64,
    pub peak_threads: usize,
    pub peak_fds: usize,
}

/// One N-subagent burst against `server`; the agent runs on its own thread,
/// the client (this thread) stamps notification arrivals.
pub fn run_burst(
    server: &MockInferenceServer,
    n: usize,
    isolation: &str,
    deadline: Duration,
) -> BurstOutcome {
    let repo_files = env_usize("ECHO_AGENT_SWEEP_REPO_FILES", 50);
    let repo = build_repo(repo_files);
    let repo_path = repo.path().to_path_buf();

    // The first foreground chat request is the parent's opening turn; every
    // later request falls through to echo mode.
    let expectation = server.expect_response(
        format!("burst-{n}-{isolation}"),
        InferenceRequestMatcher::foreground(InferenceEndpoint::ChatCompletions),
        burst_tool_calls_sse(n, isolation),
    );

    let (pipes, agent_thread) = spawn_agent_thread("sweep-agent");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("client runtime");
    let local = tokio::task::LocalSet::new();

    let outcome = local.block_on(&rt, async move {
        let (client, rec) = PerfRecorder::new();
        let (client_conn, _init) =
            acp_harness::connect_client(client, "subagent-latency-sweep", pipes).await;
        let session_id = acp_harness::new_session(&client_conn, &repo_path).await;

        let baseline = ResourceSnapshot::capture();
        eprintln!(
            "BASELINE n={n} rss_mb={:.1} threads={} fds={}",
            baseline.rss.unwrap_or(0) as f64 / (1024.0 * 1024.0),
            baseline.threads.unwrap_or(0),
            baseline.open_files.unwrap_or(0),
        );

        warm_mock(&server.origin(), n.max(32)).await;

        let sampler = RssSampler::start();
        let t0 = Instant::now();
        eprintln!(
            "{} n={n} t_us={}",
            waterfall::T0_LINE_PREFIX,
            waterfall::now_us()
        );
        let prompt = tokio::time::timeout(
            Duration::from_secs(120 + 2 * n as u64),
            client_conn.prompt(acp::PromptRequest::new(
                session_id.clone(),
                vec![acp::ContentBlock::Text(acp::TextContent::new(format!(
                    "Spawn {n} latency probe subagents."
                )))],
            )),
        )
        .await
        .expect("parent prompt timed out")
        .expect("parent prompt failed");
        let prompt_ms = offset_ms(t0, Instant::now());
        assert!(
            matches!(prompt.stop_reason, acp::StopReason::EndTurn),
            "expected EndTurn, got {:?}\n{}",
            prompt.stop_reason,
            server.request_log_summary()
        );
        expectation.assert_satisfied();

        let wait_deadline = Instant::now() + deadline;
        let mut peak_threads = 0usize;
        let mut peak_fds = 0usize;
        loop {
            let snap = ResourceSnapshot::capture();
            peak_threads = peak_threads.max(snap.threads.unwrap_or(0));
            peak_fds = peak_fds.max(snap.open_files.unwrap_or(0));
            if rec.borrow().finished.len() >= n {
                break;
            }
            if Instant::now() > wait_deadline {
                eprintln!(
                    "DEADLINE n={n} finished={}/{n} spawned={} dispatched={}",
                    rec.borrow().finished.len(),
                    rec.borrow().spawned.len(),
                    rec.borrow().dispatch.len(),
                );
                for d in rec.borrow().dispatch.iter() {
                    eprintln!(
                        "DEADLINE-DISPATCH tool_call_id={} task_id={:?} status={:?} output={:?}",
                        d.tool_call_id, d.task_id, d.last_status, d.last_output,
                    );
                }
                eprintln!("DEADLINE-MOCK {}", server.request_log_summary());
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let wall_ms = rec
            .borrow()
            .finished
            .iter()
            .map(|f| offset_ms(t0, f.at))
            .max()
            .unwrap_or(-1);

        if std::env::var_os(waterfall::ENV).is_some() {
            emit_mock_request_marks(server, n);
        }

        let rss = sampler.finish();
        let end_snap = ResourceSnapshot::capture();

        let rec = rec.borrow();
        let mut rows = Vec::with_capacity(n);
        let mut failures = 0usize;
        for i in 0..n {
            let id = probe_id(n, i);
            // Exact task_id match first, then positional fallback.
            let spawn_ms = rec
                .dispatch
                .iter()
                .find(|d| d.task_id.as_deref() == Some(id.as_str()))
                .or_else(|| rec.dispatch.get(i))
                .map(|d| offset_ms(t0, d.at))
                .unwrap_or(-1);
            let start_ms = rec
                .spawned
                .iter()
                .find(|s| s.subagent_id == id)
                .map(|s| offset_ms(t0, s.at))
                .unwrap_or(-1);
            let finished = rec.finished.iter().find(|f| f.subagent_id == id);
            let end_ms = finished.map(|f| offset_ms(t0, f.at)).unwrap_or(-1);
            let status = finished
                .map(|f| f.status.clone())
                .unwrap_or_else(|| "missing".to_string());
            let agent_ms = finished.map(|f| f.agent_duration_ms as i64).unwrap_or(-1);
            let task_ms = if start_ms >= 0 && end_ms >= 0 {
                end_ms - start_ms
            } else {
                -1
            };
            if status != "completed" {
                failures += 1;
            }
            rows.push(Row {
                id,
                spawn_ms,
                start_ms,
                end_ms,
                task_ms,
                status,
                agent_ms,
            });
        }

        BurstOutcome {
            rows,
            wall_ms,
            prompt_ms,
            failures,
            rss_end_mb: end_snap.rss.unwrap_or(0) as f64 / (1024.0 * 1024.0),
            rss_peak_mb: rss.peak_rss as f64 / (1024.0 * 1024.0),
            peak_threads,
            peak_fds,
        }
    });

    agent_thread.finish();
    outcome
}

/// Process-wide scaffold shared by the sweep and regression binaries: the
/// mock's own runtime (agent startup prefetch would starve a shared one),
/// an isolated ECHO_AGENT_HOME, and the base test env.
pub struct SweepEnv {
    pub mock_rt: tokio::runtime::Runtime,
    pub deadline: Duration,
    _echo_agent_home: TempDir,
}

/// SAFETY: call before any agent threads exist; mock workers never read env.
pub fn sweep_env_init() -> SweepEnv {
    let _ = rustls::crypto::ring::default_provider().install_default();
    // The spawn pipeline reports failures via tracing only.
    let filter = std::env::var("ECHO_AGENT_SWEEP_LOG").unwrap_or_else(|_| "warn".to_string());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
    let mock_rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("mock-worker")
        .enable_all()
        .build()
        .expect("mock runtime");
    let echo_agent_home = TempDir::new().expect("echoagent home");
    unsafe {
        std::env::set_var("ECHO_AGENT_HOME", echo_agent_home.path());
        std::env::set_var("ECHO_AGENT_API_KEY", "test-key-for-ci");
        std::env::set_var("ECHO_AGENT_TELEMETRY_ENABLED", "false");
        std::env::set_var("ECHO_AGENT_FEEDBACK_ENABLED", "false");
        std::env::set_var("ECHO_AGENT_TRACE_UPLOAD", "false");
    }
    SweepEnv {
        mock_rt,
        deadline: Duration::from_secs(env_usize("ECHO_AGENT_SWEEP_DEADLINE_S", 240) as u64),
        _echo_agent_home: echo_agent_home,
    }
}

/// One burst on a fresh mock server: repoint the env (SAFETY: the previous
/// burst's agent thread has been joined), run, let child teardown settle.
pub fn burst_on_fresh_mock(env: &SweepEnv, n: usize, isolation: &str) -> BurstOutcome {
    let server = env
        .mock_rt
        .block_on(MockInferenceServer::start())
        .expect("mock server");
    unsafe {
        std::env::set_var("ECHO_AGENT_CLI_CHAT_PROXY_BASE_URL", server.url());
        std::env::set_var("ECHO_AGENT_API_BASE_URL", server.url());
    }
    let outcome = run_burst(&server, n, isolation, env.deadline);
    std::thread::sleep(Duration::from_secs(1));
    outcome
}

/// One burst per N; the per-N wrapper tests select N by test-name filter so
/// no env plumbing is needed through remote runners.
pub fn run_sweep(ns: &[usize], isolation: &str) {
    let env = sweep_env_init();
    for &n in ns {
        let outcome = burst_on_fresh_mock(&env, n, isolation);

        for row in &outcome.rows {
            eprintln!(
                "SUBAGENT n={n} id={} spawn_ms={} start_ms={} end_ms={} task_ms={} status={} agent_ms={}",
                row.id,
                row.spawn_ms,
                row.start_ms,
                row.end_ms,
                row.task_ms,
                row.status,
                row.agent_ms,
            );
        }

        // A row must carry both stamps to count.
        let sorted = |vals: Vec<i64>| {
            let mut vals = vals;
            vals.sort_unstable();
            vals
        };
        let task_sorted = sorted(
            outcome
                .rows
                .iter()
                .filter(|r| r.task_ms >= 0)
                .map(|r| r.task_ms)
                .collect(),
        );
        let spawn_lat_sorted = sorted(
            outcome
                .rows
                .iter()
                .filter(|r| r.spawn_ms >= 0 && r.start_ms >= 0)
                .map(|r| r.start_ms - r.spawn_ms)
                .collect(),
        );
        let start_sorted = sorted(
            outcome
                .rows
                .iter()
                .filter(|r| r.start_ms >= 0)
                .map(|r| r.start_ms)
                .collect(),
        );

        let throughput = if outcome.wall_ms > 0 {
            (outcome.rows.len() - outcome.failures) as f64 / (outcome.wall_ms as f64 / 1000.0)
        } else {
            0.0
        };
        eprintln!(
            "SWEEP n={n} iso={isolation} p50={} p95={} spawn_p50={} spawn_p95={} start_p50={} start_p95={} wall_ms={} prompt_ms={} throughput={throughput:.2} rss_mb={:.1} rss_peak_mb={:.1} threads={} fds={} failures={}",
            pctl(&task_sorted, 50.0),
            pctl(&task_sorted, 95.0),
            pctl(&spawn_lat_sorted, 50.0),
            pctl(&spawn_lat_sorted, 95.0),
            pctl(&start_sorted, 50.0),
            pctl(&start_sorted, 95.0),
            outcome.wall_ms,
            outcome.prompt_ms,
            outcome.rss_end_mb,
            outcome.rss_peak_mb,
            outcome.peak_threads,
            outcome.peak_fds,
            outcome.failures,
        );

        if std::env::var_os("ECHO_AGENT_SWEEP_ASSERT_NO_FAILURES").is_some() {
            assert_eq!(outcome.failures, 0, "burst n={n} had failed subagents");
        }
    }
}

/// Per-id stage timestamps (us) parsed from a marks file from `offset`.
pub fn parse_waterfall_marks(
    path: &std::path::Path,
    offset: usize,
) -> (
    std::collections::HashMap<String, std::collections::HashMap<String, u128>>,
    usize,
) {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let slice = content.get(offset..).unwrap_or_default();
    let mut marks: std::collections::HashMap<String, std::collections::HashMap<String, u128>> =
        Default::default();
    for line in slice.lines() {
        let Some(line) = line.strip_prefix(waterfall::LINE_PREFIX) else {
            continue;
        };
        let mut id = None;
        let mut stage = None;
        let mut t_us = None;
        for part in line.split(' ') {
            if let Some(v) = part.strip_prefix("id=") {
                id = Some(v);
            } else if let Some(v) = part.strip_prefix("stage=") {
                stage = Some(v);
            } else if let Some(v) = part.strip_prefix("t_us=") {
                t_us = v.parse::<u128>().ok();
            }
        }
        if let (Some(id), Some(stage), Some(t_us)) = (id, stage, t_us)
            && id.starts_with("swp-")
        {
            marks
                .entry(id.to_string())
                .or_default()
                .insert(stage.to_string(), t_us);
        }
    }
    (marks, content.len())
}

/// p50 of (`to` − `from`) in ms across the ids that carry both marks.
pub fn segment_p50_ms(
    marks: &std::collections::HashMap<String, std::collections::HashMap<String, u128>>,
    from: &str,
    to: &str,
) -> Option<f64> {
    let vals: Vec<f64> = marks
        .values()
        .filter_map(|stages| {
            let a = *stages.get(from)?;
            let b = *stages.get(to)?;
            Some(b.saturating_sub(a) as f64 / 1000.0)
        })
        .collect();
    (!vals.is_empty()).then(|| median_f64(&vals))
}

pub fn median_f64(vals: &[f64]) -> f64 {
    if vals.is_empty() {
        return f64::NAN;
    }
    let mut vals = vals.to_vec();
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    vals[vals.len() / 2]
}

/// Sweep entry points mutate process env and share the latched sink.
pub static SWEEP_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn sweep_lock() -> std::sync::MutexGuard<'static, ()> {
    SWEEP_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}
