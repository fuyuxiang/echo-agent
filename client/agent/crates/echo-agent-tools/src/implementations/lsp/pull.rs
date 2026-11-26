//! Pull-model diagnostics (`textDocument/diagnostic`).
//!
//! Most servers push diagnostics at us. Some — Roslyn among them — never
//! publish anything and only answer when asked, so without pulling we would see
//! no C# diagnostics at all.
//!
//! The split here is deliberate: [`PullDiagnostics::request`] performs exactly
//! one round trip and reports what came back as a [`PullOutcome`], while
//! [`PullDiagnostics::resolve`] decides what to do about it. Keeping the two
//! apart is what lets the transport be tested without a server, and what keeps
//! the one judgement call, [`CONFIRM_DELAY`], in one readable place.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use async_lsp::LanguageServer;
use async_lsp::lsp_types::{
    Diagnostic, DocumentDiagnosticParams, DocumentDiagnosticReport, DocumentDiagnosticReportResult,
    TextDocumentIdentifier, Url,
};

use super::DiagnosticsNotify;
use super::diagnostics::{Answer, DiagnosticsStore};
use super::documents::Documents;

/// How long to wait for a pull-diagnostics response before giving up on it.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Delay before asking a second time, when an empty answer is not yet worth
/// believing.
///
/// This deliberately runs past [`super::DIAGNOSTICS_DRAIN_TIMEOUT`], and only
/// ever does so on the path where the answer would erase. Being a turn late to
/// say "it's fixed now" costs the reader nothing — the previous, still-accurate
/// errors stay on screen in the meantime. The path that has nothing to lose
/// does not wait at all, so a first answer, and any answer that reports
/// problems, still lands inside the drain's budget.
const CONFIRM_DELAY: std::time::Duration = std::time::Duration::from_millis(600);

// Not an accident, and not a number to "fix" by shrinking: a server needs
// longer than the drain budget to re-analyze, so confirming that a broken file
// is now clean cannot happen within it. Stated here so that shrinking it below
// the budget — which would look like an optimisation — fails the build instead.
const _: () = assert!(CONFIRM_DELAY.as_millis() > super::DIAGNOSTICS_DRAIN_TIMEOUT.as_millis());

/// Whether a server answers `textDocument/diagnostic`.
///
/// The advertised capability is not enough on its own: Roslyn implements the
/// handler but, depending on the build, advertises no diagnostic provider at
/// all. So a server that did not advertise one is still asked, and only its own
/// rejection stops us asking again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullSupport {
    /// Worth asking — either the server advertised a provider, or it has not
    /// told us otherwise.
    Asking,
    /// The server answered `MethodNotFound`. Conclusive, and the only thing
    /// that is.
    Rejected,
}

/// [`PullSupport`] shared between the client and its detached pull tasks.
///
/// Only `MethodNotFound` writes a server off; timeouts do not, since concurrent
/// pulls would spend any such budget at once on a single slow episode.
#[derive(Debug)]
struct SupportFlag {
    rejected: AtomicBool,
}

impl SupportFlag {
    fn new() -> Self {
        Self {
            rejected: AtomicBool::new(false),
        }
    }

    fn get(&self) -> PullSupport {
        if self.rejected.load(Ordering::Relaxed) {
            PullSupport::Rejected
        } else {
            PullSupport::Asking
        }
    }

    /// The server said it does not implement the request.
    fn write_off(&self) {
        self.rejected.store(true, Ordering::Relaxed);
    }
}

/// What one `textDocument/diagnostic` round trip came back with.
#[derive(Debug, PartialEq)]
enum PullOutcome {
    /// The server reported problems.
    Reported {
        items: Vec<Diagnostic>,
        result_id: Option<String>,
    },
    /// The server answered, and had nothing to report.
    Clean { result_id: Option<String> },
    /// The server stands by the answer we told it we already have.
    Unchanged { result_id: String },
    /// The server does not implement pull diagnostics.
    Unsupported,
    /// The request failed, timed out, or returned a partial result we did not
    /// ask for. Nothing to write down.
    Failed,
}

/// Pull-diagnostics state for one server connection.
///
/// Cheap to clone: every field is a shared handle, so a detached task takes a
/// whole clone rather than five separate ones.
#[derive(Clone)]
pub struct PullDiagnostics {
    server_name: Arc<str>,
    socket: async_lsp::ServerSocket,
    store: DiagnosticsStore,
    documents: Documents,
    notify: DiagnosticsNotify,
    support: Arc<SupportFlag>,
    in_flight: Arc<std::sync::Mutex<InFlight>>,
    /// Whether this server has ever answered a pull.
    ///
    /// Until it has, we do not know what kind of server it is. It may be
    /// pull-only, like Roslyn, or it may be one that publishes and has simply
    /// not had anything to publish yet — and those want opposite treatment.
    /// One way, and never cleared.
    answered_a_pull: Arc<AtomicBool>,
    /// Bumped when the server says its answers no longer describe the code.
    ///
    /// A document version cannot express this: the text did not change, the
    /// server's knowledge of it did. Without it, a pull already in flight when
    /// the refresh arrives comes back with the very answer the server has just
    /// disowned, and — being about the current version — passes for a reply to
    /// the question the refresh re-opened.
    generation: Arc<AtomicU64>,
}

/// Which documents have a pull running, and which were edited again while it
/// ran. Keeps one task per document instead of one per edit.
#[derive(Debug, Default)]
struct InFlight {
    running: HashSet<String>,
    superseded: HashSet<String>,
}

impl PullDiagnostics {
    pub fn new(
        server_name: &str,
        socket: async_lsp::ServerSocket,
        store: DiagnosticsStore,
        documents: Documents,
        notify: DiagnosticsNotify,
    ) -> Self {
        Self {
            server_name: Arc::from(server_name),
            socket,
            store,
            documents,
            notify,
            support: Arc::new(SupportFlag::new()),
            in_flight: Arc::new(std::sync::Mutex::new(InFlight::default())),
            answered_a_pull: Arc::new(AtomicBool::new(false)),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn support(&self) -> PullSupport {
        self.support.get()
    }

    /// Ask the server for `uri`'s diagnostics and fold the answer into the
    /// store, so pulled and pushed diagnostics reach readers by the same path.
    ///
    /// Detached, because the callers are synchronous notification paths that
    /// must not block on a server round trip. At most one task per document:
    /// an edit arriving while a pull is running re-runs it once at the end
    /// rather than racing a second task against the first.
    ///
    /// Returns whether the document will get an answer — `false` only when
    /// this server is not one we ask. A pull already running counts as `true`:
    /// it has been told to run once more, so the question does get put.
    pub fn will_answer(&self, uri: Url) -> bool {
        if !self.worth_asking() {
            return false;
        }
        let key = uri.to_string();
        if !self.begin(&key) {
            // Already running, and now queued to run again — the caller's
            // question will be answered, just not by a task of its own.
            return true;
        }

        let pull = self.clone();
        tokio::spawn(async move {
            loop {
                pull.resolve(&uri, &key).await;
                if !pull.finish(&key) {
                    break;
                }
            }
        });
        true
    }

    /// Whether asking this server is the right way to learn what it thinks.
    ///
    /// No, if it has told us it does not implement the request, and no if it
    /// publishes: a server with a push channel has said how it reports, and
    /// what it returns from a pull may be only part of it. Both are one-way —
    /// once either is true it stays true — so this cannot oscillate.
    fn worth_asking(&self) -> bool {
        self.support.get() == PullSupport::Asking && !self.store.server_publishes()
    }

    /// Re-ask about every open document, for when the server says its answers
    /// have changed. Coalescing means a burst of these costs one pull per
    /// document, however many arrive.
    ///
    /// What the server said before is forgotten first. It has just told us
    /// those answers are out of date, and keeping one would both misreport it
    /// as current and make the re-pull look like it had already been answered.
    /// Returns whether anything was asked.
    pub fn refresh_all(&self) -> bool {
        if !self.worth_asking() {
            // Nothing we can do about it, so nothing is thrown away either. A
            // server we do not ask keeps whatever it has already told us —
            // discarding that would leave the reader with nothing at all.
            tracing::debug!(server = %self.server_name, "server asked for a diagnostics refresh, but it is not one we pull from");
            return false;
        }
        // Everything computed before this point is now the server's own old
        // news. Bumped before anything is forgotten or asked, so no answer can
        // slip between the two and be kept.
        self.generation.fetch_add(1, Ordering::AcqRel);

        let mut asked = false;
        for uri in self.documents.uris() {
            let Ok(parsed) = Url::parse(&uri) else {
                tracing::debug!(server = %self.server_name, %uri, "cannot re-pull an unparseable uri");
                continue;
            };
            if !self.will_answer(parsed) {
                continue;
            }
            // Forgotten only once the replacement is on its way. What the
            // server has disowned is worse than nothing — reported as current
            // it is wrong, and left in place it makes the re-pull look like it
            // has already been answered — but throwing it away with no
            // replacement coming would just blind the reader.
            self.store.forget(&uri);
            asked = true;
        }
        asked
    }

    /// Ask, and write down what comes back.
    ///
    /// The one judgement call in this module lives here. A server answers
    /// before it has finished re-analyzing the change we just sent: Roslyn will
    /// return an empty report a few hundred milliseconds after an edit to a
    /// file it is still working on. Believing that erases real errors and tells
    /// the reader its problem is fixed, so when there is something to lose the
    /// answer has to be given twice. When there is nothing to lose — no
    /// diagnostics held for this document — the first answer is taken at once,
    /// so "nothing wrong here" still lands inside the drain's budget.
    ///
    /// This is the only guess in the diagnostics path. The mechanism that makes
    /// it merely a matter of latency rather than of correctness is
    /// [`super::refresh`]: when the server finishes analyzing, it says so, and
    /// everything is asked again.
    async fn resolve(&self, uri: &Url, key: &str) {
        // The revision we are asking about, read just before the request goes
        // out and used to describe the answer. `textDocument/diagnostic`
        // carries no version, so an answer to a request that predates an edit
        // may or may not have taken that edit into account — and one that
        // cannot be shown to postdate it must not settle it. Being wrong that
        // way costs one more round trip; being wrong the other way reports
        // diagnostics for text that no longer exists.
        let Some(asked) = self.documents.version(key) else {
            return;
        };

        // What the server knew when we asked. An answer from before a refresh
        // describes code the server has since disowned; the re-ask that the
        // refresh queued is the one worth listening to.
        let generation = self.generation.load(Ordering::Acquire);
        // Two reasons not to believe the first "nothing to report" we are
        // given, both answered by asking again a moment later.
        //
        // The first is that the server may not have finished re-analyzing the
        // change we just sent, and erasing errors it is about to re-report
        // tells the reader its problem is fixed when it is not.
        //
        // The second is that we may not yet know what kind of server this is.
        // A server that publishes keeps some of what it knows on that channel —
        // rust-analyzer leaves `cargo check` there — so its pull answer is a
        // part rather than the whole, and its first publish is what tells us
        // so. The wait gives that publish a chance to arrive; if it does, this
        // answer is dropped as the partial view it was.
        let mut confirming = self
            .store
            .answer(key)
            .is_some_and(|answer| !answer.items.is_empty())
            || (!self.answered_a_pull.load(Ordering::Acquire) && !self.store.server_publishes());

        loop {
            // Read afresh each time round: the id is only worth sending while
            // it names what the store holds, and the wait below is long enough
            // for that to have changed.
            let previous_result_id = self.store.answer(key).and_then(|held| held.result_id);
            match self.request(uri, previous_result_id).await {
                PullOutcome::Reported { items, result_id } => {
                    self.note_answered();
                    tracing::debug!(
                        server = %self.server_name, uri = %key, count = items.len(),
                        "diagnostic pull returned"
                    );
                    self.commit(key, Answer::new(items, asked, result_id), generation);
                    return;
                }
                PullOutcome::Unchanged { result_id } => {
                    self.note_answered();
                    // Safe to take at face value: the id we sent came out of
                    // the store, beside the answer it names.
                    if self.store.confirm_unchanged(key, asked, result_id, || {
                        !self.disowned(key, generation)
                    }) {
                        self.notify.notify_one();
                    }
                    return;
                }
                PullOutcome::Clean { result_id } => {
                    self.note_answered();
                    if confirming {
                        confirming = false;
                        tokio::time::sleep(CONFIRM_DELAY).await;
                        continue;
                    }
                    // An empty answer about text that has since been replaced
                    // is the weakest evidence there is: old text, and nothing
                    // to report about it. Writing it down would erase errors
                    // that may well still be there — and worse, leave the
                    // re-pull with nothing to protect, so it would believe the
                    // first premature blank it was given. The re-ask is
                    // already queued; this answer has nothing to add to it.
                    if self.documents.version(key) != Some(asked) {
                        tracing::debug!(
                            server = %self.server_name, uri = %key, asked,
                            "clean answer is about text that has since been replaced; leaving what we hold"
                        );
                        return;
                    }
                    tracing::debug!(
                        server = %self.server_name, uri = %key,
                        "diagnostic pull returned nothing"
                    );
                    self.commit(key, Answer::new(Vec::new(), asked, result_id), generation);
                    return;
                }
                PullOutcome::Unsupported | PullOutcome::Failed => return,
            }
        }
    }

    /// One round trip. Updates the support flag from what the server does, but
    /// writes no diagnostics — that is [`Self::resolve`]'s job.
    async fn request(&self, uri: &Url, previous_result_id: Option<String>) -> PullOutcome {
        let params = DocumentDiagnosticParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            // No identifier: servers that split diagnostics across several
            // sources merge them into one report for us.
            identifier: None,
            previous_result_id,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let mut socket = self.socket.clone();
        let response = match tokio::time::timeout(
            REQUEST_TIMEOUT,
            socket.document_diagnostic(params),
        )
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(e)) => {
                // A server that does not implement pull diagnostics should
                // only ever be asked once.
                if matches!(
                    &e,
                    async_lsp::Error::Response(r)
                        if r.code == async_lsp::ErrorCode::METHOD_NOT_FOUND
                ) {
                    self.support.write_off();
                    tracing::debug!(server = %self.server_name, "server has no pull diagnostics; not asking again");
                    return PullOutcome::Unsupported;
                }
                tracing::debug!(server = %self.server_name, error = %e, "diagnostic pull failed");
                return PullOutcome::Failed;
            }
            Err(_) => {
                // Slow is not the same as absent: a server still loading a
                // solution is asked again on the next edit, and told us so
                // itself when it finishes.
                tracing::debug!(server = %self.server_name, uri = %uri, "diagnostic pull timed out");
                return PullOutcome::Failed;
            }
        };

        let DocumentDiagnosticReportResult::Report(report) = response else {
            // Partial results only arrive via `$/progress`, which we do not
            // request, so there is nothing to fold in.
            return PullOutcome::Failed;
        };

        match report {
            DocumentDiagnosticReport::Full(full) => {
                let report = full.full_document_diagnostic_report;
                if report.items.is_empty() {
                    PullOutcome::Clean {
                        result_id: report.result_id,
                    }
                } else {
                    PullOutcome::Reported {
                        items: report.items,
                        result_id: report.result_id,
                    }
                }
            }
            DocumentDiagnosticReport::Unchanged(unchanged) => PullOutcome::Unchanged {
                result_id: unchanged.unchanged_document_diagnostic_report.result_id,
            },
        }
    }

    /// Record that the server has answered a pull, and report whether that was
    /// the first time. Answering is what tells us it is a server we can ask.
    fn note_answered(&self) -> bool {
        !self.answered_a_pull.swap(true, Ordering::AcqRel)
    }

    /// Whether the server has disowned everything it knew when this pull was
    /// sent. The re-ask is already queued — `refresh_all` goes through
    /// `will_answer`, which marks a running document for one more round.
    fn disowned(&self, key: &str, generation: u64) -> bool {
        if self.store.server_publishes() {
            // It revealed itself while we were waiting. Its own reports are the
            // whole picture; ours may be a slice of it, and writing that down
            // would replace the picture with the slice.
            tracing::debug!(
                server = %self.server_name, uri = %key,
                "server publishes; discarding the answer to a pull we should not have sent"
            );
            return true;
        }
        if self.generation.load(Ordering::Acquire) == generation {
            return false;
        }
        tracing::debug!(
            server = %self.server_name, uri = %key,
            "pull answered from before the server disowned its answers; asking again"
        );
        true
    }

    /// Write an answer down and wake the readers if it landed.
    fn commit(&self, key: &str, answer: Answer, generation: u64) {
        if self
            .store
            .install_if(key, answer, || !self.disowned(key, generation))
        {
            self.notify.notify_one();
        } else {
            // The re-pull for the newer text is already queued: the edit that
            // overtook us went through `spawn`, which either marked this
            // document superseded or started a fresh pull.
            tracing::debug!(
                server = %self.server_name, uri = %key,
                "pull answered about text that has since been replaced; a newer answer stands"
            );
        }
    }

    /// Claim the pull slot for `key`. `false` means one is already running, and
    /// has been told to run again when it finishes.
    fn begin(&self, key: &str) -> bool {
        let mut in_flight = self.in_flight.lock().unwrap_or_else(|e| e.into_inner());
        if in_flight.running.contains(key) {
            in_flight.superseded.insert(key.to_string());
            return false;
        }
        in_flight.running.insert(key.to_string());
        true
    }

    /// Release the pull slot. `true` means the document was edited again while
    /// the pull ran, so it is worth one more round.
    fn finish(&self, key: &str) -> bool {
        let mut in_flight = self.in_flight.lock().unwrap_or_else(|e| e.into_inner());
        if in_flight.superseded.remove(key) {
            return true;
        }
        in_flight.running.remove(key);
        false
    }
}

impl std::fmt::Debug for PullDiagnostics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PullDiagnostics")
            .field("server_name", &self.server_name)
            .field("support", &self.support.get())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_is_asked_until_it_says_no() {
        let flag = SupportFlag::new();
        assert_eq!(flag.get(), PullSupport::Asking);
        flag.write_off();
        assert_eq!(flag.get(), PullSupport::Rejected);
    }

    fn detached_pull() -> PullDiagnostics {
        PullDiagnostics::new(
            "test",
            async_lsp::MainLoop::new_client(|_| async_lsp::router::Router::new(())).1,
            DiagnosticsStore::new(),
            Documents::new(),
            Arc::new(tokio::sync::Notify::new()),
        )
    }

    /// The case that made this counter necessary: a pull already in flight when
    /// the refresh lands comes back with exactly the answer the server has just
    /// disowned. It is about the current document version, so nothing about the
    /// text can tell it apart — only the fact that the server changed its mind
    /// while we were waiting.
    #[tokio::test]
    async fn an_answer_from_before_a_refresh_is_not_written_down() {
        let pull = detached_pull();
        pull.documents
            .commit("file:///a.cs", 0, "csharp", Default::default());

        let sent_before = pull.generation.load(Ordering::Acquire);
        pull.refresh_all();

        pull.commit(
            "file:///a.cs",
            Answer::new(vec![Diagnostic::default()], 0, None),
            sent_before,
        );
        assert_eq!(
            pull.store.covers("file:///a.cs"),
            None,
            "the answer the server disowned must not stand as its verdict"
        );

        // The re-ask that the refresh queued is heard.
        let now = pull.generation.load(Ordering::Acquire);
        pull.commit(
            "file:///a.cs",
            Answer::new(vec![Diagnostic::default()], 0, None),
            now,
        );
        assert_eq!(pull.store.covers("file:///a.cs"), Some(0));
    }

    #[test]
    fn a_document_with_a_pull_running_is_queued_rather_than_raced() {
        let pull = detached_pull();

        assert!(pull.begin("file:///a.cs"), "the first caller runs");
        assert!(
            !pull.begin("file:///a.cs"),
            "the second is queued behind it"
        );
        assert!(pull.finish("file:///a.cs"), "and asked for one more round");
        assert!(!pull.finish("file:///a.cs"), "which is the last");
        assert!(pull.begin("file:///a.cs"), "the slot is free again");
    }
}
