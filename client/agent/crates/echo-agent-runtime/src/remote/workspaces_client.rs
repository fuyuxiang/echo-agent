use std::sync::Arc;

use serde::Deserialize;

use crate::auth::AuthManager;

const ECHO_AGENT_WEB_URL: &str = "https://cloud.echo-agent.invalid";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub create_time: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct WsQuery {
    pub page_size: i64,
    pub page_token: Option<String>,
    pub query: Option<String>,
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ListWorkspacesPage {
    pub workspaces: Vec<Workspace>,
    pub next_page_token: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum WsError {
    #[error("no OAuth credentials for workspaces:read")]
    NoOauth,
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("request failed: {status}")]
    Http { status: u16 },
    #[error("parse error: {0}")]
    Parse(#[from] serde_json::Error),
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListWorkspacesResponseWire {
    #[serde(default)]
    workspaces: Vec<Workspace>,
    #[serde(default)]
    next_page_token: Option<String>,
}

pub struct WorkspacesClient {
    http: reqwest::Client,
    base_url: String,
    auth: Arc<AuthManager>,
}

impl WorkspacesClient {
    pub fn new(auth: Arc<AuthManager>) -> Self {
        let base_url = first_nonempty_env(&[
            "ECHO_AGENT_WORKSPACES_BASE_URL",
            "ECHO_AGENT_CONVERSATIONS_BASE_URL",
            "ECHO_AGENT_CODE_WEB_URL",
        ])
        .unwrap_or_else(|| ECHO_AGENT_WEB_URL.to_string());
        Self {
            http: crate::http::shared_client(),
            base_url,
            auth,
        }
    }

    pub(crate) async fn list_workspaces(&self, q: &WsQuery) -> Result<ListWorkspacesPage, WsError> {
        let auth = self.auth.auth().await.map_err(|_| WsError::NoOauth)?;
        if !auth.is_echo_agent_auth() {
            return Err(WsError::NoOauth);
        }

        let url = format!("{}/rest/workspaces", self.base_url);
        let mut query: Vec<(&str, String)> = vec![("pageSize", q.page_size.to_string())];
        if let Some(token) = q.page_token.as_deref().filter(|s| !s.is_empty()) {
            query.push(("pageToken", token.to_owned()));
        }
        if let Some(search) = q.query.as_deref().filter(|s| !s.is_empty()) {
            query.push(("query", search.to_owned()));
        }
        if let Some(kind) = q.kind.as_deref().filter(|s| !s.is_empty()) {
            query.push(("kind", kind.to_owned()));
        }

        let mut builder = self
            .http
            .get(&url)
            .query(&query)
            .header("Authorization", format!("Bearer {}", auth.key))
            .header(
                "X-Echo-Agent-Token-Auth",
                self.auth.echo_agent_com_config().token_header.clone(),
            )
            .header("x-userid", &auth.user_id)
            .header("x-echo-agent-client-version", echo_agent_version::VERSION)
            .header(
                "x-echo-agent-client-identifier",
                crate::http::process_client_identifier(),
            )
            .header(
                crate::http::CLIENT_MODE_HEADER,
                crate::http::process_client_mode(),
            )
            .header(reqwest::header::ACCEPT, "application/json");
        if let Some(email) = &auth.email {
            builder = builder.header("x-email", email);
        }
        let builder =
            echo_agent_file_utils::trace_context::inject_trace_context_into_request(builder);

        let response = builder.send().await?;
        let status = response.status();
        if !status.is_success() {
            return Err(WsError::Http {
                status: status.as_u16(),
            });
        }

        let bytes = response.bytes().await?;
        let wire: ListWorkspacesResponseWire = serde_json::from_slice(&bytes)?;

        Ok(ListWorkspacesPage {
            workspaces: wire.workspaces,
            next_page_token: wire.next_page_token.filter(|t| !t.is_empty()),
        })
    }
}

fn first_nonempty_env(keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| std::env::var(k).ok().filter(|s| !s.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_parses_camelcase_wire() {
        let json = serde_json::json!({
            "workspaces": [{
                "workspaceId": "ws_9f3a",
                "name": "GPU vendor research",
                "createTime": "2026-06-18T17:30:00Z",
                "kind": "WORKSPACE_KIND_IMAGINE"
            }],
            "nextPageToken": "tok2"
        });
        let wire: ListWorkspacesResponseWire = serde_json::from_value(json).unwrap();
        assert_eq!(wire.workspaces.len(), 1);
        let w = &wire.workspaces[0];
        assert_eq!(w.workspace_id, "ws_9f3a");
        assert_eq!(w.name, "GPU vendor research");
        assert_eq!(w.create_time.as_deref(), Some("2026-06-18T17:30:00Z"));
        assert_eq!(w.kind.as_deref(), Some("WORKSPACE_KIND_IMAGINE"));
        assert_eq!(wire.next_page_token.as_deref(), Some("tok2"));
    }

    #[test]
    fn missing_fields_default_gracefully() {
        let json = serde_json::json!({ "workspaces": [{ "workspaceId": "w1" }] });
        let wire: ListWorkspacesResponseWire = serde_json::from_value(json).unwrap();
        let w = &wire.workspaces[0];
        assert_eq!(w.workspace_id, "w1");
        assert!(w.name.is_empty());
        assert!(w.create_time.is_none());
        assert!(w.kind.is_none());
        assert!(wire.next_page_token.is_none());
    }
}
