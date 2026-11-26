use super::*;

#[test]
fn transient_session_files_in_the_registration_dir_do_not_keep_the_worktree() {
    let fixture = Fixture::new("");
    let worktree = fixture.linked_worktree("session-leftovers");
    let git_dir = fixture.source.join(".git/worktrees/session-leftovers");
    run_git(&worktree, &["config", "rerere.enabled", "true"]);
    std::fs::write(worktree.join("tracked.txt"), "two\n").unwrap();
    run_git(&worktree, &["commit", "-am", "work"]);
    std::fs::write(
        git_dir.join("AUTO_MERGE"),
        run_git(&worktree, &["rev-parse", "HEAD^{tree}"]),
    )
    .unwrap();
    run_git(&worktree, &["sparse-checkout", "set", "--cone", "nested"]);
    run_git(&worktree, &["sparse-checkout", "disable"]);
    assert!(git_dir.join("info/sparse-checkout").exists());
    publish(&worktree, "leftovers");
    run_git(&worktree, &["update-index", "--split-index"]);
    std::fs::create_dir_all(git_dir.join("fsmonitor--daemon/cookies")).unwrap();
    std::fs::write(git_dir.join("fsmonitor--daemon.ipc"), b"").unwrap();
    std::fs::write(
        git_dir.join("REBASE_HEAD"),
        run_git(&worktree, &["rev-parse", "HEAD"]),
    )
    .unwrap();
    assert_eq!(
        std::fs::metadata(git_dir.join("MERGE_RR")).unwrap().len(),
        0
    );
    assert!(
        std::fs::read_dir(&git_dir).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("sharedindex.")
        }),
        "the split index must have a base for this test to be about one"
    );
    run_git(&worktree, &["config", "--worktree", "user.email", "a@b.c"]);
    assert_eq!(
        safe_to_delete_worktree(&worktree, None),
        Safety::Keep(KeepReason::WorktreeLocalState(
            "config.worktree".to_string()
        )),
        "a key that is not about the checkout is still state"
    );
    run_git(
        &worktree,
        &["config", "--worktree", "--unset", "user.email"],
    );

    assert_eq!(reclaim(&worktree), Safety::Delete);
    assert!(!worktree.exists());
}

#[test]
fn interrupted_rebase_keeps_the_worktree() {
    let fixture = Fixture::new("");
    let worktree = fixture.linked_worktree("rebasing");
    std::fs::write(worktree.join("tracked.txt"), "two\n").unwrap();
    run_git(&worktree, &["commit", "-am", "work"]);
    publish(&worktree, "rebased");
    run_git_with_env(
        &worktree,
        &["rebase", "-i", "HEAD~1"],
        &[("GIT_SEQUENCE_EDITOR", "printf 'break\\n' >")],
    );
    assert!(
        fixture
            .source
            .join(".git/worktrees/rebasing/rebase-merge")
            .is_dir()
    );
    assert_eq!(run_git(&worktree, &["status", "--porcelain"]), "");

    assert_eq!(
        reclaim(&worktree),
        Safety::Keep(KeepReason::WorktreeLocalState("rebase-merge".to_string()))
    );
    assert!(
        run_git(&worktree, &["status"]).contains("rebase in progress"),
        "the rebase the deletion would have thrown away is still there"
    );
}

#[test]
fn stash_keeps_a_standalone_worktree() {
    let fixture = Fixture::new("");
    let worktree = fixture.standalone_worktree("standalone-stash");
    std::fs::write(worktree.join("tracked.txt"), "stashed\n").unwrap();
    run_git(&worktree, &["stash", "push", "-m", "work in progress"]);
    assert_eq!(run_git(&worktree, &["status", "--porcelain"]), "");

    assert_eq!(
        reclaim(&worktree),
        Safety::Keep(KeepReason::Unpushed),
        "the stash commit is on no remote"
    );
    assert_eq!(
        run_git(&worktree, &["show", "stash@{0}:tracked.txt"]),
        "stashed"
    );
}

/// No snapshot transfer carries the bytes beside the object store, and
/// reachability says nothing about them, so a worktree holding one stays.
#[test]
fn lfs_object_beside_the_store_keeps_the_worktree() {
    let fixture = Fixture::new("");
    for (name, worktree, surviving, storage) in [
        (
            "snapshot",
            fixture.snapshot_worktree("holds-lfs-bytes"),
            Some(fixture.source.clone()),
            None,
        ),
        (
            "standalone",
            fixture.standalone_worktree("lfs-no-survivor"),
            None,
            None,
        ),
        // `lfs.storage` moves the store. Looking only where the default puts
        // it reads a relocated one as empty, which is a Delete on the bytes.
        (
            "relocated",
            fixture.standalone_worktree("lfs-elsewhere"),
            None,
            Some("payloads"),
        ),
    ] {
        let store = match storage {
            Some(storage) => {
                run_git(&worktree, &["config", "lfs.storage", storage]);
                worktree.join(".git").join(storage)
            }
            None => worktree.join(".git/lfs"),
        };
        let object = store.join("objects/26/cc/26ccfbb9deadbeef");
        std::fs::create_dir_all(object.parent().unwrap()).unwrap();
        std::fs::write(&object, b"the real bytes").unwrap();

        assert_eq!(
            reclaim_beside(&worktree, surviving.as_deref()),
            Safety::Keep(KeepReason::WorktreeLocalState(
                "lfs/objects/26/cc/26ccfbb9deadbeef".to_string()
            )),
            "{name}"
        );
        assert_eq!(
            std::fs::read(&object).expect("the bytes are gone"),
            b"the real bytes"
        );
    }
}

#[test]
fn submodule_store_the_source_does_not_hold_keeps_the_snapshot() {
    let fixture = Fixture::new("");
    fixture.add_source_clutter();
    let worktree = fixture.snapshot_worktree("submodule-work");
    let theirs = fixture.source.join(".git/modules/vendor/example-lib");
    assert_eq!(
        run_git(&theirs, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "main"
    );
    run_git(&theirs, &["update-ref", "-d", "refs/heads/main"]);
    // The reflog names it too, and that counts as holding it.
    std::fs::remove_dir_all(theirs.join("logs")).unwrap();

    let Safety::Keep(KeepReason::WorktreeLocalState(held)) =
        reclaim_beside(&worktree, Some(&fixture.source))
    else {
        panic!("a store the source does not hold let the snapshot go");
    };
    assert!(held.starts_with("modules/vendor/example-lib/"), "{held}");
    assert!(worktree.exists());
}

/// A store under the registration dies with it. Nothing here reasons about
/// what it holds, so any file keeps the worktree (via the registration-state
/// allowlist, not the dying-store check).
#[test]
fn a_store_under_the_registration_keeps_the_worktree() {
    let fixture = Fixture::new("");
    let worktree = fixture.linked_worktree("with-submodule");
    let store = fixture
        .source
        .join(".git/worktrees/with-submodule/modules/vendor/example-lib");
    seed_module_store(fixture.source.parent().unwrap(), &store);
    let committed = run_git(&store, &["rev-parse", "HEAD"]);

    assert_eq!(
        reclaim(&worktree),
        Safety::Keep(KeepReason::WorktreeLocalState("modules".to_string()))
    );
    assert_eq!(
        run_git(&store, &["rev-parse", "HEAD"]),
        committed,
        "the store is gone"
    );
}

/// The over-keep guard: when the survivor holds the LFS object at the same path,
/// the dying-store check finds nothing missing and the standalone copy is still
/// reclaimed. Without this a broken store pairing would silently keep every
/// worktree, and no keep-direction test would notice.
#[test]
fn an_lfs_object_the_survivor_also_holds_does_not_keep_the_worktree() {
    let fixture = Fixture::new("");
    let worktree = fixture.snapshot_worktree("lfs-shared");
    let relative = "lfs/objects/26/cc/26ccfbb9deadbeef";
    for root in [worktree.join(".git"), fixture.source.join(".git")] {
        let object = root.join(relative);
        std::fs::create_dir_all(object.parent().unwrap()).unwrap();
        std::fs::write(&object, b"the real bytes").unwrap();
    }

    assert_eq!(
        reclaim_beside(&worktree, Some(&fixture.source)),
        Safety::Delete,
        "an object the survivor also holds is not a last copy"
    );
    assert!(!worktree.exists());
}

/// Fail-closed: a store we cannot read (here `.git/modules` is a file, not a
/// directory) is a "couldn't tell", so the worktree is kept, never deleted.
#[test]
fn a_store_that_cannot_be_read_keeps_the_worktree() {
    let fixture = Fixture::new("");
    let worktree = fixture.standalone_worktree("unreadable-store");
    std::fs::write(worktree.join(".git/modules"), b"not a directory").unwrap();

    assert_eq!(
        reclaim_beside(&worktree, None),
        Safety::Keep(KeepReason::CheckFailed)
    );
    assert!(worktree.exists());
}
