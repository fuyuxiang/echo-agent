use agent_client_protocol as acp;
use echo_agent_acp::AcpAgentGatewaySender as GatewaySender;
use serde::{Deserialize, Serialize};

// Re-export from workspace crate (canonical home for fuzzy search).
pub use echo_agent_workspace::file_system::{ClientId, TargetClientId};

/// Metadata from the request, used for routing notifications back to the
/// correct client.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestMeta {
    #[serde(default)]
    pub client_id: TargetClientId,
}

/// Notification-side routing metadata. Embeddable in any outgoing
/// notification struct via `#[serde(rename = "_meta")]`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationMeta {
    #[serde(skip_serializing_if = "TargetClientId::is_none")]
    pub target_client_id: TargetClientId,
}

/// Inject `targetClientId` into the `_meta` field of a JSON params object.
/// Merges with any existing `_meta` fields rather than replacing them.
pub(crate) fn inject_routing_meta(
    params: &mut serde_json::Value,
    target_client_id: &TargetClientId,
) {
    if target_client_id.is_none() {
        return;
    }
    let meta = params.as_object_mut().and_then(|obj| {
        obj.entry("_meta")
            .or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
    });
    if let Some(meta) = meta
        && let Ok(val) = serde_json::to_value(target_client_id)
    {
        meta.insert("targetClientId".to_string(), val);
    }
}

/// Send a fire-and-forget ext notification with optional client routing.
///
/// If `target_client_id` is set, injects `_meta.targetClientId` into `params`
/// so the gateway can route the notification to the correct client.
pub fn send_routed_notification(
    gateway: &GatewaySender,
    method: &str,
    mut params: serde_json::Value,
    target_client_id: &TargetClientId,
) {
    inject_routing_meta(&mut params, target_client_id);
    if let Ok(raw) = serde_json::value::to_raw_value(&params) {
        gateway.forward_fire_and_forget(acp::ExtNotification::new(method, raw.into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_client_id_serialization() {
        let none = TargetClientId::None;
        assert_eq!(serde_json::to_string(&none).unwrap(), "null");

        let client_id = TargetClientId::ClientId(ClientId {
            instance_id: "inst-123".to_string(),
            conn_id: "conn-456".to_string(),
        });
        let json = serde_json::to_string(&client_id).unwrap();
        assert_eq!(json, r#"{"instanceId":"inst-123","connId":"conn-456"}"#);
    }

    #[test]
    fn request_meta_deserialization() {
        let json = r#"{"clientId": {"instanceId": "inst-1", "connId": "conn-2"}}"#;
        let meta: RequestMeta = serde_json::from_str(json).unwrap();
        assert!(matches!(meta.client_id, TargetClientId::ClientId(_)));

        let json = r#"{}"#;
        let meta: RequestMeta = serde_json::from_str(json).unwrap();
        assert!(meta.client_id.is_none());
    }

    #[test]
    fn notification_meta_serialization() {
        let meta = NotificationMeta {
            target_client_id: TargetClientId::ClientId(ClientId {
                instance_id: "inst-abc".to_string(),
                conn_id: "conn-xyz".to_string(),
            }),
        };
        let json = serde_json::to_string(&meta).unwrap();
        assert_eq!(
            json,
            r#"{"targetClientId":{"instanceId":"inst-abc","connId":"conn-xyz"}}"#
        );

        let meta_none = NotificationMeta {
            target_client_id: TargetClientId::None,
        };
        let json = serde_json::to_string(&meta_none).unwrap();
        assert_eq!(json, r#"{}"#);
    }

    #[test]
    fn inject_routing_meta_inserts_into_empty_params() {
        let mut params = serde_json::json!({"terminalId": "abc"});
        let target = TargetClientId::ClientId(ClientId {
            instance_id: "inst-1".to_string(),
            conn_id: "conn-2".to_string(),
        });
        inject_routing_meta(&mut params, &target);
        assert_eq!(params["_meta"]["targetClientId"]["instanceId"], "inst-1");
        assert_eq!(params["_meta"]["targetClientId"]["connId"], "conn-2");
        assert_eq!(params["terminalId"], "abc");
    }

    #[test]
    fn inject_routing_meta_merges_with_existing_meta() {
        let mut params = serde_json::json!({
            "data": "x",
            "_meta": {"eventId": "evt-1"}
        });
        let target = TargetClientId::ClientId(ClientId {
            instance_id: "inst-1".to_string(),
            conn_id: "conn-2".to_string(),
        });
        inject_routing_meta(&mut params, &target);
        assert_eq!(params["_meta"]["eventId"], "evt-1");
        assert_eq!(params["_meta"]["targetClientId"]["instanceId"], "inst-1");
    }

    #[test]
    fn inject_routing_meta_skips_when_none() {
        let mut params = serde_json::json!({"terminalId": "abc"});
        inject_routing_meta(&mut params, &TargetClientId::None);
        assert!(params.get("_meta").is_none());
    }
}
