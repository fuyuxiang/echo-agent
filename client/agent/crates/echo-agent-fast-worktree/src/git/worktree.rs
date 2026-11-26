//! Git worktree operations.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::git::checkout::git_command;

/// Create a git worktree with `--no-checkout`. Blocking.
pub(crate) fn worktree_add_no_checkout(source: &Path, dest: &str, git_ref: &str) -> Result<()> {
    let output = git_command()
        .current_dir(source)
        .args([
            "worktree",
            "add",
            "--detach",
            "--no-checkout",
            dest,
            git_ref,
        ])
        .output()
        .context("failed to run git worktree add")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git worktree add failed: {}", stderr);
    }

    Ok(())
}

/// Which stale registrations [`remove_stale_worktree_registrations`] removes.
#[derive(Clone, Copy, Debug)]
enum StaleWorktreeMatch<'a> {
    /// Exactly the registration whose recorded worktree path is this path.
    Path(&'a Path),
    /// Every registration whose recorded worktree path is under this prefix
    /// (e.g. a tool-owned base directory, proving ownership of the entries).
    UnderPrefix(&'a Path),
}

/// Remove stale `.git/worktrees/<id>` registrations matching `match_rule`.
/// Best-effort; returns the count removed.
///
/// Not `git worktree prune`: prune also drops registrations whose worktree is
/// merely invisible in the current mount namespace, wiping live linked worktrees
/// inside a container that does not mount them.
fn remove_stale_worktree_registrations(
    source_repo: &Path,
    match_rule: StaleWorktreeMatch<'_>,
) -> u64 {
    // Only a definitively-absent path is stale; a stat error keeps the
    // registration (its reflog may hold the last copy of a commit).
    if let StaleWorktreeMatch::Path(p) = match_rule
        && !matches!(p.try_exists(), Ok(false))
    {
        return 0;
    }

    let common_dir = match git_command()
        .current_dir(source_repo)
        .args(["rev-parse", "--git-common-dir"])
        .output()
    {
        Ok(o) if o.status.success() => {
            let path = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim());
            if path.is_absolute() {
                path
            } else {
                source_repo.join(path)
            }
        }
        Ok(o) => {
            tracing::warn!(
                source_repo = %source_repo.display(),
                stderr = %String::from_utf8_lossy(&o.stderr),
                "stale registration scrub skipped: git rev-parse --git-common-dir failed"
            );
            return 0;
        }
        Err(e) => {
            tracing::warn!(
                source_repo = %source_repo.display(),
                error = %e,
                "stale registration scrub skipped: git failed to spawn"
            );
            return 0;
        }
    };

    let Ok(entries) = std::fs::read_dir(common_dir.join("worktrees")) else {
        return 0;
    };
    let normalized_target = match match_rule {
        StaleWorktreeMatch::Path(p) | StaleWorktreeMatch::UnderPrefix(p) => normalized_for_match(p),
    };
    let mut removed = 0u64;
    for entry in entries.flatten() {
        let registration = entry.path();
        // Keep on a `locked` marker, or if we cannot tell whether one exists
        // (a stat error must not be read as unlocked).
        if !registration.is_dir() || !matches!(registration.join("locked").try_exists(), Ok(false))
        {
            continue;
        }
        let Ok(backlink) = std::fs::read_to_string(registration.join("gitdir")) else {
            continue;
        };
        // The backlink names `<worktree>/.git`; under
        // `worktree.useRelativePaths` (git >= 2.48) it is relative to the
        // registration dir, not the CWD.
        let backlink_path = Path::new(backlink.trim());
        let backlink_abs = if backlink_path.is_relative() {
            registration.join(backlink_path)
        } else {
            backlink_path.to_path_buf()
        };
        let Some(recorded) = backlink_abs.parent() else {
            continue;
        };
        // Present, or a stat error we cannot see past: keep the registration.
        if !matches!(recorded.try_exists(), Ok(false)) {
            continue;
        }
        let recorded = normalized_for_match(recorded);
        let matched = match match_rule {
            StaleWorktreeMatch::Path(_) => recorded == normalized_target,
            StaleWorktreeMatch::UnderPrefix(_) => recorded.starts_with(&normalized_target),
        };
        if !matched {
            continue;
        }
        // Removing the registration also drops its `logs/` reflog. The recorded
        // working tree is confirmed gone (checked above), the registration is
        // not `locked`, and this scrub is opt-in (`include_rebuild`, off by
        // default); a reflog-only commit under a vanished worktree is accepted
        // as lost here rather than named (the age-GC delete path names via
        // reclaimed.rs; this cleanup does not).
        match std::fs::remove_dir_all(&registration) {
            Ok(()) => {
                tracing::debug!(
                    registration = %registration.display(),
                    worktree = %recorded.display(),
                    "removed stale worktree registration"
                );
                removed += 1;
            }
            Err(e) => {
                tracing::warn!(
                    registration = %registration.display(),
                    error = %e,
                    "failed to remove stale worktree registration"
                );
            }
        }
    }
    removed
}

/// Remove the stale registration for exactly one worktree path.
pub fn remove_stale_worktree_registration(source_repo: &Path, worktree_path: &Path) -> u64 {
    remove_stale_worktree_registrations(source_repo, StaleWorktreeMatch::Path(worktree_path))
}

/// Remove stale registrations for every worktree under a tool-owned base directory.
///
/// Deliberately not `git worktree prune`: prune also deletes registrations whose
/// worktree path is merely invisible from the current mount namespace (e.g. a
/// container that does not mount the user's worktrees), which would wipe live
/// ones; this only removes registrations whose recorded path is confirmed gone.
pub fn remove_stale_worktree_registrations_under(source_repo: &Path, prefix: &Path) -> u64 {
    remove_stale_worktree_registrations(source_repo, StaleWorktreeMatch::UnderPrefix(prefix))
}

/// Normalized worktree path a registration's `gitdir` backlink names, or `None`
/// if missing or malformed. The backlink may be relative (`worktree.useRelativePaths`).
pub(crate) fn registration_worktree_path(registration: &Path) -> Option<PathBuf> {
    let backlink = std::fs::read_to_string(registration.join("gitdir")).ok()?;
    let backlink_path = Path::new(backlink.trim());
    let backlink_abs = if backlink_path.is_relative() {
        registration.join(backlink_path)
    } else {
        backlink_path.to_path_buf()
    };
    Some(normalized_for_match(backlink_abs.parent()?))
}

/// Canonicalize the deepest existing ancestor and re-append the missing tail, so
/// a symlinked spelling still compares equal after the path is deleted.
pub(crate) fn normalized_for_match(path: &Path) -> PathBuf {
    let mut missing = Vec::new();
    let mut cursor = path;
    loop {
        if let Ok(canonical) = dunce::canonicalize(cursor) {
            let mut result = canonical;
            for component in missing.iter().rev() {
                result.push(component);
            }
            return result;
        }
        match (cursor.parent(), cursor.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name.to_os_string());
                cursor = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hermetic git (pinned binary, masked global/system config) so a
    /// developer's `~/.gitconfig` cannot steer these registration tests.
    fn run_git(cwd: &Path, args: &[&str]) {
        let _ = echo_agent_test_utils::git::run_git(cwd, args);
    }

    fn init_repo_with_worktrees() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        echo_agent_test_utils::git::init_git_repo(&repo);
        std::fs::write(repo.join("f.txt"), b"x").unwrap();
        run_git(&repo, &["add", "f.txt"]);
        run_git(&repo, &["commit", "-m", "init"]);
        (tmp, repo)
    }

    fn add_worktree(repo: &Path, wt: &Path) {
        run_git(
            repo,
            &["worktree", "add", "--detach", wt.to_str().unwrap(), "HEAD"],
        );
    }

    #[test]
    fn removes_only_the_matching_stale_registration() {
        let (tmp, repo) = init_repo_with_worktrees();
        let git_worktrees = repo.join(".git").join("worktrees");

        let target = tmp.path().join("target-wt");
        add_worktree(&repo, &target);
        std::fs::remove_dir_all(&target).unwrap();

        let hidden = tmp.path().join("hidden-wt");
        add_worktree(&repo, &hidden);
        std::fs::rename(&hidden, tmp.path().join("hidden-wt-moved")).unwrap();

        std::fs::create_dir(git_worktrees.join("bare-entry")).unwrap();

        let removed = remove_stale_worktree_registration(&repo, &target);

        assert_eq!(removed, 1);
        assert!(!git_worktrees.join("target-wt").exists());
        assert!(
            git_worktrees.join("hidden-wt").exists(),
            "non-matching registration must survive even when its path is gone"
        );
        assert!(git_worktrees.join("bare-entry").exists());
        assert!(git_worktrees.exists());
    }

    /// Rewrite a registration's `gitdir` backlink to the relative layout
    /// `worktree.useRelativePaths` (git >= 2.48) produces, without requiring
    /// that git version on the test host.
    fn make_backlink_relative(repo: &Path, reg_name: &str, worktree: &Path) {
        let reg_dir = repo.join(".git").join("worktrees").join(reg_name);
        let target = worktree.join(".git");
        let mut ups = PathBuf::new();
        let mut cursor = reg_dir.as_path();
        loop {
            if let Ok(rest) = target.strip_prefix(cursor) {
                std::fs::write(
                    reg_dir.join("gitdir"),
                    format!("{}\n", ups.join(rest).display()),
                )
                .unwrap();
                return;
            }
            cursor = cursor.parent().expect("shared ancestor");
            ups.push("..");
        }
    }

    #[test]
    fn resolves_relative_backlink_against_registration_dir() {
        let (tmp, repo) = init_repo_with_worktrees();

        let stale = tmp.path().join("rel-stale");
        add_worktree(&repo, &stale);
        make_backlink_relative(&repo, "rel-stale", &stale);
        std::fs::remove_dir_all(&stale).unwrap();

        let live = tmp.path().join("rel-live");
        add_worktree(&repo, &live);
        make_backlink_relative(&repo, "rel-live", &live);

        let removed_live = remove_stale_worktree_registrations_under(&repo, tmp.path());
        assert_eq!(removed_live, 1, "only the stale relative entry is removed");
        assert!(!repo.join(".git/worktrees/rel-stale").exists());
        assert!(
            repo.join(".git/worktrees/rel-live").exists(),
            "live worktree with relative backlink must survive"
        );
    }

    #[test]
    fn keeps_registration_when_worktree_still_exists() {
        let (tmp, repo) = init_repo_with_worktrees();
        let wt = tmp.path().join("live-wt");
        add_worktree(&repo, &wt);

        let removed = remove_stale_worktree_registration(&repo, &wt);

        assert_eq!(removed, 0);
        assert!(repo.join(".git/worktrees/live-wt").exists());
    }

    #[test]
    fn keeps_locked_registration() {
        let (tmp, repo) = init_repo_with_worktrees();
        let wt = tmp.path().join("locked-wt");
        add_worktree(&repo, &wt);
        run_git(&repo, &["worktree", "lock", wt.to_str().unwrap()]);
        std::fs::remove_dir_all(&wt).unwrap();

        let removed = remove_stale_worktree_registration(&repo, &wt);

        assert_eq!(removed, 0);
        assert!(repo.join(".git/worktrees/locked-wt").exists());
    }

    #[test]
    fn under_prefix_removes_only_owned_stale_registrations() {
        let (tmp, repo) = init_repo_with_worktrees();
        let git_worktrees = repo.join(".git").join("worktrees");

        let owned_base = tmp.path().join("owned-base");
        let owned_stale = owned_base.join("instance").join("wt-stale");
        let owned_locked = owned_base.join("instance").join("wt-locked");
        let owned_live = owned_base.join("instance").join("wt-live");
        std::fs::create_dir_all(owned_stale.parent().unwrap()).unwrap();
        add_worktree(&repo, &owned_stale);
        add_worktree(&repo, &owned_locked);
        add_worktree(&repo, &owned_live);
        std::fs::remove_dir_all(&owned_stale).unwrap();
        run_git(&repo, &["worktree", "lock", owned_locked.to_str().unwrap()]);
        std::fs::remove_dir_all(&owned_locked).unwrap();

        let foreign = tmp.path().join("foreign-wt");
        add_worktree(&repo, &foreign);
        std::fs::rename(&foreign, tmp.path().join("foreign-wt-moved")).unwrap();

        std::fs::create_dir(git_worktrees.join("bare-entry")).unwrap();

        let removed = remove_stale_worktree_registrations_under(&repo, &owned_base);

        assert_eq!(removed, 1);
        assert!(!git_worktrees.join("wt-stale").exists());
        assert!(
            git_worktrees.join("wt-locked").exists(),
            "locked registration must survive even when owned and stale"
        );
        assert!(
            git_worktrees.join("wt-live").exists(),
            "live owned registration must survive"
        );
        assert!(
            git_worktrees.join("foreign-wt").exists(),
            "foreign registration must survive even when its path is hidden"
        );
        assert!(git_worktrees.join("bare-entry").exists());
        assert!(git_worktrees.exists());
    }

    #[cfg(unix)]
    #[test]
    fn under_prefix_matches_symlinked_base_spelling() {
        let (tmp, repo) = init_repo_with_worktrees();

        let real_base = tmp.path().join("real-base");
        std::fs::create_dir(&real_base).unwrap();
        let wt = real_base.join("wt");
        add_worktree(&repo, &wt);
        std::fs::remove_dir_all(&wt).unwrap();

        let alias_base = tmp.path().join("alias-base");
        std::os::unix::fs::symlink(&real_base, &alias_base).unwrap();

        let removed = remove_stale_worktree_registrations_under(&repo, &alias_base);
        assert_eq!(removed, 1, "symlinked base spelling must match");
        assert!(!repo.join(".git/worktrees/wt").exists());
    }

    #[test]
    fn matches_across_symlinked_parent_spelling() {
        let (tmp, repo) = init_repo_with_worktrees();
        let real_parent = tmp.path().join("real-parent");
        std::fs::create_dir(&real_parent).unwrap();
        let wt = real_parent.join("wt");
        add_worktree(&repo, &wt);
        std::fs::remove_dir_all(&wt).unwrap();

        #[cfg(unix)]
        {
            let alias = tmp.path().join("alias-parent");
            std::os::unix::fs::symlink(&real_parent, &alias).unwrap();
            let removed = remove_stale_worktree_registration(&repo, &alias.join("wt"));
            assert_eq!(removed, 1, "symlinked spelling of the parent must match");
            assert!(!repo.join(".git/worktrees/wt").exists());
        }
    }
}
