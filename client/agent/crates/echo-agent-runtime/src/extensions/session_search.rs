//! ACP extension handler for session search (`echo.agent/session/search`).
//!
//! Exposes session full-text search as an ACP extension method.
//! The client sends a query and receives ranked results across all
//! (or workspace-filtered) past sessions.
//!
//! ```text
//! JSON-RPC -> mvp_agent.ext_method()
//!          -> session_search::handle()
//!          -> storage::search::execute_search()
//!          -> search_fts::SessionSearchIndex (SQLite FTS5)
//! ```

use agent_client_protocol as acp;
use serde::{Deserialize, Serialize};

use crate::agent::MvpAgent;
use crate::session::storage::search::{SessionSearchRequest, SessionSearchResponse};
use crate::session::storage::search_fts::SessionSearchRow;

use super::ExtResult;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SearchSessionsRequest {
    /// The search query string.
    pub query: String,
    /// Optional workspace directory to scope results to.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Maximum number of results to return. Defaults to 20.
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Offset for pagination. Defaults to 0.
    #[serde(default)]
    pub offset: usize,
    /// Whether to include content snippets in results.
    #[serde(default)]
    pub include_content: bool,
}

fn default_limit() -> usize {
    20
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SearchSessionsResponse {
    pub results: Vec<SearchSessionHit>,
    pub next_offset: Option<usize>,
    pub total_estimate: Option<usize>,
    /// True when the FTS5 index is still being bootstrapped.
    pub bootstrapping: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchSessionHit {
    pub session_id: String,
    pub cwd: String,
    /// Session title/summary for display
    pub summary: String,
    /// RFC 3339 formatted updated_at
    pub updated_at: String,
    pub score: f32,
    pub matched_fields: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
}

/// Route `echo.agent/session/search` extension method calls.
pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        "echo.agent/session/search" => {
            let req: SearchSessionsRequest = super::parse_params(args)?;
            let internal_req = SessionSearchRequest {
                query: req.query,
                cwd: req.cwd,
                limit: req.limit,
                offset: req.offset,
                include_content: req.include_content,
            };

            let root_dir = crate::util::echo_agent_home::echo_agent_home();
            let result = crate::session::storage::search::execute_search(
                agent.search_index(),
                &root_dir,
                &internal_req,
            )
            .await
            .map(to_response)
            .map_err(|e| anyhow::anyhow!(e));

            super::to_ext_response(result)
        }
        _ => Err(acp::Error::method_not_found()),
    }
}

/// Convert the internal response to the ACP-facing response.
fn to_response(resp: SessionSearchResponse) -> SearchSessionsResponse {
    SearchSessionsResponse {
        results: resp
            .results
            .into_iter()
            .map(|row: SessionSearchRow| SearchSessionHit {
                session_id: row.session_id,
                cwd: row.cwd,
                summary: row.title,
                updated_at: chrono::DateTime::from_timestamp(row.updated_at_unix, 0)
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_default(),
                score: row.score,
                matched_fields: row.matched_fields,
                snippet: row.snippet,
            })
            .collect(),
        next_offset: resp.next_offset,
        total_estimate: resp.total_estimate,
        bootstrapping: resp.bootstrapping,
    }
}
