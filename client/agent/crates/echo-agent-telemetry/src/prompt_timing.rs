//! Per-turn prompt latency measurement.
//!
//! Extracted from `echo-agent-runtime::session::prompt_timing`.

use std::time::Instant;

use crate::events::PromptLatency;
use crate::session_ctx::log_event;

pub use crate::enums::McpInitStrategy;

pub struct PromptTiming {
    turn_start: Instant,
    mcp_wait_ms: u64,
    tool_collection_ms: u64,
    ttft_ms: Option<u64>,
    ttlb_ms: u64,
    attempts: u32,
    output_tokens: Option<u32>,
}

impl PromptTiming {
    pub fn start() -> Self {
        Self {
            turn_start: Instant::now(),
            mcp_wait_ms: 0,
            tool_collection_ms: 0,
            ttft_ms: None,
            ttlb_ms: 0,
            attempts: 1,
            output_tokens: None,
        }
    }

    pub fn record_tool_prep(&mut self, mcp_wait_ms: u64, total_prep_ms: u64) {
        self.mcp_wait_ms = mcp_wait_ms;
        self.tool_collection_ms = total_prep_ms.saturating_sub(mcp_wait_ms);
    }

    pub fn record_stream_latency(&mut self, ttft_ms: Option<u64>, ttlb_ms: u64) {
        self.ttft_ms = ttft_ms;
        self.ttlb_ms = ttlb_ms;
    }

    pub fn record_model_result(&mut self, attempts: u32, output_tokens: Option<u32>) {
        self.attempts = attempts;
        self.output_tokens = output_tokens;
    }

    pub fn emit(
        self,
        model_call_ms: u64,
        turn_index: u32,
        mcp_server_count: u32,
        mcp_tools_registered: u32,
        mcp_strategy: McpInitStrategy,
        model_id: String,
    ) {
        log_event(self.into_event(
            model_call_ms,
            turn_index,
            mcp_server_count,
            mcp_tools_registered,
            mcp_strategy,
            model_id,
        ));
    }

    fn into_event(
        self,
        model_call_ms: u64,
        turn_index: u32,
        mcp_server_count: u32,
        mcp_tools_registered: u32,
        mcp_strategy: McpInitStrategy,
        model_id: String,
    ) -> PromptLatency {
        let total_ms = self.turn_start.elapsed().as_millis() as u64;
        let pre_model_ms = total_ms.saturating_sub(model_call_ms);

        PromptLatency {
            turn_index,
            total_ms,
            mcp_wait_ms: self.mcp_wait_ms,
            tool_collection_ms: self.tool_collection_ms,
            model_call_ms,
            pre_model_ms,
            mcp_server_count,
            mcp_tools_registered,
            mcp_strategy,
            model_id,
            ttft_ms: self.ttft_ms,
            ttlb_ms: self.ttlb_ms,
            attempts: self.attempts,
            output_tokens: self.output_tokens,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_latency_omits_absent_stream_fields() {
        let v = serde_json::to_value(PromptLatency {
            turn_index: 3,
            total_ms: 5200,
            mcp_wait_ms: 120,
            tool_collection_ms: 45,
            model_call_ms: 4800,
            pre_model_ms: 400,
            mcp_server_count: 6,
            mcp_tools_registered: 42,
            mcp_strategy: McpInitStrategy::Blocking,
            model_id: "echo-agent-test".to_string(),
            ttft_ms: None,
            ttlb_ms: 4500,
            attempts: 2,
            output_tokens: None,
        })
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "turn_index": 3,
                "total_ms": 5200,
                "mcp_wait_ms": 120,
                "tool_collection_ms": 45,
                "model_call_ms": 4800,
                "pre_model_ms": 400,
                "mcp_server_count": 6,
                "mcp_tools_registered": 42,
                "mcp_strategy": "blocking",
                "model_id": "echo-agent-test",
                "ttlb_ms": 4500,
                "attempts": 2,
            })
        );
    }
}
