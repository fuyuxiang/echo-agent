//! Integration tests for echo-agent-hooks.
//!
//! Hooks use inline shell command strings routed via `sh -c` rather than
//! standalone scripts, avoiding `noexec` tmpdir issues in hermetic CI sandboxes
//! where `chmod +x` may not work.

use std::path::Path;

use echo_agent_hooks::discovery::load_hooks;
use echo_agent_hooks::dispatcher;
use echo_agent_hooks::event::*;
use echo_agent_hooks::result::HookDecision;
use echo_agent_hooks::runner::RunContext;

fn write_hook(dir: &Path, filename: &str, content: &str) {
    std::fs::write(dir.join(filename), content).unwrap();
}

fn pre_tool_use_envelope(tool_name: &str) -> HookEventEnvelope {
    HookEventEnvelope {
        hook_event_name: HookEventName::PreToolUse,
        session_id: "test-session".into(),
        cwd: "/tmp".into(),
        workspace_root: "/tmp".into(),
        timestamp: "2025-01-01T00:00:00Z".into(),
        transcript_path: None,
        client_identifier: None,
        prompt_id: None,
        permission_mode: None,
        payload: HookPayload::PreToolUse {
            tool_name: tool_name.into(),
            tool_use_id: "call-1".into(),
            tool_input: serde_json::json!({"command": "echo hello"}),
            tool_input_truncated: false,
            subagent_type: None,
        },
    }
}

fn session_start_envelope() -> HookEventEnvelope {
    HookEventEnvelope {
        hook_event_name: HookEventName::SessionStart,
        session_id: "test-session".into(),
        cwd: "/tmp".into(),
        workspace_root: "/tmp".into(),
        timestamp: "2025-01-01T00:00:00Z".into(),
        transcript_path: None,
        client_identifier: None,
        prompt_id: None,
        permission_mode: None,
        payload: HookPayload::SessionStart {
            source: "new".into(),
            model_id: None,
            agent_type: None,
        },
    }
}

#[tokio::test]
async fn hook_deny_via_exit_code_only() {
    let dir = tempfile::tempdir().unwrap();

    write_hook(
        dir.path(),
        "safety.json",
        r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"exit 2"}]}]}}"#,
    );

    let (registry, errors) = load_hooks(Some(dir.path()), None);
    assert!(errors.is_empty());

    let ctx = RunContext {
        session_id: "test",
        workspace_root: dir.path().to_str().unwrap(),
        process_scope: None,
    };

    let pre_result =
        dispatcher::dispatch_pre_tool_use(&registry, &pre_tool_use_envelope("read_file"), &ctx)
            .await;
    match pre_result.decision {
        HookDecision::Deny { reason, .. } => {
            assert!(reason.contains("exit code 2") || reason.contains("denied by hook"));
        }
        other => panic!("expected Deny, got {other:?}"),
    }
}

#[tokio::test]
async fn hook_fail_open_on_crash() {
    let dir = tempfile::tempdir().unwrap();

    write_hook(
        dir.path(),
        "safety.json",
        r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"exit 1"}]}]}}"#,
    );

    let (registry, errors) = load_hooks(Some(dir.path()), None);
    assert!(errors.is_empty());

    let ctx = RunContext {
        session_id: "test",
        workspace_root: dir.path().to_str().unwrap(),
        process_scope: None,
    };

    let pre_result =
        dispatcher::dispatch_pre_tool_use(&registry, &pre_tool_use_envelope("read_file"), &ctx)
            .await;
    assert_eq!(
        pre_result.decision,
        HookDecision::Allow,
        "fail-open: a crashing hook must not block the tool call"
    );
    assert_eq!(
        pre_result.results.len(),
        1,
        "the failure must still appear in run_results for UI scrollback"
    );
}

#[tokio::test]
async fn hook_fail_open_on_timeout() {
    let dir = tempfile::tempdir().unwrap();

    write_hook(
        dir.path(),
        "safety.json",
        r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"sleep 10","timeout":1}]}]}}"#,
    );

    let (registry, errors) = load_hooks(Some(dir.path()), None);
    assert!(errors.is_empty());

    let ctx = RunContext {
        session_id: "test",
        workspace_root: dir.path().to_str().unwrap(),
        process_scope: None,
    };

    let pre_result =
        dispatcher::dispatch_pre_tool_use(&registry, &pre_tool_use_envelope("read_file"), &ctx)
            .await;
    assert_eq!(
        pre_result.decision,
        HookDecision::Allow,
        "fail-open: a timing-out hook must not block the tool call"
    );
}

#[tokio::test]
async fn matcher_filters_tool_name() {
    let dir = tempfile::tempdir().unwrap();

    write_hook(
        dir.path(),
        "safety.json",
        r#"{"hooks":{"PreToolUse":[{"matcher":"run_terminal_cmd","hooks":[{"type":"command","command":"echo '{\"decision\":\"deny\",\"reason\":\"blocked\"}'; exit 2"}]}]}}"#,
    );

    let (registry, errors) = load_hooks(Some(dir.path()), None);
    assert!(errors.is_empty());

    let ctx = RunContext {
        session_id: "test",
        workspace_root: dir.path().to_str().unwrap(),
        process_scope: None,
    };

    let pre_result = dispatcher::dispatch_pre_tool_use(
        &registry,
        &pre_tool_use_envelope("run_terminal_cmd"),
        &ctx,
    )
    .await;
    assert!(matches!(pre_result.decision, HookDecision::Deny { .. }));

    let pre_result =
        dispatcher::dispatch_pre_tool_use(&registry, &pre_tool_use_envelope("read_file"), &ctx)
            .await;
    assert_eq!(pre_result.decision, HookDecision::Allow);
}

#[tokio::test]
async fn non_blocking_dispatch() {
    let dir = tempfile::tempdir().unwrap();

    write_hook(
        dir.path(),
        "lifecycle.json",
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"echo session started"}]}]}}"#,
    );

    let (registry, errors) = load_hooks(Some(dir.path()), None);
    assert!(errors.is_empty());

    let ctx = RunContext {
        session_id: "test",
        workspace_root: dir.path().to_str().unwrap(),
        process_scope: None,
    };

    let results = dispatcher::dispatch_non_blocking(
        &registry,
        HookEventName::SessionStart,
        &session_start_envelope(),
        &ctx,
    )
    .await;

    assert_eq!(results.len(), 1);
    assert!(matches!(
        &results[0],
        echo_agent_hooks::result::HookRunResult::Success { .. }
    ));
}

#[tokio::test]
async fn first_deny_stops_chain() {
    let dir = tempfile::tempdir().unwrap();

    write_hook(
        dir.path(),
        "01-deny.json",
        r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"echo '{\"decision\":\"deny\",\"reason\":\"first-deny\"}'; exit 2"}]}]}}"#,
    );
    write_hook(
        dir.path(),
        "02-allow.json",
        r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"echo '{\"decision\":\"allow\"}'"}]}]}}"#,
    );

    let (registry, errors) = load_hooks(Some(dir.path()), None);
    assert!(errors.is_empty());

    let ctx = RunContext {
        session_id: "test",
        workspace_root: dir.path().to_str().unwrap(),
        process_scope: None,
    };

    let pre_result = dispatcher::dispatch_pre_tool_use(
        &registry,
        &pre_tool_use_envelope("run_terminal_cmd"),
        &ctx,
    )
    .await;
    match pre_result.decision {
        HookDecision::Deny { reason, .. } => {
            assert_eq!(reason, "first-deny");
        }
        other => panic!("expected Deny, got {other:?}"),
    }
}

#[tokio::test]
async fn hook_receives_stdin_envelope() {
    let dir = tempfile::tempdir().unwrap();

    write_hook(
        dir.path(),
        "check.json",
        r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"INPUT=$(cat); echo \"$INPUT\" | grep -q '\"hookEventName\"' && echo \"$INPUT\" | grep -q '\"toolName\"' && echo \"$INPUT\" | grep -q '\"sessionId\"' && echo '{\"decision\":\"allow\"}' || echo '{\"decision\":\"deny\",\"reason\":\"missing fields\"}'"}]}]}}"#,
    );

    let (registry, errors) = load_hooks(Some(dir.path()), None);
    assert!(errors.is_empty());

    let ctx = RunContext {
        session_id: "test-sess-123",
        workspace_root: dir.path().to_str().unwrap(),
        process_scope: None,
    };

    let pre_result =
        dispatcher::dispatch_pre_tool_use(&registry, &pre_tool_use_envelope("read_file"), &ctx)
            .await;
    assert_eq!(pre_result.decision, HookDecision::Allow);
}

#[tokio::test]
async fn shell_pipe_command_works() {
    let dir = tempfile::tempdir().unwrap();

    write_hook(
        dir.path(),
        "pipe.json",
        r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"cat | echo '{\"decision\":\"allow\"}'"}]}]}}"#,
    );

    let (registry, errors) = load_hooks(Some(dir.path()), None);
    assert!(errors.is_empty());

    let ctx = RunContext {
        session_id: "test",
        workspace_root: dir.path().to_str().unwrap(),
        process_scope: None,
    };

    let pre_result =
        dispatcher::dispatch_pre_tool_use(&registry, &pre_tool_use_envelope("read_file"), &ctx)
            .await;
    assert_eq!(pre_result.decision, HookDecision::Allow);
}

fn make_envelope(event: HookEventName, payload: HookPayload) -> HookEventEnvelope {
    HookEventEnvelope {
        hook_event_name: event,
        session_id: "test-session".into(),
        cwd: "/tmp".into(),
        workspace_root: "/tmp".into(),
        timestamp: "2025-01-01T00:00:00Z".into(),
        transcript_path: None,
        client_identifier: None,
        prompt_id: None,
        permission_mode: None,
        payload,
    }
}

/// Each new event type: write hook file → load → dispatch → verify the
/// command fires and receives the correct JSON envelope on stdin.
#[tokio::test]
async fn new_event_types_fire_and_receive_correct_envelope() {
    struct Case {
        event_name: HookEventName,
        json_key: &'static str,
        payload: HookPayload,
        assertions: Vec<(&'static str, serde_json::Value)>,
    }

    let cases = vec![
        Case {
            event_name: HookEventName::PostToolUseFailure,
            json_key: "PostToolUseFailure",
            payload: HookPayload::PostToolUseFailure {
                tool_name: "run_terminal_cmd".into(),
                tool_use_id: "call-1".into(),
                tool_input: serde_json::json!({"command": "bad_cmd"}),
                tool_input_truncated: false,
                error: "command not found".into(),
                subagent_type: None,
            },
            assertions: vec![
                ("hookEventName", "post_tool_use_failure".into()),
                ("toolName", "run_terminal_cmd".into()),
                ("error", "command not found".into()),
            ],
        },
        Case {
            event_name: HookEventName::PermissionDenied,
            json_key: "PermissionDenied",
            payload: HookPayload::PermissionDenied {
                tool_name: "run_terminal_cmd".into(),
                tool_use_id: "call-2".into(),
                tool_input: serde_json::json!({"command": "rm -rf /"}),
                tool_input_truncated: false,
            },
            assertions: vec![
                ("hookEventName", "permission_denied".into()),
                ("toolName", "run_terminal_cmd".into()),
            ],
        },
        Case {
            event_name: HookEventName::PreCompact,
            json_key: "PreCompact",
            payload: HookPayload::PreCompact {
                source: "auto".into(),
            },
            assertions: vec![
                ("hookEventName", "pre_compact".into()),
                ("source", "auto".into()),
            ],
        },
        Case {
            event_name: HookEventName::PostCompact,
            json_key: "PostCompact",
            payload: HookPayload::PostCompact {
                source: "manual".into(),
            },
            assertions: vec![
                ("hookEventName", "post_compact".into()),
                ("source", "manual".into()),
            ],
        },
        Case {
            event_name: HookEventName::StopFailure,
            json_key: "StopFailure",
            payload: HookPayload::StopFailure {
                error: echo_agent_hooks::event::StopFailureKind::RateLimit,
                error_details: Some("429 Too Many Requests".into()),
                last_assistant_message: Some("Turn failed: rate limited".into()),
                subagent_type: None,
            },
            assertions: vec![
                ("hookEventName", "stop_failure".into()),
                ("error", "rate_limit".into()),
                ("errorDetails", "429 Too Many Requests".into()),
                ("lastAssistantMessage", "Turn failed: rate limited".into()),
            ],
        },
        Case {
            event_name: HookEventName::StopCancelled,
            json_key: "StopCancelled",
            payload: HookPayload::StopCancelled {
                reason: echo_agent_hooks::event::StopCancelledReason::UserInterrupt,
                cancelled_by: echo_agent_hooks::event::CancelledBy::User,
                cancel_trigger: Some("ctrl_c".into()),
                reason_details: Some("read_file: user declined".into()),
                last_assistant_message: Some("partial answer".into()),
                subagent_type: Some("explore".into()),
            },
            assertions: vec![
                ("hookEventName", "stop_cancelled".into()),
                ("reason", "user_interrupt".into()),
                ("cancelledBy", "user".into()),
                ("cancelTrigger", "ctrl_c".into()),
            ],
        },
    ];

    for case in &cases {
        let dir = tempfile::tempdir().unwrap();
        let output_file = dir.path().join("output.json");

        let cmd = format!("cat > {}", output_file.display());
        let hook_json = serde_json::json!({
            "hooks": {
                (case.json_key): [
                    { "hooks": [{ "type": "command", "command": cmd }] }
                ]
            }
        });
        write_hook(dir.path(), "hook.json", &hook_json.to_string());

        let (registry, errors) = load_hooks(Some(dir.path()), None);
        assert!(
            errors.is_empty(),
            "{}: load errors: {errors:?}",
            case.json_key
        );
        assert!(!registry.is_empty(), "{}: registry empty", case.json_key);

        let envelope = make_envelope(case.event_name, case.payload.clone());
        let ctx = RunContext {
            session_id: "test",
            workspace_root: dir.path().to_str().unwrap(),
            process_scope: None,
        };

        let results =
            dispatcher::dispatch_non_blocking(&registry, case.event_name, &envelope, &ctx).await;

        assert_eq!(
            results.len(),
            1,
            "{}: expected 1 result, got {}",
            case.json_key,
            results.len()
        );
        assert!(
            matches!(
                &results[0],
                echo_agent_hooks::result::HookRunResult::Success { .. }
            ),
            "{}: hook did not succeed: {:?}",
            case.json_key,
            results[0]
        );

        let raw = std::fs::read_to_string(&output_file)
            .unwrap_or_else(|e| panic!("{}: hook didn't fire: {e}", case.json_key));
        let captured: serde_json::Value = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("{}: bad JSON: {e}\nraw: {raw}", case.json_key));

        for (field, expected) in &case.assertions {
            assert_eq!(
                &captured[field], expected,
                "{}: field '{}' mismatch.\nExpected: {expected}\nGot: {}\nFull: {captured}",
                case.json_key, field, captured[field]
            );
        }
    }
}

/// Regression: a user JSON hook that declares `env` values for
/// runner-reserved keys (`ECHO_AGENT_HOOK_EVENT`, `ECHO_AGENT_HOOK_NAME`,
/// `ECHO_AGENT_SESSION_ID`, `ECHO_AGENT_WORKSPACE_ROOT`, `CLAUDE_PROJECT_DIR`)
/// must NOT spoof those values inside the spawned child. The
/// runner-injected vars always win at spawn time. This test
/// constructs the spoof JSON, dispatches a hook that writes `printenv`
/// for each key, and asserts the captured values are the runner's
/// authentic ones.
#[tokio::test]
async fn runner_injected_vars_override_extra_env_at_spawn() {
    let dir = tempfile::tempdir().unwrap();
    let output_file = dir.path().join("envcap.txt");

    let cmd = format!(
        r#"echo "EVENT=$ECHO_AGENT_HOOK_EVENT" > {f}; echo "NAME=$ECHO_AGENT_HOOK_NAME" >> {f}; echo "SESSION=$ECHO_AGENT_SESSION_ID" >> {f}; echo "ROOT=$ECHO_AGENT_WORKSPACE_ROOT" >> {f}; echo "PROJ=$CLAUDE_PROJECT_DIR" >> {f}; echo "USER_KEY=$USER_KEY" >> {f}; echo '{{"decision":"allow"}}'"#,
        f = output_file.display(),
    );

    let hook_json = serde_json::json!({
        "hooks": {
            "PreToolUse": [
                {
                    "hooks": [
                        {
                            "type": "command",
                            "command": cmd,
                            // Spoof every reserved key + add a non-reserved one
                            // that should be preserved.
                            "env": {
                                "ECHO_AGENT_HOOK_EVENT": "spoofed_event",
                                "ECHO_AGENT_HOOK_NAME": "spoofed_name",
                                "ECHO_AGENT_SESSION_ID": "spoofed_session",
                                "ECHO_AGENT_WORKSPACE_ROOT": "/spoofed/root",
                                "CLAUDE_PROJECT_DIR": "/spoofed/project",
                                "USER_KEY": "user_value_kept"
                            }
                        }
                    ]
                }
            ]
        }
    });
    write_hook(dir.path(), "spoof.json", &hook_json.to_string());

    let (registry, errors) = load_hooks(Some(dir.path()), None);
    assert!(errors.is_empty(), "errors: {errors:?}");

    let real_session = "auth-session-xyz";
    let real_workspace = dir.path().to_str().unwrap();
    let ctx = RunContext {
        session_id: real_session,
        workspace_root: real_workspace,
        process_scope: None,
    };

    let result =
        dispatcher::dispatch_pre_tool_use(&registry, &pre_tool_use_envelope("read_file"), &ctx)
            .await;
    assert_eq!(result.decision, HookDecision::Allow);

    let captured = std::fs::read_to_string(&output_file).unwrap();
    assert!(
        captured.contains("EVENT=pre_tool_use"),
        "ECHO_AGENT_HOOK_EVENT must reflect the real event, got:\n{captured}"
    );
    assert!(
        !captured.contains("EVENT=spoofed_event"),
        "spoofed ECHO_AGENT_HOOK_EVENT must NOT leak through, got:\n{captured}"
    );
    assert!(
        captured.contains(&format!("SESSION={real_session}")),
        "ECHO_AGENT_SESSION_ID must reflect the real session, got:\n{captured}"
    );
    assert!(
        !captured.contains("SESSION=spoofed_session"),
        "spoofed ECHO_AGENT_SESSION_ID must NOT leak through"
    );
    assert!(
        captured.contains(&format!("ROOT={real_workspace}")),
        "ECHO_AGENT_WORKSPACE_ROOT must reflect the real workspace root, got:\n{captured}"
    );
    assert!(
        !captured.contains("ROOT=/spoofed/root"),
        "spoofed ECHO_AGENT_WORKSPACE_ROOT must NOT leak through"
    );
    assert!(
        captured.contains(&format!("PROJ={real_workspace}")),
        "CLAUDE_PROJECT_DIR must reflect the real workspace root, got:\n{captured}"
    );
    assert!(
        !captured.contains("PROJ=/spoofed/project"),
        "spoofed CLAUDE_PROJECT_DIR must NOT leak through"
    );
    assert!(
        captured.contains("USER_KEY=user_value_kept"),
        "non-reserved user-declared env keys must pass through, got:\n{captured}"
    );
}

/// Regression: a user JSON hook with `command:
/// "${VAR}/script.sh"` (no other shell metachars) should resolve at
/// load time to the substituted path and then take the **direct-exec**
/// branch in the runner. This proves the load-time -> direct-exec
/// path works end-to-end through `load_hooks` -> `dispatcher::dispatch_*`.
#[tokio::test]
async fn direct_exec_command_with_env_var_resolves_at_load_time() {
    let dir = tempfile::tempdir().unwrap();

    // Build an inline shell script via the env map: the resolved
    // command path will be `<tmpdir>/check.sh`. We use the per-hook
    // `env` map (rather than the process env) so this test doesn't
    // need to mutate global state.
    let tmpdir_str = dir.path().to_string_lossy().into_owned();

    let script = dir.path().join("check.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\necho '{\"decision\":\"allow\"}'\nexit 0\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
    }

    let hook_json = serde_json::json!({
        "hooks": {
            "PreToolUse": [
                {
                    "hooks": [
                        {
                            "type": "command",
                            // No shell metachars apart from `${...}`. The
                            // load-time pass resolves `${ROOT}` to the
                            // tmpdir path, leaving "/tmp.../check.sh" with
                            // no `$`, so the runner picks the direct-exec
                            // branch.
                            "command": "${ROOT}/check.sh",
                            "env": { "ROOT": tmpdir_str }
                        }
                    ]
                }
            ]
        }
    });
    write_hook(dir.path(), "exec.json", &hook_json.to_string());

    let (registry, errors) = load_hooks(Some(dir.path()), None);
    assert!(errors.is_empty(), "errors: {errors:?}");

    let specs: Vec<_> = registry
        .hooks_for(HookEventName::PreToolUse)
        .iter()
        .collect();
    assert_eq!(specs.len(), 1);
    let cmd = specs[0]
        .command
        .as_ref()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(
        !cmd.contains('$'),
        "command must be fully resolved at load time, got: {cmd}"
    );
    assert!(cmd.ends_with("/check.sh"), "got: {cmd}");

    let ctx = RunContext {
        session_id: "test",
        workspace_root: dir.path().to_str().unwrap(),
        process_scope: None,
    };
    let result =
        dispatcher::dispatch_pre_tool_use(&registry, &pre_tool_use_envelope("read_file"), &ctx)
            .await;
    assert_eq!(
        result.decision,
        HookDecision::Allow,
        "direct-exec hook with env-var-resolved path must run, got {:?}",
        result.decision
    );
}

/// Regression: an HTTP hook whose `url` references `${VAR}`
/// resolved via the per-hook `env` map must reach the HTTP runner with
/// the post-expansion URL. We can't make a real network call from CI,
/// but we can prove the runner sees the expanded URL by pointing at a
/// blocked private IP and verifying the SSRF block message references
/// the post-expansion address. Pairs with the unit test
/// `run_http_hook_uses_post_expansion_url_for_ssrf`.
#[tokio::test]
async fn http_hook_url_env_expansion_end_to_end() {
    let dir = tempfile::tempdir().unwrap();

    let hook_json = serde_json::json!({
        "hooks": {
            "PreToolUse": [
                {
                    "hooks": [
                        {
                            "type": "http",
                            // `${INTERNAL}` is in the per-hook env map
                            // and resolves to a private RFC1918 IP. The
                            // HTTP runner expands the URL, then SSRF
                            // validation rejects 10.0.0.1.
                            "url": "https://${INTERNAL}/check",
                            "env": { "INTERNAL": "10.0.0.1" }
                        }
                    ]
                }
            ]
        }
    });
    write_hook(dir.path(), "http.json", &hook_json.to_string());

    let (registry, errors) = load_hooks(Some(dir.path()), None);
    assert!(errors.is_empty(), "errors: {errors:?}");

    // Sanity: load-time expansion already substituted `${INTERNAL}`,
    // because `INTERNAL` is in the per-hook env map (which feeds
    // load-time expansion).
    let specs: Vec<_> = registry
        .hooks_for(HookEventName::PreToolUse)
        .iter()
        .collect();
    assert_eq!(specs.len(), 1);
    assert_eq!(
        specs[0].url.as_deref(),
        Some("https://10.0.0.1/check"),
        "load-time expansion should have already substituted ${{INTERNAL}}"
    );
    // `url_raw` preserves the source string for display surfaces.
    assert_eq!(
        specs[0].url_raw.as_deref(),
        Some("https://${INTERNAL}/check")
    );

    let ctx = RunContext {
        session_id: "test",
        workspace_root: dir.path().to_str().unwrap(),
        process_scope: None,
    };
    let pre_result =
        dispatcher::dispatch_pre_tool_use(&registry, &pre_tool_use_envelope("read_file"), &ctx)
            .await;
    // Fail-open: SSRF block is a hook failure, not a deny. The tool
    // call is allowed; the failure is recorded for scrollback.
    assert_eq!(
        pre_result.decision,
        HookDecision::Allow,
        "fail-open: SSRF-blocked HTTP hook must NOT block the tool call"
    );
    assert_eq!(pre_result.results.len(), 1);
    let run = &pre_result.results[0];
    use echo_agent_hooks::result::HookRunResult;
    let info = match run {
        HookRunResult::Failed {
            http_info, error, ..
        } => {
            assert!(
                error.contains("blocked") || error.contains("SSRF"),
                "expected SSRF block message, got: {error}"
            );
            http_info.as_ref().expect("HttpInfo should be present")
        }
        other => panic!("expected Failed run result, got {other:?}"),
    };
    assert_eq!(
        info.url, "https://10.0.0.1/check",
        "HttpInfo.url must reflect the post-expansion URL"
    );
    // raw_url mirrors the source string so wire-DTO
    // consumers can prefer it over the post-expansion `url` for any
    // user-facing display.
    assert_eq!(
        info.raw_url.as_deref(),
        Some("https://${INTERNAL}/check"),
        "HttpInfo.raw_url must mirror HookSpec::url_raw"
    );
}

/// Mixed known + unknown events: known ones load and dispatch, unknown ones are skipped.
#[tokio::test]
async fn lenient_parsing_with_mixed_claude_events() {
    let dir = tempfile::tempdir().unwrap();

    let hook_json = serde_json::json!({
        "hooks": {
            "PreToolUse": [
                { "matcher": "run_terminal_cmd", "hooks": [{ "type": "command", "command": "echo '{\"decision\":\"allow\"}'" }] }
            ],
            "PostToolUseFailure": [
                { "hooks": [{ "type": "command", "command": "echo fail-hook" }] }
            ],
            "PreCompact": [
                { "hooks": [{ "type": "command", "command": "echo compact" }] }
            ],
            // Unknown external-only events; must not break the above.
            "PermissionRequest": [
                { "hooks": [{ "type": "command", "command": "echo perm-req" }] }
            ],
            "TaskCreated": [
                { "hooks": [{ "type": "command", "command": "echo task" }] }
            ],
            "FileChanged": [
                { "matcher": ".envrc", "hooks": [{ "type": "command", "command": "echo envrc" }] }
            ]
        }
    });
    write_hook(dir.path(), "mixed.json", &hook_json.to_string());

    let (registry, errors) = load_hooks(Some(dir.path()), None);
    assert!(errors.is_empty(), "errors: {errors:?}");
    assert_eq!(registry.hooks_for(HookEventName::PreToolUse).len(), 1);
    assert_eq!(
        registry.hooks_for(HookEventName::PostToolUseFailure).len(),
        1
    );
    assert_eq!(registry.hooks_for(HookEventName::PreCompact).len(), 1);

    let ctx = RunContext {
        session_id: "test",
        workspace_root: dir.path().to_str().unwrap(),
        process_scope: None,
    };
    let result = dispatcher::dispatch_pre_tool_use(
        &registry,
        &pre_tool_use_envelope("run_terminal_cmd"),
        &ctx,
    )
    .await;
    assert_eq!(result.decision, HookDecision::Allow);
}
