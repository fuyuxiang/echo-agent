//! `list_running_subagents` must heal a live parent's stale `running` meta
//! (tray / reconnect). Deleting that hook leaves the 10-12h Responding hole.

use super::{build_minimal_agent_for_tests, make_live_session_handle};
use crate::agent::subagent::{LIVE_ORPHAN_RECONCILE_REASON, SubagentMeta};
use crate::extensions::notification::SessionUpdate;
use crate::session::SessionCommand;
use agent_client_protocol as acp;
use echo_agent_tools::implementations::echo_agent_build::task::types::{
    SubagentEvent, SubagentInspection, SubagentSnapshot, SubagentSnapshotStatus,
};

fn running_meta(id: &str, parent: &str) -> SubagentMeta {
    SubagentMeta {
        subagent_id: id.into(),
        parent_session_id: parent.into(),
        child_session_id: format!("child-{id}"),
        subagent_type: "explore".into(),
        description: "task".into(),
        prompt: "do work".into(),
        status: "running".into(),
        started_at: chrono::Utc::now(),
        completed_at: None,
        duration_ms: None,
        tool_calls: None,
        turns: None,
        error: None,
        effective_context_source: None,
        context_normalized: false,
        fork_copy_error: None,
        persona: None,
        resumed_from: None,
        child_cwd: Some("/workspace".into()),
        worktree_path: None,
        snapshot_ref: None,
        effective_model_id: None,
    }
}

fn write_meta(dir: &std::path::Path, meta: &SubagentMeta) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("meta.json"),
        serde_json::to_string_pretty(meta).unwrap(),
    )
    .unwrap();
}

fn running_inspection(id: &str, parent: &str) -> SubagentInspection {
    SubagentInspection {
        snapshot: SubagentSnapshot {
            subagent_id: id.to_string(),
            description: "task".to_string(),
            subagent_type: "explore".to_string(),
            status: SubagentSnapshotStatus::Running {
                turn_count: 1,
                tool_call_count: 0,
                tokens_used: 0,
                context_window_tokens: 0,
                context_usage_pct: 0,
                tools_used: Vec::new(),
                error_count: 0,
            },
            started_at_epoch_ms: 0,
            duration_ms: 50,
            persona: None,
        },
        parent_session_id: parent.to_string(),
        child_session_id: format!("child-{id}"),
        fork_parent_prompt_id: None,
        resumed_from: None,
    }
}

fn drain_cancelled_finishes(
    cmd_rx: &mut tokio::sync::mpsc::UnboundedReceiver<SessionCommand>,
    id: &str,
) -> usize {
    let mut count = 0;
    while let Ok(cmd) = cmd_rx.try_recv() {
        let SessionCommand::EchoAgentSessionNotification { notification } = cmd else {
            continue;
        };
        let SessionUpdate::SubagentFinished {
            subagent_id,
            status,
            error,
            will_wake,
            ..
        } = notification.update
        else {
            continue;
        };
        if subagent_id != id {
            continue;
        }
        assert_eq!(status, "cancelled");
        assert_eq!(error.as_deref(), Some(LIVE_ORPHAN_RECONCILE_REASON));
        assert!(!will_wake);
        count += 1;
    }
    count
}

fn spawn_inspect_stub(
    mut event_rx: tokio::sync::mpsc::UnboundedReceiver<SubagentEvent>,
    inspect: Option<SubagentInspection>,
) {
    tokio::task::spawn_local(async move {
        while let Some(event) = event_rx.recv().await {
            if let SubagentEvent::Inspect(request) = event {
                let value = inspect
                    .as_ref()
                    .filter(|i| i.snapshot.subagent_id == request.subagent_id)
                    .cloned();
                let _ = request.respond_to.send(value);
            } else if let SubagentEvent::ListRunning(request) = event {
                let list = inspect
                    .as_ref()
                    .filter(|i| i.snapshot.is_running())
                    .cloned()
                    .into_iter()
                    .collect();
                let _ = request.respond_to.send(list);
            }
        }
    });
}

async fn live_session_with_running_meta(
    id: &str,
    inspect: Option<SubagentInspection>,
) -> (
    super::MvpAgent,
    String,
    std::path::PathBuf,
    tokio::sync::mpsc::UnboundedReceiver<SessionCommand>,
) {
    let agent = build_minimal_agent_for_tests();
    let event_rx = agent
        .subagent_event_rx
        .borrow_mut()
        .take()
        .expect("subagent event rx still available");
    spawn_inspect_stub(event_rx, inspect);

    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let sid = acp::SessionId::new(format!("list-running-{id}-{unique}"));
    let (handle, _cmd_tx, cmd_rx) = make_live_session_handle(&sid, None);
    let parent = sid.0.to_string();
    let session_dir = crate::session::persistence::session_dir(&handle.info);
    let sub_dir = session_dir.join("subagents").join(id);
    write_meta(&sub_dir, &running_meta(id, &parent));
    agent.insert_resident(&sid, handle);
    (agent, parent, sub_dir, cmd_rx)
}

#[tokio::test(flavor = "current_thread")]
async fn list_running_subagents_finalizes_orphan_on_live_session() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let id = "sa-list-orphan";
            let (agent, parent, sub_dir, mut cmd_rx) =
                live_session_with_running_meta(id, None).await;

            let listed = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                agent.list_running_subagents(&parent),
            )
            .await
            .expect("list_running heal must not hang");
            assert!(listed.is_empty());

            let reread: SubagentMeta =
                serde_json::from_str(&std::fs::read_to_string(sub_dir.join("meta.json")).unwrap())
                    .unwrap();
            assert_eq!(reread.status, "cancelled");
            assert_eq!(drain_cancelled_finishes(&mut cmd_rx, id), 1);
            let _ = std::fs::remove_dir_all(sub_dir.parent().unwrap());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn list_running_subagents_skips_live_coordinator_child() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let id = "sa-list-live";
            let (agent, parent, sub_dir, mut cmd_rx) =
                live_session_with_running_meta(id, Some(running_inspection(id, "parent"))).await;

            let listed = agent.list_running_subagents(&parent).await;
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0].snapshot.subagent_id, id);

            let reread: SubagentMeta =
                serde_json::from_str(&std::fs::read_to_string(sub_dir.join("meta.json")).unwrap())
                    .unwrap();
            assert_eq!(reread.status, "running");
            assert_eq!(drain_cancelled_finishes(&mut cmd_rx, id), 0);
            let _ = std::fs::remove_dir_all(sub_dir.parent().unwrap());
        })
        .await;
}

#[test]
fn release_evicts_the_registry_heal_lock() {
    let registry = super::super::session_registry::SessionRegistry::default();
    let sid = acp::SessionId::new("s-heal-evict");
    let _ = registry.live_orphan_heal_lock(&sid);
    assert_eq!(registry.counts().live_orphan_heal_locks, 1);
    registry.release(&sid);
    assert_eq!(registry.counts().live_orphan_heal_locks, 0);
}

#[test]
fn registry_heal_lock_reuses_the_same_arc_until_release() {
    let registry = super::super::session_registry::SessionRegistry::default();
    let sid = acp::SessionId::new("s-heal-race");
    let inflight = registry.live_orphan_heal_lock(&sid);
    assert_eq!(registry.counts().live_orphan_heal_locks, 1);
    let again = registry.live_orphan_heal_lock(&sid);
    assert!(
        std::sync::Arc::ptr_eq(&inflight, &again),
        "overlapping ticks must share the registry mutex"
    );
    drop(again);
    registry.release(&sid);
    assert_eq!(
        registry.counts().live_orphan_heal_locks,
        0,
        "release must drop the retained lock even while an Arc clone is held"
    );
    drop(inflight);
}
