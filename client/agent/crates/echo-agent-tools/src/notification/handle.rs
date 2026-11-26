use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::types::{
    BashExecutionBackgrounded, BashExecutionComplete, BashExecutionFailed, BashExecutionTimeout,
    BashOutputChunk, FileWritten, LspServerCrashed, LspServerFailed, LspServerReady,
    LspServerRetrying, LspServerStarting, MonitorEvent, PlanModeEntered, PlanModeExited,
    ScheduledTaskCreated, ScheduledTaskFired, ScheduledTaskRemoved, SubagentCompleted,
    ToolNotification, UserQuestionAsked,
};
use crate::types::TaskSnapshot;

/// Envelope for consumers that can acknowledge durable notification handling.
pub struct AcknowledgedToolNotification {
    /// Notification delivered in the same FIFO as unacknowledged events.
    pub notification: ToolNotification,
    /// Completion sender present only when the producer requested acknowledgement.
    pub acknowledgement: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
}

/// Failure reported after all acknowledged notification targets have settled.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum NotificationAcknowledgementError {
    #[error("{0} acknowledging notification target(s) closed during dispatch")]
    DispatchClosed(usize),
    #[error("{0} notification acknowledgement(s) were dropped")]
    AcknowledgementDropped(usize),
    #[error("notification consumer rejected delivery: {0:?}")]
    ConsumerRejected(Vec<String>),
    #[error(
        "notification acknowledgement failed: {dispatch_closed} dispatch closed, {acknowledgements_dropped} acknowledgement(s) dropped, consumer rejections: {consumer_rejections:?}"
    )]
    Multiple {
        dispatch_closed: usize,
        acknowledgements_dropped: usize,
        consumer_rejections: Vec<String>,
    },
}

/// Receipts for one acknowledged fan-out operation.
#[must_use = "acknowledged notification receipts must be awaited"]
pub struct NotificationAcknowledgementBatch {
    receipts: Vec<tokio::sync::oneshot::Receiver<Result<(), String>>>,
    dispatch_closed: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurableNotificationTargets {
    None,
    Present,
}

impl NotificationAcknowledgementBatch {
    /// Wait for every live durable target and report all observed failure classes.
    pub async fn wait(self) -> Result<(), NotificationAcknowledgementError> {
        let mut acknowledgements_dropped = 0;
        let mut consumer_rejections = Vec::new();
        for receipt in self.receipts {
            match receipt.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => consumer_rejections.push(error),
                Err(_) => acknowledgements_dropped += 1,
            }
        }
        match (
            self.dispatch_closed,
            acknowledgements_dropped,
            consumer_rejections.is_empty(),
        ) {
            (0, 0, true) => Ok(()),
            (dispatch_closed, 0, true) => Err(NotificationAcknowledgementError::DispatchClosed(
                dispatch_closed,
            )),
            (0, acknowledgements_dropped, true) => Err(
                NotificationAcknowledgementError::AcknowledgementDropped(acknowledgements_dropped),
            ),
            (0, 0, false) => Err(NotificationAcknowledgementError::ConsumerRejected(
                consumer_rejections,
            )),
            (dispatch_closed, acknowledgements_dropped, _) => {
                Err(NotificationAcknowledgementError::Multiple {
                    dispatch_closed,
                    acknowledgements_dropped,
                    consumer_rejections,
                })
            }
        }
    }
}

#[derive(Clone)]
enum ToolNotificationTarget {
    Plain(tokio::sync::mpsc::UnboundedSender<ToolNotification>),
    Bounded(tokio::sync::mpsc::Sender<ToolNotification>),
    Capped(Arc<CappedNotificationQueue>),
    Acknowledged(tokio::sync::mpsc::UnboundedSender<AcknowledgedToolNotification>),
}

struct CappedNotificationQueue {
    queue: parking_lot::Mutex<VecDeque<ToolNotification>>,
    capacity: usize,
    closed: AtomicBool,
    ready: tokio::sync::Notify,
}

impl CappedNotificationQueue {
    fn push(&self, notification: ToolNotification) {
        if self.closed.load(Ordering::Relaxed) {
            return;
        }
        let mut queue = self.queue.lock();
        if queue.len() >= self.capacity {
            if !is_critical_notification(&notification) {
                tracing::warn!("tool notification queue full; dropping newest lossy event");
                return;
            }
            let evict = queue
                .iter()
                .position(|queued| !is_critical_notification(queued))
                .unwrap_or(0);
            queue.remove(evict);
            tracing::warn!("tool notification queue full; evicting older event for terminal event");
        }
        queue.push_back(notification);
        drop(queue);
        self.ready.notify_one();
    }
}

fn is_critical_notification(notification: &ToolNotification) -> bool {
    matches!(
        notification,
        ToolNotification::BashExecutionComplete(_)
            | ToolNotification::BashExecutionTimeout(_)
            | ToolNotification::BashExecutionFailed(_)
            | ToolNotification::TaskCompleted(_)
            | ToolNotification::SubagentCompleted(_)
            | ToolNotification::PlanModeEntered(_)
            | ToolNotification::PlanModeExited(_)
            | ToolNotification::UserQuestionAsked(_)
            | ToolNotification::LspServerCrashed(_)
            | ToolNotification::LspServerFailed(_)
            | ToolNotification::ScheduledTaskFired(_)
            | ToolNotification::ScheduledTaskRemoved(_)
    )
}

/// Receiver for a capped queue that preserves terminal notifications.
pub struct CappedToolNotificationReceiver {
    queue: Arc<CappedNotificationQueue>,
}

impl CappedToolNotificationReceiver {
    pub async fn recv(&mut self) -> Option<ToolNotification> {
        loop {
            let ready = self.queue.ready.notified();
            if let Some(notification) = self.queue.queue.lock().pop_front() {
                return Some(notification);
            }
            if self.queue.closed.load(Ordering::Relaxed) {
                return None;
            }
            ready.await;
        }
    }
}

impl Drop for CappedToolNotificationReceiver {
    fn drop(&mut self) {
        self.queue.closed.store(true, Ordering::Relaxed);
        self.queue.ready.notify_waiters();
    }
}

/// Cloneable notification fan-out with per-target FIFO ordering.
#[derive(Clone)]
pub struct ToolNotificationHandle {
    targets: Arc<[ToolNotificationTarget]>,
}

impl Default for ToolNotificationHandle {
    fn default() -> Self {
        Self::noop()
    }
}

macro_rules! convenience_sends {
    ($($method:ident, $ty:ty, $variant:ident);+ $(;)?) => {
        $(pub fn $method(&self, value: $ty) { self.send(ToolNotification::$variant(value)); })+
    };
}

impl ToolNotificationHandle {
    pub fn new(sender: tokio::sync::mpsc::UnboundedSender<ToolNotification>) -> Self {
        Self {
            targets: Arc::from([ToolNotificationTarget::Plain(sender)]),
        }
    }

    pub fn from_sender(sender: tokio::sync::mpsc::UnboundedSender<ToolNotification>) -> Self {
        Self::new(sender)
    }

    pub fn channel() -> (Self, tokio::sync::mpsc::UnboundedReceiver<ToolNotification>) {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        (Self::new(sender), receiver)
    }

    /// Create a capped target that drops the newest event when full.
    pub fn bounded_channel(
        capacity: usize,
    ) -> (Self, tokio::sync::mpsc::Receiver<ToolNotification>) {
        let (sender, receiver) = tokio::sync::mpsc::channel(capacity);
        (
            Self {
                targets: Arc::from([ToolNotificationTarget::Bounded(sender)]),
            },
            receiver,
        )
    }

    /// Create a capped queue that evicts lossy events before terminal events.
    pub fn capped_channel(capacity: usize) -> (Self, CappedToolNotificationReceiver) {
        let queue = Arc::new(CappedNotificationQueue {
            queue: parking_lot::Mutex::new(VecDeque::new()),
            capacity: capacity.max(1),
            closed: AtomicBool::new(false),
            ready: tokio::sync::Notify::new(),
        });
        (
            Self {
                targets: Arc::from([ToolNotificationTarget::Capped(Arc::clone(&queue))]),
            },
            CappedToolNotificationReceiver { queue },
        )
    }

    pub fn acknowledged_channel() -> (
        Self,
        tokio::sync::mpsc::UnboundedReceiver<AcknowledgedToolNotification>,
    ) {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        (
            Self {
                targets: Arc::from([ToolNotificationTarget::Acknowledged(sender)]),
            },
            receiver,
        )
    }

    pub fn noop() -> Self {
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        Self::new(sender)
    }

    /// Combine handles while preserving each target's send order.
    pub fn tee(handles: Vec<ToolNotificationHandle>) -> ToolNotificationHandle {
        let targets = handles
            .iter()
            .flat_map(|handle| handle.targets.iter().cloned())
            .collect::<Vec<_>>();
        Self {
            targets: Arc::from(targets),
        }
    }

    pub(crate) fn durable_targets(&self) -> DurableNotificationTargets {
        if self.targets.iter().any(|target| {
            matches!(target, ToolNotificationTarget::Acknowledged(sender) if !sender.is_closed())
        }) {
            DurableNotificationTargets::Present
        } else {
            DurableNotificationTargets::None
        }
    }

    pub fn send(&self, notification: ToolNotification) {
        let last = self.targets.len().saturating_sub(1);
        let mut notification = Some(notification);
        for (index, target) in self.targets.iter().enumerate() {
            let notification = if index == last {
                let Some(notification) = notification.take() else {
                    break;
                };
                notification
            } else {
                let Some(notification) = notification.as_ref() else {
                    break;
                };
                notification.clone()
            };
            match target {
                ToolNotificationTarget::Plain(target) => {
                    let _ = target.send(notification);
                }
                ToolNotificationTarget::Bounded(target) => {
                    if target.try_send(notification).is_err() {
                        tracing::warn!("tool notification queue full; dropping newest event");
                    }
                }
                ToolNotificationTarget::Capped(target) => target.push(notification),
                ToolNotificationTarget::Acknowledged(target) => {
                    let _ = target.send(AcknowledgedToolNotification {
                        notification,
                        acknowledgement: None,
                    });
                }
            }
        }
    }

    /// Send a removal to every target and collect all durable acknowledgements.
    pub fn send_scheduled_task_removed_acknowledged(
        &self,
        removed: ScheduledTaskRemoved,
    ) -> NotificationAcknowledgementBatch {
        let notification = ToolNotification::ScheduledTaskRemoved(removed);
        let mut batch = NotificationAcknowledgementBatch {
            receipts: Vec::new(),
            dispatch_closed: 0,
        };
        for target in self.targets.iter() {
            match target {
                ToolNotificationTarget::Plain(target) => {
                    let _ = target.send(notification.clone());
                }
                ToolNotificationTarget::Bounded(target) => {
                    if target.try_send(notification.clone()).is_err() {
                        tracing::warn!("tool notification queue full; dropping newest event");
                    }
                }
                ToolNotificationTarget::Capped(target) => target.push(notification.clone()),
                ToolNotificationTarget::Acknowledged(target) => {
                    let (acknowledgement, receipt) = tokio::sync::oneshot::channel();
                    if target
                        .send(AcknowledgedToolNotification {
                            notification: notification.clone(),
                            acknowledgement: Some(acknowledgement),
                        })
                        .is_ok()
                    {
                        batch.receipts.push(receipt);
                    } else {
                        batch.dispatch_closed += 1;
                    }
                }
            }
        }
        batch
    }

    convenience_sends! {
        send_output_chunk, BashOutputChunk, BashOutputChunk;
        send_complete, BashExecutionComplete, BashExecutionComplete;
        send_timeout, BashExecutionTimeout, BashExecutionTimeout;
        send_backgrounded, BashExecutionBackgrounded, BashExecutionBackgrounded;
        send_failed, BashExecutionFailed, BashExecutionFailed;
        send_file_written, FileWritten, FileWritten;
        send_task_complete, TaskSnapshot, TaskCompleted;
        send_subagent_completed, SubagentCompleted, SubagentCompleted;
        send_plan_mode_entered, PlanModeEntered, PlanModeEntered;
        send_plan_mode_exited, PlanModeExited, PlanModeExited;
        send_user_question_asked, UserQuestionAsked, UserQuestionAsked;
        send_lsp_starting, LspServerStarting, LspServerStarting;
        send_lsp_ready, LspServerReady, LspServerReady;
        send_lsp_crashed, LspServerCrashed, LspServerCrashed;
        send_lsp_retrying, LspServerRetrying, LspServerRetrying;
        send_lsp_failed, LspServerFailed, LspServerFailed;
        send_scheduled_task_fired, ScheduledTaskFired, ScheduledTaskFired;
        send_scheduled_task_removed, ScheduledTaskRemoved, ScheduledTaskRemoved;
        send_scheduled_task_created, ScheduledTaskCreated, ScheduledTaskCreated;
        send_monitor_event, MonitorEvent, MonitorEvent;
    }
}

/// Per-call notification override applied in addition to the session-wide sink.
#[derive(Clone)]
pub struct PerCallNotificationSink(pub ToolNotificationHandle);

#[cfg(test)]
#[path = "handle_tests.rs"]
mod tests;
