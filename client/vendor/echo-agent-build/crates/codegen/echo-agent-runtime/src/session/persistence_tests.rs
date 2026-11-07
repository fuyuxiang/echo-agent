use super::*;
use crate::session::storage::jsonl::AppendDurability;

pub(super) struct ActorGuard {
    pub(super) handle: PersistenceHandle,
    task: tokio::task::JoinHandle<()>,
}

impl ActorGuard {
    pub(super) async fn stop(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

pub(super) fn test_actor(info: Info, storage: Arc<dyn StorageAdapter>) -> ActorGuard {
    test_actor_with_remote_sync(info, storage, None)
}

fn test_actor_with_remote_sync(
    info: Info,
    storage: Arc<dyn StorageAdapter>,
    remote_sync: Option<RemoteSync>,
) -> ActorGuard {
    test_actor_inner(info, storage, remote_sync, false)
}

fn test_actor_inner(
    info: Info,
    storage: Arc<dyn StorageAdapter>,
    remote_sync: Option<RemoteSync>,
    mark_summary_done: bool,
) -> ActorGuard {
    let (tx, rx) = mpsc::unbounded_channel();
    let (disk_full_tx, disk_full_rx) = tokio::sync::watch::channel(false);
    let sampling_client =
        OaiCompatClient::new(echo_agent_sampler::SamplerConfig::default()).unwrap();
    let mut summary =
        crate::session::summary::SummaryGenerator::new(crate::session::summary::SummaryConfig {
            sampling_client,
            model: String::new(),
            persistence_tx: tx.downgrade(),
        });
    if mark_summary_done {
        summary.mark_done();
    }
    let task = tokio::spawn(
        SessionPersistence {
            info,
            storage,
            pending_notification: None,
            pending_updates: VecDeque::new(),
            pending_chat_writes: VecDeque::new(),
            pending_plan_modes: VecDeque::new(),
            rx,
            remote_sync,
            // Resumed-style actor for these tests; upgrade backfill is fresh-only.
            created_fresh: false,
            relay_sync: None,
            summary,
            registry_title_sync: None,
            gateway: None,
            search_index: crate::session::storage::search::SharedSearchIndex::never_indexed(),
            disk_full_tx,
            disk_full_notified: false,
        }
        .run(),
    );
    ActorGuard {
        handle: PersistenceHandle::from_parts_for_test(tx, disk_full_rx),
        task,
    }
}

fn notification(info: &Info, text: &str) -> acp::SessionNotification {
    acp::SessionNotification::new(
        info.id.clone(),
        acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
            acp::TextContent::new(text),
        ))),
    )
}

fn neutral_update(info: &Info, text: &str) -> SessionUpdate {
    SessionUpdate::Acp(Box::new(notification(info, text)))
}

#[tokio::test]
async fn user_request_replay_survives_actor_abort_before_model_response() {
    let dir = tempfile::tempdir().unwrap();
    let info = Info {
        id: acp::SessionId::new("interrupted-user-request"),
        cwd: "/test".into(),
    };
    let storage = Arc::new(JsonlStorageAdapter::with_explicit_session_dir(
        dir.path().to_path_buf(),
    ));
    storage
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    let actor = test_actor(info.clone(), storage);
    actor
        .handle
        .tx
        .send(PersistenceMsg::Update(neutral_update(
            &info,
            "prior assistant tail",
        )))
        .unwrap();
    let request = acp::SessionNotification::new(
        info.id.clone(),
        acp::SessionUpdate::UserMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
            acp::TextContent::new("pending user request"),
        ))),
    );
    actor
        .handle
        .tx
        .send(PersistenceMsg::Update(SessionUpdate::Acp(Box::new(
            request,
        ))))
        .unwrap();
    // An actor ordering barrier, deliberately not Flush/FlushAndAck: the model
    // has produced no response and the host will not get a graceful shutdown.
    probe_writable(&actor.handle).await.unwrap();
    actor.stop().await;
    let reopened = JsonlStorageAdapter::with_explicit_session_dir(dir.path().to_path_buf());
    let replay = reopened.load_session(&info).await.unwrap().updates;
    assert_eq!(
        replay.len(),
        2,
        "the interrupted request was left only in the merge buffer"
    );
    let SessionUpdate::Acp(first) = &replay[0] else {
        panic!("missing prior ACP chunk")
    };
    assert!(matches!(
        &first.update,
        acp::SessionUpdate::AgentMessageChunk(_)
    ));
    let SessionUpdate::Acp(last) = &replay[1] else {
        panic!("missing user ACP chunk")
    };
    let acp::SessionUpdate::UserMessageChunk(chunk) = &last.update else {
        panic!("user order changed")
    };
    let acp::ContentBlock::Text(text) = &chunk.content else {
        panic!("user text changed")
    };
    assert_eq!(text.text, "pending user request");
}

#[test]
fn title_source_prefers_display_text_over_injected_wire_context() {
    let text = acp::TextContent::new(
        "<!--EXPERT_PERSONA_BEGIN-->\nexpert\n<!--EXPERT_PERSONA_END-->\n\nuser task",
    )
    .meta(
        serde_json::json!({ "displayText": "user task" })
            .as_object()
            .cloned(),
    );
    let chunk = PersistenceContentChunk::new(vec![acp::ContentBlock::Text(text)]);
    assert_eq!(chunk.title_source_text(), "user task");
}

#[test]
fn title_source_respects_an_explicitly_empty_display_text() {
    for key in ["displayText", "display_text"] {
        let text = acp::TextContent::new("injected attachment context")
            .meta(serde_json::json!({ (key): "" }).as_object().cloned());
        let chunk = PersistenceContentChunk::new(vec![acp::ContentBlock::Text(text)]);
        assert_eq!(chunk.title_source_text(), "");
    }
}

#[test]
fn title_source_falls_back_to_joined_wire_text_without_display_text() {
    let chunk = PersistenceContentChunk::new(vec![
        acp::ContentBlock::Text(acp::TextContent::new("first")),
        acp::ContentBlock::Text(acp::TextContent::new("second")),
    ]);
    assert_eq!(chunk.title_source_text(), "first\nsecond");
}

#[tokio::test]
async fn title_recovery_uses_opening_display_text_instead_of_later_topics() {
    let dir = tempfile::tempdir().unwrap();
    let info = Info {
        id: acp::SessionId::new("recover-first-title"),
        cwd: "/test".into(),
    };
    let storage = JsonlStorageAdapter::with_explicit_session_dir(dir.path().to_path_buf());
    storage
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    for (text, display) in [
        ("hidden project and expert context", Some("最初的完整问题")),
        ("attached context", None),
    ] {
        let text = acp::TextContent::new(text).meta(display.map(|display| {
            serde_json::json!({ "displayText": display })
                .as_object()
                .unwrap()
                .clone()
        }));
        storage
            .append_update(
                &info,
                &SessionUpdate::Acp(Box::new(acp::SessionNotification::new(
                    info.id.clone(),
                    acp::SessionUpdate::UserMessageChunk(acp::ContentChunk::new(
                        acp::ContentBlock::Text(text),
                    )),
                ))),
            )
            .await
            .unwrap();
    }
    storage
        .append_update(&info, &neutral_update(&info, "assistant response"))
        .await
        .unwrap();
    storage
        .append_update(
            &info,
            &SessionUpdate::Acp(Box::new(acp::SessionNotification::new(
                info.id.clone(),
                acp::SessionUpdate::UserMessageChunk(acp::ContentChunk::new(
                    acp::ContentBlock::Text(acp::TextContent::new("后来的天气问题")),
                )),
            ))),
        )
        .await
        .unwrap();
    assert_eq!(
        load_initial_title_source(&storage, &info, 0)
            .await
            .unwrap()
            .as_deref(),
        Some("最初的完整问题")
    );
}

#[tokio::test]
async fn title_recovery_preserves_a_textless_opening() {
    let dir = tempfile::tempdir().unwrap();
    let info = Info {
        id: acp::SessionId::new("recover-textless-title"),
        cwd: "/test".into(),
    };
    let storage = JsonlStorageAdapter::with_explicit_session_dir(dir.path().to_path_buf());
    storage
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    for content in [
        acp::ContentBlock::Image(acp::ImageContent::new("AQID", "image/png")),
        acp::ContentBlock::Text(acp::TextContent::new("later question")),
    ] {
        storage
            .append_update(
                &info,
                &SessionUpdate::Acp(Box::new(acp::SessionNotification::new(
                    info.id.clone(),
                    acp::SessionUpdate::UserMessageChunk(acp::ContentChunk::new(content)),
                ))),
            )
            .await
            .unwrap();
        storage
            .append_update(&info, &neutral_update(&info, "assistant response"))
            .await
            .unwrap();
    }
    assert_eq!(
        load_initial_title_source(&storage, &info, 0).await.unwrap(),
        Some(String::new())
    );
}

#[tokio::test]
async fn title_recovery_separates_prompts_without_an_assistant_reply() {
    let dir = tempfile::tempdir().unwrap();
    let info = Info {
        id: acp::SessionId::new("recover-cancelled-opening"),
        cwd: "/test".into(),
    };
    let storage = JsonlStorageAdapter::with_explicit_session_dir(dir.path().to_path_buf());
    storage
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    for (index, text) in ["最初的问题", "取消后的另一个问题"].into_iter().enumerate()
    {
        let chunk = acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(text)))
            .meta(
                serde_json::json!({ "promptIndex": index })
                    .as_object()
                    .cloned(),
            );
        storage
            .append_update(
                &info,
                &SessionUpdate::Acp(Box::new(acp::SessionNotification::new(
                    info.id.clone(),
                    acp::SessionUpdate::UserMessageChunk(chunk),
                ))),
            )
            .await
            .unwrap();
    }
    assert_eq!(
        load_initial_title_source(&storage, &info, 0)
            .await
            .unwrap()
            .as_deref(),
        Some("最初的问题")
    );
}

#[tokio::test]
async fn writeback_backfill_is_fresh_only_and_acp_only() {
    let info = Info {
        id: acp::SessionId::new("wb-backfill"),
        cwd: "/test".into(),
    };

    // Fresh session: every ACP update is queued to the writeback sync.
    let (sync, mut observed) = RemoteSync::test_observer();
    let updates = vec![neutral_update(&info, "a"), neutral_update(&info, "b")];
    let n = backfill_updates_to_sync(true, updates, &sync);
    assert_eq!(n, 2, "a fresh session backfills its full local ACP history");
    for _ in 0..2 {
        tokio::time::timeout(std::time::Duration::from_secs(1), observed.recv())
            .await
            .expect("backfilled notification not observed within 1s")
            .expect("observer channel closed unexpectedly");
    }

    // Resumed session: nothing is backfilled (prior history may already be synced).
    let (sync2, mut observed2) = RemoteSync::test_observer();
    let n2 = backfill_updates_to_sync(false, vec![neutral_update(&info, "a")], &sync2);
    assert_eq!(n2, 0, "a resumed session is forward-only, no backfill");
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(200), observed2.recv())
            .await
            .is_err(),
        "resumed session must not re-send any prior history",
    );
}

fn break_summary_writes(dir: &std::path::Path) {
    let summary = dir.join("summary.json");
    std::fs::remove_file(&summary).unwrap();
    std::fs::create_dir(summary).unwrap();
}

async fn recv_observed(
    observed: &mut tokio::sync::mpsc::UnboundedReceiver<acp::SessionNotification>,
) -> acp::SessionNotification {
    tokio::time::timeout(std::time::Duration::from_secs(1), observed.recv())
        .await
        .expect("remote sync timed out")
        .expect("remote sync observer closed")
}

#[test]
fn committed_error_returns_sync_disposition() {
    let info = Info {
        id: acp::SessionId::new("committed-update"),
        cwd: "/test".into(),
    };
    let notification = notification(&info, "committed");
    let PendingAppendOutcome::CommittedErr(sync_notification, error) =
        SessionPersistence::finish_pending_append(
            notification,
            Err(crate::session::storage::AppendUpdateError::Committed(
                io::Error::other("summary patch failed"),
            )),
        )
    else {
        panic!("expected committed failure");
    };
    assert_eq!(sync_notification.session_id, info.id);
    assert_eq!(error.to_string(), "summary patch failed");
}

#[test]
fn uncommitted_error_returns_restore_disposition() {
    let info = Info {
        id: acp::SessionId::new("uncommitted-update"),
        cwd: "/test".into(),
    };
    let notification = notification(&info, "pending");
    let PendingAppendOutcome::NotCommittedErr(pending_notification, error) =
        SessionPersistence::finish_pending_append(
            notification,
            Err(crate::session::storage::AppendUpdateError::NotCommitted(
                io::Error::other("append failed"),
            )),
        )
    else {
        panic!("expected uncommitted failure");
    };
    assert_eq!(pending_notification.session_id, info.id);
    assert_eq!(error.to_string(), "append failed");
}

#[tokio::test]
async fn noop_handle_rejects_durable_append() {
    let info = Info {
        id: acp::SessionId::new("noop-durable-update"),
        cwd: "/test".into(),
    };
    assert!(matches!(
        PersistenceHandle::noop()
            .append_update_durably(neutral_update(&info, "durable"))
            .await,
        Err(DurableAppendError::NotCommitted(error))
            if error.kind() == io::ErrorKind::Unsupported
    ));
}

#[tokio::test]
async fn pending_drain_disposition_controls_remote_sync() {
    let info = Info {
        id: acp::SessionId::new("pending-remote-sync"),
        cwd: "/test".into(),
    };
    let storage = JsonlStorageAdapter::with_update_append_probe("/unused".into(), |_| {
        Err(io::Error::other("append failed"))
    });
    let (remote_sync, mut observed) = RemoteSync::test_observer();
    let actor = test_actor_with_remote_sync(info.clone(), Arc::new(storage), Some(remote_sync));
    actor
        .handle
        .tx
        .send(PersistenceMsg::Update(neutral_update(&info, "pending")))
        .unwrap();
    assert!(matches!(
        actor
            .handle
            .append_update_durably(neutral_update(&info, "durable"))
            .await,
        Err(DurableAppendError::NotCommitted(_))
    ));
    assert!(observed.try_recv().is_err());
    actor.stop().await;

    let dir = tempfile::tempdir().unwrap();
    let attempts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed_attempts = attempts.clone();
    let storage = Arc::new(JsonlStorageAdapter::with_update_append_probe(
        dir.path().to_path_buf(),
        move |durability| {
            observed_attempts.lock().unwrap().push(durability);
            Ok(())
        },
    ));
    storage
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    let (remote_sync, mut observed) = RemoteSync::test_observer();
    let actor = test_actor_with_remote_sync(info.clone(), storage, Some(remote_sync));
    actor
        .handle
        .tx
        .send(PersistenceMsg::Update(neutral_update(&info, "pending")))
        .unwrap();
    break_summary_writes(dir.path());
    assert!(matches!(
        actor
            .handle
            .append_update_durably(neutral_update(&info, "durable"))
            .await,
        // The pending line committed, but the new durable update was skipped.
        Err(DurableAppendError::NotCommitted(_))
    ));
    let synced = recv_observed(&mut observed).await;
    assert_eq!(synced.session_id, info.id);
    assert!(matches!(
        attempts.lock().unwrap().as_slice(),
        [AppendDurability::Buffered]
    ));
    actor.stop().await;
}

#[tokio::test]
async fn durable_append_committed_failure_is_synced() {
    let dir = tempfile::tempdir().unwrap();
    let info = Info {
        id: acp::SessionId::new("durable-remote-sync"),
        cwd: "/test".into(),
    };
    let storage = Arc::new(JsonlStorageAdapter::with_explicit_session_dir(
        dir.path().to_path_buf(),
    ));
    storage
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    break_summary_writes(dir.path());
    let (remote_sync, mut observed) = RemoteSync::test_observer();
    let actor = test_actor_with_remote_sync(info.clone(), storage, Some(remote_sync));
    assert!(matches!(
        actor
            .handle
            .append_update_durably(neutral_update(&info, "durable"))
            .await,
        Err(DurableAppendError::Committed(_))
    ));
    let synced = recv_observed(&mut observed).await;
    assert_eq!(synced.session_id, info.id);
    actor.stop().await;
}

#[tokio::test]
async fn failed_pending_drain_retains_record_and_skips_durable_update() {
    let info = Info {
        id: acp::SessionId::new("durable-drain-failure"),
        cwd: "/test".into(),
    };
    let attempts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = attempts.clone();
    let storage =
        JsonlStorageAdapter::with_update_append_probe("/unused".into(), move |durability| {
            observed.lock().unwrap().push(durability);
            Err(io::Error::other("pending append failed"))
        });
    let actor = test_actor(info.clone(), Arc::new(storage));
    actor
        .handle
        .tx
        .send(PersistenceMsg::Update(neutral_update(&info, "pending")))
        .unwrap();
    for _ in 0..2 {
        assert_eq!(
            actor
                .handle
                .append_update_durably(neutral_update(&info, "durable"))
                .await
                .unwrap_err()
                .to_string(),
            "pending append failed"
        );
    }
    assert!(matches!(
        attempts.lock().unwrap().as_slice(),
        [AppendDurability::Buffered, AppendDurability::Buffered]
    ));
    actor.stop().await;
}

#[tokio::test]
async fn durable_append_drains_pending_update_in_fifo_order() {
    let dir = tempfile::tempdir().unwrap();
    let info = Info {
        id: acp::SessionId::new("durable-update"),
        cwd: dir.path().to_string_lossy().into_owned(),
    };
    let storage = Arc::new(JsonlStorageAdapter::with_explicit_session_dir(
        dir.path().to_path_buf(),
    ));
    storage
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    let actor = test_actor(info.clone(), storage.clone());
    actor
        .handle
        .tx
        .send(PersistenceMsg::Update(neutral_update(&info, "before")))
        .unwrap();
    actor
        .handle
        .append_update_durably(neutral_update(&info, "durable"))
        .await
        .unwrap();
    let summary = storage.load_summary(&info).await.unwrap();
    assert_eq!(summary.num_messages, 2);

    let updates = storage.load_session(&info).await.unwrap().updates;
    let texts = updates
        .iter()
        .filter_map(|update| {
            let SessionUpdate::Acp(notification) = update else {
                return None;
            };
            let acp::SessionUpdate::AgentMessageChunk(chunk) = &notification.update else {
                return None;
            };
            let acp::ContentBlock::Text(text) = &chunk.content else {
                return None;
            };
            Some(text.text.clone())
        })
        .collect::<Vec<_>>();
    assert_eq!(texts, ["before", "durable"]);
    actor.stop().await;
}

pub(super) async fn flush_ack(handle: &PersistenceHandle) -> io::Result<()> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    handle
        .tx
        .send(PersistenceMsg::FlushAndAck { respond_to: tx })
        .unwrap();
    rx.await.unwrap()
}

pub(super) async fn checked_flush_ack(handle: &PersistenceHandle) -> io::Result<()> {
    let (respond_to, response) = tokio::sync::oneshot::channel();
    handle
        .tx
        .send(PersistenceMsg::FlushAndSyncAndAck { respond_to })
        .unwrap();
    response.await.unwrap()
}

fn extension_update(info: &Info, text: &str) -> SessionUpdate {
    SessionUpdate::EchoAgent(Box::new(EchoAgentSessionNotification {
        session_id: info.id.clone(),
        update: EchoAgentSessionUpdate::AutoCompactFailed {
            error: text.to_string(),
        },
        meta: None,
    }))
}

fn persisted_update_labels(updates: &[SessionUpdate]) -> Vec<String> {
    updates
        .iter()
        .map(|update| match update {
            SessionUpdate::Acp(notification) => match &notification.update {
                acp::SessionUpdate::AgentMessageChunk(chunk)
                | acp::SessionUpdate::AgentThoughtChunk(chunk) => {
                    let acp::ContentBlock::Text(text) = &chunk.content else {
                        panic!("text expected")
                    };
                    text.text.clone()
                }
                _ => panic!("test ACP chunk expected"),
            },
            SessionUpdate::EchoAgent(notification) => {
                let EchoAgentSessionUpdate::AutoCompactFailed { error } = &notification.update
                else {
                    panic!("test extension update expected")
                };
                error.clone()
            }
        })
        .collect()
}

#[tokio::test]
async fn failed_displaced_acp_and_extension_updates_are_retried_in_fifo_order() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let dir = tempfile::tempdir().unwrap();
    let info = Info {
        id: acp::SessionId::new("retained-update-fifo"),
        cwd: "/test".into(),
    };
    let failing = Arc::new(AtomicBool::new(true));
    let fault = failing.clone();
    let storage = Arc::new(JsonlStorageAdapter::with_update_append_probe(
        dir.path().to_path_buf(),
        move |_| {
            if fault.load(Ordering::SeqCst) {
                Err(io::Error::other("injected update write failure"))
            } else {
                Ok(())
            }
        },
    ));
    storage
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    let (remote, mut observed) = RemoteSync::test_observer();
    let actor = test_actor_with_remote_sync(info.clone(), storage.clone(), Some(remote));
    let thought = SessionUpdate::Acp(Box::new(acp::SessionNotification::new(
        info.id.clone(),
        acp::SessionUpdate::AgentThoughtChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
            acp::TextContent::new("second-thought"),
        ))),
    )));
    for update in [
        neutral_update(&info, "first-message"),
        thought,
        extension_update(&info, "third-extension"),
        neutral_update(&info, "fourth-message"),
    ] {
        actor
            .handle
            .tx
            .send(PersistenceMsg::Update(update))
            .unwrap();
    }
    assert!(
        checked_flush_ack(&actor.handle)
            .await
            .unwrap_err()
            .to_string()
            .contains("injected update")
    );
    assert!(
        storage
            .load_session(&info)
            .await
            .unwrap()
            .updates
            .is_empty()
    );
    assert!(
        observed.try_recv().is_err(),
        "uncommitted records must not be synced remotely"
    );
    failing.store(false, Ordering::SeqCst);
    checked_flush_ack(&actor.handle).await.unwrap();
    checked_flush_ack(&actor.handle).await.unwrap();
    assert_eq!(
        persisted_update_labels(&storage.load_session(&info).await.unwrap().updates),
        [
            "first-message",
            "second-thought",
            "third-extension",
            "fourth-message"
        ]
    );
    for _ in 0..3 {
        recv_observed(&mut observed).await;
    }
    assert!(
        observed.try_recv().is_err(),
        "retry must sync each committed ACP record once"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("updates.jsonl"))
            .unwrap()
            .lines()
            .count(),
        4
    );
    actor.stop().await;
}

#[tokio::test]
async fn committed_update_failures_do_not_duplicate_lines_on_final_save_retry() {
    let dir = tempfile::tempdir().unwrap();
    let info = Info {
        id: acp::SessionId::new("committed-update-retry"),
        cwd: "/test".into(),
    };
    let storage = Arc::new(JsonlStorageAdapter::with_explicit_session_dir(
        dir.path().to_path_buf(),
    ));
    storage
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    let summary = std::fs::read(dir.path().join("summary.json")).unwrap();
    break_summary_writes(dir.path());
    let actor = test_actor(info.clone(), storage.clone());
    actor
        .handle
        .tx
        .send(PersistenceMsg::Update(extension_update(
            &info,
            "committed-extension",
        )))
        .unwrap();
    actor
        .handle
        .tx
        .send(PersistenceMsg::Update(neutral_update(&info, "later-chunk")))
        .unwrap();
    assert!(checked_flush_ack(&actor.handle).await.is_err());
    assert!(
        checked_flush_ack(&actor.handle).await.is_err(),
        "empty pending queue must not ACK a session whose committed summary patch failed"
    );
    std::fs::remove_dir(dir.path().join("summary.json")).unwrap();
    std::fs::write(dir.path().join("summary.json"), summary).unwrap();
    checked_flush_ack(&actor.handle).await.unwrap();
    checked_flush_ack(&actor.handle).await.unwrap();
    assert_eq!(
        persisted_update_labels(&storage.load_session(&info).await.unwrap().updates),
        ["committed-extension", "later-chunk"]
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("updates.jsonl"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    actor.stop().await;
}

#[tokio::test]
async fn checked_save_reports_sync_failure_and_retries_without_reappending() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let dir = tempfile::tempdir().unwrap();
    let info = Info {
        id: acp::SessionId::new("checked-file-sync"),
        cwd: "/test".into(),
    };
    let failing = Arc::new(AtomicBool::new(true));
    let attempts = Arc::new(AtomicUsize::new(0));
    let fault = failing.clone();
    let observed = attempts.clone();
    let storage = Arc::new(JsonlStorageAdapter::with_checked_sync_probe(
        dir.path().to_path_buf(),
        move || {
            observed.fetch_add(1, Ordering::SeqCst);
            if fault.load(Ordering::SeqCst) {
                Err(io::Error::other("injected local file sync failure"))
            } else {
                Ok(())
            }
        },
    ));
    storage
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    std::fs::create_dir_all(dir.path().join("goal")).unwrap();
    std::fs::write(dir.path().join("goal/state.json"), "{\"saved\":true}").unwrap();
    let actor = test_actor(info.clone(), storage.clone());
    actor
        .handle
        .tx
        .send(PersistenceMsg::Update(neutral_update(&info, "saved-once")))
        .unwrap();
    flush_ack(&actor.handle).await.unwrap();
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        0,
        "legacy flush does not require the new local sync barrier"
    );
    assert!(
        checked_flush_ack(&actor.handle)
            .await
            .unwrap_err()
            .to_string()
            .contains("local file sync")
    );
    failing.store(false, Ordering::SeqCst);
    checked_flush_ack(&actor.handle).await.unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("updates.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("goal/state.json")).unwrap(),
        "{\"saved\":true}"
    );
    actor.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn checked_sync_refuses_unowned_artifacts_then_succeeds_after_repair() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let info = Info {
        id: acp::SessionId::new("checked-sync-boundary"),
        cwd: "/test".into(),
    };
    let storage = Arc::new(JsonlStorageAdapter::with_explicit_session_dir(
        dir.path().to_path_buf(),
    ));
    storage
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    std::fs::write(outside.path().join("unowned.json"), "unowned").unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("unowned.json"),
        dir.path().join("unowned-link"),
    )
    .unwrap();
    let actor = test_actor(info, storage);
    assert!(
        checked_flush_ack(&actor.handle)
            .await
            .unwrap_err()
            .to_string()
            .contains("non-regular session artifact")
    );
    std::fs::remove_file(dir.path().join("unowned-link")).unwrap();
    checked_flush_ack(&actor.handle).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(outside.path().join("unowned.json")).unwrap(),
        "unowned"
    );
    actor.stop().await;
}

async fn probe_writable(handle: &PersistenceHandle) -> io::Result<()> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    handle
        .tx
        .send(PersistenceMsg::ProbeWritable { respond_to: tx })
        .unwrap();
    rx.await.unwrap()
}

/// Manual rename → next `RemoteSync` flush must not revert the backend title.
///
/// Seeds `RemoteSync` with a pre-rename title (the cache at init), drives
/// `PersistenceMsg::ManualTitleRenamed`, then queues an update and flushes.
/// The flush's `save_session_data` payload must carry the manual title.
#[tokio::test]
async fn manual_rename_next_flush_does_not_revert_backend_title() {
    use std::sync::Arc;

    use crate::auth::{AuthManager, EchoAgentAuth};
    use crate::remote::BackendClient;
    use crate::session::export::ExportedMetadata;
    use echo_agent_test_support::MockInferenceServer;

    const OLD_TITLE: &str = "Auto first-prompt summary";
    const NEW_TITLE: &str = "Manual rename";
    const SESSION_ID: &str = "rename-writeback";

    let server = MockInferenceServer::start()
        .await
        .expect("start MockInferenceServer");
    let home = tempfile::tempdir().unwrap();
    let auth = Arc::new(AuthManager::new(
        home.path(),
        crate::auth::EchoAgentComConfig::default(),
    ));
    auth.hot_swap(EchoAgentAuth {
        key: "writeback-test-token".into(),
        ..EchoAgentAuth::test_default()
    });

    let info = Info {
        id: acp::SessionId::new(SESSION_ID),
        cwd: "/test".into(),
    };
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(JsonlStorageAdapter::with_explicit_session_dir(
        dir.path().to_path_buf(),
    ));
    storage
        .init_session(&info, default_model_id())
        .await
        .unwrap();

    let metadata = ExportedMetadata {
        title: Some(OLD_TITLE.into()),
        cwd: info.cwd.clone(),
        model_id: Some("test-model".into()),
        created_at: None,
        updated_at: None,
        total_messages: None,
        parent_session_id: None,
        session_kind: None,
        subagent_type: None,
        subagent_persona: None,
        subagent_role: None,
        fork_context_source: None,
        subagent_depth: None,
        title_is_manual: None,
    };
    let client = BackendClient::with_base_url(server.origin()).with_auth_manager(auth);
    let remote_sync = RemoteSync::new(SESSION_ID.to_owned(), metadata, client);
    let actor = test_actor_with_remote_sync(info.clone(), storage, Some(remote_sync));

    actor
        .handle
        .tx
        .send(PersistenceMsg::ManualTitleRenamed(NEW_TITLE.into()))
        .unwrap();
    actor
        .handle
        .tx
        .send(PersistenceMsg::Update(neutral_update(
            &info,
            "turn after rename",
        )))
        .unwrap();
    flush_ack(&actor.handle).await.unwrap();

    let titles = wait_for_save_session_titles(&server, SESSION_ID).await;
    let last_nonempty = titles
        .iter()
        .rev()
        .find(|t| t.nonempty_messages)
        .map(|t| t.title.as_str());
    assert_eq!(
        last_nonempty,
        Some(NEW_TITLE),
        "next RemoteSync flush after ManualTitleRenamed must not revert to {OLD_TITLE:?}"
    );
    assert!(
        titles
            .iter()
            .filter(|t| t.nonempty_messages)
            .all(|t| t.title != OLD_TITLE),
        "no non-empty save_session_data may carry the pre-rename title"
    );
    assert!(
        titles
            .iter()
            .filter(|t| t.title == NEW_TITLE)
            .all(|t| t.title_is_manual == Some(true)),
        "every save of the manual title must stamp title_is_manual: {titles:?}"
    );
    let upsert_path = format!("/sessions/{SESSION_ID}");
    let upserted_title = server.requests().into_iter().rev().find_map(|r| {
        (r.method == "PUT" && r.path == upsert_path)
            .then(|| {
                r.body
                    .as_ref()?
                    .get("session")?
                    .get("title")?
                    .as_str()
                    .map(str::to_owned)
            })
            .flatten()
    });
    assert_eq!(
        upserted_title.as_deref(),
        Some(NEW_TITLE),
        "SetTitle must upsert the session-row title, not only the metadata blob; requests={:?}",
        request_path_summary(&server)
    );
    actor.stop().await;
}

#[derive(Debug)]
struct SaveTitle {
    title: String,
    title_present: bool,
    nonempty_messages: bool,
    title_is_manual: Option<bool>,
}

fn save_session_titles(
    server: &echo_agent_test_support::MockInferenceServer,
    session_id: &str,
) -> Vec<SaveTitle> {
    let path = format!("/sessions/{session_id}/data");
    server
        .requests()
        .into_iter()
        .filter(|r| r.method == "POST" && r.path == path)
        .filter_map(|r| {
            let body = r.body.as_ref()?;
            let metadata = body.get("metadata");
            let title_field = metadata.and_then(|m| m.get("title"));
            let title_present = title_field.is_some();
            let title = title_field
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_owned();
            let title_is_manual = metadata
                .and_then(|m| m.get("title_is_manual"))
                .and_then(|v| v.as_bool());
            let nonempty_messages = body
                .get("messages")
                .and_then(|m| m.as_array())
                .is_some_and(|msgs| !msgs.is_empty());
            Some(SaveTitle {
                title,
                title_present,
                nonempty_messages,
                title_is_manual,
            })
        })
        .collect()
}

fn request_path_summary(server: &echo_agent_test_support::MockInferenceServer) -> Vec<String> {
    server
        .requests()
        .iter()
        .map(|r| format!("{} {}", r.method, r.path))
        .collect()
}

async fn wait_for_save_session_titles(
    server: &echo_agent_test_support::MockInferenceServer,
    session_id: &str,
) -> Vec<SaveTitle> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let titles = save_session_titles(server, session_id);
        if titles.iter().any(|t| t.nonempty_messages) {
            return titles;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "timed out waiting for flush save_session_data; requests={:?}",
                request_path_summary(server)
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn manual_after_auto_last_flush_is_manual() {
    use std::sync::Arc;

    use crate::auth::{AuthManager, EchoAgentAuth};
    use crate::remote::BackendClient;
    use crate::session::export::ExportedMetadata;
    use echo_agent_test_support::MockInferenceServer;

    const AUTO: &str = "Auto title";
    const MANUAL: &str = "Manual wins";
    const SESSION_ID: &str = "rename-fifo-auto-then-manual";

    let server = MockInferenceServer::start()
        .await
        .expect("start MockInferenceServer");
    let home = tempfile::tempdir().unwrap();
    let auth = Arc::new(AuthManager::new(
        home.path(),
        crate::auth::EchoAgentComConfig::default(),
    ));
    auth.hot_swap(EchoAgentAuth {
        key: "writeback-test-token".into(),
        ..EchoAgentAuth::test_default()
    });

    let info = Info {
        id: acp::SessionId::new(SESSION_ID),
        cwd: "/test".into(),
    };
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(JsonlStorageAdapter::with_explicit_session_dir(
        dir.path().to_path_buf(),
    ));
    storage
        .init_session(&info, default_model_id())
        .await
        .unwrap();

    let metadata = ExportedMetadata {
        title: Some("pre".into()),
        cwd: info.cwd.clone(),
        model_id: Some("test-model".into()),
        created_at: None,
        updated_at: None,
        total_messages: None,
        parent_session_id: None,
        session_kind: None,
        subagent_type: None,
        subagent_persona: None,
        subagent_role: None,
        fork_context_source: None,
        subagent_depth: None,
        title_is_manual: None,
    };
    let client = BackendClient::with_base_url(server.origin()).with_auth_manager(auth);
    let remote_sync = RemoteSync::new(SESSION_ID.to_owned(), metadata, client);
    let actor = test_actor_with_remote_sync(info.clone(), storage, Some(remote_sync));

    actor
        .handle
        .tx
        .send(PersistenceMsg::GeneratedTitle(AUTO.into()))
        .unwrap();
    actor
        .handle
        .tx
        .send(PersistenceMsg::ManualTitleRenamed(MANUAL.into()))
        .unwrap();
    actor
        .handle
        .tx
        .send(PersistenceMsg::Update(neutral_update(&info, "after")))
        .unwrap();
    flush_ack(&actor.handle).await.unwrap();

    let titles = wait_for_save_session_titles(&server, SESSION_ID).await;
    let last = titles
        .iter()
        .rev()
        .find(|t| t.nonempty_messages)
        .map(|t| t.title.as_str());
    assert_eq!(
        last,
        Some(MANUAL),
        "auto-then-manual last flush must be manual"
    );
    assert!(
        titles
            .iter()
            .filter(|t| t.title == AUTO)
            .all(|t| t.title_is_manual.is_none()),
        "auto SetTitle must omit title_is_manual: {titles:?}"
    );
    assert!(
        titles
            .iter()
            .filter(|t| t.title == MANUAL)
            .all(|t| t.title_is_manual == Some(true)),
        "manual SetTitle/flush must stamp title_is_manual: {titles:?}"
    );
    actor.stop().await;
}

#[tokio::test]
async fn auto_after_committed_manual_emits_no_set_title() {
    use std::sync::Arc;

    use crate::auth::{AuthManager, EchoAgentAuth};
    use crate::remote::BackendClient;
    use crate::session::export::ExportedMetadata;
    use echo_agent_test_support::MockInferenceServer;

    const AUTO: &str = "Rejected auto";
    const MANUAL: &str = "Pinned manual";
    const SESSION_ID: &str = "rename-fifo-manual-then-auto";

    let server = MockInferenceServer::start()
        .await
        .expect("start MockInferenceServer");
    let home = tempfile::tempdir().unwrap();
    let auth = Arc::new(AuthManager::new(
        home.path(),
        crate::auth::EchoAgentComConfig::default(),
    ));
    auth.hot_swap(EchoAgentAuth {
        key: "writeback-test-token".into(),
        ..EchoAgentAuth::test_default()
    });

    let info = Info {
        id: acp::SessionId::new(SESSION_ID),
        cwd: "/test".into(),
    };
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(JsonlStorageAdapter::with_explicit_session_dir(
        dir.path().to_path_buf(),
    ));
    storage
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    storage
        .update_session_title(&info, MANUAL.to_owned())
        .await
        .unwrap();

    let metadata = ExportedMetadata {
        title: Some("stale".into()),
        cwd: info.cwd.clone(),
        model_id: Some("test-model".into()),
        created_at: None,
        updated_at: None,
        total_messages: None,
        parent_session_id: None,
        session_kind: None,
        subagent_type: None,
        subagent_persona: None,
        subagent_role: None,
        fork_context_source: None,
        subagent_depth: None,
        title_is_manual: None,
    };
    let client = BackendClient::with_base_url(server.origin()).with_auth_manager(auth);
    let remote_sync = RemoteSync::new(SESSION_ID.to_owned(), metadata, client);
    let actor = test_actor_with_remote_sync(info.clone(), storage, Some(remote_sync));

    actor
        .handle
        .tx
        .send(PersistenceMsg::ManualTitleRenamed(MANUAL.into()))
        .unwrap();
    actor
        .handle
        .tx
        .send(PersistenceMsg::GeneratedTitle(AUTO.into()))
        .unwrap();
    actor
        .handle
        .tx
        .send(PersistenceMsg::Update(neutral_update(&info, "after")))
        .unwrap();
    flush_ack(&actor.handle).await.unwrap();

    let titles = wait_for_save_session_titles(&server, SESSION_ID).await;
    assert!(
        titles.iter().all(|t| t.title != AUTO),
        "rejected auto title must not reach save_session_data"
    );
    let last = titles
        .iter()
        .rev()
        .find(|t| t.nonempty_messages)
        .map(|t| t.title.as_str());
    assert_eq!(last, Some(MANUAL));
    assert!(
        titles
            .iter()
            .filter(|t| t.title == MANUAL)
            .all(|t| t.title_is_manual == Some(true)),
        "manual stamp must survive rejected auto + flush: {titles:?}"
    );
    actor.stop().await;
}

#[tokio::test]
async fn manual_title_renamed_is_noop_without_remote_sync() {
    let dir = tempfile::tempdir().unwrap();
    let info = Info {
        id: acp::SessionId::new("rename-local-only"),
        cwd: "/test".into(),
    };
    let storage = Arc::new(JsonlStorageAdapter::with_explicit_session_dir(
        dir.path().to_path_buf(),
    ));
    storage
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    let actor = test_actor(info.clone(), storage);
    actor
        .handle
        .tx
        .send(PersistenceMsg::ManualTitleRenamed("Local only".into()))
        .unwrap();
    flush_ack(&actor.handle)
        .await
        .expect("ManualTitleRenamed with remote_sync=None must not fail");
    actor.stop().await;
}

/// Auto → manual → unpin: generator starts Done (as production `load` would),
/// a ContentChunk before reset must not spawn, ResetTitleToAuto must
/// `reset()` + clear the remote pin, and a later ContentChunk must adopt
/// via the fallback (empty model, no live LLM).
#[tokio::test]
async fn reset_title_to_auto_then_generated_title_is_adopted() {
    use std::sync::Arc;

    use crate::auth::{AuthManager, EchoAgentAuth};
    use crate::remote::BackendClient;
    use crate::session::export::ExportedMetadata;
    use crate::session::helpers::session_summary::title_fallback_from_user_text;
    use crate::session::persistence::PersistenceContentChunk;
    use echo_agent_test_support::MockInferenceServer;

    const AUTO: &str = "Auto first title";
    const MANUAL: &str = "Pinned manual";
    const CHUNK: &str = "fresh auto title from next chunk please";
    const SESSION_ID: &str = "rename-reset-to-auto";
    let expected_fresh = title_fallback_from_user_text(CHUNK);

    let server = MockInferenceServer::start()
        .await
        .expect("start MockInferenceServer");
    let home = tempfile::tempdir().unwrap();
    let auth = Arc::new(AuthManager::new(
        home.path(),
        crate::auth::EchoAgentComConfig::default(),
    ));
    auth.hot_swap(EchoAgentAuth {
        key: "writeback-test-token".into(),
        ..EchoAgentAuth::test_default()
    });

    let info = Info {
        id: acp::SessionId::new(SESSION_ID),
        cwd: "/test".into(),
    };
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(JsonlStorageAdapter::with_explicit_session_dir(
        dir.path().to_path_buf(),
    ));
    storage
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    assert!(
        storage
            .set_generated_title_if_absent(&info, AUTO.to_owned())
            .await
            .unwrap()
    );
    storage
        .update_session_title(&info, MANUAL.to_owned())
        .await
        .unwrap();
    assert!(
        !storage
            .set_generated_title_if_absent(&info, "Rejected auto".into())
            .await
            .unwrap(),
        "manual pin must reject auto title before reset"
    );

    let metadata = ExportedMetadata {
        title: Some(MANUAL.into()),
        cwd: info.cwd.clone(),
        model_id: Some("test-model".into()),
        created_at: None,
        updated_at: None,
        total_messages: None,
        parent_session_id: None,
        session_kind: None,
        subagent_type: None,
        subagent_persona: None,
        subagent_role: None,
        fork_context_source: None,
        subagent_depth: None,
        title_is_manual: Some(true),
    };
    let client = BackendClient::with_base_url(server.origin()).with_auth_manager(auth);
    let remote_sync = RemoteSync::new(SESSION_ID.to_owned(), metadata, client);
    let actor = test_actor_inner(
        info.clone(),
        storage.clone(),
        Some(remote_sync),
        true, /* mark_summary_done: production load after a titled session */
    );

    let pre_reset_chunk = PersistenceContentChunk::new(vec![acp::ContentBlock::Text(
        acp::TextContent::new("should not generate while still manual and Done"),
    )]);
    actor
        .handle
        .tx
        .send(PersistenceMsg::ContentChunk(pre_reset_chunk))
        .unwrap();
    flush_ack(&actor.handle).await.unwrap();
    let summary_path = dir.path().join("summary.json");
    let still_manual: crate::session::persistence::Summary =
        serde_json::from_slice(&std::fs::read(&summary_path).unwrap()).unwrap();
    assert_eq!(still_manual.display_title(), MANUAL);
    assert!(still_manual.title_is_manual);
    assert!(
        save_session_titles(&server, SESSION_ID)
            .iter()
            .filter(|t| t.title_present)
            .all(|t| t.title == MANUAL),
        "Done generator must not adopt an in-flight title before reset"
    );

    assert!(storage.reset_title_to_auto(&info).await.unwrap());
    actor
        .handle
        .tx
        .send(PersistenceMsg::ResetTitleToAuto)
        .unwrap();
    actor
        .handle
        .tx
        .send(PersistenceMsg::Update(neutral_update(&info, "after reset")))
        .unwrap();
    flush_ack(&actor.handle).await.unwrap();

    let upsert_path = format!("/sessions/{SESSION_ID}");
    let session_row_title = || server.requests().into_iter().rev().find_map(|r| {
        (r.method == "PUT" && r.path == upsert_path)
            .then(|| {
                r.body
                    .as_ref()?
                    .get("session")?
                    .get("title")?
                    .as_str()
                    .map(str::to_owned)
            })
            .flatten()
    });
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let titles = save_session_titles(&server, SESSION_ID);
        // The remote worker awaits POST before sending PUT. Seeing the POST
        // in the mock server is not an acknowledgement of that later request.
        if titles.iter().any(|t| t.title_present && t.title.is_empty()
            && t.title_is_manual == Some(false)) && session_row_title().as_deref() == Some("") {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("unpin must POST cleared metadata and PUT the empty session-row title: {titles:?}; requests={:?}", request_path_summary(&server));
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let upserted_title = session_row_title();
    assert_eq!(
        upserted_title.as_deref(),
        Some(""),
        "ClearTitle must upsert the session-row title empty, not only the metadata blob; requests={:?}",
        request_path_summary(&server)
    );

    let post_reset: crate::session::persistence::Summary =
        serde_json::from_slice(&std::fs::read(&summary_path).unwrap()).unwrap();
    assert!(
        post_reset.display_title().trim().is_empty(),
        "display_title must be blank so if-absent can adopt"
    );

    let post_reset_chunk =
        PersistenceContentChunk::new(vec![acp::ContentBlock::Text(acp::TextContent::new(CHUNK))]);
    actor
        .handle
        .tx
        .send(PersistenceMsg::ContentChunk(post_reset_chunk))
        .unwrap();
    actor
        .handle
        .tx
        .send(PersistenceMsg::Update(neutral_update(&info, "after auto")))
        .unwrap();
    flush_ack(&actor.handle).await.unwrap();

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(8);
    let on_disk = loop {
        let on_disk: crate::session::persistence::Summary =
            serde_json::from_slice(&std::fs::read(&summary_path).unwrap()).unwrap();
        if on_disk.display_title() == expected_fresh && !on_disk.title_is_manual {
            break on_disk;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "ContentChunk after reset never adopted fallback {expected_fresh:?}; display={:?}",
                on_disk.display_title()
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    assert_eq!(on_disk.display_title(), expected_fresh);

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let titles = save_session_titles(&server, SESSION_ID);
        if titles
            .iter()
            .any(|t| t.title == expected_fresh && t.title_is_manual.is_none())
        {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("adopted auto title after reset never reached save_session_data: {titles:?}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    actor.stop().await;
}

/// In-flight first-chunk generation overlapping unpin: disk is already blank
/// when the stale `GeneratedTitle` arrives, so if-absent adopts it as auto
/// (never re-pins).
#[tokio::test]
async fn reset_title_to_auto_adopts_in_flight_generation_as_auto() {
    use std::sync::Arc;

    use crate::session::persistence::PersistenceContentChunk;

    const MANUAL: &str = "Pinned manual";
    const CHUNK: &str = "in flight chunk text for title fallback";

    let info = Info {
        id: acp::SessionId::new("rename-reset-inflight"),
        cwd: "/test".into(),
    };
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(JsonlStorageAdapter::with_explicit_session_dir(
        dir.path().to_path_buf(),
    ));
    storage
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    storage
        .update_session_title(&info, MANUAL.to_owned())
        .await
        .unwrap();

    let actor = test_actor_inner(info.clone(), storage.clone(), None, false);

    actor
        .handle
        .tx
        .send(PersistenceMsg::ContentChunk(PersistenceContentChunk::new(
            vec![acp::ContentBlock::Text(acp::TextContent::new(CHUNK))],
        )))
        .unwrap();
    assert!(storage.reset_title_to_auto(&info).await.unwrap());
    actor
        .handle
        .tx
        .send(PersistenceMsg::ResetTitleToAuto)
        .unwrap();
    flush_ack(&actor.handle).await.unwrap();

    let summary_path = dir.path().join("summary.json");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(8);
    let on_disk = loop {
        let on_disk: crate::session::persistence::Summary =
            serde_json::from_slice(&std::fs::read(&summary_path).unwrap()).unwrap();
        if !on_disk.display_title().trim().is_empty() {
            break on_disk;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("in-flight GeneratedTitle after unpin never adopted");
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    assert_ne!(on_disk.display_title(), MANUAL);
    assert!(
        !on_disk.title_is_manual,
        "in-flight adopt after unpin must stay auto"
    );
    actor.stop().await;
}

/// Non-resident unpin only patches disk. The next load sees a blank
/// `display_title()` so the generator stays Idle and a ContentChunk adopts.
#[tokio::test]
async fn non_resident_reset_then_load_regenerates() {
    use std::sync::Arc;

    use crate::session::helpers::session_summary::title_fallback_from_user_text;
    use crate::session::persistence::PersistenceContentChunk;

    const CHUNK: &str = "dormant session next turn title text";
    let expected = title_fallback_from_user_text(CHUNK);

    let info = Info {
        id: acp::SessionId::new("rename-reset-dormant-load"),
        cwd: "/test".into(),
    };
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(JsonlStorageAdapter::with_explicit_session_dir(
        dir.path().to_path_buf(),
    ));
    storage
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    assert!(
        storage
            .set_generated_title_if_absent(&info, "Auto Title".into())
            .await
            .unwrap()
    );
    storage
        .update_session_title(&info, "Manual Title".into())
        .await
        .unwrap();
    assert!(storage.reset_title_to_auto(&info).await.unwrap());

    let summary_path = dir.path().join("summary.json");
    let after_reset: crate::session::persistence::Summary =
        serde_json::from_slice(&std::fs::read(&summary_path).unwrap()).unwrap();
    let has_title = !after_reset.display_title().is_empty();
    assert!(
        !has_title,
        "production load would mark_done() if display_title stayed set"
    );

    let actor = test_actor_inner(info.clone(), storage, None, has_title);
    actor
        .handle
        .tx
        .send(PersistenceMsg::ContentChunk(PersistenceContentChunk::new(
            vec![acp::ContentBlock::Text(acp::TextContent::new(CHUNK))],
        )))
        .unwrap();
    flush_ack(&actor.handle).await.unwrap();

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(8);
    let on_disk = loop {
        let on_disk: crate::session::persistence::Summary =
            serde_json::from_slice(&std::fs::read(&summary_path).unwrap()).unwrap();
        if on_disk.display_title() == expected && !on_disk.title_is_manual {
            break on_disk;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "load after non-resident unpin never adopted {expected:?}; display={:?}",
                on_disk.display_title()
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    assert_eq!(on_disk.display_title(), expected);
    actor.stop().await;
}

#[tokio::test]
async fn reset_title_to_auto_is_noop_without_remote_sync() {
    let dir = tempfile::tempdir().unwrap();
    let info = Info {
        id: acp::SessionId::new("reset-local-only"),
        cwd: "/test".into(),
    };
    let storage = Arc::new(JsonlStorageAdapter::with_explicit_session_dir(
        dir.path().to_path_buf(),
    ));
    storage
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    storage
        .update_session_title(&info, "Manual".into())
        .await
        .unwrap();
    storage.reset_title_to_auto(&info).await.unwrap();
    let actor = test_actor(info.clone(), storage);
    actor
        .handle
        .tx
        .send(PersistenceMsg::ResetTitleToAuto)
        .unwrap();
    flush_ack(&actor.handle)
        .await
        .expect("ResetTitleToAuto with remote_sync=None must not fail");
    actor.stop().await;
}

#[tokio::test]
async fn successful_append_clears_disk_full_latch() {
    let dir = tempfile::tempdir().unwrap();
    let info = Info {
        id: acp::SessionId::new("disk-full-clear"),
        cwd: "/test".into(),
    };
    let fail = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let fail_flag = fail.clone();
    let storage = Arc::new(JsonlStorageAdapter::with_update_append_probe(
        dir.path().to_path_buf(),
        move |_| {
            if fail_flag.load(std::sync::atomic::Ordering::SeqCst) {
                Err(io::Error::from(io::ErrorKind::StorageFull))
            } else {
                Ok(())
            }
        },
    ));
    storage
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    let actor = test_actor(info.clone(), storage);
    actor
        .handle
        .tx
        .send(PersistenceMsg::Update(neutral_update(&info, "chunk")))
        .unwrap();
    assert!(flush_ack(&actor.handle).await.is_err());
    assert!(actor.handle.is_disk_full());

    fail.store(false, std::sync::atomic::Ordering::SeqCst);
    actor
        .handle
        .tx
        .send(PersistenceMsg::Update(neutral_update(&info, "recovered")))
        .unwrap();
    assert!(flush_ack(&actor.handle).await.is_ok());
    assert!(!actor.handle.is_disk_full());
    actor.stop().await;
}

#[tokio::test]
async fn successful_probe_writable_clears_disk_full_latch() {
    let dir = tempfile::tempdir().unwrap();
    let info = Info {
        id: acp::SessionId::new("disk-full-probe"),
        cwd: "/test".into(),
    };
    let storage = Arc::new(JsonlStorageAdapter::with_update_append_probe(
        dir.path().to_path_buf(),
        |_| Err(io::Error::from(io::ErrorKind::StorageFull)),
    ));
    storage
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    let actor = test_actor(info.clone(), storage);
    actor
        .handle
        .tx
        .send(PersistenceMsg::Update(neutral_update(&info, "chunk")))
        .unwrap();
    assert!(flush_ack(&actor.handle).await.is_err());
    assert!(actor.handle.is_disk_full());

    assert!(probe_writable(&actor.handle).await.is_ok());
    assert!(!actor.handle.is_disk_full());
    actor.stop().await;
}

#[cfg(unix)]
mod prompt_file_tests {
    use super::*;
    use crate::test_support::unix_mode;

    #[test]
    fn prompt_file_dir_chain_is_owner_only() {
        let home = tempfile::TempDir::new().unwrap();
        let info = Info {
            id: agent_client_protocol::SessionId::new("prompt-perm-test"),
            cwd: "/some/project".to_string(),
        };

        let path = get_prompt_file_path_in(home.path(), &info, 0);

        // The chain below prompts/ is ensure_owner_only_session_dir_in's job,
        // pinned by ensure_owner_only_session_dir_tightens_chain — only the
        // prompts/ level is this path's own creation.
        let prompts_dir = path.parent().unwrap();
        assert_eq!(unix_mode(prompts_dir), 0o700, "prompts dir must be 0700");
    }

    /// The chat-kind (noop-persistence) writers' dir creator.
    #[test]
    fn ensure_owner_only_session_dir_tightens_chain() {
        let home = tempfile::TempDir::new().unwrap();
        let info = Info {
            id: agent_client_protocol::SessionId::new("chat-kind-perm-test"),
            cwd: "/some/project".to_string(),
        };

        let dir = ensure_owner_only_session_dir_in(home.path(), &info).unwrap();

        assert_eq!(unix_mode(&dir), 0o700, "session dir must be 0700");
        assert_eq!(
            unix_mode(dir.parent().unwrap()),
            0o700,
            "<encoded-cwd> dir must be 0700"
        );
        assert_eq!(
            unix_mode(&home.path().join("sessions")),
            0o700,
            "sessions root must be 0700"
        );
    }
}
