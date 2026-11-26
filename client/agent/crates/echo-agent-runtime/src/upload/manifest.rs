//! Upload manifest: authoritative "turn upload is done" signal.
use super::turn::PromptTraceContext;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;
pub(crate) const MANIFEST_SCHEMA_VERSION: u32 = 3;
#[derive(Debug, serde::Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ArtifactStatus {
    Succeeded,
    Failed,
    Skipped,
    Enqueued,
}
#[derive(serde::Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ManifestUploadMethod {
    Proxy,
    Direct,
    S3,
}
impl ManifestUploadMethod {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Proxy => "proxy",
            Self::Direct => "direct",
            Self::S3 => "s3",
        }
    }
}
#[derive(Debug, serde::Serialize, Clone)]
pub(crate) struct FailureDetail {
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
#[derive(serde::Serialize)]
pub(crate) struct UploadManifest {
    pub schema_version: u32,
    pub fully_uploaded: bool,
    pub completed_at: DateTime<Utc>,
    pub upload_method: ManifestUploadMethod,
    pub artifacts: HashMap<String, ArtifactStatus>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub failure_details: HashMap<String, FailureDetail>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub skip_details: HashMap<String, String>,
    /// Writer discriminator for non-standard producers; absent for the live
    /// turn-upload path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<&'static str>,
}
impl UploadManifest {
    pub(crate) fn error(upload_method: ManifestUploadMethod) -> Self {
        Self {
            schema_version: MANIFEST_SCHEMA_VERSION,
            fully_uploaded: false,
            completed_at: Utc::now(),
            upload_method,
            artifacts: HashMap::new(),
            failure_details: HashMap::new(),
            skip_details: HashMap::new(),
            source: None,
        }
    }
}
#[derive(Debug, Default)]
pub(crate) struct ArtifactTrackerInner {
    pub statuses: HashMap<String, ArtifactStatus>,
    pub failures: HashMap<String, FailureDetail>,
    pub skips: HashMap<String, String>,
}
pub(crate) type ArtifactTracker = Arc<parking_lot::Mutex<ArtifactTrackerInner>>;
pub(crate) fn new_artifact_tracker() -> ArtifactTracker {
    Arc::new(parking_lot::Mutex::new(ArtifactTrackerInner::default()))
}
pub(crate) enum ArtifactResult<'a> {
    Succeeded,
    /// Handed to the async upload pipeline; see [`ArtifactStatus::Enqueued`].
    Enqueued,
    Failed {
        reason: &'a str,
        error: Option<&'a str>,
    },
}
pub(crate) fn record_artifact(
    tracker: &ArtifactTracker,
    filename: &str,
    result: ArtifactResult<'_>,
) {
    match result {
        ArtifactResult::Succeeded => {
            tracker
                .lock()
                .statuses
                .insert(filename.to_owned(), ArtifactStatus::Succeeded);
        }
        ArtifactResult::Enqueued => {
            tracker
                .lock()
                .statuses
                .insert(filename.to_owned(), ArtifactStatus::Enqueued);
        }
        ArtifactResult::Failed { reason, error } => {
            let key = filename.to_owned();
            let mut inner = tracker.lock();
            inner.statuses.insert(key.clone(), ArtifactStatus::Failed);
            inner.failures.insert(
                key,
                FailureDetail {
                    reason: reason.to_owned(),
                    error: error.map(|s| truncate(s).to_owned()),
                },
            );
        }
    }
}
fn truncate(s: &str) -> &str {
    match s.char_indices().nth(512) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}
pub(crate) fn skip_artifact(tracker: &ArtifactTracker, filename: &str, reason: &str) {
    let key = filename.to_owned();
    let mut inner = tracker.lock();
    inner.statuses.insert(key.clone(), ArtifactStatus::Skipped);
    inner.skips.insert(key, reason.to_owned());
}
/// `fully_uploaded` is `true` iff no artifact has status `Failed`. `Enqueued`
/// counts as non-failure because every writer of it has passed a real queue
/// hand-off gate (see [`ArtifactStatus::Enqueued`]) — pre-handoff timeouts
/// record `Failed` instead — and flagging an accepted hand-off as failure
/// would permanently park a turn whose artifacts land moments later.
pub(crate) fn build_manifest(
    tracker: &ArtifactTracker,
    upload_method: ManifestUploadMethod,
    source: Option<&'static str>,
) -> UploadManifest {
    let inner = tracker.lock();
    let artifacts = inner.statuses.clone();
    let failure_details: HashMap<String, FailureDetail> = inner
        .failures
        .iter()
        .filter(|(k, _)| matches!(artifacts.get(k.as_str()), Some(ArtifactStatus::Failed)))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let skip_details: HashMap<String, String> = inner
        .skips
        .iter()
        .filter(|(k, _)| matches!(artifacts.get(k.as_str()), Some(ArtifactStatus::Skipped)))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let fully_uploaded = !artifacts
        .values()
        .any(|s| matches!(s, ArtifactStatus::Failed));
    UploadManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        fully_uploaded,
        completed_at: Utc::now(),
        upload_method,
        artifacts,
        failure_details,
        skip_details,
        source,
    }
}
#[derive(Clone)]
pub(crate) struct ArtifactUploadContext {
    pub(crate) gcs_config: crate::session::repo_changes::TraceExportConfig,
    pub(crate) artifact_tracker: ArtifactTracker,
}
pub(crate) fn resolve_upload_method(
    gcs_config: &crate::session::repo_changes::TraceExportConfig,
) -> ManifestUploadMethod {
    match &gcs_config.upload_method {
        crate::session::repo_changes::UploadMethod::Proxy { .. } => ManifestUploadMethod::Proxy,
        crate::session::repo_changes::UploadMethod::Direct { .. } => ManifestUploadMethod::Direct,
        crate::session::repo_changes::UploadMethod::S3 { .. } => ManifestUploadMethod::S3,
    }
}
pub(crate) async fn write_error_manifest(ctx: &PromptTraceContext) {
    let method = resolve_upload_method(&ctx.gcs_config);
    write_upload_manifest(ctx, &UploadManifest::error(method)).await;
}
pub(crate) async fn write_upload_manifest(ctx: &PromptTraceContext, manifest: &UploadManifest) {
    let bytes = match serde_json::to_vec_pretty(manifest) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to serialize upload manifest");
            return;
        }
    };
    let gcs_path = format!(
        "{}/upload_manifest.json",
        ctx.gcs_config.gcs_prefix.as_deref().unwrap_or("")
    );
    super::trace::upload_artifact_to_gcs(
        ctx,
        &gcs_path,
        &bytes,
        "application/json",
        "upload_manifest",
    )
    .await;
}
#[cfg(test)]
mod tests {
    use super::*;
    /// Artifact names expected by the session-trace ingest pipeline.
    /// Artifacts recorded by the turn-end upload path (excludes
    /// `upload_tool_definitions`, which runs earlier in the turn).
    fn ingestion_expected_artifacts() -> Vec<&'static str> {
        vec![
            "metadata.json",
            "turn_result.json",
            "permission_decisions.json",
            "turn_messages.json",
            "memory.tar.gz",
        ]
    }
    fn build_full_manifest() -> UploadManifest {
        let tracker = new_artifact_tracker();
        for name in ingestion_expected_artifacts() {
            record_artifact(&tracker, name, ArtifactResult::Succeeded);
        }
        build_manifest(&tracker, ManifestUploadMethod::Proxy, None)
    }
    #[test]
    fn manifest_covers_all_expected_artifacts() {
        let manifest = build_full_manifest();
        let expected = ingestion_expected_artifacts();
        let missing: Vec<&str> = expected
            .iter()
            .filter(|name| !manifest.artifacts.contains_key(**name))
            .copied()
            .collect();
        assert!(
            missing.is_empty(),
            "Manifest is missing artifacts that the ingestion pipeline expects: {missing:?}. \
             Ensure upload functions record these via upload_trace_artifact / \
             upload_trace_artifact_blocking, or add explicit record_artifact / skip_artifact calls."
        );
    }
    #[test]
    fn fully_uploaded_true_when_all_ok() {
        let manifest = build_full_manifest();
        assert!(manifest.fully_uploaded);
    }
    #[test]
    fn fully_uploaded_false_when_any_failed() {
        let tracker = new_artifact_tracker();
        record_artifact(&tracker, "metadata.json", ArtifactResult::Succeeded);
        record_artifact(
            &tracker,
            "memory.tar.gz",
            ArtifactResult::Failed {
                reason: "upload_failed",
                error: None,
            },
        );
        let manifest = build_manifest(&tracker, ManifestUploadMethod::Proxy, None);
        assert!(!manifest.fully_uploaded);
    }
    /// `enqueued` is the wire value the flush-bounded blocking path writes for
    /// artifacts still uploading at manifest time; it must not read as failure.
    #[test]
    fn enqueued_status_serializes_and_keeps_fully_uploaded() {
        let tracker = new_artifact_tracker();
        record_artifact(&tracker, "metadata.json", ArtifactResult::Succeeded);
        record_artifact(&tracker, "turn_result.json", ArtifactResult::Enqueued);
        let manifest = build_manifest(&tracker, ManifestUploadMethod::S3, None);
        assert!(manifest.fully_uploaded);
        let json: serde_json::Value = serde_json::to_value(&manifest).unwrap();
        assert_eq!(json["artifacts"]["turn_result.json"], "enqueued");
        assert!(json.get("failure_details").is_none());
    }
    /// A later terminal outcome may overwrite `enqueued` (an in-flight
    /// upload finishing during the flush); last write wins.
    #[test]
    fn enqueued_status_upgrades_to_terminal_outcome() {
        let tracker = new_artifact_tracker();
        record_artifact(&tracker, "turn_messages.json", ArtifactResult::Enqueued);
        record_artifact(
            &tracker,
            "turn_messages.json",
            ArtifactResult::Failed {
                reason: "upload_failed",
                error: None,
            },
        );
        let manifest = build_manifest(&tracker, ManifestUploadMethod::Proxy, None);
        assert!(!manifest.fully_uploaded);
        assert!(matches!(
            manifest.artifacts.get("turn_messages.json"),
            Some(ArtifactStatus::Failed)
        ));
    }
    #[test]
    fn fully_uploaded_true_when_skipped() {
        let tracker = new_artifact_tracker();
        record_artifact(&tracker, "metadata.json", ArtifactResult::Succeeded);
        skip_artifact(&tracker, "memory.tar.gz", "artifact_disabled");
        let manifest = build_manifest(&tracker, ManifestUploadMethod::Direct, None);
        assert!(manifest.fully_uploaded);
    }
    #[test]
    fn fully_uploaded_true_when_all_skipped() {
        let tracker = new_artifact_tracker();
        skip_artifact(&tracker, "memory.tar.gz", "artifact_disabled");
        skip_artifact(&tracker, "turn_messages.json", "no_turn_messages_captured");
        let manifest = build_manifest(&tracker, ManifestUploadMethod::Proxy, None);
        assert!(manifest.fully_uploaded);
    }
    #[test]
    fn fully_uploaded_false_when_ok_and_skipped_and_failed() {
        let tracker = new_artifact_tracker();
        record_artifact(&tracker, "metadata.json", ArtifactResult::Succeeded);
        skip_artifact(&tracker, "turn_messages.json", "artifact_disabled");
        record_artifact(
            &tracker,
            "memory.tar.gz",
            ArtifactResult::Failed {
                reason: "upload_failed",
                error: None,
            },
        );
        let manifest = build_manifest(&tracker, ManifestUploadMethod::Proxy, None);
        assert!(!manifest.fully_uploaded);
    }
    #[test]
    fn error_manifest_is_not_fully_uploaded() {
        let manifest = UploadManifest::error(ManifestUploadMethod::Proxy);
        assert!(!manifest.fully_uploaded);
        assert!(manifest.artifacts.is_empty());
    }
    #[test]
    fn manifest_serializes_to_expected_json_shape() {
        let tracker = new_artifact_tracker();
        record_artifact(&tracker, "turn_messages.json", ArtifactResult::Succeeded);
        skip_artifact(&tracker, "memory.tar.gz", "artifact_disabled_for_turn");
        record_artifact(
            &tracker,
            "metadata.json",
            ArtifactResult::Failed {
                reason: "upload_failed",
                error: Some("HTTP 503: service unavailable"),
            },
        );
        let manifest = build_manifest(&tracker, ManifestUploadMethod::S3, None);
        let json: serde_json::Value = serde_json::to_value(&manifest).unwrap();
        assert_eq!(json["schema_version"], 3);
        assert_eq!(json["fully_uploaded"], false);
        assert_eq!(json["upload_method"], "s3");
        assert_eq!(json["artifacts"]["turn_messages.json"], "succeeded");
        assert_eq!(json["artifacts"]["memory.tar.gz"], "skipped");
        assert_eq!(json["artifacts"]["metadata.json"], "failed");
        assert!(json["completed_at"].is_string());
        let details = &json["failure_details"]["metadata.json"];
        assert_eq!(details["reason"], "upload_failed");
        assert_eq!(details["error"], "HTTP 503: service unavailable");
        assert_eq!(
            json["skip_details"]["memory.tar.gz"],
            "artifact_disabled_for_turn"
        );
    }
    #[test]
    fn skip_details_omitted_when_nothing_skipped() {
        let tracker = new_artifact_tracker();
        record_artifact(&tracker, "metadata.json", ArtifactResult::Succeeded);
        let manifest = build_manifest(&tracker, ManifestUploadMethod::Proxy, None);
        let json: serde_json::Value = serde_json::to_value(&manifest).unwrap();
        assert!(json.get("skip_details").is_none());
    }
    #[test]
    fn stale_skip_details_filtered_when_later_uploaded() {
        let tracker = new_artifact_tracker();
        skip_artifact(&tracker, "memory.tar.gz", "session_registry_disabled");
        record_artifact(&tracker, "memory.tar.gz", ArtifactResult::Succeeded);
        let manifest = build_manifest(&tracker, ManifestUploadMethod::Proxy, None);
        assert!(manifest.skip_details.is_empty());
    }
    #[test]
    fn failure_details_omitted_when_all_succeed() {
        let tracker = new_artifact_tracker();
        record_artifact(&tracker, "metadata.json", ArtifactResult::Succeeded);
        let manifest = build_manifest(&tracker, ManifestUploadMethod::Proxy, None);
        let json: serde_json::Value = serde_json::to_value(&manifest).unwrap();
        assert!(json.get("failure_details").is_none());
    }
    #[test]
    fn record_artifact_failure_sets_both_status_and_detail() {
        let tracker = new_artifact_tracker();
        record_artifact(
            &tracker,
            "memory.tar.gz",
            ArtifactResult::Failed {
                reason: "copy_failed",
                error: None,
            },
        );
        let manifest = build_manifest(&tracker, ManifestUploadMethod::Proxy, None);
        assert!(matches!(
            manifest.artifacts.get("memory.tar.gz"),
            Some(ArtifactStatus::Failed)
        ));
        let detail = manifest.failure_details.get("memory.tar.gz").unwrap();
        assert_eq!(detail.reason, "copy_failed");
        assert!(detail.error.is_none());
    }
    #[test]
    fn stale_failure_details_filtered_on_success() {
        let tracker = new_artifact_tracker();
        record_artifact(
            &tracker,
            "metadata.json",
            ArtifactResult::Failed {
                reason: "upload_failed",
                error: None,
            },
        );
        record_artifact(&tracker, "metadata.json", ArtifactResult::Succeeded);
        let manifest = build_manifest(&tracker, ManifestUploadMethod::Proxy, None);
        assert!(manifest.fully_uploaded);
        assert!(manifest.failure_details.is_empty());
    }
    #[test]
    fn upload_method_as_str_matches_serde() {
        for method in [
            ManifestUploadMethod::Proxy,
            ManifestUploadMethod::Direct,
            ManifestUploadMethod::S3,
        ] {
            let serde_str = serde_json::to_value(method)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned();
            assert_eq!(method.as_str(), serde_str);
        }
    }
}
