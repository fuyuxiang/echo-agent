//! Reports `StopFailure` and `StopCancelled`, the two turn-end hooks that run instead of `Stop`.
//! A reporter claims the turn's one report and hands the payload to a background worker, so an
//! interrupt cannot abort a hook already running. The gate owns the third report, `Stop`.

use super::*;
use echo_agent_hooks::event::{self, StopCancelledReason, StopFailureKind};

/// This path's slice of the session's ten-second exit budget, spent twice per teardown: once by
/// `flush` and once by `drain`.
const TURN_END_DRAIN_BUDGET: std::time::Duration = std::time::Duration::from_millis(250);

#[derive(Debug)]
pub(super) enum TurnEnd {
    Failed {
        error: StopFailureKind,
        error_details: Option<String>,
        last_assistant_message: Option<String>,
    },
    Cancelled {
        reason: StopCancelledReason,
        trigger: Option<String>,
        reason_details: Option<String>,
        last_assistant_message: Option<String>,
    },
}

impl TurnEnd {
    fn event_name(&self) -> event::HookEventName {
        match self {
            Self::Failed { .. } => event::HookEventName::StopFailure,
            Self::Cancelled { .. } => event::HookEventName::StopCancelled,
        }
    }

    fn is_user_interrupt(&self) -> bool {
        matches!(
            self,
            Self::Cancelled {
                reason: StopCancelledReason::UserInterrupt,
                ..
            }
        )
    }

    fn into_payload(self, subagent_type: Option<String>) -> event::HookPayload {
        let clip = |text: Option<&str>| text.map(event::clip_stop_entry_text);
        let clip_message = |text: Option<&str>| text.map(event::clip_assistant_message);
        match self {
            Self::Failed {
                error,
                error_details,
                last_assistant_message,
            } => event::HookPayload::StopFailure {
                error,
                error_details: clip(error_details.as_deref()),
                last_assistant_message: clip_message(last_assistant_message.as_deref()),
                subagent_type,
            },
            Self::Cancelled {
                reason,
                trigger,
                reason_details,
                last_assistant_message,
            } => event::HookPayload::StopCancelled {
                reason,
                cancelled_by: reason.cancelled_by(),
                cancel_trigger: trigger
                    .as_deref()
                    .map(|t| event::clip_text(t, event::MAX_CANCEL_TRIGGER_CHARS)),
                reason_details: clip(reason_details.as_deref()),
                last_assistant_message: clip_message(last_assistant_message.as_deref()),
                subagent_type,
            },
        }
    }
}

/// Only [`Self::Queued`] consumes the turn's one report. The refusals are distinct so a test
/// can tell them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReportOutcome {
    Queued,
    /// The parent's cancel reaching a child; the parent reports it.
    InheritedInterrupt,
    NoListener,
    /// Another site reported this turn, or a later turn superseded it.
    AlreadyReported,
    /// Teardown closed the queue; the report was dropped.
    QueueClosed,
}

/// Barriers share the channel with reports, so awaiting one awaits everything queued ahead.
pub(crate) enum QueueItem {
    Report(Box<TurnEndReport>),
    Barrier(tokio::sync::oneshot::Sender<()>),
}

#[derive(Debug)]
#[must_use = "queue this report, or the turn end is never delivered"]
pub(crate) struct TurnEndReport {
    prompt_id: String,
    event: event::HookEventName,
    payload: event::HookPayload,
}

pub(super) fn cancel_reason_for_options(
    options: &crate::session::CancelOptions,
) -> Option<StopCancelledReason> {
    use crate::session::CancelKind;
    match options
        .trigger
        .as_ref()
        .map(crate::session::CancelTrigger::kind)
    {
        Some(CancelKind::StopGesture) => Some(StopCancelledReason::UserInterrupt),
        Some(CancelKind::Replace | CancelKind::Teardown) => None,
        None if options.user_initiated => Some(StopCancelledReason::UserInterrupt),
        None => {
            // Unreachable today: every production cancel names a trigger or a user.
            tracing::debug!("this cancel names neither a trigger nor a user; nothing reports it");
            None
        }
    }
}

/// `StationarityEnded` reports too: it is a completion kind, but it never runs the gate, so
/// `Stop` cannot claim the turn and `StopCancelled` names why the agent stopped.
pub(super) fn cancel_reason_for_completion(
    kind: &PromptCompletionKind,
) -> Option<StopCancelledReason> {
    use crate::session::events::CancellationCategory as Cat;
    match kind {
        PromptCompletionKind::Cancelled { category, .. } => Some(match category {
            Some(Cat::PermissionRejected) => StopCancelledReason::PermissionRejected,
            Some(Cat::PermissionCancelled) => StopCancelledReason::PermissionCancelled,
            Some(Cat::MidTurnAbort) => StopCancelledReason::UserInterrupt,
            Some(Cat::HookDenied) => {
                tracing::debug!("a hook denial ended a turn; reporting it as unclassified");
                StopCancelledReason::Unknown
            }
            None => StopCancelledReason::Unknown,
        }),
        PromptCompletionKind::MaxTurnsReached { .. } => Some(StopCancelledReason::MaxTurns),
        PromptCompletionKind::StationarityEnded => Some(StopCancelledReason::NoProgress),
        // `Completed` fires `Stop`; the other two never ran a turn.
        PromptCompletionKind::Completed
        | PromptCompletionKind::Rewound
        | PromptCompletionKind::RemovedFromQueue => None,
    }
}

pub(super) fn cancel_details(kind: &PromptCompletionKind) -> Option<String> {
    let PromptCompletionKind::Cancelled {
        context: Some(ctx), ..
    } = kind
    else {
        return None;
    };
    let subject = ctx.tool_name.as_ref().or(ctx.hook_name.as_ref());
    match (subject, &ctx.reason) {
        (Some(subject), Some(reason)) => Some(format!("{subject}: {reason}")),
        (Some(subject), None) => Some(subject.clone()),
        (None, reason) => reason.clone(),
    }
}

/// One worker, so reports keep their order and teardown has one thing to wait on.
#[must_use = "drain this queue, or the turn-end hooks still in it never run"]
pub(super) struct TurnEndQueue {
    session: Arc<SessionActor>,
    /// This queue's own sender, dropped by [`Self::drain`]: the worker's `recv` ends only once
    /// every sender is gone. Unbounded, since the claim bounds it at one clipped payload a turn.
    tx: Option<tokio::sync::mpsc::UnboundedSender<QueueItem>>,
    worker: Option<tokio::task::JoinHandle<()>>,
}

impl TurnEndQueue {
    pub(super) fn spawn(session: Arc<SessionActor>) -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<QueueItem>();
        *session.turn_end_tx.borrow_mut() = Some(tx.clone());
        let worker = tokio::task::spawn_local({
            let session = session.clone();
            async move {
                while let Some(item) = rx.recv().await {
                    match item {
                        QueueItem::Report(report) => session.dispatch_turn_end(*report).await,
                        QueueItem::Barrier(reached) => {
                            let _ = reached.send(());
                        }
                    }
                }
            }
        });
        Self {
            session,
            tx: Some(tx),
            worker: Some(worker),
        }
    }

    /// Clears the session's sender, but only while it is still this queue's: a replaced queue
    /// must not disarm its successor.
    fn disarm(&self) {
        let Some(tx) = self.tx.as_ref() else {
            return;
        };
        let mut installed = self.session.turn_end_tx.borrow_mut();
        if installed.as_ref().is_some_and(|live| live.same_channel(tx)) {
            *installed = None;
        }
    }

    /// Waits for what is queued, leaving the queue open: teardown needs an earlier turn's report
    /// to precede the session-end `Stop`, but a `Graceful` shutdown leaves the turn running.
    pub(super) async fn flush(&mut self) {
        let (reached, wait) = tokio::sync::oneshot::channel();
        // Through this queue's own sender, so the flush and the later drain wait on one worker.
        let sent = self
            .tx
            .as_ref()
            .is_some_and(|tx| tx.send(QueueItem::Barrier(reached)).is_ok());
        if !sent {
            return;
        }
        if tokio::time::timeout(TURN_END_DRAIN_BUDGET, wait)
            .await
            .is_err()
        {
            tracing::warn!(
                budget_ms = TURN_END_DRAIN_BUDGET.as_millis(),
                "a turn-end hook is still running; the session is not waiting for it"
            );
        }
    }

    /// Closes the queue and waits for the worker. Nothing can report after this.
    pub(super) async fn drain(mut self) {
        self.disarm();
        self.tx = None;
        let Some(mut worker) = self.worker.take() else {
            return;
        };
        match tokio::time::timeout(TURN_END_DRAIN_BUDGET, &mut worker).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => tracing::error!(
                %err,
                "turn-end hooks stopped after an internal error; no more run in this session"
            ),
            Err(_) => {
                tracing::warn!(
                    budget_ms = TURN_END_DRAIN_BUDGET.as_millis(),
                    "the session exited before every turn-end hook ran"
                );
                worker.abort();
            }
        }
    }
}

impl Drop for TurnEndQueue {
    /// If the loop unwinds instead of draining, the worker still holds an `Arc<SessionActor>`.
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            // Disarm first: `abort` only schedules cancellation, so until the runtime reaps the
            // task a send still succeeds and would commit a claim nothing will ever dispatch.
            self.disarm();
            tracing::warn!("the session ended unexpectedly; any queued turn-end hooks did not run");
            worker.abort();
        }
    }
}

impl SessionActor {
    /// `None` when nothing is listening, so the cancel path skips the chat-state round trip.
    pub(super) async fn last_assistant_message_for_cancel(&self) -> Option<String> {
        if !self.has_enabled_hooks_for(event::HookEventName::StopCancelled) {
            return None;
        }
        self.chat_state_handle
            .get_last_assistant_text_in_turn()
            .await
    }

    /// Reports the running turn, reading the epoch without awaiting so no next turn can start
    /// in between.
    pub(super) fn report_turn_end(&self, prompt_id: &str, end: TurnEnd) -> ReportOutcome {
        self.claim_and_queue(prompt_id, self.turn_report.epoch(), end)
    }

    /// Reports the turn named by `epoch`, which a caller may capture before an await: if a newer
    /// turn started since, the report is refused rather than filed against it.
    pub(super) fn claim_and_queue(
        &self,
        prompt_id: &str,
        epoch: TurnEpoch,
        end: TurnEnd,
    ) -> ReportOutcome {
        if self.startup_hints.is_subagent && end.is_user_interrupt() {
            return ReportOutcome::InheritedInterrupt;
        }
        let event = end.event_name();
        // Before the claim, so a turn nobody listens for leaves the slot free for a later one.
        if !self.has_enabled_hooks_for(event) {
            return ReportOutcome::NoListener;
        }
        let Some(claim) = self.turn_report.claim_at(epoch) else {
            tracing::debug!(
                prompt_id,
                "no turn-end hook: this turn was already reported, or a newer turn replaced it"
            );
            return ReportOutcome::AlreadyReported;
        };
        let report = TurnEndReport {
            prompt_id: prompt_id.to_string(),
            event,
            payload: end.into_payload(self.subagent_type_label()),
        };
        if !self.send_queue_item(QueueItem::Report(Box::new(report))) {
            tracing::warn!(
                prompt_id,
                "the turn-end queue is closed; this turn's end hook did not run"
            );
            // The claim is released on drop, so a later reporter on this turn can still report.
            return ReportOutcome::QueueClosed;
        }
        match claim.commit() {
            CommitOutcome::Reported => ReportOutcome::Queued,
            CommitOutcome::LostToAnotherReporter => {
                debug_assert!(false, "nothing can take a claim this path just made");
                ReportOutcome::AlreadyReported
            }
        }
    }

    async fn dispatch_turn_end(&self, report: TurnEndReport) {
        self.dispatch_hook(report.event, report.payload, Some(&report.prompt_id), None)
            .await;
    }

    fn send_queue_item(&self, item: QueueItem) -> bool {
        self.turn_end_tx
            .borrow()
            .as_ref()
            .is_some_and(|tx| tx.send(item).is_ok())
    }
}

#[cfg(test)]
#[path = "turn_end_hooks_tests.rs"]
mod tests;
