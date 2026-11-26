use super::*;
use echo_agent_test_utils::git::{git_init_seed, reflog_only_commit, run_git};
use std::path::{Path, PathBuf};

const LONG_AGO: &str = "2020-01-01T00:00:00Z";

struct Fixture {
    _root: tempfile::TempDir,
    source: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        git_init_seed(&source);
        std::fs::write(source.join("tracked.txt"), "one\n").unwrap();
        run_git(&source, &["add", "."]);
        run_git(&source, &["commit", "-m", "seed"]);
        std::fs::write(source.join("tracked.txt"), "two\n").unwrap();
        run_git(&source, &["commit", "-am", "second"]);
        Self {
            _root: root,
            source,
        }
    }

    fn worktree_with_a_discarded_commit(
        &self,
        name: &str,
        when: Option<&str>,
    ) -> (PathBuf, String) {
        let at = self.source.with_file_name(name);
        run_git(
            &self.source,
            &["worktree", "add", "--detach", at.to_str().unwrap(), "HEAD"],
        );
        let discarded = reflog_only_commit(&at, when);
        (at, discarded)
    }
}

#[test]
fn a_reclaimed_worktree_leaves_its_commits_readable() {
    let fixture = Fixture::new();
    let (worktree, discarded) = fixture.worktree_with_a_discarded_commit("reset-away", None);

    let Reclaim::Now { named } = reclaimable_within(&worktree, None, Duration::from_secs(60))
    else {
        panic!("the gate kept a worktree holding nothing but a named commit");
    };
    assert_eq!(named, 1);
    crate::remove_worktree(&worktree).unwrap();
    run_git(&fixture.source, &["gc", "--prune=now"]);

    assert_eq!(
        show_tracked(&fixture.source, &discarded),
        "three hours of work",
        "the prune took a commit nothing else names"
    );
}

#[test]
fn the_gate_alone_keeps_the_worktree_the_reclaim_layer_lets_go() {
    let fixture = Fixture::new();
    let (worktree, _) = fixture.worktree_with_a_discarded_commit("gate-only", None);

    assert!(matches!(
        crate::git::safe_to_delete_worktree(&worktree, None),
        Safety::Keep(KeepReason::Unpushed)
    ));
    assert!(worktree.is_dir());
}

#[test]
fn only_the_tip_of_a_discarded_chain_is_named() {
    let fixture = Fixture::new();
    let at = fixture.source.with_file_name("chain");
    run_git(
        &fixture.source,
        &["worktree", "add", "--detach", at.to_str().unwrap(), "HEAD"],
    );
    for step in ["first", "second", "third"] {
        std::fs::write(at.join("tracked.txt"), format!("{step}\n")).unwrap();
        run_git(&at, &["commit", "-am", step]);
    }
    let tip = run_git(&at, &["rev-parse", "HEAD"]);
    run_git(&at, &["reset", "--hard", "HEAD~3"]);

    let Reclaim::Now { named } = reclaimable_within(&at, None, Duration::from_secs(60)) else {
        panic!("the gate kept it");
    };

    assert_eq!(named, 1, "three commits, one chain, one name");
    let names = reclaimed_names(&fixture.source);
    let prefix = format!("{RECLAIMED}/chain/");
    let suffix = format!("/{tip}");
    assert!(
        names.starts_with(&prefix)
            && names.ends_with(&suffix)
            && names[prefix.len()..names.len() - suffix.len()]
                .parse::<i64>()
                .is_ok(),
        "name must be {RECLAIMED}/chain/<reclaim-unix-ts>/{tip}, got {names:?}"
    );
}

#[test]
fn a_name_whose_commit_something_else_holds_is_collected() {
    let fixture = Fixture::new();
    let (worktree, discarded) = fixture.worktree_with_a_discarded_commit("merged-since", None);
    expect_one_reclaimable_name(&worktree);
    crate::remove_worktree(&worktree).unwrap();

    assert_eq!(
        collect_reclaimed_names(&fixture.source, RECLAIMED_LIFETIME).unwrap(),
        0
    );

    run_git(&fixture.source, &["branch", "recovered", &discarded]);

    assert_eq!(
        collect_reclaimed_names(&fixture.source, RECLAIMED_LIFETIME).unwrap(),
        1
    );
    assert_eq!(reclaimed_names(&fixture.source), "");
    assert_eq!(
        show_tracked(&fixture.source, &discarded),
        "three hours of work",
        "collecting a name must not touch what still holds it"
    );
}

#[test]
fn a_name_older_than_its_lifetime_is_collected() {
    let fixture = Fixture::new();
    let (worktree, discarded) = fixture.worktree_with_a_discarded_commit("forgotten", None);
    expect_one_reclaimable_name(&worktree);
    crate::remove_worktree(&worktree).unwrap();

    let current = reclaimed_names(&fixture.source);
    let aged = format!("{RECLAIMED}/forgotten/1/{discarded}");
    run_git(&fixture.source, &["update-ref", &aged, &discarded]);
    run_git(&fixture.source, &["update-ref", "-d", &current]);

    assert_eq!(
        collect_reclaimed_names(&fixture.source, RECLAIMED_LIFETIME).unwrap(),
        1
    );

    assert_eq!(reclaimed_names(&fixture.source), "");
    run_git(&fixture.source, &["gc", "--prune=now"]);

    assert!(
        !holds_object(&fixture.source, &discarded),
        "the collector dropped the name and the prune took the commit, which is the deal"
    );
}

#[test]
fn a_just_reclaimed_name_survives_even_when_its_commit_is_old() {
    let fixture = Fixture::new();
    let (worktree, discarded) =
        fixture.worktree_with_a_discarded_commit("ancient-tip", Some(LONG_AGO));
    expect_one_reclaimable_name(&worktree);
    crate::remove_worktree(&worktree).unwrap();

    assert_eq!(
        collect_reclaimed_names(&fixture.source, RECLAIMED_LIFETIME).unwrap(),
        0,
        "a name written this pass must outlive the pass, whatever the commit date"
    );
    assert_eq!(
        show_tracked(&fixture.source, &discarded),
        "three hours of work"
    );
    let names = reclaimed_names(&fixture.source);
    assert!(
        names.contains(&discarded),
        "the name must still be present: {names}"
    );
}

fn reclaimed_names(repo: &Path) -> String {
    run_git(repo, &["for-each-ref", "--format=%(refname)", RECLAIMED])
}

fn expect_one_reclaimable_name(worktree: &Path) {
    assert!(matches!(
        reclaimable_within(worktree, None, Duration::from_secs(60)),
        Reclaim::Now { named: 1 }
    ));
}

fn show_tracked(repo: &Path, id: &str) -> String {
    run_git(repo, &["show", &format!("{id}:tracked.txt")])
}

fn holds_object(repo: &Path, id: &str) -> bool {
    let mut command = crate::git::checkout::git_command();
    command.current_dir(repo);
    command.args(["cat-file", "-e", id]);
    command.status().expect("git did not run").success()
}
