//! Search extension API layer (fuzzy file search, content search).
//!
//! Routing: prefers explicit `cwd`, falls back to session lookup via `sessionId`.

use crate::agent::mvp_agent::MvpAgent;
use crate::session::ExtMethodResult;
use agent_client_protocol as acp;
use echo_agent_workspace::file_system::ContentSearchRequest as ContentSearchRequestParams;
use echo_agent_workspace::workspace_ops::{FuzzyChangeReq, FuzzyCloseReq, FuzzyOpenReq};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

type ExtResult = Result<acp::ExtResponse, acp::Error>;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FuzzyOpenResponse {
    pub session_id: String,
    pub search_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FuzzyChangeResponse {
    pub session_id: String,
    pub search_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FuzzyCloseResponse {
    pub session_id: String,
    pub search_id: String,
    pub closed: bool,
}

pub use crate::extensions::routing::{ClientId, NotificationMeta, RequestMeta, TargetClientId};

fn parse<T: for<'de> Deserialize<'de>>(s: &str) -> Result<T, acp::Error> {
    serde_json::from_str::<T>(s)
        .map_err(|e| acp::Error::invalid_params().data(format!("invalid params: {}", e)))
}

/// Resolve the search root, preferring an explicit `cwd` over a `sessionId` lookup.
fn resolve_cwd(
    agent: &MvpAgent,
    cwd: Option<String>,
    session_id: Option<&acp::SessionId>,
) -> Result<PathBuf, acp::Error> {
    if let Some(cwd) = cwd {
        return Ok(PathBuf::from(cwd));
    }

    if let Some(session_id) = session_id {
        if let Some(cwd) = agent.get_session_cwd(session_id) {
            return Ok(cwd);
        }
        return Err(
            acp::Error::invalid_params().data(format!("session not found: {}", session_id.0))
        );
    }

    Err(acp::Error::invalid_params().data("either cwd or sessionId is required"))
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FuzzyOpenRequest {
    /// Optional session ID - used to lookup cwd if cwd not provided directly
    #[serde(default)]
    pub session_id: Option<acp::SessionId>,
    /// Optional absolute cwd path - preferred over session_id lookup
    #[serde(default)]
    pub cwd: Option<String>,
    /// Optional relative path within the resolved cwd
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub hidden: bool,
    /// Metadata for routing (contains client_id from relay).
    #[serde(default, rename = "_meta")]
    pub meta: Option<RequestMeta>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FuzzyChangeRequest {
    pub search_id: String,
    pub query: String,
    #[serde(default)]
    pub dirs_only: bool,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FuzzyCloseRequest {
    pub search_id: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentSearchRequest {
    #[serde(default)]
    pub session_id: Option<acp::SessionId>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(flatten)]
    pub params: ContentSearchRequestParams,
}

pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        "echo.agent/search/fuzzy/open" => {
            let req: FuzzyOpenRequest = parse(args.params.get())?;
            let cwd = resolve_cwd(agent, req.cwd, req.session_id.as_ref())?;
            let search_root = match &req.root {
                Some(r) => cwd.join(r),
                None => cwd,
            };
            let session_id = req.session_id.map(|s| s.0.to_string());
            let target_client_id = req.meta.map(|m| m.client_id).unwrap_or_default();

            let ops = agent
                .resolve_workspace_ops()
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
            let search_id = ops
                .dispatch(
                    &FuzzyOpenReq {
                        root: Some(search_root),
                        request_id: req.request_id,
                        hidden: req.hidden,
                        session_id: session_id.clone(),
                        target_client_id,
                    },
                    None,
                )
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;

            let response = FuzzyOpenResponse {
                session_id: session_id.unwrap_or_else(|| "agent".to_string()),
                search_id,
            };
            ExtMethodResult::success(response)
                .to_ext_response()
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))
        }
        "echo.agent/search/fuzzy/change" => {
            let req: FuzzyChangeRequest = parse(args.params.get())?;
            let ops = agent
                .resolve_workspace_ops()
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
            // The workspace owns the manager and spawns the status driver, which
            // streams `echo.agent/search/fuzzy/status` through the client sink.
            let found = ops
                .dispatch(
                    &FuzzyChangeReq {
                        search_id: req.search_id.clone(),
                        query: req.query.clone(),
                        dirs_only: req.dirs_only,
                        limit: req.limit,
                    },
                    None,
                )
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;

            if !found {
                return Err(acp::Error::invalid_params()
                    .data(format!("search not found: {}", req.search_id)));
            }

            let response = FuzzyChangeResponse {
                session_id: "agent".to_string(),
                search_id: req.search_id,
            };
            ExtMethodResult::success(response)
                .to_ext_response()
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))
        }
        "echo.agent/search/fuzzy/close" => {
            let req: FuzzyCloseRequest = parse(args.params.get())?;
            let ops = agent
                .resolve_workspace_ops()
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
            let closed = ops
                .dispatch(
                    &FuzzyCloseReq {
                        search_id: req.search_id.clone(),
                    },
                    None,
                )
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;

            let response = FuzzyCloseResponse {
                session_id: "agent".to_string(),
                search_id: req.search_id,
                closed,
            };
            ExtMethodResult::success(response)
                .to_ext_response()
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))
        }
        "echo.agent/search/content" => {
            let req: ContentSearchRequest = parse(args.params.get())?;
            let cwd = resolve_cwd(agent, req.cwd.clone(), req.session_id.as_ref())?;
            let context_id = req
                .session_id
                .as_ref()
                .map(|s| s.0.to_string())
                .unwrap_or_else(|| "agent".to_string());
            let ops = agent
                .resolve_workspace_ops()
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;

            // The workspace runs the streaming search and emits
            // `echo.agent/search/content/status` batches through the client sink.
            let mut op = req.params;
            op.cwd = Some(cwd);
            op.context_id = Some(context_id);
            let data = ops
                .dispatch(&op, None)
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;

            ExtMethodResult::success(data)
                .to_ext_response()
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))
        }
        _ => Err(acp::Error::method_not_found()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fuzzy_open_request_with_cwd() {
        let json = r#"{"cwd": "/path/to/project", "hidden": false}"#;
        let req: FuzzyOpenRequest = serde_json::from_str(json).unwrap();

        assert_eq!(req.cwd, Some("/path/to/project".to_string()));
        assert_eq!(req.session_id, None);
        assert!(!req.hidden);
    }

    #[test]
    fn test_fuzzy_open_request_with_session_id() {
        let json = r#"{"sessionId": "session-123", "hidden": true}"#;
        let req: FuzzyOpenRequest = serde_json::from_str(json).unwrap();

        assert!(req.session_id.is_some());
        assert_eq!(req.session_id.unwrap().0.as_ref(), "session-123");
        assert_eq!(req.cwd, None);
        assert!(req.hidden);
    }

    #[test]
    fn test_fuzzy_open_request_with_both_cwd_and_session_id() {
        let json = r#"{"sessionId": "session-123", "cwd": "/path/to/project"}"#;
        let req: FuzzyOpenRequest = serde_json::from_str(json).unwrap();

        // Both should be present - cwd takes precedence in resolve_cwd
        assert!(req.session_id.is_some());
        assert_eq!(req.cwd, Some("/path/to/project".to_string()));
    }

    #[test]
    fn test_fuzzy_open_request_with_root() {
        let json = r#"{"cwd": "/home/user", "root": "src"}"#;
        let req: FuzzyOpenRequest = serde_json::from_str(json).unwrap();

        assert_eq!(req.cwd, Some("/home/user".to_string()));
        assert_eq!(req.root, Some("src".to_string()));
    }

    #[test]
    fn test_fuzzy_change_request() {
        let json = r#"{"searchId": "search-456", "query": "main.rs", "limit": 10}"#;
        let req: FuzzyChangeRequest = serde_json::from_str(json).unwrap();

        assert_eq!(req.search_id, "search-456");
        assert_eq!(req.query, "main.rs");
        assert_eq!(req.limit, Some(10));
        assert!(!req.dirs_only);
    }

    #[test]
    fn test_fuzzy_close_request() {
        let json = r#"{"searchId": "search-456"}"#;
        let req: FuzzyCloseRequest = serde_json::from_str(json).unwrap();

        assert_eq!(req.search_id, "search-456");
    }

    #[test]
    fn test_fuzzy_open_request_defaults() {
        let json = r#"{"cwd": "/path"}"#;
        let req: FuzzyOpenRequest = serde_json::from_str(json).unwrap();

        assert_eq!(req.session_id, None);
        assert_eq!(req.root, None);
        assert_eq!(req.request_id, None);
        assert!(!req.hidden);
    }

    #[test]
    fn test_fuzzy_change_request_defaults() {
        let json = r#"{"searchId": "s1", "query": "q"}"#;
        let req: FuzzyChangeRequest = serde_json::from_str(json).unwrap();

        assert!(!req.dirs_only);
        assert_eq!(req.limit, None);
    }

    #[test]
    fn test_fuzzy_open_request_with_meta() {
        // Test that FuzzyOpenRequest correctly deserializes _meta.clientId
        // This is what the relay injects into the request
        let json = r#"{
            "cwd": "/path/to/project",
            "requestId": "req-123",
            "hidden": false,
            "_meta": {
                "clientId": {
                    "instanceId": "relay-instance-1",
                    "connId": "client-conn-abc"
                }
            }
        }"#;

        let req: FuzzyOpenRequest = serde_json::from_str(json).unwrap();

        assert_eq!(req.cwd, Some("/path/to/project".to_string()));
        assert!(req.meta.is_some());

        let meta = req.meta.unwrap();
        match &meta.client_id {
            TargetClientId::ClientId(client_id) => {
                assert_eq!(client_id.instance_id, "relay-instance-1");
                assert_eq!(client_id.conn_id, "client-conn-abc");
            }
            TargetClientId::None => {
                panic!("Expected ClientId, got None");
            }
        }
    }
}
