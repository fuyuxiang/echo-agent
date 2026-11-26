//! `LocalTransport` end-to-end coverage. Verifies that the transport
//! resolves through the bound resolver, drives both blocking and
//! streaming tools, and surfaces missing tools as `Terminal(NotFound)`.

use std::sync::Arc;

use dashmap::DashMap;

use async_trait::async_trait;
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use echo_agent_computer_hub_core::{
    CompoundResolver, ConnectionCleanupReport, ErasedTool, LocalTransport, ResolvedTool,
    SessionCleanupReport, ToolHandle, ToolRegistry, ToolSessionBindOutcome,
    ToolSessionUnbindOutcome, Transport, TransportKind,
};
use echo_agent_tool_protocol::{
    ConnectionId, RegistrationOutcome, ServerId, SessionId, ToolDefinitionMode, ToolId,
    ToolRegistration, ToolServerRegistration, TransportKind as WireTransportKind, UserId,
};
use echo_agent_tool_runtime::{
    SearchSnapshot, ServerSummary, Tool, ToolCallContext, ToolError, ToolProgress, ToolStream,
    ToolStreamItem, terminal_only, with_progress,
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
        Ok(serde_json::json!({ "echoed": args.payload }))
    }
}

#[derive(Debug)]
struct StreamerTool;

impl Tool for StreamerTool {
    type Args = EchoArgs;
    type Output = serde_json::Value;

    fn id(&self) -> ToolId {
        ToolId::new("streamer").expect("tool id")
    }

    fn description(&self, _ctx: &::echo_agent_tool_runtime::ListToolsContext) -> ToolDescription {
        ToolDescription::new("streamer", "Emits three progress chunks.")
    }

    async fn execute(&self, _ctx: ToolCallContext, args: Self::Args) -> ToolStream<Self::Output> {
        let chunks = futures::stream::iter(vec![
            ToolProgress::Text {
                text: "tick".to_string(),
            },
            ToolProgress::Text {
                text: "tock".to_string(),
            },
            ToolProgress::Text {
                text: "boom".to_string(),
            },
        ]);
        with_progress(chunks, async move {
            Ok(serde_json::json!({ "echoed": args.payload }))
        })
    }
}

type RegistryEntry = (ToolRegistration, Arc<dyn ToolHandle>);

#[derive(Debug, Default)]
struct InMemRegistry {
    entries: DashMap<(SessionId, ToolId), RegistryEntry>,
}

impl InMemRegistry {
    fn install(&self, session: SessionId, tool_id: ToolId, handle: Arc<dyn ToolHandle>) {
        let registration = ToolRegistration {
            tool_id: tool_id.clone(),
            sessions: Some(vec![session.clone()]),
            user_id: UserId::new("alice").expect("user id"),
            server_id: None,
            description: handle.description(&echo_agent_tool_runtime::ListToolsContext::default()),
            input_schema: None,
            capabilities: Some(handle.capabilities()),
            notification_schemas: None,
            transport_kind: WireTransportKind::Local,
            if_match_generation: None,
            metadata: None,
        };
        self.entries
            .insert((session, tool_id), (registration, handle));
    }
}

#[async_trait]
impl ToolRegistry for InMemRegistry {
    async fn register_tool(
        &self,
        _connection_id: ConnectionId,
        _reg: ToolRegistration,
    ) -> RegistrationOutcome {
        unreachable!("transport tests pre-populate via install()")
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

fn uid(s: &str) -> UserId {
    UserId::new(s).expect("user id")
}

async fn collect(
    stream: &mut ToolStream<echo_agent_tool_runtime::TypedToolOutput>,
) -> Vec<ToolStreamItem<echo_agent_tool_runtime::TypedToolOutput>> {
    let mut items = Vec::new();
    while let Some(item) = stream.next().await {
        items.push(item);
    }
    items
}

#[tokio::test]
async fn dispatches_blocking_tool_to_terminal_value() {
    let registry = Arc::new(InMemRegistry::default());
    registry.install(
        sid("sess-1"),
        tid("echo"),
        Arc::new(ErasedTool::new(EchoTool)),
    );
    let resolver = Arc::new(CompoundResolver::local_only(
        registry as Arc<dyn ToolRegistry>,
    ));
    let transport = LocalTransport::new(resolver, uid("alice"), sid("sess-1"));

    let mut stream = transport
        .call(
            tid("echo"),
            serde_json::json!({"payload": "hi"}),
            ToolCallContext::default(),
        )
        .await;
    let items = collect(&mut stream).await;
    assert_eq!(items.len(), 1);
    match &items[0] {
        ToolStreamItem::Terminal(Ok(typed)) => {
            assert_eq!(typed.value, serde_json::json!({"echoed": "hi"}));
        }
        other => panic!("expected Terminal(Ok), got {other:?}"),
    }
}

#[tokio::test]
async fn dispatches_streaming_tool_with_three_progress_then_terminal() {
    let registry = Arc::new(InMemRegistry::default());
    registry.install(
        sid("sess-1"),
        tid("streamer"),
        Arc::new(ErasedTool::new(StreamerTool)),
    );
    let resolver = Arc::new(CompoundResolver::local_only(
        registry as Arc<dyn ToolRegistry>,
    ));
    let transport = LocalTransport::new(resolver, uid("alice"), sid("sess-1"));

    let mut stream = transport
        .call(
            tid("streamer"),
            serde_json::json!({"payload": "hi"}),
            ToolCallContext::default(),
        )
        .await;
    let items = collect(&mut stream).await;
    assert_eq!(items.len(), 4);
    for item in &items[..3] {
        assert!(matches!(item, ToolStreamItem::Progress(_)));
    }
    assert!(matches!(items[3], ToolStreamItem::Terminal(Ok(_))));
}

#[tokio::test]
async fn missing_tool_resolves_as_terminal_not_found() {
    let registry = Arc::new(InMemRegistry::default());
    let resolver = Arc::new(CompoundResolver::local_only(
        registry as Arc<dyn ToolRegistry>,
    ));
    let transport = LocalTransport::new(resolver, uid("alice"), sid("sess-1"));

    let mut stream = transport
        .call(
            tid("ghost"),
            serde_json::json!(null),
            ToolCallContext::default(),
        )
        .await;
    let items = collect(&mut stream).await;
    assert_eq!(items.len(), 1);
    match &items[0] {
        ToolStreamItem::Terminal(Err(e))
            if e.kind == echo_agent_tool_runtime::ToolErrorKind::NotFound =>
        {
            assert!(
                e.detail.contains("ghost"),
                "detail should mention tool id: {}",
                e.detail
            );
        }
        other => panic!("expected Terminal(Err(NotFound)), got {other:?}"),
    }
}

#[tokio::test]
async fn invalid_arguments_surface_as_terminal_error() {
    let registry = Arc::new(InMemRegistry::default());
    registry.install(
        sid("sess-1"),
        tid("echo"),
        Arc::new(ErasedTool::new(EchoTool)),
    );
    let resolver = Arc::new(CompoundResolver::local_only(
        registry as Arc<dyn ToolRegistry>,
    ));
    let transport = LocalTransport::new(resolver, uid("alice"), sid("sess-1"));

    let mut stream = transport
        .call(
            tid("echo"),
            // Missing required `payload` field.
            serde_json::json!({}),
            ToolCallContext::default(),
        )
        .await;
    let items = collect(&mut stream).await;
    assert_eq!(items.len(), 1);
    match &items[0] {
        ToolStreamItem::Terminal(Err(e))
            if e.kind == echo_agent_tool_runtime::ToolErrorKind::InvalidArguments => {}
        other => panic!("expected Terminal(Err(InvalidArguments)), got {other:?}"),
    }
}

#[tokio::test]
async fn authorize_returns_bound_principal_with_invoke_scope() {
    let registry = Arc::new(InMemRegistry::default());
    let resolver = Arc::new(CompoundResolver::local_only(
        registry as Arc<dyn ToolRegistry>,
    ));
    let transport = LocalTransport::new(resolver, uid("alice"), sid("sess-1"));
    let principal = transport.authorize().await.expect("authorize");
    assert_eq!(principal.user_id, uid("alice"));
    assert!(principal.authorizes_session(&sid("sess-1")));
    assert!(principal.has_scope(echo_agent_computer_hub_core::LOCAL_INVOKE_SCOPE));
    assert_eq!(transport.kind(), TransportKind::Local);
}

#[test]
fn unused_helpers_silenced() {
    // `terminal_only` is re-exported for adapter authors; touch it here so
    // a future refactor that drops the import does not silently break the
    // re-export surface.
    let _: ToolStream<serde_json::Value> = terminal_only(Ok(serde_json::Value::Null));
}
