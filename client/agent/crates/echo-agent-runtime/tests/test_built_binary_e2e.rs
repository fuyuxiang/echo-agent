//! Built-binary end-to-end tests for the echoagent (echo-agent-pager) binary.
//!
//! These tests verify that the built echoagent binary works end-to-end against a mock
//! inference server. They catch dynamic linking failures (libgit2/OpenSSL),
//! session initialization crashes, and protocol regressions.
//!
//! The tests exercise:
//! - **Smoke** (`echoagent --version`): binary loads without crashing
//! - **ACP stdio** (`echoagent agent stdio`): full protocol lifecycle via ClientSideConnection
//!
//! Tests are `#[ignore]`d by default — they require a pre-built binary.
//!
//! Run locally (auto-builds the binary if not already present):
//! ```bash
//! cargo test -p echo-agent-runtime --test test_built_binary_e2e -- --ignored
//! ```
//!
//! In CI, set `ECHO_AGENT_BINARY` to point at the release artifact:
//! ```bash
//! ECHO_AGENT_BINARY=./artifacts/echo-agent-0.1.159-linux-x86_64 \
//!   cargo test -p echo-agent-runtime --test test_built_binary_e2e -- --ignored
//! ```

use std::future::Future;
use std::process::Command;
use std::time::Duration;

use echo_agent_test_support::*;
use serde_json::Value;

/// Run an async test body inside a `LocalSet` (required by ACP's `!Send` futures).
/// Eliminates the `let local = LocalSet::new(); local.run_until(async { ... }).await`
/// boilerplate from every stdio test.
async fn with_local_set<F, Fut>(f: F)
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()>,
{
    tokio::task::LocalSet::new().run_until(f()).await;
}

const CHAT_COMPLETIONS_MODEL: &str = "chat-completions-model";

/// Start a mock server with one model named `model` on the given API backend.
async fn single_model_server(model: &str, backend: &str) -> MockInferenceServer {
    MockInferenceServer::start_with_models(vec![
        MockModelEntry::new(model).with_api_backend(backend),
    ])
    .await
    .expect("start mock server")
}

async fn echo_agent_build_server() -> MockInferenceServer {
    MockInferenceServer::start_with_models(vec![
        MockModelEntry::with_agent_type("echo-agent-4.5", "echo-agent-build")
            .with_api_backend("responses")
            .with_supports_backend_search(true),
    ])
    .await
    .expect("start mock server")
}

/// Parse a headless run's stdout as a single JSON object.
fn parse_stdout_json(result: &HeadlessResult) -> serde_json::Value {
    serde_json::from_str(result.stdout.trim())
        .unwrap_or_else(|e| panic!("stdout not valid JSON: {e}\n{}", result.stdout))
}

fn request_tool_name(tool: &Value) -> Option<&str> {
    tool.pointer("/function/name")
        .or_else(|| tool.get("name"))
        .and_then(Value::as_str)
        .or_else(|| {
            tool.get("type")
                .and_then(Value::as_str)
                .and_then(|kind| kind.starts_with("web_search").then_some("web_search"))
        })
}

fn inference_request(server: &MockInferenceServer) -> Value {
    server
        .request_bodies()
        .into_iter()
        .find(|body| {
            body.get("tools")
                .and_then(Value::as_array)
                .is_some_and(|tools| {
                    !tools.is_empty()
                        && !tools
                            .iter()
                            .any(|tool| request_tool_name(tool) == Some("session_title"))
                })
        })
        .expect("mock server should receive a main inference request with tools")
}

fn inference_tool_names(server: &MockInferenceServer) -> Vec<String> {
    let request = inference_request(server);
    request["tools"]
        .as_array()
        .expect("inference request tools should be an array")
        .iter()
        .filter_map(request_tool_name)
        .map(str::to_owned)
        .collect()
}

// ============================================================================
// Smoke tests
// ============================================================================

/// Smoke test: the binary loads and exits without crashing.
/// This does NOT require the mock server — it's the absolute minimum bar.
#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn test_version_exits_zero() {
    let binary = echo_agent_binary();
    let output = Command::new(&binary)
        .arg("--version")
        .output()
        .unwrap_or_else(|e| panic!("failed to run {}: {e}", binary.display()));

    assert!(
        output.status.success(),
        "echoagent --version failed (exit {:?}):\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Verify the crash handler installs without interfering with normal startup.
/// Exercises install() (sigaction, sigaltstack, mmap, ucontext struct layouts)
/// on every platform the binary is built for.
#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn test_version_with_crash_handler_exits_zero() {
    let binary = echo_agent_binary();
    let output = Command::new(&binary)
        .arg("--version")
        .env("ECHO_AGENT_CRASH_HANDLER", "1")
        .output()
        .unwrap_or_else(|e| panic!("failed to run {}: {e}", binary.display()));

    assert!(
        output.status.success(),
        "echoagent --version with ECHO_AGENT_CRASH_HANDLER=1 failed (exit {:?}):\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// THE critical test. Exercises the full session lifecycle in a git repo:
/// binary start → agent init → libgit2 init → fs watchers → session create →
/// model resolve → inference request to mock server → SSE parse → response render → exit.
///
/// This catches the recurring libgit2/OpenSSL dynamic linking bug that has
/// caused ~5 broken releases.
#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn test_headless_session_in_git_repo() {
    let server = MockInferenceServer::start()
        .await
        .expect("start mock server");
    let workdir = git_workdir();
    let result = run_headless(&server, &["-p", "say hello", "--yolo"], workdir.workspace()).await;

    assert_headless_success(&result, "echoagent -p in git repo", Some(&server));
    assert_no_crashes(&result.stderr);
    assert!(
        server.request_count() > 0,
        "mock server received no inference requests\nrequest log:\n{}",
        server.request_log_summary()
    );
    assert!(
        server.has_chat_completion_request() || server.has_responses_request(),
        "headless mode should hit /v1/chat/completions or /v1/responses\nrequest log:\n{}",
        server.request_log_summary()
    );
}

/// Verify echoagent works in a non-git directory (exercises the fallback codepath
/// where libgit2 discovers there's no repo instead of initializing one).
#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn test_headless_session_in_non_git_dir() {
    let server = MockInferenceServer::start()
        .await
        .expect("start mock server");
    let workdir = tempfile::tempdir().unwrap();
    std::fs::write(workdir.path().join("test.txt"), "test\n").unwrap();

    let result = run_headless(&server, &["-p", "say hello", "--yolo"], workdir.path()).await;

    assert_headless_success(&result, "echoagent -p in non-git dir", Some(&server));
    assert_no_crashes(&result.stderr);
}

#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn test_headless_tools_allowlist_keeps_enabled_web_tools() {
    let server = echo_agent_build_server().await;
    server.preset_allow_access();
    let workdir = git_workdir();

    let result = run_headless_with_env(
        &server,
        &[
            "-p",
            "say hello",
            "--yolo",
            "--tools",
            "read_file,grep,list_dir,web_search,web_fetch",
        ],
        workdir.workspace(),
        &[("ECHO_AGENT_WEB_FETCH", "1")],
    )
    .await;

    assert_headless_success(
        &result,
        "echoagent -p --tools with web tools",
        Some(&server),
    );
    assert_no_crashes(&result.stderr);
    let names = inference_tool_names(&server);
    for expected in ["read_file", "grep", "list_dir", "web_search", "web_fetch"] {
        assert!(names.iter().any(|name| name == expected), "got: {names:?}");
    }
    for excluded in ["run_terminal_command", "search_replace"] {
        assert!(!names.iter().any(|name| name == excluded), "got: {names:?}");
    }
    let request = inference_request(&server);
    let tools = request["tools"]
        .as_array()
        .expect("inference request tools should be an array");
    assert!(
        tools.iter().any(|tool| {
            tool.get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind.starts_with("web_search"))
        }),
        "backend-capable model should receive hosted web search: {tools:?}"
    );
    assert!(
        !tools.iter().any(|tool| {
            tool.get("type").and_then(Value::as_str) == Some("function")
                && tool.get("name").and_then(Value::as_str) == Some("web_search")
        }),
        "backend-capable model must not receive function web_search: {tools:?}"
    );
    assert!(
        tools.iter().any(|tool| {
            tool.get("type").and_then(Value::as_str) == Some("function")
                && tool.get("name").and_then(Value::as_str) == Some("web_fetch")
        }),
        "web_fetch should remain a function tool: {tools:?}"
    );
}

#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn test_headless_tools_allowlist_does_not_fail_open_for_disabled_web_fetch() {
    let server = echo_agent_build_server().await;
    server.set_settings(serde_json::json!({
        "allow_access": true,
        "web_fetch_enabled": false,
    }));
    let workdir = git_workdir();

    let result = run_headless_with_env(
        &server,
        &[
            "-p",
            "say hello",
            "--yolo",
            "--tools",
            "read_file,web_fetch",
        ],
        workdir.workspace(),
        &[("ECHO_AGENT_WEB_FETCH", "0")],
    )
    .await;

    assert_headless_success(
        &result,
        "echoagent -p --tools with disabled web_fetch",
        Some(&server),
    );
    assert_no_crashes(&result.stderr);
    let names = inference_tool_names(&server);
    assert!(
        names.iter().any(|name| name == "read_file"),
        "got: {names:?}"
    );
    for excluded in ["web_fetch", "run_terminal_command", "search_replace"] {
        assert!(!names.iter().any(|name| name == excluded), "got: {names:?}");
    }
}

#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn test_headless_terminal_only_allowlist_is_foreground_only() {
    let server = echo_agent_build_server().await;
    let workdir = git_workdir();

    let result = run_headless(
        &server,
        &["-p", "say hello", "--yolo", "--tools", "run_terminal_cmd"],
        workdir.workspace(),
    )
    .await;

    assert_headless_success(
        &result,
        "echoagent -p --tools run_terminal_cmd",
        Some(&server),
    );
    assert_no_crashes(&result.stderr);
    let request = inference_request(&server);
    let terminal = request["tools"]
        .as_array()
        .expect("inference request tools should be an array")
        .iter()
        .find(|tool| request_tool_name(tool) == Some("run_terminal_command"))
        .expect("terminal tool should remain in the allowlist");
    let properties = terminal
        .pointer("/function/parameters/properties")
        .or_else(|| terminal.pointer("/parameters/properties"))
        .and_then(Value::as_object)
        .expect("terminal tool should have an input schema");
    assert!(
        !properties.contains_key("is_background"),
        "foreground-only terminal schema must omit is_background: {terminal}"
    );
}

/// Free-usage paywall in headless mode: 429s whose flat body carries the
/// `subscription:free-usage-exhausted` well-known code must surface the
/// pager's free-usage message instead of the generic rate-limit one. The
/// code reaches the pager embedded in the flattened error text (no
/// structured plumbing), so this exercises the whole detection path.
#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn test_headless_free_usage_exhausted_prints_paywall_message() {
    let server = MockInferenceServer::start()
        .await
        .expect("start mock server");
    let free_usage = || {
        ScriptedResponse::json(
            429,
            serde_json::json!({
                "code": "subscription:free-usage-exhausted",
                "error": "You have used all your free usage."
            }),
        )
    };
    // The binary may target either backend, generic-429 handling retries
    // once before going fatal, and the background session-title generation
    // may consume a script on the same path — queue three per path (any
    // leftovers are simply unused).
    for path in ["/v1/chat/completions", "/v1/responses"] {
        for _ in 0..3 {
            server.enqueue_response(path, free_usage());
        }
    }
    let workdir = git_workdir();

    let result = run_headless(&server, &["-p", "say hello", "--yolo"], workdir.workspace()).await;

    assert!(
        !result.timed_out && !result.status.success(),
        "a free-usage-exhausted turn must finish and exit non-zero\nstderr tail:\n{}",
        stderr_tail(&result.stderr, 500)
    );
    assert_no_crashes(&result.stderr);
    let combined = format!("{}\n{}", result.stdout, result.stderr);
    assert!(
        combined.contains("reached your free EchoAgent Build usage limit"),
        "expected the free-usage paywall message\nstdout:\n{}\nstderr tail:\n{}",
        result.stdout,
        stderr_tail(&result.stderr, 1000)
    );
    assert!(
        !combined.contains("hit the rate limit for your plan"),
        "generic rate-limit message must be replaced by the paywall text"
    );
}

/// Verify the streaming JSON output format works end-to-end.
/// This is the format used by programmatic integrations (`--output-format streaming-json`).
#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn test_headless_streaming_json_output() {
    let server = MockInferenceServer::start()
        .await
        .expect("start mock server");
    let workdir = git_workdir();
    let result = run_headless(
        &server,
        &[
            "-p",
            "say hello",
            "--yolo",
            "--output-format",
            "streaming-json",
        ],
        workdir.workspace(),
    )
    .await;

    assert_headless_success(
        &result,
        "echoagent -p --output-format streaming-json",
        Some(&server),
    );
    assert_no_crashes(&result.stderr);

    let events: Vec<serde_json::Value> = result
        .stdout
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .unwrap_or_else(|e| panic!("invalid streaming-json line `{line}`: {e}"))
        })
        .collect();
    assert!(
        !events.is_empty(),
        "streaming-json stdout should not be empty"
    );
    assert!(
        !events
            .iter()
            .any(|event| event.get("type").and_then(serde_json::Value::as_str) == Some("error")),
        "streaming-json emitted an error event: {:?}",
        events
    );
    assert!(
        events
            .iter()
            .any(|event| event.get("type").and_then(serde_json::Value::as_str) == Some("text")),
        "streaming-json output should include at least one text event: {:?}",
        events
    );
    assert_eq!(
        events
            .last()
            .and_then(|event| event.get("type"))
            .and_then(serde_json::Value::as_str),
        Some("end"),
        "streaming-json output should end with an `end` event: {:?}",
        events
    );
}

/// `streaming-messages-json` emits `system`/`init`, message wrapped assistant
/// messages, and a terminal `result`.
#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn test_headless_streaming_messages_json_output() {
    let server = MockInferenceServer::start()
        .await
        .expect("start mock server");
    let workdir = git_workdir();
    let result = run_headless(
        &server,
        &[
            "-p",
            "say hello",
            "--yolo",
            "--output-format",
            "streaming-messages-json",
        ],
        workdir.workspace(),
    )
    .await;

    assert_headless_success(
        &result,
        "echoagent -p --output-format streaming-messages-json",
        Some(&server),
    );
    assert_no_crashes(&result.stderr);

    let messages: Vec<serde_json::Value> = result
        .stdout
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .unwrap_or_else(|e| panic!("invalid streaming-messages-json line `{line}`: {e}"))
        })
        .collect();
    fn type_of(m: &serde_json::Value) -> Option<&str> {
        m.get("type").and_then(serde_json::Value::as_str)
    }

    let first = &messages[0];
    assert_eq!(type_of(first), Some("system"), "{messages:?}");
    assert_eq!(first["subtype"], "init", "{messages:?}");

    let assistant = messages
        .iter()
        .find(|m| type_of(m) == Some("assistant"))
        .unwrap_or_else(|| panic!("expected an assistant message: {messages:?}"));
    assert!(assistant["message"]["content"].is_array(), "{assistant:?}");

    let last = messages.last().expect("a result message");
    assert_eq!(type_of(last), Some("result"), "{messages:?}");
    assert_eq!(last["subtype"], "success", "{last:?}");
    assert_eq!(last["is_error"], false, "{last:?}");
}

/// The Messages backend reports message id, thinking signature, verbatim stop
/// reason, and per-response usage; all four must land on the assistant frame.
#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn test_headless_streaming_messages_json_carries_per_response_metadata() {
    use echo_agent_test_support::scripted::{ScriptedResponse, SseEvent};
    use serde_json::json;

    let model = "messages-compatible-model";
    let server = single_model_server(model, "messages").await;
    server.enqueue_response(
        "/v1/messages",
        ScriptedResponse::sse(vec![
            SseEvent::data(
                json!({"type":"message_start","message":{"id":"msg_e2e_9","type":"message","role":"assistant","content":[],"model":model,"stop_reason":null,"usage":{"input_tokens":12,"output_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}).to_string(),
            ),
            SseEvent::data(
                json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}).to_string(),
            ),
            SseEvent::data(
                json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"weighing it"}}).to_string(),
            ),
            SseEvent::data(
                json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig-e2e-abc"}}).to_string(),
            ),
            SseEvent::data(json!({"type":"content_block_stop","index":0}).to_string()),
            SseEvent::data(
                json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}).to_string(),
            ),
            SseEvent::data(
                json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hello there"}}).to_string(),
            ),
            SseEvent::data(json!({"type":"content_block_stop","index":1}).to_string()),
            SseEvent::data(
                json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7,"input_tokens":12}}).to_string(),
            ),
            SseEvent::data(json!({"type":"message_stop"}).to_string()),
        ]),
    );

    let workdir = git_workdir();
    let result = run_headless(
        &server,
        &[
            "-p",
            "say hi",
            "--yolo",
            "--model",
            model,
            "--max-turns",
            "1",
            "--output-format",
            "streaming-messages-json",
        ],
        workdir.workspace(),
    )
    .await;

    assert_headless_success(
        &result,
        "streaming-messages-json per-response metadata",
        Some(&server),
    );
    assert_no_crashes(&result.stderr);

    let messages: Vec<serde_json::Value> = result
        .stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .unwrap_or_else(|e| panic!("invalid streaming-messages-json line `{line}`: {e}"))
        })
        .collect();

    let assistant = messages
        .iter()
        .find(|m| m.get("type").and_then(serde_json::Value::as_str) == Some("assistant"))
        .unwrap_or_else(|| panic!("expected an assistant message: {messages:?}"));
    let message = &assistant["message"];
    assert_eq!(
        message["id"], "msg_e2e_9",
        "real provider message id: {assistant:?}"
    );
    assert_eq!(
        message["stop_reason"], "end_turn",
        "verbatim wire stop reason: {assistant:?}"
    );
    assert_eq!(
        message["usage"]["input_tokens"], 12,
        "per-response usage: {assistant:?}"
    );
    assert_eq!(
        message["usage"]["output_tokens"], 7,
        "per-response usage: {assistant:?}"
    );

    let thinking = message["content"]
        .as_array()
        .unwrap_or_else(|| panic!("assistant content must be an array: {assistant:?}"))
        .iter()
        .find(|b| b["type"] == "thinking")
        .unwrap_or_else(|| panic!("expected a thinking block: {assistant:?}"));
    assert_eq!(
        thinking["signature"], "sig-e2e-abc",
        "thinking block must carry the reasoning signature: {assistant:?}"
    );
}

/// End-to-end pipeline: a Messages-backend turn that stops on a configured stop
/// sequence must carry the provider's matched sequence all the way through the
/// sampler → shell `response_completed` → `streaming-messages-json` reducer, so
/// the flushed `assistant` frame reads `stop_reason: "stop_sequence"` with the
/// real `message.stop_sequence`. Drives the actual wire (a scripted
/// `message_delta`), not a hand-built reducer event.
#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn test_headless_streaming_messages_json_carries_stop_sequence() {
    use echo_agent_test_support::scripted::{ScriptedResponse, SseEvent};
    use serde_json::json;

    let model = "messages-compatible-model";
    let server = single_model_server(model, "messages").await;
    server.enqueue_response(
        "/v1/messages",
        ScriptedResponse::sse(vec![
            SseEvent::data(
                json!({"type":"message_start","message":{"id":"msg_stop_seq","type":"message","role":"assistant","content":[],"model":model,"stop_reason":null,"usage":{"input_tokens":8,"output_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}).to_string(),
            ),
            SseEvent::data(
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}).to_string(),
            ),
            SseEvent::data(
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"stopping here"}}).to_string(),
            ),
            SseEvent::data(json!({"type":"content_block_stop","index":0}).to_string()),
            // The matched stop sequence rides the terminal `message_delta`.
            SseEvent::data(
                json!({"type":"message_delta","delta":{"stop_reason":"stop_sequence","stop_sequence":"<END>"},"usage":{"output_tokens":3,"input_tokens":8}}).to_string(),
            ),
            SseEvent::data(json!({"type":"message_stop"}).to_string()),
        ]),
    );

    let workdir = git_workdir();
    let result = run_headless(
        &server,
        &[
            "-p",
            "emit the stop token",
            "--yolo",
            "--model",
            model,
            "--max-turns",
            "1",
            "--output-format",
            "streaming-messages-json",
        ],
        workdir.workspace(),
    )
    .await;

    assert_headless_success(
        &result,
        "streaming-messages-json stop_sequence",
        Some(&server),
    );
    assert_no_crashes(&result.stderr);

    let messages: Vec<serde_json::Value> = result
        .stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .unwrap_or_else(|e| panic!("invalid streaming-messages-json line `{line}`: {e}"))
        })
        .collect();

    let assistant = messages
        .iter()
        .find(|m| m.get("type").and_then(serde_json::Value::as_str) == Some("assistant"))
        .unwrap_or_else(|| panic!("expected an assistant message: {messages:?}"));
    let message = &assistant["message"];
    assert_eq!(
        message["stop_reason"], "stop_sequence",
        "verbatim stop reason: {assistant:?}"
    );
    assert_eq!(
        message["stop_sequence"], "<END>",
        "matched stop sequence carried end-to-end: {assistant:?}"
    );
}

#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn test_headless_json_reports_server_cost() {
    use echo_agent_test_support::scripted::SseEvent;

    let server = single_model_server(CHAT_COMPLETIONS_MODEL, "chat_completions").await;
    let chunk = |body: serde_json::Value| SseEvent::data(body.to_string());
    server.enqueue_response(
        "/v1/chat/completions",
        echo_agent_test_support::scripted::ScriptedResponse::sse(vec![
            chunk(serde_json::json!({
                "id": "chatcmpl-cost", "object": "chat.completion.chunk", "created": 0,
                "model": CHAT_COMPLETIONS_MODEL,
                "choices": [{ "index": 0, "delta": { "content": "4" }, "finish_reason": "stop" }]
            })),
            chunk(serde_json::json!({
                "id": "chatcmpl-cost", "object": "chat.completion.chunk", "created": 0,
                "model": CHAT_COMPLETIONS_MODEL, "choices": [],
                "usage": {
                    "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15,
                    "cost_in_usd_ticks": 1_234_500_000_i64
                }
            })),
            SseEvent::data("[DONE]"),
        ]),
    );

    let workdir = git_workdir();
    let result = run_headless(
        &server,
        &[
            "-p",
            "what is 2+2",
            "--yolo",
            "--model",
            CHAT_COMPLETIONS_MODEL,
            "--max-turns",
            "1",
            "--output-format",
            "json",
        ],
        workdir.workspace(),
    )
    .await;

    assert_headless_success(&result, "echoagent -p (scripted cost)", Some(&server));
    let output = parse_stdout_json(&result);
    assert_eq!(output["total_cost_usd"], 0.12345);
    assert_eq!(output["total_cost_usd_ticks"], 1_234_500_000_i64);
    assert!(output.get("cost_is_partial").is_none());
    assert!(output["usage"]["input_tokens"].as_u64().unwrap() >= 10);
    assert_eq!(output["num_turns"], 1);
    let (_, model) = output["modelUsage"]
        .as_object()
        .expect("modelUsage")
        .iter()
        .next()
        .expect("one model");
    assert_eq!(model["costUSD"], 0.12345);
    assert!(model["modelCalls"].as_u64().unwrap() >= 1);
}

#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn test_headless_json_reports_usage_on_max_turns() {
    let server = single_model_server(CHAT_COMPLETIONS_MODEL, "chat_completions").await;
    server.enqueue_response(
        "/v1/chat/completions",
        echo_agent_test_support::scripted::ScriptedResponse::sse(
            echo_agent_test_support::sse::chat_completions_reasoning_then_tool_call_events(
                "let me look",
                "call-1",
                "read_file",
                r#"{"path":"README.md"}"#,
                CHAT_COMPLETIONS_MODEL,
            ),
        ),
    );

    let workdir = git_workdir();
    let result = run_headless(
        &server,
        &[
            "-p",
            "read the readme",
            "--yolo",
            "--model",
            CHAT_COMPLETIONS_MODEL,
            "--max-turns",
            "1",
            "--output-format",
            "json",
        ],
        workdir.workspace(),
    )
    .await;

    assert!(!result.status.success());
    let output = parse_stdout_json(&result);
    assert!(output["usage"]["input_tokens"].as_u64().unwrap() >= 10);
    assert!(output["num_turns"].as_u64().unwrap() >= 1);
}

#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn test_headless_streaming_json_usage() {
    let server = single_model_server(CHAT_COMPLETIONS_MODEL, "chat_completions").await;
    let workdir = git_workdir();
    let result = run_headless(
        &server,
        &[
            "-p",
            "say hello",
            "--yolo",
            "--model",
            CHAT_COMPLETIONS_MODEL,
            "--output-format",
            "streaming-json",
        ],
        workdir.workspace(),
    )
    .await;

    assert_headless_success(&result, "streaming-json usage", Some(&server));
    let events: Vec<serde_json::Value> = result
        .stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid NDJSON line"))
        .collect();
    let end = events.last().unwrap();
    assert_eq!(end["type"], "end");
    assert!(end["usage"]["input_tokens"].as_u64().unwrap() >= 10);
    assert!(end["num_turns"].as_u64().unwrap() >= 1);
}

/// Chat Completions backend: the schema is enforced natively via
/// `response_format`, and the model's final JSON answer surfaces as
/// `structuredOutput`. The StructuredOutput tool is NOT used.
#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn headless_json_schema_chat_completions_uses_response_format() {
    let server = single_model_server(CHAT_COMPLETIONS_MODEL, "chat_completions").await;
    server.set_response(r#"{"name":"Alice","age":30}"#);

    let workdir = git_workdir();
    let result = run_headless(
        &server,
        &[
            "-p",
            "extract name and age",
            "--yolo",
            "--model",
            CHAT_COMPLETIONS_MODEL,
            "--json-schema",
            r#"{"type":"object","properties":{"name":{"type":"string"},"age":{"type":"integer"}},"required":["name","age"],"additionalProperties":false}"#,
            "--max-turns",
            "1",
        ],
        workdir.workspace(),
    )
    .await;

    assert_headless_success(
        &result,
        "echoagent -p --json-schema (chat_completions)",
        Some(&server),
    );
    assert_no_crashes(&result.stderr);

    let output = parse_stdout_json(&result);
    assert_eq!(output["structuredOutput"]["name"], "Alice");
    assert_eq!(output["structuredOutput"]["age"], 30);
    assert!(output.get("structuredOutputError").is_none());

    // Native path: the schema rides response_format; the StructuredOutput tool
    // is never advertised.
    let response_format_on_wire = server.requests().iter().any(|r| {
        r.body.as_ref().is_some_and(|body| {
            body.pointer("/response_format/type")
                .and_then(|v| v.as_str())
                == Some("json_schema")
        })
    });
    assert!(
        response_format_on_wire,
        "response_format json_schema must reach the wire\n{}",
        server.request_log_summary()
    );
    assert!(
        !any_request_advertises_structured_output_tool(&server),
        "native path must NOT advertise the StructuredOutput tool\n{}",
        server.request_log_summary()
    );
}

/// Responses backend: native schema rides `text.format` (not the tool), and the
/// final JSON answer surfaces as `structuredOutput`.
#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn headless_json_schema_responses_uses_text_format() {
    let server = single_model_server("echo-agent-4.5", "responses").await;
    server.set_response(r#"{"name":"Alice","age":30}"#);

    let workdir = git_workdir();
    let result = run_headless(
        &server,
        &[
            "-p",
            "extract name and age",
            "--yolo",
            "--model",
            "echo-agent-4.5",
            "--json-schema",
            NAME_AGE_SCHEMA,
            "--max-turns",
            "1",
        ],
        workdir.workspace(),
    )
    .await;

    assert_headless_success(
        &result,
        "echoagent -p --json-schema (responses)",
        Some(&server),
    );
    assert_no_crashes(&result.stderr);

    let output = parse_stdout_json(&result);
    assert_eq!(output["structuredOutput"]["name"], "Alice");
    assert_eq!(output["structuredOutput"]["age"], 30);
    assert!(output.get("structuredOutputError").is_none());

    let text_format_on_wire = server.requests().iter().any(|r| {
        r.body.as_ref().is_some_and(|body| {
            body.pointer("/text/format/type").and_then(|v| v.as_str()) == Some("json_schema")
        })
    });
    assert!(
        text_format_on_wire,
        "text.format json_schema must reach the wire\n{}",
        server.request_log_summary()
    );
    assert!(
        !any_request_advertises_structured_output_tool(&server),
        "native path must NOT advertise the StructuredOutput tool\n{}",
        server.request_log_summary()
    );
}

/// Messages backend (Anthropic-style) can't constrain output natively, so the
/// model returns its answer by calling the synthetic `StructuredOutput` tool.
/// Verifies the tool reaches the wire and its validated args surface as
/// `structuredOutput`.
#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn headless_json_schema_messages_backend_uses_structured_output_tool() {
    let server = single_model_server("messages-compatible-model", "messages").await;
    server.enqueue_response(
        "/v1/messages",
        structured_output_tool_call_sse("messages-compatible-model", r#"{"name":"Bob","age":42}"#),
    );

    let workdir = git_workdir();
    let result = run_headless(
        &server,
        &[
            "-p",
            "extract name and age",
            "--yolo",
            "--model",
            "messages-compatible-model",
            "--json-schema",
            r#"{"type":"object","properties":{"name":{"type":"string"},"age":{"type":"integer"}},"required":["name","age"],"additionalProperties":false}"#,
            "--max-turns",
            "2",
        ],
        workdir.workspace(),
    )
    .await;

    assert_headless_success(
        &result,
        "echoagent -p --json-schema (messages)",
        Some(&server),
    );
    assert_no_crashes(&result.stderr);

    let output = parse_stdout_json(&result);
    assert_eq!(output["structuredOutput"]["name"], "Bob");
    assert_eq!(output["structuredOutput"]["age"], 42);
    assert!(output.get("structuredOutputError").is_none());

    // The schema is advertised as the StructuredOutput tool, not response_format.
    assert!(
        any_request_advertises_structured_output_tool(&server),
        "StructuredOutput tool must reach the wire\n{}",
        server.request_log_summary()
    );
}

/// Whether any request advertised a tool named `StructuredOutput` in `tools[]`.
fn any_request_advertises_structured_output_tool(server: &MockInferenceServer) -> bool {
    server.requests().iter().any(|r| {
        r.body.as_ref().is_some_and(|body| {
            body.pointer("/tools")
                .and_then(|t| t.as_array())
                .is_some_and(|tools| {
                    tools
                        .iter()
                        .any(|t| t.get("name").and_then(|n| n.as_str()) == Some("StructuredOutput"))
                })
        })
    })
}

/// Anthropic Messages API SSE that streams a single `StructuredOutput` tool call
/// whose input is `args_json`.
fn structured_output_tool_call_sse(model: &str, args_json: &str) -> ScriptedResponse {
    use serde_json::json;
    ScriptedResponse::sse(vec![
        SseEvent::data(
            json!({"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","content":[],"model":model,"stop_reason":null,"usage":{"input_tokens":10,"output_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}).to_string(),
        ),
        SseEvent::data(
            json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"StructuredOutput","input":{}}}).to_string(),
        ),
        SseEvent::data(
            json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":args_json}}).to_string(),
        ),
        SseEvent::data(json!({"type":"content_block_stop","index":0}).to_string()),
        SseEvent::data(
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":5,"input_tokens":10}}).to_string(),
        ),
        SseEvent::data(json!({"type":"message_stop"}).to_string()),
    ])
}

const NAME_AGE_SCHEMA: &str = r#"{"type":"object","properties":{"name":{"type":"string"},"age":{"type":"integer"}},"required":["name","age"],"additionalProperties":false}"#;

/// Messages backend, model ignores the StructuredOutput tool and answers as
/// prose: the turn-end fallback still validates the text against the schema and
/// surfaces `structuredOutput` (closes the "unvalidated fallback" gap).
#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn headless_json_schema_messages_validates_text_when_tool_not_called() {
    let server = single_model_server("messages-compatible-model", "messages").await;
    server.set_response(r#"{"name":"Cara","age":7}"#);

    let workdir = git_workdir();
    let result = run_headless(
        &server,
        &[
            "-p",
            "extract name and age",
            "--yolo",
            "--model",
            "messages-compatible-model",
            "--json-schema",
            NAME_AGE_SCHEMA,
            "--max-turns",
            "1",
        ],
        workdir.workspace(),
    )
    .await;

    assert_headless_success(
        &result,
        "echoagent -p --json-schema (messages, text)",
        Some(&server),
    );
    assert_no_crashes(&result.stderr);

    let output = parse_stdout_json(&result);
    assert_eq!(output["structuredOutput"]["name"], "Cara");
    assert_eq!(output["structuredOutput"]["age"], 7);
    assert!(output.get("structuredOutputError").is_none());
}

/// Messages backend, first StructuredOutput call violates the schema (no `age`):
/// the agent feeds the error back and the model's retry conforms. Exercises the
/// validation + bounded-retry path.
#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn headless_json_schema_messages_retries_on_schema_violation() {
    let server = single_model_server("messages-compatible-model", "messages").await;
    server.enqueue_response(
        "/v1/messages",
        structured_output_tool_call_sse("messages-compatible-model", r#"{"name":"Dan"}"#),
    );
    server.enqueue_response(
        "/v1/messages",
        structured_output_tool_call_sse("messages-compatible-model", r#"{"name":"Dan","age":9}"#),
    );

    let workdir = git_workdir();
    let result = run_headless(
        &server,
        &[
            "-p",
            "extract name and age",
            "--yolo",
            "--model",
            "messages-compatible-model",
            "--json-schema",
            NAME_AGE_SCHEMA,
            "--max-turns",
            "3",
        ],
        workdir.workspace(),
    )
    .await;

    assert_headless_success(
        &result,
        "echoagent -p --json-schema (messages, retry)",
        Some(&server),
    );
    assert_no_crashes(&result.stderr);

    let output = parse_stdout_json(&result);
    assert_eq!(output["structuredOutput"]["name"], "Dan");
    assert_eq!(output["structuredOutput"]["age"], 9);
    assert!(output.get("structuredOutputError").is_none());
}

/// An invalid `--json-schema` (valid JSON object, but fails schema compilation)
/// disables both structured-output paths and surfaces the compile error.
#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn invalid_json_schema_disables_structured_output_and_surfaces_error() {
    let server = single_model_server(CHAT_COMPLETIONS_MODEL, "chat_completions").await;
    server.set_response(r#"{"name":"Alice","age":30}"#);

    let workdir = git_workdir();
    let result = run_headless(
        &server,
        &[
            "-p",
            "extract name and age",
            "--yolo",
            "--model",
            CHAT_COMPLETIONS_MODEL,
            // Valid JSON object, but `pattern` is an invalid regex → schema
            // compilation (`jsonschema::validator_for`) fails.
            "--json-schema",
            r#"{"type":"string","pattern":"["}"#,
            "--max-turns",
            "1",
        ],
        workdir.workspace(),
    )
    .await;

    assert_headless_success(
        &result,
        "echoagent -p --json-schema (invalid schema)",
        Some(&server),
    );
    assert_no_crashes(&result.stderr);

    let output = parse_stdout_json(&result);
    assert!(
        output["structuredOutput"].is_null(),
        "invalid schema must not produce a value\n{}",
        result.stdout
    );
    assert!(
        output["structuredOutputError"]
            .as_str()
            .is_some_and(|e| e.contains("invalid output schema")),
        "invalid schema must surface structuredOutputError\n{}",
        result.stdout
    );

    // Structured output disabled: no native response_format, no tool.
    let response_format_on_wire = server.requests().iter().any(|r| {
        r.body
            .as_ref()
            .is_some_and(|body| body.pointer("/response_format/type").is_some())
    });
    assert!(
        !response_format_on_wire,
        "invalid schema must NOT send response_format\n{}",
        server.request_log_summary()
    );
    assert!(
        !any_request_advertises_structured_output_tool(&server),
        "invalid schema must NOT advertise the StructuredOutput tool\n{}",
        server.request_log_summary()
    );
}

// ============================================================================
// ACP stdio tests (echoagent agent stdio)
//
// These test the agent as a server: spawn `echoagent agent stdio`, speak the full
// ACP protocol over pipes, verify the lifecycle works end-to-end.
// ============================================================================

/// Full ACP lifecycle: initialize → authenticate → create session → prompt.
/// Verifies the agent boots, authenticates with a test API key, creates a
/// session (libgit2 init), and completes a prompt round-trip to the mock server.
#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn test_stdio_full_session_lifecycle() {
    with_local_set(|| async {
        let server = MockInferenceServer::start().await.expect("start mock server");
        let workdir = git_workdir();
        let client = EchoAgentStdioClient::spawn(&server, workdir.workspace()).await;

        // Initialize and authenticate
        let init_resp = client.initialize_with_timeout().await;
        assert!(
            !init_resp.auth_methods.is_empty(),
            "agent should return at least one auth method"
        );

        // Create session (triggers libgit2 init)
        let session_id = client.create_session_with_timeout(workdir.workspace()).await;
        assert!(!session_id.0.is_empty(), "session ID should be non-empty");

        // Send prompt — triggers inference to mock server
        let result = client.prompt_with_timeout(&session_id, "say hello").await;
        assert!(
            result.is_ok(),
            "prompt failed: {:?}\nrequest log:\n{}\ncaptured text: {:?}\nnotifications: {}\nstderr:\n{}",
            result.err(),
            server.request_log_summary(),
            client.captured_text(),
            client.notification_count(),
            stderr_tail(&client.stderr(), 1200)
        );

        // Verify the mock server received at least one inference request
        assert!(
            server.request_count() > 0,
            "mock server received no inference requests\nrequest log:\n{}\nstderr:\n{}",
            server.request_log_summary(),
            stderr_tail(&client.stderr(), 1200)
        );
    })
    .await;
}

/// Verify that echo.agent/session/close frees the session.
/// Creates a session, closes it via ext_method, then verifies session/info
/// returns an empty response (session no longer exists).
#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn test_stdio_session_close() {
    with_local_set(|| async {
        let server = MockInferenceServer::start()
            .await
            .expect("start mock server");
        let workdir = git_workdir();
        let client = EchoAgentStdioClient::spawn(&server, workdir.workspace()).await;

        client.initialize_with_timeout().await;
        let session_id = client
            .create_session_with_timeout(workdir.workspace())
            .await;

        // Session should be alive — session/info returns data with sessionId
        let info_resp = client
            .ext_method(
                "echo.agent/session/info",
                serde_json::json!({ "sessionId": session_id.0.as_ref() }),
            )
            .await;
        assert!(
            info_resp.is_ok(),
            "session/info should succeed before close"
        );
        let info: serde_json::Value =
            serde_json::from_str(info_resp.unwrap().0.get()).expect("parse info");
        assert_eq!(
            info["result"]["sessionId"].as_str(),
            Some(session_id.0.as_ref()),
            "session/info should return the session we created, got: {info}"
        );

        // Close the session
        let close_resp = client
            .ext_method(
                "echo.agent/session/close",
                serde_json::json!({ "sessionId": session_id.0.as_ref() }),
            )
            .await;
        assert!(
            close_resp.is_ok(),
            "session/close failed: {:?}\nstderr:\n{}",
            close_resp.err(),
            stderr_tail(&client.stderr(), 1200)
        );

        // Session should be gone — session/info returns empty result (no sessionId)
        let info_after = client
            .ext_method(
                "echo.agent/session/info",
                serde_json::json!({ "sessionId": session_id.0.as_ref() }),
            )
            .await;
        assert!(info_after.is_ok(), "session/info should still succeed");
        let info_val: serde_json::Value =
            serde_json::from_str(info_after.unwrap().0.get()).expect("parse info after close");
        assert!(
            info_val["result"].get("sessionId").is_none(),
            "session/info should not contain sessionId after close, got: {info_val}"
        );
    })
    .await;
}

#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn test_stdio_prompt_then_immediate_load_session() {
    with_local_set(|| async {
        let server = MockInferenceServer::start().await.expect("start mock server");
        let workdir = git_workdir();
        let mut writer = EchoAgentStdioClient::spawn(&server, workdir.workspace()).await;

        let init_resp = writer.initialize_with_timeout().await;
        assert!(
            !init_resp.auth_methods.is_empty(),
            "agent should return at least one auth method"
        );

        let session_id = writer.create_session_with_timeout(workdir.workspace()).await;
        let result = writer.prompt_with_timeout(&session_id, "say hello").await;
        assert!(
            result.is_ok(),
            "prompt failed before load_session: {:?}\nrequest log:\n{}\nstderr:\n{}",
            result.err(),
            server.request_log_summary(),
            stderr_tail(&writer.stderr(), 1200)
        );

        let shared_sandbox = writer.take_sandbox();
        drop(writer);

        let reader = EchoAgentStdioClient::spawn_with_sandbox(
            &server,
            workdir.workspace(),
            shared_sandbox,
        )
        .await;
        reader.initialize_with_timeout().await;
        let _ = reader
            .load_session_with_timeout(&session_id, workdir.workspace())
            .await;
        assert!(
            reader.notification_count() > 0,
            "reloaded session should emit replay notifications\nstderr:\n{}",
            stderr_tail(&reader.stderr(), 1200)
        );
        assert!(
            reader.captured_text().contains("Echo:") && reader.captured_text().contains("say hello"),
            "reloaded session should replay the expected assistant text\ncaptured:\n{}\nstderr:\n{}",
            reader.captured_text(),
            stderr_tail(&reader.stderr(), 1200)
        );
    })
    .await;
}

// ── Raw-wire stdio driving (Xcode / Foundation shape) ───────────────────────

/// Serialize `req` compactly, then rewrite its method to the Foundation-escaped
/// form (`"session/new"` → `"session\/new"`) by string surgery, asserting the
/// escape really is in the wire bytes — so a serde_json formatting change can
/// never silently downgrade this test to the unescaped path.
fn line_with_escaped_method(req: &serde_json::Value, method: &str) -> String {
    let plain = format!(r#""method":"{method}""#);
    let escaped = format!(r#""method":"{}""#, method.replace('/', r"\/"));
    let line = req.to_string().replacen(&plain, &escaped, 1);
    assert!(
        line.contains(&escaped),
        "escaped method must be on the wire: {line}"
    );
    // One replacement only: a params value carrying the same substring must
    // fail here rather than get silently double-mangled.
    assert!(
        !line.contains(&plain),
        "plain method form must be gone from the wire: {line}"
    );
    line
}

/// Xcode 27 beta's ACP client (Swift/Foundation `JSONEncoder`) escapes forward
/// slashes in the JSON-RPC `method` field (`"session\/new"` — spec-legal JSON)
/// and uses uppercase string UUID request ids. acp 0.6 parses `method` as a
/// borrowed str, so an escaped method used to fail the envelope parse and the
/// line was silently dropped: `initialize` (no slash) worked, every `session/*`
/// request hung forever. Drives the built binary with the raw wire bytes and
/// asserts every escaped-method request gets a response.
#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn test_stdio_xcode_escaped_slash_methods_get_responses() {
    let server = MockInferenceServer::start()
        .await
        .expect("start mock server");
    let workdir = git_workdir();
    let mut agent = RawStdioClient::spawn(&server, workdir.workspace()).await;

    // initialize/authenticate carry no slash (they work from Xcode too), but
    // ride string UUID ids and minimal capabilities like Xcode's client.
    let init_id = "9B25E574-2F0C-4C8A-8C7E-2E9B3A4A0F01";
    agent
        .send_line(&format!(
            r#"{{"jsonrpc":"2.0","id":"{init_id}","method":"initialize","params":{{"protocolVersion":1,"clientCapabilities":{{"fs":{{"readTextFile":false,"writeTextFile":false}},"terminal":false}},"_meta":{{"startupHints":{{"nonInteractive":true,"skipGitStatus":true,"skipProjectLayout":true}},"clientType":"xcode-test","clientVersion":"27.0"}}}}}}"#
        ))
        .await;
    let init_resp = agent
        .response_for_id(init_id, "initialize", Duration::from_secs(20))
        .await;
    assert!(
        init_resp.get("result").is_some(),
        "initialize must respond with a result, got: {init_resp}"
    );

    let auth_id = "3C41A7D9-6B58-4E2F-A0D3-5F8C1B7E0A02";
    agent
        .send_line(&format!(
            r#"{{"jsonrpc":"2.0","id":"{auth_id}","method":"authenticate","params":{{"methodId":"echoagent.api_key","_meta":{{"headless":true}}}}}}"#
        ))
        .await;
    let auth_resp = agent
        .response_for_id(auth_id, "authenticate", Duration::from_secs(20))
        .await;
    assert!(
        auth_resp.get("error").is_none(),
        "authenticate failed: {auth_resp}\nstderr:\n{}",
        stderr_tail(&agent.stderr(), 1200)
    );

    // session/new with the escaped method literally on the wire.
    let new_id = "5DE7EA60-0B0C-4A43-9650-2B72CDF6A44B";
    let line = line_with_escaped_method(
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": new_id,
            "method": "session/new",
            "params": { "cwd": workdir.workspace(), "mcpServers": [] },
        }),
        "session/new",
    );
    agent.send_line(&line).await;
    // Returning at all asserts the exact-string-UUID id echo: response_for_id
    // only matches on it.
    let new_resp = agent
        .response_for_id(new_id, "escaped session/new", Duration::from_secs(20))
        .await;
    let session_id = new_resp["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "escaped session/new must return a sessionId, got: {new_resp}\nstderr:\n{}",
                stderr_tail(&agent.stderr(), 1200)
            )
        })
        .to_string();

    // session/prompt with the escaped method: must produce a response (result
    // or error) rather than silence; against the echo mock it completes.
    let prompt_id = "A1F3C9B2-7D64-4E85-B9A0-8C2D5E6F1A03";
    let line = line_with_escaped_method(
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": prompt_id,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{ "type": "text", "text": "say hello" }],
            },
        }),
        "session/prompt",
    );
    agent.send_line(&line).await;
    let prompt_resp = agent
        .response_for_id(prompt_id, "escaped session/prompt", Duration::from_secs(30))
        .await;
    assert!(
        prompt_resp.get("error").is_none(),
        "escaped session/prompt must complete: {prompt_resp}\nrequest log:\n{}\nstderr:\n{}",
        server.request_log_summary(),
        stderr_tail(&agent.stderr(), 1200)
    );
    assert!(
        prompt_resp["result"]["stopReason"].is_string(),
        "prompt response should carry a stopReason, got: {prompt_resp}"
    );
    assert!(
        server.request_count() > 0,
        "mock server received no inference requests\nrequest log:\n{}\nstderr:\n{}",
        server.request_log_summary(),
        stderr_tail(&agent.stderr(), 1200)
    );
}

/// `echoagent agent stdio` must initiate shutdown and exit when its client closes
/// stdin (EOF) — a dead parent means closed pipes, so this is the primary
/// orphan guard on every platform (the Linux `PR_SET_PDEATHSIG` binding in
/// `run_stdio_agent` additionally covers an agent wedged mid-turn that never
/// reads stdin again). Guards the `spawn_stdin_line_reader` → stdin_closed →
/// simplex-shutdown → `handle_io` completion chain end to end.
#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn test_stdio_agent_exits_on_stdin_eof() {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

    let server = MockInferenceServer::start()
        .await
        .expect("start mock server");
    let mut sandbox = TestSandbox::builder().git().build();
    sandbox.set_mock_url(server.url());

    let mut cmd = tokio::process::Command::new(echo_agent_binary());
    cmd.args(["agent", "stdio"])
        .current_dir(sandbox.workspace());
    let mut process = TestProcess::spawn(
        cmd,
        &sandbox,
        TestProcessConfig::new()
            .label("echoagent agent stdio (eof)")
            .stdin(TestStdin::Piped)
            .stdout(TestOutput::Piped),
    )
    .expect("spawn echoagent agent stdio");

    // Prove the agent is up and serving before the EOF (an exit during
    // startup would trivially pass the wait below).
    let mut stdin = process.take_stdin().expect("child stdin missing");
    let stdout = process.take_stdout().expect("child stdout missing");
    let mut reader = tokio::io::BufReader::new(stdout);
    stdin
        .write_all(
            concat!(
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"#,
                r#""clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false},"#,
                r#""_meta":{"startupHints":{"nonInteractive":true,"skipGitStatus":true,"skipProjectLayout":true},"#,
                r#""clientType":"eof-test","clientVersion":"0.0.0"}}}"#,
                "\n"
            )
            .as_bytes(),
        )
        .await
        .expect("write initialize");
    stdin.flush().await.expect("flush initialize");
    let mut line = String::new();
    tokio::time::timeout(scaled(Duration::from_secs(20)), reader.read_line(&mut line))
        .await
        .unwrap_or_else(|_| {
            panic!(
                "no initialize response before EOF\nstderr:\n{}",
                stderr_tail(&process.stderr_tail().text, 1200)
            )
        })
        .expect("read initialize response");
    assert!(
        line.contains("\"result\""),
        "initialize must respond with a result, got: {line}"
    );

    // Close the write end: the agent sees stdin EOF, exactly as when its
    // parent dies and the inherited pipe closes.
    drop(stdin);

    // Exit path includes a bounded teardown (100ms simplex flush + 2s upload
    // queue grace), so allow comfortably more than that.
    let status = process
        .wait_with_deadline(scaled(Duration::from_secs(30)))
        .await
        .expect("wait for agent exit")
        .unwrap_or_else(|| {
            panic!(
                "echoagent agent stdio did not exit after stdin EOF\n{}",
                process.diagnostic_summary()
            )
        });
    assert!(
        status.success(),
        "agent should exit cleanly on stdin EOF, got {status:?}\nstderr:\n{}",
        stderr_tail(&process.stderr_tail().text, 1200)
    );
}

// ── Config test harness ─────────────────────────────────────────────────────

/// Isolated headless run with a custom `~/.echo-agent/`. Clean env (no leaked
/// host credentials). Write config files into `echo_agent_dir()` before `run()`.
struct ConfigTestHarness {
    sandbox: TestSandbox,
}

impl ConfigTestHarness {
    fn new(server: &MockInferenceServer) -> Self {
        Self {
            sandbox: TestSandbox::builder().mock_url(server.url()).git().build(),
        }
    }

    fn echo_agent_dir(&self) -> std::path::PathBuf {
        self.sandbox.echo_agent_home().to_path_buf()
    }

    fn env(&mut self, key: &str, value: &str) -> &mut Self {
        self.sandbox.set_env(key, value);
        self
    }

    async fn run(self) -> HeadlessResult {
        let mut cmd = tokio::process::Command::new(echo_agent_binary());
        cmd.args(["-p", "say hello", "--yolo"])
            .current_dir(self.sandbox.workspace())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        run_headless_in_sandbox(cmd, self.sandbox).await
    }
}

// ── Enterprise managed config tests ────────────────────────────────────────

/// Enterprise BYOK: managed_config.toml overrides the default model with a custom
/// endpoint + env_key. Mock rejects unauthenticated requests with 401.
/// Regression guard for the 0.1.220 authentication regression.
#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn test_headless_managed_config_byok_sends_authorized_requests() {
    let server = MockInferenceServer::start_with_required_auth(
        vec![MockModelEntry::new("echo-agent-4.5")],
        "test-byok-secret-token",
    )
    .await
    .expect("start mock server");

    let mut h = ConfigTestHarness::new(&server);
    std::fs::write(
        h.echo_agent_dir().join("managed_config.toml"),
        format!(
            r#"
[endpoints]
deployment_key = "test-deployment-key"
echo_agent_api_base_url = "{url}"

[model."echo-agent-4.5"]
api_backend = "responses"
base_url = "{url}"
context_window = 500000
env_key = "ECHO_AGENT_TEST_BYOK_TOKEN"
model = "echo-agent-4.5"

[models]
default = "echo-agent-4.5"
"#,
            url = server.url()
        ),
    )
    .unwrap();
    h.env("ECHO_AGENT_TEST_BYOK_TOKEN", "test-byok-secret-token");

    let result = h.run().await;
    assert_headless_success(&result, "managed config BYOK", Some(&server));
    assert_no_crashes(&result.stderr);
    assert!(
        server.has_responses_request(),
        "mock server received no /v1/responses request\n{}",
        server.request_log_summary()
    );
}

/// New-server payload — a `reasoning_efforts` menu plus the legacy
/// `supportsReasoningEffort`/`reasoningEffort` (exactly what CCP emits) — parses
/// without error and the legacy effort scalar still rides the wire. Proves the
/// backwards-compat contract end-to-end through the built binary: the unknown
/// `reasoningEfforts` field never breaks the `/v1/models` parse. On the headless
/// path the wire effort comes from the legacy scalar, not from the list; the
/// list→default derivation is unit-tested in `acp_model_meta_*`.
#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn headless_reasoning_efforts_payload_parses_and_legacy_effort_rides_wire() {
    let server = MockInferenceServer::start_with_models(vec![
        MockModelEntry::new(CHAT_COMPLETIONS_MODEL)
            .with_api_backend("chat_completions")
            .with_supports_reasoning_effort(true)
            .with_reasoning_effort("xhigh")
            .with_reasoning_efforts(vec![
                serde_json::json!({ "id": "deep", "value": "xhigh", "label": "Deep", "default": true }),
                serde_json::json!({ "id": "balanced", "value": "medium", "label": "Balanced" }),
            ]),
    ])
    .await
    .expect("start mock server");
    server.set_response("done");

    let workdir = git_workdir();
    let result = run_headless(
        &server,
        &[
            "-p",
            "hi",
            "--yolo",
            "--model",
            CHAT_COMPLETIONS_MODEL,
            "--max-turns",
            "1",
        ],
        workdir.workspace(),
    )
    .await;

    assert_headless_success(
        &result,
        "echoagent -p reasoning_efforts list",
        Some(&server),
    );
    assert_no_crashes(&result.stderr);

    // The legacy effort scalar rides the chat-completions request unchanged.
    let effort_on_wire = server.requests().iter().any(|r| {
        r.body.as_ref().is_some_and(|body| {
            body.pointer("/reasoning_effort").and_then(|v| v.as_str()) == Some("xhigh")
        })
    });
    assert!(
        effort_on_wire,
        "legacy reasoning_effort=xhigh must reach the wire\n{}",
        server.request_log_summary()
    );
}

// ============================================================================
// Background-task reaping at headless exit
// ============================================================================

#[cfg(unix)]
use echo_agent_test_support::sse::{
    chat_completions_reasoning_then_tool_call_events, responses_api_reasoning_then_tool_call_events,
};

/// Poll `kill -0 <pid>` until the process is gone or the deadline passes.
#[cfg(unix)]
fn process_dead_within(pid: u32, deadline: Duration) -> bool {
    let start = std::time::Instant::now();
    loop {
        let alive = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !alive {
            return true;
        }
        if start.elapsed() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Read the PID a scripted background task wrote to `pid_file`, waiting for
/// the file to exist (the task writes it as its first action).
#[cfg(unix)]
fn read_task_pid(pid_file: &std::path::Path) -> u32 {
    let start = std::time::Instant::now();
    while !pid_file.exists() && start.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(100));
    }
    let contents = std::fs::read_to_string(pid_file).unwrap_or_else(|e| {
        panic!(
            "background task never ran: pid file {} unreadable: {e}",
            pid_file.display()
        )
    });
    contents
        .trim()
        .parse()
        .unwrap_or_else(|e| panic!("pid file {} held {contents:?}: {e}", pid_file.display()))
}

/// Script one turn that starts an `is_background: true` shell task recording
/// its PID and sleeping far longer than any timeout, followed by a plain-text
/// answer for the post-tool turn.
#[cfg(unix)]
fn enqueue_background_task_turn(server: &MockInferenceServer, pid_file: &std::path::Path) {
    let command = format!(
        "echo $$ > {0} && /bin/sync {0} 2>/dev/null; exec /bin/sleep 300",
        pid_file.display()
    );
    let args = serde_json::json!({
        "command": command,
        "description": "start long-lived background process",
        "is_background": true,
    })
    .to_string();
    server.enqueue_response(
        "/v1/responses",
        ScriptedResponse::sse(responses_api_reasoning_then_tool_call_events(
            "starting a background task",
            "call_bg",
            "run_terminal_command",
            &args,
            "test-model",
        )),
    );
    server.enqueue_response(
        "/v1/chat/completions",
        ScriptedResponse::sse(chat_completions_reasoning_then_tool_call_events(
            "starting a background task",
            "call_bg",
            "run_terminal_command",
            &args,
            "test-model",
        )),
    );
    server.set_response("done");
}

/// Timeout path: a background task outlives `--background-wait-timeout`, so
/// headless exits via the timeout valve — and must kill the task instead of
/// orphaning it.
#[cfg(unix)]
#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn test_headless_timeout_exit_kills_pending_background_task() {
    let server = MockInferenceServer::start()
        .await
        .expect("start mock server");
    let workdir = git_workdir();
    let pid_file = workdir.workspace().join("task_pid.txt");
    enqueue_background_task_turn(&server, &pid_file);

    let result = run_headless(
        &server,
        &[
            "-p",
            "start the server",
            "--yolo",
            "--background-wait-timeout",
            "1",
        ],
        workdir.workspace(),
    )
    .await;

    assert_headless_success(
        &result,
        "echoagent -p with pending background task",
        Some(&server),
    );
    assert_no_crashes(&result.stderr);

    let pid = read_task_pid(&pid_file);
    assert!(
        process_dead_within(pid, Duration::from_secs(5)),
        "background task (pid {pid}) survived headless exit on the timeout path\nstderr:\n{}",
        stderr_tail(&result.stderr, 2000)
    );
}

/// `--no-wait-for-background` path: exit is immediate after the turn, and the
/// task — tracked despite the flag — must still be killed, not leaked.
#[cfg(unix)]
#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn test_headless_no_wait_exit_kills_background_task() {
    let server = MockInferenceServer::start()
        .await
        .expect("start mock server");
    let workdir = git_workdir();
    let pid_file = workdir.workspace().join("task_pid.txt");
    enqueue_background_task_turn(&server, &pid_file);

    let result = run_headless(
        &server,
        &[
            "-p",
            "start the server",
            "--yolo",
            "--no-wait-for-background",
        ],
        workdir.workspace(),
    )
    .await;

    assert_headless_success(
        &result,
        "echoagent -p --no-wait-for-background",
        Some(&server),
    );
    assert_no_crashes(&result.stderr);

    let pid = read_task_pid(&pid_file);
    assert!(
        process_dead_within(pid, Duration::from_secs(5)),
        "background task (pid {pid}) survived --no-wait-for-background exit\nstderr:\n{}",
        stderr_tail(&result.stderr, 2000)
    );
}

/// Quiescent path regression guard: a background task that completes on its
/// own is waited for (intended behavior) and the run exits cleanly with
/// nothing reaped.
#[cfg(unix)]
#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn test_headless_waits_for_short_background_task_and_exits_clean() {
    let server = MockInferenceServer::start()
        .await
        .expect("start mock server");
    let workdir = git_workdir();
    let marker = workdir.workspace().join("finished.txt");
    let command = format!("/bin/sleep 1 && echo ok > {}", marker.display());
    let args = serde_json::json!({
        "command": command,
        "description": "short background task",
        "is_background": true,
    })
    .to_string();
    server.enqueue_response(
        "/v1/responses",
        ScriptedResponse::sse(responses_api_reasoning_then_tool_call_events(
            "starting a short background task",
            "call_bg_short",
            "run_terminal_command",
            &args,
            "test-model",
        )),
    );
    server.enqueue_response(
        "/v1/chat/completions",
        ScriptedResponse::sse(chat_completions_reasoning_then_tool_call_events(
            "starting a short background task",
            "call_bg_short",
            "run_terminal_command",
            &args,
            "test-model",
        )),
    );
    server.set_response("done");

    let result = run_headless(
        &server,
        &[
            "-p",
            "start it",
            "--yolo",
            "--background-wait-timeout",
            "30",
        ],
        workdir.workspace(),
    )
    .await;

    assert_headless_success(
        &result,
        "echoagent -p with short background task",
        Some(&server),
    );
    assert_no_crashes(&result.stderr);
    assert!(
        marker.exists(),
        "short background task did not finish before exit — the intended wait \
         was skipped\nstderr:\n{}",
        stderr_tail(&result.stderr, 2000)
    );
}
