//! `is_workspace_unavailable` recognizer coverage, pinned against the real
//! wire decode path (`error_from_envelope` / `tool_error_from_wire`).

use echo_agent_computer_hub_core::{
    error_from_envelope, is_workspace_unavailable, tool_error_from_wire,
};
use echo_agent_tool_protocol::{
    JsonRpcError, ToolErrorWire, WORKSPACE_UNAVAILABLE_SUBCODE, WorkspaceGonePhase,
    WorkspaceGoneReason, WorkspaceUnavailableDetails, workspace_unavailable_wire,
};
use echo_agent_tool_runtime::{ToolError, ToolErrorKind};
use serde_json::json;

const REASONS: [WorkspaceGoneReason; 6] = [
    WorkspaceGoneReason::IdleTimeout,
    WorkspaceGoneReason::Disconnect,
    WorkspaceGoneReason::Shutdown,
    WorkspaceGoneReason::NotBound,
    WorkspaceGoneReason::InstanceGone,
    WorkspaceGoneReason::Hibernated,
];

fn expected_retryable(reason: WorkspaceGoneReason, phase: WorkspaceGonePhase) -> bool {
    !(matches!(reason, WorkspaceGoneReason::Hibernated)
        && matches!(phase, WorkspaceGonePhase::RouteMissing))
}
const PHASES: [WorkspaceGonePhase; 2] = [
    WorkspaceGonePhase::InFlightCancelled,
    WorkspaceGonePhase::RouteMissing,
];

fn envelope_for(wire: &ToolErrorWire) -> JsonRpcError {
    JsonRpcError {
        // -32005 is the best-effort numeric companion (`tool_server_gone`);
        // recognition keys on `data.details.code`, not the numeric.
        code: -32005,
        message: "workspace server gone".to_owned(),
        data: Some(serde_json::to_value(wire).unwrap()),
    }
}

#[test]
fn round_trip_through_envelope_is_recognized_for_every_reason_and_phase() {
    for reason in REASONS {
        for phase in PHASES {
            let wire = workspace_unavailable_wire(reason, phase);
            let err = error_from_envelope(envelope_for(&wire));

            assert!(
                is_workspace_unavailable(&err),
                "should recognize {reason:?}/{phase:?}",
            );
            assert_eq!(err.kind, ToolErrorKind::Custom);

            // The full structured payload survives into `ToolError::details`,
            // so a caller can branch on code/reason/phase/retryable.
            let details: WorkspaceUnavailableDetails =
                serde_json::from_value(err.details.expect("details survive")).unwrap();
            assert_eq!(
                details,
                WorkspaceUnavailableDetails {
                    code: WORKSPACE_UNAVAILABLE_SUBCODE.to_owned(),
                    reason,
                    phase,
                    retryable: expected_retryable(reason, phase),
                },
            );
        }
    }
}

#[test]
fn hibernated_route_missing_round_trips_as_non_retryable() {
    let wire = workspace_unavailable_wire(
        WorkspaceGoneReason::Hibernated,
        WorkspaceGonePhase::RouteMissing,
    );
    let err = error_from_envelope(envelope_for(&wire));
    assert!(is_workspace_unavailable(&err));
    let details: WorkspaceUnavailableDetails =
        serde_json::from_value(err.details.expect("details survive")).unwrap();
    assert_eq!(details.reason, WorkspaceGoneReason::Hibernated);
    assert_eq!(details.phase, WorkspaceGonePhase::RouteMissing);
    assert!(!details.retryable, "hibernated route miss is non-retryable");

    let wire = workspace_unavailable_wire(
        WorkspaceGoneReason::Hibernated,
        WorkspaceGonePhase::InFlightCancelled,
    );
    let err = error_from_envelope(envelope_for(&wire));
    let details: WorkspaceUnavailableDetails =
        serde_json::from_value(err.details.expect("details survive")).unwrap();
    assert!(details.retryable);
}

#[test]
fn tool_error_from_wire_directly_is_recognized() {
    let wire = workspace_unavailable_wire(
        WorkspaceGoneReason::Disconnect,
        WorkspaceGonePhase::RouteMissing,
    );
    let err = tool_error_from_wire(wire);
    assert!(is_workspace_unavailable(&err));
    let details = err.details.expect("details survive");
    assert_eq!(details["code"], json!(WORKSPACE_UNAVAILABLE_SUBCODE));
    assert_eq!(details["reason"], json!("disconnect"));
    assert_eq!(details["phase"], json!("route_missing"));
    assert_eq!(details["retryable"], json!(true));
}

#[test]
fn wire_to_tool_error_to_wire_preserves_outer_subcode() {
    // Keying the identity on details.code lets From<ToolError> for ToolErrorWire
    // rebuild the outer subcode on re-serialization.
    let original = workspace_unavailable_wire(
        WorkspaceGoneReason::IdleTimeout,
        WorkspaceGonePhase::InFlightCancelled,
    );
    let tool_error = tool_error_from_wire(original);
    let back: ToolErrorWire = tool_error.into();
    let ToolErrorWire::Custom { subcode, .. } = back else {
        panic!("expected Custom variant");
    };
    assert_eq!(subcode, WORKSPACE_UNAVAILABLE_SUBCODE);
}

#[test]
fn recognized_with_unknown_reason_and_phase() {
    // Recognition is decoupled from the typed reason/phase enums: a newer hub
    // emitting unknown values is still recognized (it keys only on `code`).
    let wire = ToolErrorWire::Custom {
        subcode: WORKSPACE_UNAVAILABLE_SUBCODE.to_owned(),
        message: "from a newer hub".to_owned(),
        details: Some(json!({
            "code": WORKSPACE_UNAVAILABLE_SUBCODE,
            "reason": "brand_new_reason",
            "phase": "brand_new_phase",
            "retryable": true,
        })),
    };
    let err = error_from_envelope(envelope_for(&wire));
    assert!(is_workspace_unavailable(&err));
    // End-to-end decode → typed-parse → `Unknown`, the path consumers read by.
    let details: WorkspaceUnavailableDetails =
        serde_json::from_value(err.details.expect("details survive")).unwrap();
    assert_eq!(details.reason, WorkspaceGoneReason::Unknown);
    assert_eq!(details.phase, WorkspaceGonePhase::Unknown);
}

#[test]
fn decoded_custom_with_none_details_is_recognized_via_canonical_code() {
    // Wire `details: None` decodes through `ToolError::custom`, which repopulates
    // `details = {"code": subcode}`, so it IS recognized — contrast the hand-built
    // no-details case in `custom_error_without_any_details_is_not_recognized`.
    let wire = ToolErrorWire::Custom {
        subcode: WORKSPACE_UNAVAILABLE_SUBCODE.to_owned(),
        message: "no structured details".to_owned(),
        details: None,
    };
    let err = error_from_envelope(envelope_for(&wire));
    assert_eq!(err.kind, ToolErrorKind::Custom);
    assert!(is_workspace_unavailable(&err));
}

#[test]
fn decoded_custom_without_code_key_is_not_recognized() {
    // The central correctness property: recognition keys on the surviving
    // `details.code`, NOT the outer `Custom.subcode`. Here the outer subcode
    // matches, but `with_details` overwrote the auto-populated `code`, so the
    // decoded error must NOT be recognized.
    let wire = ToolErrorWire::Custom {
        subcode: WORKSPACE_UNAVAILABLE_SUBCODE.to_owned(),
        message: "details lack code".to_owned(),
        details: Some(json!({ "reason": "disconnect" })),
    };
    let err = error_from_envelope(envelope_for(&wire));
    assert_eq!(err.kind, ToolErrorKind::Custom);
    assert!(!is_workspace_unavailable(&err));
}

#[test]
fn different_custom_code_is_not_recognized() {
    let wire = ToolErrorWire::Custom {
        subcode: "some_other_error".to_owned(),
        message: "nope".to_owned(),
        details: Some(json!({ "code": "some_other_error" })),
    };
    let err = error_from_envelope(envelope_for(&wire));
    assert_eq!(err.kind, ToolErrorKind::Custom);
    assert!(!is_workspace_unavailable(&err));
}

#[test]
fn numeric_only_tool_server_gone_without_data_is_not_recognized() {
    // Recognition is by the data payload, never the numeric code: a bare -32005
    // with no `data` decodes to a `jsonrpc_-32005` custom error, not recognized.
    let err = error_from_envelope(JsonRpcError {
        code: -32005,
        message: "tool server gone".to_owned(),
        data: None,
    });
    assert!(!is_workspace_unavailable(&err));
}

#[test]
fn custom_error_without_any_details_is_not_recognized() {
    // Hand-built Custom with no `details` (no `code`) — unlike a wire `details:
    // None`, nothing repopulates `code` here, so it is not recognized.
    let err = ToolError::new(ToolErrorKind::Custom, "no details at all");
    assert!(!is_workspace_unavailable(&err));
}

#[test]
fn non_custom_error_with_matching_code_is_not_recognized() {
    // The kind guard matters: a non-Custom error carrying a matching
    // `details.code` must still be rejected.
    let err = ToolError::new(ToolErrorKind::NetworkError, "socket closed")
        .with_details(json!({ "code": WORKSPACE_UNAVAILABLE_SUBCODE }));
    assert_ne!(err.kind, ToolErrorKind::Custom);
    assert!(!is_workspace_unavailable(&err));
}

#[test]
fn non_custom_decoded_error_is_not_recognized() {
    let wire = ToolErrorWire::ToolNotFound {
        tool_id: echo_agent_tool_protocol::ToolId::new("ns:tool").unwrap(),
    };
    let err = error_from_envelope(envelope_for(&wire));
    assert_ne!(err.kind, ToolErrorKind::Custom);
    assert!(!is_workspace_unavailable(&err));

    assert!(!is_workspace_unavailable(&ToolError::network_error(
        "socket closed"
    )));
}
