use super::{PromptResponseMetaArgs, build_prompt_response_meta};
use echo_agent_sampling_types::TokenUsage;

/// Baseline args with no usage, cancellation, or structured output.
fn args<'a>(
    session_id: &'a str,
    prompt_id: &'a str,
    total_tokens: u64,
    model_id: &'a str,
) -> PromptResponseMetaArgs<'a> {
    PromptResponseMetaArgs {
        session_id,
        tool_overrides: None,
        prompt_id,
        total_tokens,
        model_id,
        last_turn_usage: None,
        prompt_usage: None,
        cancellation_category: None,
        cancel_trigger: None,
        structured_output: None,
    }
}

#[test]
fn includes_baseline_keys_without_usage() {
    let meta = build_prompt_response_meta(args("sess-1", "prompt-1", 42_000, "echo-agent-4.5"));
    assert_eq!(meta["sessionId"], "sess-1");
    assert_eq!(meta["requestId"], "prompt-1");
    assert_eq!(meta["promptId"], "prompt-1");
    assert_eq!(meta["totalTokens"], 42_000);
    assert_eq!(meta["modelId"], "echo-agent-4.5");
    // No per-turn keys when usage is absent.
    assert!(meta.get("inputTokens").is_none());
    assert!(meta.get("outputTokens").is_none());
    assert!(meta.get("cachedReadTokens").is_none());
}

#[test]
fn enriches_meta_with_camelcase_token_keys() {
    let usage = TokenUsage {
        prompt_tokens: 1500,
        completion_tokens: 200,
        total_tokens: 1700,
        reasoning_tokens: 75,
        cached_prompt_tokens: 1000,
        cache_creation_prompt_tokens: 0,
    };
    let meta = build_prompt_response_meta(PromptResponseMetaArgs {
        last_turn_usage: Some(&usage),
        ..args("sess-1", "prompt-1", 1_700, "echo-agent-4.5")
    });
    // Bot's _META_TOKEN_KEY_MAP expects exactly these camelCase keys.
    assert_eq!(meta["inputTokens"], 1500);
    assert_eq!(meta["outputTokens"], 200);
    assert_eq!(meta["cachedReadTokens"], 1000);
    // Reasoning tokens carried through for diagnostic visibility.
    assert_eq!(meta["reasoningTokens"], 75);
}

#[test]
fn preserves_zero_token_values() {
    // Responses API hits with no cache return cached_prompt_tokens=0.
    // The key is still emitted as 0 so the bot can distinguish "no cache
    // hit" from "no usage data". (The bot's _merge_meta_usage requires
    // the key to be present and integer-typed.)
    let usage = TokenUsage {
        prompt_tokens: 100,
        completion_tokens: 10,
        total_tokens: 110,
        reasoning_tokens: 0,
        cached_prompt_tokens: 0,
        cache_creation_prompt_tokens: 0,
    };
    let meta = build_prompt_response_meta(PromptResponseMetaArgs {
        last_turn_usage: Some(&usage),
        ..args("s", "p", 110, "m")
    });
    assert_eq!(meta["cachedReadTokens"], 0);
    assert_eq!(meta["reasoningTokens"], 0);
}

#[test]
fn usage_object_lands_on_meta() {
    let mut ledger = echo_agent_chat_state::UsageLedger::default();
    ledger.record_main_loop_call(
        "m",
        &TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 10,
            total_tokens: 999_999,
            reasoning_tokens: 0,
            cached_prompt_tokens: 0,
            cache_creation_prompt_tokens: 0,
        },
        None,
        None,
    );
    let meta = build_prompt_response_meta(PromptResponseMetaArgs {
        prompt_usage: Some(crate::extensions::notification::PromptUsage::from(&ledger)),
        ..args("s", "p", 110, "m")
    });
    assert_eq!(meta["usage"]["totalTokens"], 110);
    assert_eq!(meta["usage"]["modelUsage"]["m"]["inputTokens"], 100);
    assert!(
        build_prompt_response_meta(args("s", "p", 0, "m"))
            .get("usage")
            .is_none()
    );
}

#[test]
fn cancel_trigger_lands_as_camelcase_meta_key() {
    // A send-now cancelled turn's PromptResponse `_meta` carries `cancelTrigger: "send_now"`.
    let meta = build_prompt_response_meta(PromptResponseMetaArgs {
        cancel_trigger: Some("send_now".to_string()),
        ..args("s", "p", 0, "m")
    });
    assert_eq!(meta["cancelTrigger"], "send_now");

    // Absent for non-cancel completions — the key must not appear.
    let none = build_prompt_response_meta(args("s", "p", 0, "m"));
    assert!(none.get("cancelTrigger").is_none());
}

#[test]
fn tool_overrides_land_as_camelcase_meta_key() {
    let overrides = echo_agent_sampling_types::ToolOverrides {
        x_search: Some(echo_agent_sampling_types::XSearchOptions {
            date_bound: Some(
                echo_agent_sampling_types::SearchDateBound::new(None, Some("2024-03-15".to_string()))
                    .unwrap(),
            ),
        }),
        web_search: None,
    };
    let meta = build_prompt_response_meta(PromptResponseMetaArgs {
        tool_overrides: Some(overrides),
        ..args("s", "p", 0, "m")
    });
    assert_eq!(
        meta["toolOverrides"]["xSearch"]["dateBound"]["toDate"],
        "2024-03-15"
    );
    assert!(meta["toolOverrides"].get("webSearch").is_none());

    let none = build_prompt_response_meta(args("s", "p", 0, "m"));
    assert!(none.get("toolOverrides").is_none());
}

#[test]
fn structured_output_maps_to_camelcase_meta_keys() {
    // Success carries the validated value under `structuredOutput`; no error key.
    let ok = build_prompt_response_meta(PromptResponseMetaArgs {
        structured_output: Some(Ok(serde_json::json!({"name": "ada"}))),
        ..args("s", "p", 0, "m")
    });
    assert_eq!(ok["structuredOutput"]["name"], "ada");
    assert!(ok.get("structuredOutputError").is_none());

    // Failure carries the message under `structuredOutputError`; no value key.
    let err = build_prompt_response_meta(PromptResponseMetaArgs {
        structured_output: Some(Err("output does not match the required schema".to_string())),
        ..args("s", "p", 0, "m")
    });
    assert_eq!(
        err["structuredOutputError"],
        "output does not match the required schema"
    );
    assert!(err.get("structuredOutput").is_none());

    // No schema requested → neither key present.
    let none = build_prompt_response_meta(args("s", "p", 0, "m"));
    assert!(none.get("structuredOutput").is_none());
    assert!(none.get("structuredOutputError").is_none());
}
