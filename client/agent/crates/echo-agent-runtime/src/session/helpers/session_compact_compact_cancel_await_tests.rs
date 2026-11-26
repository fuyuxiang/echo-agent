use super::*;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn pre_cancelled_token_skips_fut() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    let err = await_unless_cancelled(&cancel, async {
        panic!("fut must not run when already cancelled");
    })
    .await
    .unwrap_err();
    assert!(matches!(err, CompactFailure::Cancelled));
}

#[tokio::test]
async fn cancel_aborts_pending_open() {
    let cancel = CancellationToken::new();
    let cancel2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel2.cancel();
    });
    let started = std::time::Instant::now();
    let err = await_unless_cancelled(&cancel, async {
        tokio::time::sleep(Duration::from_secs(30)).await;
        0u8
    })
    .await
    .unwrap_err();
    assert!(matches!(err, CompactFailure::Cancelled));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "stop must abort stream-open wait, elapsed {:?}",
        started.elapsed()
    );
}
