//! `InnerDispatchForResolver` coverage. Verifies the cycle-safe `Weak`
//! resolver semantics and the session-bound resolution path.

use std::sync::Arc;

use dashmap::DashMap;

use async_trait::async_trait;
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use echo_agent_computer_hub_core::{
    CompoundResolver, ConnectionCleanupReport, ErasedTool, InnerDispatchForResolver, ResolvedTool,
    SessionCleanupReport, ToolHandle, ToolRegistry, ToolSessionBindOutcome,
    ToolSessionUnbindOutcome,
};
use echo_agent_tool_protocol::{
    ConnectionId, RegistrationOutcome, ServerId, SessionId, ToolDefinitionMode, ToolId,
    ToolRegistration, ToolServerRegistration, TransportKind, UserId,
};
use echo_agent_tool_runtime::{
    SearchSnapshot, ServerSummary, Tool, ToolCallContext, ToolDispatch, ToolError, ToolStreamItem,
};
use echo_agent_tool_types::ToolDescription;

#[derive(Debug, Default, Clone, Serialize, Deserialize, schemars::JsonSchema)]
struct EchoArgs {
    payload: String,
}

#[derive(Debug)]
struct EchoTool;

impl Tool for EchoTool {
    type Args = EchoArgs;
    type Output = serde_json::Value;

    fn id(&self) -> ToolId {
        ToolId::new("echo").expect("tool id")
    }

    fn description(&self, _ctx: &::echo_agent_tool_runtime::ListToolsContext) -> ToolDescription {
        ToolDescription::new("echo", "Echoes its input.")
    }

    async fn run(
        &self,
        _ctx: ToolCallContext,
        args: Self::Args,
    ) -> Result<Self::Output, ToolError> {
        Ok(serde_json::json!({"echoed": args.payload}))
    }
}

type RegistryEntry = (ToolRegistration, Arc<dyn ToolHandle>);

#[derive(Debug, Default)]
struct InMemRegistry {
    entries: DashMap<(SessionId, ToolId), RegistryEntry>,
}

impl InMemRegistry {
    fn install(&self, session: SessionId, tool: ToolId, handle: Arc<dyn ToolHandle>) {
        let registration = ToolRegistration {
            tool_id: tool.clone(),
            sessions: Some(vec![session.clone()]),
            user_id: UserId::new("alice").expect("user id"),
            server_id: None,
            description: handle.description(&echo_agent_tool_runtime::ListToolsContext::default()),
            input_schema: None,
            capabilities: Some(handle.capabilities()),
            notification_schemas: None,
            transport_kind: TransportKind::Local,
            if_match_generation: None,
            metadata: None,
        };
        self.entries.insert((session, tool), (registration, handle));
    }
}

#[async_trait]
impl ToolRegistry for InMemRegistry {
    async fn register_tool(
        &self,
        _connection_id: ConnectionId,
        _reg: ToolRegistration,
    ) -> RegistrationOutcome {
        unreachable!()
    }
    async fn register_server(
        &self,
        _connection_id: ConnectionId,
        _reg: ToolServerRegistration,
    ) -> Vec<RegistrationOutcome> {
        unreachable!()
    }
    async fn unregister_tool(&self, _connection_id: &ConnectionId, _tool: &ToolId) -> bool {
        unreachable!()
    }
    async fn unregister_server(&self, _connection_id: &ConnectionId, _server: &ServerId) -> usize {
        unreachable!()
    }
    async fn bind_tool_session(
        &self,
        _connection_id: &ConnectionId,
        _tool: &ToolId,
        _session_id: &SessionId,
    ) -> ToolSessionBindOutcome {
        unreachable!()
    }
    async fn unbind_tool_session(
        &self,
        _connection_id: &ConnectionId,
        _tool: &ToolId,
        _session_id: &SessionId,
    ) -> ToolSessionUnbindOutcome {
        unreachable!()
    }
    async fn drop_connection(&self, _connection_id: &ConnectionId) -> ConnectionCleanupReport {
        ConnectionCleanupReport::default()
    }
    fn find_tool(&self, session: &SessionId, tool: &ToolId) -> Option<ResolvedTool> {
        let (registration, handle) = self
            .entries
            .get(&(session.clone(), tool.clone()))?
            .value()
            .clone();
        Some(ResolvedTool::Local {
            tool: handle,
            registration,
        })
    }
    fn list_tools(&self, _session: &SessionId, _mode: &ToolDefinitionMode) -> Vec<ToolDescription> {
        vec![]
    }
    fn list_servers(&self, _session: &SessionId) -> Vec<ServerSummary> {
        vec![]
    }
    fn search(&self, _session: &SessionId, _query: &str, _limit: usize) -> SearchSnapshot {
        SearchSnapshot {
            results: vec![],
            total_hidden_tools: 0,
            is_ready: true,
        }
    }
    async fn unregister_session(&self, _session: &SessionId) -> SessionCleanupReport {
        SessionCleanupReport::default()
    }
    fn tool_sessions(
        &self,
        _connection_id: &ConnectionId,
        _tool: &ToolId,
    ) -> std::collections::HashSet<SessionId> {
        std::collections::HashSet::new()
    }

    fn list_servers_for_user(
        &self,
        _user_id: &echo_agent_tool_protocol::UserId,
    ) -> Vec<echo_agent_computer_hub_core::registry::ServerRecord> {
        Vec::new()
    }

    fn get_server_record(
        &self,
        _connection_id: &ConnectionId,
    ) -> Option<echo_agent_computer_hub_core::registry::ServerRecord> {
        None
    }
}

fn sid(s: &str) -> SessionId {
    SessionId::new(s).expect("session id")
}

fn tid(s: &str) -> ToolId {
    ToolId::new(s).expect("tool id")
}

#[tokio::test]
async fn inner_dispatch_resolves_through_bound_session() {
    let registry = Arc::new(InMemRegistry::default());
    registry.install(
        sid("sess-1"),
        tid("echo"),
        Arc::new(ErasedTool::new(EchoTool)),
    );
    let resolver = Arc::new(CompoundResolver::local_only(
        registry as Arc<dyn ToolRegistry>,
    ));
    let inner = InnerDispatchForResolver::new(Arc::downgrade(&resolver), sid("sess-1"));
    assert_eq!(inner.session_id(), &sid("sess-1"));

    let result = inner
        .call_terminal(
            tid("echo"),
            serde_json::json!({"payload": "x"}),
            ToolCallContext::default(),
        )
        .await
        .expect("terminal ok");
    assert_eq!(result.value, serde_json::json!({"echoed": "x"}));
}

#[tokio::test]
async fn inner_dispatch_returns_not_found_when_tool_absent() {
    let registry = Arc::new(InMemRegistry::default());
    let resolver = Arc::new(CompoundResolver::local_only(
        registry as Arc<dyn ToolRegistry>,
    ));
    let inner = InnerDispatchForResolver::new(Arc::downgrade(&resolver), sid("sess-1"));
    let mut stream = inner
        .call(
            tid("ghost"),
            serde_json::json!(null),
            ToolCallContext::default(),
        )
        .await;
    let item = stream.next().await.expect("terminal");
    match item {
        ToolStreamItem::Terminal(Err(ref e))
            if e.kind == echo_agent_tool_runtime::ToolErrorKind::NotFound =>
        {
            assert!(
                e.detail.contains("ghost"),
                "detail should mention tool id: {}",
                e.detail
            );
        }
        other => panic!("expected Terminal(NotFound), got {other:?}"),
    }
}

#[tokio::test]
async fn inner_dispatch_uses_bound_session_not_context_session() {
    // Even if the context were to carry a different session, the inner
    // dispatch handle resolves against its construction-time session.
    let registry = Arc::new(InMemRegistry::default());
    registry.install(
        sid("sess-A"),
        tid("echo"),
        Arc::new(ErasedTool::new(EchoTool)),
    );
    let resolver = Arc::new(CompoundResolver::local_only(
        registry as Arc<dyn ToolRegistry>,
    ));
    let inner = InnerDispatchForResolver::new(Arc::downgrade(&resolver), sid("sess-B"));
    let mut stream = inner
        .call(
            tid("echo"),
            serde_json::json!({"payload": "x"}),
            ToolCallContext::default(),
        )
        .await;
    let item = stream.next().await.expect("terminal");
    match item {
        ToolStreamItem::Terminal(Err(ref e))
            if e.kind == echo_agent_tool_runtime::ToolErrorKind::NotFound => {}
        other => panic!("session-A registration must not be visible from session-B, got {other:?}"),
    }
}

#[tokio::test]
async fn inner_dispatch_after_resolver_drop_returns_computer_hub_dropped() {
    let registry = Arc::new(InMemRegistry::default());
    let resolver = Arc::new(CompoundResolver::local_only(
        registry as Arc<dyn ToolRegistry>,
    ));
    let weak = Arc::downgrade(&resolver);
    let inner = InnerDispatchForResolver::new(weak, sid("sess-1"));
    drop(resolver);
    let mut stream = inner
        .call(
            tid("echo"),
            serde_json::json!(null),
            ToolCallContext::default(),
        )
        .await;
    let item = stream.next().await.expect("terminal");
    match item {
        ToolStreamItem::Terminal(Err(ref e))
            if e.kind == echo_agent_tool_runtime::ToolErrorKind::Custom =>
        {
            assert!(
                e.detail.contains("computer_hub_dropped")
                    || e.details
                        .as_ref()
                        .and_then(|d| d.get("code"))
                        .and_then(|c| c.as_str())
                        == Some("computer_hub_dropped"),
                "expected computer_hub_dropped code, got: {:?}",
                e
            );
        }
        other => panic!("expected Terminal(Custom(computer_hub_dropped)), got {other:?}"),
    }
}

#[tokio::test]
async fn inner_dispatch_implements_object_safe_tool_dispatch() {
    let registry = Arc::new(InMemRegistry::default());
    let resolver = Arc::new(CompoundResolver::local_only(
        registry as Arc<dyn ToolRegistry>,
    ));
    let inner: Arc<dyn ToolDispatch> = Arc::new(InnerDispatchForResolver::new(
        Arc::downgrade(&resolver),
        sid("sess-1"),
    ));
    let result = inner
        .call_terminal(
            tid("ghost"),
            serde_json::json!(null),
            ToolCallContext::default(),
        )
        .await;
    assert!(
        matches!(result, Err(ref e) if e.kind == echo_agent_tool_runtime::ToolErrorKind::NotFound)
    );
}
