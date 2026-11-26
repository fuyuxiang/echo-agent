//! Gesture step tables G1–G11: the timed SGR wheel-report shapes every
//! matrix cell replays.
//!
//! A gesture is a `&'static [WheelStep]`; the A13 runner emits each step's
//! report after sleeping `pre_delay_ms` (never before the first step, never
//! after the last — the `send_wheel_sequence` contract in the pager's
//! `tests/pty_e2e/scroll.rs`). Delays are HOST-side lower bounds: scheduler
//! jitter can only stretch gaps, which is why the invariant suite
//! ([`super::invariants`]) judges timing from the recorder's own clock.
//!
//! Timing thresholds below mirror the pager's `src/input/mouse.rs` (the
//! harness deliberately has no pager dependency — same drift-tripwire
//! stance as [`super::log`]): a table's shape is meaningful only relative
//! to those constants, e.g. G2's 50ms notch gap sits under `STREAM_GAP_MS`
//! (one stream) but over `WHEEL_TICK_DETECT_MAX_MS` (no wheel promotion of
//! the train as one tick).

use crate::scripted::{SGR_SCROLL_DOWN, SGR_SCROLL_UP};

/// Mirror of `mouse.rs` `REDRAW_CADENCE_MS`: minimum flush spacing.
pub const REDRAW_CADENCE_MS: u64 = 16;
/// Mirror of `mouse.rs` `STREAM_GAP_MS`: idle gap that finalizes a stream.
pub const STREAM_GAP_MS: u64 = 80;
/// Mirror of `mouse.rs` `DEFAULT_WHEEL_TICK_DETECT_MAX_MS`: an ept≥2 stream
/// promotes to wheel only when the first tick completes within this window.
pub const WHEEL_TICK_DETECT_MAX_MS: u64 = 12;
/// Mirror of `mouse.rs` `ACCEL_MIN_INTERVAL_MS`: sub-6ms inter-event
/// intervals are terminal batching artifacts and stay out of the
/// accel/detection interval window.
pub const ACCEL_MIN_INTERVAL_MS: f64 = 6.0;
/// Mirror of `mouse.rs` `DEFAULT_TRACKPAD_ACCEL_MAX`: accel clamp ceiling.
pub const TRACKPAD_ACCEL_MAX: f64 = 3.0;
/// Mirror of `mouse.rs` `MIN_LINES_PER_WHEEL_STREAM`.
pub const MIN_LINES_PER_WHEEL_STREAM: i64 = 1;

/// One SGR wheel report: sleep `pre_delay_ms`, then emit `button`.
///
/// `button` is [`SGR_SCROLL_UP`]/[`SGR_SCROLL_DOWN`] — `u16` because those
/// harness consts are `u16` (the design sketch said `u8`; deviating keeps
/// one shared definition instead of a cast at every emission site).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WheelStep {
    /// Host-side sleep before emitting this report (0 for the first step).
    pub pre_delay_ms: u64,
    /// SGR wheel button code (64 up / 65 down).
    pub button: u16,
}

/// `count` same-direction reports, `interval_ms` between consecutive ones.
const fn burst<const N: usize>(interval_ms: u64, button: u16) -> [WheelStep; N] {
    let mut steps = [WheelStep {
        pre_delay_ms: 0,
        button,
    }; N];
    let mut i = 1;
    while i < N {
        steps[i].pre_delay_ms = interval_ms;
        i += 1;
    }
    steps
}

/// Notches of `events_per_notch` back-to-back reports, `notch_gap_ms` apart.
const fn notch_train<const N: usize>(
    events_per_notch: usize,
    notch_gap_ms: u64,
    button: u16,
) -> [WheelStep; N] {
    let mut steps = burst::<N>(0, button);
    let mut i = events_per_notch;
    while i < N {
        if i.is_multiple_of(events_per_notch) {
            steps[i].pre_delay_ms = notch_gap_ms;
        }
        i += 1;
    }
    steps
}

/// G1 single notch, ept=3 brands: 3 back-to-back reports (first tick lands
/// inside the 12ms window → wheel promotion in Auto mode).
pub const G1_NOTCH_EPT3: [WheelStep; 3] = burst::<3>(0, SGR_SCROLL_UP);
/// G1 single notch, ept=1 brands (iTerm2/zed/vscode/mux): one report.
pub const G1_NOTCH_EPT1: [WheelStep; 1] = burst::<1>(0, SGR_SCROLL_UP);
/// G2 notch train: 5 notches 50ms apart (< `STREAM_GAP_MS` → one stream).
pub const G2_NOTCH_TRAIN_EPT3: [WheelStep; 15] = notch_train::<15>(3, 50, SGR_SCROLL_UP);
pub const G2_NOTCH_TRAIN_EPT1: [WheelStep; 5] = burst::<5>(50, SGR_SCROLL_UP);
/// G3 flood: 60 back-to-back reports (cap/pacing exercise — A4 teleport).
pub const G3_FLOOD: [WheelStep; 60] = burst::<60>(0, SGR_SCROLL_UP);
/// G4 jerk repro: a 3-event anti-promotion head at 8ms, 57 dense reports,
/// then a decelerating 6-event tail (gaps growing 40→70ms, all under the
/// 80ms stream gap).
///
/// Two shape details make the repro real under a PTY (verified against the
/// live recorder):
/// - **The head.** Back-to-back writes arrive batched, so a fully dense
///   burst completes its first ept=3 tick inside the 12ms window and
///   promotes to WHEEL pricing — which never re-prices at finalize and
///   never jerks. The 8ms head lands the first tick past the window, so
///   the stream stays Unknown (priced ~1 line/event, accel window seeded
///   in the fast band).
/// - **The 40ms+ tail gaps.** They open cadence slots with no new events
///   while the dense backlog is still draining: capped `events_since_flush
///   == 0` coast flushes (the I-SMOOTH-COAST signature). Tighter gaps ride
///   every slot and mask the coast.
///
/// At the gap finalize the Unknown→Trackpad re-price (accel-weighted,
/// ~2.5× the mid-stream pricing) then bursts one more capped flush and
/// drops the rest — the I-NO-DROP half of the jerk (xfail cell until the
/// finalize-decel fix).
pub const G4_JERK: [WheelStep; 66] = {
    let mut steps = burst::<66>(0, SGR_SCROLL_UP);
    steps[1].pre_delay_ms = 8;
    steps[2].pre_delay_ms = 8;
    let tail = [40, 44, 50, 55, 60, 70];
    let mut i = 0;
    while i < tail.len() {
        steps[60 + i].pre_delay_ms = tail[i];
        i += 1;
    }
    steps
};
/// G5 ghostty dup: 10 notches 60ms apart, each report duplicated 4ms later
/// (ghostty emits ≥2 SGR reports per physical notch ~4ms apart). The 4ms
/// dups sit under `ACCEL_MIN_INTERVAL_MS` and must stay out of the
/// interval window — the I-ACCEL G5 clause.
pub const G5_GHOSTTY_DUP: [WheelStep; 20] = {
    let mut steps = burst::<20>(0, SGR_SCROLL_UP);
    let mut i = 1;
    while i < 20 {
        steps[i].pre_delay_ms = if i % 2 == 1 { 4 } else { 60 };
        i += 1;
    }
    steps
};
/// G6 flip: 10×8ms up then 10×8ms down — the direction flip finalizes
/// stream 1 and opens stream 2 at the same instant (two streams).
pub const G6_FLIP: [WheelStep; 20] = {
    let mut steps = burst::<20>(8, SGR_SCROLL_UP);
    let mut i = 10;
    while i < 20 {
        steps[i].button = SGR_SCROLL_DOWN;
        i += 1;
    }
    steps
};
/// G7 overscroll: bottom-pinned 10×8ms down (viewport must clamp — the
/// harness-side I-SCREEN check) then 3 up (must move again). Two streams.
pub const G7_OVERSCROLL: [WheelStep; 13] = {
    let mut steps = burst::<13>(8, SGR_SCROLL_DOWN);
    let mut i = 10;
    while i < 13 {
        steps[i].button = SGR_SCROLL_UP;
        i += 1;
    }
    steps
};
/// G9a mux re-chunk 1:1: 8 single reports 55ms apart (tmux re-emitting one
/// event per notch; 55ms > the 30ms ept=1 trackpad-detect window → never
/// promotes to trackpad mid-stream).
pub const G9A_MUX_SINGLES: [WheelStep; 8] = burst::<8>(55, SGR_SCROLL_UP);
/// G9b mux re-chunk batch: 8 notches 55ms apart × 3 back-to-back events
/// (tmux passing through an inner ept=3 chunking, re-timed).
pub const G9B_MUX_BATCH: [WheelStep; 24] = notch_train::<24>(3, 55, SGR_SCROLL_UP);
/// G10 ambiguous slow roll: 12 reports 40ms apart — inside the vscode-embed
/// 60ms trackpad-detect window, outside the default 30ms one.
pub const G10_AMBIGUOUS_SLOW: [WheelStep; 12] = burst::<12>(40, SGR_SCROLL_UP);
/// G11 carry: one notch, a 120ms wait (> `STREAM_GAP_MS` → finalize), one
/// notch — the sub-line carry handoff across same-direction streams.
pub const G11_CARRY_EPT3: [WheelStep; 6] = notch_train::<6>(3, 120, SGR_SCROLL_UP);
pub const G11_CARRY_EPT1: [WheelStep; 2] = burst::<2>(120, SGR_SCROLL_UP);

/// Gesture identifier a [`super::cells::MatrixCell`] references.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GestureId {
    G1Notch,
    G2NotchTrain,
    G3Flood,
    G4Jerk,
    G5GhosttyDup,
    G6Flip,
    G7Overscroll,
    /// G2's step table replayed while the turn is still streaming
    /// (`SessionKind::Streaming`); the shape lives in the session, not here.
    G8MidStreamTrain,
    G9aMuxSingles,
    G9bMuxBatch,
    G10AmbiguousSlow,
    G11Carry,
}

impl GestureId {
    /// Every gesture, for exhaustive table sweeps (tests, the A13 runner).
    pub const ALL: [GestureId; 12] = [
        GestureId::G1Notch,
        GestureId::G2NotchTrain,
        GestureId::G3Flood,
        GestureId::G4Jerk,
        GestureId::G5GhosttyDup,
        GestureId::G6Flip,
        GestureId::G7Overscroll,
        GestureId::G8MidStreamTrain,
        GestureId::G9aMuxSingles,
        GestureId::G9bMuxBatch,
        GestureId::G10AmbiguousSlow,
        GestureId::G11Carry,
    ];

    /// Step table for this gesture on a brand with `ept` events per notch.
    /// Only the notch-based gestures (G1/G2/G8/G11) vary by class; the rest
    /// are fixed event shapes (G9b is deliberately 3-per-notch even on the
    /// ept=1 mux profile — it simulates the mux passing re-chunked input).
    pub fn steps(self, ept: u16) -> &'static [WheelStep] {
        let ept3 = ept >= 2;
        match self {
            GestureId::G1Notch => {
                if ept3 {
                    &G1_NOTCH_EPT3
                } else {
                    &G1_NOTCH_EPT1
                }
            }
            GestureId::G2NotchTrain | GestureId::G8MidStreamTrain => {
                if ept3 {
                    &G2_NOTCH_TRAIN_EPT3
                } else {
                    &G2_NOTCH_TRAIN_EPT1
                }
            }
            GestureId::G3Flood => &G3_FLOOD,
            GestureId::G4Jerk => &G4_JERK,
            GestureId::G5GhosttyDup => &G5_GHOSTTY_DUP,
            GestureId::G6Flip => &G6_FLIP,
            GestureId::G7Overscroll => &G7_OVERSCROLL,
            GestureId::G9aMuxSingles => &G9A_MUX_SINGLES,
            GestureId::G9bMuxBatch => &G9B_MUX_BATCH,
            GestureId::G10AmbiguousSlow => &G10_AMBIGUOUS_SLOW,
            GestureId::G11Carry => {
                if ept3 {
                    &G11_CARRY_EPT3
                } else {
                    &G11_CARRY_EPT1
                }
            }
        }
    }

    /// Streams (finalize records) this gesture produces: 1, plus one per
    /// direction flip or >`STREAM_GAP_MS` intra-gesture pause. The A13
    /// runner's `wait_for_finalize_count` target.
    pub fn expected_streams(self) -> usize {
        match self {
            GestureId::G6Flip | GestureId::G7Overscroll | GestureId::G11Carry => 2,
            _ => 1,
        }
    }
}

/// `(up, down)` report counts — direction-sum test primitive.
pub fn direction_counts(steps: &[WheelStep]) -> (usize, usize) {
    let up = steps.iter().filter(|s| s.button == SGR_SCROLL_UP).count();
    (up, steps.len() - up)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Streams split on gaps > STREAM_GAP_MS or direction flips — recompute
    /// from the table and compare to the declared `expected_streams`.
    fn streams_in(steps: &[WheelStep]) -> usize {
        let mut streams = 1;
        for pair in steps.windows(2) {
            if pair[1].pre_delay_ms > STREAM_GAP_MS || pair[1].button != pair[0].button {
                streams += 1;
            }
        }
        streams
    }

    #[test]
    fn table_counts_and_direction_sums() {
        assert_eq!(G1_NOTCH_EPT3.len(), 3);
        assert_eq!(G1_NOTCH_EPT1.len(), 1);
        assert_eq!(G2_NOTCH_TRAIN_EPT3.len(), 15);
        assert_eq!(G2_NOTCH_TRAIN_EPT1.len(), 5);
        assert_eq!(G3_FLOOD.len(), 60);
        assert_eq!(G4_JERK.len(), 66);
        assert_eq!(G5_GHOSTTY_DUP.len(), 20);
        assert_eq!(G9B_MUX_BATCH.len(), 24);
        assert_eq!(G10_AMBIGUOUS_SLOW.len(), 12);

        assert_eq!(direction_counts(&G3_FLOOD), (60, 0));
        assert_eq!(direction_counts(&G6_FLIP), (10, 10), "flip nets to zero");
        assert_eq!(direction_counts(&G7_OVERSCROLL), (3, 10));
        assert_eq!(direction_counts(&G9A_MUX_SINGLES), (8, 0));
    }

    #[test]
    fn notch_structure_and_gaps() {
        // G2/ept3: notch starts every 3 events carry the 50ms gap, intra-notch 0.
        for (i, step) in G2_NOTCH_TRAIN_EPT3.iter().enumerate() {
            let expected = if i > 0 && i % 3 == 0 { 50 } else { 0 };
            assert_eq!(step.pre_delay_ms, expected, "G2 ept3 step {i}");
        }
        // G9b: same shape at 55ms — under the 80ms gap, one stream.
        let notch_gaps = G9B_MUX_BATCH
            .iter()
            .filter(|s| s.pre_delay_ms == 55)
            .count();
        assert_eq!(notch_gaps, 7, "8 notches → 7 inter-notch gaps");
        // G5: dup 4ms after each notch head, notch heads 60ms apart.
        for (i, step) in G5_GHOSTTY_DUP.iter().enumerate() {
            let expected = if i == 0 {
                0
            } else if i % 2 == 1 {
                4
            } else {
                60
            };
            assert_eq!(step.pre_delay_ms, expected, "G5 step {i}");
        }
    }

    #[test]
    fn jerk_head_blocks_promotion_and_tail_decays_monotonically() {
        // Anti-promotion head: the first ept=3 tick must complete strictly
        // after the 12ms wheel-promotion window even with zero jitter
        // (sleeps only stretch), or PTY batching wheel-promotes the burst
        // and the finalize re-price under test never happens.
        let head_span: u64 = G4_JERK[..3].iter().map(|s| s.pre_delay_ms).sum();
        assert!(head_span > WHEEL_TICK_DETECT_MAX_MS);

        let tail: Vec<u64> = G4_JERK[60..].iter().map(|s| s.pre_delay_ms).collect();
        assert_eq!(tail, vec![40, 44, 50, 55, 60, 70]);
        assert!(
            tail.windows(2).all(|w| w[0] < w[1]),
            "strictly decelerating"
        );
        // Coast window: every tail gap opens ≥2 empty 16ms cadence slots so
        // the dense backlog drains as events_since_flush == 0 flushes.
        assert!(tail.iter().all(|&gap| gap >= 2 * REDRAW_CADENCE_MS));
        assert!(G4_JERK[3..60].iter().all(|s| s.pre_delay_ms == 0));
    }

    #[test]
    fn delays_agree_with_stream_gap_thresholds() {
        // Single-stream gestures never pause past the 80ms finalize gap and
        // never flip; multi-stream ones split exactly as declared.
        for gesture in GestureId::ALL {
            for ept in [1u16, 3] {
                let steps = gesture.steps(ept);
                assert!(!steps.is_empty());
                assert_eq!(steps[0].pre_delay_ms, 0, "{gesture:?} first step");
                assert_eq!(
                    streams_in(steps),
                    gesture.expected_streams(),
                    "{gesture:?} ept={ept}: table shape vs declared stream count"
                );
            }
        }
        // The G11 pause is what splits it: strictly past the finalize gap.
        assert!(G11_CARRY_EPT3[3].pre_delay_ms > STREAM_GAP_MS);
        assert!(G11_CARRY_EPT1[1].pre_delay_ms > STREAM_GAP_MS);
        // G1/ept3 is a first tick inside the wheel-promotion window.
        let g1_span: u64 = G1_NOTCH_EPT3.iter().map(|s| s.pre_delay_ms).sum();
        assert!(g1_span <= WHEEL_TICK_DETECT_MAX_MS);
        // G5's dup spacing must sit under the interval-window floor.
        assert!((G5_GHOSTTY_DUP[1].pre_delay_ms as f64) < ACCEL_MIN_INTERVAL_MS);
    }
}
