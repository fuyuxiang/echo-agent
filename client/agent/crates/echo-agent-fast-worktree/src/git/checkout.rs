//! Git checkout/reset/clean operations used during worktree creation.

use crate::api::{CopyReport, WorktreeReport};
use anyhow::{Context, Result};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Environment variables set on every git command to suppress interactive prompts.
pub const GIT_AUTH_SUPPRESSION_ENVS: [(&str, &str); 4] = [
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_ASKPASS", ""),
    ("GIT_LFS_SKIP_SMUDGE", "1"),
    ("GIT_SSH_COMMAND", "ssh -o BatchMode=yes"),
];

/// Git command with auth/LFS/SSH prompt suppression and `--no-optional-locks`.
pub(crate) fn git_command() -> Command {
    let mut cmd = Command::new("git");
    echo_agent_tty_utils::detach_std_command(&mut cmd);
    cmd.stdin(Stdio::null());
    cmd.envs(echo_agent_tty_utils::pager_env());
    for &(key, val) in &GIT_AUTH_SUPPRESSION_ENVS {
        cmd.env(key, val);
    }
    cmd.arg("--no-optional-locks");
    cmd
}

/// Run `git reset --hard <target>` (defaults to `HEAD`). Blocking.
pub(crate) fn git_reset_hard_command(worktree_path: &Path, target: Option<&str>) -> Result<()> {
    let tgt = target.unwrap_or("HEAD");
    let output = git_command()
        .current_dir(worktree_path)
        .args(["reset", "--hard", tgt])
        .output()
        .context("failed to run git reset")?;

    if !output.status.success() {
        anyhow::bail!(
            "git reset --hard {} failed: {}",
            tgt,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    tracing::debug!(path = %worktree_path.display(), target = %tgt, "git reset --hard");
    Ok(())
}

/// Run `git clean -fd` (or `-fdx`) to remove untracked files and directories.
///
/// When `include_ignored` is `true`, also removes files covered by `.gitignore`
/// (equivalent to `git clean -fdx`). This is useful when recycling worktrees
/// in a pool, where leftover build artifacts must be purged.
///
/// This is a blocking operation.
pub(crate) fn git_clean_fd(worktree_path: &Path, include_ignored: bool) -> Result<()> {
    let flags = if include_ignored { "-fdx" } else { "-fd" };
    let output = git_command()
        .current_dir(worktree_path)
        .args(["clean", flags])
        .output()
        .context("failed to run git clean")?;

    if !output.status.success() {
        anyhow::bail!(
            "git clean {} failed: {}",
            flags,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

/// Run `git checkout <ref>`. Blocking.
pub(crate) fn checkout_ref(worktree_path: &Path, git_ref: &str) -> Result<()> {
    let output = git_command()
        .current_dir(worktree_path)
        .args(["checkout", git_ref])
        .output()
        .context("failed to run git checkout")?;

    if !output.status.success() {
        anyhow::bail!(
            "git checkout {} failed: {}",
            git_ref,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    tracing::debug!(path = %worktree_path.display(), git_ref = %git_ref, "git checkout");
    Ok(())
}

/// Whether `git diff-index --quiet HEAD` reports differences (tracked-only, far
/// cheaper than `git status`). `cached` adds `--cached`, comparing the index to
/// `HEAD` and skipping the working-tree stat walk. Only ever over-reports (an
/// error is treated as dirty), so a `false` result is safe to skip the reset on.
/// Blocking.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn diff_index_dirty(worktree_path: &Path, cached: bool) -> Result<bool> {
    let mut cmd = git_command();
    cmd.current_dir(worktree_path)
        .args(["diff-index", "--quiet"]);
    if cached {
        cmd.arg("--cached");
    }
    cmd.args(["HEAD", "--"]);
    let status = cmd.status().context("failed to run git diff-index")?;
    match status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        // Unborn HEAD or other error: treat as "changes" so the caller still
        // runs the reset (correctness over the optimization).
        _ => Ok(true),
    }
}

/// Whether the worktree has uncommitted changes to *tracked* files, via
/// `git diff-index --quiet HEAD` (tracked-only, far cheaper than `git status`).
/// It only ever over-reports, so a `false` result is safe to skip the reset on.
/// Blocking.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn worktree_has_tracked_changes(worktree_path: &Path) -> Result<bool> {
    diff_index_dirty(worktree_path, false)
}

/// Whether the index has staged changes vs `HEAD` (`diff-index --cached`).
/// Unlike [`worktree_has_tracked_changes`], `--cached` skips the working-tree
/// stat walk (cheap over FUSE). Over-reports on error. Blocking.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn has_staged_changes(worktree_path: &Path) -> Result<bool> {
    diff_index_dirty(worktree_path, true)
}

/// Whether `HEAD` already resolves to the same commit as `git_ref` (cheap
/// `rev-parse`, no tree walk), letting the caller skip a redundant `git checkout`.
/// Returns `false` if either side can't be resolved, so the caller falls back to
/// a real `checkout`. Blocking.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn worktree_at_ref(worktree_path: &Path, git_ref: &str) -> Result<bool> {
    let rev = |what: &str| -> Option<String> {
        let out = git_command()
            .current_dir(worktree_path)
            .args(["rev-parse", "--verify", "--quiet", what])
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    };
    Ok(
        match (rev("HEAD^{commit}"), rev(&format!("{git_ref}^{{commit}}"))) {
            (Some(head), Some(target)) => head == target,
            _ => false,
        },
    )
}

/// How long one git call on the snapshot path may take before it is treated as
/// hung. Generous, because it bounds a hang rather than policing a slow tree: a
/// `git add -A` over a large working tree is minutes of honest work, and a
/// clean filter that never returns is otherwise forever. It covers every call
/// through [`git_capture_in`], which is the snapshot, the comparison, the
/// transfer, and the `read-tree` that restores a worktree from a snapshot.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(300);

/// Where git is told to look for hooks, so nothing a repository ships runs on
/// a path nobody is watching. A directory that merely does not exist is the
/// wrong answer: git-lfs runs `install` as it filters and created this path,
/// outside any repository, with its four hooks in it. A device file cannot be
/// created and cannot hold a hook.
#[cfg(not(windows))]
pub(crate) const NO_HOOKS: &str = "/dev/null";
#[cfg(windows)]
pub(crate) const NO_HOOKS: &str = "NUL";

/// Run a git command inside `worktree_path` with `envs` applied on top of the
/// base `git_command()` environment, returning trimmed stdout on success.
///
/// Bounded, and killed at the process group, so a filter git started cannot
/// outlive the call and hold the worktree that is about to be deleted.
fn git_capture_in<S: AsRef<OsStr>>(
    worktree_path: &Path,
    args: &[S],
    envs: &[(&str, &OsStr)],
) -> Result<String> {
    let mut cmd = git_command();
    // Same reason the probes do it: the snapshot runs unattended, and the
    // hooks are the worktree's own.
    cmd.current_dir(worktree_path)
        .args(["-c", &format!("core.hooksPath={NO_HOOKS}")])
        .args(args);
    // Before the caller's own, which is what carries the scratch index: the
    // snapshot has to read the configuration the gate's probes read, or the
    // two halves judge different repositories.
    super::probe::forget_inherited_git_environment(&mut cmd);
    for &(key, val) in envs {
        cmd.env(key, val);
    }

    let shown = display_args(args);
    let output = super::probe::run_with_timeout(cmd, Vec::new(), SNAPSHOT_TIMEOUT)
        .with_context(|| format!("failed to run git {shown} in {}", worktree_path.display()))?;

    if !output.status.success() {
        anyhow::bail!(
            "git {shown} failed in {}: {}",
            worktree_path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Render git args for an error/context message (paths shown lossily).
fn display_args<S: AsRef<OsStr>>(args: &[S]) -> String {
    args.iter()
        .map(|arg| arg.as_ref().to_string_lossy().into_owned())
        .collect::<Vec<String>>()
        .join(" ")
}

/// Git config overrides (`-c key=val`, applied before the subcommand) used on
/// every snapshot git call so capture is independent of the user's/enterprise
/// git config and round-trips cleanly. Restore must apply the SAME flags.
///
/// - `core.autocrlf=false`  never mangle line endings
/// - `core.longpaths=true`  tolerate long paths (Windows/enterprise)
/// - `core.symlinks=true`   record symlinks as symlinks
/// - `core.quotepath=false` raw UTF-8 paths (stable output parsing)
/// - `core.fsmonitor=false` never trigger/depend on a configured fsmonitor
/// - `core.checkStat`/`core.trustctime` compare every stat field, so a seeded
///   index cannot call a changed file unchanged
pub(crate) const SNAPSHOT_GIT_CONFIG: &[&str] = &[
    "-c",
    "core.autocrlf=false",
    "-c",
    "core.longpaths=true",
    "-c",
    "core.symlinks=true",
    "-c",
    "core.quotepath=false",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.checkStat=default",
    "-c",
    "core.trustctime=true",
];

/// Like [`git_capture_in`], but prepends [`SNAPSHOT_GIT_CONFIG`] so the call is
/// insulated from the ambient git config. Scoped to the snapshot path; other
/// fast-worktree operations keep the plain `git_command()` behavior.
fn snapshot_git<S: AsRef<OsStr>>(
    worktree_path: &Path,
    args: &[S],
    envs: &[(&str, &OsStr)],
) -> Result<String> {
    let mut full: Vec<OsString> = SNAPSHOT_GIT_CONFIG
        .iter()
        .copied()
        .map(OsString::from)
        .collect();
    full.extend(args.iter().map(|arg| arg.as_ref().to_os_string()));
    git_capture_in(worktree_path, &full, envs)
}

/// Removes a throwaway git index file (and its `.lock` sibling) on drop, so the
/// scratch index never leaks even if a snapshot step fails partway through.
struct ScratchIndexGuard {
    path: PathBuf,
}

impl Drop for ScratchIndexGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_file(format!("{}.lock", self.path.display()));
    }
}

/// Allocate a process-unique path under the temp dir for a scratch index.
///
/// Hand-rolled (pid + nanos + counter) on purpose: `tempfile` is only an
/// optional/dev/bench dependency here, and promoting it to a mandatory prod
/// dependency for this alone is undesirable — do not "simplify" into `tempfile`.
fn scratch_index_path() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "echo-agent-snapshot-index-{}-{nanos}-{seq}",
        std::process::id()
    ))
}

/// Capture a worktree's full working state into the git ref `ref_name`,
/// returning the snapshot commit SHA. The captured state is HEAD plus all
/// working-tree changes: tracked modifications, deletions, and
/// untracked-non-ignored additions. Files tracked in HEAD are always captured
/// (even if they also match a `.gitignore` rule); only *untracked* files
/// matching `.gitignore` are excluded.
///
/// `ref_name` must be a fully-qualified ref (e.g. `refs/echoagent/subagents/<id>`);
/// it is overwritten unconditionally. The worktree must have a valid `HEAD`
/// (subagent worktrees are detached at their base commit), which becomes the
/// snapshot commit's parent (provenance only).
///
/// Staging is done against a throwaway scratch index (`GIT_INDEX_FILE`), so the
/// worktree's real index is never mutated. The snapshot commit's tree is a
/// complete copy of the working state, so it is self-contained: rehydration
/// needs only the tree.
///
/// NOTE on durability: the commit + ref are written into the git store that
/// `worktree_path` resolves to. For a *linked* worktree that is the shared
/// common dir (the main repo), so the ref survives the worktree's deletion. For
/// a *standalone* worktree (its own `.git`), the ref lives inside the worktree
/// and is destroyed when the directory is removed — callers that intend to
/// delete the worktree must first transfer the ref into a durable repo via
/// [`transfer_snapshot_to_repo`].
///
/// Every git call applies [`SNAPSHOT_GIT_CONFIG`] (`core.autocrlf=false`,
/// `core.quotepath=false`, `core.fsmonitor=false`, …) so capture is independent
/// of the user's/enterprise git config (line endings, path quoting, fsmonitor,
/// long paths, symlinks); restore MUST apply the same flags for a clean
/// round-trip. Blocking.
pub fn snapshot_worktree_to_ref(
    worktree_path: &Path,
    ref_name: &str,
    message: &str,
) -> Result<String> {
    snapshot_worktree_to_ref_inner(worktree_path, ref_name, message).with_context(|| {
        format!(
            "failed to snapshot worktree {} into ref {ref_name}",
            worktree_path.display()
        )
    })
}

fn snapshot_worktree_to_ref_inner(
    worktree_path: &Path,
    ref_name: &str,
    message: &str,
) -> Result<String> {
    // Synthetic identity scoped to this call so it is never written to git config.
    const NAME: &str = "EchoAgent Snapshot";
    const EMAIL: &str = "echo-agent-snapshot@example.com";

    let tree = write_worktree_tree(worktree_path, IndexSeed::Head)?;

    // commit-tree takes the tree directly (no index) and needs an author/
    // committer; supply the identity per-call via env vars. HEAD is the parent.
    let ident = [
        ("GIT_AUTHOR_NAME", OsStr::new(NAME)),
        ("GIT_AUTHOR_EMAIL", OsStr::new(EMAIL)),
        ("GIT_COMMITTER_NAME", OsStr::new(NAME)),
        ("GIT_COMMITTER_EMAIL", OsStr::new(EMAIL)),
    ];
    let snap = snapshot_git(
        worktree_path,
        &["commit-tree", &tree, "-p", "HEAD", "-m", message],
        &ident,
    )?;

    snapshot_git(worktree_path, &["update-ref", ref_name, &snap], &[])?;

    tracing::debug!(
        path = %worktree_path.display(),
        ref_name = %ref_name,
        snap = %snap,
        "snapshot worktree to ref"
    );
    Ok(snap)
}

/// Where the scratch index starts before `add -A` layers the working tree on.
enum IndexSeed {
    /// `HEAD`'s tree, so a file that is tracked but also matches a `.gitignore`
    /// rule survives: `add -A` never re-ignores what is already tracked. It
    /// carries no stat cache, so every tracked file is hashed.
    Head,
    /// A copy of the worktree's own index, whose stat cache means `add -A`
    /// hashes only what changed: ten seconds on a monorepo checkout rather
    /// than two minutes.
    WorktreeIndex,
}

/// Stage the whole working state into a throwaway index and write out its tree,
/// leaving the worktree's real index untouched. `add -A` layers modifications,
/// deletions and untracked additions on top of `seed`.
fn write_worktree_tree(worktree_path: &Path, seed: IndexSeed) -> Result<String> {
    let scratch = ScratchIndexGuard {
        path: scratch_index_path(),
    };
    let index_env = [("GIT_INDEX_FILE", scratch.path.as_os_str())];
    match seed {
        IndexSeed::Head => {
            snapshot_git(worktree_path, &["read-tree", "HEAD"], &index_env)?;
        }
        IndexSeed::WorktreeIndex => {
            let git_dir = snapshot_git(worktree_path, &["rev-parse", "--absolute-git-dir"], &[])?;
            std::fs::copy(Path::new(&git_dir).join("index"), &scratch.path)?;
        }
    }
    snapshot_git(worktree_path, &["add", "-A"], &index_env)?;
    snapshot_git(worktree_path, &["write-tree"], &index_env)
}

/// Whether the working state is still the one `snapshot` holds. A caller that
/// snapshots, decides, and then deletes leaves a window in between; anything
/// written during it is in no ref, and this is what sees it.
///
/// The two seeds can disagree only about a file that is ignored and staged, or
/// ignored and unstaged, which reads here as a change and keeps the worktree.
/// Erring that way is the point.
pub(crate) fn worktree_matches_snapshot(worktree_path: &Path, snapshot: &str) -> Result<bool> {
    let snapshot_tree = snapshot_git(
        worktree_path,
        &["rev-parse", "--verify", &format!("{snapshot}^{{tree}}")],
        &[],
    )
    .with_context(|| format!("failed to resolve snapshot {snapshot} to a tree"))?;
    let current =
        write_worktree_tree(worktree_path, IndexSeed::WorktreeIndex).with_context(|| {
            format!(
                "failed to re-read the working state of {}",
                worktree_path.display()
            )
        })?;
    Ok(current == snapshot_tree)
}

/// Make a snapshot `ref_name` (created by [`snapshot_worktree_to_ref`] in
/// `worktree_path`'s git) durable in `source_repo`, then verify it resolves
/// there. This is required for STANDALONE subagent worktrees, whose `.git` (and
/// thus the snapshot commit + ref) is destroyed when the worktree directory is
/// deleted; copying the commit + objects into the surviving `source_repo` lets
/// resume rehydrate from it. For linked worktrees the objects/ref already live
/// in the shared common dir, so this is effectively a no-op that just confirms
/// the ref is present.
///
/// Fetches the snapshot ref (with its reachable objects) from `worktree_path`
/// into `source_repo`, then verifies it resolves to a commit there — returning
/// an error (so the caller does NOT delete the worktree) if it does not. Every
/// git call applies [`SNAPSHOT_GIT_CONFIG`] for parity with capture. Blocking.
pub fn transfer_snapshot_to_repo(
    worktree_path: &Path,
    source_repo: &Path,
    ref_name: &str,
) -> Result<()> {
    transfer_snapshot_to_repo_inner(worktree_path, source_repo, ref_name).with_context(|| {
        format!(
            "failed to transfer snapshot ref {ref_name} from {} into {}",
            worktree_path.display(),
            source_repo.display()
        )
    })
}

fn transfer_snapshot_to_repo_inner(
    worktree_path: &Path,
    source_repo: &Path,
    ref_name: &str,
) -> Result<()> {
    // Copy the snapshot commit + reachable tree/blobs and the ref from the
    // worktree's git into the source repo. Force (`+`) matches the unconditional
    // overwrite semantics of `snapshot_worktree_to_ref`; `--no-tags` avoids
    // pulling unrelated tag refs.
    let refspec = format!("+{ref_name}:{ref_name}");
    snapshot_git(
        source_repo,
        &[
            OsStr::new("fetch"),
            OsStr::new("--no-tags"),
            worktree_path.as_os_str(),
            OsStr::new(&refspec),
        ],
        &[],
    )?;

    // Belt-and-suspenders: only succeed once the durable ref resolves in source,
    // so a caller never deletes the worktree without a recoverable snapshot.
    let commitish = format!("{ref_name}^{{commit}}");
    snapshot_git(source_repo, &["rev-parse", "--verify", &commitish], &[])?;

    tracing::debug!(
        worktree = %worktree_path.display(),
        source_repo = %source_repo.display(),
        ref_name = %ref_name,
        "transferred snapshot ref into source repo"
    );
    Ok(())
}

/// Recreate a worktree at `dest` from a `snapshot_commit` produced by
/// [`snapshot_worktree_to_ref`]. `snapshot_commit` may be a ref name or SHA;
/// `source_repo` is any path inside the repo that owns the snapshot's objects.
///
/// The snapshot was created with `commit-tree -p HEAD`, so the snapshot's first
/// parent is the original base. When that base is still reachable, the worktree
/// is added detached at the base and the snapshot tree is read into the working
/// tree: HEAD sits at the real base so restored changes show as modifications
/// and the user's future commits build on (and sign against) the real base.
/// When the base is unreachable (e.g. a parent-repo `git reset --hard` pruned
/// it), the worktree is added at the snapshot commit instead — the content is
/// still exact, only HEAD differs.
///
/// Every git call applies [`SNAPSHOT_GIT_CONFIG`] so restore round-trips
/// symmetrically with capture (line endings, path quoting, fsmonitor, …). The
/// rehydrated worktree is re-registered in the metadata DB as
/// [`WorktreeKind::Subagent`](crate::db::WorktreeKind::Subagent), tagged with
/// `session_id` (mirroring `WorktreeBuilder::create()`). Blocking.
pub fn rehydrate_worktree_from_ref(
    dest: &Path,
    source_repo: &Path,
    snapshot_commit: &str,
    session_id: Option<&str>,
) -> Result<WorktreeReport> {
    rehydrate_worktree_from_ref_inner(dest, source_repo, snapshot_commit, session_id).with_context(
        || {
            format!(
                "failed to rehydrate worktree {} from snapshot {snapshot_commit}",
                dest.display()
            )
        },
    )
}

fn rehydrate_worktree_from_ref_inner(
    dest: &Path,
    source_repo: &Path,
    snapshot_commit: &str,
    session_id: Option<&str>,
) -> Result<WorktreeReport> {
    // The snapshot's first parent is the original base. Resolve it, then confirm
    // the object is actually present — a parent-repo `git reset --hard` + gc can
    // leave the parent pointer dangling, which `rev-parse` alone would not catch.
    let parent = format!("{snapshot_commit}^");
    let base = snapshot_git(
        source_repo,
        &["rev-parse", "--verify", "--quiet", &parent],
        &[],
    )
    .ok()
    .filter(|sha| !sha.is_empty())
    .filter(|sha| snapshot_git(source_repo, &["cat-file", "-e", sha], &[]).is_ok());

    // Prefer adding at the base so HEAD sits at the real base; otherwise fall
    // back to the snapshot commit (content stays exact, only HEAD differs).
    let add_target = match &base {
        Some(sha) => sha.as_str(),
        None => {
            tracing::warn!(
                snapshot = %snapshot_commit,
                dest = %dest.display(),
                "snapshot base unreachable; rehydrating detached at the snapshot commit"
            );
            snapshot_commit
        }
    };
    // A prior rehydrate may have failed after `worktree add` and left a partial dir; remove it so this attempt starts clean (worktree add fails on an existing path).
    if dest.exists() {
        let _ = crate::remove_worktree(dest);
        if dest.exists() {
            let _ = std::fs::remove_dir_all(dest);
        }
    }
    // `git worktree add` refuses a path another registration still claims,
    // so scrub `dest`'s stale registration (previously-disposed worktree, or
    // the raw `remove_dir_all` fallback above) before adding.
    crate::git::remove_stale_worktree_registration(source_repo, dest);
    snapshot_git(
        source_repo,
        &[
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("--detach"),
            OsStr::new("--no-checkout"),
            dest.as_os_str(),
            OsStr::new(add_target),
        ],
        &[],
    )?;

    // Past this point the dest dir + its registration exist. Populate the index
    // and working tree from the snapshot tree without moving HEAD, so restored
    // content appears as changes against the base. If any step fails, tear down
    // the partial worktree so a later resume can't reuse a corrupt directory.
    let populate = || -> Result<String> {
        snapshot_git(dest, &["read-tree", "--reset", "-u", snapshot_commit], &[])?;
        snapshot_git(dest, &["rev-parse", "HEAD"], &[])
    };
    let commit = match populate() {
        Ok(commit) => commit,
        Err(e) => {
            // Best-effort cleanup; preserve the original error.
            let _ = crate::remove_worktree(dest);
            crate::git::remove_stale_worktree_registration(source_repo, dest);
            return Err(e);
        }
    };

    // Mirror `WorktreeBuilder::create()` registration so the rehydrated worktree
    // is tracked again after its directory was disposed of. `session_id` is only
    // consumed here, so silence it when the metadata DB is compiled out.
    #[cfg(not(feature = "metadata"))]
    let _ = session_id;
    #[cfg(feature = "metadata")]
    crate::api::register_worktree(
        dest,
        source_repo,
        crate::db::WorktreeKind::Subagent,
        "linked",
        "HEAD",
        &commit,
        session_id.map(str::to_owned),
        None,
        None,
    );

    tracing::debug!(
        dest = %dest.display(),
        snapshot = %snapshot_commit,
        base = ?base,
        commit = %commit,
        "rehydrated worktree from snapshot"
    );

    Ok(WorktreeReport {
        worktree_path: dest.to_path_buf(),
        commit,
        unignored_copy: CopyReport::default(),
        ignored_copy: None,
        resolved_strategy: crate::worktree::STRATEGY_GIT,
        strategy_metadata: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_test_utils::git::{git_commit_all, init_git_repo};
    use tempfile::TempDir;

    #[test]
    fn test_git_reset_hard_command() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        init_git_repo(temp.path());

        // Create and commit a file
        std::fs::write(temp.path().join("file.txt"), "original").unwrap();
        git_commit_all(temp.path(), "initial");

        // Modify the file
        std::fs::write(temp.path().join("file.txt"), "modified").unwrap();
        assert_eq!(
            std::fs::read_to_string(temp.path().join("file.txt")).unwrap(),
            "modified"
        );

        // Reset
        git_reset_hard_command(temp.path(), None).unwrap();

        // Should be back to original
        assert_eq!(
            std::fs::read_to_string(temp.path().join("file.txt")).unwrap(),
            "original"
        );
    }

    #[test]
    fn test_worktree_has_tracked_changes() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        init_git_repo(temp.path());
        std::fs::write(temp.path().join("file.txt"), "original").unwrap();
        git_commit_all(temp.path(), "initial");

        // Clean tree → no tracked changes.
        assert!(!worktree_has_tracked_changes(temp.path()).unwrap());

        // Modify a tracked file → tracked changes.
        std::fs::write(temp.path().join("file.txt"), "modified").unwrap();
        assert!(worktree_has_tracked_changes(temp.path()).unwrap());

        // Reset back → clean again.
        git_reset_hard_command(temp.path(), None).unwrap();
        assert!(!worktree_has_tracked_changes(temp.path()).unwrap());

        // Untracked files are NOT tracked changes (clean -fd handles those).
        std::fs::write(temp.path().join("new.txt"), "untracked").unwrap();
        assert!(!worktree_has_tracked_changes(temp.path()).unwrap());
    }

    #[test]
    fn test_has_staged_changes() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        init_git_repo(temp.path());
        std::fs::write(temp.path().join("file.txt"), "original").unwrap();
        git_commit_all(temp.path(), "initial");

        // Clean index → no staged changes.
        assert!(!has_staged_changes(temp.path()).unwrap());

        // A working-tree-only modification is NOT staged.
        std::fs::write(temp.path().join("file.txt"), "modified").unwrap();
        assert!(!has_staged_changes(temp.path()).unwrap());

        // `git add` stages it → staged changes.
        git_command()
            .current_dir(temp.path())
            .args(["add", "file.txt"])
            .status()
            .unwrap();
        assert!(has_staged_changes(temp.path()).unwrap());

        // A staged deletion (no other path copied up) is also detected — the
        // case the pristine-upper fast path must not skip.
        git_reset_hard_command(temp.path(), None).unwrap();
        assert!(!has_staged_changes(temp.path()).unwrap());
        git_command()
            .current_dir(temp.path())
            .args(["rm", "--cached", "file.txt"])
            .status()
            .unwrap();
        assert!(has_staged_changes(temp.path()).unwrap());
    }

    #[test]
    fn test_worktree_at_ref() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        init_git_repo(temp.path());
        std::fs::write(temp.path().join("file.txt"), "v1").unwrap();
        git_commit_all(temp.path(), "first");

        // HEAD resolves to itself and to the current branch name.
        assert!(worktree_at_ref(temp.path(), "HEAD").unwrap());
        let branch =
            git_capture_in(temp.path(), &["rev-parse", "--abbrev-ref", "HEAD"], &[]).unwrap();
        assert!(worktree_at_ref(temp.path(), &branch).unwrap());

        // A second commit: the branch tip moves, so the old commit is no longer HEAD.
        let first = git_capture_in(temp.path(), &["rev-parse", "HEAD"], &[]).unwrap();
        std::fs::write(temp.path().join("file.txt"), "v2").unwrap();
        git_commit_all(temp.path(), "second");
        assert!(!worktree_at_ref(temp.path(), &first).unwrap());
        assert!(worktree_at_ref(temp.path(), &branch).unwrap());

        // A ref that doesn't resolve → false (caller falls back to a real checkout).
        assert!(!worktree_at_ref(temp.path(), "does-not-exist").unwrap());
    }

    /// Create a source repo (one committed file) plus a worktree of it.
    fn repo_with_worktree(temp: &TempDir) -> (PathBuf, PathBuf) {
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        init_git_repo(&repo_path);
        std::fs::write(repo_path.join("tracked.txt"), "original").unwrap();
        git_commit_all(&repo_path, "initial");

        let wt = temp.path().join("wt");
        crate::WorktreeBuilder::new(&repo_path, &wt)
            .create()
            .unwrap();
        (repo_path, wt)
    }

    #[test]
    fn test_snapshot_worktree_captures_tracked_and_untracked() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let (_repo, wt) = repo_with_worktree(&temp);

        // Edit a tracked file AND add an untracked file.
        std::fs::write(wt.join("tracked.txt"), "edited").unwrap();
        std::fs::write(wt.join("untracked.txt"), "brand new").unwrap();

        let ref_name = "refs/echoagent/snapshots/test";
        let snap = snapshot_worktree_to_ref(&wt, ref_name, "snapshot test").unwrap();
        assert!(!snap.is_empty());

        // (a) The ref resolves to the snapshot commit.
        let resolved = git_capture_in(&wt, &["rev-parse", ref_name], &[]).unwrap();
        assert_eq!(resolved, snap);

        // (b) The snapshot tree contains both files.
        let listing =
            git_capture_in(&wt, &["ls-tree", "-r", "--name-only", ref_name], &[]).unwrap();
        assert!(
            listing.contains("tracked.txt"),
            "missing tracked file: {listing}"
        );
        assert!(
            listing.contains("untracked.txt"),
            "missing untracked file: {listing}"
        );

        // (c) Both files' snapshot content matches the working tree.
        let tracked = git_capture_in(
            &wt,
            &["cat-file", "-p", &format!("{snap}:tracked.txt")],
            &[],
        )
        .unwrap();
        assert_eq!(tracked, "edited");
        let untracked = git_capture_in(
            &wt,
            &["cat-file", "-p", &format!("{snap}:untracked.txt")],
            &[],
        )
        .unwrap();
        assert_eq!(untracked, "brand new");
    }

    #[test]
    fn test_snapshot_excludes_ignored_files() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let (_repo, wt) = repo_with_worktree(&temp);

        // An ignore rule plus a matching file that must NOT be captured.
        std::fs::write(wt.join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(wt.join("ignored.txt"), "secret").unwrap();
        std::fs::write(wt.join("kept.txt"), "keep me").unwrap();

        let ref_name = "refs/echoagent/snapshots/ignored";
        snapshot_worktree_to_ref(&wt, ref_name, "ignore test").unwrap();

        let listing =
            git_capture_in(&wt, &["ls-tree", "-r", "--name-only", ref_name], &[]).unwrap();
        assert!(
            !listing.contains("ignored.txt"),
            "ignored file leaked into snapshot: {listing}"
        );
        assert!(
            listing.contains("kept.txt"),
            "non-ignored untracked file missing: {listing}"
        );
    }

    #[test]
    fn test_snapshot_captures_tracked_then_ignored_file() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();

        // Commit the file FIRST so it is tracked in HEAD...
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        init_git_repo(&repo_path);
        std::fs::write(repo_path.join("config.env"), "v1").unwrap();
        git_commit_all(&repo_path, "add config");
        // ...THEN add a .gitignore rule that matches it (it stays tracked).
        std::fs::write(repo_path.join(".gitignore"), "config.env\n").unwrap();
        git_commit_all(&repo_path, "ignore config");

        let wt = temp.path().join("wt");
        crate::WorktreeBuilder::new(&repo_path, &wt)
            .create()
            .unwrap();

        // Edit the tracked-but-ignored file in the worktree.
        std::fs::write(wt.join("config.env"), "v2").unwrap();

        let ref_name = "refs/echoagent/snapshots/tracked-ignored";
        let snap = snapshot_worktree_to_ref(&wt, ref_name, "tracked-then-ignored").unwrap();

        // A file tracked in HEAD must survive even though it matches .gitignore,
        // with the working-tree edit captured (regression: empty index dropped it).
        let listing =
            git_capture_in(&wt, &["ls-tree", "-r", "--name-only", ref_name], &[]).unwrap();
        assert!(
            listing.contains("config.env"),
            "tracked-but-ignored file dropped from snapshot: {listing}"
        );
        let content =
            git_capture_in(&wt, &["cat-file", "-p", &format!("{snap}:config.env")], &[]).unwrap();
        assert_eq!(
            content, "v2",
            "edited content of tracked-but-ignored file must be captured"
        );
    }

    #[test]
    fn test_snapshot_clean_tree_equals_head_tree() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        // A clean, committed repo: nothing pending in the working tree.
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        init_git_repo(&repo_path);
        std::fs::write(repo_path.join("tracked.txt"), "original").unwrap();
        git_commit_all(&repo_path, "initial");

        let ref_name = "refs/echoagent/snapshots/clean";
        let snap = snapshot_worktree_to_ref(&repo_path, ref_name, "clean snapshot").unwrap();

        let snap_tree =
            git_capture_in(&repo_path, &["rev-parse", &format!("{snap}^{{tree}}")], &[]).unwrap();
        let head_tree = git_capture_in(&repo_path, &["rev-parse", "HEAD^{tree}"], &[]).unwrap();
        assert_eq!(
            snap_tree, head_tree,
            "clean snapshot tree must equal HEAD tree"
        );
    }

    #[test]
    fn test_snapshot_overwrites_ref_on_second_call() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let (_repo, wt) = repo_with_worktree(&temp);
        let ref_name = "refs/echoagent/snapshots/overwrite";

        std::fs::write(wt.join("tracked.txt"), "first").unwrap();
        let snap1 = snapshot_worktree_to_ref(&wt, ref_name, "first").unwrap();

        std::fs::write(wt.join("tracked.txt"), "second").unwrap();
        let snap2 = snapshot_worktree_to_ref(&wt, ref_name, "second").unwrap();

        assert_ne!(snap1, snap2, "second snapshot should be a new commit");
        let resolved = git_capture_in(&wt, &["rev-parse", ref_name], &[]).unwrap();
        assert_eq!(resolved, snap2, "ref should point at the latest snapshot");
        let content = git_capture_in(
            &wt,
            &["cat-file", "-p", &format!("{snap2}:tracked.txt")],
            &[],
        )
        .unwrap();
        assert_eq!(content, "second");
    }

    #[test]
    fn test_snapshot_survives_worktree_removal() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let (repo_path, wt) = repo_with_worktree(&temp);

        std::fs::write(wt.join("tracked.txt"), "edited").unwrap();
        std::fs::write(wt.join("untracked.txt"), "brand new").unwrap();
        let ref_name = "refs/echoagent/snapshots/survives";
        let snap = snapshot_worktree_to_ref(&wt, ref_name, "pre-removal").unwrap();

        // Delete the worktree dir; the snapshot lives in the shared object/ref store.
        crate::remove_worktree(&wt).unwrap();
        assert!(!wt.exists());

        // Ref + tree still resolve from the main repo, with captured content intact.
        let resolved = git_capture_in(&repo_path, &["rev-parse", ref_name], &[]).unwrap();
        assert_eq!(resolved, snap);
        let listing =
            git_capture_in(&repo_path, &["ls-tree", "-r", "--name-only", ref_name], &[]).unwrap();
        assert!(
            listing.contains("tracked.txt"),
            "tracked file lost after removal: {listing}"
        );
        assert!(
            listing.contains("untracked.txt"),
            "untracked file lost after removal: {listing}"
        );
        let content = git_capture_in(
            &repo_path,
            &["cat-file", "-p", &format!("{snap}:untracked.txt")],
            &[],
        )
        .unwrap();
        assert_eq!(content, "brand new");
    }

    #[test]
    fn test_snapshot_does_not_mutate_real_index() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let (_repo, wt) = repo_with_worktree(&temp);

        std::fs::write(wt.join("tracked.txt"), "edited").unwrap();
        std::fs::write(wt.join("untracked.txt"), "brand new").unwrap();

        // The real index/working tree state must be byte-identical before/after.
        let before = git_capture_in(&wt, &["status", "--porcelain"], &[]).unwrap();
        assert!(
            !before.is_empty(),
            "precondition: there are pending changes"
        );
        snapshot_worktree_to_ref(&wt, "refs/echoagent/snapshots/noindex", "no mutate").unwrap();
        let after = git_capture_in(&wt, &["status", "--porcelain"], &[]).unwrap();

        assert_eq!(
            before, after,
            "snapshot must not stage changes in the real index"
        );
    }

    #[test]
    fn test_snapshot_preserves_crlf_under_autocrlf_config() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let (_repo, wt) = repo_with_worktree(&temp);

        // Simulate an enterprise/user setting that would rewrite line endings.
        git_capture_in(&wt, &["config", "core.autocrlf", "true"], &[]).unwrap();

        // A tracked file whose working-tree content has CRLF line endings.
        std::fs::write(wt.join("tracked.txt"), "line1\r\nline2\r\n").unwrap();

        let ref_name = "refs/echoagent/snapshots/crlf";
        let snap = snapshot_worktree_to_ref(&wt, ref_name, "crlf").unwrap();

        // The snapshot blob must keep the raw CRLF bytes: our `-c
        // core.autocrlf=false` overrode the local `core.autocrlf=true`, which
        // would otherwise have stripped the `\r`. Read raw bytes (no trimming).
        let out = git_command()
            .current_dir(&wt)
            .args(["cat-file", "-p", &format!("{snap}:tracked.txt")])
            .output()
            .unwrap();
        assert!(out.status.success());
        assert!(
            out.stdout.windows(2).any(|w| w == b"\r\n"),
            "CRLF was stripped from the snapshot blob: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    #[test]
    fn test_snapshot_captures_non_ascii_spaced_path() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let (_repo, wt) = repo_with_worktree(&temp);

        // A path with a space and a non-ASCII char (no Unicode decomposition,
        // so it is stable across macOS/Linux filesystems).
        let name = "λ space.txt";
        std::fs::write(wt.join(name), "x").unwrap();

        let ref_name = "refs/echoagent/snapshots/unicode";
        snapshot_worktree_to_ref(&wt, ref_name, "unicode path").unwrap();

        // Read the tree with the same hardening (`core.quotepath=false`) so the
        // path comes back as raw UTF-8 rather than octal-escaped/quoted.
        let listing = snapshot_git(&wt, &["ls-tree", "-r", "--name-only", ref_name], &[]).unwrap();
        assert!(
            listing.lines().any(|l| l == name),
            "non-ASCII spaced path missing from snapshot: {listing}"
        );
    }

    #[test]
    fn test_rehydrate_round_trip_restores_working_state() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();

        // Base repo with several committed files so the round trip exercises
        // unchanged, edited, and deleted tracked paths.
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        init_git_repo(&repo_path);
        std::fs::write(repo_path.join("tracked.txt"), "original").unwrap();
        std::fs::write(repo_path.join("unchanged.txt"), "stable").unwrap();
        std::fs::write(repo_path.join("deleted.txt"), "doomed").unwrap();
        git_commit_all(&repo_path, "initial");

        // A shared-repo setting that would rewrite line endings on a naive
        // checkout; the rehydrated worktree inherits it, so restore must apply
        // the same `core.autocrlf=false` hardening that capture used.
        git_capture_in(&repo_path, &["config", "core.autocrlf", "true"], &[]).unwrap();

        let wt = temp.path().join("wt");
        crate::WorktreeBuilder::new(&repo_path, &wt)
            .create()
            .unwrap();

        // Edit a tracked file, delete a tracked file, leave one untouched, and
        // add untracked CRLF + LF files.
        std::fs::write(wt.join("tracked.txt"), "edited").unwrap();
        std::fs::remove_file(wt.join("deleted.txt")).unwrap();
        std::fs::write(wt.join("untracked.txt"), "brand new").unwrap();
        std::fs::write(wt.join("crlf.txt"), "line1\r\nline2\r\n").unwrap();
        std::fs::write(wt.join("lf.txt"), "a\nb\n").unwrap();

        let snap =
            snapshot_worktree_to_ref(&wt, "refs/echoagent/snapshots/roundtrip", "round trip")
                .unwrap();
        let base = git_capture_in(&repo_path, &["rev-parse", &format!("{snap}^")], &[]).unwrap();

        // Dispose of the worktree dir; only the ref/objects survive.
        crate::remove_worktree(&wt).unwrap();
        assert!(!wt.exists());

        let report = rehydrate_worktree_from_ref(&wt, &repo_path, &snap, None).unwrap();
        assert_eq!(report.worktree_path, wt);
        assert!(wt.exists());

        // Tracked edit present.
        assert_eq!(
            std::fs::read_to_string(wt.join("tracked.txt")).unwrap(),
            "edited"
        );
        // Unchanged-from-base file is repopulated with its original content,
        // proving `read-tree --reset -u` refills the post `--no-checkout` empty
        // working tree (not just the dirty paths).
        assert_eq!(
            std::fs::read_to_string(wt.join("unchanged.txt")).unwrap(),
            "stable"
        );
        // A tracked file deleted in the worktree stays absent after restore.
        assert!(
            !wt.join("deleted.txt").exists(),
            "deleted tracked file must stay absent"
        );
        // Untracked file present.
        assert_eq!(
            std::fs::read_to_string(wt.join("untracked.txt")).unwrap(),
            "brand new"
        );
        // CRLF preserved byte-for-byte.
        assert_eq!(
            std::fs::read(wt.join("crlf.txt")).unwrap(),
            b"line1\r\nline2\r\n"
        );
        // LF preserved byte-for-byte: the restore-side `core.autocrlf=false`
        // overrode the inherited `autocrlf=true`, which would otherwise smudge
        // LF→CRLF on checkout. This fails if restore drops SNAPSHOT_GIT_CONFIG.
        assert_eq!(std::fs::read(wt.join("lf.txt")).unwrap(), b"a\nb\n");

        // HEAD rests at the real base so future commits build on (and sign
        // against) it; restored content shows as changes, not a new base.
        let head = git_capture_in(&wt, &["rev-parse", "HEAD"], &[]).unwrap();
        assert_eq!(head, base, "HEAD should rest at the original base");
        assert_eq!(report.commit, base);
    }

    #[test]
    fn test_rehydrate_base_missing_falls_back_to_snapshot() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let (repo_path, wt) = repo_with_worktree(&temp);

        std::fs::write(wt.join("tracked.txt"), "edited").unwrap();
        std::fs::write(wt.join("untracked.txt"), "brand new").unwrap();

        // Build a PARENTLESS commit holding the same working state, so its `^`
        // never resolves — exercising the base-unreachable fallback without
        // depending on gc to prune a real base.
        let snap =
            snapshot_worktree_to_ref(&wt, "refs/echoagent/snapshots/orphan-src", "src").unwrap();
        let tree = git_capture_in(&wt, &["rev-parse", &format!("{snap}^{{tree}}")], &[]).unwrap();
        let ident = [
            ("GIT_AUTHOR_NAME", OsStr::new("T")),
            ("GIT_AUTHOR_EMAIL", OsStr::new("t@example.com")),
            ("GIT_COMMITTER_NAME", OsStr::new("T")),
            ("GIT_COMMITTER_EMAIL", OsStr::new("t@example.com")),
        ];
        let orphan = git_capture_in(&wt, &["commit-tree", &tree, "-m", "orphan"], &ident).unwrap();
        assert!(
            git_capture_in(
                &wt,
                &["rev-parse", "--verify", "--quiet", &format!("{orphan}^")],
                &[]
            )
            .is_err(),
            "precondition: the orphan snapshot has no parent"
        );

        crate::remove_worktree(&wt).unwrap();
        let report = rehydrate_worktree_from_ref(&wt, &repo_path, &orphan, None).unwrap();

        // Content is still fully restored despite the unreachable base.
        assert_eq!(
            std::fs::read_to_string(wt.join("tracked.txt")).unwrap(),
            "edited"
        );
        assert_eq!(
            std::fs::read_to_string(wt.join("untracked.txt")).unwrap(),
            "brand new"
        );

        // The fallback leaves HEAD at the snapshot commit itself.
        let head = git_capture_in(&wt, &["rev-parse", "HEAD"], &[]).unwrap();
        assert_eq!(head, orphan, "fallback should detach HEAD at the snapshot");
        assert_eq!(report.commit, orphan);
    }

    #[test]
    fn test_rehydrate_self_heals_over_leftover_dest_dir() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let (repo_path, wt) = repo_with_worktree(&temp);

        std::fs::write(wt.join("tracked.txt"), "edited").unwrap();
        std::fs::write(wt.join("untracked.txt"), "brand new").unwrap();
        let snap = snapshot_worktree_to_ref(&wt, "refs/echoagent/snapshots/idem", "idem").unwrap();
        crate::remove_worktree(&wt).unwrap();

        // First rehydrate recreates the dest dir.
        rehydrate_worktree_from_ref(&wt, &repo_path, &snap, None).unwrap();
        assert!(wt.exists());

        // Re-rehydrating at the SAME dest (a leftover dir, as a failed prior
        // attempt would leave) must self-heal and succeed rather than erroring.
        let report = rehydrate_worktree_from_ref(&wt, &repo_path, &snap, None).unwrap();
        assert_eq!(report.worktree_path, wt);
        assert_eq!(
            std::fs::read_to_string(wt.join("tracked.txt")).unwrap(),
            "edited"
        );
        assert_eq!(
            std::fs::read_to_string(wt.join("untracked.txt")).unwrap(),
            "brand new"
        );
    }

    /// Rehydrate must clear its own stale registration (so re-adding the
    /// same path succeeds) while leaving every other entry alone — its
    /// cleanup once pruned repo-wide and destroyed user registrations whose
    /// paths were not visible from the container mount namespace.
    #[test]
    fn test_rehydrate_clears_only_its_own_stale_registration() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let (repo_path, wt) = repo_with_worktree(&temp);

        std::fs::write(wt.join("tracked.txt"), "edited").unwrap();
        let snap =
            snapshot_worktree_to_ref(&wt, "refs/echoagent/snapshots/stalereg", "stale").unwrap();
        crate::remove_worktree(&wt).unwrap();

        rehydrate_worktree_from_ref(&wt, &repo_path, &snap, None).unwrap();
        let registration = repo_path
            .join(".git")
            .join("worktrees")
            .join(wt.file_name().unwrap());
        assert!(registration.is_dir(), "linked registration expected");

        let user_wt = temp.path().join("user-wt");
        git_capture_in(
            &repo_path,
            &["worktree", "add", "--detach", user_wt.to_str().unwrap()],
            &[],
        )
        .unwrap();
        std::fs::rename(&user_wt, temp.path().join("user-wt-hidden")).unwrap();

        std::fs::remove_dir_all(&wt).unwrap();

        let report = rehydrate_worktree_from_ref(&wt, &repo_path, &snap, None).unwrap();
        assert_eq!(report.worktree_path, wt);
        assert_eq!(
            std::fs::read_to_string(wt.join("tracked.txt")).unwrap(),
            "edited"
        );
        assert!(
            repo_path.join(".git/worktrees/user-wt").exists(),
            "user registration must survive rehydrate even when its path is hidden"
        );
    }

    #[test]
    fn test_transfer_snapshot_to_repo_makes_standalone_ref_durable() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        init_git_repo(&repo_path);
        std::fs::write(repo_path.join("tracked.txt"), "original").unwrap();
        git_commit_all(&repo_path, "initial");

        // Standalone worktree: its own `.git` (independent object store + refs),
        // matching the production default that the live E2E exercised.
        let wt = temp.path().join("standalone-wt");
        crate::WorktreeBuilder::new(&repo_path, &wt)
            .standalone(true)
            .create()
            .unwrap();
        std::fs::write(wt.join("tracked.txt"), "edited").unwrap();
        std::fs::write(wt.join("untracked.txt"), "brand new").unwrap();

        let ref_name = "refs/echoagent/subagents/standalone";
        let snap = snapshot_worktree_to_ref(&wt, ref_name, "standalone snapshot").unwrap();

        // The snapshot lives only in the standalone's own `.git`, NOT in source.
        assert!(
            git_capture_in(
                &repo_path,
                &["rev-parse", "--verify", "--quiet", ref_name],
                &[]
            )
            .is_err(),
            "precondition: standalone snapshot ref must not yet exist in source"
        );

        // Transfer copies the commit/objects + ref into the source repo.
        transfer_snapshot_to_repo(&wt, &repo_path, ref_name).unwrap();
        assert_eq!(
            git_capture_in(&repo_path, &["rev-parse", ref_name], &[]).unwrap(),
            snap,
            "source repo ref must resolve to the snapshot commit after transfer"
        );

        // Deleting the standalone worktree (its `.git` too) must NOT lose the
        // snapshot — the bug this fix addresses.
        crate::remove_worktree(&wt).unwrap();
        assert_eq!(
            git_capture_in(&repo_path, &["rev-parse", ref_name], &[]).unwrap(),
            snap,
            "snapshot ref must survive standalone worktree deletion"
        );

        // Rehydrate from the source repo restores the captured working state.
        let report = rehydrate_worktree_from_ref(&wt, &repo_path, ref_name, None).unwrap();
        assert_eq!(report.worktree_path, wt);
        assert_eq!(
            std::fs::read_to_string(wt.join("tracked.txt")).unwrap(),
            "edited"
        );
        assert_eq!(
            std::fs::read_to_string(wt.join("untracked.txt")).unwrap(),
            "brand new"
        );
    }

    #[cfg(feature = "metadata")]
    #[test]
    fn test_rehydrate_registers_worktree_in_db() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();

        // Isolate the worktree DB (lock + ECHO_AGENT_HOME → private tmp + restore).
        let fx = crate::db::EchoAgentHomeFixture::new();

        let (repo_path, wt) = repo_with_worktree(&temp);
        std::fs::write(wt.join("tracked.txt"), "edited").unwrap();
        let snap = snapshot_worktree_to_ref(&wt, "refs/echoagent/snapshots/db", "db test").unwrap();
        crate::remove_worktree(&wt).unwrap();

        // Rehydrate into a UNIQUE-basename dest so its DB id can't collide with
        // the `wt` id other concurrent rehydrate tests write to this (process-
        // global ECHO_AGENT_HOME) DB and INSERT-OR-REPLACE our row.
        let dest = temp.path().join("subagent-db-rehydrate");
        let report =
            rehydrate_worktree_from_ref(&dest, &repo_path, &snap, Some("subagent-42")).unwrap();

        // Filter to OUR record by path: concurrent open_default writers may add
        // other subagent rows since ECHO_AGENT_HOME is process-global. Match the
        // canonical path register_worktree stores (/var → /private/var on macOS).
        let dest_canon = dunce::canonicalize(&dest).unwrap_or_else(|_| dest.clone());
        let db = crate::db::WorktreeDb::open(&fx.home).unwrap();
        let mine: Vec<_> = db
            .list(&crate::db::ListFilter {
                kind: Some(crate::db::WorktreeKind::Subagent),
                ..Default::default()
            })
            .unwrap()
            .into_iter()
            .filter(|r| r.path == dest || r.path == dest_canon)
            .collect();
        assert_eq!(mine.len(), 1, "exactly one rehydrated subagent record");
        assert_eq!(mine[0].kind, crate::db::WorktreeKind::Subagent);
        assert_eq!(mine[0].head_commit.as_deref(), Some(report.commit.as_str()));
        // session_id is threaded through to the DB record (create-path parity).
        assert_eq!(mine[0].session_id.as_deref(), Some("subagent-42"));
    }
}
