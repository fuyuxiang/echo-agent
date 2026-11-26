//! Terminal-usage projection for `streaming-messages-json`: reshaping the turn's
//! aggregate ledger into `result.usage` (`message.usage` shape) and the per-model
//! `modelUsage` map. Kept apart so the token/cost/model math is self-contained.

use serde_json::{Value, json};

use crate::headless::attach_result_usage;
use crate::headless::reducer::to_line;

use super::MessagesReducer;
use super::wire::{MessageUsage, ModelUsage, ServerToolUse};

/// The reshaped terminal usage: `message.usage`, `modelUsage`, turn count, cost, and API duration.
pub(super) struct ResultUsage {
    pub(super) usage: MessageUsage,
    pub(super) model_usage: Value,
    pub(super) num_turns: u64,
    pub(super) total_cost_usd: f64,
    pub(super) duration_api_ms: u64,
}

impl MessagesReducer {
    /// The Messages `result` usage, reshaped from the shell's projection into the `message.usage` shape.
    pub(super) fn messages_result_usage(&self, end_usage: Option<&Value>) -> ResultUsage {
        let mut scratch = json!({});
        if let Some(u) = end_usage {
            attach_result_usage(&mut scratch, u);
        }
        let field = |obj: Option<&Value>, key: &str| {
            obj.and_then(|o| o.get(key))
                .and_then(Value::as_u64)
                .unwrap_or(0)
        };
        let u = scratch.get("usage");
        let usage_is_incomplete = scratch
            .get("usage_is_incomplete")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if end_usage.is_none() {
            tracing::warn!(
                "streaming-messages-json: no aggregate usage ledger at turn end; \
                 `result.usage` token counts fall back to zero (the Messages API \
                 schema has no absent-usage marker)"
            );
        } else if usage_is_incomplete {
            tracing::warn!(
                "streaming-messages-json: usage is incomplete; `result.usage` token \
                 counts may under-count or fall back to zero (the Messages API schema \
                 has no incompleteness marker)"
            );
        }
        let usage = MessageUsage {
            input_tokens: field(u, "input_tokens"),
            output_tokens: field(u, "output_tokens"),
            cache_read_input_tokens: field(u, "cache_read_input_tokens"),
            cache_creation_input_tokens: field(u, "cache_creation_input_tokens"),
            server_tool_use: Some(ServerToolUse {
                web_search_requests: self.web_search_requests,
            }),
        };
        let num_turns = scratch
            .get("num_turns")
            .and_then(Value::as_u64)
            .unwrap_or(self.completed_responses);
        let total_cost_usd = scratch
            .get("total_cost_usd")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        // `apiDurationMs` is dropped by the projection, so read it from `end_usage`.
        let duration_api_ms = end_usage.map_or(0, |u| field(Some(u), "apiDurationMs"));
        // Attribute the whole web-search count to the current model (only a global count is tracked).
        let model_usage = messages_model_usage(
            scratch.get("modelUsage"),
            self.session.as_ref().and_then(|s| s.model.as_deref()),
            self.web_search_requests,
            self.session.as_ref().and_then(|s| s.context_window),
        );
        ResultUsage {
            usage,
            model_usage,
            num_turns,
            total_cost_usd,
            duration_api_ms,
        }
    }
}

/// Map the ledger's per-model rows into `ModelUsage` entries; the web-search count
/// and `context_window` go to `current_model` only. `{}` when there is no breakdown.
pub(super) fn messages_model_usage(
    rows: Option<&Value>,
    current_model: Option<&str>,
    web_search_requests: u64,
    context_window: Option<u64>,
) -> Value {
    let Some(Value::Object(map)) = rows else {
        return json!({});
    };
    let out: serde_json::Map<String, Value> = map
        .iter()
        .map(|(model, row)| {
            let n = |k: &str| row.get(k).and_then(Value::as_u64).unwrap_or(0);
            let is_current = Some(model.as_str()) == current_model;
            (
                model.clone(),
                to_line(&ModelUsage {
                    input_tokens: n("inputTokens"),
                    output_tokens: n("outputTokens"),
                    cache_read_input_tokens: n("cacheReadInputTokens"),
                    cache_creation_input_tokens: n("cacheCreationInputTokens"),
                    web_search_requests: if is_current { web_search_requests } else { 0 },
                    cost_usd: row.get("costUSD").and_then(Value::as_f64).unwrap_or(0.0),
                    context_window: if is_current { context_window } else { None },
                }),
            )
        })
        .collect();
    Value::Object(out)
}
