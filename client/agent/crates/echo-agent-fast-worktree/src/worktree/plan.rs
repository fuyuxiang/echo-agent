//! Worktree execution planning.
//!
//! `WorktreePlan` makes the worktree creation pipeline explicit and testable.
use crate::{BtrfsDelegate, CreationMode, IgnoredFilesMode, NfsWorktreeOpts, WorkingTreeMode};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
/// Same scheme as [`crate::db::id_from_path`]: `<basename>-<hash of full path>`.
/// Derived pre-dispatch so it can double as the NFS IPC idempotency key.
///
/// The hashed path is lexical (no dest/parent symlink follow). On macOS,
/// `/tmp` and `/var` are rewritten to `/private/{tmp,var}` so the two
/// system names of the same prefix stay one id; attacker dest/parent
/// symlinks do not collapse.
pub(crate) fn worktree_id_from_path(path: &Path) -> String {
    let path = canonicalize_for_id(path);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let base = name.strip_prefix("worktree-").unwrap_or(&name);
    let sanitized = sanitize_worktree_id_base(base);
    format!("{sanitized}-{}", crate::copy::shard::short_path_hash(&path))
}
/// Map a dest basename onto `[A-Za-z0-9._-]+` without `..` so
/// [`grove_git::validate_worktree_id`] accepts dests with spaces etc.
fn sanitize_worktree_id_base(base: &str) -> String {
    let mut out: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    while out.contains("..") {
        out = out.replace("..", ".");
    }
    out = out.trim_matches('.').trim_matches('-').to_string();
    if out.is_empty() {
        return "wt".into();
    }
    if out.starts_with('.') {
        out.insert_str(0, "wt");
    }
    out
}
/// Lexical absolute dest for id + IPC. Does **not** `stat` dest or parent.
/// `dunce::canonicalize` blocks forever on a wedged NFS mount — the failure
/// mode `create` must still diagnose via the mount-table probe / InFlight path.
///
/// Relative dests are joined to `cwd` first so a not-yet-created `./wt` and
/// the post-create absolute path hash to the same id. macOS `/tmp` `/var`
/// `/etc` are rewritten to `/private/…` so the two system names stay one id.
pub(crate) fn canonicalize_for_id(path: &Path) -> PathBuf {
    {
        let abs = if path.is_absolute() {
            path.to_path_buf()
        } else {
            match std::env::current_dir() {
                Ok(cwd) => cwd.join(path),
                Err(_) => path.to_path_buf(),
            }
        };
        macos_private_prefix(strip_trailing_slashes(dunce::simplified(&abs)))
    }
}
/// `/dest` and `/dest/` must hash to one id. GC already treats them as
/// the same dest via `dest_paths_equivalent`. Leave `/` alone.
fn strip_trailing_slashes(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    let s = path.to_string_lossy();
    if s == "/" || !s.ends_with('/') {
        return path.to_path_buf();
    }
    PathBuf::from(s.trim_end_matches('/'))
}
fn macos_private_prefix(path: PathBuf) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let mut path = path;
        {
            const DATA: &str = "/System/Volumes/Data";
            let s = path.to_string_lossy();
            if s == DATA {
                path = PathBuf::from("/");
            } else if let Some(rest) = s.strip_prefix(DATA)
                && rest.starts_with('/')
            {
                path = PathBuf::from(rest);
            }
        }
        const PAIRS: &[(&str, &str)] = &[
            ("/tmp", "/private/tmp"),
            ("/var", "/private/var"),
            ("/etc", "/private/etc"),
        ];
        let s = path.to_string_lossy();
        for (from, to) in PAIRS {
            if s == *from {
                return PathBuf::from(to);
            }
            let prefix = format!("{from}/");
            if let Some(rest) = s.strip_prefix(&prefix) {
                return PathBuf::from(to).join(rest);
            }
        }
        path
    }
    #[cfg(not(target_os = "macos"))]
    {
        path
    }
}
#[derive(Clone)]
pub(crate) struct WorktreePlan {
    pub source: PathBuf,
    pub dest: PathBuf,
    pub git_ref: String,
    pub parallelism: usize,
    pub channel_buffer: usize,
    pub working_tree: WorkingTreeMode,
    pub ignored_files: IgnoredFilesMode,
    pub ignored_parallelism: usize,
    /// Strategy for worktree creation (linked, standalone, or git checkout).
    pub creation_mode: CreationMode,
    /// Cancellation token for aborting file copy mid-flight.
    pub cancellation_token: CancellationToken,
    /// Optional delegate for privileged btrfs operations (used when the caller
    /// lacks CAP_SYS_ADMIN, e.g., inside a bwrap sandbox).
    /// Only read on Linux (in `try_btrfs_delegate`).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub btrfs_delegate: Option<Arc<dyn BtrfsDelegate>>,
    /// Idempotency key (and worktrees.db id). Always set before dispatch.
    pub worktree_id: String,
    /// Explicit NFS enablement. `None` / `enabled: false` skips the NFS arm.
    pub nfs: Option<NfsWorktreeOpts>,
}
impl std::fmt::Debug for WorktreePlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorktreePlan")
            .field("source", &self.source)
            .field("dest", &self.dest)
            .field("git_ref", &self.git_ref)
            .field("parallelism", &self.parallelism)
            .field("working_tree", &self.working_tree)
            .field("creation_mode", &self.creation_mode)
            .field("has_btrfs_delegate", &self.btrfs_delegate.is_some())
            .field("worktree_id", &self.worktree_id)
            .field("nfs_enabled", &self.nfs.as_ref().is_some_and(|o| o.enabled))
            .finish()
    }
}
impl WorktreePlan {
    pub(crate) fn effective_parallelism(&self) -> usize {
        if self.parallelism == 0 {
            num_cpus::get()
        } else {
            self.parallelism
        }
    }
    pub(crate) fn effective_ignored_parallelism(&self) -> usize {
        if self.ignored_parallelism == 0 {
            num_cpus::get()
        } else {
            self.ignored_parallelism
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    #[test]
    fn worktree_id_stable_across_var_and_private_var() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("wt-var-id");
        let id_raw = worktree_id_from_path(&dest);
        let parent_canon = dunce::canonicalize(tmp.path()).unwrap();
        let via_private = parent_canon.join("wt-var-id");
        assert_eq!(id_raw, worktree_id_from_path(&via_private));
        std::fs::create_dir(&dest).unwrap();
        let after = dunce::canonicalize(&dest).unwrap();
        assert_eq!(id_raw, worktree_id_from_path(&dest));
        assert_eq!(id_raw, worktree_id_from_path(&after));
    }
    #[test]
    #[cfg(target_os = "macos")]
    fn worktree_id_tmp_matches_private_tmp() {
        let name = format!("echo-agent-fwt-id-{}", std::process::id());
        let via_var = PathBuf::from("/tmp").join(&name);
        let via_private = PathBuf::from("/private/tmp").join(&name);
        assert_eq!(
            worktree_id_from_path(&via_var),
            worktree_id_from_path(&via_private)
        );
    }
    #[test]
    fn relative_dest_id_matches_cwd_join() {
        let name = format!("echo-agent-fwt-rel-{}", std::process::id());
        let rel = PathBuf::from(&name);
        let abs = std::env::current_dir().unwrap().join(&name);
        assert_eq!(worktree_id_from_path(&rel), worktree_id_from_path(&abs));
    }
    #[test]
    fn worktree_id_does_not_follow_dest_or_parent_symlink() {
        let tmp = TempDir::new().unwrap();
        let real_parent = tmp.path().join("real");
        std::fs::create_dir_all(&real_parent).unwrap();
        let real = real_parent.join("wt");
        std::fs::create_dir(&real).unwrap();
        let via_parent = tmp.path().join("via");
        std::os::unix::fs::symlink(&real_parent, &via_parent).unwrap();
        let via = via_parent.join("wt");
        assert_ne!(
            worktree_id_from_path(&real),
            worktree_id_from_path(&via),
            "parent symlink must not collapse dest identity"
        );
        let dest_real = tmp.path().join("other").join("wt2");
        std::fs::create_dir_all(dest_real.parent().unwrap()).unwrap();
        std::fs::create_dir(&dest_real).unwrap();
        let dest_link = tmp.path().join("wt2");
        std::os::unix::fs::symlink(&dest_real, &dest_link).unwrap();
        assert_ne!(
            worktree_id_from_path(&dest_real),
            worktree_id_from_path(&dest_link),
            "dest symlink must not collapse dest identity"
        );
    }
}
