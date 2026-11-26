//! Git branch/worktree info — cached queries shared across views.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::host::HostOs;
use crate::terminal::{TerminalName, terminal_context};

/// Per-cwd git cache — the single source of truth for every git display
/// in the pager: the welcome top bar / dashboard header (process cwd),
/// each agent's status bar, and the dashboard row subtitles. Keyed per
/// directory so one directory's branch never leaks onto another's. Maps
/// each cwd to its last-computed [`CwdGitInfo`] (`None` for a non-repo)
/// plus the time of the last refresh attempt (for throttling).
///
/// Fed from three places, all off the render path:
///   - [`cwd_git_info_lazy`] — a lazy, throttled refresh when a view reads a cwd.
///   - [`populate_from_cwd_async`] — an eager warm at startup / on a cwd change.
///   - [`update_from_notification`] — the `echo.agent/git_head_changed` ACP
///     notification, so a branch switch inside an agent reflects immediately
///     instead of waiting out [`CWD_GIT_REFRESH_TTL`].
type CwdCacheEntry = (Option<CwdGitInfo>, Instant);
static CWD_GIT_CACHE: LazyLock<Mutex<HashMap<PathBuf, CwdCacheEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Minimum interval between off-thread refreshes for the same cwd, so a
/// per-frame caller can't spawn a storm of git lookups.
const CWD_GIT_REFRESH_TTL: Duration = Duration::from_secs(5);

/// Upper bound on [`CWD_GIT_CACHE`] entries. The pager only displays a
/// handful of directories at once (the process cwd + one per live agent),
/// but a long session that navigates many locations would otherwise grow
/// the map without bound. When full, the least-recently-refreshed entry is
/// evicted on insert (see [`cwd_cache_insert`]).
const CWD_GIT_CACHE_CAP: usize = 64;

/// Refresh [`CWD_GIT_CACHE`] for `dir` from a `git_head_changed`
/// notification, so a branch switch inside an agent's session reflects in
/// every view immediately instead of waiting out [`CWD_GIT_REFRESH_TTL`].
///
/// Called from [`crate::app::acp_handler::handle_git_head_changed`], which
/// also updates the agent's own `current_branch` / `is_worktree` /
/// `main_repo` fields directly. The worktree label isn't carried by the
/// notification (and is immutable for a path), so any previously-resolved
/// label is preserved. `is_worktree` matches [`compute_cwd_git_info`]:
/// notification flag, `main_repo`, or a cached non-empty label.
pub fn update_from_notification(
    dir: &Path,
    branch: Option<&str>,
    main_repo: Option<String>,
    is_worktree: bool,
) {
    if let Ok(mut cache) = CWD_GIT_CACHE.lock() {
        let worktree_label = cache
            .get(dir)
            .and_then(|(info, _)| info.as_ref())
            .and_then(|i| i.worktree_label.clone());
        let is_worktree =
            is_worktree || is_cwd_worktree(main_repo.as_deref(), worktree_label.as_deref());
        let info = CwdGitInfo {
            is_worktree,
            branch: branch.map(str::to_string),
            main_repo,
            worktree_label,
        };
        cwd_cache_insert(&mut cache, dir.to_path_buf(), (Some(info), Instant::now()));
    }
}

/// Eagerly warm [`CWD_GIT_CACHE`] for `cwd` off-thread — e.g. at pager
/// startup and after a dashboard location change — so the header / top bar
/// show the branch + worktree on the next frame instead of waiting for the
/// first lazy refresh.
///
/// No subprocess (libgit2 is filesystem-based) and a no-op when there is no
/// tokio runtime, so callers stay infallible.
pub fn populate_from_cwd_async(cwd: PathBuf) {
    spawn_cwd_git_refresh(cwd);
}

struct GitSnapshot {
    /// Tilde-shortened path to the repo workdir. `None` when not in a repo.
    repo_root_display: Option<String>,
    /// `None` when not in a repo. `Some("")` for detached HEAD, otherwise the
    /// branch shorthand.
    branch: Option<String>,
    /// Tilde-shortened path to the main repo when in a worktree, else `None`.
    main_repo_display: Option<String>,
    /// Human-readable label from the worktree metadata DB, if this cwd is a
    /// managed worktree with a label set.
    worktree_label: Option<String>,
}

/// Freshly-computed git context for a directory. Returned by
/// [`compute_cwd_git_info`].
#[derive(Debug, Clone)]
pub struct CwdGitInfo {
    /// Branch shorthand. `Some("")` for detached HEAD.
    pub branch: Option<String>,
    /// Whether `cwd` is a worktree rather than the primary checkout
    /// (linked `git worktree`, echoagent standalone clone, or worktree DB hit).
    pub is_worktree: bool,
    /// Tilde-shortened path to the main repo when in a worktree.
    pub main_repo: Option<String>,
    /// Human-readable worktree label from the metadata DB, if any.
    pub worktree_label: Option<String>,
}

/// Synchronously compute fresh git context (branch + worktree info) for
/// `cwd`. Spawns no subprocess (uses `git2`) but DOES touch the
/// filesystem + worktree DB, so call it off the render path (e.g. once
/// when a view opens), never per frame. Returns `None` when `cwd` is not
/// inside a git repository, so callers can leave existing cached values
/// untouched rather than clobbering them with empties.
pub fn compute_cwd_git_info(cwd: &Path) -> Option<CwdGitInfo> {
    let snap = compute_snapshot(cwd);
    // `repo_root_display` is `Some` only when repo discovery succeeded;
    // a `None` here means `cwd` is not a repo (or discovery failed).
    snap.repo_root_display.as_ref()?;
    Some(CwdGitInfo {
        is_worktree: is_cwd_worktree(
            snap.main_repo_display.as_deref(),
            snap.worktree_label.as_deref(),
        ),
        branch: snap.branch,
        main_repo: snap.main_repo_display,
        worktree_label: snap.worktree_label,
    })
}

fn is_cwd_worktree(main_repo: Option<&str>, worktree_label: Option<&str>) -> bool {
    main_repo.is_some() || worktree_label.is_some_and(|s| !s.is_empty())
}

/// Per-cwd git info for render paths that display many directories (the
/// dashboard agent list, each agent's status bar). Returns the cached
/// value for `cwd` (possibly `None` on the very first call) and kicks off
/// a throttled off-thread refresh when the entry is missing or older than
/// [`CWD_GIT_REFRESH_TTL`]. Never blocks and never spawns `git`
/// subprocesses (uses `git2`); safe to call every frame for many cwds.
///
/// Keyed per directory, so each agent shows the branch/worktree of ITS OWN
/// location rather than the process cwd's.
pub fn cwd_git_info_lazy(cwd: &Path) -> Option<CwdGitInfo> {
    let mut cache = CWD_GIT_CACHE.lock().ok()?;
    let (cached, needs_refresh) = match cache.get(cwd) {
        Some((info, ts)) => (info.clone(), ts.elapsed() >= CWD_GIT_REFRESH_TTL),
        None => (None, true),
    };
    if needs_refresh {
        // Reserve the slot with a fresh timestamp BEFORE spawning so this
        // frame's other reads (and the next few frames) don't spawn
        // duplicate refreshes until this one lands or the TTL elapses.
        cwd_cache_insert(
            &mut cache,
            cwd.to_path_buf(),
            (cached.clone(), Instant::now()),
        );
        drop(cache);
        spawn_cwd_git_refresh(cwd.to_path_buf());
    }
    cached
}

/// Off-thread refresh of one cwd's entry in [`CWD_GIT_CACHE`]. No-op when
/// there is no tokio runtime (e.g. unit tests) so callers stay infallible.
fn spawn_cwd_git_refresh(cwd: PathBuf) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    handle.spawn_blocking(move || {
        // Guard against panics from the vendored libgit2 C bindings.
        let info =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| compute_cwd_git_info(&cwd)))
                .unwrap_or(None);
        if let Ok(mut cache) = CWD_GIT_CACHE.lock() {
            apply_cwd_git_refresh(&mut cache, cwd, info);
        }
    });
}

/// Apply an off-thread refresh result to [`CWD_GIT_CACHE`].
///
/// A successful probe (`Some`) replaces the entry. A `None` means either
/// "not a git repo" OR a transient libgit2 discovery failure — the two are
/// indistinguishable here — so we preserve any previously-resolved value
/// rather than clobbering it with an empty (honoring
/// [`compute_cwd_git_info`]'s contract). A fresh `cwd` with no prior entry
/// still records the `None`, which is correct for a genuine non-repo. The
/// timestamp always advances so the throttle resets either way.
fn apply_cwd_git_refresh(
    cache: &mut HashMap<PathBuf, CwdCacheEntry>,
    cwd: PathBuf,
    info: Option<CwdGitInfo>,
) {
    let value = match info {
        some @ Some(_) => some,
        None => cache.get(&cwd).and_then(|(prev, _)| prev.clone()),
    };
    cwd_cache_insert(cache, cwd, (value, Instant::now()));
}

/// Insert into [`CWD_GIT_CACHE`], evicting the least-recently-refreshed
/// entry first when the map is at [`CWD_GIT_CACHE_CAP`] and `key` is new.
/// Keeps the map bounded across long sessions that visit many directories;
/// actively-rendered cwds keep fresh timestamps so they're never the
/// eviction target.
fn cwd_cache_insert(
    cache: &mut HashMap<PathBuf, CwdCacheEntry>,
    key: PathBuf,
    entry: CwdCacheEntry,
) {
    if cache.len() >= CWD_GIT_CACHE_CAP
        && !cache.contains_key(&key)
        && let Some(oldest) = cache
            .iter()
            .min_by_key(|(_, (_, ts))| *ts)
            .map(|(k, _)| k.clone())
    {
        cache.remove(&oldest);
    }
    cache.insert(key, entry);
}

fn compute_snapshot(cwd: &Path) -> GitSnapshot {
    let Ok(repo) = git2::Repository::discover(cwd) else {
        return GitSnapshot {
            repo_root_display: None,
            branch: None,
            main_repo_display: None,
            worktree_label: None,
        };
    };

    let repo_root_display = repo.workdir().map(collapse_home);

    // Empty string sentinel means "detached HEAD" — the pager's notification
    // path uses the same convention.
    let branch = repo.head().ok().map(|head| {
        head.shorthand()
            .ok()
            .filter(|s| *s != "HEAD")
            .map(str::to_string)
            .unwrap_or_default()
    });

    let linked_main_repo = (repo.path() != repo.commondir())
        .then(|| repo.commondir().parent().map(collapse_home))
        .flatten();

    // Standalone echoagent worktrees are CoW clones (`.git` is a directory), so
    // `path != commondir` is false; the source marker is the back-pointer.
    let mut marker_main_repo = None;
    for ancestor in cwd.ancestors() {
        let git = ancestor.join(".git");
        if let Ok(contents) = std::fs::read_to_string(git.join("echo-agent-worktree-source"))
            && let trimmed = contents.trim()
            && !trimmed.is_empty()
        {
            marker_main_repo = Some(collapse_home(Path::new(trimmed)));
            break;
        }
        // Don't inherit a parent checkout's marker across a nested repo root.
        if git.exists() {
            break;
        }
    }

    let (worktree_label, db_source_repo) = lookup_worktree_record(cwd);
    let db_main_repo = db_source_repo.as_deref().map(collapse_home);

    GitSnapshot {
        repo_root_display,
        branch,
        main_repo_display: marker_main_repo.or(db_main_repo).or(linked_main_repo),
        worktree_label,
    }
}

/// Map of worktree root path → human label for every managed worktree
/// that has a non-empty label. Opens the worktree metadata DB once and
/// returns an empty map on any error. Keys are canonicalized so callers
/// can match against `dunce::canonicalize`d candidate paths.
///
/// Intended to be built once (e.g. when a directory picker opens) and
/// reused for many path lookups, avoiding a DB open per candidate.
pub fn worktree_label_index() -> std::collections::HashMap<PathBuf, String> {
    let mut map = std::collections::HashMap::new();
    let Ok(db) = echo_agent_fast_worktree::db::WorktreeDb::open_default() else {
        return map;
    };
    let Ok(records) = db.list(&Default::default()) else {
        return map;
    };
    for rec in records {
        let Some(label) = rec.label() else {
            continue;
        };
        let label = label.to_owned();
        let key = dunce::canonicalize(&rec.path).unwrap_or(rec.path);
        map.insert(key, label);
    }
    map
}

/// Look up the worktree label and source repo from the metadata DB.
///
/// Returns `(None, None)` silently on any error (missing DB, no record).
/// Called from `spawn_blocking` so DB I/O is fine.
fn lookup_worktree_record(cwd: &Path) -> (Option<String>, Option<PathBuf>) {
    let Ok(db) = echo_agent_fast_worktree::db::WorktreeDb::open_default() else {
        return (None, None);
    };
    // Try exact match first, then walk up ancestors to find the worktree root.
    for ancestor in cwd.ancestors() {
        if let Ok(Some(record)) = db.get(&ancestor.to_string_lossy()) {
            let label = record.label().map(str::to_owned);
            let source = (!record.source_repo.as_os_str().is_empty()).then_some(record.source_repo);
            return (label, source);
        }
        // Nested checkout inside a registered worktree is its own repo.
        if ancestor.join(".git").exists() {
            break;
        }
    }
    (None, None)
}

fn collapse_home(path: &Path) -> String {
    collapse_home_path(path, home_dir().as_deref().map(Path::new))
}

/// Tilde-collapse with a path-component prefix so `HOME=/Users/u` does not
/// match `/Users/user/echoagent`, and trailing slashes on HOME do not break the join.
fn collapse_home_path(path: &Path, home: Option<&Path>) -> String {
    let Some(home) = home else {
        return path.display().to_string();
    };
    let home = PathBuf::from(
        home.as_os_str()
            .to_string_lossy()
            .trim_end_matches(['/', '\\']),
    );
    path.strip_prefix(&home)
        .map(|rest| format!("~/{}", rest.display()))
        .unwrap_or_else(|_| path.display().to_string())
}

/// Branch glyph for the git display, cached for process lifetime.
///
/// The Powerline glyph (`\u{e0a0}`) is a Nerd Font-only Private Use Area
/// codepoint, so it renders as "tofu" without a patched font. Where one can't
/// be assumed we fall back to a glyph the platform's stock fonts cover.
///
/// `ECHO_AGENT_NERD_FONTS=1` forces Powerline; `ECHO_AGENT_NERD_FONTS=0` forces the fallback.
pub(crate) fn branch_icon() -> &'static str {
    static ICON: OnceLock<&str> = OnceLock::new();
    ICON.get_or_init(|| {
        decide_branch_icon(
            std::env::var("ECHO_AGENT_NERD_FONTS").ok().as_deref(),
            HostOs::current(),
            terminal_context().brand,
        )
    })
}

/// Pure decision function for [`branch_icon`] so tests can drive inputs without
/// touching ambient env/host state.
fn decide_branch_icon(nerd_fonts: Option<&str>, host: HostOs, brand: TerminalName) -> &'static str {
    const POWERLINE: &str = "\u{e0a0}";
    // Windows console fonts lack `⎇`, so use `≡` (also in the legacy CP437 font).
    let fallback = if host == HostOs::Windows {
        "\u{2261}" // ≡
    } else {
        "\u{2387}" // ⎇
    };

    if decide_nerd_fonts(nerd_fonts, host, brand) {
        POWERLINE
    } else {
        fallback
    }
}

/// Whether a Nerd Font (Private Use Area glyphs) is plausible for this
/// host/terminal. Used by [`decide_branch_icon`] to pick a Powerline glyph.
///
/// An explicit `ECHO_AGENT_NERD_FONTS` override (`0`/`false` → off, anything else →
/// on) always wins. Otherwise PUA glyphs are assumed everywhere except Windows
/// consoles and the macOS terminals that ship stock fonts (Apple Terminal,
/// iTerm2), which tofu them.
fn decide_nerd_fonts(nerd_fonts: Option<&str>, host: HostOs, brand: TerminalName) -> bool {
    if let Some(val) = nerd_fonts {
        return !matches!(val, "0" | "false");
    }
    let stock_font_terminal = matches!(brand, TerminalName::AppleTerminal | TerminalName::Iterm2);
    !(host == HostOs::Windows || stock_font_terminal)
}

pub(crate) fn home_dir() -> Option<String> {
    std::env::var("HOME").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cwd_git_info_lazy` returns `None` for a non-repo path and never
    /// panics when called without a tokio runtime (no refresh spawns).
    /// Uses a unique path so it doesn't race the shared cache with other
    /// tests.
    #[test]
    fn cwd_git_info_lazy_non_repo_is_none() {
        let p = Path::new("/nonexistent-echo-agent-git-info-lazy-test-zzz");
        assert!(cwd_git_info_lazy(p).is_none());
        // Second call hits the reserved (None) entry — still None.
        assert!(cwd_git_info_lazy(p).is_none());
    }

    /// `cwd_cache_insert` bounds the map at `CWD_GIT_CACHE_CAP`, evicting the
    /// least-recently-refreshed entry when a new key would overflow it, and
    /// never evicts when overwriting an existing key. Drives a local map so
    /// it can't race the shared static cache.
    #[test]
    fn cwd_cache_insert_evicts_least_recently_refreshed() {
        let mut cache: HashMap<PathBuf, CwdCacheEntry> = HashMap::new();
        let base = Instant::now();
        // Fill to capacity with strictly increasing timestamps; `/d/0` is oldest.
        for i in 0..CWD_GIT_CACHE_CAP {
            let ts = base + Duration::from_secs(i as u64);
            cwd_cache_insert(&mut cache, PathBuf::from(format!("/d/{i}")), (None, ts));
        }
        assert_eq!(cache.len(), CWD_GIT_CACHE_CAP);
        // A new key overflows → the oldest entry is evicted, len stays capped.
        let newest = base + Duration::from_secs(10_000);
        cwd_cache_insert(&mut cache, PathBuf::from("/d/new"), (None, newest));
        assert_eq!(cache.len(), CWD_GIT_CACHE_CAP);
        assert!(
            !cache.contains_key(Path::new("/d/0")),
            "least-recently-refreshed entry must be evicted"
        );
        assert!(
            cache.contains_key(Path::new("/d/new")),
            "new entry inserted"
        );
        // Overwriting an existing key must not evict (no overflow).
        cwd_cache_insert(
            &mut cache,
            PathBuf::from("/d/new"),
            (None, base + Duration::from_secs(20_000)),
        );
        assert_eq!(cache.len(), CWD_GIT_CACHE_CAP);
    }

    /// A `None` refresh result must preserve a previously-resolved value (a
    /// transient libgit2 discovery failure must not blank the branch), while
    /// a fresh cwd with no prior entry still records the `None` (genuine
    /// non-repo). Either way the timestamp advances.
    #[test]
    fn apply_cwd_git_refresh_preserves_last_good_on_none() {
        let mut cache: HashMap<PathBuf, CwdCacheEntry> = HashMap::new();
        let dir = PathBuf::from("/repo");
        let good = CwdGitInfo {
            branch: Some("main".into()),
            is_worktree: false,
            main_repo: None,
            worktree_label: None,
        };
        // Seed a resolved entry with an old timestamp.
        let old_ts = Instant::now() - Duration::from_secs(60);
        cwd_cache_insert(&mut cache, dir.clone(), (Some(good), old_ts));

        // A `None` refresh must keep the branch and bump the timestamp.
        apply_cwd_git_refresh(&mut cache, dir.clone(), None);
        let (kept, ts) = cache.get(&dir).expect("entry retained");
        assert_eq!(
            kept.as_ref().and_then(|i| i.branch.as_deref()),
            Some("main"),
            "a transient None must not blank a resolved branch"
        );
        assert!(*ts > old_ts, "timestamp must advance on refresh");

        // A fresh cwd that resolves to None records the miss (genuine non-repo).
        let fresh = PathBuf::from("/not-a-repo");
        apply_cwd_git_refresh(&mut cache, fresh.clone(), None);
        assert!(matches!(cache.get(&fresh), Some((None, _))));
    }

    const POWERLINE: &str = "\u{e0a0}";
    const ALT_KEY: &str = "\u{2387}"; // ⎇
    const WIN_FALLBACK: &str = "\u{2261}"; // ≡

    #[test]
    fn windows_default_avoids_powerline_and_alt_key() {
        let icon = decide_branch_icon(None, HostOs::Windows, TerminalName::Unknown);
        assert_eq!(icon, WIN_FALLBACK);
        assert_ne!(icon, POWERLINE);
        assert_ne!(icon, ALT_KEY);
    }

    #[test]
    fn windows_terminal_brand_is_irrelevant_without_nerd_fonts() {
        for brand in [
            TerminalName::Unknown,
            TerminalName::VsCode,
            TerminalName::AppleTerminal,
            TerminalName::WezTerm,
            TerminalName::WindowsTerminal,
        ] {
            assert_eq!(
                decide_branch_icon(None, HostOs::Windows, brand),
                WIN_FALLBACK
            );
        }
    }

    #[test]
    fn nerd_fonts_opt_in_forces_powerline_even_on_windows() {
        for val in ["1", "true", "yes", "on"] {
            assert_eq!(
                decide_branch_icon(Some(val), HostOs::Windows, TerminalName::Unknown),
                POWERLINE
            );
            assert_eq!(
                decide_branch_icon(Some(val), HostOs::Macos, TerminalName::Unknown),
                POWERLINE
            );
        }
    }

    #[test]
    fn nerd_fonts_opt_out_forces_platform_fallback() {
        for val in ["0", "false"] {
            assert_eq!(
                decide_branch_icon(Some(val), HostOs::Windows, TerminalName::Unknown),
                WIN_FALLBACK
            );
            assert_eq!(
                decide_branch_icon(Some(val), HostOs::Macos, TerminalName::Unknown),
                ALT_KEY
            );
        }
    }

    #[test]
    fn non_windows_defaults_to_powerline() {
        assert_eq!(
            decide_branch_icon(None, HostOs::Macos, TerminalName::Unknown),
            POWERLINE
        );
        assert_eq!(
            decide_branch_icon(None, HostOs::Macos, TerminalName::VsCode),
            POWERLINE
        );
        assert_eq!(
            decide_branch_icon(None, HostOs::Linux, TerminalName::Ghostty),
            POWERLINE
        );
    }

    #[test]
    fn macos_stock_font_terminals_use_alt_key() {
        // Stock-font macOS terminals tofu the PUA glyph, so both must use `⎇`.
        for brand in [TerminalName::AppleTerminal, TerminalName::Iterm2] {
            assert_eq!(decide_branch_icon(None, HostOs::Macos, brand), ALT_KEY);
            assert_ne!(decide_branch_icon(None, HostOs::Macos, brand), POWERLINE);
        }
    }

    #[test]
    fn iterm2_nerd_fonts_opt_in_forces_powerline() {
        assert_eq!(
            decide_branch_icon(Some("1"), HostOs::Macos, TerminalName::Iterm2),
            POWERLINE
        );
    }

    #[test]
    fn collapse_home_requires_whole_path_component() {
        let home = Path::new("/Users/u");
        assert_eq!(
            collapse_home_path(Path::new("/Users/user/echoagent"), Some(home)),
            "/Users/user/echoagent"
        );
        assert_eq!(
            collapse_home_path(Path::new("/Users/u/echoagent"), Some(home)),
            "~/echoagent"
        );
        assert_eq!(
            collapse_home_path(Path::new("/Users/u/"), Some(Path::new("/Users/u/"))),
            "~/"
        );
    }

    #[test]
    fn compute_cwd_git_info_standalone_echo_agent_worktree() {
        let main = crate::test_util::TempGitRepo::init("main-only");
        let clone = main.standalone_clone("wt-branch");
        assert!(
            clone.path.join(".git").is_dir(),
            "standalone clone .git is a directory"
        );
        assert!(
            clone
                .path
                .join(".git")
                .join("echo-agent-worktree-source")
                .is_file(),
            "standalone clone carries the source marker"
        );
        let info = compute_cwd_git_info(&clone.path).expect("clone is a repo");
        assert!(info.is_worktree);
        assert_eq!(info.branch.as_deref(), Some("wt-branch"));
        assert_eq!(
            info.main_repo.as_deref(),
            Some(crate::test_util::collapsed_path_display(&main.path).as_str())
        );
    }

    #[test]
    fn compute_cwd_git_info_standalone_marker_from_nested_cwd() {
        let main = crate::test_util::TempGitRepo::init("main-only");
        let clone = main.standalone_clone("wt-branch");
        let nested = clone.path.join("sub").join("dir");
        std::fs::create_dir_all(&nested).unwrap();
        let info = compute_cwd_git_info(&nested).expect("nested path is in the clone");
        assert!(info.is_worktree);
        assert_eq!(info.branch.as_deref(), Some("wt-branch"));
        assert_eq!(
            info.main_repo.as_deref(),
            Some(crate::test_util::collapsed_path_display(&main.path).as_str())
        );
    }

    #[test]
    fn compute_cwd_git_info_nested_plain_repo_does_not_inherit_marker() {
        let main = crate::test_util::TempGitRepo::init("main-only");
        let clone = main.standalone_clone("wt-branch");
        let nested = clone.path.join("vendor").join("dep");
        crate::test_util::init_git_repo_on_branch(&nested, "dep-branch");
        let info = compute_cwd_git_info(&nested).expect("nested init is a repo");
        assert!(!info.is_worktree);
        assert!(info.main_repo.is_none());
        assert_eq!(info.branch.as_deref(), Some("dep-branch"));
    }

    #[serial_test::serial(ECHO_AGENT_HOME)]
    #[test]
    fn compute_cwd_git_info_nested_repo_does_not_inherit_db_record() {
        let home = tempfile::tempdir().unwrap();
        // serial(ECHO_AGENT_HOME) orders peers; EnvVarGuard restores on drop so
        // later `open_default()` callers do not see a deleted temp home.
        let _echo_agent_home = crate::test_util::EnvVarGuard::set("ECHO_AGENT_HOME", home.path());
        let _ = echo_agent_fast_worktree::db::WorktreeDb::open(home.path());

        let wt = crate::test_util::TempGitRepo::init("wt-branch");
        let canon_wt = dunce::canonicalize(&wt.path).unwrap_or_else(|_| wt.path.clone());
        let db = echo_agent_fast_worktree::db::WorktreeDb::open(home.path()).unwrap();
        db.register(&echo_agent_fast_worktree::db::WorktreeRecord {
            id: "db-wt".into(),
            path: canon_wt,
            source_repo: PathBuf::from("/src/main-repo"),
            repo_name: "main-repo".into(),
            kind: echo_agent_fast_worktree::db::WorktreeKind::Session,
            creation_mode: "standalone".into(),
            git_ref: None,
            head_commit: None,
            session_id: None,
            creator_pid: None,
            created_at: 1,
            last_accessed_at: None,
            status: echo_agent_fast_worktree::db::WorktreeStatus::Alive,
            metadata: Some(serde_json::json!({ "label": "db-label" })),
        })
        .unwrap();

        let nested = wt.path.join("vendor").join("dep");
        crate::test_util::init_git_repo_on_branch(&nested, "dep-branch");
        let info = compute_cwd_git_info(&nested).expect("nested init is a repo");
        assert!(!info.is_worktree);
        assert!(info.main_repo.is_none());
        assert!(info.worktree_label.is_none());
        assert_eq!(info.branch.as_deref(), Some("dep-branch"));
    }

    #[test]
    fn update_from_notification_ors_cached_label_into_is_worktree() {
        let dir = PathBuf::from("/nonexistent-echo-agent-notif-label-wt");
        {
            let mut cache = CWD_GIT_CACHE.lock().expect("cache lock");
            cwd_cache_insert(
                &mut cache,
                dir.clone(),
                (
                    Some(CwdGitInfo {
                        branch: Some("main".into()),
                        is_worktree: true,
                        main_repo: None,
                        worktree_label: Some("my-label".into()),
                    }),
                    Instant::now(),
                ),
            );
        }
        update_from_notification(&dir, Some("main"), None, /* is_worktree */ false);
        let cache = CWD_GIT_CACHE.lock().expect("cache lock");
        let info = cache
            .get(&dir)
            .and_then(|(info, _)| info.clone())
            .expect("cache entry");
        assert!(info.is_worktree);
        assert_eq!(info.worktree_label.as_deref(), Some("my-label"));
    }

    #[test]
    fn compute_cwd_git_info_plain_repo_is_not_worktree() {
        let repo = crate::test_util::TempGitRepo::init("main");
        let info = compute_cwd_git_info(&repo.path).expect("plain repo");
        assert!(!info.is_worktree);
        assert_eq!(info.branch.as_deref(), Some("main"));
        assert!(info.main_repo.is_none());
    }

    #[test]
    fn compute_cwd_git_info_linked_git_worktree() {
        let main = crate::test_util::TempGitRepo::init("main-only");
        let wt = main.add_linked_worktree("wt", "wt-branch");
        assert!(wt.join(".git").is_file(), "linked worktree .git is a file");
        let info = compute_cwd_git_info(&wt).expect("linked worktree is a repo");
        assert!(info.is_worktree);
        assert_eq!(info.branch.as_deref(), Some("wt-branch"));
        let main_repo = info.main_repo.expect("linked worktree has main_repo");
        let expected = crate::test_util::collapsed_path_display(&main.path);
        let expected_canon = dunce::canonicalize(&main.path)
            .map(|p| crate::test_util::collapsed_path_display(&p))
            .unwrap_or_else(|_| expected.clone());
        assert!(
            main_repo == expected || main_repo == expected_canon,
            "main_repo={main_repo}, expected {expected} or {expected_canon}"
        );
    }
}
