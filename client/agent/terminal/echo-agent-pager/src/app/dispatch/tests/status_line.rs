//! Status row dispatch: what it draws, when it recomputes, and which results
//! it still accepts.

use super::*;

use std::time::{Duration, Instant};

use crate::app::app_view::TickDemand;
use crate::app::status_line::{
    ABANDON_AFTER, EVENT_DEBOUNCE, MIN_REFRESH_INTERVAL_MS, RunId, RunOutcome, test_context,
};
use crate::views::status_line::{StatusLineDisplay, StatusLineFrame, StatusSegment};
use echo_agent_status_line::test_support::StatusLineConfigFixture;
use echo_agent_status_line::{StatusLineConfig, StatusLineItem, StatusLineType};

/// A `command` is set whatever the mode, so a test can switch mode without
/// losing the script.
fn command_row(kind: StatusLineType) -> StatusLineConfigFixture {
    StatusLineConfigFixture::from_kind(kind).with_command("true")
}

fn status_line_app(kind: StatusLineType) -> AppView {
    let mut app = test_app_with_agent();
    app.current_ui.status_line = command_row(kind).into_config();
    app.agents.get_mut(&AgentId(0)).unwrap().status_context = Some(test_context("/tmp"));
    app
}

fn refresh_timer_app() -> AppView {
    let mut app = status_line_app(StatusLineType::Command);
    app.current_ui.status_line = command_row(StatusLineType::Command)
        .with_refresh_interval(Some(300))
        .into_config();
    app
}

fn settled_refresh_timer_app(now: Instant) -> AppView {
    let mut app = refresh_timer_app();
    app.update_status_line_at(now);
    assert!(queued_a_run(&app), "the first run starts as ever");
    app.pending_effects.clear();
    app.on_status_line_command_finished_at(now, RunId(0), RunOutcome::Output("row".to_string()));
    app
}

/// A row that painted one result and has a second run outstanding. Moves the
/// caller's clock to the instant that second run began, so a deadline the
/// caller measures runs from the run it is about.
fn app_with_a_second_run(now: &mut Instant) -> AppView {
    let mut app = status_line_app(StatusLineType::Command);
    app.update_status_line_at(*now);
    app.on_status_line_command_finished_at(*now, RunId(0), RunOutcome::Output("drawn".to_string()));
    *now += EVENT_DEBOUNCE;
    app.update_status_line_at(*now);
    assert!(
        app.status_line.is_settled() && app.status_line.command_in_flight(*now),
        "the callers below prove nothing without both"
    );
    app
}

fn queued_a_run(app: &AppView) -> bool {
    matches!(
        app.pending_effects.as_slice(),
        [Effect::RunStatusLineCommand(_)]
    )
}

#[test]
fn refresh_timer_reruns_an_idle_settled_row() {
    let mut now = Instant::now();
    let mut app = settled_refresh_timer_app(now);
    assert_eq!(
        app.status_line_tick_demand_at(now),
        TickDemand::None,
        "between timer fires an idle settled row still parks the loop"
    );

    now += Duration::from_secs(300);
    app.note_status_line_refresh_due_at(now);
    assert!(
        queued_a_run(&app),
        "the timer is the one thing that re-runs an idle settled row"
    );
}

#[test]
fn refresh_nudge_without_a_timer_config_runs_nothing() {
    let mut now = Instant::now();
    let mut app = status_line_app(StatusLineType::Command);

    app.update_status_line_at(now);
    app.pending_effects.clear();
    app.on_status_line_command_finished_at(now, RunId(0), RunOutcome::Output("row".to_string()));

    now += Duration::from_secs(300);
    app.note_status_line_refresh_due_at(now);
    assert!(!queued_a_run(&app), "no refresh_interval, no timer runs");
    assert!(
        !app.status_line.refresh_due(),
        "and no due refresh left demanding ticks"
    );
}

#[test]
fn owed_refresh_is_deferred_never_dropped_and_consumed_by_the_first_run() {
    let mut now = Instant::now();
    let mut app = settled_refresh_timer_app(now);
    app.note_status_line_refresh_due_at(now);
    assert!(!queued_a_run(&app), "inside the floor even the timer waits");
    assert!(app.status_line.refresh_due(), "deferred, not dropped");
    now += MIN_REFRESH_INTERVAL_MS;
    app.update_status_line_at(now);
    assert!(
        queued_a_run(&app),
        "past the floor the owed refresh runs without waiting out the debounce"
    );
    assert!(!app.status_line.refresh_due(), "consumed by that run");

    let mut now = Instant::now();
    let mut app = settled_refresh_timer_app(now);
    app.agents.get_mut(&AgentId(0)).unwrap().active_subagent = Some("child".into());
    now += Duration::from_secs(300);
    app.note_status_line_refresh_due_at(now);
    assert!(!queued_a_run(&app), "no run for a frame the subagent owns");
    assert!(
        app.status_line.refresh_due(),
        "the refresh waits for the row"
    );
    app.agents.get_mut(&AgentId(0)).unwrap().active_subagent = None;
    app.update_status_line_at(now);
    assert!(queued_a_run(&app), "the first run after the row returns");
    assert!(!app.status_line.refresh_due(), "consumed, not re-demanded");

    let mut now = Instant::now();
    let mut app = refresh_timer_app();
    app.active_view = ActiveView::AgentDashboard;
    now += Duration::from_secs(300);
    app.note_status_line_refresh_due_at(now);
    assert!(!queued_a_run(&app), "no agent, nothing to describe");
    assert!(app.status_line.refresh_due(), "kept through the invalidate");
    app.active_view = ActiveView::Agent(AgentId(0));
    app.update_status_line_at(now);
    assert!(queued_a_run(&app), "the first run after an agent appears");
    assert!(
        !app.status_line.refresh_due(),
        "consumed by that run, which carries the refresh_interval trigger"
    );
}

#[test]
fn refresh_with_no_shell_context_settles_empty_and_parks_the_loop() {
    let mut now = Instant::now();
    let mut app = refresh_timer_app();
    app.agents.get_mut(&AgentId(0)).unwrap().status_context = None;

    now += Duration::from_secs(300);
    app.note_status_line_refresh_due_at(now);
    assert!(!queued_a_run(&app), "no payload, nothing to run");
    assert!(
        !app.status_line.refresh_due(),
        "settling empty takes the request with it: whatever starved this \
         update of a context starves the run the refresh waits for too"
    );
    assert_eq!(
        app.status_line_tick_demand_at(now),
        TickDemand::None,
        "a refresh nothing can answer must not become a stranded wake"
    );
}

#[test]
fn row_that_cannot_resolve_paints_the_problem_verbatim() {
    let now = Instant::now();
    let mut app = status_line_app(StatusLineType::Command);
    app.current_ui.status_line =
        StatusLineConfigFixture::from_kind(StatusLineType::Command).into_config();

    app.update_status_line_at(now);

    // A warning segment, not dim text: the row is chrome, and this is the one
    // thing in it the user has to notice.
    let problem = StatusLineDisplay::Segments(vec![StatusSegment::warn(
        "[ui.status_line] type = \"command\" needs command = \"…\"",
    )]);
    assert_eq!(app.status_line.display().as_deref(), Some(&problem));
}

#[test]
fn builtin_status_line_waits_for_the_agent_snapshot() {
    let now = Instant::now();
    let mut app = status_line_app(StatusLineType::Builtin);
    app.agents.get_mut(&AgentId(0)).unwrap().status_context = None;

    app.refresh_status_line_now_at(now);
    assert!(app.status_line.display().is_none(), "no snapshot, no row");
    assert_eq!(
        app.status_line_tick_demand_at(now),
        TickDemand::None,
        "a row with nothing to draw must let the loop park"
    );

    app.agents.get_mut(&AgentId(0)).unwrap().status_context = Some(test_context("/tmp/project"));
    app.refresh_status_line_now_at(now);
    assert!(app.status_line.display().is_some(), "the snapshot paints");
}

#[test]
fn renaming_the_session_rebuilds_a_row_that_had_already_settled() {
    let mut now = Instant::now();
    let mut app = status_line_app(StatusLineType::Builtin);
    app.current_ui.status_line = command_row(StatusLineType::Builtin)
        .with_items(vec![StatusLineItem::SessionName])
        .into_config();
    app.agents.get_mut(&AgentId(0)).unwrap().display_name = Some("before".to_string());
    app.refresh_status_line_now_at(now);
    assert_eq!(
        app.status_line_tick_demand_at(now),
        TickDemand::None,
        "a settled row must park the loop, or the assertion below proves nothing"
    );

    app.agents.get_mut(&AgentId(0)).unwrap().display_name = Some("after".to_string());
    assert_eq!(
        app.status_line_tick_demand_at(now),
        TickDemand::Slow,
        "the row asks for the tick that rebuilds it"
    );

    // Throttled like any other rebuild: the new name lands on the tick after
    // the floor passes.
    app.update_status_line_at(now);
    now += MIN_REFRESH_INTERVAL_MS;
    app.update_status_line_at(now);
    let display = app.status_line.display();
    let Some(StatusLineDisplay::Segments(segments)) = display.as_deref() else {
        panic!("the rename left the row without its one segment");
    };
    assert_eq!(
        segments.iter().map(StatusSegment::text).collect::<Vec<_>>(),
        ["after"]
    );
    assert_eq!(
        app.status_line_tick_demand_at(now),
        TickDemand::None,
        "the rebuilt row parks again rather than ticking on the same rename"
    );
}

#[test]
fn row_that_settled_on_nothing_gives_its_line_back() {
    let now = Instant::now();
    let mut app = status_line_app(StatusLineType::Command);

    app.update_status_line_at(now);
    assert_eq!(
        app.status_line_frame().height(),
        1,
        "the line is held until the script answers"
    );

    app.on_status_line_command_finished_at(now, RunId(0), RunOutcome::Output(String::new()));
    assert_eq!(
        app.status_line_frame().height(),
        0,
        "a script that printed nothing must not leave a blank line behind"
    );
}

#[test]
fn run_past_its_deadline_asks_for_the_tick_that_abandons_it() {
    let mut now = Instant::now();
    // A settled row raises no other demand, so the deadline is the only thing
    // left that can ask for the tick the watchdog runs on.
    let mut app = app_with_a_second_run(&mut now);
    assert_eq!(
        app.status_line_tick_demand_at(now),
        TickDemand::None,
        "inside its deadline a run answers through its own result, not a tick"
    );
    now += ABANDON_AFTER;
    assert_eq!(
        app.status_line_tick_demand_at(now),
        TickDemand::Slow,
        "past the deadline the tick is what runs the watchdog"
    );

    app.status_line.abandon_if_past_deadline(now);
    assert!(
        !app.status_line.command_in_flight(now),
        "the deadline releases the run so a later recompute can start one"
    );
}

#[test]
fn result_that_arrives_after_the_deadline_still_paints() {
    let mut now = Instant::now();
    let mut app = status_line_app(StatusLineType::Command);

    app.update_status_line_at(now);
    now += ABANDON_AFTER;
    app.status_line.abandon_if_past_deadline(now);
    assert!(!app.status_line.command_in_flight(now));

    let _ = app.status_line.finish_command_run(
        now,
        RunId(0),
        RunOutcome::Output("late but useful".to_string()),
    );
    assert!(
        app.status_line.display().is_some(),
        "an abandoned run keeps its id, so a late result still paints"
    );
}

#[test]
fn settled_row_recovers_an_abandoned_run_on_its_next_refresh() {
    let mut now = Instant::now();
    let mut app = app_with_a_second_run(&mut now);
    now += ABANDON_AFTER;
    // Recovery through a refresh rather than through the tick the row now asks
    // for, since a keystroke or a resize can arrive first.
    app.pending_effects.clear();
    app.refresh_status_line_now_at(now);
    assert!(
        queued_a_run(&app),
        "the refresh abandons the outstanding run and starts a fresh one"
    );
}

#[test]
fn force_raised_inside_the_floor_is_deferred_rather_than_dropped() {
    let mut now = Instant::now();
    let mut app = status_line_app(StatusLineType::Command);

    app.update_status_line_at(now);
    assert!(queued_a_run(&app), "nothing ran, so nothing below holds");
    app.pending_effects.clear();

    app.refresh_status_line_now_at(now);
    assert!(
        app.pending_effects.is_empty(),
        "the floor holds a force raised this close to the last run"
    );

    app.on_status_line_command_finished_at(now, RunId(0), RunOutcome::Output("stale".to_string()));
    assert!(
        app.status_line.force_pending(),
        "the result started before the force and cannot answer it"
    );
    assert_eq!(
        app.status_line_tick_demand_at(now),
        TickDemand::Slow,
        "a deferred rerun must keep asking for ticks, or the row stays stale"
    );
    now += MIN_REFRESH_INTERVAL_MS;
    app.update_status_line_at(now);
    assert!(queued_a_run(&app), "the force never reran past the floor");
}

#[test]
fn second_run_cannot_start_while_one_is_outstanding() {
    let mut now = Instant::now();
    let mut app = status_line_app(StatusLineType::Command);

    app.update_status_line_at(now);
    assert!(queued_a_run(&app), "nothing ran, so the slot stays free");
    app.pending_effects.clear();
    now += EVENT_DEBOUNCE;
    assert!(
        app.status_line.is_due(now),
        "the debounce is what would refuse, so this proves nothing about the slot"
    );
    app.update_status_line_at(now);
    assert!(
        app.pending_effects.is_empty(),
        "a script slower than the debounce ran twice over"
    );

    let _ = app.status_line.finish_command_run(
        now,
        RunId(0),
        RunOutcome::Output("the first run".to_string()),
    );
    assert!(
        app.status_line.display().is_some(),
        "the outstanding run kept its id, so its result still paints"
    );
}

#[test]
fn gap_between_runs_is_measured_from_the_end_of_the_last_one() {
    let mut now = Instant::now();
    let mut app = status_line_app(StatusLineType::Command);

    app.update_status_line_at(now);
    app.pending_effects.clear();
    now += EVENT_DEBOUNCE * 3;
    app.on_status_line_command_finished_at(now, RunId(0), RunOutcome::Output("row".to_string()));

    app.update_status_line_at(now);
    assert!(
        app.pending_effects.is_empty(),
        "a script slower than the debounce re-ran with no gap at all"
    );
    now += EVENT_DEBOUNCE;
    app.update_status_line_at(now);
    assert!(queued_a_run(&app), "the debounce passed and nothing reran");
}

#[test]
fn minimal_mode_runs_the_row_like_fullscreen() {
    let now = Instant::now();
    let mut app = status_line_app(StatusLineType::Command);
    app.screen_mode = crate::app::ScreenMode::Minimal;
    app.update_status_line_at(now);
    assert!(queued_a_run(&app), "the script runs in minimal mode too");

    let mut app = status_line_app(StatusLineType::Builtin);
    app.current_ui.status_line = command_row(StatusLineType::Builtin)
        .with_items(vec![StatusLineItem::Cwd])
        .into_config();
    app.screen_mode = crate::app::ScreenMode::Minimal;
    app.update_status_line_at(now);
    assert!(
        matches!(app.status_line_frame(), StatusLineFrame::On { .. }),
        "a builtin row fills for the minimal live region to paint"
    );
}

#[test]
fn resize_arms_the_next_run_only_when_there_is_a_row() {
    let mut app = status_line_app(StatusLineType::Command);
    app.queue_status_line_resize();
    assert!(
        app.status_line.force_pending(),
        "the row has to re-run at the new width"
    );

    let mut off = status_line_app(StatusLineType::Disabled);
    off.queue_status_line_resize();
    assert!(!off.status_line.force_pending(), "no row, no resize");
}

#[test]
fn row_belonging_to_another_agent_is_not_painted_under_this_one() {
    let now = Instant::now();
    let mut app = status_line_app(StatusLineType::Builtin);
    app.current_ui.status_line = command_row(StatusLineType::Builtin)
        .with_items(vec![StatusLineItem::Cwd])
        .into_config();
    app.update_status_line_at(now);
    assert!(
        matches!(app.status_line_frame(), StatusLineFrame::On { .. }),
        "the row never filled"
    );

    app.active_view = ActiveView::Agent(AgentId(7));
    assert!(
        matches!(app.status_line_frame(), StatusLineFrame::Reserved { .. }),
        "another agent's row must not paint here, and the line stays held so \
         nothing jumps while this agent's row is built"
    );
}

#[test]
fn cycling_agents_cannot_re_run_a_script_faster_than_the_floor() {
    let mut now = Instant::now();
    let mut app = status_line_app(StatusLineType::Command);
    // A second agent the row can legitimately describe, so the switch reaches
    // the throttle rather than stopping at "no session to report on".
    let second = AgentId(1);
    let session = make_test_agent_session(&app, second, "second-session");
    let mut agent = AgentView::new(session, ScrollbackState::new());
    agent.status_context = Some(test_context("/tmp/second"));
    app.agents.insert(second, agent);

    app.update_status_line_at(now);
    assert!(queued_a_run(&app), "nothing ran, so the switch is moot");
    app.pending_effects.clear();
    app.on_status_line_command_finished_at(now, RunId(0), RunOutcome::Output("row".to_string()));

    app.active_view = ActiveView::Agent(second);
    app.update_status_line_at(now);
    assert!(app.pending_effects.is_empty(), "re-ran inside the floor");
    assert!(app.status_line.force_pending(), "the switch was dropped");
    now += MIN_REFRESH_INTERVAL_MS;
    app.update_status_line_at(now);
    assert!(queued_a_run(&app), "the deferred run never happened");
}

#[test]
fn row_nobody_asked_for_arms_nothing_that_outlives_the_turn() {
    let now = Instant::now();
    let mut app = test_app_with_agent();
    app.current_ui.status_line = StatusLineConfig::default();

    app.refresh_status_line_for(AgentId(0));

    assert!(
        !app.status_line.force_pending(),
        "a disabled row that latches a force wakes the event loop for good"
    );
    assert_eq!(app.status_line_tick_demand_at(now), TickDemand::None);
}

#[test]
fn only_a_row_that_can_change_mid_turn_holds_the_loop_awake_for_one() {
    let now = Instant::now();
    for (kind, demand) in [
        (StatusLineType::Command, TickDemand::Slow),
        (StatusLineType::Builtin, TickDemand::None),
    ] {
        let mut app = status_line_app(kind);
        app.current_ui.status_line = command_row(kind)
            .with_items(vec![StatusLineItem::Cwd])
            .into_config();
        app.update_status_line_at(now);
        // Settle the run a `command` row just started, so the only thing left
        // that could ask for a tick is the turn.
        app.on_status_line_command_finished_at(
            now,
            RunId(0),
            RunOutcome::Output("row".to_string()),
        );
        app.agents.get_mut(&AgentId(0)).unwrap().turn_started_at = Some(std::time::Instant::now());

        assert_eq!(app.status_line_tick_demand_at(now), demand, "{kind:?}");
    }
}
