use super::*;
use crate::persistence::ResourcesPersistence;
use crate::types::resources::{Resources, State};
use chrono::{TimeZone, Utc};

const GENERATION: &str = "01890f42-7d5c-7c00-8000-000000000001";

fn uuid(suffix: u64) -> uuid::Uuid {
    uuid::Uuid::parse_str(&format!("01890f42-7d5c-7c00-8000-{suffix:012x}")).unwrap()
}

fn task(id: &str, recurring: bool, durable: bool) -> ScheduledTask {
    ScheduledTask {
        id: id.into(),
        interval_secs: 300,
        prompt: format!("run {id}"),
        recurring,
        durable,
        foreground: true,
        created_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
        last_fired_at: None,
        expires_at: None,
        last_subagent_id: None,
        iterations_since_fresh: 0,
        chain_reset_pending: false,
    }
}

fn version(generation: &str, revision: u64) -> SchedulerVersion {
    SchedulerVersion::from_parts(uuid::Uuid::parse_str(generation).unwrap(), revision)
}

fn versions(revision: u64) -> ScheduledOccurrenceVersions {
    ScheduledOccurrenceVersions::try_new(
        version(GENERATION, revision),
        version(GENERATION, revision + 1),
    )
    .unwrap()
}

fn occurrence_json(
    id: &str,
    task: serde_json::Value,
    versions: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({ "occurrenceId": id, "task": task, "versions": versions })
}

fn valid_occurrence_json(id_suffix: u64, task_id: &str, revision: u64) -> serde_json::Value {
    occurrence_json(
        &uuid(id_suffix).to_string(),
        serde_json::to_value(task(task_id, false, true)).unwrap(),
        serde_json::json!({
            "fire": { "generation": GENERATION, "revision": revision },
            "removal": { "generation": GENERATION, "revision": revision + 1 },
        }),
    )
}

fn state(tasks: Vec<ScheduledTask>, journal: serde_json::Value) -> SchedulerState {
    serde_json::from_value(serde_json::json!({
        "tasks": tasks,
        "occurrenceJournal": journal
    }))
    .unwrap()
}

fn prepare(state: &mut SchedulerState, task_id: &str, revision: u64) -> OneShotOccurrence {
    state
        .prepare_one_shot_occurrence_with_id(
            ScheduledOccurrenceId(uuid(100 + revision)),
            task_id,
            versions(revision),
        )
        .unwrap()
}

#[test]
fn prepare_finish_and_mutation_failures_preserve_state() {
    let mut state = SchedulerState {
        tasks: vec![task("one-shot", false, true), task("second", false, true)],
        ..Default::default()
    };
    let occurrence = prepare(&mut state, "one-shot", 7);
    assert_eq!(occurrence.task.id, "one-shot");
    state
        .finish_one_shot_removal(&occurrence.occurrence_id)
        .unwrap();

    prepare(&mut state, "second", 1);
    state.tasks.push(task("duplicate", false, true));
    assert_eq!(
        state
            .prepare_one_shot_occurrence("duplicate", versions(1))
            .unwrap_err(),
        OccurrenceJournalError::DuplicateTransitionVersion
    );

    for invalid in [
        task("recurring", true, true),
        task("ephemeral", false, false),
    ] {
        let mut state = SchedulerState {
            tasks: vec![invalid.clone()],
            ..Default::default()
        };
        assert!(matches!(
            state.prepare_one_shot_occurrence(&invalid.id, versions(3)),
            Err(OccurrenceJournalError::NotDurableOneShot(_))
        ));
    }
}

#[test]
fn validation_rejects_impossible_versions_and_non_rfc_identity() {
    for (fire_generation, removal_generation, fire, removal) in [
        (GENERATION, GENERATION, 0, 1),
        (GENERATION, GENERATION, 1, 3),
        (GENERATION, "01890f42-7d5c-7c00-8000-000000000002", 1, 2),
        ("01890f42-7d5c-7c00-c000-000000000001", GENERATION, 1, 2),
    ] {
        assert_eq!(
            ScheduledOccurrenceVersions::try_new(
                version(fire_generation, fire),
                version(removal_generation, removal),
            ),
            Err(OccurrenceJournalError::InvalidVersions)
        );
    }

    let invalid = occurrence_json(
        "01890f42-7d5c-7c00-c000-000000000001",
        serde_json::to_value(task("bad-id", false, true)).unwrap(),
        serde_json::json!({
            "fire": { "generation": GENERATION, "revision": 1 },
            "removal": { "generation": GENERATION, "revision": 2 },
        }),
    );
    let state = state(Vec::new(), serde_json::Value::Array(vec![invalid]));
    let plan = state.reconcile_one_shot_occurrences();
    assert!(plan.recovery_required() && plan.blocked_task_ids().contains("bad-id"));
}

#[test]
fn exactly_fifty_round_trips_and_mutation_reports_journal_full() {
    let entries: Vec<_> = (0..MAX_PENDING_ONE_SHOTS)
        .map(|index| {
            valid_occurrence_json(
                100 + index as u64,
                &format!("task-{index}"),
                index as u64 * 2 + 1,
            )
        })
        .collect();
    let mut state = state(Vec::new(), serde_json::Value::Array(entries));
    assert_eq!(
        state.occurrence_journal.entries.len(),
        MAX_PENDING_ONE_SHOTS
    );
    let encoded = serde_json::to_value(&state).unwrap();
    let reloaded: SchedulerState = serde_json::from_value(encoded).unwrap();
    assert_eq!(
        reloaded.occurrence_journal.entries.len(),
        MAX_PENDING_ONE_SHOTS
    );

    state.tasks.push(task("new", false, true));
    assert_eq!(
        state
            .prepare_one_shot_occurrence("new", versions(3))
            .unwrap_err(),
        OccurrenceJournalError::JournalFull
    );
}

#[test]
fn overflow_tail_suppresses_globally_and_never_serializes_a_fifty_first_entry() {
    let mut entries: Vec<_> = (0..MAX_PENDING_ONE_SHOTS)
        .map(|index| valid_occurrence_json(200 + index as u64, &format!("task-{index}"), 1))
        .collect();
    entries.push(valid_occurrence_json(999, "tail-task", 3));
    let state = state(
        vec![task("tail-task", false, true), task("other", false, true)],
        serde_json::Value::Array(entries),
    );

    let plan = state.reconcile_one_shot_occurrences();
    assert!(plan.block_all_one_shots() && plan.recovery_required());
    assert!(!plan.requires_resources_persistence());
    assert!(plan.blocked_task_ids().contains("tail-task"));
    assert!(plan.overflow_error().is_some());

    let encoded = serde_json::to_value(&state).unwrap();
    assert_eq!(
        encoded["occurrenceJournal"]["entries"]
            .as_array()
            .unwrap()
            .len(),
        MAX_PENDING_ONE_SHOTS
    );
    let mut reloaded: SchedulerState = serde_json::from_value(encoded).unwrap();
    let reloaded_plan = reloaded.reconcile_one_shot_occurrences();
    assert!(reloaded_plan.block_all_one_shots() && reloaded_plan.recovery_required());
    reloaded.tasks.push(task("new", false, true));
    let before = reloaded.tasks.len();
    assert_eq!(
        reloaded
            .prepare_one_shot_occurrence("new", versions(5))
            .unwrap_err(),
        OccurrenceJournalError::RecoveryRequired
    );
    assert_eq!(reloaded.tasks.len(), before);
}

#[test]
fn malformed_missing_task_identity_blocks_all_one_shots_across_reload() {
    let malformed = occurrence_json(
        &uuid(20).to_string(),
        serde_json::json!({ "prompt": "missing id" }),
        serde_json::json!({
            "fire": { "generation": GENERATION, "revision": 1 },
            "removal": { "generation": GENERATION, "revision": 2 },
        }),
    );
    let state = state(
        vec![task("due", false, true), task("recurring", true, true)],
        serde_json::Value::Array(vec![malformed]),
    );
    let plan = state.reconcile_one_shot_occurrences();
    assert!(plan.block_all_one_shots() && plan.recovery_required());
    assert!(plan.blocked_task_ids().contains("due"));

    let encoded = serde_json::to_value(&state).unwrap();
    let reloaded: SchedulerState = serde_json::from_value(encoded).unwrap();
    let reloaded_plan = reloaded.reconcile_one_shot_occurrences();
    assert!(reloaded_plan.block_all_one_shots() && reloaded_plan.recovery_required());
}

#[test]
fn inconsistent_current_overflow_metadata_normalizes_and_round_trips() {
    let current = serde_json::json!({
        "entries": [],
        "overflowed": true,
        "blockAllOneShots": false,
    });
    let state = state(vec![task("due", false, true)], current);
    let plan = state.reconcile_one_shot_occurrences();
    assert!(plan.block_all_one_shots() && plan.recovery_required());

    let encoded = serde_json::to_value(&state).unwrap();
    assert!(encoded["occurrenceJournal"]["blockAllOneShots"] == true);
    let reloaded: SchedulerState = serde_json::from_value(encoded).unwrap();
    assert!(
        reloaded
            .reconcile_one_shot_occurrences()
            .recovery_required()
    );
}

#[tokio::test]
async fn production_loader_preserves_tasks_and_quarantine_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("resources_state.json");
    let invalid = occurrence_json(
        &uuid(30).to_string(),
        serde_json::to_value(task("bad", true, true)).unwrap(),
        serde_json::json!({
            "fire": { "generation": GENERATION, "revision": 1 },
            "removal": { "generation": GENERATION, "revision": 2 },
        }),
    );
    std::fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({
            "state": { "echo_agent_build.Scheduler": {
                "tasks": [task("recurring", true, true)],
                "occurrenceJournal": [invalid]
            } }
        }))
        .unwrap(),
    )
    .unwrap();

    let mut resources = Resources::new();
    resources.register_state::<SchedulerState>();
    assert!(ResourcesPersistence::new(path.clone()).load(&mut resources));
    let state = resources.get::<State<SchedulerState>>().unwrap();
    assert_eq!(state.tasks[0].id, "recurring");
    let (task_ids, is_global_block, is_overflowed) =
        state.occurrence_journal.quarantine_diagnostics();
    assert_eq!(task_ids, ["bad"]);
    assert!(!is_global_block && !is_overflowed);

    for journal in [
        serde_json::json!({ "entries": "bad", "blockAllOneShots": [] }),
        serde_json::json!({ "quarantinedTaskIds": ["kept-id", 7] }),
        serde_json::json!("wrong-shape"),
    ] {
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "state": { "echo_agent_build.Scheduler": {
                    "tasks": [task("kept", true, true)],
                    "occurrenceJournal": journal
                } }
            }))
            .unwrap(),
        )
        .unwrap();
        let mut resources = Resources::new();
        resources.register_state::<SchedulerState>();
        assert!(ResourcesPersistence::new(path.clone()).load(&mut resources));
        let state = resources.get::<State<SchedulerState>>().unwrap();
        assert_eq!(state.tasks[0].id, "kept");
        assert!(state.occurrence_journal.block_all_one_shots);
    }
}

#[test]
fn reconciliation_exposes_only_persistence_and_suppression_foundation() {
    let state = state(
        vec![task("resurrected", false, true)],
        serde_json::Value::Array(vec![valid_occurrence_json(10, "resurrected", 1)]),
    );
    let plan = state.reconcile_one_shot_occurrences();
    assert!(plan.requires_resources_persistence());
    assert_eq!(plan.task_ids_to_remove(), ["resurrected"]);
    assert_eq!(state.tasks[0].id, "resurrected");
}

#[test]
fn conflict_receipts_produce_diagnostics_and_suppress_every_task() {
    for (entries, expected) in [
        (
            vec![
                valid_occurrence_json(10, "first", 1),
                valid_occurrence_json(10, "second", 3),
            ],
            OneShotJournalConflict::OccurrenceId,
        ),
        (
            vec![
                valid_occurrence_json(10, "same", 1),
                valid_occurrence_json(11, "same", 3),
            ],
            OneShotJournalConflict::TaskId,
        ),
        (
            vec![
                valid_occurrence_json(10, "first", 1),
                valid_occurrence_json(11, "second", 1),
            ],
            OneShotJournalConflict::TransitionVersion,
        ),
    ] {
        let ids: Vec<_> = entries
            .iter()
            .map(|entry| entry["task"]["id"].as_str().unwrap().to_owned())
            .collect();
        let mut state = state(
            ids.iter().map(|id| task(id, false, true)).collect(),
            serde_json::Value::Array(entries),
        );
        let plan = state.reconcile_one_shot_occurrences();
        assert!(plan.recovery_required());
        assert!(plan.task_ids_to_remove().is_empty());
        assert_eq!(plan.conflicts(), &[expected, expected]);
        assert!(ids.iter().all(|id| plan.blocked_task_ids().contains(id)));
        assert_eq!(state.tasks.len(), ids.len());
        let unrelated = "unrelated";
        state.tasks.push(task(unrelated, false, true));
        let before = state.tasks.len();
        assert_eq!(
            state
                .prepare_one_shot_occurrence(unrelated, versions(9))
                .unwrap_err(),
            OccurrenceJournalError::RecoveryRequired
        );
        assert_eq!(state.tasks.len(), before);
    }
}

#[test]
fn empty_journal_omits_legacy_field() {
    let serialized = serde_json::to_value(SchedulerState {
        tasks: vec![task("legacy", true, true)],
        ..Default::default()
    })
    .unwrap();
    assert!(serialized.get("occurrenceJournal").is_none());
}
