//! Mid-turn plan-mode toggle: `handle_session_mode("plan")` while a turn is
//! running must activate the tracker immediately and buffer the activation
//! reminder for the running turn (previously, toggling plan mode while
//! the model is thinking was ignored until the next prompt, so the model
//! jumped straight into implementation).
use super::support::*;
use super::*;

/// Park a fake in-flight turn on the actor so `handle_session_mode` sees
/// `running_task.is_some()`. The never-completing task is torn down when the
/// test's `LocalSet` is dropped.
async fn fake_running_turn(actor: &SessionActor) {
    actor.state.lock().await.running_task = Some(AgentTask {
        prompt_id: "running-turn".into(),
        handle: tokio::task::spawn_local(std::future::pending::<()>()).abort_handle(),
    });
}

/// Toggling plan mode ON mid-turn activates immediately (Pending is skipped)
/// and buffers the activation reminder on the tracker; the flush delivers it
/// into the conversation as a `<system-reminder>` user message and only then
/// advances the full/sparse alternation.
#[tokio::test]
async fn midturn_plan_toggle_activates_and_buffers_reminder() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _gateway_rx) = build_actor().await;
            fake_running_turn(&actor).await;

            actor
                .handle_session_mode(acp::SessionModeId::new("plan"))
                .await;

            {
                let tracker = actor.plan_mode.lock();
                assert_eq!(
                    tracker.state(),
                    crate::session::plan_mode::PlanModeState::Active,
                    "mid-turn toggle must activate immediately, not park in Pending"
                );
                assert!(tracker.has_pending_activation());
                // Recorded at delivery, not buffer time.
                assert!(tracker.should_use_full_reminder());
            }
            // Buffered — not pushed directly into the conversation (a direct
            // push could interleave with an in-flight tool batch).
            assert_eq!(
                actor.chat_state_handle.get_conversation_len().await,
                0,
                "reminder must be buffered, not pushed mid-batch"
            );

            // The running turn's next safe point delivers the buffer.
            actor.flush_pending_skill_reminders().await;
            let conv = actor.chat_state_handle.get_conversation().await;
            assert_eq!(conv.len(), 1);
            let text = conv[0].text_content();
            assert!(
                text.contains("<system-reminder>"),
                "reminder must be system-reminder wrapped: {text}"
            );
            assert!(
                text.contains("Plan mode is active"),
                "reminder must carry the plan-mode activation text: {text}"
            );
            {
                let tracker = actor.plan_mode.lock();
                assert!(!tracker.has_pending_activation());
                // Delivery advanced the alternation: next injection is sparse.
                assert!(!tracker.should_use_full_reminder());
            }

            // Exactly-once: the turn flushes at multiple safe points (loop
            // top, after each tool batch, cancel/idle) — later flushes must
            // not deliver the reminder again.
            actor.flush_pending_skill_reminders().await;
            actor.flush_pending_skill_reminders().await;
            assert_eq!(
                actor.chat_state_handle.get_conversation_len().await,
                1,
                "repeated flushes must not duplicate the activation reminder"
            );
        })
        .await;
}

/// Idle toggle keeps the existing deferred behavior: tracker parks in
/// `Pending` and the reminder is injected at the next turn start
/// (`inject_plan_mode_reminders`), not buffered here.
#[tokio::test]
async fn idle_plan_toggle_stays_pending_without_buffered_reminder() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _gateway_rx) = build_actor().await;

            actor
                .handle_session_mode(acp::SessionModeId::new("plan"))
                .await;

            {
                let tracker = actor.plan_mode.lock();
                assert_eq!(
                    tracker.state(),
                    crate::session::plan_mode::PlanModeState::Pending,
                    "idle toggle must keep the deferred Pending flow"
                );
                assert!(!tracker.has_pending_activation());
            }
            assert_eq!(actor.chat_state_handle.get_conversation_len().await, 0);
        })
        .await;
}

/// Toggling OFF again before the buffered reminder is delivered withdraws it:
/// the model never saw plan mode, so the activation rolls back cleanly — no
/// stale "plan mode is active" delivery, no deferred exit, no exit reminder.
/// Covers Shift+Tab cycling past Plan (Plan → Auto) mid-turn.
#[tokio::test]
async fn midturn_toggle_off_withdraws_undelivered_reminder() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _gateway_rx) = build_actor().await;
            fake_running_turn(&actor).await;

            actor
                .handle_session_mode(acp::SessionModeId::new("plan"))
                .await;
            assert!(actor.plan_mode.lock().has_pending_activation());

            actor
                .handle_session_mode(acp::SessionModeId::new("default"))
                .await;
            {
                let tracker = actor.plan_mode.lock();
                assert_eq!(
                    tracker.state(),
                    crate::session::plan_mode::PlanModeState::Inactive,
                    "undelivered activation must roll back to Inactive, not ExitPending"
                );
                assert!(!tracker.has_pending_activation());
                assert!(
                    !tracker.has_pending_exit_reminder(),
                    "no exit reminder for an entry the model never saw"
                );
            }

            // Nothing plan-related reaches the conversation.
            actor.flush_pending_skill_reminders().await;
            assert_eq!(actor.chat_state_handle.get_conversation_len().await, 0);
        })
        .await;
}

/// Once the buffered reminder HAS been delivered, toggling OFF mid-turn takes
/// the normal deferred-exit path (`ExitPending`), and toggling back ON before
/// the turn ends re-enters `Active` without buffering a duplicate reminder.
#[tokio::test]
async fn midturn_reentry_after_delivery_buffers_no_duplicate_reminder() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _gateway_rx) = build_actor().await;
            fake_running_turn(&actor).await;

            // Enter plan mode mid-turn and deliver the reminder (drain point).
            actor
                .handle_session_mode(acp::SessionModeId::new("plan"))
                .await;
            actor.flush_pending_skill_reminders().await;
            assert_eq!(actor.chat_state_handle.get_conversation_len().await, 1);

            // Toggle off mid-turn: model saw plan mode → deferred exit.
            actor
                .handle_session_mode(acp::SessionModeId::new("default"))
                .await;
            assert_eq!(
                actor.plan_mode.lock().state(),
                crate::session::plan_mode::PlanModeState::ExitPending
            );

            // Back on before the turn ends: straight to Active, no new buffer.
            actor
                .handle_session_mode(acp::SessionModeId::new("plan"))
                .await;
            {
                let tracker = actor.plan_mode.lock();
                assert_eq!(
                    tracker.state(),
                    crate::session::plan_mode::PlanModeState::Active
                );
                assert!(
                    !tracker.has_pending_activation(),
                    "ExitPending → Active re-entry must not buffer a second reminder"
                );
            }
            assert_eq!(actor.chat_state_handle.get_conversation_len().await, 1);
        })
        .await;
}
