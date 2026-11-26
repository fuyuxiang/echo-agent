//! Parsed server notification events.
//!
//! [`HubNotification`] is the typed representation of server-pushed
//! notification frames that arrive on a session inbox. The
//! [`HubNotification::parse`] constructor classifies a raw JSON value
//! by its `method` field and deserializes the known shapes; anything
//! unrecognised lands in [`HubNotification::Unknown`] so callers never
//! lose data.

use echo_agent_tool_protocol::{
    SessionId, ToolId, ToolNotificationFrame, ToolServerStatusPayload, ToolsChanged,
};
use serde_json::Value;
use tracing::warn;

/// A typed server notification event parsed from a raw JSON-RPC notification frame.
#[derive(Debug, Clone, PartialEq)]
pub enum HubNotification {
    /// The active tool set for a session changed (tools added, removed, or updated).
    ToolsChanged {
        session_id: SessionId,
        added: Vec<ToolId>,
        removed: Vec<ToolId>,
        updated: Vec<ToolId>,
    },
    /// A tool notification forwarded by the server to all subscribers.
    ToolNotification {
        session_id: SessionId,
        frame: ToolNotificationFrame,
    },
    /// Tool server lifecycle status change, extracted from
    /// `__tool_server_status` / `status_changed` notification frames.
    ToolServerStatusChanged {
        session_id: SessionId,
        status: ToolServerStatusPayload,
    },
    /// A notification whose `method` is not recognised by this SDK version.
    Unknown { method: String, params: Value },
}

impl HubNotification {
    /// Parse a raw JSON-RPC notification into a typed [`HubNotification`].
    ///
    /// Returns `None` when the value lacks a `method` field (i.e. it is
    /// not a notification at all).
    pub fn parse(value: &Value) -> Option<Self> {
        let method = value.get("method")?.as_str()?;
        let params = value
            .get("params")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));

        match method {
            // `ToolsChanged` carries `session_id` inside `params`.
            "tools_changed" => match serde_json::from_value::<ToolsChanged>(params.clone()) {
                Ok(tc) => Some(HubNotification::ToolsChanged {
                    session_id: tc.session_id,
                    added: tc.added,
                    removed: tc.removed,
                    updated: tc.updated,
                }),
                Err(err) => {
                    warn!(%err, "tools_changed params failed to deserialize; falling back to Unknown");
                    Some(HubNotification::Unknown {
                        method: method.to_owned(),
                        params,
                    })
                }
            },
            // `ToolNotificationFrame` has no `session_id`; use the envelope field.
            "tool.notification" => {
                let frame_result = serde_json::from_value::<ToolNotificationFrame>(params.clone());
                let session_id = value
                    .get("session_id")
                    .and_then(Value::as_str)
                    .and_then(|s| SessionId::new(s).ok());
                match (frame_result, session_id) {
                    (Ok(frame), Some(session_id)) => {
                        if frame
                            .tool_id
                            .as_ref()
                            .is_some_and(|id| id.as_str() == "__tool_server_status")
                            && let echo_agent_tool_protocol::notification_wire::WireToolNotification::Custom(ref c) = frame.notification
                            && c.kind == "status_changed"
                        {
                            match serde_json::from_value::<ToolServerStatusPayload>(
                                c.payload.clone(),
                            ) {
                                Ok(status) => {
                                    return Some(HubNotification::ToolServerStatusChanged {
                                        session_id,
                                        status,
                                    });
                                }
                                Err(err) => {
                                    warn!(%err, "tool_server status payload failed to deserialize");
                                }
                            }
                        }
                        Some(HubNotification::ToolNotification { session_id, frame })
                    }
                    (Err(err), _) => {
                        warn!(%err, "tool.notification params failed to deserialize; falling back to Unknown");
                        Some(HubNotification::Unknown {
                            method: method.to_owned(),
                            params,
                        })
                    }
                    (_, None) => {
                        warn!(
                            "tool.notification missing or invalid session_id; falling back to Unknown"
                        );
                        Some(HubNotification::Unknown {
                            method: method.to_owned(),
                            params,
                        })
                    }
                }
            }
            _ => Some(HubNotification::Unknown {
                method: method.to_owned(),
                params,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_tools_changed() {
        let value = json!({
            "jsonrpc": "2.0",
            "session_id": "s1",
            "method": "tools_changed",
            "params": {
                "session_id": "s1",
                "added": ["echo", "add"],
                "removed": [],
            }
        });
        let notif = HubNotification::parse(&value).expect("should parse");
        match notif {
            HubNotification::ToolsChanged {
                session_id,
                added,
                removed,
                updated,
            } => {
                assert_eq!(session_id.as_str(), "s1");
                assert_eq!(added.len(), 2);
                assert!(removed.is_empty());
                assert!(updated.is_empty());
            }
            other => panic!("expected ToolsChanged, got {other:?}"),
        }
    }

    #[test]
    fn parse_tools_changed_with_updated() {
        let value = json!({
            "jsonrpc": "2.0",
            "session_id": "s1",
            "method": "tools_changed",
            "params": {
                "session_id": "s1",
                "added": ["new_tool"],
                "removed": ["old_tool"],
                "updated": ["echo", "add"],
            }
        });
        let notif = HubNotification::parse(&value).expect("should parse");
        match notif {
            HubNotification::ToolsChanged {
                session_id,
                added,
                removed,
                updated,
            } => {
                assert_eq!(session_id.as_str(), "s1");
                assert_eq!(added.len(), 1);
                assert_eq!(removed.len(), 1);
                assert_eq!(updated.len(), 2);
                assert_eq!(updated[0].as_str(), "echo");
                assert_eq!(updated[1].as_str(), "add");
            }
            other => panic!("expected ToolsChanged, got {other:?}"),
        }
    }

    #[test]
    fn parse_tool_notification_custom() {
        let value = json!({
            "jsonrpc": "2.0",
            "session_id": "s1",
            "method": "tool.notification",
            "params": {
                "tool_id": "echo",
                "notification": {
                    "shape": "custom",
                    "value": {
                        "kind": "echo.status",
                        "payload": { "status": "idle" }
                    }
                }
            }
        });
        let notif = HubNotification::parse(&value).expect("should parse");
        match notif {
            HubNotification::ToolNotification { session_id, frame } => {
                assert_eq!(session_id.as_str(), "s1");
                assert_eq!(frame.tool_id.as_ref().unwrap().as_str(), "echo");
            }
            other => panic!("expected ToolNotification, got {other:?}"),
        }
    }

    #[test]
    fn parse_tool_notification_missing_session_id_falls_back_to_unknown() {
        let value = json!({
            "jsonrpc": "2.0",
            "method": "tool.notification",
            "params": {
                "tool_id": "echo",
                "notification": {
                    "shape": "custom",
                    "value": { "kind": "test", "payload": {} }
                }
            }
        });
        let notif = HubNotification::parse(&value).expect("should parse as Unknown, not None");
        assert!(
            matches!(notif, HubNotification::Unknown { ref method, .. } if method == "tool.notification"),
            "tool.notification without envelope session_id should fall back to Unknown, got {notif:?}"
        );
    }

    #[test]
    fn parse_unknown_method() {
        let value = json!({
            "jsonrpc": "2.0",
            "session_id": "s1",
            "method": "future.method",
            "params": { "key": "value" }
        });
        let notif = HubNotification::parse(&value).expect("should parse");
        match notif {
            HubNotification::Unknown { method, params } => {
                assert_eq!(method, "future.method");
                assert_eq!(params["key"], "value");
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn parse_missing_method_returns_none() {
        let value = json!({ "jsonrpc": "2.0", "id": "123", "result": {} });
        assert!(HubNotification::parse(&value).is_none());
    }

    #[test]
    fn parse_tools_changed_bad_params_falls_back_to_unknown() {
        // `params` has wrong shape (missing required fields) — should fall
        // back to Unknown instead of returning None and dropping the event.
        let value = json!({
            "jsonrpc": "2.0",
            "method": "tools_changed",
            "params": { "unexpected_field": true }
        });
        let notif = HubNotification::parse(&value).expect("should parse as Unknown, not None");
        assert!(
            matches!(notif, HubNotification::Unknown { ref method, .. } if method == "tools_changed"),
            "malformed tools_changed should fall back to Unknown, got {notif:?}"
        );
    }

    #[test]
    fn parse_tool_server_status_changed() {
        let value = json!({
            "jsonrpc": "2.0",
            "session_id": "s1",
            "method": "tool.notification",
            "params": {
                "tool_id": "__tool_server_status",
                "notification": {
                    "shape": "custom",
                    "value": {
                        "kind": "status_changed",
                        "payload": {
                            "status": "busy",
                            "active_tool_calls": 2,
                            "active_tool_names": ["read_file", "grep"],
                            "background_tasks": 0,
                            "pending_tool_calls": 0,
                            "last_tool_call_started_ms": 100,
                            "last_tool_call_completed_ms": 0,
                            "uptime_ms": 5000,
                        }
                    }
                }
            }
        });
        let notif = HubNotification::parse(&value).expect("should parse");
        match notif {
            HubNotification::ToolServerStatusChanged { session_id, status } => {
                assert_eq!(session_id.as_str(), "s1");
                assert_eq!(
                    status.status,
                    echo_agent_tool_protocol::ToolServerLifecycleStatus::Busy
                );
                assert_eq!(status.active_tool_calls, 2);
            }
            other => panic!("expected ToolServerStatusChanged, got {other:?}"),
        }
    }

    #[test]
    fn parse_tool_server_status_non_status_tool_id_stays_generic() {
        // A tool.notification with a different tool_id should remain
        // as ToolNotification, not be intercepted.
        let value = json!({
            "jsonrpc": "2.0",
            "session_id": "s1",
            "method": "tool.notification",
            "params": {
                "tool_id": "some_other_tool",
                "notification": {
                    "shape": "custom",
                    "value": {
                        "kind": "status_changed",
                        "payload": { "status": "ready" }
                    }
                }
            }
        });
        let notif = HubNotification::parse(&value).expect("should parse");
        assert!(
            matches!(notif, HubNotification::ToolNotification { .. }),
            "non-__tool_server_status tool_id should stay as ToolNotification, got {notif:?}"
        );
    }

    #[test]
    fn parse_tool_notification_bad_params_falls_back_to_unknown() {
        // `params` has wrong shape — should fall back to Unknown.
        let value = json!({
            "jsonrpc": "2.0",
            "session_id": "s1",
            "method": "tool.notification",
            "params": { "not_a_valid_frame": true }
        });
        let notif = HubNotification::parse(&value).expect("should parse as Unknown, not None");
        assert!(
            matches!(notif, HubNotification::Unknown { ref method, .. } if method == "tool.notification"),
            "malformed tool.notification should fall back to Unknown, got {notif:?}"
        );
    }
}
