use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_mini::{DebounceEventResult, Debouncer, new_debouncer_opt};
use tokio::sync::mpsc;

const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(1000);

/// A [`notify::Watcher`] that drops `EventKind::Access` before it reaches the
/// debouncer, breaking the MCP/skills reload storm.
///
/// `notify`'s inotify backend emits an `Access` event on every *read*, and the
/// leader re-reads the files it watches on each reload — so unfiltered, a
/// reload's own reads schedule the next reload, a ~1/sec self-sustaining loop.
/// Dropping `Access` is safe: writes still emit `Modify`/`Create` and chmod
/// emits `Modify(Metadata)`; only reads are `Access`-only.
pub(crate) struct AccessFilteredWatcher(notify::RecommendedWatcher);

impl notify::Watcher for AccessFilteredWatcher {
    fn new<F: notify::EventHandler>(
        mut event_handler: F,
        config: notify::Config,
    ) -> notify::Result<Self>
    where
        Self: Sized,
    {
        let inner = notify::RecommendedWatcher::new(
            move |res: notify::Result<notify::Event>| match &res {
                Ok(event) if matches!(event.kind, notify::EventKind::Access(_)) => {}
                _ => event_handler.handle_event(res),
            },
            config,
        )?;
        Ok(Self(inner))
    }

    fn watch(&mut self, path: &Path, recursive_mode: RecursiveMode) -> notify::Result<()> {
        self.0.watch(path, recursive_mode)
    }

    fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
        self.0.unwatch(path)
    }

    fn configure(&mut self, option: notify::Config) -> notify::Result<bool> {
        self.0.configure(option)
    }

    fn kind() -> notify::WatcherKind
    where
        Self: Sized,
    {
        notify::RecommendedWatcher::kind()
    }
}

/// `new_debouncer` equivalent that builds the debouncer on top of
/// [`AccessFilteredWatcher`] instead of the raw `RecommendedWatcher`.
fn new_filtered_debouncer<F: notify_debouncer_mini::DebounceEventHandler>(
    timeout: Duration,
    event_handler: F,
) -> Result<Debouncer<AccessFilteredWatcher>, notify::Error> {
    let config = notify_debouncer_mini::Config::default().with_timeout(timeout);
    new_debouncer_opt::<F, AccessFilteredWatcher>(config, event_handler)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigChangeEvent {
    AuthChanged,
    GlobalConfigChanged,
    /// `~/.echo-agent/models_cache.json` changed — the on-disk `/v1/models`
    /// catalog cache was rewritten, possibly by **another** echoagent process
    /// sharing the same `~/.echo-agent` (the writer may also be this process;
    /// the [`ModelsManager`](crate::agent::models::ModelsManager) dedupes
    /// by content before applying).
    ModelsCacheChanged,
    ProjectConfigChanged {
        path: PathBuf,
    },
    /// A project-scoped MCP config file changed
    /// (`<cwd>/.mcp.json` or `<cwd>/.claude.json` where `<cwd>` is a
    /// project root, **not** `$HOME`). Project `<cwd>` is derived
    /// from `path.parent()` by the reloader.
    McpConfigChanged {
        path: PathBuf,
    },
    /// The user's **home-level** `~/.claude.json` changed. Distinct
    /// from [`Self::McpConfigChanged`] because `~/.claude.json` is
    /// loaded for **every** session regardless of cwd (see
    /// `load_claude_json_mcp_servers_as_configs`), so the reload
    /// must broadcast through the legacy unit
    /// [`super::reloader::ConfigUpdate::McpServersChanged`] arm —
    /// routing it through `ProjectMcpServersChanged { cwd: $HOME }`
    /// would silently skip sessions whose cwd doesn't sit under
    /// `$HOME`.
    HomeClaudeJsonChanged,
}

/// Watches `~/.echo-agent/` for `auth.json`, `config.toml`, and `models_cache.json`
/// changes, plus any extra paths (project `.echo-agent/config.toml`, `.mcp.json`,
/// etc.) provided at startup.
///
/// Uses `notify-debouncer-mini` for built-in debounce that coalesces rapid
/// editor writes (including write-then-rename patterns).
///
/// Self-write suppression is intentionally omitted. When the agent writes
/// `auth.json` or `config.toml`, the watcher will fire and the
/// [`ConfigReloader`](super::reloader::ConfigReloader) will re-read the file.
/// The reloader's own content-based deduplication (auth key hash, toml value
/// comparison) skips the update when nothing actually changed, so the
/// redundant read is harmless. This avoids a class of bugs where an
/// optimistic suppression window accidentally swallows writes from external
/// processes (e.g. `echoagent login` in another terminal).
///
/// Adds two **non-recursive** watches per `cwd` argument:
/// `<cwd>/` (catches `.mcp.json` and `.claude.json` at the project root) and
/// `<cwd>/.echo-agent/` (catches `<cwd>/.echo-agent/config.toml`). Recursing on `<cwd>`
/// would walk `node_modules/`, `target/`, `.git/`, etc. and blow through
/// `fs.inotify.max_user_watches` on large repos. Use [`Self::watch_path`]
/// to register additional cwds at runtime when new sessions open in
/// previously-unwatched directories.
pub struct ConfigFileWatcher {
    debouncer: Debouncer<AccessFilteredWatcher>,
    /// Project cwds currently registered (via [`Self::start`]'s `cwd`
    /// argument or [`Self::watch_path`]). Tracked so that
    /// (a) [`Self::watch_path`] is idempotent at our layer instead of
    /// relying on `notify`'s internal de-dup, and
    /// (b) [`Self::unwatch_path`] can drop the OS watches for a cwd
    /// that is no longer needed, bounding inotify-watch accumulation
    /// as sessions churn across directories.
    watched_cwds: HashSet<PathBuf>,
}

impl ConfigFileWatcher {
    /// Start watching. Returns `None` if the OS watcher fails to initialize.
    ///
    /// `cwd`, when `Some`, adds two non-recursive watches: `<cwd>/` and
    /// `<cwd>/.echo-agent/`. Use [`Self::watch_path`] later to register additional
    /// project cwds for sessions that open in previously-unwatched
    /// directories.
    pub fn start(
        echo_agent_home: &Path,
        extra_paths: &[PathBuf],
        cwd: Option<&Path>,
        debounce: Option<Duration>,
    ) -> Option<(Self, mpsc::UnboundedReceiver<ConfigChangeEvent>)> {
        let debounce = debounce.unwrap_or(DEFAULT_DEBOUNCE);
        let (tx, rx) = mpsc::unbounded_channel();
        // FSEvents reports canonical paths even when the configured home uses
        // a symlink (including macOS /var -> /private/var).
        let echo_agent_home_buf =
            dunce::canonicalize(echo_agent_home).unwrap_or_else(|_| echo_agent_home.to_path_buf());
        // `~/.claude.json` is consumed by **every**
        // session (see `load_claude_json_mcp_servers_as_configs`), so
        // a write to it must broadcast through the unit
        // `McpServersChanged` arm — NOT through the per-cwd
        // `ProjectMcpServersChanged { cwd: $HOME }` arm, which
        // `cwd_matches` would silently filter for sessions outside
        // `$HOME`. We snapshot `$HOME` here so the closure can
        // discriminate `<home>/.claude.json` from a project-level
        // `<cwd>/.claude.json` purely by path.
        //
        // Canonicalize `$HOME` ONCE up front. `notify`
        // backends may deliver canonicalized event paths (e.g. macOS
        // FSEvents resolves symlinks, returning `/private/var/...`
        // where `echo_agent_config::discovery_home_dir()` returned `/var/...`), so a raw byte
        // compare against an un-canonicalized `$HOME` would mis-route
        // `~/.claude.json` to the per-cwd path. The per-event side is
        // canonicalized in `parent_is_dir`.
        let user_home_buf: Option<PathBuf> =
            echo_agent_config::discovery_home_dir().map(|h| dunce::canonicalize(&h).unwrap_or(h));

        let mut debouncer = new_filtered_debouncer(debounce, move |res: DebounceEventResult| {
            let Ok(events) = res else { return };

            let mut batch_events: Vec<ConfigChangeEvent> = Vec::new();
            for event in events {
                let path = &event.path;
                let change =
                    config_change_for_path(path, &echo_agent_home_buf, user_home_buf.as_deref());

                if let Some(evt) = change
                    && !batch_events.contains(&evt)
                {
                    batch_events.push(evt);
                }
            }
            for evt in batch_events {
                let _ = tx.send(evt);
            }
        })
        .map_err(|e| tracing::warn!(error = %e, "failed to create config file watcher"))
        .ok()?;

        debouncer
            .watcher()
            .watch(echo_agent_home, RecursiveMode::NonRecursive)
            .map_err(|e| {
                tracing::warn!(
                    path = %echo_agent_home.display(),
                    error = %e,
                    "failed to watch echoagent home directory"
                )
            })
            .ok()?;

        for p in extra_paths {
            if let Some(parent) = p.parent() {
                let _ = debouncer
                    .watcher()
                    .watch(parent, RecursiveMode::NonRecursive);
            }
        }

        // Add the two narrow non-recursive cwd watches
        // promoted to first-class watch targets. Both are non-fatal —
        // a missing directory just means the corresponding files don't
        // exist yet and will be picked up by `watch_path` on the next
        // session that opens in this cwd.
        //
        // When the leader's own cwd is also covered by
        // `extra_paths` (e.g. `find_project_configs(cwd)` already
        // includes `<cwd>/.echo-agent/config.toml` so the loop above
        // watches `<cwd>/.echo-agent/`), the call below installs a
        // duplicate watch on the same directory. `notify` dedupes
        // silently in its `RecommendedWatcher` (last-write-wins for
        // the recursion mode), so this is cosmetic — both
        // additions remain non-recursive, no event amplification.
        let mut watched_cwds = HashSet::new();
        if let Some(cwd) = cwd {
            watch_cwd_dirs(&mut debouncer, cwd);
            watched_cwds.insert(cwd.to_path_buf());
        }

        tracing::info!(
            echo_agent_home = %echo_agent_home.display(),
            extra_paths = extra_paths.len(),
            cwd = ?cwd,
            debounce_ms = debounce.as_millis(),
            "config file watcher started"
        );

        Some((
            Self {
                debouncer,
                watched_cwds,
            },
            rx,
        ))
    }

    /// Register `<cwd>/` and `<cwd>/.echo-agent/` as **non-recursive** watch
    /// targets, in addition to whatever was passed to [`Self::start`].
    ///
    /// Intended for the session-open path: when a session opens in a cwd
    /// the leader hasn't seen before, calling this method ensures edits to
    /// `<cwd>/.mcp.json` and `<cwd>/.echo-agent/config.toml` trigger a
    /// [`ConfigChangeEvent`] (and downstream [`ConfigUpdate::
    /// ProjectMcpServersChanged`](super::reloader::ConfigUpdate::
    /// ProjectMcpServersChanged)) within the debounce window.
    ///
    /// **Non-recursive by design.** Watching `<cwd>` recursively would
    /// walk `node_modules/`, `target/`, `.git/`, etc. and easily exhaust
    /// the per-user inotify quota (`fs.inotify.max_user_watches`,
    /// commonly 8192 by default) on a large repo. If `notify` cannot register the watch (e.g.
    /// the directory doesn't exist yet, or the OS quota is reached) the
    /// error is logged and swallowed — the leader continues to rely on
    /// the user-triggered refresh as the fallback.
    pub fn watch_path(&mut self, cwd: &Path) {
        // Idempotent at our layer: skip the redundant
        // `notify` watch-add when this cwd is already registered, so
        // re-opening sessions in the same directory doesn't churn the
        // OS watcher. `notify` de-dups internally too, but tracking the
        // set here also enables `unwatch_path`.
        if self.watched_cwds.contains(cwd) {
            return;
        }
        watch_cwd_dirs(&mut self.debouncer, cwd);
        self.watched_cwds.insert(cwd.to_path_buf());
    }

    /// Remove the two non-recursive watches (`<cwd>/` and
    /// `<cwd>/.echo-agent/`) previously registered for `cwd` via
    /// [`Self::start`] / [`Self::watch_path`].
    ///
    /// Best-effort and idempotent: a `cwd` that was never registered
    /// (or already unwatched) is a no-op. Intended for the
    /// session-teardown path so a long-lived leader that opens sessions
    /// across many directories doesn't accumulate inotify watches for
    /// cwds with no live sessions. **Callers must ref-count**: only
    /// unwatch once the *last* session sharing this cwd closes —
    /// `ConfigFileWatcher` tracks distinct cwds, not session counts.
    pub fn unwatch_path(&mut self, cwd: &Path) {
        if !self.watched_cwds.remove(cwd) {
            return;
        }
        unwatch_cwd_dirs(&mut self.debouncer, cwd);
    }
}

/// Both directory references are canonicalized once when the watcher starts.
fn config_change_for_path(
    path: &Path,
    echo_agent_home: &Path,
    user_home: Option<&Path>,
) -> Option<ConfigChangeEvent> {
    let name = path.file_name().and_then(|n| n.to_str());
    let parent = path.parent();
    match name {
        Some("auth.json") if parent_is_dir(parent, echo_agent_home) => {
            Some(ConfigChangeEvent::AuthChanged)
        }
        Some("config.toml") if parent_is_dir(parent, echo_agent_home) => {
            Some(ConfigChangeEvent::GlobalConfigChanged)
        }
        Some("models_cache.json") if parent_is_dir(parent, echo_agent_home) => {
            Some(ConfigChangeEvent::ModelsCacheChanged)
        }
        Some("config.toml") => Some(ConfigChangeEvent::ProjectConfigChanged {
            path: path.to_path_buf(),
        }),
        // `~/.claude.json` routes through
        // the dedicated home-level variant so the
        // reloader can broadcast. Project-level
        // `<cwd>/.claude.json` (and any `.mcp.json`)
        // continues to be a per-cwd reload.
        Some(".claude.json") if user_home.is_some_and(|h| parent_is_dir(parent, h)) => {
            Some(ConfigChangeEvent::HomeClaudeJsonChanged)
        }
        Some(".mcp.json") | Some(".claude.json") => Some(ConfigChangeEvent::McpConfigChanged {
            path: path.to_path_buf(),
        }),
        _ => None,
    }
}

/// Component-aware "is `parent` the directory `dir`?" that tolerates
/// symlink / canonicalization differences between a `notify`-delivered
/// event path and a `echo_agent_config::discovery_home_dir()`-style reference. `dir` is
/// expected to be already canonicalized (see `ConfigFileWatcher::start`).
fn parent_is_dir(parent: Option<&Path>, dir: &Path) -> bool {
    let Some(parent) = parent else {
        return false;
    };
    parent == dir || dunce::canonicalize(parent).is_ok_and(|p| p == dir)
}

/// Add the two non-recursive watches for a project root.
///
/// Both watches are best-effort and log-and-continue on failure (missing
/// directory, quota exhausted, permission denied, etc.) — the caller has
/// no reasonable recovery path beyond the existing user-triggered refresh.
///
/// **Known limitation:** if `<cwd>/.echo-agent/` does not yet
/// exist at session-open time, the `.echo-agent/` watch fails ENOENT and is
/// swallowed at `debug!`. A later `mkdir <cwd>/.echo-agent/` followed by a
/// write to `<cwd>/.echo-agent/config.toml` will NOT be observed — the
/// `<cwd>/` watch is non-recursive, so subdirectory creation isn't
/// surfaced as a watch-add trigger. Users hitting this case must hit
/// the explicit refresh button. A robust fix (re-attempt on parent-
/// directory create) is out of scope here.
fn watch_cwd_dirs(debouncer: &mut Debouncer<AccessFilteredWatcher>, cwd: &Path) {
    if let Err(e) = debouncer.watcher().watch(cwd, RecursiveMode::NonRecursive) {
        log_watch_error(&e, "failed to watch project cwd (non-recursive)");
    }
    let echo_agent_dir = cwd.join(".echo-agent");
    if let Err(e) = debouncer
        .watcher()
        .watch(&echo_agent_dir, RecursiveMode::NonRecursive)
    {
        log_watch_error(
            &e,
            "failed to watch project .echo-agent directory (non-recursive)",
        );
    }
}

/// Remove the two non-recursive watches added by [`watch_cwd_dirs`].
/// Best-effort: a `WatchNotFound` (never watched / already removed) is
/// expected and logged at `debug!`.
fn unwatch_cwd_dirs(debouncer: &mut Debouncer<AccessFilteredWatcher>, cwd: &Path) {
    if let Err(e) = debouncer.watcher().unwatch(cwd) {
        tracing::debug!(error = %e, "failed to unwatch project cwd");
    }
    let echo_agent_dir = cwd.join(".echo-agent");
    if let Err(e) = debouncer.watcher().unwatch(&echo_agent_dir) {
        tracing::debug!(error = %e, "failed to unwatch project .echo-agent directory");
    }
}

/// Log a `notify` watch failure, distinguishing the benign
/// "directory doesn't exist yet" case (logged at `debug!` — it's
/// expected for a freshly-opened session whose `<cwd>/.echo-agent/` hasn't
/// been created) from genuinely actionable failures like
/// `fs.inotify.max_user_watches` exhaustion or permission denied
/// (logged at `warn!` — these mean live edits will be silently
/// missed). Don't swallow every error at the same level.
fn log_watch_error(err: &notify::Error, msg: &str) {
    let not_found = matches!(err.kind, notify::ErrorKind::PathNotFound)
        || matches!(&err.kind, notify::ErrorKind::Io(io) if io.kind() == std::io::ErrorKind::NotFound);
    if not_found {
        tracing::debug!(error = %err, "{msg} (path not found)");
    } else {
        tracing::warn!(error = %err, "{msg}");
    }
}

const SKILLS_DEBOUNCE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryChange {
    Skills,
    Workflows,
}

fn discovery_change_for_path(path: &Path) -> Option<DiscoveryChange> {
    let file_name = path.file_name().and_then(|name| name.to_str());
    if file_name.is_some_and(|name| VENDOR_CONFIG_ROOT_NAMES.contains(&name)) {
        return Some(DiscoveryChange::Skills);
    }
    if file_name.is_some_and(|name| name == "workflows")
        || path
            .ancestors()
            .any(|ancestor| ancestor.file_name().is_some_and(|name| name == "workflows"))
    {
        return Some(DiscoveryChange::Workflows);
    }
    if file_name.is_some_and(|name| name == "skills" || name == "commands" || name == "SKILL.md")
        || path
            .ancestors()
            .any(|ancestor| ancestor.file_name().is_some_and(|name| name == "skills"))
        || (path.extension().is_some_and(|extension| extension == "md")
            && path
                .parent()
                .is_some_and(|parent| parent.file_name().is_some_and(|name| name == "commands")))
    {
        return Some(DiscoveryChange::Skills);
    }
    None
}

/// Known vendor config root basenames; kept in sync with `collect_skill_config_dirs`.
const VENDOR_CONFIG_ROOT_NAMES: &[&str] = &[".echo-agent", ".agents", ".claude", ".cursor"];

/// Vendor roots (by name or `echo_agent_home`) must use scoped watches — they can
/// contain large non-skill trees (`worktrees/`, etc.).
fn is_vendor_config_root(dir: &Path, echo_agent_home: &Path) -> bool {
    if paths_equal(dir, echo_agent_home) {
        return true;
    }
    dir.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| VENDOR_CONFIG_ROOT_NAMES.contains(&n))
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    let canon = |p: &Path| dunce::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(a) == canon(b)
}

fn dirs_contain(dirs: &[PathBuf], target: &Path) -> bool {
    dirs.iter().any(|dir| paths_equal(dir, target))
}

fn path_set_contains(dirs: &HashSet<PathBuf>, target: &Path) -> bool {
    dirs.iter().any(|dir| paths_equal(dir, target))
}

fn vendor_skill_refresh_dirs(config_dir: &Path) -> [(PathBuf, RecursiveMode); 3] {
    [
        (config_dir.join("skills"), RecursiveMode::Recursive),
        (config_dir.join("commands"), RecursiveMode::NonRecursive),
        (config_dir.join("workflows"), RecursiveMode::NonRecursive),
    ]
}

fn project_echo_agent_refresh_dirs(project_root: &Path) -> Vec<(PathBuf, RecursiveMode)> {
    let project_echo_agent = project_root.join(".echo-agent");
    let mut dirs = vec![(project_echo_agent.clone(), RecursiveMode::NonRecursive)];
    dirs.extend(vendor_skill_refresh_dirs(&project_echo_agent));
    dirs
}

fn attach_new_refresh_dirs(
    debouncer: &mut Debouncer<AccessFilteredWatcher>,
    refresh_dirs: &[(PathBuf, RecursiveMode)],
    refreshed_dirs: &mut HashSet<PathBuf>,
    err_msg: &str,
) -> bool {
    let mut changed = false;
    for (dir, mode) in refresh_dirs {
        if path_set_contains(refreshed_dirs, dir) || !dir.is_dir() {
            continue;
        }
        match debouncer.watcher().watch(dir, *mode) {
            Ok(()) => {
                refreshed_dirs.insert(dir.clone());
                changed = true;
            }
            Err(error) => log_watch_error(&error, err_msg),
        }
    }
    changed
}

/// Paths successfully watched under a scoped vendor root (root + skill subdirs).
fn watch_skill_subdirs(
    debouncer: &mut Debouncer<AccessFilteredWatcher>,
    config_dir: &Path,
) -> HashSet<PathBuf> {
    let mut watched = HashSet::new();
    match debouncer
        .watcher()
        .watch(config_dir, RecursiveMode::NonRecursive)
    {
        Ok(()) => {
            watched.insert(config_dir.to_path_buf());
        }
        Err(error) => log_watch_error(&error, "failed to watch config dir root"),
    }
    for (dir, mode) in vendor_skill_refresh_dirs(config_dir) {
        if !dir.is_dir() {
            continue;
        }
        match debouncer.watcher().watch(&dir, mode) {
            Ok(()) => {
                watched.insert(dir);
            }
            Err(error) => log_watch_error(&error, "failed to watch discovery subdir"),
        }
    }
    watched
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SkillsWatchPlan {
    vendor_roots: Vec<PathBuf>,
    recursive_roots: Vec<PathBuf>,
    /// Non-recursive parent so first create of a missing project vendor root is observed.
    project_parent_watch: Option<PathBuf>,
    refresh_dirs: Vec<(PathBuf, RecursiveMode)>,
}

/// Pure composition: classify discovery roots and seed mid-session refresh targets.
fn plan_skills_watch_targets(
    dirs_to_watch: &[PathBuf],
    echo_agent_home: &Path,
    project_root: Option<&Path>,
) -> SkillsWatchPlan {
    let mut vendor_roots = Vec::new();
    let mut recursive_roots = Vec::new();
    let mut refresh_dirs = Vec::new();

    for dir in dirs_to_watch {
        if is_vendor_config_root(dir, echo_agent_home) {
            vendor_roots.push(dir.clone());
            refresh_dirs.extend(vendor_skill_refresh_dirs(dir));
        } else {
            recursive_roots.push(dir.clone());
        }
    }

    let mut project_parent_watch = None;
    if let Some(project_root) = project_root {
        let mut missing_project_vendor = false;
        for name in VENDOR_CONFIG_ROOT_NAMES {
            let vendor_root = project_root.join(name);
            if !dirs_contain(dirs_to_watch, &vendor_root) {
                missing_project_vendor = true;
                refresh_dirs.push((vendor_root.clone(), RecursiveMode::NonRecursive));
                refresh_dirs.extend(vendor_skill_refresh_dirs(&vendor_root));
            }
        }
        if missing_project_vendor && !dirs_contain(dirs_to_watch, project_root) {
            project_parent_watch = Some(project_root.to_path_buf());
        }
    }

    SkillsWatchPlan {
        vendor_roots,
        recursive_roots,
        project_parent_watch,
        refresh_dirs,
    }
}

/// Watches project `.echo-agent` skills/commands/workflows for mid-session discovery.
///
/// After a [`DiscoveryChange`], call [`Self::refresh_new_dirs`] so newly created
/// seed dirs get watches attached.
pub(crate) struct ProjectDiscoveryWatcher {
    debouncer: Debouncer<AccessFilteredWatcher>,
    refresh_dirs: Vec<(PathBuf, RecursiveMode)>,
    refreshed_dirs: HashSet<PathBuf>,
}

impl ProjectDiscoveryWatcher {
    pub(crate) fn start(cwd: &Path) -> Option<(Self, mpsc::UnboundedReceiver<DiscoveryChange>)> {
        let project_root = crate::session::workflow::registry::project_root(cwd);
        let project_echo_agent = project_root.join(".echo-agent");
        let (tx, rx) = mpsc::unbounded_channel();
        let project_echo_agent_for_events = project_echo_agent.clone();
        let mut debouncer =
            new_filtered_debouncer(SKILLS_DEBOUNCE, move |res: DebounceEventResult| {
                let Ok(events) = res else { return };
                let mut change = None;
                for event in events
                    .iter()
                    .filter(|event| event.path.starts_with(&project_echo_agent_for_events))
                {
                    let next = discovery_change_for_path(&event.path)
                        .unwrap_or(DiscoveryChange::Workflows);
                    if next == DiscoveryChange::Skills {
                        change = Some(next);
                        break;
                    }
                    change = Some(next);
                }
                if let Some(change) = change {
                    let _ = tx.send(change);
                }
            })
            .map_err(|error| tracing::warn!(%error, "failed to create project workflow watcher"))
            .ok()?;

        let initial = if project_echo_agent.is_dir() {
            project_echo_agent.clone()
        } else {
            project_root.clone()
        };
        if let Err(error) = debouncer
            .watcher()
            .watch(&initial, RecursiveMode::NonRecursive)
        {
            log_watch_error(&error, "failed to watch project workflow parent");
            return None;
        }
        let refresh_dirs = project_echo_agent_refresh_dirs(&project_root);
        let mut refreshed_dirs = HashSet::from([initial]);
        attach_new_refresh_dirs(
            &mut debouncer,
            &refresh_dirs,
            &mut refreshed_dirs,
            "failed to watch project discovery dir",
        );
        Some((
            Self {
                debouncer,
                refresh_dirs,
                refreshed_dirs,
            },
            rx,
        ))
    }

    /// Attach watches for seed dirs that now exist (call after a discovery event).
    pub(crate) fn refresh_new_dirs(&mut self) {
        attach_new_refresh_dirs(
            &mut self.debouncer,
            &self.refresh_dirs,
            &mut self.refreshed_dirs,
            "failed to watch newly-created project workflow dir",
        );
    }
}

/// Watches skill/command/workflow discovery dirs and classifies disk changes.
pub struct SkillsFileWatcher {
    debouncer: Debouncer<AccessFilteredWatcher>,
    refresh_dirs: Vec<(PathBuf, RecursiveMode)>,
    refreshed_dirs: HashSet<PathBuf>,
}

impl SkillsFileWatcher {
    /// Start watching discovery dirs from
    /// [`collect_skill_config_dirs`](echo_agent_core::prompt::skills::collect_skill_config_dirs).
    ///
    /// After a [`DiscoveryChange`], call [`Self::refresh_new_discovery_dirs`] so
    /// newly created seed dirs get watches attached.
    pub fn start(
        cwd: Option<&Path>,
        monorepo_user_dir: Option<&Path>,
        config_paths: &[String],
    ) -> Option<(Self, mpsc::UnboundedReceiver<DiscoveryChange>)> {
        let echo_agent_home = echo_agent_tools::util::echo_agent_home::echo_agent_home();
        // Watch the full superset of vendor dirs (all-on compat). This watcher
        // is leader-global (no per-session compat resolved here); the actual
        // per-session discovery gating happens downstream, so watching a
        // currently-disabled vendor dir is harmless (a change just re-runs the
        // gated discovery) and avoids ever missing a watch if a toggle flips.
        let dirs_to_watch = echo_agent_core::prompt::skills::collect_skill_config_dirs(
            cwd,
            monorepo_user_dir,
            &echo_agent_home,
            config_paths,
            echo_agent_tools::types::compat::CompatConfig::default(),
        );
        let project_root = cwd.map(crate::session::workflow::registry::project_root);
        Self::start_with_dirs(&dirs_to_watch, &echo_agent_home, project_root.as_deref())
    }

    /// Start with explicit discovery roots (benches and isolated tests).
    ///
    /// Production code should prefer [`Self::start`], which collects the same
    /// dir set discovery uses. After a [`DiscoveryChange`], call
    /// [`Self::refresh_new_discovery_dirs`].
    pub fn start_with_dirs(
        dirs_to_watch: &[PathBuf],
        echo_agent_home: &Path,
        project_root: Option<&Path>,
    ) -> Option<(Self, mpsc::UnboundedReceiver<DiscoveryChange>)> {
        let (tx, rx) = mpsc::unbounded_channel();

        let mut debouncer =
            new_filtered_debouncer(SKILLS_DEBOUNCE, move |res: DebounceEventResult| {
                let Ok(events) = res else { return };
                let mut change = None;
                for next in events
                    .iter()
                    .filter_map(|event| discovery_change_for_path(&event.path))
                {
                    if next == DiscoveryChange::Skills {
                        change = Some(next);
                        break;
                    }
                    change = Some(next);
                }
                if let Some(change) = change {
                    let _ = tx.send(change);
                }
            })
            .map_err(|e| tracing::warn!(error = %e, "failed to create skills file watcher"))
            .ok()?;

        let plan = plan_skills_watch_targets(dirs_to_watch, echo_agent_home, project_root);

        let mut watched = 0;
        let mut refreshed_dirs = HashSet::new();
        for dir in &plan.vendor_roots {
            let attached = watch_skill_subdirs(&mut debouncer, dir);
            watched += attached.len();
            refreshed_dirs.extend(attached);
        }
        for dir in &plan.recursive_roots {
            match debouncer.watcher().watch(dir, RecursiveMode::Recursive) {
                Ok(()) => {
                    watched += 1;
                    refreshed_dirs.insert(dir.clone());
                }
                Err(e) => log_watch_error(&e, "failed to watch directory for skill changes"),
            }
        }
        if let Some(parent_watch) = &plan.project_parent_watch {
            match debouncer
                .watcher()
                .watch(parent_watch, RecursiveMode::NonRecursive)
            {
                Ok(()) => {
                    watched += 1;
                    refreshed_dirs.insert(parent_watch.clone());
                }
                Err(error) => log_watch_error(
                    &error,
                    "failed to watch workflow discovery parent directory",
                ),
            }
        }

        if watched == 0 {
            tracing::debug!("no config directories found to watch for skills");
            return None;
        }

        tracing::info!(dirs = watched, "skills file watcher started");

        Some((
            Self {
                debouncer,
                refresh_dirs: plan.refresh_dirs,
                refreshed_dirs,
            },
            rx,
        ))
    }

    /// Attach watches for seed dirs that now exist (call after a discovery event).
    /// Returns true if any new watch was attached.
    pub fn refresh_new_discovery_dirs(&mut self) -> bool {
        attach_new_refresh_dirs(
            &mut self.debouncer,
            &self.refresh_dirs,
            &mut self.refreshed_dirs,
            "failed to watch newly-created discovery directory",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn wait_ms(ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }

    fn expect_change(
        rx: &mut mpsc::UnboundedReceiver<ConfigChangeEvent>,
        expected: ConfigChangeEvent,
    ) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut observed = Vec::new();
        while std::time::Instant::now() < deadline {
            while let Ok(event) = rx.try_recv() {
                if event == expected {
                    return;
                }
                observed.push(event);
            }
            wait_ms(10);
        }
        panic!("expected {expected:?} within 2s; observed {observed:?}");
    }

    #[test]
    fn global_config_events_are_scoped_to_the_data_home() {
        let tmp = TempDir::new().unwrap();
        let home = dunce::canonicalize(tmp.path()).unwrap();
        for (name, expected) in [
            ("auth.json", ConfigChangeEvent::AuthChanged),
            ("config.toml", ConfigChangeEvent::GlobalConfigChanged),
            ("models_cache.json", ConfigChangeEvent::ModelsCacheChanged),
        ] {
            // Routing must work for deleted files, too: only the parent exists.
            assert_eq!(
                config_change_for_path(&home.join(name), &home, None),
                Some(expected)
            );
            let project_file = home.join("project").join(name);
            let expected =
                (name == "config.toml").then(|| ConfigChangeEvent::ProjectConfigChanged {
                    path: project_file.clone(),
                });
            assert_eq!(config_change_for_path(&project_file, &home, None), expected);
        }
    }

    #[test]
    #[cfg(unix)]
    fn global_config_events_accept_symlink_and_canonical_parents() {
        let tmp = TempDir::new().unwrap();
        let real = tmp.path().join("data");
        fs::create_dir(&real).unwrap();
        let alias = tmp.path().join("data-link");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        let home = dunce::canonicalize(&alias).unwrap();
        assert_ne!(alias, home);
        for parent in [&alias, &home] {
            for (name, expected) in [
                ("auth.json", ConfigChangeEvent::AuthChanged),
                ("config.toml", ConfigChangeEvent::GlobalConfigChanged),
                ("models_cache.json", ConfigChangeEvent::ModelsCacheChanged),
            ] {
                assert_eq!(
                    config_change_for_path(&parent.join(name), &home, None),
                    Some(expected)
                );
            }
        }
    }

    #[test]
    fn is_vendor_config_root_matches_known_names_at_any_tier() {
        let home = TempDir::new().unwrap();
        let home = home.path();
        let echo_agent_home = home.join(".echo-agent");

        assert!(is_vendor_config_root(&echo_agent_home, &echo_agent_home));
        assert!(is_vendor_config_root(
            &home.join(".claude"),
            &echo_agent_home
        ));
        assert!(is_vendor_config_root(
            &home.join(".cursor"),
            &echo_agent_home
        ));
        assert!(is_vendor_config_root(
            &home.join(".agents"),
            &echo_agent_home
        ));
        assert!(is_vendor_config_root(
            &home.join("repo").join(".echo-agent"),
            &echo_agent_home
        ));
        assert!(is_vendor_config_root(
            &home.join("repo").join(".claude"),
            &echo_agent_home
        ));

        assert!(!is_vendor_config_root(
            &home.join("my-skills"),
            &echo_agent_home
        ));
        assert!(!is_vendor_config_root(
            &home.join(".config"),
            &echo_agent_home
        ));
        assert!(!is_vendor_config_root(
            &home.join("repo").join("my-skills"),
            &echo_agent_home
        ));

        let custom_home = home.join("custom-echo-agent-home");
        assert!(is_vendor_config_root(&custom_home, &custom_home));
    }

    #[test]
    fn vendor_skill_refresh_dirs_paths_and_modes() {
        let root = Path::new("/tmp/project/.claude");
        assert_eq!(
            vendor_skill_refresh_dirs(root),
            [
                (root.join("skills"), RecursiveMode::Recursive),
                (root.join("commands"), RecursiveMode::NonRecursive),
                (root.join("workflows"), RecursiveMode::NonRecursive),
            ]
        );
    }

    #[test]
    fn project_echo_agent_refresh_dirs_matches_vendor_layout() {
        let project = Path::new("/tmp/repo");
        let echoagent = project.join(".echo-agent");
        let dirs = project_echo_agent_refresh_dirs(project);

        assert_eq!(dirs.len(), 4);
        assert_eq!(dirs[0], (echoagent.clone(), RecursiveMode::NonRecursive));
        assert_eq!(
            &dirs[1..],
            [
                (echoagent.join("skills"), RecursiveMode::Recursive),
                (echoagent.join("commands"), RecursiveMode::NonRecursive),
                (echoagent.join("workflows"), RecursiveMode::NonRecursive),
            ]
        );
        assert_eq!(dirs[1..], vendor_skill_refresh_dirs(&echoagent));
    }

    #[test]
    #[cfg(unix)]
    fn path_set_contains_uses_paths_equal() {
        let tmp = TempDir::new().unwrap();
        let real = tmp.path().join("real_skills");
        fs::create_dir_all(&real).unwrap();
        let link = tmp.path().join("link_skills");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        assert_ne!(real.as_os_str(), link.as_os_str());
        let mut set = HashSet::new();
        set.insert(real.clone());

        assert!(path_set_contains(&set, &real));
        assert!(
            path_set_contains(&set, &link),
            "symlink form must match via paths_equal/canonicalize"
        );
        assert!(
            !set.contains(&link),
            "HashSet::contains must not match symlink form"
        );
        assert!(!path_set_contains(&set, &tmp.path().join("other")));
    }

    #[test]
    #[cfg(unix)]
    fn attach_new_refresh_dirs_skips_known_and_missing() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let known_real = root.join("known");
        let known_link = root.join("known_link");
        let missing = root.join("missing");
        let fresh = root.join("fresh");
        fs::create_dir(&known_real).unwrap();
        std::os::unix::fs::symlink(&known_real, &known_link).unwrap();
        fs::create_dir(&fresh).unwrap();

        let mut debouncer = new_filtered_debouncer(Duration::from_millis(50), |_| {}).unwrap();
        debouncer
            .watcher()
            .watch(root, RecursiveMode::NonRecursive)
            .unwrap();

        // Seed with symlink form; refresh_dirs lists the real path (paths_equal, not byte-equal).
        let refresh_dirs = vec![
            (known_real.clone(), RecursiveMode::NonRecursive),
            (missing.clone(), RecursiveMode::NonRecursive),
            (fresh.clone(), RecursiveMode::NonRecursive),
        ];
        let mut refreshed_dirs = HashSet::from([known_link.clone()]);
        assert_ne!(known_real.as_os_str(), known_link.as_os_str());
        assert!(!refreshed_dirs.contains(&known_real));

        assert!(attach_new_refresh_dirs(
            &mut debouncer,
            &refresh_dirs,
            &mut refreshed_dirs,
            "test attach",
        ));
        assert_eq!(
            refreshed_dirs.len(),
            2,
            "skip known (path-equal form) and missing; only fresh attaches"
        );
        assert!(path_set_contains(&refreshed_dirs, &known_real));
        assert!(path_set_contains(&refreshed_dirs, &known_link));
        assert!(path_set_contains(&refreshed_dirs, &fresh));
        assert!(!path_set_contains(&refreshed_dirs, &missing));
        assert!(!attach_new_refresh_dirs(
            &mut debouncer,
            &refresh_dirs,
            &mut refreshed_dirs,
            "test attach",
        ));
        assert_eq!(refreshed_dirs.len(), 2);
    }

    fn expected_missing_vendor_refresh_seeds(project_root: &Path) -> Vec<(PathBuf, RecursiveMode)> {
        let mut expected = Vec::new();
        for name in VENDOR_CONFIG_ROOT_NAMES {
            let root = project_root.join(name);
            expected.push((root.clone(), RecursiveMode::NonRecursive));
            expected.extend(vendor_skill_refresh_dirs(&root));
        }
        expected
    }

    /// Parent-only plan (empty dirs_to_watch) must still start so mid-session
    /// vendor creates under project_root can attach via refresh seeds.
    #[test]
    fn start_with_dirs_keeps_parent_only_watch() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path();
        let echo_agent_home = project.join("home-echo-agent");
        fs::create_dir_all(&echo_agent_home).unwrap();

        let plan = plan_skills_watch_targets(&[], &echo_agent_home, Some(project));
        assert!(plan.vendor_roots.is_empty());
        assert!(plan.recursive_roots.is_empty());
        assert_eq!(plan.project_parent_watch.as_deref(), Some(project));

        let (watcher, _rx) =
            SkillsFileWatcher::start_with_dirs(&[], &echo_agent_home, Some(project))
                .expect("parent-only watch must start when no discovery roots exist yet");
        assert!(
            path_set_contains(&watcher.refreshed_dirs, project),
            "successful project parent watch must be retained"
        );
        assert_eq!(
            watcher.refresh_dirs,
            expected_missing_vendor_refresh_seeds(project)
        );
    }

    #[test]
    fn plan_skills_watch_targets_scopes_vendors_and_seeds_refresh() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path();
        let echo_agent_home = project.join("home-echo-agent");
        let project_claude = project.join(".claude");
        let project_echo_agent = project.join(".echo-agent");
        let custom = project.join("my-skills");
        fs::create_dir_all(&project_claude).unwrap();
        fs::create_dir_all(&project_echo_agent).unwrap();
        fs::create_dir_all(&custom).unwrap();

        let dirs = vec![
            project_claude.clone(),
            project_echo_agent.clone(),
            custom.clone(),
        ];
        let plan = plan_skills_watch_targets(&dirs, &echo_agent_home, Some(project));

        assert_eq!(
            plan.vendor_roots,
            vec![project_claude.clone(), project_echo_agent.clone()]
        );
        assert_eq!(plan.recursive_roots, vec![custom]);
        assert_eq!(plan.project_parent_watch.as_deref(), Some(project));

        let mut expected_refresh: Vec<(PathBuf, RecursiveMode)> =
            vendor_skill_refresh_dirs(&project_claude)
                .into_iter()
                .chain(vendor_skill_refresh_dirs(&project_echo_agent))
                .collect();
        for name in [".agents", ".cursor"] {
            let root = project.join(name);
            expected_refresh.push((root.clone(), RecursiveMode::NonRecursive));
            expected_refresh.extend(vendor_skill_refresh_dirs(&root));
        }
        assert_eq!(plan.refresh_dirs, expected_refresh);
    }

    #[test]
    fn plan_skills_watch_targets_multi_vendor_refresh_fanout() {
        let echo_agent_home = PathBuf::from("/home/u/.echo-agent");
        let a = PathBuf::from("/repo/.claude");
        let b = PathBuf::from("/repo/.agents");
        let plan = plan_skills_watch_targets(&[a.clone(), b.clone()], &echo_agent_home, None);

        assert_eq!(plan.vendor_roots, vec![a.clone(), b.clone()]);
        assert!(plan.recursive_roots.is_empty());
        assert_eq!(
            plan.refresh_dirs,
            vendor_skill_refresh_dirs(&a)
                .into_iter()
                .chain(vendor_skill_refresh_dirs(&b))
                .collect::<Vec<_>>()
        );
        for root in [&a, &b] {
            for (dir, mode) in vendor_skill_refresh_dirs(root) {
                assert!(
                    plan.refresh_dirs.contains(&(dir, mode)),
                    "missing refresh seed for {}",
                    root.display()
                );
            }
        }
    }

    #[test]
    fn plan_skills_watch_targets_seeds_all_missing_project_vendor_roots() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path();
        let echo_agent_home = project.join("elsewhere").join(".echo-agent");
        let plan = plan_skills_watch_targets(&[], &echo_agent_home, Some(project));

        assert_eq!(plan.project_parent_watch.as_deref(), Some(project));
        assert!(plan.vendor_roots.is_empty());
        assert!(plan.recursive_roots.is_empty());
        assert_eq!(
            plan.refresh_dirs,
            expected_missing_vendor_refresh_seeds(project)
        );
        for name in VENDOR_CONFIG_ROOT_NAMES {
            let root = project.join(name);
            assert!(
                plan.refresh_dirs
                    .contains(&(root.clone(), RecursiveMode::NonRecursive)),
                "missing root seed for {name}"
            );
            for (dir, mode) in vendor_skill_refresh_dirs(&root) {
                assert!(
                    plan.refresh_dirs.contains(&(dir, mode)),
                    "missing subdir seed under {name}"
                );
            }
        }
    }

    #[test]
    fn plan_skills_watch_targets_does_not_double_seed_present_vendor_roots() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path();
        let echo_agent_home = project.join("home-echo-agent");
        let present: Vec<PathBuf> = VENDOR_CONFIG_ROOT_NAMES
            .iter()
            .map(|name| project.join(name))
            .collect();
        for root in &present {
            fs::create_dir_all(root).unwrap();
        }

        let plan = plan_skills_watch_targets(&present, &echo_agent_home, Some(project));

        assert_eq!(plan.vendor_roots, present);
        assert!(plan.project_parent_watch.is_none());

        let mut expected = Vec::new();
        for root in &present {
            expected.extend(vendor_skill_refresh_dirs(root));
        }
        assert_eq!(plan.refresh_dirs, expected);
        for root in &present {
            assert!(
                !plan
                    .refresh_dirs
                    .contains(&(root.clone(), RecursiveMode::NonRecursive)),
                "present vendor root {} must not be refresh-seeded",
                root.display()
            );
            assert_eq!(
                plan.refresh_dirs
                    .iter()
                    .filter(|(dir, _)| dir == root || dir.starts_with(root))
                    .count(),
                vendor_skill_refresh_dirs(root).len()
            );
        }
    }

    #[test]
    fn plan_skills_watch_targets_partial_vendors_seed_only_missing() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path();
        let echo_agent_home = project.join("home-echo-agent");
        let project_claude = project.join(".claude");
        fs::create_dir_all(&project_claude).unwrap();

        let plan = plan_skills_watch_targets(
            std::slice::from_ref(&project_claude),
            &echo_agent_home,
            Some(project),
        );

        assert_eq!(plan.vendor_roots, vec![project_claude.clone()]);
        assert_eq!(plan.project_parent_watch.as_deref(), Some(project));

        let mut expected = vendor_skill_refresh_dirs(&project_claude).to_vec();
        for name in [".echo-agent", ".agents", ".cursor"] {
            let root = project.join(name);
            expected.push((root.clone(), RecursiveMode::NonRecursive));
            expected.extend(vendor_skill_refresh_dirs(&root));
        }
        assert_eq!(plan.refresh_dirs, expected);
        assert!(
            !plan
                .refresh_dirs
                .contains(&(project_claude.clone(), RecursiveMode::NonRecursive))
        );
    }

    #[test]
    fn plan_skills_watch_targets_parent_watches_project_when_echo_agent_present_siblings_missing() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path();
        let echo_agent_home = project.join("home-echo-agent");
        let project_echo_agent = project.join(".echo-agent");
        fs::create_dir_all(&project_echo_agent).unwrap();

        let plan = plan_skills_watch_targets(
            std::slice::from_ref(&project_echo_agent),
            &echo_agent_home,
            Some(project),
        );

        assert_eq!(plan.vendor_roots, vec![project_echo_agent.clone()]);
        assert_eq!(plan.project_parent_watch.as_deref(), Some(project));
        assert!(
            !plan
                .refresh_dirs
                .contains(&(project_echo_agent.clone(), RecursiveMode::NonRecursive))
        );
        for name in [".agents", ".claude", ".cursor"] {
            let root = project.join(name);
            assert!(
                plan.refresh_dirs
                    .contains(&(root.clone(), RecursiveMode::NonRecursive)),
                "missing root seed for {name}"
            );
            for (dir, mode) in vendor_skill_refresh_dirs(&root) {
                assert!(
                    plan.refresh_dirs.contains(&(dir, mode)),
                    "missing subdir seed under {name}"
                );
            }
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn watch_skill_subdirs_ignores_worktrees_under_scoped_root() {
        let tmp = TempDir::new().unwrap();
        let global = tmp.path();

        let alpha = global.join("skills").join("alpha");
        fs::create_dir_all(&alpha).unwrap();
        fs::write(alpha.join("SKILL.md"), "# alpha").unwrap();

        let wt_skill = global
            .join("worktrees")
            .join("wt1")
            .join(".echo-agent")
            .join("skills")
            .join("beta");
        fs::create_dir_all(&wt_skill).unwrap();
        fs::write(wt_skill.join("SKILL.md"), "# beta").unwrap();

        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut debouncer = new_filtered_debouncer(
            Duration::from_millis(50),
            move |res: DebounceEventResult| {
                let Ok(events) = res else { return };
                if events
                    .iter()
                    .any(|event| discovery_change_for_path(&event.path).is_some())
                {
                    let _ = tx.send(());
                }
            },
        )
        .expect("debouncer should build");

        let watched = watch_skill_subdirs(&mut debouncer, global);
        assert!(watched.contains(&global.join("skills")));
        wait_ms(150);
        while rx.try_recv().is_ok() {}

        fs::write(wt_skill.join("SKILL.md"), "# beta v2").unwrap();
        wait_ms(250);
        assert!(
            rx.try_recv().is_err(),
            "changes under worktrees/ must not fire under scoped watches"
        );

        fs::write(alpha.join("SKILL.md"), "# alpha v2").unwrap();
        wait_ms(250);
        assert!(rx.try_recv().is_ok(), "changes under skills/ must fire");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn watch_skill_subdirs_scopes_project_claude_not_worktrees() {
        let tmp = TempDir::new().unwrap();
        let project_claude = tmp.path().join(".claude");

        let alpha = project_claude.join("skills").join("alpha");
        fs::create_dir_all(&alpha).unwrap();
        fs::write(alpha.join("SKILL.md"), "# alpha").unwrap();

        let wt_skill = project_claude
            .join("worktrees")
            .join("wt1")
            .join("bazel-out")
            .join("deep")
            .join("SKILL.md");
        fs::create_dir_all(wt_skill.parent().unwrap()).unwrap();
        fs::write(&wt_skill, "# noise").unwrap();

        assert!(is_vendor_config_root(
            &project_claude,
            &tmp.path().join(".echo-agent")
        ));

        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut debouncer = new_filtered_debouncer(
            Duration::from_millis(50),
            move |res: DebounceEventResult| {
                let Ok(events) = res else { return };
                if events
                    .iter()
                    .any(|event| discovery_change_for_path(&event.path).is_some())
                {
                    let _ = tx.send(());
                }
            },
        )
        .expect("debouncer should build");

        let watched = watch_skill_subdirs(&mut debouncer, &project_claude);
        assert!(watched.contains(&project_claude.join("skills")));
        wait_ms(150);
        while rx.try_recv().is_ok() {}

        fs::write(&wt_skill, "# noise v2").unwrap();
        wait_ms(250);
        assert!(
            rx.try_recv().is_err(),
            "changes under project .claude/worktrees must not fire"
        );

        fs::write(alpha.join("SKILL.md"), "# alpha v2").unwrap();
        wait_ms(250);
        assert!(
            rx.try_recv().is_ok(),
            "changes under project .claude/skills must fire"
        );
    }

    #[test]
    fn workflow_change_classifies_missing_directory_creation() {
        let echoagent = Path::new("/tmp/project/.echo-agent");
        assert_eq!(
            discovery_change_for_path(echoagent),
            Some(DiscoveryChange::Skills)
        );
        assert_eq!(
            discovery_change_for_path(Path::new("/tmp/project/.claude")),
            Some(DiscoveryChange::Skills)
        );
        assert_eq!(
            discovery_change_for_path(&echoagent.join("skills")),
            Some(DiscoveryChange::Skills)
        );
        assert_eq!(
            discovery_change_for_path(&echoagent.join("commands")),
            Some(DiscoveryChange::Skills)
        );
        assert_eq!(
            discovery_change_for_path(&echoagent.join("workflows")),
            Some(DiscoveryChange::Workflows)
        );
        assert_eq!(
            discovery_change_for_path(&echoagent.join("workflows/review.rhai")),
            Some(DiscoveryChange::Workflows)
        );
        assert_eq!(
            discovery_change_for_path(&echoagent.join("skills/review/SKILL.md")),
            Some(DiscoveryChange::Skills)
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn refresh_new_discovery_dirs_attaches_first_created_workflows_dir() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let root_for_handler = root.to_path_buf();
        let mut debouncer = new_filtered_debouncer(
            Duration::from_millis(50),
            move |result: DebounceEventResult| {
                let Ok(events) = result else { return };
                if events
                    .iter()
                    .any(|event| event.path.starts_with(&root_for_handler))
                {
                    let _ = tx.send(());
                }
            },
        )
        .unwrap();
        debouncer
            .watcher()
            .watch(root, RecursiveMode::NonRecursive)
            .unwrap();
        let workflows = root.join("workflows");
        let mut watcher = SkillsFileWatcher {
            debouncer,
            refresh_dirs: vec![(workflows.clone(), RecursiveMode::NonRecursive)],
            refreshed_dirs: HashSet::new(),
        };
        fs::create_dir(&workflows).unwrap();
        wait_ms(150);
        assert!(
            rx.try_recv().is_ok(),
            "parent watch sees first directory creation"
        );
        assert!(watcher.refresh_new_discovery_dirs());
        assert!(watcher.refreshed_dirs.contains(&workflows));
    }

    #[test]
    fn refresh_new_discovery_dirs_attaches_existing_after_mkdir() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let workflows = root.join("workflows");
        let mut debouncer = new_filtered_debouncer(Duration::from_millis(50), |_| {}).unwrap();
        debouncer
            .watcher()
            .watch(root, RecursiveMode::NonRecursive)
            .unwrap();
        let mut watcher = SkillsFileWatcher {
            debouncer,
            refresh_dirs: vec![(workflows.clone(), RecursiveMode::NonRecursive)],
            refreshed_dirs: HashSet::new(),
        };
        assert!(!watcher.refresh_new_discovery_dirs());
        fs::create_dir(&workflows).unwrap();
        assert!(watcher.refresh_new_discovery_dirs());
        assert!(watcher.refreshed_dirs.contains(&workflows));
        assert!(!watcher.refresh_new_discovery_dirs());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn refresh_new_discovery_dirs_attaches_skills_and_commands() {
        let tmp = TempDir::new().unwrap();
        let vendor = tmp.path().join(".claude");
        fs::create_dir(&vendor).unwrap();

        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut debouncer = new_filtered_debouncer(
            Duration::from_millis(50),
            move |result: DebounceEventResult| {
                let Ok(events) = result else { return };
                if events
                    .iter()
                    .any(|event| discovery_change_for_path(&event.path).is_some())
                {
                    let _ = tx.send(());
                }
            },
        )
        .unwrap();
        debouncer
            .watcher()
            .watch(&vendor, RecursiveMode::NonRecursive)
            .unwrap();

        let skills = vendor.join("skills");
        let commands = vendor.join("commands");
        let mut watcher = SkillsFileWatcher {
            debouncer,
            refresh_dirs: vendor_skill_refresh_dirs(&vendor).to_vec(),
            refreshed_dirs: HashSet::new(),
        };

        fs::create_dir_all(skills.join("alpha")).unwrap();
        wait_ms(150);
        assert!(rx.try_recv().is_ok(), "must see skills/ creation");
        assert!(watcher.refresh_new_discovery_dirs());
        assert!(watcher.refreshed_dirs.contains(&skills));
        while rx.try_recv().is_ok() {}

        fs::write(skills.join("alpha").join("SKILL.md"), "# alpha").unwrap();
        wait_ms(250);
        assert!(
            rx.try_recv().is_ok(),
            "SKILL.md under newly created skills/ must fire"
        );
        while rx.try_recv().is_ok() {}

        fs::create_dir(&commands).unwrap();
        wait_ms(150);
        assert!(rx.try_recv().is_ok(), "must see commands/ creation");
        assert!(watcher.refresh_new_discovery_dirs());
        assert!(watcher.refreshed_dirs.contains(&commands));
        while rx.try_recv().is_ok() {}

        fs::write(commands.join("foo.md"), "# foo").unwrap();
        wait_ms(250);
        assert!(
            rx.try_recv().is_ok(),
            "command md under newly created commands/ must fire"
        );
    }

    #[test]
    fn watcher_detects_auth_json_change() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("auth.json"), "{}").unwrap();

        let (_w, mut rx) =
            ConfigFileWatcher::start(tmp.path(), &[], None, Some(Duration::from_millis(50)))
                .expect("watcher should start");

        fs::write(tmp.path().join("auth.json"), r#"{"new":"token"}"#).unwrap();
        expect_change(&mut rx, ConfigChangeEvent::AuthChanged);
    }

    /// Regression test for the MCP/skills reload storm (feedback loop):
    /// merely *reading* a watched config file must NOT produce a
    /// `ConfigChangeEvent`. Linux inotify delivers `IN_OPEN`/`IN_ACCESS`
    /// for reads, `notify` subscribes to `OPEN`, and `notify-debouncer-mini`
    /// forwards every kind — so without [`AccessFilteredWatcher`] each
    /// leader-initiated reload's own re-reads of `config.toml` would
    /// schedule the next debounce tick and re-fire forever. A write
    /// afterwards must still be detected (the filter only drops `Access`).
    #[test]
    #[cfg(target_os = "linux")]
    fn watcher_ignores_reads_of_watched_files() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("config.toml"), "a = 1").unwrap();
        fs::write(tmp.path().join("auth.json"), "{}").unwrap();

        let (_w, mut rx) =
            ConfigFileWatcher::start(tmp.path(), &[], None, Some(Duration::from_millis(50)))
                .expect("watcher should start");
        wait_ms(150);
        while rx.try_recv().is_ok() {} // drain any startup noise

        // Simulate what the leader does on every reload: read the watched
        // files. Repeatedly, to defeat any incidental coalescing.
        for _ in 0..5 {
            let _ = fs::read(tmp.path().join("config.toml")).unwrap();
            let _ = fs::read(tmp.path().join("auth.json")).unwrap();
            wait_ms(20);
        }
        wait_ms(300);

        let mut read_events = Vec::new();
        while let Ok(evt) = rx.try_recv() {
            read_events.push(evt);
        }
        assert!(
            read_events.is_empty(),
            "reads of watched files must not emit config-change events \
             (reload-storm feedback loop); got {read_events:?}"
        );

        // Sanity: a real write is still observed through the filter.
        fs::write(tmp.path().join("config.toml"), "a = 2").unwrap();
        wait_ms(300);
        let mut found = false;
        while let Ok(evt) = rx.try_recv() {
            if evt == ConfigChangeEvent::GlobalConfigChanged {
                found = true;
            }
        }
        assert!(found, "a write must still be detected after read filtering");
    }

    #[test]
    fn watcher_detects_config_toml_change() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("config.toml"), "").unwrap();

        let (_w, mut rx) =
            ConfigFileWatcher::start(tmp.path(), &[], None, Some(Duration::from_millis(50)))
                .expect("watcher should start");

        fs::write(tmp.path().join("config.toml"), "[ui]\ntheme = \"dark\"").unwrap();
        expect_change(&mut rx, ConfigChangeEvent::GlobalConfigChanged);
    }

    /// A write to `<echo_agent_home>/models_cache.json` must surface as
    /// `ConfigChangeEvent::ModelsCacheChanged` so a long-running leader can
    /// hot-load a catalog fetched by another echoagent process.
    #[test]
    fn watcher_detects_models_cache_change() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("models_cache.json"), "{}").unwrap();

        let (_w, mut rx) =
            ConfigFileWatcher::start(tmp.path(), &[], None, Some(Duration::from_millis(50)))
                .expect("watcher should start");

        fs::write(
            tmp.path().join("models_cache.json"),
            r#"{"fetched_at":"2026-01-01T00:00:00Z","models":{}}"#,
        )
        .unwrap();
        expect_change(&mut rx, ConfigChangeEvent::ModelsCacheChanged);
    }

    #[test]
    #[ignore = "flaky on CI: OS file watcher may fail to initialize"]
    fn watcher_ignores_unrelated_files() {
        let tmp = TempDir::new().unwrap();

        let (_w, mut rx) =
            ConfigFileWatcher::start(tmp.path(), &[], None, Some(Duration::from_millis(50)))
                .expect("watcher should start");

        fs::write(tmp.path().join("leader.log"), "log line").unwrap();
        fs::write(tmp.path().join("leader.lock"), "12345").unwrap();
        wait_ms(300);

        assert!(
            rx.try_recv().is_err(),
            "should not emit events for unrelated files"
        );
    }

    #[test]
    fn watcher_debounces_rapid_writes() {
        let tmp = TempDir::new().unwrap();

        // Use a long debounce (500ms) so all rapid writes (50ms total)
        // land in a single debounce window regardless of platform.
        let (_w, mut rx) =
            ConfigFileWatcher::start(tmp.path(), &[], None, Some(Duration::from_millis(500)))
                .expect("watcher should start");

        wait_ms(200);

        // 5 rapid writes — total ~50ms, well within the 500ms debounce window
        for i in 0..5 {
            fs::write(tmp.path().join("config.toml"), format!("version = {i}")).unwrap();
            wait_ms(10);
        }
        // Wait for the single debounce tick to fire
        wait_ms(800);

        let mut count = 0;
        while rx.try_recv().is_ok() {
            count += 1;
        }
        // All writes should coalesce into a small number of events
        // (1 per debounce tick, or a few if OS delivers events in
        // separate batches within the window).
        assert!(count >= 1, "expected at least 1 event, got {count}");
        assert!(count <= 3, "expected coalesced events (<=3), got {count}");
    }

    /// A write to `<cwd>/.echo-agent/config.toml` must surface as
    /// a `ConfigChangeEvent::ProjectConfigChanged` so the reloader emits
    /// `ConfigUpdate::ProjectMcpServersChanged { cwd }`. Uses a longer
    /// debounce and explicit poll loop so it survives the slower-than-
    /// usual FSEvents delivery on macOS CI.
    #[test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "flaky on macOS: FSEvents does not reliably deliver events in test harness"
    )]
    fn project_cwd_toml_triggers_reload() {
        let echo_agent_home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let project_echo_agent = cwd.path().join(".echo-agent");
        fs::create_dir_all(&project_echo_agent).unwrap();
        // Seed the file before the watcher starts so we observe the
        // modification rather than the creation event.
        fs::write(project_echo_agent.join("config.toml"), "").unwrap();

        let (_w, mut rx) = ConfigFileWatcher::start(
            echo_agent_home.path(),
            &[],
            Some(cwd.path()),
            Some(Duration::from_millis(100)),
        )
        .expect("watcher should start");

        fs::write(
            project_echo_agent.join("config.toml"),
            "[mcp_servers.x]\ncommand = \"/bin/true\"",
        )
        .unwrap();

        // Poll up to 2s for the event.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut found = false;
        while std::time::Instant::now() < deadline {
            if let Ok(evt) = rx.try_recv()
                && matches!(evt, ConfigChangeEvent::ProjectConfigChanged { .. })
            {
                found = true;
                break;
            }
            wait_ms(50);
        }
        assert!(
            found,
            "expected ProjectConfigChanged for <cwd>/.echo-agent/config.toml within 2s"
        );
    }

    /// A write to `<cwd>/.mcp.json` must surface as a
    /// `ConfigChangeEvent::McpConfigChanged` so the reloader can fan out
    /// a `ProjectMcpServersChanged { cwd }`. Same FSEvents caveat as
    /// [`project_cwd_toml_triggers_reload`].
    #[test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "flaky on macOS: FSEvents does not reliably deliver events in test harness"
    )]
    fn project_mcp_json_triggers_reload() {
        let echo_agent_home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        fs::write(cwd.path().join(".mcp.json"), "{}").unwrap();

        let (_w, mut rx) = ConfigFileWatcher::start(
            echo_agent_home.path(),
            &[],
            Some(cwd.path()),
            Some(Duration::from_millis(100)),
        )
        .expect("watcher should start");

        fs::write(
            cwd.path().join(".mcp.json"),
            r#"{"mcpServers": {"x": {"command": "/bin/true"}}}"#,
        )
        .unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut found = false;
        while std::time::Instant::now() < deadline {
            if let Ok(evt) = rx.try_recv()
                && matches!(evt, ConfigChangeEvent::McpConfigChanged { .. })
            {
                found = true;
                break;
            }
            wait_ms(50);
        }
        assert!(
            found,
            "expected McpConfigChanged for <cwd>/.mcp.json within 2s"
        );
    }

    /// The cwd watch is **non-recursive** by design. This writes a
    /// file that the watcher's name filter **would** route
    /// (`.mcp.json`) into a deeply nested subdir. If a future
    /// regression flips `RecursiveMode::NonRecursive` → `Recursive`,
    /// recursive notify would surface the write, the name filter would
    /// map it to `McpConfigChanged`, and the test would fail. The file
    /// name must match the filter (`.mcp.json`, not e.g. `file.txt`)
    /// or the filter drops it regardless of recursion mode, so this is
    /// the test that actually guards the constraint.
    #[test]
    #[ignore = "flaky on CI: OS file watcher may fail to initialize"]
    fn nested_subdir_change_does_not_trigger() {
        let echo_agent_home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let nested = cwd.path().join("some").join("deep").join("nested");
        fs::create_dir_all(&nested).unwrap();

        let (_w, mut rx) = ConfigFileWatcher::start(
            echo_agent_home.path(),
            &[],
            Some(cwd.path()),
            Some(Duration::from_millis(100)),
        )
        .expect("watcher should start");

        // Write a file whose name DOES match the watcher filter —
        // under recursive mode this would surface
        // as a `ConfigChangeEvent`; under non-recursive mode no
        // event must reach `rx`.
        fs::write(
            nested.join(".mcp.json"),
            r#"{"mcpServers": {"x": {"command": "/bin/true"}}}"#,
        )
        .unwrap();
        wait_ms(500);

        assert!(
            rx.try_recv().is_err(),
            "non-recursive watch must not surface .mcp.json events from <cwd>/some/deep/nested/"
        );
    }

    /// [`ConfigFileWatcher::watch_path`] registered after
    /// `start` must light up `<new_cwd>/.echo-agent/config.toml` writes
    /// identically to a cwd passed in at `start`. Exercises the
    /// session-open registration path where the leader learns about a
    /// new project root after the watcher is already running.
    #[test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "flaky on macOS: FSEvents does not reliably deliver events in test harness"
    )]
    fn watch_path_dynamic_registration() {
        let echo_agent_home = TempDir::new().unwrap();
        let new_cwd = TempDir::new().unwrap();
        let project_echo_agent = new_cwd.path().join(".echo-agent");
        fs::create_dir_all(&project_echo_agent).unwrap();
        fs::write(project_echo_agent.join("config.toml"), "").unwrap();

        let (mut watcher, mut rx) = ConfigFileWatcher::start(
            echo_agent_home.path(),
            &[],
            None,
            Some(Duration::from_millis(100)),
        )
        .expect("watcher should start");

        watcher.watch_path(new_cwd.path());

        fs::write(
            project_echo_agent.join("config.toml"),
            "[mcp_servers.y]\ncommand = \"/bin/true\"",
        )
        .unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut found = false;
        while std::time::Instant::now() < deadline {
            if let Ok(evt) = rx.try_recv()
                && matches!(evt, ConfigChangeEvent::ProjectConfigChanged { .. })
            {
                found = true;
                break;
            }
            wait_ms(50);
        }
        assert!(
            found,
            "watch_path-registered cwd must surface ProjectConfigChanged within 2s"
        );
    }

    /// Bookkeeping-only (no OS event delivery, so deterministic on
    /// every platform): `watch_path` records the cwd in `watched_cwds`
    /// and is idempotent; `unwatch_path` removes it and is a no-op for
    /// an unknown cwd. Guards the set that backs `unwatch_path` and the
    /// `watch_path` de-dup.
    #[test]
    fn watch_and_unwatch_path_bookkeeping() {
        let echo_agent_home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let Some((mut watcher, _rx)) = ConfigFileWatcher::start(
            echo_agent_home.path(),
            &[],
            None,
            Some(Duration::from_millis(100)),
        ) else {
            // OS watcher unavailable in this environment; nothing to assert.
            return;
        };
        let p = cwd.path();
        assert!(!watcher.watched_cwds.contains(p));

        watcher.watch_path(p);
        assert!(watcher.watched_cwds.contains(p));

        // Idempotent: a second registration doesn't duplicate the entry.
        watcher.watch_path(p);
        assert_eq!(
            watcher
                .watched_cwds
                .iter()
                .filter(|c| c.as_path() == p)
                .count(),
            1,
        );

        // Unwatch removes it; a second unwatch is a no-op.
        watcher.unwatch_path(p);
        assert!(!watcher.watched_cwds.contains(p));
        watcher.unwatch_path(p);
        assert!(!watcher.watched_cwds.contains(p));
    }
}
