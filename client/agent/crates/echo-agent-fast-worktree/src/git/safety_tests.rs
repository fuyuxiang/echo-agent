use super::conversions::hashes_line_up;
use super::*;
use crate::test_support::{add_worktree, publish, seed_source};
use echo_agent_test_utils::git::{run_git, run_git_with_env};
use std::path::PathBuf;

#[path = "safety_tests/conversions.rs"]
mod conversions;
#[path = "safety_tests/gate.rs"]
mod gate;
#[path = "safety_tests/git_dir.rs"]
mod git_dir;
#[path = "safety_tests/reachability.rs"]
mod reachability;
fn reclaim(worktree: &Path) -> Safety {
    reclaim_beside(worktree, None)
}

fn reclaim_beside(worktree: &Path, surviving: Option<&Path>) -> Safety {
    let safety = safe_to_delete_worktree(worktree, surviving);
    if safety == Safety::Delete {
        crate::remove_worktree(worktree).expect("removal");
    }
    safety
}

fn reclaim_after_snapshot(worktree: &Path, source: &Path, ref_name: &str) -> Safety {
    snapshot_into(worktree, source, ref_name);
    let safety = safe_to_delete_worktree_after_snapshot(worktree, Some(source), ref_name);
    if safety == Safety::Delete {
        crate::remove_worktree(worktree).expect("removal");
    }
    safety
}

#[path = "safety_tests/working_tree.rs"]
mod working_tree;

struct Fixture {
    _root: tempfile::TempDir,
    source: PathBuf,
    remote: PathBuf,
}

impl Fixture {
    fn new(ignore_lines: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        seed_source(&source, ignore_lines);
        Self {
            remote: root.path().join("remote.git"),
            source,
            _root: root,
        }
    }

    fn linked_worktree(&self, name: &str) -> PathBuf {
        add_worktree(&self.source, &self.source.with_file_name(name))
    }

    fn snapshot_worktree(&self, name: &str) -> PathBuf {
        let at = self.source.with_file_name(name);
        copy_tree(&self.source, &at);
        at
    }

    fn add_source_clutter(&self) {
        std::fs::write(self.source.join("tracked.txt"), "tagged\n").unwrap();
        run_git(
            &self.source,
            &["commit", "-am", "a commit only a tag holds"],
        );
        let tagged = run_git(&self.source, &["rev-parse", "HEAD"]);
        run_git(&self.source, &["tag", "backup-2026-08-07"]);
        run_git(&self.source, &["reset", "--hard", "HEAD~1"]);
        for name in ["refs/prefetch/origin/main", "refs/backup/nightly"] {
            run_git(&self.source, &["update-ref", name, &tagged]);
        }
        run_git(&self.source, &["update-ref", "refs/stash", &tagged]);
        seed_module_store(
            self.source.parent().unwrap(),
            &self.source.join(".git/modules/vendor/example-lib"),
        );
    }

    fn standalone_worktree(&self, name: &str) -> PathBuf {
        let at = self.source.with_file_name(name);
        run_git(
            self.source.parent().unwrap(),
            &[
                "clone",
                "--branch",
                "main",
                self.remote.to_str().unwrap(),
                at.to_str().unwrap(),
            ],
        );
        at
    }
}

fn seed_module_store(root: &Path, at: &Path) {
    let scratch = root.join("module-scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    echo_agent_test_utils::git::git_init_seed(&scratch);
    std::fs::write(scratch.join("file.txt"), "sub\n").unwrap();
    run_git(&scratch, &["add", "."]);
    run_git(&scratch, &["commit", "-m", "submodule seed"]);
    std::fs::create_dir_all(at.parent().unwrap()).unwrap();
    copy_tree(&scratch.join(".git"), at);
    std::fs::remove_dir_all(&scratch).unwrap();
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let (from, to) = (entry.path(), to.join(entry.file_name()));
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&from, &to);
        } else {
            std::fs::copy(&from, &to).unwrap();
        }
    }
}

fn attributes(worktree: &Path, rules: impl AsRef<[u8]>, branch: &str) {
    std::fs::write(worktree.join(".gitattributes"), rules).unwrap();
    run_git(worktree, &["add", ".gitattributes"]);
    run_git(worktree, &["commit", "-m", "route the paths"]);
    publish(worktree, branch);
}

fn snapshot_into(worktree: &Path, source: &Path, ref_name: &str) {
    crate::snapshot_worktree_to_ref(worktree, ref_name, "snapshot").unwrap();
    crate::transfer_snapshot_to_repo(worktree, source, ref_name).unwrap();
}
