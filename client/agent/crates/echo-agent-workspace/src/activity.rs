//! Per-session and connection-level activity tracking for tool server
//! lifecycle status reporting.

// A panic on a teardown path leaks whatever it was about to free; tests panic freely.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented
    )
)]

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use dashmap::DashMap;
use echo_agent_file_utils::queue::UploadQueueStats;
use echo_agent_session_events::{Event, EventWriter, ToolCompletedSource, ToolOutcome};
use echo_agent_tool_protocol::{
    IdleWithholdReason, ToolServerLifecycleStatus, ToolServerStatusPayload,
};

const LIFECYCLE_NONE: u8 = 0;
const LIFECYCLE_DRAINING: u8 = 1;
const LIFECYCLE_SHUTTING_DOWN: u8 = 2;

const DEFAULT_SESSION: &str = "__default__";
const SESSION_IDLE_PRUNE_MS: u64 = 5 * 60 * 1000;

/// Default cap (ms) on how long pending durability work (artifact producers /
/// queued uploads) may withhold `idle_since_ms`. Overridable via
/// `ECHO_AGENT_WORKSPACE_DURABILITY_IDLE_HOLD_MAX_MS`.
const DEFAULT_DURABILITY_IDLE_HOLD_MAX_MS: u64 = 600_000;

/// How long recent preview-proxy traffic withholds `idle_since_ms` — a decaying
/// window (not a reset), so a polled preview stays alive but a single stale poll
/// can't pin it. Larger than the 5s status poll, smaller than the idle grace.
pub(crate) const PREVIEW_ACTIVITY_WINDOW_MS: u64 = 60_000;

/// How long a client-driven mutation RPC (file write, git commit, …) withholds
/// `idle_since_ms`. Same decaying-window semantics as preview activity: the
/// sandbox stays alive while mutations keep arriving and the normal idle grace
/// starts once they stop. `0` disables the withhold entirely (kill switch).
pub(crate) const RPC_ACTIVITY_WINDOW_MS: u64 = PREVIEW_ACTIVITY_WINDOW_MS;

/// How long a client-presence note (`workspace.presence.note`) withholds
/// `idle_since_ms`; `0` disables the withhold entirely (kill switch).
pub(crate) const PRESENCE_ACTIVITY_WINDOW_MS: u64 =
    echo_agent_workspace_types::rpc::presence::PRESENCE_ACTIVITY_WINDOW_MS;

/// Keep the sandbox awake while a live loop's next run is at most this far away. Set above the 12-hour sandbox
/// lifetime, so a live loop keeps the sandbox awake until the sandbox dies. `0` turns this off.
pub(crate) const SCHEDULED_TASK_KEEP_AWAKE_WINDOW_MS: u64 = 13 * 60 * 60 * 1000; // 13 hours

/// Ignore poll results older than this, so a dead poller cannot keep the sandbox awake.
const SCHEDULER_POLL_MAX_AGE_MS: u64 = 5 * 60 * 1000; // 5 minutes

/// One saved scheduler poll: the next run it reported and when the poll happened.
struct ScheduledPoll {
    next_fire_ms: u64,
    seen_at_ms: u64,
}

struct SessionActivity {
    active_tool_calls: AtomicU32,
    active_tools: DashMap<String, String>,
    last_call_started_ms: AtomicU64,
    last_call_completed_ms: AtomicU64,
    idle_since_ms: AtomicU64,
    /// Current turn number (set by `turn_started`).
    current_turn: AtomicU64,
    /// Whether a turn is currently active.
    turn_active: AtomicBool,
}

impl SessionActivity {
    fn new() -> Self {
        Self {
            active_tool_calls: AtomicU32::new(0),
            active_tools: DashMap::new(),
            last_call_started_ms: AtomicU64::new(0),
            last_call_completed_ms: AtomicU64::new(0),
            idle_since_ms: AtomicU64::new(now_ms()),
            current_turn: AtomicU64::new(0),
            turn_active: AtomicBool::new(false),
        }
    }
}

/// Tracks in-flight tool calls and background tasks for
/// [`ToolServerStatusPayload`] reporting.
///
/// All methods are `&self` — share via `Arc` across the tool handler, the
/// activity feed, and the status publisher.
pub struct ActivityTracker {
    active_tool_calls: AtomicU32,
    active_tools: DashMap<String, String>,
    background_tasks: AtomicU32,
    background_ids: DashMap<String, ()>,
    last_call_started_ms: AtomicU64,
    last_call_completed_ms: AtomicU64,
    idle_since_ms: AtomicU64,
    started_at: Instant,
    lifecycle: AtomicU8,
    /// `Arc` so the upload queue (via [`notify_handle`](Self::notify_handle))
    /// can wake the same waiter the status publisher blocks on.
    notify: Arc<tokio::sync::Notify>,
    /// Coupled upload-queue stats; unset for bare trackers (tests, queue-less mode).
    upload_queue_stats: OnceLock<Arc<UploadQueueStats>>,
    /// Epoch ms a graceful drain began; `0` means "not draining".
    drain_started_ms: AtomicU64,
    /// Coupled artifact-producer task tracker; unset for bare trackers.
    producer_tasks: OnceLock<tokio_util::task::TaskTracker>,
    /// Epoch ms the durability-busy condition started; `0` while clear. Stamped
    /// lazily at snapshot time.
    durability_busy_since_ms: AtomicU64,
    /// Cap (ms) on how long durability work may withhold `idle_since_ms`.
    durability_idle_hold_max_ms: u64,
    /// When set, the idle verdict ignores background tasks so `idle_since_ms`
    /// tracks foreground tool-call activity only. Drain/`status` stay bg-aware.
    idle_ignores_background: bool,
    /// Window (ms) recent preview-proxy traffic withholds idle for; defaults to
    /// [`PREVIEW_ACTIVITY_WINDOW_MS`], overridable via the builder.
    preview_activity_window_ms: u64,
    /// Epoch ms the pane's own status poll was last observed (`0` = none). Fed
    /// by the preview-activity scraper; withholds idle within
    /// [`preview_activity_window_ms`](Self::preview_activity_window_ms).
    ///
    /// The *observation* time, not the proxy's stamp: the two processes are not
    /// clock-coupled, and only the local clock is comparable with the rest.
    last_preview_status_ms: AtomicU64,
    /// Epoch ms real app traffic was last observed (`0` = none).
    last_preview_routed_ms: AtomicU64,
    /// Window (ms) a client mutation RPC withholds idle for; defaults to
    /// [`RPC_ACTIVITY_WINDOW_MS`], overridable via the builder. `0` disables.
    rpc_activity_window_ms: u64,
    /// Epoch ms a client-driven mutation RPC was last dispatched (`0` = none).
    /// Stamped by [`note_client_rpc_activity`](Self::note_client_rpc_activity);
    /// withholds idle within
    /// [`rpc_activity_window_ms`](Self::rpc_activity_window_ms).
    last_client_rpc_ms: AtomicU64,
    /// Window (ms) a client-presence note withholds idle for; `0` disables.
    presence_activity_window_ms: u64,
    /// See [`SCHEDULED_TASK_KEEP_AWAKE_WINDOW_MS`]; overridable via the builder. `0` turns it off.
    scheduled_task_keep_awake_window_ms: u64,
    /// The last scheduler poll that saw a live loop with a run coming; `None` = no hold.
    /// One lock for the pair, so a reader can never see a new run time with an old poll time.
    scheduled_poll: Mutex<Option<ScheduledPoll>>,
    /// Epoch ms a visible client-presence note was last received (`0` = none).
    last_presence_ms: AtomicU64,
    /// Highest presence-note `seq` applied (`0` = none). Guards against a
    /// slow superseded visible note landing after a newer hidden note and
    /// re-arming the withhold. A mutex (not an atomic) so the seq gate and
    /// the visible stamp commit as one decision under concurrent applies.
    last_presence_seq: Mutex<u64>,
    /// Open preview WebSocket (HMR) tunnels as of the last scrape. Nonzero ⇒ a
    /// client is attached, which no activity stamp would reveal.
    preview_ws_tunnels_open: AtomicU64,
    /// In-flight `Routed` preview requests as of the last scrape.
    preview_routed_in_flight: AtomicU64,
    /// Epoch ms this process started — the floor of the withhold anchor, so a
    /// young or freshly-restored workspace is never treated as long-idle.
    /// Distinct from [`Self::started_at`], a monotonic `Instant` that cannot be
    /// compared against the epoch stamps around it.
    started_at_ms: u64,

    sessions: DashMap<String, SessionActivity>,
    /// call_id → session_id so `tool_call_completed` can decrement
    /// the right session without the caller repeating it.
    call_to_session: DashMap<String, String>,
    /// Idle window (ms) after which an inactive session is pruned by
    /// [`known_sessions`]. Set once at construction; no locking.
    prune_window_ms: u64,
    /// Per-session `events.jsonl` writers, shared (`Arc`) with
    /// [`WorkspaceShared`](crate::session::WorkspaceShared). `None` until
    /// [`set_event_writers`](Self::set_event_writers) is called during
    /// `WorkspaceHandle` construction; bare trackers (the existing unit tests)
    /// leave it unset so no `Tool*` events are emitted — behaviour-preserving.
    event_writers: OnceLock<Arc<DashMap<String, EventWriter>>>,
    /// Per-call start timestamps (epoch ms), keyed by `call_id`. Populated only
    /// when the call's session has an OPEN `events.jsonl` writer — i.e. the same
    /// condition under which `ToolStarted` is emitted — so the flag-off /
    /// no-writer path inserts nothing (the per-session writer map is empty when
    /// `ECHO_AGENT_WORKSPACE_EVENTS_ENABLED` is off). Consumed by
    /// [`tool_call_completed`](Self::tool_call_completed) to report a truthful
    /// `ToolCompleted.duration_ms`.
    ///
    /// The stored value pairs the start timestamp with a clone of the session's
    /// `EventWriter` captured at `ToolStarted` time. Holding the writer handle
    /// here guarantees a symmetric `ToolStarted`/`ToolCompleted` pair: presence
    /// of this entry is the single source of truth that a `ToolStarted` was
    /// emitted, and the captured `Arc`-backed writer keeps the session's
    /// `events.jsonl` fd alive so the paired `ToolCompleted` is still written
    /// even if the per-session writer was evicted (e.g. `on_session_ended`)
    /// mid-call.
    call_started_ms: DashMap<String, (u64, EventWriter)>,
}

impl Default for ActivityTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ActivityTracker {
    /// Construct a tracker with the default 5-minute session-prune window.
    pub fn new() -> Self {
        Self::with_prune_window(std::time::Duration::from_millis(SESSION_IDLE_PRUNE_MS))
    }

    /// Construct a tracker with a custom session-prune window. The durability
    /// idle-hold cap comes from `ECHO_AGENT_WORKSPACE_DURABILITY_IDLE_HOLD_MAX_MS`
    /// (default [`DEFAULT_DURABILITY_IDLE_HOLD_MAX_MS`]).
    pub fn with_prune_window(prune_window: std::time::Duration) -> Self {
        Self::with_prune_window_and_idle_hold(prune_window, durability_idle_hold_max_from_env())
    }

    /// [`with_prune_window`](Self::with_prune_window) with an explicit
    /// durability idle-hold cap, so tests never race process env.
    pub fn with_prune_window_and_idle_hold(
        prune_window: std::time::Duration,
        durability_idle_hold_max_ms: u64,
    ) -> Self {
        Self {
            active_tool_calls: AtomicU32::new(0),
            active_tools: DashMap::new(),
            background_tasks: AtomicU32::new(0),
            background_ids: DashMap::new(),
            last_call_started_ms: AtomicU64::new(0),
            last_call_completed_ms: AtomicU64::new(0),
            idle_since_ms: AtomicU64::new(now_ms()),
            started_at: Instant::now(),
            lifecycle: AtomicU8::new(LIFECYCLE_NONE),
            notify: Arc::new(tokio::sync::Notify::new()),
            upload_queue_stats: OnceLock::new(),
            drain_started_ms: AtomicU64::new(0),
            producer_tasks: OnceLock::new(),
            durability_busy_since_ms: AtomicU64::new(0),
            durability_idle_hold_max_ms,
            idle_ignores_background: false,
            preview_activity_window_ms: PREVIEW_ACTIVITY_WINDOW_MS,
            last_preview_status_ms: AtomicU64::new(0),
            last_preview_routed_ms: AtomicU64::new(0),
            rpc_activity_window_ms: RPC_ACTIVITY_WINDOW_MS,
            last_client_rpc_ms: AtomicU64::new(0),
            presence_activity_window_ms: PRESENCE_ACTIVITY_WINDOW_MS,
            scheduled_task_keep_awake_window_ms: SCHEDULED_TASK_KEEP_AWAKE_WINDOW_MS,
            scheduled_poll: Mutex::new(None),
            last_presence_ms: AtomicU64::new(0),
            last_presence_seq: Mutex::new(0),
            preview_ws_tunnels_open: AtomicU64::new(0),
            preview_routed_in_flight: AtomicU64::new(0),
            started_at_ms: now_ms(),
            sessions: DashMap::new(),
            call_to_session: DashMap::new(),
            prune_window_ms: prune_window.as_millis() as u64,
            event_writers: OnceLock::new(),
            call_started_ms: DashMap::new(),
        }
    }

    /// Opt into foreground-only idle: background tasks stop withholding
    /// `idle_since_ms`.
    pub fn with_idle_ignores_background(mut self, enabled: bool) -> Self {
        self.idle_ignores_background = enabled;
        self
    }

    /// Override the preview-activity withhold window; the WorkspaceServer sources
    /// it from `StatusConfig`.
    pub fn with_preview_activity_window_ms(mut self, window_ms: u64) -> Self {
        self.preview_activity_window_ms = window_ms;
        self
    }

    /// Override the client-RPC withhold window (`0` disables); the
    /// WorkspaceServer sources it from `StatusConfig`.
    pub fn with_rpc_activity_window_ms(mut self, window_ms: u64) -> Self {
        self.rpc_activity_window_ms = window_ms;
        self
    }

    /// Override the client-presence withhold window (`0` disables); the
    /// WorkspaceServer sources it from `StatusConfig`.
    pub fn with_presence_activity_window_ms(mut self, window_ms: u64) -> Self {
        self.presence_activity_window_ms = window_ms;
        self
    }

    /// Override the keep-awake window (`0` turns it off); the WorkspaceServer sources it from `StatusConfig`.
    pub fn with_scheduled_task_keep_awake_window_ms(mut self, window_ms: u64) -> Self {
        self.scheduled_task_keep_awake_window_ms = window_ms;
        self
    }

    /// Save the scheduler poll result. `Some(ms)` = a live scheduler with its next run at `ms`; `None` = no live scheduler, or nothing left to run.
    pub fn record_scheduler_poll(&self, next_fire_ms: Option<u64>) {
        self.record_scheduler_poll_at(next_fire_ms, now_ms());
    }

    /// `record_scheduler_poll` with the poll time passed in, so tests can fake an old poll.
    fn record_scheduler_poll_at(&self, next_fire_ms: Option<u64>, seen_at_ms: u64) {
        let next = next_fire_ms.map(|next_fire_ms| ScheduledPoll {
            next_fire_ms,
            seen_at_ms,
        });

        let mut poll = self
            .scheduled_poll
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let changed =
            poll.as_ref().map(|p| p.next_fire_ms) != next.as_ref().map(|p| p.next_fire_ms);
        *poll = next;
        drop(poll);

        if changed {
            self.notify.notify_waiters();
        }
    }

    /// True when a recent poll saw a live loop with a run coming inside the window.
    fn scheduled_fire_withholds_idle(&self, now: u64) -> bool {
        if self.scheduled_task_keep_awake_window_ms == 0 {
            return false;
        }

        let poll = self
            .scheduled_poll
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(poll) = poll.as_ref() else {
            return false;
        };
        if now.saturating_sub(poll.seen_at_ms) > SCHEDULER_POLL_MAX_AGE_MS {
            return false;
        }

        now.saturating_add(self.scheduled_task_keep_awake_window_ms) >= poll.next_fire_ms
    }

    /// Wire the shared per-session `events.jsonl` writer map into the tracker so
    /// [`tool_call_started`](Self::tool_call_started) /
    /// [`tool_call_completed`](Self::tool_call_completed) can emit `Tool*`
    /// events. Set once during `WorkspaceHandle` construction; calling it a
    /// second time is a no-op (the first map wins).
    pub fn set_event_writers(&self, writers: Arc<DashMap<String, EventWriter>>) {
        let _ = self.event_writers.set(writers);
    }

    /// Couple upload-queue stats (status reports queue depth; `is_drained` waits
    /// for the queue). Set once; a second call is a no-op.
    pub fn set_upload_queue_stats(&self, stats: Arc<UploadQueueStats>) {
        let _ = self.upload_queue_stats.set(stats);
    }

    /// Couple the artifact-producer tracker (status counts producers, withholds
    /// idle while they run). Set once; a second call is a no-op.
    pub fn set_producer_tasks(&self, tasks: tokio_util::task::TaskTracker) {
        let _ = self.producer_tasks.set(tasks);
    }

    /// Clone of the internal `Notify` for the upload queue to drive republishes.
    pub fn notify_handle(&self) -> Arc<tokio::sync::Notify> {
        self.notify.clone()
    }

    /// Record fresh `Routed` preview traffic. Withholds `idle_since_ms` for
    /// [`preview_activity_window_ms`](Self::preview_activity_window_ms) and wakes
    /// the status publisher so the renewed "active" status reaches the server promptly.
    pub fn note_preview_routed_activity(&self) {
        self.last_preview_routed_ms
            .store(now_ms(), Ordering::Relaxed);
        self.notify.notify_waiters();
    }

    /// Record a fresh preview status poll. Withholds idle exactly as routed
    /// traffic does today, but is tracked separately: the poll continues at the
    /// same cadence whether or not anyone is watching.
    pub fn note_preview_status_activity(&self) {
        self.last_preview_status_ms
            .store(now_ms(), Ordering::Relaxed);
        self.notify.notify_waiters();
    }

    /// Record a client-driven mutation RPC (file write, git commit, …).
    /// Withholds `idle_since_ms` for
    /// [`rpc_activity_window_ms`](Self::rpc_activity_window_ms) and wakes the
    /// status publisher so the renewed "active" status reaches the server
    /// promptly. Read/poll RPCs deliberately never call this — an unattended
    /// tab polls but does not mutate, so it cannot pin its sandbox.
    pub fn note_client_rpc_activity(&self) {
        self.last_client_rpc_ms.store(now_ms(), Ordering::Relaxed);
        self.notify.notify_waiters();
    }

    /// Apply a presence note. Only a visible note stamps (a hide must never
    /// cut an existing withhold short), and a note whose `seq` is not newer
    /// than the last applied one is dropped — reordering protection for the
    /// gateway's fire-and-forget sends. `seq: None` (old gateway) always
    /// applies.
    pub fn apply_presence_note(&self, visible: bool, seq: Option<u64>) {
        // Gate and stamp under one lock: split across two atomics, a slow
        // older visible note racing a newer hidden one could pass the seq
        // check and stamp after the hidden note, re-arming the withhold.
        let mut last_seq = self
            .last_presence_seq
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(seq) = seq {
            if *last_seq >= seq {
                return;
            }
            *last_seq = seq;
        }
        if visible {
            self.last_presence_ms.store(now_ms(), Ordering::Relaxed);
            self.notify.notify_waiters();
        }
    }

    /// Mirror the proxy's attached-client counters. Absolute values, not edges,
    /// so a missed scrape self-corrects on the next one.
    pub fn set_preview_attached(&self, ws_tunnels_open: u64, routed_in_flight: u64) {
        let was_attached = self.has_preview_client_attached();
        self.preview_ws_tunnels_open
            .store(ws_tunnels_open, Ordering::Relaxed);
        self.preview_routed_in_flight
            .store(routed_in_flight, Ordering::Relaxed);
        // The scraper calls this every tick; the common case is 0 → 0.
        if was_attached != self.has_preview_client_attached() {
            self.notify.notify_waiters();
        }
    }

    /// An open WebSocket tunnel or a routed request in flight. Never a poll.
    fn has_preview_client_attached(&self) -> bool {
        self.preview_ws_tunnels_open.load(Ordering::Relaxed) > 0
            || self.preview_routed_in_flight.load(Ordering::Relaxed) > 0
    }

    pub fn preview_ws_tunnels_open(&self) -> u64 {
        self.preview_ws_tunnels_open.load(Ordering::Relaxed)
    }

    /// Window recent preview activity withholds idle for — the horizon the
    /// stamps decay over, and the one the scraper ages an absent proxy against.
    pub fn preview_activity_window_ms(&self) -> u64 {
        self.preview_activity_window_ms
    }

    /// Pending upload-queue items (0 when no queue is coupled).
    fn upload_queue_pending(&self) -> u64 {
        self.upload_queue_stats
            .get()
            .map(|s| s.pending.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// Queue/drain status fields shared by both snapshot paths; all zero when no
    /// queue is coupled. Best-effort: independent `Relaxed` loads can transiently
    /// show `inflight > pending` (cosmetic; the drain reads `pending` alone).
    fn drain_status_fields(&self) -> (u32, u64, u32, bool, Option<u64>) {
        let (pending, pending_bytes, inflight, breaker) = match self.upload_queue_stats.get() {
            Some(s) => (
                s.pending.load(Ordering::Relaxed) as u32,
                s.pending_bytes.load(Ordering::Relaxed),
                s.inflight.load(Ordering::Relaxed) as u32,
                s.circuit_breaker_active.load(Ordering::Relaxed),
            ),
            None => (0, 0, 0, false),
        };
        let drain_started = match self.drain_started_ms.load(Ordering::Relaxed) {
            0 => None,
            ms => Some(ms),
        };
        (pending, pending_bytes, inflight, breaker, drain_started)
    }

    /// Whether a drain has been started (`set_draining` ran) in this process.
    pub fn drain_started(&self) -> bool {
        self.drain_started_ms.load(Ordering::Relaxed) != 0
    }

    /// Durability tail shared by [`Self::snapshot`] and [`Self::snapshot_session`]
    /// (one construction site so the two payloads can't drift).
    fn durability_payload_fields(&self, idle_since: u64) -> DurabilityPayloadFields {
        let (queue_pending, queue_pending_bytes, queue_inflight, breaker, drain_started) =
            self.drain_status_fields();
        let (producers, durability_withhold) = self.durability_gate(queue_pending, breaker);
        let now = now_ms();
        let (preview_withhold, preview_reason, preview_anchor) = self.client_withholds_idle(now);
        // Withhold idle on durability work OR preview activity, decided here
        // once so both snapshot paths agree (preview has no hold cap; 12h VM TTL backstops).
        let withhold_idle = durability_withhold || preview_withhold;
        // Invariants: no reason while genuinely busy (`idle_since == 0` — the
        // work is the cause, not a concurrent poll); durability outranks
        // preview; every reason carries a stamp.
        let (withhold_reason, withhold_since_ms) = if idle_since == 0 {
            (None, None)
        } else if durability_withhold {
            (
                Some(IdleWithholdReason::Durability),
                Some(self.durability_busy_since_ms.load(Ordering::Relaxed)),
            )
        } else {
            (preview_reason, preview_reason.map(|_| preview_anchor))
        };
        DurabilityPayloadFields {
            idle_since_ms: if idle_since == 0 || withhold_idle {
                None
            } else {
                Some(idle_since)
            },
            withhold_reason,
            withhold_since_ms,
            preview_ws_tunnels_open: self.preview_ws_tunnels_open().min(u32::MAX as u64) as u32,
            upload_queue_pending: queue_pending,
            upload_queue_pending_bytes: queue_pending_bytes,
            upload_queue_inflight: queue_inflight,
            upload_queue_circuit_breaker_tripped: breaker,
            artifact_producers_inflight: producers,
            drain_started_ms: drain_started,
        }
    }

    /// Durability gate: returns (producers in flight, whether `idle_since_ms`
    /// must be withheld). Idle is withheld while producers or queued uploads are
    /// outstanding, except past the bounded hold cap, or — queued items only —
    /// when the circuit breaker is tripped (queued items survive via disk spill +
    /// restart recovery; an in-flight producer has nothing on disk yet, so it
    /// withholds regardless of queue health). The hold resets when it clears.
    fn durability_gate(&self, queue_pending: u32, breaker_tripped: bool) -> (u32, bool) {
        let producers = self
            .producer_tasks
            .get()
            .map(|t| t.len() as u32)
            .unwrap_or(0);
        let withholding = producers > 0 || (queue_pending > 0 && !breaker_tripped);
        if !withholding {
            self.durability_busy_since_ms.store(0, Ordering::Relaxed);
            return (producers, false);
        }
        let now = now_ms();
        // First observation of the busy condition wins the stamp. Relaxed:
        // the stamp guards no other data, it's just a monotonic-enough clock.
        let since = match self.durability_busy_since_ms.compare_exchange(
            0,
            now,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => now,
            Err(prev) => prev,
        };
        let hold_expired = now.saturating_sub(since) >= self.durability_idle_hold_max_ms;
        (producers, !hold_expired)
    }

    /// Whether client activity (preview traffic or mutation RPCs) should
    /// withhold idle, and on what grounds.
    ///
    /// Returns `(withhold, reason, anchor)`, where the anchor is the epoch-ms
    /// the current hold is measured from. Tiers are checked strongest first;
    /// all five withhold identically today, only the accounting differs.
    fn client_withholds_idle(&self, now: u64) -> (bool, Option<IdleWithholdReason>, u64) {
        // Including process start means a young or freshly-restored workspace
        // can never look long-idle — the process restarts on restore, so this
        // covers revived sessions without a separate minimum-age rule. A
        // mutation RPC is genuine use (unlike a status poll), so it advances
        // the anchor like routed preview traffic: a continuously-mutating
        // client is intentionally uncapped by the hub's hold ceiling (the VM
        // TTL backstops it); the ceiling bounds only a stale stamp.
        let anchor = self
            .last_preview_routed_ms
            .load(Ordering::Relaxed)
            .max(self.last_client_rpc_ms.load(Ordering::Relaxed))
            .max(self.last_call_completed_ms.load(Ordering::Relaxed))
            .max(self.started_at_ms);

        if self.has_preview_client_attached() {
            return (true, Some(IdleWithholdReason::PreviewAttached), anchor);
        }
        if activity_stamp_withholds_idle(
            now,
            self.last_preview_routed_ms.load(Ordering::Relaxed),
            self.preview_activity_window_ms,
        ) {
            return (true, Some(IdleWithholdReason::PreviewRouted), anchor);
        }
        if activity_stamp_withholds_idle(
            now,
            self.last_client_rpc_ms.load(Ordering::Relaxed),
            self.rpc_activity_window_ms,
        ) {
            return (true, Some(IdleWithholdReason::ClientRpc), anchor);
        }
        if activity_stamp_withholds_idle(
            now,
            self.last_preview_status_ms.load(Ordering::Relaxed),
            self.preview_activity_window_ms,
        ) {
            // Holds, but never advances the anchor: a poll must not reset a
            // clock meant to measure real use.
            return (true, Some(IdleWithholdReason::PreviewStatusOnly), anchor);
        }
        if activity_stamp_withholds_idle(
            now,
            self.last_presence_ms.load(Ordering::Relaxed),
            self.presence_activity_window_ms,
        ) {
            // Same anchor rule as the status poll: presence is not use, so
            // the hub's ceiling genuinely bounds an attached-but-idle client.
            return (true, Some(IdleWithholdReason::ClientPresence), anchor);
        }
        if self.scheduled_fire_withholds_idle(now) {
            // A waiting loop is not user activity, so the anchor stays put. The poll max age limits this hold.
            return (true, Some(IdleWithholdReason::ScheduledTask), anchor);
        }
        (false, None, anchor)
    }

    /// Whether any tracked session currently has an active turn (the aggregate
    /// `turn_active`).
    fn any_turn_active(&self) -> bool {
        self.sessions
            .iter()
            .any(|s| s.value().turn_active.load(Ordering::Acquire))
    }

    /// Resolve the `events.jsonl` writer for `session_id` — `Some` only when an
    /// event sink is configured AND the session already has an open writer
    /// (opened at turn start). Returns `None` (so no event is emitted) for the
    /// `__default__` / unknown-session cases.
    fn session_writer(&self, session_id: Option<&str>) -> Option<EventWriter> {
        let session_id = session_id?;
        let writers = self.event_writers.get()?;
        writers.get(session_id).map(|w| w.value().clone())
    }

    pub fn tool_call_started(&self, call_id: &str, tool_name: &str, session_id: Option<&str>) {
        if self.active_tools.contains_key(call_id) {
            return;
        }
        let now = now_ms();

        self.active_tool_calls.fetch_add(1, Ordering::Relaxed);
        self.active_tools
            .insert(call_id.to_owned(), tool_name.to_owned());
        self.last_call_started_ms.store(now, Ordering::Relaxed);
        self.idle_since_ms.store(0, Ordering::Relaxed);

        let sid = session_id.unwrap_or(DEFAULT_SESSION);
        self.call_to_session
            .insert(call_id.to_owned(), sid.to_owned());
        let session = self
            .sessions
            .entry(sid.to_owned())
            .or_insert_with(SessionActivity::new);
        session.active_tool_calls.fetch_add(1, Ordering::Relaxed);
        session
            .active_tools
            .insert(call_id.to_owned(), tool_name.to_owned());
        session.last_call_started_ms.store(now, Ordering::Relaxed);
        session.idle_since_ms.store(0, Ordering::Relaxed);

        self.notify.notify_waiters();

        // events.jsonl: only when the session's writer is already open (turn
        // started, under the events flag) do we record the start time (for a
        // truthful completion duration) and emit `ToolStarted`. When the writer
        // is absent — flag-off (empty per-session map) or no turn yet — this
        // whole block is skipped, so the flag-off path allocates nothing.
        if let Some(writer) = self.session_writer(session_id) {
            // Capture the writer handle alongside the start time so the paired
            // `ToolCompleted` is emitted to the same `events.jsonl` even if the
            // session writer is evicted mid-call.
            self.call_started_ms
                .insert(call_id.to_owned(), (now, writer.clone()));
            writer.emit(Event::ToolStarted {
                tool_name: tool_name.to_owned(),
            });
        }
    }

    /// Mark an in-flight tool call as completed.
    ///
    /// `session_id` identifies the owning session for the caller's bookkeeping;
    /// the internal session counters key off the recorded `call_id → session`
    /// mapping, and the [`ToolCompleted`](Event::ToolCompleted) event is written
    /// via the `EventWriter` handle captured at `ToolStarted` time (so it lands
    /// in the right `events.jsonl` even if the session writer was evicted
    /// mid-call). `outcome` is the truthful terminal status from the caller
    /// (`Success`/`Error` at the tool handler, `Cancelled` from the cancel
    /// paths).
    pub fn tool_call_completed(
        &self,
        call_id: &str,
        _session_id: Option<&str>,
        outcome: ToolOutcome,
    ) {
        let Some((_, tool_name)) = self.active_tools.remove(call_id) else {
            return;
        };
        let now = now_ms();

        self.active_tool_calls.fetch_sub(1, Ordering::AcqRel);
        self.last_call_completed_ms.store(now, Ordering::Relaxed);
        // Re-read after decrement: a concurrent `tool_call_started` may
        // have bumped the counter back up between our `fetch_sub` and
        // this load. Only transition to idle if we're truly at zero.
        if self.active_tool_calls.load(Ordering::Acquire) == 0
            && (self.idle_ignores_background || self.background_tasks.load(Ordering::Acquire) == 0)
        {
            self.idle_since_ms.store(now, Ordering::Relaxed);
        }

        if let Some((_, sid)) = self.call_to_session.remove(call_id)
            && let Some(session) = self.sessions.get(&sid)
        {
            session.active_tool_calls.fetch_sub(1, Ordering::AcqRel);
            session.active_tools.remove(call_id);
            session.last_call_completed_ms.store(now, Ordering::Relaxed);
            if session.active_tool_calls.load(Ordering::Acquire) == 0 {
                session.idle_since_ms.store(now, Ordering::Relaxed);
            }
        }

        self.notify.notify_waiters();

        // events.jsonl: emit `ToolCompleted` only when a paired `ToolStarted`
        // was recorded for this call (its `call_started_ms` entry is present).
        // Gating on that entry — rather than re-checking writer state at
        // completion — keeps the start/completion pair symmetric: no orphan
        // zero-duration `ToolCompleted` when a writer opens mid-call, and no
        // dropped completion when the session writer is evicted mid-call (the
        // captured writer handle keeps the file open). `duration_ms` is always
        // truthful because the start time is paired with the writer.
        if let Some((_, (started_ms, writer))) = self.call_started_ms.remove(call_id) {
            // Workspace = hub/proxy hop; join package durations on shell rows (no source).
            writer.emit(Event::ToolCompleted {
                tool_name,
                duration_ms: now.saturating_sub(started_ms),
                outcome,
                tool_call_id: call_id.to_owned(),
                source: ToolCompletedSource::Workspace,
            });
        }
    }

    pub fn background_task_started(&self, task_id: &str) {
        if self.background_ids.contains_key(task_id) {
            return;
        }
        self.background_tasks.fetch_add(1, Ordering::Relaxed);
        self.background_ids.insert(task_id.to_owned(), ());
        if self.idle_ignores_background {
            // An active fg call's store(0) must keep idle withheld.
            if self.active_tool_calls.load(Ordering::Acquire) == 0 {
                self.idle_since_ms.store(now_ms(), Ordering::Relaxed);
            }
        } else {
            self.idle_since_ms.store(0, Ordering::Relaxed);
        }
        self.notify.notify_waiters();
    }

    pub fn background_task_completed(&self, task_id: &str) {
        if self.background_ids.remove(task_id).is_none() {
            return;
        }
        let prev = self.background_tasks.fetch_sub(1, Ordering::AcqRel);
        if !self.idle_ignores_background
            && prev == 1
            && self.active_tool_calls.load(Ordering::Acquire) == 0
        {
            self.idle_since_ms.store(now_ms(), Ordering::Relaxed);
        }
        self.notify.notify_waiters();
    }

    pub fn turn_started(&self, session_id: &str, turn_number: u64) {
        let session = self
            .sessions
            .entry(session_id.to_owned())
            .or_insert_with(SessionActivity::new);
        session.current_turn.store(turn_number, Ordering::Release);
        session.turn_active.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    pub fn turn_completed(&self, session_id: &str, turn_number: u64, _duration_ms: u64) {
        if let Some(session) = self.sessions.get(session_id)
            && session.current_turn.load(Ordering::Acquire) == turn_number
        {
            session.turn_active.store(false, Ordering::Release);
            self.notify.notify_waiters();
        }
    }

    /// Complete all in-flight tool calls for the given session.
    ///
    /// Returns the number of calls that were marked as completed.
    /// Called when a session-wide Cancel hook arrives without a specific
    /// `call_id` (broadcast cancel from Ctrl+C).
    pub fn cancel_all_session_calls(&self, session_id: &str) -> usize {
        let call_ids: Vec<String> = self
            .call_to_session
            .iter()
            .filter(|entry| entry.value() == session_id)
            .map(|entry| entry.key().clone())
            .collect();
        let count = call_ids.len();
        for call_id in call_ids {
            self.tool_call_completed(&call_id, Some(session_id), ToolOutcome::Cancelled);
        }
        count
    }

    /// Releases the session's entry. One with calls in flight keeps it: the
    /// swap policy reads that count, and the idle prune collects it later.
    pub fn session_ended(&self, session_id: &str) {
        let released = self
            .sessions
            .remove_if(session_id, |_, s| {
                s.active_tool_calls.load(Ordering::Acquire) == 0
            })
            .is_some();
        if !released && let Some(session) = self.sessions.get(session_id) {
            session.turn_active.store(false, Ordering::Release);
        }
        self.notify.notify_waiters();
    }

    /// Whether a turn is currently active for the given session.
    pub fn is_turn_active(&self, session_id: &str) -> bool {
        self.sessions
            .get(session_id)
            .is_some_and(|s| s.turn_active.load(Ordering::Acquire))
    }

    /// In-flight tool calls for the given session (`0` when unknown). Only
    /// the model-facing tool handler ticks the underlying counter, so
    /// `workspace_rpc` traffic never contributes.
    pub fn session_active_tool_calls(&self, session_id: &str) -> u32 {
        self.sessions
            .get(session_id)
            .map_or(0, |s| s.active_tool_calls.load(Ordering::Acquire))
    }

    pub fn set_active(&self) {
        self.lifecycle.store(LIFECYCLE_NONE, Ordering::Release);
        // Clear the drain stamp symmetrically with `set_draining`: leaving it set
        // after a resume would make `drain_started_ms` mean "a drain ever began"
        // rather than "currently draining", and the server's idle gate keys its
        // unconditional drain escape off that stamp (a stale stamp would let it
        // report idle while durable work is still outstanding).
        self.drain_started_ms.store(0, Ordering::Release);
        self.notify.notify_waiters();
    }

    pub fn set_draining(&self) {
        self.lifecycle.store(LIFECYCLE_DRAINING, Ordering::Release);
        // First transition wins, so `drain_started_ms` is stable across calls.
        let _ = self.drain_started_ms.compare_exchange(
            0,
            now_ms(),
            Ordering::AcqRel,
            Ordering::Relaxed,
        );
        self.notify.notify_waiters();
    }

    pub fn set_shutting_down(&self) {
        self.lifecycle
            .store(LIFECYCLE_SHUTTING_DOWN, Ordering::Release);
        self.notify.notify_waiters();
    }

    pub fn is_draining(&self) -> bool {
        self.lifecycle.load(Ordering::Acquire) >= LIFECYCLE_DRAINING
    }

    /// Fully drained: draining, no active tool calls/background tasks, and the
    /// upload queue emptied (must not exit with artifacts still pending).
    ///
    /// In-flight artifact producers are intentionally *not* part of this gate:
    /// they are withheld from idle by [`Self::durability_gate`] and are awaited
    /// by the graceful drain's producer phase (`phase 1.5` of
    /// `WorkspaceHandle::two_phase_drain`)
    /// *before* the queue flush, so by the time the queue empties their work has
    /// already been enqueued and is reflected in `upload_queue_pending`.
    pub fn is_drained(&self) -> bool {
        self.is_draining() && self.total_active() == 0 && self.upload_queue_pending() == 0
    }

    /// Phase-1 drain condition: all in-flight tool calls and background tasks
    /// finished, independent of the upload queue.
    pub fn tools_idle(&self) -> bool {
        self.total_active() == 0
    }

    pub fn total_active(&self) -> u32 {
        self.active_tool_calls.load(Ordering::Relaxed)
            + self.background_tasks.load(Ordering::Relaxed)
    }

    pub async fn wait_for_change(&self, timeout: std::time::Duration) {
        let _ = tokio::time::timeout(timeout, self.notify.notified()).await;
    }

    /// Wake the status publisher so it sends a heartbeat immediately.
    pub fn poke(&self) {
        self.notify.notify_waiters();
    }

    /// Wait until the tracker is both draining and all active work
    /// (tool calls + background tasks + the upload queue) has completed.
    pub async fn wait_until_drained(&self) {
        loop {
            if self.is_drained() {
                return;
            }
            // `notify_waiters` stores no permit, so a wake between the check and
            // this await is missed — safe because every caller is timeout-bounded.
            self.notify.notified().await;
        }
    }

    /// Wait until all in-flight tool calls and background tasks have finished,
    /// ignoring the upload queue (phase 1 of the two-phase drain).
    pub async fn wait_until_tools_idle(&self) {
        loop {
            if self.tools_idle() {
                return;
            }
            // Same timeout-bounded missed-wakeup tolerance as `wait_until_drained`.
            self.notify.notified().await;
        }
    }

    /// Resident session records. Does not prune, unlike [`Self::known_sessions`].
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Returns live session IDs. As a side-effect, prunes sessions
    /// that have been idle longer than the configured prune window.
    pub fn known_sessions(&self) -> Vec<String> {
        let now = now_ms();
        let mut live = Vec::new();
        let mut stale = Vec::new();

        for entry in self.sessions.iter() {
            let key = entry.key();
            if key == DEFAULT_SESSION {
                continue;
            }
            if entry.value().active_tool_calls.load(Ordering::Relaxed) > 0 {
                live.push(key.clone());
                continue;
            }
            let idle_since = entry.value().idle_since_ms.load(Ordering::Relaxed);
            if idle_since > 0 && now.saturating_sub(idle_since) > self.prune_window_ms {
                stale.push(key.clone());
            } else {
                live.push(key.clone());
            }
        }

        for key in &stale {
            self.sessions.remove(key);
            self.call_to_session.retain(|_, sid| sid != key);
        }

        live
    }

    /// Per-session snapshot. `background_task_ids` is the connection aggregate,
    /// re-published to the session's client.
    pub fn snapshot_session(&self, session_id: &str) -> ToolServerStatusPayload {
        let lifecycle = self.lifecycle.load(Ordering::Acquire);
        let bg = self.background_tasks.load(Ordering::Relaxed);

        let (active, active_tool_names, last_started, last_completed, idle_since, turn_active) =
            if let Some(session) = self.sessions.get(session_id) {
                let a = session.active_tool_calls.load(Ordering::Relaxed);
                let names: Vec<String> = session
                    .active_tools
                    .iter()
                    .map(|r| r.value().clone())
                    .collect();
                let started = session.last_call_started_ms.load(Ordering::Relaxed);
                let completed = session.last_call_completed_ms.load(Ordering::Relaxed);
                let idle = session.idle_since_ms.load(Ordering::Relaxed);
                let turn = session.turn_active.load(Ordering::Acquire);
                (a, names, started, completed, idle, turn)
            } else {
                (0, vec![], 0, 0, now_ms(), false)
            };

        let status = match lifecycle {
            LIFECYCLE_SHUTTING_DOWN => ToolServerLifecycleStatus::ShuttingDown,
            LIFECYCLE_DRAINING => ToolServerLifecycleStatus::Draining,
            _ if active > 0 => ToolServerLifecycleStatus::Busy,
            _ => ToolServerLifecycleStatus::Ready,
        };

        let background_task_ids: Vec<String> = self
            .background_ids
            .iter()
            .map(|r| r.key().clone())
            .collect();

        let d = self.durability_payload_fields(idle_since);

        ToolServerStatusPayload {
            status,
            session_id: echo_agent_tool_protocol::SessionId::new(session_id).ok(),
            connection_id: None,
            active_tool_calls: active,
            active_tool_names,
            background_tasks: bg,
            background_task_ids,
            pending_tool_calls: 0,
            last_tool_call_started_ms: last_started,
            last_tool_call_completed_ms: last_completed,
            uptime_ms: self.started_at.elapsed().as_millis() as u64,
            idle_since_ms: d.idle_since_ms,
            upload_queue_pending: d.upload_queue_pending,
            upload_queue_pending_bytes: d.upload_queue_pending_bytes,
            upload_queue_inflight: d.upload_queue_inflight,
            upload_queue_circuit_breaker_tripped: d.upload_queue_circuit_breaker_tripped,
            artifact_producers_inflight: d.artifact_producers_inflight,
            drain_started_ms: d.drain_started_ms,
            turn_active,
            idle_ignores_background: self.idle_ignores_background,
            withhold_reason: d.withhold_reason,
            withhold_since_ms: d.withhold_since_ms,
            // No ceilings configured yet, so a hold can never be capped.
            withhold_capped: false,
            preview_ws_tunnels_open: d.preview_ws_tunnels_open,
        }
    }

    /// Aggregate snapshot across all sessions.
    pub fn snapshot(&self) -> ToolServerStatusPayload {
        let lifecycle = self.lifecycle.load(Ordering::Acquire);
        let active = self.active_tool_calls.load(Ordering::Relaxed);
        let bg = self.background_tasks.load(Ordering::Relaxed);

        let status = match lifecycle {
            LIFECYCLE_SHUTTING_DOWN => ToolServerLifecycleStatus::ShuttingDown,
            LIFECYCLE_DRAINING => ToolServerLifecycleStatus::Draining,
            _ if active + bg > 0 => ToolServerLifecycleStatus::Busy,
            _ => ToolServerLifecycleStatus::Ready,
        };

        let active_tool_names: Vec<String> = self
            .active_tools
            .iter()
            .map(|r| r.value().clone())
            .collect();
        let background_task_ids: Vec<String> = self
            .background_ids
            .iter()
            .map(|r| r.key().clone())
            .collect();

        let idle_since = self.idle_since_ms.load(Ordering::Relaxed);

        let d = self.durability_payload_fields(idle_since);

        ToolServerStatusPayload {
            status,
            session_id: None,
            connection_id: None,
            active_tool_calls: active,
            active_tool_names,
            background_tasks: bg,
            background_task_ids,
            pending_tool_calls: 0,
            last_tool_call_started_ms: self.last_call_started_ms.load(Ordering::Relaxed),
            last_tool_call_completed_ms: self.last_call_completed_ms.load(Ordering::Relaxed),
            uptime_ms: self.started_at.elapsed().as_millis() as u64,
            idle_since_ms: d.idle_since_ms,
            upload_queue_pending: d.upload_queue_pending,
            upload_queue_pending_bytes: d.upload_queue_pending_bytes,
            upload_queue_inflight: d.upload_queue_inflight,
            upload_queue_circuit_breaker_tripped: d.upload_queue_circuit_breaker_tripped,
            artifact_producers_inflight: d.artifact_producers_inflight,
            drain_started_ms: d.drain_started_ms,
            turn_active: self.any_turn_active(),
            idle_ignores_background: self.idle_ignores_background,
            withhold_reason: d.withhold_reason,
            withhold_since_ms: d.withhold_since_ms,
            // No ceilings configured yet, so a hold can never be capped.
            withhold_capped: false,
            preview_ws_tunnels_open: d.preview_ws_tunnels_open,
        }
    }
}

/// Durability tail of a [`ToolServerStatusPayload`], built only by
/// [`ActivityTracker::durability_payload_fields`].
struct DurabilityPayloadFields {
    idle_since_ms: Option<u64>,
    withhold_reason: Option<IdleWithholdReason>,
    withhold_since_ms: Option<u64>,
    preview_ws_tunnels_open: u32,
    upload_queue_pending: u32,
    upload_queue_pending_bytes: u64,
    upload_queue_inflight: u32,
    upload_queue_circuit_breaker_tripped: bool,
    artifact_producers_inflight: u32,
    drain_started_ms: Option<u64>,
}

/// The durability idle-hold cap from `ECHO_AGENT_WORKSPACE_DURABILITY_IDLE_HOLD_MAX_MS`.
fn durability_idle_hold_max_from_env() -> u64 {
    durability_idle_hold_from_raw(
        std::env::var("ECHO_AGENT_WORKSPACE_DURABILITY_IDLE_HOLD_MAX_MS").ok(),
    )
}

/// Pure parse of the idle-hold env value: a non-negative integer ms wins (0
/// disables the hold entirely); absent or malformed falls back to the default.
fn durability_idle_hold_from_raw(raw: Option<String>) -> u64 {
    raw.and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_DURABILITY_IDLE_HOLD_MAX_MS)
}

/// Whether a preview-activity stamp still withholds idle at `now`: true while it
/// is within `window` ms. A zero stamp (no activity recorded) never withholds,
/// and the window is exclusive at the boundary so it decays rather than pins.
fn activity_stamp_withholds_idle(now: u64, last_activity_ms: u64, window_ms: u64) -> bool {
    last_activity_ms != 0 && now.saturating_sub(last_activity_ms) < window_ms
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_ready() {
        let t = ActivityTracker::new();
        let s = t.snapshot();
        assert_eq!(s.status, ToolServerLifecycleStatus::Ready);
        assert_eq!(s.active_tool_calls, 0);
        assert!(s.idle_since_ms.is_some());
        assert!(s.session_id.is_none());
    }

    #[test]
    fn tool_call_transitions_to_busy() {
        let t = ActivityTracker::new();
        t.tool_call_started("c1", "read_file", Some("sess-a"));
        let s = t.snapshot();
        assert_eq!(s.status, ToolServerLifecycleStatus::Busy);
        assert_eq!(s.active_tool_calls, 1);
        assert_eq!(s.active_tool_names, vec!["read_file"]);
        assert!(s.idle_since_ms.is_none());
    }

    #[test]
    fn tool_call_completion_returns_to_ready() {
        let t = ActivityTracker::new();
        t.tool_call_started("c1", "read_file", Some("sess-a"));
        t.tool_call_completed("c1", None, ToolOutcome::Success);
        let s = t.snapshot();
        assert_eq!(s.status, ToolServerLifecycleStatus::Ready);
        assert_eq!(s.active_tool_calls, 0);
        assert!(s.idle_since_ms.is_some());
    }

    #[test]
    fn background_task_makes_busy() {
        let t = ActivityTracker::new();
        t.background_task_started("t1");
        let s = t.snapshot();
        assert_eq!(s.status, ToolServerLifecycleStatus::Busy);
        assert_eq!(s.background_tasks, 1);
    }

    #[test]
    fn background_task_started_dedups_by_id() {
        let t = ActivityTracker::new();
        t.background_task_started("dup");
        t.background_task_started("dup");
        assert_eq!(t.snapshot().background_tasks, 1);
    }

    #[test]
    fn background_task_completed_unknown_id_does_not_underflow() {
        let t = ActivityTracker::new();
        t.background_task_completed("never-started");
        assert_eq!(t.snapshot().background_tasks, 0);
        t.background_task_started("real");
        assert_eq!(t.snapshot().background_tasks, 1);
    }

    #[test]
    fn background_task_decrement_restores_idle_only_at_zero() {
        let t = ActivityTracker::new();
        t.background_task_started("a");
        t.background_task_started("b");
        assert!(t.snapshot().idle_since_ms.is_none());
        t.background_task_completed("a");
        assert!(
            t.snapshot().idle_since_ms.is_none(),
            "one bg task left → still not idle"
        );
        t.background_task_completed("b");
        assert!(
            t.snapshot().idle_since_ms.is_some(),
            "idle restored only when the last bg task completes"
        );
    }

    #[test]
    fn background_after_calls_complete_pins_idle_only_when_flag_off() {
        let on = ActivityTracker::new().with_idle_ignores_background(true);
        on.tool_call_started("c1", "read_file", None);
        on.tool_call_completed("c1", None, ToolOutcome::Success);
        on.background_task_started("bg1");
        assert!(on.snapshot().idle_since_ms.is_some());

        let off = ActivityTracker::new();
        off.tool_call_started("c1", "read_file", None);
        off.tool_call_completed("c1", None, ToolOutcome::Success);
        off.background_task_started("bg1");
        assert!(off.snapshot().idle_since_ms.is_none());
    }

    #[test]
    fn flag_on_active_call_withholds_idle_until_it_completes_despite_background() {
        let t = ActivityTracker::new().with_idle_ignores_background(true);
        t.tool_call_started("c1", "read_file", None);
        t.background_task_started("bg1");
        assert!(t.snapshot().idle_since_ms.is_none());
        t.tool_call_completed("c1", None, ToolOutcome::Success);
        assert!(t.snapshot().idle_since_ms.is_some());
    }

    #[test]
    fn flag_on_keeps_busy_status_while_reporting_idle() {
        let t = ActivityTracker::new().with_idle_ignores_background(true);
        t.background_task_started("bg1");
        let s = t.snapshot();
        assert_eq!(s.status, ToolServerLifecycleStatus::Busy);
        assert!(s.idle_since_ms.is_some());
    }

    #[test]
    fn flag_on_background_completion_does_not_advance_idle() {
        let t = ActivityTracker::new().with_idle_ignores_background(true);
        t.background_task_started("bg1");
        let after_start = t.snapshot().idle_since_ms;
        std::thread::sleep(std::time::Duration::from_millis(5));
        t.background_task_completed("bg1");
        assert_eq!(t.snapshot().idle_since_ms, after_start);
    }

    #[test]
    fn flag_on_background_start_advances_idle_when_no_active_call() {
        let t = ActivityTracker::new().with_idle_ignores_background(true);
        t.tool_call_started("c1", "read_file", None);
        t.tool_call_completed("c1", None, ToolOutcome::Success);
        let before = t.snapshot().idle_since_ms.unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        t.background_task_started("bg1");
        let after = t.snapshot().idle_since_ms.unwrap();
        assert!(after > before);
    }

    #[test]
    fn drain_counts_background_tasks_regardless_of_flag() {
        for flag in [false, true] {
            let t = ActivityTracker::new().with_idle_ignores_background(flag);
            t.background_task_started("bg1");
            assert_eq!(t.total_active(), 1);
            t.set_draining();
            assert!(!t.is_drained());
            assert!(!t.tools_idle());
            t.background_task_completed("bg1");
            assert!(t.is_drained());
            assert!(t.tools_idle());
        }
    }

    #[test]
    fn snapshot_payloads_report_idle_ignores_background_flag() {
        let on = ActivityTracker::new().with_idle_ignores_background(true);
        assert!(on.snapshot().idle_ignores_background);
        assert!(on.snapshot_session("sess-a").idle_ignores_background);

        let off = ActivityTracker::new();
        assert!(!off.snapshot().idle_ignores_background);
        assert!(!off.snapshot_session("sess-a").idle_ignores_background);
    }

    #[test]
    fn draining_overrides_busy() {
        let t = ActivityTracker::new();
        t.tool_call_started("c1", "grep", None);
        t.set_draining();
        assert_eq!(t.snapshot().status, ToolServerLifecycleStatus::Draining);
    }

    #[test]
    fn set_active_clears_draining() {
        let t = ActivityTracker::new();
        t.set_draining();
        assert_eq!(t.snapshot().status, ToolServerLifecycleStatus::Draining);
        t.set_active();
        assert_eq!(t.snapshot().status, ToolServerLifecycleStatus::Ready);
        assert!(!t.is_draining());
        t.tool_call_started("c1", "read_file", Some("sess-a"));
        assert_eq!(t.snapshot().status, ToolServerLifecycleStatus::Busy);
    }

    #[test]
    fn shutting_down_overrides_draining() {
        let t = ActivityTracker::new();
        t.set_draining();
        t.set_shutting_down();
        assert_eq!(t.snapshot().status, ToolServerLifecycleStatus::ShuttingDown);
    }

    #[test]
    fn is_drained_requires_zero_active() {
        let t = ActivityTracker::new();
        t.tool_call_started("c1", "x", None);
        t.set_draining();
        assert!(!t.is_drained());
        t.tool_call_completed("c1", None, ToolOutcome::Success);
        assert!(t.is_drained());
    }

    // ── upload-queue coupling + drain status fields ───────────────

    /// Coupled queue depth surfaces in both aggregate and per-session payloads.
    #[test]
    fn snapshot_reports_coupled_upload_queue_stats() {
        let t = ActivityTracker::new();
        let stats = Arc::new(UploadQueueStats::new());
        stats.pending.store(3, Ordering::Relaxed);
        stats.pending_bytes.store(9000, Ordering::Relaxed);
        stats.inflight.store(1, Ordering::Relaxed);
        stats.circuit_breaker_active.store(true, Ordering::Relaxed);
        t.set_upload_queue_stats(stats);

        let agg = t.snapshot();
        assert_eq!(agg.upload_queue_pending, 3);
        assert_eq!(agg.upload_queue_pending_bytes, 9000);
        assert_eq!(agg.upload_queue_inflight, 1);
        assert!(agg.upload_queue_circuit_breaker_tripped);

        t.tool_call_started("c1", "x", Some("sess-a"));
        let sess = t.snapshot_session("sess-a");
        assert_eq!(sess.upload_queue_pending, 3);
        assert_eq!(sess.upload_queue_inflight, 1);
        assert!(sess.upload_queue_circuit_breaker_tripped);
    }

    /// The queue-less path reports zeroed queue fields (legacy behaviour).
    #[test]
    fn snapshot_queue_fields_zero_without_coupled_queue() {
        let t = ActivityTracker::new();
        let s = t.snapshot();
        assert_eq!(s.upload_queue_pending, 0);
        assert_eq!(s.upload_queue_pending_bytes, 0);
        assert_eq!(s.upload_queue_inflight, 0);
        assert!(!s.upload_queue_circuit_breaker_tripped);
        assert_eq!(s.drain_started_ms, None);
    }

    /// Draining with no tool calls but a non-empty queue is NOT drained.
    #[test]
    fn is_drained_requires_empty_upload_queue() {
        let t = ActivityTracker::new();
        let stats = Arc::new(UploadQueueStats::new());
        stats.pending.store(2, Ordering::Relaxed);
        t.set_upload_queue_stats(stats.clone());

        t.set_draining();
        assert!(
            !t.is_drained(),
            "queue still has 2 pending → not drained even with no tool calls"
        );

        stats.pending.store(0, Ordering::Relaxed);
        assert!(t.is_drained(), "queue emptied → now fully drained");
    }

    /// The phase-1 drain condition must not wait on the queue.
    #[test]
    fn tools_idle_ignores_upload_queue() {
        let t = ActivityTracker::new();
        let stats = Arc::new(UploadQueueStats::new());
        stats.pending.store(5, Ordering::Relaxed);
        t.set_upload_queue_stats(stats);
        assert!(
            t.tools_idle(),
            "no tool calls → tools idle regardless of queue"
        );
        assert!(
            !t.is_drained(),
            "but not fully drained while queue is non-empty"
        );
    }

    // ── durability-aware idle gating ───────────────────────────────

    #[tokio::test]
    async fn idle_withheld_while_producer_in_flight() {
        let t = ActivityTracker::new();
        let tasks = tokio_util::task::TaskTracker::new();
        t.set_producer_tasks(tasks.clone());
        let s = t.snapshot();
        assert_eq!(s.artifact_producers_inflight, 0);
        assert!(s.idle_since_ms.is_some(), "no producers → idle reported");

        let gate = Arc::new(tokio::sync::Notify::new());
        let gate2 = gate.clone();
        let join = tasks.spawn(async move { gate2.notified().await });

        let s = t.snapshot();
        assert_eq!(s.artifact_producers_inflight, 1);
        assert!(
            s.idle_since_ms.is_none(),
            "an in-flight producer must withhold idle"
        );
        assert!(t.snapshot_session("any").idle_since_ms.is_none());

        gate.notify_one();
        join.await.expect("producer task must not panic");

        let s = t.snapshot();
        assert_eq!(s.artifact_producers_inflight, 0);
        assert!(
            s.idle_since_ms.is_some(),
            "idle must be restored once the producer completes"
        );
    }

    #[test]
    fn idle_withheld_while_upload_queue_pending() {
        let t = ActivityTracker::new();
        let stats = Arc::new(UploadQueueStats::new());
        stats.pending.store(1, Ordering::Relaxed);
        t.set_upload_queue_stats(stats.clone());

        assert!(
            t.snapshot().idle_since_ms.is_none(),
            "pending uploads must withhold idle"
        );

        stats.pending.store(0, Ordering::Relaxed);
        assert!(
            t.snapshot().idle_since_ms.is_some(),
            "idle must be restored once the queue empties"
        );
    }

    #[test]
    fn durability_hold_cap_expiry_allows_idle() {
        let t = ActivityTracker::with_prune_window_and_idle_hold(
            std::time::Duration::from_millis(SESSION_IDLE_PRUNE_MS),
            1_000,
        );
        let stats = Arc::new(UploadQueueStats::new());
        stats.pending.store(1, Ordering::Relaxed);
        t.set_upload_queue_stats(stats);

        assert!(t.snapshot().idle_since_ms.is_none(), "within the hold cap");

        // Backdate the busy stamp past the cap.
        t.durability_busy_since_ms
            .store(now_ms() - 2_000, Ordering::Relaxed);
        let s = t.snapshot();
        assert!(
            s.idle_since_ms.is_some(),
            "expired hold cap must allow idle despite pending work"
        );
        assert_eq!(
            s.upload_queue_pending, 1,
            "the pending depth stays truthfully reported"
        );
    }

    #[test]
    fn durability_busy_stamp_resets_when_condition_clears() {
        let t = ActivityTracker::with_prune_window_and_idle_hold(
            std::time::Duration::from_millis(SESSION_IDLE_PRUNE_MS),
            1_000,
        );
        let stats = Arc::new(UploadQueueStats::new());
        stats.pending.store(1, Ordering::Relaxed);
        t.set_upload_queue_stats(stats.clone());
        let _ = t.snapshot(); // stamps the busy start
        t.durability_busy_since_ms
            .store(now_ms() - 600, Ordering::Relaxed);

        stats.pending.store(0, Ordering::Relaxed);
        let _ = t.snapshot();
        assert_eq!(
            t.durability_busy_since_ms.load(Ordering::Relaxed),
            0,
            "a clear condition must reset the stamp"
        );

        // A new busy condition measures its hold from a fresh stamp.
        stats.pending.store(1, Ordering::Relaxed);
        assert!(t.snapshot().idle_since_ms.is_none());
    }

    #[test]
    fn breaker_tripped_allows_idle_despite_pending_work() {
        let t = ActivityTracker::new();
        let stats = Arc::new(UploadQueueStats::new());
        stats.pending.store(3, Ordering::Relaxed);
        stats.circuit_breaker_active.store(true, Ordering::Relaxed);
        t.set_upload_queue_stats(stats);

        let s = t.snapshot();
        assert!(
            s.idle_since_ms.is_some(),
            "a tripped breaker must allow hibernation despite pending work"
        );
        assert!(s.upload_queue_circuit_breaker_tripped);
    }

    /// The breaker escape covers queued items only (they survive via disk
    /// spill + restart recovery); an in-flight producer has nothing on disk
    /// yet and must keep withholding idle even with the breaker tripped.
    #[tokio::test]
    async fn breaker_tripped_does_not_bypass_producer_hold() {
        let t = ActivityTracker::new();
        let stats = Arc::new(UploadQueueStats::new());
        stats.circuit_breaker_active.store(true, Ordering::Relaxed);
        t.set_upload_queue_stats(stats);
        let tasks = tokio_util::task::TaskTracker::new();
        t.set_producer_tasks(tasks.clone());

        let gate = Arc::new(tokio::sync::Notify::new());
        let gate2 = gate.clone();
        let join = tasks.spawn(async move { gate2.notified().await });

        assert!(
            t.snapshot().idle_since_ms.is_none(),
            "a producer must withhold idle even with the breaker tripped"
        );

        gate.notify_one();
        join.await.expect("producer task must not panic");
        assert!(
            t.snapshot().idle_since_ms.is_some(),
            "idle restored once the producer completes (breaker alone is no hold)"
        );
    }

    #[test]
    fn preview_activity_withholds_idle_window_boundaries() {
        let now = 10_000_000;
        assert!(
            !activity_stamp_withholds_idle(now, 0, PREVIEW_ACTIVITY_WINDOW_MS),
            "a zero stamp (no activity ever) must never withhold"
        );
        assert!(
            activity_stamp_withholds_idle(now, now, PREVIEW_ACTIVITY_WINDOW_MS),
            "activity right now withholds"
        );
        assert!(
            activity_stamp_withholds_idle(
                now,
                now - (PREVIEW_ACTIVITY_WINDOW_MS - 1),
                PREVIEW_ACTIVITY_WINDOW_MS
            ),
            "just inside the window withholds"
        );
        assert!(
            !activity_stamp_withholds_idle(
                now,
                now - PREVIEW_ACTIVITY_WINDOW_MS,
                PREVIEW_ACTIVITY_WINDOW_MS
            ),
            "exactly at the window edge no longer withholds (decaying, exclusive)"
        );
        assert!(
            !activity_stamp_withholds_idle(
                now,
                now - (PREVIEW_ACTIVITY_WINDOW_MS + 5_000),
                PREVIEW_ACTIVITY_WINDOW_MS
            ),
            "past the window no longer withholds"
        );
    }

    #[test]
    fn note_preview_activity_withholds_then_resumes_idle() {
        for (label, note) in [
            (
                "routed",
                &ActivityTracker::note_preview_routed_activity as &dyn Fn(&ActivityTracker),
            ),
            ("status", &ActivityTracker::note_preview_status_activity),
        ] {
            let t = ActivityTracker::new();
            assert!(
                t.snapshot().idle_since_ms.is_some(),
                "{label}: an idle tracker reports idle before any preview activity"
            );

            note(&t);
            assert!(
                t.snapshot().idle_since_ms.is_none(),
                "{label}: recent preview activity must withhold idle"
            );
            assert!(
                t.snapshot_session("any").idle_since_ms.is_none(),
                "{label}: the per-session payload must withhold idle too"
            );

            let stale = now_ms().saturating_sub(PREVIEW_ACTIVITY_WINDOW_MS + 1_000);
            t.last_preview_routed_ms.store(stale, Ordering::Relaxed);
            t.last_preview_status_ms.store(stale, Ordering::Relaxed);
            assert!(
                t.snapshot().idle_since_ms.is_some(),
                "{label}: idle must resume once the preview window decays"
            );
        }
    }

    #[test]
    fn withhold_reason_reports_the_tier_that_is_holding() {
        let t = ActivityTracker::new();
        assert_eq!(
            t.snapshot().withhold_reason,
            None,
            "nothing holding ⇒ no reason"
        );

        t.note_preview_status_activity();
        let s = t.snapshot();
        assert_eq!(
            s.withhold_reason,
            Some(IdleWithholdReason::PreviewStatusOnly),
            "a bare status poll is the weakest tier"
        );
        assert!(
            s.withhold_since_ms.is_some(),
            "every reason carries a since-stamp so a reader can age the hold"
        );

        t.note_client_rpc_activity();
        assert_eq!(
            t.snapshot().withhold_reason,
            Some(IdleWithholdReason::ClientRpc),
            "a client mutation outranks the status poll"
        );

        t.note_preview_routed_activity();
        assert_eq!(
            t.snapshot().withhold_reason,
            Some(IdleWithholdReason::PreviewRouted),
            "real app traffic outranks the status poll"
        );

        t.set_preview_attached(1, 0);
        let s = t.snapshot();
        assert_eq!(
            s.withhold_reason,
            Some(IdleWithholdReason::PreviewAttached),
            "an open tunnel outranks everything below it"
        );
        assert_eq!(s.preview_ws_tunnels_open, 1);
        assert!(!s.withhold_capped, "no ceilings configured yet");
    }

    #[test]
    fn client_rpc_withholds_and_advances_the_anchor() {
        let t = ActivityTracker::new();
        let before = now_ms();
        t.note_client_rpc_activity();

        let s = t.snapshot();
        assert!(
            s.idle_since_ms.is_none(),
            "a recent mutation withholds idle"
        );
        assert_eq!(s.withhold_reason, Some(IdleWithholdReason::ClientRpc));
        assert!(
            s.withhold_since_ms.is_some_and(|since| since >= before),
            "a mutation is real use, so it advances the anchor (unlike a poll)"
        );

        let per_session = t.snapshot_session("some-session");
        assert!(
            per_session.idle_since_ms.is_none(),
            "the hold is aggregate: per-session payloads withhold too"
        );
    }

    #[test]
    fn zero_rpc_window_disables_the_client_rpc_withhold() {
        let t = ActivityTracker::new().with_rpc_activity_window_ms(0);
        t.note_client_rpc_activity();
        let s = t.snapshot();
        assert!(
            s.idle_since_ms.is_some(),
            "window 0 is the kill switch: the stamp must never withhold"
        );
        assert_eq!(s.withhold_reason, None);
    }

    #[test]
    fn live_scheduler_with_pending_fire_withholds_idle() {
        let t = ActivityTracker::new();
        let started = t.started_at_ms;
        t.record_scheduler_poll(Some(now_ms() + 60 * 60 * 1000));

        let s = t.snapshot();
        assert!(
            s.idle_since_ms.is_none(),
            "a live loop with a coming run withholds idle at any cadence inside the window"
        );
        assert_eq!(s.withhold_reason, Some(IdleWithholdReason::ScheduledTask));
        assert_eq!(
            s.withhold_since_ms,
            Some(started),
            "a waiting loop is not user activity, so the anchor stays on process start"
        );

        let per_session = t.snapshot_session("some-session");
        assert!(
            per_session.idle_since_ms.is_none(),
            "the hold is aggregate: per-session payloads withhold too"
        );
    }

    #[test]
    fn dead_scheduler_releases_the_hold() {
        let t = ActivityTracker::new();
        t.record_scheduler_poll(Some(now_ms() + 60_000));

        assert_eq!(
            t.snapshot().withhold_reason,
            Some(IdleWithholdReason::ScheduledTask)
        );

        // The poller reports None when the actor's channel is closed or no task is pending.
        t.record_scheduler_poll(None);
        let s = t.snapshot();
        assert!(s.idle_since_ms.is_some());
        assert_eq!(s.withhold_reason, None);
    }

    #[test]
    fn stale_poll_stamp_releases_the_hold() {
        let t = ActivityTracker::new();
        t.record_scheduler_poll_at(
            Some(now_ms() + 60_000),
            now_ms() - SCHEDULER_POLL_MAX_AGE_MS - 1,
        );

        assert_eq!(
            t.snapshot().withhold_reason,
            None,
            "a dead poller cannot keep the sandbox awake"
        );
    }

    #[test]
    fn a_run_beyond_the_window_does_not_keep_awake() {
        let t = ActivityTracker::new();
        t.record_scheduler_poll(Some(
            now_ms() + SCHEDULED_TASK_KEEP_AWAKE_WINDOW_MS + 60_000,
        ));

        let s = t.snapshot();
        assert!(
            s.idle_since_ms.is_some(),
            "no sandbox lives to see a run past the window, so it must not keep awake"
        );
        assert_eq!(s.withhold_reason, None);
    }

    #[test]
    fn zero_window_turns_the_keep_awake_off() {
        let t = ActivityTracker::new().with_scheduled_task_keep_awake_window_ms(0);
        t.record_scheduler_poll(Some(now_ms() + 60_000));

        let s = t.snapshot();
        assert!(
            s.idle_since_ms.is_some(),
            "window 0 is the off switch: a live loop must never keep the sandbox awake"
        );
        assert_eq!(s.withhold_reason, None);
    }

    #[test]
    fn client_presence_withholds_without_advancing_the_anchor() {
        let t = ActivityTracker::new();
        let started = t.started_at_ms;
        t.apply_presence_note(true, Some(1));

        let s = t.snapshot();
        assert!(
            s.idle_since_ms.is_none(),
            "a recent presence note withholds idle"
        );
        assert_eq!(s.withhold_reason, Some(IdleWithholdReason::ClientPresence));
        assert_eq!(
            s.withhold_since_ms,
            Some(started),
            "presence never advances the anchor past process start"
        );

        t.note_client_rpc_activity();
        assert_eq!(
            t.snapshot().withhold_reason,
            Some(IdleWithholdReason::ClientRpc),
            "any stronger tier outranks presence"
        );
    }

    /// A slow superseded visible note landing after a newer hidden note must
    /// not re-arm the withhold.
    #[test]
    fn stale_presence_seq_is_dropped() {
        let t = ActivityTracker::new();
        t.apply_presence_note(false, Some(2));
        t.apply_presence_note(true, Some(1));
        assert_eq!(t.snapshot().withhold_reason, None);

        t.apply_presence_note(true, Some(3));
        assert_eq!(
            t.snapshot().withhold_reason,
            Some(IdleWithholdReason::ClientPresence)
        );
    }

    #[test]
    fn zero_presence_window_disables_the_client_presence_withhold() {
        let t = ActivityTracker::new().with_presence_activity_window_ms(0);
        t.apply_presence_note(true, Some(1));
        let s = t.snapshot();
        assert!(
            s.idle_since_ms.is_some(),
            "window 0 is the kill switch (and the dark default): the stamp must never withhold"
        );
        assert_eq!(s.withhold_reason, None);
    }

    /// An HMR socket writes no activity stamps, so the counter must hold alone
    /// — which is exactly why the scraper must not clear it on one missed poll.
    #[test]
    fn attached_client_withholds_without_any_activity_stamp() {
        let t = ActivityTracker::new();
        assert!(t.snapshot().idle_since_ms.is_some());

        t.set_preview_attached(1, 0);
        assert!(
            t.snapshot().idle_since_ms.is_none(),
            "an open WebSocket tunnel withholds idle by itself"
        );

        t.set_preview_attached(0, 1);
        assert_eq!(
            t.snapshot().withhold_reason,
            Some(IdleWithholdReason::PreviewAttached),
            "a routed request in flight is equally an attached client"
        );

        t.set_preview_attached(0, 0);
        assert!(
            t.snapshot().idle_since_ms.is_some(),
            "once detached with no stamp in window, idle resumes"
        );
    }

    /// A ceiling will be measured from the anchor, so letting the pane's own
    /// poll reset it would make that ceiling unreachable.
    #[test]
    fn status_poll_does_not_advance_the_withhold_anchor() {
        let t = ActivityTracker::new();
        t.note_preview_routed_activity();
        let anchor = t.snapshot().withhold_since_ms.expect("routed holds");

        std::thread::sleep(std::time::Duration::from_millis(5));
        t.note_preview_status_activity();
        assert_eq!(
            t.snapshot().withhold_since_ms,
            Some(anchor),
            "a status poll leaves the anchor where the last real use put it"
        );
    }

    /// A session busy with real work must not be attributed to the preview just
    /// because the pane happens to be polling it. Both look like a missing
    /// `idle_since_ms` on the wire, and conflating them would inflate the
    /// preview share of exactly the population this field exists to measure.
    #[test]
    fn a_genuinely_busy_session_reports_no_withhold_reason() {
        let t = ActivityTracker::new();
        t.note_preview_status_activity();
        assert_eq!(
            t.snapshot().withhold_reason,
            Some(IdleWithholdReason::PreviewStatusOnly),
            "idle but polled ⇒ the poll is the reason"
        );

        t.tool_call_started("c1", "read_file", Some("sess-a"));
        let s = t.snapshot();
        assert!(
            s.idle_since_ms.is_none(),
            "a tool call in flight withholds idle on its own"
        );
        assert_eq!(
            s.withhold_reason, None,
            "the tool call is the cause, not the concurrent poll"
        );
        assert_eq!(s.withhold_since_ms, None);

        t.tool_call_completed("c1", Some("sess-a"), ToolOutcome::Success);
        assert_eq!(
            t.snapshot().withhold_reason,
            Some(IdleWithholdReason::PreviewStatusOnly),
            "once the real work finishes, the poll is the reason again"
        );
    }

    /// Durability is already bounded by its own cap, so it is the reason worth
    /// reporting when both hold.
    #[tokio::test]
    async fn durability_outranks_preview_in_the_reported_reason() {
        let t = ActivityTracker::new();
        let tasks = tokio_util::task::TaskTracker::new();
        t.set_producer_tasks(tasks.clone());

        t.note_preview_status_activity();
        assert_eq!(
            t.snapshot().withhold_reason,
            Some(IdleWithholdReason::PreviewStatusOnly),
            "preview alone reports the preview reason"
        );

        // An in-flight producer engages the durability gate. Nothing else does
        // — a background task is not durable work.
        let gate = Arc::new(tokio::sync::Notify::new());
        let gate2 = gate.clone();
        let join = tasks.spawn(async move { gate2.notified().await });

        let s = t.snapshot();
        assert_eq!(s.artifact_producers_inflight, 1, "the gate is engaged");
        assert_eq!(
            s.withhold_reason,
            Some(IdleWithholdReason::Durability),
            "durability outranks a concurrent preview hold"
        );
        assert!(
            s.withhold_since_ms.is_some(),
            "a durability hold reports its own busy-since stamp"
        );

        gate.notify_one();
        join.await.expect("producer task must not panic");
        assert_eq!(
            t.snapshot().withhold_reason,
            Some(IdleWithholdReason::PreviewStatusOnly),
            "once durability clears, the preview hold is reported again"
        );
    }

    #[test]
    fn configured_preview_window_overrides_default() {
        let configured = ActivityTracker::new().with_preview_activity_window_ms(500);
        configured
            .last_preview_status_ms
            .store(now_ms().saturating_sub(1_000), Ordering::Relaxed);
        assert!(configured.snapshot().idle_since_ms.is_some());

        let default = ActivityTracker::new();
        default
            .last_preview_status_ms
            .store(now_ms().saturating_sub(1_000), Ordering::Relaxed);
        assert!(default.snapshot().idle_since_ms.is_none());
    }

    #[test]
    fn preview_activity_does_not_override_active_tool_call() {
        let t = ActivityTracker::new();
        t.tool_call_started("c1", "read_file", Some("sess-a"));
        t.note_preview_routed_activity();
        let s = t.snapshot();
        assert!(s.idle_since_ms.is_none());
        assert_eq!(
            s.status,
            ToolServerLifecycleStatus::Busy,
            "an in-flight tool call stays Busy; preview activity only gates idle reporting"
        );
    }

    #[test]
    fn durability_idle_hold_from_raw_parses_and_falls_back() {
        assert_eq!(
            durability_idle_hold_from_raw(None),
            DEFAULT_DURABILITY_IDLE_HOLD_MAX_MS
        );
        assert_eq!(durability_idle_hold_from_raw(Some("1234".into())), 1234);
        assert_eq!(durability_idle_hold_from_raw(Some(" 99 ".into())), 99);
        assert_eq!(durability_idle_hold_from_raw(Some("0".into())), 0);
        assert_eq!(
            durability_idle_hold_from_raw(Some("nonsense".into())),
            DEFAULT_DURABILITY_IDLE_HOLD_MAX_MS
        );
        assert_eq!(
            durability_idle_hold_from_raw(Some("-5".into())),
            DEFAULT_DURABILITY_IDLE_HOLD_MAX_MS
        );
    }

    #[test]
    fn set_draining_stamps_drain_started_ms_once() {
        let t = ActivityTracker::new();
        assert_eq!(t.snapshot().drain_started_ms, None);
        t.set_draining();
        let first = t.snapshot().drain_started_ms.expect("drain_started_ms set");
        assert!(first > 0);
        // A second set_draining must not move the stamp.
        t.set_draining();
        assert_eq!(t.snapshot().drain_started_ms, Some(first));
    }

    #[test]
    fn set_active_clears_drain_started_ms() {
        let t = ActivityTracker::new();
        t.set_draining();
        assert!(t.snapshot().drain_started_ms.is_some());
        // Resuming clears the stamp so it tracks "currently draining", not
        // "ever drained" — the server idle gate's drain escape keys off it.
        t.set_active();
        assert_eq!(t.snapshot().drain_started_ms, None);
        // A fresh drain after the resume re-stamps with a new value.
        t.set_draining();
        assert!(t.snapshot().drain_started_ms.is_some());
    }

    #[test]
    fn turn_active_surfaces_in_snapshots() {
        let t = ActivityTracker::new();
        assert!(!t.snapshot().turn_active);
        t.turn_started("sess-a", 1);
        assert!(t.snapshot().turn_active, "aggregate: any session active");
        assert!(t.snapshot_session("sess-a").turn_active);
        assert!(
            !t.snapshot_session("sess-b").turn_active,
            "a different session is not turn-active"
        );
        t.turn_completed("sess-a", 1, 10);
        assert!(!t.snapshot().turn_active);
    }

    /// The notify handle wakes the same waiter the status publisher blocks on.
    #[tokio::test]
    async fn notify_handle_shares_the_publisher_waiter() {
        let t = Arc::new(ActivityTracker::new());
        let notify = t.notify_handle();
        let t2 = t.clone();
        let waiter =
            tokio::spawn(
                async move { t2.wait_for_change(std::time::Duration::from_secs(5)).await },
            );
        tokio::task::yield_now().await;
        notify.notify_waiters();
        tokio::time::timeout(std::time::Duration::from_secs(2), waiter)
            .await
            .expect("publisher waiter must wake via the shared notify handle")
            .expect("waiter task should not panic");
    }

    #[test]
    fn multiple_concurrent_calls() {
        let t = ActivityTracker::new();
        t.tool_call_started("c1", "read_file", None);
        t.tool_call_started("c2", "grep", None);
        assert_eq!(t.snapshot().active_tool_calls, 2);
        t.tool_call_completed("c1", None, ToolOutcome::Success);
        assert_eq!(t.snapshot().active_tool_calls, 1);
        t.tool_call_completed("c2", None, ToolOutcome::Success);
        assert_eq!(t.snapshot().status, ToolServerLifecycleStatus::Ready);
    }

    #[test]
    fn background_task_keeps_busy_after_calls_complete() {
        let t = ActivityTracker::new();
        t.tool_call_started("c1", "run_terminal_cmd", None);
        t.background_task_started("bg1");
        t.tool_call_completed("c1", None, ToolOutcome::Success);
        assert_eq!(t.snapshot().status, ToolServerLifecycleStatus::Busy);
        t.background_task_completed("bg1");
        assert_eq!(t.snapshot().status, ToolServerLifecycleStatus::Ready);
    }

    #[test]
    fn per_session_independent_tracking() {
        let t = ActivityTracker::new();
        t.tool_call_started("c1", "read_file", Some("sess-a"));
        t.tool_call_started("c2", "grep", Some("sess-b"));

        let sa = t.snapshot_session("sess-a");
        assert_eq!(sa.active_tool_calls, 1);
        assert_eq!(sa.session_id.as_ref().map(|s| s.as_str()), Some("sess-a"));

        let sb = t.snapshot_session("sess-b");
        assert_eq!(sb.active_tool_calls, 1);

        assert_eq!(t.snapshot().active_tool_calls, 2);
    }

    #[test]
    fn per_session_completion_independent() {
        let t = ActivityTracker::new();
        t.tool_call_started("c1", "read_file", Some("sess-a"));
        t.tool_call_started("c2", "grep", Some("sess-b"));
        t.tool_call_completed("c1", None, ToolOutcome::Success);

        assert_eq!(
            t.snapshot_session("sess-a").status,
            ToolServerLifecycleStatus::Ready
        );
        assert_eq!(
            t.snapshot_session("sess-b").status,
            ToolServerLifecycleStatus::Busy
        );
    }

    #[test]
    fn unknown_session_returns_ready() {
        let t = ActivityTracker::new();
        assert_eq!(
            t.snapshot_session("nonexistent").status,
            ToolServerLifecycleStatus::Ready
        );
    }

    #[test]
    fn known_sessions_excludes_default() {
        let t = ActivityTracker::new();
        t.tool_call_started("c1", "x", None);
        t.tool_call_started("c2", "y", Some("sess-a"));
        assert_eq!(t.known_sessions(), vec!["sess-a"]);
    }

    #[test]
    fn draining_propagates_to_session_snapshot() {
        let t = ActivityTracker::new();
        t.tool_call_started("c1", "x", Some("sess-a"));
        t.set_draining();
        assert_eq!(
            t.snapshot_session("sess-a").status,
            ToolServerLifecycleStatus::Draining
        );
    }

    #[test]
    fn prune_keeps_active_sessions() {
        let t = ActivityTracker::new();
        t.tool_call_started("c1", "x", Some("active"));
        assert_eq!(t.known_sessions(), vec!["active"]);
    }

    #[test]
    fn prune_keeps_recently_idle() {
        let t = ActivityTracker::new();
        t.tool_call_started("c1", "x", Some("recent"));
        t.tool_call_completed("c1", None, ToolOutcome::Success);
        assert_eq!(t.known_sessions(), vec!["recent"]);
    }

    #[test]
    fn prune_removes_stale() {
        let t = ActivityTracker::new();
        t.tool_call_started("c1", "x", Some("stale"));
        t.tool_call_completed("c1", None, ToolOutcome::Success);
        if let Some(session) = t.sessions.get("stale") {
            let old = now_ms() - SESSION_IDLE_PRUNE_MS - 1000;
            session.idle_since_ms.store(old, Ordering::Relaxed);
        }
        assert!(t.known_sessions().is_empty());
        assert!(!t.sessions.contains_key("stale"));
    }

    #[test]
    fn small_prune_window_evicts_session_default_window_retains() {
        // A session idle for ~50ms: pruned under a 10ms window, retained
        // under the default 300s window. Proves the window is actually used.
        let idle_ago = 50;

        let small = ActivityTracker::with_prune_window(std::time::Duration::from_millis(10));
        small.tool_call_started("c1", "x", Some("sess"));
        small.tool_call_completed("c1", None, ToolOutcome::Success);
        if let Some(session) = small.sessions.get("sess") {
            session
                .idle_since_ms
                .store(now_ms() - idle_ago, Ordering::Relaxed);
        }
        assert!(
            small.known_sessions().is_empty(),
            "session idle past the small window must be pruned"
        );
        assert!(!small.sessions.contains_key("sess"));

        let default = ActivityTracker::new();
        default.tool_call_started("c1", "x", Some("sess"));
        default.tool_call_completed("c1", None, ToolOutcome::Success);
        if let Some(session) = default.sessions.get("sess") {
            session
                .idle_since_ms
                .store(now_ms() - idle_ago, Ordering::Relaxed);
        }
        assert_eq!(
            default.known_sessions(),
            vec!["sess"],
            "the same idle time must be retained under the default window"
        );
    }

    #[test]
    fn duplicate_start_is_noop() {
        let t = ActivityTracker::new();
        t.tool_call_started("c1", "read_file", Some("sess-a"));
        t.tool_call_started("c1", "grep", Some("sess-b"));
        // Second start with the same call_id must be ignored.
        assert_eq!(t.snapshot().active_tool_calls, 1);
        assert_eq!(t.snapshot_session("sess-a").active_tool_calls, 1);
        // sess-b should not have been created by the duplicate.
        assert_eq!(t.snapshot_session("sess-b").active_tool_calls, 0);
        // Completion still works normally.
        t.tool_call_completed("c1", None, ToolOutcome::Success);
        assert_eq!(t.snapshot().active_tool_calls, 0);
    }

    #[test]
    fn duplicate_completion_is_noop() {
        let t = ActivityTracker::new();
        t.tool_call_started("c1", "read_file", None);
        assert_eq!(t.snapshot().active_tool_calls, 1);
        t.tool_call_completed("c1", None, ToolOutcome::Success);
        assert_eq!(t.snapshot().active_tool_calls, 0);
        // Second completion of the same call_id must not underflow.
        t.tool_call_completed("c1", None, ToolOutcome::Success);
        assert_eq!(t.snapshot().active_tool_calls, 0);
    }

    #[test]
    fn unknown_call_id_completion_is_noop() {
        let t = ActivityTracker::new();
        // Completing a call_id that was never started must not underflow.
        t.tool_call_completed("never-started", None, ToolOutcome::Success);
        assert_eq!(t.snapshot().active_tool_calls, 0);
        assert_eq!(t.snapshot().status, ToolServerLifecycleStatus::Ready);
    }

    #[test]
    fn duplicate_background_start_is_noop() {
        let t = ActivityTracker::new();
        t.background_task_started("bg1");
        t.background_task_started("bg1");
        assert_eq!(t.snapshot().background_tasks, 1);
        t.background_task_completed("bg1");
        assert_eq!(t.snapshot().background_tasks, 0);
    }

    #[test]
    fn duplicate_background_completion_is_noop() {
        let t = ActivityTracker::new();
        t.background_task_started("bg1");
        assert_eq!(t.snapshot().background_tasks, 1);
        t.background_task_completed("bg1");
        assert_eq!(t.snapshot().background_tasks, 0);
        // Second completion must not underflow.
        t.background_task_completed("bg1");
        assert_eq!(t.snapshot().background_tasks, 0);
    }

    #[test]
    fn unknown_background_task_completion_is_noop() {
        let t = ActivityTracker::new();
        t.background_task_completed("never-started");
        assert_eq!(t.snapshot().background_tasks, 0);
    }

    #[test]
    fn prune_cleans_call_to_session_mapping() {
        let t = ActivityTracker::new();
        t.tool_call_started("c1", "x", Some("stale"));
        t.tool_call_completed("c1", None, ToolOutcome::Success);
        // Simulate stale idle time.
        if let Some(session) = t.sessions.get("stale") {
            let old = now_ms() - SESSION_IDLE_PRUNE_MS - 1000;
            session.idle_since_ms.store(old, Ordering::Relaxed);
        }
        // Prune should remove the session and its call_to_session entries.
        t.known_sessions();
        assert!(!t.sessions.contains_key("stale"));
        // Verify call_to_session was cleaned (no dangling entries).
        assert!(!t.call_to_session.contains_key("c1"));
    }

    #[test]
    fn per_session_completion_after_prune_is_safe() {
        let t = ActivityTracker::new();
        t.tool_call_started("c1", "x", Some("sess"));
        // Force-prune the session while the call is still "active"
        // (simulates the theoretical race).
        t.sessions.remove("sess");
        // Completing should not panic or underflow — the session
        // lookup returns None, so only the global counter decrements.
        t.tool_call_completed("c1", None, ToolOutcome::Success);
        assert_eq!(t.snapshot().active_tool_calls, 0);
    }

    #[test]
    fn turn_started_sets_current_turn_and_active() {
        let t = ActivityTracker::new();
        t.turn_started("sess-a", 1);
        let session = t.sessions.get("sess-a").expect("session should exist");
        assert_eq!(session.current_turn.load(Ordering::Acquire), 1);
        assert!(session.turn_active.load(Ordering::Acquire));
    }

    #[test]
    fn turn_completed_clears_active_when_turn_matches() {
        let t = ActivityTracker::new();
        t.turn_started("sess-a", 1);
        t.turn_completed("sess-a", 1, 500);
        let session = t.sessions.get("sess-a").expect("session should exist");
        assert!(!session.turn_active.load(Ordering::Acquire));
    }

    #[test]
    fn turn_completed_stale_turn_does_not_clear_active() {
        let t = ActivityTracker::new();
        t.turn_started("sess-a", 1);
        t.turn_started("sess-a", 2);
        // Completing the stale turn 1 must not clear turn_active for turn 2.
        t.turn_completed("sess-a", 1, 500);
        let session = t.sessions.get("sess-a").expect("session should exist");
        assert!(
            session.turn_active.load(Ordering::Acquire),
            "turn_active should still be true for current turn 2"
        );
        assert_eq!(session.current_turn.load(Ordering::Acquire), 2);
    }

    #[test]
    fn turn_completed_unknown_session_is_noop() {
        let t = ActivityTracker::new();
        // Must not panic or create a session entry.
        t.turn_completed("nonexistent", 1, 100);
        assert!(!t.sessions.contains_key("nonexistent"));
    }

    #[test]
    fn session_ended_releases_an_idle_session() {
        let t = ActivityTracker::new();
        t.turn_started("sess-a", 3);
        assert!(t.is_turn_active("sess-a"));

        t.session_ended("sess-a");

        assert!(
            !t.sessions.contains_key("sess-a"),
            "an ended session must not stay resident"
        );
    }

    #[test]
    fn session_ended_unknown_session_is_noop() {
        let t = ActivityTracker::new();
        // Must not panic or create a session entry.
        t.session_ended("nonexistent");
        assert!(!t.sessions.contains_key("nonexistent"));
    }

    #[test]
    fn session_ended_without_active_turn_is_safe() {
        let t = ActivityTracker::new();
        // Create a session via a tool call cycle (turn never started).
        t.tool_call_started("c1", "read_file", Some("sess-a"));
        t.tool_call_completed("c1", None, ToolOutcome::Success);

        t.session_ended("sess-a");

        assert!(!t.sessions.contains_key("sess-a"));
    }

    #[tokio::test]
    async fn session_ended_notifies_waiters() {
        let t = std::sync::Arc::new(ActivityTracker::new());
        t.turn_started("sess-a", 1);

        let t2 = t.clone();
        let waiter = tokio::spawn(async move {
            t2.wait_for_change(std::time::Duration::from_secs(5)).await;
        });

        // Give the waiter a moment to register.
        tokio::task::yield_now().await;

        t.session_ended("sess-a");

        // The waiter should complete promptly (not timeout).
        tokio::time::timeout(std::time::Duration::from_secs(2), waiter)
            .await
            .expect("waiter should complete within timeout")
            .expect("waiter task should not panic");
    }

    #[test]
    fn session_ended_with_inflight_tool_calls() {
        let t = ActivityTracker::new();
        t.turn_started("sess-a", 1);
        t.tool_call_started("c1", "read_file", Some("sess-a"));
        assert!(t.is_turn_active("sess-a"));
        assert_eq!(t.snapshot_session("sess-a").active_tool_calls, 1);

        t.session_ended("sess-a");

        assert!(!t.is_turn_active("sess-a"));
        assert!(
            t.sessions.contains_key("sess-a"),
            "a session with a call in flight keeps its counters"
        );
        assert_eq!(
            t.snapshot_session("sess-a").active_tool_calls,
            1,
            "session_ended must not clear active tool calls"
        );
        assert_eq!(
            t.snapshot_session("sess-a").status,
            ToolServerLifecycleStatus::Busy,
            "session should still be busy due to in-flight tool call"
        );
    }

    #[test]
    fn cancel_all_session_calls_completes_all_for_session() {
        let t = ActivityTracker::new();
        t.tool_call_started("c1", "read_file", Some("sess-a"));
        t.tool_call_started("c2", "grep", Some("sess-a"));
        t.tool_call_started("c3", "write", Some("sess-b"));
        assert_eq!(t.snapshot().active_tool_calls, 3);

        let cancelled = t.cancel_all_session_calls("sess-a");
        assert_eq!(cancelled, 2, "should cancel 2 calls for sess-a");
        assert_eq!(
            t.snapshot().active_tool_calls,
            1,
            "only sess-b call remains"
        );
        assert_eq!(t.snapshot_session("sess-a").active_tool_calls, 0);
        assert_eq!(t.snapshot_session("sess-b").active_tool_calls, 1);
    }

    #[test]
    fn cancel_all_session_calls_noop_for_empty_session() {
        let t = ActivityTracker::new();
        let cancelled = t.cancel_all_session_calls("nonexistent");
        assert_eq!(cancelled, 0);
        assert_eq!(t.snapshot().active_tool_calls, 0);
    }

    #[test]
    fn cancel_all_session_calls_idempotent() {
        let t = ActivityTracker::new();
        t.tool_call_started("c1", "read_file", Some("sess-a"));
        assert_eq!(t.cancel_all_session_calls("sess-a"), 1);
        assert_eq!(
            t.cancel_all_session_calls("sess-a"),
            0,
            "second cancel_all should find no calls"
        );
        assert_eq!(t.snapshot().active_tool_calls, 0);
    }

    #[test]
    fn double_cancel_via_tool_call_completed_is_idempotent() {
        let t = ActivityTracker::new();
        t.tool_call_started("c1", "read_file", Some("sess-a"));
        assert_eq!(t.snapshot().active_tool_calls, 1);

        t.tool_call_completed("c1", None, ToolOutcome::Success);
        assert_eq!(t.snapshot().active_tool_calls, 0);

        // Second completion of the same call_id must not underflow.
        t.tool_call_completed("c1", None, ToolOutcome::Success);
        assert_eq!(
            t.snapshot().active_tool_calls,
            0,
            "double cancel must not underflow active_tool_calls"
        );
        assert_eq!(
            t.snapshot_session("sess-a").active_tool_calls,
            0,
            "double cancel must not underflow session active_tool_calls"
        );
    }

    // ── events.jsonl emission ─────────────────────────────────

    /// Build a tracker whose event sink points at a fresh tempdir, with the
    /// `events.jsonl` writer for `session` pre-opened (as turn-start would do).
    fn tracker_with_writer(session: &str) -> (ActivityTracker, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let writers: Arc<DashMap<String, EventWriter>> = Arc::new(DashMap::new());
        writers.insert(session.to_owned(), EventWriter::open(dir.path()));
        let t = ActivityTracker::new();
        t.set_event_writers(writers);
        (t, dir)
    }

    fn read_events(dir: &tempfile::TempDir) -> Vec<serde_json::Value> {
        let text = std::fs::read_to_string(dir.path().join("events.jsonl")).unwrap();
        text.trim()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn emits_tool_started_and_completed_with_real_content() {
        let (t, dir) = tracker_with_writer("sess-a");
        t.tool_call_started("c1", "read_file", Some("sess-a"));
        t.tool_call_completed("c1", Some("sess-a"), ToolOutcome::Success);

        let events = read_events(&dir);
        assert_eq!(events.len(), 2, "expected ToolStarted + ToolCompleted");
        assert_eq!(events[0]["type"], "tool_started");
        assert_eq!(events[0]["tool_name"], "read_file");
        assert_eq!(events[1]["type"], "tool_completed");
        assert_eq!(events[1]["tool_name"], "read_file");
        assert_eq!(events[1]["outcome"], "success");
        assert_eq!(events[1]["source"], "workspace");
        assert!(
            events[1]["duration_ms"].as_u64().is_some(),
            "ToolCompleted must carry a duration_ms"
        );
    }

    #[test]
    fn tool_completed_outcome_is_truthful_error() {
        let (t, dir) = tracker_with_writer("sess-a");
        t.tool_call_started("c1", "run_terminal_command", Some("sess-a"));
        t.tool_call_completed("c1", Some("sess-a"), ToolOutcome::Error);

        let events = read_events(&dir);
        assert_eq!(events[1]["type"], "tool_completed");
        assert_eq!(events[1]["outcome"], "error");
    }

    #[test]
    fn cancel_all_marks_tool_completed_cancelled() {
        let (t, dir) = tracker_with_writer("sess-a");
        t.tool_call_started("c1", "run_terminal_command", Some("sess-a"));
        assert_eq!(t.cancel_all_session_calls("sess-a"), 1);

        let events = read_events(&dir);
        let last = events.last().unwrap();
        assert_eq!(last["type"], "tool_completed");
        assert_eq!(last["outcome"], "cancelled");
    }

    #[test]
    fn no_tool_events_without_event_sink() {
        // Behaviour preservation: the default tracker (no event sink — the state
        // for all the legacy tests above) must never touch the filesystem.
        let t = ActivityTracker::new();
        t.tool_call_started("c1", "read_file", Some("sess-a"));
        t.tool_call_completed("c1", Some("sess-a"), ToolOutcome::Success);
        // Counters still behave, and nothing was recorded.
        assert_eq!(t.snapshot().active_tool_calls, 0);
        assert!(t.call_started_ms.is_empty());
    }

    #[test]
    fn no_event_when_session_writer_not_open() {
        // Sink configured but the session's writer was never opened (no
        // turn-start): no event is emitted and no start time leaks.
        let dir = tempfile::tempdir().unwrap();
        let writers: Arc<DashMap<String, EventWriter>> = Arc::new(DashMap::new());
        writers.insert("other".to_owned(), EventWriter::open(dir.path()));
        let t = ActivityTracker::new();
        t.set_event_writers(writers);

        t.tool_call_started("c1", "read_file", Some("unopened-sess"));
        t.tool_call_completed("c1", Some("unopened-sess"), ToolOutcome::Success);

        // The "other" session's events.jsonl must stay empty.
        let text = std::fs::read_to_string(dir.path().join("events.jsonl")).unwrap();
        assert!(text.trim().is_empty(), "no event should be written");
        // No start time was ever recorded: the insert is gated on this session's
        // writer being open, and "unopened-sess" has none — so the map is empty
        // (this is exactly the production flag-off shape: sink wired, map empty).
        assert!(t.call_started_ms.is_empty());
    }
}
