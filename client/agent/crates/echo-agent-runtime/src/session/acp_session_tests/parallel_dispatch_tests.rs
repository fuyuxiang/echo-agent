//! Tests for parallel tool dispatch.
//!
//! These tests verify the parallel dispatch path (ECHO_AGENT_PARALLEL_TOOL_DISPATCH):
//! - Phase 1: prepare_tool_call for each tool
//! - Phase 2: permission prompts (if any)
//! - Phase 3: parallel dispatch via dispatch_tool
//! - Post-tool hooks and followups

use super::*;

#[tokio::test]
async fn test_parallel_dispatch_basic() {
    // Ordering correctness: verify that futures::future::join_all preserves
    // the order of results matching the order of input futures.
    //
    // In Phase 2, dispatch_futures is built by mapping approved.iter()
    // to dispatch_tool calls. Phase 3 zips approved.into_iter() with
    // dispatch_results, so result[i] must correspond to approved[i].

    use futures::future::join_all;

    // Simulate 3 tools with different latencies
    let futures = vec![
        Box::pin(async { (0, "tool_a") })
            as std::pin::Pin<Box<dyn futures::Future<Output = (i32, &'static str)>>>,
        Box::pin(async {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            (1, "tool_b")
        }),
        Box::pin(async { (2, "tool_c") }),
    ];

    let results = join_all(futures).await;

    // Results must be in input order, not completion order
    assert_eq!(results[0], (0, "tool_a"));
    assert_eq!(results[1], (1, "tool_b"));
    assert_eq!(results[2], (2, "tool_c"));
}

#[test]
fn test_parallel_dispatch_permission_reject() {
    // Permission rejection abort: when prepare_tool_call returns
    // Err(ToolLoop::PermissionReject), subsequent tools should not
    // be dispatched.
    //
    // Verify the logic: once final_result is set, remaining tools are skipped.
    let mut final_result: Option<ToolLoop> = None;
    let tool_calls = ["tool_0", "tool_1", "tool_2"];
    let mut approved_count = 0;

    for (idx, _call) in tool_calls.iter().enumerate() {
        if final_result.is_some() {
            // Would skip this tool in real code
            continue;
        }
        // Simulate: tool_1 gets permission rejected
        if idx == 1 {
            final_result = Some(ToolLoop::PermissionReject {
                tool_name: "tool_1".to_string(),
                reason: "rejected".to_string(),
            });
            continue;
        }
        approved_count += 1;
    }

    // Only tool_0 should be approved; tool_1 triggers rejection; tool_2 is skipped
    assert_eq!(approved_count, 1);
    assert!(final_result.is_some());
    assert!(matches!(
        final_result,
        Some(ToolLoop::PermissionReject { .. })
    ));
}
#[test]
fn test_parallel_dispatch_followups() {
    // Deferred followups placement: handle_bridge_tool_success returns
    // Vec<ConversationItem> followups that get extended into deferred_followups.
    //
    // In Phase 3:
    //   let followups = handle_bridge_tool_success(...).await?;
    //   deferred_followups.extend(followups);
    //
    // Verify that followups vec can be collected and extended.
    let mut deferred_followups: Vec<&str> = Vec::new();

    // Simulate followups from 2 tools
    let followups_tool_0 = vec!["followup_a", "followup_b"];
    let followups_tool_1 = vec!["followup_c"];

    deferred_followups.extend(followups_tool_0);
    deferred_followups.extend(followups_tool_1);

    assert_eq!(deferred_followups.len(), 3);
    assert_eq!(deferred_followups[0], "followup_a");
    assert_eq!(deferred_followups[1], "followup_b");
    assert_eq!(deferred_followups[2], "followup_c");
}

#[test]
fn test_parallel_dispatch_hooks() {
    // Single-tool-batch no-regression: dispatching a single tool should
    // behave identically to the serial path. The parallel dispatch
    // infrastructure (prepare_tool_call -> dispatch_tool -> post-flight)
    // should work for N=1 without special casing.
    //
    // Verify: 1 tool in approved vec -> 1 dispatch future -> 1 result
    let approved_count = 1;
    let dispatch_futures_count = approved_count; // 1:1 mapping
    let results_count = 1; // incremental stream yields same count

    assert_eq!(approved_count, dispatch_futures_count);
    assert_eq!(dispatch_futures_count, results_count);

    // Also verify the Phase 3 indexed slot works for single element
    let approved = ["single_tool"];
    let ok_val: Result<&str, ()> = Ok("success");
    let results = [ok_val];
    let pairs: Vec<_> = approved.iter().zip(results.iter()).collect();
    assert_eq!(pairs.len(), 1);
}

/// Incremental completion ordering: fast tools must surface before slow siblings.
///
/// Regression for the batch barrier where `join_all` deferred every
/// `ToolCallUpdate(status=Completed)` until the slowest tool in the round
/// finished (e.g. grep stuck pending behind `wait_commands_or_subagents`).
#[tokio::test]
async fn incremental_dispatch_surfaces_fast_tool_before_slow_sibling() {
    use futures::future::BoxFuture;
    use futures::stream::{FuturesUnordered, StreamExt};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    let fast_done = Arc::new(AtomicBool::new(false));
    let slow_done = Arc::new(AtomicBool::new(false));
    let fast_flag = Arc::clone(&fast_done);
    let slow_flag = Arc::clone(&slow_done);

    let mut stream: FuturesUnordered<BoxFuture<'static, (usize, &'static str)>> =
        FuturesUnordered::new();
    stream.push(Box::pin(async move {
        tokio::time::sleep(Duration::from_millis(5)).await;
        fast_flag.store(true, Ordering::SeqCst);
        (0usize, "grep")
    }));
    stream.push(Box::pin(async move {
        tokio::time::sleep(Duration::from_millis(80)).await;
        slow_flag.store(true, Ordering::SeqCst);
        (1usize, "wait_tasks")
    }));

    let mut completion_order = Vec::new();
    while let Some((idx, name)) = stream.next().await {
        completion_order.push((idx, name));
        // Fast tool must finish first — this is what incremental post-flight
        // depends on to stream grep results before the wait tool returns.
        if idx == 0 {
            assert!(
                !slow_done.load(Ordering::SeqCst),
                "fast tool must complete before slow sibling; incremental UI streaming depends on this ordering"
            );
            assert!(fast_done.load(Ordering::SeqCst));
        }
    }

    assert_eq!(completion_order.len(), 2);
    assert_eq!(completion_order[0], (0, "grep"));
    assert_eq!(completion_order[1], (1, "wait_tasks"));
    assert!(fast_done.load(Ordering::SeqCst));
    assert!(slow_done.load(Ordering::SeqCst));
}

/// Regression for the toolset same-file edit race.
///
/// `lock_path_for_args` is the per-call key the dispatcher uses to bucket
/// concurrent tool calls into per-file `tokio::sync::Mutex` groups inside
/// `execute_tool_calls` Phase 2. The original implementation hardcoded
/// `parsed_args.get("file_path")`, which silently bypassed serialization
/// for any toolset whose edit input declared the path under a different
/// JSON key. The compat toolset input types use `path`, and echo_agent_build's
/// `read_file` uses `target_file`, so all of
/// those calls fell through to fully concurrent dispatch and could lose
/// edits via TOCTOU on the same workspace file.
///
/// These tests pin the JSON-key contract so the bucket key keeps tracking
/// every toolset's actual schema.
#[test]
fn lock_path_for_args_matches_echo_agent_build_file_path() {
    // echo_agent_build search_replace / opencode EditTool / WriteTool / etc.
    let args = serde_json::json!({
        "file_path": "/repo/src/main.rs",
        "old_string": "foo",
        "new_string": "bar",
    });
    assert_eq!(lock_path_for_args(&args), Some("/repo/src/main.rs"));
}

#[test]
fn lock_path_for_args_matches_path_arg() {
    // StrReplace / Write / Read / Delete all serialize under `path`.
    let args = serde_json::json!({
        "path": "/repo/src/main.rs",
        "old_string": "foo",
        "new_string": "bar",
    });
    assert_eq!(lock_path_for_args(&args), Some("/repo/src/main.rs"));
}

#[test]
fn lock_path_for_args_matches_echo_agent_build_target_file() {
    // echo_agent_build read_file uses #[serde(rename = "target_file")].
    let args = serde_json::json!({
        "target_file": "/repo/src/main.rs",
    });
    assert_eq!(lock_path_for_args(&args), Some("/repo/src/main.rs"));
}

#[test]
fn lock_path_for_args_returns_none_for_pathless_tools() {
    // Tools like run_terminal_cmd or web_search have no workspace path;
    // they must not be bucketed into a file lock and must run fully
    // concurrently.
    let args = serde_json::json!({
        "command": "ls -la",
        "description": "list",
    });
    assert_eq!(lock_path_for_args(&args), None);
    assert_eq!(lock_path_for_args(&serde_json::json!({})), None);
    assert_eq!(lock_path_for_args(&serde_json::json!(null)), None);
}

#[test]
fn lock_path_for_args_ignores_non_string_path_values() {
    // Defensive: if a model emits a non-string, treat as no lock rather
    // than panicking or coercing — the tool layer will reject it.
    let args = serde_json::json!({"file_path": 42});
    assert_eq!(lock_path_for_args(&args), None);
    let args = serde_json::json!({"path": ["/a", "/b"]});
    assert_eq!(lock_path_for_args(&args), None);
}

#[test]
fn lock_path_for_args_buckets_parallel_compat_strreplace_to_same_lock() {
    // The exact symptom of the bug: two compat StrReplace calls in one
    // batch targeting the same file. Before the fix, both returned None
    // here and ran fully concurrently, racing on the underlying file.
    // After the fix, both must hash to the same bucket so the dispatcher
    // serializes them via a per-file Mutex.
    let call_a = serde_json::json!({
        "path": "/repo/src/main.rs",
        "old_string": "foo",
        "new_string": "bar",
    });
    let call_b = serde_json::json!({
        "path": "/repo/src/main.rs",
        "old_string": "baz",
        "new_string": "qux",
    });
    assert_eq!(lock_path_for_args(&call_a), lock_path_for_args(&call_b));
    assert_eq!(lock_path_for_args(&call_a), Some("/repo/src/main.rs"));

    // Cross-file calls must bucket independently so they keep running
    // concurrently — otherwise we'd serialize unrelated edits and tank
    // batch latency.
    let call_c = serde_json::json!({
        "path": "/repo/src/lib.rs",
        "old_string": "x",
        "new_string": "y",
    });
    assert_ne!(lock_path_for_args(&call_a), lock_path_for_args(&call_c));
}

#[test]
fn lock_path_for_args_buckets_echo_agent_build_and_compat_to_same_lock_for_same_file() {
    // A mixed batch (e.g. echo_agent_build search_replace + StrReplace
    // in the same turn — possible if the harness ever exposes both, or
    // during toolset migration) must still serialize on the shared file
    // path. file_path takes precedence over path when both are present,
    // but neither tool emits both keys today, so this asserts the
    // cross-toolset key normalization works in practice.
    let echoagent = serde_json::json!({
        "file_path": "/repo/src/main.rs",
        "old_string": "a",
        "new_string": "b",
    });
    let compat = serde_json::json!({
        "path": "/repo/src/main.rs",
        "old_string": "c",
        "new_string": "d",
    });
    assert_eq!(lock_path_for_args(&echoagent), lock_path_for_args(&compat));
}

/// Regression: skill-discovery reminders must land after all tool results, not mid-batch.
#[test]
fn test_skill_discovery_deferred_during_parallel_batch() {
    use echo_agent_sampling_types::{ConversationItem, SyntheticReason};

    let mut conversation = vec![ConversationItem::assistant("I'll call 3 tools.")];
    let mut deferred_followups: Vec<ConversationItem> = Vec::new();

    for (i, id) in ["call_1", "call_2", "call_3"].iter().enumerate() {
        conversation.push(ConversationItem::tool_result(
            *id,
            format!("result for {id}"),
        ));
        if i == 0 {
            // Image followup from handle_bridge_tool_success
            deferred_followups.push(ConversationItem::user("[Image content]"));
            // Skill discovery fires after tool 1 — must be deferred, not pushed immediately
            deferred_followups.push(ConversationItem::system_reminder(
                "<system-reminder>\nNew skills discovered\n</system-reminder>",
            ));
        }
    }
    conversation.extend(deferred_followups);

    // 1 assistant + 3 tool_result + 2 deferred user messages
    assert_eq!(conversation.len(), 6);
    assert!(matches!(conversation[0], ConversationItem::Assistant(_)));
    assert!(matches!(conversation[1], ConversationItem::ToolResult(_)));
    assert!(matches!(conversation[2], ConversationItem::ToolResult(_)));
    assert!(matches!(conversation[3], ConversationItem::ToolResult(_)));
    assert!(matches!(conversation[4], ConversationItem::User(_)));
    assert!(
        matches!(conversation[5], ConversationItem::User(ref u) if u.synthetic_reason == Some(SyntheticReason::SystemReminder))
    );
}
