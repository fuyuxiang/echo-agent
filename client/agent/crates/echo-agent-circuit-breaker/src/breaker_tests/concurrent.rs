//! Concurrent-access stress tests.

use std::sync::Arc;
use std::thread;

use super::super::*;
use super::support::fast_config;

#[test]
fn concurrent_check_and_record_no_panic() {
    let cb = CircuitBreaker::new(fast_config(|c| {
        c.min_samples = 5;
        c.open_duration = std::time::Duration::from_millis(10);
        c.half_open_max_probes = 2;
    }));

    let handles: Vec<_> = (0..8)
        .map(|i| {
            let cb = cb.clone();
            thread::spawn(move || {
                for j in 0..200 {
                    let _ = cb.check();
                    let outcome = if (i + j) % 3 == 0 {
                        Outcome::Failure
                    } else {
                        Outcome::Success
                    };
                    cb.record(outcome);
                }
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    // State and error_rate must be readable without panic
    let _state = cb.state();
    let _rate = cb.error_rate();
}

/// 100 threads × 100 `record(Failure)` calls. The breaker must remain
/// readable, its sliding window must stay bounded at
/// `MAX_WINDOW_ENTRIES` (10k), and `error_rate()` must read as a
/// finite f64 (no NaN from divide-by-zero or counter corruption).
#[test]
fn concurrent_record_does_not_panic_or_corrupt_window() {
    let cb = Arc::new(CircuitBreaker::new(BreakerConfig {
        // Keep the breaker closed throughout so every record() goes
        // through the `Closed` branch that mutates the window.
        enabled: true,
        min_samples: usize::MAX,
        error_rate_threshold: 2.0,
        ..BreakerConfig::server()
    }));
    let handles: Vec<_> = (0..100)
        .map(|_| {
            let cb = cb.clone();
            thread::spawn(move || {
                for _ in 0..100 {
                    cb.record(Outcome::Failure);
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }

    // Breaker must still report a valid state.
    assert!(matches!(
        cb.state(),
        BreakerState::Closed | BreakerState::Open | BreakerState::HalfOpen
    ));
    // min_samples = usize::MAX and threshold = 2.0 keep us Closed.
    assert_eq!(cb.state(), BreakerState::Closed);
    // 10,000 failures into an unbounded-rate-threshold breaker must
    // produce a finite error_rate — no NaN from a corrupted counter.
    let rate = cb.error_rate();
    assert!(rate.is_finite(), "error_rate must be finite, got {rate}");
    assert!(
        (0.0..=1.0).contains(&rate),
        "error_rate out of range: {rate}"
    );
}
