//! `permission_prompt` fires only while a real UI permission prompt is waiting.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use agent_client_protocol as acp;
use echo_agent_acp::AcpAgentGatewaySender;
use echo_agent_paths::AbsPathBuf;
use echo_agent_tools::registry::types::ToolConfig;
use echo_agent_workspace::permission::{ClientType, spawn_permission_manager};

use super::support::*;
use super::*;

fn install_notification_client_hook(actor: &SessionActor) {
    let mut client_hooks = crate::extensions::hooks::ClientHooks::new();
    client_hooks.insert(
        echo_agent_hooks::event::HookEventName::Notification,
        vec![crate::extensions::hooks::ClientHookGroup {
            matcher: None,
            callback_ids: vec!["cb_permission".to_string()],
            timeout: None,
        }],
    );
    *actor.client_hooks.borrow_mut() = client_hooks;
}

fn install_real_permissions(actor: &mut SessionActor, yolo: bool, gateway: AcpAgentGatewaySender) {
    let cwd = AbsPathBuf::new(std::path::PathBuf::from(actor.session_info.cwd.clone()))
        .unwrap_or_else(|_| AbsPathBuf::new(std::path::PathBuf::from("/tmp")).unwrap());
    let (handle, _ev) = spawn_permission_manager(
        actor.session_info.id.clone(),
        gateway,
        cwd,
        ClientType::Generic,
        None,
        vec![],
        vec![],
        yolo,
        None,
    );
    actor.permissions = handle;
}

fn read_call(id: &str) -> ToolCallResponse {
    ToolCallResponse {
        id: id.to_string(),
        kind: "function".to_string(),
        function: crate::sampling::types::ToolCallFunction::new(
            "read_file",
            serde_json::json!({ "target_file": "/tmp/permission-hook.txt" }).to_string(),
        ),
    }
}

fn edit_call(id: &str) -> ToolCallResponse {
    ToolCallResponse {
        id: id.to_string(),
        kind: "function".to_string(),
        function: crate::sampling::types::ToolCallFunction::new(
            "search_replace",
            serde_json::json!({
                "file_path": "/tmp/permission-hook.txt",
                "old_string": "a",
                "new_string": "b",
            })
            .to_string(),
        ),
    }
}

fn spawn_gateway_loop(
    gateway_rx: tokio::sync::mpsc::UnboundedReceiver<echo_agent_acp::AcpClientMessage>,
    permission_prompt_hooks: Arc<AtomicUsize>,
    park_until_hook: bool,
) {
    let mut gateway_rx = gateway_rx;
    tokio::task::spawn_local(async move {
        while let Some(msg) = gateway_rx.recv().await {
            match msg {
                echo_agent_acp::AcpClientMessage::RequestPermission(args) => {
                    let hooks = permission_prompt_hooks.clone();
                    tokio::task::spawn_local(async move {
                        if park_until_hook {
                            let start = Instant::now();
                            while hooks.load(Ordering::SeqCst) == 0 {
                                assert!(
                                    start.elapsed() < Duration::from_secs(3),
                                    "permission_prompt hook must fire before the user answers"
                                );
                                tokio::task::yield_now().await;
                            }
                        }
                        let _ = args
                            .response_tx
                            .send(Ok(acp::RequestPermissionResponse::new(
                                acp::RequestPermissionOutcome::Selected(
                                    acp::SelectedPermissionOutcome::new(
                                        acp::PermissionOptionId::new("allow-once"),
                                    ),
                                ),
                            )));
                    });
                }
                echo_agent_acp::AcpClientMessage::ExtNotification(args) => {
                    if args.request.method.as_ref() == "echo.agent/hooks/event" {
                        let params: serde_json::Value =
                            serde_json::from_str(args.request.params.get()).unwrap_or_default();
                        if params["notificationType"] == "permission_prompt" {
                            permission_prompt_hooks.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                }
                echo_agent_acp::AcpClientMessage::SessionNotification(args) => {
                    let _ = args.response_tx.send(Ok(()));
                }
                _ => {}
            }
        }
    });
}

/// A regression notify hops manager → listener → `fire_hook` → gateway.
/// Drain enough LocalSet turns that a real hook would have incremented.
async fn drain_permission_prompt_hook_chain() {
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
}

async fn prepare(
    actor: &SessionActor,
    call: ToolCallResponse,
) -> Result<PreparedToolCall, ToolLoop> {
    let mut deferred = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        actor.prepare_tool_call(call, &mut deferred),
    )
    .await
    .expect("prepare_tool_call must not hang")
    .expect("prepare_tool_call must not error")
}

fn edit_toolset() -> Vec<ToolConfig> {
    vec![
        ToolConfig::from_id("EchoAgentBuild:read_file"),
        ToolConfig {
            id: "EchoAgentBuild:search_replace".into(),
            params: Some(
                serde_json::from_value(serde_json::json!({
                    "skip_read_before_edit": true
                }))
                .unwrap(),
            ),
            name_override: None,
            params_name_overrides: None,
            description_override: None,
            behavior_version: None,
            kind: None,
        },
    ]
}

async fn setup_actor(
    yolo: bool,
    park_until_hook: bool,
) -> (Arc<SessionActor>, Arc<AtomicUsize>, tokio::task::LocalSet) {
    let local = tokio::task::LocalSet::new();
    // LocalSet must wrap construction: permission manager uses spawn_local.
    let (actor, hooks) = local
        .run_until(async {
            let (gateway_tx, gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let hook_gateway = AcpAgentGatewaySender::new(gateway_tx.clone());
            let (persistence_tx, _persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            *actor.agent.borrow_mut() = test_agent_with_tools(edit_toolset()).await;
            install_real_permissions(&mut actor, yolo, hook_gateway);
            install_notification_client_hook(&actor);
            let actor = Arc::new(actor);
            actor.wire_permission_prompt_notification();
            let hooks = Arc::new(AtomicUsize::new(0));
            spawn_gateway_loop(gateway_rx, hooks.clone(), park_until_hook);
            (actor, hooks)
        })
        .await;
    (actor, hooks, local)
}

#[tokio::test(flavor = "current_thread")]
async fn auto_allowed_tool_does_not_fire_permission_prompt_notification() {
    let (actor, hooks, local) = setup_actor(/*yolo=*/ false, /*park_until_hook=*/ false).await;
    local
        .run_until(async {
            let result = prepare(&actor, read_call("call_safe")).await;
            assert!(
                result.is_ok(),
                "read must auto-allow; got {:?}",
                result.err()
            );
            drain_permission_prompt_hook_chain().await;
            assert_eq!(
                hooks.load(Ordering::SeqCst),
                0,
                "auto-allowed tool permission must not fire permission_prompt"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn real_user_prompt_fires_permission_prompt_notification() {
    let (actor, hooks, local) = setup_actor(/*yolo=*/ false, /*park_until_hook=*/ true).await;
    local
        .run_until(async {
            let result = prepare(&actor, edit_call("call_unsafe")).await;
            assert!(
                result.is_ok(),
                "prompted allow-once must prepare; got {:?}",
                result.err()
            );
            assert_eq!(
                hooks.load(Ordering::SeqCst),
                1,
                "a real user permission prompt must fire permission_prompt once"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn inherited_handle_second_wire_does_not_steal_parent_hook() {
    let (parent, hooks, local) = setup_actor(/*yolo=*/ false, /*park_until_hook=*/ true).await;
    local
        .run_until(async {
            let (child_gateway_tx, child_gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let mut child =
                create_test_actor(0, 256_000, 85, child_gateway_tx, persistence_tx).await;
            child.startup_hints.is_subagent = true;
            child.permissions = parent.permissions.clone();
            install_notification_client_hook(&child);
            let child = Arc::new(child);
            child.wire_permission_prompt_notification();
            let (steal_tx, _steal_rx) = tokio::sync::mpsc::unbounded_channel();
            child.permissions.set_user_prompt_notify(steal_tx);
            drop(child_gateway_rx);

            let result = prepare(&parent, edit_call("call_shared")).await;
            assert!(
                result.is_ok(),
                "prompted allow-once must prepare; got {:?}",
                result.err()
            );
            assert_eq!(
                hooks.load(Ordering::SeqCst),
                1,
                "parent hook must still fire once after a cloned handle tries to re-wire"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn yolo_tool_does_not_fire_permission_prompt_notification() {
    let (actor, hooks, local) = setup_actor(/*yolo=*/ true, /*park_until_hook=*/ false).await;
    local
        .run_until(async {
            let result = prepare(&actor, edit_call("call_yolo")).await;
            assert!(
                result.is_ok(),
                "yolo must auto-allow; got {:?}",
                result.err()
            );
            drain_permission_prompt_hook_chain().await;
            assert_eq!(
                hooks.load(Ordering::SeqCst),
                0,
                "yolo auto-approve must not fire permission_prompt"
            );
        })
        .await;
}

fn pre_tool_use_registry(script: &str) -> echo_agent_hooks::discovery::HookRegistry {
    let (mut registry, _) = echo_agent_hooks::discovery::load_hooks(None, None);
    registry.append_specs(vec![echo_agent_hooks::config::HookSpec {
        name: "test/pretooluse".into(),
        event: echo_agent_hooks::event::HookEventName::PreToolUse,
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
async fn pre_tool_use_updated_input_rewrites_prepared_call() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let hook_gateway = AcpAgentGatewaySender::new(gateway_tx.clone());
            let (persistence_tx, _persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            *actor.agent.borrow_mut() = test_agent_with_tools(edit_toolset()).await;
            actor.hook_resolved_workspace_root = "/tmp".to_string();
            install_real_permissions(&mut actor, /*yolo=*/ true, hook_gateway);
            *actor.hook_registry.borrow_mut() = Some(std::sync::Arc::new(pre_tool_use_registry(
                r#"echo '{"hookSpecificOutput":{"updatedInput":{"target_file":"/tmp/rewritten.txt"}}}'"#,
            )));
            let actor = Arc::new(actor);
            spawn_gateway_loop(gateway_rx, Arc::new(AtomicUsize::new(0)), false);

            let prepared = prepare(&actor, read_call("call_rewrite"))
                .await
                .expect("hook rewrite must prepare");
            assert_eq!(
                prepared.parsed_args["target_file"], "/tmp/rewritten.txt",
                "hook updatedInput must replace the tool input; got {}",
                prepared.raw_arguments
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn pre_tool_use_invalid_updated_input_is_tool_parsing_error() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let hook_gateway = AcpAgentGatewaySender::new(gateway_tx.clone());
            let (persistence_tx, _persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            *actor.agent.borrow_mut() = test_agent_with_tools(edit_toolset()).await;
            actor.hook_resolved_workspace_root = "/tmp".to_string();
            install_real_permissions(&mut actor, /*yolo=*/ true, hook_gateway);
            *actor.hook_registry.borrow_mut() = Some(std::sync::Arc::new(pre_tool_use_registry(
                r#"echo '{"hookSpecificOutput":{"updatedInput":{"target_file":123}}}'"#,
            )));
            let actor = Arc::new(actor);
            spawn_gateway_loop(gateway_rx, Arc::new(AtomicUsize::new(0)), false);

            let result = prepare(&actor, read_call("call_bad_rewrite")).await;
            assert!(
                matches!(result, Err(ToolLoop::ToolParsingError)),
                "an invalid hook updatedInput must surface as a parse error, got {result:?}"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn pre_tool_use_updated_input_reflected_in_permission_prompt() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let hook_gateway = AcpAgentGatewaySender::new(gateway_tx.clone());
            let (persistence_tx, _persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            *actor.agent.borrow_mut() = test_agent_with_tools(edit_toolset()).await;
            actor.hook_resolved_workspace_root = "/tmp".to_string();
            install_real_permissions(&mut actor, /*yolo=*/ false, hook_gateway);
            *actor.hook_registry.borrow_mut() = Some(std::sync::Arc::new(pre_tool_use_registry(
                r#"echo '{"hookSpecificOutput":{"updatedInput":{"file_path":"/tmp/rewritten-edit.txt","old_string":"a","new_string":"b"}}}'"#,
            )));
            let actor = Arc::new(actor);

            let captured: Arc<std::sync::Mutex<Option<serde_json::Value>>> =
                Arc::new(std::sync::Mutex::new(None));
            let captured_loop = captured.clone();
            tokio::task::spawn_local(async move {
                let mut gateway_rx = gateway_rx;
                while let Some(msg) = gateway_rx.recv().await {
                    match msg {
                        echo_agent_acp::AcpClientMessage::RequestPermission(args) => {
                            *captured_loop.lock().unwrap() =
                                args.request.tool_call.fields.raw_input.clone();
                            let _ = args.response_tx.send(Ok(
                                acp::RequestPermissionResponse::new(
                                    acp::RequestPermissionOutcome::Selected(
                                        acp::SelectedPermissionOutcome::new(
                                            acp::PermissionOptionId::new("allow-once"),
                                        ),
                                    ),
                                ),
                            ));
                        }
                        echo_agent_acp::AcpClientMessage::SessionNotification(args) => {
                            let _ = args.response_tx.send(Ok(()));
                        }
                        _ => {}
                    }
                }
            });

            prepare(&actor, edit_call("call_perm_rewrite"))
                .await
                .expect("prompted allow-once must prepare");
            let raw = captured
                .lock()
                .unwrap()
                .clone()
                .expect("permission prompt must carry raw_input");
            assert!(
                raw.to_string().contains("/tmp/rewritten-edit.txt"),
                "permission prompt must reflect the hook's updatedInput, got {raw}"
            );
        })
        .await;
}
