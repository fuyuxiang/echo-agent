use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterSessionRequest {
    pub session_id: String,
    pub cwd: String,
    /// Ignored; server derives this from `session_id`. Kept for wire-compat.
    #[serde(default)]
    pub gcs_trace_prefix: Option<String>,
    #[serde(default)]
    pub model_id: Option<String>,
    #[serde(default)]
    pub repo_remote_url: Option<String>,
    #[serde(default)]
    pub repo_branch: Option<String>,
    #[serde(default)]
    pub repo_head_at_start: Option<String>,
    /// Ignored; server uses its own bucket constant. Kept for wire-compat.
    #[serde(default)]
    pub gcs_bucket: Option<String>,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub parent_session_id: Option<String>,
    /// Opaque per-machine device id (`deviceId` on the wire). Sent by the CLI
    /// at register; optional for backward-compat with older clients.
    #[serde(default)]
    pub device_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSessionRequest {
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub first_prompt: Option<String>,
    #[serde(default)]
    pub last_turn_number: Option<i32>,
    #[serde(default)]
    pub repo_head_at_end: Option<String>,
    /// Latest turn whose restore artifacts are confirmed durable.
    /// `None` = leave unchanged.  Written separately from `last_turn_number`
    /// once session-state upload is confirmed.
    #[serde(default)]
    pub restorable_turn_number: Option<i32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchSessionsQuery {
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    20
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionReplicaResponse {
    pub session_id: String,
    pub summary: String,
    pub first_prompt: Option<String>,
    pub model_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub last_turn_number: i32,
    /// See `UpdateSessionRequest.restorable_turn_number`.  Optional in the wire
    /// type so newer CLI builds can parse responses from older servers gracefully.
    pub restorable_turn_number: Option<i32>,
    pub cwd: String,
    pub repo_remote_url: Option<String>,
    pub repo_branch: Option<String>,
    pub repo_head_at_start: Option<String>,
    pub repo_head_at_end: Option<String>,
    pub gcs_trace_prefix: String,
    pub gcs_bucket: String,
    pub hostname: Option<String>,
    pub parent_session_id: Option<String>,
    pub status: String,
    pub last_active_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchSessionsResponse {
    pub sessions: Vec<SessionReplicaResponse>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadSessionQuery {
    pub file: String,
    #[serde(default)]
    pub turn: Option<i32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadSessionResponse {
    pub download_url: String,
    pub expires_in_seconds: u64,
    pub file: String,
    pub turn: i32,
}
