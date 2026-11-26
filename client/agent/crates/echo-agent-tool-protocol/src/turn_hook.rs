//! Turn lifecycle hook payload types for `HookEvent::Custom`.
//!
//! These types ride inside `HookEvent::Custom { kind, payload }` and
//! provide typed serialization for `before_turn` and `after_turn`
//! custom hook payloads. They are NOT new `HookEvent` variants.

use serde::{Deserialize, Serialize};

/// Well-known `HookEvent::Custom` kind string for before-turn hooks.
pub const BEFORE_TURN_KIND: &str = "before_turn";

/// Well-known `HookEvent::Custom` kind string for after-turn hooks.
pub const AFTER_TURN_KIND: &str = "after_turn";

/// Default `session_relationship` wire value (mirrors
/// `echo_agent_session_events::SessionRelationship::Primary`).
pub const DEFAULT_SESSION_RELATIONSHIP: &str = "primary";

/// Default `schema_version` wire value. Bare literal (not the
/// `echo-agent-session-events` constant) to avoid a dependency cycle.
pub const DEFAULT_SCHEMA_VERSION: &str = "1.0";

fn default_session_relationship() -> String {
    DEFAULT_SESSION_RELATIONSHIP.to_owned()
}

fn default_schema_version() -> String {
    DEFAULT_SCHEMA_VERSION.to_owned()
}

/// Payload for `before_turn` custom hooks.
///
/// Sent by the harness before the agent loop begins a new turn.
/// Recipients can use this to prepare state (clear caches, initialize
/// tracking, etc.) but MUST NOT block — hooks are fire-and-forget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeforeTurnPayload {
    /// Per-session user-turn counter, 0-based. Not strictly monotonic: a tool-result continuation keeps the issuing turn's number, and
    /// editing or regenerating an earlier message reuses that turn's number (consumers deduping on it treat a regenerate as the same turn).
    pub turn_number: u64,
    /// Model being used for this turn (e.g. "echo-agent-3").
    pub model_id: String,
    /// Whether the session is in YOLO / auto-approve mode.
    #[serde(default)]
    pub yolo_mode: bool,
    // ── Extended fields (workspace mirrors these into `events.jsonl`);
    // all `#[serde(default)]` for old-shell / old-workspace interop. ──
    /// Mirrors `Event::TurnStarted::conversation_message_count`.
    #[serde(default)]
    pub conversation_message_count: usize,
    /// Snake-case mirror of `Event::TurnStarted::session_relationship`
    /// (`"primary"` | `"subagent"`). A `String`, not the `echo-agent-file-utils`
    /// enum, to avoid a dependency cycle; decoded by the workspace at emit time.
    #[serde(default = "default_session_relationship")]
    pub session_relationship: String,
    /// Mirrors `Event::TurnStarted::schema_version`.
    #[serde(default = "default_schema_version")]
    pub schema_version: String,
}

impl Default for BeforeTurnPayload {
    /// Mirrors the per-field serde defaults so producers that don't yet track a
    /// field (e.g. the server-side sampler for `conversation_message_count`) can
    /// use `..Default::default()` instead of repeating literal stub values.
    fn default() -> Self {
        Self {
            turn_number: 0,
            model_id: String::new(),
            yolo_mode: false,
            conversation_message_count: 0,
            session_relationship: default_session_relationship(),
            schema_version: default_schema_version(),
        }
    }
}

/// Payload for `after_turn` custom hooks.
///
/// Sent by the harness after the agent loop completes a turn.
///
/// **Design note:** This payload carries `tool_call_count` but intentionally
/// omits per-tool names. The workspace can correlate tool names from its own
/// `ActivityTracker` per-session state if needed. Keeping the payload small
/// avoids unbounded growth on tool-heavy turns. `written_repo_paths` is the
/// exception: bounded by distinct files edited, not tool-call volume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AfterTurnPayload {
    /// Same turn counter as the preceding `before_turn`.
    pub turn_number: u64,
    /// High-level outcome of the turn.
    pub outcome: TurnHookOutcome,
    /// Wall-clock duration of the turn in milliseconds.
    pub duration_ms: u64,
    /// Number of tool calls made during the turn.
    /// Tool names are intentionally excluded — the workspace can correlate
    /// from its own `ActivityTracker` if richer data is needed.
    pub tool_call_count: u32,
    /// Model used (may differ from `before_turn` if model was switched mid-turn).
    pub model_id: String,
    /// Repo-relative agent writes, so proxy-mode workspaces can force-include
    /// gitignored edits. Empty in local mode.
    #[serde(default)]
    pub written_repo_paths: Vec<String>,
    /// Snake-case mirror of `Event::TurnEnded::cancellation_category` (e.g.
    /// `"doom_loop_repetition"`). Carried as a `String` for the same
    /// dep-cycle-avoidance reason as `BeforeTurnPayload::session_relationship`;
    /// the workspace decodes it into the `echo-agent-file-utils`
    /// `CancellationCategory` enum at emit time. `None` for non-cancelled turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellation_category: Option<String>,
    /// Opaque JSON mirror of `Event::TurnEnded::cancellation_context` (e.g.
    /// `{ "reason": "max_turns_reached", "limit": 50 }`). Passed through
    /// verbatim by the workspace. `None` when there is no context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellation_context: Option<serde_json::Value>,
}

/// Turn outcome as observed by the sampler.
///
/// Named `TurnHookOutcome` (not `TurnOutcome`) to avoid collision with the
/// shell's existing `TurnOutcome` and the telemetry crate's
/// `TurnOutcomeLabel`. Module-qualified usage (`turn_hook::TurnHookOutcome`)
/// is still recommended in shell code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TurnHookOutcome {
    /// Turn completed normally (model finished generating).
    Completed,
    /// Turn was cancelled by the user (Ctrl+C / abort).
    Cancelled,
    /// Turn ended due to an error.
    Error,
}

/// `HookEvent::Custom` kind for the request/response turn hook.
pub const TURN_HOOK_KIND: &str = "turn_hook";

/// Request/response turn hook (sampler → bound workspace), internally tagged on `phase`.
/// `phase` is a reserved key — `BeforeTurnPayload`/`AfterTurnPayload` must not define a field of that name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TurnHookRequest {
    /// Fired just before the sampler begins a new turn (before inference).
    Before(BeforeTurnPayload),
    /// Fired just after the sampler completes a turn (tool results are in).
    After(AfterTurnPayload),
}

/// Conversation role for a turn the workspace asks the sampler to append.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum InjectionRole {
    /// Append as a system turn.
    System,
    /// Append as a developer turn.
    Developer,
    /// Append as a user turn (e.g. a `<system-reminder>`-wrapped message).
    User,
}

/// A single turn the workspace asks the sampler to append before the next sampling step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookInjection {
    /// Role to append the content as.
    pub role: InjectionRole,
    /// Verbatim turn content.
    pub content: String,
}

/// Override of the sampler's loop decision at a turn boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TurnControl {
    /// No override — the sampler proceeds with its own completion logic.
    #[default]
    Auto,
    /// Force another turn even if the model ended without a tool call.
    ForceContinue,
    /// Force the loop to stop after this turn.
    ForceStop,
}

/// Reply to a [`TurnHookRequest`]: turns to inject plus a loop-control decision; default (`{}`) is a no-op.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookReply {
    /// Turns to append before the next sampling step, in order.
    #[serde(default)]
    pub injections: Vec<HookInjection>,
    /// Optional loop-control override.
    #[serde(default)]
    pub control: TurnControl,
    /// Artifact-handling ack for a [`TurnHookRequest::After`] request; `None`
    /// on `Before` replies and from workspaces that predate the ack.
    /// Informational only — the requester never gates its loop on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_turn_ack: Option<AfterTurnAckPayload>,
}

/// Terminal status of the workspace's per-turn artifact handling, carried in
/// the [`AfterTurnAckPayload`] the workspace sends back to the shell.
///
/// The variants are wire-stable snake_case strings; the shell routes on them
/// to decide how to record the turn's data-collection outcome. The ack
/// is informational — the shell never blocks its agent loop on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AfterTurnAckStatus {
    /// Every archive the workspace attempted was durably handed off to its
    /// upload queue (written to the on-disk spill, or an inline-fallback
    /// upload is in flight). The cloud upload then proceeds independently with
    /// the queue's own retry policy. The caller MAY advance.
    Enqueued,
    /// At least one archive could not be handed off (temp file unwritable,
    /// queue worker shut down, or the archive build failed). The workspace has
    /// done what it can — the caller MUST NOT retry.
    Failed,
    /// The workspace skipped uploads before touching disk (no upload queue
    /// configured / not in proxy mode). `error_message` carries the reason.
    Skipped,
}

/// Artifact-handling ack the workspace returns for a
/// [`TurnHookRequest::After`] request on [`HookReply::after_turn_ack`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AfterTurnAckPayload {
    /// The turn this ack corresponds to (matches `AfterTurnPayload::turn_number`).
    pub turn_number: u64,
    /// Terminal artifact-handling status for the turn.
    pub status: AfterTurnAckStatus,
    /// Failure / skip reason. `Some` only for [`AfterTurnAckStatus::Failed`] or
    /// [`AfterTurnAckStatus::Skipped`]; omitted from the wire when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Count of archives this turn that landed durably on the queue's on-disk
    /// spill — `0`, `1`, or `2` (before/after repository snapshot archives).
    /// Informational; defaults to `0` for back-compat.
    #[serde(default)]
    pub artifact_count: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn before_turn_round_trip() {
        let payload = BeforeTurnPayload {
            turn_number: 42,
            model_id: "echo-agent-3".to_string(),
            yolo_mode: true,
            conversation_message_count: 9,
            session_relationship: "subagent".to_string(),
            schema_version: "1.0".to_string(),
        };

        let serialized = serde_json::to_value(&payload).unwrap();
        assert_eq!(
            serialized,
            json!({
                "turn_number": 42,
                "model_id": "echo-agent-3",
                "yolo_mode": true,
                "conversation_message_count": 9,
                "session_relationship": "subagent",
                "schema_version": "1.0",
            })
        );

        let deserialized: BeforeTurnPayload = serde_json::from_value(serialized).unwrap();
        assert_eq!(deserialized, payload);
    }

    #[test]
    fn before_turn_yolo_mode_defaults_false() {
        let json = json!({
            "turn_number": 1,
            "model_id": "echo-agent-3",
        });
        let payload: BeforeTurnPayload = serde_json::from_value(json).unwrap();
        assert!(!payload.yolo_mode);
    }

    #[test]
    fn after_turn_round_trip() {
        // Completed turn: both cancellation fields are `None` and therefore
        // skip serialization — the wire shape is byte-identical to the legacy shape.
        let payload = AfterTurnPayload {
            turn_number: 42,
            outcome: TurnHookOutcome::Completed,
            duration_ms: 1500,
            tool_call_count: 3,
            model_id: "echo-agent-3".to_string(),
            written_repo_paths: vec!["outputs/result.md".to_string()],
            cancellation_category: None,
            cancellation_context: None,
        };

        let serialized = serde_json::to_value(&payload).unwrap();
        assert_eq!(
            serialized,
            json!({
                "turn_number": 42,
                "outcome": "completed",
                "duration_ms": 1500,
                "tool_call_count": 3,
                "model_id": "echo-agent-3",
                "written_repo_paths": ["outputs/result.md"],
            })
        );

        let deserialized: AfterTurnPayload = serde_json::from_value(serialized).unwrap();
        assert_eq!(deserialized, payload);
    }

    #[test]
    fn after_turn_written_repo_paths_defaults_empty() {
        let json = json!({
            "turn_number": 1,
            "outcome": "completed",
            "duration_ms": 10,
            "tool_call_count": 0,
            "model_id": "echo-agent-3",
        });
        let payload: AfterTurnPayload = serde_json::from_value(json).unwrap();
        assert!(payload.written_repo_paths.is_empty());
    }

    #[test]
    fn after_turn_round_trip_with_cancellation_fields() {
        let payload = AfterTurnPayload {
            turn_number: 7,
            outcome: TurnHookOutcome::Cancelled,
            duration_ms: 200,
            tool_call_count: 1,
            model_id: "echo-agent-4".to_string(),
            written_repo_paths: vec![],
            cancellation_category: Some("doom_loop_repetition".to_string()),
            cancellation_context: Some(json!({ "reason": "repetition" })),
        };

        let serialized = serde_json::to_value(&payload).unwrap();
        assert_eq!(
            serialized["cancellation_category"],
            json!("doom_loop_repetition")
        );
        assert_eq!(
            serialized["cancellation_context"],
            json!({ "reason": "repetition" })
        );

        let deserialized: AfterTurnPayload = serde_json::from_value(serialized).unwrap();
        assert_eq!(deserialized, payload);
    }

    #[test]
    fn outcome_variants_serialize_snake_case() {
        assert_eq!(
            serde_json::to_value(TurnHookOutcome::Completed).unwrap(),
            json!("completed"),
        );
        assert_eq!(
            serde_json::to_value(TurnHookOutcome::Cancelled).unwrap(),
            json!("cancelled"),
        );
        assert_eq!(
            serde_json::to_value(TurnHookOutcome::Error).unwrap(),
            json!("error"),
        );
    }

    #[test]
    fn outcome_deserializes_from_snake_case() {
        assert_eq!(
            serde_json::from_value::<TurnHookOutcome>(json!("completed")).unwrap(),
            TurnHookOutcome::Completed,
        );
        assert_eq!(
            serde_json::from_value::<TurnHookOutcome>(json!("cancelled")).unwrap(),
            TurnHookOutcome::Cancelled,
        );
        assert_eq!(
            serde_json::from_value::<TurnHookOutcome>(json!("error")).unwrap(),
            TurnHookOutcome::Error,
        );
    }

    #[test]
    fn kind_constants() {
        assert_eq!(BEFORE_TURN_KIND, "before_turn");
        assert_eq!(AFTER_TURN_KIND, "after_turn");
        assert_eq!(DEFAULT_SESSION_RELATIONSHIP, "primary");
        assert_eq!(DEFAULT_SCHEMA_VERSION, "1.0");
    }

    #[test]
    fn unknown_outcome_variant_rejected() {
        let result = serde_json::from_value::<TurnHookOutcome>(json!("timeout"));
        assert!(result.is_err());
    }

    #[test]
    fn after_turn_missing_required_field_rejected() {
        let json = json!({
            "turn_number": 1,
            "duration_ms": 100,
            "tool_call_count": 0,
            "model_id": "echo-agent-3",
        });
        assert!(serde_json::from_value::<AfterTurnPayload>(json).is_err());
    }

    #[test]
    fn extra_fields_ignored() {
        let json = json!({
            "turn_number": 1,
            "model_id": "echo-agent-3",
            "future_field": "should be ignored",
        });
        let payload: BeforeTurnPayload = serde_json::from_value(json).unwrap();
        assert_eq!(payload.turn_number, 1);
    }

    #[test]
    fn before_turn_yolo_false_serialized() {
        let payload = BeforeTurnPayload {
            turn_number: 1,
            model_id: "echo-agent-3".to_string(),
            yolo_mode: false,
            conversation_message_count: 0,
            session_relationship: "primary".to_string(),
            schema_version: "1.0".to_string(),
        };
        let serialized = serde_json::to_value(&payload).unwrap();
        assert_eq!(serialized["yolo_mode"], json!(false));
    }

    #[test]
    fn turn_hook_kind_constant() {
        assert_eq!(TURN_HOOK_KIND, "turn_hook");
    }

    #[test]
    fn turn_hook_request_before_round_trip() {
        let req = TurnHookRequest::Before(BeforeTurnPayload {
            turn_number: 7,
            model_id: "echo-agent-3".to_string(),
            yolo_mode: true,
            conversation_message_count: 0,
            session_relationship: "primary".to_string(),
            schema_version: "1.0".to_string(),
        });
        let serialized = serde_json::to_value(&req).unwrap();
        assert_eq!(
            serialized,
            json!({
                "phase": "before",
                "turn_number": 7,
                "model_id": "echo-agent-3",
                "yolo_mode": true,
                "conversation_message_count": 0,
                "session_relationship": "primary",
                "schema_version": "1.0",
            })
        );
        let deserialized: TurnHookRequest = serde_json::from_value(serialized).unwrap();
        assert_eq!(deserialized, req);
    }

    #[test]
    fn turn_hook_request_after_round_trip() {
        let req = TurnHookRequest::After(AfterTurnPayload {
            turn_number: 7,
            outcome: TurnHookOutcome::Completed,
            duration_ms: 10,
            tool_call_count: 2,
            model_id: "echo-agent-3".to_string(),
            written_repo_paths: Vec::new(),
            cancellation_category: None,
            cancellation_context: None,
        });
        let serialized = serde_json::to_value(&req).unwrap();
        assert_eq!(serialized["phase"], json!("after"));
        assert_eq!(serialized["tool_call_count"], json!(2));
        let deserialized: TurnHookRequest = serde_json::from_value(serialized).unwrap();
        assert_eq!(deserialized, req);
    }

    #[test]
    fn hook_reply_default_is_empty_auto() {
        let reply = HookReply::default();
        assert!(reply.injections.is_empty());
        assert_eq!(reply.control, TurnControl::Auto);
        assert_eq!(reply.after_turn_ack, None);
        // `None` must skip serialization so the default reply stays the legacy
        // `{}`-compatible shape (old decoders use `deny_unknown_fields`).
        let serialized = serde_json::to_value(&reply).unwrap();
        assert!(serialized.get("after_turn_ack").is_none());
    }

    /// An `After` reply carrying the ack round-trips, and a legacy reply
    /// without the field decodes with `after_turn_ack == None`.
    #[test]
    fn hook_reply_after_turn_ack_round_trip_and_legacy_decode() {
        let reply = HookReply {
            injections: vec![],
            control: TurnControl::Auto,
            after_turn_ack: Some(AfterTurnAckPayload {
                turn_number: 7,
                status: AfterTurnAckStatus::Enqueued,
                error_message: None,
                artifact_count: 2,
            }),
        };
        let serialized = serde_json::to_value(&reply).unwrap();
        assert_eq!(serialized["after_turn_ack"]["turn_number"], json!(7));
        assert_eq!(serialized["after_turn_ack"]["status"], json!("enqueued"));
        let deserialized: HookReply = serde_json::from_value(serialized).unwrap();
        assert_eq!(deserialized, reply);

        let legacy: HookReply =
            serde_json::from_value(json!({"injections": [], "control": "auto"})).unwrap();
        assert_eq!(legacy.after_turn_ack, None);
    }

    #[test]
    fn hook_reply_deserializes_from_empty_object() {
        let reply: HookReply = serde_json::from_value(json!({})).unwrap();
        assert_eq!(reply, HookReply::default());
    }

    #[test]
    fn hook_reply_rejects_unknown_field() {
        let result: Result<HookReply, _> =
            serde_json::from_value(json!({"injection": [], "control": "auto"}));
        assert!(result.is_err());
    }

    #[test]
    fn hook_reply_round_trip() {
        let reply = HookReply {
            injections: vec![
                HookInjection {
                    role: InjectionRole::System,
                    content: "Available channels: response".to_string(),
                },
                HookInjection {
                    role: InjectionRole::User,
                    content: "<system-reminder>\nkeep going\n</system-reminder>".to_string(),
                },
            ],
            control: TurnControl::ForceContinue,
            after_turn_ack: None,
        };
        let serialized = serde_json::to_value(&reply).unwrap();
        assert_eq!(
            serialized,
            json!({
                "injections": [
                    { "role": "system", "content": "Available channels: response" },
                    {
                        "role": "user",
                        "content": "<system-reminder>\nkeep going\n</system-reminder>",
                    },
                ],
                "control": "force_continue",
            })
        );
        let deserialized: HookReply = serde_json::from_value(serialized).unwrap();
        assert_eq!(deserialized, reply);
    }

    #[test]
    fn turn_control_variants_serialize_snake_case() {
        assert_eq!(
            serde_json::to_value(TurnControl::Auto).unwrap(),
            json!("auto")
        );
        assert_eq!(
            serde_json::to_value(TurnControl::ForceContinue).unwrap(),
            json!("force_continue"),
        );
        assert_eq!(
            serde_json::to_value(TurnControl::ForceStop).unwrap(),
            json!("force_stop"),
        );
    }

    #[test]
    fn injection_role_serializes_snake_case() {
        assert_eq!(
            serde_json::to_value(InjectionRole::Developer).unwrap(),
            json!("developer"),
        );
    }

    /// Back-compat: a `before_turn` payload from an OLD shell (without the extended fields)
    /// must still deserialize, with the new fields taking their serde defaults.
    #[test]
    fn before_turn_legacy_payload_defaults_new_fields() {
        let json = json!({
            "turn_number": 3,
            "model_id": "echo-agent-3",
            "yolo_mode": true,
        });
        let payload: BeforeTurnPayload = serde_json::from_value(json).unwrap();
        assert_eq!(payload.conversation_message_count, 0);
        assert_eq!(payload.session_relationship, DEFAULT_SESSION_RELATIONSHIP);
        assert_eq!(payload.schema_version, DEFAULT_SCHEMA_VERSION);
    }

    /// Back-compat: an `after_turn` payload from an OLD shell (without the
    /// cancellation fields) must still deserialize, defaulting both to `None`.
    #[test]
    fn after_turn_legacy_payload_defaults_new_fields() {
        let json = json!({
            "turn_number": 3,
            "outcome": "completed",
            "duration_ms": 10,
            "tool_call_count": 0,
            "model_id": "echo-agent-3",
        });
        let payload: AfterTurnPayload = serde_json::from_value(json).unwrap();
        assert_eq!(payload.cancellation_category, None);
        assert_eq!(payload.cancellation_context, None);
    }

    #[test]
    fn after_turn_ack_status_serializes_snake_case() {
        assert_eq!(
            serde_json::to_value(AfterTurnAckStatus::Enqueued).unwrap(),
            json!("enqueued"),
        );
        assert_eq!(
            serde_json::to_value(AfterTurnAckStatus::Failed).unwrap(),
            json!("failed"),
        );
        assert_eq!(
            serde_json::to_value(AfterTurnAckStatus::Skipped).unwrap(),
            json!("skipped"),
        );
    }

    #[test]
    fn after_turn_ack_payload_round_trip_enqueued() {
        // `Enqueued` ack with no error message: `error_message` skips the wire.
        let payload = AfterTurnAckPayload {
            turn_number: 42,
            status: AfterTurnAckStatus::Enqueued,
            error_message: None,
            artifact_count: 2,
        };
        let serialized = serde_json::to_value(&payload).unwrap();
        assert_eq!(
            serialized,
            json!({
                "turn_number": 42,
                "status": "enqueued",
                "artifact_count": 2,
            })
        );
        let deserialized: AfterTurnAckPayload = serde_json::from_value(serialized).unwrap();
        assert_eq!(deserialized, payload);
    }

    #[test]
    fn after_turn_ack_payload_round_trip_failed_carries_message() {
        let payload = AfterTurnAckPayload {
            turn_number: 1,
            status: AfterTurnAckStatus::Failed,
            error_message: Some("disk budget exhausted".to_string()),
            artifact_count: 1,
        };
        let serialized = serde_json::to_value(&payload).unwrap();
        assert_eq!(serialized["status"], json!("failed"));
        assert_eq!(serialized["error_message"], json!("disk budget exhausted"));
        assert_eq!(serialized["artifact_count"], json!(1));
        let deserialized: AfterTurnAckPayload = serde_json::from_value(serialized).unwrap();
        assert_eq!(deserialized, payload);
    }

    /// Back-compat: an ack with only the required fields (old sender) defaults
    /// `artifact_count` to 0 and `error_message` to `None`.
    #[test]
    fn after_turn_ack_payload_minimal_defaults() {
        let json = json!({
            "turn_number": 5,
            "status": "skipped",
        });
        let payload: AfterTurnAckPayload = serde_json::from_value(json).unwrap();
        assert_eq!(payload.status, AfterTurnAckStatus::Skipped);
        assert_eq!(payload.artifact_count, 0);
        assert_eq!(payload.error_message, None);
    }
}
