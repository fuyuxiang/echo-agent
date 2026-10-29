use super::*;
use std::path::PathBuf;

#[tokio::test]
async fn resilient_transport_skips_undecodable_line_and_keeps_stream_alive() {
    let (mut server_out, client_in) = tokio::io::duplex(64 * 1024);
    let mut transport = ResilientRwTransport::new(
        client_in,
        tokio::io::sink(),
        "fwbuild".to_string(),
        echo_agent_session_events::EventWriter::noop(),
    );

    let valid = r#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#;
    let garbage = "info: fwbuild started, listening on stdio";
    server_out
        .write_all(format!("{valid}\n{garbage}\n{valid}\n").as_bytes())
        .await
        .unwrap();
    drop(server_out);

    assert!(
        transport.receive().await.is_some(),
        "first valid message must be received"
    );
    assert!(
        transport.receive().await.is_some(),
        "the undecodable line must be skipped and the next valid message delivered"
    );
    assert!(
        transport.receive().await.is_none(),
        "only a genuine end-of-stream yields None"
    );
}

fn make_stdio_server(name: &str, command: &str) -> acp::McpServer {
    acp::McpServer::Stdio(acp::McpServerStdio::new(name, PathBuf::from(command)))
}

fn make_http_server(name: &str, url: &str) -> acp::McpServer {
    acp::McpServer::Http(acp::McpServerHttp::new(name, url))
}

#[test]
fn plan_stdio_spawn_windows_resolves_bare_launcher_to_cmd_shim() {
    let args = vec!["-y".to_string(), "@scope/pkg".to_string()];
    let (program, spawn_args) = plan_stdio_spawn("npx", &args, true, |c| {
        assert_eq!(c, "npx");
        Some(PathBuf::from(r"C:\path\npx.cmd"))
    });
    assert_eq!(program, OsString::from(r"C:\path\npx.cmd"));
    assert_eq!(spawn_args, args);
}

#[test]
fn plan_stdio_spawn_windows_unresolved_falls_back_to_raw_command() {
    let args = vec!["-y".to_string(), "@scope/pkg".to_string()];
    let (program, spawn_args) = plan_stdio_spawn("npx", &args, true, |_| None);
    assert_eq!(program, OsString::from("npx"));
    assert_eq!(spawn_args, args);
}

#[test]
fn plan_stdio_spawn_windows_backslash_path_command_used_as_is_without_resolving() {
    let args = vec!["--config".to_string(), "x.json".to_string()];
    let (program, spawn_args) = plan_stdio_spawn(r"C:\tools\server.exe", &args, true, |_| {
        panic!("resolver must not be consulted for a command with a backslash separator")
    });
    assert_eq!(program, OsString::from(r"C:\tools\server.exe"));
    assert_eq!(spawn_args, args);
}

#[test]
fn plan_stdio_spawn_windows_forward_slash_path_command_used_as_is_without_resolving() {
    let args = vec!["--port".to_string(), "8080".to_string()];
    let (program, spawn_args) = plan_stdio_spawn("C:/tools/server.exe", &args, true, |_| {
        panic!("resolver must not be consulted for a command with a forward-slash separator")
    });
    assert_eq!(program, OsString::from("C:/tools/server.exe"));
    assert_eq!(spawn_args, args);
}

#[test]
fn plan_stdio_spawn_non_windows_never_resolves() {
    let args = vec!["-y".to_string(), "pkg".to_string()];
    let (program, spawn_args) = plan_stdio_spawn("npx", &args, false, |_| {
        panic!("resolver must not be consulted on non-Windows")
    });
    assert_eq!(program, OsString::from("npx"));
    assert_eq!(spawn_args, args);
}

#[test]
fn stdio_path_override_matches_path_case_insensitively() {
    let mk = |name: &str, value: &str| acp::EnvVariable::new(name, value);

    let env = vec![mk("FOO", "bar"), mk("Path", r"C:\node")];
    assert_eq!(stdio_path_override(&env), Some(r"C:\node"));

    let env_upper = vec![mk("PATH", "/custom/bin")];
    assert_eq!(stdio_path_override(&env_upper), Some("/custom/bin"));

    let env_none = vec![mk("FOO", "bar")];
    assert_eq!(stdio_path_override(&env_none), None);
}

#[test]
fn is_figma_mcp_matches_name_and_host() {
    assert!(is_figma_mcp("figma", "https://example.com/mcp"));
    assert!(is_figma_mcp("Figma", "https://example.com/mcp"));
    assert!(is_figma_mcp(
        "echo_agent_com_figma",
        "https://example.com/mcp"
    ));
    assert!(is_figma_mcp(
        "ECHO_AGENT_COM_FIGMA",
        "https://example.com/mcp"
    ));
    assert!(is_figma_mcp(
        "echo_agent_com_FIGMA",
        "https://example.com/mcp"
    ));
    assert!(is_figma_mcp("other", "https://mcp.figma.com/mcp"));
    assert!(is_figma_mcp("other", "https://figma.com/mcp"));
    assert!(!is_figma_mcp("linear", "https://mcp.linear.app/mcp"));
    assert!(!is_figma_mcp("figma_extra", "https://example.com/mcp"));
    assert!(!is_figma_mcp(
        "echo_agent_com_linear",
        "https://example.com/mcp"
    ));
    assert!(!is_figma_mcp("linear", "not-a-url"));
    assert!(!is_figma_mcp("linear", "https://notfigma.com/mcp"));
    assert!(!is_figma_mcp("linear", "https://figma.com.evil/mcp"));
}

#[test]
fn ensure_figma_user_agent_sets_echo_agent_cli_when_missing() {
    let mut headers = reqwest::header::HeaderMap::new();
    ensure_figma_user_agent(&mut headers, "figma", "https://mcp.figma.com/mcp");
    assert_eq!(
        headers.get(reqwest::header::USER_AGENT).unwrap(),
        "echo-agent-cli"
    );

    let mut host_only = reqwest::header::HeaderMap::new();
    ensure_figma_user_agent(&mut host_only, "other", "https://mcp.figma.com/mcp");
    assert_eq!(
        host_only.get(reqwest::header::USER_AGENT).unwrap(),
        "echo-agent-cli"
    );
}

#[test]
fn ensure_figma_user_agent_does_not_overwrite_existing() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::USER_AGENT,
        reqwest::header::HeaderValue::from_static("custom-ua"),
    );
    ensure_figma_user_agent(&mut headers, "figma", "https://mcp.figma.com/mcp");
    assert_eq!(
        headers.get(reqwest::header::USER_AGENT).unwrap(),
        "custom-ua"
    );
}

#[test]
fn ensure_figma_user_agent_skips_non_figma() {
    let mut headers = reqwest::header::HeaderMap::new();
    ensure_figma_user_agent(&mut headers, "linear", "https://mcp.linear.app/mcp");
    assert!(!headers.contains_key(reqwest::header::USER_AGENT));

    let mut invalid_url = reqwest::header::HeaderMap::new();
    ensure_figma_user_agent(&mut invalid_url, "linear", "not-a-url");
    assert!(!invalid_url.contains_key(reqwest::header::USER_AGENT));
}

#[test]
fn parse_config_headers_skips_invalid_and_keeps_last_duplicate() {
    let pairs = [
        ("X-Api-Key", "first"),
        ("bad header", "value"),
        ("X-Other", "bad\nvalue"),
        ("x-api-key", "second"),
    ];
    let headers = parse_config_headers("srv", "transport", pairs.iter().copied());
    assert_eq!(headers.len(), 1);
    assert_eq!(headers.get("X-Api-Key").unwrap(), "second");
}

#[test]
fn apply_user_agent_policy_sets_versioned_echo_agent_cli() {
    let mut headers = reqwest::header::HeaderMap::new();
    apply_user_agent_policy(&mut headers, "linear", "https://mcp.linear.app/mcp");
    let expected = format!("echo-agent-cli/{}", echo_agent_version::VERSION);
    assert_eq!(
        headers.get(reqwest::header::USER_AGENT).unwrap(),
        expected.as_str()
    );
}

#[test]
fn apply_user_agent_policy_preserves_configured_user_agent() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::USER_AGENT,
        reqwest::header::HeaderValue::from_static("custom-ua"),
    );
    apply_user_agent_policy(&mut headers, "linear", "https://mcp.linear.app/mcp");
    assert_eq!(
        headers.get(reqwest::header::USER_AGENT).unwrap(),
        "custom-ua"
    );
}

#[test]
fn apply_user_agent_policy_preserves_figma_attribution() {
    let mut headers = reqwest::header::HeaderMap::new();
    apply_user_agent_policy(&mut headers, "other", "https://mcp.figma.com/mcp");
    assert_eq!(
        headers.get(reqwest::header::USER_AGENT).unwrap(),
        "echo-agent-cli"
    );
}

#[cfg(unix)]
#[test]
fn safe_stdio_child_drop_without_entered_runtime_reaps_child() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");

    let (transport, pid) = rt.block_on(async {
        let mut cmd = Command::new("sleep");
        cmd.arg("30").kill_on_drop(true);
        echo_agent_tools::util::detach_command(&mut cmd);
        let (transport, _stderr) = SafeTokioChildProcess::spawn(
            cmd,
            None,
            "test".to_string(),
            echo_agent_session_events::EventWriter::noop(),
        )
        .await
        .expect("spawn test child");
        let pid = transport.id().expect("spawned child pid");
        (transport, pid)
    });

    drop(rt);
    drop(transport);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if !unix_process_exists(pid) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }

    panic!("MCP child process {pid} was not reaped after no-runtime drop");
}

#[cfg(unix)]
fn unix_process_exists(pid: u32) -> bool {
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if result == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

#[cfg(unix)]
#[tokio::test]
async fn scope_kill_all_reaps_enrolled_mcp_child_while_owner_wedged() {
    use std::time::Duration;

    let scope = ProcessScope::new();

    let mut cmd = Command::new("sleep");
    cmd.arg("600").kill_on_drop(true);
    echo_agent_tools::util::detach_command(&mut cmd);
    let (mut child_process, _stderr) = SafeTokioChildProcess::spawn(
        cmd,
        Some(&scope),
        "wedge-test".to_string(),
        echo_agent_session_events::EventWriter::noop(),
    )
    .await
    .expect("spawn enrolled MCP child");
    assert_eq!(
        scope.live_count(),
        1,
        "the enrolled MCP child group must be tracked by the scope"
    );

    scope.kill_all();

    let mut child = child_process.child.take().expect("child handle present");
    child_process.process_group = None;
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("scope.kill_all must have SIGKILL'd the enrolled MCP child group")
        .expect("wait on the reclaimed child succeeds");
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(
        status.signal(),
        Some(libc::SIGKILL),
        "the MCP child must have been SIGKILL'd by the scope, not have exited cleanly"
    );
}

#[test]
fn test_mcp_state_new() {
    let configs = vec![make_stdio_server("test", "/bin/test")];
    let state = McpState::new(configs.clone());

    assert_eq!(state.configs.len(), 1);
    assert!(state.owned_clients.is_empty());
    assert!(!state.is_initialized());
    assert!(!state.is_initializing());
    assert!(!state.has_finished_init());
    assert!(matches!(state.init_progress(), InitProgress::NotStarted));
    assert_eq!(state.generation, 0);
}

#[test]
fn config_update_clears_stale_failure_records() {
    let mut state = McpState::new(vec![make_http_server("a", "https://old.example/a")]);
    state.record_init_failure("a", false, Some("old cause".to_string()));
    let diff = state
        .update_configs_diff(vec![make_http_server("a", "https://new.example/a")])
        .expect("configs changed");
    assert_eq!(diff.removed, vec!["a".to_string()]);
    assert!(
        !state.init_failed.contains_key("a"),
        "changed config must clear the stale failure record"
    );

    let mut state = McpState::new(vec![make_http_server("b", "https://old.example/b")]);
    state.record_init_failure("b", false, Some("old cause".to_string()));
    assert!(state.update_configs(vec![make_http_server("b", "https://new.example/b")]));
    assert!(state.init_failed.is_empty());
}

#[test]
fn test_mcp_state_update_configs_returns_false_when_unchanged() {
    let configs = vec![make_stdio_server("test", "/bin/test")];
    let mut state = McpState::new(configs.clone());

    let changed = state.update_configs(configs.clone());
    assert!(!changed);
    assert_eq!(state.generation, 0);
}

#[test]
fn test_mcp_state_update_configs_returns_true_when_changed() {
    let configs = vec![make_stdio_server("test", "/bin/test")];
    let mut state = McpState::new(configs);

    let new_configs = vec![make_stdio_server("test2", "/bin/test2")];
    let changed = state.update_configs(new_configs);
    assert!(changed);
    assert_eq!(state.generation, 1);
}

#[test]
fn test_mcp_state_update_configs_resets_initialized() {
    let configs = vec![make_stdio_server("test", "/bin/test")];
    let mut state = McpState::new(configs);
    assert!(state.try_start_init());
    state.mark_servers_initializing(["a".to_string()]);
    state.finish_init();
    assert!(state.has_finished_init());
    assert!(state.is_server_handshaking("a"));

    let new_configs = vec![make_stdio_server("test2", "/bin/test2")];
    let changed = state.update_configs(new_configs);
    assert!(changed);
    assert!(!state.is_initialized());
    assert!(!state.is_initializing());
    assert!(!state.has_finished_init());
    assert!(matches!(state.init_progress(), InitProgress::NotStarted));
}

#[tokio::test]
async fn acp_servers_survive_update_configs_clear() {
    use crate::acp_transport::AcpReverseInvoker;
    use std::time::Duration;

    struct NoopInvoker;
    #[async_trait::async_trait]
    impl AcpReverseInvoker for NoopInvoker {
        async fn invoke(
            &self,
            _server_id: &str,
            _message: serde_json::Value,
            _timeout: Duration,
        ) -> Result<serde_json::Value, String> {
            Ok(serde_json::Value::Null)
        }
    }

    let mut state = McpState::new(vec![make_http_server("http-srv", "http://localhost")]);
    state.set_acp_servers(
        vec![AcpServerEntry {
            name: "sdk-tools".to_string(),
            server_id: "srv_0".to_string(),
        }],
        Arc::new(NoopInvoker),
    );
    assert!(state.has_acp_servers());
    assert_eq!(state.build_pending_acp_clients(&HashMap::new()).len(), 1);

    let changed = state.update_configs(vec![make_http_server("other", "http://other")]);
    assert!(changed);
    assert_eq!(state.generation, 1);
    assert!(
        state.has_acp_servers(),
        "acp servers must survive update_configs"
    );
    let pending = state.build_pending_acp_clients(&HashMap::new());
    assert_eq!(pending.len(), 1, "acp clients rebuild after the clear");
    assert_eq!(pending[0].server_name(), "sdk-tools");
}

#[tokio::test]
async fn acp_overrides_apply_to_built_clients() {
    use crate::acp_transport::AcpReverseInvoker;
    use std::time::Duration;

    struct NoopInvoker;
    #[async_trait::async_trait]
    impl AcpReverseInvoker for NoopInvoker {
        async fn invoke(
            &self,
            _server_id: &str,
            _message: serde_json::Value,
            _timeout: Duration,
        ) -> Result<serde_json::Value, String> {
            Ok(serde_json::Value::Null)
        }
    }

    let mut overrides = HashMap::new();
    overrides.insert(
        "sdk-tools".to_string(),
        McpClientTimeoutOverrides {
            tool_timeout_sec: Some(123),
            ..Default::default()
        },
    );

    let mut state = McpState::new(vec![]);
    state.set_acp_servers(
        vec![AcpServerEntry {
            name: "sdk-tools".to_string(),
            server_id: "srv_0".to_string(),
        }],
        Arc::new(NoopInvoker),
    );

    let pending = state.build_pending_acp_clients(&overrides);
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].tool_timeout_sec(),
        123,
        "config.toml tool_timeout_sec override must reach the SDK client"
    );
}

#[tokio::test]
async fn acp_clients_are_not_liveness_watched() {
    use crate::acp_transport::AcpReverseInvoker;
    use std::time::Duration;

    struct NoopInvoker;
    #[async_trait::async_trait]
    impl AcpReverseInvoker for NoopInvoker {
        async fn invoke(
            &self,
            _server_id: &str,
            _message: serde_json::Value,
            _timeout: Duration,
        ) -> Result<serde_json::Value, String> {
            Ok(serde_json::Value::Null)
        }
    }

    let acp = McpClient::new_acp(
        "sdk".to_string(),
        "srv_0".to_string(),
        Arc::new(NoopInvoker),
        None,
        None,
    );
    assert!(acp.is_acp());
    assert!(!acp.is_http());

    let http = McpClient::new_http(
        "http".to_string(),
        HttpConfig {
            url: "http://localhost/api/mcp".to_string(),
            headers: vec![],
        },
        None,
        None,
    );
    assert!(!http.is_acp());

    assert!(!McpClient::stub("stdio").is_acp());

    assert!(
        !Arc::new(acp)
            .arm_liveness_watcher(Duration::from_millis(500))
            .await
    );
}

#[test]
fn test_mark_servers_initializing_clears_prior_init_failure() {
    let mut state = McpState::new(vec![make_stdio_server("a", "/bin/a")]);
    state.init_failed.insert("a".to_string(), String::new());
    state.init_failed.insert("b".to_string(), String::new());

    state.mark_servers_initializing(["a".to_string()]);

    assert!(
        !state.init_failed.contains_key("a"),
        "fresh init attempt must clear the prior failure for that server",
    );
    assert!(
        state.init_failed.contains_key("b"),
        "servers not in this init attempt must keep their failure flag",
    );
}

#[test]
fn test_record_init_failure_keeps_auth_and_init_failed_disjoint() {
    let mut state = McpState::new(vec![make_stdio_server("a", "/bin/a")]);

    state.record_init_failure("auth-srv", true, None);
    assert!(state.auth_required.contains("auth-srv"));
    assert!(
        !state.init_failed.contains_key("auth-srv"),
        "auth-required failures must not also be flagged init_failed",
    );

    state.record_init_failure(
        "dead-srv",
        false,
        Some("tools/list failed: boom".to_string()),
    );
    assert!(!state.auth_required.contains("dead-srv"));
    assert_eq!(
        state.init_failed.get("dead-srv").map(String::as_str),
        Some("tools/list failed: boom"),
    );

    state.mark_servers_initializing(["dead-srv".to_string()]);
    assert!(!state.init_failed.contains_key("dead-srv"));
}

#[test]
fn test_clear_init_failed_removes_entry() {
    let mut state = McpState::new(vec![make_stdio_server("a", "/bin/a")]);
    state.record_init_failure("dead-srv", false, Some("boom".to_string()));
    assert!(state.init_failed.contains_key("dead-srv"));

    state.clear_init_failed("dead-srv");
    assert!(!state.init_failed.contains_key("dead-srv"));
    state.clear_init_failed("never-seen");
}

#[test]
fn test_mcp_state_update_configs_increments_generation() {
    let mut state = McpState::new(vec![]);

    state.update_configs(vec![make_stdio_server("a", "/bin/a")]);
    assert_eq!(state.generation, 1);

    state.update_configs(vec![make_stdio_server("b", "/bin/b")]);
    assert_eq!(state.generation, 2);

    state.update_configs(vec![make_stdio_server("c", "/bin/c")]);
    assert_eq!(state.generation, 3);
}

#[test]
fn test_mcp_servers_equal_empty_lists() {
    let a: Vec<acp::McpServer> = vec![];
    let b: Vec<acp::McpServer> = vec![];
    assert!(mcp_servers_equal(&a, &b));
}

#[test]
fn test_mcp_servers_equal_identical_configs() {
    let a = vec![make_stdio_server("test", "/bin/test")];
    let b = vec![make_stdio_server("test", "/bin/test")];
    assert!(mcp_servers_equal(&a, &b));
}

#[test]
fn test_mcp_servers_equal_different_names() {
    let a = vec![make_stdio_server("test1", "/bin/test")];
    let b = vec![make_stdio_server("test2", "/bin/test")];
    assert!(!mcp_servers_equal(&a, &b));
}

#[test]
fn test_mcp_servers_equal_different_lengths() {
    let a = vec![make_stdio_server("test", "/bin/test")];
    let b = vec![
        make_stdio_server("test", "/bin/test"),
        make_stdio_server("test2", "/bin/test2"),
    ];
    assert!(!mcp_servers_equal(&a, &b));
}

#[test]
fn test_mcp_servers_equal_different_types() {
    let a = vec![make_stdio_server("test", "/bin/test")];
    let b = vec![make_http_server("test", "http://localhost")];
    assert!(!mcp_servers_equal(&a, &b));
}

#[test]
fn test_mcp_servers_equal_order_matters() {
    let a = vec![
        make_stdio_server("a", "/bin/a"),
        make_stdio_server("b", "/bin/b"),
    ];
    let b = vec![
        make_stdio_server("b", "/bin/b"),
        make_stdio_server("a", "/bin/a"),
    ];
    assert!(!mcp_servers_equal(&a, &b));
}

#[test]
fn test_try_start_init_prevents_concurrent_init() {
    let mut state = McpState::new(vec![make_stdio_server("test", "/bin/test")]);

    assert!(state.try_start_init());
    assert!(state.is_initializing());
    assert!(!state.is_initialized());

    assert!(!state.try_start_init());
}

#[test]
fn test_try_start_init_fails_when_initialized() {
    let mut state = McpState::new(vec![make_stdio_server("test", "/bin/test")]);
    assert!(state.try_start_init());
    state.finish_init();
    assert!(state.is_initialized());

    assert!(!state.try_start_init());
    assert!(!state.is_initializing());
    assert!(state.is_initialized(), "is_initialized stays true");
}

#[test]
fn test_finish_init_clears_initializing() {
    let mut state = McpState::new(vec![make_stdio_server("test", "/bin/test")]);

    state.try_start_init();
    assert!(state.is_initializing());
    assert!(!state.is_initialized());

    state.finish_init();
    assert!(!state.is_initializing());
    assert!(state.is_initialized());
}

#[test]
fn test_cancel_init_clears_initializing() {
    let mut state = McpState::new(vec![make_stdio_server("test", "/bin/test")]);

    state.try_start_init();
    assert!(state.is_initializing());

    state.cancel_init();
    assert!(!state.is_initializing());
    assert!(!state.is_initialized());
}

#[test]
fn test_update_configs_resets_initializing() {
    let mut state = McpState::new(vec![make_stdio_server("test", "/bin/test")]);
    state.try_start_init();
    assert!(state.is_initializing());

    state.update_configs(vec![make_stdio_server("test2", "/bin/test2")]);
    assert!(!state.is_initializing());
    assert!(!state.is_initialized());
}

#[test]
fn test_parse_mcp_meta_config_with_tool_timeouts_ms() {
    let meta = serde_json::json!({
        "mcpConfig": {
            "github": {
                "toolTimeoutMs": 60000,
                "toolTimeoutsMs": {
                    "create_issue": 120000,
                    "search": 30000
                }
            }
        }
    })
    .as_object()
    .cloned()
    .unwrap();
    let map = parse_mcp_meta_config(Some(&meta));
    let github = map.get("github").unwrap();
    assert_eq!(github.tool_timeout_ms, Some(60000));
    let tt = github.tool_timeouts_ms.as_ref().unwrap();
    assert_eq!(tt.get("create_issue"), Some(&120000));
    assert_eq!(tt.get("search"), Some(&30000));
}

#[test]
fn test_parse_mcp_meta_config_without_tool_timeouts_ms() {
    let meta = serde_json::json!({
        "mcpConfig": {
            "github": {
                "toolTimeoutMs": 60000
            }
        }
    })
    .as_object()
    .cloned()
    .unwrap();
    let map = parse_mcp_meta_config(Some(&meta));
    let github = map.get("github").unwrap();
    assert_eq!(github.tool_timeout_ms, Some(60000));
    assert!(github.tool_timeouts_ms.is_none());
    assert!(github.expose_image_base64.is_none());
}

#[test]
fn test_parse_mcp_meta_config_with_expose_image_base64() {
    let meta = serde_json::json!({
        "mcpConfig": {
            "grafana": { "exposeImageBase64": true },
            "linear":  { "exposeImageBase64": false },
        }
    })
    .as_object()
    .cloned()
    .unwrap();
    let map = parse_mcp_meta_config(Some(&meta));
    assert_eq!(map.get("grafana").unwrap().expose_image_base64, Some(true));
    assert_eq!(map.get("linear").unwrap().expose_image_base64, Some(false));
}

#[test]
fn test_tool_timeout_for_returns_per_tool_override() {
    let mut tool_timeouts = HashMap::new();
    tool_timeouts.insert("create_issue".to_string(), 120u64);
    tool_timeouts.insert("search".to_string(), 30u64);

    let overrides = McpClientTimeoutOverrides {
        startup_timeout_sec: Some(10),
        tool_timeout_sec: Some(60),
        tool_timeouts: Some(tool_timeouts),
        ..Default::default()
    };
    let client = McpClient::new_http(
        "github".to_string(),
        HttpConfig {
            url: String::new(),
            headers: vec![],
        },
        Some(&overrides),
        None,
    );

    assert_eq!(client.tool_timeout_for("create_issue"), 120);
    assert_eq!(client.tool_timeout_for("search"), 30);
    assert_eq!(client.tool_timeout_for("list_repos"), 60);
    assert_eq!(client.tool_timeout_for(""), 60);
}

#[test]
fn test_tool_timeout_for_empty_map_returns_default() {
    let overrides = McpClientTimeoutOverrides {
        startup_timeout_sec: Some(10),
        tool_timeout_sec: Some(45),
        ..Default::default()
    };
    let client = McpClient::new_http(
        "test".to_string(),
        HttpConfig {
            url: String::new(),
            headers: vec![],
        },
        Some(&overrides),
        None,
    );

    assert_eq!(client.tool_timeout_for("any_tool"), 45);
    assert_eq!(client.tool_timeout_sec(), 45);
}

#[test]
fn test_load_timeouts_startup_precedence() {
    assert_eq!(
        McpClient::load_timeouts(None, None).0,
        DEFAULT_STARTUP_TIMEOUT_SECS
    );

    let overrides = McpClientTimeoutOverrides {
        startup_timeout_sec: Some(7),
        ..Default::default()
    };
    assert_eq!(McpClient::load_timeouts(Some(&overrides), None).0, 7);

    let meta = McpServerMetaConfig {
        startup_timeout_ms: Some(12_000),
        ..Default::default()
    };
    assert_eq!(
        McpClient::load_timeouts(Some(&overrides), Some(&meta)).0,
        12
    );
}

#[test]
fn test_update_configs_diff_no_change() {
    let configs = vec![make_stdio_server("test", "/bin/test")];
    let mut state = McpState::new(configs.clone());
    assert!(state.update_configs_diff(configs).is_none());
    assert_eq!(state.generation, 0);
}

#[test]
fn test_update_configs_diff_added() {
    let configs = vec![make_stdio_server("a", "/bin/a")];
    let mut state = McpState::new(configs);

    let new_configs = vec![
        make_stdio_server("a", "/bin/a"),
        make_stdio_server("b", "/bin/b"),
    ];
    let diff = state
        .update_configs_diff(new_configs)
        .expect("should detect change");
    assert_eq!(diff.retained, vec!["a"]);
    assert_eq!(diff.added, vec!["b"]);
    assert!(diff.removed.is_empty());
    assert_eq!(state.generation, 1);
}

#[test]
fn test_update_configs_diff_removed() {
    let configs = vec![
        make_stdio_server("a", "/bin/a"),
        make_stdio_server("b", "/bin/b"),
    ];
    let mut state = McpState::new(configs);

    let new_configs = vec![make_stdio_server("a", "/bin/a")];
    let diff = state
        .update_configs_diff(new_configs)
        .expect("should detect change");
    assert_eq!(diff.retained, vec!["a"]);
    assert!(diff.added.is_empty());
    assert_eq!(diff.removed, vec!["b"]);
}

#[test]
fn test_update_configs_diff_changed() {
    let configs = vec![make_stdio_server("a", "/bin/a")];
    let mut state = McpState::new(configs);

    let new_configs = vec![make_stdio_server("a", "/bin/a_v2")];
    let diff = state
        .update_configs_diff(new_configs)
        .expect("should detect change");
    assert!(diff.retained.is_empty());
    assert_eq!(diff.added, vec!["a"]);
    assert_eq!(diff.removed, vec!["a"]);
}

#[test]
fn test_update_configs_diff_auth_required_cleanup() {
    let configs = vec![
        make_stdio_server("keep", "/bin/keep"),
        make_stdio_server("remove", "/bin/remove"),
    ];
    let mut state = McpState::new(configs);
    state.auth_required.insert("remove".to_string());
    state.auth_required.insert("keep".to_string());

    let new_configs = vec![make_stdio_server("keep", "/bin/keep")];
    let diff = state
        .update_configs_diff(new_configs)
        .expect("should detect change");
    assert_eq!(diff.retained, vec!["keep"]);
    assert_eq!(diff.removed, vec!["remove"]);
    assert!(state.auth_required.contains("keep"));
    assert!(!state.auth_required.contains("remove"));
}

#[test]
fn test_update_configs_diff_empty_to_nonempty() {
    let mut state = McpState::new(vec![]);
    let new_configs = vec![make_stdio_server("a", "/bin/a")];
    let diff = state
        .update_configs_diff(new_configs)
        .expect("should detect change");
    assert!(diff.retained.is_empty());
    assert_eq!(diff.added, vec!["a"]);
    assert!(diff.removed.is_empty());
}

#[test]
fn test_update_configs_diff_nonempty_to_empty() {
    let configs = vec![make_stdio_server("a", "/bin/a")];
    let mut state = McpState::new(configs);
    let diff = state
        .update_configs_diff(vec![])
        .expect("should detect change");
    assert!(diff.retained.is_empty());
    assert!(diff.added.is_empty());
    assert_eq!(diff.removed, vec!["a"]);
}

#[test]
fn test_mcp_erased_tool_id_is_qualified() {
    use echo_agent_tool_runtime::Tool;

    let mcp_state = Arc::new(Mutex::new(McpState::new(vec![])));

    let tool_a = McpErasedTool {
        tool: McpTool::new(
            "SearchUsers".to_string(),
            "Search users".to_string(),
            "calendar".to_string(),
            Arc::clone(&mcp_state),
            serde_json::json!({"type": "object"}),
            None,
        ),
    };
    let tool_b = McpErasedTool {
        tool: McpTool::new(
            "SearchUsers".to_string(),
            "Search users".to_string(),
            "teams".to_string(),
            Arc::clone(&mcp_state),
            serde_json::json!({"type": "object"}),
            None,
        ),
    };

    let id_a = tool_a.id();
    let id_b = tool_b.id();

    assert_eq!(id_a.as_str(), "calendar__SearchUsers");
    assert_eq!(id_b.as_str(), "teams__SearchUsers");

    assert_ne!(id_a, id_b);
}

#[test]
fn test_same_raw_name_different_servers_no_local_registry_collision() {
    use echo_agent_computer_hub_sdk::LocalRegistry;
    use echo_agent_tool_runtime::Tool;

    let mcp_state = Arc::new(Mutex::new(McpState::new(vec![])));
    let registry = LocalRegistry::new();

    let tool_a = McpErasedTool {
        tool: McpTool::new(
            "SearchUsers".to_string(),
            "Search users on calendar".to_string(),
            "calendar".to_string(),
            Arc::clone(&mcp_state),
            serde_json::json!({"type": "object"}),
            None,
        ),
    };
    let tool_b = McpErasedTool {
        tool: McpTool::new(
            "SearchUsers".to_string(),
            "Search users on teams".to_string(),
            "teams".to_string(),
            Arc::clone(&mcp_state),
            serde_json::json!({"type": "object"}),
            None,
        ),
    };

    let id_a = tool_a.id();
    let id_b = tool_b.id();

    let displaced_a = registry.register(tool_a);
    assert!(
        displaced_a.is_none(),
        "first registration should not displace"
    );

    let displaced_b = registry.register(tool_b);
    assert!(
        displaced_b.is_none(),
        "second registration must not overwrite first"
    );

    assert!(
        registry.find(&id_a).is_some(),
        "calendar tool must be found"
    );
    assert!(registry.find(&id_b).is_some(), "teams tool must be found");
    assert_eq!(registry.len(), 2);
}

fn make_test_client(name: &str) -> Arc<McpClient> {
    Arc::new(McpClient::stub(name))
}

#[test]
fn test_shared_mcp_pool_from_empty_state() {
    let state = McpState::new(vec![]);
    let pool = SharedMcpPool::from_state(&state);
    assert_eq!(pool.len(), 0);
    assert_eq!(pool.server_names().count(), 0);
    assert!(pool.configs().is_empty());
    assert!(pool.meta_config_map().is_empty());
    assert!(pool.get_client("anything").is_none());
}

#[test]
fn test_shared_mcp_pool_len_matches_client_count() {
    let mut state = McpState::new(vec![]);
    for name in ["alpha", "beta", "gamma"] {
        state
            .owned_clients
            .insert(name.to_string(), make_test_client(name));
    }
    let pool = SharedMcpPool::from_state(&state);
    assert_eq!(pool.len(), 3);
    assert_eq!(pool.len(), pool.server_names().count());
}

#[test]
fn test_shared_mcp_pool_snapshot_shares_arc_clients() {
    let mut state = McpState::new(vec![make_stdio_server("github", "/bin/gh")]);
    let client = make_test_client("github");
    state
        .owned_clients
        .insert("github".to_string(), Arc::clone(&client));

    let pool = SharedMcpPool::from_state(&state);
    let pool_client = pool.get_client("github").expect("should find client");

    assert!(Arc::ptr_eq(&client, pool_client));
}

#[test]
fn test_shared_mcp_pool_get_client_missing() {
    let mut state = McpState::new(vec![]);
    state
        .owned_clients
        .insert("a".to_string(), make_test_client("a"));
    let pool = SharedMcpPool::from_state(&state);

    assert!(pool.get_client("a").is_some());
    assert!(pool.get_client("nonexistent").is_none());
    assert!(pool.get_client("").is_none());
}

#[test]
fn test_shared_mcp_pool_server_names() {
    let mut state = McpState::new(vec![]);
    for name in ["alpha", "beta", "gamma"] {
        state
            .owned_clients
            .insert(name.to_string(), make_test_client(name));
    }

    let pool = SharedMcpPool::from_state(&state);
    let mut names: Vec<&str> = pool.server_names().collect();
    names.sort();
    assert_eq!(names, vec!["alpha", "beta", "gamma"]);
}

#[test]
fn test_shared_mcp_pool_snapshot_independent_of_state_mutations() {
    let mut state = McpState::new(vec![make_stdio_server("srv", "/bin/srv")]);
    state
        .owned_clients
        .insert("srv".to_string(), make_test_client("srv"));

    let pool = SharedMcpPool::from_state(&state);

    state.owned_clients.clear();
    state.configs.clear();

    assert_eq!(pool.server_names().count(), 1);
    assert!(pool.get_client("srv").is_some());
    assert_eq!(pool.configs().len(), 1);
}

#[test]
fn test_shared_mcp_pool_meta_config_preserved() {
    let mut meta = McpMetaConfigMap::new();
    meta.insert(
        "github".to_string(),
        McpServerMetaConfig {
            startup_timeout_ms: Some(5000),
            tool_timeout_ms: Some(120000),
            tool_timeouts_ms: None,
            expose_image_base64: None,
        },
    );
    let state = McpState::new_with_meta(vec![make_http_server("github", "http://gh.local")], meta);
    let pool = SharedMcpPool::from_state(&state);

    let mc = pool
        .meta_config_map()
        .get("github")
        .expect("should have meta config");
    assert_eq!(mc.startup_timeout_ms, Some(5000));
    assert_eq!(mc.tool_timeout_ms, Some(120000));
}

#[test]
fn test_shared_mcp_pool_clone_shares_arcs() {
    let mut state = McpState::new(vec![]);
    let client = make_test_client("svc");
    state
        .owned_clients
        .insert("svc".to_string(), Arc::clone(&client));

    let pool = SharedMcpPool::from_state(&state);
    let pool2 = pool.clone();

    let c1 = pool.get_client("svc").unwrap();
    let c2 = pool2.get_client("svc").unwrap();
    assert!(Arc::ptr_eq(c1, c2));
}

#[test]
fn test_get_client_owned_overrides_shared() {
    let mut state = McpState::new(vec![]);
    let shared = make_test_client("srv");
    let owned = make_test_client("srv");
    state
        .shared_clients
        .insert("srv".to_string(), Arc::clone(&shared));
    state
        .owned_clients
        .insert("srv".to_string(), Arc::clone(&owned));

    let got = state.get_client("srv").unwrap();
    assert!(Arc::ptr_eq(got, &owned));
    assert!(!Arc::ptr_eq(got, &shared));
}

#[test]
fn test_get_client_falls_through_to_shared() {
    let mut state = McpState::new(vec![]);
    let shared = make_test_client("srv");
    state
        .shared_clients
        .insert("srv".to_string(), Arc::clone(&shared));

    let got = state.get_client("srv").unwrap();
    assert!(Arc::ptr_eq(got, &shared));
    assert!(state.get_client("missing").is_none());
}

#[test]
fn test_all_clients_deduplicates_shared_by_owned() {
    let mut state = McpState::new(vec![]);
    state
        .owned_clients
        .insert("a".to_string(), make_test_client("a"));
    state
        .shared_clients
        .insert("a".to_string(), make_test_client("a-shared"));
    state
        .shared_clients
        .insert("b".to_string(), make_test_client("b-shared"));

    let all: Vec<_> = state.all_clients().map(|(n, _)| n.as_str()).collect();
    assert_eq!(all.iter().filter(|&&n| n == "a").count(), 1);
    assert!(all.contains(&"b"));
    assert_eq!(all.len(), 2);

    let (_, a_client) = state.all_clients().find(|(n, _)| *n == "a").unwrap();
    assert!(Arc::ptr_eq(a_client, state.owned_clients.get("a").unwrap()));
}

#[test]
fn test_import_shared_clients_skips_config_collisions() {
    let mut state = McpState::new(vec![make_stdio_server("github", "/bin/gh")]);
    let mut pool_clients = HashMap::new();
    pool_clients.insert("github".to_string(), make_test_client("github"));
    pool_clients.insert("linear".to_string(), make_test_client("linear"));
    let pool = SharedMcpPool {
        clients: pool_clients,
        configs: vec![],
        meta_config_map: McpMetaConfigMap::new(),
    };

    state.import_shared_clients(&pool);

    assert!(
        !state.shared_clients.contains_key("github"),
        "github should be skipped — collides with child config"
    );
    assert!(
        state.shared_clients.contains_key("linear"),
        "linear should be imported — no collision"
    );
}

#[test]
fn test_update_configs_preserves_shared_clients() {
    let mut state = McpState::new(vec![make_stdio_server("old", "/bin/old")]);
    state
        .owned_clients
        .insert("old".to_string(), make_test_client("old"));
    let shared = make_test_client("inherited");
    state
        .shared_clients
        .insert("inherited".to_string(), Arc::clone(&shared));

    let changed = state.update_configs(vec![make_stdio_server("new", "/bin/new")]);

    assert!(changed);
    assert!(state.owned_clients.is_empty(), "owned should be cleared");
    assert_eq!(state.shared_clients.len(), 1, "shared should be untouched");
    assert!(Arc::ptr_eq(
        state.shared_clients.get("inherited").unwrap(),
        &shared
    ));
}

#[test]
fn test_update_configs_diff_preserves_shared_clients() {
    let mut state = McpState::new(vec![
        make_stdio_server("keep", "/bin/keep"),
        make_stdio_server("drop", "/bin/drop"),
    ]);
    state
        .owned_clients
        .insert("keep".to_string(), make_test_client("keep"));
    state
        .owned_clients
        .insert("drop".to_string(), make_test_client("drop"));
    let shared = make_test_client("inherited");
    state
        .shared_clients
        .insert("inherited".to_string(), Arc::clone(&shared));

    let diff = state
        .update_configs_diff(vec![make_stdio_server("keep", "/bin/keep")])
        .expect("configs changed");

    assert!(diff.removed.contains(&"drop".to_string()));
    assert!(diff.retained.contains(&"keep".to_string()));
    assert!(!state.owned_clients.contains_key("drop"));
    assert!(state.owned_clients.contains_key("keep"));
    assert!(Arc::ptr_eq(
        state.shared_clients.get("inherited").unwrap(),
        &shared
    ));
}

#[test]
fn test_from_state_captures_both_owned_and_shared() {
    let mut state = McpState::new(vec![]);
    let owned = make_test_client("owned-srv");
    let shared = make_test_client("shared-srv");
    state
        .owned_clients
        .insert("owned-srv".to_string(), Arc::clone(&owned));
    state
        .shared_clients
        .insert("shared-srv".to_string(), Arc::clone(&shared));

    let pool = SharedMcpPool::from_state(&state);

    assert!(Arc::ptr_eq(pool.get_client("owned-srv").unwrap(), &owned));
    assert!(Arc::ptr_eq(pool.get_client("shared-srv").unwrap(), &shared));
    assert_eq!(pool.server_names().count(), 2);
}

#[test]
fn test_retain_clients_keeps_matching() {
    let mut state = McpState::new(vec![]);
    for name in ["github", "linear", "slack"] {
        state
            .owned_clients
            .insert(name.to_string(), make_test_client(name));
    }
    let mut pool = SharedMcpPool::from_state(&state);

    pool.retain_clients(|name| name == "github" || name == "slack");

    assert!(pool.get_client("github").is_some());
    assert!(pool.get_client("slack").is_some());
    assert!(pool.get_client("linear").is_none());
    assert_eq!(pool.server_names().count(), 2);
}

#[test]
fn test_retain_clients_remove_all() {
    let mut state = McpState::new(vec![]);
    state
        .owned_clients
        .insert("srv".to_string(), make_test_client("srv"));
    let mut pool = SharedMcpPool::from_state(&state);

    pool.retain_clients(|_| false);

    assert_eq!(pool.server_names().count(), 0);
    assert!(pool.get_client("srv").is_none());
}

#[test]
fn test_retain_clients_keep_all() {
    let mut state = McpState::new(vec![]);
    for name in ["a", "b", "c"] {
        state
            .owned_clients
            .insert(name.to_string(), make_test_client(name));
    }
    let mut pool = SharedMcpPool::from_state(&state);

    pool.retain_clients(|_| true);

    assert_eq!(pool.server_names().count(), 3);
}

#[test]
fn test_retain_clients_preserves_arc_identity() {
    let mut state = McpState::new(vec![]);
    let client = make_test_client("keep");
    state
        .owned_clients
        .insert("keep".to_string(), Arc::clone(&client));
    state
        .owned_clients
        .insert("drop".to_string(), make_test_client("drop"));
    let mut pool = SharedMcpPool::from_state(&state);

    pool.retain_clients(|name| name == "keep");

    assert!(Arc::ptr_eq(pool.get_client("keep").unwrap(), &client));
}

fn make_mcp_tool(server_name: &str, name: &str) -> McpTool {
    McpTool::new(
        name.to_string(),
        "test desc".to_string(),
        server_name.to_string(),
        Arc::new(Mutex::new(McpState::new(vec![]))),
        serde_json::json!({}),
        None,
    )
}

#[test]
fn qualified_mcp_name_parser_accepts_structurally_valid_tool_ids() {
    for (name, expected) in [
        ("linear__list_issues", ("linear", "list_issues")),
        ("123__lookup", ("123", "lookup")),
        ("server:scope__tool", ("server:scope", "tool")),
    ] {
        let (id, server, tool) = parse_mcp_qualified_name(name).expect("valid qualified ID");
        assert_eq!(id.as_str(), name);
        assert_eq!((server, tool), expected);
        assert_eq!(
            parse_mcp_tool_name(name),
            Some((expected.0.to_owned(), expected.1.to_owned()))
        );
    }
}

#[test]
fn qualified_mcp_name_parser_rejects_malformed_names() {
    for name in [
        "server__part__tool",
        "server__tool__part",
        "foo___bar",
        "foo____bar",
        "__tool",
        "server__",
        "server",
        "",
        "server__bad.tool",
    ] {
        assert!(
            parse_mcp_qualified_name(name).is_none(),
            "unexpectedly accepted {name:?}"
        );
    }
}

#[test]
fn into_registration_validates_qualified_name() {
    let registration = make_mcp_tool("linear", "list_issues")
        .into_registration()
        .expect("should register");
    assert_eq!(registration.name, "linear__list_issues");

    for (server, tool) in [
        ("server__part", "tool"),
        ("server", "tool__part"),
        ("foo_", "bar"),
        ("foo", "_bar"),
        ("foo_", "_bar"),
        ("", "tool"),
        ("server", ""),
    ] {
        assert!(
            make_mcp_tool(server, tool).into_registration().is_none(),
            "unexpectedly registered {server:?} and {tool:?}"
        );
    }
}

#[test]
fn into_registration_preserves_provider_name_policy() {
    for qualified in ["123__lookup", "server:scope__tool"] {
        assert!(parse_mcp_qualified_name(qualified).is_some());
        let (server, tool) = qualified.split_once("__").unwrap();
        assert!(make_mcp_tool(server, tool).into_registration().is_none());
    }

    let server_61 = format!("a{}", "b".repeat(60));
    let server_62 = format!("a{}", "b".repeat(61));
    let valid_64 = format!("{server_61}__b");
    let invalid_65 = format!("{server_62}__b");
    assert_eq!(valid_64.len(), 64);
    assert_eq!(invalid_65.len(), 65);
    assert!(parse_mcp_qualified_name(&valid_64).is_some());
    assert!(parse_mcp_qualified_name(&invalid_65).is_some());
    assert!(make_mcp_tool(&server_61, "b").into_registration().is_some());
    assert!(make_mcp_tool(&server_62, "b").into_registration().is_none());
}

#[test]
fn test_is_retriable_transport_closed() {
    assert!(is_retriable_transport_error(&ServiceError::TransportClosed));
}

#[test]
fn test_is_retriable_transport_send() {
    let err = ServiceError::TransportSend(rmcp::transport::DynamicTransportError::from_parts(
        "test",
        std::any::TypeId::of::<()>(),
        Box::new(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "connection reset",
        )),
    ));
    assert!(is_retriable_transport_error(&err));
}

#[test]
fn test_not_retriable_unexpected_response() {
    assert!(!is_retriable_transport_error(
        &ServiceError::UnexpectedResponse
    ));
}

#[test]
fn test_not_retriable_cancelled() {
    assert!(!is_retriable_transport_error(&ServiceError::Cancelled {
        reason: Some("shutdown".to_string()),
    }));
}

#[test]
fn test_not_retriable_timeout() {
    assert!(!is_retriable_transport_error(&ServiceError::Timeout {
        timeout: std::time::Duration::from_secs(30),
    }));
}

fn mcp_service_err(code: i32) -> ServiceError {
    ServiceError::McpError(rmcp::ErrorData::new(
        rmcp::model::ErrorCode(code),
        "boom",
        None,
    ))
}

#[test]
fn should_recover_mcp_error_recovers_everything_outside_excluded_set() {
    assert!(should_recover_mcp_error(-32603));
    assert!(should_recover_mcp_error(-32002));
    assert!(should_recover_mcp_error(-32000));
    assert!(should_recover_mcp_error(-32099));
    assert!(should_recover_mcp_error(-32100));
    assert!(should_recover_mcp_error(0));
    assert!(should_recover_mcp_error(1));
    assert!(should_recover_mcp_error(i32::MIN));
    assert!(should_recover_mcp_error(i32::MAX));
}

#[test]
fn should_recover_mcp_error_skips_deterministic_client_errors() {
    assert!(!should_recover_mcp_error(-32700));
    assert!(!should_recover_mcp_error(-32600));
    assert!(!should_recover_mcp_error(-32601));
    assert!(!should_recover_mcp_error(-32602));
}

#[test]
fn should_recover_service_error_http_mcperror_recoverable() {
    assert!(should_recover_service_error(
        &mcp_service_err(-32603),
        true,
        false,
    ));
}

#[test]
fn should_recover_service_error_http_mcperror_invalid_params_skipped() {
    assert!(!should_recover_service_error(
        &mcp_service_err(-32602),
        true,
        false,
    ));
}

#[test]
fn should_recover_service_error_stdio_mcperror_not_recovered() {
    assert!(!should_recover_service_error(
        &mcp_service_err(-32603),
        false,
        false,
    ));
}

#[test]
fn should_recover_service_error_mcperror_at_most_once_per_dispatch() {
    assert!(!should_recover_service_error(
        &mcp_service_err(-32603),
        true,
        true,
    ));
}

#[test]
fn should_recover_service_error_http_mcperror_auth_rejection_not_recovered() {
    let auth_err = ServiceError::McpError(rmcp::ErrorData::new(
        rmcp::model::ErrorCode(-32603),
        "Unauthorized: token expired",
        None,
    ));
    assert!(!should_recover_service_error(&auth_err, true, false));
    let session_err = ServiceError::McpError(rmcp::ErrorData::new(
        rmcp::model::ErrorCode(-32603),
        "session not found",
        None,
    ));
    assert!(should_recover_service_error(&session_err, true, false));
}

#[test]
fn should_recover_service_error_transport_errors_always_recover() {
    assert!(should_recover_service_error(
        &ServiceError::TransportClosed,
        true,
        false
    ));
    assert!(should_recover_service_error(
        &ServiceError::TransportClosed,
        false,
        false
    ));
    assert!(should_recover_service_error(
        &ServiceError::TransportClosed,
        true,
        true
    ));
}

#[test]
fn should_recover_service_error_other_non_transport_not_recovered() {
    assert!(!should_recover_service_error(
        &ServiceError::UnexpectedResponse,
        true,
        false
    ));
    assert!(!should_recover_service_error(
        &ServiceError::Timeout {
            timeout: std::time::Duration::from_secs(30),
        },
        true,
        false
    ));
}

#[tokio::test]
async fn recover_and_retry_surfaces_original_error_when_recover_fails() {
    let config = HttpConfig {
        url: "http://192.0.2.1:1/unreachable".to_string(),
        headers: vec![],
    };
    let overrides = McpClientTimeoutOverrides {
        startup_timeout_sec: Some(1),
        ..Default::default()
    };
    let client = Arc::new(McpClient::new_http(
        "wedged".to_string(),
        config,
        Some(&overrides),
        None,
    ));

    let tool = McpErasedTool {
        tool: McpTool::new(
            "do_thing".to_string(),
            "desc".to_string(),
            "wedged".to_string(),
            Arc::new(Mutex::new(McpState::new(vec![]))),
            serde_json::json!({"type": "object"}),
            None,
        ),
    };

    let original = mcp_service_err(-32603);
    let expected = original.to_string();
    let params = CallToolRequestParams::new("do_thing");

    let mut reconnect_attempted = false;
    let mut is_timeout = false;
    let ew = echo_agent_session_events::EventWriter::noop();

    let err = tool
        .recover_and_retry(
            &client,
            params,
            std::time::Duration::from_secs(1),
            1,
            original,
            &mut reconnect_attempted,
            &mut is_timeout,
            &ew,
        )
        .await
        .expect_err("recover must fail against an unreachable host");

    assert_eq!(err.to_string(), expected, "original error must be surfaced");
    assert!(reconnect_attempted, "reconnect attempt must be flagged");
    assert!(!is_timeout, "a recover failure is not a tool timeout");
}

use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Copy)]
enum CallToolBehavior {
    ErrorThenOk { code: i32 },
    AlwaysError { code: i32 },
    HangThenOk { hang_ms: u64 },
    ErrorThenHang { code: i32, hang_ms: u64 },
}

#[derive(Clone)]
struct FakeMcpHandles {
    inits: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
    init_version: Arc<parking_lot::Mutex<Option<String>>>,
    init_user_agents: Arc<parking_lot::Mutex<Vec<String>>>,
}

fn header_values(
    headers: &axum::http::HeaderMap,
    name: axum::http::header::HeaderName,
) -> Vec<String> {
    headers
        .get_all(name)
        .iter()
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
        .collect()
}

#[derive(Clone)]
struct FakeMcpState {
    behavior: CallToolBehavior,
    handles: FakeMcpHandles,
}

async fn fake_handle_post(
    axum::extract::State(state): axum::extract::State<FakeMcpState>,
    headers: axum::http::HeaderMap,
    axum::Json(req): axum::Json<serde_json::Value>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let id = req["id"].clone();
    let ok = || {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id.clone(),
            "result": {"content": [{"type": "text", "text": "ok"}], "isError": false},
        })
    };
    let err = |code: i32, msg: String| {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id.clone(),
            "error": {"code": code, "message": msg},
        })
    };
    match req["method"].as_str() {
        Some("initialize") => {
            state.handles.inits.fetch_add(1, Ordering::Relaxed);
            *state.handles.init_version.lock() =
                req["params"]["protocolVersion"].as_str().map(str::to_owned);
            state
                .handles
                .init_user_agents
                .lock()
                .extend(header_values(&headers, axum::http::header::USER_AGENT));
            let result = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id.clone(),
                "result": {
                    "protocolVersion": req["params"]["protocolVersion"].clone(),
                    "capabilities": {},
                    "serverInfo": {"name": "fake", "version": "0.0.0"},
                },
            });
            ([("mcp-session-id", "fake-session")], axum::Json(result)).into_response()
        }
        Some("tools/list") => axum::Json(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id.clone(),
            "result": {"tools": [{"name": "echo", "inputSchema": {"type": "object"}}]},
        }))
        .into_response(),
        Some("tools/call") => {
            let n = state.handles.calls.fetch_add(1, Ordering::Relaxed);
            match state.behavior {
                CallToolBehavior::ErrorThenOk { code } => {
                    if n == 0 {
                        axum::Json(err(code, "session expired".to_string())).into_response()
                    } else {
                        axum::Json(ok()).into_response()
                    }
                }
                CallToolBehavior::AlwaysError { code } => {
                    axum::Json(err(code, format!("attempt {}", n + 1))).into_response()
                }
                CallToolBehavior::HangThenOk { hang_ms } => {
                    if n == 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(hang_ms)).await;
                    }
                    axum::Json(ok()).into_response()
                }
                CallToolBehavior::ErrorThenHang { code, hang_ms } => {
                    if n == 0 {
                        axum::Json(err(code, "session expired".to_string())).into_response()
                    } else {
                        tokio::time::sleep(std::time::Duration::from_millis(hang_ms)).await;
                        axum::Json(ok()).into_response()
                    }
                }
            }
        }
        _ => axum::http::StatusCode::ACCEPTED.into_response(),
    }
}

async fn fake_handle_get() -> axum::response::Response {
    use axum::response::IntoResponse;
    let body =
        axum::body::Body::from_stream(futures::stream::pending::<Result<String, std::io::Error>>());
    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        body,
    )
        .into_response()
}

async fn spawn_fake_mcp(behavior: CallToolBehavior) -> (String, FakeMcpHandles) {
    let handles = FakeMcpHandles {
        inits: Arc::new(AtomicUsize::new(0)),
        calls: Arc::new(AtomicUsize::new(0)),
        init_version: Arc::new(parking_lot::Mutex::new(None)),
        init_user_agents: Arc::new(parking_lot::Mutex::new(Vec::new())),
    };
    let app = axum::Router::new()
        .route(
            "/mcp",
            axum::routing::get(fake_handle_get).post(fake_handle_post),
        )
        .with_state(FakeMcpState {
            behavior,
            handles: handles.clone(),
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/mcp"), handles)
}

fn fake_http_client(url: &str, tool_timeout_sec: u64) -> Arc<McpClient> {
    let overrides = McpClientTimeoutOverrides {
        startup_timeout_sec: Some(5),
        tool_timeout_sec: Some(tool_timeout_sec),
        ..Default::default()
    };
    Arc::new(McpClient::new_http(
        "fake".to_string(),
        HttpConfig {
            url: url.to_string(),
            headers: vec![],
        },
        Some(&overrides),
        None,
    ))
}

fn fake_echo_tool() -> McpErasedTool {
    McpErasedTool {
        tool: McpTool::new(
            "echo".to_string(),
            "echo desc".to_string(),
            "fake".to_string(),
            Arc::new(Mutex::new(McpState::new(vec![]))),
            serde_json::json!({"type": "object"}),
            None,
        ),
    }
}

fn event_types(jsonl: &str) -> Vec<serde_json::Value> {
    jsonl
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn http_transport_sends_default_user_agent_on_initialize() {
    let (url, handles) = spawn_fake_mcp(CallToolBehavior::HangThenOk { hang_ms: 0 }).await;
    let client = fake_http_client(&url, 5);
    client.ensure_initialized().await.expect("handshake");
    assert_eq!(
        *handles.init_user_agents.lock(),
        vec![format!("echo-agent-cli/{}", echo_agent_version::VERSION)]
    );
}

async fn desktop_coding_call_without_replay(
    behavior: CallToolBehavior,
    timeout: u64,
    protocol_terminal: bool,
) {
    let (url, handles) = spawn_fake_mcp(behavior).await;
    let client = fake_http_client(&url, timeout);
    let tool = fake_echo_tool();
    tool.tool
        .mcp_state
        .lock()
        .await
        .shared_clients
        .insert("fake".into(), client);
    let mut resources = echo_agent_tools::types::resources::Resources::new();
    resources.insert(echo_agent_tools::types::resources::DesktopMcpNoReplay(true));
    let shared: echo_agent_tools::types::resources::SharedResources =
        Arc::new(Mutex::new(resources));
    let mut context = echo_agent_tool_runtime::ToolCallContext::default();
    context.extensions.insert(shared);
    let result = echo_agent_tool_runtime::Tool::run(&tool, context, serde_json::json!({})).await;
    if protocol_terminal {
        assert!(
            matches!(result, Ok(ToolOutput::MCP(ref output))
            if matches!(output.output(), MCPOutputDetails::Error(_))),
            "a real protocol error must be a failed terminal, never a successful replay"
        );
    } else {
        assert!(
            result.is_err(),
            "an uncertain call must be surfaced, not replaced by a successful replay"
        );
    }
    assert_eq!(
        handles.calls.load(Ordering::Relaxed),
        1,
        "Coding may issue exactly one real tools/call"
    );
    assert_eq!(
        handles.inits.load(Ordering::Relaxed),
        1,
        "Coding must not reconnect/reinitialize and replay the ambiguous request"
    );
}
#[tokio::test(flavor = "multi_thread")]
async fn desktop_coding_mcp_no_replay_after_real_rpc_server_error() {
    desktop_coding_call_without_replay(CallToolBehavior::ErrorThenOk { code: -32603 }, 5, true)
        .await;
}
#[tokio::test(flavor = "multi_thread")]
async fn desktop_coding_mcp_no_replay_after_real_rpc_timeout() {
    desktop_coding_call_without_replay(CallToolBehavior::HangThenOk { hang_ms: 3000 }, 1, false)
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn try_call_tool_http_mcperror_recovers_then_retry_succeeds() {
    let (url, handles) = spawn_fake_mcp(CallToolBehavior::ErrorThenOk { code: -32603 }).await;
    let client = fake_http_client(&url, 5);
    let tool = fake_echo_tool();
    let tmp = tempfile::tempdir().unwrap();
    let ew = echo_agent_session_events::EventWriter::open(tmp.path());

    let mut reconnect = false;
    let mut is_timeout = false;
    let raw = serde_json::json!({});
    let out = tool
        .try_call_tool(&client, &raw, &mut reconnect, &mut is_timeout, &ew)
        .await
        .expect("recovered call should succeed");

    assert!(
        !out.is_error.unwrap_or(false),
        "retry should return a success result"
    );
    assert!(reconnect, "reconnect_attempted must be set");
    assert!(!is_timeout);
    assert_eq!(
        handles.calls.load(Ordering::Relaxed),
        2,
        "one failed + one retried tools/call"
    );
    assert_eq!(
        handles.inits.load(Ordering::Relaxed),
        2,
        "initial handshake + one recovery re-init"
    );
    assert_eq!(
        handles.init_version.lock().as_deref(),
        Some("2025-11-25"),
        "initialize must offer protocolVersion 2025-11-25"
    );

    let jsonl = std::fs::read_to_string(tmp.path().join("events.jsonl")).unwrap();
    let events = event_types(&jsonl);
    assert!(
        events.iter().any(|e| e["type"] == "mcp_transport_error"),
        "expected mcp_transport_error in {jsonl}"
    );
    assert!(
        events
            .iter()
            .any(|e| e["type"] == "mcp_transport_reconnect" && e["success"] == true),
        "expected a successful mcp_transport_reconnect in {jsonl}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn try_call_tool_http_retry_failure_surfaces_retry_error() {
    let (url, handles) = spawn_fake_mcp(CallToolBehavior::AlwaysError { code: -32603 }).await;
    let client = fake_http_client(&url, 5);
    let tool = fake_echo_tool();
    let ew = echo_agent_session_events::EventWriter::noop();

    let mut reconnect = false;
    let mut is_timeout = false;
    let raw = serde_json::json!({});
    let err = tool
        .try_call_tool(&client, &raw, &mut reconnect, &mut is_timeout, &ew)
        .await
        .expect_err("both attempts fail");

    let msg = err.to_string();
    assert!(msg.contains("attempt 2"), "want retry error, got: {msg}");
    assert!(
        !msg.contains("attempt 1"),
        "must not surface the original error: {msg}"
    );
    assert!(reconnect);
    assert!(!is_timeout);
    assert_eq!(
        handles.calls.load(Ordering::Relaxed),
        2,
        "one failed + one retried tools/call"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn try_call_tool_http_invalid_params_not_recovered() {
    let (url, handles) = spawn_fake_mcp(CallToolBehavior::AlwaysError { code: -32602 }).await;
    let client = fake_http_client(&url, 5);
    let tool = fake_echo_tool();
    let ew = echo_agent_session_events::EventWriter::noop();

    let mut reconnect = false;
    let mut is_timeout = false;
    let raw = serde_json::json!({});
    let err = tool
        .try_call_tool(&client, &raw, &mut reconnect, &mut is_timeout, &ew)
        .await
        .expect_err("invalid params surfaced as-is");

    assert!(err.to_string().contains("attempt 1"), "got: {err}");
    assert!(!reconnect, "invalid-params must not trigger recovery");
    assert!(!is_timeout);
    assert_eq!(handles.calls.load(Ordering::Relaxed), 1, "no retry POST");
    assert_eq!(
        handles.inits.load(Ordering::Relaxed),
        1,
        "no recovery re-init"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn try_call_tool_http_outer_timeout_resets_transport_no_retry() {
    let (url, handles) = spawn_fake_mcp(CallToolBehavior::HangThenOk { hang_ms: 3000 }).await;
    let client = fake_http_client(&url, 1);
    let tool = fake_echo_tool();
    let ew = echo_agent_session_events::EventWriter::noop();

    let mut reconnect = false;
    let mut is_timeout = false;
    let raw = serde_json::json!({});
    let err = tool
        .try_call_tool(&client, &raw, &mut reconnect, &mut is_timeout, &ew)
        .await
        .expect_err("call must time out");

    assert!(err.to_string().contains("timed out"), "got: {err}");
    assert!(is_timeout, "is_timeout must be set");
    assert!(reconnect, "timeout arm flags the reconnect after resetting");
    assert_eq!(
        handles.calls.load(Ordering::Relaxed),
        1,
        "timed-out call is NOT retried"
    );
    assert!(matches!(
        client.state_kind().await,
        ClientStateKind::Pending
    ));
    assert_eq!(
        handles.inits.load(Ordering::Relaxed),
        1,
        "no re-init during the timed-out dispatch"
    );

    let mut reconnect2 = false;
    let mut is_timeout2 = false;
    let out = tool
        .try_call_tool(&client, &raw, &mut reconnect2, &mut is_timeout2, &ew)
        .await
        .expect("second dispatch should re-init and succeed");
    assert!(!out.is_error.unwrap_or(false));
    assert!(!is_timeout2);
    assert_eq!(
        handles.inits.load(Ordering::Relaxed),
        2,
        "second dispatch re-initialized the session"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn try_call_tool_http_retry_timeout_surfaces_timeout() {
    let (url, handles) = spawn_fake_mcp(CallToolBehavior::ErrorThenHang {
        code: -32603,
        hang_ms: 3000,
    })
    .await;
    let client = fake_http_client(&url, 1);
    let tool = fake_echo_tool();
    let ew = echo_agent_session_events::EventWriter::noop();

    let mut reconnect = false;
    let mut is_timeout = false;
    let raw = serde_json::json!({});
    let err = tool
        .try_call_tool(&client, &raw, &mut reconnect, &mut is_timeout, &ew)
        .await
        .expect_err("the retried call must time out");

    assert!(err.to_string().contains("timed out"), "got: {err}");
    assert!(is_timeout, "retry-timeout must set is_timeout");
    assert!(reconnect, "recovery was attempted");
    assert_eq!(
        handles.calls.load(Ordering::Relaxed),
        2,
        "the retry tools/call was attempted"
    );
    assert_eq!(
        handles.inits.load(Ordering::Relaxed),
        2,
        "recovery re-initialized before the retry"
    );
}

#[test]
fn test_new_http_stores_http_config() {
    let config = HttpConfig {
        url: "http://localhost:5000/api/mcp".to_string(),
        headers: vec![("x-token".to_string(), "abc".to_string())],
    };
    let client = McpClient::new_http("example-mcp".to_string(), config, None, None);
    let stored = client
        .http_config
        .as_ref()
        .expect("http_config should be Some");
    assert_eq!(stored.url, "http://localhost:5000/api/mcp");
    assert_eq!(stored.headers.len(), 1);
    assert_eq!(stored.headers[0].0, "x-token");
}

#[test]
fn test_new_stdio_has_no_http_config() {
    let client = McpClient::stub("stdio-srv");
    assert!(client.http_config.is_none());
}

#[tokio::test]
async fn test_reset_transport_succeeds_for_http_client() {
    let config = HttpConfig {
        url: "http://127.0.0.1:9/api/mcp".to_string(),
        headers: vec![],
    };
    let client = McpClient::new_http("example-mcp".to_string(), config, None, None);
    assert!(client.reset_transport().await);
}

#[tokio::test]
async fn test_reset_transport_fails_for_stub() {
    let client = McpClient::stub("stdio-srv");
    assert!(!client.reset_transport().await);
}

#[tokio::test]
async fn test_reset_transport_is_idempotent() {
    let config = HttpConfig {
        url: "http://127.0.0.1:9/api/mcp".to_string(),
        headers: vec![],
    };
    let client = McpClient::new_http("example-mcp".to_string(), config, None, None);

    assert!(client.reset_transport().await);
    assert!(client.reset_transport().await);
    assert!(client.reset_transport().await);
}

#[tokio::test]
async fn test_reset_transport_makes_ensure_initialized_retry_handshake() {
    let config = HttpConfig {
        url: "http://127.0.0.1:1/unreachable".to_string(),
        headers: vec![],
    };
    let client = McpClient::new_http("test".to_string(), config, None, None);

    let err1 = client.ensure_initialized().await.unwrap_err();
    assert!(
        matches!(
            err1,
            McpError::Timeout { .. } | McpError::HandshakeFailed { .. }
        ),
        "first init should fail: {err1}"
    );

    assert!(client.reset_transport().await);

    let err2 = client.ensure_initialized().await.unwrap_err();
    assert!(
        matches!(
            err2,
            McpError::Timeout { .. } | McpError::HandshakeFailed { .. }
        ),
        "second init after reset should also attempt handshake: {err2}"
    );
}

#[tokio::test]
async fn recover_errors_for_client_with_no_restorable_transport() {
    let err = Arc::new(McpClient::stub("stdio"))
        .recover()
        .await
        .unwrap_err();
    assert!(matches!(err, McpError::ClientError(_)), "got {err}");
}

#[tokio::test]
async fn reset_transport_rebuilds_acp_client() {
    use crate::acp_transport::AcpReverseInvoker;
    use std::time::Duration;

    struct NoopInvoker;
    #[async_trait::async_trait]
    impl AcpReverseInvoker for NoopInvoker {
        async fn invoke(
            &self,
            _server_id: &str,
            _message: serde_json::Value,
            _timeout: Duration,
        ) -> Result<serde_json::Value, String> {
            Ok(serde_json::Value::Null)
        }
    }

    let client = McpClient::new_acp(
        "sdk-tools".to_string(),
        "srv_0".to_string(),
        Arc::new(NoopInvoker),
        None,
        None,
    );

    assert!(client.reset_transport().await);
    assert!(
        matches!(
            &*client.state.lock().await,
            ClientState::Pending(PendingTransport::Acp { .. })
        ),
        "reset_transport should restore the ACP transport to Pending"
    );
}

#[tokio::test]
async fn try_call_tool_reconnects_then_succeeds_after_retriable_transport_error() {
    use crate::acp_transport::AcpReverseInvoker;
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    struct EchoSdkServer;
    #[async_trait::async_trait]
    impl AcpReverseInvoker for EchoSdkServer {
        async fn invoke(
            &self,
            _server_id: &str,
            message: serde_json::Value,
            _timeout: Duration,
        ) -> Result<serde_json::Value, String> {
            let id = message
                .get("id")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let method = message
                .get("method")
                .and_then(|m| m.as_str())
                .unwrap_or_default();
            let result = match method {
                "initialize" => serde_json::json!({
                    "protocolVersion": message["params"]["protocolVersion"],
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "echo", "version": "0.0.0" },
                }),
                "tools/call" => serde_json::json!({
                    "content": [{
                        "type": "text",
                        "text": message["params"]["arguments"]["text"]
                            .as_str()
                            .unwrap_or_default(),
                    }],
                    "isError": false,
                }),
                other => return Err(format!("unexpected method {other}")),
            };
            Ok(serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }))
        }
    }

    async fn dead_service() -> McpService {
        let (client_read, server_write) = tokio::io::duplex(64 * 1024);
        let (server_read, client_write) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            let mut reader = BufReader::new(server_read);
            let mut writer = server_write;
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                    return;
                }
                let Ok(msg) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
                    continue;
                };
                if msg.get("method").and_then(|m| m.as_str()) == Some("initialize") {
                    let id = msg.get("id").cloned().unwrap_or(serde_json::Value::Null);
                    let resp = serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": {
                        "protocolVersion": msg["params"]["protocolVersion"],
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "dead", "version": "0.0.0" },
                    }});
                    let mut encoded = serde_json::to_string(&resp).unwrap();
                    encoded.push('\n');
                    let _ = writer.write_all(encoded.as_bytes()).await;
                    let _ = writer.flush().await;
                    let _ = reader.read_line(&mut line).await;
                    return;
                }
            }
        });
        let handler = EchoAgentClientHandler {
            info: McpClient::make_client_info("dead", /* advertise_elicitation */ true),
            server_name: "dead".to_string(),
            notify_tx: Arc::new(parking_lot::Mutex::new(None)),
            elicitation_tx: Arc::new(parking_lot::Mutex::new(None)),
        };
        let transport = rmcp::transport::async_rw::AsyncRwTransport::<RoleClient, _, _>::new(
            client_read,
            client_write,
        );
        Arc::new(
            handler
                .serve(transport)
                .await
                .expect("dead-service handshake"),
        )
    }

    let client = Arc::new(McpClient::new_acp(
        "sdk".to_string(),
        "srv_0".to_string(),
        Arc::new(EchoSdkServer),
        None,
        None,
    ));
    let dead = dead_service().await;
    *client.state.lock().await = ClientState::Ready {
        service: dead,
        _connected: echo_agent_telemetry::activity::MCP_SERVERS_CONNECTED.enter(),
    };

    let erased = McpErasedTool {
        tool: McpTool::new(
            "echo".to_string(),
            "echo".to_string(),
            "sdk".to_string(),
            Arc::new(Mutex::new(McpState::new(vec![]))),
            serde_json::json!({}),
            None,
        ),
    };

    let raw = serde_json::json!({ "text": "after reconnect" });
    let mut reconnect_attempted = false;
    let mut is_timeout = false;
    let ew = echo_agent_session_events::EventWriter::noop();
    let result = erased
        .try_call_tool(
            &client,
            &raw,
            &mut reconnect_attempted,
            &mut is_timeout,
            &ew,
        )
        .await
        .expect("retry after reconnect should succeed");

    assert_eq!(
        result.content[0].as_text().expect("text content").text,
        "after reconnect"
    );
    assert!(
        reconnect_attempted,
        "retriable transport error must set reconnect_attempted"
    );
    assert!(
        !is_timeout,
        "successful retry must not be flagged as timeout"
    );
    assert!(matches!(
        &*client.state.lock().await,
        ClientState::Ready { .. }
    ));
    assert!(
        echo_agent_telemetry::activity::MCP_SERVERS_CONNECTED.get() >= 1,
        "a Ready client must hold a connected-gauge slot"
    );
}

async fn watched_live_client(name: &str) -> Arc<McpClient> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let (client_read, server_write) = tokio::io::duplex(64 * 1024);
    let (server_read, client_write) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        let mut reader = BufReader::new(server_read);
        let mut writer = server_write;
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                return;
            }
            let Ok(msg) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
                continue;
            };
            if msg.get("method").and_then(|m| m.as_str()) == Some("initialize") {
                let id = msg.get("id").cloned().unwrap_or(serde_json::Value::Null);
                let resp = serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": {
                    "protocolVersion": msg["params"]["protocolVersion"],
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "live", "version": "0.0.0" },
                }});
                let mut encoded = serde_json::to_string(&resp).unwrap();
                encoded.push('\n');
                let _ = writer.write_all(encoded.as_bytes()).await;
                let _ = writer.flush().await;
            }
        }
    });
    let handler = EchoAgentClientHandler {
        info: McpClient::make_client_info(name, /* advertise_elicitation */ true),
        server_name: name.to_string(),
        notify_tx: Arc::new(parking_lot::Mutex::new(None)),
        elicitation_tx: Arc::new(parking_lot::Mutex::new(None)),
    };
    let transport = rmcp::transport::async_rw::AsyncRwTransport::<RoleClient, _, _>::new(
        client_read,
        client_write,
    );
    let service: McpService = Arc::new(
        handler
            .serve(transport)
            .await
            .expect("live-service handshake"),
    );

    let client = Arc::new(McpClient::new_http(
        name.to_string(),
        HttpConfig {
            url: "http://127.0.0.1:0/".to_string(),
            headers: Vec::new(),
        },
        None,
        None,
    ));
    *client.state.lock().await = ClientState::Ready {
        service,
        _connected: echo_agent_telemetry::activity::MCP_SERVERS_CONNECTED.enter(),
    };
    let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
    client.set_event_tx(Some(event_tx));
    assert!(
        client
            .arm_liveness_watcher(std::time::Duration::from_secs(3600))
            .await,
        "watcher must arm on a healthy Ready client"
    );
    client
}

async fn assert_watcher_releases(weak: std::sync::Weak<McpClient>, what: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while weak.upgrade().is_some() {
        assert!(
            std::time::Instant::now() < deadline,
            "{what} must cancel the watcher and release its client Arc"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn evicting_a_watched_client_releases_the_watcher_arc() {
    let client = watched_live_client("evict").await;
    let weak = Arc::downgrade(&client);
    let mut owned = crate::owned_clients::OwnedClients::new();
    owned.insert("evict".to_string(), client);
    owned.remove("evict");
    assert_watcher_releases(weak, "eviction").await;
}

#[tokio::test]
async fn dropping_the_owned_map_releases_watched_clients() {
    let client = watched_live_client("teardown").await;
    let weak = Arc::downgrade(&client);
    let mut owned = crate::owned_clients::OwnedClients::new();
    owned.insert("teardown".to_string(), client);
    drop(owned);
    assert_watcher_releases(weak, "map teardown").await;
}

/// Regression contract for the orphan-MCP-process bug.
///
/// Symptom: opening a new conversation spawned a fresh stdio child but
/// never reaped the previous one, leading to dozens of `node`/`npx`
/// grandchildren accumulating in Task Manager.
///
/// Root cause: `OwnedClients::clear` only cancelled the liveness watcher
/// and relied on `Arc<McpClient>` count hitting zero to drop the
/// `McpClient` → `SafeTokioChildProcess` → kill the child. Any other
/// holder of the Arc (e.g. `McpState::shared_clients`, a subagent pool
/// snapshot, a stale task reference) keeps the Arc alive and the child
/// orphaned forever.
///
/// Contract: when an `OwnedClients` map evicts a client, the child's
/// stdio process must die **immediately**, regardless of whether other
/// Arc references to the McpClient still exist.
#[cfg(unix)]
#[tokio::test]
async fn owned_clients_clear_reaps_stdio_child_even_with_external_arc() {
    use std::time::{Duration, Instant};

    let mut cmd = Command::new("sleep");
    cmd.arg("600").kill_on_drop(true);
    echo_agent_tools::util::detach_command(&mut cmd);
    let (transport, _stderr) = SafeTokioChildProcess::spawn(
        cmd,
        None,
        "reap-test".to_string(),
        echo_agent_session_events::EventWriter::noop(),
    )
    .await
    .expect("spawn test child");
    let child_pid = transport.id().expect("spawned child pid");

    // Wrap in McpClient. The transport is held in `reconnect` until the
    // handshake completes; a Pending-state child still owns a live PID
    // via SafeTokioChildProcess's `child` field.
    let client = Arc::new(McpClient::new_stdio(
        "reap-test".to_string(),
        transport,
        None,
        None,
    ));

    // Simulate the production leak: an external holder (e.g.
    // `McpState::shared_clients`, or a stale liveness task reference)
    // keeps a strong Arc clone alive past the eviction.
    let external_holder = Arc::clone(&client);

    let mut owned = crate::owned_clients::OwnedClients::new();
    owned.insert("reap-test".to_string(), client);

    // Sanity: child is alive before eviction.
    assert!(
        unix_process_exists(child_pid),
        "test setup invariant: child {child_pid} must be alive before clear()",
    );

    // The contract: eviction must reap the child now, not "eventually".
    owned.clear();

    // The fix spawns an async shutdown task; let the runtime poll it.
    // 2 seconds is generous — `kill_process_group` is synchronous and
    // SIGKILL lands in milliseconds.
    let deadline = Instant::now() + Duration::from_secs(2);
    while unix_process_exists(child_pid) {
        assert!(
            Instant::now() < deadline,
            "child {child_pid} survived OwnedClients::clear() — Arc-release path \
             did not reap; OwnedClients must actively shutdown transports on eviction",
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // The external holder is still alive — that's intentional and the
    // whole point: the child must die *without* requiring Arc release.
    // strong_count >= 1 means the external reference is still valid;
    // we deliberately don't assert ">= 2" because the spawned
    // shutdown task in `cancel_and_shutdown` has already completed
    // and dropped its own Arc clone by now.
    assert!(
        Arc::strong_count(&external_holder) >= 1,
        "external holder should still hold an Arc reference, proving \
         the child was reaped actively rather than via Arc-release chain",
    );
    drop(external_holder);
}

#[test]
fn is_auth_rejection_message_matches_auth_signals() {
    assert!(is_auth_rejection_message(
        "MCP server 'echo_agent_com_notion' handshake failed: Auth required, when send initialize request"
    ));
    assert!(is_auth_rejection_message("401 Unauthorized"));
    assert!(is_auth_rejection_message("unauthorized"));
    assert!(is_auth_rejection_message("Authentication required"));
    assert!(is_auth_rejection_message("authentication failed"));
    assert!(is_auth_rejection_message("status: 401"));
    assert!(is_auth_rejection_message("HTTP status 401"));
    assert!(is_auth_rejection_message("server returned status code 401"));
    assert!(is_auth_rejection_message("HTTP 401"));
    assert!(is_auth_rejection_message("error 401"));
    assert!(is_auth_rejection_message(
        "worker quit with fatal: Transport channel closed, when Auth(AuthorizationRequired)"
    ));
    let auth_req = McpError::AuthRequired {
        server: "clickhouse".into(),
    };
    assert!(auth_req.is_auth_rejection());
    assert_eq!(auth_req.server_name(), Some("clickhouse"));
}

#[test]
fn auth_required_records_as_auth_not_init_failed_and_maps_category() {
    let mut state = McpState::new(vec![]);
    state.record_init_failure("oauth-srv", true, None);
    assert!(state.auth_required.contains("oauth-srv"));
    assert!(!state.init_failed.contains_key("oauth-srv"));

    let err = McpError::AuthRequired {
        server: "oauth-srv".into(),
    };
    assert!(matches!(
        err.error_category(),
        echo_agent_session_events::McpErrorCategory::AuthRequired
    ));
}

#[test]
fn is_auth_rejection_message_rejects_non_auth() {
    assert!(!is_auth_rejection_message("Transport closed"));
    assert!(!is_auth_rejection_message(
        "MCP server 'x' timed out after 30s"
    ));
    assert!(!is_auth_rejection_message(
        "Failed to spawn MCP server 'x': No such file or directory"
    ));
    assert!(!is_auth_rejection_message("403 Forbidden"));
    assert!(!is_auth_rejection_message("forbidden"));
    assert!(!is_auth_rejection_message("request took 401ms"));
    assert!(!is_auth_rejection_message("connect 10.0.4.01:443"));
    assert!(!is_auth_rejection_message("read 401 bytes"));
    assert!(!is_auth_rejection_message("http 4012"));
    assert!(!is_auth_rejection_message("error 4012"));
    assert!(!is_auth_rejection_message("status: 4012"));
    assert!(!is_auth_rejection_message("http 401ms"));
    assert!(!is_auth_rejection_message("error 401ms"));
    assert!(is_auth_rejection_message("http 401."));
    assert!(is_auth_rejection_message("error 401: token expired"));
}

#[test]
fn mcp_error_is_auth_rejection_delegates() {
    assert!(McpError::ClientError("Auth required".to_string()).is_auth_rejection());
    assert!(!McpError::ClientError("Transport closed".to_string()).is_auth_rejection());
    assert!(
        !McpError::Timeout {
            server: "x".to_string(),
            timeout_secs: 30,
        }
        .is_auth_rejection()
    );
    assert!(
        !McpError::SpawnFailed {
            server: "x".to_string(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "401 Unauthorized"),
        }
        .is_auth_rejection()
    );
    assert!(
        McpError::HandshakeFailed {
            server: "x".to_string(),
            source: Box::new(ClientInitializeError::ConnectionClosed(
                "Auth required, when send initialize request".to_string()
            )),
        }
        .is_auth_rejection()
    );
    assert!(
        !McpError::HandshakeFailed {
            server: "x".to_string(),
            source: Box::new(ClientInitializeError::ConnectionClosed(
                "transport closed".to_string()
            )),
        }
        .is_auth_rejection()
    );
}

#[test]
fn format_mcp_image_default_emits_only_data_uri() {
    let out = format_mcp_image("image/png", "AAAA", false);
    assert_eq!(out, "data:image/png;base64,AAAA");
    assert!(!out.contains("<mcp_image_base64"));
}

#[test]
fn format_mcp_image_expose_emits_data_uri_and_raw_block() {
    let out = format_mcp_image("image/png", "AAAA", true);
    assert!(out.contains("data:image/png;base64,AAAA"));
    assert!(out.contains("<mcp_image_base64 mime=\"image/png\">\nAAAA\n</mcp_image_base64>"));
}

#[test]
fn format_mcp_image_expose_raw_block_has_no_data_prefix() {
    let out = format_mcp_image("image/jpeg", "ZZZZ", true);
    assert_eq!(out.matches("data:image/").count(), 1);
}

#[test]
fn load_expose_image_base64_defaults_to_false() {
    assert!(!McpClient::load_expose_image_base64(None, None));
}

#[test]
fn load_expose_image_base64_uses_overrides_when_meta_unset() {
    let overrides = McpClientTimeoutOverrides {
        expose_image_base64: Some(true),
        ..Default::default()
    };
    assert!(McpClient::load_expose_image_base64(Some(&overrides), None));
}

#[test]
fn load_expose_image_base64_meta_wins_over_overrides() {
    let overrides = McpClientTimeoutOverrides {
        expose_image_base64: Some(true),
        ..Default::default()
    };
    let meta = McpServerMetaConfig {
        expose_image_base64: Some(false),
        ..Default::default()
    };
    assert!(!McpClient::load_expose_image_base64(
        Some(&overrides),
        Some(&meta)
    ));
}

#[test]
fn load_expose_image_base64_meta_falls_through_when_none() {
    let overrides = McpClientTimeoutOverrides {
        expose_image_base64: Some(true),
        ..Default::default()
    };
    let meta = McpServerMetaConfig::default();
    assert!(McpClient::load_expose_image_base64(
        Some(&overrides),
        Some(&meta)
    ));
}

#[test]
fn new_http_propagates_expose_image_base64_override_to_getter() {
    let config = HttpConfig {
        url: "http://localhost/api/mcp".to_string(),
        headers: vec![],
    };
    let overrides = McpClientTimeoutOverrides {
        expose_image_base64: Some(true),
        ..Default::default()
    };
    let client = McpClient::new_http(
        "grafana".to_string(),
        config.clone(),
        Some(&overrides),
        None,
    );
    assert!(client.expose_image_base64());

    let client_default = McpClient::new_http("grafana".to_string(), config, None, None);
    assert!(!client_default.expose_image_base64());
}

#[tokio::test]
async fn ensure_initialized_on_empty_client_returns_no_transport_error() {
    let client = McpClient::stub("test-server");

    let err = client.ensure_initialized().await.unwrap_err();
    let msg = err.to_string();

    assert!(
        msg.contains("no transport configured"),
        "expected clear 'no transport configured' error, got: {msg}"
    );
    assert!(
        !msg.contains("already initializing"),
        "regression: legacy fast-fail sentinel surfaced: {msg}"
    );
}

#[tokio::test]
async fn ensure_initialized_concurrent_callers_never_see_legacy_fast_fail() {
    let config = HttpConfig {
        url: "http://192.0.2.1:1/unreachable".to_string(),
        headers: vec![],
    };
    let overrides = McpClientTimeoutOverrides {
        startup_timeout_sec: Some(1),
        ..Default::default()
    };
    let client = Arc::new(McpClient::new_http(
        "test-server".to_string(),
        config,
        Some(&overrides),
        None,
    ));

    let mut handles = Vec::new();
    for _ in 0..5 {
        let c = Arc::clone(&client);
        handles.push(tokio::spawn(async move { c.ensure_initialized().await }));
    }

    for (idx, handle) in handles.into_iter().enumerate() {
        let result = handle.await.expect("task did not panic");
        let err = result.expect_err("unreachable host must fail");
        let msg = err.to_string();
        assert!(
            !msg.contains("MCP client already initializing"),
            "caller {idx}: legacy fast-fail sentinel surfaced: {msg}"
        );
        assert!(
            matches!(
                err,
                McpError::Timeout { .. } | McpError::HandshakeFailed { .. }
            ),
            "caller {idx}: expected handshake failure, got: {err}"
        );
    }
}

#[tokio::test]
async fn ensure_initialized_parked_caller_retries_after_notify() {
    let config = HttpConfig {
        url: "http://192.0.2.1:1/unreachable".to_string(),
        headers: vec![],
    };
    let overrides = McpClientTimeoutOverrides {
        startup_timeout_sec: Some(1),
        ..Default::default()
    };
    let client = Arc::new(McpClient::new_http(
        "test-server".to_string(),
        config.clone(),
        Some(&overrides),
        None,
    ));

    *client.state.lock().await = ClientState::Initializing;

    let parker_client = Arc::clone(&client);
    let parker = tokio::spawn(async move { parker_client.ensure_initialized().await });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    *client.state.lock().await = ClientState::Pending(PendingTransport::Http(config.clone()));
    client.init_done.notify_waiters();

    let err = parker
        .await
        .expect("parker did not panic")
        .expect_err("unreachable host must fail");
    let msg = err.to_string();
    assert!(
        !msg.contains("MCP client already initializing"),
        "regression: legacy fast-fail sentinel: {msg}"
    );
    assert!(
        !msg.contains("init still in progress"),
        "parker should not hit wait-timeout when notified: {msg}"
    );
    assert!(
        matches!(
            err,
            McpError::Timeout { .. } | McpError::HandshakeFailed { .. }
        ),
        "expected handshake failure, got: {err}"
    );
}

#[tokio::test]
async fn ensure_initialized_inflight_wait_times_out_when_holder_silent() {
    let config = HttpConfig {
        url: "http://192.0.2.1:1/unreachable".to_string(),
        headers: vec![],
    };
    let overrides = McpClientTimeoutOverrides {
        startup_timeout_sec: Some(0),
        ..Default::default()
    };
    let client = McpClient::new_http("test-server".to_string(), config, Some(&overrides), None);

    *client.state.lock().await = ClientState::Initializing;

    let err = client.ensure_initialized().await.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("init still in progress"),
        "expected wait-timeout error, got: {msg}"
    );
    assert!(
        !msg.contains("already initializing"),
        "regression: legacy fast-fail sentinel: {msg}"
    );
}

#[tokio::test]
async fn ensure_initialized_drop_guard_restores_state_after_holder_aborted() {
    let config = HttpConfig {
        url: "http://192.0.2.1:1/unreachable".to_string(),
        headers: vec![],
    };
    let overrides = McpClientTimeoutOverrides {
        startup_timeout_sec: Some(10),
        ..Default::default()
    };
    let client = Arc::new(McpClient::new_http(
        "test-server".to_string(),
        config,
        Some(&overrides),
        None,
    ));

    let holder_client = Arc::clone(&client);
    let holder = tokio::spawn(async move { holder_client.ensure_initialized().await });

    let started = std::time::Instant::now();
    loop {
        if matches!(&*client.state.lock().await, ClientState::Initializing) {
            break;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "holder never reached Initializing"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    holder.abort();
    let _ = holder.await;

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    match &*client.state.lock().await {
        ClientState::Pending(_) => {}
        other => panic!(
            "expected Pending after holder abort + drop guard, found {}",
            state_label(other)
        ),
    }
}

/// Cross-platform regression for eviction during the `Initializing` window.
/// The previous orphan-process test only covered Pending stdio and only ran on
/// Unix; this exercises the state-machine race on Windows as well.
#[tokio::test(flavor = "multi_thread")]
async fn shutdown_during_handshake_cancels_and_never_republishes_ready() {
    use crate::acp_transport::AcpReverseInvoker;
    use std::time::Duration;

    struct HangingInvoker {
        started: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl AcpReverseInvoker for HangingInvoker {
        async fn invoke(
            &self,
            _server_id: &str,
            _message: serde_json::Value,
            _timeout: Duration,
        ) -> Result<serde_json::Value, String> {
            self.started.notify_waiters();
            std::future::pending().await
        }
    }

    let started = Arc::new(tokio::sync::Notify::new());
    let started_wait = started.notified();
    let client = Arc::new(McpClient::new_acp(
        "shutdown-race".to_string(),
        "sdk-0".to_string(),
        Arc::new(HangingInvoker {
            started: Arc::clone(&started),
        }),
        None,
        None,
    ));
    let holder_client = Arc::clone(&client);
    let holder = tokio::spawn(async move { holder_client.ensure_initialized().await });

    tokio::time::timeout(Duration::from_secs(2), started_wait)
        .await
        .expect("handshake should reach the reverse invoker");
    assert_eq!(client.state_kind().await, ClientStateKind::Initializing);

    tokio::time::timeout(Duration::from_secs(2), client.shutdown_transport())
        .await
        .expect("shutdown must not wait for the configured startup timeout");
    let error = tokio::time::timeout(Duration::from_secs(2), holder)
        .await
        .expect("initialization holder must finish after shutdown")
        .expect("holder task must not panic")
        .expect_err("a shut-down client cannot initialize successfully");
    assert!(error.to_string().contains("shut down"), "got {error}");
    assert_eq!(client.state_kind().await, ClientStateKind::Empty);

    let second = client.ensure_initialized().await.unwrap_err();
    assert!(second.to_string().contains("shut down"), "got {second}");
    assert_eq!(client.state_kind().await, ClientStateKind::Empty);
}

#[test]
fn test_mcp_state_is_initialized_requires_empty_initializing_servers() {
    let mut state = McpState::new(vec![make_stdio_server("a", "/bin/a")]);

    assert!(!state.is_initialized());
    assert!(!state.is_initializing());
    assert!(!state.has_finished_init());
    assert!(matches!(state.init_progress(), InitProgress::NotStarted));

    assert!(state.try_start_init());
    state.mark_servers_initializing(["a".to_string()]);
    assert!(!state.is_initialized());
    assert!(state.is_initializing());
    assert!(!state.has_finished_init());
    assert!(matches!(
        state.init_progress(),
        InitProgress::Starting { .. }
    ));

    state.finish_init();
    assert!(
        !state.is_initialized(),
        "is_initialized() must wait for per-server handshakes"
    );
    assert!(
        state.is_initializing(),
        "is_initializing() must report in-flight per-server work"
    );
    assert!(state.has_finished_init());
    assert!(state.is_server_handshaking("a"));
    assert_eq!(state.handshaking_servers_count(), 1);

    state.mark_server_ready("a");
    assert!(state.is_initialized());
    assert!(!state.is_initializing());
    assert!(state.has_finished_init());
    assert!(!state.is_server_handshaking("a"));
    assert_eq!(state.handshaking_servers_count(), 0);
}

#[test]
fn test_init_progress_state_machine_invariants() {
    let mut state = McpState::new(vec![make_stdio_server("a", "/bin/a")]);

    assert!(state.try_start_init());
    assert!(!state.try_start_init(), "double try_start_init is rejected");

    state.mark_servers_initializing(["a".to_string(), "b".to_string()]);
    assert_eq!(state.handshaking_servers_count(), 2);
    state.mark_all_servers_ready();
    assert_eq!(state.handshaking_servers_count(), 0);
    assert!(
        matches!(state.init_progress(), InitProgress::Starting { .. }),
        "mark_all_servers_ready preserves the lifecycle variant"
    );

    state.finish_init();
    assert!(state.is_initialized());
    assert!(matches!(
        state.init_progress(),
        InitProgress::Finished { .. }
    ));

    state.cancel_init();
    assert!(matches!(state.init_progress(), InitProgress::NotStarted));
    assert!(state.try_start_init(), "cancel_init re-enables init");
}

fn state_label(s: &ClientState) -> &'static str {
    match s {
        ClientState::Empty => "Empty",
        ClientState::Pending(_) => "Pending",
        ClientState::Initializing => "Initializing",
        ClientState::Ready { .. } => "Ready",
    }
}

#[tokio::test]
async fn is_healthy_empty_returns_false() {
    let client = McpClient::stub("empty");
    assert!(matches!(*client.state.lock().await, ClientState::Empty));
    assert!(!client.is_healthy().await);
    assert_eq!(client.state_kind().await, ClientStateKind::Empty);
}

#[tokio::test]
async fn is_healthy_pending_returns_false() {
    let config = HttpConfig {
        url: "http://192.0.2.1:1/unreachable".to_string(),
        headers: vec![],
    };
    let client = McpClient::new_http("pending".to_string(), config, None, None);
    assert!(matches!(
        *client.state.lock().await,
        ClientState::Pending(_)
    ));
    assert!(!client.is_healthy().await);
    assert_eq!(client.state_kind().await, ClientStateKind::Pending);
}

#[tokio::test]
async fn is_healthy_initializing_returns_false() {
    let client = McpClient::stub("initializing");
    *client.state.lock().await = ClientState::Initializing;
    assert!(!client.is_healthy().await);
    assert_eq!(client.state_kind().await, ClientStateKind::Initializing);
}

#[tokio::test]
async fn is_healthy_pending_does_not_block_on_handshake() {
    let config = HttpConfig {
        url: "http://192.0.2.1:1/unreachable".to_string(),
        headers: vec![],
    };
    let overrides = McpClientTimeoutOverrides {
        startup_timeout_sec: Some(10),
        ..Default::default()
    };
    let client = McpClient::new_http(
        "pending-unreachable".to_string(),
        config,
        Some(&overrides),
        None,
    );
    let start = std::time::Instant::now();
    let healthy = client.is_healthy().await;
    let elapsed = start.elapsed();
    assert!(!healthy);
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "is_healthy must be a cheap state inspection, took {elapsed:?}"
    );
}

#[test]
fn make_client_info_pins_protocol_version() {
    assert_eq!(
        McpClient::make_client_info("test-srv", /* advertise_elicitation */ true).protocol_version,
        rmcp::model::ProtocolVersion::V_2025_11_25
    );
}

#[test]
fn make_client_info_advertises_form_and_url_elicitation() {
    let info = McpClient::make_client_info("test-srv", /* advertise_elicitation */ true);
    let elicitation = info
        .capabilities
        .elicitation
        .as_ref()
        .expect("elicitation capability advertised");
    assert!(
        elicitation.form.is_some(),
        "form elicitation must be advertised"
    );
    assert!(
        elicitation.url.is_some(),
        "url elicitation must be advertised"
    );
    assert_eq!(
        elicitation.form.as_ref().and_then(|f| f.schema_validation),
        Some(true),
        "client validates form content before Accept"
    );
}

#[test]
fn acp_zero_ipc_client_info_does_not_advertise_elicitation() {
    use crate::acp_transport::AcpReverseInvoker;
    use std::time::Duration;

    struct NoopInvoker;
    #[async_trait::async_trait]
    impl AcpReverseInvoker for NoopInvoker {
        async fn invoke(
            &self,
            _server_id: &str,
            _message: serde_json::Value,
            _timeout: Duration,
        ) -> Result<serde_json::Value, String> {
            Ok(serde_json::Value::Null)
        }
    }

    let acp = McpClient::new_acp(
        "sdk".to_string(),
        "srv_0".to_string(),
        Arc::new(NoopInvoker),
        None,
        None,
    );
    let acp_info = acp.make_client_handler().get_info();
    assert!(
        acp_info.capabilities.elicitation.is_none(),
        "ACP zero-IPC cannot deliver elicitation/create"
    );

    let no_bridge = McpClient::stub("stdio");
    assert!(
        no_bridge
            .make_client_handler()
            .get_info()
            .capabilities
            .elicitation
            .is_none(),
        "stdio without an elicitation inbox must not advertise"
    );

    let hitl = McpClient::stub("stdio");
    hitl.set_elicitation_tx(Some(crate::elicitation::ElicitationInbox::new()));
    assert!(
        hitl.make_client_handler()
            .get_info()
            .capabilities
            .elicitation
            .is_some(),
        "stdio/HITL path with an inbox must still advertise elicitation"
    );
}

#[tokio::test]
async fn client_handler_routes_tools_changed() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<McpClientEvent>();
    let handler = EchoAgentClientHandler {
        info: McpClient::make_client_info("test", /* advertise_elicitation */ true),
        server_name: "test".to_string(),
        notify_tx: Arc::new(parking_lot::Mutex::new(Some(tx))),
        elicitation_tx: Arc::new(parking_lot::Mutex::new(None)),
    };
    handler.emit(McpClientEvent::ToolsChanged {
        server: handler.server_name.clone(),
    });
    let ev = rx.recv().await.expect("event arrived");
    match ev {
        McpClientEvent::ToolsChanged { server } => assert_eq!(server, "test"),
        other => panic!("expected ToolsChanged, got {other:?}"),
    }
}

#[tokio::test]
async fn client_handler_no_dispatcher_is_silent() {
    let handler = EchoAgentClientHandler {
        info: McpClient::make_client_info("test", /* advertise_elicitation */ true),
        server_name: "test".to_string(),
        notify_tx: Arc::new(parking_lot::Mutex::new(None)),
        elicitation_tx: Arc::new(parking_lot::Mutex::new(None)),
    };
    handler.emit(McpClientEvent::ToolsChanged {
        server: "test".to_string(),
    });
}

#[tokio::test]
async fn client_handler_get_info_round_trips() {
    let info = McpClient::make_client_info("test-srv", /* advertise_elicitation */ true);
    let handler = EchoAgentClientHandler {
        info: info.clone(),
        server_name: "test-srv".to_string(),
        notify_tx: Arc::new(parking_lot::Mutex::new(None)),
        elicitation_tx: Arc::new(parking_lot::Mutex::new(None)),
    };
    let got = handler.get_info();
    assert_eq!(got.client_info.name, info.client_info.name);
    assert_eq!(got.client_info.version, info.client_info.version);
}

#[tokio::test]
async fn client_handler_observes_post_handshake_set_event_tx() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<McpClientEvent>();
    let client = Arc::new(McpClient::stub("test"));

    let handler = client.make_client_handler();

    assert!(handler.notify_tx.lock().is_none());

    client.set_event_tx(Some(tx));

    handler.emit(McpClientEvent::ToolsChanged {
        server: "test".to_string(),
    });
    let ev = rx.recv().await.expect("event arrived");
    match ev {
        McpClientEvent::ToolsChanged { server } => assert_eq!(server, "test"),
        other => panic!("expected ToolsChanged, got {other:?}"),
    }
}

#[tokio::test]
async fn event_tx_clone_observes_set_event_tx() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<McpClientEvent>();
    let client = McpClient::stub("test");
    assert!(client.event_tx_clone().is_none());
    client.set_event_tx(Some(tx));
    assert!(client.event_tx_clone().is_some());
    client.set_event_tx(None);
    assert!(client.event_tx_clone().is_none());
}

#[test]
fn config_added_kind_carries_correct_server_name() {
    let ev = McpClientEvent::ConfigAdded {
        server: "srv".to_string(),
    };
    assert_eq!(ev.server_name(), Some("srv"));
}

#[derive(Clone)]
struct FakeStreamableHttpHandles {
    post_headers: Arc<parking_lot::Mutex<Option<axum::http::HeaderMap>>>,
}

async fn spawn_fake_streamable_http(
    post_status: axum::http::StatusCode,
) -> (String, FakeStreamableHttpHandles) {
    use axum::response::IntoResponse;
    let handles = FakeStreamableHttpHandles {
        post_headers: Arc::new(parking_lot::Mutex::new(None)),
    };
    let post_handles = handles.clone();
    let app = axum::Router::new().route(
        "/mcp",
        axum::routing::get(fake_handle_get).post(
            move |headers: axum::http::HeaderMap| async move {
                *post_handles.post_headers.lock() = Some(headers);
                post_status.into_response()
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake server");
    let addr = listener.local_addr().expect("fake server addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/mcp"), handles)
}

fn probe_ctx<'a>(
    event_writer: &'a echo_agent_session_events::EventWriter,
    mode: OauthInteractivity,
) -> McpSpawnCtx<'a> {
    McpSpawnCtx {
        session_id: None,
        event_writer,
        mode,
        scope: None,
    }
}

const TEST_DISCOVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(300);

async fn resolve_tokenless_with_headers(
    url: &str,
    headers: &[(String, String)],
    mode: OauthInteractivity,
) -> HttpOauthPrep {
    let event_writer = echo_agent_session_events::EventWriter::noop();
    let ctx = probe_ctx(&event_writer, mode);
    resolve_http_oauth_prep("fake", url, headers, &ctx, TEST_DISCOVERY_TIMEOUT).await
}

async fn resolve_tokenless(url: &str, mode: OauthInteractivity) -> HttpOauthPrep {
    resolve_tokenless_with_headers(url, &[], mode).await
}

#[tokio::test(flavor = "multi_thread")]
async fn inconclusive_oauth_probe_connects_tokenless_streamable_http_headless() {
    let (url, _handles) = spawn_fake_streamable_http(axum::http::StatusCode::OK).await;
    let prep = resolve_tokenless(&url, OauthInteractivity::NonInteractive).await;
    assert!(
        matches!(prep, HttpOauthPrep::NoOauthSupport),
        "tokenless streamable-http server must connect plain in non-interactive mode"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn anonymous_access_probe_sends_default_user_agent() {
    let (url, handles) = spawn_fake_streamable_http(axum::http::StatusCode::OK).await;
    let prep = resolve_tokenless(&url, OauthInteractivity::NonInteractive).await;
    assert!(matches!(prep, HttpOauthPrep::NoOauthSupport));

    let captured = handles
        .post_headers
        .lock()
        .take()
        .expect("probe POST must reach the fake server");
    assert_eq!(
        header_values(&captured, axum::http::header::USER_AGENT),
        vec![format!("echo-agent-cli/{}", echo_agent_version::VERSION)]
    );
    assert_eq!(
        header_values(&captured, axum::http::header::CONTENT_TYPE),
        vec!["application/json".to_string()]
    );
    assert_eq!(
        header_values(&captured, axum::http::header::ACCEPT),
        vec!["application/json, text/event-stream".to_string()]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn anonymous_access_probe_preserves_configured_user_agent() {
    let (url, handles) = spawn_fake_streamable_http(axum::http::StatusCode::OK).await;
    let headers = [
        ("User-Agent".to_string(), "custom-ua".to_string()),
        ("Content-Type".to_string(), "text/plain".to_string()),
        ("Accept".to_string(), "text/html".to_string()),
    ];
    let prep =
        resolve_tokenless_with_headers(&url, &headers, OauthInteractivity::NonInteractive).await;
    assert!(matches!(prep, HttpOauthPrep::NoOauthSupport));

    let captured = handles
        .post_headers
        .lock()
        .take()
        .expect("probe POST must reach the fake server");
    assert_eq!(
        header_values(&captured, axum::http::header::USER_AGENT),
        vec!["custom-ua".to_string()]
    );
    assert_eq!(
        header_values(&captured, axum::http::header::CONTENT_TYPE),
        vec!["application/json".to_string()]
    );
    assert_eq!(
        header_values(&captured, axum::http::header::ACCEPT),
        vec!["application/json, text/event-stream".to_string()]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn anonymous_access_probe_accepts_bad_request_reply() {
    let (url, _handles) = spawn_fake_streamable_http(axum::http::StatusCode::BAD_REQUEST).await;
    let prep = resolve_tokenless(&url, OauthInteractivity::NonInteractive).await;
    assert!(matches!(prep, HttpOauthPrep::NoOauthSupport));
}

#[tokio::test(flavor = "multi_thread")]
async fn inconclusive_oauth_probe_stays_fail_closed_on_auth_challenge() {
    let (url, _handles) = spawn_fake_streamable_http(axum::http::StatusCode::UNAUTHORIZED).await;
    let prep = resolve_tokenless(&url, OauthInteractivity::NonInteractive).await;
    assert!(
        matches!(prep, HttpOauthPrep::NeedsInteractiveLogin),
        "auth-challenging server must keep failing closed in non-interactive mode"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn inconclusive_oauth_probe_unreachable_fails_closed() {
    use axum::response::IntoResponse;
    let app = axum::Router::new().route(
        "/mcp",
        axum::routing::get(fake_handle_get).post(|| async {
            futures::future::pending::<()>().await;
            axum::http::StatusCode::OK.into_response()
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let prep = resolve_tokenless(
        &format!("http://{addr}/mcp"),
        OauthInteractivity::NonInteractive,
    )
    .await;
    assert!(matches!(prep, HttpOauthPrep::NeedsInteractiveLogin));
}

#[tokio::test(flavor = "multi_thread")]
async fn inconclusive_oauth_probe_connects_plain_interactively() {
    let (url, _handles) = spawn_fake_streamable_http(axum::http::StatusCode::UNAUTHORIZED).await;
    let prep = resolve_tokenless(&url, OauthInteractivity::Interactive).await;
    assert!(matches!(prep, HttpOauthPrep::NoOauthSupport));
}

#[tokio::test(flavor = "multi_thread")]
async fn inconclusive_oauth_probe_emits_timeout_and_verdict_events() {
    let (url, _handles) = spawn_fake_streamable_http(axum::http::StatusCode::OK).await;
    let tmp = tempfile::tempdir().unwrap();
    let event_writer = echo_agent_session_events::EventWriter::open(tmp.path());
    let ctx = probe_ctx(&event_writer, OauthInteractivity::NonInteractive);
    let prep = resolve_http_oauth_prep("fake", &url, &[], &ctx, TEST_DISCOVERY_TIMEOUT).await;
    assert!(matches!(prep, HttpOauthPrep::NoOauthSupport));

    let jsonl = std::fs::read_to_string(tmp.path().join("events.jsonl")).unwrap();
    let events = event_types(&jsonl);
    let types: Vec<String> = events
        .iter()
        .filter_map(|e| e.get("type").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
        .collect();
    assert!(
        types.iter().any(|t| t == "mcp_oauth_discovery_timeout"),
        "missing timeout event; got {types:?}"
    );
    assert!(
        types.iter().any(|t| t == "mcp_oauth_probe_resolved"),
        "missing verdict event; got {types:?}"
    );
}

#[test]
fn apply_stdio_env_session_id_cannot_be_shadowed() {
    let mut cmd = Command::new("true");
    let env = vec![acp::EnvVariable::new("ECHO_AGENT_SESSION_ID", "spoofed")];
    apply_stdio_env(&mut cmd, &env, Some("sess-real"));

    let value = cmd
        .as_std()
        .get_envs()
        .find(|(k, _)| *k == "ECHO_AGENT_SESSION_ID")
        .and_then(|(_, v)| v)
        .map(|v| v.to_string_lossy().into_owned());
    assert_eq!(value.as_deref(), Some("sess-real"));
}

#[test]
fn mcp_icon_from_rmcp_drops_empty_and_disallowed_src() {
    assert!(McpIcon::from_rmcp(rmcp::model::Icon::new("   ")).is_none());
    assert!(
        McpIcon::from_rmcp(rmcp::model::Icon::new("http://insecure.example/icon.png")).is_none()
    );
    assert!(McpIcon::from_rmcp(rmcp::model::Icon::new("javascript:alert(1)")).is_none());

    let icon = rmcp::model::Icon::new("https://example.com/icon.png")
        .with_mime_type("image/png")
        .with_sizes(vec!["48x48".to_string()])
        .with_theme(rmcp::model::IconTheme::Dark);
    let converted = McpIcon::from_rmcp(icon).unwrap();
    assert_eq!(converted.src, "https://example.com/icon.png");
    assert_eq!(converted.mime_type.as_deref(), Some("image/png"));
    assert_eq!(converted.sizes.as_deref(), Some(&["48x48".to_string()][..]));
    assert_eq!(converted.theme, Some(McpIconTheme::Dark));

    let padded = rmcp::model::Icon::new("  https://example.com/padded.png  ");
    assert_eq!(
        McpIcon::from_rmcp(padded).unwrap().src,
        "https://example.com/padded.png"
    );

    let data = rmcp::model::Icon::new("data:image/png;base64,aaa");
    assert!(McpIcon::from_rmcp(data).is_some());
}

#[test]
fn mcp_icon_from_rmcp_list_caps_count_and_src_bytes() {
    let many: Vec<_> = (0..20)
        .map(|i| rmcp::model::Icon::new(format!("https://example.com/{i}.png")))
        .collect();
    assert_eq!(
        McpIcon::from_rmcp_list(Some(many)).len(),
        MAX_MCP_ICONS_PER_ENTITY
    );

    let huge = format!("https://example.com/{}", "x".repeat(MAX_MCP_ICON_SRC_BYTES));
    assert!(McpIcon::from_rmcp(rmcp::model::Icon::new(huge)).is_none());
}

#[test]
fn mcp_icon_from_rmcp_caps_mime_type_and_sizes() {
    let long_mime = "a".repeat(MAX_MCP_ICON_MIME_TYPE_BYTES + 1);
    let converted = McpIcon::from_rmcp(
        rmcp::model::Icon::new("https://example.com/icon.png").with_mime_type(long_mime),
    )
    .unwrap();
    assert_eq!(converted.mime_type, None);

    let many_sizes: Vec<_> = (0..20).map(|i| format!("{i}x{i}")).collect();
    let converted = McpIcon::from_rmcp(
        rmcp::model::Icon::new("https://example.com/icon.png").with_sizes(many_sizes),
    )
    .unwrap();
    assert_eq!(
        converted.sizes.as_ref().map(|s| s.len()),
        Some(MAX_MCP_ICON_SIZES)
    );

    let long_token = "x".repeat(MAX_MCP_ICON_SIZE_TOKEN_BYTES + 1);
    let converted = McpIcon::from_rmcp(
        rmcp::model::Icon::new("https://example.com/icon.png")
            .with_sizes(vec![long_token, "48x48".to_string()]),
    )
    .unwrap();
    assert_eq!(converted.sizes.as_deref(), Some(&["48x48".to_string()][..]));
}

#[test]
fn record_tool_icons_insert_empty_removes() {
    let mut state = McpState::new(vec![]);
    let name = "server__tool".to_string();
    let icons = vec![McpIcon {
        src: "https://example.com/a.png".to_string(),
        mime_type: None,
        sizes: None,
        theme: None,
    }];
    state.record_tool_icons(name.clone(), icons);
    assert_eq!(state.mcp_tool_icons.get(&name).map(|v| v.len()), Some(1));
    state.record_tool_icons(name.clone(), Vec::new());
    assert!(!state.mcp_tool_icons.contains_key(&name));
}

#[cfg(unix)]
#[tokio::test]
async fn dropping_the_spawn_guard_kills_grandchildren() {
    use std::time::{Duration, Instant};
    use tokio::io::{AsyncBufReadExt, BufReader};

    let mut cmd = Command::new("sh");
    cmd.args(["-c", "sleep 600 & echo $!; wait"])
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true);
    echo_agent_tools::util::detach_command(&mut cmd);
    #[allow(clippy::disallowed_methods)]
    let mut child = cmd.spawn().expect("spawn wrapper");
    let mut group = ProcessGroup::new().expect("group");
    group.attach(&child).expect("attach");

    let stdout = child.stdout.take().expect("piped stdout");
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .await
        .expect("read grandchild pid");
    let grandchild: u32 = line.trim().parse().expect("parse grandchild pid");
    let guard = SpawnGuard::new(child, Some(Arc::new(group)));
    assert!(
        unix_process_exists(grandchild),
        "grandchild must be alive before the guard drops"
    );

    drop(guard);

    let deadline = Instant::now() + Duration::from_secs(5);
    while unix_process_exists(grandchild) {
        assert!(
            Instant::now() < deadline,
            "grandchild {grandchild} survived the guard drop"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn spawn_into_a_closed_scope_fails_fast() {
    let scope = ProcessScope::new();
    scope.kill_all();

    let mut cmd = Command::new("sleep");
    cmd.arg("600").kill_on_drop(true);
    echo_agent_tools::util::detach_command(&mut cmd);
    let result = SafeTokioChildProcess::spawn(
        cmd,
        Some(&scope),
        "closed-scope".to_string(),
        echo_agent_session_events::EventWriter::noop(),
    )
    .await;

    assert!(
        result.is_err(),
        "spawning into a closed scope must fail fast, not start a doomed server"
    );
}
