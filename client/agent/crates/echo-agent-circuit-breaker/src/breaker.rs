//! The [`CircuitBreaker`] state machine: sliding-window-with-min-samples
//! algorithm with three states (`Closed`, `Open`, `HalfOpen`) and an
//! atomic-mirror lock-free fast-path for `is_open()`.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::clock::{Clock, SystemClock};
use crate::config::BreakerConfig;
use crate::observer::{NoopObserver, Observer};
use crate::state::{BreakerOpen, BreakerState, Outcome};
use crate::window::SlidingWindow;

static NOOP_OBSERVER: NoopObserver = NoopObserver;

/// Cheaply-clonable handle around a shared [`CircuitBreakerInner`].
#[derive(Clone)]
pub struct CircuitBreaker {
    inner: Arc<CircuitBreakerInner>,
}

pub(crate) struct CircuitBreakerInner {
    config: BreakerConfig,
    state: AtomicU8,
    /// Monotonic baseline captured at construction; `opened_at_millis`
    /// stores millisecond offsets from this instant, avoiding NTP
    /// drift issues and letting `MockClock` drive cool-down windows.
    baseline: Instant,
    opened_at_millis: AtomicU64,
    half_open_probes: AtomicUsize,
    /// When the most recent half-open probe slot was claimed
    /// (millisecond offset from `baseline`). A probe whose owner never
    /// reaches `record()` — e.g. its future is dropped on caller
    /// cancellation — would otherwise hold its slot forever and strand
    /// the breaker in `HalfOpen`, shedding all traffic with no path
    /// back to `Closed`. `try_half_open_probe` treats a claim older
    /// than `open_duration` as abandoned and lets one caller reclaim
    /// it, so a lost probe delays recovery by at most one cool-down.
    probe_claimed_at_millis: AtomicU64,
    /// Lock-free mirror of `state == Open`. Written after the
    /// authoritative `state` store with `Release`; read with
    /// `Relaxed` from the `is_open()` hot path.
    is_open_fast: AtomicBool,
    window: Mutex<SlidingWindow>,
    clock: Arc<dyn Clock>,
    /// Install-once-on-shared-inner so `with_observer` keeps working
    /// after a clone (the registry hands out clones).
    observer: OnceLock<Arc<dyn Observer>>,
}

impl CircuitBreaker {
    /// Construct a breaker with the [`SystemClock`] and a no-op observer.
    pub fn new(config: BreakerConfig) -> Self {
        Self::with_clock(config, Arc::new(SystemClock))
    }

    /// Construct a breaker with an injected clock (used by tests to
    /// drive cool-down windows deterministically).
    pub fn with_clock(mut config: BreakerConfig, clock: Arc<dyn Clock>) -> Self {
        config.half_open_max_probes = config.half_open_max_probes.max(1);
        let baseline = clock.now();
        Self {
            inner: Arc::new(CircuitBreakerInner {
                config,
                state: AtomicU8::new(BreakerState::Closed as u8),
                baseline,
                opened_at_millis: AtomicU64::new(0),
                half_open_probes: AtomicUsize::new(0),
                probe_claimed_at_millis: AtomicU64::new(0),
                is_open_fast: AtomicBool::new(false),
                window: Mutex::new(SlidingWindow::new()),
                clock,
                observer: OnceLock::new(),
            }),
        }
    }

    /// Install an [`Observer`] that receives telemetry callbacks.
    /// First install wins (`OnceLock`); safe after clone.
    pub fn with_observer(self, observer: Arc<dyn Observer>) -> Self {
        let _ = self.inner.observer.set(observer);
        self
    }

    fn observer(&self) -> &dyn Observer {
        self.inner
            .observer
            .get()
            .map(|a| a.as_ref() as &dyn Observer)
            .unwrap_or(&NOOP_OBSERVER)
    }

    /// Consult the breaker before issuing a request. Returns `Ok` if
    /// the request may proceed, `Err(BreakerOpen)` if the breaker is
    /// currently shedding traffic.
    pub fn check(&self) -> Result<(), BreakerOpen> {
        if !self.inner.config.enabled {
            return Ok(());
        }
        match self.state() {
            BreakerState::Closed => Ok(()),
            BreakerState::Open => self.check_open(),
            BreakerState::HalfOpen => self.try_half_open_probe(),
        }
    }

    /// Record the outcome of a request.
    pub fn record(&self, outcome: Outcome) {
        if !self.inner.config.enabled {
            return;
        }
        let is_failure = matches!(outcome, Outcome::Failure);
        let now = self.inner.clock.now();
        let prev_state = self.state();

        match prev_state {
            BreakerState::Closed => {
                let should_trip = {
                    let mut window = self.lock_window();
                    window.push(is_failure, now);
                    window.evict(self.inner.config.window_duration, now);
                    window.sample_count() >= self.inner.config.min_samples
                        && window.error_rate() >= self.inner.config.error_rate_threshold
                };
                if should_trip {
                    self.trip(prev_state, "trip");
                }
            }
            BreakerState::HalfOpen => {
                if is_failure {
                    self.trip(prev_state, "probe_failure");
                } else {
                    self.close(prev_state, "probe_success");
                }
            }
            BreakerState::Open => {
                let mut window = self.lock_window();
                window.push(is_failure, now);
                window.evict(self.inner.config.window_duration, now);
            }
        }

        let new_state = self.state();
        self.observer().on_outcome(outcome, new_state);
    }

    /// Current authoritative [`BreakerState`].
    pub fn state(&self) -> BreakerState {
        BreakerState::from_u8(self.inner.state.load(Ordering::Acquire))
    }

    /// Lock-free "is the breaker currently open?" check (`Relaxed`
    /// load of the `is_open_fast` mirror).
    pub fn is_open(&self) -> bool {
        self.inner.is_open_fast.load(Ordering::Relaxed)
    }

    /// Failure rate over the live sliding window (`0.0` for an empty
    /// window). Evicts samples older than `window_duration` against
    /// the breaker's clock before computing the rate so reads stay
    /// time-window-accurate even when no `record()` fired recently.
    pub fn error_rate(&self) -> f64 {
        let now = self.inner.clock.now();
        let mut window = self.lock_window();
        window.evict(self.inner.config.window_duration, now);
        window.error_rate()
    }

    /// `true` if `status` is in the configured failure code set.
    pub fn is_failure_status(&self, status: u16) -> bool {
        self.inner.config.is_failure_status(status)
    }

    /// Force-transition to `HalfOpen` for tests (bypasses the
    /// open-duration timer).
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn force_half_open(&self) {
        let prev = self.state();
        self.inner
            .state
            .store(BreakerState::HalfOpen as u8, Ordering::Release);
        self.inner.is_open_fast.store(false, Ordering::Release);
        self.inner.half_open_probes.store(0, Ordering::Release);
        if prev != BreakerState::HalfOpen {
            self.observer()
                .on_state_change(prev, BreakerState::HalfOpen, "force_half_open");
        }
    }

    fn lock_window(&self) -> std::sync::MutexGuard<'_, SlidingWindow> {
        self.inner.window.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn elapsed_millis(&self) -> u64 {
        self.inner
            .clock
            .now()
            .saturating_duration_since(self.inner.baseline)
            .as_millis() as u64
    }

    fn check_open(&self) -> Result<(), BreakerOpen> {
        let opened = self.inner.opened_at_millis.load(Ordering::Acquire);
        let now = self.elapsed_millis();
        let elapsed = Duration::from_millis(now.saturating_sub(opened));

        if elapsed >= self.inner.config.open_duration {
            if self.cas_state(BreakerState::Open, BreakerState::HalfOpen) {
                self.inner.is_open_fast.store(false, Ordering::Release);
                // Do NOT reset `half_open_probes` here. It is already 0:
                // `trip()` zeroes it on entry to `Open` and nothing
                // increments it while `Open`. Resetting after the CAS
                // publishes `HalfOpen` races a loser thread that observes
                // `HalfOpen` and claims a probe slot in the gap, which the
                // reset would then clear — admitting two probes instead of
                // one.
                self.observer().on_state_change(
                    BreakerState::Open,
                    BreakerState::HalfOpen,
                    "open_elapsed",
                );
                // Route through the shared probe-accounting path so
                // the loser of the CAS race and the winner agree on
                // the counter.
                return self.try_half_open_probe();
            }
            // Lost CAS race — re-evaluate.
            match self.state() {
                BreakerState::Closed => return Ok(()),
                BreakerState::HalfOpen => return self.try_half_open_probe(),
                BreakerState::Open => {
                    let opened = self.inner.opened_at_millis.load(Ordering::Acquire);
                    let elapsed =
                        Duration::from_millis(self.elapsed_millis().saturating_sub(opened));
                    return Err(BreakerOpen {
                        retry_after: self.inner.config.open_duration.saturating_sub(elapsed),
                    });
                }
            }
        }

        Err(BreakerOpen {
            retry_after: self.inner.config.open_duration.saturating_sub(elapsed),
        })
    }

    fn trip(&self, prev: BreakerState, reason: &'static str) {
        self.inner
            .state
            .store(BreakerState::Open as u8, Ordering::Release);
        self.inner
            .opened_at_millis
            .store(self.elapsed_millis(), Ordering::Release);
        self.inner.half_open_probes.store(0, Ordering::Release);
        // Mirror after the authoritative state store.
        self.inner.is_open_fast.store(true, Ordering::Release);
        if prev != BreakerState::Open {
            self.observer()
                .on_state_change(prev, BreakerState::Open, reason);
        }
    }

    fn close(&self, prev: BreakerState, reason: &'static str) {
        self.inner
            .state
            .store(BreakerState::Closed as u8, Ordering::Release);
        self.lock_window().clear();
        self.inner.half_open_probes.store(0, Ordering::Release);
        self.inner.is_open_fast.store(false, Ordering::Release);
        if prev != BreakerState::Closed {
            self.observer()
                .on_state_change(prev, BreakerState::Closed, reason);
        }
    }

    fn try_half_open_probe(&self) -> Result<(), BreakerOpen> {
        let now = self.elapsed_millis();
        let prev = self.inner.half_open_probes.fetch_add(1, Ordering::AcqRel);
        if prev < self.inner.config.half_open_max_probes {
            self.inner
                .probe_claimed_at_millis
                .store(now, Ordering::Release);
            self.observer().on_probe_admission(true);
            return Ok(());
        }
        self.inner.half_open_probes.fetch_sub(1, Ordering::AcqRel);

        // All probe slots are claimed. A claim is only released via
        // `record()`; if a probe's owner was cancelled before recording
        // (its future dropped mid-flight), the slot would be held forever
        // and the breaker could never leave `HalfOpen`. Treat a claim
        // older than `open_duration` as abandoned and let exactly one
        // caller (the CAS winner) take it over. A slow-but-alive probe
        // that outlives the lease may briefly coexist with its
        // replacement; both outcomes are recorded, same as running with
        // an extra probe slot.
        let lease_millis = self.inner.config.open_duration.as_millis() as u64;
        let claimed = self.inner.probe_claimed_at_millis.load(Ordering::Acquire);
        if now.saturating_sub(claimed) >= lease_millis
            && self
                .inner
                .probe_claimed_at_millis
                .compare_exchange(claimed, now, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            self.observer().on_probe_admission(true);
            return Ok(());
        }

        self.observer().on_probe_admission(false);
        // Slot-exhausted rejection: callers that map this to HTTP
        // `Retry-After` shouldn't advertise the full open-duration
        // cool-down; advertise a small fixed backoff (capped to
        // `open_duration`).
        const HALF_OPEN_PROBE_BACKOFF: Duration = Duration::from_millis(50);
        Err(BreakerOpen {
            retry_after: HALF_OPEN_PROBE_BACKOFF.min(self.inner.config.open_duration),
        })
    }

    fn cas_state(&self, from: BreakerState, to: BreakerState) -> bool {
        self.inner
            .state
            .compare_exchange(from as u8, to as u8, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

impl std::fmt::Debug for CircuitBreaker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CircuitBreaker")
            .field("state", &self.state())
            .field("error_rate", &self.error_rate())
            .finish()
    }
}

#[cfg(test)]
#[path = "breaker_tests/support.rs"]
mod support;

#[cfg(test)]
#[path = "breaker_tests/state_machine.rs"]
mod state_machine;

#[cfg(test)]
#[path = "breaker_tests/half_open.rs"]
mod half_open;

#[cfg(test)]
#[path = "breaker_tests/parity.rs"]
mod parity;

#[cfg(test)]
#[path = "breaker_tests/observer.rs"]
mod observer_tests;

#[cfg(test)]
#[path = "breaker_tests/breaker_size.rs"]
mod breaker_size;

#[cfg(test)]
#[path = "breaker_tests/concurrent.rs"]
mod concurrent;
