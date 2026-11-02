// Run the production group state machine without starting a desktop or Runtime.
extern crate self as echo_agent_memory;
#[path = "../src/groups/model.rs"]
mod model;
#[path = "../../vendor/echo-agent-build/crates/codegen/echo-agent-memory/src/text_utils.rs"]
pub mod text_utils;

use model::{Group, Member};

fn fixture(discussion: bool) -> (Group, String) {
    let member = Member {
        id: "member-a".into(),
        name: "A".into(),
        expert: Some("A".into()),
        role: "delivery".into(),
        profile: serde_json::json!({}),
        model_id: "M3".into(),
        session_id: Some("execution-before-stop".into()),
        discussion_session_id: Some("discussion-before-stop".into()),
        seen_seq: 9,
        discussion_seen_seq: 8,
        generation: 4,
        history_start_seq: 3,
        removed: false,
    };
    let mut group = Group {
        id: "group".into(),
        name: "QA".into(),
        goal: "bounded delivery".into(),
        cwd: "/isolated".into(),
        source_session_id: None,
        source_team_id: None,
        coordinator_id: member.id.clone(),
        permission_mode: "ask".into(),
        members: vec![member],
        messages: vec![],
        works: vec![],
        decisions: vec![],
        archived: false,
        pinned: false,
        paused: false,
        epoch: 0,
        revision: 1,
        created_at: 0,
        updated_at: 0,
    };
    let work = group
        .enqueue(
            "request",
            "member-a",
            "create exact file".into(),
            discussion,
            None,
            0,
        )
        .unwrap();
    let sid = if discussion {
        "discussion-before-stop"
    } else {
        "execution-before-stop"
    };
    group.works[0].session_id = Some(sid.into());
    group.works[0].status = "cancelled".into();
    group
        .post(None, "user", "create exact file".into(), None)
        .id = "request".into();
    (group, work)
}

#[test]
fn cancelled_execution_retry_uses_fresh_actor_without_erasing_memory_or_history() {
    let (mut group, work) = fixture(false);
    let old = group.members[0].clone();
    group.retry_work(&work).unwrap();
    let member = &group.members[0];
    assert_eq!(
        member.session_id, None,
        "a cancelled actor must not own the retry's next permission request"
    );
    assert_eq!(member.discussion_session_id, old.discussion_session_id);
    assert_eq!(
        member.generation, old.generation,
        "long-term memory must survive retry"
    );
    assert_eq!(member.history_start_seq, old.history_start_seq);
    assert_eq!(member.seen_seq, member.history_start_seq);
    assert_eq!(member.model_id, old.model_id);
    assert_eq!(
        group.works[0].session_id.as_deref(),
        Some("execution-before-stop")
    );
    assert_eq!(group.works[0].status, "cancelled");
    assert!(group
        .works
        .last()
        .unwrap()
        .prompt
        .contains("create exact file"));
    assert_eq!(group.messages.len(), 2);
}

#[test]
fn cancelled_discussion_retry_retires_only_discussion_context() {
    let (mut group, work) = fixture(true);
    let old = group.members[0].clone();
    group.retry_work(&work).unwrap();
    let member = &group.members[0];
    assert_eq!(member.discussion_session_id, None);
    assert_eq!(member.session_id, old.session_id);
    assert_eq!(member.discussion_seen_seq, member.history_start_seq);
    assert_eq!(member.generation, old.generation);
}

#[test]
fn retry_of_old_cancelled_work_keeps_a_newer_member_context() {
    let (mut group, work) = fixture(false);
    group.members[0].session_id = Some("newer-context".into());
    group.retry_work(&work).unwrap();
    assert_eq!(
        group.members[0].session_id.as_deref(),
        Some("newer-context")
    );
}

#[test]
fn retry_never_retires_a_context_owned_by_other_active_work() {
    let (mut group, work) = fixture(false);
    group
        .enqueue(
            "new-request",
            "member-a",
            "current work".into(),
            false,
            None,
            0,
        )
        .unwrap();
    group.works[1].status = "running".into();
    group.works[1].session_id = group.members[0].session_id.clone();
    let old = group.members[0].clone();
    group.retry_work(&work).unwrap();
    assert_eq!(group.members[0], old);
}

#[test]
fn failed_model_request_keeps_its_completed_context_for_normal_retry() {
    let (mut group, work) = fixture(false);
    group.works[0].status = "failed".into();
    let old = group.members[0].clone();
    group.retry_work(&work).unwrap();
    assert_eq!(group.members[0], old);
}

#[test]
fn empty_success_gets_one_read_only_recovery_and_a_public_delivery() {
    let (mut group, original) = fixture(false);
    group.works[0].status = "running".into();
    group.messages[0].id = "request".into();
    group.finish_execution(&original, None, false);
    assert_eq!(group.works.len(), 2);
    let recovery = group.works[1].id.clone();
    assert!(group.works[1].read_only());
    assert!(group.works[1].result_repair);
    assert_eq!(group.works[0].status, "waiting_children");
    group.works[1].status = "running".into();
    group.works[1].output = "已核对已有文件，结果可用。".into();
    group.finish_execution(&recovery, None, false);
    let snapshot = group.snapshot();
    assert_eq!(snapshot.requests[0].state, "delivered");
    let replies = snapshot
        .group
        .messages
        .iter()
        .filter(|m| m.kind == "assistant")
        .collect::<Vec<_>>();
    assert_eq!(
        replies.len(),
        1,
        "propagated results must not create duplicate avatars"
    );
    assert_eq!(
        snapshot.requests[0].final_message_id.as_deref(),
        Some(replies[0].id.as_str())
    );
}

#[test]
fn empty_recovery_fails_visibly_without_an_infinite_repair_loop() {
    let (mut group, original) = fixture(false);
    group.works[0].status = "running".into();
    group.finish_execution(&original, None, false);
    let recovery = group.works[1].id.clone();
    group.works[1].status = "running".into();
    group.finish_execution(&recovery, None, false);
    assert_eq!(group.works.len(), 2);
    assert!(group.works.iter().all(|work| work.status == "failed"));
    assert_eq!(group.snapshot().requests[0].state, "failed");
}

#[test]
fn legacy_budgets_are_ignored_and_retained_text_is_recovered_without_execution() {
    let (mut group, _) = fixture(false);
    group.works[0].status = "completed".into();
    group.works[0].tokens = 10_000_000;
    group.works[0].output = "历史中实际保留的公开回复".into();
    let mut legacy = serde_json::to_value(&group).unwrap();
    legacy["tokenBudget"] = 1.into();
    legacy["maxCalls"] = 1.into();
    let restored: Group = serde_json::from_value(legacy).unwrap();
    let snapshot = restored.snapshot();
    assert_eq!(snapshot.requests[0].state, "delivered");
    assert_eq!(snapshot.group.works.len(), 1);
    assert_eq!(
        snapshot.group.messages.last().unwrap().text,
        "历史中实际保留的公开回复"
    );
    let saved = serde_json::to_value(&snapshot.group).unwrap();
    assert!(saved.get("tokenBudget").is_none());
    assert!(saved.get("maxCalls").is_none());
}

#[test]
fn stopped_work_is_not_repaired_and_manual_summary_creates_a_read_only_request() {
    let (mut group, original) = fixture(false);
    group.finish_execution(&original, None, true);
    assert_eq!(group.works.len(), 1);
    group.summarize_request(&original).unwrap();
    let summary = group.works.last().unwrap();
    assert!(summary.read_only());
    assert!(summary.summary);
    assert_ne!(summary.root_id, group.works[0].root_id);
    assert_eq!(group.works[0].status, "cancelled");
}

#[test]
fn repeated_members_and_deep_work_are_allowed_without_token_or_call_caps() {
    let (mut group, _) = fixture(false);
    group.works[0].tokens = 9_000_000;
    for depth in 1..30 {
        group
            .enqueue(
                "request",
                "member-a",
                format!("review-{depth}"),
                false,
                None,
                depth,
            )
            .unwrap();
    }
    assert_eq!(group.works.len(), 30);
}

#[test]
fn paused_dispatch_keeps_read_only_recovery_queued_and_uses_unique_message_sequences() {
    let (mut group, original) = fixture(false);
    group.works[0].status = "running".into();
    group.paused = true;
    group.finish_execution(&original, None, false);
    assert!(group.paused);
    assert_eq!(group.works[1].status, "queued");
    let recovery = group.works[1].id.clone();
    group.paused = false;
    group.works[1].status = "running".into();
    group.works[1].output = "已确认真实结果".into();
    let previous = group.messages.last().unwrap().seq;
    group.finish_execution(&recovery, None, false);
    assert!(group.messages.last().unwrap().seq > previous);
    let mut sequences = group.messages.iter().map(|m| m.seq).collect::<Vec<_>>();
    sequences.sort();
    sequences.dedup();
    assert_eq!(sequences.len(), group.messages.len());
}

#[test]
fn read_only_recovery_binds_to_discussion_context_without_overwriting_execution() {
    let (mut group, original) = fixture(false);
    group.works[0].status = "running".into();
    group.finish_execution(&original, None, false);
    let recovery = group.works[1].id.clone();
    group.works[1].status = "dispatching".into();
    assert!(group.bind_session(&recovery, "read-only-recovery"));
    assert_eq!(
        group.members[0].session_id.as_deref(),
        Some("execution-before-stop")
    );
    assert_eq!(
        group.members[0].discussion_session_id.as_deref(),
        Some("read-only-recovery")
    );
}

#[test]
fn cancelling_one_child_still_allows_the_owner_to_deliver_a_partial_conclusion() {
    let (mut group, original) = fixture(false);
    let mut expert = group.members[0].clone();
    expert.id = "member-b".into();
    group.members.push(expert);
    group.works[0].status = "running".into();
    let child = group
        .enqueue(
            "request",
            "member-b",
            "复核".into(),
            false,
            Some(original.clone()),
            1,
        )
        .unwrap();
    group.finish(&original, None, false);
    group.stop_work(Some(&child)).unwrap();
    group.ready_callbacks();
    let continuation = group.works.last().unwrap().id.clone();
    group.works.last_mut().unwrap().output = "已交付可确认部分；复核已停止，尚未完成。".into();
    group.finish_execution(&continuation, None, false);
    let snapshot = group.snapshot();
    assert_eq!(snapshot.requests[0].state, "partial");
    assert!(snapshot.requests[0].final_message_id.is_some());
}

#[test]
fn retrying_a_missing_reply_only_summarizes_instead_of_repeating_the_original_action() {
    let (mut group, original) = fixture(false);
    group.works[0].status = "running".into();
    group.finish_execution(&original, None, false);
    let recovery = group.works[1].id.clone();
    group.finish_execution(&recovery, None, false);
    group.retry_work(&original).unwrap();
    let next = group.works.last().unwrap();
    assert!(next.summary);
    assert!(next.read_only());
    assert_ne!(next.root_id, "request");
    assert_eq!(group.works[0].status, "failed");
}

#[test]
fn missing_reply_after_multiple_continuations_never_replays_the_original_action() {
    let (mut group, original) = fixture(false);
    group.works[0].status = "failed".into();
    for index in 0..5 {
        let mut continuation = group.works.last().unwrap().clone();
        continuation.continuation_of = Some(continuation.id.clone());
        continuation.id = format!("continuation-{index}");
        continuation.result_repair = index == 4;
        group.works.push(continuation);
    }
    assert!(group.result_only_retry(&original));
    group.retry_work(&original).unwrap();
    assert!(group.works.last().unwrap().summary);
    assert!(group.works.last().unwrap().read_only());
    assert_eq!(group.works[0].status, "failed");
}

#[test]
fn failed_summary_does_not_label_an_earlier_opinion_as_the_final_conclusion() {
    let (mut group, _) = fixture(true);
    group.works[0].status = "completed".into();
    group.works[0].output = "初步观点，不是汇总结论".into();
    let mut summary = group.works[0].clone();
    summary.id = "failed-summary".into();
    summary.summary = true;
    summary.status = "failed".into();
    summary.output.clear();
    group.works.push(summary);
    let snapshot = group.snapshot();
    assert_eq!(snapshot.requests[0].state, "failed");
    assert!(snapshot.requests[0].final_message_id.is_none());
}

#[test]
fn interrupted_result_repair_can_be_retried_as_a_new_read_only_summary() {
    let (mut group, original) = fixture(false);
    group.works[0].status = "running".into();
    group.finish_execution(&original, None, false);
    let recovery = group.works[1].id.clone();
    group.works[1].status = "uncertain".into();
    group.paused = true;
    group.retry_work(&recovery).unwrap();
    assert_eq!(group.works[1].status, "cancelled");
    assert!(group.works.last().unwrap().summary);
    assert!(group.works.last().unwrap().read_only());
}

#[test]
fn manual_result_summary_preserves_forgetting_and_history_cutoff() {
    let (mut group, original) = fixture(false);
    group.works[0].status = "completed".into();
    group.works[0].output = "FORGOTTEN_VALUE".into();
    group.messages[0].text = "FORGOTTEN_VALUE".into();
    group
        .summarize_request_in_context(&original, 0, |text| !text.contains("FORGOTTEN_VALUE"))
        .unwrap();
    assert!(!group
        .works
        .last()
        .unwrap()
        .prompt
        .contains("FORGOTTEN_VALUE"));
    assert!(group.messages.last().unwrap().reply_to.is_none());
    assert_eq!(
        group.works[0].output, "FORGOTTEN_VALUE",
        "public history is retained, not silently deleted"
    );

    let (mut group, original) = fixture(false);
    group.works[0].status = "completed".into();
    group.works[0].output = "PRE_RESET_RESULT".into();
    let cutoff = group.messages.last().unwrap().seq;
    group
        .summarize_request_in_context(&original, cutoff, |_| true)
        .unwrap();
    assert!(!group
        .works
        .last()
        .unwrap()
        .prompt
        .contains("PRE_RESET_RESULT"));
    assert!(group.messages.last().unwrap().reply_to.is_none());
}

#[test]
fn paused_unconsumed_followup_remains_queued_until_resume() {
    let (mut group, original) = fixture(false);
    group.works[0].status = "running".into();
    group.works[0].context_seq = group.messages[0].seq;
    group.works[0].output = "Original result".into();
    group.paused = true;
    group
        .post(None, "user", "Use the latest requirement".into(), None)
        .request_id = Some("request".into());
    group.finish_execution(&original, None, false);
    assert!(group.paused);
    assert_eq!(group.works.len(), 2);
    assert_eq!(group.works[1].status, "queued");
    group.works[1].status = "dispatching".into();
    assert!(!group.begin_prompt(&group.works[1].id.clone()));
    group.paused = false;
    let next = group.works[1].id.clone();
    group.works[1].context_seq = group.messages.last().unwrap().seq;
    group.works[1].output = "Corrected result".into();
    group.finish_execution(&next, None, false);
    assert!(group.works.iter().all(|work| work.terminal()));
}

#[test]
fn repeated_result_summary_preserves_latest_inputs_and_filters_forgotten_excerpts() {
    let (mut group, original) = fixture(false);
    group.works[0].status = "completed".into();
    group.works[0].output = "Earlier result".into();
    group.messages[0].knowledge.personal = true;
    group.messages[0].knowledge.context = "FORGOTTEN_EXCERPT".into();
    let followup = group.post(None, "user", "LATEST_CORRECTION".into(), None);
    followup.request_id = Some("request".into());
    followup.attachments = vec!["/new-input.pdf".into()];
    followup.knowledge.personal = true;
    followup.knowledge.context = "PERMITTED_NEW_EVIDENCE".into();
    let permits = |text: &str| !text.contains("FORGOTTEN_EXCERPT");
    group
        .summarize_request_in_context(&original, 0, permits)
        .unwrap();
    let summary = group.works.last().unwrap().clone();
    assert!(summary.prompt.contains("LATEST_CORRECTION"));
    assert!(summary.prompt.contains("PERMITTED_NEW_EVIDENCE"));
    assert!(group
        .request_attachments(&summary.root_id)
        .contains(&"/new-input.pdf".into()));
    assert!(group.messages.last().unwrap().knowledge.context.is_empty());
    group.works.last_mut().unwrap().status = "failed".into();
    group
        .summarize_request_in_context(&summary.id, 0, permits)
        .unwrap();
    let repeated = group.works.last().unwrap();
    assert!(repeated.prompt.contains("LATEST_CORRECTION"));
    assert!(repeated.prompt.contains("create exact file"));
    assert!(group
        .request_attachments(&repeated.root_id)
        .contains(&"/new-input.pdf".into()));
    assert_eq!(
        group.messages.last().unwrap().source_request_id.as_deref(),
        Some("request")
    );
}

#[test]
fn completed_multi_member_request_keeps_its_original_conclusion_after_coordinator_change() {
    let (mut group, _) = fixture(false);
    let mut expert = group.members[0].clone();
    expert.id = "member-b".into();
    group.members.push(expert);
    group
        .enqueue("request", "member-b", "Review".into(), false, None, 0)
        .unwrap();
    for work in &mut group.works {
        work.status = "completed".into();
        work.output = format!("Individual view {}", work.member_id);
    }
    group.ready_summaries();
    let summary = group.works.last().unwrap().id.clone();
    group.works.last_mut().unwrap().output = "Final conclusion".into();
    group.finish_execution(&summary, None, false);
    let before = group.clone().snapshot();
    group.coordinator_id = "member-b".into();
    let after = group.snapshot();
    assert_eq!(before.requests[0].owner_id, after.requests[0].owner_id);
    assert_eq!(
        before.requests[0].final_message_id,
        after.requests[0].final_message_id
    );
}

#[test]
fn public_revisions_track_content_changes_and_ignore_tool_updates() {
    let (mut group, _) = fixture(false);
    group.post(
        Some("member-a".into()),
        "assistant",
        "First part".into(),
        None,
    );
    let before = group.clone();
    group
        .messages
        .last_mut()
        .unwrap()
        .text
        .push_str("; final conclusion");
    group.refresh_public_revisions(&before);
    assert!(group.public_seq() > before.public_seq());
    let before = group.clone();
    group
        .messages
        .last_mut()
        .unwrap()
        .tool_calls
        .push(serde_json::json!({"status":"completed"}));
    group.refresh_public_revisions(&before);
    assert_eq!(group.public_seq(), before.public_seq());
    let before = group.clone();
    group.messages.last_mut().unwrap().text = "Same message edited".into();
    group.refresh_public_revisions(&before);
    assert!(group.public_seq() > before.public_seq());
}

#[test]
fn empty_turn_with_unconsumed_correction_continues_execution_before_result_repair() {
    let (mut group, original) = fixture(false);
    group.works[0].status = "running".into();
    group.works[0].context_seq = group.messages[0].seq;
    group
        .post(None, "user", "Also update the second file".into(), None)
        .request_id = Some("request".into());
    group.paused = true;
    group.finish_execution(&original, None, false);
    let next = group.works.last().unwrap();
    assert!(
        !next.read_only(),
        "an unhandled execution request must not silently become a read-only result check"
    );
    assert_eq!(next.continuation_of.as_deref(), Some(original.as_str()));
    assert_eq!(next.status, "queued");
}

#[test]
fn cancelled_read_only_summary_uses_a_fresh_actor_for_result_retry() {
    let (mut group, original) = fixture(false);
    group.works[0].summary = true;
    group.works[0].session_id = Some("discussion-before-stop".into());
    let execution = group.members[0].session_id.clone();
    group.summarize_request(&original).unwrap();
    assert!(group.works.last().unwrap().read_only());
    assert!(group.members[0].discussion_session_id.is_none());
    assert_eq!(group.members[0].session_id, execution);
}

#[test]
fn summary_callback_retains_read_only_policy_after_user_assigned_child() {
    let (mut group, original) = fixture(false);
    let mut expert = group.members[0].clone();
    expert.id = "member-b".into();
    group.members.push(expert);
    group.works[0].status = "running".into();
    group.works[0].summary = true;
    let child = group
        .enqueue(
            "request",
            "member-b",
            "User-assigned check".into(),
            false,
            Some(original.clone()),
            1,
        )
        .unwrap();
    group.finish(&original, None, false);
    group.works.last_mut().unwrap().output = "Child result".into();
    group.finish_execution(&child, None, false);
    let callback = group.works.last().unwrap();
    assert_eq!(callback.continuation_of.as_deref(), Some(original.as_str()));
    assert!(callback.summary);
    assert!(callback.read_only());
}

#[test]
fn removed_request_owner_can_be_summarized_by_the_current_coordinator() {
    let (mut group, original) = fixture(false);
    group.messages[0].request_owner_id = Some("member-a".into());
    group.members[0].removed = true;
    let mut coordinator = group.members[0].clone();
    coordinator.id = "member-b".into();
    coordinator.removed = false;
    group.members.push(coordinator);
    group.coordinator_id = "member-b".into();
    group.summarize_request(&original).unwrap();
    assert_eq!(group.works.last().unwrap().member_id, "member-b");
    assert_eq!(group.request_owner("request"), "member-a");
}

#[test]
fn owner_with_already_prepared_child_results_still_repairs_an_empty_reply() {
    let (mut group, original) = fixture(false);
    let mut expert = group.members[0].clone();
    expert.id = "member-b".into();
    group.members.push(expert);
    let child = group
        .enqueue(
            "request",
            "member-b",
            "Prepared review".into(),
            false,
            Some(original.clone()),
            1,
        )
        .unwrap();
    group.works[1].status = "completed".into();
    group.works[1].output = "Reviewed evidence".into();
    group.works[0].status = "running".into();
    group.works[0].context_children.push(child);
    group.finish_execution(&original, None, false);
    assert!(group.works.last().unwrap().result_repair);
    assert_eq!(group.works[0].status, "waiting_children");
}
