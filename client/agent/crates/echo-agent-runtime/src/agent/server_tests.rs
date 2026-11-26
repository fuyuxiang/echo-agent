//! Slot-lifecycle tests for the persistent agent boot path.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::timeout;

use super::{AgentSlot, BootSlotGuard, fail_boot, reclaim_abandoned_boot};

fn booting(boot_id: u64) -> (tokio::sync::watch::Sender<()>, AgentSlot) {
    let (boot_tx, boot_rx) = tokio::sync::watch::channel(());
    (
        boot_tx,
        AgentSlot::Booting {
            boot_id,
            rx: boot_rx,
        },
    )
}

fn boot_rx(slot: &AgentSlot) -> tokio::sync::watch::Receiver<()> {
    match slot {
        AgentSlot::Booting { rx, .. } => rx.clone(),
        _ => panic!("expected Booting"),
    }
}

#[tokio::test]
async fn dropped_boot_sender_reclaims_booting_slot() {
    let (boot_tx, slot_val) = booting(1);
    let slot = tokio::sync::Mutex::new(slot_val);
    let rx = boot_rx(&*slot.lock().await);
    drop(boot_tx);

    timeout(Duration::from_secs(1), reclaim_abandoned_boot(&slot, rx, 1))
        .await
        .expect("reclaim must not wait after the sender is gone");
    assert!(matches!(*slot.lock().await, AgentSlot::Down));
}

#[tokio::test]
async fn boot_guard_drop_resets_booting_and_wakes_waiter() {
    let (boot_tx, slot_val) = booting(1);
    let slot = Arc::new(tokio::sync::Mutex::new(slot_val));
    let rx = boot_rx(&*slot.lock().await);

    let waiter = {
        let slot = Arc::clone(&slot);
        tokio::spawn(async move { reclaim_abandoned_boot(&slot, rx, 1).await })
    };

    drop(BootSlotGuard::new(&slot, boot_tx, 1));
    timeout(Duration::from_secs(1), waiter)
        .await
        .expect("waiter must observe the dropped boot sender")
        .expect("waiter task");
    assert!(matches!(*slot.lock().await, AgentSlot::Down));
}

#[tokio::test]
async fn boot_guard_does_not_clobber_up_after_notify() {
    let (boot_tx, slot_val) = booting(1);
    let (conn_tx, _conn_rx) = mpsc::unbounded_channel();
    let slot = tokio::sync::Mutex::new(slot_val);
    let mut guard = BootSlotGuard::new(&slot, boot_tx, 1);
    *slot.lock().await = AgentSlot::Up(conn_tx);
    guard.notify_waiters();
    drop(guard);
    assert!(matches!(*slot.lock().await, AgentSlot::Up(_)));
}

#[tokio::test]
async fn waiter_keeps_up_when_sender_drops_after_success() {
    let (boot_tx, slot_val) = booting(1);
    let (conn_tx, _conn_rx) = mpsc::unbounded_channel();
    let slot = tokio::sync::Mutex::new(slot_val);
    let rx = boot_rx(&*slot.lock().await);
    *slot.lock().await = AgentSlot::Up(conn_tx);
    drop(boot_tx);

    timeout(Duration::from_secs(1), reclaim_abandoned_boot(&slot, rx, 1))
        .await
        .expect("reclaim must return");
    assert!(matches!(*slot.lock().await, AgentSlot::Up(_)));
}

#[tokio::test]
async fn stale_reclaim_does_not_clobber_newer_boot() {
    let (old_tx, old) = booting(1);
    let slot = tokio::sync::Mutex::new(old);
    let old_rx = boot_rx(&*slot.lock().await);
    let (_new_tx, new) = booting(2);
    *slot.lock().await = new;
    drop(old_tx);

    timeout(
        Duration::from_secs(1),
        reclaim_abandoned_boot(&slot, old_rx, 1),
    )
    .await
    .expect("stale reclaim must return");
    assert!(matches!(
        *slot.lock().await,
        AgentSlot::Booting { boot_id: 2, .. }
    ));
}

#[tokio::test]
async fn stale_guard_drop_does_not_clobber_newer_boot() {
    let (old_tx, old) = booting(1);
    let slot = tokio::sync::Mutex::new(old);
    let guard = BootSlotGuard::new(&slot, old_tx, 1);
    let (_new_tx, new) = booting(2);
    *slot.lock().await = new;
    drop(guard);
    assert!(matches!(
        *slot.lock().await,
        AgentSlot::Booting { boot_id: 2, .. }
    ));
}

#[tokio::test]
async fn stale_fail_boot_does_not_clobber_newer_boot() {
    let (_old_tx, old) = booting(1);
    let slot = tokio::sync::Mutex::new(old);
    let (_new_tx, new) = booting(2);
    *slot.lock().await = new;
    let _ = fail_boot(&slot, 1).await;
    assert!(matches!(
        *slot.lock().await,
        AgentSlot::Booting { boot_id: 2, .. }
    ));
}

#[tokio::test]
async fn matching_fail_boot_resets_slot() {
    let (_tx, val) = booting(3);
    let slot = tokio::sync::Mutex::new(val);
    let _ = fail_boot(&slot, 3).await;
    assert!(matches!(*slot.lock().await, AgentSlot::Down));
}
