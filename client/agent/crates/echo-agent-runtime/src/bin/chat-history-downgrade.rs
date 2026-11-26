//! normalize chat_history.jsonl, convert any v1 (ConversationItem) to v0 (ChatRequestMessage) format.
//! Used for data processing pipeline.
//!
//! Usage:
//!   chat-history-downgrade <INPUT> <OUTPUT>
//!
//! ## Reasoning-shape compatibility
//!
//! Two on-disk v1 shapes carry reasoning, both must be downgraded into
//! the v0 `reasoning_content: Option<String>` field:
//!
//! 1. **Legacy shape** -- reasoning lived as a field on the
//!    assistant item itself: `{"type":"assistant","reasoning":{"text":...},...}`.
//!    Post-refactor `AssistantItem` no longer has that field, so serde
//!    silently drops it on deserialize. We pre-extract it from the raw
//!    JSON before parsing as `ConversationItem`.
//!
//! 2. **Current shape** -- reasoning is a sibling
//!    `ConversationItem::Reasoning(rs::ReasoningItem)` item that precedes
//!    the assistant in the JSONL stream. The downgrade buffers these
//!    sibling lines and folds their text into the next assistant's
//!    `reasoning_content`, matching what `conversation_to_chat_messages`
//!    does at the in-process layer. Intervening user / tool messages
//!    clear the buffer.

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;

use clap::Parser;
use echo_agent_runtime::sampling::conversation::{
    ConversationItem, conversation_item_to_chat_message,
};
use echo_agent_runtime::sampling::types::{ChatRequestMessage, Role};

/// normalize chat_history.jsonl, convert any v1 (ConversationItem) to v0 (ChatRequestMessage) format.
#[derive(Parser)]
#[command(name = "chat-history-downgrade")]
struct Args {
    /// Input v1 chat_history.jsonl file
    input: PathBuf,
    /// Output v0 chat_history.jsonl file
    output: PathBuf,
}

/// Convert one v1 JSONL line to a v0 `ChatRequestMessage`, threading
/// `pending_reasoning` across calls so sibling `Reasoning` items are
/// folded into the following assistant.
///
/// Returns:
/// - `Ok(Some(msg))` -- emit this v0 message.
/// - `Ok(None)` -- line was a sibling `Reasoning` item; its text has been
///   buffered into `pending_reasoning` for the next assistant. Skip emit.
/// - `Err(_)` -- line was neither v1 nor v0 parseable.
fn convert_line(
    trimmed: &str,
    pending_reasoning: &mut Vec<String>,
) -> anyhow::Result<Option<ChatRequestMessage>> {
    // Inspect the raw JSON first so we can:
    //   (a) extract a legacy `assistant.reasoning.text` field before
    //       strongly-typed parsing drops it, and
    //   (b) buffer sibling `Reasoning` lines.
    let raw: serde_json::Value = serde_json::from_str(trimmed)
        .map_err(|e| anyhow::anyhow!("line is not valid JSON: {e}"))?;
    let item_type = raw.get("type").and_then(|t| t.as_str());

    // (b) Sibling Reasoning: extract text and buffer for the next assistant.
    if item_type == Some("reasoning") {
        if let Some(text) = extract_reasoning_text(&raw) {
            pending_reasoning.push(text);
        }
        return Ok(None);
    }

    // (a) Legacy reasoning field on the assistant item.
    // Tries `reasoning.text` first (chat-completions-style) then
    // `reasoning.encrypted` (responses-API-style); the latter is opaque
    // bytes so we surface it as a placeholder rather than dropping it
    // silently. Real text wins if both are present.
    let legacy_reasoning: Option<String> = if item_type == Some("assistant") {
        raw.get("reasoning").and_then(|r| {
            r.get("text")
                .and_then(|t| t.as_str())
                .map(String::from)
                .or_else(|| {
                    r.get("encrypted")
                        .and_then(|t| t.as_str())
                        .map(|_| "[encrypted reasoning]".to_string())
                })
        })
    } else {
        None
    };

    // Parse as v1, fall back to v0 passthrough.
    let mut chat_msg: ChatRequestMessage =
        match serde_json::from_value::<ConversationItem>(raw.clone()) {
            Ok(item) => conversation_item_to_chat_message(item),
            Err(v1_err) => serde_json::from_value::<ChatRequestMessage>(raw)
                .map_err(|_| anyhow::anyhow!("failed to parse as v1 or v0: {v1_err}"))?,
        };

    // Attach reasoning to assistant messages, with the legacy field
    // taking precedence when both sources exist.
    if let Some(text) = legacy_reasoning {
        chat_msg.reasoning_content = Some(text);
        pending_reasoning.clear();
    } else if matches!(chat_msg.role, Role::Assistant) && !pending_reasoning.is_empty() {
        chat_msg.reasoning_content = Some(pending_reasoning.join("\n"));
        pending_reasoning.clear();
    } else if !matches!(chat_msg.role, Role::Assistant) {
        // Intervening user / tool message clears pending reasoning --
        // matches `conversation_to_chat_messages` (attaches to the
        // immediately-following assistant only).
        pending_reasoning.clear();
    }

    Ok(Some(chat_msg))
}

/// Extract joined reasoning text from a sibling `Reasoning` JSON value.
/// Joins `summary[].text` and `content[].text` blocks.
fn extract_reasoning_text(raw: &serde_json::Value) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(summary) = raw.get("summary").and_then(|s| s.as_array()) {
        for sp in summary {
            if let Some(t) = sp.get("text").and_then(|t| t.as_str())
                && !t.is_empty()
            {
                parts.push(t.to_string());
            }
        }
    }
    if let Some(content) = raw.get("content").and_then(|c| c.as_array()) {
        for cp in content {
            if let Some(t) = cp.get("text").and_then(|t| t.as_str())
                && !t.is_empty()
            {
                parts.push(t.to_string());
            }
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let reader = BufReader::new(File::open(&args.input)?);
    let mut writer = BufWriter::new(File::create(&args.output)?);

    let mut converted = 0usize;
    let mut pending_reasoning: Vec<String> = Vec::new();

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let Some(chat_msg) = convert_line(trimmed, &mut pending_reasoning)? else {
            // Sibling Reasoning item: buffered, no v0 line to emit.
            continue;
        };

        serde_json::to_writer(&mut writer, &chat_msg)?;
        writer.write_all(b"\n")?;
        converted += 1;
    }

    writer.flush()?;
    eprintln!("Done: {converted} messages converted");
    Ok(())
}

// ============================================================================
// Tests — hardcoded JSON fixtures so schema changes in either
// ConversationItem (v1) or ChatRequestMessage (v0) will break these.
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a single v1 JSON line, convert to v0, re-serialize, and
    /// re-parse as v0. Returns the v0 JSON string for further assertions.
    /// Uses a fresh empty `pending_reasoning` buffer so single-line tests
    /// stay self-contained.
    fn convert_line_for_test(v1_json: &str) -> String {
        let mut pending = Vec::new();
        let v0 = convert_line(v1_json, &mut pending)
            .expect("convert_line should succeed")
            .expect("v1 line should produce a v0 message (not a buffered Reasoning)");
        let out = serde_json::to_string(&v0).expect("v0 should serialize");
        // Verify the output is valid v0
        let _: ChatRequestMessage =
            serde_json::from_str(&out).expect("v0 output should round-trip");
        out
    }

    fn v0_value(json: &str) -> serde_json::Value {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn test_system_message() {
        let v1 = r#"{"type":"system","content":"You are a helpful assistant."}"#;
        let out = convert_line_for_test(v1);
        let v = v0_value(&out);
        assert_eq!(v["role"], "system");
        assert_eq!(v["content"], "You are a helpful assistant.");
    }

    #[test]
    fn test_user_text_message() {
        let v1 = r#"{"type":"user","content":[{"type":"text","text":"Hello!"}]}"#;
        let out = convert_line_for_test(v1);
        let v = v0_value(&out);
        assert_eq!(v["role"], "user");
        // v0 content must be a plain string, not an array of blocks
        assert_eq!(v["content"], "Hello!");
    }

    #[test]
    fn test_user_with_image() {
        let v1 = r#"{"type":"user","content":[{"type":"text","text":"Look at this"},{"type":"image","url":"https://example.com/img.png"}]}"#;
        let out = convert_line_for_test(v1);
        let v = v0_value(&out);
        assert_eq!(v["role"], "user");
        let blocks = v["content"].as_array().expect("content should be array");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[1]["image_url"]["url"], "https://example.com/img.png");
    }

    #[test]
    fn test_assistant_simple() {
        let v1 = r#"{"type":"assistant","content":"Hi there!","tool_calls":[]}"#;
        let out = convert_line_for_test(v1);
        let v = v0_value(&out);
        assert_eq!(v["role"], "assistant");
        assert_eq!(v["content"], "Hi there!");
    }

    /// Legacy shape: `reasoning` was a field on the
    /// assistant item. Post-refactor the field doesn't exist on
    /// `AssistantItem`, so serde would silently drop it. We pre-extract
    /// it from the raw JSON to preserve downstream data fidelity.
    #[test]
    fn test_assistant_with_reasoning() {
        let v1 = r#"{"type":"assistant","content":"The answer is 42.","reasoning":{"text":"Let me think..."},"tool_calls":[],"model_id":"echo-agent-3"}"#;
        let out = convert_line_for_test(v1);
        let v = v0_value(&out);
        assert_eq!(v["role"], "assistant");
        assert_eq!(v["content"], "The answer is 42.");
        assert_eq!(v["reasoning_content"], "Let me think...");
        assert_eq!(v["model_id"], "echo-agent-3");
    }

    /// Current shape: reasoning is a sibling line
    /// before the assistant. The downgrade buffers it and folds the
    /// text into the next assistant's `reasoning_content`.
    #[test]
    fn test_sibling_reasoning_folds_into_following_assistant() {
        let mut pending = Vec::new();

        let r_line = r#"{"type":"reasoning","id":"rs_abc","summary":[{"type":"summary_text","text":"Let me think..."}]}"#;
        let r = convert_line(r_line, &mut pending).unwrap();
        assert!(
            r.is_none(),
            "sibling Reasoning line must be buffered, not emitted"
        );
        assert_eq!(pending.len(), 1);

        let a_line = r#"{"type":"assistant","content":"The answer is 42.","tool_calls":[],"model_id":"echo-agent-3"}"#;
        let a = convert_line(a_line, &mut pending)
            .unwrap()
            .expect("assistant line produces a v0 message");
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
        assert_eq!(v["role"], "assistant");
        assert_eq!(v["content"], "The answer is 42.");
        assert_eq!(v["reasoning_content"], "Let me think...");
        assert!(pending.is_empty(), "buffer must be flushed after attaching");
    }

    /// Multiple sibling Reasoning lines before one assistant get joined
    /// with newlines (matches `conversation_to_chat_messages`).
    #[test]
    fn test_multiple_sibling_reasoning_joined_into_one_assistant() {
        let mut pending = Vec::new();

        for (id, text) in &[("rs_1", "first"), ("rs_2", "second"), ("rs_3", "third")] {
            let r_line = format!(
                r#"{{"type":"reasoning","id":"{id}","summary":[{{"type":"summary_text","text":"{text}"}}]}}"#
            );
            let r = convert_line(&r_line, &mut pending).unwrap();
            assert!(r.is_none());
        }
        assert_eq!(pending.len(), 3);

        let a_line = r#"{"type":"assistant","content":"ok","tool_calls":[]}"#;
        let a = convert_line(a_line, &mut pending).unwrap().unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
        assert_eq!(v["reasoning_content"], "first\nsecond\nthird");
        assert!(pending.is_empty());
    }

    /// An intervening user / tool message clears pending reasoning --
    /// matches the `conversation_to_chat_messages` semantic where
    /// reasoning attaches only to the immediately-following assistant.
    #[test]
    fn test_intervening_user_clears_pending_reasoning() {
        let mut pending = Vec::new();

        let r_line = r#"{"type":"reasoning","id":"rs_orphan","summary":[{"type":"summary_text","text":"orphan"}]}"#;
        convert_line(r_line, &mut pending).unwrap();
        assert_eq!(pending.len(), 1);

        let u_line = r#"{"type":"user","content":[{"type":"text","text":"new turn"}]}"#;
        convert_line(u_line, &mut pending).unwrap();
        assert!(
            pending.is_empty(),
            "user message must clear pending reasoning"
        );

        let a_line = r#"{"type":"assistant","content":"ok","tool_calls":[]}"#;
        let a = convert_line(a_line, &mut pending).unwrap().unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
        assert!(
            v.get("reasoning_content").is_none() || v["reasoning_content"].is_null(),
            "orphan reasoning must not attach to assistant across a user turn"
        );
    }

    /// Legacy assistant.reasoning field wins over buffered sibling
    /// reasoning -- the explicit per-assistant field is the more
    /// specific signal.
    #[test]
    fn test_legacy_field_overrides_buffered_sibling() {
        let mut pending = Vec::new();

        let r_line = r#"{"type":"reasoning","id":"rs_sib","summary":[{"type":"summary_text","text":"from sibling"}]}"#;
        convert_line(r_line, &mut pending).unwrap();

        let a_line = r#"{"type":"assistant","content":"ok","reasoning":{"text":"from legacy field"},"tool_calls":[]}"#;
        let a = convert_line(a_line, &mut pending).unwrap().unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
        assert_eq!(v["reasoning_content"], "from legacy field");
        assert!(pending.is_empty());
    }

    #[test]
    fn test_assistant_with_tool_calls() {
        let v1 = r#"{"type":"assistant","content":"","tool_calls":[{"id":"call_1","name":"bash","arguments":"{\"command\":\"ls\"}"}]}"#;
        let out = convert_line_for_test(v1);
        let v = v0_value(&out);
        assert_eq!(v["role"], "assistant");
        let tc = &v["tool_calls"][0];
        assert_eq!(tc["id"], "call_1");
        assert_eq!(tc["function"]["name"], "bash");
        assert_eq!(tc["function"]["arguments"], r#"{"command":"ls"}"#);
    }

    #[test]
    fn test_tool_result() {
        let v1 =
            r#"{"type":"tool_result","tool_call_id":"call_1","content":"file1.txt\nfile2.txt"}"#;
        let out = convert_line_for_test(v1);
        let v = v0_value(&out);
        assert_eq!(v["role"], "tool");
        assert_eq!(v["tool_call_id"], "call_1");
        assert_eq!(v["content"], "file1.txt\nfile2.txt");
    }

    #[test]
    fn test_v0_passthrough() {
        // Already v0 — should pass through without error
        let v0_input = r#"{"role":"system","content":"Hello"}"#;
        let parsed: ChatRequestMessage =
            serde_json::from_str(v0_input).expect("v0 fixture should parse as ChatRequestMessage");
        let out = serde_json::to_string(&parsed).unwrap();
        let v = v0_value(&out);
        assert_eq!(v["role"], "system");
    }

    #[test]
    fn test_invalid_json_fails() {
        let bad = r#"{"type":"unknown_variant","foo":"bar"}"#;
        let v1_result = serde_json::from_str::<ConversationItem>(bad);
        let v0_result = serde_json::from_str::<ChatRequestMessage>(bad);
        assert!(v1_result.is_err());
        assert!(v0_result.is_err());
    }

    #[test]
    fn test_full_file_roundtrip() {
        let v1_lines = [
            r#"{"type":"system","content":"System prompt"}"#,
            r#"{"type":"user","content":[{"type":"text","text":"Hello"}]}"#,
            r#"{"type":"assistant","content":"Hi!","tool_calls":[]}"#,
            r#"{"type":"assistant","content":"","tool_calls":[{"id":"c1","name":"bash","arguments":"{}"}]}"#,
            r#"{"type":"tool_result","tool_call_id":"c1","content":"ok"}"#,
        ];

        let dir = tempfile::tempdir().unwrap();
        let input_path = dir.path().join("input.jsonl");
        let output_path = dir.path().join("output.jsonl");

        // Write v1 input
        std::fs::write(&input_path, v1_lines.join("\n") + "\n").unwrap();

        // Run the conversion logic using the same `convert_line` the
        // binary's main loop uses, so the test exercises the real path.
        let reader = BufReader::new(File::open(&input_path).unwrap());
        let mut writer = BufWriter::new(File::create(&output_path).unwrap());
        let mut count = 0;
        let mut pending = Vec::new();
        for line in reader.lines() {
            let line = line.unwrap();
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Some(v0) = convert_line(trimmed, &mut pending).unwrap() else {
                continue;
            };
            serde_json::to_writer(&mut writer, &v0).unwrap();
            writer.write_all(b"\n").unwrap();
            count += 1;
        }
        writer.flush().unwrap();
        assert_eq!(count, 5);

        // Verify every output line parses as v0
        let output = std::fs::read_to_string(&output_path).unwrap();
        for line in output.lines() {
            let _: ChatRequestMessage = serde_json::from_str(line)
                .expect("each output line should be valid v0 ChatRequestMessage");
        }
    }

    /// Full-file round trip with the new-shape sibling Reasoning lines
    /// interleaved between turns. Verifies the binary's buffered
    /// extraction folds correctly across the whole stream.
    #[test]
    fn test_full_file_roundtrip_with_sibling_reasoning() {
        let v1_lines = [
            r#"{"type":"system","content":"sys"}"#,
            r#"{"type":"user","content":[{"type":"text","text":"q1"}]}"#,
            r#"{"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":"think 1"}]}"#,
            r#"{"type":"assistant","content":"a1","tool_calls":[]}"#,
            r#"{"type":"user","content":[{"type":"text","text":"q2"}]}"#,
            r#"{"type":"reasoning","id":"rs_2","summary":[{"type":"summary_text","text":"think 2"}]}"#,
            r#"{"type":"assistant","content":"a2","tool_calls":[]}"#,
        ];

        let dir = tempfile::tempdir().unwrap();
        let input_path = dir.path().join("input.jsonl");
        let output_path = dir.path().join("output.jsonl");
        std::fs::write(&input_path, v1_lines.join("\n") + "\n").unwrap();

        let reader = BufReader::new(File::open(&input_path).unwrap());
        let mut writer = BufWriter::new(File::create(&output_path).unwrap());
        let mut pending = Vec::new();
        let mut count = 0;
        for line in reader.lines() {
            let line = line.unwrap();
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(v0) = convert_line(trimmed, &mut pending).unwrap() {
                serde_json::to_writer(&mut writer, &v0).unwrap();
                writer.write_all(b"\n").unwrap();
                count += 1;
            }
        }
        writer.flush().unwrap();

        // 5 emitted lines: sys, q1, a1, q2, a2 (two reasoning lines folded in).
        assert_eq!(count, 5);

        let output = std::fs::read_to_string(&output_path).unwrap();
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 5);

        let a1: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(a1["role"], "assistant");
        assert_eq!(a1["content"], "a1");
        assert_eq!(a1["reasoning_content"], "think 1");

        let a2: serde_json::Value = serde_json::from_str(lines[4]).unwrap();
        assert_eq!(a2["role"], "assistant");
        assert_eq!(a2["content"], "a2");
        assert_eq!(a2["reasoning_content"], "think 2");
    }
}
