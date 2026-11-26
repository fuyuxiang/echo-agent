//! `From<ToolError> for ToolErrorWire` coverage for the struct-based ToolError.

use serde_json::json;

use echo_agent_tool_protocol::{ToolErrorWire, ToolId};
use echo_agent_tool_runtime::error::{ToolError, ToolErrorKind};

fn tid(name: &str) -> ToolId {
    ToolId::new(name).unwrap()
}

#[test]
fn not_implemented_maps_to_custom_with_snake_case_code() {
    let err = ToolError::not_implemented("nope");
    let wire: ToolErrorWire = err.into();
    match wire {
        ToolErrorWire::Custom {
            subcode, message, ..
        } => {
            assert_eq!(subcode, "not_implemented");
            assert_eq!(message, "nope");
        }
        other => panic!("expected Custom, got {other:?}"),
    }
}

#[test]
fn invalid_arguments_round_trips_message_and_details() {
    let details = json!({"field": "name", "expected": "non-empty"});
    let err = ToolError::invalid_arguments("bad name").with_details(details.clone());
    let wire: ToolErrorWire = err.into();
    match wire {
        ToolErrorWire::InvalidArguments {
            message,
            details: d,
        } => {
            assert_eq!(message, "bad name");
            assert_eq!(d, Some(details));
        }
        other => panic!("expected InvalidArguments, got {other:?}"),
    }
}

#[test]
fn not_found_maps_to_tool_not_found() {
    let err = ToolError::not_found(tid("missing"), "tool 'missing' not registered");
    let wire: ToolErrorWire = err.into();
    assert!(matches!(wire, ToolErrorWire::ToolNotFound { tool_id } if tool_id == tid("missing")));
}

#[test]
fn permission_denied_round_trips_reason() {
    let err = ToolError::permission_denied("not authorised for write");
    let wire: ToolErrorWire = err.into();
    assert!(
        matches!(wire, ToolErrorWire::PermissionDenied { reason } if reason == "not authorised for write")
    );
}

#[test]
fn unauthorized_maps_to_custom_with_unauthorized_subcode() {
    let err = ToolError::unauthorized("session expired");
    let wire: ToolErrorWire = err.into();
    match wire {
        ToolErrorWire::Custom {
            subcode, message, ..
        } => {
            assert_eq!(subcode, "unauthorized");
            assert_eq!(message, "session expired");
        }
        other => panic!("expected Custom(unauthorized), got {other:?}"),
    }
}

#[test]
fn timeout_with_details() {
    let err = ToolError::timeout(tid("slow"), "image generation timed out")
        .with_details(json!({"tool_id": "slow", "elapsed_ms": 2500}));
    let wire: ToolErrorWire = err.into();
    match wire {
        ToolErrorWire::Timeout {
            tool_id,
            elapsed_ms,
        } => {
            assert_eq!(tool_id, tid("slow"));
            assert_eq!(elapsed_ms, 2_500);
        }
        other => panic!("expected Timeout, got {other:?}"),
    }
}

#[test]
fn cancelled_round_trips_tool_id() {
    let err = ToolError::cancelled(tid("paused"), "user cancelled");
    let wire: ToolErrorWire = err.into();
    assert!(matches!(wire, ToolErrorWire::Cancelled { tool_id } if tool_id == tid("paused")));
}

#[test]
fn rate_limited_carries_detail_message() {
    let err = ToolError::rate_limited("You've reached your image generation limit.");
    let wire: ToolErrorWire = err.into();
    match wire {
        ToolErrorWire::Custom {
            subcode, message, ..
        } => {
            assert_eq!(subcode, "rate_limited");
            assert_eq!(message, "You've reached your image generation limit.");
        }
        other => panic!("expected Custom, got {other:?}"),
    }
}

#[test]
fn service_unavailable_carries_detail() {
    let err = ToolError::service_unavailable("Media service temporarily down.");
    let wire: ToolErrorWire = err.into();
    match wire {
        ToolErrorWire::Custom {
            subcode, message, ..
        } => {
            assert_eq!(subcode, "service_unavailable");
            assert_eq!(message, "Media service temporarily down.");
        }
        other => panic!("expected Custom, got {other:?}"),
    }
}

#[test]
fn network_error_maps_to_custom_with_message() {
    let err = ToolError::network_error("dns failure");
    let wire: ToolErrorWire = err.into();
    match wire {
        ToolErrorWire::Custom {
            subcode, message, ..
        } => {
            assert_eq!(subcode, "network_error");
            assert_eq!(message, "dns failure");
        }
        other => panic!("expected Custom, got {other:?}"),
    }
}

#[test]
fn execution_uses_detail_as_message() {
    let err = ToolError::execution(
        tid("worker"),
        "image generation failed: model returned empty response",
    )
    .with_source(anyhow::anyhow!("root cause"));
    let wire: ToolErrorWire = err.into();
    match wire {
        ToolErrorWire::Execution { tool_id, message } => {
            assert_eq!(tool_id, tid("worker"));
            assert_eq!(
                message,
                "image generation failed: model returned empty response"
            );
        }
        other => panic!("expected Execution, got {other:?}"),
    }
}

#[test]
fn custom_round_trips_code_message_details() {
    let err = ToolError::custom("billing_overflow", "quota exhausted")
        .with_details(json!({"code": "billing_overflow", "limit": 1000}));
    let wire: ToolErrorWire = err.into();
    match wire {
        ToolErrorWire::Custom {
            subcode,
            message,
            details,
        } => {
            assert_eq!(subcode, "billing_overflow");
            assert_eq!(message, "quota exhausted");
            assert!(details.is_some());
        }
        other => panic!("expected Custom, got {other:?}"),
    }
}

#[test]
fn variant_name_covers_all_kinds() {
    let kinds = [
        ToolErrorKind::NotImplemented,
        ToolErrorKind::InvalidArguments,
        ToolErrorKind::NotFound,
        ToolErrorKind::PermissionDenied,
        ToolErrorKind::Unauthorized,
        ToolErrorKind::Timeout,
        ToolErrorKind::Cancelled,
        ToolErrorKind::RateLimited,
        ToolErrorKind::ServiceUnavailable,
        ToolErrorKind::NetworkError,
        ToolErrorKind::Execution,
        ToolErrorKind::BehaviorVersionUnsupported,
        ToolErrorKind::RenderLimited,
        ToolErrorKind::TerminalError,
        ToolErrorKind::Custom,
    ];
    let names: std::collections::HashSet<_> = kinds.iter().map(|k| k.as_str()).collect();
    assert_eq!(names.len(), 15);
}

#[test]
fn display_shows_detail_not_kind() {
    let err = ToolError::rate_limited("You've exceeded your quota.");
    assert_eq!(err.to_string(), "You've exceeded your quota.");
}

#[test]
fn serde_json_error_converts_to_invalid_arguments() {
    let err: ToolError = serde_json::from_str::<u32>("\"not a number\"")
        .unwrap_err()
        .into();
    assert_eq!(err.kind, ToolErrorKind::InvalidArguments);
}

#[test]
fn with_source_preserves_detail() {
    let err = ToolError::execution(tid("test"), "something broke")
        .with_source(anyhow::anyhow!("inner cause"));
    assert_eq!(err.detail, "something broke");
    assert!(std::error::Error::source(&err).is_some());
}
