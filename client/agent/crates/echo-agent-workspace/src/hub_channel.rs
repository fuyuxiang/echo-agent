//! Server-proxied workspace utilities.
//!
//! Helper functions for consuming server tool streams and extracting typed
//! notifications from server notification frames. These are used by both
//! the workspace crate (hub_server) and the shell crate (proxy-mode
//! session actors) to interact with the server.
//!
//! The `HubWorkspaceChannel` struct that previously lived here has been
//! removed. Sessions now call the server harness directly via `ToolContext`.

use echo_agent_tools::notification::types::ToolNotification;
use echo_agent_workspace_types::WorkspaceEvent;

pub use crate::hub_ids::WORKSPACE_RPC_TOOL_ID;
// Canonical in the client crate; re-exported for existing importers.
pub use echo_agent_workspace_client::consume_stream_terminal;

/// Extract a `WorkspaceEvent` from a custom `ToolNotificationFrame`.
pub fn extract_workspace_event(
    frame: &echo_agent_tool_protocol::ToolNotificationFrame,
) -> Option<WorkspaceEvent> {
    use echo_agent_tool_protocol::WireToolNotification;
    match &frame.notification {
        WireToolNotification::Custom(c) => serde_json::from_value(c.payload.clone()).ok(),
        _ => None,
    }
}

/// Extract a `ToolNotification` from a custom `ToolNotificationFrame`.
///
/// Currently has no producer; kept for a future per-session `tool.notify` path.
pub fn extract_tool_notification(
    frame: &echo_agent_tool_protocol::ToolNotificationFrame,
) -> Option<ToolNotification> {
    use echo_agent_tool_protocol::WireToolNotification;
    match &frame.notification {
        WireToolNotification::Custom(c) => {
            serde_json::from_value::<ToolNotification>(c.payload.clone()).ok()
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_tool_protocol::{ToolId, ToolNotificationFrame, WireToolNotification};

    #[test]
    fn extract_workspace_event_custom_valid() {
        let event = WorkspaceEvent::ToolsChanged {
            session_id: "main".into(),
        };
        let payload = serde_json::to_value(&event).unwrap();
        let frame = ToolNotificationFrame::custom(
            ToolId::new("workspace_events").unwrap(),
            "workspace_event",
            payload,
        );
        let result = extract_workspace_event(&frame);
        assert!(result.is_some(), "should parse valid WorkspaceEvent");
        match result.unwrap() {
            WorkspaceEvent::ToolsChanged { session_id } => {
                assert_eq!(session_id, "main");
            }
            other => panic!("expected ToolsChanged, got {other:?}"),
        }
    }

    #[test]
    fn extract_workspace_event_invalid_payload() {
        let frame = ToolNotificationFrame::custom(
            ToolId::new("workspace_events").unwrap(),
            "workspace_event",
            serde_json::json!({"not_a_valid_event": true}),
        );
        assert!(
            extract_workspace_event(&frame).is_none(),
            "invalid payload should return None"
        );
    }

    #[test]
    fn extract_workspace_event_known_variant_returns_none() {
        let frame = ToolNotificationFrame {
            tool_call_id: None,
            tool_id: Some(ToolId::new("workspace_events").unwrap()),
            notification: WireToolNotification::Known(serde_json::json!({})),
        };
        assert!(
            extract_workspace_event(&frame).is_none(),
            "Known variant should return None"
        );
    }

    #[test]
    fn extract_tool_notification_invalid_payload() {
        let frame = ToolNotificationFrame::custom(
            ToolId::new("workspace_tool_notifications").unwrap(),
            "tool_notification",
            serde_json::json!({"not_a_notification": true}),
        );
        assert!(
            extract_tool_notification(&frame).is_none(),
            "invalid payload should return None"
        );
    }

    #[test]
    fn extract_tool_notification_known_variant_returns_none() {
        let frame = ToolNotificationFrame {
            tool_call_id: None,
            tool_id: Some(ToolId::new("workspace_tool_notifications").unwrap()),
            notification: WireToolNotification::Known(serde_json::json!({})),
        };
        assert!(
            extract_tool_notification(&frame).is_none(),
            "Known variant should return None"
        );
    }

    // consume_stream_terminal tests live in echo-agent-workspace-client.
}
