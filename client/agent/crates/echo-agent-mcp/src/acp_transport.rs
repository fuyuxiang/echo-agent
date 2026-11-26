//! rmcp transport bridge over the ACP reverse channel.
//!
//! In-process SDK MCP servers (the official `echo-agent-agent-sdk`'s `@tool` /
//! `create_sdk_mcp_server`) run in the SDK-host process, not behind a socket. The
//! agent reaches them by sending each MCP JSON-RPC message to the client as a
//! reverse `echo.agent/mcp/sdk_call` request and feeding the response back. This module
//! adapts that request/response channel into an rmcp transport so an in-process
//! server reuses the same `RunningService` / tool-dispatch path as HTTP/stdio
//! servers for tool calls.
//!
//! Half-duplex (v1 limitation): the bridge carries ONLY client→server requests and
//! their responses. Server→client traffic is NOT bridged — neither notifications
//! (`notifications/*`) nor server-initiated requests such as
//! `sampling/createMessage` or `roots/list` are delivered (elicitation is not
//! advertised on this transport, so compliant servers never send it). Tools that
//! depend on those features will not work over this transport yet. The duplex
//! plumbing below exists to decouple slow tool calls (one task per request), not to
//! deliver a second message direction.
//!
//! The invoker is abstract ([`AcpReverseInvoker`]) so this crate stays free of the
//! ACP gateway types; the host (shell) supplies an impl backed by its gateway.

use std::sync::Arc;
use std::time::Duration;

use rmcp::service::RoleClient;
use rmcp::transport::async_rw::AsyncRwTransport;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

/// Sends one MCP JSON-RPC message to an in-process server over the ACP reverse
/// channel (`echo.agent/mcp/sdk_call`) and returns its JSON-RPC response. The `Err` string is
/// surfaced as a JSON-RPC error to the waiting rmcp request (fail-closed: a missing
/// tool server is a real error, unlike a hook gate).
///
/// `timeout` bounds the single round trip so a missing or hung client fails this
/// reverse call instead of stalling the agent's tool loop forever. It carries the
/// resolved per-server tool timeout (the same `tool_timeout_ms` the HTTP path uses),
/// threaded in from the bridge so zero-IPC and loopback share one tool budget.
#[async_trait::async_trait]
pub trait AcpReverseInvoker: Send + Sync + 'static {
    async fn invoke(
        &self,
        server_id: &str,
        message: Value,
        timeout: Duration,
    ) -> Result<Value, String>;
}

/// rmcp transport for an in-process server reached over ACP reverse-RPC.
pub type AcpBridgeTransport = AsyncRwTransport<RoleClient, DuplexStream, DuplexStream>;

/// Duplex buffer for the bridge. MCP messages are small; this only needs to hold
/// one in-flight message comfortably.
const BRIDGE_BUF: usize = 256 * 1024;

/// Bounded capacity for the server→client response channel. The only producers are
/// the in-flight invoke tasks (one per outstanding rmcp request, and rmcp bounds its
/// own in-flight concurrency), so this small buffer gives backpressure/defensiveness
/// without ever realistically blocking a producer.
const RESPONSE_CHANNEL_CAP: usize = 128;

/// JSON-RPC "Internal error" code, used for every error this bridge synthesizes.
const INTERNAL_ERROR_CODE: i64 = -32603;

/// Build an rmcp transport that bridges to an in-process MCP server via `invoker`.
///
/// Spawns a pump that forwards each client→server message as a reverse
/// `echo.agent/mcp/sdk_call` and writes the server→client response back. The pump exits when
/// rmcp drops its half of the duplex (service shutdown), so it never leaks.
///
/// `invoke_timeout` is the resolved per-server tool timeout; it bounds every reverse
/// round trip so the zero-IPC path honors the same budget as loopback/HTTP.
pub fn acp_bridge_transport(
    server_id: String,
    invoker: Arc<dyn AcpReverseInvoker>,
    invoke_timeout: Duration,
) -> AcpBridgeTransport {
    let (agent_read, pump_write) = tokio::io::duplex(BRIDGE_BUF); // server -> client
    let (pump_read, agent_write) = tokio::io::duplex(BRIDGE_BUF); // client -> server
    tokio::spawn(pump(
        server_id,
        invoker,
        invoke_timeout,
        pump_read,
        pump_write,
    ));
    AsyncRwTransport::new(agent_read, agent_write)
}

/// Forward newline-delimited JSON-RPC between rmcp and the reverse channel.
///
/// Each client→server request is invoked in its own task so a slow tool can't block
/// later requests to the same server (JSON-RPC correlates by `id`, not order). All
/// responses funnel through one writer task so their bytes never interleave on the
/// duplex.
async fn pump(
    server_id: String,
    invoker: Arc<dyn AcpReverseInvoker>,
    invoke_timeout: Duration,
    client_to_server: DuplexStream,
    server_to_client: DuplexStream,
) {
    let (responses_tx, responses_rx) = tokio::sync::mpsc::channel::<String>(RESPONSE_CHANNEL_CAP);
    let writer = write_responses(server_to_client, responses_rx);
    let reader = read_requests(
        server_id,
        invoker,
        invoke_timeout,
        client_to_server,
        responses_tx,
    );
    // `writer` then drains and exits once `reader` returns and closes the channel.
    tokio::join!(reader, writer);
}

/// Read each client→server line and dispatch its request on a fresh task.
///
/// The spawned tasks live in a [`tokio::task::JoinSet`] owned by this function rather
/// than as detached `tokio::spawn`s, so when this function returns (EOF = teardown)
/// the set is dropped and every still-running invoke is aborted promptly instead of
/// being left to run out its timeout. Finished tasks are reaped (non-blockingly)
/// after each read so the set can't grow unbounded over a long-lived session.
///
/// IMPORTANT: `read_line` is NOT cancellation-safe, so it must never be raced in a
/// `select!`. A client→server message can arrive across multiple `fill_buf` chunks
/// (e.g. a tool call whose JSON args exceed the read buffer); if another `select!`
/// branch (such as reaping a finished invoke) fired while a `read_line` was pending,
/// the partially-consumed bytes would be dropped on the next `line.clear()`,
/// desyncing the JSON-RPC stream and hanging that request to its tool-level timeout.
/// We therefore read each line to completion FIRST, then reap finished invokes with a
/// synchronous, non-cancelling `try_join_next` drain.
async fn read_requests(
    server_id: String,
    invoker: Arc<dyn AcpReverseInvoker>,
    invoke_timeout: Duration,
    client_to_server: DuplexStream,
    responses_tx: tokio::sync::mpsc::Sender<String>,
) {
    let mut reader = BufReader::new(client_to_server);
    let mut line = String::new();
    let mut invokes: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => break, // rmcp closed its end
            Ok(_) => {}
        }
        // Reap finished invokes so the set stays bounded.
        while invokes.try_join_next().is_some() {}
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!(%err, "acp mcp bridge: dropping unparseable client message");
                continue;
            }
        };
        // An id-less message is a notification (no response). The SDK peer rejects reverse
        // `echo.agent/mcp/sdk_call`s without a JSON-RPC id, so id-less messages (e.g. rmcp's
        // `notifications/initialized` on every handshake) are logged and discarded locally
        // rather than spawning a doomed round-trip. Safe only because the SDK `Server` is
        // lenient about never receiving `initialized` (a documented v1 limit).
        let Some(id) = message.get("id").filter(|id| !id.is_null()).cloned() else {
            tracing::debug!(
                %message,
                "acp mcp bridge: discarding id-less notification (half-duplex v1)"
            );
            continue;
        };
        let invoker = invoker.clone();
        let server_id = server_id.clone();
        let responses_tx = responses_tx.clone();
        invokes.spawn(async move {
            let result = invoker.invoke(&server_id, message, invoke_timeout).await;
            let response = match result {
                Ok(response) => with_id(response, id),
                Err(err) => json_rpc_error(id, INTERNAL_ERROR_CODE, &err),
            };
            match serde_json::to_string(&response) {
                Ok(mut encoded) => {
                    encoded.push('\n');
                    let _ = responses_tx.send(encoded).await;
                }
                Err(err) => tracing::warn!(%err, "acp mcp bridge: failed to serialize response"),
            }
        });
    }
}

/// Serialize every server→client response onto the duplex through a single writer.
async fn write_responses(
    mut server_to_client: DuplexStream,
    mut responses_rx: tokio::sync::mpsc::Receiver<String>,
) {
    while let Some(encoded) = responses_rx.recv().await {
        if server_to_client
            .write_all(encoded.as_bytes())
            .await
            .is_err()
            || server_to_client.flush().await.is_err()
        {
            break; // rmcp closed its end
        }
    }
}

/// Overwrite a JSON-RPC response object's `id` with the request id.
///
/// If the SDK response isn't a JSON object (so it has nowhere to carry an `id`),
/// rmcp can't correlate it and the waiting request would otherwise stall until its
/// timeout. In that case synthesize a properly-keyed JSON-RPC error instead, so the
/// waiting request fails fast and correctly.
fn with_id(mut response: Value, id: Value) -> Value {
    match response.as_object_mut() {
        Some(obj) => {
            obj.insert("id".to_string(), id);
            response
        }
        None => json_rpc_error(
            id,
            INTERNAL_ERROR_CODE,
            "acp mcp bridge: server returned a non-object JSON-RPC response",
        ),
    }
}

fn json_rpc_error(id: Value, code: i64, message: &str) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Invoker that echoes the request's method back as the result, or fails for a
    /// method named "boom".
    struct EchoInvoker;

    #[async_trait::async_trait]
    impl AcpReverseInvoker for EchoInvoker {
        async fn invoke(
            &self,
            _server_id: &str,
            message: Value,
            _timeout: Duration,
        ) -> Result<Value, String> {
            let method = message.get("method").cloned().unwrap_or(Value::Null);
            if method == "boom" {
                return Err("server exploded".to_string());
            }
            let id = message.get("id").cloned().unwrap_or(Value::Null);
            Ok(serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": { "method": method } }))
        }
    }

    /// Drive the pump directly (no rmcp): write client→server lines, read back the
    /// server→client lines.
    fn spawn_pump() -> (DuplexStream, DuplexStream) {
        let (test_write, pump_read) = tokio::io::duplex(BRIDGE_BUF);
        let (pump_write, test_read) = tokio::io::duplex(BRIDGE_BUF);
        tokio::spawn(pump(
            "srv".to_string(),
            Arc::new(EchoInvoker),
            Duration::from_secs(60),
            pump_read,
            pump_write,
        ));
        (test_write, test_read)
    }

    async fn read_line(reader: &mut BufReader<DuplexStream>) -> Value {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        serde_json::from_str(line.trim()).unwrap()
    }

    #[tokio::test]
    async fn request_gets_a_response_notification_does_not() {
        let (mut to_server, from_server) = spawn_pump();
        let mut reader = BufReader::new(from_server);

        // A notification (no id) must NOT produce a response line...
        to_server
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
            .await
            .unwrap();
        // ...so the first line we read back is the request's response (id 1), proving
        // the notification was silently consumed.
        to_server
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n")
            .await
            .unwrap();

        let response = read_line(&mut reader).await;
        assert_eq!(response["id"], 1);
        assert_eq!(response["result"]["method"], "tools/list");
    }

    /// A slow request must not block a later fast one: the fast response comes back
    /// first even though its request was written second (head-of-line free).
    #[tokio::test]
    async fn a_slow_request_does_not_block_a_later_fast_one() {
        struct DelayInvoker;
        #[async_trait::async_trait]
        impl AcpReverseInvoker for DelayInvoker {
            async fn invoke(
                &self,
                _server_id: &str,
                message: Value,
                _timeout: Duration,
            ) -> Result<Value, String> {
                let id = message.get("id").cloned().unwrap_or(Value::Null);
                // id 1 is slow, id 2 is fast.
                if id == 1 {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                Ok(serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": {} }))
            }
        }

        let (test_write, pump_read) = tokio::io::duplex(BRIDGE_BUF);
        let (pump_write, test_read) = tokio::io::duplex(BRIDGE_BUF);
        tokio::spawn(pump(
            "srv".to_string(),
            Arc::new(DelayInvoker),
            Duration::from_secs(60),
            pump_read,
            pump_write,
        ));
        let mut to_server = test_write;
        let mut reader = BufReader::new(test_read);

        to_server
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"slow\"}\n")
            .await
            .unwrap();
        to_server
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"fast\"}\n")
            .await
            .unwrap();

        // The fast request (id 2) returns before the slow one (id 1).
        assert_eq!(read_line(&mut reader).await["id"], 2);
        assert_eq!(read_line(&mut reader).await["id"], 1);
    }

    /// Regression: a chunked request (JSON args exceed the read buffer) must still parse
    /// when an in-flight invoke completes mid-read. The pre-fix `select!` reaped the invoke
    /// and cleared the partially-read line, desyncing the stream; the cancellation-safe read
    /// does not.
    #[tokio::test]
    async fn chunked_request_survives_an_invoke_completing_mid_read() {
        /// id 1 completes after a short delay; everything else returns immediately.
        struct DelayInvoker;
        #[async_trait::async_trait]
        impl AcpReverseInvoker for DelayInvoker {
            async fn invoke(
                &self,
                _server_id: &str,
                message: Value,
                _timeout: Duration,
            ) -> Result<Value, String> {
                let id = message.get("id").cloned().unwrap_or(Value::Null);
                if id == 1 {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Ok(serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": {} }))
            }
        }

        let (test_write, pump_read) = tokio::io::duplex(BRIDGE_BUF);
        let (pump_write, test_read) = tokio::io::duplex(BRIDGE_BUF);
        tokio::spawn(pump(
            "srv".to_string(),
            Arc::new(DelayInvoker),
            Duration::from_secs(60),
            pump_read,
            pump_write,
        ));
        let mut to_server = test_write;
        let mut reader = BufReader::new(test_read);

        // In-flight invoke (id 1): its task will finish ~50ms from now.
        to_server
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"slow\"}\n")
            .await
            .unwrap();

        // Begin a second request (id 2) but withhold its closing brace + newline, so
        // the reader blocks mid-message while id 1's invoke completes.
        to_server
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,")
            .await
            .unwrap();
        to_server.flush().await.unwrap();
        // Let id 1's invoke complete *during* the pending chunked read.
        tokio::time::sleep(Duration::from_millis(120)).await;
        to_server
            .write_all(b"\"method\":\"chunked\"}\n")
            .await
            .unwrap();
        to_server.flush().await.unwrap();

        // Both responses must arrive (order may vary). Critically, id 2 parsed — no
        // desync from the mid-read completion of id 1.
        let first = read_line(&mut reader).await;
        let second = read_line(&mut reader).await;
        let mut ids = [
            first["id"].as_i64().unwrap(),
            second["id"].as_i64().unwrap(),
        ];
        ids.sort_unstable();
        assert_eq!(ids, [1, 2]);
    }

    #[tokio::test]
    async fn invoker_error_becomes_a_json_rpc_error_keyed_to_the_request_id() {
        let (mut to_server, from_server) = spawn_pump();
        let mut reader = BufReader::new(from_server);

        to_server
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"boom\"}\n")
            .await
            .unwrap();

        let response = read_line(&mut reader).await;
        assert_eq!(response["id"], 7);
        assert_eq!(response["error"]["code"], -32603);
        assert_eq!(response["error"]["message"], "server exploded");
    }

    /// A non-object SDK response can't carry an `id`, so rmcp couldn't correlate it.
    /// The bridge synthesizes an id-keyed JSON-RPC error so the waiting request fails
    /// fast instead of timing out.
    #[tokio::test]
    async fn non_object_response_becomes_a_json_rpc_error_keyed_to_the_request_id() {
        /// Returns a JSON array (not an object) as its "response".
        struct NonObjectInvoker;
        #[async_trait::async_trait]
        impl AcpReverseInvoker for NonObjectInvoker {
            async fn invoke(
                &self,
                _server_id: &str,
                _message: Value,
                _timeout: Duration,
            ) -> Result<Value, String> {
                Ok(serde_json::json!([1, 2, 3]))
            }
        }

        let (test_write, pump_read) = tokio::io::duplex(BRIDGE_BUF);
        let (pump_write, test_read) = tokio::io::duplex(BRIDGE_BUF);
        tokio::spawn(pump(
            "srv".to_string(),
            Arc::new(NonObjectInvoker),
            Duration::from_secs(60),
            pump_read,
            pump_write,
        ));
        let mut to_server = test_write;
        let mut reader = BufReader::new(test_read);

        to_server
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"tools/list\"}\n")
            .await
            .unwrap();

        let response = read_line(&mut reader).await;
        assert_eq!(response["id"], 9);
        assert_eq!(response["error"]["code"], -32603);
    }

    /// The configured per-server timeout (not a hardcoded constant) must reach the
    /// invoker for every reverse call, so the zero-IPC path can't silently shrink a
    /// long tool's budget.
    #[tokio::test]
    async fn pump_forwards_the_configured_timeout_to_the_invoker() {
        use std::sync::Mutex;

        /// Records the timeout it was invoked with so the test can assert on it.
        struct RecordingInvoker(Arc<Mutex<Option<Duration>>>);
        #[async_trait::async_trait]
        impl AcpReverseInvoker for RecordingInvoker {
            async fn invoke(
                &self,
                _server_id: &str,
                message: Value,
                timeout: Duration,
            ) -> Result<Value, String> {
                *self.0.lock().unwrap() = Some(timeout);
                let id = message.get("id").cloned().unwrap_or(Value::Null);
                Ok(serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": {} }))
            }
        }

        let seen = Arc::new(Mutex::new(None));
        let configured = Duration::from_secs(4242);
        let (test_write, pump_read) = tokio::io::duplex(BRIDGE_BUF);
        let (pump_write, test_read) = tokio::io::duplex(BRIDGE_BUF);
        tokio::spawn(pump(
            "srv".to_string(),
            Arc::new(RecordingInvoker(seen.clone())),
            configured,
            pump_read,
            pump_write,
        ));
        let mut to_server = test_write;
        let mut reader = BufReader::new(test_read);

        to_server
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n")
            .await
            .unwrap();
        // Wait for the response so the invoke has definitely run.
        assert_eq!(read_line(&mut reader).await["id"], 1);
        assert_eq!(*seen.lock().unwrap(), Some(configured));
    }

    /// Teardown (rmcp dropping the duplex) must ABORT an in-flight invoke promptly,
    /// not wait out its timeout. We send a request whose invoke sleeps far longer than
    /// the test budget, tear the transport down, and assert the pump self-terminates
    /// quickly while the invoke neither completes nor lingers.
    #[tokio::test]
    async fn teardown_aborts_in_flight_invokes() {
        use std::sync::atomic::{AtomicBool, Ordering};

        /// Flips a flag when the invoke future is dropped (i.e. aborted).
        struct DropFlag(Arc<AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        struct SlowInvoker {
            started: Arc<AtomicBool>,
            completed: Arc<AtomicBool>,
            dropped: Arc<AtomicBool>,
        }
        #[async_trait::async_trait]
        impl AcpReverseInvoker for SlowInvoker {
            async fn invoke(
                &self,
                _server_id: &str,
                _message: Value,
                _timeout: Duration,
            ) -> Result<Value, String> {
                let _drop_flag = DropFlag(self.dropped.clone());
                self.started.store(true, Ordering::SeqCst);
                // Far longer than the per-call timeout AND the test's wait budget, so a
                // "completed" or "timed out" outcome can only mean it wasn't aborted.
                tokio::time::sleep(Duration::from_secs(3600)).await;
                self.completed.store(true, Ordering::SeqCst);
                Ok(Value::Null)
            }
        }

        let started = Arc::new(AtomicBool::new(false));
        let completed = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicBool::new(false));

        let (test_write, pump_read) = tokio::io::duplex(BRIDGE_BUF);
        let (pump_write, test_read) = tokio::io::duplex(BRIDGE_BUF);
        let pump_handle = tokio::spawn(pump(
            "srv".to_string(),
            Arc::new(SlowInvoker {
                started: started.clone(),
                completed: completed.clone(),
                dropped: dropped.clone(),
            }),
            Duration::from_secs(600),
            pump_read,
            pump_write,
        ));

        let mut to_server = test_write;
        to_server
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"slow\"}\n")
            .await
            .unwrap();

        // Wait until the invoke has actually started before tearing down.
        for _ in 0..200 {
            if started.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(started.load(Ordering::SeqCst), "invoke should have started");

        // Tear down: dropping both client ends closes the duplex, exactly as rmcp does
        // on shutdown.
        drop(to_server);
        drop(test_read);

        // The pump must self-terminate well within the (600s) invoke timeout, proving
        // the in-flight invoke was aborted rather than awaited.
        tokio::time::timeout(Duration::from_secs(5), pump_handle)
            .await
            .expect("pump should self-terminate promptly after teardown")
            .unwrap();

        // The aborted invoke future must be dropped (abort is async, so poll briefly)
        // and must never have run to completion.
        for _ in 0..200 {
            if dropped.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            dropped.load(Ordering::SeqCst),
            "aborted invoke future should be dropped"
        );
        assert!(
            !completed.load(Ordering::SeqCst),
            "aborted invoke must not run to completion"
        );
    }

    /// A mock SDK MCP **server** behind the reverse channel. It speaks just enough
    /// real MCP to satisfy an rmcp client: the `initialize` handshake, a `tools/list`
    /// advertising one `echo` tool, and a `tools/call` that echoes its text argument.
    /// Each `invoke` receives one JSON-RPC request and returns one JSON-RPC response
    /// (the bridge overwrites the `id`), mirroring the real on-wire shapes.
    struct MockSdkServer;

    #[async_trait::async_trait]
    impl AcpReverseInvoker for MockSdkServer {
        async fn invoke(
            &self,
            _server_id: &str,
            message: Value,
            _timeout: Duration,
        ) -> Result<Value, String> {
            let id = message.get("id").cloned().unwrap_or(Value::Null);
            let method = message
                .get("method")
                .and_then(|m| m.as_str())
                .unwrap_or_default();
            let result = match method {
                "initialize" => serde_json::json!({
                    // Echo the client's protocol version so the handshake is always compatible.
                    "protocolVersion": message["params"]["protocolVersion"],
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "mock-sdk-server", "version": "0.0.0" },
                }),
                "tools/list" => serde_json::json!({
                    "tools": [{
                        "name": "echo",
                        "description": "Echoes its text argument back.",
                        "inputSchema": {
                            "type": "object",
                            "properties": { "text": { "type": "string" } },
                            "required": ["text"],
                        },
                    }],
                }),
                "tools/call" => {
                    let text = message["params"]["arguments"]["text"]
                        .as_str()
                        .unwrap_or_default();
                    serde_json::json!({
                        "content": [{ "type": "text", "text": text }],
                        "isError": false,
                    })
                }
                other => return Err(format!("mock SDK server: unexpected method {other}")),
            };
            Ok(serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }))
        }
    }

    /// End-to-end: drive a REAL `rmcp` client (`RunningService<RoleClient, _>`) through
    /// `acp_bridge_transport` against [`MockSdkServer`], proving the bridge speaks real
    /// MCP — the full `initialize` handshake (including rmcp's id-less
    /// `notifications/initialized`, which the bridge discards), `tools/list`, and
    /// `tools/call` — then a clean cancel/teardown. This is the same client path
    /// production uses in `servers.rs` (`client.serve(transport)`).
    #[tokio::test]
    async fn real_rmcp_client_handshakes_lists_and_calls_over_the_bridge() {
        use rmcp::ServiceExt;
        use rmcp::model::{CallToolRequestParams, PaginatedRequestParams};

        let transport = acp_bridge_transport(
            "srv".to_string(),
            Arc::new(MockSdkServer),
            Duration::from_secs(60),
        );

        // `()` is rmcp's minimal `ClientHandler`; `serve` runs the real initialize
        // handshake over our bridge transport and yields a live `RunningService`.
        let client =
            ().serve(transport)
                .await
                .expect("rmcp handshake over the bridge should succeed");

        let tools = client
            .list_tools(Some(PaginatedRequestParams::default()))
            .await
            .expect("tools/list over the bridge");
        assert_eq!(tools.tools.len(), 1);
        assert_eq!(tools.tools[0].name.as_ref(), "echo");

        let result = client
            .call_tool(
                CallToolRequestParams::new("echo").with_arguments(
                    serde_json::json!({ "text": "hello bridge" })
                        .as_object()
                        .cloned()
                        .expect("arguments object"),
                ),
            )
            .await
            .expect("tools/call over the bridge");
        let text = result.content[0]
            .as_text()
            .expect("text content")
            .text
            .clone();
        assert_eq!(text, "hello bridge");

        // Clean teardown: cancelling drops rmcp's duplex end; the pump observes EOF and
        // self-terminates (the abort mechanics are covered by
        // `teardown_aborts_in_flight_invokes`).
        client.cancel().await.expect("clean teardown");
    }
}
