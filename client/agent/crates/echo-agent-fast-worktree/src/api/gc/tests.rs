use super::*;

// `expire_now`/`expire_now_forced` live in `crate::test_support` so the
// GC integration tests (`gc/integration_tests.rs`) can share them.
use crate::test_support::{expire_now, expire_now_forced};

#[test]
fn a_candidate_the_pass_has_no_time_for_is_kept_and_named() {
    let fx = crate::db::EchoAgentHomeFixture::new();
    let db = WorktreeDb::open(&fx.home).unwrap();
    let path = expiring_worktree(&db, &fx, "no-time");

    let report = run_pass(
        &db,
        &expire_now_forced(),
        Pass {
            gate_timeout: Duration::from_nanos(1),
            ..Default::default()
        },
        None,
        None,
    )
    .unwrap();

    assert_eq!(report.kept_reasons.get("gate-timed-out"), Some(&1));
    assert_eq!(report.expired_removed, 0);
    assert!(path.is_dir(), "a worktree nobody judged is still there");
}

#[test]
fn a_worktree_entered_during_the_gate_is_kept() {
    let fx = crate::db::EchoAgentHomeFixture::new();
    let db = WorktreeDb::open(&fx.home).unwrap();
    let path = expiring_worktree(&db, &fx, "entered-late");

    let report = run_pass(
        &db,
        &expire_now(),
        Pass::default(),
        None,
        Some(&enter_worktree(&db, Entered::AfterGate)),
    )
    .unwrap();

    assert_eq!(report.skipped_alive, 1);
    assert_eq!(report.expired_removed, 0);
    assert!(path.is_dir(), "somebody is standing in it");
}

/// Work that appears in the worktree after the first verdict but before the
/// removal must not be lost: the pass re-judges the tree immediately before the
/// delete, so a file written (or a commit made) during the gate window flips
/// the verdict back to Keep. Liveness alone cannot see this — only the re-gate.
#[test]
fn work_appearing_during_the_gate_window_is_not_deleted() {
    let fx = crate::db::EchoAgentHomeFixture::new();
    let db = WorktreeDb::open(&fx.home).unwrap();
    let path = expiring_worktree(&db, &fx, "committed-late");

    let dirtied = path.clone();
    let dirty_mid_pass = move |at: Entered| {
        if at == Entered::AfterGate {
            std::fs::write(dirtied.join("late-work.txt"), b"unsaved work").unwrap();
        }
    };
    let report = run_pass(
        &db,
        &expire_now(),
        Pass::default(),
        None,
        Some(&dirty_mid_pass),
    )
    .unwrap();

    assert_eq!(
        report.expired_removed, 0,
        "work that appeared mid-pass must not be deleted"
    );
    assert_eq!(report.kept_unsafe, 1);
    assert!(
        path.join("late-work.txt").exists(),
        "the late work survived"
    );
}

#[test]
fn a_record_the_first_re_check_rejects_is_never_put_through_the_gate() {
    let fx = crate::db::EchoAgentHomeFixture::new();
    let db = WorktreeDb::open(&fx.home).unwrap();
    let path = expiring_worktree(&db, &fx, "entered-early");

    let report = run_pass(
        &db,
        &expire_now(),
        Pass {
            gate_timeout: Duration::from_nanos(1),
            ..Default::default()
        },
        None,
        Some(&enter_worktree(&db, Entered::BeforeGate)),
    )
    .unwrap();

    assert_eq!(report.skipped_alive, 1);
    assert!(
        report.kept_reasons.is_empty(),
        "the gate ran for a record already rejected: {:?}",
        report.kept_reasons
    );
    assert!(path.is_dir());
}

fn enter_worktree(db: &WorktreeDb, when: Entered) -> impl Fn(Entered) + '_ {
    move |at| {
        if at != when {
            return;
        }
        for rec in db.list(&ListFilter::default()).unwrap() {
            let mut live = rec.clone();
            live.creator_pid = Some(std::process::id());
            db.register(&live).unwrap();
        }
    }
}

fn expiring_worktree(db: &WorktreeDb, fx: &crate::db::EchoAgentHomeFixture, name: &str) -> PathBuf {
    let path = crate::test_support::deletable_linked_worktree(&fx.home, name);
    db.register(&session_record(
        &format!("{name}-1"),
        path.clone(),
        fx.home.join("gate-source"),
        1,
        None,
    ))
    .unwrap();
    path
}

#[test]
fn a_dry_run_does_not_move_where_the_next_pass_starts() {
    let fx = crate::db::EchoAgentHomeFixture::new();
    let db = WorktreeDb::open(&fx.home).unwrap();
    let path = crate::test_support::deletable_linked_worktree(&fx.home, "previewed");
    db.register(&session_record(
        "previewed-1",
        path,
        fx.home.join("gate-source"),
        1,
        None,
    ))
    .unwrap();

    let _ = gc_worktrees(
        &db,
        &GcOptions {
            dry_run: true,
            ..expire_now_forced()
        },
    )
    .unwrap();

    assert_eq!(db.get_meta(META_LAST_AGE_CURSOR).unwrap(), None);
}

#[test]
fn a_pass_that_runs_out_of_time_resumes_where_it_stopped() {
    let fx = crate::db::EchoAgentHomeFixture::new();
    let db = WorktreeDb::open(&fx.home).unwrap();
    for id in ["older", "newer"] {
        std::fs::create_dir_all(fx.home.join(id)).unwrap();
        let at = if id == "older" { 1 } else { 2 };
        db.register(&session_record(id, fx.home.join(id), "/repo", at, Some(at)))
            .unwrap();
    }
    let no_time = expire_now_forced();
    let no_budget = Pass {
        budget: Duration::ZERO,
        ..Default::default()
    };

    let report = run_pass(&db, &no_time, no_budget, None, None).unwrap();

    assert_eq!(report.not_judged, 2, "no time to judge either of them");
    assert_eq!(
        db.get_meta(META_LAST_AGE_CURSOR).unwrap().as_deref(),
        Some("older"),
        "the row it stopped at, which is the oldest on a first pass"
    );
}

#[test]
fn a_pass_starts_at_the_row_the_last_one_stopped_at() {
    let ids = ["a", "b", "c"];

    assert_eq!(resume_at(&ids, None), 0, "no pass has run yet");
    assert_eq!(resume_at(&ids, Some("")), 0, "the last pass finished");
    assert_eq!(resume_at(&ids, Some("a")), 0);
    assert_eq!(resume_at(&ids, Some("b")), 1);
    assert_eq!(resume_at(&ids, Some("c")), 2);
    assert_eq!(resume_at(&ids, Some("gone")), 0, "the row was removed");
    assert_eq!(resume_at(&[], Some("a")), 0, "nothing to start from");
}

fn session_record(
    id: &str,
    path: impl Into<PathBuf>,
    source_repo: impl Into<PathBuf>,
    created_at: i64,
    last_accessed_at: Option<i64>,
) -> crate::db::WorktreeRecord {
    crate::db::WorktreeRecord {
        source_repo: source_repo.into(),
        created_at,
        last_accessed_at,
        ..crate::test_support::worktree_record(id, path)
    }
}

fn rec_at(path: &str, created_at: i64) -> crate::db::WorktreeRecord {
    session_record("r", path, "/repo", created_at, None)
}

#[test]
fn classify_covers_expiry_guards_and_kind_ttls() {
    let now = 1_000;
    let base = expire_now;

    let fresh = {
        let mut rec = rec_at("/no/such/wt", 1);
        rec.last_accessed_at = Some(now + 10);
        rec
    };
    let live_creator = {
        let mut rec = rec_at("/no/such/wt", 1);
        rec.creator_pid = Some(std::process::id());
        rec
    };
    let aged = {
        let mut rec = rec_at("/no/such/wt", 1);
        rec.last_accessed_at = Some(now - 100);
        rec
    };

    // Non-Unix PID probe is fail-closed (never alive), so a live pid can't guard there.
    let live_pid_verdict = if cfg!(unix) {
        Eligibility::Guarded
    } else {
        Eligibility::Reclaimable
    };

    let cases: Vec<(
        &str,
        crate::db::WorktreeRecord,
        Vec<PathBuf>,
        GcOptions,
        Eligibility,
    )> = vec![
        (
            "expired and unguarded",
            rec_at("/no/such/wt", 1),
            vec![],
            base(),
            Eligibility::Reclaimable,
        ),
        (
            "not yet expired",
            fresh,
            vec![],
            base(),
            Eligibility::NotYetExpired,
        ),
        (
            "live creator_pid guards it",
            live_creator,
            vec![],
            base(),
            live_pid_verdict,
        ),
        (
            "a live cwd within guards it",
            rec_at("/no/such/wt", 1),
            vec![PathBuf::from("/no/such/wt/sub")],
            base(),
            Eligibility::Guarded,
        ),
        (
            "an in-use path inside it keeps it",
            rec_at("/no/such/wt", 1),
            vec![],
            GcOptions {
                keep_worktrees_containing: vec![PathBuf::from("/no/such/wt/nested")],
                ..expire_now()
            },
            Eligibility::Guarded,
        ),
        (
            "grove dest cwd inside dest must guard without canonicalize",
            {
                let mut rec = rec_at("/tmp/nfs-wt", 1);
                rec.creation_mode = "grove-fuse".into();
                rec
            },
            vec![PathBuf::from("/tmp/nfs-wt/sub")],
            expire_now(),
            Eligibility::Guarded,
        ),
        #[cfg(target_os = "macos")]
        (
            "grove-fuse dest cwd via /tmp↔/private/tmp must guard without canonicalize",
            {
                let mut rec = rec_at("/tmp/nfs-wt", 1);
                rec.creation_mode = "grove-fuse".into();
                rec
            },
            vec![PathBuf::from("/private/tmp/nfs-wt/sub")],
            expire_now(),
            Eligibility::Guarded,
        ),
        #[cfg(target_os = "macos")]
        (
            "grove-nfs keep_worktrees_containing must not canonicalize the dest",
            {
                let mut rec = rec_at("/tmp/nfs-wt", 1);
                rec.creation_mode = "grove-nfs".into();
                rec
            },
            vec![],
            GcOptions {
                keep_worktrees_containing: vec![PathBuf::from("/private/tmp/nfs-wt/sub")],
                ..expire_now()
            },
            Eligibility::Guarded,
        ),
        (
            "never-expire kind",
            rec_at("/no/such/wt", 1),
            vec![],
            GcOptions {
                max_age_by_kind: [(WorktreeKind::Session, None)].into_iter().collect(),
                ..expire_now()
            },
            Eligibility::NeverExpires,
        ),
        (
            "kind ttl shorter than age expires",
            aged.clone(),
            vec![],
            GcOptions {
                max_age_secs: Some(10_000),
                max_age_by_kind: [(WorktreeKind::Session, Some(50))].into_iter().collect(),
                ..Default::default()
            },
            Eligibility::Reclaimable,
        ),
        (
            "kind ttl longer than age keeps",
            aged,
            vec![],
            GcOptions {
                max_age_secs: Some(10),
                max_age_by_kind: [(WorktreeKind::Session, Some(200))].into_iter().collect(),
                ..Default::default()
            },
            Eligibility::NotYetExpired,
        ),
    ];

    for (why, rec, live_cwds, opts, want) in cases {
        assert_eq!(classify(&rec, now, &live_cwds, &opts), want, "{why}");
    }
}

#[test]
fn effective_max_age_precedence() {
    let opts = GcOptions {
        max_age_secs: Some(100),
        max_age_by_kind: [
            (WorktreeKind::Subagent, Some(10)),
            (WorktreeKind::Pool, None),
            (WorktreeKind::Manual, None),
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    assert_eq!(effective_max_age(&opts, WorktreeKind::Session), Some(100));
    assert_eq!(effective_max_age(&opts, WorktreeKind::Subagent), Some(10));
    assert_eq!(effective_max_age(&opts, WorktreeKind::Pool), None);
    assert_eq!(
        effective_max_age(&opts, WorktreeKind::Manual),
        None,
        "None in max_age_by_kind means never-expire"
    );
}

#[test]
fn run_pass_prunes_orphan_grove_pins_after_grace() {
    echo_agent_test_utils::require_git!();
    use echo_agent_test_utils::git::{git_commit_all, init_git_repo};

    let mut fx = crate::db::EchoAgentHomeFixture::new();
    let grove = fx.isolate_xdg_grove_data();
    let repo = fx.home.join("src-repo");
    std::fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);
    std::fs::write(repo.join("f.txt"), "x").unwrap();
    git_commit_all(&repo, "c");
    let oid = {
        let mut cmd = std::process::Command::new("git");
        echo_agent_tty_utils::detach_std_command(&mut cmd);
        let out = cmd
            .current_dir(&repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    let pin = "refs/echoagent/worktrees/wt-orphan";
    let mut uref = std::process::Command::new("git");
    echo_agent_tty_utils::detach_std_command(&mut uref);
    assert!(
        uref.current_dir(&repo)
            .args(["update-ref", pin, &oid])
            .status()
            .unwrap()
            .success()
    );

    let conn = rusqlite::Connection::open(grove.join("daemon.db")).unwrap();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS wt_create_state (
            worktree_id TEXT PRIMARY KEY,
            phase TEXT NOT NULL,
            dest TEXT NOT NULL,
            source TEXT NOT NULL,
            orphan_seen_at INTEGER,
            updated_at INTEGER NOT NULL
        );",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO wt_create_state(worktree_id, phase, dest, source, updated_at)
         VALUES ('wt-orphan', 'aborted', '/gone', ?1, 1)",
        rusqlite::params![repo.display().to_string()],
    )
    .unwrap();
    std::fs::write(
        grove.join("pin_gc_orphans.json"),
        serde_json::json!({
            "orphans": {
                "wt-orphan": {
                    "first_seen": 1,
                    "cycles": 1,
                    "source": repo,
                    "pin_ref": pin,
                }
            }
        })
        .to_string(),
    )
    .unwrap();

    let db = WorktreeDb::open(&fx.home).unwrap();
    let report = run_pass(&db, &GcOptions::default(), Pass::default(), None, None).unwrap();
    assert!(
        report.pin_gc_examined >= 1,
        "production GC must invoke pin sweep: {report:?}"
    );
    assert_eq!(report.pin_gc_pruned, 1, "{report:?}");
    let mut show = std::process::Command::new("git");
    echo_agent_tty_utils::detach_std_command(&mut show);
    let shown = show
        .current_dir(&repo)
        .args(["show-ref", "--verify", pin])
        .status()
        .unwrap();
    assert!(!shown.success(), "aged orphan pin must be deleted");
}
