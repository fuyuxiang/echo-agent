//! Unit tests for the turn-finalize rails in [`super`] (`turn_completion`),
//! split out via `#[path]` to keep the module itself small.

use super::*;
use crate::app::agent::AgentState;
use crate::scrollback::block::RenderBlock;
use crate::scrollback::blocks::SessionEventBlock;
use crate::scrollback::state::ScrollbackState;
use std::path::PathBuf;
use std::time::Instant;

fn last_session_event(sb: &ScrollbackState) -> Option<SessionEvent> {
    (0..sb.len())
        .rev()
        .find_map(|i| match sb.get(i).map(|e| &e.block) {
            Some(RenderBlock::SessionEvent(b)) => Some(b.event.clone()),
            _ => None,
        })
}

/// A viewer in TurnRunning with an adopted prompt id, ready to be finalized.
fn running_viewer(prompt_id: &str) -> AgentView {
    let mut agent = super::super::agent_view::test_agent_view(Some("s1"), PathBuf::from("/tmp"));
    agent.attached_as_viewer = true;
    agent.session.start_turn(&mut agent.scrollback);
    agent.session.current_prompt_id = Some(prompt_id.into());
    agent.turn_started_at = Some(Instant::now());
    agent
}

/// A driver in TurnRunning with a local prompt id (default
/// `attached_as_viewer == false`).
fn running_driver(prompt_id: &str) -> AgentView {
    let mut agent = super::super::agent_view::test_agent_view(Some("s1"), PathBuf::from("/tmp"));
    agent.session.start_turn(&mut agent.scrollback);
    agent.session.current_prompt_id = Some(prompt_id.into());
    agent.turn_started_at = Some(Instant::now());
    agent
}

#[test]
fn viewer_finalize_idles_and_pushes_completed_marker() {
    let mut agent = running_viewer("p1");
    let outcome = finalize_turn_from_terminal(
        &mut agent,
        "s1",
        TerminalSignal {
            prompt_id: Some("p1"),
            stop_reason: Some("end_turn"),
            ..Default::default()
        },
    );
    assert!(matches!(outcome, TerminalApply::ViewerFinalized));
    assert!(agent.session.state.is_idle());
    assert!(agent.session.current_prompt_id.is_none());
    assert!(agent.turn_started_at.is_none());
    assert!(matches!(
        last_session_event(&agent.scrollback),
        Some(SessionEvent::TurnCompleted { .. })
    ));
}

fn one_stop_group() -> Vec<(String, Vec<crate::scrollback::blocks::tool::HookRunEntry>)> {
    use crate::scrollback::blocks::tool::{HookRunEntry, HookRunStatus};
    vec![(
        "stop".to_string(),
        vec![HookRunEntry {
            name: "global/notify".into(),
            status: HookRunStatus::Success {
                elapsed: std::time::Duration::from_millis(12),
            },
            output: None,
        }],
    )]
}

/// Stop-hook groups attached to the last session-event marker.
fn last_marker_groups(sb: &ScrollbackState) -> Option<usize> {
    (0..sb.len())
        .rev()
        .find_map(|i| match sb.get(i).map(|e| &e.block) {
            Some(RenderBlock::SessionEvent(b)) => Some(b.stop_hooks.len()),
            _ => None,
        })
}

fn count_lifecycle_blocks(sb: &ScrollbackState) -> usize {
    use crate::scrollback::blocks::tool::ToolCallBlock;
    (0..sb.len())
        .filter(|i| {
            matches!(
                sb.get(*i).map(|e| &e.block),
                Some(RenderBlock::ToolCall(ToolCallBlock::Lifecycle(_)))
            )
        })
        .count()
}

#[test]
fn marker_push_consumes_matching_stop_hook_stash() {
    let mut agent = running_driver("p1");
    agent.pending_stop_hooks = Some(super::super::agent_view::PendingStopHooks {
        prompt_id: Some("p1".into()),
        groups: one_stop_group(),
    });

    push_turn_terminal_marker(
        &mut agent,
        Some(SessionEvent::TurnCompleted {
            elapsed: Some(std::time::Duration::from_secs(2)),
        }),
        Some("p1"),
    );

    assert_eq!(
        last_marker_groups(&agent.scrollback),
        Some(1),
        "the stash must fold into the marker"
    );
    assert!(agent.pending_stop_hooks.is_none());
    assert_eq!(count_lifecycle_blocks(&agent.scrollback), 0);
}

#[test]
fn marker_push_flushes_stale_stash_standalone() {
    // A stash stamped with another turn's prompt id must not attach to
    // this marker — it flushes as the legacy standalone block.
    let mut agent = running_driver("p2");
    agent.pending_stop_hooks = Some(super::super::agent_view::PendingStopHooks {
        prompt_id: Some("p1".into()),
        groups: one_stop_group(),
    });

    push_turn_terminal_marker(
        &mut agent,
        Some(SessionEvent::TurnCompleted {
            elapsed: Some(std::time::Duration::from_secs(2)),
        }),
        Some("p2"),
    );

    assert_eq!(
        last_marker_groups(&agent.scrollback),
        Some(0),
        "a stale stash must not attach to the new marker"
    );
    assert_eq!(count_lifecycle_blocks(&agent.scrollback), 1);
    assert!(agent.pending_stop_hooks.is_none());
}

#[test]
fn marker_without_ending_pid_flushes_stamped_stash_standalone() {
    // A stamped stash can't be confirmed against a marker whose ending
    // turn id is missing — it flushes standalone instead of folding into
    // a marker it may not belong to.
    let mut agent = running_driver("p1");
    agent.pending_stop_hooks = Some(super::super::agent_view::PendingStopHooks {
        prompt_id: Some("p1".into()),
        groups: one_stop_group(),
    });

    push_turn_terminal_marker(
        &mut agent,
        Some(SessionEvent::TurnCompleted {
            elapsed: Some(std::time::Duration::from_secs(2)),
        }),
        None,
    );

    assert_eq!(
        last_marker_groups(&agent.scrollback),
        Some(0),
        "an unconfirmable stamped stash must not attach to the marker"
    );
    assert_eq!(count_lifecycle_blocks(&agent.scrollback), 1);
    assert!(agent.pending_stop_hooks.is_none());
}

#[test]
fn no_marker_flushes_stash_as_standalone_block() {
    // Turn ends without a marker (bash turn / rate-limit UX): the held
    // hooks still surface, in the legacy standalone form.
    let mut agent = running_driver("p1");
    agent.pending_stop_hooks = Some(super::super::agent_view::PendingStopHooks {
        prompt_id: Some("p1".into()),
        groups: one_stop_group(),
    });

    push_turn_terminal_marker(&mut agent, None, Some("p1"));

    assert_eq!(count_lifecycle_blocks(&agent.scrollback), 1);
    assert!(agent.pending_stop_hooks.is_none());
}

#[test]
fn viewer_finalize_consumes_stop_hook_stash() {
    // A viewer that stashed hooks mid-turn folds them into the marker the
    // finalize pushes.
    let mut agent = running_viewer("p1");
    agent.pending_stop_hooks = Some(super::super::agent_view::PendingStopHooks {
        prompt_id: Some("p1".into()),
        groups: one_stop_group(),
    });

    let _ = finalize_turn_from_terminal(
        &mut agent,
        "s1",
        TerminalSignal {
            prompt_id: Some("p1"),
            stop_reason: Some("end_turn"),
            ..Default::default()
        },
    );

    assert_eq!(last_marker_groups(&agent.scrollback), Some(1));
    assert!(agent.pending_stop_hooks.is_none());
}

#[test]
fn viewer_finalize_duplicate_terminal_is_noop() {
    let mut agent = running_viewer("p1");
    let _ = finalize_turn_from_terminal(
        &mut agent,
        "s1",
        TerminalSignal {
            prompt_id: Some("p1"),
            stop_reason: Some("end_turn"),
            ..Default::default()
        },
    );
    let len_after_first = agent.scrollback.len();

    // A duplicate/stale terminal for the now-finished turn does nothing.
    let outcome = finalize_turn_from_terminal(
        &mut agent,
        "s1",
        TerminalSignal {
            prompt_id: Some("p1"),
            stop_reason: Some("end_turn"),
            ..Default::default()
        },
    );
    assert!(matches!(outcome, TerminalApply::Ignored));
    assert!(agent.session.state.is_idle());
    assert_eq!(
        agent.scrollback.len(),
        len_after_first,
        "a duplicate terminal must not push a second marker"
    );
}

/// A cancelled terminal stamped `cancellationCategory: "HookDenied"` renders
/// the blocked-by-hook marker, never "cancelled by user" — a policy block is
/// not a user action (wrong story for an audit trail). Unknown categories and
/// absent meta (older shells) keep the user-cancel copy.
#[test]
fn viewer_finalize_hook_denied_renders_blocked_marker() {
    let mut agent = running_viewer("p1");
    let _ = finalize_turn_from_terminal(
        &mut agent,
        "s1",
        TerminalSignal {
            prompt_id: Some("p1"),
            stop_reason: Some("cancelled"),
            cancellation_category: Some(HOOK_DENIED_CATEGORY),
            ..Default::default()
        },
    );
    match last_session_event(&agent.scrollback) {
        Some(SessionEvent::TurnBlockedByHook { .. }) => {}
        other => panic!("expected TurnBlockedByHook, got {other:?}"),
    }

    // Unknown category → user-cancel copy (no false hook attribution).
    let mut agent = running_viewer("p1");
    let _ = finalize_turn_from_terminal(
        &mut agent,
        "s1",
        TerminalSignal {
            prompt_id: Some("p1"),
            stop_reason: Some("cancelled"),
            cancellation_category: Some("DoomLoop"),
            ..Default::default()
        },
    );
    assert!(matches!(
        last_session_event(&agent.scrollback),
        Some(SessionEvent::TurnCancelled { .. })
    ));
}

/// One chooser for every rail: only the hook-denied category flips the copy.
#[test]
fn cancelled_turn_event_picks_marker_by_category() {
    let d = std::time::Duration::from_millis(700);
    assert!(matches!(
        cancelled_turn_event(Some(HOOK_DENIED_CATEGORY), d),
        SessionEvent::TurnBlockedByHook { .. }
    ));
    assert!(matches!(
        cancelled_turn_event(None, d),
        SessionEvent::TurnCancelled { .. }
    ));
    assert_eq!(
        cancelled_turn_event(Some(HOOK_DENIED_CATEGORY), d).message(),
        "Turn blocked by a hook in 0.7s."
    );
}

#[test]
fn viewer_finalize_stop_reason_to_marker_mapping() {
    // cancelled → Turn cancelled.
    let mut agent = running_viewer("p1");
    let _ = finalize_turn_from_terminal(
        &mut agent,
        "s1",
        TerminalSignal {
            prompt_id: Some("p1"),
            stop_reason: Some("cancelled"),
            ..Default::default()
        },
    );
    assert!(matches!(
        last_session_event(&agent.scrollback),
        Some(SessionEvent::TurnCancelled { .. })
    ));

    // error (+agentResult) → TurnFailed with formatted text.
    let mut agent = running_viewer("p1");
    let _ = finalize_turn_from_terminal(
        &mut agent,
        "s1",
        TerminalSignal {
            prompt_id: Some("p1"),
            stop_reason: Some("error"),
            agent_result: Some(r#"API error (status 500): {"error":"boom"}"#),
            ..Default::default()
        },
    );
    match last_session_event(&agent.scrollback) {
        Some(SessionEvent::TurnFailed { error, .. }) => {
            assert_eq!(
                error,
                "Server error (500): Something went wrong on our side. Wait a minute and send again."
            );
        }
        other => panic!("expected TurnFailed, got {other:?}"),
    }

    // error with a dedicated banner already in the trailing run → the
    // banner explains the failure; no redundant TurnFailed marker.
    let mut agent = running_viewer("p1");
    agent
        .scrollback
        .push_block(RenderBlock::session_event(SessionEvent::RequestFailed {
            status: Some(500),
            headline: "Server error (500)".into(),
            detail: String::new(),
        }));
    let _ = finalize_turn_from_terminal(
        &mut agent,
        "s1",
        TerminalSignal {
            prompt_id: Some("p1"),
            stop_reason: Some("error"),
            ..Default::default()
        },
    );
    assert!(
        !matches!(
            last_session_event(&agent.scrollback),
            Some(SessionEvent::TurnFailed { .. })
        ),
        "a trailing RequestFailed banner must suppress the viewer's TurnFailed"
    );

    // …but a banner buried behind a substantive block is a previous turn's:
    // the trailing-run scan stops and the marker is pushed.
    let mut agent = running_viewer("p1");
    agent
        .scrollback
        .push_block(RenderBlock::session_event(SessionEvent::RequestFailed {
            status: Some(500),
            headline: "Server error (500)".into(),
            detail: String::new(),
        }));
    agent.scrollback.push_block(RenderBlock::user_prompt("hi"));
    let _ = finalize_turn_from_terminal(
        &mut agent,
        "s1",
        TerminalSignal {
            prompt_id: Some("p1"),
            stop_reason: Some("error"),
            ..Default::default()
        },
    );
    assert!(
        matches!(
            last_session_event(&agent.scrollback),
            Some(SessionEvent::TurnFailed { .. })
        ),
        "a banner behind a substantive block must not suppress the marker"
    );

    // rate_limit → finished, but no marker (not actionable from a viewer).
    let mut agent = running_viewer("p1");
    let _ = finalize_turn_from_terminal(
        &mut agent,
        "s1",
        TerminalSignal {
            prompt_id: Some("p1"),
            stop_reason: Some("rate_limit"),
            ..Default::default()
        },
    );
    assert!(agent.session.state.is_idle());
    assert!(
        last_session_event(&agent.scrollback).is_none(),
        "rate_limit must not push a marker on a viewer"
    );

    // unknown/other reason → Turn completed (the catch-all).
    let mut agent = running_viewer("p1");
    let _ = finalize_turn_from_terminal(
        &mut agent,
        "s1",
        TerminalSignal {
            prompt_id: Some("p1"),
            stop_reason: Some("max_tokens"),
            ..Default::default()
        },
    );
    assert!(matches!(
        last_session_event(&agent.scrollback),
        Some(SessionEvent::TurnCompleted { .. })
    ));
}

#[test]
fn driver_arms_reconcile_and_does_not_finish() {
    let mut agent = running_driver("p1");
    let outcome = finalize_turn_from_terminal(
        &mut agent,
        "s1",
        TerminalSignal {
            prompt_id: Some("p1"),
            stop_reason: Some("cancelled"),
            ..Default::default()
        },
    );
    assert!(matches!(outcome, TerminalApply::ReconcileArmed));
    assert!(
        matches!(agent.session.state, AgentState::TurnRunning),
        "the driver's turn must NOT be finished — the PromptResponse RPC owns it"
    );
    let pending = agent
        .pending_turn_end_reconcile
        .as_ref()
        .expect("the driver's awaited turn must arm a reconcile");
    assert_eq!(pending.prompt_id, "p1");
    assert_eq!(pending.stop_reason.as_deref(), Some("cancelled"));
}

#[test]
fn driver_mismatched_prompt_id_does_not_arm() {
    // Stale/peer terminal must not arm reconcile on a different live turn.
    let mut agent = running_driver("p1");
    let outcome = finalize_turn_from_terminal(
        &mut agent,
        "s1",
        TerminalSignal {
            prompt_id: Some("p-other"),
            stop_reason: Some("end_turn"),
            ..Default::default()
        },
    );
    assert!(matches!(outcome, TerminalApply::Ignored));
    assert!(agent.pending_turn_end_reconcile.is_none());
    assert!(matches!(agent.session.state, AgentState::TurnRunning));
}

#[test]
fn driver_missing_prompt_id_arms_against_current_when_idle_in_turn() {
    let mut agent = running_driver("p1");
    let outcome = finalize_turn_from_terminal(
        &mut agent,
        "s1",
        TerminalSignal {
            stop_reason: Some("end_turn"),
            ..Default::default()
        },
    );
    assert!(matches!(outcome, TerminalApply::ReconcileArmed));
    assert_eq!(
        agent.pending_turn_end_reconcile.as_ref().unwrap().prompt_id,
        "p1"
    );
}

fn stream_agent_text(agent: &mut AgentView, text: &str) {
    use crate::acp::meta::NotificationMeta;
    use agent_client_protocol as acp;
    let meta = NotificationMeta::default();
    let _ = agent.session.tracker.handle_update(
        acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
            acp::TextContent::new(text),
        ))),
        &meta,
        &mut agent.scrollback,
    );
}

/// Missing wire pid still arms lost-PR reconcile (full teardown
/// lives in `reconcile_overdue_turn_ends` / PromptResponse).
#[test]
fn repro_terminal_without_prompt_id_arms_reconcile_for_lost_pr() {
    let mut agent = running_driver("p1");
    stream_agent_text(&mut agent, "done");
    let outcome = finalize_turn_from_terminal(
        &mut agent,
        "s1",
        TerminalSignal {
            stop_reason: Some("end_turn"),
            ..Default::default()
        },
    );
    assert!(matches!(outcome, TerminalApply::ReconcileArmed));
    assert!(agent.pending_turn_end_reconcile.is_some());
    assert!(
        matches!(agent.session.state, AgentState::TurnRunning),
        "driver stays TurnRunning until PR or overdue reconcile"
    );
    assert_eq!(
        agent.session.tracker.activity(),
        Some(crate::acp::tracker::TurnActivity::Responding)
    );
}

/// Armed, the overdue reconcile would force-finish the live turn mid-write.
#[test]
fn driver_missing_prompt_id_ignored_during_tool_call_write() {
    let mut agent = running_driver("p1");
    assert!(
        agent
            .session
            .tracker
            .note_tool_call_arguments_delta(Some("spawn_subagent"), 0)
    );
    assert!(matches!(
        agent.session.tracker.activity(),
        Some(crate::acp::tracker::TurnActivity::WritingToolCall(_))
    ));

    let outcome = finalize_turn_from_terminal(
        &mut agent,
        "s1",
        TerminalSignal {
            stop_reason: Some("end_turn"),
            ..Default::default()
        },
    );
    assert!(matches!(outcome, TerminalApply::Ignored));
    assert!(
        agent.pending_turn_end_reconcile.is_none(),
        "mid-write terminal must not arm the lost-PR reconcile"
    );
    assert!(matches!(agent.session.state, AgentState::TurnRunning));
}

/// A dead stream mid-write must not block lost-response recovery.
#[test]
fn driver_missing_prompt_id_arms_when_tool_call_write_is_stale() {
    let mut agent = running_driver("p1");
    agent
        .session
        .tracker
        .note_tool_call_arguments_delta(Some("spawn_subagent"), 0);
    agent.session.tracker.backdate_last_tool_call_delta(
        crate::acp::tracker::WRITING_DELTA_STALE_AFTER + std::time::Duration::from_secs(1),
    );

    let outcome = finalize_turn_from_terminal(
        &mut agent,
        "s1",
        TerminalSignal {
            stop_reason: Some("end_turn"),
            ..Default::default()
        },
    );
    assert!(matches!(outcome, TerminalApply::ReconcileArmed));
    assert_eq!(
        agent.pending_turn_end_reconcile.as_ref().unwrap().prompt_id,
        "p1"
    );
}

/// Exact pid still arms (control).
#[test]
fn recovery_mode_matching_turn_completed_arms_reconcile_for_lost_pr() {
    let mut agent = running_driver("p1");
    stream_agent_text(&mut agent, "done");

    let outcome = finalize_turn_from_terminal(
        &mut agent,
        "s1",
        TerminalSignal {
            prompt_id: Some("p1"),
            stop_reason: Some("end_turn"),
            ..Default::default()
        },
    );
    assert!(matches!(outcome, TerminalApply::ReconcileArmed));
    assert!(matches!(agent.session.state, AgentState::TurnRunning));
    assert!(agent.pending_turn_end_reconcile.is_some());
}

/// Re-arm same pid keeps earliest received_at (does not extend grace forever).
#[test]
fn driver_rearm_same_pid_preserves_received_at() {
    let mut agent = running_driver("p1");
    let _ = finalize_turn_from_terminal(
        &mut agent,
        "s1",
        TerminalSignal {
            prompt_id: Some("p1"),
            stop_reason: Some("end_turn"),
            ..Default::default()
        },
    );
    let first = agent
        .pending_turn_end_reconcile
        .as_ref()
        .unwrap()
        .received_at;
    std::thread::sleep(std::time::Duration::from_millis(5));
    let _ = finalize_turn_from_terminal(
        &mut agent,
        "s1",
        TerminalSignal {
            prompt_id: Some("p1"),
            stop_reason: Some("end_turn"),
            ..Default::default()
        },
    );
    let second = agent
        .pending_turn_end_reconcile
        .as_ref()
        .unwrap()
        .received_at;
    assert_eq!(first, second);
}

// ── End markers: always the plain event text (work lives in the status row) ──

fn insert_bg_task(agent: &mut AgentView, task_id: &str, is_monitor: bool) {
    agent.session.bg_tasks.insert(
        task_id.into(),
        crate::app::agent::BgTaskState {
            task_id: task_id.into(),
            tool_call_id: format!("call-{task_id}"),
            command: "sleep 5".into(),
            description: None,
            cwd: "/tmp".into(),
            output_file: "/tmp/out".into(),
            status: crate::app::agent::BgTaskStatus::Running,
            start_time: std::time::SystemTime::now(),
            end_time: None,
            exit_code: None,
            signal: None,
            stdout: String::new(),
            stdout_line_count: 0,
            truncated: false,
            pending_kill: false,
            kill_requested_at: None,
            scrollback_entry_id: None,
            is_monitor,
            restored_from_replay: false,
        },
    );
}

/// The newest session-event marker block.
fn last_marker_block(agent: &AgentView) -> &SessionEventBlock {
    (0..agent.scrollback.len())
        .rev()
        .find_map(|i| match agent.scrollback.get(i).map(|e| &e.block) {
            Some(RenderBlock::SessionEvent(b)) => Some(b),
            _ => None,
        })
        .expect("a session-event marker must exist")
}

#[test]
fn real_end_marker_stays_plain_with_running_work() {
    let mut agent = running_driver("p1");
    insert_bg_task(&mut agent, "bg-1", false);

    push_turn_terminal_marker(
        &mut agent,
        Some(SessionEvent::TurnCompleted {
            elapsed: Some(std::time::Duration::from_secs(2)),
        }),
        Some("p1"),
    );

    let block = last_marker_block(&agent);
    assert_eq!(block.prompt_id.as_deref(), Some("p1"));
    assert_eq!(block.event.message(), "Worked for 2.0s");
    assert_eq!(
        agent.watchers().commands,
        1,
        "the running command feeds the status-row watchers cue instead"
    );
}

#[test]
fn workless_marker_renders_legacy_text() {
    let mut agent = running_driver("p1");

    push_turn_terminal_marker(
        &mut agent,
        Some(SessionEvent::TurnCompleted {
            elapsed: Some(std::time::Duration::from_secs(2)),
        }),
        Some("p1"),
    );

    let block = last_marker_block(&agent);
    assert_eq!(block.event.message(), "Worked for 2.0s");
}

// ── Send-now cancel marker suppression (viewer finalize rail) ────────

/// A viewer finalizing a `send_now`-stamped `cancelled` terminal pushes no marker.
#[test]
fn viewer_finalize_suppresses_send_now_cancel_marker() {
    let mut agent = running_viewer("p1");
    let outcome = finalize_turn_from_terminal(
        &mut agent,
        "s1",
        TerminalSignal {
            prompt_id: Some("p1"),
            stop_reason: Some("cancelled"),
            cancel_trigger: Some("send_now"),
            ..Default::default()
        },
    );
    assert!(matches!(outcome, TerminalApply::ViewerFinalized));
    assert!(agent.session.state.is_idle(), "the turn still finishes");
    assert!(
        last_session_event(&agent.scrollback).is_none(),
        "a send-now cancel pushes no marker on a viewer"
    );

    // A non-send-now trigger keeps the marker even with a local expectation armed (wire is authoritative).
    let mut agent = running_viewer("p1");
    agent.expect_send_now_cancel = Some("p-mine".into());
    let _ = finalize_turn_from_terminal(
        &mut agent,
        "s1",
        TerminalSignal {
            prompt_id: Some("p1"),
            stop_reason: Some("cancelled"),
            cancel_trigger: Some("ctrl_c"),
            ..Default::default()
        },
    );
    assert!(matches!(
        last_session_event(&agent.scrollback),
        Some(SessionEvent::TurnCancelled { .. })
    ));

    // Older shell (no meta): the armed expectation is the fallback.
    let mut agent = running_viewer("p1");
    agent.expect_send_now_cancel = Some("p-mine".into());
    let _ = finalize_turn_from_terminal(
        &mut agent,
        "s1",
        TerminalSignal {
            prompt_id: Some("p1"),
            stop_reason: Some("cancelled"),
            ..Default::default()
        },
    );
    assert!(
        last_session_event(&agent.scrollback).is_none(),
        "the armed expectation suppresses the marker without wire meta"
    );
    assert!(
        agent.expect_send_now_cancel.is_none(),
        "the expectation is consumed on the viewer finalize"
    );
}

/// The driver arm records the broadcast's `cancelTrigger` for a lost-RPC reconcile.
#[test]
fn driver_arm_records_cancel_trigger_for_reconcile() {
    let mut agent = running_driver("p1");
    let outcome = finalize_turn_from_terminal(
        &mut agent,
        "s1",
        TerminalSignal {
            prompt_id: Some("p1"),
            stop_reason: Some("cancelled"),
            cancel_trigger: Some("send_now"),
            cancellation_category: Some(HOOK_DENIED_CATEGORY),
            ..Default::default()
        },
    );
    assert!(matches!(outcome, TerminalApply::ReconcileArmed));
    let pending = agent
        .pending_turn_end_reconcile
        .as_ref()
        .expect("reconcile must be armed");
    assert_eq!(pending.cancel_trigger.as_deref(), Some("send_now"));
    assert_eq!(
        pending.cancellation_category.as_deref(),
        Some(HOOK_DENIED_CATEGORY),
        "the armed pending must park the category for the reconcile sweep"
    );
}

/// The turn-end marker takes no fold path — a park has no row to fold into.
#[test]
fn turn_end_after_park_pushes_single_marker() {
    use crate::app::agent_view::test_fixtures::count_turn_markers;

    let mut agent = running_driver("p1");
    super::super::agent_view::test_fixtures::simulate_task_output_wait(&mut agent, "bg-1");
    assert!(agent.renders_parked());
    assert_eq!(count_turn_markers(&agent), 0, "the park writes no marker");

    push_turn_terminal_marker(
        &mut agent,
        Some(SessionEvent::TurnCompleted {
            elapsed: Some(std::time::Duration::from_secs(5)),
        }),
        Some("p1"),
    );

    assert_eq!(
        count_turn_markers(&agent),
        1,
        "the real turn end pushes exactly one marker"
    );
    assert_eq!(last_marker_block(&agent).event.message(), "Worked for 5.0s");
}
