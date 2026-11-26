use super::*;

/// Regression test: exact incident that caused kimi-k2.5 / OpenRouter
/// sessions to fail with 400 errors on every retry.
///
/// The model produced malformed JSON (missing `"` before `new_string`).
/// The error message must include:
///   1. The original broken arguments (up to MAX_ARGS_IN_ERROR chars) so
///      the model can fix the one-character syntax error directly.
///   2. The JSON parse error with the exact char position.
#[test]
fn test_malformed_json_includes_original_args_and_position() {
    let bad_args = r#"{"file_path": "/testbed/cxx_polynomial/include/emsr/remez.h", "old_string": "", new_string": "content"}"#;
    // bad_args is ~100 chars, well under MAX_ARGS_IN_ERROR.
    let err: echo_agent_tool_runtime::ToolError =
        serde_json::from_str::<serde_json::Value>(bad_args)
            .unwrap_err()
            .into();

    let msg = build_tool_parse_error_message("search_replace", &err, bad_args);

    // Must contain the original arguments.
    assert!(
        msg.contains(bad_args),
        "error message must contain original arguments; got:\n{msg}"
    );
    // Must flag that the arguments contain invalid JSON.
    assert!(
        msg.contains("invalid JSON"),
        "error message must mention invalid JSON; got:\n{msg}"
    );
    // Must include the char-position hint from serde_json (line 1 column 81 / char 80).
    assert!(
        msg.contains("column 81") || msg.contains("char 80"),
        "error message must include the parse-error position; got:\n{msg}"
    );
    // Must tell the model to fix and retry.
    assert!(
        msg.contains("fix") || msg.contains("retry"),
        "error message must guide the model to fix and retry; got:\n{msg}"
    );
}

/// Valid JSON arguments must NOT trigger the "invalid JSON" note.
#[test]
fn test_valid_json_no_invalid_json_note() {
    let good_args = r#"{"file_path": "/foo.rs", "old_string": "a", "new_string": "b"}"#;
    // A deserialization error (missing required field), not a parse error.
    let err = echo_agent_tool_runtime::ToolError::invalid_arguments(
        "missing field `old_string`".to_string(),
    );

    let msg = build_tool_parse_error_message("search_replace", &err, good_args);

    assert!(
        msg.contains(good_args),
        "error message must contain original arguments"
    );
    assert!(
        !msg.contains("invalid JSON"),
        "valid JSON must not trigger invalid-JSON note; got:\n{msg}"
    );
}

/// Empty arguments must not panic and must not add noise.
#[test]
fn test_empty_arguments_no_extra_content() {
    let err = echo_agent_tool_runtime::ToolError::invalid_arguments(
        "missing field `file_path`".to_string(),
    );
    let msg = build_tool_parse_error_message("search_replace", &err, "");

    assert!(msg.contains("Failed to parse arguments for tool `search_replace`"));
    assert!(!msg.contains("Your original arguments"));
    assert!(!msg.contains("invalid JSON"));
}

#[test]
fn test_missing_tool_name_has_actionable_provider_error() {
    let err = echo_agent_tool_runtime::ToolError::not_found(
        echo_agent_tool_protocol::ToolId::new("unknown").unwrap(),
        "Tool not found: ".to_string(),
    );
    let args = r#"{"target_file":"README.md"}"#;

    let msg = build_tool_parse_error_message("", &err, args);

    assert!(msg.contains("without a function name"));
    assert!(msg.contains("advertised tool names"));
    assert!(msg.contains(args));
    assert!(!msg.contains("tool ``"));
}

/// Arguments longer than MAX_ARGS_IN_ERROR must be truncated with a marker.
#[test]
fn test_long_arguments_are_truncated() {
    // Build an argument string longer than MAX_ARGS_IN_ERROR.
    let long_value = "x".repeat(MAX_ARGS_IN_ERROR + 500);
    let long_args = format!(r#"{{"key": "{long_value}"}}"#);
    assert!(long_args.len() > MAX_ARGS_IN_ERROR);

    let err = echo_agent_tool_runtime::ToolError::invalid_arguments(
        "missing field `file_path`".to_string(),
    );
    let msg = build_tool_parse_error_message("search_replace", &err, &long_args);

    // The message must be capped — the full args must NOT appear verbatim.
    assert!(
        !msg.contains(&long_args),
        "long arguments must be truncated; message length: {}",
        msg.len()
    );
    // A truncation marker must be present.
    assert!(
        msg.contains("(truncated)"),
        "truncation marker must appear in message"
    );
    // Use truncate_bytes (not raw byte slice) for the expected prefix check.
    let expected_prefix = truncate_bytes(&long_args, MAX_ARGS_IN_ERROR);
    assert!(
        msg.contains(expected_prefix),
        "first {MAX_ARGS_IN_ERROR} bytes of args must appear in message"
    );
}

/// truncate_bytes must not panic on a multi-byte char boundary.
/// Non-ASCII tool arguments (CJK paths, accented strings, emoji) are common.
#[test]
fn test_truncate_bytes_non_ascii() {
    // "日本語" is 9 bytes (3 chars × 3 bytes each).
    // Truncating at 5 would land in the middle of the second char — must walk back.
    let s = "日本語";
    assert_eq!(s.len(), 9);
    let t = truncate_bytes(s, 5);
    assert!(
        s.is_char_boundary(t.len()),
        "result must end on a char boundary"
    );
    assert_eq!(t, "日"); // only the first char (3 bytes) fits before byte 5

    // Exact boundary is fine.
    assert_eq!(truncate_bytes(s, 3), "日");
    // Longer than string returns the whole string.
    assert_eq!(truncate_bytes(s, 100), s);
    // Zero max returns empty.
    assert_eq!(truncate_bytes(s, 0), "");
}

/// build_tool_parse_error_message must not panic when raw_arguments contains
/// non-ASCII characters and the byte limit falls mid-char.
#[test]
fn test_non_ascii_arguments_truncated_safely() {
    // Construct args where MAX_ARGS_IN_ERROR bytes falls inside a multi-byte char.
    // Each '文' is 3 bytes; fill to just past the boundary.
    let filler = "文".repeat(MAX_ARGS_IN_ERROR / 3 + 1);
    let long_args = format!(r#"{{"old_string": "{filler}"}}"#);
    assert!(long_args.len() > MAX_ARGS_IN_ERROR);

    let err = echo_agent_tool_runtime::ToolError::invalid_arguments("missing field".to_string());
    // Must not panic.
    let msg = build_tool_parse_error_message("search_replace", &err, &long_args);
    assert!(msg.contains("(truncated)"));
    // The prefix in the message must be valid UTF-8 (implicit — String is always UTF-8).
    assert!(!msg.is_empty());
}
