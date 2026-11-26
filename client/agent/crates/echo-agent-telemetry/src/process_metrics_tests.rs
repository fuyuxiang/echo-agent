//! Non-unix pin; the unix behavior lives in the `process_snapshot` binary.

#[cfg(not(unix))]
#[test]
fn non_unix_snapshots_report_no_cpu_readings() {
    let first = super::snapshot();
    let second = super::snapshot();
    assert_eq!(first.cpu_time_ms, None);
    assert_eq!(second.cpu_time_ms, None, "no getrusage means no counter");
    assert_eq!(second.cpu, None);
}
