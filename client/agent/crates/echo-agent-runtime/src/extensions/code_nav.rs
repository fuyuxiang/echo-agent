//! Code Navigation Extension Methods
//!
//! Provides go-to-definition, go-to-references, and symbol lookup functionality
//! using the echo-agent-codebase-graph index.
//!
//! ## Extension Methods
//!
//! | Method | Description |
//! |--------|-------------|
//! | `echo.agent/code/goto-definition` | Definition location(s) for symbol at position |
//! | `echo.agent/code/goto-references` | Reference location(s) for symbol at position |
//! | `echo.agent/code/find-definitions` | All definitions of a symbol by name |
//! | `echo.agent/code/find-references` | All references to a symbol by name |
//! | `echo.agent/code/status` | Indexing status |

use std::path::{Path, PathBuf};

use crate::agent::mvp_agent::{CodeNavEligibility, MvpAgent};
use agent_client_protocol as acp;
use serde::{Deserialize, Serialize};

/// Record a structured telemetry event at the end of a code-nav handler call.
///
/// This is called once per request with the method name, triggering session,
/// cwd, whether the index was newly spawned or reused, and total elapsed time.
/// These fields make it possible to:
///  - identify first-use latency (newly spawned + high elapsed_ms)
///  - identify reuse latency (reused + low elapsed_ms)
///  - attribute slowness to index startup vs query processing
fn log_code_nav_telemetry(
    method: &str,
    session_id: Option<&acp::SessionId>,
    cwd: &Path,
    was_newly_started: bool,
    elapsed_ms: u128,
) {
    tracing::info!(
        method,
        session_id = session_id.map(|s| s.0.as_ref()).unwrap_or(""),
        cwd = %cwd.display(),
        index_newly_started = was_newly_started,
        elapsed_ms,
        "code-nav request completed"
    );
}

type ExtResult = Result<acp::ExtResponse, acp::Error>;

// ========== Request Types ==========

/// Position-based query request (for goto-definition, goto-references).
/// Position parameters are 1-indexed (matching editor display).
///
/// **`sessionId` is required** for all code-nav requests.  Per-client
/// capability gating requires a valid session so eligibility is resolved
/// correctly in both simple and leader modes.  Requests without `sessionId`
/// receive `reason: sessionRequired` in the error response.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GotoRequest {
    /// Session ID — required for code navigation.
    pub session_id: Option<acp::SessionId>,
    /// Working directory (optional when session_id is provided).
    pub cwd: Option<String>,
    /// Relative path to the file within the cwd
    pub path: String,
    /// 1-indexed line number
    pub row: usize,
    /// 1-indexed column number
    pub column: usize,
}

/// Symbol name query request (for find-definitions, find-references).
///
/// **`sessionId` is required** — same contract as [`GotoRequest`].
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FindSymbolRequest {
    /// Session ID — required for code navigation.
    pub session_id: Option<acp::SessionId>,
    /// Working directory (optional when session_id is provided).
    pub cwd: Option<String>,
    /// Symbol name to search for
    pub symbol: String,
    /// Optional context file path for ranking results
    pub context_path: Option<String>,
}

/// Status request — check indexing status.
///
/// **`sessionId` is required** — same contract as [`GotoRequest`].
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StatusRequest {
    /// Session ID — required for code navigation.
    pub session_id: Option<acp::SessionId>,
    /// Working directory (optional when session_id is provided).
    pub cwd: Option<String>,
}

// ========== Response Types ==========

/// Response for goto-definition and goto-references queries.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeNavResponse {
    /// The symbol that was queried
    pub symbol: String,
    /// List of locations where the symbol was found
    pub locations: Vec<SymbolLocation>,
}

/// A symbol location in a file.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolLocation {
    /// Absolute path to the file
    pub path: String,
    /// 1-indexed line number
    pub line: usize,
    /// 1-indexed column (start of symbol, if available)
    pub column: usize,
    /// 1-indexed end line
    pub end_line: usize,
    /// 1-indexed end column
    pub end_column: usize,
    /// The matched symbol name (useful for aliases/imports)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_symbol: Option<String>,
}

/// Reason string for the `echo.agent/code/status` response.
///
/// Serialised as a camelCase string so clients can pattern-match on it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum IndexStatusReason {
    /// Index is running and ready.
    Active,
    /// Index is eligible but has not been started yet (first code-nav request
    /// will trigger lazy startup).
    NotStarted,
    /// Client type is not web (web-only for initial rollout).
    ClientNotWeb,
    /// Client did not advertise `echo.agent/codeNavigation.enabled`.
    CapabilityNotAdvertised,
    /// `codebase_indexing` feature is disabled in config.
    DisabledByConfig,
    /// The cwd is not inside a git repository.
    NotGitRepo,
    /// `sessionId` is required but was absent or refers to an unknown session.
    SessionRequired,
}

/// Response for status query.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StatusResponse {
    /// Whether an index is currently active for this cwd.
    pub indexed: bool,
    /// Whether this client is eligible to use codebase indexing.
    pub eligible: bool,
    /// Reason code describing the current status.
    pub reason: IndexStatusReason,
    /// Number of files in the index (present only when `indexed` is true).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_count: Option<usize>,
}

// ========== Handler ==========

/// Handle code navigation extension methods.
///
/// Routes through [`WorkspaceOps`]. Eligibility checks still run in shell since
/// they depend on agent-level config (client type, feature flags).
pub async fn handle(
    agent: &MvpAgent,
    ops: &echo_agent_workspace::WorkspaceOps,
    args: &acp::ExtRequest,
) -> ExtResult {
    use echo_agent_workspace::workspace_ops::*;

    match args.method.as_ref() {
        "echo.agent/code/goto-definition" => {
            let req: GotoRequest = serde_json::from_str(args.params.get())
                .map_err(|e| acp::Error::invalid_params().data(format!("invalid params: {e}")))?;
            let cwd = resolve_cwd(agent, req.cwd.clone(), req.session_id.as_ref())?;
            let was_newly_started =
                ensure_eligible_and_started(agent, req.session_id.as_ref(), &cwd)?;
            let start = std::time::Instant::now();
            let result = ops
                .dispatch(
                    &CodeGotoDefinitionReq {
                        root: Some(cwd.clone()),
                        file: cwd.join(&req.path).to_string_lossy().to_string(),
                        line: req.row,
                        col: req.column,
                    },
                    None,
                )
                .await
                .map_err(|e| acp::Error::internal_error().data(format!("code nav error: {e}")))?;
            log_code_nav_telemetry(
                "goto-definition",
                req.session_id.as_ref(),
                &cwd,
                was_newly_started,
                start.elapsed().as_millis(),
            );
            to_code_nav_ext_response(result)
        }
        "echo.agent/code/goto-references" => {
            let req: GotoRequest = serde_json::from_str(args.params.get())
                .map_err(|e| acp::Error::invalid_params().data(format!("invalid params: {e}")))?;
            let cwd = resolve_cwd(agent, req.cwd.clone(), req.session_id.as_ref())?;
            let was_newly_started =
                ensure_eligible_and_started(agent, req.session_id.as_ref(), &cwd)?;
            let start = std::time::Instant::now();
            let result = ops
                .dispatch(
                    &CodeGotoReferencesReq {
                        root: Some(cwd.clone()),
                        file: cwd.join(&req.path).to_string_lossy().to_string(),
                        line: req.row,
                        col: req.column,
                        include_definition: true,
                    },
                    None,
                )
                .await
                .map_err(|e| acp::Error::internal_error().data(format!("code nav error: {e}")))?;
            log_code_nav_telemetry(
                "goto-references",
                req.session_id.as_ref(),
                &cwd,
                was_newly_started,
                start.elapsed().as_millis(),
            );
            to_code_nav_ext_response(result)
        }
        "echo.agent/code/find-definitions" => {
            let req: FindSymbolRequest = serde_json::from_str(args.params.get())
                .map_err(|e| acp::Error::invalid_params().data(format!("invalid params: {e}")))?;
            let cwd = resolve_cwd(agent, req.cwd.clone(), req.session_id.as_ref())?;
            let was_newly_started =
                ensure_eligible_and_started(agent, req.session_id.as_ref(), &cwd)?;
            let start = std::time::Instant::now();
            let result = ops
                .dispatch(
                    &CodeFindDefinitionsReq {
                        root: Some(cwd.clone()),
                        symbol: req.symbol.clone(),
                        context_file: req
                            .context_path
                            .as_ref()
                            .map(|p| cwd.join(p).to_string_lossy().to_string()),
                    },
                    None,
                )
                .await
                .map_err(|e| acp::Error::internal_error().data(format!("code nav error: {e}")))?;
            log_code_nav_telemetry(
                "find-definitions",
                req.session_id.as_ref(),
                &cwd,
                was_newly_started,
                start.elapsed().as_millis(),
            );
            to_code_nav_ext_response(result)
        }
        "echo.agent/code/find-references" => {
            let req: FindSymbolRequest = serde_json::from_str(args.params.get())
                .map_err(|e| acp::Error::invalid_params().data(format!("invalid params: {e}")))?;
            let cwd = resolve_cwd(agent, req.cwd.clone(), req.session_id.as_ref())?;
            let was_newly_started =
                ensure_eligible_and_started(agent, req.session_id.as_ref(), &cwd)?;
            let start = std::time::Instant::now();
            let result = ops
                .dispatch(
                    &CodeFindReferencesReq {
                        root: Some(cwd.clone()),
                        symbol: req.symbol.clone(),
                        context_file: req
                            .context_path
                            .as_ref()
                            .map(|p| cwd.join(p).to_string_lossy().to_string()),
                    },
                    None,
                )
                .await
                .map_err(|e| acp::Error::internal_error().data(format!("code nav error: {e}")))?;
            log_code_nav_telemetry(
                "find-references",
                req.session_id.as_ref(),
                &cwd,
                was_newly_started,
                start.elapsed().as_millis(),
            );
            to_code_nav_ext_response(result)
        }
        "echo.agent/code/status" => {
            let req: StatusRequest = serde_json::from_str(args.params.get())
                .map_err(|e| acp::Error::invalid_params().data(format!("invalid params: {e}")))?;
            let cwd = resolve_cwd(agent, req.cwd.clone(), req.session_id.as_ref())?;

            // Check eligibility for the status response.
            let (eligible, reason, indexed, file_count) = match agent
                .code_nav_eligibility_for_request(req.session_id.as_ref(), &cwd)
            {
                Ok(()) => {
                    let result = ops
                        .dispatch(
                            &CodeIndexStatusReq {
                                root: Some(cwd.clone()),
                            },
                            None,
                        )
                        .await
                        .map_err(|e| {
                            acp::Error::internal_error().data(format!("code nav error: {e}"))
                        })?;
                    if result.active {
                        (true, IndexStatusReason::Active, true, result.file_count)
                    } else {
                        (true, IndexStatusReason::NotStarted, false, None)
                    }
                }
                Err(ineligible) => {
                    let reason = match ineligible {
                        CodeNavEligibility::ClientNotWeb => IndexStatusReason::ClientNotWeb,
                        CodeNavEligibility::CapabilityNotAdvertised => {
                            IndexStatusReason::CapabilityNotAdvertised
                        }
                        CodeNavEligibility::DisabledByConfig => IndexStatusReason::DisabledByConfig,
                        CodeNavEligibility::NotGitRepo => IndexStatusReason::NotGitRepo,
                        CodeNavEligibility::SessionRequired => IndexStatusReason::SessionRequired,
                    };
                    (false, reason, false, None)
                }
            };

            let status = StatusResponse {
                indexed,
                eligible,
                reason,
                file_count,
            };
            super::to_ext_response(Ok(status))
        }
        _ => Err(acp::Error::method_not_found()),
    }
}

/// Convert workspace CodeNavResponse to the shell's CodeNavResponse format
/// and wrap in the `ExtMethodResult` envelope that clients expect.
fn to_code_nav_ext_response(
    resp: echo_agent_workspace::workspace_ops::CodeNavResponse,
) -> ExtResult {
    let symbol = resp
        .locations
        .first()
        .and_then(|l| l.symbol.clone())
        .unwrap_or_default();
    let shell_resp = CodeNavResponse {
        symbol,
        locations: resp
            .locations
            .into_iter()
            .map(|loc| SymbolLocation {
                path: loc.path,
                line: loc.line,
                column: 0,
                end_line: loc.line,
                end_column: 0,
                matched_symbol: loc.symbol,
            })
            .collect(),
    };
    super::to_ext_response(Ok(shell_resp))
}

/// Check eligibility, ensure the codebase index is started, and return
/// whether the index was newly created (for telemetry).
fn ensure_eligible_and_started(
    agent: &MvpAgent,
    session_id: Option<&acp::SessionId>,
    cwd: &Path,
) -> Result<bool, acp::Error> {
    if let Err(reason) = agent.code_nav_eligibility_for_request(session_id, cwd) {
        return Err(eligibility_error(reason));
    }
    // Start the index if not already running (lazy creation).
    let was_newly_started = agent
        .start_codebase_index_for_code_nav(session_id, cwd)
        .map(|(_, was_new)| was_new)
        .unwrap_or(false);
    Ok(was_newly_started)
}

// ========== Helper Functions ==========

/// Resolve cwd from session_id or direct cwd parameter.
fn resolve_cwd(
    agent: &MvpAgent,
    cwd: Option<String>,
    session_id: Option<&acp::SessionId>,
) -> Result<PathBuf, acp::Error> {
    // Prefer direct cwd if provided
    if let Some(cwd_str) = cwd {
        return Ok(PathBuf::from(cwd_str));
    }

    // Fall back to session's cwd
    if let Some(sid) = session_id
        && let Some(session_cwd) = agent.get_session_cwd(sid)
    {
        return Ok(session_cwd);
    }

    Err(acp::Error::invalid_params().data("either cwd or valid sessionId must be provided"))
}

/// Map a `CodeNavEligibility` error to a human-readable ACP error.
fn eligibility_error(reason: CodeNavEligibility) -> acp::Error {
    let msg = match reason {
        CodeNavEligibility::ClientNotWeb => {
            "code navigation is currently only enabled for echo-agent-web clients"
        }
        CodeNavEligibility::CapabilityNotAdvertised => {
            "client must advertise echo.agent/codeNavigation.enabled to use code navigation"
        }
        CodeNavEligibility::DisabledByConfig => "code navigation is disabled by configuration",
        CodeNavEligibility::NotGitRepo => {
            "code navigation requires the workspace to be inside a git repository"
        }
        CodeNavEligibility::SessionRequired => {
            "sessionId is required for code navigation and must refer to a valid active session"
        }
    };
    acp::Error::invalid_params().data(msg)
}
