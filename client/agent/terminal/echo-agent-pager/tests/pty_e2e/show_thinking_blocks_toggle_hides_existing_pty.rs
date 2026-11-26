// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

const THINKING_SENTINEL: &str = "THINKING_SENTINEL_XYZ";
const THOUGHT_HEADER: &str = "Thought for";

fn chat_completion_with_reasoning_stream(
    reasoning: &str,
    content: &str,
    model: &str,
) -> Vec<SseEvent> {
    let mut events = Vec::new();

    for word in reasoning.split_whitespace() {
        let chunk = json!({
            "id": "chatcmpl-thinking-toggle",
            "object": "chat.completion.chunk",
            "created": 1234567890,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": { "reasoning_content": format!("{word} ") },
                "finish_reason": null
            }]
        });
        events.push(SseEvent::data(chunk.to_string()));
    }

    for word in content.split_whitespace() {
        let chunk = json!({
            "id": "chatcmpl-thinking-toggle",
            "object": "chat.completion.chunk",
            "created": 1234567890,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": { "content": format!("{word} ") },
                "finish_reason": null
            }]
        });
        events.push(SseEvent::data(chunk.to_string()));
    }

    let final_chunk = json!({
        "id": "chatcmpl-thinking-toggle",
        "object": "chat.completion.chunk",
        "created": 1234567890,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 20,
            "total_tokens": 30
        }
    });
    events.push(SseEvent::data(final_chunk.to_string()));
    events.push(SseEvent::data("[DONE]"));
    events
}

fn responses_api_with_reasoning_stream(
    reasoning: &str,
    content: &str,
    model: &str,
) -> Vec<SseEvent> {
    let mut events = Vec::new();
    let mut seq = 0;

    events.push(SseEvent::data(
        json!({
            "type": "response.created",
            "sequence_number": seq,
            "response": {
                "id": "resp_thinking_toggle",
                "object": "response",
                "created_at": 1234567890,
                "model": model,
                "status": "in_progress",
                "output": []
            }
        })
        .to_string(),
    ));
    seq += 1;

    for word in reasoning.split_whitespace() {
        events.push(SseEvent::data(
            json!({
                "type": "response.reasoning_summary_text.delta",
                "sequence_number": seq,
                "item_id": "reasoning_item_1",
                "output_index": 0,
                "summary_index": 0,
                "delta": format!("{word} ")
            })
            .to_string(),
        ));
        seq += 1;
    }

    for word in content.split_whitespace() {
        events.push(SseEvent::data(
            json!({
                "type": "response.output_text.delta",
                "sequence_number": seq,
                "item_id": "msg_item_1",
                "output_index": 1,
                "content_index": 0,
                "delta": format!("{word} ")
            })
            .to_string(),
        ));
        seq += 1;
    }

    events.push(SseEvent::data(
        json!({
            "type": "response.completed",
            "sequence_number": seq,
            "response": {
                "id": "resp_thinking_toggle",
                "object": "response",
                "created_at": 1234567890,
                "model": model,
                "status": "completed",
                "output": [
                    {
                        "type": "reasoning",
                        "id": "reasoning_item_1",
                        "summary": [{
                            "type": "summary_text",
                            "text": reasoning
                        }]
                    },
                    {
                        "type": "message",
                        "id": "msg_item_1",
                        "role": "assistant",
                        "status": "completed",
                        "content": [{
                            "type": "output_text",
                            "text": content,
                            "annotations": []
                        }]
                    }
                ],
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 20,
                    "total_tokens": 30,
                    "input_tokens_details": { "cached_tokens": 0 },
                    "output_tokens_details": { "reasoning_tokens": 5 }
                }
            }
        })
        .to_string(),
    ));
    events.push(SseEvent::data("[DONE]"));
    events
}

fn wait_until_gone(harness: &mut PtyHarness, needle: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        harness.update(Duration::from_millis(150));
        if !harness.contains_text(needle) {
            return true;
        }
    }
    !harness.contains_text(needle)
}

/// F2 → filter `show thinking blocks` (unique Bool row; bare "thinking" hits
/// Max thoughts width) → Enter commit → Space toggle once → assert row value.
fn toggle_show_thinking_blocks(harness: &mut PtyHarness, want_on: bool) {
    const F2: &[u8] = b"\x1bOQ";
    if harness.contains_text("Space:prompt") {
        harness.inject_keys(b" ").expect("Space:prompt");
        harness.update(Duration::from_millis(200));
    }

    harness.inject_keys(F2).expect("F2 open settings");
    harness.update(Duration::from_millis(500));

    harness.inject_keys(b"/").expect("start filter");
    harness.update(Duration::from_millis(150));
    for ch in b"show thinking blocks" {
        harness
            .inject_keys(std::slice::from_ref(ch))
            .expect("filter char");
        harness.update(Duration::from_millis(30));
    }
    harness.update(Duration::from_millis(300));
    assert!(
        harness.contains_text("Show thinking blocks"),
        "settings filter must show Show thinking blocks row\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.inject_keys(b"\r").expect("commit filter");
    harness.update(Duration::from_millis(300));
    harness
        .inject_keys(b" ")
        .expect("toggle show_thinking_blocks");
    harness.update(Duration::from_millis(500));

    let want_label = if want_on { "on" } else { "off" };
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut saw_value = false;
    while Instant::now() < deadline {
        let screen = harness.screen_contents();
        if screen.contains("Show thinking blocks") {
            for line in screen.lines() {
                if line.contains("Show thinking blocks")
                    && line.split_whitespace().any(|w| w == want_label)
                {
                    saw_value = true;
                    break;
                }
            }
        }
        if saw_value {
            break;
        }
        harness.update(Duration::from_millis(150));
    }
    assert!(
        saw_value,
        "settings row must show Show thinking blocks = {want_label}\nscreen:\n{}",
        harness.screen_contents()
    );

    for _ in 0..4 {
        if !harness.contains_text("Show thinking blocks") && !harness.contains_text("Appearance") {
            break;
        }
        harness.inject_keys(keys::ESC).expect("esc settings");
        harness.update(Duration::from_millis(250));
    }
}

fn expand_thinking_to_show_sentinel(harness: &mut PtyHarness) {
    if harness.contains_text(THINKING_SENTINEL) {
        return;
    }
    harness.inject_keys(b"\t").expect("tab to scrollback");
    harness
        .wait_for_text("Space:prompt", Duration::from_secs(5))
        .expect("Tab moves focus to the scrollback");

    const UP: &[u8] = b"\x1b[A";
    for _ in 0..8 {
        if harness.contains_text(THINKING_SENTINEL) {
            return;
        }
        harness.inject_keys(UP).expect("Up");
        harness.update(Duration::from_millis(120));
        harness.inject_keys(b"\r").expect("Enter expand");
        harness.update(Duration::from_millis(250));
        if harness.contains_text(THINKING_SENTINEL) {
            return;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn show_thinking_blocks_toggle_hides_existing_pty() {
    let content = ContentController::start().await.expect("start content");
    // Default is off (rollout); opt in so the turn can create/show thinking first.
    seed_ui_config(&content, "show_thinking_blocks = true");
    let model = "test-model";
    let reasoning =
        format!("{THINKING_SENTINEL} reason carefully about the user prompt and list every step");
    let response_body = format!("{MOCK_RESPONSE_SENTINEL} after thinking.");

    let _thinking_turn = content.expect_agent_turn_with_responses(
        "thinking turn before visibility toggle",
        ScriptedResponse::sse(responses_api_with_reasoning_stream(
            &reasoning,
            &response_body,
            model,
        )),
        ScriptedResponse::sse(chat_completion_with_reasoning_stream(
            &reasoning,
            &response_body,
            model,
        )),
    );

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager");

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    harness
        .inject_keys(format!("{PROMPT}\r").as_bytes())
        .expect("submit prompt");

    harness
        .wait_for_text(MOCK_RESPONSE_SENTINEL, Duration::from_secs(30))
        .expect("mock response on screen");
    harness
        .wait_for_text(THOUGHT_HEADER, Duration::from_secs(10))
        .expect("collapsed thinking header on screen");
    harness.update(Duration::from_millis(300));

    expand_thinking_to_show_sentinel(&mut harness);
    assert!(
        harness.contains_text(THINKING_SENTINEL) || harness.contains_text(THOUGHT_HEADER),
        "thinking must be visible before toggle off\nscreen:\n{}",
        harness.screen_contents()
    );
    let marker = if harness.contains_text(THINKING_SENTINEL) {
        THINKING_SENTINEL
    } else {
        THOUGHT_HEADER
    };

    toggle_show_thinking_blocks(&mut harness, false);

    assert!(
        wait_until_gone(&mut harness, marker, Duration::from_secs(8)),
        "thinking marker `{marker}` must disappear after OFF\nscreen:\n{}",
        harness.screen_contents()
    );
    assert!(
        !harness.contains_text(THINKING_SENTINEL) && !harness.contains_text(THOUGHT_HEADER),
        "no thinking body or header may remain after hide\nscreen:\n{}",
        harness.screen_contents()
    );

    toggle_show_thinking_blocks(&mut harness, true);
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut restored = false;
    while Instant::now() < deadline {
        harness.update(Duration::from_millis(150));
        if harness.contains_text(THINKING_SENTINEL) || harness.contains_text(THOUGHT_HEADER) {
            restored = true;
            break;
        }
    }
    assert!(
        restored,
        "thinking must reappear after ON\nscreen:\n{}",
        harness.screen_contents()
    );

    harness.quit().expect("clean quit");
}
