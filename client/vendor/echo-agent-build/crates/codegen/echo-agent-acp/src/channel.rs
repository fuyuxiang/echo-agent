use std::fmt;

use tokio::sync::{mpsc, oneshot};

use crate::{
    common::{AcpChannelFailure, AcpResult, acp_channel_failure_error},
    message::{AcpAgentMessage, AcpArgs, AcpClientMessage, AcpMethod, AcpRequest},
};

/// Receiver/sender pair, either for client/agent or agent/client message types.
pub struct AcpChannel<I, O> {
    pub rx: mpsc::UnboundedReceiver<I>,
    pub tx: mpsc::UnboundedSender<O>,
}

impl<I: AcpMethod, O: AcpMethod> AcpChannel<I, O> {
    pub fn new(rx: mpsc::UnboundedReceiver<I>, tx: mpsc::UnboundedSender<O>) -> Self {
        Self { rx, tx }
    }
}

/// Client channel: receive client messages from agent, send agent messages to agent.
pub type AcpClientChannel = AcpChannel<AcpClientMessage, AcpAgentMessage>;
/// Agent channel: receive agent messages from client, send client messages to client.
pub type AcpAgentChannel = AcpChannel<AcpAgentMessage, AcpClientMessage>;

/// Create a linked pair of client/agent channels.
pub fn acp_channels() -> (AcpClientChannel, AcpAgentChannel) {
    let (tx1, rx1) = mpsc::unbounded_channel();
    let (tx2, rx2) = mpsc::unbounded_channel();
    (AcpChannel::new(rx1, tx2), AcpChannel::new(rx2, tx1))
}

pub async fn acp_send<R, T>(request: T, tx: &mpsc::UnboundedSender<R>) -> AcpResult<T::Response>
where
    T: AcpRequest,
    R: From<AcpArgs<T>> + fmt::Debug,
{
    acp_enqueue(request, tx)?.await
}

/// Queue a request synchronously and return a future for the reply. Callers
/// can hold a short admission lock across enqueue without blocking on the turn.
pub fn acp_enqueue<R, T>(
    request: T,
    tx: &mpsc::UnboundedSender<R>,
) -> AcpResult<impl Future<Output = AcpResult<T::Response>> + use<R, T>>
where
    T: AcpRequest,
    R: From<AcpArgs<T>> + fmt::Debug,
{
    let (response_tx, response_rx) = oneshot::channel();
    let method = request.method_name();
    let args = AcpArgs {
        request,
        response_tx,
    };

    tx.send(args.into()).map_err(|_| {
        acp_channel_failure_error(
            format!("unable to send '{method}' request, channel closed"),
            AcpChannelFailure::SendFailed,
        )
    })?;

    Ok(async move {
        response_rx.await.map_err(|_| {
            acp_channel_failure_error(
                format!("unable to receive '{method}' response, channel closed"),
                AcpChannelFailure::RecvFailed,
            )
        })?
    })
}

#[cfg(test)]
mod acp_send_failure_tests {
    use super::{acp_enqueue, acp_send};
    use crate::common::{AcpChannelFailure, acp_channel_failure};
    use crate::message::AcpAgentMessage;
    use agent_client_protocol as acp;
    use tokio::sync::mpsc;

    fn ext_request() -> acp::ExtRequest {
        acp::ExtRequest::new(
            "echo.agent/test",
            serde_json::value::to_raw_value(&serde_json::json!({}))
                .unwrap()
                .into(),
        )
    }

    #[tokio::test]
    async fn send_failed_when_receiver_dropped_before_send() {
        let (tx, rx) = mpsc::unbounded_channel::<AcpAgentMessage>();
        drop(rx); // no peer listening -> enqueue fails
        let err = acp_send(ext_request(), &tx).await.unwrap_err();
        assert_eq!(
            acp_channel_failure(&err),
            Some(AcpChannelFailure::SendFailed)
        );
    }

    #[tokio::test]
    async fn recv_failed_when_response_channel_dropped_after_send() {
        let (tx, mut rx) = mpsc::unbounded_channel::<AcpAgentMessage>();
        let mut send_fut = Box::pin(acp_send(ext_request(), &tx));
        // First poll enqueues the request, then parks on the response channel.
        assert!(futures::poll!(send_fut.as_mut()).is_pending());
        // The peer "receives" the request then drops it (dropping response_tx).
        drop(rx.try_recv().expect("request should be enqueued"));
        let err = send_fut.await.unwrap_err();
        assert_eq!(
            acp_channel_failure(&err),
            Some(AcpChannelFailure::RecvFailed)
        );
    }

    #[tokio::test]
    async fn enqueue_sends_without_polling_the_response() {
        let (tx, mut rx) = mpsc::unbounded_channel::<AcpAgentMessage>();
        let response = acp_enqueue(ext_request(), &tx).unwrap();
        drop(tx);
        drop(rx.try_recv().expect("enqueue must happen before awaiting the reply"));
        assert_eq!(
            acp_channel_failure(&response.await.unwrap_err()),
            Some(AcpChannelFailure::RecvFailed)
        );
    }
}
