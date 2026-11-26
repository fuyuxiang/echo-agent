//! Discovery method (`workspace.discover_agents_md`).

use serde::{Deserialize, Serialize};

use super::{RpcActivityClass, WorkspaceRpc};

/// `workspace.discover_agents_md` — project-instruction files (AGENTS.md /
/// Claude.md / `.echo-agent/rules/*.md`) discovered from the workspace root up to
/// the git root, plus `~/.echo-agent` and compat dirs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiscoverAgentsMdReq {}

impl WorkspaceRpc for DiscoverAgentsMdReq {
    const METHOD: &'static str = "workspace.discover_agents_md";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = Vec<AgentConfigFile>;
}

/// Mirrors the serde shape of `echo-agent-core`'s `AgentConfigFile`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfigFile {
    pub file_name: String,
    pub file_path: String,
    pub content: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_constant() {
        assert_eq!(DiscoverAgentsMdReq::METHOD, "workspace.discover_agents_md");
    }

    #[test]
    fn agent_config_file_round_trips() {
        let raw = serde_json::json!({
            "file_name": "AGENTS.md",
            "file_path": "/repo/AGENTS.md",
            "content": "# Instructions\n",
        });
        let file: AgentConfigFile = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(file.file_name, "AGENTS.md");
        assert_eq!(file.file_path, "/repo/AGENTS.md");
        assert_eq!(serde_json::to_value(&file).unwrap(), raw);
    }

    #[test]
    fn agent_config_file_ignores_unknown_fields() {
        let raw = serde_json::json!({
            "file_name": "Claude.md",
            "file_path": "/repo/Claude.md",
            "content": "x",
            "brand_new_field": {"nested": true},
        });
        let file: AgentConfigFile = serde_json::from_value(raw).unwrap();
        assert_eq!(file.file_name, "Claude.md");
    }
}
