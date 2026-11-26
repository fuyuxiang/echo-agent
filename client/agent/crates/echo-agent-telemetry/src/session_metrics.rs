//! Session lifecycle event structs.
//!
//! Fires in both `Enabled` and `SessionMetrics` telemetry modes via
//! `log_session_event`.

use serde::Serialize;

/// The ACP method the client called, kept separate from the warm/cold
/// mechanism (`SessionStarted::restored_from_disk`) so intent and mechanism can
/// be queried independently.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStartKind {
    New,
    Load,
    Resume,
}

#[derive(Serialize)]
pub struct SessionStarted {
    pub session_id: String,
    pub kind: SessionStartKind,
    pub setup_duration_ms: u64,
    /// Whether setup rebuilt the session from disk (cold) rather than
    /// reconnecting to a resident actor (warm). Mirrors `SessionLoad`.
    pub restored_from_disk: bool,
}

impl SessionStarted {
    pub fn new(
        session_id: String,
        kind: SessionStartKind,
        setup_duration: std::time::Duration,
        restored_from_disk: bool,
    ) -> Self {
        Self {
            session_id,
            kind,
            setup_duration_ms: setup_duration.as_millis() as u64,
            restored_from_disk,
        }
    }
}

#[derive(Serialize)]
pub struct Turn {
    pub session_id: String,
    pub turn_number: u64,
}

#[derive(Serialize)]
pub struct TurnCompletedLifecycle {
    pub session_id: String,
    pub turn_number: u64,
}

/// Doom-loop recovery acted this turn: poisoned attempts were resampled
/// and/or a response was accepted with confident signals after the budget
/// was spent. Trigger labels only — never generation content.
#[derive(Serialize)]
pub struct DoomLoopRecovery {
    pub session_id: String,
    pub turn_number: u64,
    /// Resamples this turn (doomed attempts discarded).
    pub attempts: u32,
    /// Whether the final response kept confident signals (budget spent).
    pub accepted_after_budget: bool,
    /// Tightest raw trigger label observed this turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_trigger: Option<String>,
    /// Model that produced the doomed attempts.
    pub model: String,
}

#[derive(Serialize)]
pub struct TraceUploadAttempted {
    pub session_id: String,
    pub turn_number: u64,
    pub upload_method: String,
}

#[derive(Serialize)]
pub struct TraceUploadSucceeded {
    pub session_id: String,
    pub turn_number: u64,
    pub upload_method: String,
    pub fully_uploaded: bool,
}

#[derive(Serialize)]
pub struct TraceUploadSkipped {
    pub session_id: String,
    pub turn_number: u64,
    pub reason: String,
}

#[derive(Serialize)]
pub struct TraceUploadFailed {
    pub session_id: String,
    pub turn_number: u64,
    pub upload_method: String,
    pub error_category: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
}

/// Why trace uploads are enabled or disabled for a given prompt.
/// Recorded on the `agent.prompt` span as `upload_reason` for analytics queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceUploadReason {
    /// ZDR (zero data retention) team — all uploads disabled.
    ZdrTeam,
    /// `[telemetry] trace_upload = false` in config.
    FeatureOff,
    /// No cloud.echo-agent.invalid auth or deployment key.
    NoCredentials,
    /// Direct-to-bucket S3 upload.
    DirectS3,
    /// Proxy mode via cloud.echo-agent.invalid auth.
    Proxy,
    /// Direct GCS with service account key.
    DirectGcs,
    /// Session handle not found (edge case).
    SessionNotFound,
}

impl TraceUploadReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ZdrTeam => "zdr_team",
            Self::FeatureOff => "feature_off",
            Self::NoCredentials => "no_credentials",
            Self::DirectS3 => "direct_s3",
            Self::Proxy => "proxy",
            Self::DirectGcs => "direct_gcs",
            Self::SessionNotFound => "session_not_found",
        }
    }

    pub fn from_upload_method(method: &Option<echo_agent_file_utils::UploadMethod>) -> Self {
        match method {
            Some(echo_agent_file_utils::UploadMethod::Proxy { .. }) => Self::Proxy,
            Some(echo_agent_file_utils::UploadMethod::S3 { .. }) => Self::DirectS3,
            Some(echo_agent_file_utils::UploadMethod::Direct { .. }) => Self::DirectGcs,
            None => Self::NoCredentials,
        }
    }
}

#[cfg(test)]
mod tests {
    use echo_agent_file_utils::UploadMethod;

    use super::TraceUploadReason;

    /// The `echo-agent-shell-doom_loop_recovery` Mixpanel event's name and
    /// property keys are dashboard contracts — pin them.
    #[test]
    fn doom_loop_recovery_event_shape_is_stable() {
        use crate::events::TelemetryEvent;
        assert_eq!(super::DoomLoopRecovery::NAME, "doom_loop_recovery");
        let with_trigger = serde_json::to_value(super::DoomLoopRecovery {
            session_id: "s1".to_string(),
            turn_number: 7,
            attempts: 2,
            accepted_after_budget: true,
            top_trigger: Some("tail_repetition:4@thinking".to_string()),
            model: "echo-agent-4.5".to_string(),
        })
        .unwrap();
        assert_eq!(
            with_trigger,
            serde_json::json!({
                "session_id": "s1",
                "turn_number": 7,
                "attempts": 2,
                "accepted_after_budget": true,
                "top_trigger": "tail_repetition:4@thinking",
                "model": "echo-agent-4.5",
            })
        );
        let no_trigger = serde_json::to_value(super::DoomLoopRecovery {
            session_id: "s1".to_string(),
            turn_number: 7,
            attempts: 1,
            accepted_after_budget: false,
            top_trigger: None,
            model: "echo-agent-4.5".to_string(),
        })
        .unwrap();
        assert!(no_trigger.get("top_trigger").is_none(), "None is omitted");
    }

    /// `as_str` values are recorded on the `agent.prompt` span as
    /// `upload_reason` and queried in analytics — they are a wire contract and
    /// must not drift.
    #[test]
    fn as_str_values_are_stable() {
        assert_eq!(TraceUploadReason::ZdrTeam.as_str(), "zdr_team");
        assert_eq!(TraceUploadReason::FeatureOff.as_str(), "feature_off");
        assert_eq!(TraceUploadReason::NoCredentials.as_str(), "no_credentials");
        assert_eq!(TraceUploadReason::DirectS3.as_str(), "direct_s3");
        assert_eq!(TraceUploadReason::Proxy.as_str(), "proxy");
        assert_eq!(TraceUploadReason::DirectGcs.as_str(), "direct_gcs");
        assert_eq!(
            TraceUploadReason::SessionNotFound.as_str(),
            "session_not_found"
        );
    }

    /// Each `UploadMethod` maps to its corresponding reason; `None` (no
    /// credentials resolved) maps to `NoCredentials`.
    #[test]
    fn from_upload_method_maps_each_variant() {
        assert_eq!(
            TraceUploadReason::from_upload_method(&None),
            TraceUploadReason::NoCredentials
        );
        assert_eq!(
            TraceUploadReason::from_upload_method(&Some(UploadMethod::Direct {
                service_account_key: None,
            })),
            TraceUploadReason::DirectGcs
        );
        assert_eq!(
            TraceUploadReason::from_upload_method(&Some(UploadMethod::Proxy {
                proxy_base_url: String::new(),
                user_token: String::new(),
                deployment_key: None,
                alpha_test_key: None,
            })),
            TraceUploadReason::Proxy
        );
        assert_eq!(
            TraceUploadReason::from_upload_method(&Some(UploadMethod::S3 {
                bucket: String::new(),
                region: String::new(),
                credentials_file: None,
                credentials_content: None,
                endpoint_url: None,
            })),
            TraceUploadReason::DirectS3
        );
    }
}
