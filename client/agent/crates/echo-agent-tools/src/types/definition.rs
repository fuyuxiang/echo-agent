//! Tool definition types for the model API.
//!
//! These types represent tool schemas sent to the model when tools are
//! advertised for a turn.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum ToolType {
    Function,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub kind: ToolType,
    pub function: FunctionTool,
}

impl ToolDefinition {
    pub fn function(
        name: impl Into<String>,
        description: Option<impl Into<String>>,
        parameters: Value,
    ) -> Self {
        Self {
            kind: ToolType::Function,
            function: FunctionTool {
                name: name.into(),
                description: description.map(Into::into),
                parameters,
            },
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FunctionTool {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub parameters: Value,
}
