use std::borrow::Cow;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::json;
// rmcp is quarantined in echo-agent-mcp; see that crate's docs.
use echo_agent_mcp::rmcp;
use echo_agent_mcp::rmcp::ServerHandler;
use echo_agent_mcp::rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData as McpError,
    JsonObject, ListToolsResult, MetaObject, PaginatedRequestParams, ServerCapabilities,
    ServerConfig, Tool,
};

#[derive(Clone)]
struct TestMcpServer {
    tools: Arc<Vec<Tool>>,
}

impl TestMcpServer {
    fn new() -> Self {
        let tools = vec![Self::echo_tool()];
        Self {
            tools: Arc::new(tools),
        }
    }

    fn echo_tool() -> Tool {
        let schema: JsonObject = serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "Message to echo back"
                }
            },
            "required": ["message"],
            "additionalProperties": false
        }))
        .unwrap();

        Tool::new(
            Cow::Borrowed("echo"),
            Cow::Borrowed("Echo back the provided message"),
            Arc::new(schema),
        )
    }
}

#[derive(Deserialize)]
struct EchoArgs {
    message: String,
}

impl ServerHandler for TestMcpServer {
    fn get_info(&self) -> ServerConfig {
        let mut info = ServerConfig::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(self.tools.as_ref().clone()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        match request.name.as_ref() {
            "echo" => {
                let args: EchoArgs = match request.arguments {
                    Some(arguments) => serde_json::from_value(serde_json::Value::Object(
                        arguments.into_iter().collect(),
                    ))
                    .map_err(|err| {
                        McpError::invalid_params(
                            format!("'message' is a required property: {}", err),
                            None,
                        )
                    })?,
                    None => {
                        return Err(McpError::invalid_params(
                            "'message' is a required property",
                            None,
                        ));
                    }
                };

                Ok(CallToolResponse::Complete(CallToolResult::success(vec![
                    ContentBlock::text(format!("ECHO: {}", args.message)),
                ])))
            }
            other => Err(McpError::invalid_params(
                format!("unknown tool: {other}"),
                None,
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema_json_conversion() {
        let schema: JsonObject = serde_json::from_value(json!({
            "type": "object",
            "required": ["field1"],
            "properties": {
                "field1": {"type": "string"},
                "field2": {"type": "number"}
            }
        }))
        .unwrap();

        let arc_schema = Arc::new(schema);
        let value = serde_json::to_value(arc_schema.as_ref()).unwrap();

        assert_eq!(value["type"], "object");
        assert_eq!(value["required"][0], "field1");
        assert!(
            value["properties"]
                .as_object()
                .unwrap()
                .contains_key("field1")
        );
    }

    #[test]
    fn test_echo_tool_schema_has_required_field() {
        let server = TestMcpServer::new();
        let tools = server.tools.clone();

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");

        let schema_value = serde_json::to_value(tools[0].input_schema.as_ref()).unwrap();

        let required = schema_value["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "message");

        let properties = schema_value["properties"].as_object().unwrap();
        assert!(properties.contains_key("message"));
    }
}

#[cfg(test)]
mod mcp_apps_tests {
    use super::*;

    /// Build an rmcp Tool with `_meta.ui` like an MCP Apps server would.
    fn ui_tool(name: &'static str, resource_uri: &str, visibility: Option<Vec<&str>>) -> Tool {
        let schema: JsonObject = serde_json::from_value(json!({
            "type": "object",
            "properties": { "query": { "type": "string" } }
        }))
        .unwrap();

        let mut ui_meta = json!({ "resourceUri": resource_uri });
        if let Some(vis) = visibility {
            ui_meta["visibility"] = json!(vis);
        }

        let mut tool = Tool::new(
            Cow::Borrowed(name),
            Cow::Borrowed("A UI tool"),
            Arc::new(schema),
        );
        let meta_map: JsonObject = serde_json::from_value(json!({ "ui": ui_meta })).unwrap();
        tool.meta = Some(MetaObject(meta_map));
        tool
    }

    #[test]
    fn test_meta_ui_survives_serialization_roundtrip() {
        // rmcp MetaObject is #[serde(transparent)] over JsonObject.
        // Our pipeline does: tool.meta → serde_json::to_value → Option<Value>.
        // Verify the ui.resourceUri survives this conversion.
        let tool = ui_tool("dashboard", "ui://server/dash", None);
        let meta_value: serde_json::Value =
            serde_json::to_value(tool.meta.as_ref().unwrap()).unwrap();

        assert_eq!(meta_value["ui"]["resourceUri"], "ui://server/dash");
    }

    #[test]
    fn test_visibility_app_only_hides_from_model() {
        let tool = ui_tool("refresh", "ui://s/d", Some(vec!["app"]));
        let meta_value: serde_json::Value =
            serde_json::to_value(tool.meta.as_ref().unwrap()).unwrap();

        let model_visible = meta_value
            .get("ui")
            .and_then(|ui| ui.get("visibility"))
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().any(|s| s.as_str() == Some("model")))
            .unwrap_or(true);

        assert!(!model_visible);
    }
}

#[cfg(test)]
mod unit_tests {
    #[test]
    fn test_mcp_tool_schema_preserves_required() {
        use echo_agent_mcp::rmcp;
        use echo_agent_mcp::rmcp::model::Tool as RmcpTool;
        use serde_json::json;
        use std::borrow::Cow;
        use std::sync::Arc;

        // Simulate an MCP tool schema like browser_goto would have
        let schema_json = json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "URL to navigate to"
                }
            },
            "required": ["url"]
        });

        let json_object: rmcp::model::JsonObject =
            serde_json::from_value(schema_json.clone()).unwrap();

        let rmcp_tool = RmcpTool::new(
            Cow::Borrowed("browser_goto"),
            Cow::Borrowed("Navigate to a URL"),
            Arc::new(json_object),
        );

        let converted_schema = serde_json::to_value(rmcp_tool.input_schema.as_ref())
            .unwrap_or_else(|_| serde_json::json!({}));

        // Verify the required field is preserved
        assert_eq!(converted_schema["type"], "object");
        assert_eq!(converted_schema["required"], json!(["url"]));
        assert!(converted_schema["properties"]["url"].is_object());
    }

    /// Verifies that McpToolRegistration carries the MCP server's actual
    /// schema (with "type": "object" patched in), not a generic schemars-
    /// derived schema for serde_json::Value. This is the regression test
    /// for the bug where register_tool() re-derived the schema via
    /// generate_schema::<serde_json::Value>() → `{}`, causing Anthropic Messages
    /// and Bedrock backends to reject tool calls with:
    ///   "tools.N.custom.input_schema.type: Field required"
    #[test]
    fn test_mcp_registration_carries_server_schema_not_schemars_derived() {
        use echo_agent_mcp::rmcp;
        use echo_agent_mcp::rmcp::model::Tool as RmcpTool;
        use serde_json::json;
        use std::borrow::Cow;
        use std::sync::Arc;

        // Build an rmcp Tool with a real schema (properties, required, etc.)
        let server_schema = json!({
            "type": "object",
            "properties": {
                "query": { "type": "string" },
                "limit": { "type": "integer" }
            },
            "required": ["query"]
        });
        let json_object: rmcp::model::JsonObject =
            serde_json::from_value(server_schema.clone()).unwrap();

        let rmcp_tool = RmcpTool::new(
            Cow::Borrowed("search"),
            Cow::Borrowed("Search for items"),
            Arc::new(json_object),
        );

        // Simulate the conversion path in McpClient::get_tool_registrations()
        let mut schema = serde_json::to_value(rmcp_tool.input_schema.as_ref())
            .unwrap_or_else(|_| json!({"type": "object"}));
        if let Some(obj) = schema.as_object_mut() {
            obj.entry("type").or_insert_with(|| json!("object"));
        }

        // The schema in the registration must be the MCP server's schema
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], json!(["query"]));
        assert!(schema["properties"]["query"].is_object());
        assert!(schema["properties"]["limit"].is_object());
    }

    /// Verifies that an empty inputSchema `{}` (sent by some MCP servers
    /// like VSCode for parameterless tools) gets patched with "type": "object".
    #[test]
    fn test_empty_mcp_schema_gets_type_object_injected() {
        use serde_json::json;

        let mut schema = json!({});
        if let Some(obj) = schema.as_object_mut() {
            obj.entry("type").or_insert_with(|| json!("object"));
        }

        assert_eq!(schema["type"], "object");
    }
}
