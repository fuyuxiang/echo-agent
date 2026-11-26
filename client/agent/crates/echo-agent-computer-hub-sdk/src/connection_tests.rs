use super::*;
/// Window expected for the default table, using *literal* 1 s / 10 s
/// so a change to [`RECONNECT_SPREAD_FLOOR`] or the cap is a
/// deliberate test edit, not a tautology on the constant.
fn expected_default_window(attempt: u32) -> Duration {
    let slots_ms = [100_u64, 200, 500, 1_000, 2_000, 5_000, 10_000];
    let idx = (attempt as usize).saturating_sub(1).min(slots_ms.len() - 1);
    let base = Duration::from_millis(slots_ms[idx]);
    let floor = Duration::from_secs(1);
    let cap = Duration::from_secs(10);
    Duration::from_nanos(
        duration_nanos_u64(base)
            .max(duration_nanos_u64(floor))
            .min(duration_nanos_u64(cap)),
    )
}
fn assert_window_is(attempt: u32, schedule: &[Duration], expected: Duration) {
    let mut max = Duration::ZERO;
    for seed in 0..256_u64 {
        let d = backoff_for(attempt, schedule, seed, 1);
        assert!(
            d < expected || expected.is_zero(),
            "attempt {attempt}: {d:?} escapes Uniform[0, {expected:?})"
        );
        if d > max {
            max = d;
        }
    }
    if !expected.is_zero() {
        assert!(
            max >= expected * 4 / 5,
            "attempt {attempt}: max {max:?} never approaches the {expected:?} window"
        );
    }
}
#[test]
fn spread_floor_and_reset_dwell_are_the_documented_literals() {
    assert_eq!(RECONNECT_SPREAD_FLOOR, Duration::from_secs(1));
    assert_eq!(RECONNECT_ATTEMPT_RESET_AFTER, Duration::from_secs(10));
    assert_eq!(
        RECONNECT_BACKOFF_MS,
        &[100, 200, 500, 1_000, 2_000, 5_000, 10_000]
    );
}
#[test]
fn backoff_for_follows_exponential_schedule() {
    let schedule = default_reconnect_backoff();
    for attempt in 1_u32..=7 {
        assert_window_is(attempt, &schedule, expected_default_window(attempt));
    }
}
#[test]
fn backoff_for_caps_at_last_slot() {
    let schedule = default_reconnect_backoff();
    let cap = Duration::from_secs(10);
    for attempt in [8_u32, 50, u32::MAX] {
        assert_window_is(attempt, &schedule, cap);
    }
}
#[test]
fn backoff_for_zero_attempt_uses_first_slot_window() {
    let schedule = default_reconnect_backoff();
    assert_window_is(0, &schedule, expected_default_window(1));
}
#[test]
fn backoff_for_honors_configured_schedule() {
    let schedule = resolve_reconnect_backoff(Some(Arc::from([
        Duration::from_millis(5),
        Duration::from_millis(15),
    ])));
    let cap = Duration::from_millis(15);
    for attempt in [1_u32, 2, 3, 99] {
        assert_window_is(attempt, &schedule, cap);
    }
}
#[test]
fn resolve_attempt_reset_after_none_is_ten_seconds() {
    assert_eq!(
        resolve_attempt_reset_after(None),
        Duration::from_secs(10),
        "unconfigured (production) path must be the 10s dwell, not ZERO"
    );
    assert_eq!(
        resolve_attempt_reset_after(Some(Duration::ZERO)),
        Duration::ZERO,
        "Some(ZERO) is honored verbatim"
    );
    assert_eq!(
        resolve_attempt_reset_after(Some(Duration::from_secs(3))),
        Duration::from_secs(3)
    );
}
#[test]
fn backoff_for_empty_schedule_is_zero_not_panic() {
    assert_eq!(backoff_for(1, &[], 1, 1), Duration::ZERO);
    assert_eq!(backoff_for(0, &[], 1, 1), Duration::ZERO);
    assert_eq!(backoff_for(u32::MAX, &[], 1, 1), Duration::ZERO);
}
#[test]
fn resolve_reconnect_backoff_falls_back_when_unset_or_empty() {
    let from_none = resolve_reconnect_backoff(None);
    let from_empty = resolve_reconnect_backoff(Some(Arc::from([])));
    let expected: &[Duration] = &[
        Duration::from_millis(100),
        Duration::from_millis(200),
        Duration::from_millis(500),
        Duration::from_millis(1_000),
        Duration::from_millis(2_000),
        Duration::from_millis(5_000),
        Duration::from_millis(10_000),
    ];
    assert_eq!(&*from_none, expected);
    assert_eq!(&*from_empty, expected);
}
#[test]
fn apply_reconnect_jitter_is_uniform_half_open() {
    let one_sec_ns = 1_000_000_000_u64;
    let one_sec = Duration::from_nanos(one_sec_ns);
    assert_eq!(apply_reconnect_jitter(one_sec, 0), Duration::ZERO);
    assert_eq!(
        apply_reconnect_jitter(one_sec, one_sec_ns - 1),
        Duration::from_nanos(one_sec_ns - 1)
    );
    assert_eq!(apply_reconnect_jitter(one_sec, one_sec_ns), Duration::ZERO);
    assert_eq!(apply_reconnect_jitter(Duration::ZERO, 99), Duration::ZERO);
}
#[test]
fn first_slot_window_is_one_second_not_relative_dither() {
    let schedule = default_reconnect_backoff();
    let mut max = Duration::ZERO;
    let mut min = Duration::from_secs(10);
    for seed in 0..256_u64 {
        let d = backoff_for(1, &schedule, seed, 1);
        assert!(d < Duration::from_secs(1), "{d:?} escapes the 1s window");
        if d > max {
            max = d;
        }
        if d < min {
            min = d;
        }
    }
    assert!(
        max > Duration::from_millis(125),
        "max {max:?} fits inside ±25% of 100 ms; spread floor is missing"
    );
    assert!(
        max >= Duration::from_millis(800),
        "max {max:?} does not reach the top of a 1s window"
    );
    assert!(
        min < Duration::from_millis(200),
        "min {min:?} should be near 0"
    );
}
#[test]
fn backoff_jitter_dephases_clients_including_first_attempt() {
    let schedule = default_reconnect_backoff();
    for attempt in [0_u32, 1, 7, 8] {
        let window = expected_default_window(attempt.max(1));
        let mut seen = std::collections::HashSet::new();
        for seed in 0..64_u64 {
            let d = backoff_for(attempt, &schedule, seed, 1);
            assert!(
                d < window || window.is_zero(),
                "{d:?} not in Uniform[0, {window:?})"
            );
            seen.insert(d.as_nanos());
        }
        assert!(
            seen.len() >= 60,
            "attempt {attempt}: only {} distinct delays over 64 seeds",
            seen.len()
        );
    }
}
#[test]
fn backoff_jitter_sequences_differ_across_clients_and_attempts() {
    let schedule = default_reconnect_backoff();
    let seq = |seed: u64, outage: u32| -> Vec<u128> {
        (0..=8)
            .map(|attempt| backoff_for(attempt, &schedule, seed, outage).as_nanos())
            .collect()
    };
    let mut seen = std::collections::HashSet::new();
    for seed in 0..32_u64 {
        seen.insert(seq(seed, 1));
    }
    assert_eq!(seen.len(), 32, "each seed must produce a unique sequence");
    assert_eq!(seq(42, 1), seq(42, 1), "same seed+outage is deterministic");
    let fracs: std::collections::HashSet<u128> = (1..=4)
        .map(|a| backoff_for(a, &schedule, 42, 1).as_nanos())
        .collect();
    assert!(
        fracs.len() >= 3,
        "jitter must re-roll per attempt, not once per client (got {})",
        fracs.len()
    );
    let rephased = (0..32_u64)
        .filter(|&seed| backoff_for(1, &schedule, seed, 1) != backoff_for(1, &schedule, seed, 2))
        .count();
    assert!(
        rephased >= 30,
        "outage index must re-phase delays (only {rephased}/32 moved)"
    );
}
#[test]
fn backoff_jitter_last_slot_has_no_cap_pileup() {
    let schedule = default_reconnect_backoff();
    let cap = Duration::from_secs(10);
    let mut seen = std::collections::HashSet::new();
    for seed in 0..128_u64 {
        let d = backoff_for(7, &schedule, seed, 1);
        assert!(
            d < cap,
            "{d:?} must be in Uniform[0, cap), not piled on cap"
        );
        seen.insert(d.as_nanos());
    }
    assert!(
        seen.len() >= 120,
        "last-slot full jitter collapsed to {} values",
        seen.len()
    );
}
#[test]
fn derive_jitter_seed_mixes_counter_pid_and_clock() {
    assert_ne!(
        derive_jitter_seed(1, 42, 1_000),
        derive_jitter_seed(2, 42, 1_000),
        "counter must de-phase same-instant same-pid constructors"
    );
    assert_ne!(
        derive_jitter_seed(1, 42, 1_000),
        derive_jitter_seed(1, 43, 1_000),
        "pid must de-phase counter=1 across processes"
    );
    assert_ne!(
        derive_jitter_seed(1, 42, 1_000),
        derive_jitter_seed(1, 42, 2_000),
        "clock nanos must enter the mix"
    );
}
#[test]
fn new_seed_mixes_in_pid_and_clock_not_just_the_counter() {
    let before = NEXT_RECONNECT_JITTER_SEED.load(Ordering::Relaxed);
    let pid = u64::from(std::process::id());
    let s = new_reconnect_jitter_seed();
    let after = NEXT_RECONNECT_JITTER_SEED.load(Ordering::Relaxed);
    let ns = before..after.max(before + 1);
    assert!(
        !ns.clone().any(|n| s == derive_jitter_seed(n, 0, 0)),
        "new_reconnect_jitter_seed must mix pid+clock, not derive(n, 0, 0)"
    );
    assert!(
        !ns.clone().any(|n| s == derive_jitter_seed(n, pid, 0)),
        "clock nanos must enter the seed; pid alone lock-steps a shared PID namespace"
    );
}
/// A zero or unset ping interval must resolve to the default. A zero
/// period would otherwise reach `tokio::time::interval`, which panics on
/// `Duration::ZERO`; a positive override is honored verbatim.
#[test]
fn resolve_ws_ping_interval_clamps_zero_and_unset_to_default() {
    assert_eq!(resolve_ws_ping_interval(None), DEFAULT_WS_PING_INTERVAL);
    assert_eq!(
        resolve_ws_ping_interval(Some(Duration::ZERO)),
        DEFAULT_WS_PING_INTERVAL
    );
    let custom = Duration::from_secs(7);
    assert_eq!(resolve_ws_ping_interval(Some(custom)), custom);
}
/// Resolving a zero ping interval to a non-zero default means
/// `tokio::time::interval` can be constructed without panicking.
#[tokio::test]
async fn resolved_zero_ping_interval_builds_interval_without_panic() {
    let resolved = resolve_ws_ping_interval(Some(Duration::ZERO));
    assert!(!resolved.is_zero());
    let _interval = tokio::time::interval(resolved);
}
/// A zero or unset initial-connect budget resolves to the 10s default —
/// a zero budget would abort every attempt before the upgrade could
/// complete; a positive override is honored verbatim. Mirrors the
/// `resolve_ws_ping_interval` clamp semantics.
#[test]
fn resolve_initial_connect_attempt_timeout_clamps_zero_and_unset_to_default() {
    assert_eq!(
        resolve_initial_connect_attempt_timeout(None),
        INITIAL_CONNECT_ATTEMPT_TIMEOUT
    );
    assert_eq!(
        resolve_initial_connect_attempt_timeout(Some(Duration::ZERO)),
        INITIAL_CONNECT_ATTEMPT_TIMEOUT
    );
    let custom = Duration::from_secs(3);
    assert_eq!(
        resolve_initial_connect_attempt_timeout(Some(custom)),
        custom
    );
}
/// Only transport failures (`NetworkError`, which is also how the
/// per-attempt timeout surfaces) and server closes warrant another
/// initial-connect attempt; deterministic failures (auth, config,
/// protocol, insecure scheme) must surface immediately.
#[test]
fn initial_connect_retryable_classifies_errors() {
    assert!(initial_connect_retryable(&ClientError::NetworkError(
        "io".into()
    )));
    assert!(initial_connect_retryable(&ClientError::Closed(
        "bye".into()
    )));
    assert!(!initial_connect_retryable(
        &ClientError::HandshakeAuthFailed { status: 401 }
    ));
    assert!(!initial_connect_retryable(&ClientError::InvalidConfig(
        "cfg".into()
    )));
    assert!(!initial_connect_retryable(&ClientError::ProtocolError(
        "proto".into()
    )));
    assert!(!initial_connect_retryable(&ClientError::InsecureScheme {
        url: Url::parse("ws://hub.example.com/").expect("valid url"),
    }));
}
/// A listener that accepts the TCP connection but never answers the
/// WebSocket upgrade black-holes an unbounded connect (the 2026-08-19
/// hub-roll incident shape). The per-attempt budget must convert the
/// hang into a retryable `NetworkError` and the attempt cap must bound
/// the total wait instead of retrying forever.
#[tokio::test]
async fn initial_connect_times_out_and_bounds_retries_against_black_hole() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback listener");
    let addr = listener.local_addr().expect("listener addr");
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((sock, _)) = listener.accept().await {
            held.push(sock);
        }
    });
    let credential: Arc<dyn AuthProvider> = Arc::new(AuthCredential::bearer("test-token"));
    let started = std::time::Instant::now();
    let result = HubConnection::connect(ConnectionConfig {
        url: Url::parse(&format!("ws://{addr}/")).expect("valid url"),
        credential,
        kind: ConnectionKind::Harness,
        on_reconnect: None,
        on_disconnect: None,
        on_terminal_close: None,
        on_connect: None,
        server_id: None,
        server_description: None,
        server_metadata: None,
        outbound_buffer: None,
        tuning: ConnectionTuning {
            initial_connect_attempt_timeout: Some(Duration::from_millis(100)),
            reconnect_backoff: Some(Arc::from([Duration::from_millis(10)])),
            ..Default::default()
        },
        alpha_test_key: None,
        allow_insecure_ws: false,
        on_fatal: None,
    })
    .await;
    let elapsed = started.elapsed();
    match result {
        Err(ClientError::NetworkError(msg)) => {
            assert!(
                msg.contains("timed out"),
                "expected a per-attempt timeout message; got: {msg}"
            );
        }
        Err(other) => panic!("expected NetworkError timeout; got {other:?}"),
        Ok(_) => panic!("expected NetworkError timeout; got a live connection"),
    }
    assert!(
        elapsed < Duration::from_secs(5),
        "initial connect was not bounded: {elapsed:?}"
    );
}
fn bearer_credential() -> AuthCredential {
    AuthCredential::bearer("test-token")
}
#[tokio::test]
async fn open_socket_refuses_plaintext_ws_to_remote_host() {
    let url = Url::parse("ws://hub.example.com:8080/v1/tools").expect("valid url");
    let credential = bearer_credential();
    match open_socket(&url, &credential, ConnectionKind::Harness, None, false).await {
        Err(ClientError::InsecureScheme { url: rejected }) => {
            assert_eq!(rejected, url);
        }
        other => panic!("expected InsecureScheme; got {other:?}"),
    }
}
#[tokio::test]
async fn open_socket_allows_plaintext_ws_to_loopback() {
    let url = Url::parse("ws://127.0.0.1:1/").expect("valid url");
    let credential = bearer_credential();
    if let Err(ClientError::InsecureScheme { .. }) =
        open_socket(&url, &credential, ConnectionKind::Harness, None, false).await
    {
        panic!("loopback ws:// must not be rejected by the scheme guard")
    }
}
#[tokio::test]
async fn open_socket_allows_wss_to_remote_host() {
    let url = Url::parse("wss://hub.example.com/").expect("valid url");
    let credential = bearer_credential();
    if let Err(ClientError::InsecureScheme { .. }) =
        open_socket(&url, &credential, ConnectionKind::Harness, None, false).await
    {
        panic!("wss:// must not be rejected by the scheme guard")
    }
}
#[tokio::test]
async fn open_socket_allows_plaintext_ws_when_insecure_opt_in() {
    let url = Url::parse("ws://hub.example.com:1/").expect("valid url");
    let credential = bearer_credential();
    if let Err(ClientError::InsecureScheme { .. }) =
        open_socket(&url, &credential, ConnectionKind::Harness, None, true).await
    {
        panic!("allow_insecure_ws must bypass the scheme guard")
    }
}
#[tokio::test]
async fn open_socket_rejects_role_mismatch() {
    let url = Url::parse("ws://127.0.0.1:1/?role=harness").expect("valid url");
    let credential = bearer_credential();
    match open_socket(&url, &credential, ConnectionKind::ToolServer, None, false).await {
        Err(ClientError::InvalidConfig(msg)) => {
            assert!(
                msg.contains("conflicts with"),
                "message should mention conflict; got: {msg}"
            );
        }
        other => panic!("expected InvalidConfig; got {other:?}"),
    }
}
#[test]
fn host_is_loopback_recognises_canonical_names() {
    for raw in [
        "ws://127.0.0.1/",
        "ws://[::1]/",
        "ws://localhost/",
        "ws://LOCALHOST/",
    ] {
        let url = Url::parse(raw).expect("valid url");
        assert!(host_is_loopback(&url), "{raw} must be treated as loopback");
    }
    for raw in ["ws://hub.example.com/", "ws://10.0.0.1/", "ws://127.0.0.2/"] {
        let url = Url::parse(raw).expect("valid url");
        assert!(
            !host_is_loopback(&url),
            "{raw} must NOT be treated as loopback",
        );
    }
}
#[test]
fn exit_for_close_code_classifies_terminal_range() {
    assert!(matches!(
        exit_for_close_code(Some(4100)),
        ConnectedExit::TerminalClose(4100)
    ));
    assert!(matches!(
        exit_for_close_code(Some(4199)),
        ConnectedExit::TerminalClose(4199)
    ));
    assert!(matches!(
        exit_for_close_code(Some(4099)),
        ConnectedExit::SocketClosed(DisconnectCause::CloseFrame(Some(4099)))
    ));
    assert!(matches!(
        exit_for_close_code(Some(4200)),
        ConnectedExit::SocketClosed(DisconnectCause::CloseFrame(Some(4200)))
    ));
    assert!(matches!(
        exit_for_close_code(Some(1000)),
        ConnectedExit::SocketClosed(DisconnectCause::CloseFrame(Some(1000)))
    ));
    assert!(matches!(
        exit_for_close_code(None),
        ConnectedExit::SocketClosed(DisconnectCause::CloseFrame(None))
    ));
}
#[test]
fn disconnect_cause_labels_and_fields() {
    assert_eq!(
        DisconnectCause::CloseFrame(Some(1006)).label(),
        "close_frame"
    );
    assert_eq!(
        DisconnectCause::CloseFrame(Some(1006)).close_code(),
        Some(1006)
    );
    assert_eq!(DisconnectCause::Eof.label(), "eof");
    assert_eq!(DisconnectCause::Eof.close_code(), None);
    assert_eq!(DisconnectCause::Eof.detail(), None);
    let read = DisconnectCause::ReadError("reset".to_owned());
    assert_eq!(read.label(), "transport_read_error");
    assert_eq!(read.detail(), Some("reset"));
    let write = DisconnectCause::WriteError("pipe".to_owned());
    assert_eq!(write.label(), "transport_write_error");
    assert_eq!(write.detail(), Some("pipe"));
    assert_eq!(DisconnectCause::Forced.label(), "forced");
}
#[test]
fn classify_transport_detail_is_bounded() {
    assert_eq!(
        classify_transport_detail("Connection reset by peer (os error 104)"),
        "connection_reset"
    );
    assert_eq!(classify_transport_detail("Broken pipe"), "broken_pipe");
    assert_eq!(
        classify_transport_detail("Unexpected EOF"),
        "unexpected_eof"
    );
    assert_eq!(classify_transport_detail("operation timed out"), "timeout");
    assert_eq!(
        classify_transport_detail("Connection aborted"),
        "connection_aborted"
    );
    assert_eq!(classify_transport_detail("something novel"), "other");
    assert_eq!(
        DisconnectCause::ReadError("ECONNRESET".to_owned()).detail_class(),
        Some("connection_reset")
    );
    assert!(DisconnectCause::Eof.detail_class().is_none());
}
#[test]
fn conn_health_snapshot_without_clock_skew_reports_zero_jump() {
    let health = ConnHealth::new();
    health.record_inbound();
    health.refresh_clock();
    let snap = health.snapshot();
    assert_eq!(snap.clock_jump_ms, 0);
    assert!(snap.since_last_probe_monotonic_ms < 2_000);
}
#[test]
fn conn_health_snapshot_reports_wall_clock_jump() {
    let health = ConnHealth::new();
    {
        let mut state = health.state.lock();
        state.wall_ref = SystemTime::now() - Duration::from_secs(10);
    }
    let snap = health.snapshot();
    assert!(snap.since_last_probe_wall_ms >= 9_000);
    assert!(snap.since_last_probe_monotonic_ms < 2_000);
    assert!(snap.clock_jump_ms >= 8_000);
    health.reset();
    assert_eq!(health.snapshot().clock_jump_ms, 0);
}
#[test]
fn conn_health_accumulates_jump_across_refreshes() {
    let health = ConnHealth::new();
    {
        let mut state = health.state.lock();
        state.wall_ref = SystemTime::now() - Duration::from_secs(5);
    }
    health.refresh_clock();
    {
        let mut state = health.state.lock();
        state.wall_ref = SystemTime::now() - Duration::from_secs(4);
    }
    let snap = health.snapshot();
    assert!(snap.clock_jump_ms >= 8_000);
}
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Context, Poll};
/// In-memory [`futures::Sink`] for `run_writer` tests. Records the
/// text payload of every `Message::Text` sent and counts every
/// `Message::Ping` (keepalive). When the `fail` flag is set, `send`
/// errors at `poll_ready`, modelling a dead socket.
#[derive(Clone)]
struct RecordingSink {
    recorded: Arc<std::sync::Mutex<Vec<String>>>,
    pings: Arc<AtomicUsize>,
    fail: Arc<AtomicBool>,
}
impl RecordingSink {
    fn new() -> Self {
        Self {
            recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
            pings: Arc::new(AtomicUsize::new(0)),
            fail: Arc::new(AtomicBool::new(false)),
        }
    }
    fn recorded(&self) -> Arc<std::sync::Mutex<Vec<String>>> {
        self.recorded.clone()
    }
    fn pings(&self) -> Arc<AtomicUsize> {
        self.pings.clone()
    }
    fn fail_flag(&self) -> Arc<AtomicBool> {
        self.fail.clone()
    }
}
impl futures::Sink<Message> for RecordingSink {
    type Error = std::io::Error;
    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if self.fail.load(Ordering::SeqCst) {
            Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "sink dead",
            )))
        } else {
            Poll::Ready(Ok(()))
        }
    }
    fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        match item {
            Message::Text(text) => {
                self.recorded
                    .lock()
                    .expect("recorded lock")
                    .push(text.as_str().to_owned());
            }
            Message::Ping(_) => {
                self.pings.fetch_add(1, Ordering::SeqCst);
            }
            Message::Close(frame) => {
                let (code, reason) = frame
                    .map(|f| (u16::from(f.code), f.reason.to_string()))
                    .unwrap_or((0, String::new()));
                self.recorded
                    .lock()
                    .expect("recorded lock")
                    .push(format!("CLOSE:{code}:{reason}"));
            }
            _ => {}
        }
        Ok(())
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}
type TestCtl = WriterControl<RecordingSink>;
fn idle_write_error_slot() -> WriteErrorSlot {
    Arc::new(parking_lot::Mutex::new(None))
}
fn outbound_data_frames(recorded: &std::sync::Mutex<Vec<String>>) -> Vec<String> {
    recorded
        .lock()
        .expect("lock")
        .iter()
        .filter(|f| !(f.contains("\"method\":\"ping\"") && f.contains("ts_ms")))
        .cloned()
        .collect()
}
/// Poll `predicate` every 5ms up to ~2s. Keeps the writer-task tests
/// off arbitrary fixed sleeps for the positive assertions.
async fn wait_until<F: Fn() -> bool>(predicate: F, label: &str) {
    for _ in 0..400 {
        if predicate() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("timed out waiting for: {label}");
}
#[tokio::test]
async fn writer_sends_close_before_pause() {
    let sink = RecordingSink::new();
    let recorded = sink.recorded();
    let (out_tx, out_rx) = mpsc::channel::<String>(8);
    let (ctl_tx, ctl_rx) = mpsc::channel::<TestCtl>(4);
    let (stop_tx, stop_rx) = mpsc::channel::<()>(1);
    let (_prio_tx, prio_rx) = mpsc::channel::<String>(4);
    let writer = tokio::spawn(run_writer(
        sink,
        out_rx,
        prio_rx,
        ctl_rx,
        stop_rx,
        None,
        idle_write_error_slot(),
        None,
    ));
    ctl_tx
        .send(WriterControl::Close {
            code: 1001,
            reason: "liveness_deadline".to_owned(),
        })
        .await
        .expect("close");
    ctl_tx.send(WriterControl::Pause).await.expect("pause");
    out_tx.send("buffered".to_owned()).await.expect("buffer");
    wait_until(
        || {
            recorded
                .lock()
                .expect("lock")
                .iter()
                .any(|f| f == "CLOSE:1001:liveness_deadline")
        },
        "close frame written",
    )
    .await;
    assert!(
        !recorded
            .lock()
            .expect("lock")
            .iter()
            .any(|f| f == "buffered"),
        "buffered data must not flush on the old sink after Close+Pause"
    );
    let fresh = RecordingSink::new();
    let fresh_recorded = fresh.recorded();
    ctl_tx
        .send(WriterControl::Resume(fresh))
        .await
        .expect("resume");
    wait_until(
        || {
            fresh_recorded
                .lock()
                .expect("lock")
                .iter()
                .any(|f| f == "buffered")
        },
        "buffered data flushes on the fresh sink after Resume",
    )
    .await;
    drop(out_tx);
    stop_tx.send(()).await.expect("stop");
    writer.await.expect("writer task joins");
}
/// Sink whose first non-Close `poll_ready` stays pending until released.
/// Models a half-open peer with a full TCP send buffer.
struct BlockingSink {
    recorded: Arc<std::sync::Mutex<Vec<String>>>,
    block: Arc<AtomicBool>,
    waker: Arc<std::sync::Mutex<Option<std::task::Waker>>>,
}
impl Clone for BlockingSink {
    fn clone(&self) -> Self {
        Self {
            recorded: self.recorded.clone(),
            block: self.block.clone(),
            waker: self.waker.clone(),
        }
    }
}
impl BlockingSink {
    fn new() -> Self {
        Self {
            recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
            block: Arc::new(AtomicBool::new(true)),
            waker: Arc::new(std::sync::Mutex::new(None)),
        }
    }
    fn recorded(&self) -> Arc<std::sync::Mutex<Vec<String>>> {
        self.recorded.clone()
    }
    fn release(&self) {
        self.block.store(false, Ordering::SeqCst);
        if let Some(w) = self.waker.lock().expect("waker").take() {
            w.wake();
        }
    }
}
impl futures::Sink<Message> for BlockingSink {
    type Error = std::io::Error;
    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if self.block.load(Ordering::SeqCst) {
            *self.waker.lock().expect("waker") = Some(cx.waker().clone());
            Poll::Pending
        } else {
            Poll::Ready(Ok(()))
        }
    }
    fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        match item {
            Message::Text(text) => self
                .recorded
                .lock()
                .expect("lock")
                .push(text.as_str().to_owned()),
            Message::Close(frame) => {
                let (code, reason) = frame
                    .map(|f| (u16::from(f.code), f.reason.to_string()))
                    .unwrap_or((0, String::new()));
                self.recorded
                    .lock()
                    .expect("lock")
                    .push(format!("CLOSE:{code}:{reason}"));
            }
            _ => {}
        }
        Ok(())
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}
#[tokio::test]
async fn writer_preempts_blocked_data_send_for_close() {
    let sink = BlockingSink::new();
    let recorded = sink.recorded();
    let (out_tx, out_rx) = mpsc::channel::<String>(8);
    let (ctl_tx, ctl_rx) = mpsc::channel::<WriterControl<BlockingSink>>(4);
    let (stop_tx, stop_rx) = mpsc::channel::<()>(1);
    let (_prio_tx, prio_rx) = mpsc::channel::<String>(4);
    let writer = tokio::spawn(run_writer(
        sink.clone(),
        out_rx,
        prio_rx,
        ctl_rx,
        stop_rx,
        None,
        idle_write_error_slot(),
        None,
    ));
    out_tx.send("stuck".to_owned()).await.expect("data");
    tokio::time::sleep(Duration::from_millis(20)).await;
    ctl_tx
        .send(WriterControl::Close {
            code: 1001,
            reason: "liveness_deadline".to_owned(),
        })
        .await
        .expect("close");
    ctl_tx.send(WriterControl::Pause).await.expect("pause");
    tokio::time::sleep(Duration::from_millis(10)).await;
    sink.release();
    wait_until(
        || {
            recorded
                .lock()
                .expect("lock")
                .iter()
                .any(|f| f == "CLOSE:1001:liveness_deadline")
        },
        "close preempts blocked data write",
    )
    .await;
    assert!(
        !recorded.lock().expect("lock").iter().any(|f| f == "stuck"),
        "blocked data must not be written after Close preempt"
    );
    drop(out_tx);
    stop_tx.send(()).await.expect("stop");
    writer.await.expect("writer task joins");
}
/// Accepts frames (records them) but never completes flush — models a
/// half-open TCP sndbuf so Close can be observed then time out.
#[tokio::test]
async fn writer_close_then_queued_pause_still_writes_close_1001() {
    let sink = RecordingSink::new();
    let recorded = sink.recorded();
    let (out_tx, out_rx) = mpsc::channel::<String>(4);
    let (ctl_tx, ctl_rx) = mpsc::channel::<TestCtl>(4);
    let (stop_tx, stop_rx) = mpsc::channel::<()>(1);
    let (_prio_tx, prio_rx) = mpsc::channel::<String>(4);
    let writer = tokio::spawn(run_writer(
        sink,
        out_rx,
        prio_rx,
        ctl_rx,
        stop_rx,
        None,
        idle_write_error_slot(),
        None,
    ));
    ctl_tx
        .send(WriterControl::Close {
            code: 1001,
            reason: "liveness_deadline".to_owned(),
        })
        .await
        .expect("close");
    ctl_tx.send(WriterControl::Pause).await.expect("pause");
    wait_until(
        || {
            recorded
                .lock()
                .expect("lock")
                .iter()
                .any(|f| f.starts_with("CLOSE:1001:"))
        },
        "Close 1001 recorded",
    )
    .await;
    let live = RecordingSink::new();
    let live_log = live.recorded();
    ctl_tx
        .send(WriterControl::Resume(live))
        .await
        .expect("resume");
    out_tx.send("after".to_owned()).await.expect("after");
    wait_until(
        || outbound_data_frames(&live_log).iter().any(|f| f == "after"),
        "Resume installs a live sink",
    )
    .await;
    stop_tx.send(()).await.expect("stop");
    writer.await.expect("join");
}
#[tokio::test]
async fn writer_ctl_preempts_in_flight_blocking_ping_then_resumes() {
    let sink = RecordingSink::new();
    let (out_tx, out_rx) = mpsc::channel::<String>(4);
    let (ctl_tx, ctl_rx) = mpsc::channel::<TestCtl>(4);
    let (stop_tx, stop_rx) = mpsc::channel::<()>(1);
    let (_prio_tx, prio_rx) = mpsc::channel::<String>(4);
    let writer = tokio::spawn(run_writer(
        sink,
        out_rx,
        prio_rx,
        ctl_rx,
        stop_rx,
        Some(Duration::from_millis(1)),
        idle_write_error_slot(),
        None,
    ));
    ctl_tx
        .send(WriterControl::Close {
            code: 1001,
            reason: "liveness_deadline".to_owned(),
        })
        .await
        .expect("close");
    ctl_tx.send(WriterControl::Pause).await.expect("pause");
    let live = RecordingSink::new();
    let live_log = live.recorded();
    ctl_tx
        .send(WriterControl::Resume(live))
        .await
        .expect("resume");
    out_tx.send("resumed".to_owned()).await.expect("send");
    wait_until(
        || {
            outbound_data_frames(&live_log)
                .iter()
                .any(|f| f == "resumed")
        },
        "fresh sink accepts data after close+pause+resume",
    )
    .await;
    stop_tx.send(()).await.expect("stop");
    writer.await.expect("join");
}
#[tokio::test]
async fn writer_drains_outbound_while_live() {
    let sink = RecordingSink::new();
    let recorded = sink.recorded();
    let (out_tx, out_rx) = mpsc::channel::<String>(8);
    let (_ctl_tx, ctl_rx) = mpsc::channel::<TestCtl>(2);
    let (stop_tx, stop_rx) = mpsc::channel::<()>(1);
    let (_prio_tx, prio_rx) = mpsc::channel::<String>(4);
    let writer = tokio::spawn(run_writer(
        sink,
        out_rx,
        prio_rx,
        ctl_rx,
        stop_rx,
        None,
        idle_write_error_slot(),
        None,
    ));
    out_tx.send("a".to_owned()).await.expect("send a");
    out_tx.send("b".to_owned()).await.expect("send b");
    wait_until(
        || outbound_data_frames(&recorded).len() == 2,
        "two frames drained",
    )
    .await;
    assert_eq!(
        outbound_data_frames(&recorded),
        vec!["a".to_owned(), "b".to_owned()],
        "frames must be written to the live sink in order"
    );
    stop_tx.send(()).await.expect("stop");
    writer.await.expect("writer task joins");
}
#[tokio::test(start_paused = true)]
async fn writer_first_ping_is_immediate() {
    let sink = RecordingSink::new();
    let pings = sink.pings();
    let recorded = sink.recorded();
    let (_out_tx, out_rx) = mpsc::channel::<String>(4);
    let (_ctl_tx, ctl_rx) = mpsc::channel::<TestCtl>(2);
    let (stop_tx, stop_rx) = mpsc::channel::<()>(1);
    let (_prio_tx, prio_rx) = mpsc::channel::<String>(4);
    let writer = tokio::spawn(run_writer(
        sink,
        out_rx,
        prio_rx,
        ctl_rx,
        stop_rx,
        Some(Duration::from_secs(30)),
        idle_write_error_slot(),
        None,
    ));
    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert!(
        pings.load(Ordering::SeqCst) >= 1,
        "WS ping must fire immediately after writer spawn"
    );
    assert!(
        recorded
            .lock()
            .expect("lock")
            .iter()
            .any(|f| f.contains("\"method\":\"ping\"")),
        "app ping must fire immediately after writer spawn"
    );
    stop_tx.send(()).await.expect("stop");
    writer.await.expect("writer task joins");
}
#[tokio::test(start_paused = true)]
async fn writer_first_ping_after_resume_is_immediate() {
    let dead = RecordingSink::new();
    let (_out_tx, out_rx) = mpsc::channel::<String>(4);
    let (ctl_tx, ctl_rx) = mpsc::channel::<TestCtl>(4);
    let (stop_tx, stop_rx) = mpsc::channel::<()>(1);
    let (_prio_tx, prio_rx) = mpsc::channel::<String>(4);
    let writer = tokio::spawn(run_writer(
        dead,
        out_rx,
        prio_rx,
        ctl_rx,
        stop_rx,
        Some(Duration::from_secs(30)),
        idle_write_error_slot(),
        None,
    ));
    ctl_tx.send(WriterControl::Pause).await.expect("pause");
    let fresh = RecordingSink::new();
    let pings = fresh.pings();
    let recorded = fresh.recorded();
    ctl_tx
        .send(WriterControl::Resume(fresh))
        .await
        .expect("resume");
    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert!(
        pings.load(Ordering::SeqCst) >= 1,
        "WS ping must fire immediately after Resume"
    );
    assert!(
        recorded
            .lock()
            .expect("lock")
            .iter()
            .any(|f| f.contains("\"method\":\"ping\"")),
        "app ping must fire immediately after Resume"
    );
    stop_tx.send(()).await.expect("stop");
    writer.await.expect("writer task joins");
}
#[tokio::test]
async fn writer_priority_pong_bypasses_full_outbound() {
    let sink = RecordingSink::new();
    let recorded = sink.recorded();
    let (out_tx, out_rx) = mpsc::channel::<String>(1);
    let (prio_tx, prio_rx) = mpsc::channel::<String>(4);
    let (_ctl_tx, ctl_rx) = mpsc::channel::<TestCtl>(2);
    let (stop_tx, stop_rx) = mpsc::channel::<()>(1);
    out_tx
        .try_send("blocked".to_owned())
        .expect("fill outbound");
    prio_tx
        .try_send(r#"{"method":"pong","ts_ms":1}"#.to_owned())
        .expect("priority send before spawn");
    let writer = tokio::spawn(run_writer(
        sink,
        out_rx,
        prio_rx,
        ctl_rx,
        stop_rx,
        None,
        idle_write_error_slot(),
        None,
    ));
    wait_until(
        || {
            outbound_data_frames(&recorded)
                .first()
                .is_some_and(|f| f.contains("\"method\":\"pong\""))
        },
        "priority pong is the first data frame",
    )
    .await;
    assert!(
        outbound_data_frames(&recorded)
            .iter()
            .any(|f| f.contains("\"method\":\"pong\"")),
    );
    assert!(
        outbound_data_frames(&recorded)
            .iter()
            .position(|f| f.contains("\"method\":\"pong\""))
            < outbound_data_frames(&recorded)
                .iter()
                .position(|f| f == "blocked"),
    );
    stop_tx.send(()).await.expect("stop");
    writer.await.expect("writer task joins");
}
#[tokio::test]
async fn writer_pause_drops_stale_priority_pongs() {
    let dead = RecordingSink::new();
    let (out_tx, out_rx) = mpsc::channel::<String>(4);
    let (prio_tx, prio_rx) = mpsc::channel::<String>(4);
    let (ctl_tx, ctl_rx) = mpsc::channel::<TestCtl>(4);
    let (stop_tx, stop_rx) = mpsc::channel::<()>(1);
    let writer = tokio::spawn(run_writer(
        dead,
        out_rx,
        prio_rx,
        ctl_rx,
        stop_rx,
        None,
        idle_write_error_slot(),
        None,
    ));
    ctl_tx.send(WriterControl::Pause).await.expect("pause");
    tokio::time::sleep(Duration::from_millis(20)).await;
    prio_tx
        .try_send(r#"{"method":"pong","ts_ms":1}"#.to_owned())
        .expect("stale pong");
    let fresh = RecordingSink::new();
    let recorded = fresh.recorded();
    ctl_tx
        .send(WriterControl::Resume(fresh))
        .await
        .expect("resume");
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(
        !recorded
            .lock()
            .expect("lock")
            .iter()
            .any(|f| f.contains("pong")),
        "stale priority pong must be drained on Pause/Resume"
    );
    drop(out_tx);
    stop_tx.send(()).await.expect("stop");
    writer.await.expect("join");
}
#[tokio::test]
async fn writer_honors_custom_ping_interval() {
    let sink = RecordingSink::new();
    let pings = sink.pings();
    let recorded = sink.recorded();
    let (_out_tx, out_rx) = mpsc::channel::<String>(4);
    let (_ctl_tx, ctl_rx) = mpsc::channel::<TestCtl>(2);
    let (stop_tx, stop_rx) = mpsc::channel::<()>(1);
    let (_prio_tx, prio_rx) = mpsc::channel::<String>(4);
    let writer = tokio::spawn(run_writer(
        sink,
        out_rx,
        prio_rx,
        ctl_rx,
        stop_rx,
        Some(Duration::from_millis(20)),
        idle_write_error_slot(),
        None,
    ));
    wait_until(
        || pings.load(Ordering::SeqCst) >= 3,
        "three keepalive pings at the configured cadence",
    )
    .await;
    wait_until(
        || {
            recorded.lock().expect("lock").iter().any(|text| {
                serde_json::from_str::<serde_json::Value>(text)
                    .ok()
                    .and_then(|v| {
                        v.get("method")
                            .and_then(serde_json::Value::as_str)
                            .map(|m| m == "ping")
                    })
                    .unwrap_or(false)
            })
        },
        "serialized app ping {\"method\":\"ping\",...} on the sink",
    )
    .await;
    stop_tx.send(()).await.expect("stop");
    writer.await.expect("writer task joins");
}
#[tokio::test]
async fn writer_re_arms_custom_ping_interval_after_resume() {
    let dead = RecordingSink::new();
    let (_out_tx, out_rx) = mpsc::channel::<String>(4);
    let (ctl_tx, ctl_rx) = mpsc::channel::<TestCtl>(2);
    let (stop_tx, stop_rx) = mpsc::channel::<()>(1);
    let (_prio_tx, prio_rx) = mpsc::channel::<String>(4);
    let writer = tokio::spawn(run_writer(
        dead,
        out_rx,
        prio_rx,
        ctl_rx,
        stop_rx,
        Some(Duration::from_millis(20)),
        idle_write_error_slot(),
        None,
    ));
    ctl_tx.send(WriterControl::Pause).await.expect("pause");
    let fresh = RecordingSink::new();
    let fresh_pings = fresh.pings();
    ctl_tx
        .send(WriterControl::Resume(fresh))
        .await
        .expect("resume");
    wait_until(
        || fresh_pings.load(Ordering::SeqCst) >= 3,
        "keepalive pings resume on the configured cadence after Resume",
    )
    .await;
    stop_tx.send(()).await.expect("stop");
    writer.await.expect("writer task joins");
}
#[tokio::test]
async fn writer_buffers_during_pause_and_flushes_on_resume() {
    let dead = RecordingSink::new();
    let dead_log = dead.recorded();
    let (out_tx, out_rx) = mpsc::channel::<String>(16);
    let (ctl_tx, ctl_rx) = mpsc::channel::<TestCtl>(2);
    let (stop_tx, stop_rx) = mpsc::channel::<()>(1);
    let (_prio_tx, prio_rx) = mpsc::channel::<String>(4);
    let writer = tokio::spawn(run_writer(
        dead,
        out_rx,
        prio_rx,
        ctl_rx,
        stop_rx,
        None,
        idle_write_error_slot(),
        None,
    ));
    ctl_tx.send(WriterControl::Pause).await.expect("pause");
    tokio::time::sleep(Duration::from_millis(20)).await;
    for frame in ["g1", "g2", "g3"] {
        out_tx
            .send(frame.to_owned())
            .await
            .expect("enqueue during gap");
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        outbound_data_frames(&dead_log).is_empty(),
        "paused writer must not drain onto the dead sink; got {:?}",
        outbound_data_frames(&dead_log)
    );
    let fresh = RecordingSink::new();
    let fresh_log = fresh.recorded();
    ctl_tx
        .send(WriterControl::Resume(fresh))
        .await
        .expect("resume");
    wait_until(
        || outbound_data_frames(&fresh_log).len() == 3,
        "buffered frames flush after resume",
    )
    .await;
    assert_eq!(
        outbound_data_frames(&fresh_log),
        vec!["g1".to_owned(), "g2".to_owned(), "g3".to_owned()],
        "all gap frames flush, in order, to the fresh sink"
    );
    assert!(
        outbound_data_frames(&dead_log).is_empty(),
        "no data frame must ever reach the dead sink"
    );
    stop_tx.send(()).await.expect("stop");
    writer.await.expect("writer task joins");
}
#[tokio::test]
async fn writer_send_error_pauses_until_resume_without_multi_frame_loss() {
    let failing = RecordingSink::new();
    let failing_log = failing.recorded();
    let fail_flag = failing.fail_flag();
    let (out_tx, out_rx) = mpsc::channel::<String>(16);
    let (ctl_tx, ctl_rx) = mpsc::channel::<TestCtl>(2);
    let (stop_tx, stop_rx) = mpsc::channel::<()>(1);
    let write_error = idle_write_error_slot();
    let (_prio_tx, prio_rx) = mpsc::channel::<String>(4);
    let writer = tokio::spawn(run_writer(
        failing,
        out_rx,
        prio_rx,
        ctl_rx,
        stop_rx,
        None,
        write_error.clone(),
        None,
    ));
    out_tx.send("ok".to_owned()).await.expect("send ok");
    wait_until(
        || outbound_data_frames(&failing_log).len() == 1,
        "first frame drained before failure",
    )
    .await;
    fail_flag.store(true, Ordering::SeqCst);
    out_tx.send("lost".to_owned()).await.expect("enqueue lost");
    out_tx
        .send("kept1".to_owned())
        .await
        .expect("enqueue kept1");
    out_tx
        .send("kept2".to_owned())
        .await
        .expect("enqueue kept2");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        outbound_data_frames(&failing_log),
        vec!["ok".to_owned()],
        "only the pre-failure frame should have been recorded on the dead sink"
    );
    assert!(
        write_error
            .lock()
            .as_deref()
            .is_some_and(|detail| detail.contains("sink dead")),
        "failed send must record the write-error detail for disconnect classification"
    );
    let fresh = RecordingSink::new();
    let fresh_log = fresh.recorded();
    ctl_tx
        .send(WriterControl::Resume(fresh))
        .await
        .expect("resume");
    wait_until(
        || outbound_data_frames(&fresh_log).len() == 2,
        "buffered post-failure frames flush after resume",
    )
    .await;
    assert_eq!(
        outbound_data_frames(&fresh_log),
        vec!["kept1".to_owned(), "kept2".to_owned()],
        "post-failure frames survive; only the in-flight 'lost' frame is gone"
    );
    stop_tx.send(()).await.expect("stop");
    writer.await.expect("writer task joins");
}
#[tokio::test]
async fn writer_resume_discards_stale_write_error() {
    let sink = RecordingSink::new();
    let (_out_tx, out_rx) = mpsc::channel::<String>(4);
    let (ctl_tx, ctl_rx) = mpsc::channel::<TestCtl>(2);
    let (stop_tx, stop_rx) = mpsc::channel::<()>(1);
    let write_error = idle_write_error_slot();
    let (_prio_tx, prio_rx) = mpsc::channel::<String>(4);
    let writer = tokio::spawn(run_writer(
        sink,
        out_rx,
        prio_rx,
        ctl_rx,
        stop_rx,
        None,
        write_error.clone(),
        None,
    ));
    ctl_tx.send(WriterControl::Pause).await.expect("pause");
    *write_error.lock() = Some("frame send failed: stale broken pipe".to_owned());
    ctl_tx
        .send(WriterControl::Resume(RecordingSink::new()))
        .await
        .expect("resume");
    wait_until(
        || write_error.lock().is_none(),
        "Resume must clear a stale write-error left by a late old-sink send",
    )
    .await;
    stop_tx.send(()).await.expect("stop");
    writer.await.expect("writer task joins");
}
#[tokio::test]
async fn writer_exits_on_stop_signal() {
    let sink = RecordingSink::new();
    let (_out_tx, out_rx) = mpsc::channel::<String>(4);
    let (_ctl_tx, ctl_rx) = mpsc::channel::<TestCtl>(2);
    let (stop_tx, stop_rx) = mpsc::channel::<()>(1);
    let (_prio_tx, prio_rx) = mpsc::channel::<String>(4);
    let writer = tokio::spawn(run_writer(
        sink,
        out_rx,
        prio_rx,
        ctl_rx,
        stop_rx,
        None,
        idle_write_error_slot(),
        None,
    ));
    stop_tx.send(()).await.expect("stop");
    tokio::time::timeout(Duration::from_secs(2), writer)
        .await
        .expect("writer must exit on the stop signal")
        .expect("writer task joins");
}
#[tokio::test]
async fn writer_exits_when_outbound_channel_closes() {
    let sink = RecordingSink::new();
    let (out_tx, out_rx) = mpsc::channel::<String>(4);
    let (_ctl_tx, ctl_rx) = mpsc::channel::<TestCtl>(2);
    let (_stop_tx, stop_rx) = mpsc::channel::<()>(1);
    let (_prio_tx, prio_rx) = mpsc::channel::<String>(4);
    let writer = tokio::spawn(run_writer(
        sink,
        out_rx,
        prio_rx,
        ctl_rx,
        stop_rx,
        None,
        idle_write_error_slot(),
        None,
    ));
    drop(out_tx);
    tokio::time::timeout(Duration::from_secs(2), writer)
        .await
        .expect("writer must exit when outbound closes")
        .expect("writer task joins");
}
#[tokio::test]
async fn writer_exits_when_control_channel_closes() {
    let sink = RecordingSink::new();
    let (_out_tx, out_rx) = mpsc::channel::<String>(4);
    let (ctl_tx, ctl_rx) = mpsc::channel::<TestCtl>(2);
    let (_stop_tx, stop_rx) = mpsc::channel::<()>(1);
    let (_prio_tx, prio_rx) = mpsc::channel::<String>(4);
    let writer = tokio::spawn(run_writer(
        sink,
        out_rx,
        prio_rx,
        ctl_rx,
        stop_rx,
        None,
        idle_write_error_slot(),
        None,
    ));
    drop(ctl_tx);
    tokio::time::timeout(Duration::from_secs(2), writer)
        .await
        .expect("writer must exit when the control channel closes")
        .expect("writer task joins");
}
/// Socket-less `HubConnection` for tests: observe the sent frame and
/// resolve the response waiter without a live server or actor task.
fn test_connection() -> (Arc<HubConnection>, Arc<Demux>, mpsc::Receiver<String>) {
    let (outbound_tx, outbound_rx) = mpsc::channel::<String>(8);
    let demux = Arc::new(Demux::with_outbound(outbound_tx.clone()));
    let credential: Arc<dyn AuthProvider> = Arc::new(AuthCredential::bearer("test-token"));
    let (stop_tx, _stop_rx) = mpsc::channel::<()>(1);
    let (reconnect_tx, _reconnect_rx) = mpsc::channel::<()>(1);
    let inner = Arc::new(HubConnectionInner {
        key: ConnKey {
            url: "ws://test/v1/tools".to_owned(),
            principal: credential.principal_key(),
        },
        kind: ConnectionKind::ToolServer,
        credential,
        on_reconnect: None,
        on_disconnect: None,
        on_terminal_close: None,
        server_id: None,
        server_description: None,
        server_metadata: None,
        alpha_test_key: None,
        allow_insecure_ws: false,
        on_fatal: None,
        reconnect_backoff: resolve_reconnect_backoff(None),
        reconnect_jitter_seed: 1,
        attempt_reset_after: resolve_attempt_reset_after(None),
        reconnect_after_terminal_close_codes: Vec::new(),
        outage_seq: AtomicU32::new(0),
        outbound_tx,
        demux: demux.clone(),
        bound_sessions: Arc::new(RefCountedSet::new()),
        connection_id: Arc::new(Mutex::new(None)),
        hello_capabilities: parking_lot::RwLock::new(Vec::new()),
        next_request_id: std::sync::atomic::AtomicU64::new(1),
        shutdown: CancellationToken::new(),
        stop_tx,
        reconnect_tx,
        early_notif_rx: parking_lot::Mutex::new(Some(demux.subscribe_notifications())),
        health: ConnHealth::new(),
        writer_error: Arc::new(parking_lot::Mutex::new(None)),
    });
    (Arc::new(HubConnection { inner }), demux, outbound_rx)
}
#[test]
fn classify_stream_end_prefers_recorded_write_error() {
    let (conn, _demux, _outbound_rx) = test_connection();
    let inner = conn.inner.as_ref();
    assert!(matches!(
        classify_stream_end(inner, None),
        DisconnectCause::Eof
    ));
    assert!(matches!(
        classify_stream_end(inner, Some("reset by peer".to_owned())),
        DisconnectCause::ReadError(detail) if detail == "reset by peer"
    ));
    *inner.writer_error.lock() = Some("ping send failed: broken pipe".to_owned());
    assert!(matches!(
        classify_stream_end(inner, None),
        DisconnectCause::WriteError(detail) if detail == "ping send failed: broken pipe"
    ));
    assert!(
        inner.writer_error.lock().is_none(),
        "classification must consume the recorded write error"
    );
    *inner.writer_error.lock() = Some("frame send failed: broken pipe".to_owned());
    assert!(matches!(
        classify_stream_end(inner, Some("reset".to_owned())),
        DisconnectCause::WriteError(_)
    ));
}
#[test]
fn supports_is_unknown_until_capabilities_advertised() {
    let (conn, _demux, _outbound_rx) = test_connection();
    assert_eq!(conn.supports("session_attach_server"), None);
    *conn.inner.hello_capabilities.write() = vec!["session_attach_server".to_owned()];
    assert_eq!(conn.supports("session_attach_server"), Some(true));
    assert_eq!(conn.supports("some_other_method"), Some(false));
}
#[tokio::test]
async fn call_request_with_timeout_round_trips_via_demux() {
    let (conn, demux, mut outbound_rx) = test_connection();
    let session = SessionId::new("rt_session").expect("valid");
    let request_id = conn.try_alloc_request_id().expect("request id");
    let id_str = request_id.to_string();
    let req = JsonRpcRequest {
        jsonrpc: JsonRpcVersion,
        id: JsonRpcId::from_request_id(&request_id),
        session_id: Some(session.clone()),
        method: Method::Hook.as_wire_str().to_owned(),
        params: serde_json::json!({ "k": "v" }),
    };
    let call = tokio::spawn(async move {
        conn.call_request_with_timeout(request_id, &req, Duration::from_secs(5))
            .await
    });
    let sent = tokio::time::timeout(Duration::from_secs(1), outbound_rx.recv())
        .await
        .expect("frame sent before deadline")
        .expect("outbound frame present");
    let sent_value: Value = serde_json::from_str(&sent).expect("sent frame is valid json");
    assert_eq!(sent_value["id"].as_str(), Some(id_str.as_str()));
    assert_eq!(
        sent_value["method"].as_str(),
        Some(Method::Hook.as_wire_str())
    );
    let outcome = demux.route(serde_json::json!({
        "jsonrpc": "2.0",
        "id": id_str,
        "session_id": session.as_str(),
        "result": { "ok": true },
    }));
    assert_eq!(outcome, crate::demux::RouteOutcome::Response);
    let resp = call
        .await
        .expect("call task joins")
        .expect("call resolves with a response");
    let ResponseOutcome::Result(value) = resp.outcome else {
        panic!("expected a result outcome");
    };
    assert_eq!(value, serde_json::json!({ "ok": true }));
}
#[tokio::test]
async fn call_request_reclaims_waiter_on_send_failure() {
    let (conn, demux, outbound_rx) = test_connection();
    drop(outbound_rx);
    let request_id = conn.try_alloc_request_id().expect("request id");
    let probe_id = request_id.clone();
    let req = JsonRpcRequest {
        jsonrpc: JsonRpcVersion,
        id: JsonRpcId::from_request_id(&request_id),
        session_id: None,
        method: Method::Hook.as_wire_str().to_owned(),
        params: serde_json::json!({}),
    };
    let result = conn.call_request(request_id, &req).await;
    assert!(matches!(result, Err(ClientError::NetworkError(_))));
    assert!(
        demux.take_response_waiter(&probe_id).is_none(),
        "the failed-send waiter is reclaimed so it cannot leak"
    );
}
#[tokio::test]
async fn serve_send_failure_fails_fast_without_retry() {
    let (conn, demux, outbound_rx) = test_connection();
    drop(outbound_rx);
    let session = SessionId::new("serve_session").expect("valid");
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        conn.serve(
            session,
            echo_agent_tool_protocol::ServeParams { tools: vec![] },
        ),
    )
    .await
    .expect("serve must fail bounded, not park");
    assert!(matches!(result, Err(ClientError::NetworkError(_))));
    let request_id = echo_agent_tool_protocol::RequestId::new("c1").expect("valid");
    assert!(
        demux.take_response_waiter(&request_id).is_none(),
        "the failed attempt must not leak a waiter"
    );
    assert_eq!(
        conn.try_alloc_request_id().expect("request id").to_string(),
        "c2",
        "a non-timeout failure must consume a single attempt, not retry"
    );
}
#[tokio::test(start_paused = true)]
async fn serve_times_out_bounded_and_reclaims_every_attempt_waiter() {
    let (conn, demux, mut outbound_rx) = test_connection();
    let session = SessionId::new("serve_timeout").expect("valid");
    let result = conn
        .serve(
            session,
            echo_agent_tool_protocol::ServeParams { tools: vec![] },
        )
        .await;
    assert!(matches!(result, Err(ClientError::NetworkError(_))));
    for id in ["c1", "c2", "c3"] {
        let sent = outbound_rx.try_recv().expect("attempt frame sent");
        let value: Value = serde_json::from_str(&sent).expect("valid json");
        assert_eq!(value["id"].as_str(), Some(id));
        let request_id = echo_agent_tool_protocol::RequestId::new(id).expect("valid");
        assert!(
            demux.take_response_waiter(&request_id).is_none(),
            "attempt {id} must not leak a waiter"
        );
    }
    assert!(
        outbound_rx.try_recv().is_err(),
        "exactly SERVE_MAX_ATTEMPTS frames are sent"
    );
}
#[tokio::test]
async fn call_request_reclaims_waiter_on_caller_cancellation() {
    let (conn, demux, mut outbound_rx) = test_connection();
    let request_id = conn.try_alloc_request_id().expect("request id");
    let probe_id = request_id.clone();
    let conn_for_call = conn.clone();
    let call = tokio::spawn(async move {
        let req = JsonRpcRequest {
            jsonrpc: JsonRpcVersion,
            id: JsonRpcId::from_request_id(&request_id),
            session_id: None,
            method: Method::Hook.as_wire_str().to_owned(),
            params: serde_json::json!({}),
        };
        conn_for_call.call_request(request_id, &req).await
    });
    tokio::time::timeout(Duration::from_secs(1), outbound_rx.recv())
        .await
        .expect("frame sent")
        .expect("outbound frame present");
    call.abort();
    let _ = call.await;
    assert!(
        demux.take_response_waiter(&probe_id).is_none(),
        "the cancelled caller's waiter is reclaimed so it cannot leak"
    );
}
#[tokio::test]
async fn reader_phase_exits_socket_closed_on_forced_reconnect_signal() {
    let (conn, _demux, _outbound_rx) = test_connection();
    let (reconnect_tx, mut reconnect_rx) = mpsc::channel::<()>(1);
    let (_stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
    reconnect_tx.try_send(()).expect("queue forced reconnect");
    let mut stream =
        futures::stream::pending::<Result<Message, tokio_tungstenite::tungstenite::Error>>();
    let exit = tokio::time::timeout(
        Duration::from_secs(1),
        run_reader_phase(
            conn.inner.as_ref(),
            &mut stream,
            &mut stop_rx,
            &mut reconnect_rx,
            Duration::from_secs(75),
            &conn.inner.outbound_tx,
        ),
    )
    .await
    .expect("forced reconnect must break the reader phase");
    assert!(
        matches!(exit, ConnectedExit::SocketClosed(DisconnectCause::Forced)),
        "a forced reconnect exits as SocketClosed (drives the reconnect path)"
    );
}
#[tokio::test]
async fn stop_signal_outranks_forced_reconnect() {
    let (conn, _demux, _outbound_rx) = test_connection();
    let (reconnect_tx, mut reconnect_rx) = mpsc::channel::<()>(1);
    let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
    reconnect_tx.try_send(()).expect("queue forced reconnect");
    stop_tx.try_send(()).expect("queue stop");
    let mut stream =
        futures::stream::pending::<Result<Message, tokio_tungstenite::tungstenite::Error>>();
    let exit = tokio::time::timeout(
        Duration::from_secs(1),
        run_reader_phase(
            conn.inner.as_ref(),
            &mut stream,
            &mut stop_rx,
            &mut reconnect_rx,
            Duration::from_secs(75),
            &conn.inner.outbound_tx,
        ),
    )
    .await
    .expect("stop must break the reader phase");
    assert!(matches!(exit, ConnectedExit::Stop));
}
#[tokio::test]
async fn drain_reconnect_signals_clears_stale_signal_only() {
    let (reconnect_tx, mut reconnect_rx) = mpsc::channel::<()>(1);
    reconnect_tx.try_send(()).expect("queue stale signal");
    drain_reconnect_signals(&mut reconnect_rx);
    assert!(
        reconnect_rx.try_recv().is_err(),
        "a stale pre-reconnect signal is consumed by the drain"
    );
    reconnect_tx.try_send(()).expect("queue fresh signal");
    assert!(
        reconnect_rx.try_recv().is_ok(),
        "the drain must not disable the channel for future signals"
    );
}
#[tokio::test]
async fn early_subscribed_receiver_buffers_pre_run_connection_notifications() {
    let (conn, demux, _outbound_rx) = test_connection();
    let outcome = demux.route(serde_json::json!({
        "jsonrpc": "2.0",
        "id": "b1",
        "method": "session.bind",
        "params": { "session_id": "s1" },
    }));
    assert_eq!(outcome, crate::demux::RouteOutcome::Notification);
    let mut rx = conn
        .take_early_notifications()
        .expect("receiver retained until taken");
    let frame = rx.try_recv().expect("pre-run frame buffered");
    assert_eq!(frame["method"], "session.bind");
    assert!(
        conn.take_early_notifications().is_none(),
        "the early receiver is handed off exactly once"
    );
}
#[tokio::test]
async fn call_request_with_timeout_reclaims_waiter_on_deadline() {
    let (conn, demux, mut outbound_rx) = test_connection();
    let session = SessionId::new("to_session").expect("valid");
    let request_id = conn.try_alloc_request_id().expect("request id");
    let probe_id = request_id.clone();
    let req = JsonRpcRequest {
        jsonrpc: JsonRpcVersion,
        id: JsonRpcId::from_request_id(&request_id),
        session_id: Some(session),
        method: Method::Hook.as_wire_str().to_owned(),
        params: serde_json::json!({}),
    };
    let result = conn
        .call_request_with_timeout(request_id, &req, Duration::from_millis(50))
        .await;
    assert!(matches!(result, Err(ClientError::NetworkError(_))));
    assert!(
        outbound_rx.try_recv().is_ok(),
        "the request frame is sent before the deadline fires"
    );
    assert!(
        demux.take_response_waiter(&probe_id).is_none(),
        "the timed-out waiter is reclaimed so it cannot leak"
    );
}
/// Regression: a *forced* reconnect abandons a still-healthy socket. If
/// the first reconnect attempt then fails, the actor must keep retrying
/// off the abandoned stream — falling back into the reader phase would
/// park in `stream.next()` on the live old connection forever (the
/// reconnect signal was already consumed), stalling the retry loop.
///
/// Mock: conn #0 (initial) completes the handshake and stays healthy;
/// conn #1 (first reconnect) is dropped before the ack (transport
/// failure); conn #2 must then be attempted and complete. With the bug,
/// upgrade #2 never happens and the test times out.
#[tokio::test]
async fn forced_reconnect_retries_past_failed_attempt_without_repolling_old_stream() {
    use futures::{SinkExt as _, StreamExt as _};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock hub");
    let addr = listener.local_addr().expect("mock addr");
    let upgrades = Arc::new(AtomicUsize::new(0));
    let upgrades_srv = upgrades.clone();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let n = upgrades_srv.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await else {
                    return;
                };
                if n == 1 {
                    return;
                }
                let _ = ws.next().await;
                let ack = serde_json::json!({
                    "connection_id": format!("mock-conn-{n}"),
                    "user_id": "test",
                    "computer_hub_version": "test",
                    "supported_protocol_versions": ["1.0.0"],
                });
                if ws
                    .send(tokio_tungstenite::tungstenite::Message::Text(
                        ack.to_string().into(),
                    ))
                    .await
                    .is_err()
                {
                    return;
                }
                while let Some(msg) = ws.next().await {
                    if msg.is_err() {
                        return;
                    }
                }
            });
        }
    });
    let credential: Arc<dyn AuthProvider> = Arc::new(AuthCredential::bearer("test-token"));
    let conn = HubConnection::connect(ConnectionConfig {
        url: url::Url::parse(&format!("ws://{addr}/v1/tools")).expect("mock url"),
        credential,
        kind: ConnectionKind::ToolServer,
        on_reconnect: None,
        on_disconnect: None,
        on_terminal_close: None,
        on_connect: None,
        server_id: None,
        server_description: None,
        server_metadata: None,
        outbound_buffer: None,
        tuning: ConnectionTuning {
            reconnect_backoff: Some(Arc::from([Duration::from_millis(10)])),
            ..Default::default()
        },
        alpha_test_key: None,
        allow_insecure_ws: false,
        on_fatal: None,
    })
    .await
    .expect("initial connect");
    conn.force_reconnect();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while upgrades.load(Ordering::SeqCst) < 3 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "retry stalled after a failed forced-reconnect attempt: \
             {} upgrades observed (expected 3: initial + failed + successful)",
            upgrades.load(Ordering::SeqCst)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    conn.request_shutdown();
    conn.await_shutdown().await;
}
/// After a *stable* connection a new outage starts at attempt 1. A
/// failed attempt in the first episode still increments so
/// `on_reconnect` reports 2; `reset_after = 0` treats even a brief
/// socket as stable so the next episode is 1 again.
#[tokio::test]
async fn successful_reconnect_resets_attempt_after_stable_dwell() {
    use futures::{SinkExt as _, StreamExt as _};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock hub");
    let addr = listener.local_addr().expect("mock addr");
    let upgrades = Arc::new(AtomicUsize::new(0));
    let upgrades_srv = upgrades.clone();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let n = upgrades_srv.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await else {
                    return;
                };
                if n == 1 {
                    return;
                }
                let _ = ws.next().await;
                let ack = serde_json::json!({
                    "connection_id": format!("mock-conn-{n}"),
                    "user_id": "test",
                    "computer_hub_version": "test",
                    "supported_protocol_versions": ["1.0.0"],
                });
                if ws
                    .send(tokio_tungstenite::tungstenite::Message::Text(
                        ack.to_string().into(),
                    ))
                    .await
                    .is_err()
                {
                    return;
                }
                while let Some(msg) = ws.next().await {
                    if msg.is_err() {
                        return;
                    }
                }
            });
        }
    });
    let (attempt_tx, mut attempt_rx) = mpsc::unbounded_channel();
    let on_reconnect: Arc<ReconnectCallback> = Arc::new(Box::new(move |event: ReconnectEvent| {
        let _ = attempt_tx.send(event.attempt);
    }));
    let credential: Arc<dyn AuthProvider> = Arc::new(AuthCredential::bearer("test-token"));
    let conn = HubConnection::connect(ConnectionConfig {
        url: url::Url::parse(&format!("ws://{addr}/v1/tools")).expect("mock url"),
        credential,
        kind: ConnectionKind::ToolServer,
        on_reconnect: Some(on_reconnect),
        on_disconnect: None,
        on_terminal_close: None,
        on_connect: None,
        server_id: None,
        server_description: None,
        server_metadata: None,
        outbound_buffer: None,
        tuning: ConnectionTuning {
            reconnect_backoff: Some(Arc::from([Duration::from_millis(10)])),
            reconnect_attempt_reset_after: Some(Duration::ZERO),
            ..Default::default()
        },
        alpha_test_key: None,
        allow_insecure_ws: false,
        on_fatal: None,
    })
    .await
    .expect("initial connect");
    conn.force_reconnect();
    let first = tokio::time::timeout(Duration::from_secs(5), attempt_rx.recv())
        .await
        .expect("first episode reconnect event")
        .expect("reconnect channel open");
    assert_eq!(
        first, 2,
        "failed attempt then success must report attempt 2 (increment still lives)"
    );
    conn.force_reconnect();
    let second = tokio::time::timeout(Duration::from_secs(5), attempt_rx.recv())
        .await
        .expect("second episode reconnect event")
        .expect("reconnect channel open");
    assert_eq!(
        second, 1,
        "stable (reset_after=0) prior connection must reset so the next outage starts at 1"
    );
    conn.request_shutdown();
    conn.await_shutdown().await;
}
/// A flap shorter than the dwell must keep climbing: resetting on every
/// handshake is the crash-loop / drain-then-immediate-redrop regression.
#[tokio::test]
async fn flapping_reconnect_does_not_reset_attempt() {
    use futures::{SinkExt as _, StreamExt as _};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock hub");
    let addr = listener.local_addr().expect("mock addr");
    let upgrades = Arc::new(AtomicUsize::new(0));
    let upgrades_srv = upgrades.clone();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let n = upgrades_srv.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await else {
                    return;
                };
                if n == 1 {
                    return;
                }
                let _ = ws.next().await;
                let ack = serde_json::json!({
                    "connection_id": format!("mock-conn-{n}"),
                    "user_id": "test",
                    "computer_hub_version": "test",
                    "supported_protocol_versions": ["1.0.0"],
                });
                if ws
                    .send(tokio_tungstenite::tungstenite::Message::Text(
                        ack.to_string().into(),
                    ))
                    .await
                    .is_err()
                {
                    return;
                }
                while let Some(msg) = ws.next().await {
                    if msg.is_err() {
                        return;
                    }
                }
            });
        }
    });
    let (attempt_tx, mut attempt_rx) = mpsc::unbounded_channel();
    let on_reconnect: Arc<ReconnectCallback> = Arc::new(Box::new(move |event: ReconnectEvent| {
        let _ = attempt_tx.send(event.attempt);
    }));
    let credential: Arc<dyn AuthProvider> = Arc::new(AuthCredential::bearer("test-token"));
    let conn = HubConnection::connect(ConnectionConfig {
        url: url::Url::parse(&format!("ws://{addr}/v1/tools")).expect("mock url"),
        credential,
        kind: ConnectionKind::ToolServer,
        on_reconnect: Some(on_reconnect),
        on_disconnect: None,
        on_terminal_close: None,
        on_connect: None,
        server_id: None,
        server_description: None,
        server_metadata: None,
        outbound_buffer: None,
        tuning: ConnectionTuning {
            reconnect_backoff: Some(Arc::from([Duration::from_millis(10)])),
            reconnect_attempt_reset_after: Some(Duration::from_secs(60)),
            ..Default::default()
        },
        alpha_test_key: None,
        allow_insecure_ws: false,
        on_fatal: None,
    })
    .await
    .expect("initial connect");
    conn.force_reconnect();
    let first = tokio::time::timeout(Duration::from_secs(5), attempt_rx.recv())
        .await
        .expect("first episode")
        .expect("channel open");
    assert_eq!(first, 2);
    assert_eq!(
        conn.inner.outage_seq.load(Ordering::Relaxed),
        1,
        "first outage must advance outage_seq"
    );
    conn.force_reconnect();
    let second = tokio::time::timeout(Duration::from_secs(5), attempt_rx.recv())
        .await
        .expect("second episode")
        .expect("channel open");
    assert_eq!(
        second, 3,
        "flap inside the dwell must climb, not restart at 1"
    );
    assert_eq!(
        conn.inner.outage_seq.load(Ordering::Relaxed),
        2,
        "each outage must advance the jitter phase fed to reconnect_delay"
    );
    conn.request_shutdown();
    conn.await_shutdown().await;
}
async fn spawn_ack_only_hub() -> std::net::SocketAddr {
    use futures::{SinkExt as _, StreamExt as _};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock hub");
    let addr = listener.local_addr().expect("mock addr");
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await else {
                    return;
                };
                let _ = ws.next().await;
                let ack = serde_json::json!({
                    "connection_id": "mock",
                    "user_id": "test",
                    "computer_hub_version": "test",
                    "supported_protocol_versions": ["1.0.0"],
                });
                if ws
                    .send(tokio_tungstenite::tungstenite::Message::Text(
                        ack.to_string().into(),
                    ))
                    .await
                    .is_err()
                {
                    return;
                }
                while let Some(msg) = ws.next().await {
                    if msg.is_err() {
                        return;
                    }
                }
            });
        }
    });
    addr
}
async fn connect_tracking_attempts(
    addr: std::net::SocketAddr,
    reset_after: Duration,
    on_disconnect: Option<Arc<DisconnectCallback>>,
) -> (Arc<HubConnection>, mpsc::UnboundedReceiver<u32>) {
    let (attempt_tx, attempt_rx) = mpsc::unbounded_channel();
    let on_reconnect: Arc<ReconnectCallback> = Arc::new(Box::new(move |event: ReconnectEvent| {
        let _ = attempt_tx.send(event.attempt);
    }));
    let credential: Arc<dyn AuthProvider> = Arc::new(AuthCredential::bearer("test-token"));
    let conn = HubConnection::connect(ConnectionConfig {
        url: url::Url::parse(&format!("ws://{addr}/v1/tools")).expect("mock url"),
        credential,
        kind: ConnectionKind::ToolServer,
        on_reconnect: Some(on_reconnect),
        on_disconnect,
        on_terminal_close: None,
        on_connect: None,
        server_id: None,
        server_description: None,
        server_metadata: None,
        outbound_buffer: None,
        tuning: ConnectionTuning {
            reconnect_backoff: Some(Arc::from([Duration::from_millis(10)])),
            reconnect_attempt_reset_after: Some(reset_after),
            ..Default::default()
        },
        alpha_test_key: None,
        allow_insecure_ws: false,
        on_fatal: None,
    })
    .await
    .expect("initial connect");
    (conn, attempt_rx)
}
async fn recv_attempt(rx: &mut mpsc::UnboundedReceiver<u32>) -> u32 {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("reconnect event")
        .expect("channel open")
}
/// 1s dwell so a stalled worker between reconnect-complete and the next
/// forced outage cannot look stable.
#[tokio::test]
async fn attempt_reset_dwell_crosses_a_real_threshold() {
    let addr = spawn_ack_only_hub().await;
    let (conn, mut attempt_rx) =
        connect_tracking_attempts(addr, Duration::from_secs(1), None).await;
    conn.force_reconnect();
    assert_eq!(
        recv_attempt(&mut attempt_rx).await,
        1,
        "fresh connect is not yet stable"
    );
    conn.force_reconnect();
    assert_eq!(
        recv_attempt(&mut attempt_rx).await,
        2,
        "immediate flap must climb"
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    conn.force_reconnect();
    assert_eq!(
        recv_attempt(&mut attempt_rx).await,
        1,
        "after the dying socket outlived the dwell, attempt must reset"
    );
    conn.force_reconnect();
    assert_eq!(
        recv_attempt(&mut attempt_rx).await,
        2,
        "dwell must use the dying connection's age, not the actor lifetime"
    );
    conn.request_shutdown();
    conn.await_shutdown().await;
}
/// `on_disconnect` runs after detect-time capture and before the gate.
/// Sleeping past the dwell there must not reset.
#[tokio::test]
async fn attempt_reset_ignores_latency_between_detect_and_gate() {
    let addr = spawn_ack_only_hub().await;
    let on_disconnect: Arc<DisconnectCallback> = Arc::new(Box::new(|| {
        std::thread::sleep(Duration::from_secs(2));
    }));
    let (conn, mut attempt_rx) =
        connect_tracking_attempts(addr, Duration::from_secs(1), Some(on_disconnect)).await;
    conn.force_reconnect();
    assert_eq!(
        recv_attempt(&mut attempt_rx).await,
        1,
        "fresh connect is not yet stable"
    );
    conn.force_reconnect();
    assert_eq!(
        recv_attempt(&mut attempt_rx).await,
        2,
        "on_disconnect sleep past the dwell must not reset; detect-time age is still short"
    );
    conn.request_shutdown();
    conn.await_shutdown().await;
}
/// 4409 is reconnectable (not 41xx terminal). A drain followed by an
/// immediate redrop must climb the ladder, not reset to slot 1.
#[tokio::test]
async fn drain_4409_then_quick_redrop_climbs_attempt() {
    use futures::{SinkExt as _, StreamExt as _};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio_tungstenite::tungstenite::protocol::CloseFrame;
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock hub");
    let addr = listener.local_addr().expect("mock addr");
    let upgrades = Arc::new(AtomicUsize::new(0));
    let upgrades_srv = upgrades.clone();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let n = upgrades_srv.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await else {
                    return;
                };
                let _ = ws.next().await;
                let ack = serde_json::json!({
                    "connection_id": format!("mock-conn-{n}"),
                    "user_id": "test",
                    "computer_hub_version": "test",
                    "supported_protocol_versions": ["1.0.0"],
                });
                if ws
                    .send(tokio_tungstenite::tungstenite::Message::Text(
                        ack.to_string().into(),
                    ))
                    .await
                    .is_err()
                {
                    return;
                }
                if n < 2 {
                    let _ = ws
                        .send(tokio_tungstenite::tungstenite::Message::Close(Some(
                            CloseFrame {
                                code: CloseCode::from(4409),
                                reason: "drain".into(),
                            },
                        )))
                        .await;
                    return;
                }
                while let Some(msg) = ws.next().await {
                    if msg.is_err() {
                        return;
                    }
                }
            });
        }
    });
    let (attempt_tx, mut attempt_rx) = mpsc::unbounded_channel();
    let on_reconnect: Arc<ReconnectCallback> = Arc::new(Box::new(move |event: ReconnectEvent| {
        let _ = attempt_tx.send(event.attempt);
    }));
    let credential: Arc<dyn AuthProvider> = Arc::new(AuthCredential::bearer("test-token"));
    let conn = HubConnection::connect(ConnectionConfig {
        url: url::Url::parse(&format!("ws://{addr}/v1/tools")).expect("mock url"),
        credential,
        kind: ConnectionKind::ToolServer,
        on_reconnect: Some(on_reconnect),
        on_disconnect: None,
        on_terminal_close: None,
        on_connect: None,
        server_id: None,
        server_description: None,
        server_metadata: None,
        outbound_buffer: None,
        tuning: ConnectionTuning {
            reconnect_backoff: Some(Arc::from([Duration::from_millis(10)])),
            reconnect_attempt_reset_after: Some(Duration::from_secs(60)),
            ..Default::default()
        },
        alpha_test_key: None,
        allow_insecure_ws: false,
        on_fatal: None,
    })
    .await
    .expect("initial connect");
    let first = tokio::time::timeout(Duration::from_secs(5), attempt_rx.recv())
        .await
        .expect("first 4409 reconnect")
        .expect("channel open");
    assert_eq!(first, 1, "4409 must reconnect, not terminate");
    let second = tokio::time::timeout(Duration::from_secs(5), attempt_rx.recv())
        .await
        .expect("second 4409 reconnect")
        .expect("channel open");
    assert_eq!(
        second, 2,
        "immediate 4409 redrop must climb; a per-success reset would report 1"
    );
    conn.request_shutdown();
    conn.await_shutdown().await;
}
#[tokio::test]
async fn distinct_connections_use_distinct_jitter_seeds() {
    use futures::{SinkExt as _, StreamExt as _};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock hub");
    let addr = listener.local_addr().expect("mock addr");
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await else {
                    return;
                };
                let _ = ws.next().await;
                let ack = serde_json::json!({
                    "connection_id": "mock",
                    "user_id": "test",
                    "computer_hub_version": "test",
                    "supported_protocol_versions": ["1.0.0"],
                });
                if ws
                    .send(tokio_tungstenite::tungstenite::Message::Text(
                        ack.to_string().into(),
                    ))
                    .await
                    .is_err()
                {
                    return;
                }
                while let Some(msg) = ws.next().await {
                    if msg.is_err() {
                        return;
                    }
                }
            });
        }
    });
    let mk = || async {
        let credential: Arc<dyn AuthProvider> = Arc::new(AuthCredential::bearer("test-token"));
        HubConnection::connect(ConnectionConfig {
            url: url::Url::parse(&format!("ws://{addr}/v1/tools")).expect("mock url"),
            credential,
            kind: ConnectionKind::ToolServer,
            on_reconnect: None,
            on_disconnect: None,
            on_terminal_close: None,
            on_connect: None,
            server_id: None,
            server_description: None,
            server_metadata: None,
            outbound_buffer: None,
            tuning: ConnectionTuning::default(),
            alpha_test_key: None,
            allow_insecure_ws: false,
            on_fatal: None,
        })
        .await
        .expect("connect")
    };
    let a = mk().await;
    let b = mk().await;
    let sa = a.inner.reconnect_jitter_seed;
    let sb = b.inner.reconnect_jitter_seed;
    assert_ne!(sa, sb, "connect() must call new_reconnect_jitter_seed()");
    assert_eq!(
        a.inner.outage_seq.load(Ordering::Relaxed),
        0,
        "fresh connection has no reconnect outage yet"
    );
    assert_eq!(
        a.inner.attempt_reset_after,
        Duration::from_secs(10),
        "ConnectionTuning::default() must resolve None to the 10s production dwell"
    );
    assert_ne!(
        a.inner.reconnect_delay(1),
        b.inner.reconnect_delay(1),
        "seed must reach the delay used by the reader actor"
    );
    assert_eq!(
        a.inner.reconnect_delay(1),
        backoff_for(1, &a.inner.reconnect_backoff, sa, 0),
        "actor helper must use the stored seed and outage_seq, not literals"
    );
    a.request_shutdown();
    b.request_shutdown();
    a.await_shutdown().await;
    b.await_shutdown().await;
}
#[tokio::test]
async fn call_request_serialization_failure_registers_no_waiter() {
    struct FailingParams;
    impl serde::Serialize for FailingParams {
        fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("intentionally unserializable"))
        }
    }
    let (conn, demux, _outbound_rx) = test_connection();
    let session = SessionId::new("serde_fail_session").expect("valid");
    let request_id = conn.try_alloc_request_id().expect("request id");
    let probe_id = request_id.clone();
    let req = JsonRpcRequest {
        jsonrpc: JsonRpcVersion,
        id: JsonRpcId::from_request_id(&request_id),
        session_id: Some(session),
        method: Method::Hook.as_wire_str().to_owned(),
        params: FailingParams,
    };
    let result = conn.call_request(request_id, &req).await;
    assert!(result.is_err(), "serialization failure must surface");
    assert!(
        demux.take_response_waiter(&probe_id).is_none(),
        "no waiter may be registered when serialization fails"
    );
}
type WsError = tokio_tungstenite::tungstenite::Error;
type InboundTx = futures::channel::mpsc::UnboundedSender<Result<Message, WsError>>;
type InboundRx = futures::channel::mpsc::UnboundedReceiver<Result<Message, WsError>>;
/// In-memory inbound frame source for `run_reader_phase` tests
/// (mirrors `RecordingSink` for the writer half).
fn test_inbound() -> (InboundTx, InboundRx) {
    futures::channel::mpsc::unbounded()
}
/// A zero or unset liveness deadline resolves to `min(4× ping, 120s)`;
/// a positive override is honored verbatim. Mirrors the
/// `resolve_ws_ping_interval` clamp semantics.
#[test]
fn resolve_ws_liveness_deadline_clamps_zero_and_unset_to_default() {
    let ping = Duration::from_secs(30);
    assert_eq!(
        resolve_ws_liveness_deadline(None, ping),
        Duration::from_secs(120)
    );
    assert_eq!(
        resolve_ws_liveness_deadline(Some(Duration::ZERO), ping),
        Duration::from_secs(120)
    );
    let custom = Duration::from_secs(45);
    assert_eq!(resolve_ws_liveness_deadline(Some(custom), ping), custom);
}
/// The per-attempt reconnect budget tracks the liveness deadline above
/// the floor and is clamped to the floor below it, so a small liveness
/// override can never starve connection establishment.
#[test]
fn reconnect_attempt_budget_floors_small_deadlines() {
    assert_eq!(
        reconnect_attempt_budget(Duration::from_millis(2_500)),
        RECONNECT_ATTEMPT_MIN_BUDGET
    );
    assert_eq!(
        reconnect_attempt_budget(RECONNECT_ATTEMPT_MIN_BUDGET),
        RECONNECT_ATTEMPT_MIN_BUDGET
    );
    let large = Duration::from_secs(300);
    assert_eq!(reconnect_attempt_budget(large), large);
}
#[test]
fn resolve_ws_liveness_deadline_scales_with_ping_override() {
    assert_eq!(
        resolve_ws_liveness_deadline(None, Duration::from_secs(10)),
        Duration::from_secs(40)
    );
    assert_eq!(
        resolve_ws_liveness_deadline(None, Duration::from_secs(60)),
        Duration::from_secs(120),
        "default liveness is capped below hub idle_timeout",
    );
}
#[tokio::test(start_paused = true)]
async fn reader_deadline_kills_silently_dead_connection() {
    let (conn, _demux, _outbound_rx) = test_connection();
    let (inbound_tx, mut inbound_rx) = test_inbound();
    let (_stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
    let (_reconnect_tx, mut reconnect_rx) = mpsc::channel::<()>(1);
    let liveness = Duration::from_secs(75);
    let start = tokio::time::Instant::now();
    let exit = run_reader_phase(
        &conn.inner,
        &mut inbound_rx,
        &mut stop_rx,
        &mut reconnect_rx,
        liveness,
        &conn.inner.outbound_tx,
    )
    .await;
    assert!(matches!(
        exit,
        ConnectedExit::SocketClosed(DisconnectCause::LivenessDeadline)
    ));
    assert_eq!(
        start.elapsed(),
        liveness,
        "expiry exactly one liveness window after (re)entry"
    );
    drop(inbound_tx);
}
#[tokio::test(start_paused = true)]
async fn reader_deadline_rearms_on_rtt_proof_frames() {
    let (conn, _demux, _outbound_rx) = test_connection();
    let (inbound_tx, mut inbound_rx) = test_inbound();
    let (_stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
    let (_reconnect_tx, mut reconnect_rx) = mpsc::channel::<()>(1);
    let liveness = Duration::from_secs(75);
    let phase = run_reader_phase(
        &conn.inner,
        &mut inbound_rx,
        &mut stop_rx,
        &mut reconnect_rx,
        liveness,
        &conn.inner.outbound_tx,
    );
    tokio::pin!(phase);
    let frames = [
        Message::Pong(Vec::new().into()),
        Message::Text(r#"{"method":"pong","ts_ms":1}"#.into()),
        Message::Pong(Vec::new().into()),
        Message::Text(r#"{"method":"pong","ts_ms":2}"#.into()),
    ];
    for frame in frames {
        tokio::time::advance(liveness * 3 / 4).await;
        inbound_tx.unbounded_send(Ok(frame)).expect("send frame");
        assert!(
            futures::poll!(phase.as_mut()).is_pending(),
            "phase must stay live while RTT-proof frames keep arriving"
        );
    }
    tokio::time::advance(liveness - Duration::from_millis(1)).await;
    assert!(
        futures::poll!(phase.as_mut()).is_pending(),
        "still inside the window re-armed by the last frame"
    );
    tokio::time::advance(Duration::from_millis(1)).await;
    match futures::poll!(phase.as_mut()) {
        std::task::Poll::Ready(exit) => {
            assert!(matches!(
                exit,
                ConnectedExit::SocketClosed(DisconnectCause::LivenessDeadline)
            ));
        }
        std::task::Poll::Pending => {
            panic!("deadline must fire one window after the last frame")
        }
    }
}
#[test]
fn classify_inbound_hub_ping_is_app_ping_not_data() {
    let (conn, _demux, _outbound_rx) = test_connection();
    assert!(
        matches!(
            classify_inbound_text(&conn.inner, r#"{"method":"ping","ts_ms":1}"#),
            InboundText::AppPing { .. }
        ),
        "hub app ping must classify as AppPing"
    );
    assert!(
        matches!(
            classify_inbound_text(&conn.inner, r#"{"method":"pong","ts_ms":1}"#),
            InboundText::AppPong
        ),
        "hub app pong must classify as AppPong"
    );
}
#[tokio::test(start_paused = true)]
async fn reader_deadline_ignores_inbound_only_hub_pings() {
    let (conn, _demux, _outbound_rx) = test_connection();
    let (prio_tx, prio_rx) = mpsc::channel::<String>(4);
    drop(prio_rx);
    let (inbound_tx, mut inbound_rx) = test_inbound();
    let (_stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
    let (_reconnect_tx, mut reconnect_rx) = mpsc::channel::<()>(1);
    let liveness = Duration::from_secs(75);
    let phase = run_reader_phase(
        &conn.inner,
        &mut inbound_rx,
        &mut stop_rx,
        &mut reconnect_rx,
        liveness,
        &prio_tx,
    );
    tokio::pin!(phase);
    assert!(
        futures::poll!(phase.as_mut()).is_pending(),
        "phase must start pending"
    );
    let pong_dropped_before = crate::metrics::heartbeat_pong_dropped_count();
    for _ in 0..3 {
        inbound_tx
            .unbounded_send(Ok(Message::Text(r#"{"method":"ping","ts_ms":1}"#.into())))
            .expect("send hub ping");
        inbound_tx
            .unbounded_send(Ok(Message::Ping(Vec::new().into())))
            .expect("send ws ping");
        assert!(
            futures::poll!(phase.as_mut()).is_pending(),
            "inbound-only pings must not kill early"
        );
    }
    tokio::time::advance(liveness * 3 / 4).await;
    inbound_tx
        .unbounded_send(Ok(Message::Text(r#"{"method":"ping","ts_ms":2}"#.into())))
        .expect("late hub ping");
    assert!(
        futures::poll!(phase.as_mut()).is_pending(),
        "late hub ping must not re-arm"
    );
    tokio::time::advance(liveness / 4 - Duration::from_millis(1)).await;
    assert!(
        futures::poll!(phase.as_mut()).is_pending(),
        "still inside the original liveness window"
    );
    tokio::time::advance(Duration::from_millis(1)).await;
    let mut exit = None;
    for _ in 0..16 {
        match futures::poll!(phase.as_mut()) {
            std::task::Poll::Ready(e) => {
                exit = Some(e);
                break;
            }
            std::task::Poll::Pending => tokio::task::yield_now().await,
        }
    }
    match exit.expect("deadline should fire on original L") {
        ConnectedExit::SocketClosed(DisconnectCause::LivenessDeadline) => {}
        ConnectedExit::SocketClosed(cause) => panic!("wrong cause {}", cause.label()),
        ConnectedExit::Stop => panic!("stop"),
        ConnectedExit::TerminalClose(code) => panic!("terminal {code}"),
    }
    assert!(
        crate::metrics::heartbeat_pong_dropped_count() > pong_dropped_before,
        "dropped priority rx must count heartbeat_pong_dropped"
    );
}
#[tokio::test(start_paused = true)]
async fn reader_deadline_huge_override_saturates_instead_of_panicking() {
    let (conn, _demux, _outbound_rx) = test_connection();
    let (inbound_tx, mut inbound_rx) = test_inbound();
    let (_stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
    let (_reconnect_tx, mut reconnect_rx) = mpsc::channel::<()>(1);
    let phase = run_reader_phase(
        &conn.inner,
        &mut inbound_rx,
        &mut stop_rx,
        &mut reconnect_rx,
        Duration::MAX,
        &conn.inner.outbound_tx,
    );
    tokio::pin!(phase);
    inbound_tx
        .unbounded_send(Ok(Message::Pong(Vec::new().into())))
        .expect("send frame");
    assert!(
        futures::poll!(phase.as_mut()).is_pending(),
        "saturating re-arm must neither panic nor fire"
    );
}
/// Sink for writer↔reader composition tests: echoes every keepalive
/// `Ping` back as a `Pong` on the reader's inbound channel, emulating a
/// healthy server whose only traffic is the keepalive exchange.
struct PongEchoSink {
    inbound: InboundTx,
}
impl futures::Sink<Message> for PongEchoSink {
    type Error = std::io::Error;
    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        match item {
            Message::Ping(payload) => {
                let _ = self.inbound.unbounded_send(Ok(Message::Pong(payload)));
            }
            Message::Text(text) => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(text.as_ref())
                    && v.get("method").and_then(serde_json::Value::as_str) == Some("ping")
                    && let Ok(pong) = serde_json::to_string(&PongFrame::new(now_unix_millis()))
                {
                    let _ = self.inbound.unbounded_send(Ok(Message::Text(pong.into())));
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}
#[tokio::test(start_paused = true)]
async fn default_ping_pong_composition_keeps_idle_connection_alive() {
    let ping = resolve_ws_ping_interval(None);
    let deadline = resolve_ws_liveness_deadline(None, ping);
    let (conn, _demux, _outbound_rx) = test_connection();
    let (inbound_tx, mut inbound_rx) = test_inbound();
    let (_out_tx, out_rx) = mpsc::channel::<String>(4);
    let (ctl_tx, ctl_rx) = mpsc::channel::<WriterControl<PongEchoSink>>(2);
    let (writer_stop_tx, writer_stop_rx) = mpsc::channel::<()>(1);
    let (_prio_tx, prio_rx) = mpsc::channel::<String>(4);
    let writer = tokio::spawn(run_writer(
        PongEchoSink {
            inbound: inbound_tx.clone(),
        },
        out_rx,
        prio_rx,
        ctl_rx,
        writer_stop_rx,
        Some(ping),
        idle_write_error_slot(),
        None,
    ));
    let (_stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
    let (_reconnect_tx, mut reconnect_rx) = mpsc::channel::<()>(1);
    {
        let phase = run_reader_phase(
            &conn.inner,
            &mut inbound_rx,
            &mut stop_rx,
            &mut reconnect_rx,
            deadline,
            &conn.inner.outbound_tx,
        );
        tokio::pin!(phase);
        tokio::select! {
            _ = phase.as_mut() => panic!("idle-but-healthy connection tripped the deadline"),
            _ = tokio::time::sleep(deadline * 4) => {}
        }
    }
    ctl_tx.send(WriterControl::Pause).await.expect("pause");
    let (fresh_tx, mut fresh_rx) = test_inbound();
    ctl_tx
        .send(WriterControl::Resume(PongEchoSink { inbound: fresh_tx }))
        .await
        .expect("resume");
    {
        let phase = run_reader_phase(
            &conn.inner,
            &mut fresh_rx,
            &mut stop_rx,
            &mut reconnect_rx,
            deadline,
            &conn.inner.outbound_tx,
        );
        tokio::pin!(phase);
        tokio::select! {
            _ = phase.as_mut() => {
                panic!("idle connection tripped the deadline after Pause→Resume")
            }
            _ = tokio::time::sleep(deadline * 4) => {}
        }
    }
    writer_stop_tx.send(()).await.expect("stop");
    writer.await.expect("writer task joins");
}
struct AppPingOnlyEchoSink {
    inbound: InboundTx,
}
impl futures::Sink<Message> for AppPingOnlyEchoSink {
    type Error = std::io::Error;
    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        if let Message::Text(text) = item
            && text.contains("\"method\":\"ping\"")
            && text.contains("ts_ms")
        {
            let pong = serde_json::to_string(&PongFrame::new(1)).expect("pong");
            let _ = self.inbound.unbounded_send(Ok(Message::Text(pong.into())));
        }
        Ok(())
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}
#[tokio::test(start_paused = true)]
async fn app_ping_only_keeps_idle_connection_alive_without_ws_pongs() {
    let ping = resolve_ws_ping_interval(None);
    let deadline = resolve_ws_liveness_deadline(None, ping);
    let (conn, _demux, _outbound_rx) = test_connection();
    let (inbound_tx, mut inbound_rx) = test_inbound();
    let (_out_tx, out_rx) = mpsc::channel::<String>(4);
    let (_ctl_tx, ctl_rx) = mpsc::channel::<WriterControl<AppPingOnlyEchoSink>>(2);
    let (writer_stop_tx, writer_stop_rx) = mpsc::channel::<()>(1);
    let (_prio_tx, prio_rx) = mpsc::channel::<String>(4);
    let writer = tokio::spawn(run_writer(
        AppPingOnlyEchoSink {
            inbound: inbound_tx,
        },
        out_rx,
        prio_rx,
        ctl_rx,
        writer_stop_rx,
        Some(ping),
        idle_write_error_slot(),
        None,
    ));
    let (_stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
    let (_reconnect_tx, mut reconnect_rx) = mpsc::channel::<()>(1);
    let phase = run_reader_phase(
        &conn.inner,
        &mut inbound_rx,
        &mut stop_rx,
        &mut reconnect_rx,
        deadline,
        &conn.inner.outbound_tx,
    );
    tokio::pin!(phase);
    tokio::select! {
        _ = phase.as_mut() => panic!("app-pong-only keepalive tripped liveness"),
        _ = tokio::time::sleep(deadline * 4) => {}
    }
    writer_stop_tx.send(()).await.expect("stop");
    writer.await.expect("join");
}
#[tokio::test]
async fn reader_phase_close_frame_classification_unchanged() {
    use tokio_tungstenite::tungstenite::protocol::CloseFrame;
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
    let (conn, _demux, _outbound_rx) = test_connection();
    let (inbound_tx, mut inbound_rx) = test_inbound();
    let (_stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
    let (_reconnect_tx, mut reconnect_rx) = mpsc::channel::<()>(1);
    inbound_tx
        .unbounded_send(Ok(Message::Close(Some(CloseFrame {
            code: CloseCode::from(4100),
            reason: "evicted".into(),
        }))))
        .expect("send close");
    let exit = run_reader_phase(
        &conn.inner,
        &mut inbound_rx,
        &mut stop_rx,
        &mut reconnect_rx,
        Duration::from_secs(75),
        &conn.inner.outbound_tx,
    )
    .await;
    assert!(matches!(exit, ConnectedExit::TerminalClose(4100)));
}
async fn spawn_hub_close_after_ack(close: Option<u16>) -> std::net::SocketAddr {
    use futures::{SinkExt as _, StreamExt as _};
    use tokio_tungstenite::tungstenite::protocol::CloseFrame;
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock hub");
    let addr = listener.local_addr().expect("mock addr");
    tokio::spawn(async move {
        let Ok((tcp, _)) = listener.accept().await else {
            return;
        };
        let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await else {
            return;
        };
        let _ = ws.next().await;
        let ack = serde_json::json!({
            "connection_id": "mock",
            "user_id": "test",
            "computer_hub_version": "test",
            "supported_protocol_versions": ["1.0.0"],
        });
        if ws
            .send(tokio_tungstenite::tungstenite::Message::Text(
                ack.to_string().into(),
            ))
            .await
            .is_err()
        {
            return;
        }
        match close {
            Some(code) => {
                let _ = ws
                    .send(tokio_tungstenite::tungstenite::Message::Close(Some(
                        CloseFrame {
                            code: CloseCode::from(code),
                            reason: "test".into(),
                        },
                    )))
                    .await;
            }
            None => drop(ws),
        }
    });
    addr
}
#[tokio::test]
async fn terminal_close_fires_on_terminal_close_then_on_disconnect() {
    let events = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let terminal_events = Arc::clone(&events);
    let disconnect_events = Arc::clone(&events);
    let addr = spawn_hub_close_after_ack(Some(4103)).await;
    let credential: Arc<dyn AuthProvider> = Arc::new(AuthCredential::bearer("test-token"));
    let conn = HubConnection::connect(ConnectionConfig {
        url: url::Url::parse(&format!("ws://{addr}/v1/tools")).expect("mock url"),
        credential,
        kind: ConnectionKind::ToolServer,
        on_reconnect: None,
        on_disconnect: Some(Arc::new(Box::new(move || {
            disconnect_events
                .lock()
                .expect("events")
                .push("disconnect".into());
        }))),
        on_terminal_close: Some(Arc::new(Box::new(move |code| {
            terminal_events
                .lock()
                .expect("events")
                .push(format!("terminal:{code}"));
        }))),
        on_connect: None,
        server_id: None,
        server_description: None,
        server_metadata: None,
        outbound_buffer: None,
        tuning: ConnectionTuning::default(),
        alpha_test_key: None,
        allow_insecure_ws: false,
        on_fatal: None,
    })
    .await
    .expect("initial connect");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot = events.lock().expect("events").clone();
        if snapshot.as_slice() == ["terminal:4103", "disconnect"] {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "callbacks not observed in order: {snapshot:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    conn.request_shutdown();
    conn.await_shutdown().await;
}
#[tokio::test]
async fn terminal_close_stops_actor_by_default() {
    let addr = spawn_hub_close_after_ack(Some(4103)).await;
    let credential: Arc<dyn AuthProvider> = Arc::new(AuthCredential::bearer("test-token"));
    let conn = HubConnection::connect(ConnectionConfig {
        url: url::Url::parse(&format!("ws://{addr}/v1/tools")).expect("mock url"),
        credential,
        kind: ConnectionKind::ToolServer,
        on_reconnect: None,
        on_disconnect: None,
        on_terminal_close: None,
        on_connect: None,
        server_id: None,
        server_description: None,
        server_metadata: None,
        outbound_buffer: None,
        tuning: ConnectionTuning::default(),
        alpha_test_key: None,
        allow_insecure_ws: false,
        on_fatal: None,
    })
    .await
    .expect("initial connect");
    tokio::time::timeout(Duration::from_secs(5), conn.await_shutdown())
        .await
        .expect("default terminal close must stop the actor without an embedder shutdown");
}
async fn spawn_hub_close_then_accept(close: u16) -> std::net::SocketAddr {
    use futures::{SinkExt as _, StreamExt as _};
    use tokio_tungstenite::tungstenite::protocol::CloseFrame;
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock hub");
    let addr = listener.local_addr().expect("mock addr");
    tokio::spawn(async move {
        for stay_up in [false, true] {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await else {
                return;
            };
            let _ = ws.next().await;
            let ack = serde_json::json!({
                "connection_id": if stay_up { "mock-reconnected" } else { "mock" },
                "user_id": "test",
                "computer_hub_version": "test",
                "supported_protocol_versions": ["1.0.0"],
            });
            if ws
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    ack.to_string().into(),
                ))
                .await
                .is_err()
            {
                return;
            }
            if stay_up {
                while let Some(Ok(_)) = ws.next().await {}
                return;
            }
            let _ = ws
                .send(tokio_tungstenite::tungstenite::Message::Close(Some(
                    CloseFrame {
                        code: CloseCode::from(close),
                        reason: "test".into(),
                    },
                )))
                .await;
        }
    });
    addr
}
#[tokio::test]
async fn terminal_close_reconnects_when_embedder_opts_in() {
    let reconnects = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let reconnects_cb = Arc::clone(&reconnects);
    let terminals = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let terminals_cb = Arc::clone(&terminals);
    let addr = spawn_hub_close_then_accept(4103).await;
    let credential: Arc<dyn AuthProvider> = Arc::new(AuthCredential::bearer("test-token"));
    let conn = HubConnection::connect(ConnectionConfig {
        url: url::Url::parse(&format!("ws://{addr}/v1/tools")).expect("mock url"),
        credential,
        kind: ConnectionKind::ToolServer,
        on_reconnect: Some(Arc::new(Box::new(move |_event| {
            reconnects_cb.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }))),
        on_disconnect: None,
        on_terminal_close: Some(Arc::new(Box::new(move |_code| {
            terminals_cb.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }))),
        on_connect: None,
        server_id: None,
        server_description: None,
        server_metadata: None,
        outbound_buffer: None,
        tuning: ConnectionTuning {
            reconnect_backoff: Some(Arc::from([Duration::from_millis(10)])),
            reconnect_after_terminal_close_codes: vec![4103],
            ..Default::default()
        },
        alpha_test_key: None,
        allow_insecure_ws: false,
        on_fatal: None,
    })
    .await
    .expect("initial connect");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if reconnects.load(std::sync::atomic::Ordering::SeqCst) >= 1
            && terminals.load(std::sync::atomic::Ordering::SeqCst) >= 1
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "opt-in terminal close must fire on_terminal_close then reconnect"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        1,
        terminals.load(std::sync::atomic::Ordering::SeqCst),
        "terminal-close callback still fires when reconnect is opted in"
    );
    conn.request_shutdown();
    conn.await_shutdown().await;
}
#[tokio::test]
async fn non_allowlisted_terminal_close_stops_actor_despite_allowlist() {
    for code in [4100u16, 4101, 4102, 4104] {
        let reconnects = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let reconnects_cb = Arc::clone(&reconnects);
        let terminals = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let terminals_cb = Arc::clone(&terminals);
        let addr = spawn_hub_close_then_accept(code).await;
        let credential: Arc<dyn AuthProvider> = Arc::new(AuthCredential::bearer("test-token"));
        let conn = HubConnection::connect(ConnectionConfig {
            url: url::Url::parse(&format!("ws://{addr}/v1/tools")).expect("mock url"),
            credential,
            kind: ConnectionKind::ToolServer,
            on_reconnect: Some(Arc::new(Box::new(move |_event| {
                reconnects_cb.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }))),
            on_disconnect: None,
            on_terminal_close: Some(Arc::new(Box::new(move |_code| {
                terminals_cb.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }))),
            on_connect: None,
            server_id: None,
            server_description: None,
            server_metadata: None,
            outbound_buffer: None,
            tuning: ConnectionTuning {
                reconnect_backoff: Some(Arc::from([Duration::from_millis(10)])),
                reconnect_after_terminal_close_codes: vec![4103],
                ..Default::default()
            },
            alpha_test_key: None,
            allow_insecure_ws: false,
            on_fatal: None,
        })
        .await
        .expect("initial connect");
        tokio::time::timeout(Duration::from_secs(5), conn.await_shutdown())
            .await
            .unwrap_or_else(|_| panic!("non-allowlisted close {code} must stop the actor"));
        assert_eq!(
            1,
            terminals.load(std::sync::atomic::Ordering::SeqCst),
            "terminal-close callback fires once for {code}"
        );
        assert_eq!(
            0,
            reconnects.load(std::sync::atomic::Ordering::SeqCst),
            "non-allowlisted close {code} must not reconnect"
        );
    }
}
#[tokio::test]
async fn default_terminal_close_never_reconnects_for_any_41xx() {
    for code in [4100u16, 4101, 4102, 4103, 4104] {
        let reconnects = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let reconnects_cb = Arc::clone(&reconnects);
        let addr = spawn_hub_close_then_accept(code).await;
        let credential: Arc<dyn AuthProvider> = Arc::new(AuthCredential::bearer("test-token"));
        let conn = HubConnection::connect(ConnectionConfig {
            url: url::Url::parse(&format!("ws://{addr}/v1/tools")).expect("mock url"),
            credential,
            kind: ConnectionKind::ToolServer,
            on_reconnect: Some(Arc::new(Box::new(move |_event| {
                reconnects_cb.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }))),
            on_disconnect: None,
            on_terminal_close: None,
            on_connect: None,
            server_id: None,
            server_description: None,
            server_metadata: None,
            outbound_buffer: None,
            tuning: ConnectionTuning {
                reconnect_backoff: Some(Arc::from([Duration::from_millis(10)])),
                ..Default::default()
            },
            alpha_test_key: None,
            allow_insecure_ws: false,
            on_fatal: None,
        })
        .await
        .expect("initial connect");
        tokio::time::timeout(Duration::from_secs(5), conn.await_shutdown())
            .await
            .unwrap_or_else(|_| panic!("default close {code} must stop the actor"));
        assert_eq!(
            0,
            reconnects.load(std::sync::atomic::Ordering::SeqCst),
            "default (empty allowlist) close {code} must not reconnect"
        );
    }
}
#[tokio::test]
async fn socket_close_does_not_fire_on_terminal_close() {
    let terminal = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let disconnect = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let terminal_cb = Arc::clone(&terminal);
    let disconnect_cb = Arc::clone(&disconnect);
    let addr = spawn_hub_close_after_ack(Some(1000)).await;
    let credential: Arc<dyn AuthProvider> = Arc::new(AuthCredential::bearer("test-token"));
    let conn = HubConnection::connect(ConnectionConfig {
        url: url::Url::parse(&format!("ws://{addr}/v1/tools")).expect("mock url"),
        credential,
        kind: ConnectionKind::ToolServer,
        on_reconnect: None,
        on_disconnect: Some(Arc::new(Box::new(move || {
            disconnect_cb.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }))),
        on_terminal_close: Some(Arc::new(Box::new(move |_code| {
            terminal_cb.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }))),
        on_connect: None,
        server_id: None,
        server_description: None,
        server_metadata: None,
        outbound_buffer: None,
        tuning: ConnectionTuning {
            reconnect_backoff: Some(Arc::from([Duration::from_secs(60)])),
            ..Default::default()
        },
        alpha_test_key: None,
        allow_insecure_ws: false,
        on_fatal: None,
    })
    .await
    .expect("initial connect");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while disconnect.load(std::sync::atomic::Ordering::SeqCst) == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "on_disconnect must fire on a non-terminal close"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        0,
        terminal.load(std::sync::atomic::Ordering::SeqCst),
        "socket close must not invoke on_terminal_close"
    );
    conn.request_shutdown();
    conn.await_shutdown().await;
}
