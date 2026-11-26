//! Shared git-repo dir-chain primitive.
//!
//! One `git2` discovery + one cwd→root walk, reused across the many repo-local
//! config marker checks the folder-trust gate runs back-to-back. Lives in its
//! own module (rather than `discovery`) because it is a generic repo-walk
//! primitive consumed cross-crate by `echo-agent-workspace`, not agent-definition
//! discovery.

use std::path::{Path, PathBuf};

/// The git worktree root for `cwd` (if any) plus the directory chain from `cwd`
/// up to that root (inclusive, cwd-first), resolved with ONE `git2` discovery
/// and ONE upward walk.
///
/// The folder-trust gate's `repo_configs_present` probes a dozen repo-local
/// code-exec markers (`.mcp.json`, `.echo-agent/config.toml`, `.claude/settings.json`,
/// project plugin/agent dirs, …) back-to-back on the agent startup path. Each
/// marker walker used to run its own `discover` + cwd→root walk; sharing one
/// `RepoDirChain` collapses that to a single traversal (each redundant syscall
/// is taxed 10-100x on Windows, and on a non-git dir each `discover` walks to
/// the filesystem root). Both the gate and the real loaders consume the same
/// chain via `*_in` walker variants, so detection can't drift from loading.
///
/// The public cwd-taking delegators (`find_project_configs`,
/// `project_plugin_dirs`, `project_agent_dirs`, …) now resolve through this
/// chain too, so their non-gate callers (config watcher, reloader, the mcp/
/// config loaders, inspect, upload, mcp_doctor) gain the per-level canonicalize
/// below. That is deliberate: all those callers are cold (startup / file-change /
/// session-setup / manual commands), never per-keystroke, and the canonical stop
/// is strictly more correct.
///
/// Outside a git repo `git_root` is `None` and `dirs` is just `[cwd]`, matching
/// every walker's no-repo branch (probe `cwd` only).
#[derive(Debug, Clone)]
pub struct RepoDirChain {
    /// Git worktree root (`workdir`), or `None` when `cwd` is not inside a repo.
    pub git_root: Option<PathBuf>,
    /// `cwd` up to and including `git_root`, cwd-first (`[cwd]` with no repo).
    pub dirs: Vec<PathBuf>,
}

impl RepoDirChain {
    /// Resolve the chain for `cwd`: ONE `git2` discovery + ONE upward walk.
    pub fn resolve(cwd: &Path) -> Self {
        let git_root = git2::Repository::discover(cwd)
            .ok()
            .and_then(|repo| repo.workdir().map(|p| p.to_path_buf()))
            // Home-is-a-git-repo (dotfiles in $HOME): a discovery that walks up
            // to $HOME must NOT treat the whole home subtree as one repo, or
            // home-level `.echo-agent`/`.mcp.json`/plugins would look repo-local. Drop
            // it so cwd is handled as no-repo (probe cwd only). Home is compared
            // canonically to match the symlink handling in the walk below.
            .filter(|root| !is_home_dir(root));

        let mut dirs = Vec::new();
        if let Some(ref root) = git_root {
            // Canonicalize only for the stop test so a symlinked cwd/ancestor
            // still halts AT the worktree root instead of over-walking to the
            // filesystem root; pushed dirs keep their original spelling (callers
            // `join` markers onto them, which resolve the same either way). The
            // per-level canonicalize is required to stop at root through a
            // symlinked ancestor while keeping raw spelling — do NOT reduce to a
            // 2-call `starts_with` variant (it would mis-handle a mid-chain
            // absolute symlink and reintroduce the over-walk).
            let root_canonical = dunce::canonicalize(root).unwrap_or_else(|_| root.clone());
            let mut current = Some(cwd.to_path_buf());
            while let Some(dir) = current {
                let dir_canonical = dunce::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
                let parent = dir.parent().map(|p| p.to_path_buf());
                dirs.push(dir);
                if dir_canonical == root_canonical {
                    break;
                }
                current = parent;
            }
        } else {
            dirs.push(cwd.to_path_buf());
        }

        Self { git_root, dirs }
    }
}

/// Whether `path` canonicalizes to the user's home directory. Local (not reused
/// from `echo-agent-workspace`, which depends on THIS crate) to keep the dep edge
/// one-way; backs the home-is-dotfiles guard in [`RepoDirChain::resolve`].
fn is_home_dir(path: &Path) -> bool {
    let Some(home) = echo_agent_config::discovery_home_dir() else {
        return false;
    };
    let canon = |p: &Path| dunce::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(path) == canon(&home)
}

/// Existing `<dir>/<subdir>` directories under each dir of a precomputed
/// cwd→git-root chain ([`RepoDirChain::dirs`]), in chain order (cwd-first, then
/// each `subdirs` entry in order). Shared body for the project plugin/agent dir
/// walkers so the byte-identical double-loop lives in one place.
pub(crate) fn existing_subdirs_along(chain_dirs: &[PathBuf], subdirs: &[&str]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for dir in chain_dirs {
        for subdir in subdirs {
            let candidate = dir.join(subdir);
            if candidate.is_dir() {
                found.push(candidate);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// RAII guard: set an env var, restore the prior value (or unset) on drop,
    /// so a test never leaves process-global env pointing at a dropped tempdir.
    struct EnvVarGuard {
        key: &'static str,
        prev: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let prev = std::env::var_os(key);
            unsafe { std::env::set_var(key, value) };
            Self { key, prev }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => unsafe { std::env::set_var(self.key, v) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    #[test]
    fn resolve_in_repo_yields_cwd_to_root_chain() {
        // A git-init'd tmp with a 2-deep subdir: the chain is cwd→root inclusive,
        // cwd-first, in the dirs' original spelling, and `git_root` is the root.
        let tmp = tempfile::tempdir().unwrap();
        git2::Repository::init(tmp.path()).unwrap();
        let nested = tmp.path().join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();

        let chain = RepoDirChain::resolve(&nested);
        assert_eq!(
            chain.dirs,
            vec![
                nested.clone(),
                tmp.path().join("a"),
                tmp.path().to_path_buf(),
            ]
        );
        // `git_root` is the canonical worktree root (git2's `workdir`); compare by
        // canonical form so a `/tmp`→`/private/tmp` symlink doesn't fail the test.
        let root = chain.git_root.expect("inside a repo");
        assert_eq!(
            dunce::canonicalize(&root).unwrap(),
            dunce::canonicalize(tmp.path()).unwrap()
        );
    }

    #[test]
    fn resolve_outside_repo_is_cwd_only() {
        // A non-git tmp: no discovery hit, so the chain is just `[cwd]` and there
        // is no git root. Only assert the no-repo shape when the temp dir is
        // genuinely outside any repo (a dev/CI checkout may place $TMPDIR inside
        // a larger git worktree).
        let tmp = tempfile::tempdir().unwrap();
        let plain = tmp.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        if git2::Repository::discover(&plain).is_err() {
            let chain = RepoDirChain::resolve(&plain);
            assert_eq!(chain.dirs, vec![plain]);
            assert_eq!(chain.git_root, None);
        }
    }

    #[test]
    #[serial(home_env)]
    fn resolve_treats_home_git_repo_as_no_repo() {
        // Home-is-a-git-repo (dotfiles in $HOME): discovery walks up to $HOME,
        // but the guard drops that root so a subdir resolves as no-repo (probe
        // cwd only) instead of spanning the whole home subtree. $HOME is guarded
        // (dirs::home_dir reads it) and canonicalized to match the guard.
        let tmp = tempfile::tempdir().unwrap();
        let home = dunce::canonicalize(tmp.path()).unwrap();
        git2::Repository::init(&home).unwrap();
        let _home_guard = EnvVarGuard::set("HOME", &home);
        let sub = home.join("proj");
        std::fs::create_dir_all(&sub).unwrap();

        let chain = RepoDirChain::resolve(&sub);
        assert_eq!(chain.git_root, None, "a home-dir git root must be dropped");
        assert_eq!(chain.dirs, vec![sub]);
    }

    #[test]
    #[serial(home_env)]
    fn resolve_keeps_non_home_git_root() {
        // The guard is home-EXACT: a git root that is NOT $HOME still resolves
        // normally (no over-trigger), so $HOME points at an unrelated dir here.
        let home = tempfile::tempdir().unwrap();
        let _home_guard = EnvVarGuard::set("HOME", home.path());
        let repo = tempfile::tempdir().unwrap();
        git2::Repository::init(repo.path()).unwrap();
        let sub = repo.path().join("pkg");
        std::fs::create_dir_all(&sub).unwrap();

        let chain = RepoDirChain::resolve(&sub);
        let root = chain.git_root.expect("a non-home git root must be kept");
        assert_eq!(
            dunce::canonicalize(&root).unwrap(),
            dunce::canonicalize(repo.path()).unwrap()
        );
    }
}
