//! Union-liveness for NFS pin-ref GC and discovery rebuild.
//!
//! A pin is live if **any** of daemon.db / mounts.toml / backing markers /
//! worktrees.db names its id (except aborted journal rows). GC never trusts
//! worktrees.db alone. Pin deletion always goes through [`grove_git::delete_pin_ref`]
//! (id → `refs/echoagent/worktrees/<id>` only).
use super::confined::is_safe_worktree_id;
use super::mount_table::{dest_is_mountpoint, dest_is_nfs_mount};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;
pub const BACKING_MARKER_FILE: &str = "echo-agent-nfs-worktree.json";
pub const WORKTREE_BACKING_DIR: &str = "worktree-backing";
pub const PIN_GC_GRACE_SECS: i64 = 24 * 60 * 60;
const PIN_GC_MIN_CYCLES: u32 = 2;
const PIN_GC_STATE_FILE: &str = "pin_gc_orphans.json";
const MOUNTS_FILE: &str = "mounts.toml";
const DAEMON_DB_FILE: &str = "daemon.db";
const MAX_MARKER_BYTES: u64 = 64 * 1024;
/// Grove data dirs production actually uses: env override, then XDG, then HOME.
/// Deduped: `XDG_DATA_HOME=$HOME/.local/share` would otherwise visit the same
/// grove dir twice and burn pin-GC grace cycles in one pass.
#[must_use]
pub fn candidate_data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut push = |p: PathBuf| {
        if !dirs.iter().any(|d| d == &p) {
            dirs.push(p);
        }
    };
    if let Ok(p) = std::env::var("GROVE_DATA_DIR") {
        push(PathBuf::from(p));
    }
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        push(PathBuf::from(xdg).join("grove"));
    }
    if let Ok(home) = std::env::var("HOME") {
        push(PathBuf::from(&home).join(".local/share/grove"));
    }
    if let Some(echo_agent_home) = echo_agent_home::resolve_echo_agent_home() {
        push(echo_agent_home.join("grove"));
    }
    dirs
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackingMarker {
    pub schema: u32,
    pub worktree_id: String,
    pub dest: PathBuf,
    pub source_repo: PathBuf,
    pub pin_ref: String,
    pub mount_id: i64,
    pub created_at: i64,
}
#[derive(Debug, Clone)]
pub struct NfsIdentity {
    pub worktree_id: String,
    pub dest: Option<PathBuf>,
    pub source_repo: Option<PathBuf>,
    pub pin_ref: Option<String>,
    pub backing: Option<PathBuf>,
    pub mount_id: Option<i64>,
    pub rank: u8,
    /// Journal phase when sourced from daemon.db; aborted is not live.
    pub phase: Option<String>,
}
/// Rank: daemon.db (3) > mounts.toml (2) > backing marker (1) > worktrees.db (0).
pub const RANK_DB: u8 = 0;
pub const RANK_MARKER: u8 = 1;
pub const RANK_MOUNTS: u8 = 2;
pub const RANK_DAEMON: u8 = 3;
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PinGcReport {
    pub examined: u64,
    pub pruned: u64,
    pub deferred_grace: u64,
    pub kept_live: u64,
    /// Worktree ids counted in `pruned`. Callers union these across grove
    /// data dirs so dry-run does not double-count a pin that is never deleted.
    #[serde(default)]
    pub pruned_ids: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PinGcState {
    orphans: HashMap<String, OrphanEntry>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct OrphanEntry {
    first_seen: i64,
    cycles: u32,
    source: PathBuf,
    pin_ref: String,
}
#[must_use]
pub fn nfs_record_is_dead(dest: &Path, backing: Option<&Path>) -> bool {
    if dest_is_nfs_mount(dest) || dest_is_mountpoint(dest) || !super::dest_is_known_unmounted(dest)
    {
        return false;
    }
    let backing = backing.filter(|b| !b.as_os_str().is_empty());
    match backing {
        Some(b) => !b.exists(),
        None => std::fs::symlink_metadata(dest).is_err(),
    }
}
/// Dirent name is the id. JSON `worktree_id` must equal that dirent.
pub fn marker_from_dirent(dirent: &str, bytes: &[u8]) -> Option<BackingMarker> {
    if !is_safe_worktree_id(dirent) {
        return None;
    }
    let m: BackingMarker = serde_json::from_slice(bytes).ok()?;
    if m.worktree_id != dirent {
        return None;
    }
    Some(m)
}
pub fn load_backing_markers(data_dir: &Path) -> Vec<BackingMarker> {
    let root = data_dir.join(WORKTREE_BACKING_DIR);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for ent in entries.flatten() {
        if !ent.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let dirent = ent.file_name().to_string_lossy().into_owned();
        let marker_path = ent.path().join(BACKING_MARKER_FILE);
        let Some(bytes) = read_marker_capped(&marker_path) else {
            continue;
        };
        if let Some(m) = marker_from_dirent(&dirent, &bytes) {
            out.push(m);
        }
    }
    out
}
fn identity_sources(data_dir: &Path, worktrees: &[NfsIdentity]) -> Vec<NfsIdentity> {
    let mut out = Vec::with_capacity(worktrees.len());
    out.extend_from_slice(worktrees);
    out.extend(identities_from_markers(data_dir));
    out.extend(identities_from_mounts_toml(data_dir));
    out.extend(identities_from_daemon_db(data_dir));
    out
}
pub fn collect_identities(
    data_dir: &Path,
    worktrees: &[NfsIdentity],
) -> HashMap<String, NfsIdentity> {
    let mut by_id: HashMap<String, NfsIdentity> = HashMap::new();
    merge_nfs_identities(&mut by_id, identity_sources(data_dir, worktrees));
    by_id
}
fn is_aborted(idn: &NfsIdentity) -> bool {
    idn.phase.as_deref() == Some("aborted")
}
fn dest_usable(dest: &Option<PathBuf>) -> bool {
    dest.as_ref()
        .is_some_and(|p| !p.as_os_str().is_empty() && p.as_path() != Path::new("unknown"))
}
/// Rank still prefers dest/mount_id among live sources. Aborted journal rows
/// never replace a live identity; missing fields are filled from the other.
pub fn merge_nfs_identities(
    into: &mut HashMap<String, NfsIdentity>,
    src: impl IntoIterator<Item = NfsIdentity>,
) {
    for idn in src {
        let id = idn.worktree_id.clone();
        let merged = match into.remove(&id) {
            None => idn,
            Some(prev) => merge_pair(prev, idn),
        };
        into.insert(id, merged);
    }
}
fn merge_pair(a: NfsIdentity, b: NfsIdentity) -> NfsIdentity {
    let a_aborted = is_aborted(&a);
    let b_aborted = is_aborted(&b);
    if b_aborted && !a_aborted {
        fill_missing(a, b)
    } else if a_aborted && !b_aborted {
        fill_missing(b, a)
    } else if a.rank > b.rank {
        fill_missing(a, b)
    } else if b.rank > a.rank {
        fill_missing(b, a)
    } else {
        let a_live = a.backing.as_ref().is_some_and(|p| p.exists());
        let b_live = b.backing.as_ref().is_some_and(|p| p.exists());
        if b_live && !a_live {
            fill_missing(b, a)
        } else {
            fill_missing(a, b)
        }
    }
}
fn fill_missing(mut win: NfsIdentity, lose: NfsIdentity) -> NfsIdentity {
    if !dest_usable(&win.dest) && dest_usable(&lose.dest) && !is_aborted(&lose) {
        win.dest = lose.dest;
    }
    if win.source_repo.is_none() {
        win.source_repo = lose.source_repo;
    }
    if win.pin_ref.is_none() {
        win.pin_ref = lose.pin_ref;
    }
    if win.backing.is_none() {
        win.backing = lose.backing;
    }
    if win.mount_id.is_none() {
        win.mount_id = lose.mount_id;
    }
    win
}
fn identities_from_markers(data_dir: &Path) -> Vec<NfsIdentity> {
    load_backing_markers(data_dir)
        .into_iter()
        .map(|m| NfsIdentity {
            worktree_id: m.worktree_id,
            dest: Some(m.dest),
            source_repo: Some(m.source_repo),
            pin_ref: Some(m.pin_ref),
            backing: Some(data_dir.join(WORKTREE_BACKING_DIR).join("")),
            mount_id: Some(m.mount_id),
            rank: RANK_MARKER,
            phase: None,
        })
        .map(|mut idn| {
            idn.backing = Some(data_dir.join(WORKTREE_BACKING_DIR).join(&idn.worktree_id));
            idn
        })
        .collect()
}
fn identities_from_mounts_toml(data_dir: &Path) -> Vec<NfsIdentity> {
    let text = match std::fs::read_to_string(data_dir.join(MOUNTS_FILE)) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    parse_worktree_mounts(&text)
}
/// Minimal `[[mounts]]` extract: only worktree-kind rows.
fn parse_worktree_mounts(text: &str) -> Vec<NfsIdentity> {
    let mut out = Vec::new();
    let mut cur: HashMap<String, String> = HashMap::new();
    let flush = |cur: &mut HashMap<String, String>, out: &mut Vec<NfsIdentity>| {
        if cur.is_empty() {
            return;
        }
        let kind = cur.get("kind").map(String::as_str).unwrap_or("store");
        if kind != "worktree" {
            cur.clear();
            return;
        }
        let backing = cur.get("backing").map(PathBuf::from);
        let id = backing
            .as_ref()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .or_else(|| {
                cur.get("pin_ref")
                    .and_then(|p| p.rsplit('/').next().map(str::to_owned))
            });
        if let Some(worktree_id) = id.filter(|s| is_safe_worktree_id(s)) {
            out.push(NfsIdentity {
                worktree_id,
                dest: cur.get("mountpoint").map(PathBuf::from),
                source_repo: cur.get("source_repo").map(PathBuf::from),
                pin_ref: cur.get("pin_ref").cloned(),
                backing,
                mount_id: cur.get("identity").and_then(|s| s.parse().ok()),
                rank: RANK_MOUNTS,
                phase: None,
            });
        }
        cur.clear();
    };
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("[[") {
            flush(&mut cur, &mut out);
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim().to_owned();
            let v = v.trim().trim_matches('"').to_owned();
            cur.insert(k, v);
        }
    }
    flush(&mut cur, &mut out);
    out
}
fn identities_from_daemon_db(data_dir: &Path) -> Vec<NfsIdentity> {
    #[cfg(not(feature = "metadata"))]
    {
        let _ = data_dir;
        Vec::new()
    }
    #[cfg(feature = "metadata")]
    {
        let path = data_dir.join(DAEMON_DB_FILE);
        if !path.exists() {
            return Vec::new();
        }
        let conn = match rusqlite::Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let mut out = Vec::new();
        if let Ok(mut stmt) =
            conn.prepare("SELECT worktree_id, dest, source, phase FROM wt_create_state")
            && let Ok(rows) = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
        {
            for row in rows.flatten() {
                if !is_safe_worktree_id(&row.0) {
                    continue;
                }
                out.push(NfsIdentity {
                    worktree_id: row.0,
                    dest: Some(PathBuf::from(row.1)),
                    source_repo: Some(PathBuf::from(row.2)),
                    pin_ref: None,
                    backing: None,
                    mount_id: None,
                    rank: RANK_DAEMON,
                    phase: Some(row.3),
                });
            }
        }
        if let Ok(mut stmt) =
            conn.prepare("SELECT backing, mountpoint, source, mount_id FROM nfs_mounts")
            && let Ok(rows) = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })
        {
            for row in rows.flatten() {
                let backing = PathBuf::from(row.0);
                let Some(id) = backing
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .filter(|s| is_safe_worktree_id(s))
                else {
                    continue;
                };
                out.push(NfsIdentity {
                    worktree_id: id,
                    dest: Some(PathBuf::from(row.1)),
                    source_repo: row.2.map(PathBuf::from),
                    pin_ref: None,
                    backing: Some(backing),
                    mount_id: Some(row.3),
                    rank: RANK_DAEMON,
                    phase: None,
                });
            }
        }
        out
    }
}
/// Ids that must keep their pin (union liveness). Any non-aborted source keeps
/// the id alive; an aborted journal row does not add liveness and cannot
/// mask a marker, `mounts.toml` row, or worktrees.db row.
pub fn collect_live_nfs_ids(data_dir: &Path, worktrees: &[NfsIdentity]) -> HashSet<String> {
    identity_sources(data_dir, worktrees)
        .into_iter()
        .filter(|idn| !is_aborted(idn))
        .map(|idn| idn.worktree_id)
        .collect()
}
pub fn gc_orphan_pins(
    data_dir: &Path,
    worktrees: &[NfsIdentity],
    now: i64,
    dry_run: bool,
) -> Result<PinGcReport> {
    let identities = collect_identities(data_dir, worktrees);
    let live = collect_live_nfs_ids(data_dir, worktrees);
    let mut state = load_pin_gc_state(data_dir);
    let mut report = PinGcReport::default();
    let mut candidates: HashMap<String, (PathBuf, String, Option<PathBuf>, Option<PathBuf>)> =
        HashMap::new();
    for idn in identities.values() {
        if !is_safe_worktree_id(&idn.worktree_id) {
            continue;
        }
        let pin = format!("refs/echoagent/worktrees/{}", idn.worktree_id);
        if let Some(src) = &idn.source_repo {
            candidates.insert(
                idn.worktree_id.clone(),
                (src.clone(), pin, idn.dest.clone(), idn.backing.clone()),
            );
        }
    }
    for (id, ent) in &state.orphans {
        if !is_safe_worktree_id(id) {
            continue;
        }
        let pin = format!("refs/echoagent/worktrees/{id}");
        candidates
            .entry(id.clone())
            .or_insert_with(|| (ent.source.clone(), pin, None, None));
    }
    let mut still_orphan: HashSet<String> = HashSet::new();
    for (id, (source, pin_ref, dest, backing)) in candidates {
        report.examined += 1;
        if live.contains(&id) {
            report.kept_live += 1;
            state.orphans.remove(&id);
            continue;
        }
        if dest.as_ref().is_some_and(|d| dest_is_mountpoint(d)) {
            report.kept_live += 1;
            state.orphans.remove(&id);
            continue;
        }
        if backing.as_ref().is_some_and(|b| b.exists()) {
            report.kept_live += 1;
            state.orphans.remove(&id);
            continue;
        }
        match pin_exists(&source, &id) {
            Ok(false) => {
                state.orphans.remove(&id);
                continue;
            }
            Ok(true) => {}
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    id,
                    "pin_exists failed; aging orphan toward delete_pin_ref"
                );
            }
        }
        still_orphan.insert(id.clone());
        let entry = state
            .orphans
            .entry(id.clone())
            .or_insert_with(|| OrphanEntry {
                first_seen: now,
                cycles: 0,
                source: source.clone(),
                pin_ref,
            });
        entry.cycles = entry.cycles.saturating_add(1);
        let aged = now.saturating_sub(entry.first_seen) >= PIN_GC_GRACE_SECS;
        if entry.cycles >= PIN_GC_MIN_CYCLES && aged {
            if dry_run {
                report.pruned += 1;
                report.pruned_ids.push(id.clone());
                state.orphans.remove(&id);
            } else if let Err(e) = delete_pin_ref_gated(&source, &id) {
                tracing::warn!(
                    id = %id,
                    error = %e,
                    "pin GC: delete failed; keeping orphan for later cycles"
                );
                report.deferred_grace += 1;
            } else {
                report.pruned += 1;
                report.pruned_ids.push(id.clone());
                state.orphans.remove(&id);
            }
        } else {
            report.deferred_grace += 1;
        }
    }
    state
        .orphans
        .retain(|id, _| still_orphan.contains(id) || live.contains(id));
    if !dry_run {
        save_pin_gc_state(data_dir, &state)?;
    }
    Ok(report)
}
fn load_pin_gc_state(data_dir: &Path) -> PinGcState {
    let path = data_dir.join(PIN_GC_STATE_FILE);
    let Ok(bytes) = std::fs::read(&path) else {
        return PinGcState::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}
fn save_pin_gc_state(data_dir: &Path, state: &PinGcState) -> Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let path = data_dir.join(PIN_GC_STATE_FILE);
    let json = serde_json::to_vec_pretty(state)?;
    let mut tmp = NamedTempFile::new_in(data_dir)?;
    tmp.write_all(&json)?;
    tmp.as_file().sync_all()?;
    tmp.persist(&path)?;
    Ok(())
}
fn read_marker_capped(path: &Path) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    Read::take(file, MAX_MARKER_BYTES.saturating_add(1))
        .read_to_end(&mut buf)
        .ok()?;
    if buf.len() as u64 > MAX_MARKER_BYTES {
        return None;
    }
    Some(buf)
}
fn pin_exists(source: &Path, worktree_id: &str) -> Result<bool> {
    {
        let _ = (source, worktree_id);
        Ok(false)
    }
}
fn delete_pin_ref_gated(source: &Path, worktree_id: &str) -> Result<()> {
    {
        let _ = (source, worktree_id);
        anyhow::bail!("pin delete requires grove")
    }
}
#[cfg(feature = "metadata")]
pub fn identities_from_worktree_records(recs: &[crate::db::WorktreeRecord]) -> Vec<NfsIdentity> {
    recs.iter()
        .filter(|r| crate::worktree::is_grove_strategy(&r.creation_mode))
        .map(|r| {
            let grove = r
                .metadata
                .as_ref()
                .and_then(|m| m.get("grove").or_else(|| m.get("nfs")));
            let backing = grove
                .and_then(|n| n.get("backing"))
                .and_then(|b| b.as_str())
                .filter(|s| !s.is_empty())
                .map(PathBuf::from);
            let pin = grove
                .and_then(|n| n.get("source_pin"))
                .and_then(|b| b.as_str())
                .map(str::to_owned);
            NfsIdentity {
                worktree_id: r.id.clone(),
                dest: Some(r.path.clone()),
                source_repo: Some(r.source_repo.clone()),
                pin_ref: pin,
                backing,
                mount_id: None,
                rank: RANK_DB,
                phase: None,
            }
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_test_utils::git::{git_commit_all, init_git_repo};
    use tempfile::TempDir;
    fn git_rev_parse(repo: &Path, rev: &str) -> String {
        let mut cmd = std::process::Command::new("git");
        echo_agent_tty_utils::detach_std_command(&mut cmd);
        let out = cmd
            .current_dir(repo)
            .args(["rev-parse", rev])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }
    fn write_marker(data: &Path, m: &BackingMarker) {
        let dir = data.join(WORKTREE_BACKING_DIR).join(&m.worktree_id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(BACKING_MARKER_FILE),
            serde_json::to_vec(m).unwrap(),
        )
        .unwrap();
    }
    #[test]
    fn oversized_marker_is_skipped() {
        let tmp = TempDir::new().unwrap();
        let data = tmp.path();
        let dir = data.join(WORKTREE_BACKING_DIR).join("wt-big");
        std::fs::create_dir_all(&dir).unwrap();
        let mut huge = br#"{"schema":1,"worktree_id":"wt-big","pad":""#.to_vec();
        huge.extend(std::iter::repeat_n(b'x', (MAX_MARKER_BYTES as usize) + 8));
        huge.extend_from_slice(br#""}"#);
        std::fs::write(dir.join(BACKING_MARKER_FILE), huge).unwrap();
        assert!(
            load_backing_markers(data).is_empty(),
            "oversized marker must not be slurped"
        );
    }
    #[cfg(feature = "metadata")]
    fn write_create_state(data: &Path, id: &str, phase: &str, dest: &str, source: &Path) {
        std::fs::create_dir_all(data).unwrap();
        let conn = rusqlite::Connection::open(data.join(DAEMON_DB_FILE)).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS wt_create_state (
                worktree_id TEXT PRIMARY KEY,
                phase TEXT NOT NULL,
                dest TEXT NOT NULL,
                source TEXT NOT NULL,
                orphan_seen_at INTEGER,
                updated_at INTEGER NOT NULL
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO wt_create_state(worktree_id, phase, dest, source, updated_at)
             VALUES (?1, ?2, ?3, ?4, 1)",
            rusqlite::params![id, phase, dest, source.display().to_string()],
        )
        .unwrap();
    }
    #[cfg(feature = "metadata")]
    #[test]
    fn identities_prefer_metadata_grove_over_legacy_nfs() {
        let mut rec = crate::test_support::worktree_record("wt-grove", "/tmp/wt-grove");
        rec.creation_mode = "grove-fuse".into();
        rec.metadata = Some(serde_json::json!({
            "grove": {
                "backing": "/data/grove/worktree-backing/wt-grove",
                "source_pin": "refs/echoagent/worktrees/wt-grove"
            },
            "nfs": {
                "backing": "/legacy/should-not-win",
                "source_pin": "refs/echoagent/worktrees/legacy"
            }
        }));
        let ids = identities_from_worktree_records(&[rec]);
        assert_eq!(ids.len(), 1);
        assert_eq!(
            ids[0].backing.as_deref(),
            Some(Path::new("/data/grove/worktree-backing/wt-grove"))
        );
        assert_eq!(
            ids[0].pin_ref.as_deref(),
            Some("refs/echoagent/worktrees/wt-grove")
        );
    }
    #[test]
    fn empty_backing_is_not_dead_while_dest_exists() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("still-here");
        std::fs::create_dir(&dest).unwrap();
        assert!(!nfs_record_is_dead(&dest, Some(Path::new(""))));
        assert!(!nfs_record_is_dead(&dest, None));
        let gone = tmp.path().join("gone");
        assert!(nfs_record_is_dead(&gone, None));
        assert!(nfs_record_is_dead(&gone, Some(Path::new(""))));
        let backing = tmp.path().join("worktree-backing").join("wt");
        assert!(nfs_record_is_dead(&dest, Some(&backing)));
        std::fs::create_dir_all(&backing).unwrap();
        assert!(!nfs_record_is_dead(&dest, Some(&backing)));
    }
    #[test]
    fn db_loss_then_source_gc_keeps_pin_via_union_liveness() {
        echo_agent_test_utils::require_git!();
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_git_repo(&repo);
        std::fs::write(repo.join("keep.txt"), "head").unwrap();
        git_commit_all(&repo, "head");
        std::fs::write(repo.join("orphan.txt"), "unique-orphan-blob").unwrap();
        git_commit_all(&repo, "orphan");
        let orphan = git_rev_parse(&repo, "HEAD");
        let mut reset = std::process::Command::new("git");
        echo_agent_tty_utils::detach_std_command(&mut reset);
        assert!(
            reset
                .current_dir(&repo)
                .args(["reset", "--hard", "HEAD~1"])
                .status()
                .unwrap()
                .success()
        );
        let pin = "refs/echoagent/worktrees/wt-live";
        let mut uref = std::process::Command::new("git");
        echo_agent_tty_utils::detach_std_command(&mut uref);
        assert!(
            uref.current_dir(&repo)
                .args(["update-ref", pin, &orphan])
                .status()
                .unwrap()
                .success()
        );
        let data = tmp.path().join("grove-data");
        write_marker(
            &data,
            &BackingMarker {
                schema: 1,
                worktree_id: "wt-live".into(),
                dest: tmp.path().join("dest"),
                source_repo: repo.clone(),
                pin_ref: pin.into(),
                mount_id: 7,
                created_at: 1,
            },
        );
        std::fs::write(
                data.join(MOUNTS_FILE),
                format!(
                "[[mounts]]\nkind = \"worktree\"\nmountpoint = \"{}\"\nbacking = \"{}\"\npin_ref = \"{pin}\"\nsource_repo = \"{}\"\n",
                tmp.path().join("dest").display(),
                data.join(WORKTREE_BACKING_DIR).join("wt-live").display(),
                repo.display()
            ),
            )
            .unwrap();
        let report = gc_orphan_pins(&data, &[], 10, false).unwrap();
        assert_eq!(report.kept_live, 1, "union liveness must keep the pin");
        assert_eq!(report.pruned, 0);
        assert!(pin_exists(&repo, "wt-live").unwrap());
        let mut gc = std::process::Command::new("git");
        echo_agent_tty_utils::detach_std_command(&mut gc);
        assert!(
            gc.current_dir(&repo)
                .args(["gc", "--prune=now"])
                .status()
                .unwrap()
                .success()
        );
        let mut cat = std::process::Command::new("git");
        echo_agent_tty_utils::detach_std_command(&mut cat);
        let cat_out = cat
            .current_dir(&repo)
            .args(["cat-file", "-t", &orphan])
            .output()
            .unwrap();
        assert!(
            cat_out.status.success(),
            "orphaned commit must remain reachable through the pin after git gc"
        );
    }
    #[test]
    #[cfg(feature = "metadata")]
    fn aborted_partial_removal_prunes_after_grace() {
        echo_agent_test_utils::require_git!();
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_git_repo(&repo);
        std::fs::write(repo.join("f.txt"), "x").unwrap();
        git_commit_all(&repo, "c");
        let oid = git_rev_parse(&repo, "HEAD");
        let pin = "refs/echoagent/worktrees/wt-orphan";
        let mut uref = std::process::Command::new("git");
        echo_agent_tty_utils::detach_std_command(&mut uref);
        assert!(
            uref.current_dir(&repo)
                .args(["update-ref", pin, &oid])
                .status()
                .unwrap()
                .success()
        );
        let data = tmp.path().join("grove-data");
        write_create_state(&data, "wt-orphan", "aborted", "/gone", &repo);
        let t0 = 1_000;
        let r1 = gc_orphan_pins(&data, &[], t0, false).unwrap();
        assert_eq!(r1.pruned, 0);
        assert!(r1.deferred_grace >= 1);
        assert!(pin_exists(&repo, "wt-orphan").unwrap());
        let r2 = gc_orphan_pins(&data, &[], t0 + PIN_GC_GRACE_SECS + 1, false).unwrap();
        assert_eq!(r2.pruned, 1);
        assert!(!pin_exists(&repo, "wt-orphan").unwrap());
    }
    #[test]
    #[cfg(feature = "metadata")]
    fn in_flight_create_pin_survives_gc() {
        echo_agent_test_utils::require_git!();
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_git_repo(&repo);
        std::fs::write(repo.join("f.txt"), "x").unwrap();
        git_commit_all(&repo, "c");
        let oid = git_rev_parse(&repo, "HEAD");
        let pin = "refs/echoagent/worktrees/wt-fly";
        let mut uref = std::process::Command::new("git");
        echo_agent_tty_utils::detach_std_command(&mut uref);
        assert!(
            uref.current_dir(&repo)
                .args(["update-ref", pin, &oid])
                .status()
                .unwrap()
                .success()
        );
        let data = tmp.path().join("grove-data");
        write_create_state(&data, "wt-fly", "pinned", "/dest", &repo);
        let r = gc_orphan_pins(&data, &[], 10 + PIN_GC_GRACE_SECS, false).unwrap();
        assert_eq!(r.pruned, 0);
        assert!(pin_exists(&repo, "wt-fly").unwrap());
    }
    #[test]
    #[cfg(feature = "metadata")]
    fn aborted_journal_does_not_mask_marker_or_mounts_toml() {
        echo_agent_test_utils::require_git!();
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_git_repo(&repo);
        std::fs::write(repo.join("keep.txt"), "head").unwrap();
        git_commit_all(&repo, "head");
        std::fs::write(repo.join("orphan.txt"), "unique-aborted-mask-blob").unwrap();
        git_commit_all(&repo, "orphan");
        let orphan = git_rev_parse(&repo, "HEAD");
        let mut reset = std::process::Command::new("git");
        echo_agent_tty_utils::detach_std_command(&mut reset);
        assert!(
            reset
                .current_dir(&repo)
                .args(["reset", "--hard", "HEAD~1"])
                .status()
                .unwrap()
                .success()
        );
        let pin = "refs/echoagent/worktrees/wt-mask";
        let mut uref = std::process::Command::new("git");
        echo_agent_tty_utils::detach_std_command(&mut uref);
        assert!(
            uref.current_dir(&repo)
                .args(["update-ref", pin, &orphan])
                .status()
                .unwrap()
                .success()
        );
        let data = tmp.path().join("grove-data");
        write_create_state(&data, "wt-mask", "aborted", "/gone", &repo);
        write_marker(
            &data,
            &BackingMarker {
                schema: 1,
                worktree_id: "wt-mask".into(),
                dest: tmp.path().join("dest"),
                source_repo: repo.clone(),
                pin_ref: pin.into(),
                mount_id: 3,
                created_at: 1,
            },
        );
        std::fs::write(
                data.join(MOUNTS_FILE),
                format!(
                "[[mounts]]\nkind = \"worktree\"\nmountpoint = \"{}\"\nbacking = \"{}\"\npin_ref = \"{pin}\"\nsource_repo = \"{}\"\n",
                tmp.path().join("dest").display(),
                data.join(WORKTREE_BACKING_DIR).join("wt-mask").display(),
                repo.display()
            ),
            )
            .unwrap();
        let t0 = 1_000;
        let r1 = gc_orphan_pins(&data, &[], t0, false).unwrap();
        assert!(
            r1.kept_live >= 1,
            "aborted journal must not hide marker/mounts.toml: {r1:?}"
        );
        assert_eq!(r1.pruned, 0);
        assert!(pin_exists(&repo, "wt-mask").unwrap());
        let r2 = gc_orphan_pins(&data, &[], t0 + PIN_GC_GRACE_SECS + 1, false).unwrap();
        assert!(r2.kept_live >= 1, "still live after grace: {r2:?}");
        assert_eq!(r2.pruned, 0);
        assert!(pin_exists(&repo, "wt-mask").unwrap());
        let mut gc = std::process::Command::new("git");
        echo_agent_tty_utils::detach_std_command(&mut gc);
        assert!(
            gc.current_dir(&repo)
                .args(["gc", "--prune=now"])
                .status()
                .unwrap()
                .success()
        );
        let mut cat = std::process::Command::new("git");
        echo_agent_tty_utils::detach_std_command(&mut cat);
        let cat_out = cat
            .current_dir(&repo)
            .args(["cat-file", "-t", &orphan])
            .output()
            .unwrap();
        assert!(
            cat_out.status.success(),
            "pin must still protect the commit from source-side git gc"
        );
    }
    #[test]
    fn planted_orphan_state_does_not_delete_heads_main() {
        echo_agent_test_utils::require_git!();
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("gc-victim");
        std::fs::create_dir(&repo).unwrap();
        init_git_repo(&repo);
        std::fs::write(repo.join("tracked.txt"), "keep\n").unwrap();
        git_commit_all(&repo, "keep");
        let mut uref = std::process::Command::new("git");
        echo_agent_tty_utils::detach_std_command(&mut uref);
        assert!(
            uref.current_dir(&repo)
                .args(["update-ref", "refs/heads/main", "HEAD"])
                .status()
                .unwrap()
                .success()
        );
        let data = tmp.path().join("grove");
        std::fs::create_dir_all(&data).unwrap();
        let sidecar = serde_json::json!({
            "orphans": {
                "wt-planted": {
                    "first_seen": 0,
                    "cycles": 1,
                    "source": repo,
                    "pin_ref": "refs/heads/main"
                }
            }
        });
        std::fs::write(
            data.join(PIN_GC_STATE_FILE),
            serde_json::to_vec_pretty(&sidecar).unwrap(),
        )
        .unwrap();
        let report = gc_orphan_pins(&data, &[], 10 + PIN_GC_GRACE_SECS, false).unwrap();
        assert_eq!(
            report.pruned, 0,
            "foreign pin_ref in orphan state must not prune: {report:?}"
        );
        let mut check = std::process::Command::new("git");
        echo_agent_tty_utils::detach_std_command(&mut check);
        assert!(
            check
                .current_dir(&repo)
                .args(["rev-parse", "--verify", "--quiet", "refs/heads/main"])
                .status()
                .unwrap()
                .success(),
            "refs/heads/main must survive planted pin_gc_orphans.json"
        );
        assert_eq!(git_rev_parse(&repo, "HEAD").len(), 40);
    }
}
