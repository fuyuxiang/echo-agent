//! `echo.agent/prompt_history` extension handler.
//!
//! Returns the user-prompt history for a given cwd. Three paths:
//! - **fast path** (no ids): reads the per-CWD `prompt_history.jsonl` file
//!   directly so Ctrl+R is instant; returns all sessions, most-recent-first.
//! - **fast scoped path** (`filter_session_id`): the same file filtered to a
//!   single session, most-recent-first. This is what the pager's up-arrow /
//!   Ctrl+R overlay uses to scope history to the current session.
//! - **slow path** (`session_id`): rebuilds prompts from session storage in
//!   chronological order with stable per-session indices. Not used by the
//!   pager; retained for clients that request session-scoped history this way.

use agent_client_protocol as acp;
use serde::{Deserialize, Serialize};

use super::{ExtResult, parse_params, to_raw_response};
use crate::agent::MvpAgent;
use crate::session::persistence::list_summaries;
use crate::session::prompt_history;
use crate::session::storage::StorageAdapter;
use crate::session::storage::jsonl::JsonlStorageAdapter;
use crate::timed;

#[derive(Deserialize)]
struct PromptHistoryRequest {
    cwd: String,
    /// Optional session ID to filter to a specific session. Routes to the
    /// session-storage "slow path" (chronological order, stable per-session
    /// indices). Not used by the pager — see `filter_session_id`.
    #[serde(default)]
    session_id: Option<String>,
    /// Optional session ID to restrict the **fast** per-CWD history file to a
    /// single session, keeping most-recent-first ordering. Used by the pager's
    /// up-arrow / Ctrl+R overlay to scope history to the current session.
    /// Takes precedence over `session_id` when both are set.
    #[serde(default)]
    filter_session_id: Option<String>,
}

#[derive(Serialize)]
struct PromptHistoryResponse {
    prompts: Vec<String>,
}

#[tracing::instrument(skip_all, fields(method = %args.method))]
pub async fn handle(_agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        "echo.agent/prompt_history" => handle_prompt_history(args).await,
        _ => Err(acp::Error::method_not_found()),
    }
}

async fn handle_prompt_history(args: &acp::ExtRequest) -> ExtResult {
    let request: PromptHistoryRequest = parse_params(args)?;

    // If session_id is specified, use slow path (needed for rewind feature).
    // Use timed!(try: ...) so we still log timing even when returning early on error.
    let all_prompts = timed!(try: "prompt_history: load prompts", async {
        tracing::debug!(
            "Loading prompt history for cwd: {}, session_id: {:?}, filter_session_id: {:?}",
            request.cwd,
            request.session_id,
            request.filter_session_id
        );

        if let Some(filter_session_id) = request.filter_session_id.as_deref() {
            // Fast path, scoped to a single session: filter the per-CWD history
            // file by session id, preserving most-recent-first ordering.
            prompt_history::load_prompts_for_session_async(
                request.cwd.clone(),
                filter_session_id.to_string(),
            )
            .await
                .map_err(|e| {
                    acp::Error::internal_error()
                        .data(format!("failed to load prompt history: {e}"))
                })
        } else if request.session_id.is_some() {
            // Slow path: load from session storage for per-session queries
            load_session_prompts(&request.cwd, request.session_id.as_deref()).await
        } else {
            // Fast path: use per-CWD prompt history file
            prompt_history::load_prompts_async(request.cwd.clone())
                .await
                .map_err(|e| {
                    acp::Error::internal_error()
                        .data(format!("failed to load prompt history: {e}"))
                })
        }
    })?;

    tracing::debug!(
        "Found {} prompts for cwd {}",
        all_prompts.len(),
        request.cwd
    );

    to_raw_response(&PromptHistoryResponse {
        prompts: all_prompts,
    })
}

/// Load prompts using the slow path (session-based loading).
/// Used when `session_id` is specified: rebuilds prompts from session storage
/// in chronological order with stable per-session indices.
async fn load_session_prompts(
    cwd: &str,
    session_id: Option<&str>,
) -> Result<Vec<String>, acp::Error> {
    // Load session summaries - either all for the cwd or just the specific session
    let mut summaries = list_summaries(Some(cwd)).await.map_err(|e| {
        acp::Error::internal_error().data(format!("failed to load session history: {e}"))
    })?;

    // Filter to specific session if session_id is provided
    if let Some(target_session_id) = session_id {
        summaries.retain(|s| s.info.id.0.as_ref() == target_session_id);
    }

    // Sort sessions by updated_at ascending (oldest first)
    // so that when we reverse the final list, most recent prompts are first
    summaries.sort_by(|a, b| a.updated_at.cmp(&b.updated_at));

    // Load only user prompts using the optimized method (avoids loading full session data)
    let root_dir = crate::util::echo_agent_home::echo_agent_home();
    let storage = JsonlStorageAdapter::with_root(root_dir);

    // Load prompts from sessions with bounded concurrency using stream
    // Using `buffered` (not `buffer_unordered`) to preserve session order
    use futures::stream::{self, StreamExt};

    // Limit concurrent file reads to avoid overwhelming the blocking thread pool
    const MAX_CONCURRENT_READS: usize = 32;

    let mut all_prompts: Vec<String> = stream::iter(summaries)
        .map(|summary| {
            let storage = storage.clone();
            async move {
                storage
                    .load_prompts_only(&summary.info)
                    .await
                    .unwrap_or_default()
            }
        })
        .buffered(MAX_CONCURRENT_READS)
        .flat_map(stream::iter)
        .collect()
        .await;

    // Deduplicate consecutive identical prompts
    all_prompts.dedup();

    // DON'T reverse when filtering to a single session - keep chronological
    // order so per-session prompt indices stay stable (0-indexed from the first
    // prompt). Only reverse when showing all sessions (history search, most
    // recent first).
    if session_id.is_none() {
        all_prompts.reverse();
    }

    Ok(all_prompts)
}
