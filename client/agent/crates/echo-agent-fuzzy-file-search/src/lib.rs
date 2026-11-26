//! Fuzzy file search over a directory tree.
//!
//! An `ignore` walk feeds paths into a `nucleo` matcher; [`FuzzyFileMatcher`]
//! owns that pair and [`FuzzyFileMatcherDaemon`] drives it from a background
//! thread so callers can poll for the current top-k. Every stage degrades
//! rather than aborting when a thread cannot be spawned: an empty query still
//! browses from a serial top-level walk, and a fully refused matcher returns
//! empty results.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{RecvTimeoutError, SyncSender, sync_channel},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use ignore::{DirEntry, WalkBuilder, WalkState, overrides::OverrideBuilder};
use nucleo::{
    Match, Matcher, Nucleo, Snapshot, Utf32String,
    pattern::{CaseMatching, MultiPattern, Normalization, Pattern},
};
use serde::Serialize;

const NUM_NUCLEO_THREADS: usize = 2;
const NUM_IGNORE_THREADS: usize = 8;

/// What a fuzzy matcher can serve. Browsing is a serial depth-1 walk that needs
/// no thread pool; only keyed matching needs the nucleo pool, and only the
/// daemon needs its worker thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatcherMode {
    /// Nucleo pool up: keyed fuzzy queries and empty-query browsing.
    Full,
    /// Nucleo pool refused: empty-query browsing only; keyed queries are empty.
    BrowseOnly,
    /// Daemon worker thread refused: every result is empty.
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WalkMode {
    Parallel,
    Serial,
}

#[derive(Debug, Clone, Default)]
pub struct FuzzyMatchResult {
    /// Path of the matched entry.
    pub path: Utf32String,
    /// Matcher score, higher is better.
    pub score: u32,
    /// Matched indices of characters.
    pub indices: Vec<u32>,
    /// Is it a directory.
    pub is_dir: bool,
}

impl Serialize for FuzzyMatchResult {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        use std::borrow::Cow;

        let path_str = self.path.to_string();
        let node_type = if self.is_dir { "directory" } else { "file" };
        let name: Cow<str> = std::path::Path::new(&path_str)
            .file_name()
            .map(|s| s.to_string_lossy())
            .unwrap_or(Cow::Borrowed(&path_str));

        let mut state = serializer.serialize_struct("FuzzyMatchResult", 5)?;
        state.serialize_field("name", &name)?;
        state.serialize_field("type", node_type)?;
        state.serialize_field("path", &path_str)?;
        state.serialize_field("score", &self.score)?;
        state.serialize_field("indices", &self.indices)?;
        state.end()
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FuzzyMatcherStatus {
    pub changed: bool,
    pub done: bool,
}

#[derive(Debug, Clone)]
struct MatchEntry {
    pub is_dir: bool,
}

fn check_entry<'a>(entry: &'a DirEntry, root: &Path) -> Option<(&'a str, bool)> {
    let path = entry.path();
    if path != root
        && let Some(file_type) = entry.file_type()
        && (file_type.is_file() || file_type.is_dir())
        && let Ok(path) = path.strip_prefix(root)
        && let Some(path) = path.as_os_str().to_str()
        && !path.is_empty()
    {
        Some((path, file_type.is_dir()))
    } else {
        None
    }
}

fn push_match(
    injector: &nucleo::Injector<MatchEntry>,
    root: &Path,
    entry: Result<DirEntry, ignore::Error>,
) {
    if let Ok(entry) = entry
        && let Some((path, is_dir)) = check_entry(&entry, root)
    {
        injector.push(MatchEntry { is_dir }, |_entry, columns| {
            columns[0] = path.into();
        });
    }
}

/// Whether `n` OS threads can be spawned (each joined before return).
/// Probe-then-build is racy.
fn threads_spawnable(n: usize) -> bool {
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let mut handles = Vec::with_capacity(n);
    let mut all_spawned = true;
    for _ in 0..n {
        let gate = gate.clone();
        match thread::Builder::new()
            .name("thread-probe".into())
            .spawn(move || {
                let (lock, cv) = &*gate;
                let mut released = lock.lock().unwrap_or_else(|e| e.into_inner());
                while !*released {
                    released = cv.wait(released).unwrap_or_else(|e| e.into_inner());
                }
            }) {
            Ok(handle) => handles.push(handle),
            Err(_) => {
                all_spawned = false;
                break;
            }
        }
    }
    let (lock, cv) = &*gate;
    *lock.lock().unwrap_or_else(|e| e.into_inner()) = true;
    cv.notify_all();
    for handle in handles {
        let _ = handle.join();
    }
    all_spawned
}

/// Probes `NUM_IGNORE_THREADS + 1` because `build_parallel` runs inside the
/// spawned fuzzy-walk thread, which itself builds the ignore pool.
fn choose_walk_mode(nucleo_enabled: bool, probe: impl Fn(usize) -> bool) -> WalkMode {
    if nucleo_enabled && probe(NUM_IGNORE_THREADS + 1) {
        WalkMode::Parallel
    } else {
        WalkMode::Serial
    }
}

/// A very fast fuzzy matcher that does ignore-walking. Both happen in background threads.
pub struct FuzzyFileMatcher {
    root: PathBuf,
    query: String,
    /// `None` when the matcher pool cannot be spawned; keyed matching then
    /// degrades to `MatcherMode::BrowseOnly`. See `mode`.
    nucleo: Option<Nucleo<MatchEntry>>,
    matcher: Matcher,
    walk_handle: Option<JoinHandle<()>>,
    cancel: Arc<AtomicBool>,
    top_entries: Vec<FuzzyMatchResult>,
    dirs: bool,
}

impl FuzzyFileMatcher {
    /// Create a new matcher with default config focused on matching paths.
    ///
    /// If the matcher thread pool cannot be spawned (cgroup pids / `RLIMIT_NPROC`
    /// exhaustion), the matcher degrades to browse-only: it logs once and keyed
    /// queries return no matches (see [`Self::is_enabled`]). Empty-query browsing
    /// still works from the serial top-level walk.
    ///
    /// The probe asks for `NUM_NUCLEO_THREADS + 1`: peak demand is the persistent
    /// nucleo pool plus the daemon's worker thread, so reserving the extra slot
    /// keeps the daemon spawn (browse) from being starved by the pool.
    pub fn new(root: &Path) -> Self {
        Self::new_inner(root, threads_spawnable(NUM_NUCLEO_THREADS + 1))
    }

    fn new_inner(root: &Path, nucleo_available: bool) -> Self {
        let matcher_config = nucleo::Config::DEFAULT.match_paths();

        let nucleo = nucleo_available.then(|| {
            let mut nucleo = Nucleo::new(
                matcher_config.clone(),
                Arc::new(move || ()),
                Some(NUM_NUCLEO_THREADS),
                1,
            );
            nucleo.pattern = MultiPattern::new(1);
            nucleo
        });
        if nucleo.is_none() {
            tracing::error!(
                "keyed fuzzy search disabled (browse-only): cannot spawn matcher \
                 threads (out of thread slots / cgroup pids cap)"
            );
        }

        Self {
            root: root.to_owned(),
            nucleo,
            matcher: Matcher::new(matcher_config),
            walk_handle: None,
            cancel: Arc::new(AtomicBool::new(false)),
            query: String::new(),
            top_entries: Vec::new(),
            dirs: false,
        }
    }

    /// This matcher's own capability; it is never `Disabled` because the serial
    /// top-level walk browses without the nucleo pool.
    fn mode(&self) -> MatcherMode {
        if self.nucleo.is_some() {
            MatcherMode::Full
        } else {
            MatcherMode::BrowseOnly
        }
    }

    /// Whether keyed fuzzy matching is active. When false, only empty-query
    /// browsing returns results; keyed queries return empty.
    fn is_enabled(&self) -> bool {
        matches!(self.mode(), MatcherMode::Full)
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// Start a new walk and restart nucleo matcher.
    pub fn restart_walk_with(
        &mut self,
        make_walker: impl FnOnce(&mut WalkBuilder) -> &mut WalkBuilder,
    ) {
        // Join the previous walk first so the probe measures freed threads, not
        // the outgoing walk's.
        self.join_walk();
        let mode = choose_walk_mode(self.is_enabled(), threads_spawnable);
        self.restart_walk_inner(make_walker, mode);
    }

    /// Cancel the current walk and join its thread. Idempotent.
    fn join_walk(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        // Join without unwrapping so a panicked walk thread does not re-panic.
        if let Some(walk_handle) = self.walk_handle.take() {
            let _ = walk_handle.join();
        }
    }

    /// Sorted top-level entries for empty-query browsing (a serial, depth-1
    /// walk, so it works even when the matcher is disabled).
    fn collect_top_entries(&self, walker_builder: &WalkBuilder) -> Vec<FuzzyMatchResult> {
        walker_builder
            .clone()
            .max_depth(Some(1))
            .sort_by_file_name(|a, b| a.cmp(b))
            .build()
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let (path, is_dir) = check_entry(&entry, &self.root)?;
                Some(FuzzyMatchResult {
                    path: path.into(),
                    score: 0,
                    indices: Vec::new(),
                    is_dir,
                })
            })
            .collect()
    }

    /// The shared git/ignore walk configuration, before per-walk tweaks.
    /// `threads` only affects `build_parallel()`; the serial `build()` fallback
    /// ignores it and spawns no threads.
    fn base_walker(&self) -> WalkBuilder {
        let mut builder = WalkBuilder::new(&self.root);
        builder
            .threads(NUM_IGNORE_THREADS)
            .follow_links(false)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .ignore(true)
            .hidden(true)
            .require_git(false)
            .overrides(
                OverrideBuilder::new(&self.root)
                    .add("!.git")
                    .expect("static \"!.git\" override must parse")
                    .build()
                    .expect("static \"!.git\" override must build"),
            );
        builder
    }

    /// Restart the walk with a pre-computed [`WalkMode`].
    ///
    /// Assumes any prior walk is already joined: `restart_walk_with` joins
    /// before probing, and the direct test-seam callers start from a fresh
    /// matcher.
    fn restart_walk_inner(
        &mut self,
        make_walker: impl FnOnce(&mut WalkBuilder) -> &mut WalkBuilder,
        mode: WalkMode,
    ) {
        debug_assert!(
            self.walk_handle.is_none(),
            "restart_walk_inner requires the prior walk to be joined"
        );
        if let Some(nucleo) = self.nucleo.as_mut() {
            nucleo.restart(true);
        }
        self.cancel.store(false, Ordering::Relaxed);

        let mut base = self.base_walker();
        let walker_builder = make_walker(&mut base).clone();

        self.top_entries = self.collect_top_entries(&walker_builder);

        let injector = self.nucleo.as_mut().map(|nucleo| {
            let injector = nucleo.injector();
            nucleo.tick(0);
            injector
        });
        let Some(injector) = injector else {
            // Disabled matcher: browsing still works from `top_entries`, but
            // there is no background walk to feed.
            tracing::debug!("fuzzy walk skipped: matcher disabled");
            return;
        };

        let root = self.root.clone();
        let cancel = self.cancel.clone();
        let walk = thread::Builder::new()
            .name("fuzzy-walk".into())
            .spawn(move || {
                if mode == WalkMode::Parallel {
                    walker_builder.build_parallel().run(|| {
                        let injector = injector.clone();
                        let root = root.clone();
                        let cancel = cancel.clone();
                        Box::new(move |entry| {
                            if cancel.load(Ordering::Relaxed) {
                                return WalkState::Quit;
                            }
                            push_match(&injector, &root, entry);
                            WalkState::Continue
                        })
                    });
                } else {
                    // Serial fallback: `Walk` spawns no threads.
                    tracing::debug!("fuzzy walk running serially (parallel pool unavailable)");
                    for entry in walker_builder.build() {
                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        push_match(&injector, &root, entry);
                    }
                }
            });
        match walk {
            Ok(handle) => self.walk_handle = Some(handle),
            Err(e) => tracing::error!(
                error = %e,
                "fuzzy walk thread spawn failed; file search results unavailable this walk"
            ),
        }
    }

    /// Restart the walk with default walker parameters.
    pub fn restart_walk(&mut self) {
        self.restart_walk_with(|w| w);
    }

    /// Set the query to a given string and trigger reparse.
    ///
    /// It will be faster if the current query is a strict prefix of the new query.
    pub fn set_query(&mut self, mut query: &str, dirs: bool) {
        self.dirs = dirs;
        if dirs && query.ends_with('/') {
            query = &query[..query.len() - 1];
        }
        if query == self.query {
            return;
        }
        // see this re: backslash etc: https://github.com/helix-editor/nucleo/pull/87
        let append = query.as_bytes().starts_with(self.query.as_bytes())
            && !query.ends_with('\\')
            && !query
                .as_bytes()
                .last()
                .is_some_and(|ch| ch.is_ascii_whitespace());
        if let Some(nucleo) = self.nucleo.as_mut() {
            nucleo
                .pattern
                .reparse(0, query, CaseMatching::Smart, Normalization::Smart, append);
            nucleo.tick(0);
        }
        self.query = query.to_owned();
    }

    /// Sends a tick to nucleo matcher. Can be safely called at any frequency.
    pub fn tick(&mut self, tick_timeout_ms: u64) -> FuzzyMatcherStatus {
        if self.query.is_empty() {
            return FuzzyMatcherStatus {
                done: true,
                changed: false,
            };
        }
        let Some(nucleo) = self.nucleo.as_mut() else {
            return FuzzyMatcherStatus {
                done: true,
                changed: false,
            };
        };
        let status = nucleo.tick(tick_timeout_ms);
        let done = nucleo.active_injectors() == 0 && !status.running;
        FuzzyMatcherStatus {
            done,
            changed: status.changed,
        }
    }

    /// Total number of currently matched items in the snapshot.
    pub fn num_items(&self) -> usize {
        if self.query.is_empty() {
            self.top_entries.len()
        } else {
            self.nucleo
                .as_ref()
                .map_or(0, |nucleo| nucleo.snapshot().item_count() as _)
        }
    }

    /// Get top `k` items from the snapshot and sort them by score, path length and path.
    pub fn get_top_k(&mut self, k: usize) -> Vec<FuzzyMatchResult> {
        // note: &mut only because we access self.matcher which has internal allocations

        // A HRTB helper so we can sort by a borrowed key without cloning.
        fn sort_by_key_hrtb<T, F, K, Q>(slice: &mut [T], f: F)
        where
            F: for<'a> Fn(&'a T) -> (Q, &'a K),
            K: Ord,
            Q: Ord,
        {
            slice.sort_by(|a, b| f(a).cmp(&f(b)))
        }

        if self.query.is_empty() {
            return self
                .top_entries
                .iter()
                .filter(|e| !self.dirs || e.is_dir)
                .take(k)
                .cloned()
                .collect();
        }

        // https://github.com/helix-editor/helix/blob/d79cce4e4bfc24dd204f1b294c899ed73f7e9453/helix-term/src/ui/completion.rs#L369
        // suggested min score = 7 * len + 14
        let len = self.query.chars().count() as u32;
        let min_score = 7 + len * 14;

        let Some(nucleo) = self.nucleo.as_ref() else {
            return Vec::new();
        };

        let mut items = Vec::with_capacity(k);
        let pattern = nucleo.pattern.column_pattern(0);
        let snapshot = nucleo.snapshot();
        let mut iter = snapshot.matches().iter().peekable();

        while items.len() < k
            && let Some(m) = iter.next()
            && m.score >= min_score
        {
            fn extract_match(
                m: &Match,
                snapshot: &Snapshot<MatchEntry>,
                pattern: &Pattern,
                matcher: &mut Matcher,
                dirs_only: bool,
            ) -> Option<FuzzyMatchResult> {
                // SAFETY: `m.idx` comes from this snapshot's own match list, so
                // it is a valid index into the snapshot.
                let item = unsafe { snapshot.get_item_unchecked(m.idx) };
                if dirs_only && !item.data.is_dir {
                    return None;
                }
                let path = item.matcher_columns[0].clone();
                let mut indices = Vec::new();
                if !pattern.atoms.is_empty() {
                    pattern.indices(path.slice(..), matcher, &mut indices);
                }
                Some(FuzzyMatchResult {
                    path,
                    score: m.score,
                    indices,
                    is_dir: item.data.is_dir,
                })
            }

            if !pattern.atoms.is_empty() {
                let start = items.len();
                items.extend(extract_match(
                    m,
                    snapshot,
                    pattern,
                    &mut self.matcher,
                    self.dirs,
                ));
                while iter.peek().is_some_and(|p| p.score == m.score) {
                    let m = iter.next().unwrap();
                    items.extend(extract_match(
                        m,
                        snapshot,
                        pattern,
                        &mut self.matcher,
                        self.dirs,
                    ));
                }
                sort_by_key_hrtb(&mut items[start..], |m| (m.path.len(), &m.path));
            } else {
                items.extend(extract_match(
                    m,
                    snapshot,
                    pattern,
                    &mut self.matcher,
                    self.dirs,
                ));
            }
        }

        if items.len() > k {
            items.truncate(k);
        }

        if pattern.atoms.is_empty() {
            sort_by_key_hrtb(&mut items, |m| (true, &m.path));
        }

        items
    }
}

impl Drop for FuzzyFileMatcher {
    fn drop(&mut self) {
        // Join the walk (join_walk sets cancel) so it stops before nucleo drops.
        self.join_walk();
    }
}

#[derive(Debug, Clone, Default)]
pub struct FuzzyMatcherDaemonResults {
    pub topk: Arc<[FuzzyMatchResult]>,
    pub num_items: usize,
    pub status: FuzzyMatcherStatus,
    pub generation: usize,
}

impl AsRef<[FuzzyMatchResult]> for FuzzyMatcherDaemonResults {
    fn as_ref(&self) -> &[FuzzyMatchResult] {
        self.topk.as_ref()
    }
}

#[derive(Debug, Clone)]
enum FuzzyMatcherDaemonMessage {
    RestartWalk { hidden: bool },
    SetQuery { query: String, dirs: bool },
    Stop,
}

pub struct FuzzyFileMatcherDaemon {
    results: Arc<Mutex<FuzzyMatcherDaemonResults>>,
    tx: SyncSender<FuzzyMatcherDaemonMessage>,
    /// `None` when the daemon thread cannot be spawned; messages are dropped.
    /// Joined in `Drop` for deterministic teardown.
    handle: Option<JoinHandle<()>>,
    /// Served capability. `Disabled` means the worker thread was refused, so
    /// `get` yields only empty results; `BrowseOnly` still returns browse hits.
    mode: MatcherMode,
}

impl FuzzyFileMatcherDaemon {
    pub fn new(mut matcher: FuzzyFileMatcher, topk: usize) -> Self {
        let results = Arc::new(Mutex::new(FuzzyMatcherDaemonResults::default()));
        let (tx, rx) = sync_channel(1024);

        let matcher_mode = matcher.mode();
        let res = results.clone();
        let handle = thread::Builder::new()
            .name("fuzzy-daemon".into())
            .spawn(move || {
                let results = res;
                let mut done = false;
                let mut generation = 0;
                loop {
                    let msg = if !done {
                        rx.recv_timeout(Duration::from_micros(250))
                    } else {
                        rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
                    };
                    match msg {
                        Ok(FuzzyMatcherDaemonMessage::RestartWalk { hidden }) => {
                            if !hidden {
                                tracing::trace!("restarting normal walk");
                                matcher.restart_walk();
                            } else {
                                tracing::trace!("restarting hidden walk");
                                matcher.restart_walk_with(|w| {
                                    w.hidden(false).ignore(false).git_ignore(false)
                                });
                            }
                            generation += 1;
                            *results.lock().unwrap() = FuzzyMatcherDaemonResults::default();
                            done = false;
                        }
                        Ok(FuzzyMatcherDaemonMessage::SetQuery { query, dirs }) => {
                            matcher.set_query(&query, dirs);
                            generation += 1;
                            done = false;
                        }
                        Ok(FuzzyMatcherDaemonMessage::Stop)
                        | Err(RecvTimeoutError::Disconnected) => {
                            break;
                        }
                        Err(RecvTimeoutError::Timeout) => {
                            if !done {
                                let status = matcher.tick(10);
                                done = status.done;
                                let num_items = matcher.num_items();
                                let topk: Arc<[_]> = matcher.get_top_k(topk).into();
                                *results.lock().unwrap() = FuzzyMatcherDaemonResults {
                                    topk,
                                    num_items,
                                    status,
                                    generation,
                                };
                                generation += 1;
                            }
                        }
                    }
                }
            });

        let (handle, mode) = match handle {
            Ok(handle) => (Some(handle), matcher_mode),
            Err(e) => {
                // nucleo built but the daemon thread was refused: degrade to
                // fully disabled, so `get` reads empty and terminal.
                tracing::error!(
                    error = %e,
                    "fuzzy daemon thread spawn failed; file search disabled"
                );
                (None, MatcherMode::Disabled)
            }
        };

        Self {
            results,
            tx,
            handle,
            mode,
        }
    }

    /// Latest results. A `Disabled` daemon (worker thread refused) never
    /// populated them, so this yields only empty results.
    pub fn get(&self) -> FuzzyMatcherDaemonResults {
        if self.mode == MatcherMode::Disabled {
            // Terminal empty state. `generation` is MAX so it also clears the
            // callers' `generation >= min_gen` gate: otherwise a disabled search
            // reads as perpetually pending once the first query bumps min_gen
            // past zero.
            return FuzzyMatcherDaemonResults {
                status: FuzzyMatcherStatus {
                    done: true,
                    changed: false,
                },
                generation: usize::MAX,
                ..Default::default()
            };
        }
        self.results.lock().unwrap().clone()
    }

    pub fn set_query(&self, query: impl AsRef<str>, dirs: bool) {
        let query = query.as_ref().to_owned();
        let _ = self
            .tx
            .send(FuzzyMatcherDaemonMessage::SetQuery { query, dirs });
    }

    pub fn restart_walk(&self, hidden: bool) {
        let _ = self
            .tx
            .send(FuzzyMatcherDaemonMessage::RestartWalk { hidden });
    }
}

impl Drop for FuzzyFileMatcherDaemon {
    fn drop(&mut self) {
        let _ = self.tx.send(FuzzyMatcherDaemonMessage::Stop);
        // Join for deterministic teardown: the loop breaks on Stop and dropping
        // the matcher cancels the walk. `None` if the thread never spawned.
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    //! Regression guards: a refused thread degrades fuzzy search rather than
    //! aborting under `panic = "abort"`.

    use super::*;

    fn temp_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("alpha.txt"), b"x").unwrap();
        std::fs::write(dir.path().join("beta.txt"), b"y").unwrap();
        dir
    }

    fn drain_until_done(matcher: &mut FuzzyFileMatcher) {
        for _ in 0..1000 {
            if matcher.tick(10).done {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn disabled_matcher_degrades_without_panicking() {
        let dir = temp_repo();
        let mut matcher = FuzzyFileMatcher::new_inner(dir.path(), false);
        assert!(
            !matcher.is_enabled(),
            "a refused threadpool disables search instead of building nucleo"
        );

        matcher.restart_walk();
        assert!(
            matcher.num_items() >= 2,
            "empty-query browsing still lists files from the serial top walk"
        );
        assert!(!matcher.get_top_k(10).is_empty());

        matcher.set_query("alpha", false);
        assert!(matcher.tick(10).done, "a disabled tick is immediately done");
        assert_eq!(matcher.num_items(), 0);
        assert!(matcher.get_top_k(10).is_empty());
    }

    #[test]
    fn both_walk_paths_feed_the_matcher() {
        let dir = temp_repo();
        for mode in [WalkMode::Serial, WalkMode::Parallel] {
            let mut matcher = FuzzyFileMatcher::new_inner(dir.path(), true);
            matcher.restart_walk_inner(|w| w, mode);
            matcher.set_query("alpha", false);
            drain_until_done(&mut matcher);
            assert!(
                matcher
                    .get_top_k(10)
                    .iter()
                    .any(|h| h.path.to_string().contains("alpha")),
                "walk (mode={mode:?}) should feed the matcher"
            );
        }
    }

    #[test]
    fn choose_walk_mode_degrades_and_probes_with_walk_thread() {
        use std::cell::Cell;

        assert_eq!(choose_walk_mode(true, |_| true), WalkMode::Parallel);
        assert_eq!(
            choose_walk_mode(true, |_| false),
            WalkMode::Serial,
            "a refused probe degrades to serial even with nucleo up"
        );

        let probed = Cell::new(false);
        assert_eq!(
            choose_walk_mode(false, |_| {
                probed.set(true);
                true
            }),
            WalkMode::Serial,
            "disabled keyed matching walks serially"
        );
        assert!(!probed.get(), "a disabled matcher must not probe threads");

        let asked = Cell::new(0);
        let _ = choose_walk_mode(true, |n| {
            asked.set(n);
            true
        });
        assert_eq!(
            asked.get(),
            NUM_IGNORE_THREADS + 1,
            "probe reserves the outer fuzzy-walk thread that builds the pool"
        );
    }
}

// Unix child re-exec under `RLIMIT_NPROC` exercises the real `new()` probe path.
#[cfg(all(test, unix))]
mod thread_exhaustion_tests {
    use super::*;

    const CHILD_ENV: &str = "ECHO_AGENT_FUZZY_THREAD_EXHAUSTION_CHILD";
    const PASS_MARK: &str = "fuzzy-contained:";
    const SKIP_MARK: &str = "skip-child:";

    /// Child: cap `RLIMIT_NPROC` so no new thread spawns, then build and drive
    /// the real matcher. Every path must degrade, never abort.
    fn run_child() -> ! {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("alpha.txt"), b"x").expect("write fixture");

        let mut lim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: getrlimit writes only into local `lim`.
        if unsafe { libc::getrlimit(libc::RLIMIT_NPROC, &mut lim) } != 0 {
            println!("{SKIP_MARK} getrlimit failed");
            std::process::exit(0);
        }
        lim.rlim_cur = 1.min(lim.rlim_max);
        // SAFETY: lowers only this process's soft limit; existing threads live on.
        if unsafe { libc::setrlimit(libc::RLIMIT_NPROC, &lim) } != 0 {
            println!("{SKIP_MARK} setrlimit failed");
            std::process::exit(0);
        }

        let mut matcher = FuzzyFileMatcher::new(dir.path());
        if matcher.is_enabled() {
            println!("{SKIP_MARK} threadpool built despite the cap");
            std::process::exit(0);
        }
        // Drive the disabled matcher end to end: restart_walk collects the
        // top-level entries and returns before spawning a walk thread (nucleo is
        // None), and every query call returns empty. A panic here would abort
        // under panic=abort.
        matcher.restart_walk();
        matcher.set_query("alpha", false);
        let _ = matcher.tick(10);
        let _ = matcher.num_items();
        let _ = matcher.get_top_k(10);

        // The daemon degrades too: its worker thread fails to spawn under the
        // cap, so set_query/get must return empty without panicking. Fail (not
        // skip) if it somehow returns matches.
        let daemon = FuzzyFileMatcherDaemon::new(FuzzyFileMatcher::new(dir.path()), 10);
        daemon.set_query("alpha", false);
        if !daemon.get().topk.is_empty() {
            eprintln!("disabled daemon returned matches despite the cap");
            std::process::exit(1);
        }

        println!("{PASS_MARK} degraded to disabled and survived");
        std::process::exit(0);
    }

    /// Doubles as the child entry point when `CHILD_ENV` is set.
    #[test]
    fn child_entry_matcher_under_thread_exhaustion() {
        if std::env::var_os(CHILD_ENV).is_some() {
            run_child();
        }
    }

    #[test]
    fn matcher_construction_under_thread_exhaustion_is_contained() {
        // module_path!() includes the crate name; libtest filters do not.
        let filter = module_path!()
            .split_once("::")
            .map(|(_, rest)| rest)
            .unwrap_or_default();
        let exe = std::env::current_exe().expect("current_exe");
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("--exact")
            .arg(format!(
                "{filter}::child_entry_matcher_under_thread_exhaustion"
            ))
            .arg("--nocapture")
            .arg("--test-threads=1")
            .env(CHILD_ENV, "1")
            .stdin(std::process::Stdio::null());
        echo_agent_tty_utils::detach_std_command(&mut cmd);
        let out = cmd.output().expect("spawn child test process");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);

        assert!(
            out.status.success() && !stderr.contains("panicked at"),
            "child aborted/panicked instead of degrading (status: {:?})\nstdout:\n{stdout}\nstderr:\n{stderr}",
            out.status
        );
        if stdout.contains(SKIP_MARK) {
            eprintln!("skipped: {stdout}");
            return;
        }
        assert!(
            stdout.contains(PASS_MARK),
            "no pass/skip marker (filter matched nothing?)\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
