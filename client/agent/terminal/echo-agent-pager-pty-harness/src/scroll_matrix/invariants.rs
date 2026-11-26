//! The invariant suite: predicates over grouped `ECHO_AGENT_SCROLL_LOG` streams.
//!
//! Stall-safety: every timing predicate reads the RECORDER's clock
//! (`ts_ms`, `ms_since_prev_flush`, `avg_interval_ms`), never the test
//! process's — CI load can stretch host-side gesture delays but can only
//! ever *widen* the producer-measured spacings, so no invariant here can
//! false-fail on a loaded machine.
//!
//! Two invariants are declared here but checked by the A13 matrix runner,
//! not this module: [`InvariantId::Screen`] (the viewport visibly
//! moved/clamped — needs `PtyHarness` marker positions) and
//! [`InvariantId::Quiet`] (no repaint churn after finalize — needs the
//! harness frame watermark). They exist in the id enum so cells can declare
//! them and the runner can route by [`InvariantId::is_log_side`];
//! [`check_log_invariant`] panics if asked to evaluate them.

use super::cells::ExpectedProfile;
use super::gestures::{
    ACCEL_MIN_INTERVAL_MS, MIN_LINES_PER_WHEEL_STREAM, REDRAW_CADENCE_MS, TRACKPAD_ACCEL_MAX,
};
use super::log::{ScrollLogLine, StreamGroup};

/// Float slack for f32-serialized fields (accel/speed/carry comparisons).
const F32_TOLERANCE: f64 = 0.01;

/// Invariant identifier; `as_str` is the design's I-* vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InvariantId {
    /// I-ORD: `ts_ms` is non-decreasing across the capture (flip
    /// boundaries legitimately share a timestamp).
    Ord,
    /// I-CAP: every record's `|flushed| ≤ cap` — the A4 teleport guard.
    Cap,
    /// I-DROP-EQ: finalize carries `dropped == backlog_after` (the producer
    /// constructs it that way; a mismatch is producer drift).
    DropEq,
    /// I-CADENCE: intra-stream flush spacings ≥ `REDRAW_CADENCE_MS − 1` —
    /// the A2 busy-spin guard. Skips `promotion`-triggered records (they
    /// flush immediately by design) and `finalize` records (the finalize
    /// flush deliberately ignores the cadence gate — `finalize_stream_at`
    /// in `mouse.rs` — and a flip finalize can land mid-slot; a gap
    /// finalize is ≥80ms anyway, so nothing real is lost). The design
    /// sketch listed only the promotion skip; the finalize skip is the
    /// code-verified correction.
    Cadence,
    /// I-CONS-W: forced-wheel totals are exact —
    /// `|applied+dropped| == trunc(events × wheel_lpt/ept × speed)` ±1,
    /// with the `MIN_LINES_PER_WHEEL_STREAM` substitution when the raw
    /// pricing truncates to zero. Wheel pricing never includes carry.
    ConsW,
    /// I-CONS-A: auto/trackpad totals are bounded —
    /// per-event pricing lies in `[min(wheel_lpt/ept, tp_lpt/3),
    /// max(wheel_lpt/ept, ACCEL_MAX × tp_lpt/3)] × speed` (trackpad divisor
    /// is the normalized 3, accel ceiling 3.0 = `trackpad_accel_max`;
    /// effective accel tops at 2.5 so the bound is loose-but-sound). Checks
    /// the finalize's `desired` within `[lo, hi]` and the delivered
    /// `|applied+dropped| ≤ hi`; ±1 slack absorbs carry/trunc.
    ConsA,
    /// I-ACCEL: `1.0 ≤ accel ≤ 3.0` on every record, and `avg_interval_ms`,
    /// when present, is ≥ `ACCEL_MIN_INTERVAL_MS` (6). The G5 clause: even
    /// ghostty-style 4ms duplicate reports must never drag the average
    /// under 6 — the producer excludes sub-6ms intervals from the window,
    /// so a lower value means that artifact guard regressed.
    Accel,
    /// I-CARRY: `|carry| < 1.0` everywhere (only sub-line remainders ride
    /// across streams), and a wheel-kind finalize zeroes it — the NEXT
    /// `stream_start` must echo `carry == 0`. The log carries no direction,
    /// but the machine zeroes carry at wheel finalize and resets it on
    /// direction change, so the next-start check holds for both same- and
    /// opposite-direction successors (direction-agnostic strengthening of
    /// the design's same-direction phrasing).
    Carry,
    /// I-CFG: the `stream_start` config echo matches the cell's expected
    /// profile (mode/ept/wheel_lpt/trackpad_lpt/invert/speed) — the env →
    /// profile plumbing witness.
    Cfg,
    /// I-MUX-NO-OVER: delivered total per stream ≤ `events × speed + 1` —
    /// the conservative remuxed profile (ept=1/wheel_lpt=1) prices at most
    /// one line per event. Attach only to accel-free gestures (>20ms
    /// spacing — exactly 20ms still interpolates to 1.6× in the accel band;
    /// G9's 55ms clears it): a fast trackpad-classified mux stream may
    /// legitimately exceed it via accel.
    MuxNoOver,
    /// I-SMOOTH-COAST: per stream, `Σ|flushed|` over flush-bearing records
    /// with `events_since_flush == 0` ≤ cap — motion delivered after input
    /// stopped is at most one capped catch-up. The jerk's coast-drain +
    /// finalize re-price burst exceeds it (xfail until A13's decel fix).
    SmoothCoast,
    /// I-NO-DROP: every finalize has `dropped == 0`. Attach to gestures the
    /// cap can keep up with; floods legitimately drop.
    NoDrop,
    /// I-SCREEN (harness-side, A13): viewport marker delta matches the
    /// gesture — moved on scroll, clamped at the bottom pin (G7).
    Screen,
    /// I-QUIET (harness-side, A13): frame watermark stays put after the
    /// last finalize — no post-gesture repaint churn (A2's symptom).
    Quiet,
}

impl InvariantId {
    /// Design vocabulary label.
    pub fn as_str(self) -> &'static str {
        match self {
            InvariantId::Ord => "I-ORD",
            InvariantId::Cap => "I-CAP",
            InvariantId::DropEq => "I-DROP-EQ",
            InvariantId::Cadence => "I-CADENCE",
            InvariantId::ConsW => "I-CONS-W",
            InvariantId::ConsA => "I-CONS-A",
            InvariantId::Accel => "I-ACCEL",
            InvariantId::Carry => "I-CARRY",
            InvariantId::Cfg => "I-CFG",
            InvariantId::MuxNoOver => "I-MUX-NO-OVER",
            InvariantId::SmoothCoast => "I-SMOOTH-COAST",
            InvariantId::NoDrop => "I-NO-DROP",
            InvariantId::Screen => "I-SCREEN",
            InvariantId::Quiet => "I-QUIET",
        }
    }

    /// Whether [`check_log_invariant`] can evaluate this id from the log
    /// alone. `false` = harness-side (screen/frame state), owned by the
    /// A13 runner.
    pub fn is_log_side(self) -> bool {
        !matches!(self, InvariantId::Screen | InvariantId::Quiet)
    }
}

/// Verdict of one invariant over one cell's captured streams.
#[derive(Clone, Debug, PartialEq)]
pub enum InvariantResult {
    Pass,
    Violated { detail: String },
}

impl InvariantResult {
    pub fn is_pass(&self) -> bool {
        matches!(self, InvariantResult::Pass)
    }
}

fn violated(detail: String) -> InvariantResult {
    InvariantResult::Violated { detail }
}

/// All records of all groups, in capture order.
fn records<'a>(groups: &'a [StreamGroup<'a>]) -> impl Iterator<Item = &'a ScrollLogLine> {
    groups.iter().flat_map(|g| {
        std::iter::once(g.start)
            .chain(g.flushes.iter().copied())
            .chain(g.finalize)
    })
}

/// Signed delivered total of a finalized stream: applied + discarded.
fn delivered(finalize: &ScrollLogLine) -> i64 {
    finalize.applied_total + finalize.dropped.unwrap_or(0)
}

/// Evaluate one log-side invariant. Panics on harness-side ids
/// ([`InvariantId::is_log_side`] == false) — routing those here is a
/// runner bug, not a cell verdict.
pub fn check_log_invariant(
    id: InvariantId,
    expected: &ExpectedProfile,
    groups: &[StreamGroup<'_>],
) -> InvariantResult {
    match id {
        InvariantId::Ord => check_ord(groups),
        InvariantId::Cap => check_cap(groups),
        InvariantId::DropEq => check_drop_eq(groups),
        InvariantId::Cadence => check_cadence(groups),
        InvariantId::ConsW => check_cons_w(expected, groups),
        InvariantId::ConsA => check_cons_a(expected, groups),
        InvariantId::Accel => check_accel(groups),
        InvariantId::Carry => check_carry(groups),
        InvariantId::Cfg => check_cfg(expected, groups),
        InvariantId::MuxNoOver => check_mux_no_over(expected, groups),
        InvariantId::SmoothCoast => check_smooth_coast(groups),
        InvariantId::NoDrop => check_no_drop(groups),
        InvariantId::Screen | InvariantId::Quiet => panic!(
            "{} is harness-side (needs PtyHarness screen/frame state); the A13 matrix \
             runner checks it — route by InvariantId::is_log_side",
            id.as_str()
        ),
    }
}

fn check_ord(groups: &[StreamGroup<'_>]) -> InvariantResult {
    let mut prev = f64::NEG_INFINITY;
    for rec in records(groups) {
        if rec.ts_ms < prev {
            return violated(format!(
                "ts_ms went backwards: {} after {prev} ({} record)",
                rec.ts_ms, rec.evt
            ));
        }
        prev = rec.ts_ms;
    }
    InvariantResult::Pass
}

fn check_cap(groups: &[StreamGroup<'_>]) -> InvariantResult {
    for rec in records(groups) {
        if rec.flushed.abs() > rec.cap {
            return violated(format!(
                "flushed {} exceeds cap {} at ts_ms={} (trigger {})",
                rec.flushed, rec.cap, rec.ts_ms, rec.trigger
            ));
        }
    }
    InvariantResult::Pass
}

fn check_drop_eq(groups: &[StreamGroup<'_>]) -> InvariantResult {
    for group in groups {
        let Some(fin) = group.finalize else { continue };
        match fin.dropped {
            Some(dropped) if dropped == fin.backlog_after => {}
            Some(dropped) => {
                return violated(format!(
                    "finalize at ts_ms={} has dropped={dropped} but backlog_after={}",
                    fin.ts_ms, fin.backlog_after
                ));
            }
            None => {
                return violated(format!("finalize at ts_ms={} lacks dropped", fin.ts_ms));
            }
        }
    }
    InvariantResult::Pass
}

fn check_cadence(groups: &[StreamGroup<'_>]) -> InvariantResult {
    // A stream's first flush-bearing record is skipped: its spacing is
    // global (measured from the previous stream — see
    // `intra_stream_flush_spacings_ms`).
    let floor = (REDRAW_CADENCE_MS - 1) as f64;
    for group in groups {
        for rec in group.flush_bearing().skip(1) {
            if rec.trigger == "promotion" || rec.trigger == "finalize" {
                continue;
            }
            if let Some(spacing) = rec.ms_since_prev_flush
                && spacing < floor
            {
                return violated(format!(
                    "{}ms flush spacing < {floor}ms at ts_ms={} (trigger {})",
                    spacing, rec.ts_ms, rec.trigger
                ));
            }
        }
    }
    InvariantResult::Pass
}

fn check_cons_w(expected: &ExpectedProfile, groups: &[StreamGroup<'_>]) -> InvariantResult {
    if expected.mode != "wheel" {
        return violated(format!(
            "I-CONS-W attached to a mode={} cell (needs forced wheel)",
            expected.mode
        ));
    }
    let rate = f64::from(expected.wheel_lpt) / f64::from(expected.ept.max(1));
    for group in groups {
        let Some(fin) = group.finalize else { continue };
        let raw = (fin.events_total as f64 * rate * f64::from(expected.speed)).trunc() as i64;
        let want = if fin.events_total > 0 {
            raw.max(MIN_LINES_PER_WHEEL_STREAM)
        } else {
            0
        };
        let got = delivered(fin).abs();
        if (got - want).abs() > 1 {
            return violated(format!(
                "wheel stream at ts_ms={}: delivered {got} lines for {} events, expected {want}±1",
                fin.ts_ms, fin.events_total
            ));
        }
    }
    InvariantResult::Pass
}

fn check_cons_a(expected: &ExpectedProfile, groups: &[StreamGroup<'_>]) -> InvariantResult {
    if expected.mode == "wheel" {
        return violated("I-CONS-A attached to a forced-wheel cell (use I-CONS-W)".into());
    }
    let wheel_rate = f64::from(expected.wheel_lpt) / f64::from(expected.ept.max(1));
    let tp_rate = f64::from(expected.trackpad_lpt) / 3.0;
    // Forced trackpad never prices via the wheel table.
    let (min_rate, max_rate) = if expected.mode == "trackpad" {
        (tp_rate, TRACKPAD_ACCEL_MAX * tp_rate)
    } else {
        (
            wheel_rate.min(tp_rate),
            wheel_rate.max(TRACKPAD_ACCEL_MAX * tp_rate),
        )
    };
    for group in groups {
        let Some(fin) = group.finalize else { continue };
        let events = fin.events_total as f64;
        let speed = f64::from(expected.speed);
        let lo = events * min_rate * speed - 1.0;
        let hi = events * max_rate * speed + 1.0;
        let desired = f64::from(fin.desired).abs();
        if desired < lo || desired > hi {
            return violated(format!(
                "stream at ts_ms={}: |desired|={desired:.2} outside [{lo:.2}, {hi:.2}] \
                 for {} events",
                fin.ts_ms, fin.events_total
            ));
        }
        let got = delivered(fin).abs() as f64;
        if got > hi {
            return violated(format!(
                "stream at ts_ms={}: delivered {got} lines > bound {hi:.2} for {} events",
                fin.ts_ms, fin.events_total
            ));
        }
    }
    InvariantResult::Pass
}

fn check_accel(groups: &[StreamGroup<'_>]) -> InvariantResult {
    for rec in records(groups) {
        let accel = f64::from(rec.accel);
        if !(1.0 - F32_TOLERANCE..=TRACKPAD_ACCEL_MAX + F32_TOLERANCE).contains(&accel) {
            return violated(format!(
                "accel {accel} outside [1.0, {TRACKPAD_ACCEL_MAX}] at ts_ms={}",
                rec.ts_ms
            ));
        }
        if let Some(avg) = rec.avg_interval_ms
            && avg < ACCEL_MIN_INTERVAL_MS - F32_TOLERANCE
        {
            return violated(format!(
                "avg_interval_ms {avg} < 6 at ts_ms={} — sub-6ms batching artifacts \
                 (ghostty dups) leaked into the interval window",
                rec.ts_ms
            ));
        }
    }
    InvariantResult::Pass
}

fn check_carry(groups: &[StreamGroup<'_>]) -> InvariantResult {
    for rec in records(groups) {
        if f64::from(rec.carry).abs() >= 1.0 {
            return violated(format!(
                "|carry| {} ≥ 1.0 at ts_ms={} — whole lines leaked across streams",
                rec.carry, rec.ts_ms
            ));
        }
    }
    for pair in groups.windows(2) {
        let Some(fin) = pair[0].finalize else {
            continue;
        };
        if fin.kind == "wheel" && f64::from(pair[1].start.carry).abs() > F32_TOLERANCE {
            return violated(format!(
                "stream_start at ts_ms={} carries {} after a wheel finalize (must be 0)",
                pair[1].start.ts_ms, pair[1].start.carry
            ));
        }
    }
    InvariantResult::Pass
}

fn check_cfg(expected: &ExpectedProfile, groups: &[StreamGroup<'_>]) -> InvariantResult {
    for group in groups {
        let start = group.start;
        let echo = (
            start.mode.as_deref(),
            start.ept,
            start.wheel_lpt,
            start.trackpad_lpt,
            start.invert,
        );
        let want = (
            Some(expected.mode),
            Some(expected.ept),
            Some(expected.wheel_lpt),
            Some(expected.trackpad_lpt),
            Some(expected.invert),
        );
        let speed_ok = start
            .speed
            .is_some_and(|s| (f64::from(s) - f64::from(expected.speed)).abs() <= F32_TOLERANCE);
        if echo != want || !speed_ok {
            return violated(format!(
                "config echo at ts_ms={} is {:?} speed={:?}, cell expects \
                 mode={} ept={} wheel_lpt={} trackpad_lpt={} invert={} speed={}",
                start.ts_ms,
                echo,
                start.speed,
                expected.mode,
                expected.ept,
                expected.wheel_lpt,
                expected.trackpad_lpt,
                expected.invert,
                expected.speed,
            ));
        }
    }
    InvariantResult::Pass
}

fn check_mux_no_over(expected: &ExpectedProfile, groups: &[StreamGroup<'_>]) -> InvariantResult {
    for group in groups {
        let Some(fin) = group.finalize else { continue };
        let bound = fin.events_total as f64 * f64::from(expected.speed) + 1.0;
        let got = delivered(fin).abs() as f64;
        if got > bound {
            return violated(format!(
                "remuxed stream at ts_ms={}: delivered {got} lines for {} events \
                 (> {bound:.2} — over-scroll on the conservative mux profile)",
                fin.ts_ms, fin.events_total
            ));
        }
    }
    InvariantResult::Pass
}

fn check_smooth_coast(groups: &[StreamGroup<'_>]) -> InvariantResult {
    for group in groups {
        let coast: i64 = group
            .flush_bearing()
            .filter(|rec| rec.events_since_flush == 0)
            .map(|rec| rec.flushed.abs())
            .sum();
        let cap = group
            .flush_bearing()
            .map(|rec| rec.cap)
            .max()
            .unwrap_or(i64::MAX);
        if coast > cap {
            return violated(format!(
                "stream at ts_ms={}: {coast} coast lines (flushes with no new events) \
                 > cap {cap} — post-input motion beyond one capped catch-up (the jerk)",
                group.start.ts_ms
            ));
        }
    }
    InvariantResult::Pass
}

fn check_no_drop(groups: &[StreamGroup<'_>]) -> InvariantResult {
    for group in groups {
        let Some(fin) = group.finalize else { continue };
        let dropped = fin.dropped.unwrap_or(0);
        if dropped != 0 {
            return violated(format!(
                "finalize at ts_ms={} dropped {dropped} lines",
                fin.ts_ms
            ));
        }
    }
    InvariantResult::Pass
}

#[cfg(test)]
mod tests {
    use super::super::log::{group_streams, parse_jsonl_str};
    use super::*;

    // ── JSONL fixture builders ─────────────────────────────────────────
    // Raw strings through parse_jsonl_str so every fixture also exercises
    // the wire schema (same stance as log.rs's producer-shaped constants).

    const C1: ExpectedProfile = ExpectedProfile {
        mode: "auto",
        ept: 3,
        wheel_lpt: 3,
        trackpad_lpt: 3,
        invert: false,
        speed: 1.0,
    };
    const C1_WHEEL: ExpectedProfile = ExpectedProfile {
        mode: "wheel",
        ..C1
    };

    // Floats are formatted with `{:?}` so whole values keep their decimal
    // point (`32.0`, not `32`) — the mutation `.replace()`s below match on
    // that spelling, and it mirrors serde_json's f64 output for round
    // numbers.
    fn start(ts: f64, carry: f32, mode: &str, speed: f32) -> String {
        format!(
            r#"{{"ts_ms":{ts:?},"evt":"stream_start","trigger":"event","kind":"unknown","events_total":0,"events_since_flush":0,"accel":1.0,"desired":0.0,"applied_total":0,"flushed":0,"backlog_after":0,"carry":{carry:?},"cap":25,"mode":"{mode}","ept":3,"wheel_lpt":3,"trackpad_lpt":3,"invert":false,"speed":{speed:?},"viewport_height":50}}"#
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn row(
        evt: &str,
        ts: f64,
        trigger: &str,
        kind: &str,
        events: (u64, u64),
        accel_avg: (f32, Option<f64>),
        lines: (f32, i64, i64, i64), // desired, applied_total, flushed, backlog_after
        msf: Option<f64>,
        dropped: Option<i64>,
    ) -> String {
        let avg = accel_avg
            .1
            .map_or(String::new(), |v| format!(r#""avg_interval_ms":{v:?},"#));
        let msf = msf.map_or(String::new(), |v| {
            format!(r#","ms_since_prev_flush":{v:?}"#)
        });
        let dropped = dropped.map_or(String::new(), |v| format!(r#","dropped":{v}"#));
        format!(
            r#"{{"ts_ms":{ts:?},"evt":"{evt}","trigger":"{trigger}","kind":"{kind}","events_total":{},"events_since_flush":{},{avg}"accel":{:?},"desired":{:?},"applied_total":{},"flushed":{},"backlog_after":{},"carry":0.0,"cap":25{msf}{dropped}}}"#,
            events.0, events.1, accel_avg.0, lines.0, lines.1, lines.2, lines.3,
        )
    }

    fn check(id: InvariantId, expected: &ExpectedProfile, jsonl: &[String]) -> InvariantResult {
        let raw = jsonl.join("\n");
        let records = parse_jsonl_str(&raw).expect("fixture must parse");
        let groups = group_streams(&records).expect("fixture must group");
        check_log_invariant(id, expected, &groups)
    }

    fn assert_violated(result: InvariantResult, needle: &str) {
        match result {
            InvariantResult::Violated { detail } => {
                assert!(
                    detail.contains(needle),
                    "detail {detail:?} lacks {needle:?}"
                );
            }
            InvariantResult::Pass => panic!("expected violation mentioning {needle:?}"),
        }
    }

    /// Canonical clean capture: a trackpad stream (sub-line carry out), a
    /// wheel stream (carry zeroed), a third stream echoing that zero.
    /// One record per line (rustfmt::skip) — reads like the JSONL it builds.
    #[rustfmt::skip]
    fn canonical() -> Vec<String> {
        vec![
            start(0.0, 0.0, "auto", 1.0),
            row("flush", 16.0, "event", "trackpad", (6, 6), (1.0, Some(10.0)), (6.4, 6, 6, 0), None, None),
            row("flush", 32.0, "tick", "trackpad", (9, 3), (1.0, Some(10.0)), (9.4, 9, 3, 0), Some(16.0), None),
            row("finalize", 120.0, "finalize", "trackpad", (9, 0), (1.0, Some(10.0)), (9.4, 9, 0, 0), Some(88.0), Some(0)),
            start(500.0, 0.4, "auto", 1.0),
            row("flush", 505.0, "promotion", "wheel", (3, 3), (1.0, None), (3.0, 3, 3, 0), Some(385.0), None),
            row("finalize", 600.0, "finalize", "wheel", (3, 0), (1.0, None), (3.0, 3, 0, 0), Some(95.0), Some(0)),
            start(700.0, 0.0, "auto", 1.0),
            row("flush", 716.0, "event", "unknown", (6, 6), (1.0, Some(8.0)), (6.2, 6, 6, 0), Some(116.0), None),
            row("finalize", 800.0, "finalize", "trackpad", (6, 0), (1.0, Some(8.0)), (6.2, 6, 0, 0), Some(84.0), Some(0)),
        ]
    }

    /// The real G4 signature: coast drain + finalize re-price burst with a
    /// drop — mid-stream priced as unknown (~1 line/event), re-priced ×2.5
    /// at the trackpad finalize, backlog beyond one capped flush discarded.
    #[rustfmt::skip]
    fn jerk() -> Vec<String> {
        vec![
            start(0.0, 0.0, "auto", 1.0),
            row("flush", 16.0, "event", "unknown", (40, 40), (2.5, Some(6.0)), (40.0, 25, 25, 15), None, None),
            row("flush", 32.0, "tick", "unknown", (66, 26), (2.5, Some(6.0)), (66.0, 50, 25, 16), Some(16.0), None),
            row("flush", 48.0, "tick", "unknown", (66, 0), (2.5, Some(6.0)), (66.0, 66, 16, 0), Some(16.0), None),
            row("finalize", 146.0, "finalize", "trackpad", (66, 0), (2.5, Some(6.0)), (165.0, 91, 25, 74), Some(98.0), Some(74)),
        ]
    }

    #[test]
    fn every_log_side_invariant_passes_on_the_canonical_capture() {
        // All log-side ids except I-CONS-W, which requires a forced-wheel
        // profile and has its own pass fixture.
        let fixture = canonical();
        for id in [
            InvariantId::Ord,
            InvariantId::Cap,
            InvariantId::DropEq,
            InvariantId::Cadence,
            InvariantId::ConsA,
            InvariantId::Accel,
            InvariantId::Carry,
            InvariantId::Cfg,
            InvariantId::MuxNoOver,
            InvariantId::SmoothCoast,
            InvariantId::NoDrop,
        ] {
            let result = check(id, &C1, &fixture);
            assert!(result.is_pass(), "{} on canonical: {result:?}", id.as_str());
        }
    }

    #[test]
    fn ord_rejects_backwards_timestamps() {
        let mut fixture = canonical();
        fixture[2] = fixture[2].replace(r#""ts_ms":32.0"#, r#""ts_ms":12.0"#);
        assert_violated(check(InvariantId::Ord, &C1, &fixture), "backwards");
    }

    /// A4 teleport: one flush delivering more than the per-flush cap.
    #[test]
    fn cap_rejects_over_cap_flush() {
        let mut fixture = canonical();
        fixture[1] = fixture[1].replace(r#""flushed":6"#, r#""flushed":40"#);
        assert_violated(check(InvariantId::Cap, &C1, &fixture), "exceeds cap");
    }

    #[test]
    fn drop_eq_rejects_mismatched_finalize_accounting() {
        let mut fixture = canonical();
        fixture[3] = fixture[3].replace(r#""backlog_after":0,"#, r#""backlog_after":9,"#);
        fixture[3] = fixture[3].replace(r#""dropped":0"#, r#""dropped":5"#);
        assert_violated(check(InvariantId::DropEq, &C1, &fixture), "dropped=5");
    }

    /// A2 busy-spin: an 8ms tick-flush spacing violates; the same spacing on
    /// a promotion-triggered record is skipped (promotion flushes bypass the
    /// cadence gate by design), as are finalize records (flip finalizes).
    #[test]
    fn cadence_rejects_sub_16ms_spacing_but_skips_promotion_and_finalize() {
        let mut fixture = canonical();
        fixture[2] = fixture[2].replace(
            r#""ms_since_prev_flush":16.0"#,
            r#""ms_since_prev_flush":8.0"#,
        );
        assert_violated(check(InvariantId::Cadence, &C1, &fixture), "8ms");

        let mut skipped = canonical();
        skipped[2] = skipped[2]
            .replace(r#""trigger":"tick""#, r#""trigger":"promotion""#)
            .replace(
                r#""ms_since_prev_flush":16.0"#,
                r#""ms_since_prev_flush":8.0"#,
            );
        // Flip-style finalize 8ms after the previous flush: also skipped.
        skipped[3] = skipped[3].replace(
            r#""ms_since_prev_flush":88.0"#,
            r#""ms_since_prev_flush":8.0"#,
        );
        assert!(check(InvariantId::Cadence, &C1, &skipped).is_pass());
    }

    /// A3 under-travel: a forced-wheel stream delivering two lines short.
    #[test]
    fn cons_w_exact_totals_with_min_lines_substitution() {
        // 6 events × (3/3) × 1.0 = 6 lines, delivered exactly; a second
        // 1-event stream prices to 1.0 → still ≥ the MIN_LINES floor.
        #[rustfmt::skip]
        let pass = vec![
            start(0.0, 0.0, "wheel", 1.0),
            row("flush", 16.0, "event", "wheel", (6, 6), (1.0, None), (6.0, 6, 6, 0), None, None),
            row("finalize", 100.0, "finalize", "wheel", (6, 0), (1.0, None), (6.0, 6, 0, 0), Some(84.0), Some(0)),
            start(300.0, 0.0, "wheel", 1.0),
            row("finalize", 400.0, "finalize", "wheel", (1, 1), (1.0, None), (1.0, 1, 1, 0), Some(300.0), Some(0)),
        ];
        assert!(check(InvariantId::ConsW, &C1_WHEEL, &pass).is_pass());

        let mut short = pass.clone();
        short[1] = short[1].replace(
            r#""applied_total":6,"flushed":6"#,
            r#""applied_total":4,"flushed":4"#,
        );
        short[2] = short[2].replace(r#""applied_total":6"#, r#""applied_total":4"#);
        assert_violated(check(InvariantId::ConsW, &C1_WHEEL, &short), "expected 6±1");

        // Attached to a non-wheel cell = cell-table bug, not a pass.
        assert_violated(check(InvariantId::ConsW, &C1, &pass), "needs forced wheel");
    }

    #[test]
    fn cons_a_bounds_auto_totals() {
        // 3 events on the C1 profile can never desire 100 lines
        // (hi = 3 × max(1, 3×1) × 1 + 1 = 10).
        let mut fixture = canonical();
        fixture[6] = fixture[6].replace(r#""desired":3.0"#, r#""desired":100.0"#);
        assert_violated(check(InvariantId::ConsA, &C1, &fixture), "outside");
        assert_violated(
            check(InvariantId::ConsA, &C1_WHEEL, &canonical()),
            "use I-CONS-W",
        );
    }

    /// G5 clause: a sub-6ms average means ghostty-style duplicate reports
    /// leaked into the interval window; out-of-range accel is the same
    /// regression on the multiplier side.
    #[test]
    fn accel_rejects_out_of_band_multiplier_and_sub_6ms_average() {
        let mut fixture = canonical();
        fixture[1] = fixture[1].replace(r#""accel":1.0"#, r#""accel":4.5"#);
        assert_violated(check(InvariantId::Accel, &C1, &fixture), "accel 4.5");

        let mut dup = canonical();
        dup[1] = dup[1].replace(r#""avg_interval_ms":10.0"#, r#""avg_interval_ms":3.0"#);
        assert_violated(check(InvariantId::Accel, &C1, &dup), "interval window");
    }

    #[test]
    fn carry_rejects_whole_line_carry_and_nonzero_start_after_wheel_finalize() {
        let mut fixture = canonical();
        fixture[4] = start(500.0, 1.4, "auto", 1.0);
        assert_violated(check(InvariantId::Carry, &C1, &fixture), "≥ 1.0");

        // Wheel finalize (stream 2) must zero the carry into stream 3.
        let mut leak = canonical();
        leak[7] = start(700.0, 0.4, "auto", 1.0);
        assert_violated(check(InvariantId::Carry, &C1, &leak), "wheel finalize");
    }

    #[test]
    fn cfg_rejects_echo_profile_mismatch() {
        assert!(check(InvariantId::Cfg, &C1, &canonical()).is_pass());
        let mut fixture = canonical();
        fixture[0] = start(0.0, 0.0, "auto", 6.0); // speed echo ≠ expected 1.0
        assert_violated(check(InvariantId::Cfg, &C1, &fixture), "speed");
        let expected_wheel = ExpectedProfile {
            mode: "wheel",
            ..C1
        };
        assert_violated(
            check(InvariantId::Cfg, &expected_wheel, &canonical()),
            "mode",
        );
    }

    /// A4-class over-scroll on the conservative remuxed profile: more
    /// delivered lines than events at speed 1.0.
    #[test]
    fn mux_no_over_rejects_over_delivery() {
        let mut fixture = canonical();
        fixture[3] = fixture[3].replace(r#""applied_total":9"#, r#""applied_total":20"#);
        // Keep DropEq-independent: only this invariant is under test.
        assert_violated(check(InvariantId::MuxNoOver, &C1, &fixture), "over-scroll");
    }

    /// The jerk fixture violates exactly the two xfail invariants of
    /// `c1_auto_g4_jerk_xfail` — and nothing else in the core suite, which
    /// is what confines the expected failure to those rows.
    #[test]
    fn jerk_shape_violates_smooth_coast_and_no_drop_only() {
        let fixture = jerk();
        assert_violated(check(InvariantId::SmoothCoast, &C1, &fixture), "coast");
        assert_violated(check(InvariantId::NoDrop, &C1, &fixture), "dropped 74");
        for id in [
            InvariantId::Ord,
            InvariantId::Cap,
            InvariantId::DropEq,
            InvariantId::Cadence,
            InvariantId::ConsA,
            InvariantId::Accel,
            InvariantId::Carry,
            InvariantId::Cfg,
        ] {
            let result = check(id, &C1, &fixture);
            assert!(result.is_pass(), "{} on jerk: {result:?}", id.as_str());
        }
    }

    /// One capped catch-up flush after input stops is legitimate (that is
    /// the finalize contract) — coast ≤ cap passes.
    #[test]
    fn smooth_coast_allows_a_single_capped_catchup() {
        #[rustfmt::skip]
        let fixture = vec![
            start(0.0, 0.0, "auto", 1.0),
            row("flush", 16.0, "event", "unknown", (30, 30), (1.0, Some(6.0)), (30.0, 25, 25, 5), None, None),
            row("finalize", 100.0, "finalize", "trackpad", (30, 0), (1.0, Some(6.0)), (30.0, 30, 5, 0), Some(84.0), Some(0)),
        ];
        assert!(check(InvariantId::SmoothCoast, &C1, &fixture).is_pass());
        assert!(check(InvariantId::NoDrop, &C1, &fixture).is_pass());
    }

    #[test]
    #[should_panic(expected = "harness-side")]
    fn harness_side_ids_panic_in_the_log_checker() {
        let _ = check_log_invariant(InvariantId::Screen, &C1, &[]);
    }

    #[test]
    fn log_side_partition_matches_the_a13_split() {
        for id in [InvariantId::Screen, InvariantId::Quiet] {
            assert!(!id.is_log_side(), "{}", id.as_str());
        }
        for id in [
            InvariantId::Ord,
            InvariantId::Cap,
            InvariantId::DropEq,
            InvariantId::Cadence,
            InvariantId::ConsW,
            InvariantId::ConsA,
            InvariantId::Accel,
            InvariantId::Carry,
            InvariantId::Cfg,
            InvariantId::MuxNoOver,
            InvariantId::SmoothCoast,
            InvariantId::NoDrop,
        ] {
            assert!(id.is_log_side(), "{}", id.as_str());
        }
    }
}
