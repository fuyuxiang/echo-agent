use std::cell::Cell;
use std::time::{Duration, Instant};

/// Minimum gap between automatic recap *attempts* while still away. The shell
/// may no-op early requests (<3 min since last turn, etc.); we must retry later
/// without hammering every 20s poll.
const AUTO_RECAP_RETRY_INTERVAL: Duration = Duration::from_secs(90);

pub struct FocusTracker {
    focused: Cell<bool>,
    lost_at: Cell<Option<Instant>>,
    idle_threshold: Duration,
    /// Minimum unfocused time before an automatic session recap is offered on
    /// return. See [`FocusTracker::recap_due`].
    recap_threshold: Duration,
    /// Whether an automatic recap has already been *shown* for the current away
    /// period (set when a `SessionRecap` notification arrives). Cleared on focus
    /// loss. Stops further requests for this away period once the user has a recap.
    recap_shown_this_away: Cell<bool>,
    /// Last time we dispatched an automatic recap request (pre-gen or focus-gained).
    /// Used for retry backoff while waiting for shell gates (e.g. 3 min since last turn).
    last_auto_recap_attempt_at: Cell<Option<Instant>>,
}

impl FocusTracker {
    pub fn new(idle_threshold_secs: u64, recap_threshold_secs: u64) -> Self {
        Self {
            focused: Cell::new(true),
            lost_at: Cell::new(None),
            idle_threshold: Duration::from_secs(idle_threshold_secs),
            recap_threshold: Duration::from_secs(recap_threshold_secs),
            recap_shown_this_away: Cell::new(false),
            last_auto_recap_attempt_at: Cell::new(None),
        }
    }

    pub fn on_focus_gained(&self) {
        self.focused.set(true);
        self.lost_at.set(None);
    }

    pub fn on_focus_lost(&self) {
        self.focused.set(false);
        self.lost_at.set(Some(Instant::now()));
        // A fresh away period begins — re-arm auto recap.
        self.recap_shown_this_away.set(false);
        self.last_auto_recap_attempt_at.set(None);
    }

    pub fn should_notify(&self) -> bool {
        if self.focused.get() {
            return false;
        }
        match self.lost_at.get() {
            Some(lost) => lost.elapsed() >= self.idle_threshold,
            None => false,
        }
    }

    pub fn is_focused(&self) -> bool {
        self.focused.get()
    }

    /// `true` if an automatic session recap request should be sent: unfocused
    /// past the recap threshold, no successful recap shown this away period,
    /// and not within the retry backoff after a recent attempt.
    ///
    /// Shell gates (≥3 turns, ≥3 min since last main turn, never twice in a
    /// row) are authoritative; early attempts may no-op, so we retry on a
    /// 90s interval until shown or focus returns.
    pub fn recap_due(&self) -> bool {
        if self.focused.get() || self.recap_shown_this_away.get() {
            return false;
        }
        if let Some(last) = self.last_auto_recap_attempt_at.get()
            && last.elapsed() < AUTO_RECAP_RETRY_INTERVAL
        {
            return false;
        }
        match self.lost_at.get() {
            Some(lost) => lost.elapsed() >= self.recap_threshold,
            None => false,
        }
    }

    /// Record that an automatic recap was dispatched (pre-gen or focus-gained).
    /// Does **not** consume the away period — only starts retry backoff so we
    /// do not spam every poll while the shell still rejects (e.g. <3 min idle).
    pub fn note_auto_recap_attempt(&self) {
        self.last_auto_recap_attempt_at.set(Some(Instant::now()));
    }

    /// Record that a recap was shown (auto or manual `/recap`) for the current
    /// away period. Stops further **auto** requests until focus is lost again.
    /// Manual `/recap` may still be invoked repeatedly.
    pub fn mark_recap_shown(&self) {
        self.recap_shown_this_away.set(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newly_created_tracker_is_focused() {
        let tracker = FocusTracker::new(3, 180);
        assert!(tracker.is_focused());
        assert!(!tracker.should_notify());
    }

    #[test]
    fn should_not_notify_immediately_after_focus_lost() {
        let tracker = FocusTracker::new(3, 180);
        tracker.on_focus_lost();
        assert!(!tracker.is_focused());
        assert!(!tracker.should_notify());
    }

    #[test]
    fn should_notify_after_threshold_elapsed() {
        let tracker = FocusTracker::new(0, 180);
        tracker.on_focus_lost();
        // With a 0-second threshold, should_notify is true immediately
        assert!(tracker.should_notify());
    }

    #[test]
    fn should_not_notify_when_refocused_after_threshold() {
        let tracker = FocusTracker::new(0, 180);
        tracker.on_focus_lost();
        assert!(tracker.should_notify());
        tracker.on_focus_gained();
        assert!(tracker.is_focused());
        assert!(!tracker.should_notify());
    }

    #[test]
    fn rapid_focus_toggle_resets_timer() {
        let tracker = FocusTracker::new(60, 180);
        tracker.on_focus_lost();
        tracker.on_focus_gained();
        tracker.on_focus_lost();
        // Timer restarted on the second loss, so threshold is far away
        assert!(!tracker.should_notify());
    }

    #[test]
    fn threshold_boundary_with_manual_instant() {
        let tracker = FocusTracker::new(5, 180);
        // Simulate loss in the past by directly setting lost_at
        tracker.focused.set(false);
        tracker
            .lost_at
            .set(Some(Instant::now() - Duration::from_secs(6)));
        assert!(tracker.should_notify());
    }

    #[test]
    fn threshold_boundary_not_yet_reached() {
        let tracker = FocusTracker::new(5, 180);
        tracker.focused.set(false);
        tracker
            .lost_at
            .set(Some(Instant::now() - Duration::from_secs(2)));
        assert!(!tracker.should_notify());
    }

    #[test]
    fn focus_gained_clears_lost_at() {
        let tracker = FocusTracker::new(0, 180);
        tracker.on_focus_lost();
        assert!(tracker.should_notify());
        tracker.on_focus_gained();
        // lost_at is None after regain, so even unfocused state would return false
        assert!(tracker.is_focused());
        assert!(tracker.lost_at.get().is_none());
    }

    #[test]
    fn multiple_focus_lost_calls_update_timestamp() {
        let tracker = FocusTracker::new(5, 180);
        tracker.focused.set(false);
        let old = Instant::now() - Duration::from_secs(10);
        tracker.lost_at.set(Some(old));
        assert!(tracker.should_notify());

        // Second on_focus_lost resets the timer
        tracker.on_focus_lost();
        assert!(!tracker.should_notify());
    }

    // --- Auto session-recap (recap_due) tests ---

    #[test]
    fn recap_not_due_while_focused() {
        let tracker = FocusTracker::new(3, 0);
        assert!(!tracker.recap_due(), "focused terminal is never away");
    }

    #[test]
    fn recap_not_due_immediately_after_focus_lost() {
        let tracker = FocusTracker::new(3, 180);
        tracker.on_focus_lost();
        assert!(!tracker.recap_due(), "not away long enough yet");
    }

    #[test]
    fn recap_due_after_away_threshold() {
        let tracker = FocusTracker::new(3, 5);
        tracker.focused.set(false);
        tracker
            .lost_at
            .set(Some(Instant::now() - Duration::from_secs(6)));
        assert!(tracker.recap_due());
    }

    #[test]
    fn recap_due_respects_independent_threshold() {
        // idle (notification) threshold is 0, but recap threshold is large:
        // a brief away period must not be recap-eligible.
        let tracker = FocusTracker::new(0, 180);
        tracker.on_focus_lost();
        assert!(tracker.should_notify(), "notification fires immediately");
        assert!(!tracker.recap_due(), "recap waits for its own threshold");
    }

    #[test]
    fn recap_due_stops_after_shown() {
        let tracker = FocusTracker::new(3, 0);
        tracker.on_focus_lost();
        assert!(tracker.recap_due());
        tracker.mark_recap_shown();
        assert!(
            !tracker.recap_due(),
            "must not request again once recap is on screen"
        );
    }

    #[test]
    fn recap_re_arms_after_new_away_period() {
        let tracker = FocusTracker::new(3, 0);
        tracker.on_focus_lost();
        tracker.mark_recap_shown();
        assert!(!tracker.recap_due());
        // Return, then leave again — a new away period re-arms the recap.
        tracker.on_focus_gained();
        tracker.on_focus_lost();
        assert!(tracker.recap_due());
    }

    /// Early dispatch must not consume the away period (shell may no-op until
    /// ≥3 min since last turn). Only backoff applies; after the interval we retry.
    #[test]
    fn recap_due_backoff_after_attempt_allows_retry() {
        let tracker = FocusTracker::new(3, 0);
        tracker.on_focus_lost();
        assert!(tracker.recap_due());
        tracker.note_auto_recap_attempt();
        assert!(
            !tracker.recap_due(),
            "must not re-fire on the next 20s poll"
        );
        // Simulate retry interval elapsed without a successful notification.
        tracker.last_auto_recap_attempt_at.set(Some(
            Instant::now() - AUTO_RECAP_RETRY_INTERVAL - Duration::from_secs(1),
        ));
        assert!(
            tracker.recap_due(),
            "shell may accept once 3 min since last turn; pager must retry"
        );
    }

    #[test]
    fn recap_due_shown_wins_over_retry_backoff() {
        let tracker = FocusTracker::new(3, 0);
        tracker.on_focus_lost();
        tracker.note_auto_recap_attempt();
        tracker.last_auto_recap_attempt_at.set(Some(
            Instant::now() - AUTO_RECAP_RETRY_INTERVAL - Duration::from_secs(1),
        ));
        tracker.mark_recap_shown();
        assert!(!tracker.recap_due(), "shown recap must not retry");
    }
}
