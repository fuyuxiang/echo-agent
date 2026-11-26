//! Terminal-mode tracking and restore emission for `echoagent wrap`.
//!
//! `echoagent wrap` cannot tell a clean child exit from a connection drop: an ssh
//! transport death reaches it as a plain PTY EOF plus an exit code. What it
//! *can* know is which DEC private modes the child enabled on the local
//! terminal and never disabled — reset bytes that died with the link.
//! [`ModeTracker`] observes every complete CSI sequence the wrap output
//! filter forwards and keeps a bitmask of latched modes plus the kitty
//! keyboard push depth; [`restore_bytes`] emits disables for exactly that
//! latched state. Dirty deaths get repaired, clean exits stay byte-for-byte
//! transparent, and kitty pops are exactly as deep as the child's net pushes
//! (a blind pop could corrupt an enclosing context's keyboard stack).
//!
//! The tracked set mirrors the canonical teardown table in
//! [`echo_agent_crash_handler::terminal`] (`RESTORE_SEQ`) — pinned mechanically by a
//! unit test below — plus the remaining mouse encodings (`?1005`, `?1016`)
//! and the legacy alternate screens (`?47`, `?1047`) that a wrapped TUI may
//! use.
//!
//! Known limitation: the kitty protocol keeps an independent stack per screen
//! buffer, while this tracker keeps one net-depth counter. A child that
//! pushes on one screen and dies while the other is active is therefore
//! restored by count, not by screen — the same limitation as the pager crash
//! handler's teardown; tracking a depth per screen would close that gap.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Normal mouse tracking (X11 press/release), `?1000`.
const MOUSE_1000: u32 = 1 << 0;
/// Button-event mouse tracking (cell motion while held), `?1002`.
const MOUSE_1002: u32 = 1 << 1;
/// All-motion mouse tracking (any movement), `?1003`.
const MOUSE_1003: u32 = 1 << 2;
/// UTF-8 extended mouse coordinates, `?1005`.
const MOUSE_1005: u32 = 1 << 3;
/// RXVT extended mouse reporting (coords > 223), `?1015`.
const MOUSE_1015: u32 = 1 << 4;
/// SGR-pixel extended mouse reporting, `?1016`.
const MOUSE_1016: u32 = 1 << 5;
/// SGR extended mouse reporting format, `?1006`.
const MOUSE_1006: u32 = 1 << 6;
/// Bracketed paste mode, `?2004`.
const PASTE_2004: u32 = 1 << 7;
/// Focus reporting (focus in/out events), `?1004`.
const FOCUS_1004: u32 = 1 << 8;
/// Synchronized update, `?2026`.
const SYNC_2026: u32 = 1 << 9;
/// Legacy alternate screen buffer, `?47`.
const ALT_47: u32 = 1 << 10;
/// Alternate screen buffer without cursor save, `?1047`.
const ALT_1047: u32 = 1 << 11;
/// Alternate screen buffer with cursor save/restore, `?1049`.
const ALT_1049: u32 = 1 << 12;
/// Cursor hidden — mode `?25` tracked INVERTED: DECTCEM's set side (`?25h`)
/// shows the cursor, so the latched (needs-repair) state is having seen
/// `?25l` without a later `?25h`.
const CURSOR_HIDDEN: u32 = 1 << 13;

/// Mouse/paste/focus disables in the relative order pinned by
/// `echo_agent_crash_handler::terminal::RESTORE_SEQ`'s ordering tests.
const DISABLE_ORDER: &[(u32, &[u8])] = &[
    (MOUSE_1000, b"\x1b[?1000l"),
    (MOUSE_1002, b"\x1b[?1002l"),
    (MOUSE_1003, b"\x1b[?1003l"),
    (MOUSE_1005, b"\x1b[?1005l"),
    (MOUSE_1015, b"\x1b[?1015l"),
    (MOUSE_1016, b"\x1b[?1016l"),
    (MOUSE_1006, b"\x1b[?1006l"),
    (PASTE_2004, b"\x1b[?2004l"),
    (FOCUS_1004, b"\x1b[?1004l"),
];

/// Bit for a tracked DECSET/DECRST parameter; `None` for untracked modes.
fn mode_bit(mode: u32) -> Option<u32> {
    Some(match mode {
        25 => CURSOR_HIDDEN,
        47 => ALT_47,
        1000 => MOUSE_1000,
        1002 => MOUSE_1002,
        1003 => MOUSE_1003,
        1004 => FOCUS_1004,
        1005 => MOUSE_1005,
        1006 => MOUSE_1006,
        1015 => MOUSE_1015,
        1016 => MOUSE_1016,
        1047 => ALT_1047,
        1049 => ALT_1049,
        2004 => PASTE_2004,
        2026 => SYNC_2026,
        _ => return None,
    })
}

/// Latched-terminal-state tracker shared (via `Arc`) between the wrap output
/// filter, the exit-path drop guard, and the terminate-signal thread.
///
/// All state is atomic: the read loop updates it while other threads snapshot
/// it, and the two-phase `restore_claimed`/`restore_done` gate keeps the
/// multiple exit paths from emitting restores twice while still letting a
/// losing path wait for the winner to finish. `SeqCst` throughout: every
/// access is on a cold path (a few RMWs per tracked mode change, none per
/// output byte), so the uniform strongest ordering is chosen over reasoning
/// about minimal per-site orderings.
#[derive(Debug, Default)]
pub(crate) struct ModeTracker {
    /// Bitmask of latched modes (the `MOUSE_*`/`PASTE_*`/... bits above).
    modes: AtomicU32,
    /// Net kitty keyboard protocol pushes (`CSI > .. u`) minus pops
    /// (`CSI < .. u`), floored at zero.
    kitty_depth: AtomicU32,
    /// Claim phase of the one-shot restore gate: set by the first exit path
    /// that starts the restore.
    restore_claimed: AtomicBool,
    /// Completion phase: set by the claim winner once the restore has been
    /// fully emitted, so losing exit paths know it is safe to let the
    /// process exit.
    restore_done: AtomicBool,
}

/// Point-in-time copy of the latched state, safe to take from any thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ModeSnapshot {
    modes: u32,
    kitty_depth: u32,
}

impl ModeTracker {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Update state from one complete CSI sequence (`ESC [ .. final`),
    /// exactly as forwarded to the terminal.
    ///
    /// Only child→terminal output flows here, so mouse *reports* (which a
    /// real terminal sends the other way, on stdin) never reach this parser.
    pub(crate) fn observe_csi(&self, seq: &[u8]) {
        if seq.len() < 3 {
            return;
        }
        let final_byte = seq[seq.len() - 1];
        let body = &seq[2..seq.len() - 1];
        match final_byte {
            // DECSET/DECRST only; ANSI SM/RM (no `?`) is untracked.
            b'h' | b'l' => {
                let Some(params) = body.strip_prefix(b"?") else {
                    return;
                };
                let set = final_byte == b'h';
                for param in params.split(|&b| b == b';') {
                    if let Some(mode) = parse_decimal(param) {
                        self.apply_dec_mode(mode, set);
                    }
                }
            }
            b'u' => match body.first() {
                // Kitty keyboard push: `CSI > flags u`.
                Some(b'>') => {
                    self.kitty_depth.fetch_add(1, Ordering::SeqCst);
                }
                // Kitty keyboard pop: `CSI < n u`, n defaulting to 1. The
                // depth floors at zero so a child popping an entry it never
                // pushed cannot make wrap pop one on its behalf later.
                Some(b'<') => {
                    // Zero also means the default (1): under the common CSI
                    // zero-means-default convention a terminal may pop one
                    // entry for `<0u`, and over-counting depth here risks the
                    // destructive extra pop at exit.
                    let n = parse_decimal(&body[1..]).filter(|&n| n > 0).unwrap_or(1);
                    let _ = self.kitty_depth.fetch_update(
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                        |depth| Some(depth.saturating_sub(n)),
                    );
                }
                // `CSI u` restores the cursor, `CSI ? u` queries, and
                // `CSI = .. u` sets flags without pushing — none are stack
                // operations.
                _ => {}
            },
            _ => {}
        }
    }

    fn apply_dec_mode(&self, mode: u32, set: bool) {
        let Some(bit) = mode_bit(mode) else {
            return;
        };
        // Mode 25 is show-cursor: its latched (needs-repair) side is `l`.
        let latch = if mode == 25 { !set } else { set };
        if latch {
            self.modes.fetch_or(bit, Ordering::SeqCst);
        } else {
            self.modes.fetch_and(!bit, Ordering::SeqCst);
        }
    }

    pub(crate) fn snapshot(&self) -> ModeSnapshot {
        ModeSnapshot {
            modes: self.modes.load(Ordering::SeqCst),
            kitty_depth: self.kitty_depth.load(Ordering::SeqCst),
        }
    }

    /// Claim the one-shot restore shared by every exit path (drop guard,
    /// signal thread): the first caller gets `true` and must call
    /// [`finish_restore`](Self::finish_restore) when done; later callers get
    /// `false` and must not emit (the terminal would be reset twice — the
    /// kitty pop is a destructive stack operation) but should wait for
    /// completion before letting the process exit.
    pub(crate) fn begin_restore(&self) -> bool {
        !self.restore_claimed.swap(true, Ordering::SeqCst)
    }

    /// Mark the claimed restore as fully emitted.
    pub(crate) fn finish_restore(&self) {
        self.restore_done.store(true, Ordering::SeqCst);
    }

    /// Whether a claimed restore has completed.
    pub(crate) fn restore_done(&self) -> bool {
        self.restore_done.load(Ordering::SeqCst)
    }
}

/// Disable sequences for exactly the latched state in `snapshot`.
///
/// Nothing latched yields an empty vec — clean exits must stay
/// byte-transparent. The emission order matches
/// `echo_agent_crash_handler::terminal::RESTORE_SEQ` for every element the two
/// share (pinned by a unit test below): synchronized-update end first
/// (multiplexers must stop buffering before the other resets arrive), cursor
/// show, mouse/paste/focus disables, kitty pops before the alt-screen exits
/// (the kitty stack is per-screen), alt-screen exits last.
pub(crate) fn restore_bytes(snapshot: ModeSnapshot) -> Vec<u8> {
    let mut out = Vec::new();
    if snapshot.modes & SYNC_2026 != 0 {
        out.extend_from_slice(b"\x1b[?2026l");
    }
    if snapshot.modes & CURSOR_HIDDEN != 0 {
        out.extend_from_slice(b"\x1b[?25h");
    }
    for &(bit, seq) in DISABLE_ORDER {
        if snapshot.modes & bit != 0 {
            out.extend_from_slice(seq);
        }
    }
    // One pop per net push: unwinds the child's stack entries exactly and
    // leaves any enclosing context's entries alone.
    for _ in 0..snapshot.kitty_depth {
        out.extend_from_slice(b"\x1b[<u");
    }
    if snapshot.modes & ALT_1047 != 0 {
        out.extend_from_slice(b"\x1b[?1047l");
    }
    if snapshot.modes & ALT_47 != 0 {
        out.extend_from_slice(b"\x1b[?47l");
    }
    if snapshot.modes & ALT_1049 != 0 {
        out.extend_from_slice(b"\x1b[?1049l");
    }
    out
}

/// Parse an ASCII decimal parameter; `None` when empty or non-numeric.
fn parse_decimal(bytes: &[u8]) -> Option<u32> {
    if bytes.is_empty() || !bytes.iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut value: u32 = 0;
    for &b in bytes {
        value = value.checked_mul(10)?.checked_add(u32::from(b - b'0'))?;
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observe_all(tracker: &ModeTracker, seqs: &[&[u8]]) {
        for seq in seqs {
            tracker.observe_csi(seq);
        }
    }

    fn restore_for(seqs: &[&[u8]]) -> Vec<u8> {
        let tracker = ModeTracker::new();
        observe_all(&tracker, seqs);
        restore_bytes(tracker.snapshot())
    }

    fn position_of(haystack: &[u8], needle: &[u8]) -> usize {
        haystack
            .windows(needle.len())
            .position(|w| w == needle)
            .unwrap_or_else(|| {
                panic!(
                    "restore bytes must contain {:?} in {:?}",
                    String::from_utf8_lossy(needle),
                    String::from_utf8_lossy(haystack)
                )
            })
    }

    #[test]
    fn nothing_latched_emits_nothing() {
        assert!(restore_for(&[]).is_empty());
    }

    #[test]
    fn balanced_enable_disable_emits_nothing() {
        let out = restore_for(&[
            b"\x1b[?1049h",
            b"\x1b[?1003h",
            b"\x1b[?2004h",
            b"\x1b[?25l",
            b"\x1b[?25h",
            b"\x1b[?2004l",
            b"\x1b[?1003l",
            b"\x1b[?1049l",
        ]);
        assert!(
            out.is_empty(),
            "balanced state must restore nothing, got {:?}",
            String::from_utf8_lossy(&out)
        );
    }

    #[test]
    fn latched_modes_emit_only_their_disables() {
        let out = restore_for(&[b"\x1b[?1003h", b"\x1b[?2004h"]);
        assert_eq!(out, b"\x1b[?1003l\x1b[?2004l");
    }

    #[test]
    fn multi_param_set_latches_every_mode() {
        let out = restore_for(&[b"\x1b[?1002;1006h"]);
        assert_eq!(out, b"\x1b[?1002l\x1b[?1006l");
    }

    #[test]
    fn cursor_hide_is_tracked_inverted() {
        assert_eq!(restore_for(&[b"\x1b[?25l"]), b"\x1b[?25h");
        assert!(restore_for(&[b"\x1b[?25l", b"\x1b[?25h"]).is_empty());
        // A bare show-cursor must not latch anything.
        assert!(restore_for(&[b"\x1b[?25h"]).is_empty());
    }

    #[test]
    fn untracked_sequences_are_ignored() {
        // Autowrap reset, ANSI insert mode, SGR color, cursor restore.
        let out = restore_for(&[b"\x1b[?7l", b"\x1b[4h", b"\x1b[31m", b"\x1b[u"]);
        assert!(out.is_empty());
    }

    #[test]
    fn kitty_two_pushes_emit_two_pops() {
        let out = restore_for(&[b"\x1b[>1u", b"\x1b[>11u"]);
        assert_eq!(out, b"\x1b[<u\x1b[<u");
    }

    #[test]
    fn kitty_pop_with_count_pops_that_many() {
        let out = restore_for(&[b"\x1b[>1u", b"\x1b[>1u", b"\x1b[<2u"]);
        assert!(out.is_empty(), "CSI <2u must pop both pushes");
    }

    #[test]
    fn kitty_pop_floors_at_zero() {
        // Popping an entry the child never pushed must not go negative and
        // must not make wrap emit pops of its own later.
        assert!(restore_for(&[b"\x1b[<u"]).is_empty());
        assert!(restore_for(&[b"\x1b[<5u", b"\x1b[>1u", b"\x1b[<u"]).is_empty());
    }

    #[test]
    fn kitty_pop_zero_count_means_default_one() {
        // A terminal following the CSI zero-means-default convention pops one
        // entry for `<0u`; counting it as zero would leave wrap's depth one
        // too high — an extra pop into an enclosing stack at exit.
        assert!(restore_for(&[b"\x1b[>1u", b"\x1b[<0u"]).is_empty());
    }

    #[test]
    fn kitty_query_and_set_forms_do_not_push() {
        assert!(restore_for(&[b"\x1b[?u", b"\x1b[=5;1u", b"\x1b[u"]).is_empty());
    }

    #[test]
    fn restore_ends_synchronized_update_first() {
        let out = restore_for(&[b"\x1b[?1049h", b"\x1b[?1003h", b"\x1b[?2026h"]);
        assert!(
            out.starts_with(b"\x1b[?2026l"),
            "sync end must come first in {:?}",
            String::from_utf8_lossy(&out)
        );
    }

    #[test]
    fn restore_pops_kitty_before_alt_screen_leave() {
        let out = restore_for(&[b"\x1b[?1049h", b"\x1b[>1u"]);
        assert!(position_of(&out, b"\x1b[<u") < position_of(&out, b"\x1b[?1049l"));
    }

    #[test]
    fn restore_leaves_alt_screen_last() {
        let out = restore_for(&[
            b"\x1b[?2026h",
            b"\x1b[?1049h",
            b"\x1b[?1003h",
            b"\x1b[?1006h",
            b"\x1b[?2004h",
            b"\x1b[?25l",
            b"\x1b[>1u",
        ]);
        assert!(
            out.ends_with(b"\x1b[?1049l"),
            "alt-screen leave must be last in {:?}",
            String::from_utf8_lossy(&out)
        );
        for needle in [
            b"\x1b[?2026l".as_slice(),
            b"\x1b[?1003l".as_slice(),
            b"\x1b[?1006l".as_slice(),
            b"\x1b[?2004l".as_slice(),
            b"\x1b[?25h".as_slice(),
            b"\x1b[<u".as_slice(),
        ] {
            position_of(&out, needle);
        }
    }

    #[test]
    fn legacy_alt_screens_are_tracked() {
        assert_eq!(restore_for(&[b"\x1b[?47h"]), b"\x1b[?47l");
        assert_eq!(restore_for(&[b"\x1b[?1047h"]), b"\x1b[?1047l");
    }

    #[test]
    fn restore_gate_is_two_phase_and_one_shot() {
        let tracker = ModeTracker::new();
        assert!(!tracker.restore_done());
        assert!(tracker.begin_restore(), "first claim must win");
        assert!(!tracker.begin_restore(), "second claim must lose");
        assert!(!tracker.restore_done(), "a claim alone is not completion");
        tracker.finish_restore();
        assert!(tracker.restore_done());
    }

    /// Drift guard for the cross-crate mirror claim: `RESTORE_SEQ` is the
    /// canonical "every mode the pager enables" teardown table, so every
    /// disable it contains must be covered by this tracker, and the shared
    /// elements must be emitted in the same relative order. If a mode is
    /// added to `RESTORE_SEQ` without extending the tracker, this fails.
    #[test]
    fn covers_and_orders_every_crash_handler_restore_seq_element() {
        let elements: Vec<Vec<u8>> = echo_agent_crash_handler::terminal::RESTORE_SEQ
            .split(|&b| b == 0x1b)
            .filter(|chunk| !chunk.is_empty())
            .map(|chunk| {
                let mut seq = vec![0x1b];
                seq.extend_from_slice(chunk);
                seq
            })
            .collect();
        assert!(
            elements.len() >= 11,
            "RESTORE_SEQ must parse into its CSI elements"
        );

        // Latch, for each RESTORE_SEQ element, the state it disables.
        let tracker = ModeTracker::new();
        for element in &elements {
            let enable: Vec<u8> = match element.as_slice() {
                // Kitty pop is undone-by-tracking a single push.
                b"\x1b[<u" => b"\x1b[>1u".to_vec(),
                // Show-cursor disarms the inverted hidden-cursor latch.
                b"\x1b[?25h" => b"\x1b[?25l".to_vec(),
                seq if seq.starts_with(b"\x1b[?") && seq.ends_with(b"l") => {
                    let mut enable = seq[..seq.len() - 1].to_vec();
                    enable.push(b'h');
                    enable
                }
                seq => panic!(
                    "unhandled RESTORE_SEQ element {:?} — extend ModeTracker \
                     (and this mapping) to cover it",
                    String::from_utf8_lossy(seq)
                ),
            };
            tracker.observe_csi(&enable);
        }

        // Every element present, in RESTORE_SEQ's relative order (wrap-only
        // extras like ?1005/?1016/?1047 may interleave between them).
        let out = restore_bytes(tracker.snapshot());
        let positions: Vec<usize> = elements
            .iter()
            .map(|element| position_of(&out, element))
            .collect();
        for (window, pair) in elements.windows(2).zip(positions.windows(2)) {
            assert!(
                pair[0] < pair[1],
                "{:?} must precede {:?} to match RESTORE_SEQ, got {:?}",
                String::from_utf8_lossy(&window[0]),
                String::from_utf8_lossy(&window[1]),
                String::from_utf8_lossy(&out)
            );
        }
    }
}
