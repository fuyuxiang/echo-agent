//! The edits one server still owes us a verdict on.
//!
//! A URI is pending from the moment we tell the server about an edit until the
//! server gives a verdict on that edit — diagnostics, or the news that there
//! are none — or until it has waited long enough that we stop expecting one.
//!
//! Both of those are questions about *data*, not about bookkeeping: whether the
//! store holds a verdict for the version we sent, and whether the wall clock
//! has passed a deadline. Nothing here has to be told how a drain ended, so
//! there is no path on which a drain can forget to tell it, and no counter that
//! can drift.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use super::diagnostics::DiagnosticsStore;

/// How long we hold on to a file waiting for a verdict.
///
/// Bounds the pending set: a file nobody ever answers for is let go rather than
/// carried for the rest of the session. Generous, because the cost of holding
/// one is two integers and the cost of dropping one too early is a missed
/// diagnostic.
pub const VERDICT_TTL: Duration = Duration::from_secs(30);

/// How long a server gets to say *something* before we stop blocking on it.
///
/// Measured only while we are actually waiting on it — see
/// [`PendingEdits::asking_since`] — so an idle stretch with nothing outstanding
/// does not count against a server, and any answer starts it over. A server
/// that has been asked for this long without a word is not about to answer
/// within the drain's budget, and waiting out that budget on every later turn
/// just makes every turn slower.
///
/// It measures *silence*, not "no problems found": a server reporting that a
/// file is clean has answered. Conflating the two writes a healthy server off
/// after a few clean edits, which is the common case.
pub const SERVER_PATIENCE: Duration = Duration::from_secs(10);

/// The two durations, together, so a caller that wants to change one is
/// choosing between two named things rather than editing a constant that also
/// means something else.
///
/// They answer different questions — how long we hold on to a file, and
/// whether the server is worth blocking on — which is why they are separate.
/// One number doing both jobs is how a server that answered three clean edits
/// in a row came to be written off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingPolicy {
    pub verdict_ttl: Duration,
    pub server_patience: Duration,
}

impl Default for PendingPolicy {
    fn default() -> Self {
        Self {
            verdict_ttl: VERDICT_TTL,
            server_patience: SERVER_PATIENCE,
        }
    }
}

/// One edit we are waiting on.
#[derive(Debug, Clone, Copy)]
struct PendingEdit {
    /// The document version we want a verdict on. A verdict on this version or
    /// a later one settles it; one on an earlier version — an answer that was
    /// already being computed when this edit arrived — does not.
    version: i32,
    expires_at: Instant,
}

/// Files edited since the server last gave a verdict on them.
#[derive(Debug, Default)]
pub struct PendingEdits {
    policy: PendingPolicy,
    lifecycle_id: u64,
    by_uri: BTreeMap<String, PendingEdit>,
    /// When the current stretch of asking-without-an-answer began.
    ///
    /// Set while something is outstanding, cleared by any answer. That is what
    /// makes it a measure of the server rather than of the clock: a session
    /// spent reading code does not make a healthy server look dead.
    asking_since: Option<Instant>,
}

impl PendingEdits {
    pub fn new(policy: PendingPolicy) -> Self {
        Self {
            policy,
            ..Self::default()
        }
    }

    /// Start waiting for a verdict on `version` of `uri`.
    ///
    /// A restart makes every outstanding question meaningless — the fresh
    /// server was never asked them — so a new lifecycle starts from nothing.
    pub fn mark(&mut self, lifecycle_id: u64, uri: &str, version: i32, now: Instant) {
        if self.lifecycle_id != lifecycle_id {
            *self = Self {
                policy: self.policy,
                lifecycle_id,
                ..Self::default()
            };
        }
        self.asking_since.get_or_insert(now);
        // Re-editing a file we are already waiting on moves the goalposts: only
        // a verdict on this version or later will do, and the clock restarts.
        self.by_uri.insert(
            uri.to_string(),
            PendingEdit {
                version,
                expires_at: now + self.policy.verdict_ttl,
            },
        );
    }

    /// The server has spoken of its own accord — it has asked us to read its
    /// answers again — which is proof of life whatever it was about, so any
    /// stretch of silence it was in is over.
    ///
    /// Without this the case the refresh exists for is the one it fails on: a
    /// server that spends a long time loading has already been written off as
    /// silent by the time it announces it is ready, so the very drain that
    /// should wait for the re-pull it just asked for would not wait at all.
    pub fn note_server_spoke(&mut self) {
        self.asking_since = None;
    }

    /// Forget everything, for a server whose client has been replaced.
    pub fn clear(&mut self) {
        self.by_uri.clear();
        self.asking_since = None;
    }

    pub fn lifecycle_id(&self) -> u64 {
        self.lifecycle_id
    }

    pub fn is_empty(&self) -> bool {
        self.by_uri.is_empty()
    }

    pub fn len(&self) -> usize {
        self.by_uri.len()
    }

    #[cfg(test)]
    pub fn contains(&self, uri: &str) -> bool {
        self.by_uri.contains_key(uri)
    }

    /// Take the files the server has now given a verdict on, and let go of the
    /// ones that have waited long enough.
    ///
    /// Returned in URI order, so what a reader sees does not depend on the
    /// order edits happened to arrive in.
    pub fn take_answered(
        &mut self,
        server_name: &str,
        store: &DiagnosticsStore,
        now: Instant,
    ) -> Vec<String> {
        let mut answered = Vec::new();
        let mut expired = 0usize;

        self.by_uri.retain(|uri, pending| {
            if store.answered_for(uri, pending.version) {
                answered.push(uri.clone());
                return false;
            }
            if now >= pending.expires_at {
                expired += 1;
                return false;
            }
            true
        });

        if expired > 0 {
            tracing::debug!(
                server = %server_name, expired,
                "no verdict on these files in time; no longer waiting"
            );
        }
        // An answer about any file is proof the server is working, whatever it
        // was about, so the stretch of silence is over. It does not restart for
        // whatever is still outstanding: a server that answers for most files
        // and never for one would then be judged silent on the strength of the
        // one, and stop being waited on while it was plainly working. The cost
        // of erring this way is bounded — the stuck file leaves on its own
        // deadline — and it errs towards waiting for a server we have evidence
        // is alive, which is the right direction.
        //
        // Only an answer does this. Letting go of a file because it ran out of
        // time is the opposite of evidence, and treating an empty set as a
        // fresh start would hand a server that has never said a word a clean
        // slate every time its files expired.
        if !answered.is_empty() {
            self.asking_since = None;
        }
        answered
    }

    /// Whether it is still worth blocking a turn on this server.
    pub fn worth_blocking(&self, now: Instant) -> bool {
        !self.by_uri.is_empty()
            && self.asking_since.is_none_or(|since| {
                now.saturating_duration_since(since) < self.policy.server_patience
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::implementations::lsp::diagnostics::Answer;
    use async_lsp::lsp_types::Diagnostic;

    const SERVER: &str = "test-server";
    const A: &str = "file:///a.cs";
    const B: &str = "file:///b.cs";

    fn diagnostic() -> Diagnostic {
        Diagnostic {
            message: "boom".into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_verdict_on_the_edit_settles_it() {
        let store = DiagnosticsStore::new();
        let mut pending = PendingEdits::default();
        let now = Instant::now();
        pending.mark(1, A, 3, now);

        assert!(pending.take_answered(SERVER, &store, now).is_empty());
        assert!(pending.contains(A));

        store.install(A, Answer::new(vec![diagnostic()], 3, None));
        assert_eq!(pending.take_answered(SERVER, &store, now), vec![A]);
        assert!(pending.is_empty());
    }

    #[test]
    fn a_clean_verdict_settles_it_too() {
        let store = DiagnosticsStore::new();
        let mut pending = PendingEdits::default();
        let now = Instant::now();
        pending.mark(1, A, 0, now);

        store.install(A, Answer::new(vec![], 0, None));
        assert_eq!(
            pending.take_answered(SERVER, &store, now),
            vec![A],
            "'no problems' is a verdict; a file that keeps waiting for one it \
             has already had is what makes the set grow without bound"
        );
    }

    #[test]
    fn a_verdict_on_the_previous_revision_does_not_settle_the_new_one() {
        let store = DiagnosticsStore::new();
        let mut pending = PendingEdits::default();
        let now = Instant::now();

        store.install(A, Answer::new(vec![diagnostic()], 2, None));
        pending.mark(1, A, 3, now);

        assert!(
            pending.take_answered(SERVER, &store, now).is_empty(),
            "the answer already in the store is about text we have since replaced"
        );
    }

    #[test]
    fn a_file_nobody_answers_for_is_eventually_let_go() {
        let store = DiagnosticsStore::new();
        let mut pending = PendingEdits::default();
        let start = Instant::now();
        pending.mark(1, A, 0, start);

        assert!(
            pending
                .take_answered(
                    SERVER,
                    &store,
                    start + VERDICT_TTL - Duration::from_millis(1)
                )
                .is_empty()
        );
        assert!(pending.contains(A), "still within its time");

        assert!(
            pending
                .take_answered(SERVER, &store, start + VERDICT_TTL)
                .is_empty(),
            "letting go is not the same as reporting"
        );
        assert!(pending.is_empty());
    }

    /// The set is bounded by what the server owes us, not by how many files
    /// have been touched: a server answering for one file while ignoring
    /// another must still let go of the one it ignores.
    #[test]
    fn a_productive_server_still_lets_go_of_a_file_it_never_answers_for() {
        let store = DiagnosticsStore::new();
        let mut pending = PendingEdits::default();
        let start = Instant::now();

        pending.mark(1, A, 0, start);
        pending.mark(1, B, 0, start);
        store.install(A, Answer::new(vec![diagnostic()], 0, None));

        assert_eq!(pending.take_answered(SERVER, &store, start), vec![A]);
        assert_eq!(pending.len(), 1, "b is still owed");

        assert!(
            pending
                .take_answered(SERVER, &store, start + VERDICT_TTL)
                .is_empty()
        );
        assert!(pending.is_empty(), "and eventually let go of");
    }

    #[test]
    fn a_silent_server_stops_costing_every_turn_its_budget() {
        let store = DiagnosticsStore::new();
        let mut pending = PendingEdits::default();
        let start = Instant::now();

        pending.mark(1, A, 0, start);
        assert!(pending.worth_blocking(start));

        // Turn after turn of edits, and never a word back.
        let later = start + SERVER_PATIENCE;
        pending.mark(1, B, 0, later);
        pending.take_answered(SERVER, &store, later);
        assert!(
            !pending.worth_blocking(later),
            "a fresh edit does not restart the clock on a server that has said nothing"
        );
    }

    #[test]
    fn a_server_that_starts_answering_is_waited_on_again() {
        let store = DiagnosticsStore::new();
        let mut pending = PendingEdits::default();
        let start = Instant::now();

        pending.mark(1, A, 0, start);
        let quiet = start + SERVER_PATIENCE;
        assert!(!pending.worth_blocking(quiet));

        // Roslyn finishes loading its solution and finally says something.
        store.install(A, Answer::new(vec![diagnostic()], 0, None));
        assert_eq!(pending.take_answered(SERVER, &store, quiet), vec![A]);

        pending.mark(1, B, 0, quiet);
        assert!(
            pending.worth_blocking(quiet),
            "an answer is proof of life, whatever it was about"
        );
    }

    /// A server that answers for most files and never for one must not be
    /// judged silent on the strength of the one. The stuck file leaves on its
    /// own deadline; until then the server keeps being waited on, because it is
    /// plainly working.
    #[test]
    fn one_file_nobody_answers_for_does_not_make_a_busy_server_look_dead() {
        let store = DiagnosticsStore::new();
        let mut pending = PendingEdits::default();
        let start = Instant::now();
        let mut now = start;

        pending.mark(1, A, 1, now); // never answered
        // Several stretches longer than the server's patience, all well within
        // the stuck file's own deadline.
        for round in 1..3 {
            pending.mark(1, B, round, now);
            store.install(B, Answer::new(vec![diagnostic()], round, None));
            assert_eq!(pending.take_answered(SERVER, &store, now), vec![B]);
            now += SERVER_PATIENCE;
            assert!(
                pending.contains(A),
                "round {round}: the stuck file is still within its deadline"
            );
            assert!(
                pending.worth_blocking(now),
                "round {round}: the server answered, so it is worth waiting for"
            );
        }
        assert!(
            now.duration_since(start) < VERDICT_TTL,
            "the test must stay inside the stuck file's deadline to be about patience"
        );
    }

    /// Roslyn goes quiet for as long as it takes to load a solution, and then
    /// says it is ready. Asking for a refresh is the server speaking, so it
    /// starts the clock over — otherwise the drain that should wait for the
    /// re-pull the server just asked for would return without waiting.
    #[test]
    fn a_server_that_asks_for_a_refresh_is_worth_waiting_for_again() {
        let mut pending = PendingEdits::default();
        let start = Instant::now();
        pending.mark(1, A, 1, start);

        let quiet = start + SERVER_PATIENCE;
        assert!(!pending.worth_blocking(quiet), "written off as silent");

        pending.note_server_spoke();
        pending.mark(1, A, 1, quiet);
        assert!(
            pending.worth_blocking(quiet),
            "the refresh was the server proving it is alive and about to answer"
        );
    }

    #[test]
    fn nothing_outstanding_is_nothing_to_wait_for() {
        let pending = PendingEdits::default();
        assert!(!pending.worth_blocking(Instant::now()));
    }

    /// Letting go of a file is not the server saying something. If running out
    /// of time counted as the end of a stretch of silence, a server that never
    /// speaks would get a clean slate every time its files expired, and every
    /// later turn would block on it for the full drain budget again.
    #[test]
    fn running_out_of_time_is_not_evidence_of_life() {
        let store = DiagnosticsStore::new();
        let mut pending = PendingEdits::default();
        let start = Instant::now();

        pending.mark(1, A, 0, start);
        let expired = start + VERDICT_TTL;
        assert!(pending.take_answered(SERVER, &store, expired).is_empty());
        assert!(pending.is_empty(), "the file was let go");

        pending.mark(1, B, 0, expired);
        assert!(
            !pending.worth_blocking(expired),
            "the server has still never said a word"
        );
    }

    #[test]
    fn a_clean_stretch_does_not_count_against_the_server() {
        let store = DiagnosticsStore::new();
        let mut pending = PendingEdits::default();
        let start = Instant::now();

        // Several turns, every one of them answered "no problems".
        let mut now = start;
        for round in 0..5 {
            pending.mark(1, A, round, now);
            store.install(A, Answer::new(vec![], round, None));
            assert_eq!(pending.take_answered(SERVER, &store, now), vec![A]);
            now += Duration::from_secs(5);
        }

        pending.mark(1, A, 5, now);
        assert!(
            pending.worth_blocking(now),
            "five clean edits are five answers, not five silences"
        );
    }

    #[test]
    fn a_restart_forgets_what_the_old_server_was_asked() {
        let mut pending = PendingEdits::default();
        let now = Instant::now();
        pending.mark(1, A, 7, now);

        pending.mark(2, B, 0, now);
        assert!(!pending.contains(A), "the fresh server was never asked");
        assert!(pending.contains(B));
        assert_eq!(pending.lifecycle_id(), 2);
    }
}
