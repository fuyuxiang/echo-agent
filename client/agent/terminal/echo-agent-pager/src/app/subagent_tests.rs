use super::test_support::make_info;
use super::*;
use crate::acp::meta::NotificationMeta;
use crate::acp::model_state::ModelState;
use crate::acp::tracker::AcpUpdateTracker;
use crate::app::agent::{AgentId, AgentSession, AgentState};
use crate::app::agent_view::AgentView;
use crate::scrollback::block::RenderBlock;
use crate::scrollback::state::ScrollbackState;
use agent_client_protocol as acp;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
fn make_min_child_view() -> AgentView {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let session = AgentSession {
        id: AgentId(0),
        acp_tx: tx,
        session_id: Some(acp::SessionId::new(Arc::from("child"))),
        models: ModelState::default(),
        state: AgentState::Idle,
        tracker: AcpUpdateTracker::new(),
        cwd: PathBuf::from("/tmp"),
        is_worktree: false,
        forked_from: None,
        pending_prompts: VecDeque::new(),
        next_queue_id: 0,
        yolo_mode: false,
        auto_mode: false,
        prompt_history: Vec::new(),
        prompt_history_loading: false,
        loading_replay: false,
        restore_degree: None,
        rate_limited: false,
        model_incompatible: false,
        credit_limit_blocked: false,
        free_usage_blocked: false,
        available_commands: Vec::new(),
        available_commands_generation: 0,
        available_tools: None,
        model_switch_pending: false,
        user_model_preference: None,
        deferred_model_switch: None,
        bg_tasks: BTreeMap::new(),
        bg_tool_call_to_task: HashMap::new(),
        scheduled_tasks: HashMap::new(),
        in_flight_prompt: None,
        compact_held_prompt: None,
        current_prompt_id: None,
        created_via_new: false,
    };
    AgentView::new(session, ScrollbackState::new())
}
fn seed_tool_call(view: &mut AgentView) {
    view.session.tracker.handle_update(
        acp::SessionUpdate::ToolCall(
            acp::ToolCall::new(acp::ToolCallId::new(Arc::from("tc1")), "Read foo")
                .kind(acp::ToolKind::Other)
                .status(acp::ToolCallStatus::Pending)
                .content(vec![])
                .locations(vec![]),
        ),
        &NotificationMeta::default(),
        &mut view.scrollback,
    );
}
#[test]
fn scrollback_is_prompt_only_classifies_content() {
    let empty = make_min_child_view();
    assert!(scrollback_is_prompt_only(&empty.scrollback), "empty");
    let mut prompt = make_min_child_view();
    prompt
        .scrollback
        .push_block(RenderBlock::user_prompt("scan src/"));
    assert!(
        scrollback_is_prompt_only(&prompt.scrollback),
        "injected prompt"
    );
    let mut tool = make_min_child_view();
    seed_tool_call(&mut tool);
    assert!(!scrollback_is_prompt_only(&tool.scrollback), "tool call");
    let mut both = make_min_child_view();
    both.scrollback
        .push_block(RenderBlock::user_prompt("scan src/"));
    seed_tool_call(&mut both);
    assert!(
        !scrollback_is_prompt_only(&both.scrollback),
        "prompt + tool call"
    );
}
#[test]
fn a_disk_backed_child_is_not_replayed_again() {
    let mut parent = make_min_child_view();
    let child_sid = "child-skip";
    let mut child = make_min_child_view();
    child
        .scrollback
        .push_block(RenderBlock::user_prompt("task only"));
    parent
        .subagent_views
        .insert(child_sid.to_string(), Box::new(child));
    let mut info = make_info();
    info.child_session_id = child_sid.into();
    info.transcript = ChildTranscript::DiskBacked;
    parent.subagent_sessions.insert(child_sid.to_string(), info);
    assert_eq!(
        ensure_subagent_child_replayed(&mut parent, child_sid),
        ChildReplayOutcome::NothingToRead
    );
    let child = parent.subagent_views.get(child_sid).unwrap();
    assert_eq!(child.scrollback.len(), 1);
    assert!(matches!(
        child.scrollback.entry(0).unwrap().block,
        RenderBlock::UserPrompt(_)
    ));
}
#[test]
fn replay_reports_live_blocks_and_unknown_children_distinctly() {
    let mut parent = make_min_child_view();
    let child_sid = "child-live-blocks";
    let mut child = make_min_child_view();
    seed_tool_call(&mut child);
    parent
        .subagent_views
        .insert(child_sid.to_string(), Box::new(child));
    let mut info = make_info();
    info.child_session_id = child_sid.into();
    parent.subagent_sessions.insert(child_sid.to_string(), info);
    assert_eq!(
        ensure_subagent_child_replayed(&mut parent, child_sid),
        ChildReplayOutcome::ViewHoldsLiveBlocks
    );
    assert_eq!(
        ensure_subagent_child_replayed(&mut parent, "no-such-child"),
        ChildReplayOutcome::UnknownChild
    );
}
#[test]
fn empty_read_of_a_running_child_is_cached_until_it_finishes() {
    let mut parent = make_min_child_view();
    let child_sid = "child-empty-cache";
    parent
        .subagent_views
        .insert(child_sid.to_string(), Box::new(make_min_child_view()));
    let mut info = make_info();
    info.child_session_id = child_sid.into();
    parent.subagent_sessions.insert(child_sid.to_string(), info);
    let home = tempfile::tempdir().unwrap();
    set_replay_echo_agent_home_for_tests(Some(home.path().to_path_buf()));
    let before = test_support::transcript_reads();
    assert_eq!(
        ensure_subagent_child_replayed(&mut parent, child_sid),
        ChildReplayOutcome::FoundNothingOnDisk
    );
    assert_eq!(
        parent.subagent_sessions[child_sid].transcript,
        ChildTranscript::DiskEmptyWhileRunning
    );
    assert_eq!(
        ensure_subagent_child_replayed(&mut parent, child_sid),
        ChildReplayOutcome::NothingToRead
    );
    assert_eq!(
        test_support::transcript_reads(),
        before + 1,
        "the cached empty result must not re-read the transcript"
    );
    parent
        .subagent_sessions
        .get_mut(child_sid)
        .unwrap()
        .transcript
        .retry_disk_after_finish();
    assert_eq!(
        parent.subagent_sessions[child_sid].transcript,
        ChildTranscript::NeedsReplay,
        "the finish must allow one more read for a late persistence flush"
    );
    set_replay_echo_agent_home_for_tests(None);
}
#[test]
fn an_empty_read_of_a_running_resumed_child_stays_needs_replay_and_retries() {
    let home = tempfile::tempdir().unwrap();
    let child_sid = "child-resumed-empty";
    set_replay_echo_agent_home_for_tests(Some(home.path().to_path_buf()));
    let mut parent = make_min_child_view();
    let mut child = make_min_child_view();
    child
        .scrollback
        .push_block(RenderBlock::user_prompt("task only"));
    parent
        .subagent_views
        .insert(child_sid.to_string(), Box::new(child));
    let mut info = make_info();
    info.child_session_id = child_sid.into();
    info.context_source = Some("resumed".into());
    parent.subagent_sessions.insert(child_sid.to_string(), info);
    assert_eq!(
        ensure_subagent_child_replayed(&mut parent, child_sid),
        ChildReplayOutcome::FoundNothingOnDisk
    );
    assert_eq!(
        parent.subagent_sessions[child_sid].transcript,
        ChildTranscript::NeedsReplay,
        "a resumed child's empty-while-running read must not settle: its inherited history is expected on disk"
    );
    let session_dir = home
        .path()
        .join("sessions")
        .join(urlencoding::encode("/tmp").as_ref())
        .join(child_sid);
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(session_dir.join("summary.json"), "{}").unwrap();
    let tool_line = format!(
        r#"{{"method":"session/update","params":{{"sessionId":"{child_sid}","update":{{"sessionUpdate":"tool_call","toolCallId":"tc1","title":"Read foo","kind":"read","locations":[{{"path":"/tmp/foo"}}]}}}}}}"#
    );
    std::fs::write(session_dir.join("updates.jsonl"), tool_line + "\n").unwrap();
    assert_eq!(
        ensure_subagent_child_replayed(&mut parent, child_sid),
        ChildReplayOutcome::Replayed,
        "the retry after the transcript flushes must replay the inherited prefix"
    );
    assert_eq!(
        parent.subagent_sessions[child_sid].transcript,
        ChildTranscript::DiskBacked
    );
    let child = parent.subagent_views.get(child_sid).unwrap();
    let tools = (0..child.scrollback.len())
        .filter(|i| {
            child
                .scrollback
                .entry(*i)
                .is_some_and(|e| matches!(e.block, RenderBlock::ToolCall(_)))
        })
        .count();
    assert_eq!(
        tools, 1,
        "the inherited tool call must appear after the retry"
    );
    set_replay_echo_agent_home_for_tests(None);
}
#[test]
fn a_child_replay_releases_retained_memory_only_once() {
    use crate::memory_release::test_support;
    test_support::install_counting_hook();
    let child_sid = "child-purge-real";
    let home = tempfile::tempdir().unwrap();
    let session_dir = home
        .path()
        .join("sessions")
        .join(urlencoding::encode("/tmp").as_ref())
        .join(child_sid);
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(session_dir.join("summary.json"), "{}").unwrap();
    let tool_line = format!(
        r#"{{"method":"session/update","params":{{"sessionId":"{child_sid}","update":{{"sessionUpdate":"tool_call","toolCallId":"tc1","title":"Read foo","kind":"read","locations":[{{"path":"/tmp/foo"}}]}}}}}}"#
    );
    std::fs::write(session_dir.join("updates.jsonl"), tool_line + "\n").unwrap();
    set_replay_echo_agent_home_for_tests(Some(home.path().to_path_buf()));
    let mut parent = make_min_child_view();
    parent
        .subagent_views
        .insert(child_sid.to_string(), Box::new(make_min_child_view()));
    let mut info = make_info();
    info.child_session_id = child_sid.into();
    parent.subagent_sessions.insert(child_sid.to_string(), info);
    let before = test_support::calls();
    assert_eq!(
        ensure_subagent_child_replayed(&mut parent, child_sid),
        ChildReplayOutcome::Replayed,
        "an emitting replay returns Replayed"
    );
    assert_eq!(
        test_support::calls(),
        before + 1,
        "a real replay must purge after the parsed transient drops"
    );
    assert!(
        !parent.subagent_sessions[child_sid]
            .transcript
            .needs_replay(),
        "fixture sanity: the emitting replay must record the disk copy"
    );
    let before = test_support::calls();
    let _ = ensure_subagent_child_replayed(&mut parent, child_sid);
    assert_eq!(
        test_support::calls(),
        before,
        "the skip path allocates nothing and must not purge"
    );
    let ghost_sid = "child-purge-ghost";
    parent
        .subagent_views
        .insert(ghost_sid.to_string(), Box::new(make_min_child_view()));
    let mut ghost = make_info();
    ghost.child_session_id = ghost_sid.into();
    parent
        .subagent_sessions
        .insert(ghost_sid.to_string(), ghost);
    let before = test_support::calls();
    let _ = ensure_subagent_child_replayed(&mut parent, ghost_sid);
    assert_eq!(
        test_support::calls(),
        before,
        "a no-op replay (missing transcript) must not purge"
    );
    let empty_sid = "child-purge-empty";
    let empty_dir = home
        .path()
        .join("sessions")
        .join(urlencoding::encode("/tmp").as_ref())
        .join(empty_sid);
    std::fs::create_dir_all(&empty_dir).unwrap();
    std::fs::write(empty_dir.join("summary.json"), "{}").unwrap();
    std::fs::write(empty_dir.join("updates.jsonl"), "").unwrap();
    parent
        .subagent_views
        .insert(empty_sid.to_string(), Box::new(make_min_child_view()));
    let mut empty = make_info();
    empty.child_session_id = empty_sid.into();
    parent
        .subagent_sessions
        .insert(empty_sid.to_string(), empty);
    let before = test_support::calls();
    let _ = ensure_subagent_child_replayed(&mut parent, empty_sid);
    assert_eq!(
        test_support::calls(),
        before,
        "an empty replay (zero updates parsed) must not purge"
    );
    set_replay_echo_agent_home_for_tests(None);
}
#[test]
fn rebuilt_child_transcript_keeps_persisted_timestamps_not_the_rebuild_time() {
    use chrono::TimeZone;
    let child_sid = "child-timestamps";
    let prompt_ms: i64 = 1_700_000_000_000;
    let msg_ms: i64 = 1_700_000_060_000;
    let home = tempfile::tempdir().unwrap();
    let session_dir = home
        .path()
        .join("sessions")
        .join(urlencoding::encode("/tmp").as_ref())
        .join(child_sid);
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(session_dir.join("summary.json"), "{}").unwrap();
    let echo = format!(
        r#"{{"method":"session/update","params":{{"sessionId":"{child_sid}","update":{{"sessionUpdate":"user_message_chunk","content":{{"type":"text","text":"scan src/"}}}},"_meta":{{"agentTimestampMs":{prompt_ms},"turnStartMs":{prompt_ms}}}}}}}"#
    );
    let msg = format!(
        r#"{{"method":"session/update","params":{{"sessionId":"{child_sid}","update":{{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":"done"}}}},"_meta":{{"agentTimestampMs":{msg_ms}}}}}}}"#
    );
    std::fs::write(
        session_dir.join("updates.jsonl"),
        format!("{echo}\n{msg}\n"),
    )
    .unwrap();
    set_replay_echo_agent_home_for_tests(Some(home.path().to_path_buf()));
    let mut parent = make_min_child_view();
    parent
        .subagent_views
        .insert(child_sid.to_string(), Box::new(make_min_child_view()));
    let mut info = make_info();
    info.child_session_id = child_sid.into();
    info.prompt = Some("scan src/".into());
    info.finished = true;
    info.duration_ms = Some(1_000);
    parent.subagent_sessions.insert(child_sid.to_string(), info);
    let _ = ensure_subagent_child_replayed(&mut parent, child_sid);
    let expected = |ms: i64| {
        chrono::Utc
            .timestamp_millis_opt(ms)
            .single()
            .unwrap()
            .with_timezone(&chrono::Local)
    };
    let child = parent.subagent_views.get(child_sid).unwrap();
    let mut prompt_seen = false;
    let mut msg_seen = false;
    for i in 0..child.scrollback.len() {
        let entry = child.scrollback.entry(i).unwrap();
        match &entry.block {
            RenderBlock::UserPrompt(_) => {
                assert_eq!(
                    entry.created_at,
                    Some(expected(prompt_ms)),
                    "injected task prompt must carry the persisted turn start, not the rebuild time"
                );
                prompt_seen = true;
            }
            RenderBlock::AgentMessage(_) => {
                assert_eq!(
                    entry.created_at,
                    Some(expected(msg_ms)),
                    "replayed agent message must carry the persisted timestamp, not the rebuild time"
                );
                msg_seen = true;
            }
            _ => {}
        }
    }
    assert!(prompt_seen, "fixture must produce a user prompt entry");
    assert!(msg_seen, "fixture must produce an agent message entry");
    set_replay_echo_agent_home_for_tests(None);
}
#[test]
fn a_replayed_transcript_collapses_a_tool_call_and_its_updates() {
    let home = tempfile::tempdir().unwrap();
    let child_sid = "child-batch";
    let session_dir = home
        .path()
        .join("sessions")
        .join(urlencoding::encode("/tmp").as_ref())
        .join(child_sid);
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(session_dir.join("summary.json"), "{}").unwrap();
    let user = format!(
        r#"{{"method":"session/update","params":{{"sessionId":"{child_sid}","update":{{"sessionUpdate":"user_message_chunk","content":{{"type":"text","text":"go"}}}}}}}}"#
    );
    let tool = format!(
        r#"{{"method":"session/update","params":{{"sessionId":"{child_sid}","update":{{"sessionUpdate":"tool_call","toolCallId":"t1","title":"bash","kind":"execute","status":"pending"}}}}}}"#
    );
    let ip = format!(
        r#"{{"method":"session/update","params":{{"sessionId":"{child_sid}","update":{{"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"in_progress","content":[{{"type":"text","text":"out"}}]}}}}}}"#
    );
    let done = format!(
        r#"{{"method":"session/update","params":{{"sessionId":"{child_sid}","update":{{"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"completed","content":[{{"type":"text","text":"out"}}]}}}}}}"#
    );
    let agent_msg = format!(
        r#"{{"method":"session/update","params":{{"sessionId":"{child_sid}","update":{{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":"ok"}}}}}}}}"#
    );
    std::fs::write(
        session_dir.join("updates.jsonl"),
        format!("{user}\n{tool}\n{ip}\n{done}\n{agent_msg}\n"),
    )
    .unwrap();
    set_replay_echo_agent_home_for_tests(Some(home.path().to_path_buf()));
    let mut view = make_min_child_view();
    assert!(matches!(
        replay_inherited_updates(
            &mut view,
            child_sid,
            std::path::Path::new("/tmp"),
            None,
            ReplayLookupFallback::Relocation,
        ),
        Ok(ReplayEmission::Emitted)
    ));
    assert!(
        !view.scrollback.in_batch(),
        "end_batch must run after streamed apply"
    );
    assert_eq!(
        view.scrollback.turn_count(),
        1,
        "end_batch must rebuild turns once after the stream"
    );
    let tools = (0..view.scrollback.len())
        .filter(|i| {
            view.scrollback
                .entry(*i)
                .is_some_and(|e| matches!(e.block, RenderBlock::ToolCall(_)))
        })
        .count();
    assert_eq!(tools, 1, "ToolCall+updates must collapse to one block");
    set_replay_echo_agent_home_for_tests(None);
}
#[test]
fn a_read_error_reports_read_failed_and_closes_the_scrollback_batch() {
    let home = tempfile::tempdir().unwrap();
    let child_sid = "child-read-err";
    let session_dir = home
        .path()
        .join("sessions")
        .join(urlencoding::encode("/tmp").as_ref())
        .join(child_sid);
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(session_dir.join("summary.json"), "{}").unwrap();
    std::fs::create_dir(session_dir.join("updates.jsonl")).unwrap();
    set_replay_echo_agent_home_for_tests(Some(home.path().to_path_buf()));
    let mut view = make_min_child_view();
    assert!(
        replay_inherited_updates(
            &mut view,
            child_sid,
            std::path::Path::new("/tmp"),
            None,
            ReplayLookupFallback::Relocation,
        )
        .is_err()
    );
    assert!(
        !view.scrollback.in_batch(),
        "end_batch must run after a read error"
    );
    let mut parent = make_min_child_view();
    parent
        .subagent_views
        .insert(child_sid.to_string(), Box::new(view));
    let mut info = make_info();
    info.child_session_id = child_sid.into();
    parent.subagent_sessions.insert(child_sid.to_string(), info);
    assert_eq!(
        ensure_subagent_child_replayed(&mut parent, child_sid),
        ChildReplayOutcome::ReadFailed,
        "a broken transcript surfaces as ReadFailed so the next open retries"
    );
    set_replay_echo_agent_home_for_tests(None);
}
#[test]
fn a_replay_locates_the_transcript_via_the_child_cwd_hint() {
    let home = tempfile::tempdir().unwrap();
    let child_sid = "child-wt-hint";
    let child_cwd = "/work/wt";
    let session_dir = home
        .path()
        .join("sessions")
        .join(echo_agent_config::encode_cwd_dirname(child_cwd))
        .join(child_sid);
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(session_dir.join("summary.json"), "{}").unwrap();
    let user = format!(
        r#"{{"method":"session/update","params":{{"sessionId":"{child_sid}","update":{{"sessionUpdate":"user_message_chunk","content":{{"type":"text","text":"from-wt"}}}}}}}}"#
    );
    std::fs::write(session_dir.join("updates.jsonl"), format!("{user}\n")).unwrap();
    set_replay_echo_agent_home_for_tests(Some(home.path().to_path_buf()));
    let mut view = make_min_child_view();
    assert!(matches!(
        replay_inherited_updates(
            &mut view,
            child_sid,
            std::path::Path::new("/tmp"),
            Some(std::path::Path::new(child_cwd)),
            ReplayLookupFallback::Relocation,
        ),
        Ok(ReplayEmission::Emitted)
    ));
    assert_ne!(
        view.scrollback.len(),
        0,
        "child_cwd hint must locate the worktree transcript"
    );
    set_replay_echo_agent_home_for_tests(None);
}
#[test]
fn child_view_for_live_update_hydrates_a_resumed_child_before_returning_it() {
    let home = tempfile::tempdir().unwrap();
    let child_sid = "child-live-update-hydrate";
    let session_dir = home
        .path()
        .join("sessions")
        .join(urlencoding::encode("/tmp").as_ref())
        .join(child_sid);
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(session_dir.join("summary.json"), "{}").unwrap();
    let tool_line = format!(
        r#"{{"method":"session/update","params":{{"sessionId":"{child_sid}","update":{{"sessionUpdate":"tool_call","toolCallId":"tc1","title":"Read foo","kind":"read","locations":[{{"path":"/tmp/foo"}}]}}}}}}"#
    );
    std::fs::write(session_dir.join("updates.jsonl"), tool_line + "\n").unwrap();
    set_replay_echo_agent_home_for_tests(Some(home.path().to_path_buf()));
    let mut parent = make_min_child_view();
    let mut child = make_min_child_view();
    child
        .scrollback
        .push_block(RenderBlock::user_prompt("task only"));
    parent
        .subagent_views
        .insert(child_sid.to_string(), Box::new(child));
    let mut info = make_info();
    info.child_session_id = child_sid.into();
    info.context_source = Some("resumed".into());
    parent.subagent_sessions.insert(child_sid.to_string(), info);
    {
        let view = parent.child_view_for_live_update_mut(child_sid).unwrap();
        let tools = (0..view.scrollback.len())
            .filter(|i| {
                view.scrollback
                    .entry(*i)
                    .is_some_and(|e| matches!(e.block, RenderBlock::ToolCall(_)))
            })
            .count();
        assert_eq!(
            tools, 1,
            "the accessor must replay a resumed child before handing back its view for a live block"
        );
    }
    assert_eq!(
        parent.subagent_sessions[child_sid].transcript,
        ChildTranscript::DiskBacked,
        "the hydrate records the proven disk copy"
    );
    set_replay_echo_agent_home_for_tests(None);
}
