use super::{
    CollectedTodoGateInput, TodoGateDecision, TodoGateInput, TodoGateReason,
    build_todo_gate_reminder, evaluate_todo_gate,
};
use crate::tools::todo::TodoStatus;
use echo_agent_tools::types::template_renderer::TemplateRenderer;
use echo_agent_tools::types::tool::ToolKind;
use std::collections::HashMap;

// ── TodoGate pure-function tests ──────────────────────────────────
//
// Integration coverage lands via the replay harness.
// These tests cover the gate's decision function plus the reminder
// builders.

#[test]
fn todo_gate_fires_when_pending_remains() {
    let input = TodoGateInput {
        pending: vec!["fix-round-1"],
        in_progress_unbacked: vec![],
        in_progress_backed: vec![],
        backing_task_count: 0,
    };
    assert!(matches!(
        evaluate_todo_gate(&input),
        TodoGateDecision::Nudge { .. }
    ));
}

#[test]
fn todo_gate_passes_when_in_progress_count_le_backing_count() {
    // One in-progress item, one live backing task → backed → no nudge.
    let input = TodoGateInput {
        pending: vec![],
        in_progress_unbacked: vec![],
        in_progress_backed: vec!["review-round-2"],
        backing_task_count: 1,
    };
    assert!(matches!(
        evaluate_todo_gate(&input),
        TodoGateDecision::Continue
    ));
}

#[test]
fn todo_gate_fires_when_in_progress_exceeds_backing_count() {
    // The `/pr-babysit` false-positive regression test: 3 PR todos
    // in_progress but only 1 polling subagent → 2 unbacked.
    let input = TodoGateInput {
        pending: vec![],
        in_progress_unbacked: vec!["pr-2:ci-green", "pr-3:ci-green"],
        in_progress_backed: vec!["pr-1:ci-green"],
        backing_task_count: 1,
    };
    let decision = evaluate_todo_gate(&input);
    let TodoGateDecision::Nudge { reminder, reason } = decision else {
        panic!("expected Nudge when in_progress exceeds backing count");
    };
    assert_eq!(reason, TodoGateReason::InFlight);
    // The reminder must surface the unbacked items so the model
    // knows which ones to advance.
    assert!(reminder.contains("pr-2:ci-green"));
    assert!(reminder.contains("pr-3:ci-green"));
    // Backed items are deliberately NOT listed — the gate already
    // decided not to nudge on them, so re-listing them would be
    // noise.
    assert!(!reminder.contains("pr-1:ci-green"));
}

#[test]
fn todo_gate_reminder_renders_plan_tool_name() {
    let raw = build_todo_gate_reminder(&["fix-round-1"], &[]);
    let renderer = TemplateRenderer::new(
        HashMap::from([(ToolKind::Plan, "todo_write".to_string())]),
        HashMap::new(),
    );
    let rendered = renderer.render(&raw).unwrap();
    assert!(
        rendered.contains("todo_write"),
        "rendered reminder must contain the model-facing plan tool name"
    );
    assert!(
        !rendered.contains("${{"),
        "no unresolved template tokens, got:\n{rendered}"
    );
}

// Interaction with the existing periodic TodoNudge reminder: they
// address different concerns. The design intentionally
// separates them, so the gate's reminder must use the gate's own
// vocabulary — not the periodic-nudge phrasing. The real
// `TodoNudgeState::try_fire` text is gated behind `&mut self` +
// private counter fields, so we keep the assertion to the gate
// side (positive: the gate uses its own phrasing; the periodic
// nudge's signature phrase must not leak in).
#[test]
fn todo_gate_has_its_own_vocabulary() {
    let gate = build_todo_gate_reminder(&["only-pending"], &[]);
    // Gate's signature phrase — distinguishes it from the periodic
    // TodoNudge ("hasn't been used recently") in dashboards and
    // model-side debugging.
    assert!(
        gate.contains("ended your turn"),
        "gate reminder must use its own signature phrase, got:\n{gate}"
    );
    // The periodic-nudge text from
    // `echo_agent_tools::reminders::todo_nudge::try_fire` is "The {}
    // tool hasn't been used recently…" — leaking that phrase into
    // the gate's body would conflate the two reminders.
    assert!(
        !gate.contains("hasn't been used recently"),
        "gate must not borrow the periodic-nudge phrasing, got:\n{gate}"
    );
}

// The integration block uses a bare `<` comparison —
//   `if todo_gate_fires < gate_cfg.max_fires_per_prompt { ... }`
// — so the cap logic is exercised end-to-end by the replay
// harness; the property covered here is just the off-by-one shape
// ("cap=N permits exactly N fires"). Mirrors the loop the
// production code runs but counts loop iterations separately so a
// future regression that decouples `fires` from `nudged` would
// surface.
#[test]
fn fires_lt_cap_permits_exactly_cap_fires() {
    let cap = 2u32;
    let total_iterations = 5;
    let mut fires = 0u32;
    let mut nudged = 0u32;
    for _ in 0..total_iterations {
        if fires < cap {
            fires += 1;
            nudged += 1;
        }
    }
    assert_eq!(
        nudged, cap,
        "cap-N must permit exactly N fires across more-than-N iterations"
    );
    assert_eq!(fires, cap);
}

#[test]
fn fires_lt_cap_zero_blocks_every_iteration() {
    // Observation-only / operator-disabled mode: with cap=0 the
    // production predicate `todo_gate_fires < cap` must be `false`
    // for every `fires` value the counter could reach.
    // Cap and `fires` come from a runtime variable (`black_box`)
    // so clippy doesn't constant-fold the comparison away.
    let cap = std::hint::black_box(0u32);
    for fires in 0u32..=8 {
        let permitted = std::hint::black_box(fires) < cap;
        assert!(!permitted, "cap=0 must never permit a fire (fires={fires})");
    }
}

#[test]
fn todo_gate_empty_state_no_compaction_passes() {
    // Degenerate-input test: empty everything → no nudge. Required
    // because the gate is reachable on the very first content-only
    // turn of a session before any todo_write has happened.
    let input = TodoGateInput {
        pending: vec![],
        in_progress_unbacked: vec![],
        in_progress_backed: vec![],
        backing_task_count: 0,
    };
    assert!(matches!(
        evaluate_todo_gate(&input),
        TodoGateDecision::Continue
    ));
}

#[test]
fn todo_gate_reminder_omits_empty_sections() {
    // Only the populated sections render; empty buckets are dropped.
    // The backed-in-progress bucket is never listed (deliberately
    // removed — the gate already decided not to nudge on those).
    let r = build_todo_gate_reminder(&["only-pending"], &[]);
    assert!(r.contains("Pending:"));
    assert!(!r.contains("In-progress (no backing"));
    assert!(!r.contains("backed by a live background task"));
}

// ── `CollectedTodoGateInput::as_input` partition heuristic ───────
//
// The "first N in_progress are backed (insertion order); pending is
// never backed" rule is the design's primary fix for the
// `/pr-babysit` false-positive. Earlier tests constructed the
// partition by hand —
// these tests exercise the real `as_input` against owned input.

fn collected(
    items: &[(&str, &str, TodoStatus)],
    backing_task_count: usize,
) -> CollectedTodoGateInput {
    CollectedTodoGateInput {
        todos: items
            .iter()
            .map(|(id, content, status)| ((*id).to_string(), (*content).to_string(), *status))
            .collect(),
        backing_task_count,
    }
}

#[test]
fn as_input_marks_everything_unbacked_when_no_backing_tasks() {
    // (a) backing_count = 0 with one in_progress → all unbacked.
    let c = collected(&[("ip", "do work", TodoStatus::InProgress)], 0);
    let input = c.as_input();
    assert_eq!(input.in_progress_backed, Vec::<&str>::new());
    assert_eq!(input.in_progress_unbacked, vec!["do work"]);
    assert!(input.pending.is_empty());
}

#[test]
fn as_input_marks_all_backed_when_backing_count_ge_in_progress() {
    // (b) backing_count >= |in_progress| → all backed, none unbacked.
    let c = collected(
        &[
            ("a", "alpha", TodoStatus::InProgress),
            ("b", "bravo", TodoStatus::InProgress),
        ],
        5,
    );
    let input = c.as_input();
    // Insertion order preserved: alpha before bravo.
    assert_eq!(input.in_progress_backed, vec!["alpha", "bravo"]);
    assert!(input.in_progress_unbacked.is_empty());
}

#[test]
fn as_input_partitions_first_n_as_backed() {
    // (c) backing_count = 1, |in_progress| = 3 → 1 backed + 2 unbacked.
    // This is the `/pr-babysit` regression: 3 PR todos, 1 poller.
    let c = collected(
        &[
            ("pr-1", "pr-1:ci-green", TodoStatus::InProgress),
            ("pr-2", "pr-2:ci-green", TodoStatus::InProgress),
            ("pr-3", "pr-3:ci-green", TodoStatus::InProgress),
        ],
        1,
    );
    let input = c.as_input();
    // Insertion order: pr-1 is backed; pr-2 / pr-3 are unbacked.
    assert_eq!(input.in_progress_backed, vec!["pr-1:ci-green"]);
    assert_eq!(
        input.in_progress_unbacked,
        vec!["pr-2:ci-green", "pr-3:ci-green"]
    );
}

#[test]
fn as_input_pending_never_backed_even_with_high_backing_count() {
    // (d) pending items never count as backed, regardless of count.
    let c = collected(
        &[
            ("p", "pending-task", TodoStatus::Pending),
            ("ip", "in-progress-task", TodoStatus::InProgress),
        ],
        100,
    );
    let input = c.as_input();
    // Pending bucket carries the pending item.
    assert_eq!(input.pending, vec!["pending-task"]);
    // The single in-progress item is backed (count >= 1) but the
    // pending item does NOT appear in either in_progress bucket.
    assert_eq!(input.in_progress_backed, vec!["in-progress-task"]);
    assert!(input.in_progress_unbacked.is_empty());
}

#[test]
fn as_input_completed_and_cancelled_are_dropped() {
    // Completed/cancelled items aren't actionable for the gate and
    // must not appear in any output bucket. They also must not
    // shift the insertion-order partition for in_progress items.
    let c = collected(
        &[
            ("done", "done-task", TodoStatus::Completed),
            ("ip-1", "first-ip", TodoStatus::InProgress),
            ("cancel", "cancelled-task", TodoStatus::Cancelled),
            ("ip-2", "second-ip", TodoStatus::InProgress),
        ],
        1,
    );
    let input = c.as_input();
    assert!(input.pending.is_empty());
    // Insertion-order partition is computed AFTER completed /
    // cancelled are filtered out: `first-ip` (which appears
    // before `second-ip` in `todos`) is the one backed slot.
    assert_eq!(input.in_progress_backed, vec!["first-ip"]);
    assert_eq!(input.in_progress_unbacked, vec!["second-ip"]);
}
