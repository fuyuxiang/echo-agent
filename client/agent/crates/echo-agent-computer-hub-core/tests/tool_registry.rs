//! `ToolRegistry` trait coverage via a per-test mock backed by `DashMap`
//! — lock-free per-key concurrent access mirrors the production
//! direction even at the test layer. The mock implements the
//! connection-scoped `ToolRegistry` trait surface.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use dashmap::DashMap;

use echo_agent_computer_hub_core::{
    ConnectionCleanupReport, ErasedTool, ResolvedTool, SessionCleanupReport, ToolHandle,
    ToolRegistry, ToolSessionBindOutcome, ToolSessionUnbindOutcome, resolver::CompoundResolver,
};
use echo_agent_tool_protocol::{
    ConnectionId, RegistrationOutcome, ServerId, SessionId, ToolDefinitionMode, ToolId,
    ToolRegistration, ToolServerRegistration, TransportKind, UserId,
};
use echo_agent_tool_runtime::{SearchSnapshot, ServerSummary, Tool, ToolCallContext, ToolError};
use echo_agent_tool_types::ToolDescription;
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
        unreachable!("registry tests do not exercise execution")
    }
}

#[derive(Debug, Clone)]
struct MockEntry {
    registration: ToolRegistration,
    sessions: HashSet<SessionId>,
}

/// Mock registry. Last-write-wins on duplicate registrations within a
/// `(connection, tool_id)` slot — pinned here so the trait contract has
/// a clear test fixture.
#[derive(Debug, Default)]
struct MockRegistry {
    entries: DashMap<(ConnectionId, ToolId), MockEntry>,
    by_session: DashMap<(SessionId, ToolId), ConnectionId>,
    handles: DashMap<ToolId, Arc<dyn ToolHandle>>,
}

impl MockRegistry {
    fn install_handle(&self, tool: Arc<dyn ToolHandle>) {
        self.handles.insert(tool.id(), tool);
    }
}

fn build_registration(tool: &ToolId, sessions: &[SessionId]) -> ToolRegistration {
    ToolRegistration {
        tool_id: tool.clone(),
        sessions: Some(sessions.to_vec()),
        user_id: UserId::new("alice").expect("valid user id"),
        server_id: None,
        description: ToolDescription::new(tool.as_str(), format!("desc for {tool}")),
        input_schema: None,
        capabilities: None,
        notification_schemas: None,
        transport_kind: TransportKind::Local,
        if_match_generation: None,
        metadata: None,
    }
}

#[async_trait]
impl ToolRegistry for MockRegistry {
    async fn register_tool(
        &self,
        connection_id: ConnectionId,
        reg: ToolRegistration,
    ) -> RegistrationOutcome {
        let key = (connection_id.clone(), reg.tool_id.clone());
        let sessions: HashSet<SessionId> = reg
            .sessions
            .as_ref()
            .map(|v| v.iter().cloned().collect())
            .unwrap_or_default();
        let updated = self
            .entries
            .insert(
                key,
                MockEntry {
                    registration: reg.clone(),
                    sessions: sessions.clone(),
                },
            )
            .is_some();
        for session in &sessions {
            self.by_session.insert(
                (session.clone(), reg.tool_id.clone()),
                connection_id.clone(),
            );
        }
        if updated {
            RegistrationOutcome::Updated {
                tool_id: reg.tool_id,
                generation: 1,
            }
        } else {
            RegistrationOutcome::Registered {
                tool_id: reg.tool_id,
                generation: 0,
            }
        }
    }

    async fn register_server(
        &self,
        connection_id: ConnectionId,
        reg: ToolServerRegistration,
    ) -> Vec<RegistrationOutcome> {
        let mut outcomes = Vec::with_capacity(reg.tools.len());
        for tool in reg.tools {
            let tool_id = tool
                .derive_tool_id()
                .expect("test descriptions have valid tool ids");
            let registration = ToolRegistration {
                tool_id: tool_id.clone(),
                sessions: reg.sessions.clone(),
                user_id: reg.user_id.clone(),
                server_id: Some(reg.server_id.clone()),
                description: tool.description,
                input_schema: tool.input_schema,
                capabilities: tool.capabilities,
                notification_schemas: tool.notification_schemas,
                transport_kind: TransportKind::Remote,
                if_match_generation: None,
                metadata: None,
            };
            outcomes.push(
                self.register_tool(connection_id.clone(), registration)
                    .await,
            );
        }
        outcomes
    }

    async fn unregister_tool(&self, connection_id: &ConnectionId, tool: &ToolId) -> bool {
        let Some((_, removed)) = self.entries.remove(&(connection_id.clone(), tool.clone())) else {
            return false;
        };
        for session in &removed.sessions {
            self.by_session
                .remove_if(&(session.clone(), tool.clone()), |_, owner| {
                    owner == connection_id
                });
        }
        true
    }

    async fn unregister_server(&self, connection_id: &ConnectionId, server: &ServerId) -> usize {
        let to_remove: Vec<ToolId> = self
            .entries
            .iter()
            .filter(|r| {
                r.key().0 == *connection_id
                    && r.value().registration.server_id.as_ref() == Some(server)
            })
            .map(|r| r.key().1.clone())
            .collect();
        let mut removed = 0usize;
        for tool in to_remove {
            if self.unregister_tool(connection_id, &tool).await {
                removed += 1;
            }
        }
        removed
    }

    async fn bind_tool_session(
        &self,
        connection_id: &ConnectionId,
        tool: &ToolId,
        session_id: &SessionId,
    ) -> ToolSessionBindOutcome {
        let key = (connection_id.clone(), tool.clone());
        let Some(mut entry) = self.entries.get_mut(&key) else {
            return ToolSessionBindOutcome::UnknownTool;
        };
        if !entry.value_mut().sessions.insert(session_id.clone()) {
            return ToolSessionBindOutcome::AlreadyBound;
        }
        self.by_session
            .insert((session_id.clone(), tool.clone()), connection_id.clone());
        ToolSessionBindOutcome::Bound
    }

    async fn unbind_tool_session(
        &self,
        connection_id: &ConnectionId,
        tool: &ToolId,
        session_id: &SessionId,
    ) -> ToolSessionUnbindOutcome {
        let key = (connection_id.clone(), tool.clone());
        let Some(mut entry) = self.entries.get_mut(&key) else {
            return ToolSessionUnbindOutcome::UnknownTool;
        };
        if !entry.value_mut().sessions.remove(session_id) {
            return ToolSessionUnbindOutcome::NotBound;
        }
        self.by_session
            .remove_if(&(session_id.clone(), tool.clone()), |_, owner| {
                owner == connection_id
            });
        ToolSessionUnbindOutcome::Unbound
    }

    async fn drop_connection(&self, connection_id: &ConnectionId) -> ConnectionCleanupReport {
        let to_remove: Vec<ToolId> = self
            .entries
            .iter()
            .filter(|r| r.key().0 == *connection_id)
            .map(|r| r.key().1.clone())
            .collect();
        let mut report = ConnectionCleanupReport::default();
        for tool in to_remove {
            if let Some((_, removed)) = self.entries.remove(&(connection_id.clone(), tool.clone()))
            {
                report.tools_dropped += 1;
                for session in removed.sessions {
                    if self
                        .by_session
                        .remove_if(&(session, tool.clone()), |_, owner| owner == connection_id)
                        .is_some()
                    {
                        report.session_bindings_cleared += 1;
                    }
                }
            }
        }
        report
    }

    fn find_tool(&self, session: &SessionId, tool: &ToolId) -> Option<ResolvedTool> {
        let owner = self
            .by_session
            .get(&(session.clone(), tool.clone()))?
            .value()
            .clone();
        let entry = self.entries.get(&(owner, tool.clone()))?;
        let registration = entry.value().registration.clone();
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

    fn list_tools(&self, session: &SessionId, _mode: &ToolDefinitionMode) -> Vec<ToolDescription> {
        self.by_session
            .iter()
            .filter(|r| r.key().0 == *session)
            .filter_map(|r| {
                let owner = r.value().clone();
                let tool_id = r.key().1.clone();
                self.entries
                    .get(&(owner, tool_id))
                    .map(|e| e.value().registration.description.clone())
            })
            .collect()
    }

    fn list_servers(&self, session: &SessionId) -> Vec<ServerSummary> {
        let mut by_server: HashMap<ServerId, Vec<String>> = HashMap::new();
        for r in self.by_session.iter().filter(|r| r.key().0 == *session) {
            let owner = r.value().clone();
            let tool_id = r.key().1.clone();
            if let Some(entry) = self.entries.get(&(owner, tool_id)) {
                let reg = &entry.value().registration;
                let server = reg
                    .server_id
                    .clone()
                    .unwrap_or_else(|| ServerId::synthesize_for_tool(r.value(), &reg.tool_id));
                by_server
                    .entry(server)
                    .or_default()
                    .push(reg.tool_id.as_str().to_string());
            }
        }
        by_server
            .into_iter()
            .map(|(server, mut names)| {
                names.sort();
                ServerSummary {
                    name: server.into_inner(),
                    description: None,
                    tool_names: names,
                }
            })
            .collect()
    }

    fn search(&self, session: &SessionId, query: &str, limit: usize) -> SearchSnapshot {
        let matches: Vec<_> = self
            .by_session
            .iter()
            .filter(|r| r.key().0 == *session)
            .filter_map(|r| {
                let owner = r.value().clone();
                let tool_id = r.key().1.clone();
                let entry = self.entries.get(&(owner, tool_id))?;
                let reg = &entry.value().registration;
                if reg.tool_id.as_str().contains(query) {
                    Some(echo_agent_tool_runtime::ToolSearchResult {
                        tool_name: reg.tool_id.as_str().to_string(),
                        server_name: reg
                            .server_id
                            .as_ref()
                            .map(|s| s.as_str().to_string())
                            .unwrap_or_default(),
                        description: reg.description.description.clone(),
                        score: 1.0,
                        parameters: vec![],
                        input_schema: serde_json::Value::Null,
                    })
                } else {
                    None
                }
            })
            .take(limit)
            .collect();
        SearchSnapshot {
            results: matches,
            total_hidden_tools: 0,
            is_ready: true,
        }
    }

    async fn unregister_session(&self, session: &SessionId) -> SessionCleanupReport {
        let pairs: Vec<(ToolId, ConnectionId)> = self
            .by_session
            .iter()
            .filter(|r| r.key().0 == *session)
            .map(|r| (r.key().1.clone(), r.value().clone()))
            .collect();
        let mut report = SessionCleanupReport::default();
        for (tool_id, owner) in pairs {
            self.by_session
                .remove_if(&(session.clone(), tool_id.clone()), |_, value| {
                    value == &owner
                });
            if let Some(mut entry) = self.entries.get_mut(&(owner, tool_id)) {
                entry.value_mut().sessions.remove(session);
                report.tools_touched += 1;
                if entry.value().sessions.is_empty() {
                    report.tools_left_orphaned += 1;
                }
            }
        }
        report
    }

    fn tool_sessions(&self, connection_id: &ConnectionId, tool: &ToolId) -> HashSet<SessionId> {
        self.entries
            .get(&(connection_id.clone(), tool.clone()))
            .map(|r| r.value().sessions.clone())
            .unwrap_or_default()
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
    SessionId::new(s).expect("valid session id")
}

fn tid(s: &str) -> ToolId {
    ToolId::new(s).expect("valid tool id")
}

fn cid(s: &str) -> ConnectionId {
    ConnectionId::new(s).expect("valid connection id")
}

#[tokio::test]
async fn register_then_find_returns_local_resolution() {
    let reg = MockRegistry::default();
    reg.install_handle(Arc::new(ErasedTool::new(StubTool { id: tid("foo") })));
    let outcome = reg
        .register_tool(cid("c1"), build_registration(&tid("foo"), &[sid("sess-1")]))
        .await;
    assert!(matches!(outcome, RegistrationOutcome::Registered { .. }));
    let resolved = reg
        .find_tool(&sid("sess-1"), &tid("foo"))
        .expect("registration found");
    match resolved {
        ResolvedTool::Local { registration, .. } => {
            assert_eq!(registration.tool_id, tid("foo"));
            assert!(
                registration
                    .sessions
                    .as_ref()
                    .is_some_and(|s| s.contains(&sid("sess-1")))
            );
        }
        other => panic!("expected Local, got {other:?}"),
    }
}

#[tokio::test]
async fn find_in_other_session_returns_none() {
    let reg = MockRegistry::default();
    reg.install_handle(Arc::new(ErasedTool::new(StubTool { id: tid("foo") })));
    reg.register_tool(cid("c1"), build_registration(&tid("foo"), &[sid("sess-1")]))
        .await;
    assert!(reg.find_tool(&sid("sess-2"), &tid("foo")).is_none());
}

#[tokio::test]
async fn duplicate_registration_yields_updated_outcome() {
    let reg = MockRegistry::default();
    reg.install_handle(Arc::new(ErasedTool::new(StubTool { id: tid("foo") })));
    let first = reg
        .register_tool(cid("c1"), build_registration(&tid("foo"), &[sid("sess-1")]))
        .await;
    let second = reg
        .register_tool(cid("c1"), build_registration(&tid("foo"), &[sid("sess-1")]))
        .await;
    assert!(matches!(first, RegistrationOutcome::Registered { .. }));
    assert!(matches!(second, RegistrationOutcome::Updated { .. }));
}

#[tokio::test]
async fn unregister_tool_removes_only_that_entry() {
    let reg = MockRegistry::default();
    reg.install_handle(Arc::new(ErasedTool::new(StubTool { id: tid("foo") })));
    reg.install_handle(Arc::new(ErasedTool::new(StubTool { id: tid("bar") })));
    reg.register_tool(cid("c1"), build_registration(&tid("foo"), &[sid("sess-1")]))
        .await;
    reg.register_tool(cid("c1"), build_registration(&tid("bar"), &[sid("sess-1")]))
        .await;
    assert!(reg.unregister_tool(&cid("c1"), &tid("foo")).await);
    assert!(reg.find_tool(&sid("sess-1"), &tid("foo")).is_none());
    assert!(reg.find_tool(&sid("sess-1"), &tid("bar")).is_some());
}

#[tokio::test]
async fn unregister_session_drops_session_binding_and_leaves_orphan_count() {
    let reg = MockRegistry::default();
    reg.install_handle(Arc::new(ErasedTool::new(StubTool { id: tid("foo") })));
    reg.install_handle(Arc::new(ErasedTool::new(StubTool { id: tid("bar") })));
    reg.register_tool(cid("c1"), build_registration(&tid("foo"), &[sid("sess-1")]))
        .await;
    reg.register_tool(
        cid("c1"),
        build_registration(&tid("bar"), &[sid("sess-1"), sid("sess-2")]),
    )
    .await;
    let report = reg.unregister_session(&sid("sess-1")).await;
    assert_eq!(report.tools_touched, 2);
    // `foo` had only sess-1 → orphaned. `bar` had sess-2 left → not orphaned.
    assert_eq!(report.tools_left_orphaned, 1);
    assert!(reg.find_tool(&sid("sess-1"), &tid("foo")).is_none());
    assert!(reg.find_tool(&sid("sess-1"), &tid("bar")).is_none());
    assert!(reg.find_tool(&sid("sess-2"), &tid("bar")).is_some());
}

#[tokio::test]
async fn list_tools_filters_by_session() {
    let reg = MockRegistry::default();
    reg.install_handle(Arc::new(ErasedTool::new(StubTool { id: tid("foo") })));
    reg.install_handle(Arc::new(ErasedTool::new(StubTool { id: tid("bar") })));
    reg.register_tool(cid("c1"), build_registration(&tid("foo"), &[sid("sess-1")]))
        .await;
    reg.register_tool(cid("c1"), build_registration(&tid("bar"), &[sid("sess-2")]))
        .await;
    let s1 = reg.list_tools(&sid("sess-1"), &ToolDefinitionMode::Full);
    let s2 = reg.list_tools(&sid("sess-2"), &ToolDefinitionMode::Full);
    assert_eq!(s1.len(), 1);
    assert_eq!(s1[0].name, "foo");
    assert_eq!(s2.len(), 1);
    assert_eq!(s2[0].name, "bar");
}

#[tokio::test]
async fn list_servers_groups_by_owning_server() {
    let reg = MockRegistry::default();
    reg.install_handle(Arc::new(ErasedTool::new(StubTool { id: tid("foo") })));
    reg.register_tool(cid("c1"), build_registration(&tid("foo"), &[sid("sess-1")]))
        .await;
    let summaries = reg.list_servers(&sid("sess-1"));
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].tool_count(), 1);
    assert_eq!(summaries[0].tool_names[0], "foo");
}

#[tokio::test]
async fn search_returns_substring_matches() {
    let reg = MockRegistry::default();
    reg.install_handle(Arc::new(ErasedTool::new(StubTool { id: tid("foo") })));
    reg.install_handle(Arc::new(ErasedTool::new(StubTool { id: tid("foobar") })));
    reg.register_tool(cid("c1"), build_registration(&tid("foo"), &[sid("sess-1")]))
        .await;
    reg.register_tool(
        cid("c1"),
        build_registration(&tid("foobar"), &[sid("sess-1")]),
    )
    .await;
    let snap = reg.search(&sid("sess-1"), "foo", 10);
    assert_eq!(snap.results.len(), 2);
    assert!(snap.is_ready);
    assert_eq!(snap.total_hidden_tools, 0);
}

#[tokio::test]
async fn registry_drives_compound_resolver() {
    let registry = Arc::new(MockRegistry::default());
    registry.install_handle(Arc::new(ErasedTool::new(StubTool { id: tid("foo") })));
    registry
        .register_tool(cid("c1"), build_registration(&tid("foo"), &[sid("sess-1")]))
        .await;
    let resolver = CompoundResolver::local_only(registry as Arc<dyn ToolRegistry>);
    assert!(resolver.resolve(&sid("sess-1"), &tid("foo")).is_some());
    assert!(resolver.resolve(&sid("sess-1"), &tid("missing")).is_none());
}

#[tokio::test]
async fn bind_and_unbind_tool_session_round_trips_visibility() {
    let reg = MockRegistry::default();
    reg.install_handle(Arc::new(ErasedTool::new(StubTool { id: tid("foo") })));
    reg.register_tool(cid("c1"), build_registration(&tid("foo"), &[]))
        .await;
    // Empty sessions: tool is registered but unreachable.
    assert!(reg.find_tool(&sid("sess-1"), &tid("foo")).is_none());
    let outcome = reg
        .bind_tool_session(&cid("c1"), &tid("foo"), &sid("sess-1"))
        .await;
    assert_eq!(outcome, ToolSessionBindOutcome::Bound);
    assert!(reg.find_tool(&sid("sess-1"), &tid("foo")).is_some());
    let again = reg
        .bind_tool_session(&cid("c1"), &tid("foo"), &sid("sess-1"))
        .await;
    assert_eq!(again, ToolSessionBindOutcome::AlreadyBound);
    let unbind = reg
        .unbind_tool_session(&cid("c1"), &tid("foo"), &sid("sess-1"))
        .await;
    assert_eq!(unbind, ToolSessionUnbindOutcome::Unbound);
    assert!(reg.find_tool(&sid("sess-1"), &tid("foo")).is_none());
    let unknown = reg
        .bind_tool_session(&cid("c1"), &tid("missing"), &sid("sess-1"))
        .await;
    assert_eq!(unknown, ToolSessionBindOutcome::UnknownTool);
}

#[tokio::test]
async fn drop_connection_releases_every_owned_tool() {
    let reg = MockRegistry::default();
    reg.install_handle(Arc::new(ErasedTool::new(StubTool { id: tid("foo") })));
    reg.install_handle(Arc::new(ErasedTool::new(StubTool { id: tid("bar") })));
    reg.register_tool(cid("c1"), build_registration(&tid("foo"), &[sid("sess-1")]))
        .await;
    reg.register_tool(
        cid("c1"),
        build_registration(&tid("bar"), &[sid("sess-1"), sid("sess-2")]),
    )
    .await;
    let report = reg.drop_connection(&cid("c1")).await;
    assert_eq!(report.tools_dropped, 2);
    assert_eq!(report.session_bindings_cleared, 3);
    assert!(reg.find_tool(&sid("sess-1"), &tid("foo")).is_none());
    assert!(reg.find_tool(&sid("sess-2"), &tid("bar")).is_none());
    assert!(reg.tool_sessions(&cid("c1"), &tid("foo")).is_empty());
}
