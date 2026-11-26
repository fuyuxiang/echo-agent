//! Backend `web_search` reconciliation for `streaming-messages-json`: folding a
//! completed search inline (or the generic client split on failure), plus parsing
//! EchoAgent's `WebSearchCall` output into the wire hit array.

use agent_client_protocol as acp;
use serde_json::{Value, json};

use crate::headless::reducer::{ToolCallEvent, ToolCallUpdateEvent};

use super::MessagesReducer;
use super::wire::ContentBlock;

impl MessagesReducer {
    /// Resolve a completed backend `web_search`. A successful search folds inline and
    /// counts; a failure pairs an error result (uncounted); a non-search action falls
    /// back to the generic client split.
    pub(super) fn finish_web_search(
        &mut self,
        out: &mut Vec<Value>,
        tc: ToolCallEvent,
        u: ToolCallUpdateEvent,
    ) {
        let failed = u.status == Some(acp::ToolCallStatus::Failed);
        let (query, hits) = parse_web_search(&u.raw_output);
        let has_hits = hits.as_array().is_some_and(|a| !a.is_empty());
        if failed {
            let error = json!({
                "type": "web_search_tool_result_error",
                "error_code": "unavailable",
            });
            self.append_web_search_result(out, &u.tool_call_id, &query, &error);
            return;
        }
        if query.is_empty() && !has_hits {
            // Non-search action or unparseable output: keep the generic split, not a fake search.
            self.emit_client_tool_call(out, tc);
            self.close_and_flush(out, Some("tool_use"));
            self.buffer_tool_result(u);
            return;
        }
        self.web_search_requests += 1;
        self.append_web_search_result(out, &u.tool_call_id, &query, &hits);
    }

    /// Fold a `web_search` into the open frame as an adjacent `server_tool_use` +
    /// `web_search_tool_result` pair. The frame is not flushed, so text around the
    /// search stays in one message; the request counter is untouched.
    pub(super) fn append_web_search_result(
        &mut self,
        out: &mut Vec<Value>,
        id: &str,
        query: &str,
        content: &Value,
    ) {
        // Materialize any pending signature-only thinking block first so indices stay in sync.
        self.partial_signature_only_block(out);
        if self.include_partials() {
            self.partial_close_block(out);
        }
        self.finalize_open();
        let server_tool_use_index = self.blocks.len();
        self.blocks.push(ContentBlock::ServerToolUse {
            id: id.to_string(),
            name: "web_search",
            input: json!({ "query": query }),
        });
        let result_index = self.blocks.len();
        self.blocks.push(ContentBlock::WebSearchToolResult {
            tool_use_id: id.to_string(),
            content: content.clone(),
        });
        if self.include_partials() {
            self.partial_server_tool_use(out, server_tool_use_index, id, query);
            self.partial_web_search_result(out, result_index, id, content);
        }
    }
}

/// Parse a `web_search` `raw_output` into the query and `web_search_result` hit array.
/// EchoAgent nests query/sources under `action`, with a flat `{"query",...,"sources"}` fallback.
fn parse_web_search(raw_output: &Value) -> (String, Value) {
    let query = raw_output
        .pointer("/action/query")
        .or_else(|| raw_output.get("query"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let hits = raw_output
        .pointer("/action/sources")
        .or_else(|| raw_output.get("sources"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    let url = s.get("url").and_then(Value::as_str)?;
                    let title = s
                        .get("title")
                        .and_then(Value::as_str)
                        .filter(|t| !t.is_empty())
                        .unwrap_or(url);
                    Some(json!({
                        "type": "web_search_result",
                        "url": url,
                        "title": title,
                    }))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    (query, Value::Array(hits))
}
