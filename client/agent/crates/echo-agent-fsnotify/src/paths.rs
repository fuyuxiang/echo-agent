//! `.git/` path classification. Component-based against the discovered
//! `git_dir` (not substring matching), so `/tmp/.git-backup/HEAD` is safe
//! and Windows separators work.
//!
//! Watched: `HEAD`, `index`, `refs/*`, `packed-refs`, `FETCH_HEAD`.
//! Skipped: `COMMIT_EDITMSG`, `MERGE_HEAD`, `REBASE_HEAD`, `objects/*`
//! (too noisy or no meaningful state change). `index.lock` is handled by
//! the lock state machine, not here.

use std::path::Path;

use crate::event::GitMetaKind;

/// `git_dir` is from `git2::Repository::discover().path()` (handles worktrees).
pub(crate) fn classify_git_path(path: &Path, git_dir: &Path) -> Option<GitMetaKind> {
    let rel = path.strip_prefix(git_dir).ok()?.to_str()?;
    match rel {
        "HEAD" => Some(GitMetaKind::HeadChanged),
        "FETCH_HEAD" => Some(GitMetaKind::FetchHeadChanged),
        "index" => Some(GitMetaKind::IndexChanged),
        "packed-refs" => Some(GitMetaKind::RefsChanged),
        s if s.starts_with("refs/") || s.starts_with("refs\\") => Some(GitMetaKind::RefsChanged),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn classify(p: &str, git_dir: &str) -> Option<GitMetaKind> {
        classify_git_path(&PathBuf::from(p), &PathBuf::from(git_dir))
    }

    #[test]
    fn classify_positive_cases() {
        let g = "/r/.git";
        assert_eq!(classify("/r/.git/HEAD", g), Some(GitMetaKind::HeadChanged));
        assert_eq!(
            classify("/r/.git/index", g),
            Some(GitMetaKind::IndexChanged)
        );
        assert_eq!(
            classify("/r/.git/FETCH_HEAD", g),
            Some(GitMetaKind::FetchHeadChanged)
        );
        assert_eq!(
            classify("/r/.git/packed-refs", g),
            Some(GitMetaKind::RefsChanged)
        );
        assert_eq!(
            classify("/r/.git/refs/heads/feature-branch-with-slashes", g),
            Some(GitMetaKind::RefsChanged)
        );
        assert_eq!(
            classify("/r/.git/refs/remotes/origin/main", g),
            Some(GitMetaKind::RefsChanged)
        );
    }

    #[test]
    fn classify_returns_none() {
        let g = "/r/.git";
        // Excluded git internals.
        assert_eq!(classify("/r/.git/COMMIT_EDITMSG", g), None);
        assert_eq!(classify("/r/.git/MERGE_HEAD", g), None);
        assert_eq!(classify("/r/.git/objects/ab/1234", g), None);
        assert_eq!(classify("/r/.git/index.lock", g), None);
        // Workspace files.
        assert_eq!(classify("/r/src/main.rs", g), None);
        // Substring false-positive prevented by strip_prefix.
        assert_eq!(classify("/r/.git-backup/HEAD", g), None);
        // Path under a different git_dir.
        assert_eq!(classify("/other/.git/HEAD", g), None);
    }

    #[test]
    fn classify_handles_worktree_gitdir() {
        assert_eq!(
            classify("/r/.git/worktrees/wt/HEAD", "/r/.git/worktrees/wt"),
            Some(GitMetaKind::HeadChanged)
        );
    }
}
