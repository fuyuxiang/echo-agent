//! Wire types for the `echo.agent/exit_plan_mode` ACP ext_method.
//!
//! Shared between the shell (serializer) and the pager/desktop/VS Code
//! (deserializer) so both sides stay in sync.

/// ACP `ext_method` request payload (shell coordinator sends to client/pager).
///
/// Serialized as `camelCase` for the ACP JSON-RPC wire format.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExitPlanModeExtRequest {
    pub session_id: String,
    pub tool_call_id: String,
    pub plan_content: Option<String>,
}

/// ACP `ext_method` response payload (client/pager returns to shell coordinator).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExitPlanModeExtResponse {
    /// `"approved"`, `"cancelled"`, or `"abandoned"`.
    pub outcome: String,
    /// Only present on `"cancelled"` when the user typed feedback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ext_request_serializes_camel_case() {
        let req = ExitPlanModeExtRequest {
            session_id: "sess-1".into(),
            tool_call_id: "tc-1".into(),
            plan_content: Some("# Plan".into()),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("sessionId").is_some());
        assert!(json.get("toolCallId").is_some());
        assert!(json.get("planContent").is_some());
        // Must NOT contain snake_case keys
        assert!(json.get("session_id").is_none());
        assert!(json.get("tool_call_id").is_none());
        assert!(json.get("plan_content").is_none());
    }

    #[test]
    fn ext_request_round_trips() {
        let req = ExitPlanModeExtRequest {
            session_id: "sess-1".into(),
            tool_call_id: "tc-1".into(),
            plan_content: Some("# Plan\n\n## Step 1\nDo something".into()),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: ExitPlanModeExtRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.session_id, "sess-1");
        assert_eq!(back.tool_call_id, "tc-1");
        assert_eq!(
            back.plan_content.as_deref(),
            Some("# Plan\n\n## Step 1\nDo something")
        );
    }

    #[test]
    fn ext_request_round_trips_no_plan() {
        let req = ExitPlanModeExtRequest {
            session_id: "sess-2".into(),
            tool_call_id: "tc-2".into(),
            plan_content: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: ExitPlanModeExtRequest = serde_json::from_str(&json).unwrap();
        assert!(back.plan_content.is_none());
    }

    #[test]
    fn ext_response_approved_round_trips() {
        let resp = ExitPlanModeExtResponse {
            outcome: "approved".into(),
            feedback: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: ExitPlanModeExtResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.outcome, "approved");
        assert!(back.feedback.is_none());
    }

    #[test]
    fn ext_response_cancelled_with_feedback_round_trips() {
        let resp = ExitPlanModeExtResponse {
            outcome: "cancelled".into(),
            feedback: Some("Please add error handling".into()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: ExitPlanModeExtResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.outcome, "cancelled");
        assert_eq!(back.feedback.as_deref(), Some("Please add error handling"));
    }

    #[test]
    fn ext_response_omits_none_feedback() {
        let resp = ExitPlanModeExtResponse {
            outcome: "approved".into(),
            feedback: None,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert!(json.get("feedback").is_none());
    }

    #[test]
    fn ext_response_abandoned_round_trips() {
        let resp = ExitPlanModeExtResponse {
            outcome: "abandoned".into(),
            feedback: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: ExitPlanModeExtResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.outcome, "abandoned");
        assert!(back.feedback.is_none());
    }
}
