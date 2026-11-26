//! Tests for session forking.

use super::*;

// ── Worktree session tests ───────────────────────────────────────

#[test]
fn open_new_worktree_dialog_sets_dialog_state() {
    let mut app = test_app();
    assert!(app.new_worktree_dialog.is_none());
    let effects = dispatch(Action::OpenNewWorktreeDialog, &mut app);
    assert!(effects.is_empty());
    assert!(app.new_worktree_dialog.is_some());
}

#[test]
fn worktree_forked_sets_session_id_eagerly_and_emits_load() {
    let mut app = test_app_git();
    dispatch(
        Action::NewWorktreeSession {
            load_session_id: Some("orig-sess".into()),
            label: None,
            git_ref: None,
        },
        &mut app,
    );
    let id = AgentId(0);

    // Before WorktreeForked: session_id is None, loading_replay is false.
    assert!(app.agents[&id].session.session_id.is_none());
    assert!(!app.agents[&id].session.loading_replay);

    let worktree_path = PathBuf::from("/tmp/echo-agent-worktrees/pager-fork");
    let session_cwd = PathBuf::from("/tmp/echo-agent-worktrees/pager-fork/sub");
    let effects = dispatch(
        Action::TaskComplete(TaskResult::WorktreeForked {
            agent_id: id,
            session_id: acp::SessionId::new("forked-sess-1"),
            worktree_path,
            session_cwd: session_cwd.clone(),
            code_restored: false,
            restore_summary: None,
            restore_degree: None,
            resume_session_id: Some("orig-sess".into()),
        }),
        &mut app,
    );

    // session_id set eagerly.
    assert_eq!(
        app.agents[&id].session.session_id,
        Some(acp::SessionId::new("forked-sess-1"))
    );
    // loading_replay enabled so UI suppresses redraws during replay.
    assert!(app.agents[&id].session.loading_replay);
    // CWD updated to worktree.
    assert_eq!(app.agents[&id].session.cwd, session_cwd);
    assert!(app.agents[&id].session.is_worktree);
    // Emits LoadSession effect.
    assert_eq!(effects.len(), 1);
    assert!(matches!(&effects[0], Effect::LoadSession { session_id, .. }
                if session_id == "forked-sess-1"));
    // Scrollback has the "Worktree ready" message.
    assert!(!app.agents[&id].scrollback.is_empty());
}

#[test]
fn startup_fork_parent_is_worktree_for_standalone_clone() {
    let mut app = test_app();
    let main = crate::test_util::TempGitRepo::init("main-only");
    let clone = main.standalone_clone("wt-branch");
    let effects = dispatch(
        Action::StartupForkSession {
            parent_session_id: "parent-sid".into(),
            parent_cwd: Some(clone.path.clone()),
            new_session_id: None,
        },
        &mut app,
    );
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::ForkSession {
                parent_is_worktree: true,
                ..
            }
        )),
        "startup fork from a standalone clone must pass parent_is_worktree, got {effects:?}"
    );
}

#[test]
fn worktree_forked_clears_sticky_branch_from_main_repo() {
    let mut app = test_app_git();
    dispatch(
        Action::NewWorktreeSession {
            load_session_id: Some("orig-sess".into()),
            label: None,
            git_ref: None,
        },
        &mut app,
    );
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.current_branch = Some("main-random".into());
        agent.main_repo = Some("~/old-main".into());
        agent.is_worktree = false;
    }

    let worktree_path = PathBuf::from("/tmp/echo-agent-worktrees/pager-fork-sticky");
    let session_cwd = worktree_path.join("sub");
    dispatch(
        Action::TaskComplete(TaskResult::WorktreeForked {
            agent_id: id,
            session_id: acp::SessionId::new("forked-sess-sticky"),
            worktree_path,
            session_cwd: session_cwd.clone(),
            code_restored: false,
            restore_summary: None,
            restore_degree: None,
            resume_session_id: Some("orig-sess".into()),
        }),
        &mut app,
    );

    let agent = &app.agents[&id];
    assert!(
        agent.current_branch.is_none(),
        "sticky main-repo branch must not survive the worktree cwd switch"
    );
    assert!(agent.main_repo.is_none());
    assert!(agent.is_worktree);
    assert!(agent.session.is_worktree);
    assert_eq!(agent.session.cwd, session_cwd);
}

#[test]
fn worktree_forked_with_restore_shows_summary_in_scrollback() {
    let mut app = test_app_git();
    dispatch(
        Action::NewWorktreeSession {
            load_session_id: Some("orig-sess".into()),
            label: None,
            git_ref: None,
        },
        &mut app,
    );
    let id = AgentId(0);

    let worktree_path = PathBuf::from("/tmp/echo-agent-worktrees/pager-fork");
    let session_cwd = PathBuf::from("/tmp/echo-agent-worktrees/pager-fork/sub");
    let effects = dispatch(
        Action::TaskComplete(TaskResult::WorktreeForked {
            agent_id: id,
            session_id: acp::SessionId::new("forked-sess-2"),
            worktree_path,
            session_cwd: session_cwd.clone(),
            code_restored: true,
            restore_summary: Some(
                "checked out abc12345, staged: true, unstaged: false, untracked: 3".into(),
            ),
            restore_degree: Some(echo_agent_workspace::session::git::RestoreDegree::Full),
            resume_session_id: Some("orig-sess".into()),
        }),
        &mut app,
    );

    // Should emit LoadSession.
    assert_eq!(effects.len(), 1);
    assert!(matches!(
        &effects[0],
        Effect::LoadSession { session_id, .. } if session_id == "forked-sess-2"
    ));
    // Scrollback should contain the restore summary.
    let has_restore_msg = app.agents[&id]
        .scrollback
        .entries_in_range(0..app.agents[&id].scrollback.len())
        .iter()
        .any(|e| matches!(&e.block, RenderBlock::System(s) if s.text.contains("Code restored")));
    assert!(has_restore_msg, "expected restore summary in scrollback");
    assert_eq!(
        app.agents[&id].session.restore_degree,
        Some(echo_agent_workspace::session::git::RestoreDegree::Full),
        "restore_degree must be stored on the session"
    );
}

/// When the server emits `code_restored: false` with a non-empty
/// summary (e.g. "restore aborted (checkout failed)…"), the pager
/// MUST surface a warning banner; a success-only gate would silently
/// drop it.
#[test]
fn worktree_forked_with_restore_failure_shows_warning_banner() {
    let mut app = test_app_git();
    dispatch(
        Action::NewWorktreeSession {
            load_session_id: Some("orig-fail".into()),
            label: None,
            git_ref: None,
        },
        &mut app,
    );
    let id = AgentId(0);

    let worktree_path = PathBuf::from("/tmp/echo-agent-worktrees/pager-fail");
    let session_cwd = PathBuf::from("/tmp/echo-agent-worktrees/pager-fail/sub");
    dispatch(
        Action::TaskComplete(TaskResult::WorktreeForked {
            agent_id: id,
            session_id: acp::SessionId::new("forked-sess-fail"),
            worktree_path,
            session_cwd,
            code_restored: false,
            restore_summary: Some(
                "restore aborted (checkout failed); stash skipped: MERGE_HEAD present".into(),
            ),
            restore_degree: None,
            resume_session_id: Some("orig-fail".into()),
        }),
        &mut app,
    );

    let entries = app.agents[&id]
        .scrollback
        .entries_in_range(0..app.agents[&id].scrollback.len());
    let warn = entries.iter().find_map(|e| match &e.block {
        RenderBlock::System(s) if s.text.contains("Code restore failed") => Some(&s.text),
        _ => None,
    });
    let text = warn.expect("warning banner missing").as_str();
    assert!(
        text.starts_with('\u{26A0}'),
        "expected ⚠ prefix, got: {text}"
    );
    assert!(text.contains("restore aborted"));
    assert!(text.contains("MERGE_HEAD present"));
    // The success banner must NOT also appear.
    let success_present = entries
        .iter()
        .any(|e| matches!(&e.block, RenderBlock::System(s) if s.text.contains("Code restored")));
    assert!(
        !success_present,
        "success banner must not appear on failure"
    );
}

/// A load INITIATION taking over a windowed agent supersedes the window
/// (finalized as failed) so the new load owns the batch/replay state and
/// its own `SessionLoaded` is NOT deferred.
#[test]
fn fork_initiation_supersedes_open_reload_window() {
    let mut app = test_app();
    dispatch(
        Action::LoadSession("sess-old".into(), None, false),
        &mut app,
    );
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        // Settle the fresh-view load, then open a reconnect window.
        agent.scrollback.end_batch();
        agent.session.loading_replay = false;
        agent
            .scrollback
            .push_block(RenderBlock::system("pre-outage content"));
        agent.begin_session_reload(1);
    }

    let effects = dispatch(
        Action::TaskComplete(TaskResult::ForkSessionReady {
            agent_id: id,
            new_session_id: acp::SessionId::new("sess-fork"),
            cwd: std::path::PathBuf::from("/tmp"),
            parent_session_id: acp::SessionId::new("sess-old"),
        }),
        &mut app,
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::LoadSession { .. })),
        "the fork's load proceeds"
    );
    {
        let agent = app.agents.get(&id).unwrap();
        assert!(
            agent.session_reload.is_none(),
            "the initiation finalized the window"
        );
        assert_eq!(
            agent.session.session_id.as_ref().map(|s| s.0.as_ref()),
            Some("sess-fork")
        );
    }

    // The fork's own SessionLoaded is not deferred — it settles the batch.
    dispatch(
        Action::TaskComplete(TaskResult::SessionLoaded {
            agent_id: id,
            session_id: acp::SessionId::new("sess-fork"),
            models: None,
            code_restored: false,
            restore_summary: None,
            restore_degree: None,
            running_prompt_id: None,
            scheduler_background_loops: None,
        }),
        &mut app,
    );
    let agent = app.agents.get(&id).unwrap();
    assert!(!agent.scrollback.in_batch(), "no batch leaked");
    assert!(!agent.session.loading_replay);
}

#[test]
fn slash_new_uses_worktree_cwd() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let worktree_path = PathBuf::from("/worktree/path");
    app.agents.get_mut(&id).unwrap().session.cwd = worktree_path.clone();
    app.agents.get_mut(&id).unwrap().session.is_worktree = true;

    let effects = dispatch(Action::SendPrompt("/new".into()), &mut app);
    let create = effects
        .iter()
        .find(|e| matches!(e, Effect::CreateSession { .. }));
    assert!(create.is_some(), "expected CreateSession effect");
    match create.unwrap() {
        Effect::CreateSession { cwd, .. } => assert_eq!(cwd, &worktree_path),
        _ => unreachable!(),
    }
    // The new agent (id=1) should inherit is_worktree from the source agent.
    let new_id = AgentId(1);
    assert!(app.agents[&new_id].session.is_worktree);
}

#[test]
fn auth_complete_dispatches_deferred_worktree() {
    let mut app = test_app();
    app.cwd_has_git_ancestor = true;
    app.auth_state = AuthState::Authenticating {
        request_seq: 1,
        handle: None,
        auth_url: None,
        mode: AuthMode::Pending,
    };
    app.deferred_startup.worktree = true;

    let effects = dispatch(
        Action::TaskComplete(TaskResult::AuthComplete {
            request_seq: 1,
            meta: None,
        }),
        &mut app,
    );

    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::CreateWorktreeSession { .. }))
    );
    assert!(!app.deferred_startup.worktree);
}

#[test]
fn auth_complete_resume_plus_worktree_creates_worktree_with_session() {
    let mut app = test_app();
    app.cwd_has_git_ancestor = true;
    app.auth_state = AuthState::Authenticating {
        request_seq: 1,
        handle: None,
        auth_url: None,
        mode: AuthMode::Pending,
    };
    app.deferred_startup.session =
        Some(crate::app::session_startup::DeferredSessionStartup::Load {
            session_id: "test-session".into(),
            session_cwd: None,
            chat_kind: false,
        });
    app.deferred_startup.worktree = true;

    let effects = dispatch(
        Action::TaskComplete(TaskResult::AuthComplete {
            request_seq: 1,
            meta: None,
        }),
        &mut app,
    );

    // --resume + --worktree: CreateWorktreeSession with the session ID.
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::CreateWorktreeSession {
            load_session_id: Some(_),
            ..
        }
    )));
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::LoadSession { .. }))
    );
}

#[test]
fn dispatch_fork_from_welcome_toasts_and_returns_no_effect() {
    let mut app = fork_test_app();
    app.active_view = ActiveView::Welcome;
    let effects = dispatch(Action::Fork(fork_args(None, None)), &mut app);
    assert!(effects.is_empty());
    // No active agent on Welcome, so no toast lands -- but the
    // important property is that no effect / agent was created.
    assert_eq!(app.agents.len(), 1);
}

#[test]
fn dispatch_fork_without_session_id_toasts_and_returns_no_effect() {
    let mut app = fork_test_app();
    // Strip the placeholder session_id so the "still being created"
    // guard fires.
    app.agents.get_mut(&AgentId(0)).unwrap().session.session_id = None;
    let effects = dispatch(Action::Fork(fork_args(None, None)), &mut app);
    assert!(effects.is_empty());
    assert_eq!(app.agents.len(), 1);
    // Toast lives on the agent (not in scrollback).
    let toast = app.agents[&AgentId(0)]
        .toast
        .as_ref()
        .expect("toast should be set");
    assert!(toast.0.contains("still being created"), "got: {}", toast.0);
}

#[test]
fn dispatch_fork_worktree_flag_skips_modal() {
    let mut app = fork_test_app();
    let effects = dispatch(Action::Fork(fork_args(Some(true), None)), &mut app);
    assert!(matches!(
        effects.as_slice(),
        [Effect::CreateWorktreeSession { .. }]
    ));
    assert!(
        app.agents.values().all(|a| a.question_view.is_none()),
        "--worktree must skip the modal"
    );
}

#[test]
fn dispatch_fork_no_worktree_flag_skips_modal() {
    let mut app = fork_test_app();
    let effects = dispatch(Action::Fork(fork_args(Some(false), None)), &mut app);
    assert!(matches!(effects.as_slice(), [Effect::ForkSession { .. }]));
    assert!(
        app.agents.values().all(|a| a.question_view.is_none()),
        "--no-worktree must skip the modal"
    );
}

#[test]
fn dispatch_fork_worktree_flag_non_git_toasts_and_returns_no_effect() {
    // --worktree in a non-git directory must be rejected synchronously
    // rather than waiting for an async WorktreeSessionFailed.
    let mut app = test_app_with_agent();
    // current_branch stays None (no git repo).
    let effects = dispatch(Action::Fork(fork_args(Some(true), None)), &mut app);
    assert!(effects.is_empty(), "must reject synchronously");
    assert_eq!(app.agents.len(), 1, "no placeholder agent created");
    let toast = app.agents[&AgentId(0)]
        .toast
        .as_ref()
        .expect("toast should be set");
    assert!(
        toast.0.contains("not in a git repository"),
        "got: {}",
        toast.0
    );
}

#[test]
fn dispatch_fork_no_flag_non_git_skips_modal_and_forks_without_worktree() {
    // When current_branch is None (not in a git repo), the worktree
    // question is meaningless — skip it and fork with worktree=false.
    let mut app = test_app_with_agent();
    // Do NOT set current_branch — the default None simulates a non-git cwd.
    assert!(app.agents[&AgentId(0)].current_branch.is_none());
    let effects = dispatch(
        Action::Fork(fork_args(None, Some("explore offline"))),
        &mut app,
    );
    // Must skip the modal and emit ForkSession immediately.
    assert!(
        matches!(effects.as_slice(), [Effect::ForkSession { .. }]),
        "non-git cwd must skip modal and emit ForkSession, got {effects:?}"
    );
    assert!(
        app.agents.values().all(|a| a.question_view.is_none()),
        "non-git cwd must not open the modal"
    );
    // Directive must still reach the new agent.
    let new_agent = app.agents.get(&AgentId(1)).expect("fork agent created");
    assert_eq!(
        new_agent.pending_first_prompt.as_deref(),
        Some("explore offline")
    );
}

#[test]
fn dispatch_fork_no_flag_always_opens_question_modal() {
    let mut app = fork_test_app();
    app.fork_worktree_mode = crate::app::app_view::WorktreeMode::Ask;
    let effects = dispatch(
        Action::Fork(fork_args(None, Some("debug timeout"))),
        &mut app,
    );
    assert!(effects.is_empty(), "no effects until modal answered");
    // No fork agent yet (placeholder construction is deferred).
    assert_eq!(app.agents.len(), 1);
    let qv = app.agents[&AgentId(0)]
        .question_view
        .as_ref()
        .expect("modal must be open");
    match qv.local_kind.as_ref().expect("local_kind must be set") {
        crate::views::question_view::LocalQuestionKind::Fork { directive } => {
            assert_eq!(directive.as_deref(), Some("debug timeout"));
        }
        other => panic!("expected Fork, got {other:?}"),
    }
    // The modal must offer four options: Yes / No / Always / Never.
    assert_eq!(
        qv.questions[0].options.len(),
        4,
        "modal must offer exactly 4 options (Yes/No/Always/Never)"
    );
    let labels: Vec<&str> = qv.questions[0]
        .options
        .iter()
        .map(|o| o.label.as_str())
        .collect();
    assert_eq!(
        labels,
        vec!["Yes", "No", "Always worktree", "Never worktree"]
    );
}

#[test]
fn open_fork_question_refuses_when_existing_question_is_open() {
    let mut app = fork_test_app();
    // Plant an existing question (e.g. an ACP-driven one).
    use crate::views::question_view::QuestionViewState;
    use echo_agent_tools::implementations::echo_agent_build::ask_user_question::{
        Question, QuestionOption,
    };
    let q = Question {
        question: "existing ACP question?".into(),
        options: vec![QuestionOption {
            label: "ok".into(),
            description: "ok".into(),
            preview: None,
            id: None,
        }],
        multi_select: Some(false),
        id: None,
    };
    let stashed = app.agents.get_mut(&AgentId(0)).unwrap().prompt.stash();
    app.agents.get_mut(&AgentId(0)).unwrap().question_view =
        Some(QuestionViewState::new("existing".into(), vec![q], stashed));

    let effects = dispatch(Action::Fork(fork_args(None, None)), &mut app);
    assert!(effects.is_empty());
    // Existing question survives; toast was shown via show_toast.
    let qv = app.agents[&AgentId(0)]
        .question_view
        .as_ref()
        .expect("existing question still present");
    assert_eq!(qv.tool_call_id, "existing");
    assert!(qv.local_kind.is_none(), "must not overwrite ACP question");
}

#[test]
fn dispatch_fork_resolved_no_worktree_emits_fork_effect() {
    let mut app = fork_test_app();
    let effects = dispatch(Action::Fork(fork_args(Some(false), None)), &mut app);
    match effects.as_slice() {
        [
            Effect::ForkSession {
                agent_id,
                parent_session_id,
                parent_cwd,
                parent_is_worktree,
                new_session_id: None,
            },
        ] => {
            assert_eq!(*agent_id, AgentId(1));
            assert_eq!(parent_session_id.0.as_ref(), "test-session");
            assert_eq!(parent_cwd, std::path::Path::new("/tmp"));
            assert!(!*parent_is_worktree);
        }
        other => panic!("expected ForkSession, got {other:?}"),
    }
}

#[test]
fn dispatch_fork_resolved_worktree_reuses_create_worktree_session() {
    let mut app = fork_test_app();
    let effects = dispatch(Action::Fork(fork_args(Some(true), None)), &mut app);
    match effects.as_slice() {
        [
            Effect::CreateWorktreeSession {
                agent_id,
                load_session_id,
                ..
            },
        ] => {
            assert_eq!(*agent_id, AgentId(1));
            assert_eq!(load_session_id.as_deref(), Some("test-session"));
        }
        other => panic!("expected CreateWorktreeSession, got {other:?}"),
    }
}

#[test]
fn dispatch_fork_sets_forked_from_on_new_agent() {
    let mut app = fork_test_app();
    dispatch(Action::Fork(fork_args(Some(false), None)), &mut app);
    let new_agent = app.agents.get(&AgentId(1)).expect("fork agent created");
    assert_eq!(new_agent.session.forked_from, Some(AgentId(0)));
}

/// GBT-4789 — dashboard attach follows the forked child.
#[test]
fn dispatch_fork_repoints_dashboard_attached_agent_to_child() {
    let mut app = fork_test_app();
    ensure_dashboard_state(&mut app);
    app.dashboard.as_mut().unwrap().attached_agent = Some(AgentId(0));

    dispatch(Action::Fork(fork_args(Some(false), None)), &mut app);

    assert!(
        matches!(app.active_view, ActiveView::Agent(id) if id == AgentId(1)),
        "fork must switch active view to the child"
    );
    assert_eq!(
        app.dashboard.as_ref().unwrap().attached_agent,
        Some(AgentId(1)),
        "attached_agent must re-point to the forked child so overlay \
         back-out (Left/Esc/Ctrl+\\) keeps working",
    );
}

/// Fork must not invent dashboard attach when none was set.
#[test]
fn dispatch_fork_without_dashboard_attach_leaves_attached_none() {
    let mut app = fork_test_app();
    ensure_dashboard_state(&mut app);
    assert!(app.dashboard.as_ref().unwrap().attached_agent.is_none());

    dispatch(Action::Fork(fork_args(Some(false), None)), &mut app);

    assert_eq!(
        app.dashboard.as_ref().unwrap().attached_agent,
        None,
        "fork must not enable overlay chrome when the parent was not attached",
    );
}

#[test]
fn dispatch_fork_keeps_stale_attach_on_other_agent() {
    let mut app = fork_test_app();
    insert_placeholder_agent(&mut app, AgentId(1));
    app.next_agent_id = 2;
    ensure_dashboard_state(&mut app);
    app.dashboard.as_mut().unwrap().attached_agent = Some(AgentId(1));

    dispatch(Action::Fork(fork_args(Some(false), None)), &mut app);

    assert!(
        matches!(app.active_view, ActiveView::Agent(id) if id == AgentId(2)),
        "fork must switch active view to the child"
    );
    assert_eq!(
        app.dashboard.as_ref().unwrap().attached_agent,
        Some(AgentId(1)),
        "attach on a different agent must not be re-pointed to the fork child",
    );
}

#[test]
fn dispatch_fork_pushes_parent_marker_with_directive() {
    let mut app = fork_test_app();
    dispatch(
        Action::Fork(fork_args(Some(false), Some("investigate races"))),
        &mut app,
    );
    // Last system block on parent is the fork marker.
    let parent_text = last_system_text(&app, AgentId(0));
    assert!(
        parent_text.contains("Forked: investigate races"),
        "parent marker missing or malformed: {parent_text}"
    );
    assert!(
        !parent_text.contains("/forward"),
        "parent marker must not reference /forward: {parent_text}"
    );
}

#[test]
fn dispatch_fork_pushes_parent_marker_without_directive() {
    let mut app = fork_test_app();
    dispatch(Action::Fork(fork_args(Some(false), None)), &mut app);
    // Last system block on parent is the fork marker.
    let parent_text = last_system_text(&app, AgentId(0));
    assert_eq!(parent_text, "Forked", "got: {parent_text}");
}

#[test]
fn dispatch_fork_defers_banner_worktree() {
    let mut app = fork_test_app();
    dispatch(Action::Fork(fork_args(Some(true), None)), &mut app);
    let info = app.agents[&AgentId(1)]
        .pending_fork_banner
        .as_ref()
        .expect("pending_fork_banner must be set");
    assert_eq!(info.parent_sid, "test-session");
    assert!(info.worktree, "worktree flag must be true");
}

#[test]
fn dispatch_fork_defers_banner_no_worktree() {
    let mut app = fork_test_app();
    dispatch(Action::Fork(fork_args(Some(false), None)), &mut app);
    let info = app.agents[&AgentId(1)]
        .pending_fork_banner
        .as_ref()
        .expect("pending_fork_banner must be set");
    assert_eq!(info.parent_sid, "test-session");
    assert!(!info.worktree, "worktree flag must be false");
}

#[test]
fn dispatch_fork_stores_full_parent_session_id_in_banner() {
    let mut app = fork_test_app();
    app.agents.get_mut(&AgentId(0)).unwrap().session.session_id = Some("abcdef0123456789".into());
    dispatch(Action::Fork(fork_args(Some(true), None)), &mut app);
    let info = app.agents[&AgentId(1)]
        .pending_fork_banner
        .as_ref()
        .expect("pending_fork_banner must be set");
    assert_eq!(
        info.parent_sid, "abcdef0123456789",
        "full parent session id must be stored (no truncation)"
    );
}

#[test]
fn build_child_fork_marker_worktree_format() {
    let banner = build_child_fork_marker("child-sid", "parent-sid", true, Some("/dashboard"));
    assert!(
        banner.contains("Session child-sid"),
        "must contain child session id: {banner}"
    );
    assert!(
        banner.contains("forked from parent-sid"),
        "must contain full parent session id: {banner}"
    );
    assert!(
        banner.contains("/dashboard"),
        "must advertise /dashboard: {banner}"
    );
    assert!(
        !banner.contains("share cwd"),
        "worktree case must NOT have shared-cwd line: {banner}"
    );
}

#[test]
fn build_child_fork_marker_no_worktree_format() {
    let banner = build_child_fork_marker("child-sid", "parent-sid", false, Some("/dashboard"));
    let lines: Vec<&str> = banner.split('\n').collect();
    assert_eq!(lines.len(), 2, "expected 2-line banner, got: {banner}");
    assert!(
        lines[0].contains("Session child-sid"),
        "first line must contain child session id: {banner}"
    );
    assert!(
        lines[0].contains("forked from parent-sid"),
        "first line must contain full parent session id: {banner}"
    );
    assert!(
        lines[0].contains("/dashboard"),
        "first line must advertise /dashboard: {banner}"
    );
    assert!(
        lines[1].contains("both agents share cwd"),
        "second line must surface shared-cwd caveat: {banner}"
    );
}

/// With the dashboard feature flag off, the banner must not advertise
/// `/dashboard` (the command is refused when disabled) but still carry
/// the session ids and the shared-cwd caveat.
#[test]
fn build_child_fork_marker_omits_dashboard_tip_when_disabled() {
    let banner = build_child_fork_marker("child-sid", "parent-sid", false, None);
    assert!(
        !banner.contains("/dashboard"),
        "must NOT advertise /dashboard when the flag is off: {banner}"
    );
    assert!(
        banner.contains("Session child-sid"),
        "must contain child session id: {banner}"
    );
    assert!(
        banner.contains("forked from parent-sid"),
        "must contain full parent session id: {banner}"
    );
    assert!(
        banner.contains("both agents share cwd"),
        "shared-cwd caveat must survive without the tip: {banner}"
    );
}

/// In minimal mode the caller passes `/resume` (the dashboard is refused
/// there but the session picker works) — the banner must advertise it and
/// never mention `/dashboard`.
#[test]
fn build_child_fork_marker_minimal_mode_advertises_resume() {
    let banner = build_child_fork_marker("child-sid", "parent-sid", false, Some("/resume"));
    assert!(
        banner.contains("use /resume to switch between sessions"),
        "must advertise /resume in minimal mode: {banner}"
    );
    assert!(
        !banner.contains("/dashboard"),
        "must NOT advertise /dashboard in minimal mode: {banner}"
    );
}

#[test]
fn dispatch_fork_pushes_progress_message_worktree() {
    let mut app = fork_test_app();
    dispatch(Action::Fork(fork_args(Some(true), None)), &mut app);
    let progress = last_system_text(&app, AgentId(1));
    assert_eq!(progress, "Creating worktree\u{2026}");
}

#[test]
fn dispatch_fork_stashes_directive_in_pending_first_prompt() {
    let mut app = fork_test_app();
    dispatch(
        Action::Fork(fork_args(Some(false), Some("first prompt text"))),
        &mut app,
    );
    let new_agent = app.agents.get(&AgentId(1)).unwrap();
    assert_eq!(
        new_agent.pending_first_prompt.as_deref(),
        Some("first prompt text")
    );
}

#[test]
fn dispatch_fork_inherits_appearance_sharing_and_plugin_visibility() {
    let mut app = fork_test_app();
    // Tweak app-level state so we can verify the sweep applied it.
    app.appearance.prompt.compact = true;
    app.sharing_enabled = false;
    app.usage_visible = false;
    app.appearance.disable_plugins = true;
    // Cached billing state must be inherited so the credits warning is
    // correct from the first frame (not just after a billing fetch).
    app.credit_balance = Some(crate::views::credit_bar::CreditBalance {
        prepaid_balance_cents: Some(1500),
        ..test_bal(50.0)
    });
    app.auto_topup = Some(crate::views::credit_bar::AutoTopupInfo {
        enabled: true,
        topup_amount_cents: Some(2000),
        max_amount_cents: None,
    });
    dispatch(Action::Fork(fork_args(Some(false), None)), &mut app);
    let new_agent = app.agents.get(&AgentId(1)).unwrap();
    assert!(!new_agent.sharing_enabled);
    assert!(
        new_agent
            .prompt
            .slash_controller
            .registry()
            .get("usage")
            .is_some()
    );
    assert!(!new_agent.billing_surface_visible);
    assert_eq!(
        new_agent
            .credit_balance
            .as_ref()
            .and_then(|b| b.prepaid_balance_cents),
        Some(1500)
    );
    assert!(new_agent.auto_topup.as_ref().is_some_and(|at| at.enabled));
}

#[test]
fn dispatch_fork_answered_re_dispatches_to_dispatch_fork_resolved() {
    let mut app = fork_test_app();
    let effects = dispatch(
        Action::ForkAnswered {
            worktree: false,
            directive: Some("answered directive".into()),
            persist_mode: None,
        },
        &mut app,
    );
    assert!(matches!(effects.as_slice(), [Effect::ForkSession { .. }]));
    let new_agent = app.agents.get(&AgentId(1)).expect("fork created");
    assert_eq!(
        new_agent.pending_first_prompt.as_deref(),
        Some("answered directive")
    );
}

/// `Action::ForkAnswered { worktree: true, .. }` produces the
/// `CreateWorktreeSession` effect (the "Yes" submit path).
#[test]
fn dispatch_fork_answered_worktree_true_emits_create_worktree_session() {
    let mut app = fork_test_app();
    let effects = dispatch(
        Action::ForkAnswered {
            worktree: true,
            directive: None,
            persist_mode: None,
        },
        &mut app,
    );
    assert!(matches!(
        effects.as_slice(),
        [Effect::CreateWorktreeSession { .. }]
    ));
}

/// `Action::ForkAnswered { worktree: false, .. }` produces the
/// `ForkSession` effect (the "No" submit path).
#[test]
fn dispatch_fork_answered_worktree_false_emits_fork_session() {
    let mut app = fork_test_app();
    let effects = dispatch(
        Action::ForkAnswered {
            worktree: false,
            directive: None,
            persist_mode: None,
        },
        &mut app,
    );
    assert!(matches!(effects.as_slice(), [Effect::ForkSession { .. }]));
}

#[test]
fn dispatch_fork_worktree_mode_always_skips_modal_and_creates_worktree() {
    let mut app = fork_test_app();
    app.fork_worktree_mode = crate::app::app_view::WorktreeMode::Always;
    let effects = dispatch(Action::Fork(fork_args(None, None)), &mut app);
    assert!(
        app.agents[&AgentId(0)].question_view.is_none(),
        "modal must not open when fork_worktree_mode is Always"
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::CreateWorktreeSession { .. })),
        "expected CreateWorktreeSession, got {effects:?}"
    );
}

#[test]
fn dispatch_fork_worktree_mode_never_skips_modal_and_forks_in_cwd() {
    let mut app = fork_test_app();
    app.fork_worktree_mode = crate::app::app_view::WorktreeMode::Never;
    let effects = dispatch(Action::Fork(fork_args(None, None)), &mut app);
    assert!(
        app.agents[&AgentId(0)].question_view.is_none(),
        "modal must not open when fork_worktree_mode is Never"
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::ForkSession { .. })),
        "expected ForkSession, got {effects:?}"
    );
}

#[test]
fn dispatch_fork_answered_with_persist_always_updates_mode_and_emits_effect() {
    let mut app = fork_test_app();
    app.fork_worktree_mode = crate::app::app_view::WorktreeMode::Ask;
    let effects = dispatch(
        Action::ForkAnswered {
            worktree: true,
            directive: None,
            persist_mode: Some(crate::app::app_view::WorktreeMode::Always),
        },
        &mut app,
    );
    assert_eq!(
        app.fork_worktree_mode,
        crate::app::app_view::WorktreeMode::Always
    );
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::PersistWorktreeMode {
                config_key: "fork_worktree_mode",
                ..
            }
        )),
        "expected PersistWorktreeMode with config_key fork_worktree_mode"
    );
}

#[test]
fn dispatch_new_session_answered_no_worktree_does_not_cancel_old_turn() {
    let mut app = new_session_test_app();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;
    let effects = dispatch(
        Action::NewSessionAnswered {
            worktree: false,
            persist_mode: None,
        },
        &mut app,
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::CreateSession { .. })),
        "expected CreateSession, got {effects:?}"
    );
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::CancelTurn { .. }))
    );
    assert!(
        app.agents[&id].session.state.is_turn_running(),
        "old agent's turn must remain running"
    );
}

#[test]
fn dispatch_new_session_answered_worktree_does_not_cancel_old_turn() {
    let mut app = new_session_test_app();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;
    let effects = dispatch(
        Action::NewSessionAnswered {
            worktree: true,
            persist_mode: None,
        },
        &mut app,
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::CreateWorktreeSession { .. })),
        "expected CreateWorktreeSession, got {effects:?}"
    );
    // Old agent's turn must still be running.
    assert!(
        app.agents[&id].session.state.is_turn_running(),
        "old agent's turn must remain running"
    );
}

#[test]
fn dispatch_new_session_worktree_mode_always_skips_modal_and_creates_worktree() {
    let mut app = new_session_test_app();
    app.new_session_worktree_mode = crate::app::app_view::WorktreeMode::Always;
    let effects = dispatch(Action::NewSession, &mut app);
    assert!(
        app.agents[&AgentId(0)].question_view.is_none(),
        "modal must not open when new_session_worktree_mode is Always"
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::CreateWorktreeSession { .. })),
        "expected CreateWorktreeSession, got {effects:?}"
    );
}

#[test]
fn dispatch_new_session_worktree_mode_never_skips_modal_and_creates_in_cwd() {
    let mut app = new_session_test_app();
    app.new_session_worktree_mode = crate::app::app_view::WorktreeMode::Never;
    let effects = dispatch(Action::NewSession, &mut app);
    assert!(
        app.agents[&AgentId(0)].question_view.is_none(),
        "modal must not open when new_session_worktree_mode is Never"
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::CreateSession { .. })),
        "expected CreateSession, got {effects:?}"
    );
}

#[test]
fn fork_session_ready_retargets_suppress_from_parent_to_child() {
    let mut app = fork_test_app();
    insert_placeholder_agent(&mut app, AgentId(1));
    app.agents.get_mut(&AgentId(1)).unwrap().session.session_id = None;
    app.suppress_code_restore_once = Some("parent-sid".into());
    let effects = dispatch(
        Action::TaskComplete(TaskResult::ForkSessionReady {
            agent_id: AgentId(1),
            new_session_id: "child-sid".into(),
            cwd: PathBuf::from("/tmp/forked"),
            parent_session_id: acp::SessionId::new("parent-sid"),
        }),
        &mut app,
    );
    assert_eq!(app.suppress_code_restore_once.as_deref(), Some("child-sid"));
    assert!(matches!(
        effects.as_slice(),
        [Effect::LoadSession { session_id, .. }] if session_id == "child-sid"
    ));
    let flags_restore = crate::app::event_loop::take_load_restore_code(&mut app, &effects);
    assert_eq!(flags_restore, Some(false));
    assert!(app.suppress_code_restore_once.is_none());
}

#[test]
fn fork_session_ready_does_not_retarget_unrelated_suppress() {
    let mut app = fork_test_app();
    insert_placeholder_agent(&mut app, AgentId(1));
    app.agents.get_mut(&AgentId(1)).unwrap().session.session_id = None;
    app.suppress_code_restore_once = Some("other-sid".into());
    dispatch(
        Action::TaskComplete(TaskResult::ForkSessionReady {
            agent_id: AgentId(1),
            new_session_id: "child-sid".into(),
            cwd: PathBuf::from("/tmp/forked"),
            parent_session_id: acp::SessionId::new("parent-sid"),
        }),
        &mut app,
    );
    assert_eq!(app.suppress_code_restore_once.as_deref(), Some("other-sid"));
}

#[test]
fn worktree_forked_retargets_suppress_to_child() {
    let mut app = test_app_git();
    dispatch(
        Action::NewWorktreeSession {
            load_session_id: Some("orig-sess".into()),
            label: None,
            git_ref: None,
        },
        &mut app,
    );
    app.suppress_code_restore_once = Some("orig-sess".into());
    let effects = dispatch(
        Action::TaskComplete(TaskResult::WorktreeForked {
            agent_id: AgentId(0),
            session_id: acp::SessionId::new("forked-sess-1"),
            worktree_path: PathBuf::from("/tmp/echo-agent-worktrees/pager-fork"),
            session_cwd: PathBuf::from("/tmp/echo-agent-worktrees/pager-fork/sub"),
            code_restored: false,
            restore_summary: None,
            restore_degree: None,
            resume_session_id: Some("orig-sess".into()),
        }),
        &mut app,
    );
    assert_eq!(
        app.suppress_code_restore_once.as_deref(),
        Some("forked-sess-1")
    );
    let flags_restore = crate::app::event_loop::take_load_restore_code(&mut app, &effects);
    assert_eq!(flags_restore, Some(false));
    assert!(app.suppress_code_restore_once.is_none());
}

#[test]
fn worktree_forked_does_not_retarget_unrelated_suppress() {
    let mut app = test_app_git();
    dispatch(
        Action::NewWorktreeSession {
            load_session_id: Some("orig-sess".into()),
            label: None,
            git_ref: None,
        },
        &mut app,
    );
    app.suppress_code_restore_once = Some("other-sid".into());
    dispatch(
        Action::TaskComplete(TaskResult::WorktreeForked {
            agent_id: AgentId(0),
            session_id: acp::SessionId::new("forked-sess-1"),
            worktree_path: PathBuf::from("/tmp/echo-agent-worktrees/pager-fork"),
            session_cwd: PathBuf::from("/tmp/echo-agent-worktrees/pager-fork/sub"),
            code_restored: false,
            restore_summary: None,
            restore_degree: None,
            resume_session_id: Some("orig-sess".into()),
        }),
        &mut app,
    );
    assert_eq!(app.suppress_code_restore_once.as_deref(), Some("other-sid"));
}

#[test]
fn fork_session_ready_emits_load_session_with_new_id() {
    let mut app = fork_test_app();
    // Plant a placeholder fork agent (mirroring what dispatch_fork
    // would do).
    insert_placeholder_agent(&mut app, AgentId(1));
    app.agents.get_mut(&AgentId(1)).unwrap().session.session_id = None;
    let effects = dispatch(
        Action::TaskComplete(TaskResult::ForkSessionReady {
            agent_id: AgentId(1),
            new_session_id: "new-sid-123".into(),
            cwd: PathBuf::from("/tmp/forked"),
            parent_session_id: acp::SessionId::new("test-session"),
        }),
        &mut app,
    );
    match effects.as_slice() {
        [
            Effect::LoadSession {
                agent_id,
                session_id,
                session_cwd,
                chat_kind,
            },
        ] => {
            assert_eq!(*agent_id, AgentId(1));
            assert_eq!(session_id, "new-sid-123");
            assert_eq!(
                session_cwd.as_deref(),
                Some(std::path::Path::new("/tmp/forked"))
            );
            assert!(
                !*chat_kind,
                "LoadSession chat_kind is conversation-entry only, not sticky --chat"
            );
        }
        other => panic!("expected LoadSession, got {other:?}"),
    }
    // session_id was eagerly adopted on the placeholder.
    assert_eq!(
        app.agents[&AgentId(1)]
            .session
            .session_id
            .as_ref()
            .unwrap()
            .0
            .as_ref(),
        "new-sid-123"
    );
    assert!(app.agents[&AgentId(1)].session.loading_replay);
}

/// Successful no-worktree fork under sticky `--chat` (child not a local
/// Build row under cwd) must stamp `conversation_entry` so `rename_kind()`
/// matches the effects `kind=chat` load stamp.
#[test]
fn fork_session_ready_sticky_chat_sets_rename_kind_chat() {
    let mut app = fork_test_app();
    insert_placeholder_agent(&mut app, AgentId(1));
    app.agents.get_mut(&AgentId(1)).unwrap().session.session_id = None;
    app.agents.get_mut(&AgentId(1)).unwrap().conversation_entry = false;
    app.chat_mode = true;
    let effects = dispatch(
        Action::TaskComplete(TaskResult::ForkSessionReady {
            agent_id: AgentId(1),
            new_session_id: "new-sid-chat".into(),
            cwd: PathBuf::from("/tmp/forked"),
            parent_session_id: acp::SessionId::new("test-session"),
        }),
        &mut app,
    );
    match effects.as_slice() {
        [Effect::LoadSession { chat_kind, .. }] => {
            assert!(
                !*chat_kind,
                "LoadSession chat_kind stays the raw conversation-entry bit"
            );
        }
        other => panic!("expected LoadSession, got {other:?}"),
    }
    let agent = app.agents.get(&AgentId(1)).expect("fork agent kept");
    assert!(agent.chat_kind, "UI bit comes from sticky --chat");
    assert!(
        agent.conversation_entry,
        "sticky --chat fork with no local disk opens as chat (rename kind)"
    );
    assert_eq!(
        agent.rename_kind(),
        echo_agent_runtime::session::unified_list::SessionKind::Chat
    );
}

/// No-worktree fork under sticky `--chat` must refuse a local Build row
/// (same gate as WorktreeForked / dispatch_load_session_ungated).
#[test]
fn fork_session_ready_refuses_local_build_under_chat_mode() {
    let mut app = fork_test_app();
    insert_placeholder_agent(&mut app, AgentId(1));
    app.agents.get_mut(&AgentId(1)).unwrap().session.session_id = None;
    app.agents.get_mut(&AgentId(1)).unwrap().chat_kind = false;
    app.chat_mode = true;
    let cwd = app.cwd.clone();
    let session_id = format!("fork-build-{}", std::process::id());
    let sess_dir = plant_local_build_session(&cwd, &session_id);

    let effects = dispatch(
        Action::TaskComplete(TaskResult::ForkSessionReady {
            agent_id: AgentId(1),
            new_session_id: session_id.clone().into(),
            cwd: cwd.clone(),
            parent_session_id: acp::SessionId::new("test-session"),
        }),
        &mut app,
    );
    let _ = std::fs::remove_dir_all(&sess_dir);
    assert!(
        effects.is_empty(),
        "ForkSessionReady under --chat must refuse local Build, got {effects:?}"
    );
    assert!(
        !app.agents.contains_key(&AgentId(1)),
        "refuse tears down the placeholder agent"
    );
}

#[test]
fn fork_session_failed_pushes_turn_failed_block() {
    let mut app = fork_test_app();
    insert_placeholder_agent(&mut app, AgentId(1));
    let effects = dispatch(
        Action::TaskComplete(TaskResult::ForkSessionFailed {
            agent_id: AgentId(1),
            error: "shell oops".into(),
        }),
        &mut app,
    );
    assert!(effects.is_empty());
    // The placeholder agent stays in app.agents (no rollback).
    assert!(app.agents.contains_key(&AgentId(1)));
}

#[test]
fn translate_local_submit_yes_returns_worktree_true_action() {
    use crate::views::question_view::{LocalQuestionKind, QuestionViewState};
    use echo_agent_tools::implementations::echo_agent_build::ask_user_question::{
        Question, QuestionOption,
    };
    let q = Question {
        question: "?".into(),
        options: (0..2)
            .map(|i| QuestionOption {
                label: format!("opt{i}"),
                description: String::new(),
                preview: None,
                id: None,
            })
            .collect(),
        multi_select: Some(false),
        id: None,
    };
    let mut state = QuestionViewState::new(
        "x".into(),
        vec![q],
        crate::views::prompt_widget::StashedPrompt::default(),
    )
    .with_local_kind(LocalQuestionKind::Fork {
        directive: Some("d".into()),
    });
    // Set selection to option 0 ("Yes" in production).
    state.selections[0] = crate::views::question_view::QuestionSelection::Single(Some(0));
    let kind = state.local_kind.take().unwrap();
    let outcome = crate::app::agent_view::translate_local_submit_for_test(&state, kind, false);
    match outcome {
        crate::app::app_view::InputOutcome::Action(Action::ForkAnswered {
            worktree,
            directive,
            persist_mode,
        }) => {
            assert!(worktree);
            assert_eq!(directive.as_deref(), Some("d"));
            assert!(persist_mode.is_none());
        }
        other => panic!("expected ForkAnswered, got {other:?}"),
    }
}

#[test]
fn translate_local_submit_no_returns_worktree_false_action() {
    use crate::views::question_view::{LocalQuestionKind, QuestionViewState};
    use echo_agent_tools::implementations::echo_agent_build::ask_user_question::{
        Question, QuestionOption,
    };
    let q = Question {
        question: "?".into(),
        options: (0..2)
            .map(|_| QuestionOption {
                label: "opt".into(),
                description: String::new(),
                preview: None,
                id: None,
            })
            .collect(),
        multi_select: Some(false),
        id: None,
    };
    let mut state = QuestionViewState::new(
        "x".into(),
        vec![q],
        crate::views::prompt_widget::StashedPrompt::default(),
    )
    .with_local_kind(LocalQuestionKind::Fork { directive: None });
    // Option 1 = "No" -> worktree=false.
    state.selections[0] = crate::views::question_view::QuestionSelection::Single(Some(1));
    let kind = state.local_kind.take().unwrap();
    let outcome = crate::app::agent_view::translate_local_submit_for_test(&state, kind, false);
    match outcome {
        crate::app::app_view::InputOutcome::Action(Action::ForkAnswered {
            worktree,
            directive,
            persist_mode,
        }) => {
            assert!(!worktree);
            assert!(directive.is_none());
            assert!(persist_mode.is_none());
        }
        other => panic!("expected ForkAnswered, got {other:?}"),
    }
}

#[test]
fn translate_local_submit_always_returns_persist_always_for_fork() {
    use crate::views::question_view::{LocalQuestionKind, QuestionViewState};
    use echo_agent_tools::implementations::echo_agent_build::ask_user_question::{
        Question, QuestionOption,
    };
    let q = Question {
        question: "?".into(),
        options: (0..4)
            .map(|i| QuestionOption {
                label: format!("opt{i}"),
                description: String::new(),
                preview: None,
                id: None,
            })
            .collect(),
        multi_select: Some(false),
        id: None,
    };
    let mut state = QuestionViewState::new(
        "x".into(),
        vec![q],
        crate::views::prompt_widget::StashedPrompt::default(),
    )
    .with_local_kind(LocalQuestionKind::Fork { directive: None });
    state.selections[0] = crate::views::question_view::QuestionSelection::Single(Some(2));
    let kind = state.local_kind.take().unwrap();
    let outcome = crate::app::agent_view::translate_local_submit_for_test(&state, kind, false);
    match outcome {
        crate::app::app_view::InputOutcome::Action(Action::ForkAnswered {
            worktree,
            persist_mode,
            ..
        }) => {
            assert!(worktree);
            assert_eq!(
                persist_mode,
                Some(crate::app::app_view::WorktreeMode::Always)
            );
        }
        other => panic!("expected ForkAnswered with persist Always, got {other:?}"),
    }
}

#[test]
fn translate_local_submit_never_returns_persist_never_for_fork() {
    use crate::views::question_view::{LocalQuestionKind, QuestionViewState};
    use echo_agent_tools::implementations::echo_agent_build::ask_user_question::{
        Question, QuestionOption,
    };
    let q = Question {
        question: "?".into(),
        options: (0..4)
            .map(|i| QuestionOption {
                label: format!("opt{i}"),
                description: String::new(),
                preview: None,
                id: None,
            })
            .collect(),
        multi_select: Some(false),
        id: None,
    };
    let mut state = QuestionViewState::new(
        "x".into(),
        vec![q],
        crate::views::prompt_widget::StashedPrompt::default(),
    )
    .with_local_kind(LocalQuestionKind::Fork { directive: None });
    state.selections[0] = crate::views::question_view::QuestionSelection::Single(Some(3));
    let kind = state.local_kind.take().unwrap();
    let outcome = crate::app::agent_view::translate_local_submit_for_test(&state, kind, false);
    match outcome {
        crate::app::app_view::InputOutcome::Action(Action::ForkAnswered {
            worktree,
            persist_mode,
            ..
        }) => {
            assert!(!worktree);
            assert_eq!(
                persist_mode,
                Some(crate::app::app_view::WorktreeMode::Never)
            );
        }
        other => panic!("expected ForkAnswered with persist Never, got {other:?}"),
    }
}

#[test]
fn handle_ask_user_question_pushes_system_block_when_displaced_local_fork_modal() {
    use crate::scrollback::block::RenderBlock;
    use crate::views::question_view::{LocalQuestionKind, QuestionViewState};
    use echo_agent_tools::implementations::echo_agent_build::ask_user_question::{
        Question, QuestionOption,
    };

    let mut app = fork_test_app();
    let id = AgentId(0);
    // Plant a local fork modal on the active agent.
    let stashed = app.agents.get_mut(&id).unwrap().prompt.stash();
    let q = Question {
        question: "fork worktree?".into(),
        options: vec![QuestionOption {
            label: "yes".into(),
            description: "y".into(),
            preview: None,
            id: None,
        }],
        multi_select: Some(false),
        id: None,
    };
    app.agents.get_mut(&id).unwrap().question_view = Some(
        QuestionViewState::new("local-fork".into(), vec![q], stashed).with_local_kind(
            LocalQuestionKind::Fork {
                directive: Some("dropped".into()),
            },
        ),
    );
    let scrollback_len_before = app.agents[&id].scrollback.len();

    // Drive the real production handler.
    let (args, _rx) = make_ask_user_question_args("acp-driven-question");
    let handled = crate::app::acp_handler::handle_ask_user_question(args, &mut app);
    assert!(handled, "handler must return true (ACP question accepted)");

    // The new ACP question replaced the local fork modal.
    let qv = app.agents[&id]
        .question_view
        .as_ref()
        .expect("new ACP question must be active");
    assert_eq!(qv.tool_call_id, "acp-driven-question");
    assert!(
        qv.local_kind.is_none(),
        "ACP-driven question must not have local_kind set"
    );
    // The displaced local modal explains why the question disappeared.
    let last = app.agents[&id]
        .scrollback
        .get(app.agents[&id].scrollback.len() - 1)
        .expect("scrollback should have a new entry");
    match &last.block {
        RenderBlock::System(sys) => {
            assert_eq!(sys.text, "/fork cancelled because another question opened.")
        }
        other => panic!("expected System block, got {other:?}"),
    }
    assert_eq!(
        app.agents[&id].scrollback.len(),
        scrollback_len_before + 1,
        "exactly one system block pushed"
    );
}
