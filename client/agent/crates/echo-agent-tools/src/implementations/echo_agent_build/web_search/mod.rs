//! `web_search` tool — new architecture (`Tool` trait).
//!
//! Calls the Responses API with web search capability. Reads the
//! pre-constructed `WebSearchClient` from Resources (inserted by
//! `with_backend()` when the config is `Enabled`).

use crate::implementations::web_search::client::WebSearchClient;
use crate::types::output::WebSearchOutput;
use crate::types::requirements::{Expr, ToolRequirement};
use crate::types::tool::{ToolKind, ToolNamespace};

// ───────────────────────────────────────────────────────────────────────────
// Input
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct WebSearchInput {
    #[schemars(description = "The search query to perform.")]
    pub query: String,
    #[schemars(description = "Optional list of domains to restrict search to.")]
    pub allowed_domains: Option<Vec<String>>,
}

// ───────────────────────────────────────────────────────────────────────────
// Tool implementation
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct WebSearchTool;

impl crate::types::tool_metadata::ToolMetadata for WebSearchTool {
    fn kind(&self) -> ToolKind {
        ToolKind::WebSearch
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::EchoAgentBuild
    }

    fn description_template(&self) -> &str {
        "Search the web for up-to-date information, tailored for coding and software development tasks."
    }

    fn requires_expr(&self) -> Expr<ToolRequirement> {
        Expr::True
    }
}

impl echo_agent_tool_runtime::Tool for WebSearchTool {
    type Args = WebSearchInput;
    type Output = WebSearchOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("web_search").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "web_search",
            crate::types::tool_metadata::ToolMetadata::sanitized_description_template(self),
        )
    }

    fn capabilities(&self) -> echo_agent_tool_protocol::ToolCapabilities {
        echo_agent_tool_protocol::ToolCapabilities {
            is_read_only: true,
            tool_scope: Some(echo_agent_tool_protocol::ToolScope::Read),
            ..Default::default()
        }
    }

    #[tracing::instrument(name = "tool.web_search", skip_all)]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: WebSearchInput,
    ) -> Result<WebSearchOutput, echo_agent_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;

        let client;
        {
            let res = resources.lock().await;
            client = res.require::<WebSearchClient>()?.clone();
        }

        let (content, citations) = client
            .search(&input.query, input.allowed_domains.clone())
            .await
            .map_err(|e| {
                echo_agent_tool_runtime::ToolError::execution(
                    echo_agent_tool_protocol::ToolId::new("web_search").expect("valid"),
                    e.to_string(),
                )
            })?;

        Ok(WebSearchOutput {
            query: input.query.clone(),
            content,
            citations,
            allowed_domains: input.allowed_domains.clone(),
            pre_formatted: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::resources::Resources;
    use crate::types::tool_metadata::test_ctx_with_call_id;

    #[test]
    fn tool_name_and_description() {
        let tool = WebSearchTool;
        assert_eq!(
            echo_agent_tool_runtime::Tool::id(&tool).as_str(),
            "web_search"
        );
        assert!(
            crate::types::tool_metadata::ToolMetadata::description_template(&tool)
                .contains("Search the web")
        );
        assert!(
            crate::types::tool_metadata::ToolMetadata::description_template(&tool)
                .contains("coding")
        );
    }

    #[tokio::test]
    async fn errors_when_client_not_in_resources() {
        let resources = Resources::new();
        let tool = WebSearchTool;
        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx_with_call_id(resources.into_shared(), "test-call"),
            WebSearchInput {
                query: "test".into(),
                allowed_domains: None,
            },
        )
        .await;

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("missing required resource"),
            "Expected 'missing required resource' error, got: {err_msg}"
        );
    }
}
