//! Per-turn trace artifact uploads to cloud storage.
use super::turn::{PromptTraceContext, UploadWait};
use crate::sampling::types::ToolDefinition;
use crate::session::repo_changes::{TraceExportConfig, UploadMethod};
use base64::Engine as _;
use echo_agent_file_utils::queue::{
    EnqueueOutcome, TraceExportSource, UploadQueue, UploadRetryPolicy,
};
use echo_agent_workspace::permission::PermissionEvent;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::oneshot;
use url::Url;
/// Upload the canonical tool definitions trace and wait for completion.
///
/// `ToolDefinition` serializes in Chat Completions format:
/// `{ "type": "function", "function": { ... } }`, which is the shape
/// downstream ingest/enrichment expects to read from `tool_definitions.json`.
pub(crate) async fn upload_tool_definitions(
    gcs_config: TraceExportConfig,
    auth_manager: Option<Arc<crate::auth::AuthManager>>,
    tool_definitions: &[ToolDefinition],
    artifact_tracker: Option<&super::manifest::ArtifactTracker>,
) {
    let Some(prefix) = gcs_config.gcs_prefix.as_deref() else {
        tracing::debug!("Skipping tool definitions upload: gcs_prefix is not set");
        return;
    };
    let bytes = match serde_json::to_vec_pretty(tool_definitions) {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::debug!(?e, "Failed to serialize tool definitions for trace upload");
            return;
        }
    };
    let prefix = prefix.trim_matches('/');
    let object_path = if prefix.is_empty() {
        "tool_definitions.json".to_string()
    } else {
        format!("{prefix}/tool_definitions.json")
    };
    use crate::upload::gcs::WithAuth as _;
    let ok = echo_agent_file_utils::gcs::upload_bytes(
        &gcs_config.with_auth(auth_manager),
        &object_path,
        &bytes,
        "application/json",
    )
    .await;
    if let Err(ref e) = ok {
        tracing::debug!(
            ?e,
            object_path = %object_path,
            "Failed to upload tool definitions trace"
        );
    }
    if let Some(manifest) = artifact_tracker {
        match &ok {
            Ok(_) => super::manifest::record_artifact(
                manifest,
                "tool_definitions.json",
                super::manifest::ArtifactResult::Succeeded,
            ),
            Err(e) => super::manifest::record_artifact(
                manifest,
                "tool_definitions.json",
                super::manifest::ArtifactResult::Failed {
                    reason: "upload_failed",
                    error: Some(&format!("{e:#}")),
                },
            ),
        }
    }
}
/// `restorable_turn_number` is not advanced without a cloud archive.
pub(crate) async fn upload_session_state(
    _ctx: &PromptTraceContext,
    _phase: &str,
    session_copy_rx: oneshot::Receiver<
        anyhow::Result<crate::session::persistence::SessionStateCopy>,
    >,
    _wait: UploadWait,
) -> super::turn::UploadOutcome {
    let _ = session_copy_rx.await;
    super::turn::UploadOutcome::Failed {
        reason: "session_state_upload_unavailable",
        status_code: None,
    }
}
/// Truth for a Defer-timeout of the blocking session-state upload: `Enqueued`
/// only while the cancellation left the item parked on queue confirmation (the
/// live worker still owns it); a cancelled direct attempt queued nothing
/// durable and must record the loss.
#[cfg(test)]
fn confirm_timeout_artifact_result(
    direct_attempt_started: bool,
) -> super::manifest::ArtifactResult<'static> {
    if direct_attempt_started {
        super::manifest::ArtifactResult::Failed {
            reason: "direct_upload_timed_out",
            error: None,
        }
    } else {
        super::manifest::ArtifactResult::Enqueued
    }
}
#[derive(Default)]
struct UploadFailure<'a> {
    artifact: &'a str,
    reason: &'static str,
    error: &'a str,
    phase: Option<&'a str>,
    gcs_path: Option<&'a str>,
    status_code: Option<u16>,
    bytes: Option<usize>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UploadFailureLogLevel {
    Error,
    Warn,
    Debug,
}
/// ERROR is what logging / alerting treat as a first-party incident, so it is
/// reserved for first-party backends (proxy, cloud storage); a failing
/// customer-managed S3 bucket is the customer's outage and logs at WARN.
/// Repeats within one failure episode drop to DEBUG.
fn upload_failure_log_level(method: &UploadMethod, prior_failures: u64) -> UploadFailureLogLevel {
    if prior_failures > 0 {
        UploadFailureLogLevel::Debug
    } else if matches!(method, UploadMethod::S3 { .. }) {
        UploadFailureLogLevel::Warn
    } else {
        UploadFailureLogLevel::Error
    }
}
/// Wire label for the upload backend used by structured session events.
fn upload_method_label(method: &UploadMethod) -> &'static str {
    use super::turn::TraceUploadReason;
    match method {
        UploadMethod::Direct { .. } => TraceUploadReason::DirectGcs,
        UploadMethod::Proxy { .. } => TraceUploadReason::Proxy,
        UploadMethod::S3 { .. } => TraceUploadReason::DirectS3,
    }
    .as_str()
}
/// A confirmed upload ends the session's failure episode; the next failure
/// logs at full detail again.
fn record_upload_success(ctx: &PromptTraceContext) {
    use std::sync::atomic::Ordering::Relaxed;
    ctx.session_handle
        .upload_failures_since_success
        .store(0, Relaxed);
}
/// Full detail (and the unified-log mirror) for the first failure of a
/// session's episode, split on `artifact` + `reason`; repeats log at debug
/// with `suppressed_count`. Only logging is suppressed, never the uploads.
fn record_upload_failure(ctx: &PromptTraceContext, f: UploadFailure<'_>) {
    use std::sync::atomic::Ordering::Relaxed;
    let prior_failures = ctx
        .session_handle
        .upload_failures_since_success
        .fetch_add(1, Relaxed);
    let level = upload_failure_log_level(&ctx.gcs_config.upload_method, prior_failures);
    let method = upload_method_label(&ctx.gcs_config.upload_method);
    macro_rules! log_failure {
        ($level:ident) => {
            tracing::$level!(
                artifact = f.artifact,
                reason = f.reason,
                method,
                phase = f.phase.unwrap_or(""),
                gcs_path = f.gcs_path.unwrap_or(""),
                status_code = ?f.status_code,
                bytes = ?f.bytes,
                session_id = %ctx.session_info.id.0,
                turn_number = ctx.turn_number,
                suppressed_count = prior_failures,
                error = f.error,
                "file upload failed"
            )
        };
    }
    match level {
        UploadFailureLogLevel::Error => log_failure!(error),
        UploadFailureLogLevel::Warn => log_failure!(warn),
        UploadFailureLogLevel::Debug => log_failure!(debug),
    }
    if prior_failures > 0 {
        return;
    }
    let msg = format!("upload failed: {} ({})", f.artifact, f.reason);
    let sid = Some(ctx.session_info.id.0.as_ref());
    let log_ctx = Some(serde_json::json!({
        "artifact": f.artifact,
        "reason": f.reason,
        "method": method,
        "error": f.error,
        "gcs_path": f.gcs_path,
        "status_code": f.status_code,
        "bytes": f.bytes,
        "phase": f.phase,
    }));
    if level == UploadFailureLogLevel::Warn {
        echo_agent_telemetry::unified_log::warn(&msg, sid, log_ctx);
    } else {
        echo_agent_telemetry::unified_log::error(&msg, sid, log_ctx);
    }
}
/// Increment when making breaking changes to PromptMetadata structure.
/// Re-exported from the shared types crate.
pub(crate) use echo_agent_chat_proxy_types::{
    GCS_SCHEMA_VERSION, LocalSandboxTelemetry, PromptMetadata, PromptMetadataParams,
};
pub(crate) fn local_sandbox_telemetry() -> Option<LocalSandboxTelemetry> {
    let profile = echo_agent_sandbox::configured_profile_name()?;
    Some(LocalSandboxTelemetry {
        profile: profile.to_owned(),
        applied: echo_agent_sandbox::is_active(),
    })
}
/// Strip username/password credentials from a git remote URL.
///
/// In CI environments, git config may inject access tokens via URL rewriting
/// (e.g., `url."https://x-access-token:TOKEN@github.com/".insteadOf`).
/// We strip these to avoid leaking credentials in metadata.
pub(crate) fn strip_url_credentials(url_str: &str) -> String {
    if let Ok(mut parsed) = Url::parse(url_str) {
        if !parsed.username().is_empty() || parsed.password().is_some() {
            let _ = parsed.set_username("");
            let _ = parsed.set_password(None);
            return parsed.to_string();
        }
        return url_str.to_string();
    }
    url_str.to_string()
}
/// Best-effort git repo root and remote URL for the session's cwd.
/// Runs git2 discovery off-thread since it walks parent directories.
pub(crate) async fn resolve_git_repo_info(cwd: &str) -> (Option<String>, Option<String>) {
    let cwd = cwd.to_owned();
    tokio::task::spawn_blocking(move || {
        let repo = git2::Repository::discover(&cwd).ok()?;
        let repo_root = repo.workdir().map(|p| p.to_string_lossy().to_string());
        let remote_url = repo
            .find_remote("origin")
            .ok()
            .and_then(|r| r.url().ok().map(strip_url_credentials));
        Some((repo_root, remote_url))
    })
    .await
    .ok()
    .flatten()
    .unwrap_or((None, None))
}
fn classify_workspace(cwd: &str) -> String {
    let path = std::path::Path::new(cwd);
    if path.ancestors().any(|p| p.join(".git").exists()) {
        "git".to_owned()
    } else if echo_agent_file_utils::workspace_classifier::is_project_dir(path) {
        "project".to_owned()
    } else {
        "non_project".to_owned()
    }
}
/// Fill in `repo_root`, `remote_url`, and `workspace_type` on a [`PromptMetadata`].
async fn fill_git_fields(metadata: &mut PromptMetadata, cwd: &str) {
    if metadata.repo_root.is_some() && metadata.remote_url.is_some() {
        metadata.workspace_type = Some("git".to_owned());
        return;
    }
    let (repo_root, remote_url) = resolve_git_repo_info(cwd).await;
    let has_repo = metadata.repo_root.is_some() || repo_root.is_some();
    metadata.workspace_type = Some(if has_repo {
        "git".to_owned()
    } else {
        let cwd = cwd.to_owned();
        tokio::task::spawn_blocking(move || classify_workspace(&cwd))
            .await
            .unwrap_or_else(|_| "non_project".to_owned())
    });
    if metadata.repo_root.is_none() {
        metadata.repo_root = repo_root;
    }
    if metadata.remote_url.is_none() {
        metadata.remote_url = remote_url;
    }
}
/// Fill in `repo_root`, `remote_url`, and `workspace_type` on a
/// [`PromptMetadata`].
pub(crate) async fn enrich_git_metadata(ctx: &PromptTraceContext, metadata: &mut PromptMetadata) {
    fill_git_fields(metadata, &ctx.session_info.cwd).await;
}
/// Metadata about the prompt turn, uploaded as JSON for tracing/debugging.
///
/// Note: Session state is uploaded as an archive by `upload_session_state()`.
/// See `complete_prompt_trace` for the upload flow.
/// The struct definition lives in the shared metadata types crate.
///
/// Uploads prompt metadata to cloud storage as JSON.
/// Path format: {session_id}/turn_{N}/metadata.json
pub(crate) async fn upload_metadata(ctx: &PromptTraceContext, metadata: PromptMetadata) {
    let mut metadata = metadata;
    enrich_git_metadata(ctx, &mut metadata).await;
    let metadata_json = match serde_json::to_vec_pretty(&metadata) {
        Ok(json) => json,
        Err(e) => {
            tracing::warn!(
                session_id = %ctx.session_info.id.0,
                turn_number = ctx.turn_number,
                error = %e,
                "Failed to serialize prompt metadata"
            );
            super::manifest::record_artifact(
                &ctx.artifact_tracker,
                "metadata.json",
                super::manifest::ArtifactResult::Failed {
                    reason: "serialize_failed",
                    error: Some(&format!("{e:#}")),
                },
            );
            return;
        }
    };
    let gcs_path = format!(
        "{}/metadata.json",
        ctx.gcs_config.gcs_prefix.as_deref().unwrap_or("")
    );
    upload_small_artifact(
        ctx,
        &metadata_json,
        &gcs_path,
        "application/json",
        "metadata",
        UploadWait::Confirm,
    )
    .await;
}
/// Uploads subagent session metadata to cloud storage as `subagent.json`.
///
/// Path format: `{child_session_id}/subagent.json` (session-root, not turn-scoped).
///
/// Called at spawn (`status = running`) and again at completion with final
/// status/duration/tool-calls/turns.
pub(crate) async fn upload_subagent_metadata(
    metadata: &crate::agent::subagent::SubagentSessionMetadata,
    bucket_url: &str,
    upload_method: crate::session::repo_changes::UploadMethod,
    auth_manager: std::sync::Arc<crate::auth::AuthManager>,
) {
    let json = match serde_json::to_vec_pretty(metadata) {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!(
                session_id = %metadata.child_session_id,
                error = %e,
                "Failed to serialize subagent metadata"
            );
            return;
        }
    };
    let gcs_path = format!("{}/subagent.json", metadata.child_session_id);
    let base_config = crate::session::repo_changes::TraceExportConfig {
        bucket_url: Some(bucket_url.to_owned()),
        service_account_key: None,
        prefix_dir: None,
        gcs_prefix: None,
        absolute_paths: false,
        archive_name_override: None,
        upload_method,
    };
    use crate::upload::gcs::WithAuth as _;
    let config = base_config.with_auth(Some(auth_manager));
    if let Err(e) =
        echo_agent_file_utils::gcs::upload_bytes(&config, &gcs_path, &json, "application/json")
            .await
    {
        tracing::warn!(
            session_id = %metadata.child_session_id,
            gcs_path = %gcs_path,
            error = %e,
            "Failed to upload subagent.json to GCS"
        );
    }
}
/// Uploads prompt images to cloud storage as standalone files.
/// Path format: {session_id}/turn_{N}/images/image_{i}.{ext}
///
/// Each image from the user prompt is decoded from base64 and uploaded
/// as a separate file. The file extension is derived from the MIME type.
pub(crate) async fn upload_images(
    ctx: &PromptTraceContext,
    images: &[agent_client_protocol::ImageContent],
) {
    if images.is_empty() {
        return;
    }
    let image_count = images.len();
    tracing::info!(
        session_id = %ctx.session_info.id.0,
        turn_number = ctx.turn_number,
        image_count,
        "Uploading prompt images to GCS"
    );
    for (i, image) in images.iter().enumerate() {
        let ext = mime_type_to_extension(&image.mime_type);
        let filename = format!("image_{i}.{ext}");
        let gcs_path = format!(
            "{}/images/{filename}",
            ctx.gcs_config.gcs_prefix.as_deref().unwrap_or("")
        );
        let decoded = match base64::engine::general_purpose::STANDARD.decode(&image.data) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!(
                    session_id = %ctx.session_info.id.0,
                    turn_number = ctx.turn_number,
                    image_index = i,
                    error = %e,
                    "Failed to decode base64 image data, skipping"
                );
                continue;
            }
        };
        upload_trace_artifact(ctx, &decoded, &gcs_path, &image.mime_type, "image").await;
    }
}
pub(crate) fn mime_type_to_extension(mime_type: &str) -> &str {
    match mime_type {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpeg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        "image/heic" => "heic",
        "image/heif" => "heif",
        "image/avif" => "avif",
        _ => "bin",
    }
}
pub(crate) async fn upload_full_prompt_txt(ctx: &PromptTraceContext, _full_prompt: &str) {
    super::manifest::skip_artifact(
        &ctx.artifact_tracker,
        "full_prompt.txt",
        "prompt_content_upload_disabled",
    );
}
/// Plugin state snapshot for cloud storage trace upload.
///
/// Captures which plugins are loaded, their enabled/trusted status, and basic metadata.
/// Uploaded as `plugins.json` alongside other per-turn trace artifacts.
pub(crate) async fn upload_plugin_state(
    ctx: &PromptTraceContext,
    registry: Option<&echo_agent_core::plugins::PluginRegistry>,
) {
    use echo_agent_core::plugins::discovery::PluginScope;
    /// Serializable plugin entry for trace upload.
    #[derive(serde::Serialize)]
    struct PluginEntry {
        name: String,
        id: String,
        enabled: bool,
        trusted: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        scope: String,
        skill_count: usize,
        agent_count: usize,
        mcp_server_count: usize,
        has_hooks: bool,
        has_inline_hooks_only: bool,
        has_inline_mcp_only: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        conflict: Option<String>,
    }
    let plugins: Vec<PluginEntry> = match registry {
        Some(reg) => reg
            .list()
            .into_iter()
            .map(|p| PluginEntry {
                name: p.name.clone(),
                id: p.id.0.clone(),
                enabled: p.enabled,
                trusted: p.trusted,
                version: p.version.clone(),
                description: p.description.clone(),
                scope: match p.scope {
                    PluginScope::CliOverride => "cli".to_string(),
                    PluginScope::Project => "project".to_string(),
                    PluginScope::User => "user".to_string(),
                    PluginScope::ConfigPath => "config".to_string(),
                },
                skill_count: p.skill_count,
                agent_count: p.agent_count,
                mcp_server_count: p.mcp_server_count,
                has_hooks: p.has_hooks,
                has_inline_hooks_only: p.has_inline_hooks_only,
                has_inline_mcp_only: p.has_inline_mcp_only,
                conflict: p.conflict.clone(),
            })
            .collect(),
        None => Vec::new(),
    };
    let payload = serde_json::json!({
        "schema_version": 1u32,
        "plugins": plugins,
    });
    let json = match serde_json::to_vec_pretty(&payload) {
        Ok(json) => json,
        Err(e) => {
            tracing::warn!(
                session_id = %ctx.session_info.id.0,
                turn_number = ctx.turn_number,
                error = %e,
                "Failed to serialize plugin state"
            );
            return;
        }
    };
    let gcs_path = format!(
        "{}/plugins.json",
        ctx.gcs_config.gcs_prefix.as_deref().unwrap_or("")
    );
    upload_small_artifact(
        ctx,
        &json,
        &gcs_path,
        "application/json",
        "plugins",
        UploadWait::Confirm,
    )
    .await;
}
use super::gcs::WithAuth as _;
use echo_agent_file_utils::gcs::upload_bytes;
/// Uploads bytes to cloud storage, logging start/finish and tracing success or failure.
pub(crate) async fn upload_artifact_to_gcs(
    ctx: &PromptTraceContext,
    gcs_path: &str,
    content: &[u8],
    content_type: &str,
    artifact: &str,
) -> Option<String> {
    let _upload_start = std::time::Instant::now();
    let config = ctx.gcs_config.with_auth(Some(ctx.auth_manager.clone()));
    match upload_bytes(&config, gcs_path, content, content_type).await {
        Ok(gcs_url) => {
            record_upload_success(ctx);
            tracing::info!(
                session_id = %ctx.session_info.id.0,
                turn_number = ctx.turn_number,
                artifact,
                gcs_url = %gcs_url,
                bytes = content.len(),
                "Artifact uploaded to GCS",
            );
            Some(gcs_url)
        }
        Err(e) => {
            let status_code = e
                .downcast_ref::<echo_agent_file_utils::storage_client::HttpUploadError>()
                .map(|e| e.status_code);
            record_upload_failure(
                ctx,
                UploadFailure {
                    artifact,
                    reason: "gcs_upload_failed",
                    error: &format!("{e:#}"),
                    gcs_path: Some(gcs_path),
                    bytes: Some(content.len()),
                    status_code,
                    ..Default::default()
                },
            );
            None
        }
    }
}
/// One-shot artifact upload + manifest recording.
///
/// `Confirm` (detached/interactive contexts) keeps the direct awaited upload:
/// the recorded status reflects the actual result, and an interactive turn's
/// manifest never races a queue it does not flush. `Defer` (blocking turn
/// end) routes through the durable queue accept so the prompt response stays
/// fast and a process exit cannot lose the artifact.
pub(crate) async fn upload_small_artifact(
    ctx: &PromptTraceContext,
    content: &[u8],
    gcs_path: &str,
    content_type: &str,
    artifact_name: &str,
    wait: UploadWait,
) {
    match wait {
        UploadWait::Confirm => {
            let ok = upload_artifact_to_gcs(ctx, gcs_path, content, content_type, artifact_name)
                .await
                .is_some();
            if let Some(filename) = gcs_path.rsplit('/').next() {
                super::manifest::record_artifact(
                    &ctx.artifact_tracker,
                    filename,
                    if ok {
                        super::manifest::ArtifactResult::Succeeded
                    } else {
                        super::manifest::ArtifactResult::Failed {
                            reason: "direct_upload_failed",
                            error: None,
                        }
                    },
                );
            }
        }
        UploadWait::Defer { deadline } => {
            let _ = upload_trace_artifact_deferred(
                ctx,
                content,
                gcs_path,
                content_type,
                artifact_name,
                deadline,
            )
            .await;
        }
    }
}
/// Lightweight back-reference from a parent turn to a spawned subagent.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct SubagentSpawnedRef {
    pub(crate) subagent_id: String,
    pub(crate) child_session_id: String,
    pub(crate) subagent_type: String,
    /// Human-readable spawn description; see
    /// [`crate::agent::subagent::SubagentSessionMetadata::description`] for
    /// why goal-role subagents need it serialized.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) persona: Option<String>,
    /// ID of the source subagent this session was resumed from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) resumed_from: Option<String>,
}
/// Metadata about the prompt turn outcome, uploaded as JSON.
///
/// Path format: {session_id}/turn_{N}/turn_result.json
#[derive(serde::Serialize)]
pub(crate) struct TurnResultMetadata {
    /// Schema version for this metadata format
    pub(crate) schema_version: &'static str,
    /// Request ID for this prompt (UUID generated by the agent)
    pub(crate) request_id: String,
    /// Whether the turn completed successfully (i.e., not cancelled and no error)
    pub(crate) completed: bool,
    /// Stop reason (if available)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stop_reason: Option<String>,
    /// Total tokens accumulated for the session (best-effort)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) total_tokens: Option<u64>,
    /// Last-turn input tokens (includes cached portion).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) input_tokens: Option<u64>,
    /// Last-turn input tokens served from prompt cache.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cached_input_tokens: Option<u64>,
    /// Last-turn output tokens (includes reasoning; reasoning also tracked in signals delta).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) output_tokens: Option<u64>,
    /// Error message (if available)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
    /// RFC3339 timestamp when this record was written
    pub(crate) finished_at: String,
    /// Cumulative session signals at turn end
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) signals: Option<crate::session::signals::SessionSignals>,
    /// Per-turn signal delta (what changed this turn)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) turn_delta: Option<crate::session::signals::SessionSignalsDelta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) resolved_model: Option<String>,
    /// Subagent sessions spawned during this turn (child_session_id list).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) subagents_spawned: Vec<SubagentSpawnedRef>,
    /// Prompt mode at turn start (from request _meta.mode).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) start_prompt_mode: Option<String>,
    /// Prompt mode at turn end (may differ from start if mode changed mid-turn).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) end_prompt_mode: Option<String>,
}
pub(crate) async fn upload_turn_result(
    ctx: &PromptTraceContext,
    result: &TurnResultMetadata,
    wait: UploadWait,
) {
    let json = match serde_json::to_vec_pretty(result) {
        Ok(json) => json,
        Err(e) => {
            tracing::warn!(
                session_id = %ctx.session_info.id.0,
                turn_number = ctx.turn_number,
                error = %e,
                "Failed to serialize turn result metadata"
            );
            return;
        }
    };
    let gcs_path = format!(
        "{}/turn_result.json",
        ctx.gcs_config.gcs_prefix.as_deref().unwrap_or("")
    );
    upload_small_artifact(
        ctx,
        &json,
        &gcs_path,
        "application/json",
        "turn_result",
        wait,
    )
    .await;
}
/// Uploads the out-of-band streaming-turn capture to cloud storage as JSON.
/// Path format: `{session_id}/turn_N/streaming_partial.json`
///
/// Called when a turn ended without `record_assistant_response` committing
/// the canonical assistant turn — user-cancel mid-stream, a sampler terminal
/// error such as `MaxTokensTruncation`, or a doomloop where every generation
/// returns reasoning-only. The artifact carries every uncommitted generation
/// of the turn as `segments[]` (cancel/error mid-response partials included,
/// regardless of whether they were reasoning, response text, or a tool call),
/// plus a flat joined `reasoning_text`/`response_text` view of them for the
/// currently-deployed trace viewer.
///
/// This artifact is intentionally separate from `chat.jsonl` /
/// `turn_messages.json` so the model never sees the partial on
/// subsequent turns (no conversation-history pollution).
pub(crate) async fn upload_streaming_partial(
    ctx: &PromptTraceContext,
    capture: &crate::session::acp_session::StreamingTurnCapture,
    wait: UploadWait,
) {
    let json = match serde_json::to_vec_pretty(capture) {
        Ok(json) => json,
        Err(e) => {
            tracing::warn!(
                session_id = %ctx.session_info.id.0,
                turn_number = ctx.turn_number,
                error = %e,
                "Failed to serialize streaming partial capture"
            );
            return;
        }
    };
    let gcs_path = format!(
        "{}/streaming_partial.json",
        ctx.gcs_config.gcs_prefix.as_deref().unwrap_or("")
    );
    upload_small_artifact(
        ctx,
        &json,
        &gcs_path,
        "application/json",
        "streaming_partial",
        wait,
    )
    .await;
}
/// Metadata uploaded when a session is shared.
#[derive(serde::Serialize)]
struct ShareMetadata {
    session_id: String,
    turn_number: u64,
    shared_at: String,
}
/// Type of session metadata to upload
pub(crate) enum SessionMetadataType {
    Share,
}
/// Uploads session metadata (share) to cloud storage.
/// Path format: share/{session_id}_{timestamp}_share.json
pub(crate) async fn upload_session_metadata(
    ctx: &PromptTraceContext,
    metadata_type: SessionMetadataType,
) {
    let session_id = ctx.session_info.id.0.to_string();
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let (metadata_json, gcs_path, artifact) = match metadata_type {
        SessionMetadataType::Share => {
            let share_metadata = ShareMetadata {
                session_id: session_id.clone(),
                turn_number: ctx.turn_number,
                shared_at: chrono::Utc::now().to_rfc3339(),
            };
            let json = match serde_json::to_vec(&share_metadata) {
                Ok(json) => json,
                Err(e) => {
                    tracing::warn!(
                        session_id = %session_id,
                        error = %e,
                        "Failed to serialize share metadata"
                    );
                    return;
                }
            };
            let path = format!("share/{}_{}_share.json", session_id, timestamp);
            (json, path, "share_metadata")
        }
    };
    upload_artifact_to_gcs(ctx, &gcs_path, &metadata_json, "application/json", artifact).await;
}
/// Upload memory .md files as `memory.tar.gz` alongside the per-turn trace.
/// Only runs when session registry is enabled via remote settings or config.toml.
pub(crate) async fn upload_memory_state(ctx: &PromptTraceContext) {
    if !ctx.session_registry_enabled {
        tracing::debug!("memory upload skipped: session_registry_enabled=false");
        super::manifest::skip_artifact(
            &ctx.artifact_tracker,
            "memory.tar.gz",
            "session_registry_disabled",
        );
        return;
    }
    let storage = crate::session::memory::MemoryStorage::new(
        std::path::Path::new(&ctx.session_info.cwd),
        None,
    );
    let archive = match crate::session::memory::archive::build_memory_archive(&storage) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(error = %e, "failed to build memory archive, skipping");
            return;
        }
    };
    if archive.is_empty() || archive.len() < 30 {
        return;
    }
    let prefix = ctx.gcs_config.gcs_prefix.as_deref().unwrap_or("");
    let gcs_path = format!("{prefix}/memory.tar.gz");
    upload_trace_artifact(
        ctx,
        &archive,
        &gcs_path,
        "application/gzip",
        "memory_archive",
    )
    .await;
}
/// Uploads the session-scoped unified log to cloud storage.
/// Path format: {session_id}/turn_{N}/unified_log.jsonl
///
/// Called only from 401/404 auth-failure diagnostics, never per turn.
///
/// Only entries belonging to the current session (matching `sid`) are included.
/// The snapshot runs on a blocking thread since `snapshot_session_log` reads
/// and parses the on-disk log file.
pub(crate) async fn upload_unified_log(ctx: &PromptTraceContext, wait: UploadWait) {
    let session_id = ctx.session_info.id.0.to_string();
    let log_bytes = match tokio::task::spawn_blocking(move || {
        echo_agent_telemetry::unified_log::snapshot_session_log(&session_id)
    })
    .await
    {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            tracing::debug!(
                session_id = %ctx.session_info.id.0,
                turn_number = ctx.turn_number,
                "No unified log entries for this session, skipping upload"
            );
            return;
        }
        Err(e) => {
            tracing::warn!(
                session_id = %ctx.session_info.id.0,
                turn_number = ctx.turn_number,
                error = %e,
                "Failed to snapshot unified log"
            );
            return;
        }
    };
    let gcs_path = format!(
        "{}/unified_log.jsonl",
        ctx.gcs_config.gcs_prefix.as_deref().unwrap_or("")
    );
    upload_small_artifact(
        ctx,
        &log_bytes,
        &gcs_path,
        "application/x-ndjson",
        "unified_log",
        wait,
    )
    .await;
    let full_log_bytes =
        tokio::task::spawn_blocking(echo_agent_telemetry::unified_log::snapshot_log).await;
    let user_id = ctx
        .auth_manager
        .current_or_expired()
        .map(|a| a.user_id)
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| "unknown".to_owned());
    if let Ok(Some(full_bytes)) = full_log_bytes {
        crate::upload::gcs::upload_to_auth_diagnostics(
            &full_bytes,
            &user_id,
            &ctx.gcs_config.upload_method,
            ctx.auth_manager.clone(),
        )
        .await;
    }
}
/// Uploads permission events to cloud storage.
/// Path format: {session_id}/turn_{N}/permission_decisions.json
pub(crate) async fn upload_permission_events(
    ctx: &PromptTraceContext,
    events: &[PermissionEvent],
    wait: UploadWait,
) {
    let events_json = match serde_json::to_vec_pretty(events) {
        Ok(json) => json,
        Err(e) => {
            tracing::warn!(
                session_id = %ctx.session_info.id.0,
                turn_number = ctx.turn_number,
                error = %e,
                "Failed to serialize permission events"
            );
            return;
        }
    };
    let gcs_path = format!(
        "{}/permission_decisions.json",
        ctx.gcs_config.gcs_prefix.as_deref().unwrap_or("")
    );
    upload_small_artifact(
        ctx,
        &events_json,
        &gcs_path,
        "application/json",
        "permission_events",
        wait,
    )
    .await;
}
pub(crate) async fn upload_turn_messages(
    ctx: &PromptTraceContext,
    _capture: echo_agent_chat_state::TurnCapture,
    _wait: UploadWait,
) -> bool {
    super::manifest::skip_artifact(
        &ctx.artifact_tracker,
        "turn_messages.json",
        "chat_content_upload_disabled",
    );
    true
}
/// A failed `chat_history.jsonl` archive build, tagged with the manifest
/// `reason` so the caller records the matching artifact-failure category
/// (`serialize_failed` vs `archive_failed`), mirroring `upload_turn_messages`.
#[derive(Debug)]
#[allow(dead_code)]
pub(crate) struct SessionStateBuildError {
    pub reason: &'static str,
    pub error: anyhow::Error,
}
fn session_state_archive_join_error(err: tokio::task::JoinError) -> SessionStateBuildError {
    SessionStateBuildError {
        reason: "archive_failed",
        error: anyhow::anyhow!("spawn_blocking join failed: {err}"),
    }
}
fn serialize_chat_history_jsonl(
    messages: &[echo_agent_sampling_types::conversation::ConversationItem],
) -> Result<Vec<u8>, SessionStateBuildError> {
    {
        let _ = messages;
        Ok(Vec::new())
    }
}
fn compress_chat_history_archive(jsonl: Vec<u8>) -> Result<Vec<u8>, SessionStateBuildError> {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    fn archive_failed(error: std::io::Error) -> SessionStateBuildError {
        SessionStateBuildError {
            reason: "archive_failed",
            error: error.into(),
        }
    }
    #[cfg(test)]
    apply_test_archive_fault().map_err(archive_failed)?;
    let mut archive_data = Vec::new();
    {
        let encoder = GzEncoder::new(&mut archive_data, Compression::default());
        let mut archive = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_size(jsonl.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        );
        archive
            .append_data(&mut header, "chat_history.jsonl", jsonl.as_slice())
            .map_err(archive_failed)?;
        archive
            .into_inner()
            .and_then(|encoder| encoder.finish())
            .map_err(archive_failed)?;
    }
    Ok(archive_data)
}
/// Build a gzipped tar holding a single `chat_history.jsonl` entry from the
/// in-memory conversation `messages`.
///
/// The trace viewer renders a turn's conversation only from the
/// `chat_history.jsonl` entry inside a session-state archive, parsed as JSONL.
/// Harness sub-turns upload no other session state, so we emit that shape from
/// the same items that feed `turn_messages.json`: each `ConversationItem`
/// serialized compactly, one per `\n`-terminated line. Empty `messages` yield a
/// zero-byte payload the viewer treats as "no history" (harness pairs always
/// carry ≥1 message, so this is only a safety floor).
pub(crate) async fn build_chat_history_session_state(
    messages: &[echo_agent_sampling_types::conversation::ConversationItem],
) -> Result<Vec<u8>, SessionStateBuildError> {
    let jsonl = serialize_chat_history_jsonl(messages)?;
    match tokio::task::spawn_blocking(move || compress_chat_history_archive(jsonl)).await {
        Ok(result) => result,
        Err(e) => Err(session_state_archive_join_error(e)),
    }
}
pub(crate) async fn build_chat_history_then_move_capture(
    capture: echo_agent_chat_state::TurnCapture,
) -> (
    Result<Vec<u8>, SessionStateBuildError>,
    echo_agent_chat_state::TurnCapture,
) {
    let session_state = build_chat_history_session_state(&capture.messages).await;
    (session_state, capture)
}
pub(crate) async fn upload_harness_session_archive(
    _ctx: &PromptTraceContext,
    _tar: Result<Vec<u8>, SessionStateBuildError>,
) -> bool {
    false
}
/// Credential resolver for the queue worker that supplies a refresh-aware
/// [`ShellAuthCredentialProvider`] and a [`StorageClientAttributionBridge`]
/// via [`TraceExportSource::proxy_credentials`] /
/// [`TraceExportSource::proxy_attribution`]. The queue worker stitches
/// both onto the resolved config before constructing the per-attempt
/// `StorageClient`, which is what closes the buffer-window 401 leak and
/// makes upload-queue 401s show up in the `auth_401_attribution` event
/// stream.
///
/// The `proxy_*` methods delegate to [`crate::upload::gcs::WithAuth`] so
/// the wiring stays in one place (the `TraceExportConfigWithAuth`
/// adapter). `resolve()` returns the bare `base_config` -- the static
/// `user_token` snapshot it carries is unused at the wire level (the
/// provider returned by `proxy_credentials` always drives the bearer)
/// but the queue worker still reads other fields like `gcs_prefix` /
/// `bucket_url` off the resolved config.
pub(crate) struct DynamicResolver {
    auth_manager: Arc<crate::auth::AuthManager>,
    base_config: TraceExportConfig,
}
impl DynamicResolver {
    /// Build the auth-bearing wrapper used by all three `proxy_*` methods.
    fn with_auth(&self) -> crate::upload::gcs::TraceExportConfigWithAuth {
        use crate::upload::gcs::WithAuth as _;
        self.base_config.with_auth(Some(self.auth_manager.clone()))
    }
}
impl TraceExportSource for DynamicResolver {
    fn resolve(&self) -> TraceExportConfig {
        let mut config = self.base_config.clone();
        if let crate::session::repo_changes::UploadMethod::Proxy {
            ref mut user_token, ..
        } = config.upload_method
        {
            let auth = self.auth_manager.current().or_else(|| {
                self.auth_manager.force_reload_from_disk();
                self.auth_manager.current()
            });
            if let Some(auth) = auth {
                *user_token = auth.key;
            }
        }
        config
    }
    fn proxy_attribution(
        &self,
    ) -> Option<Arc<dyn echo_agent_file_utils::storage_client::Auth401AttributionCallback>> {
        echo_agent_file_utils::gcs::StorageConfig::proxy_attribution(&self.with_auth())
    }
    fn proxy_credentials(&self) -> Option<Arc<dyn echo_agent_auth::AuthCredentialProvider>> {
        echo_agent_file_utils::gcs::StorageConfig::proxy_credentials(&self.with_auth())
    }
    fn proxy_http_client(&self) -> Option<reqwest::Client> {
        echo_agent_file_utils::gcs::StorageConfig::proxy_http_client(&self.with_auth())
    }
    fn has_usable_credential(&self) -> bool {
        if let crate::session::repo_changes::UploadMethod::Proxy {
            deployment_key: Some(_),
            ..
        } = &self.base_config.upload_method
        {
            return true;
        }
        self.auth_manager.has_usable_token()
    }
    /// Defers to the `AuthManager` token-rotation notifier (same mechanism
    /// the signals sync loop waits on, so parking adds no refresh paths).
    /// `None` after an IdP-confirmed permanent failure: the queue drops
    /// instead of parking for an unrecoverable credential.
    fn wait_for_auth_recovery(
        &self,
        failed_bearer: Option<&str>,
        timeout: std::time::Duration,
    ) -> Option<std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + '_>>> {
        if self.auth_manager.has_permanent_failure() {
            return None;
        }
        let current_wire = match &self.base_config.upload_method {
            crate::session::repo_changes::UploadMethod::Proxy {
                deployment_key: Some(dk),
                ..
            } => Some(dk.clone()),
            _ => self.auth_manager.current_or_expired().map(|a| a.key),
        };
        if let (Some(failed), Some(current)) = (failed_bearer, current_wire)
            && current != failed
        {
            return Some(Box::pin(std::future::ready(true)));
        }
        let am = self.auth_manager.clone();
        Some(Box::pin(
            async move { am.wait_for_token_refresh(timeout).await },
        ))
    }
    fn resolve_async(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = TraceExportConfig> + Send + '_>> {
        Box::pin(async move {
            let mut config = self.base_config.clone();
            if let crate::session::repo_changes::UploadMethod::Proxy {
                ref mut user_token, ..
            } = config.upload_method
            {
                match self.auth_manager.get_valid_token().await {
                    Ok(key) => *user_token = key,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "auth: upload credential resolve failed"
                        )
                    }
                }
            }
            config
        })
    }
}
/// The spill dir is shared by every session's queue in the process, so the
/// reconcile runs at most once per verdict class: one recovery, and one purge
/// that may escalate over it if data collection is disabled later in the same
/// process (re-auth to a ZDR account, a leader session carrying an opt-out).
static SPILL_RECONCILE_STATE: std::sync::atomic::AtomicU8 =
    std::sync::atomic::AtomicU8::new(SPILL_NOT_RUN);
const SPILL_NOT_RUN: u8 = 0;
const SPILL_RAN_RECOVERY: u8 = 1;
const SPILL_RAN_PURGE: u8 = 2;
/// Claim the reconcile transition for the caller's collection verdict.
/// Enabled runs only from a fresh state (a purge is never resurrected);
/// disabled runs from fresh OR escalates exactly once over a prior recovery.
fn claim_spill_reconcile(state: &std::sync::atomic::AtomicU8, collection_enabled: bool) -> bool {
    use std::sync::atomic::Ordering::Relaxed;
    if collection_enabled {
        state
            .compare_exchange(SPILL_NOT_RUN, SPILL_RAN_RECOVERY, Relaxed, Relaxed)
            .is_ok()
    } else {
        state
            .compare_exchange(SPILL_NOT_RUN, SPILL_RAN_PURGE, Relaxed, Relaxed)
            .is_ok()
            || state
                .compare_exchange(SPILL_RAN_RECOVERY, SPILL_RAN_PURGE, Relaxed, Relaxed)
                .is_ok()
    }
}
/// Reconcile upload-queue spill pairs left by a prior process life:
/// re-enqueue them when uploads are enabled (`queue` present), purge them
/// when data collection is disabled (`None`). Detached so session setup
/// never waits on disk or cloud I/O.
pub(crate) fn spawn_startup_spill_reconcile(
    echo_agent_home: std::path::PathBuf,
    queue: Option<UploadQueue>,
) {
    if !claim_spill_reconcile(&SPILL_RECONCILE_STATE, queue.is_some()) {
        return;
    }
    tokio::spawn(async move {
        match queue {
            Some(queue) => {
                let report =
                    echo_agent_workspace::recovery::run_startup_recovery(&echo_agent_home, &queue)
                        .await;
                tracing::info!(?report, "startup spill recovery complete");
                queue.cleanup_orphans(echo_agent_file_utils::queue::DEFAULT_MAX_AGE);
            }
            None => {
                let purged = tokio::task::spawn_blocking(move || {
                    echo_agent_workspace::recovery::purge_spilled_items(&echo_agent_home)
                })
                .await;
                match purged {
                    Ok(removed) => {
                        tracing::info!(removed, "purged spilled uploads from a prior run");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "startup spill purge task failed")
                    }
                }
            }
        }
    });
}
/// Bounded, non-terminal flush of the session's upload queue: wait until
/// every queued item settles or the deadline passes. The worker stays alive
/// either way, so later turns (and the flush-timeout stragglers themselves)
/// keep uploading in the background.
pub(crate) async fn flush_upload_queue(
    ctx: &PromptTraceContext,
    deadline: tokio::time::Instant,
) -> usize {
    let Some(queue) = &ctx.upload_queue else {
        return 0;
    };
    let budget = deadline.saturating_duration_since(tokio::time::Instant::now());
    let remaining = queue.wait_idle(budget).await;
    if remaining > 0 {
        tracing::info!(
            remaining,
            "upload flush deadline reached; uploads continue in the background"
        );
    }
    remaining
}
/// Budget for one awaited attempt on the blocking path (a non-durable-accept
/// direct upload, or the manifest write): whatever remains of the flush
/// deadline, floored at 10s (the attempt must still happen after a fully
/// consumed deadline — the manifest is the ingestion trigger) and capped at
/// 30s (no storage client on this path has a request timeout, so this cap is
/// the only bound against a tarpit endpoint).
pub(crate) fn blocking_attempt_budget(deadline: tokio::time::Instant) -> std::time::Duration {
    deadline
        .saturating_duration_since(tokio::time::Instant::now())
        .max(std::time::Duration::from_secs(10))
        .min(std::time::Duration::from_secs(30))
}
/// Blocking error path: bounded queue flush, then the error manifest under
/// its own budget so the prompt response cannot hang on the final write.
pub(crate) async fn flush_then_write_error_manifest(
    ctx: &PromptTraceContext,
    deadline: tokio::time::Instant,
) {
    flush_upload_queue(ctx, deadline).await;
    let budget = blocking_attempt_budget(deadline);
    if tokio::time::timeout(budget, super::manifest::write_error_manifest(ctx))
        .await
        .is_err()
    {
        tracing::warn!("error manifest write timed out");
    }
}
/// Delete `upload_queue/scratch` (staging copies only). Does not touch the
/// durable queue worker's spill files under `upload_queue/` root.
pub(crate) fn purge_stale_upload_scratch_dir(scratch_dir: &Path) -> std::io::Result<bool> {
    if !scratch_dir.exists() {
        return Ok(false);
    }
    let is_scratch = scratch_dir.file_name().is_some_and(|n| n == "scratch")
        && scratch_dir
            .parent()
            .and_then(|p| p.file_name())
            .is_some_and(|n| n == "upload_queue");
    if !is_scratch {
        return Ok(false);
    }
    std::fs::remove_dir_all(scratch_dir)?;
    Ok(true)
}
/// Once-per-process: best-effort removal of leftover `upload_queue/scratch`
/// staging written by other builds of the shell — this build never writes it,
/// so anything found there is stale. Never touches the durable queue itself.
/// Skipped under cargo test so unit/integration helpers cannot wipe a
/// developer's real home.
pub(crate) fn spawn_purge_stale_upload_scratch() {
    {
        use std::sync::atomic::{AtomicBool, Ordering};
        if std::env::var_os("RUST_TEST_THREADS").is_some()
            || std::env::var_os("CARGO_TARGET_TMPDIR").is_some()
        {
            return;
        }
        static PURGE_STARTED: AtomicBool = AtomicBool::new(false);
        if PURGE_STARTED.swap(true, Ordering::Relaxed) {
            return;
        }
        let dir = crate::util::echo_agent_home::echo_agent_home()
            .join("upload_queue")
            .join("scratch");
        let run = move || match purge_stale_upload_scratch_dir(&dir) {
            Ok(true) => {
                tracing::info!(
                    path = %dir.display(),
                    "removed stale upload_queue/scratch staging"
                )
            }
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(
                    path = %dir.display(),
                    error = %e,
                    "failed to remove stale upload_queue/scratch staging"
                )
            }
        };
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::spawn_blocking(run);
        } else {
            std::thread::spawn(run);
        }
    }
}
/// Spawn a background upload queue for the given trace config.
///
/// Credentials are re-read on each upload attempt via [`DynamicResolver`].
/// Session/trace artifacts use this queue for durable spill in every build.
pub(crate) fn spawn_upload_queue(
    echo_agent_home: &Path,
    gcs_config: &TraceExportConfig,
    client_version: Option<&str>,
    auth_manager: Arc<crate::auth::AuthManager>,
) -> UploadQueue {
    let resolver: Arc<dyn TraceExportSource> = Arc::new(DynamicResolver {
        auth_manager,
        base_config: gcs_config.clone(),
    });
    let queue = UploadQueue::spawn(echo_agent_home, resolver, UploadRetryPolicy::default());
    if let Some(ver) = client_version {
        queue.with_client_version(ver)
    } else {
        queue
    }
}
/// Only these accept shapes are durably owned by the queue (temp + recovery
/// sidecar on disk, flushed by the turn-end wait or recovered next run).
/// `FellBackToInline` is a fire-and-forget task the flush cannot see and
/// `Failed` was never handed off, so both need a real awaited attempt before
/// the manifest may claim anything.
fn enqueue_outcome_is_durable(outcome: &EnqueueOutcome) -> bool {
    match outcome {
        EnqueueOutcome::Enqueued | EnqueueOutcome::Deduplicated => true,
        EnqueueOutcome::FellBackToInline
        | EnqueueOutcome::Failed { .. }
        | EnqueueOutcome::Skipped { .. } => false,
    }
}
/// Durable-accept a trace artifact for the flush-bounded blocking path: the
/// bytes and a recovery sidecar are on local disk once this returns and the
/// queue owns the upload (recorded `Enqueued`; the caller runs one bounded
/// queue flush). Non-durable accept shapes and queue-less contexts get one
/// awaited direct attempt — bounded by `blocking_attempt_budget(deadline)`,
/// since the storage clients carry no request timeout — so the recorded
/// status is the real result. `Ok` means durably accepted or directly
/// uploaded.
pub(crate) async fn upload_trace_artifact_deferred(
    ctx: &PromptTraceContext,
    content: &[u8],
    gcs_path: &str,
    content_type: &str,
    artifact_name: &str,
    deadline: tokio::time::Instant,
) -> anyhow::Result<()> {
    if let Some(queue) = &ctx.upload_queue {
        let session_id = ctx.session_info.id.0.to_string();
        let outcome = queue
            .enqueue_bytes_blocking(
                content,
                gcs_path,
                content_type,
                artifact_name,
                &session_id,
                ctx.turn_number,
            )
            .await;
        if enqueue_outcome_is_durable(&outcome) {
            if let Some(filename) = gcs_path.rsplit('/').next() {
                super::manifest::record_artifact(
                    &ctx.artifact_tracker,
                    filename,
                    super::manifest::ArtifactResult::Enqueued,
                );
            }
            return Ok(());
        }
        tracing::debug!(
            artifact = artifact_name,
            ?outcome,
            "durable enqueue not owned by the queue; attempting direct upload"
        );
    }
    let attempt = tokio::time::timeout(
        blocking_attempt_budget(deadline),
        upload_artifact_to_gcs(ctx, gcs_path, content, content_type, artifact_name),
    )
    .await;
    let failure_reason = match &attempt {
        Ok(Some(_url)) => None,
        Ok(None) => Some("direct_upload_failed"),
        Err(_) => {
            record_upload_failure(
                ctx,
                UploadFailure {
                    artifact: artifact_name,
                    reason: "direct_upload_timed_out",
                    error: "direct upload attempt exceeded the blocking attempt budget",
                    gcs_path: Some(gcs_path),
                    bytes: Some(content.len()),
                    ..Default::default()
                },
            );
            Some("direct_upload_timed_out")
        }
    };
    if let Some(filename) = gcs_path.rsplit('/').next() {
        super::manifest::record_artifact(
            &ctx.artifact_tracker,
            filename,
            match failure_reason {
                None => super::manifest::ArtifactResult::Succeeded,
                Some(reason) => super::manifest::ArtifactResult::Failed {
                    reason,
                    error: None,
                },
            },
        );
    }
    match failure_reason {
        None => Ok(()),
        Some(reason) => Err(anyhow::anyhow!("direct upload attempt failed: {reason}")),
    }
}
/// Upload a trace artifact via the queue, falling back to inline upload.
pub(crate) async fn upload_trace_artifact(
    ctx: &PromptTraceContext,
    content: &[u8],
    gcs_path: &str,
    content_type: &str,
    artifact_name: &str,
) {
    let (ok, err_msg) = if let Some(queue) = &ctx.upload_queue {
        let session_id = ctx.session_info.id.0.to_string();
        match queue
            .enqueue(
                content,
                gcs_path,
                content_type,
                artifact_name,
                &session_id,
                ctx.turn_number,
            )
            .await
        {
            Ok(()) => {
                tracing::debug!(
                    artifact = artifact_name,
                    "Artifact enqueued for background upload"
                );
                (true, None)
            }
            Err(e) => {
                tracing::warn!(
                    artifact = artifact_name,
                    error = ?e,
                    "Enqueue failed, inline fallback also failed"
                );
                (false, Some(format!("{e:#}")))
            }
        }
    } else if upload_artifact_to_gcs(ctx, gcs_path, content, content_type, artifact_name)
        .await
        .is_some()
    {
        (true, None)
    } else {
        (false, Some("inline upload failed".to_owned()))
    };
    if let Some(filename) = gcs_path.rsplit('/').next() {
        super::manifest::record_artifact(
            &ctx.artifact_tracker,
            filename,
            if ok {
                super::manifest::ArtifactResult::Succeeded
            } else {
                super::manifest::ArtifactResult::Failed {
                    reason: "upload_failed",
                    error: err_msg.as_deref(),
                }
            },
        );
    }
}
#[cfg(test)]
fn sort_session_files_by_priority(files: &mut [crate::session::persistence::CopiedSessionFile]) {
    files.sort_by_key(|f| match f.name.as_str() {
        "summary.json" => 0,
        "chat_history.jsonl" => 1,
        "events.jsonl" => 2,
        "updates.jsonl" => 3,
        _ => 4,
    });
}
#[cfg(test)]
#[derive(Clone, Copy)]
enum ArchiveBuildFault {
    Panic = 1,
    Io = 2,
}
#[cfg(test)]
static ARCHIVE_BUILD_FAULT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
#[cfg(test)]
struct ArchiveFaultGuard;
#[cfg(test)]
impl Drop for ArchiveFaultGuard {
    fn drop(&mut self) {
        ARCHIVE_BUILD_FAULT.store(0, std::sync::atomic::Ordering::SeqCst);
    }
}
#[cfg(test)]
fn set_archive_build_fault(fault: ArchiveBuildFault) -> ArchiveFaultGuard {
    ARCHIVE_BUILD_FAULT.store(fault as u8, std::sync::atomic::Ordering::SeqCst);
    ArchiveFaultGuard
}
#[cfg(test)]
fn apply_test_archive_fault() -> std::io::Result<()> {
    match ARCHIVE_BUILD_FAULT.swap(0, std::sync::atomic::Ordering::SeqCst) {
        1 => panic!("test-forced archive panic"),
        2 => Err(std::io::Error::other("test-forced archive io failure")),
        _ => Ok(()),
    }
}
#[cfg(test)]
fn build_session_state_archive(
    mut session_copy: crate::session::persistence::SessionStateCopy,
    session_id: &str,
) -> std::io::Result<Vec<u8>> {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    #[cfg(test)]
    apply_test_archive_fault()?;
    sort_session_files_by_priority(&mut session_copy.files);
    let mut archive_data = Vec::new();
    {
        let encoder = GzEncoder::new(&mut archive_data, Compression::default());
        let mut archive = tar::Builder::new(encoder);
        for file in &session_copy.files {
            let mut header = tar::Header::new_gnu();
            header.set_size(file.data.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            );
            if let Err(e) = archive.append_data(&mut header, &file.name, file.data.as_slice()) {
                tracing::warn!(
                    session_id = %session_id,
                    file = %file.name,
                    error = %e,
                    "Failed to add file to session state archive"
                );
            }
        }
        archive.into_inner().and_then(|encoder| encoder.finish())?;
    }
    Ok(archive_data)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::persistence::CopiedSessionFile;
    use echo_agent_chat_proxy_types::PromptMetadata;
    fn bare_prompt_metadata() -> PromptMetadata {
        PromptMetadata::new(PromptMetadataParams {
            schema_version: GCS_SCHEMA_VERSION.to_string(),
            session_id: "test-session".into(),
            turn_number: 1,
            request_id: "req-001".into(),
            turn_started_at: "2026-01-01T00:00:00Z".into(),
            model: "echo-agent-3".into(),
            host_os: "linux".into(),
            host_arch: "x86_64".into(),
            prompt_has_image: Some(false),
            prompt_was_truncated: Some(false),
            ..Default::default()
        })
    }
    #[tokio::test]
    async fn fill_git_fields_skips_when_both_present() {
        let mut meta = bare_prompt_metadata();
        meta.repo_root = Some("/repo".into());
        meta.remote_url = Some("git@github.com:org/repo.git".into());
        fill_git_fields(&mut meta, "/nonexistent/path").await;
        assert_eq!(meta.workspace_type.as_deref(), Some("git"));
        assert_eq!(meta.repo_root.as_deref(), Some("/repo"));
        assert_eq!(
            meta.remote_url.as_deref(),
            Some("git@github.com:org/repo.git")
        );
    }
    #[tokio::test]
    async fn fill_git_fields_preserves_existing_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(tmp.path()).unwrap();
        repo.remote("origin", "https://github.com/test/repo.git")
            .unwrap();
        let expected_remote = repo
            .find_remote("origin")
            .unwrap()
            .url()
            .map(strip_url_credentials)
            .expect("origin remote must have a url");
        let mut meta = bare_prompt_metadata();
        meta.repo_root = Some("/custom/root".into());
        fill_git_fields(&mut meta, tmp.path().to_str().unwrap()).await;
        assert_eq!(
            meta.repo_root.as_deref(),
            Some("/custom/root"),
            "pre-set repo_root must not be overwritten"
        );
        assert_eq!(
            meta.remote_url.as_deref(),
            Some(expected_remote.as_str()),
            "missing remote_url should be filled"
        );
        assert_eq!(meta.workspace_type.as_deref(), Some("git"));
    }
    #[tokio::test]
    async fn fill_git_fields_sets_git_when_both_preset() {
        let mut meta = bare_prompt_metadata();
        meta.repo_root = Some("/some/repo".to_owned());
        meta.remote_url = Some("https://github.com/org/repo".to_owned());
        fill_git_fields(&mut meta, "/whatever").await;
        assert_eq!(meta.workspace_type.as_deref(), Some("git"));
    }
    #[tokio::test]
    async fn resolve_git_repo_info_populates_from_git_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(tmp.path()).unwrap();
        repo.remote("origin", "https://github.com/test/repo.git")
            .unwrap();
        let expected_remote = repo
            .find_remote("origin")
            .unwrap()
            .url()
            .map(strip_url_credentials)
            .expect("origin remote must have a url");
        let (repo_root, remote_url) = resolve_git_repo_info(tmp.path().to_str().unwrap()).await;
        assert!(repo_root.is_some(), "repo_root should be populated");
        assert_eq!(remote_url.as_deref(), Some(expected_remote.as_str()));
    }
    #[tokio::test]
    async fn resolve_git_repo_info_none_outside_git_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let (repo_root, remote_url) = resolve_git_repo_info(tmp.path().to_str().unwrap()).await;
        assert!(repo_root.is_none());
        assert!(remote_url.is_none());
    }
    #[test]
    fn strip_url_credentials_removes_token() {
        let url_with_token = "https://x-access-token:ghs_secrettoken123@github.com/org/repo.git";
        assert_eq!(
            strip_url_credentials(url_with_token),
            "https://github.com/org/repo.git"
        );
    }
    #[test]
    fn strip_url_credentials_preserves_clean_https_url() {
        let clean_url = "https://github.com/test/repo.git";
        assert_eq!(strip_url_credentials(clean_url), clean_url);
    }
    #[test]
    fn strip_url_credentials_preserves_ssh_url() {
        let ssh_url = "git@github.com:org/repo.git";
        assert_eq!(strip_url_credentials(ssh_url), ssh_url);
    }
    #[test]
    fn strip_url_credentials_removes_username_password() {
        let url_with_creds = "https://user:password@gitlab.com/group/project.git";
        assert_eq!(
            strip_url_credentials(url_with_creds),
            "https://gitlab.com/group/project.git"
        );
    }
    #[test]
    fn dynamic_resolver_refreshes_proxy_token() {
        use crate::auth::{EchoAgentAuth, EchoAgentComConfig};
        use crate::session::repo_changes::UploadMethod;
        use chrono::{Duration, Utc};
        use std::collections::BTreeMap;
        let dir = tempfile::tempdir().unwrap();
        let echo_agent_com_config = EchoAgentComConfig::default();
        let scope = echo_agent_com_config.auth_scope();
        let initial_auth = EchoAgentAuth {
            key: "initial-token".into(),
            ..EchoAgentAuth::test_default()
        };
        let mut store = BTreeMap::new();
        store.insert(scope.clone(), initial_auth);
        let auth_json = serde_json::to_string_pretty(&store).unwrap();
        std::fs::write(dir.path().join("auth.json"), &auth_json).unwrap();
        let auth_manager = Arc::new(crate::auth::AuthManager::new(
            dir.path(),
            echo_agent_com_config.clone(),
        ));
        let base_config = TraceExportConfig {
            bucket_url: None,
            service_account_key: None,
            prefix_dir: None,
            gcs_prefix: Some("session/turn_0".into()),
            absolute_paths: false,
            archive_name_override: None,
            upload_method: UploadMethod::Proxy {
                proxy_base_url: "https://proxy.example.com".into(),
                user_token: "stale-token".into(),
                deployment_key: None,
                alpha_test_key: None,
            },
        };
        let resolver = DynamicResolver {
            auth_manager: auth_manager.clone(),
            base_config,
        };
        let provider = resolver
            .proxy_credentials()
            .expect("proxy_credentials should be Some for Proxy upload_method");
        assert_eq!(
            provider.snapshot().token.as_deref(),
            Some("initial-token"),
            "snapshot should reflect AuthManager.current(), not the stale base_config token"
        );
        let refreshed_auth = EchoAgentAuth {
            key: "refreshed-token".into(),
            expires_at: Some(Utc::now() + Duration::hours(1)),
            ..EchoAgentAuth::test_default()
        };
        store.insert(scope, refreshed_auth);
        let auth_json = serde_json::to_string_pretty(&store).unwrap();
        std::fs::write(dir.path().join("auth.json"), &auth_json).unwrap();
        auth_manager.force_reload_from_disk();
        assert_eq!(
            provider.snapshot().token.as_deref(),
            Some("refreshed-token")
        );
        let config = resolver.resolve();
        assert_eq!(config.gcs_prefix.as_deref(), Some("session/turn_0"));
        match &config.upload_method {
            UploadMethod::Proxy { proxy_base_url, .. } => {
                assert_eq!(proxy_base_url, "https://proxy.example.com");
            }
            _ => unreachable!(),
        }
    }
    #[test]
    fn dynamic_resolver_rereads_disk_on_expired_token() {
        use crate::auth::{EchoAgentAuth, EchoAgentComConfig};
        use crate::session::repo_changes::UploadMethod;
        use chrono::{Duration, Utc};
        use std::collections::BTreeMap;
        let dir = tempfile::tempdir().unwrap();
        let echo_agent_com_config = EchoAgentComConfig::default();
        let scope = echo_agent_com_config.auth_scope();
        let expired_auth = EchoAgentAuth {
            key: "expired-token".into(),
            expires_at: Some(Utc::now() - Duration::hours(1)),
            ..EchoAgentAuth::test_default()
        };
        let mut store = BTreeMap::new();
        store.insert(scope.clone(), expired_auth);
        let auth_json = serde_json::to_string_pretty(&store).unwrap();
        std::fs::write(dir.path().join("auth.json"), &auth_json).unwrap();
        let auth_manager = Arc::new(crate::auth::AuthManager::new(
            dir.path(),
            echo_agent_com_config.clone(),
        ));
        assert!(auth_manager.current().is_none());
        let resolver = DynamicResolver {
            auth_manager: auth_manager.clone(),
            base_config: TraceExportConfig {
                bucket_url: None,
                service_account_key: None,
                prefix_dir: None,
                gcs_prefix: None,
                absolute_paths: false,
                archive_name_override: None,
                upload_method: UploadMethod::Proxy {
                    proxy_base_url: "https://proxy.example.com".into(),
                    user_token: "stale-base-token".into(),
                    deployment_key: None,
                    alpha_test_key: None,
                },
            },
        };
        let fresh_auth = EchoAgentAuth {
            key: "fresh-from-chat-flow".into(),
            expires_at: Some(Utc::now() + Duration::hours(1)),
            ..EchoAgentAuth::test_default()
        };
        store.insert(scope, fresh_auth);
        let auth_json = serde_json::to_string_pretty(&store).unwrap();
        std::fs::write(dir.path().join("auth.json"), &auth_json).unwrap();
        auth_manager.force_reload_from_disk();
        let provider = resolver
            .proxy_credentials()
            .expect("proxy_credentials should be Some for Proxy upload_method");
        assert_eq!(
            provider.snapshot().token.as_deref(),
            Some("fresh-from-chat-flow"),
            "snapshot should pick up disk-refreshed token, not stale base_config"
        );
    }
    /// When both memory and disk tokens are expired and no refresher is
    /// configured, `resolve_async()` falls back gracefully: `get_valid_token()`
    /// returns an error and the resolver keeps the stale `base_config` token.
    /// This verifies the error path doesn't panic.
    #[tokio::test]
    async fn resolve_async_falls_back_when_no_refresher() {
        use crate::auth::{EchoAgentAuth, EchoAgentComConfig};
        use crate::session::repo_changes::UploadMethod;
        use chrono::{Duration, Utc};
        use std::collections::BTreeMap;
        let dir = tempfile::tempdir().unwrap();
        let echo_agent_com_config = EchoAgentComConfig::default();
        let scope = echo_agent_com_config.auth_scope();
        let expired_auth = EchoAgentAuth {
            key: "expired-on-disk".into(),
            expires_at: Some(Utc::now() - Duration::hours(1)),
            ..EchoAgentAuth::test_default()
        };
        let mut store = BTreeMap::new();
        store.insert(scope, expired_auth);
        let auth_json = serde_json::to_string_pretty(&store).unwrap();
        std::fs::write(dir.path().join("auth.json"), &auth_json).unwrap();
        let auth_manager = Arc::new(crate::auth::AuthManager::new(
            dir.path(),
            echo_agent_com_config,
        ));
        let resolver = DynamicResolver {
            auth_manager,
            base_config: TraceExportConfig {
                bucket_url: None,
                service_account_key: None,
                prefix_dir: None,
                gcs_prefix: None,
                absolute_paths: false,
                archive_name_override: None,
                upload_method: UploadMethod::Proxy {
                    proxy_base_url: "https://proxy.example.com".into(),
                    user_token: "stale-base-token".into(),
                    deployment_key: None,
                    alpha_test_key: None,
                },
            },
        };
        let config = resolver.resolve_async().await;
        match &config.upload_method {
            UploadMethod::Proxy { user_token, .. } => {
                assert_eq!(user_token, "stale-base-token");
            }
            other => panic!("expected Proxy, got {:?}", other),
        }
    }
    /// `resolve_async()` picks up a fresh token from disk when the in-memory
    /// token is expired and a valid one exists on disk (written by another flow).
    #[tokio::test]
    async fn resolve_async_picks_up_disk_refreshed_token() {
        use crate::auth::{EchoAgentAuth, EchoAgentComConfig};
        use crate::session::repo_changes::UploadMethod;
        use chrono::{Duration, Utc};
        use std::collections::BTreeMap;
        let dir = tempfile::tempdir().unwrap();
        let echo_agent_com_config = EchoAgentComConfig::default();
        let scope = echo_agent_com_config.auth_scope();
        let valid_auth = EchoAgentAuth {
            key: "fresh-disk-token".into(),
            expires_at: Some(Utc::now() + Duration::hours(1)),
            ..EchoAgentAuth::test_default()
        };
        let mut store = BTreeMap::new();
        store.insert(scope, valid_auth);
        let auth_json = serde_json::to_string_pretty(&store).unwrap();
        std::fs::write(dir.path().join("auth.json"), &auth_json).unwrap();
        let auth_manager = Arc::new(crate::auth::AuthManager::new(
            dir.path(),
            echo_agent_com_config,
        ));
        let resolver = DynamicResolver {
            auth_manager,
            base_config: TraceExportConfig {
                bucket_url: None,
                service_account_key: None,
                prefix_dir: None,
                gcs_prefix: None,
                absolute_paths: false,
                archive_name_override: None,
                upload_method: UploadMethod::Proxy {
                    proxy_base_url: "https://proxy.example.com".into(),
                    user_token: "stale-base-token".into(),
                    deployment_key: None,
                    alpha_test_key: None,
                },
            },
        };
        let config = resolver.resolve_async().await;
        match &config.upload_method {
            UploadMethod::Proxy { user_token, .. } => {
                assert_eq!(
                    user_token, "fresh-disk-token",
                    "resolve_async should use get_valid_token() to pick up disk token"
                );
            }
            other => panic!("expected Proxy, got {:?}", other),
        }
    }
    /// `resolve_async()` drives `refresh_chain` when the in-memory OIDC
    /// token is expired and no fresh disk token exists. The refresher fires
    /// and the resolved config carries the fresh token — not the stale
    /// `base_config` snapshot.
    #[tokio::test]
    async fn resolve_async_drives_refresh_chain_when_token_expired() {
        use crate::auth::{EchoAgentAuth, EchoAgentComConfig};
        use crate::session::repo_changes::UploadMethod;
        use chrono::{Duration, Utc};
        use std::collections::BTreeMap;
        let dir = tempfile::tempdir().unwrap();
        let echo_agent_com_config = EchoAgentComConfig::default();
        let scope = echo_agent_com_config.auth_scope();
        let expired_auth = EchoAgentAuth {
            key: "expired-oidc".into(),
            refresh_token: Some("rt-old".into()),
            expires_at: Some(Utc::now() - Duration::hours(1)),
            ..EchoAgentAuth::test_default()
        };
        let mut store = BTreeMap::new();
        store.insert(scope, expired_auth);
        let auth_json = serde_json::to_string_pretty(&store).unwrap();
        std::fs::write(dir.path().join("auth.json"), &auth_json).unwrap();
        let auth_manager = Arc::new(crate::auth::AuthManager::new(
            dir.path(),
            echo_agent_com_config,
        ));
        struct FreshRefresher;
        #[async_trait::async_trait]
        impl crate::auth::refresh::TokenRefresher for FreshRefresher {
            async fn refresh(
                &self,
                _r: crate::auth::manager::RefreshReason,
            ) -> crate::auth::refresh::RefreshOutcome {
                crate::auth::refresh::RefreshOutcome::Success(Box::new(
                    crate::auth::EchoAgentAuth {
                        key: "refresher-fresh-token".into(),
                        expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
                        refresh_token: Some("rt-new".into()),
                        ..crate::auth::EchoAgentAuth::test_default()
                    },
                ))
            }
        }
        auth_manager.set_refresher(Arc::new(FreshRefresher));
        let resolver = DynamicResolver {
            auth_manager,
            base_config: TraceExportConfig {
                bucket_url: None,
                service_account_key: None,
                prefix_dir: None,
                gcs_prefix: None,
                absolute_paths: false,
                archive_name_override: None,
                upload_method: UploadMethod::Proxy {
                    proxy_base_url: "https://proxy.example.com".into(),
                    user_token: "stale-base-token".into(),
                    deployment_key: None,
                    alpha_test_key: None,
                },
            },
        };
        let config = resolver.resolve_async().await;
        match &config.upload_method {
            UploadMethod::Proxy { user_token, .. } => {
                assert_eq!(
                    user_token, "refresher-fresh-token",
                    "resolve_async must use token from refresh_chain, not stale base_config"
                );
            }
            other => panic!("expected Proxy, got {:?}", other),
        }
    }
    /// Proactive refresh keeps the cache hot so `resolve_async` on the
    /// trace upload path is a cache hit — the refresher fires once
    /// (proactive), then `resolve_async` picks up the cached token
    /// without calling the refresher again.
    #[tokio::test]
    async fn proactive_refresh_makes_trace_resolve_a_cache_hit() {
        use crate::auth::{EchoAgentAuth, EchoAgentComConfig};
        use crate::session::repo_changes::UploadMethod;
        use chrono::{Duration, Utc};
        let dir = tempfile::tempdir().unwrap();
        let echo_agent_com_config = EchoAgentComConfig::default();
        let auth_manager = Arc::new(crate::auth::AuthManager::new(
            dir.path(),
            echo_agent_com_config,
        ));
        auth_manager.hot_swap(EchoAgentAuth {
            key: "expired-oidc".into(),
            refresh_token: Some("rt".into()),
            expires_at: Some(Utc::now() - Duration::hours(1)),
            ..EchoAgentAuth::test_default()
        });
        let call_count = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let cc = call_count.clone();
        struct Counting(Arc<std::sync::atomic::AtomicU32>);
        #[async_trait::async_trait]
        impl crate::auth::refresh::TokenRefresher for Counting {
            async fn refresh(
                &self,
                _: crate::auth::manager::RefreshReason,
            ) -> crate::auth::refresh::RefreshOutcome {
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                crate::auth::refresh::RefreshOutcome::Success(Box::new(EchoAgentAuth {
                    key: "proactive-fresh".into(),
                    expires_at: Some(chrono::Utc::now() + Duration::hours(1)),
                    refresh_token: Some("rt-new".into()),
                    ..EchoAgentAuth::test_default()
                }))
            }
        }
        auth_manager.set_refresher(Arc::new(Counting(cc)));
        let cancel = tokio_util::sync::CancellationToken::new();
        auth_manager.start_proactive_refresh(cancel.clone());
        tokio::time::sleep(
            crate::auth::manager::PROACTIVE_MIN_SLEEP + std::time::Duration::from_millis(1000),
        )
        .await;
        assert!(call_count.load(std::sync::atomic::Ordering::SeqCst) >= 1);
        let count_after_proactive = call_count.load(std::sync::atomic::Ordering::SeqCst);
        let resolver = DynamicResolver {
            auth_manager,
            base_config: TraceExportConfig {
                bucket_url: None,
                service_account_key: None,
                prefix_dir: None,
                gcs_prefix: None,
                absolute_paths: false,
                archive_name_override: None,
                upload_method: UploadMethod::Proxy {
                    proxy_base_url: "https://proxy.example.com".into(),
                    user_token: "stale".into(),
                    deployment_key: None,
                    alpha_test_key: None,
                },
            },
        };
        let config = resolver.resolve_async().await;
        match &config.upload_method {
            UploadMethod::Proxy { user_token, .. } => {
                assert_eq!(
                    user_token, "proactive-fresh",
                    "resolve_async must pick up the proactively-refreshed token"
                );
            }
            other => panic!("expected Proxy, got {:?}", other),
        }
        assert_eq!(
            call_count.load(std::sync::atomic::Ordering::SeqCst),
            count_after_proactive,
            "resolve_async must NOT call the refresher again (cache hit)"
        );
        cancel.cancel();
    }
    #[test]
    fn dynamic_resolver_preserves_token_when_auth_unavailable() {
        use crate::session::repo_changes::UploadMethod;
        let dir = tempfile::tempdir().unwrap();
        let echo_agent_com_config = crate::auth::EchoAgentComConfig::default();
        let auth_manager = Arc::new(crate::auth::AuthManager::new(
            dir.path(),
            echo_agent_com_config,
        ));
        let base_config = TraceExportConfig {
            bucket_url: None,
            service_account_key: None,
            prefix_dir: None,
            gcs_prefix: None,
            absolute_paths: false,
            archive_name_override: None,
            upload_method: UploadMethod::Proxy {
                proxy_base_url: "https://proxy.example.com".into(),
                user_token: "original-token".into(),
                deployment_key: None,
                alpha_test_key: None,
            },
        };
        let resolver = DynamicResolver {
            auth_manager,
            base_config,
        };
        let config = resolver.resolve();
        match &config.upload_method {
            UploadMethod::Proxy { user_token, .. } => {
                assert_eq!(user_token, "original-token");
            }
            other => panic!("expected Proxy, got {:?}", other),
        }
    }
    #[test]
    fn dynamic_resolver_noop_for_direct_mode() {
        use crate::auth::EchoAgentAuth;
        use crate::session::repo_changes::UploadMethod;
        use std::collections::BTreeMap;
        let dir = tempfile::tempdir().unwrap();
        let echo_agent_com_config = crate::auth::EchoAgentComConfig::default();
        let scope = echo_agent_com_config.auth_scope();
        let auth = EchoAgentAuth {
            key: "some-token".into(),
            ..EchoAgentAuth::test_default()
        };
        let mut store = BTreeMap::new();
        store.insert(scope, auth);
        let auth_json = serde_json::to_string_pretty(&store).unwrap();
        std::fs::write(dir.path().join("auth.json"), &auth_json).unwrap();
        let auth_manager = Arc::new(crate::auth::AuthManager::new(
            dir.path(),
            echo_agent_com_config,
        ));
        let base_config = TraceExportConfig {
            bucket_url: Some("gs://bucket".into()),
            service_account_key: Some("sa-key".into()),
            prefix_dir: None,
            gcs_prefix: None,
            absolute_paths: false,
            archive_name_override: None,
            upload_method: UploadMethod::Direct {
                service_account_key: Some("sa-key".into()),
            },
        };
        let resolver = DynamicResolver {
            auth_manager,
            base_config,
        };
        let config = resolver.resolve();
        match &config.upload_method {
            UploadMethod::Direct {
                service_account_key,
            } => {
                assert_eq!(service_account_key.as_deref(), Some("sa-key"));
            }
            other => panic!("expected Direct, got {:?}", other),
        }
    }
    /// `DynamicResolver` must supply `proxy_credentials` and
    /// `proxy_attribution` so the queue worker's per-attempt
    /// `StorageClient` gets a refresh-aware credential provider AND emits
    /// `auth_401_attribution` on 401. Without these, the worker falls back
    /// to the static `user_token` snapshot baked into `TraceExportConfig`
    /// and emits no attribution -- the exact gap in production that this
    /// PR fixes.
    #[test]
    fn dynamic_resolver_supplies_proxy_credentials_and_attribution() {
        use crate::session::repo_changes::UploadMethod;
        use echo_agent_file_utils::queue::TraceExportSource;
        let dir = tempfile::tempdir().unwrap();
        let auth_manager = Arc::new(crate::auth::AuthManager::new(
            dir.path(),
            crate::auth::EchoAgentComConfig::default(),
        ));
        let base_config = TraceExportConfig {
            bucket_url: None,
            service_account_key: None,
            prefix_dir: None,
            gcs_prefix: None,
            absolute_paths: false,
            archive_name_override: None,
            upload_method: UploadMethod::Proxy {
                proxy_base_url: "https://proxy.example.com".into(),
                user_token: "snapshot".into(),
                deployment_key: None,
                alpha_test_key: None,
            },
        };
        let resolver = DynamicResolver {
            auth_manager,
            base_config,
        };
        assert!(
            resolver.proxy_credentials().is_some(),
            "expected refresh-aware credential provider"
        );
        assert!(
            resolver.proxy_attribution().is_some(),
            "expected 401-attribution callback"
        );
        assert!(
            resolver.proxy_http_client().is_some(),
            "expected tuned HTTP client"
        );
    }
    /// A rotation that lands between park wait slices is invisible to the
    /// notifier; the bearer comparison must wake the parked item immediately.
    #[tokio::test]
    async fn dynamic_resolver_auth_recovery_wakes_on_already_rotated_token() {
        use crate::session::repo_changes::UploadMethod;
        use echo_agent_file_utils::queue::TraceExportSource;
        let dir = tempfile::tempdir().unwrap();
        let auth_manager = Arc::new(crate::auth::AuthManager::new(
            dir.path(),
            crate::auth::EchoAgentComConfig::default(),
        ));
        auth_manager.hot_swap(crate::auth::EchoAgentAuth {
            key: "fresh-token".into(),
            expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
            ..crate::auth::EchoAgentAuth::test_default()
        });
        let resolver = DynamicResolver {
            auth_manager,
            base_config: TraceExportConfig {
                bucket_url: None,
                service_account_key: None,
                prefix_dir: None,
                gcs_prefix: None,
                absolute_paths: false,
                archive_name_override: None,
                upload_method: UploadMethod::Proxy {
                    proxy_base_url: "https://proxy.example.com".into(),
                    user_token: "snapshot".into(),
                    deployment_key: None,
                    alpha_test_key: None,
                },
            },
        };
        let wake = resolver
            .wait_for_auth_recovery(Some("stale-token"), std::time::Duration::from_secs(30))
            .expect("recovery hook available");
        assert!(wake.await, "already-rotated token wakes without waiting");
        let wait = resolver
            .wait_for_auth_recovery(Some("fresh-token"), std::time::Duration::from_millis(10))
            .expect("recovery hook available");
        assert!(!wait.await, "unchanged token falls through to the notifier");
    }
    /// With a deployment key on the wire, the session token in `AuthManager`
    /// always differs from `failed_bearer` — the wake comparison must use the
    /// deployment key (wire precedence) or parking becomes a hot retry loop.
    #[tokio::test]
    async fn dynamic_resolver_auth_recovery_ignores_session_token_for_deployment_key() {
        use crate::session::repo_changes::UploadMethod;
        use echo_agent_file_utils::queue::TraceExportSource;
        let dir = tempfile::tempdir().unwrap();
        let auth_manager = Arc::new(crate::auth::AuthManager::new(
            dir.path(),
            crate::auth::EchoAgentComConfig::default(),
        ));
        auth_manager.hot_swap(crate::auth::EchoAgentAuth {
            key: "session-token".into(),
            expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
            ..crate::auth::EchoAgentAuth::test_default()
        });
        let resolver = DynamicResolver {
            auth_manager,
            base_config: TraceExportConfig {
                bucket_url: None,
                service_account_key: None,
                prefix_dir: None,
                gcs_prefix: None,
                absolute_paths: false,
                archive_name_override: None,
                upload_method: UploadMethod::Proxy {
                    proxy_base_url: "https://proxy.example.com".into(),
                    user_token: "snapshot".into(),
                    deployment_key: Some("deployment-key".into()),
                    alpha_test_key: None,
                },
            },
        };
        let wait = resolver
            .wait_for_auth_recovery(Some("deployment-key"), std::time::Duration::from_millis(10))
            .expect("recovery hook available");
        assert!(
            !wait.await,
            "static deployment key never satisfies the immediate-wake check"
        );
    }
    #[tokio::test]
    async fn spawn_upload_queue_uses_dynamic_resolver_when_auth_manager_provided() {
        use crate::session::repo_changes::UploadMethod;
        let dir = tempfile::tempdir().unwrap();
        let echo_agent_com_config = crate::auth::EchoAgentComConfig::default();
        let auth_manager = Arc::new(crate::auth::AuthManager::new(
            dir.path(),
            echo_agent_com_config,
        ));
        let gcs_config = TraceExportConfig {
            bucket_url: None,
            service_account_key: None,
            prefix_dir: None,
            gcs_prefix: None,
            absolute_paths: false,
            archive_name_override: None,
            upload_method: UploadMethod::Proxy {
                proxy_base_url: "https://proxy.example.com".into(),
                user_token: "token".into(),
                deployment_key: None,
                alpha_test_key: None,
            },
        };
        let _queue = spawn_upload_queue(dir.path(), &gcs_config, Some("1.0.0"), auth_manager);
    }
    #[test]
    fn purge_stale_scratch_only_removes_scratch_tree() {
        let home = tempfile::tempdir().unwrap();
        let queue_dir = home.path().join("upload_queue");
        let scratch = queue_dir.join("scratch");
        std::fs::create_dir_all(scratch.join("sess")).unwrap();
        std::fs::write(queue_dir.join("durable_spill.tmp"), b"keep").unwrap();
        std::fs::write(scratch.join("sess").join("blob"), b"x").unwrap();
        assert!(purge_stale_upload_scratch_dir(&scratch).unwrap());
        assert!(!scratch.exists());
        assert!(queue_dir.join("durable_spill.tmp").exists());
        assert!(!purge_stale_upload_scratch_dir(&queue_dir).unwrap());
        assert!(!purge_stale_upload_scratch_dir(&scratch).unwrap());
    }
    fn copied_file(name: &str) -> CopiedSessionFile {
        CopiedSessionFile {
            name: name.to_string(),
            data: vec![],
        }
    }
    fn names(files: &[CopiedSessionFile]) -> Vec<&str> {
        files.iter().map(|f| f.name.as_str()).collect()
    }
    #[test]
    fn session_state_priority_files_sorted_first() {
        let mut files = vec![
            copied_file("call_001.jsonl"),
            copied_file("updates.jsonl"),
            copied_file("call_002.jsonl"),
            copied_file("events.jsonl"),
            copied_file("chat_history.jsonl"),
            copied_file("summary.json"),
        ];
        sort_session_files_by_priority(&mut files);
        assert_eq!(
            names(&files),
            vec![
                "summary.json",
                "chat_history.jsonl",
                "events.jsonl",
                "updates.jsonl",
                "call_001.jsonl",
                "call_002.jsonl"
            ]
        );
    }
    #[test]
    fn session_state_sort_stable_for_non_priority_files() {
        let mut files = vec![
            copied_file("z_call.jsonl"),
            copied_file("summary.json"),
            copied_file("a_call.jsonl"),
        ];
        sort_session_files_by_priority(&mut files);
        assert_eq!(
            names(&files),
            vec!["summary.json", "z_call.jsonl", "a_call.jsonl"]
        );
    }
    /// Gunzip + untar `archive`, returning `(entry_name, raw_bytes)` for every
    /// entry. Mirrors how the trace viewer extracts `chat_history.jsonl`.
    fn read_tar_gz_entries(archive: &[u8]) -> Vec<(String, Vec<u8>)> {
        use std::io::Read as _;
        let decoder = flate2::read::GzDecoder::new(archive);
        let mut tar = tar::Archive::new(decoder);
        let mut out = Vec::new();
        for entry in tar.entries().unwrap() {
            let mut entry = entry.unwrap();
            let name = entry.path().unwrap().to_string_lossy().into_owned();
            let mut data = Vec::new();
            entry.read_to_end(&mut data).unwrap();
            out.push((name, data));
        }
        out
    }
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(archive_build_fault)]
    async fn chat_history_session_state_empty_messages_yields_valid_empty_archive() {
        let archive = build_chat_history_session_state(&[]).await.unwrap();
        let entries = read_tar_gz_entries(&archive);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "chat_history.jsonl");
        assert!(entries[0].1.is_empty());
    }
    #[test]
    fn chat_history_jsonl_is_empty_when_feature_disabled() {
        use echo_agent_sampling_types::conversation::ConversationItem;
        let messages = vec![ConversationItem::user("ignored without the feature")];
        let jsonl = serialize_chat_history_jsonl(&messages).unwrap();
        assert!(jsonl.is_empty());
        assert_eq!(messages.len(), 1);
    }
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(archive_build_fault)]
    async fn chat_history_wrapper_ignores_messages_when_feature_disabled() {
        use echo_agent_sampling_types::conversation::ConversationItem;
        let messages = vec![ConversationItem::user("must not appear in the archive")];
        let archive = build_chat_history_session_state(&messages).await.unwrap();
        let entries = read_tar_gz_entries(&archive);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "chat_history.jsonl");
        assert!(entries[0].1.is_empty());
    }
    #[test]
    #[serial_test::serial(archive_build_fault)]
    fn chat_history_compress_round_trips_owned_jsonl() {
        let jsonl = b"{\"role\":\"user\",\"content\":\"hi\"}\n";
        let archive = compress_chat_history_archive(jsonl.to_vec()).unwrap();
        let entries = read_tar_gz_entries(&archive);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "chat_history.jsonl");
        assert_eq!(entries[0].1, jsonl);
    }
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(archive_build_fault)]
    async fn chat_history_spawn_blocking_panic_is_archive_failed() {
        let _fault = set_archive_build_fault(ArchiveBuildFault::Panic);
        let err = build_chat_history_session_state(&[])
            .await
            .expect_err("compress panic must surface as archive_failed");
        assert_eq!(err.reason, "archive_failed");
        let msg = format!("{:#}", err.error);
        assert!(
            msg.contains("spawn_blocking join failed"),
            "join error must keep cancel vs panic distinguishable: {msg}"
        );
    }
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(archive_build_fault)]
    async fn chat_history_spawn_blocking_io_error_is_archive_failed() {
        let _fault = set_archive_build_fault(ArchiveBuildFault::Io);
        let err = build_chat_history_session_state(&[])
            .await
            .expect_err("compress io error must surface as archive_failed");
        assert_eq!(err.reason, "archive_failed");
        assert!(format!("{:#}", err.error).contains("test-forced archive io failure"));
    }
    fn session_copy_with(
        files: Vec<(&str, &[u8])>,
    ) -> crate::session::persistence::SessionStateCopy {
        crate::session::persistence::SessionStateCopy {
            files: files
                .into_iter()
                .map(|(name, data)| CopiedSessionFile {
                    name: name.to_string(),
                    data: data.to_vec(),
                })
                .collect(),
        }
    }
    #[test]
    #[serial_test::serial(archive_build_fault)]
    fn session_state_archive_contains_sorted_files() {
        let session_copy = session_copy_with(vec![
            ("call_001.jsonl", b"call"),
            ("summary.json", b"sum"),
            ("chat_history.jsonl", b"hist"),
            ("events.jsonl", b"ev"),
        ]);
        let archive = build_session_state_archive(session_copy, "test-sid").unwrap();
        let entries = read_tar_gz_entries(&archive);
        let names: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "summary.json",
                "chat_history.jsonl",
                "events.jsonl",
                "call_001.jsonl"
            ]
        );
        assert_eq!(entries[0].1, b"sum");
        assert_eq!(entries[1].1, b"hist");
        assert_eq!(entries[2].1, b"ev");
        assert_eq!(entries[3].1, b"call");
    }
    #[test]
    #[serial_test::serial(archive_build_fault)]
    fn session_state_archive_empty_files_is_valid_tar_gz() {
        let archive = build_session_state_archive(session_copy_with(vec![]), "test-sid").unwrap();
        let entries = read_tar_gz_entries(&archive);
        assert!(entries.is_empty());
    }
    #[test]
    fn turn_result_metadata_serializes_token_breakdown() {
        let meta = TurnResultMetadata {
            schema_version: "v1.17",
            request_id: "req".into(),
            completed: true,
            stop_reason: None,
            total_tokens: Some(378),
            input_tokens: Some(141),
            cached_input_tokens: Some(128),
            output_tokens: Some(237),
            error: None,
            finished_at: "2026-05-21T00:00:00Z".into(),
            signals: None,
            turn_delta: None,
            resolved_model: None,
            subagents_spawned: vec![],
            start_prompt_mode: None,
            end_prompt_mode: None,
        };
        let v: serde_json::Value = serde_json::to_value(&meta).unwrap();
        assert_eq!(v["input_tokens"], 141);
        assert_eq!(v["cached_input_tokens"], 128);
        assert_eq!(v["output_tokens"], 237);
        assert_eq!(v["total_tokens"], 378);
    }
    #[test]
    fn turn_result_metadata_omits_unset_breakdown_fields() {
        let meta = TurnResultMetadata {
            schema_version: "v1.17",
            request_id: "req".into(),
            completed: false,
            stop_reason: None,
            total_tokens: None,
            input_tokens: None,
            cached_input_tokens: None,
            output_tokens: None,
            error: None,
            finished_at: "2026-05-21T00:00:00Z".into(),
            signals: None,
            turn_delta: None,
            resolved_model: None,
            subagents_spawned: vec![],
            start_prompt_mode: None,
            end_prompt_mode: None,
        };
        let v: serde_json::Value = serde_json::to_value(&meta).unwrap();
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("input_tokens"));
        assert!(!obj.contains_key("cached_input_tokens"));
        assert!(!obj.contains_key("output_tokens"));
    }
    #[test]
    fn sample_rss_bytes_returns_plausible_value() {
        let rss = crate::session::signals::sample_rss_bytes();
        #[cfg(unix)]
        {
            assert!(rss > 0, "expected non-zero RSS on Unix, got {rss}");
            assert!(
                rss < 10 * 1024 * 1024 * 1024,
                "RSS {rss} exceeds 10 GiB — likely a unit-scaling regression"
            );
        }
        #[cfg(not(unix))]
        assert_eq!(rss, 0);
    }
    #[test]
    fn classify_workspace_git_for_repo() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join(".git")).unwrap();
        assert_eq!(classify_workspace(&tmp.path().to_string_lossy()), "git");
    }
    #[test]
    fn classify_workspace_project_for_non_git_project_dir() {
        let Some(dir) = home_project_dir() else {
            return;
        };
        assert_eq!(classify_workspace(&dir.path().to_string_lossy()), "project");
    }
    #[test]
    fn classify_workspace_non_project_for_tmp() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            classify_workspace(&tmp.path().to_string_lossy()),
            "non_project"
        );
    }
    /// Project dir under $HOME so `is_project_dir` passes; None in sandboxes or git-repo homes.
    fn home_project_dir() -> Option<tempfile::TempDir> {
        let home = echo_agent_config::discovery_home_dir()?;
        if home.ancestors().any(|p| p.join(".git").exists()) {
            return None;
        }
        if !echo_agent_file_utils::workspace_classifier::is_project_dir(&home.join("probe")) {
            return None;
        }
        tempfile::tempdir_in(home).ok()
    }
    /// Customer-managed S3 failures stay below the ERROR alerting threshold,
    /// repeats within an episode drop to debug, and the `method` log field
    /// keeps the structured upload-method vocabulary.
    #[test]
    fn upload_failure_log_level_splits_on_backend_and_repeats() {
        use crate::session::repo_changes::UploadMethod;
        let s3 = UploadMethod::S3 {
            bucket: "bucket".into(),
            region: "region".into(),
            credentials_file: None,
            credentials_content: None,
            endpoint_url: None,
        };
        let proxy = UploadMethod::Proxy {
            proxy_base_url: "https://proxy.example/v1".into(),
            user_token: "token".into(),
            deployment_key: None,
            alpha_test_key: None,
        };
        let gcs = UploadMethod::Direct {
            service_account_key: None,
        };
        assert_eq!(
            upload_failure_log_level(&s3, 0),
            UploadFailureLogLevel::Warn
        );
        assert_eq!(
            upload_failure_log_level(&proxy, 0),
            UploadFailureLogLevel::Error
        );
        assert_eq!(
            upload_failure_log_level(&gcs, 0),
            UploadFailureLogLevel::Error
        );
        for method in [&s3, &proxy, &gcs] {
            assert_eq!(
                upload_failure_log_level(method, 1),
                UploadFailureLogLevel::Debug
            );
        }
        assert_eq!(upload_method_label(&s3), "direct_s3");
        assert_eq!(upload_method_label(&proxy), "proxy");
        assert_eq!(upload_method_label(&gcs), "direct_gcs");
    }
    /// The manifest may claim `enqueued` (queue-owned: flushable and
    /// sidecar-recoverable) only for these accept shapes; an inline fallback
    /// or a failed enqueue must take the awaited direct attempt so the
    /// recorded status is a real outcome.
    #[test]
    fn deferred_enqueue_durability_mapping() {
        assert!(enqueue_outcome_is_durable(&EnqueueOutcome::Enqueued));
        assert!(enqueue_outcome_is_durable(&EnqueueOutcome::Deduplicated));
        assert!(!enqueue_outcome_is_durable(
            &EnqueueOutcome::FellBackToInline
        ));
        assert!(!enqueue_outcome_is_durable(&EnqueueOutcome::Failed {
            reason: "upload queue worker is shut down".to_owned(),
        }));
        assert!(!enqueue_outcome_is_durable(&EnqueueOutcome::Skipped {
            reason: "collect_deadline".to_owned(),
        }));
    }
    /// A disabled verdict escalates exactly once over a prior recovery (pairs
    /// it re-enqueued must not outlive a no-collection verdict); a purge is
    /// never resurrected by a later enabled verdict.
    #[test]
    fn spill_reconcile_escalates_to_purge_but_never_resurrects() {
        use std::sync::atomic::AtomicU8;
        let state = AtomicU8::new(SPILL_NOT_RUN);
        assert!(
            claim_spill_reconcile(&state, true),
            "first enabled verdict recovers"
        );
        assert!(!claim_spill_reconcile(&state, true), "recovery runs once");
        assert!(
            claim_spill_reconcile(&state, false),
            "disabled verdict escalates to a purge"
        );
        assert!(!claim_spill_reconcile(&state, false), "the purge runs once");
        assert!(
            !claim_spill_reconcile(&state, true),
            "no recovery after a purge"
        );
        let state = AtomicU8::new(SPILL_NOT_RUN);
        assert!(
            claim_spill_reconcile(&state, false),
            "disabled-first purges immediately"
        );
        assert!(
            !claim_spill_reconcile(&state, true),
            "a purged state never recovers"
        );
        assert!(!claim_spill_reconcile(&state, false));
    }
    /// A Defer-timeout may claim `enqueued` only when the cancelled future was
    /// parked on queue confirmation; a timeout that cancelled a direct attempt
    /// queued nothing and must record the loss.
    #[test]
    fn confirm_timeout_is_enqueued_only_when_queue_owned() {
        use crate::upload::manifest::ArtifactResult;
        assert!(matches!(
            confirm_timeout_artifact_result(false),
            ArtifactResult::Enqueued
        ));
        assert!(matches!(
            confirm_timeout_artifact_result(true),
            ArtifactResult::Failed {
                reason: "direct_upload_timed_out",
                ..
            }
        ));
    }
}
