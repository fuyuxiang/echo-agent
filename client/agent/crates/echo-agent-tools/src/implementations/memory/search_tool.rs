//! `memory_search` tool — new architecture (`Tool` trait).

use std::sync::Arc;

use super::types::MemorySearchInput;
use crate::types::memory_backend::MemoryBackend;
use crate::types::output::ToolOutput;
use crate::types::tool::{ToolKind, ToolNamespace};

#[path = "search_output.rs"]
mod search_output;

#[derive(Debug, Default)]
pub struct MemorySearchImpl;

impl crate::types::tool_metadata::ToolMetadata for MemorySearchImpl {
    fn kind(&self) -> ToolKind {
        ToolKind::MemorySearch
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::EchoAgentBuild
    }

    fn description_template(&self) -> &str {
        "Search cross-session memory for relevant knowledge chunks. Returns ranked results \
         from global, workspace, and session memory files.\n\n\
         Use this proactively when:\n\
         - A question references prior work, decisions, or context you don't have\n\
         - You need project conventions, coding patterns, or user preferences\n\
         - The user mentions something discussed or decided in a previous session\n\
         - Starting work in an unfamiliar part of the codebase\n\
         - After compaction when prior context may have been lost\n\n\
         A successful empty search is a valid end of recall, not a storage error. \
         For a narrow single-fact question, try at most one focused reformulation, \
         then say you do not know if no exact evidence exists. Never substitute a \
         similarly named subject or recover missing/forgotten facts through internal \
         files or unrequested conversation history."
    }
}

impl echo_agent_tool_runtime::Tool for MemorySearchImpl {
    type Args = MemorySearchInput;
    type Output = ToolOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("memory_search").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "memory_search",
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

    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: MemorySearchInput,
    ) -> Result<ToolOutput, echo_agent_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;
        let Some(memory) = resources
            .lock()
            .await
            .get::<Arc<dyn MemoryBackend>>()
            .cloned()
        else {
            return Ok(ToolOutput::Text(
                "Memory is not enabled. Use --experimental-memory to enable.".into(),
            ));
        };
        let max_results = input
            .max_results
            .unwrap_or_else(|| memory.default_search_max_results());
        let min_score = input
            .min_score
            .unwrap_or_else(|| memory.default_search_min_score());
        tracing::info!(target: crate::types::memory_backend::MEMORY_LOG_TARGET, max_results, "MEMORY_SEARCH: invoked");
        let results = memory
            .search(&input.query, max_results, min_score)
            .await
            .map_err(|e| {
                echo_agent_tool_runtime::ToolError::execution(
                    echo_agent_tool_protocol::ToolId::new("memory_search").expect("valid"),
                    format!("memory search failed: {e}"),
                )
            })?;
        tracing::info!(target: crate::types::memory_backend::MEMORY_LOG_TARGET, results = results.len(), "MEMORY_SEARCH: complete");
        Ok(ToolOutput::Text(
            search_output::format_memory_search_results(&results).into(),
        ))
    }
}
