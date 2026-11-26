use std::collections::VecDeque;
use std::io;
use std::process::ExitStatus;
use std::time::Duration;

use super::{CHILD_EXIT_POLL_QUANTUM, wait_child_bounded_with};

const RUNNING: Option<ExitStatus> = None;

fn exited(code: i32) -> Option<ExitStatus> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        Some(ExitStatus::from_raw(code << 8))
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::ExitStatusExt;
        Some(ExitStatus::from_raw(code as u32))
    }
}

#[test]
fn status_after_waits_stops_all_further_work() {
    let mut outcomes = VecDeque::from([RUNNING, RUNNING, exited(7), exited(9)]);
    let mut elapsed = VecDeque::from([
        Duration::ZERO,
        Duration::from_millis(10),
        Duration::from_millis(10),
        Duration::from_millis(20),
    ]);
    let mut waits = Vec::new();
    let result = wait_child_bounded_with(
        Duration::from_secs(1),
        || Ok(outcomes.pop_front().expect("unexpected poll")),
        || elapsed.pop_front().expect("unexpected elapsed call"),
        |duration| waits.push(duration),
    )
    .expect("wait")
    .expect("status");

    assert_eq!(Some(result), exited(7));
    assert_eq!(waits, [CHILD_EXIT_POLL_QUANTUM, CHILD_EXIT_POLL_QUANTUM]);
    assert_eq!(outcomes.len(), 1, "terminal poll must end the algorithm");
}

#[test]
fn timeout_performs_one_final_poll() {
    let mut polls = 0;
    let mut elapsed = VecDeque::from([Duration::ZERO, Duration::from_secs(1)]);
    let mut waits = Vec::new();
    let result = wait_child_bounded_with(
        Duration::from_millis(10),
        || {
            polls += 1;
            Ok(RUNNING)
        },
        || elapsed.pop_front().expect("unexpected elapsed call"),
        |duration| waits.push(duration),
    )
    .expect("wait");

    assert!(result.is_none());
    assert_eq!(polls, 2);
    assert_eq!(waits, [Duration::from_millis(10)]);
}

#[test]
fn status_at_the_final_deadline_poll_wins() {
    let mut outcomes = VecDeque::from([RUNNING, exited(3)]);
    let mut elapsed = VecDeque::from([Duration::ZERO, Duration::from_secs(1)]);
    let result = wait_child_bounded_with(
        Duration::from_millis(10),
        || Ok(outcomes.pop_front().expect("unexpected poll")),
        || elapsed.pop_front().expect("unexpected elapsed call"),
        |_| {},
    )
    .expect("wait");

    assert_eq!(result, exited(3));
}

#[test]
fn initial_and_later_poll_errors_return_without_cleanup_work() {
    for error_poll in [1, 2] {
        let mut polls = 0;
        let mut waits = 0;
        let mut elapsed = VecDeque::from([Duration::ZERO, Duration::ZERO]);
        let result = wait_child_bounded_with(
            Duration::from_secs(1),
            || {
                polls += 1;
                if polls == error_poll {
                    Err(io::Error::from_raw_os_error(5))
                } else {
                    Ok(RUNNING)
                }
            },
            || elapsed.pop_front().expect("unexpected elapsed call"),
            |_| waits += 1,
        );

        assert_eq!(result.expect_err("poll must fail").raw_os_error(), Some(5));
        assert_eq!(polls, error_poll);
        assert_eq!(waits, usize::from(error_poll > 1));
    }
}

#[test]
fn zero_timeout_has_two_boundary_polls_and_no_wait() {
    let mut polls = 0;
    let mut waits = 0;
    let result = wait_child_bounded_with(
        Duration::ZERO,
        || {
            polls += 1;
            Ok(RUNNING)
        },
        || Duration::ZERO,
        |_| waits += 1,
    )
    .expect("wait");

    assert!(result.is_none());
    assert_eq!((polls, waits), (2, 0));
}

#[test]
fn huge_timeout_is_capped_to_the_poll_quantum() {
    let mut outcomes = VecDeque::from([RUNNING, exited(0)]);
    let mut elapsed = VecDeque::from([Duration::ZERO, Duration::ZERO]);
    let mut waits = Vec::new();
    let result = wait_child_bounded_with(
        Duration::MAX,
        || Ok(outcomes.pop_front().expect("unexpected poll")),
        || elapsed.pop_front().expect("unexpected elapsed call"),
        |duration| waits.push(duration),
    )
    .expect("wait");

    assert!(result.is_some());
    assert_eq!(waits, [CHILD_EXIT_POLL_QUANTUM]);
}
