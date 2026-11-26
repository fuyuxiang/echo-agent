use super::*;

#[tokio::test]
async fn dropping_the_session_handle_closes_the_actor_channel() {
    let (handle, mut rx, summary_tx, _disk_full_tx) = actor_channel();

    drop(handle);

    assert!(
        summary_tx.upgrade().is_none(),
        "the generator's sender must not keep the channel open"
    );
    assert!(
        rx.recv().await.is_none(),
        "the actor's receive loop must end once the session drops its handle"
    );
}
