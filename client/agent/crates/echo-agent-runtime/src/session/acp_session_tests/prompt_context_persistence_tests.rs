use super::support::create_test_actor;
use super::*;

/// Test that PromptContext can round-trip through JSON serialization,
/// matching the save/load format used by `save_prompt_context` and
/// `load_prompt_context`.
#[test]
fn test_json_round_trip() {
    let ctx = echo_agent_core::PromptContext {
        ..Default::default()
    };

    let json = serde_json::to_string_pretty(&ctx).unwrap();
    let loaded: echo_agent_core::PromptContext = serde_json::from_str(&json).unwrap();

    assert_eq!(loaded.version, 1);
}

/// Test that PromptContext survives a JSON write-to-disk / read-from-disk
/// cycle with field-level fidelity. This exercises serde + filesystem I/O
/// but not the `save_prompt_context`/`load_prompt_context` wrappers (which
/// depend on `echo_agent_home()` and `SessionInfo` path encoding).
#[test]
fn test_json_round_trip_via_filesystem() {
    let tmp = tempfile::tempdir().unwrap();
    let session_dir = tmp.path().join("session-test");
    std::fs::create_dir_all(&session_dir).unwrap();

    let ctx = echo_agent_core::PromptContext::default();

    // Write directly (mimicking save_prompt_context's logic)
    let path = session_dir.join(PROMPT_CONTEXT_FILENAME);
    let json = serde_json::to_string_pretty(&ctx).unwrap();
    std::fs::write(&path, &json).unwrap();

    // Read back
    let read_json = std::fs::read_to_string(&path).unwrap();
    let loaded: echo_agent_core::PromptContext = serde_json::from_str(&read_json).unwrap();

    assert_eq!(loaded.version, ctx.version);
    assert_eq!(loaded.build_timestamp_utc, ctx.build_timestamp_utc);
}

#[test]
fn test_system_prompt_write_and_read() {
    let tmp = tempfile::tempdir().unwrap();
    let session_dir = tmp.path().join("session-prompt-test");
    std::fs::create_dir_all(&session_dir).unwrap();

    let prompt = "You are a test agent.\n\nDo the thing.";
    let path = session_dir.join(SYSTEM_PROMPT_FILENAME);
    std::fs::write(&path, prompt).unwrap();

    let read_back = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        read_back, prompt,
        "system_prompt.txt must round-trip exactly"
    );
}

#[test]
fn test_system_prompt_is_plain_text_not_json() {
    let prompt = "You are a EchoAgent Build subagent.";
    // system_prompt.txt is raw text, NOT JSON-encoded.
    assert!(!prompt.starts_with('"'), "must not be JSON-quoted");
    assert!(!prompt.starts_with('{'), "must not be JSON object");
}

#[test]
fn test_canonical_artifacts_coexist() {
    let tmp = tempfile::tempdir().unwrap();
    let session_dir = tmp.path().join("session-artifacts");
    std::fs::create_dir_all(&session_dir).unwrap();

    // Write both canonical artifacts.
    let prompt = "You are a test subagent.";
    let ctx = echo_agent_core::PromptContext {
        ..Default::default()
    };

    std::fs::write(session_dir.join(SYSTEM_PROMPT_FILENAME), prompt).unwrap();
    std::fs::write(
        session_dir.join(PROMPT_CONTEXT_FILENAME),
        serde_json::to_string_pretty(&ctx).unwrap(),
    )
    .unwrap();

    // Both files exist and are independently readable.
    assert!(session_dir.join(SYSTEM_PROMPT_FILENAME).exists());
    assert!(session_dir.join(PROMPT_CONTEXT_FILENAME).exists());

    let read_prompt = std::fs::read_to_string(session_dir.join(SYSTEM_PROMPT_FILENAME)).unwrap();
    assert_eq!(read_prompt, prompt);

    let read_ctx: echo_agent_core::PromptContext = serde_json::from_str(
        &std::fs::read_to_string(session_dir.join(PROMPT_CONTEXT_FILENAME)).unwrap(),
    )
    .unwrap();
    assert_eq!(read_ctx.version, 1);
}

/// Core invariant: `system_prompt.txt` must match the first System
/// entry in `chat_history.jsonl`.
#[test]
fn test_system_prompt_matches_chat_history_system_message() {
    let tmp = tempfile::tempdir().unwrap();
    let session_dir = tmp.path().join("session-consistency");
    std::fs::create_dir_all(&session_dir).unwrap();

    let system_prompt = "You are a EchoAgent Build subagent.\n\n<tool_calling>\n...";

    // Write system_prompt.txt (same string used for chat_history).
    std::fs::write(session_dir.join(SYSTEM_PROMPT_FILENAME), system_prompt).unwrap();

    // Simulate chat_history.jsonl first entry.
    let entry = serde_json::json!({ "role": "system", "content": system_prompt });
    std::fs::write(
        session_dir.join("chat_history.jsonl"),
        format!("{}\n", serde_json::to_string(&entry).unwrap()),
    )
    .unwrap();

    // Verify byte-identity.
    let file_prompt = std::fs::read_to_string(session_dir.join(SYSTEM_PROMPT_FILENAME)).unwrap();
    let chat_json = std::fs::read_to_string(session_dir.join("chat_history.jsonl")).unwrap();
    let first_line: serde_json::Value =
        serde_json::from_str(chat_json.lines().next().unwrap()).unwrap();

    assert_eq!(
        file_prompt,
        first_line["content"].as_str().unwrap(),
        "system_prompt.txt must match first system message in chat_history.jsonl"
    );
}

/// Test that missing file gracefully returns None (simulating old sessions).
#[test]
fn test_missing_file_deserializes_as_none() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join(PROMPT_CONTEXT_FILENAME);

    let result = std::fs::read_to_string(&path);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::NotFound);
}

/// Test that corrupt JSON gracefully returns a deserialization error.
#[test]
fn test_corrupt_json_returns_error() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join(PROMPT_CONTEXT_FILENAME);
    std::fs::write(&path, "not valid json {{{").unwrap();

    let json = std::fs::read_to_string(&path).unwrap();
    let result: Result<echo_agent_core::PromptContext, _> = serde_json::from_str(&json);
    assert!(result.is_err(), "corrupt JSON should fail to deserialize");
}

// ── Canonical artifact load tests ───────────────────────────────────

#[test]
fn test_load_system_prompt_returns_content_when_present() {
    let tmp = tempfile::tempdir().unwrap();
    let session_dir = tmp.path().join("session-load-test");
    std::fs::create_dir_all(&session_dir).unwrap();

    let prompt = "You are a EchoAgent Build subagent.";
    std::fs::write(session_dir.join(SYSTEM_PROMPT_FILENAME), prompt).unwrap();

    let loaded = load_system_prompt_from_dir(&session_dir);
    assert_eq!(loaded.as_deref(), Some(prompt));
}

#[test]
fn test_load_system_prompt_returns_none_for_old_sessions() {
    let tmp = tempfile::tempdir().unwrap();
    let session_dir = tmp.path().join("session-old");
    std::fs::create_dir_all(&session_dir).unwrap();

    let loaded = load_system_prompt_from_dir(&session_dir);
    assert!(
        loaded.is_none(),
        "old sessions without system_prompt.txt should return None"
    );
}

#[test]
fn test_load_prompt_context_returns_context_when_present() {
    let tmp = tempfile::tempdir().unwrap();
    let session_dir = tmp.path().join("session-ctx-load");
    std::fs::create_dir_all(&session_dir).unwrap();

    let ctx = echo_agent_core::PromptContext::default();
    std::fs::write(
        session_dir.join(PROMPT_CONTEXT_FILENAME),
        serde_json::to_string_pretty(&ctx).unwrap(),
    )
    .unwrap();

    let loaded = load_prompt_context_from_dir(&session_dir);
    assert!(loaded.is_some());
    assert_eq!(loaded.unwrap().version, 1);
}

#[test]
fn test_load_prompt_context_returns_none_for_old_sessions() {
    let tmp = tempfile::tempdir().unwrap();
    let session_dir = tmp.path().join("session-no-ctx");
    std::fs::create_dir_all(&session_dir).unwrap();

    let loaded = load_prompt_context_from_dir(&session_dir);
    assert!(
        loaded.is_none(),
        "old sessions without prompt_context.json should return None"
    );
}

#[test]
fn test_load_prompt_context_returns_none_for_corrupt_json() {
    let tmp = tempfile::tempdir().unwrap();
    let session_dir = tmp.path().join("session-corrupt");
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(
        session_dir.join(PROMPT_CONTEXT_FILENAME),
        "not valid json {{{",
    )
    .unwrap();

    let loaded = load_prompt_context_from_dir(&session_dir);
    assert!(
        loaded.is_none(),
        "corrupt JSON should return None gracefully"
    );
}

// ── Large-prompt truncation: maybe_truncate_large_prompt_with_skills ────
//
// Oversized prompts are offloaded to an owner-only file; the bounding logic is
// the pure `build_truncated_prompt_message` helper, tested directly below.

// Distinctive markers so head/middle/tail are individually assertable.
const HEAD_TOKEN: &str = "HEADSTART_TOKEN_aaa";
const TAIL_TOKEN: &str = "TAILEND_TOKEN_zzz";

fn fake_prompt_path() -> std::path::PathBuf {
    std::path::PathBuf::from("/tmp/echo-agent-test-home/sessions/cwd/sid/prompts/prompt_0.txt")
}

/// `truncate_bytes_suffix` keeps a char-boundary-safe suffix (multibyte-safe).
#[test]
fn truncate_bytes_suffix_is_utf8_safe() {
    assert_eq!(truncate_bytes_suffix("hello", 5), "hello");
    assert_eq!(truncate_bytes_suffix("hello world", 5), "world");
    // "a🎉🎉b" = 10 bytes; asking for 6 lands mid-codepoint → advances to boundary.
    let s = "a🎉🎉b";
    let out = truncate_bytes_suffix(s, 6);
    assert!(out.len() <= 6);
    assert!(s.ends_with(out));
    assert!(std::str::from_utf8(out.as_bytes()).is_ok());
}

/// `bound_head_tail`: input when it fits, else head+marker+tail within budget.
#[test]
fn bound_head_tail_boundary_and_utf8() {
    // At budget → unchanged (`<=`).
    let fits = "a".repeat(100);
    assert_eq!(bound_head_tail(&fits, 100), fits);
    // One over → bounded.
    let over = "a".repeat(101);
    let out = bound_head_tail(&over, 100);
    assert!(
        out.len() <= 100,
        "bounded output ({}) exceeds budget",
        out.len()
    );
    assert!(out.contains(ELISION_MARKER));
    // Multibyte: no panic, within budget.
    let mb = "🎉".repeat(5_000); // 20_000 bytes
    let out_mb = bound_head_tail(&mb, 8_000);
    assert!(out_mb.len() <= 8_000);
    assert!(out_mb.starts_with('🎉'));
    assert!(out_mb.ends_with('🎉'));
}

/// (a) Oversized query: the bounded message keeps a HEAD and a TAIL (trailing
/// question survives), elides the middle; full body never inlined.
#[test]
fn build_truncated_keeps_query_head_and_tail() {
    let path = fake_prompt_path();
    let middle = "M".repeat(LARGE_PROMPT_THRESHOLD * 3);
    let query = format!("{HEAD_TOKEN} {middle} {TAIL_TOKEN} what does this say?");
    let full = crate::session::prompt_parser::ParsedPrompt::assemble_parts_with_skills(
        "", &query, "", false,
    );

    let message = build_truncated_prompt_message("", &query, "", false, &path, full.len());

    assert!(message.contains(HEAD_TOKEN), "head must survive inline");
    assert!(message.contains(TAIL_TOKEN), "tail must survive inline");
    assert!(
        message.contains("what does this say?"),
        "trailing question must survive inline"
    );
    let head_idx = message.find(HEAD_TOKEN).expect("head present");
    let tail_idx = message.find(TAIL_TOKEN).expect("tail present");
    assert!(
        head_idx < tail_idx,
        "head must appear before tail in the bounded inline message"
    );
    assert!(
        !message.contains(&middle),
        "middle bulk must not be inlined"
    );
    assert!(
        message.contains(ELISION_MARKER),
        "elision marker must mark the cut"
    );
    assert!(
        !message.contains(&query),
        "full query body must not be inlined"
    );
    assert!(message.contains(OFFLOAD_NOTICE_MARKER));
    assert!(message.contains(&path.display().to_string()));
    assert!(
        message.len() <= TRUNCATED_PROMPT_PREFIX_SIZE,
        "message ({}) must stay within budget",
        message.len()
    );
}

/// (b) Large context + small query: query intact, context truncated.
#[test]
fn build_truncated_preserves_small_query_truncates_context() {
    let path = fake_prompt_path();
    let context = format!("CTXHEAD_TOKEN {}", "C".repeat(LARGE_PROMPT_THRESHOLD * 3));
    let query = "please summarise the attached file".to_string();
    let full = crate::session::prompt_parser::ParsedPrompt::assemble_parts_with_skills(
        &context, &query, "", false,
    );

    let message = build_truncated_prompt_message(&context, &query, "", false, &path, full.len());

    assert!(message.contains(&query), "small query preserved intact");
    assert!(
        message.starts_with(&query),
        "echoagent ordering: query block first"
    );
    assert!(message.contains("CTXHEAD_TOKEN"), "context head preserved");
    assert!(!message.contains(&context), "oversized context truncated");
    assert!(message.len() <= TRUNCATED_PROMPT_PREFIX_SIZE);
}

/// Both query and context oversized (the 80/20 split arm): both bounded, neither full body inlined.
#[test]
fn build_truncated_both_oversized_keeps_bounded_heads() {
    let path = fake_prompt_path();
    let query = format!(
        "QHEAD_TOKEN {} QTAIL_TOKEN",
        "Q".repeat(LARGE_PROMPT_THRESHOLD * 2)
    );
    let context = format!("CHEAD_TOKEN {}", "C".repeat(LARGE_PROMPT_THRESHOLD * 2));
    let full = crate::session::prompt_parser::ParsedPrompt::assemble_parts_with_skills(
        &context, &query, "", false,
    );

    let message = build_truncated_prompt_message(&context, &query, "", false, &path, full.len());

    assert!(
        message.contains("QHEAD_TOKEN"),
        "bounded query head present"
    );
    assert!(
        message.contains("QTAIL_TOKEN"),
        "bounded query tail present"
    );
    assert!(
        message.contains("CHEAD_TOKEN"),
        "bounded context head present"
    );
    assert!(!message.contains(&query), "full query not inlined");
    assert!(!message.contains(&context), "full context not inlined");
    assert!(
        message.starts_with("QHEAD_TOKEN"),
        "echoagent ordering: query first"
    );
    assert!(
        message.len() <= TRUNCATED_PROMPT_PREFIX_SIZE,
        "message ({}) must stay within budget",
        message.len()
    );
}

/// Compat-harness ordering: context + notice first, query block last.
#[test]
fn build_truncated_cursor_ordering() {
    let path = fake_prompt_path();
    let query = format!(
        "QHEAD_TOKEN {} QTAIL_TOKEN",
        "Q".repeat(LARGE_PROMPT_THRESHOLD * 2)
    );
    let context = format!("CHEAD_TOKEN {}", "C".repeat(LARGE_PROMPT_THRESHOLD * 2));
    let full = crate::session::prompt_parser::ParsedPrompt::assemble_parts_with_skills(
        &context, &query, "", true,
    );

    let message = build_truncated_prompt_message(&context, &query, "", true, &path, full.len());

    assert!(message.starts_with("CHEAD_TOKEN"), "cursor: context first");
    assert!(
        !message.starts_with("QHEAD_TOKEN"),
        "cursor: query is not first"
    );
    assert!(message.ends_with("QTAIL_TOKEN"), "cursor: query block last");
    let marker_idx = message.find(OFFLOAD_NOTICE_MARKER).expect("notice present");
    let query_idx = message.find("QHEAD_TOKEN").expect("query head present");
    assert!(
        marker_idx < query_idx,
        "cursor: notice precedes the query block"
    );
    assert!(message.len() <= TRUNCATED_PROMPT_PREFIX_SIZE);
}

/// Skills survive inline even when the query is oversized (own reservation).
#[test]
fn build_truncated_preserves_skill_information() {
    let path = fake_prompt_path();
    let query = "Q".repeat(LARGE_PROMPT_THRESHOLD * 3);
    let skills = "SKILL_MARKER: follow the xyz skill steps".to_string();
    let full = crate::session::prompt_parser::ParsedPrompt::assemble_parts_with_skills(
        "", &query, &skills, false,
    );

    let message = build_truncated_prompt_message("", &query, &skills, false, &path, full.len());

    assert!(
        message.contains("SKILL_MARKER"),
        "invoked-skill text must survive inline even with an oversized query"
    );
    assert!(
        !message.contains(&query),
        "full query body must not be inlined"
    );
    assert!(message.len() <= TRUNCATED_PROMPT_PREFIX_SIZE);
}

/// A skill over `SKILL_INLINE_BUDGET` is bounded head+tail; full body not inlined.
#[test]
fn build_truncated_bounds_oversized_skill_head_and_tail() {
    let path = fake_prompt_path();
    let query = "short query".to_string();
    // Skill well over the 4 KB budget, with distinct head/tail markers.
    let skills = format!(
        "SKILLHEAD_TOKEN {} SKILLTAIL_TOKEN",
        "S".repeat(SKILL_INLINE_BUDGET * 2)
    );
    let full = crate::session::prompt_parser::ParsedPrompt::assemble_parts_with_skills(
        "", &query, &skills, false,
    );

    let message = build_truncated_prompt_message("", &query, &skills, false, &path, full.len());

    assert!(
        message.contains("SKILLHEAD_TOKEN"),
        "skill head must survive inline"
    );
    assert!(
        message.contains("SKILLTAIL_TOKEN"),
        "skill tail (closing framing) must survive inline"
    );
    assert!(
        !message.contains(&skills),
        "full skill body must not be inlined"
    );
    assert!(
        message.contains(ELISION_MARKER),
        "oversized skill must be marked as elided"
    );
    assert!(message.contains(&query), "small query stays intact");
    assert!(message.len() <= TRUNCATED_PROMPT_PREFIX_SIZE);
}

/// Multibyte query + context: bounding must not panic, stays within budget.
#[test]
fn build_truncated_multibyte_no_panic() {
    let path = fake_prompt_path();
    let query = "路".repeat(LARGE_PROMPT_THRESHOLD); // 3 bytes each → oversized
    let context = "🎉".repeat(LARGE_PROMPT_THRESHOLD); // 4 bytes each → oversized
    let full = crate::session::prompt_parser::ParsedPrompt::assemble_parts_with_skills(
        &context, &query, "", false,
    );

    let message = build_truncated_prompt_message(&context, &query, "", false, &path, full.len());

    assert!(message.len() <= TRUNCATED_PROMPT_PREFIX_SIZE);
    assert!(message.contains(OFFLOAD_NOTICE_MARKER));
}

/// The offload notice reports bytes, the marker, the path, and `read_file`.
#[test]
fn build_offload_notice_reports_bytes_marker_and_path() {
    let path = fake_prompt_path();
    let notice = build_offload_notice(123_456, &path);
    assert!(notice.contains(OFFLOAD_NOTICE_MARKER));
    assert!(notice.contains("123456 bytes"));
    assert!(notice.contains(&path.display().to_string()));
    assert!(notice.contains("read_file"));
}

// ── Method gate + call-site wiring (hermetic) ───────────────────────────
//
// `echo_agent_home()` is a process-wide `OnceLock`, so the real async method is
// only exercised for the no-offload gate; the offload + fallback wiring is
// covered via the injected-writer seam.

/// Threshold gate: a prompt exactly at `LARGE_PROMPT_THRESHOLD` is returned unchanged, no file.
#[tokio::test(flavor = "current_thread")]
async fn maybe_truncate_at_threshold_returns_unchanged_no_file() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 1_000_000, 85, gateway_tx, persistence_tx).await;

            // Empty context ⇒ full_message == query.
            let at = "Q".repeat(LARGE_PROMPT_THRESHOLD);
            let expected = crate::session::prompt_parser::ParsedPrompt::assemble_parts_with_skills(
                "", &at, "", false,
            );
            let (message, path) = actor
                .maybe_truncate_large_prompt_with_skills(
                    String::new(),
                    at,
                    String::new(),
                    false,
                    70_020,
                )
                .await;
            assert!(path.is_none(), "at-threshold prompt must not offload");
            assert_eq!(message, expected, "at-threshold prompt returned unchanged");
        })
        .await;
}

/// Call-site wiring (injected-writer seam): success → bounded message + `Some(path)`;
/// write failure → the SAME bounded message + `None` (never the oversized original).
#[test]
fn write_offload_and_build_wires_offload_and_fallback() {
    let temp = tempfile::tempdir().unwrap();
    let file_path = temp.path().join("sid").join("prompts").join("prompt_0.txt");
    std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();

    let query = format!(
        "HEAD_TOKEN {} TAIL_TOKEN",
        "Q".repeat(LARGE_PROMPT_THRESHOLD * 3)
    );
    let full = crate::session::prompt_parser::ParsedPrompt::assemble_parts_with_skills(
        "", &query, "", false,
    );
    let bounded = build_truncated_prompt_message("", &query, "", false, &file_path, full.len());

    // Success path: real secure writer.
    let (message, path) = write_offload_and_build(
        &full,
        bounded.clone(),
        file_path.clone(),
        crate::util::secure_file::write_secure_file,
    );
    let path = path.expect("over-threshold offload must return the file path");
    assert_eq!(path, file_path, "returned path is the offload target");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        full,
        "file holds the full message bytes"
    );
    assert_eq!(message, bounded, "success returns the bounded message");
    assert!(!message.contains(&query), "full query body not inlined");
    assert!(
        message.contains(OFFLOAD_NOTICE_MARKER),
        "success keeps the file-referencing offload notice"
    );
    assert!(
        message.contains(&file_path.display().to_string()),
        "success points the model at the real offloaded file"
    );

    // Failure path: erroring writer → bounded excerpt (NOT the oversized
    // original), no path, AND the file-referencing notice stripped so the model
    // is never told to read a file that was never written.
    let (fallback_msg, fallback_path) =
        write_offload_and_build(&full, bounded.clone(), file_path.clone(), |_p, _b| {
            Err(std::io::Error::other("simulated disk full"))
        });
    assert!(
        fallback_path.is_none(),
        "write failure must not return an offload path"
    );
    assert_ne!(
        fallback_msg, bounded,
        "write failure must rewrite the notice, not return it verbatim"
    );
    assert!(
        !fallback_msg.contains(OFFLOAD_NOTICE_MARKER),
        "write failure must strip the file-referencing offload notice"
    );
    assert!(
        !fallback_msg.contains(&file_path.display().to_string()),
        "write failure must not point the model at a file that was never written"
    );
    assert!(
        fallback_msg.contains("could not be saved"),
        "write failure must explain the excerpt is all there is"
    );
    assert!(
        fallback_msg.contains("HEAD_TOKEN") && fallback_msg.contains("TAIL_TOKEN"),
        "the bounded head+tail excerpt must survive the failure path"
    );
    assert!(
        fallback_msg.len() <= TRUNCATED_PROMPT_PREFIX_SIZE,
        "fallback must stay within budget (no re-overflow)"
    );
    assert!(
        !fallback_msg.contains(&query),
        "fallback must not inline the full query"
    );
}

/// `strip_offload_notice` swaps the exact file-referencing notice for the no-file
/// failure notice, and is a no-op when the notice is absent (defensive).
#[test]
fn strip_offload_notice_swaps_notice_for_no_file_text() {
    let path = fake_prompt_path();
    let notice = build_offload_notice(45_177, &path);
    let message = format!("bounded excerpt body{notice}");
    let stripped = strip_offload_notice(&message, &notice);
    assert!(
        stripped.starts_with("bounded excerpt body"),
        "excerpt preserved"
    );
    assert!(
        !stripped.contains(OFFLOAD_NOTICE_MARKER),
        "file-referencing marker removed"
    );
    assert!(
        !stripped.contains(&path.display().to_string()),
        "file path removed"
    );
    assert!(
        stripped.contains("could not be saved"),
        "no-file failure notice substituted"
    );
    // Absent notice → message returned unchanged.
    assert_eq!(
        strip_offload_notice("plain message", &notice),
        "plain message"
    );
}

/// Compat-harness ordering puts the notice MID-message (before the trailing query block);
/// a write failure must strip it in place without discarding that query block.
#[test]
fn write_offload_failure_strips_cursor_midmessage_notice() {
    let temp = tempfile::tempdir().unwrap();
    let file_path = temp.path().join("sid").join("prompts").join("prompt_0.txt");
    let query = format!(
        "QHEAD_TOKEN {} QTAIL_TOKEN",
        "Q".repeat(LARGE_PROMPT_THRESHOLD * 2)
    );
    let context = format!("CHEAD_TOKEN {}", "C".repeat(LARGE_PROMPT_THRESHOLD * 2));
    let full = crate::session::prompt_parser::ParsedPrompt::assemble_parts_with_skills(
        &context, &query, "", true,
    );
    let bounded =
        build_truncated_prompt_message(&context, &query, "", true, &file_path, full.len());
    // Sanity: the notice appears before the trailing query block.
    assert!(bounded.contains(OFFLOAD_NOTICE_MARKER));
    assert!(bounded.ends_with("QTAIL_TOKEN"));

    let (msg, path) = write_offload_and_build(&full, bounded, file_path.clone(), |_p, _b| {
        Err(std::io::Error::other("simulated disk full"))
    });
    assert!(path.is_none(), "failed offload returns no path");
    assert!(
        !msg.contains(OFFLOAD_NOTICE_MARKER),
        "cursor mid-message notice must be stripped"
    );
    assert!(
        !msg.contains(&file_path.display().to_string()),
        "no dangling file path may leak"
    );
    assert!(
        msg.contains("could not be saved"),
        "failure notice substituted"
    );
    assert!(
        msg.ends_with("QTAIL_TOKEN"),
        "trailing query block must survive the in-place strip"
    );
    assert!(
        msg.contains("CHEAD_TOKEN"),
        "context head must survive the strip"
    );
    assert!(msg.len() <= TRUNCATED_PROMPT_PREFIX_SIZE);
}
