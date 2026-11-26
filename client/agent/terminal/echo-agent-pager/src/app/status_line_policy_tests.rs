use super::*;

/// One labelled single-field change to the base inputs.
type Tweak = (&'static str, fn(&mut TickInputs));

fn quiet() -> TickInputs {
    TickInputs {
        row_is_drawn: true,
        settled: true,
        source_changed: false,
        client_fields_changed: false,
        turn_timer_running: false,
        has_agent: true,
        forced: false,
        refresh_due: false,
        run: RunSlot::Free,
    }
}

/// Every reason to repaint, so a suppression below is visible.
fn busy() -> TickInputs {
    TickInputs {
        settled: false,
        source_changed: true,
        client_fields_changed: true,
        turn_timer_running: true,
        forced: true,
        refresh_due: true,
        ..quiet()
    }
}

#[test]
fn quiet_settled_row_lets_the_loop_park() {
    assert_eq!(status_line_tick_demand(quiet()), TickDemand::None);
}

#[test]
fn each_reason_to_repaint_asks_for_ticks() {
    let reasons: [Tweak; 6] = [
        ("a turn is running", |i| i.turn_timer_running = true),
        ("the agent changed", |i| i.source_changed = true),
        ("the session was renamed", |i| {
            i.client_fields_changed = true
        }),
        ("nothing has painted yet", |i| i.settled = false),
        // A force deferred by the floor still needs the tick that runs it.
        ("a force is owed", |i| i.forced = true),
        // Raised behind a busy slot or a hidden row; the tick carries it.
        ("a refresh is owed", |i| i.refresh_due = true),
    ];
    for (why, raise) in reasons {
        let mut inputs = quiet();
        raise(&mut inputs);
        assert_eq!(status_line_tick_demand(inputs), TickDemand::Slow, "{why}");
    }
    let lost_run = TickInputs {
        run: RunSlot::PastDeadline,
        ..quiet()
    };
    assert_eq!(status_line_tick_demand(lost_run), TickDemand::Slow);
}

#[test]
fn suppressor_beats_every_reason_to_repaint() {
    let suppressors: [Tweak; 3] = [
        ("no agent attached", |i| i.has_agent = false),
        // Minimal mode and a row that is off both reach here as one answer.
        ("no row is drawn", |i| i.row_is_drawn = false),
        // A run answers through its own task result, not through a tick.
        ("a run is outstanding", |i| i.run = RunSlot::WithinDeadline),
    ];
    for (why, suppress) in suppressors {
        let mut inputs = busy();
        suppress(&mut inputs);
        assert_eq!(status_line_tick_demand(inputs), TickDemand::None, "{why}");
    }
}

#[test]
fn each_disposition_earns_the_log_line_the_guide_promises() {
    use crate::app::status_line::{FinishDisposition, REFRESH_FAILURES_TO_PAINT};

    let painted = |failures| FinishDisposition::RefreshFailurePainted {
        error: "exit 7".into(),
        failures,
    };
    let line = |disposition| {
        refresh_failure_log(disposition).map(|line| (line.level, line.message, line.failures))
    };

    assert!(refresh_failure_log(FinishDisposition::Applied).is_none());
    assert_eq!(
        line(FinishDisposition::RefreshFailureKept {
            error: "exit 7".into(),
            failures: 1,
        }),
        Some((
            RefreshFailureLogLevel::Debug,
            "status_line: refresh run failed; keeping the last output",
            1,
        )),
        "a kept failure changed nothing the user can see"
    );
    assert_eq!(
        line(painted(1)),
        Some((
            RefreshFailureLogLevel::Warn,
            "status_line: refresh run failed; painting the error",
            1,
        )),
        "painting an unanswered row's first failure is user visible and warns"
    );
    assert_eq!(
        line(painted(REFRESH_FAILURES_TO_PAINT)).map(|(level, ..)| level),
        Some(RefreshFailureLogLevel::Warn),
        "so is the strike that crossed the threshold"
    );
    assert_eq!(
        line(painted(REFRESH_FAILURES_TO_PAINT + 1)).map(|(level, ..)| level),
        Some(RefreshFailureLogLevel::Debug),
        "a script broken all night must not write a warn line per interval"
    );
    assert_eq!(
        refresh_failure_log(painted(1)).map(|line| line.error),
        Some("exit 7".to_string()),
        "the raw error rides the line into the log context"
    );
}
