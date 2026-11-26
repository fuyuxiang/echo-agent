//! File search state: owns the fuzzy matcher daemon, results, and dropdown state.
//!
//! This is the core engine for @-completion. It manages:
//! - A background [`FuzzyFileMatcherDaemon`] that walks the directory tree
//! - The current [`AtContext`] (parsed from prompt text + cursor)
//! - Cached fuzzy match results (polled on tick)
//! - Dropdown selection state (selected index, scroll offset)
//! - Text replacement logic when a result is accepted

use std::path::{Path, PathBuf};
use std::sync::Arc;

use echo_agent_workspace::file_system::{
    FuzzyFileMatcher, FuzzyFileMatcherDaemon, FuzzyMatchResult, FuzzyMatcherDaemonResults,
};

use super::context::{self, AtContext, normalize_display_path};

/// Top-K results to request from the fuzzy matcher.
const MATCHER_TOP_K: usize = 1000;

/// Whether a new query should restart the daemon's directory walk.
enum RestartWalk {
    /// Reuse the current walk (query changed but hidden mode did not).
    Keep,
    /// Restart the walk, including or excluding hidden entries.
    Restart { hidden: bool },
}

/// Replacement to apply to the prompt text after accepting a fuzzy result.
#[derive(Debug, Clone)]
pub struct FileSearchReplacement {
    /// Byte range in the prompt text to replace (excludes the `@`).
    pub range: std::ops::Range<usize>,
    /// Replacement text (the normalized path, possibly with trailing space or `/`).
    pub text: String,
    /// Where to place the cursor after replacement.
    pub cursor: usize,
    /// Whether the @-context should be cleared (an already-present directory was committed).
    pub dismiss: bool,
}

/// Build accepted directory replacement text: append `/` for drill-down and a
/// trailing space when the token ends the prompt.
fn accept_text(path: &str, at_end: bool) -> String {
    let mut text = path.to_owned();
    text.push('/');
    if at_end {
        text.push(' ');
    }
    text
}

/// File search state for @-completion.
pub struct FileSearchState {
    /// Directory the matcher walks. Mirrors the daemon's root (which is
    /// otherwise moved into its worker thread) so callers can introspect
    /// where `@`-completion is currently pointed.
    root: PathBuf,
    /// Background fuzzy matcher daemon, built lazily on first @-use. Eager
    /// construction spawns the nucleo pool and walker threads even in sessions
    /// that never open @-search; deferring it moves that thread spawn, and its
    /// EAGAIN risk, to first use rather than removing it.
    daemon: Option<FuzzyFileMatcherDaemon>,
    /// Test-only count of daemon builds, to prove reuse (no drop-and-rebuild).
    #[cfg(test)]
    daemon_builds: usize,
    /// Latest results snapshot from the daemon.
    results: FuzzyMatcherDaemonResults,
    /// Current @-context (if cursor is inside an @-token).
    context: Option<AtContext>,
    /// Selected index in the dropdown list (keyboard-driven).
    selected: usize,
    /// Hovered index in the dropdown list (mouse-driven).
    /// `None` when the mouse is not over any item.
    hovered: Option<usize>,
    /// Scroll offset for the dropdown list.
    scroll_offset: usize,
    /// Floor for accepted result generations: the stale-result fence.
    ///
    /// Rises monotonically and is never lowered. Each new query bumps it (see
    /// `start_query`); the daemon paces its own per-tick `generation`
    /// independently, so `poll` drops any snapshot whose `generation` predates
    /// the floor and, on accept, raises the floor to the accepted snapshot's
    /// generation. This keeps matches from a prior query from flickering in.
    min_generation: usize,
    /// Directory being drilled into; keeps the @-token alive when its name has
    /// whitespace (`my dir`). Self-validating — applies only while the path matches.
    drill_prefix: Option<String>,
}

impl FileSearchState {
    /// Create a new file search state rooted at the given path.
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_owned(),
            daemon: None,
            #[cfg(test)]
            daemon_builds: 0,
            results: FuzzyMatcherDaemonResults::default(),
            context: None,
            selected: 0,
            hovered: None,
            scroll_offset: 0,
            min_generation: 0,
            drill_prefix: None,
        }
    }

    /// Point @-completion at a new tree (e.g. after worktree creation).
    ///
    /// Drops any built daemon; the next @-use rebuilds it lazily against `root`.
    pub fn retarget(&mut self, root: &Path) {
        *self = Self::new(root);
    }

    /// The directory the matcher currently walks (the `@`-completion root).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The fuzzy matcher daemon, built lazily on first use.
    ///
    /// The first `@`-keystroke pays a one-time cost on the UI thread: building
    /// the daemon spawns the nucleo matcher pool and the directory walker.
    fn ensure_daemon(&mut self) -> &mut FuzzyFileMatcherDaemon {
        if self.daemon.is_none() {
            let daemon =
                FuzzyFileMatcherDaemon::new(FuzzyFileMatcher::new(&self.root), MATCHER_TOP_K);
            self.daemon = Some(daemon);
            #[cfg(test)]
            {
                self.daemon_builds += 1;
            }
        }
        self.daemon.as_mut().expect("daemon built above")
    }

    /// Point the daemon (building it if needed) at `query`, optionally restarting
    /// the directory walk, then reset dropdown selection and scroll.
    ///
    /// The matcher never filters to directories only: a trailing `/` scopes the
    /// query to a folder without hiding that folder's files.
    fn start_query(&mut self, restart: RestartWalk, query: &str) {
        let daemon = self.ensure_daemon();
        if let RestartWalk::Restart { hidden } = restart {
            daemon.restart_walk(hidden);
        }
        daemon.set_query(query, false);
        // Advance the stale-result fence past the prior query (see `min_generation`).
        self.min_generation += 1;
        self.selected = 0;
        self.hovered = None;
        self.scroll_offset = 0;
    }

    // ── Visibility ──────────────────────────────────────────────────────

    /// Whether the dropdown should be visible.
    pub fn is_visible(&self) -> bool {
        self.context.is_some() && !self.results.topk.is_empty()
    }

    /// The current @-context, if any.
    pub fn context(&self) -> Option<&AtContext> {
        self.context.as_ref()
    }

    /// The current results snapshot.
    pub fn results(&self) -> &FuzzyMatcherDaemonResults {
        &self.results
    }

    /// Currently selected index in the results.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Scroll offset for the dropdown.
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Currently hovered index (mouse-driven), if any.
    pub fn hovered(&self) -> Option<usize> {
        self.hovered
    }

    /// Set the hovered index. Returns `true` if changed.
    pub fn set_hovered(&mut self, index: Option<usize>) -> bool {
        let clamped = index.filter(|&i| i < self.results.topk.len());
        let changed = clamped != self.hovered;
        self.hovered = clamped;
        changed
    }

    /// Whether the current query is in directory-only mode.
    pub fn is_dir_mode(&self) -> bool {
        self.context.as_ref().is_some_and(|c| c.is_dir_mode())
    }

    // ── Context updates ─────────────────────────────────────────────────

    /// Anchor (or clear) the drilled directory for whitespace-aware detection.
    pub fn set_drill_prefix(&mut self, prefix: Option<String>) {
        self.drill_prefix = prefix;
    }

    /// Recompute the @-context from the current prompt text and cursor position.
    ///
    /// Called after every text change or cursor movement.
    pub fn update_context(&mut self, text: &str, cursor: usize) {
        let new_ctx = context::detect_with_drill(text, cursor, self.drill_prefix.as_deref());

        match (&self.context, &new_ctx) {
            (None, Some(ctx)) => {
                // Fresh `@` token is never a drill — drop any stale anchor.
                self.drill_prefix = None;
                // Entering @-mode always restarts the walk.
                self.start_query(
                    RestartWalk::Restart {
                        hidden: ctx.is_hidden_mode(),
                    },
                    ctx.matcher_query(),
                );
            }
            (Some(old), Some(new)) => {
                // Drop a stale anchor once the @-token's path content no longer
                // starts with it (e.g. undo/paste reverted the drill), so it
                // can't silently re-match on a later edit.
                let anchor_stale = self.drill_prefix.as_deref().is_some_and(|prefix| {
                    !text
                        .get(new.path_range().start..)
                        .is_some_and(|rest| rest.starts_with(prefix))
                });
                if anchor_stale {
                    self.drill_prefix = None;
                }
                // Staying in @-mode only re-walks when hidden mode toggled.
                let restart = if old.is_hidden_mode() != new.is_hidden_mode() {
                    RestartWalk::Restart {
                        hidden: new.is_hidden_mode(),
                    }
                } else {
                    RestartWalk::Keep
                };
                self.start_query(restart, new.matcher_query());
            }
            (Some(_), None) => {
                // Leaving @-mode: clear results and the drill anchor.
                self.context = None;
                self.drill_prefix = None;
                self.results = FuzzyMatcherDaemonResults::default();
                return;
            }
            (None, None) => return,
        }

        self.context = new_ctx;
        // Both @-mode arms build the daemon via `start_query`, so an active
        // context implies a built daemon.
        debug_assert!(self.context.is_none() || self.daemon.is_some());
    }

    /// Clear the context (e.g., on Esc).
    pub fn clear_context(&mut self) {
        self.context = None;
        self.drill_prefix = None;
        self.results = FuzzyMatcherDaemonResults::default();
    }

    // ── Tick / polling ──────────────────────────────────────────────────

    /// Poll the daemon for new results. Returns `true` if results changed.
    ///
    /// Should be called on every tick (~4ms) while the dropdown is potentially visible.
    pub fn poll(&mut self) -> bool {
        if self.context.is_none() {
            return false;
        }

        // Never build the daemon on the poll path: no daemon means no results yet.
        let Some(daemon) = self.daemon.as_ref() else {
            return false;
        };
        let results = daemon.get();

        if Arc::ptr_eq(&results.topk, &self.results.topk) {
            return false;
        }

        // Avoid flickering: skip empty intermediate results unless matching is done.
        if !results.topk.is_empty() || results.status.done {
            // Skip stale generations (e.g., from a previous @-context).
            if results.generation >= self.min_generation {
                self.min_generation = results.generation;
                self.results = results;
                if !self.results.topk.is_empty() {
                    self.selected = self.selected.min(self.results.topk.len() - 1);
                }
                return true;
            }
        }

        false
    }

    // ── Navigation ──────────────────────────────────────────────────────

    /// Move selection by `delta` items (negative = up, positive = down).
    pub fn move_selection(&mut self, delta: isize) {
        let len = self.results.topk.len();
        if len == 0 {
            return;
        }
        let max_idx = len - 1;
        let current = self.selected.min(max_idx);
        self.selected = (current as isize + delta).clamp(0, max_idx as isize) as usize;
    }

    /// Move selection by a page (half of visible height).
    pub fn page_move(&mut self, delta: isize, visible_rows: usize) {
        let half = (visible_rows / 2).max(1) as isize;
        self.move_selection(delta * half);
    }

    /// Ensure the selected item is visible in the dropdown viewport.
    pub fn ensure_visible(&mut self, visible_rows: usize) {
        if visible_rows == 0 {
            return;
        }
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + visible_rows {
            self.scroll_offset = self.selected + 1 - visible_rows;
        }
    }

    // ── Selection / replacement ─────────────────────────────────────────

    /// Select the hovered item (for click-to-accept).
    /// Returns `true` if there was a valid hovered item to select.
    pub fn select_hovered(&mut self) -> bool {
        if let Some(idx) = self.hovered
            && idx < self.results.topk.len()
        {
            self.selected = idx;
            return true;
        }
        false
    }

    /// Get the currently selected fuzzy match result.
    pub fn selected_result(&self) -> Option<&FuzzyMatchResult> {
        self.results.topk.get(self.selected)
    }

    /// Compute the text replacement for accepting the currently selected
    /// directory (drill-down acceptance).
    ///
    /// Pure query. `dismiss` reports whether the caller should clear the
    /// context: a directory whose `/`-append matches text already present is
    /// committed (dismiss), otherwise the caller drills in and stays open. The
    /// `src` parameter is the full prompt text, needed to detect that no-op
    /// `/`-append.
    pub fn try_replace(&self, src: &str) -> Option<FileSearchReplacement> {
        let ctx = self.context.as_ref()?;
        let res = self.results.topk.get(self.selected)?;

        // Dir-only contract: this always appends `/`, so it is valid only for a
        // directory chosen in dir mode. Enforce it here so a file-selection
        // caller can never emit `some/file.rs/`.
        if !res.is_dir || !ctx.is_dir_mode() {
            return None;
        }

        // Replace only the path portion of the @-token (preserving `@` and any
        // hidden-mode `!` marker). See `AtContext::path_range`.
        let range = ctx.path_range();
        let path = normalize_display_path(&res.path.to_string()).to_owned();
        let at_end = range.end == src.len();

        // A `/`-append that matches text already present commits the dir and
        // dismisses; otherwise it drills in and stays open.
        let no_op = src.get(range.clone()) == Some(accept_text(&path, false).as_str());
        let text = accept_text(&path, no_op && at_end);

        // Cursor sits just past the emitted text (after the trailing `/`).
        let mut cursor = range.start + text.len();
        // A committed dir that is not at the prompt end keeps its existing
        // terminator (whitespace, `,`, or `;`, possibly multibyte; see
        // `context::detect`); step past that one char so typing resumes after
        // the directory.
        if no_op && !at_end {
            cursor += src[range.end..].chars().next().map_or(1, char::len_utf8);
        }

        Some(FileSearchReplacement {
            text,
            range,
            cursor,
            dismiss: no_op,
        })
    }

    /// Number of result items.
    pub fn result_count(&self) -> usize {
        self.results.topk.len()
    }

    /// Total items the matcher knows about (for "k/n" display).
    pub fn total_items(&self) -> usize {
        self.results.num_items
    }

    /// Test-only: install a fake context + results snapshot so tests can drive
    /// acceptance flows without spinning up the background fuzzy daemon.
    ///
    /// Bumps `min_generation` past the seeded generation so any in-flight real
    /// daemon poll is rejected and cannot clobber the seeded state.
    #[cfg(test)]
    pub(crate) fn set_test_state(
        &mut self,
        context: AtContext,
        results: Vec<FuzzyMatchResult>,
        selected: usize,
    ) {
        self.context = Some(context);
        self.results = FuzzyMatcherDaemonResults {
            topk: Arc::from(results),
            num_items: 0,
            status: Default::default(),
            generation: self.min_generation,
        };
        self.min_generation += 1;
        self.selected = selected;
    }

    /// Test-only observable state: whether the lazy daemon has been built yet.
    #[cfg(test)]
    pub(crate) fn daemon_is_built(&self) -> bool {
        self.daemon.is_some()
    }

    /// Test-only observable state: how many times the lazy daemon has been built.
    #[cfg(test)]
    pub(crate) fn daemon_build_count(&self) -> usize {
        self.daemon_builds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_result(path: &str) -> FuzzyMatchResult {
        FuzzyMatchResult {
            path: nucleo::Utf32String::from(path),
            is_dir: true,
            ..Default::default()
        }
    }

    #[test]
    fn try_replace_commits_directory_already_present() {
        // The selected dir's `/`-append already matches the token text, so
        // acceptance commits (dismiss) rather than drilling.
        let mut state = FileSearchState::new(Path::new("."));

        // At the prompt end: append a trailing space so typing can continue.
        let src = "@src/";
        let ctx = context::detect(src, src.len()).expect("context");
        state.set_test_state(ctx, vec![dir_result("src")], 0);
        let r = state.try_replace(src).expect("replacement");
        assert!(r.dismiss);
        assert_eq!(r.range, 1..5);
        assert_eq!(r.text, "src/ ");
        assert_eq!(r.cursor, "@src/ ".len());

        // Mid-prompt: no appended space; step past the existing terminator.
        let src = "@src/ tail";
        let ctx = context::detect(src, 5).expect("context");
        state.set_test_state(ctx, vec![dir_result("src")], 0);
        let r = state.try_replace(src).expect("replacement");
        assert!(r.dismiss);
        assert_eq!(r.text, "src/");
        assert_eq!(r.cursor, 6);

        // Mid-prompt with a multibyte terminator: step past the whole char.
        let src = "@src/\u{a0}tail";
        let ctx = context::detect(src, 5).expect("context");
        state.set_test_state(ctx, vec![dir_result("src")], 0);
        let r = state.try_replace(src).expect("replacement");
        assert!(r.dismiss);
        assert_eq!(r.text, "src/");
        assert_eq!(r.cursor, 5 + '\u{a0}'.len_utf8());
    }

    #[test]
    fn retarget_drops_built_daemon() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut state = FileSearchState::new(dir.path());
        state.update_context("@alpha", "@alpha".len());
        assert!(state.daemon_is_built());

        state.retarget(Path::new(".."));
        assert_eq!(state.root(), Path::new(".."));
        assert!(!state.daemon_is_built());
    }

    #[test]
    fn poll_does_not_build_daemon() {
        let mut state = FileSearchState::new(Path::new("."));
        // With no @-context, poll returns early and never touches the daemon.
        assert!(!state.poll());
        assert!(!state.daemon_is_built());

        // With an @-context but an unbuilt daemon, poll must not force construction.
        let ctx = context::detect("@foo", 4).expect("context");
        state.set_test_state(ctx, Vec::new(), 0);
        assert!(state.context().is_some());
        assert!(!state.poll());
        assert!(!state.daemon_is_built());
    }

    #[test]
    fn daemon_is_built_lazily_on_first_use() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut state = FileSearchState::new(dir.path());
        assert!(!state.daemon_is_built());

        // The first @-search interaction builds the daemon lazily.
        state.update_context("@alpha", "@alpha".len());
        assert!(state.daemon_is_built());
        assert!(state.context().is_some());

        // A query edit stays in @-mode and reuses the same daemon: the build
        // count stays at 1, proving no drop-and-rebuild.
        assert_eq!(state.daemon_build_count(), 1);
        state.update_context("@alpha_marker", "@alpha_marker".len());
        assert!(state.daemon_is_built());
        assert_eq!(state.daemon_build_count(), 1);
    }
}
