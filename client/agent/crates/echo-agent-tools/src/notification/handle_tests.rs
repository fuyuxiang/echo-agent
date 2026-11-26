use super::*;
use crate::notification::ScheduledTaskRemovedReason;

fn removed(task_id: &str) -> ScheduledTaskRemoved {
    ScheduledTaskRemoved {
        task_id: task_id.into(),
        reason: ScheduledTaskRemovedReason::Deleted,
        generation: String::new(),
        revision: 0,
    }
}

fn created(task_id: &str) -> ScheduledTaskCreated {
    ScheduledTaskCreated {
        task_id: task_id.into(),
        prompt: task_id.into(),
        human_schedule: "every 5 minutes".into(),
        next_fire_at: None,
        generation: String::new(),
        revision: 0,
    }
}

fn task_id(notification: &ToolNotification) -> &str {
    match notification {
        ToolNotification::ScheduledTaskCreated(value) => &value.task_id,
        ToolNotification::ScheduledTaskRemoved(value) => &value.task_id,
        other => panic!("unexpected notification: {other:?}"),
    }
}

#[tokio::test]
async fn acknowledged_removal_stays_in_fifo() {
    let (handle, mut receiver) = ToolNotificationHandle::acknowledged_channel();
    handle.send_scheduled_task_created(created("before"));
    let batch = handle.send_scheduled_task_removed_acknowledged(removed("deleted"));
    handle.send_scheduled_task_created(created("after"));

    let first = receiver.recv().await.unwrap();
    assert_eq!(task_id(&first.notification), "before");
    assert!(first.acknowledgement.is_none());
    let second = receiver.recv().await.unwrap();
    assert_eq!(task_id(&second.notification), "deleted");
    second.acknowledgement.unwrap().send(Ok(())).unwrap();
    let third = receiver.recv().await.unwrap();
    assert_eq!(task_id(&third.notification), "after");
    assert!(third.acknowledgement.is_none());

    batch.wait().await.unwrap();
}

#[tokio::test]
async fn mixed_fanout_attempts_every_target_before_reporting_closed_dispatch() {
    let (closed, closed_rx) = ToolNotificationHandle::acknowledged_channel();
    drop(closed_rx);
    let (plain, mut plain_rx) = ToolNotificationHandle::channel();
    let (durable, mut durable_rx) = ToolNotificationHandle::acknowledged_channel();
    let handle = ToolNotificationHandle::tee(vec![closed, plain, durable]);

    handle.send_scheduled_task_created(created("before"));
    let batch = handle.send_scheduled_task_removed_acknowledged(removed("deleted"));
    handle.send_scheduled_task_created(created("after"));

    assert_eq!(task_id(&plain_rx.recv().await.unwrap()), "before");
    assert_eq!(task_id(&plain_rx.recv().await.unwrap()), "deleted");
    assert_eq!(task_id(&plain_rx.recv().await.unwrap()), "after");
    let durable_before = durable_rx.recv().await.unwrap();
    assert_eq!(task_id(&durable_before.notification), "before");
    let durable_removed = durable_rx.recv().await.unwrap();
    assert_eq!(task_id(&durable_removed.notification), "deleted");
    durable_removed
        .acknowledgement
        .unwrap()
        .send(Ok(()))
        .unwrap();
    let durable_after = durable_rx.recv().await.unwrap();
    assert_eq!(task_id(&durable_after.notification), "after");

    assert_eq!(
        batch.wait().await,
        Err(NotificationAcknowledgementError::DispatchClosed(1))
    );
}

#[tokio::test]
async fn batch_distinguishes_dropped_and_rejected_acknowledgements() {
    let (dropped, mut dropped_rx) = ToolNotificationHandle::acknowledged_channel();
    let (rejected, mut rejected_rx) = ToolNotificationHandle::acknowledged_channel();
    let handle = ToolNotificationHandle::tee(vec![dropped, rejected]);
    let batch = handle.send_scheduled_task_removed_acknowledged(removed("deleted"));

    drop(dropped_rx.recv().await.unwrap().acknowledgement);
    rejected_rx
        .recv()
        .await
        .unwrap()
        .acknowledgement
        .unwrap()
        .send(Err("rejected".into()))
        .unwrap();

    assert_eq!(
        batch.wait().await,
        Err(NotificationAcknowledgementError::Multiple {
            dispatch_closed: 0,
            acknowledgements_dropped: 1,
            consumer_rejections: vec!["rejected".into()],
        })
    );
}

#[tokio::test]
async fn bounded_channel_drops_newest_when_full() {
    let (handle, mut receiver) = ToolNotificationHandle::bounded_channel(1);
    handle.send_scheduled_task_created(created("kept"));
    handle.send_scheduled_task_created(created("dropped"));

    assert_eq!(task_id(&receiver.recv().await.unwrap()), "kept");
    assert!(receiver.try_recv().is_err());
}

#[tokio::test]
async fn capped_channel_evicts_lossy_event_for_terminal_event() {
    let (handle, mut receiver) = ToolNotificationHandle::capped_channel(1);
    handle.send_scheduled_task_created(created("lossy"));
    handle.send(ToolNotification::ScheduledTaskRemoved(removed("terminal")));

    assert_eq!(task_id(&receiver.recv().await.unwrap()), "terminal");
}
