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
async fn desktop_document_bridge_and_readonly_children_disable_command_hooks() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let directory = tempfile::tempdir().unwrap();
            let marker = directory.path().join("hook-must-not-run");
            let script = format!("touch '{}'", marker.display());
            let mut actor = build_gate_actor().await;
            actor.desktop_document_turn.set(true);
            *actor.hook_registry.borrow_mut() = Some(std::sync::Arc::new(command_hook_registry(
                echo_agent_hooks::event::HookEventName::SubagentStart,
                &script,
            )));
            assert!(actor.allowed_hook_registry().is_none());
            actor
                .dispatch_hook(
                    echo_agent_hooks::event::HookEventName::SubagentStart,
                    echo_agent_hooks::event::HookPayload::SubagentStart {
                        subagent_id: "child".into(),
                        subagent_type: "explore".into(),
                        description: None,
                    },
                    Some("run"),
                    None,
                )
                .await;
            assert!(!marker.exists());
            *actor.hook_registry.borrow_mut() =
                Some(std::sync::Arc::new(pre_tool_use_registry(&script)));
            assert!(
                prepare(&actor, search_replace_call("doc-hook", "/tmp/a.txt"))
                    .await
                    .is_ok()
            );
            assert!(
                !marker.exists(),
                "document Agent hooks cannot bypass dirty-buffer preflight"
            );
            actor.desktop_document_turn.set(false);
            actor.startup_hints.inherited_turn_authority =
                Some(crate::session::InheritedTurnAuthority {
                    prompt_mode: crate::session::plan_mode::PromptMode::Ask,
                    document_tools: true,
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
async fn desktop_document_bridge_preserves_bounded_task_and_constrains_dynamic_dispatch() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use echo_agent_tools::implementations::echo_agent_build::kill_task::KillTaskTool;
            use echo_agent_tools::implementations::echo_agent_build::task::TaskTool;
            use echo_agent_tools::implementations::echo_agent_build::task_output::TaskOutputTool;
            use echo_agent_tools::implementations::use_tool::UseTool;
            use echo_agent_tools::registry::types::ToolConfig;
            use echo_agent_tools::types::tool::ToolKind;
            let actor = build_gate_actor().await;
            *actor.agent.borrow_mut() = test_agent_with_tools(vec![
                ToolConfig::from_id("EchoAgentBuild:read_file"),
                ToolConfig::for_tool::<TaskTool>(),
                ToolConfig::for_tool::<TaskOutputTool>(),
                ToolConfig::for_tool::<KillTaskTool>(),
                ToolConfig::for_tool::<UseTool>(),
            ])
            .await;
            let names = actor
                .prepare_tool_definitions()
                .await
                .into_iter()
                .filter(|tool| {
                    matches!(
                        actor.tool_bridge_handle().tool_kind(&tool.function.name),
                        Some(ToolKind::Task | ToolKind::UseTool)
                    )
                })
                .map(|tool| tool.function.name)
                .collect::<Vec<_>>();
            assert_eq!(
                names.len(),
                2,
                "real registered delegation tools must exist before the desktop turn"
            );
            actor
                .tool_bridge_handle()
                .update_resource(DesktopDocumentTools(true))
                .await;
            let advertised = actor.prepare_tool_definitions().await;
            for name in names {
                if actor.tool_bridge_handle().tool_kind(&name)==Some(ToolKind::Task) {
                    assert!(advertised.iter().any(|tool|tool.function.name==name));
                    let call=ToolCallResponse{id:"desktop_inherited_task".into(),kind:"function".into(),function:crate::sampling::types::ToolCallFunction::new(name,r#"{"prompt":"Review the current buffer","description":"Readonly review","subagent_type":"general-purpose"}"#)};
                    assert!(prepare(&actor,call).await.is_ok(),"Task must remain available through inherited coordination");
                    continue;
                }
                assert!(advertised.iter().any(|tool| tool.function.name == name), "MCP use_tool must remain advertised in Agent");
                let id = format!("desktop_no_delegation_{name}");
                let call = ToolCallResponse {
                    id: id.clone(),
                    kind: "function".into(),
                    function: crate::sampling::types::ToolCallFunction::new(name, r#"{"tool_name":"read_file","tool_input":{}}"#),
                };
                assert!(matches!(
                    prepare(&actor, call).await,
                    Err(ToolLoop::Continue)
                ));
                assert!(
                    tool_result_text(&actor, &id)
                        .await
                        .contains("cannot be routed through use_tool")
                );
            }
        })
        .await;
}

#[derive(Debug)]
struct CoordinatedMcpFixture(std::sync::Arc<std::sync::atomic::AtomicUsize>);
impl echo_agent_tools::types::tool_metadata::ToolMetadata for CoordinatedMcpFixture {
    fn kind(&self) -> echo_agent_tools::types::tool::ToolKind {
        echo_agent_tools::types::tool::ToolKind::Other
    }
    fn tool_namespace(&self) -> echo_agent_tools::types::tool::ToolNamespace {
        echo_agent_tools::types::tool::ToolNamespace::MCP
    }
    fn description_template(&self) -> &str {
        "Counter for the actual local MCP dispatch adapter"
    }
}
impl echo_agent_tool_runtime::Tool for CoordinatedMcpFixture {
    type Args = serde_json::Value;
    type Output = String;
    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("coordinated_mcp_fixture").unwrap()
    }
    fn description(
        &self,
        _: &echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "coordinated_mcp_fixture",
            "actual dispatch counter",
        )
    }
    async fn run(
        &self,
        _: echo_agent_tool_runtime::ToolCallContext,
        _: serde_json::Value,
    ) -> Result<String, echo_agent_tool_runtime::ToolError> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok("actual MCP adapter executed".into())
    }
}
#[derive(Debug)]
struct CoordinatedGatewayFixture(
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
    Option<(
        std::sync::Arc<tokio::sync::Notify>,
        std::sync::Arc<tokio::sync::Notify>,
    )>,
);
#[async_trait::async_trait]
impl echo_agent_tools::types::resources::ManagedGatewayToolCaller for CoordinatedGatewayFixture {
    async fn call_tool(
        &self,
        call_id: &str,
        _: serde_json::Value,
        caller: &str,
    ) -> Result<
        echo_agent_tools::types::resources::ManagedGatewayToolCallResponse,
        echo_agent_tool_runtime::ToolError,
    > {
        if let Some((started, release)) = &self.1 {
            started.notify_one();
            release.notified().await;
        }
        assert_eq!(call_id, "trusted-call-id");
        assert_eq!(caller, "use_tool");
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(
            echo_agent_tools::types::resources::ManagedGatewayToolCallResponse {
                result: serde_json::json!({"content":[{"type":"text","text":"actual managed MCP adapter executed"}]}),
                connectors_needing_reauth: Vec::new(),
            },
        )
    }
}

fn host_completion_seen(
    observed: &std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    call: &str,
) -> bool {
    observed.lock().unwrap().iter().any(|request| {
        request["phase"] == "after"
            && request["operationId"]
                .as_str()
                .is_some_and(|id| id.ends_with(call))
            && request["executionSettled"] == true
    })
}

#[tokio::test(flavor = "current_thread")]
async fn desktop_document_bridge_mcp_executes_only_after_preflight_and_observes_afterward() {
    tokio::task::LocalSet::new().run_until(async {
        use std::sync::{Arc,atomic::{AtomicBool,AtomicUsize,Ordering}};
        use echo_agent_tools::{implementations::use_tool::UseTool,registry::types::ToolConfig,types::resources::{ManagedGatewayToolCatalog,ManagedGatewayToolClient,ManagedGatewayToolSource}};
        let (gateway_tx,mut gateway_rx)=tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
        let (persistence_tx,_persistence_rx)=tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
        let actor=Arc::new(create_test_actor(0,256_000,85,gateway_tx,persistence_tx).await);
        *actor.agent.borrow_mut()=test_agent_with_tools(vec![ToolConfig::from_id("EchoAgentBuild:read_file"),ToolConfig::for_tool::<UseTool>()]).await;
        let bridge=actor.tool_bridge_handle();
        bridge.update_resource(DesktopDocumentTools(true)).await;
        actor.desktop_document_turn.set(true);
        *actor.current_prompt_id.lock().unwrap()=Some("actual-mcp-parent-run".into());
        let counter=Arc::new(AtomicUsize::new(0));
        bridge.toolset().register_tool("fixture__counter".into(),CoordinatedMcpFixture(counter.clone()),Some(serde_json::json!({"type":"object"}))).unwrap();
        let managed_counter=Arc::new(AtomicUsize::new(0));
        let mut catalog=ManagedGatewayToolCatalog::default();
        catalog.0.insert("managed_fixture".into(),ManagedGatewayToolSource{connector_id:"trusted-connector".into(),connector_name:"fixture".into(),tool_id:"trusted-tool-id".into(),tool_name:"managed_fixture".into(),call_id:"trusted-call-id".into()});
        bridge.update_resource(catalog).await;
        let session_id=actor.session_id_string();
        actor.workspace_ops.bind_local_session(&session_id,actor.tool_context.cwd.as_path().to_path_buf(),actor.tool_context.hunk_tracker_handle.clone(),bridge.toolset(),None).unwrap();
        let blocked=Arc::new(AtomicBool::new(true));
        let observed=Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let host_blocked=blocked.clone();let host_observed=observed.clone();let host_session=session_id.clone();
        let responder=tokio::task::spawn_local(async move {
            while let Some(message)=gateway_rx.recv().await {
                match message {
                    echo_agent_acp::AcpClientMessage::SessionNotification(args)=>{let _=args.response_tx.send(Ok(()));},
                    echo_agent_acp::AcpClientMessage::ExtMethod(args)=>{
                        assert_eq!(&*args.request.method,"echo.agent/desktop_document_tool");
                        let request:serde_json::Value=serde_json::from_str(args.request.params.get()).unwrap();
                        assert_eq!(request["sessionId"],host_session);assert_eq!(request["runId"],"actual-mcp-parent-run");
                        assert_eq!(request["toolKind"],"use_tool");
                        host_observed.lock().unwrap().push(request.clone());
                        let result=if request["phase"]=="before"&&host_blocked.load(Ordering::SeqCst) {serde_json::json!({"handled":true,"error":"dirty editor blocks arbitrary MCP effects"})}
                            else if request["phase"]=="before" {serde_json::json!({"handled":false})} else {serde_json::json!({"handled":true,"text":"observed actual MCP completion"})};
                        let _=args.response_tx.send(Ok(acp::ExtResponse::new(serde_json::value::to_raw_value(&result).unwrap().into())));
                    },
                    _=>{},
                }
            }
        });
        let call=|id:&str,target:&str|ToolCallResponse{id:id.into(),kind:"function".into(),function:crate::sampling::types::ToolCallFunction::new("use_tool",serde_json::json!({"tool_name":target,"tool_input":{}}).to_string())};
        for (id,target) in [("native-bypass","read_file"),("task-bypass","Task"),("unregistered","unknown__tool"),("gateway-no-client","managed_fixture")] {
            actor.execute_tool_calls(vec![call(id,target)]).await.unwrap();
            assert!(tool_result_text(&actor,id).await.contains("existing registered MCP"));
        }
        assert!(observed.lock().unwrap().is_empty(),"untrusted routing must be rejected before any external preflight/execution");
        actor.execute_tool_calls(vec![call("dirty-refused","fixture__counter")]).await.unwrap();
        assert_eq!(counter.load(Ordering::SeqCst),0);
        assert!(tool_result_text(&actor,"dirty-refused").await.contains("dirty editor"));
        blocked.store(false,Ordering::SeqCst);
        actor.execute_tool_calls(vec![call("clean-executed","fixture__counter")]).await.unwrap();
        assert_eq!(counter.load(Ordering::SeqCst),1);
        assert!(tool_result_text(&actor,"clean-executed").await.contains("actual MCP adapter executed"));
        bridge.update_resource(ManagedGatewayToolClient(Arc::new(CoordinatedGatewayFixture(managed_counter.clone(),None)))).await;
        actor.execute_tool_calls(vec![call("managed-executed","managed_fixture")]).await.unwrap();
        assert_eq!(managed_counter.load(Ordering::SeqCst),1);
        assert!(tool_result_text(&actor,"managed-executed").await.contains("actual managed MCP adapter executed"));
        assert_eq!(observed.lock().unwrap().iter().map(|request|request["phase"].as_str().unwrap()).collect::<Vec<_>>(),["before","before","after","before","after"]);
        let started=Arc::new(tokio::sync::Notify::new());let release=Arc::new(tokio::sync::Notify::new());
        bridge.update_resource(ManagedGatewayToolClient(Arc::new(CoordinatedGatewayFixture(managed_counter.clone(),Some((started.clone(),release.clone())))))).await;
        let delayed_call=call("delayed-cancelled-parent","managed_fixture");let running_actor=actor.clone();
        let running=tokio::task::spawn_local(async move {running_actor.execute_tool_calls(vec![delayed_call]).await});
        tokio::time::timeout(std::time::Duration::from_secs(2),started.notified()).await.expect("actual managed RPC must enter before cancel");
        running.abort();let _=running.await;
        assert_eq!(managed_counter.load(Ordering::SeqCst),1,"cancel did not fabricate the remote terminal");
        release.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(2),async {
            loop {
                if host_completion_seen(&observed,"delayed-cancelled-parent") {break;}
                tokio::task::yield_now().await;
            }
        }).await.expect("retained actual RPC must emit settlement after its outer model turn was aborted");
        assert_eq!(managed_counter.load(Ordering::SeqCst),2);
        *actor.turn_start_prompt_mode.lock()=crate::session::plan_mode::PromptMode::Ask;
        actor.execute_tool_calls(vec![call("readonly-mcp-refused","fixture__counter")]).await.unwrap();
        assert_eq!(counter.load(Ordering::SeqCst),1,"Ask must still block MCP even if the tool has been discovered");
        responder.abort();
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn desktop_document_bridge_child_readonly_authority_cannot_escalate() {
    tokio::task::LocalSet::new().run_until(async {
        let mut actor=build_gate_actor().await;
        actor.startup_hints.inherited_turn_authority=Some(crate::session::InheritedTurnAuthority {
            prompt_mode:crate::session::plan_mode::PromptMode::Ask,document_tools:true,
            session_id:"parent".into(),run_id:"parent-turn".into(),workspace:"/tmp".into(),
        });
        *actor.turn_start_prompt_mode.lock()=crate::session::plan_mode::PromptMode::Agent;
        *actor.current_prompt_mode.lock()=crate::session::plan_mode::PromptMode::Agent;
        assert!(actor.is_readonly_turn());
        let result=prepare(&actor,search_replace_call("child-write","/tmp/a.txt")).await;
        assert!(matches!(result,Err(ToolLoop::Continue)));
        assert!(tool_result_text(&actor,"child-write").await.contains("capability policy blocked"));
        let hints:crate::session::StartupHints=serde_json::from_value(serde_json::json!({"inheritedTurnAuthority":{"promptMode":"agent","documentTools":false}})).unwrap();
        assert!(hints.inherited_turn_authority.is_none(),"wire JSON must not manufacture native authority");
    }).await;
}
