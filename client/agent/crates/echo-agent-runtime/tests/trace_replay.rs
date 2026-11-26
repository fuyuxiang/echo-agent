//! Replay-trace verification harness for the turn-end TodoGate.
//!
//! Reads synthetic JSON fixtures from `tests/fixtures/synthetic_*.json`,
//! walks each turn, and at every assistant `end-of-turn` snapshot asserts
//! that `evaluate_todo_gate` returns the decision the fixture declares.
//!
//! Pure-function integration test — no `SessionActor`, no completion
//! stream. Complements unit tests of the pure function with
//! data-driven trace-replay coverage.
//!
//! Data-driven: dropping a new `synthetic_*.json` fixture into
//! `tests/fixtures/` enrolls it in the harness automatically.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use echo_agent_runtime::session::{
    CollectedTodoGateInput, TodoGateDecision, TodoGateReason, evaluate_todo_gate,
};
use echo_agent_runtime::tools::todo::TodoStatus;

/// Closed canonical set of shipped fixtures. Adding a fixture
/// here is the one required Rust-side change when a new failure shape
/// is enrolled — `canonical_fixtures_present` asserts set equality
/// against this list so a missing-or-extra fixture fails the harness.
const CANONICAL_FIXTURES: &[&str] = &[
    "synthetic_clean_completion.json",
    "synthetic_pr_babysit_partial_backing.json",
    "synthetic_stranded_narration.json",
];

#[derive(Debug, Deserialize)]
struct Fixture {
    name: String,
    #[allow(dead_code)] // human-readable; surfaced only on assertion failure
    description: String,
    turns: Vec<Turn>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Turn {
    #[allow(dead_code)] // user turns are walked but never gate-evaluated
    User(UserTurn),
    Assistant(AssistantTurn),
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct UserTurn {
    turn_index: usize,
    content: String,
}

#[derive(Debug, Deserialize)]
struct AssistantTurn {
    turn_index: usize,
    #[allow(dead_code)] // present for fixture clarity; the gate does not consult it
    tool_calls_emitted: Vec<serde_json::Value>,
    todo_state_after_turn: Vec<TodoSnapshot>,
    backing_task_count: usize,
    expected_gate_decision: ExpectedGateDecision,
    #[serde(default)]
    expected_reason: Option<ExpectedReason>,
    #[serde(default)]
    expected_reminder_contains: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct TodoSnapshot {
    #[allow(dead_code)] // id is preserved for fixture readability
    id: String,
    status: TodoStatus,
    content: String,
}

/// Typed mirror of the fixture's `expected_gate_decision` field. A
/// closed enum (not `String`) catches typos at deserialize time rather
/// than silently passing the wrong assertion branch.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ExpectedGateDecision {
    Nudge,
    Continue,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ExpectedReason {
    InFlight,
}

impl From<TodoGateReason> for ExpectedReason {
    fn from(reason: TodoGateReason) -> Self {
        match reason {
            TodoGateReason::InFlight => Self::InFlight,
        }
    }
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Every `synthetic_*.json` in `tests/fixtures/`, sorted for stable
/// output. Directory-iteration IO errors panic with diagnostic context
/// rather than being silently dropped (a permissioned-out fixture must
/// not vanish from the set unnoticed).
fn discover_fixtures() -> Vec<PathBuf> {
    let dir = fixtures_dir();
    let entries = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read fixtures dir {}: {e}", dir.display()));
    let mut paths: Vec<PathBuf> = entries
        .map(|entry| {
            entry
                .unwrap_or_else(|e| panic!("read entry in {}: {e}", dir.display()))
                .path()
        })
        .filter(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|name| name.starts_with("synthetic_") && name.ends_with(".json"))
        })
        .collect();
    paths.sort();
    assert!(
        !paths.is_empty(),
        "no synthetic_*.json fixtures found in {}",
        dir.display()
    );
    paths
}

fn load_fixture(path: &Path) -> Fixture {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

/// Consume a fixture's assistant-turn snapshot into a
/// `CollectedTodoGateInput`. Moves todo `content` strings out of the
/// snapshot — no clones. Calling `as_input()` on the result also
/// re-exercises the production "first N in_progress are backed"
/// partition heuristic.
fn collected_from(
    snapshots: Vec<TodoSnapshot>,
    backing_task_count: usize,
) -> CollectedTodoGateInput {
    let todos = snapshots
        .into_iter()
        .map(|t| (t.id, t.content, t.status))
        .collect();
    CollectedTodoGateInput {
        todos,
        backing_task_count,
    }
}

/// Run the gate against one assistant turn. `fixture_name` is only used
/// in panic messages so failures point straight at the offending JSON.
fn check_assistant_turn(fixture_name: &str, turn: AssistantTurn) {
    let AssistantTurn {
        turn_index,
        tool_calls_emitted: _,
        todo_state_after_turn,
        backing_task_count,
        expected_gate_decision,
        expected_reason,
        expected_reminder_contains,
    } = turn;

    let collected = collected_from(todo_state_after_turn, backing_task_count);
    let input = collected.as_input();
    let decision = evaluate_todo_gate(&input);

    match (expected_gate_decision, decision) {
        (ExpectedGateDecision::Continue, TodoGateDecision::Continue) => {
            assert!(
                expected_reason.is_none(),
                "fixture {fixture_name} turn {turn_index} declares `continue` with \
                 `expected_reason` — a continue decision has no reason",
            );
            assert!(
                expected_reminder_contains.is_empty(),
                "fixture {fixture_name} turn {turn_index} declares `continue` with \
                 `expected_reminder_contains` — a continue decision emits no reminder",
            );
        }
        (ExpectedGateDecision::Nudge, TodoGateDecision::Nudge { reminder, reason }) => {
            if let Some(expected) = expected_reason {
                assert_eq!(
                    expected,
                    ExpectedReason::from(reason),
                    "fixture {fixture_name} turn {turn_index}: gate reason mismatch",
                );
            }
            for needle in &expected_reminder_contains {
                assert!(
                    reminder.contains(needle.as_str()),
                    "fixture {fixture_name} turn {turn_index}: reminder missing substring \
                     {needle:?}.\nFull reminder:\n{reminder}",
                );
            }
        }
        (expected, TodoGateDecision::Continue) => {
            panic!("fixture {fixture_name} turn {turn_index}: expected {expected:?}, got Continue",)
        }
        (expected, TodoGateDecision::Nudge { reason, .. }) => panic!(
            "fixture {fixture_name} turn {turn_index}: expected {expected:?}, got Nudge({reason:?})",
        ),
    }
}

#[test]
fn replay_all_synthetic_fixtures() {
    for path in discover_fixtures() {
        let fixture = load_fixture(&path);
        let Fixture {
            name,
            description: _,
            turns,
        } = fixture;
        let mut saw_assistant = false;
        for turn in turns {
            match turn {
                Turn::User(_) => {}
                Turn::Assistant(at) => {
                    saw_assistant = true;
                    check_assistant_turn(&name, at);
                }
            }
        }
        assert!(
            saw_assistant,
            "fixture {name} ({}) has no assistant turns to evaluate",
            path.display(),
        );
    }
}

/// Set-equality guard: the on-disk fixture set must exactly equal
/// [`CANONICAL_FIXTURES`]. Adding a fixture without updating the
/// constant — or losing one — fails the harness. Closes the
/// "open-ended presence check" gap.
#[test]
fn canonical_fixtures_match_disk() {
    let actual: BTreeSet<String> = discover_fixtures()
        .iter()
        .map(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_else(|| panic!("non-UTF8 fixture path: {}", p.display()))
                .to_string()
        })
        .collect();
    let expected: BTreeSet<String> = CANONICAL_FIXTURES
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    assert_eq!(actual, expected, "fixture set drift vs CANONICAL_FIXTURES");
}

/// Compile-time guard: `ExpectedGateDecision` must stay a closed
/// two-variant enum so a fixture typo fails to load instead of
/// silently passing the wrong branch.
#[test]
fn expected_decision_is_closed() {
    let parsed: ExpectedGateDecision = serde_json::from_str(r#""nudge""#).unwrap();
    assert_eq!(parsed, ExpectedGateDecision::Nudge);
    let parsed: ExpectedGateDecision = serde_json::from_str(r#""continue""#).unwrap();
    assert_eq!(parsed, ExpectedGateDecision::Continue);
    let err = serde_json::from_str::<ExpectedGateDecision>(r#""maybe""#).unwrap_err();
    assert!(
        err.to_string().contains("unknown variant"),
        "expected unknown-variant error, got: {err}"
    );
}
