//! Session-scoped query, inspection, and progress delivery.

use std::sync::Arc;

use tokio::sync::oneshot;

use super::super::coordinator_state::{
    BlockingWaiter, CompletedChild, ListRequest, OUTPUT_UNAVAILABLE_PLACEHOLDER, ProgressFuture,
    ProgressTarget, RunningSeed, completed_inspection, completed_snapshot, pending_inspection,
    pending_snapshot, queued_inspection, queued_snapshot, running_inspection, running_seed,
};
use super::super::types::{SubagentInspection, SubagentSnapshot};
use super::{ChildControl, ChildRunner, SubagentCoordinator, SubagentProgress, belongs_to_session};

const DEFAULT_QUERY_BLOCK_TIMEOUT_MS: u64 = 30_000;

impl<R: ChildRunner> SubagentCoordinator<R> {
    fn push_blocking_waiter(
        &mut self,
        id: String,
        timeout_ms: Option<u64>,
        respond_to: oneshot::Sender<Option<SubagentSnapshot>>,
    ) {
        self.waiters.entry(id).or_default().push(BlockingWaiter {
            deadline: tokio::time::Instant::now()
                + std::time::Duration::from_millis(
                    timeout_ms.unwrap_or(DEFAULT_QUERY_BLOCK_TIMEOUT_MS),
                ),
            respond_to,
        });
    }

    pub(super) fn handle_query(
        &mut self,
        id: String,
        parent_session_id: Option<String>,
        block: bool,
        timeout_ms: Option<u64>,
        respond_to: oneshot::Sender<Option<SubagentSnapshot>>,
    ) {
        if let Some(child) = self
            .completed
            .get(&id)
            .filter(|child| belongs_to_session(&child.request, parent_session_id.as_deref()))
        {
            let snapshot = (!child.request.owner.is_workflow())
                .then(|| self.completed_snapshot_for_query(child));
            let _ = respond_to.send(snapshot);
            return;
        }
        if let Some(child) = self
            .active
            .get(&id)
            .filter(|child| belongs_to_session(&child.request, parent_session_id.as_deref()))
        {
            if child.request.owner.is_workflow() {
                let _ = respond_to.send(None);
                return;
            }
            if block {
                self.push_blocking_waiter(id, timeout_ms, respond_to);
            } else {
                self.queue_active_progress(&id, ProgressTarget::Query(respond_to));
            }
            return;
        }
        if let Some(child) = self
            .pending
            .get(&id)
            .filter(|child| belongs_to_session(&child.request, parent_session_id.as_deref()))
        {
            if child.request.owner.is_workflow() {
                let _ = respond_to.send(None);
                return;
            }
            if block {
                self.push_blocking_waiter(id, timeout_ms, respond_to);
            } else {
                let _ = respond_to.send(Some(pending_snapshot(child)));
            }
            return;
        }
        if let Some(queued) = self.queued.iter().find(|queued| {
            queued.request.id == id
                && belongs_to_session(&queued.request, parent_session_id.as_deref())
        }) {
            if block {
                self.push_blocking_waiter(id, timeout_ms, respond_to);
            } else {
                let _ = respond_to.send(Some(queued_snapshot(
                    &queued.request,
                    queued.queued_at.into_std(),
                )));
            }
            return;
        }
        let _ = respond_to.send(None);
    }

    pub(super) fn handle_inspect(
        &mut self,
        id: String,
        parent_session_id: Option<String>,
        respond_to: oneshot::Sender<Option<SubagentInspection>>,
    ) {
        if let Some(child) = self
            .completed
            .get(&id)
            .filter(|child| belongs_to_session(&child.request, parent_session_id.as_deref()))
        {
            let _ = respond_to.send(Some(self.completed_inspection_for_query(child)));
        } else if let Some(child) = self
            .pending
            .get(&id)
            .filter(|child| belongs_to_session(&child.request, parent_session_id.as_deref()))
        {
            let _ = respond_to.send(Some(pending_inspection(child)));
        } else if self
            .active
            .get(&id)
            .is_some_and(|child| belongs_to_session(&child.request, parent_session_id.as_deref()))
        {
            self.queue_active_progress(&id, ProgressTarget::Inspect(respond_to));
        } else if let Some(queued) = self.queued.iter().find(|queued| {
            queued.request.id == id
                && belongs_to_session(&queued.request, parent_session_id.as_deref())
        }) {
            let _ = respond_to.send(Some(queued_inspection(
                &queued.request,
                queued.queued_at.into_std(),
            )));
        } else {
            let _ = respond_to.send(None);
        }
    }

    fn persisted_output(&self, child: &CompletedChild) -> Option<Arc<str>> {
        child.persisted_output_ref.as_deref().map(|reference| {
            self.runner
                .load_persisted_output(reference)
                .unwrap_or_else(|| Arc::from(OUTPUT_UNAVAILABLE_PLACEHOLDER))
        })
    }

    fn completed_snapshot_for_query(&self, child: &CompletedChild) -> SubagentSnapshot {
        let output = self.persisted_output(child);
        completed_snapshot(child, output.as_deref())
    }

    fn completed_inspection_for_query(&self, child: &CompletedChild) -> SubagentInspection {
        let output = self.persisted_output(child);
        completed_inspection(child, output.as_deref())
    }

    pub(super) fn ready_snapshot(&self, id: &str) -> Option<SubagentSnapshot> {
        self.completed
            .get(id)
            .filter(|child| !child.request.owner.is_workflow())
            .map(|child| self.completed_snapshot_for_query(child))
            .or_else(|| {
                self.pending
                    .get(id)
                    .filter(|child| !child.request.owner.is_workflow())
                    .map(pending_snapshot)
            })
            .or_else(|| {
                self.queued
                    .iter()
                    // Workflow spawns never queue; the filter matches the
                    // completed/pending arms above should that ever bend.
                    .find(|queued| queued.request.id == id && !queued.request.owner.is_workflow())
                    .map(|queued| queued_snapshot(&queued.request, queued.queued_at.into_std()))
            })
    }

    pub(super) fn handle_list_running(
        &mut self,
        parent_session_id: String,
        respond_to: oneshot::Sender<Vec<SubagentInspection>>,
    ) {
        let ids: Vec<_> = self
            .active
            .values()
            .filter(|child| {
                child.request.parent_session_id == parent_session_id
                    && !child.request.owner.is_workflow()
            })
            .map(|child| child.request.id.clone())
            .collect();
        if ids.is_empty() {
            let _ = respond_to.send(Vec::new());
            return;
        }

        let request_id = self.next_list_request_id;
        self.next_list_request_id = self.next_list_request_id.wrapping_add(1);
        self.list_requests.insert(
            request_id,
            ListRequest {
                slots: vec![None; ids.len()],
                remaining: ids.len(),
                respond_to,
            },
        );
        for (index, id) in ids.into_iter().enumerate() {
            self.queue_active_progress(&id, ProgressTarget::List { request_id, index });
        }
    }

    pub(super) fn queue_active_progress(&mut self, id: &str, target: ProgressTarget) {
        let Some(child) = self.active.get(id) else {
            match target {
                ProgressTarget::Query(tx) => {
                    let _ = tx.send(self.ready_snapshot(id));
                }
                ProgressTarget::Inspect(tx) => {
                    let value = self
                        .completed
                        .get(id)
                        .map(|child| self.completed_inspection_for_query(child));
                    let _ = tx.send(value);
                }
                ProgressTarget::List { request_id, index } => {
                    self.finish_list_slot(request_id, index, None);
                }
            }
            return;
        };
        self.progress.push(ProgressFuture {
            future: Box::pin(child.control.progress()),
            seed: Some(running_seed(child)),
            target: Some(target),
        });
    }

    pub(super) fn finish_progress(
        &mut self,
        seed: RunningSeed,
        target: ProgressTarget,
        progress: SubagentProgress,
    ) {
        let still_active = self.active.contains_key(&seed.subagent_id);
        if !still_active {
            match target {
                ProgressTarget::Query(respond_to) => {
                    let _ = respond_to.send(self.ready_snapshot(&seed.subagent_id));
                }
                ProgressTarget::Inspect(respond_to) => {
                    let value = self
                        .completed
                        .get(&seed.subagent_id)
                        .map(|child| self.completed_inspection_for_query(child));
                    let _ = respond_to.send(value);
                }
                ProgressTarget::List { request_id, index } => {
                    self.finish_list_slot(request_id, index, None);
                }
            }
            return;
        }
        let inspection = running_inspection(seed, progress);
        match target {
            ProgressTarget::Query(respond_to) => {
                let _ = respond_to.send(Some(inspection.snapshot));
            }
            ProgressTarget::Inspect(respond_to) => {
                let _ = respond_to.send(Some(inspection));
            }
            ProgressTarget::List { request_id, index } => {
                self.finish_list_slot(request_id, index, Some(inspection));
            }
        }
    }

    fn finish_list_slot(
        &mut self,
        request_id: u64,
        index: usize,
        inspection: Option<SubagentInspection>,
    ) {
        let Some(request) = self.list_requests.get_mut(&request_id) else {
            return;
        };
        request.slots[index] = inspection;
        request.remaining = request.remaining.saturating_sub(1);
        if request.remaining != 0 {
            return;
        }
        let Some(request) = self.list_requests.remove(&request_id) else {
            return;
        };
        let values = request.slots.into_iter().flatten().collect();
        let _ = request.respond_to.send(values);
    }
}
