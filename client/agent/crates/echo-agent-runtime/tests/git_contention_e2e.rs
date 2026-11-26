//! End-to-end repro of the git refresh storm during agent-run rebases.
//!
//! Stands up a real in-process `MvpAgent` over duplex ACP pipes with a
//! hand-rolled `initialize` (the stock test client advertises empty
//! capabilities, so the fs watcher would never spawn) and drives two scripted
//! turns through `MockInferenceServer`:
//!
//! 1. `search_replace` creates two files, so the AgentOnly hunk tracker has
//!    tracked paths (defeating its nothing-tracked early return).
//! 2. `run_terminal_command` runs a real multi-pick `git rebase -i` whose
//!    picks chain back to back, reproducing the continuous
//!    `.git/index.lock` / HEAD-move churn of an agent-run rebase.
//!
//! With the fs-watch machinery on (`echo.agent/hunkTracker` + `echo.agent/gitHeadChanged`
//! advertised), fsnotify merges rapid lock cycles into one operation and the
//! session defers debounce fires while an op is in flight, so one rebase
//! costs at most a couple of `refresh_all_baselines` scans (each scoped to
//! the tracked paths) instead of one full-worktree scan per inter-pick gap;
//! with capabilities absent the watcher never spawns and no scans run. The
//! test counts real scans via a global tracing layer (sessions run on their
//! own threads, so a thread-scoped subscriber would miss them), prints both
//! runs, and asserts only invariants: zero scans with the machinery off, a
//! small bounded number per rebase with it on.
//!
//! Knobs: ECHO_AGENT_PERF_GIT_FILES (default 300), ECHO_AGENT_PERF_GIT_PICKS (default 6).
//! Keep the rebase under ~15s or the terminal tool auto-backgrounds the
//! command and the turn wall time loses meaning.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use agent_client_protocol::{self as acp, Agent as _};
use echo_agent_acp::{
    AcpAgentGatewayReceiver as GatewayReceiver, AcpAgentGatewaySender as GatewaySender,
    LineBufferedRead,
};
use echo_agent_runtime::agent::config::Config as AgentConfig;
use echo_agent_runtime::agent::mvp_agent::MvpAgent;
use echo_agent_test_support::{MockInferenceServer, ScriptedResponse, SseEvent};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

const DUPLEX_BUFFER_BYTES: usize = 8 * 1024 * 1024;

use echo_agent_test_utils::env::env_usize;

/// Run a git command with deterministic author/committer; assert success.
fn git(dir: &Path, args: &[&str]) -> String {
    echo_agent_test_utils::git::run_git(dir, args)
}

// ── scan counting ─────────────────────────────────────────────────────────

use echo_agent_hunk_tracker::{REFRESH_SCAN_LOG_PREFIX, REFRESH_SKIP_LOG_PREFIX};
use echo_agent_test_utils::tracing_capture::MessagePrefixCounter;

/// Counts hunk-tracker scan completions/skips across all threads (the session
/// actor and its consumers run off the test thread). Only real scans log the
/// scan prefix; the unchanged-git-state skip logs the other.
#[derive(Clone)]
struct ScanCounter(MessagePrefixCounter);

impl ScanCounter {
    fn scans(&self) -> usize {
        self.0.count(REFRESH_SCAN_LOG_PREFIX)
    }

    fn skips(&self) -> usize {
        self.0.count(REFRESH_SKIP_LOG_PREFIX)
    }
}

fn install_global_scan_counter() -> ScanCounter {
    // ECHO_AGENT_E2E_LOG=<filter> tees shell logs to stderr for local debugging.
    let filter = std::env::var("ECHO_AGENT_E2E_LOG").ok();
    ScanCounter(
        echo_agent_test_utils::tracing_capture::install_prefix_counter_global(
            &[REFRESH_SCAN_LOG_PREFIX, REFRESH_SKIP_LOG_PREFIX],
            filter.as_deref(),
        ),
    )
}

// ── repo fixture ──────────────────────────────────────────────────────────

/// Committed tree of ~`files` files plus a `feature` branch with `picks`
/// one-file commits and an advanced base branch, `feature` checked out.
/// Returns the repo dir and the base branch name.
fn build_repo(files: usize, picks: usize) -> (TempDir, String) {
    let dir = TempDir::new().expect("repo tempdir");
    let wd = dir.path();

    git(wd, &["init"]);
    git(wd, &["config", "user.name", "Test User"]);
    git(wd, &["config", "user.email", "test@test.com"]);

    echo_agent_test_utils::git::write_fanout_tree(wd, files, 100);
    git(wd, &["add", "."]);
    git(wd, &["commit", "-m", "populate tree"]);

    let base = echo_agent_test_utils::git::make_feature_branch(wd, picks);
    (dir, base)
}

// ── scripted responses (chat-completions SSE) ────────────────────────────

fn chat_chunk(delta: Value, finish_reason: Value) -> SseEvent {
    SseEvent::data(
        json!({
            "id": "chatcmpl-test",
            "object": "chat.completion.chunk",
            "created": 1234567890,
            "model": "test-model",
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish_reason }]
        })
        .to_string(),
    )
}

fn tool_call_sse(name: &str, arguments: &Value) -> ScriptedResponse {
    static CALL_SEQ: AtomicUsize = AtomicUsize::new(0);
    let call_id = format!("call_{name}_{}", CALL_SEQ.fetch_add(1, Ordering::Relaxed));
    let events = vec![
        chat_chunk(
            json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": call_id,
                    "type": "function",
                    "function": { "name": name, "arguments": arguments.to_string() }
                }]
            }),
            Value::Null,
        ),
        chat_chunk(json!({}), json!("tool_calls")),
        SseEvent::data("[DONE]"),
    ];
    ScriptedResponse::sse(events)
}

fn text_sse(text: &str) -> ScriptedResponse {
    let events = vec![
        chat_chunk(json!({ "role": "assistant", "content": text }), Value::Null),
        chat_chunk(json!({}), json!("stop")),
        SseEvent::data("[DONE]"),
    ];
    ScriptedResponse::sse(events)
}

// ── client ────────────────────────────────────────────────────────────────

/// Auto-approves permissions (AllowOnce preferred) and drops notifications.
struct AutoApproveClient;

#[async_trait::async_trait(?Send)]
impl acp::Client for AutoApproveClient {
    async fn request_permission(
        &self,
        args: acp::RequestPermissionRequest,
    ) -> acp::Result<acp::RequestPermissionResponse> {
        let outcome = args
            .options
            .iter()
            .find(|o| o.kind == acp::PermissionOptionKind::AllowOnce)
            .or(args.options.first())
            .map(|o| {
                acp::RequestPermissionOutcome::Selected(acp::SelectedPermissionOutcome::new(
                    o.option_id.clone(),
                ))
            })
            .unwrap_or(acp::RequestPermissionOutcome::Cancelled);
        Ok(acp::RequestPermissionResponse::new(outcome))
    }

    async fn session_notification(&self, _args: acp::SessionNotification) -> acp::Result<()> {
        Ok(())
    }
}

// ── one full agent run ────────────────────────────────────────────────────

struct RunStats {
    scans: usize,
    skips: usize,
    edit_turn: Duration,
    rebase_turn: Duration,
}

async fn prompt_turn(
    client_conn: &acp::ClientSideConnection,
    session_id: &acp::SessionId,
    text: &str,
    label: &str,
) -> Duration {
    let started = Instant::now();
    let resp = tokio::time::timeout(
        Duration::from_secs(180),
        client_conn.prompt(acp::PromptRequest::new(
            session_id.clone(),
            vec![acp::ContentBlock::Text(acp::TextContent::new(
                text.to_owned(),
            ))],
        )),
    )
    .await
    .unwrap_or_else(|_| panic!("{label}: prompt timed out"))
    .unwrap_or_else(|e| panic!("{label}: prompt failed: {e}"));
    assert!(
        matches!(resp.stop_reason, acp::StopReason::EndTurn),
        "{label}: expected EndTurn, got {:?}",
        resp.stop_reason
    );
    started.elapsed()
}

/// Drive both scripted turns through a fresh in-process agent over a fresh
/// repo. `caps_meta` is the `client_capabilities._meta` advertised on
/// `initialize` (None = fs-watch machinery off).
async fn run_storm(
    server: &MockInferenceServer,
    caps_meta: Option<Value>,
    counter: &ScanCounter,
    label: &str,
) -> RunStats {
    let files = env_usize("ECHO_AGENT_PERF_GIT_FILES", 300);
    let picks = env_usize("ECHO_AGENT_PERF_GIT_PICKS", 6);
    let (repo, base) = build_repo(files, picks);
    eprintln!(
        "[perf] {label}: repo ~{files} files, {picks} picks at {:?}",
        repo.path()
    );

    // Turn 1: two file-creating edits, then a final text.
    server.enqueue_response(
        "/v1/chat/completions",
        tool_call_sse(
            "search_replace",
            &json!({
                "file_path": "agent_notes_a.md",
                "old_string": "",
                "new_string": "agent notes a\n"
            }),
        ),
    );
    server.enqueue_response(
        "/v1/chat/completions",
        tool_call_sse(
            "search_replace",
            &json!({
                "file_path": "agent_notes_b.md",
                "old_string": "",
                "new_string": "agent notes b\n"
            }),
        ),
    );
    server.enqueue_response("/v1/chat/completions", text_sse("created the notes files"));

    // Turn 2: the storm shape — a real multi-pick rebase whose picks chain
    // continuously. No --exec: each inserted exec spawns shells that add >1s
    // of lock-free idle per pick in this environment, and idle-gapped
    // rebases legitimately refresh per gap; continuous lock churn is what
    // must merge into a single operation (one refresh), and is what an
    // agent-run rebase looks like.
    let rebase_cmd = format!("GIT_SEQUENCE_EDITOR=: git rebase -i {base} 2>&1");
    server.enqueue_response(
        "/v1/chat/completions",
        tool_call_sse(
            "run_terminal_command",
            &json!({ "command": rebase_cmd, "description": "storm repro rebase" }),
        ),
    );
    server.enqueue_response("/v1/chat/completions", text_sse("rebase complete"));

    let counter = counter.clone();
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            // Blocks this thread on the startup models/settings prefetch —
            // served by the mock's dedicated runtime thread, so it completes
            // and the catalog contains the mock's chat-completions model.
            let agent_config = AgentConfig::default();
            let auth_manager = Arc::new(agent_config.create_auth_manager());
            let (gw_tx, gw_rx) = tokio::sync::mpsc::unbounded_channel();
            let gateway = GatewaySender::new(gw_tx);
            let agent =
                MvpAgent::new(gateway, &agent_config, auth_manager, None).expect("valid config");

            let (c2a_a, c2a_b) = tokio::io::duplex(DUPLEX_BUFFER_BYTES);
            let (a2c_a, a2c_b) = tokio::io::duplex(DUPLEX_BUFFER_BYTES);

            let agent_incoming = LineBufferedRead::spawn_local(c2a_b.compat());
            let (agent_conn, agent_io) =
                acp::AgentSideConnection::new(agent, a2c_a.compat_write(), agent_incoming, |fut| {
                    tokio::task::spawn_local(fut);
                });
            tokio::task::spawn_local(
                GatewayReceiver::new(gw_rx, agent_conn)
                    .with_on_meta(echo_agent_file_utils::trace_context::span_from_meta_traceparent)
                    .run(),
            );
            tokio::task::spawn_local(agent_io);

            let client_incoming = LineBufferedRead::spawn_local(a2c_b.compat());
            let (client_conn, client_io) = acp::ClientSideConnection::new(
                AutoApproveClient,
                c2a_a.compat_write(),
                client_incoming,
                |fut| {
                    tokio::task::spawn_local(fut);
                },
            );
            tokio::task::spawn_local(client_io);

            let init = tokio::time::timeout(
                Duration::from_secs(60),
                client_conn.initialize(
                    acp::InitializeRequest::new(acp::ProtocolVersion::V1)
                        .client_capabilities(
                            acp::ClientCapabilities::new()
                                .fs(acp::FileSystemCapabilities::new())
                                .terminal(false)
                                .meta(caps_meta.and_then(|v| v.as_object().cloned())),
                        )
                        .meta(
                            json!({
                                "startupHints": {
                                    "nonInteractive": true,
                                    "skipGitStatus": true,
                                    "skipProjectLayout": true,
                                },
                                "clientType": "git-contention-e2e",
                                "clientVersion": "0.0-test",
                            })
                            .as_object()
                            .cloned(),
                        ),
                ),
            )
            .await
            .expect("initialize timed out")
            .expect("initialize failed");

            // Strict: authenticating is what triggers the remote model fetch,
            // and the session must resolve the mock's chat-completions model.
            let method = init
                .auth_methods
                .iter()
                .find(|m| &*m.id().0 == "echoagent.api_key")
                .unwrap_or_else(|| panic!("{label}: echoagent.api_key auth method not advertised"));
            client_conn
                .authenticate(
                    acp::AuthenticateRequest::new(method.id().clone())
                        .meta(json!({ "headless": true }).as_object().cloned()),
                )
                .await
                .unwrap_or_else(|e| panic!("{label}: authenticate failed: {e}"));

            let session = tokio::time::timeout(
                Duration::from_secs(60),
                client_conn.new_session(
                    acp::NewSessionRequest::new(repo.path().to_path_buf())
                        .meta(json!({ "modelId": "test-model" }).as_object().cloned()),
                ),
            )
            .await
            .expect("session/new timed out")
            .expect("session/new failed");
            let session_id = session.session_id;

            let edit_turn =
                prompt_turn(&client_conn, &session_id, "create the notes files", label).await;
            // The scripted edits must actually have run: the storm depends on
            // the hunk tracker holding tracked paths during the rebase.
            assert!(
                repo.path().join("agent_notes_a.md").exists()
                    && repo.path().join("agent_notes_b.md").exists(),
                "{label}: scripted search_replace edits did not run\n{}",
                server.request_log_summary()
            );
            // Let the edit turn's fs events settle before windowing the storm.
            tokio::time::sleep(Duration::from_millis(500)).await;

            let scans_before = counter.scans();
            let skips_before = counter.skips();
            let rebase_turn = prompt_turn(&client_conn, &session_id, "run the rebase", label).await;
            assert!(
                repo.path().join("base_advance.txt").exists(),
                "{label}: the scripted rebase did not run (HEAD: {})\n{}",
                git(repo.path(), &["log", "--oneline", "-1"]),
                server.request_log_summary()
            );
            // Trailing drain: the last debounce window (quiet 500ms, cap 3s)
            // plus the spawned refresh itself.
            tokio::time::sleep(Duration::from_secs(4)).await;

            RunStats {
                scans: counter.scans() - scans_before,
                skips: counter.skips() - skips_before,
                edit_turn,
                rebase_turn,
            }
        })
        .await
}

/// Real FS watcher + real git + real timers: too timing-dependent for CI.
#[test]
#[ignore = "perf repro; real FS events; run locally with --ignored --nocapture"]
fn git_rebase_refresh_storm_e2e() {
    echo_agent_extra_ca::ensure_default_crypto_provider();
    let counter = install_global_scan_counter();

    // The mock gets its own runtime thread: agent startup blocks the test
    // thread on a models/settings prefetch (thread spawn + join), which would
    // starve a mock sharing the agent's runtime and time the fetch out.
    let mock_rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("mock runtime");
    let server = mock_rt
        .block_on(MockInferenceServer::start())
        .expect("mock server");
    let echo_agent_home = TempDir::new().expect("echoagent home");

    // SAFETY: the only live threads are the mock runtime's workers, which
    // serve HTTP and never read the process environment.
    unsafe {
        std::env::set_var("ECHO_AGENT_HOME", echo_agent_home.path());
        std::env::set_var("ECHO_AGENT_CLI_CHAT_PROXY_BASE_URL", server.url());
        std::env::set_var("ECHO_AGENT_API_BASE_URL", server.url());
        std::env::set_var("ECHO_AGENT_API_KEY", "test-key-for-ci");
        std::env::set_var("ECHO_AGENT_TELEMETRY_ENABLED", "false");
        std::env::set_var("ECHO_AGENT_FEEDBACK_ENABLED", "false");
        std::env::set_var("ECHO_AGENT_TRACE_UPLOAD", "false");
    }

    let agent_rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("agent runtime");

    // Machinery off first so the on-run's trailing refreshes cannot bleed
    // into the off-run's counting window.
    let off = agent_rt.block_on(run_storm(&server, None, &counter, "machinery-off"));
    let on = agent_rt.block_on(run_storm(
        &server,
        Some(json!({
            "echo.agent/hunkTracker": { "mode": "agent_only" },
            "echo.agent/gitHeadChanged": true,
        })),
        &counter,
        "machinery-on",
    ));

    eprintln!("\n[perf] ===== git refresh storm e2e (rebase turn) =====");
    eprintln!(
        "  machinery ON : scans={} skips={} edit_turn={:?} rebase_turn={:?}",
        on.scans, on.skips, on.edit_turn, on.rebase_turn
    );
    eprintln!(
        "  machinery OFF: scans={} skips={} edit_turn={:?} rebase_turn={:?}",
        off.scans, off.skips, off.edit_turn, off.rebase_turn
    );
    eprintln!("==================================================\n");

    assert_eq!(
        off.scans, 0,
        "without fs-watch capabilities no watcher spawns and no scans run"
    );
    // Merged lock cycles + in-op deferral: at least the post-op refresh runs,
    // and at most one more fire lands in a live window. A regression to
    // per-cycle completions or mid-op fires storms this back to one
    // full-worktree scan per pick.
    assert!(
        (1..=2).contains(&on.scans),
        "expected the merged operation to cost 1-2 scans, got {}",
        on.scans
    );
}
