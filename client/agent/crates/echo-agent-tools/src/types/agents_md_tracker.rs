//! Tracks undiscovered AGENTS.md files during a session.
//!
//! When the agent accesses files outside the initial CWD→root discovery
//! chain, this tracker walks up from the target path to the git root,
//! checking each directory for AGENTS.md files. Newly discovered files
//! are reported once per session (or once per compaction cycle) as
//! path-only reminders — the agent decides whether to read them.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ignore::gitignore::Gitignore;

use crate::types::compat::CompatConfig;

/// Filenames (and relative paths) recognized as project instruction files.
///
/// The runtime list is produced by `CompatConfig::agent_filenames()`; this
/// constant is retained only as the all-on reference that the pinning test
/// `compat_default_matches_legacy_constants` asserts parity against.
#[cfg(test)]
pub(crate) const AGENT_FILENAMES: &[&str] = &[
    "Agents.md",
    "Claude.md",
    "CLAUDE.md",
    "CLAUDE.local.md",
    "AGENT.md",
    "AGENTS.md",
    ".claude/CLAUDE.md",
    ".claude/CLAUDE.local.md",
];

/// Subdirectories to scan for `*.md` rules files.
///
/// The runtime list is produced by `CompatConfig::rules_dirs()`; this constant
/// is retained only as the all-on reference for the pinning test.
#[cfg(test)]
pub(crate) const RULES_DIRS: &[&str] = &[".echo-agent/rules", ".claude/rules", ".cursor/rules"];

/// Maximum number of parent directories to walk upward per call.
///
/// In very deep repos, this prevents doing many stat calls on a
/// single tool invocation. Directories beyond this depth are
/// silently skipped — they'll be checked on future accesses
/// closer to them. 10 levels covers deep nested project paths like
/// `packages/app/src/lib/types/file.rs` (6 levels from `packages/`).
const MAX_WALK_DEPTH: usize = 10;

/// Canonicalize a path for consistent HashSet lookups.
///
/// Different tools produce paths in different forms:
///   - `read_file` returns **canonicalized** paths (dunce-simplified, via `util::fs`)
///   - `search_replace` returns `cwd.join(input)` — NOT canonicalized
///   - `list_dir` returns `cwd.join(input)` — NOT canonicalized
///   - `agents_md.rs` discovery returns `dir.join(name).display()` — NOT canonicalized
///   - `git2::Repository::workdir()` returns the **canonical** repo root
///
/// On systems with symlinks (e.g., macOS `/tmp` → `/private/tmp`, Docker volume
/// mounts), these representations diverge for the same physical directory.
/// Without normalization, a seeded `initial_discovery` entry of `/tmp/repo/AGENTS.md`
/// wouldn't match a `check_path()` input of `/private/tmp/repo/file.rs` (whose
/// parent walk yields `/private/tmp/repo/`), causing spurious duplicate reminders.
///
/// Uses [`crate::util::fs::canonicalize_with_timeout`] (dunce-simplified, runs
/// on the blocking thread pool) so a slow/overlayfs-backed filesystem cannot
/// hang the async executor. If canonicalization fails or times out, the input
/// is returned as-is — this is the same fallback used by `read_file`.
async fn normalize(path: &Path) -> PathBuf {
    crate::util::fs::canonicalize_with_timeout(path.to_path_buf()).await
}

/// Tracks which AGENTS.md files have been discovered and reported to the agent
/// during the session.
///
/// This is a plain struct on ToolState — no Arc, no Mutex.
///
/// **Path normalization**: All paths stored in `checked_dirs`, `initial_discovery`,
/// `reminded`, and `git_root` are canonicalized via [`normalize()`] on insertion.
/// All paths passed to `check_path()` are canonicalized before lookup. This
/// ensures consistent matching regardless of whether the caller provides a
/// symlink-resolved path (like `read_file` does) or a raw `cwd.join()` path
/// (like `search_replace` and `list_dir` do).
#[derive(Debug, Default)]
pub struct AgentsMdTracker {
    /// Directories we've already scanned for AGENTS.md.
    /// Prevents redundant stat() calls on repeated accesses to the same subtree.
    /// All entries are canonicalized via `normalize()`.
    checked_dirs: HashSet<PathBuf>,

    /// AGENTS.md file paths that were part of the initial system prompt injection
    /// (seeded by AgentBuilder at session start from agents_md.rs discovery).
    /// We never remind about these — the agent already has them.
    /// All entries are canonicalized via `normalize()`.
    initial_discovery: HashSet<PathBuf>,

    /// AGENTS.md file paths we've already reminded the agent about.
    /// Each file gets at most one reminder per session (or compaction cycle).
    /// All entries are canonicalized via `normalize()`.
    reminded: HashSet<PathBuf>,

    /// Upper bound for walking. We never walk above the git root.
    /// If None, walking is disabled (no git repo found).
    /// Canonicalized via `normalize()` on insertion.
    git_root: Option<PathBuf>,

    /// Gitignore rules for the repo. Discovered AGENTS.md files that match
    /// .gitignore are silently skipped, matching the behavior of the initial
    /// discovery in agents_md.rs.
    gitignore: Option<Gitignore>,

    /// Resolved vendor-compat config governing which rules dirs and agent
    /// filenames are scanned. Defaults to all-on (historical behavior).
    /// Set by the bridge at seed time.
    compat: CompatConfig,
}

impl AgentsMdTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the resolved vendor-compat config used by runtime AGENTS.md / rules
    /// discovery. Must be called at session start (alongside `seed`) so
    /// `check_path` gates vendor surfaces correctly.
    pub fn set_compat(&mut self, compat: CompatConfig) {
        self.compat = compat;
    }

    /// Seed the tracker with paths from the initial AGENTS.md discovery.
    /// Called once at session start by AgentBuilder/ToolBridge.
    ///
    /// `initial_paths` are the full paths to AGENTS.md files already injected
    /// into the system prompt. Their parent directories are marked as checked.
    ///
    /// `initial_chain` is the CWD→root directory chain that was already scanned
    /// during initial discovery (even dirs with no AGENTS.md). These are marked
    /// as checked to prevent redundant stat calls.
    ///
    /// `gitignore` is the parsed .gitignore rules for the repo, built by the
    /// same `build_gitignore()` used by the initial discovery in agents_md.rs.
    ///
    /// All paths are canonicalized via `normalize()` before insertion.
    pub async fn seed(
        &mut self,
        initial_paths: Vec<PathBuf>,
        git_root: Option<PathBuf>,
        initial_chain: Vec<PathBuf>,
        gitignore: Option<Gitignore>,
    ) {
        for path in &initial_paths {
            let canonical = normalize(path).await;
            self.initial_discovery.insert(canonical.clone());
            if let Some(parent) = canonical.parent() {
                self.checked_dirs.insert(parent.to_path_buf());
            }
        }
        for dir in initial_chain {
            self.checked_dirs.insert(normalize(&dir).await);
        }
        self.git_root = match git_root {
            Some(r) => Some(normalize(&r).await),
            None => None,
        };
        self.gitignore = gitignore;
    }

    /// Given a file or directory path the agent is accessing, walk up to the
    /// git root and find any AGENTS.md files that:
    ///   1. Exist on disk
    ///   2. Are NOT gitignored
    ///   3. Were NOT in the initial discovery set
    ///   4. Haven't been reminded about yet
    ///
    /// Returns a list of discovered AGENTS.md paths (canonicalized). Marks them
    /// as "reminded" so they won't trigger again.
    /// Async version that offloads all filesystem I/O to the tokio blocking
    /// thread pool with timeouts, preventing hung `stat()`/`canonicalize()`
    /// calls on slow or overlayfs-backed filesystems from blocking the
    /// single-threaded async executor (and holding the `registry` lock).
    ///
    /// Bails out immediately on the first filesystem timeout — if the mount
    /// can't respond to a single `stat()` within [`FS_SYSCALL_TIMEOUT`],
    /// continuing the walk would just accumulate ~50 sequential timeouts
    /// (up to 25 minutes total), making the session appear stuck.
    pub async fn check_path(&mut self, target_path: &Path) -> Vec<PathBuf> {
        use crate::util::fs::FS_SYSCALL_TIMEOUT;

        let git_root = match &self.git_root {
            Some(root) => root.clone(),
            None => return vec![], // No git repo → no discovery
        };

        // Determine starting directory:
        // - If target is a directory, start from it
        // - If target is a file, start from its parent
        //
        // We check the raw path first (before normalization) because
        // normalize() on a non-existent file returns it as-is, making
        // is_dir() return false for existing directories passed with
        // a non-existent child. By checking the raw path, then normalizing
        // the starting directory, we get correct canonical paths.
        let is_dir = match tokio::time::timeout(
            FS_SYSCALL_TIMEOUT,
            tokio::fs::metadata(target_path),
        )
        .await
        {
            Ok(Ok(m)) => m.is_dir(),
            Ok(Err(_)) => false, // stat error (e.g. not found) — treat as file
            Err(_elapsed) => {
                tracing::warn!(
                    path = %target_path.display(),
                    "check_path: initial metadata timed out, aborting walk \
                     (slow/overlayfs filesystem?)"
                );
                return vec![];
            }
        };

        let raw_start_dir = if is_dir {
            target_path.to_path_buf()
        } else {
            match target_path.parent() {
                Some(p) => p.to_path_buf(),
                None => return vec![],
            }
        };

        // Canonicalize the starting directory. This resolves symlinks and
        // ensures consistent matching against checked_dirs/initial_discovery.
        // The starting directory should exist on disk (it's the parent of
        // a file the agent just accessed successfully), so canonicalize
        // should succeed.
        let start_dir = normalize(&raw_start_dir).await;

        // Verify the start dir is within the git root
        if !start_dir.starts_with(&git_root) {
            return vec![];
        }

        let mut discoveries = Vec::new();

        // Compute the gated filename / rules-dir lists once per call (they are
        // constant across the walk). The vendor-gated entries drop when the
        // matching compat cell is off; all-on reproduces `AGENT_FILENAMES` /
        // `RULES_DIRS` exactly.
        let agent_filenames = self.compat.agent_filenames();
        let rules_dirs = self.compat.rules_dirs();

        // Walk up from start_dir to git_root, bounded by MAX_WALK_DEPTH
        let mut current = Some(start_dir.as_path());
        let mut depth = 0;
        while let Some(dir) = current {
            // Stop if we've gone above git root.
            if !dir.starts_with(&git_root) {
                break;
            }

            // Stop if we've walked too deep
            if depth >= MAX_WALK_DEPTH {
                break;
            }
            depth += 1;

            // Skip if already checked.
            let dir_buf = dir.to_path_buf();
            if !self.checked_dirs.contains(&dir_buf) {
                self.checked_dirs.insert(dir_buf.clone());

                // Check for AGENTS.md files in this directory.
                // Each exists() check goes through the tokio blocking pool
                // with a timeout. On the first timeout we abort the entire
                // walk — a single hung stat means the filesystem is
                // unresponsive and continuing would just pile up timeouts.
                // `agent_filenames` is computed once above the walk.
                for filename in &agent_filenames {
                    let agents_path = dir.join(filename);
                    let stat_result =
                        tokio::time::timeout(FS_SYSCALL_TIMEOUT, tokio::fs::metadata(&agents_path))
                            .await;

                    if stat_result.is_err() {
                        // Timeout — filesystem unresponsive, bail out
                        tracing::warn!(
                            path = %agents_path.display(),
                            "check_path: exists() timed out, aborting walk"
                        );
                        return discoveries;
                    }

                    let file_exists = stat_result.ok().and_then(|r| r.ok()).is_some();

                    if file_exists {
                        // Canonicalize the discovered path for consistent lookups
                        let canonical_agents = normalize(&agents_path).await;
                        if !self.is_ignored(&canonical_agents)
                            && !self.initial_discovery.contains(&canonical_agents)
                            && !self.reminded.contains(&canonical_agents)
                        {
                            discoveries.push(canonical_agents.clone());
                            self.reminded.insert(canonical_agents);
                        }
                    }
                }

                // Check for rules files in .echo-agent/rules/, .claude/rules/, and
                // .cursor/rules/ subdirectories (vendor-compat paths).
                // `rules_dirs` is computed once above the walk.
                for rules_subdir in &rules_dirs {
                    let rules_dir = dir.join(rules_subdir);
                    let stat_result =
                        tokio::time::timeout(FS_SYSCALL_TIMEOUT, tokio::fs::metadata(&rules_dir))
                            .await;

                    if stat_result.is_err() {
                        tracing::warn!(
                            path = %rules_dir.display(),
                            "check_path: rules dir stat timed out, aborting walk"
                        );
                        return discoveries;
                    }

                    let is_dir = stat_result
                        .ok()
                        .and_then(|r| r.ok())
                        .is_some_and(|m| m.is_dir());
                    if !is_dir {
                        continue;
                    }

                    // Read directory entries (blocking, with timeout).
                    let read_result = tokio::time::timeout(FS_SYSCALL_TIMEOUT, async {
                        tokio::fs::read_dir(&rules_dir).await
                    })
                    .await;

                    let mut read_dir = match read_result {
                        Ok(Ok(rd)) => rd,
                        Ok(Err(_)) => continue,
                        Err(_) => {
                            tracing::warn!(
                                path = %rules_dir.display(),
                                "check_path: read_dir timed out, aborting walk"
                            );
                            return discoveries;
                        }
                    };

                    let mut rule_paths = Vec::new();
                    while let Ok(Some(entry)) = read_dir.next_entry().await {
                        let path = entry.path();
                        let is_md = path
                            .extension()
                            .and_then(|ext| ext.to_str())
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("md"));
                        if is_md {
                            rule_paths.push(path);
                        }
                    }
                    // Sort alphabetically for deterministic ordering.
                    rule_paths.sort_by(|a, b| a.file_name().cmp(&b.file_name()));

                    for rule_path in rule_paths {
                        let canonical = normalize(&rule_path).await;
                        if !self.is_ignored(&canonical)
                            && !self.initial_discovery.contains(&canonical)
                            && !self.reminded.contains(&canonical)
                        {
                            discoveries.push(canonical.clone());
                            self.reminded.insert(canonical);
                        }
                    }
                }
            }

            // Stop at git root
            if dir == git_root.as_path() {
                break;
            }
            current = dir.parent();
        }

        discoveries
    }

    /// Reset discovery state so that reminders re-fire after compaction.
    /// Called by the compaction flow to ensure the agent is re-notified about
    /// AGENTS.md files it may have lost context on.
    ///
    /// Clears both `reminded` (so reminders aren't deduplicated) AND
    /// `checked_dirs` (so directories are re-scanned via stat). Without
    /// clearing `checked_dirs`, `check_path()` would skip directories it
    /// already visited and never re-discover the AGENTS.md files in them —
    /// making the `reminded.clear()` useless.
    ///
    /// The cost is a few extra stat calls on the first tool access after
    /// compaction — negligible compared to the tool execution itself.
    ///
    /// Does NOT clear `initial_discovery` — the system prompt survives
    /// compaction and still contains those files.
    pub fn on_compaction(&mut self) {
        self.reminded.clear();
        self.checked_dirs.clear();
    }

    /// Get the set of AGENTS.md paths that were discovered at runtime
    /// and reminded about. Used by compaction to surface them in the
    /// compaction context.
    pub fn reminded_paths(&self) -> &HashSet<PathBuf> {
        &self.reminded
    }

    /// Check if a path is gitignored.
    fn is_ignored(&self, path: &Path) -> bool {
        let Some(ref gi) = self.gitignore else {
            return false;
        };
        crate::gitignore::is_ignored(gi, path, self.git_root.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ignore::gitignore::GitignoreBuilder;
    use std::fs;

    /// Create a Gitignore from patterns for testing.
    fn build_test_gitignore(root: &Path, patterns: &[&str]) -> Gitignore {
        let mut builder = GitignoreBuilder::new(root);
        for pattern in patterns {
            builder.add_line(None, pattern).unwrap();
        }
        builder.build().unwrap()
    }

    #[tokio::test]
    async fn tracker_seed_marks_initial_as_known() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("AGENTS.md"), "initial").unwrap();

        let mut tracker = AgentsMdTracker::new();
        tracker
            .seed(
                vec![root.join("AGENTS.md")],
                Some(root.to_path_buf()),
                vec![],
                None,
            )
            .await;

        let results = tracker.check_path(&root.join("foo.rs")).await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn check_path_finds_new_agents_md() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let sub = root.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("AGENTS.md"), "sub instructions").unwrap();

        let mut tracker = AgentsMdTracker::new();
        tracker
            .seed(vec![], Some(root.to_path_buf()), vec![], None)
            .await;

        let results = tracker.check_path(&sub.join("foo.rs")).await;
        assert_eq!(results.len(), 1);
        assert!(results[0].ends_with("AGENTS.md"));
    }

    #[tokio::test]
    async fn check_path_finds_all_filename_variants() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let sub = root.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("Claude.md"), "claude instructions").unwrap();

        let mut tracker = AgentsMdTracker::new();
        tracker
            .seed(vec![], Some(root.to_path_buf()), vec![], None)
            .await;

        let results = tracker.check_path(&sub.join("foo.rs")).await;
        assert_eq!(results.len(), 1);
        assert!(results[0].ends_with("Claude.md"));
    }

    #[tokio::test]
    async fn check_path_skips_already_reminded() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let sub = root.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("AGENTS.md"), "instructions").unwrap();

        let mut tracker = AgentsMdTracker::new();
        tracker
            .seed(vec![], Some(root.to_path_buf()), vec![], None)
            .await;

        let first = tracker.check_path(&sub.join("foo.rs")).await;
        assert_eq!(first.len(), 1);

        let second = tracker.check_path(&sub.join("bar.rs")).await;
        assert!(second.is_empty());
    }

    #[tokio::test]
    async fn check_path_skips_initial_discovery() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("AGENTS.md"), "root instructions").unwrap();

        let mut tracker = AgentsMdTracker::new();
        tracker
            .seed(
                vec![root.join("AGENTS.md")],
                Some(root.to_path_buf()),
                vec![root.to_path_buf()],
                None,
            )
            .await;

        let results = tracker.check_path(&root.join("foo.rs")).await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn check_path_stops_at_git_root() {
        let tmp = tempfile::tempdir().unwrap();
        let outer = tmp.path();
        let repo = outer.join("repo");
        let sub = repo.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(outer.join("AGENTS.md"), "above root").unwrap();

        let mut tracker = AgentsMdTracker::new();
        tracker.seed(vec![], Some(repo.clone()), vec![], None).await;

        let results = tracker.check_path(&sub.join("foo.rs")).await;
        assert!(
            results.is_empty(),
            "Should not find AGENTS.md above git root"
        );
    }

    #[tokio::test]
    async fn check_path_returns_empty_when_no_git_root() {
        let mut tracker = AgentsMdTracker::new();
        tracker.seed(vec![], None, vec![], None).await;

        let results = tracker.check_path(Path::new("/any/path/file.rs")).await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn check_path_marks_dirs_as_checked() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let deep = root.join("a").join("b").join("c");
        fs::create_dir_all(&deep).unwrap();

        let mut tracker = AgentsMdTracker::new();
        tracker
            .seed(vec![], Some(root.to_path_buf()), vec![], None)
            .await;

        let results = tracker.check_path(&deep.join("file.rs")).await;
        assert!(results.is_empty());

        fs::write(root.join("a").join("b").join("AGENTS.md"), "late").unwrap();

        let results = tracker.check_path(&deep.join("other.rs")).await;
        assert!(
            results.is_empty(),
            "Dirs already checked should not be re-scanned"
        );
    }

    #[tokio::test]
    async fn check_path_idempotent_on_second_call() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let sub = root.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("AGENTS.md"), "content").unwrap();

        let mut tracker = AgentsMdTracker::new();
        tracker
            .seed(vec![], Some(root.to_path_buf()), vec![], None)
            .await;

        let first = tracker.check_path(&sub.join("file.rs")).await;
        assert_eq!(first.len(), 1);
        let reminded_count_after_first = tracker.reminded.len();

        let second = tracker.check_path(&sub.join("file2.rs")).await;
        assert!(second.is_empty());
        assert_eq!(tracker.reminded.len(), reminded_count_after_first);
    }

    #[tokio::test]
    async fn check_path_stops_at_max_walk_depth() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        fs::write(root.join("AGENTS.md"), "root").unwrap();

        let mut deep = root.to_path_buf();
        for i in 0..12 {
            deep = deep.join(format!("d{}", i));
        }
        fs::create_dir_all(&deep).unwrap();

        let mut tracker = AgentsMdTracker::new();
        tracker
            .seed(vec![], Some(root.to_path_buf()), vec![], None)
            .await;

        let results = tracker.check_path(&deep.join("file.rs")).await;
        assert!(results.len() <= 1);
    }

    #[tokio::test]
    async fn check_path_skips_gitignored_agents_md() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let root = dunce::canonicalize(root).unwrap();
        let build_dir = root.join("build");
        fs::create_dir_all(&build_dir).unwrap();
        fs::write(build_dir.join("AGENTS.md"), "build instructions").unwrap();

        let gi = build_test_gitignore(&root, &["build/"]);
        let mut tracker = AgentsMdTracker::new();
        tracker
            .seed(vec![], Some(root.to_path_buf()), vec![], Some(gi))
            .await;

        let results = tracker.check_path(&build_dir.join("output.o")).await;
        assert!(results.is_empty(), "Gitignored AGENTS.md should be skipped");
    }

    #[tokio::test]
    async fn check_path_does_not_skip_non_gitignored() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let root = dunce::canonicalize(root).unwrap();
        let src_dir = root.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("AGENTS.md"), "src instructions").unwrap();

        let gi = build_test_gitignore(&root, &["build/"]);
        let mut tracker = AgentsMdTracker::new();
        tracker
            .seed(vec![], Some(root.to_path_buf()), vec![], Some(gi))
            .await;

        let results = tracker.check_path(&src_dir.join("main.rs")).await;
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn on_compaction_clears_reminded_and_checked_dirs_for_refire() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let sub = root.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("AGENTS.md"), "content").unwrap();

        let mut tracker = AgentsMdTracker::new();
        tracker
            .seed(vec![], Some(root.to_path_buf()), vec![], None)
            .await;

        let first = tracker.check_path(&sub.join("file.rs")).await;
        assert_eq!(first.len(), 1);
        assert_eq!(tracker.reminded.len(), 1);

        let second = tracker.check_path(&sub.join("file2.rs")).await;
        assert!(second.is_empty());

        tracker.on_compaction();
        assert!(tracker.reminded.is_empty());
        assert!(tracker.checked_dirs.is_empty());

        let refire = tracker.check_path(&sub.join("file3.rs")).await;
        assert_eq!(
            refire.len(),
            1,
            "AGENTS.md reminder must re-fire after compaction"
        );
        assert_eq!(tracker.reminded.len(), 1);
    }

    #[tokio::test]
    async fn on_compaction_does_not_clear_initial_discovery() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("AGENTS.md"), "root").unwrap();

        let mut tracker = AgentsMdTracker::new();
        tracker
            .seed(
                vec![root.join("AGENTS.md")],
                Some(root.to_path_buf()),
                vec![root.to_path_buf()],
                None,
            )
            .await;

        tracker.on_compaction();

        let results = tracker.check_path(&root.join("foo.rs")).await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn reminded_paths_returns_current_set() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let sub1 = root.join("sub1");
        let sub2 = root.join("sub2");
        fs::create_dir_all(&sub1).unwrap();
        fs::create_dir_all(&sub2).unwrap();
        fs::write(sub1.join("AGENTS.md"), "sub1").unwrap();
        fs::write(sub2.join("AGENTS.md"), "sub2").unwrap();

        let mut tracker = AgentsMdTracker::new();
        tracker
            .seed(vec![], Some(root.to_path_buf()), vec![], None)
            .await;

        tracker.check_path(&sub1.join("file.rs")).await;
        tracker.check_path(&sub2.join("file.rs")).await;
        assert_eq!(tracker.reminded_paths().len(), 2);

        tracker.on_compaction();
        assert!(tracker.reminded_paths().is_empty());
    }

    #[tokio::test]
    async fn check_path_handles_dot_dot_segments() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let a = root.join("a");
        let b = a.join("b");
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("AGENTS.md"), "a instructions").unwrap();

        let mut tracker = AgentsMdTracker::new();
        tracker
            .seed(vec![], Some(root.to_path_buf()), vec![], None)
            .await;

        let dotdot_path = b.join("..").join("b").join("file.rs");
        let results = tracker.check_path(&dotdot_path).await;
        assert_eq!(results.len(), 1);
        assert!(!results[0].to_str().unwrap().contains(".."));
    }

    #[tokio::test]
    async fn normalize_returns_input_for_nonexistent_path() {
        let path = Path::new("/nonexistent/path/file.rs");
        let result = normalize(path).await;
        assert_eq!(result, path);
    }

    #[tokio::test]
    async fn check_path_with_directory_target() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let sub = root.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("AGENTS.md"), "content").unwrap();

        let mut tracker = AgentsMdTracker::new();
        tracker
            .seed(vec![], Some(root.to_path_buf()), vec![], None)
            .await;

        let results = tracker.check_path(&sub).await;
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn check_path_discovers_parent_agents_md() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let frontend = root.join("frontend");
        let apps = frontend.join("apps");
        fs::create_dir_all(&apps).unwrap();
        fs::write(frontend.join("AGENTS.md"), "frontend rules").unwrap();

        let mut tracker = AgentsMdTracker::new();
        tracker
            .seed(vec![], Some(root.to_path_buf()), vec![], None)
            .await;

        let results = tracker.check_path(&apps.join("foo.ts")).await;
        assert_eq!(results.len(), 1);
        assert!(results[0].to_str().unwrap().contains("frontend"));
    }

    // ── Rules directory discovery tests ─────────────────────────────

    #[tokio::test]
    async fn check_path_discovers_claude_rules_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let sub = root.join("sub");
        fs::create_dir_all(&sub).unwrap();

        let rules_dir = sub.join(".claude").join("rules");
        fs::create_dir_all(&rules_dir).unwrap();
        fs::write(rules_dir.join("style.md"), "# Style rules").unwrap();

        let mut tracker = AgentsMdTracker::new();
        tracker
            .seed(vec![], Some(root.to_path_buf()), vec![], None)
            .await;

        let results = tracker.check_path(&sub.join("foo.rs")).await;
        assert!(
            results
                .iter()
                .any(|p| p.to_str().unwrap().contains("style.md")),
            "Should discover .claude/rules/style.md, got: {:?}",
            results
        );
    }

    #[tokio::test]
    async fn check_path_rules_not_reminded_twice() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let sub = root.join("sub");
        fs::create_dir_all(&sub).unwrap();

        let rules_dir = sub.join(".claude").join("rules");
        fs::create_dir_all(&rules_dir).unwrap();
        fs::write(rules_dir.join("style.md"), "# Style").unwrap();

        let mut tracker = AgentsMdTracker::new();
        tracker
            .seed(vec![], Some(root.to_path_buf()), vec![], None)
            .await;

        let first = tracker.check_path(&sub.join("foo.rs")).await;
        assert_eq!(first.len(), 1);

        let second = tracker.check_path(&sub.join("bar.rs")).await;
        assert!(second.is_empty(), "Rules should not be reminded twice");
    }

    // ── AGENT_SUBDIRS discovery tests ───────────────────────────────

    #[tokio::test]
    async fn check_path_discovers_claude_subdir_claude_md() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let sub = root.join("sub");
        fs::create_dir_all(&sub).unwrap();

        let claude_dir = sub.join(".claude");
        fs::create_dir_all(&claude_dir).unwrap();
        fs::write(claude_dir.join("CLAUDE.md"), "# Project instructions").unwrap();

        let mut tracker = AgentsMdTracker::new();
        tracker
            .seed(vec![], Some(root.to_path_buf()), vec![], None)
            .await;

        let results = tracker.check_path(&sub.join("foo.rs")).await;
        assert!(
            results
                .iter()
                .any(|p| p.to_str().unwrap().contains("CLAUDE.md")),
            "Should discover .claude/CLAUDE.md, got: {:?}",
            results
        );
    }

    // ── compat gating + byte-for-byte parity ───────────────

    /// Pin that the all-on compat helpers reproduce the legacy constants
    /// exactly (same entries, same order). If either drifts, the
    /// byte-for-byte default-behavior invariant is broken.
    #[test]
    fn compat_default_matches_legacy_constants() {
        use crate::types::compat::CompatConfig;
        let c = CompatConfig::default();
        assert_eq!(c.agent_filenames(), AGENT_FILENAMES.to_vec());
        assert_eq!(c.rules_dirs(), RULES_DIRS.to_vec());
    }

    #[tokio::test]
    async fn check_path_gates_cursor_rules_when_off() {
        use crate::types::compat::CompatConfig;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let sub = root.join("sub");
        fs::create_dir_all(&sub).unwrap();

        let rules_dir = sub.join(".cursor").join("rules");
        fs::create_dir_all(&rules_dir).unwrap();
        fs::write(rules_dir.join("r.md"), "# Cursor rule").unwrap();

        let mut compat = CompatConfig::default();
        compat.cursor.rules = false;

        let mut tracker = AgentsMdTracker::new();
        tracker.set_compat(compat);
        tracker
            .seed(vec![], Some(root.to_path_buf()), vec![], None)
            .await;

        let results = tracker.check_path(&sub.join("foo.rs")).await;
        assert!(
            !results.iter().any(|p| p.to_str().unwrap().contains("r.md")),
            "cursor rules must be gated off: {results:?}"
        );
    }

    #[tokio::test]
    async fn check_path_gates_claude_agents_when_off() {
        use crate::types::compat::CompatConfig;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let sub = root.join("sub");
        fs::create_dir_all(&sub).unwrap();

        let claude_dir = sub.join(".claude");
        fs::create_dir_all(&claude_dir).unwrap();
        fs::write(claude_dir.join("CLAUDE.md"), "# Project instructions").unwrap();

        let mut compat = CompatConfig::default();
        compat.claude.agents = false;

        let mut tracker = AgentsMdTracker::new();
        tracker.set_compat(compat);
        tracker
            .seed(vec![], Some(root.to_path_buf()), vec![], None)
            .await;

        let results = tracker.check_path(&sub.join("foo.rs")).await;
        assert!(
            !results
                .iter()
                .any(|p| p.to_str().unwrap().contains(".claude/CLAUDE.md")
                    || p.to_str().unwrap().contains(".claude\\CLAUDE.md")),
            "claude .claude/CLAUDE.md must be gated off: {results:?}"
        );
    }
}
