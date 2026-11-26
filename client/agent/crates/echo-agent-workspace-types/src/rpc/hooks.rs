//! Wire mirror of the `workspace.hook_registry` response, kept byte-identical to
//! the upstream serde shape so this lean crate avoids the heavy `echo_agent_hooks` dep.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{RpcActivityClass, WorkspaceRpc};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HookRegistryReq {}

impl WorkspaceRpc for HookRegistryReq {
    const METHOD: &'static str = "workspace.hook_registry";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = HookRegistryWire;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HookRegistryWire {
    pub hooks: HashMap<HookEventNameWire, Vec<HookSpecWire>>,
}

/// Compiled `matcher` omitted; drift-guarded by `hook_spec_wire_covers_all_upstream_fields`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookSpecWire {
    pub name: String,
    pub event: HookEventNameWire,
    pub handler_type: String,
    pub configured_matcher: Option<String>,
    pub enabled: bool,
    pub command: Option<PathBuf>,
    pub command_raw: Option<String>,
    pub url: Option<String>,
    pub url_raw: Option<String>,
    pub timeout_ms: u64,
    pub source_dir: PathBuf,
    pub extra_env: HashMap<String, String>,
    /// `default` decodes a pre-field server as `file`.
    #[serde(default = "default_layer")]
    pub layer: String,
}

fn default_layer() -> String {
    "file".to_string()
}

/// Snake_case JSON map key; hand-written serde keeps an unknown event lossless in
/// [`Unknown`](Self::Unknown) so decode never fails under deploy skew.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HookEventNameWire {
    SessionStart,
    SessionEnd,
    Stop,
    StopFailure,
    StopCancelled,
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    PermissionDenied,
    UserPromptSubmit,
    Notification,
    SubagentStart,
    SubagentStop,
    SubagentEnd,
    PreCompact,
    PostCompact,
    Unknown(String),
}

impl HookEventNameWire {
    pub fn as_str(&self) -> &str {
        match self {
            Self::SessionStart => "session_start",
            Self::SessionEnd => "session_end",
            Self::Stop => "stop",
            Self::StopFailure => "stop_failure",
            Self::StopCancelled => "stop_cancelled",
            Self::PreToolUse => "pre_tool_use",
            Self::PostToolUse => "post_tool_use",
            Self::PostToolUseFailure => "post_tool_use_failure",
            Self::PermissionDenied => "permission_denied",
            Self::UserPromptSubmit => "user_prompt_submit",
            Self::Notification => "notification",
            Self::SubagentStart => "subagent_start",
            Self::SubagentStop => "subagent_stop",
            Self::SubagentEnd => "subagent_end",
            Self::PreCompact => "pre_compact",
            Self::PostCompact => "post_compact",
            Self::Unknown(s) => s,
        }
    }
}

impl Serialize for HookEventNameWire {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for HookEventNameWire {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(match s.as_str() {
            "session_start" => Self::SessionStart,
            "session_end" => Self::SessionEnd,
            "stop" => Self::Stop,
            "stop_failure" => Self::StopFailure,
            "stop_cancelled" => Self::StopCancelled,
            "pre_tool_use" => Self::PreToolUse,
            "post_tool_use" => Self::PostToolUse,
            "post_tool_use_failure" => Self::PostToolUseFailure,
            "permission_denied" => Self::PermissionDenied,
            "user_prompt_submit" => Self::UserPromptSubmit,
            "notification" => Self::Notification,
            "subagent_start" => Self::SubagentStart,
            "subagent_stop" => Self::SubagentStop,
            "subagent_end" => Self::SubagentEnd,
            "pre_compact" => Self::PreCompact,
            "post_compact" => Self::PostCompact,
            _ => Self::Unknown(s),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_constant() {
        assert_eq!(HookRegistryReq::METHOD, "workspace.hook_registry");
    }

    /// Mirrors `event_name_deser_all_variants` in echo-agent-hooks; the two lists move together.
    #[test]
    fn hook_event_name_wire_snake_case_round_trip() {
        for (variant, wire) in [
            (HookEventNameWire::SessionStart, "session_start"),
            (HookEventNameWire::SessionEnd, "session_end"),
            (HookEventNameWire::Stop, "stop"),
            (HookEventNameWire::StopFailure, "stop_failure"),
            (HookEventNameWire::StopCancelled, "stop_cancelled"),
            (HookEventNameWire::PreToolUse, "pre_tool_use"),
            (HookEventNameWire::PostToolUse, "post_tool_use"),
            (
                HookEventNameWire::PostToolUseFailure,
                "post_tool_use_failure",
            ),
            (HookEventNameWire::PermissionDenied, "permission_denied"),
            (HookEventNameWire::UserPromptSubmit, "user_prompt_submit"),
            (HookEventNameWire::Notification, "notification"),
            (HookEventNameWire::SubagentStart, "subagent_start"),
            (HookEventNameWire::SubagentStop, "subagent_stop"),
            (HookEventNameWire::SubagentEnd, "subagent_end"),
            (HookEventNameWire::PreCompact, "pre_compact"),
            (HookEventNameWire::PostCompact, "post_compact"),
        ] {
            assert_eq!(
                serde_json::to_value(&variant).unwrap(),
                serde_json::json!(wire)
            );
            let parsed: HookEventNameWire =
                serde_json::from_value(serde_json::json!(wire)).unwrap();
            assert_eq!(parsed, variant);
        }
    }

    #[test]
    fn hook_event_name_wire_unknown_round_trips_losslessly() {
        // A newer server's event must decode (not error) and preserve its raw
        // value so it stays a distinct map key.
        let v: HookEventNameWire =
            serde_json::from_value(serde_json::json!("future_event")).unwrap();
        assert_eq!(v, HookEventNameWire::Unknown("future_event".to_string()));
        assert_eq!(
            serde_json::to_value(&v).unwrap(),
            serde_json::json!("future_event")
        );
    }

    #[test]
    fn hook_registry_wire_round_trips_server_json() {
        // A representative server-side `HookRegistry` serialization.
        let json = serde_json::json!({
            "hooks": {
                "pre_tool_use": [{
                    "name": "global/safety",
                    "event": "pre_tool_use",
                    "handler_type": "command",
                    "configured_matcher": "Bash",
                    "enabled": true,
                    "command": "/bin/check.sh",
                    "command_raw": "${X}/check.sh",
                    "url": null,
                    "url_raw": null,
                    "timeout_ms": 5000,
                    "source_dir": "/home/u/.echo-agent/hooks",
                    "extra_env": { "FOO": "bar" },
                    "layer": "file"
                }]
            }
        });
        let wire: HookRegistryWire = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(&wire).unwrap(), json);
    }
}
