//! Crate-contract tests: handle size and `with_observer` after clone.

use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use super::super::*;

/// Pins the 8-byte handle size that the `clippy::large_enum_variant`
/// fix depends on.
#[test]
fn handle_is_pointer_sized() {
    assert_eq!(
        std::mem::size_of::<CircuitBreaker>(),
        std::mem::size_of::<usize>()
    );
}

/// `with_observer` must work even after the handle has already
/// been cloned — the registry hands out clones, so this is the
/// realistic call shape.
#[test]
fn with_observer_works_after_clone() {
    #[derive(Default)]
    struct Counting {
        transitions: StdMutex<usize>,
    }
    impl Observer for Counting {
        fn on_state_change(&self, _: BreakerState, _: BreakerState, _: &str) {
            *self.transitions.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        }
    }

    let cb = CircuitBreaker::new(BreakerConfig::client());
    let cloned = cb.clone();
    let obs = Arc::new(Counting::default());
    let _ = cloned.with_observer(obs.clone());

    // The observer was installed on the SHARED inner, so both
    // `cb` and `cloned` see it.
    for _ in 0..5 {
        cb.record(Outcome::Failure);
    }
    assert_eq!(cb.state(), BreakerState::Open);
    assert_eq!(*obs.transitions.lock().unwrap(), 1);
}
