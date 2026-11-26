// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// The fail-closed gate refuses during bootstrap, after the TUI redirects fd 2 to `/dev/null`; its refusal must
/// still reach the real terminal (a regression that doesn't restore fd 2 leaves a managed user a blank `exit 1`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn managed_policy_gate_refusal_reaches_real_terminal() {
    let sandbox = echo_agent_test_support::TestSandbox::new();
    let home_path = sandbox.echo_agent_home();
    std::fs::write(
        home_path.join("config.toml"),
        // Dead local port so any incidental fetch fails fast offline (the gate is synchronous anyway).
        "[endpoints]\n\
         deployment_key = \"KEY-AAA\"\n\
         managed_config_url = \"http://127.0.0.1:1/deployment/config\"\n\
         cli_chat_proxy_base_url = \"http://127.0.0.1:1\"\n",
    )
    .expect("write config.toml");
    std::fs::write(
        home_path.join("managed_config.toml"),
        "[cli]\ntheme = \"dark\"\n",
    )
    .expect("write managed_config.toml");
    // requirements.toml is deliberately absent (served-then-deleted).
    let synced_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs();
    std::fs::write(
        home_path.join("managed_config_cache.json"),
        format!(
            "{{\"synced_at\":{synced_at},\"principal\":\"deploy-A\",\
             \"had_managed_config\":true,\"had_requirements\":true,\
             \"key_fingerprint\":null,\"fail_closed\":true}}"
        ),
    )
    .expect("write marker");

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness = PtyHarness::new_in_sandbox_ops(
        &binary,
        DEFAULT_ROWS,
        DEFAULT_COLS,
        &["--no-auto-update"],
        &sandbox,
        // ECHO_AGENT_MANAGED_CONFIG=0 disables the background refetch so the gate decision is deterministic and offline.
        &[
            EnvOp::set("ECHO_AGENT_MANAGED_CONFIG", "0"),
            EnvOp::set("NO_COLOR", "1"),
        ],
        None,
    )
    .expect("spawn pager");

    // The gate refuses synchronously and exits; drain output until its cached status arrives.
    let gate_msg = "Managed policy is required for this account";
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut exit_code = None;
    while Instant::now() < deadline {
        harness.update(Duration::from_millis(100));
        if exit_code.is_none() {
            match wait_for_exit_status(&mut harness, Duration::ZERO).expect("poll gate exit") {
                PtyExitPoll::Exited(code) => exit_code = Some(code),
                PtyExitPoll::Running | PtyExitPoll::PendingStatus => {}
            }
            if exit_code.is_some() {
                harness.update(Duration::from_millis(200)); // final drain after exit
                break;
            }
        }
    }

    let raw = String::from_utf8_lossy(harness.raw_output()).into_owned();
    assert!(
        harness.contains_text(gate_msg) || raw.contains(gate_msg),
        "fail-closed gate refusal must reach the real terminal (fd 2 restored \
         from the /dev/null redirect); screen:\n{}\nraw:\n{raw}",
        harness.screen_contents()
    );
    assert_eq!(
        exit_code,
        Some(1),
        "fail-closed gate must exit 1 (refusal); got {exit_code:?}\nraw:\n{raw}"
    );
}
