//! NFS worktree removal: daemon-first, verified-unmount, then confined backing delete.
//!
//! Never `umount -f`. Unverifiable unmount retains backing + pin.
use super::NfsWorktreeOpts;
use super::client::NfsWorktreeClient;
use super::confined::is_safe_worktree_id;
use super::liveness::{BACKING_MARKER_FILE, BackingMarker};
use super::mount_table::{dest_is_mountpoint, dest_is_projected_mount};
use crate::RemoveReport;
use anyhow::Context;
use anyhow::{Result, bail};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
pub fn try_nfs_remove(worktree_path: &Path) -> Result<Option<RemoveReport>> {
    if !dest_is_mountpoint(worktree_path) && !super::dest_is_known_unmounted(worktree_path) {
        bail!(
            "mount table inconclusive for {}; refusing remove",
            worktree_path.display()
        );
    }
    let is_projected = dest_is_projected_mount(worktree_path);
    if is_projected {
        if lookup_from_markers(worktree_path).is_none() {
            bail!(
                "{} is a live grove mount without a backing marker; refusing rm -rf",
                worktree_path.display()
            );
        }
    } else if dest_is_mountpoint(worktree_path) || lookup_nfs_meta(worktree_path).is_none() {
        return Ok(None);
    }
    remove_nfs_worktree(worktree_path)
}
fn remove_nfs_worktree(worktree_path: &Path) -> Result<Option<RemoveReport>> {
    let opts = nfs_opts_from_env_and_meta(None);
    let client = NfsWorktreeClient::from_opts(&opts);
    if client.ping() {
        match client.remove_worktree(worktree_path, false) {
            Ok(()) => return report_after_daemon_unmount(worktree_path),
            Err(e) => {
                bail!("daemon RemoveWorktree failed: {e}");
            }
        }
    }
    if dest_is_mountpoint(worktree_path) {
        if lookup_from_markers(worktree_path).is_none() {
            bail!(
                "{} is still a mountpoint without a grove marker; refusing umount/rm",
                worktree_path.display()
            );
        }
        plain_umount(worktree_path)?;
    }
    if !super::dest_is_known_unmounted(worktree_path) {
        bail!(
            "unmount of {} could not be verified (still mounted or mount table \
             inconclusive); retaining backing and pin",
            worktree_path.display()
        );
    }
    let meta = lookup_nfs_meta(worktree_path);
    if let Some(m) = meta.as_ref() {
        let Some(id) = m.worktree_id.as_deref() else {
            bail!(
                "unmounted dest {} has grove metadata without a worktree id; \
                 retaining pin and dest",
                worktree_path.display()
            );
        };
        if !is_safe_worktree_id(id) {
            bail!(
                "unmounted dest {} has unsafe worktree id {id:?}; retaining pin and dest",
                worktree_path.display()
            );
        }
        if let Some(src) = m.source.as_ref() {
            {
                let _ = src;
                bail!("pin delete requires grove");
            }
        }
        if let Some(data_dir) = m.data_dir.as_ref() {
            {
                let _ = (data_dir, id);
                bail!("backing delete after verified unmount requires grove");
            }
        }
    }
    if !super::dest_is_known_unmounted(worktree_path) {
        bail!(
            "mount table inconclusive for {}; refusing dest delete",
            worktree_path.display()
        );
    }
    if worktree_path.is_dir() {
        return Ok(None);
    }
    Ok(Some(RemoveReport {
        used_btrfs_delete: false,
        unmounted_bind: false,
        unmounted_overlay: false,
    }))
}
/// After a successful daemon `RemoveWorktree`, dest is no longer a mount.
/// The daemon already deleted backing/pin; a leftover dest directory must
/// be `Ok(None)` so the caller `rm -rf`s and unregisters. When dest is fully
/// gone, return `Ok(Some(...))` so the caller does not need a second delete.
fn report_after_daemon_unmount(worktree_path: &Path) -> Result<Option<RemoveReport>> {
    if !super::dest_is_known_unmounted(worktree_path) {
        bail!(
            "RemoveWorktree returned ok but {} is still a mount or the mount table is inconclusive",
            worktree_path.display()
        );
    }
    if worktree_path.is_dir() {
        return Ok(None);
    }
    Ok(Some(RemoveReport {
        used_btrfs_delete: false,
        unmounted_bind: false,
        unmounted_overlay: false,
    }))
}
fn plain_umount(dest: &Path) -> Result<()> {
    {
        let mut cmd = std::process::Command::new("umount");
        echo_agent_tty_utils::detach_std_command(&mut cmd);
        cmd.arg(dest).stdin(Stdio::null());
        #[allow(clippy::disallowed_methods)]
        let child = cmd.spawn().context("umount")?;
        let group = echo_agent_tty_utils::global_process_scope()
            .enroll_std(&child)
            .context("enroll umount")?;
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&done);
        let group_kill = std::sync::Arc::clone(&group);
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(5));
            if !flag.load(std::sync::atomic::Ordering::SeqCst) {
                let _ = group_kill.kill();
            }
        });
        let out = child.wait_with_output().context("umount wait")?;
        done.store(true, std::sync::atomic::Ordering::SeqCst);
        drop(group);
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            tracing::warn!(dest = %dest.display(), error = %err, "umount failed");
        }
        Ok(())
    }
}
const MAX_MARKER_BYTES: u64 = 64 * 1024;
struct NfsRemoveMeta {
    worktree_id: Option<String>,
    data_dir: Option<PathBuf>,
    source: Option<PathBuf>,
    control_sock: Option<PathBuf>,
    runtime_dir: Option<PathBuf>,
}
fn lookup_nfs_meta(worktree_path: &Path) -> Option<NfsRemoveMeta> {
    #[cfg(feature = "metadata")]
    {
        if let Ok(db) = crate::db::WorktreeDb::open_default()
            && let Ok(Some(rec)) = db.get(&worktree_path.to_string_lossy())
            && crate::worktree::is_grove_strategy(&rec.creation_mode)
        {
            let nfs = rec
                .metadata
                .as_ref()
                .and_then(|m| m.get("grove").or_else(|| m.get("nfs")));
            let backing = nfs
                .and_then(|n| n.get("backing"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(PathBuf::from);
            let data_dir = backing
                .as_ref()
                .and_then(|b| b.parent())
                .and_then(|p| p.parent())
                .map(Path::to_path_buf);
            let from_db = NfsRemoveMeta {
                worktree_id: Some(rec.id),
                data_dir,
                source: Some(rec.source_repo),
                control_sock: std::env::var_os("GROVE_CONTROL_SOCK").map(PathBuf::from),
                runtime_dir: None,
            };
            if from_db.data_dir.is_some() {
                return Some(from_db);
            }
            if let Some(from_marker) = lookup_from_markers(worktree_path) {
                return Some(NfsRemoveMeta {
                    worktree_id: from_marker.worktree_id.or(from_db.worktree_id),
                    data_dir: from_marker.data_dir,
                    source: from_marker.source.or(from_db.source),
                    control_sock: from_db.control_sock.or(from_marker.control_sock),
                    runtime_dir: from_db.runtime_dir.or(from_marker.runtime_dir),
                });
            }
            return Some(from_db);
        }
    }
    lookup_from_markers(worktree_path)
}
fn lookup_from_markers(worktree_path: &Path) -> Option<NfsRemoveMeta> {
    for data in super::liveness::candidate_data_dirs() {
        let root = data.join(super::liveness::WORKTREE_BACKING_DIR);
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for ent in entries.flatten() {
            let dirent = ent.file_name().to_string_lossy().into_owned();
            if !is_safe_worktree_id(&dirent) {
                continue;
            }
            let bytes = match std::fs::read(ent.path().join(BACKING_MARKER_FILE)) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let Some(marker) = super::liveness::marker_from_dirent(&dirent, &bytes) else {
                continue;
            };
            if super::mount_table::dest_paths_equivalent(&marker.dest, worktree_path) {
                return Some(NfsRemoveMeta {
                    worktree_id: Some(dirent),
                    data_dir: Some(data),
                    source: Some(marker.source_repo),
                    control_sock: std::env::var_os("GROVE_CONTROL_SOCK").map(PathBuf::from),
                    runtime_dir: None,
                });
            }
        }
    }
    None
}
fn nfs_opts_from_env_and_meta(meta: Option<&NfsRemoveMeta>) -> NfsWorktreeOpts {
    NfsWorktreeOpts {
        enabled: true,
        control_sock: meta
            .and_then(|m| m.control_sock.clone())
            .or_else(|| std::env::var_os("GROVE_CONTROL_SOCK").map(PathBuf::from)),
        data_dir: meta.and_then(|m| m.data_dir.clone()),
        runtime_dir: meta.and_then(|m| m.runtime_dir.clone()),
        ..NfsWorktreeOpts::default()
    }
}
/// Read a backing marker from an already-open backing dir (tests / rebuild).
#[allow(dead_code)]
pub fn read_backing_marker(backing: &Path) -> Option<BackingMarker> {
    let file = std::fs::File::open(backing.join(BACKING_MARKER_FILE)).ok()?;
    let mut buf = Vec::new();
    Read::take(file, MAX_MARKER_BYTES.saturating_add(1))
        .read_to_end(&mut buf)
        .ok()?;
    if buf.len() as u64 > MAX_MARKER_BYTES {
        return None;
    }
    serde_json::from_slice(&buf).ok()
}
#[cfg(test)]
mod tests {
    use super::super::liveness::WORKTREE_BACKING_DIR;
    use super::*;
    use tempfile::TempDir;
    #[test]
    fn non_nfs_path_returns_none() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("plain");
        std::fs::create_dir(&p).unwrap();
        assert!(try_nfs_remove(&p).unwrap().is_none());
    }
    #[test]
    fn rm_planted_marker_does_not_delete_victim_backing() {
        let tmp = TempDir::new().unwrap();
        let data = tmp.path().join("grove");
        let victim_id = "wt-victim";
        let decoy_id = "wt-decoy";
        let victim_dest = tmp.path().join("real-dest");
        let harmless = tmp.path().join("harmless");
        std::fs::create_dir_all(&victim_dest).unwrap();
        std::fs::create_dir_all(&harmless).unwrap();
        let victim_backing = data.join(WORKTREE_BACKING_DIR).join(victim_id);
        let decoy_backing = data.join(WORKTREE_BACKING_DIR).join(decoy_id);
        std::fs::create_dir_all(&victim_backing).unwrap();
        std::fs::write(victim_backing.join("SECRET"), b"do-not-delete").unwrap();
        std::fs::create_dir_all(&decoy_backing).unwrap();
        let victim_marker = BackingMarker {
            schema: 1,
            worktree_id: victim_id.into(),
            dest: victim_dest,
            source_repo: tmp.path().join("repo"),
            pin_ref: format!("refs/echoagent/worktrees/{victim_id}"),
            mount_id: 1,
            created_at: 1,
        };
        let decoy_marker = BackingMarker {
            schema: 1,
            worktree_id: victim_id.into(),
            dest: harmless.clone(),
            source_repo: tmp.path().join("repo"),
            pin_ref: format!("refs/echoagent/worktrees/{decoy_id}"),
            mount_id: 1,
            created_at: 1,
        };
        std::fs::write(
            victim_backing.join(BACKING_MARKER_FILE),
            serde_json::to_vec(&victim_marker).unwrap(),
        )
        .unwrap();
        std::fs::write(
            decoy_backing.join(BACKING_MARKER_FILE),
            serde_json::to_vec(&decoy_marker).unwrap(),
        )
        .unwrap();
        let _env = crate::nfs::GROVE_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("GROVE_DATA_DIR", &data) };
        let report = try_nfs_remove(&harmless);
        unsafe { std::env::remove_var("GROVE_DATA_DIR") };
        assert!(
            victim_backing.join("SECRET").exists(),
            "victim backing must survive planted decoy: {report:?}"
        );
        assert!(
            report.as_ref().ok().and_then(|r| r.as_ref()).is_none(),
            "id≠dirent decoy marker must be ignored, not used for remove: {report:?}"
        );
    }
    #[test]
    fn marker_lookup_finds_dest() {
        let tmp = TempDir::new().unwrap();
        let data = tmp.path().join("grove");
        let dest = tmp.path().join("wt");
        std::fs::create_dir(&dest).unwrap();
        let id = "wt-rm1";
        let backing = data.join(WORKTREE_BACKING_DIR).join(id);
        std::fs::create_dir_all(&backing).unwrap();
        let marker = BackingMarker {
            schema: 1,
            worktree_id: id.into(),
            dest: dest.clone(),
            source_repo: tmp.path().join("repo"),
            pin_ref: format!("refs/echoagent/worktrees/{id}"),
            mount_id: 1,
            created_at: 1,
        };
        std::fs::write(
            backing.join(BACKING_MARKER_FILE),
            serde_json::to_vec(&marker).unwrap(),
        )
        .unwrap();
        let _env = crate::nfs::GROVE_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("GROVE_DATA_DIR", &data) };
        let found = lookup_nfs_meta(&dest);
        unsafe { std::env::remove_var("GROVE_DATA_DIR") };
        let found = found.expect("marker must resolve dest");
        assert_eq!(found.worktree_id.as_deref(), Some(id));
    }
    #[test]
    fn empty_backing_falls_through_to_marker() {
        let tmp = TempDir::new().unwrap();
        let data = tmp.path().join("grove");
        let dest = tmp.path().join("wt");
        std::fs::create_dir(&dest).unwrap();
        let id = "wt-empty-back";
        let backing = data.join(WORKTREE_BACKING_DIR).join(id);
        std::fs::create_dir_all(&backing).unwrap();
        let marker = BackingMarker {
            schema: 1,
            worktree_id: id.into(),
            dest: dest.clone(),
            source_repo: tmp.path().join("repo"),
            pin_ref: format!("refs/echoagent/worktrees/{id}"),
            mount_id: 1,
            created_at: 1,
        };
        std::fs::write(
            backing.join(BACKING_MARKER_FILE),
            serde_json::to_vec(&marker).unwrap(),
        )
        .unwrap();
        let _env = crate::nfs::GROVE_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("GROVE_DATA_DIR", &data) };
        let found = lookup_from_markers(&dest);
        unsafe { std::env::remove_var("GROVE_DATA_DIR") };
        let found = found.expect("marker recovery");
        assert_eq!(found.worktree_id.as_deref(), Some(id));
        assert_eq!(found.data_dir.as_deref(), Some(data.as_path()));
    }
    #[test]
    fn leftover_dest_after_daemon_unmount_is_ok_none() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("wt");
        std::fs::create_dir(&dest).unwrap();
        assert!(report_after_daemon_unmount(&dest).unwrap().is_none());
        assert!(dest.is_dir(), "helper must not delete leftover dest");
    }
    #[test]
    fn absent_dest_after_daemon_unmount_is_some() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("gone");
        assert!(report_after_daemon_unmount(&dest).unwrap().is_some());
    }
}
