//! Clipboard-image tip trigger: while the terminal is focused and the active
//! agent is image-eligible, hint that ctrl+v pastes an image sitting on the
//! pasteboard — without waiting for a focus switch.
//!
//! Trigger model: opportunistic, focus-scoped polling. The caller drives
//! [`ClipboardFocusTipState::poll`] only from event-loop iterations that are
//! already running for another reason (input, FocusGained, resize, an animation
//! tick); nothing schedules a wakeup and the tip never forces animation, so an
//! idle/hibernating/unfocused app polls zero times. Each in-window poll is
//! throttled to one cheap `changeCount` read per [`POLL_INTERVAL`], and the
//! heavier type classification runs ONLY on a changeCount delta. Frequency is
//! further capped by a fire cooldown plus a changeCount dedup (the same copied
//! content never re-fires), not a seen-count — the tip is contextual and
//! recurring by design.
//!
//! The state machine takes the clock and BOTH probe steps as inputs, so every
//! transition — including "classify is not called when the changeCount is
//! unchanged" — is unit-testable with a fake clock and call-counting probes.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::EphemeralTip;
use crate::input::key::KeyShortcut;
use crate::theme::Theme;

/// Ephemeral-tip dedup key for the clipboard-image hint.
pub const CLIPBOARD_IMAGE_TIP_KEY: &str = "clipboard_image_tip";

/// Throttle for the opportunistic pasteboard poll: at most one `changeCount`
/// read per this interval, even when the event loop iterates at ~30fps. The
/// poll rides existing loop activity (it never schedules a tick), so this only
/// caps how often an already-running iteration touches the pasteboard.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Minimum spacing between fires, so copy-heavy workflows aren't nagged on every
/// new image.
const FIRE_COOLDOWN: Duration = Duration::from_secs(30);

/// Paste chord for the tip copy: always `ctrl+v`. Most macOS terminal emulators
/// capture Cmd by default and don't forward Cmd+V to a raw-mode TUI, so Ctrl+V
/// is the chord actually delivered. Derived from the real binding (not a
/// literal) — Ctrl+V is one of the two chords [`crate::input::key::is_paste_key`]
/// accepts — so it can't drift.
fn paste_label() -> String {
    KeyShortcut::new(KeyCode::Char('v'), KeyModifiers::CONTROL)
        .display()
        .to_ascii_lowercase()
}

/// Build the "Image in clipboard · {chord} to paste" tip. No seen-cap:
/// changeCount dedup + the cooldown are the frequency caps.
pub fn clipboard_image_tip() -> EphemeralTip {
    let theme = Theme::current();
    let dim = Style::default().fg(theme.gray);
    // Key chord styled like the shortcuts bar (bold secondary on dim text).
    let chord = Style::default()
        .fg(theme.text_secondary)
        .add_modifier(Modifier::BOLD);
    EphemeralTip::new(
        CLIPBOARD_IMAGE_TIP_KEY,
        Line::from(vec![
            Span::styled("Image in clipboard · ", dim),
            Span::styled(paste_label(), chord),
            Span::styled(" to paste", dim),
        ]),
    )
}

/// Result of one pasteboard classification pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckOutcome {
    /// Pasteboard change count at probe time (`None` when unavailable).
    pub change_count: Option<u64>,
    /// Whether a *pasteable* image was advertised: raster types with no
    /// file-URL types alongside. File-manager copies (Finder) put a file-icon
    /// raster on the board next to the file URLs, but ctrl+v routes those
    /// through path handling, so they must not fire a tip promising an image
    /// paste (see `clipboard_image_snapshot`).
    pub has_image: bool,
}

/// The `classify` step: the heavier native probe in one pasteboard pass — the
/// changeCount plus the advertised type list (no image bytes, no subprocess),
/// so it stays sub-millisecond and safe to call inline on the ~30fps loop.
///
/// The throttled poll reaches here ONLY on a changeCount delta (see
/// [`ClipboardFocusTipState::poll`]); the cheap changeCount-only read gates it.
/// The expensive one-time AppKit `dlopen` is pre-warmed off the UI thread at the
/// first focus-gain (see `clipboard::prewarm_image_probe`); if the warm-up
/// hasn't finished yet the memoised `dlopen` happens here once as a fallback.
pub fn run_clipboard_check() -> CheckOutcome {
    let (change_count, has_image) = crate::clipboard::clipboard_image_snapshot();
    CheckOutcome {
        change_count,
        has_image,
    }
}

/// Pure state machine for the focus-scoped, opportunistically-polled
/// clipboard-image tip.
///
/// Owns the poll throttle, the changeCount delta-detection, the fire cooldown,
/// and the changeCount dedup. It never schedules itself — the caller drives
/// [`Self::poll`] from event-loop iterations that are already running for some
/// other reason (input, FocusGained, resize, an animation tick), so an idle or
/// hibernating app polls zero times. All inputs (clock, both probe steps) are
/// injected, so every transition is unit-testable with a fake clock and
/// call-counting probes.
#[derive(Debug, Default)]
pub struct ClipboardFocusTipState {
    /// When the last poll actually read the pasteboard (throttle anchor).
    last_poll_at: Option<Instant>,
    /// changeCount observed by the last cheap read; a differing value is what
    /// warrants paying for the type classification.
    last_seen_change_count: Option<u64>,
    /// When the tip last actually showed (cooldown anchor).
    last_fired_at: Option<Instant>,
    /// changeCount of the content that last fired; identical content never
    /// fires twice even across long gaps.
    last_fired_change_count: Option<u64>,
}

impl ClipboardFocusTipState {
    /// Throttle gate: at most one poll per [`POLL_INTERVAL`], so a ~30fps loop
    /// still reads the pasteboard at most ~once a second. Pure — does not mutate.
    pub fn due_to_poll(&self, now: Instant) -> bool {
        self.last_poll_at
            .is_none_or(|at| now.duration_since(at) >= POLL_INTERVAL)
    }

    /// Whether `change_count` differs from the one the last cheap read saw — the
    /// signal that the pasteboard changed and a classification is worth paying
    /// for. A `None` (changeCount unavailable, e.g. AppKit failed to load) is
    /// treated as "nothing new" so the cheap path never escalates blindly.
    fn is_new_change_count(&self, change_count: Option<u64>) -> bool {
        change_count.is_some() && change_count != self.last_seen_change_count
    }

    /// Run one throttled poll on an already-running loop iteration.
    ///
    /// `cheap` reads ONLY the pasteboard changeCount (one Obj-C message);
    /// `classify` runs the heavier type scan. The contract that keeps the idle
    /// cost at ~zero: `classify` is invoked ONLY when `cheap` reports a
    /// changeCount that differs from the last one seen. Returns the classified
    /// [`CheckOutcome`] for the caller to evaluate via [`Self::should_fire`], or
    /// `None` when the poll was throttled or the changeCount was unchanged (the
    /// hot path — no classify, no redraw).
    ///
    /// Dedup-commit policy: the classify-dedup (`last_seen_change_count`) is
    /// advanced here ONLY for content this poll fully handles — non-image
    /// content, which has nothing to show. A fireable image is deferred to
    /// [`Self::note_fired`] (called only on a landed show): committing it here
    /// would let a *refused* show skip re-classification forever, breaking the
    /// "refused show burns nothing" contract. So an image found but not shown
    /// re-classifies on the next poll; a shown image is deduped post-cooldown.
    pub fn poll(
        &mut self,
        now: Instant,
        cheap: impl FnOnce() -> Option<u64>,
        classify: impl FnOnce() -> CheckOutcome,
    ) -> Option<CheckOutcome> {
        if !self.due_to_poll(now) {
            return None;
        }
        self.last_poll_at = Some(now);
        let change_count = cheap();
        if !self.is_new_change_count(change_count) {
            return None;
        }
        let outcome = classify();
        // Commit the classify-dedup now only for non-image content (nothing to
        // show, so it's fully handled). A fireable image waits for `note_fired`
        // so a refused show stays retryable.
        if !outcome.has_image {
            self.last_seen_change_count = change_count;
        }
        Some(outcome)
    }

    /// Whether `outcome` warrants showing the tip right now. Pure check — the
    /// caller commits via [`Self::note_fired`] only after the show actually
    /// lands, so refused shows never burn the cooldown or dedup.
    pub fn should_fire(&self, outcome: &CheckOutcome, now: Instant) -> bool {
        outcome.has_image
            && !self.in_cooldown(now)
            && (outcome.change_count.is_none()
                || outcome.change_count != self.last_fired_change_count)
    }

    /// Commit a successful (landed) show: anchors the cooldown, records the
    /// fired changeCount, and — because the show is now fully handled — commits
    /// the classify-dedup too (`poll` defers it for fireable images). So the
    /// same image isn't re-scanned once the cooldown elapses, while a refused
    /// show (which never calls this) leaves `last_seen` stale and stays retryable.
    pub fn note_fired(&mut self, outcome: &CheckOutcome, now: Instant) {
        self.last_fired_at = Some(now);
        if outcome.change_count.is_some() {
            self.last_fired_change_count = outcome.change_count;
            self.last_seen_change_count = outcome.change_count;
        }
    }

    /// Whether the fire cooldown is still in effect. Part of the caller's
    /// in-window gate, so during the cooldown the poll touches the pasteboard
    /// zero times.
    pub fn in_cooldown(&self, now: Instant) -> bool {
        self.last_fired_at
            .is_some_and(|at| now.duration_since(at) < FIRE_COOLDOWN)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    fn outcome(change_count: Option<u64>, has_image: bool) -> CheckOutcome {
        CheckOutcome {
            change_count,
            has_image,
        }
    }

    /// Drive a full successful fire through the poll path (changeCount delta →
    /// classify → should_fire → note_fired).
    fn fire_via_poll(state: &mut ClipboardFocusTipState, now: Instant, change_count: u64) {
        let got = state
            .poll(
                now,
                || Some(change_count),
                || outcome(Some(change_count), true),
            )
            .expect("a changeCount delta should classify");
        assert!(state.should_fire(&got, now));
        state.note_fired(&got, now);
    }

    #[test]
    fn paste_chord_is_ctrl_v() {
        // Always ctrl+v (the chord terminals actually deliver) — derived from
        // the real binding so the label can't drift.
        assert_eq!(paste_label(), "ctrl+v");
        assert!(crate::input::key::is_paste_key(
            &crossterm::event::KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL)
        ));
    }

    #[test]
    fn clipboard_image_tip_is_not_seen_capped() {
        // Frequency is capped by changeCount dedup + cooldown, never a
        // seen-count — so the builder must not opt into the seen gate.
        assert!(clipboard_image_tip().session_seen.is_none());
    }

    #[test]
    fn throttle_limits_reads_to_one_per_interval() {
        let mut state = ClipboardFocusTipState::default();
        let t0 = Instant::now();
        let cheap_reads = Cell::new(0u32);

        // First poll reads the cheap changeCount.
        let _ = state.poll(
            t0,
            || {
                cheap_reads.set(cheap_reads.get() + 1);
                Some(1)
            },
            || outcome(Some(1), false),
        );
        assert_eq!(cheap_reads.get(), 1);

        // A second poll within the interval is throttled — no cheap read at all.
        let _ = state.poll(
            t0 + Duration::from_millis(500),
            || {
                cheap_reads.set(cheap_reads.get() + 1);
                Some(1)
            },
            || outcome(Some(1), false),
        );
        assert_eq!(cheap_reads.get(), 1, "two polls <1s apart → one read");

        // Once the interval elapses the next poll reads again.
        let _ = state.poll(
            t0 + POLL_INTERVAL,
            || {
                cheap_reads.set(cheap_reads.get() + 1);
                Some(2)
            },
            || outcome(Some(2), false),
        );
        assert_eq!(cheap_reads.get(), 2, "poll resumes after the interval");
    }

    #[test]
    fn unchanged_change_count_skips_classify_and_does_not_fire() {
        let mut state = ClipboardFocusTipState::default();
        let t0 = Instant::now();
        let classify_calls = Cell::new(0u32);

        // First poll: changeCount 5 is new → classify runs once.
        let first = state.poll(
            t0,
            || Some(5),
            || {
                classify_calls.set(classify_calls.get() + 1);
                outcome(Some(5), false)
            },
        );
        assert_eq!(first, Some(outcome(Some(5), false)));
        assert_eq!(classify_calls.get(), 1);

        // Next interval, SAME changeCount → cheap path returns; the call-counter
        // proves the classify probe was NOT invoked.
        let next = state.poll(
            t0 + POLL_INTERVAL,
            || Some(5),
            || {
                classify_calls.set(classify_calls.get() + 1);
                outcome(Some(5), false)
            },
        );
        assert_eq!(next, None, "unchanged changeCount → no outcome");
        assert_eq!(
            classify_calls.get(),
            1,
            "classify must not run on an unchanged changeCount"
        );
    }

    #[test]
    fn change_to_image_classifies_and_fires_once() {
        let mut state = ClipboardFocusTipState::default();
        let t0 = Instant::now();
        let classify_calls = Cell::new(0u32);

        let got = state
            .poll(
                t0,
                || Some(3),
                || {
                    classify_calls.set(classify_calls.get() + 1);
                    outcome(Some(3), true)
                },
            )
            .expect("a changeCount delta classifies");
        assert_eq!(classify_calls.get(), 1);
        assert!(state.should_fire(&got, t0));
        state.note_fired(&got, t0);

        // Same content, past the cooldown, changeCount unchanged → cheap path
        // short-circuits; the classify closure panics if reached, proving the
        // same image never re-classifies or re-fires.
        let later = t0 + FIRE_COOLDOWN + Duration::from_secs(1);
        let again = state.poll(
            later,
            || Some(3),
            || panic!("classify must not run for unchanged (deduped) content"),
        );
        assert_eq!(again, None, "same image never re-fires");
    }

    #[test]
    fn refused_show_keeps_retrying_then_dedups_once_landed() {
        let mut state = ClipboardFocusTipState::default();
        let t0 = Instant::now();
        let classify_calls = Cell::new(0u32);

        // Image copied: classify runs and returns a fireable image.
        let got = state
            .poll(
                t0,
                || Some(7),
                || {
                    classify_calls.set(classify_calls.get() + 1);
                    outcome(Some(7), true)
                },
            )
            .expect("a changeCount delta classifies");
        assert_eq!(classify_calls.get(), 1);
        assert!(state.should_fire(&got, t0));

        // Show REFUSED — the caller did NOT call note_fired. `poll` must not have
        // advanced `last_seen` for a fireable image, so the same changeCount
        // RE-classifies on the next poll (preserving the retry).
        let t1 = t0 + POLL_INTERVAL;
        let retry = state.poll(
            t1,
            || Some(7),
            || {
                classify_calls.set(classify_calls.get() + 1);
                outcome(Some(7), true)
            },
        );
        assert_eq!(
            retry,
            Some(outcome(Some(7), true)),
            "a refused image must re-classify, not be skipped as 'seen'"
        );
        assert_eq!(
            classify_calls.get(),
            2,
            "classify ran again for the un-shown image"
        );

        // Now the show LANDS: note_fired commits the classify-dedup too, so the
        // same content past the cooldown does NOT re-classify (panic if it does).
        let landed = retry.unwrap();
        state.note_fired(&landed, t1);
        let t2 = t1 + FIRE_COOLDOWN + Duration::from_secs(1);
        let after = state.poll(
            t2,
            || Some(7),
            || panic!("a shown image must not re-classify"),
        );
        assert_eq!(
            after, None,
            "a successfully shown image is deduped post-cooldown"
        );
    }

    #[test]
    fn cooldown_blocks_fire_until_elapsed() {
        let mut state = ClipboardFocusTipState::default();
        let t0 = Instant::now();
        fire_via_poll(&mut state, t0, 1);

        // A different copy mid-cooldown classifies (changeCount changed) but
        // should_fire refuses while the cooldown holds.
        let during = t0 + Duration::from_secs(5);
        let o2 = state
            .poll(during, || Some(2), || outcome(Some(2), true))
            .expect("a new changeCount classifies");
        assert!(!state.should_fire(&o2, during), "inside the cooldown");

        // After the cooldown a fresh copy fires again.
        let after = t0 + FIRE_COOLDOWN + Duration::from_secs(1);
        let o3 = state
            .poll(after, || Some(3), || outcome(Some(3), true))
            .expect("a new changeCount classifies");
        assert!(state.should_fire(&o3, after), "cooldown over");
    }

    #[test]
    fn no_image_never_fires() {
        let state = ClipboardFocusTipState::default();
        let t0 = Instant::now();
        assert!(!state.should_fire(&outcome(Some(3), false), t0));
    }

    #[test]
    fn refused_show_burns_nothing() {
        let state = ClipboardFocusTipState::default();
        let t0 = Instant::now();
        let got = outcome(Some(4), true);
        assert!(state.should_fire(&got, t0));
        // Caller could not paint (e.g. modal raced in) and did NOT commit: the
        // same outcome stays fireable and no cooldown started.
        assert!(state.should_fire(&got, t0 + Duration::from_secs(1)));
        assert!(!state.in_cooldown(t0 + Duration::from_secs(1)));
    }

    #[test]
    fn missing_change_count_still_fires_under_cooldown_cap() {
        let mut state = ClipboardFocusTipState::default();
        let t0 = Instant::now();
        let got = outcome(None, true);
        assert!(state.should_fire(&got, t0));
        state.note_fired(&got, t0);
        assert!(
            !state.should_fire(&got, t0 + Duration::from_secs(1)),
            "cooldown still caps when dedup is unavailable"
        );
    }
}
