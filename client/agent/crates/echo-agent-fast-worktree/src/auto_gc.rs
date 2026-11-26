//! Throttled automatic worktree GC (feature `metadata`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::Result;

use crate::CleanupReport;
use crate::api::gc::{GcOptions, GcReport, age_path_enabled, gc_worktrees};
use crate::db::{ListFilter, WorktreeDb, WorktreeKind, now_epoch_secs, resolve_echo_agent_home};
use crate::discovery::{RebuildReport, rebuild_worktree_db};

pub(crate) const META_LAST_AUTO_GC_AT: &str = "last_auto_gc_at";
/// Independent throttle stamp for optional DB rebuild (not shared with GC).
pub(crate) const META_LAST_AUTO_REBUILD_AT: &str = "last_auto_rebuild_at";

/// `0` / `false` / `off` / empty disables auto-GC.
pub const ENV_AUTO_GC: &str = "ECHO_AGENT_WORKTREE_AUTO_GC";
/// `1` / `true` / `on` forces age-count without delete.
pub const ENV_AUTO_GC_DRY_RUN: &str = "ECHO_AGENT_WORKTREE_AUTO_GC_DRY_RUN";
/// Default max age in seconds (overrides TOML/remote when set and parseable).
pub const ENV_AUTO_GC_MAX_AGE: &str = "ECHO_AGENT_WORKTREE_AUTO_GC_MAX_AGE";
/// `1` / `true` / `on` enables optional discovery rebuild + stale git prune.
pub const ENV_AUTO_GC_REBUILD: &str = "ECHO_AGENT_WORKTREE_AUTO_GC_REBUILD";

/// Remove every `ECHO_AGENT_WORKTREE_AUTO_GC*` env var so a test starts from a clean
/// slate. Exposed (not `cfg(test)`) so other crates' tests can share the single
/// source of truth for the var list; not intended for production use.
///
/// # Safety
/// `remove_var` is unsound under concurrent environment access. The caller must
/// hold its env test lock and run no other thread that touches the environment.
#[doc(hidden)]
pub unsafe fn clear_auto_gc_env_for_test() {
    unsafe {
        std::env::remove_var(ENV_AUTO_GC);
        std::env::remove_var(ENV_AUTO_GC_DRY_RUN);
        std::env::remove_var(ENV_AUTO_GC_MAX_AGE);
        std::env::remove_var(ENV_AUTO_GC_REBUILD);
    }
}

pub(crate) const DEFAULT_MAX_AGE_SECS: i64 = 7 * 86400;
pub(crate) const DEFAULT_MIN_INTERVAL_SECS: i64 = 6 * 3600;
/// Rebuild is costlier than GC; default 24h until cost is measured in dogfood.
pub(crate) const DEFAULT_REBUILD_MIN_INTERVAL_SECS: i64 = 24 * 3600;

pub(crate) const MAX_AGE_SECS_MIN: i64 = 3600;
pub(crate) const MAX_AGE_SECS_MAX: i64 = 90 * 86400;
pub(crate) const MIN_INTERVAL_SECS_MIN: i64 = 60;
pub(crate) const MIN_INTERVAL_SECS_MAX: i64 = 7 * 86400;

/// Product default: Manual never age-expires unless config overrides.
pub(crate) fn default_max_age_by_kind() -> BTreeMap<WorktreeKind, Option<i64>> {
    BTreeMap::from([(WorktreeKind::Manual, None)])
}

/// Compile-time CWD-scan platforms (Linux/macOS). Runtime failure fail-closes in `gc_worktrees`.
pub(crate) fn process_cwd_scan_available() -> bool {
    cfg!(any(target_os = "linux", target_os = "macos"))
}

/// Real age-expiry when a CWD-scan platform, or dry-run metrics without deletes.
pub(crate) fn age_expiry_allowed(scan_platform: bool, dry_run: bool) -> bool {
    scan_platform || dry_run
}

/// One local/remote config layer (`max_age_by_kind`: `Some(secs)` or `None`=never).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorktreeAutoGcLayer {
    pub enabled: Option<bool>,
    pub max_age_secs: Option<u64>,
    pub min_interval_secs: Option<u64>,
    pub dry_run: Option<bool>,
    pub include_orphan_snapshots: Option<bool>,
    pub max_age_by_kind: BTreeMap<WorktreeKind, Option<u64>>,
    /// Optional discovery rebuild + echo-agent-scoped stale `.git/worktrees/` scrub (default off).
    pub include_rebuild: Option<bool>,
    /// Independent rebuild throttle; absent ⇒ 24h.
    pub rebuild_min_interval_secs: Option<u64>,
}

/// Policy after env / TOML / remote merge; the runtime options `maybe_auto_gc`
/// consumes. The env kill-switch, dry-run, and rebuild flags are re-applied
/// inside `maybe_auto_gc`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedWorktreeAutoGc {
    pub enabled: bool,
    pub max_age_secs: i64,
    pub min_interval_secs: i64,
    pub dry_run: bool,
    pub include_orphan_snapshots: bool,
    pub max_age_by_kind: BTreeMap<WorktreeKind, Option<i64>>,
    /// When true, rebuild the DB from disk and prune stale git registrations.
    /// Off by default until rebuild cost is measured.
    pub include_rebuild: bool,
    pub rebuild_min_interval_secs: i64,
}

impl Default for ResolvedWorktreeAutoGc {
    fn default() -> Self {
        Self {
            enabled: true,
            max_age_secs: DEFAULT_MAX_AGE_SECS,
            min_interval_secs: DEFAULT_MIN_INTERVAL_SECS,
            dry_run: false,
            include_orphan_snapshots: cfg!(target_os = "linux"),
            max_age_by_kind: default_max_age_by_kind(),
            include_rebuild: false,
            rebuild_min_interval_secs: DEFAULT_REBUILD_MIN_INTERVAL_SECS,
        }
    }
}

fn clamp_kind_age(secs: Option<u64>) -> Option<i64> {
    secs.map(clamp_max_age_secs)
}

/// Merge kind maps: product default Manual=never, then remote, then local.
fn merge_max_age_by_kind(
    local: Option<&BTreeMap<WorktreeKind, Option<u64>>>,
    remote: Option<&BTreeMap<WorktreeKind, Option<u64>>>,
) -> BTreeMap<WorktreeKind, Option<i64>> {
    let mut map = default_max_age_by_kind();
    if let Some(r) = remote {
        for (&k, &v) in r {
            map.insert(k, clamp_kind_age(v));
        }
    }
    if let Some(l) = local {
        for (&k, &v) in l {
            map.insert(k, clamp_kind_age(v));
        }
    }
    map
}

/// Precedence: env > local > remote > defaults (with numeric clamps).
pub fn resolve_worktree_auto_gc_from_layers(
    local: Option<&WorktreeAutoGcLayer>,
    remote: Option<&WorktreeAutoGcLayer>,
) -> ResolvedWorktreeAutoGc {
    let enabled = if env_auto_gc_disabled() {
        false
    } else {
        local
            .and_then(|s| s.enabled)
            .or(remote.and_then(|s| s.enabled))
            .unwrap_or(true)
    };

    let max_age_secs = env_auto_gc_max_age()
        .or(local.and_then(|s| s.max_age_secs))
        .or(remote.and_then(|s| s.max_age_secs))
        .map(clamp_max_age_secs)
        .unwrap_or(DEFAULT_MAX_AGE_SECS);

    let min_interval_secs = local
        .and_then(|s| s.min_interval_secs)
        .or(remote.and_then(|s| s.min_interval_secs))
        .map(clamp_min_interval_secs)
        .unwrap_or(DEFAULT_MIN_INTERVAL_SECS);

    let dry_run = if env_auto_gc_dry_run() {
        true
    } else {
        local
            .and_then(|s| s.dry_run)
            .or(remote.and_then(|s| s.dry_run))
            .unwrap_or(false)
    };

    let include_orphan_snapshots = local
        .and_then(|s| s.include_orphan_snapshots)
        .or(remote.and_then(|s| s.include_orphan_snapshots))
        .unwrap_or(cfg!(target_os = "linux"));

    // Env REBUILD=1 forces on; config cannot disable over env.
    let include_rebuild = if env_auto_gc_rebuild() {
        true
    } else {
        local
            .and_then(|s| s.include_rebuild)
            .or(remote.and_then(|s| s.include_rebuild))
            .unwrap_or(false)
    };

    let rebuild_min_interval_secs = local
        .and_then(|s| s.rebuild_min_interval_secs)
        .or(remote.and_then(|s| s.rebuild_min_interval_secs))
        .map(clamp_min_interval_secs)
        .unwrap_or(DEFAULT_REBUILD_MIN_INTERVAL_SECS);

    ResolvedWorktreeAutoGc {
        enabled,
        max_age_secs,
        min_interval_secs,
        dry_run,
        include_orphan_snapshots,
        max_age_by_kind: merge_max_age_by_kind(
            local.map(|s| &s.max_age_by_kind),
            remote.map(|s| &s.max_age_by_kind),
        ),
        include_rebuild,
        rebuild_min_interval_secs,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AutoGcOutcome {
    Disabled,
    Throttled,
    Ran,
}

#[derive(Debug)]
pub struct AutoGcReport {
    pub outcome: AutoGcOutcome,
    pub gc: Option<GcReport>,
    pub overlay: Option<CleanupReport>,
    pub btrfs: Option<CleanupReport>,
    pub age_expiry_enabled: bool,
    pub stamped: bool,
    /// Present only when a rebuild pass ran in this invocation.
    pub rebuild: Option<RebuildReport>,
    /// True when `last_auto_rebuild_at` was written this pass.
    pub rebuild_stamped: bool,
    /// Entries removed from known source repos' `.git/worktrees/` via prune.
    pub stale_registrations_cleaned: u64,
}

impl AutoGcReport {
    fn empty(outcome: AutoGcOutcome) -> Self {
        Self {
            outcome,
            gc: None,
            overlay: None,
            btrfs: None,
            age_expiry_enabled: false,
            stamped: false,
            rebuild: None,
            rebuild_stamped: false,
            stale_registrations_cleaned: 0,
        }
    }

    fn disabled() -> Self {
        Self::empty(AutoGcOutcome::Disabled)
    }

    fn throttled() -> Self {
        Self::empty(AutoGcOutcome::Throttled)
    }
}

fn env_var_truthy(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on" | "enabled"
        ),
        Err(_) => false,
    }
}

fn env_var_disabled(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off" | "disabled" | ""
        ),
        Err(_) => false,
    }
}

pub(crate) fn env_auto_gc_disabled() -> bool {
    env_var_disabled(ENV_AUTO_GC)
}

pub(crate) fn env_auto_gc_dry_run() -> bool {
    env_var_truthy(ENV_AUTO_GC_DRY_RUN)
}

pub(crate) fn env_auto_gc_rebuild() -> bool {
    env_var_truthy(ENV_AUTO_GC_REBUILD)
}

/// Parse `ECHO_AGENT_WORKTREE_AUTO_GC_MAX_AGE` as seconds; invalid/absent → None.
pub(crate) fn env_auto_gc_max_age() -> Option<u64> {
    match std::env::var(ENV_AUTO_GC_MAX_AGE) {
        Ok(v) => {
            let trimmed = v.trim();
            if trimmed.is_empty() {
                return None;
            }
            trimmed.parse::<u64>().ok()
        }
        Err(_) => None,
    }
}

pub(crate) fn clamp_max_age_secs(v: u64) -> i64 {
    i64::try_from(v)
        .unwrap_or(i64::MAX)
        .clamp(MAX_AGE_SECS_MIN, MAX_AGE_SECS_MAX)
}

pub(crate) fn clamp_min_interval_secs(v: u64) -> i64 {
    i64::try_from(v)
        .unwrap_or(i64::MAX)
        .clamp(MIN_INTERVAL_SECS_MIN, MIN_INTERVAL_SECS_MAX)
}

/// Auto-path `GcOptions` (`force=false`, kind policy map, in-use paths canon once).
/// Test-only: production builds `GcOptions` through `build_auto_gc_options_with_dry_run`.
#[cfg(test)]
pub(crate) fn build_auto_gc_options(
    auto_opts: &ResolvedWorktreeAutoGc,
    in_use: Vec<PathBuf>,
) -> GcOptions {
    build_auto_gc_options_with_dry_run(auto_opts, in_use, auto_opts.dry_run)
}

fn build_auto_gc_options_with_dry_run(
    auto_opts: &ResolvedWorktreeAutoGc,
    in_use: Vec<PathBuf>,
    dry_run: bool,
) -> GcOptions {
    let age_allowed = age_expiry_allowed(process_cwd_scan_available(), dry_run);
    let keep_worktrees_containing = in_use
        .into_iter()
        .map(|p| dunce::canonicalize(&p).unwrap_or(p))
        .collect();
    // Clone kind map only when the age path is live; platform-off drops it.
    let max_age_by_kind = if age_allowed {
        auto_opts.max_age_by_kind.clone()
    } else {
        BTreeMap::new()
    };
    GcOptions {
        max_age_secs: age_allowed.then_some(auto_opts.max_age_secs),
        force: false,
        dry_run,
        keep_worktrees_containing,
        max_age_by_kind,
    }
}

/// Future stamps (clock skew) are treated as due so throttle cannot black out forever.
pub(crate) fn is_throttled(now: i64, last: i64, min_interval_secs: i64) -> bool {
    if last > now {
        return false;
    }
    now.saturating_sub(last) < min_interval_secs.max(0)
}

/// Open the default worktree DB and run one throttled auto-GC pass, warning
/// (tagged with `context`) on failure. Callers own the opt-in/deferral decision;
/// this is the shared open-run-warn body for the startup and remote-attach hooks.
pub fn run_auto_gc_pass(policy: &ResolvedWorktreeAutoGc, context: &'static str) {
    if let Err(error) = WorktreeDb::open_default().and_then(|db| maybe_auto_gc(&db, policy)) {
        tracing::warn!(%error, context, "auto worktree gc failed");
    }
}

/// Throttled auto-GC. `Ok` always carries a report; `Err` means infrastructure
/// failure before/during GC (not stamped). Env kill/dry-run/rebuild override
/// raw options. GC throttle short-circuits the whole pass (including rebuild).
pub fn maybe_auto_gc(db: &WorktreeDb, auto_opts: &ResolvedWorktreeAutoGc) -> Result<AutoGcReport> {
    if env_auto_gc_disabled() || !auto_opts.enabled {
        tracing::debug!("auto worktree gc disabled");
        return Ok(AutoGcReport::disabled());
    }

    let dry_run = auto_opts.dry_run || env_auto_gc_dry_run();
    let include_rebuild = auto_opts.include_rebuild || env_auto_gc_rebuild();

    let now = now_epoch_secs();
    // GC meta: fail closed on read Err; unparseable fails open. Throttle skips rebuild too.
    if let Some(ts) = db.get_meta(META_LAST_AUTO_GC_AT)? {
        match ts.parse::<i64>() {
            Ok(last) if is_throttled(now, last, auto_opts.min_interval_secs) => {
                tracing::debug!(
                    last_auto_gc_at = last,
                    min_interval_secs = auto_opts.min_interval_secs,
                    "auto worktree gc throttled"
                );
                return Ok(AutoGcReport::throttled());
            }
            Ok(_) => {}
            Err(_) => {
                tracing::warn!(
                    value = %ts,
                    "auto worktree gc ignoring unparseable last_auto_gc_at; running reclaim"
                );
            }
        }
    }

    // Rebuild before the prune snapshot (so new worktrees' source repos are in
    // it) and before dead-GC (so sole-dead repos survive unregister). Meta is
    // stamped by the caller after GC succeeds, not here: a GC failure must leave
    // rebuild unthrottled so the next pass sees worktrees made in between.
    let (rebuild, rebuild_due_to_stamp) = maybe_run_rebuild(
        db,
        include_rebuild,
        dry_run,
        auto_opts.rebuild_min_interval_secs,
        now,
    );

    let prune_repos = if include_rebuild && !dry_run {
        collect_source_repos_for_prune(db)
    } else {
        BTreeSet::new()
    };

    // The current process's cwd is "in use"; never reclaim the worktree we run in.
    let mut in_use = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        in_use.push(cwd);
    }

    let gc_opts = build_auto_gc_options_with_dry_run(auto_opts, in_use, dry_run);
    let age_expiry_enabled = age_path_enabled(&gc_opts);
    debug_assert!(!gc_opts.force);

    let gc_report = match gc_worktrees(db, &gc_opts) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                error = %e,
                rebuild_ran = rebuild.is_some(),
                "auto worktree gc failed; rebuild meta left unstamped so next pass can re-discover"
            );
            return Err(e);
        }
    };

    if gc_report.remove_failed > 0 {
        tracing::warn!(
            remove_failed = gc_report.remove_failed,
            "auto worktree gc had remove failures"
        );
    }

    let (overlay, btrfs) = run_orphan_cleaners(dry_run, auto_opts.include_orphan_snapshots);

    // Scrub each full pass when opted in (cheap vs discovery; not rebuild-throttled).
    let stale_registrations_cleaned = if include_rebuild && !dry_run {
        prune_stale_git_worktree_registrations(&prune_repos)
    } else {
        0
    };

    let stamp_now = now_epoch_secs();
    // Stamp rebuild only after GC succeeds (see maybe_run_rebuild).
    let rebuild_stamped = if rebuild_due_to_stamp {
        match db.set_meta(META_LAST_AUTO_REBUILD_AT, &stamp_now.to_string()) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(error = %e, "auto worktree rebuild failed to stamp meta");
                false
            }
        }
    } else {
        false
    };
    let stamped = match db.set_meta(META_LAST_AUTO_GC_AT, &stamp_now.to_string()) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(error = %e, "auto worktree gc failed to stamp meta");
            false
        }
    };

    let overlay_errors = overlay.as_ref().map(|r| r.errors).unwrap_or(0);
    let btrfs_errors = btrfs.as_ref().map(|r| r.errors).unwrap_or(0);
    let rebuild_discovered = rebuild.as_ref().map(|r| r.discovered).unwrap_or(0);
    let rebuild_registered = rebuild.as_ref().map(|r| r.registered).unwrap_or(0);
    tracing::info!(
        age_expiry_enabled,
        dead_removed = gc_report.dead_removed,
        expired_removed = gc_report.expired_removed,
        skipped_alive = gc_report.skipped_alive,
        never_expiring = gc_report.never_expiring,
        kept_unsafe = gc_report.kept_unsafe,
        ?gc_report.kept_reasons,
        no_repo_paths = gc_report.no_repo_paths,
        not_judged = gc_report.not_judged,
        unnamed = gc_report.unnamed,
        remove_failed = gc_report.remove_failed,
        overlay_errors,
        btrfs_errors,
        rebuild_discovered,
        rebuild_registered,
        stale_registrations_cleaned,
        dry_run,
        stamped,
        rebuild_stamped,
        "auto worktree gc complete"
    );

    Ok(AutoGcReport {
        outcome: AutoGcOutcome::Ran,
        gc: Some(gc_report),
        overlay,
        btrfs,
        age_expiry_enabled,
        stamped,
        rebuild,
        rebuild_stamped,
        stale_registrations_cleaned,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RebuildMetaClass {
    Due,
    Throttled,
    /// Meta read failed: skip rebuild, do not abort GC.
    SkipFailed,
}

/// `Err` ⇒ skip rebuild only (GC continues).
fn classify_rebuild_meta(
    meta: Result<Option<String>>,
    now: i64,
    rebuild_min_interval_secs: i64,
) -> RebuildMetaClass {
    match meta {
        Err(e) => {
            tracing::warn!(
                error = %e,
                "auto worktree rebuild skipped: meta read failed; continuing GC"
            );
            RebuildMetaClass::SkipFailed
        }
        Ok(None) => RebuildMetaClass::Due,
        Ok(Some(ts)) => match ts.parse::<i64>() {
            Ok(last) if is_throttled(now, last, rebuild_min_interval_secs) => {
                tracing::debug!(
                    last_auto_rebuild_at = last,
                    rebuild_min_interval_secs,
                    "auto worktree rebuild throttled"
                );
                RebuildMetaClass::Throttled
            }
            Ok(_) => RebuildMetaClass::Due,
            Err(_) => {
                tracing::warn!(
                    value = %ts,
                    "auto worktree rebuild ignoring unparseable last_auto_rebuild_at"
                );
                RebuildMetaClass::Due
            }
        },
    }
}

/// Optional rebuild; never fails the GC pass.
///
/// Returns `(report, due_to_stamp)`. Stamp is applied by the caller **only
/// after** GC succeeds: stamping here would throttle rebuild while GC can
/// still `Err` and leave `last_auto_gc_at` unstamped.
fn maybe_run_rebuild(
    db: &WorktreeDb,
    include_rebuild: bool,
    dry_run: bool,
    rebuild_min_interval_secs: i64,
    now: i64,
) -> (Option<RebuildReport>, bool) {
    if !include_rebuild || dry_run {
        return (None, false);
    }

    match classify_rebuild_meta(
        db.get_meta(META_LAST_AUTO_REBUILD_AT),
        now,
        rebuild_min_interval_secs,
    ) {
        RebuildMetaClass::Due => {}
        RebuildMetaClass::Throttled | RebuildMetaClass::SkipFailed => return (None, false),
    }

    let home = match resolve_echo_agent_home() {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(error = %e, "auto worktree rebuild skipped: echoagent home unresolved");
            return (None, false);
        }
    };

    match rebuild_worktree_db(db, &home) {
        Ok(report) => {
            tracing::info!(
                discovered = report.discovered,
                registered = report.registered,
                already_tracked = report.already_tracked,
                "auto worktree db rebuild complete"
            );
            (Some(report), true)
        }
        Err(e) => {
            tracing::warn!(error = %e, "auto worktree rebuild failed; continuing GC");
            (None, false)
        }
    }
}

/// Distinct non-unknown `source_repo` values (alive + dead) for prune.
fn collect_source_repos_for_prune(db: &WorktreeDb) -> BTreeSet<PathBuf> {
    let filter = ListFilter {
        include_dead: true,
        ..Default::default()
    };
    let Ok(records) = db.list(&filter) else {
        tracing::warn!("auto worktree prune skipped: list failed");
        return BTreeSet::new();
    };
    records
        .into_iter()
        .filter(|r| r.source_repo.as_os_str() != "unknown")
        .map(|r| r.source_repo)
        .collect()
}

/// Scrub stale echo-agent-owned registrations from each known source repo,
/// scoped to worktrees under the echoagent home to prove ownership (see
/// [`crate::git::remove_stale_worktree_registrations_under`] for why a blanket
/// `git worktree prune` is unsafe here).
fn prune_stale_git_worktree_registrations(repos: &BTreeSet<PathBuf>) -> u64 {
    let Ok(echo_agent_home) = resolve_echo_agent_home() else {
        tracing::warn!("auto worktree registration scrub skipped: echoagent home unresolved");
        return 0;
    };
    let cleaned: u64 = repos
        .iter()
        .filter(|repo| repo.is_dir())
        .map(|repo| crate::git::remove_stale_worktree_registrations_under(repo, &echo_agent_home))
        .fold(0u64, u64::saturating_add);
    if cleaned > 0 {
        tracing::info!(
            stale_registrations_cleaned = cleaned,
            "auto worktree stale git registrations scrubbed"
        );
    }
    cleaned
}

fn run_orphan_cleaners(
    dry_run: bool,
    include_orphan_snapshots: bool,
) -> (Option<CleanupReport>, Option<CleanupReport>) {
    #[cfg(target_os = "linux")]
    {
        if dry_run || !include_orphan_snapshots {
            return (None, None);
        }
        let overlay = crate::cleanup_orphaned_overlay_snapshots();
        let btrfs = crate::cleanup_orphaned_btrfs_snapshots();
        if overlay.errors > 0 {
            tracing::warn!(
                errors = overlay.errors,
                "auto worktree gc overlay orphan cleanup had errors"
            );
        }
        if btrfs.errors > 0 {
            tracing::warn!(
                errors = btrfs.errors,
                "auto worktree gc btrfs orphan cleanup had errors"
            );
        }
        (Some(overlay), Some(btrfs))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (dry_run, include_orphan_snapshots);
        (None, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::WorktreeRecord;
    use crate::test_support::deletable_linked_worktree;
    use echo_agent_test_utils::git::{init_git_repo, run_git};
    use std::path::Path;
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn env_guard() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn clear_auto_gc_env() {
        // SAFETY: callers hold ENV_LOCK via env_guard(); no other thread touches env.
        unsafe { super::clear_auto_gc_env_for_test() };
    }

    fn make_rec(id: &str, path: PathBuf, kind: WorktreeKind, created_at: i64) -> WorktreeRecord {
        WorktreeRecord {
            kind,
            created_at,
            ..crate::test_support::worktree_record(id, path)
        }
    }

    /// Base test options: GC always due, orphan cleaners off.
    fn auto_opts() -> ResolvedWorktreeAutoGc {
        ResolvedWorktreeAutoGc {
            min_interval_secs: 0,
            include_orphan_snapshots: false,
            ..ResolvedWorktreeAutoGc::default()
        }
    }

    fn opts_enabled_no_orphans(dry_run: bool) -> ResolvedWorktreeAutoGc {
        ResolvedWorktreeAutoGc {
            dry_run,
            ..auto_opts()
        }
    }

    /// Base options for the rebuild tests: rebuild enabled and always due
    /// (`rebuild_min_interval_secs: 0`).
    fn rebuild_opts() -> ResolvedWorktreeAutoGc {
        ResolvedWorktreeAutoGc {
            include_rebuild: true,
            rebuild_min_interval_secs: 0,
            ..auto_opts()
        }
    }

    /// Git repo + stale linked-worktree registration (working tree deleted).
    fn plant_stale_git_worktree(repo: &Path, wt: &Path) {
        std::fs::create_dir_all(repo).unwrap();
        init_git_repo(repo);
        std::fs::write(repo.join("f.txt"), b"x").unwrap();
        run_git(repo, &["add", "f.txt"]);
        run_git(repo, &["commit", "-m", "i"]);
        crate::test_support::add_worktree(repo, wt);
        std::fs::remove_dir_all(wt).unwrap();
    }

    fn count_regs(repo: &Path) -> usize {
        std::fs::read_dir(repo.join(".git/worktrees"))
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .filter(|e| e.path().is_dir())
                    .count()
            })
            .unwrap_or(0)
    }

    #[test]
    fn age_expiry_allowed_table() {
        for (scan, dry_run, expected) in [
            (false, false, false),
            (false, true, true),
            (true, false, true),
            (true, true, true),
        ] {
            assert_eq!(
                age_expiry_allowed(scan, dry_run),
                expected,
                "age_expiry_allowed(scan={scan}, dry_run={dry_run})"
            );
        }
    }

    /// Builder invariants across the dry-run matrix: `force` is never set, the
    /// dry-run flag propagates, and the real age path (`max_age` + kind map) is
    /// present iff `age_expiry_allowed(scan, dry_run)` (`scan` is the
    /// compile-time platform capability).
    #[test]
    fn build_auto_gc_options_table() {
        let _g = env_guard();
        clear_auto_gc_env();
        let scan = process_cwd_scan_available();
        for dry_run in [false, true] {
            let opts = ResolvedWorktreeAutoGc {
                max_age_secs: 999,
                min_interval_secs: 1,
                include_orphan_snapshots: true,
                dry_run,
                ..auto_opts()
            };
            let gc = build_auto_gc_options(&opts, Vec::new());
            assert!(!gc.force, "auto path must never set force=true");
            assert_eq!(gc.dry_run, dry_run);
            let age_allowed = age_expiry_allowed(scan, dry_run);
            assert_eq!(
                gc.max_age_secs,
                age_allowed.then_some(999),
                "max_age set iff age path live (dry_run={dry_run})"
            );
            if age_allowed {
                assert_eq!(gc.max_age_by_kind.get(&WorktreeKind::Manual), Some(&None));
            } else {
                assert!(
                    gc.max_age_by_kind.is_empty(),
                    "kind map dropped when age path off"
                );
            }
        }
    }

    /// Real age-expiry (scan platform): an unguarded expired session is
    /// deleted while a live `creator_pid` session and a Manual tree (never
    /// age-expires by default) both survive. `force` is never applied by the
    /// auto path; the live tree would be deleted if it were.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn maybe_auto_gc_age_path_expires_unguarded_protects_live_and_manual() {
        // Age path needs a successful CWD scan; serialize with chdir tests.
        let _g = env_guard();
        let _cwd_lock = crate::api::cwd_test_guard();
        clear_auto_gc_env();
        let tmp = tempfile::TempDir::new().unwrap();
        let db = WorktreeDb::open(tmp.path()).unwrap();

        let expired = deletable_linked_worktree(tmp.path(), "expired-session");
        db.register(&make_rec(
            "expired",
            expired.clone(),
            WorktreeKind::Session,
            1,
        ))
        .unwrap();

        let live = tmp.path().join("kept-session");
        std::fs::create_dir(&live).unwrap();
        let mut live_rec = make_rec("kept", live.clone(), WorktreeKind::Session, 1);
        live_rec.creator_pid = Some(std::process::id());
        db.register(&live_rec).unwrap();

        let manual = tmp.path().join("man");
        std::fs::create_dir(&manual).unwrap();
        db.register(&make_rec("m", manual.clone(), WorktreeKind::Manual, 1))
            .unwrap();

        let report = maybe_auto_gc(
            &db,
            &ResolvedWorktreeAutoGc {
                max_age_secs: 0,
                dry_run: false,
                ..auto_opts()
            },
        )
        .unwrap();
        assert_eq!(report.outcome, AutoGcOutcome::Ran);
        assert!(report.age_expiry_enabled);
        assert!(
            !expired.exists(),
            "unguarded expired session must be age-deleted on scan platforms"
        );
        assert!(live.exists(), "live creator_pid must protect its tree");
        assert!(manual.exists(), "manual never age-expires by default");
        let gc = report.gc.as_ref().unwrap();
        assert!(gc.expired_removed >= 1);
        assert!(gc.skipped_alive >= 1);
    }

    #[test]
    fn maybe_auto_gc_protects_process_cwd() {
        // Lock order: ENV_LOCK then CWD_TEST_LOCK.
        let _g = env_guard();
        let _cwd_lock = crate::api::cwd_test_guard();
        clear_auto_gc_env();
        let tmp = tempfile::TempDir::new().unwrap();
        let db = WorktreeDb::open(tmp.path()).unwrap();
        let dir = tmp.path().join("cwd-wt");
        std::fs::create_dir(&dir).unwrap();
        db.register(&make_rec("cwd", dir.clone(), WorktreeKind::Session, 1))
            .unwrap();

        let _cwd = crate::api::CwdGuard(std::env::current_dir().unwrap());
        std::env::set_current_dir(&dir).unwrap();
        let report = maybe_auto_gc(
            &db,
            &ResolvedWorktreeAutoGc {
                max_age_secs: 0,
                dry_run: true,
                ..auto_opts()
            },
        )
        .unwrap();
        assert_eq!(report.outcome, AutoGcOutcome::Ran);
        let gc = report.gc.unwrap();
        assert_eq!(
            gc.expired_removed, 0,
            "process cwd inside wt must not count as would-expire"
        );
        assert!(
            gc.skipped_alive >= 1,
            "the in-use cwd worktree must be kept"
        );
        assert!(dir.exists());
    }

    #[test]
    fn auto_path_dead_reclaim_includes_manual_kind() {
        // never-expire is age-only; dead Manual still unregisters.
        let _g = env_guard();
        clear_auto_gc_env();
        let tmp = tempfile::TempDir::new().unwrap();
        let db = WorktreeDb::open(tmp.path()).unwrap();
        db.register(&make_rec(
            "manual-dead",
            PathBuf::from("/nonexistent/manual-wt"),
            WorktreeKind::Manual,
            100,
        ))
        .unwrap();
        let report = maybe_auto_gc(&db, &opts_enabled_no_orphans(false)).unwrap();
        assert_eq!(report.outcome, AutoGcOutcome::Ran);
        assert_eq!(report.gc.as_ref().unwrap().dead_removed, 1);
        let all = db
            .list(&ListFilter {
                include_dead: true,
                ..Default::default()
            })
            .unwrap();
        assert!(all.is_empty());
    }

    /// Orphan cleaners are gated: dry-run never invokes them (all platforms);
    /// a real pass invokes them only on Linux (compile-gated symbols),
    /// otherwise they are always absent.
    #[test]
    fn orphan_cleaners_gating() {
        let _g = env_guard();
        clear_auto_gc_env();

        let tmp = tempfile::TempDir::new().unwrap();
        let db = WorktreeDb::open(tmp.path()).unwrap();
        let dry = maybe_auto_gc(
            &db,
            &ResolvedWorktreeAutoGc {
                include_orphan_snapshots: true,
                dry_run: true,
                ..auto_opts()
            },
        )
        .unwrap();
        assert_eq!(dry.outcome, AutoGcOutcome::Ran);
        assert!(
            dry.overlay.is_none() && dry.btrfs.is_none(),
            "dry_run must not invoke orphan cleaners"
        );

        let tmp2 = tempfile::TempDir::new().unwrap();
        let db2 = WorktreeDb::open(tmp2.path()).unwrap();
        let real = maybe_auto_gc(
            &db2,
            &ResolvedWorktreeAutoGc {
                include_orphan_snapshots: true,
                dry_run: false,
                ..auto_opts()
            },
        )
        .unwrap();
        assert_eq!(real.outcome, AutoGcOutcome::Ran);
        #[cfg(target_os = "linux")]
        assert!(
            real.overlay.is_some() && real.btrfs.is_some(),
            "non-dry-run + include_orphan_snapshots must invoke cleaners on Linux"
        );
        #[cfg(not(target_os = "linux"))]
        assert!(
            real.overlay.is_none() && real.btrfs.is_none(),
            "orphan cleaners are compile-gated; non-Linux always None"
        );
    }

    /// Kill switch: env `ECHO_AGENT_WORKTREE_AUTO_GC=0` or `opts.enabled=false` both
    /// short-circuit to `Disabled` with no stamp; an enabled pass with a clean
    /// env runs and stamps.
    #[test]
    fn maybe_auto_gc_enable_disable_table() {
        let _g = env_guard();
        for (env_kill, enabled, expected, stamps) in [
            (true, true, AutoGcOutcome::Disabled, false),
            (false, false, AutoGcOutcome::Disabled, false),
            (false, true, AutoGcOutcome::Ran, true),
        ] {
            clear_auto_gc_env();
            if env_kill {
                unsafe { std::env::set_var(ENV_AUTO_GC, "0") };
            }
            let tmp = tempfile::TempDir::new().unwrap();
            let db = WorktreeDb::open(tmp.path()).unwrap();
            let report = maybe_auto_gc(
                &db,
                &ResolvedWorktreeAutoGc {
                    enabled,
                    ..opts_enabled_no_orphans(false)
                },
            )
            .unwrap();
            assert_eq!(
                report.outcome, expected,
                "env_kill={env_kill} enabled={enabled}"
            );
            assert_eq!(
                db.get_meta(META_LAST_AUTO_GC_AT).unwrap().is_some(),
                stamps,
                "stamp presence for env_kill={env_kill} enabled={enabled}"
            );
        }
        clear_auto_gc_env();
    }

    #[test]
    fn env_kill_truthy_falsy_table() {
        let _g = env_guard();
        for (val, disabled) in [
            ("0", true),
            ("false", true),
            ("FALSE", true),
            ("off", true),
            ("no", true),
            ("disabled", true),
            ("", true),
            ("1", false),
            ("true", false),
            ("on", false),
            ("yes", false),
            ("enabled", false),
        ] {
            clear_auto_gc_env();
            unsafe { std::env::set_var(ENV_AUTO_GC, val) };
            assert_eq!(
                env_auto_gc_disabled(),
                disabled,
                "ENV_AUTO_GC={val:?} disabled={disabled}"
            );
        }
        clear_auto_gc_env();
        assert!(!env_auto_gc_disabled(), "unset is not disabled");
    }

    #[test]
    fn env_dry_run_truthy_table() {
        let _g = env_guard();
        for (val, on) in [
            ("1", true),
            ("true", true),
            ("yes", true),
            ("on", true),
            ("enabled", true),
            ("0", false),
            ("false", false),
            ("", false),
            ("nope", false),
        ] {
            clear_auto_gc_env();
            unsafe { std::env::set_var(ENV_AUTO_GC_DRY_RUN, val) };
            assert_eq!(
                env_auto_gc_dry_run(),
                on,
                "ENV_AUTO_GC_DRY_RUN={val:?} on={on}"
            );
        }
        clear_auto_gc_env();
        assert!(!env_auto_gc_dry_run());
    }

    #[test]
    fn env_dry_run_forces_dry_run_on_raw_opts() {
        // Raw ResolvedWorktreeAutoGc dry_run=false must still dry-run (no
        // deletion) when the env forces it inside maybe_auto_gc.
        let _g = env_guard();
        clear_auto_gc_env();
        unsafe { std::env::set_var(ENV_AUTO_GC_DRY_RUN, "1") };
        let tmp = tempfile::TempDir::new().unwrap();
        let db = WorktreeDb::open(tmp.path()).unwrap();
        let dir = tmp.path().join("would-expire");
        std::fs::create_dir(&dir).unwrap();
        db.register(&make_rec("exp", dir.clone(), WorktreeKind::Session, 1))
            .unwrap();
        let report = maybe_auto_gc(&db, &opts_enabled_no_orphans(false)).unwrap();
        assert_eq!(report.outcome, AutoGcOutcome::Ran);
        assert!(
            dir.exists(),
            "env dry-run must not delete even when opts.dry_run=false"
        );
        assert!(report.age_expiry_enabled, "dry-run enables age metrics");
        clear_auto_gc_env();
    }

    #[test]
    fn is_throttled_logic() {
        assert!(!is_throttled(1000, 2000, 3600), "future stamp is due");
        assert!(is_throttled(1000, 900, 3600), "within interval");
        assert!(!is_throttled(5000, 1000, 3600), "past interval");
        assert!(
            !is_throttled(1000, 1000, 0),
            "zero interval never throttles"
        );
    }

    /// Fail-closed: a broken schema surfaces as `Err` (never a silent success)
    /// and never stamps, for both a GC-time failure (worktrees table gone,
    /// which fails after the meta read) and a meta-read failure (meta table
    /// gone, which fails before GC even starts).
    #[test]
    fn fail_closed_paths_return_err_without_stamp() {
        {
            let _g = env_guard();
            clear_auto_gc_env();
            let tmp = tempfile::TempDir::new().unwrap();
            let db = WorktreeDb::open(tmp.path()).unwrap();
            db.execute_batch_for_test("DROP TABLE worktrees;").unwrap();
            let err = maybe_auto_gc(&db, &opts_enabled_no_orphans(false));
            assert!(err.is_err(), "GC failure must surface as Err");
            assert!(
                db.get_meta(META_LAST_AUTO_GC_AT).unwrap().is_none(),
                "GC Err must not stamp last_auto_gc_at"
            );
        }
        {
            let _g = env_guard();
            clear_auto_gc_env();
            let tmp = tempfile::TempDir::new().unwrap();
            let db = WorktreeDb::open(tmp.path()).unwrap();
            db.execute_batch_for_test("DROP TABLE meta;").unwrap();
            assert!(
                maybe_auto_gc(&db, &opts_enabled_no_orphans(false)).is_err(),
                "meta read failure must fail closed"
            );
        }
    }

    #[test]
    fn unparseable_stamp_fails_open_and_restamps() {
        let _g = env_guard();
        clear_auto_gc_env();
        let tmp = tempfile::TempDir::new().unwrap();
        let db = WorktreeDb::open(tmp.path()).unwrap();
        db.set_meta(META_LAST_AUTO_GC_AT, "not-a-number").unwrap();
        let report = maybe_auto_gc(&db, &opts_enabled_no_orphans(false)).unwrap();
        assert_eq!(report.outcome, AutoGcOutcome::Ran);
        assert!(report.stamped);
        let stamp = db.get_meta(META_LAST_AUTO_GC_AT).unwrap().unwrap();
        assert!(
            stamp.parse::<i64>().is_ok(),
            "must restamp a parseable epoch after unparseable prior value"
        );
    }

    #[test]
    fn set_meta_err_after_gc_still_returns_ran() {
        // Stamp write failure must not turn a successful GC into Err for hooks.
        let _g = env_guard();
        clear_auto_gc_env();
        let tmp = tempfile::TempDir::new().unwrap();
        let db = WorktreeDb::open(tmp.path()).unwrap();
        db.execute_batch_for_test(
            "CREATE TRIGGER block_meta_write BEFORE INSERT ON meta BEGIN
               SELECT RAISE(ABORT, 'blocked');
             END;",
        )
        .unwrap();
        let report = maybe_auto_gc(&db, &opts_enabled_no_orphans(false)).unwrap();
        assert_eq!(
            report.outcome,
            AutoGcOutcome::Ran,
            "set_meta Err after GC Ok must still return Ok(Ran)"
        );
        assert!(!report.stamped, "failed set_meta must report stamped=false");
    }

    /// `resolve_worktree_auto_gc_from_layers` precedence (env > local > remote
    /// > defaults), kind-map merge, and numeric clamps.
    #[test]
    fn resolve_worktree_auto_gc_layers_table() {
        let _g = env_guard();
        type Check = Box<dyn Fn(&ResolvedWorktreeAutoGc)>;
        struct Case {
            name: &'static str,
            env: Vec<(&'static str, &'static str)>,
            local: Option<WorktreeAutoGcLayer>,
            remote: Option<WorktreeAutoGcLayer>,
            check: Check,
        }
        let cases: Vec<Case> = vec![
            Case {
                name: "defaults include manual never",
                env: vec![],
                local: None,
                remote: None,
                check: Box::new(|p| {
                    assert_eq!(p.max_age_by_kind, default_max_age_by_kind());
                    assert_eq!(p.max_age_secs, DEFAULT_MAX_AGE_SECS);
                    assert!(p.enabled);
                    assert!(!p.dry_run);
                    assert!(!p.include_rebuild, "rebuild off by default");
                    assert_eq!(
                        p.rebuild_min_interval_secs,
                        DEFAULT_REBUILD_MIN_INTERVAL_SECS
                    );
                }),
            },
            Case {
                name: "local disable beats remote; clamps to MIN",
                env: vec![],
                local: Some(WorktreeAutoGcLayer {
                    enabled: Some(false),
                    ..Default::default()
                }),
                remote: Some(WorktreeAutoGcLayer {
                    enabled: Some(true),
                    max_age_secs: Some(1),
                    min_interval_secs: Some(1),
                    dry_run: Some(false),
                    ..Default::default()
                }),
                check: Box::new(|p| {
                    assert!(!p.enabled, "local enabled=false beats remote true");
                    assert_eq!(p.max_age_secs, MAX_AGE_SECS_MIN, "remote TTL clamped low");
                    assert_eq!(
                        p.min_interval_secs, MIN_INTERVAL_SECS_MIN,
                        "interval clamped low"
                    );
                    assert_eq!(p.max_age_by_kind.get(&WorktreeKind::Manual), Some(&None));
                }),
            },
            Case {
                name: "env kill wins over local enabled",
                env: vec![(ENV_AUTO_GC, "0")],
                local: Some(WorktreeAutoGcLayer {
                    enabled: Some(true),
                    ..Default::default()
                }),
                remote: None,
                check: Box::new(|p| assert!(!p.enabled, "env kill wins")),
            },
            Case {
                name: "env dry-run wins over local/remote false",
                env: vec![(ENV_AUTO_GC_DRY_RUN, "1")],
                local: Some(WorktreeAutoGcLayer {
                    dry_run: Some(false),
                    ..Default::default()
                }),
                remote: Some(WorktreeAutoGcLayer {
                    dry_run: Some(false),
                    ..Default::default()
                }),
                check: Box::new(|p| {
                    assert!(p.dry_run, "env dry-run wins over local/remote false");
                    assert!(p.enabled);
                }),
            },
            Case {
                name: "env max_age wins over local + remote",
                env: vec![(ENV_AUTO_GC_MAX_AGE, "7200")],
                local: Some(WorktreeAutoGcLayer {
                    max_age_secs: Some(86400),
                    ..Default::default()
                }),
                remote: Some(WorktreeAutoGcLayer {
                    max_age_secs: Some(3600),
                    ..Default::default()
                }),
                check: Box::new(|p| assert_eq!(p.max_age_secs, 7200, "env MAX_AGE wins")),
            },
            Case {
                name: "invalid env max_age falls through to local",
                env: vec![(ENV_AUTO_GC_MAX_AGE, "not-a-number")],
                local: Some(WorktreeAutoGcLayer {
                    max_age_secs: Some(86400),
                    ..Default::default()
                }),
                remote: None,
                check: Box::new(|p| {
                    assert_eq!(p.max_age_secs, 86400, "invalid env max age → local")
                }),
            },
            Case {
                name: "kind map: local wins, remote can expire manual, pool from remote",
                env: vec![],
                local: Some(WorktreeAutoGcLayer {
                    max_age_by_kind: BTreeMap::from([(WorktreeKind::Subagent, Some(7200))]),
                    ..Default::default()
                }),
                remote: Some(WorktreeAutoGcLayer {
                    max_age_by_kind: BTreeMap::from([
                        (WorktreeKind::Subagent, Some(1)),
                        (WorktreeKind::Manual, Some(86400)),
                        (WorktreeKind::Pool, Some(172800)),
                    ]),
                    ..Default::default()
                }),
                check: Box::new(|p| {
                    assert_eq!(
                        p.max_age_by_kind.get(&WorktreeKind::Subagent),
                        Some(&Some(7200)),
                        "local kind TTL wins"
                    );
                    assert_eq!(
                        p.max_age_by_kind.get(&WorktreeKind::Manual),
                        Some(&Some(86400)),
                        "remote makes manual expire when local omits"
                    );
                    assert_eq!(
                        p.max_age_by_kind.get(&WorktreeKind::Pool),
                        Some(&Some(172800))
                    );
                }),
            },
            Case {
                name: "local restores manual never over remote expire",
                env: vec![],
                local: Some(WorktreeAutoGcLayer {
                    max_age_by_kind: BTreeMap::from([(WorktreeKind::Manual, None)]),
                    ..Default::default()
                }),
                remote: Some(WorktreeAutoGcLayer {
                    max_age_by_kind: BTreeMap::from([(WorktreeKind::Manual, Some(86400))]),
                    ..Default::default()
                }),
                check: Box::new(|p| {
                    assert_eq!(p.max_age_by_kind.get(&WorktreeKind::Manual), Some(&None))
                }),
            },
            Case {
                name: "env rebuild wins over local false",
                env: vec![(ENV_AUTO_GC_REBUILD, "1")],
                local: Some(WorktreeAutoGcLayer {
                    include_rebuild: Some(false),
                    ..Default::default()
                }),
                remote: None,
                check: Box::new(|p| {
                    assert!(p.include_rebuild, "env REBUILD=1 wins over local false")
                }),
            },
            Case {
                name: "local include_rebuild + custom interval",
                env: vec![],
                local: Some(WorktreeAutoGcLayer {
                    include_rebuild: Some(true),
                    rebuild_min_interval_secs: Some(120),
                    ..Default::default()
                }),
                remote: None,
                check: Box::new(|p| {
                    assert!(p.include_rebuild);
                    assert_eq!(p.rebuild_min_interval_secs, 120);
                }),
            },
            Case {
                name: "numeric clamps high (max_age, min_interval) + low (rebuild interval)",
                env: vec![],
                local: Some(WorktreeAutoGcLayer {
                    max_age_secs: Some(u64::MAX),
                    min_interval_secs: Some(u64::MAX),
                    rebuild_min_interval_secs: Some(1),
                    ..Default::default()
                }),
                remote: None,
                check: Box::new(|p| {
                    assert_eq!(p.max_age_secs, MAX_AGE_SECS_MAX, "max_age clamps high");
                    assert_eq!(
                        p.min_interval_secs, MIN_INTERVAL_SECS_MAX,
                        "min_interval clamps high"
                    );
                    assert_eq!(
                        p.rebuild_min_interval_secs, MIN_INTERVAL_SECS_MIN,
                        "rebuild interval clamps low"
                    );
                }),
            },
        ];
        for case in cases {
            let Case {
                name,
                env,
                local,
                remote,
                check,
            } = case;
            clear_auto_gc_env();
            for (k, v) in &env {
                unsafe { std::env::set_var(k, v) };
            }
            eprintln!("resolve layer case: {name}");
            let policy = resolve_worktree_auto_gc_from_layers(local.as_ref(), remote.as_ref());
            check(&policy);
            clear_auto_gc_env();
        }
    }

    #[test]
    fn include_rebuild_true_registers_untracked_under_echo_agent_home() {
        let _g = env_guard();
        clear_auto_gc_env();
        let fx = crate::db::EchoAgentHomeFixture::new();
        let db = WorktreeDb::open(&fx.home).unwrap();

        let wt = fx.home.join("worktrees/repo/untracked-sess");
        std::fs::create_dir_all(wt.join(".git")).unwrap();

        let report = maybe_auto_gc(&db, &rebuild_opts()).unwrap();
        assert_eq!(report.outcome, AutoGcOutcome::Ran);
        let rebuild = report
            .rebuild
            .as_ref()
            .expect("rebuild must run when enabled and due");
        assert_eq!(rebuild.discovered, 1);
        assert_eq!(rebuild.registered, 1);
        assert!(report.rebuild_stamped);
        assert!(
            db.get_meta(META_LAST_AUTO_REBUILD_AT).unwrap().is_some(),
            "successful rebuild must stamp last_auto_rebuild_at"
        );
        assert!(
            db.get(&wt.to_string_lossy()).unwrap().is_some(),
            "untracked dir under echo_agent_home/worktrees must be registered"
        );
    }

    /// Rebuild + prune run only on a real (non-dry-run) pass with
    /// `include_rebuild=true`. Every other flag combination leaves the DB
    /// untouched: no rebuild report, no rebuild-meta stamp, the untracked tree
    /// stays unregistered, and stale git registrations are not pruned.
    #[test]
    fn rebuild_prune_gated_on_real_rebuild_pass() {
        for (include_rebuild, dry_run) in [(false, false), (true, true), (false, true)] {
            let case = format!("include_rebuild={include_rebuild} dry_run={dry_run}");
            let _g = env_guard();
            clear_auto_gc_env();
            let fx = crate::db::EchoAgentHomeFixture::new();
            let db = WorktreeDb::open(&fx.home).unwrap();

            // An untracked tree a rebuild *would* register.
            let wt = fx.home.join("worktrees/repo/untracked-sess");
            std::fs::create_dir_all(wt.join(".git")).unwrap();

            // A stale git registration a prune *would* scrub.
            let repo = fx.home.join("src-repo");
            let stale = fx.home.join("stale-wt");
            plant_stale_git_worktree(&repo, &stale);
            let before = count_regs(&repo);
            assert!(before >= 1, "{case}: expected a stale registration");
            let tracked = fx.home.join("tracked");
            std::fs::create_dir_all(&tracked).unwrap();
            let mut rec = make_rec("tracked", tracked, WorktreeKind::Session, now_epoch_secs());
            rec.source_repo = repo.clone();
            db.register(&rec).unwrap();

            let report = maybe_auto_gc(
                &db,
                &ResolvedWorktreeAutoGc {
                    include_rebuild,
                    rebuild_min_interval_secs: 0,
                    dry_run,
                    ..auto_opts()
                },
            )
            .unwrap();
            assert_eq!(report.outcome, AutoGcOutcome::Ran, "{case}");
            assert!(report.rebuild.is_none(), "{case}: no rebuild report");
            assert!(!report.rebuild_stamped, "{case}: no rebuild stamp");
            assert_eq!(report.stale_registrations_cleaned, 0, "{case}: no prune");
            assert!(
                db.get_meta(META_LAST_AUTO_REBUILD_AT).unwrap().is_none(),
                "{case}: rebuild meta must stay unset"
            );
            assert!(
                db.get(&wt.to_string_lossy()).unwrap().is_none(),
                "{case}: untracked tree must not be registered"
            );
            assert_eq!(
                count_regs(&repo),
                before,
                "{case}: registration count must not drop"
            );
        }
    }

    #[test]
    fn rebuild_throttled_independently_of_gc() {
        let _g = env_guard();
        clear_auto_gc_env();
        let fx = crate::db::EchoAgentHomeFixture::new();
        let db = WorktreeDb::open(&fx.home).unwrap();

        let opts = ResolvedWorktreeAutoGc {
            include_rebuild: true,
            rebuild_min_interval_secs: 3600,
            ..auto_opts()
        };

        let first = maybe_auto_gc(&db, &opts).unwrap();
        assert_eq!(first.outcome, AutoGcOutcome::Ran);
        assert!(first.rebuild.is_some());
        assert!(first.rebuild_stamped);
        let rebuild_stamp = db.get_meta(META_LAST_AUTO_REBUILD_AT).unwrap();
        assert!(rebuild_stamp.is_some());

        // Second GC pass still runs (min_interval 0) but rebuild is throttled.
        let second = maybe_auto_gc(&db, &opts).unwrap();
        assert_eq!(second.outcome, AutoGcOutcome::Ran);
        assert!(
            second.rebuild.is_none(),
            "rebuild within rebuild_min_interval must skip"
        );
        assert!(!second.rebuild_stamped);
        assert_eq!(
            db.get_meta(META_LAST_AUTO_REBUILD_AT).unwrap(),
            rebuild_stamp,
            "throttled rebuild must not rewrite stamp"
        );
        assert!(
            second.stamped,
            "GC stamp still advances when rebuild is throttled"
        );
    }

    #[test]
    fn rebuild_failure_does_not_block_dead_record_gc() {
        // INSERT-aborting trigger makes rebuild register fail; SELECT/DELETE for
        // dead-path GC still work so reclaim continues after rebuild Err.
        let _g = env_guard();
        clear_auto_gc_env();
        let fx = crate::db::EchoAgentHomeFixture::new();
        let db = WorktreeDb::open(&fx.home).unwrap();

        db.register(&make_rec(
            "dead-after-rebuild-err",
            PathBuf::from("/nonexistent/dead-wt-rebuild-err"),
            WorktreeKind::Session,
            100,
        ))
        .unwrap();

        let untracked = fx.home.join("worktrees/repo/untracked-for-fail");
        std::fs::create_dir_all(untracked.join(".git")).unwrap();

        db.execute_batch_for_test(
            "CREATE TRIGGER block_worktree_insert BEFORE INSERT ON worktrees BEGIN
               SELECT RAISE(ABORT, 'rebuild-blocked');
             END;",
        )
        .unwrap();

        let report = maybe_auto_gc(&db, &rebuild_opts()).unwrap();
        assert_eq!(report.outcome, AutoGcOutcome::Ran);
        assert!(
            report.rebuild.is_none(),
            "rebuild Err must not populate rebuild report"
        );
        assert!(
            !report.rebuild_stamped,
            "failed rebuild must not stamp last_auto_rebuild_at"
        );
        assert!(
            db.get_meta(META_LAST_AUTO_REBUILD_AT).unwrap().is_none(),
            "failed rebuild must leave rebuild meta unset"
        );
        assert_eq!(
            report.gc.as_ref().unwrap().dead_removed,
            1,
            "dead-record GC must still run when rebuild fails"
        );
        assert!(report.stamped, "GC Ok must still stamp last_auto_gc_at");
    }

    /// A real rebuild pass prunes a stale echo-agent-owned git registration. The
    /// source repo is discovered from the tracked row's snapshot, which holds
    /// even when that row is the sole record and is *dead* (GC unregisters it
    /// only after the prune snapshot is taken).
    #[test]
    fn prune_removes_stale_registration_alive_and_dead_source() {
        for dead_source in [false, true] {
            let case = format!("dead_source={dead_source}");
            let _g = env_guard();
            clear_auto_gc_env();
            let fx = crate::db::EchoAgentHomeFixture::new();
            let db = WorktreeDb::open(&fx.home).unwrap();

            let repo = fx.home.join("src-repo");
            let stale = fx.home.join("linked-wt");
            plant_stale_git_worktree(&repo, &stale);
            let before = count_regs(&repo);
            assert!(before >= 1, "{case}: expected a stale registration");

            // The tracked row carries source_repo; alive vs dead controls
            // whether GC unregisters it before prune reads the snapshot.
            let (id, path, created_at) = if dead_source {
                ("sole-dead", PathBuf::from("/nonexistent/sole-dead-wt"), 100)
            } else {
                let alive = fx.home.join("still-there");
                std::fs::create_dir_all(&alive).unwrap();
                ("tracked", alive, now_epoch_secs())
            };
            let mut rec = make_rec(id, path, WorktreeKind::Session, created_at);
            rec.source_repo = repo.clone();
            db.register(&rec).unwrap();

            let report = maybe_auto_gc(&db, &rebuild_opts()).unwrap();
            assert_eq!(report.outcome, AutoGcOutcome::Ran, "{case}");
            if dead_source {
                assert_eq!(report.gc.as_ref().unwrap().dead_removed, 1, "{case}");
            }
            assert!(
                report.stale_registrations_cleaned >= 1,
                "{case}: stale registration must be pruned; cleaned={}",
                report.stale_registrations_cleaned
            );
            assert!(
                count_regs(&repo) < before,
                "{case}: registration count must drop after prune"
            );
        }
    }

    #[test]
    fn rebuild_not_stamped_when_gc_fails_after_rebuild() {
        // Rebuild succeeds (registers untracked), then GC fails on sweep UPDATE.
        // Rebuild meta must stay unset so the next pass can re-discover.
        let _g = env_guard();
        clear_auto_gc_env();
        let fx = crate::db::EchoAgentHomeFixture::new();
        let db = WorktreeDb::open(&fx.home).unwrap();
        db.register(&make_rec(
            "alive-missing-path",
            PathBuf::from("/nonexistent/alive-for-gc-fail"),
            WorktreeKind::Session,
            100,
        ))
        .unwrap();
        let untracked = fx.home.join("worktrees/repo/rebuild-then-gc-fail");
        std::fs::create_dir_all(untracked.join(".git")).unwrap();

        db.execute_batch_for_test(
            "CREATE TRIGGER block_worktree_update BEFORE UPDATE ON worktrees BEGIN
               SELECT RAISE(ABORT, 'gc-sweep-blocked');
             END;",
        )
        .unwrap();

        maybe_auto_gc(&db, &rebuild_opts()).expect_err("GC sweep UPDATE must fail the pass");
        assert!(
            db.get_meta(META_LAST_AUTO_REBUILD_AT).unwrap().is_none(),
            "rebuild must not stamp when GC fails after a successful rebuild"
        );
        assert!(
            db.get_meta(META_LAST_AUTO_GC_AT).unwrap().is_none(),
            "GC must not stamp when the pass returns Err"
        );
        // Rebuild already registered the untracked tree before GC failed.
        assert!(
            db.get(untracked.to_str().unwrap()).unwrap().is_some(),
            "rebuild registration from the failed pass is retained"
        );
    }

    #[test]
    fn classify_rebuild_meta_err_skips_not_aborts() {
        let err = Err(anyhow::anyhow!("meta unavailable"));
        assert_eq!(
            classify_rebuild_meta(err, 1000, 3600),
            RebuildMetaClass::SkipFailed
        );
        assert_eq!(
            classify_rebuild_meta(Ok(None), 1000, 3600),
            RebuildMetaClass::Due
        );
        assert_eq!(
            classify_rebuild_meta(Ok(Some("900".into())), 1000, 3600),
            RebuildMetaClass::Throttled
        );
        assert_eq!(
            classify_rebuild_meta(Ok(Some("not-a-number".into())), 1000, 3600),
            RebuildMetaClass::Due
        );
        assert_eq!(
            classify_rebuild_meta(Ok(Some("100".into())), 10000, 3600),
            RebuildMetaClass::Due
        );
    }

    #[test]
    fn rebuild_set_meta_failure_still_continues_gc() {
        let _g = env_guard();
        clear_auto_gc_env();
        let fx = crate::db::EchoAgentHomeFixture::new();
        let db = WorktreeDb::open(&fx.home).unwrap();
        db.register(&make_rec(
            "dead-stamp",
            PathBuf::from("/nonexistent/dead-stamp-wt"),
            WorktreeKind::Session,
            100,
        ))
        .unwrap();
        // Block only INSERT (UPSERT is INSERT OR REPLACE → INSERT path).
        db.execute_batch_for_test(
            "CREATE TRIGGER block_meta_insert BEFORE INSERT ON meta BEGIN
               SELECT RAISE(ABORT, 'meta-blocked');
             END;",
        )
        .unwrap();

        let report = maybe_auto_gc(&db, &rebuild_opts()).unwrap();
        assert_eq!(report.outcome, AutoGcOutcome::Ran);
        // Rebuild may succeed registering before stamp; both stamps use set_meta INSERT.
        if report.rebuild.is_some() {
            assert!(!report.rebuild_stamped);
        }
        assert_eq!(
            report.gc.as_ref().unwrap().dead_removed,
            1,
            "GC must continue after rebuild stamp failure"
        );
        assert!(!report.stamped, "GC stamp also uses set_meta INSERT");
    }

    #[test]
    fn env_rebuild_reapplied_inside_maybe_auto_gc() {
        let _g = env_guard();
        clear_auto_gc_env();
        unsafe { std::env::set_var(ENV_AUTO_GC_REBUILD, "1") };
        let fx = crate::db::EchoAgentHomeFixture::new();
        let db = WorktreeDb::open(&fx.home).unwrap();
        let wt = fx.home.join("worktrees/repo/env-rebuild-sess");
        std::fs::create_dir_all(wt.join(".git")).unwrap();

        // opts.include_rebuild false; env must still enable.
        let report = maybe_auto_gc(
            &db,
            &ResolvedWorktreeAutoGc {
                include_rebuild: false,
                rebuild_min_interval_secs: 0,
                ..auto_opts()
            },
        )
        .unwrap();
        assert!(
            report.rebuild.is_some(),
            "env REBUILD must re-apply inside maybe_auto_gc"
        );
        clear_auto_gc_env();
    }

    #[test]
    fn gc_throttled_short_circuits_rebuild() {
        let _g = env_guard();
        clear_auto_gc_env();
        let fx = crate::db::EchoAgentHomeFixture::new();
        let db = WorktreeDb::open(&fx.home).unwrap();
        // GC recently stamped; rebuild never stamped and would be due.
        db.set_meta(META_LAST_AUTO_GC_AT, &now_epoch_secs().to_string())
            .unwrap();
        let wt = fx.home.join("worktrees/repo/throttle-rebuild");
        std::fs::create_dir_all(wt.join(".git")).unwrap();

        let report = maybe_auto_gc(
            &db,
            &ResolvedWorktreeAutoGc {
                min_interval_secs: 3600,
                ..rebuild_opts()
            },
        )
        .unwrap();
        assert_eq!(report.outcome, AutoGcOutcome::Throttled);
        assert!(report.rebuild.is_none());
        assert!(
            db.get(&wt.to_string_lossy()).unwrap().is_none(),
            "GC throttle must skip rebuild"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn rebuild_same_pass_does_not_age_expire_new_registration() {
        let _g = env_guard();
        let _cwd_lock = crate::api::cwd_test_guard();
        clear_auto_gc_env();
        let fx = crate::db::EchoAgentHomeFixture::new();
        let db = WorktreeDb::open(&fx.home).unwrap();
        let wt = fx.home.join("worktrees/repo/fresh-rebuild");
        std::fs::create_dir_all(wt.join(".git")).unwrap();
        // Old directory mtime would look expired under max_age=0 without touch.
        let report = maybe_auto_gc(
            &db,
            &ResolvedWorktreeAutoGc {
                max_age_secs: 0,
                dry_run: false,
                ..rebuild_opts()
            },
        )
        .unwrap();
        assert!(report.age_expiry_enabled);
        assert!(report.rebuild.as_ref().is_some_and(|r| r.registered == 1));
        assert!(
            wt.exists(),
            "just-registered rebuild path must not age-delete same pass"
        );
        assert!(db.get(&wt.to_string_lossy()).unwrap().is_some());
    }
}
