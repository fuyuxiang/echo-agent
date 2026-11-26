use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::Duration as StdDuration;

use fs2::FileExt;
use tempfile::TempDir;

use super::super::{Heartbeat, try_lock_auth_file_async};
use super::{Arc, DepositOnDrop, Mutex, Round, Ticket, WAITS, Wait, Weak, join};

fn parked_wait(lock_path: &Path) -> Option<Arc<Wait>> {
    WAITS.lock().unwrap().get(lock_path).and_then(Weak::upgrade)
}

fn deposited_wait(result: std::io::Result<File>) -> Arc<Wait> {
    Arc::new(Wait {
        round: Mutex::new(Round::Deposited(result)),
        notify: tokio::sync::Notify::new(),
    })
}

#[cfg(unix)]
#[tokio::test]
async fn free_lock_claim_never_misses_its_own_round() {
    let dir = TempDir::new().unwrap();
    for round in 0..10 {
        let lock_path = dir.path().join(format!("auth-{round}.json.lock"));
        let ticket = join(&lock_path);
        tokio::time::timeout(StdDuration::from_secs(5), ticket.claim())
            .await
            .expect("free-lock claim must resolve")
            .expect("the creator must claim its own round, not see an unclaimed drop")
            .expect("uncontended blocking acquire must succeed");
    }
}

#[tokio::test]
async fn panicking_wait_thread_deposits_an_error_and_wakes_waiters_promptly() {
    let wait = Arc::new(Wait {
        round: Mutex::new(Round::Waiting),
        notify: tokio::sync::Notify::new(),
    });
    let ticket = Ticket {
        wait: Arc::clone(&wait),
    };
    let deposit = DepositOnDrop { wait, result: None };
    std::thread::spawn(move || {
        let _deposit = deposit;
        panic!("acquire panicked");
    });

    let claimed = tokio::time::timeout(StdDuration::from_secs(5), ticket.claim())
        .await
        .expect("waiters must wake promptly on a panicked wait thread")
        .expect("the failure must be deposited, not lost");
    claimed.expect_err("a panicked acquire must surface as an error");
}

#[test]
fn try_claim_takes_a_deposited_result_exactly_once() {
    let dir = TempDir::new().unwrap();
    let file = File::create(dir.path().join("auth.json.lock")).unwrap();
    let ticket = Ticket {
        wait: deposited_wait(Ok(file)),
    };

    let claimed = ticket
        .try_claim()
        .expect("a deposited result must be claimable before the ticket drops");
    claimed.expect("the deposit must carry the acquired file");
    assert!(
        ticket.try_claim().is_none(),
        "a claim must consume the deposit"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn error_deposit_backs_off_rejoins_fresh_and_still_acquires() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("auth.json");
    let lock_path = path.with_file_name("auth.json.lock");
    let holder = crate::auth::manager::lock::test_support::hold_backdated_stale_lock(&lock_path);

    let planted = Arc::new(Wait {
        round: Mutex::new(Round::Waiting),
        notify: tokio::sync::Notify::new(),
    });
    WAITS
        .lock()
        .unwrap()
        .insert(lock_path.clone(), Arc::downgrade(&planted));
    let depositor = {
        let wait = Arc::clone(&planted);
        std::thread::spawn(move || {
            std::thread::sleep(StdDuration::from_millis(50));
            drop(DepositOnDrop {
                wait,
                result: Some(Err(std::io::Error::other("planted failure"))),
            });
        })
    };
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(StdDuration::from_millis(150));
        drop(holder);
    });

    let lock = try_lock_auth_file_async(&path, StdDuration::from_secs(5), Heartbeat::Skip)
        .await
        .into_guard();
    assert!(
        lock.is_some(),
        "an error round must back off, rejoin fresh, and still acquire"
    );
    depositor.join().expect("depositor thread");
    releaser.join().expect("releaser thread");
}

#[cfg(unix)]
#[tokio::test]
async fn freed_flock_at_the_deadline_is_acquired_through_the_public_api() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("auth.json");
    let lock_path = path.with_file_name("auth.json.lock");
    let holder = crate::auth::manager::lock::test_support::hold_backdated_stale_lock(&lock_path);

    let planted = Arc::new(Wait {
        round: Mutex::new(Round::Waiting),
        notify: tokio::sync::Notify::new(),
    });
    WAITS
        .lock()
        .unwrap()
        .insert(lock_path.clone(), Arc::downgrade(&planted));
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(StdDuration::from_millis(50));
        drop(holder);
    });

    let budget = StdDuration::from_millis(300);
    let started = tokio::time::Instant::now();
    let got = try_lock_auth_file_async(&path, budget, Heartbeat::Skip).await;
    let elapsed = started.elapsed();

    assert!(
        got.into_guard().is_some(),
        "the deadline salvage must acquire the freed flock"
    );
    assert!(
        elapsed >= budget,
        "the salvage must run after the budget expires, took {elapsed:?}"
    );
    releaser.join().expect("releaser thread");
}

#[cfg(unix)]
#[test]
fn err_deposit_at_the_deadline_surfaces_failed_not_timed_out() {
    let dir = TempDir::new().unwrap();
    let lock_path = dir.path().join("auth.json.lock");
    let _holder = crate::auth::manager::lock::test_support::hold_backdated_stale_lock(&lock_path);
    let ticket = Ticket {
        wait: deposited_wait(Err(std::io::Error::other("wait thread failed"))),
    };

    let salvaged = super::super::salvage_at_deadline(Some(&ticket), &lock_path);
    let error = salvaged.expect_err("a deposited failure must not be reclassified as a timeout");
    assert_eq!(error.to_string(), "wait thread failed");
}

#[cfg(unix)]
#[test]
fn ok_deposit_at_the_deadline_is_claimed_not_dropped() {
    let dir = TempDir::new().unwrap();
    let lock_path = dir.path().join("auth.json.lock");
    let mut deposit = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&lock_path)
        .unwrap();
    deposit.try_lock_exclusive().unwrap();
    write!(deposit, "{}:0", std::process::id()).unwrap();
    let ticket = Ticket {
        wait: deposited_wait(Ok(deposit)),
    };

    let salvaged = super::super::salvage_at_deadline(Some(&ticket), &lock_path);
    salvaged
        .expect("a deposited acquisition is not a failure")
        .expect("the deposit must be claimed, not dropped with the ticket");
}

#[cfg(unix)]
#[tokio::test]
async fn concurrent_waiters_share_one_parked_flock_wait() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("auth.json");
    let lock_path = path.with_file_name("auth.json.lock");

    let holder = crate::auth::manager::lock::test_support::hold_backdated_stale_lock(&lock_path);

    let spawn_waiter = |p: std::path::PathBuf| {
        tokio::spawn(async move {
            try_lock_auth_file_async(&p, StdDuration::from_secs(10), Heartbeat::Skip)
                .await
                .into_guard()
                .is_some()
        })
    };
    let waiter_a = spawn_waiter(path.clone());
    let waiter_b = spawn_waiter(path.clone());

    let deadline = tokio::time::Instant::now() + StdDuration::from_secs(5);
    loop {
        let owners = parked_wait(&lock_path)
            .map(|wait| Arc::strong_count(&wait))
            .unwrap_or(0);
        if owners == 4 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "both waiters must subscribe to one shared flock wait \
             (deposit guard + two waiter tickets + this probe), saw {owners} owners"
        );
        tokio::time::sleep(StdDuration::from_millis(10)).await;
    }

    drop(holder);
    let (a, b) = tokio::time::timeout(StdDuration::from_secs(5), async {
        tokio::join!(waiter_a, waiter_b)
    })
    .await
    .expect("both waiters must resolve after the holder releases");
    assert!(
        a.expect("waiter A must not panic") && b.expect("waiter B must not panic"),
        "both waiters must acquire via the shared parked wait"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn timed_out_waiters_reuse_one_parked_flock_wait() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("auth.json");
    let lock_path = path.with_file_name("auth.json.lock");

    let holder = crate::auth::manager::lock::test_support::hold_backdated_stale_lock(&lock_path);

    let first =
        try_lock_auth_file_async(&path, StdDuration::from_millis(300), Heartbeat::Skip).await;
    assert!(
        first.into_guard().is_none(),
        "wedged holder: first waiter must time out"
    );
    let wait_after_first =
        parked_wait(&lock_path).expect("a timed-out waiter must leave the parked wait in place");
    assert!(
        matches!(*wait_after_first.lock_round(), Round::Waiting),
        "the parked wait must still be blocked on the wedged holder"
    );

    let second =
        try_lock_auth_file_async(&path, StdDuration::from_millis(300), Heartbeat::Skip).await;
    assert!(
        second.into_guard().is_none(),
        "wedged holder: second waiter must time out"
    );
    let wait_after_second =
        parked_wait(&lock_path).expect("the parked wait must persist across successive callers");
    assert!(
        Arc::ptr_eq(&wait_after_first, &wait_after_second),
        "successive waiters must reuse the SAME parked wait, not spawn another thread"
    );
    drop(wait_after_first);
    drop(wait_after_second);

    drop(holder);
    let lock = try_lock_auth_file_async(&path, StdDuration::from_secs(2), Heartbeat::Skip)
        .await
        .into_guard();
    assert!(
        lock.is_some(),
        "flock must be free after the unclaimed acquisition is dropped"
    );
}
