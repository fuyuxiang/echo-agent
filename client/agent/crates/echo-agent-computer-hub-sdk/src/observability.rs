//! Server-side session event emitter.
//!
//! [`ObservabilityBridge`] is a thin facade for emitting session-level
//! events (turn lifecycle, phase changes) to the connected server. Tool-call events
//! (`ToolCallStarted` / `ToolCallCompleted`) are emitted automatically
//! by [`crate::harness::ToolHarness::call`] and do not need the bridge.
//!
//! The caller is responsible for also emitting to the local sink
//! (`EventTracker` in the shell, `EventProcPublisher` in the
//! chat service) — the bridge handles only the server leg.
//!
//! This separation is deliberate: each sampler's local sink has a
//! different type and API surface.  Forcing a trait/callback into the
//! bridge would add abstraction overhead without benefit, since the
//! call sites already have the local sink in scope.

use std::sync::Arc;

use echo_agent_tool_protocol::{SessionId, session_event::SessionEvent};

use crate::harness::ToolHarness;

/// Emits [`SessionEvent`]s to the connected server as `ToolNotificationFrame` custom
/// notifications with `kind = "session_event"`.
///
/// No-ops gracefully when no harness is present (i.e. `harness` is
/// `None`).  Server notification failures are silently ignored — the bridge
/// is fire-and-forget so server issues never affect the sampler's main loop.
///
/// Callers MUST also emit to their local sink separately:
/// - Shell: `self.events.emit(Event::...)`
/// - Chat service: `publisher.publish_agent_event(...)`
pub struct ObservabilityBridge {
    harness: Option<Arc<ToolHarness>>,
    /// Retained for future payload enrichment and logging.
    session_id: SessionId,
}

impl ObservabilityBridge {
    pub fn new(harness: Option<Arc<ToolHarness>>, session_id: SessionId) -> Self {
        Self {
            harness,
            session_id,
        }
    }

    /// The session id this bridge was created for.
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Whether a harness is present (i.e. server emission is active).
    pub fn has_harness(&self) -> bool {
        self.harness.is_some()
    }

    /// Emit a session event to the connected server.  No-ops if no harness is present.
    ///
    /// Delegates frame construction + wire dispatch to
    /// [`ToolHarness::emit_session_event`] so the SDK keeps a single
    /// canonical encoding path.
    ///
    /// Callers MUST also emit to their local sink separately:
    /// - Shell: `self.events.emit(Event::...)`
    /// - Chat service: `publisher.publish_agent_event(...)`
    pub async fn emit(&self, event: SessionEvent) {
        let event_type = match &event {
            SessionEvent::TurnStarted { .. } => "turn_started",
            SessionEvent::TurnEnded { .. } => "turn_ended",
            SessionEvent::ToolCallStarted { .. } => "tool_call_started",
            SessionEvent::ToolCallCompleted { .. } => "tool_call_completed",
            SessionEvent::PhaseChanged { .. } => "phase_changed",
            SessionEvent::Unknown => "unknown",
        };
        crate::metrics::session_event(event_type);
        if let Some(harness) = &self.harness {
            harness.emit_session_event(event).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_tool_protocol::session_event::{SessionEvent, SessionPhase, ToolCallOutcome};
    use echo_agent_tool_protocol::turn_hook::TurnHookOutcome;

    fn test_session_id() -> SessionId {
        SessionId::new("test-obs-session").expect("valid")
    }

    // ── No-harness path ─────────────────────────────────────────────

    #[tokio::test]
    async fn emit_without_harness_is_noop() {
        let bridge = ObservabilityBridge::new(None, test_session_id());
        // Must not panic and should return immediately.
        bridge
            .emit(SessionEvent::TurnStarted {
                turn_number: 1,
                model_id: "echo-agent-3".into(),
                yolo_mode: false,
            })
            .await;
    }

    #[test]
    fn has_harness_returns_false_when_none() {
        let bridge = ObservabilityBridge::new(None, test_session_id());
        assert!(!bridge.has_harness());
    }

    // ── Constructor field storage ───────────────────────────────────

    #[test]
    fn new_stores_session_id() {
        let sid = test_session_id();
        let bridge = ObservabilityBridge::new(None, sid.clone());
        assert_eq!(bridge.session_id(), &sid);
    }

    #[test]
    fn has_harness_returns_true_when_present() {
        let harness = ToolHarness::local_only_with(
            crate::harness::LocalRegistry::new(),
            test_session_id(),
            Default::default(),
        );
        let bridge = ObservabilityBridge::new(Some(Arc::new(harness)), test_session_id());
        assert!(bridge.has_harness());
    }

    // ── Serialization correctness ───────────────────────────────────

    #[test]
    fn session_event_serializes_to_expected_json() {
        let event = SessionEvent::TurnStarted {
            turn_number: 1,
            model_id: "echo-agent-3".into(),
            yolo_mode: true,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["event_type"], "turn_started");
        assert_eq!(value["turn_number"], 1);
        assert_eq!(value["model_id"], "echo-agent-3");
        assert_eq!(value["yolo_mode"], true);
    }

    #[test]
    fn session_event_turn_ended_serializes_correctly() {
        let event = SessionEvent::TurnEnded {
            turn_number: 5,
            outcome: TurnHookOutcome::Completed,
            duration_ms: 3200,
            tool_call_count: 12,
            model_id: "echo-agent-3".into(),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["event_type"], "turn_ended");
        assert_eq!(value["outcome"], "completed");
        assert_eq!(value["tool_call_count"], 12);
    }

    #[test]
    fn session_event_tool_call_completed_serializes_correctly() {
        let event = SessionEvent::ToolCallCompleted {
            tool_call_id: "call-1".into(),
            tool_name: "bash".into(),
            duration_ms: 500,
            outcome: ToolCallOutcome::Success,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["event_type"], "tool_call_completed");
        assert_eq!(value["outcome"], "success");
    }

    #[test]
    fn session_event_phase_changed_serializes_correctly() {
        let event = SessionEvent::PhaseChanged {
            phase: SessionPhase::Sampling,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["event_type"], "phase_changed");
        assert_eq!(value["phase"], "sampling");
    }

    // Frame-construction invariants moved to `crate::harness` where the
    // builder now lives (`ToolHarness::emit_session_event`).

    // ── End-to-end with local-only harness ───────────────────────────

    #[tokio::test]
    async fn emit_with_local_only_harness_does_not_panic() {
        // A local-only harness has no server connection, so
        // `send_notification` returns `Err` — but the bridge ignores
        // errors, so this must succeed silently.
        let harness = ToolHarness::local_only_with(
            crate::harness::LocalRegistry::new(),
            test_session_id(),
            Default::default(),
        );
        let bridge = ObservabilityBridge::new(Some(Arc::new(harness)), test_session_id());
        bridge
            .emit(SessionEvent::PhaseChanged {
                phase: SessionPhase::ToolExecution,
            })
            .await;
    }

    #[tokio::test]
    async fn emit_all_event_variants_does_not_panic() {
        let harness = ToolHarness::local_only_with(
            crate::harness::LocalRegistry::new(),
            test_session_id(),
            Default::default(),
        );
        let bridge = ObservabilityBridge::new(Some(Arc::new(harness)), test_session_id());

        // Smoke test: every variant emits without panic through a
        // local-only harness. Includes error/cancelled outcomes to
        // cover non-happy-path enum values.
        let events = vec![
            SessionEvent::TurnStarted {
                turn_number: 1,
                model_id: "echo-agent-3".into(),
                yolo_mode: false,
            },
            SessionEvent::ToolCallStarted {
                tool_call_id: "c1".into(),
                tool_name: "bash".into(),
                turn_number: 1,
            },
            SessionEvent::ToolCallCompleted {
                tool_call_id: "c1".into(),
                tool_name: "bash".into(),
                duration_ms: 100,
                outcome: ToolCallOutcome::Success,
            },
            SessionEvent::ToolCallCompleted {
                tool_call_id: "c2".into(),
                tool_name: "read_file".into(),
                duration_ms: 50,
                outcome: ToolCallOutcome::Error,
            },
            SessionEvent::ToolCallCompleted {
                tool_call_id: "c3".into(),
                tool_name: "grep".into(),
                duration_ms: 10,
                outcome: ToolCallOutcome::Cancelled,
            },
            SessionEvent::PhaseChanged {
                phase: SessionPhase::Idle,
            },
            SessionEvent::TurnEnded {
                turn_number: 1,
                outcome: TurnHookOutcome::Completed,
                duration_ms: 500,
                tool_call_count: 3,
                model_id: "echo-agent-3".into(),
            },
            SessionEvent::TurnEnded {
                turn_number: 2,
                outcome: TurnHookOutcome::Error,
                duration_ms: 100,
                tool_call_count: 0,
                model_id: "echo-agent-3".into(),
            },
            SessionEvent::Unknown,
        ];

        for event in events {
            bridge.emit(event).await;
        }
    }
}
