use super::durable_update_tests::{checked_flush_ack, flush_ack, test_actor};
use super::*;
use crate::session::plan_mode::{PlanModeSnapshot, PlanModeState};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn test_info(directory: &Path, id: &str) -> Info {
    Info {
        id: acp::SessionId::new(id),
        cwd: directory.to_string_lossy().into_owned(),
    }
}

fn chat_texts(history: &[ConversationItem]) -> Vec<String> {
    history.iter().map(ConversationItem::text_content).collect()
}

fn physical_chat_lines(directory: &Path) -> usize {
    std::fs::read_to_string(directory.join("chat_history.jsonl"))
        .unwrap()
        .lines()
        .count()
}

#[tokio::test]
async fn failed_chat_appends_retain_user_and_assistant_in_fifo_order() {
    let directory = tempfile::tempdir().unwrap();
    let info = test_info(directory.path(), "chat-fifo-retry");
    let blocked = Arc::new(AtomicBool::new(false));
    let blocked_for_probe = blocked.clone();
    let storage = Arc::new(JsonlStorageAdapter::with_chat_append_probe(
        directory.path().to_path_buf(),
        move || {
            if blocked_for_probe.load(Ordering::SeqCst) {
                Err(io::Error::other("chat writes blocked"))
            } else {
                Ok(())
            }
        },
    ));
    storage
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    storage
        .append_chat_message(&info, &ConversationItem::user("older user"))
        .await
        .unwrap();
    let actor = test_actor(info.clone(), storage.clone());

    blocked.store(true, Ordering::SeqCst);
    actor
        .handle
        .tx
        .send(PersistenceMsg::Chat(ConversationItem::user("latest user")))
        .unwrap();
    actor
        .handle
        .tx
        .send(PersistenceMsg::Chat(ConversationItem::assistant(
            "latest assistant",
        )))
        .unwrap();
    assert!(flush_ack(&actor.handle).await.is_err());
    assert!(checked_flush_ack(&actor.handle).await.is_err());
    assert_eq!(physical_chat_lines(directory.path()), 1);
    assert_eq!(
        chat_texts(&storage.load_session(&info).await.unwrap().chat_history),
        ["older user"],
        "the nonempty old cache must not hide the failed latest turn",
    );

    blocked.store(false, Ordering::SeqCst);
    checked_flush_ack(&actor.handle).await.unwrap();
    checked_flush_ack(&actor.handle).await.unwrap();
    let restored = storage.load_session(&info).await.unwrap();
    assert_eq!(
        chat_texts(&restored.chat_history),
        ["older user", "latest user", "latest assistant"]
    );
    assert!(matches!(
        &restored.chat_history[1],
        ConversationItem::User(_)
    ));
    assert!(matches!(
        &restored.chat_history[2],
        ConversationItem::Assistant(_)
    ));
    assert_eq!(physical_chat_lines(directory.path()), 3);
    actor.stop().await;
}

#[tokio::test]
async fn committed_chat_error_retries_sync_without_reappending_the_user() {
    let directory = tempfile::tempdir().unwrap();
    let info = test_info(directory.path(), "chat-committed-retry");
    let append_attempts = Arc::new(AtomicUsize::new(0));
    let observed_attempts = append_attempts.clone();
    let storage = Arc::new(JsonlStorageAdapter::with_chat_append_probe(
        directory.path().to_path_buf(),
        move || {
            observed_attempts.fetch_add(1, Ordering::SeqCst);
            Ok(())
        },
    ));
    storage
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    storage
        .append_chat_message(&info, &ConversationItem::user("older user"))
        .await
        .unwrap();
    append_attempts.store(0, Ordering::SeqCst);
    let summary_path = directory.path().join("summary.json");
    let original_summary = std::fs::read(&summary_path).unwrap();
    std::fs::remove_file(&summary_path).unwrap();
    std::fs::create_dir(&summary_path).unwrap();
    let actor = test_actor(info.clone(), storage.clone());

    actor
        .handle
        .tx
        .send(PersistenceMsg::Chat(ConversationItem::user("latest user")))
        .unwrap();
    actor
        .handle
        .tx
        .send(PersistenceMsg::Chat(ConversationItem::assistant(
            "latest assistant",
        )))
        .unwrap();
    assert!(flush_ack(&actor.handle).await.is_err());
    assert!(checked_flush_ack(&actor.handle).await.is_err());
    assert_eq!(append_attempts.load(Ordering::SeqCst), 1);
    assert_eq!(physical_chat_lines(directory.path()), 2);

    std::fs::remove_dir(&summary_path).unwrap();
    std::fs::write(&summary_path, original_summary).unwrap();
    checked_flush_ack(&actor.handle).await.unwrap();
    checked_flush_ack(&actor.handle).await.unwrap();
    assert_eq!(
        append_attempts.load(Ordering::SeqCst),
        2,
        "retry syncs the committed user and appends only the still-pending assistant"
    );
    assert_eq!(physical_chat_lines(directory.path()), 3);
    assert_eq!(
        chat_texts(&storage.load_session(&info).await.unwrap().chat_history),
        ["older user", "latest user", "latest assistant"],
    );
    actor.stop().await;
}

fn plan_snapshot(reminder_count: u32, awaiting_plan_approval: bool) -> PlanModeSnapshot {
    PlanModeSnapshot {
        state: PlanModeState::Active,
        was_previously_active: true,
        reminder_count,
        pending_exit_reminder: false,
        awaiting_plan_approval,
    }
}

#[tokio::test]
async fn failed_plan_mode_snapshots_preserve_old_state_and_retry_in_fifo_order() {
    let directory = tempfile::tempdir().unwrap();
    let info = test_info(directory.path(), "plan-mode-fifo-retry");
    let plan_path = directory.path().join("plan_mode.json");
    let observed_path = plan_path.clone();
    let blocked = Arc::new(AtomicBool::new(false));
    let blocked_for_probe = blocked.clone();
    let previous_states = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed_states = previous_states.clone();
    let storage = Arc::new(JsonlStorageAdapter::with_plan_mode_write_probe(
        directory.path().to_path_buf(),
        move || {
            if let Ok(bytes) = std::fs::read(&observed_path) {
                let previous: PlanModeSnapshot = serde_json::from_slice(&bytes).unwrap();
                observed_states
                    .lock()
                    .unwrap()
                    .push(previous.reminder_count);
            }
            if blocked_for_probe.load(Ordering::SeqCst) {
                Err(io::Error::other("plan mode writes blocked"))
            } else {
                Ok(())
            }
        },
    ));
    storage
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    storage
        .write_plan_mode_state(&info, &plan_snapshot(0, false))
        .await
        .unwrap();
    let old_bytes = std::fs::read(&plan_path).unwrap();
    let actor = test_actor(info.clone(), storage.clone());

    blocked.store(true, Ordering::SeqCst);
    actor
        .handle
        .tx
        .send(PersistenceMsg::PlanModeState(plan_snapshot(1, false)))
        .unwrap();
    actor
        .handle
        .tx
        .send(PersistenceMsg::PlanModeState(plan_snapshot(2, true)))
        .unwrap();
    assert!(flush_ack(&actor.handle).await.is_err());
    assert!(checked_flush_ack(&actor.handle).await.is_err());
    assert_eq!(
        std::fs::read(&plan_path).unwrap(),
        old_bytes,
        "failed writes must leave the old atomic snapshot intact"
    );
    let old = storage
        .load_session(&info)
        .await
        .unwrap()
        .plan_mode_state
        .unwrap();
    assert_eq!(old.reminder_count, 0);
    assert!(!old.awaiting_plan_approval);
    {
        let attempted_states = previous_states.lock().unwrap();
        assert!(!attempted_states.is_empty());
        assert!(attempted_states.iter().all(|count| *count == 0));
    }

    previous_states.lock().unwrap().clear();
    blocked.store(false, Ordering::SeqCst);
    checked_flush_ack(&actor.handle).await.unwrap();
    checked_flush_ack(&actor.handle).await.unwrap();
    assert_eq!(
        *previous_states.lock().unwrap(),
        [0, 1],
        "the second snapshot must follow the older queued snapshot"
    );
    let restored = storage
        .load_session(&info)
        .await
        .unwrap()
        .plan_mode_state
        .unwrap();
    assert_eq!(restored.state, PlanModeState::Active);
    assert_eq!(restored.reminder_count, 2);
    assert!(restored.was_previously_active);
    assert!(!restored.pending_exit_reminder);
    assert!(
        restored.awaiting_plan_approval,
        "resume must restore the outstanding plan approval"
    );
    actor.stop().await;
}

#[tokio::test]
async fn chat_appends_cannot_overtake_a_failed_history_rewrite() {
    let directory = tempfile::tempdir().unwrap();
    let info = test_info(directory.path(), "chat-rewrite-fifo-retry");
    let append_attempts = Arc::new(AtomicUsize::new(0));
    let observed_attempts = append_attempts.clone();
    let storage = Arc::new(JsonlStorageAdapter::with_chat_append_probe(
        directory.path().to_path_buf(),
        move || {
            observed_attempts.fetch_add(1, Ordering::SeqCst);
            Ok(())
        },
    ));
    storage
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    storage
        .append_chat_message(&info, &ConversationItem::user("older user"))
        .await
        .unwrap();
    let chat_path = directory.path().join("chat_history.jsonl");
    let old_bytes = std::fs::read(&chat_path).unwrap();
    append_attempts.store(0, Ordering::SeqCst);
    std::fs::remove_file(&chat_path).unwrap();
    std::fs::create_dir(&chat_path).unwrap();
    let actor = test_actor(info.clone(), storage.clone());

    actor
        .handle
        .tx
        .send(PersistenceMsg::ReplaceChatHistory(vec![
            ConversationItem::user("compacted user"),
            ConversationItem::assistant("compacted assistant"),
        ]))
        .unwrap();
    actor
        .handle
        .tx
        .send(PersistenceMsg::Chat(ConversationItem::user("latest user")))
        .unwrap();
    actor
        .handle
        .tx
        .send(PersistenceMsg::Chat(ConversationItem::assistant(
            "latest assistant",
        )))
        .unwrap();
    assert!(flush_ack(&actor.handle).await.is_err());
    assert!(checked_flush_ack(&actor.handle).await.is_err());
    assert_eq!(
        append_attempts.load(Ordering::SeqCst),
        0,
        "later chat appends must wait for the earlier rewrite"
    );

    std::fs::remove_dir(&chat_path).unwrap();
    std::fs::write(&chat_path, old_bytes).unwrap();
    checked_flush_ack(&actor.handle).await.unwrap();
    checked_flush_ack(&actor.handle).await.unwrap();
    assert_eq!(append_attempts.load(Ordering::SeqCst), 2);
    assert_eq!(physical_chat_lines(directory.path()), 4);
    assert_eq!(
        chat_texts(&storage.load_session(&info).await.unwrap().chat_history),
        [
            "compacted user",
            "compacted assistant",
            "latest user",
            "latest assistant"
        ],
        "the rewrite must land before either latest chat item and leave no duplicates",
    );
    actor.stop().await;
}
