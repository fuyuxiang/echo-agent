//! Session lifecycle events designed to ride inside
//! `ToolNotificationFrame` as `Custom` notifications with
//! `kind = "session_event"`. They will provide a unified view of
//! turn/tool activity across both samplers once the emitting side
//! is wired up.

use serde::{Deserialize, Serialize};

use crate::turn_hook::TurnHookOutcome;

/// Session lifecycle event.
///
/// Serialized with an internally-tagged `event_type` discriminator so
/// consumers can match on the string tag before deserializing the rest.
///
/// The `Unknown` variant acts as a forward-compatibility catch-all:
/// older consumers that encounter a new `event_type` value deserialize
/// it as `Unknown` instead of failing.  Consumers MUST silently ignore
/// `Unknown` events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event_type", rename_all = "snake_case")]
pub enum SessionEvent {
    /// Fields mirror [`crate::turn_hook::BeforeTurnPayload`] but are
    /// structurally independent — this is a notification event, not a
    /// hook payload.
    TurnStarted {
        turn_number: u64,
        model_id: String,
        #[serde(default)]
        yolo_mode: bool,
    },
    /// Fields mirror [`crate::turn_hook::AfterTurnPayload`] but are
    /// structurally independent — this is a notification event, not a
    /// hook payload.
    TurnEnded {
        turn_number: u64,
        outcome: TurnHookOutcome,
        duration_ms: u64,
        tool_call_count: u32,
        model_id: String,
    },
    ToolCallStarted {
        tool_call_id: String,
        tool_name: String,
        turn_number: u64,
    },
    ToolCallCompleted {
        tool_call_id: String,
        tool_name: String,
        duration_ms: u64,
        outcome: ToolCallOutcome,
    },
    PhaseChanged {
        phase: SessionPhase,
    },
    /// Forward-compatibility catch-all. Older consumers that encounter
    /// a new `event_type` value deserialize it as `Unknown` instead of
    /// failing.  Consumers MUST silently ignore `Unknown` events.
    ///
    /// The original `event_type` value is not preserved; consumers that
    /// need to log unrecognized types should inspect the raw JSON before
    /// deserializing into `SessionEvent`.
    #[serde(other)]
    Unknown,
}

/// Outcome of a completed tool call within a session event.
///
/// The `Unknown` variant is a forward-compatibility catch-all for
/// variants added in newer protocol versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallOutcome {
    Success,
    Error,
    Cancelled,
    #[serde(other)]
    Unknown,
}

/// Current phase of the session lifecycle.
///
/// The `Unknown` variant is a forward-compatibility catch-all for
/// phases added in newer protocol versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPhase {
    Idle,
    Sampling,
    ToolExecution,
    PermissionPrompt,
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── SessionEvent round-trip tests ────────────────────────────────

    #[test]
    fn turn_started_round_trip() {
        let event = SessionEvent::TurnStarted {
            turn_number: 1,
            model_id: "echo-agent-3".into(),
            yolo_mode: true,
        };
        let v = serde_json::to_value(&event).unwrap();
        assert_eq!(v["event_type"], "turn_started");
        assert_eq!(v["turn_number"], 1);
        assert_eq!(v["model_id"], "echo-agent-3");
        assert_eq!(v["yolo_mode"], true);
        let back: SessionEvent = serde_json::from_value(v).unwrap();
        assert_eq!(back, event);
    }

    #[test]
    fn turn_started_yolo_mode_defaults_false() {
        let v = json!({
            "event_type": "turn_started",
            "turn_number": 5,
            "model_id": "echo-agent-3",
        });
        let event: SessionEvent = serde_json::from_value(v).unwrap();
        assert_eq!(
            event,
            SessionEvent::TurnStarted {
                turn_number: 5,
                model_id: "echo-agent-3".into(),
                yolo_mode: false,
            }
        );
    }

    #[test]
    fn turn_ended_round_trip() {
        let event = SessionEvent::TurnEnded {
            turn_number: 3,
            outcome: TurnHookOutcome::Completed,
            duration_ms: 2500,
            tool_call_count: 7,
            model_id: "echo-agent-3".into(),
        };
        let v = serde_json::to_value(&event).unwrap();
        assert_eq!(v["event_type"], "turn_ended");
        assert_eq!(v["outcome"], "completed");
        assert_eq!(v["duration_ms"], 2500);
        assert_eq!(v["tool_call_count"], 7);
        let back: SessionEvent = serde_json::from_value(v).unwrap();
        assert_eq!(back, event);
    }

    #[test]
    fn turn_ended_uses_turn_hook_outcome_variants() {
        for (outcome, expected_str) in [
            (TurnHookOutcome::Completed, "completed"),
            (TurnHookOutcome::Cancelled, "cancelled"),
            (TurnHookOutcome::Error, "error"),
        ] {
            let event = SessionEvent::TurnEnded {
                turn_number: 1,
                outcome,
                duration_ms: 100,
                tool_call_count: 0,
                model_id: "m".into(),
            };
            let v = serde_json::to_value(&event).unwrap();
            assert_eq!(v["outcome"], expected_str, "TurnHookOutcome::{outcome:?}");
            let back: SessionEvent = serde_json::from_value(v).unwrap();
            assert_eq!(back, event);
        }
    }

    #[test]
    fn tool_call_started_round_trip() {
        let event = SessionEvent::ToolCallStarted {
            tool_call_id: "call-42".into(),
            tool_name: "read_file".into(),
            turn_number: 2,
        };
        let v = serde_json::to_value(&event).unwrap();
        assert_eq!(v["event_type"], "tool_call_started");
        assert_eq!(v["tool_call_id"], "call-42");
        assert_eq!(v["tool_name"], "read_file");
        let back: SessionEvent = serde_json::from_value(v).unwrap();
        assert_eq!(back, event);
    }

    #[test]
    fn tool_call_completed_round_trip_all_outcomes() {
        for (outcome, expected_str) in [
            (ToolCallOutcome::Success, "success"),
            (ToolCallOutcome::Error, "error"),
            (ToolCallOutcome::Cancelled, "cancelled"),
        ] {
            let event = SessionEvent::ToolCallCompleted {
                tool_call_id: "call-42".into(),
                tool_name: "read_file".into(),
                duration_ms: 350,
                outcome,
            };
            let v = serde_json::to_value(&event).unwrap();
            assert_eq!(v["event_type"], "tool_call_completed");
            assert_eq!(v["outcome"], expected_str, "ToolCallOutcome::{outcome:?}");
            let back: SessionEvent = serde_json::from_value(v).unwrap();
            assert_eq!(back, event);
        }
    }

    #[test]
    fn phase_changed_round_trip_all_phases() {
        for (phase, expected_str) in [
            (SessionPhase::Idle, "idle"),
            (SessionPhase::Sampling, "sampling"),
            (SessionPhase::ToolExecution, "tool_execution"),
            (SessionPhase::PermissionPrompt, "permission_prompt"),
        ] {
            let event = SessionEvent::PhaseChanged { phase };
            let v = serde_json::to_value(&event).unwrap();
            assert_eq!(v["event_type"], "phase_changed");
            assert_eq!(v["phase"], expected_str, "SessionPhase::{phase:?}");
            let back: SessionEvent = serde_json::from_value(v).unwrap();
            assert_eq!(back, event);
        }
    }

    // ── #[serde(other)] backward-compat ─────────────────────────────

    #[test]
    fn unknown_event_type_deserializes_as_unknown() {
        let v = json!({ "event_type": "some_future_event", "extra": 123 });
        let event: SessionEvent = serde_json::from_value(v).unwrap();
        assert_eq!(event, SessionEvent::Unknown);
    }

    #[test]
    fn another_unknown_event_type_deserializes_as_unknown() {
        let v = json!({ "event_type": "metrics_snapshot", "ts": 0 });
        let event: SessionEvent = serde_json::from_value(v).unwrap();
        assert_eq!(event, SessionEvent::Unknown);
    }

    // ── ToolCallOutcome serialization ───────────────────────────────

    #[test]
    fn tool_call_outcome_snake_case() {
        for (variant, expected) in [
            (ToolCallOutcome::Success, "success"),
            (ToolCallOutcome::Error, "error"),
            (ToolCallOutcome::Cancelled, "cancelled"),
            (ToolCallOutcome::Unknown, "unknown"),
        ] {
            let v = serde_json::to_value(variant).unwrap();
            assert_eq!(v.as_str(), Some(expected), "ToolCallOutcome::{variant:?}");
            let back: ToolCallOutcome = serde_json::from_value(v).unwrap();
            assert_eq!(back, variant);
        }
    }

    // ── SessionPhase serialization ──────────────────────────────────

    #[test]
    fn session_phase_snake_case() {
        for (variant, expected) in [
            (SessionPhase::Idle, "idle"),
            (SessionPhase::Sampling, "sampling"),
            (SessionPhase::ToolExecution, "tool_execution"),
            (SessionPhase::PermissionPrompt, "permission_prompt"),
            (SessionPhase::Unknown, "unknown"),
        ] {
            let v = serde_json::to_value(variant).unwrap();
            assert_eq!(v.as_str(), Some(expected), "SessionPhase::{variant:?}");
            let back: SessionPhase = serde_json::from_value(v).unwrap();
            assert_eq!(back, variant);
        }
    }

    // ── Forward-compat: inner enum Unknown ──────────────────────────

    #[test]
    fn tool_call_outcome_unknown_variant_on_future_value() {
        let back: ToolCallOutcome = serde_json::from_value(json!("timeout")).unwrap();
        assert_eq!(back, ToolCallOutcome::Unknown);
    }

    #[test]
    fn session_phase_unknown_variant_on_future_value() {
        let back: SessionPhase = serde_json::from_value(json!("cleanup")).unwrap();
        assert_eq!(back, SessionPhase::Unknown);
    }

    #[test]
    fn tool_call_completed_with_unknown_outcome_deserializes() {
        let v = json!({
            "event_type": "tool_call_completed",
            "tool_call_id": "call-99",
            "tool_name": "future_tool",
            "duration_ms": 42,
            "outcome": "timeout",
        });
        let event: SessionEvent = serde_json::from_value(v).unwrap();
        assert_eq!(
            event,
            SessionEvent::ToolCallCompleted {
                tool_call_id: "call-99".into(),
                tool_name: "future_tool".into(),
                duration_ms: 42,
                outcome: ToolCallOutcome::Unknown,
            }
        );
    }

    #[test]
    fn phase_changed_with_unknown_phase_deserializes() {
        let v = json!({
            "event_type": "phase_changed",
            "phase": "cleanup",
        });
        let event: SessionEvent = serde_json::from_value(v).unwrap();
        assert_eq!(
            event,
            SessionEvent::PhaseChanged {
                phase: SessionPhase::Unknown,
            }
        );
    }

    // ── Unknown variant serialization ───────────────────────────────

    #[test]
    fn unknown_variant_serializes_as_expected() {
        let v = serde_json::to_value(SessionEvent::Unknown).unwrap();
        assert_eq!(v, json!({"event_type": "unknown"}));
    }

    // ── Extra/unknown fields on known variants ──────────────────────

    #[test]
    fn extra_fields_ignored_on_known_variant() {
        let v = json!({
            "event_type": "turn_started",
            "turn_number": 1,
            "model_id": "echo-agent-3",
            "future_field": "should be ignored",
        });
        let event: SessionEvent = serde_json::from_value(v).unwrap();
        assert_eq!(
            event,
            SessionEvent::TurnStarted {
                turn_number: 1,
                model_id: "echo-agent-3".into(),
                yolo_mode: false,
            }
        );
    }

    // ── Negative: missing required fields ───────────────────────────

    #[test]
    fn turn_ended_missing_required_field_rejected() {
        let v = json!({
            "event_type": "turn_ended",
            "turn_number": 1,
            "duration_ms": 100,
            "tool_call_count": 0,
            "model_id": "echo-agent-3",
            // missing "outcome"
        });
        assert!(serde_json::from_value::<SessionEvent>(v).is_err());
    }

    // ── Boundary values ─────────────────────────────────────────────

    #[test]
    fn turn_number_zero_and_max() {
        for turn_number in [0, u64::MAX] {
            let event = SessionEvent::TurnStarted {
                turn_number,
                model_id: "m".into(),
                yolo_mode: false,
            };
            let v = serde_json::to_value(&event).unwrap();
            let back: SessionEvent = serde_json::from_value(v).unwrap();
            assert_eq!(back, event);
        }
    }

    #[test]
    fn duration_ms_zero() {
        let event = SessionEvent::TurnEnded {
            turn_number: 0,
            outcome: TurnHookOutcome::Completed,
            duration_ms: 0,
            tool_call_count: 0,
            model_id: "m".into(),
        };
        let v = serde_json::to_value(&event).unwrap();
        assert_eq!(v["duration_ms"], 0);
        let back: SessionEvent = serde_json::from_value(v).unwrap();
        assert_eq!(back, event);
    }
}
