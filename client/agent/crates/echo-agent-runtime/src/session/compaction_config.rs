//! Compaction configuration and runtime state for the session actor.

use std::cell::Cell;
use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

/// Auto-compaction is gated whenever `auto_compact_suppressed` is not [`SUPPRESS_NONE`].
pub(crate) const SUPPRESS_NONE: u8 = 0;
/// Resolvable failure (`other`): suppressed for the current turn, then
/// cleared at the next turn start so compaction self-heals once the cause clears.
pub(crate) const SUPPRESS_TURN: u8 = 1;
/// Fatal failure (size/schema) retrying can never fix: survives turn boundaries,
/// cleared only when the context budget changes — a successful compaction, a
/// rewind (context shrank), or a model switch (a larger window may now fit).
pub(crate) const SUPPRESS_STICKY: u8 = 2;
/// Credit block: suppress until a model `200` (credits aren't client-observable).
/// Survives turns; context changes can't fix it. Token refresh must not clear this.
pub(crate) const SUPPRESS_UNTIL_SUCCESS: u8 = 3;
/// Auth-expired auto-compact: suppress until login/token refresh, not until 200
/// (waiting for a sample deadlocks when context is already over the window).
pub(crate) const SUPPRESS_AUTH: u8 = 4;

/// Model slug and context window from the previous turn.
#[derive(Clone, Debug)]
pub(crate) struct PreviousModelInfo {
    pub model_slug: String,
    pub context_window: u64,
}

/// Cached result of an **async** (background / prefire) pass-1 sample for
/// two-pass compaction. Held on the session actor between the background
/// pass-1 and the synchronous pass-2 apply at compaction time.
#[derive(Clone, Debug)]
pub(crate) struct AsyncCompactionCache {
    /// The successor-usable NOTE₁ text (extracted `<summary>` or full pass-1 output).
    pub note1: String,
    /// Number of leading conversation items pass-1 summarized (the prefix
    /// boundary in the LIVE conversation as of pass-1 time). The pass-2 tail is
    /// `conversation[prefix_len..]`.
    pub prefix_len: usize,
    /// Fingerprint of `conversation[..prefix_len]` at pass-1 time. Pass-2 only
    /// applies NOTE₁ when the current conversation still has this exact prefix.
    pub fingerprint: u64,
    /// Model slug pass-1 ran under; invalidated on model switch.
    pub model_slug: String,
    /// Wall time pass-1 took (ms) — latency that ran off the critical path
    /// when prefire finished before compact (not counted in telemetry TTFT unless
    /// the user waited on an in-flight pass-1).
    pub pass1_latency_ms: u64,
}

/// Cancel gate for an in-flight compact / prefire sample.
///
/// Holder count (not a bool): prefire and compact can overlap. The first
/// `enter` installs a token; nested enters reuse it; `in_flight` stays true
/// until the last scope drops. A normal turn stop is a no-op when idle.
#[derive(Default)]
pub(crate) struct CompactCancelGate {
    token: RefCell<tokio_util::sync::CancellationToken>,
    holders: AtomicUsize,
}

/// Decrements the holder count when a compact/prefire scope ends.
pub(crate) struct CompactCancelScope<'a>(&'a CompactCancelGate);

impl Drop for CompactCancelScope<'_> {
    fn drop(&mut self) {
        self.0.end();
    }
}

impl CompactCancelGate {
    /// Start or join a compact/prefire scope. Nested callers share one token,
    /// including a token already cancelled by stop, so overlapping prefire +
    /// compact both observe the same abort. A later independent enter after
    /// holders drain installs a fresh token.
    pub(crate) fn enter(&self) -> (tokio_util::sync::CancellationToken, CompactCancelScope<'_>) {
        let prev = self.holders.fetch_add(1, Ordering::AcqRel);
        let token = if prev == 0 {
            let token = tokio_util::sync::CancellationToken::new();
            self.token.replace(token.clone());
            token
        } else {
            self.token.borrow().clone()
        };
        (token, CompactCancelScope(self))
    }

    fn end(&self) {
        self.holders.fetch_sub(1, Ordering::AcqRel);
    }

    pub(crate) fn request_cancel(&self) {
        if self.holders.load(Ordering::Acquire) > 0 {
            self.token.borrow().cancel();
        }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.holders.load(Ordering::Acquire) > 0 && self.token.borrow().is_cancelled()
    }
}

/// Prefire two-pass state. `Default` so it drops into existing `CompactionConfig`
/// struct literals with a single `prefire: PrefireState::default()` field.
///
/// `SessionActor` is `!Send` and single-threaded; the `AtomicBool` is only used
/// for its ergonomic `compare_exchange` (no cross-thread sharing), and the
/// `RefCell`s need no locking (the `JoinHandle` is from `spawn_local`, so it is
/// local to this LocalSet and never crosses threads).
#[derive(Default)]
pub(crate) struct PrefireState {
    /// Set while a background pass-1 sample is running, so the per-turn trigger
    /// never spawns a second concurrent job.
    in_flight: AtomicBool,
    /// Cached async pass-1 result, ready for pass-2 apply (or `None`).
    cache: RefCell<Option<AsyncCompactionCache>>,
    /// Handle to the in-flight background pass-1 task. Pass-2 awaits this when
    /// compaction fires before prefire finished, so a still-running pass-1 is
    /// used rather than discarded for a full single-pass.
    handle: RefCell<Option<tokio::task::JoinHandle<()>>>,
}

impl PrefireState {
    /// Try to claim the single in-flight slot. Returns `true` iff this caller
    /// won the race and should spawn pass-1 (the caller must later call
    /// [`Self::finish`]).
    pub(crate) fn try_begin(&self) -> bool {
        self.in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
    }

    /// Release the in-flight slot (call exactly once after a `try_begin` win).
    pub(crate) fn finish(&self) {
        self.in_flight.store(false, Ordering::Release);
    }

    pub(crate) fn is_in_flight(&self) -> bool {
        self.in_flight.load(Ordering::Acquire)
    }

    /// Stash the spawned pass-1 task handle so pass-2 can await it if it is
    /// still running when compaction fires.
    pub(crate) fn set_handle(&self, handle: tokio::task::JoinHandle<()>) {
        self.handle.replace(Some(handle));
    }

    /// Take the pass-1 task handle, if any, so the caller can await completion
    /// before reading the cache. Leaves `None`.
    pub(crate) fn take_handle(&self) -> Option<tokio::task::JoinHandle<()>> {
        self.handle.borrow_mut().take()
    }

    pub(crate) fn store(&self, cache: AsyncCompactionCache) {
        self.cache.replace(Some(cache));
    }

    /// Take the cache, leaving `None`.
    pub(crate) fn take(&self) -> Option<AsyncCompactionCache> {
        self.cache.borrow_mut().take()
    }

    /// Drop any cached async pass-1 result (invalidation: model switch, rewind,
    /// apply, edits).
    pub(crate) fn clear(&self) {
        self.cache.replace(None);
    }

    pub(crate) fn has_cache(&self) -> bool {
        self.cache.borrow().is_some()
    }
}

pub(crate) struct CompactionConfig {
    /// Context window usage percentage (0-100) at which auto-compact triggers.
    ///
    /// `Cell` so the value can be re-resolved at model-switch time without
    /// holding `&mut self` on the actor. `SessionActor` is `!Send`, so
    /// `Cell` is sufficient (no atomic ordering needed).
    pub threshold_percent: Cell<u8>,
    /// Debug: when set, next auto-compact check triggers unconditionally.
    pub force_compact: Arc<AtomicBool>,
    /// Auto-compaction suppression state (`SUPPRESS_*`) after a deterministic
    /// failure; the gates early-return unless `SUPPRESS_NONE`. Manual `/compact` ignores it.
    pub auto_compact_suppressed: AtomicU8,
    /// Locks the context window when `ECHO_AGENT_DEBUG_CONTEXT_WINDOW` is set.
    pub context_window_override: Option<std::num::NonZeroU64>,
    pub count: AtomicU64,
    /// Set at turn end; consumed at next turn start for model-switch compaction.
    /// `Cell` because `SessionActor` is `!Send`.
    pub previous_model: Cell<Option<PreviousModelInfo>>,
    /// The resolved mode; `Segments` carries its detail level inline.
    pub compaction_mode: echo_agent_chat_state::CompactionMode,
    /// When `true`, feed the summarizer the verbatim conversation instead of the lossy rewrite (the retry loop may still fall back).
    pub verbatim_input: bool,
    pub tool_choice: crate::util::config::CompactionToolChoice,
    /// Prefire two-pass state (background NOTE₁ cache + in-flight guard).
    /// `Default` (empty cache, not in-flight).
    pub prefire: PrefireState,
    /// Sticky once a forked session releases its inherited prefix under compaction pressure (see `run_compact_inner`), so it stops re-pinning it.
    pub prefix_released: AtomicBool,
    /// User/stop cancel for the current compact generation.
    pub cancel: CompactCancelGate,
}

#[cfg(test)]
mod prefire_state_tests {
    use super::*;

    fn dummy_cache() -> AsyncCompactionCache {
        AsyncCompactionCache {
            note1: "NOTE1".to_string(),
            prefix_len: 3,
            fingerprint: 42,
            model_slug: "echoagent".to_string(),
            pass1_latency_ms: 5,
        }
    }

    /// Pass-2 must be able to await a still-running pass-1 and then read its
    /// cache — i.e. an in-flight prefire is waited for, not discarded for a full
    /// single-pass.
    #[tokio::test]
    async fn take_handle_awaits_in_flight_pass1_then_cache_is_available() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let state = std::rc::Rc::new(PrefireState::default());
                let worker = std::rc::Rc::clone(&state);
                // Background pass-1 that stores its cache only after yielding,
                // so the cache is absent at the moment pass-2 starts.
                let handle = tokio::task::spawn_local(async move {
                    tokio::task::yield_now().await;
                    worker.store(dummy_cache());
                    worker.finish();
                });
                state.set_handle(handle);

                assert!(!state.has_cache(), "cache absent before pass-1 completes");

                if let Some(h) = state.take_handle() {
                    let _ = h.await;
                }

                assert!(state.has_cache(), "cache present after awaiting pass-1");
                assert_eq!(state.take().unwrap().note1, "NOTE1");
                assert!(state.take_handle().is_none(), "handle consumed once taken");
            })
            .await;
    }

    /// No prefire spawned → no handle to await (pass-2 falls straight through to
    /// the single-pass path via the `take()?` that follows in the caller).
    #[tokio::test]
    async fn take_handle_is_none_without_a_spawned_pass1() {
        let state = PrefireState::default();
        assert!(state.take_handle().is_none());
        assert!(state.take().is_none());
    }
}

#[cfg(test)]
mod compact_cancel_gate_tests {
    use super::*;

    #[test]
    fn request_cancel_trips_shared_token() {
        let gate = CompactCancelGate::default();
        let (token, _scope) = gate.enter();
        assert!(!token.is_cancelled());
        gate.request_cancel();
        assert!(token.is_cancelled());
        assert!(gate.is_cancelled());
    }

    #[test]
    fn request_cancel_is_noop_when_idle() {
        let gate = CompactCancelGate::default();
        gate.request_cancel();
        let (token, _scope) = gate.enter();
        assert!(!token.is_cancelled());
        assert!(!gate.is_cancelled());
    }

    #[test]
    fn nested_enter_keeps_in_flight_after_inner_drop() {
        let gate = CompactCancelGate::default();
        let (outer_tok, outer) = gate.enter();
        let (inner_tok, inner) = gate.enter();
        gate.request_cancel();
        assert!(outer_tok.is_cancelled());
        assert!(inner_tok.is_cancelled());
        drop(inner);
        assert!(gate.is_cancelled());
        drop(outer);
        assert!(!gate.is_cancelled());
        let (next, _scope) = gate.enter();
        assert!(!next.is_cancelled());
    }

    #[test]
    fn join_while_cancelled_reuses_cancelled_token() {
        let gate = CompactCancelGate::default();
        let (_outer, outer) = gate.enter();
        gate.request_cancel();
        let (joined, joined_scope) = gate.enter();
        assert!(
            joined.is_cancelled(),
            "nested enter during stop must keep sharing the cancelled token"
        );
        drop(joined_scope);
        drop(outer);
        let (next, _scope) = gate.enter();
        assert!(
            !next.is_cancelled(),
            "fresh enter after scopes drain must not inherit the prior stop"
        );
    }
}
