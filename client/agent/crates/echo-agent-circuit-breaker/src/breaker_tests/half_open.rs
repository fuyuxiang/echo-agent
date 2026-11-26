//! Half-open probe limiting, `half_open_max_probes = 0` clamping,
//! abandoned-probe lease reclaim, and CAS-loss recovery on the
//! Open → HalfOpen transition.

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use super::super::*;
use super::support::{breaker_with_mock, fast_config};

#[test]
fn half_open_limits_concurrent_probes() {
    let (cb, clock) = breaker_with_mock(fast_config(|c| {
        c.min_samples = 1;
        c.open_duration = Duration::from_millis(50);
        c.half_open_max_probes = 2;
    }));

    cb.record(Outcome::Failure);
    clock.advance(Duration::from_millis(70));

    assert!(cb.check().is_ok());
    assert_eq!(cb.state(), BreakerState::HalfOpen);

    assert!(cb.check().is_ok());

    // Third exceeds max_probes
    assert!(cb.check().is_err());
}

#[test]
fn max_probes_clamped_to_at_least_one() {
    let (cb, clock) = breaker_with_mock(BreakerConfig {
        half_open_max_probes: 0,
        min_samples: 1,
        open_duration: Duration::from_millis(50),
        ..Default::default()
    });

    cb.record(Outcome::Failure);
    clock.advance(Duration::from_millis(70));

    // Even with max_probes=0 in config, clamped to 1 so one probe gets through
    assert!(cb.check().is_ok());
    assert_eq!(cb.state(), BreakerState::HalfOpen);
    // Second is rejected
    assert!(cb.check().is_err());
}

#[test]
fn breaker_half_open_serialises_concurrent_probes() {
    let (cb, clock) = breaker_with_mock(BreakerConfig {
        half_open_max_probes: 1,
        ..BreakerConfig::client()
    });
    for _ in 0..5 {
        cb.record(Outcome::Failure);
    }
    assert_eq!(cb.state(), BreakerState::Open);

    clock.advance(Duration::from_secs(61));

    // First check claims the only probe slot.
    assert!(cb.check().is_ok());
    // Subsequent checks must short-circuit until the probe
    // resolves and the breaker transitions.
    for _ in 0..10 {
        assert!(cb.check().is_err());
    }
}

/// A probe whose owner never records (its future was dropped on caller
/// cancellation) must not strand the breaker in `HalfOpen` forever:
/// once the claim is older than `open_duration`, one caller reclaims
/// the slot and recovery proceeds.
#[test]
fn abandoned_probe_slot_reclaimed_after_lease_expiry() {
    let (cb, clock) = breaker_with_mock(fast_config(|c| {
        c.min_samples = 1;
        c.open_duration = Duration::from_millis(50);
        c.half_open_max_probes = 1;
    }));

    cb.record(Outcome::Failure);
    clock.advance(Duration::from_millis(70));

    // Claim the only probe slot, then abandon it: no record() ever fires.
    assert!(cb.check().is_ok());
    assert_eq!(cb.state(), BreakerState::HalfOpen);
    // While the lease is live, the slot stays claimed.
    assert!(cb.check().is_err());

    // Once the lease (open_duration) expires, the claim is treated as
    // abandoned: exactly one caller takes the slot over.
    clock.advance(Duration::from_millis(50));
    assert!(
        cb.check().is_ok(),
        "expired probe lease must be reclaimable"
    );
    assert!(cb.check().is_err(), "only one takeover per expired lease");

    // The takeover probe's outcome drives the state machine as usual.
    cb.record(Outcome::Success);
    assert_eq!(cb.state(), BreakerState::Closed);
}

/// The reclaim path must also handle repeated abandonment: each expired
/// lease admits exactly one replacement probe.
#[test]
fn repeatedly_abandoned_probes_keep_recovery_alive() {
    let (cb, clock) = breaker_with_mock(fast_config(|c| {
        c.min_samples = 1;
        c.open_duration = Duration::from_millis(50);
        c.half_open_max_probes = 1;
    }));

    cb.record(Outcome::Failure);
    clock.advance(Duration::from_millis(70));

    for round in 0..3 {
        assert!(cb.check().is_ok(), "round {round}: probe must be admitted");
        assert!(cb.check().is_err(), "round {round}: second probe rejected");
        // Abandon the probe and let its lease expire.
        clock.advance(Duration::from_millis(50));
    }

    // A probe that finally records still closes the breaker.
    assert!(cb.check().is_ok());
    cb.record(Outcome::Success);
    assert_eq!(cb.state(), BreakerState::Closed);
}

/// Race two threads attempting the Open → HalfOpen CAS. Only one
/// should win the CAS; the loser must observe `HalfOpen` and
/// take the same probe-counting path so the half_open_probes
/// counter is consistent.
#[test]
fn cas_loss_recovery_with_mock_clock() {
    let (cb, clock) = breaker_with_mock(BreakerConfig {
        half_open_max_probes: 1,
        ..fast_config(|c| {
            c.min_samples = 1;
            c.open_duration = Duration::from_millis(50);
        })
    });
    cb.record(Outcome::Failure);
    assert_eq!(cb.state(), BreakerState::Open);

    clock.advance(Duration::from_millis(70));

    // Spawn many threads simultaneously. Only one probe slot;
    // exactly one Ok overall.
    let cb_arc = Arc::new(cb);
    let barrier = Arc::new(std::sync::Barrier::new(16));
    let handles: Vec<_> = (0..16)
        .map(|_| {
            let cb = cb_arc.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                cb.check().is_ok()
            })
        })
        .collect();
    let oks: usize = handles
        .into_iter()
        .map(|h| h.join().unwrap() as usize)
        .sum();
    assert_eq!(oks, 1, "exactly one thread should claim the probe slot");
    assert_eq!(cb_arc.state(), BreakerState::HalfOpen);
}
