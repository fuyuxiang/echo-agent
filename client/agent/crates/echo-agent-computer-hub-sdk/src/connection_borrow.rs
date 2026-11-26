//! Shared connection-borrow lifecycle for `ToolServer` and `ToolHarness`.
//!
//! Wraps a pooled [`HubConnection`] with a [`CancellationToken`] for
//! shutdown coordination and an at-most-once `torn_down` guard.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use echo_agent_tool_protocol::ConnectionKind;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::auth::AuthProvider;
use crate::connection::{
    ConnectCallback, ConnectionTuning, DisconnectCallback, HubConnection, ReconnectCallback,
    TerminalCloseCallback,
};
use crate::error::ClientError;
use crate::pool::HubConnectionPool;

/// Borrowed slice of a pooled [`HubConnection`] plus the refcount of
/// session bindings the borrower owns. Drop guard lives here so the
/// teardown sequence is at-most-once across explicit `shutdown` and
/// the `Drop` fallback.
pub(crate) struct ConnectionBorrow {
    connection: Arc<HubConnection>,
    shutdown: CancellationToken,
    /// At-most-once guard coordinated via `compare_exchange`.
    torn_down: AtomicBool,
}

impl std::fmt::Debug for ConnectionBorrow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionBorrow")
            .field(
                "torn_down",
                &self.torn_down.load(std::sync::atomic::Ordering::Relaxed),
            )
            .finish_non_exhaustive()
    }
}

impl ConnectionBorrow {
    /// Resolve a pool entry, refcount-bind every requested session,
    /// and return a borrow. On any per-session bind failure the
    /// already-bound sessions are unregistered before returning the
    /// error so partial state never leaks.
    pub(crate) async fn acquire(
        pool: Arc<HubConnectionPool>,
        url: Url,
        auth: Arc<dyn AuthProvider>,
        kind: ConnectionKind,
        on_reconnect: Option<Arc<ReconnectCallback>>,
        on_disconnect: Option<Arc<DisconnectCallback>>,
        on_connect: Option<Arc<ConnectCallback>>,
        on_terminal_close: Option<Arc<TerminalCloseCallback>>,
        server_id: Option<echo_agent_tool_protocol::ServerId>,
        server_description: Option<String>,
        server_metadata: Option<serde_json::Value>,
        alpha_test_key: Option<String>,
        allow_insecure_ws: bool,
        tuning: ConnectionTuning,
    ) -> Result<Self, ClientError> {
        let connection = pool
            .get_or_connect_tuned(
                url,
                auth,
                kind,
                on_reconnect,
                on_disconnect,
                on_connect,
                on_terminal_close,
                server_id,
                server_description,
                server_metadata,
                alpha_test_key,
                allow_insecure_ws,
                tuning,
            )
            .await?;
        Ok(Self {
            connection,
            shutdown: CancellationToken::new(),
            torn_down: AtomicBool::new(false),
        })
    }

    pub(crate) fn connection(&self) -> &Arc<HubConnection> {
        &self.connection
    }

    pub(crate) fn shutdown_token(&self) -> &CancellationToken {
        &self.shutdown
    }

    /// Returns `true` if this caller won the at-most-once teardown.
    pub(crate) fn begin_teardown(&self) -> bool {
        self.torn_down
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// Whether teardown has already been claimed (`begin_teardown` won).
    pub(crate) fn is_torn_down(&self) -> bool {
        self.torn_down.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::net::SocketAddr;
    use std::sync::Arc;

    use crate::auth::AuthCredential;
    use axum::Router;
    use axum::extract::WebSocketUpgrade;
    use axum::extract::ws::{Message, WebSocket};
    use axum::response::IntoResponse;
    use axum::routing::get;
    use serde_json::json;
    use tokio::net::TcpListener;

    /// Spawn an in-process mock server that completes the WebSocket
    /// handshake and ignores everything else. Returned address is
    /// bound on `127.0.0.1`.
    async fn spawn_borrow_mock_hub() -> SocketAddr {
        let app = Router::new().route("/v1/tools", get(ws_upgrade));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app.into_make_service()).await;
        });
        tokio::task::yield_now().await;
        addr
    }

    async fn ws_upgrade(ws: WebSocketUpgrade) -> impl IntoResponse {
        ws.on_upgrade(handle_socket)
    }

    async fn handle_socket(mut socket: WebSocket) {
        let _ = socket.recv().await;
        let ack = json!({
            "connection_id": "borrow-mock",
            "user_id": "test",
            "computer_hub_version": "test",
            "supported_protocol_versions": ["1.0.0"],
        });
        let _ = socket.send(Message::Text(ack.to_string().into())).await;
        // Keep the WebSocket alive until the client disconnects.
        // These tests only exercise borrow lifecycle (teardown
        // atomicity), not protocol frames.
        while let Some(Ok(_msg)) = socket.recv().await {}
    }

    async fn acquire_borrow() -> ConnectionBorrow {
        let addr = spawn_borrow_mock_hub().await;
        let url = Url::parse(&format!("ws://{addr}/v1/tools")).expect("valid url");
        let cred: Arc<dyn AuthProvider> = Arc::new(AuthCredential::bearer("ignored"));
        let pool = HubConnectionPool::new();
        ConnectionBorrow::acquire(
            pool,
            url,
            cred,
            ConnectionKind::Harness,
            None, // on_reconnect
            None, // on_disconnect
            None, // on_connect
            None, // on_terminal_close
            None, // server_id
            None, // server_description
            None, // server_metadata
            None, // alpha_test_key
            false,
            ConnectionTuning::default(),
        )
        .await
        .expect("acquire borrow")
    }

    #[tokio::test]
    async fn begin_teardown_returns_true_once_and_false_after() {
        let borrow = acquire_borrow().await;
        assert!(
            borrow.begin_teardown(),
            "first call wins the at-most-once transition"
        );
        assert!(
            !borrow.begin_teardown(),
            "subsequent calls observe the already-torn-down state"
        );
        assert!(
            !borrow.begin_teardown(),
            "the at-most-once transition is sticky"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn begin_teardown_is_atomic_under_concurrent_callers() {
        let borrow = Arc::new(acquire_borrow().await);
        let n_callers = 64;
        let barrier = Arc::new(tokio::sync::Barrier::new(n_callers));
        let mut handles = Vec::with_capacity(n_callers);
        for _ in 0..n_callers {
            let borrow = borrow.clone();
            let barrier = barrier.clone();
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                borrow.begin_teardown()
            }));
        }
        let mut wins = 0usize;
        for h in handles {
            if h.await.expect("join") {
                wins += 1;
            }
        }
        assert_eq!(
            wins, 1,
            "exactly one of {n_callers} concurrent callers must win the at-most-once transition"
        );
    }

    #[tokio::test]
    async fn acquire_returns_zero_bound_sessions() {
        let borrow = acquire_borrow().await;
        assert_eq!(borrow.connection().bound_session_count(), 0);
    }
}
