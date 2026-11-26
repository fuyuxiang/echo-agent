//! Resume identity validation: ensures that a resumed subagent matches the
//! source's identity fields (type, persona).
//!
//! Model is not an identity gate on resume: the shell always inherits/pins the
//! source model, and any caller-provided model override is soft-ignored.
//!
//! Extracted from `echo-agent-runtime/src/agent/subagent/` resume validation block.

use crate::types::ResumeSourceData;

/// Error type for resume validation failures.
///
/// Each variant describes a specific identity mismatch between the resume
/// request and the source subagent's recorded identity fields.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResumeValidationError {
    /// The requested subagent_type differs from the source's type.
    #[error(
        "Cannot resume with subagent_type '{requested}': source subagent was '{source_value}'. \
         Resumed sessions must use the same subagent type as the source."
    )]
    TypeMismatch {
        requested: String,
        source_value: String,
    },

    /// The requested persona differs from the source's persona.
    #[error(
        "Cannot resume with persona '{requested}': source subagent used {source_value:?}. \
         Resumed sessions must use the same persona as the source."
    )]
    PersonaMismatch {
        requested: String,
        source_value: Option<String>,
    },
}

/// Validate that a resume request's identity fields match the source subagent.
///
/// Resume contract: the resumed child inherits the source's raw transcript,
/// tool state, and model. System prompt and prompt context are freshly
/// rendered from the current agent definition. Reject type/persona overrides
/// that conflict with the inherited identity fields. Model overrides are not
/// validated here — callers soft-ignore them and pin the source model.
///
/// Returns `Ok(())` if identity fields match, or `Err(ResumeValidationError)`
/// describing the first mismatch found.
pub fn validate_resume_identity(
    requested_type: &str,
    requested_persona: Option<&str>,
    source: &ResumeSourceData,
) -> Result<(), ResumeValidationError> {
    // Check subagent type match
    if requested_type != source.subagent_type {
        return Err(ResumeValidationError::TypeMismatch {
            requested: requested_type.to_string(),
            source_value: source.subagent_type.clone(),
        });
    }

    // Check persona match (only if explicitly requested)
    if let Some(persona) = requested_persona
        && source.persona.as_deref() != Some(persona)
    {
        return Err(ResumeValidationError::PersonaMismatch {
            requested: persona.to_string(),
            source_value: source.persona.clone(),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_source(
        subagent_type: &str,
        persona: Option<&str>,
        model_id: Option<&str>,
    ) -> ResumeSourceData {
        ResumeSourceData {
            subagent_id: "source-id".into(),
            subagent_type: subagent_type.into(),
            persona: persona.map(String::from),
            model_id: model_id.map(String::from),
            child_cwd: "/workspace".into(),
            worktree_path: None,
            snapshot_ref: None,
            child_session_id: "child-session".into(),
        }
    }

    // ── Matching cases ───────────────────────────────────────────

    #[test]
    fn matching_type_no_persona() {
        let source = make_source("general-purpose", None, None);
        let result = validate_resume_identity("general-purpose", None, &source);
        assert!(result.is_ok());
    }

    #[test]
    fn matching_type_and_persona() {
        let source = make_source("general-purpose", Some("implementer"), None);
        let result = validate_resume_identity("general-purpose", Some("implementer"), &source);
        assert!(result.is_ok());
    }

    #[test]
    fn matching_type_and_persona_source_has_model() {
        let source = make_source("general-purpose", Some("impl"), Some("echo-agent-3"));
        let result = validate_resume_identity("general-purpose", Some("impl"), &source);
        assert!(result.is_ok());
    }

    #[test]
    fn no_persona_requested_source_has_persona() {
        // Not requesting a persona is always valid (no override = inherit)
        let source = make_source("general-purpose", Some("implementer"), None);
        let result = validate_resume_identity("general-purpose", None, &source);
        assert!(result.is_ok());
    }

    #[test]
    fn source_model_is_not_validated() {
        // Model is not an identity gate; source model is used only for pinning.
        let source = make_source("general-purpose", None, Some("echo-agent-3"));
        let result = validate_resume_identity("general-purpose", None, &source);
        assert!(result.is_ok());
    }

    // ── Mismatching cases ────────────────────────────────────────

    #[test]
    fn type_mismatch_rejected() {
        let source = make_source("general-purpose", None, None);
        let result = validate_resume_identity("explore", None, &source);
        assert!(matches!(
            result,
            Err(ResumeValidationError::TypeMismatch { .. })
        ));
        let err = result.unwrap_err();
        assert!(err.to_string().contains("explore"));
        assert!(err.to_string().contains("general-purpose"));
    }

    #[test]
    fn persona_mismatch_rejected() {
        let source = make_source("general-purpose", Some("implementer"), None);
        let result = validate_resume_identity("general-purpose", Some("reviewer"), &source);
        assert!(matches!(
            result,
            Err(ResumeValidationError::PersonaMismatch { .. })
        ));
        let err = result.unwrap_err();
        assert!(err.to_string().contains("reviewer"));
    }

    #[test]
    fn persona_requested_but_source_had_none() {
        let source = make_source("general-purpose", None, None);
        let result = validate_resume_identity("general-purpose", Some("implementer"), &source);
        assert!(matches!(
            result,
            Err(ResumeValidationError::PersonaMismatch { .. })
        ));
    }

    // ── Validation order ─────────────────────────────────────────

    #[test]
    fn type_mismatch_checked_before_persona() {
        let source = make_source("general-purpose", Some("impl"), None);
        let result = validate_resume_identity("explore", Some("reviewer"), &source);
        // Should be TypeMismatch, not PersonaMismatch
        assert!(matches!(
            result,
            Err(ResumeValidationError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn persona_mismatch_still_rejected_when_source_has_model() {
        let source = make_source("general-purpose", Some("impl"), Some("echo-agent-3"));
        let result = validate_resume_identity("general-purpose", Some("reviewer"), &source);
        assert!(matches!(
            result,
            Err(ResumeValidationError::PersonaMismatch { .. })
        ));
    }
}
