//! The cell table: terminal-class × config × gesture rows the matrix runs.
//!
//! Classes are `ScrollConfig` equivalence classes, not brands — every brand
//! sharing a profile is represented once (`from_terminal_context` in the
//! pager's `mouse.rs` is the source of truth):
//!
//! | class | env                        | ept | wheel_lpt | trackpad_lpt |
//! |-------|----------------------------|-----|-----------|--------------|
//! | C1    | none (harness strips)      | 3   | 3         | 3            |
//! | C2    | `TERM_PROGRAM=iTerm.app`   | 1   | 1         | 3            |
//! | C3    | `TERM_PROGRAM=zed`         | 1   | 3         | 3            |
//! | C4    | `TERM_PROGRAM=vscode`      | 1   | 3         | 15           |
//! | C5    | `TMUX=…` (remuxed)         | 1   | 1         | 3            |
//!
//! Mux honesty: C5's env only exercises profile *selection* (the pager
//! can't tell a fake `TMUX` from a real one); real tmux event-mangling is
//! simulated by the G9 gesture shapes, and a real-tmux tier stays local.
//!
//! Trim note: the design sketched ~40 rows; this table ships the curated 8
//! plus 17 representative full-tier rows (every class, every gesture, every
//! config knob at least once) to hold the A12 size budget — growing the
//! full tier is additive row work.

use super::gestures::GestureId;
use super::invariants::InvariantId;
use super::session::SessionKind;

/// The config echo a cell expects on every `stream_start` (I-CFG) and the
/// pricing inputs the consistency invariants use.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExpectedProfile {
    /// `mode` label: `auto` | `wheel` | `trackpad`.
    pub mode: &'static str,
    pub ept: u16,
    pub wheel_lpt: u16,
    pub trackpad_lpt: u16,
    pub invert: bool,
    /// Speed multiplier (NOT the 1-100 setting): `ECHO_AGENT_SCROLL_SPEED=100`
    /// echoes 6.0 via the pager's `speed_to_multiplier`.
    pub speed: f32,
}

/// Execution tier: `Curated` runs in CI (A13's runner test); `Full` only in
/// the local full sweep.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Curated,
    Full,
}

/// One matrix cell: environment in, gesture replayed, invariants judged.
#[derive(Clone, Copy, Debug)]
pub struct MatrixCell {
    pub id: &'static str,
    pub tier: Tier,
    /// Pager env pairs (terminal-class markers + config vars). The runner
    /// appends `ECHO_AGENT_SCROLL_LOG`; the harness's env strips guarantee the
    /// host terminal can't leak competing markers underneath these.
    pub env: &'static [(&'static str, &'static str)],
    pub expected: ExpectedProfile,
    pub gesture: GestureId,
    pub session: SessionKind,
    /// Invariants judged for this cell (harness-side ids included; the
    /// runner routes by `InvariantId::is_log_side`).
    pub invariants: &'static [InvariantId],
    /// Invariants expected to VIOLATE on current code (known bugs, e.g. the
    /// G4 jerk until A13's finalize-decel fix). Must be ⊆ `invariants`; the
    /// runner fails a cell on any non-xfail violation AND on an xfail PASS
    /// (a fixed bug must be promoted out of xfail, not silently absorbed).
    pub xfail: &'static [InvariantId],
}

const C1: ExpectedProfile = ExpectedProfile {
    mode: "auto",
    ept: 3,
    wheel_lpt: 3,
    trackpad_lpt: 3,
    invert: false,
    speed: 1.0,
};
const C2: ExpectedProfile = ExpectedProfile {
    ept: 1,
    wheel_lpt: 1,
    ..C1
};
const C3: ExpectedProfile = ExpectedProfile { ept: 1, ..C1 };
const C4: ExpectedProfile = ExpectedProfile {
    ept: 1,
    trackpad_lpt: 15,
    ..C1
};
/// Remuxed conservative profile — identical numbers to C2 by design
/// (`multiplexer_reencodes_mouse` forces ept=1/wheel_lpt=1).
const C5: ExpectedProfile = C2;

const ITERM: (&str, &str) = ("TERM_PROGRAM", "iTerm.app");
const ZED: (&str, &str) = ("TERM_PROGRAM", "zed");
const VSCODE: (&str, &str) = ("TERM_PROGRAM", "vscode");
const TMUX: (&str, &str) = ("TMUX", "/tmp/tmux-0/default,1,0");
const SPEED100: (&str, &str) = ("ECHO_AGENT_SCROLL_SPEED", "100");
const MODE_WHEEL: (&str, &str) = ("ECHO_AGENT_SCROLL_MODE", "wheel");
const MODE_TRACKPAD: (&str, &str) = ("ECHO_AGENT_SCROLL_MODE", "trackpad");
const LINES1: (&str, &str) = ("ECHO_AGENT_SCROLL_LINES", "1");
const INVERT: (&str, &str) = ("ECHO_AGENT_INVERT_SCROLL", "1");

use InvariantId::*;

/// Core log-side suite for auto-mode cells.
const AUTO: &[InvariantId] = &[Ord, Cap, DropEq, Cadence, ConsA, Accel, Carry, Cfg];
const AUTO_NODROP: &[InvariantId] = &[Ord, Cap, DropEq, Cadence, ConsA, Accel, Carry, Cfg, NoDrop];
/// Floods keep the core suite (drops are legitimate) + post-gesture quiet.
const AUTO_QUIET: &[InvariantId] = &[Ord, Cap, DropEq, Cadence, ConsA, Accel, Carry, Cfg, Quiet];
/// Forced wheel: exact totals; nothing may drop on notch-scale gestures.
const WHEEL: &[InvariantId] = &[Ord, Cap, DropEq, Cadence, ConsW, Accel, Carry, Cfg, NoDrop];
const WHEEL_MUX: &[InvariantId] = &[
    Ord, Cap, DropEq, Cadence, ConsW, Accel, Carry, Cfg, NoDrop, MuxNoOver,
];
const AUTO_MUX_NODROP: &[InvariantId] = &[
    Ord, Cap, DropEq, Cadence, ConsA, Accel, Carry, Cfg, NoDrop, MuxNoOver,
];
const AUTO_SCREEN: &[InvariantId] = &[Ord, Cap, DropEq, Cadence, ConsA, Accel, Carry, Cfg, Screen];
/// The jerk suite: core + the two smoothness invariants the finalize-decel
/// fix made hold (formerly this cell's xfail set).
const JERK: &[InvariantId] = &[
    Ord,
    Cap,
    DropEq,
    Cadence,
    ConsA,
    Accel,
    Carry,
    Cfg,
    SmoothCoast,
    NoDrop,
];

const fn cell(
    id: &'static str,
    tier: Tier,
    env: &'static [(&'static str, &'static str)],
    expected: ExpectedProfile,
    gesture: GestureId,
    session: SessionKind,
    invariants: &'static [InvariantId],
) -> MatrixCell {
    MatrixCell {
        id,
        tier,
        env,
        expected,
        gesture,
        session,
        invariants,
        xfail: &[],
    }
}

/// The matrix. Ids are `<class>_<config>_<gesture>[_qualifier]`.
#[rustfmt::skip]
pub const CELLS: &[MatrixCell] = &[
    // ── Curated (CI tier, 8 cells) ─────────────────────────────────────
    cell("c1_auto_g3_flood_speed100", Tier::Curated, &[SPEED100],
        ExpectedProfile { speed: 6.0, ..C1 }, GestureId::G3Flood, SessionKind::Settled, AUTO_QUIET),
    cell("c2_auto_g3_flood_speed100", Tier::Curated, &[ITERM, SPEED100],
        ExpectedProfile { speed: 6.0, ..C2 }, GestureId::G3Flood, SessionKind::Settled, AUTO_QUIET),
    cell("c3_wheel_lines1_g1", Tier::Curated, &[ZED, MODE_WHEEL, LINES1],
        ExpectedProfile { mode: "wheel", wheel_lpt: 1, trackpad_lpt: 1, ..C3 },
        GestureId::G1Notch, SessionKind::Settled, WHEEL),
    cell("c4_auto_g10_ambiguous", Tier::Curated, &[VSCODE],
        C4, GestureId::G10AmbiguousSlow, SessionKind::Settled, AUTO_NODROP),
    cell("c5_tmux_g9a", Tier::Curated, &[TMUX],
        C5, GestureId::G9aMuxSingles, SessionKind::Settled, AUTO_MUX_NODROP),
    cell("c5_tmux_g9b", Tier::Curated, &[TMUX],
        C5, GestureId::G9bMuxBatch, SessionKind::Settled, AUTO_MUX_NODROP),
    cell("c1_auto_g8_midstream", Tier::Curated, &[],
        C1, GestureId::G8MidStreamTrain, SessionKind::Streaming, AUTO),
    // Id kept for artifact/test continuity: the cell pinned the G4 jerk as
    // xfail until the finalize-decel fix; its former xfail rows
    // (I-SMOOTH-COAST, I-NO-DROP) are ordinary pass rows now.
    cell("c1_auto_g4_jerk_xfail", Tier::Curated, &[],
        C1, GestureId::G4Jerk, SessionKind::Settled, JERK),
    // ── Full tier (local sweep; representative subset — see trim note) ─
    cell("c1_auto_g1", Tier::Full, &[], C1,
        GestureId::G1Notch, SessionKind::Settled, AUTO_NODROP),
    cell("c1_auto_g2", Tier::Full, &[], C1,
        GestureId::G2NotchTrain, SessionKind::Settled, AUTO_NODROP),
    cell("c1_auto_g5_ghostty_dup", Tier::Full, &[], C1,
        GestureId::G5GhosttyDup, SessionKind::Settled, AUTO_NODROP),
    cell("c1_auto_g6_flip", Tier::Full, &[], C1,
        GestureId::G6Flip, SessionKind::Settled, AUTO),
    cell("c1_auto_g7_overscroll", Tier::Full, &[], C1,
        GestureId::G7Overscroll, SessionKind::BottomPinned, AUTO_SCREEN),
    cell("c1_wheel_g2", Tier::Full, &[MODE_WHEEL], ExpectedProfile { mode: "wheel", ..C1 },
        GestureId::G2NotchTrain, SessionKind::Settled, WHEEL),
    cell("c1_trackpad_g3_flood", Tier::Full, &[MODE_TRACKPAD],
        ExpectedProfile { mode: "trackpad", ..C1 },
        GestureId::G3Flood, SessionKind::Settled, AUTO_QUIET),
    cell("c1_auto_g11_carry", Tier::Full, &[], C1,
        GestureId::G11Carry, SessionKind::Settled, AUTO_NODROP),
    cell("c1_invert_g1", Tier::Full, &[INVERT], ExpectedProfile { invert: true, ..C1 },
        GestureId::G1Notch, SessionKind::Settled, AUTO_NODROP),
    cell("c2_auto_g1", Tier::Full, &[ITERM], C2,
        GestureId::G1Notch, SessionKind::Settled, AUTO_NODROP),
    cell("c2_wheel_g1", Tier::Full, &[ITERM, MODE_WHEEL],
        ExpectedProfile { mode: "wheel", ..C2 },
        GestureId::G1Notch, SessionKind::Settled, WHEEL),
    cell("c3_auto_g1", Tier::Full, &[ZED], C3,
        GestureId::G1Notch, SessionKind::Settled, AUTO_NODROP),
    cell("c3_auto_g10_repricing", Tier::Full, &[ZED], C3,
        GestureId::G10AmbiguousSlow, SessionKind::Settled, AUTO_NODROP),
    cell("c4_auto_g3_flood", Tier::Full, &[VSCODE], C4,
        GestureId::G3Flood, SessionKind::Settled, AUTO_QUIET),
    cell("c4_lines1_g10", Tier::Full, &[VSCODE, LINES1],
        ExpectedProfile { wheel_lpt: 1, trackpad_lpt: 1, ..C4 },
        GestureId::G10AmbiguousSlow, SessionKind::Settled, AUTO_NODROP),
    cell("c5_wheel_g9b", Tier::Full, &[TMUX, MODE_WHEEL],
        ExpectedProfile { mode: "wheel", ..C5 },
        GestureId::G9bMuxBatch, SessionKind::Settled, WHEEL_MUX),
    cell("c5_auto_g6_flip", Tier::Full, &[TMUX], C5,
        GestureId::G6Flip, SessionKind::Settled, AUTO),
];

/// The CI subset.
pub fn curated() -> impl Iterator<Item = &'static MatrixCell> {
    CELLS.iter().filter(|c| c.tier == Tier::Curated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Re-derive the expected profile from a cell's env pairs the way the
    /// pager would (`from_terminal_context` + the `ECHO_AGENT_SCROLL_*` env
    /// overrides) — a tripwire against rows drifting from `mouse.rs`.
    fn derive_expected(env: &'static [(&'static str, &'static str)]) -> ExpectedProfile {
        let get = |k: &str| env.iter().find(|(key, _)| *key == k).map(|(_, v)| *v);
        let remuxed = get("TMUX").is_some();
        let (ept, mut wheel_lpt, mut trackpad_lpt) = if remuxed {
            (1, 1, 3)
        } else {
            match get("TERM_PROGRAM") {
                Some("iTerm.app") => (1, 1, 3),
                Some("zed") => (1, 3, 3),
                Some("vscode") => (1, 3, 15),
                None => (3, 3, 3),
                Some(other) => panic!("unmapped TERM_PROGRAM {other:?}"),
            }
        };
        if let Some(lines) = get("ECHO_AGENT_SCROLL_LINES") {
            let lines: u16 = lines.parse().unwrap();
            wheel_lpt = lines;
            trackpad_lpt = lines; // one knob overrides both paths
        }
        // speed_to_multiplier re-derivation for the settings used in rows.
        let speed = match get("ECHO_AGENT_SCROLL_SPEED") {
            None | Some("50") => 1.0,
            Some("100") => 6.0,
            Some(other) => panic!("unmapped ECHO_AGENT_SCROLL_SPEED {other:?}"),
        };
        ExpectedProfile {
            mode: get("ECHO_AGENT_SCROLL_MODE").unwrap_or("auto"),
            ept,
            wheel_lpt,
            trackpad_lpt,
            invert: get("ECHO_AGENT_INVERT_SCROLL") == Some("1"),
            speed,
        }
    }

    #[test]
    fn ids_are_unique() {
        let mut seen = HashSet::new();
        for cell in CELLS {
            assert!(seen.insert(cell.id), "duplicate cell id {}", cell.id);
        }
    }

    #[test]
    fn curated_tier_is_the_designed_eight() {
        let ids: Vec<&str> = curated().map(|c| c.id).collect();
        assert_eq!(
            ids,
            [
                "c1_auto_g3_flood_speed100",
                "c2_auto_g3_flood_speed100",
                "c3_wheel_lines1_g1",
                "c4_auto_g10_ambiguous",
                "c5_tmux_g9a",
                "c5_tmux_g9b",
                "c1_auto_g8_midstream",
                "c1_auto_g4_jerk_xfail",
            ]
        );
    }

    #[test]
    fn xfail_is_a_subset_of_the_cell_invariants() {
        for cell in CELLS {
            for id in cell.xfail {
                assert!(
                    cell.invariants.contains(id),
                    "{}: xfail {} not in the invariant list",
                    cell.id,
                    id.as_str()
                );
            }
        }
        // The finalize-decel fix promoted the jerk cell's xfail rows
        // (I-SMOOTH-COAST, I-NO-DROP) into ordinary pass rows: the table
        // must carry no xfail anywhere until the next pinned bug.
        let jerk = CELLS
            .iter()
            .find(|c| c.id == "c1_auto_g4_jerk_xfail")
            .unwrap();
        assert!(
            jerk.xfail.is_empty(),
            "the jerk is fixed; xfail must stay empty"
        );
        assert!(
            [InvariantId::SmoothCoast, InvariantId::NoDrop]
                .iter()
                .all(|id| jerk.invariants.contains(id)),
            "the promoted invariants must remain in the pass set"
        );
    }

    #[test]
    fn expected_profiles_agree_with_env() {
        for cell in CELLS {
            assert_eq!(
                cell.expected,
                derive_expected(cell.env),
                "{}: expected profile drifted from its env",
                cell.id
            );
        }
    }

    #[test]
    fn invariant_lists_are_coherent() {
        for cell in CELLS {
            let mut seen = HashSet::new();
            for id in cell.invariants {
                assert!(seen.insert(id), "{}: duplicate {}", cell.id, id.as_str());
            }
            // Consistency invariants match the forced mode.
            assert_eq!(
                cell.invariants.contains(&InvariantId::ConsW),
                cell.expected.mode == "wheel",
                "{}: I-CONS-W ⇔ forced wheel",
                cell.id
            );
            assert_eq!(
                cell.invariants.contains(&InvariantId::ConsA),
                cell.expected.mode != "wheel",
                "{}: I-CONS-A ⇔ not forced wheel",
                cell.id
            );
            // Mux over-scroll bound only makes sense on the remuxed class,
            // and only on its accel-free G9 gestures.
            if cell.invariants.contains(&InvariantId::MuxNoOver) {
                assert!(
                    matches!(
                        cell.gesture,
                        GestureId::G9aMuxSingles | GestureId::G9bMuxBatch
                    ) && cell.env.iter().any(|(k, _)| *k == "TMUX"),
                    "{}: I-MUX-NO-OVER outside the mux/G9 envelope",
                    cell.id
                );
            }
        }
    }

    #[test]
    fn sessions_and_gestures_pair_correctly() {
        for cell in CELLS {
            // TMUX ⇒ the conservative remuxed profile.
            if cell.env.iter().any(|(k, _)| *k == "TMUX") {
                assert_eq!(
                    (cell.expected.ept, cell.expected.wheel_lpt),
                    (1, 1),
                    "{}",
                    cell.id
                );
            }
            // Streaming sessions exist exactly for the mid-stream gesture;
            // the bottom-pin matters exactly for the overscroll gesture.
            assert_eq!(
                cell.session == SessionKind::Streaming,
                cell.gesture == GestureId::G8MidStreamTrain,
                "{}",
                cell.id
            );
            assert_eq!(
                cell.session == SessionKind::BottomPinned,
                cell.gesture == GestureId::G7Overscroll,
                "{}",
                cell.id
            );
        }
    }
}
