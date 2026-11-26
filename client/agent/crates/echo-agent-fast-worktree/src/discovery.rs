//! Filesystem scanner for discovering worktrees not yet tracked in the DB.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::db::{
    WorktreeKind, WorktreeRecord, WorktreeStatus, id_from_path, now_epoch_secs, repo_name_from_path,
};

pub const WORKTREES_DIR: &str = "worktrees";
pub const WORKTREE_POOL_DIR: &str = "worktree_pool";
/// Depth of a worktree below its managed root: `<root>/<repo>/<worktree>`.
/// [`scan_two_level_dir`] and `echoagent du`'s bucketing have to agree on it.
pub const WORKTREE_DEPTH: usize = 2;

#[derive(Debug)]
pub struct DiscoveredWorktree {
    pub path: PathBuf,
    pub kind: WorktreeKind,
    pub creation_mode: &'static str,
    pub source_repo: Option<PathBuf>,
}

#[derive(Debug, Default)]
pub struct DiscoveryReport {
    pub found: Vec<DiscoveredWorktree>,
    pub skipped: u64,
}

fn should_skip_entry(name: &str) -> bool {
    name.starts_with('.')
        || name.ends_with(".ready")
        || name.ends_with(".claimed")
        || name.ends_with(".claiming")
}

fn detect_creation_mode(worktree_path: &Path) -> &'static str {
    let git_entry = worktree_path.join(".git");
    if git_entry.is_file() {
        "linked"
    } else if git_entry.is_dir() {
        "standalone"
    } else {
        "unknown"
    }
}

fn detect_source_repo(worktree_path: &Path) -> Option<PathBuf> {
    let git_entry = worktree_path.join(".git");
    if git_entry.is_file() {
        let content = std::fs::read_to_string(&git_entry).ok()?;
        let gitdir = content.trim().strip_prefix("gitdir: ")?;
        // Walk up from .git/worktrees/<name> → .git → repo root
        Path::new(gitdir)
            .parent()?
            .parent()?
            .parent()
            .map(|p| p.to_path_buf())
    } else if git_entry.is_dir() {
        Some(worktree_path.to_path_buf())
    } else {
        None
    }
}

fn scan_two_level_dir(
    base_dir: &Path,
    kind: WorktreeKind,
    report: &mut DiscoveryReport,
    skip_dests: &[PathBuf],
) {
    const _: () = assert!(WORKTREE_DEPTH == 2, "this scan is written for depth 2");
    let Ok(outer_entries) = std::fs::read_dir(base_dir) else {
        return;
    };

    for outer in outer_entries.flatten() {
        let outer_path = outer.path();
        if !outer_path.is_dir() {
            continue;
        }
        let outer_name = outer.file_name();
        if should_skip_entry(&outer_name.to_string_lossy()) {
            report.skipped += 1;
            continue;
        }

        let Ok(inner_entries) = std::fs::read_dir(&outer_path) else {
            continue;
        };
        for inner in inner_entries.flatten() {
            let path = inner.path();
            // Lexical skip before is_dir / .git / canonicalize: those stat the
            // dest and hang on a wedged grove NFS mount.
            if skip_dests
                .iter()
                .any(|dest| crate::nfs::dest_paths_equivalent(dest, &path))
            {
                report.skipped += 1;
                continue;
            }
            if !path.is_dir() || should_skip_entry(&inner.file_name().to_string_lossy()) {
                report.skipped += 1;
                continue;
            }
            report.found.push(DiscoveredWorktree {
                creation_mode: detect_creation_mode(&path),
                source_repo: detect_source_repo(&path),
                path,
                kind,
            });
        }
    }
}

pub fn discover_worktrees(echo_agent_home: &Path) -> DiscoveryReport {
    discover_worktrees_skipping(echo_agent_home, &[])
}

fn discover_worktrees_skipping(echo_agent_home: &Path, skip_dests: &[PathBuf]) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    scan_two_level_dir(
        &echo_agent_home.join(WORKTREES_DIR),
        WorktreeKind::Session,
        &mut report,
        skip_dests,
    );
    scan_two_level_dir(
        &echo_agent_home.join(WORKTREE_POOL_DIR),
        WorktreeKind::Pool,
        &mut report,
        skip_dests,
    );
    report
}

fn fs_creation_time(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.created())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or_else(now_epoch_secs)
}

impl DiscoveredWorktree {
    pub fn into_record(self) -> WorktreeRecord {
        let repo_name = self
            .source_repo
            .as_deref()
            .map(repo_name_from_path)
            .unwrap_or_else(|| "unknown".to_string());
        let source_repo = self.source_repo.unwrap_or_else(|| PathBuf::from("unknown"));
        let created_at = fs_creation_time(&self.path);
        // Match `WorktreeDb::get`, which looks up by canonical path.
        let path = dunce::canonicalize(&self.path).unwrap_or(self.path);

        WorktreeRecord {
            id: id_from_path(&path),
            path,
            source_repo,
            repo_name,
            kind: self.kind,
            creation_mode: self.creation_mode.to_owned(),
            git_ref: None,
            head_commit: None,
            session_id: None,
            creator_pid: None,
            created_at,
            last_accessed_at: None,
            status: WorktreeStatus::Alive,
            metadata: None,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RebuildReport {
    pub discovered: u64,
    pub registered: u64,
    pub already_tracked: u64,
}

pub fn managed_worktree_roots(echo_agent_home: &Path) -> [PathBuf; 2] {
    [
        echo_agent_home.join(WORKTREES_DIR),
        echo_agent_home.join(WORKTREE_POOL_DIR),
    ]
    .map(|root| dunce::canonicalize(&root).unwrap_or(root))
}

/// True when `path` is under a managed root (`worktrees/` or `worktree_pool/`).
/// Prefer an already-canonical `path`; the roots are canonicalized inside.
pub fn path_under_managed_worktree_roots(path: &Path, echo_agent_home: &Path) -> bool {
    path_under_worktree_roots(path, &managed_worktree_roots(echo_agent_home))
}

/// True when `path` is under (or is) one of `roots`, both already canonical.
pub fn path_under_worktree_roots(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

pub fn rebuild_worktree_db(
    db: &crate::db::WorktreeDb,
    echo_agent_home: &Path,
) -> anyhow::Result<RebuildReport> {
    // Same XDG/HOME candidates as pin-GC / marker lookup : not env-only.
    rebuild_worktree_db_from_grove_dirs(db, echo_agent_home, &crate::nfs::candidate_data_dirs())
}

/// Rebuild with an explicit grove data dir (daemon.db / mounts.toml / markers).
/// `None` skips the NFS union pass (tests).
pub fn rebuild_worktree_db_with_grove_data(
    db: &crate::db::WorktreeDb,
    echo_agent_home: &Path,
    grove_data_dir: Option<&Path>,
) -> anyhow::Result<RebuildReport> {
    match grove_data_dir {
        Some(dir) => rebuild_worktree_db_from_grove_dirs(db, echo_agent_home, &[dir.to_path_buf()]),
        None => rebuild_worktree_db_from_grove_dirs(db, echo_agent_home, &[]),
    }
}

fn rebuild_worktree_db_from_grove_dirs(
    db: &crate::db::WorktreeDb,
    echo_agent_home: &Path,
    grove_data_dirs: &[PathBuf],
) -> anyhow::Result<RebuildReport> {
    let mut report = RebuildReport::default();
    let now = now_epoch_secs();
    let roots = managed_worktree_roots(echo_agent_home);

    // Union grove identities before any managed-root walk. The dests we
    // learn here are skipped in discover_worktrees_skipping so is_dir /
    // .git / canonicalize never touch a wedged NFS mount. Registering NFS
    // first also keeps a grove dest from being labeled linked/standalone
    // (sweep_dead would then Path::exists the live mount).
    let mut seen = HashSet::new();
    let mut counted_nfs = HashSet::new();
    let recs = db.list(&crate::db::ListFilter {
        include_dead: true,
        ..Default::default()
    })?;
    let existing: Vec<crate::nfs::NfsIdentity> =
        crate::nfs::identities_from_worktree_records(&recs);
    // Union every grove data dir before writing metadata. A leftover
    // ~/.echo-agent/grove marker must not rewrite backing/source_pin alone and
    // outrank the live XDG identity (pin-GC already unions first).
    let mut by_id: HashMap<String, crate::nfs::NfsIdentity> = HashMap::new();
    for data_dir in grove_data_dirs {
        if data_dir.as_os_str().is_empty() || !seen.insert(data_dir.clone()) {
            continue;
        }
        crate::nfs::merge_nfs_identities(
            &mut by_id,
            crate::nfs::collect_identities(data_dir, &existing).into_values(),
        );
    }
    let skip_dests = register_nfs_from_union(db, by_id, now, &mut report, &mut counted_nfs, &recs)?;

    let discovery = discover_worktrees_skipping(echo_agent_home, &skip_dests);
    report.discovered += discovery.found.len() as u64;
    for wt in discovery.found {
        let path = dunce::canonicalize(&wt.path).unwrap_or_else(|_| wt.path.clone());
        // Refuse symlink escape outside managed roots.
        if !path_under_worktree_roots(&path, &roots) {
            tracing::warn!(
                path = %path.display(),
                "rebuild skipped path outside echoagent worktrees/worktree_pool"
            );
            continue;
        }
        let id = id_from_path(&path);
        let path_str = path.to_string_lossy();
        if db.get_by_id(&id)?.is_some() || db.get(&path_str)?.is_some() {
            report.already_tracked += 1;
            continue;
        }
        let mut rec = wt.into_record();
        // Touch so same-pass age GC does not reclaim solely from old FS mtime.
        rec.last_accessed_at = Some(now);
        db.register(&rec)?;
        report.registered += 1;
    }

    Ok(report)
}

fn register_nfs_from_union(
    db: &crate::db::WorktreeDb,
    by_id: HashMap<String, crate::nfs::NfsIdentity>,
    now: i64,
    report: &mut RebuildReport,
    counted: &mut HashSet<String>,
    recs: &[crate::db::WorktreeRecord],
) -> anyhow::Result<Vec<PathBuf>> {
    for id in by_id.keys() {
        if counted.insert(id.clone()) {
            report.discovered += 1;
        }
    }
    let mut ordered: Vec<_> = by_id.into_iter().collect();
    // HashMap order would let a stale lower-rank marker claim dest first and
    // permanently skip the live identity. Highest rank first; id tie-break.
    ordered.sort_by(|a, b| b.1.rank.cmp(&a.1.rank).then_with(|| a.0.cmp(&b.0)));
    let mut skip_dests: Vec<PathBuf> = Vec::new();
    // Hang-avoidance skips (aborted / missing backing) stay in skip_dests so
    // FS rediscovery does not poke a wedged mount, but they are not claims.
    // dest_taken must ignore them or a rank-3 aborted journal blocks a live
    // marker/mounts identity at the same dest.
    let mut claimed_dests: Vec<PathBuf> = Vec::new();
    for (id, idn) in ordered {
        if let Some(dest) = idn
            .dest
            .as_ref()
            .filter(|p| !p.as_os_str().is_empty() && p.as_path() != Path::new("unknown"))
        {
            let phys = physical_nfs_dest(dest.clone());
            let dest_taken = claimed_dests
                .iter()
                .any(|s| crate::nfs::dest_paths_equivalent(s, &phys));
            // A different identity may have claimed dest first. Still
            // refresh this id's own DB row from higher-rank sources.
            if dest_taken && db.get_by_id(&id)?.is_none() {
                report.already_tracked += 1;
                continue;
            }
        }
        if idn.phase.as_deref() == Some("aborted") {
            // Journal aborted, but a wedged mount can still be at dest
            // (marker/mounts already cleared). Skip only if it is a mount.
            if let Some(dest) = idn
                .dest
                .as_ref()
                .filter(|p| !p.as_os_str().is_empty() && p.as_path() != Path::new("unknown"))
            {
                skip_if_grove_mount(dest, &mut skip_dests);
            }
            continue;
        }
        if let Some(mut rec) = db.get_by_id(&id)? {
            if crate::worktree::is_grove_strategy(&rec.creation_mode) {
                if idn.rank > crate::nfs::RANK_DB {
                    rec.metadata = Some(merge_nfs_metadata(rec.metadata.take(), &idn));
                    db.register(&rec)?;
                }
                if let Some(dest) = idn
                    .dest
                    .as_ref()
                    .filter(|p| !p.as_os_str().is_empty() && p.as_path() != Path::new("unknown"))
                {
                    claim_nfs_dest(
                        physical_nfs_dest(dest.clone()),
                        &mut skip_dests,
                        &mut claimed_dests,
                    );
                }
            } else if let Some(dest) = idn
                .dest
                .as_ref()
                .filter(|p| !p.as_os_str().is_empty() && p.as_path() != Path::new("unknown"))
            {
                // Same path-hash id as a copy/linked row: skip dest so FS
                // rediscovery and GC never exists()/try_nfs_remove it.
                claim_nfs_dest(
                    physical_nfs_dest(dest.clone()),
                    &mut skip_dests,
                    &mut claimed_dests,
                );
            }
            report.already_tracked += 1;
            continue;
        }
        let Some(dest) = idn
            .dest
            .clone()
            .filter(|p| !p.as_os_str().is_empty() && p != Path::new("unknown"))
        else {
            tracing::warn!(id, "rebuild skipped NFS identity with no dest");
            continue;
        };
        let dest = physical_nfs_dest(dest);
        // Lexical match only : db.get canonicalize() hangs on wedged NFS.
        if recs
            .iter()
            .any(|r| crate::nfs::dest_paths_equivalent(&r.path, &dest))
        {
            // Dest already registered under another id (nfs or linked/copy).
            // Never overlay this identity's backing/source_pin (stale marker
            // would make dead-NFS GC drop the live pin) and never flip a
            // linked/copy row to nfs. Always skip dest so FS rediscovery and
            // GC cannot exists()/try_nfs_remove a live grove tree.
            claim_nfs_dest(dest, &mut skip_dests, &mut claimed_dests);
            report.already_tracked += 1;
            continue;
        }
        let source = idn
            .source_repo
            .clone()
            .unwrap_or_else(|| PathBuf::from("unknown"));
        if idn
            .backing
            .as_ref()
            .is_none_or(|b| b.as_os_str().is_empty())
        {
            tracing::warn!(id, "rebuild skipped NFS identity with empty backing");
            // In-flight mkdir dest must be skipped even if currently unmounted.
            claim_nfs_dest(dest, &mut skip_dests, &mut claimed_dests);
            continue;
        }
        if idn.backing.as_ref().is_some_and(|b| !b.exists()) {
            tracing::warn!(id, "rebuild skipped NFS identity with missing backing");
            skip_if_grove_mount(&dest, &mut skip_dests);
            continue;
        }
        let rec = crate::db::WorktreeRecord {
            id,
            path: dest,
            repo_name: repo_name_from_path(&source),
            source_repo: source,
            kind: WorktreeKind::Session,
            creation_mode: grove_mode_for_identity(&idn).into(),
            git_ref: None,
            head_commit: None,
            session_id: None,
            creator_pid: None,
            created_at: now,
            last_accessed_at: Some(now),
            status: WorktreeStatus::Alive,
            metadata: Some(grove_metadata_from_identity(&idn)),
        };
        claim_nfs_dest(rec.path.clone(), &mut skip_dests, &mut claimed_dests);
        db.register(&rec)?;
        report.registered += 1;
    }
    Ok(skip_dests)
}

fn claim_nfs_dest(dest: PathBuf, skip_dests: &mut Vec<PathBuf>, claimed_dests: &mut Vec<PathBuf>) {
    if !skip_dests
        .iter()
        .any(|s| crate::nfs::dest_paths_equivalent(s, &dest))
    {
        skip_dests.push(dest.clone());
    }
    if !claimed_dests
        .iter()
        .any(|s| crate::nfs::dest_paths_equivalent(s, &dest))
    {
        claimed_dests.push(dest);
    }
}

fn skip_if_grove_mount(dest: &Path, skip_dests: &mut Vec<PathBuf>) {
    if crate::nfs::dest_is_nfs_mount(dest)
        || crate::nfs::dest_is_mountpoint(dest)
        || !crate::nfs::dest_is_known_unmounted(dest)
    {
        let dest = physical_nfs_dest(dest.to_path_buf());
        if !skip_dests
            .iter()
            .any(|s| crate::nfs::dest_paths_equivalent(s, &dest))
        {
            skip_dests.push(dest);
        }
    }
}

/// Lexical dest rewrite only : never canonicalize (wedged NFS hangs).
fn physical_nfs_dest(p: PathBuf) -> PathBuf {
    crate::worktree::plan::canonicalize_for_id(&p)
}

fn grove_mode_for_identity(idn: &crate::nfs::NfsIdentity) -> &'static str {
    if let Some(dest) = idn.dest.as_ref() {
        if crate::nfs::dest_is_nfs_mount(dest) {
            return crate::worktree::STRATEGY_GROVE_NFS;
        }
        if crate::nfs::dest_is_projected_mount(dest) || crate::nfs::dest_is_mountpoint(dest) {
            return crate::worktree::STRATEGY_GROVE_FUSE;
        }
    }
    crate::nfs::default_grove_creation_mode()
}

fn grove_metadata_from_identity(idn: &crate::nfs::NfsIdentity) -> serde_json::Value {
    let transport = if grove_mode_for_identity(idn) == crate::worktree::STRATEGY_GROVE_FUSE {
        "fuse"
    } else {
        "nfs"
    };
    serde_json::json!({
        "grove": {
            "transport": transport,
            "mount_id": idn.mount_id,
            "backing": idn.backing.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
            "source_pin": idn.pin_ref.clone().unwrap_or_else(|| {
                format!("refs/echoagent/worktrees/{}", idn.worktree_id)
            }),
        }
    })
}

/// Overlay the grove object onto an existing metadata blob so create-time
/// keys (labels, strategy, …) survive a rebuild refresh of an already-tracked row.
fn merge_nfs_metadata(
    existing: Option<serde_json::Value>,
    idn: &crate::nfs::NfsIdentity,
) -> serde_json::Value {
    let grove = grove_metadata_from_identity(idn);
    match existing {
        Some(serde_json::Value::Object(mut map)) => {
            if let Some(grove_obj) = grove.get("grove") {
                map.insert("grove".into(), grove_obj.clone());
            }
            serde_json::Value::Object(map)
        }
        _ => grove,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_fake_linked_worktree(path: &Path, gitdir_target: &str) {
        std::fs::create_dir_all(path).unwrap();
        std::fs::write(path.join(".git"), format!("gitdir: {gitdir_target}\n")).unwrap();
    }

    fn make_fake_standalone_worktree(path: &Path) {
        std::fs::create_dir_all(path.join(".git")).unwrap();
    }

    #[test]
    fn discover_session_worktrees() {
        let tmp = tempfile::TempDir::new().unwrap();
        let echo_agent_home = tmp.path();

        let wt = echo_agent_home.join("worktrees/myrepo/worktree-abc123");
        make_fake_linked_worktree(&wt, "/repo/.git/worktrees/abc123");

        let report = discover_worktrees(echo_agent_home);
        assert_eq!(report.found.len(), 1);
        assert_eq!(report.found[0].kind, WorktreeKind::Session);
        assert_eq!(report.found[0].creation_mode, "linked");
        assert_eq!(report.found[0].path, wt);
    }

    #[test]
    fn discover_pool_worktrees() {
        let tmp = tempfile::TempDir::new().unwrap();
        let echo_agent_home = tmp.path();

        let wt = echo_agent_home.join("worktree_pool/inst-1/pool-a");
        make_fake_standalone_worktree(&wt);

        let report = discover_worktrees(echo_agent_home);
        assert_eq!(report.found.len(), 1);
        assert_eq!(report.found[0].kind, WorktreeKind::Pool);
        assert_eq!(report.found[0].creation_mode, "standalone");
    }

    #[test]
    fn skips_dot_prefixed_and_markers() {
        let tmp = tempfile::TempDir::new().unwrap();
        let echo_agent_home = tmp.path();

        let base = echo_agent_home.join("worktrees/myrepo");
        std::fs::create_dir_all(&base).unwrap();

        std::fs::create_dir_all(base.join(".tmp_creating")).unwrap();
        std::fs::create_dir_all(base.join(".hidden")).unwrap();
        std::fs::write(base.join("abc.ready"), "").unwrap();
        std::fs::write(base.join("abc.claimed"), "").unwrap();

        make_fake_standalone_worktree(&base.join("real-session"));

        let report = discover_worktrees(echo_agent_home);
        assert_eq!(report.found.len(), 1);
        assert_eq!(report.found[0].path, base.join("real-session"));
        assert!(report.skipped > 0);
    }

    #[test]
    fn discover_empty_dirs_is_fine() {
        let tmp = tempfile::TempDir::new().unwrap();
        let report = discover_worktrees(tmp.path());
        assert!(report.found.is_empty());
        assert_eq!(report.skipped, 0);
    }

    #[test]
    fn rebuild_registers_and_skips_duplicates() {
        let tmp = tempfile::TempDir::new().unwrap();
        let echo_agent_home = tmp.path();

        let wt = echo_agent_home.join("worktrees/repo/worktree-sess1");
        make_fake_standalone_worktree(&wt);

        let db = crate::db::WorktreeDb::open_in_memory().unwrap();

        let r1 = rebuild_worktree_db_with_grove_data(&db, echo_agent_home, None).unwrap();
        assert_eq!(r1.discovered, 1);
        assert_eq!(r1.registered, 1);
        assert_eq!(r1.already_tracked, 0);

        let r2 = rebuild_worktree_db_with_grove_data(&db, echo_agent_home, None).unwrap();
        assert_eq!(r2.discovered, 1);
        assert_eq!(r2.registered, 0);
        assert_eq!(r2.already_tracked, 1);
    }

    #[test]
    fn rebuild_keeps_same_basename_worktrees_in_different_repos() {
        // The cross-repo eviction bug: two repos each have a `wt-abc`
        // worktree. Discovery + rebuild must register BOTH (distinct ids), not
        // collapse them into one and then permanently skip the other.
        let tmp = tempfile::TempDir::new().unwrap();
        let echo_agent_home = tmp.path();

        let wt_a = echo_agent_home.join("worktrees/repo-a/wt-abc");
        let wt_b = echo_agent_home.join("worktrees/repo-b/wt-abc");
        make_fake_standalone_worktree(&wt_a);
        make_fake_standalone_worktree(&wt_b);

        let db = crate::db::WorktreeDb::open_in_memory().unwrap();
        let report = rebuild_worktree_db_with_grove_data(&db, echo_agent_home, None).unwrap();
        assert_eq!(report.discovered, 2);
        assert_eq!(
            report.registered, 2,
            "both same-basename worktrees must register"
        );

        let all = db.list(&crate::db::ListFilter::default()).unwrap();
        assert_eq!(all.len(), 2);
        assert!(db.get(&wt_a.to_string_lossy()).unwrap().is_some());
        assert!(db.get(&wt_b.to_string_lossy()).unwrap().is_some());

        // Idempotent: a second rebuild finds both already tracked, skips neither.
        let report2 = rebuild_worktree_db_with_grove_data(&db, echo_agent_home, None).unwrap();
        assert_eq!(report2.registered, 0);
        assert_eq!(report2.already_tracked, 2);
    }

    #[test]
    fn detect_source_repo_from_linked() {
        let tmp = tempfile::TempDir::new().unwrap();
        let wt = tmp.path().join("wt");
        let gitdir = "/home/user/myrepo/.git/worktrees/wt";
        make_fake_linked_worktree(&wt, gitdir);

        let source = detect_source_repo(&wt);
        assert_eq!(source, Some(PathBuf::from("/home/user/myrepo")));
    }

    #[test]
    fn rebuild_report_serde_round_trip() {
        let report = RebuildReport {
            discovered: 5,
            registered: 3,
            already_tracked: 2,
        };
        let json = serde_json::to_string(&report).unwrap();
        let deser: RebuildReport = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.discovered, 5);
        assert_eq!(deser.registered, 3);
        assert_eq!(deser.already_tracked, 2);
    }

    #[test]
    fn rebuild_nfs_under_managed_roots_is_not_labeled_linked() {
        let tmp = tempfile::TempDir::new().unwrap();
        let echo_agent_home = tmp.path().join("echoagent");
        let data = tmp.path().join("grove");
        let dest = echo_agent_home.join("worktrees/repo/nfs-sess");
        let local = echo_agent_home.join("worktrees/repo/local-sess");
        make_fake_standalone_worktree(&dest);
        make_fake_standalone_worktree(&local);
        let id = "nfs-wt-under-roots";
        let backing = data.join(crate::nfs::WORKTREE_BACKING_DIR).join(id);
        std::fs::create_dir_all(&backing).unwrap();
        let marker = serde_json::json!({
            "schema": 1,
            "worktree_id": id,
            "dest": dest,
            "source_repo": tmp.path().join("src-repo"),
            "pin_ref": format!("refs/echoagent/worktrees/{id}"),
            "mount_id": 3,
            "created_at": 9,
        });
        std::fs::write(
            backing.join("echo-agent-nfs-worktree.json"),
            serde_json::to_vec(&marker).unwrap(),
        )
        .unwrap();

        let db = crate::db::WorktreeDb::open_in_memory().unwrap();
        let report =
            rebuild_worktree_db_with_grove_data(&db, &echo_agent_home, Some(&data)).unwrap();
        assert_eq!(
            report.discovered, 2,
            "nfs identity + local fs row; must not also count the nfs dest via is_dir/.git"
        );
        let rec = db.get_by_id(id).unwrap().expect("nfs row");
        assert_eq!(
            rec.creation_mode,
            crate::nfs::default_grove_creation_mode(),
            "grove dest under managed roots must not be labeled linked from .git"
        );
        let local_rec = db
            .get(&local.to_string_lossy())
            .unwrap()
            .expect("local sibling");
        assert!(!crate::worktree::is_grove_strategy(
            &local_rec.creation_mode
        ));
        assert_eq!(
            db.list(&crate::db::ListFilter::default())
                .unwrap()
                .iter()
                .filter(|r| !crate::worktree::is_grove_strategy(&r.creation_mode))
                .count(),
            1,
            "only the local sibling is a non-grove row"
        );
    }

    #[test]
    fn discover_skips_known_nfs_dests_without_statting() {
        let tmp = tempfile::TempDir::new().unwrap();
        let echo_agent_home = tmp.path();
        let dest = echo_agent_home.join("worktrees/repo/nfs-sess");
        make_fake_standalone_worktree(&dest);
        assert_eq!(discover_worktrees(echo_agent_home).found.len(), 1);
        let skipped = discover_worktrees_skipping(echo_agent_home, std::slice::from_ref(&dest));
        assert!(
            skipped.found.is_empty(),
            "skip must be lexical, before is_dir"
        );
        assert!(skipped.skipped > 0);
    }

    #[test]
    fn rebuild_registers_nfs_from_backing_marker() {
        let tmp = tempfile::TempDir::new().unwrap();
        let echo_agent_home = tmp.path().join("echoagent");
        let data = tmp.path().join("grove");
        std::fs::create_dir_all(echo_agent_home.join("worktrees")).unwrap();
        // Dest is outside managed roots so FS discovery does not register a
        // competing linked/unknown row under a different id.
        let dest = tmp.path().join("nfs-dest");
        std::fs::create_dir_all(&dest).unwrap();
        let id = "nfs-wt-rebuild";
        let backing = data.join(crate::nfs::WORKTREE_BACKING_DIR).join(id);
        std::fs::create_dir_all(&backing).unwrap();
        let marker = serde_json::json!({
            "schema": 1,
            "worktree_id": id,
            "dest": dest,
            "source_repo": tmp.path().join("src-repo"),
            "pin_ref": format!("refs/echoagent/worktrees/{id}"),
            "mount_id": 42,
            "created_at": 9,
        });
        std::fs::write(
            backing.join("echo-agent-nfs-worktree.json"),
            serde_json::to_vec(&marker).unwrap(),
        )
        .unwrap();

        let db = crate::db::WorktreeDb::open_in_memory().unwrap();
        let report =
            rebuild_worktree_db_with_grove_data(&db, &echo_agent_home, Some(&data)).unwrap();
        assert!(report.registered >= 1);
        let rec = db.get_by_id(id).unwrap().expect("nfs row");
        assert_eq!(rec.creation_mode, crate::nfs::default_grove_creation_mode());
        assert_eq!(
            rec.metadata
                .as_ref()
                .unwrap()
                .get("grove")
                .unwrap()
                .get("mount_id")
                .unwrap()
                .as_i64(),
            Some(42)
        );
    }

    #[test]
    fn rebuild_dest_equivalent_does_not_overwrite_live_nfs_metadata() {
        let tmp = tempfile::TempDir::new().unwrap();
        let echo_agent_home = tmp.path().join("echoagent");
        let data = tmp.path().join("grove");
        std::fs::create_dir_all(echo_agent_home.join("worktrees")).unwrap();
        let dest = tmp.path().join("shared-dest");
        std::fs::create_dir_all(&dest).unwrap();
        let live_id = "live-nfs";
        let stale_id = "stale-marker";
        let live_backing = data.join(crate::nfs::WORKTREE_BACKING_DIR).join(live_id);
        let stale_backing = data.join(crate::nfs::WORKTREE_BACKING_DIR).join(stale_id);
        std::fs::create_dir_all(&live_backing).unwrap();
        std::fs::create_dir_all(&stale_backing).unwrap();
        std::fs::write(
            stale_backing.join("echo-agent-nfs-worktree.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema": 1,
                "worktree_id": stale_id,
                "dest": dest,
                "source_repo": tmp.path().join("src-repo"),
                "pin_ref": format!("refs/echoagent/worktrees/{stale_id}"),
                "mount_id": 99,
                "created_at": 1,
            }))
            .unwrap(),
        )
        .unwrap();

        let db = crate::db::WorktreeDb::open_in_memory().unwrap();
        let rec = crate::db::WorktreeRecord {
            id: live_id.into(),
            path: dest.clone(),
            repo_name: "src".into(),
            source_repo: tmp.path().join("src-repo"),
            kind: WorktreeKind::Session,
            creation_mode: "nfs".into(),
            git_ref: None,
            head_commit: None,
            session_id: None,
            creator_pid: None,
            created_at: 1,
            last_accessed_at: Some(1),
            status: WorktreeStatus::Alive,
            metadata: Some(serde_json::json!({
                "nfs": {
                    "mount_id": 1,
                    "backing": live_backing.display().to_string(),
                    "source_pin": format!("refs/echoagent/worktrees/{live_id}"),
                }
            })),
        };
        db.register(&rec).unwrap();

        rebuild_worktree_db_with_grove_data(&db, &echo_agent_home, Some(&data)).unwrap();
        let kept = db.get_by_id(live_id).unwrap().expect("live row");
        let nfs = kept.metadata.as_ref().unwrap().get("nfs").unwrap();
        assert_eq!(
            nfs.get("backing").and_then(|b| b.as_str()),
            Some(live_backing.display().to_string()).as_deref(),
            "stale dest-equivalent marker must not overwrite live backing"
        );
        assert_eq!(
            nfs.get("source_pin").and_then(|b| b.as_str()),
            Some(format!("refs/echoagent/worktrees/{live_id}")).as_deref()
        );
        assert!(
            db.get_by_id(stale_id).unwrap().is_none(),
            "stale marker must not replace the live dest row"
        );
    }

    #[test]
    fn rebuild_sets_last_accessed_at() {
        let tmp = tempfile::TempDir::new().unwrap();
        let echo_agent_home = tmp.path();
        let wt = echo_agent_home.join("worktrees/repo/sess");
        make_fake_standalone_worktree(&wt);
        let db = crate::db::WorktreeDb::open_in_memory().unwrap();
        rebuild_worktree_db_with_grove_data(&db, echo_agent_home, None).unwrap();
        let rec = db.get(&wt.to_string_lossy()).unwrap().expect("registered");
        assert!(
            rec.last_accessed_at.is_some(),
            "rebuild must touch last_accessed_at for same-pass age safety"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rebuild_skips_symlink_escape_outside_managed_roots() {
        let tmp = tempfile::TempDir::new().unwrap();
        let echo_agent_home = tmp.path().join("echoagent");
        let outside = tmp.path().join("outside-real");
        make_fake_standalone_worktree(&outside);
        let link_parent = echo_agent_home.join("worktrees/repo");
        std::fs::create_dir_all(&link_parent).unwrap();
        std::os::unix::fs::symlink(&outside, link_parent.join("escaped")).unwrap();

        let db = crate::db::WorktreeDb::open_in_memory().unwrap();
        let report = rebuild_worktree_db_with_grove_data(&db, &echo_agent_home, None).unwrap();
        assert_eq!(report.discovered, 1);
        assert_eq!(report.registered, 0, "symlink escape must not register");
        assert!(
            db.list(&crate::db::ListFilter::default())
                .unwrap()
                .is_empty()
        );
        assert!(!path_under_managed_worktree_roots(
            &dunce::canonicalize(&outside).unwrap(),
            &echo_agent_home
        ));
    }

    #[test]
    fn rebuild_scans_xdg_grove_without_grove_data_dir() {
        let mut fx = crate::db::EchoAgentHomeFixture::new();
        let grove = fx.isolate_xdg_grove_data();
        assert!(
            std::env::var_os("GROVE_DATA_DIR").is_none(),
            "production path must not rely on GROVE_DATA_DIR"
        );
        let echo_agent_home = fx.home.clone();
        std::fs::create_dir_all(echo_agent_home.join("worktrees")).unwrap();
        let dest = echo_agent_home.parent().unwrap().join("nfs-xdg-dest");
        std::fs::create_dir_all(&dest).unwrap();
        let id = "nfs-wt-xdg";
        let backing = grove.join(crate::nfs::WORKTREE_BACKING_DIR).join(id);
        std::fs::create_dir_all(&backing).unwrap();
        let marker = serde_json::json!({
            "schema": 1,
            "worktree_id": id,
            "dest": dest,
            "source_repo": echo_agent_home.join("src"),
            "pin_ref": format!("refs/echoagent/worktrees/{id}"),
            "mount_id": 7,
            "created_at": 1,
        });
        std::fs::write(
            backing.join("echo-agent-nfs-worktree.json"),
            serde_json::to_vec(&marker).unwrap(),
        )
        .unwrap();

        let db = crate::db::WorktreeDb::open_in_memory().unwrap();
        let report = rebuild_worktree_db(&db, &echo_agent_home).unwrap();
        assert!(report.registered >= 1, "{report:?}");
        let rec = db.get_by_id(id).unwrap().expect("xdg nfs row");
        assert_eq!(rec.creation_mode, crate::nfs::default_grove_creation_mode());
    }

    #[test]
    fn rebuild_skips_destless_nfs_identity() {
        let tmp = tempfile::TempDir::new().unwrap();
        let echo_agent_home = tmp.path().join("echoagent");
        std::fs::create_dir_all(echo_agent_home.join("worktrees")).unwrap();
        let data = tmp.path().join("grove");
        std::fs::create_dir_all(&data).unwrap();
        // mounts.toml worktree row with pin_ref id but no mountpoint.
        std::fs::write(
            data.join("mounts.toml"),
            "[[mounts]]\nkind = \"worktree\"\npin_ref = \"refs/echoagent/worktrees/no-dest\"\nbacking = \"/unused/worktree-backing/no-dest\"\n",
        )
        .unwrap();
        let db = crate::db::WorktreeDb::open_in_memory().unwrap();
        let report =
            rebuild_worktree_db_with_grove_data(&db, &echo_agent_home, Some(&data)).unwrap();
        assert!(
            db.get_by_id("no-dest").unwrap().is_none(),
            "dest-less identity must not register"
        );
        assert!(
            db.get("unknown").unwrap().is_none(),
            "must not insert path 'unknown'"
        );
        let _ = report;
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn physical_nfs_dest_strips_data_volume_firmlink() {
        assert_eq!(
            physical_nfs_dest(PathBuf::from("/System/Volumes/Data/Users/me/wt")),
            PathBuf::from("/Users/me/wt")
        );
        assert_eq!(
            physical_nfs_dest(PathBuf::from("/System/Volumes/Data/private/tmp/nfs-probe")),
            PathBuf::from("/private/tmp/nfs-probe")
        );
        assert_eq!(
            physical_nfs_dest(PathBuf::from("/tmp/nfs-probe")),
            PathBuf::from("/private/tmp/nfs-probe")
        );
    }
}
