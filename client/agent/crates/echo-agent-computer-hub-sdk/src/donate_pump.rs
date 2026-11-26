//! Shared donation transport: a bounded retry buffer + in-order drain
//! barrier, parameterized over a `donate` closure. Traces, logs, and
//! metrics all pump through this; failed sends are retained briefly,
//! overflow drops payloads — telemetry, never correctness.

use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
use opentelemetry_proto::tonic::resource::v1::Resource;
use tokio::sync::{mpsc, oneshot};

/// Bound on payloads queued before the pump drains them.
pub(crate) const PENDING_FLUSHES: usize = 8;
/// Payloads retained across failed sends (disconnect/reconnect window).
pub(crate) const RETRY_CAP: usize = 8;

// ---------------------------------------------------------------------------
// Shared OTLP encoding helpers
//
// Reused by the log and metric donation clients so the AnyValue/KeyValue/
// Resource construction lives in one place instead of being copy-pasted per
// client. (`trace_donate` builds its payload via `opentelemetry_sdk`'s own
// conversion and does not use these.)
// ---------------------------------------------------------------------------

/// Current wall-clock time as Unix-epoch nanoseconds (OTLP `time_unix_nano`).
pub(crate) fn now_unix_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// OTLP string `AnyValue`.
pub(crate) fn string_value(s: String) -> AnyValue {
    AnyValue {
        value: Some(any_value::Value::StringValue(s)),
    }
}

/// OTLP string-valued `KeyValue`.
pub(crate) fn string_kv(key: &str, value: String) -> KeyValue {
    KeyValue {
        key: key.to_owned(),
        value: Some(string_value(value)),
        ..Default::default()
    }
}

/// OTLP `Resource` carrying just `service.name`.
pub(crate) fn make_resource(service_name: String) -> Resource {
    Resource {
        attributes: vec![string_kv("service.name", service_name)],
        ..Default::default()
    }
}

pub(crate) enum PumpMsg {
    /// Base64 OTLP request, ready for the wire.
    Payload(String),
    /// In-order drain fence — a barrier, not a timeout.
    Barrier(oneshot::Sender<()>),
}

/// Resolves once every payload queued before this call has had a send
/// attempt. Call after the producer's flush (e.g. `fastrace::flush()`).
pub(crate) async fn drain_via(tx: &mpsc::Sender<PumpMsg>) {
    let (ack_tx, ack_rx) = oneshot::channel();
    if tx.send(PumpMsg::Barrier(ack_tx)).await.is_ok() {
        let _ = ack_rx.await;
    }
}

/// `donate` hands the payload back so a failed send retains it
/// without cloning.
pub(crate) async fn run_pump<D, F>(mut rx: mpsc::Receiver<PumpMsg>, donate: D)
where
    D: Fn(String) -> F,
    F: std::future::Future<Output = (bool, String)>,
{
    let mut retry: VecDeque<String> = VecDeque::new();
    while let Some(msg) = rx.recv().await {
        match msg {
            PumpMsg::Payload(payload) => {
                if retry.len() == RETRY_CAP {
                    retry.pop_front();
                    tracing::debug!("donation retry buffer full; dropping oldest payload");
                }
                retry.push_back(payload);
            }
            PumpMsg::Barrier(ack) => {
                attempt_sends(&mut retry, &donate).await;
                let _ = ack.send(());
                continue;
            }
        }
        attempt_sends(&mut retry, &donate).await;
    }
}

/// Send in order, stopping at the first failure; the remainder stays
/// queued for the next wake.
async fn attempt_sends<D, F>(retry: &mut VecDeque<String>, donate: &D)
where
    D: Fn(String) -> F,
    F: std::future::Future<Output = (bool, String)>,
{
    while let Some(payload) = retry.pop_front() {
        let (ok, payload) = donate(payload).await;
        if !ok {
            tracing::debug!("donation send failed; retaining payload for retry");
            retry.push_front(payload);
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use parking_lot::Mutex;

    use super::*;

    fn payload(tag: u64) -> PumpMsg {
        PumpMsg::Payload(format!("payload-{tag}"))
    }

    /// The drain barrier acks even while the link is down.
    #[tokio::test]
    async fn pump_retries_failed_payloads_across_reconnect() {
        let healthy = Arc::new(AtomicBool::new(false));
        let sent: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = mpsc::channel::<PumpMsg>(PENDING_FLUSHES);
        let pump = {
            let healthy = Arc::clone(&healthy);
            let sent = Arc::clone(&sent);
            tokio::spawn(run_pump(rx, move |p: String| {
                let healthy = Arc::clone(&healthy);
                let sent = Arc::clone(&sent);
                async move {
                    if healthy.load(Ordering::SeqCst) {
                        sent.lock().push(p.clone());
                        (true, p)
                    } else {
                        (false, p)
                    }
                }
            }))
        };

        tx.send(payload(1)).await.unwrap();
        tx.send(payload(2)).await.unwrap();
        drain_via(&tx).await;
        assert!(sent.lock().is_empty(), "nothing sent while link is down");

        healthy.store(true, Ordering::SeqCst);
        drain_via(&tx).await;
        assert_eq!(*sent.lock(), vec!["payload-1", "payload-2"]);

        drop(tx);
        pump.await.expect("pump must exit cleanly");
    }

    #[tokio::test]
    async fn pump_retry_buffer_drops_oldest_beyond_cap() {
        let sent: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let healthy = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel::<PumpMsg>(RETRY_CAP + 2);
        let pump = {
            let healthy = Arc::clone(&healthy);
            let sent = Arc::clone(&sent);
            tokio::spawn(run_pump(rx, move |p: String| {
                let healthy = Arc::clone(&healthy);
                let sent = Arc::clone(&sent);
                async move {
                    if healthy.load(Ordering::SeqCst) {
                        sent.lock().push(p.clone());
                        (true, p)
                    } else {
                        (false, p)
                    }
                }
            }))
        };

        for i in 0..=(RETRY_CAP as u64) {
            tx.send(payload(i + 1)).await.unwrap();
        }
        drain_via(&tx).await;

        healthy.store(true, Ordering::SeqCst);
        drain_via(&tx).await;
        {
            let sent = sent.lock();
            assert_eq!(sent.len(), RETRY_CAP, "buffer bounded at RETRY_CAP");
            assert_eq!(sent[0], "payload-2", "oldest payload evicted first");
        }

        drop(tx);
        pump.await.expect("pump must exit cleanly");
    }
}
