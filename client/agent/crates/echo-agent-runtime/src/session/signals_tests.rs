use super::*;

#[tokio::test]
async fn test_session_signals_basic() {
    let (handle, actor) = SessionSignalsActor::new();

    // Spawn the actor
    let actor_handle = tokio::spawn(actor.run());

    // Send some signals
    handle.increment_turn();
    handle.increment_turn();
    handle.record_tool_call("read_file");
    handle.record_tool_call("search_replace");
    handle.record_tool_call("read_file"); // Duplicate tool
    handle.record_error();
    handle.record_compaction(5_000);
    handle.record_cancellation();

    // Get snapshot
    let snapshot = handle.snapshot().await.unwrap();

    assert_eq!(snapshot.turn_count, 2);
    assert_eq!(snapshot.user_message_count, 2); // Incremented with turn
    assert_eq!(snapshot.tool_call_count, 3);
    assert_eq!(snapshot.error_count, 1);
    assert_eq!(snapshot.compaction_count, 1);
    assert_eq!(snapshot.cancellation_count, 1);
    assert_eq!(snapshot.consecutive_cancellations, 1);
    assert_eq!(snapshot.tools_used.len(), 2); // Only unique tools

    // Shutdown
    handle.shutdown();
    actor_handle.await.unwrap();
}

#[tokio::test]
async fn test_pr_metrics_counters_and_turn_delta() {
    let (handle, actor) = SessionSignalsActor::new();
    let actor_handle = tokio::spawn(actor.run());

    // Create recorded BEFORE the commit (parallel tool results can land
    // out of order) — turn-end reconciliation must still attribute it.
    handle.record_pr_created(PrCreatedSignal {
        url: Some("https://github.com/o/r/pull/7".into()),
        number: Some(7),
        source: PrCreationSource::Bash,
        had_commit_in_session: false,
    });
    handle.record_git_commit();
    handle.record_pr_merged();

    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(snapshot.git_commit_count, 1);
    assert_eq!(snapshot.pr_created_count, 1);
    assert_eq!(snapshot.pr_merged_count, 1);

    let turn = handle.take_turn_end_snapshot().await.unwrap();
    assert_eq!(turn.delta.delta_git_commits, 1);
    assert_eq!(turn.delta.delta_prs_created, 1);
    assert_eq!(turn.delta.delta_prs_merged, 1);
    assert_eq!(turn.delta.prs_created_this_turn.len(), 1);
    let pr = &turn.delta.prs_created_this_turn[0];
    assert_eq!(pr.number, Some(7));
    assert!(pr.had_commit_in_session);

    // Next turn: cumulative counters persist, deltas and the vec reset.
    let turn = handle.take_turn_end_snapshot().await.unwrap();
    assert_eq!(turn.current.pr_created_count, 1);
    assert_eq!(turn.delta.delta_git_commits, 0);
    assert_eq!(turn.delta.delta_prs_created, 0);
    assert!(turn.delta.prs_created_this_turn.is_empty());

    // Serialized delta uses camelCase and omits the vec when empty.
    let json = serde_json::to_string(&turn.delta).unwrap();
    assert!(json.contains("\"deltaPrsCreated\":0"));
    assert!(!json.contains("prsCreatedThisTurn"));

    handle.shutdown();
    actor_handle.await.unwrap();
}

#[tokio::test]
async fn test_consecutive_cancellations() {
    let (handle, actor) = SessionSignalsActor::new();
    let actor_handle = tokio::spawn(actor.run());

    // Multiple cancellations
    handle.record_cancellation();
    handle.record_cancellation();
    handle.record_cancellation();

    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(snapshot.cancellation_count, 3);
    assert_eq!(snapshot.consecutive_cancellations, 3);

    // Turn completion resets consecutive
    handle.record_turn_complete();

    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(snapshot.cancellation_count, 3); // Total unchanged
    assert_eq!(snapshot.consecutive_cancellations, 0); // Reset

    // New cancellation starts counting again
    handle.record_cancellation();
    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(snapshot.consecutive_cancellations, 1);

    handle.shutdown();
    actor_handle.await.unwrap();
}

#[tokio::test]
async fn test_tool_failure() {
    let (handle, actor) = SessionSignalsActor::new();
    let actor_handle = tokio::spawn(actor.run());

    handle.record_tool_failure("bash");
    handle.record_tool_failure("search_replace");

    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(snapshot.tool_failure_count, 2);
    assert_eq!(snapshot.error_count, 2); // Tool failures also count as errors

    handle.shutdown();
    actor_handle.await.unwrap();
}

#[tokio::test]
async fn test_model_tracking() {
    let (handle, actor) = SessionSignalsActor::new();
    let actor_handle = tokio::spawn(actor.run());

    // Set primary model
    handle.set_primary_model("echo-agent-3");

    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(snapshot.primary_model_id, Some("echo-agent-3".to_string()));
    assert_eq!(snapshot.models_used, vec!["echo-agent-3".to_string()]);

    // Record additional model usage
    handle.record_model_usage("echo-agent-4");
    handle.record_model_usage("echo-agent-3"); // Duplicate

    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(snapshot.models_used.len(), 2);
    assert!(snapshot.models_used.contains(&"echo-agent-3".to_string()));
    assert!(snapshot.models_used.contains(&"echo-agent-4".to_string()));

    handle.shutdown();
    actor_handle.await.unwrap();
}

#[tokio::test]
async fn test_assistant_message_count() {
    let (handle, actor) = SessionSignalsActor::new();
    let actor_handle = tokio::spawn(actor.run());

    handle.increment_turn();
    handle.record_assistant_message();
    handle.increment_turn();
    handle.record_assistant_message();

    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(snapshot.turn_count, 2);
    assert_eq!(snapshot.user_message_count, 2);
    assert_eq!(snapshot.assistant_message_count, 2);

    handle.shutdown();
    actor_handle.await.unwrap();
}

#[tokio::test]
async fn test_context_window_usage() {
    let (handle, actor) = SessionSignalsActor::new();
    let actor_handle = tokio::spawn(actor.run());

    handle.update_context_usage(50000, 100000);
    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(snapshot.context_window_usage, 50);
    assert_eq!(snapshot.context_tokens_used, 50000);
    assert_eq!(snapshot.context_window_tokens, 100000);

    handle.update_context_usage(80000, 100000);
    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(snapshot.context_window_usage, 80);
    assert_eq!(snapshot.context_tokens_used, 80000);
    assert_eq!(snapshot.context_window_tokens, 100000);

    // Test edge cases
    handle.update_context_usage(0, 100000);
    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(snapshot.context_window_usage, 0);
    assert_eq!(snapshot.context_tokens_used, 0);
    assert_eq!(snapshot.context_window_tokens, 100000);

    handle.update_context_usage(100000, 100000);
    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(snapshot.context_window_usage, 100);
    assert_eq!(snapshot.context_tokens_used, 100000);
    assert_eq!(snapshot.context_window_tokens, 100000);

    // Over 100% should clamp to 100
    handle.update_context_usage(150000, 100000);
    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(snapshot.context_window_usage, 100);
    assert_eq!(snapshot.context_tokens_used, 150000);
    assert_eq!(snapshot.context_window_tokens, 100000);

    handle.shutdown();
    actor_handle.await.unwrap();
}

#[tokio::test]
async fn test_session_duration() {
    let (handle, actor) = SessionSignalsActor::new();
    let actor_handle = tokio::spawn(actor.run());

    // Small delay to ensure duration > 0
    tokio::time::sleep(Duration::from_millis(10)).await;

    let snapshot = handle.snapshot().await.unwrap();
    // Duration tracking works (u64 so always >= 0)
    assert!(snapshot.session_duration_seconds < 100); // Sanity check - not hours old

    handle.shutdown();
    actor_handle.await.unwrap();
}

#[tokio::test]
async fn test_check_and_mark_sync() {
    let (handle, actor) = SessionSignalsActor::with_sync_interval(Duration::from_millis(50));
    let actor_handle = tokio::spawn(actor.run());

    // First sync should be allowed
    assert!(handle.check_and_mark_sync().await);

    // Immediate check should return false
    assert!(!handle.check_and_mark_sync().await);

    // Wait for sync interval
    tokio::time::sleep(Duration::from_millis(60)).await;

    // Now should be allowed again
    assert!(handle.check_and_mark_sync().await);

    handle.shutdown();
    actor_handle.await.unwrap();
}

#[tokio::test]
async fn test_mark_reverted() {
    let (handle, actor) = SessionSignalsActor::new();
    let actor_handle = tokio::spawn(actor.run());

    let snapshot = handle.snapshot().await.unwrap();
    assert!(!snapshot.has_reverted);

    handle.mark_reverted();

    let snapshot = handle.snapshot().await.unwrap();
    assert!(snapshot.has_reverted);

    handle.shutdown();
    actor_handle.await.unwrap();
}

#[tokio::test]
async fn test_spawn_convenience() {
    let handle = spawn_signals_actor();

    handle.increment_turn();
    handle.record_tool_call("test_tool");

    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(snapshot.turn_count, 1);
    assert_eq!(snapshot.tool_call_count, 1);

    handle.shutdown();
}

#[tokio::test]
async fn test_handle_clone() {
    let (handle1, actor) = SessionSignalsActor::new();
    let handle2 = handle1.clone();
    let actor_handle = tokio::spawn(actor.run());

    // Both handles should affect the same actor
    handle1.increment_turn();
    handle2.increment_turn();

    let snapshot = handle1.snapshot().await.unwrap();
    assert_eq!(snapshot.turn_count, 2);

    handle1.shutdown();
    actor_handle.await.unwrap();
}

#[tokio::test]
async fn test_turn_end_snapshot_first_turn() {
    let (handle, actor) = SessionSignalsActor::new();
    let actor_handle = tokio::spawn(actor.run());

    // Simulate a turn: user prompt, tool calls, assistant response
    handle.increment_turn();
    handle.record_tool_call("read_file");
    handle.record_tool_call("search_replace");
    handle.record_tool_call("read_file"); // repeat
    handle.record_assistant_message();
    handle.record_latency(150, 2500);

    let snap = handle.take_turn_end_snapshot().await.unwrap();

    // First turn — delta should equal cumulative
    assert_eq!(snap.delta.turn_number, 1);
    assert_eq!(snap.delta.delta_tool_calls, 3);
    assert_eq!(snap.delta.delta_assistant_messages, 1);
    assert_eq!(snap.delta.delta_errors, 0);
    assert_eq!(
        snap.delta.tools_this_turn,
        vec!["read_file", "search_replace"]
    );
    assert_eq!(snap.delta.last_time_to_first_token_ms, Some(150));
    assert_eq!(snap.delta.last_total_response_time_ms, Some(2500));
    // New fields
    assert_eq!(snap.delta.delta_long_pauses, 0);
    assert_eq!(snap.delta.delta_successful_tool_uses, 3); // 3 calls, 0 failures
    assert_eq!(snap.delta.consecutive_cancellations, 0);
    assert!(snap.delta.error_types_this_turn.is_empty());
    // No explicit success/failure signals sent, so tool_outcomes should be empty
    assert!(snap.delta.tool_outcomes_this_turn.is_empty());

    // Cumulative should match
    assert_eq!(snap.current.turn_count, 1);
    assert_eq!(snap.current.tool_call_count, 3);

    handle.shutdown();
    actor_handle.await.unwrap();
}

#[tokio::test]
async fn test_inference_metrics_single_response() {
    let (handle, actor) = SessionSignalsActor::new();
    let actor_handle = tokio::spawn(actor.run());

    handle.record_inference_metrics(InferenceLatencyStats {
        time_to_first_token_ms: Some(150),
        time_to_last_byte_ms: 2500,
        chunk_count: 20,
        itl_intervals_ms: vec![30, 30, 30], // 3 intervals, all 30ms
        itl_p50_ms: Some(30),
        itl_p99_ms: Some(30),
        itl_max_ms: Some(30),
        itl_mean_ms: Some(30),
        attempts: 0,
    });

    let snap = handle.snapshot().await.unwrap();

    // Session-level ITL (computed from TDigest)
    assert_eq!(snap.itl_p50_ms, Some(30));
    assert_eq!(snap.itl_p99_ms, Some(30));
    assert_eq!(snap.itl_max_ms, Some(30));
    assert_eq!(snap.itl_mean_ms, Some(30));
    // Counts
    assert_eq!(snap.total_chunk_count, 20);
    assert_eq!(snap.itl_sample_count, 1);
    // Existing TTFB/TTLB tracking should also be populated
    assert_eq!(snap.avg_time_to_first_token_ms, 150);
    assert_eq!(snap.avg_response_time_ms, 2500);
    assert_eq!(snap.latency_sample_count, 1);

    handle.shutdown();
    actor_handle.await.unwrap();
}

#[tokio::test]
async fn test_turn_end_snapshot_multi_turn_deltas() {
    let (handle, actor) = SessionSignalsActor::new();
    let actor_handle = tokio::spawn(actor.run());

    // === Turn 1 ===
    handle.increment_turn();
    handle.record_tool_call("read_file");
    handle.record_tool_call("bash");
    handle.record_error_typed("timeout");
    handle.record_assistant_message();
    handle.record_latency(100, 2000);

    let snap1 = handle.take_turn_end_snapshot().await.unwrap();
    assert_eq!(snap1.delta.turn_number, 1);
    assert_eq!(snap1.delta.delta_tool_calls, 2);
    assert_eq!(snap1.delta.delta_errors, 1);
    assert_eq!(snap1.delta.delta_successful_tool_uses, 2); // 2 calls, 0 failures
    assert_eq!(snap1.delta.tools_this_turn.len(), 2);
    assert_eq!(snap1.delta.error_types_this_turn, vec!["timeout"]);

    // === Turn 2 ===
    handle.increment_turn();
    handle.record_tool_call("search_replace");
    handle.record_tool_failure("search_replace"); // 1 tool failure (also increments error_count)
    handle.record_assistant_message();
    handle.record_latency(200, 3000);

    let snap2 = handle.take_turn_end_snapshot().await.unwrap();
    assert_eq!(snap2.delta.turn_number, 2);
    assert_eq!(snap2.delta.delta_tool_calls, 1); // only 1 this turn
    assert_eq!(snap2.delta.delta_errors, 1); // tool failure counted as error
    assert_eq!(snap2.delta.delta_tool_failures, 1);
    assert_eq!(snap2.delta.delta_successful_tool_uses, 0); // 1 call - 1 failure
    assert_eq!(snap2.delta.delta_assistant_messages, 1);
    assert_eq!(snap2.delta.tools_this_turn, vec!["search_replace"]);
    assert_eq!(snap2.delta.last_time_to_first_token_ms, Some(200));
    // error_types_this_turn should be empty — tool_failure doesn't set an error type
    assert!(snap2.delta.error_types_this_turn.is_empty());

    // Cumulative should reflect both turns
    assert_eq!(snap2.current.turn_count, 2);
    assert_eq!(snap2.current.tool_call_count, 3);
    assert_eq!(snap2.current.error_count, 2); // 1 typed error + 1 tool failure

    // === Turn 3: empty turn (no tool calls, no errors) ===
    handle.increment_turn();
    handle.record_assistant_message();

    let snap3 = handle.take_turn_end_snapshot().await.unwrap();
    assert_eq!(snap3.delta.turn_number, 3);
    assert_eq!(snap3.delta.delta_tool_calls, 0);
    assert_eq!(snap3.delta.delta_errors, 0);
    assert_eq!(snap3.delta.delta_successful_tool_uses, 0);
    assert_eq!(snap3.delta.tools_this_turn, Vec::<String>::new());
    assert_eq!(snap3.delta.last_time_to_first_token_ms, None); // no latency recorded

    handle.shutdown();
    actor_handle.await.unwrap();
}

#[tokio::test]
async fn test_inference_metrics_multi_response_aggregation() {
    let (handle, actor) = SessionSignalsActor::new();
    let actor_handle = tokio::spawn(actor.run());

    // Response 1: intervals [10, 20, 30, 40, 50, 60, 70, 80, 90, 100]
    handle.record_inference_metrics(InferenceLatencyStats {
        time_to_first_token_ms: Some(100),
        time_to_last_byte_ms: 1000,
        chunk_count: 10,
        itl_intervals_ms: vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100],
        itl_p50_ms: Some(40),
        itl_p99_ms: Some(90),
        itl_max_ms: Some(100),
        itl_mean_ms: Some(50),
        attempts: 0,
    });

    // Response 2: intervals [100, 110, 120, 130, 140, 150, 160, 170, 180, 190, 200] (11 intervals)
    handle.record_inference_metrics(InferenceLatencyStats {
        time_to_first_token_ms: Some(120),
        time_to_last_byte_ms: 2000,
        chunk_count: 20,
        itl_intervals_ms: vec![100, 110, 120, 130, 140, 150, 160, 170, 180, 190, 200],
        itl_p50_ms: Some(55),
        itl_p99_ms: Some(150),
        itl_max_ms: Some(200),
        itl_mean_ms: Some(60),
        attempts: 0,
    });

    // Response 3: intervals [5, 10, 15, 20, 25] (5 intervals)
    handle.record_inference_metrics(InferenceLatencyStats {
        time_to_first_token_ms: Some(90),
        time_to_last_byte_ms: 1500,
        chunk_count: 30,
        itl_intervals_ms: vec![5, 10, 15, 20, 25],
        itl_p50_ms: Some(25),
        itl_p99_ms: Some(70),
        itl_max_ms: Some(80),
        itl_mean_ms: Some(30),
        attempts: 0,
    });

    let snap = handle.snapshot().await.unwrap();

    // With 26 total intervals combined: [5,10,10,15,20,20,25,30,40,50,60,70,80,90,100,100,110,120,130,140,150,160,170,180,190,200]
    // Sorted: [5,10,10,15,20,20,25,30,40,50,60,70,80,90,100,100,110,120,130,140,150,160,170,180,190,200]
    // Exact p50 (26/2=13) -> index 13 = 90
    // Exact p99: ceil(26*0.99)-1 = ceil(25.74)-1 = 26-1 = 25, min(25, 25) = 25 -> 200
    // Exact max = 200
    // Exact mean = (10+20+30+40+50+60+70+80+90+100 + 100+110+120+130+140+150+160+170+180+190+200 + 5+10+15+20+25) / 26
    //            = (550 + 1650 + 75) / 26 = 2275 / 26 = 87

    // TDigest gives approximate percentiles - verify they're reasonable
    let p50 = snap.itl_p50_ms.unwrap();
    let p99 = snap.itl_p99_ms.unwrap();
    assert!(
        (75..=105).contains(&p50),
        "p50={} should be near 90 (75-105)",
        p50
    );
    assert!(p99 >= 190, "p99={} should be near 200 (>=190)", p99);

    // Max and mean are exact
    assert_eq!(snap.itl_max_ms, Some(200));
    assert_eq!(snap.itl_mean_ms, Some(87));
    // Counts
    assert_eq!(snap.total_chunk_count, 60);
    assert_eq!(snap.itl_sample_count, 3);

    handle.shutdown();
    actor_handle.await.unwrap();
}

#[tokio::test]
async fn test_turn_end_snapshot_resets_per_turn_state() {
    let (handle, actor) = SessionSignalsActor::new();
    let actor_handle = tokio::spawn(actor.run());

    // Record some signals
    handle.increment_turn();
    handle.record_tool_call("bash");
    handle.record_error_typed("rate_limit");
    handle.record_latency(100, 500);

    // Take snapshot — should consume per-turn state
    let snap1 = handle.take_turn_end_snapshot().await.unwrap();
    assert_eq!(snap1.delta.tools_this_turn, vec!["bash"]);
    assert_eq!(snap1.delta.error_types_this_turn, vec!["rate_limit"]);
    assert_eq!(snap1.delta.last_time_to_first_token_ms, Some(100));

    // Take another snapshot immediately — per-turn state should be empty
    let snap2 = handle.take_turn_end_snapshot().await.unwrap();
    assert!(snap2.delta.tools_this_turn.is_empty());
    assert!(snap2.delta.error_types_this_turn.is_empty());
    assert_eq!(snap2.delta.last_time_to_first_token_ms, None);
    assert_eq!(snap2.delta.delta_tool_calls, 0);

    handle.shutdown();
    actor_handle.await.unwrap();
}

#[tokio::test]
async fn test_turn_end_snapshot_consecutive_cancellations() {
    let (handle, actor) = SessionSignalsActor::new();
    let actor_handle = tokio::spawn(actor.run());

    // Turn 1: user cancels twice, then assistant completes
    handle.increment_turn();
    handle.record_cancellation();
    handle.record_cancellation();
    handle.record_assistant_message();

    let snap1 = handle.take_turn_end_snapshot().await.unwrap();
    assert_eq!(snap1.delta.delta_cancellations, 2);
    assert_eq!(snap1.delta.consecutive_cancellations, 2);

    // Turn complete resets consecutive count
    handle.record_turn_complete();

    // Turn 2: no cancellations
    handle.increment_turn();
    handle.record_tool_call("read_file");
    handle.record_assistant_message();

    let snap2 = handle.take_turn_end_snapshot().await.unwrap();
    assert_eq!(snap2.delta.delta_cancellations, 0);
    assert_eq!(snap2.delta.consecutive_cancellations, 0);

    handle.shutdown();
    actor_handle.await.unwrap();
}

#[tokio::test]
async fn test_turn_end_snapshot_error_types_mixed() {
    let (handle, actor) = SessionSignalsActor::new();
    let actor_handle = tokio::spawn(actor.run());

    handle.increment_turn();
    // Mix of typed and untyped errors
    handle.record_error_typed("timeout");
    handle.record_error(); // untyped — doesn't add to error_types_this_turn
    handle.record_error_typed("rate_limit");
    handle.record_assistant_message();

    let snap = handle.take_turn_end_snapshot().await.unwrap();
    assert_eq!(snap.delta.delta_errors, 3); // all 3 count
    assert_eq!(
        snap.delta.error_types_this_turn,
        vec!["timeout", "rate_limit"]
    ); // only typed ones

    handle.shutdown();
    actor_handle.await.unwrap();
}

#[tokio::test]
async fn test_turn_end_snapshot_tool_outcomes() {
    let (handle, actor) = SessionSignalsActor::new();
    let actor_handle = tokio::spawn(actor.run());

    // Turn 1: bash succeeds twice, read_file succeeds once, search_replace fails once
    handle.increment_turn();
    handle.record_tool_call("bash");
    handle.record_tool_success("bash");
    handle.record_tool_call("bash");
    handle.record_tool_success("bash");
    handle.record_tool_call("read_file");
    handle.record_tool_success("read_file");
    handle.record_tool_call("search_replace");
    handle.record_tool_failure("search_replace");
    handle.record_assistant_message();

    let snap1 = handle.take_turn_end_snapshot().await.unwrap();
    assert_eq!(snap1.delta.delta_tool_calls, 4);
    assert_eq!(snap1.delta.delta_tool_failures, 1);
    assert_eq!(snap1.delta.delta_successful_tool_uses, 3);

    // tool_outcomes_this_turn should be sorted by name
    let outcomes = &snap1.delta.tool_outcomes_this_turn;
    assert_eq!(outcomes.len(), 3);
    assert_eq!(
        outcomes[0],
        ToolOutcome {
            tool_name: "bash".to_string(),
            successes: 2,
            failures: 0,
        }
    );
    assert_eq!(
        outcomes[1],
        ToolOutcome {
            tool_name: "read_file".to_string(),
            successes: 1,
            failures: 0,
        }
    );
    assert_eq!(
        outcomes[2],
        ToolOutcome {
            tool_name: "search_replace".to_string(),
            successes: 0,
            failures: 1,
        }
    );

    // Turn 2: no tools — outcomes should be empty
    handle.increment_turn();
    handle.record_assistant_message();

    let snap2 = handle.take_turn_end_snapshot().await.unwrap();
    assert!(snap2.delta.tool_outcomes_this_turn.is_empty());

    handle.shutdown();
    actor_handle.await.unwrap();
}

#[tokio::test]
async fn test_turn_end_snapshot_token_usage() {
    let (handle, actor) = SessionSignalsActor::new();
    let actor_handle = tokio::spawn(actor.run());

    // Turn 1: one response with completion and reasoning tokens
    handle.increment_turn();
    handle.record_assistant_message();
    handle.record_token_usage(500, 200); // 500 completion, 200 reasoning → 300 response

    let snap1 = handle.take_turn_end_snapshot().await.unwrap();
    assert_eq!(snap1.delta.response_tokens, Some(300)); // 500 - 200
    assert_eq!(snap1.delta.thinking_tokens, Some(200));

    // Turn 2: multi-round tool use — two responses accumulate
    handle.increment_turn();
    handle.record_tool_call("bash");
    handle.record_token_usage(100, 50); // first response: 50 response + 50 thinking
    handle.record_assistant_message();
    handle.record_token_usage(400, 0); // second response: 400 response, 0 thinking

    let snap2 = handle.take_turn_end_snapshot().await.unwrap();
    assert_eq!(snap2.delta.response_tokens, Some(450)); // (100-50) + (400-0)
    assert_eq!(snap2.delta.thinking_tokens, Some(50)); // 50 + 0

    // Turn 3: no token usage recorded — should be None (not Some(0))
    handle.increment_turn();
    handle.record_assistant_message();

    let snap3 = handle.take_turn_end_snapshot().await.unwrap();
    assert_eq!(snap3.delta.response_tokens, None);
    assert_eq!(snap3.delta.thinking_tokens, None);

    handle.shutdown();
    actor_handle.await.unwrap();
}

#[tokio::test]
async fn test_seed_counts_restores_all_counters() {
    let (handle, actor) = SessionSignalsActor::new();
    let actor_handle = tokio::spawn(actor.run());

    // Seed counts as if resuming a session with prior tool usage and models
    handle.seed_counts(
        5,  // user messages
        4,  // assistant messages
        12, // tool calls
        vec![
            "read_file".to_string(),
            "bash".to_string(),
            "search_replace".to_string(),
        ],
        vec!["echo-agent-3".to_string(), "echo-agent-4".to_string()],
    );

    let snapshot = handle.snapshot().await.unwrap();

    // Message counts (existing behavior)
    assert_eq!(snapshot.turn_count, 5);
    assert_eq!(snapshot.user_message_count, 5);
    assert_eq!(snapshot.assistant_message_count, 4);

    // Tool counts (newly restored)
    assert_eq!(snapshot.tool_call_count, 12);
    assert_eq!(snapshot.tools_used.len(), 3);
    assert!(snapshot.tools_used.contains(&"read_file".to_string()));
    assert!(snapshot.tools_used.contains(&"bash".to_string()));
    assert!(snapshot.tools_used.contains(&"search_replace".to_string()));

    // Model tracking (newly restored)
    assert_eq!(snapshot.models_used.len(), 2);
    assert!(snapshot.models_used.contains(&"echo-agent-3".to_string()));
    assert!(snapshot.models_used.contains(&"echo-agent-4".to_string()));

    // After seeding, new tool calls should accumulate correctly
    handle.record_tool_call("bash"); // existing tool
    handle.record_tool_call("grep"); // new tool
    handle.record_model_usage("echo-agent-3"); // existing model
    handle.record_model_usage("echo-agent-4.5"); // new model

    let snapshot = handle.snapshot().await.unwrap();
    assert_eq!(snapshot.tool_call_count, 14); // 12 + 2
    assert_eq!(snapshot.tools_used.len(), 4); // bash not duplicated, grep added
    assert!(snapshot.tools_used.contains(&"grep".to_string()));
    assert_eq!(snapshot.models_used.len(), 3); // echo-agent-3 not duplicated, echo-agent-5 added
    assert!(snapshot.models_used.contains(&"echo-agent-4.5".to_string()));

    handle.shutdown();
    actor_handle.await.unwrap();
}

#[tokio::test]
async fn test_restore_signals_full_round_trip() {
    // Phase 1: Build up state in an actor, then snapshot it
    let (handle1, actor1) = SessionSignalsActor::new();
    let actor_handle1 = tokio::spawn(actor1.run());

    // Simulate several turns with diverse signals
    handle1.increment_turn(); // turn 1
    handle1.record_tool_call("bash");
    handle1.record_tool_call("read_file");
    handle1.record_tool_failure("bash");
    handle1.record_error();
    handle1.record_assistant_message();
    handle1.record_model_usage("echo-agent-3");

    // Record inference metrics with ITL intervals for turn 1
    handle1.record_inference_metrics(InferenceLatencyStats {
        time_to_first_token_ms: Some(100),
        time_to_last_byte_ms: 1000,
        chunk_count: 6,
        itl_intervals_ms: vec![10, 20, 30, 40, 50],
        itl_p50_ms: Some(30),
        itl_p99_ms: Some(50),
        itl_max_ms: Some(50),
        itl_mean_ms: Some(30),
        attempts: 0,
    });

    handle1.increment_turn(); // turn 2
    handle1.record_tool_call("search_replace");
    handle1.record_cancellation();
    handle1.record_assistant_message();
    handle1.record_model_usage("echo-agent-4");

    handle1.increment_turn(); // turn 3
    handle1.record_tool_call("bash");
    handle1.record_assistant_message();

    // Record latency for an additional response (no ITL)
    handle1.record_latency(200, 2000);

    // Take a turn-end snapshot (to set previous_turn_snapshot baseline)
    let _snap1 = handle1.take_turn_end_snapshot().await;

    // Take the final snapshot
    let snapshot = handle1.snapshot().await.unwrap();

    // Verify the snapshot has meaningful data
    assert_eq!(snapshot.turn_count, 3);
    assert_eq!(snapshot.user_message_count, 3);
    assert_eq!(snapshot.assistant_message_count, 3);
    assert_eq!(snapshot.tool_call_count, 4);
    assert_eq!(snapshot.tool_failure_count, 1);
    assert_eq!(snapshot.error_count, 2); // 1 tool failure (counted as error) + 1 explicit error
    assert_eq!(snapshot.cancellation_count, 1);
    assert_eq!(snapshot.tools_used.len(), 3);
    assert_eq!(snapshot.models_used.len(), 2);
    assert_eq!(snapshot.latency_sample_count, 2);
    assert_eq!(snapshot.avg_time_to_first_token_ms, 150); // (100+200)/2
    assert_eq!(snapshot.avg_response_time_ms, 1500); // (1000+2000)/2
    assert_eq!(snapshot.min_time_to_first_token_ms, 100);
    assert_eq!(snapshot.max_time_to_first_token_ms, 200);
    // ITL stats from phase 1 should be present
    assert!(
        snapshot.itl_p50_ms.is_some(),
        "itl_p50_ms should be set after recording ITL data"
    );
    assert!(
        snapshot.itl_p99_ms.is_some(),
        "itl_p99_ms should be set after recording ITL data"
    );
    assert_eq!(snapshot.itl_max_ms, Some(50));
    assert_eq!(snapshot.itl_mean_ms, Some(30)); // (10+20+30+40+50)/5
    assert_eq!(snapshot.total_chunk_count, 6);
    assert_eq!(snapshot.itl_sample_count, 1);

    handle1.shutdown();
    actor_handle1.await.unwrap();

    // Phase 2: Restore the snapshot into a new actor
    let (handle2, actor2) = SessionSignalsActor::new();
    let actor_handle2 = tokio::spawn(actor2.run());

    handle2.restore_signals(snapshot.clone());

    // Verify all fields are faithfully restored
    let restored = handle2.snapshot().await.unwrap();
    assert_eq!(restored.turn_count, 3);
    assert_eq!(restored.user_message_count, 3);
    assert_eq!(restored.assistant_message_count, 3);
    assert_eq!(restored.tool_call_count, 4);
    assert_eq!(restored.tool_failure_count, 1);
    assert_eq!(restored.error_count, 2);
    assert_eq!(restored.cancellation_count, 1);
    assert_eq!(restored.tools_used.len(), 3);
    assert!(restored.tools_used.contains(&"bash".to_string()));
    assert!(restored.tools_used.contains(&"read_file".to_string()));
    assert!(restored.tools_used.contains(&"search_replace".to_string()));
    assert_eq!(restored.models_used.len(), 2);
    assert!(restored.models_used.contains(&"echo-agent-3".to_string()));
    assert!(restored.models_used.contains(&"echo-agent-4".to_string()));
    assert_eq!(restored.latency_sample_count, 2);
    assert_eq!(restored.avg_time_to_first_token_ms, 150);
    assert_eq!(restored.avg_response_time_ms, 1500);
    assert_eq!(restored.min_time_to_first_token_ms, 100);
    assert_eq!(restored.max_time_to_first_token_ms, 200);
    // ITL stats must survive the restore (regression test for echo-agent-critique bug)
    assert_eq!(
        restored.itl_p50_ms, snapshot.itl_p50_ms,
        "itl_p50_ms should survive restore"
    );
    assert_eq!(
        restored.itl_p99_ms, snapshot.itl_p99_ms,
        "itl_p99_ms should survive restore"
    );
    assert_eq!(
        restored.itl_max_ms,
        Some(50),
        "itl_max_ms should survive restore"
    );
    assert_eq!(
        restored.itl_mean_ms,
        Some(30),
        "itl_mean_ms should survive restore"
    );
    assert_eq!(restored.total_chunk_count, 6);
    assert_eq!(restored.itl_sample_count, 1);

    // Phase 2b: Take a turn-end snapshot *without* recording new ITL data.
    // This is the exact scenario the echo-agent-critique bug describes: the
    // TakeTurnEndSnapshot handler calls update_session_itl_percentiles()
    // which must NOT wipe persisted ITL p50/p99 when itl_digest is None.
    handle2.increment_turn(); // turn 4 (no ITL data recorded this turn)
    handle2.record_assistant_message();
    let delta_snap_no_itl = handle2.take_turn_end_snapshot().await.unwrap();
    let after_empty_turn = handle2.snapshot().await.unwrap();
    // ITL percentiles must still be present even without new ITL data
    assert_eq!(
        after_empty_turn.itl_p50_ms, snapshot.itl_p50_ms,
        "itl_p50_ms must survive TakeTurnEndSnapshot without new ITL data"
    );
    assert_eq!(
        after_empty_turn.itl_p99_ms, snapshot.itl_p99_ms,
        "itl_p99_ms must survive TakeTurnEndSnapshot without new ITL data"
    );
    assert_eq!(after_empty_turn.itl_max_ms, Some(50));
    assert_eq!(after_empty_turn.itl_mean_ms, Some(30));
    // Per-turn ITL should be None since no ITL data was recorded this turn
    assert_eq!(delta_snap_no_itl.delta.last_itl_p50_ms, None);

    // Phase 3: Verify subsequent signals accumulate correctly after restore
    handle2.increment_turn(); // turn 5
    handle2.record_tool_call("grep"); // new tool
    handle2.record_tool_call("bash"); // existing tool (should dedup)
    handle2.record_model_usage("echo-agent-3"); // existing model (should dedup)
    handle2.record_error();
    handle2.record_assistant_message();

    // Record latency — average should incorporate restored history
    handle2.record_latency(300, 3000);

    let after_turn = handle2.snapshot().await.unwrap();
    assert_eq!(after_turn.turn_count, 5);
    assert_eq!(after_turn.user_message_count, 5);
    assert_eq!(after_turn.assistant_message_count, 5);
    assert_eq!(after_turn.tool_call_count, 6); // 4 + 2
    assert_eq!(after_turn.error_count, 3); // 2 + 1
    assert_eq!(after_turn.tools_used.len(), 4); // bash not duplicated, grep added
    assert!(after_turn.tools_used.contains(&"grep".to_string()));
    assert_eq!(after_turn.models_used.len(), 2); // echo-agent-3 not duplicated
    // Latency: (100+200+300)/3 = 200
    assert_eq!(after_turn.latency_sample_count, 3);
    assert_eq!(after_turn.avg_time_to_first_token_ms, 200);
    assert_eq!(after_turn.avg_response_time_ms, 2000);

    // Phase 4: Verify turn-end delta is computed against restored baseline, not zero
    let delta_snap = handle2.take_turn_end_snapshot().await.unwrap();
    // Delta should only reflect turn 5, not all 5 turns
    assert_eq!(delta_snap.delta.turn_number, 5);
    assert_eq!(delta_snap.delta.delta_tool_calls, 2); // only the 2 new calls
    assert_eq!(delta_snap.delta.delta_errors, 1); // only the 1 new error
    assert_eq!(delta_snap.delta.delta_tool_failures, 0); // no new failures

    // session_duration should be >= the restored value (not reset to 0)
    assert!(after_turn.session_duration_seconds >= snapshot.session_duration_seconds);

    handle2.shutdown();
    actor_handle2.await.unwrap();
}

// =========================================================================
// LOC Attribution Tests
// =========================================================================

#[tokio::test]
async fn test_loc_change_accumulates_correctly() {
    let (handle, actor) = SessionSignalsActor::new();
    let actor_handle = tokio::spawn(actor.run());

    // Agent adds lines to two files
    handle.record_loc_change(true, 10, 0, "/tmp/a.rs".into());
    handle.record_loc_change(true, 5, 2, "/tmp/b.rs".into());

    // Human adds lines
    handle.record_loc_change(false, 3, 0, "/tmp/a.rs".into());
    handle.record_loc_change(false, 7, 1, "/tmp/c.rs".into());

    let snap = handle.snapshot().await.unwrap();

    // Agent: 10+5=15 added, 0+2=2 removed
    assert_eq!(snap.agent_lines_added, 15);
    assert_eq!(snap.agent_lines_removed, 2);
    assert_eq!(snap.agent_files_touched, 2); // a.rs, b.rs

    // Human: 3+7=10 added, 0+1=1 removed
    assert_eq!(snap.human_lines_added, 10);
    assert_eq!(snap.human_lines_removed, 1);
    assert_eq!(snap.human_files_touched, 2); // a.rs, c.rs

    // Total files: a.rs, b.rs, c.rs = 3
    assert_eq!(snap.total_files_touched, 3);

    // No reverts yet
    assert_eq!(snap.agent_lines_added_reverted, 0);
    assert_eq!(snap.human_lines_added_reverted, 0);

    handle.shutdown();
    actor_handle.await.unwrap();
}

#[tokio::test]
async fn test_loc_revert_is_noop_until_per_author_attribution() {
    let (handle, actor) = SessionSignalsActor::new();
    let actor_handle = tokio::spawn(actor.run());

    // Agent adds 10 lines
    handle.record_loc_change(true, 10, 0, "/tmp/a.rs".into());

    // Revert event is received but intentionally ignored — all 4 revert
    // counters stay at 0 to avoid publishing misleading partial data.
    handle.record_loc_revert(5, 0);

    let snap = handle.snapshot().await.unwrap();

    // Gross stays at 10
    assert_eq!(snap.agent_lines_added, 10);
    // Reverts are 0 (handler is a no-op until per-author attribution is implemented)
    assert_eq!(snap.agent_lines_added_reverted, 0);
    assert_eq!(snap.agent_lines_removed_reverted, 0);
    assert_eq!(snap.human_lines_added_reverted, 0);
    assert_eq!(snap.human_lines_removed_reverted, 0);

    handle.shutdown();
    actor_handle.await.unwrap();
}

#[tokio::test]
async fn test_loc_turn_deltas() {
    let (handle, actor) = SessionSignalsActor::new();
    let actor_handle = tokio::spawn(actor.run());

    // Turn 1: agent adds 10 lines
    handle.increment_turn();
    handle.record_loc_change(true, 10, 0, "/tmp/a.rs".into());
    handle.record_assistant_message();
    let snap1 = handle.take_turn_end_snapshot().await.unwrap();

    assert_eq!(snap1.delta.delta_agent_lines_added, 10);
    assert_eq!(snap1.delta.delta_human_lines_added, 0);
    assert_eq!(snap1.delta.delta_agent_files_touched, 1);

    // Turn 2: human adds 5 lines to a different file
    handle.increment_turn();
    handle.record_loc_change(false, 5, 0, "/tmp/b.rs".into());
    handle.record_assistant_message();
    let snap2 = handle.take_turn_end_snapshot().await.unwrap();

    // Turn 2 deltas should only reflect turn 2 changes
    assert_eq!(snap2.delta.delta_agent_lines_added, 0);
    assert_eq!(snap2.delta.delta_human_lines_added, 5);
    assert_eq!(snap2.delta.delta_human_files_touched, 1);
    assert_eq!(snap2.delta.delta_agent_files_touched, 0);
    assert_eq!(snap2.delta.delta_total_files_touched, 1);

    // Cumulative should have both
    assert_eq!(snap2.current.agent_lines_added, 10);
    assert_eq!(snap2.current.human_lines_added, 5);
    assert_eq!(snap2.current.total_files_touched, 2);

    handle.shutdown();
    actor_handle.await.unwrap();
}

#[tokio::test]
async fn test_loc_file_dedup() {
    let (handle, actor) = SessionSignalsActor::new();
    let actor_handle = tokio::spawn(actor.run());

    // Agent edits same file multiple times
    handle.record_loc_change(true, 5, 0, "/tmp/a.rs".into());
    handle.record_loc_change(true, 3, 0, "/tmp/a.rs".into());
    handle.record_loc_change(true, 2, 0, "/tmp/a.rs".into());

    let snap = handle.snapshot().await.unwrap();

    // Lines accumulate, but file count stays at 1
    assert_eq!(snap.agent_lines_added, 10);
    assert_eq!(snap.agent_files_touched, 1);
    assert_eq!(snap.total_files_touched, 1);

    handle.shutdown();
    actor_handle.await.unwrap();
}

/// Hunk reshuffling (content moves between hunks during diff recomputation)
/// must cancel out. A -12 and +12 from two ContentChanged events should
/// net to zero, not inflate the counter.
#[tokio::test]
async fn test_loc_hunk_reshuffle_cancels_out() {
    let (handle, actor) = SessionSignalsActor::new();
    let actor_handle = tokio::spawn(actor.run());

    // Agent adds 13 lines
    handle.record_loc_change(true, 13, 1, "/tmp/jokes.md".into());

    // Human edits the file — hunk reshuffling:
    // One hunk shrinks by 12 (content migrated away)
    handle.record_loc_change(false, -12, 0, "/tmp/jokes.md".into());
    // Another hunk grows by 12 (absorbed the content)
    handle.record_loc_change(false, 12, 0, "/tmp/jokes.md".into());
    // Plus the actual human addition: 1 line
    handle.record_loc_change(false, 1, 0, "/tmp/jokes.md".into());

    let snap = handle.snapshot().await.unwrap();

    // Agent totals unchanged
    assert_eq!(snap.agent_lines_added, 13);
    assert_eq!(snap.agent_lines_removed, 1);

    // Human: the -12 and +12 must cancel out, leaving only the +1
    assert_eq!(
        snap.human_lines_added, 1,
        "Hunk reshuffling (-12/+12) must cancel out, only actual +1 should count"
    );

    handle.shutdown();
    actor_handle.await.unwrap();
}

// === Tracing signal event tests ===

#[tokio::test]
async fn test_idle_timeout_counter() {
    let (handle, actor) = SessionSignalsActor::new();
    let actor_handle = tokio::spawn(actor.run());

    handle.record_idle_timeout();
    handle.record_idle_timeout();

    let snap = handle.snapshot().await.unwrap();
    assert_eq!(snap.inference_idle_timeouts, 2);

    handle.shutdown();
    actor_handle.await.unwrap();
}

#[tokio::test]
async fn test_doom_loop_recovery_counters() {
    let (handle, actor) = SessionSignalsActor::new();
    let actor_handle = tokio::spawn(actor.run());

    handle.record_doom_loop_recovery_attempt(
        vec!["tail_repetition:8@thinking".to_string()],
        Some(421),
    );
    // Terminal detection: no chunk index, tighter trigger.
    handle.record_doom_loop_recovery_attempt(
        vec![
            "tail_repetition:16@thinking".to_string(),
            "tail_repetition:4@thinking".to_string(),
        ],
        None,
    );
    handle.record_doom_loop_accepted_after_budget(vec!["tail_repetition:8@thinking".to_string()]);

    let snap = handle.snapshot().await.unwrap();
    assert_eq!(snap.doom_loop_recovery_attempts, 2);
    assert_eq!(snap.doom_loop_recovery_accepted_after_budget, 1);
    assert_eq!(snap.doom_loop_recovery_aborted_chunks, 421);
    assert_eq!(
        snap.doom_loop_recovery_top_trigger.as_deref(),
        Some("tail_repetition:4@thinking"),
        "tightest label observed across all recovery events"
    );

    handle.shutdown();
    actor_handle.await.unwrap();
}

#[tokio::test]
async fn test_set_tracing_config() {
    let (handle, actor) = SessionSignalsActor::new();
    let actor_handle = tokio::spawn(actor.run());

    handle.set_tracing_config(300);

    let snap = handle.snapshot().await.unwrap();
    assert_eq!(snap.inference_idle_timeout_configured_secs, Some(300));

    handle.shutdown();
    actor_handle.await.unwrap();
}

#[tokio::test]
async fn test_gcs_queue_snapshot() {
    let (handle, actor) = SessionSignalsActor::new();
    let actor_handle = tokio::spawn(actor.run());

    let _ = handle.tx.send(SignalEvent::RecordGcsQueueSnapshot {
        enqueued: 50,
        uploaded: 48,
        failed: 1,
        fallbacks: 1,
        circuit_breaker_trips: 0,
        pending: 3,
        pending_bytes: 1_048_576,
        orphans_cleaned: 5,
    });

    let snap = handle.snapshot().await.unwrap();
    assert_eq!(snap.gcs_queue_enqueued, 50);
    assert_eq!(snap.gcs_queue_uploaded, 48);
    assert_eq!(snap.gcs_queue_failed, 1);
    assert_eq!(snap.gcs_queue_fallbacks, 1);
    assert_eq!(snap.gcs_queue_circuit_breaker_trips, 0);
    assert_eq!(snap.gcs_queue_pending, 3);
    assert_eq!(snap.gcs_queue_pending_bytes, 1_048_576);
    assert_eq!(snap.gcs_queue_orphans_cleaned, 5);

    handle.shutdown();
    actor_handle.await.unwrap();
}

#[test]
fn test_sample_rss_bytes_returns_nonzero() {
    let rss = sample_rss_bytes();
    // On macOS and Linux, RSS should be > 0 for any running process
    assert!(rss > 0, "RSS should be > 0, got {rss}");
    // Sanity upper bound: process RSS should be under 10 GB
    assert!(
        rss < 10 * 1024 * 1024 * 1024,
        "RSS unreasonably large: {rss} bytes ({:.1} GB) — possible sign extension bug",
        rss as f64 / 1024.0 / 1024.0 / 1024.0
    );
}

#[test]
fn test_sample_rss_bytes_is_stable() {
    // Two consecutive calls should return similar values (no wild swings)
    let rss1 = sample_rss_bytes();
    let rss2 = sample_rss_bytes();
    assert!(rss1 > 0);
    assert!(rss2 > 0);
    // The two samples should be within 10x of each other
    let ratio = rss1.max(rss2) as f64 / rss1.min(rss2) as f64;
    assert!(
        ratio < 10.0,
        "RSS samples diverged too much: {rss1} vs {rss2} (ratio {ratio:.1}x)"
    );
}

#[tokio::test]
async fn test_peak_rss_recorded_at_turn_end() {
    let (handle, actor) = SessionSignalsActor::new();
    let actor_handle = tokio::spawn(actor.run());

    handle.increment_turn();
    let snapshot = handle.take_turn_end_snapshot().await.unwrap();

    // peak_rss_bytes should have been sampled during the turn-end snapshot
    assert!(
        snapshot.current.peak_rss_bytes > 0,
        "peak_rss_bytes should be > 0 after turn end, got {}",
        snapshot.current.peak_rss_bytes
    );

    handle.shutdown();
    actor_handle.await.unwrap();
}

/// Regression test for Windows Instant underflow panic.
///
/// When `session_duration_seconds` exceeds system uptime, the old code
/// `Instant::now() - Duration::from_secs(d)` panicked. The fix uses
/// `checked_sub` and falls back to `Instant::now()`.
#[tokio::test]
async fn test_restore_signals_with_huge_duration_does_not_panic() {
    let (handle, actor) = SessionSignalsActor::new();
    let actor_handle = tokio::spawn(actor.run());

    // A duration larger than any realistic uptime — would panic before the fix.
    let signals = SessionSignals {
        session_duration_seconds: u64::MAX,
        ..Default::default()
    };

    handle.restore_signals(signals);

    // If we reach here without panicking, the fix works. Verify the
    // snapshot is still usable and duration was reset to ~0.
    let snapshot = handle.snapshot().await.unwrap();
    assert!(
        snapshot.session_duration_seconds < 5,
        "After overflow fallback, duration should be near 0, got {}",
        snapshot.session_duration_seconds
    );

    handle.shutdown();
    actor_handle.await.unwrap();
}

#[test]
fn tool_duration_serializes_camel_case_with_call_id() {
    let d = ToolDuration {
        tool_name: "run_terminal_command".into(),
        tool_call_id: "call_abc".into(),
        duration_ms: 4_720,
    };
    let v = serde_json::to_value(&d).unwrap();
    assert_eq!(v["toolName"], "run_terminal_command");
    assert_eq!(v["toolCallId"], "call_abc");
    assert_eq!(v["durationMs"], 4720);
}

#[test]
fn tool_duration_deserializes_legacy_without_call_id() {
    let v = serde_json::json!({
        "toolName": "bash",
        "durationMs": 12
    });
    let d: ToolDuration = serde_json::from_value(v).unwrap();
    assert_eq!(d.tool_name, "bash");
    assert!(d.tool_call_id.is_empty());
    assert_eq!(d.duration_ms, 12);
}

#[tokio::test]
async fn record_tool_duration_includes_call_id_in_turn_delta() {
    let (handle, actor) = SessionSignalsActor::new();
    let actor_handle = tokio::spawn(actor.run());

    handle.record_tool_duration("bash", "call_1", 5_000);
    let snap = handle.take_turn_end_snapshot().await.unwrap();
    assert_eq!(snap.delta.tool_durations_this_turn.len(), 1);
    let td = &snap.delta.tool_durations_this_turn[0];
    assert_eq!(td.tool_name, "bash");
    assert_eq!(td.tool_call_id, "call_1");
    assert_eq!(td.duration_ms, 5_000);

    handle.shutdown();
    actor_handle.await.unwrap();
}
