use super::*;

#[test]
fn snapshotted_subagent_worktree_goes_and_its_work_stays() {
    let fixture = Fixture::new("");
    let worktree = fixture.linked_worktree("subagent");
    std::fs::write(worktree.join("tracked.txt"), "edited\n").unwrap();
    run_git(&worktree, &["commit", "-am", "a commit no remote holds"]);
    std::fs::write(worktree.join("untracked.rs"), b"untracked work").unwrap();
    assert_eq!(
        safe_to_delete_worktree(&worktree, Some(&fixture.source)),
        Safety::Keep(KeepReason::Dirty)
    );

    let ref_name = "refs/echoagent/subagents/test";
    assert_eq!(
        reclaim_after_snapshot(&worktree, &fixture.source, ref_name),
        Safety::Delete
    );
    assert!(!worktree.exists());
    for (path, bytes) in [
        ("tracked.txt", "edited"),
        ("untracked.rs", "untracked work"),
    ] {
        assert_eq!(
            run_git(&fixture.source, &["show", &format!("{ref_name}:{path}")]),
            bytes,
            "the snapshot must hold what the deletion took"
        );
    }
}

#[test]
fn content_a_snapshot_cannot_capture_keeps_the_worktree() {
    use KeepReason::{HiddenFromStatus, IgnoredContent, WorktreeLocalState};
    type Case = (&'static str, fn(&Path, &Path), KeepReason);
    let cases: &[Case] = &[
        (
            "ignored-content",
            |worktree, _| {
                std::fs::write(worktree.join(".gitignore"), ".env\n").unwrap();
                std::fs::write(worktree.join(".env"), b"secret").unwrap();
                run_git(worktree, &["commit", "-am", "ignore .env"]);
            },
            IgnoredContent(".env".to_string()),
        ),
        (
            "hidden-index-bit",
            |worktree, _| {
                run_git(
                    worktree,
                    &["update-index", "--skip-worktree", "tracked.txt"],
                );
            },
            HiddenFromStatus,
        ),
        (
            "per-worktree-ref",
            |worktree, git_dir| {
                let head = run_git(worktree, &["rev-parse", "HEAD"]);
                run_git(worktree, &["update-ref", "refs/worktree/save", &head]);
                assert!(git_dir.join("refs/worktree/save").exists());
            },
            WorktreeLocalState("refs".to_string()),
        ),
    ];
    for (case, disturb, want) in cases {
        let fixture = Fixture::new("");
        let worktree = fixture.linked_worktree(case);
        disturb(&worktree, &fixture.source.join(".git/worktrees").join(case));

        assert_eq!(
            reclaim_after_snapshot(&worktree, &fixture.source, "refs/echoagent/subagents/test"),
            Safety::Keep(want.clone()),
            "{case}"
        );
        assert!(worktree.exists(), "{case}: the worktree must still be here");
    }
}

#[test]
fn work_written_after_the_snapshot_keeps_the_worktree() {
    let fixture = Fixture::new("");
    let worktree = fixture.linked_worktree("late-writer");
    let ref_name = "refs/echoagent/subagents/late";
    snapshot_into(&worktree, &fixture.source, ref_name);
    assert_eq!(
        safe_to_delete_worktree_after_snapshot(&worktree, Some(&fixture.source), ref_name),
        Safety::Delete
    );

    std::fs::write(worktree.join("late.rs"), b"written after the snapshot").unwrap();

    assert_eq!(
        safe_to_delete_worktree_after_snapshot(&worktree, Some(&fixture.source), ref_name),
        Safety::Keep(KeepReason::Dirty)
    );
    assert_eq!(
        std::fs::read(worktree.join("late.rs")).expect("the late write is gone"),
        b"written after the snapshot"
    );
}

#[test]
fn a_converted_file_keeps_a_snapshotted_worktree() {
    type Case = (
        &'static str,
        fn(&Path),
        &'static str,
        &'static [u8],
        KeepReason,
    );
    let cases: &[Case] = &[
        (
            "clean-filter",
            |worktree| {
                attributes(worktree, "*.log filter=stripout\n", "clean-filter");
                run_git(
                    worktree,
                    &["config", "filter.stripout.clean", "sed /^OUT:/d"],
                );
                std::fs::write(
                    worktree.join("notes.log"),
                    b"keep this\nOUT: six hours of output\n",
                )
                .unwrap();
            },
            "notes.log",
            b"keep this\nOUT: six hours of output\n",
            KeepReason::NotInSnapshot("notes.log".to_string()),
        ),
        (
            "check-in",
            |worktree| {
                attributes(worktree, "*.csv text\n", "check-in");
                std::fs::write(worktree.join("export.csv"), b"a,b\r\n").unwrap();
            },
            "export.csv",
            b"a,b\r\n",
            KeepReason::NotInSnapshot("export.csv".to_string()),
        ),
    ];
    for (case, disturb, file, bytes, want) in cases {
        let fixture = Fixture::new("");
        let worktree = fixture.linked_worktree(case);
        disturb(&worktree);
        let ref_name = format!("refs/echoagent/subagents/{case}");
        snapshot_into(&worktree, &fixture.source, &ref_name);

        assert_eq!(
            safe_to_delete_worktree_after_snapshot(&worktree, Some(&fixture.source), &ref_name),
            Safety::Keep(want.clone()),
            "{case}"
        );
        assert_eq!(
            std::fs::read(worktree.join(file)).expect("the converted output is gone"),
            *bytes,
            "{case}"
        );
    }
}

#[cfg_attr(target_os = "macos", ignore = "APFS rejects these filenames")]
#[test]
fn a_converted_file_whose_name_reads_as_a_pathspec_keeps_the_worktree() {
    let fixture = Fixture::new("");
    let worktree = fixture.linked_worktree("pathspec-magic");
    let ref_name = "refs/echoagent/subagents/pathspec-magic";
    attributes(&worktree, "*.log filter=stripout\n", "pathspec-magic");
    run_git(
        &worktree,
        &["config", "filter.stripout.clean", "sed /^OUT:/d"],
    );
    let held = b"keep this\nOUT: six hours of output\n";
    let notes = worktree.join(":notes.log");
    std::fs::write(&notes, held).unwrap();
    snapshot_into(&worktree, &fixture.source, ref_name);

    assert_eq!(
        safe_to_delete_worktree_after_snapshot(&worktree, Some(&fixture.source), ref_name),
        Safety::Keep(KeepReason::NotInSnapshot(":notes.log".to_string()))
    );
    assert_eq!(
        std::fs::read(&notes).expect("the filtered output is gone"),
        held
    );
}

#[cfg(unix)]
#[cfg_attr(target_os = "macos", ignore = "APFS rejects these filenames")]
#[test]
fn paths_that_differ_only_outside_utf8_are_told_apart() {
    use std::os::unix::ffi::OsStrExt;

    let fixture = Fixture::new("");
    let worktree = fixture.linked_worktree("not-quite-text");
    let ref_name = "refs/echoagent/subagents/not-quite-text";
    let exempt = worktree.join(std::ffi::OsStr::from_bytes(b"a\xff.csv"));
    std::fs::write(&exempt, b"a,b\r\n").unwrap();
    let mut rules = Vec::new();
    rules.extend_from_slice(b"a\xff.csv -text\n");
    rules.extend_from_slice(b"a\xe9.csv text\n");
    attributes(&worktree, rules, "not-quite");
    let converted = worktree.join(std::ffi::OsStr::from_bytes(b"a\xe9.csv"));
    std::fs::write(&converted, b"a,b\r\n").unwrap();
    snapshot_into(&worktree, &fixture.source, ref_name);

    assert!(matches!(
        safe_to_delete_worktree_after_snapshot(&worktree, Some(&fixture.source), ref_name),
        Safety::Keep(KeepReason::NotInSnapshot(_))
    ));
    assert_eq!(
        std::fs::read(&converted).expect("the carriage returns are gone"),
        b"a,b\r\n"
    );
}

#[test]
fn commit_made_after_the_snapshot_keeps_the_worktree() {
    let fixture = Fixture::new("");
    let worktree = fixture.linked_worktree("committed-late");
    let ref_name = "refs/echoagent/subagents/committed-late";
    std::fs::write(worktree.join("tracked.txt"), "three hours of work\n").unwrap();
    snapshot_into(&worktree, &fixture.source, ref_name);
    run_git(&worktree, &["commit", "-am", "the message a human wrote"]);
    let committed = run_git(&worktree, &["rev-parse", "HEAD"]);

    assert_eq!(
        safe_to_delete_worktree_after_snapshot(&worktree, Some(&fixture.source), ref_name),
        Safety::Keep(KeepReason::Unpushed)
    );
    assert_eq!(
        run_git(&worktree, &["show", &format!("{committed}:tracked.txt")]),
        "three hours of work",
        "the commit the snapshot does not hold is still here"
    );
}

#[test]
fn hashes_line_up_only_when_hash_and_path_counts_match() {
    let three = b"a\nb\nc\n";

    assert_eq!(
        hashes_line_up(three, 3),
        Some(vec![&b"a"[..], &b"b"[..], &b"c"[..]])
    );
    assert_eq!(hashes_line_up(three, 4), None, "git answered one short");
    assert_eq!(hashes_line_up(three, 2), None, "git answered one too many");
    assert_eq!(
        hashes_line_up(b"a\nb\nc", 3),
        None,
        "a truncated read is not two thirds of an answer"
    );
}

#[test]
fn probe_that_cannot_run_keeps_a_snapshotted_worktree() {
    let fixture = Fixture::new("");
    let worktree = fixture.linked_worktree("unrunnable");
    let ref_name = "refs/echoagent/subagents/unrunnable";
    std::fs::write(worktree.join("notes.txt"), b"three hours of work\n").unwrap();
    snapshot_into(&worktree, &fixture.source, ref_name);
    run_git(&worktree, &["config", "status.renameLimit", "not-a-number"]);

    assert_eq!(
        safe_to_delete_worktree_after_snapshot(&worktree, Some(&fixture.source), ref_name),
        Safety::Keep(KeepReason::CheckFailed)
    );
    assert_eq!(
        std::fs::read(worktree.join("notes.txt")).expect("the work is gone"),
        b"three hours of work\n"
    );
}
