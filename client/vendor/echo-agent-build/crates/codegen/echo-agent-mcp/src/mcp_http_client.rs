//! MCP HTTP client wrapper that throttles SSE reconnects with exponential
//! backoff, working around rmcp's zero-backoff reconnect loop: when an
//! established SSE stream errors, rmcp re-issues the `GET` immediately with
//! its retry counter reset to 0, never consulting its `SseRetryPolicy`
//! (only connect failures and graceful EOF consult it). We ship rmcp 2.1;
//! still unfixed upstream as of rmcp 2.1.0:
//! <https://github.com/modelcontextprotocol/rust-sdk/blob/rmcp-v2.1.0/crates/rmcp/src/transport/common/client_side_sse.rs#L250-L261>

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::stream::BoxStream;
use http::{HeaderName, HeaderValue};
use rmcp::model::ClientJsonRpcMessage;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClient, StreamableHttpError, StreamableHttpPostResponse,
};
use sse_stream::{Error as SseError, Sse};

/// A stream that survived this long is healthy and resets the backoff. Flood
/// lifetimes are sub-millisecond; healthy proxies/LBs recycle idle streams no
/// faster than ~25s.
const STABLE_STREAM_THRESHOLD: Duration = Duration::from_secs(2);
/// Delay for the n-th consecutive rapid death: `BASE_DELAY * 2^(n-2)`
/// (the first reconnects immediately), capped at [`MAX_DELAY`].
const BASE_DELAY: Duration = Duration::from_millis(500);
/// Caps a broken server's cost at ~2 attempts/min; a healed server gets its
/// stream back within 30s.
const MAX_DELAY: Duration = Duration::from_secs(30);
const WARN_COOLDOWN: Duration = Duration::from_secs(60 * 60);

/// How to log a throttled reconnect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReconnectLog {
    Warn,
    SuppressedWarn,
    Debug,
}

#[derive(Debug, Clone, Copy)]
struct BackoffPlan {
    attempt: u32,
    delay: Duration,
    log: ReconnectLog,
}

/// Hold one per `McpClient`; clones share state, so transport rebuilds keep
/// the limit.
#[derive(Debug, Clone, Default)]
pub struct WarnBudget(Arc<parking_lot::Mutex<Option<Instant>>>);

impl WarnBudget {
    /// Latches `now` and returns true when no warn fired within
    /// `WARN_COOLDOWN`. Never acquire a `ThrottleState` lock while holding
    /// this one.
    fn try_consume(&self, now: Instant) -> bool {
        let mut last_warn_at = self.0.lock();
        let available = last_warn_at.is_none_or(|t| now.duration_since(t) >= WARN_COOLDOWN);
        if available {
            *last_warn_at = Some(now);
        }
        available
    }

    #[cfg(test)]
    fn last_warn_at(&self) -> Option<Instant> {
        *self.0.lock()
    }
}

#[derive(Debug, Default)]
struct ThrottleState {
    /// Age at the next `get_stream` approximates the previous stream's
    /// lifetime, since reconnects follow deaths within a round trip.
    last_established: Option<Instant>,
    consecutive_rapid: u32,
    warn_budget: WarnBudget,
    /// Limits each episode to one warn; cleared when the episode resets.
    /// The warn may fire late if the cooldown held it back at episode entry.
    episode_warned: bool,
}

impl ThrottleState {
    fn with_budget(warn_budget: WarnBudget) -> Self {
        Self {
            warn_budget,
            ..Self::default()
        }
    }

    fn delay_for_attempt(attempt: u32) -> Duration {
        // 2^6 * BASE_DELAY already saturates MAX_DELAY; clamp guards pow overflow.
        let exp = attempt.saturating_sub(2).min(6);
        (BASE_DELAY * 2u32.pow(exp)).min(MAX_DELAY)
    }

    fn plan_on_get_stream(&mut self, now: Instant) -> Option<BackoffPlan> {
        let rapid = self
            .last_established
            .is_some_and(|t| now.duration_since(t) < STABLE_STREAM_THRESHOLD);
        if rapid {
            self.consecutive_rapid = self.consecutive_rapid.saturating_add(1);
        } else {
            self.consecutive_rapid = 0;
            self.episode_warned = false;
        }
        let attempt = self.consecutive_rapid;
        if attempt < 2 {
            return None;
        }
        let log = if self.episode_warned {
            ReconnectLog::Debug
        } else if self.warn_budget.try_consume(now) {
            self.episode_warned = true;
            ReconnectLog::Warn
        } else {
            ReconnectLog::SuppressedWarn
        };
        Some(BackoffPlan {
            attempt,
            delay: Self::delay_for_attempt(attempt),
            log,
        })
    }

    fn mark_established(&mut self, at: Instant) {
        self.last_established = Some(at);
    }
}

/// Wraps any [`StreamableHttpClient`] and backs off `get_stream` reconnects;
/// `post_message` / `delete_session` delegate untouched. Clones share the
/// throttle state (rmcp clones the client per stream task / reconnect).
///
/// Backoff and episode state are per instance; the [`WarnBudget`] is the
/// caller's, so a rebuilt client does not warn again within the cooldown.
#[derive(Clone)]
pub struct McpHttpClient<C> {
    inner: C,
    server_name: Arc<str>,
    state: Arc<parking_lot::Mutex<ThrottleState>>,
}
// No `Debug` derive: rmcp's `AuthClient` (an inner type) is not `Debug`.

impl<C> McpHttpClient<C> {
    pub fn new(inner: C, server_name: impl Into<Arc<str>>, warn_budget: WarnBudget) -> Self {
        Self {
            inner,
            server_name: server_name.into(),
            state: Arc::new(parking_lot::Mutex::new(ThrottleState::with_budget(
                warn_budget,
            ))),
        }
    }
}

/// The system clock in production; the paused clock under `start_paused`
/// tests. Use this for all throttle timing so timing tests stay
/// deterministic.
fn now() -> Instant {
    tokio::time::Instant::now().into_std()
}

// `C: Sync` because the trait's `+ Send` futures borrow `&self`.
impl<C: StreamableHttpClient + Sync> StreamableHttpClient for McpHttpClient<C> {
    type Error = C::Error;

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, StreamableHttpError<Self::Error>> {
        let plan = {
            let mut st = self.state.lock();
            st.plan_on_get_stream(now())
        };

        if let Some(plan) = plan {
            match plan.log {
                ReconnectLog::Warn => {
                    tracing::warn!(
                        server = %self.server_name,
                        uri = %uri,
                        attempt = plan.attempt,
                        delay_ms = plan.delay.as_millis() as u64,
                        max_delay_ms = MAX_DELAY.as_millis() as u64,
                        cooldown_secs = WARN_COOLDOWN.as_secs(),
                        "MCP SSE stream keeps dying immediately after connect; \
                         backing off reconnects (capped, retries forever)"
                    );
                }
                ReconnectLog::SuppressedWarn | ReconnectLog::Debug => {
                    tracing::debug!(
                        server = %self.server_name,
                        uri = %uri,
                        attempt = plan.attempt,
                        delay_ms = plan.delay.as_millis() as u64,
                        suppressed_warn = plan.log == ReconnectLog::SuppressedWarn,
                        "MCP SSE reconnect backoff"
                    );
                }
            }
            tokio::time::sleep(plan.delay).await;
        }

        let result = self
            .inner
            .get_stream(uri, session_id, last_event_id, auth_token, custom_headers)
            .await;
        if result.is_ok() {
            self.state.lock().mark_established(now());
        }
        result
    }

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        self.inner
            .post_message(uri, message, session_id, auth_token, custom_headers)
            .await
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), StreamableHttpError<Self::Error>> {
        self.inner
            .delete_session(uri, session_id, auth_token, custom_headers)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulates rapid stream deaths starting at `start` until the throttle
    /// engages (attempt 2). Returns the throttle-entry time and its plan.
    fn drive_to_first_throttle(st: &mut ThrottleState, start: Instant) -> (Instant, BackoffPlan) {
        assert!(st.plan_on_get_stream(start).is_none());
        st.mark_established(start);
        let t1 = start + Duration::from_millis(10);
        assert!(st.plan_on_get_stream(t1).is_none());
        st.mark_established(t1);
        let t2 = t1 + Duration::from_millis(10);
        let plan = st.plan_on_get_stream(t2).expect("attempt 2 is throttled");
        assert_eq!(plan.attempt, 2);
        (t2, plan)
    }

    #[test]
    fn warn_lifecycle_across_episodes_and_cooldowns() {
        let mut st = ThrottleState::default();

        // Episode 1: the entry attempt warns once; later attempts stay debug.
        let (t2, p2) = drive_to_first_throttle(&mut st, Instant::now());
        assert_eq!(p2.delay, BASE_DELAY);
        assert_eq!(p2.log, ReconnectLog::Warn);
        let first_warn_at = st.warn_budget.last_warn_at().expect("latched");
        st.mark_established(t2);
        let p3 = st
            .plan_on_get_stream(t2 + Duration::from_millis(10))
            .expect("throttled");
        assert_eq!(p3.log, ReconnectLog::Debug);
        st.mark_established(t2 + Duration::from_millis(10));

        // Stable recovery, then a new outage inside the cooldown: the
        // episode reset must not reset the cooldown, so entry is suppressed.
        let (mut t, p_entry) = drive_to_first_throttle(&mut st, t2 + Duration::from_secs(30 * 60));
        assert_eq!(p_entry.log, ReconnectLog::SuppressedWarn);
        st.mark_established(t);

        // The outage continues: every attempt is suppressed until the
        // cooldown expires, then the episode's one warn fires late.
        let step = STABLE_STREAM_THRESHOLD - Duration::from_millis(1);
        let rearm_at = first_warn_at + WARN_COOLDOWN;
        let late_warn_at = loop {
            t += step;
            let p = st.plan_on_get_stream(t).expect("throttled");
            st.mark_established(t);
            match p.log {
                ReconnectLog::SuppressedWarn => assert!(t < rearm_at),
                ReconnectLog::Warn => {
                    assert!(t >= rearm_at, "late warn must wait for the cooldown");
                    assert!(p.attempt > 2, "late warn fires mid-episode");
                    break t;
                }
                ReconnectLog::Debug => {
                    panic!("Debug event before the episode's warn fired")
                }
            }
        };

        // Same outage, another full cooldown: elapsed time alone must not
        // produce more warns.
        let past_next_cooldown = late_warn_at + WARN_COOLDOWN + Duration::from_secs(1);
        let mut attempts = 0u32;
        while t < past_next_cooldown {
            t += step;
            attempts += 1;
            let p = st.plan_on_get_stream(t).expect("throttled");
            assert_eq!(p.log, ReconnectLog::Debug);
            st.mark_established(t);
        }
        let min_attempts = (WARN_COOLDOWN.as_millis() / step.as_millis()) as u32;
        assert!(
            attempts >= min_attempts,
            "loop must cross the cooldown window"
        );
    }

    #[test]
    fn suppressed_episode_that_recovers_drops_its_warn() {
        let mut st = ThrottleState::default();
        let (t2, p2) = drive_to_first_throttle(&mut st, Instant::now());
        assert_eq!(p2.log, ReconnectLog::Warn);
        let first_warn_at = st.warn_budget.last_warn_at().expect("latched");
        st.mark_established(t2);

        let (t_ep2, p_ep2) = drive_to_first_throttle(&mut st, t2 + Duration::from_secs(30 * 60));
        assert_eq!(p_ep2.log, ReconnectLog::SuppressedWarn);
        st.mark_established(t_ep2);

        // Land the third episode's throttle entry exactly on the cooldown
        // boundary, which is inclusive.
        let rearm_at = first_warn_at + WARN_COOLDOWN;
        let (entry, p_ep3) = drive_to_first_throttle(&mut st, rearm_at - Duration::from_millis(20));
        assert_eq!(entry, rearm_at);
        assert_eq!(p_ep3.log, ReconnectLog::Warn);
    }

    /// A rebuilt client for the same server shares the warn budget, so it
    /// does not warn again within the cooldown.
    #[test]
    fn rebuilt_client_shares_the_server_warn_budget() {
        let budget = WarnBudget::default();
        let mut st1 = ThrottleState::with_budget(budget.clone());
        let (t2, p2) = drive_to_first_throttle(&mut st1, Instant::now());
        assert_eq!(p2.log, ReconnectLog::Warn);

        let mut st2 = ThrottleState::with_budget(budget);
        let (_, p_rebuilt) = drive_to_first_throttle(&mut st2, t2 + Duration::from_secs(60));
        assert_eq!(p_rebuilt.log, ReconnectLog::SuppressedWarn);
    }

    /// Inner client whose streams always succeed and end immediately,
    /// simulating rapid stream deaths.
    #[derive(Clone)]
    struct MockInner;

    impl StreamableHttpClient for MockInner {
        type Error = std::io::Error;

        async fn get_stream(
            &self,
            _uri: Arc<str>,
            _session_id: Option<Arc<str>>,
            _last_event_id: Option<String>,
            _auth_token: Option<String>,
            _custom_headers: HashMap<HeaderName, HeaderValue>,
        ) -> Result<BoxStream<'static, Result<Sse, SseError>>, StreamableHttpError<Self::Error>>
        {
            Ok(Box::pin(futures::stream::empty()))
        }

        async fn post_message(
            &self,
            _uri: Arc<str>,
            _message: ClientJsonRpcMessage,
            _session_id: Option<Arc<str>>,
            _auth_token: Option<String>,
            _custom_headers: HashMap<HeaderName, HeaderValue>,
        ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
            unimplemented!("not used by these tests")
        }

        async fn delete_session(
            &self,
            _uri: Arc<str>,
            _session_id: Arc<str>,
            _auth_token: Option<String>,
            _custom_headers: HashMap<HeaderName, HeaderValue>,
        ) -> Result<(), StreamableHttpError<Self::Error>> {
            unimplemented!("not used by these tests")
        }
    }

    /// Counts this module's warn events and records each debug event's
    /// `suppressed_warn` field.
    #[derive(Clone, Default)]
    struct LogCapture {
        warns: Arc<std::sync::atomic::AtomicUsize>,
        debug_suppressed_flags: Arc<parking_lot::Mutex<Vec<Option<bool>>>>,
    }

    struct SuppressedFlag(Option<bool>);
    impl tracing::field::Visit for SuppressedFlag {
        fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
            if field.name() == "suppressed_warn" {
                self.0 = Some(value);
            }
        }
        fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
    }

    impl tracing::Subscriber for LogCapture {
        fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
            metadata
                .target()
                .starts_with("echo_agent_mcp::mcp_http_client")
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            use std::sync::atomic::Ordering;
            let level = *event.metadata().level();
            if level == tracing::Level::WARN {
                self.warns.fetch_add(1, Ordering::Relaxed);
            } else if level == tracing::Level::DEBUG {
                let mut flag = SuppressedFlag(None);
                event.record(&mut flag);
                self.debug_suppressed_flags.lock().push(flag.0);
            }
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    async fn drive_once(client: &McpHttpClient<MockInner>) {
        let stream = client
            .get_stream(
                "http://mock".into(),
                Some("session".into()),
                None,
                None,
                HashMap::new(),
            )
            .await
            .expect("mock stream");
        drop(stream);
    }

    // Paused tokio time drives both the backoff sleeps and the throttle
    // clock.
    #[tokio::test(start_paused = true)]
    async fn get_stream_maps_warn_debug_and_suppressed_severities() {
        use std::sync::atomic::Ordering;

        let capture = LogCapture::default();
        let _guard = tracing::subscriber::set_default(capture.clone());

        let client = McpHttpClient::new(MockInner, "mock-server", WarnBudget::default());
        // Attempts 0 and 1 are unthrottled; attempt 2 warns; attempt 3 is debug.
        for _ in 0..4 {
            drive_once(&client).await;
        }
        assert_eq!(capture.warns.load(Ordering::Relaxed), 1, "exactly one warn");
        assert_eq!(
            *capture.debug_suppressed_flags.lock(),
            vec![Some(false)],
            "in-episode debug is not a suppressed warning"
        );

        // A stable gap resets the episode; the next throttled attempt is a
        // suppressed warning and must also log at debug, not warn.
        tokio::time::advance(STABLE_STREAM_THRESHOLD + Duration::from_millis(1)).await;
        for _ in 0..3 {
            drive_once(&client).await;
        }
        assert_eq!(
            capture.warns.load(Ordering::Relaxed),
            1,
            "suppressed warning must not log at warn"
        );
        assert_eq!(
            *capture.debug_suppressed_flags.lock(),
            vec![Some(false), Some(true)],
            "suppressed entry logs at debug with suppressed_warn set"
        );
    }
}
