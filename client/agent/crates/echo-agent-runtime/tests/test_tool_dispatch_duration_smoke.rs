//! Smoke: dispatch wall-clock tool duration (not post-flight ~0ms).
//!
//! Scripts a model turn that calls `run_terminal_command` with `sleep 2`,
//! runs headless against the mock inference server, then asserts
//! `events.jsonl` has a `tool_completed` with multi-second `duration_ms`
//! and a non-empty `tool_call_id`.
//!
//! `#[ignore]` (needs a built binary). Run locally (auto-builds the pager):
//! ```bash
//! cargo test -p echo-agent-runtime --test test_tool_dispatch_duration_smoke -- --ignored
//! ```

#![cfg(unix)]

use std::path::Path;
use std::time::Duration;

use echo_agent_test_support::sse::{
    chat_completions_reasoning_then_tool_call_events, responses_api_reasoning_then_tool_call_events,
};
use echo_agent_test_support::*;

const CALL_ID: &str = "call_sleep_timing";
const SLEEP_SECS: u64 = 2;

fn enqueue_sleep_tool_turn(server: &MockInferenceServer) {
    let args = serde_json::json!({
        "command": format!("sleep {SLEEP_SECS}"),
        "description": "timing smoke sleep",
    })
    .to_string();

    // Both backends: mock may pick either depending on model/settings.
    server.enqueue_response(
        "/v1/responses",
        ScriptedResponse::sse(responses_api_reasoning_then_tool_call_events(
            "running a short sleep",
            CALL_ID,
            "run_terminal_command",
            &args,
            "test-model",
        )),
    );
    server.enqueue_response(
        "/v1/chat/completions",
        ScriptedResponse::sse(chat_completions_reasoning_then_tool_call_events(
            "running a short sleep",
            CALL_ID,
            "run_terminal_command",
            &args,
            "test-model",
        )),
    );
    // After tool result, model finishes with plain text.
    server.set_response("slept");
}

/// Shell `tool_completed` for this call id. Workspace rows share the id and can
/// also be multi-second (hub hop), so require omitted `source`.
fn find_tool_completed(events_jsonl: &str, call_id: &str) -> Option<serde_json::Value> {
    events_jsonl.lines().find_map(|line| {
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        let is_match = v.get("type").and_then(|t| t.as_str()) == Some("tool_completed")
            && v.get("tool_call_id").and_then(|t| t.as_str()) == Some(call_id)
            && v.get("source").is_none();
        is_match.then_some(v)
    })
}

fn collect_events_jsonl(root: &Path) -> Vec<(std::path::PathBuf, String)> {
    let mut out = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<(std::path::PathBuf, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.file_name().and_then(|n| n.to_str()) == Some("events.jsonl")
                && let Ok(text) = std::fs::read_to_string(&path)
            {
                out.push((path, text));
            }
        }
    }
    walk(root, &mut out);
    out
}

#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn sleep_tool_records_multi_second_dispatch_duration() {
    let server = MockInferenceServer::start()
        .await
        .expect("start mock server");
    enqueue_sleep_tool_turn(&server);

    let sandbox = TestSandbox::builder().mock_url(server.url()).git().build();

    let mut cmd = tokio::process::Command::new(echo_agent_binary());
    cmd.args(["-p", "please sleep two seconds", "--yolo"])
        .current_dir(sandbox.workspace());

    let started = std::time::Instant::now();
    let result = run_headless_in_sandbox_borrowed(cmd, &sandbox).await;
    let wall = started.elapsed();

    eprintln!(
        "headless elapsed={wall:?} timed_out={} status={:?}\nstderr_tail:\n{}",
        result.timed_out,
        result.status.code(),
        stderr_tail(&result.stderr, 2500)
    );

    assert_headless_success(&result, "echoagent -p sleep timing smoke", Some(&server));
    assert_no_crashes(&result.stderr);

    // Session artifacts live under the sandbox ECHO_AGENT_HOME.
    let home = sandbox.echo_agent_home();
    let events_files = collect_events_jsonl(home);
    assert!(
        !events_files.is_empty(),
        "no events.jsonl under ECHO_AGENT_HOME {}\nstderr:\n{}",
        home.display(),
        stderr_tail(&result.stderr, 2000)
    );

    let (path, ev) = events_files
        .iter()
        .find_map(|(path, text)| find_tool_completed(text, CALL_ID).map(|ev| (path, ev)))
        .unwrap_or_else(|| {
            panic!(
                "no shell tool_completed (source omitted) for {CALL_ID} under {}\nfiles: {:?}",
                home.display(),
                events_files
                    .iter()
                    .map(|(p, t)| (p.display().to_string(), t.lines().count()))
                    .collect::<Vec<_>>()
            )
        });

    let duration_ms = ev
        .get("duration_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or_else(|| panic!("duration_ms missing in {ev} ({})", path.display()));
    let tool_name = ev.get("tool_name").and_then(|v| v.as_str()).unwrap_or("?");

    eprintln!(
        "tool_completed path={} tool_name={tool_name} duration_ms={duration_ms}",
        path.display()
    );

    // A post-flight clock reports single-digit ms for this call; 1.5s floor
    // absorbs CI jitter, the ceiling only catches a nonsense value.
    assert!(
        duration_ms >= 1_500,
        "expected duration_ms >= 1500 for sleep {SLEEP_SECS}s, got {duration_ms}ms \
         (if ~0–50ms, dispatch timing regressed to post-flight clock)"
    );
    assert!(
        duration_ms < 30_000,
        "duration_ms={duration_ms} implausibly high for sleep {SLEEP_SECS}"
    );

    // Sanity: the sleep really happened inside this run.
    assert!(
        wall >= Duration::from_millis(1_500),
        "headless wall {wall:?} shorter than sleep — tool may not have run"
    );
}
