//! In-process multi-client leader cluster: a REAL leader IPC server fronting a
//! REAL `MvpAgent`, with each test client being a full pager view-model
//! (`AppView`) wired through the production leader bridge. Deterministically
//! exercises the multi-client surface the PTY `LeaderCluster` covers, one
//! layer down — no subprocesses, no terminals, no screen scraping.
//!
//! Per client: `LeaderClient::connect(sock).into_channels()` →
//! [`bridge_channels`] → one `AppView`; inbound ACP is pumped through
//! `acp_handler::handle`, user intent is driven through `dispatch`, and
//! effects run through the real `effects::execute` (the same loop
//! `event_loop::run` performs, minus the terminal).
//!
//! Env sandboxing follows this crate's `serial(ECHO_AGENT_HOME)` idiom; note
//! `echo_agent_home()` is process-cached (OnceLock), so disk assertions always go
//! through [`effective_echo_agent_home`] rather than assuming the temp dir won.
//!
//! The scenarios are `#[ignore]`d in the shared lib test binary: the harness
//! mutates process-global env (proxy URLs, `ECHO_AGENT_API_KEY`,
//! `ECHO_AGENT_LEADER_SOCKET`, `ECHO_AGENT_HOME`) for a real agent's whole lifetime, and
//! in a several-thousand-test process that poisons concurrently-running tests
//! (and `echo_agent_home()`'s OnceLock is usually already pinned). Run on demand:
//!
//! ```bash
//! cargo test -p echo-agent-pager --lib -- app::leader_cluster --ignored --test-threads=1
//! ```
//!
//! Follow-up to un-ignore: move the scenarios to a dedicated test binary
//! (single-process isolation via a test-harness feature over the pub(crate)
//! seams), where env is set before any process-global's first touch.
//!
//! Unix-only: the leader transport here is a unix socket.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::time::Duration;

use agent_client_protocol as acp;
use echo_agent_acp::{AcpClientRx, acp_send};
use echo_agent_runtime::leader::{
    ClientCapabilities as LeaderClientCapabilities, ClientMode, ConnectionStatus,
    LEADER_SOCKET_ENV, LeaderClient, LeaderEnvUrls, LeaderLock, LeaderReconnector,
    LeaderServerControlState, LeaderServerMetadata, ReconnectPolicy, run_leader_server,
};
use echo_agent_test_support::MockInferenceServer;
use tempfile::TempDir;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::actions::{Action, TaskResult};
use super::agent::AgentState;
use super::agent_view::AgentView;
use super::app_view::{AppView, AuthState, TrustState};
use super::{acp_handler, dispatch, effects};
use crate::acp::leader_bridge::bridge_channels;
use crate::acp::model_state::ModelState;
use crate::scrollback::block::RenderBlock;

const PUMP_TICK: Duration = Duration::from_millis(10);
const TURN_BUDGET: Duration = Duration::from_secs(60);

/// Await a bring-up step with a hard budget so an on-demand run that hangs
/// names its phase instead of parking until the test-runner kill.
async fn bounded<T>(what: &str, fut: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(30), fut)
        .await
        .unwrap_or_else(|_| panic!("leader-cluster bring-up timed out: {what}"))
}

/// The echoagent home the agent actually persisted under: `echo_agent_home()` is
/// process-cached, so an earlier test in this binary may have pinned it.
fn effective_echo_agent_home() -> PathBuf {
    echo_agent_config::echo_agent_home()
}

/// Concatenated agent-message text across a view's scrollback (copy of the
/// acp_handler tests' helper; that one is test-mod private).
fn agent_message_text(view: &AgentView) -> String {
    let mut out = String::new();
    for i in 0..view.scrollback.len() {
        if let Some(entry) = view.scrollback.get(i)
            && let RenderBlock::AgentMessage(msg) = &entry.block
        {
            out.push_str(&msg.text());
        }
    }
    out
}

/// One pager client: a full `AppView` behind the production leader bridge.
struct ClusterClient {
    app: AppView,
    rx: AcpClientRx,
    tasks: JoinSet<TaskResult>,
    progress_tx: tokio::sync::mpsc::UnboundedSender<effects::RestoreProgressMsg>,
    _progress_rx: tokio::sync::mpsc::UnboundedReceiver<effects::RestoreProgressMsg>,
    bridge_cancel: CancellationToken,
    /// Present when the client was built with a reconnector: observes
    /// generation bumps after a leader kill/respawn.
    status_rx: Option<tokio::sync::watch::Receiver<ConnectionStatus>>,
}

impl ClusterClient {
    /// Drain everything currently ready (inbound ACP + finished tasks).
    /// Returns whether anything was processed.
    fn pump_once(&mut self) -> bool {
        let mut progressed = false;
        while let Ok(msg) = self.rx.try_recv() {
            acp_handler::handle(msg, &mut self.app);
            self.drain_pending_effects();
            progressed = true;
        }
        while let Some(joined) = self.tasks.try_join_next() {
            if let Ok(result) = joined {
                let effs = dispatch::dispatch(Action::TaskComplete(result), &mut self.app);
                self.process_effects(effs);
            }
            progressed = true;
        }
        progressed
    }

    fn drain_pending_effects(&mut self) {
        if !self.app.pending_effects.is_empty() {
            let effs = std::mem::take(&mut self.app.pending_effects);
            self.process_effects(effs);
        }
    }

    /// The event loop's `process_effects`, minus terminal/auth-handle wiring
    /// (that fn is event_loop-private; this mirrors its body).
    fn process_effects(&mut self, effs: Vec<super::actions::Effect>) {
        let flags = super::event_loop::session_flags_for_effects(&mut self.app, &effs);
        for eff in effs {
            let (_quit, _meta) = effects::execute(
                eff,
                &mut self.tasks,
                &self.app.acp_tx,
                &self.app.cwd,
                &flags,
                &self.progress_tx,
            );
        }
        self.drain_pending_effects();
    }

    /// Dispatch a user action and run its effects.
    fn act(&mut self, action: Action) {
        let effs = dispatch::dispatch(action, &mut self.app);
        self.process_effects(effs);
    }

    /// Pump until `pred(app)` holds, within [`TURN_BUDGET`]. No fixed sleeps
    /// beyond the pump tick; panics with `what` on expiry. Single-client sugar
    /// over [`pump_clients_until`] so there is exactly one pump loop.
    async fn pump_until(&mut self, what: &str, pred: impl Fn(&AppView) -> bool) {
        pump_clients_until(&mut [self], what, |clients| pred(&clients[0].app)).await;
    }

    /// The most recently created agent view (scenarios add tabs in order).
    fn latest_agent(&self) -> &AgentView {
        self.app
            .agents
            .values()
            .last()
            .expect("client has no agent view yet")
    }

    fn agent_for_session(&self, sid: &str) -> &AgentView {
        self.app
            .agents
            .values()
            .find(|a| {
                a.session
                    .session_id
                    .as_ref()
                    .is_some_and(|s| s.0.as_ref() == sid)
            })
            .unwrap_or_else(|| panic!("no agent view for session"))
    }

    /// Create a new session through the real dispatch → effect → agent path.
    async fn new_session(&mut self) -> String {
        self.act(Action::NewSession);
        self.pump_until("session/new completes", |app| {
            app.agents
                .values()
                .any(|a| a.session.session_id.is_some() && !a.session.loading_replay)
        })
        .await;
        self.latest_agent()
            .session
            .session_id
            .as_ref()
            .expect("session id set")
            .0
            .to_string()
    }

    /// Attach to an existing session (viewer path) and wait for the replay to
    /// land.
    async fn load_session(&mut self, sid: &str) {
        self.act(Action::LoadSession(sid.to_string(), None, false));
        let sid_owned = sid.to_string();
        self.pump_until("session/load completes", move |app| {
            app.agents.values().any(|a| {
                a.session
                    .session_id
                    .as_ref()
                    .is_some_and(|s| s.0.as_ref() == sid_owned)
                    && !a.session.loading_replay
            })
        })
        .await;
    }

    /// Drive one full turn on the active agent and wait until it lands
    /// (sentinel visible + agent back to Idle).
    async fn run_turn(&mut self, prompt: &str, sentinel: &str) {
        self.act(Action::SendPrompt(prompt.to_string()));
        let sentinel_owned = sentinel.to_string();
        self.pump_until("turn completes", move |app| {
            app.agents.values().any(|a| {
                matches!(a.session.state, AgentState::Idle)
                    && agent_message_text(a).contains(&sentinel_owned)
            })
        })
        .await;
    }

    fn sever(self) {
        self.bridge_cancel.cancel();
    }
}

/// Pump several clients until `pred` holds across them, within
/// [`TURN_BUDGET`]; panics with `what` on expiry.
async fn pump_clients_until(
    clients: &mut [&mut ClusterClient],
    what: &str,
    pred: impl Fn(&[&mut ClusterClient]) -> bool,
) {
    let deadline = tokio::time::Instant::now() + TURN_BUDGET;
    loop {
        for client in clients.iter_mut() {
            client.pump_once();
        }
        if pred(clients) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "pump_clients_until budget expired: {what}"
        );
        tokio::time::sleep(PUMP_TICK).await;
    }
}

/// The cluster: leader server + real agent, plus knobs to kill/respawn the
/// leader generation under the same socket path.
struct PagerLeaderCluster {
    sock_path: PathBuf,
    server: MockInferenceServer,
    server_cancel: CancellationToken,
    /// The current generation's server/agent/bridge tasks. `kill_leader`
    /// aborts + drains them so a respawn can never race a still-running old
    /// agent on the same ECHO_AGENT_HOME (two agents on one updates.jsonl is the
    /// corruption class the real leader's flock exists to prevent).
    generation_tasks: Vec<tokio::task::JoinHandle<()>>,
    client_count: Arc<AtomicUsize>,
    workdir: TempDir,
    authenticated: bool,
    /// Held for the cluster's lifetime so a `LeaderReconnector`-driven
    /// `connect_or_spawn` can never win the flock and spawn a subprocess —
    /// it always takes the wait-for-socket path onto our in-process server.
    _flock: LeaderLock,
    /// Restored on drop, INCLUDING panic unwinds. Field order is load-bearing:
    /// `_flock` drops first (removes its lock/sock files while the env still
    /// points at the sandbox), then the guards restore the env, then the temp
    /// home is deleted.
    _env: Vec<crate::test_util::EnvVarGuard>,
    _echo_agent_home: TempDir,
}

impl PagerLeaderCluster {
    /// Stand up the cluster. Callers MUST be `#[serial_test::serial(ECHO_AGENT_HOME)]`
    /// (env mutation) and run inside a current-thread `LocalSet`.
    async fn start() -> Self {
        echo_agent_extra_ca::ensure_default_crypto_provider();

        let server = MockInferenceServer::start().await.expect("mock server");
        let echo_agent_home = TempDir::new().unwrap();
        let workdir = TempDir::new().unwrap();
        let sock_path = echo_agent_home.path().join("leader-cluster.sock");

        let env = vec![
            crate::test_util::EnvVarGuard::set("ECHO_AGENT_HOME", echo_agent_home.path()),
            crate::test_util::EnvVarGuard::set("ECHO_AGENT_CLI_CHAT_PROXY_BASE_URL", server.url()),
            crate::test_util::EnvVarGuard::set("ECHO_AGENT_API_BASE_URL", server.url()),
            crate::test_util::EnvVarGuard::set("ECHO_AGENT_API_KEY", "test-key-for-ci"),
            crate::test_util::EnvVarGuard::set("ECHO_AGENT_TELEMETRY_ENABLED", "false"),
            crate::test_util::EnvVarGuard::set("ECHO_AGENT_FEEDBACK_ENABLED", "false"),
            crate::test_util::EnvVarGuard::set("ECHO_AGENT_TRACE_UPLOAD", "false"),
            // Pin every leader-path derivation (LeaderLock::new / reconnect's
            // connect_or_spawn) to this cluster's socket.
            crate::test_util::EnvVarGuard::set(LEADER_SOCKET_ENV, &sock_path),
        ];

        // Hold the flock for the cluster's lifetime (see field doc).
        let mut flock = LeaderLock::new("");
        assert!(
            flock.try_acquire().expect("acquire cluster flock"),
            "cluster flock unexpectedly held"
        );
        flock.write_pid().expect("stamp cluster flock");

        let client_count = Arc::new(AtomicUsize::new(0));
        let mut cluster = Self {
            sock_path,
            server,
            server_cancel: CancellationToken::new(),
            generation_tasks: Vec::new(),
            client_count,
            workdir,
            authenticated: false,
            _flock: flock,
            _env: env,
            _echo_agent_home: echo_agent_home,
        };
        cluster.spawn_leader_generation().await;
        cluster
    }

    /// Bind a fresh leader-server generation at the fixed socket path and
    /// wire a fresh REAL agent behind it.
    async fn spawn_leader_generation(&mut self) {
        let _ = std::fs::remove_file(&self.sock_path);
        let (acp_tx, acp_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let (response_tx, response_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let cancel = CancellationToken::new();
        self.server_cancel = cancel.clone();

        let control_state = LeaderServerControlState::new(LeaderServerMetadata {
            pid: std::process::id(),
            socket_path: self.sock_path.clone(),
            lock_path: self.sock_path.with_extension("lock"),
            ws_url_suffix: String::new(),
            // MUST be the client-side comparison source (echo_agent_version), not
            // this crate's version: a reconnecting client evicts strictly-older
            // leaders, and "evict" here would signal THIS test process.
            leader_binary_version: echo_agent_version::VERSION.to_string(),
        });
        let sock_for_server = self.sock_path.clone();
        let cancel_for_server = cancel.clone();
        let client_count_for_server = self.client_count.clone();
        let mut generation_tasks = Vec::new();
        generation_tasks.push(tokio::task::spawn_local(async move {
            let _ = run_leader_server(
                sock_for_server,
                acp_tx,
                response_rx,
                cancel_for_server,
                true,
                client_count_for_server,
                Arc::new(AtomicBool::new(false)),
                echo_agent_runtime::agent::activity::AgentActivity::default(),
                tokio::sync::watch::channel(true).1,
                tokio::sync::watch::channel(false).0,
                tokio::sync::watch::channel(echo_agent_runtime::leader::ShutdownReason::Manual).0,
                None,
                control_state,
            )
            .await;
        }));

        generation_tasks.extend(echo_agent_runtime::leader::in_process::spawn_agent(
            acp_rx,
            response_tx,
        ));
        self.generation_tasks = generation_tasks;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !self.sock_path.exists() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(self.sock_path.exists(), "leader socket never bound");
    }

    /// Kill the current leader generation (server + agent die together, like
    /// a real leader process crash) and wait for the socket to vanish.
    async fn kill_leader(&mut self) {
        self.server_cancel.cancel();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while self.sock_path.exists() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Fail HERE if the old generation never released the socket: its late
        // shutdown cleanup would otherwise delete the respawned generation's
        // fresh socket from under it (same-path race), which surfaces as a
        // confusing reconnect-budget expiry downstream.
        assert!(
            !self.sock_path.exists(),
            "old leader generation never released the socket"
        );
        // Abort + drain the generation's agent/bridge tasks (the server task
        // has already run its socket cleanup above). Channel-closure teardown
        // is only eventual; without this drain an old agent task could still
        // be running against the same ECHO_AGENT_HOME when the next generation's
        // agent starts — two writers on one updates.jsonl, the corruption
        // class the real leader's flock prevents.
        for task in self.generation_tasks.drain(..) {
            task.abort();
            let _ = task.await;
        }
        // The next generation's agent must re-authenticate its ACP surface.
        self.authenticated = false;
    }

    async fn respawn_leader(&mut self) {
        self.spawn_leader_generation().await;
    }

    /// Connect a pager client. With `reconnect: true` the bridge gets a real
    /// `LeaderReconnector` (socket pinned via `ECHO_AGENT_LEADER_SOCKET`, flock held
    /// by the cluster, so reconnects always adopt the in-process server).
    async fn client(&mut self, name: &str, reconnect: bool) -> ClusterClient {
        let conn = bounded(
            "client connect",
            LeaderClient::connect(
                self.sock_path.clone(),
                name,
                ClientMode::Stdio,
                LeaderClientCapabilities {
                    client_version: Some("0.0.0-test".to_string()),
                    ..Default::default()
                },
            ),
        )
        .await
        .expect("cluster client connect");
        let (leader_tx, leader_rx) = conn.into_channels();

        let cancel = CancellationToken::new();
        let (reconnector, status_rx) = if reconnect {
            let (status_tx, status_rx) = LeaderReconnector::status_channel();
            let reconnector = LeaderReconnector::new(
                name,
                ClientMode::Stdio,
                LeaderEnvUrls {
                    echo_agent_ws_url: String::new(),
                    echo_agent_ws_origin: String::new(),
                },
                LeaderClientCapabilities {
                    client_version: Some("0.0.0-test".to_string()),
                    ..Default::default()
                },
                status_tx,
            );
            (Some(reconnector), Some(status_rx))
        } else {
            (None, None)
        };

        let bridge = bridge_channels(
            leader_tx,
            leader_rx,
            cancel.clone(),
            reconnector,
            ReconnectPolicy::unbounded(),
        )
        .expect("bridge spawn");
        let tx = bridge.channel.tx;
        let rx = bridge.channel.rx;

        // Same handshake the pager performs after bridging (spawn path).
        let _init: acp::InitializeResponse = bounded(
            "initialize",
            acp_send(
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
                            "clientType": "pager-cluster",
                            "clientVersion": "0.0.0-test",
                        })
                        .as_object()
                        .cloned(),
                    ),
                &tx,
            ),
        )
        .await
        .expect("initialize through bridge");
        if !self.authenticated {
            let _: acp::AuthenticateResponse = bounded(
                "authenticate",
                acp_send(
                    acp::AuthenticateRequest::new(acp::AuthMethodId::new("echoagent.api_key"))
                        .meta(serde_json::json!({ "headless": true }).as_object().cloned()),
                    &tx,
                ),
            )
            .await
            .expect("authenticate through bridge");
            self.authenticated = true;
        }

        let mut app = AppView::new(tx, ModelState::default(), Vec::new());
        app.leader_mode = true;
        app.auth_state = AuthState::Done;
        app.trust_state = TrustState::Done;
        app.cwd = self.workdir.path().to_path_buf();

        let (progress_tx, progress_rx) = tokio::sync::mpsc::unbounded_channel();
        ClusterClient {
            app,
            rx,
            tasks: JoinSet::new(),
            progress_tx,
            _progress_rx: progress_rx,
            bridge_cancel: cancel,
            status_rx,
        }
    }

    /// Inference request count (chat/responses/messages only), for
    /// no-turn-was-re-driven invariants.
    fn inference_request_count(&self) -> usize {
        self.server
            .requests()
            .iter()
            .filter(|e| {
                e.path.contains("/chat/completions")
                    || e.path.contains("/responses")
                    || e.path.contains("/messages")
            })
            .count()
    }
}

impl Drop for PagerLeaderCluster {
    fn drop(&mut self) {
        self.server_cancel.cancel();
        // Best-effort (Drop cannot await): stop the generation's tasks so they
        // never outlive the env guards / temp dirs dropping right after.
        for task in self.generation_tasks.drain(..) {
            task.abort();
        }
    }
}

fn occurrences(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

mod scenarios;
