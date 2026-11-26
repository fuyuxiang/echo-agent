use super::super::support::*;
use super::*;

#[tokio::test(flavor = "current_thread")]
async fn activity_probes_all_session_children_and_keeps_session_on_probe_failure() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use echo_agent_tools::implementations::echo_agent_build::task::types::SubagentEvent;
            let (mut actor, _) = build_actor().await;
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            Arc::get_mut(&mut actor)
                .unwrap()
                .tool_context
                .subagent_event_tx = Some(tx);
            let expected = actor.session_id_string();
            let coordinator = tokio::task::spawn_local(async move {
                for busy in [true, false] {
                    let Some(SubagentEvent::HasLiveChildren {
                        parent_session_id,
                        respond_to,
                    }) = rx.recv().await
                    else {
                        panic!("liveness must use the pending/active/queued query");
                    };
                    assert_eq!(parent_session_id, expected);
                    respond_to.send(busy).unwrap();
                }
            });
            assert!(actor.has_live_tool_work().await);
            assert!(!actor.has_live_tool_work().await);
            coordinator.await.unwrap();
            assert!(
                actor.has_live_tool_work().await,
                "a closed probe must not permit eviction"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn activity_tracks_background_work_after_the_turn_finishes() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use super::super::auto_wake_suppression_tests::{OneTaskTerminal, completed_bash_task};
            let (actor, _) = build_actor().await;
            for (completed, owner, expected) in [
                (false, None, true),
                (false, Some(actor.session_info.id.0.to_string()), true),
                (false, Some("other-session".to_string()), false),
                (true, None, false),
            ] {
                let mut task = completed_bash_task("background");
                task.completed = completed;
                task.is_backgrounded = true;
                task.owner_session_id = owner;
                *actor.agent.borrow_mut() = test_agent_from_config(
                    echo_agent_tools::registry::types::ToolServerConfig {
                        tools: vec![],
                        behavior_preset: None,
                    },
                    echo_agent_core::AgentDefinition::default_echo_agent_build(),
                    Arc::new(OneTaskTerminal { tasks: vec![task] }),
                )
                .await;
                assert_eq!(actor.has_live_tool_work().await, expected);
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn activity_preserves_running_queued_goal_and_interaction_work() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (actor, _) = build_actor().await;
            assert!(!actor.has_live_tool_work().await);
            actor.state.lock().await.running_task = Some(running_task_stub("running"));
            assert!(actor.has_live_tool_work().await);
            actor.state.lock().await.running_task = None;
            actor
                .state
                .lock()
                .await
                .pending_inputs
                .push_back(user_item("queued", "A"));
            assert!(actor.has_live_tool_work().await);
            actor.state.lock().await.pending_inputs.clear();
            actor.pending_interactions.lock().unwrap().insert(
                "question".into(),
                crate::session::pending_interaction::PendingKind::Question,
            );
            assert!(actor.has_live_tool_work().await);
            actor.pending_interactions.lock().unwrap().clear();
            assert!(!actor.has_live_tool_work().await);
            let reservations = actor
                .tool_context
                .task_completion_reservations
                .as_ref()
                .unwrap();
            reservations.reserve("completed-task".into());
            assert!(actor.has_live_tool_work().await);
            reservations.release("completed-task");
            actor
                .state
                .lock()
                .await
                .pending_notifications
                .push(PendingNotification {
                    prompt_id: "completion".into(),
                    prompt_blocks: vec![],
                    priority: NotificationPriority::Later,
                    source: NotificationSource::BashTaskCompleted {
                        task_id: "completed-task".into(),
                    },
                });
            assert!(actor.has_live_tool_work().await);
            actor.state.lock().await.pending_notifications.clear();
            assert!(!actor.has_live_tool_work().await);
            actor.goal_tracker.lock().create_goal(
                "goal".into(),
                "objective".into(),
                None,
                0,
                "2026-01-01T00:00:00Z".into(),
                None,
            );
            assert!(actor.has_live_tool_work().await);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn activity_is_busy_when_scheduler_probe_fails() {
    tokio::task::LocalSet::new().run_until(async {
        use echo_agent_tools::implementations::echo_agent_build::scheduler::types::SchedulerHandle;
        let (actor, _) = build_actor().await;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        drop(rx);
        actor.tool_bridge_handle().toolset().resources.lock().await.insert(SchedulerHandle(tx));
        assert!(actor.has_live_tool_work().await);
    }).await;
}
