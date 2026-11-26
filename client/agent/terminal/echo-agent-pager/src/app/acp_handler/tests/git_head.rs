#![cfg_attr(rustfmt, rustfmt::skip)]
    use super::*;

    // ── derive_child_cwd ─────────────────────────────────────────────

    #[test]
    fn derive_child_cwd_uses_child_cwd_from_info() {
        let parent_cwd = PathBuf::from("/parent/cwd");
        let mut info = make_subagent_info("child-1");
        info.child_cwd = Some("/child/worktree".into());
        info.worktree_path = Some("/child/worktree".into());

        let (cwd, is_wt) = derive_child_cwd(&parent_cwd, Some(&info));
        assert_eq!(cwd, PathBuf::from("/child/worktree"));
        assert!(is_wt);
    }

    #[test]
    fn derive_child_cwd_falls_back_to_parent_when_child_cwd_is_none() {
        let parent_cwd = PathBuf::from("/parent/cwd");
        let info = make_subagent_info("child-2");

        let (cwd, is_wt) = derive_child_cwd(&parent_cwd, Some(&info));
        assert_eq!(cwd, PathBuf::from("/parent/cwd"));
        assert!(!is_wt);
    }

    #[test]
    fn derive_child_cwd_worktree_independent_of_child_cwd() {
        let parent_cwd = PathBuf::from("/parent/cwd");
        let mut info = make_subagent_info("child-3");
        info.child_cwd = None;
        info.worktree_path = Some("/some/worktree".into());

        let (cwd, is_wt) = derive_child_cwd(&parent_cwd, Some(&info));
        assert_eq!(cwd, PathBuf::from("/parent/cwd"), "falls back to parent");
        assert!(
            is_wt,
            "worktree flag must be set even when child_cwd is None"
        );
    }

    #[test]
    fn derive_child_cwd_no_info_falls_back() {
        let parent_cwd = PathBuf::from("/parent/cwd");
        let (cwd, is_wt) = derive_child_cwd(&parent_cwd, None);
        assert_eq!(cwd, PathBuf::from("/parent/cwd"));
        assert!(!is_wt);
    }

    #[test]
    fn git_head_changed_updates_root_agent() {
        let mut app = make_app_with_agent("sess-A");
        let notif = make_git_head_changed_notif("sess-A", Some("feature/x"), false, None);
        let changed = handle_git_head_changed(&notif, &mut app);

        assert!(changed);
        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(agent.current_branch.as_deref(), Some("feature/x"));
        assert!(!agent.is_worktree);
        assert!(agent.main_repo.is_none());
    }

    #[test]
    fn git_head_changed_routes_to_child_subagent_view() {
        let mut app = make_app_with_agent("sess-A");
        let child_sid = "child-sess-1";
        {
            let parent = app.agents.get_mut(&AgentId(0)).unwrap();
            parent
                .subagent_views
                .insert(child_sid.into(), Box::new(make_agent(Some(child_sid))));
        }

        let notif = make_git_head_changed_notif(
            child_sid,
            Some("worktree-branch"),
            true,
            Some("main-repo"),
        );
        let changed = handle_git_head_changed(&notif, &mut app);

        assert!(changed);
        let parent = app.agents.get(&AgentId(0)).unwrap();
        let child_view = parent.subagent_views.get(child_sid).unwrap();
        assert_eq!(
            child_view.current_branch.as_deref(),
            Some("worktree-branch")
        );
        assert!(child_view.is_worktree);
        assert_eq!(child_view.main_repo.as_deref(), Some("main-repo"));
        // Parent must not be affected.
        assert!(parent.current_branch.is_none());
        assert!(!parent.is_worktree);
    }

    #[test]
    fn git_head_changed_unknown_session_returns_false() {
        let mut app = make_app_with_agent("sess-A");
        let notif = make_git_head_changed_notif("unknown-sess", Some("main"), false, None);
        let changed = handle_git_head_changed(&notif, &mut app);

        assert!(!changed);
        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(agent.current_branch.is_none());
    }

    #[test]
    fn git_head_changed_root_agent_not_affected_when_child_matches() {
        let mut app = make_app_with_agent("sess-A");
        let child_sid = "child-sess-2";
        {
            let parent = app.agents.get_mut(&AgentId(0)).unwrap();
            parent
                .subagent_views
                .insert(child_sid.into(), Box::new(make_agent(Some(child_sid))));
            parent.current_branch = Some("parent-branch".into());
        }

        let notif = make_git_head_changed_notif(child_sid, Some("child-branch"), true, None);
        handle_git_head_changed(&notif, &mut app);

        let parent = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(
            parent.current_branch.as_deref(),
            Some("parent-branch"),
            "parent's branch must not change when child is updated"
        );
        let child_view = parent.subagent_views.get(child_sid).unwrap();
        assert_eq!(child_view.current_branch.as_deref(), Some("child-branch"));
    }

