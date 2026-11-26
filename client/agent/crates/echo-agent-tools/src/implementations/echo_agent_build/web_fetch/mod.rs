//! `web_fetch` tool — client-side URL fetching with improved HTML-to-markdown
//! conversion and SSRF protection.
//!
//! Fetches a URL via `reqwest`, converts HTML to markdown via `htmd` (with
//! `<script>`/`<style>`/etc. stripped), and returns content to the model.
//! Prefers `text/markdown` in the Accept header so doc sites that serve markdown
//! directly bypass the HTML conversion entirely.

mod artifact;
mod cache;
pub mod client;
pub mod config;
pub mod domain;
pub mod error;
mod http;
pub(crate) mod overflow;
mod ssrf;

pub use client::WebFetchClient;
pub use config::WebFetchParams;
pub use domain::{DomainMatcher, domain_from_url};
pub use error::WebFetchError;

// ───────────────────────────────────────────────────────────────────────────
// Config enum (feature flag gating)
// ───────────────────────────────────────────────────────────────────────────

/// Configuration for the `web_fetch` tool.
///
/// When `Enabled`, the tool is registered and a `WebFetchClient` is injected
/// into `Resources`. When `Disabled` (default), the tool is not registered.
#[derive(Debug, Clone, Default)]
pub enum WebFetchConfig {
    #[default]
    Disabled,
    Enabled {
        /// Runtime parameters (allowed_domains, proxy_endpoint, timeouts, etc.)
        params: WebFetchParams,
    },
}

impl WebFetchConfig {
    /// Returns `true` when the config is the `Enabled` variant.
    pub fn is_enabled(&self) -> bool {
        matches!(self, Self::Enabled { .. })
    }
}

use crate::types::output::WebFetchOutput;
use crate::types::requirements::{Expr, ToolRequirement};
use crate::types::resources::SessionFolder;
use crate::types::tool::{ToolKind, ToolNamespace};

// ───────────────────────────────────────────────────────────────────────────
// Input
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct WebFetchInput {
    /// The URL to fetch content from.
    #[schemars(description = "The URL to fetch content from.")]
    pub url: String,
}

// ───────────────────────────────────────────────────────────────────────────
// Tool
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct WebFetchTool;

impl crate::types::tool_metadata::ToolMetadata for WebFetchTool {
    fn kind(&self) -> ToolKind {
        ToolKind::WebFetch
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::EchoAgentBuild
    }

    fn description_template(&self) -> &str {
        r#"Fetch the content of a specific URL and return it as markdown.

IMPORTANT: ${{ tools.by_kind.web_fetch }} WILL FAIL for authenticated or private URLs (e.g. Google Docs, Confluence, Jira, GitHub private repos). Use specialized MCP tools for those instead.

Usage notes:
  - HTTP URLs will be automatically upgraded to HTTPS
  - Long pages will be truncated to fit your context window"#
    }

    fn versioned_definition(
        &self,
        _contract_version: Option<&str>,
        client_name: &str,
        description_override: Option<&str>,
        renderer: &crate::types::template_renderer::TemplateRenderer,
        param_map: &std::collections::HashMap<String, String>,
        input_schema: &serde_json::Value,
        effective_params: &serde_json::Value,
    ) -> crate::types::definition::ToolDefinition {
        let params: WebFetchParams = serde_json::from_value(effective_params.clone())
            .unwrap_or_else(|e| {
                tracing::warn!("Failed to deserialize WebFetchParams: {e}");
                WebFetchParams::default()
            });
        let raw_desc = description_override.unwrap_or_else(|| {
            crate::types::tool_metadata::ToolMetadata::description_template(self)
        });
        let extras = serde_json::json!({
            "proxy_enabled": params.proxy_endpoint.is_some(),
        });
        let description = renderer
            .render_with_extra(raw_desc, &extras)
            .unwrap_or_else(|e| {
                tracing::warn!("Description template render failed, using raw: {e}");
                raw_desc.to_string()
            });
        let remapped_schema = if param_map.is_empty() {
            input_schema.clone()
        } else {
            crate::util::remap::remap_schema_properties(input_schema, param_map)
        };
        crate::types::definition::ToolDefinition::function(
            client_name,
            Some(&description),
            remapped_schema,
        )
    }

    fn requires_expr(&self) -> Expr<ToolRequirement> {
        Expr::True
    }
}

impl echo_agent_tool_runtime::Tool for WebFetchTool {
    type Args = WebFetchInput;
    type Output = WebFetchOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("web_fetch").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "web_fetch",
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

    #[tracing::instrument(name = "tool.web_fetch", skip_all, fields(url = %input.url))]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: WebFetchInput,
    ) -> Result<WebFetchOutput, echo_agent_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;

        let (client, session_folder, read_tool_name, execute_tool_name) = {
            let res = resources.lock().await;
            let client = res.require::<WebFetchClient>()?.clone();
            let session_folder = res.get::<SessionFolder>().map(|folder| folder.0.clone());
            let renderer = res.get::<crate::types::template_renderer::TemplateRenderer>();
            let read_tool_name = renderer
                .and_then(|renderer| renderer.tool_for_kind(ToolKind::Read))
                .map(str::to_owned);
            let execute_tool_name = renderer
                .and_then(|renderer| renderer.tool_for_kind(ToolKind::Execute))
                .map(str::to_owned);
            (client, session_folder, read_tool_name, execute_tool_name)
        };

        let output = client
            .fetch(
                &input.url,
                session_folder.as_deref(),
                read_tool_name.as_deref(),
                execute_tool_name.as_deref(),
            )
            .await?;
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::tool_metadata::test_ctx_with_call_id;

    #[test]
    fn tool_name_and_description() {
        let tool = WebFetchTool;
        assert_eq!(
            echo_agent_tool_runtime::Tool::id(&tool).as_str(),
            "web_fetch"
        );
        assert_eq!(
            crate::types::tool_metadata::ToolMetadata::kind(&tool),
            ToolKind::WebFetch
        );
        assert!(
            crate::types::tool_metadata::ToolMetadata::description_template(&tool)
                .contains("Fetch the content of a specific URL")
        );
    }

    #[tokio::test]
    async fn errors_when_client_not_in_resources() {
        let resources = crate::types::resources::Resources::new();
        let tool = WebFetchTool;
        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx_with_call_id(resources.into_shared(), "test-call"),
            WebFetchInput {
                url: "https://example.com".into(),
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
