//! Feature-gated Prometheus metrics for the SDK.
//!
//! When the `metrics` cargo feature is enabled, each helper records to a
//! lazily-registered Prometheus counter / gauge / histogram. When
//! disabled (the default), every helper compiles to an empty function
//! body so the SDK carries zero prometheus dependency.

#[cfg(feature = "metrics")]
mod inner {
    use prometheus::{
        Histogram, HistogramVec, IntCounter, IntCounterVec, IntGauge, IntGaugeVec,
        exponential_buckets, register_histogram, register_histogram_vec, register_int_counter,
        register_int_counter_vec, register_int_gauge, register_int_gauge_vec,
    };
    use std::sync::LazyLock;

    static POOL_CONNECTIONS: LazyLock<IntGauge> = LazyLock::new(|| {
        register_int_gauge!(
            "computer_hub_client_pool_connections",
            "Active pooled connections in the SDK connection pool."
        )
        .expect("computer_hub_client_pool_connections must register once")
    });

    static POOL_EVICTIONS_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
        register_int_counter!(
            "computer_hub_client_pool_evictions_total",
            "Pooled connections closed by the idle reaper (unused past the idle TTL)."
        )
        .expect("computer_hub_client_pool_evictions_total must register once")
    });

    static RECONNECTS_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
        register_int_counter!(
            "computer_hub_client_reconnects_total",
            "Cumulative reconnect attempts that succeeded."
        )
        .expect("computer_hub_client_reconnects_total must register once")
    });

    static RECONNECT_FAILED_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
        register_int_counter_vec!(
            "computer_hub_client_reconnect_failed_total",
            "Cumulative reconnect attempts that failed, by reason \
             (handshake_auth = fatal 401/403, transport = retryable).",
            &["reason"]
        )
        .expect("computer_hub_client_reconnect_failed_total must register once")
    });

    static RECONNECT_DURATION_SECONDS: LazyLock<Histogram> = LazyLock::new(|| {
        register_histogram!(
            "computer_hub_client_reconnect_duration_seconds",
            "Time to complete a reconnect cycle (handshake + session/tool replay).",
            exponential_buckets(0.01, 2.0, 14).expect("valid bucket params")
        )
        .expect("computer_hub_client_reconnect_duration_seconds must register once")
    });

    static RECONNECTS_BY_CAUSE_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
        register_int_counter_vec!(
            "computer_hub_client_reconnects_by_cause_total",
            "Successful reconnects by disconnect cause of the previous connection \
             (close_frame, eof, transport_read_error, transport_write_error, forced). \
             Cause-labeled companion to computer_hub_client_reconnects_total.",
            &["cause"]
        )
        .expect("computer_hub_client_reconnects_by_cause_total must register once")
    });

    static DISCONNECT_DETAIL_CLASS_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
        register_int_counter_vec!(
            "computer_hub_client_disconnect_detail_class_total",
            "Disconnects with a transport error detail, by cause (transport_read_error |\
             transport_write_error) and bounded detail_class (connection_reset | \
             broken_pipe | unexpected_eof | timeout | connection_aborted | other).",
            &["cause", "detail_class"]
        )
        .expect("computer_hub_client_disconnect_detail_class_total must register once")
    });

    static RECONNECT_GAP_SECONDS: LazyLock<Histogram> = LazyLock::new(|| {
        register_histogram!(
            "computer_hub_client_reconnect_gap_seconds",
            "Time from the last inbound frame on the dead connection to a successful reconnect.",
            exponential_buckets(0.1, 2.0, 14).expect("valid bucket params")
        )
        .expect("computer_hub_client_reconnect_gap_seconds must register once")
    });

    static CALL_DISPATCH_SECONDS: LazyLock<Histogram> = LazyLock::new(|| {
        register_histogram!(
            "computer_hub_client_call_dispatch_seconds",
            "Time to set up and queue the outbound remote dispatch.",
            exponential_buckets(0.0001, 2.0, 14).expect("valid bucket params")
        )
        .expect("computer_hub_client_call_dispatch_seconds must register once")
    });

    static DEMUX_INBOX_DEPTH: LazyLock<IntGauge> = LazyLock::new(|| {
        register_int_gauge!(
            "computer_hub_client_demux_inbox_depth",
            "Number of session inboxes registered in the inbound demux."
        )
        .expect("computer_hub_client_demux_inbox_depth must register once")
    });

    static CALL_ID_COLLISIONS_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
        register_int_counter!(
            "computer_hub_client_call_id_collisions_total",
            "Call-id collisions detected in the harness dispatch path."
        )
        .expect("computer_hub_client_call_id_collisions_total must register once")
    });

    // ── Server integration metrics ──────────────────────────────────

    static HARNESS_CONNECT_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
        register_int_counter_vec!(
            "hub_harness_connect_total",
            "Hub connection attempts by outcome and sampler.",
            &["status", "sampler"]
        )
        .expect("hub_harness_connect_total must register once")
    });

    static SESSION_EVENT_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
        register_int_counter_vec!(
            "hub_session_event_total",
            "SessionEvent emissions by event type.",
            &["event_type"]
        )
        .expect("hub_session_event_total must register once")
    });

    static SESSION_OP_DURATION_SECONDS: LazyLock<HistogramVec> = LazyLock::new(|| {
        register_histogram_vec!(
            "hub_session_op_duration_seconds",
            "Latency of hub session lifecycle operations (open/bind) by op and outcome.",
            &["op", "status"],
            exponential_buckets(0.001, 2.0, 14).expect("valid bucket params")
        )
        .expect("hub_session_op_duration_seconds must register once")
    });

    static SESSION_SOFT_REBIND_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
        register_int_counter!(
            "hub_session_soft_rebind_total",
            "Redundant session.bind frames for a session with a live dispatch loop, \
             handled as a non-destructive soft rebind (serve state refreshed, \
             in-flight calls preserved)."
        )
        .expect("hub_session_soft_rebind_total must register once")
    });

    static NO_HANDLER_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
        register_int_counter!(
            "hub_sdk_no_handler_total",
            "tool_call_request frames rejected with -32011 because the session's \
             current handler set has no handler for the requested tool_id."
        )
        .expect("hub_sdk_no_handler_total must register once")
    });

    static HOOK_SEND_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
        register_int_counter_vec!(
            "hub_hook_send_total",
            "Hook sends by hook type.",
            &["hook_type"]
        )
        .expect("hub_hook_send_total must register once")
    });

    static PROGRESS_FRAMES_FORWARDED_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
        register_int_counter!(
            "hub_progress_frames_forwarded_total",
            "Progress frames forwarded by ToolServer."
        )
        .expect("hub_progress_frames_forwarded_total must register once")
    });

    static CANCEL_HOOK_RECEIVED_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
        register_int_counter!(
            "hub_cancel_hook_received_total",
            "Cancel hooks received by workspace tool server."
        )
        .expect("hub_cancel_hook_received_total must register once")
    });

    static WRITER_SINK_SEND_ERRORS_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
        register_int_counter!(
            "computer_hub_client_writer_sink_send_errors_total",
            "Writer-task sink send failures; each signals the reader to reconnect."
        )
        .expect("computer_hub_client_writer_sink_send_errors_total must register once")
    });

    static RECONNECT_WRITER_RESUME_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
        register_int_counter!(
            "computer_hub_client_reconnect_writer_resume_total",
            "Fresh-sink Resume handoffs delivered to the writer task after reconnect."
        )
        .expect("computer_hub_client_reconnect_writer_resume_total must register once")
    });

    static LIVENESS_DEADLINE_EXPIRED_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
        register_int_counter!(
            "computer_hub_client_liveness_deadline_expired_total",
            "Liveness-deadline expiries in the reader (no inbound WebSocket \
             frame within the deadline); each declares the connection dead and \
             drives the normal reconnect path."
        )
        .expect("computer_hub_client_liveness_deadline_expired_total must register once")
    });

    static HEARTBEAT_PONG_DROPPED_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
        register_int_counter!(
            "computer_hub_client_heartbeat_pong_dropped_total",
            "App-level heartbeat pongs dropped because outbound_tx was saturated (split reader)."
        )
        .expect("computer_hub_client_heartbeat_pong_dropped_total must register once")
    });

    static CANCEL_APPLIED_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
        register_int_counter!(
            "hub_cancel_applied_total",
            "Cancel hooks that hit a live in-flight call and cancelled it."
        )
        .expect("hub_cancel_applied_total must register once")
    });

    static CANCEL_PENDING_TOMBSTONED_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
        register_int_counter!(
            "hub_cancel_pending_tombstoned_total",
            "Cancel hooks recorded as a pending tombstone (call not yet registered or already done)."
        )
        .expect("hub_cancel_pending_tombstoned_total must register once")
    });

    static CANCEL_NO_TARGET_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
        register_int_counter!(
            "hub_cancel_no_target_total",
            "Cancel hooks with no call_id (session-wide, no specific call to cancel)."
        )
        .expect("hub_cancel_no_target_total must register once")
    });

    static TOOL_CALL_REJECTED_OVERLOADED_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
        register_int_counter!(
            "hub_tool_call_rejected_overloaded_total",
            "Tool calls rejected by admission timeout (-32016 tool_busy)."
        )
        .expect("hub_tool_call_rejected_overloaded_total must register once")
    });

    static INBOX_FULL_REQUEST_REJECTED_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
        register_int_counter!(
            "hub_inbox_full_request_rejected_total",
            "Requests rejected with an overloaded response on a full session inbox."
        )
        .expect("hub_inbox_full_request_rejected_total must register once")
    });

    static INBOX_FULL_REJECT_SEND_FAILED_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
        register_int_counter!(
            "hub_inbox_full_reject_send_failed_total",
            "Overloaded rejections dropped because outbound was also full (residual silent loss)."
        )
        .expect("hub_inbox_full_reject_send_failed_total must register once")
    });

    static INBOX_FULL_NOTIFICATION_DROPPED_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
        register_int_counter!(
            "hub_inbox_full_notification_dropped_total",
            "Notifications (no id) dropped on a full session inbox."
        )
        .expect("hub_inbox_full_notification_dropped_total must register once")
    });

    static SERVE_REPLAY_TIMEOUT_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
        register_int_counter!(
            "computer_hub_client_serve_replay_timeout_total",
            "serve attempts that hit the per-attempt reply deadline, from any \
             serve call site (reconnect replay, run(), bind, tool updates)."
        )
        .expect("computer_hub_client_serve_replay_timeout_total must register once")
    });

    static NOTIF_LAGGED_RECOVERED_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
        register_int_counter!(
            "hub_notif_lagged_recovered_total",
            "Connection-notification broadcast Lagged events recovered by \
             continuing the loop instead of exiting."
        )
        .expect("hub_notif_lagged_recovered_total must register once")
    });

    static EARLY_NOTIF_BUFFERED_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
        register_int_counter!(
            "hub_early_notif_buffered_total",
            "Connection-level notification frames (binds, unbinds, evicts, \
             ...) buffered between connect and ToolServer::run() and replayed \
             instead of dropped."
        )
        .expect("hub_early_notif_buffered_total must register once")
    });

    static TOOL_CALL_INFLIGHT: LazyLock<IntGaugeVec> = LazyLock::new(|| {
        register_int_gauge_vec!(
            "hub_tool_call_inflight",
            "Concurrent running tool calls holding an admission permit, by scope.",
            &["scope"]
        )
        .expect("hub_tool_call_inflight must register once")
    });

    static ADMISSION_WAIT_SECONDS: LazyLock<Histogram> = LazyLock::new(|| {
        register_histogram!(
            "hub_tool_call_admission_wait_seconds",
            "Time blocked acquiring the three admission permits (one shared deadline).",
            exponential_buckets(0.0001, 2.0, 16).expect("valid bucket params")
        )
        .expect("hub_tool_call_admission_wait_seconds must register once")
    });

    pub(crate) fn pool_connections_inc() {
        POOL_CONNECTIONS.inc();
    }

    pub(crate) fn pool_connections_dec() {
        POOL_CONNECTIONS.dec();
    }

    pub(crate) fn pool_evictions_inc() {
        POOL_EVICTIONS_TOTAL.inc();
    }

    pub(crate) fn reconnect_succeeded() {
        RECONNECTS_TOTAL.inc();
    }

    pub(crate) fn reconnect_failed(reason: &str) {
        RECONNECT_FAILED_TOTAL.with_label_values(&[reason]).inc();
    }

    pub(crate) fn reconnect_duration_observe(secs: f64) {
        RECONNECT_DURATION_SECONDS.observe(secs);
    }

    pub(crate) fn reconnect_cause(cause: &str) {
        RECONNECTS_BY_CAUSE_TOTAL.with_label_values(&[cause]).inc();
    }

    pub(crate) fn disconnect_detail_class(cause: &str, detail_class: &str) {
        DISCONNECT_DETAIL_CLASS_TOTAL
            .with_label_values(&[cause, detail_class])
            .inc();
    }

    pub(crate) fn reconnect_gap_observe(secs: f64) {
        RECONNECT_GAP_SECONDS.observe(secs);
    }

    pub(crate) fn call_dispatch_observe(secs: f64) {
        CALL_DISPATCH_SECONDS.observe(secs);
    }

    pub(crate) fn demux_inbox_depth_set(depth: i64) {
        DEMUX_INBOX_DEPTH.set(depth);
    }

    pub(crate) fn call_id_collision() {
        CALL_ID_COLLISIONS_TOTAL.inc();
    }

    /// Record a harness connection attempt.
    ///
    /// `sampler` identifies the caller (`"chat"` or `"shell"`).
    /// `status` is `"ok"`, `"error"`, or `"fallback"` (fallback is
    /// emitted by the caller in `AgentBuilder::build_harness()`, not
    /// by the SDK).
    pub fn harness_connect(status: &str, sampler: &str) {
        HARNESS_CONNECT_TOTAL
            .with_label_values(&[status, sampler])
            .inc();
    }

    pub(crate) fn session_event(event_type: &str) {
        SESSION_EVENT_TOTAL.with_label_values(&[event_type]).inc();
    }

    /// Observe the latency of a session lifecycle operation.
    /// `op` is `"open"` or `"bind"`; `status` is `"ok"` or `"error"`.
    pub(crate) fn session_op_observe(op: &str, status: &str, secs: f64) {
        SESSION_OP_DURATION_SECONDS
            .with_label_values(&[op, status])
            .observe(secs);
    }

    pub(crate) fn session_soft_rebind() {
        SESSION_SOFT_REBIND_TOTAL.inc();
    }

    pub(crate) fn no_handler() {
        NO_HANDLER_TOTAL.inc();
    }

    pub(crate) fn hook_send(hook_type: &str) {
        HOOK_SEND_TOTAL.with_label_values(&[hook_type]).inc();
    }

    pub(crate) fn progress_frame_forwarded() {
        PROGRESS_FRAMES_FORWARDED_TOTAL.inc();
    }

    pub(crate) fn cancel_hook_received() {
        CANCEL_HOOK_RECEIVED_TOTAL.inc();
    }

    pub(crate) fn writer_sink_send_error() {
        WRITER_SINK_SEND_ERRORS_TOTAL.inc();
    }

    pub(crate) fn reconnect_writer_resume() {
        RECONNECT_WRITER_RESUME_TOTAL.inc();
    }

    pub(crate) fn liveness_deadline_expired() {
        LIVENESS_DEADLINE_EXPIRED_TOTAL.inc();
    }

    pub(crate) fn heartbeat_pong_dropped() {
        HEARTBEAT_PONG_DROPPED_TOTAL.inc();
    }

    #[cfg(test)]
    pub(crate) fn heartbeat_pong_dropped_count() -> u64 {
        HEARTBEAT_PONG_DROPPED_TOTAL.get()
    }

    pub(crate) fn cancel_applied() {
        CANCEL_APPLIED_TOTAL.inc();
    }

    pub(crate) fn cancel_pending_tombstoned() {
        CANCEL_PENDING_TOMBSTONED_TOTAL.inc();
    }

    pub(crate) fn cancel_no_target() {
        CANCEL_NO_TARGET_TOTAL.inc();
    }

    pub(crate) fn tool_call_rejected_overloaded() {
        TOOL_CALL_REJECTED_OVERLOADED_TOTAL.inc();
    }

    pub(crate) fn inbox_full_request_rejected() {
        INBOX_FULL_REQUEST_REJECTED_TOTAL.inc();
    }

    pub(crate) fn inbox_full_reject_send_failed() {
        INBOX_FULL_REJECT_SEND_FAILED_TOTAL.inc();
    }

    pub(crate) fn inbox_full_notification_dropped() {
        INBOX_FULL_NOTIFICATION_DROPPED_TOTAL.inc();
    }

    pub(crate) fn serve_replay_timeout() {
        SERVE_REPLAY_TIMEOUT_TOTAL.inc();
    }

    pub(crate) fn notif_lagged_recovered() {
        NOTIF_LAGGED_RECOVERED_TOTAL.inc();
    }

    pub(crate) fn early_notif_buffered(frames: u64) {
        EARLY_NOTIF_BUFFERED_TOTAL.inc_by(frames);
    }

    pub(crate) fn tool_call_inflight_inc(scope: &str) {
        TOOL_CALL_INFLIGHT.with_label_values(&[scope]).inc();
    }

    pub(crate) fn tool_call_inflight_dec(scope: &str) {
        TOOL_CALL_INFLIGHT.with_label_values(&[scope]).dec();
    }

    pub(crate) fn admission_wait_observe(secs: f64) {
        ADMISSION_WAIT_SECONDS.observe(secs);
    }

    // ── OIDC refresh (auth.current path) ────────────────────────────

    /// Closed-set outcomes for `AuthProvider::current` (metric labels).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum OidcRefreshOutcome {
        SkippedNotExpired,
        Ok,
        FailedUsedStale,
    }

    impl OidcRefreshOutcome {
        pub const fn as_str(self) -> &'static str {
            match self {
                Self::SkippedNotExpired => "skipped_not_expired",
                Self::Ok => "ok",
                Self::FailedUsedStale => "failed_used_stale",
            }
        }
    }

    static OIDC_REFRESH_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
        register_int_counter_vec!(
            "computer_hub_oidc_refresh_total",
            "OIDC AuthProvider::current outcomes: skipped_not_expired (no network), \
             ok (refresh succeeded), failed_used_stale (refresh failed, stale token returned).",
            &["outcome"]
        )
        .expect("computer_hub_oidc_refresh_total must register once")
    });

    static OIDC_REFRESH_DURATION_SECONDS: LazyLock<Histogram> = LazyLock::new(|| {
        register_histogram!(
            "computer_hub_oidc_refresh_duration_seconds",
            "Wall-clock time of an attempted OIDC refresh (discovery + token exchange). \
             Not sampled for skipped_not_expired.",
            exponential_buckets(0.01, 2.0, 14).expect("valid bucket params")
        )
        .expect("computer_hub_oidc_refresh_duration_seconds must register once")
    });

    /// Duration observed only for attempted refreshes (`Ok` / `FailedUsedStale`).
    pub(crate) fn oidc_refresh_observe(outcome: OidcRefreshOutcome, secs: Option<f64>) {
        OIDC_REFRESH_TOTAL
            .with_label_values(&[outcome.as_str()])
            .inc();
        if let Some(secs) = secs {
            OIDC_REFRESH_DURATION_SECONDS.observe(secs);
        }
    }

    #[cfg(test)]
    pub(crate) fn oidc_refresh_count(outcome: OidcRefreshOutcome) -> u64 {
        OIDC_REFRESH_TOTAL
            .with_label_values(&[outcome.as_str()])
            .get()
    }

    #[cfg(test)]
    pub(crate) fn oidc_refresh_duration_sample_count() -> u64 {
        OIDC_REFRESH_DURATION_SECONDS.get_sample_count()
    }

    /// Serializes OIDC metric delta assertions under parallel `cargo test`.
    #[cfg(test)]
    static OIDC_METRICS_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[cfg(test)]
    pub(crate) fn lock_oidc_metrics_test() -> std::sync::MutexGuard<'static, ()> {
        OIDC_METRICS_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(not(feature = "metrics"))]
mod inner {
    #[cfg(test)]
    static TEST_HEARTBEAT_PONG_DROPPED: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(0);

    pub(crate) fn pool_connections_inc() {}
    pub(crate) fn pool_connections_dec() {}
    pub(crate) fn pool_evictions_inc() {}
    pub(crate) fn reconnect_succeeded() {}
    pub(crate) fn reconnect_failed(_reason: &str) {}
    pub(crate) fn reconnect_duration_observe(_secs: f64) {}
    pub(crate) fn reconnect_cause(_cause: &str) {}
    pub(crate) fn disconnect_detail_class(_cause: &str, _detail_class: &str) {}
    pub(crate) fn reconnect_gap_observe(_secs: f64) {}
    pub(crate) fn call_dispatch_observe(_secs: f64) {}
    pub(crate) fn demux_inbox_depth_set(_depth: i64) {}
    pub(crate) fn call_id_collision() {}
    pub fn harness_connect(_status: &str, _sampler: &str) {}
    pub(crate) fn session_event(_event_type: &str) {}
    pub(crate) fn session_op_observe(_op: &str, _status: &str, _secs: f64) {}
    pub(crate) fn session_soft_rebind() {}
    pub(crate) fn no_handler() {}
    pub(crate) fn hook_send(_hook_type: &str) {}
    pub(crate) fn progress_frame_forwarded() {}
    pub(crate) fn cancel_hook_received() {}
    pub(crate) fn writer_sink_send_error() {}
    pub(crate) fn reconnect_writer_resume() {}
    pub(crate) fn liveness_deadline_expired() {}
    pub(crate) fn heartbeat_pong_dropped() {
        #[cfg(test)]
        TEST_HEARTBEAT_PONG_DROPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn heartbeat_pong_dropped_count() -> u64 {
        TEST_HEARTBEAT_PONG_DROPPED.load(std::sync::atomic::Ordering::Relaxed)
    }
    pub(crate) fn cancel_applied() {}
    pub(crate) fn cancel_pending_tombstoned() {}
    pub(crate) fn cancel_no_target() {}
    pub(crate) fn tool_call_rejected_overloaded() {}
    pub(crate) fn inbox_full_request_rejected() {}
    pub(crate) fn inbox_full_reject_send_failed() {}
    pub(crate) fn inbox_full_notification_dropped() {}
    pub(crate) fn serve_replay_timeout() {}
    pub(crate) fn notif_lagged_recovered() {}
    pub(crate) fn early_notif_buffered(_frames: u64) {}
    pub(crate) fn tool_call_inflight_inc(_scope: &str) {}
    pub(crate) fn tool_call_inflight_dec(_scope: &str) {}
    pub(crate) fn admission_wait_observe(_secs: f64) {}
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum OidcRefreshOutcome {
        SkippedNotExpired,
        Ok,
        FailedUsedStale,
    }
    pub(crate) fn oidc_refresh_observe(_outcome: OidcRefreshOutcome, _secs: Option<f64>) {}
}

pub(crate) use inner::OidcRefreshOutcome;
pub(crate) use inner::admission_wait_observe;
pub(crate) use inner::call_dispatch_observe;
pub(crate) use inner::call_id_collision;
pub(crate) use inner::cancel_applied;
pub(crate) use inner::cancel_hook_received;
pub(crate) use inner::cancel_no_target;
pub(crate) use inner::cancel_pending_tombstoned;
pub(crate) use inner::demux_inbox_depth_set;
pub(crate) use inner::disconnect_detail_class;
pub(crate) use inner::early_notif_buffered;
pub(crate) use inner::heartbeat_pong_dropped;
#[cfg(test)]
pub(crate) use inner::heartbeat_pong_dropped_count;
pub(crate) use inner::hook_send;
pub(crate) use inner::inbox_full_notification_dropped;
pub(crate) use inner::inbox_full_reject_send_failed;
pub(crate) use inner::inbox_full_request_rejected;
pub(crate) use inner::liveness_deadline_expired;
#[cfg(all(test, feature = "metrics"))]
pub(crate) use inner::lock_oidc_metrics_test;
pub(crate) use inner::no_handler;
pub(crate) use inner::notif_lagged_recovered;
#[cfg(all(test, feature = "metrics"))]
pub(crate) use inner::oidc_refresh_count;
#[cfg(all(test, feature = "metrics"))]
pub(crate) use inner::oidc_refresh_duration_sample_count;
pub(crate) use inner::oidc_refresh_observe;
pub(crate) use inner::pool_connections_dec;
pub(crate) use inner::pool_connections_inc;
pub(crate) use inner::pool_evictions_inc;
pub(crate) use inner::progress_frame_forwarded;
pub(crate) use inner::reconnect_cause;
pub(crate) use inner::reconnect_duration_observe;
pub(crate) use inner::reconnect_failed;
pub(crate) use inner::reconnect_gap_observe;
pub(crate) use inner::reconnect_succeeded;
pub(crate) use inner::reconnect_writer_resume;
pub(crate) use inner::serve_replay_timeout;
pub(crate) use inner::session_event;
pub(crate) use inner::session_op_observe;
pub(crate) use inner::session_soft_rebind;
pub(crate) use inner::tool_call_inflight_dec;
pub(crate) use inner::tool_call_inflight_inc;
pub(crate) use inner::tool_call_rejected_overloaded;
pub(crate) use inner::writer_sink_send_error;

/// Record a harness connection attempt. Public so callers outside
/// the SDK (e.g. `AgentBuilder::build_harness()` in the agentic sampler)
/// can emit `status="fallback"` when the server connection fails and the
/// builder falls back to a local-only harness.
pub use inner::harness_connect;
