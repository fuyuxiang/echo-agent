//! Tests for [`crate::window::SlidingWindow`].

use super::*;

#[test]
fn push_evict_and_error_rate() {
    let mut w = SlidingWindow::new();
    let base = Instant::now();
    w.push(true, base);
    w.push(false, base + Duration::from_millis(10));
    w.push(true, base + Duration::from_millis(20));

    assert_eq!(w.sample_count(), 3);
    assert!((w.error_rate() - (2.0 / 3.0)).abs() < 1e-9);

    // Evict everything older than 5ms relative to base + 20ms.
    w.evict(Duration::from_millis(5), base + Duration::from_millis(20));
    assert_eq!(w.sample_count(), 1);
}

#[test]
fn empty_error_rate_is_zero() {
    let w = SlidingWindow::new();
    assert_eq!(w.error_rate(), 0.0);
}

#[test]
fn push_respects_max_entries_cap() {
    let mut w = SlidingWindow::new();
    let base = Instant::now();
    for i in 0..(MAX_WINDOW_ENTRIES + 5) {
        w.push(true, base + Duration::from_nanos(i as u64));
    }
    assert_eq!(w.sample_count(), MAX_WINDOW_ENTRIES);
}

#[test]
fn failure_count_stays_consistent_under_cap_eviction() {
    // Push enough failures to overflow the cap and confirm
    // error_rate() (which is O(1) via the cached failures
    // counter) still reads 1.0 after entries are dropped from
    // the front.
    let mut w = SlidingWindow::new();
    let base = Instant::now();
    for i in 0..(MAX_WINDOW_ENTRIES + 100) {
        w.push(true, base + Duration::from_nanos(i as u64));
    }
    assert_eq!(w.sample_count(), MAX_WINDOW_ENTRIES);
    assert!((w.error_rate() - 1.0).abs() < f64::EPSILON);
}

#[test]
fn failure_count_decrements_on_time_eviction() {
    let mut w = SlidingWindow::new();
    let base = Instant::now();
    w.push(true, base);
    w.push(false, base + Duration::from_millis(10));
    w.push(true, base + Duration::from_millis(20));
    assert!((w.error_rate() - (2.0 / 3.0)).abs() < 1e-9);

    // Evict the first two entries (the leading true and false).
    // Remaining is one true → error_rate = 1.0.
    w.evict(Duration::from_millis(5), base + Duration::from_millis(20));
    assert_eq!(w.sample_count(), 1);
    assert!((w.error_rate() - 1.0).abs() < f64::EPSILON);
}

#[test]
fn clear_resets_failure_count() {
    let mut w = SlidingWindow::new();
    let base = Instant::now();
    w.push(true, base);
    w.push(true, base + Duration::from_millis(1));
    w.clear();
    // After clear, pushing one success must read error_rate 0.0;
    // a stale failures counter would read 2/1 instead.
    w.push(false, base + Duration::from_millis(2));
    assert!(w.error_rate().abs() < f64::EPSILON);
}

/// Push past the cap, then advance time past the window duration and
/// push more — eviction must continue to read the correct cached
/// failures count even when the deque is at the cap.
#[test]
fn cap_then_time_eviction_keeps_failure_count_consistent() {
    let mut w = SlidingWindow::new();
    let base = Instant::now();

    // Fill the deque to the cap with failures.
    for i in 0..MAX_WINDOW_ENTRIES {
        w.push(true, base + Duration::from_micros(i as u64));
    }
    assert_eq!(w.sample_count(), MAX_WINDOW_ENTRIES);
    assert!((w.error_rate() - 1.0).abs() < f64::EPSILON);

    // Move past the window and evict — every existing sample falls
    // out, cached failures counter must reach zero.
    let way_later = base + Duration::from_secs(3600);
    w.evict(Duration::from_secs(1), way_later);
    assert_eq!(w.sample_count(), 0);
    assert!(w.error_rate().abs() < f64::EPSILON);

    // New samples after a full eviction must continue to read
    // consistently (regression on a stale `failures` field).
    w.push(false, way_later);
    w.push(true, way_later + Duration::from_micros(1));
    assert_eq!(w.sample_count(), 2);
    assert!((w.error_rate() - 0.5).abs() < f64::EPSILON);
}
