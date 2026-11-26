use super::*;
use echo_agent_test_utils::git::reflog_only_commit;

#[test]
fn commit_no_remote_holds_keeps_the_worktree() {
    let fixture = Fixture::new("");
    let worktree = fixture.linked_worktree("committed");
    std::fs::write(worktree.join("tracked.txt"), "two\n").unwrap();
    run_git(&worktree, &["commit", "-am", "work"]);

    assert_eq!(reclaim(&worktree), Safety::Keep(KeepReason::Unpushed));
    assert_eq!(
        std::fs::read(worktree.join("tracked.txt")).expect("the commit's bytes are gone"),
        b"two\n"
    );

    publish(&worktree, "work");
    assert_eq!(reclaim(&worktree), Safety::Delete);
    assert!(!worktree.exists());
    assert_eq!(
        run_git(&fixture.remote, &["show", "refs/heads/work:tracked.txt"]),
        "two",
        "the remote must hold the bytes the deletion took"
    );
}

#[test]
fn commit_only_orig_head_holds_does_not_keep_the_worktree() {
    let fixture = Fixture::new("");
    let worktree = fixture.linked_worktree("reset-hard");
    std::fs::write(worktree.join("tracked.txt"), "discarded\n").unwrap();
    run_git(&worktree, &["commit", "-am", "work a reset throws away"]);
    run_git(&worktree, &["reset", "--hard", "HEAD~1"]);
    let registration = fixture.source.join(".git/worktrees/reset-hard");
    std::fs::remove_dir_all(registration.join("logs")).unwrap();
    assert_eq!(run_git(&worktree, &["status", "--porcelain"]), "");
    assert!(
        registration.join("ORIG_HEAD").exists(),
        "the file this test is about must be there"
    );

    assert_eq!(reclaim(&worktree), Safety::Delete);
    assert!(!worktree.exists());
}

#[test]
fn worktree_that_is_itself_a_parent_keeps_its_children() {
    let fixture = Fixture::new("");
    let parent = fixture.standalone_worktree("standalone-parent");
    let child = parent.with_file_name("standalone-child");
    run_git(
        &parent,
        &["worktree", "add", "--detach", child.to_str().unwrap()],
    );
    std::fs::write(child.join("tracked.txt"), "only the child holds this\n").unwrap();
    run_git(&child, &["commit", "-am", "work only the child holds"]);
    let saved = run_git(&child, &["rev-parse", "HEAD"]);
    run_git(&child, &["update-ref", "refs/worktree/save", &saved]);

    assert_eq!(
        reclaim_beside(&parent, Some(&fixture.source)),
        Safety::Keep(KeepReason::OnlyCopy)
    );
    assert_eq!(
        run_git(&child, &["show", "refs/worktree/save:tracked.txt"]),
        "only the child holds this"
    );
}

#[test]
fn child_on_a_commit_of_its_own_keeps_the_parent() {
    let fixture = Fixture::new("");
    let parent = fixture.standalone_worktree("detached-parent");
    let child = parent.with_file_name("detached-child");
    run_git(
        &parent,
        &["worktree", "add", "--detach", child.to_str().unwrap()],
    );
    std::fs::write(child.join("tracked.txt"), "only this commit holds it\n").unwrap();
    run_git(&child, &["commit", "-am", "work no ref names"]);

    assert_eq!(run_git(&child, &["status", "--porcelain"]), "");
    assert_eq!(
        reclaim_beside(&parent, Some(&fixture.source)),
        Safety::Keep(KeepReason::OnlyCopy)
    );
    assert_eq!(
        run_git(&child, &["show", "HEAD:tracked.txt"]),
        "only this commit holds it"
    );
}

#[test]
fn commit_a_surviving_branch_holds_lets_the_worktree_go() {
    let fixture = Fixture::new("");
    let worktree = fixture.linked_worktree("branched");
    run_git(&worktree, &["checkout", "-b", "saved"]);
    std::fs::write(worktree.join("tracked.txt"), "saved\n").unwrap();
    run_git(&worktree, &["commit", "-am", "work a branch holds"]);

    assert_eq!(reclaim(&worktree), Safety::Delete);
    assert!(!worktree.exists());
    assert_eq!(
        run_git(&fixture.source, &["show", "saved:tracked.txt"]),
        "saved",
        "the branch must hold the bytes the deletion took"
    );
}

#[test]
fn standalone_worktree_goes_only_when_a_remote_holds_every_local_ref() {
    let fixture = Fixture::new("");

    let unpushed = fixture.standalone_worktree("standalone-unpushed");
    std::fs::write(unpushed.join("tracked.txt"), "two\n").unwrap();
    run_git(&unpushed, &["checkout", "-b", "work"]);
    run_git(&unpushed, &["commit", "-am", "a branch that dies with it"]);
    run_git(&unpushed, &["checkout", "--detach", "origin/main"]);
    assert_eq!(reclaim(&unpushed), Safety::Keep(KeepReason::Unpushed));
    assert_eq!(
        run_git(&unpushed, &["show", "work:tracked.txt"]),
        "two",
        "the branch's bytes are gone"
    );

    let pushed = fixture.standalone_worktree("standalone-pushed");
    assert_eq!(reclaim(&pushed), Safety::Delete);
    assert!(!pushed.exists(), "a standalone worktree is reclaimable");
}

#[test]
fn snapshot_of_the_source_goes_when_the_source_still_holds_it() {
    let fixture = Fixture::new("");
    fixture.add_source_clutter();
    let worktree = fixture.snapshot_worktree("pristine-snapshot");
    let repo = gix::open(&worktree).unwrap();
    assert_eq!(repo.common_dir(), repo.git_dir());
    assert!(worktree.join(".git/modules/vendor/example-lib").is_dir());
    assert!(worktree.join(".git/ORIG_HEAD").exists());

    assert_eq!(reclaim(&worktree), Safety::Keep(KeepReason::Unpushed));
    assert_eq!(
        reclaim_beside(&worktree, Some(&fixture.source)),
        Safety::Delete
    );
    assert!(!worktree.exists());
    assert_eq!(
        run_git(&fixture.source, &["show", "backup-2026-08-07:tracked.txt"]),
        "tagged",
        "the source must still hold what the deletion took"
    );
}

#[test]
fn commit_the_source_reaches_but_does_not_name_lets_the_snapshot_go() {
    let fixture = Fixture::new("");
    std::fs::write(fixture.source.join("tracked.txt"), "two\n").unwrap();
    run_git(&fixture.source, &["commit", "-am", "second"]);
    run_git(&fixture.source, &["push", "origin", "HEAD:refs/heads/main"]);
    run_git(&fixture.source, &["fetch", "origin"]);
    let worktree = fixture.snapshot_worktree("older-head");
    run_git(&worktree, &["checkout", "--detach", "HEAD~1"]);

    assert_eq!(
        reclaim_beside(&worktree, Some(&fixture.source)),
        Safety::Delete
    );
    assert!(!worktree.exists());
}

#[test]
fn repository_that_keeps_no_reflog_is_still_judged() {
    let fixture = Fixture::new("");
    let worktree = fixture.standalone_worktree("no-reflog");
    run_git(&worktree, &["config", "core.logAllRefUpdates", "false"]);
    std::fs::remove_dir_all(worktree.join(".git/logs")).unwrap();

    assert_eq!(
        reclaim_beside(&worktree, Some(&fixture.source)),
        Safety::Delete
    );
    assert!(!worktree.exists());
}

#[test]
fn commit_the_registration_reflog_named_keeps_the_worktree() {
    let fixture = Fixture::new("");
    let worktree = fixture.linked_worktree("reset-away");
    let discarded = reflog_only_commit(&worktree, None);

    assert_eq!(reclaim(&worktree), Safety::Keep(KeepReason::Unpushed));
    assert!(worktree.exists());
    assert_eq!(
        run_git(&worktree, &["show", &format!("{discarded}:tracked.txt")]),
        "three hours of work",
        "the commit the deletion would have taken is still here"
    );
}

#[test]
fn commit_only_the_reflog_names_keeps_the_snapshot() {
    let fixture = Fixture::new("");
    let worktree = fixture.snapshot_worktree("reset-hard");
    let discarded = reflog_only_commit(&worktree, None);
    assert_eq!(run_git(&worktree, &["status", "--porcelain"]), "");

    assert_eq!(
        reclaim_beside(&worktree, Some(&fixture.source)),
        Safety::Keep(KeepReason::OnlyCopy)
    );
    assert_eq!(
        run_git(&worktree, &["show", &format!("{discarded}:tracked.txt")]),
        "three hours of work",
        "the reflog is the only place holding it"
    );
}

#[test]
fn the_worktree_cannot_be_its_own_surviving_repository() {
    let fixture = Fixture::new("");
    let worktree = fixture.snapshot_worktree("swallowed-source");
    std::fs::write(worktree.join("tracked.txt"), "work\n").unwrap();
    run_git(&worktree, &["commit", "-am", "work the source never saw"]);

    assert_eq!(
        reclaim_beside(&worktree, Some(&worktree)),
        Safety::Keep(KeepReason::Unpushed)
    );
    assert!(worktree.exists());
}
