//! Full-text search over scrollback text, scanned on a background thread.
//!
//! [`ScrollbackSearchIndex`] caches each entry's [`searchable_text`] (rendered
//! plain text for markdown blocks, stored source for the rest) keyed by the
//! scrollback's `content_generation`. It holds one owned `String` per entry
//! (never a per-rendered-line `Vec`), so memory tracks total searchable-text
//! size rather than rendered-line count.
//!
//! The cache is shared as an `Arc<[IndexedEntry]>` so it can be handed to a
//! background [`SearchDaemon`] without re-cloning the strings. The daemon runs
//! the regex scan off the input thread: query mutations only enqueue the latest
//! corpus and query (O(1) on the UI thread), and
//! [`ScrollbackSearchState::poll`] picks up results once the scan completes.
//! This keeps per-keystroke typing responsive on long sessions where a
//! synchronous scan would stall the input thread.
//!
//! [`searchable_text`]: super::block::RenderBlock::searchable_text

use std::ops::Range;
use std::sync::{
    Arc, Mutex,
    mpsc::{Receiver, Sender, channel},
};
use std::thread::{self, JoinHandle};

use crossterm::event::KeyEvent;

use super::entry::EntryId;
use super::state::ScrollbackState;
use crate::input::line_editor::{LineEditOutcome, LineEditor};
use crate::search::{QueryKind, TextMatcher};

/// A located query match within the scrollback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrollbackMatch {
    /// Entry the match was found in.
    pub entry_id: EntryId,
    /// Zero-based line index of the match within the entry's searchable text.
    pub line_in_entry: usize,
    /// Byte range within the entry's searchable text; distinguishes matches
    /// that share a `line_in_entry`.
    pub byte_range: Range<usize>,
}

/// Per-entry cache of searchable source text plus a query scan over it.
///
/// [`sync`](Self::sync) rebuilds the cache only when scrollback content changes;
/// [`find`](Self::find) scans the cache without touching the scrollback. The
/// cache is stored as an `Arc<[IndexedEntry]>` so [`entries_arc`](Self::entries_arc)
/// can hand it to the background daemon with a cheap pointer clone.
#[derive(Debug, Default)]
pub struct ScrollbackSearchIndex {
    entries: Arc<[IndexedEntry]>,
    /// `content_generation` the cache was built from; `None` before first sync.
    built_generation: Option<u64>,
}

#[derive(Debug)]
struct IndexedEntry {
    id: EntryId,
    text: String,
}

impl ScrollbackSearchIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild the cached text when scrollback content changed since the last
    /// sync, returning `true` if it rebuilt and `false` on the no-op early
    /// return. A no-op when `content_generation` is unchanged, so scrolling and
    /// viewport changes never trigger work.
    ///
    /// Re-derives every entry's source text wholesale, and content changes
    /// (e.g. streaming) bump the key often, so callers should sync on query
    /// change or search open — not every frame. Per-entry incremental updates
    /// are left until profiling on large sessions calls for them.
    pub fn sync(&mut self, state: &ScrollbackState) -> bool {
        if self.built_generation == Some(state.content_generation()) {
            return false;
        }
        self.entries = state
            .iter_entries()
            .filter_map(|(id, entry)| {
                entry
                    .block
                    .searchable_text()
                    .map(|text| IndexedEntry { id, text })
            })
            .collect::<Vec<_>>()
            .into();
        self.built_generation = Some(state.content_generation());
        true
    }

    /// A cheap pointer clone of the cached corpus, for handing to the daemon.
    fn entries_arc(&self) -> Arc<[IndexedEntry]> {
        self.entries.clone()
    }

    /// All matches for `matcher`, in scrollback order. Call [`sync`](Self::sync)
    /// first so the cache reflects current content.
    ///
    /// Retained for the benchmark and unit tests; the production path scans on
    /// the daemon thread via [`scan_matches`].
    ///
    /// An empty query yields nothing (an empty pattern would otherwise match at
    /// every byte); zero-width matches are skipped for the same reason.
    pub fn find(&self, matcher: &TextMatcher) -> Vec<ScrollbackMatch> {
        scan_matches(&self.entries, matcher)
    }
}

/// Scan `entries` for every match of `matcher`, in scrollback order.
///
/// Shared by the synchronous [`ScrollbackSearchIndex::find`] and the background
/// [`SearchDaemon`]. An empty query yields nothing (an empty pattern would
/// otherwise match at every byte); zero-width matches are skipped for the same
/// reason.
fn scan_matches(entries: &[IndexedEntry], matcher: &TextMatcher) -> Vec<ScrollbackMatch> {
    if matcher.query().is_empty() {
        return Vec::new();
    }
    let regex = matcher.compiled_regex();
    let mut matches = Vec::new();
    for entry in entries {
        // Matches arrive in ascending byte order, so walk newlines forward
        // once per entry to label each match's line instead of rescanning.
        let mut line = 0usize;
        let mut counted_to = 0usize;
        for m in regex.find_iter(&entry.text) {
            if m.start() == m.end() {
                continue;
            }
            line += entry.text[counted_to..m.start()].matches('\n').count();
            counted_to = m.start();
            matches.push(ScrollbackMatch {
                entry_id: entry.id,
                line_in_entry: line,
                byte_range: m.range(),
            });
        }
    }
    matches
}

// ---------------------------------------------------------------------------
// Background search daemon
// ---------------------------------------------------------------------------

/// Latest scan results published by the daemon for the UI thread to pick up.
///
/// `request_generation` is assigned synchronously by the UI and identifies the
/// exact query/corpus request that produced this snapshot.
#[derive(Clone, Default, Debug)]
struct SearchSnapshot {
    matches: Arc<[ScrollbackMatch]>,
    request_generation: u64,
    /// The query these matches were computed for. `poll` compares it against
    /// the live editor and drops results for a query the user has already
    /// typed past (the scan was in flight while they kept editing), so the
    /// match count / cursor never desync from the visible query.
    query: String,
}

/// Work sent from the UI thread to the daemon.
///
/// Each keystroke is one atomic `Update` carrying the latest query plus, only
/// when content changed, the new corpus. Bundling them means the daemon can
/// never wake having seen a new corpus but not yet the matching query (a
/// split-snapshot that would publish one stale result before correcting).
enum SearchMsg {
    Update {
        /// New corpus to scan, or `None` to keep the corpus the daemon holds.
        corpus: Option<Arc<[IndexedEntry]>>,
        /// Query to scan for.
        query: String,
        /// UI-owned request identity.
        request_generation: u64,
    },
    /// Shut the daemon thread down.
    Stop,
}

/// The newest corpus and query coalesced from a burst of `Update`s.
#[derive(Default)]
struct DrainedUpdate {
    corpus: Option<Arc<[IndexedEntry]>>,
    query: Option<String>,
    request_generation: Option<u64>,
    stop: bool,
}

/// Coalesce all currently-pending messages, keeping the newest corpus and the
/// newest query so a burst of keystrokes triggers a single scan of the latest
/// query. A later `None` corpus means "unchanged" and must not clobber a corpus
/// carried by an earlier message in the burst. `Stop` always wins and ends
/// draining immediately.
fn drain_to_latest(first: SearchMsg, rx: &Receiver<SearchMsg>) -> DrainedUpdate {
    let mut out = DrainedUpdate::default();
    let mut msg = first;
    loop {
        match msg {
            SearchMsg::Update {
                corpus,
                query,
                request_generation,
            } => {
                if corpus.is_some() {
                    out.corpus = corpus;
                }
                out.query = Some(query);
                out.request_generation = Some(request_generation);
            }
            SearchMsg::Stop => {
                out.stop = true;
                return out;
            }
        }
        match rx.try_recv() {
            Ok(next) => msg = next,
            Err(_) => return out,
        }
    }
}

/// Owns the background scan thread and the shared result snapshot.
#[derive(Debug)]
struct SearchDaemon {
    shared: Arc<Mutex<SearchSnapshot>>,
    tx: Sender<SearchMsg>,
    /// Deliberately detached, never joined: closing a search must not block the
    /// UI on an in-flight scan. `Stop` plus channel-disconnect both end the
    /// thread, and the worker owns its `Arc` clones, so dropping this is safe.
    _handle: JoinHandle<()>,
}

impl SearchDaemon {
    fn new() -> Self {
        let shared = Arc::new(Mutex::new(SearchSnapshot::default()));
        // Unbounded so `update_query`'s send never blocks the input thread.
        // A bounded channel could stall the 257th keystroke on a full buffer,
        // and dropping on full (try_send) could lose the final query. Messages
        // are tiny (an Arc pointer + the query) and the daemon drains the whole
        // queue on each wakeup, so it stays short in practice.
        let (tx, rx) = channel::<SearchMsg>();

        let out = shared.clone();
        let handle = thread::spawn(move || {
            let mut corpus: Arc<[IndexedEntry]> = Arc::from([]);
            let mut query = String::new();
            while let Ok(msg) = rx.recv() {
                let update = drain_to_latest(msg, &rx);
                if update.stop {
                    break;
                }
                if let Some(new_corpus) = update.corpus {
                    corpus = new_corpus;
                }
                if let Some(new_query) = update.query {
                    query = new_query;
                }
                let Some(request_generation) = update.request_generation else {
                    continue;
                };

                // Every non-`Stop` burst carries a query, so rescan once here.
                // Compile + scan off the lock; the mutex only guards the quick
                // snapshot swap below, never the scan itself.
                let matcher = TextMatcher::new(query.as_str(), QueryKind::Regex);
                let matches: Arc<[ScrollbackMatch]> = if query.is_empty() || matcher.is_error() {
                    Arc::from([])
                } else {
                    scan_matches(&corpus, &matcher).into()
                };
                *out.lock().unwrap() = SearchSnapshot {
                    matches,
                    request_generation,
                    query: query.clone(),
                };
            }
        });

        Self {
            shared,
            tx,
            _handle: handle,
        }
    }
}

impl Drop for SearchDaemon {
    fn drop(&mut self) {
        // Best-effort: a closed channel just means the thread already exited.
        let _ = self.tx.send(SearchMsg::Stop);
    }
}

/// An interactive search session over the scrollback: owns the query editor,
/// derived matcher, cached index, background scan daemon, latest match list,
/// and a cursor into it.
///
/// Matching runs off-thread. [`update_query`](Self::update_query) only enqueues
/// the corpus and query for the daemon (it never scans); results arrive later
/// via [`poll`](Self::poll). `n` / `N` navigation ([`next`](Self::next) /
/// [`prev`](Self::prev)) stays synchronous since the match list is already in
/// hand by then.
///
/// The lifecycle has two phases. While **composing**, the user is still editing
/// the query and each edit re-queries. [`accept`](Self::accept) freezes the
/// query and switches to **browsing**. Canceling is simply dropping the state
/// (the owner holds it as an `Option`), which stops the daemon thread, so there
/// is deliberately no `cancel` method.
#[derive(Debug)]
pub struct ScrollbackSearchState {
    /// Canonical editable query and cursor.
    editor: LineEditor,
    /// Cached searchable text; re-synced lazily on content change, never per
    /// frame. Lives on the UI thread and is handed to the daemon as an `Arc`.
    index: ScrollbackSearchIndex,
    /// Compiled query derived from `editor` for highlighting and error state.
    matcher: TextMatcher,
    /// Latest matches from the daemon, in scrollback order.
    matches: Arc<[ScrollbackMatch]>,
    /// Cursor into `matches`; `None` when there are no matches.
    current: Option<usize>,
    /// `true` while the query is still being edited (search bar), `false` once
    /// accepted (browsing with `n` / `N`).
    composing: bool,
    /// Background scan thread; dropped (and stopped) when the session closes.
    daemon: SearchDaemon,
    /// Snapshot request generation last observed by `poll`.
    last_seen_generation: u64,
    /// Monotonic UI-owned request generation, incremented only when enqueuing
    /// query/corpus work.
    request_generation: u64,
}

impl ScrollbackSearchState {
    /// Open an empty session in the composing phase. No matches exist until the
    /// first [`update_query`](Self::update_query) result is [`poll`](Self::poll)ed in.
    pub fn open() -> Self {
        Self {
            editor: LineEditor::default(),
            index: ScrollbackSearchIndex::new(),
            matcher: TextMatcher::new("", QueryKind::Regex),
            matches: Arc::from([]),
            current: None,
            composing: true,
            daemon: SearchDaemon::new(),
            last_seen_generation: 0,
            request_generation: 0,
        }
    }

    /// Replace the canonical query, recompile its derived matcher, and enqueue
    /// the latest corpus/query snapshot for the background scan.
    ///
    /// The corpus is only re-synced and re-sent when scrollback content changed
    /// since the last send, so steady-state keystrokes just push a query string.
    pub fn update_query(&mut self, query: &str, state: &ScrollbackState) {
        self.editor.set_text(query);
        self.update_derived_query(state);
    }

    fn update_derived_query(&mut self, state: &ScrollbackState) {
        let query = self.editor.text().to_owned();
        // Compile UI-side: the render layer highlights from this matcher, and it
        // backs `query` / `has_error`, all of which must update immediately.
        self.matcher = TextMatcher::new(query.as_str(), QueryKind::Regex);
        // Keep the last settled result navigable while the next scan is in
        // flight. Empty and malformed queries are known synchronously to have
        // no matches, so those can clear immediately.
        if query.is_empty() || self.matcher.is_error() {
            self.matches = Arc::from([]);
            self.current = None;
        }

        // Re-send the corpus only when `sync` actually rebuilt it (content
        // changed); otherwise `None` tells the daemon to keep the corpus it
        // already holds. The index itself guards the rebuild on
        // `content_generation`, so no separate generation tracking is needed.
        let corpus = self.index.sync(state).then(|| self.index.entries_arc());
        let Some(request_generation) = self.request_generation.checked_add(1) else {
            tracing::debug!("scrollback search request generation exhausted; dropping update");
            return;
        };
        self.request_generation = request_generation;
        if let Err(err) = self.daemon.tx.send(SearchMsg::Update {
            corpus,
            query,
            request_generation,
        }) {
            // A failed send means the daemon thread is gone (panicked or already
            // stopped) — search has silently stopped working, so leave a trace.
            tracing::debug!(%err, "scrollback search daemon unavailable; dropping query update");
        }
    }

    pub(crate) fn apply_query_key(
        &mut self,
        key: &KeyEvent,
        state: &ScrollbackState,
    ) -> LineEditOutcome {
        let outcome = self.editor.handle_key(key);
        if outcome == LineEditOutcome::TextChanged {
            self.update_derived_query(state);
        }
        outcome
    }

    pub(crate) fn apply_query_paste(
        &mut self,
        text: &str,
        state: &ScrollbackState,
    ) -> LineEditOutcome {
        let outcome = self.editor.insert_paste(text);
        if outcome == LineEditOutcome::TextChanged {
            self.update_derived_query(state);
        }
        outcome
    }

    /// Apply one canonical query-edit key. Returns whether the key was consumed.
    pub fn handle_query_key(&mut self, key: &KeyEvent, state: &ScrollbackState) -> bool {
        self.apply_query_key(key, state) != LineEditOutcome::Unhandled
    }

    /// Pick up the latest scan results from the daemon. Returns `true` when the
    /// results changed (so the caller can redraw / reveal the new match).
    ///
    /// On a change the cursor parks on the first match, preserving the old
    /// "jump to the first match when the query changes" behavior.
    pub fn poll(&mut self) -> bool {
        // Hold the lock only for the cheap compares (and, on a real change, an
        // Arc-pointer clone). Never clone the whole snapshot: that would heap-
        // allocate its `query: String` on every no-change tick at ~30 Hz.
        let guard = self.daemon.shared.lock().unwrap();
        if guard.request_generation == self.last_seen_generation {
            return false;
        }
        self.last_seen_generation = guard.request_generation;
        // Drop a scan that finished for a superseded query: while it was in
        // flight the user kept typing, so the editor and derived matcher have
        // already moved on. Applying it would desync the match count and
        // cursor from the visible query until the current query's scan lands.
        if guard.request_generation != self.request_generation || guard.query != self.query() {
            return false;
        }
        let matches = guard.matches.clone();
        drop(guard);
        self.matches = matches;
        self.current = (!self.matches.is_empty()).then_some(0);
        true
    }

    /// Move the cursor to the next match, wrapping past the end. No-op when
    /// there are no matches.
    pub fn next(&mut self) {
        self.step(1);
    }

    /// Move the cursor to the previous match, wrapping past the front. No-op
    /// when there are no matches.
    pub fn prev(&mut self) {
        self.step(-1);
    }

    fn step(&mut self, delta: isize) {
        let len = self.matches.len();
        if len == 0 {
            self.current = None;
            return;
        }
        let from = self.current.unwrap_or(0) as isize;
        // `rem_euclid` keeps the index within `0..len` for either direction.
        self.current = Some((from + delta).rem_euclid(len as isize) as usize);
    }

    /// Freeze the query and switch from composing to browsing. The matcher and
    /// match list are retained so `n` / `N` can step through them.
    pub fn accept(&mut self) {
        self.composing = false;
    }

    /// The match under the cursor, if any.
    pub fn current(&self) -> Option<&ScrollbackMatch> {
        self.current.and_then(|i| self.matches.get(i))
    }

    /// Cursor position within the match list (0-based), if there are matches.
    pub fn current_index(&self) -> Option<usize> {
        self.current
    }

    /// Total number of matches for the current query.
    pub fn match_count(&self) -> usize {
        self.matches.len()
    }

    /// The raw query string being searched.
    pub fn query(&self) -> &str {
        self.editor.text()
    }

    pub fn query_viewport(&self, width: usize) -> echo_agent_ratatui_textarea::SingleLineViewport {
        self.editor.viewport(width)
    }

    /// The compiled query regex for the highlight pass, or `None` when the
    /// query is empty or fails to compile (nothing to highlight). Decoupled
    /// from the index — the render layer re-runs it per visible row.
    pub fn highlight_regex(&self) -> Option<regex::Regex> {
        (!self.query().is_empty() && !self.matcher.is_error())
            .then(|| self.matcher.compiled_regex().clone())
    }

    /// Whether the current query is a malformed regex (so it matches nothing).
    pub fn has_error(&self) -> bool {
        self.matcher.is_error()
    }

    /// Whether the query is still being edited (`true`) versus accepted and
    /// being browsed (`false`).
    pub fn is_composing(&self) -> bool {
        self.composing
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scrollback::block::RenderBlock;
    use crate::search::QueryKind;
    use crossterm::event::{KeyCode, KeyModifiers};

    fn substring(query: &str) -> TextMatcher {
        TextMatcher::new(query, QueryKind::Substring)
    }

    #[test]
    fn find_locates_substring_matches_in_scrollback_order() {
        let mut state = ScrollbackState::new();
        let first = state.push_block(RenderBlock::user_prompt("foo bar"));
        let second = state.push_block(RenderBlock::user_prompt("baz foo"));

        let mut index = ScrollbackSearchIndex::new();
        index.sync(&state);
        let matches = index.find(&substring("foo"));

        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].entry_id, first);
        assert_eq!(matches[0].line_in_entry, 0);
        assert_eq!(matches[0].byte_range, 0..3);
        assert_eq!(matches[1].entry_id, second);
        assert_eq!(matches[1].line_in_entry, 0);
        assert_eq!(matches[1].byte_range, 4..7);
    }

    #[test]
    fn find_reports_line_within_multiline_entry() {
        let mut state = ScrollbackState::new();
        let id = state.push_block(RenderBlock::user_prompt("alpha\nbravo\ncharlie\nbravo"));

        let mut index = ScrollbackSearchIndex::new();
        index.sync(&state);
        let matches = index.find(&substring("bravo"));

        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].entry_id, id);
        assert_eq!(matches[0].line_in_entry, 1);
        assert_eq!(matches[1].line_in_entry, 3);
    }

    #[test]
    fn find_matches_markdown_phrase_spanning_emphasis() {
        // Regression for "highlighted but no matches": the index searches the
        // rendered text, so a phrase that spans markdown emphasis is found —
        // matching what the on-screen highlight shows. Over the raw source
        // `this is **really** important` the phrase would be missed.
        let mut state = ScrollbackState::new();
        let id = state.push_block(RenderBlock::agent_message("this is **really** important"));

        let mut index = ScrollbackSearchIndex::new();
        index.sync(&state);
        let matches = index.find(&substring("is really important"));

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].entry_id, id);
    }

    #[test]
    fn empty_query_finds_nothing() {
        let mut state = ScrollbackState::new();
        state.push_block(RenderBlock::user_prompt("anything"));

        let mut index = ScrollbackSearchIndex::new();
        index.sync(&state);

        assert!(index.find(&substring("")).is_empty());
    }

    #[test]
    fn zero_width_matches_are_skipped() {
        let mut state = ScrollbackState::new();
        state.push_block(RenderBlock::user_prompt("abc"));

        let mut index = ScrollbackSearchIndex::new();
        index.sync(&state);
        // `x*` matches the empty string at every position; all are zero-width.
        let matcher = TextMatcher::new("x*", QueryKind::Regex);

        assert!(!matcher.is_error());
        assert!(index.find(&matcher).is_empty());
    }

    #[test]
    fn invalid_regex_finds_nothing() {
        let mut state = ScrollbackState::new();
        state.push_block(RenderBlock::user_prompt("[invalid"));

        let mut index = ScrollbackSearchIndex::new();
        index.sync(&state);
        let matcher = TextMatcher::new("[invalid", QueryKind::Regex);

        assert!(matcher.is_error());
        assert!(index.find(&matcher).is_empty());
    }

    #[test]
    fn find_respects_smart_case() {
        let mut state = ScrollbackState::new();
        state.push_block(RenderBlock::user_prompt("Hello world"));

        let mut index = ScrollbackSearchIndex::new();
        index.sync(&state);

        // Lowercase query is case-insensitive; an uppercase letter makes it strict.
        assert_eq!(index.find(&substring("hello")).len(), 1);
        assert!(index.find(&substring("Hxllo")).is_empty());
        assert!(index.find(&substring("WORLD")).is_empty());
    }

    #[test]
    fn entries_without_searchable_text_are_skipped() {
        let mut state = ScrollbackState::new();
        state.push_block(RenderBlock::user_prompt(""));
        state.push_block(RenderBlock::user_prompt("foo"));

        let mut index = ScrollbackSearchIndex::new();
        index.sync(&state);

        assert_eq!(index.entries.len(), 1);
        assert_eq!(index.find(&substring("foo")).len(), 1);
    }

    #[test]
    fn sync_is_noop_when_content_generation_unchanged() {
        let mut state = ScrollbackState::new();
        state.push_block(RenderBlock::user_prompt("foo"));

        let mut index = ScrollbackSearchIndex::new();
        assert!(index.sync(&state), "first sync rebuilds the cache");
        let built = index.built_generation;

        // Scrolling and viewport changes bump `generation` but not content.
        state.scroll_up(1);
        state.prepare_layout(80, 10);
        assert!(
            !index.sync(&state),
            "sync must report no rebuild when content is unchanged"
        );

        assert_eq!(
            index.built_generation, built,
            "sync must not rebuild when content is unchanged"
        );
    }

    #[test]
    fn sync_rebuilds_after_content_change() {
        let mut state = ScrollbackState::new();
        state.push_block(RenderBlock::user_prompt("foo one"));

        let mut index = ScrollbackSearchIndex::new();
        assert!(index.sync(&state));
        assert_eq!(index.find(&substring("foo")).len(), 1);

        state.push_block(RenderBlock::user_prompt("foo two"));
        assert!(index.sync(&state), "content change triggers a rebuild");
        assert_eq!(
            index.find(&substring("foo")).len(),
            2,
            "rebuild picks up the appended entry"
        );

        state.clear();
        assert!(index.sync(&state), "clearing content triggers a rebuild");
        assert!(
            index.find(&substring("foo")).is_empty(),
            "rebuild drops removed entries"
        );
    }

    fn state_with(prompts: &[&str]) -> ScrollbackState {
        let mut state = ScrollbackState::new();
        for p in prompts {
            state.push_block(RenderBlock::user_prompt(*p));
        }
        state
    }

    /// Send a query and wait for the daemon to publish its result.
    ///
    /// One `update_query` is one atomic `Update`, so it yields exactly one
    /// snapshot bump; break on the first `poll` that observes it. Panics if the
    /// daemon never responds so a wedged daemon surfaces here, not as a
    /// confusing downstream assertion.
    fn update_and_wait(search: &mut ScrollbackSearchState, query: &str, state: &ScrollbackState) {
        search.update_query(query, state);
        for _ in 0..1000 {
            if search.poll() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("scrollback search daemon did not publish a result for {query:?}");
    }

    #[test]
    fn open_starts_empty_and_composing() {
        let search = ScrollbackSearchState::open();
        assert!(search.is_composing());
        assert_eq!(search.match_count(), 0);
        assert_eq!(search.current_index(), None);
        assert!(search.current().is_none());
        assert_eq!(search.query(), "");
    }

    #[test]
    fn update_query_finds_matches_and_parks_on_first() {
        let state = state_with(&["foo bar", "baz foo"]);
        let mut search = ScrollbackSearchState::open();

        update_and_wait(&mut search, "foo", &state);

        assert_eq!(search.query(), "foo");
        assert_eq!(search.match_count(), 2);
        assert_eq!(search.current_index(), Some(0));
        assert_eq!(search.current().unwrap().byte_range, 0..3);
    }

    #[test]
    fn update_query_with_no_matches_clears_cursor() {
        let state = state_with(&["foo bar"]);
        let mut search = ScrollbackSearchState::open();

        // The daemon publishes an empty snapshot (with a bumped generation) even
        // when nothing matches, so the no-match result still arrives via poll.
        update_and_wait(&mut search, "nope", &state);

        assert_eq!(search.match_count(), 0);
        assert_eq!(search.current_index(), None);
        assert!(search.current().is_none());
    }

    #[test]
    fn update_query_back_to_empty_clears_matches() {
        let state = state_with(&["foo bar"]);
        let mut search = ScrollbackSearchState::open();
        update_and_wait(&mut search, "foo", &state);
        assert_eq!(search.match_count(), 1);

        // Deleting the query (as Backspace would) clears the cursor and matches.
        update_and_wait(&mut search, "", &state);
        assert_eq!(search.query(), "");
        assert_eq!(search.match_count(), 0);
        assert_eq!(search.current_index(), None);
    }

    #[test]
    fn update_query_with_invalid_regex_finds_nothing_but_keeps_query() {
        let state = state_with(&["[brackets]"]);
        let mut search = ScrollbackSearchState::open();

        // The query is always interpreted as a regex; a malformed one matches
        // nothing but is still echoed back for the search bar to display.
        update_and_wait(&mut search, "[invalid", &state);

        assert_eq!(search.query(), "[invalid");
        assert!(search.has_error());
        assert_eq!(search.match_count(), 0);
        assert_eq!(search.current_index(), None);
    }

    #[test]
    fn cursor_only_query_edits_do_not_enqueue_daemon_work() {
        let state = state_with(&["foo bar"]);
        let mut search = ScrollbackSearchState::open();
        update_and_wait(&mut search, "foo", &state);
        let generation = search.request_generation;

        let outcome =
            search.apply_query_key(&KeyEvent::new(KeyCode::Left, KeyModifiers::NONE), &state);
        assert_eq!(outcome, LineEditOutcome::CursorChanged);
        assert_eq!(search.query(), "foo");
        assert_eq!(search.request_generation, generation);
        assert_eq!(search.match_count(), 1);

        let outcome = search.apply_query_key(
            &KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            &state,
        );
        assert_eq!(outcome, LineEditOutcome::TextChanged);
        assert_eq!(search.query(), "foxo");
        assert_eq!(search.request_generation, generation + 1);
        assert_eq!(search.match_count(), 1);
        assert_eq!(search.current_index(), Some(0));
        search.next();
        assert_eq!(search.current_index(), Some(0));

        let mut settled = false;
        for _ in 0..1000 {
            if search.poll() {
                settled = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(settled, "daemon did not publish the text mutation");
        assert_eq!(search.match_count(), 0);
        assert_eq!(search.current_index(), None);
        assert_eq!(
            search.daemon.shared.lock().unwrap().request_generation,
            generation + 1
        );
    }

    #[test]
    fn paste_sanitizes_at_cursor_and_enqueues_only_text_mutation() {
        let state = state_with(&["alpha beta"]);
        let mut search = ScrollbackSearchState::open();
        search.update_query("ab", &state);
        let _ = search.editor.set_cursor_byte(1);
        let generation = search.request_generation;

        let outcome = search.apply_query_paste("中\r\n", &state);
        assert_eq!(outcome, LineEditOutcome::TextChanged);
        assert_eq!(search.query(), "a中b");
        assert_eq!(search.request_generation, generation + 1);

        let outcome = search.apply_query_paste("\r\n", &state);
        assert_eq!(outcome, LineEditOutcome::HandledNoChange);
        assert_eq!(search.query(), "a中b");
        assert_eq!(search.request_generation, generation + 1);
    }

    #[test]
    fn query_viewport_preserves_graphemes_and_actual_cursor() {
        let state = state_with(&["anything"]);
        let mut search = ScrollbackSearchState::open();
        let grapheme = "👩🏽\u{200d}💻";
        search.update_query(&format!("123456中e\u{301}{grapheme}z"), &state);
        let cursor_byte = search.query().len() - 1;
        let _ = search.editor.set_cursor_byte(cursor_byte);
        let viewport = search.query_viewport(10);
        let visible = &search.query()[viewport.visible_byte_range];
        assert!(visible.contains('中'));
        assert!(visible.contains("e\u{301}"));
        assert!(visible.contains(grapheme));
        assert!(viewport.cursor_display_column < 10);
    }

    #[test]
    fn update_query_picks_up_content_added_mid_session() {
        // Content appended while a session is open changes `content_generation`,
        // so the next query re-syncs the corpus to the daemon and finds it.
        let mut state = state_with(&["foo one"]);
        let mut search = ScrollbackSearchState::open();
        update_and_wait(&mut search, "foo", &state);
        assert_eq!(search.match_count(), 1);
        let generation = search.request_generation;

        state.push_block(RenderBlock::user_prompt("foo two"));
        update_and_wait(&mut search, "foo", &state);
        assert_eq!(search.request_generation, generation + 1);
        assert_eq!(search.match_count(), 2);
    }

    #[test]
    fn async_scan_delivers_results_and_navigation_wraps() {
        // End-to-end of the async path: open → enqueue query → poll in results →
        // synchronous navigation over the delivered match list.
        let state = state_with(&["foo bar", "baz foo"]);
        let mut search = ScrollbackSearchState::open();

        update_and_wait(&mut search, "foo", &state);
        assert_eq!(search.match_count(), 2);
        assert_eq!(search.current_index(), Some(0));

        search.next();
        assert_eq!(search.current_index(), Some(1));
        search.next();
        assert_eq!(search.current_index(), Some(0), "next wraps past the end");
    }

    #[test]
    fn next_and_prev_wrap_around_matches() {
        let mut state = ScrollbackState::new();
        let first = state.push_block(RenderBlock::user_prompt("foo"));
        let second = state.push_block(RenderBlock::user_prompt("foo"));
        let third = state.push_block(RenderBlock::user_prompt("foo"));
        let mut search = ScrollbackSearchState::open();
        update_and_wait(&mut search, "foo", &state);
        assert_eq!(search.current_index(), Some(0));
        // `current()` resolves to the match the cursor points at.
        assert_eq!(search.current().unwrap().entry_id, first);

        search.next();
        assert_eq!(search.current_index(), Some(1));
        assert_eq!(search.current().unwrap().entry_id, second);
        search.next();
        assert_eq!(search.current_index(), Some(2));
        assert_eq!(search.current().unwrap().entry_id, third);
        search.next();
        assert_eq!(search.current_index(), Some(0), "next wraps past the end");
        assert_eq!(search.current().unwrap().entry_id, first);

        search.prev();
        assert_eq!(search.current_index(), Some(2), "prev wraps past the front");
        assert_eq!(search.current().unwrap().entry_id, third);
        search.prev();
        assert_eq!(search.current_index(), Some(1));
    }

    #[test]
    fn next_and_prev_are_noop_without_matches() {
        let mut search = ScrollbackSearchState::open();

        search.next();
        assert_eq!(search.current_index(), None);
        search.prev();
        assert_eq!(search.current_index(), None);
    }

    #[test]
    fn accept_stops_composing_but_keeps_matches() {
        let state = state_with(&["foo", "foo"]);
        let mut search = ScrollbackSearchState::open();
        update_and_wait(&mut search, "foo", &state);
        search.next();

        search.accept();

        assert!(!search.is_composing());
        assert_eq!(search.query(), "foo");
        assert_eq!(search.match_count(), 2);
        assert_eq!(
            search.current_index(),
            Some(1),
            "accept preserves the cursor"
        );
        assert!(
            search.current().is_some(),
            "the cursor still resolves to a match after accept"
        );
    }

    #[test]
    fn update_query_refinds_against_cached_corpus() {
        // Changing the query without changing content re-scans the same corpus
        // (no new corpus is sent — `Update.corpus` is `None` — so the daemon
        // reuses the corpus it holds).
        let state = state_with(&["alpha beta", "beta gamma"]);
        let mut search = ScrollbackSearchState::open();

        update_and_wait(&mut search, "alpha", &state);
        assert_eq!(search.match_count(), 1);

        update_and_wait(&mut search, "beta", &state);
        assert_eq!(search.match_count(), 2);
        assert_eq!(search.current_index(), Some(0));
    }

    fn corpus_of(prompts: &[&str]) -> Arc<[IndexedEntry]> {
        let mut index = ScrollbackSearchIndex::new();
        index.sync(&state_with(prompts));
        index.entries_arc()
    }

    #[test]
    fn drain_to_latest_keeps_newest_corpus_and_query() {
        let c1 = corpus_of(&["one"]);
        let c2 = corpus_of(&["two", "three"]);
        let (tx, rx) = std::sync::mpsc::channel::<SearchMsg>();
        tx.send(SearchMsg::Update {
            corpus: Some(c1),
            query: "a".into(),
            request_generation: 1,
        })
        .unwrap();
        tx.send(SearchMsg::Update {
            corpus: None,
            query: "ab".into(),
            request_generation: 2,
        })
        .unwrap();
        tx.send(SearchMsg::Update {
            corpus: Some(c2.clone()),
            query: "abc".into(),
            request_generation: 3,
        })
        .unwrap();

        let first = rx.recv().unwrap();
        let out = drain_to_latest(first, &rx);

        assert!(!out.stop);
        assert_eq!(out.query.as_deref(), Some("abc"), "newest query wins");
        assert_eq!(out.request_generation, Some(3));
        assert!(
            std::sync::Arc::ptr_eq(&out.corpus.unwrap(), &c2),
            "newest Some(corpus) wins"
        );
    }

    #[test]
    fn drain_to_latest_none_corpus_keeps_earlier_corpus() {
        let c1 = corpus_of(&["one"]);
        let (tx, rx) = std::sync::mpsc::channel::<SearchMsg>();
        tx.send(SearchMsg::Update {
            corpus: Some(c1.clone()),
            query: "a".into(),
            request_generation: 1,
        })
        .unwrap();
        tx.send(SearchMsg::Update {
            corpus: None,
            query: "ab".into(),
            request_generation: 2,
        })
        .unwrap();

        let first = rx.recv().unwrap();
        let out = drain_to_latest(first, &rx);

        assert!(
            std::sync::Arc::ptr_eq(&out.corpus.unwrap(), &c1),
            "a later None corpus must not clobber the earlier corpus"
        );
        assert_eq!(out.query.as_deref(), Some("ab"));
        assert_eq!(out.request_generation, Some(2));
    }

    #[test]
    fn drain_to_latest_stop_wins() {
        let (tx, rx) = std::sync::mpsc::channel::<SearchMsg>();
        tx.send(SearchMsg::Update {
            corpus: None,
            query: "a".into(),
            request_generation: 1,
        })
        .unwrap();
        tx.send(SearchMsg::Stop).unwrap();

        let first = rx.recv().unwrap();
        let out = drain_to_latest(first, &rx);

        assert!(out.stop);
    }

    #[test]
    fn poll_rejects_same_query_and_aba_stale_snapshots() {
        // The daemon parks on recv() until a message is sent; since this test
        // never calls update_query, the shared snapshot is uncontested and we
        // can publish snapshots in a deterministic order.
        let mut search = ScrollbackSearchState::open();
        search.editor.set_text("A");
        search.matcher = TextMatcher::new("A", QueryKind::Regex);
        search.request_generation = 3;

        let mut state = ScrollbackState::new();
        let id = state.push_block(RenderBlock::user_prompt("x"));
        let a_match = ScrollbackMatch {
            entry_id: id,
            line_in_entry: 0,
            byte_range: 0..1,
        };

        // Generation 1 used the same visible query but an older corpus.
        *search.daemon.shared.lock().unwrap() = SearchSnapshot {
            matches: std::sync::Arc::from([a_match.clone()]),
            request_generation: 1,
            query: "A".into(),
        };
        assert!(!search.poll(), "same-query stale corpus result is dropped");
        assert_eq!(search.match_count(), 0);

        // Generation 2 is the intermediate B in an A→B→A sequence.
        *search.daemon.shared.lock().unwrap() = SearchSnapshot {
            matches: std::sync::Arc::from([a_match.clone()]),
            request_generation: 2,
            query: "B".into(),
        };
        assert!(!search.poll(), "intermediate B result is dropped");
        assert_eq!(search.match_count(), 0);

        *search.daemon.shared.lock().unwrap() = SearchSnapshot {
            matches: std::sync::Arc::from([a_match]),
            request_generation: 3,
            query: "A".into(),
        };
        assert!(search.poll(), "current A generation is applied");
        assert_eq!(search.match_count(), 1);
        assert_eq!(search.current_index(), Some(0));
    }

    #[test]
    fn poll_without_new_generation_keeps_navigation_cursor() {
        // The generation guard's load-bearing job: once results land and the
        // user navigates, repeated polls that observe no new daemon write must
        // not snap the cursor back to the first match. (Deleting the guard would
        // regress this without tripping any other test.)
        let state = state_with(&["foo", "foo", "foo"]);
        let mut search = ScrollbackSearchState::open();
        update_and_wait(&mut search, "foo", &state);
        assert_eq!(search.match_count(), 3);
        assert_eq!(search.current_index(), Some(0));

        search.next();
        assert_eq!(search.current_index(), Some(1));

        // No further daemon write happened, so each poll sees the same
        // generation and must be a no-op that preserves the cursor.
        for _ in 0..5 {
            assert!(
                !search.poll(),
                "poll with no new generation reports no change"
            );
            assert_eq!(
                search.current_index(),
                Some(1),
                "poll must not reset the cursor when nothing changed"
            );
        }
    }

    #[test]
    fn coalesced_burst_carries_corpus_forward_to_last_query() {
        // Settle once so the daemon holds the corpus, then fire a burst of
        // queries back-to-back without polling between sends so they coalesce in
        // the channel. None of the burst updates carry a corpus (content is
        // unchanged), so the daemon must reuse the corpus it holds and settle on
        // the LAST query — drain_to_latest coalescing + corpus carry-forward
        // (`Update.corpus` is `None`) exercised end-to-end through the daemon.
        let state = state_with(&["alpha", "alpha beta", "beta gamma"]);
        let mut search = ScrollbackSearchState::open();
        update_and_wait(&mut search, "alpha", &state);
        assert_eq!(search.match_count(), 2);

        search.update_query("beta", &state);
        search.update_query("alpha", &state);
        search.update_query("gamma", &state);

        // poll() drops snapshots for the superseded earlier queries (the matcher
        // already moved to "gamma"), so the first true poll is the last query's
        // result — proving the corpus survived the None-corpus burst.
        let mut settled = false;
        for _ in 0..1000 {
            if search.poll() {
                settled = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(settled, "daemon never settled on the final burst query");
        assert_eq!(search.query(), "gamma");
        assert_eq!(
            search.match_count(),
            1,
            "the held corpus is reused across a None-corpus burst"
        );
        assert_eq!(search.current_index(), Some(0));
    }
}
