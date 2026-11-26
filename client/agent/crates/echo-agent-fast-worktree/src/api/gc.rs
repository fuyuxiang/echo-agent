use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::BtrfsDelegate;
use crate::db::{ListFilter, WorktreeDb, WorktreeKind, WorktreeStatus};
use crate::git::dirs::has_any_file;
use crate::git::{KeepReason, RECLAIMED_LIFETIME, Reclaim, collect_reclaimed_names};

mod process_scan;

pub(crate) use process_scan::{LiveCwdScan, live_process_cwds, usable_cwds};
use process_scan::{cwd_within, is_pid_alive};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GcOptions {
    pub max_age_secs: Option<i64>,
    pub force: bool,
    pub dry_run: bool,
    #[serde(default, rename = "protect_paths")]
    pub keep_worktrees_containing: Vec<PathBuf>,
    #[serde(default)]
    pub max_age_by_kind: BTreeMap<WorktreeKind, Option<i64>>,
}

/// Time limits for one age pass.
#[derive(Clone, Copy)]
struct Pass {
    /// Wall-clock budget for the whole pass.
    budget: Duration,
    /// Per-worktree safety-gate timeout.
    gate_timeout: Duration,
}

impl Default for Pass {
    fn default() -> Self {
        Self {
            budget: AGE_PASS_BUDGET,
            gate_timeout: GATE_TIMEOUT_IN_PASS,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Entered {
    BeforeGate,
    AfterGate,
}

fn effective_max_age(opts: &GcOptions, kind: WorktreeKind) -> Option<i64> {
    opts.max_age_by_kind
        .get(&kind)
        .copied()
        .unwrap_or(opts.max_age_secs)
}

const AGE_PASS_BUDGET: Duration = Duration::from_secs(60);

const MAX_KEPT_REPORTED: usize = 100;

enum Verdict {
    AlreadyGone,
    Clear,
    NoRepo,
    Kept(KeepReason),
    Unnamed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Removal {
    Worktree,
    Path,
    Abandoned,
    Failed,
}

impl Removal {
    fn of(removed: bool, not_a_repo: bool) -> Self {
        match (removed, not_a_repo) {
            (true, false) => Removal::Worktree,
            (true, true) => Removal::Path,
            (false, true) => Removal::Abandoned,
            (false, false) => Removal::Failed,
        }
    }
}

enum StillReclaimable {
    Yes,
    Held,
    Unanswerable,
}

fn still_reclaimable(
    db: &WorktreeDb,
    rec: &crate::db::WorktreeRecord,
    now: i64,
    live_cwds: &[PathBuf],
    opts: &GcOptions,
) -> StillReclaimable {
    let Ok(Some(fresh)) = db.get_by_id(&rec.id) else {
        return StillReclaimable::Unanswerable;
    };
    if classify(&fresh, now, live_cwds, opts) == Eligibility::Reclaimable {
        StillReclaimable::Yes
    } else {
        StillReclaimable::Held
    }
}

fn recheck_holds(
    db: &WorktreeDb,
    rec: &crate::db::WorktreeRecord,
    now: i64,
    live_cwds: &[PathBuf],
    opts: &GcOptions,
    report: &mut GcReport,
) -> ControlFlow<()> {
    match still_reclaimable(db, rec, now, live_cwds, opts) {
        StillReclaimable::Yes => ControlFlow::Continue(()),
        StillReclaimable::Held => {
            report.skipped_alive += 1;
            ControlFlow::Break(())
        }
        StillReclaimable::Unanswerable => {
            tracing::debug!(id = %rec.id, "failed to re-read worktree record; keeping this pass");
            ControlFlow::Break(())
        }
    }
}

// Deliberately larger than the pass budget: a pass judges at most one slow
// worktree and lets its gate run over rather than truncating the safety check.
const GATE_TIMEOUT_IN_PASS: Duration = Duration::from_secs(120);

const _: () = assert!(
    GATE_TIMEOUT_IN_PASS.as_secs() > AGE_PASS_BUDGET.as_secs(),
    "the gate timeout must outlast the pass budget so a slow gate runs over rather than being truncated",
);

fn judge_one(path: &Path, source_repo: &Path, pass: Pass) -> Verdict {
    if path.exists() {
        decide_removal(path, source_repo, pass.gate_timeout)
    } else if std::fs::symlink_metadata(path).is_ok() {
        // A dangling symlink where the worktree was: the link is still on disk
        // but its target is gone (`exists()` above followed it and saw nothing).
        // It holds no repo content, so remove the link rather than leak it.
        Verdict::NoRepo
    } else {
        Verdict::AlreadyGone
    }
}

fn decide_removal(path: &Path, source_repo: &Path, timeout: Duration) -> Verdict {
    match crate::git::reclaimable_within(path, Some(source_repo), timeout) {
        Reclaim::Now { .. } => Verdict::Clear,
        Reclaim::Unnamed(error) => {
            tracing::warn!(path = %path.display(), %error, "keeping worktree: gate cleared but discarded commits could not be named");
            Verdict::Unnamed
        }
        Reclaim::Keep(KeepReason::NoRepo) => {
            // A stat error is "couldn't tell": keep, never remove. A non-dir
            // (file/symlink) at the path holds nothing a repo would lose.
            let meta = match std::fs::symlink_metadata(path) {
                Ok(meta) => meta,
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "failed to stat expired path; keeping");
                    return Verdict::Kept(KeepReason::CheckFailed);
                }
            };
            if !meta.is_dir() {
                return Verdict::NoRepo;
            }
            match has_any_file(path) {
                Ok(false) => {
                    tracing::warn!(path = %path.display(), "expired path is not a repo, no files; removing");
                    Verdict::NoRepo
                }
                Ok(true) => {
                    tracing::warn!(path = %path.display(), "expired path is not a repo, holds files; keeping");
                    Verdict::Kept(KeepReason::NoRepo)
                }
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "failed to read expired path; keeping");
                    Verdict::Kept(KeepReason::CheckFailed)
                }
            }
        }
        Reclaim::Keep(reason) => {
            tracing::info!(path = %path.display(), %reason, "keeping worktree: expired but not reclaimable");
            Verdict::Kept(reason)
        }
    }
}

pub(crate) fn age_path_enabled(opts: &GcOptions) -> bool {
    opts.max_age_secs.is_some() || !opts.max_age_by_kind.is_empty()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptWorktree {
    pub path: String,
    pub reason: String,
}

#[must_use]
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GcReport {
    pub dead_removed: u64,
    pub expired_removed: u64,
    pub skipped_alive: u64,
    /// Expired records whose kind never age-expires (e.g. `Manual`). Liveness is
    /// not consulted; counted separately from `skipped_alive` (which covers a
    /// live pid, a live CWD, or a protected path).
    #[serde(default)]
    pub never_expiring: u64,
    #[serde(default)]
    pub kept_unsafe: u64,
    #[serde(default)]
    pub kept_reasons: BTreeMap<String, u64>,
    #[serde(default)]
    pub kept: Vec<KeptWorktree>,
    #[serde(default)]
    pub no_repo_paths: u64,
    /// Reclaimable worktrees the pass ran out of time to judge (budget exhausted).
    #[serde(default)]
    pub not_judged: u64,
    /// Worktrees the gate cleared but whose discarded commits could not be
    /// named, so they were kept rather than removed.
    #[serde(default)]
    pub unnamed: u64,
    #[serde(default)]
    pub names_collected: u64,
    #[serde(default)]
    pub remove_failed: u64,
    /// Grove pin-ref union-liveness sweep (`refs/echoagent/worktrees/*`).
    #[serde(default)]
    pub pin_gc_examined: u64,
    #[serde(default)]
    pub pin_gc_pruned: u64,
    #[serde(default)]
    pub pin_gc_deferred: u64,
    #[serde(default)]
    pub pin_gc_kept: u64,
}

impl GcReport {
    fn record_kept(&mut self, path: &Path, reason: &KeepReason) {
        self.kept_unsafe += 1;
        *self
            .kept_reasons
            .entry(reason.name().to_string())
            .or_default() += 1;
        if self.kept.len() < MAX_KEPT_REPORTED {
            self.kept.push(KeptWorktree {
                path: path.display().to_string(),
                reason: reason.to_string(),
            });
        }
    }
}

fn last_active(rec: &crate::db::WorktreeRecord) -> i64 {
    rec.last_accessed_at
        .unwrap_or(rec.created_at)
        .max(rec.created_at)
}

fn is_expired(rec: &crate::db::WorktreeRecord, now: i64, max_age: i64) -> bool {
    last_active(rec) < now.saturating_sub(max_age.max(0))
}

/// Dest is a kernel/NFS mount. `exists()` and `canonicalize` can hang;
/// probe the mount table only (never the dest inode).
fn dest_must_not_stat(path: &Path) -> bool {
    !crate::nfs::dest_is_known_unmounted(path)
}

fn rec_cwd_within(rec: &crate::db::WorktreeRecord, live_cwds: &[PathBuf]) -> bool {
    let path = Path::new(&rec.path);
    if crate::worktree::is_grove_strategy(&rec.creation_mode) || dest_must_not_stat(path) {
        // Never canonicalize an NFS dest (wedged mount hang), including
        // linked/copy rows whose dest is a live grove mount.
        return live_cwds
            .iter()
            .any(|cwd| crate::nfs::dest_path_contains(path, cwd));
    }
    cwd_within(path, live_cwds)
}

fn is_guarded(rec: &crate::db::WorktreeRecord, live_cwds: &[PathBuf]) -> bool {
    rec.creator_pid.is_some_and(is_pid_alive) || rec_cwd_within(rec, live_cwds)
}

/// True when one of `in_use` lies at or inside the worktree dest
/// (see `GcOptions::keep_worktrees_containing`).
fn worktree_holds_in_use_path(rec: &crate::db::WorktreeRecord, in_use: &[PathBuf]) -> bool {
    !in_use.is_empty() && rec_cwd_within(rec, in_use)
}

/// Single verdict on whether an age pass may reclaim a worktree. Every other
/// eligibility check (the main loop, the post-gate recheck) routes through here
/// so the rules (and the `force` override) live in exactly one place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Eligibility {
    /// Kind never age-expires (e.g. `Manual` with a `never` TTL).
    NeverExpires,
    /// Within its TTL.
    NotYetExpired,
    /// Expired but held by a live pid, a live cwd, or an in-use path.
    Guarded,
    /// Expired and unheld: a candidate for the safety gate.
    Reclaimable,
}

fn classify(
    rec: &crate::db::WorktreeRecord,
    now: i64,
    live_cwds: &[PathBuf],
    opts: &GcOptions,
) -> Eligibility {
    let Some(max_age) = effective_max_age(opts, rec.kind) else {
        return Eligibility::NeverExpires;
    };
    if !is_expired(rec, now, max_age) {
        return Eligibility::NotYetExpired;
    }
    // `force` is the operator override: it ignores liveness and in-use guards.
    if !opts.force
        && (is_guarded(rec, live_cwds)
            || worktree_holds_in_use_path(rec, &opts.keep_worktrees_containing))
    {
        return Eligibility::Guarded;
    }
    Eligibility::Reclaimable
}

fn unregister_logged(db: &WorktreeDb, id: &str) -> bool {
    match db.unregister(id) {
        Ok(removed) => removed,
        Err(error) => {
            tracing::warn!(%id, %error, "failed to unregister worktree row");
            false
        }
    }
}

fn reclaim_dead_records(db: &WorktreeDb, opts: &GcOptions, report: &mut GcReport) -> Result<()> {
    if opts.dry_run {
        let all = db.list(&ListFilter {
            include_dead: true,
            ..Default::default()
        })?;
        let dead = all
            .iter()
            .filter(|rec| {
                if rec.status == WorktreeStatus::Dead {
                    return true;
                }
                let path = Path::new(&rec.path);
                // `exists()` hangs on a wedged grove NFS dest.
                if crate::worktree::is_grove_strategy(&rec.creation_mode)
                    || dest_must_not_stat(path)
                {
                    return crate::nfs::nfs_record_is_dead(path, None);
                }
                std::fs::symlink_metadata(path).is_err()
            })
            .count();
        report.dead_removed = u64::try_from(dead).unwrap_or(u64::MAX);
        return Ok(());
    }
    db.sweep_dead()?;
    let dead = db.list(&ListFilter {
        status: Some(WorktreeStatus::Dead),
        include_dead: true,
        ..Default::default()
    })?;
    for rec in dead {
        if unregister_logged(db, &rec.id) {
            report.dead_removed += 1;
        }
    }
    Ok(())
}

fn count_never_expiring(
    rec: &crate::db::WorktreeRecord,
    now: i64,
    opts: &GcOptions,
    report: &mut GcReport,
) {
    let expired = match opts.max_age_secs {
        Some(reference) => is_expired(rec, now, reference),
        None => true,
    };
    if expired {
        report.never_expiring += 1;
    }
}

fn record_removal(
    db: &WorktreeDb,
    rec: &crate::db::WorktreeRecord,
    outcome: Removal,
    report: &mut GcReport,
) {
    match outcome {
        Removal::Worktree => report.expired_removed += 1,
        Removal::Path => report.no_repo_paths += 1,
        Removal::Abandoned => {
            if unregister_logged(db, &rec.id) {
                report.no_repo_paths += 1;
            } else {
                report.remove_failed += 1;
            }
        }
        Removal::Failed => report.remove_failed += 1,
    }
}

/// Records a non-removable verdict and tells the caller to skip this worktree:
/// `Break` = kept or unnamed (counted here); `Continue` = removable, proceed.
fn settle(verdict: &Verdict, path: &Path, report: &mut GcReport) -> ControlFlow<()> {
    match verdict {
        Verdict::Kept(reason) => {
            report.record_kept(path, reason);
            ControlFlow::Break(())
        }
        Verdict::Unnamed => {
            report.unnamed += 1;
            ControlFlow::Break(())
        }
        Verdict::AlreadyGone | Verdict::Clear | Verdict::NoRepo => ControlFlow::Continue(()),
    }
}

fn dispose_of(
    db: &WorktreeDb,
    rec: &crate::db::WorktreeRecord,
    verdict: Verdict,
    delegate: Option<Arc<dyn BtrfsDelegate>>,
    report: &mut GcReport,
) {
    let path = Path::new(&rec.path);
    // `exists()` follows symlinks; a dangling link reads as absent but must still be unlinked.
    if !path.exists() && std::fs::symlink_metadata(path).is_err() {
        if unregister_logged(db, &rec.id) {
            report.expired_removed += 1;
        }
        return;
    }
    let not_a_repo = match verdict {
        Verdict::Clear => false,
        Verdict::NoRepo => true,
        Verdict::AlreadyGone => {
            tracing::warn!(
                path = %path.display(),
                "expired path appeared after the verdict; leaving it for the next pass"
            );
            return;
        }
        Verdict::Kept(_) | Verdict::Unnamed => return,
    };
    let removal = super::remove_worktree_with_delegate(path, delegate);
    let outcome = Removal::of(removal.is_ok(), not_a_repo);
    if let Err(error) = &removal {
        tracing::warn!(
            path = %path.display(),
            %error,
            ?outcome,
            "failed to remove expired worktree"
        );
    }
    record_removal(db, rec, outcome, report);
}

fn reclaim_expired_worktrees(
    db: &WorktreeDb,
    opts: &GcOptions,
    pass: Pass,
    delegate: Option<Arc<dyn BtrfsDelegate>>,
    now: i64,
    hook: Option<&dyn Fn(Entered)>,
    report: &mut GcReport,
) -> Result<()> {
    let cwd_scan = if opts.force {
        LiveCwdScan::Ok(Vec::new())
    } else {
        live_process_cwds()
    };
    let Some(live_cwds) = usable_cwds(&cwd_scan, opts.force) else {
        tracing::warn!("process CWD scan failed or unusable; skipping age-expiry (fail closed)");
        return Ok(());
    };
    let mut alive = db.list(&ListFilter::default())?;
    // Skip the "unknown" discovery sentinel and vanished repos so the collector
    // does not shell `git` into a bad path (and warn) on every pass.
    let sources: BTreeSet<PathBuf> = alive
        .iter()
        .map(|rec| rec.source_repo.clone())
        .filter(|repo| repo.as_os_str() != "unknown" && repo.is_dir())
        .collect();
    alive.sort_by_key(last_active);
    let stopped_last_time = db.get_meta(META_LAST_AGE_CURSOR).unwrap_or_default();
    let ids: Vec<&str> = alive.iter().map(|rec| rec.id.as_str()).collect();
    let start = resume_at(&ids, stopped_last_time.as_deref());
    alive.rotate_left(start);
    let deadline = Instant::now() + pass.budget;
    let mut stopped_at = None;
    for rec in alive {
        match classify(&rec, now, live_cwds, opts) {
            Eligibility::NeverExpires => {
                count_never_expiring(&rec, now, opts, report);
                continue;
            }
            Eligibility::NotYetExpired => continue,
            Eligibility::Guarded => {
                report.skipped_alive += 1;
                continue;
            }
            Eligibility::Reclaimable => {}
        }
        let path = Path::new(&rec.path);
        if dest_must_not_stat(path) {
            // Any kernel mount: never exists()/gate. judge_one and dispose_of
            // stat dest; remove_worktree also symlink_metadata after the NFS
            // arm. Unmounted leftover dirs fall through for dest reuse.
            report.skipped_alive += 1;
            continue;
        }
        if Instant::now() >= deadline {
            report.not_judged += 1;
            stopped_at.get_or_insert_with(|| rec.id.clone());
            continue;
        }
        if let Some(hook) = hook {
            hook(Entered::BeforeGate);
        }
        if !opts.force
            && !opts.dry_run
            && recheck_holds(db, &rec, now, live_cwds, opts, report).is_break()
        {
            continue;
        }
        let verdict = judge_one(path, rec.source_repo.as_path(), pass);
        if settle(&verdict, path, report).is_break() {
            continue;
        }
        if opts.dry_run {
            match &verdict {
                Verdict::NoRepo => report.no_repo_paths += 1,
                Verdict::Clear => report.expired_removed += 1,
                Verdict::AlreadyGone | Verdict::Kept(_) | Verdict::Unnamed => {}
            }
            continue;
        }
        if let Some(hook) = hook {
            hook(Entered::AfterGate);
        }
        // Re-check the freshly read record after the gate (which can run for
        // minutes): a session that re-registered with a live creator_pid in
        // that window must not be removed. The CWD list is the pass-start
        // snapshot, so a bare chdir after the scan is not re-observed here.
        if !opts.force && recheck_holds(db, &rec, now, live_cwds, opts, report).is_break() {
            continue;
        }
        // Re-judge immediately before the irreversible removal: the first
        // verdict is up to GATE_TIMEOUT_IN_PASS old and liveness cannot see work
        // written into the worktree since. Only a fresh verdict may delete.
        let verdict = judge_one(path, rec.source_repo.as_path(), pass);
        if settle(&verdict, path, report).is_break() {
            continue;
        }
        dispose_of(db, &rec, verdict, delegate.clone(), report);
    }
    if !opts.dry_run {
        collect_names_in(&sources, report);
    }
    if !opts.dry_run
        && let Err(error) = db.set_meta(
            META_LAST_AGE_CURSOR,
            stopped_at.as_deref().unwrap_or_default(),
        )
    {
        tracing::warn!(%error, "failed to persist age-pass cursor");
    }
    Ok(())
}

pub fn gc_worktrees(db: &WorktreeDb, opts: &GcOptions) -> Result<GcReport> {
    gc_worktrees_with_delegate(db, opts, None)
}

pub fn gc_worktrees_with_delegate(
    db: &WorktreeDb,
    opts: &GcOptions,
    delegate: Option<Arc<dyn BtrfsDelegate>>,
) -> Result<GcReport> {
    run_pass(db, opts, Pass::default(), delegate, None)
}

fn collect_names_in(sources: &BTreeSet<PathBuf>, report: &mut GcReport) {
    for source in sources {
        match collect_reclaimed_names(source, RECLAIMED_LIFETIME) {
            Ok(dropped) => report.names_collected += u64::try_from(dropped).unwrap_or(u64::MAX),
            Err(error) => {
                // The probe already warned; note the retry at debug to avoid a double warn.
                tracing::debug!(
                    path = %source.display(),
                    %error,
                    "reclaimed names were not collected; the next pass tries again"
                );
            }
        }
    }
}

fn run_pass(
    db: &WorktreeDb,
    opts: &GcOptions,
    pass: Pass,
    delegate: Option<Arc<dyn BtrfsDelegate>>,
    hook: Option<&dyn Fn(Entered)>,
) -> Result<GcReport> {
    let mut report = GcReport::default();
    let now = crate::db::now_epoch_secs();

    reclaim_dead_records(db, opts, &mut report)?;

    if age_path_enabled(opts) {
        reclaim_expired_worktrees(db, opts, pass, delegate, now, hook, &mut report)?;
    }

    reclaim_orphan_pins(db, opts, now, &mut report);

    Ok(report)
}

/// Union-liveness pin sweep. Never fails the worktree GC pass: one grove
/// data dir must not block dead/age reclaim.
fn reclaim_orphan_pins(db: &WorktreeDb, opts: &GcOptions, now: i64, report: &mut GcReport) {
    let recs = match db.list(&ListFilter {
        include_dead: true,
        ..Default::default()
    }) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "pin GC: worktrees.db list failed");
            return;
        }
    };
    let existing = crate::nfs::identities_from_worktree_records(&recs);
    let mut seen = HashSet::new();
    let mut pruned_ids = HashSet::new();
    for dir in crate::nfs::candidate_data_dirs() {
        if dir.as_os_str().is_empty() || !seen.insert(dir.clone()) {
            continue;
        }
        match crate::nfs::gc_orphan_pins(&dir, &existing, now, opts.dry_run) {
            Ok(r) => {
                report.pin_gc_examined = report.pin_gc_examined.saturating_add(r.examined);
                report.pin_gc_deferred = report.pin_gc_deferred.saturating_add(r.deferred_grace);
                report.pin_gc_kept = report.pin_gc_kept.saturating_add(r.kept_live);
                for id in r.pruned_ids {
                    if pruned_ids.insert(id) {
                        report.pin_gc_pruned = report.pin_gc_pruned.saturating_add(1);
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    dir = %dir.display(),
                    error = %e,
                    "pin GC: grove data dir sweep failed"
                );
            }
        }
    }
}

const META_LAST_AGE_CURSOR: &str = "last_age_cursor";

fn resume_at(ids: &[&str], stopped_at: Option<&str>) -> usize {
    stopped_at
        .and_then(|stopped_at| ids.iter().position(|id| *id == stopped_at))
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "gc/tests.rs"]
mod tests;
