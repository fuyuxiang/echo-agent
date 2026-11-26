//! `WorkspaceError` <-> wire-code mapping for the workspace RPC envelope
//! (the envelope types are canonical in `echo_agent_workspace_types::rpc`).
//!
//! `error_code` uses a non-wildcard match so the compiler enforces
//! coverage of new `WorkspaceError` variants.
use crate::error::WorkspaceError;
pub use echo_agent_workspace_types::rpc::{RpcEnvelope, RpcError};
/// Build an error envelope from a `WorkspaceError`.
pub fn envelope_err<T>(error: &WorkspaceError) -> RpcEnvelope<T> {
    RpcEnvelope::err_parts(error_code(error), error.to_string())
}
/// Map a `WorkspaceError` to its wire code string.
///
/// Uses an exhaustive match with no wildcard -- the compiler will
/// error when new variants are added to `WorkspaceError`, forcing
/// the implementer to assign a wire code.
pub fn error_code(err: &WorkspaceError) -> &'static str {
    match err {
        WorkspaceError::ParentSessionNotFound(_) => "parent_session_not_found",
        WorkspaceError::SessionNotFound(_) => "session_not_found",
        WorkspaceError::SessionAlreadyExists(_) => "session_already_exists",
        WorkspaceError::EmptyAgentId => "empty_agent_id",
        WorkspaceError::CannotDropMainSession => "cannot_drop_main",
        WorkspaceError::Finalize(_) => "finalize",
        WorkspaceError::CapabilityWidening { .. } => "capability_widening",
        WorkspaceError::Unauthorized { .. } => "unauthorized",
        WorkspaceError::TurnActive(_) => echo_agent_workspace_types::rpc::envelope::TURN_ACTIVE,
        WorkspaceError::MaxDepthExceeded { .. } => "max_depth_exceeded",
        WorkspaceError::JoinError(_) => "join_error",
        WorkspaceError::InvalidHunkAction(_) => "invalid_hunk_action",
        WorkspaceError::HunkActionFailed(_) => "hunk_action_failed",
        WorkspaceError::HubError(_) => "hub_error",
        WorkspaceError::UnknownMethod(_) => "unknown_method",
        WorkspaceError::ExportArchiveLimitExceeded(_) => "export_archive_limit_exceeded",
        WorkspaceError::ExportGithub { kind, .. } => kind.wire_code(),
        WorkspaceError::ShuttingDown => "shutting_down",
        WorkspaceError::ToolsetExternallyOwned(_) => "toolset_externally_owned",
    }
}
/// Map a wire [`RpcError`] back to a [`WorkspaceError`].
///
/// Known codes are mapped to their specific variants. Unknown codes
/// degrade gracefully to `WorkspaceError::HubError`, ensuring
/// forward compatibility when a newer workspace sends codes an older
/// shell does not recognise.
///
/// # Intentional degradation
///
/// The structured variants `CapabilityWidening`, `Unauthorized`, and
/// `MaxDepthExceeded` carry multiple fields that are not preserved in
/// the wire `message` string. These are mapped to `HubError` on the
/// deserializing side because reconstructing the original struct fields
/// from a flattened `Display` string would be fragile and error-prone.
/// Callers that need to distinguish these errors can match on the
/// `HubError` message which contains the original error code as a
/// prefix (e.g. `"capability_widening: ..."`).
pub fn rpc_error_to_workspace(err: RpcError) -> WorkspaceError {
    if let Some(kind) =
        echo_agent_workspace_types::rpc::export_github::ExportGithubError::from_wire_code(&err.code)
    {
        return WorkspaceError::ExportGithub {
            kind,
            message: err.message,
        };
    }
    match err.code.as_str() {
        "parent_session_not_found" => WorkspaceError::ParentSessionNotFound(err.message),
        "session_not_found" => WorkspaceError::SessionNotFound(err.message),
        "session_already_exists" => WorkspaceError::SessionAlreadyExists(err.message),
        "empty_agent_id" => WorkspaceError::EmptyAgentId,
        "cannot_drop_main" => WorkspaceError::CannotDropMainSession,
        "finalize" => WorkspaceError::Finalize(err.message),
        "capability_widening" => {
            WorkspaceError::HubError(format!("capability_widening: {}", err.message))
        }
        "unauthorized" => WorkspaceError::HubError(format!("unauthorized: {}", err.message)),
        "turn_active" => WorkspaceError::TurnActive(err.message),
        "max_depth_exceeded" => {
            WorkspaceError::HubError(format!("max_depth_exceeded: {}", err.message))
        }
        "join_error" => WorkspaceError::JoinError(err.message),
        "invalid_hunk_action" => WorkspaceError::InvalidHunkAction(err.message),
        "hunk_action_failed" => WorkspaceError::HunkActionFailed(err.message),
        "hub_error" => WorkspaceError::HubError(err.message),
        "unknown_method" => WorkspaceError::UnknownMethod(err.message),
        "export_archive_limit_exceeded" => WorkspaceError::ExportArchiveLimitExceeded(err.message),
        "shutting_down" => WorkspaceError::ShuttingDown,
        "toolset_externally_owned" => WorkspaceError::ToolsetExternallyOwned(err.message),
        unknown => {
            WorkspaceError::HubError(format!("unknown error code: {unknown}: {}", err.message))
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::CapabilityMode;
    /// Verify round-trip fidelity for every `WorkspaceError` variant.
    #[test]
    fn error_code_round_trip_all_variants() {
        let mut variants: Vec<WorkspaceError> = vec![
            WorkspaceError::ParentSessionNotFound("p".into()),
            WorkspaceError::SessionNotFound("s".into()),
            WorkspaceError::SessionAlreadyExists("s".into()),
            WorkspaceError::EmptyAgentId,
            WorkspaceError::CannotDropMainSession,
            WorkspaceError::Finalize("f".into()),
            WorkspaceError::CapabilityWidening {
                parent: CapabilityMode::ReadOnly,
                child: CapabilityMode::All,
            },
            WorkspaceError::Unauthorized {
                caller: "a".into(),
                target: "b".into(),
            },
            WorkspaceError::TurnActive("s".into()),
            WorkspaceError::MaxDepthExceeded { parent: "p".into() },
            WorkspaceError::JoinError("j".into()),
            WorkspaceError::InvalidHunkAction("h".into()),
            WorkspaceError::HunkActionFailed("h".into()),
            WorkspaceError::HubError("hub".into()),
            WorkspaceError::UnknownMethod("workspace.bogus".into()),
            WorkspaceError::ExportArchiveLimitExceeded("too big".into()),
            WorkspaceError::ShuttingDown,
            WorkspaceError::ToolsetExternallyOwned("s".into()),
        ];
        for err in &variants {
            let code = error_code(err);
            assert!(!code.is_empty(), "code must not be empty for {err:?}");
            let rpc_err = RpcError {
                code: code.to_owned(),
                message: err.to_string(),
            };
            let recovered = rpc_error_to_workspace(rpc_err);
            let recovered_code = error_code(&recovered);
            match err {
                WorkspaceError::CapabilityWidening { .. } => {
                    assert_eq!(recovered_code, "hub_error");
                    let msg = recovered.to_string();
                    assert!(
                        msg.contains("capability_widening"),
                        "degraded error should contain original code: {msg}"
                    );
                }
                WorkspaceError::Unauthorized { .. } => {
                    assert_eq!(recovered_code, "hub_error");
                    let msg = recovered.to_string();
                    assert!(
                        msg.contains("unauthorized"),
                        "degraded error should contain original code: {msg}"
                    );
                }
                WorkspaceError::MaxDepthExceeded { .. } => {
                    assert_eq!(recovered_code, "hub_error");
                    let msg = recovered.to_string();
                    assert!(
                        msg.contains("max_depth_exceeded"),
                        "degraded error should contain original code: {msg}"
                    );
                }
                _ => {
                    assert_eq!(
                        recovered_code, code,
                        "round-trip mismatch for {err:?}: got code {recovered_code}"
                    );
                }
            }
        }
    }
    /// Verify unknown codes degrade to HubError.
    #[test]
    fn export_github_codes_round_trip_typed() {
        for kind in echo_agent_workspace_types::rpc::export_github::ExportGithubError::ALL {
            let err = WorkspaceError::ExportGithub {
                kind,
                message: "boom".into(),
            };
            let rpc_err = RpcError {
                code: error_code(&err).into(),
                message: "boom".into(),
            };
            let recovered = rpc_error_to_workspace(rpc_err);
            assert!(
                matches!(recovered, WorkspaceError::ExportGithub { kind: k, .. } if k == kind),
                "lost typed export error for {kind:?}: {recovered:?}"
            );
        }
    }
    #[test]
    fn unknown_code_degrades_to_hub_error() {
        let rpc_err = RpcError {
            code: "future_new_variant".into(),
            message: "something new".into(),
        };
        let recovered = rpc_error_to_workspace(rpc_err);
        assert!(matches!(recovered, WorkspaceError::HubError(_)));
        let msg = recovered.to_string();
        assert!(msg.contains("future_new_variant"));
    }
    /// Verify serde round-trip of RpcEnvelope.
    #[test]
    fn envelope_serde_round_trip_ok() {
        let env: RpcEnvelope<String> = RpcEnvelope::ok("hello".into());
        let json = serde_json::to_value(&env).unwrap();
        let recovered: RpcEnvelope<String> = serde_json::from_value(json).unwrap();
        match recovered.into_result() {
            Ok(v) => assert_eq!(v, "hello"),
            Err(e) => panic!("expected Ok, got {e:?}"),
        }
    }
    /// Verify serde round-trip of RpcEnvelope error, through the
    /// `WorkspaceError` mapping in both directions.
    #[test]
    fn envelope_serde_round_trip_err() {
        let err = WorkspaceError::SessionNotFound("ghost".into());
        let env: RpcEnvelope<String> = envelope_err(&err);
        let json = serde_json::to_value(&env).unwrap();
        let recovered: RpcEnvelope<String> = serde_json::from_value(json).unwrap();
        match recovered.into_result().map_err(rpc_error_to_workspace) {
            Ok(_) => panic!("expected Err"),
            Err(e) => {
                assert_eq!(error_code(&e), "session_not_found");
            }
        }
    }
}
