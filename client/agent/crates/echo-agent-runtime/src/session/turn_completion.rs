//! Pure construction of the durable, replayable turn-completion terminal.
//!
//! `TurnCompleted` is the persisted + replayed twin of the fire-and-forget
//! `echo.agent/session/prompt_complete` notification: it rides the
//! `_echo.agent/session/update` rail so a viewer that re-attaches mid-turn finalizes
//! the turn from replay instead of stranding on "Waiting…". The
//! `(stop_reason, agent_result)` pair is the SAME pair `prompt_complete`
//! carries (from [`crate::sampling::error::prompt_complete_fields`]), so the
//! two signals never disagree.

use crate::extensions::notification::SessionUpdate;

/// Build a `TurnCompleted` from a prompt id and the `(stop_reason, agent_result)`
/// JSON pair produced by [`crate::sampling::error::prompt_complete_fields`].
/// `stop_reason` is always a JSON string; `agent_result` is a string or null.
/// Non-string inputs fall back to their JSON text so a terminal is never
/// dropped for a shape mismatch.
pub(crate) fn build_turn_completed(
    prompt_id: String,
    stop_reason: serde_json::Value,
    agent_result: serde_json::Value,
    usage: Option<crate::extensions::notification::PromptUsage>,
) -> SessionUpdate {
    SessionUpdate::TurnCompleted {
        prompt_id,
        stop_reason: json_to_string(stop_reason),
        agent_result: match agent_result {
            serde_json::Value::Null => None,
            other => Some(json_to_string(other)),
        },
        usage,
    }
}

/// A JSON string yields its inner text; any other shape falls back to its JSON
/// serialization rather than being dropped.
fn json_to_string(value: serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s,
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_ok_end_turn_pair() {
        // The exact pair `prompt_complete_fields(&Ok(EndTurn))` produces.
        let update = build_turn_completed(
            "p-1".into(),
            serde_json::json!("end_turn"),
            serde_json::Value::Null,
            None,
        );
        assert_eq!(
            update,
            SessionUpdate::TurnCompleted {
                prompt_id: "p-1".into(),
                stop_reason: "end_turn".into(),
                agent_result: None,
                usage: None,
            }
        );
    }

    #[test]
    fn maps_error_pair_with_detail() {
        // The pair `prompt_complete_fields(&Err(..))` produces for a generic error.
        let update = build_turn_completed(
            "p-2".into(),
            serde_json::json!("error"),
            serde_json::json!("connection reset"),
            None,
        );
        assert_eq!(
            update,
            SessionUpdate::TurnCompleted {
                prompt_id: "p-2".into(),
                stop_reason: "error".into(),
                agent_result: Some("connection reset".into()),
                usage: None,
            }
        );
    }

    #[test]
    fn null_agent_result_maps_to_none() {
        let update = build_turn_completed(
            "p-3".into(),
            serde_json::json!("cancelled"),
            serde_json::Value::Null,
            None,
        );
        assert!(matches!(
            update,
            SessionUpdate::TurnCompleted {
                agent_result: None,
                ..
            }
        ));
    }

    #[test]
    fn non_string_values_fall_back_to_json_text() {
        // Defensive: a non-string stop_reason / object agent_result still
        // produces a best-effort terminal rather than being dropped.
        let update = build_turn_completed(
            "p-4".into(),
            serde_json::json!(42),
            serde_json::json!({ "k": "v" }),
            None,
        );
        assert_eq!(
            update,
            SessionUpdate::TurnCompleted {
                prompt_id: "p-4".into(),
                stop_reason: "42".into(),
                agent_result: Some("{\"k\":\"v\"}".into()),
                usage: None,
            }
        );
    }
}
