//! Models the `Round` mutex protocol and the shipped `subscribe_if_waiting` peek;
//! Notify wakeups and `Arc`/`Weak` counts are pinned by the tokio tests instead.
//! Run: `cargo test --release --features loom -p echo-agent-runtime --lib flock_wait::loom -- --test-threads=1`

use std::io;
use std::sync::Arc;

use super::{DepositOnDrop, Mutex, Round, Ticket, Wait, subscribe_if_waiting};

fn waiting_wait() -> Arc<Wait> {
    Arc::new(Wait {
        round: Mutex::new(Round::Waiting),
        notify: tokio::sync::Notify::new(),
    })
}

fn deposit_err(wait: Arc<Wait>) {
    drop(DepositOnDrop {
        wait,
        result: Some(Err(io::Error::other("model deposit"))),
    });
}

#[test]
fn loom_try_claim_consumes_the_deposit_exactly_once() {
    loom::model(|| {
        let wait = waiting_wait();
        let ticket_a = Ticket {
            wait: Arc::clone(&wait),
        };
        let ticket_b = Ticket {
            wait: Arc::clone(&wait),
        };
        let depositor = loom::thread::spawn(move || deposit_err(wait));
        let claimer = loom::thread::spawn(move || {
            let won = ticket_b.try_claim().is_some();
            (ticket_b, won)
        });

        let a_won = ticket_a.try_claim().is_some();
        let (ticket_b, b_won) = claimer.join().expect("claimer thread");
        depositor.join().expect("depositor thread");
        let leftover_a = ticket_a.try_claim().is_some();
        let leftover_b = ticket_b.try_claim().is_some();

        let claims =
            u8::from(a_won) + u8::from(b_won) + u8::from(leftover_a) + u8::from(leftover_b);
        assert_eq!(
            claims, 1,
            "the deposit must be claimed exactly once: never lost, never doubled"
        );
    });
}

#[test]
fn loom_subscribe_peek_rides_a_waiting_wait_or_never_strands_the_deposit() {
    loom::model(|| {
        let wait = waiting_wait();
        let registry_entry = Arc::downgrade(&wait);
        let ticket = Ticket {
            wait: Arc::clone(&wait),
        };
        let depositor = loom::thread::spawn(move || deposit_err(wait));

        let joined = subscribe_if_waiting(&registry_entry);

        depositor.join().expect("depositor thread");
        match joined {
            Some(late_ticket) => assert!(
                late_ticket.try_claim().is_some(),
                "a wait joined while Waiting must deliver its deposit"
            ),
            None => assert!(
                ticket.try_claim().is_some(),
                "a refused join means the deposit was already visible to the ticket"
            ),
        }
    });
}
