// Per-test-case module for the `leader_pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// **Leader mode: a `/model` pick in the TUI dismisses a remote campaign.**
///
/// The dismiss chokepoint (`persist_user_choice`) runs in the **TUI process**,
/// but in leader mode no in-process agent ever seeds the TUI's remote campaign
/// cache — only `app::run`'s own seed makes a remote campaign visible to
/// `resolve_dismissable_campaigns`. Without that seed this test times out in
/// the dismiss phase: the pick persists but no dismissal is recorded, and the
/// leader re-nudges every new session over the user's explicit choice.
///
/// The TUI's settings prefetch is deliberately 2s-capped, so on a loaded
/// runner a spawn can miss the fetch (unseeded cache — the documented
/// transient leader-mode divergence). The test retries with fresh TUI spawns
/// (same leader) until a pick lands the dismissal, then proves it sticks.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run with cargo test -p echo-agent-pager --test leader_pty_e2e -- --ignored --test-threads=1"]
async fn campaign_leader_mode_remote_dismiss_on_model_pick() {
    const CONFIG_MODEL: &str = "config-model";
    const CAMPAIGN_MODEL: &str = "campaign-model";
    const CAMPAIGN_ID: &str = "e2e-leader-remote-nudge";

    let content = ContentController::start_with_models(vec![
        MockModel::new(CONFIG_MODEL),
        MockModel::new(CAMPAIGN_MODEL),
    ])
    .await
    .expect("start content with two models");

    // Serve the campaign from the settings endpoint (restating `allow_access`,
    // which the preset otherwise provides).
    content.server().set_settings(json!({
        "allow_access": true,
        "campaigns": [
            { "id": CAMPAIGN_ID, "models": { "default": CAMPAIGN_MODEL } }
        ]
    }));

    // Seed config.toml with the user's own default model; a fixed leader
    // socket under the shared ECHO_AGENT_HOME so every spawn elects/attaches to the
    // same leader (mirrors `LeaderCluster`).
    let echo_agent_home = content.home().join(".echo-agent");
    std::fs::create_dir_all(&echo_agent_home).expect("create ECHO_AGENT_HOME");
    std::fs::write(
        echo_agent_home.join("config.toml"),
        format!("[models]\ndefault = \"{CONFIG_MODEL}\"\n"),
    )
    .expect("write config.toml");
    let socket = echo_agent_home.join("leader-e2e.sock");
    let socket = socket.to_str().expect("socket path is utf-8").to_owned();

    // Session (OAuth) auth, not the harness's default ECHO_AGENT_API_KEY: the
    // settings fetch requires `auth_manager.auth()` — in ApiKey/BYOK mode the
    // pager never requests `/v1/settings`, so a remote campaign would be
    // structurally unreachable (see `spawn_polling_session`'s doc).
    seed_fake_oauth(&content, "pty-campaign-leader");
    let binary = pager_binary().expect("resolve pager binary");
    let spawn = || -> PtyHarness {
        PtyHarness::spawn_with_content_env_ops(
            &binary,
            DEFAULT_ROWS,
            DEFAULT_COLS,
            &content,
            &["--leader", "--leader-socket", &socket],
            &oauth_credential_ops(),
        )
        .expect("spawn leader-mode pager")
    };
    let state_path = echo_agent_home.join("campaigns_state.json");
    let dismissed = |state_path: &std::path::Path| {
        std::fs::read_to_string(state_path)
            .map(|s| s.contains(CAMPAIGN_ID))
            .unwrap_or(false)
    };

    // ── Phase 1+2: nudge on a new session; a pick records the dismissal in
    // the TUI process. Retries fresh TUI spawns (same leader) so a missed
    // 2s prefetch window on a loaded runner can't wedge the test.
    let mut recorded = false;
    'attempts: for attempt in 0..3 {
        let mut h = spawn();
        // Cold leader bring-up (leader election plus an unoptimized-binary
        // boot) can miss the welcome paint within LEADER_TIMEOUT under
        // remote-runner load. The leader outlives this client, so a fresh spawn
        // attaches to the now-live leader and paints promptly: retry like a
        // missed campaign rather than hard-failing on the first loaded cold
        // start, and only panic once all attempts are exhausted.
        if h.wait_for_text(WELCOME_SCREEN_SENTINEL, LEADER_TIMEOUT)
            .is_err()
        {
            let screen = h.screen_contents();
            h.quit().expect("clean quit");
            assert!(
                attempt < 2,
                "leader-mode welcome never rendered after 3 attempts\nscreen:\n{screen}"
            );
            continue;
        }
        if !wait_for_model_via_new_sessions(&mut h, CAMPAIGN_MODEL, Duration::from_secs(60)) {
            // Campaign never applied on this spawn; try a fresh TUI.
            h.quit().expect("clean quit");
            continue;
        }
        h.inject_keys(format!("/model {CONFIG_MODEL}\r").as_bytes())
            .expect("pick model");
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            h.update(Duration::from_millis(200));
            if dismissed(&state_path) {
                recorded = true;
                h.quit().expect("clean quit");
                break 'attempts;
            }
        }
        // The regression under test: pick persisted but dismissal missing.
        // With the app::run seed present this only happens when the prefetch
        // missed on this spawn; retry once more before declaring failure.
        h.quit().expect("clean quit");
    }
    assert!(
        recorded,
        "leader-mode TUI must record the remote campaign dismissal in {state_path:?}"
    );

    // ── Phase 3: the dismissal is durable and the pick is persisted. The
    // user's choice must be in config.toml (campaign value never laundered
    // in), and the dismissed id on disk is what every future resolution —
    // leader or not — filters on (`dismissed_id_is_dropped_from_override`
    // pins the filter; the sibling remote-settings e2e pins the full
    // no-re-nudge reboot in-process). A fresh same-leader-socket client is
    // deliberately not asserted on-screen here: reattach paint timing is the
    // one flaky piece and adds no coverage over the disk + sibling asserts.
    let config =
        std::fs::read_to_string(echo_agent_home.join("config.toml")).expect("read config.toml");
    assert!(
        config.contains(&format!("default = \"{CONFIG_MODEL}\"")),
        "the user's pick must be persisted to config.toml:\n{config}"
    );
    assert!(
        !config.contains(CAMPAIGN_MODEL),
        "the campaign value must never be written to config.toml:\n{config}"
    );
    assert!(
        dismissed(&state_path),
        "the dismissal must survive on disk after the client exits"
    );
}
