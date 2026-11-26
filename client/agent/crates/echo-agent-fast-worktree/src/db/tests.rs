use super::*;

fn make_record(id: &str, path: &str, kind: WorktreeKind) -> WorktreeRecord {
    WorktreeRecord {
        source_repo: PathBuf::from("/src/repo"),
        kind,
        git_ref: Some("main".to_string()),
        head_commit: Some("abc123".to_string()),
        session_id: Some(format!("sess-{id}")),
        creator_pid: Some(12345),
        created_at: 1000,
        ..crate::test_support::worktree_record(id, path)
    }
}

fn make_labeled_record(id: &str, path: &str, label: &str) -> WorktreeRecord {
    let mut rec = make_record(id, path, WorktreeKind::Session);
    rec.metadata = Some(serde_json::json!({"label": label, "user_provided": true}));
    rec
}

#[test]
fn register_and_get_by_id() {
    let db = WorktreeDb::open_in_memory().unwrap();
    let rec = make_record("abc", "/tmp/wt-abc", WorktreeKind::Session);

    db.register(&rec).unwrap();

    let fetched = db.get("abc").unwrap().expect("should find by id");
    assert_eq!(fetched.id, "abc");
    assert_eq!(fetched.path, PathBuf::from("/tmp/wt-abc"));
    assert_eq!(fetched.kind, WorktreeKind::Session);
    assert_eq!(fetched.status, WorktreeStatus::Alive);
    assert_eq!(fetched.creator_pid, Some(12345));
    assert_eq!(fetched.git_ref.as_deref(), Some("main"));
    assert_eq!(fetched.session_id.as_deref(), Some("sess-abc"));
}

#[test]
fn get_by_path() {
    let db = WorktreeDb::open_in_memory().unwrap();
    let rec = make_record("xyz", "/tmp/wt-xyz", WorktreeKind::Fork);
    db.register(&rec).unwrap();

    let fetched = db.get("/tmp/wt-xyz").unwrap().expect("should find by path");
    assert_eq!(fetched.id, "xyz");
    assert_eq!(fetched.kind, WorktreeKind::Fork);
}

#[test]
fn get_missing_returns_none() {
    let db = WorktreeDb::open_in_memory().unwrap();
    assert!(db.get("nonexistent").unwrap().is_none());
    assert!(db.get("/no/such/path").unwrap().is_none());
}

#[test]
fn unregister_by_id() {
    let db = WorktreeDb::open_in_memory().unwrap();
    db.register(&make_record("a", "/tmp/a", WorktreeKind::Session))
        .unwrap();

    assert!(db.unregister("a").unwrap());
    assert!(db.get("a").unwrap().is_none());
    assert!(!db.unregister("a").unwrap());
}

#[test]
fn mark_dead_and_list_filter() {
    let db = WorktreeDb::open_in_memory().unwrap();
    db.register(&make_record("live", "/tmp/live", WorktreeKind::Session))
        .unwrap();
    db.register(&make_record("gone", "/tmp/gone", WorktreeKind::Session))
        .unwrap();

    db.mark_dead("gone").unwrap();

    // Default filter excludes dead
    let alive = db.list(&ListFilter::default()).unwrap();
    assert_eq!(alive.len(), 1);
    assert_eq!(alive[0].id, "live");

    // include_dead shows both
    let all = db
        .list(&ListFilter {
            include_dead: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(all.len(), 2);

    let dead_rec = all.iter().find(|r| r.id == "gone").unwrap();
    assert_eq!(dead_rec.status, WorktreeStatus::Dead);
}

#[test]
fn list_filter_by_kind() {
    let db = WorktreeDb::open_in_memory().unwrap();
    db.register(&make_record("s1", "/tmp/s1", WorktreeKind::Session))
        .unwrap();
    db.register(&make_record("p1", "/tmp/p1", WorktreeKind::Pool))
        .unwrap();
    db.register(&make_record("f1", "/tmp/f1", WorktreeKind::Fork))
        .unwrap();

    let sessions = db
        .list(&ListFilter {
            kind: Some(WorktreeKind::Session),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, "s1");

    let pools = db
        .list(&ListFilter {
            kind: Some(WorktreeKind::Pool),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(pools.len(), 1);
    assert_eq!(pools[0].id, "p1");
}

#[test]
fn list_filter_by_repo() {
    let db = WorktreeDb::open_in_memory().unwrap();
    let mut r1 = make_record("a", "/tmp/a", WorktreeKind::Session);
    r1.repo_name = "myrepo".to_string();
    let mut r2 = make_record("b", "/tmp/b", WorktreeKind::Session);
    r2.repo_name = "other".to_string();
    db.register(&r1).unwrap();
    db.register(&r2).unwrap();

    let matched = db
        .list(&ListFilter {
            repo_name: Some("myrepo".to_string()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(matched.len(), 1);
    assert_eq!(matched[0].id, "a");
}

#[test]
fn touch_updates_last_accessed() {
    let db = WorktreeDb::open_in_memory().unwrap();
    db.register(&make_record("t", "/tmp/t", WorktreeKind::Session))
        .unwrap();

    let before = db.get("t").unwrap().unwrap();
    assert!(before.last_accessed_at.is_none());

    db.touch("t").unwrap();

    let after = db.get("t").unwrap().unwrap();
    assert!(after.last_accessed_at.is_some());
    assert!(after.last_accessed_at.unwrap() > 0);
}

#[test]
fn stats_counts() {
    let db = WorktreeDb::open_in_memory().unwrap();
    db.register(&make_record("a", "/tmp/a", WorktreeKind::Session))
        .unwrap();
    db.register(&make_record("b", "/tmp/b", WorktreeKind::Pool))
        .unwrap();
    db.register(&make_record("c", "/tmp/c", WorktreeKind::Fork))
        .unwrap();
    db.mark_dead("c").unwrap();

    let stats = db.stats().unwrap();
    assert_eq!(stats.total_records, 3);
    assert_eq!(stats.alive_count, 2);
    assert_eq!(stats.dead_count, 1);
}

#[test]
fn sweep_dead_marks_missing_paths() {
    let db = WorktreeDb::open_in_memory().unwrap();

    let tmp = tempfile::TempDir::new().unwrap();
    let existing = tmp.path().join("exists");
    std::fs::create_dir(&existing).unwrap();

    db.register(&make_record(
        "exists",
        &existing.to_string_lossy(),
        WorktreeKind::Session,
    ))
    .unwrap();
    db.register(&make_record(
        "gone",
        "/nonexistent/path/xyz",
        WorktreeKind::Session,
    ))
    .unwrap();

    let marked = db.sweep_dead().unwrap();
    assert_eq!(marked, 1);

    let gone_rec = db
        .list(&ListFilter {
            include_dead: true,
            ..Default::default()
        })
        .unwrap()
        .into_iter()
        .find(|r| r.id == "gone")
        .unwrap();
    assert_eq!(gone_rec.status, WorktreeStatus::Dead);

    let exists_rec = db.get("exists").unwrap().unwrap();
    assert_eq!(exists_rec.status, WorktreeStatus::Alive);
}

#[cfg(unix)]
#[test]
fn sweep_dead_does_not_mark_dangling_symlink() {
    let db = WorktreeDb::open_in_memory().unwrap();
    let tmp = tempfile::TempDir::new().unwrap();
    let link = tmp.path().join("dangling");
    std::os::unix::fs::symlink(tmp.path().join("gone"), &link).unwrap();
    db.register(&make_record(
        "dangling",
        &link.to_string_lossy(),
        WorktreeKind::Session,
    ))
    .unwrap();
    assert_eq!(db.sweep_dead().unwrap(), 0);
    let rec = db.get("dangling").unwrap().unwrap();
    assert_eq!(rec.status, WorktreeStatus::Alive);
}

#[test]
fn sweep_dead_skips_live_grove_dests() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db = WorktreeDb::open_in_memory().unwrap();
    for (id, mode) in [
        ("nfs-legacy", "nfs"),
        ("grove-nfs", "grove-nfs"),
        ("grove-fuse", "grove-fuse"),
    ] {
        let dest = tmp.path().join(id);
        std::fs::create_dir(&dest).unwrap();
        let mut rec = make_record(id, dest.to_str().unwrap(), WorktreeKind::Session);
        rec.creation_mode = mode.into();
        db.register(&rec).unwrap();
    }
    assert_eq!(db.sweep_dead().unwrap(), 0);
    for id in ["nfs-legacy", "grove-nfs", "grove-fuse"] {
        let fetched = db.get(id).unwrap().unwrap();
        assert_eq!(fetched.status, WorktreeStatus::Alive, "{id}");
    }
}

#[test]
fn sweep_dead_marks_missing_grove_dest() {
    let db = WorktreeDb::open_in_memory().unwrap();
    let mut rec = make_record(
        "grove-gone",
        "/nonexistent/grove-fuse/dest",
        WorktreeKind::Session,
    );
    rec.creation_mode = "grove-fuse".into();
    db.register(&rec).unwrap();
    assert_eq!(db.sweep_dead().unwrap(), 1);
    let fetched = db.get("grove-gone").unwrap().unwrap();
    assert_eq!(fetched.status, WorktreeStatus::Dead);
}

#[test]
fn register_upsert_overwrites() {
    let db = WorktreeDb::open_in_memory().unwrap();
    let mut rec = make_record("up", "/tmp/up", WorktreeKind::Session);
    db.register(&rec).unwrap();

    rec.head_commit = Some("new-sha".to_string());
    rec.kind = WorktreeKind::Fork;
    db.register(&rec).unwrap();

    let fetched = db.get("up").unwrap().unwrap();
    assert_eq!(fetched.head_commit.as_deref(), Some("new-sha"));
    assert_eq!(fetched.kind, WorktreeKind::Fork);
}

#[test]
fn list_ordered_by_created_at_desc() {
    let db = WorktreeDb::open_in_memory().unwrap();
    let mut r1 = make_record("old", "/tmp/old", WorktreeKind::Session);
    r1.created_at = 100;
    let mut r2 = make_record("new", "/tmp/new", WorktreeKind::Session);
    r2.created_at = 200;
    let mut r3 = make_record("mid", "/tmp/mid", WorktreeKind::Session);
    r3.created_at = 150;

    db.register(&r1).unwrap();
    db.register(&r2).unwrap();
    db.register(&r3).unwrap();

    let all = db.list(&ListFilter::default()).unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].id, "new");
    assert_eq!(all[1].id, "mid");
    assert_eq!(all[2].id, "old");
}

/// The derived id keeps the basename (minus any `worktree-` prefix) and appends
/// a 16-hex hash of the full path. Assert the shape rather than a literal hash.
fn assert_id_shape(id: &str, basename: &str) {
    let hash = id
        .strip_prefix(&format!("{basename}-"))
        .unwrap_or_else(|| panic!("id {id:?} must keep the `{basename}-` prefix"));
    assert_eq!(hash.len(), 16, "hash must be 16 hex chars: {id:?}");
    assert!(
        hash.bytes().all(|b| b.is_ascii_hexdigit()),
        "hash must be hex: {id:?}"
    );
}

#[test]
fn id_from_path_strips_worktree_prefix_and_hashes_full_path() {
    let p = Path::new("/home/.echo-agent/worktrees/myrepo/worktree-019caa03");
    assert_id_shape(&id_from_path(p), "019caa03");
    assert_id_shape(
        &id_from_path(Path::new("/home/.echo-agent/worktree_pool/inst/a1b2c3")),
        "a1b2c3",
    );
    assert_id_shape(&id_from_path(Path::new("/tmp/my-worktree")), "my-worktree");
    // No file name → sanitizer uses `wt`, still suffixed with a hash.
    assert_id_shape(&id_from_path(Path::new("/")), "wt");
    // Deterministic.
    assert_eq!(id_from_path(p), id_from_path(p));
}

#[test]
fn same_basename_worktrees_in_different_repos_coexist() {
    // Two repos each have a `wt-abc` worktree. Registering both (the way
    // discovery/register derive ids) must keep BOTH records — neither evicts
    // the other via the `id` PRIMARY KEY or the `path UNIQUE` constraint.
    let db = WorktreeDb::open_in_memory().unwrap();

    let path_a = "/home/.echo-agent/worktrees/repo-a/session/wt-abc";
    let path_b = "/home/.echo-agent/worktrees/repo-b/session/wt-abc";
    let mut rec_a = make_record(
        &id_from_path(Path::new(path_a)),
        path_a,
        WorktreeKind::Session,
    );
    rec_a.repo_name = "repo-a".into();
    rec_a.source_repo = PathBuf::from("/src/repo-a");
    let mut rec_b = make_record(
        &id_from_path(Path::new(path_b)),
        path_b,
        WorktreeKind::Session,
    );
    rec_b.repo_name = "repo-b".into();
    rec_b.source_repo = PathBuf::from("/src/repo-b");

    db.register(&rec_a).unwrap();
    db.register(&rec_b).unwrap();

    // Both rows survive and resolve independently by id and by path.
    assert_eq!(
        db.list(&ListFilter::default()).unwrap().len(),
        2,
        "both same-basename worktrees must coexist"
    );
    assert_eq!(db.get(path_a).unwrap().unwrap().repo_name, "repo-a");
    assert_eq!(db.get(path_b).unwrap().unwrap().repo_name, "repo-b");
    assert_eq!(
        db.get_by_id(&rec_a.id).unwrap().unwrap().path,
        PathBuf::from(path_a)
    );
    assert_eq!(
        db.get_by_id(&rec_b.id).unwrap().unwrap().path,
        PathBuf::from(path_b)
    );

    // Removing one (by path) leaves the other intact.
    assert!(db.unregister_by_path(Path::new(path_a)).unwrap());
    assert!(db.get(path_a).unwrap().is_none());
    assert_eq!(db.get(path_b).unwrap().unwrap().repo_name, "repo-b");
}

#[test]
fn kind_str_roundtrip() {
    for kind in [
        WorktreeKind::Session,
        WorktreeKind::Ab,
        WorktreeKind::Pool,
        WorktreeKind::Fork,
        WorktreeKind::Manual,
        WorktreeKind::Subagent,
    ] {
        assert_eq!(WorktreeKind::from_str_lossy(kind.as_str()), kind);
    }
    assert_eq!(
        WorktreeKind::from_str_lossy("garbage"),
        WorktreeKind::Manual
    );
}

#[test]
fn list_filter_by_source_repo() {
    let db = WorktreeDb::open_in_memory().unwrap();

    let mut r1 = make_record("wt-1", "/wt/1", WorktreeKind::Session);
    r1.source_repo = PathBuf::from("/src/repo-A");
    r1.repo_name = "repo-A".into();
    db.register(&r1).unwrap();

    let mut r2 = make_record("wt-2", "/wt/2", WorktreeKind::Session);
    r2.source_repo = PathBuf::from("/src/repo-A");
    r2.repo_name = "repo-A".into();
    db.register(&r2).unwrap();

    let mut r3 = make_record("wt-3", "/wt/3", WorktreeKind::Session);
    r3.source_repo = PathBuf::from("/src/repo-B");
    r3.repo_name = "repo-B".into();
    db.register(&r3).unwrap();

    // Filter by source_repo = repo-A: should get 2
    let filter = ListFilter {
        source_repo: Some(PathBuf::from("/src/repo-A")),
        ..Default::default()
    };
    let results = db.list(&filter).unwrap();
    assert_eq!(results.len(), 2);
    assert!(
        results
            .iter()
            .all(|r| r.source_repo == Path::new("/src/repo-A"))
    );

    // Filter by source_repo = repo-B: should get 1
    let filter = ListFilter {
        source_repo: Some(PathBuf::from("/src/repo-B")),
        ..Default::default()
    };
    let results = db.list(&filter).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "wt-3");

    // Filter by nonexistent source_repo: should get 0
    let filter = ListFilter {
        source_repo: Some(PathBuf::from("/src/nonexistent")),
        ..Default::default()
    };
    let results = db.list(&filter).unwrap();
    assert!(results.is_empty());

    // No source_repo filter: should get all 3
    let results = db.list(&ListFilter::default()).unwrap();
    assert_eq!(results.len(), 3);
}

#[test]
fn get_by_label_returns_matching_record() {
    let db = WorktreeDb::open_in_memory().unwrap();
    let rec = make_labeled_record("wt-abc123", "/tmp/wt-abc123", "my-feature");
    db.register(&rec).unwrap();

    let fetched = db
        .get_by_label("my-feature")
        .unwrap()
        .expect("should find by label");
    assert_eq!(fetched.id, "wt-abc123");
    assert_eq!(fetched.path, PathBuf::from("/tmp/wt-abc123"));
}

#[test]
fn get_by_label_ignores_records_without_metadata() {
    let db = WorktreeDb::open_in_memory().unwrap();
    let rec = make_record("wt-plain", "/tmp/wt-plain", WorktreeKind::Session);
    db.register(&rec).unwrap();

    assert!(db.get_by_label("wt-plain").unwrap().is_none());
}

#[test]
fn get_resolves_by_label_when_id_misses() {
    let db = WorktreeDb::open_in_memory().unwrap();
    let rec = make_labeled_record("wt-abc123", "/tmp/wt-abc123", "test-2");
    db.register(&rec).unwrap();

    // "test-2" doesn't match any ID, so it should fall back to label lookup
    let fetched = db.get("test-2").unwrap().expect("should resolve by label");
    assert_eq!(fetched.id, "wt-abc123");
}

#[test]
fn get_prefers_id_over_label() {
    let db = WorktreeDb::open_in_memory().unwrap();

    // Record whose ID is "ambiguous"
    let r1 = make_record("ambiguous", "/tmp/wt-by-id", WorktreeKind::Session);
    db.register(&r1).unwrap();

    // Record whose label is "ambiguous"
    let r2 = make_labeled_record("wt-other", "/tmp/wt-other", "ambiguous");
    db.register(&r2).unwrap();

    let fetched = db
        .get("ambiguous")
        .unwrap()
        .expect("should find by id first");
    assert_eq!(fetched.id, "ambiguous");
    assert_eq!(fetched.path, PathBuf::from("/tmp/wt-by-id"));
}

#[test]
fn get_by_label_ignores_malformed_metadata() {
    let db = WorktreeDb::open_in_memory().unwrap();
    let rec = make_record("wt-bad", "/tmp/wt-bad", WorktreeKind::Session);
    db.register(&rec).unwrap();

    // Overwrite metadata with non-JSON text via raw SQL
    db.conn
        .execute(
            "UPDATE worktrees SET metadata = 'not json at all' WHERE id = 'wt-bad'",
            [],
        )
        .unwrap();

    assert!(db.get_by_label("not json at all").unwrap().is_none());
    assert!(db.get_by_label("anything").unwrap().is_none());
}

#[test]
fn get_by_label_returns_most_recent_on_duplicate_labels() {
    let db = WorktreeDb::open_in_memory().unwrap();

    let mut older = make_labeled_record("wt-old", "/tmp/wt-old", "shared-label");
    older.created_at = 100;
    db.register(&older).unwrap();

    let mut newer = make_labeled_record("wt-new", "/tmp/wt-new", "shared-label");
    newer.created_at = 200;
    db.register(&newer).unwrap();

    let fetched = db
        .get_by_label("shared-label")
        .unwrap()
        .expect("should find the most recent");
    assert_eq!(fetched.id, "wt-new");

    // Also verify via the get() fallback path
    let via_get = db
        .get("shared-label")
        .unwrap()
        .expect("should resolve via label fallback");
    assert_eq!(via_get.id, "wt-new");
}

#[test]
fn concurrent_open_at_survives_wal_conversion_race() {
    // Many openers hitting a FRESH db at once race the one-time WAL conversion
    // (which ignores busy_timeout). set_journal_mode's retry must make every
    // open succeed rather than intermittently returning Err (which callers
    // swallow, silently dropping worktree tracking). Without the retry this
    // flakes.
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("worktrees.db");

    let handles: Vec<_> = (0..16)
        .map(|_| {
            let path = path.clone();
            std::thread::spawn(move || WorktreeDb::open_at(&path).is_ok())
        })
        .collect();

    for h in handles {
        assert!(
            h.join().unwrap(),
            "concurrent open_at must not fail on the WAL conversion race"
        );
    }
}

fn journal_mode(db: &WorktreeDb) -> String {
    db.conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn open_at_uses_wal_on_local_fs() {
    // Ambient kill-switch would override the decision; skip if set.
    if std::env::var("ECHO_AGENT_SQLITE_JOURNAL_MODE").is_ok() {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let db = WorktreeDb::open_at(&tmp.path().join("worktrees.db")).unwrap();
    assert_eq!(journal_mode(&db), "wal");
}

#[test]
fn network_mode_uses_fresh_per_host_truncate_db() {
    // Network mode opens a per-host sibling of the given path (the legacy
    // shared file is left untouched — a live old binary can flip it back to
    // WAL at any time) in rollback-journal mode.
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("worktrees.db");

    {
        let db = WorktreeDb::open_at(&path).unwrap();
        db.register(&make_record(
            "wt-legacy",
            "/tmp/wt-legacy",
            WorktreeKind::Session,
        ))
        .unwrap();
    }

    let db = WorktreeDb::open_at_with_journal_mode(&path, JournalMode::Truncate).unwrap();
    assert_eq!(journal_mode(&db), "truncate");
    // Fresh per-host DB: legacy rows are intentionally not visible.
    assert!(db.get("wt-legacy").unwrap().is_none());
    db.register(&make_record("wt-nfs", "/tmp/wt-nfs", WorktreeKind::Manual))
        .unwrap();
    assert!(db.get("wt-nfs").unwrap().is_some());
    drop(db);

    let eff = JournalMode::Truncate.effective_db_path(&path);
    assert_ne!(eff, path);
    let base = eff.display().to_string();
    assert!(!std::fs::exists(format!("{base}-wal")).unwrap());
    assert!(!std::fs::exists(format!("{base}-shm")).unwrap());
}

#[test]
fn journal_conversion_respects_deadline_under_contention() {
    use std::time::{Duration, Instant};

    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("worktrees.db");
    // WAL-stamp the exact file the forced-network open will use.
    let eff = JournalMode::Truncate.effective_db_path(&path);
    {
        let conn = rusqlite::Connection::open(&eff).unwrap();
        JournalMode::Wal.apply(&conn).unwrap();
        conn.execute_batch("CREATE TABLE t (v TEXT); INSERT INTO t VALUES ('x');")
            .unwrap();
    }

    // A held WAL read transaction blocks the exclusive lock the WAL->TRUNCATE
    // conversion needs, so the open must give up at the deadline instead of
    // stalling for attempts x busy_timeout.
    let holder = rusqlite::Connection::open(&eff).unwrap();
    holder
        .execute_batch("BEGIN; SELECT COUNT(*) FROM t;")
        .unwrap();

    let start = Instant::now();
    let res = WorktreeDb::open_at_with_journal_mode(&path, JournalMode::Truncate);
    let elapsed = start.elapsed();
    let err = match res {
        Ok(_) => panic!("conversion must fail while a WAL reader holds the DB"),
        Err(e) => e,
    };
    assert!(
        format!("{err:#}").contains("database is locked"),
        "expected the deadline-busy error, got: {err:#}"
    );
    assert!(
        elapsed < Duration::from_secs(20),
        "10s budget (+slack) exceeded: {elapsed:?}"
    );

    // Release the reader: the same open now converts and succeeds.
    holder.execute_batch("COMMIT;").unwrap();
    drop(holder);
    let db = WorktreeDb::open_at_with_journal_mode(&path, JournalMode::Truncate).unwrap();
    assert_eq!(journal_mode(&db), "truncate");
}

#[test]
fn record_label_reads_metadata() {
    assert_eq!(
        make_labeled_record("a", "/tmp/wt-a", "my-feature").label(),
        Some("my-feature")
    );
    let mut rec = make_record("b", "/tmp/wt-b", WorktreeKind::Session);
    assert_eq!(rec.label(), None);
    rec.metadata = Some(serde_json::json!({"label": ""}));
    assert_eq!(rec.label(), None);
}

#[test]
fn open_read_only_never_creates_and_reads_existing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path().join("echo-agent-home");

    assert!(matches!(
        WorktreeDb::open_read_only(&home),
        RegistryOpen::Absent { .. }
    ));
    assert!(
        !home.exists(),
        "read-only open must not create the home dir"
    );

    let rw = WorktreeDb::open(&home).unwrap();
    rw.register(&make_labeled_record("a", "/tmp/wt-a", "lbl"))
        .unwrap();

    let RegistryOpen::Opened { db: ro, .. } = WorktreeDb::open_read_only(&home) else {
        panic!("an existing DB must open");
    };
    let recs = ro.list(&ListFilter::default()).unwrap();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].label(), Some("lbl"));
}

#[test]
fn sqlite_errors_classify_by_code() {
    use rusqlite::ErrorCode;
    fn err(code: ErrorCode) -> anyhow::Error {
        anyhow::Error::new(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code,
                extended_code: 0,
            },
            None,
        ))
        .context("failed to list worktrees")
    }
    let cases = [
        (ErrorCode::DatabaseBusy, SqliteFailureKind::Busy),
        (ErrorCode::DatabaseLocked, SqliteFailureKind::Busy),
        (ErrorCode::NotADatabase, SqliteFailureKind::Corrupt),
        (ErrorCode::DatabaseCorrupt, SqliteFailureKind::Corrupt),
        (ErrorCode::SystemIoFailure, SqliteFailureKind::Other),
        (ErrorCode::ReadOnly, SqliteFailureKind::Other),
        (ErrorCode::Unknown, SqliteFailureKind::Other),
    ];
    for (code, want) in cases {
        assert_eq!(classify_sqlite_error(&err(code)), want, "{code:?}");
    }
    assert_eq!(
        classify_sqlite_error(&anyhow::anyhow!("not a sqlite error at all")),
        SqliteFailureKind::Other
    );
}

#[test]
fn open_rejects_a_corrupt_db_file() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path().join("echo-agent-home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        WorktreeDb::resolve_db_path(&home),
        b"garbage, not an sqlite header",
    )
    .unwrap();
    assert!(
        WorktreeDb::open(&home).is_err(),
        "rebuild opens read-write, so a corrupt file must fail there"
    );
}

#[test]
fn read_only_open_respects_deadline_under_contention() {
    use std::time::{Duration, Instant};
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("worktrees.db");
    // WAL-stamp the exact file the forced-network open will use, then hold a
    // read transaction: the WAL->TRUNCATE conversion needs an exclusive lock,
    // so the retry must give up at the deadline as busy, not as a broken file.
    let eff = JournalMode::Truncate.effective_db_path(&path);
    {
        let conn = rusqlite::Connection::open(&eff).unwrap();
        JournalMode::Wal.apply(&conn).unwrap();
        conn.execute_batch("CREATE TABLE t (v TEXT); INSERT INTO t VALUES ('x');")
            .unwrap();
    }
    let holder = rusqlite::Connection::open(&eff).unwrap();
    holder
        .execute_batch("BEGIN; SELECT COUNT(*) FROM t;")
        .unwrap();

    let short = Instant::now() + Duration::from_millis(500);
    let start = Instant::now();
    let Err(OpenFailure::Busy(_)) =
        WorktreeDb::open_readonly_with_busy_retry(JournalMode::Truncate, &path, short)
    else {
        panic!("a WAL reader holding the DB must classify as busy, never as a broken file");
    };
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "a 500ms deadline must bound the whole open, took {:?}",
        start.elapsed()
    );

    let start = Instant::now();
    let deadline = Instant::now() + BUSY_RETRY_BUDGET;
    let Err(OpenFailure::Busy(err)) =
        WorktreeDb::open_readonly_with_busy_retry(JournalMode::Truncate, &path, deadline)
    else {
        panic!("a WAL reader holding the DB must classify as busy, never as a broken file");
    };
    assert!(
        format!("{err:#}").contains("database busy after"),
        "expected the deadline-busy error, got: {err:#}"
    );
    assert!(
        start.elapsed() < BUSY_RETRY_BUDGET + Duration::from_secs(3),
        "one budget covers the whole open, took {:?}",
        start.elapsed()
    );

    holder.execute_batch("COMMIT;").unwrap();
    drop(holder);
    let conn = WorktreeDb::open_readonly_with_busy_retry(
        JournalMode::Truncate,
        &path,
        Instant::now() + BUSY_RETRY_BUDGET,
    )
    .unwrap();
    let v: String = conn.query_row("SELECT v FROM t", [], |r| r.get(0)).unwrap();
    assert_eq!(v, "x");
}

#[test]
fn read_only_handles_reject_writes_on_both_journal_arms() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path().join("echo-agent-home");
    WorktreeDb::open(&home)
        .unwrap()
        .register(&make_labeled_record("a", "/tmp/wt-a", "x"))
        .unwrap();
    let RegistryOpen::Opened { db: ro, .. } = WorktreeDb::open_read_only(&home) else {
        panic!("an existing DB must open");
    };
    ro.register(&make_labeled_record("b", "/tmp/wt-b", "x"))
        .expect_err("WAL-arm read-only handle must reject writes");

    let path = tmp.path().join("net-home/worktrees.db");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    WorktreeDb::open_at_with_journal_mode(&path, JournalMode::Truncate)
        .unwrap()
        .register(&make_labeled_record("c", "/tmp/wt-c", "x"))
        .unwrap();
    let conn = WorktreeDb::open_readonly_with_busy_retry(
        JournalMode::Truncate,
        &path,
        std::time::Instant::now() + BUSY_RETRY_BUDGET,
    )
    .unwrap();
    let ro = WorktreeDb { conn };
    assert_eq!(ro.list(&ListFilter::default()).unwrap().len(), 1);
    ro.register(&make_labeled_record("d", "/tmp/wt-d", "x"))
        .expect_err("Truncate-arm read-only handle must reject writes");
}

#[test]
fn get_set_meta_round_trip() {
    let db = WorktreeDb::open_in_memory().unwrap();
    assert_eq!(db.get_meta("k").unwrap(), None);
    db.set_meta("k", "v").unwrap();
    assert_eq!(db.get_meta("k").unwrap().as_deref(), Some("v"));
    db.set_meta("k", "v2").unwrap();
    assert_eq!(db.get_meta("k").unwrap().as_deref(), Some("v2"));
}
