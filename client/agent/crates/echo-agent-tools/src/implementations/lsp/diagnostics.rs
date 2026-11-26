//! The per-server diagnostics store.
//!
//! Diagnostics reach us two ways — pushed by the server via
//! `textDocument/publishDiagnostics`, or pulled by us via
//! `textDocument/diagnostic` — and both land here, so the rest of the code has
//! one place to read from.
//!
//! Every answer says which document version it describes. That is what makes
//! "has the server given a verdict on the edit I just sent?" a comparison
//! rather than a guess. Without it, the presence of an entry means only "the
//! server said something about this file at some point", which cannot tell a
//! file that is genuinely clean now from one that was clean before the edit
//! that broke it.
//!
//! The version comes from the protocol wherever the protocol provides one: a
//! pull knows which revision it asked about, and a pushed report may carry the
//! version it was computed for. Only a push that omits it falls back to
//! arrival order — it is credited with the newest version we had sent when it
//! arrived.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use async_lsp::lsp_types::Diagnostic;

/// The server's latest word on one document.
///
/// The items and the `result_id` that names them are one value on purpose. An
/// id is a promise that what it names is what a reader would find, and a
/// promise kept by remembering to update two containers together is a promise
/// that eventually gets broken. Here there is nothing to keep in step: an
/// answer the store turns away takes its id with it.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    pub items: Vec<Diagnostic>,
    /// The document version this verdict describes.
    pub covers: i32,
    /// Result id from a pull, valid only for the `items` beside it. Sending it
    /// back lets the server reply "unchanged" instead of recomputing.
    pub result_id: Option<String>,
}

impl Answer {
    pub fn new(items: Vec<Diagnostic>, covers: i32, result_id: Option<String>) -> Self {
        Self {
            items,
            covers,
            result_id,
        }
    }
}

/// The version of a document the server cannot have a verdict on, because we
/// have never told it about one. Real documents start at
/// [`super::documents::FIRST_VERSION`].
pub const NO_VERSION: i32 = 0;

/// Diagnostics for every document one server has reported on.
///
/// Cheap to clone (shared handle) so the pull tasks, the router and the manager
/// can each hold one. Lock poisoning is recovered from in one place rather than
/// being spelled differently at each call site: a panicking writer leaves the
/// map structurally intact, and stale diagnostics beat no diagnostics.
#[derive(Debug, Clone, Default)]
pub struct DiagnosticsStore {
    inner: Arc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    documents: RwLock<HashMap<String, Answer>>,
    publishes: AtomicBool,
}

impl DiagnosticsStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether this server publishes diagnostics of its own accord.
    ///
    /// Set by the first `publishDiagnostics` to arrive and never cleared. It
    /// decides whether to ask the server for diagnostics as well, and the
    /// answer is no: a server with a push channel is telling us how it reports,
    /// and its answer to a pull may be only part of what it knows.
    /// rust-analyzer is the case in point — it answers
    /// `textDocument/diagnostic` with its own analysis and *deliberately* does
    /// not include `cargo check` results there, publishing those instead. Take
    /// the pull answer as the whole picture and every clippy and type error in
    /// the crate disappears.
    pub fn server_publishes(&self) -> bool {
        self.inner.publishes.load(Ordering::Acquire)
    }

    /// Write `answer` down unless what we hold is newer. Returns whether it
    /// landed.
    ///
    /// This is the only rule in the store, and every write goes through it.
    /// Two things fall out of it that used to be maintained by hand:
    ///
    /// - an answer about superseded text cannot erase a newer one, so a
    ///   mid-analysis blank that arrives late is harmless;
    /// - an answer that did not land leaves no `result_id` behind, because the
    ///   id is part of the value that did not land.
    pub fn install(&self, uri: &str, answer: Answer) -> bool {
        self.install_if(uri, answer, || true)
    }

    /// The same, for a writer whose answer may have been overtaken by
    /// something the store cannot see — a refresh, or the server revealing
    /// that it publishes.
    ///
    /// `still_wanted` is evaluated under the same lock that installs, so
    /// nothing can slip between deciding to write and writing. Checking first
    /// and writing second leaves a gap in which a `forget` is undone or a
    /// fuller report is replaced by a thinner one.
    ///
    /// It must not touch the store, or it will deadlock; the flags it reads
    /// are atomics for that reason.
    pub fn install_if(
        &self,
        uri: &str,
        answer: Answer,
        still_wanted: impl FnOnce() -> bool,
    ) -> bool {
        let mut documents = self.write();
        if let Some(held) = documents.get(uri)
            && held.covers > answer.covers
        {
            return false;
        }
        if !still_wanted() {
            return false;
        }
        documents.insert(uri.to_string(), answer);
        true
    }

    /// Record a pushed report.
    ///
    /// `reported` is the version the server said it analyzed, and
    /// `latest_sent` the newest version we have sent it. A server that names a
    /// version is taken at its word; one that does not is credited with the
    /// text it had most recently been given, which is all arrival order can
    /// tell us.
    pub fn record_push(
        &self,
        uri: &str,
        items: Vec<Diagnostic>,
        reported: Option<i32>,
        latest_sent: Option<i32>,
    ) -> bool {
        self.inner.publishes.store(true, Ordering::Release);
        let covers = match (reported, latest_sent) {
            // Never above what we sent. A server naming a version we never gave
            // it — its own numbering, or a counter left over from a previous
            // connection — would otherwise set a bar no later answer could
            // clear, freezing that file's diagnostics for the session.
            (Some(reported), Some(sent)) => reported.min(sent),
            (None, Some(sent)) => sent,
            // We have told this server nothing about the document, so nothing
            // it says can be a verdict on text of ours.
            (_, None) => NO_VERSION,
        };
        // A push replaces the whole set for the document, and carries no id.
        self.install(uri, Answer::new(items, covers, None))
    }

    /// Record that the server stands by its previous answer for `uri`, as an
    /// `unchanged` pull report does: same items, but a verdict on newer text.
    ///
    /// Nothing to stand by means nothing to record — the id we sent named an
    /// answer that has since been forgotten.
    pub fn confirm_unchanged(
        &self,
        uri: &str,
        covers: i32,
        result_id: String,
        still_wanted: impl FnOnce() -> bool,
    ) -> bool {
        let Some(held) = self.answer(uri) else {
            return false;
        };
        self.install_if(
            uri,
            Answer::new(held.items, covers, Some(result_id)),
            still_wanted,
        )
    }

    /// The version of the latest answer for `uri`, or `None` if the server has
    /// never answered for it.
    pub fn covers(&self, uri: &str) -> Option<i32> {
        self.read().get(uri).map(|answer| answer.covers)
    }

    /// Whether the server has given a verdict on `uri` at `version` or later.
    ///
    /// "No problems" is a verdict like any other. Conflating it with silence is
    /// what makes a clean file wait forever for an answer it has already had.
    pub fn answered_for(&self, uri: &str, version: i32) -> bool {
        self.covers(uri).is_some_and(|covers| covers >= version)
    }

    /// The whole of the latest answer for `uri`, items and id together.
    pub fn answer(&self, uri: &str) -> Option<Answer> {
        self.read().get(uri).cloned()
    }

    /// The latest diagnostics for `uri`; empty both when the server has
    /// answered "clean" and when it has not answered at all. Callers that need
    /// to tell those apart use [`Self::answered_for`].
    pub fn items(&self, uri: &str) -> Vec<Diagnostic> {
        self.read()
            .get(uri)
            .map(|answer| answer.items.clone())
            .unwrap_or_default()
    }

    /// Forget a document, for when it is closed.
    pub fn forget(&self, uri: &str) {
        self.write().remove(uri);
    }

    fn read(&self) -> RwLockReadGuard<'_, HashMap<String, Answer>> {
        self.inner
            .documents
            .read()
            .unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> RwLockWriteGuard<'_, HashMap<String, Answer>> {
        self.inner
            .documents
            .write()
            .unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "file:///a.cs";

    fn diagnostic(message: &str) -> Diagnostic {
        Diagnostic {
            message: message.to_string(),
            ..Default::default()
        }
    }

    fn reported(message: &str, covers: i32) -> Answer {
        Answer::new(vec![diagnostic(message)], covers, None)
    }

    #[test]
    fn an_unanswered_document_has_no_verdict() {
        let store = DiagnosticsStore::new();
        assert_eq!(store.covers(A), None);
        assert!(!store.answered_for(A, 0));
        assert!(store.items(A).is_empty());
    }

    #[test]
    fn a_clean_answer_is_still_an_answer() {
        let store = DiagnosticsStore::new();
        assert!(store.install(A, Answer::new(vec![], 0, None)));
        assert!(
            store.answered_for(A, 0),
            "an empty report is the server saying the file is clean"
        );
        assert!(store.items(A).is_empty());
    }

    #[test]
    fn an_answer_from_before_the_edit_does_not_count_as_a_reply_to_it() {
        let store = DiagnosticsStore::new();
        store.install(A, Answer::new(vec![], 0, None));
        assert!(
            !store.answered_for(A, 1),
            "the pre-edit clean report must not be mistaken for a post-edit one"
        );

        store.install(A, reported("boom", 1));
        assert!(store.answered_for(A, 1));
        assert_eq!(store.items(A).len(), 1);
    }

    #[test]
    fn a_late_answer_does_not_overwrite_a_newer_one() {
        let store = DiagnosticsStore::new();
        assert!(store.install(A, reported("the current truth", 2)));
        assert!(
            !store.install(A, reported("the late pull", 1)),
            "an answer about text the server has since been sent a replacement for"
        );
        assert_eq!(store.items(A)[0].message, "the current truth");
        assert_eq!(store.covers(A), Some(2));
    }

    /// A stale answer written down anyway would erase the errors it should
    /// have deferred to, and the next pull would then have nothing to lose and
    /// believe the first mid-analysis blank it was given.
    #[test]
    fn an_overtaken_empty_answer_does_not_erase_what_we_know() {
        let store = DiagnosticsStore::new();
        store.install(A, reported("boom", 3));

        assert!(!store.install(A, Answer::new(vec![], 2, None)));
        assert_eq!(store.items(A).len(), 1, "the real error stands");
        assert_eq!(store.covers(A), Some(3));
    }

    /// The id and the items are one value, so an answer the store turns away
    /// cannot leave its id behind to be sent back later — which is how a file
    /// that had been fixed went on reporting its old errors.
    #[test]
    fn an_answer_the_store_refused_leaves_no_result_id_behind() {
        let store = DiagnosticsStore::new();
        store.install(A, Answer::new(vec![diagnostic("boom")], 3, None));

        assert!(!store.install(A, Answer::new(vec![], 2, Some("stale-id".into()))));
        assert_eq!(
            store.answer(A).and_then(|answer| answer.result_id),
            None,
            "the id belonged to the answer that did not land"
        );
    }

    #[test]
    fn unchanged_refreshes_the_verdict_without_losing_the_diagnostics() {
        let store = DiagnosticsStore::new();
        store.install(
            A,
            Answer::new(vec![diagnostic("boom")], 1, Some("id-1".into())),
        );

        assert!(store.confirm_unchanged(A, 2, "id-2".into(), || true));
        assert!(
            store.answered_for(A, 2),
            "standing by an answer is a verdict on the newer text"
        );
        assert_eq!(store.items(A).len(), 1, "and it keeps what it stands by");
        assert_eq!(
            store.answer(A).and_then(|answer| answer.result_id),
            Some("id-2".into())
        );
    }

    #[test]
    fn an_overtaken_unchanged_reply_does_not_reply_either() {
        let store = DiagnosticsStore::new();
        store.install(A, Answer::new(vec![diagnostic("boom")], 3, None));

        assert!(!store.confirm_unchanged(A, 2, "stale-id".into(), || true));
        assert!(!store.answered_for(A, 4));
        assert_eq!(store.covers(A), Some(3));
    }

    #[test]
    fn standing_by_an_answer_we_no_longer_hold_records_nothing() {
        let store = DiagnosticsStore::new();
        assert!(!store.confirm_unchanged(A, 1, "id".into(), || true));
        assert_eq!(store.covers(A), None);
    }

    #[test]
    fn a_push_naming_its_version_is_taken_at_its_word() {
        let store = DiagnosticsStore::new();
        // The server is one revision behind: we are at 4, it analyzed 3.
        store.record_push(A, vec![diagnostic("boom")], Some(3), Some(4));

        assert!(store.answered_for(A, 3));
        assert!(
            !store.answered_for(A, 4),
            "a verdict on the previous revision does not settle the newest edit"
        );
    }

    #[test]
    fn a_push_without_a_version_is_credited_with_the_text_the_server_has() {
        let store = DiagnosticsStore::new();
        store.record_push(A, vec![diagnostic("boom")], None, Some(4));

        assert!(
            store.answered_for(A, 4),
            "arrival order is all a versionless report gives us"
        );
    }

    #[test]
    fn a_push_for_a_document_we_never_opened_is_still_kept() {
        let store = DiagnosticsStore::new();
        store.record_push(A, vec![diagnostic("boom")], None, None);
        assert_eq!(store.items(A).len(), 1);
    }

    /// The writer's own reason to change its mind is checked under the lock
    /// that installs, so there is no gap for a refresh or a push to fall into.
    #[test]
    fn an_answer_its_writer_no_longer_wants_does_not_land() {
        let store = DiagnosticsStore::new();
        store.install(A, reported("boom", 1));

        assert!(!store.install_if(A, Answer::new(vec![], 2, None), || false));
        assert_eq!(store.items(A).len(), 1);
        assert_eq!(store.covers(A), Some(1));

        assert!(store.install_if(A, Answer::new(vec![], 2, None), || true));
        assert!(store.items(A).is_empty());
    }

    #[test]
    fn forgetting_a_document_forgets_its_verdict() {
        let store = DiagnosticsStore::new();
        store.install(A, reported("boom", 1));
        store.forget(A);
        assert_eq!(store.covers(A), None);
    }
}
