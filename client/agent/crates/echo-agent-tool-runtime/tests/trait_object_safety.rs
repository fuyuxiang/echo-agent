//! `ToolDispatch` is intentionally object-safe so an impl can be
//! stored as `Box<dyn ToolDispatch>` (or `Arc<dyn ToolDispatch>`) for
//! shared dynamic dispatch. `Tool` is NOT object-safe — its associated
//! types make sense only via a typed-erasure adapter that lives downstream
//! of this crate.

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};

use echo_agent_tool_protocol::ToolId;
use echo_agent_tool_runtime::{
    ToolCallContext, ToolDispatch, ToolError, ToolErrorKind, ToolProgress, ToolStream,
    ToolStreamItem, TypedToolOutput, terminal_only, with_progress,
};

fn tid(s: &str) -> ToolId {
    ToolId::new(s).expect("test tool ids are well-formed")
}

struct EchoDispatch;

#[async_trait]
impl ToolDispatch for EchoDispatch {
    async fn call(
        &self,
        tool_id: ToolId,
        args: Value,
        _ctx: ToolCallContext,
    ) -> ToolStream<TypedToolOutput> {
        if tool_id.as_str() == "echo" {
            terminal_only(Ok(TypedToolOutput::from_value(tool_id, args)))
        } else {
            terminal_only(Err(ToolError::not_found(
                tool_id.clone(),
                format!("tool '{}' not registered", tool_id),
            )))
        }
    }
}

struct ProgressDispatch;

#[async_trait]
impl ToolDispatch for ProgressDispatch {
    async fn call(
        &self,
        tool_id: ToolId,
        args: Value,
        _ctx: ToolCallContext,
    ) -> ToolStream<TypedToolOutput> {
        let progress = futures::stream::iter(vec![
            ToolProgress::Text {
                text: "tick".into(),
            },
            ToolProgress::Text {
                text: "tock".into(),
            },
        ]);
        let tid = tool_id.clone();
        with_progress(progress, async move {
            Ok(TypedToolOutput::from_value(tid, args))
        })
    }
}

/// Constructs a stream that ends without a `Terminal` item — drives the
/// `call_terminal` default-impl recovery path.
struct EmptyStreamDispatch;

#[async_trait]
impl ToolDispatch for EmptyStreamDispatch {
    async fn call(
        &self,
        _tool_id: ToolId,
        _args: Value,
        _ctx: ToolCallContext,
    ) -> ToolStream<TypedToolOutput> {
        Box::pin(futures::stream::empty())
    }
}

#[tokio::test]
async fn dispatch_is_object_safe() {
    let boxed: Box<dyn ToolDispatch> = Box::new(EchoDispatch);
    let mut stream = boxed
        .call(tid("echo"), json!({"k": "v"}), ToolCallContext::default())
        .await;
    let item = stream.next().await.unwrap();
    match item {
        ToolStreamItem::Terminal(Ok(typed)) => assert_eq!(typed.value, json!({"k": "v"})),
        other => panic!("expected Terminal(Ok), got {other:?}"),
    }
}

#[tokio::test]
async fn arc_dispatch_is_object_safe() {
    let arc: std::sync::Arc<dyn ToolDispatch> = std::sync::Arc::new(EchoDispatch);
    let result = arc
        .call_terminal(tid("echo"), json!(42), ToolCallContext::default())
        .await;
    assert_eq!(result.unwrap().value, json!(42));
}

#[tokio::test]
async fn unknown_tool_returns_not_found() {
    let dispatch = EchoDispatch;
    let result = dispatch
        .call_terminal(tid("missing"), json!(null), ToolCallContext::default())
        .await;
    match result {
        Err(ref err) if err.kind == ToolErrorKind::NotFound => {
            assert!(
                err.detail.contains("missing"),
                "detail should mention the tool id, got: {}",
                err.detail
            );
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[tokio::test]
async fn call_terminal_drops_progress_items() {
    let dispatch = ProgressDispatch;
    let result = dispatch
        .call_terminal(tid("any"), json!("x"), ToolCallContext::default())
        .await;
    assert_eq!(result.unwrap().value, json!("x"));
}

#[tokio::test]
async fn call_terminal_surfaces_no_terminal_as_custom_error() {
    let dispatch = EmptyStreamDispatch;
    let result = dispatch
        .call_terminal(tid("any"), json!(null), ToolCallContext::default())
        .await;
    match result {
        Err(ref err) if err.kind == ToolErrorKind::Custom => {
            let code = err
                .details
                .as_ref()
                .and_then(|d| d.get("code"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            assert_eq!(code, "stream_no_terminal");
        }
        other => panic!("expected Custom(stream_no_terminal), got {other:?}"),
    }
}
