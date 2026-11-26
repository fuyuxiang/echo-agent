//! End-to-end failure-scenario suite for the MCP status dispatcher +
//! bounded auto-restart pipeline.
//!
//! Every test drives the **real** [`run_dispatcher`] loop —
//! `collect_window` (50 ms coalesce) → `collect_close_candidates` +
//! `drop_dead_clients` → `flush_window` (status push + `shutting_down`
//! book-keeping) → `maybe_schedule_restart` → `auto_restart_stdio`
//! bounded `[1,4,16]s` backoff — against a single mock that wires the
//! same three observation points production uses:
//!
//! 1. `mcp_state.owned_clients`  — did the dead `Arc<McpClient>` get torn down?
//! 2. the shared [`SharedShutdownState`] — did the teardown classify as intentional?
//! 3. the mock's recorded `respawn_stdio` calls + wire pushes — did auto-restart
//!    do the right thing (fire / skip / exhaust / retry)?
//!
//! The mock shares the same `SharedShutdownState` the dispatcher
//! mutates, so the dispatcher ↔ restart-actions binding is genuinely
//! exercised rather than stubbed on both sides.
//!
//! All tests run under `start_paused = true, flavor = "current_thread"`:
//! time only advances via explicit `tokio::time::advance`, so the
//! 50 ms window and the 1/4/16 s backoff fire deterministically with no
//! wall-clock sleeps.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use echo_agent_mcp::servers::{McpClient, McpClientEvent, McpState};
use tokio::sync::Mutex as TokioMutex;

use crate::session::mcp_dispatcher::{
    McpServerStatus, McpServerStatusPayload, McpServerStatusReason, SharedShutdownState,
    new_shutdown_state, run_dispatcher,
};
use crate::session::mcp_restart::RestartActions;

/// Past the 50 ms `collect_window` deadline so the window flushes.
const PAST_WINDOW: Duration = Duration::from_millis(60);

/// `RestartActions` mock for the e2e tests. It reads
/// `is_in_shutting_down` from the shared dispatcher state, scripts
/// per-server respawn outcomes, and captures every wire push — so a
/// single instance can observe the full crash → drop → restart loop
/// across multiple coalesce windows (flapping).
struct E2eActions {
    configured: RefCell<HashSet<String>>,
    outcomes: RefCell<HashMap<String, VecDeque<Result<(), String>>>>,
    respawn_calls: RefCell<Vec<String>>,
    pushes: RefCell<Vec<McpServerStatusPayload>>,
    /// Servers configured as HTTP/SSE (for `is_http_server_configured`).
    http_configured: RefCell<HashSet<String>>,
    /// Scripted `reset_http_client` outcomes, per server.
    reset_outcomes: RefCell<HashMap<String, VecDeque<Result<(), String>>>>,
    /// Recorded `reset_http_client` calls.
    reset_calls: RefCell<Vec<String>>,
    shutdown: SharedShutdownState,
    /// Shared `McpState` so a scripted-`Ok` `respawn_stdio` can mirror
    /// production by re-inserting the freshly-handshook client into
    /// `owned_clients` — letting flapping scenarios start each cycle
    /// from an "available" state without manual re-seeding.
    mcp_state: Arc<TokioMutex<McpState>>,
}

impl E2eActions {
    fn new(mcp_state: Arc<TokioMutex<McpState>>, shutdown: SharedShutdownState) -> Self {
        Self {
            configured: RefCell::new(HashSet::new()),
            outcomes: RefCell::new(HashMap::new()),
            respawn_calls: RefCell::new(Vec::new()),
            pushes: RefCell::new(Vec::new()),
            http_configured: RefCell::new(HashSet::new()),
            reset_outcomes: RefCell::new(HashMap::new()),
            reset_calls: RefCell::new(Vec::new()),
            shutdown,
            mcp_state,
        }
    }
    fn configure(&self, name: &str) {
        self.configured.borrow_mut().insert(name.to_string());
    }
    fn configure_http(&self, name: &str) {
        self.http_configured.borrow_mut().insert(name.to_string());
    }
    fn script_reset(&self, name: &str, outcome: Result<(), String>) {
        self.reset_outcomes
            .borrow_mut()
            .entry(name.to_string())
            .or_default()
            .push_back(outcome);
    }
    fn reset_calls(&self) -> Vec<String> {
        self.reset_calls.borrow().clone()
    }
    fn unconfigure(&self, name: &str) {
        self.configured.borrow_mut().remove(name);
    }
    /// Queue one `respawn_stdio` outcome for `name`. Popped FIFO per
    /// attempt; an empty queue surfaces `Err("not scripted")` so a test
    /// that under-scripts fails loudly instead of passing on a silent
    /// default.
    fn script(&self, name: &str, outcome: Result<(), String>) {
        self.outcomes
            .borrow_mut()
            .entry(name.to_string())
            .or_default()
            .push_back(outcome);
    }
    fn respawn_calls(&self) -> Vec<String> {
        self.respawn_calls.borrow().clone()
    }
    fn pushes(&self) -> Vec<McpServerStatusPayload> {
        self.pushes.borrow().clone()
    }
    fn pushes_with_reason(&self, reason: McpServerStatusReason) -> usize {
        self.pushes
            .borrow()
            .iter()
            .filter(|p| p.reason == reason)
            .count()
    }
}

#[async_trait::async_trait(?Send)]
impl RestartActions for E2eActions {
    async fn is_stdio_server_configured(&self, server: &str) -> bool {
        self.configured.borrow().contains(server)
    }
    fn is_in_shutting_down(&self, server: &str) -> bool {
        self.shutdown
            .lock()
            .expect("ShutdownState mutex poisoned")
            .is_shutting_down(server)
    }
    async fn respawn_stdio(&self, server: &str) -> Result<(), String> {
        self.respawn_calls.borrow_mut().push(server.to_string());
        // Panic on an unscripted call: a test that under-scripts its
        // outcomes is a test bug, and `Err("not scripted")` would
        // silently masquerade as a real respawn failure (e.g. inflating
        // a `RestartFailed`/`exhausted` count) and pass the wrong
        // assertion. A panic fails the test loudly at the exact call.
        let outcome = {
            let mut outcomes = self.outcomes.borrow_mut();
            outcomes
                .get_mut(server)
                .and_then(|q| q.pop_front())
                .unwrap_or_else(|| {
                    panic!("respawn_stdio({server}) called with no scripted outcome")
                })
        };
        // Mirror production on success: re-insert the recovered client
        // into `owned_clients` so the next crash cycle starts from an
        // "available" state without the test manually re-seeding.
        if outcome.is_ok() {
            self.mcp_state
                .lock()
                .await
                .owned_clients
                .insert(server.to_string(), Arc::new(McpClient::stub(server)));
        }
        outcome
    }
    fn push_status(&self, payload: &McpServerStatusPayload) {
        self.pushes.borrow_mut().push(payload.clone());
    }
    async fn is_http_server_configured(&self, server: &str) -> bool {
        self.http_configured.borrow().contains(server)
    }
    async fn reset_http_client(&self, server: &str) -> Result<(), String> {
        self.reset_calls.borrow_mut().push(server.to_string());
        self.reset_outcomes
            .borrow_mut()
            .get_mut(server)
            .and_then(|q| q.pop_front())
            .unwrap_or_else(|| Err("not scripted".to_string()))
    }
}

/// Discarding gateway: these tests assert on the drop / shutdown /
/// restart observation points, not on the ACP pushes `flush_window`
/// emits, so the gateway receiver is dropped.
fn discard_gateway() -> echo_agent_acp::AcpAgentGatewaySender {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    echo_agent_acp::AcpAgentGatewaySender::new(tx)
}

/// Yield enough times for the dispatcher task + any spawned
/// `auto_restart_stdio` task to make progress after a clock advance.
///
/// Why 8: after a `tokio::time::advance`, the work hops across several
/// independent `spawn_local` tasks, one task per `yield_now`. The
/// longest chain in these tests is:
///   1. dispatcher wakes from `collect_window`'s timer,
///   2. `drop_dead_clients` acquires the `McpState` lock,
///   3. `flush_window` emits,
///   4. `maybe_schedule_restart` `spawn_local`s `auto_restart_stdio`,
///   5. that task wakes from its backoff `select!`,
///   6. it runs the in-loop guard checks (one of which `.await`s
///      `is_stdio_server_configured`),
///   7. it `.await`s `respawn_stdio` (which now `.await`s the
///      `McpState` lock to re-insert), and
///   8. it pushes the status payload.
///
/// That's ~7 hand-offs; 8 yields is a small, fixed upper bound that
/// drains the whole chain deterministically under `start_paused` (no
/// wall-clock cost — `yield_now` doesn't advance the paused clock).
async fn settle() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

/// Pre-populate `owned_clients` with stub clients for `names`.
async fn seed_clients(state: &Arc<TokioMutex<McpState>>, names: &[&str]) {
    let mut guard = state.lock().await;
    for n in names {
        guard
            .owned_clients
            .insert((*n).to_string(), Arc::new(McpClient::stub(n)));
    }
}

async fn has_client(state: &Arc<TokioMutex<McpState>>, name: &str) -> bool {
    state.lock().await.owned_clients.contains_key(name)
}

/// Send a `TransportClosed` stamped with the currently registered
/// client's id, mirroring the production liveness watcher.
async fn send_transport_closed(
    tx: &tokio::sync::mpsc::UnboundedSender<McpClientEvent>,
    state: &Arc<TokioMutex<McpState>>,
    name: &str,
) {
    let client_id = state
        .lock()
        .await
        .owned_clients
        .get(name)
        .map(|c| c.client_id())
        .unwrap_or_else(|| panic!("send_transport_closed: no client registered for {name}"));
    tx.send(McpClientEvent::TransportClosed {
        server: name.to_string(),
        client_id,
    })
    .unwrap();
}

/// Scenario 1 — server crashes and recovers.
///
/// A `TransportClosed` for a configured stdio server must: drop the
/// dead `Arc<McpClient>` from `owned_clients`, schedule a restart, and
/// (respawn scripted `Ok`) emit exactly one `RestartSucceeded` push
/// without marking the server as an intentional teardown.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn e2e_crash_recovers_drops_client_then_restart_succeeds() {
    let mcp_state = Arc::new(TokioMutex::new(McpState::new(vec![])));
    seed_clients(&mcp_state, &["svr"]).await;
    let shutdown = new_shutdown_state();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<McpClientEvent>();

    let actions = Rc::new(E2eActions::new(
        Arc::clone(&mcp_state),
        Arc::clone(&shutdown),
    ));
    actions.configure("svr");
    actions.script("svr", Ok(()));
    let assert_actions = Rc::clone(&actions);
    let restart_actions: Rc<dyn RestartActions> = actions;

    let state_for_dispatcher = Arc::clone(&mcp_state);
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let dispatcher = tokio::task::spawn_local(run_dispatcher(
                "sess-1".to_string(),
                rx,
                discard_gateway(),
                state_for_dispatcher,
                Arc::clone(&shutdown),
                Some(restart_actions),
                std::path::PathBuf::from("."),
            ));

            send_transport_closed(&tx, &mcp_state, "svr").await;
            tokio::task::yield_now().await;
            tokio::time::advance(PAST_WINDOW).await; // close window: drop + flush + schedule
            settle().await;

            // After the window closes (but before the backoff fires) the
            // dead client must be gone from owned_clients.
            assert!(
                !has_client(&mcp_state, "svr").await,
                "TransportClosed must drop the dead client from owned_clients",
            );

            tokio::time::advance(Duration::from_secs(1)).await; // BACKOFF[0]
            settle().await;

            // The scripted-Ok respawn mirrors production by re-inserting
            // the recovered client, so it is back in owned_clients now.
            assert!(
                has_client(&mcp_state, "svr").await,
                "successful restart must re-insert the recovered client",
            );
            assert_eq!(
                assert_actions.respawn_calls(),
                vec!["svr".to_string()],
                "exactly one respawn attempt",
            );
            let pushes = assert_actions.pushes();
            assert_eq!(
                pushes.len(),
                1,
                "exactly one push on success; got {pushes:?}"
            );
            assert_eq!(pushes[0].reason, McpServerStatusReason::RestartSucceeded);
            assert_eq!(pushes[0].status, McpServerStatus::Ready);
            assert!(
                !shutdown.lock().unwrap().is_shutting_down("svr"),
                "a crash is NOT an intentional teardown",
            );

            dispatcher.abort();
        })
        .await;
}

/// Scenario 2 — server is permanently dead.
///
/// Three scripted `Err` respawns exhaust the `[1,4,16]s` backoff:
/// 3 per-attempt `RestartFailed` pushes + 1 final exhausted
/// `RestartFailed`, and the client stays dropped.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn e2e_crash_permanently_dead_exhausts_after_three_attempts() {
    let mcp_state = Arc::new(TokioMutex::new(McpState::new(vec![])));
    seed_clients(&mcp_state, &["svr"]).await;
    let shutdown = new_shutdown_state();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<McpClientEvent>();

    let actions = Rc::new(E2eActions::new(
        Arc::clone(&mcp_state),
        Arc::clone(&shutdown),
    ));
    actions.configure("svr");
    actions.script("svr", Err("reset 1".into()));
    actions.script("svr", Err("reset 2".into()));
    actions.script("svr", Err("reset 3".into()));
    let assert_actions = Rc::clone(&actions);
    let restart_actions: Rc<dyn RestartActions> = actions;

    let state_for_dispatcher = Arc::clone(&mcp_state);
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let dispatcher = tokio::task::spawn_local(run_dispatcher(
                "sess-1".to_string(),
                rx,
                discard_gateway(),
                state_for_dispatcher,
                Arc::clone(&shutdown),
                Some(restart_actions),
                std::path::PathBuf::from("."),
            ));

            send_transport_closed(&tx, &mcp_state, "svr").await;
            tokio::task::yield_now().await;
            tokio::time::advance(PAST_WINDOW).await;
            settle().await;
            // Step each backoff interval so every re-armed sleep fires;
            // a single 21s jump would only trip the first attempt's
            // already-armed timer. Drive from the production `BACKOFF`
            // constant so this test can't drift from the real schedule.
            for wait in crate::session::mcp_restart::BACKOFF {
                tokio::time::advance(wait).await;
                settle().await;
            }

            assert!(!has_client(&mcp_state, "svr").await);
            assert_eq!(assert_actions.respawn_calls().len(), 3);
            let pushes = assert_actions.pushes();
            assert_eq!(pushes.len(), 4, "3 attempts + 1 exhausted; got {pushes:?}");
            for p in &pushes {
                assert_eq!(p.reason, McpServerStatusReason::RestartFailed);
                assert_eq!(p.status, McpServerStatus::Unavailable);
            }
            // Per-attempt details encode their 1-based attempt index and
            // carry the scripted error string.
            for (i, expected_err) in ["reset 1", "reset 2", "reset 3"].iter().enumerate() {
                let want = format!("attempt {} of 3: {expected_err}", i + 1);
                assert_eq!(
                    pushes[i].detail.as_deref(),
                    Some(want.as_str()),
                    "push[{i}] detail",
                );
            }
            assert_eq!(
                pushes[3].detail.as_deref(),
                Some("exhausted after 3 attempts"),
            );

            dispatcher.abort();
        })
        .await;
}

/// Scenario 3 — handshake failure triggers a restart.
///
/// `HandshakeFailed` is a restart trigger but is NOT in the dead-client
/// drop set (only `TransportClosed`/`ConfigRemoved` drop). So a
/// configured stdio server's seeded client must SURVIVE while the
/// restart is still scheduled and succeeds.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn e2e_handshake_failed_schedules_restart_without_dropping_client() {
    let mcp_state = Arc::new(TokioMutex::new(McpState::new(vec![])));
    seed_clients(&mcp_state, &["svr"]).await;
    let shutdown = new_shutdown_state();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<McpClientEvent>();

    let actions = Rc::new(E2eActions::new(
        Arc::clone(&mcp_state),
        Arc::clone(&shutdown),
    ));
    actions.configure("svr");
    actions.script("svr", Ok(()));
    let assert_actions = Rc::clone(&actions);
    let restart_actions: Rc<dyn RestartActions> = actions;

    let state_for_dispatcher = Arc::clone(&mcp_state);
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let dispatcher = tokio::task::spawn_local(run_dispatcher(
                "sess-1".to_string(),
                rx,
                discard_gateway(),
                state_for_dispatcher,
                Arc::clone(&shutdown),
                Some(restart_actions),
                std::path::PathBuf::from("."),
            ));

            tx.send(McpClientEvent::HandshakeFailed {
                server: "svr".to_string(),
                reason: "boom".to_string(),
            })
            .unwrap();
            tokio::task::yield_now().await;
            tokio::time::advance(PAST_WINDOW).await;
            settle().await;
            tokio::time::advance(Duration::from_secs(1)).await;
            settle().await;

            assert!(
                has_client(&mcp_state, "svr").await,
                "HandshakeFailed must NOT drop owned_clients (only TransportClosed does)",
            );
            assert_eq!(assert_actions.respawn_calls(), vec!["svr".to_string()]);
            let pushes = assert_actions.pushes();
            assert_eq!(pushes.len(), 1);
            assert_eq!(pushes[0].reason, McpServerStatusReason::RestartSucceeded);

            dispatcher.abort();
        })
        .await;
}

/// Scenario 4 — user removes the server (config diff).
///
/// `ConfigDiff{removed}` must mark the server `shutting_down` and
/// schedule NO restart — and must NOT evict whatever client is
/// registered under that name. `ConfigRemoved` is excluded from
/// eviction entirely (see `collect_close_candidates`); the
/// remove+re-add race where the registered entry is a fresh
/// replacement is modeled end-to-end by
/// `e2e_remove_readd_race_keeps_replacement_client`.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn e2e_config_removed_keeps_replacement_client_marks_shutdown_no_restart() {
    let mcp_state = Arc::new(TokioMutex::new(McpState::new(vec![])));
    seed_clients(&mcp_state, &["svr"]).await;
    let shutdown = new_shutdown_state();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<McpClientEvent>();

    let actions = Rc::new(E2eActions::new(
        Arc::clone(&mcp_state),
        Arc::clone(&shutdown),
    ));
    // Configured-as-stdio on purpose: proves the no-restart outcome is
    // driven by the event KIND (ConfigRemoved), not by the stdio guard
    // rail.
    actions.configure("svr");
    let assert_actions = Rc::clone(&actions);
    let restart_actions: Rc<dyn RestartActions> = actions;

    let state_for_dispatcher = Arc::clone(&mcp_state);
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let dispatcher = tokio::task::spawn_local(run_dispatcher(
                "sess-1".to_string(),
                rx,
                discard_gateway(),
                state_for_dispatcher,
                Arc::clone(&shutdown),
                Some(restart_actions),
                std::path::PathBuf::from("."),
            ));

            tx.send(McpClientEvent::ConfigDiff {
                added: vec![],
                removed: vec!["svr".to_string()],
            })
            .unwrap();
            tokio::task::yield_now().await;
            tokio::time::advance(PAST_WINDOW).await;
            settle().await;
            tokio::time::advance(Duration::from_secs(1)).await;
            settle().await;

            assert!(
                has_client(&mcp_state, "svr").await,
                "ConfigRemoved must not evict the registered client",
            );
            assert!(
                shutdown.lock().unwrap().is_shutting_down("svr"),
                "ConfigRemoved marks intentional teardown",
            );
            assert!(
                assert_actions.respawn_calls().is_empty(),
                "ConfigRemoved must never schedule a restart",
            );

            dispatcher.abort();
        })
        .await;
}

/// Scenario 5 — intentional shutdown suppresses a follow-up crash.
///
/// The kill_on_drop guard rail end-to-end: window 1 removes the server
/// (marks `shutting_down`); window 2's `TransportClosed` (the SIGKILL'd
/// child's death rattle) must be skipped — no respawn.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn e2e_intentional_shutdown_suppresses_restart_on_transport_closed() {
    let mcp_state = Arc::new(TokioMutex::new(McpState::new(vec![])));
    seed_clients(&mcp_state, &["svr"]).await;
    let shutdown = new_shutdown_state();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<McpClientEvent>();

    let actions = Rc::new(E2eActions::new(
        Arc::clone(&mcp_state),
        Arc::clone(&shutdown),
    ));
    actions.configure("svr");
    let assert_actions = Rc::clone(&actions);
    let restart_actions: Rc<dyn RestartActions> = actions;

    let state_for_dispatcher = Arc::clone(&mcp_state);
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let dispatcher = tokio::task::spawn_local(run_dispatcher(
                "sess-1".to_string(),
                rx,
                discard_gateway(),
                state_for_dispatcher,
                Arc::clone(&shutdown),
                Some(restart_actions),
                std::path::PathBuf::from("."),
            ));

            // Window 1: config removal marks shutting_down.
            tx.send(McpClientEvent::ConfigDiff {
                added: vec![],
                removed: vec!["svr".to_string()],
            })
            .unwrap();
            tokio::task::yield_now().await;
            tokio::time::advance(PAST_WINDOW).await;
            settle().await;
            assert!(shutdown.lock().unwrap().is_shutting_down("svr"));

            // Window 2: the kill_on_drop death rattle arrives.
            send_transport_closed(&tx, &mcp_state, "svr").await;
            tokio::task::yield_now().await;
            tokio::time::advance(PAST_WINDOW).await;
            settle().await;
            tokio::time::advance(Duration::from_secs(1)).await;
            settle().await;

            assert!(
                assert_actions.respawn_calls().is_empty(),
                "a TransportClosed for a shutting_down server must NOT restart",
            );
            // No restart task ran, so the restart-actions push sink must
            // be empty — in particular zero `RestartFailed` pushes (the
            // ConfigRemoved status goes to the gateway, not push_status).
            assert_eq!(
                assert_actions.pushes_with_reason(McpServerStatusReason::RestartFailed),
                0,
                "intentional-shutdown suppression must emit no RestartFailed push",
            );
            assert!(
                assert_actions.pushes().is_empty(),
                "no restart task ran, so no restart-status pushes; got {:?}",
                assert_actions.pushes(),
            );

            dispatcher.abort();
        })
        .await;
}

/// Scenario 6 — HTTP / unconfigured server crashes.
///
/// `TransportClosed` for a server that is NOT a configured stdio entry
/// (production's gate returns `false` for HTTP/HttpAuth) must still drop
/// the dead client, but schedule NO restart — HTTP recovers via
/// `reset_transport` on the next tool call.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn e2e_unconfigured_http_server_drops_client_but_no_restart() {
    let mcp_state = Arc::new(TokioMutex::new(McpState::new(vec![])));
    seed_clients(&mcp_state, &["http-svr"]).await;
    let shutdown = new_shutdown_state();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<McpClientEvent>();

    // Intentionally NOT configured as stdio.
    let actions = Rc::new(E2eActions::new(
        Arc::clone(&mcp_state),
        Arc::clone(&shutdown),
    ));
    let assert_actions = Rc::clone(&actions);
    let restart_actions: Rc<dyn RestartActions> = actions;

    let state_for_dispatcher = Arc::clone(&mcp_state);
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let dispatcher = tokio::task::spawn_local(run_dispatcher(
                "sess-1".to_string(),
                rx,
                discard_gateway(),
                state_for_dispatcher,
                Arc::clone(&shutdown),
                Some(restart_actions),
                std::path::PathBuf::from("."),
            ));

            send_transport_closed(&tx, &mcp_state, "http-svr").await;
            tokio::task::yield_now().await;
            tokio::time::advance(PAST_WINDOW).await;
            settle().await;
            tokio::time::advance(Duration::from_secs(1)).await;
            settle().await;

            assert!(
                !has_client(&mcp_state, "http-svr").await,
                "TransportClosed drops the client regardless of transport type",
            );
            assert!(
                assert_actions.respawn_calls().is_empty(),
                "non-stdio (unconfigured) servers must not auto-restart",
            );
            assert_eq!(
                assert_actions.pushes().len(),
                0,
                "unconfigured/HTTP crash schedules nothing, so no restart-status pushes; got {:?}",
                assert_actions.pushes(),
            );

            dispatcher.abort();
        })
        .await;
}

/// Scenario 7 — server disabled mid-backoff.
///
/// A configured stdio server crashes and a restart is scheduled, but the
/// user toggles it off (config flips to unconfigured) during the first
/// backoff sleep. The loop's in-iteration re-check must skip the respawn
/// and emit a single `Disabled` push instead of `RestartFailed`.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn e2e_server_disabled_mid_backoff_emits_disabled_no_respawn() {
    let mcp_state = Arc::new(TokioMutex::new(McpState::new(vec![])));
    seed_clients(&mcp_state, &["svr"]).await;
    let shutdown = new_shutdown_state();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<McpClientEvent>();

    let actions = Rc::new(E2eActions::new(
        Arc::clone(&mcp_state),
        Arc::clone(&shutdown),
    ));
    actions.configure("svr");
    let assert_actions = Rc::clone(&actions);
    let restart_actions: Rc<dyn RestartActions> = actions;

    let state_for_dispatcher = Arc::clone(&mcp_state);
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let dispatcher = tokio::task::spawn_local(run_dispatcher(
                "sess-1".to_string(),
                rx,
                discard_gateway(),
                state_for_dispatcher,
                Arc::clone(&shutdown),
                Some(restart_actions),
                std::path::PathBuf::from("."),
            ));

            send_transport_closed(&tx, &mcp_state, "svr").await;
            tokio::task::yield_now().await;
            tokio::time::advance(PAST_WINDOW).await; // schedule restart
            settle().await;

            // Toggle the server off BEFORE the first 1s backoff fires.
            assert_actions.unconfigure("svr");
            tokio::time::advance(Duration::from_secs(1)).await;
            settle().await;

            assert!(
                assert_actions.respawn_calls().is_empty(),
                "respawn must not run once the server is unconfigured mid-backoff",
            );
            let pushes = assert_actions.pushes();
            assert_eq!(pushes.len(), 1, "exactly one terminal push; got {pushes:?}");
            assert_eq!(
                pushes[0].reason,
                McpServerStatusReason::Disabled,
                "mid-backoff disable surfaces as Disabled, not RestartFailed",
            );
            assert_eq!(pushes[0].status, McpServerStatus::Unavailable);

            dispatcher.abort();
        })
        .await;
}

/// Scenario 8 — burst of crash events coalesces to a single restart.
///
/// A flapping server that emits 50 `TransportClosed` notifications
/// inside one 50 ms window must collapse to ONE coalesced key and
/// therefore exactly ONE scheduled restart — not 50 racing respawn
/// tasks.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn e2e_burst_transport_closed_coalesces_to_single_restart() {
    let mcp_state = Arc::new(TokioMutex::new(McpState::new(vec![])));
    seed_clients(&mcp_state, &["svr"]).await;
    let shutdown = new_shutdown_state();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<McpClientEvent>();

    let actions = Rc::new(E2eActions::new(
        Arc::clone(&mcp_state),
        Arc::clone(&shutdown),
    ));
    actions.configure("svr");
    actions.script("svr", Ok(()));
    let assert_actions = Rc::clone(&actions);
    let restart_actions: Rc<dyn RestartActions> = actions;

    let state_for_dispatcher = Arc::clone(&mcp_state);
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let dispatcher = tokio::task::spawn_local(run_dispatcher(
                "sess-1".to_string(),
                rx,
                discard_gateway(),
                state_for_dispatcher,
                Arc::clone(&shutdown),
                Some(restart_actions),
                std::path::PathBuf::from("."),
            ));

            for _ in 0..50 {
                send_transport_closed(&tx, &mcp_state, "svr").await;
            }
            tokio::task::yield_now().await;
            tokio::time::advance(PAST_WINDOW).await;
            settle().await;
            tokio::time::advance(Duration::from_secs(1)).await;
            settle().await;

            assert_eq!(
                assert_actions.respawn_calls(),
                vec!["svr".to_string()],
                "50 coalesced TransportClosed must schedule exactly one restart",
            );
            // One coalesced restart → exactly one success push, no
            // duplicate from any racing-but-coalesced sibling event.
            let pushes = assert_actions.pushes();
            assert_eq!(
                pushes.len(),
                1,
                "coalesced burst yields exactly one restart push; got {pushes:?}",
            );
            assert_eq!(pushes[0].reason, McpServerStatusReason::RestartSucceeded);

            dispatcher.abort();
        })
        .await;
}

/// Scenario 9 — flapping server: never reliably available.
///
/// A server that repeatedly crashes across SEPARATE coalesce windows
/// (crash → restart succeeds → crash again → restart succeeds → …) must
/// produce one independent restart cycle per crash. This is the
/// canonical "MCP server is flapping / not always available" case: each
/// crash is treated as a fresh transport death (the previous `Ready`
/// cleared any shutdown mark), so every cycle drops the client and
/// re-restarts.
///
/// Models three crash→recover cycles. The mock's `respawn_stdio` now
/// re-inserts the recovered `Arc<McpClient>` into `owned_clients` on a
/// scripted `Ok` (mirroring production), so each cycle starts from an
/// "available" state with no manual re-seeding between cycles.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn e2e_flapping_server_restarts_on_each_crash_cycle() {
    let mcp_state = Arc::new(TokioMutex::new(McpState::new(vec![])));
    seed_clients(&mcp_state, &["flappy"]).await;
    let shutdown = new_shutdown_state();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<McpClientEvent>();

    let actions = Rc::new(E2eActions::new(
        Arc::clone(&mcp_state),
        Arc::clone(&shutdown),
    ));
    actions.configure("flappy");
    // Every restart attempt across all cycles succeeds.
    for _ in 0..3 {
        actions.script("flappy", Ok(()));
    }
    let assert_actions = Rc::clone(&actions);
    let restart_actions: Rc<dyn RestartActions> = actions;

    let state_for_dispatcher = Arc::clone(&mcp_state);
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let dispatcher = tokio::task::spawn_local(run_dispatcher(
                "sess-1".to_string(),
                rx,
                discard_gateway(),
                state_for_dispatcher,
                Arc::clone(&shutdown),
                Some(restart_actions),
                std::path::PathBuf::from("."),
            ));

            for cycle in 0..3 {
                // Each cycle: client is up, then the transport dies.
                send_transport_closed(&tx, &mcp_state, "flappy").await;
                tokio::task::yield_now().await;
                tokio::time::advance(PAST_WINDOW).await; // drop + flush + schedule
                settle().await;

                assert!(
                    !has_client(&mcp_state, "flappy").await,
                    "cycle {cycle}: crash must drop the client",
                );
                assert!(
                    !shutdown.lock().unwrap().is_shutting_down("flappy"),
                    "cycle {cycle}: a crash is never an intentional teardown",
                );

                tokio::time::advance(Duration::from_secs(1)).await; // BACKOFF[0] → respawn
                settle().await;

                assert_eq!(
                    assert_actions.respawn_calls().len(),
                    cycle + 1,
                    "cycle {cycle}: exactly one new respawn per crash cycle",
                );

                // The mock's `respawn_stdio` re-inserted the recovered
                // client on its scripted `Ok`, so the next cycle already
                // starts from an "available" state — no manual re-seed.
                assert!(
                    has_client(&mcp_state, "flappy").await,
                    "cycle {cycle}: successful respawn must re-insert the client",
                );
            }

            assert_eq!(
                assert_actions.respawn_calls(),
                vec!["flappy".to_string(); 3],
                "three crash cycles → three independent restarts",
            );
            assert_eq!(
                assert_actions.pushes_with_reason(McpServerStatusReason::RestartSucceeded),
                3,
                "each flap recovers with its own RestartSucceeded push",
            );

            dispatcher.abort();
        })
        .await;
}

/// Scenario 10 — intermittently healthy: transient failure then recovery
/// within a single restart window.
///
/// A flapping server whose first respawn fails (still unhealthy) but
/// whose second respawn succeeds must NOT exhaust: the backoff loop
/// retries and recovers. Expect 2 respawn calls and pushes
/// `[RestartFailed(attempt 1), RestartSucceeded]` — and crucially no
/// `exhausted` push, since recovery happened before the third attempt.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn e2e_intermittently_healthy_recovers_after_transient_failure() {
    let mcp_state = Arc::new(TokioMutex::new(McpState::new(vec![])));
    seed_clients(&mcp_state, &["svr"]).await;
    let shutdown = new_shutdown_state();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<McpClientEvent>();

    let actions = Rc::new(E2eActions::new(
        Arc::clone(&mcp_state),
        Arc::clone(&shutdown),
    ));
    actions.configure("svr");
    // Attempt 1 fails (server still flapping), attempt 2 succeeds.
    actions.script("svr", Err("handshake timeout".into()));
    actions.script("svr", Ok(()));
    let assert_actions = Rc::clone(&actions);
    let restart_actions: Rc<dyn RestartActions> = actions;

    let state_for_dispatcher = Arc::clone(&mcp_state);
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let dispatcher = tokio::task::spawn_local(run_dispatcher(
                "sess-1".to_string(),
                rx,
                discard_gateway(),
                state_for_dispatcher,
                Arc::clone(&shutdown),
                Some(restart_actions),
                std::path::PathBuf::from("."),
            ));

            send_transport_closed(&tx, &mcp_state, "svr").await;
            tokio::task::yield_now().await;
            tokio::time::advance(PAST_WINDOW).await;
            settle().await;

            // Attempt 1 at t=+1s fails.
            tokio::time::advance(Duration::from_secs(1)).await;
            settle().await;
            assert_eq!(assert_actions.respawn_calls().len(), 1);

            // Attempt 2 at t=+4s succeeds — recovery before exhaustion.
            tokio::time::advance(Duration::from_secs(4)).await;
            settle().await;

            assert_eq!(
                assert_actions.respawn_calls().len(),
                2,
                "second attempt fires after the 4s backoff",
            );
            let pushes = assert_actions.pushes();
            assert_eq!(pushes.len(), 2, "one failure + one success; got {pushes:?}");
            assert_eq!(pushes[0].reason, McpServerStatusReason::RestartFailed);
            assert!(
                matches!(pushes[0].detail.as_deref(), Some(s) if s.starts_with("attempt 1 of 3")),
                "first push records the transient attempt-1 failure: {:?}",
                pushes[0].detail,
            );
            assert_eq!(pushes[1].reason, McpServerStatusReason::RestartSucceeded);
            assert_eq!(pushes[1].status, McpServerStatus::Ready);
            assert_eq!(
                assert_actions.pushes_with_reason(McpServerStatusReason::RestartFailed),
                1,
                "recovery on attempt 2 means NO exhausted push",
            );

            dispatcher.abort();
        })
        .await;
}

/// Scenario 11 — auto-restart disabled (`restart_actions: None`).
///
/// The kill-switch path: with `mcp.auto_restart=false` the dispatcher
/// receives `None` and must still drop the dead `Arc<McpClient>` on
/// `TransportClosed` (the H1 teardown is independent of auto-restart),
/// while skipping the restart branch entirely — no task is scheduled
/// and the `None` is never unwrapped. Guards the otherwise-untested
/// `restart_actions.is_none()` arm of `run_dispatcher`.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn e2e_auto_restart_disabled_drops_client_but_schedules_nothing() {
    let mcp_state = Arc::new(TokioMutex::new(McpState::new(vec![])));
    seed_clients(&mcp_state, &["svr"]).await;
    let shutdown = new_shutdown_state();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<McpClientEvent>();

    // No restart actions — auto-restart is off for this session.
    let restart_actions: Option<Rc<dyn RestartActions>> = None;

    let state_for_dispatcher = Arc::clone(&mcp_state);
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let dispatcher = tokio::task::spawn_local(run_dispatcher(
                "sess-1".to_string(),
                rx,
                discard_gateway(),
                state_for_dispatcher,
                Arc::clone(&shutdown),
                restart_actions,
                std::path::PathBuf::from("."),
            ));

            send_transport_closed(&tx, &mcp_state, "svr").await;
            tokio::task::yield_now().await;
            tokio::time::advance(PAST_WINDOW).await; // drop + flush, no schedule
            settle().await;
            // Advance well past the first backoff to prove nothing was
            // scheduled to fire later.
            tokio::time::advance(Duration::from_secs(1)).await;
            settle().await;

            assert!(
                !has_client(&mcp_state, "svr").await,
                "TransportClosed drops the client even with auto-restart disabled",
            );
            assert!(
                !shutdown.lock().unwrap().is_shutting_down("svr"),
                "a crash is not an intentional teardown, even with restarts off",
            );

            dispatcher.abort();
        })
        .await;
}

/// Scenario 12 — config remove+re-add race (non-managed HTTP server):
/// the old client is removed and a replacement handshakes
/// inside ONE coalesce window; the old client's `ConfigRemoved` and
/// stale `TransportClosed` then flush. The replacement must survive —
/// pre-fix, name-keyed eviction destroyed it, leaving tools
/// registered but no client ("MCP server 'demo-mcp' not found").
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn e2e_remove_readd_race_keeps_replacement_client() {
    let mcp_state = Arc::new(TokioMutex::new(McpState::new(vec![])));
    let old_client = Arc::new(McpClient::stub("demo-mcp"));
    let old_id = old_client.client_id();
    mcp_state
        .lock()
        .await
        .owned_clients
        .insert("demo-mcp".to_string(), Arc::clone(&old_client));
    let shutdown = new_shutdown_state();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<McpClientEvent>();

    let actions = Rc::new(E2eActions::new(
        Arc::clone(&mcp_state),
        Arc::clone(&shutdown),
    ));
    // Configure `demo-mcp` as a restart-eligible stdio server so the
    // no-respawn assertion has teeth: if the stale close were NOT stripped
    // it would schedule a respawn. The strip is what keeps it inert.
    actions.configure("demo-mcp");
    let assert_actions = Rc::clone(&actions);
    let restart_actions: Rc<dyn RestartActions> = actions;

    let state_for_dispatcher = Arc::clone(&mcp_state);
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let dispatcher = tokio::task::spawn_local(run_dispatcher(
                "sess-1".to_string(),
                rx,
                discard_gateway(),
                state_for_dispatcher,
                Arc::clone(&shutdown),
                Some(restart_actions),
                std::path::PathBuf::from("."),
            ));

            // 1. The session actor's config diff: old client removed
            //    synchronously, then the ConfigDiff event is emitted.
            mcp_state.lock().await.owned_clients.remove("demo-mcp");
            tx.send(McpClientEvent::ConfigDiff {
                added: vec!["demo-mcp".to_string()],
                removed: vec!["demo-mcp".to_string()],
            })
            .unwrap();

            // 2. The old client's lingering liveness watcher fires its
            //    death rattle, stamped with the OLD client's id.
            tx.send(McpClientEvent::TransportClosed {
                server: "demo-mcp".to_string(),
                client_id: old_id,
            })
            .unwrap();

            // 3. The replacement handshake completes before the 50 ms
            //    window flushes (16 ms in the incident).
            let replacement = Arc::new(McpClient::stub("demo-mcp"));
            assert_ne!(replacement.client_id(), old_id);
            mcp_state
                .lock()
                .await
                .owned_clients
                .insert("demo-mcp".to_string(), Arc::clone(&replacement));

            // 4. Flush.
            tokio::task::yield_now().await;
            tokio::time::advance(PAST_WINDOW).await;
            settle().await;
            tokio::time::advance(Duration::from_secs(1)).await;
            settle().await;

            let guard = mcp_state.lock().await;
            let current = guard
                .owned_clients
                .get("demo-mcp")
                .expect("replacement client must survive the remove+re-add window flush");
            assert!(
                Arc::ptr_eq(current, &replacement),
                "the registered client must be the replacement instance",
            );
            drop(guard);
            assert!(
                assert_actions.respawn_calls().is_empty(),
                "the stale close is stripped before restart scheduling, so no \
                 respawn fires even though demo-mcp is restart-eligible",
            );

            dispatcher.abort();
        })
        .await;
}

/// Scenario — non-managed HTTP server (e.g. `http-mcp-server`) drops mid-session.
///
/// Pins the core symptom end-to-end through the real `run_dispatcher`:
/// a `TransportClosed` for an HTTP server in `McpState::configs` must
/// NOT evict the client (that was the `MCP server '<name>' not found`
/// bug) and must instead schedule an in-place `reset_http_client`. No
/// stdio respawn fires.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn e2e_http_transport_closed_recovers_in_place_not_evicted() {
    // `http-mcp-server` is present in configs as HTTP so the dispatcher's
    // `recoverable_http_servers` classifies it as recoverable.
    let http_mcp_cfg = agent_client_protocol::McpServer::Http(
        agent_client_protocol::McpServerHttp::new(
            "http-mcp-server".to_string(),
            "https://relay.test/mcp".to_string(),
        )
        .headers(vec![]),
    );
    let mcp_state = Arc::new(TokioMutex::new(McpState::new(vec![http_mcp_cfg])));
    seed_clients(&mcp_state, &["http-mcp-server"]).await;
    let shutdown = new_shutdown_state();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<McpClientEvent>();

    let actions = Rc::new(E2eActions::new(
        Arc::clone(&mcp_state),
        Arc::clone(&shutdown),
    ));
    actions.configure_http("http-mcp-server");
    actions.script_reset("http-mcp-server", Ok(()));
    let assert_actions = Rc::clone(&actions);
    let restart_actions: Rc<dyn RestartActions> = actions;

    let state_for_dispatcher = Arc::clone(&mcp_state);
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let dispatcher = tokio::task::spawn_local(run_dispatcher(
                "sess-1".to_string(),
                rx,
                discard_gateway(),
                state_for_dispatcher,
                Arc::clone(&shutdown),
                Some(restart_actions),
                std::path::PathBuf::from("."),
            ));

            send_transport_closed(&tx, &mcp_state, "http-mcp-server").await;
            tokio::task::yield_now().await;
            tokio::time::advance(PAST_WINDOW).await; // close window
            settle().await;

            assert!(
                has_client(&mcp_state, "http-mcp-server").await,
                "HTTP TransportClosed must NOT evict the client (in-place recovery)",
            );
            assert_eq!(
                assert_actions.reset_calls(),
                vec!["http-mcp-server".to_string()],
                "HTTP TransportClosed must schedule exactly one in-place reset",
            );
            assert!(
                assert_actions.respawn_calls().is_empty(),
                "HTTP recovery must not trigger a stdio respawn",
            );
            assert!(
                !shutdown.lock().unwrap().is_shutting_down("http-mcp-server"),
                "a transport drop is not an intentional teardown",
            );

            dispatcher.abort();
        })
        .await;
}
