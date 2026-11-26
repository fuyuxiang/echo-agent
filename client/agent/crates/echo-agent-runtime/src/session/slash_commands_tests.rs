use super::*;
use echo_agent_tools::implementations::skills::types::SkillScope;

/// Shadows [`super::resolve`] for the cases that route something other
/// than `/loop`: they are indifferent to the fire mode, and pinning it
/// here keeps a plumbing change out of every unrelated call site. Tests
/// that care about the mode call `super::resolve` directly.
fn resolve(
    prompt_blocks: Vec<acp::ContentBlock>,
    skills: &[SkillInfo],
    availability: CommandAvailability,
    skill_rewrite: SkillSlashRewrite,
    workflows: &[crate::session::workflow::registry::WorkflowListing],
) -> Result<Vec<acp::ContentBlock>, SlashCommandOutcome> {
    super::resolve(
        prompt_blocks,
        skills,
        availability,
        skill_rewrite,
        workflows,
        LoopFireMode::Detached,
    )
}

#[test]
fn acu_skill_source_chat_vs_build() {
    assert_eq!(acu_skill_source(true), AcuSkillSource::Product);
    assert_eq!(acu_skill_source(false), AcuSkillSource::Disk);
}

#[tokio::test(flavor = "current_thread")]
async fn product_skill_infos_none_without_auth() {
    clear_product_skills_cache_for_test();
    assert!(product_skill_infos(None).await.is_none());
}

#[test]
fn product_skills_cache_matches_identity_and_team() {
    use crate::auth::{AuthMode, EchoAgentAuth};
    let base = ProductSkillsCacheEntry {
        auth_key: "tok-a".into(),
        user_id: "user-1".into(),
        team_id: Some("team-a".into()),
        organization_id: None,
        skills: vec![],
        fetched_at: std::time::Instant::now(),
    };
    let same = EchoAgentAuth {
        key: "tok-a".into(),
        user_id: "user-1".into(),
        team_id: Some("team-a".into()),
        auth_mode: AuthMode::Oidc,
        create_time: chrono::Utc::now(),
        ..Default::default()
    };
    assert!(product_skills_cache_matches(&base, &same));
    let other_team = EchoAgentAuth {
        team_id: Some("team-b".into()),
        ..same.clone()
    };
    assert!(!product_skills_cache_matches(&base, &other_team));
    let same_user_other_key = EchoAgentAuth {
        key: "tok-b".into(),
        user_id: "user-1".into(),
        team_id: Some("team-a".into()),
        auth_mode: AuthMode::WebLogin,
        create_time: chrono::Utc::now(),
        ..Default::default()
    };
    assert!(product_skills_cache_matches(&base, &same_user_other_key));
    let other_user = EchoAgentAuth {
        key: "tok-c".into(),
        user_id: "user-2".into(),
        team_id: Some("team-a".into()),
        auth_mode: AuthMode::Oidc,
        create_time: chrono::Utc::now(),
        ..Default::default()
    };
    assert!(!product_skills_cache_matches(&base, &other_user));
    // Personal (empty team) must not hit a team-keyed entry via user_id.
    let personal_same_user = EchoAgentAuth {
        key: "tok-personal".into(),
        user_id: "user-1".into(),
        team_id: None,
        organization_id: None,
        auth_mode: AuthMode::WebLogin,
        create_time: chrono::Utc::now(),
        ..Default::default()
    };
    assert!(!product_skills_cache_matches(&base, &personal_same_user));
}

#[test]
fn product_skills_cache_after_untagged_recovery_keeps_primary_tenant() {
    use crate::auth::{AuthMode, EchoAgentAuth};
    let primary = EchoAgentAuth {
        key: "oidc-team".into(),
        user_id: "user-1".into(),
        team_id: Some("team-a".into()),
        organization_id: Some("org-1".into()),
        auth_mode: AuthMode::Oidc,
        create_time: chrono::Utc::now(),
        ..Default::default()
    };
    let entry = product_skills_cache_entry_after_fetch(&primary, vec![], true);
    assert_eq!(entry.team_id.as_deref(), Some("team-a"));
    assert_eq!(entry.organization_id.as_deref(), Some("org-1"));
    assert_eq!(entry.user_id, "user-1");
    assert!(product_skills_cache_matches(&entry, &primary));
    let personal = EchoAgentAuth {
        key: "web-personal".into(),
        user_id: "user-1".into(),
        team_id: None,
        organization_id: None,
        auth_mode: AuthMode::WebLogin,
        create_time: chrono::Utc::now(),
        ..Default::default()
    };
    assert!(!product_skills_cache_matches(&entry, &personal));
}

fn all_gated() -> CommandAvailability {
    CommandAvailability::all_enabled()
}

fn text_block(s: &str) -> acp::ContentBlock {
    acp::ContentBlock::Text(acp::TextContent::new(s.to_string()))
}

fn make_skill(name: &str, user_invocable: bool) -> SkillInfo {
    SkillInfo {
        name: name.to_string(),
        display_name: None,
        description: format!("A skill called {name}"),
        when_to_use: None,
        short_description: Some(format!("Short: {name}")),
        author: None,
        argument_hint: None,
        path: format!("/path/to/{name}/SKILL.md"),
        scope: SkillScope::Local,
        config_source: None,
        plugin_name: None,
        plugin_version: None,
        plugin_root: None,
        plugin_data: None,
        allowed_tools: None,
        license: None,
        compatibility: None,
        metadata: None,
        model: None,
        effort: None,
        user_invocable,
        disable_model_invocation: false,
        has_user_specified_description: false,
        paths: None,
        enabled: true,
        body: None,
    }
}

/// Extract the first parsed skill from an InvokeSkill outcome.
fn first_skill(outcome: SlashCommandOutcome) -> ParsedSkillRef {
    match outcome {
        SlashCommandOutcome::InvokeSkill { skills, .. } => {
            assert!(!skills.is_empty(), "expected at least one skill");
            skills.into_iter().next().unwrap()
        }
        _ => panic!("expected InvokeSkill"),
    }
}

/// Extract original text from InvokeSkill blocks (for prompt-only commands like /loop).
fn invoke_text(outcome: SlashCommandOutcome) -> String {
    match outcome {
        SlashCommandOutcome::InvokeSkill { blocks, .. } => blocks
            .iter()
            .find_map(|b| match b {
                acp::ContentBlock::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .unwrap(),
        _ => panic!("expected InvokeSkill"),
    }
}

// ── parse_slash_prefix ──────────────────────────────────────────

#[test]
fn parse_slash_prefix_extracts_name_and_args() {
    assert_eq!(
        parse_slash_prefix(&[text_block("/compact keep auth")]),
        Some(("compact", "keep auth")),
    );
    assert_eq!(
        parse_slash_prefix(&[text_block("/yolo")]),
        Some(("yolo", "")),
    );
}

#[test]
fn parse_slash_prefix_ignores_non_leading_slash() {
    assert_eq!(
        parse_slash_prefix(&[text_block("please run /commit")]),
        None
    );
    assert_eq!(parse_slash_prefix(&[text_block("fix the bug")]), None);
    assert_eq!(parse_slash_prefix(&[text_block("/")]), None);
}

#[test]
fn parse_slash_prefix_trims_whitespace() {
    assert_eq!(
        parse_slash_prefix(&[text_block("  /commit fix typo  ")]),
        Some(("commit", "fix typo")),
    );
}

// ── builtin resolve fns ─────────────────────────────────────────

fn resolve_builtin(name: &str, args: &str) -> Option<BuiltinAction> {
    BUILTIN_COMMANDS
        .iter()
        .chain(PROMPT_COMMANDS.iter())
        .find(|b| b.name == name)
        .map(|b| (b.resolve)(args))
}

#[test]
fn compact_parses_optional_context() {
    assert!(matches!(
        resolve_builtin("compact", ""),
        Some(BuiltinAction::Compact { user_context: None })
    ));
    assert!(matches!(
        resolve_builtin("compact", "keep auth"),
        Some(BuiltinAction::Compact { user_context: Some(ctx) }) if ctx == "keep auth"
    ));
}

#[test]
fn always_approve_parses_on_off() {
    for arg in ["", "on", "true", "1", "yes", "enable"] {
        assert!(
            matches!(
                resolve_builtin("always-approve", arg),
                Some(BuiltinAction::SetYolo { enabled: true })
            ),
            "expected on for {arg:?}",
        );
    }
    for arg in ["off", "false", "0", "no", "disable"] {
        assert!(
            matches!(
                resolve_builtin("always-approve", arg),
                Some(BuiltinAction::SetYolo { enabled: false })
            ),
            "expected off for {arg:?}",
        );
    }
}

#[test]
fn yolo_alias_resolves_to_always_approve() {
    // /yolo should resolve via alias to the always-approve command
    let blocks = vec![text_block("/yolo on")];
    let outcome = resolve(blocks, &[], all_gated(), SkillSlashRewrite::default(), &[]).unwrap_err();
    assert!(matches!(
        outcome,
        SlashCommandOutcome::Builtin(BuiltinAction::SetYolo { enabled: true })
    ));
}

#[test]
fn host_automation_metadata_never_becomes_builtin_command_arguments() {
    use crate::session::helpers::host_prompt_context::AUTOMATION_CONTEXT_KEY;
    for command in [
        "/yolo off",
        "/always-approve off",
        "/memory off",
        "/compact preserve auth",
    ] {
        let block = acp::TextContent::new(command.to_owned()).meta(acp::Meta::from_iter([
            (
                AUTOMATION_CONTEXT_KEY.into(),
                serde_json::json!("[Host automation capability selection]\nturnId=current"),
            ),
            ("displayText".into(), serde_json::json!(command)),
        ]));
        let outcome = resolve(
            vec![acp::ContentBlock::Text(block)],
            &[],
            all_gated(),
            SkillSlashRewrite::default(),
            &[],
        )
        .unwrap_err();
        match outcome {
            SlashCommandOutcome::Builtin(BuiltinAction::SetYolo { enabled }) => assert!(!enabled),
            SlashCommandOutcome::Builtin(BuiltinAction::MemoryToggle { enabled }) => {
                assert!(!enabled)
            }
            SlashCommandOutcome::Builtin(BuiltinAction::Compact { user_context }) => {
                assert_eq!(user_context.as_deref(), Some("preserve auth"))
            }
            _ => panic!("host context changed command resolution"),
        }
    }
}

#[test]
fn desktop_skill_receives_original_arguments_and_keeps_the_host_turn_metadata() {
    use crate::session::helpers::host_prompt_context::{
        AUTOMATION_CONTEXT_KEY, automation_context,
    };
    let block =
        acp::TextContent::new("/desktop-task 保存报告".to_owned()).meta(acp::Meta::from_iter([(
            AUTOMATION_CONTEXT_KEY.into(),
            serde_json::json!("turnId=current"),
        )]));
    let outcome = resolve(
        vec![acp::ContentBlock::Text(block)],
        &[make_skill("desktop-task", true)],
        all_gated(),
        SkillSlashRewrite::default(),
        &[],
    )
    .unwrap_err();
    let SlashCommandOutcome::InvokeSkill { blocks, skills } = outcome else {
        panic!("expected the requested skill");
    };
    assert_eq!(skills[0].args, "保存报告");
    let acp::ContentBlock::Text(text) = &blocks[0] else {
        panic!("missing user input")
    };
    assert_eq!(
        automation_context(text.meta.as_ref().unwrap()),
        Some("turnId=current")
    );
}

// ── resolve ─────────────────────────────────────────────────────

#[test]
fn resolve_routes_builtin() {
    let outcome = resolve(
        vec![text_block("/compact preserve auth")],
        &[],
        all_gated(),
        SkillSlashRewrite::default(),
        &[],
    )
    .unwrap_err();
    assert!(matches!(
        outcome,
        SlashCommandOutcome::Builtin(BuiltinAction::Compact { user_context: Some(ctx) })
        if ctx == "preserve auth"
    ));
}

#[test]
fn status_alias_resolves_to_session_info() {
    let outcome = resolve(
        vec![text_block("/status")],
        &[],
        all_gated(),
        SkillSlashRewrite::default(),
        &[],
    )
    .unwrap_err();
    assert!(matches!(
        outcome,
        SlashCommandOutcome::Builtin(BuiltinAction::SessionInfo)
    ));
}

#[test]
fn resolve_parses_skill_with_args() {
    let skills = vec![make_skill("commit", true)];
    let outcome = resolve(
        vec![text_block("/commit fix typo")],
        &skills,
        all_gated(),
        SkillSlashRewrite::default(),
        &[],
    )
    .unwrap_err();
    let skill = first_skill(outcome);
    assert_eq!(skill.name, "commit");
    assert_eq!(skill.args, "fix typo");

    let outcome = resolve(
        vec![text_block("/commit")],
        &skills,
        all_gated(),
        SkillSlashRewrite::default(),
        &[],
    )
    .unwrap_err();
    let skill = first_skill(outcome);
    assert_eq!(skill.name, "commit");
    assert_eq!(skill.args, "");
}

/// `build_skill_information_for_refs` loads the SKILL.md, applies
/// substitutions, and wraps everything in `<skill_information>`;
/// unloadable refs are skipped, and no loadable content → `None`.
/// Shared by turn start and the interjection drain.
#[tokio::test]
async fn build_skill_information_for_refs_loads_and_wraps() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("SKILL.md");
    std::fs::write(&path, "Body with $ARGUMENTS").unwrap();

    let mut skill = make_skill("commit", true);
    skill.path = path.to_string_lossy().to_string();
    let skills = vec![skill];

    let parsed = parse_skill_references("/commit fix typo", &skills, all_gated())
        .expect("known skill must parse");
    let info = build_skill_information_for_refs(&parsed, &skills, "sid-1")
        .await
        .expect("skill body must load");
    assert!(info.starts_with("<skill_information>"), "got: {info}");
    assert!(
        info.contains("<skill name=\"commit\" args=\"fix typo\">"),
        "got: {info}"
    );
    assert!(
        info.contains("Body with fix typo"),
        "$ARGUMENTS must substitute: {info}"
    );

    // Missing file → logged, skipped, and with nothing loaded: None.
    let missing = vec![make_skill("ghost", true)];
    let parsed =
        parse_skill_references("/ghost", &missing, all_gated()).expect("known skill must parse");
    assert_eq!(
        build_skill_information_for_refs(&parsed, &missing, "sid-1").await,
        None
    );
}

#[test]
fn resolve_loop_annotates_block_with_compact_display_text() {
    let outcome = resolve(
        vec![text_block("/loop 1m echo hello")],
        &[],
        all_gated(),
        SkillSlashRewrite::default(),
        &[],
    )
    .unwrap_err();
    let blocks = match outcome {
        SlashCommandOutcome::InvokeSkill { blocks, skills } => {
            assert!(skills.is_empty(), "/loop is a prompt-only command");
            blocks
        }
        _ => panic!("expected InvokeSkill for /loop"),
    };
    let acp::ContentBlock::Text(tb) = blocks.first().expect("one block") else {
        panic!("expected a text block");
    };
    assert!(
        tb.text.len() > "/loop 1m echo hello".len(),
        "wire text should be the expanded instruction"
    );
    let display = tb
        .meta
        .as_ref()
        .and_then(|m| m.get("displayText"))
        .and_then(|v| v.as_str());
    assert_eq!(display, Some("/loop 1m echo hello"));
    assert!(
        tb.meta
            .as_ref()
            .and_then(|m| m.get("displayAsSkill"))
            .is_none(),
        "/loop renders as a plain prompt, not a skill"
    );
}

#[test]
fn resolve_loop_without_args_uses_bare_command_display_text() {
    // `/loop` with no args expands to the usage message but should still
    // carry a sensible compact `displayText`.
    let outcome = resolve(
        vec![text_block("/loop")],
        &[],
        all_gated(),
        SkillSlashRewrite::default(),
        &[],
    )
    .unwrap_err();
    let SlashCommandOutcome::InvokeSkill { blocks, .. } = outcome else {
        panic!("expected InvokeSkill for /loop");
    };
    let acp::ContentBlock::Text(tb) = blocks.first().expect("one block") else {
        panic!("expected a text block");
    };
    assert_eq!(
        tb.meta
            .as_ref()
            .and_then(|m| m.get("displayText"))
            .and_then(|v| v.as_str()),
        Some("/loop")
    );
}

#[test]
fn resolve_loop_expands_for_the_sessions_fire_mode() {
    let text_of = |mode| {
        let outcome = super::resolve(
            vec![text_block("/loop 1m echo hello")],
            &[],
            all_gated(),
            SkillSlashRewrite::default(),
            &[],
            mode,
        )
        .unwrap_err();
        let SlashCommandOutcome::InvokeSkill { blocks, .. } = outcome else {
            panic!("expected InvokeSkill for /loop");
        };
        let Some(acp::ContentBlock::Text(tb)) = blocks.into_iter().next() else {
            panic!("expected a text block");
        };
        tb.text
    };
    assert!(
        text_of(LoopFireMode::Detached).contains("cannot see this conversation"),
        "detached sessions must get the standalone-prompt framing"
    );
    assert!(
        text_of(LoopFireMode::InSession).contains("arrives as a new turn in this conversation"),
        "in-session sessions must get the standing-order framing"
    );
}

#[test]
fn resolve_passthrough_preserves_original_blocks() {
    // External-harness agents: blocks are passed through verbatim.
    // The prompt assembly layer decides how to format them.
    let skills = vec![make_skill("commit", true)];
    let outcome = resolve(
        vec![text_block("/commit fix typo")],
        &skills,
        all_gated(),
        SkillSlashRewrite::Passthrough,
        &[],
    )
    .unwrap_err();
    // Original text is preserved in blocks.
    assert_eq!(invoke_text(outcome), "/commit fix typo");

    let outcome = resolve(
        vec![text_block("/commit")],
        &skills,
        all_gated(),
        SkillSlashRewrite::Passthrough,
        &[],
    )
    .unwrap_err();
    assert_eq!(invoke_text(outcome), "/commit");
}

#[test]
fn resolve_passes_through_normal_prompts() {
    let skills = vec![make_skill("commit", true)];
    assert!(
        resolve(
            vec![text_block("fix the login bug")],
            &skills,
            all_gated(),
            SkillSlashRewrite::default(),
            &[],
        )
        .is_ok()
    );
    assert!(
        resolve(
            vec![text_block("/unknown")],
            &skills,
            all_gated(),
            SkillSlashRewrite::default(),
            &[],
        )
        .is_ok()
    );
}

#[test]
fn resolve_filters_non_invocable_skills() {
    let skills = vec![make_skill("internal-only", false)];
    assert!(
        resolve(
            vec![text_block("/internal-only")],
            &skills,
            all_gated(),
            SkillSlashRewrite::default(),
            &[],
        )
        .is_ok()
    );
}

#[test]
fn resolve_builtin_shadows_same_named_skill() {
    let skills = vec![make_skill("compact", true)];
    let outcome = resolve(
        vec![text_block("/compact")],
        &skills,
        all_gated(),
        SkillSlashRewrite::default(),
        &[],
    )
    .unwrap_err();
    assert!(matches!(outcome, SlashCommandOutcome::Builtin(_)));
}

// ── available_commands (ACP) ─────────────────────────────────────

#[test]
fn available_commands_orders_builtins_first() {
    let skills = vec![make_skill("commit", true), make_skill("deploy", true)];
    let commands = available_commands(&skills, all_gated(), &[]);
    let names: Vec<&str> = commands.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "compact",
            "always-approve",
            "flush",
            "dream",
            "memory",
            "context",
            "hooks-trust",
            "hooks-list",
            "hooks-add",
            "hooks-remove",
            "hooks-untrust",
            "plugins",
            "reload-plugins",
            "session-info",
            "feedback",
            "deep-research",
            "workflow",
            "goal",
            "loop",
            "commit",
            "deploy",
        ]
    );
}

fn advertised_names(availability: CommandAvailability) -> Vec<String> {
    available_commands(&[], availability, &[])
        .into_iter()
        .map(|c| c.name)
        .collect()
}

#[test]
fn availability_filters_memory_commands() {
    // memory=false hides /flush and /dream but NOT /memory (gated on
    // memory_configured instead, so the user can re-enable via toggle).
    let names = advertised_names(CommandAvailability {
        memory: false,
        ..CommandAvailability::all_enabled()
    });
    assert!(!names.iter().any(|n| n == "flush"), "got: {names:?}");
    assert!(!names.iter().any(|n| n == "dream"), "got: {names:?}");
    assert!(
        names.iter().any(|n| n == "memory"),
        "/memory should still be available when memory_configured=true, got: {names:?}"
    );
    assert!(names.iter().any(|n| n == "compact"));

    // memory_configured=false hides /memory too.
    let names2 = advertised_names(CommandAvailability {
        memory: false,
        memory_configured: false,
        ..CommandAvailability::all_enabled()
    });
    assert!(
        !names2.iter().any(|n| n == "memory"),
        "/memory should be hidden when memory_configured=false, got: {names2:?}"
    );
}

#[test]
fn availability_filters_loop_command() {
    let names = advertised_names(CommandAvailability {
        scheduler: false,
        ..CommandAvailability::all_enabled()
    });
    assert!(!names.iter().any(|n| n == "loop"), "got: {names:?}");
}

#[test]
fn workflows_gate_hides_workflow_but_not_goal() {
    let names = advertised_names(CommandAvailability {
        workflows: false,
        workflow_management: false,
        ..CommandAvailability::all_enabled()
    });
    assert!(!names.iter().any(|n| n == "workflow"), "got: {names:?}");
    assert!(names.iter().any(|n| n == "goal"), "got: {names:?}");

    let names2 = advertised_names(CommandAvailability {
        goal: false,
        ..CommandAvailability::all_enabled()
    });
    assert!(!names2.iter().any(|n| n == "goal"), "got: {names2:?}");
    assert!(names2.iter().any(|n| n == "workflow"), "got: {names2:?}");
}

#[test]
fn availability_filters_hooks_and_plugins() {
    let names = advertised_names(CommandAvailability {
        hooks: false,
        plugins: false,
        ..CommandAvailability::all_enabled()
    });
    for n in [
        "hooks-trust",
        "hooks-list",
        "hooks-add",
        "hooks-remove",
        "hooks-untrust",
        "plugins",
        "reload-plugins",
    ] {
        assert!(
            !names.iter().any(|x| x == n),
            "{n} should be hidden, got: {names:?}",
        );
    }
}

#[test]
fn availability_filters_goal_command() {
    let names = advertised_names(CommandAvailability {
        goal: false,
        ..CommandAvailability::all_enabled()
    });
    assert!(!names.iter().any(|n| n == "goal"), "got: {names:?}");
}

#[test]
fn goal_does_not_resolve_when_host_capability_is_off() {
    let availability = CommandAvailability {
        goal: false,
        ..CommandAvailability::all_enabled()
    };
    assert!(
        resolve(
            vec![text_block("/goal status")],
            &[],
            availability,
            SkillSlashRewrite::default(),
            &[],
        )
        .is_ok(),
        "expected pass-through (Ok), got an outcome",
    );
}

#[test]
fn loop_does_not_resolve_when_scheduler_unavailable() {
    // Without the scheduler gate the shell should not route /loop --
    // it would otherwise produce a useless "call scheduler_create"
    // prompt the model can't act on.
    let availability = CommandAvailability {
        scheduler: false,
        ..CommandAvailability::all_enabled()
    };
    assert!(
        resolve(
            vec![text_block("/loop 5m do thing")],
            &[],
            availability,
            SkillSlashRewrite::default(),
            &[],
        )
        .is_ok(),
        "expected pass-through (Ok), got an outcome",
    );
}

/// Extract the text of the first block produced by `build_loop_prompt_blocks`.
fn loop_text(args: &str, mode: LoopFireMode) -> String {
    match build_loop_prompt_blocks(args, mode).into_iter().next() {
        Some(acp::ContentBlock::Text(t)) => t.text,
        other => panic!("expected a text block, got {other:?}"),
    }
}

#[test]
fn loop_usage_has_no_10m_default() {
    // The shell client must not advertise a silent 10m default.
    let usage = loop_text("", LoopFireMode::Detached);
    assert!(usage.contains("Usage: /loop"), "got: {usage}");
    assert!(
        !usage.contains("10m"),
        "usage must not claim a default: {usage}"
    );
}

#[test]
fn loop_instruction_derives_interval_without_default_or_inline_execute() {
    let instr = loop_text("every 30 minutes do x", LoopFireMode::Detached);
    assert!(
        !instr.contains("10m"),
        "instruction must not default: {instr}"
    );
    assert!(instr.contains("30 minutes"));
    assert!(instr.contains("<number><unit>"));
    assert!(instr.contains("ask the user how often"));
    assert!(instr.contains("Do NOT execute the prompt inline"));
    assert!(
        !instr.contains("immediately execute the parsed prompt"),
        "stale inline-execute wording must be gone: {instr}"
    );
    assert!(instr.contains("every 30 minutes do x"));
}

#[test]
fn loop_prompt_matches_pager_wording() {
    // The shell and pager must stay textually identical so they don't drift.
    use echo_agent_tools::implementations::echo_agent_build::{
        loop_schedule_instruction, loop_usage_message,
    };
    assert_eq!(loop_text("", LoopFireMode::Detached), loop_usage_message());
    for mode in [LoopFireMode::Detached, LoopFireMode::InSession] {
        assert_eq!(
            loop_text("2h run tests", mode),
            loop_schedule_instruction("2h run tests", mode)
        );
    }
}

#[test]
fn build_tools_meta_serialises_tool_names() {
    let names = vec!["scheduler_create".to_string(), "image_gen".to_string()];
    let v = build_tools_meta(&names);
    assert_eq!(
        serde_json::Value::Object(v),
        serde_json::json!({"tools": ["scheduler_create", "image_gen"]})
    );
}

#[test]
fn pre_session_builtin_commands_excludes_gated_entries() {
    // The pre-session list (advertised in InitializeResponse._meta)
    // with a default (fail-closed) availability must not include any
    // gated command -- we don't know the toolset yet at that point.
    let names: Vec<String> = builtin_commands(CommandAvailability::default())
        .into_iter()
        .map(|c| c.name)
        .collect();
    for forbidden in [
        "flush",
        "dream",
        "memory",
        "feedback",
        "goal",
        "hooks-list",
        "plugins",
        "reload-plugins",
    ] {
        assert!(
            !names.iter().any(|n| n == forbidden),
            "{forbidden} should be excluded pre-session, got: {names:?}",
        );
    }
    // Always-on commands are still present.
    for required in ["compact", "always-approve", "context", "session-info"] {
        assert!(
            names.iter().any(|n| n == required),
            "{required} should be present, got: {names:?}",
        );
    }
}

#[test]
fn pre_session_builtin_commands_advertises_goal_when_flag_enabled() {
    // `/goal` is gated on a config feature flag known at initialize
    // time (not a live toolset), so when the pre-session availability
    // enables it the command must be advertised -- otherwise it would
    // only show up after the first user turn created a session.
    let availability = CommandAvailability {
        goal: true,
        ..CommandAvailability::default()
    };
    let names: Vec<String> = builtin_commands(availability)
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert!(
        names.iter().any(|n| n == "goal"),
        "goal should be advertised pre-session when the flag is on, got: {names:?}",
    );
    // Runtime/tool-dependent gates stay closed pre-session.
    for forbidden in ["flush", "dream", "memory", "feedback", "plugins"] {
        assert!(
            !names.iter().any(|n| n == forbidden),
            "{forbidden} should stay excluded pre-session, got: {names:?}",
        );
    }
}

#[test]
fn available_commands_populates_acp_fields() {
    let skills = vec![make_skill("commit", true)];
    let commands = available_commands(&skills, all_gated(), &[]);

    let builtin = commands.iter().find(|c| c.name == "compact").unwrap();
    assert!(builtin.input.is_some());

    let flush = commands.iter().find(|c| c.name == "flush").unwrap();
    assert!(flush.input.is_none()); // no argument_hint

    let skill = commands.iter().find(|c| c.name == "commit").unwrap();
    assert_eq!(skill.description, "Short: commit");
    let meta = skill.meta.as_ref().expect("skill meta");
    assert_eq!(meta.get("scope").and_then(|v| v.as_str()), Some("local"));
    assert!(meta.get("path").and_then(|v| v.as_str()).is_some());
    assert_eq!(
        meta.get("qualifiedName").and_then(|v| v.as_str()),
        Some("local:commit")
    );
    assert!(meta.get("pluginName").is_none());
}

#[test]
fn pager_blocked_shell_command_skill_is_advertised_qualified() {
    let skills = vec![make_skill("hooks-add", true)];
    let commands = available_commands(&skills, CommandAvailability::default(), &[]);
    assert!(
        !commands.iter().any(|c| c.name == "hooks-add"),
        "pager-blocked name must not be advertised bare"
    );
    assert!(
        commands.iter().any(|c| c.name == "local:hooks-add"),
        "skill must stay reachable as /local:hooks-add, got {:?}",
        commands.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
    );
}

#[test]
fn plugin_skill_colliding_with_pager_builtin_is_advertised_qualified() {
    let mut skill = make_scoped_skill("login", SkillScope::Plugin);
    skill.plugin_name = Some("acme".into());
    let commands = available_commands(&[skill], all_gated(), &[]);

    assert!(
        !commands.iter().any(|c| c.name == "login"),
        "colliding skill must not take the bare name (pager owns /login)"
    );
    let cmd = commands
        .iter()
        .find(|c| c.name == "acme:login")
        .expect("plugin skill stays reachable as /acme:login");
    let meta = cmd.meta.as_ref().expect("skill meta");
    assert_eq!(meta.get("scope").and_then(|v| v.as_str()), Some("plugin"));
    assert_eq!(meta.get("bareName").and_then(|v| v.as_str()), Some("login"));
    assert_eq!(
        meta.get("pluginName").and_then(|v| v.as_str()),
        Some("acme")
    );
    assert_eq!(
        meta.get("qualifiedName").and_then(|v| v.as_str()),
        Some("acme:login")
    );
}

// ── /flush ─────────────────────────────────────────────────────

#[test]
fn flush_resolves_to_builtin_action() {
    assert!(matches!(
        resolve_builtin("flush", ""),
        Some(BuiltinAction::FlushMemory)
    ));
    // Args are ignored — still resolves to FlushMemory
    assert!(matches!(
        resolve_builtin("flush", "some extra args"),
        Some(BuiltinAction::FlushMemory)
    ));
}

#[test]
fn resolve_routes_flush_builtin() {
    let outcome = resolve(
        vec![text_block("/flush")],
        &[],
        all_gated(),
        SkillSlashRewrite::default(),
        &[],
    )
    .unwrap_err();
    assert!(matches!(
        outcome,
        SlashCommandOutcome::Builtin(BuiltinAction::FlushMemory)
    ));
}

#[test]
fn flush_builtin_shadows_same_named_skill() {
    let skills = vec![make_skill("flush", true)];
    let outcome = resolve(
        vec![text_block("/flush")],
        &skills,
        all_gated(),
        SkillSlashRewrite::default(),
        &[],
    )
    .unwrap_err();
    assert!(matches!(outcome, SlashCommandOutcome::Builtin(_)));
}

// ── /dream ─────────────────────────────────────────────────────

#[test]
fn dream_resolves_to_builtin_action() {
    assert!(matches!(
        resolve_builtin("dream", ""),
        Some(BuiltinAction::Dream)
    ));
    assert!(matches!(
        resolve_builtin("dream", "extra args"),
        Some(BuiltinAction::Dream)
    ));
}

#[test]
fn resolve_routes_dream_builtin() {
    let outcome = resolve(
        vec![text_block("/dream")],
        &[],
        all_gated(),
        SkillSlashRewrite::default(),
        &[],
    )
    .unwrap_err();
    assert!(matches!(
        outcome,
        SlashCommandOutcome::Builtin(BuiltinAction::Dream)
    ));
}

#[test]
fn dream_builtin_shadows_same_named_skill() {
    let skills = vec![make_skill("dream", true)];
    let outcome = resolve(
        vec![text_block("/dream")],
        &skills,
        all_gated(),
        SkillSlashRewrite::default(),
        &[],
    )
    .unwrap_err();
    assert!(matches!(outcome, SlashCommandOutcome::Builtin(_)));
}

// ── ambiguous skill names ─────────────────────────────────────

fn make_scoped_skill(name: &str, scope: SkillScope) -> SkillInfo {
    SkillInfo {
        name: name.to_string(),
        display_name: None,
        description: format!("A {scope:?} skill called {name}"),
        when_to_use: None,
        short_description: Some(format!("Short: {name}")),
        author: None,
        argument_hint: None,
        path: format!("/path/to/{name}/{scope:?}/SKILL.md"),
        scope,
        config_source: None,
        plugin_name: None,
        plugin_version: None,
        plugin_root: None,
        plugin_data: None,
        allowed_tools: None,
        license: None,
        compatibility: None,
        metadata: None,
        model: None,
        effort: None,
        user_invocable: true,
        disable_model_invocation: false,
        has_user_specified_description: false,
        paths: None,
        enabled: true,
        body: None,
    }
}

#[test]
fn resolve_ambiguous_bare_name_passes_through() {
    // Two skills share the bare name "commit" in different scopes.
    let skills = vec![
        make_scoped_skill("commit", SkillScope::Local),
        make_scoped_skill("commit", SkillScope::User),
    ];
    // Bare "/commit" is ambiguous -- should pass through (not first-match).
    assert!(
        resolve(
            vec![text_block("/commit")],
            &skills,
            all_gated(),
            SkillSlashRewrite::default(),
            &[],
        )
        .is_ok()
    );
}

#[test]
fn resolve_qualified_skill_name() {
    let skills = vec![
        make_scoped_skill("commit", SkillScope::Local),
        make_scoped_skill("commit", SkillScope::User),
    ];

    // Qualified "/local:commit" resolves unambiguously.
    let outcome = resolve(
        vec![text_block("/local:commit fix typo")],
        &skills,
        all_gated(),
        SkillSlashRewrite::default(),
        &[],
    )
    .unwrap_err();
    let skill = first_skill(outcome);
    assert_eq!(skill.name, "local:commit");
    assert_eq!(skill.args, "fix typo");

    // Qualified "/user:commit" resolves unambiguously.
    let outcome = resolve(
        vec![text_block("/user:commit")],
        &skills,
        all_gated(),
        SkillSlashRewrite::default(),
        &[],
    )
    .unwrap_err();
    let skill = first_skill(outcome);
    assert_eq!(skill.name, "user:commit");
    assert_eq!(skill.args, "");
}

#[test]
fn resolve_accepts_qualified_form_of_bare_advertised_skill() {
    let skills = vec![make_scoped_skill("deploy", SkillScope::Local)];
    // Advertised bare (no collision, no duplicate).
    let names: Vec<String> = available_commands(&skills, all_gated(), &[])
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert!(names.iter().any(|n| n == "deploy"));

    let outcome = resolve(
        vec![text_block("/local:deploy to staging")],
        &skills,
        all_gated(),
        SkillSlashRewrite::default(),
        &[],
    )
    .unwrap_err();
    let skill = first_skill(outcome);
    assert_eq!(skill.name, "local:deploy");
    assert_eq!(skill.args, "to staging");

    assert!(
        parse_skill_references("see /local:deploy for how we ship", &skills, all_gated()).is_none(),
        "unadvertised qualified spelling must not match mid-prose"
    );
}

#[test]
fn available_commands_uses_qualified_names_for_duplicates() {
    let skills = vec![
        make_scoped_skill("commit", SkillScope::Local),
        make_scoped_skill("commit", SkillScope::User),
        make_skill("deploy", true),
    ];
    let commands = available_commands(&skills, all_gated(), &[]);
    let names: Vec<&str> = commands.iter().map(|c| c.name.as_str()).collect();
    // Duplicate "commit" skills should use qualified names.
    assert!(names.contains(&"local:commit"));
    assert!(names.contains(&"user:commit"));
    // Unique "deploy" keeps bare name only (no duplicate qualified form).
    assert!(names.contains(&"deploy"));
    assert!(
        !names.contains(&"local:deploy"),
        "non-colliding skill should NOT get a qualified duplicate, got: {names:?}"
    );
    // Bare "commit" should NOT appear.
    assert!(!names.contains(&"commit"));
}

#[test]
fn available_commands_qualifies_builtin_colliding_skill() {
    let skills = vec![
        make_scoped_skill("compact", SkillScope::Local),
        make_skill("deploy", true),
    ];
    let commands = available_commands(&skills, all_gated(), &[]);
    let names: Vec<&str> = commands.iter().map(|c| c.name.as_str()).collect();
    assert!(
        names.contains(&"local:compact"),
        "builtin-colliding skill should use qualified name, got: {names:?}"
    );
    let compact_cmd = commands.iter().find(|c| c.name == "compact").unwrap();
    assert!(
        compact_cmd.meta.is_none(),
        "bare 'compact' should be the builtin (no meta)"
    );
    assert!(names.contains(&"deploy"));
}

#[test]
fn resolve_qualified_builtin_colliding_skill() {
    let skills = vec![make_scoped_skill("compact", SkillScope::Local)];

    let outcome = resolve(
        vec![text_block("/compact")],
        &skills,
        all_gated(),
        SkillSlashRewrite::default(),
        &[],
    )
    .unwrap_err();
    assert!(matches!(outcome, SlashCommandOutcome::Builtin(_)));

    let outcome = resolve(
        vec![text_block("/local:compact")],
        &skills,
        all_gated(),
        SkillSlashRewrite::default(),
        &[],
    )
    .unwrap_err();
    let skill = first_skill(outcome);
    assert_eq!(skill.name, "local:compact");
    assert_eq!(skill.args, "");
}

fn make_plugin_skill(name: &str, plugin: &str) -> SkillInfo {
    let mut skill = make_scoped_skill(name, SkillScope::Plugin);
    skill.plugin_name = Some(plugin.to_string());
    skill.path = format!("/plugins/{plugin}/skills/{name}/SKILL.md");
    skill
}

#[test]
fn plugin_login_skill_resolves_by_qualified_name_only() {
    let skills = vec![make_plugin_skill("login", "acme")];

    assert!(
        resolve(
            vec![text_block("/login")],
            &skills,
            all_gated(),
            SkillSlashRewrite::default(),
            &[],
        )
        .is_ok()
    );

    let outcome = resolve(
        vec![text_block("/acme:login now")],
        &skills,
        all_gated(),
        SkillSlashRewrite::default(),
        &[],
    )
    .unwrap_err();
    let skill = first_skill(outcome);
    assert_eq!(skill.name, "acme:login");
    assert_eq!(skill.args, "now");
    assert_eq!(skill.plugin_name.as_deref(), Some("acme"));
}

#[test]
fn inspect_reserved_names_exclude_gated_shell_builtins() {
    assert!(super::is_reserved_slash_name("login"));
    assert!(super::is_reserved_slash_name("Login"));
    assert!(super::is_reserved_slash_name("delete"));
    assert!(super::is_reserved_slash_name("compact"));
    assert!(super::is_reserved_slash_name("hooks-add"));
    assert!(super::is_reserved_slash_name("HOOKS-ADD"));
    assert!(!super::is_reserved_slash_name("flush"));
    assert!(!super::is_reserved_slash_name("deploy"));
}

#[test]
fn mixed_case_pager_collision_is_advertised_qualified_lowercase() {
    let skills = vec![make_scoped_skill("Login", SkillScope::Local)];
    let commands = available_commands(&skills, all_gated(), &[]);
    let names: Vec<&str> = commands.iter().map(|c| c.name.as_str()).collect();
    assert!(
        !names.contains(&"Login") && !names.contains(&"login"),
        "mixed-case colliding skill must not take the bare name, got {names:?}"
    );
    let cmd = commands
        .iter()
        .find(|c| c.name == "local:login")
        .expect("pager folds ACP names; advertised form must be lowercase qualified");
    let meta = cmd.meta.as_ref().expect("skill meta");
    assert_eq!(meta.get("bareName").and_then(|v| v.as_str()), Some("Login"));
    assert_eq!(
        meta.get("qualifiedName").and_then(|v| v.as_str()),
        Some("local:login")
    );
}

#[test]
fn mixed_case_unique_skill_advertises_lowercase_bare_name() {
    let names: Vec<String> = available_commands(
        &[make_scoped_skill("Deploy", SkillScope::Local)],
        all_gated(),
        &[],
    )
    .into_iter()
    .map(|c| c.name)
    .collect();
    assert!(names.iter().any(|n| n == "deploy"), "{names:?}");
    assert!(!names.iter().any(|n| n == "Deploy"), "{names:?}");
}

#[test]
fn resolve_mixed_case_skill_invocation() {
    let skills = vec![make_scoped_skill("Deploy", SkillScope::Local)];
    for typed in ["/deploy to prod", "/Deploy to prod", "/DEPLOY to prod"] {
        let outcome = resolve(
            vec![text_block(typed)],
            &skills,
            all_gated(),
            SkillSlashRewrite::default(),
            &[],
        )
        .unwrap_err();
        let skill = first_skill(outcome);
        assert_eq!(skill.qualified_name, "local:Deploy", "{typed}");
        assert_eq!(skill.args, "to prod", "{typed}");
    }
}

#[test]
fn resolve_mixed_case_builtin() {
    let outcome = resolve(
        vec![text_block("/Compact keep auth")],
        &[],
        all_gated(),
        SkillSlashRewrite::default(),
        &[],
    )
    .unwrap_err();
    assert!(matches!(
        outcome,
        SlashCommandOutcome::Builtin(BuiltinAction::Compact {
            user_context: Some(ref ctx)
        }) if ctx == "keep auth"
    ));
}

#[test]
fn same_bare_name_differing_only_by_case_qualifies_both() {
    let skills = vec![
        make_scoped_skill("Commit", SkillScope::Local),
        make_scoped_skill("commit", SkillScope::User),
    ];
    let names: Vec<String> = available_commands(&skills, all_gated(), &[])
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert!(names.iter().any(|n| n == "local:commit"), "{names:?}");
    assert!(names.iter().any(|n| n == "user:commit"), "{names:?}");
    assert!(!names.iter().any(|n| n == "commit"));
    assert!(!names.iter().any(|n| n == "Commit"));
}

#[test]
fn same_qualified_name_differing_only_by_case_is_withheld() {
    let skills = vec![
        make_scoped_skill("Commit", SkillScope::Local),
        make_scoped_skill("commit", SkillScope::Local),
    ];
    let names: Vec<String> = available_commands(&skills, all_gated(), &[])
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert!(
        names
            .iter()
            .all(|name| name != "local:commit" && name != "local:Commit"),
        "got {names:?}"
    );
}

#[test]
fn mixed_case_workflow_does_not_take_reserved_name() {
    let workflows = vec![listing("Login"), listing("Review")];
    let names: Vec<String> = available_commands(&[], all_gated(), &workflows)
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert!(
        !names.iter().any(|n| n.eq_ignore_ascii_case("login")),
        "{names:?}"
    );
    assert!(names.iter().any(|n| n == "Review"), "{names:?}");
}

#[test]
fn resolve_mixed_case_workflow_launch_keeps_listing_name() {
    let workflows = vec![listing("Triage-Flakes")];
    match resolve(
        vec![text_block("/triage-flakes now")],
        &[],
        all_gated(),
        SkillSlashRewrite::default(),
        &workflows,
    )
    .unwrap_err()
    {
        SlashCommandOutcome::Builtin(BuiltinAction::WorkflowLaunch { name, input }) => {
            assert_eq!(name, "Triage-Flakes");
            assert_eq!(input, "now");
        }
        other => panic!("expected WorkflowLaunch, got {other:?}"),
    }
}

#[test]
fn feedback_does_not_resolve_when_disabled() {
    // /feedback should pass through as unrecognized when the feature is off.
    assert!(
        resolve(
            vec![text_block("/feedback hello")],
            &[],
            CommandAvailability::default(),
            SkillSlashRewrite::default(),
            &[],
        )
        .is_ok()
    );
}

#[test]
fn feedback_resolves_when_enabled() {
    let outcome = resolve(
        vec![text_block("/feedback hello")],
        &[],
        all_gated(),
        SkillSlashRewrite::default(),
        &[],
    )
    .unwrap_err();
    assert!(matches!(
        outcome,
        SlashCommandOutcome::Builtin(BuiltinAction::Feedback { ref text }) if text == "hello"
    ));
}

/// Collect the advertised command names for the given availability.
fn advertised_names_with(availability: CommandAvailability) -> Vec<String> {
    available_commands(&[], availability, &[])
        .into_iter()
        .map(|c| c.name)
        .collect()
}

/// `CommandAvailability::default()` must be fail-closed: every gated
/// command is hidden, only `BuiltinGate::AlwaysOn` survives. The
/// pre-session `MvpAgent::command_availability()` builds on this value
/// (only flipping config-derived gates like `goal` on), so a
/// regression here would re-expose `/flush`, `/loop`, etc. on the home
/// screen for harnesses that won't actually run them.
#[test]
fn default_availability_is_fail_closed_on_every_gate() {
    let names = advertised_names_with(CommandAvailability::default());
    for forbidden in [
        "flush",
        "dream",
        "feedback",
        "goal",
        "loop",
        "hooks-list",
        "hooks-trust",
        "hooks-untrust",
        "hooks-add",
        "hooks-remove",
        "plugins",
        "reload-plugins",
    ] {
        assert!(
            !names.iter().any(|n| n == forbidden),
            "{forbidden} must not be advertised under default fail-closed availability, got: {names:?}",
        );
    }
    for required in ["compact", "always-approve", "context", "session-info"] {
        assert!(
            names.iter().any(|n| n == required),
            "AlwaysOn {required} must always be advertised, got: {names:?}",
        );
    }
}

/// `/flush` is a memory-write that's only useful when the model can
/// later read back what it wrote. The shell's
/// `build_command_availability()` ANDs `memory.is_enabled()` with
/// `memory_search`/`memory_get` registration; the gate itself just
/// reads `availability.memory`. Lock both halves so a future change
/// to either side is forced through this test.
#[test]
fn flush_hidden_when_memory_gate_off_visible_when_on() {
    let off = advertised_names_with(CommandAvailability::default());
    assert!(!off.iter().any(|n| n == "flush"), "got: {off:?}");
    assert!(!off.iter().any(|n| n == "dream"), "got: {off:?}");
    // /memory is gated on memory_configured, not memory — hidden here
    // because Default sets both to false.
    assert!(!off.iter().any(|n| n == "memory"), "got: {off:?}");

    let on = advertised_names_with(CommandAvailability {
        memory: true,
        memory_configured: true,
        ..CommandAvailability::default()
    });
    assert!(on.iter().any(|n| n == "flush"), "got: {on:?}");
    assert!(on.iter().any(|n| n == "dream"), "got: {on:?}");
    assert!(on.iter().any(|n| n == "memory"), "got: {on:?}");
}

// ── /memory ─────────────────────────────────────────────────────

#[test]
fn memory_bare_resolves_to_browse() {
    assert!(matches!(
        resolve_builtin("memory", ""),
        Some(BuiltinAction::MemoryBrowse)
    ));
    // Any unrecognized arg also falls through to browse
    assert!(matches!(
        resolve_builtin("memory", "status"),
        Some(BuiltinAction::MemoryBrowse)
    ));
}

#[test]
fn memory_on_off_resolves_to_toggle() {
    for (arg, expected) in [
        ("on", true),
        ("enable", true),
        ("ON", true),
        ("Enable", true),
        ("off", false),
        ("disable", false),
        ("OFF", false),
        ("Disable", false),
    ] {
        assert!(
            matches!(
                resolve_builtin("memory", arg),
                Some(BuiltinAction::MemoryToggle { enabled }) if enabled == expected
            ),
            "expected toggle({expected}) for {arg:?}",
        );
    }
}

#[test]
fn mem_alias_resolves_to_memory_browse() {
    let outcome = resolve(
        vec![text_block("/mem")],
        &[],
        all_gated(),
        SkillSlashRewrite::default(),
        &[],
    )
    .unwrap_err();
    assert!(matches!(
        outcome,
        SlashCommandOutcome::Builtin(BuiltinAction::MemoryBrowse)
    ));
}

#[test]
fn mem_alias_resolves_toggle_with_args() {
    let outcome = resolve(
        vec![text_block("/mem off")],
        &[],
        all_gated(),
        SkillSlashRewrite::default(),
        &[],
    )
    .unwrap_err();
    assert!(matches!(
        outcome,
        SlashCommandOutcome::Builtin(BuiltinAction::MemoryToggle { enabled: false })
    ));
}

#[test]
fn memory_resolves_when_disabled_but_configured() {
    // memory=false but memory_configured=true: /memory must still work
    // so the user can re-enable via the toggle.
    let availability = CommandAvailability {
        memory: false,
        ..CommandAvailability::all_enabled()
    };
    let outcome = resolve(
        vec![text_block("/memory")],
        &[],
        availability,
        SkillSlashRewrite::default(),
        &[],
    );
    assert!(
        outcome.is_err(),
        "expected /memory to resolve when memory_configured=true",
    );
}

#[test]
fn memory_not_resolved_when_not_configured() {
    let availability = CommandAvailability {
        memory: false,
        memory_configured: false,
        ..CommandAvailability::all_enabled()
    };
    assert!(
        resolve(
            vec![text_block("/memory")],
            &[],
            availability,
            SkillSlashRewrite::default(),
            &[],
        )
        .is_ok(),
        "expected pass-through (Ok) when memory_configured is false",
    );
}

// ── parse_skill_references ──────────────────────────────────────

#[test]
fn parse_skill_refs_single_skill() {
    let skills = vec![make_skill("commit", true)];
    let refs = parse_skill_references("/commit fix typo", &skills, all_gated()).unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].name, "commit");
    assert_eq!(refs[0].args, "fix typo");
}

#[test]
fn parse_skill_refs_single_no_args() {
    let skills = vec![make_skill("commit", true)];
    let refs = parse_skill_references("/commit", &skills, all_gated()).unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].name, "commit");
    assert_eq!(refs[0].args, "");
}

#[test]
fn parse_skill_refs_multi_skill() {
    let skills = vec![make_skill("review", true), make_skill("lint", true)];
    let refs =
        parse_skill_references("/review fix auth /lint --strict", &skills, all_gated()).unwrap();
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[0].name, "review");
    assert_eq!(refs[0].args, "fix auth");
    assert_eq!(refs[1].name, "lint");
    assert_eq!(refs[1].args, "--strict");
}

#[test]
fn parse_skill_refs_ignores_unknown_slash() {
    let skills = vec![make_skill("commit", true)];
    // /api/v2/users is not a known skill — should be ignored.
    let result = parse_skill_references("check /api/v2/users", &skills, all_gated());
    assert!(result.is_none());
}

#[test]
fn parse_skill_refs_ignores_builtins() {
    let skills = vec![make_skill("commit", true)];
    // /compact is a builtin — should NOT appear in skill refs.
    let result = parse_skill_references("/compact", &skills, all_gated());
    assert!(result.is_none());
}

#[test]
fn parse_skill_refs_empty_text() {
    let skills = vec![make_skill("commit", true)];
    assert!(parse_skill_references("", &skills, all_gated()).is_none());
}

#[test]
fn parse_skill_refs_no_slash() {
    let skills = vec![make_skill("commit", true)];
    assert!(parse_skill_references("just some text", &skills, all_gated()).is_none());
}

#[test]
fn parse_skill_refs_non_invocable_skill_ignored() {
    let skills = vec![make_skill("internal-only", false)];
    assert!(parse_skill_references("/internal-only", &skills, all_gated()).is_none());
}

#[test]
fn parse_skill_refs_qualified_name() {
    let skills = vec![
        make_scoped_skill("commit", SkillScope::Local),
        make_scoped_skill("commit", SkillScope::User),
    ];
    let refs = parse_skill_references("/local:commit fix typo", &skills, all_gated()).unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].name, "local:commit");
    assert_eq!(refs[0].args, "fix typo");
    assert_eq!(refs[0].qualified_name, "local:commit");
}

#[test]
fn parse_skill_refs_text_before_first_skill() {
    // Text before the first skill reference is part of user query,
    // not consumed as args.
    let skills = vec![make_skill("commit", true)];
    let refs = parse_skill_references("please do /commit fix typo", &skills, all_gated()).unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].name, "commit");
    assert_eq!(refs[0].args, "fix typo");
}

// ── /goal command resolution ─────────────────────────────────

fn resolve_goal(args: &str) -> BuiltinAction {
    let blocks = vec![text_block(&format!("/goal {args}"))];
    match resolve(blocks, &[], all_gated(), SkillSlashRewrite::default(), &[]).unwrap_err() {
        SlashCommandOutcome::Builtin(action) => action,
        _ => panic!("expected Builtin outcome"),
    }
}

#[test]
fn goal_empty_resolves_to_status() {
    assert!(matches!(resolve_goal(""), BuiltinAction::GoalStatus));
}

fn listing(name: &str) -> crate::session::workflow::registry::WorkflowListing {
    crate::session::workflow::registry::WorkflowListing {
        name: name.to_string(),
        description: "does things".to_string(),
        when_to_use: None,
        source: "project",
        path: Some(format!(".echo-agent/workflows/{name}.rhai")),
    }
}

#[test]
fn same_named_builtin_projects_workflow_metadata_without_replacing_command() {
    let workflow = crate::session::workflow::registry::WorkflowListing {
        name: "deep-research".to_string(),
        description: "Workflow metadata description".to_string(),
        when_to_use: None,
        source: "builtin",
        path: None,
    };
    let commands = available_commands(&[], all_gated(), std::slice::from_ref(&workflow));
    let matching: Vec<_> = commands
        .iter()
        .filter(|command| command.name == "deep-research")
        .collect();
    assert_eq!(matching.len(), 1);
    let command = matching[0];
    assert_eq!(
        command.description,
        "Research with bounded parallel agents, cross-check evidence, and write a cited report"
    );
    assert_eq!(
        command.input,
        Some(acp::AvailableCommandInput::Unstructured(
            acp::UnstructuredCommandInput::new("<query>".to_string())
        ))
    );
    let meta = command.meta.as_ref().expect("workflow metadata");
    assert_eq!(
        meta.get("workflowSource")
            .and_then(serde_json::Value::as_str),
        Some("builtin")
    );
    assert!(!meta.contains_key("workflowPath"));

    assert!(matches!(
        resolve(
            vec![text_block("/deep-research rust pitfalls")],
            &[],
            all_gated(),
            SkillSlashRewrite::default(),
            std::slice::from_ref(&workflow),
        )
        .unwrap_err(),
        SlashCommandOutcome::Builtin(BuiltinAction::DeepResearch { query })
            if query == "rust pitfalls"
    ));
}

#[test]
fn ordinary_builtin_collisions_do_not_project_workflow_metadata() {
    let mut status_workflow = listing("status");
    status_workflow.source = "project";
    let mut goal_workflow = listing("goal");
    goal_workflow.source = "user";
    let commands = available_commands(&[], all_gated(), &[status_workflow, goal_workflow]);

    assert_eq!(
        commands
            .iter()
            .filter(|command| command.name == "session-info")
            .count(),
        1
    );
    assert_eq!(
        commands
            .iter()
            .filter(|command| command.name == "goal")
            .count(),
        1
    );
    assert!(commands.iter().all(|command| command.name != "status"));
    for name in ["session-info", "goal"] {
        let command = commands
            .iter()
            .find(|command| command.name == name)
            .expect("builtin command");
        assert!(
            command
                .meta
                .as_ref()
                .is_none_or(|meta| !meta.contains_key("workflowSource")),
            "{name} must not expose a colliding saved workflow"
        );
    }
}

#[test]
fn named_workflows_advertise_and_resolve() {
    let workflows = vec![listing("triage-flakes"), listing("goal")];
    let commands = available_commands(&[], all_gated(), &workflows);
    let names: Vec<&str> = commands.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"triage-flakes"), "{names:?}");
    assert_eq!(names.iter().filter(|n| **n == "goal").count(), 1);
    let wf = commands.iter().find(|c| c.name == "triage-flakes").unwrap();
    assert!(
        wf.description.starts_with("Workflow:"),
        "{}",
        wf.description
    );

    let blocks = vec![text_block("/triage-flakes fix the CI")];
    match resolve(
        blocks,
        &[],
        all_gated(),
        SkillSlashRewrite::default(),
        &workflows,
    )
    .unwrap_err()
    {
        SlashCommandOutcome::Builtin(BuiltinAction::WorkflowLaunch { name, input }) => {
            assert_eq!(name, "triage-flakes");
            assert_eq!(input, "fix the CI");
        }
        other => panic!("expected WorkflowLaunch, got {other:?}"),
    }

    let blocks = vec![text_block("/goal status")];
    assert!(matches!(
        resolve(
            blocks,
            &[],
            all_gated(),
            SkillSlashRewrite::default(),
            &workflows
        )
        .unwrap_err(),
        SlashCommandOutcome::Builtin(BuiltinAction::GoalStatus)
    ));
}

#[test]
fn workflow_collision_policy_includes_aliases_and_ambiguous_skills() {
    let skills = vec![
        make_scoped_skill("commit", SkillScope::Local),
        make_scoped_skill("commit", SkillScope::User),
    ];
    let workflows = vec![
        listing("status"),
        listing("yolo"),
        listing("sessions"),
        listing("commit"),
        listing("review"),
    ];
    let names: Vec<_> = available_commands(&skills, all_gated(), &workflows)
        .into_iter()
        .map(|command| command.name)
        .collect();
    assert!(!names.iter().any(|name| name == "status"));
    assert!(!names.iter().any(|name| name == "yolo"));
    assert!(!names.iter().any(|name| name == "sessions"));
    assert!(!names.iter().any(|name| name == "commit"));
    assert!(names.iter().any(|name| name == "local:commit"));
    assert!(names.iter().any(|name| name == "user:commit"));
    assert!(names.iter().any(|name| name == "review"));

    assert!(matches!(
        resolve(
            vec![text_block("/status")],
            &skills,
            all_gated(),
            SkillSlashRewrite::default(),
            &workflows,
        )
        .unwrap_err(),
        SlashCommandOutcome::Builtin(BuiltinAction::SessionInfo)
    ));
    for unavailable in ["sessions", "commit"] {
        assert!(
            resolve(
                vec![text_block(&format!("/{unavailable}"))],
                &skills,
                all_gated(),
                SkillSlashRewrite::default(),
                &workflows,
            )
            .is_ok()
        );
    }
}

#[test]
fn duplicate_qualified_skills_are_omitted_and_do_not_first_match() {
    let mut first = make_scoped_skill("commit", SkillScope::Plugin);
    first.plugin_name = Some("same-plugin".into());
    let mut second = first.clone();
    second.path = "/other/commit/SKILL.md".into();
    let skills = vec![first, second];
    assert!(
        available_commands(&skills, all_gated(), &[])
            .iter()
            .all(|command| command.name != "same-plugin:commit")
    );
    assert!(
        resolve(
            vec![text_block("/same-plugin:commit")],
            &skills,
            all_gated(),
            SkillSlashRewrite::default(),
            &[],
        )
        .is_ok()
    );
}

#[test]
fn existing_runs_keep_management_but_hide_launch_catalog() {
    let availability = CommandAvailability {
        workflows: false,
        workflow_management: true,
        ..CommandAvailability::all_enabled()
    };
    let workflows = vec![listing("review")];
    let names: Vec<_> = available_commands(&[], availability, &workflows)
        .into_iter()
        .map(|command| command.name)
        .collect();
    assert!(names.iter().any(|name| name == "workflow"));
    assert!(!names.iter().any(|name| name == "review"));
    assert!(!names.iter().any(|name| name == "deep-research"));
    assert!(matches!(
        resolve(
            vec![text_block("/workflow stop old-run")],
            &[],
            availability,
            SkillSlashRewrite::default(),
            &workflows,
        )
        .unwrap_err(),
        SlashCommandOutcome::Builtin(BuiltinAction::WorkflowManage { .. })
    ));
    assert!(
        resolve(
            vec![text_block("/workflow review")],
            &[],
            availability,
            SkillSlashRewrite::default(),
            &workflows,
        )
        .is_ok()
    );
}

#[test]
fn workflow_manage_parses_both_orders_and_optional_id() {
    let resolve_workflow = |args: &str| -> BuiltinAction {
        let blocks = vec![text_block(&format!("/workflow {args}"))];
        match resolve(blocks, &[], all_gated(), SkillSlashRewrite::default(), &[]).unwrap_err() {
            SlashCommandOutcome::Builtin(action) => action,
            _ => panic!("expected Builtin outcome"),
        }
    };
    for (args, want_id, want_op) in [
        ("resume", "", "resume"),
        ("pause", "", "pause"),
        ("wf_12ab pause", "wf_12ab", "pause"),
        ("pause wf_12ab", "wf_12ab", "pause"),
        ("SAVE wf_12ab", "wf_12ab", "save"),
        ("pause deep research", "deep research", "pause"),
        ("runs", "", "runs"),
        ("RUNS", "", "runs"),
        ("", "", ""),
    ] {
        match resolve_workflow(args) {
            BuiltinAction::WorkflowManage { run_id, op } => {
                assert_eq!(run_id, want_id, "args: {args:?}");
                assert_eq!(op, want_op, "args: {args:?}");
            }
            other => panic!("expected WorkflowManage, got {}", other.command_name()),
        }
    }

    for (args, want_name, want_input) in [
        (
            r#"pr-review {"pr": 243776}"#,
            "pr-review",
            r#"{"pr": 243776}"#,
        ),
        ("pr-review", "pr-review", ""),
        (
            "deep-research rust pitfalls",
            "deep-research",
            "rust pitfalls",
        ),
        (
            "triage resume the failed jobs",
            "triage",
            "resume the failed jobs",
        ),
        // `runs` is only an op in the bare form; with args it stays a launch.
        ("runs extra words", "runs", "extra words"),
    ] {
        match resolve_workflow(args) {
            BuiltinAction::WorkflowLaunch { name, input } => {
                assert_eq!(name, want_name, "args: {args:?}");
                assert_eq!(input, want_input, "args: {args:?}");
            }
            other => panic!(
                "expected WorkflowLaunch for {args:?}, got {}",
                other.command_name()
            ),
        }
    }
}

#[test]
fn workflow_named_runs_is_shadowed_by_the_runs_op() {
    // `/workflow runs` is always the overview op, even with a workflow named
    // `runs` installed; that workflow still launches via its advertised bare
    // `/runs` command or `/workflow runs <args>`.
    let workflows = vec![listing("runs")];
    assert!(matches!(
        resolve(
            vec![text_block("/workflow runs")],
            &[],
            all_gated(),
            SkillSlashRewrite::default(),
            &workflows,
        )
        .unwrap_err(),
        SlashCommandOutcome::Builtin(BuiltinAction::WorkflowManage { run_id, op })
            if run_id.is_empty() && op == "runs"
    ));
    assert!(matches!(
        resolve(
            vec![text_block("/runs")],
            &[],
            all_gated(),
            SkillSlashRewrite::default(),
            &workflows,
        )
        .unwrap_err(),
        SlashCommandOutcome::Builtin(BuiltinAction::WorkflowLaunch { name, .. }) if name == "runs"
    ));
}

#[test]
fn goal_status_keyword_resolves_to_status() {
    assert!(matches!(resolve_goal("status"), BuiltinAction::GoalStatus));
    assert!(matches!(resolve_goal("STATUS"), BuiltinAction::GoalStatus));
}

#[test]
fn goal_pause_resolves_to_pause() {
    assert!(matches!(resolve_goal("pause"), BuiltinAction::GoalPause));
    assert!(matches!(resolve_goal("PAUSE"), BuiltinAction::GoalPause));
}

#[test]
fn goal_resume_resolves_to_resume() {
    assert!(matches!(resolve_goal("resume"), BuiltinAction::GoalResume));
}

#[test]
fn goal_clear_resolves_to_clear() {
    assert!(matches!(resolve_goal("clear"), BuiltinAction::GoalClear));
}

#[test]
fn goal_objective_resolves_to_set() {
    match resolve_goal("implement auth module") {
        BuiltinAction::GoalSet {
            objective,
            token_budget,
        } => {
            assert_eq!(objective, "implement auth module");
            assert_eq!(token_budget, None);
        }
        other => panic!("expected GoalSet, got {}", other.command_name()),
    }
}

#[test]
fn goal_set_preserves_original_casing() {
    match resolve_goal("Fix BUG in AuthManager") {
        BuiltinAction::GoalSet { objective, .. } => {
            assert_eq!(objective, "Fix BUG in AuthManager");
        }
        other => panic!("expected GoalSet, got {}", other.command_name()),
    }
}

#[test]
fn goal_set_trailing_budget_flag_parses() {
    match resolve_goal("implement X --budget 500000") {
        BuiltinAction::GoalSet {
            objective,
            token_budget,
        } => {
            assert_eq!(objective, "implement X");
            assert_eq!(token_budget, Some(500_000));
        }
        other => panic!("expected GoalSet, got {}", other.command_name()),
    }
}

#[test]
fn goal_set_budget_accepts_boundary_and_extra_whitespace() {
    for (text, objective, budget) in [
        ("do x --budget 1", "do x", 1),
        ("do x --budget   77", "do x", 77),
        ("do x \t --budget 500000", "do x", 500_000),
    ] {
        match resolve_goal(text) {
            BuiltinAction::GoalSet {
                objective: o,
                token_budget,
            } => {
                assert_eq!(o, objective);
                assert_eq!(token_budget, Some(budget), "for {text:?}");
            }
            other => panic!("expected GoalSet, got {}", other.command_name()),
        }
    }
}

#[test]
fn goal_set_malformed_budget_stays_in_objective() {
    // Non-numeric, missing, non-positive, glued, signed, overflowing,
    // or mid-text values must not be consumed as a budget.
    for text in [
        "implement X --budget abc",
        "implement X --budget",
        "implement X --budget 0",
        "implement X --budget -5",
        "implement X --budget +5",
        "implement X --budget 99999999999999999999",
        "implement X --budget5",
        "implement X --budget500000",
        "tune my-fund--budget 100",
        "fix the --budget flag parsing bug",
        "--budget 500000",
    ] {
        match resolve_goal(text) {
            BuiltinAction::GoalSet {
                objective,
                token_budget,
            } => {
                assert_eq!(objective, text, "objective must be preserved verbatim");
                assert_eq!(token_budget, None, "no budget must be parsed from {text:?}");
            }
            other => panic!("expected GoalSet, got {}", other.command_name()),
        }
    }
}

#[test]
fn goal_command_name_is_goal() {
    assert_eq!(BuiltinAction::GoalStatus.command_name(), "goal");
    assert_eq!(BuiltinAction::GoalPause.command_name(), "goal");
    assert_eq!(BuiltinAction::GoalResume.command_name(), "goal");
    assert_eq!(BuiltinAction::GoalClear.command_name(), "goal");
    assert_eq!(
        BuiltinAction::GoalSet {
            objective: "x".into(),
            token_budget: None,
        }
        .command_name(),
        "goal"
    );
}

#[test]
fn goal_args_provided() {
    assert!(
        BuiltinAction::GoalSet {
            objective: "x".into(),
            token_budget: None,
        }
        .args_provided()
    );
    assert!(!BuiltinAction::GoalStatus.args_provided());
    assert!(!BuiltinAction::GoalPause.args_provided());
    assert!(!BuiltinAction::GoalResume.args_provided());
    assert!(!BuiltinAction::GoalClear.args_provided());
}

// ── GoalTracker handler-level interaction tests ──────────────
// These test the exact tracker state transitions that the slash
// command handlers perform, without constructing a full SessionActor.

#[test]
fn goal_tracker_status_with_no_goal_returns_none() {
    use crate::session::goal_tracker::GoalTracker;
    let tracker = GoalTracker::new(std::path::PathBuf::from("/tmp/test"));
    assert!(tracker.snapshot().is_none());
    assert!(tracker.status().is_none());
}

#[test]
fn goal_tracker_create_sets_active() {
    use crate::session::goal_tracker::{GoalStatus, GoalTracker};
    let mut tracker = GoalTracker::new(std::path::PathBuf::from("/tmp/test"));
    tracker.create_goal("g1".into(), "obj".into(), None, 0, "now".into(), None);
    assert_eq!(tracker.status(), Some(GoalStatus::Active));
    assert_eq!(tracker.objective(), Some("obj"));
}

#[test]
fn goal_tracker_pause_only_when_active() {
    use crate::session::goal_tracker::{GoalPauseReason, GoalStatus, GoalTracker};
    let mut tracker = GoalTracker::new(std::path::PathBuf::from("/tmp/test"));
    // No goal — pause returns false
    assert!(!tracker.pause(GoalPauseReason::User));

    tracker.create_goal("g1".into(), "obj".into(), None, 0, "now".into(), None);
    assert!(tracker.pause(GoalPauseReason::User));
    assert_eq!(tracker.status(), Some(GoalStatus::UserPaused));
    // Already paused — pause returns false
    assert!(!tracker.pause(GoalPauseReason::User));
}

#[test]
fn goal_tracker_resume_only_when_paused() {
    use crate::session::goal_tracker::{GoalPauseReason, GoalStatus, GoalTracker};
    let mut tracker = GoalTracker::new(std::path::PathBuf::from("/tmp/test"));
    tracker.create_goal("g1".into(), "obj".into(), None, 0, "now".into(), None);
    // Active — resume returns false
    assert!(!tracker.resume());
    tracker.pause(GoalPauseReason::User);
    assert!(tracker.resume());
    assert_eq!(tracker.status(), Some(GoalStatus::Active));
}

#[test]
fn goal_tracker_clear_removes_orchestration() {
    use crate::session::goal_tracker::GoalTracker;
    let mut tracker = GoalTracker::new(std::path::PathBuf::from("/tmp/test"));
    tracker.create_goal("g1".into(), "obj".into(), None, 0, "now".into(), None);
    assert!(tracker.snapshot().is_some());
    tracker.clear();
    assert!(tracker.snapshot().is_none());
}

#[test]
fn goal_tracker_create_replaces_existing() {
    use crate::session::goal_tracker::GoalTracker;
    let mut tracker = GoalTracker::new(std::path::PathBuf::from("/tmp/test"));
    tracker.create_goal("g1".into(), "first".into(), None, 0, "now".into(), None);
    tracker.create_goal("g2".into(), "second".into(), None, 0, "now".into(), None);
    assert_eq!(tracker.objective(), Some("second"));
}

#[test]
fn goal_tracker_account_elapsed_flushes_delta() {
    use crate::session::goal_tracker::GoalTracker;
    let mut tracker = GoalTracker::new(std::path::PathBuf::from("/tmp/test"));
    tracker.create_goal("g1".into(), "obj".into(), None, 0, "now".into(), None);
    // After create_goal, elapsed_ms is 0 but active_since is set.
    let before = tracker.snapshot().unwrap().elapsed_ms;
    assert_eq!(before, 0);
    // account_elapsed flushes pending wall-clock time.
    std::thread::sleep(std::time::Duration::from_millis(5));
    tracker.account_elapsed();
    let after = tracker.snapshot().unwrap().elapsed_ms;
    assert!(after > 0, "elapsed should be > 0 after account_elapsed");
}
