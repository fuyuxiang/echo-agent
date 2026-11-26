// Per-test-case module for the `pty_e2e` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// **Campaign nudge via the real remote path** — the production source
/// (`GET /v1/settings` → `RemoteSettings.campaigns` → process cache seed →
/// apply/dismiss), unlike the sibling test which injects the campaign through
/// `ECHO_AGENT_CAMPAIGNS_OVERRIDE`.
///
/// - boot with a `[models].default` in config.toml plus a **server-served**
///   campaign nudging a *different* model → a (possibly not first — see
///   [`wait_for_model_via_new_sessions`]) new session opens on the
///   **campaign** model;
/// - pick the config model via `/model` → the remote campaign id is recorded
///   dismissed in `campaigns_state.json`;
/// - reboot against the *same* server settings → the **config** model wins
///   and stays winning across `/new`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PTY e2e; run the owning pty_e2e_* Cargo test with --ignored (see Cargo.toml)"]
async fn campaign_remote_settings_nudge_and_dismiss() {
    const CONFIG_MODEL: &str = "config-model";
    const CAMPAIGN_MODEL: &str = "campaign-model";
    const CAMPAIGN_ID: &str = "e2e-remote-nudge";

    let content = ContentController::start_with_models(vec![
        MockModel::new(CONFIG_MODEL),
        MockModel::new(CAMPAIGN_MODEL),
    ])
    .await
    .expect("start content with two models");

    // Serve the campaign from the settings endpoint (replaces the preset, so
    // `allow_access` must be restated or the pager parks on the upsell screen).
    content.server().set_settings(json!({
        "allow_access": true,
        "campaigns": [
            { "id": CAMPAIGN_ID, "models": { "default": CAMPAIGN_MODEL } }
        ]
    }));

    // Seed config.toml with the user's own default model.
    let echo_agent_home = content.home().join(".echo-agent");
    std::fs::create_dir_all(&echo_agent_home).expect("create ECHO_AGENT_HOME");
    std::fs::write(
        echo_agent_home.join("config.toml"),
        format!("[models]\ndefault = \"{CONFIG_MODEL}\"\n"),
    )
    .expect("write config.toml");

    // Session (OAuth) auth, not the harness's default ECHO_AGENT_API_KEY: the
    // settings fetch requires `auth_manager.auth()` — in ApiKey/BYOK mode the
    // pager never requests `/v1/settings`, so a remote campaign would be
    // structurally unreachable (see `spawn_polling_session`'s doc).
    seed_fake_oauth(&content, "pty-campaign-remote");
    let binary = pager_binary().expect("resolve pager binary");
    let spawn = || -> PtyHarness {
        PtyHarness::spawn_with_content_env_ops(
            &binary,
            DEFAULT_ROWS,
            DEFAULT_COLS,
            &content,
            &[],
            &oauth_credential_ops(),
        )
        .expect("spawn pager")
    };

    // ── Phase 1+2: the campaign applies to a new session; a pick dismisses. ──
    {
        let mut h = spawn();
        h.wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
            .expect("welcome renders");
        assert!(
            wait_for_model_via_new_sessions(&mut h, CAMPAIGN_MODEL, Duration::from_secs(60)),
            "a new session should open on the remote campaign model\nscreen:\n{}",
            h.screen_contents()
        );
        assert!(
            !h.contains_text("panicked"),
            "pager panicked\n{}",
            h.screen_contents()
        );

        // Explicit pick of the config model → persists default + dismisses.
        h.inject_keys(format!("/model {CONFIG_MODEL}\r").as_bytes())
            .expect("pick model");

        // Deterministically wait for the dismiss to land on disk.
        let state_path = echo_agent_home.join("campaigns_state.json");
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            h.update(Duration::from_millis(200));
            let dismissed = std::fs::read_to_string(&state_path)
                .map(|s| s.contains(CAMPAIGN_ID))
                .unwrap_or(false);
            if dismissed {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "remote campaign id should be recorded dismissed in {state_path:?}\nscreen:\n{}",
                h.screen_contents()
            );
        }
        h.quit().expect("clean quit");
    }

    // ── Phase 3: reboot against the SAME settings → the config model wins. ──
    {
        let mut h = spawn();
        h.wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
            .expect("welcome renders after reboot");
        h.wait_for_text(CONFIG_MODEL, Duration::from_secs(20))
            .unwrap_or_else(|_| {
                panic!(
                    "after dismissal the config model must show\nscreen:\n{}",
                    h.screen_contents()
                )
            });
        // Give the settings fetch time to land, then prove a fresh session
        // still resolves to the user's model (dismissed campaigns never
        // re-apply, even once the remote campaign is in the cache).
        let _ = h.inject_keys(b"/new\r");
        h.update(Duration::from_millis(4000));
        h.wait_for_text(CONFIG_MODEL, Duration::from_secs(10))
            .expect("config model after post-fetch /new");
        assert!(
            !h.contains_text(CAMPAIGN_MODEL),
            "a dismissed remote campaign must not re-nudge the model\nscreen:\n{}",
            h.screen_contents()
        );
        h.quit().expect("clean quit");
    }
}
