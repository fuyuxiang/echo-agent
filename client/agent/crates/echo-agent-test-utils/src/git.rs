//! Hermetic git helpers for tests.
//!
//! When running under `bazel test`, the `GIT_BIN_PATH` environment variable
//! points to a statically-linked git binary provided by Bazel.  The helpers
//! in this module prepend that binary's directory to `PATH` so that
//! `Command::new("git")` resolves to it instead of relying on a
//! system-installed git.

use std::path::{Path, PathBuf};
use std::sync::Once;

static HERMETIC_GIT_INIT: Once = Once::new();

/// Prepend the hermetic git binary directory to `PATH` so that
/// `Command::new("git")` resolves to the Bazel-provided static binary
/// instead of relying on a system-installed git.
///
/// Safe to call multiple times — only the first call mutates `PATH`.
pub fn ensure_hermetic_git_on_path() {
    HERMETIC_GIT_INIT.call_once(|| {
        if let Ok(git_bin) = std::env::var("GIT_BIN_PATH") {
            let git_path = PathBuf::from(&git_bin);
            let git_path = if git_path.is_relative() {
                std::env::current_dir().unwrap().join(&git_path)
            } else {
                git_path
            };
            if let Some(bin_dir) = git_path.parent() {
                let current_path = std::env::var("PATH").unwrap_or_default();
                // SAFETY: called once via `Once` before any child processes are spawned.
                unsafe {
                    std::env::set_var("PATH", format!("{}:{}", bin_dir.display(), current_path));
                    // git-minimal spawns subcommands (`git stash` → `git
                    // update-index`) through its exec path, which is baked to
                    // a build-machine prefix. Helpers live next to the binary,
                    // so point the exec path there. Skip the host-fallback
                    // wrapper (`git-host-fallback.sh`): host git must keep its
                    // own exec path.
                    if git_path.file_name().is_some_and(|name| name == "git") {
                        std::env::set_var("GIT_EXEC_PATH", bin_dir);
                    }
                }
            }
        }
    });
}

/// Ensure the hermetic git binary is on `PATH` before running tests that
/// need git.  Call at the top of any `#[test]` that spawns `git` commands.
///
/// ```ignore
/// #[test]
/// fn my_git_test() {
///     echo_agent_test_utils::require_git!();
///     // ... git commands work here ...
/// }
/// ```
#[macro_export]
macro_rules! require_git {
    () => {
        $crate::git::ensure_hermetic_git_on_path();
    };
}

/// Initialise a fresh git repository at `path` with a dummy user config.
///
/// Calls [`ensure_hermetic_git_on_path`] first so the hermetic binary is used.
pub fn init_git_repo(path: &Path) {
    run_git(path, &["init"]);
    run_git(path, &["config", "user.email", "test@test.com"]);
    run_git(path, &["config", "user.name", "Test"]);
}

/// Stage everything and commit. Panics unless the commit succeeded or there
/// was nothing staged, since a silent no-op leaves the test asserting against
/// a state it never reached.
pub fn git_commit_all(path: &Path, message: &str) {
    run_git(path, &["add", "."]);
    let output = git_command_with_identity(path, &["commit", "-m", message])
        .output()
        .unwrap_or_else(|e| panic!("git commit failed to spawn: {e}"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() || stdout.contains("nothing to commit"),
        "git commit failed: {}{}",
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Run git in `dir`, assert success, and return trimmed stdout.
pub fn run_git(dir: &Path, args: &[&str]) -> String {
    run_git_with_env(dir, args, &[])
}

/// Git with a fixed identity and the machine's own configuration masked, so a
/// repository with no local identity still commits and a developer's global
/// settings cannot change what a test observes.
fn git_command_with_identity(dir: &Path, args: &[&str]) -> std::process::Command {
    ensure_hermetic_git_on_path();
    let mut cmd = std::process::Command::new("git");
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test User")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test User")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        // Callers assert on git's own wording, so pin the language the same way
        // the configuration is pinned.
        .env("LC_ALL", "C");
    cmd
}

/// [`run_git`] with extra environment variables, applied last so a caller can
/// override any of the defaults.
pub fn run_git_with_env(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> String {
    let mut cmd = git_command_with_identity(dir, args);
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let output = cmd
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} failed to spawn: {e}"));
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// `git init -b main` plus `core.excludesFile=/dev/null`, the shared preamble
/// for a seeded source repo.
pub fn git_init_seed(dir: &Path) {
    run_git(dir, &["init", "-b", "main"]);
    run_git(dir, &["config", "core.excludesFile", "/dev/null"]);
}

/// Create `<temp>/repo` with one committed `tracked.txt` and return its path.
pub fn seed_repo(temp: &Path) -> PathBuf {
    let repo = temp.join("repo");
    std::fs::create_dir(&repo).unwrap();
    init_git_repo(&repo);
    std::fs::write(repo.join("tracked.txt"), "original").unwrap();
    git_commit_all(&repo, "initial");
    repo
}

/// [`seed_repo`] plus a bare `origin` remote holding `refs/heads/main`.
/// Returns `(repo, remote)`.
pub fn seed_repo_with_remote(temp: &Path) -> (PathBuf, PathBuf) {
    let repo = seed_repo(temp);
    let remote = temp.join("remote.git");
    run_git(temp, &["init", "--bare", remote.to_str().unwrap()]);
    run_git(
        &repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    run_git(&repo, &["push", "origin", "HEAD:refs/heads/main"]);
    run_git(&repo, &["fetch", "origin"]);
    (repo, remote)
}

/// Commit a modification to `tracked.txt`, then `reset --hard HEAD~1` so only
/// the reflog names the discarded commit. `when` pins the author/committer date
/// (e.g. to age the commit); `None` uses the current time. Returns the discarded
/// commit's SHA and asserts no ref reaches it.
pub fn reflog_only_commit(worktree: &Path, when: Option<&str>) -> String {
    std::fs::write(worktree.join("tracked.txt"), "three hours of work\n").unwrap();
    let dates = when.map(|w| [("GIT_AUTHOR_DATE", w), ("GIT_COMMITTER_DATE", w)]);
    run_git_with_env(
        worktree,
        &["commit", "-am", "work only this worktree saw"],
        dates.as_ref().map(|d| &d[..]).unwrap_or(&[]),
    );
    let discarded = run_git(worktree, &["rev-parse", "HEAD"]);
    run_git(worktree, &["reset", "--hard", "HEAD~1"]);
    assert_eq!(
        run_git(
            worktree,
            &["rev-list", "--max-count=1", &discarded, "--not", "--all"]
        ),
        discarded,
        "no ref names it, so only the reflog does"
    );
    discarded
}

/// Write a grouped fan-out tree of ~`files` files (`files_per_dir` per
/// directory, directories bucketed 100 per group) under `dir`. No git
/// operations — callers stage/commit as needed.
pub fn write_fanout_tree(dir: &Path, files: usize, files_per_dir: usize) {
    for d in 0..files.div_ceil(files_per_dir) {
        let sub = dir.join(format!("g{}", d / 100)).join(format!("d{d}"));
        std::fs::create_dir_all(&sub).expect("create populated dir");
        for f in 0..files_per_dir {
            std::fs::write(
                sub.join(format!("file_{f}.txt")),
                format!("content {d} {f}\n"),
            )
            .expect("write populated file");
        }
    }
}

/// Create a `feature` branch with `picks` one-file commits off the current
/// HEAD, advance the base branch by one commit (so a rebase has work), and
/// leave `feature` checked out. Returns the base branch name.
pub fn make_feature_branch(dir: &Path, picks: usize) -> String {
    let base = run_git(dir, &["rev-parse", "--abbrev-ref", "HEAD"]);
    run_git(dir, &["checkout", "-b", "feature"]);
    for k in 0..picks {
        let name = format!("pick_{k}.txt");
        std::fs::write(dir.join(&name), format!("pick {k}\n")).expect("write pick file");
        run_git(dir, &["add", &name]);
        run_git(dir, &["commit", "-m", &format!("pick {k}")]);
    }
    run_git(dir, &["checkout", &base]);
    std::fs::write(dir.join("base_advance.txt"), "advance\n").expect("write base advance file");
    run_git(dir, &["add", "base_advance.txt"]);
    run_git(dir, &["commit", "-m", "advance base"]);
    run_git(dir, &["checkout", "feature"]);
    base
}
