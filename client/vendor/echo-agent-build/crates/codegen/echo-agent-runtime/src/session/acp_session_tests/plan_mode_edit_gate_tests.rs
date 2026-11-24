//! Plan-mode edit gate through the real `prepare_tool_call` path: plan mode
//! is read-only except the plan file in EVERY permission mode. The fixture's
//! `PermissionHandle::allow_all()` is the always-approve worst case — before
//! the gate, it silently approved any edit in plan mode (the "yolo edits in
//! plan mode" bug); these tests pin that the gate rejects
//! BEFORE the permission layer can auto-approve.
use super::support::*;
use super::*;
/// Build an actor whose toolset parses echoagent `search_replace` plus the plan
/// tools (so `${{ tools.by_kind.exit_plan }}` resolves in the rejection
/// message), with a gateway drain answering session notifications.
async fn build_gate_actor() -> SessionActor {
    use echo_agent_tools::implementations::echo_agent_build::enter_plan_mode::EnterPlanModeTool;
    use echo_agent_tools::implementations::echo_agent_build::exit_plan_mode::ExitPlanModeTool;
    use echo_agent_tools::registry::types::ToolConfig;
    let (gateway_tx, mut gateway_rx) =
        tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
    let (persistence_tx, _persistence_rx) =
        tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
    let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
    *actor.agent.borrow_mut() = test_agent_with_tools(vec![
        // search_replace's requirements demand a Read tool in the same toolset.
        ToolConfig::from_id("EchoAgentBuild:read_file"),
        ToolConfig::from_id("EchoAgentBuild:search_replace"),
        ToolConfig::for_tool::<EnterPlanModeTool>(),
        ToolConfig::for_tool::<ExitPlanModeTool>(),
    ])
    .await;
    tokio::task::spawn_local(async move {
        while let Some(msg) = gateway_rx.recv().await {
            if let echo_agent_acp::AcpClientMessage::SessionNotification(args) = msg {
                let _ = args.response_tx.send(Ok(()));
            }
        }
    });
    actor
}
/// Flip the fixture's tracker to Active (plan file: `/tmp/test-session/plan.md`).
fn activate_plan_mode(actor: &SessionActor) {
    let mut tracker = actor.plan_mode.lock();
    assert!(tracker.enter_pending());
    assert!(tracker.activate());
}
fn search_replace_call(id: &str, path: &str) -> ToolCallResponse {
    ToolCallResponse {
        id: id.to_string(),
        kind: "function".to_string(),
        function: crate::sampling::types::ToolCallFunction::new(
            "search_replace",
            format!(r#"{{"file_path":"{path}","old_string":"a","new_string":"b"}}"#),
        ),
    }
}
fn pre_tool_use_registry(script: &str) -> echo_agent_hooks::discovery::HookRegistry {
    command_hook_registry(echo_agent_hooks::event::HookEventName::PreToolUse, script)
}
fn command_hook_registry(
    event: echo_agent_hooks::event::HookEventName,
    script: &str,
) -> echo_agent_hooks::discovery::HookRegistry {
    let (mut registry, _) = echo_agent_hooks::discovery::load_hooks(None, None);
    registry.append_specs(vec![echo_agent_hooks::config::HookSpec {
        name: "test/pretooluse".into(),
        event,
        handler_type: echo_agent_hooks::config::HandlerType::Command,
        configured_matcher: None,
        matcher: None,
        enabled: true,
        command: Some(std::path::PathBuf::from(script)),
        command_raw: Some(script.to_string()),
        url: None,
        url_raw: None,
        timeout_ms: 5000,
        source_dir: std::path::PathBuf::from("/tmp"),
        extra_env: std::collections::HashMap::new(),
        layer: echo_agent_hooks::config::HookProvenance::File,
    }]);
    registry
}

#[tokio::test(flavor = "current_thread")]
async fn readonly_children_disable_command_hooks() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut actor = build_gate_actor().await;
            *actor.hook_registry.borrow_mut() = Some(std::sync::Arc::new(pre_tool_use_registry("true")));
            actor.startup_hints.inherited_turn_authority =
                Some(crate::session::InheritedTurnAuthority {
                    prompt_mode: crate::session::plan_mode::PromptMode::Ask,
                    session_id: "root".into(),
                    run_id: "run".into(),
                    workspace: "/tmp".into(),
                });
            assert!(actor.allowed_hook_registry().is_none());
            assert!(!actor.may_have_hooks_for(echo_agent_hooks::event::HookEventName::PreToolUse));
            actor.startup_hints.inherited_turn_authority = None;
            assert!(
                actor.allowed_hook_registry().is_some(),
                "ordinary CLI Agent hooks remain available"
            );
        })
        .await;
}
async fn prepare(
    actor: &SessionActor,
    call: ToolCallResponse,
) -> Result<PreparedToolCall, ToolLoop> {
    let mut deferred = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        actor.prepare_tool_call(call, &mut deferred),
    )
    .await
    .expect("prepare_tool_call must not hang (a hang means a permission prompt was issued)")
    .expect("prepare_tool_call must not error")
}
/// Last tool_result pushed for `call_id`, or panic.
async fn tool_result_text(actor: &SessionActor, call_id: &str) -> String {
    let conv = actor.chat_state_handle.get_conversation().await;
    conv.iter()
        .rev()
        .find_map(|item| match item {
            echo_agent_sampling_types::ConversationItem::ToolResult(tr)
                if tr.tool_call_id == call_id =>
            {
                Some(tr.content.to_string())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no tool_result for {call_id} in {conv:?}"))
}
/// The headline: plan mode Active + allow-all permissions (the always-approve
/// worst case) still rejects a echoagent edit outside the plan file, without ever
/// reaching the permission layer, and steers the model to `exit_plan_mode`.
#[tokio::test(flavor = "current_thread")]
async fn plan_mode_rejects_echo_agent_edit_outside_plan_file_despite_allow_all_permissions() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let actor = build_gate_actor().await;
            activate_plan_mode(&actor);
            let result =
                prepare(&actor, search_replace_call("call_gate", "/tmp/src/main.rs")).await;
            assert!(
                matches!(result, Err(ToolLoop::Continue)),
                "gate must reject with Continue (tool not executed); got {result:?}"
            );
            let text = tool_result_text(&actor, "call_gate").await;
            assert!(
                text.contains("Rejected: file edits are not allowed in plan mode"),
                "rejection text: {text}"
            );
            assert!(
                text.contains("/tmp/test-session/plan.md"),
                "must name the plan file so the model knows the one editable path: {text}"
            );
            assert!(
                !text.contains("exit_plan_mode"),
                "rejection should stay short (no exit-tool steering): {text}"
            );
        })
        .await;
}
/// The carve-out: the plan file itself prepares cleanly (the gate defers to
/// `should_auto_approve_edit`, the same predicate as the permission bypass).
#[tokio::test(flavor = "current_thread")]
async fn plan_mode_allows_plan_file_edit() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let actor = build_gate_actor().await;
            activate_plan_mode(&actor);
            let result = prepare(
                &actor,
                search_replace_call("call_plan_file", "/tmp/test-session/plan.md"),
            )
            .await;
            assert!(
                result.is_ok(),
                "plan-file edit must pass the gate and prepare; got {:?}",
                result.err()
            );
        })
        .await;
}
/// Control: with plan mode inactive the same edit prepares cleanly — the gate
/// is plan-scoped, not a general edit block.
#[tokio::test(flavor = "current_thread")]
async fn inactive_plan_mode_does_not_gate_edits() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let actor = build_gate_actor().await;
            let result = prepare(
                &actor,
                search_replace_call("call_no_plan", "/tmp/src/main.rs"),
            )
            .await;
            assert!(
                result.is_ok(),
                "edit outside plan mode must prepare; got {:?}",
                result.err()
            );
        })
        .await;
}
/// The gate must see a PreToolUse hook's rewrite: the model edits the plan file
/// (allowed), the hook redirects it outside, and the gate rejects. Pins hook
/// dispatch running before the plan gate.
#[tokio::test(flavor = "current_thread")]
async fn plan_gate_sees_hook_rewritten_path() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut actor = build_gate_actor().await;
            actor.hook_resolved_workspace_root = "/tmp".to_string();
            *actor.hook_registry.borrow_mut() = Some(
                std::sync::Arc::new(
                    pre_tool_use_registry(
                        r#"echo '{"hookSpecificOutput":{"updatedInput":{"file_path":"/tmp/src/main.rs","old_string":"a","new_string":"b"}}}'"#,
                    ),
                ),
            );
            activate_plan_mode(&actor);
            let result = prepare(
                    &actor,
                    search_replace_call("call_hook_gate", "/tmp/test-session/plan.md"),
                )
                .await;
            assert!(
                matches!(result, Err(ToolLoop::Continue)),
                "plan gate must reject the hook-rewritten non-plan path; got {result:?}"
            );
        })
        .await;
}

/// Explicit user modes are an immutable capability ceiling, including in the
/// always-approve fixture, without waiting for MCP initialization or hooks.
#[tokio::test(flavor = "current_thread")]
async fn explicit_readonly_turn_rejects_writes_shell_mcp_children_and_escalation() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for mode in [
                crate::session::plan_mode::PromptMode::Ask,
                crate::session::plan_mode::PromptMode::Plan,
            ] {
                let actor = build_gate_actor().await;
                *actor.turn_start_prompt_mode.lock() = mode;
                // A mid-turn mode update cannot widen the original user grant.
                *actor.current_prompt_mode.lock() = crate::session::plan_mode::PromptMode::Agent;
                *actor.turn_prompt_mode.lock() = crate::session::plan_mode::PromptMode::Agent;
                for (name, args) in [
                    (
                        "search_replace",
                        r#"{"file_path":"/tmp/readonly-target","old_string":"a","new_string":"b"}"#,
                    ),
                    (
                        "run_terminal_command",
                        r#"{"command":"touch /tmp/readonly-target"}"#,
                    ),
                    ("unknown_mcp__write", "{}"),
                    ("use_tool", r#"{"tool_name":"write_file","tool_input":{}}"#),
                    ("spawn_subagent", r#"{"prompt":"write a file"}"#),
                    ("exit_plan_mode", "{}"),
                ] {
                    let id = format!("readonly_{mode}_{name}");
                    let call = ToolCallResponse {
                        id: id.clone(),
                        kind: "function".into(),
                        function: crate::sampling::types::ToolCallFunction::new(
                            name,
                            args.to_string(),
                        ),
                    };
                    assert!(
                        matches!(prepare(&actor, call).await, Err(ToolLoop::Continue)),
                        "{mode}: {name}"
                    );
                    assert!(
                        tool_result_text(&actor, &id)
                            .await
                            .contains("capability policy blocked")
                    );
                }
                let definitions = actor.prepare_tool_definitions().await;
                assert!(
                    definitions
                        .iter()
                        .any(|tool| tool.function.name == "read_file")
                );
                assert!(
                    !definitions
                        .iter()
                        .any(|tool| tool.function.name == "search_replace"
                            || tool.function.name == "exit_plan_mode")
                );
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn child_readonly_authority_cannot_escalate() {
    tokio::task::LocalSet::new().run_until(async {
        let mut actor=build_gate_actor().await;
        actor.startup_hints.inherited_turn_authority=Some(crate::session::InheritedTurnAuthority {
            prompt_mode:crate::session::plan_mode::PromptMode::Ask,
            session_id:"parent".into(),run_id:"parent-turn".into(),workspace:"/tmp".into(),
        });
        *actor.turn_start_prompt_mode.lock()=crate::session::plan_mode::PromptMode::Agent;
        *actor.current_prompt_mode.lock()=crate::session::plan_mode::PromptMode::Agent;
        assert!(actor.is_readonly_turn());
        let result=prepare(&actor,search_replace_call("child-write","/tmp/a.txt")).await;
        assert!(matches!(result,Err(ToolLoop::Continue)));
        assert!(tool_result_text(&actor,"child-write").await.contains("capability policy blocked"));
        let hints:crate::session::StartupHints=serde_json::from_value(serde_json::json!({"inheritedTurnAuthority":{"promptMode":"agent"}})).unwrap();
        assert!(hints.inherited_turn_authority.is_none(),"wire JSON must not manufacture native authority");
    }).await;
}
