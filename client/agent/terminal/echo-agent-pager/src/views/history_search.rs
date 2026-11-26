//! Prompt history search with background-thread nucleo matching.
//!
//! Architecture mirrors the file search `FuzzyFileMatcherDaemon`:
//! - A background `std::thread` owns the nucleo `Matcher` + `MultiPattern`.
//! - The UI thread sends queries via a channel (`set_query`) — never blocks.
//! - The background thread scores items, computes indices, writes results
//!   to `Arc<Mutex<…>>`.
//! - The UI thread polls results on each tick via `poll()`.
//! - The daemon spawns lazily on first activation and is kept for reuse:
//!   every `PromptWidget` (one per agent view, including subagent child
//!   views) owns a `HistorySearchState`, so an eager spawn leaks one parked
//!   thread per subagent for the process lifetime.

use std::sync::{
    Arc, Mutex,
    mpsc::{SyncSender, sync_channel},
};
use std::thread::{self, JoinHandle};

use nucleo::{
    Config, Matcher, Utf32String,
    pattern::{CaseMatching, MultiPattern, Normalization},
};

// ---------------------------------------------------------------------------
// Public data types
// ---------------------------------------------------------------------------

/// A single entry in the prompt history.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub text: String,
}

/// A matched result with highlight positions (produced by the daemon).
#[derive(Debug, Clone)]
pub struct HistoryMatchResult {
    pub text: String,
    pub indices: Vec<u32>,
}

// ---------------------------------------------------------------------------
// Shared state (daemon → UI)
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct Snapshot {
    items: Arc<[HistoryMatchResult]>,
    generation: usize,
}

// ---------------------------------------------------------------------------
// Daemon messages (UI → daemon)
// ---------------------------------------------------------------------------

enum Msg {
    SetItems(Vec<String>),
    SetItemsAndQuery(Vec<String>, String),
    SetQuery(String),
    Stop,
}

// ---------------------------------------------------------------------------
// Background daemon
// ---------------------------------------------------------------------------

struct Daemon {
    shared: Arc<Mutex<Snapshot>>,
    tx: SyncSender<Msg>,
    _handle: JoinHandle<()>,
}

const MAX_RESULTS: usize = 100;

impl Daemon {
    /// Spawn the matcher thread. `None` when the spawn fails — no
    /// half-constructed daemon whose only effect on drop is a `Stop` into a
    /// channel nobody reads.
    fn spawn() -> Option<Self> {
        let shared = Arc::new(Mutex::new(Snapshot::default()));
        let (tx, rx) = sync_channel::<Msg>(256);

        let out = shared.clone();
        let worker = move || {
            let mut pattern = MultiPattern::new(1);
            let mut matcher = Matcher::new(Config::DEFAULT);
            let mut items: Vec<(String, Utf32String)> = Vec::new();
            let mut generation: usize = 0;
            let mut prev_q = String::new();

            while let Ok(msg) = rx.recv() {
                let msg = drain_to_latest(msg, &rx);

                match msg {
                    Msg::SetItems(new) => {
                        items = build_items(new);
                        prev_q.clear();
                        generation += 1;
                        publish_matches(&items, "", &mut pattern, &mut matcher, &out, generation);
                    }
                    Msg::SetItemsAndQuery(new, query) => {
                        items = build_items(new);
                        prev_q.clear();
                        generation += 1;
                        let trimmed = query.trim().to_string();
                        publish_matches(
                            &items,
                            &trimmed,
                            &mut pattern,
                            &mut matcher,
                            &out,
                            generation,
                        );
                        prev_q = trimmed;
                    }
                    Msg::SetQuery(query) => {
                        generation += 1;
                        let trimmed = query.trim().to_string();

                        if trimmed.is_empty() {
                            publish_matches(
                                &items,
                                "",
                                &mut pattern,
                                &mut matcher,
                                &out,
                                generation,
                            );
                            prev_q.clear();
                        } else {
                            let append = !prev_q.is_empty()
                                && trimmed.as_bytes().starts_with(prev_q.as_bytes())
                                && !trimmed.ends_with('\\')
                                && !trimmed
                                    .as_bytes()
                                    .last()
                                    .is_some_and(|b| b.is_ascii_whitespace());
                            publish_query_matches(
                                &items,
                                &trimmed,
                                append,
                                &mut pattern,
                                &mut matcher,
                                &out,
                                generation,
                            );
                            prev_q = trimmed;
                        }
                    }
                    Msg::Stop => break,
                }
            }
        };
        match thread::Builder::new()
            .name("history-search".into())
            .spawn(worker)
        {
            Ok(handle) => Some(Self {
                shared,
                tx,
                _handle: handle,
            }),
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "history search daemon thread spawn failed; history search disabled"
                );
                None
            }
        }
    }
}

fn build_items(items: Vec<String>) -> Vec<(String, Utf32String)> {
    items
        .into_iter()
        .filter(|s| !s.is_empty())
        .map(|s| {
            let u = Utf32String::from(s.as_str());
            (s, u)
        })
        .collect()
}

fn publish_matches(
    items: &[(String, Utf32String)],
    query: &str,
    pattern: &mut MultiPattern,
    matcher: &mut Matcher,
    out: &Arc<Mutex<Snapshot>>,
    generation: usize,
) {
    if query.is_empty() {
        // Items arrive most-recent-first; reverse so the most recent prompt is
        // last (rendered at the bottom of the overlay, nearest the prompt).
        let mut all: Vec<HistoryMatchResult> = items
            .iter()
            .take(MAX_RESULTS)
            .map(|(s, _)| HistoryMatchResult {
                text: s.clone(),
                indices: Vec::new(),
            })
            .collect();
        all.reverse();
        *out.lock().unwrap() = Snapshot {
            items: all.into(),
            generation,
        };
    } else {
        publish_query_matches(items, query, false, pattern, matcher, out, generation);
    }
}

fn publish_query_matches(
    items: &[(String, Utf32String)],
    query: &str,
    append: bool,
    pattern: &mut MultiPattern,
    matcher: &mut Matcher,
    out: &Arc<Mutex<Snapshot>>,
    generation: usize,
) {
    pattern.reparse(0, query, CaseMatching::Smart, Normalization::Smart, append);

    let mut hits: Vec<(usize, u32)> = Vec::new();
    for (i, (_, u)) in items.iter().enumerate() {
        if let Some(sc) = pattern.score(std::slice::from_ref(u), matcher) {
            hits.push((i, sc));
        }
    }
    hits.sort_unstable_by(|a, b| b.1.cmp(&a.1));
    if hits.len() > MAX_RESULTS {
        hits.truncate(MAX_RESULTS);
    }

    let col = pattern.column_pattern(0);
    let mut matched: Vec<HistoryMatchResult> = hits
        .into_iter()
        .map(|(i, _)| {
            let (text, u) = &items[i];
            let mut idx = Vec::new();
            col.indices(u.slice(..), matcher, &mut idx);
            HistoryMatchResult {
                text: text.clone(),
                indices: idx,
            }
        })
        .collect();
    // `hits` is sorted best-first; reverse so the best match is last (rendered
    // at the bottom of the overlay, selected by default).
    matched.reverse();
    *out.lock().unwrap() = Snapshot {
        items: matched.into(),
        generation,
    };
}

/// Drain the channel to the most recent message, coalescing queries.
fn drain_to_latest(first: Msg, rx: &std::sync::mpsc::Receiver<Msg>) -> Msg {
    let mut current = first;
    while let Ok(next) = rx.try_recv() {
        current = match (current, next) {
            // Coalesce consecutive SetQuery — keep latest.
            (Msg::SetQuery(_), next @ Msg::SetQuery(_)) => next,
            // Preserve the item refresh and latest query as one atomic update.
            (Msg::SetItems(items), Msg::SetQuery(query)) => Msg::SetItemsAndQuery(items, query),
            (Msg::SetItemsAndQuery(items, _), Msg::SetQuery(query)) => {
                Msg::SetItemsAndQuery(items, query)
            }
            // Stop always wins.
            (_, stop @ Msg::Stop) => return stop,
            // SetItems after SetQuery — keep SetItems (reset).
            (_, next) => next,
        };
    }
    current
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Stop);
    }
}

// ---------------------------------------------------------------------------
// HistorySearchState (UI-thread side)
// ---------------------------------------------------------------------------

/// UI-side state for the history search overlay.
///
/// The UI thread never runs nucleo. All matching happens on the daemon
/// thread. The UI sends queries via `update_query()` and polls results
/// via `poll()`, exactly like `FuzzyFileMatcherDaemon`.
/// Which entry point opened the overlay.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// `/history`: the composer is the filter query.
    Search,
    /// Up on an empty prompt: selection live-populates the composer.
    Browse,
}

pub struct HistorySearchState {
    /// Matcher daemon, built lazily on first activation (see module docs
    /// for why eager spawning leaks) and kept until the widget drops
    /// (`Daemon::drop` stops the thread). Its copy of the history is
    /// released on `deactivate`, so retained memory is bounded by the time
    /// the overlay is open. Mirrors `FileSearchState::daemon`.
    daemon: Option<Daemon>,
    /// Test-only count of daemon builds, to prove reuse (no drop-and-rebuild).
    #[cfg(test)]
    daemon_builds: usize,
    active: bool,
    saved_text: String,
    snapshot: Snapshot,
    last_gen: usize,
    pub selected: usize,
    /// While `true`, selection tracks the bottom-most (most-recent / best-match)
    /// entry as results stream in. Set on `activate`, cleared once the user
    /// navigates (Up/Down/PageUp/PageDown/click). This makes the overlay open
    /// with the most recent prompt selected at the bottom of the list.
    stick_to_bottom: bool,
    /// The last query sent to the daemon. Used to distinguish a genuine query
    /// change (user typing → re-anchor selection to the best match) from a
    /// re-application of the same query (e.g. a late background
    /// `PromptHistoryLoaded` refresh → must not clobber the user's selection).
    last_query: String,
    /// Mouse-hovered result index (visual highlight only).
    hovered: Option<usize>,
    /// Browse (Up-arrow entry point): the selection lives in the composer
    /// (live-populated on every move), typing detaches to edit, and Down at
    /// the newest closes. Search (`/history`) keeps the composer as the
    /// filter query instead.
    mode: Mode,
}

impl Default for HistorySearchState {
    fn default() -> Self {
        Self::new()
    }
}

impl HistorySearchState {
    pub fn new() -> Self {
        Self {
            daemon: None,
            #[cfg(test)]
            daemon_builds: 0,
            active: false,
            saved_text: String::new(),
            snapshot: Snapshot::default(),
            last_gen: 0,
            selected: 0,
            stick_to_bottom: true,
            last_query: String::new(),
            hovered: None,
            mode: Mode::Search,
        }
    }

    /// Build the matcher daemon on first activation. Returns `false` when
    /// the thread spawn failed — that attempt is not cached, so a later
    /// activation retries (thread pressure is often transient).
    fn ensure_daemon(&mut self) -> bool {
        if self.daemon.is_none() {
            self.daemon = Daemon::spawn();
            #[cfg(test)]
            if self.daemon.is_some() {
                self.daemon_builds += 1;
            }
        }
        self.daemon.is_some()
    }

    /// Send to the daemon. A disconnected channel means the matcher thread
    /// is gone (panicked); drop the daemon so the next activation respawns
    /// it instead of serving an overlay that never updates.
    fn send(&mut self, msg: Msg) {
        let Some(daemon) = &self.daemon else {
            return;
        };
        if daemon.tx.send(msg).is_err() {
            self.daemon = None;
        }
    }

    /// Whether the matcher daemon has been spawned. Regression accessor for
    /// the subagent storm test: child views must never build one.
    #[cfg(test)]
    pub(crate) fn daemon_built(&self) -> bool {
        self.daemon.is_some()
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn saved_text(&self) -> &str {
        &self.saved_text
    }

    pub fn result_count(&self) -> usize {
        self.snapshot.items.len()
    }

    /// Push the latest history into a running daemon. No-op before first
    /// activation (`activate_inner` re-sends the items it is given, so
    /// nothing is lost).
    pub fn refresh_items(&mut self, history: &[HistoryEntry]) {
        if self.daemon.is_none() {
            return;
        }
        let items: Vec<String> = history.iter().map(|e| e.text.clone()).collect();
        self.send(Msg::SetItems(items));
    }

    /// Activate in SEARCH mode (`/history`): send items to the
    /// daemon, show overlay. The composer is the filter query; navigation
    /// highlights only, Enter/Tab accepts. Returns `false` when the matcher
    /// thread could not start — the overlay stays closed and callers must
    /// leave the composer alone.
    #[must_use]
    pub fn activate(&mut self, history: &[HistoryEntry], current_text: &str) -> bool {
        self.activate_inner(history, current_text, Mode::Search)
    }

    /// Activate in BROWSE mode (Up on an empty prompt): same panel, but the
    /// caller fills the newest entry straight into the composer and every
    /// selection move live-populates it; typing detaches to edit, and Down
    /// at the newest entry closes the panel. Returns `false` when the matcher
    /// thread could not start.
    #[must_use]
    pub fn activate_browse(&mut self, history: &[HistoryEntry], current_text: &str) -> bool {
        self.activate_inner(history, current_text, Mode::Browse)
    }

    fn activate_inner(&mut self, history: &[HistoryEntry], current_text: &str, mode: Mode) -> bool {
        if !self.ensure_daemon() {
            return false;
        }
        self.active = true;
        self.mode = mode;
        self.saved_text = current_text.to_string();
        // Open with the most-recent prompt (rendered at the bottom) selected;
        // `poll` keeps it pinned to the bottom until the user navigates.
        self.stick_to_bottom = true;
        self.last_query.clear();
        self.refresh_items(history);
        // Eagerly grab the initial snapshot. Poison-tolerant: a daemon-side
        // panic must not take the UI thread down with it.
        if let Some(daemon) = &self.daemon {
            self.snapshot = daemon
                .shared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
        }
        self.last_gen = self.snapshot.generation;
        self.selected = self.snapshot.items.len().saturating_sub(1);
        true
    }

    /// True while the overlay is in browse mode (see [`Self::activate_browse`]).
    pub fn is_browse(&self) -> bool {
        self.active && self.mode == Mode::Browse
    }

    /// Deactivate: clear the overlay. The daemon thread stays alive for
    /// reuse, but its copy of the history is released so retained memory is
    /// bounded by the time the overlay is open (`activate` re-sends items).
    pub fn deactivate(&mut self) {
        self.active = false;
        self.mode = Mode::Search;
        self.snapshot = Snapshot::default();
        self.selected = 0;
        self.send(Msg::SetItems(Vec::new()));
    }

    /// Send a query update to the daemon (non-blocking, never stalls UI).
    pub fn update_query(&mut self, query: &str) {
        if self.daemon.is_none() {
            return;
        }
        // A genuinely new query (the user typed) re-anchors selection to the
        // best match at the bottom. Re-applying the *same* query (e.g. a late
        // background `PromptHistoryLoaded` refresh that re-sends the current
        // query) must not move a selection the user has already navigated to.
        if query != self.last_query {
            self.last_query = query.to_string();
            self.stick_to_bottom = true;
        }
        self.send(Msg::SetQuery(query.to_string()));
    }

    /// Poll for new results from the daemon. Returns `true` if changed.
    /// Call on every tick while the overlay is active.
    pub fn poll(&mut self) -> bool {
        if !self.active {
            return false;
        }
        let Some(daemon) = &self.daemon else {
            return false;
        };
        let snap = daemon
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if snap.generation == self.last_gen {
            return false;
        }
        self.last_gen = snap.generation;
        self.snapshot = snap;
        let len = self.snapshot.items.len();
        if len == 0 {
            self.selected = 0;
        } else if self.stick_to_bottom {
            // Keep the most-recent / best-match (bottom) entry selected as
            // results stream in or the query narrows.
            self.selected = len - 1;
        } else {
            self.selected = self.selected.min(len - 1);
        }
        true
    }

    /// Currently hovered index (mouse-driven), if any.
    pub fn hovered(&self) -> Option<usize> {
        self.hovered
    }

    /// Set hovered index. Returns `true` if changed.
    pub fn set_hovered(&mut self, index: Option<usize>) -> bool {
        let clamped = index.and_then(|i| {
            if i < self.snapshot.items.len() {
                Some(i)
            } else {
                None
            }
        });
        let changed = clamped != self.hovered;
        self.hovered = clamped;
        changed
    }

    /// Select the hovered item (for click-to-accept). Returns `true` if valid.
    pub fn select_hovered(&mut self) -> bool {
        if let Some(idx) = self.hovered
            && idx < self.snapshot.items.len()
        {
            self.stick_to_bottom = false;
            self.selected = idx;
            true
        } else {
            false
        }
    }

    /// Move the selection one row up (older). No wrap: at the top (oldest)
    /// the selection stays put. Returns `true` when it moved.
    pub fn move_up(&mut self) -> bool {
        let len = self.snapshot.items.len();
        if len == 0 || self.selected == 0 {
            return false;
        }
        self.stick_to_bottom = false;
        self.selected -= 1;
        true
    }

    /// Move the selection one row down (newer). No wrap: returns `false` at
    /// the bottom (newest) — the caller closes the overlay there, so a Down
    /// right after opening (newest is selected) backs out of history.
    pub fn move_down(&mut self) -> bool {
        let len = self.snapshot.items.len();
        if len == 0 || self.selected >= len - 1 {
            return false;
        }
        self.stick_to_bottom = false;
        self.selected += 1;
        true
    }

    /// Move selection by a page (half of visible height).
    pub fn page_move(&mut self, delta: isize, visible_rows: usize) {
        let half = (visible_rows / 2).max(1) as isize;
        let len = self.snapshot.items.len();
        if len == 0 {
            return;
        }
        self.stick_to_bottom = false;
        let max_idx = len - 1;
        let current = self.selected.min(max_idx);
        self.selected = (current as isize + delta * half).clamp(0, max_idx as isize) as usize;
    }

    /// Selected entry (returns `None` — use `selected_text()` instead).
    pub fn selected(&self) -> Option<&HistoryEntry> {
        // We can't return &HistoryEntry from Arc<[HistoryMatchResult]>.
        // Callers should use selected_text(). This returns None to satisfy
        // the type signature used by accept logic — the accept path uses
        // selected_text() via a separate check.
        None
    }

    /// Text of the currently selected entry.
    pub fn selected_text(&self) -> Option<&str> {
        self.snapshot
            .items
            .get(self.selected)
            .map(|r| r.text.as_str())
    }

    /// Get a result at a given index (for rendering).
    pub fn result_at(&self, idx: usize) -> Option<&HistoryMatchResult> {
        self.snapshot.items.get(idx)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(texts: &[&str]) -> Vec<HistoryEntry> {
        texts
            .iter()
            .map(|t| HistoryEntry {
                text: t.to_string(),
            })
            .collect()
    }

    /// Helper: activate + poll until results arrive.
    fn activate_and_poll(state: &mut HistorySearchState, history: &[HistoryEntry], saved: &str) {
        assert!(state.activate(history, saved));
        // The daemon runs on another thread; spin-poll briefly.
        for _ in 0..100 {
            if state.poll() && state.result_count() > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// Helper: send query + poll until results update.
    fn query_and_poll(state: &mut HistorySearchState, query: &str) {
        state.update_query(query);
        for _ in 0..100 {
            if state.poll() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// The leak regression this module's laziness exists for: construction
    /// (one state per `PromptWidget`) must not spawn the matcher thread.
    #[test]
    fn construction_does_not_spawn_the_daemon() {
        let state = HistorySearchState::new();
        assert!(state.daemon.is_none());
    }

    /// Deactivate/reactivate reuses the one daemon (no drop-and-respawn).
    #[test]
    fn reactivation_reuses_the_daemon() {
        let mut state = HistorySearchState::new();
        assert!(state.activate(&entries(&["a"]), ""));
        state.deactivate();
        assert!(state.activate(&entries(&["a", "b"]), ""));
        assert_eq!(state.daemon_builds, 1);
    }

    /// Pre-activation refresh/query/poll are inert — no daemon, no panic.
    #[test]
    fn refresh_query_poll_are_noops_before_first_activation() {
        let mut state = HistorySearchState::new();
        state.refresh_items(&entries(&["a"]));
        state.update_query("a");
        assert!(!state.poll());
        assert!(state.daemon.is_none());
    }

    #[test]
    fn refresh_items_and_query_are_applied_together() {
        let mut state = HistorySearchState::new();
        assert!(state.activate(&[], ""));
        state.refresh_items(&entries(&["alpha", "beta"]));
        state.update_query("beta");

        let mut delivered = false;
        for _ in 0..100 {
            if state.poll() && state.result_count() == 1 && state.selected_text() == Some("beta") {
                delivered = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(delivered);
    }

    #[test]
    fn empty_query_returns_all_reversed_most_recent_last() {
        let mut state = HistorySearchState::new();
        // Input is most-recent-first (as `combined_prompt_history` produces).
        let history = entries(&["alpha", "beta", "gamma"]);
        activate_and_poll(&mut state, &history, "");

        assert_eq!(state.result_count(), 3);
        // Reversed: oldest at top, most recent ("alpha") at the bottom.
        assert_eq!(state.result_at(0).unwrap().text, "gamma");
        assert_eq!(state.result_at(1).unwrap().text, "beta");
        assert_eq!(state.result_at(2).unwrap().text, "alpha");
        // Opens with the most-recent prompt (bottom) selected.
        assert_eq!(state.selected, 2);
        assert_eq!(state.selected_text(), Some("alpha"));
    }

    #[test]
    fn non_empty_query_filters() {
        let mut state = HistorySearchState::new();
        let history = entries(&["fix bug", "add feature", "fix typo", "refactor code"]);
        activate_and_poll(&mut state, &history, "");
        query_and_poll(&mut state, "fix");

        assert!(state.result_count() >= 2);
        let texts: Vec<&str> = (0..state.result_count())
            .filter_map(|i| state.result_at(i).map(|r| r.text.as_str()))
            .collect();
        assert!(texts.contains(&"fix bug"));
        assert!(texts.contains(&"fix typo"));
        assert!(!state.result_at(0).unwrap().indices.is_empty());
        // Results are reversed (best match last) and, with no navigation,
        // selection sticks to the bottom-most (best) match.
        assert_eq!(state.selected, state.result_count() - 1);
    }

    #[test]
    fn typing_after_navigation_reanchors_to_best_match() {
        let mut state = HistorySearchState::new();
        activate_and_poll(
            &mut state,
            &entries(&["match1", "match2", "match3", "zzz", "www"]),
            "",
        );
        assert_eq!(state.result_count(), 5);
        assert_eq!(state.selected, 4); // bottom (most recent) selected on open

        // Navigate up off the bottom — selection is no longer sticky.
        state.move_up();
        state.move_up();
        state.move_up();
        assert_eq!(state.selected, 1);

        // Typing a new query re-anchors selection to the best match (bottom).
        query_and_poll(&mut state, "match");
        assert_eq!(state.result_count(), 3);
        assert_eq!(state.selected, state.result_count() - 1);
    }

    #[test]
    fn activate_stores_saved_text() {
        let mut state = HistorySearchState::new();
        assert!(state.activate(&entries(&["hello"]), "my draft"));
        assert!(state.is_active());
        assert_eq!(state.saved_text(), "my draft");
    }

    #[test]
    fn deactivate_clears_state() {
        let mut state = HistorySearchState::new();
        activate_and_poll(&mut state, &entries(&["a", "b"]), "text");
        assert!(state.is_active());
        state.deactivate();
        assert!(!state.is_active());
        assert_eq!(state.result_count(), 0);
    }

    #[test]
    fn opens_with_most_recent_selected_at_bottom() {
        let mut state = HistorySearchState::new();
        // Input most-recent-first: "a" is most recent, "b" is older.
        activate_and_poll(&mut state, &entries(&["a", "b"]), "");
        // Most recent ("a") is reversed to the bottom (last index) and selected.
        assert_eq!(state.selected, 1);
        assert_eq!(state.selected_text(), Some("a"));
    }

    #[test]
    fn move_up_selects_earlier_then_stops_at_top() {
        let mut state = HistorySearchState::new();
        activate_and_poll(&mut state, &entries(&["a", "b"]), "");
        assert_eq!(state.selected, 1);
        // Up moves toward earlier prompts (up the list).
        assert!(state.move_up());
        assert_eq!(state.selected, 0);
        assert_eq!(state.selected_text(), Some("b"));
        // No wrap: at the oldest entry Up stays put.
        assert!(!state.move_up());
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn move_down_at_bottom_reports_end_instead_of_wrapping() {
        let mut state = HistorySearchState::new();
        activate_and_poll(&mut state, &entries(&["a", "b"]), "");
        assert_eq!(state.selected, 1);
        // At the newest (bottom) entry Down reports the end — the caller
        // closes the panel there ("Down right after opening backs out").
        assert!(!state.move_down());
        assert_eq!(state.selected, 1);
        // From an older entry Down moves normally.
        assert!(state.move_up());
        assert!(state.move_down());
        assert_eq!(state.selected, 1);
    }

    #[test]
    fn browse_mode_flag_tracks_activation_kind() {
        let mut state = HistorySearchState::new();
        assert!(state.activate_browse(&entries(&["a"]), ""));
        assert!(state.is_active());
        assert!(state.is_browse());
        state.deactivate();
        assert!(!state.is_browse());
        assert!(state.activate(&entries(&["a"]), ""));
        assert!(state.is_active());
        assert!(!state.is_browse(), "/history search mode is not browse");
    }

    #[test]
    fn no_panic_on_empty_history() {
        let mut state = HistorySearchState::new();
        assert!(state.activate(&[], ""));
        assert_eq!(state.result_count(), 0);
        assert!(state.selected_text().is_none());
        assert!(!state.move_up());
        assert!(!state.move_down());
    }

    #[test]
    fn default_is_inactive() {
        let state = HistorySearchState::default();
        assert!(!state.is_active());
        assert_eq!(state.result_count(), 0);
    }
}
