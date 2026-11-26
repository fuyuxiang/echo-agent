use super::*;

use std::time::Instant;

/// A state holding run id 0, which is where every case below starts.
fn state_with_run(now: Instant) -> StatusLineState {
    let mut state = StatusLineState::default();
    let ctx = Box::new(super::test_context("/tmp"));
    let effect = state.begin_command_run(now, "true".into(), ctx, RowSize::FALLBACK);
    assert!(effect.is_some(), "the run must start");
    state
}

#[test]
fn result_from_an_id_nobody_awaits_cannot_paint() {
    let now = Instant::now();
    let mut state = state_with_run(now);

    let _ = state.finish_command_run(now, RunId(u64::MAX), RunOutcome::Output("stale".into()));
    assert!(state.display().is_none(), "a foreign id cannot paint");
    assert!(state.command_in_flight(now), "the real run keeps the slot");

    let _ = state.finish_command_run(now, RunId(0), RunOutcome::Output("row".into()));
    assert!(state.display().is_some());
}

#[test]
fn superseded_run_drops_its_output_and_leaves_the_slot_idle() {
    let now = Instant::now();
    let mut state = state_with_run(now);
    state.supersede_command_run(AfterSupersede::NoRun);

    let _ = state.finish_command_run(now, RunId(0), RunOutcome::Output("stale".into()));
    assert!(!state.command_in_flight(now), "the slot is idle again");
    assert!(state.display().is_none(), "its output never paints");
}

#[test]
fn abandoned_run_is_counted_once_and_cannot_be_counted_again() {
    let mut now = Instant::now();
    let mut state = state_with_run(now);
    now += ABANDON_AFTER;
    state.abandon_if_past_deadline(now);
    assert!(matches!(state.run, RunState::Abandoned(_)));

    state.supersede_command_run(AfterSupersede::NoRun);
    assert!(
        matches!(state.run, RunState::Idle),
        "an abandoned run must not return to a state the watchdog counts again"
    );

    state.abandon_if_past_deadline(now);
    assert!(matches!(state.run, RunState::Idle), "and it stays there");

    let _ = state.finish_command_run(now, RunId(0), RunOutcome::Output("stale output".into()));
    assert!(state.display().is_none(), "nor can its late result paint");
}

#[test]
fn empty_row_holds_no_display_and_still_settles() {
    let mut state = StatusLineState::default();
    state.set_segments(Vec::new());
    assert!(state.display().is_none());
    assert!(state.is_settled(), "empty is an answer, not a wait");
}

#[test]
fn superseding_settles_what_happens_next_even_when_there_was_no_run() {
    let now = Instant::now();
    let mut state = state_with_run(now);
    state.supersede_command_run(AfterSupersede::Rerun);
    assert!(
        state.force_pending(),
        "a superseder that wants fresh output must leave the force standing"
    );

    // Nothing left to supersede, and the force from above is still up.
    state.supersede_command_run(AfterSupersede::NoRun);
    assert!(
        !state.force_pending(),
        "a superseder that wants no run must clear the force it found, or the \
         row asks for ticks nothing will answer"
    );
}

#[test]
fn superseded_run_the_watchdog_abandons_can_never_paint_again() {
    let mut now = Instant::now();
    let mut state = state_with_run(now);
    state.supersede_command_run(AfterSupersede::NoRun);
    assert!(matches!(state.run, RunState::Superseded(_)));
    now += ABANDON_AFTER;
    state.abandon_if_past_deadline(now);
    assert!(
        matches!(state.run, RunState::Idle),
        "a superseded run must not come back as Abandoned, whose id can still paint"
    );

    let _ = state.finish_command_run(
        now,
        RunId(0),
        RunOutcome::Output("stale output".to_string()),
    );
    assert!(state.display().is_none());
}

/// The trigger the started run stamped into its payload.
fn started_trigger(effect: Option<Effect>) -> StatusLineTrigger {
    match effect {
        Some(Effect::RunStatusLineCommand(run)) => run.ctx.trigger.expect("every run is stamped"),
        _ => panic!("no run started"),
    }
}

fn begin(state: &mut StatusLineState, now: Instant) -> Option<Effect> {
    state.begin_command_run(
        now,
        "true".into(),
        Box::new(super::test_context("/tmp")),
        RowSize::FALLBACK,
    )
}

fn failed() -> RunOutcome {
    RunOutcome::Failed {
        text: "[status line: exit 7]".into(),
        error: "exit 7".into(),
    }
}

#[test]
fn refresh_request_marks_one_run_and_waits_out_a_busy_slot() {
    let now = Instant::now();
    let mut state = StatusLineState::default();

    state.request_refresh();
    assert_eq!(
        started_trigger(begin(&mut state, now)),
        StatusLineTrigger::RefreshInterval
    );
    assert!(!state.refresh_due(), "consumed by the run that started");

    state.request_refresh();
    assert!(begin(&mut state, now).is_none(), "the slot is taken");
    assert!(
        state.refresh_due(),
        "a run that never started must not consume the request"
    );

    let _ = state.finish_command_run(now, RunId(0), RunOutcome::Output("row".into()));
    assert_eq!(
        started_trigger(begin(&mut state, now)),
        StatusLineTrigger::RefreshInterval
    );
    let _ = state.finish_command_run(now, RunId(1), RunOutcome::Output("row".into()));
    assert_eq!(
        started_trigger(begin(&mut state, now)),
        StatusLineTrigger::State,
        "a request marks one run, not every run after it"
    );
}

#[test]
fn refresh_failures_keep_the_last_output_until_the_script_is_plainly_broken() {
    let now = Instant::now();
    let mut state = StatusLineState::default();
    let _ = begin(&mut state, now);
    let _ = state.finish_command_run(now, RunId(0), RunOutcome::Output("healthy".into()));
    let healthy = state.display();
    assert!(healthy.is_some());

    for failures in 1..REFRESH_FAILURES_TO_PAINT {
        state.request_refresh();
        let _ = begin(&mut state, now);
        let disposition = state.finish_command_run(now, RunId(failures as u64), failed());
        assert_eq!(
            disposition,
            FinishDisposition::RefreshFailureKept {
                error: "exit 7".into(),
                failures,
            }
        );
        assert_eq!(
            state.display(),
            healthy,
            "a flaky endpoint must not paint an error over the last answer"
        );
        assert!(state.is_settled(), "a kept failure is still an answer");
    }

    state.request_refresh();
    let _ = begin(&mut state, now);
    let disposition =
        state.finish_command_run(now, RunId(REFRESH_FAILURES_TO_PAINT as u64), failed());
    assert_eq!(
        disposition,
        FinishDisposition::RefreshFailurePainted {
            error: "exit 7".into(),
            failures: REFRESH_FAILURES_TO_PAINT,
        }
    );
    assert_ne!(
        state.display(),
        healthy,
        "enough failures in a row and the row stops vouching for stale data"
    );
}

#[test]
fn one_success_resets_the_refresh_failure_count() {
    let now = Instant::now();
    let mut state = StatusLineState::default();
    for id in 0..2 {
        state.request_refresh();
        let _ = begin(&mut state, now);
        let _ = state.finish_command_run(now, RunId(id), failed());
    }

    let _ = begin(&mut state, now);
    let _ = state.finish_command_run(now, RunId(2), RunOutcome::Output("healthy".into()));

    state.request_refresh();
    let _ = begin(&mut state, now);
    assert_eq!(
        state.finish_command_run(now, RunId(3), failed()),
        FinishDisposition::RefreshFailureKept {
            error: "exit 7".into(),
            failures: 1,
        },
        "the count is consecutive failures, not failures ever"
    );
}

#[test]
fn refresh_failure_strikes_survive_an_invalidated_row() {
    let now = Instant::now();
    let mut state = StatusLineState::default();
    for id in 0..2 {
        state.request_refresh();
        let _ = begin(&mut state, now);
        let _ = state.finish_command_run(now, RunId(id), failed());
    }

    // An agent switch clears the row, not the script's record: it is the
    // same fixed script, and the switch does not absolve it.
    state.invalidate();

    state.request_refresh();
    let _ = begin(&mut state, now);
    assert_eq!(
        state.finish_command_run(now, RunId(2), failed()),
        FinishDisposition::RefreshFailurePainted {
            error: "exit 7".into(),
            failures: REFRESH_FAILURES_TO_PAINT,
        },
        "the third consecutive failure still counts across an invalidate"
    );
}

#[test]
fn refresh_failure_below_threshold_cannot_pop_a_row_an_empty_print_hid() {
    let now = Instant::now();
    let mut state = StatusLineState::default();
    let _ = begin(&mut state, now);
    let _ = state.finish_command_run(now, RunId(0), RunOutcome::Output(String::new()));
    assert!(state.display().is_none(), "the script hid the row");

    state.request_refresh();
    let _ = begin(&mut state, now);
    assert_eq!(
        state.finish_command_run(now, RunId(1), failed()),
        FinishDisposition::RefreshFailureKept {
            error: "exit 7".into(),
            failures: 1,
        },
        "an empty print was an answer: one failure must not paint an error \
         over a row the script hid on purpose"
    );
    assert!(state.display().is_none());
}

#[test]
fn superseded_refresh_failure_neither_paints_nor_counts_a_strike() {
    let now = Instant::now();
    let mut state = StatusLineState::default();
    state.request_refresh();
    let _ = begin(&mut state, now);
    state.supersede_command_run(AfterSupersede::NoRun);

    assert_eq!(
        state.finish_command_run(now, RunId(0), failed()),
        FinishDisposition::Applied,
        "a superseded run has no refresh story to tell"
    );
    assert!(state.display().is_none(), "its failure text never paints");

    // No strike was counted, so the next real failure is the first. It
    state.request_refresh();
    let _ = begin(&mut state, now);
    assert_eq!(
        state.finish_command_run(now, RunId(1), failed()),
        FinishDisposition::RefreshFailurePainted {
            error: "exit 7".into(),
            failures: 1,
        }
    );
}

#[test]
fn superseded_and_abandoned_refresh_runs_re_raise_the_owed_refresh() {
    let now = Instant::now();
    let mut state = StatusLineState::default();
    state.request_refresh();
    let _ = begin(&mut state, now);
    assert!(!state.refresh_due(), "consumed by the run that started");
    state.supersede_command_run(AfterSupersede::Rerun);
    assert!(state.refresh_due(), "the replacement run keeps the fetch");
    let _ = state.finish_command_run(now, RunId(0), RunOutcome::Output("stale".into()));
    assert_eq!(
        started_trigger(begin(&mut state, now)),
        StatusLineTrigger::RefreshInterval
    );

    let mut now = Instant::now();
    let mut state = StatusLineState::default();
    state.request_refresh();
    let _ = begin(&mut state, now);
    now += ABANDON_AFTER;
    state.abandon_if_past_deadline(now);
    assert!(
        state.refresh_due(),
        "a refresh run that never answered must not swallow its cycle"
    );

    let _ = state.finish_command_run(now, RunId(0), RunOutcome::Output("late".into()));
    assert!(!state.refresh_due(), "the late result answered the refresh");
    assert_eq!(
        started_trigger(begin(&mut state, now)),
        StatusLineTrigger::State,
        "the next run is an ordinary one"
    );
}

#[test]
fn state_run_failure_paints_at_once() {
    let now = Instant::now();
    let mut state = state_with_run(now);
    let disposition = state.finish_command_run(now, RunId(0), failed());
    assert_eq!(disposition, FinishDisposition::Applied);
    assert!(
        state.display().is_some(),
        "the user just did something and is owed the truth about their script"
    );
}

#[test]
fn every_field_on_the_wire_is_named_in_the_guide() {
    let guide = include_str!("../../docs/user-guide/25-status-line.md");
    let documented = documented_paths(guide);
    let fixture: serde_json::Value =
        serde_json::from_str(echo_agent_status_line::test_support::WIRE_FIXTURE_JSON)
            .expect("the fixture parses");

    let mut missing = Vec::new();
    let mut walk = |value: &serde_json::Value| {
        fn visit(
            value: &serde_json::Value,
            path: &str,
            documented: &std::collections::HashSet<String>,
            missing: &mut Vec<String>,
        ) {
            let serde_json::Value::Object(fields) = value else {
                return;
            };
            for (name, child) in fields {
                if name == "_comment" {
                    continue;
                }
                let dotted = if path.is_empty() {
                    name.clone()
                } else {
                    format!("{path}.{name}")
                };
                // Only values are checked. A container is not something a
                // script reads; the guide names it through its members.
                if !child.is_object() && !documented.contains(&dotted) {
                    missing.push(dotted.clone());
                }
                visit(child, &dotted, documented, missing);
            }
        }
        visit(value, "", &documented, &mut missing);
    };
    walk(&fixture);
    assert!(
        missing.is_empty(),
        "on the wire and absent from 25-status-line.md: {missing:?}"
    );
}

#[test]
fn every_builtin_item_is_named_in_the_guide() {
    let guide = include_str!("../../docs/user-guide/25-status-line.md");
    let documented: std::collections::HashSet<&str> =
        table_cells(section(guide, "## Set up")).collect();
    let missing: Vec<_> = echo_agent_status_line::StatusLineItem::ALL
        .iter()
        .map(|item| item.as_str())
        .filter(|name| !documented.contains(name))
        .collect();
    assert!(
        missing.is_empty(),
        "items absent from the guide: {missing:?}"
    );
}

/// The numbers the guide states literally, so none of them can move without
/// editing 25-status-line.md in the same change.
#[test]
fn the_numbers_the_guide_states_are_pinned() {
    assert_eq!(
        REFRESH_FAILURES_TO_PAINT, 3,
        "the guide says three consecutive refresh failures paint the error"
    );
    assert_eq!(
        EVENT_DEBOUNCE,
        Duration::from_millis(300),
        "the guide says updates are debounced at a fixed 300 ms"
    );
    assert_eq!(
        MIN_REFRESH_INTERVAL_MS,
        Duration::from_millis(100),
        "the guide says a change that must show at once waits only 100 ms"
    );
    assert_eq!(
        (
            StatusLineConfig::MIN_REFRESH_INTERVAL_SECS,
            StatusLineConfig::MAX_REFRESH_INTERVAL_SECS,
        ),
        (1, 86_400),
        "the guide says refresh_interval runs from 1 to 86,400 seconds"
    );
}

/// Every key the section serializes must be a backticked cell in the guide's
/// setup tables, so renaming one in the contract crate cannot strand the
/// guide on the old spelling.
#[test]
fn every_config_key_the_section_serializes_is_named_in_the_guide() {
    let guide = include_str!("../../docs/user-guide/25-status-line.md");
    let cells: std::collections::HashSet<&str> = table_cells(section(guide, "## Set up")).collect();
    // Every field set, so a key the fixture leaves unset cannot slip out of
    // the serialized form and past this test.
    let written = serde_json::to_value(
        echo_agent_status_line::test_support::StatusLineConfigFixture::from_kind(
            echo_agent_status_line::StatusLineType::Command,
        )
        .with_command("x")
        .with_items(vec![echo_agent_status_line::StatusLineItem::Cwd])
        .with_padding(1)
        .with_refresh_interval(Some(300))
        .into_config(),
    )
    .expect("the section serializes");
    let missing: Vec<&String> = written
        .as_object()
        .expect("the section is an object")
        .keys()
        .filter(|key| !cells.contains(key.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "config keys absent from the guide's options table: {missing:?}"
    );
}

/// Every field path the guide spells in backticks, with `parent.{a,b}` groups
/// expanded and a leading-dot continuation resolved against the token before
/// it. Only backticked text counts, so prose cannot document a field by
/// accident.
fn documented_paths(guide: &str) -> std::collections::HashSet<String> {
    let mut paths = std::collections::HashSet::new();
    let mut parent = String::new();
    for token in table_cells(section(guide, "## Available data")) {
        // A `head.{a,b}` group is expanded before the comma split, which would
        // otherwise tear its members away from the head that names them.
        let expanded = match token.split_once(".{") {
            Some((head, rest)) => {
                let (members, tail) = rest.split_once('}').unwrap_or((rest, ""));
                let members = members
                    .split(',')
                    .map(|member| format!("{head}.{}", member.trim()))
                    .collect::<Vec<_>>()
                    .join(",");
                format!("{members}{tail}")
            }
            None => token.to_string(),
        };
        for part in expanded.split(',').map(str::trim) {
            // A leading dot continues the row's previous path, which is how the
            // table writes a second field of the same parent.
            let path = match part.strip_prefix('.') {
                Some(rest) => format!("{parent}.{rest}"),
                None => part.to_string(),
            };
            if let Some((head, _)) = path.rsplit_once('.') {
                parent = head.to_string();
            }
            paths.insert(path);
        }
    }
    paths
}

/// The backticked cells of a section's table rows. Prose in the same section
/// does not count: the porting notes name several fields, and a field named
/// only there would vouch for the table row that documents it.
fn table_cells(section: &str) -> impl Iterator<Item = &str> {
    section
        .lines()
        .filter(|line| line.starts_with('|'))
        .flat_map(|row| row.split('`').skip(1).step_by(2))
}

/// One `##` section of the guide. Scoped, because a name in a neighbouring
/// section would otherwise vouch for a row that was deleted.
fn section<'a>(guide: &'a str, heading: &str) -> &'a str {
    let start = guide.find(heading).expect("the guide has this section");
    let rest = &guide[start + heading.len()..];
    match rest.find("\n## ") {
        Some(end) => &rest[..end],
        None => rest,
    }
}
