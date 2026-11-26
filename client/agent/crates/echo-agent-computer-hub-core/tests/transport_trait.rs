//! Behavioural coverage for the `Transport` trait, `Principal` builder,
//! and `TransportKind` re-export.

use async_trait::async_trait;
use serde_json::{Value, json};

use echo_agent_computer_hub_core::{Principal, Transport, TransportKind};
use echo_agent_tool_protocol::{SessionId, ToolId, UserId};
use echo_agent_tool_runtime::{
    ToolCallContext, ToolError, ToolStream, ToolStreamItem, TypedToolOutput, terminal_only,
};

fn uid(s: &str) -> UserId {
    UserId::new(s).expect("test user id")
}

fn sid(s: &str) -> SessionId {
    SessionId::new(s).expect("test session id")
}

fn tid(s: &str) -> ToolId {
    ToolId::new(s).expect("test tool id")
}

#[derive(Debug)]
struct EchoTransport {
    kind: TransportKind,
    user: UserId,
    session: SessionId,
}

#[async_trait]
impl Transport for EchoTransport {
    fn kind(&self) -> TransportKind {
        self.kind
    }

    async fn authorize(&self) -> Result<Principal, ToolError> {
        Ok(Principal::new(self.user.clone())
            .with_session(self.session.clone())
            .with_scope("tool.invoke"))
    }

    async fn call(
        &self,
        tool_id: ToolId,
        args: Value,
        _ctx: ToolCallContext,
    ) -> ToolStream<TypedToolOutput> {
        terminal_only(Ok(TypedToolOutput::from_value(tool_id, args)))
    }
}

#[tokio::test]
async fn boxed_transport_compiles_and_dispatches() {
    let boxed: Box<dyn Transport> = Box::new(EchoTransport {
        kind: TransportKind::Local,
        user: uid("alice"),
        session: sid("sess-1"),
    });
    let mut stream = boxed
        .call(tid("echo"), json!({"k": "v"}), ToolCallContext::default())
        .await;
    let item = futures::StreamExt::next(&mut stream)
        .await
        .expect("at least one item");
    match item {
        ToolStreamItem::Terminal(Ok(typed)) => assert_eq!(typed.value, json!({"k": "v"})),
        other => panic!("expected Terminal(Ok), got {other:?}"),
    }
}

#[tokio::test]
async fn kind_distinguishes_local_and_remote() {
    let local = EchoTransport {
        kind: TransportKind::Local,
        user: uid("alice"),
        session: sid("sess-1"),
    };
    let remote = EchoTransport {
        kind: TransportKind::Remote,
        user: uid("alice"),
        session: sid("sess-1"),
    };
    assert_eq!(local.kind(), TransportKind::Local);
    assert_eq!(remote.kind(), TransportKind::Remote);
    assert_ne!(local.kind(), remote.kind());
}

#[tokio::test]
async fn authorize_returns_bound_principal() {
    let t = EchoTransport {
        kind: TransportKind::Local,
        user: uid("alice"),
        session: sid("sess-1"),
    };
    let principal = t.authorize().await.expect("authorize succeeds");
    assert_eq!(principal.user_id, uid("alice"));
    assert!(principal.authorizes_session(&sid("sess-1")));
    assert!(!principal.authorizes_session(&sid("sess-other")));
    assert!(principal.has_scope("tool.invoke"));
    assert!(!principal.has_scope("admin"));
}

#[test]
fn principal_builder_chains_in_order() {
    let principal = Principal::new(uid("alice"))
        .with_session(sid("sess-a"))
        .with_session(sid("sess-b"))
        .with_scope("tool.invoke")
        .with_scope("tool.search")
        .with_audience("dispatcher.example");
    assert_eq!(principal.session_ids, vec![sid("sess-a"), sid("sess-b")]);
    assert_eq!(principal.scopes, vec!["tool.invoke", "tool.search"]);
    assert_eq!(principal.audiences, vec!["dispatcher.example"]);
}

#[test]
fn principal_supports_multi_session_tokens() {
    let p = Principal::new(uid("alice"))
        .with_session(sid("sess-1"))
        .with_session(sid("sess-2"));
    assert!(p.authorizes_session(&sid("sess-1")));
    assert!(p.authorizes_session(&sid("sess-2")));
    assert!(!p.authorizes_session(&sid("sess-3")));
    assert_eq!(p.session_ids.len(), 2);
}

#[test]
fn principal_default_state_is_empty() {
    let p = Principal::new(uid("alice"));
    assert!(p.session_ids.is_empty());
    assert!(p.scopes.is_empty());
    assert!(p.audiences.is_empty());
    assert!(!p.has_scope("anything"));
    assert!(!p.authorizes_session(&sid("sess")));
}
