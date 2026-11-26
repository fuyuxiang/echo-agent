//! `RemoteToolProxy` and `RemoteTransport` coverage. A channel-backed
//! mock `ConnectionClient` lets the test inspect outgoing frames and
//! drive synthetic responses + progress without any tokio I/O.

use std::sync::{Arc, Mutex};

use dashmap::DashMap;

use async_trait::async_trait;
use futures::StreamExt;
use futures::channel::{mpsc, oneshot};
use futures::stream::BoxStream;

use echo_agent_computer_hub_core::{
    ConnectionClient, RemoteToolProxy, RemoteTransport, ToolHandle, Transport, TransportKind,
};
use echo_agent_tool_protocol::{
    JsonRpcError, JsonRpcId, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, JsonRpcVersion,
    Method, ResponseOutcome, SessionId, ToolCallId, ToolCallParams, ToolCallProgressFrame,
    ToolCallResult, ToolCapabilities, ToolErrorWire, ToolId, ToolOutputWire, UserId,
};
use echo_agent_tool_runtime::{
    ContentBlock, ToolCallContext, ToolError, ToolOutput, ToolProgress, ToolStreamItem,
};
use echo_agent_tool_types::ToolDescription;

/// Programmable `ConnectionClient`. Each request gets a pre-staged
/// response; progress frames are pushed through per-call senders.
#[derive(Debug, Default)]
struct MockConnection {
    /// Senders keyed by `tool_call_id`. Pulled out of the inner state so
    /// per-call subscription touches a lock-free DashMap rather than the
    /// shared Mutex that guards the rest of the queue + capture state.
    progress_senders: DashMap<ToolCallId, mpsc::UnboundedSender<ToolCallProgressFrame>>,
    /// Three-Vec state guarded by one Mutex. The lock provides atomic
    /// pop-from-`responses` + push-to-`captured_requests` semantics that
    /// some tests rely on.
    inner: Mutex<MockState>,
}

#[derive(Default)]
struct MockState {
    /// FIFO queue of responses to return for each `request` call.
    responses: Vec<MockResponse>,
    /// Captured outgoing requests so tests can assert on them.
    captured_requests: Vec<JsonRpcRequest>,
    /// Captured one-way notifications.
    captured_notifications: Vec<JsonRpcNotification>,
}

impl std::fmt::Debug for MockState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockState")
            .field("responses_len", &self.responses.len())
            .field("captured_reqs", &self.captured_requests.len())
            .field("captured_notifs", &self.captured_notifications.len())
            .finish()
    }
}

enum MockResponse {
    Ok(serde_json::Value),
    Err(JsonRpcError),
    /// Resolves a oneshot when the request arrives so the test can
    /// release progress before allowing the response.
    Gated {
        gate: oneshot::Receiver<()>,
        body: serde_json::Value,
    },
    /// Fail at the transport layer (e.g. socket dropped).
    Network(String),
}

impl MockConnection {
    fn enqueue_ok(&self, body: serde_json::Value) {
        self.inner
            .lock()
            .expect("mutex")
            .responses
            .push(MockResponse::Ok(body));
    }

    fn enqueue_err(&self, code: i32, message: impl Into<String>, data: Option<serde_json::Value>) {
        self.inner
            .lock()
            .expect("mutex")
            .responses
            .push(MockResponse::Err(JsonRpcError {
                code,
                message: message.into(),
                data,
            }));
    }

    fn enqueue_gated(&self, gate: oneshot::Receiver<()>, body: serde_json::Value) {
        self.inner
            .lock()
            .expect("mutex")
            .responses
            .push(MockResponse::Gated { gate, body });
    }

    fn enqueue_network_failure(&self, message: impl Into<String>) {
        self.inner
            .lock()
            .expect("mutex")
            .responses
            .push(MockResponse::Network(message.into()));
    }

    fn last_request(&self) -> Option<JsonRpcRequest> {
        self.inner
            .lock()
            .expect("mutex")
            .captured_requests
            .last()
            .cloned()
    }

    fn captured_request_count(&self) -> usize {
        self.inner.lock().expect("mutex").captured_requests.len()
    }

    fn push_progress(&self, tool_call_id: &ToolCallId, frame: ToolCallProgressFrame) {
        if let Some(tx) = self.progress_senders.get(tool_call_id) {
            let _ = tx.value().unbounded_send(frame);
        }
    }
}

#[async_trait]
impl ConnectionClient for MockConnection {
    async fn request(&self, request: JsonRpcRequest) -> Result<JsonRpcResponse, ToolError> {
        let response = {
            let mut guard = self.inner.lock().expect("mutex");
            guard.captured_requests.push(request.clone());
            if guard.responses.is_empty() {
                return Err(ToolError::custom(
                    "mock_response_missing",
                    "no response staged",
                ));
            }
            guard.responses.remove(0)
        };
        match response {
            MockResponse::Ok(body) => Ok(JsonRpcResponse {
                jsonrpc: JsonRpcVersion,
                id: request.id,
                session_id: request.session_id,
                outcome: ResponseOutcome::Result(body),
            }),
            MockResponse::Err(err) => Ok(JsonRpcResponse {
                jsonrpc: JsonRpcVersion,
                id: request.id,
                session_id: request.session_id,
                outcome: ResponseOutcome::Error(err),
            }),
            MockResponse::Gated { gate, body } => {
                let _ = gate.await;
                Ok(JsonRpcResponse {
                    jsonrpc: JsonRpcVersion,
                    id: request.id,
                    session_id: request.session_id,
                    outcome: ResponseOutcome::Result(body),
                })
            }
            MockResponse::Network(msg) => Err(ToolError::network_error(msg)),
        }
    }

    async fn subscribe_progress(
        &self,
        tool_call_id: ToolCallId,
    ) -> BoxStream<'static, ToolCallProgressFrame> {
        let (tx, rx) = mpsc::unbounded();
        self.progress_senders.insert(tool_call_id, tx);
        rx.boxed()
    }

    async fn notify(&self, notification: JsonRpcNotification) -> Result<(), ToolError> {
        self.inner
            .lock()
            .expect("mutex")
            .captured_notifications
            .push(notification);
        Ok(())
    }
}

fn sid(s: &str) -> SessionId {
    SessionId::new(s).expect("session id")
}

fn tid(s: &str) -> ToolId {
    ToolId::new(s).expect("tool id")
}

fn uid(s: &str) -> UserId {
    UserId::new(s).expect("user id")
}

fn description_for(name: &str) -> ToolDescription {
    ToolDescription::new(name, format!("desc for {name}"))
}

fn ok_call_result(call_id: &ToolCallId, output: ToolOutputWire) -> serde_json::Value {
    serde_json::to_value(ToolCallResult {
        tool_call_id: call_id.clone(),
        output,
        follow_ups: vec![],
        reminders: vec![],
        chat_completion_output: None,
    })
    .expect("serialise call result")
}

fn ok_call_result_with_cco(
    call_id: &ToolCallId,
    output: ToolOutputWire,
    chat_completion_output: serde_json::Value,
) -> serde_json::Value {
    serde_json::to_value(ToolCallResult {
        tool_call_id: call_id.clone(),
        output,
        follow_ups: vec![],
        reminders: vec![],
        chat_completion_output: Some(chat_completion_output),
    })
    .expect("serialise call result")
}

#[tokio::test]
async fn proxy_sends_well_formed_tool_call_request() {
    let conn = Arc::new(MockConnection::default());
    let proxy = RemoteToolProxy::new(
        tid("foo"),
        sid("sess-1"),
        description_for("foo"),
        ToolCapabilities::default(),
        conn.clone(),
    );
    let call_id = ToolCallId::new_v7();
    let ctx = ToolCallContext::new(call_id.clone());
    conn.enqueue_ok(ok_call_result(
        &call_id,
        ToolOutputWire::Text("hello".to_string()),
    ));
    let mut stream = proxy.execute(ctx, serde_json::json!({"k": "v"})).await;
    while stream.next().await.is_some() {}
    let req = conn.last_request().expect("captured request");
    assert_eq!(req.method, Method::ToolCallRequest.as_wire_str());
    let params: ToolCallParams = serde_json::from_value(req.params).expect("decode params");
    assert_eq!(params.tool_id, tid("foo"));
    assert_eq!(params.tool_call_id, call_id);
    assert_eq!(params.arguments, serde_json::json!({"k": "v"}));
}

#[tokio::test]
async fn progress_then_terminal_orders_correctly() {
    let conn = Arc::new(MockConnection::default());
    let proxy = RemoteToolProxy::new(
        tid("foo"),
        sid("sess-1"),
        description_for("foo"),
        ToolCapabilities::default(),
        conn.clone(),
    );
    let call_id = ToolCallId::new_v7();
    let ctx = ToolCallContext::new(call_id.clone());
    let (gate_tx, gate_rx) = oneshot::channel();
    conn.enqueue_gated(
        gate_rx,
        ok_call_result(&call_id, ToolOutputWire::Text("done".to_string())),
    );
    let mut stream = proxy.execute(ctx, serde_json::json!(null)).await;

    // Push two progress frames before the terminal is unblocked.
    conn.push_progress(
        &call_id,
        ToolCallProgressFrame {
            tool_call_id: call_id.clone(),
            kind: "log".to_string(),
            body: serde_json::json!({"text": "tick"}),
            dropped_count: None,
        },
    );
    conn.push_progress(
        &call_id,
        ToolCallProgressFrame {
            tool_call_id: call_id.clone(),
            kind: "log".to_string(),
            body: serde_json::json!({"text": "tock"}),
            dropped_count: None,
        },
    );

    let first = stream.next().await.expect("first item");
    let second = stream.next().await.expect("second item");
    match (&first, &second) {
        (ToolStreamItem::Progress(p1), ToolStreamItem::Progress(p2)) => {
            match p1 {
                ToolProgress::Custom { subkind, payload } => {
                    assert_eq!(subkind, "log");
                    assert_eq!(payload, &serde_json::json!({"text": "tick"}));
                }
                other => panic!("expected Custom progress, got {other:?}"),
            }
            match p2 {
                ToolProgress::Custom { subkind, .. } => assert_eq!(subkind, "log"),
                other => panic!("expected Custom progress, got {other:?}"),
            }
        }
        other => panic!("expected two Progress items, got {other:?}"),
    }

    // Release the response and consume the terminal.
    let _ = gate_tx.send(());
    let terminal = stream.next().await.expect("terminal");
    match terminal {
        ToolStreamItem::Terminal(Ok(typed)) => {
            assert_eq!(typed.value, serde_json::json!("done"));
        }
        other => panic!("expected Terminal(Ok), got {other:?}"),
    }
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn json_rpc_error_response_decodes_into_tool_error() {
    let conn = Arc::new(MockConnection::default());
    let proxy = RemoteToolProxy::new(
        tid("foo"),
        sid("sess-1"),
        description_for("foo"),
        ToolCapabilities::default(),
        conn.clone(),
    );
    let wire = ToolErrorWire::ToolNotFound {
        tool_id: tid("foo"),
    };
    conn.enqueue_err(
        -32011,
        "tool not found",
        Some(serde_json::to_value(&wire).unwrap()),
    );
    let mut stream = proxy
        .execute(ToolCallContext::default(), serde_json::json!(null))
        .await;
    let item = stream.next().await.expect("terminal");
    match item {
        ToolStreamItem::Terminal(Err(ref e))
            if e.kind == echo_agent_tool_runtime::ToolErrorKind::NotFound =>
        {
            assert!(
                e.detail.contains("foo"),
                "detail should mention tool id: {}",
                e.detail
            );
        }
        other => panic!("expected Terminal(NotFound), got {other:?}"),
    }
}

#[tokio::test]
async fn network_failure_surfaces_as_terminal_network_error() {
    let conn = Arc::new(MockConnection::default());
    let proxy = RemoteToolProxy::new(
        tid("foo"),
        sid("sess-1"),
        description_for("foo"),
        ToolCapabilities::default(),
        conn.clone(),
    );
    conn.enqueue_network_failure("socket closed");
    let mut stream = proxy
        .execute(ToolCallContext::default(), serde_json::json!(null))
        .await;
    let item = stream.next().await.expect("terminal");
    match item {
        ToolStreamItem::Terminal(Err(ref e))
            if e.kind == echo_agent_tool_runtime::ToolErrorKind::NetworkError =>
        {
            assert!(
                e.detail.contains("socket closed"),
                "detail should mention cause: {}",
                e.detail
            );
        }
        other => panic!("expected Terminal(NetworkError), got {other:?}"),
    }
}

#[tokio::test]
async fn mcp_output_re_serialises_into_blocks_value() {
    let conn = Arc::new(MockConnection::default());
    let proxy = RemoteToolProxy::new(
        tid("foo"),
        sid("sess-1"),
        description_for("foo"),
        ToolCapabilities::default(),
        conn.clone(),
    );
    let call_id = ToolCallId::new_v7();
    let ctx = ToolCallContext::new(call_id.clone());
    let blocks = vec![echo_agent_tool_protocol::McpBlock::Text {
        text: "hello".to_string(),
    }];
    conn.enqueue_ok(ok_call_result(&call_id, ToolOutputWire::Mcp { blocks }));
    let mut stream = proxy.execute(ctx, serde_json::json!(null)).await;
    let item = stream.next().await.expect("terminal");
    match item {
        ToolStreamItem::Terminal(Ok(typed)) => {
            // The wire blocks round-trip through ContentBlock; assert the
            // text value survives the transformation.
            let blocks_value = typed
                .value
                .get("blocks")
                .and_then(|v| v.as_array())
                .cloned()
                .expect("blocks array");
            assert_eq!(blocks_value.len(), 1);
            let block: ContentBlock =
                serde_json::from_value(blocks_value[0].clone()).expect("decode runtime block");
            match block {
                ContentBlock::Text { text } => assert_eq!(text, "hello"),
                other => panic!("expected Text block, got {other:?}"),
            }
        }
        other => panic!("expected Terminal(Ok), got {other:?}"),
    }
}

#[tokio::test]
async fn terminal_carries_chat_completion_output_from_wire() {
    let conn = Arc::new(MockConnection::default());
    let proxy = RemoteToolProxy::new(
        tid("bash"),
        sid("sess-1"),
        description_for("bash"),
        ToolCapabilities::default(),
        conn.clone(),
    );
    let call_id = ToolCallId::new_v7();
    let ctx = ToolCallContext::new(call_id.clone());
    let cco = serde_json::json!({
        "result": {
            "sender": "assistant",
            "message": "",
            "code_execution_result": {
                "stdout": "hi\n",
                "stderr": "",
                "exit_code": 0,
                "command_timed_out": false
            }
        }
    });
    conn.enqueue_ok(ok_call_result_with_cco(
        &call_id,
        ToolOutputWire::Json(serde_json::json!({"stdout": "hi\n"})),
        cco,
    ));
    let mut stream = proxy.execute(ctx, serde_json::json!(null)).await;
    let item = stream.next().await.expect("terminal");
    match item {
        ToolStreamItem::Terminal(Ok(typed)) => {
            let response = typed
                .chat_completion_output()
                .expect("chat completion output survives the wire");
            let completion = response.result.expect("completion result present");
            let exec = completion
                .code_execution_result
                .expect("code execution result present");
            assert_eq!(exec.stdout, "hi\n");
            assert_eq!(exec.exit_code, 0);
            assert!(!exec.command_timed_out);
        }
        other => panic!("expected Terminal(Ok), got {other:?}"),
    }
}

#[tokio::test]
async fn terminal_without_chat_completion_output_is_none() {
    let conn = Arc::new(MockConnection::default());
    let proxy = RemoteToolProxy::new(
        tid("bash"),
        sid("sess-1"),
        description_for("bash"),
        ToolCapabilities::default(),
        conn.clone(),
    );
    let call_id = ToolCallId::new_v7();
    let ctx = ToolCallContext::new(call_id.clone());
    conn.enqueue_ok(ok_call_result(
        &call_id,
        ToolOutputWire::Json(serde_json::json!({"stdout": "hi\n"})),
    ));
    let mut stream = proxy.execute(ctx, serde_json::json!(null)).await;
    let item = stream.next().await.expect("terminal");
    match item {
        ToolStreamItem::Terminal(Ok(typed)) => {
            assert!(typed.chat_completion_output().is_none());
        }
        other => panic!("expected Terminal(Ok), got {other:?}"),
    }
}

#[tokio::test]
async fn malformed_inner_chat_completion_output_degrades_to_none() {
    let conn = Arc::new(MockConnection::default());
    let proxy = RemoteToolProxy::new(
        tid("bash"),
        sid("sess-1"),
        description_for("bash"),
        ToolCapabilities::default(),
        conn.clone(),
    );
    let call_id = ToolCallId::new_v7();
    let ctx = ToolCallContext::new(call_id.clone());
    conn.enqueue_ok(ok_call_result_with_cco(
        &call_id,
        ToolOutputWire::Json(serde_json::json!({"stdout": "hi\n"})),
        serde_json::json!({"result": "not-a-completion-object"}),
    ));
    let mut stream = proxy.execute(ctx, serde_json::json!(null)).await;
    let item = stream.next().await.expect("terminal");
    match item {
        ToolStreamItem::Terminal(Ok(typed)) => {
            assert_eq!(typed.value, serde_json::json!({"stdout": "hi\n"}));
            assert!(typed.chat_completion_output().is_none());
        }
        other => panic!("expected Terminal(Ok) with degraded cco, got {other:?}"),
    }
}

#[tokio::test]
async fn bare_non_enveloped_success_body_passes_through() {
    let conn = Arc::new(MockConnection::default());
    let proxy = RemoteToolProxy::new(
        tid("bash"),
        sid("sess-1"),
        description_for("bash"),
        ToolCapabilities::default(),
        conn.clone(),
    );
    let ctx = ToolCallContext::new(ToolCallId::new_v7());
    conn.enqueue_ok(serde_json::json!({"stdout": "hi\n"}));
    let mut stream = proxy.execute(ctx, serde_json::json!(null)).await;
    let item = stream.next().await.expect("terminal");
    match item {
        ToolStreamItem::Terminal(Ok(typed)) => {
            assert_eq!(typed.value, serde_json::json!({"stdout": "hi\n"}));
            assert!(typed.chat_completion_output().is_none());
        }
        other => panic!("expected Terminal(Ok) passthrough, got {other:?}"),
    }
}

#[tokio::test]
async fn malformed_envelope_with_tool_call_id_surfaces_decode_error() {
    let conn = Arc::new(MockConnection::default());
    let proxy = RemoteToolProxy::new(
        tid("bash"),
        sid("sess-1"),
        description_for("bash"),
        ToolCapabilities::default(),
        conn.clone(),
    );
    let ctx = ToolCallContext::new(ToolCallId::new_v7());
    conn.enqueue_ok(serde_json::json!({"tool_call_id": "call_x", "output": 123}));
    let mut stream = proxy.execute(ctx, serde_json::json!(null)).await;
    let item = stream.next().await.expect("terminal");
    match item {
        ToolStreamItem::Terminal(Err(ref e))
            if e.kind == echo_agent_tool_runtime::ToolErrorKind::Custom =>
        {
            let code = e
                .details
                .as_ref()
                .and_then(|d| d.get("code"))
                .and_then(|c| c.as_str());
            assert_eq!(code, Some("response_decoding"), "error: {e:?}");
        }
        other => panic!("expected Terminal(Err) decode failure, got {other:?}"),
    }
}

#[tokio::test]
async fn remote_transport_call_dispatches_via_connection() {
    let conn = Arc::new(MockConnection::default());
    let transport = RemoteTransport::new(conn.clone(), sid("sess-1"), uid("alice"));
    let call_id = ToolCallId::new_v7();
    let ctx = ToolCallContext::new(call_id.clone());
    conn.enqueue_ok(ok_call_result(&call_id, ToolOutputWire::Text("hi".into())));
    let mut stream = transport
        .call(tid("foo"), serde_json::json!({"k": "v"}), ctx)
        .await;
    let _ = stream.next().await;
    assert_eq!(conn.captured_request_count(), 1);
    assert_eq!(transport.kind(), TransportKind::Remote);
}

#[tokio::test]
async fn remote_transport_authorize_returns_bound_principal() {
    let conn = Arc::new(MockConnection::default());
    let transport = RemoteTransport::new(conn, sid("sess-1"), uid("alice"));
    let principal = transport.authorize().await.expect("authorize");
    assert_eq!(principal.user_id, uid("alice"));
    assert!(principal.authorizes_session(&sid("sess-1")));
}

#[tokio::test]
async fn proxy_subscribe_happens_before_request_send() {
    // Locks in BOTH halves of the subscribe-before-send contract:
    //   1. the subscription IS active by the time `execute` returns;
    //   2. the request HAS NOT been sent yet at that point.
    // A future refactor that eagerly sent the request inside
    // `execute` would still satisfy (1) but would break (2).
    let conn = Arc::new(MockConnection::default());
    let proxy = RemoteToolProxy::new(
        tid("foo"),
        sid("sess-1"),
        description_for("foo"),
        ToolCapabilities::default(),
        conn.clone(),
    );
    let call_id = ToolCallId::new_v7();
    let ctx = ToolCallContext::new(call_id.clone());
    conn.enqueue_ok(ok_call_result(&call_id, ToolOutputWire::Text("ok".into())));
    let mut stream = proxy.execute(ctx, serde_json::json!(null)).await;
    {
        // The DashMap subscription read and the captured-requests check
        // are individually atomic. Single-threaded `#[tokio::test]`
        // execution means no other task can mutate either between the
        // two checks, so the pair is observationally simultaneous.
        assert!(
            conn.progress_senders.contains_key(&call_id),
            "subscription must be active before request send"
        );
        let guard = conn.inner.lock().expect("mutex");
        assert!(
            guard.captured_requests.is_empty(),
            "request must not be sent before stream is polled"
        );
    }
    // Polling the stream is what actually drives the request future,
    // so the captured-requests vec only fills in once we start consuming.
    while stream.next().await.is_some() {}
    {
        let guard = conn.inner.lock().expect("mutex");
        assert_eq!(
            guard.captured_requests.len(),
            1,
            "request must have been sent during stream polling"
        );
    }
}

#[tokio::test]
async fn notify_round_trips_through_connection_client() {
    let conn = Arc::new(MockConnection::default());
    let notification = JsonRpcNotification {
        jsonrpc: JsonRpcVersion,
        session_id: Some(sid("sess-1")),
        seq: None,
        method: Method::Hook.as_wire_str().to_string(),
        params: serde_json::json!({
            "session_id": "sess-1",
            "tool_id": "foo",
            "call_id": "call-1",
            "event": { "type": "Cancel" }
        }),
    };
    let trait_handle: &dyn ConnectionClient = conn.as_ref();
    trait_handle
        .notify(notification.clone())
        .await
        .expect("notify succeeds");
    let guard = conn.inner.lock().expect("mutex");
    assert_eq!(guard.captured_notifications.len(), 1);
    let captured = &guard.captured_notifications[0];
    assert_eq!(captured.method, Method::Hook.as_wire_str());
    assert_eq!(captured.session_id, Some(sid("sess-1")));
    assert_eq!(
        captured.params.get("event").and_then(|v| v.get("type")),
        Some(&serde_json::Value::String("Cancel".to_string()))
    );
    assert_eq!(captured, &notification);
}

#[tokio::test]
async fn json_rpc_id_is_unique_per_call() {
    let conn = Arc::new(MockConnection::default());
    let transport = RemoteTransport::new(conn.clone(), sid("sess-1"), uid("alice"));
    let call_a = ToolCallId::new_v7();
    let call_b = ToolCallId::new_v7();
    conn.enqueue_ok(ok_call_result(&call_a, ToolOutputWire::Text("a".into())));
    conn.enqueue_ok(ok_call_result(&call_b, ToolOutputWire::Text("b".into())));

    let mut s1 = transport
        .call(
            tid("foo"),
            serde_json::json!(null),
            ToolCallContext::new(call_a.clone()),
        )
        .await;
    while s1.next().await.is_some() {}
    let mut s2 = transport
        .call(
            tid("foo"),
            serde_json::json!(null),
            ToolCallContext::new(call_b.clone()),
        )
        .await;
    while s2.next().await.is_some() {}

    let guard = conn.inner.lock().expect("mutex");
    assert_eq!(guard.captured_requests.len(), 2);
    let id_a = match &guard.captured_requests[0].id {
        JsonRpcId::String(s) => s.clone(),
        JsonRpcId::Number(n) => n.to_string(),
    };
    let id_b = match &guard.captured_requests[1].id {
        JsonRpcId::String(s) => s.clone(),
        JsonRpcId::Number(n) => n.to_string(),
    };
    assert_ne!(id_a, id_b, "envelope ids must differ across calls");
}
