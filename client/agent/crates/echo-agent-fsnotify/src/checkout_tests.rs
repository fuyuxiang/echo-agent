use super::*;
use std::fs;
use tempfile::TempDir;

/// A repository declaring `deps/lib` as a submodule, beside checkouts it does not.
fn workspace() -> TempDir {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    git2::Repository::init(root).unwrap();
    fs::write(
        root.join(".gitmodules"),
        "[submodule \"deps/lib\"]\n\tpath = deps/lib\n\turl = ../lib.git\n",
    )
    .unwrap();

    fs::create_dir_all(root.join("deps/lib/src")).unwrap();
    fs::write(
        root.join("deps/lib/.git"),
        "gitdir: ../../.git/modules/lib\n",
    )
    .unwrap();

    fs::create_dir_all(root.join("vendor/upstream/.git")).unwrap();

    let worktree = root.join(".harness/worktrees/feature");
    fs::create_dir_all(worktree.join("src")).unwrap();
    fs::write(
        worktree.join(".git"),
        "gitdir: /elsewhere/.git/worktrees/x\n",
    )
    .unwrap();

    fs::create_dir_all(root.join("sl-repo/.sl")).unwrap();
    fs::create_dir_all(root.join("crates/core")).unwrap();
    temp
}

#[test]
fn only_an_undeclared_checkout_is_another_workspace() {
    let temp = workspace();
    let root = temp.path();

    assert!(!is_another_workspace(&root.join("deps/lib")));
    assert!(is_another_workspace(&root.join("vendor/upstream")));
    assert!(is_another_workspace(
        &root.join(".harness/worktrees/feature")
    ));
    assert!(is_another_workspace(&root.join("sl-repo")));
    assert!(!is_another_workspace(&root.join("crates/core")));
}

/// Per-dir decides at every level, so a worktree anywhere ends coverage.
/// Fan-out watches each top-level child recursively, so only a checkout that
/// is itself a top-level child does.
#[test]
fn coverage_follows_the_watch_strategy() {
    let temp = workspace();
    let root = temp.path();
    let nested = root.join(".harness/worktrees/feature/src");
    let top_level = root.join("vendored");
    fs::create_dir_all(top_level.join(".git")).unwrap();

    let per_dir = |path: &Path| watch_root_covers_with(WatchStrategy::PerDir, root, path);
    assert!(per_dir(root));
    assert!(per_dir(&root.join("deps/lib/src")));
    assert!(!per_dir(&nested));

    let fanout = |path: &Path| watch_root_covers_with(WatchStrategy::Fanout, root, path);
    assert!(fanout(&nested));
    assert!(!fanout(&top_level.join("src")));
}
