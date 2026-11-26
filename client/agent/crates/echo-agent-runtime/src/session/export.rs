//! Session export for sharing via the remote session-sharing backend.
//!
//! Uses `updates.jsonl` (ACP SessionNotifications) as the source of truth,
//! not `chat_history.jsonl` which is only for LLM API calls.

use crate::session::info::Info;
use crate::session::persistence::Summary;
use crate::session::storage::{JsonlStorageAdapter, PersistedData, SessionUpdate, StorageAdapter};
use agent_client_protocol as acp;
use serde::{Deserialize, Serialize};

/// JSON-RPC wrapper for ACP notifications.
#[derive(Debug, Serialize)]
struct AcpJsonRpcNotification<'a> {
    method: &'static str,
    params: &'a acp::SessionNotification,
}

/// JSON-RPC wrapper for EchoAgent extension notifications.
#[derive(Debug, Serialize)]
struct EchoAgentJsonRpcNotification<'a> {
    method: &'static str,
    params: &'a crate::extensions::notification::SessionNotification,
}

const ACP_SESSION_UPDATE_METHOD: &str = "session/update";
const ECHO_AGENT_SESSION_UPDATE_METHOD: &str = "_echo.agent/session/update";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedMessage {
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

impl ExportedMessage {
    pub fn from_notification(notification: &acp::SessionNotification) -> Self {
        let wrapper = AcpJsonRpcNotification {
            method: ACP_SESSION_UPDATE_METHOD,
            params: notification,
        };
        let content = serde_json::to_string(&wrapper).unwrap_or_else(|_| "{}".to_string());

        let timestamp = notification
            .meta
            .as_ref()
            .and_then(|m| m.get("timestamp"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Self { content, timestamp }
    }

    pub(crate) fn from_echo_agent_notification(
        notification: &crate::extensions::notification::SessionNotification,
    ) -> Self {
        let wrapper = EchoAgentJsonRpcNotification {
            method: ECHO_AGENT_SESSION_UPDATE_METHOD,
            params: notification,
        };
        let content = serde_json::to_string(&wrapper).unwrap_or_else(|_| "{}".to_string());

        let timestamp = notification
            .meta
            .as_ref()
            .and_then(|m| m.get("timestamp"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Self { content, timestamp }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_messages: Option<usize>,
    /// Parent session ID if this session was forked from another session
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,

    // --- Subagent-specific fields (all optional for backward compatibility) ---
    /// Session kind: "parent", "subagent", or "subagent_fork".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_kind: Option<String>,
    /// Subagent type (e.g., "general-purpose", "explore", "plan").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_type: Option<String>,
    /// Named persona applied to this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_persona: Option<String>,
    /// Named role applied to this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_role: Option<String>,
    /// Effective context source ("new" or "resumed").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_context_source: Option<String>,
    /// Subagent nesting depth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_depth: Option<u32>,
    /// Whether `title` was set by a manual rename. Omitted when `None`
    /// (`skip_serializing_if = "Option::is_none"`). Producers write
    /// `Some(true)` via `then_some(true)` / `manual_title_opt()`, and
    /// `ClearTitle` writes `Some(false)` so a merge-style backend drops
    /// a prior pin. Auto `SetTitle` still omits the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_is_manual: Option<bool>,
}

impl ExportedMetadata {
    /// Build metadata from a [`Summary`].
    pub(crate) fn from_summary(summary: &Summary) -> Self {
        Self {
            title: summary.display_title_opt(),
            cwd: summary.info.cwd.clone(),
            model_id: Some(summary.current_model_id.0.to_string()),
            created_at: Some(summary.created_at.to_rfc3339()),
            updated_at: Some(summary.updated_at.to_rfc3339()),
            total_messages: Some(summary.num_messages),
            parent_session_id: summary.parent_session_id.clone(),
            session_kind: None,
            subagent_type: None,
            subagent_persona: None,
            subagent_role: None,
            fork_context_source: None,
            subagent_depth: None,
            title_is_manual: summary.manual_title_opt().is_some().then_some(true),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedSession {
    pub session_id: String,
    pub messages: Vec<ExportedMessage>,
    pub metadata: ExportedMetadata,
}

impl ExportedSession {
    pub(crate) fn from_persisted_data(info: &Info, data: &PersistedData) -> Self {
        let messages = Self::convert_updates(&data.updates);
        let metadata = ExportedMetadata::from_summary(&data.summary);

        Self {
            session_id: info.id.to_string(),
            messages,
            metadata,
        }
    }

    pub(crate) async fn from_local_session(info: &Info) -> std::io::Result<Self> {
        let storage = JsonlStorageAdapter::new();
        let data = storage.load_session(info).await?;
        Ok(Self::from_persisted_data(info, &data))
    }

    fn convert_updates(updates: &[SessionUpdate]) -> Vec<ExportedMessage> {
        updates
            .iter()
            .map(|update| match update {
                SessionUpdate::Acp(notification) => {
                    ExportedMessage::from_notification(notification)
                }
                SessionUpdate::EchoAgent(notification) => {
                    ExportedMessage::from_echo_agent_notification(notification)
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod from_summary_tests {
    use super::*;
    use crate::session::info::Info;
    use crate::session::persistence::Summary;

    #[test]
    fn from_summary_title_uses_display_title_not_stale_session_summary() {
        let info = Info {
            id: acp::SessionId::new("export-title"),
            cwd: "/tmp".into(),
        };
        let mut summary = Summary::new(&info, acp::ModelId::new("test-model")).unwrap();
        summary.session_summary = "stale auto title".into();
        summary.generated_title = Some("Manual rename".into());
        summary.title_is_manual = true;

        let meta = ExportedMetadata::from_summary(&summary);
        assert_eq!(
            meta.title.as_deref(),
            Some("Manual rename"),
            "export title must follow display_title() (generated_title), not session_summary"
        );
    }

    #[test]
    fn from_summary_auto_generated_title_wins_over_session_summary() {
        let info = Info {
            id: acp::SessionId::new("export-auto"),
            cwd: "/tmp".into(),
        };
        let mut summary = Summary::new(&info, acp::ModelId::new("test-model")).unwrap();
        summary.session_summary = "first prompt fallback".into();
        summary.generated_title = Some("Auto".into());
        summary.title_is_manual = false;
        assert_eq!(
            ExportedMetadata::from_summary(&summary).title.as_deref(),
            Some("Auto")
        );
    }

    #[test]
    fn from_summary_falls_back_to_session_summary_when_generated_absent() {
        let info = Info {
            id: acp::SessionId::new("export-fallback"),
            cwd: "/tmp".into(),
        };
        let mut summary = Summary::new(&info, acp::ModelId::new("test-model")).unwrap();
        summary.session_summary = "fallback".into();
        summary.generated_title = None;
        assert_eq!(
            ExportedMetadata::from_summary(&summary).title.as_deref(),
            Some("fallback")
        );
    }

    #[test]
    fn from_summary_blank_titles_export_none() {
        let info = Info {
            id: acp::SessionId::new("export-blank"),
            cwd: "/tmp".into(),
        };
        let mut summary = Summary::new(&info, acp::ModelId::new("test-model")).unwrap();
        summary.session_summary = "  ".into();
        summary.generated_title = Some("".into());
        assert_eq!(ExportedMetadata::from_summary(&summary).title, None);
    }

    #[test]
    fn from_summary_title_is_manual_true_when_manual() {
        let info = Info {
            id: acp::SessionId::new("export-manual-flag"),
            cwd: "/tmp".into(),
        };
        let mut summary = Summary::new(&info, acp::ModelId::new("test-model")).unwrap();
        summary.generated_title = Some("Pinned".into());
        summary.title_is_manual = true;
        assert_eq!(
            ExportedMetadata::from_summary(&summary).title_is_manual,
            Some(true)
        );
    }

    #[test]
    fn from_summary_omits_stale_manual_flag_over_blank_generated_title() {
        let info = Info {
            id: acp::SessionId::new("export-stale-flag"),
            cwd: "/tmp".into(),
        };
        let mut summary = Summary::new(&info, acp::ModelId::new("test-model")).unwrap();
        summary.session_summary = "auto first-prompt summary".into();
        summary.generated_title = Some("   ".into());
        summary.title_is_manual = true;
        let meta = ExportedMetadata::from_summary(&summary);
        assert!(
            summary.manual_title_opt().is_none(),
            "local contract: stale flag is not a manual title"
        );
        assert_eq!(
            meta.title_is_manual, None,
            "stale flag must not be exported"
        );
        assert_eq!(
            meta.title.as_deref(),
            Some("auto first-prompt summary"),
            "display fallback still exports as title text"
        );
    }

    #[test]
    fn title_is_manual_omitted_when_false_or_none() {
        let info = Info {
            id: acp::SessionId::new("export-flag-omit"),
            cwd: "/tmp".into(),
        };
        let mut summary = Summary::new(&info, acp::ModelId::new("test-model")).unwrap();
        summary.generated_title = Some("Auto".into());
        summary.title_is_manual = false;
        let meta = ExportedMetadata::from_summary(&summary);
        assert_eq!(meta.title_is_manual, None);
        let json = serde_json::to_value(&meta).unwrap();
        assert!(
            json.get("title_is_manual").is_none(),
            "false/None must omit the field for wire stability: {json}"
        );

        let mut none_flag = meta.clone();
        none_flag.title_is_manual = None;
        let json_none = serde_json::to_value(&none_flag).unwrap();
        assert!(json_none.get("title_is_manual").is_none());

        let mut explicit_false = meta;
        explicit_false.title_is_manual = Some(false);
        let json_false = serde_json::to_value(&explicit_false).unwrap();
        assert_eq!(
            json_false.get("title_is_manual"),
            Some(&serde_json::json!(false))
        );
    }

    #[test]
    fn title_is_manual_round_trips_when_true() {
        let meta = ExportedMetadata {
            title: Some("Pinned".into()),
            cwd: "/tmp".into(),
            model_id: None,
            created_at: None,
            updated_at: None,
            total_messages: None,
            parent_session_id: None,
            session_kind: None,
            subagent_type: None,
            subagent_persona: None,
            subagent_role: None,
            fork_context_source: None,
            subagent_depth: None,
            title_is_manual: Some(true),
        };
        let json = serde_json::to_value(&meta).unwrap();
        assert_eq!(json["title_is_manual"], true);
        let back: ExportedMetadata = serde_json::from_value(json).unwrap();
        assert_eq!(back.title_is_manual, Some(true));
        assert_eq!(back.title.as_deref(), Some("Pinned"));
    }
}
