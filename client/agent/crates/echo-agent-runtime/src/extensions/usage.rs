//! `echo.agent/session/usage` — cumulative session token/cost as [`PromptUsage`].
//!
//! Projects the in-memory [`echo_agent_chat_state::UsageLedger`] (main-loop + folded
//! subagent spend). Partial costs are scrubbed (absence ≠ free). Totals reset
//! when a session is resumed in a new agent process.

use agent_client_protocol as acp;
use serde::{Deserialize, Serialize};

use super::{ExtResult, parse_params, to_raw_response};
use crate::agent::MvpAgent;
use crate::extensions::notification::PromptUsage;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionUsageRequest {
    session_id: String,
}

/// Wire response for `echo.agent/session/usage`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUsageResponse {
    pub usage: PromptUsage,
}

#[tracing::instrument(skip_all, fields(method = %args.method))]
pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        "echo.agent/session/usage" => handle_session_usage(agent, args).await,
        _ => Err(acp::Error::method_not_found()),
    }
}

async fn handle_session_usage(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let req: SessionUsageRequest = parse_params(args)?;
    let session_id = acp::SessionId::new(req.session_id.as_str());

    // Wait out in-flight session/load rather than racing reconnect to not-found.
    let Some(handle) = agent.session_handle_waiting_for_load(&session_id).await else {
        return Err(acp::Error::resource_not_found(Some(format!(
            "session not found: {}",
            req.session_id
        ))));
    };

    // Fail closed: a dead chat-state actor is an error, never a zero bill.
    let ledger = handle
        .chat_state_handle
        .try_get_session_usage()
        .await
        .map_err(|()| acp::Error::internal_error().data("failed to read session usage"))?;

    to_raw_response(&SessionUsageResponse {
        usage: PromptUsage::from(&ledger),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_chat_state::UsageLedger;
    use echo_agent_sampling_types::TokenUsage;

    fn usage(prompt: u32, completion: u32) -> TokenUsage {
        TokenUsage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: 0,
            reasoning_tokens: 0,
            cached_prompt_tokens: 0,
            cache_creation_prompt_tokens: 0,
        }
    }

    #[test]
    fn response_serializes_ledger_as_prompt_usage_wire_shape() {
        let mut ledger = UsageLedger::default();
        ledger.record_main_loop_call(
            "echo-agent-build",
            &usage(100, 10),
            Some(50),
            Some(20_000_000),
        );
        let v = serde_json::to_value(&SessionUsageResponse {
            usage: PromptUsage::from(&ledger),
        })
        .unwrap();
        assert_eq!(v["usage"]["inputTokens"], 100);
        assert_eq!(v["usage"]["outputTokens"], 10);
        assert_eq!(v["usage"]["numTurns"], 1);
        assert_eq!(v["usage"]["costUsdTicks"], 20_000_000);
        assert_eq!(
            v["usage"]["modelUsage"]["echo-agent-build"]["inputTokens"],
            100
        );
        let rt: SessionUsageResponse = serde_json::from_value(v).unwrap();
        assert_eq!(rt.usage.totals.cost_usd_ticks, Some(20_000_000));
    }

    #[test]
    fn response_scrubs_partial_costs() {
        let mut ledger = UsageLedger::default();
        ledger.record_main_loop_call("a", &usage(100, 10), None, Some(70));
        ledger.record_main_loop_call("a", &usage(50, 5), None, None);
        let v = serde_json::to_value(&SessionUsageResponse {
            usage: PromptUsage::from(&ledger),
        })
        .unwrap();
        assert_eq!(v["usage"]["costUsdTicks"], serde_json::Value::Null);
        assert_eq!(v["usage"]["costIsPartial"], true);
    }
}
