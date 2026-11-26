//! Integration tests using the public API only. Each test exercises the
//! real OS watcher against a `tempfile`-rooted fake git repo.
//!
//! These can be flaky on some CI runners where FS events aren't reliably
//! delivered (matches the existing pattern in `watcher.rs` integration
//! tests). Marked `#[ignore]` for now; run locally with
//! `cargo test --test integration -- --ignored`.

use std::fs;
use std::time::Duration;

use echo_agent_fsnotify::{FsConfig, FsEvent, FsEventKind, FsEventSource};
use serial_test::serial;
use tempfile::TempDir;
use tokio::sync::broadcast;
use tokio::time::timeout;

fn fake_git_repo() -> TempDir {
    let temp = TempDir::new().unwrap();
    let git_dir = temp.path().join(".git");
    fs::create_dir(&git_dir).unwrap();
    fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    fs::create_dir_all(git_dir.join("objects")).unwrap();
    fs::create_dir_all(git_dir.join("refs")).unwrap();
    temp
}

fn fake_sl_repo() -> TempDir {
    let temp = TempDir::new().unwrap();
    let sl_dir = temp.path().join(".sl");
    fs::create_dir(&sl_dir).unwrap();
    fs::write(sl_dir.join("dirstate"), sl_dirstate(0x11)).unwrap();
    temp
}

/// `.sl/dirstate` = p1(20) ‖ p2(NULL_ID, 20) ‖ "\ntreestate\n"; only the
/// leading p1 (working-copy parent) is read by the source.
fn sl_dirstate(p1_byte: u8) -> Vec<u8> {
    let mut v = vec![p1_byte; 20];
    v.extend_from_slice(&[0u8; 20]);
    v.extend_from_slice(b"\ntreestate\n");
    v
}

async fn recv_until(
    rx: &mut broadcast::Receiver<FsEvent>,
    pred: impl Fn(&FsEvent) -> bool,
) -> FsEvent {
    loop {
        match rx.recv().await {
            Ok(e) if pred(&e) => return e,
            Ok(_) => continue,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => panic!("channel closed"),
        }
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "flaky in CI; FS events not reliably delivered"]
async fn source_emits_files_changed() {
    let temp = fake_git_repo();
    let source = FsEventSource::start(temp.path().to_path_buf(), {
        let mut c = FsConfig::default();
        c.debounce_ms = 50;
        c
    })
    .unwrap();
    let mut rx = source.subscribe();

    tokio::time::sleep(Duration::from_millis(200)).await;
    fs::write(temp.path().join("hello.txt"), "world").unwrap();

    let event = timeout(
        Duration::from_secs(2),
        recv_until(&mut rx, |e| matches!(e, FsEvent::FilesChanged { .. })),
    )
    .await
    .unwrap();
    match event {
        FsEvent::FilesChanged { kind, paths } => {
            assert!(matches!(kind, FsEventKind::Created | FsEventKind::Modified));
            assert!(paths.iter().any(|p| p.ends_with("hello.txt")));
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "flaky in CI; FS events not reliably delivered"]
async fn source_emits_git_op_started_and_completed_no_head_change() {
    let temp = fake_git_repo();
    let source = FsEventSource::start(temp.path().to_path_buf(), {
        let mut c = FsConfig::default();
        c.debounce_ms = 50;
        c
    })
    .unwrap();
    let mut rx = source.subscribe();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let lock = temp.path().join(".git/index.lock");
    fs::write(&lock, "").unwrap();

    let _ = timeout(
        Duration::from_secs(2),
        recv_until(&mut rx, |e| matches!(e, FsEvent::GitOperationStarted)),
    )
    .await
    .unwrap();

    fs::remove_file(&lock).unwrap();

    let completed = timeout(
        Duration::from_secs(2),
        recv_until(&mut rx, |e| {
            matches!(e, FsEvent::GitOperationCompleted { .. })
        }),
    )
    .await
    .unwrap();
    match completed {
        FsEvent::GitOperationCompleted { head_changed } => assert!(!head_changed),
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "flaky in CI; FS events not reliably delivered"]
async fn source_emits_completed_with_head_change_on_branch_switch() {
    let temp = fake_git_repo();
    let source = FsEventSource::start(temp.path().to_path_buf(), {
        let mut c = FsConfig::default();
        c.debounce_ms = 50;
        c
    })
    .unwrap();
    let mut rx = source.subscribe();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let lock = temp.path().join(".git/index.lock");
    fs::write(&lock, "").unwrap();
    let _ = timeout(
        Duration::from_secs(2),
        recv_until(&mut rx, |e| matches!(e, FsEvent::GitOperationStarted)),
    )
    .await
    .unwrap();

    fs::write(temp.path().join(".git/HEAD"), "ref: refs/heads/feature\n").unwrap();
    fs::remove_file(&lock).unwrap();

    let event = timeout(
        Duration::from_secs(2),
        recv_until(&mut rx, |e| {
            matches!(e, FsEvent::GitOperationCompleted { .. })
        }),
    )
    .await
    .unwrap();
    match event {
        FsEvent::GitOperationCompleted { head_changed } => assert!(head_changed),
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "flaky in CI; FS events not reliably delivered"]
async fn source_emits_sl_op_started_and_completed_no_head_change() {
    let temp = fake_sl_repo();
    let source = FsEventSource::start(temp.path().to_path_buf(), {
        let mut c = FsConfig::default();
        c.debounce_ms = 50;
        c
    })
    .unwrap();
    let mut rx = source.subscribe();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let wlock = temp.path().join(".sl/wlock");
    fs::write(&wlock, "").unwrap();
    let _ = timeout(
        Duration::from_secs(2),
        recv_until(&mut rx, |e| matches!(e, FsEvent::GitOperationStarted)),
    )
    .await
    .unwrap();

    // Release without moving p1 (e.g. a dirty-treestate `sl status`).
    fs::remove_file(&wlock).unwrap();
    let completed = timeout(
        Duration::from_secs(2),
        recv_until(&mut rx, |e| {
            matches!(e, FsEvent::GitOperationCompleted { .. })
        }),
    )
    .await
    .unwrap();
    match completed {
        FsEvent::GitOperationCompleted { head_changed } => assert!(!head_changed),
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "flaky in CI; FS events not reliably delivered"]
async fn source_emits_completed_with_head_change_on_sl_goto() {
    let temp = fake_sl_repo();
    let source = FsEventSource::start(temp.path().to_path_buf(), {
        let mut c = FsConfig::default();
        c.debounce_ms = 50;
        c
    })
    .unwrap();
    let mut rx = source.subscribe();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let wlock = temp.path().join(".sl/wlock");
    fs::write(&wlock, "").unwrap();
    let _ = timeout(
        Duration::from_secs(2),
        recv_until(&mut rx, |e| matches!(e, FsEvent::GitOperationStarted)),
    )
    .await
    .unwrap();

    // Move the working-copy parent (p1) before releasing wlock, then release.
    // `read_head` reads the new p1 on demand when the wlock-removal is processed.
    fs::write(temp.path().join(".sl/dirstate"), sl_dirstate(0x22)).unwrap();
    fs::remove_file(&wlock).unwrap();

    let event = timeout(
        Duration::from_secs(2),
        recv_until(&mut rx, |e| {
            matches!(e, FsEvent::GitOperationCompleted { .. })
        }),
    )
    .await
    .unwrap();
    match event {
        FsEvent::GitOperationCompleted { head_changed } => assert!(head_changed),
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn shared_dedupes_by_directory() {
    use std::sync::Arc;

    let temp = TempDir::new().unwrap();
    let path = temp.path().to_path_buf();

    // First call creates the watcher; subsequent calls for the same canonical
    // directory hand back clones of the *same* source rather than opening a
    // new OS watch. Skip gracefully where the OS denies watches (CI limits).
    let Ok(a) = echo_agent_fsnotify::shared(path.clone(), FsConfig::default()) else {
        eprintln!("skipping: OS watcher unavailable (resource limit?)");
        return;
    };
    let before = echo_agent_fsnotify::stats();
    let b = echo_agent_fsnotify::shared(path.clone(), FsConfig::default()).unwrap();
    assert!(Arc::ptr_eq(&a, &b), "same dir must share one watcher");
    assert_eq!(
        Arc::strong_count(&a),
        2,
        "second shared() must clone the existing source, not create a new one"
    );

    // The reuse must be counted as a cache hit (no new OS watcher created).
    let after = echo_agent_fsnotify::stats();
    assert_eq!(
        after.reused_total - before.reused_total,
        1,
        "reuse must increment reused_total"
    );
    assert_eq!(
        after.created_total, before.created_total,
        "reuse must not create a new watcher"
    );
    assert!(after.live_watchers >= 1, "the shared watcher must be live");

    // A different directory gets its own independent watcher (a real miss).
    let other = TempDir::new().unwrap();
    let c = echo_agent_fsnotify::shared(other.path().to_path_buf(), FsConfig::default()).unwrap();
    assert!(!Arc::ptr_eq(&a, &c), "different dirs must not share");
    assert_eq!(
        echo_agent_fsnotify::stats().created_total - after.created_total,
        1,
        "a new directory must create a new watcher"
    );

    // Once the last sharer drops, the registry entry is reclaimed and a later
    // request rebuilds a fresh source (exercises the recreate-after-drop path).
    drop(a);
    drop(b);
    let d = echo_agent_fsnotify::shared(path, FsConfig::default()).unwrap();
    assert_eq!(
        Arc::strong_count(&d),
        1,
        "after all sharers drop, shared() must build a new source"
    );
}

/// Runnable measurement: simulates many sessions/subagents all watching the
/// same working directory and prints how many OS watchers were saved.
///
/// ```bash
/// cargo test -p echo-agent-fsnotify --test integration \
///   shared_watcher_scaling_demo -- --nocapture
/// ```
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn shared_watcher_scaling_demo() {
    const SESSIONS: usize = 50;

    let temp = TempDir::new().unwrap();
    let path = temp.path().to_path_buf();

    let before = echo_agent_fsnotify::stats();
    // Hold every handle alive, mirroring N concurrent sessions on one cwd.
    let mut handles = Vec::with_capacity(SESSIONS);
    for _ in 0..SESSIONS {
        let Ok(src) = echo_agent_fsnotify::shared(path.clone(), FsConfig::default()) else {
            eprintln!("skipping: OS watcher unavailable (resource limit?)");
            return;
        };
        handles.push(src);
    }
    let after = echo_agent_fsnotify::stats();

    let created = after.created_total - before.created_total;
    let reused = after.reused_total - before.reused_total;
    println!(
        "shared watcher scaling: {SESSIONS} sessions on one cwd -> \
         created={created}, reused(saved)={reused}, live_watchers={}",
        after.live_watchers
    );
    println!(
        "  before sharing this needed {SESSIONS} OS watchers; after sharing it needs {created}."
    );

    // One real OS watch for the whole fleet; the rest are cache hits.
    assert_eq!(created, 1, "all sessions on one cwd share a single watcher");
    assert_eq!(reused, (SESSIONS - 1) as u64);
    assert!(after.live_watchers >= 1);
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "flaky in CI; FS events not reliably delivered"]
async fn source_works_in_non_git_workspace() {
    let temp = TempDir::new().unwrap();
    let source = FsEventSource::start(temp.path().to_path_buf(), {
        let mut c = FsConfig::default();
        c.debounce_ms = 50;
        c
    })
    .unwrap();
    let mut rx = source.subscribe();
    tokio::time::sleep(Duration::from_millis(200)).await;

    fs::write(temp.path().join("hi.txt"), "x").unwrap();
    let event = timeout(
        Duration::from_secs(2),
        recv_until(&mut rx, |e| matches!(e, FsEvent::FilesChanged { .. })),
    )
    .await
    .unwrap();
    assert!(matches!(event, FsEvent::FilesChanged { .. }));
}
