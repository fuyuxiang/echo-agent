//! Built-binary e2e smoke tests for Stop hook decision control. `#[ignore]`d by
//! default since they need the echoagent binary (`ECHO_AGENT_BINARY` or a local debug build):
//! ```bash
//! cargo test -p echo-agent-runtime --test test_stop_hook_e2e -- --ignored
//! ```

use echo_agent_test_support::*;

/// Everything a test needs to assert on after a headless run with a Stop hook.
struct StopHookRun {
    result: HeadlessResult,
    server: MockInferenceServer,
    state_dir: tempfile::TempDir,
}

impl StopHookRun {
    fn invocations(&self) -> u32 {
        std::fs::read_to_string(self.state_dir.path().join("count"))
            .map(|s| s.trim().parse().expect("count file holds a number"))
            .unwrap_or(0)
    }

    /// The stdin envelope the hook received on its `n`-th run (1-based).
    fn hook_input(&self, n: u32) -> serde_json::Value {
        let path = self.state_dir.path().join(format!("input_{n}.json"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("hook stdin not JSON: {e}\n{text}"))
    }

    /// Substring match over the serialized request JSON; keep needles free of
    /// quotes and newlines.
    fn some_request_contains(&self, needle: &str) -> bool {
        self.server
            .request_bodies()
            .iter()
            .any(|body| body.to_string().contains(needle))
    }
}

/// Runs the built binary headless with a global Stop hook whose script body is
/// `respond`. `$n` holds the 1-based invocation number when `respond` runs.
async fn run_with_stop_hook(respond: &str) -> StopHookRun {
    let state_dir = tempfile::TempDir::new().expect("create state dir");
    let server = MockInferenceServer::start()
        .await
        .expect("start mock server");
    let sandbox = TestSandbox::builder().mock_url(server.url()).git().build();

    let state = state_dir.path().display();
    let script_path = sandbox.home().join("stop_hook.sh");
    // Only turn-end gate fires (`reason: "end_turn"`) are counted and
    // responded to, so a session-end Stop fire (`channel_closed`/`shutdown`)
    // can never skew the counts these tests assert on.
    std::fs::write(
        &script_path,
        format!(
            "#!/bin/sh\n\
             cat > {state}/stdin.json\n\
             grep -q '\"reason\":\"end_turn\"' {state}/stdin.json || exit 0\n\
             n=$(cat {state}/count 2>/dev/null || echo 0)\n\
             n=$((n+1))\n\
             echo $n > {state}/count\n\
             mv {state}/stdin.json {state}/input_$n.json\n\
             {respond}\n"
        ),
    )
    .expect("write hook script");

    let hooks_dir = sandbox.echo_agent_home().join("hooks");
    std::fs::create_dir_all(&hooks_dir).expect("create hooks dir");
    std::fs::write(
        hooks_dir.join("stop.json"),
        serde_json::json!({
            "hooks": {
                "Stop": [{
                    "hooks": [{
                        "type": "command",
                        "command": format!("sh {}", script_path.display()),
                        "timeout": 30
                    }]
                }]
            }
        })
        .to_string(),
    )
    .expect("write hook config");

    let mut cmd = tokio::process::Command::new(echo_agent_binary());
    cmd.args(["-p", "say hello", "--yolo"])
        .current_dir(sandbox.workspace())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let result = run_headless_in_sandbox(cmd, sandbox).await;

    StopHookRun {
        result,
        server,
        state_dir,
    }
}

fn assert_success(run: &StopHookRun, label: &str) {
    assert_headless_success(&run.result, label, Some(&run.server));
}

#[tokio::test]
#[ignore]
async fn stop_block_keeps_agent_working_then_allows() {
    let run = run_with_stop_hook(
        r#"if [ $n -eq 1 ]; then echo '{"decision":"block","reason":"finish the checklist first"}'; fi"#,
    )
    .await;
    assert_success(&run, "stop block e2e");

    assert_eq!(
        run.invocations(),
        2,
        "gate must re-fire once after the block, then allow"
    );

    let first = run.hook_input(1);
    assert_eq!(first["stopHookActive"], false, "first fire: no prior block");
    assert!(
        first["lastAssistantMessage"].is_string(),
        "input carries the turn's final response, got: {first}"
    );

    let second = run.hook_input(2);
    assert_eq!(
        second["stopHookActive"], true,
        "re-fire must set stopHookActive"
    );

    assert!(
        run.some_request_contains("finish the checklist first"),
        "the block reason must be fed back to the model"
    );
}

#[tokio::test]
#[ignore]
async fn stop_exit_2_blocks_with_stderr_feedback() {
    let run = run_with_stop_hook(
        r#"if [ $n -eq 1 ]; then echo 'run the linter before finishing' >&2; exit 2; fi"#,
    )
    .await;
    assert_success(&run, "stop exit-2 e2e");

    assert_eq!(run.invocations(), 2, "exit 2 must block, then allow");
    assert!(
        run.some_request_contains("run the linter before finishing"),
        "stderr must be fed back to the model as the block reason"
    );
}

#[tokio::test]
#[ignore]
async fn stop_continue_false_overrides_block() {
    let run = run_with_stop_hook(
        r#"echo '{"decision":"block","reason":"never stop","continue":false,"stopReason":"budget exhausted"}'"#,
    )
    .await;
    assert_success(&run, "stop force-stop e2e");

    assert_eq!(
        run.invocations(),
        1,
        "force-stop must end the turn without re-firing the gate"
    );
    assert!(
        !run.some_request_contains("never stop"),
        "the overridden block reason must not be fed back to the model"
    );
}

#[tokio::test]
#[ignore]
async fn stop_block_loop_ends_at_continuation_cap() {
    let run =
        run_with_stop_hook(r#"echo '{"decision":"block","reason":"keep going forever"}'"#).await;
    assert_success(&run, "stop cap e2e");

    assert_eq!(
        run.invocations(),
        echo_agent_runtime::session::MAX_STOP_HOOK_CONTINUATIONS_PER_TURN,
        "the gate must stop being consulted at the continuation cap"
    );
}
