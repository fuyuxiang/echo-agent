use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::auth::{AuthManager, EchoAgentAuth};

const ECHO_AGENT_WEB_URL: &str = "https://cloud.echo-agent.invalid";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    #[serde(default)]
    pub conversation_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub starred: bool,
    #[serde(default)]
    pub create_time: Option<String>,
    #[serde(default)]
    pub modify_time: Option<String>,
    #[serde(default)]
    pub workspaces: Vec<Workspace>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    #[serde(default)]
    pub workspace_id: String,
}

#[derive(Debug, Clone, Default)]
pub struct ConvQuery {
    pub page_size: i64,
    pub page_token: Option<String>,
    pub search_query: Option<String>,
    pub workspace_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ListConversationsPage {
    pub conversations: Vec<Conversation>,
    pub next_page_token: Option<String>,
}

/// Body for `PUT /rest/app-chat/conversations/{id}` (echo-agent-web `chatUpdateConversation`).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateConversationBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starred: Option<bool>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConvError {
    #[error("no OAuth credentials for conversations:read")]
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
struct ListConversationsResponseWire {
    #[serde(default)]
    conversations: Vec<Conversation>,
    #[serde(default)]
    next_page_token: Option<String>,
    #[serde(default)]
    text_search_matches: Vec<ListConversationsMatchWire>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListConversationsMatchWire {
    #[serde(default)]
    conversation: Option<Conversation>,
}

pub struct ConversationsClient {
    http: reqwest::Client,
    base_url: String,
    auth: Arc<AuthManager>,
}

impl ConversationsClient {
    pub fn new(auth: Arc<AuthManager>) -> Self {
        let base_url = std::env::var("ECHO_AGENT_CONVERSATIONS_BASE_URL")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                std::env::var("ECHO_AGENT_CODE_WEB_URL")
                    .ok()
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or_else(|| ECHO_AGENT_WEB_URL.to_string());
        Self {
            http: crate::http::shared_client(),
            base_url,
            auth,
        }
    }

    async fn require_echo_agent_auth(&self) -> Result<EchoAgentAuth, ConvError> {
        let auth = self.auth.auth().await.map_err(|_| ConvError::NoOauth)?;
        if !auth.is_echo_agent_auth() {
            return Err(ConvError::NoOauth);
        }
        Ok(auth)
    }

    fn apply_auth_headers(
        &self,
        builder: reqwest::RequestBuilder,
        auth: &EchoAgentAuth,
    ) -> reqwest::RequestBuilder {
        let mut builder = builder
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
        echo_agent_file_utils::trace_context::inject_trace_context_into_request(builder)
    }

    pub async fn list_conversations(
        &self,
        q: &ConvQuery,
    ) -> Result<ListConversationsPage, ConvError> {
        let auth = self.require_echo_agent_auth().await?;

        let url = format!("{}/rest/app-chat/conversations", self.base_url);
        let mut query: Vec<(&str, String)> = vec![("pageSize", q.page_size.to_string())];
        if let Some(token) = q.page_token.as_deref().filter(|s| !s.is_empty()) {
            query.push(("pageToken", token.to_owned()));
        }
        if let Some(search) = q.search_query.as_deref().filter(|s| !s.is_empty()) {
            query.push(("searchQuery", search.to_owned()));
        }
        if let Some(workspace) = q.workspace_id.as_deref().filter(|s| !s.is_empty()) {
            query.push(("workspaceId", workspace.to_owned()));
        }

        let builder = self.apply_auth_headers(self.http.get(&url).query(&query), &auth);

        let response = builder.send().await?;
        let status = response.status();
        if !status.is_success() {
            return Err(ConvError::Http {
                status: status.as_u16(),
            });
        }

        let bytes = response.bytes().await?;
        let wire: ListConversationsResponseWire = serde_json::from_slice(&bytes)?;

        let searching = q.search_query.as_deref().is_some_and(|s| !s.is_empty());
        // During an active search, results come exclusively from
        // `text_search_matches`. Never fall back to `wire.conversations` here:
        // an empty match set means "no hits", and the server may return
        // recent/unfiltered conversations in `conversations` that are NOT search
        // matches — surfacing those would be wrong.
        let conversations = if searching {
            wire.text_search_matches
                .into_iter()
                .filter_map(|m| m.conversation)
                .collect()
        } else {
            wire.conversations
        };

        Ok(ListConversationsPage {
            conversations,
            next_page_token: wire.next_page_token.filter(|t| !t.is_empty()),
        })
    }

    /// `PUT /rest/app-chat/conversations/{conversation_id}` — rename and/or star.
    pub async fn update_conversation(
        &self,
        conversation_id: &str,
        body: &UpdateConversationBody,
    ) -> Result<(), ConvError> {
        let auth = self.require_echo_agent_auth().await?;
        let url = format!(
            "{}/rest/app-chat/conversations/{}",
            self.base_url,
            urlencoding::encode(conversation_id)
        );
        let builder = self
            .apply_auth_headers(self.http.put(&url), &auth)
            .json(body);

        let response = builder.send().await?;
        let status = response.status();
        if !status.is_success() {
            return Err(ConvError::Http {
                status: status.as_u16(),
            });
        }
        Ok(())
    }

    /// `DELETE /rest/app-chat/conversations/soft/{conversation_id}` — soft-delete.
    pub(crate) async fn soft_delete_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<(), ConvError> {
        let auth = self.require_echo_agent_auth().await?;
        let url = format!(
            "{}/rest/app-chat/conversations/soft/{}",
            self.base_url,
            urlencoding::encode(conversation_id)
        );
        let builder = self.apply_auth_headers(self.http.delete(&url), &auth);

        let response = builder.send().await?;
        let status = response.status();
        // 404 = already soft-deleted; keep deletion idempotent like the
        // build path's `classify_remote_delete`.
        if !status.is_success() && status.as_u16() != 404 {
            return Err(ConvError::Http {
                status: status.as_u16(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversation_parses_camelcase_wire() {
        let json = serde_json::json!({
            "conversations": [{
                "conversationId": "conv_abc",
                "title": "Compare GPU vendors",
                "starred": true,
                "createTime": "2026-06-18T17:30:00Z",
                "modifyTime": "2026-06-18T18:02:00Z",
                "workspaces": [{ "workspaceId": "ws_9f3a" }]
            }],
            "nextPageToken": "tok2"
        });
        let wire: ListConversationsResponseWire = serde_json::from_value(json).unwrap();
        assert_eq!(wire.conversations.len(), 1);
        let c = &wire.conversations[0];
        assert_eq!(c.conversation_id, "conv_abc");
        assert_eq!(c.title, "Compare GPU vendors");
        assert!(c.starred);
        assert_eq!(c.modify_time.as_deref(), Some("2026-06-18T18:02:00Z"));
        assert_eq!(c.workspaces[0].workspace_id, "ws_9f3a");
        assert_eq!(wire.next_page_token.as_deref(), Some("tok2"));
    }

    #[test]
    fn missing_fields_default_gracefully() {
        let json = serde_json::json!({ "conversations": [{ "conversationId": "c1" }] });
        let wire: ListConversationsResponseWire = serde_json::from_value(json).unwrap();
        let c = &wire.conversations[0];
        assert_eq!(c.conversation_id, "c1");
        assert!(c.title.is_empty());
        assert!(c.modify_time.is_none());
        assert!(c.create_time.is_none());
        assert!(c.workspaces.is_empty());
        assert!(wire.next_page_token.is_none());
    }

    #[test]
    fn update_body_serializes_only_set_fields() {
        let title_only = UpdateConversationBody {
            title: Some("New title".into()),
            starred: None,
        };
        assert_eq!(
            serde_json::to_value(&title_only).unwrap(),
            serde_json::json!({ "title": "New title" })
        );

        let both = UpdateConversationBody {
            title: Some("T".into()),
            starred: Some(true),
        };
        assert_eq!(
            serde_json::to_value(&both).unwrap(),
            serde_json::json!({ "title": "T", "starred": true })
        );
    }
}
