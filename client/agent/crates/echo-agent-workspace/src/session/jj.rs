//! Jujutsu (jj) operations for colocated repos.
//!
//! Mirrors the git operations in [`super::git`] but uses the `jj` CLI.
//! All read-only calls use `--ignore-working-copy`; mutating calls use
//! [`super::git::jj_cli_mut`].

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Result;

use super::git::{
    ChangeType, CommitData, CommitResult, GitBranchEntry, GitBranchListData, GitFileChange,
    GitInfoData, GitStatusData, VcsKind, git_cli, jj_cli, jj_cli_mut,
};

/// Query bookmarks attached to a revision (returns `None` if empty).
async fn bookmarks_at(cwd: &Path, revset: &str) -> Option<String> {
    jj_cli(
        cwd,
        &[
            "log",
            "--no-graph",
            "-r",
            revset,
            "-T",
            r#"bookmarks.join(", ")"#,
        ],
    )
    .await
    .ok()
    .filter(|s| !s.is_empty())
}

/// Repo info (reuses `GitInfoData` for ACP compatibility).
pub async fn info(cwd: &Path) -> Result<GitInfoData> {
    let root = jj_cli(cwd, &["workspace", "root"]).await?;
    let current_branch = bookmarks_at(cwd, "@-").await;

    // Remote URLs via colocated git
    let remotes = git_cli(cwd, &["remote", "-v"])
        .await
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            (parts.len() >= 2 && line.contains("(fetch)")).then(|| parts[1].to_string())
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    Ok(GitInfoData {
        root,
        remotes,
        current_branch,
        default_branch: None,
        vcs_kind: Some(VcsKind::JujutsuColocated),
    })
}

/// Status mapped to `GitStatusData` (all changes in `unstaged` — jj has no index).
pub async fn status(cwd: &Path) -> Result<GitStatusData> {
    let root = jj_cli(cwd, &["workspace", "root"]).await.ok();
    let commit = jj_cli(
        cwd,
        &[
            "log",
            "--no-graph",
            "-r",
            "@",
            "-T",
            "commit_id.shortest(12)",
        ],
    )
    .await
    .ok();
    let branch = bookmarks_at(cwd, "@-").await;

    let diff_output = jj_cli(cwd, &["diff", "--summary"])
        .await
        .unwrap_or_default();

    let unstaged: Vec<GitFileChange> = diff_output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let (change_type, path) = if let Some(rest) = line.strip_prefix("M ") {
                (ChangeType::Edit, rest.trim())
            } else if let Some(rest) = line.strip_prefix("A ") {
                (ChangeType::Create, rest.trim())
            } else if let Some(rest) = line.strip_prefix("D ") {
                (ChangeType::Delete, rest.trim())
            } else if let Some(rest) = line.strip_prefix("R ") {
                (ChangeType::Rename, rest.trim())
            } else {
                return None;
            };
            Some(GitFileChange {
                path: path.to_string(),
                old_path: None,
                change_type,
                staged: Some(false),
                additions: 0,
                deletions: 0,
                patch: None,
                patch_bytes: None,
                patch_lines: None,
                old_text: None,
                new_text: None,
            })
        })
        .collect();

    Ok(GitStatusData {
        root,
        main_root: None,
        is_worktree: None,
        branch,
        commit,
        upstream: None,
        remote_url: None,
        ahead: None,
        behind: None,
        staged: Vec::new(),
        unstaged,
    })
}

/// Current commit id: the working-copy commit (`@`), matching the `commit`
/// field reported by [`status`].
///
/// In a colocated repo, git HEAD points at `@-` (the parent of the working-copy
/// commit), so reading git HEAD would return a different revision than jj's
/// current commit. Returns `Ok(None)` if the id can't be determined, mirroring
/// the lenient behavior of `git::get_current_commit`.
pub async fn current_commit(cwd: &Path) -> Result<Option<String>> {
    let commit = jj_cli(cwd, &["log", "--no-graph", "-r", "@", "-T", "commit_id"])
        .await
        .ok()
        .filter(|s| !s.is_empty());
    Ok(commit)
}

/// Commit: describe the current change and start a new one.
pub async fn commit(cwd: &Path, message: &str) -> Result<CommitResult> {
    jj_cli_mut(cwd, &["describe", "-m", message]).await?;
    let commit_hash = jj_cli(
        cwd,
        &[
            "log",
            "--no-graph",
            "-r",
            "@",
            "-T",
            "commit_id.shortest(12)",
        ],
    )
    .await
    .ok();
    jj_cli_mut(cwd, &["new"]).await?;

    Ok(CommitResult {
        data: CommitData {
            commit_hash,
            output: Some("Commit described and new change started".to_string()),
        },
        warning: None,
        // The structured outcome is a git-backend concept.
        outcome: None,
    })
}

/// Discard: restore working copy from parent.
pub async fn discard(cwd: &Path, paths: Option<Vec<String>>) -> Result<()> {
    match paths {
        Some(paths) if !paths.is_empty() => {
            let mut args: Vec<&str> = vec!["restore"];
            let refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
            args.extend(refs);
            jj_cli_mut(cwd, &args).await?;
        }
        _ => {
            jj_cli_mut(cwd, &["restore"]).await?;
        }
    }
    Ok(())
}

/// Bookmark list, mapped to `GitBranchListData` for ACP compatibility.
pub async fn list_bookmarks(cwd: &Path) -> Result<GitBranchListData> {
    let root = jj_cli(cwd, &["workspace", "root"]).await?;

    let bookmark_output = jj_cli(
        cwd,
        &[
            "bookmark",
            "list",
            "--all",
            "-T",
            r#"name ++ if(remote, "@" ++ remote, "") ++ "\n""#,
        ],
    )
    .await
    .unwrap_or_default();

    let current_bookmark = bookmarks_at(cwd, "@-").await;

    let branches: Vec<GitBranchEntry> = bookmark_output
        .lines()
        .filter(|l| !l.is_empty())
        .map(|name| {
            let name = name.to_string();
            let is_remote = name.contains('@');
            let is_current = !is_remote && current_bookmark.as_deref().is_some_and(|cb| cb == name);
            GitBranchEntry {
                name,
                current: is_current,
                remote: is_remote,
            }
        })
        .collect();

    Ok(GitBranchListData {
        current_branch: current_bookmark,
        repo_root: root,
        branches,
    })
}
