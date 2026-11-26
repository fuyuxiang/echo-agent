use super::*;

const CHILD_WORKTREE: &str = "ECHO_AGENT_FAST_WORKTREE_SAFETY_CHILD_WORKTREE";
const CHILD_SOURCE: &str = "ECHO_AGENT_FAST_WORKTREE_SAFETY_CHILD_SOURCE";
const CHILD_TEST: &str = "git::safety::tests::gate::verdict_under_a_foreign_git_dir";
const CHILD_SNAPSHOT_TEST: &str = "git::safety::tests::gate::snapshot_under_a_foreign_clean_filter";

fn run_child(test: &str, worktree: &Path, source: &Path, envs: &[(&str, &std::ffi::OsStr)]) {
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child
        .args([test, "--exact", "--ignored", "--test-threads=1"])
        .env(CHILD_WORKTREE, worktree)
        .env(CHILD_SOURCE, source);
    for (key, value) in envs {
        child.env(key, value);
    }
    let out = child.output().expect("the test binary re-runs itself");
    let report = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && report.contains("1 passed"),
        "{report}{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn git_dir_in_the_environment_does_not_change_the_verdict() {
    let fixture = Fixture::new("");
    let worktree = fixture.linked_worktree("inherits-a-git-dir");
    std::fs::write(worktree.join("tracked.txt"), "two\n").unwrap();
    run_git(&worktree, &["commit", "-am", "work no remote holds"]);
    let decoy = fixture.source.with_file_name("decoy");
    run_git(
        fixture.source.parent().unwrap(),
        &[
            "clone",
            fixture.remote.to_str().unwrap(),
            decoy.to_str().unwrap(),
        ],
    );

    run_child(
        CHILD_TEST,
        &worktree,
        &fixture.source,
        &[("GIT_DIR", decoy.join(".git").as_os_str())],
    );

    assert_eq!(
        std::fs::read(worktree.join("tracked.txt")).expect("the commit's bytes are gone"),
        b"two\n"
    );
}

#[test]
fn git_configuration_in_the_environment_does_not_reach_the_snapshot() {
    let fixture = Fixture::new("");
    let worktree = fixture.linked_worktree("inherits-a-filter");
    attributes(&worktree, "*.txt filter=conv\n", "inherits-a-filter");
    std::fs::write(worktree.join("notes.txt"), "the only copy\n").unwrap();
    let foreign = fixture.source.with_file_name("foreign.gitconfig");
    std::fs::write(
        &foreign,
        "[filter \"conv\"]\n\tclean = sed s/only/nothing/\n",
    )
    .unwrap();

    run_child(
        CHILD_SNAPSHOT_TEST,
        &worktree,
        &fixture.source,
        &[("GIT_CONFIG_GLOBAL", foreign.as_os_str())],
    );
}

#[test]
fn no_combination_of_working_tree_shapes_loses_uncarried_work() {
    struct Shape {
        name: &'static str,
        write: fn(&Path),
        carried: bool,
    }

    let shapes = [
        Shape {
            name: "plain",
            write: |at| write_at(at, "notes.txt", b"ordinary work\n"),
            carried: true,
        },
        Shape {
            name: "ignored",
            write: |at| {
                write_at(at, ".gitignore", b".env\n");
                write_at(at, ".env", b"SECRET=1\n");
            },
            carried: false,
        },
        Shape {
            name: "converted",
            write: |at| {
                write_at(at, ".gitattributes", b"*.csv text\n");
                write_at(at, "export.csv", b"a,b\r\n");
            },
            carried: false,
        },
        Shape {
            name: "dot-git",
            write: |at| write_at(at, "testdata/.git/fixture.txt", b"the only copy\n"),
            carried: false,
        },
    ];

    for subset in 0..(1u8 << shapes.len()) {
        let chosen: Vec<_> = shapes
            .iter()
            .enumerate()
            .filter(|(at, _)| subset & (1 << at) != 0)
            .map(|(_, shape)| shape)
            .collect();
        let name = format!("subset-{subset}");
        let fixture = Fixture::new("");
        let worktree = fixture.linked_worktree(&name);
        for shape in &chosen {
            (shape.write)(&worktree);
        }
        let carried = chosen.iter().all(|shape| shape.carried);
        let ref_name = format!("refs/echoagent/subagents/{name}");

        let safety = reclaim_after_snapshot(&worktree, &fixture.source, &ref_name);

        let held: Vec<&str> = chosen.iter().map(|shape| shape.name).collect();
        assert_eq!(
            safety == Safety::Delete,
            carried,
            "{held:?} answered {safety:?}"
        );
        if carried {
            assert!(!worktree.exists(), "{held:?} was not removed");
            if subset != 0 {
                assert_eq!(
                    run_git(&fixture.source, &["show", &format!("{ref_name}:notes.txt")]),
                    "ordinary work"
                );
            }
        } else {
            assert!(worktree.exists(), "{held:?} was removed");
        }
    }
}

fn write_at(worktree: &Path, path: &str, bytes: &[u8]) {
    let at = worktree.join(path);
    std::fs::create_dir_all(at.parent().unwrap()).unwrap();
    std::fs::write(at, bytes).unwrap();
}

#[test]
fn reclaiming_a_worktree_runs_none_of_its_hooks() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new("");
    let worktree = fixture.linked_worktree("has-hooks");
    std::fs::write(worktree.join("tracked.txt"), "work\n").unwrap();
    std::fs::write(worktree.join("untracked.rs"), b"more work").unwrap();

    let ran = fixture.source.with_file_name("the-hook-ran");
    let hooks = fixture.source.join(".git/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    for hook in ["reference-transaction", "post-index-change", "pre-commit"] {
        let at = hooks.join(hook);
        std::fs::write(&at, format!("#!/bin/sh\ntouch {}\n", ran.display())).unwrap();
        std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    snapshot_into(
        &worktree,
        &fixture.source,
        "refs/echoagent/subagents/has-hooks",
    );

    assert!(
        !ran.exists(),
        "the snapshot ran a hook the repository ships"
    );
}

#[test]
fn gate_that_panics_keeps_the_worktree() {
    let answer = answer_within(Path::new("/nonexistent"), Duration::from_secs(30), || {
        panic!("the gate fell over")
    });

    assert_eq!(answer, Safety::Keep(KeepReason::CheckFailed));
}

#[test]
fn gate_that_does_not_answer_keeps_the_worktree() {
    let answer = answer_within(Path::new("/nonexistent"), Duration::from_millis(50), || {
        std::thread::sleep(Duration::from_secs(30));
        Safety::Delete
    });

    assert_eq!(
        answer,
        Safety::Keep(KeepReason::GateTimedOut),
        "a hang is reported apart from a check that failed cheaply: only this one \
         leaves a thread behind, and a soak has to be able to count it"
    );
}

#[test]
#[ignore = "run by git_dir_in_the_environment_does_not_change_the_verdict"]
fn verdict_under_a_foreign_git_dir() {
    let worktree = std::env::var_os(CHILD_WORKTREE)
        .expect("the parent sets CHILD_WORKTREE; without it this test proves nothing");
    assert!(
        std::env::var_os("GIT_DIR").is_some(),
        "the parent must set the variable this test is about"
    );
    assert_eq!(
        reclaim(Path::new(&worktree)),
        Safety::Keep(KeepReason::Unpushed)
    );
}

#[test]
#[ignore = "run by git_configuration_in_the_environment_does_not_reach_the_snapshot"]
fn snapshot_under_a_foreign_clean_filter() {
    let worktree = PathBuf::from(
        std::env::var_os(CHILD_WORKTREE)
            .expect("the parent sets CHILD_WORKTREE; without it this test proves nothing"),
    );
    let source =
        PathBuf::from(std::env::var_os(CHILD_SOURCE).expect("the parent sets CHILD_SOURCE"));
    assert!(
        std::env::var_os("GIT_CONFIG_GLOBAL").is_some(),
        "the parent must set the variable this test is about"
    );
    let ref_name = "refs/echoagent/subagents/inherits-a-filter";
    snapshot_into(&worktree, &source, ref_name);

    assert_eq!(
        run_git(&source, &["show", &format!("{ref_name}:notes.txt")]),
        "the only copy",
        "the snapshot stored what a foreign filter rewrote, so the file's bytes are in no tree"
    );
    assert_eq!(
        safe_to_delete_worktree_after_snapshot(&worktree, Some(&source), ref_name),
        Safety::Delete
    );
}
