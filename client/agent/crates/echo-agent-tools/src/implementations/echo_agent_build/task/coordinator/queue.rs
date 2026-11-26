//! Spawns parked at the session concurrent limit, and their dequeue path.

use std::collections::VecDeque;

use tokio::sync::oneshot;

use super::super::types::{SubagentRequest, SubagentResult};
use super::{ChildRunner, SubagentCoordinator};

/// Cancelling a queued spawn's token schedules no wake of its own; sweeps
/// run at least this often while spawns are parked.
pub(super) const QUEUED_REAP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// The session admission queue: spawns parked at the concurrent limit, in
/// arrival order. Every queue convention lives behind these methods.
#[derive(Default)]
pub(super) struct SpawnQueue {
    entries: VecDeque<QueuedSpawn>,
}

impl SpawnQueue {
    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn push_back(&mut self, entry: QueuedSpawn) {
        self.entries.push_back(entry);
    }

    pub(super) fn contains_id(&self, id: &str) -> bool {
        self.entries.iter().any(|queued| queued.request.id == id)
    }

    pub(super) fn count_for_session(&self, parent_session_id: &str) -> usize {
        self.entries
            .iter()
            .filter(|queued| queued.request.parent_session_id == parent_session_id)
            .count()
    }

    pub(super) fn iter(&self) -> std::collections::vec_deque::Iter<'_, QueuedSpawn> {
        self.entries.iter()
    }

    pub(super) fn iter_mut(&mut self) -> std::collections::vec_deque::IterMut<'_, QueuedSpawn> {
        self.entries.iter_mut()
    }
}

/// A spawn parked at the session concurrent limit.
pub(super) struct QueuedSpawn {
    pub(super) request: Box<SubagentRequest>,
    /// Tokio clock so paused-clock tests can assert the wait.
    pub(super) queued_at: tokio::time::Instant,
    pub(super) caller: QueuedCaller,
}

pub(super) enum QueuedCaller {
    /// `deadline` is the caller's await budget, running from enqueue and
    /// carried through start.
    Awaiting {
        result_tx: oneshot::Sender<SubagentResult>,
        deadline: Option<tokio::time::Instant>,
    },
    /// The queued analog of `handle_only`: the caller was already handed off.
    Backgrounded,
}

impl QueuedCaller {
    pub(super) fn deadline(&self) -> Option<tokio::time::Instant> {
        match self {
            Self::Awaiting { deadline, .. } => *deadline,
            Self::Backgrounded => None,
        }
    }

    pub(super) fn is_backgrounded(&self) -> bool {
        matches!(self, Self::Backgrounded)
    }

    pub(super) fn into_spawn_reply(self) -> Option<oneshot::Sender<SubagentResult>> {
        match self {
            Self::Awaiting { result_tx, .. } => Some(result_tx),
            Self::Backgrounded => None,
        }
    }
}

pub(super) enum StartOrigin {
    Direct,
    Dequeued {
        queued_for: std::time::Duration,
        deadline: Option<tokio::time::Instant>,
    },
}

impl<R: ChildRunner> SubagentCoordinator<R> {
    pub(super) fn session_queued_count(&self, parent_session_id: &str) -> usize {
        self.queued.count_for_session(parent_session_id)
    }

    pub(super) fn start_queued_within_capacity(&mut self) {
        // Finishing a cancelled entry below re-enters through `finish_child`;
        // the latch makes that inner sweep a no-op instead of a recursion
        // (a cancelled entry frees no running slot, so it admits nothing).
        if self.draining_queued {
            return;
        }
        self.draining_queued = true;
        let mut cancelled = Vec::new();
        let mut kept = VecDeque::new();
        for queued in std::mem::take(&mut self.queued.entries) {
            if queued.request.cancel_token.is_cancelled() {
                cancelled.push(queued);
                continue;
            }
            // A session at capacity must not block other sessions' spawns.
            let running = self.session_running_count(&queued.request.parent_session_id);
            if self.admission.has_capacity(running) {
                let QueuedSpawn {
                    request,
                    queued_at,
                    caller,
                } = queued;
                let (spawn_reply, deadline) = match caller {
                    QueuedCaller::Awaiting {
                        result_tx,
                        deadline,
                    } => (Some(result_tx), deadline),
                    QueuedCaller::Backgrounded => (None, None),
                };
                self.start_child(
                    *request,
                    spawn_reply,
                    StartOrigin::Dequeued {
                        queued_for: queued_at.elapsed(),
                        deadline,
                    },
                );
            } else {
                kept.push_back(queued);
            }
        }
        self.queued.entries = kept;
        for queued in cancelled {
            self.finish_cancelled_queued(queued);
        }
        self.draining_queued = false;
    }

    pub(super) fn remove_queued(
        &mut self,
        mut matches: impl FnMut(&SubagentRequest) -> bool,
    ) -> usize {
        let (removed, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.queued.entries)
            .into_iter()
            .partition(|queued| matches(&queued.request));
        self.queued.entries = kept.into();
        let count = removed.len();
        for queued in removed {
            self.finish_cancelled_queued(queued);
        }
        count
    }

    /// Resolve a spawn cancelled while still queued: waiters resolve instead
    /// of timing out and the id stays queryable as a cancelled record.
    fn finish_cancelled_queued(&mut self, queued: QueuedSpawn) {
        let QueuedSpawn {
            request,
            caller,
            queued_at,
        } = queued;
        // Token observers must see command-path cancels too.
        request.cancel_token.cancel();
        let result = cancelled_while_queued_result(&request);
        self.finish_never_started(
            *request,
            caller.into_spawn_reply(),
            result,
            queued_at.into_std(),
        );
    }

    /// The destructor's queue drain: resolve awaiting callers and cancel
    /// tokens without touching child records or the host runner — the host
    /// may already be tearing down when the actor future is dropped.
    pub(super) fn resolve_queued_at_drop(&mut self) {
        for queued in std::mem::take(&mut self.queued.entries) {
            queued.request.cancel_token.cancel();
            if let Some(result_tx) = queued.caller.into_spawn_reply() {
                let _ = result_tx.send(cancelled_while_queued_result(&queued.request));
            }
        }
    }
}

fn cancelled_while_queued_result(request: &SubagentRequest) -> SubagentResult {
    SubagentResult {
        success: false,
        cancelled: true,
        error: Some("cancelled while queued for a subagent slot".to_owned()),
        subagent_id: request.id.clone(),
        child_session_id: request.id.clone(),
        ..Default::default()
    }
}
