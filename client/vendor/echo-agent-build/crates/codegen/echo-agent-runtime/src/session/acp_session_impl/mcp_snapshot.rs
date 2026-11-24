//! MCP snapshot concern for `SessionActor`: server-snapshot refresh and
//! reminder scheduling, templated-prefix handshake waits, and tool
//! re-registration on a rebuilt bridge.

use super::*;

pub(super) const MCP_INIT_CANCELLED_CONFIG_CHANGED: &str = "config_changed";

impl McpReminderMode {
    pub(super) fn from_env() -> Self {
        match std::env::var("MCP_REMINDER_MODE")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "full" => Self::Full,
            _ => Self::Delta,
        }
    }
}

pub(super) fn gateway_tool_is_disabled(
    tool: &crate::session::managed_mcp::GatewayTool,
    disabled_gateway_tools: &std::collections::HashMap<String, std::collections::HashSet<String>>,
) -> bool {
    let qualified_name = tool.qualified_name();
    disabled_gateway_tools
        .get(crate::util::config::MANAGED_GATEWAY_DISABLED_CONNECTORS_KEY)
        .is_some_and(|set| set.contains(&tool.connector_id))
        || disabled_gateway_tools
            .get(&tool.connector_id)
            .is_some_and(|set| set.contains(&qualified_name))
}

pub(super) async fn refresh_mcp_snapshot_and_schedule_reminder_with(
    tool_bridge: Arc<crate::tools::bridge::ToolBridge>,
    mcp_state: Arc<TokioMutex<McpState>>,
    managed_mcp_handle: crate::session::managed_mcp::ManagedMcpStateHandle,
    tool_metadata_snapshot: Arc<std::sync::Mutex<crate::session::tool_index::ToolMetadataSnapshot>>,
    mcp_reminder_dirty: Arc<std::sync::atomic::AtomicBool>,
    mcp_initialized: bool,
    disabled_gateway_tools: &std::collections::HashMap<String, std::collections::HashSet<String>>,
    // External harness only: per-workspace `mcps/` descriptor root. `Some` makes
    // this refresh also update the on-disk descriptor mirror so late-connecting
    // servers become discoverable; `None` for other agent types (no-op).
    mcps_root: Option<std::path::PathBuf>,
) {
    use crate::session::tool_index::{
        ServerMetadata, ToolMetadata, extract_parameter_names, split_qualified_name,
    };

    let all_defs = tool_bridge.tool_definitions().await;
    let mut seen_tools = std::collections::HashSet::new();
    let mut mcp_tools: Vec<ToolMetadata> = all_defs
        .iter()
        .filter(|d| d.function.name.contains("__"))
        .filter(|d| seen_tools.insert(d.function.name.clone()))
        .map(|d| {
            let (server, tool) = split_qualified_name(&d.function.name);
            ToolMetadata {
                qualified_name: d.function.name.clone(),
                server_name: server.to_string(),
                tool_name: tool.to_string(),
                description: d.function.description.clone().unwrap_or_default(),
                parameters: extract_parameter_names(&d.function.parameters),
                input_schema: d.function.parameters.clone(),
            }
        })
        .collect();

    let (gateway_catalog, mut gateway_connectors) = {
        let state = managed_mcp_handle.lock().await;
        let catalog = if state.gateway_tools_active {
            match &state.gateway_tool_cache {
                crate::session::managed_mcp::GatewayToolCatalogCache::Ready(catalog) => {
                    Some(catalog.clone())
                }
                _ => None,
            }
        } else {
            None
        };
        let connectors: Vec<String> = state.gateway_tool_connectors_seen.iter().cloned().collect();
        (catalog, connectors)
    };

    if let Some(catalog) = gateway_catalog.as_ref() {
        gateway_connectors.extend(catalog.tools.iter().map(|tool| tool.connector_id.clone()));
    }
    gateway_connectors.sort_unstable();
    gateway_connectors.dedup();
    let mut gateway_resource_entries = Vec::new();
    if let Some(catalog) = gateway_catalog.as_ref() {
        for tool in &catalog.tools {
            let qualified_name = tool.qualified_name();
            if gateway_tool_is_disabled(tool, disabled_gateway_tools) {
                continue;
            }
            if !seen_tools.insert(qualified_name.clone()) {
                continue;
            }
            gateway_resource_entries.push((
                qualified_name.clone(),
                echo_agent_tools::types::resources::ManagedGatewayToolSource {
                    connector_id: tool.connector_id.clone(),
                    connector_name: tool.connector_name.clone(),
                    tool_id: tool.tool_id.clone(),
                    tool_name: tool.tool_name.clone(),
                    call_id: tool.call_id.clone(),
                    input_schema: tool.json_schema.clone(),
                },
            ));
            // Gateway ids are the model/search contract. Display labels stay
            // out of ToolMetadata so search_tool and permissions use stable ids:
            // connector_id/tool_id here, connector_name/tool_name in UI only.
            mcp_tools.push(ToolMetadata {
                qualified_name,
                server_name: tool.connector_id.clone(),
                tool_name: tool.tool_id.clone(),
                description: tool.description.clone(),
                parameters: extract_parameter_names(&tool.json_schema),
                input_schema: tool.json_schema.clone(),
            });
        }
    }

    let servers_with_tools: std::collections::HashSet<&str> =
        mcp_tools.iter().map(|t| t.server_name.as_str()).collect();

    let server_metadata: Vec<ServerMetadata> = {
        let mcp_state = mcp_state.lock().await;
        let mut metadata = Vec::new();
        for (name, client) in mcp_state.all_clients() {
            if servers_with_tools.contains(name.as_str()) {
                metadata.push(ServerMetadata {
                    name: name.clone(),
                    description: client.server_instructions().await,
                });
            }
        }
        metadata
    };

    // Scope the synchronous snapshot guard so it is released before the
    // descriptor-materialization await below (a std `Mutex` guard must not
    // cross an await point).
    {
        let mut snapshot = tool_metadata_snapshot.lock().unwrap();
        snapshot.tools = mcp_tools;
        snapshot.servers = server_metadata;
        snapshot.mcp_initialized = mcp_initialized;
    }

    tool_bridge
        .update_resource(
            echo_agent_tools::types::resources::ManagedGatewayToolCatalog(
                gateway_resource_entries.into_iter().collect(),
            ),
        )
        .await;

    mcp_reminder_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
    tracing::debug!("MCP snapshot updated, reminder marked dirty");

    // External harness: refresh the on-disk descriptor mirror for the current
    // clients, so a server that connected after the first-turn prefix was built
    // is still discoverable. Snapshot under the lock, then materialize outside.
    if let Some(mcps_root) = mcps_root {
        let clients: Vec<(String, Arc<crate::session::mcp_servers::McpClient>)> = {
            let state = mcp_state.lock().await;
            state
                .all_clients()
                .map(|(n, c)| (n.clone(), Arc::clone(c)))
                .collect()
        };
        let protected_connectors = clients.iter().map(|(name, _)| name.clone()).collect();
        let mut gateway_descriptors = Vec::new();
        if let Some(catalog) = gateway_catalog.as_ref() {
            for tool in &catalog.tools {
                if gateway_tool_is_disabled(tool, disabled_gateway_tools) {
                    continue;
                }
                gateway_descriptors.push(crate::session::mcp_descriptors::GatewayToolDescriptor {
                    connector_id: tool.connector_id.clone(),
                    tool_id: tool.tool_id.clone(),
                    description: tool.description.clone(),
                    json_schema: tool.json_schema.clone(),
                });
            }
        }
        crate::session::mcp_descriptors::materialize_descriptors_for_clients(&mcps_root, clients)
            .await;
        crate::session::mcp_descriptors::materialize_descriptors_for_gateway_tools(
            &mcps_root,
            gateway_descriptors,
            gateway_connectors,
            protected_connectors,
        )
        .await;
    }
}

#[cfg(test)]
pub(crate) async fn refresh_mcp_snapshot_for_test(
    tool_bridge: Arc<crate::tools::bridge::ToolBridge>,
    mcp_state: Arc<TokioMutex<McpState>>,
    managed_mcp_handle: crate::session::managed_mcp::ManagedMcpStateHandle,
    tool_metadata_snapshot: Arc<std::sync::Mutex<crate::session::tool_index::ToolMetadataSnapshot>>,
) {
    refresh_mcp_snapshot_for_test_with_disabled(
        tool_bridge,
        mcp_state,
        managed_mcp_handle,
        tool_metadata_snapshot,
        &Default::default(),
    )
    .await;
}

#[cfg(test)]
pub(crate) async fn refresh_mcp_snapshot_for_test_with_disabled(
    tool_bridge: Arc<crate::tools::bridge::ToolBridge>,
    mcp_state: Arc<TokioMutex<McpState>>,
    managed_mcp_handle: crate::session::managed_mcp::ManagedMcpStateHandle,
    tool_metadata_snapshot: Arc<std::sync::Mutex<crate::session::tool_index::ToolMetadataSnapshot>>,
    disabled_gateway_tools: &std::collections::HashMap<String, std::collections::HashSet<String>>,
) {
    refresh_mcp_snapshot_and_schedule_reminder_with(
        tool_bridge,
        mcp_state,
        managed_mcp_handle,
        tool_metadata_snapshot,
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
        false,
        disabled_gateway_tools,
        None,
    )
    .await;
}

impl SessionActor {
    /// Block the templated user prefix until MCP handshakes have finished populating
    /// [`McpState::clients`], or until a timeout elapses.
    ///
    /// [`gather_mcp_servers`](Self::gather_mcp_servers) only sees entries in `clients`.
    /// [`McpState::finish_init`](crate::session::mcp_servers::McpState::finish_init) runs
    /// immediately after spawning MCP processes, while tool registration and `clients`
    /// insertion happen in a follow-on background task; [`McpState::initializing_servers`]
    /// stays non-empty until that task completes.
    pub(super) async fn wait_for_mcp_templated_prefix_ready(
        &self,
        template: &echo_agent_core::prompt::user_message::UserMessageTemplate,
    ) {
        use echo_agent_core::prompt::user_message::UserMessageTemplate;
        if matches!(template, UserMessageTemplate::Default) {
            return;
        }

        // Register the notification future *before* checking state so we
        // cannot miss a signal that fires between releasing the lock and
        // entering the wait.
        let notified = self.mcp_handshakes_done.notified();
        tokio::pin!(notified);

        let (configs_empty, already_ready, initializing_count, client_count, finished_init) = {
            let s = self.mcp_state.lock().await;
            (
                s.configs.is_empty(),
                s.is_initialized(),
                s.handshaking_servers_count(),
                s.owned_clients.len() + s.shared_clients.len(),
                s.has_finished_init(),
            )
        };
        if configs_empty {
            tracing::debug!(
                session_id = %self.session_info.id.0,
                "wait_for_mcp_templated_prefix_ready: no MCP configs, skipping"
            );
            return;
        }
        if already_ready {
            tracing::debug!(
                session_id = %self.session_info.id.0,
                client_count,
                "wait_for_mcp_templated_prefix_ready: already ready, skipping"
            );
            return;
        }

        const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
        let start = std::time::Instant::now();
        tracing::info!(
            session_id = %self.session_info.id.0,
            initializing_count,
            client_count,
            finished_init,
            timeout_ms = TIMEOUT.as_millis() as u64,
            "wait_for_mcp_templated_prefix_ready: waiting for MCP handshakes"
        );

        // Wait for `mcp_handshakes_done` (signalled after
        // `mark_all_servers_ready()` in the bg handshake task) or the
        // timeout, whichever comes first.
        let outcome = tokio::select! {
            () = &mut notified => "notified",
            () = tokio::time::sleep(TIMEOUT) => "timed_out",
        };

        // Only acquire the post-wait snapshot when INFO tracing is active;
        // in production the extra lock + string cloning is unnecessary.
        if tracing::enabled!(tracing::Level::INFO) {
            let s = self.mcp_state.lock().await;
            tracing::info!(
                session_id = %self.session_info.id.0,
                outcome,
                elapsed_ms = start.elapsed().as_millis() as u64,
                final_initializing = s.handshaking_servers_count(),
                final_clients = s.owned_clients.len() + s.shared_clients.len(),
                final_finished_init = s.has_finished_init(),
                final_initializing_names = ?s.handshaking_servers_iter().cloned().collect::<Vec<_>>(),
                final_client_names = ?s.all_clients().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
                "wait_for_mcp_templated_prefix_ready: done"
            );
        }
    }

    pub(super) async fn wait_for_mcp_handshakes_bounded(&self, timeout: std::time::Duration) {
        let notified = self.mcp_handshakes_done.notified();
        tokio::pin!(notified);

        let (configs_empty, already_ready) = {
            let s = self.mcp_state.lock().await;
            (s.configs.is_empty(), s.is_initialized())
        };
        if configs_empty || already_ready {
            return;
        }

        let start = std::time::Instant::now();
        tracing::info!(
            session_id = %self.session_info.id.0,
            timeout_ms = timeout.as_millis() as u64,
            "wait_for_mcp_handshakes_bounded: waiting"
        );

        let outcome = tokio::select! {
            () = &mut notified => "notified",
            () = tokio::time::sleep(timeout) => "timed_out",
        };

        tracing::info!(
            session_id = %self.session_info.id.0,
            outcome,
            elapsed_ms = start.elapsed().as_millis() as u64,
            "wait_for_mcp_handshakes_bounded: done"
        );
    }

    /// Re-register MCP tools onto a freshly-built `ToolBridge` after a
    /// zero-turn harness rebuild.
    ///
    /// Snapshots the live MCP `Client` connections from `mcp_state` and
    /// (eventually) re-walks each client's `list_tools` to mirror its
    /// tool registrations onto the new bridge. Best-effort: per-server
    /// failures are logged but do not abort the rebuild.
    ///
    /// Re-register MCP tools from existing clients onto the rebuilt bridge.
    ///
    /// Iterates over all connected MCP clients, calls `list_tools` on each
    /// to obtain tool registrations, and registers them on the new bridge.
    /// Errors on individual servers are logged but don't abort the process.
    /// After re-registration, refreshes the tool metadata snapshot so
    /// `search_tool` returns accurate results.
    pub(super) async fn re_register_mcp_tools_on_rebuilt_bridge(&self) {
        // Snapshot server names + client Arcs to avoid holding the lock
        // across async list_tools calls.
        let clients: Vec<(
            String,
            std::sync::Arc<crate::session::mcp_servers::McpClient>,
        )> = {
            let st = self.mcp_state.lock().await;
            st.all_clients()
                .map(|(name, client)| (name.clone(), std::sync::Arc::clone(client)))
                .collect()
        };

        if clients.is_empty() {
            self.refresh_mcp_snapshot_and_schedule_reminder().await;
            return;
        }

        tracing::info!(
            session_id = %self.session_info.id.0,
            server_count = clients.len(),
            "re_register_mcp_tools_on_rebuilt_bridge: re-registering MCP tools from existing clients"
        );

        let mcp_state_arc = std::sync::Arc::clone(&self.mcp_state);
        let mut all_ui_tools: std::collections::HashMap<
            String,
            Vec<crate::extensions::mcp::McpToolEntry>,
        > = std::collections::HashMap::new();

        for (server_name, client) in &clients {
            let registrations = match client
                .get_tool_registrations(std::sync::Arc::clone(&mcp_state_arc))
                .await
            {
                Ok(regs) => regs,
                Err(e) => {
                    tracing::warn!(
                        session_id = %self.session_info.id.0,
                        server = %server_name,
                        error = %e,
                        "re_register_mcp_tools_on_rebuilt_bridge: failed to list tools, skipping server"
                    );
                    continue;
                }
            };

            let tool_count = registrations.len();
            let mut mcp_state = self.mcp_state.lock().await;

            for reg in registrations {
                self.register_mcp_tool(server_name, reg, &mut mcp_state, &mut all_ui_tools)
                    .await;
            }
            drop(mcp_state);

            tracing::info!(
                session_id = %self.session_info.id.0,
                server = %server_name,
                tool_count,
                "re_register_mcp_tools_on_rebuilt_bridge: re-registered tools"
            );
        }

        // Refresh the snapshot so search_tool returns accurate results
        // against the newly-registered tools.
        self.refresh_mcp_snapshot_and_schedule_reminder().await;
        self.emit_mcp_tools_changed_notifications(all_ui_tools);
    }
}
