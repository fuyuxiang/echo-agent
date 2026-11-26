//! Numeric ↔ string error-code mapping.
//!
//! Receivers SHOULD switch on `data.code` (the snake_case string) rather
//! than the numeric JSON-RPC `error.code`. The numeric is the JSON-RPC
//! envelope code; the string is the EchoAgent stable identifier.
//!
//! Implemented as a `&'static [(i32, &'static str)]` table; the table is
//! a small fixed set so a linear scan is faster than any
//! `HashMap`/`OnceLock`-shaped alternative.

use serde::{Deserialize, Serialize};

use crate::error_wire::ToolErrorWire;

/// `(numeric_code, string_code)` pairs. Both columns are unique.
pub const ERROR_CODES: &[(i32, &str)] = &[
    (-32700, "parse_error"),
    (-32600, "invalid_request"),
    (-32601, "method_not_found"),
    (-32602, "invalid_params"),
    (-32603, "internal_error"),
    (-32605, "unsupported_protocol_version"),
    (-32001, "timeout"),
    (-32002, "unauthorized"),
    (-32003, "forbidden"),
    (-32004, "connection_lost"),
    (-32005, "tool_server_gone"),
    (-32006, "session_not_found"),
    (-32008, "session_draining"),
    (-32011, "tool_not_found"),
    (-32012, "tool_already_registered"),
    (-32013, "tool_unavailable"),
    (-32014, "stale_generation"),
    (-32015, "duplicate_client_name"),
    (-32016, "tool_busy"),
    (-32017, "notification_schema_violation"),
    (-32018, "frame_too_large"),
    (-32019, "schema_unknown_kind"),
    (-32020, "behavior_version_unsupported"),
    (-32021, "server_id_in_use"),
    (-32022, "invalid_description"),
    (-32023, "render_limited"),
    (-32024, "terminal_error"),
    (-32099, "rate_limited"),
];

/// Returns `None` for strings not in the table. Receivers should fall
/// back to `-32603 internal_error` for unknown strings.
pub fn numeric_for(code_str: &str) -> Option<i32> {
    ERROR_CODES
        .iter()
        .find_map(|(n, s)| (*s == code_str).then_some(*n))
}

/// Returns `None` for codes not in the table.
pub fn string_for(code: i32) -> Option<&'static str> {
    ERROR_CODES
        .iter()
        .find_map(|(n, s)| (*n == code).then_some(*s))
}

/// Numeric code most-appropriate for a [`ToolErrorWire`] variant.
/// `Custom` always maps to `-32603 internal_error` since its `code`
/// string is not in the table by definition.
pub fn from_tool_error_wire(err: &ToolErrorWire) -> i32 {
    match err {
        ToolErrorWire::ToolNotFound { .. } => -32011,
        ToolErrorWire::SessionMismatch => -32600,
        ToolErrorWire::PermissionDenied { .. } => -32003,
        ToolErrorWire::TransportClosed { .. } => -32004,
        ToolErrorWire::Timeout { .. } => -32001,
        ToolErrorWire::Cancelled { .. } => -32603,
        ToolErrorWire::InvalidArguments { .. } => -32602,
        ToolErrorWire::Execution { .. } => -32603,
        ToolErrorWire::UnsupportedProtocolVersion { .. } => -32605,
        ToolErrorWire::PayloadTooLarge { .. } => -32018,
        ToolErrorWire::BehaviorVersionUnsupported { .. } => -32020,
        ToolErrorWire::Internal { .. } => -32603,
        ToolErrorWire::RenderLimited { .. } => -32023,
        ToolErrorWire::TerminalError { .. } => -32024,
        ToolErrorWire::Custom { .. } => -32603,
    }
}

/// Stable identifier for "this session's workspace (tool) server is gone;
/// re-provision and retry", used as both the [`ToolErrorWire::Custom`] subcode
/// and the `details["code"]` value. Reusing `Custom` (not a new variant) keeps
/// the frame deserializable on older peers.
pub const WORKSPACE_UNAVAILABLE_SUBCODE: &str = "workspace_unavailable";

/// Generic, tenant-data-free message paired with the workspace-gone error.
pub const WORKSPACE_UNAVAILABLE_MESSAGE: &str = "workspace server gone; re-provision and retry";

/// JSON-RPC envelope code paired with the workspace-unavailable error. Shares
/// the canonical `tool_server_gone` numeric; recognizers key on `data.subcode`,
/// not this companion.
pub const WORKSPACE_UNAVAILABLE_JSONRPC_CODE: i32 = -32005;

/// Why the workspace (tool) server went away. `Unknown` absorbs values a newer
/// peer may add, so the typed parse never fails across independently-deployed
/// hub/SDK versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceGoneReason {
    IdleTimeout,
    Disconnect,
    Shutdown,
    /// No owner has bound a tool-server for the session yet (an attach-time
    /// miss), as opposed to a workspace that was bound and then lost.
    NotBound,
    /// Target hub liveness key absent (origin reaper or forward-time check).
    InstanceGone,
    Hibernated,
    #[serde(other)]
    Unknown,
}

/// When, relative to the failing tool call, the loss was observed. `Unknown`
/// absorbs values a newer peer may add.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceGonePhase {
    InFlightCancelled,
    RouteMissing,
    /// Observed while resolving a `session_attach_server` request.
    Attach,
    #[serde(other)]
    Unknown,
}

/// Structured payload placed in the wire `details` object. `code` mirrors the
/// `Custom` subcode (the `ToolError::custom` convention), so it survives a
/// `Wire → ToolError → Wire` round-trip and is the field recognizers read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceUnavailableDetails {
    pub code: String,
    pub reason: WorkspaceGoneReason,
    pub phase: WorkspaceGonePhase,
    pub retryable: bool,
}

/// Build the recognizable "workspace gone" error as a [`ToolErrorWire::Custom`].
///
/// `details.retryable` is the client contract. `false` (only the hibernated
/// route miss) means do not blind-retry: the call becomes servable again
/// once the workspace is revived, e.g. by the next successful bind.
pub fn workspace_unavailable_wire(
    reason: WorkspaceGoneReason,
    phase: WorkspaceGonePhase,
) -> ToolErrorWire {
    let retryable = !(matches!(reason, WorkspaceGoneReason::Hibernated)
        && matches!(phase, WorkspaceGonePhase::RouteMissing));
    let details = serde_json::to_value(WorkspaceUnavailableDetails {
        code: WORKSPACE_UNAVAILABLE_SUBCODE.to_owned(),
        reason,
        phase,
        retryable,
    });
    // This plain struct serializes infallibly; a missing `details` would make
    // the error unrecognizable, so guard the invariant in debug builds.
    debug_assert!(details.is_ok(), "workspace details must serialize");
    ToolErrorWire::Custom {
        subcode: WORKSPACE_UNAVAILABLE_SUBCODE.to_owned(),
        message: WORKSPACE_UNAVAILABLE_MESSAGE.to_owned(),
        details: details.ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const REASONS: [WorkspaceGoneReason; 6] = [
        WorkspaceGoneReason::IdleTimeout,
        WorkspaceGoneReason::Disconnect,
        WorkspaceGoneReason::Shutdown,
        WorkspaceGoneReason::NotBound,
        WorkspaceGoneReason::InstanceGone,
        WorkspaceGoneReason::Hibernated,
    ];
    const PHASES: [WorkspaceGonePhase; 3] = [
        WorkspaceGonePhase::InFlightCancelled,
        WorkspaceGonePhase::RouteMissing,
        WorkspaceGonePhase::Attach,
    ];

    // Exhaustive-match helpers pin the exact snake_case wire strings; adding a
    // variant forces an update here.
    fn reason_wire(r: WorkspaceGoneReason) -> &'static str {
        match r {
            WorkspaceGoneReason::IdleTimeout => "idle_timeout",
            WorkspaceGoneReason::Disconnect => "disconnect",
            WorkspaceGoneReason::Shutdown => "shutdown",
            WorkspaceGoneReason::NotBound => "not_bound",
            WorkspaceGoneReason::InstanceGone => "instance_gone",
            WorkspaceGoneReason::Hibernated => "hibernated",
            WorkspaceGoneReason::Unknown => "unknown",
        }
    }
    fn phase_wire(p: WorkspaceGonePhase) -> &'static str {
        match p {
            WorkspaceGonePhase::InFlightCancelled => "in_flight_cancelled",
            WorkspaceGonePhase::RouteMissing => "route_missing",
            WorkspaceGonePhase::Attach => "attach",
            WorkspaceGonePhase::Unknown => "unknown",
        }
    }

    fn expected_retryable(reason: WorkspaceGoneReason, phase: WorkspaceGonePhase) -> bool {
        !(matches!(reason, WorkspaceGoneReason::Hibernated)
            && matches!(phase, WorkspaceGonePhase::RouteMissing))
    }

    #[test]
    fn builder_emits_custom_with_code_in_details_for_every_reason_and_phase() {
        for reason in REASONS {
            for phase in PHASES {
                let v = serde_json::to_value(workspace_unavailable_wire(reason, phase)).unwrap();
                assert_eq!(v["code"], json!("custom"), "outer discriminator");
                assert_eq!(v["subcode"], json!(WORKSPACE_UNAVAILABLE_SUBCODE));
                // details.code mirrors the subcode (round-trip identity).
                assert_eq!(v["details"]["code"], json!(WORKSPACE_UNAVAILABLE_SUBCODE));
                assert_eq!(v["details"]["reason"], json!(reason_wire(reason)));
                assert_eq!(v["details"]["phase"], json!(phase_wire(phase)));
                assert_eq!(
                    v["details"]["retryable"],
                    json!(expected_retryable(reason, phase)),
                    "retryable for {reason:?}/{phase:?}",
                );
            }
        }
    }

    #[test]
    fn hibernated_route_missing_is_the_only_non_retryable_shape() {
        let v = serde_json::to_value(workspace_unavailable_wire(
            WorkspaceGoneReason::Hibernated,
            WorkspaceGonePhase::RouteMissing,
        ))
        .unwrap();
        assert_eq!(v["details"]["reason"], json!("hibernated"));
        assert_eq!(v["details"]["phase"], json!("route_missing"));
        assert_eq!(v["details"]["retryable"], json!(false));

        for reason in REASONS {
            for phase in PHASES {
                if matches!(reason, WorkspaceGoneReason::Hibernated)
                    && matches!(phase, WorkspaceGonePhase::RouteMissing)
                {
                    continue;
                }
                let v = serde_json::to_value(workspace_unavailable_wire(reason, phase)).unwrap();
                assert_eq!(
                    v["details"]["retryable"],
                    json!(true),
                    "{reason:?}/{phase:?} must stay retryable",
                );
            }
        }
    }

    #[test]
    fn builder_uses_the_pinned_generic_message() {
        let ToolErrorWire::Custom { message, .. } = workspace_unavailable_wire(
            WorkspaceGoneReason::IdleTimeout,
            WorkspaceGonePhase::RouteMissing,
        ) else {
            panic!("expected Custom variant");
        };
        // Exact, tenant-data-free contract.
        assert_eq!(message, WORKSPACE_UNAVAILABLE_MESSAGE);
    }

    #[test]
    fn unknown_reason_and_phase_deserialize_to_unknown() {
        // Independently-deployed peers may emit reason/phase values this build
        // does not know; the typed parse must absorb them, not fail.
        let details: WorkspaceUnavailableDetails = serde_json::from_value(json!({
            "code": WORKSPACE_UNAVAILABLE_SUBCODE,
            "reason": "reason_from_a_newer_hub",
            "phase": "phase_from_a_newer_hub",
            "retryable": true,
        }))
        .expect("typed parse tolerates unknown enum values");
        assert_eq!(details.reason, WorkspaceGoneReason::Unknown);
        assert_eq!(details.phase, WorkspaceGonePhase::Unknown);
    }

    #[test]
    fn unknown_reason_serializes_and_round_trips() {
        // The route-missing classifier emits `Unknown` ("cause not observed"),
        // so — despite `Unknown` being the `#[serde(other)]` deserialize
        // catch-all — it must serialize to a stable `"unknown"` label and parse
        // back, both in the wire payload and as the bare enum.
        assert_eq!(
            serde_json::to_value(WorkspaceGoneReason::Unknown).unwrap(),
            json!("unknown"),
        );
        let wire = workspace_unavailable_wire(
            WorkspaceGoneReason::Unknown,
            WorkspaceGonePhase::RouteMissing,
        );
        let v = serde_json::to_value(&wire).unwrap();
        assert_eq!(v["details"]["reason"], json!("unknown"));
        let parsed: WorkspaceUnavailableDetails =
            serde_json::from_value(v["details"].clone()).expect("details round-trip");
        assert_eq!(parsed.reason, WorkspaceGoneReason::Unknown);
    }

    #[test]
    fn custom_variant_tolerates_unknown_future_details_shape() {
        // An unknown subcode + richer future details must still deserialize rather than failing the frame.
        let future = json!({
            "code": "custom",
            "subcode": "some_future_subcode",
            "message": "from a newer peer",
            "details": {
                "code": "some_future_subcode",
                "extra_new_field": {"nested": [1, 2, 3]},
            },
        });
        let wire: ToolErrorWire =
            serde_json::from_value(future).expect("custom variant deserializes");
        let ToolErrorWire::Custom {
            subcode, details, ..
        } = &wire
        else {
            panic!("expected Custom variant");
        };
        assert_eq!(subcode, "some_future_subcode");
        assert!(
            details
                .as_ref()
                .and_then(|d| d.get("extra_new_field"))
                .is_some(),
            "unknown details fields are preserved",
        );
        // Re-serialization preserves the unknown fields.
        let reser = serde_json::to_value(&wire).unwrap();
        assert_eq!(
            reser["details"]["extra_new_field"]["nested"],
            json!([1, 2, 3])
        );
    }
}
