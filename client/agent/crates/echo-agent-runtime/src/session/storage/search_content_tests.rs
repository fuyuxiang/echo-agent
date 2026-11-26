use super::*;

fn write_updates_jsonl(lines: &[String]) -> tempfile::NamedTempFile {
    use std::io::Write as _;
    let mut f = tempfile::NamedTempFile::new().unwrap();
    for line in lines {
        writeln!(f, "{line}").unwrap();
    }
    f
}

fn acp_update(session_update_json: &str) -> String {
    format!(
        r#"{{"timestamp":1,"method":"session/update","params":{{"sessionId":"s","update":{session_update_json}}}}}"#
    )
}

fn echo_agent_update(session_update_json: &str) -> String {
    format!(
        r#"{{"timestamp":1,"method":"_echo.agent/session/update","params":{{"sessionId":"s","update":{session_update_json}}}}}"#
    )
}

#[test]
fn test_single_pass_extracts_user_prompts() {
    let lines = vec![
        acp_update(
            r#"{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"hello world"}}"#,
        ),
        acp_update(
            r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hi there"}}"#,
        ),
    ];
    let f = write_updates_jsonl(&lines);
    let (content, _bytes) = collect_all_indexable_content_single_pass(f.path()).unwrap();
    assert!(
        content.contains("hello world"),
        "should contain user prompt"
    );
}

#[test]
fn test_single_pass_extracts_assistant_text() {
    let lines = vec![
        acp_update(
            r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"assistant reply"}}"#,
        ),
        acp_update(
            r#"{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"next prompt"}}"#,
        ),
    ];
    let f = write_updates_jsonl(&lines);
    let (content, _bytes) = collect_all_indexable_content_single_pass(f.path()).unwrap();
    assert!(
        content.contains("assistant reply"),
        "should contain assistant text"
    );
}

#[test]
fn test_single_pass_extracts_tool_metadata() {
    let lines = vec![acp_update(
        r#"{"sessionUpdate":"tool_call","toolCallId":"tc1","title":"Read file","kind":"read","locations":[{"path":"/tmp/foo.rs"}]}"#,
    )];
    let f = write_updates_jsonl(&lines);
    let (content, _bytes) = collect_all_indexable_content_single_pass(f.path()).unwrap();
    assert!(content.contains("Read file"), "should contain tool title");
    assert!(
        content.contains("/tmp/foo.rs"),
        "should contain tool location path"
    );
}

#[test]
fn test_single_pass_extracts_text_with_json_escapes() {
    // Escaped JSON strings cannot be borrowed as &str; a regression to
    // borrowed peek fields silently drops these messages from the index.
    let lines = vec![
        acp_update(
            r#"{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"fix the bug\nin main.rs"}}"#,
        ),
        acp_update(
            r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"use \"quotes\" and caf\u00e9"}}"#,
        ),
        acp_update(
            r#"{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"! echo \"hi\"","_meta":{"bash_command":"echo \"hi\""}}}"#,
        ),
        acp_update(
            r#"{"sessionUpdate":"tool_call","toolCallId":"tc1","title":"Run \"cargo test\"","kind":"execute","locations":[{"path":"/tmp/my\tdir/foo.rs"}]}"#,
        ),
    ];
    let f = write_updates_jsonl(&lines);
    let (content, _bytes) = collect_all_indexable_content_single_pass(f.path()).unwrap();
    assert!(
        content.contains("fix the bug\nin main.rs"),
        "multiline user prompt must be indexed: {content:?}"
    );
    assert!(
        content.contains("use \"quotes\" and caf\u{e9}"),
        "assistant text with escaped quotes and unicode escape must be indexed: {content:?}"
    );
    assert!(
        content.contains("Run \"cargo test\""),
        "tool title with escaped quotes must be indexed: {content:?}"
    );
    assert!(
        content.contains("/tmp/my\tdir/foo.rs"),
        "tool location path with escapes must be indexed: {content:?}"
    );
    assert!(
        !content.contains("echo \"hi\""),
        "escaped bash command must still be excluded from the index: {content:?}"
    );
}

#[test]
fn test_single_pass_handles_rewind() {
    let lines = vec![
        acp_update(
            r#"{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"first prompt"}}"#,
        ),
        acp_update(
            r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"first reply"}}"#,
        ),
        acp_update(
            r#"{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"second prompt"}}"#,
        ),
        acp_update(
            r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"second reply"}}"#,
        ),
        echo_agent_update(
            r#"{"sessionUpdate":"rewind_marker","target_prompt_index":1,"created_at":"2024-01-01"}"#,
        ),
        acp_update(
            r#"{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"replacement prompt"}}"#,
        ),
        acp_update(
            r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"replacement reply"}}"#,
        ),
    ];
    let f = write_updates_jsonl(&lines);
    let (content, _bytes) = collect_all_indexable_content_single_pass(f.path()).unwrap();
    assert!(
        content.contains("first prompt"),
        "first prompt should survive rewind"
    );
    assert!(
        !content.contains("second prompt"),
        "rewound prompt should be removed"
    );
    assert!(
        content.contains("replacement prompt"),
        "replacement prompt should be present"
    );
}

#[test]
fn test_single_pass_thought_chunk_does_not_flush_assistant() {
    // agent_thought_chunk interleaved between agent_message_chunk should
    // NOT break the assistant text into separate entries.
    let lines = vec![
        acp_update(
            r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hello"}}"#,
        ),
        acp_update(
            r#"{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"thinking about stuff"}}"#,
        ),
        acp_update(
            r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"world"}}"#,
        ),
        // A user message ends the assistant turn
        acp_update(
            r#"{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"thanks"}}"#,
        ),
    ];
    let f = write_updates_jsonl(&lines);
    let (content, _bytes) = collect_all_indexable_content_single_pass(f.path()).unwrap();
    // "hello" and "world" should be in the same assistant turn (not split)
    assert!(
        content.contains("hello world"),
        "thought chunk should not flush assistant text: got {content:?}"
    );
}

#[test]
fn test_single_pass_empty_file() {
    let f = write_updates_jsonl(&[]);
    let (content, bytes) = collect_all_indexable_content_single_pass(f.path()).unwrap();
    assert!(content.is_empty() || content.trim().is_empty());
    assert_eq!(bytes, 0, "empty file should report 0 bytes read");
}

#[test]
fn test_single_pass_nonexistent_file() {
    let (content, bytes) =
        collect_all_indexable_content_single_pass(Path::new("/nonexistent/updates.jsonl")).unwrap();
    assert!(content.is_empty());
    assert_eq!(bytes, 0, "nonexistent file should report 0 bytes read");
}

#[test]
fn test_single_pass_assistant_text_cap() {
    // Two 60K chunks in the same turn — the 100K assistant cap should
    // truncate the second chunk.  Total assistant text ≤ 100K.
    let big_text = "x".repeat(60_000);
    let lines = vec![
        acp_update(&format!(
            r#"{{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":"{big_text}"}}}}"#
        )),
        acp_update(&format!(
            r#"{{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":"{big_text}"}}}}"#
        )),
        // Flush the assistant turn
        acp_update(
            r#"{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"q"}}"#,
        ),
    ];
    let f = write_updates_jsonl(&lines);
    let (content, _bytes) = collect_all_indexable_content_single_pass(f.path()).unwrap();
    // Count 'x' chars — the assistant section is the only source of 'x'
    let x_count = content.chars().filter(|&c| c == 'x').count();
    assert!(
        x_count <= 100_000,
        "assistant text should be capped at 100K chars, got {x_count}"
    );
    // Must have truncated the second chunk (60K + 60K > 100K)
    assert!(
        x_count < 120_001,
        "without the cap this would be 120K, got {x_count}"
    );
    // Verify we actually collected substantial text (not accidentally empty)
    assert!(
        x_count > 50_000,
        "should have collected at least the first 60K chunk, got {x_count}"
    );
}

#[test]
fn test_single_pass_tool_call_count_cap() {
    // Generate 250 tool calls — only the first 200 should be indexed
    let lines: Vec<String> = (0..250)
        .map(|i| {
            acp_update(&format!(
                r#"{{"sessionUpdate":"tool_call","toolCallId":"tc{i}","title":"tool_{i}","kind":"exec","locations":[]}}"#
            ))
        })
        .collect();
    let f = write_updates_jsonl(&lines);
    let (content, _bytes) = collect_all_indexable_content_single_pass(f.path()).unwrap();
    // tool_200 through tool_249 should NOT appear
    assert!(
        !content.contains("tool_200"),
        "tool calls beyond 200 should be ignored"
    );
    assert!(
        !content.contains("tool_249"),
        "tool calls beyond 200 should be ignored"
    );
    // tool_0 and tool_199 should appear
    assert!(content.contains("tool_0"), "first tool should be indexed");
    assert!(
        content.contains("tool_199"),
        "tool #200 (0-indexed) should be indexed"
    );
}

#[test]
fn test_single_pass_tool_chars_cap() {
    // Generate tool calls with long titles that exceed the 100K char budget
    let long_title = "a".repeat(20_000);
    let lines: Vec<String> = (0..10)
        .map(|i| {
            acp_update(&format!(
                r#"{{"sessionUpdate":"tool_call","toolCallId":"tc{i}","title":"{long_title}","kind":"exec","locations":[]}}"#
            ))
        })
        .collect();
    let f = write_updates_jsonl(&lines);
    let (content, _bytes) = collect_all_indexable_content_single_pass(f.path()).unwrap();
    // 10 * 20K = 200K, but cap is 100K, so 'a' count should be ≤ 100K
    let a_count = content.chars().filter(|&c| c == 'a').count();
    assert!(
        a_count <= 100_000,
        "tool metadata should be capped at 100K chars, got {a_count}"
    );
    // Should have at least some tool metadata
    assert!(
        a_count > 19_000,
        "should have collected at least one tool title, got {a_count}"
    );
}

#[test]
fn test_single_pass_reports_bytes_read() {
    let lines = vec![acp_update(
        r#"{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"hello"}}"#,
    )];
    let f = write_updates_jsonl(&lines);
    let file_size = std::fs::metadata(f.path()).unwrap().len();

    let (_content, bytes_read) = collect_all_indexable_content_single_pass(f.path()).unwrap();
    assert_eq!(
        bytes_read, file_size,
        "bytes_read should match the actual file size"
    );
    assert!(
        bytes_read > 0,
        "bytes_read should be non-zero for non-empty file"
    );
}
