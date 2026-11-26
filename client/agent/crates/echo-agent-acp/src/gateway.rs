use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use agent_client_protocol as acp;
use serde::Serialize;
use tokio::sync::{mpsc, oneshot};
use tracing::Instrument;

use crate::{
    AcpMethod, acp_send,
    common::AcpResult,
    message::{AcpAgentMessage, AcpArgs, AcpClientMessage, AcpRequest, AcpSide},
};

type SpawnFn = Rc<dyn Fn(Pin<Box<dyn Future<Output = ()>>>)>;
/// Callback that creates a `tracing::Span` from `_meta` for distributed tracing.
type OnMetaFn = Rc<dyn Fn(&acp::Meta) -> tracing::Span>;

/// Gateway receiver - allows sending messages to it via a channel and it will
/// forward them to an underlying connection.
pub struct AcpGatewayReceiver<S: AcpSide, C> {
    rx: mpsc::UnboundedReceiver<S::OutMessage>,
    conn: C,
    tracing: bool,
    spawn_fn: SpawnFn,
    on_meta: Option<OnMetaFn>,
}

impl<S: AcpSide, C> AcpGatewayReceiver<S, C> {
    pub fn new(rx: mpsc::UnboundedReceiver<S::OutMessage>, conn: C) -> Self {
        Self {
            rx,
            conn,
            tracing: false,
            spawn_fn: Rc::new(|fut| {
                tokio::task::spawn_local(fut);
            }),
            on_meta: None,
        }
    }

    pub fn with_tracing(mut self, tracing: bool) -> Self {
        self.tracing = tracing;
        self
    }

    /// Override the spawner used for dispatching incoming messages.
    ///
    /// By default, `spawn_local` is used (suitable for `LocalSet` runtimes).
    /// Pass a custom spawner to use a different execution strategy.
    pub fn with_spawn_fn(
        mut self,
        f: impl Fn(Pin<Box<dyn Future<Output = ()>>>) + 'static,
    ) -> Self {
        self.spawn_fn = Rc::new(f);
        self
    }

    /// Hook that builds a `tracing::Span` from `_meta` to `.instrument()` dispatched messages.
    pub fn with_on_meta(mut self, f: impl Fn(&acp::Meta) -> tracing::Span + 'static) -> Self {
        self.on_meta = Some(Rc::new(f));
        self
    }
}

/// The other side of the gateway. Allows to send messages to a channel so that
/// they will be forwarded automatically to a connection (as long as gateway
/// receiver side is running in the background).
pub struct AcpGatewaySender<S: AcpSide> {
    tx: mpsc::UnboundedSender<S::OutMessage>,
    tracing: bool,
}

impl<S: AcpSide> Clone for AcpGatewaySender<S> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            tracing: self.tracing,
        }
    }
}

impl<S: AcpSide> AcpGatewaySender<S> {
    pub fn new(tx: mpsc::UnboundedSender<S::OutMessage>) -> Self {
        Self { tx, tracing: false }
    }

    pub fn tx(&self) -> mpsc::UnboundedSender<S::OutMessage> {
        self.tx.clone()
    }

    pub fn with_tracing(mut self, tracing: bool) -> Self {
        self.tracing = tracing;
        self
    }
}

pub fn acp_gateway<S: AcpSide, C>(conn: C) -> (AcpGatewaySender<S>, AcpGatewayReceiver<S, C>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let sender = AcpGatewaySender::new(tx);
    let receiver = AcpGatewayReceiver::new(rx, conn);
    (sender, receiver)
}

pub type AcpAgentGatewayReceiver = AcpGatewayReceiver<acp::AgentSide, acp::AgentSideConnection>;
pub type AcpAgentGatewaySender = AcpGatewaySender<acp::AgentSide>;
pub type AcpClientGatewayReceiver = AcpGatewayReceiver<acp::ClientSide, acp::ClientSideConnection>;
pub type AcpClientGatewaySender = AcpGatewaySender<acp::ClientSide>;

fn before_request<T: AcpRequest>(args: &AcpArgs<T>, tracing: bool) -> Option<String> {
    tracing.then(|| {
        let method = crate::common::compact_json(&args.method_name());
        tracing::debug!(
            "sending {method} request: {}",
            crate::common::compact_json(&args.request)
        );
        method
    })
}

fn after_request<T: Serialize>(
    response_tx: oneshot::Sender<AcpResult<T>>,
    response: AcpResult<T>,
    method: Option<String>,
) -> bool {
    if let Some(method) = method {
        match response {
            Ok(ref response) => {
                tracing::debug!(
                    "received {method} response: {}",
                    crate::common::compact_json(&response)
                );
            }
            Err(ref err) => {
                // Log at debug level - errors are handled visually in the TUI status bar
                tracing::debug!("received {method} error: {err}");
            }
        }
    }
    response_tx.send(response).is_ok()
}

macro_rules! handle {
    ($args:expr, $tracing:expr, $conn:expr, $name:ident, $spawn:expr, $on_meta:expr $(,)?) => {{
        let span = ($on_meta)
            .as_ref()
            .zip(($args).request.meta.as_ref())
            .map(|(f, meta)| f(meta))
            .unwrap_or_else(tracing::Span::none);
        ($spawn)(Box::pin(
            async move {
                let method = before_request(&($args), $tracing);
                let response = ($conn).$name(($args).request).await;
                let _ = after_request(($args).response_tx, response, method);
            }
            .instrument(span),
        ));
    }};
    // Variant for types without `meta` field (ExtRequest, ExtNotification).
    // $on_meta is accepted (but unused) to disambiguate from the primary pattern.
    (no_meta, $args:expr, $tracing:expr, $conn:expr, $name:ident, $spawn:expr, $on_meta:expr $(,)?) => {
        ($spawn)(Box::pin(async move {
            let method = before_request(&($args), $tracing);
            let response = ($conn).$name(($args).request).await;
            let _ = after_request(($args).response_tx, response, method);
        }));
    };
}

impl<C: acp::Agent + 'static> AcpGatewayReceiver<acp::ClientSide, C> {
    pub async fn run(mut self) {
        let conn = Rc::new(self.conn);
        let spawn = self.spawn_fn.clone();
        let on_meta = self.on_meta.clone();
        while let Some(msg) = self.rx.recv().await {
            let conn = conn.clone();
            match msg {
                AcpAgentMessage::Initialize(args) => {
                    handle!(args, self.tracing, conn, initialize, spawn, on_meta);
                }
                AcpAgentMessage::Authenticate(args) => {
                    handle!(args, self.tracing, conn, authenticate, spawn, on_meta);
                }
                AcpAgentMessage::NewSession(args) => {
                    handle!(args, self.tracing, conn, new_session, spawn, on_meta);
                }
                AcpAgentMessage::LoadSession(args) => {
                    handle!(args, self.tracing, conn, load_session, spawn, on_meta);
                }
                AcpAgentMessage::SetSessionMode(args) => {
                    handle!(args, self.tracing, conn, set_session_mode, spawn, on_meta);
                }
                AcpAgentMessage::Prompt(args) => {
                    handle!(args, self.tracing, conn, prompt, spawn, on_meta);
                }
                AcpAgentMessage::Cancel(args) => {
                    handle!(args, self.tracing, conn, cancel, spawn, on_meta);
                }
                AcpAgentMessage::ExtMethod(args) => {
                    handle!(
                        no_meta,
                        args,
                        self.tracing,
                        conn,
                        ext_method,
                        spawn,
                        on_meta
                    );
                }
                AcpAgentMessage::ExtNotification(args) => {
                    handle!(
                        no_meta,
                        args,
                        self.tracing,
                        conn,
                        ext_notification,
                        spawn,
                        on_meta
                    );
                }
                AcpAgentMessage::SetSessionModel(args) => {
                    handle!(args, self.tracing, conn, set_session_model, spawn, on_meta);
                }
            }
        }
        if self.tracing {
            tracing::trace!("stopping gateway loop: receiver channel is closed");
        }
    }
}

impl<C: acp::Client + 'static> AcpGatewayReceiver<acp::AgentSide, C> {
    pub async fn run(mut self) {
        let conn = Rc::new(self.conn);
        let spawn = self.spawn_fn.clone();
        let on_meta = self.on_meta.clone();
        while let Some(msg) = self.rx.recv().await {
            let conn = conn.clone();
            match msg {
                AcpClientMessage::RequestPermission(args) => {
                    handle!(args, self.tracing, conn, request_permission, spawn, on_meta);
                }
                AcpClientMessage::ReadTextFile(args) => {
                    handle!(args, self.tracing, conn, read_text_file, spawn, on_meta);
                }
                AcpClientMessage::WriteTextFile(args) => {
                    handle!(args, self.tracing, conn, write_text_file, spawn, on_meta);
                }
                AcpClientMessage::SessionNotification(args) => {
                    handle!(
                        args,
                        self.tracing,
                        conn,
                        session_notification,
                        spawn,
                        on_meta
                    );
                }
                AcpClientMessage::CreateTerminal(args) => {
                    handle!(args, self.tracing, conn, create_terminal, spawn, on_meta);
                }
                AcpClientMessage::TerminalOutput(args) => {
                    handle!(args, self.tracing, conn, terminal_output, spawn, on_meta);
                }
                AcpClientMessage::ReleaseTerminal(args) => {
                    handle!(args, self.tracing, conn, release_terminal, spawn, on_meta);
                }
                AcpClientMessage::WaitForTerminalExit(args) => {
                    handle!(
                        args,
                        self.tracing,
                        conn,
                        wait_for_terminal_exit,
                        spawn,
                        on_meta
                    );
                }
                AcpClientMessage::KillTerminalCommand(args) => {
                    handle!(args, self.tracing, conn, kill_terminal, spawn, on_meta);
                }
                AcpClientMessage::ExtMethod(args) => {
                    handle!(
                        no_meta,
                        args,
                        self.tracing,
                        conn,
                        ext_method,
                        spawn,
                        on_meta
                    );
                }
                AcpClientMessage::ExtNotification(args) => {
                    handle!(
                        no_meta,
                        args,
                        self.tracing,
                        conn,
                        ext_notification,
                        spawn,
                        on_meta
                    );
                }
            }
        }
        if self.tracing {
            tracing::trace!("stopping gateway loop: receiver channel is closed");
        }
    }
}

impl<S: AcpSide> AcpGatewaySender<S> {
    /// Shared enqueue for the forward variants; `caller` attributes the
    /// dropped-receiver log to the right public method.
    fn enqueue<T>(
        &self,
        request: T,
        caller: &'static str,
    ) -> (bool, oneshot::Receiver<AcpResult<T::Response>>)
    where
        T: AcpRequest,
        S::OutMessage: From<AcpArgs<T>>,
    {
        let (response_tx, response_rx) = oneshot::channel();
        let method = request.method_name();
        let args = AcpArgs {
            request,
            response_tx,
        };
        let accepted = self.tx.send(args.into()).is_ok();
        if !accepted {
            tracing::debug!(method, "{caller}: receiver dropped, notification discarded");
        }
        (accepted, response_rx)
    }

    /// Enqueue a request and return a completion receiver for handler finish.
    pub fn forward_with_completion<T>(
        &self,
        request: T,
    ) -> oneshot::Receiver<AcpResult<T::Response>>
    where
        T: AcpRequest,
        S::OutMessage: From<AcpArgs<T>>,
    {
        self.enqueue(request, "forward_with_completion").1
    }

    /// Enqueue a request without waiting for the response. Returns whether
    /// the gateway channel accepted it (`false`: receiver gone, message
    /// discarded) so callers keeping delivery-dependent state can retry.
    pub fn forward_fire_and_forget<T>(&self, request: T) -> bool
    where
        T: AcpRequest,
        S::OutMessage: From<AcpArgs<T>>,
    {
        self.enqueue(request, "forward_fire_and_forget").0
    }

    /// Send a request and await the response. Returns a `Send` future.
    ///
    /// Equivalent to the `acp::Client` / `acp::Agent` trait methods but the
    /// returned future is `Send` because this is an inherent async fn — not
    /// wrapped by `#[async_trait(?Send)]`.
    pub async fn send<T>(&self, request: T) -> AcpResult<T::Response>
    where
        T: AcpRequest,
        S::OutMessage: From<AcpArgs<T>>,
    {
        self.forward(request).await
    }

    async fn forward<T>(&self, request: T) -> AcpResult<T::Response>
    where
        T: AcpRequest,
        S::OutMessage: From<AcpArgs<T>>,
    {
        if self.tracing {
            let method = crate::common::compact_json(&request.method_name());
            tracing::debug!(
                "received {method} request: {}",
                crate::common::compact_json(&request)
            );
        }
        acp_send(request, &self.tx).await
    }
}

#[async_trait::async_trait(?Send)]
impl acp::Client for AcpGatewaySender<acp::AgentSide> {
    async fn request_permission(
        &self,
        args: acp::RequestPermissionRequest,
    ) -> AcpResult<acp::RequestPermissionResponse> {
        self.forward(args).await
    }

    async fn write_text_file(
        &self,
        args: acp::WriteTextFileRequest,
    ) -> AcpResult<acp::WriteTextFileResponse> {
        self.forward(args).await
    }

    async fn read_text_file(
        &self,
        args: acp::ReadTextFileRequest,
    ) -> AcpResult<acp::ReadTextFileResponse> {
        self.forward(args).await
    }

    async fn create_terminal(
        &self,
        args: acp::CreateTerminalRequest,
    ) -> AcpResult<acp::CreateTerminalResponse> {
        self.forward(args).await
    }

    async fn terminal_output(
        &self,
        args: acp::TerminalOutputRequest,
    ) -> AcpResult<acp::TerminalOutputResponse> {
        self.forward(args).await
    }

    async fn release_terminal(
        &self,
        args: acp::ReleaseTerminalRequest,
    ) -> AcpResult<acp::ReleaseTerminalResponse> {
        self.forward(args).await
    }

    async fn wait_for_terminal_exit(
        &self,
        args: acp::WaitForTerminalExitRequest,
    ) -> AcpResult<acp::WaitForTerminalExitResponse> {
        self.forward(args).await
    }

    async fn kill_terminal(
        &self,
        args: acp::KillTerminalRequest,
    ) -> AcpResult<acp::KillTerminalResponse> {
        self.forward(args).await
    }

    async fn session_notification(&self, args: acp::SessionNotification) -> AcpResult<()> {
        // Fire-and-forget: session notifications carry no meaningful response (the
        // ACK is `()`), so we must not block the caller waiting for the client to
        // acknowledge.  When the agent→relay→client path is degraded (e.g. a Slack
        // session whose ephemeral WebSocket died mid-turn), the relay write can
        // stall for minutes (TCP retransmit timeout).  Blocking here freezes the
        // terminal streaming loop — its timeout check never fires, the session
        // actor can't process new prompts, and the entire session hangs.
        self.forward_fire_and_forget(args);
        Ok(())
    }

    async fn ext_method(&self, args: acp::ExtRequest) -> AcpResult<acp::ExtResponse> {
        self.forward(args).await
    }

    async fn ext_notification(&self, args: acp::ExtNotification) -> AcpResult<()> {
        // Fire-and-forget for the same reason as `session_notification` above:
        // the ACK is `()` and blocking risks hanging the caller when the
        // relay→client path is degraded.  Many call sites already bypass this
        // trait method and call `forward_fire_and_forget` directly.
        self.forward_fire_and_forget(args);
        Ok(())
    }
}

#[async_trait::async_trait(?Send)]
impl acp::Agent for AcpGatewaySender<acp::ClientSide> {
    async fn initialize(&self, args: acp::InitializeRequest) -> AcpResult<acp::InitializeResponse> {
        self.forward(args).await
    }

    async fn authenticate(
        &self,
        args: acp::AuthenticateRequest,
    ) -> AcpResult<acp::AuthenticateResponse> {
        self.forward(args).await
    }

    async fn new_session(
        &self,
        args: acp::NewSessionRequest,
    ) -> AcpResult<acp::NewSessionResponse> {
        self.forward(args).await
    }

    async fn load_session(
        &self,
        args: acp::LoadSessionRequest,
    ) -> AcpResult<acp::LoadSessionResponse> {
        self.forward(args).await
    }

    async fn set_session_mode(
        &self,
        args: acp::SetSessionModeRequest,
    ) -> AcpResult<acp::SetSessionModeResponse> {
        self.forward(args).await
    }

    async fn prompt(&self, args: acp::PromptRequest) -> AcpResult<acp::PromptResponse> {
        self.forward(args).await
    }

    async fn cancel(&self, args: acp::CancelNotification) -> AcpResult<()> {
        self.forward(args).await
    }

    async fn ext_method(&self, args: acp::ExtRequest) -> AcpResult<acp::ExtResponse> {
        self.forward(args).await
    }

    async fn ext_notification(&self, args: acp::ExtNotification) -> AcpResult<()> {
        self.forward(args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    use agent_client_protocol as acp;

    struct OrderTrackingClient {
        log: Rc<RefCell<Vec<String>>>,
    }

    #[async_trait::async_trait(?Send)]
    impl acp::Client for OrderTrackingClient {
        async fn request_permission(
            &self,
            _: acp::RequestPermissionRequest,
        ) -> acp::Result<acp::RequestPermissionResponse> {
            unimplemented!()
        }
        async fn session_notification(&self, args: acp::SessionNotification) -> acp::Result<()> {
            if let acp::SessionUpdate::AgentMessageChunk(chunk) = &args.update
                && let acp::ContentBlock::Text(text) = &chunk.content
            {
                self.log.borrow_mut().push(text.text.clone());
            }
            Ok(())
        }
    }

    fn text_notification(marker: &str) -> acp::SessionNotification {
        acp::SessionNotification::new(
            acp::SessionId::new("s"),
            acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
                acp::TextContent::new(marker),
            ))),
        )
    }

    /// Regression: draining completion receivers preserves notification ordering.
    #[tokio::test]
    async fn completion_drain_preserves_notification_ordering() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let log = Rc::new(RefCell::new(Vec::<String>::new()));
                let (sender, receiver) =
                    acp_gateway::<acp::AgentSide, _>(OrderTrackingClient { log: log.clone() });
                tokio::task::spawn_local(receiver.run());

                const N: usize = 100;
                let completions: Vec<_> = (0..N)
                    .map(|i| sender.forward_with_completion(text_notification(&format!("{i}"))))
                    .collect();
                for rx in completions {
                    let _ = rx.await;
                }

                log.borrow_mut().push("RESPONSE".into());

                let log = log.borrow();
                assert_eq!(log.len(), N + 1);
                assert_eq!(log[N], "RESPONSE");
                for i in 0..N {
                    assert_eq!(log[i], format!("{i}"));
                }
            })
            .await;
    }

    /// Regression: two-phase cutover keeps replay-before-response and avoids
    /// dropping live updates during drain.
    #[tokio::test]
    async fn two_phase_cutover_no_missing_updates() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let log = Rc::new(RefCell::new(Vec::<String>::new()));
                let (sender, receiver) =
                    acp_gateway::<acp::AgentSide, _>(OrderTrackingClient { log: log.clone() });
                tokio::task::spawn_local(receiver.run());

                const DELTA: usize = 50;
                const LIVE: usize = 20;

                // Phase 1: sync enqueue of replay notifications.
                let completions: Vec<_> = (0..DELTA)
                    .map(|i| {
                        sender.forward_with_completion(text_notification(&format!("delta-{i}")))
                    })
                    .collect();

                // Gate-open point; then concurrent producer emits live updates.
                let live_sender = sender.clone();
                let producer = tokio::task::spawn_local(async move {
                    for i in 0..LIVE {
                        live_sender
                            .forward_fire_and_forget(text_notification(&format!("live-{i}")));
                        // Encourage interleaving with drain.
                        tokio::task::yield_now().await;
                    }
                });

                // Drain replay completions while producer runs.
                for rx in completions {
                    let _ = rx.await;
                }

                // Mark response boundary.
                log.borrow_mut().push("RESPONSE".into());

                // Let producer and gateway finish remaining live updates.
                let _ = producer.await;
                for _ in 0..LIVE + 5 {
                    tokio::task::yield_now().await;
                }

                let log = log.borrow();
                let response_idx = log
                    .iter()
                    .position(|s| s == "RESPONSE")
                    .expect("RESPONSE marker must be in the log");

                // (1) Delta notifications are all present and before RESPONSE.
                for i in 0..DELTA {
                    let tag = format!("delta-{i}");
                    let pos = log
                        .iter()
                        .position(|s| s == &tag)
                        .unwrap_or_else(|| panic!("missing delta notification: {tag}"));
                    assert!(
                        pos < response_idx,
                        "{tag} at index {pos} must precede RESPONSE at index {response_idx}"
                    );
                }

                // (2) Delta notifications preserve enqueue order.
                let delta_positions: Vec<usize> = (0..DELTA)
                    .map(|i| log.iter().position(|s| s == &format!("delta-{i}")).unwrap())
                    .collect();
                for w in delta_positions.windows(2) {
                    assert!(
                        w[0] < w[1],
                        "delta ordering violated: delta at index {} came after delta at index {}",
                        w[0],
                        w[1]
                    );
                }

                // (3) No live updates are lost.
                for i in 0..LIVE {
                    let tag = format!("live-{i}");
                    assert!(
                        log.iter().any(|s| s == &tag),
                        "live update lost: {tag} not found in log"
                    );
                }

                // (4) Live updates do not precede replay delta.
                let last_delta = *delta_positions.last().unwrap();
                for i in 0..LIVE {
                    let tag = format!("live-{i}");
                    let pos = log.iter().position(|s| s == &tag).unwrap();
                    assert!(
                        pos > last_delta,
                        "{tag} at index {pos} must come after last delta at index {last_delta}"
                    );
                }
            })
            .await;
    }
}
