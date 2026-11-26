//! End-to-end actor tests for the MCP "always allow" persistence path.
//!
//! Spawns a real `spawn_permission_manager` actor with a fake gateway whose
//! `request_permission` returns canned responses. Drives the actor through
//! the request → prompt → grant → re-request flow and verifies that the
//! state file on disk reflects the grant.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use agent_client_protocol as acp;
use echo_agent_acp::{AcpAgentGatewaySender, AcpClientMessage};
use echo_agent_paths::AbsPathBuf;
use echo_agent_workspace::permission::types::{
    PatternMode, PermissionConfig, PermissionRule, RuleAction, ToolFilter,
};
use echo_agent_workspace::permission::{
    AccessKind, ClientType, Decision, PermissionCommand, PermissionHandle, PermissionState,
    spawn_permission_manager, spawn_permission_manager_with_hub,
};
use serial_test::serial;
use tokio::sync::{mpsc, oneshot};

/// Shared `ECHO_AGENT_HOME` for the entire test binary. The `OnceLock` in
/// `echo-agent-config` only allows one value per process, so all tests share
/// this temp directory and `#[serial]` keeps them from clobbering each
/// other's state files.
fn test_home() -> &'static PathBuf {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.keep();
        // SAFETY: called once at init before other threads touch this var.
        unsafe { std::env::set_var("ECHO_AGENT_HOME", &path) };
        path
    })
}

fn fresh_cwd() -> AbsPathBuf {
    let home = test_home();
    let unique = format!(
        "test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let cwd = home.join(unique);
    std::fs::create_dir_all(&cwd).unwrap();
    AbsPathBuf::new(cwd).unwrap()
}

fn permission_state_path(cwd: &AbsPathBuf) -> PathBuf {
    test_home()
        .join("sessions")
        .join(urlencoding::encode(cwd.as_str()).into_owned())
        .join("permission.toml")
}

fn load_state(cwd: &AbsPathBuf) -> PermissionState {
    let path = permission_state_path(cwd);
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("permission state not yet written at {:?}", path));
    toml::from_str(&contents).unwrap()
}

fn tool_call_update(id: &str, name: &str) -> acp::ToolCallUpdate {
    acp::ToolCallUpdate::new(
        acp::ToolCallId::new(Arc::from(id)),
        acp::ToolCallUpdateFields::new()
            .kind(Some(acp::ToolKind::Other))
            .title(Some(name.to_owned())),
    )
}

fn make_session_id() -> acp::SessionId {
    acp::SessionId::new(Arc::from("test-session"))
}

/// Build a fake gateway plus a handle to enqueue scripted responses.
///
/// Each `expect_*` call queues one response; the gateway task pops them in
/// FIFO order as `RequestPermission` messages arrive.
struct FakeGateway {
    sender: AcpAgentGatewaySender,
    /// Queue of (option_id, optional response meta) pairs.
    expected: tokio::sync::mpsc::UnboundedSender<(String, Option<serde_json::Value>)>,
}

fn fake_gateway() -> (FakeGateway, tokio::task::JoinHandle<()>) {
    let (gw_tx, mut gw_rx) = mpsc::unbounded_channel::<AcpClientMessage>();
    let (script_tx, mut script_rx) =
        mpsc::unbounded_channel::<(String, Option<serde_json::Value>)>();

    let join = tokio::task::spawn_local(async move {
        while let Some(msg) = gw_rx.recv().await {
            if let AcpClientMessage::RequestPermission(args) = msg {
                let (option_id, meta) = script_rx
                    .recv()
                    .await
                    .expect("test ran out of scripted responses");
                let mut response = acp::RequestPermissionResponse::new(
                    acp::RequestPermissionOutcome::Selected(acp::SelectedPermissionOutcome::new(
                        acp::PermissionOptionId::new(Arc::from(option_id.as_str())),
                    )),
                );
                if let Some(m) = meta.and_then(|v| v.as_object().cloned()) {
                    response = response.meta(m);
                }
                let _ = args.response_tx.send(Ok(response));
            }
        }
    });

    let sender = AcpAgentGatewaySender::new(gw_tx);
    (
        FakeGateway {
            sender,
            expected: script_tx,
        },
        join,
    )
}

impl FakeGateway {
    fn expect_allow_always_mcp_tool(&self, tool_name: &str) {
        let meta = serde_json::json!({
            "kind": "tool",
            "tool_name": tool_name,
        });
        self.expected
            .send(("allow-always-mcp".to_string(), Some(meta)))
            .unwrap();
    }

    fn expect_allow_always_mcp_server(&self, server: &str) {
        let meta = serde_json::json!({
            "kind": "server",
            "server": server,
        });
        self.expected
            .send(("allow-always-mcp".to_string(), Some(meta)))
            .unwrap();
    }

    fn expect_plain_allow_always(&self) {
        // Legacy `"always-allow"` option id from `fallback_options`.
        self.expected
            .send(("always-allow".to_string(), None))
            .unwrap();
    }
}

async fn request(handle: &PermissionHandle, access: AccessKind, id: &str) -> Decision {
    let (tx, rx) = oneshot::channel();
    let cmd = PermissionCommand::Request {
        access,
        tool_call_update: tool_call_update(id, "mcp"),
        path_context: None,
        respond_to: tx,
        session_id: None,
        subagent_type: None,
        subagent_description: None,
    };
    let PermissionHandle::Actor { cmd_tx, .. } = handle else {
        panic!("expected actor handle");
    };
    cmd_tx.send(cmd).unwrap();
    rx.await.unwrap().decision
}

/// Build an MCP access kind from a tool name; these persistence tests only
/// exercise the name, so args are empty.
fn mcp(name: &str) -> AccessKind {
    AccessKind::MCPTool {
        name: name.to_string(),
        input: serde_json::Value::Null,
    }
}

async fn run_actor_test<F, Fut>(client_type: ClientType, body: F)
where
    F: FnOnce(PermissionHandle, FakeGateway, AbsPathBuf) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    run_actor_test_with_policy(client_type, None, body).await;
}

async fn run_actor_test_with_policy<F, Fut>(
    client_type: ClientType,
    policy: Option<PermissionConfig>,
    body: F,
) where
    F: FnOnce(PermissionHandle, FakeGateway, AbsPathBuf) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    run_actor_test_full(client_type, policy, false, body).await;
}

async fn run_actor_test_full<F, Fut>(
    client_type: ClientType,
    policy: Option<PermissionConfig>,
    initial_yolo: bool,
    body: F,
) where
    F: FnOnce(PermissionHandle, FakeGateway, AbsPathBuf) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let cwd = fresh_cwd();
            let (gw, _gw_task) = fake_gateway();
            let (handle, _events) = spawn_permission_manager(
                make_session_id(),
                gw.sender.clone(),
                cwd.clone(),
                client_type,
                policy,
                vec![], // deny_read_globs
                vec![],
                initial_yolo,
                None,
            );
            body(handle, gw, cwd).await;
        })
        .await;
}

fn rule(action: RuleAction, pattern: &str) -> PermissionRule {
    PermissionRule {
        action,
        tool: ToolFilter::Mcp,
        pattern: Some(pattern.to_owned()),
        pattern_mode: PatternMode::Glob,
    }
}

// --- mcp_pre_decision-style end-to-end ---

#[tokio::test]
#[serial]
async fn mcp_tool_grant_persists_and_short_circuits_next_request() {
    run_actor_test(ClientType::EchoAgentPager, |handle, gw, cwd| async move {
        // First request prompts; user picks tool-scope.
        gw.expect_allow_always_mcp_tool("linear__list");
        let d = request(&handle, mcp("linear__list"), "1").await;
        assert!(matches!(d, Decision::Allow));

        // Allow disk write to land.
        for _ in 0..50 {
            if permission_state_path(&cwd).exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let state = load_state(&cwd);
        assert!(state.allowed_mcp_tools.contains("linear__list"));
        assert!(state.allowed_mcp_servers.is_empty());

        // Second request for the same tool must NOT prompt — actor returns
        // Allow without consuming a scripted response. If it tried to
        // prompt, the gateway task would block forever, and the request
        // call below would hang; we assert by simply receiving an Allow
        // synchronously.
        let d = request(&handle, mcp("linear__list"), "2").await;
        assert!(matches!(d, Decision::Allow));

        // A different tool from the same server still prompts (tool-scope
        // is exact). We script a server-scope grant for "linear" next.
        gw.expect_allow_always_mcp_server("linear");
        let d = request(&handle, mcp("linear__create"), "3").await;
        assert!(matches!(d, Decision::Allow));

        // Wait for the new write.
        for _ in 0..50 {
            let s = load_state(&cwd);
            if s.allowed_mcp_servers.contains("linear") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let state = load_state(&cwd);
        assert!(state.allowed_mcp_servers.contains("linear"));

        // Now any other linear__* tool short-circuits via server-scope.
        let d = request(&handle, mcp("linear__update"), "4").await;
        assert!(matches!(d, Decision::Allow));

        // A different server still prompts.
        gw.expect_allow_always_mcp_tool("notion__fetch");
        let d = request(&handle, mcp("notion__fetch"), "5").await;
        assert!(matches!(d, Decision::Allow));
    })
    .await;
}

#[tokio::test]
#[serial]
async fn fallback_client_plain_allow_always_persists_mcp_tool() {
    // Regression: Generic / EchoAgentWeb / Extension clients
    // submit the legacy `"always-allow"` option id. The prompter maps that
    // to plain `PromptOutcome::AllowAlways`, and the manager's plain arm
    // must persist tool-scope into `allowed_mcp_tools`.
    run_actor_test(ClientType::Generic, |handle, gw, cwd| async move {
        gw.expect_plain_allow_always();
        let d = request(&handle, mcp("notion__fetch"), "1").await;
        assert!(matches!(d, Decision::Allow));

        for _ in 0..50 {
            if permission_state_path(&cwd).exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let state = load_state(&cwd);
        assert!(
            state.allowed_mcp_tools.contains("notion__fetch"),
            "fallback client AllowAlways must persist tool-scope, got state={state:?}"
        );

        // Re-request the same tool — must short-circuit without prompting.
        let d = request(&handle, mcp("notion__fetch"), "2").await;
        assert!(matches!(d, Decision::Allow));
    })
    .await;
}

#[tokio::test]
#[serial]
async fn policy_ask_suppresses_mcp_tool_allowlist() {
    // With `remember_tool_approvals` OFF (explicitly disabled), a policy `Ask` rule on an
    // MCP tool overrides a session tool-scope grant: the actor must prompt rather
    // than auto-allow. (The gate-ON "grant satisfies ask" path is covered by the
    // `mcp_pre_decision` unit tests in `manager.rs`.)
    let policy = PermissionConfig::new(vec![rule(RuleAction::Ask, "linear__*")]);

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let cwd = fresh_cwd();

            // Pre-seed the state file with a tool-scope grant.
            let mut state = PermissionState::default();
            state.allowed_mcp_tools.insert("linear__list".to_owned());
            let dir = test_home()
                .join("sessions")
                .join(urlencoding::encode(cwd.as_str()).into_owned());
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("permission.toml"),
                toml::to_string_pretty(&state).unwrap(),
            )
            .unwrap();

            let (gw, _gw_task) = fake_gateway();
            // Gate OFF so the `ask` rule stays a hard floor over the grant.
            let (handle, _events) = spawn_permission_manager_with_hub(
                make_session_id(),
                gw.sender.clone(),
                cwd.clone(),
                ClientType::EchoAgentPager,
                Some(policy),
                vec![], // deny_read_globs
                vec![],
                false,
                None,
                false, // remember_tool_approvals
                None,
            );

            // Script an outright reject so we can confirm the prompt fires.
            gw.expected.send(("reject-once".to_string(), None)).unwrap();

            let d = request(&handle, mcp("linear__list"), "1").await;
            // If the allowlist had won, the actor would have returned Allow
            // without consuming the scripted response; the gateway's reject
            // proves the prompt path executed.
            assert!(matches!(d, Decision::Reject(_)));
        })
        .await;
}

#[tokio::test]
#[serial]
async fn policy_ask_suppresses_mcp_server_allowlist() {
    // Gate-OFF floor over a server-scope grant (see the tool-scope test above).
    let policy = PermissionConfig::new(vec![rule(RuleAction::Ask, "linear__*")]);

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let cwd = fresh_cwd();

            let mut state = PermissionState::default();
            state.allowed_mcp_servers.insert("linear".to_owned());
            let dir = test_home()
                .join("sessions")
                .join(urlencoding::encode(cwd.as_str()).into_owned());
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("permission.toml"),
                toml::to_string_pretty(&state).unwrap(),
            )
            .unwrap();

            let (gw, _gw_task) = fake_gateway();
            // Gate OFF so the `ask` rule stays a hard floor over the grant.
            let (handle, _events) = spawn_permission_manager_with_hub(
                make_session_id(),
                gw.sender.clone(),
                cwd.clone(),
                ClientType::EchoAgentPager,
                Some(policy),
                vec![], // deny_read_globs
                vec![],
                false,
                None,
                false, // remember_tool_approvals
                None,
            );

            gw.expected.send(("reject-once".to_string(), None)).unwrap();

            let d = request(&handle, mcp("linear__create"), "1").await;
            assert!(matches!(d, Decision::Reject(_)));
        })
        .await;
}

#[tokio::test]
#[serial]
async fn policy_deny_takes_precedence_over_mcp_allowlist() {
    let policy = PermissionConfig::new(vec![rule(RuleAction::Deny, "linear__*")]);

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let cwd = fresh_cwd();

            let mut state = PermissionState::default();
            state.allowed_mcp_tools.insert("linear__list".to_owned());
            let dir = test_home()
                .join("sessions")
                .join(urlencoding::encode(cwd.as_str()).into_owned());
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("permission.toml"),
                toml::to_string_pretty(&state).unwrap(),
            )
            .unwrap();

            let (gw, _gw_task) = fake_gateway();
            let (handle, _events) = spawn_permission_manager(
                make_session_id(),
                gw.sender.clone(),
                cwd.clone(),
                ClientType::EchoAgentPager,
                Some(policy),
                vec![], // deny_read_globs
                vec![],
                false,
                None,
            );

            // Do NOT script a response: a policy Deny must short-circuit
            // before the prompt path is reached, so the gateway must never
            // be invoked.
            let d = request(&handle, mcp("linear__list"), "1").await;
            assert!(matches!(d, Decision::PolicyDeny(_)));
        })
        .await;
}

#[tokio::test]
#[serial]
async fn policy_allow_short_circuits_before_mcp_allowlist() {
    // Sanity: a policy Allow returns Allow immediately and never touches
    // the pre-decision lookup.
    let policy = PermissionConfig::new(vec![rule(RuleAction::Allow, "linear__*")]);

    run_actor_test_with_policy(
        ClientType::EchoAgentPager,
        Some(policy),
        |handle, _gw, _cwd| async move {
            let d = request(&handle, mcp("linear__list"), "1").await;
            assert!(matches!(d, Decision::Allow));
        },
    )
    .await;
}

#[tokio::test]
#[serial]
async fn empty_server_prefix_falls_back_to_tool_scope() {
    // Defense-in-depth: even if a malformed `McpScopeSelection::Server { server: "" }`
    // somehow makes it through, the prompter must downgrade to tool-scope
    // and persist via `AllowAlwaysMcpTool` — never write an empty server
    // prefix into `allowed_mcp_servers`.
    run_actor_test(ClientType::EchoAgentPager, |handle, gw, cwd| async move {
        let meta = serde_json::json!({
            "kind": "server",
            "server": "",
        });
        gw.expected
            .send(("allow-always-mcp".to_string(), Some(meta)))
            .unwrap();

        let d = request(&handle, mcp("linear__list"), "1").await;
        assert!(matches!(d, Decision::Allow));

        for _ in 0..50 {
            if permission_state_path(&cwd).exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let state = load_state(&cwd);
        assert!(state.allowed_mcp_servers.is_empty());
        assert!(state.allowed_mcp_tools.contains("linear__list"));
    })
    .await;
}

#[tokio::test]
#[serial]
async fn allow_always_mcp_tool_ignores_client_supplied_tool_name() {
    // Security regression: the response meta `tool_name` is informational
    // only. The manager MUST persist the name from `AccessKind::MCPTool`
    // so a buggy or malicious client cannot whitelist a different tool
    // than the one the user saw in the prompt.
    run_actor_test(ClientType::EchoAgentPager, |handle, gw, cwd| async move {
        // Request approves `linear__list`, but the response claims a
        // different tool name (e.g. `notion__fetch`).
        let meta = serde_json::json!({
            "kind": "tool",
            "tool_name": "notion__fetch",
        });
        gw.expected
            .send(("allow-always-mcp".to_string(), Some(meta)))
            .unwrap();

        let d = request(&handle, mcp("linear__list"), "1").await;
        assert!(matches!(d, Decision::Allow));

        for _ in 0..50 {
            if permission_state_path(&cwd).exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let state = load_state(&cwd);
        assert!(
            state.allowed_mcp_tools.contains("linear__list"),
            "must persist the access-kind name"
        );
        assert!(
            !state.allowed_mcp_tools.contains("notion__fetch"),
            "must NOT persist the client-supplied name"
        );
    })
    .await;
}

#[tokio::test]
#[serial]
async fn allow_always_mcp_server_rejects_mismatched_prefix() {
    // Security regression: the response meta `server` must match the
    // canonical server prefix derived from the access kind. On mismatch,
    // the manager downgrades to tool-scope on the access-kind name -- the
    // smallest blast radius the user actually approved.
    run_actor_test(ClientType::EchoAgentPager, |handle, gw, cwd| async move {
        // Approve `linear__list` (canonical server prefix is `linear`),
        // but the client claims `notion` as the server.
        let meta = serde_json::json!({
            "kind": "server",
            "server": "notion",
        });
        gw.expected
            .send(("allow-always-mcp".to_string(), Some(meta)))
            .unwrap();

        let d = request(&handle, mcp("linear__list"), "1").await;
        assert!(matches!(d, Decision::Allow));

        for _ in 0..50 {
            if permission_state_path(&cwd).exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let state = load_state(&cwd);
        assert!(
            state.allowed_mcp_servers.is_empty(),
            "must NOT persist any server-scope grant on mismatch"
        );
        assert!(
            !state.allowed_mcp_servers.contains("notion"),
            "must NOT trust the client-supplied server prefix"
        );
        assert!(
            state.allowed_mcp_tools.contains("linear__list"),
            "must downgrade to tool-scope using the access-kind name"
        );
    })
    .await;
}

#[tokio::test]
#[serial]
async fn allow_always_mcp_server_persists_canonical_prefix_on_match() {
    // Sanity: a client that supplies the correct canonical prefix
    // succeeds (this is the common case post-fix).
    run_actor_test(ClientType::EchoAgentPager, |handle, gw, cwd| async move {
        let meta = serde_json::json!({
            "kind": "server",
            "server": "linear",
        });
        gw.expected
            .send(("allow-always-mcp".to_string(), Some(meta)))
            .unwrap();

        let d = request(&handle, mcp("linear__list"), "1").await;
        assert!(matches!(d, Decision::Allow));

        for _ in 0..50 {
            if permission_state_path(&cwd).exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let state = load_state(&cwd);
        assert!(state.allowed_mcp_servers.contains("linear"));
        assert!(state.allowed_mcp_tools.is_empty());
    })
    .await;
}

#[tokio::test]
#[serial]
async fn allow_always_mcp_server_downgrades_when_access_has_no_separator() {
    // Defensive: if the access name itself has no `__` (e.g. via a
    // malformed `ToolInput::MCPTool`), the canonical prefix is None and
    // server-scope is unreachable. The manager downgrades to tool-scope
    // on the raw access name rather than persisting the client prefix.
    run_actor_test(ClientType::EchoAgentPager, |handle, gw, cwd| async move {
        let meta = serde_json::json!({
            "kind": "server",
            "server": "linear",
        });
        gw.expected
            .send(("allow-always-mcp".to_string(), Some(meta)))
            .unwrap();

        let d = request(&handle, mcp("standalone"), "1").await;
        assert!(matches!(d, Decision::Allow));

        for _ in 0..50 {
            if permission_state_path(&cwd).exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let state = load_state(&cwd);
        assert!(state.allowed_mcp_servers.is_empty());
        assert!(state.allowed_mcp_tools.contains("standalone"));
    })
    .await;
}

#[tokio::test]
#[serial]
async fn dont_ask_policy_denies_without_prompting() {
    use echo_agent_workspace::permission::types::{PermissionConfig, PromptPolicy};

    let mut policy = PermissionConfig::new(vec![]);
    policy.prompt_policy = PromptPolicy::Deny;

    // No scripted gateway responses: if the manager tried to prompt,
    // the gateway would block forever, proving dont_ask short-circuits.
    run_actor_test_with_policy(
        ClientType::EchoAgentPager,
        Some(policy),
        |handle, _gw, _cwd| async move {
            let d = request(&handle, mcp("linear__list"), "1").await;
            assert!(matches!(d, Decision::PolicyDeny(_)));

            let d = request(&handle, AccessKind::Bash("npm install".to_string()), "2").await;
            assert!(matches!(d, Decision::PolicyDeny(_)));

            // Reads still auto-approve (pre-decision, before dont_ask)
            let d = request(
                &handle,
                AccessKind::Read(Some("/tmp/test.txt".to_string())),
                "3",
            )
            .await;
            assert!(matches!(d, Decision::Allow));
        },
    )
    .await;
}

// --- deny rules survive YOLO mode ---

#[tokio::test]
#[serial]
async fn deny_rule_enforced_in_yolo_mode_bash() {
    let policy = PermissionConfig::new(vec![PermissionRule {
        action: RuleAction::Deny,
        tool: ToolFilter::Bash,
        pattern: Some("rm*".to_owned()),
        pattern_mode: PatternMode::Glob,
    }]);

    run_actor_test_full(
        ClientType::EchoAgentPager,
        Some(policy),
        true,
        |handle, _gw, _cwd| async move {
            let d = request(
                &handle,
                AccessKind::Bash("rm -rf /tmp/foo".to_string()),
                "1",
            )
            .await;
            assert!(
                matches!(d, Decision::PolicyDeny(_)),
                "deny rule must block even in YOLO mode, got {d:?}"
            );

            let d = request(&handle, AccessKind::Bash("cargo test".to_string()), "2").await;
            assert!(
                matches!(d, Decision::Allow),
                "non-denied bash must auto-approve in YOLO mode, got {d:?}"
            );
        },
    )
    .await;
}

#[tokio::test]
#[serial]
async fn deny_rule_enforced_in_yolo_mode_mcp() {
    let policy = PermissionConfig::new(vec![PermissionRule {
        action: RuleAction::Deny,
        tool: ToolFilter::Mcp,
        pattern: Some("dangerous__*".to_owned()),
        pattern_mode: PatternMode::Glob,
    }]);

    run_actor_test_full(
        ClientType::EchoAgentPager,
        Some(policy),
        true,
        |handle, _gw, _cwd| async move {
            let d = request(&handle, mcp("dangerous__delete_all"), "1").await;
            assert!(
                matches!(d, Decision::PolicyDeny(_)),
                "deny rule must block MCP tool even in YOLO mode, got {d:?}"
            );

            let d = request(&handle, mcp("linear__list"), "2").await;
            assert!(
                matches!(d, Decision::Allow),
                "non-denied MCP tool must auto-approve in YOLO mode, got {d:?}"
            );
        },
    )
    .await;
}

#[tokio::test]
#[serial]
async fn deny_rule_enforced_in_yolo_mode_edit() {
    let policy = PermissionConfig::new(vec![PermissionRule {
        action: RuleAction::Deny,
        tool: ToolFilter::Edit,
        pattern: Some("/etc/**".to_owned()),
        pattern_mode: PatternMode::Glob,
    }]);

    run_actor_test_full(
        ClientType::EchoAgentPager,
        Some(policy),
        true,
        |handle, _gw, _cwd| async move {
            let d = request(&handle, AccessKind::Edit("/etc/passwd".to_string()), "1").await;
            assert!(
                matches!(d, Decision::PolicyDeny(_)),
                "deny rule must block edits even in YOLO mode, got {d:?}"
            );

            let d = request(&handle, AccessKind::Edit("src/main.rs".to_string()), "2").await;
            assert!(
                matches!(d, Decision::Allow),
                "non-denied edit must auto-approve in YOLO mode, got {d:?}"
            );
        },
    )
    .await;
}

#[tokio::test]
#[serial]
async fn deny_rule_enforced_in_yolo_mode_web_fetch() {
    let policy = PermissionConfig::new(vec![PermissionRule {
        action: RuleAction::Deny,
        tool: ToolFilter::WebFetch,
        pattern: Some("evil.com".to_owned()),
        pattern_mode: PatternMode::Domain,
    }]);

    run_actor_test_full(
        ClientType::EchoAgentPager,
        Some(policy),
        true,
        |handle, _gw, _cwd| async move {
            let d = request(
                &handle,
                AccessKind::WebFetch("https://evil.com/exfiltrate".to_string()),
                "1",
            )
            .await;
            assert!(
                matches!(d, Decision::PolicyDeny(_)),
                "deny rule must block web_fetch even in YOLO mode, got {d:?}"
            );

            let d = request(
                &handle,
                AccessKind::WebFetch("https://docs.rs/tokio".to_string()),
                "2",
            )
            .await;
            assert!(
                matches!(d, Decision::Allow),
                "non-denied web_fetch must auto-approve in YOLO mode, got {d:?}"
            );
        },
    )
    .await;
}

#[tokio::test]
#[serial]
async fn yolo_mode_without_deny_rules_approves_everything() {
    run_actor_test_full(
        ClientType::EchoAgentPager,
        None,
        true,
        |handle, _gw, _cwd| async move {
            let d = request(&handle, AccessKind::Bash("rm -rf /".to_string()), "1").await;
            assert!(matches!(d, Decision::Allow));

            let d = request(&handle, mcp("linear__list"), "2").await;
            assert!(matches!(d, Decision::Allow));

            let d = request(&handle, AccessKind::Edit("/etc/passwd".to_string()), "3").await;
            assert!(matches!(d, Decision::Allow));

            let d = request(
                &handle,
                AccessKind::WebFetch("https://evil.com".to_string()),
                "4",
            )
            .await;
            assert!(matches!(d, Decision::Allow));
        },
    )
    .await;
}
