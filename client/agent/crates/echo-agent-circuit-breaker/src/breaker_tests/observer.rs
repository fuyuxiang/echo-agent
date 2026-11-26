//! Observer-invocation invariants: fires-once-per-transition,
//! post-transition state visible to the observer, and `is_open()`
//! Release-ordering after `record()`.

use std::sync::Arc;
use std::sync::Barrier;
use std::sync::Mutex as StdMutex;
use std::thread;
use std::time::{Duration, Instant};

use super::super::*;
use crate::clock::MockClock;

/// Uses a `Mutex<Vec<_>>` recording observer to assert exactly one
/// warn on open and one info on close, rather than an in-breaker
/// `warn_count` counter.
#[test]
fn observer_emits_exactly_one_open_and_one_close_transition() {
    #[derive(Default)]
    struct RecordingObserver {
        transitions: StdMutex<Vec<(BreakerState, BreakerState)>>,
    }
    impl Observer for RecordingObserver {
        fn on_state_change(&self, old: BreakerState, new: BreakerState, _reason: &str) {
            self.transitions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((old, new));
        }
    }

    let obs = Arc::new(RecordingObserver::default());
    let clock = Arc::new(MockClock::new());
    let cb = CircuitBreaker::with_clock(BreakerConfig::client(), clock.clone())
        .with_observer(obs.clone());

    // Cross the threshold many times in the open state -- no
    // additional Closed->Open transitions should be reported.
    for _ in 0..50 {
        cb.record(Outcome::Failure);
    }
    assert_eq!(cb.state(), BreakerState::Open);

    // Close via probe.
    clock.advance(Duration::from_secs(61));
    assert!(cb.check().is_ok());
    cb.record(Outcome::Success);
    assert_eq!(cb.state(), BreakerState::Closed);

    let transitions = obs.transitions.lock().unwrap();
    let to_open = transitions
        .iter()
        .filter(|(_, to)| *to == BreakerState::Open)
        .count();
    let to_closed = transitions
        .iter()
        .filter(|(from, to)| *from == BreakerState::HalfOpen && *to == BreakerState::Closed)
        .count();
    assert_eq!(to_open, 1, "exactly one open transition");
    assert_eq!(to_closed, 1, "exactly one close-via-probe transition");
}

/// Observer's `on_state_change` is called AFTER the inner state
/// has transitioned. We assert this by having the observer call
/// `breaker.state()` and compare against the `new` argument.
#[test]
fn observer_sees_post_transition_state() {
    struct StateProbingObserver {
        cb: StdMutex<Option<CircuitBreaker>>,
        mismatches: StdMutex<Vec<(BreakerState, BreakerState)>>,
    }
    impl Observer for StateProbingObserver {
        fn on_state_change(&self, _old: BreakerState, new: BreakerState, _reason: &str) {
            let cb_guard = self.cb.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(cb) = cb_guard.as_ref() {
                let observed = cb.state();
                if observed != new {
                    self.mismatches
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push((new, observed));
                }
            }
        }
    }
    let obs = Arc::new(StateProbingObserver {
        cb: StdMutex::new(None),
        mismatches: StdMutex::new(Vec::new()),
    });
    let cb = CircuitBreaker::new(BreakerConfig::client()).with_observer(obs.clone());
    *obs.cb.lock().unwrap() = Some(cb.clone());

    for _ in 0..5 {
        cb.record(Outcome::Failure);
    }
    assert_eq!(cb.state(), BreakerState::Open);

    let mismatches = obs.mismatches.lock().unwrap();
    assert!(
        mismatches.is_empty(),
        "observer observed pre-transition states: {mismatches:?}"
    );
}

/// `is_open()` must reflect the post-transition state on a separate
/// reader thread within a bounded spin. This exercises the
/// `is_open_fast` `AtomicBool` mirror's cross-thread Release/Acquire
/// visibility: a `Relaxed` store on the writer side would still allow
/// this test to pass under x86's TSO, but a regression that drops the
/// `state` Release store before the mirror would let the reader
/// observe `is_open() == true` *before* `state() == Open` is visible,
/// which the post-spin invariants assert against.
#[test]
fn is_open_visible_to_reader_thread_after_trip() {
    let cb = Arc::new(CircuitBreaker::new(BreakerConfig::client()));
    let barrier = Arc::new(Barrier::new(2));

    let writer = {
        let cb = cb.clone();
        let barrier = barrier.clone();
        thread::spawn(move || {
            barrier.wait();
            for _ in 0..5 {
                cb.record(Outcome::Failure);
            }
        })
    };

    let reader = {
        let cb = cb.clone();
        let barrier = barrier.clone();
        thread::spawn(move || {
            barrier.wait();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !cb.is_open() {
                assert!(
                    Instant::now() < deadline,
                    "reader thread never observed is_open() == true \
                     within the timeout — possible Release-mirror regression"
                );
                std::hint::spin_loop();
            }
            // Mirror saw the trip; the authoritative `state` Acquire
            // load must also reflect Open (or HalfOpen on a racing
            // open-elapsed CAS, which can't happen here — no clock
            // advance).
            cb.state()
        })
    };

    writer.join().unwrap();
    let observed_state = reader.join().unwrap();
    assert_eq!(
        observed_state,
        BreakerState::Open,
        "reader's state() must agree with the is_open() mirror"
    );
    assert!(cb.is_open());
    assert_eq!(cb.state(), BreakerState::Open);
}
