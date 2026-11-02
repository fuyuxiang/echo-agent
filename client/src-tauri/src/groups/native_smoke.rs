//! Runs the real embedded Runtime against an isolated loopback model. No user
//! data, provider credentials, or paid inference is used by this test.
use super::*;
use axum::{extract::State as AxumState, response::IntoResponse, routing::post, Json, Router};
use std::sync::{Arc, Mutex};

// Several inference/background requests can contain the same history. This
// event stays set after delivery so every request observes the acknowledgement.
static PROBE_INTERJECTION_QUEUED: std::sync::LazyLock<tokio::sync::watch::Sender<bool>> =
    std::sync::LazyLock::new(|| tokio::sync::watch::channel(false).0);

#[test]
fn native_runtime_group_contract() {
    let home = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "groups::native_smoke::isolated_native_runtime",
            "--exact",
            "--nocapture",
        ])
        .env("ECHO_GROUP_NATIVE_TEST", "1")
        .env("ECHO_AGENT_HOME", home.path())
        .env("GROK_HOME", home.path().join("empty-legacy"))
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("ALL_PROXY")
        .env_remove("http_proxy")
        .env_remove("https_proxy")
        .env_remove("all_proxy")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Native group integration failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn model(
    AxumState(requests): AxumState<Arc<Mutex<Vec<Value>>>>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    requests.lock().unwrap().push(body.clone());
    let request_text = body.get("messages").unwrap_or(&Value::Null).to_string();
    let messages = body.get("messages").and_then(Value::as_array);
    let latest_user = messages
        .and_then(|messages| {
            messages
                .iter()
                .rev()
                .find(|message| message["role"] == "user")
        })
        .map(|message| message["content"].to_string())
        .unwrap_or_default();
    let tool_reply = messages.and_then(|messages| {
        messages
            .iter()
            .rev()
            .take_while(|message| message["role"] != "user")
            .find(|message| message["role"] == "tool")
    });
    let policy_probe = if latest_user.contains("GROUP_MCP_ALLOWED_PROBE") {
        Some("echoagent__group_read_history")
    } else if latest_user.contains("GROUP_MCP_DISCOVERY_PROBE") {
        Some("search_tool")
    } else if latest_user.contains("GROUP_MCP_DENIED_PROBE") {
        Some("external__write_file")
    } else {
        None
    };
    if let Some(target) = policy_probe.filter(|_| tool_reply.is_none()) {
        let (name, arguments) = if target == "search_tool" {
            (
                "search_tool",
                json!({"query":"echoagent group_call_member","limit":10}),
            )
        } else {
            ("use_tool", json!({"tool_name":target,"tool_input":{}}))
        };
        let call = json!({"id":format!("policy-{target}"),"type":"function","function":{"name":name,"arguments":arguments.to_string()}});
        if body.get("stream").and_then(Value::as_bool) == Some(true) {
            let mut delta_call = call.clone();
            delta_call["index"] = json!(0);
            let first = json!({"id":"policy","object":"chat.completion.chunk","created":1,"model":"mock","choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[delta_call]},"finish_reason":null}]});
            let last = json!({"id":"policy","object":"chat.completion.chunk","created":1,"model":"mock","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]});
            return (
                [("content-type", "text/event-stream")],
                format!("data: {first}\n\ndata: {last}\n\ndata: [DONE]\n\n"),
            )
                .into_response();
        }
        return Json(json!({"id":"policy","object":"chat.completion","created":1,"model":"mock","choices":[{"index":0,"message":{"role":"assistant","tool_calls":[call]},"finish_reason":"tool_calls"}]})).into_response();
    }
    let response = if let Some(target) = policy_probe {
        let result = tool_reply
            .map(|message| message.to_string())
            .unwrap_or_default();
        if target == "echoagent__group_read_history" && result.contains("本群可见的目标") {
            "READ_ONLY_TOOL_PROBE_OK"
        } else if target == "search_tool" && result.contains("echoagent__group_call_member") {
            "READ_ONLY_DISCOVERY_OK"
        } else if target == "external__write_file"
            && result.contains("not permitted in this read-only session")
        {
            "READ_ONLY_TOOL_DENIED_OK"
        } else {
            "READ_ONLY_TOOL_PROBE_FAILED"
        }
    } else if request_text.contains("INTERJECTION_PROBE") {
        if request_text.contains("INTERJECTION_CORRECTION") {
            "CORRECTION_APPLIED"
        } else {
            let mut queued = PROBE_INTERJECTION_QUEUED.subscribe();
            tokio::time::timeout(
                std::time::Duration::from_secs(15),
                queued.wait_for(|queued| *queued),
            )
            .await
            .expect("host did not deliver the scoped interjection")
            .expect("interjection acknowledgement channel closed");
            "INITIAL_PASS"
        }
    } else if request_text.contains("MEMORY_PROBE") {
        if request_text.contains("MEMORY_PREFERENCE_BETA") {
            "MEMORY_APPLIED_BETA"
        } else if request_text.contains("MEMORY_PREFERENCE_ALPHA") {
            "MEMORY_APPLIED_ALPHA"
        } else {
            "MEMORY_EMPTY"
        }
    } else {
        "原生群聊执行成功"
    };
    if body.get("stream").and_then(Value::as_bool) == Some(true) {
        let first = json!({"id":"group-test","object":"chat.completion.chunk","created":1,"model":"mock","choices":[{"index":0,"delta":{"role":"assistant","content":response},"finish_reason":null}]});
        let last = json!({"id":"group-test","object":"chat.completion.chunk","created":1,"model":"mock","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":12,"completion_tokens":6,"total_tokens":18}});
        (
            [("content-type", "text/event-stream")],
            format!("data: {first}\n\ndata: {last}\n\ndata: [DONE]\n\n"),
        )
            .into_response()
    } else {
        Json(json!({"id":"group-test","object":"chat.completion","created":1,"model":"mock","choices":[{"index":0,"message":{"role":"assistant","content":response},"finish_reason":"stop"}],"usage":{"prompt_tokens":12,"completion_tokens":6,"total_tokens":18}})).into_response()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn isolated_native_runtime() {
    if std::env::var("ECHO_GROUP_NATIVE_TEST").as_deref() != Ok("1") {
        return;
    }
    let requests = Arc::new(Mutex::new(vec![]));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let router = Router::new()
        .route("/v1/chat/completions", post(model))
        .with_state(requests.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let home = crate::paths::echo_agent_home_dir();
    std::fs::create_dir_all(&home).unwrap();
    let plugin = tempfile::Builder::new()
        .prefix(".echo-plugin-removal-test-")
        .tempdir_in(std::env::current_dir().unwrap())
        .unwrap();
    std::fs::write(
        plugin.path().join("plugin.json"),
        r#"{"name":"relative-removal-regression"}"#,
    )
    .unwrap();
    let relative_plugin = plugin
        .path()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    std::fs::write(
        home.join("config.toml"),
        format!(
            r#"
[models]
default = "group-test/mock"
[model_providers.group-test]
provider_kind = "custom"
base_url = "http://127.0.0.1:{port}/v1"
api_key = "isolated-local-model"
api_backend = "chat_completions"
auth_scheme = "bearer"
context_window = 131072
[model."group-test/mock"]
model = "mock"
model_provider = "group-test"
name = "Group integration model"
max_completion_tokens = 1024
[memory]
enabled = false
[memory.initial_injection]
enabled = false
[model."group-test/reviewer"]
model = "reviewer"
model_provider = "group-test"
name = "Independent reviewer model"
max_completion_tokens = 1024
[plugins]
paths = ["{relative_plugin}"]
"#
        ),
    )
    .unwrap();
    let workspace = tempfile::tempdir().unwrap();
    crate::team_mcp::serve();
    let handle = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        tokio::task::spawn_blocking({
            let cwd = workspace.path().to_path_buf();
            move || crate::agent_runtime::spawn_agent_runtime(cwd)
        }),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    let crate::agent_runtime::AgentHandle {
        tx,
        mut rx,
        cancel,
        mut shutdown_complete,
        ..
    } = handle;
    let chunks = Arc::new(Mutex::new(String::new()));
    let captured = chunks.clone();
    let dispatcher = tokio::spawn(async move {
        use echo_agent_acp::AcpClientMessage;
        while let Some(message) = rx.recv().await {
            match message {
                AcpClientMessage::SessionNotification(args) => {
                    if let Ok(value) = serde_json::to_value(&args.request.update) {
                        let meta = args
                            .request
                            .meta
                            .as_ref()
                            .and_then(|m| serde_json::to_value(m).ok());
                        super::observe(args.request.session_id.0.as_ref(), &value, meta.as_ref());
                        if let Some(text) = value.pointer("/content/text").and_then(Value::as_str) {
                            captured.lock().unwrap().push_str(text);
                        }
                    }
                    let _ = args.response_tx.send(Ok(()));
                }
                AcpClientMessage::ExtNotification(args) => {
                    if let Ok(params) = serde_json::from_str::<Value>(args.request.params.get()) {
                        if params
                            .get("text")
                            .and_then(Value::as_str)
                            .is_some_and(|text| text.contains("INTERJECTION_CORRECTION"))
                            && params.get("interjectionId").is_some()
                        {
                            PROBE_INTERJECTION_QUEUED.send_replace(true);
                        }
                        if let (Some(session), Some(update)) = (
                            params.get("sessionId").and_then(Value::as_str),
                            params.get("update"),
                        ) {
                            super::observe(session, update, params.get("_meta"));
                        }
                    }
                    let _ = args.response_tx.send(Ok(()));
                }
                AcpClientMessage::ExtMethod(args) => {
                    let _ = args
                        .response_tx
                        .send(Ok(agent_client_protocol::ExtResponse::new(
                            crate::ext::raw_params(&json!({})),
                        )));
                }
                AcpClientMessage::RequestPermission(args) => {
                    let request = serde_json::to_value(&args.request).unwrap();
                    assert_ne!(
                        request
                            .pointer("/toolCall/rawInput/tool_name")
                            .and_then(Value::as_str),
                        Some("external__write_file"),
                        "Forbidden MCP calls must be rejected before approval"
                    );
                    // Simulate the existing UI's explicit one-time approval for
                    // this local read; all unrelated native actions stay denied.
                    let outcome = if request
                        .pointer("/toolCall/rawInput/tool_name")
                        .and_then(Value::as_str)
                        == Some("echoagent__group_read_history")
                    {
                        agent_client_protocol::RequestPermissionOutcome::Selected(
                            agent_client_protocol::SelectedPermissionOutcome::new(
                                agent_client_protocol::PermissionOptionId::new("allow-once"),
                            ),
                        )
                    } else {
                        agent_client_protocol::RequestPermissionOutcome::Cancelled
                    };
                    let _ = args.response_tx.send(Ok(
                        agent_client_protocol::RequestPermissionResponse::new(outcome),
                    ));
                }
                _ => panic!("Native group unexpectedly requested host filesystem/terminal IO"),
            }
        }
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        crate::agent_runtime::initialize(&tx),
    )
    .await
    .unwrap()
    .unwrap();
    let profile = json!({"name":echo_agent_core::AgentDefinition::default_echo_agent_build().name,"description":"native test expert","promptMode":"extend","promptBody":"GROUP_ROLE_MUST_SURVIVE_RESTORE"});
    let store = Store::open(&home.join("collaboration.sqlite")).unwrap();
    let mut group = super::tests::group();
    group.cwd = workspace.path().to_string_lossy().into_owned();
    let gid = group.id.clone();
    let member = group.coordinator_id.clone();
    group.post(None, "user", "本群可见的目标".into(), None);
    let work = group
        .enqueue("root", &member, "请确认身份".into(), true, None, 0)
        .unwrap();
    store.create(&group).unwrap();
    assert!(STORE.set(store).is_ok());
    // Runtime fetches tools before a newly allocated session can be bound in
    // SQLite. Authenticated group/member headers must already select group tools.
    let client = reqwest::Client::new();
    let catalog: Value = client
        .post(crate::team_mcp::server_url().unwrap())
        .header(
            crate::team_mcp::AUTH_HEADER,
            crate::team_mcp::authorization_header().unwrap(),
        )
        .header("X-EchoAgent-Group-Id", &gid)
        .header("X-EchoAgent-Member-Id", &member)
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(catalog["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["name"] == "group_call_member"));
    assert!(!catalog["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["name"] == "create_team"));
    let sid = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        crate::agent_runtime::group_session(
            &tx,
            crate::agent_runtime::GroupSessionRequest {
                cwd: workspace.path(),
                model: "group-test/mock",
                profile: &profile,
                group_id: &gid,
                member_id: &member,
                generation: 0,
                discussion: true,
                existing: None,
            },
        ),
    )
    .await
    .unwrap()
    .unwrap();
    db().unwrap()
        .update(&gid, |g| {
            g.members[0].discussion_session_id = Some(sid.clone());
            let w = g.works.iter_mut().find(|w| w.id == work).unwrap();
            w.session_id = Some(sid.clone());
            w.prompt_id = "prompt-1".into();
            w.status = "running".into();
            Ok(())
        })
        .unwrap();
    remember_owner(&sid, &gid, &member);
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        crate::agent_runtime::group_prompt(&tx, &sid, "prompt-1", "请确认身份", &[]),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!matches!(
        result.stop_reason.as_str(),
        "error" | "rate_limit"
    ));
    assert!(chunks.lock().unwrap().contains("原生群聊执行成功"));
    super::runtime::flush(&sid).unwrap();
    let saved = db().unwrap().get(&gid).unwrap();
    assert!(saved.works[0].output.contains("原生群聊执行成功"));
    let history: Value = client.post(crate::team_mcp::server_url().unwrap())
        .header(crate::team_mcp::AUTH_HEADER, crate::team_mcp::authorization_header().unwrap())
        .header(crate::team_mcp::SESSION_HEADER, &sid)
        .json(&json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"group_read_history","arguments":{}}}))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(history["result"]["isError"], false);
    assert!(history.to_string().contains("本群可见的目标"));
    for (prompt_id, probe, expected) in [
        (
            "policy-discovery",
            "GROUP_MCP_DISCOVERY_PROBE",
            "READ_ONLY_DISCOVERY_OK",
        ),
        (
            "policy-read",
            "GROUP_MCP_ALLOWED_PROBE",
            "READ_ONLY_TOOL_PROBE_OK",
        ),
        (
            "policy-deny",
            "GROUP_MCP_DENIED_PROBE",
            "READ_ONLY_TOOL_DENIED_OK",
        ),
    ] {
        db().unwrap()
            .update(&gid, |group| {
                group
                    .works
                    .iter_mut()
                    .find(|item| item.id == work)
                    .unwrap()
                    .prompt_id = prompt_id.into();
                Ok(())
            })
            .unwrap();
        let completion = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            crate::agent_runtime::group_prompt(&tx, &sid, prompt_id, probe, &[]),
        )
        .await
        .unwrap()
        .unwrap();
        let captured = chunks.lock().unwrap().clone();
        let last_tool_result = requests
            .lock()
            .unwrap()
            .last()
            .and_then(|request| request.get("messages").and_then(Value::as_array))
            .and_then(|messages| {
                messages
                    .iter()
                    .rev()
                    .find(|message| message["role"] == "tool")
            })
            .cloned();
        assert!(
            captured.contains(expected),
            "Native read-only MCP probe {probe} failed ({}): {captured}; tool result: {last_tool_result:?}", completion.stop_reason
        );
    }
    db().unwrap()
        .update(&gid, |g| {
            g.finish_execution(&work, None, false);
            let snapshot = g.clone().snapshot();
            assert_eq!(
                snapshot
                    .requests
                    .iter()
                    .find(|request| request.id == "root")
                    .unwrap()
                    .state,
                "delivered"
            );
            assert!(snapshot
                .group
                .messages
                .iter()
                .any(|message| message.kind == "assistant"
                    && message.text.contains("原生群聊执行成功")));
            let next = g.enqueue("root-2", &member, "继续确认".into(), true, None, 0)?;
            let w = g.works.iter_mut().find(|w| w.id == next).unwrap();
            w.session_id = Some(sid.clone());
            w.prompt_id = "prompt-2".into();
            w.status = "running".into();
            Ok(())
        })
        .unwrap();
    // Drop the actor so the next load exercises disk restore, not a resident
    // session that already happened to have the correct expert definition.
    let closed: Value = crate::ext::call_ext(
        &tx,
        "echo.agent/session/close",
        crate::ext::raw_params(&json!({"sessionId":sid})),
    )
    .await
    .unwrap();
    assert_eq!(closed["outcome"], "closed");
    crate::agent_runtime::group_session(
        &tx,
        crate::agent_runtime::GroupSessionRequest {
            cwd: workspace.path(),
            model: "group-test/mock",
            profile: &profile,
            group_id: &gid,
            member_id: &member,
            generation: 0,
            discussion: true,
            existing: Some(&sid),
        },
    )
    .await
    .unwrap();
    crate::agent_runtime::group_prompt(&tx, &sid, "prompt-2", "继续确认身份", &[])
        .await
        .unwrap();
    {
        let calls = requests.lock().unwrap();
        assert!(
            calls.iter().any(|call| {
                let request = call.to_string();
                request.contains("继续确认身份")
                    && request.contains("GROUP_ROLE_MUST_SURVIVE_RESTORE")
            }),
            "The restored model turn must retain the expert role"
        );
        let role_calls = calls
            .iter()
            .filter(|request| {
                request
                    .to_string()
                    .contains("GROUP_ROLE_MUST_SURVIVE_RESTORE")
            })
            .collect::<Vec<_>>();
        assert!(
            role_calls.len() >= 2,
            "Expert role was lost: {} model calls",
            calls.len()
        );
        assert!(
            role_calls.iter().any(
                |call| call
                    .get("tools")
                    .and_then(Value::as_array)
                    .is_some_and(|tools| tools.iter().any(|tool| tool
                        .pointer("/function/name")
                        .and_then(Value::as_str)
                        .is_some_and(|name| name.ends_with("use_tool"))))
            ),
            "Read-only discussions must retain controlled MCP dispatch"
        );
        for call in role_calls {
            for tool in call
                .get("tools")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let name = tool
                    .pointer("/function/name")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let name = name
                    .rsplit("__")
                    .next()
                    .unwrap_or(name)
                    .rsplit(':')
                    .next()
                    .unwrap_or(name);
                assert!(
                    !matches!(
                        name,
                        "run_terminal_command" | "edit_file" | "write_file" | "task"
                    ),
                    "Discussion exposed mutating tool {name}"
                );
            }
        }
    }
    // Execution keeps ordinary Runtime capabilities, while the host owns every
    // multi-agent dispatch and its lifetime. Native goals/workflows/subagents
    // must not create invisible background work outside the group work graph.
    let executor = group.members[1].id.clone();
    let execution_sid = crate::agent_runtime::group_session(
        &tx,
        crate::agent_runtime::GroupSessionRequest {
            cwd: workspace.path(),
            model: "group-test/mock",
            profile: &profile,
            group_id: &gid,
            member_id: &executor,
            generation: 0,
            discussion: false,
            existing: None,
        },
    )
    .await
    .unwrap();
    db().unwrap()
        .update(&gid, |g| {
            g.members[1].session_id = Some(execution_sid.clone());
            let id = g.enqueue(
                "execution-root",
                &executor,
                "确认执行身份".into(),
                false,
                None,
                0,
            )?;
            let work = g.works.iter_mut().find(|w| w.id == id).unwrap();
            work.session_id = Some(execution_sid.clone());
            work.prompt_id = "execution-prompt".into();
            work.status = "running".into();
            Ok(())
        })
        .unwrap();
    remember_owner(&execution_sid, &gid, &executor);
    crate::agent_runtime::group_prompt(
        &tx,
        &execution_sid,
        "execution-prompt",
        "确认执行身份",
        &[],
    )
    .await
    .unwrap();
    let execution_call = requests
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|request| {
            let body = request.to_string();
            body.contains("GROUP_ROLE_MUST_SURVIVE_RESTORE") && body.contains("确认执行身份")
        })
        .cloned()
        .expect("The execution profile never reached the real Runtime model request");
    assert!(execution_call
        .to_string()
        .contains("GROUP_ROLE_MUST_SURVIVE_RESTORE"));
    let tool_names = execution_call
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tool| tool.pointer("/function/name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert!(
        tool_names
            .iter()
            .any(|name| name.ends_with("use_tool") || name.ends_with("group_call_member")),
        "Group members cannot reach the collaboration MCP tools: {tool_names:?}"
    );
    for tool in execution_call
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = tool
            .pointer("/function/name")
            .and_then(Value::as_str)
            .unwrap_or("");
        let name = name
            .rsplit("__")
            .next()
            .unwrap_or(name)
            .rsplit(':')
            .next()
            .unwrap_or(name);
        assert!(
            !matches!(name, "task" | "Agent" | "workflow" | "update_goal"),
            "Group execution exposed a second background scheduler: {name}"
        );
    }
    // Seed an explicitly confirmed preference through the same record API used
    // by memory_manage, then observe its arrival in real native model requests.
    // The loopback response proves delivery/return plumbing, not model quality.
    use echo_agent_memory::{records, MemoryStorage};
    let memory_root = home
        .join("group-memory")
        .join(&gid)
        .join(&executor)
        .join("generation-0");
    let storage = MemoryStorage::new_flat(workspace.path(), &memory_root);
    let record = records::remember(
        &storage,
        records::RecordInput {
            id: None,
            scope: "workspace".into(),
            kind: "feedback".into(),
            key: "交付偏好".into(),
            content: "MEMORY_PREFERENCE_ALPHA".into(),
            evidence: "用户明确确认".into(),
            pinned: true,
            valid_until: None,
            expected_revision: None,
            observed_at: None,
        },
        None,
        false,
    )
    .unwrap()
    .expect("explicit scoped memory must be saved");
    let probe = crate::agent_runtime::GroupSessionRequest {
        cwd: workspace.path(),
        model: "group-test/reviewer",
        profile: &profile,
        group_id: &gid,
        member_id: &executor,
        generation: 0,
        discussion: false,
        existing: None,
    };
    let memory_sid = memory_probe(&tx, probe, "memory-1").await;
    let _: Value = crate::ext::call_ext(&tx, "echo.agent/interject", crate::ext::raw_params(&json!({
        "sessionId":memory_sid,"expectedPromptId":"already-finished", "text":"UNTRACKED_LATE_INTERJECTION", "interjectionId":"late"
    }))).await.unwrap();
    assert_eq!(
        db().unwrap()
            .get(&gid)
            .unwrap()
            .works
            .last()
            .unwrap()
            .output,
        "MEMORY_APPLIED_ALPHA"
    );
    let _: Value = crate::ext::call_ext(
        &tx,
        "echo.agent/session/close",
        crate::ext::raw_params(&json!({"sessionId":memory_sid})),
    )
    .await
    .unwrap();
    memory_probe(
        &tx,
        crate::agent_runtime::GroupSessionRequest {
            cwd: workspace.path(),
            model: "group-test/reviewer",
            profile: &profile,
            group_id: &gid,
            member_id: &executor,
            generation: 0,
            discussion: false,
            existing: Some(&memory_sid),
        },
        "memory-restored",
    )
    .await;
    assert!(requests
        .lock()
        .unwrap()
        .iter()
        .any(|r| r["model"] == "reviewer"
            && r["messages"]
                .to_string()
                .contains("MEMORY_PREFERENCE_ALPHA")));
    let corrected = records::remember(
        &storage,
        records::RecordInput {
            id: Some(record.id),
            scope: record.scope,
            kind: record.kind,
            key: record.key,
            content: "MEMORY_PREFERENCE_BETA".into(),
            evidence: "用户明确纠正".into(),
            pinned: true,
            valid_until: None,
            expected_revision: Some(record.revision),
            observed_at: None,
        },
        None,
        false,
    )
    .unwrap()
    .expect("explicit scoped memory must be saved");
    // New model, new actor, same member memory namespace.
    memory_probe(
        &tx,
        crate::agent_runtime::GroupSessionRequest {
            cwd: workspace.path(),
            model: "group-test/mock",
            profile: &profile,
            group_id: &gid,
            member_id: &executor,
            generation: 0,
            discussion: false,
            existing: None,
        },
        "memory-corrected",
    )
    .await;
    assert_eq!(
        db().unwrap()
            .get(&gid)
            .unwrap()
            .works
            .last()
            .unwrap()
            .output,
        "MEMORY_APPLIED_BETA"
    );
    records::retire(
        &storage,
        "workspace",
        &corrected.id,
        Some(corrected.revision),
        false,
    )
    .unwrap();
    memory_probe(
        &tx,
        crate::agent_runtime::GroupSessionRequest {
            cwd: workspace.path(),
            model: "group-test/mock",
            profile: &profile,
            group_id: &gid,
            member_id: &executor,
            generation: 0,
            discussion: false,
            existing: None,
        },
        "memory-forgotten",
    )
    .await;
    assert_eq!(
        db().unwrap()
            .get(&gid)
            .unwrap()
            .works
            .last()
            .unwrap()
            .output,
        "MEMORY_EMPTY"
    );
    memory_probe(
        &tx,
        crate::agent_runtime::GroupSessionRequest {
            cwd: workspace.path(),
            model: "group-test/mock",
            profile: &profile,
            group_id: &gid,
            member_id: &member,
            generation: 0,
            discussion: true,
            existing: None,
        },
        "memory-other-member",
    )
    .await;
    assert_eq!(
        db().unwrap()
            .get(&gid)
            .unwrap()
            .works
            .last()
            .unwrap()
            .output,
        "MEMORY_EMPTY"
    );
    assert!(crate::sessions::list_workspaces().is_empty(),
        "This isolated Runtime only created group sessions; none may appear in ordinary workspace counts");
    assert!(
        !requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.to_string().contains("UNTRACKED_LATE_INTERJECTION")),
        "a late scoped interjection created an untracked native turn"
    );
    followup_contract(&tx, &gid, &executor, &execution_sid, &requests).await;
    // Exercise the real Runtime's permanent-history deletion, including
    // retired/restored native contexts, before removing the group transaction.
    db().unwrap()
        .update(&gid, |g| {
            g.paused = true;
            let ids = g
                .works
                .iter()
                .filter(|w| !w.terminal())
                .map(|w| w.id.clone())
                .collect::<Vec<_>>();
            for id in ids {
                g.finish(&id, None, false);
            }
            Ok(())
        })
        .unwrap();
    // History is owned by the group even when the source workspace no longer exists.
    std::fs::remove_dir_all(workspace.path()).unwrap();
    let native_sessions = db().unwrap().sessions(&gid).unwrap();
    assert!(native_sessions.len() >= 2);
    let removed: Value = crate::ext::call_ext(
        &tx,
        "echo.agent/plugins/action",
        crate::ext::raw_params(
            &json!({"sessionId":sid,"action":{"type":"remove","path":relative_plugin}}),
        ),
    )
    .await
    .unwrap();
    assert_eq!(removed["status"], "success");
    let config: toml::Value = std::fs::read_to_string(home.join("config.toml"))
        .unwrap()
        .parse()
        .unwrap();
    assert!(!config["plugins"]["paths"]
        .as_array()
        .unwrap()
        .iter()
        .any(|path| path.as_str() == Some(&relative_plugin)));
    assert!(
        plugin.path().join("plugin.json").exists(),
        "removing configuration must preserve its source"
    );
    let mut physical_paths = vec![];
    for native in &native_sessions {
        let paths =
            echo_agent_runtime::session::persistence::session_history_paths(native, None).unwrap();
        assert!(!paths.is_empty());
        // Deletion must work without catalog metadata, after a real restart's
        // in-memory workspace map would have disappeared.
        for path in &paths {
            std::fs::write(path.join("summary.json"), "{broken").unwrap();
        }
        physical_paths.extend(paths);
    }
    // Exercise the exact implementation used by the Tauri command with an
    // empty in-memory workspace map, as after an application restart.
    let state = crate::commands::AppState::default();
    *state.tx.lock().unwrap() = Some(tx.clone());
    let deleted = delete_group_data(&state, gid.clone(), true).await.unwrap();
    assert_eq!(deleted.session_ids.len(), native_sessions.len());
    assert!(physical_paths.iter().all(|path| !path.exists()));
    assert_eq!(
        db().unwrap().pending_cleanup().unwrap()[0]
            .session_ids
            .len(),
        native_sessions.len()
    );
    for native in &native_sessions {
        assert!(
            !crate::sessions::persisted_session_exists(native).unwrap(),
            "native session survived deletion: {native}"
        );
    }
    assert!(db().unwrap().get(&gid).is_err());
    for native in &native_sessions {
        assert!(db().unwrap().owner(native).unwrap().is_none());
    }
    // Exact-ID cleanup also removes directory-only remnants and duplicate
    // historical copies, without relying on a valid encoded workspace/summary.
    let orphan_id = id();
    let orphan_paths = [
        home.join("sessions/orphan-a").join(&orphan_id),
        home.join("sessions/orphan-b").join(&orphan_id),
    ];
    for path in &orphan_paths {
        std::fs::create_dir_all(path).unwrap();
        std::fs::write(path.join("updates.jsonl"), "private orphan history").unwrap();
    }
    std::fs::write(orphan_paths[0].join("summary.json"), "{broken").unwrap();
    crate::agent_runtime::delete_session(&tx, &orphan_id, None)
        .await
        .unwrap();
    assert!(orphan_paths.iter().all(|path| !path.exists()));
    #[cfg(unix)]
    {
        let protected = home.join("protected-source");
        std::fs::create_dir_all(&protected).unwrap();
        std::fs::write(protected.join("keep.txt"), "preserve this source").unwrap();
        let redirected_id = id();
        let redirected = home.join("sessions/orphan-a").join(&redirected_id);
        std::os::unix::fs::symlink(&protected, &redirected).unwrap();
        assert!(
            crate::agent_runtime::delete_session(&tx, &redirected_id, None)
                .await
                .is_err()
        );
        assert!(protected.join("keep.txt").exists());
    }
    cancel.cancel();
    tokio::time::timeout(
        std::time::Duration::from_secs(20),
        shutdown_complete.wait_for(|done| *done),
    )
    .await
    .unwrap()
    .unwrap();
    dispatcher.abort();
    server.abort();
}

async fn memory_probe(
    tx: &echo_agent_acp::AcpAgentTx,
    request: crate::agent_runtime::GroupSessionRequest<'_>,
    prompt: &str,
) -> String {
    let gid = request.group_id.to_string();
    let member = request.member_id.to_string();
    let model = request.model.to_string();
    let discussion = request.discussion;
    let sid = crate::agent_runtime::group_session(tx, request)
        .await
        .unwrap();
    db().unwrap()
        .update(&gid, |g| {
            let pending = g
                .works
                .iter()
                .filter(|w| !w.terminal())
                .map(|w| w.id.clone())
                .collect::<Vec<_>>();
            for id in pending {
                g.finish(&id, None, false);
            }
            let m = g.members.iter_mut().find(|m| m.id == member).unwrap();
            m.model_id = model;
            if discussion {
                m.discussion_session_id = Some(sid.clone());
            } else {
                m.session_id = Some(sid.clone());
            }
            let id = g.enqueue(prompt, &member, "MEMORY_PROBE".into(), discussion, None, 0)?;
            let w = g.works.iter_mut().find(|w| w.id == id).unwrap();
            w.status = "running".into();
            w.session_id = Some(sid.clone());
            w.prompt_id = prompt.into();
            Ok(())
        })
        .unwrap();
    remember_owner(&sid, &gid, &member);
    let g = db().unwrap().get(&gid).unwrap();
    let text = runtime::work_prompt(
        &g,
        g.works.last().unwrap(),
        g.members.iter().find(|m| m.id == member).unwrap(),
        &Knowledge::default(),
    )
    .unwrap();
    crate::agent_runtime::group_prompt(tx, &sid, prompt, &text, &[])
        .await
        .unwrap();
    runtime::flush(&sid).unwrap();
    sid
}

async fn followup_contract(
    tx: &echo_agent_acp::AcpAgentTx,
    gid: &str,
    member: &str,
    sid: &str,
    requests: &Arc<Mutex<Vec<Value>>>,
) {
    let root = id();
    let work = db()
        .unwrap()
        .update(gid, |g| {
            let pending = g
                .works
                .iter()
                .filter(|w| !w.terminal())
                .map(|w| w.id.clone())
                .collect::<Vec<_>>();
            for work in pending {
                g.finish(&work, None, false);
            }
            send(
                g,
                &SendInput {
                    new_request: false,
                    request_id: None,
                    id: root.clone(),
                    text: "INTERJECTION_PROBE".into(),
                    member_ids: vec![member.into()],
                    discussion: false,
                    attachments: vec![],
                    reply_to: None,
                    knowledge: Knowledge::default(),
                },
            )?;
            let context = g.messages.last().unwrap().seq;
            let work = g.works.last_mut().unwrap();
            work.status = "running".into();
            work.session_id = Some(sid.into());
            work.context_seq = context;
            Ok(work.clone())
        })
        .unwrap();
    let task = tokio::spawn({
        let tx = tx.clone();
        let sid = sid.to_owned();
        let prompt = work.prompt_id.clone();
        async move {
            crate::agent_runtime::group_prompt(&tx, &sid, &prompt, "INTERJECTION_PROBE", &[])
                .await
                .unwrap()
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if requests
                .lock()
                .unwrap()
                .iter()
                .any(|r| r.to_string().contains("INTERJECTION_PROBE"))
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let correction = id();
    db().unwrap()
        .update(gid, |g| {
            send(
                g,
                &SendInput {
                    new_request: false,
                    request_id: None,
                    id: correction.clone(),
                    text: "INTERJECTION_CORRECTION".into(),
                    member_ids: vec![member.into()],
                    discussion: false,
                    attachments: vec![],
                    reply_to: Some(root.clone()),
                    knowledge: Knowledge::default(),
                },
            )
        })
        .unwrap();
    let latest = db().unwrap().get(gid).unwrap();
    let message = latest.messages.iter().find(|m| m.id == correction).unwrap();
    assert_eq!(message.request_id.as_deref(), Some(root.as_str()));
    super::deliver_followup(tx, &latest, message, &root)
        .await
        .unwrap();
    task.await.unwrap();
    runtime::flush(sid).unwrap();
    let completed = db()
        .unwrap()
        .update(gid, |g| {
            let work = g.works.iter().find(|w| w.id == work.id).unwrap();
            assert!(
                work.output.contains("CORRECTION_APPLIED"),
                "correction did not reach actual inference: {}",
                work.output
            );
            assert!(
                work.consumed_followups.contains(&correction),
                "native safe-point consumption was not acknowledged"
            );
            let id = work.id.clone();
            g.finish(&id, None, false);
            Ok(g.works.iter().find(|w| w.id == id).unwrap().clone())
        })
        .unwrap();
    assert_eq!(completed.status, "completed");
    assert!(
        !db()
            .unwrap()
            .get(gid)
            .unwrap()
            .works
            .iter()
            .any(|w| w.continuation_of.as_deref() == Some(&completed.id)),
        "already consumed correction started another host turn"
    );
}
