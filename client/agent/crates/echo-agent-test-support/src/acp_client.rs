//! ACP stdio clients for testing echoagent sessions end-to-end: the typed
//! [`EchoAgentStdioClient`] (`agent-client-protocol::ClientSideConnection` —
//! authentication, session lifecycle, permissions, notification streaming) and
//! the raw-wire [`RawStdioClient`] (verbatim JSON-RPC lines for shapes the
//! typed client can't produce), all backed by the shared [`TestProcess`]
//! lifecycle owner.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use crate::scaled;

use agent_client_protocol::{self as acp, Agent as _};
use echo_agent_acp::LineBufferedRead;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::env::echo_agent_binary;
use crate::headless::stderr_tail;
use crate::mock_server::MockInferenceServer;
use crate::process::{TestOutput, TestProcess, TestProcessConfig, TestStdin};
use crate::sandbox::TestSandbox;

/// Spawn `echoagent agent stdio` with the sandbox's canonical hermetic environment.
/// `leading_args` go before the `agent stdio` subcommand (global flags).
fn spawn_agent_process(
    sandbox: &mut TestSandbox,
    server: &MockInferenceServer,
    cwd: &Path,
    extra_env: &[(&str, &str)],
    leading_args: &[&str],
) -> TestProcess {
    sandbox.set_mock_url(server.url());
    for (key, value) in extra_env {
        sandbox.set_env(*key, *value);
    }

    let binary = echo_agent_binary();
    let mut cmd = tokio::process::Command::new(&binary);
    cmd.args(leading_args)
        .args(["agent", "stdio"])
        .current_dir(cwd);

    TestProcess::spawn(
        cmd,
        sandbox,
        TestProcessConfig::new()
            .label("echoagent agent stdio")
            .stdin(TestStdin::Piped)
            .stdout(TestOutput::Piped),
    )
    .unwrap_or_else(|error| {
        panic!(
            "failed to spawn ACP test client at {}: {error}\n{}",
            binary.display(),
            sandbox.diagnostic_summary(),
        )
    })
}

#[derive(Default)]
struct TextCapture {
    chunks: std::sync::Mutex<Vec<String>>,
    notification_count: AtomicU32,
}

/// ACP client impl: auto-approves permissions, captures text chunks.
struct TestAcpClient {
    capture: Arc<TextCapture>,
}

#[async_trait::async_trait(?Send)]
impl acp::Client for TestAcpClient {
    async fn request_permission(
        &self,
        args: acp::RequestPermissionRequest,
    ) -> acp::Result<acp::RequestPermissionResponse> {
        // Auto-approve: pick AllowOnce if available, otherwise first option.
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

    async fn session_notification(&self, args: acp::SessionNotification) -> acp::Result<()> {
        self.capture
            .notification_count
            .fetch_add(1, Ordering::SeqCst);

        if let acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk { content, .. }) =
            args.update
            && let acp::ContentBlock::Text(text_content) = content
            && !text_content.text.is_empty()
        {
            self.capture.chunks.lock().unwrap().push(text_content.text);
        }
        Ok(())
    }
}

/// Drives `echoagent agent stdio` via the ACP protocol over pipes.
///
/// Handles the full lifecycle: spawn → initialize → authenticate → session → prompt.
/// Child process is killed on drop.
pub struct EchoAgentStdioClient {
    conn: acp::ClientSideConnection,
    process: TestProcess,
    sandbox: Option<TestSandbox>,
    capture: Arc<TextCapture>,
}

impl EchoAgentStdioClient {
    pub async fn spawn(server: &MockInferenceServer, cwd: &Path) -> Self {
        Self::spawn_with_sandbox(server, cwd, TestSandbox::new()).await
    }

    pub async fn spawn_with_sandbox(
        server: &MockInferenceServer,
        cwd: &Path,
        sandbox: TestSandbox,
    ) -> Self {
        Self::spawn_with_sandbox_env_and_args(server, cwd, sandbox, &[], &[]).await
    }

    pub async fn spawn_with_sandbox_env_and_args(
        server: &MockInferenceServer,
        cwd: &Path,
        mut sandbox: TestSandbox,
        extra_env: &[(&str, &str)],
        leading_args: &[&str],
    ) -> Self {
        let mut process = spawn_agent_process(&mut sandbox, server, cwd, extra_env, leading_args);

        let outgoing = process
            .take_stdin()
            .expect("child stdin missing")
            .compat_write();
        let incoming = process
            .take_stdout()
            .expect("child stdout missing")
            .compat();

        let capture = Arc::new(TextCapture::default());
        let client = TestAcpClient {
            capture: capture.clone(),
        };

        let incoming = LineBufferedRead::spawn_local(incoming);
        let (conn, handle_io) = acp::ClientSideConnection::new(client, outgoing, incoming, |fut| {
            tokio::task::spawn_local(fut);
        });
        tokio::task::spawn_local(handle_io);

        Self {
            conn,
            process,
            sandbox: Some(sandbox),
            capture,
        }
    }

    /// Initialize and authenticate (picks `api_key` auth method).
    pub async fn initialize(&self) -> acp::InitializeResponse {
        let init_resp = self
            .conn
            .initialize(
                acp::InitializeRequest::new(acp::ProtocolVersion::V1)
                    .client_capabilities(
                        acp::ClientCapabilities::new()
                            .fs(acp::FileSystemCapabilities::new())
                            .terminal(false),
                    )
                    .meta(
                        serde_json::json!({
                            "startupHints": {
                                "nonInteractive": true,
                                "skipGitStatus": true,
                                "skipProjectLayout": true
                            },
                            "clientType": "test-client",
                            "clientVersion": "0.0.0-test"
                        })
                        .as_object()
                        .cloned(),
                    ),
            )
            .await
            .expect("initialize failed");

        let api_key_method = init_resp
            .auth_methods
            .iter()
            .find(|m| &*m.id().0 == "echoagent.api_key")
            .unwrap_or_else(|| {
                let ids: Vec<_> = init_resp.auth_methods.iter().map(|m| &m.id().0).collect();
                panic!(
                    "expected auth method 'echoagent.api_key' but got: {ids:?}\n\
                     If the method ID changed, update this test."
                )
            });

        self.conn
            .authenticate(
                acp::AuthenticateRequest::new(api_key_method.id().clone())
                    .meta(serde_json::json!({"headless": true}).as_object().cloned()),
            )
            .await
            .expect("authenticate failed");

        init_resp
    }

    pub async fn create_session(&self, cwd: &Path) -> acp::SessionId {
        let resp = self
            .conn
            .new_session(acp::NewSessionRequest::new(cwd.to_path_buf()).mcp_servers(vec![]))
            .await
            .expect("session/new failed");
        resp.session_id
    }

    /// Create a session with a specific model pre-selected.
    pub async fn create_session_with_model(&self, cwd: &Path, model_id: &str) -> acp::SessionId {
        let resp = self
            .conn
            .new_session(
                acp::NewSessionRequest::new(cwd.to_path_buf())
                    .mcp_servers(vec![])
                    .meta(
                        serde_json::json!({ "modelId": model_id })
                            .as_object()
                            .cloned(),
                    ),
            )
            .await
            .expect("session/new with modelId failed");
        resp.session_id
    }

    /// Switch model on an existing session via the typed ACP `session/set_model`.
    pub async fn set_model(
        &self,
        session_id: &acp::SessionId,
        model_id: &str,
    ) -> acp::Result<acp::SetSessionModelResponse> {
        use acp::Agent as _;
        self.conn
            .set_session_model(acp::SetSessionModelRequest::new(
                session_id.clone(),
                acp::ModelId::new(model_id),
            ))
            .await
    }

    pub async fn prompt(
        &self,
        session_id: &acp::SessionId,
        text: &str,
    ) -> acp::Result<acp::PromptResponse> {
        self.conn
            .prompt(acp::PromptRequest::new(
                session_id.clone(),
                vec![acp::ContentBlock::Text(acp::TextContent::new(
                    text.to_string(),
                ))],
            ))
            .await
    }

    pub fn captured_text(&self) -> String {
        self.capture.chunks.lock().unwrap().join("")
    }

    pub fn notification_count(&self) -> u32 {
        self.capture.notification_count.load(Ordering::SeqCst)
    }

    pub fn stderr(&self) -> String {
        self.process.stderr_tail().text
    }

    pub fn child_pid(&self) -> Option<u32> {
        self.process.pid()
    }

    pub fn process_diagnostics(&self) -> String {
        self.process.diagnostic_summary()
    }

    pub fn start_terminate(&mut self) -> std::io::Result<()> {
        self.process.start_terminate()
    }

    pub fn start_kill(&mut self) {
        self.process.start_kill();
    }

    pub async fn close(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.process.close().await
    }

    pub fn take_sandbox(&mut self) -> TestSandbox {
        self.sandbox.take().expect("test sandbox already taken")
    }

    pub fn sandbox(&self) -> &TestSandbox {
        self.sandbox.as_ref().expect("test sandbox already taken")
    }

    /// Timing breadcrumb for tuning CI timeout budgets (visible with --nocapture).
    fn log_timing(what: &str, started: std::time::Instant) {
        eprintln!("[harness-timing] {what}: {:?}", started.elapsed());
    }

    pub async fn initialize_with_timeout(&self) -> acp::InitializeResponse {
        let started = std::time::Instant::now();
        let r = tokio::time::timeout(scaled(Duration::from_secs(20)), self.initialize())
            .await
            .unwrap_or_else(|_| panic!("initialize timed out\nstderr:\n{}", self.stderr()));
        Self::log_timing("initialize", started);
        r
    }

    pub async fn create_session_with_timeout(&self, cwd: &Path) -> acp::SessionId {
        let started = std::time::Instant::now();
        let r = tokio::time::timeout(scaled(Duration::from_secs(20)), self.create_session(cwd))
            .await
            .unwrap_or_else(|_| panic!("session/new timed out\nstderr:\n{}", self.stderr()));
        Self::log_timing("session/new", started);
        r
    }

    pub async fn create_session_with_model_timeout(
        &self,
        cwd: &Path,
        model_id: &str,
    ) -> acp::SessionId {
        tokio::time::timeout(
            scaled(Duration::from_secs(20)),
            self.create_session_with_model(cwd, model_id),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "session/new with modelId={model_id} timed out\nstderr:\n{}",
                self.stderr()
            )
        })
    }

    pub async fn set_model_with_timeout(
        &self,
        session_id: &acp::SessionId,
        model_id: &str,
    ) -> acp::Result<acp::SetSessionModelResponse> {
        tokio::time::timeout(
            scaled(Duration::from_secs(20)),
            self.set_model(session_id, model_id),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "session/set_model({model_id}) timed out\nstderr:\n{}",
                self.stderr()
            )
        })
    }

    pub async fn prompt_with_timeout(
        &self,
        session_id: &acp::SessionId,
        text: &str,
    ) -> acp::Result<acp::PromptResponse> {
        let started = std::time::Instant::now();
        let r = tokio::time::timeout(
            scaled(Duration::from_secs(30)),
            self.prompt(session_id, text),
        )
        .await
        .unwrap_or_else(|_| panic!("prompt timed out\nstderr:\n{}", self.stderr()));
        Self::log_timing("prompt", started);
        r
    }

    pub async fn load_session_with_timeout(
        &self,
        session_id: &acp::SessionId,
        cwd: &Path,
    ) -> acp::LoadSessionResponse {
        // 60s: session/load replays history and is slower under Rosetta
        // (macos-x86_64 lifecycle CI). 20s flaked repeatedly there.
        tokio::time::timeout(
            scaled(Duration::from_secs(60)),
            self.conn.load_session(
                acp::LoadSessionRequest::new(session_id.clone(), cwd.to_path_buf())
                    .mcp_servers(vec![]),
            ),
        )
        .await
        .unwrap_or_else(|_| panic!("session/load timed out\nstderr:\n{}", self.stderr()))
        .expect("session/load failed")
    }

    pub async fn ext_method(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> acp::Result<acp::ExtResponse> {
        let raw = serde_json::value::RawValue::from_string(params.to_string())
            .expect("serialize ext params");
        self.conn
            .ext_method(acp::ExtRequest::new(method, std::sync::Arc::from(raw)))
            .await
    }
}

/// Drives `echoagent agent stdio` with verbatim newline-delimited JSON-RPC lines.
///
/// Exists for wire shapes the typed [`EchoAgentStdioClient`] (`ClientSideConnection`,
/// integer ids) can never produce — e.g. Xcode's Swift/Foundation `JSONEncoder`
/// output: escaped-slash methods (`"session\/prompt"`) and string UUID request
/// ids. Child process is killed on drop.
pub struct RawStdioClient {
    stdin: tokio::process::ChildStdin,
    stdout: tokio::io::BufReader<crate::process::TestProcessStdout>,
    process: TestProcess,
    _sandbox: TestSandbox,
}

impl RawStdioClient {
    pub async fn spawn(server: &MockInferenceServer, cwd: &Path) -> Self {
        let mut sandbox = TestSandbox::new();
        let mut process = spawn_agent_process(&mut sandbox, server, cwd, &[], &[]);

        let stdin = process.take_stdin().expect("child stdin missing");
        let child_stdout = process.take_stdout().expect("child stdout missing");

        Self {
            stdin,
            stdout: tokio::io::BufReader::new(child_stdout),
            process,
            _sandbox: sandbox,
        }
    }

    pub fn stderr(&self) -> String {
        self.process.stderr_tail().text
    }

    pub fn child_pid(&self) -> Option<u32> {
        self.process.pid()
    }

    pub fn process_diagnostics(&self) -> String {
        self.process.diagnostic_summary()
    }

    pub fn start_terminate(&mut self) -> std::io::Result<()> {
        self.process.start_terminate()
    }

    pub fn start_kill(&mut self) {
        self.process.start_kill();
    }

    pub async fn close(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.process.close().await
    }

    /// Write `line` verbatim followed by `\n`, and flush.
    pub async fn send_line(&mut self, line: &str) {
        use tokio::io::AsyncWriteExt as _;

        self.stdin
            .write_all(line.as_bytes())
            .await
            .expect("write line to agent stdin");
        self.stdin.write_all(b"\n").await.expect("write newline");
        self.stdin.flush().await.expect("flush agent stdin");
    }

    /// Read stdout lines until the response to `id` arrives (no `method` key +
    /// exact string-id match) — returning IS the id-echo assertion: an id
    /// echoed with different bytes or as a different JSON type never matches
    /// and surfaces in the timeout diagnostics instead. Notifications are
    /// skipped; any agent→client request is refused with a JSON-RPC error so a
    /// turn can never hang on this capability-less client. On timeout the
    /// panic reports how much non-matching traffic was seen (0 = true
    /// silence, the acp-0.6 escaped-method symptom) plus the last few lines.
    pub async fn response_for_id(
        &mut self,
        id: &str,
        what: &str,
        timeout: Duration,
    ) -> serde_json::Value {
        use tokio::io::AsyncBufReadExt as _;

        let deadline = tokio::time::Instant::now() + scaled(timeout);
        let mut line = String::new();
        let mut skipped = 0_usize;
        let mut skipped_tail: Vec<String> = Vec::new();
        loop {
            line.clear();
            let next_line = self.stdout.read_line(&mut line);
            let Ok(io_result) = tokio::time::timeout_at(deadline, next_line).await else {
                panic!(
                    "{what}: no matching response within {timeout:?} ({skipped} other messages \
                     seen; last: {skipped_tail:?})\nstderr:\n{}",
                    stderr_tail(&self.stderr(), 1200)
                );
            };
            let read =
                io_result.unwrap_or_else(|e| panic!("{what}: agent stdout read failed: {e}"));
            if read == 0 {
                panic!(
                    "{what}: agent closed stdout before responding ({skipped} other messages \
                     seen)\nstderr:\n{}",
                    stderr_tail(&self.stderr(), 1200)
                );
            }
            let Ok(msg) = serde_json::from_str::<serde_json::Value>(line.trim_end()) else {
                push_skipped_tail(&mut skipped, &mut skipped_tail, &line);
                continue;
            };
            let is_response = msg.get("method").is_none();
            if is_response && msg.get("id").and_then(|v| v.as_str()) == Some(id) {
                return msg;
            }
            push_skipped_tail(&mut skipped, &mut skipped_tail, &line);
            if !is_response && let Some(req_id) = msg.get("id") {
                let refusal = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "error": { "code": -32601, "message": "unsupported by raw test client" },
                });
                self.send_line(&refusal.to_string()).await;
            }
        }
    }
}

/// Record a non-matching line for [`RawStdioClient::response_for_id`]'s timeout
/// diagnostics: bump the count, keep the last 3 lines (truncated).
fn push_skipped_tail(skipped: &mut usize, tail: &mut Vec<String>, line: &str) {
    *skipped += 1;
    if tail.len() == 3 {
        tail.remove(0);
    }
    tail.push(line.trim_end().chars().take(200).collect());
}
