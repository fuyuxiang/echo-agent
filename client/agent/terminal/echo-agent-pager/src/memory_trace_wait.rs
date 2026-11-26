//! Full-interval wait for the detached memory sampler.
//!
//! `std::thread::sleep` asserts when `nanosleep` returns a non-`EINTR` errno.
//! An outer `SIGCHLD` handler can overwrite the interrupted-sleep `EINTR` with
//! `EAGAIN`; release builds use `panic=abort`, so that assert kills the whole
//! CLI. `park_timeout` does not inspect errno that way, and the loop below
//! recomputes remaining time from a single start instant so an early wake
//! never shortens the sampling period.
//!
//! Contract: this consumes the calling thread's park token, so it is only safe
//! on a thread nobody else unparks. The memtrace sampler satisfies that today
//! because its `JoinHandle` is dropped and nothing retains a `Thread` handle.

use std::time::{Duration, Instant};

pub(super) fn wait_full_interval(interval: Duration) {
    let started = Instant::now();
    loop {
        let remaining = interval.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return;
        }
        std::thread::park_timeout(remaining);
    }
}
