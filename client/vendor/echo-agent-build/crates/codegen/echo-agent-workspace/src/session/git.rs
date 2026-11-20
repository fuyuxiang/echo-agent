//! Git operations: CLI for simple actions (stage, commit, push); git2 for structured data (status, diffs).
#![allow(dead_code)]
pub use crate::restore_fetch::git_object_exists;
use anyhow::Result;
pub use echo_agent_workspace_types::rpc::git::{
    ChangeType, CheckoutCommitResponse, CommitData, CommitOutcome, CommitResult, DiscardScope,
    GitBranchEntry, GitBranchListData, GitCommitReq, GitDiffsData, GitEnsureBindingResult,
    GitError, GitFileChange, GitInfoData, GitMergeToMainOutcome, GitMergeToMainResult,
    GitPushResult, GitReadFile, GitReadFilesData, GitStatusData, GitSyncBaseOutcome,
    GitSyncBaseResult, PushStatus, StageData, VcsKind,
};
use git2::{DiffOptions, Reference, Repository, StatusOptions};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use tokio::process::Command;
use tokio::sync::Mutex;
use url::Url;
pub const ERROR_CODE_DIFF_SIZE_EXCEEDED: &str = "DIFF_SIZE_EXCEEDED";
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffSizeExceededError {
    pub files: Vec<DiffSizeExceededFile>,
}
impl DiffSizeExceededError {
    pub fn message(&self) -> String {
        let file_names: Vec<_> = self.files.iter().map(|f| f.path.as_str()).collect();
        format!(
            "Diff exceeds size limits for {} file(s): {}",
            self.files.len(),
            file_names.join(", ")
        )
    }
}
impl<T: Serialize> From<DiffSizeExceededError> for super::result::ExtMethodResult<T> {
    fn from(err: DiffSizeExceededError) -> Self {
        Self {
            result: None,
            error: serde_json::to_value(super::result::ExtMethodError::with_data(
                0,
                err.message(),
                err.message(),
            ))
            .ok(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffSizeExceededFile {
    pub path: String,
    pub patch_bytes: u64,
    pub patch_lines: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_lines: Option<u64>,
}
/// Run a git CLI command and return stdout on success, or error with stderr.
///
/// All invocations use `--no-optional-locks` to prevent background stat-cache
/// refreshes from creating `index.lock`.  This flag only suppresses *optional*
/// sub-operations (e.g. refreshing stat info after `status`); locks that are
/// *required* for the requested operation (e.g. `git add`, `git commit`) are
/// unaffected.  See `git(1)` and `GIT_OPTIONAL_LOCKS`.
pub async fn git_cli(cwd: &Path, args: &[&str]) -> Result<String> {
    tracing::debug!(cwd = %cwd.display(), args = ?args, "git_cli");
    let mut cmd = Command::from(echo_agent_tty_utils::git_command());
    cmd.current_dir(cwd);
    let output = match cmd.args(args).output().await {
        Ok(o) => o,
        Err(e) => {
            tracing::error!(
                error = %e,
                error_kind = ?e.kind(),
                cwd = %cwd.display(),
                "git_cli: Command::output() FAILED (spawn error)"
            );
            if e.kind() == std::io::ErrorKind::NotFound && cwd.is_dir() {
                return Err(anyhow::Error::new(e).context(
                    "Git executable unavailable. Install Git on PATH or set GIT_BIN_PATH to its executable.",
                ));
            }
            return Err(e.into());
        }
    };
    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        tracing::debug!(exit_code = 0, stdout_len = stdout.len(), "git_cli success");
        Ok(stdout)
    } else {
        let stderr = scrub_git_output(&String::from_utf8_lossy(&output.stderr))
            .trim()
            .to_string();
        let code = output.status.code();
        tracing::debug!(exit_code = ?code, stderr = %stderr, "git_cli failed");
        Err(anyhow::anyhow!(
            "{}",
            if stderr.is_empty() {
                "git command failed"
            } else {
                &stderr
            }
        ))
    }
}
/// Mutating [`git_cli`]: bump the gate epoch after the attempt. Failed
/// commands can still change the repo (`pull --rebase` conflicts, partial
/// checkout); skipping invalidate would keep pre-mutation snapshots.
async fn git_cli_mut(cwd: &Path, args: &[&str]) -> Result<String> {
    let result = git_cli(cwd, args).await;
    super::git_gate::invalidate(cwd);
    result
}
/// Run a jj CLI command and return stdout on success, or error with stderr.
///
/// Passes `--ignore-working-copy` to skip the automatic working-copy snapshot
/// that jj performs at the start of every command. This is safe for read-only
/// queries and avoids unnecessary I/O. For mutating commands (`describe`,
/// `new`, `restore`, `workspace add`) use [`jj_cli_mut`] instead.
pub async fn jj_cli(cwd: &Path, args: &[&str]) -> Result<String> {
    jj_cli_inner(cwd, args, true).await
}
/// Run a mutating jj CLI command (no `--ignore-working-copy`).
///
/// Use this for commands that modify state: `describe`, `new`, `restore`,
/// `workspace add/forget`. The working copy will be snapshotted and updated.
pub async fn jj_cli_mut(cwd: &Path, args: &[&str]) -> Result<String> {
    jj_cli_inner(cwd, args, false).await
}
async fn jj_cli_inner(cwd: &Path, args: &[&str], ignore_wc: bool) -> Result<String> {
    tracing::debug!(cwd = %cwd.display(), args = ?args, ignore_wc, "jj_cli");
    let mut cmd = Command::new("jj");
    cmd.current_dir(cwd)
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null());
    echo_agent_tools::util::detach_command(&mut cmd);
    if ignore_wc {
        cmd.arg("--ignore-working-copy");
    }
    let output = match cmd.args(args).output().await {
        Ok(o) => o,
        Err(e) => {
            tracing::error!(
                error = %e,
                error_kind = ?e.kind(),
                cwd = %cwd.display(),
                "jj_cli_inner: Command::output() FAILED (spawn error)"
            );
            return Err(e.into());
        }
    };
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !stderr.is_empty() {
            tracing::warn!(
                cwd = %cwd.display(),
                "jj_cli success with stderr warnings"
            );
        }
        tracing::debug!(exit_code = 0, stdout_len = stdout.len(), "jj_cli success");
        Ok(stdout)
    } else {
        let code = output.status.code();
        tracing::warn!(
            cwd = %cwd.display(),
            exit_code = ?code,
            "jj_cli FAILED"
        );
        Err(anyhow::anyhow!(
            "{}",
            if stderr.is_empty() {
                "jj command failed"
            } else {
                &stderr
            }
        ))
    }
}
/// Detect the VCS kind for a given path based on the discovered git root.
///
/// Checks for `.jj/` directory alongside `.git/` to identify colocated Jujutsu repos.
pub fn detect_vcs_kind(git_root: &Path) -> VcsKind {
    if git_root.join(".jj").is_dir() {
        VcsKind::JujutsuColocated
    } else {
        VcsKind::Git
    }
}
/// Result of attempting to discover a git repository from a path.
#[derive(Debug)]
pub enum GitDiscoveryResult {
    /// Successfully found a git repo; contains the worktree root.
    Found(PathBuf),
    /// The path is definitively not inside a git repository.
    NotARepo,
    /// libgit2 failed for a reason other than "not found" (e.g. permissions,
    /// unsupported extensions, corrupt repo). The user may or may not be in a
    /// git repo — we can't tell.
    DiscoveryFailed(anyhow::Error),
}
/// Discover whether `path` is inside a git repository.
///
/// Returns [`GitDiscoveryResult::Found`] with the worktree root on success,
/// [`GitDiscoveryResult::NotARepo`] when the path is definitively outside any
/// repo, or [`GitDiscoveryResult::DiscoveryFailed`] when libgit2 errors for
/// an unexpected reason (so callers can avoid false-positive "not a repo"
/// decisions).
pub fn discover_git_root(path: &Path) -> GitDiscoveryResult {
    match Repository::discover(path) {
        Ok(repo) => match repo.workdir() {
            Some(root) => GitDiscoveryResult::Found(root.to_path_buf()),
            None => GitDiscoveryResult::DiscoveryFailed(anyhow::anyhow!(
                "bare git repository: {}",
                path.display()
            )),
        },
        Err(e) => {
            let is_not_found =
                e.code() == git2::ErrorCode::NotFound && e.class() == git2::ErrorClass::Repository;
            if is_not_found {
                GitDiscoveryResult::NotARepo
            } else {
                GitDiscoveryResult::DiscoveryFailed(e.into())
            }
        }
    }
}
#[allow(
    dead_code,
    reason = "Phase 1 internal git helper; will be used by WorkspaceChannel git operations"
)]
pub(crate) fn strip_url_credentials(url_str: &str) -> String {
    if let Ok(mut parsed) = Url::parse(url_str) {
        let _ = parsed.set_username("");
        let _ = parsed.set_password(None);
        return parsed.to_string();
    }
    url_str.to_string()
}
/// Scrub credentials from any URL embedded in free-form git output before it is
/// returned to a caller or logged. A `github` remote may carry
/// `https://x-access-token:TOKEN@host/...`, and git echoes the remote URL in
/// push/fetch errors — so the token would otherwise leak to the FE and logs.
/// Rewrites `scheme://<userinfo>@` → `scheme://` in place, preserving the rest
/// (line structure, quotes, trailing punctuation) so diagnostics stay readable.
pub(crate) fn scrub_git_output(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut remaining = text;
    while let Some(pos) = remaining.find("://") {
        let after = pos + "://".len();
        let auth_end = remaining[after..]
            .find(|c: char| {
                c == '/' || c == '?' || c == '#' || c == '\'' || c == '"' || c.is_whitespace()
            })
            .map(|e| after + e)
            .unwrap_or(remaining.len());
        let authority = &remaining[after..auth_end];
        result.push_str(&remaining[..after]);
        match authority.rfind('@') {
            Some(at) => result.push_str(&authority[at + 1..]),
            None => result.push_str(authority),
        }
        remaining = &remaining[auth_end..];
    }
    result.push_str(remaining);
    result
}
/// Normalize a git remote URL to a transport-agnostic canonical form.
///
/// Produces `host/path` (lowercase host, no scheme, no `.git` suffix,
/// no credentials, no port). Both SSH and HTTPS URLs for the same repo
/// produce identical output.
///
/// Returns `None` for URLs that cannot be meaningfully normalized
/// (e.g. `file://` paths, empty strings).
///
/// # Examples
///
/// ```
/// use echo_agent_workspace::session::git::normalize_repo_url;
///
/// assert_eq!(
///     normalize_repo_url("git@github.com:org/repo.git"),
///     Some("github.com/org/repo".into()),
/// );
/// assert_eq!(
///     normalize_repo_url("https://github.com/org/repo.git"),
///     Some("github.com/org/repo".into()),
/// );
/// assert_eq!(normalize_repo_url("file:///tmp/repo"), None);
/// ```
pub fn normalize_repo_url(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    if !url.contains("://") {
        return normalize_scp_url(url);
    }
    if let Ok(parsed) = Url::parse(url) {
        return normalize_parsed_url(&parsed);
    }
    None
}
/// `git@host:path` or `host:path` → `host/path`
fn normalize_scp_url(url: &str) -> Option<String> {
    let after_user = match url.find('@') {
        Some(pos) => &url[pos + 1..],
        None => url,
    };
    let colon = after_user.find(':')?;
    let host = &after_user[..colon];
    let path = &after_user[colon + 1..];
    if host.is_empty() || path.is_empty() {
        return None;
    }
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    if path.is_empty() {
        return None;
    }
    Some(format!("{}/{}", host.to_ascii_lowercase(), path))
}
/// Standard URL (`https://`, `ssh://`, `git://`, `http://`) → `host/path`
fn normalize_parsed_url(parsed: &Url) -> Option<String> {
    if parsed.scheme() == "file" {
        return None;
    }
    let host = parsed.host_str().filter(|h| !h.is_empty())?;
    let path = parsed.path().trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    if path.is_empty() {
        return None;
    }
    Some(format!("{}/{}", host.to_ascii_lowercase(), path))
}
/// Collect normalized repo remote URLs for the repo at `cwd`.
///
/// Returns an empty vec if `cwd` is not inside a git repo or has no remotes.
pub fn resolve_normalized_remote_urls(cwd: &Path) -> Vec<String> {
    let repo = match Repository::discover(cwd) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let mut urls = Vec::new();
    if let Ok(names) = repo.remotes() {
        for name in names.iter().filter_map(|name| name.ok().flatten()) {
            if let Ok(remote) = repo.find_remote(name)
                && let Ok(raw) = remote.url()
                && let Some(n) = normalize_repo_url(raw)
            {
                urls.push(n);
            }
        }
    }
    urls.sort_unstable();
    urls.dedup();
    urls
}
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PersistedGitMetadata {
    pub git_root_dir: Option<String>,
    pub git_remotes: Vec<String>,
    /// HEAD's OID as `git rev-parse HEAD` reports it (not verified to exist).
    pub head_commit: Option<String>,
    pub head_branch: Option<String>,
}
/// Resolve session-persistence git metadata: worktree root, HEAD, and
/// deduplicated, credential-stripped remotes. Reads refs and config only, via
/// the repo's commondir so linked worktrees resolve like the main repo.
pub fn resolve_persisted_session_git_metadata_sync(cwd: &Path) -> PersistedGitMetadata {
    let git_root = match discover_git_root(cwd) {
        GitDiscoveryResult::Found(root) => root,
        _ => return PersistedGitMetadata::default(),
    };
    let repo = match Repository::discover(cwd) {
        Ok(r) => r,
        Err(_) => {
            return PersistedGitMetadata {
                git_root_dir: Some(git_root.to_string_lossy().to_string()),
                git_remotes: Vec::new(),
                head_commit: None,
                head_branch: None,
            };
        }
    };
    let mut remotes = BTreeSet::new();
    if let Ok(remote_names) = repo.remotes() {
        for name in remote_names.iter().filter_map(|name| name.ok().flatten()) {
            if let Ok(remote) = repo.find_remote(name)
                && let Ok(url) = remote.url()
            {
                remotes.insert(strip_url_credentials(url));
            }
        }
    }
    let head_ref = head_reference(&repo);
    let head_commit = head_ref
        .as_ref()
        .and_then(|h| h.target())
        .map(|oid| oid.to_string());
    let head_branch = head_ref.as_ref().and_then(|h| {
        h.shorthand()
            .ok()
            .filter(|s| *s != "HEAD")
            .map(str::to_owned)
    });
    PersistedGitMetadata {
        git_root_dir: Some(git_root.to_string_lossy().to_string()),
        git_remotes: remotes.into_iter().collect(),
        head_commit,
        head_branch,
    }
}
pub fn find_git_root_from_path(path: &Path) -> Result<PathBuf> {
    match discover_git_root(path) {
        GitDiscoveryResult::Found(root) => Ok(root),
        GitDiscoveryResult::NotARepo => {
            anyhow::bail!("not a git repository: {}", path.display())
        }
        GitDiscoveryResult::DiscoveryFailed(e) => Err(e),
    }
}
/// Find the main repo root (not the worktree working directory).
/// For regular repos this is the same as find_git_root_from_path.
/// For worktrees, this returns the parent repo's root.
/// Use this for worktree management operations (create/remove/apply).
pub fn find_main_repo_root_from_path(path: &Path) -> Result<PathBuf> {
    let repo = Repository::discover(path)?;
    repo.commondir()
        .parent()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| anyhow::anyhow!("Invalid git repository: {}", path.display()))
}
pub fn change_type_from_git2_delta(delta: git2::Delta) -> ChangeType {
    match delta {
        git2::Delta::Added => ChangeType::Create,
        git2::Delta::Deleted => ChangeType::Delete,
        git2::Delta::Modified => ChangeType::Edit,
        git2::Delta::Renamed => ChangeType::Rename,
        git2::Delta::Copied => ChangeType::Copy,
        git2::Delta::Typechange => ChangeType::Typechange,
        git2::Delta::Untracked => ChangeType::Untracked,
        other => {
            tracing::warn!(?other, "unexpected git delta type, treating as Edit");
            ChangeType::Edit
        }
    }
}
fn change_type_from_git2_status(s: git2::Status, staged: bool) -> ChangeType {
    use git2::Status;
    if !staged && s.contains(Status::WT_NEW) {
        return ChangeType::Untracked;
    }
    if staged {
        if s.contains(Status::INDEX_NEW) {
            ChangeType::Create
        } else if s.contains(Status::INDEX_DELETED) {
            ChangeType::Delete
        } else if s.contains(Status::INDEX_RENAMED) {
            ChangeType::Rename
        } else if s.contains(Status::INDEX_TYPECHANGE) {
            ChangeType::Typechange
        } else {
            ChangeType::Edit
        }
    } else if s.contains(Status::WT_DELETED) {
        ChangeType::Delete
    } else if s.contains(Status::WT_RENAMED) {
        ChangeType::Rename
    } else if s.contains(Status::WT_TYPECHANGE) {
        ChangeType::Typechange
    } else {
        ChangeType::Edit
    }
}
fn has_index_changes(s: git2::Status) -> bool {
    s.intersects(
        git2::Status::INDEX_NEW
            | git2::Status::INDEX_MODIFIED
            | git2::Status::INDEX_DELETED
            | git2::Status::INDEX_RENAMED
            | git2::Status::INDEX_TYPECHANGE,
    )
}
fn has_worktree_changes(s: git2::Status) -> bool {
    s.intersects(
        git2::Status::WT_NEW
            | git2::Status::WT_MODIFIED
            | git2::Status::WT_DELETED
            | git2::Status::WT_RENAMED
            | git2::Status::WT_TYPECHANGE,
    )
}
/// A git reference for diffing: either a working state (Index/Workdir) or a commit-ish.
#[derive(Clone, Debug, PartialEq, Eq)]
enum GitRef {
    Index,
    Workdir,
    Treeish(String),
}
impl GitRef {
    fn parse(s: &str) -> Self {
        match s {
            "staged" => Self::Index,
            "working" => Self::Workdir,
            _ => Self::Treeish(s.to_string()),
        }
    }
    fn is_working_state(&self) -> bool {
        matches!(self, Self::Index | Self::Workdir)
    }
}
pub async fn get_branch(cwd: &Path) -> Option<String> {
    git_cli(cwd, &["branch", "--show-current"])
        .await
        .ok()
        .filter(|b| !b.is_empty())
}
/// Path-component tilde collapse (`~/src/repo`). String prefix matching would
/// treat `HOME=/Users/u` as a prefix of `/Users/user/echoagent`.
fn collapse_home_path(path: &Path, home: Option<&Path>) -> String {
    let Some(home) = home else {
        return path.display().to_string();
    };
    let home = PathBuf::from(
        home.as_os_str()
            .to_string_lossy()
            .trim_end_matches(['/', '\\']),
    );
    path.strip_prefix(&home)
        .map(|rest| format!("~/{}", rest.display()))
        .unwrap_or_else(|_| path.display().to_string())
}
/// Returns (is_worktree, main_repo_display_name) if this is a git checkout.
/// `None` when `cwd` is not inside a repo. The display name is the main repo
/// path, preferably relative to $HOME as ~...
pub async fn get_worktree_info(cwd: &Path) -> Option<(bool, Option<String>)> {
    let cwd = cwd.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let repo = Repository::discover(&cwd).ok()?;
        let display = |path: &Path| -> String {
            collapse_home_path(path, std::env::var("HOME").ok().as_deref().map(Path::new))
        };
        let mut marker_main = None;
        for ancestor in cwd.ancestors() {
            let git = ancestor.join(".git");
            if let Ok(contents) = std::fs::read_to_string(git.join("echo-agent-worktree-source"))
                && let trimmed = contents.trim()
                && !trimmed.is_empty()
            {
                marker_main = Some(display(Path::new(trimmed)));
                break;
            }
            if git.exists() {
                break;
            }
        }
        let mut db_main = None;
        let mut db_label = None;
        if let Ok(db) = crate::worktree::open_db() {
            for ancestor in cwd.ancestors() {
                if let Ok(Some(record)) = db.get(&ancestor.to_string_lossy()) {
                    db_label = record.label().map(str::to_owned).filter(|s| !s.is_empty());
                    if !record.source_repo.as_os_str().is_empty() {
                        db_main = Some(display(&record.source_repo));
                    }
                    break;
                }
                if ancestor.join(".git").exists() {
                    break;
                }
            }
        }
        let linked_main = (repo.path() != repo.commondir())
            .then(|| repo.commondir().parent().map(display))
            .flatten();
        let main_repo = marker_main.or(db_main).or(linked_main);
        let is_worktree = main_repo.is_some() || db_label.is_some();
        Some((is_worktree, main_repo))
    })
    .await
    .ok()
    .flatten()
}
/// Switch the working tree to a different branch, optionally creating it.
///
/// Refuses to switch if the working tree is dirty (staged or unstaged changes)
/// to avoid losing work. The dirty check uses `git2` (no subprocess).
pub async fn checkout_branch(git_root: &Path, branch: &str, create: bool) -> Result<()> {
    let root = git_root.to_path_buf();
    let has_changes = tokio::task::spawn_blocking(move || -> Result<bool> {
        let repo = Repository::discover(&root)?;
        let mut opts = StatusOptions::new();
        opts.include_untracked(false)
            .include_ignored(false)
            .exclude_submodules(true);
        let statuses = repo.statuses(Some(&mut opts))?;
        Ok(!statuses.is_empty())
    })
    .await??;
    if has_changes {
        anyhow::bail!(
            "working tree has uncommitted changes; commit or stash before switching branches"
        );
    }
    if create {
        git_cli_mut(git_root, &["checkout", "-b", branch]).await?;
    } else {
        git_cli_mut(git_root, &["checkout", branch]).await?;
    }
    Ok(())
}
async fn get_upstream(cwd: &Path) -> Option<String> {
    git_cli(cwd, &["rev-parse", "--abbrev-ref", "@{upstream}"])
        .await
        .ok()
}
async fn get_remote_url(cwd: &Path) -> Option<String> {
    git_cli(cwd, &["remote", "get-url", "origin"]).await.ok()
}
/// Compute how many commits the local branch is ahead/behind its upstream.
/// Returns (ahead, behind) or None if there's no upstream or an error occurs.
fn compute_ahead_behind(repo: &Repository) -> Option<(usize, usize)> {
    let head = repo.head().ok()?;
    let local_oid = head.target()?;
    let branch_name = head.shorthand().ok()?;
    let local_branch = repo
        .find_branch(branch_name, git2::BranchType::Local)
        .ok()?;
    let upstream = local_branch.upstream().ok()?;
    let upstream_oid = upstream.get().target()?;
    repo.graph_ahead_behind(local_oid, upstream_oid).ok()
}
/// HEAD resolved from refs only. `None` before the first commit; other failures
/// are logged and also `None`.
///
/// Never peel to the commit: on a huge or corrupt pack that read can hang,
/// allocating until the process dies — it hung session startup, and the file
/// watcher hits this on every git event.
fn head_reference(repo: &Repository) -> Option<Reference<'_>> {
    match repo.head() {
        Ok(head) => Some(head),
        Err(e) if e.code() == git2::ErrorCode::UnbornBranch => None,
        Err(e) => {
            tracing::debug!(
                path = %repo.path().display(),
                code = ?e.code(),
                error = %e,
                "git.head: unresolvable",
            );
            None
        }
    }
}
/// The hash stored in HEAD, as `git rev-parse HEAD` prints it. Never loads the
/// object, so it may name one this repo does not have (see [`head_reference`]).
fn head_sha(repo: &Repository) -> Option<String> {
    Some(head_reference(repo)?.target()?.to_string())
}
fn read_blob_from_tree(repo: &Repository, tree: &git2::Tree, path: &str) -> Result<Vec<u8>> {
    let entry = tree
        .get_path(Path::new(path))
        .map_err(|e| anyhow::anyhow!("path '{}' not found in commit: {}", path, e))?;
    let blob = repo
        .find_blob(entry.id())
        .map_err(|e| anyhow::anyhow!("failed to read file content for '{}': {}", path, e))?;
    Ok(blob.content().to_vec())
}
fn read_blob_from_index(repo: &Repository, path: &str) -> Result<Vec<u8>> {
    let index = repo.index()?;
    let entry = index
        .get_path(Path::new(path), 0)
        .ok_or_else(|| anyhow::anyhow!("path '{}' not found in staging area", path))?;
    let blob = repo.find_blob(entry.id)?;
    Ok(blob.content().to_vec())
}
fn resolve_tree<'a>(repo: &'a Repository, refspec: &str) -> Option<git2::Tree<'a>> {
    repo.revparse_single(refspec).ok()?.peel_to_tree().ok()
}
fn resolve_oid(repo: &Repository, refspec: &str) -> Option<git2::Oid> {
    repo.revparse_single(refspec)
        .ok()
        .and_then(|obj| obj.peel_to_commit().ok())
        .map(|c| c.id())
}
fn compute_merge_base(repo: &Repository, base: &str, head: &str) -> Option<git2::Oid> {
    let base_oid = resolve_oid(repo, base)?;
    let head_oid = resolve_oid(repo, head)?;
    repo.merge_base(base_oid, head_oid).ok()
}
/// Diff working state: staging panel use case (Index/Workdir combinations).
fn diff_working_state<'a>(
    repo: &'a Repository,
    base: &GitRef,
    head: &GitRef,
    opts: &mut DiffOptions,
) -> Option<git2::Diff<'a>> {
    use GitRef::*;
    match (base, head) {
        (Treeish(refspec), Index) => {
            let tree = resolve_tree(repo, refspec)?;
            repo.diff_tree_to_index(Some(&tree), None, Some(opts)).ok()
        }
        (Index, Workdir) => repo.diff_index_to_workdir(None, Some(opts)).ok(),
        (Treeish(refspec), Workdir) => {
            let tree = resolve_tree(repo, refspec)?;
            repo.diff_tree_to_workdir_with_index(Some(&tree), Some(opts))
                .ok()
        }
        _ => None,
    }
}
/// Diff refs: PR/branch comparison use case (tree-to-tree).
/// When `merge_base` is true, uses the common ancestor as base (GitHub PR behavior).
fn diff_compare<'a>(
    repo: &'a Repository,
    base: &str,
    head: &str,
    merge_base: bool,
    opts: &mut DiffOptions,
) -> Option<git2::Diff<'a>> {
    let effective_base = if merge_base {
        compute_merge_base(repo, base, head)
            .map(|oid| oid.to_string())
            .unwrap_or_else(|| base.to_string())
    } else {
        base.to_string()
    };
    let base_tree = resolve_tree(repo, &effective_base)?;
    let head_tree = resolve_tree(repo, head)?;
    repo.diff_tree_to_tree(Some(&base_tree), Some(&head_tree), Some(opts))
        .ok()
}
/// Routes to diff_working_state or diff_compare based on ref types.
fn create_diff<'a>(
    repo: &'a Repository,
    base: &GitRef,
    head: &GitRef,
    merge_base: bool,
    opts: &mut DiffOptions,
) -> Option<git2::Diff<'a>> {
    if base.is_working_state() || head.is_working_state() {
        diff_working_state(repo, base, head, opts)
    } else {
        let GitRef::Treeish(base_ref) = base else {
            return None;
        };
        let GitRef::Treeish(head_ref) = head else {
            return None;
        };
        diff_compare(repo, base_ref, head_ref, merge_base, opts)
    }
}
fn read_version_bytes(repo: &Repository, path: &str, version: &str) -> Result<Vec<u8>> {
    match GitRef::parse(version) {
        GitRef::Workdir => {
            let full_path = repo
                .workdir()
                .ok_or_else(|| anyhow::anyhow!("cannot read working files from bare repository"))?
                .join(path);
            std::fs::read(&full_path)
                .map_err(|e| anyhow::anyhow!("failed to read '{}': {}", path, e))
        }
        GitRef::Index => read_blob_from_index(repo, path),
        GitRef::Treeish(refspec) => {
            let obj = repo
                .revparse_single(&refspec)
                .map_err(|e| anyhow::anyhow!("'{}' is not a valid revision: {}", refspec, e))?;
            let commit = obj
                .peel_to_commit()
                .map_err(|e| anyhow::anyhow!("'{}' does not refer to a commit: {}", refspec, e))?;
            let tree = commit.tree()?;
            read_blob_from_tree(repo, &tree, path)
        }
    }
}
/// Binary files return empty content with is_binary=true.
fn read_version_content(repo: &Repository, path: &str, version: &str) -> Result<(String, bool)> {
    let bytes = read_version_bytes(repo, path, version)?;
    match String::from_utf8(bytes) {
        Ok(text) => Ok((text, false)),
        Err(_) => Ok((String::new(), true)),
    }
}
fn read_version_text(repo: &Repository, path: &str, version: &str) -> Option<String> {
    read_version_content(repo, path, version)
        .ok()
        .filter(|(_, is_binary)| !is_binary)
        .map(|(text, _)| text)
}
#[derive(Clone, Default)]
struct DiffFileStats {
    additions: u64,
    deletions: u64,
    patch: Option<String>,
    patch_bytes: Option<u64>,
    patch_lines: Option<u64>,
    old_path: Option<String>,
    delta: Option<git2::Delta>,
}
fn extract_old_path(delta: &git2::DiffDelta, new_path: &str) -> Option<String> {
    if matches!(delta.status(), git2::Delta::Renamed | git2::Delta::Copied) {
        delta
            .old_file()
            .path()
            .map(|p| p.to_string_lossy().to_string())
            .filter(|op| op != new_path)
    } else {
        None
    }
}
struct DiffStatsResult {
    stats: HashMap<String, DiffFileStats>,
    paths: Vec<String>,
}
fn collect_diff_stats(
    repo: &Repository,
    from: &str,
    to: &str,
    pathspecs: Option<&[String]>,
    include_patch: bool,
) -> DiffStatsResult {
    let base_ref = GitRef::parse(from);
    let head_ref = GitRef::parse(to);
    let mut opts = DiffOptions::new();
    opts.ignore_submodules(true);
    if matches!(head_ref, GitRef::Workdir) {
        opts.include_untracked(true)
            .recurse_untracked_dirs(true)
            .show_untracked_content(true);
    }
    if let Some(specs) = pathspecs {
        for spec in specs {
            opts.pathspec(spec);
        }
    }
    let diff = match create_diff(repo, &base_ref, &head_ref, false, &mut opts) {
        Some(d) => d,
        None => {
            return DiffStatsResult {
                stats: HashMap::new(),
                paths: Vec::new(),
            };
        }
    };
    let mut stats = HashMap::new();
    let mut paths = Vec::new();
    for (idx, delta) in diff.deltas().enumerate() {
        let path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        if path.is_empty() {
            continue;
        }
        paths.push(path.clone());
        let (additions, deletions, patch, patch_bytes, patch_lines) =
            if let Ok(Some(mut patch_obj)) = git2::Patch::from_diff(&diff, idx) {
                let (_, adds, dels) = patch_obj.line_stats().unwrap_or((0, 0, 0));
                let (patch_text, p_bytes, p_lines) = if include_patch {
                    match patch_obj.to_buf() {
                        Ok(buf) => {
                            let text = buf.as_str().unwrap_or("");
                            let bytes = text.len() as u64;
                            let lines = text.lines().count() as u64;
                            (Some(text.to_string()), Some(bytes), Some(lines))
                        }
                        Err(_) => (None, None, None),
                    }
                } else {
                    (None, None, None)
                };
                (adds as u64, dels as u64, patch_text, p_bytes, p_lines)
            } else {
                (0, 0, None, None, None)
            };
        stats.insert(
            path.clone(),
            DiffFileStats {
                additions,
                deletions,
                patch,
                patch_bytes,
                patch_lines,
                old_path: extract_old_path(&delta, &path),
                delta: Some(delta.status()),
            },
        );
    }
    DiffStatsResult { stats, paths }
}
/// Payload for the `echo.agent/git_head_changed` ACP extension notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHeadChanged {
    pub session_id: String,
    pub branch: Option<String>,
    #[serde(default)]
    pub is_worktree: bool,
    #[serde(default)]
    pub main_repo: Option<String>,
}
/// Discover the git root, current branch, and remote URLs.
/// Uses `git2` — no subprocess.
pub async fn git_info(cwd: &Path) -> Result<GitInfoData> {
    let cwd = cwd.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let repo = Repository::discover(&cwd)?;
        let root = dunce::canonicalize(repo.workdir().unwrap_or_else(|| repo.path()))
            .unwrap_or_else(|_| repo.workdir().unwrap_or_else(|| repo.path()).to_path_buf());
        let current_branch = repo
            .head()
            .ok()
            .and_then(|h| h.shorthand().ok().map(String::from));
        let mut remotes_set = BTreeSet::new();
        if let Ok(remote_names) = repo.remotes() {
            for name in remote_names.iter().filter_map(|name| name.ok().flatten()) {
                if let Ok(remote) = repo.find_remote(name)
                    && let Ok(url) = remote.url()
                {
                    remotes_set.insert(url.to_string());
                }
            }
        }
        let default_branch = detect_default_branch(&repo);
        let vcs = detect_vcs_kind(&root);
        Ok(GitInfoData {
            root: root.to_string_lossy().into_owned(),
            remotes: remotes_set.into_iter().collect(),
            current_branch,
            default_branch,
            vcs_kind: Some(vcs),
        })
    })
    .await?
}
/// Detect the default branch for this repository.
///
/// Priority:
/// 1. `refs/remotes/origin/HEAD` symbolic ref (set by `git clone` or
///    `git remote set-head origin --auto`).
/// 2. `init.defaultBranch` git config value (user/system preference).
fn detect_default_branch(repo: &Repository) -> Option<String> {
    if let Some(branch) = detect_remote_default_branch(repo) {
        return Some(branch);
    }
    if let Ok(config) = repo.config()
        && let Ok(val) = config.get_string("init.defaultBranch")
    {
        return Some(val);
    }
    None
}
/// Resolve `refs/remotes/origin/HEAD` to the remote's default branch name.
fn detect_remote_default_branch(repo: &Repository) -> Option<String> {
    let reference = repo.find_reference("refs/remotes/origin/HEAD").ok()?;
    let prefix = "refs/remotes/origin/";
    if let Ok(resolved) = reference.resolve()
        && let Some(branch) = resolved.name().ok().and_then(|n| n.strip_prefix(prefix))
    {
        return Some(branch.to_string());
    }
    reference
        .symbolic_target()
        .ok()
        .flatten()
        .and_then(|t| t.strip_prefix(prefix))
        .map(|b| b.to_string())
}
/// List all local + remote branches.
/// Uses `git2` — no subprocess, no `git status`.
pub async fn list_branches(git_root: &Path) -> Result<GitBranchListData> {
    let root = git_root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let repo = Repository::discover(&root)?;
        let repo_root = dunce::canonicalize(repo.workdir().unwrap_or_else(|| repo.path()))
            .unwrap_or_else(|_| repo.workdir().unwrap_or_else(|| repo.path()).to_path_buf());
        let current_branch = repo
            .head()
            .ok()
            .and_then(|h| h.shorthand().ok().map(String::from));
        let mut branches = Vec::new();
        for result in repo.branches(None)? {
            let (branch, branch_type) = result?;
            let Some(name) = branch.name()?.map(String::from) else {
                continue;
            };
            if branch.get().symbolic_target().ok().flatten().is_some()
                && branch_type == git2::BranchType::Remote
            {
                continue;
            }
            let is_remote = branch_type == git2::BranchType::Remote;
            let is_current = !is_remote && current_branch.as_deref() == Some(name.as_str());
            branches.push(GitBranchEntry {
                name,
                current: is_current,
                remote: is_remote,
            });
        }
        Ok(GitBranchListData {
            current_branch,
            repo_root: repo_root.to_string_lossy().into_owned(),
            branches,
        })
    })
    .await?
}
/// HEAD's commit hash for cache validation and `--restore-code` (see
/// [`head_sha`]). `None` outside a git repo or before the first commit.
pub async fn get_current_commit(git_root: &Path) -> Option<String> {
    let cwd = git_root.to_path_buf();
    tokio::task::spawn_blocking(move || -> Option<String> {
        let repo = Repository::discover(&cwd).ok()?;
        head_sha(&repo)
    })
    .await
    .ok()
    .flatten()
}
/// Parse `git rev-list --left-right --count HEAD...@{upstream}` output into (ahead, behind).
async fn cli_ahead_behind(cwd: &Path) -> (Option<usize>, Option<usize>) {
    match git_cli(
        cwd,
        &["rev-list", "--left-right", "--count", "HEAD...@{upstream}"],
    )
    .await
    {
        Ok(output) => {
            let mut parts = output.split_whitespace();
            let ahead = parts.next().and_then(|s| s.parse().ok());
            let behind = parts.next().and_then(|s| s.parse().ok());
            (ahead, behind)
        }
        Err(_) => (None, None),
    }
}
/// Map a porcelain-v2 status character to our ChangeType.
fn change_type_from_porcelain(ch: char, staged: bool) -> ChangeType {
    match ch {
        'A' => ChangeType::Create,
        'D' => ChangeType::Delete,
        'R' => ChangeType::Rename,
        'C' => ChangeType::Copy,
        'T' => ChangeType::Typechange,
        '?' if !staged => ChangeType::Untracked,
        _ => ChangeType::Edit,
    }
}
/// Parse `git diff --numstat` output into a map of path → (additions, deletions).
///
/// We intentionally omit `-M` from our `git diff --numstat` invocations, so rename
/// entries won't appear in practice. The format is simply `ADDS\tDELS\tPATH`
/// (or `-\t-\tPATH` for binary files).
fn parse_numstat(output: &str) -> HashMap<String, (u64, u64)> {
    let mut map = HashMap::new();
    for line in output.lines() {
        let mut parts = line.splitn(3, '\t');
        let adds = parts.next().and_then(|s| s.parse::<u64>().ok());
        let dels = parts.next().and_then(|s| s.parse::<u64>().ok());
        if let Some(path) = parts.next() {
            map.insert(path.to_string(), (adds.unwrap_or(0), dels.unwrap_or(0)));
        }
    }
    map
}
/// Porcelain-v2 entry for ordinary changes:
///   `1 XY <sub> <mH> <mI> <mW> <hH> <hI> <path>`
/// Rename/copy:
///   `2 XY <sub> <mH> <mI> <mW> <hH> <hI> R<score> <path>\t<origPath>`
/// Untracked:
///   `? <path>`
fn parse_porcelain_v2(
    output: &str,
    include_untracked: bool,
    ignore_submodules: bool,
    git_root: &Path,
    staged_stats: &HashMap<String, (u64, u64)>,
    unstaged_stats: &HashMap<String, (u64, u64)>,
) -> (Vec<GitFileChange>, Vec<GitFileChange>) {
    let mut staged = Vec::new();
    let mut unstaged = Vec::new();
    for line in output.lines() {
        if line.starts_with("# ") || line.is_empty() {
            continue;
        }
        if let Some(path) = line.strip_prefix("? ") {
            if !include_untracked {
                continue;
            }
            if ignore_submodules && git_root.join(path).join(".git").exists() {
                continue;
            }
            unstaged.push(GitFileChange {
                path: path.to_string(),
                old_path: None,
                change_type: ChangeType::Untracked,
                staged: Some(false),
                additions: 0,
                deletions: 0,
                patch: None,
                patch_bytes: None,
                patch_lines: None,
                old_text: None,
                new_text: None,
            });
            continue;
        }
        let is_rename = line.starts_with("2 ");
        let is_unmerged = line.starts_with("u ");
        if !line.starts_with("1 ") && !is_rename && !is_unmerged {
            continue;
        }
        let after_prefix = &line[2..];
        if after_prefix.len() < 4 {
            continue;
        }
        let index_status = after_prefix.as_bytes()[0] as char;
        let worktree_status = after_prefix.as_bytes()[1] as char;
        if ignore_submodules && !after_prefix[3..].starts_with('N') {
            continue;
        }
        let fields_to_skip: usize = if is_unmerged {
            9
        } else if is_rename {
            8
        } else {
            7
        };
        let mut field_end = 0;
        let mut fields_found = 0;
        for _ in 0..fields_to_skip {
            if let Some(pos) = after_prefix[field_end..].find(' ') {
                field_end += pos + 1;
                fields_found += 1;
            } else {
                break;
            }
        }
        if fields_found < fields_to_skip {
            continue;
        }
        let (path, old_path) = if is_rename {
            let path_part = &after_prefix[field_end..];
            if let Some(tab) = path_part.find('\t') {
                (
                    path_part[..tab].to_string(),
                    Some(path_part[tab + 1..].to_string()),
                )
            } else {
                (path_part.to_string(), None)
            }
        } else {
            let path_str = &after_prefix[field_end..];
            if path_str.is_empty() {
                continue;
            }
            (path_str.to_string(), None)
        };
        if index_status != '.' {
            let (adds, dels) = staged_stats.get(&path).copied().unwrap_or((0, 0));
            staged.push(GitFileChange {
                path: path.clone(),
                old_path: old_path.clone(),
                change_type: change_type_from_porcelain(index_status, true),
                staged: Some(true),
                additions: adds,
                deletions: dels,
                patch: None,
                patch_bytes: None,
                patch_lines: None,
                old_text: None,
                new_text: None,
            });
        }
        if worktree_status != '.' {
            let (adds, dels) = unstaged_stats.get(&path).copied().unwrap_or((0, 0));
            unstaged.push(GitFileChange {
                path,
                old_path,
                change_type: change_type_from_porcelain(worktree_status, false),
                staged: Some(false),
                additions: adds,
                deletions: dels,
                patch: None,
                patch_bytes: None,
                patch_lines: None,
                old_text: None,
                new_text: None,
            });
        }
    }
    (staged, unstaged)
}
/// Full git-status via CLI only — used as fallback when libgit2 cannot read the
/// index (e.g. split-index `link` extension).
///
/// **Limitation:** patch content (`patch`, `patch_bytes`, `patch_lines`) is not
/// populated — all entries return `None` for these fields. Currently no caller
/// passes `include_patches=true` via the extension API. If that changes, add
/// `git diff --cached -p` / `git diff -p` parsing here.
async fn status_via_cli(
    git_root: &Path,
    include_untracked: bool,
    include_stats: bool,
    ignore_submodules: bool,
) -> Result<GitStatusData> {
    let start = std::time::Instant::now();
    let root_fut = git_cli(git_root, &["rev-parse", "--show-toplevel"]);
    let commit_fut = git_cli(git_root, &["rev-parse", "HEAD"]);
    let git_dir_fut = git_cli(git_root, &["rev-parse", "--git-dir"]);
    let common_dir_fut = git_cli(git_root, &["rev-parse", "--git-common-dir"]);
    let branch_fut = get_branch(git_root);
    let upstream_fut = get_upstream(git_root);
    let remote_url_fut = get_remote_url(git_root);
    let ahead_behind_fut = cli_ahead_behind(git_root);
    let mut porcelain_args = vec!["status", "--porcelain=v2"];
    if ignore_submodules {
        porcelain_args.push("--ignore-submodules");
    }
    if !include_untracked {
        porcelain_args.push("--untracked-files=no");
    } else {
        porcelain_args.push("--untracked-files=all");
    }
    let porcelain_fut = git_cli(git_root, &porcelain_args);
    let staged_numstat_fut = async {
        if include_stats {
            git_cli(
                git_root,
                &["diff", "--cached", "--numstat", "--ignore-submodules"],
            )
            .await
            .ok()
        } else {
            None
        }
    };
    let unstaged_numstat_fut = async {
        if include_stats {
            git_cli(git_root, &["diff", "--numstat", "--ignore-submodules"])
                .await
                .ok()
        } else {
            None
        }
    };
    let (
        root_res,
        commit_res,
        git_dir_res,
        common_dir_res,
        branch,
        upstream,
        remote_url,
        (ahead, behind),
        porcelain_res,
        staged_numstat,
        unstaged_numstat,
    ) = tokio::join!(
        root_fut,
        commit_fut,
        git_dir_fut,
        common_dir_fut,
        branch_fut,
        upstream_fut,
        remote_url_fut,
        ahead_behind_fut,
        porcelain_fut,
        staged_numstat_fut,
        unstaged_numstat_fut,
    );
    let root = root_res.ok().map(|s| s.trim_end_matches('/').to_string());
    let git_dir = git_dir_res.ok();
    let common_dir = common_dir_res.ok();
    let is_worktree = matches!((&git_dir, &common_dir), (Some(gd), Some(cd)) if gd != cd);
    let main_root = if is_worktree {
        common_dir.and_then(|d| {
            let p = PathBuf::from(&d);
            let resolved = if p.is_absolute() {
                p
            } else {
                git_root.join(&p)
            };
            resolved
                .parent()
                .map(|pp| pp.to_string_lossy().trim_end_matches('/').to_string())
        })
    } else {
        root.clone()
    };
    let commit = commit_res.ok();
    let porcelain = porcelain_res?;
    let staged_stats = staged_numstat
        .as_deref()
        .map(parse_numstat)
        .unwrap_or_default();
    let unstaged_stats = unstaged_numstat
        .as_deref()
        .map(parse_numstat)
        .unwrap_or_default();
    let (staged, unstaged) = parse_porcelain_v2(
        &porcelain,
        include_untracked,
        ignore_submodules,
        git_root,
        &staged_stats,
        &unstaged_stats,
    );
    let data = GitStatusData {
        root,
        main_root,
        is_worktree: Some(is_worktree),
        branch,
        commit,
        upstream,
        remote_url,
        ahead,
        behind,
        staged,
        unstaged,
    };
    tracing::debug!(
        root = ?data.root,
        branch = ?data.branch,
        staged = data.staged.len(),
        unstaged = data.unstaged.len(),
        elapsed = ?start.elapsed(),
        "git.status (CLI fallback)"
    );
    Ok(data)
}
pub async fn status(
    git_root: &Path,
    include_untracked: bool,
    include_stats: bool,
    ignore_submodules: bool,
    include_patches: bool,
) -> Result<GitStatusData> {
    let git_root_buf = git_root.to_path_buf();
    super::git_gate::shared()
        .run(
            git_root,
            super::git_gate::FlightOp::Status {
                include_untracked,
                include_stats,
                ignore_submodules,
                include_patches,
            },
            move || {
                let git_root = git_root_buf.clone();
                async move {
                    status_ungated(
                        &git_root,
                        include_untracked,
                        include_stats,
                        ignore_submodules,
                        include_patches,
                    )
                    .await
                }
            },
        )
        .await
}
async fn status_ungated(
    git_root: &Path,
    include_untracked: bool,
    include_stats: bool,
    ignore_submodules: bool,
    include_patches: bool,
) -> Result<GitStatusData> {
    let start = std::time::Instant::now();
    let cwd = git_root.to_path_buf();
    let (branch, upstream, remote_url) =
        tokio::join!(get_branch(&cwd), get_upstream(&cwd), get_remote_url(&cwd));
    let result = tokio::task::spawn_blocking(move || {
        let repo = match Repository::discover(&cwd) {
            Ok(r) => r,
            Err(e) => {
                return Err(anyhow::anyhow!("not a git repository: {}", e));
            }
        };
        let is_worktree = repo.is_worktree();
        let root = repo
            .workdir()
            .map(|p| p.to_string_lossy().trim_end_matches('/').to_string());
        let main_root = repo
            .commondir()
            .parent()
            .map(|p| p.to_string_lossy().trim_end_matches('/').to_string());
        let commit = head_sha(&repo);
        let (ahead, behind) = compute_ahead_behind(&repo)
            .map(|(a, b)| (Some(a), Some(b)))
            .unwrap_or((None, None));
        let mut opts = StatusOptions::new();
        opts.include_untracked(include_untracked);
        opts.recurse_untracked_dirs(include_untracked);
        opts.update_index(false);
        if ignore_submodules {
            opts.exclude_submodules(true);
        }
        let statuses = repo.statuses(Some(&mut opts))?;
        let need_stats = include_stats || include_patches;
        let staged_stats = if need_stats {
            collect_diff_stats(&repo, "HEAD", "staged", None, include_patches).stats
        } else {
            HashMap::new()
        };
        let unstaged_stats = if need_stats {
            collect_diff_stats(&repo, "staged", "working", None, include_patches).stats
        } else {
            HashMap::new()
        };
        let mut staged = Vec::new();
        let mut unstaged = Vec::new();
        for entry in statuses.iter() {
            let path = entry.path().unwrap_or("").to_string();
            let status = entry.status();
            if has_index_changes(status) {
                let stats = staged_stats.get(&path).cloned().unwrap_or_default();
                staged.push(GitFileChange {
                    path: path.clone(),
                    old_path: stats.old_path,
                    change_type: change_type_from_git2_status(status, true),
                    staged: Some(true),
                    additions: stats.additions,
                    deletions: stats.deletions,
                    patch: if include_patches { stats.patch } else { None },
                    patch_bytes: if include_patches {
                        stats.patch_bytes
                    } else {
                        None
                    },
                    patch_lines: if include_patches {
                        stats.patch_lines
                    } else {
                        None
                    },
                    old_text: None,
                    new_text: None,
                });
            }
            if has_worktree_changes(status) {
                if ignore_submodules
                    && status.contains(git2::Status::WT_NEW)
                    && cwd.join(&path).join(".git").exists()
                {
                    continue;
                }
                let stats = unstaged_stats.get(&path).cloned().unwrap_or_default();
                unstaged.push(GitFileChange {
                    path,
                    old_path: stats.old_path,
                    change_type: change_type_from_git2_status(status, false),
                    staged: Some(false),
                    additions: stats.additions,
                    deletions: stats.deletions,
                    patch: if include_patches { stats.patch } else { None },
                    patch_bytes: if include_patches {
                        stats.patch_bytes
                    } else {
                        None
                    },
                    patch_lines: if include_patches {
                        stats.patch_lines
                    } else {
                        None
                    },
                    old_text: None,
                    new_text: None,
                });
            }
        }
        Ok::<_, anyhow::Error>(GitStatusData {
            root,
            main_root,
            is_worktree: Some(is_worktree),
            branch,
            commit,
            upstream,
            remote_url,
            ahead,
            behind,
            staged,
            unstaged,
        })
    })
    .await?;
    let libgit2_err = match &result {
        Ok(data) => {
            tracing::debug!(
                root = ?data.root,
                branch = ?data.branch,
                staged = data.staged.len(),
                unstaged = data.unstaged.len(),
                elapsed = ?start.elapsed(),
                "git.status"
            );
            return result;
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                elapsed = ?start.elapsed(),
                "git.status: libgit2 failed, falling back to CLI"
            );
            e.to_string()
        }
    };
    status_via_cli(
        git_root,
        include_untracked,
        include_stats || include_patches,
        ignore_submodules,
    )
    .await
    .map_err(|cli_err| {
        cli_err.context(format!(
            "CLI fallback also failed (libgit2 error: {libgit2_err})"
        ))
    })
}
pub async fn read_files(
    git_root: &Path,
    paths: &[String],
    version: &str,
) -> Result<GitReadFilesData> {
    let start = std::time::Instant::now();
    let cwd = git_root.to_path_buf();
    let paths = paths.to_vec();
    let version = version.to_string();
    let result = tokio::task::spawn_blocking(move || {
        let repo = Repository::discover(&cwd)?;
        let git_root = repo
            .workdir()
            .ok_or_else(|| anyhow::anyhow!("cannot read files from bare repository"))?;
        let paths: Vec<String> = paths
            .into_iter()
            .map(|p| {
                if Path::new(&p).is_absolute() {
                    Path::new(&p)
                        .strip_prefix(git_root)
                        .map(|rel| rel.to_string_lossy().to_string())
                        .map_err(|_| {
                            anyhow::anyhow!(
                                "path '{}' is not within git repository '{}'",
                                p,
                                git_root.display()
                            )
                        })
                } else {
                    Ok(p)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut files = Vec::new();
        let mut errors = Vec::new();
        for path in &paths {
            match read_version_content(&repo, path, &version) {
                Ok((content, is_binary)) => {
                    files.push(GitReadFile {
                        path: path.clone(),
                        version: version.clone(),
                        content,
                        is_binary: Some(is_binary),
                    });
                }
                Err(e) => {
                    errors.push(GitError {
                        path: Some(path.clone()),
                        code: "READ_FAILED".to_string(),
                        message: e.to_string(),
                    });
                }
            }
        }
        Ok::<_, anyhow::Error>(GitReadFilesData { files, errors })
    })
    .await?;
    match &result {
        Ok(data) => {
            tracing::debug!(
                files = data.files.len(),
                errors = data.errors.len(),
                elapsed = ?start.elapsed(),
                "git.files"
            )
        }
        Err(e) => {
            tracing::debug!(error = %e, elapsed = ?start.elapsed(), "git.files failed")
        }
    }
    result
}
pub async fn diffs(
    git_root: &Path,
    paths: Option<&[String]>,
    from: &str,
    to: &str,
    include_patch: bool,
    include_content: bool,
    merge_base: bool,
) -> Result<GitDiffsData> {
    let git_root_buf = git_root.to_path_buf();
    let paths_buf = paths.map(|p| p.to_vec());
    let from_owned = from.to_string();
    let to_owned = to.to_string();
    let mut key_paths = paths_buf.clone().unwrap_or_default();
    key_paths.sort();
    super::git_gate::shared()
        .run(
            git_root,
            super::git_gate::FlightOp::Diff {
                from: from_owned.clone(),
                to: to_owned.clone(),
                merge_base,
                include_patch,
                include_content,
                paths: key_paths,
            },
            move || {
                let git_root = git_root_buf.clone();
                let paths = paths_buf.clone();
                let from = from_owned.clone();
                let to = to_owned.clone();
                async move {
                    diffs_ungated(
                        &git_root,
                        paths.as_deref(),
                        &from,
                        &to,
                        include_patch,
                        include_content,
                        merge_base,
                    )
                    .await
                }
            },
        )
        .await
}
async fn diffs_ungated(
    git_root: &Path,
    paths: Option<&[String]>,
    from: &str,
    to: &str,
    include_patch: bool,
    include_content: bool,
    merge_base: bool,
) -> Result<GitDiffsData> {
    let start = std::time::Instant::now();
    let cwd = git_root.to_path_buf();
    let paths = paths.map(|p| p.to_vec());
    let from = from.to_string();
    let to = to.to_string();
    let result = tokio::task::spawn_blocking(move || {
        let repo = Repository::discover(&cwd)?;
        let work_dir = repo
            .workdir()
            .ok_or_else(|| anyhow::anyhow!("cannot get diffs from bare repository"))?;
        let effective_from = if merge_base {
            match compute_merge_base(&repo, &from, &to) {
                Some(oid) => oid.to_string(),
                None => {
                    tracing::warn!(
                        from = %from,
                        to = %to,
                        "git.diffs: could not compute merge-base, falling back to direct diff"
                    );
                    from.clone()
                }
            }
        } else {
            from.clone()
        };
        let paths = match paths {
            Some(ps) => Some(
                ps.into_iter()
                    .map(|p| {
                        if Path::new(&p).is_absolute() {
                            Path::new(&p)
                                .strip_prefix(work_dir)
                                .map(|rel| rel.to_string_lossy().to_string())
                                .map_err(|_| {
                                    anyhow::anyhow!(
                                        "path '{}' is not within git repository '{}'",
                                        p,
                                        work_dir.display()
                                    )
                                })
                        } else {
                            Ok(p)
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            None => None,
        };
        let DiffStatsResult {
            mut stats,
            paths: changed_paths,
        } = collect_diff_stats(&repo, &effective_from, &to, paths.as_deref(), include_patch);
        let final_paths = match paths {
            Some(p) if !p.is_empty() => p,
            _ => changed_paths,
        };
        let mut files = Vec::new();
        for path in &final_paths {
            let file_stats = stats.remove(path).unwrap_or_default();
            let (old_text, new_text) = if include_content {
                (
                    read_version_text(&repo, path, &effective_from),
                    read_version_text(&repo, path, &to),
                )
            } else {
                (None, None)
            };
            let change_type = file_stats
                .delta
                .map(change_type_from_git2_delta)
                .unwrap_or_else(|| {
                    let old_exists = old_text.as_ref().is_some_and(|s| !s.is_empty());
                    let new_exists = new_text.as_ref().is_some_and(|s| !s.is_empty());
                    match (old_exists, new_exists) {
                        (false, true) => ChangeType::Untracked,
                        (true, false) => ChangeType::Delete,
                        _ => ChangeType::Edit,
                    }
                });
            files.push(GitFileChange {
                path: path.clone(),
                old_path: file_stats.old_path,
                change_type,
                staged: None,
                additions: file_stats.additions,
                deletions: file_stats.deletions,
                patch: file_stats.patch,
                patch_bytes: file_stats.patch_bytes,
                patch_lines: file_stats.patch_lines,
                old_text,
                new_text,
            });
        }
        Ok::<_, anyhow::Error>(GitDiffsData { files })
    })
    .await?;
    match &result {
        Ok(data) => {
            tracing::debug!(files = data.files.len(), elapsed = ?start.elapsed(), "git.diffs")
        }
        Err(e) => {
            tracing::debug!(error = %e, elapsed = ?start.elapsed(), "git.diffs failed")
        }
    }
    result
}
pub fn check_diff_size_limits(
    data: &GitDiffsData,
    max_patch_bytes: Option<usize>,
    max_patch_lines: Option<usize>,
) -> Result<(), DiffSizeExceededError> {
    let exceeded_files: Vec<DiffSizeExceededFile> = data
        .files
        .iter()
        .filter_map(|file| {
            let patch_bytes = file.patch_bytes?;
            let patch_lines = file.patch_lines?;
            let exceeds_bytes = max_patch_bytes.is_some_and(|limit| patch_bytes > limit as u64);
            let exceeds_lines = max_patch_lines.is_some_and(|limit| patch_lines > limit as u64);
            if exceeds_bytes || exceeds_lines {
                Some(DiffSizeExceededFile {
                    path: file.path.clone(),
                    patch_bytes,
                    patch_lines,
                    limit_bytes: if exceeds_bytes {
                        max_patch_bytes.map(|l| l as u64)
                    } else {
                        None
                    },
                    limit_lines: if exceeds_lines {
                        max_patch_lines.map(|l| l as u64)
                    } else {
                        None
                    },
                })
            } else {
                None
            }
        })
        .collect();
    if !exceeded_files.is_empty() {
        return Err(DiffSizeExceededError {
            files: exceeded_files,
        });
    }
    Ok(())
}
pub async fn stage(git_root: &Path, paths: Option<Vec<String>>) -> Result<StageData> {
    let start = std::time::Instant::now();
    let paths_to_stage = paths.unwrap_or_default();
    let result = if paths_to_stage.is_empty() {
        git_cli_mut(git_root, &["add", "-A"]).await
    } else {
        let mut args = vec!["add", "--"];
        args.extend(paths_to_stage.iter().map(String::as_str));
        git_cli_mut(git_root, &args).await
    };
    tracing::debug!(paths = paths_to_stage.len(), elapsed = ?start.elapsed(), "git.stage");
    result.map(|_| StageData {
        paths: paths_to_stage,
    })
}
pub async fn unstage(git_root: &Path, paths: Option<Vec<String>>) -> Result<()> {
    let start = std::time::Instant::now();
    let result = match &paths {
        Some(p) if !p.is_empty() => {
            let mut args = vec!["reset", "HEAD", "--"];
            args.extend(p.iter().map(String::as_str));
            git_cli_mut(git_root, &args).await
        }
        _ => git_cli_mut(git_root, &["reset", "HEAD"]).await,
    };
    tracing::debug!(
        paths = paths.as_ref().map(|v| v.len()).unwrap_or(0),
        elapsed = ?start.elapsed(),
        "git.unstage"
    );
    result.map(|_| ())
}
pub async fn discard(
    git_root: &Path,
    paths: Option<Vec<String>>,
    scope: DiscardScope,
    include_untracked: bool,
) -> Result<()> {
    let start = std::time::Instant::now();
    let path_refs: Vec<&str> = paths
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(String::as_str)
        .collect();
    if matches!(scope, DiscardScope::Staged | DiscardScope::Both) {
        let mut args = vec!["reset", "HEAD"];
        if !path_refs.is_empty() {
            args.push("--");
            args.extend(&path_refs);
        }
        git_cli_mut(git_root, &args).await?;
    }
    if matches!(scope, DiscardScope::Working | DiscardScope::Both) {
        let mut args = vec!["checkout"];
        if path_refs.is_empty() {
            args.push(".");
        } else {
            args.push("--");
            args.extend(&path_refs);
        }
        let result = git_cli_mut(git_root, &args).await;
        if !include_untracked {
            result?;
        }
    }
    if include_untracked {
        let mut args = vec!["clean", "-fd"];
        if !path_refs.is_empty() {
            args.push("--");
            args.extend(&path_refs);
        }
        git_cli_mut(git_root, &args).await?;
    }
    tracing::debug!(paths = path_refs.len(), elapsed = ?start.elapsed(), "git.discard");
    Ok(())
}
pub async fn stash(git_root: &Path, include_untracked: bool) -> Result<()> {
    let start = std::time::Instant::now();
    let mut args = vec!["stash", "push"];
    if include_untracked {
        args.push("--include-untracked");
    }
    git_cli_mut(git_root, &args).await?;
    tracing::debug!(include_untracked, elapsed = ?start.elapsed(), "git.stash");
    Ok(())
}
/// Tracing target used by all `--restore-code` log lines that are NOT
/// scoped to a specific worktree subsystem. Operators filter on this to
/// find restore-code-related warnings.
pub const RESTORE_CODE_LOG: &str = "echo_agent_restore_code";
/// Emit the "session registry disabled" warning shared by both the
/// worktree and non-worktree `--restore-code` paths. Centralised so a
/// future refactor cannot silently downgrade one site to `debug!`.
pub fn warn_registry_disabled_restore(session_id: &str) {
    tracing::warn!(
        target: RESTORE_CODE_LOG,
        session_id,
        "session registry disabled — staged/unstaged/untracked will not be restored"
    );
}
/// Gate for [`warn_registry_disabled_restore`]: the warn should fire
/// only when the working tree is a real git repo (jj has its own
/// changeset model that bypasses the staged/unstaged/untracked concept)
/// AND the registry is unavailable. Exposed so the production gate and
/// its regression test share the same predicate.
pub fn should_warn_registry_disabled(is_jj: bool, registry_present: bool) -> bool {
    !is_jj && !registry_present
}
/// Outcome of a [`checkout_session_commit`] call.
///
/// `checked_out: true` means HEAD is at the requested commit after this
/// call returned — including the no-op early-return where HEAD was
/// already at the target.
#[derive(Debug, Default, Clone)]
pub struct CheckoutSessionOutcome {
    pub checked_out: bool,
    pub stash_ref: Option<String>,
    /// Set when the working tree was dirty but no stash was created (e.g.
    /// an in-progress merge/rebase/cherry-pick blocked it, or `git stash`
    /// itself failed). Callers surface this to the user.
    pub stash_skipped_reason: Option<String>,
}
/// Result of attempting to stash dirty working-tree state.
#[derive(Debug, Clone)]
pub enum StashOutcome {
    /// Working tree was already clean — no stash needed.
    Clean,
    /// Stash created; carries the captured stash ref (commit SHA).
    Stashed(String),
    /// Stash needed but skipped; carries a human-readable reason.
    Skipped(String),
}
const IN_PROGRESS_STATE_FILES: &[&str] = &[
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "REBASE_HEAD",
    "BISECT_LOG",
];
fn in_progress_state_reason(git_root: &Path) -> Option<String> {
    let git_dir = git_root.join(".git");
    for name in IN_PROGRESS_STATE_FILES {
        if git_dir.join(name).exists() {
            return Some(format!(
                "in-progress {} — refusing to stash to preserve operation state",
                name
            ));
        }
    }
    None
}
/// Stash dirty working-tree state (including untracked files) before a
/// destructive operation like `git checkout`.
///
/// Best-effort: returns [`StashOutcome::Skipped`] (with a reason) when an
/// in-progress merge/rebase/cherry-pick/bisect blocks the stash, or when
/// `git stash` itself fails. Returns [`StashOutcome::Clean`] when the
/// tree was already clean.
///
/// `stash push` + `rev-parse stash@{0}` is mostly atomic in practice (the
/// only racer is another concurrent stash in the same repo). The truly
/// atomic `git stash create + stash store` flow is not viable here
/// because `git stash create` does not support `--include-untracked` —
/// using it would silently lose untracked files from the snapshot. On
/// `rev-parse` failure we return `Skipped` rather than a misleading
/// `stash@{0}` literal.
pub async fn stash_before_destructive_op(
    git_root: &Path,
    label: &str,
    session_id: &str,
) -> StashOutcome {
    let dirty = git_cli(git_root, &["status", "--porcelain"])
        .await
        .unwrap_or_default();
    if dirty.trim().is_empty() {
        return StashOutcome::Clean;
    }
    if let Some(reason) = in_progress_state_reason(git_root) {
        tracing::warn!(
            target: RESTORE_CODE_LOG,
            path = %git_root.display(),
            label,
            session_id,
            reason = %reason,
            "stash_before_destructive_op: skipping stash (in-progress operation detected)"
        );
        return StashOutcome::Skipped(reason);
    }
    let message = format!(
        "echoagent: pre-{label} {} {}",
        session_id,
        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
    );
    if let Err(e) = git_cli_mut(
        git_root,
        &["stash", "push", "--include-untracked", "-m", &message],
    )
    .await
    {
        let reason = format!("git stash failed: {e}");
        tracing::warn!(
            target: RESTORE_CODE_LOG,
            path = %git_root.display(),
            label,
            session_id,
            error = %e,
            "stash_before_destructive_op: stash failed, continuing without stash"
        );
        return StashOutcome::Skipped(reason);
    }
    match git_cli(git_root, &["rev-parse", "stash@{0}"]).await {
        Ok(s) if !s.trim().is_empty() => {
            let stash_ref = s.trim().to_owned();
            tracing::info!(
                target: RESTORE_CODE_LOG,
                path = %git_root.display(),
                label,
                session_id,
                stash_ref = %stash_ref,
                "stash_before_destructive_op: dirty state stashed"
            );
            StashOutcome::Stashed(stash_ref)
        }
        _ => {
            let reason = "git rev-parse stash@{0} returned empty or failed".to_owned();
            tracing::warn!(
                target: RESTORE_CODE_LOG,
                path = %git_root.display(),
                label,
                session_id,
                "stash_before_destructive_op: could not capture stash ref after push"
            );
            StashOutcome::Skipped(reason)
        }
    }
}
/// Checkout a specific commit, optionally stashing dirty state first.
///
/// Gracefully degrades: logs warnings but never returns an error.
///
/// Contract: `outcome.checked_out` is `true` when HEAD is at `target_sha`
/// after this call returned, including the no-op early-return where HEAD
/// was already at the target. Callers should rely on this flag when
/// gating user-visible "restored" banners.
///
/// If a fetch is required it runs on `spawn_blocking` and is **not** cancelled
/// when this future is dropped. The helper still kills the git process group
/// when [`crate::restore_fetch::RESTORE_FETCH_BUDGET`] elapses.
pub async fn checkout_session_commit(
    git_root: &Path,
    target_sha: &str,
    stash_if_dirty: bool,
    session_id: &str,
) -> CheckoutSessionOutcome {
    if let Ok(current) = git_cli(git_root, &["rev-parse", "HEAD"]).await
        && current.trim() == target_sha
    {
        tracing::debug!(
            path = %git_root.display(),
            commit = %target_sha,
            "checkout_session_commit: already at target commit"
        );
        return CheckoutSessionOutcome {
            checked_out: true,
            ..Default::default()
        };
    }
    let (stash_ref, stash_skipped_reason) = if stash_if_dirty {
        match stash_before_destructive_op(git_root, "restore-code", session_id).await {
            StashOutcome::Clean => (None, None),
            StashOutcome::Stashed(r) => (Some(r), None),
            StashOutcome::Skipped(reason) => (None, Some(reason)),
        }
    } else {
        (None, None)
    };
    let mut outcome = CheckoutSessionOutcome {
        checked_out: false,
        stash_ref,
        stash_skipped_reason,
    };
    if git_cli_mut(git_root, &["checkout", target_sha])
        .await
        .is_ok()
    {
        tracing::info!(
            path = %git_root.display(),
            commit = %target_sha,
            stash_ref = ?outcome.stash_ref,
            "checkout_session_commit: checked out session HEAD"
        );
        outcome.checked_out = true;
        return outcome;
    }
    if !crate::restore_fetch::is_full_object_id(target_sha) {
        tracing::warn!(
            path = %git_root.display(),
            commit = %target_sha,
            "checkout_session_commit: refusing non-oid fetch refspec, giving up"
        );
        return outcome;
    }
    if git_cli(git_root, &["cat-file", "-t", target_sha])
        .await
        .is_ok()
    {
        tracing::warn!(
            path = %git_root.display(),
            commit = %target_sha,
            "checkout_session_commit: checkout failed with object already local; not fetching"
        );
        return outcome;
    }
    tracing::info!(
        path = %git_root.display(),
        commit = %target_sha,
        "checkout_session_commit: fetching missing session HEAD from origin"
    );
    match fetch_missing_commit(git_root, target_sha).await {
        AsyncFetchOutcome::Completed(Ok(fetch_outcome)) => {
            tracing::info!(
                path = %git_root.display(),
                commit = %target_sha,
                ?fetch_outcome,
                "checkout_session_commit: targeted fetch finished"
            );
        }
        AsyncFetchOutcome::Completed(Err(e)) => {
            tracing::warn!(
                path = %git_root.display(),
                commit = %target_sha,
                error = %e,
                "checkout_session_commit: fetch failed, giving up"
            );
            return outcome;
        }
        AsyncFetchOutcome::Abandoned => {
            tracing::warn!(
                path = %git_root.display(),
                commit = %target_sha,
                "checkout_session_commit: fetch join timed out; not checking out while fetch may still run"
            );
            return outcome;
        }
    }
    if git_cli_mut(git_root, &["checkout", target_sha])
        .await
        .is_ok()
    {
        tracing::info!(
            path = %git_root.display(),
            commit = %target_sha,
            stash_ref = ?outcome.stash_ref,
            "checkout_session_commit: checked out after fetch"
        );
        outcome.checked_out = true;
        return outcome;
    }
    tracing::warn!(
        path = %git_root.display(),
        commit = %target_sha,
        "checkout_session_commit: checkout still failed after fetch, giving up"
    );
    outcome
}
#[derive(Debug)]
pub(crate) enum AsyncFetchOutcome {
    Completed(anyhow::Result<crate::restore_fetch::FetchCommitOutcome>),
    Abandoned,
}
pub(crate) async fn fetch_missing_commit(git_root: &Path, oid: &str) -> AsyncFetchOutcome {
    spawn_targeted_fetch(git_root, oid, FetchKind::CommitOidOnly).await
}
pub(crate) async fn fetch_checkout_target(git_root: &Path, target: &str) -> AsyncFetchOutcome {
    spawn_targeted_fetch(git_root, target, FetchKind::CheckoutRef).await
}
#[derive(Clone, Copy)]
enum FetchKind {
    CommitOidOnly,
    CheckoutRef,
}
async fn spawn_targeted_fetch(git_root: &Path, spec: &str, kind: FetchKind) -> AsyncFetchOutcome {
    let fetch_root = git_root.to_path_buf();
    let fetch_spec = spec.to_owned();
    match tokio::time::timeout(
        crate::restore_fetch::RESTORE_FETCH_BUDGET + crate::restore_fetch::RESTORE_FETCH_JOIN_SLACK,
        tokio::task::spawn_blocking(move || match kind {
            FetchKind::CheckoutRef => {
                crate::restore_fetch::fetch_checkout_target_if_missing(&fetch_root, &fetch_spec)
            }
            FetchKind::CommitOidOnly => {
                crate::restore_fetch::fetch_commit_if_missing(&fetch_root, &fetch_spec)
            }
        }),
    )
    .await
    {
        Err(_) => AsyncFetchOutcome::Abandoned,
        Ok(Err(e)) => {
            AsyncFetchOutcome::Completed(Err(anyhow::anyhow!("targeted fetch task failed: {e}")))
        }
        Ok(Ok(result)) => AsyncFetchOutcome::Completed(result),
    }
}
/// Checkout `head_commit` (full oid or simple ref), fetching from origin if needed.
pub(crate) async fn checkout_commit_with_fetch(
    git_root: &Path,
    head_commit: &str,
    stash_if_dirty: bool,
) -> CheckoutCommitResponse {
    if let Some(current) = get_current_commit(git_root).await
        && current == head_commit
        && git_cli(git_root, &["cat-file", "-t", head_commit])
            .await
            .is_ok()
    {
        return CheckoutCommitResponse {
            checked_out: true,
            stashed: false,
            fetched: false,
            error: None,
        };
    }
    let mut stashed = false;
    if stash_if_dirty {
        let status = git_cli(git_root, &["status", "--porcelain"]).await;
        if let Ok(output) = &status
            && !output.trim().is_empty()
        {
            let msg = format!("auto-stash before checkout {head_commit}");
            if git_cli_mut(git_root, &["stash", "push", "-m", &msg])
                .await
                .is_ok()
            {
                stashed = true;
            }
        }
    }
    if git_cli_mut(git_root, &["checkout", head_commit])
        .await
        .is_ok()
    {
        return CheckoutCommitResponse {
            checked_out: true,
            stashed,
            fetched: false,
            error: None,
        };
    }
    let fetched = match fetch_checkout_target(git_root, head_commit).await {
        AsyncFetchOutcome::Completed(Ok(crate::restore_fetch::FetchCommitOutcome::Fetched)) => {
            tracing::info!(
                path = %git_root.display(),
                target = %head_commit,
                "checkout_commit_with_fetch: fetched target"
            );
            true
        }
        AsyncFetchOutcome::Completed(Ok(
            crate::restore_fetch::FetchCommitOutcome::AlreadyPresent,
        )) => false,
        AsyncFetchOutcome::Completed(Ok(
            crate::restore_fetch::FetchCommitOutcome::SkippedInvalidOid,
        )) => {
            return pop_checkout_auto_stash(
                git_root,
                stashed,
                false,
                format!("unsupported checkout target: {head_commit}"),
            )
            .await;
        }
        AsyncFetchOutcome::Completed(Err(e)) => {
            tracing::warn!(
                path = %git_root.display(),
                target = %head_commit,
                error = %e,
                "checkout_commit_with_fetch: fetch failed"
            );
            if !crate::restore_fetch::is_safe_fetch_refspec(head_commit) {
                return pop_checkout_auto_stash(git_root, stashed, false, e.to_string()).await;
            }
            false
        }
        AsyncFetchOutcome::Abandoned => {
            tracing::warn!(
                path = %git_root.display(),
                target = %head_commit,
                "checkout_commit_with_fetch: fetch join timed out; not checking out"
            );
            return pop_checkout_auto_stash(
                git_root,
                stashed,
                false,
                "targeted fetch timed out".to_owned(),
            )
            .await;
        }
    };
    match git_cli_mut(git_root, &["checkout", head_commit]).await {
        Ok(_) => CheckoutCommitResponse {
            checked_out: true,
            stashed,
            fetched,
            error: None,
        },
        Err(e) => pop_checkout_auto_stash(git_root, stashed, fetched, e.to_string()).await,
    }
}
/// Restore a pre-checkout auto-stash on failure so callers that only inspect
/// `error` are not left on a clean tree with a hidden stash entry.
async fn pop_checkout_auto_stash(
    git_root: &Path,
    stashed: bool,
    fetched: bool,
    error: String,
) -> CheckoutCommitResponse {
    if stashed {
        let _ = git_cli_mut(git_root, &["stash", "pop"]).await;
    }
    CheckoutCommitResponse {
        checked_out: false,
        stashed: false,
        fetched,
        error: Some(error),
    }
}
/// Decide whether a `--restore-code` HEAD checkout is safe to run against
/// `supplied_cwd`.
///
/// The restore-code path may run a targeted
/// `git fetch --no-tags [--depth=1] origin <sha>` + `git checkout <sha>`,
/// which *detaches HEAD*. `--depth=1` is only added when the repo is already
/// shallow. That is only acceptable in two situations:
///
/// 1. `supplied_cwd` is a echo-agent-managed worktree (`~/.echo-agent/worktrees/...`).
///    These are disposable snapshots that exist precisely to carry a
///    detached session HEAD.
/// 2. `supplied_cwd` is exactly the cwd the session was persisted with
///    (`persisted_cwd`) — the original "same-directory restore" intent.
///
/// In every other case — notably a forked-worktree session that was
/// persisted with `git_ref = origin/main` but is later loaded with
/// `cwd = <source repo>` — running the checkout would silently detach the
/// user's real repository and leave their active branch behind, so we
/// refuse.
pub fn restore_code_checkout_allowed(supplied_cwd: &Path, persisted_cwd: Option<&str>) -> bool {
    let worktrees_dir =
        echo_agent_tools::util::echo_agent_home::echo_agent_home().join("worktrees");
    restore_code_checkout_allowed_in(supplied_cwd, persisted_cwd, &worktrees_dir)
}
/// Pure core of [`restore_code_checkout_allowed`] with the worktrees root
/// injected so the decision can be unit-tested without touching
/// `~/.echo-agent`.
fn restore_code_checkout_allowed_in(
    supplied_cwd: &Path,
    persisted_cwd: Option<&str>,
    worktrees_dir: &Path,
) -> bool {
    if supplied_cwd.starts_with(worktrees_dir) {
        return true;
    }
    persisted_cwd
        .map(|p| Path::new(p).components().eq(supplied_cwd.components()))
        .unwrap_or(false)
}
/// Env var backing the `workspace_rewind_git` flag. See [`git_rewind_enabled`].
const REWIND_GIT_ENV: &str = "ECHO_AGENT_WORKSPACE_REWIND_GIT";
/// Whether the git rewind domain (capture + soft restore) is enabled. Default
/// OFF: git is the only domain that moves `HEAD`, so it is gated behind
/// `workspace_rewind_git`.
pub fn git_rewind_enabled() -> bool {
    echo_agent_config::env_bool(REWIND_GIT_ENV).unwrap_or(false)
}
/// Lightweight, in-memory git state captured at a turn boundary. `staged` holds
/// repo-root-relative paths (from `git diff --cached --name-only`), matching what
/// [`restage_git_paths`] re-stages via root-anchored `git add`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStateRef {
    /// HEAD commit SHA at capture time.
    pub head: String,
    /// Repo-root-relative paths with staged (HEAD→index) changes at capture time.
    pub staged: Vec<PathBuf>,
}
/// Per-prompt, in-memory store of captured [`GitStateRef`]s keyed by
/// `prompt_index`. Capture is first-wins (matching `FileStateTracker::begin_prompt`);
/// [`truncate_from`](Self::truncate_from) drops indices `>= target` after a rewind.
#[derive(Debug, Default)]
pub struct GitCheckpointStore {
    by_prompt: Mutex<HashMap<usize, GitStateRef>>,
    /// Prompt indices whose pre-turn capture was already attempted. A re-delivered
    /// begin skips capture (even if the first attempt recorded nothing), so it
    /// can't replace the pre-turn checkpoint with mid-turn state.
    attempted: Mutex<HashSet<usize>>,
}
impl GitCheckpointStore {
    pub fn new() -> Self {
        Self::default()
    }
    /// Record the git state for `prompt_index`, first-wins: a re-delivered begin
    /// must not overwrite the pre-turn state with mid-turn state. Mirrors
    /// `FileStateTracker::begin_prompt`'s `or_insert_with`.
    pub async fn record(&self, prompt_index: usize, state: GitStateRef) {
        self.by_prompt
            .lock()
            .await
            .entry(prompt_index)
            .or_insert(state);
    }
    /// Claim the one-time pre-turn capture for `prompt_index`, returning `true`
    /// only for the first caller; later begins get `false` and must skip capture
    /// (even if the first attempt recorded nothing). Once-per-prompt, like the FS begin.
    pub async fn claim_attempt(&self, prompt_index: usize) -> bool {
        self.attempted.lock().await.insert(prompt_index)
    }
    /// Get the git state captured for `prompt_index`, if any.
    pub async fn get(&self, prompt_index: usize) -> Option<GitStateRef> {
        self.by_prompt.lock().await.get(&prompt_index).cloned()
    }
    /// Whether a git state is recorded for `prompt_index`. Cheaper than
    /// [`get`](Self::get) — no clone of the `GitStateRef`.
    pub async fn contains(&self, prompt_index: usize) -> bool {
        self.by_prompt.lock().await.contains_key(&prompt_index)
    }
    /// Get the checkpoint with the greatest captured index `<= target`, returned
    /// with that index. An exact match at `target` is returned as-is; otherwise
    /// the nearest earlier checkpoint is returned so rewind can still land HEAD
    /// on the closest known-good git state when capture was skipped at the target
    /// or git-rewind was enabled mid-session. `None` only when no checkpoint at
    /// or before `target` exists.
    pub async fn get_at_or_before(&self, target: usize) -> Option<(usize, GitStateRef)> {
        self.by_prompt
            .lock()
            .await
            .iter()
            .filter(|&(&idx, _)| idx <= target)
            .max_by_key(|&(&idx, _)| idx)
            .map(|(&idx, state)| (idx, state.clone()))
    }
    /// Drop all checkpoints at indices `>= prompt_index` (post-rewind cleanup).
    pub async fn truncate_from(&self, prompt_index: usize) {
        self.by_prompt
            .lock()
            .await
            .retain(|&idx, _| idx < prompt_index);
        self.attempted
            .lock()
            .await
            .retain(|&idx| idx < prompt_index);
    }
}
/// Capture the current git state (HEAD + staged paths) for a rewind checkpoint.
/// `cwd` may be a subdirectory; the repo root is resolved so staged paths are
/// repo-root-relative (matching restore's root-anchored `git add`). Best-effort:
/// `None` outside a repo or with an unresolvable `HEAD` — capture must never fail a turn.
pub async fn capture_git_state(cwd: &Path) -> Option<GitStateRef> {
    let git_root = resolve_git_root(cwd).await?;
    let head = get_current_commit(&git_root).await?;
    let staged = staged_paths(&git_root).await?;
    Some(GitStateRef { head, staged })
}
/// Real git commit at a turn boundary. Best-effort — never fails the turn.
/// Stages the worktree and commits on the current branch with `turn {n}`.
pub async fn commit_turn_if_dirty(cwd: &Path, turn_number: u64) -> Option<String> {
    let git_root = resolve_git_root(cwd).await?;
    for pending in [
        "MERGE_HEAD",
        "REBASE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
    ] {
        let in_progress = git_cli_raw(&git_root, &["rev-parse", "-q", "--verify", pending])
            .await
            .map(|(ok, _)| ok)
            .unwrap_or(false);
        if in_progress {
            tracing::debug!(
                turn_number,
                pending,
                cwd = %git_root.display(),
                "turn-boundary commit skipped: a git operation is in progress"
            );
            return None;
        }
    }
    if let Ok(bisect_path) = git_cli(&git_root, &["rev-parse", "--git-path", "BISECT_LOG"]).await {
        let bisect_path = bisect_path.trim();
        if !bisect_path.is_empty() && git_root.join(bisect_path).exists() {
            tracing::debug!(
                turn_number,
                cwd = %git_root.display(),
                "turn-boundary commit skipped: a bisect is in progress"
            );
            return None;
        }
    }
    let status = git_cli(&git_root, &["status", "--porcelain"]).await.ok()?;
    if status.is_empty() {
        return None;
    }
    let _ = seed_default_excludes(&git_root).await;
    let index_tree = match git_cli(&git_root, &["write-tree"]).await {
        Ok(out) if !out.trim().is_empty() => out.trim().to_owned(),
        Ok(_) => {
            tracing::warn!(
                turn_number,
                cwd = %git_root.display(),
                "turn-boundary commit skipped: empty write-tree output"
            );
            return None;
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                turn_number,
                cwd = %git_root.display(),
                "turn-boundary commit skipped: could not snapshot the index"
            );
            return None;
        }
    };
    if let Err(e) = git_cli_mut(&git_root, &["add", "-A"]).await {
        tracing::warn!(
            error = %e,
            turn_number,
            cwd = %git_root.display(),
            "turn-boundary git add skipped"
        );
        let _ = git_cli_mut(&git_root, &["read-tree", &index_tree]).await;
        return None;
    }
    let msg = format!("turn {turn_number}");
    match git_cli_mut(
        &git_root,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            &msg,
            "--no-verify",
        ],
    )
    .await
    {
        Ok(_) => get_current_commit(&git_root).await,
        Err(e) => {
            tracing::warn!(
                error = %e,
                turn_number,
                cwd = %git_root.display(),
                "turn-boundary git commit skipped"
            );
            let _ = git_cli_mut(&git_root, &["read-tree", &index_tree]).await;
            None
        }
    }
}
/// Resolve the worktree root for `cwd` via `git rev-parse --show-toplevel`.
/// Path-sensitive index ops must run from the root so repo-root-relative paths
/// are consistent — a session `cwd` may be a subdirectory.
async fn resolve_git_root(cwd: &Path) -> Option<PathBuf> {
    let out = git_cli(cwd, &["rev-parse", "--show-toplevel"]).await.ok()?;
    let root = out.trim();
    (!root.is_empty()).then(|| PathBuf::from(root))
}
/// List repo-root-relative paths with staged (HEAD→index) changes via
/// `git diff --cached --name-only -z`. `Some(empty)` means nothing staged;
/// `None` means the diff failed, so callers can tell that apart from "nothing
/// staged" instead of recording a lossy empty set. `git_root` must be the worktree root.
async fn staged_paths(git_root: &Path) -> Option<Vec<PathBuf>> {
    let out = match git_cli(git_root, &["diff", "--cached", "--name-only", "-z"]).await {
        Ok(out) => out,
        Err(e) => {
            tracing::warn!(
                path = %git_root.display(),
                error = %e,
                "staged_paths: `git diff --cached` failed; skipping git-checkpoint \
                 capture for this turn rather than recording an empty staged set"
            );
            return None;
        }
    };
    Some(
        out.split('\0')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect(),
    )
}
/// Outcome of [`soft_restore_git_state`]. Mirrors [`CheckoutSessionOutcome`] (a
/// success flag + optional stash bookkeeping) plus an `aborted_reason` for the
/// dirty-but-unstashable guard.
#[derive(Debug, Default, Clone)]
pub struct GitRestoreOutcome {
    /// `true` when HEAD is at the recorded commit after this call returned.
    pub restored: bool,
    /// `true` when the index was reset to the new HEAD (`git reset -- .`). The
    /// caller should drop checkpoints only after HEAD reset + index reset +
    /// re-stage all succeed, so a partial failure keeps them for retry.
    pub index_reset: bool,
    /// Set when restore was refused without touching git (e.g. unstashable dirty
    /// tree, in-progress merge/rebase).
    pub aborted_reason: Option<String>,
    /// Stash ref holding pre-rewind uncommitted work, when one was created.
    pub stash_ref: Option<String>,
}
/// Soft-restore git state to a recorded [`GitStateRef`]. `cwd` may be a
/// subdirectory; the repo root is resolved so index ops match the recorded
/// root-relative paths.
///
/// SOFT-ONLY and non-destructive to commits:
/// 1. **Stash-or-abort** uncommitted work via [`stash_before_destructive_op`];
///    if it can't be stashed (in-progress merge/rebase, stash failure), abort
///    without touching git.
/// 2. **`git reset --soft <head>`** moves HEAD back, leaving tree + index intact
///    so turn-local commits survive (never `--hard`, never drops a commit).
/// 3. **Unstage to the new HEAD** (`git reset -- .`); the recorded staged *paths*
///    are re-applied later by [`restage_git_paths`] AFTER the FS revert, so blobs
///    reflect the reverted tree.
///
/// Returns a [`GitRestoreOutcome`]; never errors. Phase 2 re-stage is not done here.
pub async fn soft_restore_git_state(
    cwd: &Path,
    git_ref: &GitStateRef,
    session_id: &str,
) -> GitRestoreOutcome {
    let Some(git_root) = resolve_git_root(cwd).await else {
        tracing::warn!(
            path = %cwd.display(),
            session_id,
            "soft_restore_git_state: aborting — could not resolve git repo root"
        );
        return GitRestoreOutcome {
            restored: false,
            index_reset: false,
            aborted_reason: Some("could not resolve git repo root".to_owned()),
            stash_ref: None,
        };
    };
    let stash_ref = match stash_before_destructive_op(&git_root, "rewind-git", session_id).await {
        StashOutcome::Clean => None,
        StashOutcome::Stashed(r) => Some(r),
        StashOutcome::Skipped(reason) => {
            tracing::warn!(
                path = %git_root.display(),
                session_id,
                reason = %reason,
                "soft_restore_git_state: aborting — dirty tree could not be stashed"
            );
            return GitRestoreOutcome {
                restored: false,
                index_reset: false,
                aborted_reason: Some(reason),
                stash_ref: None,
            };
        }
    };
    if let Err(e) = git_cli_mut(&git_root, &["reset", "--soft", &git_ref.head]).await {
        tracing::warn!(
            path = %git_root.display(),
            session_id,
            commit = %git_ref.head,
            error = %e,
            "soft_restore_git_state: reset --soft failed"
        );
        let stash_ref = match stash_ref {
            Some(stash) => match git_cli_mut(&git_root, &["stash", "pop"]).await {
                Ok(_) => None,
                Err(pop_err) => {
                    tracing::warn!(
                        path = %git_root.display(),
                        session_id,
                        stash_ref = %stash,
                        error = %pop_err,
                        "soft_restore_git_state: could not restore stashed changes after a \
                         failed reset; uncommitted work remains in the stash"
                    );
                    Some(stash)
                }
            },
            None => None,
        };
        return GitRestoreOutcome {
            restored: false,
            index_reset: false,
            aborted_reason: Some(format!("reset --soft failed: {e}")),
            stash_ref,
        };
    }
    let index_reset = match git_cli_mut(&git_root, &["reset", "--quiet", "--", "."]).await {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!(
                path = %git_root.display(),
                session_id,
                error = %e,
                "soft_restore_git_state: `git reset -- .` (unstage) failed; staged path \
                 set may not match the recorded checkpoint"
            );
            false
        }
    };
    tracing::info!(
        path = %git_root.display(),
        session_id,
        commit = %git_ref.head,
        staged = git_ref.staged.len(),
        stash_ref = ?stash_ref,
        "soft_restore_git_state: soft-restored HEAD and unstaged; staged paths re-applied post-FS-revert"
    );
    GitRestoreOutcome {
        restored: true,
        index_reset,
        aborted_reason: None,
        stash_ref,
    }
}
/// Re-stage the recorded staged path set — phase 2 of a soft git rewind, run
/// AFTER the FS revert (phase 1 is [`soft_restore_git_state`]) so blobs reflect
/// the reverted tree. Per-path best-effort: a path removed during the turn is
/// skipped, not fatal. Never errors; returns `true` when the full set was
/// re-applied (nothing-to-do counts as success) so the caller can gate truncate on it.
pub async fn restage_git_paths(cwd: &Path, git_ref: &GitStateRef, session_id: &str) -> bool {
    if git_ref.staged.is_empty() {
        return true;
    }
    let Some(git_root) = resolve_git_root(cwd).await else {
        tracing::warn!(
            path = %cwd.display(),
            session_id,
            "restage_git_paths: could not resolve git repo root; staged path set not restored"
        );
        return false;
    };
    let path_strs: Vec<String> = git_ref
        .staged
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let mut batch_args: Vec<&str> = Vec::with_capacity(path_strs.len() + 2);
    batch_args.extend(["add", "--"]);
    batch_args.extend(path_strs.iter().map(String::as_str));
    if git_cli_mut(&git_root, &batch_args).await.is_ok() {
        return true;
    }
    tracing::debug!(
        path = %git_root.display(),
        session_id,
        total = git_ref.staged.len(),
        "restage_git_paths: batched `git add` failed; falling back to per-path best-effort"
    );
    let mut failed_adds = 0usize;
    for path in &git_ref.staged {
        let path_str = path.to_string_lossy();
        if git_cli_mut(&git_root, &["add", "--", path_str.as_ref()])
            .await
            .is_err()
        {
            failed_adds += 1;
        }
    }
    if failed_adds > 0 {
        tracing::debug!(
            path = %git_root.display(),
            session_id,
            failed_adds,
            total = git_ref.staged.len(),
            "restage_git_paths: some recorded staged paths could not be re-added \
             (typically removed during the turn; best-effort)"
        );
    }
    failed_adds == 0
}
/// Run a git CLI command returning `(success, combined stdout+stderr)` instead
/// of collapsing failure into an error. `LC_ALL=C` pins git's message locale so
/// callers can classify output (e.g. non-fast-forward push rejections) by
/// string-match. Errors only on spawn failure.
async fn git_cli_raw(cwd: &Path, args: &[&str]) -> Result<(bool, String)> {
    tracing::debug!(cwd = %cwd.display(), args = ?args, "git_cli_raw");
    let mut cmd = Command::from(echo_agent_tty_utils::git_command());
    cmd.current_dir(cwd).env("LC_ALL", "C");
    let output = cmd.args(args).output().await?;
    let mut combined = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stderr.is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(&stderr);
    }
    Ok((output.status.success(), combined))
}
/// Like [`git_cli_raw`] but returns the process exit code (`-1` if none).
async fn git_cli_status(cwd: &Path, args: &[&str]) -> Result<(i32, String)> {
    tracing::debug!(cwd = %cwd.display(), args = ?args, "git_cli_status");
    let mut cmd = Command::from(echo_agent_tty_utils::git_command());
    cmd.current_dir(cwd).env("LC_ALL", "C");
    let output = cmd.args(args).output().await?;
    let mut combined = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stderr.is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(&stderr);
    }
    Ok((output.status.code().unwrap_or(-1), combined))
}
async fn git_cli_raw_mut(cwd: &Path, args: &[&str]) -> Result<(bool, String)> {
    let result = git_cli_raw(cwd, args).await;
    if result.is_ok() {
        super::git_gate::invalidate(cwd);
    }
    result
}
/// Marker line guarding the default-exclude seed. Environments may pre-seed
/// the same block at provision time under this marker; whichever side seeds
/// first wins and the other becomes a no-op.
const DEFAULT_EXCLUDES_MARKER: &str = "echoagent default excludes";
/// Local-only default excludes (`.git/info/exclude` — never enters the repo's
/// history) so `stage_all` can't sweep in dependency trees, build output, or
/// env files. `git add -f` still overrides.
const DEFAULT_EXCLUDES_BLOCK: &str = "\
# echoagent default excludes (local-only; seeded by the workspace git_commit op)
.echo-agent/
node_modules/
.env
.env.*
dist/
build/
out/
.next/
.nuxt/
.svelte-kit/
.turbo/
.cache/
.parcel-cache/
coverage/
__pycache__/
*.pyc
.venv/
venv/
.pnpm-store/
.DS_Store
npm-debug.log*
yarn-debug.log*
yarn-error.log*
";
/// Seed [`DEFAULT_EXCLUDES_BLOCK`] into the repo's `info/exclude` unless the
/// marker is already present. Resolves the real file via `--git-path` so
/// gitfile and linked-worktree checkouts seed the common dir, not a dead
/// `.git/info` path.
async fn seed_default_excludes(git_root: &Path) -> Result<()> {
    let exclude_rel = git_cli(git_root, &["rev-parse", "--git-path", "info/exclude"]).await?;
    let exclude_path = {
        let p = Path::new(&exclude_rel);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            git_root.join(p)
        }
    };
    let existing = tokio::fs::read_to_string(&exclude_path)
        .await
        .unwrap_or_default();
    if existing.contains(DEFAULT_EXCLUDES_MARKER) {
        return Ok(());
    }
    if let Some(parent) = exclude_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut content = existing;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(DEFAULT_EXCLUDES_BLOCK);
    tokio::fs::write(&exclude_path, content).await?;
    tracing::debug!(path = %exclude_path.display(), "seeded default git excludes");
    Ok(())
}
/// Push HEAD to origin, classifying the failure mode. Never forces. Returns
/// the combined output alongside the [`PushStatus`].
async fn push_classified(git_root: &Path) -> Result<(PushStatus, String)> {
    let (ok, out) = git_cli_raw_mut(git_root, &["push", "-u", "origin", "HEAD"]).await?;
    if ok {
        return Ok((PushStatus::Ok, out));
    }
    let lower = out.to_lowercase();
    let status = if lower.contains("non-fast-forward") || lower.contains("fetch first") {
        PushStatus::Conflict
    } else {
        PushStatus::Failed
    };
    Ok((status, out))
}
/// Refuse unless the workspace is on exactly `expected`. Detached HEAD
/// reports "HEAD" and so never matches.
async fn ensure_on_branch(git_root: &Path, expected: Option<&str>) -> Result<()> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let current = git_cli(git_root, &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
    anyhow::ensure!(
        current == expected,
        "workspace is on '{current}', expected '{expected}'"
    );
    Ok(())
}
pub async fn commit(git_root: &Path, req: &GitCommitReq) -> Result<CommitResult> {
    let start = std::time::Instant::now();
    ensure_on_branch(git_root, req.expected_branch.as_deref()).await?;
    if req.seed_default_excludes {
        seed_default_excludes(git_root).await?;
    }
    if req.stage_all {
        git_cli_mut(git_root, &["add", "-A"]).await?;
    }
    let clean = req.stage_all
        && !req.amend
        && git_cli_raw(git_root, &["diff", "--cached", "--quiet"])
            .await?
            .0;
    let mut combined_output;
    if clean {
        combined_output = "Nothing to commit; working tree clean".to_owned();
    } else {
        let mut args = vec!["commit", "-m", &req.message];
        if req.amend {
            args.push("--amend");
        }
        if req.signoff {
            args.push("--signoff");
        }
        git_cli_mut(git_root, &args).await?;
        combined_output = String::new();
    }
    let mut commit_hash = git_cli(git_root, &["rev-parse", "HEAD"]).await.ok();
    if !clean {
        let short_hash = commit_hash
            .as_ref()
            .and_then(|h| h.get(..7))
            .unwrap_or("unknown");
        combined_output = format!("Committed: {}", short_hash);
    }
    let mut warning = None;
    let mut push_status = PushStatus::NotRequested;
    if req.sync {
        match git_cli_mut(git_root, &["pull", "--rebase"]).await {
            Ok(pull_out) => {
                combined_output.push_str("\n--- Pull ---\n");
                combined_output.push_str(&pull_out);
                commit_hash = git_cli(git_root, &["rev-parse", "HEAD"]).await.ok();
            }
            Err(e) => {
                warning = Some(format!("Couldn't pull the latest changes. {}", e));
                push_status = PushStatus::Skipped;
            }
        }
    }
    if warning.is_none() && (req.push || req.sync) {
        let (status, push_out) = push_classified(git_root).await?;
        push_status = status;
        match status {
            PushStatus::Ok => {
                combined_output.push_str("\n--- Push ---\n");
                combined_output.push_str(&push_out);
            }
            _ => {
                warning = Some(format!("Couldn't push your changes. {}", push_out));
            }
        }
    }
    tracing::debug!(
        amend = req.amend,
        push = req.push,
        sync = req.sync,
        stage_all = req.stage_all,
        clean,
        push_status = ?push_status,
        elapsed = ?start.elapsed(),
        "git.commit"
    );
    Ok(CommitResult {
        data: CommitData {
            commit_hash: commit_hash.clone(),
            output: Some(combined_output),
        },
        warning,
        outcome: Some(CommitOutcome {
            sha: commit_hash,
            clean,
            pushed: push_status == PushStatus::Ok,
            push: push_status,
        }),
    })
}
/// Merge the base branch into the current branch (`workspace.git_sync_base`).
/// Merge, never rebase: conv-branch history must not be rewritten. On
/// conflicts the merge is left in progress for resolution; `abort` rolls it
/// back.
pub async fn sync_base(
    git_root: &Path,
    base_ref: Option<&str>,
    abort: bool,
    expected_branch: Option<&str>,
) -> Result<GitSyncBaseResult> {
    async fn merge_in_progress(git_root: &Path) -> Result<bool> {
        Ok(
            git_cli_raw(git_root, &["rev-parse", "-q", "--verify", "MERGE_HEAD"])
                .await?
                .0,
        )
    }
    if abort {
        let (_ok, out) = git_cli_raw_mut(git_root, &["merge", "--abort"]).await?;
        anyhow::ensure!(
            !merge_in_progress(git_root).await?,
            "merge --abort failed: {out}"
        );
        return Ok(GitSyncBaseResult {
            outcome: GitSyncBaseOutcome::Aborted,
        });
    }
    ensure_on_branch(git_root, expected_branch).await?;
    anyhow::ensure!(
        !merge_in_progress(git_root).await?,
        "a merge is already in progress; resolve it or call again with abort"
    );
    let dirty = git_cli(git_root, &["status", "--porcelain"]).await?;
    anyhow::ensure!(
        dirty.is_empty(),
        "working tree is not clean; commit or discard changes before syncing the base"
    );
    let base = base_ref.unwrap_or("HEAD");
    let (fetched, fetch_out) = git_cli_raw_mut(git_root, &["fetch", "origin", base]).await?;
    anyhow::ensure!(fetched, "fetch of base ref '{base}' failed: {fetch_out}");
    if git_cli_raw(
        git_root,
        &["merge-base", "--is-ancestor", "FETCH_HEAD", "HEAD"],
    )
    .await?
    .0
    {
        return Ok(GitSyncBaseResult {
            outcome: GitSyncBaseOutcome::UpToDate,
        });
    }
    let (merged, merge_out) =
        git_cli_raw_mut(git_root, &["merge", "--no-edit", "FETCH_HEAD"]).await?;
    if merged {
        let sha = git_cli(git_root, &["rev-parse", "HEAD"]).await?;
        return Ok(GitSyncBaseResult {
            outcome: GitSyncBaseOutcome::Merged { sha },
        });
    }
    if merge_in_progress(git_root).await? {
        let files = git_cli(git_root, &["diff", "--name-only", "--diff-filter=U"])
            .await?
            .lines()
            .map(str::to_owned)
            .collect();
        return Ok(GitSyncBaseResult {
            outcome: GitSyncBaseOutcome::Conflicts { files },
        });
    }
    anyhow::bail!("merge of base ref '{base}' failed: {merge_out}")
}
/// Reject a ref/branch value that could be parsed as a git option (leading `-`)
/// or that carries whitespace/control characters or `..`. A boundary guard for
/// client-influenced refs (notably `base_ref`) so they cannot be smuggled in as
/// flags; combined with `--end-of-options` at each call site.
fn ensure_ref_arg_safe(value: &str, what: &str) -> Result<()> {
    anyhow::ensure!(!value.is_empty(), "{what} must not be empty");
    anyhow::ensure!(
        !value.starts_with('-'),
        "{what} '{value}' must not start with '-'"
    );
    anyhow::ensure!(
        !value.chars().any(|c| c.is_whitespace() || c.is_control()),
        "{what} '{value}' contains whitespace or control characters"
    );
    anyhow::ensure!(
        !value.contains(".."),
        "{what} '{value}' must not contain '..'"
    );
    Ok(())
}
/// Seed a committed `.gitignore` (secrets never enter git)
/// when a fresh conversation branch is created and the repo has none. Distinct
/// from [`seed_default_excludes`], which seeds the *local-only* `info/exclude`
/// as a `stage_all` backstop; this file is meant to be committed, so it also
/// protects explicit user commits and BYO-remote exports. Never overwrites an
/// existing `.gitignore`.
async fn seed_default_gitignore(git_root: &Path) -> Result<()> {
    let path = git_root.join(".gitignore");
    if tokio::fs::metadata(&path).await.is_ok() {
        return Ok(());
    }
    tokio::fs::write(
        &path,
        echo_agent_workspace_types::binding::DEFAULT_GITIGNORE,
    )
    .await?;
    git_cli(git_root, &["add", "--end-of-options", ".gitignore"]).await?;
    git_cli(
        git_root,
        &[
            "commit",
            "-m",
            "Seed default .gitignore",
            "--end-of-options",
            ".gitignore",
        ],
    )
    .await?;
    tracing::debug!(path = %path.display(), "seeded and committed default .gitignore");
    Ok(())
}
/// `EnsureBinding` (`workspace.git_ensure_binding`): make the conversation
/// branch exist and be checked out. Resolution order: already-current → local
/// branch → remote `conv/<id>` (resume in a fresh sandbox: check it out, do not
/// re-fork) → fork off `base_ref`. The base is never written directly. On a
/// genuine fresh fork a committed `.gitignore` is seeded if absent.
pub async fn ensure_binding(
    git_root: &Path,
    session_branch: &str,
    base_ref: &str,
) -> Result<GitEnsureBindingResult> {
    ensure_ref_arg_safe(session_branch, "session_branch")?;
    ensure_ref_arg_safe(base_ref, "base_ref")?;
    let already_current = git_cli(git_root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .await
        .map(|b| b == session_branch)
        .unwrap_or(false);
    if already_current {
        let head_sha = git_cli(git_root, &["rev-parse", "HEAD"]).await.ok();
        return Ok(GitEnsureBindingResult {
            branch: session_branch.to_owned(),
            created: false,
            head_sha,
        });
    }
    let local_exists = git_cli_raw(
        git_root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{session_branch}"),
        ],
    )
    .await?
    .0;
    let mut created = false;
    if local_exists {
        checkout_branch(git_root, session_branch, false).await?;
    } else {
        let (ls_code, ls_out) = git_cli_status(
            git_root,
            &[
                "ls-remote",
                "--exit-code",
                "--heads",
                "origin",
                session_branch,
            ],
        )
        .await?;
        let remote_exists = match ls_code {
            0 => true,
            2 => false,
            other => {
                anyhow::bail!(
                    "ls-remote origin '{session_branch}' failed (exit {other}); not treating as missing: {}",
                    scrub_git_output(&ls_out)
                );
            }
        };
        if remote_exists {
            let refspec =
                format!("+refs/heads/{session_branch}:refs/remotes/origin/{session_branch}");
            git_cli(git_root, &["fetch", "origin", "--end-of-options", &refspec]).await?;
            git_cli(
                git_root,
                &[
                    "checkout",
                    "-b",
                    session_branch,
                    "--track",
                    &format!("origin/{session_branch}"),
                ],
            )
            .await?;
        } else {
            git_cli(
                git_root,
                &[
                    "checkout",
                    "-b",
                    session_branch,
                    "--end-of-options",
                    base_ref,
                ],
            )
            .await?;
            created = true;
            seed_default_gitignore(git_root).await?;
        }
    }
    super::git_gate::invalidate(git_root);
    let head_sha = git_cli(git_root, &["rev-parse", "HEAD"]).await.ok();
    Ok(GitEnsureBindingResult {
        branch: session_branch.to_owned(),
        created,
        head_sha,
    })
}
/// `MergeToMain` (`workspace.git_merge_to_main`): merge the conversation branch
/// into its target. Merge, never rebase; never force. On conflicts the merge
/// is aborted and HEAD is restored to `conv_branch` (never leave `MERGE_HEAD`
/// on the integration branch).
///
/// Fetch `origin/<target>` *before* checkout, then fast-forward the local
/// target so the deployed SHA can never be behind the durable remote tip; a
/// target that has *diverged* from origin is an error (never a force).
pub async fn merge_to_main(
    git_root: &Path,
    conv_branch: &str,
    target_branch: &str,
    push: bool,
) -> Result<GitMergeToMainResult> {
    ensure_ref_arg_safe(conv_branch, "session_branch")?;
    ensure_ref_arg_safe(target_branch, "target_branch")?;
    let dirty = git_cli(git_root, &["status", "--porcelain"]).await?;
    anyhow::ensure!(
        dirty.is_empty(),
        "working tree is not clean; commit before merging to '{target_branch}'"
    );
    let (fetched, _) = git_cli_raw(
        git_root,
        &["fetch", "origin", "--end-of-options", target_branch],
    )
    .await?;
    checkout_branch(git_root, target_branch, false).await?;
    if fetched
        && !git_cli_raw(
            git_root,
            &["merge-base", "--is-ancestor", "FETCH_HEAD", "HEAD"],
        )
        .await?
        .0
    {
        let (ff, ff_out) = git_cli_raw(git_root, &["merge", "--ff-only", "FETCH_HEAD"]).await?;
        if !ff {
            let _ = checkout_branch(git_root, conv_branch, false).await;
            anyhow::bail!(
                "local '{target_branch}' has diverged from origin; resolve before publishing: {}",
                scrub_git_output(&ff_out)
            );
        }
    }
    if git_cli_raw(
        git_root,
        &["merge-base", "--is-ancestor", conv_branch, "HEAD"],
    )
    .await?
    .0
    {
        let sha = git_cli(git_root, &["rev-parse", "HEAD"]).await?;
        let push_res = push_merged_target_if_requested(git_root, target_branch, push).await;
        checkout_branch(git_root, conv_branch, false).await?;
        push_res?;
        return Ok(GitMergeToMainResult {
            outcome: GitMergeToMainOutcome::UpToDate { sha },
        });
    }
    let (merged, merge_out) = git_cli_raw(
        git_root,
        &["merge", "--no-edit", "--end-of-options", conv_branch],
    )
    .await?;
    if merged {
        let sha = git_cli(git_root, &["rev-parse", "HEAD"]).await?;
        let push_res = push_merged_target_if_requested(git_root, target_branch, push).await;
        checkout_branch(git_root, conv_branch, false).await?;
        push_res?;
        return Ok(GitMergeToMainResult {
            outcome: GitMergeToMainOutcome::Merged { sha },
        });
    }
    let in_progress = git_cli_raw(git_root, &["rev-parse", "-q", "--verify", "MERGE_HEAD"])
        .await?
        .0;
    if in_progress {
        let files = git_cli(git_root, &["diff", "--name-only", "--diff-filter=U"])
            .await?
            .lines()
            .map(str::to_owned)
            .collect();
        let _ = git_cli_raw(git_root, &["merge", "--abort"]).await;
        checkout_branch(git_root, conv_branch, false).await?;
        return Ok(GitMergeToMainResult {
            outcome: GitMergeToMainOutcome::Conflicts { files },
        });
    }
    let _ = checkout_branch(git_root, conv_branch, false).await;
    anyhow::bail!(
        "merge of '{conv_branch}' into '{target_branch}' failed: {}",
        scrub_git_output(&merge_out)
    )
}
/// Push the merged target to origin when `push` is set, failing loudly (never
/// forcing) so publish never records a deploy against an unpushed target.
async fn push_merged_target_if_requested(
    git_root: &Path,
    target_branch: &str,
    push: bool,
) -> Result<()> {
    if !push {
        return Ok(());
    }
    let (status, out) = push_classified(git_root).await?;
    anyhow::ensure!(
        status == PushStatus::Ok,
        "merge into '{target_branch}' succeeded but push failed ({status:?}): {}",
        scrub_git_output(&out)
    );
    Ok(())
}
/// `Push` (`workspace.git_push`): push a branch (or current `HEAD`) to `origin`,
/// classifying the outcome. Never forces. Output is credential-scrubbed.
pub async fn push_branch(git_root: &Path, branch: Option<&str>) -> Result<GitPushResult> {
    let refspec = branch.unwrap_or("HEAD");
    if let Some(branch) = branch {
        ensure_ref_arg_safe(branch, "branch")?;
    }
    let (ok, out) = git_cli_raw_mut(
        git_root,
        &["push", "-u", "origin", "--end-of-options", refspec],
    )
    .await?;
    let status = if ok {
        PushStatus::Ok
    } else {
        let lower = out.to_lowercase();
        if lower.contains("non-fast-forward") || lower.contains("fetch first") {
            PushStatus::Conflict
        } else {
            PushStatus::Failed
        }
    };
    Ok(GitPushResult {
        status,
        output: scrub_git_output(&out),
    })
}
pub async fn stage_content(git_root: &Path, path: &str, content: &str) -> Result<()> {
    let git_root_buf = git_root.to_path_buf();
    let path = path.to_string();
    let content = content.to_string();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let git_root = git_root_buf;
        let repo = Repository::open(&git_root)?;
        let work_dir = repo
            .workdir()
            .ok_or_else(|| anyhow::anyhow!("cannot stage content in bare repository"))?;
        let relative_path = if Path::new(&path).is_absolute() {
            Path::new(&path)
                .strip_prefix(work_dir)
                .map_err(|_| {
                    anyhow::anyhow!(
                        "path '{}' is not within git repository '{}'",
                        path,
                        work_dir.display()
                    )
                })?
                .to_string_lossy()
                .to_string()
        } else {
            path.clone()
        };
        let blob_oid = repo.blob(content.as_bytes())?;
        let mut index = repo.index()?;
        let existing_mode = index
            .get_path(Path::new(&relative_path), 0)
            .map(|e| e.mode)
            .unwrap_or(git2::FileMode::Blob.into());
        let entry = git2::IndexEntry {
            ctime: git2::IndexTime::new(0, 0),
            mtime: git2::IndexTime::new(0, 0),
            dev: 0,
            ino: 0,
            mode: existing_mode,
            uid: 0,
            gid: 0,
            file_size: content.len() as u32,
            id: blob_oid,
            flags: 0,
            flags_extended: 0,
            path: relative_path.as_bytes().to_vec(),
        };
        index.add(&entry)?;
        index.write()?;
        Ok(())
    })
    .await??;
    super::git_gate::invalidate(git_root);
    Ok(())
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitRepoRequest {
    pub current_working_directory: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitRepoPathResponse {
    pub git_root: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum GitRepoResponse {
    NotGitRepo,
    GitRepo(GitRepoPathResponse),
}
/// Strip `root` from `child`, canonicalizing both sides to handle symlinks
/// (e.g. `/tmp` → `/private/tmp` on macOS). Falls back through partial and
/// raw `strip_prefix` when one side can't be resolved (deleted files, etc.).
///
/// Returns `None` when `child` is not under `root` or they are the same path.
pub fn strip_prefix_canonicalized(child: &Path, root: &Path) -> Option<PathBuf> {
    let child_canonical = dunce::canonicalize(child).ok();
    let root_canonical = dunce::canonicalize(root).ok();
    child_canonical
        .as_deref()
        .zip(root_canonical.as_deref())
        .and_then(|(c, r)| c.strip_prefix(r).ok().map(Path::to_path_buf))
        .or_else(|| {
            root_canonical
                .as_deref()
                .and_then(|r| child.strip_prefix(r).ok().map(Path::to_path_buf))
        })
        .or_else(|| {
            child_canonical
                .as_deref()
                .and_then(|c| c.strip_prefix(root).ok().map(Path::to_path_buf))
        })
        .or_else(|| child.strip_prefix(root).ok().map(Path::to_path_buf))
        .filter(|p| !p.as_os_str().is_empty())
}
/// Compute the subdirectory offset and git root for worktree creation.
///
/// When a user's session cwd is a subdirectory of the git root (e.g.
/// `/repo/packages/foo`), worktrees are always created at the repo root
/// level.  Forked sessions must have their cwd set to the corresponding
/// subdirectory inside the new worktree so that tool calls (search, terminal,
/// file operations) behave the same as in the original session.
///
/// Returns `(subdir_offset, git_root)`:
/// - `subdir_offset`: relative path from the git root to `source_cwd`
///   (empty when `source_cwd == git_root`).
/// - `git_root`: the resolved git root directory.
///
/// If the git root cannot be resolved, both values fall back to `source_cwd`.
pub fn compute_subdir_offset(source_cwd: &str) -> (PathBuf, String) {
    match find_git_root_from_path(Path::new(source_cwd)) {
        Ok(git_root) => {
            let offset =
                strip_prefix_canonicalized(Path::new(source_cwd), &git_root).unwrap_or_default();
            (offset, git_root.to_string_lossy().to_string())
        }
        Err(_) => (PathBuf::new(), source_cwd.to_string()),
    }
}
/// Like [`compute_subdir_offset`] + [`effective_worktree_cwd`] combined, but
/// for callers that already have the source git root (e.g. from an ACP
/// response) instead of discovering it on disk.
///
/// Returns `worktree_root` joined with the subdirectory offset from
/// `source_git_root` to `source_cwd`, or `worktree_root` unchanged when
/// there is no offset or `source_git_root` is `None`.
pub fn effective_worktree_path(
    worktree_root: &Path,
    source_cwd: &Path,
    source_git_root: Option<&Path>,
) -> PathBuf {
    let Some(git_root) = source_git_root else {
        return worktree_root.to_path_buf();
    };
    match strip_prefix_canonicalized(source_cwd, git_root) {
        Some(offset) => worktree_root.join(offset),
        None => worktree_root.to_path_buf(),
    }
}
/// Compute the effective cwd for a forked session by joining a worktree root
/// with a subdirectory offset.
///
/// When `subdir_offset` is empty this returns `worktree_root` unchanged.
/// When it is non-empty the result is `worktree_root/subdir_offset` (using
/// native path separators).
pub fn effective_worktree_cwd(worktree_root: &str, subdir_offset: &Path) -> String {
    if subdir_offset.as_os_str().is_empty() {
        worktree_root.to_string()
    } else {
        PathBuf::from(worktree_root)
            .join(subdir_offset)
            .to_string_lossy()
            .to_string()
    }
}
pub async fn is_git_repo(req: &GitRepoRequest) -> Result<GitRepoResponse> {
    let args = ["rev-parse", "--show-toplevel"];
    let current_working_directory = Path::new(&req.current_working_directory);
    let output = git_cli(current_working_directory, &args).await;
    if let Ok(output) = output {
        Ok(GitRepoResponse::GitRepo(GitRepoPathResponse {
            git_root: output,
        }))
    } else {
        Ok(GitRepoResponse::NotGitRepo)
    }
}
/// Divergence between a session's persisted HEAD and the current working directory HEAD.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeadDivergence {
    pub session_commit: String,
    pub current_commit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_branch: Option<String>,
}
/// Compare a session's persisted HEAD commit against the current HEAD.
///
/// Returns `Some` only when both commits are known and differ.
/// Old sessions without `head_commit` or non-git directories yield `None`.
pub fn detect_head_divergence(
    session_head_commit: Option<&str>,
    session_head_branch: Option<&str>,
    current_head_commit: Option<&str>,
) -> Option<HeadDivergence> {
    let session_commit = session_head_commit?;
    let current_commit = current_head_commit?;
    if session_commit == current_commit {
        return None;
    }
    Some(HeadDivergence {
        session_commit: session_commit.to_owned(),
        current_commit: current_commit.to_owned(),
        session_branch: session_head_branch.map(|s| s.to_owned()),
    })
}
#[cfg(test)]
#[path = "git_head_divergence_tests.rs"]
mod head_divergence_tests;
#[cfg(test)]
#[path = "git_tests.rs"]
mod tests;
/// Format a human-readable summary of what was restored from an archive.
pub fn format_restore_summary(
    sha: Option<&str>,
    staged: bool,
    unstaged: bool,
    untracked: usize,
) -> String {
    match sha.filter(|s| !s.is_empty()).map(short_sha) {
        Some(s) => {
            format!(
                "checked out {s}, staged: {staged}, unstaged: {unstaged}, untracked: {untracked}"
            )
        }
        None => format!("staged: {staged}, unstaged: {unstaged}, untracked: {untracked}"),
    }
}
/// Append a "; saved your dirty changes to stash <ref>" suffix when a
/// stash was created. Uses `;` (not parenthesised) so the suffix
/// composes cleanly with summaries that already end in `)`. No-op when
/// `stash_ref` is `None`.
pub fn append_stash_suffix(summary: &mut String, stash_ref: Option<&str>) {
    use std::fmt::Write as _;
    if let Some(r) = stash_ref {
        let _ = write!(summary, "; saved your dirty changes to stash {r}");
    }
}
/// Append "; stash skipped: <reason>" when a stash was needed but could
/// not be created (in-progress merge, `git stash` failure, etc.).
pub fn append_stash_skipped_suffix(summary: &mut String, reason: Option<&str>) {
    use std::fmt::Write as _;
    if let Some(r) = reason {
        let _ = write!(summary, "; stash skipped: {r}");
    }
}
/// Short (8-char) representation of a SHA, returning a placeholder when
/// the input is empty.
pub fn short_sha(sha: &str) -> &str {
    if sha.is_empty() {
        "unknown"
    } else {
        &sha[..sha.len().min(8)]
    }
}
/// Depth of a `--restore-code` restoration.
///
/// Serialised to `"full"` / `"head_only"` on the wire (camelCase /
/// snake_case agnostic — the variants are themselves snake_case-style).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreDegree {
    /// HEAD checkout + staged/unstaged/untracked applied from GCS archive.
    Full,
    /// HEAD checkout only — no archive applied.
    HeadOnly,
}
/// Why a restore decision is being made — drives the summary string and
/// degree. Shared by the non-worktree (`mvp_agent.rs`) and worktree
/// (`session/worktree.rs`) call sites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreKind {
    /// Local `git checkout` failed — archive must not be applied; caller
    /// should pass this variant directly rather than relying on the
    /// `!outcome.checked_out` short-circuit so the intent is explicit.
    CheckoutFailed,
    /// Session registry disabled — only HEAD was checked out. Also used
    /// when repository-snapshot restore is unavailable in this build.
    RegistryOff,
}
/// Neutral restore-outcome description shared by both restore code-paths.
///
/// Each caller wraps it into its own wire shape (JSON meta for the
/// non-worktree path, struct fields for the worktree path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreDecision {
    /// `true` iff the working tree is at the requested commit after the
    /// restore. `false` when `checkout_session_commit` failed; in that
    /// case `summary` describes the failure and `degree` is `None`.
    pub restored: bool,
    /// Human-readable summary line. Always populated when a restore was
    /// attempted (even on failure) so the UI can render a banner.
    pub summary: Option<String>,
    /// `Some` when `restored == true`; `None` on failure.
    pub degree: Option<RestoreDegree>,
}
/// Build a [`RestoreDecision`] from the checkout outcome and the policy
/// kind. Pure function — no I/O. The single source of truth shared by
/// the agent (`build_code_restore_meta`) and worktree
/// (`build_worktree_restore_outcome`) wire-format adapters.
///
/// Callers should pass [`RestoreKind::CheckoutFailed`] explicitly when
/// `!outcome.checked_out`; the `!outcome.checked_out` fast-path is
/// retained as a defensive fallback so a caller that picks any kind
/// without checking the outcome still cannot apply the archive on top
/// of arbitrary state.
pub fn build_restore_decision(
    head_commit: Option<&str>,
    outcome: &CheckoutSessionOutcome,
    kind: RestoreKind,
) -> RestoreDecision {
    if matches!(kind, RestoreKind::CheckoutFailed) || !outcome.checked_out {
        let mut summary = "restore aborted (checkout failed)".to_owned();
        append_stash_skipped_suffix(&mut summary, outcome.stash_skipped_reason.as_deref());
        return RestoreDecision {
            restored: false,
            summary: Some(summary),
            degree: None,
        };
    }
    let short = short_sha(head_commit.unwrap_or(""));
    let (mut summary, degree) = match kind {
        RestoreKind::CheckoutFailed => unreachable!("handled by short-circuit above"),
        RestoreKind::RegistryOff => (
            format!(
                "checked out {short} (session registry disabled — staged/unstaged/untracked not restored)"
            ),
            RestoreDegree::HeadOnly,
        ),
    };
    append_stash_suffix(&mut summary, outcome.stash_ref.as_deref());
    append_stash_skipped_suffix(&mut summary, outcome.stash_skipped_reason.as_deref());
    RestoreDecision {
        restored: true,
        summary: Some(summary),
        degree: Some(degree),
    }
}
#[cfg(test)]
#[path = "git_restore_code_tests.rs"]
mod restore_code_tests;
