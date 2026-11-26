//! Wire-shape parsers for ext-notification params handled by `MvpAgent`.
//!
//! Pure parsing only (params JSON → `SessionCommand`); session lookup and
//! command dispatch stay in `mvp_agent::ext_notification`.

use crate::session::SessionCommand;

/// Parse a `echo.agent/queue/{remove,reorder,clear,edit,interject,hold_edit,release_edit}`
/// ext-notification's params into the corresponding [`SessionCommand`].
/// `owner` is the resolved attribution (params `owner`/`clientIdentifier`) used
/// to scope remove/clear to the requesting client's own items, and recorded as
/// `last_editor` for in-place text edits. Returns `None` for unrecognized
/// methods, for `edit` when `newText` is missing, or when a required `id` is
/// missing.
pub(super) fn parse_queue_edit_command(
    method: &str,
    params: &serde_json::Value,
    owner: Option<String>,
) -> Option<SessionCommand> {
    match method {
        "echo.agent/queue/remove" => {
            let id = params.get("id").and_then(|v| v.as_str())?.to_string();
            // The client supplies the version it last saw; the handler removes
            // only on an exact match (stale = benign no-op + rebroadcast).
            // Default 0 covers never-edited prompts (the common case).
            let expected_version = params
                .get("expectedVersion")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            Some(SessionCommand::RemoveQueuedPrompt {
                id,
                expected_version,
                owner,
            })
        }
        "echo.agent/queue/reorder" => {
            let ordered_ids = params
                .get("orderedIds")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Some(SessionCommand::ReorderQueue { ordered_ids })
        }
        "echo.agent/queue/clear" => Some(SessionCommand::ClearQueue { owner }),
        "echo.agent/queue/interject" => {
            let id = params.get("id").and_then(|v| v.as_str())?.to_string();
            // The client supplies the version it last saw; the handler acts
            // only on an exact match (stale = benign no-op + rebroadcast).
            let expected_version = params
                .get("expectedVersion")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            // Optional client-edited replacement text (atomic edit+interject).
            // Blank overrides are dropped (degrade to the stored queue text) —
            // never interject an empty prompt on a malformed client param.
            let new_text = params
                .get("newText")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string);
            Some(SessionCommand::InterjectQueuedPrompt {
                id,
                expected_version,
                owner,
                new_text,
            })
        }
        "echo.agent/queue/edit" => {
            let id = params.get("id").and_then(|v| v.as_str())?.to_string();
            let new_text = params.get("newText").and_then(|v| v.as_str())?.to_string();
            // `owner` is the resolved attribution; for edit it represents the
            // most recent editor (recorded as `last_editor`), not the original
            // enqueuer.
            Some(SessionCommand::EditQueuedPrompt {
                id,
                new_text,
                editor: owner,
            })
        }
        "echo.agent/queue/hold_edit" => {
            let id = params.get("id").and_then(|v| v.as_str())?.to_string();
            Some(SessionCommand::HoldEdit { id })
        }
        "echo.agent/queue/release_edit" => {
            let id = params.get("id").and_then(|v| v.as_str())?.to_string();
            Some(SessionCommand::ReleaseEdit { id })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each `echo.agent/queue/*` ext-notification maps to the
    /// correct versioned/idempotent `SessionCommand`.
    #[test]
    fn parse_queue_edit_command_maps_each_method() {
        // remove: id + expectedVersion + owner.
        let p = serde_json::json!({
            "sessionId": "s1", "id": "p7", "expectedVersion": 3
        });
        match parse_queue_edit_command("echo.agent/queue/remove", &p, Some("echo-agent-tui".into()))
        {
            Some(SessionCommand::RemoveQueuedPrompt {
                id,
                expected_version,
                owner,
            }) => {
                assert_eq!(id, "p7");
                assert_eq!(expected_version, 3);
                assert_eq!(owner.as_deref(), Some("echo-agent-tui"));
            }
            _ => panic!("expected RemoveQueuedPrompt"),
        }

        // remove without expectedVersion defaults to 0.
        let p = serde_json::json!({ "sessionId": "s1", "id": "p8" });
        match parse_queue_edit_command("echo.agent/queue/remove", &p, None) {
            Some(SessionCommand::RemoveQueuedPrompt {
                expected_version, ..
            }) => assert_eq!(expected_version, 0),
            _ => panic!("expected RemoveQueuedPrompt"),
        }

        // reorder: orderedIds array.
        let p = serde_json::json!({ "sessionId": "s1", "orderedIds": ["a", "b", "c"] });
        match parse_queue_edit_command("echo.agent/queue/reorder", &p, None) {
            Some(SessionCommand::ReorderQueue { ordered_ids }) => {
                assert_eq!(ordered_ids, vec!["a", "b", "c"]);
            }
            _ => panic!("expected ReorderQueue"),
        }

        // clear: owner-scoped.
        match parse_queue_edit_command(
            "echo.agent/queue/clear",
            &serde_json::json!({ "sessionId": "s1" }),
            Some("echo-agent-tui".into()),
        ) {
            Some(SessionCommand::ClearQueue { owner }) => {
                assert_eq!(owner.as_deref(), Some("echo-agent-tui"));
            }
            _ => panic!("expected ClearQueue"),
        }

        // edit: id + newText + editor (resolved via owner/clientIdentifier).
        let p = serde_json::json!({
            "sessionId": "s1", "id": "p9", "newText": "replacement text"
        });
        match parse_queue_edit_command(
            "echo.agent/queue/edit",
            &p,
            Some("echo-agent-vscode".into()),
        ) {
            Some(SessionCommand::EditQueuedPrompt {
                id,
                new_text,
                editor,
            }) => {
                assert_eq!(id, "p9");
                assert_eq!(new_text, "replacement text");
                assert_eq!(editor.as_deref(), Some("echo-agent-vscode"));
            }
            _ => panic!("expected EditQueuedPrompt"),
        }

        // edit without editor (no owner/clientIdentifier) → editor: None.
        match parse_queue_edit_command(
            "echo.agent/queue/edit",
            &serde_json::json!({ "sessionId": "s1", "id": "p9", "newText": "x" }),
            None,
        ) {
            Some(SessionCommand::EditQueuedPrompt { editor, .. }) => {
                assert!(editor.is_none());
            }
            _ => panic!("expected EditQueuedPrompt"),
        }

        // edit without newText → None (can't replace text we don't have).
        assert!(
            parse_queue_edit_command(
                "echo.agent/queue/edit",
                &serde_json::json!({ "sessionId": "s1", "id": "p9" }),
                None,
            )
            .is_none()
        );

        // edit without id → None (can't target an entry).
        assert!(
            parse_queue_edit_command(
                "echo.agent/queue/edit",
                &serde_json::json!({ "sessionId": "s1", "newText": "x" }),
                None,
            )
            .is_none()
        );

        // interject: id + expectedVersion + owner (mirrors remove).
        let p = serde_json::json!({
            "sessionId": "s1", "id": "p10", "expectedVersion": 2
        });
        match parse_queue_edit_command(
            "echo.agent/queue/interject",
            &p,
            Some("echo-agent-tui".into()),
        ) {
            Some(SessionCommand::InterjectQueuedPrompt {
                id,
                expected_version,
                owner,
                new_text,
            }) => {
                assert_eq!(id, "p10");
                assert_eq!(expected_version, 2);
                assert_eq!(owner.as_deref(), Some("echo-agent-tui"));
                assert_eq!(new_text, None, "newText absent → None");
            }
            _ => panic!("expected InterjectQueuedPrompt"),
        }

        // interject with newText (client-edited row) carries the override.
        let p = serde_json::json!({
            "sessionId": "s1", "id": "p10", "expectedVersion": 2, "newText": "edited"
        });
        match parse_queue_edit_command("echo.agent/queue/interject", &p, None) {
            Some(SessionCommand::InterjectQueuedPrompt { new_text, .. }) => {
                assert_eq!(new_text.as_deref(), Some("edited"));
            }
            _ => panic!("expected InterjectQueuedPrompt"),
        }

        // Blank newText is dropped → degrades to the stored queue text.
        let p = serde_json::json!({
            "sessionId": "s1", "id": "p10", "expectedVersion": 2, "newText": "   "
        });
        match parse_queue_edit_command("echo.agent/queue/interject", &p, None) {
            Some(SessionCommand::InterjectQueuedPrompt { new_text, .. }) => {
                assert_eq!(new_text, None, "blank override must be dropped");
            }
            _ => panic!("expected InterjectQueuedPrompt"),
        }

        // interject without expectedVersion defaults to 0.
        match parse_queue_edit_command(
            "echo.agent/queue/interject",
            &serde_json::json!({ "sessionId": "s1", "id": "p11" }),
            None,
        ) {
            Some(SessionCommand::InterjectQueuedPrompt {
                expected_version, ..
            }) => assert_eq!(expected_version, 0),
            _ => panic!("expected InterjectQueuedPrompt"),
        }

        // interject without id → None (can't target an entry).
        assert!(
            parse_queue_edit_command("echo.agent/queue/interject", &serde_json::json!({}), None)
                .is_none()
        );

        // hold_edit / release_edit: id only (combine-hold while the client edits).
        let p = serde_json::json!({ "sessionId": "s1", "id": "p12" });
        match parse_queue_edit_command("echo.agent/queue/hold_edit", &p, None) {
            Some(SessionCommand::HoldEdit { id }) => assert_eq!(id, "p12"),
            _ => panic!("expected HoldEdit"),
        }
        match parse_queue_edit_command("echo.agent/queue/release_edit", &p, None) {
            Some(SessionCommand::ReleaseEdit { id }) => assert_eq!(id, "p12"),
            _ => panic!("expected ReleaseEdit"),
        }

        // hold_edit / release_edit without id → None (can't target an entry).
        assert!(
            parse_queue_edit_command("echo.agent/queue/hold_edit", &serde_json::json!({}), None)
                .is_none()
        );
        assert!(
            parse_queue_edit_command(
                "echo.agent/queue/release_edit",
                &serde_json::json!({}),
                None
            )
            .is_none()
        );

        // unknown method → None. Outbound `changed` is the other production
        // `echo.agent/queue/*` method and must not parse as an edit command.
        assert!(
            parse_queue_edit_command("echo.agent/queue/bogus", &serde_json::json!({}), None)
                .is_none()
        );
        assert!(
            parse_queue_edit_command(
                "echo.agent/queue/changed",
                &serde_json::json!({
                    "sessionId": "s1",
                    "entries": [{
                        "id": "p1",
                        "version": 0,
                        "kind": "prompt",
                        "text": "hello",
                        "position": 0,
                    }],
                }),
                None,
            )
            .is_none()
        );
        // remove without id → None (can't target an entry).
        assert!(
            parse_queue_edit_command("echo.agent/queue/remove", &serde_json::json!({}), None)
                .is_none()
        );
    }
}
