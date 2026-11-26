//! MCP descriptor mirror.
//!
//! Some templates read MCP metadata from an on-disk descriptor tree. Keep that
//! tree current as servers connect by (re)writing descriptors for connected
//! servers on every MCP tool-set change, not just at the first turn.
//!
//! Local MCP writes are upsert-only — folders for servers removed mid-session are
//! not pruned (cleaned on the next session's first-turn build); pruning against
//! an async-changing client set risks deleting a just-connected server's folder.
//! Managed gateway writes converge to the admitted gateway catalog so disabled
//! gateway tools are not discoverable from stale descriptor files.
//!
//! Owning the descriptor I/O here keeps `acp_session.rs` thin.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::session::mcp_servers::{McpClient, sanitize_descriptor_segment};

#[derive(Debug, Clone)]
pub(crate) struct GatewayToolDescriptor {
    pub(crate) connector_id: String,
    pub(crate) tool_id: String,
    pub(crate) description: String,
    pub(crate) json_schema: serde_json::Value,
}

/// Per-server descriptor folder: `<mcps_root>/<sanitized server name>`. Uses the
/// sanitizer shared with `echo-agent-mcp` so the advertised folder matches disk.
pub(crate) fn server_descriptor_dir(mcps_root: &Path, server_name: &str) -> PathBuf {
    mcps_root.join(sanitize_descriptor_segment(server_name))
}

/// Upsert the on-disk tool descriptors for the given connected clients.
///
/// Safe to run concurrently (the first-turn build and the background handshake
/// task can both call it): `materialize_descriptors` writes each file
/// atomically, so overlapping writers converge without a lock. Errors are
/// logged, not propagated.
pub(crate) async fn materialize_descriptors_for_clients(
    mcps_root: &Path,
    clients: Vec<(String, Arc<McpClient>)>,
) {
    for (name, client) in clients {
        let server_dir = server_descriptor_dir(mcps_root, &name);
        if let Err(e) = client.materialize_descriptors(&server_dir).await {
            tracing::warn!(
                server = %name,
                path = %server_dir.display(),
                error = %e,
                "failed to materialize MCP descriptors",
            );
        }
    }
}

pub(crate) async fn materialize_descriptors_for_gateway_tools(
    mcps_root: &Path,
    tools: Vec<GatewayToolDescriptor>,
    gateway_connectors: Vec<String>,
    protected_connectors: HashSet<String>,
) {
    let mut files_by_connector: BTreeMap<String, Vec<(String, Vec<u8>)>> = BTreeMap::new();
    for tool in tools {
        let descriptor = serde_json::json!({
            "name": tool.tool_id,
            "description": tool.description,
            "inputSchema": tool.json_schema,
        });
        match serde_json::to_vec_pretty(&descriptor) {
            Ok(bytes) => {
                let file_name = format!("{}.json", sanitize_descriptor_segment(&tool.tool_id));
                files_by_connector
                    .entry(tool.connector_id)
                    .or_default()
                    .push((file_name, bytes));
            }
            Err(e) => tracing::warn!(
                connector = %tool.connector_id,
                tool = %tool.tool_id,
                error = %e,
                "failed to serialize managed gateway tool descriptor",
            ),
        }
    }

    let connectors: BTreeSet<String> = gateway_connectors.into_iter().collect();

    let mcps_root = mcps_root.to_path_buf();
    if let Err(e) = tokio::task::spawn_blocking(move || {
        for connector_id in connectors {
            let server_dir = server_descriptor_dir(&mcps_root, &connector_id);
            let tools_dir = server_dir.join("tools");
            let files = files_by_connector.remove(&connector_id).unwrap_or_default();
            let admitted: BTreeSet<String> = files.iter().map(|(name, _)| name.clone()).collect();
            let protected = protected_connectors.contains(&connector_id);

            if protected {
                if let Err(e) = remove_gateway_owned_tool_descriptors(&tools_dir)
                    && e.kind() != std::io::ErrorKind::NotFound
                {
                    tracing::warn!(
                        connector = %connector_id,
                        path = %tools_dir.display(),
                        error = %e,
                        "failed to remove managed gateway descriptors from protected connector dir",
                    );
                }
                continue;
            }

            if files.is_empty() {
                if let Err(e) = std::fs::remove_dir_all(&server_dir)
                    && e.kind() != std::io::ErrorKind::NotFound
                {
                    tracing::warn!(
                        connector = %connector_id,
                        path = %server_dir.display(),
                        error = %e,
                        "failed to remove stale managed gateway descriptor dir",
                    );
                }
                continue;
            }

            if let Err(e) = std::fs::create_dir_all(&tools_dir) {
                tracing::warn!(
                    connector = %connector_id,
                    path = %tools_dir.display(),
                    error = %e,
                    "failed to create managed gateway tools descriptor dir",
                );
                continue;
            }

            for (file_name, bytes) in files {
                let path = tools_dir.join(&file_name);
                let write_result =
                    tempfile::NamedTempFile::new_in(&tools_dir).and_then(|mut tmp| {
                        std::io::Write::write_all(&mut tmp, &bytes)?;
                        tmp.persist(&path).map_err(|e| e.error)
                    });
                if let Err(e) = write_result {
                    tracing::warn!(
                        connector = %connector_id,
                        path = %path.display(),
                        error = %e,
                        "failed to write managed gateway tool descriptor",
                    );
                }
            }

            prune_stale_gateway_tool_descriptors(&tools_dir, &admitted, &connector_id);
        }
    })
    .await
    {
        tracing::warn!(
            error = %e,
            "managed gateway descriptor write task panicked",
        );
    }
}

fn remove_gateway_owned_tool_descriptors(tools_dir: &Path) -> std::io::Result<()> {
    let entries = std::fs::read_dir(tools_dir)?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        if is_gateway_owned_descriptor(&path) {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

fn is_gateway_owned_descriptor(path: &Path) -> bool {
    let Ok(contents) = std::fs::read(path) else {
        return false;
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&contents) else {
        return false;
    };
    value
        .get("x-echo-agent-managed-gateway")
        .and_then(|v| v.as_bool())
        == Some(true)
}

fn prune_stale_gateway_tool_descriptors(
    tools_dir: &Path,
    admitted: &BTreeSet<String>,
    connector_id: &str,
) {
    let entries = match std::fs::read_dir(tools_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            tracing::warn!(
                connector = %connector_id,
                path = %tools_dir.display(),
                error = %e,
                "failed to read managed gateway tools descriptor dir for pruning",
            );
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if admitted.contains(file_name) {
            continue;
        }
        if let Err(e) = std::fs::remove_file(&path) {
            tracing::warn!(
                connector = %connector_id,
                path = %path.display(),
                error = %e,
                "failed to remove stale managed gateway tool descriptor",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_replaces_unsafe_chars_and_never_empty() {
        assert_eq!(
            sanitize_descriptor_segment("echo_agent_com_linear"),
            "echo_agent_com_linear"
        );
        assert_eq!(sanitize_descriptor_segment("a/b:c d"), "a_b_c_d");
        assert_eq!(sanitize_descriptor_segment(""), "_");
        assert_eq!(sanitize_descriptor_segment("keep-1.2_x"), "keep-1.2_x");
    }

    #[test]
    fn server_dir_is_joined_under_root() {
        let root = Path::new("/home/u/.echo-agent/projects/enc/mcps");
        assert_eq!(server_descriptor_dir(root, "vercel"), root.join("vercel"));
    }

    #[tokio::test]
    async fn materializes_gateway_tool_descriptors_by_connector_and_tool() {
        let temp = tempfile::tempdir().unwrap();
        materialize_descriptors_for_gateway_tools(
            temp.path(),
            vec![
                GatewayToolDescriptor {
                    connector_id: "linear".into(),
                    tool_id: "list_issues".into(),
                    description: "List issues".into(),
                    json_schema: serde_json::json!({
                        "type": "object",
                        "properties": {"limit": {"type": "number"}}
                    }),
                },
                GatewayToolDescriptor {
                    connector_id: "slack/team".into(),
                    tool_id: "search messages".into(),
                    description: "Search Slack".into(),
                    json_schema: serde_json::json!({
                        "type": "object",
                        "properties": {"query": {"type": "string"}}
                    }),
                },
            ],
            vec!["linear".into(), "slack/team".into()],
            HashSet::new(),
        )
        .await;

        let linear: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(temp.path().join("linear/tools/list_issues.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(linear["name"], "list_issues");
        assert_eq!(linear["description"], "List issues");
        assert_eq!(
            linear["inputSchema"]["properties"]["limit"]["type"],
            "number"
        );

        let slack: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(temp.path().join("slack_team/tools/search_messages.json"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(slack["name"], "search messages");
        assert_eq!(slack["description"], "Search Slack");
        assert_eq!(
            slack["inputSchema"]["properties"]["query"]["type"],
            "string"
        );
    }

    #[tokio::test]
    async fn gateway_descriptor_collision_prunes_only_gateway_owned_files() {
        let temp = tempfile::tempdir().unwrap();
        let tools_dir = temp.path().join("linear/tools");
        std::fs::create_dir_all(&tools_dir).unwrap();
        std::fs::write(
            tools_dir.join("local_tool.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "name": "local_tool",
                "description": "Local tool",
                "inputSchema": {"type": "object"}
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            tools_dir.join("gateway_tool.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "name": "gateway_tool",
                "description": "Gateway tool",
                "inputSchema": {"type": "object"},
                "x-echo-agent-managed-gateway": true,
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(tools_dir.join("gateway_tool.json").exists());

        materialize_descriptors_for_gateway_tools(
            temp.path(),
            Vec::new(),
            vec!["linear".into()],
            HashSet::from(["linear".into()]),
        )
        .await;
        assert!(tools_dir.join("local_tool.json").exists());
        assert!(!tools_dir.join("gateway_tool.json").exists());
    }

    #[tokio::test]
    async fn protected_gateway_connector_skips_writing_into_local_server_dir() {
        let temp = tempfile::tempdir().unwrap();
        materialize_descriptors_for_gateway_tools(
            temp.path(),
            vec![GatewayToolDescriptor {
                connector_id: "linear".into(),
                tool_id: "gateway_tool".into(),
                description: "Gateway tool".into(),
                json_schema: serde_json::json!({"type": "object"}),
            }],
            vec!["linear".into()],
            HashSet::from(["linear".into()]),
        )
        .await;
        assert!(!temp.path().join("linear/tools/gateway_tool.json").exists());
    }

    #[tokio::test]
    async fn prunes_stale_gateway_tool_descriptors() {
        let temp = tempfile::tempdir().unwrap();
        materialize_descriptors_for_gateway_tools(
            temp.path(),
            vec![
                GatewayToolDescriptor {
                    connector_id: "linear".into(),
                    tool_id: "list_issues".into(),
                    description: "List issues".into(),
                    json_schema: serde_json::json!({"type": "object"}),
                },
                GatewayToolDescriptor {
                    connector_id: "slack".into(),
                    tool_id: "search".into(),
                    description: "Search Slack".into(),
                    json_schema: serde_json::json!({"type": "object"}),
                },
            ],
            vec!["linear".into(), "slack".into()],
            HashSet::new(),
        )
        .await;
        assert!(temp.path().join("linear/tools/list_issues.json").exists());
        assert!(temp.path().join("slack/tools/search.json").exists());

        materialize_descriptors_for_gateway_tools(
            temp.path(),
            vec![GatewayToolDescriptor {
                connector_id: "linear".into(),
                tool_id: "list_issues".into(),
                description: "List issues".into(),
                json_schema: serde_json::json!({"type": "object"}),
            }],
            vec!["linear".into(), "slack".into()],
            HashSet::new(),
        )
        .await;
        assert!(temp.path().join("linear/tools/list_issues.json").exists());
        assert!(!temp.path().join("slack/tools/search.json").exists());
        assert!(!temp.path().join("slack").exists());

        materialize_descriptors_for_gateway_tools(
            temp.path(),
            Vec::new(),
            vec!["linear".into()],
            HashSet::new(),
        )
        .await;
        assert!(!temp.path().join("linear").exists());
    }
}
