use echo_agent_status_line::test_support::StatusLineConfigFixture;

use super::*;

fn command_row() -> StatusLineConfig {
    StatusLineConfigFixture::from_kind(StatusLineType::Command)
        .with_command("~/status_line.sh")
        .into_config()
}

#[test]
fn session_reports_its_config_once() {
    let metrics = StatusLineMetrics::new();

    // A section that named no mode is `unset`, not `disabled`.
    metrics.report_config(&StatusLineConfig::default());
    metrics.report_config(&command_row());

    assert_eq!(metrics.kind.get().copied(), Some("unset"));
    // The flag health is gated on cannot move on the second call either.
    assert!(!metrics.draws_a_row.load(Ordering::Relaxed));
}

#[test]
fn health_reports_every_run_and_the_slowest_of_them_once() {
    let metrics = StatusLineMetrics::new();
    metrics.report_config(&command_row());
    metrics.note_content();
    metrics.record_ok(10);
    metrics.record_ok(20);
    // Recorded before the shorter run below, so a last-write-wins bug shows.
    metrics.record_timed_out(10_000);
    metrics.record_failed(30);
    // An abandoned run has no duration to record.
    metrics.record_abandoned();

    let event = metrics.health_event().expect("the row existed");
    assert_eq!(event.kind, "command");
    assert!(event.had_content);
    assert_eq!(event.runs_ok, 2);
    assert_eq!(event.runs_failed, 1);
    assert_eq!(event.runs_timed_out, 1);
    assert_eq!(event.runs_abandoned, 1);
    assert_eq!(event.slowest_ms, 10_000);
    assert!(metrics.health_event().is_none());
}

#[test]
fn only_a_builtin_row_reports_the_items_it_drew() {
    let builtin = StatusLineConfigFixture::from_kind(StatusLineType::Builtin)
        .with_items(vec![StatusLineItem::Cwd, StatusLineItem::Cost])
        .into_config();
    assert_eq!(items_label(&builtin), "cwd,cost");
    assert_eq!(items_label(&command_row()), "");
}

#[test]
fn row_the_client_cannot_draw_reports_adoption_but_not_health() {
    let metrics = StatusLineMetrics::new();
    metrics.report_config(&StatusLineConfig::default());
    metrics.note_content();

    assert_eq!(metrics.kind.get().copied(), Some("unset"));
    assert!(metrics.health_event().is_none());
}
