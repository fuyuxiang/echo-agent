use std::time::Duration;

use super::DualClock;

/// A backward wall jump (NTP step) clamps to zero rather than underflowing,
/// so it can never fabricate a suspend or inflate a duration.
#[test]
fn backward_wall_jump_clamps_to_zero() {
    let start = DualClock::now();
    let stepped_back = DualClock {
        mono: start.mono + Duration::from_secs(5),
        wall: start.wall - Duration::from_secs(60),
    };
    assert_eq!(
        start.elapsed_between(stepped_back),
        (Duration::from_secs(5), Duration::ZERO)
    );
}
