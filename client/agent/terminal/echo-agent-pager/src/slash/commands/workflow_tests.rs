use super::*;
use crate::acp::model_state::ModelState;
use crate::app::ScreenMode;
use crate::app::bundle::BundleState;
use crate::settings::PagerLocalSnapshot;

static DEFAULT_BUNDLE_STATE: BundleState = BundleState {
    has_cache: false,
    version: String::new(),
    personas: Vec::new(),
    roles: Vec::new(),
    agents: Vec::new(),
    skills: Vec::new(),
    persona_details: Vec::new(),
    role_details: Vec::new(),
};

fn model_with_reasoning() -> ModelState {
    let mut state = ModelState::default();
    let id = agent_client_protocol::ModelId::new(std::sync::Arc::from("reasoning-x"));
    let meta = serde_json::json!({
        "supportsReasoningEffort": true,
        "reasoningEfforts": [
            { "id": "deep", "value": "xhigh", "label": "Deep", "description": "Max" },
            { "value": "high", "label": "High" },
            { "value": "low", "label": "Low" },
        ],
    })
    .as_object()
    .cloned();
    let info =
        agent_client_protocol::ModelInfo::new(id.clone(), "Reasoning X".to_string()).meta(meta);
    state.available.insert(id.clone(), info);
    state.current = Some(id);
    state
}

fn app_ctx<'a>(
    models: &'a ModelState,
    saved_workflows: &'a [crate::slash::WorkflowChoice],
) -> AppCtx<'a> {
    AppCtx {
        models,
        cwd: std::path::Path::new("."),
        has_session_announcements: false,
        billing_surface_visible: true,
        usage_command_visible: true,
        workflows_available: true,
        saved_workflows,
        workflow_runs: &[],
        screen_mode: ScreenMode::Fullscreen,
        current_title: None,
    }
}

fn exec_ctx(models: &ModelState, screen_mode: ScreenMode) -> CommandExecCtx<'_> {
    CommandExecCtx {
        models,
        session_id: None,
        bundle_state: &DEFAULT_BUNDLE_STATE,
        screen_mode,
        billing_surface_visible: true,
        usage_command_visible: true,
        pager_state: PagerLocalSnapshot::default(),
    }
}

#[test]
fn hidden_until_shell_advertises_workflow_support() {
    let models = ModelState::default();
    for (available, want) in [(false, false), (true, true)] {
        let ctx = AppCtx {
            models: &models,
            cwd: std::path::Path::new("."),
            has_session_announcements: false,
            billing_surface_visible: true,
            usage_command_visible: true,
            workflows_available: available,
            saved_workflows: &[],
            workflow_runs: &[],
            screen_mode: ScreenMode::Fullscreen,
            current_title: None,
        };
        assert_eq!(WorkflowCommand.visible(&ctx), want);
    }
}

#[test]
fn runs_form_toggles_dashboard_in_fullscreen_and_inline() {
    let models = ModelState::default();
    // Inline (--no-alt-screen) renders the pane too — it counts as
    // fullscreen for mode gating.
    for screen_mode in [ScreenMode::Fullscreen, ScreenMode::Inline] {
        let mut ctx = exec_ctx(&models, screen_mode);
        for args in ["runs", "RUNS", "  runs  "] {
            assert!(
                matches!(
                    WorkflowCommand.run(&mut ctx, args),
                    CommandResult::Action(Action::ToggleWorkflows)
                ),
                "screen_mode: {screen_mode:?}, args: {args:?}"
            );
        }
    }
}

#[test]
fn runs_form_passes_through_in_minimal() {
    let models = ModelState::default();
    let mut ctx = exec_ctx(&models, ScreenMode::Minimal);
    assert!(matches!(
        WorkflowCommand.run(&mut ctx, "runs"),
        CommandResult::PassThrough(text) if text == "/workflow runs"
    ));
}

#[test]
fn takes_args_so_placeholder_and_ops_surface() {
    // Without takes_args Tab stays in command-token phase, so the placeholder and op rows vanish.
    assert!(WorkflowCommand.takes_args());
    assert!(!WorkflowCommand.args_required());
    let models = ModelState::default();
    let ctx = AppCtx {
        models: &models,
        cwd: std::path::Path::new("."),
        has_session_announcements: false,
        billing_surface_visible: true,
        usage_command_visible: true,
        workflows_available: true,
        saved_workflows: &[],
        workflow_runs: &[],
        screen_mode: ScreenMode::Fullscreen,
        current_title: None,
    };
    let items = WorkflowCommand
        .suggest_args(&ctx, "")
        .expect("ops must be advertised");
    let ops: Vec<&str> = items.iter().map(|i| i.display.as_str()).collect();
    assert_eq!(ops, ["runs", "pause", "resume", "stop", "save"]);
}

#[test]
fn suggest_args_lists_saved_workflows_then_ops() {
    let models = ModelState::default();
    let saved = [
        crate::slash::WorkflowChoice {
            name: "deep-research".into(),
            description: "Research with bounded parallel agents".into(),
        },
        crate::slash::WorkflowChoice {
            name: "demo".into(),
            description: "Demo workflow".into(),
        },
    ];
    let ctx = AppCtx {
        models: &models,
        cwd: std::path::Path::new("."),
        has_session_announcements: false,
        billing_surface_visible: true,
        usage_command_visible: true,
        workflows_available: true,
        saved_workflows: &saved,
        workflow_runs: &[],
        screen_mode: ScreenMode::Fullscreen,
        current_title: None,
    };
    let items = WorkflowCommand
        .suggest_args(&ctx, "")
        .expect("names and ops");
    let displays: Vec<&str> = items.iter().map(|i| i.display.as_str()).collect();
    assert_eq!(
        displays,
        [
            "deep-research",
            "demo",
            "runs",
            "pause",
            "resume",
            "stop",
            "save"
        ]
    );
    let deep = items.iter().find(|i| i.display == "deep-research").unwrap();
    assert_eq!(deep.insert_text, "deep-research ");
    assert_eq!(deep.description, "Research with bounded parallel agents");
}

#[test]
fn launch_name_suggests_budget_and_effort_flags() {
    let models = model_with_reasoning();
    let saved = [crate::slash::WorkflowChoice {
        name: "audit".into(),
        description: "Research".into(),
    }];
    let ctx = AppCtx {
        models: &models,
        cwd: std::path::Path::new("."),
        has_session_announcements: false,
        billing_surface_visible: true,
        usage_command_visible: true,
        workflows_available: true,
        saved_workflows: &saved,
        workflow_runs: &[],
        screen_mode: ScreenMode::Fullscreen,
        current_title: None,
    };
    let items = WorkflowCommand
        .suggest_args(&ctx, "audit ")
        .expect("launch flags");
    assert_eq!(items[0].display, "--agent-budget");
    assert_eq!(items[0].insert_text, "audit --agent-budget ");
    let effort = items
        .iter()
        .find(|item| item.display == "--effort xhigh")
        .expect("remapped effort flag");
    assert_eq!(effort.match_text, "audit --effort xhigh deep Deep");
    assert_eq!(effort.insert_text, "audit --effort xhigh ");
    assert_eq!(effort.description, "Deep — Max");

    for (query, expected) in [
        (
            "audit -",
            &[
                "--agent-budget",
                "--effort xhigh",
                "--effort high",
                "--effort low",
            ][..],
        ),
        (
            "audit --e",
            &["--effort xhigh", "--effort high", "--effort low"][..],
        ),
        ("audit --agent", &["--agent-budget"][..]),
        ("audit --agent-budget", &["--agent-budget"][..]),
        (
            "audit --effort",
            &["--effort xhigh", "--effort high", "--effort low"][..],
        ),
        (
            "audit --effort ",
            &["--effort xhigh", "--effort high", "--effort low"][..],
        ),
        (
            "audit --effort x",
            &["--effort xhigh", "--effort high", "--effort low"][..],
        ),
        (
            "audit --effort deep",
            &["--effort xhigh", "--effort high", "--effort low"][..],
        ),
        (
            "audit --effort Deep",
            &["--effort xhigh", "--effort high", "--effort low"][..],
        ),
    ] {
        let items = WorkflowCommand
            .suggest_args(&ctx, query)
            .expect("supported flag prefix");
        let displays: Vec<&str> = items.iter().map(|item| item.display.as_str()).collect();
        assert_eq!(displays, expected, "query: {query:?}");
    }

    for query in [
        "audit deep",
        "audit audit this",
        r#"audit {"query":"audit"}"#,
        "audit target=prod",
        "audit --agent-budget ",
        "audit --effort=unknown",
        "audit -x",
    ] {
        assert!(
            WorkflowCommand.suggest_args(&ctx, query).is_none(),
            "completed or nonflag args must close suggestions: {query:?}"
        );
    }
    assert!(WorkflowCommand.suggest_args(&ctx, "runs extra").is_none());
}

#[test]
fn registered_launch_flag_specs_drive_remaining_suggestions() {
    let models = model_with_reasoning();
    let saved = [crate::slash::WorkflowChoice {
        name: "demo".into(),
        description: "Demo workflow".into(),
    }];
    let ctx = app_ctx(&models, &saved);

    for (query, expected, insert_text) in [
        (
            "demo --agent-budget 5 ",
            &["--effort xhigh", "--effort high", "--effort low"][..],
            "demo --agent-budget 5 --effort xhigh ",
        ),
        (
            "demo --agent-budget=5 ",
            &["--effort xhigh", "--effort high", "--effort low"][..],
            "demo --agent-budget=5 --effort xhigh ",
        ),
        (
            "demo --effort low ",
            &["--agent-budget"][..],
            "demo --effort low --agent-budget ",
        ),
        (
            "demo --effort=low ",
            &["--agent-budget"][..],
            "demo --effort=low --agent-budget ",
        ),
    ] {
        let items = WorkflowCommand
            .suggest_args(&ctx, query)
            .expect("remaining launch flag");
        assert_eq!(
            items
                .iter()
                .map(|item| item.display.as_str())
                .collect::<Vec<_>>(),
            expected,
            "query: {query:?}"
        );
        assert_eq!(items[0].insert_text, insert_text, "query: {query:?}");
    }

    for (query, expected) in [
        (
            "demo --agent-budget 5 --e",
            &["--effort xhigh", "--effort high", "--effort low"][..],
        ),
        ("demo --effort low --a", &["--agent-budget"][..]),
    ] {
        let items = WorkflowCommand
            .suggest_args(&ctx, query)
            .expect("remaining flag prefix");
        assert_eq!(
            items
                .iter()
                .map(|item| item.display.as_str())
                .collect::<Vec<_>>(),
            expected,
            "query: {query:?}"
        );
    }
}

#[test]
fn invalid_duplicate_and_complete_launch_flags_close_suggestions() {
    let models = model_with_reasoning();
    let saved = [crate::slash::WorkflowChoice {
        name: "demo".into(),
        description: "Demo workflow".into(),
    }];
    let ctx = app_ctx(&models, &saved);

    for query in [
        "demo --agent-budget 5 --agent",
        "demo --agent-budget=5 --agent-budget ",
        "demo --effort low --effort",
        "demo --effort=low --effort=high ",
        "demo --agent-budget 5 --effort low ",
        "demo --effort=low --agent-budget=5 ",
        "demo --agent-budget 5 audit",
        "demo --effort low target=prod",
        r#"demo --agent-budget 5 {"query":"audit"}"#,
        "demo --bogus ",
    ] {
        assert!(
            WorkflowCommand.suggest_args(&ctx, query).is_none(),
            "query must close suggestions: {query:?}"
        );
    }
}

#[test]
fn exact_effort_values_advance_only_after_trailing_whitespace() {
    let models = model_with_reasoning();
    let saved = [crate::slash::WorkflowChoice {
        name: "demo".into(),
        description: "Demo workflow".into(),
    }];
    let ctx = app_ctx(&models, &saved);

    for query in [
        "demo --effort xhigh",
        "demo --effort deep",
        "demo --effort Deep",
        "demo --effort=high",
    ] {
        let items = WorkflowCommand
            .suggest_args(&ctx, query)
            .expect("exact effort value remains selectable before whitespace");
        assert!(
            items
                .iter()
                .any(|item| item.display.starts_with("--effort ")),
            "query: {query:?}"
        );
    }
    for query in [
        "demo --effort xhigh ",
        "demo --effort deep ",
        "demo --effort Deep ",
        "demo --effort=high ",
    ] {
        let items = WorkflowCommand
            .suggest_args(&ctx, query)
            .expect("trailing whitespace advances to remaining flags");
        assert_eq!(items.len(), 1, "query: {query:?}");
        assert_eq!(items[0].display, "--agent-budget", "query: {query:?}");
    }
}

#[test]
fn manage_op_suggests_stoppable_run_names() {
    let models = ModelState::default();
    let runs = [
        crate::slash::WorkflowRunChoice {
            name: "demo".into(),
            status: "complete".into(),
            builtin: false,
        },
        crate::slash::WorkflowRunChoice {
            name: "demo-2".into(),
            status: "active".into(),
            builtin: false,
        },
    ];
    let ctx = AppCtx {
        models: &models,
        cwd: std::path::Path::new("."),
        has_session_announcements: false,
        billing_surface_visible: true,
        usage_command_visible: true,
        workflows_available: true,
        saved_workflows: &[],
        workflow_runs: &runs,
        screen_mode: ScreenMode::Fullscreen,
        current_title: None,
    };
    let items = WorkflowCommand
        .suggest_args(&ctx, "stop ")
        .expect("stoppable runs");
    let displays: Vec<&str> = items.iter().map(|i| i.display.as_str()).collect();
    assert_eq!(displays, ["demo-2"]);
    assert_eq!(items[0].insert_text, "stop demo-2");
    assert_eq!(items[0].match_text, "stop demo-2");

    let spaced = WorkflowCommand
        .suggest_args(&ctx, "stop demo")
        .expect("prefix still lists runs");
    assert_eq!(
        spaced
            .iter()
            .map(|i| i.display.as_str())
            .collect::<Vec<_>>(),
        ["demo-2"]
    );
}

#[test]
fn resume_lists_budget_limited_and_save_skips_builtins() {
    let models = ModelState::default();
    let catalog = [
        crate::slash::WorkflowChoice {
            name: "deep-research".into(),
            description: "Research".into(),
        },
        crate::slash::WorkflowChoice {
            name: "demo".into(),
            description: "Review".into(),
        },
        crate::slash::WorkflowChoice {
            name: "sprint-2".into(),
            description: "Sprint".into(),
        },
    ];
    let runs = [
        crate::slash::WorkflowRunChoice {
            name: "deep-research".into(),
            status: "budget_limited".into(),
            builtin: true,
        },
        crate::slash::WorkflowRunChoice {
            name: "demo".into(),
            status: "user_paused".into(),
            builtin: false,
        },
    ];
    let ctx = AppCtx {
        models: &models,
        cwd: std::path::Path::new("."),
        has_session_announcements: false,
        billing_surface_visible: true,
        usage_command_visible: true,
        workflows_available: true,
        saved_workflows: &catalog,
        workflow_runs: &runs,
        screen_mode: ScreenMode::Fullscreen,
        current_title: None,
    };
    let resume_items = WorkflowCommand
        .suggest_args(&ctx, "resume ")
        .expect("resumable");
    let resume: Vec<&str> = resume_items.iter().map(|i| i.display.as_str()).collect();
    assert_eq!(resume, ["demo"]);
    let save_items = WorkflowCommand
        .suggest_args(&ctx, "save ")
        .expect("savable");
    let save: Vec<&str> = save_items.iter().map(|i| i.display.as_str()).collect();
    assert_eq!(save, ["demo"]);

    let numbered = [
        crate::slash::WorkflowRunChoice {
            name: "demo-2".into(),
            status: "complete".into(),
            builtin: false,
        },
        crate::slash::WorkflowRunChoice {
            name: "demo".into(),
            status: "complete".into(),
            builtin: false,
        },
        crate::slash::WorkflowRunChoice {
            name: "sprint-2".into(),
            status: "complete".into(),
            builtin: false,
        },
    ];
    let numbered_ctx = AppCtx {
        models: &models,
        cwd: std::path::Path::new("."),
        has_session_announcements: false,
        billing_surface_visible: true,
        usage_command_visible: true,
        workflows_available: true,
        saved_workflows: &catalog,
        workflow_runs: &numbered,
        screen_mode: ScreenMode::Fullscreen,
        current_title: None,
    };
    let save_numbered_items = WorkflowCommand
        .suggest_args(&numbered_ctx, "save ")
        .expect("savable");
    let save_numbered: Vec<&str> = save_numbered_items
        .iter()
        .map(|i| i.display.as_str())
        .collect();
    assert_eq!(save_numbered, ["demo", "sprint-2"]);
}

#[test]
fn exact_resume_verb_lists_paused_runs_not_saved_names() {
    use crate::slash::{SlashController, SlashState, WorkflowRunChoice};

    let mut ctrl = SlashController::with_builtins(std::path::PathBuf::from("."));
    ctrl.set_workflows_available(true);
    let wf = agent_client_protocol::AvailableCommand::new(
        "demo".to_string(),
        "Workflow: Demo workflow".to_string(),
    )
    .meta(
        serde_json::json!({ "workflowSource": "user" })
            .as_object()
            .cloned()
            .expect("object"),
    );
    ctrl.registry_mut().set_acp_commands(&[wf]);
    ctrl.set_workflow_runs(vec![
        WorkflowRunChoice {
            name: "demo".into(),
            status: "cancelled".into(),
            builtin: false,
        },
        WorkflowRunChoice {
            name: "demo-2".into(),
            status: "blocked".into(),
            builtin: false,
        },
        WorkflowRunChoice {
            name: "active-one".into(),
            status: "active".into(),
            builtin: false,
        },
    ]);
    let state = SlashState::default();
    let models = ModelState::default();
    let text = "/workflow resume";
    ctrl.refresh(&state, text, text.len(), &models);
    let snapshot = state.snapshot();
    let displays: Vec<&str> = snapshot
        .matches
        .iter()
        .map(|m| m.display.as_str())
        .collect();
    assert_eq!(
        displays,
        ["demo"],
        "resume lists stopped runs, not still-blocked ones: {displays:?}"
    );
    assert!(!displays.contains(&"demo-2"));
    assert!(!displays.contains(&"active-one"));
    assert!(!displays.contains(&"resume"));
}

#[test]
fn args_phase_shows_placeholder_and_highlighted_ops() {
    use crate::slash::{SlashController, SlashState};

    let mut ctrl = SlashController::with_builtins(std::path::PathBuf::from("."));
    ctrl.set_workflows_available(true);
    let state = SlashState::default();
    let models = ModelState::default();
    let text = "/workflow ";
    ctrl.refresh(&state, text, text.len(), &models);
    let snapshot = state.snapshot();
    assert_eq!(
        snapshot.args_placeholder.as_deref(),
        Some(
            "<name> [--agent-budget N] [--effort LEVEL] [args] | runs | pause|resume|stop|save [name]"
        )
    );
    let displays: Vec<&str> = snapshot
        .matches
        .iter()
        .map(|m| m.display.as_str())
        .collect();
    assert_eq!(displays, ["runs", "pause", "resume", "stop", "save"]);
}

#[test]
fn args_phase_lists_saved_workflows_from_acp_catalog() {
    use crate::slash::{SlashController, SlashState};

    let mut ctrl = SlashController::with_builtins(std::path::PathBuf::from("."));
    ctrl.set_workflows_available(true);
    let wf = agent_client_protocol::AvailableCommand::new(
        "demo".to_string(),
        "Workflow: Demo workflow".to_string(),
    )
    .meta(
        serde_json::json!({ "workflowSource": "user" })
            .as_object()
            .cloned()
            .expect("object"),
    );
    ctrl.registry_mut().set_acp_commands(&[wf]);
    let state = SlashState::default();
    let models = model_with_reasoning();
    let text = "/workflow ";
    ctrl.refresh(&state, text, text.len(), &models);
    let snapshot = state.snapshot();
    let displays: Vec<&str> = snapshot
        .matches
        .iter()
        .map(|m| m.display.as_str())
        .collect();
    assert_eq!(
        displays,
        ["demo", "runs", "pause", "resume", "stop", "save"]
    );
    assert_eq!(snapshot.matches[0].insert_text, "demo ");

    for (text, expected) in [
        (
            "/workflow demo ",
            &[
                "--agent-budget",
                "--effort high",
                "--effort low",
                "--effort xhigh",
            ][..],
        ),
        (
            "/workflow demo -",
            &[
                "--agent-budget",
                "--effort high",
                "--effort low",
                "--effort xhigh",
            ][..],
        ),
        (
            "/workflow demo --e",
            &["--effort high", "--effort low", "--effort xhigh"][..],
        ),
        ("/workflow demo --agent", &["--agent-budget"][..]),
        ("/workflow demo --agent-budget", &["--agent-budget"][..]),
        (
            "/workflow demo --effort",
            &["--effort high", "--effort low", "--effort xhigh"][..],
        ),
        (
            "/workflow demo --effort ",
            &["--effort high", "--effort low", "--effort xhigh"][..],
        ),
        ("/workflow demo --effort x", &["--effort xhigh"][..]),
        ("/workflow demo --effort deep", &["--effort xhigh"][..]),
        ("/workflow demo --effort Deep", &["--effort xhigh"][..]),
        (
            "/workflow demo --agent-budget 5 ",
            &["--effort high", "--effort low", "--effort xhigh"][..],
        ),
        (
            "/workflow demo --agent-budget=5 ",
            &["--effort high", "--effort low", "--effort xhigh"][..],
        ),
        (
            "/workflow demo --agent-budget 5 --e",
            &["--effort high", "--effort low", "--effort xhigh"][..],
        ),
        ("/workflow demo --effort low ", &["--agent-budget"][..]),
        ("/workflow demo --effort=low ", &["--agent-budget"][..]),
        ("/workflow demo --effort low --a", &["--agent-budget"][..]),
    ] {
        ctrl.refresh(&state, text, text.len(), &models);
        let launch_snapshot = state.snapshot();
        let displays: Vec<&str> = launch_snapshot
            .matches
            .iter()
            .map(|item| item.display.as_str())
            .collect();
        assert_eq!(displays, expected, "text: {text:?}");
        assert!(launch_snapshot.open, "text: {text:?}");
        if text.eq_ignore_ascii_case("/workflow demo --effort deep") {
            assert_eq!(
                launch_snapshot
                    .selection()
                    .map(|row| row.insert_text.as_str()),
                Some("demo --effort xhigh ")
            );
        }
    }

    for text in [
        "/workflow demo deep",
        "/workflow demo audit this",
        r#"/workflow demo {"query":"demo"}"#,
        "/workflow demo --agent-budget ",
        "/workflow demo --effort=unknown",
        "/workflow demo -x",
        "/workflow demo --agent-budget 5 --agent",
        "/workflow demo --effort low --effort",
        "/workflow demo --agent-budget 5 --effort low ",
        "/workflow demo --effort=low --agent-budget=5 ",
        "/workflow demo --agent-budget 5 audit",
        r#"/workflow demo --effort low {"query":"demo"}"#,
    ] {
        ctrl.refresh(&state, text, text.len(), &models);
        let snapshot = state.snapshot();
        assert!(snapshot.matches.is_empty(), "text: {text:?}");
        assert!(!snapshot.open, "text: {text:?}");
    }
}

#[test]
fn first_token_prefix_keeps_names_and_ops_open() {
    use crate::slash::{SlashController, SlashState};

    let mut ctrl = SlashController::with_builtins(std::path::PathBuf::from("."));
    ctrl.set_workflows_available(true);
    let wf = agent_client_protocol::AvailableCommand::new(
        "demo".to_string(),
        "Workflow: Demo workflow".to_string(),
    )
    .meta(
        serde_json::json!({ "workflowSource": "user" })
            .as_object()
            .cloned()
            .expect("object"),
    );
    ctrl.registry_mut().set_acp_commands(&[wf]);
    let state = SlashState::default();
    let models = ModelState::default();
    let text = "/workflow de";
    ctrl.refresh(&state, text, text.len(), &models);
    let snapshot = state.snapshot();
    let displays: Vec<&str> = snapshot
        .matches
        .iter()
        .map(|m| m.display.as_str())
        .collect();
    assert!(
        displays.contains(&"demo"),
        "saved name should stay ranked: {displays:?}"
    );
    let resume = "/workflow re";
    ctrl.refresh(&state, resume, resume.len(), &models);
    let resume_snapshot = state.snapshot();
    let resume_displays: Vec<&str> = resume_snapshot
        .matches
        .iter()
        .map(|m| m.display.as_str())
        .collect();
    assert!(
        resume_displays.contains(&"resume"),
        "manage verb should stay ranked: {resume_displays:?}"
    );
    assert!(
        snapshot.open,
        "picker must stay open while the first token is still being typed"
    );
}

#[test]
fn resume_prefix_keeps_matching_run_names_open() {
    use crate::slash::{SlashController, SlashState, WorkflowRunChoice};

    let mut ctrl = SlashController::with_builtins(std::path::PathBuf::from("."));
    ctrl.set_workflows_available(true);
    ctrl.set_workflow_runs(vec![
        WorkflowRunChoice {
            name: "demo".into(),
            status: "user_paused".into(),
            builtin: false,
        },
        WorkflowRunChoice {
            name: "other".into(),
            status: "user_paused".into(),
            builtin: false,
        },
    ]);
    let state = SlashState::default();
    let models = ModelState::default();
    let text = "/workflow resume de";
    ctrl.refresh(&state, text, text.len(), &models);
    let snapshot = state.snapshot();
    let displays: Vec<&str> = snapshot
        .matches
        .iter()
        .map(|m| m.display.as_str())
        .collect();
    assert_eq!(displays, ["demo"], "typed prefix must keep the picker open");
    assert_eq!(snapshot.matches[0].insert_text, "resume demo");
}

#[test]
fn other_forms_pass_through_unchanged() {
    let models = ModelState::default();
    let mut ctx = exec_ctx(&models, ScreenMode::Fullscreen);
    for (args, forwarded) in [
        ("", "/workflow"),
        ("pr-review {\"pr\": 1}", "/workflow pr-review {\"pr\": 1}"),
        ("pause wf_12ab", "/workflow pause wf_12ab"),
        // `runs` with trailing args is a launch form, not the dashboard.
        ("runs extra words", "/workflow runs extra words"),
    ] {
        assert!(
            matches!(
                WorkflowCommand.run(&mut ctx, args),
                CommandResult::PassThrough(text) if text == forwarded
            ),
            "args: {args:?}"
        );
    }
}
