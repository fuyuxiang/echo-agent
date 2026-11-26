//! Per-session strict-cancellation registry.
//!
//! Maps each in-flight `tool_call_id` to its [`CancellationToken`] so a
//! `Cancel` hook (or session teardown) can hard-cancel the running call
//! by dropping its future. A small `pending` tombstone set covers the
//! race where a `Cancel` arrives *before* the dispatcher registered the
//! token (the symmetric window to pre-spawn registration): the id is
//! tombstoned and the dispatcher cancels it at registration time.
//!
//! One registry per session, tied to the session-loop lifetime alongside
//! the inbox and the per-session admission semaphore.

use std::sync::atomic::{AtomicBool, Ordering};

use dashmap::{DashMap, DashSet};
use echo_agent_tool_protocol::ToolCallId;
use tokio_util::sync::CancellationToken;

/// Upper bound on outstanding pre-registration tombstones. Tombstones
/// cover the microscopic window between a `Cancel` hook and the matching
/// `register`, so in steady state the set holds a handful of entries. A
/// `Cancel` whose call never registers (e.g. one racing call completion,
/// after `deregister` already removed the live token) leaves a tombstone
/// that no `register` ever consumes; this cap reclaims such stragglers so
/// a single long-lived session cannot grow `pending` without bound.
const MAX_PENDING_TOMBSTONES: usize = 8192;

/// Per-session `tool_call_id -> CancellationToken` map plus a pending
/// tombstone set for cancels that land before registration.
#[derive(Default, Debug)]
pub(crate) struct CancelRegistry {
    map: DashMap<ToolCallId, CancellationToken>,
    pending: DashSet<ToolCallId>,
    /// Set once by [`Self::cancel_all`] (teardown). After this, every new
    /// `register` starts cancelled so a request dispatched in the teardown
    /// window cannot escape as an orphaned, uncancellable task.
    closed: AtomicBool,
}

impl CancelRegistry {
    /// Register `token` for `call_id` before the call is spawned. If a
    /// `Cancel` already tombstoned this id, the token is cancelled
    /// immediately so the call starts cancelled. Returns whether the
    /// token was pre-cancelled (by a tombstone or because the registry was
    /// torn down).
    pub(crate) fn register(&self, call_id: ToolCallId, token: &CancellationToken) -> bool {
        if self.closed.load(Ordering::Acquire) {
            token.cancel();
            return true;
        }
        let pre_cancelled = self.pending.remove(&call_id).is_some();
        if pre_cancelled {
            token.cancel();
        }
        self.map.insert(call_id.clone(), token.clone());
        // Re-check after the insert: if `cancel_all` drained the map
        // between our closed-check and the insert, our entry would be
        // missed. The DashMap shard lock orders the insert against the
        // drain, so observing `closed` here guarantees we cancel + drop
        // any entry the drain could not reach (closes the teardown race).
        if self.closed.load(Ordering::Acquire) {
            if let Some((_, missed)) = self.map.remove(&call_id) {
                missed.cancel();
            }
            return true;
        }
        pre_cancelled
    }

    /// Cancel a live call, else tombstone the id so the dispatcher
    /// cancels it at registration time. Returns true when a live token
    /// was found and cancelled.
    pub(crate) fn cancel(&self, call_id: &ToolCallId) -> bool {
        if let Some((_, token)) = self.map.remove(call_id) {
            token.cancel();
            true
        } else {
            if self.pending.len() >= MAX_PENDING_TOMBSTONES {
                // Evict one straggler tombstone (a cancel whose call never
                // registered) before inserting so the set stays bounded.
                // Collect the key first, then remove, so we never hold a
                // shard iterator across the removal.
                let stale = self.pending.iter().next().map(|e| e.key().clone());
                if let Some(stale) = stale {
                    self.pending.remove(&stale);
                }
            }
            self.pending.insert(call_id.clone());
            false
        }
    }

    /// Deregister a call's token on completion or cancel. Idempotent.
    pub(crate) fn deregister(&self, call_id: &ToolCallId) {
        self.map.remove(call_id);
    }

    /// Whether [`Self::cancel_all`] has closed this registry. A closed
    /// registry marks a session whose loop is (or is about to be) torn
    /// down — used by the soft-rebind liveness gate.
    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Drain-and-cancel every live token and close the registry. Used on
    /// session teardown (`unbind_session` / `shutdown` / full rebind of a
    /// dead loop — a soft rebind of a live session keeps its registry) so
    /// detached `execute_call` tasks wind down promptly AND any call
    /// dispatched in
    /// the teardown window starts cancelled (see [`Self::register`]).
    /// Returns the number of tokens cancelled.
    pub(crate) fn cancel_all(&self) -> usize {
        // Mark closed BEFORE draining so a concurrent `register` either
        // observes the close (and self-cancels) or has its entry drained
        // here — never both-miss.
        self.closed.store(true, Ordering::Release);
        let mut cancelled = 0;
        self.map.retain(|_, token| {
            token.cancel();
            cancelled += 1;
            false
        });
        // Drop tombstones too: teardown closes the registry, so no future
        // `register` will consume them. Leaving them would let a stale
        // straggler set survive to the end of the (already-done) session.
        self.pending.clear();
        cancelled
    }

    #[cfg(test)]
    pub(crate) fn live_count(&self) -> usize {
        self.map.len()
    }

    #[cfg(test)]
    pub(crate) fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cid() -> ToolCallId {
        ToolCallId::new_v7()
    }

    #[test]
    fn cancel_live_token_fires_and_removes_entry() {
        let reg = CancelRegistry::default();
        let id = cid();
        let token = CancellationToken::new();
        assert!(
            !reg.register(id.clone(), &token),
            "fresh register, no tombstone"
        );
        assert_eq!(reg.live_count(), 1);

        assert!(reg.cancel(&id), "live token must report a hit");
        assert!(
            token.is_cancelled(),
            "the registered token must be cancelled"
        );
        assert_eq!(reg.live_count(), 0, "cancel removes the live entry");
        assert_eq!(reg.pending_count(), 0, "a live hit leaves no tombstone");
    }

    #[test]
    fn cancel_before_registration_tombstones_then_register_pre_cancels() {
        let reg = CancelRegistry::default();
        let id = cid();

        // Cancel arrives first: no live token, so it tombstones.
        assert!(!reg.cancel(&id), "no live token yet → miss");
        assert_eq!(reg.pending_count(), 1);
        assert_eq!(reg.live_count(), 0);

        // Registration consumes the tombstone and starts cancelled.
        let token = CancellationToken::new();
        assert!(
            reg.register(id.clone(), &token),
            "register must report the pre-cancel"
        );
        assert!(token.is_cancelled(), "tombstone must pre-cancel the token");
        assert_eq!(reg.pending_count(), 0, "tombstone consumed at registration");
        assert_eq!(reg.live_count(), 1);
    }

    #[test]
    fn deregister_clears_live_entry_without_cancel() {
        let reg = CancelRegistry::default();
        let id = cid();
        let token = CancellationToken::new();
        reg.register(id.clone(), &token);

        reg.deregister(&id);
        assert_eq!(reg.live_count(), 0);
        assert!(
            !token.is_cancelled(),
            "deregister on normal completion must NOT cancel the token"
        );
        // A later cancel for a completed call only tombstones (harmless).
        assert!(!reg.cancel(&id));
        assert_eq!(reg.pending_count(), 1);
    }

    #[test]
    fn cancel_all_drains_and_cancels_every_live_token() {
        let reg = CancelRegistry::default();
        let ids: Vec<ToolCallId> = (0..5).map(|_| cid()).collect();
        let tokens: Vec<CancellationToken> = ids
            .iter()
            .map(|id| {
                let t = CancellationToken::new();
                reg.register(id.clone(), &t);
                t
            })
            .collect();
        assert_eq!(reg.live_count(), 5);

        assert_eq!(
            reg.cancel_all(),
            5,
            "cancel_all reports every drained token"
        );
        assert_eq!(reg.live_count(), 0, "registry is empty after teardown");
        for token in &tokens {
            assert!(token.is_cancelled(), "every live token must be cancelled");
        }
        // Idempotent: a second teardown cancels nothing.
        assert_eq!(reg.cancel_all(), 0);
    }

    #[test]
    fn register_after_cancel_all_starts_cancelled() {
        // Teardown race regression: once `cancel_all` has closed the
        // registry, a call dispatched in the teardown window must start
        // cancelled and must NOT linger as a live, uncancellable entry.
        let reg = CancelRegistry::default();
        assert_eq!(reg.cancel_all(), 0, "empty teardown cancels nothing");

        let id = cid();
        let token = CancellationToken::new();
        assert!(
            reg.register(id.clone(), &token),
            "register on a closed registry must report pre-cancel"
        );
        assert!(
            token.is_cancelled(),
            "a call dispatched after teardown must start cancelled"
        );
        assert_eq!(
            reg.live_count(),
            0,
            "a closed-registry register must not leave a live (orphan) entry"
        );
    }

    #[test]
    fn register_without_tombstone_does_not_cancel() {
        let reg = CancelRegistry::default();
        let id = cid();
        let token = CancellationToken::new();
        assert!(!reg.register(id, &token));
        assert!(
            !token.is_cancelled(),
            "a clean registration must leave the token live"
        );
    }

    #[test]
    fn cancel_all_clears_pending_tombstones() {
        let reg = CancelRegistry::default();
        reg.cancel(&cid());
        reg.cancel(&cid());
        assert_eq!(reg.pending_count(), 2);
        reg.cancel_all();
        assert_eq!(
            reg.pending_count(),
            0,
            "teardown must drop pending tombstones"
        );
    }

    #[test]
    fn pending_tombstones_stay_bounded_under_spurious_cancels() {
        // A long-lived session that keeps receiving cancels for call_ids
        // that never register (e.g. cancels racing call completion) must
        // not grow `pending` without bound.
        let reg = CancelRegistry::default();
        for _ in 0..(MAX_PENDING_TOMBSTONES + 256) {
            assert!(!reg.cancel(&cid()), "never-registered id is a miss");
        }
        assert!(
            reg.pending_count() <= MAX_PENDING_TOMBSTONES,
            "tombstone set must stay within its cap, got {}",
            reg.pending_count()
        );
    }
}
