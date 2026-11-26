//! Inbound frame demultiplexer.
//!
//! Frames inbound from the WebSocket fall into four buckets:
//!
//! 1. JSON-RPC **responses** correlated to a previously-issued request
//!    by `id`. Routed through the crate-internal response-waiter map.
//! 2. **`tool_call_progress` notifications** correlated to a per-call
//!    `tool_call_id` carried in `params`. Routed through the
//!    crate-internal progress-waiter map registered via
//!    `Demux::try_register_progress_waiter` (crate-internal).
//! 3. JSON-RPC **requests / notifications** carrying a `session_id` —
//!    routed to the per-session inbox registered via
//!    [`Demux::register_session_inbox`].
//! 4. Connection-level frames (handshake, ping/pong) that the
//!    connection actor handles directly without going through the demux.
//!
//! The demux owns the session inbox map, the in-flight response
//! waiters, and the per-call progress waiters; the connection actor
//! parses each text frame, classifies it,
//! and pushes it through this module.
//!
//! Routing inbound frames is non-blocking: a full session inbox or a
//! dropped receiver returns a typed [`RouteOutcome`] variant rather
//! than awaiting the inbox. Blocking on a slow consumer would back up
//! the entire connection actor and starve every other session sharing
//! the socket.

use dashmap::DashMap;
use serde_json::Value;
use tokio::sync::oneshot;
use tracing::warn;

use echo_agent_tool_protocol::{
    JsonRpcId, JsonRpcResponse, RequestId, SessionId, ToolCallId, ToolCallProgressFrame,
};

use crate::error::ClientError;

/// Frame routed to a session inbox.
#[derive(Debug, Clone)]
pub enum InboundFrame {
    /// A request the inbox owner must answer (any session frame carrying
    /// an `id`): a `tool_call_request`, or a reverse-direction `hook`
    /// answered via `ToolHarness::send_hook_reply`. Carries raw JSON.
    Request(Value),
    /// Server-issued notification (e.g. `tool.notification`) — fire-and-
    /// forget, no reply expected.
    Notification(Value),
}

/// Outcome of [`Demux::route`].
#[derive(Debug, PartialEq, Eq)]
pub enum RouteOutcome {
    /// Matched a response waiter; the oneshot was fulfilled.
    Response,
    /// Forwarded to a session inbox.
    Session,
    /// Matched a progress waiter; the progress frame was forwarded to
    /// the per-call progress channel.
    Progress,
    /// No inbox is bound for the targeted session.
    UnknownSession,
    /// No progress waiter is parked for the targeted `tool_call_id`. The
    /// caller's stream is no longer subscribed (typical post-terminal),
    /// so the frame is dropped.
    UnknownProgress,
    /// Connection-level notification broadcast to subscribers.
    Notification,
    /// No waiter is parked for the targeted request id, OR the frame
    /// was unaddressable.
    Unrouted,
    /// The session inbox sender was full; the frame was dropped to
    /// avoid blocking the connection actor.
    InboxFull,
    /// The session inbox receiver was dropped (e.g. the consumer's
    /// run loop exited); the binding is now stale and the frame was
    /// dropped.
    SessionDropped,
    /// The progress channel was full; the frame was dropped to avoid
    /// blocking the connection actor. The caller's stream consumer
    /// fell behind on draining progress.
    ProgressFull,
    /// The progress receiver was dropped (e.g. the caller's stream was
    /// dropped); the waiter binding is now stale and the frame was
    /// dropped.
    ProgressDropped,
}

/// Demux state. Cheap to construct; uses [`DashMap`] internally so
/// concurrent registers and routes never block each other.
#[derive(Debug)]
pub struct Demux {
    sessions: DashMap<SessionId, tokio::sync::mpsc::Sender<InboundFrame>>,
    waiters: DashMap<RequestId, oneshot::Sender<Result<JsonRpcResponse, ClientError>>>,
    /// Session index for `tool.call` response waiters only. Lets the SDK
    /// in-flight short-circuit fail every parked call for a session on a
    /// workspace Disconnected notification without waiting for the server.
    /// Turn-hook / session-RPC waiters are NOT indexed here, so the
    /// short-circuit never touches them.
    call_sessions: DashMap<RequestId, SessionId>,
    progress: DashMap<ToolCallId, tokio::sync::mpsc::Sender<ToolCallProgressFrame>>,
    /// Broadcast channel for connection-level notifications (no session_id).
    notifications: tokio::sync::broadcast::Sender<Value>,
    /// Clone of the connection's outbound sender. Used to synthesize the
    /// overloaded (-32016) response when a session inbox is full so a
    /// Request is rejected with an error rather than silently dropped.
    /// `None` in unit tests that construct a bare demux.
    outbound: Option<tokio::sync::mpsc::Sender<String>>,
}

impl Default for Demux {
    fn default() -> Self {
        let (notifications, _) = tokio::sync::broadcast::channel(64);
        Self {
            sessions: DashMap::new(),
            waiters: DashMap::new(),
            call_sessions: DashMap::new(),
            progress: DashMap::new(),
            notifications,
            outbound: None,
        }
    }
}

impl Demux {
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct a demux wired to the connection's outbound sender so the
    /// inbox-full Request path can ship an overloaded (-32016) response.
    pub fn with_outbound(outbound: tokio::sync::mpsc::Sender<String>) -> Self {
        Self {
            outbound: Some(outbound),
            ..Self::default()
        }
    }

    /// Subscribe to connection-level notifications (no session_id).
    pub fn subscribe_notifications(&self) -> tokio::sync::broadcast::Receiver<Value> {
        self.notifications.subscribe()
    }

    /// Bind `session_id` to `inbox`; replaces any existing binding.
    /// Returns the previous sender if one existed; the caller may
    /// drop or drain it as appropriate.
    pub fn register_session_inbox(
        &self,
        session_id: SessionId,
        inbox: tokio::sync::mpsc::Sender<InboundFrame>,
    ) -> Option<tokio::sync::mpsc::Sender<InboundFrame>> {
        self.sessions.insert(session_id, inbox)
    }

    /// Remove the inbox bound to `session_id`. The returned sender (if
    /// present) is dropped by the caller, signalling EOF to its
    /// receiver task.
    pub fn unregister_session_inbox(
        &self,
        session_id: &SessionId,
    ) -> Option<tokio::sync::mpsc::Sender<InboundFrame>> {
        self.sessions.remove(session_id).map(|(_, sender)| sender)
    }

    /// Remove the inbox only if it is still the same channel as `expected`.
    ///
    /// Prevents a late untrack→unregister from clobbering a peer harness that
    /// rebound the same session in between (identity, not key-only).
    pub fn unregister_session_inbox_if(
        &self,
        session_id: &SessionId,
        expected: &tokio::sync::mpsc::Sender<InboundFrame>,
    ) -> Option<tokio::sync::mpsc::Sender<InboundFrame>> {
        self.sessions
            .remove_if(session_id, |_, sender| sender.same_channel(expected))
            .map(|(_, sender)| sender)
    }

    /// Like [`Self::unregister_session_inbox_if`], but compares via a
    /// [`tokio::sync::mpsc::WeakSender`] so callers need not hold a strong
    /// sender (which would pin the channel open after demux replacement).
    pub fn unregister_session_inbox_if_weak(
        &self,
        session_id: &SessionId,
        expected: &tokio::sync::mpsc::WeakSender<InboundFrame>,
    ) -> Option<tokio::sync::mpsc::Sender<InboundFrame>> {
        let expected_strong = expected.upgrade()?;
        self.unregister_session_inbox_if(session_id, &expected_strong)
    }

    /// Park a oneshot waiter for `request_id`. Crate-internal: only
    /// the connection actor allocates request ids.
    pub(crate) fn register_response_waiter(
        &self,
        request_id: RequestId,
        waiter: oneshot::Sender<Result<JsonRpcResponse, ClientError>>,
    ) {
        self.waiters.insert(request_id, waiter);
    }

    /// Park a `tool.call` response waiter and record its `session_id` so the
    /// SDK in-flight short-circuit ([`Self::fail_calls_for_session`]) can
    /// resolve it on a workspace Disconnected notification. Crate-internal.
    pub(crate) fn register_call_response_waiter(
        &self,
        request_id: RequestId,
        session_id: SessionId,
        waiter: oneshot::Sender<Result<JsonRpcResponse, ClientError>>,
    ) {
        self.call_sessions.insert(request_id.clone(), session_id);
        self.waiters.insert(request_id, waiter);
    }

    /// Pop the waiter for `request_id`, if any. Also drops the session index
    /// entry so the two maps stay consistent. Crate-internal.
    pub(crate) fn take_response_waiter(
        &self,
        request_id: &RequestId,
    ) -> Option<oneshot::Sender<Result<JsonRpcResponse, ClientError>>> {
        self.call_sessions.remove(request_id);
        self.waiters.remove(request_id).map(|(_, waiter)| waiter)
    }

    /// Fail every in-flight `tool.call` waiter bound to `session_id`,
    /// completing each with `result_factory`. Returns the number resolved.
    ///
    /// Drives the SDK in-flight short-circuit: on a workspace
    /// `ToolServerStatusChanged(Disconnected)` notification the harness fails
    /// its parked calls for that session promptly instead of parking until
    /// `rpc_ttl_ms`. Idempotent with the server-side cancel — each waiter is
    /// taken at most once, so a call already resolved by the server is skipped.
    pub(crate) fn fail_calls_for_session<F>(
        &self,
        session_id: &SessionId,
        result_factory: F,
    ) -> usize
    where
        F: Fn() -> ClientError,
    {
        // Snapshot the matching request ids first so we never hold a DashMap
        // shard lock across the oneshot send.
        let request_ids: Vec<RequestId> = self
            .call_sessions
            .iter()
            .filter(|kv| kv.value() == session_id)
            .map(|kv| kv.key().clone())
            .collect();
        let mut resolved = 0;
        for request_id in request_ids {
            if let Some(waiter) = self.take_response_waiter(&request_id)
                && waiter.send(Err(result_factory())).is_ok()
            {
                resolved += 1;
            }
        }
        resolved
    }

    /// Park a per-call progress sender keyed by `tool_call_id`.
    /// Returns `Err(progress)` (handing the not-yet-inserted sender
    /// back) when another in-flight call already owns the id, leaving
    /// the prior waiter intact. The caller drops the matching
    /// receiver to terminate the subscription — subsequent inbound
    /// progress for the same id is silently dropped via
    /// [`RouteOutcome::ProgressDropped`].
    ///
    /// Atomic check-then-insert under a single shard lock so a
    /// concurrent caller cannot observe a transient empty slot.
    pub(crate) fn try_register_progress_waiter(
        &self,
        tool_call_id: ToolCallId,
        progress: tokio::sync::mpsc::Sender<ToolCallProgressFrame>,
    ) -> Result<(), tokio::sync::mpsc::Sender<ToolCallProgressFrame>> {
        use dashmap::mapref::entry::Entry;
        match self.progress.entry(tool_call_id) {
            Entry::Occupied(_) => Err(progress),
            Entry::Vacant(slot) => {
                slot.insert(progress);
                Ok(())
            }
        }
    }

    /// Remove the progress sender bound to `tool_call_id`. Crate-internal;
    /// called by the harness once the terminal frame for `tool_call_id`
    /// has been observed.
    pub(crate) fn unregister_progress_waiter(
        &self,
        tool_call_id: &ToolCallId,
    ) -> Option<tokio::sync::mpsc::Sender<ToolCallProgressFrame>> {
        self.progress.remove(tool_call_id).map(|(_, tx)| tx)
    }

    /// Drain every parked waiter, completing each with `result_factory`.
    /// Used by the reconnect path to fast-fail in-flight calls with
    /// [`ClientError::NetworkError`]. Crate-internal.
    pub(crate) fn drain_waiters_with<F>(&self, result_factory: F)
    where
        F: Fn() -> ClientError,
    {
        // Snapshot keys, then remove individually so we never hold
        // a DashMap shard lock across the oneshot send.
        let keys: Vec<RequestId> = self.waiters.iter().map(|kv| kv.key().clone()).collect();
        for key in keys {
            if let Some((_, waiter)) = self.waiters.remove(&key) {
                self.call_sessions.remove(&key);
                let _ = waiter.send(Err(result_factory()));
            }
        }
    }

    /// Drop every parked progress sender. Used by the reconnect path
    /// after [`Self::drain_waiters_with`]: the response waiter resolves
    /// with `NetworkError` and the matching progress channel closes,
    /// so any in-flight harness call stream terminates promptly
    /// instead of stalling on a half-empty progress channel.
    pub(crate) fn drain_progress(&self) {
        let keys: Vec<ToolCallId> = self.progress.iter().map(|kv| kv.key().clone()).collect();
        for key in keys {
            self.progress.remove(&key);
        }
    }

    /// Route a parsed JSON value. Classification rules:
    ///
    /// - presence of `result`/`error` → response, routed to waiter;
    /// - method == `tool_call_progress` notification → progress waiter
    ///   keyed by `params.tool_call_id`;
    /// - presence of `session_id` → session inbox, request vs.
    ///   notification distinguished by the presence of `id`;
    /// - otherwise → [`RouteOutcome::Unrouted`].
    ///
    /// Routing to a session inbox or progress channel uses non-blocking
    /// `try_send`. A full inbox or progress channel returns the matching
    /// `*Full` variant; a dropped receiver returns the matching
    /// `*Dropped` variant. Either way the frame is dropped without
    /// awaiting the consumer, so a slow handler never starves other
    /// sessions or calls multiplexed onto the same connection.
    pub fn route(&self, frame: Value) -> RouteOutcome {
        crate::metrics::demux_inbox_depth_set(self.sessions.len() as i64);
        if frame.get("result").is_some() || frame.get("error").is_some() {
            return self.route_response(frame);
        }
        if frame.get("method").and_then(Value::as_str) == Some("tool_call_progress") {
            return self.route_progress(frame);
        }
        if frame.get("session_id").is_some() {
            return self.route_session(frame);
        }
        // Connection-level notification (e.g. session.bind, session.unbind).
        if frame.get("method").is_some() {
            let _ = self.notifications.send(frame);
            return RouteOutcome::Notification;
        }
        RouteOutcome::Unrouted
    }

    fn route_progress(&self, frame: Value) -> RouteOutcome {
        let Some(params) = frame.get("params") else {
            return RouteOutcome::Unrouted;
        };
        let Some(call_id_str) = params.get("tool_call_id").and_then(Value::as_str) else {
            return RouteOutcome::Unrouted;
        };
        let Ok(tool_call_id) = ToolCallId::new(call_id_str) else {
            return RouteOutcome::Unrouted;
        };
        let Some(sender) = self.progress.get(&tool_call_id) else {
            return RouteOutcome::UnknownProgress;
        };
        let tx = sender.value().clone();
        drop(sender);
        let progress_frame: ToolCallProgressFrame = match serde_json::from_value(params.clone()) {
            Ok(p) => p,
            Err(err) => {
                warn!(%tool_call_id, ?err, "failed to decode tool_call_progress params");
                return RouteOutcome::Unrouted;
            }
        };
        match tx.try_send(progress_frame) {
            Ok(()) => RouteOutcome::Progress,
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                warn!(%tool_call_id, "progress channel full; dropping inbound progress frame");
                RouteOutcome::ProgressFull
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                self.progress.remove(&tool_call_id);
                RouteOutcome::ProgressDropped
            }
        }
    }

    fn route_response(&self, frame: Value) -> RouteOutcome {
        let Some(id_value) = frame.get("id") else {
            return RouteOutcome::Unrouted;
        };
        let request_id = match id_value {
            Value::String(s) => RequestId::new(s.as_str()).ok(),
            Value::Number(n) => RequestId::new(n.to_string()).ok(),
            _ => None,
        };
        let Some(request_id) = request_id else {
            return RouteOutcome::Unrouted;
        };
        let Some(waiter) = self.take_response_waiter(&request_id) else {
            return RouteOutcome::Unrouted;
        };
        let parsed: Result<JsonRpcResponse, ClientError> =
            serde_json::from_value::<JsonRpcResponse>(frame).map_err(ClientError::from);
        let _ = waiter.send(parsed);
        RouteOutcome::Response
    }

    fn route_session(&self, frame: Value) -> RouteOutcome {
        let Some(sid_str) = frame.get("session_id").and_then(Value::as_str) else {
            return RouteOutcome::Unrouted;
        };
        let Ok(session_id) = SessionId::new(sid_str) else {
            return RouteOutcome::Unrouted;
        };
        let Some(sender) = self.sessions.get(&session_id) else {
            return RouteOutcome::UnknownSession;
        };
        let inbox = sender.value().clone();
        drop(sender);
        let kind = if frame.get("id").is_some() {
            InboundFrame::Request(frame)
        } else {
            InboundFrame::Notification(frame)
        };
        match inbox.try_send(kind) {
            Ok(()) => RouteOutcome::Session,
            Err(tokio::sync::mpsc::error::TrySendError::Full(frame)) => {
                self.reject_inbox_full(&session_id, frame);
                RouteOutcome::InboxFull
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                warn!(%session_id, "session inbox dropped; binding stale");
                self.sessions.remove(&session_id);
                RouteOutcome::SessionDropped
            }
        }
    }

    /// Handle a full session inbox without blocking the reader.
    ///
    /// A Request (has an `id`) is rejected with the shared overloaded
    /// (-32016 "tool_busy") response on a best-effort `try_send`; if the
    /// outbound is *also* full the rejection itself is dropped and metered
    /// (`inbox_full_reject_send_failed`). A Notification (no `id`) stays
    /// fire-and-forget and is metered (`inbox_full_notification_dropped`).
    fn reject_inbox_full(&self, session_id: &SessionId, frame: InboundFrame) {
        let InboundFrame::Request(value) = frame else {
            crate::metrics::inbox_full_notification_dropped();
            return;
        };
        crate::metrics::inbox_full_request_rejected();
        warn!(%session_id, "session inbox full; rejecting request with tool_busy");
        let Some(out) = &self.outbound else {
            return;
        };
        // A `Request` always carries an `id` (that is how `route_session`
        // classifies it). A well-formed id deserializes into a `JsonRpcId`;
        // a malformed id (object/array/bool/null) cannot, but the request
        // must STILL get an overloaded response rather than be silently
        // dropped, so we fall back to echoing the raw id JSON as a string.
        let raw_id = value.get("id");
        let id = raw_id
            .and_then(|v| serde_json::from_value::<JsonRpcId>(v.clone()).ok())
            .unwrap_or_else(|| {
                JsonRpcId::new_string(raw_id.map(ToString::to_string).unwrap_or_default())
            });
        let response = crate::admission::overloaded_response(id, session_id.clone());
        let Ok(text) = serde_json::to_string(&response) else {
            return;
        };
        if out.try_send(text).is_err() {
            crate::metrics::inbox_full_reject_send_failed();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn response_route_matches_waiter() {
        let demux = Demux::new();
        let request_id = RequestId::new("r1").expect("valid");
        let (tx, rx) = oneshot::channel();
        demux.register_response_waiter(request_id.clone(), tx);
        let outcome = demux.route(json!({
            "jsonrpc": "2.0",
            "id": "r1",
            "result": {"outcome": "bound"},
        }));
        assert_eq!(outcome, RouteOutcome::Response);
        let resp = rx.await.expect("waiter").expect("ok");
        assert_eq!(resp.id.to_string(), "r1");
    }

    #[tokio::test]
    async fn fail_calls_for_session_resolves_only_matching_call_waiters() {
        // Fails exactly the session's `tool.call` waiters; other sessions'
        // calls and non-call (turn-hook) waiters stay parked.
        let demux = Demux::new();
        let s1 = SessionId::new("s1").expect("valid");
        let s2 = SessionId::new("s2").expect("valid");

        let (tx_a, rx_a) = oneshot::channel();
        let (tx_b, rx_b) = oneshot::channel();
        let (tx_other, rx_other) = oneshot::channel();
        // Two calls on s1, one on s2.
        demux.register_call_response_waiter(RequestId::new("a").unwrap(), s1.clone(), tx_a);
        demux.register_call_response_waiter(RequestId::new("b").unwrap(), s1.clone(), tx_b);
        demux.register_call_response_waiter(RequestId::new("c").unwrap(), s2.clone(), tx_other);
        // A non-call waiter (e.g. a turn hook) on s1 — NOT session-indexed.
        let (tx_hook, rx_hook) = oneshot::channel();
        demux.register_response_waiter(RequestId::new("hook").unwrap(), tx_hook);

        let n = demux.fail_calls_for_session(&s1, || ClientError::NetworkError("gone".to_owned()));
        assert_eq!(n, 2, "only the two s1 call waiters are failed");

        assert!(matches!(rx_a.await, Ok(Err(ClientError::NetworkError(_)))));
        assert!(matches!(rx_b.await, Ok(Err(ClientError::NetworkError(_)))));
        // s2's call and the turn-hook waiter are untouched (still parked).
        assert!(
            demux
                .take_response_waiter(&RequestId::new("c").unwrap())
                .is_some(),
            "the s2 call must remain parked"
        );
        assert!(
            demux
                .take_response_waiter(&RequestId::new("hook").unwrap())
                .is_some(),
            "the turn-hook waiter must remain parked"
        );
        // Keep the receivers alive until the asserts above ran.
        drop((rx_other, rx_hook));
    }

    #[tokio::test]
    async fn fail_calls_for_session_is_idempotent_after_resolution() {
        // A call already resolved (waiter taken) must not be double-counted by
        // the short-circuit.
        let demux = Demux::new();
        let s1 = SessionId::new("s1").expect("valid");
        let (tx_a, rx_a) = oneshot::channel();
        demux.register_call_response_waiter(RequestId::new("a").unwrap(), s1.clone(), tx_a);
        // Simulate the server-side resolution taking the waiter first.
        let waiter = demux
            .take_response_waiter(&RequestId::new("a").unwrap())
            .expect("waiter present");
        drop(waiter);
        drop(rx_a);
        let n = demux.fail_calls_for_session(&s1, || ClientError::NetworkError("gone".to_owned()));
        assert_eq!(n, 0, "already-resolved call is not re-failed");
    }

    #[tokio::test]
    async fn short_circuit_then_late_response_is_unrouted() {
        // A short-circuit that resolves first leaves no waiter, so a late server
        // response for the same id is dropped (no double-resolve).
        let demux = Demux::new();
        let s1 = SessionId::new("s1").expect("valid");
        let (tx_a, rx_a) = oneshot::channel();
        demux.register_call_response_waiter(RequestId::new("a").unwrap(), s1.clone(), tx_a);

        let n = demux.fail_calls_for_session(&s1, || ClientError::NetworkError("gone".to_owned()));
        assert_eq!(n, 1);
        assert!(matches!(rx_a.await, Ok(Err(ClientError::NetworkError(_)))));

        let outcome = demux.route(json!({ "jsonrpc": "2.0", "id": "a", "result": {} }));
        assert_eq!(
            outcome,
            RouteOutcome::Unrouted,
            "the late normal response must not double-resolve the call"
        );
    }

    #[tokio::test]
    async fn session_route_pushes_to_inbox() {
        let demux = Demux::new();
        let session = SessionId::new("s1").expect("valid");
        let (tx, mut rx) = mpsc::channel(4);
        demux.register_session_inbox(session.clone(), tx);
        let frame = json!({
            "jsonrpc": "2.0",
            "id": "x",
            "session_id": "s1",
            "method": "tool_call_request",
            "params": {},
        });
        let outcome = demux.route(frame.clone());
        assert_eq!(outcome, RouteOutcome::Session);
        match rx.recv().await {
            Some(InboundFrame::Request(value)) => assert_eq!(value, frame),
            other => panic!("expected request inbound; got {other:?}"),
        }
    }

    #[tokio::test]
    async fn reverse_hook_request_routes_to_inbox_as_request() {
        // A reverse hook request carries an `id`, so it must route to the
        // inbox as `Request` (not `Notification`) for the harness to answer.
        let demux = Demux::new();
        let session = SessionId::new("s1").expect("valid");
        let (tx, mut rx) = mpsc::channel(4);
        demux.register_session_inbox(session.clone(), tx);
        let hook = echo_agent_tool_protocol::HookFrame::custom_request(
            session.clone(),
            "hook-7".to_owned(),
            crate::harness::PERMISSION_REQUEST_KIND.to_owned(),
            json!({}),
        );
        let frame = json!({
            "jsonrpc": "2.0",
            "id": "h1",
            "session_id": "s1",
            "method": echo_agent_tool_protocol::Method::Hook.as_wire_str(),
            "params": serde_json::to_value(&hook).expect("serialize hook"),
        });
        assert_eq!(demux.route(frame.clone()), RouteOutcome::Session);
        match rx.recv().await {
            Some(InboundFrame::Request(value)) => assert_eq!(value, frame),
            other => panic!("expected request inbound; got {other:?}"),
        }
    }

    #[tokio::test]
    async fn notification_classified_without_id() {
        let demux = Demux::new();
        let session = SessionId::new("s1").expect("valid");
        let (tx, mut rx) = mpsc::channel(4);
        demux.register_session_inbox(session.clone(), tx);
        let frame = json!({
            "jsonrpc": "2.0",
            "session_id": "s1",
            "method": "tool.notification",
            "params": {},
        });
        let outcome = demux.route(frame);
        assert_eq!(outcome, RouteOutcome::Session);
        match rx.recv().await {
            Some(InboundFrame::Notification(_)) => {}
            other => panic!("expected notification; got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_session_returns_unknown_session() {
        let demux = Demux::new();
        let outcome = demux.route(
            json!({"jsonrpc":"2.0","id":"x","session_id":"missing","method":"x","params":{}}),
        );
        assert_eq!(outcome, RouteOutcome::UnknownSession);
    }

    #[tokio::test]
    async fn unknown_request_id_returns_unrouted() {
        let demux = Demux::new();
        let outcome = demux.route(json!({
            "jsonrpc": "2.0",
            "id": "missing",
            "result": {},
        }));
        assert_eq!(outcome, RouteOutcome::Unrouted);
    }

    #[tokio::test]
    async fn full_inbox_returns_inbox_full_without_blocking() {
        let demux = Demux::new();
        let session = SessionId::new("backed_up").expect("valid");
        let (tx, _rx) = mpsc::channel(1);
        demux.register_session_inbox(session.clone(), tx);
        let frame = || {
            json!({
                "jsonrpc": "2.0",
                "id": "x",
                "session_id": "backed_up",
                "method": "tool_call_request",
                "params": {},
            })
        };
        // First send fills capacity.
        assert_eq!(demux.route(frame()), RouteOutcome::Session);
        // Second send must NOT block; it returns InboxFull.
        assert_eq!(demux.route(frame()), RouteOutcome::InboxFull);
    }

    #[tokio::test]
    async fn unregister_session_inbox_if_is_identity_guarded() {
        let demux = Demux::new();
        let session = SessionId::new("id-guard").expect("valid");
        let (old_tx, _old_rx) = mpsc::channel(1);
        let (new_tx, _new_rx) = mpsc::channel(1);
        demux.register_session_inbox(session.clone(), old_tx.clone());
        demux.register_session_inbox(session.clone(), new_tx.clone());
        // Stale teardown with old sender must not remove the peer's inbox.
        assert!(
            demux
                .unregister_session_inbox_if(&session, &old_tx)
                .is_none()
        );
        assert!(demux.sessions.get(&session).is_some());
        // Matching sender removes.
        assert!(
            demux
                .unregister_session_inbox_if(&session, &new_tx)
                .is_some()
        );
        assert!(demux.sessions.get(&session).is_none());
    }

    #[tokio::test]
    async fn dropped_receiver_returns_session_dropped() {
        let demux = Demux::new();
        let session = SessionId::new("gone").expect("valid");
        let (tx, rx) = mpsc::channel(1);
        demux.register_session_inbox(session.clone(), tx);
        drop(rx);
        let frame = json!({
            "jsonrpc": "2.0",
            "id": "x",
            "session_id": "gone",
            "method": "tool_call_request",
            "params": {},
        });
        assert_eq!(demux.route(frame), RouteOutcome::SessionDropped);
        // Stale binding should have been removed.
        assert!(demux.sessions.get(&session).is_none());
    }

    #[tokio::test]
    async fn inbox_full_request_synthesizes_overloaded_response_onto_outbound() {
        // A full session inbox for a Request must produce the shared
        // -32016 "tool_busy" response on outbound, not a silent drop.
        let (out_tx, mut out_rx) = mpsc::channel::<String>(4);
        let demux = Demux::with_outbound(out_tx);
        let session = SessionId::new("busy").expect("valid");
        let (tx, _rx) = mpsc::channel(1);
        demux.register_session_inbox(session.clone(), tx);
        let frame = |id: &str| {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "session_id": "busy",
                "method": "tool_call_request",
                "params": {},
            })
        };
        // First fills capacity (cap 1); second overflows → InboxFull.
        assert_eq!(demux.route(frame("a")), RouteOutcome::Session);
        assert_eq!(demux.route(frame("b")), RouteOutcome::InboxFull);

        let text = out_rx.try_recv().expect("overloaded response enqueued");
        let wire: Value = serde_json::from_str(&text).expect("valid json");
        assert_eq!(wire["id"], "b");
        assert_eq!(wire["session_id"], "busy");
        assert_eq!(wire["error"]["code"], -32016);
        assert_eq!(wire["error"]["data"]["code"], "tool_busy");
        assert_eq!(wire["error"]["data"]["retryable"], true);
        assert!(
            out_rx.try_recv().is_err(),
            "exactly one rejection emitted for one overflow"
        );
    }

    #[tokio::test]
    async fn inbox_full_request_with_malformed_id_still_emits_overloaded_response() {
        // A Request whose `id` is present but not a valid JsonRpcId
        // (object/array/null) must NOT be silently dropped on a full
        // inbox: it still gets the shared -32016 response, with the raw
        // id echoed back as a string.
        let (out_tx, mut out_rx) = mpsc::channel::<String>(4);
        let demux = Demux::with_outbound(out_tx);
        let session = SessionId::new("bad_id").expect("valid");
        let (tx, _rx) = mpsc::channel(1);
        demux.register_session_inbox(session.clone(), tx);
        let frame = |id: Value| {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "session_id": "bad_id",
                "method": "tool_call_request",
                "params": {},
            })
        };
        // First fills capacity (cap 1); the malformed-id second overflows.
        assert_eq!(demux.route(frame(json!("a"))), RouteOutcome::Session);
        assert_eq!(
            demux.route(frame(json!({ "nested": 1 }))),
            RouteOutcome::InboxFull
        );

        let text = out_rx.try_recv().expect("overloaded response enqueued");
        let wire: Value = serde_json::from_str(&text).expect("valid json");
        assert_eq!(
            wire["id"], "{\"nested\":1}",
            "malformed id is echoed back as its raw JSON text"
        );
        assert_eq!(wire["error"]["code"], -32016);
        assert_eq!(wire["error"]["data"]["code"], "tool_busy");
    }

    #[tokio::test]
    async fn inbox_full_notification_is_dropped_without_outbound_response() {
        // A Notification (no id) on a full inbox stays fire-and-forget:
        // no synthesized response is emitted.
        let (out_tx, mut out_rx) = mpsc::channel::<String>(4);
        let demux = Demux::with_outbound(out_tx);
        let session = SessionId::new("notif_busy").expect("valid");
        let (tx, _rx) = mpsc::channel(1);
        demux.register_session_inbox(session.clone(), tx);
        let notif = || {
            json!({
                "jsonrpc": "2.0",
                "session_id": "notif_busy",
                "method": "tool.notification",
                "params": {},
            })
        };
        // First notification fills capacity; second overflows.
        assert_eq!(demux.route(notif()), RouteOutcome::Session);
        assert_eq!(demux.route(notif()), RouteOutcome::InboxFull);
        assert!(
            out_rx.try_recv().is_err(),
            "notifications must not synthesize an outbound response"
        );
    }

    #[tokio::test]
    async fn inbox_full_request_without_outbound_does_not_panic() {
        // A bare demux (no outbound, e.g. unit context) must still report
        // InboxFull cleanly when it cannot synthesize a rejection.
        let demux = Demux::new();
        let session = SessionId::new("no_out").expect("valid");
        let (tx, _rx) = mpsc::channel(1);
        demux.register_session_inbox(session.clone(), tx);
        let frame = || {
            json!({
                "jsonrpc": "2.0",
                "id": "x",
                "session_id": "no_out",
                "method": "tool_call_request",
                "params": {},
            })
        };
        assert_eq!(demux.route(frame()), RouteOutcome::Session);
        assert_eq!(demux.route(frame()), RouteOutcome::InboxFull);
    }

    #[tokio::test]
    async fn progress_route_pushes_to_progress_waiter() {
        let demux = Demux::new();
        let call_id = ToolCallId::new_v7();
        let (tx, mut rx) = mpsc::channel(4);
        demux
            .try_register_progress_waiter(call_id.clone(), tx)
            .expect("first registration");
        let frame = json!({
            "jsonrpc": "2.0",
            "session_id": "any",
            "method": "tool_call_progress",
            "params": {
                "tool_call_id": call_id.as_str(),
                "kind": "log_chunk",
                "body": {"text": "hello"},
            },
        });
        let outcome = demux.route(frame);
        assert_eq!(outcome, RouteOutcome::Progress);
        let progress = rx.recv().await.expect("progress frame");
        assert_eq!(progress.tool_call_id, call_id);
        assert_eq!(progress.kind, "log_chunk");
        assert_eq!(progress.body, json!({"text": "hello"}));
    }

    #[tokio::test]
    async fn progress_with_no_waiter_returns_unknown_progress() {
        let demux = Demux::new();
        let call_id = ToolCallId::new_v7();
        let frame = json!({
            "jsonrpc": "2.0",
            "session_id": "any",
            "method": "tool_call_progress",
            "params": {
                "tool_call_id": call_id.as_str(),
                "kind": "log_chunk",
                "body": {},
            },
        });
        assert_eq!(demux.route(frame), RouteOutcome::UnknownProgress);
    }

    #[tokio::test]
    async fn dropped_progress_receiver_returns_progress_dropped() {
        let demux = Demux::new();
        let call_id = ToolCallId::new_v7();
        let (tx, rx) = mpsc::channel(1);
        demux
            .try_register_progress_waiter(call_id.clone(), tx)
            .expect("first registration");
        drop(rx);
        let frame = json!({
            "jsonrpc": "2.0",
            "session_id": "any",
            "method": "tool_call_progress",
            "params": {
                "tool_call_id": call_id.as_str(),
                "kind": "x",
                "body": {},
            },
        });
        assert_eq!(demux.route(frame), RouteOutcome::ProgressDropped);
        assert!(demux.progress.get(&call_id).is_none());
    }

    #[tokio::test]
    async fn unregister_progress_waiter_returns_sender_when_present() {
        let demux = Demux::new();
        let call_id = ToolCallId::new_v7();
        let (tx, _rx) = mpsc::channel::<ToolCallProgressFrame>(1);
        demux
            .try_register_progress_waiter(call_id.clone(), tx)
            .expect("first registration");
        assert!(demux.unregister_progress_waiter(&call_id).is_some());
        assert!(demux.unregister_progress_waiter(&call_id).is_none());
    }

    #[tokio::test]
    async fn try_register_progress_waiter_rejects_collision_and_preserves_existing() {
        let demux = Demux::new();
        let call_id = ToolCallId::new_v7();
        let (tx_first, mut rx_first) = mpsc::channel::<ToolCallProgressFrame>(1);
        demux
            .try_register_progress_waiter(call_id.clone(), tx_first)
            .expect("first registration");
        let (tx_second, _rx_second) = mpsc::channel::<ToolCallProgressFrame>(1);
        let returned = demux
            .try_register_progress_waiter(call_id.clone(), tx_second)
            .expect_err("collision returns the rejected sender");
        // Returned sender is independent of the live one: dropping
        // it must not close the original receiver.
        drop(returned);
        let frame = json!({
            "jsonrpc": "2.0",
            "session_id": "any",
            "method": "tool_call_progress",
            "params": {
                "tool_call_id": call_id.as_str(),
                "kind": "log_chunk",
                "body": {"text": "first"},
            },
        });
        assert_eq!(demux.route(frame), RouteOutcome::Progress);
        let progress = rx_first.recv().await.expect("original receiver still live");
        assert_eq!(progress.body, json!({"text": "first"}));
    }

    #[tokio::test]
    async fn full_progress_channel_returns_progress_full_without_blocking() {
        let demux = Demux::new();
        let call_id = ToolCallId::new_v7();
        let (tx, _rx) = mpsc::channel::<ToolCallProgressFrame>(1);
        demux
            .try_register_progress_waiter(call_id.clone(), tx)
            .expect("first registration");
        let frame = || {
            json!({
                "jsonrpc": "2.0",
                "session_id": "any",
                "method": "tool_call_progress",
                "params": {
                    "tool_call_id": call_id.as_str(),
                    "kind": "x",
                    "body": {},
                },
            })
        };
        // First send fills capacity (mpsc(1)).
        assert_eq!(demux.route(frame()), RouteOutcome::Progress);
        // Second send must NOT block; it returns ProgressFull.
        assert_eq!(demux.route(frame()), RouteOutcome::ProgressFull);
    }

    #[tokio::test]
    async fn drain_progress_removes_all_waiters_and_drops_senders() {
        let demux = Demux::new();
        let call_a = ToolCallId::new_v7();
        let call_b = ToolCallId::new_v7();
        let (tx_a, mut rx_a) = mpsc::channel::<ToolCallProgressFrame>(1);
        let (tx_b, mut rx_b) = mpsc::channel::<ToolCallProgressFrame>(1);
        demux
            .try_register_progress_waiter(call_a.clone(), tx_a)
            .expect("first registration");
        demux
            .try_register_progress_waiter(call_b.clone(), tx_b)
            .expect("first registration");
        assert_eq!(demux.progress.len(), 2);

        demux.drain_progress();

        // Post-drain: every entry removed.
        assert_eq!(demux.progress.len(), 0);
        assert!(demux.progress.get(&call_a).is_none());
        assert!(demux.progress.get(&call_b).is_none());
        // The senders held by the demux were dropped, so each
        // receiver sees `None` (channel closed).
        assert!(
            rx_a.recv().await.is_none(),
            "sender dropped → receiver closes"
        );
        assert!(
            rx_b.recv().await.is_none(),
            "sender dropped → receiver closes"
        );
    }
}
