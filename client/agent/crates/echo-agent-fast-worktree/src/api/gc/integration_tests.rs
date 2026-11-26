use super::*;
use crate::db::{ListFilter, WorktreeDb, WorktreeKind};
use crate::test_support::{
    deletable_linked_worktree, deletable_standalone_worktree, dry_run_opts, expire_now,
    expire_now_forced,
};

fn db_at(tmp: &tempfile::TempDir) -> WorktreeDb {
    WorktreeDb::open(tmp.path()).unwrap()
}

/// Register a worktree of `kind` at `path` off the canonical fixture
/// (`created_at: 1`, so it is expired under any positive TTL). Kills the
/// repeated field-by-field `WorktreeRecord` register blocks.
fn register_kind(db: &WorktreeDb, id: &str, path: &std::path::Path, kind: WorktreeKind) {
    db.register(&crate::db::WorktreeRecord {
        kind,
        ..crate::test_support::worktree_record(id, path)
    })
    .unwrap();
}

#[test]
fn register_worktree_writes_correct_fields() {
    // Isolate ECHO_AGENT_HOME so register_worktree's open_default write lands
    // in our own DB (lock + private tmp + restore via the fixture).
    let fx = crate::db::EchoAgentHomeFixture::new();

    // Unique basename → unique id, so a concurrent open_default writer
    // (ECHO_AGENT_HOME is process-global) can't INSERT-OR-REPLACE our row.
    let wt_path = fx.home.join("register-fields-wt");
    std::fs::create_dir(&wt_path).unwrap();
    // register_worktree stores the canonical path (/var → /private/var on macOS).
    let wt_canon = dunce::canonicalize(&wt_path).unwrap_or_else(|_| wt_path.clone());

    super::register_worktree(
        &wt_path,
        std::path::Path::new("/src/repo"),
        WorktreeKind::Session,
        "linked",
        "main",
        "abc123",
        Some("test-session".to_string()),
        None,
        None,
    );

    // register_worktree wrote to open_default, which resolves to fx.home.
    // Filter to OUR record by path: concurrent tests may add rows here.
    let db = WorktreeDb::open(&fx.home).unwrap();
    let mine: Vec<_> = db
        .list(&ListFilter::default())
        .unwrap()
        .into_iter()
        .filter(|r| r.path == wt_canon)
        .collect();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].kind, WorktreeKind::Session);
    assert_eq!(mine[0].session_id.as_deref(), Some("test-session"));
    assert_eq!(mine[0].creation_mode, "linked");
    assert_eq!(mine[0].head_commit.as_deref(), Some("abc123"));
    assert!(mine[0].creator_pid.is_some());
}

#[test]
fn unregister_worktree_removes_by_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db = db_at(&tmp);
    let wt_path = tmp.path().join("wt");

    let record = crate::db::WorktreeRecord {
        created_at: 100,
        ..crate::test_support::worktree_record("test-wt", wt_path.clone())
    };
    db.register(&record).unwrap();
    assert_eq!(db.list(&ListFilter::default()).unwrap().len(), 1);

    db.unregister_by_path(&wt_path).unwrap();
    assert!(db.list(&ListFilter::default()).unwrap().is_empty());
}

#[test]
fn gc_removes_dead_records() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db = db_at(&tmp);

    let record = crate::db::WorktreeRecord {
        created_at: 100,
        ..crate::test_support::worktree_record("dead-1", "/nonexistent/worktree")
    };
    db.register(&record).unwrap();

    let report = gc::gc_worktrees(&db, &gc::GcOptions::default()).unwrap();
    assert_eq!(report.dead_removed, 1);

    let all = db
        .list(&ListFilter {
            include_dead: true,
            ..Default::default()
        })
        .unwrap();
    assert!(all.is_empty());
}

#[test]
fn gc_skips_alive_pids() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db = db_at(&tmp);
    let my_pid = std::process::id();

    let record = crate::db::WorktreeRecord {
        creator_pid: Some(my_pid),
        ..crate::test_support::worktree_record("alive-wt", "/nonexistent/path")
    };
    db.register(&record).unwrap();

    // A missing path is swept to dead; use a real dir to exercise the
    // liveness skip on an alive record.
    let dir = deletable_linked_worktree(tmp.path(), "real-wt");
    let mut record2 = record.clone();
    record2.id = "alive-wt2".to_string();
    record2.path = dir.clone();
    db.register(&record2).unwrap();

    let report = gc::gc_worktrees(&db, &expire_now()).unwrap();

    // Our PID is alive, so the real-path worktree should be skipped
    assert_eq!(report.skipped_alive, 1);
    // The nonexistent-path one gets swept to dead then removed
    assert_eq!(report.dead_removed, 1);
}

#[test]
fn gc_dry_run_preserves_records() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db = db_at(&tmp);

    let record = crate::db::WorktreeRecord {
        created_at: 100,
        ..crate::test_support::worktree_record("dry-1", "/nonexistent")
    };
    db.register(&record).unwrap();

    let report = gc::gc_worktrees(
        &db,
        &gc::GcOptions {
            dry_run: true,
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(report.dead_removed, 1); // counted as would-be-removed
    // Dry run must NOT mutate: the record is still present AND still
    // Alive (it was never swept to Dead).
    let all = db
        .list(&ListFilter {
            include_dead: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].status, crate::db::WorktreeStatus::Alive);
}

#[test]
fn gc_force_overrides_liveness() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db = db_at(&tmp);

    let dir = deletable_linked_worktree(tmp.path(), "force-wt");

    let record = crate::db::WorktreeRecord {
        creator_pid: Some(std::process::id()), // our own PID
        ..crate::test_support::worktree_record("force-1", dir.clone())
    };
    db.register(&record).unwrap();

    let report = gc::gc_worktrees(&db, &expire_now_forced()).unwrap();

    assert_eq!(report.expired_removed, 1);
    assert_eq!(report.skipped_alive, 0);
}

#[test]
fn gc_clamps_extreme_max_age_without_overflow() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db = db_at(&tmp);
    let dir = deletable_linked_worktree(tmp.path(), "fresh-wt");
    let record = crate::db::WorktreeRecord {
        created_at: i64::MAX,
        ..crate::test_support::worktree_record("fresh-1", dir.clone())
    };
    db.register(&record).unwrap();
    // `now - i64::MIN` would overflow/wrap the cutoff into the future and
    // reclaim everything; the clamp treats any negative age as 0 so the
    // cutoff is `now` and nothing fresh is reclaimed (and no panic).
    let report = gc::gc_worktrees(
        &db,
        &gc::GcOptions {
            max_age_secs: Some(i64::MIN),
            force: false,
            dry_run: false,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(report.expired_removed, 0);
    assert!(dir.exists());
}

#[test]
fn gc_honors_last_accessed_time() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db = db_at(&tmp);
    let fresh = deletable_linked_worktree(tmp.path(), "fresh-access");
    let stale = deletable_linked_worktree(tmp.path(), "stale-access");
    // creator_pid None ⇒ no liveness guard: isolate the age logic;
    // created_at 1 (helper default) ⇒ both are old by creation time.
    let base = crate::test_support::worktree_record("", std::path::PathBuf::new());
    db.register(&crate::db::WorktreeRecord {
        id: "fresh".to_string(),
        path: fresh.clone(),
        last_accessed_at: Some(i64::MAX), // touched within the window
        ..base.clone()
    })
    .unwrap();
    db.register(&crate::db::WorktreeRecord {
        id: "stale".to_string(),
        path: stale.clone(),
        last_accessed_at: Some(1), // never re-touched
        ..base
    })
    .unwrap();

    let report = gc::gc_worktrees(&db, &expire_now()).unwrap();

    assert!(
        fresh.exists(),
        "a recently accessed worktree must survive despite an old created_at"
    );
    assert!(
        !stale.exists(),
        "a never-touched expired worktree must be reclaimed"
    );
    assert_eq!(report.expired_removed, 1);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn scan_has_cwd_under(prefix: &std::path::Path) -> bool {
    match gc::live_process_cwds() {
        gc::LiveCwdScan::Ok(cwds) => cwds.iter().any(|p| {
            p.starts_with(prefix) || dunce::canonicalize(p).is_ok_and(|c| c.starts_with(prefix))
        }),
        _ => false,
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn wait_until(pred: impl Fn() -> bool) -> bool {
    use std::time::Duration;
    for _ in 0..200 {
        if pred() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn gc_cwd_guard_skips_then_reclaims_expired_worktree() {
    let _cwd_lock = crate::api::cwd_test_guard();
    let tmp = tempfile::TempDir::new().unwrap();
    let db = db_at(&tmp);
    let dir = deletable_linked_worktree(tmp.path(), "cwd-wt");
    let nested = dir.join("nested");
    let record = crate::test_support::worktree_record("cwd-1", dir.clone());
    db.register(&record).unwrap();

    #[allow(clippy::disallowed_methods)] // test fixture; the test kills it
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .current_dir(&nested)
        .spawn()
        .expect("spawn sleep");
    let want = dunce::canonicalize(&nested).unwrap();
    assert!(
        wait_until(|| scan_has_cwd_under(&want)),
        "live_process_cwds must observe the parked child before GC"
    );

    let opts = expire_now();
    let guarded = gc::gc_worktrees(&db, &opts).unwrap();
    assert_eq!(
        guarded.skipped_alive, 1,
        "a live in-tree CWD must protect the expired worktree"
    );
    assert_eq!(guarded.expired_removed, 0);
    assert!(dir.exists());

    // Once the process exits, the same expired worktree is reclaimed.
    child.kill().ok();
    child.wait().ok();
    assert!(
        wait_until(|| !scan_has_cwd_under(&want)),
        "child CWD must leave the scan after exit before reclaim"
    );
    let reclaimed = gc::gc_worktrees(&db, &opts).unwrap();
    assert_eq!(
        reclaimed.expired_removed, 1,
        "no live process inside ⇒ the expired worktree is reclaimed"
    );
    assert!(!dir.exists());
}

#[test]
fn gc_dry_run_with_max_age_does_not_remove_expired() {
    // An expired worktree whose dir exists must be previewed (counted)
    // but never removed under dry_run.
    let tmp = tempfile::TempDir::new().unwrap();
    let db = db_at(&tmp);

    let dir = deletable_linked_worktree(tmp.path(), "expired-wt");

    let record = crate::test_support::worktree_record("expired-1", dir.clone());
    db.register(&record).unwrap();

    let report = gc::gc_worktrees(
        &db,
        &gc::GcOptions {
            dry_run: true,
            ..expire_now_forced()
        },
    )
    .unwrap();

    assert_eq!(
        report.expired_removed, 1,
        "dry run should count the candidate"
    );
    // No mutation: the dir and the (still Alive) record both survive.
    assert!(dir.exists(), "dry run must not remove the worktree dir");
    let all = db.list(&ListFilter::default()).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].status, crate::db::WorktreeStatus::Alive);
}

#[test]
fn gc_dry_run_missing_and_expired_counted_once() {
    // A record that is Alive, has a MISSING path, AND is expired must be
    // counted EXACTLY once (a real run sweeps it to dead and unregisters
    // it before the expired loop). It belongs to dead_removed, not both.
    let tmp = tempfile::TempDir::new().unwrap();
    let db = db_at(&tmp);

    let record = crate::test_support::worktree_record("missing-expired", "/nonexistent/expired-wt");
    db.register(&record).unwrap();

    let report = gc::gc_worktrees(
        &db,
        &gc::GcOptions {
            dry_run: true,
            ..expire_now_forced()
        },
    )
    .unwrap();

    assert_eq!(
        report.dead_removed, 1,
        "missing path counts as would-be-dead"
    );
    assert_eq!(
        report.expired_removed, 0,
        "must not also be counted in expired_removed"
    );
}

/// A path that is no repository and will not remove either. Retrying it
/// every pass only grows the candidate set, so the record goes and the
/// bytes stay where they are, which is the one outcome that leaves both
/// halves behind.
#[test]
fn expired_path_that_will_not_remove_loses_its_record_and_keeps_its_bytes() {
    let fx = crate::db::EchoAgentHomeFixture::new();
    let db = WorktreeDb::open(&fx.home).unwrap();
    // A file, so the directory removal fails with ENOTDIR whoever runs
    // this, root included.
    let path = fx.home.join("not-a-directory");
    std::fs::write(&path, b"bytes nobody asked about").unwrap();

    let record = crate::test_support::worktree_record("stuck-1", path.clone());
    db.register(&record).unwrap();

    let report = gc::gc_worktrees(&db, &expire_now_forced()).unwrap();

    assert_eq!(report.no_repo_paths, 1);
    assert_eq!(report.expired_removed, 0);
    assert_eq!(
        report.remove_failed, 0,
        "the record was dropped, not failed"
    );
    assert!(!record_present(&db, &path), "the record is not retried");
    assert_eq!(
        std::fs::read(&path).expect("the bytes were taken after all"),
        b"bytes nobody asked about"
    );
}

#[test]
fn gc_removes_an_expired_path_with_no_repo_and_counts_it_apart() {
    // An overlay mount or btrfs snapshot needs no `.git`, and only
    // `remove_worktree` reclaims either. ECHO_AGENT_HOME is the gc DB dir so
    // its unregister hits the DB holding the record.
    let fx = crate::db::EchoAgentHomeFixture::new();
    let db = WorktreeDb::open(&fx.home).unwrap();

    // What an unmounted overlay leaves: directories, no files.
    let path = fx.home.join("no-repo-wt");
    std::fs::create_dir_all(path.join("subdir")).unwrap();

    let record = crate::test_support::worktree_record("no-repo-1", path.clone());
    db.register(&record).unwrap();

    let report = gc::gc_worktrees(&db, &expire_now_forced()).unwrap();

    assert_eq!(report.no_repo_paths, 1);
    assert_eq!(
        report.expired_removed, 0,
        "a path that is not a repository is not a worktree removed"
    );
    assert_eq!(report.kept_unsafe, 0);
    assert_eq!(report.skipped_alive, 0);
    assert!(!path.exists());
    assert!(!record_present(&db, &path));
}

/// A dangling worktree symlink (its target gone) reads as absent to `exists()`,
/// which follows the link. The pass must still unlink it rather than drop the
/// record and leak the broken symlink on disk.
#[cfg(unix)]
#[test]
fn gc_removes_a_dangling_worktree_symlink() {
    let fx = crate::db::EchoAgentHomeFixture::new();
    let db = WorktreeDb::open(&fx.home).unwrap();

    let link = fx.home.join("dangling-wt");
    std::os::unix::fs::symlink(fx.home.join("gone-target"), &link).unwrap();
    assert!(!link.exists(), "precondition: the symlink target is absent");
    assert!(
        std::fs::symlink_metadata(&link).is_ok(),
        "precondition: the link itself is on disk"
    );

    let record = crate::test_support::worktree_record("dangling-1", link.clone());
    db.register(&record).unwrap();

    let report = gc::gc_worktrees(&db, &expire_now_forced()).unwrap();

    assert_eq!(
        report.no_repo_paths, 1,
        "the dangling link is reclaimed as a no-repo path, not leaked"
    );
    assert!(
        std::fs::symlink_metadata(&link).is_err(),
        "the dangling symlink is unlinked"
    );
    assert!(!record_present(&db, &link));
}

#[test]
fn gc_keeps_an_expired_path_that_is_not_a_repository_but_holds_files() {
    let fx = crate::db::EchoAgentHomeFixture::new();
    let db = WorktreeDb::open(&fx.home).unwrap();

    let path = fx.home.join("no-repo-with-files");
    std::fs::create_dir_all(path.join("subdir")).unwrap();
    std::fs::write(path.join("subdir/notes.md"), b"work").unwrap();

    db.register(&crate::test_support::worktree_record(
        "no-repo-files-1",
        path.clone(),
    ))
    .unwrap();

    let report = gc::gc_worktrees(&db, &expire_now_forced()).unwrap();

    assert_eq!(report.no_repo_paths, 0);
    assert_eq!(report.expired_removed, 0);
    assert_eq!(report.kept_unsafe, 1);
    assert_eq!(
        std::fs::read(path.join("subdir/notes.md")).expect("the notes are gone"),
        b"work"
    );
}

#[test]
fn gc_counts_a_kept_worktree_apart_from_a_busy_one() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db = db_at(&tmp);
    let dir = deletable_linked_worktree(tmp.path(), "kept-wt");
    std::fs::write(dir.join("untracked.rs"), b"work").unwrap();

    db.register(&crate::test_support::worktree_record("kept-1", dir.clone()))
        .unwrap();

    let report = gc::gc_worktrees(&db, &expire_now_forced()).unwrap();

    assert_eq!(report.expired_removed, 0);
    assert_eq!(report.skipped_alive, 0, "no process is holding it");
    assert_eq!(report.kept_unsafe, 1);
    assert_eq!(
        report.kept_reasons,
        [("dirty".to_string(), 1)].into_iter().collect(),
    );
    assert_eq!(
        std::fs::read(dir.join("untracked.rs")).expect("the untracked file is gone"),
        b"work"
    );
}

/// A standalone worktree is judged against the source repository gc
/// hands the gate, so this is the only test where that argument does
/// any work.
#[test]
fn gc_asks_the_source_repository_about_a_standalone_worktree() {
    echo_agent_test_utils::require_git!();
    let tmp = tempfile::TempDir::new().unwrap();
    let db = db_at(&tmp);
    let (dir, source) = deletable_standalone_worktree(tmp.path(), "standalone-wt");
    std::fs::write(dir.join("tracked.txt"), b"work\n").unwrap();
    echo_agent_test_utils::git::run_git(&dir, &["commit", "-am", "work no remote holds"]);

    db.register(&crate::db::WorktreeRecord {
        source_repo: source.clone(),
        creation_mode: "standalone".to_string(),
        ..crate::test_support::worktree_record("standalone-1", dir.clone())
    })
    .unwrap();
    let opts = expire_now_forced();

    let report = gc::gc_worktrees(&db, &opts).unwrap();
    assert_eq!(report.kept_unsafe, 1);
    assert_eq!(
        report.kept_reasons,
        [("only-copy".to_string(), 1)].into_iter().collect(),
    );
    assert_eq!(
        std::fs::read(dir.join("tracked.txt")).expect("the commit's bytes are gone"),
        b"work\n"
    );

    echo_agent_test_utils::git::run_git(&dir, &["push", "origin", "HEAD:refs/heads/main"]);
    // The gate asks the source repository, not the remote, so the source
    // is what has to hold the commit.
    echo_agent_test_utils::git::run_git(&source, &["fetch", "origin"]);

    let report = gc::gc_worktrees(&db, &opts).unwrap();
    assert_eq!(report.expired_removed, 1, "{report:?}");
    assert_eq!(report.kept_unsafe, 0);
    assert!(!dir.exists());
}

/// True if a record with `path` exists in the DB (assert on our own
/// record rather than total count: other tests may write to the same
/// open_default DB concurrently). Matches `register_worktree`'s
/// canonical path storage (/var vs /private/var on macOS).
fn record_present(db: &WorktreeDb, path: &std::path::Path) -> bool {
    let canon = dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    db.list(&ListFilter::default())
        .unwrap()
        .iter()
        .any(|r| r.path == path || r.path == canon)
}

#[test]
fn db_record_survives_failed_removal() {
    // remove_worktree must keep the DB record when the on-disk removal
    // fails, so the worktree isn't lost from tracking while leaking on
    // disk (unregister only after a successful removal).
    let fx = crate::db::EchoAgentHomeFixture::new();

    // A regular file makes remove_dir_all fail (ENOTDIR) deterministically.
    let wt_path = fx.home.join("doomed-wt");
    std::fs::write(&wt_path, b"not a dir").unwrap();

    // Register via the production registration path (uses open_default).
    super::register_worktree(
        &wt_path,
        std::path::Path::new("/src/repo"),
        WorktreeKind::Session,
        "linked",
        "main",
        "abc123",
        None,
        None,
        None,
    );
    let db = WorktreeDb::open(&fx.home).unwrap();
    assert!(
        record_present(&db, &wt_path),
        "precondition: record registered"
    );

    assert!(
        crate::remove_worktree(&wt_path).is_err(),
        "removing a non-directory path must fail"
    );

    assert!(
        record_present(&db, &wt_path),
        "record must survive a failed removal"
    );
}

#[test]
fn db_record_removed_after_successful_removal() {
    // The success direction: a removable worktree must still be
    // unregistered from the DB (catches a regression dropping the
    // unregister).
    echo_agent_test_utils::require_git!();
    use echo_agent_test_utils::git::{git_commit_all, init_git_repo};

    let fx = crate::db::EchoAgentHomeFixture::new();

    // A real repo + a real worktree so remove_worktree succeeds on disk.
    let repo = fx.home.join("repo");
    std::fs::create_dir(&repo).unwrap();
    init_git_repo(&repo);
    std::fs::write(repo.join("f.txt"), "x").unwrap();
    git_commit_all(&repo, "init");
    let wt_path = fx.home.join("live-wt");
    crate::WorktreeBuilder::new(&repo, &wt_path)
        .create()
        .unwrap();

    super::register_worktree(
        &wt_path,
        &repo,
        WorktreeKind::Session,
        "linked",
        "main",
        "abc123",
        None,
        None,
        None,
    );
    let db = WorktreeDb::open(&fx.home).unwrap();
    assert!(
        record_present(&db, &wt_path),
        "precondition: record registered"
    );

    crate::remove_worktree(&wt_path).unwrap();

    assert!(!wt_path.exists(), "worktree dir should be gone");
    assert!(
        !record_present(&db, &wt_path),
        "a successful removal must unregister the DB record"
    );
}

#[test]
fn gc_with_delegate_removes_expired_and_unregisters() {
    // gc_worktrees_with_delegate threads the delegate through the expired
    // path and, on a successful removal, counts it and drops the record.
    // (The delegate's btrfs fallback only fires on a real btrfs-delete
    // failure, which needs a btrfs host; here the plain-dir fast path
    // succeeds, so the mock's delete_snapshot is not called.)
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ECHO_AGENT_HOME == the gc DB dir so remove_worktree's open_default
    // unregister hits the same DB the gc record lives in.
    let fx = crate::db::EchoAgentHomeFixture::new();
    let db = WorktreeDb::open(&fx.home).unwrap();

    let dir = deletable_linked_worktree(&fx.home, "expired-wt");
    let record = crate::test_support::worktree_record("expired-del-1", dir.clone());
    db.register(&record).unwrap();

    let deletes = Arc::new(AtomicUsize::new(0));
    let delegate: Arc<dyn BtrfsDelegate> = Arc::new(super::RecordingDelegate {
        snapshot_path: fx.home.join("unused-snap"),
        worktree_path: dir.clone(),
        deletes: Arc::clone(&deletes),
    });

    let report = gc::gc_worktrees_with_delegate(&db, &expire_now_forced(), Some(delegate)).unwrap();

    assert_eq!(
        report.expired_removed, 1,
        "expired worktree should be reclaimed"
    );
    assert!(!dir.exists(), "the worktree dir should be removed");
    assert!(
        db.get("expired-del-1").unwrap().is_none(),
        "the DB record should be unregistered after a successful removal"
    );
    // Plain-dir fast path succeeds without needing the delegate fallback.
    assert_eq!(deletes.load(Ordering::Relaxed), 0);
}

#[test]
fn gc_report_serde_round_trip() {
    let report = gc::GcReport {
        dead_removed: 3,
        expired_removed: 1,
        skipped_alive: 2,
        never_expiring: 0,
        kept_unsafe: 5,
        kept_reasons: [("dirty".to_string(), 5)].into_iter().collect(),
        kept: vec![gc::KeptWorktree {
            path: "/wt".to_string(),
            reason: "dirty".to_string(),
        }],
        no_repo_paths: 6,
        remove_failed: 4,
        not_judged: 0,
        unnamed: 2,
        names_collected: 7,
        pin_gc_examined: 2,
        pin_gc_pruned: 1,
        pin_gc_deferred: 0,
        pin_gc_kept: 1,
    };
    let json = serde_json::to_string(&report).unwrap();
    let deser: gc::GcReport = serde_json::from_str(&json).unwrap();
    assert_eq!(deser.dead_removed, 3);
    assert_eq!(deser.expired_removed, 1);
    assert_eq!(deser.skipped_alive, 2);
    assert_eq!(deser.kept_unsafe, 5);
    assert_eq!(deser.kept.first().map(|k| k.path.as_str()), Some("/wt"));
    assert_eq!(deser.kept_reasons["dirty"], 5);
    assert_eq!(deser.names_collected, 7);
    assert_eq!(deser.no_repo_paths, 6);
    assert_eq!(deser.remove_failed, 4);
    assert_eq!(deser.unnamed, 2);

    // An older agent's report still deserializes, and its count stays
    // where it was.
    let older = r#"{"dead_removed":0,"expired_removed":0,"skipped_alive":7}"#;
    let deser: gc::GcReport = serde_json::from_str(older).unwrap();
    assert_eq!(deser.skipped_alive, 7);
    assert_eq!(deser.kept_unsafe, 0);
    assert_eq!(deser.no_repo_paths, 0);
    assert!(deser.kept_reasons.is_empty());
}

#[test]
fn gc_options_serde_round_trip() {
    let opts = gc::GcOptions {
        max_age_secs: Some(86400),
        force: true,
        dry_run: false,
        keep_worktrees_containing: vec![std::path::PathBuf::from("/tmp/p")],
        max_age_by_kind: [(WorktreeKind::Subagent, Some(3600))].into_iter().collect(),
    };
    let json = serde_json::to_string(&opts).unwrap();
    // The field still serializes under its wire name for cross-agent compat.
    assert!(json.contains("\"protect_paths\""));
    let deser: gc::GcOptions = serde_json::from_str(&json).unwrap();
    assert_eq!(deser.max_age_secs, Some(86400));
    assert!(deser.force);
    assert!(!deser.dry_run);
    assert_eq!(
        deser.keep_worktrees_containing,
        opts.keep_worktrees_containing
    );
    assert_eq!(
        deser.max_age_by_kind.get(&WorktreeKind::Subagent),
        Some(&Some(3600))
    );
    // A wire blob using the legacy `protect_paths` key still lands in the field.
    let legacy_named = r#"{"max_age_secs":1,"force":false,"dry_run":true,"protect_paths":["/x"]}"#;
    let legacy_named_opts: gc::GcOptions = serde_json::from_str(legacy_named).unwrap();
    assert_eq!(
        legacy_named_opts.keep_worktrees_containing,
        vec![std::path::PathBuf::from("/x")]
    );
    // Absent new fields deserialize as empty (old agents).
    let legacy = r#"{"max_age_secs":1,"force":false,"dry_run":true}"#;
    let legacy_opts: gc::GcOptions = serde_json::from_str(legacy).unwrap();
    assert!(legacy_opts.keep_worktrees_containing.is_empty());
    assert!(legacy_opts.max_age_by_kind.is_empty());

    // JSON null value in map → never-expire (None).
    let with_null = r#"{
                "max_age_secs": 100,
                "force": false,
                "dry_run": false,
                "max_age_by_kind": {"manual": null, "subagent": 3600}
            }"#;
    let null_opts: gc::GcOptions = serde_json::from_str(with_null).unwrap();
    assert_eq!(
        null_opts.max_age_by_kind.get(&WorktreeKind::Manual),
        Some(&None)
    );
    assert_eq!(
        null_opts.max_age_by_kind.get(&WorktreeKind::Subagent),
        Some(&Some(3600))
    );
    let round = serde_json::to_string(&null_opts).unwrap();
    let back: gc::GcOptions = serde_json::from_str(&round).unwrap();
    assert_eq!(back.max_age_by_kind, null_opts.max_age_by_kind);
}

#[test]
fn gc_in_use_path_skips_age_expiry_including_dry_run() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db = db_at(&tmp);
    let dir = deletable_linked_worktree(tmp.path(), "protected-wt");
    let nested = dir.join("nested");
    let record = crate::test_support::worktree_record("prot-1", dir.clone());
    db.register(&record).unwrap();

    // Keep via an in-use path inside it (same canonicalize rules as cwd_within).
    let report = gc::gc_worktrees(
        &db,
        &gc::GcOptions {
            keep_worktrees_containing: vec![nested],
            ..dry_run_opts()
        },
    )
    .unwrap();
    assert_eq!(
        report.expired_removed, 0,
        "dry_run must not count an in-use worktree as would-expire"
    );
    assert_eq!(report.skipped_alive, 1);
    assert!(dir.exists());

    // With nothing in use, dry_run would-count it.
    let unguarded = gc::gc_worktrees(&db, &dry_run_opts()).unwrap();
    assert_eq!(unguarded.expired_removed, 1);
}

#[test]
fn gc_in_use_path_pre_remove_recheck() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db = db_at(&tmp);
    let dir = deletable_linked_worktree(tmp.path(), "prot-real");
    db.register(&crate::test_support::worktree_record(
        "prot-real",
        dir.clone(),
    ))
    .unwrap();
    let report = gc::gc_worktrees(
        &db,
        &gc::GcOptions {
            keep_worktrees_containing: vec![dir.clone()],
            ..expire_now()
        },
    )
    .unwrap();
    assert_eq!(report.expired_removed, 0);
    assert_eq!(report.skipped_alive, 1);
    assert!(dir.exists(), "an in-use worktree must not be removed");
}

#[test]
fn force_does_not_override_never_expire_kinds() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db = db_at(&tmp);
    let dir = deletable_linked_worktree(tmp.path(), "manual-force");
    register_kind(&db, "manual-force", &dir, WorktreeKind::Manual);
    let report = gc::gc_worktrees(
        &db,
        &gc::GcOptions {
            max_age_by_kind: [(WorktreeKind::Manual, None)].into_iter().collect(),
            ..expire_now_forced()
        },
    )
    .unwrap();
    assert!(
        dir.exists(),
        "force must not age-expire never-expire kinds (max_age_by_kind None)"
    );
    assert_eq!(report.expired_removed, 0);
}

#[test]
fn gc_never_expire_manual_age_only_not_dead() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db = db_at(&tmp);
    let manual_dir = deletable_linked_worktree(tmp.path(), "manual-alive");
    let session_dir = deletable_linked_worktree(tmp.path(), "session-alive");
    register_kind(&db, "manual", &manual_dir, WorktreeKind::Manual);
    register_kind(&db, "session", &session_dir, WorktreeKind::Session);
    // Dead manual (missing path) still reclaimed on dead path.
    register_kind(
        &db,
        "manual-dead",
        std::path::Path::new("/nonexistent/manual-dead"),
        WorktreeKind::Manual,
    );

    let report = gc::gc_worktrees(
        &db,
        &gc::GcOptions {
            max_age_by_kind: [(WorktreeKind::Manual, None)].into_iter().collect(),
            ..expire_now()
        },
    )
    .unwrap();
    assert!(
        manual_dir.exists(),
        "Manual must not age-expire when never-expire (max_age_by_kind None)"
    );
    assert!(
        !session_dir.exists(),
        "Session must still age-expire under Manual=never"
    );
    assert_eq!(report.expired_removed, 1);
    assert!(
        report.never_expiring >= 1,
        "expired never-expire kinds must surface in never_expiring"
    );
    assert_eq!(report.dead_removed, 1, "dead Manual still unregisters");

    // dry_run must not count skipped kinds as would-expire.
    let dir2 = deletable_linked_worktree(tmp.path(), "manual-dry");
    register_kind(&db, "manual-dry", &dir2, WorktreeKind::Manual);
    let dry = gc::gc_worktrees(
        &db,
        &gc::GcOptions {
            max_age_by_kind: [(WorktreeKind::Manual, None)].into_iter().collect(),
            ..dry_run_opts()
        },
    )
    .unwrap();
    assert_eq!(
        dry.expired_removed, 0,
        "dry_run must not count never-expire kinds as expired"
    );
    assert!(
        dry.never_expiring >= 1,
        "dry_run still counts expired never-expire kinds as never_expiring"
    );
    assert!(dir2.exists());
}

/// End-to-end survivors for the per-kind age cutoff: each row registers
/// real worktree dirs of the given kinds, runs gc, and asserts which
/// dirs survive plus `expired_removed`. The reclaimability logic itself
/// is unit-tested in `classify_covers_expiry_guards_and_kind_ttls`
/// and `effective_max_age_precedence`; this pins the disk effect.
#[test]
fn per_kind_age_expiry_reclaims_listed_kinds_and_keeps_the_rest() {
    const HOUR: i64 = 3600;
    const ANCIENT: i64 = 10 * 365 * 86400;
    struct Case {
        name: &'static str,
        // (id, kind, seconds-ago for both created_at and last_accessed)
        records: Vec<(&'static str, WorktreeKind, i64)>,
        opts: gc::GcOptions,
        survivors: Vec<&'static str>,
        expired_removed: u64,
    }
    let cases = vec![
        Case {
            name: "subagent past its shorter TTL goes, session within default stays",
            records: vec![
                ("sub", WorktreeKind::Subagent, 2 * HOUR),
                ("sess", WorktreeKind::Session, 2 * HOUR),
            ],
            opts: gc::GcOptions {
                max_age_secs: Some(7 * 86400),
                max_age_by_kind: [(WorktreeKind::Subagent, Some(HOUR))].into_iter().collect(),
                ..Default::default()
            },
            survivors: vec!["sess"],
            expired_removed: 1,
        },
        Case {
            name: "a listed kind expires while an unlisted kind stays with no default",
            records: vec![
                ("pool", WorktreeKind::Pool, ANCIENT),
                ("sess", WorktreeKind::Session, ANCIENT),
            ],
            opts: gc::GcOptions {
                max_age_secs: None,
                max_age_by_kind: [(WorktreeKind::Pool, Some(0))].into_iter().collect(),
                ..Default::default()
            },
            survivors: vec!["sess"],
            expired_removed: 1,
        },
        Case {
            name: "manual is configurable to expire with an explicit TTL",
            records: vec![("manual", WorktreeKind::Manual, ANCIENT)],
            opts: gc::GcOptions {
                max_age_secs: Some(7 * 86400),
                max_age_by_kind: [(WorktreeKind::Manual, Some(0))].into_iter().collect(),
                ..Default::default()
            },
            survivors: vec![],
            expired_removed: 1,
        },
    ];

    for case in cases {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = db_at(&tmp);
        let now = crate::db::now_epoch_secs();
        let dirs: Vec<(&str, std::path::PathBuf)> = case
            .records
            .iter()
            .map(|&(id, kind, secs_ago)| {
                let dir = deletable_linked_worktree(tmp.path(), id);
                db.register(&crate::db::WorktreeRecord {
                    id: id.to_string(),
                    path: dir.clone(),
                    kind,
                    created_at: now - secs_ago,
                    last_accessed_at: Some(now - secs_ago),
                    ..crate::test_support::worktree_record("", std::path::PathBuf::new())
                })
                .unwrap();
                (id, dir)
            })
            .collect();

        let report = gc::gc_worktrees(&db, &case.opts).unwrap();

        assert_eq!(
            report.expired_removed, case.expired_removed,
            "{}",
            case.name
        );
        for (id, dir) in &dirs {
            assert_eq!(
                dir.exists(),
                case.survivors.contains(id),
                "{}: {id}",
                case.name
            );
        }
    }
}

#[test]
fn dry_run_counts_per_kind_cutoffs() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db = db_at(&tmp);
    let now = crate::db::now_epoch_secs();
    let age = 2 * 3600;
    let sub_dir = deletable_linked_worktree(tmp.path(), "sub-dry");
    let sess_dir = deletable_linked_worktree(tmp.path(), "sess-dry");
    let man_dir = deletable_linked_worktree(tmp.path(), "man-dry");
    let base = crate::db::WorktreeRecord {
        created_at: now - age,
        last_accessed_at: Some(now - age),
        ..crate::test_support::worktree_record("", std::path::PathBuf::new())
    };
    db.register(&crate::db::WorktreeRecord {
        id: "sub-dry".into(),
        path: sub_dir.clone(),
        kind: WorktreeKind::Subagent,
        ..base.clone()
    })
    .unwrap();
    db.register(&crate::db::WorktreeRecord {
        id: "sess-dry".into(),
        path: sess_dir.clone(),
        kind: WorktreeKind::Session,
        ..base.clone()
    })
    .unwrap();
    db.register(&crate::db::WorktreeRecord {
        id: "man-dry".into(),
        path: man_dir.clone(),
        kind: WorktreeKind::Manual,
        ..base
    })
    .unwrap();

    let dry = gc::gc_worktrees(
        &db,
        &gc::GcOptions {
            max_age_secs: Some(7 * 86400),
            force: false,
            dry_run: true,
            max_age_by_kind: [
                (WorktreeKind::Subagent, Some(3600)),
                (WorktreeKind::Manual, None),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        dry.expired_removed, 1,
        "only subagent past its kind TTL is would-expire"
    );
    assert!(sub_dir.exists() && sess_dir.exists() && man_dir.exists());

    // max_age_secs=0: session+subagent would-expire; manual never → skipped.
    let dry0 = gc::gc_worktrees(
        &db,
        &gc::GcOptions {
            max_age_by_kind: [
                (WorktreeKind::Subagent, Some(3600)),
                (WorktreeKind::Manual, None),
            ]
            .into_iter()
            .collect(),
            ..dry_run_opts()
        },
    )
    .unwrap();
    assert_eq!(dry0.expired_removed, 2);
    assert!(dry0.never_expiring >= 1);
}

#[test]
fn db_stats_serde_round_trip() {
    let stats = crate::db::DbStats {
        total_records: 10,
        alive_count: 7,
        dead_count: 3,
        db_file_bytes: 4096,
    };
    let json = serde_json::to_string(&stats).unwrap();
    let deser: crate::db::DbStats = serde_json::from_str(&json).unwrap();
    assert_eq!(deser.total_records, 10);
    assert_eq!(deser.alive_count, 7);
    assert_eq!(deser.dead_count, 3);
    assert_eq!(deser.db_file_bytes, 4096);
}
