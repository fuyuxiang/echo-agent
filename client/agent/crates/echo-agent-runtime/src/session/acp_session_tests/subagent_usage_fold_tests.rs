//! Subagent usage attribution and incomplete-bill gates.
use super::support::*;
use super::*;

async fn make_actor() -> SessionActor {
    let (gateway_tx, _gateway_rx) =
        tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
    let (persistence_tx, _persistence_rx) =
        tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
    create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await
}

fn usage_rows() -> Vec<(String, echo_agent_chat_state::UsageTotals)> {
    vec![(
        "m".into(),
        echo_agent_chat_state::UsageTotals {
            input_tokens: 40,
            model_calls: 1,
            ..Default::default()
        },
    )]
}

#[tokio::test(flavor = "current_thread")]
async fn subagent_usage_fold_attribution_gate() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let actor = make_actor().await;
            let usage = usage_rows();

            *actor.current_prompt_id.lock().unwrap() = Some("p-1".into());
            assert_eq!(
                actor
                    .record_subagent_usage(&usage, Some("p-1"), false)
                    .await,
                Ok(super::updates::SubagentUsageApply::AttributedToPrompt)
            );
            assert_eq!(
                actor
                    .chat_state_handle
                    .try_get_prompt_usage()
                    .await
                    .unwrap()
                    .unwrap()
                    .totals
                    .input_tokens,
                40
            );

            actor.chat_state_handle.increment_prompt_index();
            for (live, stamped) in [
                (Some("p-2"), Some("p-1")),
                (Some("p-1"), None),
                (None, Some("p-1")),
            ] {
                *actor.current_prompt_id.lock().unwrap() = live.map(str::to_string);
                // Session-only: ledger apply ok, not attributed to live prompt.
                assert_eq!(
                    actor.record_subagent_usage(&usage, stamped, false).await,
                    Ok(super::updates::SubagentUsageApply::SessionOnly)
                );
                assert!(
                    actor
                        .chat_state_handle
                        .try_get_prompt_usage()
                        .await
                        .ok()
                        .flatten()
                        .is_none()
                );
            }
            assert_eq!(
                actor
                    .chat_state_handle
                    .try_get_session_usage()
                    .await
                    .expect("chat-state actor alive")
                    .totals
                    .input_tokens,
                160
            );
        })
        .await;
}

/// One matrix for the shared freeze/cancel outcome policy, including the
/// `usage_incomplete_from_reply` wrapper the error path uses.
#[test]
fn usage_drain_outcome_policy_matches_freeze_and_cancel() {
    use super::turn::UsageDrainOutcome;
    use echo_agent_tools::implementations::echo_agent_build::task::types::SubagentOutstandingReply;

    let none = UsageDrainOutcome::from_outstanding_reply(None);
    assert!(none.fail_closed);
    assert!(none.report_incomplete());
    assert!(SessionActor::usage_incomplete_from_reply(None));

    let fg_reply = SubagentOutstandingReply {
        live_ids: vec!["s1".into()],
        background_live: false,
        subagent_usage_not_applied: false,
    };
    let fg = UsageDrainOutcome::from_outstanding_reply(Some(&fg_reply));
    assert!(fg.fail_closed);
    assert!(fg.report_incomplete());
    assert!(SessionActor::usage_incomplete_from_reply(Some(&fg_reply)));

    let sticky_reply = SubagentOutstandingReply {
        live_ids: vec![],
        background_live: false,
        subagent_usage_not_applied: true,
    };
    let sticky = UsageDrainOutcome::from_outstanding_reply(Some(&sticky_reply));
    assert!(!sticky.fail_closed, "sticky is report-only");
    assert!(sticky.sticky_report);
    assert!(sticky.report_incomplete());
    assert!(SessionActor::usage_incomplete_from_reply(Some(
        &sticky_reply
    )));

    let bg_reply = SubagentOutstandingReply {
        live_ids: vec![],
        background_live: true,
        subagent_usage_not_applied: false,
    };
    let bg = UsageDrainOutcome::from_outstanding_reply(Some(&bg_reply));
    assert!(!bg.fail_closed, "background is report-only");
    assert!(bg.background_live);
    assert!(bg.report_incomplete());
    assert!(SessionActor::usage_incomplete_from_reply(Some(&bg_reply)));

    let clean_reply = SubagentOutstandingReply {
        live_ids: vec![],
        background_live: false,
        subagent_usage_not_applied: false,
    };
    let clean = UsageDrainOutcome::from_outstanding_reply(Some(&clean_reply));
    assert!(!clean.fail_closed);
    assert!(!clean.report_incomplete());
    assert!(!SessionActor::usage_incomplete_from_reply(Some(
        &clean_reply
    )));
}

#[test]
fn project_from_ledger_never_drops_incomplete_flag() {
    use crate::extensions::notification::PromptUsage;

    assert!(PromptUsage::project_from_ledger(None, false).is_none());
    assert!(
        PromptUsage::project_from_ledger(None, true)
            .unwrap()
            .usage_is_incomplete
    );

    let mut ledger = echo_agent_chat_state::UsageLedger::default();
    ledger.record_main_loop_call(
        "m",
        &echo_agent_sampling_types::TokenUsage {
            prompt_tokens: 3,
            completion_tokens: 1,
            total_tokens: 4,
            reasoning_tokens: 0,
            cached_prompt_tokens: 0,
            cache_creation_prompt_tokens: 0,
        },
        None,
        None,
    );
    let complete = PromptUsage::project_from_ledger(Some(&ledger), false).unwrap();
    assert!(!complete.usage_is_incomplete);
    assert_eq!(complete.totals.input_tokens, 3);

    let marked = PromptUsage::project_from_ledger(Some(&ledger), true).unwrap();
    assert!(marked.usage_is_incomplete);
    assert_eq!(marked.totals.input_tokens, 3);
}

#[tokio::test(flavor = "current_thread")]
async fn nested_incomplete_fold_marks_parent_ledger() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let actor = make_actor().await;
            *actor.current_prompt_id.lock().unwrap() = Some("p-1".into());
            let usage = usage_rows();
            assert_eq!(
                actor.record_subagent_usage(&usage, Some("p-1"), true).await,
                Ok(super::updates::SubagentUsageApply::AttributedToPrompt)
            );
            let prompt = actor
                .chat_state_handle
                .try_get_prompt_usage()
                .await
                .unwrap()
                .expect("prompt ledger");
            assert!(prompt.incomplete);
            assert_eq!(prompt.totals.input_tokens, 40);
            assert!(
                actor
                    .chat_state_handle
                    .try_get_session_usage()
                    .await
                    .expect("chat-state actor alive")
                    .incomplete
            );
        })
        .await;
}

#[test]
fn for_error_path_shared_policy() {
    use crate::extensions::notification::PromptUsage;

    assert!(PromptUsage::for_error_path(None, false).is_none());
    assert!(
        PromptUsage::for_error_path(None, true)
            .unwrap()
            .usage_is_incomplete
    );

    let mut ledger = echo_agent_chat_state::UsageLedger::default();
    ledger.record_main_loop_call(
        "m",
        &echo_agent_sampling_types::TokenUsage {
            prompt_tokens: 5,
            completion_tokens: 1,
            total_tokens: 6,
            reasoning_tokens: 0,
            cached_prompt_tokens: 0,
            cache_creation_prompt_tokens: 0,
        },
        None,
        None,
    );
    let with_ledger = PromptUsage::for_error_path(Some(&ledger), false).unwrap();
    assert!(with_ledger.usage_is_incomplete);
    assert_eq!(with_ledger.totals.input_tokens, 5);
}

#[tokio::test(flavor = "current_thread")]
async fn error_path_omits_usage_when_never_billed() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let actor = make_actor().await;
            assert!(actor.error_path_usage_fallback("p-1").await.is_none());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn error_path_marks_incomplete_when_ledger_open() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let actor = make_actor().await;
            actor.chat_state_handle.record_model_call_usage(
                Some("m".into()),
                echo_agent_sampling_types::TokenUsage {
                    prompt_tokens: 10,
                    completion_tokens: 2,
                    total_tokens: 12,
                    reasoning_tokens: 0,
                    cached_prompt_tokens: 0,
                    cache_creation_prompt_tokens: 0,
                },
                None,
                None,
            );
            let usage = actor.error_path_usage_fallback("p-1").await.unwrap();
            assert!(usage.usage_is_incomplete);
            assert_eq!(usage.totals.input_tokens, 10);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn session_only_incomplete_does_not_stain_live_open_prompt() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let actor = make_actor().await;
            *actor.current_prompt_id.lock().unwrap() = Some("p-live".into());
            // Open a live prompt ledger via a main-loop call.
            actor.chat_state_handle.record_model_call_usage(
                Some("m".into()),
                echo_agent_sampling_types::TokenUsage {
                    prompt_tokens: 7,
                    completion_tokens: 1,
                    total_tokens: 8,
                    reasoning_tokens: 0,
                    cached_prompt_tokens: 0,
                    cache_creation_prompt_tokens: 0,
                },
                None,
                None,
            );
            let usage = usage_rows();
            // Stamped pin ≠ live pin → session-only.
            assert_eq!(
                actor
                    .record_subagent_usage(&usage, Some("p-stamped"), true)
                    .await,
                Ok(super::updates::SubagentUsageApply::SessionOnly)
            );
            let prompt = actor
                .chat_state_handle
                .try_get_prompt_usage()
                .await
                .unwrap()
                .expect("prompt ledger");
            assert!(
                !prompt.incomplete,
                "live open prompt must not inherit session-only incomplete"
            );
            assert_eq!(prompt.totals.input_tokens, 7);
            assert!(
                actor
                    .chat_state_handle
                    .try_get_session_usage()
                    .await
                    .expect("chat-state actor alive")
                    .incomplete
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn snapshot_ors_ledger_incomplete_even_when_reply_complete() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let actor = make_actor().await;
            *actor.current_prompt_id.lock().unwrap() = Some("p-1".into());
            let usage = usage_rows();
            assert_eq!(
                actor.record_subagent_usage(&usage, Some("p-1"), true).await,
                Ok(super::updates::SubagentUsageApply::AttributedToPrompt)
            );
            // Orchestration says complete (no live/sticky); ledger still incomplete.
            let snap = actor.snapshot_prompt_usage_marked(false).await.unwrap();
            assert!(snap.usage_is_incomplete);
            assert_eq!(snap.totals.input_tokens, 40);
        })
        .await;
}

/// Scripted coordinator stub: answers each `Outstanding` query with the next
/// queued reply, repeating the last one; other events are ignored.
fn scripted_outstanding_responder(
    replies: Vec<
        echo_agent_tools::implementations::echo_agent_build::task::types::SubagentOutstandingReply,
    >,
) -> tokio::sync::mpsc::UnboundedSender<
    echo_agent_tools::implementations::echo_agent_build::task::types::SubagentEvent,
> {
    use echo_agent_tools::implementations::echo_agent_build::task::types::SubagentEvent;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<SubagentEvent>();
    tokio::task::spawn_local(async move {
        let mut queue = replies.into_iter();
        let mut last = None;
        while let Some(event) = rx.recv().await {
            if let SubagentEvent::Outstanding(req) = event {
                let reply = queue.next().or_else(|| last.clone()).unwrap_or_default();
                last = Some(reply.clone());
                let _ = req.respond_to.send(reply);
            }
        }
    });
    tx
}

/// Drain timeout (wedged foreground child) fails closed: the report and both
/// ledgers are marked incomplete.
#[tokio::test(flavor = "current_thread")]
async fn freeze_timeout_marks_report_and_both_ledgers() {
    use echo_agent_tools::implementations::echo_agent_build::task::types::SubagentOutstandingReply;
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut actor = make_actor().await;
            actor.tool_context.subagent_event_tx = Some(scripted_outstanding_responder(vec![
                SubagentOutstandingReply {
                    live_ids: vec!["wedged".into()],
                    background_live: false,
                    subagent_usage_not_applied: false,
                },
            ]));
            let usage = actor
                .freeze_prompt_usage_bounded("p-1", std::time::Duration::from_millis(120))
                .await
                .expect("incomplete usage always attaches");
            assert!(usage.usage_is_incomplete);
            let prompt = actor
                .chat_state_handle
                .try_get_prompt_usage()
                .await
                .unwrap()
                .expect("fail-closed mark opens the prompt ledger");
            assert!(prompt.incomplete);
            assert!(
                actor
                    .chat_state_handle
                    .try_get_session_usage()
                    .await
                    .unwrap()
                    .incomplete
            );
        })
        .await;
}

/// A live background child flags only the report: no ledger is marked,
/// because its fold still lands on the session ledger at completion.
#[tokio::test(flavor = "current_thread")]
async fn freeze_background_only_flags_report_not_ledgers() {
    use echo_agent_tools::implementations::echo_agent_build::task::types::SubagentOutstandingReply;
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut actor = make_actor().await;
            actor.tool_context.subagent_event_tx = Some(scripted_outstanding_responder(vec![
                SubagentOutstandingReply {
                    live_ids: vec![],
                    background_live: true,
                    subagent_usage_not_applied: false,
                },
            ]));
            let usage = actor
                .freeze_prompt_usage_bounded("p-1", std::time::Duration::from_millis(120))
                .await
                .expect("incomplete usage always attaches");
            assert!(usage.usage_is_incomplete, "report is incomplete");
            let prompt = actor
                .chat_state_handle
                .try_get_prompt_usage()
                .await
                .unwrap();
            assert!(
                prompt.is_none_or(|l| !l.incomplete),
                "background child must not mark the prompt ledger"
            );
            assert!(
                !actor
                    .chat_state_handle
                    .try_get_session_usage()
                    .await
                    .unwrap()
                    .incomplete,
                "session ledger stays unflagged: the fold still lands there"
            );
        })
        .await;
}

/// Cancel/freeze share `finalize_usage_from_outcome`: bg-only is report incomplete only.
#[tokio::test(flavor = "current_thread")]
async fn finalize_background_only_flags_report_not_ledgers() {
    use super::turn::UsageDrainOutcome;
    use echo_agent_tools::implementations::echo_agent_build::task::types::SubagentOutstandingReply;

    tokio::task::LocalSet::new()
        .run_until(async {
            let actor = make_actor().await;
            actor.chat_state_handle.record_model_call_usage(
                Some("m".into()),
                echo_agent_sampling_types::TokenUsage {
                    prompt_tokens: 4,
                    completion_tokens: 1,
                    total_tokens: 5,
                    reasoning_tokens: 0,
                    cached_prompt_tokens: 0,
                    cache_creation_prompt_tokens: 0,
                },
                None,
                None,
            );
            let outcome =
                UsageDrainOutcome::from_outstanding_reply(Some(&SubagentOutstandingReply {
                    live_ids: vec![],
                    background_live: true,
                    subagent_usage_not_applied: false,
                }));
            assert!(!outcome.fail_closed);
            let usage = actor
                .finalize_usage_from_outcome("p-1", outcome)
                .await
                .expect("billed prompt attaches");
            assert!(usage.usage_is_incomplete);
            assert_eq!(usage.totals.input_tokens, 4);
            assert!(
                !actor
                    .chat_state_handle
                    .try_get_session_usage()
                    .await
                    .unwrap()
                    .incomplete,
                "bg-only must not stain session ledger"
            );
            let prompt = actor
                .chat_state_handle
                .try_get_prompt_usage()
                .await
                .unwrap()
                .expect("prompt ledger open");
            assert!(!prompt.incomplete, "bg-only must not stain prompt ledger");
        })
        .await;
}

/// Pin-aware apply-miss: stamped pin ≠ live open prompt stains session only.
#[tokio::test(flavor = "current_thread")]
async fn apply_miss_mismatched_pin_does_not_stain_live_prompt() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let actor = make_actor().await;
            *actor.current_prompt_id.lock().unwrap() = Some("p-live".into());
            actor.chat_state_handle.record_model_call_usage(
                Some("m".into()),
                echo_agent_sampling_types::TokenUsage {
                    prompt_tokens: 11,
                    completion_tokens: 1,
                    total_tokens: 12,
                    reasoning_tokens: 0,
                    cached_prompt_tokens: 0,
                    cache_creation_prompt_tokens: 0,
                },
                None,
                None,
            );
            assert!(actor.mark_apply_miss_incomplete(Some("p-stamped")).await);
            let live = actor
                .chat_state_handle
                .try_get_prompt_usage()
                .await
                .unwrap()
                .expect("live prompt ledger");
            assert!(
                !live.incomplete,
                "mismatched pin must not stain the live open prompt"
            );
            assert_eq!(live.totals.input_tokens, 11);
            assert!(
                actor
                    .chat_state_handle
                    .try_get_session_usage()
                    .await
                    .unwrap()
                    .incomplete
            );
        })
        .await;
}

/// Pin-aware apply-miss: matching pin stains both ledgers.
#[tokio::test(flavor = "current_thread")]
async fn apply_miss_matching_pin_stains_prompt_and_session() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let actor = make_actor().await;
            *actor.current_prompt_id.lock().unwrap() = Some("p-1".into());
            actor.chat_state_handle.record_model_call_usage(
                Some("m".into()),
                echo_agent_sampling_types::TokenUsage {
                    prompt_tokens: 3,
                    completion_tokens: 1,
                    total_tokens: 4,
                    reasoning_tokens: 0,
                    cached_prompt_tokens: 0,
                    cache_creation_prompt_tokens: 0,
                },
                None,
                None,
            );
            assert!(actor.mark_apply_miss_incomplete(Some("p-1")).await);
            assert!(
                actor
                    .chat_state_handle
                    .try_get_prompt_usage()
                    .await
                    .unwrap()
                    .expect("prompt")
                    .incomplete
            );
            assert!(
                actor
                    .chat_state_handle
                    .try_get_session_usage()
                    .await
                    .unwrap()
                    .incomplete
            );
        })
        .await;
}

/// Sticky (session-only) is report-only on freeze: session ledger stays complete.
#[tokio::test(flavor = "current_thread")]
async fn freeze_sticky_only_flags_report_not_ledgers() {
    use echo_agent_tools::implementations::echo_agent_build::task::types::SubagentOutstandingReply;
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut actor = make_actor().await;
            actor.chat_state_handle.record_model_call_usage(
                Some("m".into()),
                echo_agent_sampling_types::TokenUsage {
                    prompt_tokens: 9,
                    completion_tokens: 1,
                    total_tokens: 10,
                    reasoning_tokens: 0,
                    cached_prompt_tokens: 0,
                    cache_creation_prompt_tokens: 0,
                },
                None,
                None,
            );
            actor.tool_context.subagent_event_tx = Some(scripted_outstanding_responder(vec![
                SubagentOutstandingReply {
                    live_ids: vec![],
                    background_live: false,
                    subagent_usage_not_applied: true,
                },
            ]));
            let usage = actor
                .freeze_prompt_usage_bounded("p-1", std::time::Duration::from_millis(120))
                .await
                .expect("incomplete usage always attaches");
            assert!(usage.usage_is_incomplete);
            assert!(
                !actor
                    .chat_state_handle
                    .try_get_session_usage()
                    .await
                    .unwrap()
                    .incomplete,
                "sticky session-only must not stain session ledger"
            );
            let prompt = actor
                .chat_state_handle
                .try_get_prompt_usage()
                .await
                .unwrap()
                .expect("prompt ledger open");
            assert!(
                !prompt.incomplete,
                "sticky session-only must not stain prompt ledger"
            );
        })
        .await;
}

/// A fold landing mid-drain completes cleanly: no incomplete flag anywhere.
#[tokio::test(flavor = "current_thread")]
async fn freeze_completes_when_fold_lands_mid_drain() {
    use echo_agent_tools::implementations::echo_agent_build::task::types::SubagentOutstandingReply;
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut actor = make_actor().await;
            actor.chat_state_handle.record_model_call_usage(
                Some("m".into()),
                echo_agent_sampling_types::TokenUsage {
                    prompt_tokens: 10,
                    completion_tokens: 2,
                    total_tokens: 12,
                    reasoning_tokens: 0,
                    cached_prompt_tokens: 0,
                    cache_creation_prompt_tokens: 0,
                },
                None,
                None,
            );
            actor.tool_context.subagent_event_tx = Some(scripted_outstanding_responder(vec![
                SubagentOutstandingReply {
                    live_ids: vec!["finishing".into()],
                    background_live: false,
                    subagent_usage_not_applied: false,
                },
                SubagentOutstandingReply::default(),
            ]));
            let usage = actor
                .freeze_prompt_usage_bounded("p-1", std::time::Duration::from_secs(5))
                .await
                .expect("billed prompt attaches usage");
            assert!(!usage.usage_is_incomplete);
            assert_eq!(usage.totals.input_tokens, 10);
            assert!(
                !actor
                    .chat_state_handle
                    .try_get_session_usage()
                    .await
                    .unwrap()
                    .incomplete
            );
        })
        .await;
}
