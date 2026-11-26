//! `CompoundResolver` and `ResolvedTool` coverage. Exercises local-only,
//! local-shadows-remote, remote-fallback, and cross-session scenarios.

use std::sync::Arc;

use dashmap::DashMap;

use async_trait::async_trait;

use echo_agent_computer_hub_core::{
    CompoundResolver, ConnectionCleanupReport, ErasedTool, ResolvedTool, SessionCleanupReport,
    ToolHandle, ToolRegistry, ToolSessionBindOutcome, ToolSessionUnbindOutcome,
};
use echo_agent_tool_protocol::{
    ConnectionId, RegistrationOutcome, ServerId, SessionId, ToolDefinitionMode, ToolId,
    ToolRegistration, ToolServerRegistration, TransportKind, UserId,
};
use echo_agent_tool_runtime::{
    SearchSnapshot, ServerSummary, Tool, ToolCallContext, ToolError, ToolStreamItem,
};
use echo_agent_tool_types::ToolDescription;
use futures::StreamExt;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize, schemars::JsonSchema)]
struct EmptyArgs {}

#[derive(Debug)]
struct StubTool {
    id: ToolId,
}

impl Tool for StubTool {
    type Args = EmptyArgs;
    type Output = serde_json::Value;

    fn id(&self) -> ToolId {
        self.id.clone()
    }

    fn description(&self, _ctx: &::echo_agent_tool_runtime::ListToolsContext) -> ToolDescription {
        ToolDescription::new(self.id.as_str(), format!("stub for {}", self.id))
    }

    async fn run(
        &self,
        _ctx: ToolCallContext,
        _args: Self::Args,
    ) -> Result<Self::Output, ToolError> {
        Ok(serde_json::json!({"id": self.id.as_str()}))
    }
}

#[derive(Debug)]
struct PlaneRegistry {
    // Set once at construction; `TransportKind` is `Copy` so a direct
    // field is the obvious choice — no interior mutability required.
    transport_kind: TransportKind,
    entries: DashMap<(SessionId, ToolId), ToolRegistration>,
    handles: DashMap<ToolId, Arc<dyn ToolHandle>>,
}

impl PlaneRegistry {
    fn new(kind: TransportKind) -> Self {
        Self {
            transport_kind: kind,
            entries: DashMap::new(),
            handles: DashMap::new(),
        }
    }

    fn install(&self, session: &SessionId, id: &ToolId) {
        let reg = ToolRegistration {
            tool_id: id.clone(),
            sessions: Some(vec![session.clone()]),
            user_id: UserId::new("alice").expect("user id"),
            server_id: None,
            description: ToolDescription::new(id.as_str(), format!("stub for {id}")),
            input_schema: None,
            capabilities: None,
            notification_schemas: None,
            transport_kind: self.transport_kind,
            if_match_generation: None,
            metadata: None,
        };
        self.entries.insert((session.clone(), id.clone()), reg);
        self.handles.insert(
            id.clone(),
            Arc::new(ErasedTool::new(StubTool { id: id.clone() })),
        );
    }
}

#[async_trait]
impl ToolRegistry for PlaneRegistry {
    async fn register_tool(
        &self,
        _connection_id: ConnectionId,
        _reg: ToolRegistration,
    ) -> RegistrationOutcome {
        unreachable!("resolver tests pre-populate via install()")
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
        let registration = self
            .entries
            .get(&(session.clone(), tool.clone()))?
            .value()
            .clone();
        let handle = self.handles.get(tool)?.value().clone();
        match registration.transport_kind {
            TransportKind::Local => Some(ResolvedTool::Local {
                tool: handle,
                registration,
            }),
            TransportKind::Remote => Some(ResolvedTool::Remote {
                proxy: handle,
                registration,
            }),
        }
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
async fn local_only_resolves_local_hits() {
    let local = Arc::new(PlaneRegistry::new(TransportKind::Local));
    local.install(&sid("sess-1"), &tid("foo"));
    let resolver = CompoundResolver::local_only(local as Arc<dyn ToolRegistry>);
    match resolver.resolve(&sid("sess-1"), &tid("foo")) {
        Some(ResolvedTool::Local { registration, .. }) => {
            assert_eq!(registration.tool_id, tid("foo"));
        }
        other => panic!("expected Local, got {other:?}"),
    }
}

#[tokio::test]
async fn local_only_returns_none_for_unknown() {
    let local = Arc::new(PlaneRegistry::new(TransportKind::Local));
    let resolver = CompoundResolver::local_only(local as Arc<dyn ToolRegistry>);
    assert!(resolver.resolve(&sid("sess-1"), &tid("missing")).is_none());
}

#[tokio::test]
async fn compound_falls_through_to_remote_when_local_misses() {
    let local = Arc::new(PlaneRegistry::new(TransportKind::Local));
    let remote = Arc::new(PlaneRegistry::new(TransportKind::Remote));
    remote.install(&sid("sess-1"), &tid("foo"));
    let resolver = CompoundResolver::compound(
        local as Arc<dyn ToolRegistry>,
        remote as Arc<dyn ToolRegistry>,
    );
    match resolver.resolve(&sid("sess-1"), &tid("foo")) {
        Some(ResolvedTool::Remote { registration, .. }) => {
            assert_eq!(registration.tool_id, tid("foo"));
            assert_eq!(registration.transport_kind, TransportKind::Remote);
        }
        other => panic!("expected Remote, got {other:?}"),
    }
}

#[tokio::test]
async fn local_shadows_same_id_remote() {
    let local = Arc::new(PlaneRegistry::new(TransportKind::Local));
    local.install(&sid("sess-1"), &tid("foo"));
    let remote = Arc::new(PlaneRegistry::new(TransportKind::Remote));
    remote.install(&sid("sess-1"), &tid("foo"));
    let resolver = CompoundResolver::compound(
        local as Arc<dyn ToolRegistry>,
        remote as Arc<dyn ToolRegistry>,
    );
    match resolver.resolve(&sid("sess-1"), &tid("foo")) {
        Some(ResolvedTool::Local { registration, .. }) => {
            assert_eq!(registration.transport_kind, TransportKind::Local);
        }
        other => panic!("expected local resolution to shadow remote, got {other:?}"),
    }
}

#[tokio::test]
async fn cross_session_lookup_returns_none() {
    let local = Arc::new(PlaneRegistry::new(TransportKind::Local));
    local.install(&sid("sess-1"), &tid("foo"));
    let resolver = CompoundResolver::local_only(local as Arc<dyn ToolRegistry>);
    assert!(resolver.resolve(&sid("sess-other"), &tid("foo")).is_none());
}

#[tokio::test]
async fn compound_returns_none_when_neither_plane_holds_id() {
    let local = Arc::new(PlaneRegistry::new(TransportKind::Local));
    let remote = Arc::new(PlaneRegistry::new(TransportKind::Remote));
    let resolver = CompoundResolver::compound(
        local as Arc<dyn ToolRegistry>,
        remote as Arc<dyn ToolRegistry>,
    );
    assert!(resolver.resolve(&sid("sess-1"), &tid("foo")).is_none());
}

#[tokio::test]
async fn resolved_tool_helpers_borrow_active_handle_and_registration() {
    let local = Arc::new(PlaneRegistry::new(TransportKind::Local));
    local.install(&sid("sess-1"), &tid("foo"));
    let resolver = CompoundResolver::local_only(local as Arc<dyn ToolRegistry>);
    let resolved = resolver.resolve(&sid("sess-1"), &tid("foo")).expect("hit");
    assert_eq!(resolved.registration().tool_id, tid("foo"));
    assert_eq!(resolved.handle().id(), tid("foo"));
}

#[tokio::test]
async fn resolve_and_dispatch_drives_the_resolved_handle() {
    let local = Arc::new(PlaneRegistry::new(TransportKind::Local));
    local.install(&sid("sess-1"), &tid("foo"));
    let resolver = CompoundResolver::local_only(local as Arc<dyn ToolRegistry>);
    let mut stream = resolver
        .resolve_and_dispatch(
            &sid("sess-1"),
            tid("foo"),
            serde_json::json!({}),
            ToolCallContext::default(),
        )
        .await;
    let item = stream.next().await.expect("terminal");
    match item {
        ToolStreamItem::Terminal(Ok(typed)) => {
            assert_eq!(typed.value, serde_json::json!({"id": "foo"}));
        }
        other => panic!("expected Terminal(Ok), got {other:?}"),
    }
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn resolve_and_dispatch_misses_yield_terminal_not_found() {
    let local = Arc::new(PlaneRegistry::new(TransportKind::Local));
    let resolver = CompoundResolver::local_only(local as Arc<dyn ToolRegistry>);
    let mut stream = resolver
        .resolve_and_dispatch(
            &sid("sess-1"),
            tid("missing"),
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
                e.detail.contains("missing"),
                "detail should mention tool id: {}",
                e.detail
            );
        }
        other => panic!("expected Terminal(Err(NotFound)), got {other:?}"),
    }
}
