use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpElicitMode {
    Form,
    Url,
}

/// Per-mode fields of an elicitation request, internally tagged with the
/// wire `mode` key ("form" / "url") so a request can never carry a mode
/// with the wrong companion fields. Flattened into [`McpElicitExtRequest`],
/// keeping the flat top-level camelCase wire shape.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum McpElicitModeFields {
    Form {
        // Optional: clients default a missing schema to an empty form.
        #[serde(
            rename = "requestedSchema",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        requested_schema: Option<Value>,
    },
    Url {
        url: String,
        #[serde(rename = "elicitationId")]
        elicitation_id: String,
    },
}

impl McpElicitModeFields {
    pub fn kind(&self) -> McpElicitMode {
        match self {
            Self::Form { .. } => McpElicitMode::Form,
            Self::Url { .. } => McpElicitMode::Url,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpElicitExtRequest {
    pub session_id: String,
    pub tool_call_id: String,
    pub server_name: String,
    pub message: String,
    #[serde(flatten)]
    pub mode: McpElicitModeFields,
}

impl McpElicitExtRequest {
    pub fn mode_kind(&self) -> McpElicitMode {
        self.mode.kind()
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpElicitCompletePayload {
    pub session_id: String,
    pub elicitation_id: String,
    /// Emitting server, so a client can refuse a complete notification
    /// aimed at another server's card. `Option` for version skew: older
    /// shells omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_name: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum McpElicitExtResponse {
    Accept {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<Value>,
    },
    Decline,
    Cancel,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_serializes_camel_case() {
        let req = McpElicitExtRequest {
            session_id: "s1".into(),
            tool_call_id: "mcp-elicit-1".into(),
            server_name: "github".into(),
            message: "Need email".into(),
            mode: McpElicitModeFields::Form {
                requested_schema: Some(json!({
                    "type": "object",
                    "properties": { "email": { "type": "string" } },
                    "required": ["email"]
                })),
            },
        };
        let v = serde_json::to_value(&req).unwrap();
        assert!(v.get("sessionId").is_some());
        assert!(v.get("toolCallId").is_some());
        assert!(v.get("serverName").is_some());
        assert!(v.get("requestedSchema").is_some());
        assert!(v.get("session_id").is_none());
    }

    #[test]
    fn form_request_round_trips() {
        let req = McpElicitExtRequest {
            session_id: "s1".into(),
            tool_call_id: "mcp-elicit-1".into(),
            server_name: "github".into(),
            message: "Need email".into(),
            mode: McpElicitModeFields::Form {
                requested_schema: Some(json!({"type": "object", "properties": {}})),
            },
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: McpElicitExtRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.server_name, "github");
        assert_eq!(back.mode_kind(), McpElicitMode::Form);
        assert!(matches!(
            back.mode,
            McpElicitModeFields::Form {
                requested_schema: Some(_)
            }
        ));
    }

    #[test]
    fn url_request_round_trips() {
        let req = McpElicitExtRequest {
            session_id: "s1".into(),
            tool_call_id: "mcp-elicit-2".into(),
            server_name: "oauth-server".into(),
            message: "Login".into(),
            mode: McpElicitModeFields::Url {
                url: "https://example.com/auth".into(),
                elicitation_id: "el-1".into(),
            },
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: McpElicitExtRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.mode_kind(), McpElicitMode::Url);
        let McpElicitModeFields::Url {
            url,
            elicitation_id,
        } = back.mode
        else {
            panic!("expected url mode");
        };
        assert_eq!(url, "https://example.com/auth");
        assert_eq!(elicitation_id, "el-1");
    }

    /// The flattened mode enum must keep the exact flat top-level key set
    /// (and camelCase spellings) of the previous struct-with-Options shape.
    #[test]
    fn request_wire_key_set_is_unchanged_per_mode() {
        let keys = |req: &McpElicitExtRequest| -> Vec<String> {
            let serde_json::Value::Object(map) = serde_json::to_value(req).unwrap() else {
                panic!("request must serialize to an object");
            };
            let mut keys: Vec<String> = map.keys().cloned().collect();
            keys.sort();
            keys
        };

        let form = McpElicitExtRequest {
            session_id: "s1".into(),
            tool_call_id: "mcp-elicit-1".into(),
            server_name: "github".into(),
            message: "Need email".into(),
            mode: McpElicitModeFields::Form {
                requested_schema: Some(json!({"type": "object", "properties": {}})),
            },
        };
        assert_eq!(
            keys(&form),
            [
                "message",
                "mode",
                "requestedSchema",
                "serverName",
                "sessionId",
                "toolCallId",
            ]
        );
        assert_eq!(serde_json::to_value(&form).unwrap()["mode"], "form");

        // A schema-less form omits `requestedSchema` entirely.
        let bare_form = McpElicitExtRequest {
            mode: McpElicitModeFields::Form {
                requested_schema: None,
            },
            ..form
        };
        assert_eq!(
            keys(&bare_form),
            ["message", "mode", "serverName", "sessionId", "toolCallId"]
        );

        let url = McpElicitExtRequest {
            session_id: "s1".into(),
            tool_call_id: "mcp-elicit-2".into(),
            server_name: "oauth-server".into(),
            message: "Login".into(),
            mode: McpElicitModeFields::Url {
                url: "https://example.com/auth".into(),
                elicitation_id: "el-1".into(),
            },
        };
        assert_eq!(
            keys(&url),
            [
                "elicitationId",
                "message",
                "mode",
                "serverName",
                "sessionId",
                "toolCallId",
                "url",
            ]
        );
        assert_eq!(serde_json::to_value(&url).unwrap()["mode"], "url");
    }

    #[test]
    fn response_accept_with_content() {
        let resp = McpElicitExtResponse::Accept {
            content: Some(json!({"email": "a@b.com"})),
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["outcome"], "accept");
        assert_eq!(v["content"]["email"], "a@b.com");
        let back: McpElicitExtResponse = serde_json::from_value(v).unwrap();
        assert!(matches!(back, McpElicitExtResponse::Accept { .. }));
    }

    #[test]
    fn response_decline_and_cancel() {
        for resp in [McpElicitExtResponse::Decline, McpElicitExtResponse::Cancel] {
            let json = serde_json::to_string(&resp).unwrap();
            let back: McpElicitExtResponse = serde_json::from_str(&json).unwrap();
            match (&resp, &back) {
                (McpElicitExtResponse::Decline, McpElicitExtResponse::Decline) => {}
                (McpElicitExtResponse::Cancel, McpElicitExtResponse::Cancel) => {}
                _ => panic!("mismatch: {json}"),
            }
        }
    }
}
