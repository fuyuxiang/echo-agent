#[cfg(unix)]
use std::ffi::OsString;

#[cfg(windows)]
use super::windows;
use super::*;

#[cfg(unix)]
#[test]
fn retained_directory_capability_survives_path_replacement() {
    use std::io::Read as _;

    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let original = root.path().join("sessions");
    std::fs::create_dir_all(&original).unwrap();
    std::fs::write(original.join("inside"), "inside").unwrap();
    std::fs::write(outside.path().join("outside"), "outside").unwrap();
    let approved = ApprovedRoot::new(root.path()).unwrap();
    let retained = approved.subroot(&original).unwrap();

    let moved = root.path().join("sessions-original");
    std::fs::rename(&original, &moved).unwrap();
    std::os::unix::fs::symlink(outside.path(), &original).unwrap();

    let mut names = Vec::new();
    assert!(retained.for_each_entry(|name| names.push(name)));
    assert!(names.contains(&OsString::from("inside")));
    assert!(!names.contains(&OsString::from("outside")));
    let mut contents = String::new();
    retained
        .open_regular_file(&retained.join("inside"))
        .unwrap()
        .file
        .read_to_string(&mut contents)
        .unwrap();
    assert_eq!(contents, "inside");
    assert!(
        retained
            .open_regular_file(&retained.join("outside"))
            .is_none()
    );
}

#[cfg(unix)]
#[test]
fn bounded_directory_visit_reports_exact_cutoff() {
    let root = tempfile::tempdir().unwrap();
    for index in 0..5 {
        std::fs::write(root.path().join(format!("entry-{index}")), "").unwrap();
    }
    let approved = ApprovedRoot::new(root.path()).unwrap();
    let mut visited = Vec::new();
    let outcome = approved.for_each_entry_bounded(3, |name| visited.push(name));
    assert_eq!(visited.len(), 3);
    assert_eq!(
        outcome,
        DirectoryVisit {
            visited: 3,
            complete: false,
        }
    );

    let small = tempfile::tempdir().unwrap();
    std::fs::write(small.path().join("a"), "").unwrap();
    std::fs::write(small.path().join("b"), "").unwrap();
    let approved = ApprovedRoot::new(small.path()).unwrap();
    let mut visited = 0;
    assert_eq!(
        approved.for_each_entry_bounded(3, |_| visited += 1),
        DirectoryVisit {
            visited: 2,
            complete: true,
        }
    );

    let exact = tempfile::tempdir().unwrap();
    for index in 0..3 {
        std::fs::write(exact.path().join(format!("entry-{index}")), "").unwrap();
    }
    let approved = ApprovedRoot::new(exact.path()).unwrap();
    let mut visited = 0;
    assert_eq!(
        approved.for_each_entry_bounded(3, |_| visited += 1),
        DirectoryVisit {
            visited: 3,
            complete: true,
        }
    );
    assert_eq!(visited, 3);
}

#[cfg(unix)]
#[test]
fn nonblocking_open_rejects_fifo() {
    use std::os::unix::ffi::OsStrExt as _;

    let root = tempfile::tempdir().unwrap();
    let fifo = root.path().join("metadata");
    let path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: the path is NUL-terminated and points into the live tempdir.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let approved = ApprovedRoot::new(root.path()).unwrap();
    assert!(approved.open_regular_file(&fifo).is_none());
}

#[test]
fn sqlite_truncate_mode_skips_foreign_database_without_mutation() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE values_for_test (value INTEGER);
             INSERT INTO values_for_test VALUES (42);",
        )
        .unwrap();
    drop(connection);
    let contents = std::fs::read(&path).unwrap();

    let approved = ApprovedRoot::new(root.path()).unwrap();
    assert!(
        open_sqlite_transaction_with_journal_mode(&approved, &path, JournalMode::Truncate)
            .is_none()
    );
    assert_eq!(std::fs::read(&path).unwrap(), contents);

    let connection =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let journal_mode: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(journal_mode, "wal");
}

#[test]
fn sqlite_wal_mode_queries_and_pins_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let writer = rusqlite::Connection::open(&path).unwrap();
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA wal_autocheckpoint=0;
             CREATE TABLE values_for_test (value INTEGER);
             PRAGMA wal_checkpoint(TRUNCATE);
             INSERT INTO values_for_test VALUES (42);",
        )
        .unwrap();

    let approved = ApprovedRoot::new(root.path()).unwrap();
    let database =
        open_sqlite_transaction_with_journal_mode(&approved, &path, JournalMode::Wal).unwrap();
    let value: i64 = database
        .query_row("SELECT value FROM values_for_test", [], |row| row.get(0))
        .unwrap();
    assert_eq!(value, 42);
    writer
        .execute("INSERT INTO values_for_test VALUES (43)", [])
        .unwrap();
    let count: i64 = database
        .query_row("SELECT COUNT(*) FROM values_for_test", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
    drop(writer);
}

#[test]
fn sqlite_scanner_connection_is_query_only() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch("CREATE TABLE values_for_test (value INTEGER);")
        .unwrap();
    drop(connection);

    let approved = ApprovedRoot::new(root.path()).unwrap();
    let database = open_sqlite_transaction(&approved, &path).unwrap();
    let query_only: i64 = database
        .query_row("PRAGMA query_only", [], |row| row.get(0))
        .unwrap();
    assert_eq!(query_only, 1);
    assert!(
        database
            .execute("INSERT INTO values_for_test VALUES (1)", [])
            .is_err()
    );
}

#[cfg(windows)]
#[test]
fn windows_open_verifies_stable_file_identity() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first");
    let second = root.path().join("second");
    std::fs::write(&first, "first").unwrap();
    std::fs::write(&second, "second").unwrap();
    let approved = ApprovedRoot::new(root.path()).unwrap();
    let opened = approved.open_regular_file(&first).unwrap();
    let first_again = windows::open_regular_path(&first).unwrap();
    let second = windows::open_regular_path(&second).unwrap();
    assert!(windows::same_file_for_test(&first_again, &opened.file));
    assert!(!windows::same_file_for_test(&second, &opened.file));
    assert!(opened.path.starts_with(approved.path()));
    assert!(windows::final_path_matches_for_test(
        &opened.path,
        &opened.file
    ));
}

#[cfg(windows)]
#[test]
fn windows_directory_scans_fail_closed() {
    let root = tempfile::tempdir().unwrap();
    let child = root.path().join("child");
    std::fs::create_dir_all(&child).unwrap();
    let approved = ApprovedRoot::new(root.path()).unwrap();
    assert!(approved.subroot(&child).is_none());
    assert!(!approved.for_each_entry(|_| panic!("enumerated on Windows")));
}
