//! Shared types for relay session sharing.

use serde::{Deserialize, Serialize};

/// Distinguishes between local TUI agents and cloud-hosted agents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentType {
    Tui,
    Agent,
}

impl std::fmt::Display for AgentType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tui => write!(f, "tui"),
            Self::Agent => write!(f, "agent"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_type_serialization() {
        let tui = AgentType::Tui;
        let json = serde_json::to_string(&tui).unwrap();
        assert_eq!(json, "\"tui\"");

        let agent = AgentType::Agent;
        let json = serde_json::to_string(&agent).unwrap();
        assert_eq!(json, "\"agent\"");
    }

    #[test]
    fn test_agent_type_deserialization() {
        let tui: AgentType = serde_json::from_str("\"tui\"").unwrap();
        assert_eq!(tui, AgentType::Tui);

        let agent: AgentType = serde_json::from_str("\"agent\"").unwrap();
        assert_eq!(agent, AgentType::Agent);
    }

    #[test]
    fn test_agent_type_display() {
        assert_eq!(AgentType::Tui.to_string(), "tui");
        assert_eq!(AgentType::Agent.to_string(), "agent");
    }
}
