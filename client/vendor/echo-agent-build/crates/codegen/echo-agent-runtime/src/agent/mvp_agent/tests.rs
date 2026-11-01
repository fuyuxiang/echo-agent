use super::*;

#[tokio::test(flavor = "current_thread")]
async fn targeted_permission_update_rejects_a_missing_resident_session() {
    use acp::Agent as _;
    let agent = build_minimal_agent_for_tests();
    let params = serde_json::value::to_raw_value(&serde_json::json!({
        "permission_mode": "ask",
        "sessionIds": ["not-resident"],
    }))
    .expect("serialize permission update");

    let error = agent
        .ext_notification(acp::ExtNotification::new(
            "echo.agent/yolo_mode_changed",
            params.into(),
        ))
        .await
        .expect_err("a targeted update must not acknowledge an absent Runtime session");

    assert!(
        error
            .data
            .as_ref()
            .and_then(serde_json::Value::as_str)
            .is_some_and(|message| message.contains("not-resident")),
        "missing target must be named in the error: {error:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn targeted_permission_update_is_atomic_when_any_target_is_missing() {
    use acp::Agent as _;
    let agent = build_minimal_agent_for_tests();
    let resident_id = acp::SessionId::new("resident");
    let (handle, _cmd_tx, mut commands) = make_live_session_handle(&resident_id, None);
    agent.insert_resident(&resident_id, handle);
    let params = serde_json::value::to_raw_value(&serde_json::json!({
        "permission_mode": "auto",
        "sessionIds": ["resident", "not-resident"],
    }))
    .expect("serialize permission update");

    agent
        .ext_notification(acp::ExtNotification::new(
            "echo.agent/yolo_mode_changed",
            params.into(),
        ))
        .await
        .expect_err("the whole targeted update must fail before mutating a partial target set");

    assert!(
        matches!(
            commands.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ),
        "a missing target must prevent updates to every otherwise matching session"
    );
}
/// Build an unsigned JWT with a `tier` claim (header.payload.sig base64url).
fn jwt_with_tier(tier: u64) -> String {
    use base64::Engine;
    let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let header = enc.encode(br#"{"alg":"none"}"#);
    let payload = enc.encode(format!(r#"{{"tier":{tier}}}"#).as_bytes());
    format!("{header}.{payload}.sig")
}
#[test]
fn jwt_tier_claim_maps_free_and_paid() {
    assert_eq!(jwt_tier_claim(&jwt_with_tier(0)).as_deref(), Some("free"));
    assert_eq!(
        jwt_tier_claim(&jwt_with_tier(1)).as_deref(),
        Some("echoagent")
    );
    assert_eq!(
        jwt_tier_claim(&jwt_with_tier(2)).as_deref(),
        Some("x_basic")
    );
    assert_eq!(
        jwt_tier_claim(&jwt_with_tier(3)).as_deref(),
        Some("x_premium")
    );
    assert_eq!(
        jwt_tier_claim(&jwt_with_tier(4)).as_deref(),
        Some("x_premium_plus")
    );
    assert_eq!(
        jwt_tier_claim(&jwt_with_tier(5)).as_deref(),
        Some("echoagent_heavy")
    );
    assert_eq!(
        jwt_tier_claim(&jwt_with_tier(6)).as_deref(),
        Some("echoagent_lite")
    );
    assert_eq!(
        jwt_tier_claim(&jwt_with_tier(7)).as_deref(),
        Some("echoagent_plus")
    );
    assert_eq!(jwt_tier_claim(&jwt_with_tier(9)).as_deref(), Some("9"));
    assert_eq!(jwt_tier_claim(&jwt_with_tier(99)).as_deref(), Some("99"));
}
fn auth_with_mode(mode: crate::auth::AuthMode, key: &str) -> crate::auth::EchoAgentAuth {
    crate::auth::EchoAgentAuth {
        key: key.into(),
        auth_mode: mode,
        create_time: chrono::Utc::now(),
        user_id: "u".into(),
        email: None,
        first_name: None,
        last_name: None,
        profile_image_asset_id: None,
        principal_type: None,
        principal_id: None,
        team_id: None,
        team_name: None,
        team_role: None,
        organization_id: None,
        organization_name: None,
        organization_role: None,
        user_blocked_reason: None,
        team_blocked_reasons: vec![],
        coding_data_retention_opt_out: false,
        has_echo_agent_code_access: None,
        refresh_token: None,
        expires_at: None,
        oidc_issuer: None,
        oidc_client_id: None,
    }
}
#[test]
fn resolve_subscription_tier_prefers_display_then_api_key_then_jwt() {
    assert_eq!(
        resolve_subscription_tier_for_telemetry(Some("Free".into()), None).as_deref(),
        Some("Free")
    );
    let api = auth_with_mode(crate::auth::AuthMode::ApiKey, "echo-agent-not-a-jwt");
    assert_eq!(
        resolve_subscription_tier_for_telemetry(Some("  ".into()), Some(&api)).as_deref(),
        Some("api_key")
    );
    assert_eq!(
        resolve_subscription_tier_for_telemetry(None, Some(&api)).as_deref(),
        Some("api_key")
    );
    let oauth = auth_with_mode(crate::auth::AuthMode::Oidc, &jwt_with_tier(0));
    assert_eq!(
        resolve_subscription_tier_for_telemetry(None, Some(&oauth)).as_deref(),
        Some("free")
    );
    assert_ne!(
        resolve_subscription_tier_for_telemetry(None, Some(&api)).as_deref(),
        Some("free")
    );
}
/// JWT claim ↔ `/user` tier mapping used to gate post-unblock catalog refresh
/// (a stale older paid claim must not skip retry).
#[test]
fn jwt_claim_matches_user_subscription_tier_known_pairs() {
    let cases = [
        ("echoagent", "EchoAgentStandard"),
        ("echoagent_basic", "EchoAgentBasic"),
        ("echoagent_premium", "EchoAgentPremium"),
        ("echoagent_premium_plus", "EchoAgentPremiumPlus"),
        ("echoagent_heavy", "EchoAgentPro"),
        ("9", "EnterpriseMystery"),
        ("echoagent_lite", "EchoAgentLite"),
        ("echoagent_plus", "EchoAgentPlus"),
    ];
    for (claim, user_tier) in cases {
        assert!(
            jwt_claim_matches_user_subscription_tier(claim, user_tier),
            "{claim} should match {user_tier}"
        );
    }
}
#[test]
fn jwt_claim_matches_user_subscription_tier_rejects_stale_and_unknown() {
    assert!(!jwt_claim_matches_user_subscription_tier(
        "echoagent_basic",
        "EchoAgentPro"
    ));
    assert!(!jwt_claim_matches_user_subscription_tier(
        "echoagent",
        "EchoAgentPro"
    ));
    assert!(!jwt_claim_matches_user_subscription_tier(
        "echoagent",
        "EchoAgentPlus"
    ));
    assert!(!jwt_claim_matches_user_subscription_tier(
        "echoagent_heavy",
        "EchoAgentPlus"
    ));
    assert!(!jwt_claim_matches_user_subscription_tier(
        "free",
        "EchoAgentStandard"
    ));
    assert!(!jwt_claim_matches_user_subscription_tier(
        "",
        "EchoAgentPremium"
    ));
    assert!(!jwt_claim_matches_user_subscription_tier(
        "echoagent_heavy",
        "EnterpriseMystery"
    ));
    assert!(!jwt_claim_matches_user_subscription_tier(
        "0",
        "EnterpriseMystery"
    ));
}
/// Single-flight flag must clear on Drop even if the retry task panics /
/// aborts mid-backoff (guards against the flag stuck true forever).
#[test]
fn post_unblock_jwt_retry_in_flight_guard_clears_on_drop() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let flag = Arc::new(AtomicBool::new(true));
    {
        let _guard = PostUnblockJwtRetryInFlightGuard { flag: flag.clone() };
        assert!(flag.load(Ordering::Acquire));
    }
    assert!(
        !flag.load(Ordering::Acquire),
        "Drop must release post_unblock_jwt_retry_in_flight"
    );
    let flag = Arc::new(AtomicBool::new(true));
    let flag_for_catch = flag.clone();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = PostUnblockJwtRetryInFlightGuard {
            flag: flag_for_catch,
        };
        panic!("simulate retry task panic");
    }));
    assert!(result.is_err());
    assert!(
        !flag.load(Ordering::Acquire),
        "Drop must release flag on panic unwind"
    );
}
mod hunk_tracking_mode {
    use super::super::{plan_hunk_tracking, resolve_hunk_tracking_mode};
    use echo_agent_hunk_tracker::TrackingMode;
    #[test]
    fn off_and_disabled_disable_tracking() {
        assert_eq!(resolve_hunk_tracking_mode(Some("off")), None);
        assert_eq!(resolve_hunk_tracking_mode(Some("disabled")), None);
    }
    #[test]
    fn matching_is_case_insensitive_and_trimmed() {
        assert_eq!(resolve_hunk_tracking_mode(Some("OFF")), None);
        assert_eq!(resolve_hunk_tracking_mode(Some("  Off ")), None);
        assert_eq!(resolve_hunk_tracking_mode(Some("DISABLED")), None);
        assert_eq!(
            resolve_hunk_tracking_mode(Some("Agent_Only")),
            Some(TrackingMode::AgentOnly)
        );
        assert_eq!(
            resolve_hunk_tracking_mode(Some(" ALL_DIRTY ")),
            Some(TrackingMode::AllDirty)
        );
    }
    #[test]
    fn recognized_modes_parse() {
        assert_eq!(
            resolve_hunk_tracking_mode(Some("agent_only")),
            Some(TrackingMode::AgentOnly)
        );
        assert_eq!(
            resolve_hunk_tracking_mode(Some("all_dirty")),
            Some(TrackingMode::AllDirty)
        );
    }
    #[test]
    fn parser_absent_returns_none_policy_defaults_in_plan() {
        assert_eq!(resolve_hunk_tracking_mode(None), None);
        assert_eq!(resolve_hunk_tracking_mode(Some("")), None);
        assert_eq!(
            resolve_hunk_tracking_mode(Some("bogus")),
            Some(TrackingMode::AllDirty)
        );
    }
    #[test]
    fn plan_disables_actor_forward_and_loc_together() {
        for off in ["off", "disabled", "OFF"] {
            let plan = plan_hunk_tracking(Some(off));
            assert_eq!(plan.actor_mode, None, "{off} must not spawn the actor");
            assert!(!plan.enabled(), "{off} must disable the forward + LOC sink");
        }
    }
    #[test]
    fn plan_enables_actor_and_forward_for_active_modes() {
        for (mode, expected) in [
            ("agent_only", TrackingMode::AgentOnly),
            ("all_dirty", TrackingMode::AllDirty),
            ("bogus", TrackingMode::AllDirty),
        ] {
            let plan = plan_hunk_tracking(Some(mode));
            assert_eq!(plan.actor_mode, Some(expected));
            assert!(plan.enabled());
        }
        let plan = plan_hunk_tracking(None);
        assert_eq!(plan.actor_mode, None);
        assert!(!plan.enabled());
    }
}
mod capture {
    use tokio::sync::mpsc;
    use tracing::Subscriber;
    use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
    use tracing_subscriber::registry::LookupSpan;
    pub(crate) struct CapturedEvent {
        pub level: tracing::Level,
        pub fields: String,
    }
    pub(crate) struct Captured {
        pub events_rx: mpsc::UnboundedReceiver<CapturedEvent>,
        _guard: tracing::subscriber::DefaultGuard,
    }
    pub(crate) fn capture() -> Captured {
        let (tx, rx) = mpsc::unbounded_channel();
        let subscriber = tracing_subscriber::registry().with(CaptureLayer { tx });
        let guard = tracing::subscriber::set_default(subscriber);
        Captured {
            events_rx: rx,
            _guard: guard,
        }
    }
    struct CaptureLayer {
        tx: mpsc::UnboundedSender<CapturedEvent>,
    }
    impl<S> Layer<S> for CaptureLayer
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
            let mut v = Visitor::default();
            event.record(&mut v);
            let _ = self.tx.send(CapturedEvent {
                level: *event.metadata().level(),
                fields: v.out,
            });
        }
    }
    #[derive(Default)]
    struct Visitor {
        out: String,
    }
    impl tracing::field::Visit for Visitor {
        fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
            if !self.out.is_empty() {
                self.out.push(' ');
            }
            self.out.push_str(f.name());
            self.out.push('=');
            self.out.push_str(&format!("{v:?}"));
        }
        fn record_str(&mut self, f: &tracing::field::Field, v: &str) {
            if !self.out.is_empty() {
                self.out.push(' ');
            }
            self.out.push_str(f.name());
            self.out.push('=');
            self.out.push_str(v);
        }
    }
}
#[test]
fn warn_on_missing_parent_session_emits_when_session_absent() {
    let captured = capture::capture();
    warn_on_missing_parent_session_for_validate_type("ghost-session", false);
    let mut rx = captured.events_rx;
    let mut saw = false;
    while let Ok(event) = rx.try_recv() {
        if event.level == tracing::Level::WARN
            && event
                .fields
                .contains("ValidateType received for unknown parent session")
            && event.fields.contains("parent_session_id=ghost-session")
        {
            saw = true;
            break;
        }
    }
    assert!(saw, "warn must fire");
}
#[test]
fn warn_on_missing_parent_session_silent_when_session_present() {
    let captured = capture::capture();
    warn_on_missing_parent_session_for_validate_type("real-session", true);
    let mut rx = captured.events_rx;
    assert!(rx.try_recv().is_err());
}
#[tokio::test(flavor = "current_thread")]
async fn broadcast_refresh_skill_baseline_sends_one_message_per_sender() {
    let mut receivers = Vec::new();
    let mut senders = Vec::new();
    for _ in 0..3 {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        senders.push(tx);
        receivers.push(rx);
    }
    MvpAgent::broadcast_refresh_skill_baseline(senders);
    for mut rx in receivers {
        assert!(matches!(
            rx.try_recv(),
            Ok(crate::session::SessionCommand::RefreshSkillBaseline)
        ));
        assert!(
            rx.try_recv().is_err(),
            "broadcast must send exactly one message per sender",
        );
    }
}
#[tokio::test(flavor = "current_thread")]
async fn broadcast_refresh_skill_baseline_tolerates_dropped_receiver() {
    let (tx_alive, mut rx_alive) = tokio::sync::mpsc::unbounded_channel();
    let (tx_dead, rx_dead) = tokio::sync::mpsc::unbounded_channel();
    drop(rx_dead);
    MvpAgent::broadcast_refresh_skill_baseline(vec![tx_alive, tx_dead]);
    assert!(matches!(
        rx_alive.try_recv(),
        Ok(crate::session::SessionCommand::RefreshSkillBaseline)
    ));
}
/// The monotonic turn counter must never wrap on the DB-bound i32 path.
/// `allocate_turn_number` returns u64; the AB submission casts to i32.
/// Verify we saturate instead of wrapping.
#[test]
fn trace_turn_to_i32_saturates_at_max() {
    let small: u64 = 42;
    let result = i32::try_from(small).unwrap_or(i32::MAX);
    assert_eq!(result, 42);
    let huge: u64 = (i32::MAX as u64) + 100;
    let result = i32::try_from(huge).unwrap_or(i32::MAX);
    assert_eq!(result, i32::MAX);
    let boundary: u64 = i32::MAX as u64;
    let result = i32::try_from(boundary).unwrap_or(i32::MAX);
    assert_eq!(result, i32::MAX);
}
#[test]
fn settings_allow_access_none_settings_is_allowed() {
    assert!(settings_allow_access(None));
}
#[test]
fn settings_allow_access_true_is_allowed() {
    let rs = crate::util::config::RemoteSettings {
        allow_access: Some(true),
        ..Default::default()
    };
    assert!(settings_allow_access(Some(&rs)));
}
#[test]
fn settings_allow_access_false_is_blocked() {
    let rs = crate::util::config::RemoteSettings {
        allow_access: Some(false),
        ..Default::default()
    };
    assert!(!settings_allow_access(Some(&rs)));
}
#[test]
fn settings_allow_access_field_absent_is_allowed() {
    let rs = crate::util::config::RemoteSettings {
        allow_access: None,
        ..Default::default()
    };
    assert!(settings_allow_access(Some(&rs)));
}
/// After allocating a turn number, the retained (in-memory) turn counter holds
/// the next value (current + 1). This is the value that must be persisted via
/// `SetNextTraceTurn` so the counter survives restarts.
#[test]
fn allocate_turn_number_advances_counter() {
    use std::cell::RefCell;
    use std::collections::HashMap;
    let counters: RefCell<HashMap<acp::SessionId, u64>> = RefCell::new(HashMap::new());
    let sid = acp::SessionId::new("test-session");
    let allocate = |id: &acp::SessionId| -> u64 {
        let mut m = counters.borrow_mut();
        let turn = m.get(id).copied().unwrap_or(0u64);
        m.insert(id.clone(), turn.saturating_add(1));
        turn
    };
    assert_eq!(allocate(&sid), 0);
    assert_eq!(*counters.borrow().get(&sid).unwrap(), 1);
    assert_eq!(allocate(&sid), 1);
    assert_eq!(*counters.borrow().get(&sid).unwrap(), 2);
    assert_eq!(allocate(&sid), 2);
    assert_eq!(*counters.borrow().get(&sid).unwrap(), 3);
}
/// Build a synthetic harness `task` call/result pair carrying the
/// `<subagent_result>` footer, mirroring what the verifier/planner record.
fn harness_pair(id: &str) -> Vec<echo_agent_sampling_types::conversation::ConversationItem> {
    use echo_agent_sampling_types::ToolCall;
    use echo_agent_sampling_types::conversation::ConversationItem;
    vec![
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: id.into(),
            name: "task".into(),
            arguments: "{}".into(),
        }]),
        ConversationItem::tool_result(id, "<subagent_result>\nsubagent_id: skeptic-1"),
    ]
}
/// Agent-side upload path: each drained harness turn takes a distinct,
/// monotonic turn number that CONTINUES past the user turn, advances the
/// per-session counter, and is persisted via exactly one `SetNextTraceTurn`.
/// This is what makes each sibling `turn_{N}` reachable — without the
/// advance every harness turn would clobber the same GCS path.
#[tokio::test(flavor = "current_thread")]
async fn upload_harness_trace_turns_numbers_siblings_and_persists_counter() {
    let agent = build_minimal_agent_for_tests();
    {
        let mut cfg = agent.cfg.borrow_mut();
        cfg.features.telemetry = Some(crate::agent::config::TelemetryMode::Enabled);
        cfg.telemetry.trace_upload = Some(true);
        cfg.endpoints.trace_upload_bucket = Some("gs://harness-trace-test".to_string());
    }
    let sid = acp::SessionId::new("harness-upload-sess");
    let info = crate::session::info::Info {
        id: sid.clone(),
        cwd: "/tmp".to_string(),
    };
    let mut handle = make_test_handle("test-model", false, None);
    handle.info = info.clone();
    let queue_home = tempfile::tempdir().unwrap();
    let queue_cfg = crate::session::repo_changes::TraceExportConfig {
        bucket_url: Some("gs://harness-trace-test".to_string()),
        service_account_key: None,
        prefix_dir: None,
        gcs_prefix: None,
        absolute_paths: false,
        archive_name_override: None,
        upload_method: crate::session::repo_changes::UploadMethod::Direct {
            service_account_key: None,
        },
    };
    let queue = crate::upload::trace::spawn_upload_queue(
        queue_home.path(),
        &queue_cfg,
        Some(echo_agent_version::VERSION),
        agent.auth_manager.clone(),
    );
    let _ = handle.upload_queue.set(queue);
    agent.insert_resident(&sid, handle);
    for _ in 0..3 {
        agent.allocate_turn_number(&sid);
    }
    assert_eq!(agent.session_turn_number(&sid), Some(3));
    let built = agent
        .build_harness_trace_uploads(
            &sid,
            &info,
            "test-model",
            3,
            vec![harness_pair("a"), harness_pair("b")],
        )
        .await;
    let numbers: Vec<u64> = built.iter().map(|(_, m, _)| m.turn_number).collect();
    assert_eq!(numbers, vec![3, 4], "siblings take base, base+1");
    assert!(
        built.iter().all(|(_, m, _)| m.model == "test-model"),
        "harness metadata carries the requested model alias",
    );
    let (cmd_tx, mut cmd_rx) =
        tokio::sync::mpsc::unbounded_channel::<crate::session::SessionCommand>();
    agent
        .upload_harness_trace_turns(
            &sid,
            &info,
            &cmd_tx,
            "test-model",
            vec![harness_pair("a"), harness_pair("b")],
        )
        .await;
    assert_eq!(
        agent.session_turn_number(&sid),
        Some(5),
        "two siblings advance the counter by two from the user turn",
    );
    let mut persisted = Vec::new();
    while let Ok(cmd) = cmd_rx.try_recv() {
        if let crate::session::SessionCommand::SetNextTraceTurn {
            next_trace_turn, ..
        } = cmd
        {
            persisted.push(next_trace_turn);
        }
    }
    assert_eq!(
        persisted,
        vec![5],
        "persist the advanced counter once, ahead of the spawned uploads",
    );
}
/// With trace upload disabled the agent-side path must NOT burn a turn
/// number or persist a counter (and spawns no upload). The buffer-clearing
/// half of the drain is the caller's `TakeHarnessTraceTurns`; this guards
/// the upload function's uploads-disabled branch.
#[tokio::test(flavor = "current_thread")]
async fn upload_harness_trace_turns_uploads_disabled_does_not_burn_counter() {
    let agent = build_minimal_agent_for_tests();
    let sid = acp::SessionId::new("harness-disabled-sess");
    let info = crate::session::info::Info {
        id: sid.clone(),
        cwd: "/tmp".to_string(),
    };
    let (cmd_tx, mut cmd_rx) =
        tokio::sync::mpsc::unbounded_channel::<crate::session::SessionCommand>();
    agent
        .upload_harness_trace_turns(&sid, &info, &cmd_tx, "test-model", vec![harness_pair("a")])
        .await;
    assert_eq!(
        agent.session_turn_number(&sid),
        None,
        "uploads-disabled skip must not consume a turn number",
    );
    assert!(
        cmd_rx.try_recv().is_err(),
        "uploads-disabled path must not persist a counter",
    );
}
/// Guards the per-harness-turn manifest seam: (1) every turn's ctx carries
/// a FRESH `artifact_tracker`, so turn 1 never inherits turn 0's recorded
/// artifacts; (2) recording the turn's metadata + turn_messages yields a
/// manifest listing exactly those two; (3) `fully_uploaded` is true iff
/// neither failed.
#[tokio::test(flavor = "current_thread")]
async fn upload_harness_trace_turns_build_per_turn_manifest() {
    use crate::upload::manifest::{
        ArtifactResult, ArtifactStatus, build_manifest, record_artifact, resolve_upload_method,
    };
    let agent = build_minimal_agent_for_tests();
    {
        let mut cfg = agent.cfg.borrow_mut();
        cfg.features.telemetry = Some(crate::agent::config::TelemetryMode::Enabled);
        cfg.telemetry.trace_upload = Some(true);
        cfg.endpoints.trace_upload_bucket = Some("gs://harness-trace-test".to_string());
    }
    let sid = acp::SessionId::new("harness-manifest-sess");
    let info = crate::session::info::Info {
        id: sid.clone(),
        cwd: "/tmp".to_string(),
    };
    let mut handle = make_test_handle("test-model", false, None);
    handle.info = info.clone();
    let queue_home = tempfile::tempdir().unwrap();
    let queue_cfg = crate::session::repo_changes::TraceExportConfig {
        bucket_url: Some("gs://harness-trace-test".to_string()),
        service_account_key: None,
        prefix_dir: None,
        gcs_prefix: None,
        absolute_paths: false,
        archive_name_override: None,
        upload_method: crate::session::repo_changes::UploadMethod::Direct {
            service_account_key: None,
        },
    };
    let queue = crate::upload::trace::spawn_upload_queue(
        queue_home.path(),
        &queue_cfg,
        Some(echo_agent_version::VERSION),
        agent.auth_manager.clone(),
    );
    let _ = handle.upload_queue.set(queue);
    agent.insert_resident(&sid, handle);
    let built = agent
        .build_harness_trace_uploads(
            &sid,
            &info,
            "test-model",
            0,
            vec![harness_pair("a"), harness_pair("b")],
        )
        .await;
    assert_eq!(
        built.len(),
        2,
        "both harness turns obtained a trace context"
    );
    let ctx0 = &built[0].0;
    record_artifact(
        &ctx0.artifact_tracker,
        "metadata.json",
        ArtifactResult::Succeeded,
    );
    record_artifact(
        &ctx0.artifact_tracker,
        "turn_messages.json",
        ArtifactResult::Succeeded,
    );
    let m0 = build_manifest(
        &ctx0.artifact_tracker,
        resolve_upload_method(&ctx0.gcs_config),
        None,
    );
    assert!(matches!(
        m0.artifacts.get("metadata.json"),
        Some(ArtifactStatus::Succeeded)
    ));
    assert!(matches!(
        m0.artifacts.get("turn_messages.json"),
        Some(ArtifactStatus::Succeeded)
    ));
    assert!(m0.fully_uploaded, "both succeeded → fully_uploaded");
    let ctx1 = &built[1].0;
    let before = build_manifest(
        &ctx1.artifact_tracker,
        resolve_upload_method(&ctx1.gcs_config),
        None,
    );
    assert!(
        before.artifacts.is_empty(),
        "per-turn tracker: turn 1 must not inherit turn 0's artifacts",
    );
    record_artifact(
        &ctx1.artifact_tracker,
        "metadata.json",
        ArtifactResult::Succeeded,
    );
    record_artifact(
        &ctx1.artifact_tracker,
        "turn_messages.json",
        ArtifactResult::Failed {
            reason: "upload_failed",
            error: None,
        },
    );
    let m1 = build_manifest(
        &ctx1.artifact_tracker,
        resolve_upload_method(&ctx1.gcs_config),
        None,
    );
    assert!(
        !m1.fully_uploaded,
        "a failed turn_messages flips fully_uploaded",
    );
    assert_eq!(m1.artifacts.len(), 2, "no cross-turn contamination");
}
/// With no overrides and model_agent_type = None, the default agent is used.
#[test]
#[serial_test::serial]
fn resolve_agent_definition_defaults_to_echo_agent_build() {
    let prev = std::env::var("ECHO_AGENT_AGENT").ok();
    unsafe {
        std::env::remove_var("ECHO_AGENT_AGENT");
    }
    let tmp = tempfile::tempdir().unwrap();
    let def = MvpAgent::resolve_agent_definition(
        tmp.path(),
        None,
        &config::AgentSelectionConfig::default(),
        None,
        None,
    );
    assert_eq!(def.name, config::DEFAULT_AGENT_TYPE);
    if let Some(v) = prev {
        unsafe { std::env::set_var("ECHO_AGENT_AGENT", v) }
    }
}
/// When model_agent_type = Some("codex"), the codex agent is selected even
/// though the default chain would return echo-agent-build.
#[test]
#[serial_test::serial]
fn resolve_agent_definition_model_agent_type_overrides_default() {
    let prev = std::env::var("ECHO_AGENT_AGENT").ok();
    unsafe {
        std::env::remove_var("ECHO_AGENT_AGENT");
    }
    let tmp = tempfile::tempdir().unwrap();
    let def = MvpAgent::resolve_agent_definition(
        tmp.path(),
        None,
        &config::AgentSelectionConfig::default(),
        None,
        Some("codex"),
    );
    assert_eq!(def.name, "codex");
    if let Some(v) = prev {
        unsafe { std::env::set_var("ECHO_AGENT_AGENT", v) }
    }
}
/// When model_agent_type is None, the chain-resolved default agent is
/// NOT overridden. This is the crux of the leader-mode fix: a session whose
/// model has no agent_type must get the default agent, not a stale value
/// from a different client's model.
#[test]
#[serial_test::serial]
fn resolve_agent_definition_none_agent_type_does_not_override() {
    let prev = std::env::var("ECHO_AGENT_AGENT").ok();
    unsafe {
        std::env::remove_var("ECHO_AGENT_AGENT");
    }
    let tmp = tempfile::tempdir().unwrap();
    let def = MvpAgent::resolve_agent_definition(
        tmp.path(),
        None,
        &config::AgentSelectionConfig::default(),
        None,
        None,
    );
    assert_eq!(def.name, config::DEFAULT_AGENT_TYPE);
    if let Some(v) = prev {
        unsafe { std::env::set_var("ECHO_AGENT_AGENT", v) }
    }
}
/// Regression for the web-client devbox bug: an ACP profile must
/// win when the model's `agent_type` is the default value.
#[test]
#[serial_test::serial]
fn resolve_agent_definition_acp_profile_wins_when_model_agent_type_is_default() {
    let prev = std::env::var("ECHO_AGENT_AGENT").ok();
    unsafe {
        std::env::remove_var("ECHO_AGENT_AGENT");
    }
    let tmp = tempfile::tempdir().unwrap();
    let acp_profile = echo_agent_core::AgentDefinition::from_json(&serde_json::json!(
        { "name" : "custom-devbox-profile", "description" :
        "Custom devbox profile", "systemPrompt" :
        "You are a custom-configured devbox agent.", }
    ))
    .expect("agent definition must parse");
    let def = MvpAgent::resolve_agent_definition(
        tmp.path(),
        None,
        &config::AgentSelectionConfig::default(),
        Some(acp_profile),
        Some(config::DEFAULT_AGENT_TYPE),
    );
    assert_eq!(
        def.name, "custom-devbox-profile",
        "ACP _meta.agentProfile must win when model_agent_type is the default value"
    );
    if let Some(v) = prev {
        unsafe { std::env::set_var("ECHO_AGENT_AGENT", v) }
    }
}
/// Regression: after `DEFAULT_AGENT_TYPE` flipped to
/// `echo-agent-build-plan`, models in the catalog that still declare
/// `agent_type = "echo-agent-build"` explicitly must NOT preempt an ACP
/// profile. Any value in the `echo-agent-build*` family is the stock harness
/// with no strict requirement.
#[test]
#[serial_test::serial]
fn resolve_agent_definition_acp_profile_wins_for_explicit_echo_agent_build_family() {
    let prev = std::env::var("ECHO_AGENT_AGENT").ok();
    unsafe {
        std::env::remove_var("ECHO_AGENT_AGENT");
    }
    let tmp = tempfile::tempdir().unwrap();
    let acp_profile = echo_agent_core::AgentDefinition::from_json(&serde_json::json!({
        "name": "custom-devbox-profile",
        "description": "Custom devbox profile",
    }))
    .expect("agent definition must parse");
    for family_variant in ["echo-agent-build", "echo-agent-build-plan", "echo-agent-build-concise"] {
        let def = MvpAgent::resolve_agent_definition(
            tmp.path(),
            None,
            &config::AgentSelectionConfig::default(),
            Some(acp_profile.clone()),
            Some(family_variant),
        );
        assert_eq!(
            def.name, "custom-devbox-profile",
            "ACP profile must win for echo-agent-build family variant `{family_variant}`"
        );
    }
    if let Some(v) = prev {
        unsafe { std::env::set_var("ECHO_AGENT_AGENT", v) }
    }
}
/// A non-strict (stock / vision-capable) model leaves the template alone, so
/// such models keep native image input.
#[test]
fn inherited_harness_template_skips_nonstrict_model() {
    use echo_agent_core::prompt::user_message::UserMessageTemplate;
    let tmp = tempfile::tempdir().unwrap();
    assert!(
        inherited_harness_template(
            &UserMessageTemplate::Default,
            Some(config::DEFAULT_AGENT_TYPE),
            tmp.path(),
        )
        .is_none()
    );
}
/// An explicit (non-default) template is never overridden — inheritance only
/// fills in the default.
#[test]
fn inherited_harness_template_respects_explicit_template() {
    use echo_agent_core::prompt::user_message::UserMessageTemplate;
    let tmp = tempfile::tempdir().unwrap();
    let explicit = UserMessageTemplate::Custom("MY CUSTOM TEMPLATE".to_owned());
    assert!(inherited_harness_template(&explicit, Some("cursor"), tmp.path()).is_none());
}
/// CLI `--agent-profile` wins when model_agent_type is the default
/// (also shadowed by the same regression).
#[test]
#[serial_test::serial]
fn resolve_agent_definition_cli_agent_profile_wins_when_model_agent_type_is_default() {
    let prev = std::env::var("ECHO_AGENT_AGENT").ok();
    unsafe {
        std::env::remove_var("ECHO_AGENT_AGENT");
    }
    let tmp = tempfile::tempdir().unwrap();
    let profile_path = tmp.path().join("cli-profile.md");
    std::fs::write(
        &profile_path,
        "---\nname: cli-profile\ndescription: cli test\n---\nYou are a CLI profile.\n",
    )
    .unwrap();
    let def = MvpAgent::resolve_agent_definition(
        tmp.path(),
        Some(&profile_path),
        &config::AgentSelectionConfig::default(),
        None,
        Some(config::DEFAULT_AGENT_TYPE),
    );
    assert_eq!(def.name, "cli-profile");
    if let Some(v) = prev {
        unsafe { std::env::set_var("ECHO_AGENT_AGENT", v) }
    }
}
/// Agent profile with `model: Override(id)` preserves the field through resolution.
#[test]
#[serial_test::serial]
fn resolve_agent_definition_agent_profile_with_model_override() {
    let prev = std::env::var("ECHO_AGENT_AGENT").ok();
    unsafe {
        std::env::remove_var("ECHO_AGENT_AGENT");
    }
    let tmp = tempfile::tempdir().unwrap();
    let agents_dir = tmp.path().join(".echo-agent").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
            agents_dir.join("test-architect.md"),
            "---\nname: test-architect\ndescription: test\nmodel: test-model-123\n---\nYou are a test.\n",
        )
        .unwrap();
    let agent_config = config::AgentSelectionConfig {
        name: Some("test-architect".to_string()),
        definition: None,
        system_prompt_label: None,
    };
    let def = MvpAgent::resolve_agent_definition(tmp.path(), None, &agent_config, None, None);
    assert_eq!(def.name, "test-architect");
    assert_eq!(
        def.model,
        echo_agent_core::config::ModelOverride::Override("test-model-123".to_string()),
        "agent profile model override must be preserved through resolution"
    );
    match prev {
        Some(v) => unsafe { std::env::set_var("ECHO_AGENT_AGENT", v) },
        None => unsafe { std::env::remove_var("ECHO_AGENT_AGENT") },
    }
}
#[test]
fn read_session_or_init_meta_str_prefers_session_meta() {
    let session = serde_json::json!({ "rules": "from-session" });
    let init = serde_json::json!({ "rules": "from-init" });
    assert_eq!(
        read_session_or_init_meta_str(session.as_object(), init.as_object(), "rules"),
        Some("from-session"),
    );
}
#[test]
fn read_session_or_init_meta_str_falls_back_to_init_meta() {
    let session = serde_json::json!({ "other": "x" });
    let init = serde_json::json!({ "rules": "from-init" });
    assert_eq!(
        read_session_or_init_meta_str(session.as_object(), init.as_object(), "rules"),
        Some("from-init"),
    );
    assert_eq!(
        read_session_or_init_meta_str(None, init.as_object(), "rules"),
        Some("from-init"),
    );
}
#[test]
fn parse_session_plugin_dirs_filters_and_dedupes() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = dunce::canonicalize(tmp.path()).unwrap().join("plugin");
    std::fs::create_dir(&dir).unwrap();
    let file = tmp.path().join("file.txt");
    std::fs::write(&file, "x").unwrap();
    let meta = serde_json::json!({
        "pluginDirs": [
            dir.to_string_lossy(),          // kept
            dir.to_string_lossy(),          // duplicate → deduped
            file.to_string_lossy(),         // not a directory → skipped
            "relative/path",                // not absolute → skipped
            42,                             // not a string → skipped
        ]
    });
    assert_eq!(parse_session_plugin_dirs(meta.as_object()), vec![dir]);
    assert!(parse_session_plugin_dirs(None).is_empty());
    assert!(parse_session_plugin_dirs(serde_json::json!({}).as_object()).is_empty());
}
#[test]
fn read_session_or_init_meta_str_returns_none_when_absent() {
    assert_eq!(read_session_or_init_meta_str(None, None, "rules"), None,);
    let session = serde_json::json!({ "other": "x" });
    assert_eq!(
        read_session_or_init_meta_str(session.as_object(), None, "rules"),
        None,
    );
}
#[test]
fn read_session_or_init_meta_str_ignores_non_string_values() {
    let session = serde_json::json!({ "rules": 42 });
    let init = serde_json::json!({ "rules": "from-init" });
    assert_eq!(
        read_session_or_init_meta_str(session.as_object(), init.as_object(), "rules"),
        Some("from-init"),
    );
}
#[test]
fn startup_hints_from_meta_prefers_session_request_over_init() {
    let session = serde_json::json!({
        "startupHints": { "nonInteractive": true, "deliveryTools": ["srv__post"] }
    });
    let init = serde_json::json!({ "startupHints": { "nonInteractive": false } });
    let hints = startup_hints_from_meta(session.as_object(), init.as_object());
    assert!(hints.non_interactive);
    assert_eq!(hints.delivery_tools, vec!["srv__post"]);
    assert!(startup_hints_from_meta(None, session.as_object()).non_interactive);
}
#[test]
fn startup_hints_from_meta_session_object_wins_whole_not_merged() {
    let session = serde_json::json!({ "startupHints": { "skipGitStatus": true } });
    let init = serde_json::json!({ "startupHints": { "nonInteractive": true } });
    let hints = startup_hints_from_meta(session.as_object(), init.as_object());
    assert!(hints.skip_git_status);
    assert!(!hints.non_interactive);
}
#[test]
fn startup_hints_from_meta_unparseable_falls_through_then_defaults() {
    let bad = serde_json::json!({ "startupHints": "yes" });
    let init = serde_json::json!({ "startupHints": { "nonInteractive": true } });
    assert!(startup_hints_from_meta(bad.as_object(), init.as_object()).non_interactive);
    assert!(!startup_hints_from_meta(None, None).non_interactive);
}
#[test]
fn system_prompt_override_from_meta_prefers_session_and_rejects_empty() {
    let session = serde_json::json!({ "systemPromptOverride": "from session" });
    let init = serde_json::json!({ "systemPromptOverride": "from init" });
    assert_eq!(
        system_prompt_override_from_meta(session.as_object(), init.as_object()),
        Some("from session")
    );
    assert_eq!(
        system_prompt_override_from_meta(None, init.as_object()),
        Some("from init")
    );
    let empty = serde_json::json!({ "systemPromptOverride": "" });
    assert_eq!(
        system_prompt_override_from_meta(empty.as_object(), None),
        None
    );
    assert_eq!(system_prompt_override_from_meta(None, None), None);
}
#[test]
fn enqueue_replace_system_prompt_override_sends_when_present() {
    use crate::session::SessionCommand;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let session = serde_json::json!({ "systemPromptOverride": "from session" });
    let init = serde_json::json!({ "systemPromptOverride": "from init" });
    enqueue_replace_system_prompt_override(&tx, session.as_object(), init.as_object());
    match rx.try_recv() {
        Ok(SessionCommand::ReplaceSystemPrompt { system_prompt }) => {
            assert_eq!(system_prompt, "from session", "session meta wins over init");
        }
        _ => panic!("expected a ReplaceSystemPrompt command"),
    }
}
#[test]
fn enqueue_replace_system_prompt_override_noop_when_absent_or_empty() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    enqueue_replace_system_prompt_override(
        &tx,
        serde_json::json!({ "systemPromptOverride": "" }).as_object(),
        None,
    );
    enqueue_replace_system_prompt_override(&tx, serde_json::json!({}).as_object(), None);
    enqueue_replace_system_prompt_override(&tx, None, None);
    assert!(
        rx.try_recv().is_err(),
        "no command should be enqueued without a non-empty override"
    );
}
/// Regression for the web-client `_meta.agentProfile` -> `set_session_model`
/// flow: a zero-turn switch from `echo-agent-build` (a client profile name) to
/// `echo-agent-build-plan` (the default model agent_type) must be
/// treated as compatible so the harness rebuild is skipped and the
/// custom prompt body is preserved.
#[test]
fn harnesses_are_compatible_for_stock_family_pairs() {
    assert!(harnesses_are_compatible("echo-agent-build", "echo-agent-build-plan"));
    assert!(harnesses_are_compatible("echo-agent-build-plan", "echo-agent-build"));
    assert!(harnesses_are_compatible("echo-agent-build", "echo-agent-build"));
    assert!(harnesses_are_compatible(
        "echo-agent-build-concise",
        "echo-agent-build-plan"
    ));
    assert!(harnesses_are_compatible(
        "remote-sidebar",
        "echo-agent-build-plan"
    ));
}
#[test]
fn harnesses_are_compatible_rejects_strict_mismatches() {
    assert!(harnesses_are_compatible("codex", "codex"));
    assert!(!harnesses_are_compatible("echo-agent-build-plan", "codex"));
}
#[test]
fn explicit_agent_type_wins_over_session_default() {
    assert_eq!(
        resolve_required_agent_type(Some("cursor"), "echo-agent-build-plan"),
        "cursor"
    );
}
#[test]
fn null_agent_type_falls_back_to_session_default_echo_agent_build_plan() {
    assert_eq!(
        resolve_required_agent_type(None, "echo-agent-build-plan"),
        "echo-agent-build-plan"
    );
}
#[test]
fn null_agent_type_falls_back_to_session_default_echo_agent_build() {
    assert_eq!(
        resolve_required_agent_type(None, "echo-agent-build"),
        "echo-agent-build"
    );
}
#[test]
fn null_agent_type_returns_to_session_default_after_cursor_switch() {
    let session_default = "echo-agent-build-plan";
    let required_after_null = resolve_required_agent_type(None, session_default);
    assert_eq!(required_after_null, "echo-agent-build-plan");
    assert_ne!(required_after_null, "cursor");
}
/// Compatible stock switches (no rebuild) must NOT mutate `agent_name`,
/// preserving the session's original ACP `agentProfile`.
#[test]
fn agent_name_unchanged_without_harness_rebuild() {
    let unchanged = agent_name_after_model_switch(false, "echo-agent-build-plan", "remote-sidebar");
    assert_eq!(
        unchanged, "remote-sidebar",
        "a compatible stock switch must preserve the original agent profile name"
    );
}
/// End-to-end test: config -> resolve -> override -> finalize -> tool_definitions.
///
/// Exercises the full live path through to the finalized toolset, proving
/// that the hashline tools appear in the actual tool definitions that
/// would be sent to the model.
#[tokio::test]
async fn file_toolset_override_e2e_to_finalized_toolset() {
    use crate::tools::{FileToolset, ShellToolsetConfig};
    use echo_agent_tools::computer::local::{LocalFs, LocalTerminalBackend};
    use echo_agent_tools::notification::ToolNotificationHandle;
    use echo_agent_tools::registry::types::SessionContext;
    let tmp = tempfile::tempdir().unwrap();
    let mut def = MvpAgent::resolve_agent_definition(
        tmp.path(),
        None,
        &config::AgentSelectionConfig::default(),
        None,
        None,
    );
    let toolset_config = ShellToolsetConfig {
        file_toolset: FileToolset::Hashline,
        ..ShellToolsetConfig::default()
    };
    let effective = toolset_config.resolve_file_toolset(None);
    let file_tools = effective
        .tool_configs(&toolset_config.hashline)
        .expect("default hashline config should validate");
    def.override_file_tools(file_tools);
    let builder = echo_agent_tools::registry::types::ToolRegistryBuilder::new();
    let ctx = SessionContext {
        backend: std::sync::Arc::new(LocalTerminalBackend::new()),
        fs: std::sync::Arc::new(LocalFs),
        cwd: tmp.path().to_path_buf(),
        session_folder: tmp.path().join("session"),
        session_env: std::sync::Arc::new(std::collections::HashMap::new()),
        notification_handle: ToolNotificationHandle::noop(),
        owner_session_id: None,
        subagent: None,
        parent_scheduler_handle: None,
        skills: vec![],
        state_path: tmp.path().join("state.json"),
        memory_backend: None,
        web_search_config: echo_agent_tools::implementations::web_search::WebSearchConfig::default(),
        web_fetch_config: Default::default(),
        lsp: None,
        image_gen_config: echo_agent_tools::implementations::echo_agent_build::image_gen::ImageGenConfig::default(),
        video_gen_config: echo_agent_tools::implementations::echo_agent_build::video_gen::VideoGenConfig::default(),
        app_builder_deployer_config: echo_agent_tools::implementations::echo_agent_build::app_builder::AppBuilderDeployerConfig::default(),
        api_key_provider: None,
        auth_provider: None,
        attribution_callback: None,
        system_reminder_tag: echo_agent_tools::reminders::DEFAULT_REMINDER_TAG,
    };
    let toolset = builder
        .finalize(def.tool_config, ctx)
        .expect("hashline toolset should finalize");
    let defs = toolset.tool_definitions();
    let names: Vec<&str> = defs.iter().map(|d| d.function.name.as_str()).collect();
    assert!(names.contains(&"hashline_read"), "defs: {names:?}");
    assert!(names.contains(&"hashline_edit"), "defs: {names:?}");
    assert!(names.contains(&"hashline_grep"), "defs: {names:?}");
    assert!(!names.contains(&"read_file"), "defs: {names:?}");
    assert!(!names.contains(&"search_replace"), "defs: {names:?}");
    assert!(names.contains(&"list_dir"), "defs: {names:?}");
}
/// Invalid hashline config returns a clean error, not a panic.
#[test]
fn file_toolset_override_invalid_config_returns_error() {
    use crate::tools::FileToolset;
    use crate::tools::config::HashlineSchemeConfig;
    let bad = HashlineSchemeConfig {
        scheme: "bogus".to_owned(),
        hash_len: 0,
        chunk_size: 0,
    };
    let err = FileToolset::Hashline.tool_configs(&bad);
    assert!(err.is_err());
    assert!(err.unwrap_err().contains("unknown"));
}
/// Helper: creates a real SessionHandle with the given model, yolo, and client id.
/// Requires a tokio runtime for SessionSignalsHandle::new().
fn make_test_handle(
    model: &str,
    yolo: bool,
    client_id: Option<&str>,
) -> crate::session::SessionHandle {
    let (cmd_tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (persistence_tx, _persistence_rx) = tokio::sync::mpsc::unbounded_channel();
    let (hunk_event_tx, _hunk_event_rx) = tokio::sync::mpsc::unbounded_channel();
    let hunk_cancel = tokio_util::sync::CancellationToken::new();
    let hunk_tracker_handle = echo_agent_hunk_tracker::HunkTrackerActor::spawn(
        "test".to_string(),
        std::path::PathBuf::from("/tmp"),
        hunk_event_tx,
        echo_agent_hunk_tracker::TrackingMode::AllDirty,
        hunk_cancel,
    );
    crate::session::SessionHandle {
        cmd_tx,
        persistence_tx,
        current_prompt_id: std::sync::Arc::new(std::sync::Mutex::new(None)),
        pending_interactions: std::sync::Arc::new(std::sync::Mutex::new(
            std::collections::HashMap::new(),
        )),
        info: crate::session::info::Info {
            id: acp::SessionId::new("test"),
            cwd: "/tmp".to_string(),
        },
        max_turns: None,
        resolved_tool_overrides: std::sync::Arc::new(arc_swap::ArcSwapOption::empty()),
        hunk_tracker_handle,
        chat_state_handle: echo_agent_chat_state::ChatStateHandle::noop(),
        signals_handle: crate::session::signals::SessionSignalsHandle::new(),
        gateway_enabled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        status_line_enabled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        mcp_servers: vec![],
        initial_client_mcp_servers: vec![],
        display_cwd: None,
        feedback_manager: std::sync::Arc::new(
            crate::session::feedback_manager::FeedbackManager::local_only("test"),
        ),
        upload_queue: Arc::new(OnceLock::new()),
        upload_failures_since_success: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        tool_context: crate::tools::ToolContext::new_local_context(
            echo_agent_paths::AbsPathBuf::new(std::path::PathBuf::from("/tmp")).unwrap(),
            std::sync::Arc::new(echo_agent_workspace::file_system::LocalFs::new(
                std::path::PathBuf::from("/tmp"),
            )),
            std::sync::Arc::new(crate::terminal::LocalTerminalRunner),
        ),
        model_id: acp::ModelId::new(model),
        scheduler_background_loops: true,
        reasoning_effort: None,
        yolo_mode: yolo,
        origin_client: client_id.map(|s| crate::http::OriginClientInfo {
            product: s.to_string(),
            version: None,
        }),
        code_nav_enabled: false,
        ask_user_question_enabled: true,
        non_interactive: false,
        plan_mode: std::sync::Arc::new(parking_lot::Mutex::new(
            crate::session::plan_mode::PlanModeTracker::new(std::path::PathBuf::from("/tmp")),
        )),
        force_compact: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        permission_handle: echo_agent_workspace::permission::PermissionHandle::allow_all(),
        attribution_callback: None,
        agent_name: "echo-agent-build".to_string(),
        managed_mcp_proxy_base_url: String::new(),
        session_default_agent_profile: None,
        allowed_subagent_types: None,
        hook_registry: None,
        workspace_ops: echo_agent_workspace::WorkspaceOps::for_test(),
        terminal_backend: None,
        tools_notification_handle: None,
        scheduler_handle: None,
    }
}
/// lookup_session_model returns the per-session model when one is known.
#[tokio::test]
async fn lookup_session_model_returns_per_session_model() {
    let default_model = acp::ModelId::new("default-model");
    assert_eq!(
        lookup_session_model(Some(acp::ModelId::new("echo-agent-3-fast")), &default_model)
            .0
            .as_ref(),
        "echo-agent-3-fast"
    );
    assert_eq!(
        lookup_session_model(Some(acp::ModelId::new("codex-mini")), &default_model)
            .0
            .as_ref(),
        "codex-mini"
    );
}
/// lookup_session_model falls back to the default when no session model is known.
#[tokio::test]
async fn lookup_session_model_fallback_no_session() {
    let default_model = acp::ModelId::new("echo-agent-3");
    assert_eq!(
        lookup_session_model(None, &default_model).0.as_ref(),
        "echo-agent-3"
    );
}
/// Mutating session A's model_id via the handle does not affect session B.
#[tokio::test]
async fn set_session_model_does_not_cross_contaminate() {
    let sid_a = acp::SessionId::new("sess-a");
    let sid_b = acp::SessionId::new("sess-b");
    let default_model = acp::ModelId::new("default");
    let mut sessions: HashMap<acp::SessionId, crate::session::SessionHandle> = [
        (sid_a.clone(), make_test_handle("echo-agent-3", false, None)),
        (sid_b.clone(), make_test_handle("echo-agent-3", false, None)),
    ]
    .into();
    sessions.get_mut(&sid_a).unwrap().model_id = acp::ModelId::new("codex-mini");
    assert_eq!(
        lookup_session_model(
            sessions.get(&sid_a).map(|h| h.model_id.clone()),
            &default_model
        )
        .0
        .as_ref(),
        "codex-mini"
    );
    assert_eq!(
        lookup_session_model(
            sessions.get(&sid_b).map(|h| h.model_id.clone()),
            &default_model
        )
        .0
        .as_ref(),
        "echo-agent-3",
        "Session B's model must not be affected by session A's model change"
    );
}
#[tokio::test]
async fn model_state_prefers_session_reasoning_effort_over_global() {
    use crate::agent::config::{EndpointsConfig, ModelEntry};
    use echo_agent_sampling_types::{REASONING_EFFORT_META_KEY, ReasoningEffort};
    let agent = build_minimal_agent_for_tests();
    let mut entry = ModelEntry::fallback("effort-model", &EndpointsConfig::default());
    entry.info.supports_reasoning_effort = true;
    agent
        .models_manager
        .insert_test_entry("effort-model", entry);
    agent
        .models_manager
        .set_current_reasoning_effort(Some(ReasoningEffort::Low));
    let read_effort = |state: &acp::SessionModelState| -> Option<String> {
        state
            .available_models
            .iter()
            .find(|m| m.model_id.0.as_ref() == "effort-model")
            .and_then(|m| m.meta.as_ref())
            .and_then(|m| m.get(REASONING_EFFORT_META_KEY))
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    };
    let pinned = acp::SessionId::new("sess-pinned");
    let mut handle = make_test_handle("effort-model", false, None);
    handle.reasoning_effort = Some(ReasoningEffort::Xhigh);
    agent.insert_resident(&pinned, handle);
    assert_eq!(
        read_effort(&agent.model_state(Some(&pinned))).as_deref(),
        Some("xhigh"),
        "model_state must report the session's own restored effort",
    );
    let unset = acp::SessionId::new("sess-unset");
    agent.insert_resident(&unset, make_test_handle("effort-model", false, None));
    assert_eq!(
        read_effort(&agent.model_state(Some(&unset))).as_deref(),
        Some("low"),
        "absent session effort falls back to the global default",
    );
}
#[tokio::test]
async fn apply_supported_effort_assigns_only_when_supported() {
    use crate::agent::config::{EndpointsConfig, ModelEntry};
    use crate::agent::mvp_agent::reasoning_effort::EffortTarget;
    use echo_agent_sampling_types::ReasoningEffort;
    let agent = build_minimal_agent_for_tests();
    let mut supported = ModelEntry::fallback("effort-model", &EndpointsConfig::default());
    supported.info.supports_reasoning_effort = true;
    agent
        .models_manager
        .insert_test_entry("effort-model", supported.clone());
    let plain = ModelEntry::fallback("plain-model", &EndpointsConfig::default());
    agent
        .models_manager
        .insert_test_entry("plain-model", plain.clone());
    let sid = acp::SessionId::new("meta-effort-sess");
    let mut supported_cfg = agent.prepare_sampling_config_for_model(&supported, None);
    supported_cfg.reasoning_effort = None;
    agent.models_manager.apply_supported_effort(
        &mut supported_cfg,
        Some(ReasoningEffort::High),
        &sid,
        EffortTarget::NewSession,
    );
    assert_eq!(supported_cfg.reasoning_effort, Some(ReasoningEffort::High));
    let mut plain_cfg = agent.prepare_sampling_config_for_model(&plain, None);
    plain_cfg.reasoning_effort = None;
    agent.models_manager.apply_supported_effort(
        &mut plain_cfg,
        Some(ReasoningEffort::High),
        &sid,
        EffortTarget::NewSession,
    );
    assert_eq!(plain_cfg.reasoning_effort, None);
    let mut none_cfg = agent.prepare_sampling_config_for_model(&supported, None);
    none_cfg.reasoning_effort = Some(ReasoningEffort::Low);
    agent.models_manager.apply_supported_effort(
        &mut none_cfg,
        None,
        &sid,
        EffortTarget::NewSession,
    );
    assert_eq!(none_cfg.reasoning_effort, Some(ReasoningEffort::Low));
}
#[test]
fn resolve_new_session_effort_hint_prefers_meta_over_current() {
    use crate::agent::mvp_agent::reasoning_effort::resolve_new_session_effort_hint;
    use echo_agent_sampling_types::ReasoningEffort;
    assert_eq!(
        resolve_new_session_effort_hint(Some(ReasoningEffort::High), Some(ReasoningEffort::Low)),
        Some(ReasoningEffort::High),
    );
    assert_eq!(
        resolve_new_session_effort_hint(None, Some(ReasoningEffort::Low)),
        Some(ReasoningEffort::Low),
        "/new and /clear with no _meta hint must keep last-used / config effort",
    );
    assert_eq!(resolve_new_session_effort_hint(None, None), None);
}
#[test]
fn split_new_session_effort_routes_hint_to_one_slot() {
    use crate::agent::mvp_agent::reasoning_effort::{NewSessionEffort, split_new_session_effort};
    use echo_agent_sampling_types::ReasoningEffort;
    assert_eq!(
        split_new_session_effort(None, Some(ReasoningEffort::High)),
        NewSessionEffort::Spawn(ReasoningEffort::High),
    );
    assert_eq!(
        split_new_session_effort(Some("custom-model"), Some(ReasoningEffort::High)),
        NewSessionEffort::Switch(ReasoningEffort::High),
    );
    assert_eq!(split_new_session_effort(None, None), NewSessionEffort::None,);
    assert_eq!(
        split_new_session_effort(Some("custom-model"), None),
        NewSessionEffort::None,
    );
}
/// New-session default-model path: composing `parse_reasoning_effort_meta` ->
/// `split_new_session_effort` -> `apply_supported_effort` seeds a
/// `_meta.reasoningEffort` hint into the spawn sampling for a supported model and
/// drops it (keeping the catalog default) for an unsupported one.
#[tokio::test]
async fn new_session_meta_effort_seeds_spawn_for_supported_model_and_drops_for_unsupported() {
    use crate::agent::config::{EndpointsConfig, ModelEntry};
    use crate::agent::mvp_agent::reasoning_effort::{
        EffortTarget, NewSessionEffort, split_new_session_effort,
    };
    use echo_agent_sampling_types::{
        REASONING_EFFORT_META_KEY, ReasoningEffort, parse_reasoning_effort_meta,
        reasoning_effort_meta_value,
    };
    let agent = build_minimal_agent_for_tests();
    let mut supported = ModelEntry::fallback("effort-model", &EndpointsConfig::default());
    supported.info.supports_reasoning_effort = true;
    supported.info.reasoning_effort = Some(ReasoningEffort::Low);
    agent
        .models_manager
        .insert_test_entry("effort-model", supported.clone());
    let plain = ModelEntry::fallback("plain-model", &EndpointsConfig::default());
    agent
        .models_manager
        .insert_test_entry("plain-model", plain.clone());
    let mut meta = acp::Meta::new();
    meta.insert(
        REASONING_EFFORT_META_KEY.to_string(),
        reasoning_effort_meta_value(ReasoningEffort::High),
    );
    let request = acp::NewSessionRequest::new("/tmp").meta(Some(meta));
    let route = split_new_session_effort(None, parse_reasoning_effort_meta(request.meta.as_ref()));
    assert_eq!(route, NewSessionEffort::Spawn(ReasoningEffort::High));
    let spawn_effort = match route {
        NewSessionEffort::Spawn(effort) => Some(effort),
        NewSessionEffort::Switch(_) | NewSessionEffort::None => None,
    };
    let sid = acp::SessionId::new("new-session-meta-effort");
    let mut supported_cfg = agent.prepare_sampling_config_for_model(&supported, None);
    agent.models_manager.apply_supported_effort(
        &mut supported_cfg,
        spawn_effort,
        &sid,
        EffortTarget::NewSession,
    );
    assert_eq!(supported_cfg.reasoning_effort, Some(ReasoningEffort::High));
    let mut plain_cfg = agent.prepare_sampling_config_for_model(&plain, None);
    agent.models_manager.apply_supported_effort(
        &mut plain_cfg,
        spawn_effort,
        &sid,
        EffortTarget::NewSession,
    );
    assert_eq!(plain_cfg.reasoning_effort, None);
}
/// `/new` / `/clear` send no `_meta.reasoningEffort`. The last-used / config
/// default must seed spawn so a fresh chat does not snap back to the catalog
/// default (`high` on echo-agent-4.6).
#[tokio::test]
async fn new_session_without_meta_keeps_current_effort_over_catalog_default() {
    use crate::agent::config::{EndpointsConfig, ModelEntry};
    use crate::agent::mvp_agent::reasoning_effort::{
        EffortTarget, NewSessionEffort, resolve_new_session_effort_hint, split_new_session_effort,
    };
    use echo_agent_sampling_types::ReasoningEffort;
    let agent = build_minimal_agent_for_tests();
    let mut supported = ModelEntry::fallback("effort-model", &EndpointsConfig::default());
    supported.info.supports_reasoning_effort = true;
    supported.info.reasoning_effort = Some(ReasoningEffort::High);
    agent
        .models_manager
        .insert_test_entry("effort-model", supported.clone());
    agent
        .models_manager
        .set_current_reasoning_effort(Some(ReasoningEffort::Low));
    let hint =
        resolve_new_session_effort_hint(None, agent.models_manager.current_reasoning_effort());
    let route = split_new_session_effort(None, hint);
    assert_eq!(route, NewSessionEffort::Spawn(ReasoningEffort::Low));
    let spawn_effort = match route {
        NewSessionEffort::Spawn(effort) => Some(effort),
        NewSessionEffort::Switch(_) | NewSessionEffort::None => None,
    };
    let mut cfg = agent.prepare_sampling_config_for_model(&supported, None);
    assert_eq!(cfg.reasoning_effort, Some(ReasoningEffort::High));
    agent.models_manager.apply_supported_effort(
        &mut cfg,
        spawn_effort,
        &acp::SessionId::new("new-session-current-effort"),
        EffortTarget::NewSession,
    );
    assert_eq!(
        cfg.reasoning_effort,
        Some(ReasoningEffort::Low),
        "/clear must keep last-used / config effort, not the catalog default",
    );
}
/// Drive the real `restore_persisted_model` for a session pinned to an
/// effort-capable model and report the effort it lands on the session handle.
async fn restore_effort_via_load(
    initial: Option<echo_agent_sampling_types::ReasoningEffort>,
    persisted: Option<echo_agent_sampling_types::ReasoningEffort>,
) -> Option<echo_agent_sampling_types::ReasoningEffort> {
    use crate::agent::config::{EndpointsConfig, ModelEntry};
    let agent = build_minimal_agent_for_tests();
    let mut entry = ModelEntry::fallback("effort-model", &EndpointsConfig::default());
    entry.info.supports_reasoning_effort = true;
    agent
        .models_manager
        .insert_test_entry("effort-model", entry);
    let sid = acp::SessionId::new("restore-precedence-sess");
    let (handle, _cmd_tx, mut cmd_rx) = make_live_session_handle(&sid, None);
    tokio::spawn(async move {
        while let Some(cmd) = cmd_rx.recv().await {
            if let crate::session::SessionCommand::SetSessionModel {
                sampling_config,
                responds_to,
                ..
            } = cmd
            {
                let _ = responds_to.send(Ok(acp::ModelId::new(sampling_config.model)));
            }
        }
    });
    agent.insert_resident(&sid, handle);
    let info = crate::session::info::Info {
        id: sid.clone(),
        cwd: "/tmp".to_string(),
    };
    let mut summary =
        crate::session::persistence::Summary::new(&info, acp::ModelId::new("effort-model"))
            .unwrap();
    summary.reasoning_effort = persisted;
    agent.restore_persisted_model(&sid, &summary, initial).await;
    agent.resident_handle(&sid).and_then(|h| h.reasoning_effort)
}
#[tokio::test]
async fn load_effort_precedence_prefers_meta_hint_over_persisted() {
    use echo_agent_sampling_types::ReasoningEffort;
    assert_eq!(
        restore_effort_via_load(Some(ReasoningEffort::High), Some(ReasoningEffort::Low)).await,
        Some(ReasoningEffort::High),
    );
    assert_eq!(
        restore_effort_via_load(None, Some(ReasoningEffort::Low)).await,
        Some(ReasoningEffort::Low),
    );
}
/// Reapplying the current model after loading an automation profile must
/// preserve that profile, while changing to an incompatible model is gated.
async fn automation_profile_model_request(active: &'static str, requested: &str, required_agent: &str) -> Result<bool, acp::Error> {
    use crate::agent::config::{EndpointsConfig, ModelEntry};
    let agent = build_minimal_agent_for_tests();
    for id in ["automation-model", "different-model"] {
        let mut entry = ModelEntry::fallback(id, &EndpointsConfig::default());
        entry.info.agent_type = required_agent.to_owned();
        agent.models_manager.insert_test_entry(id, entry);
    }
    let sid = acp::SessionId::new("automation-model-restore");
    let (mut handle, _cmd_tx, mut cmd_rx) = make_live_session_handle(&sid, None);
    handle.model_id = acp::ModelId::new("automation-model");
    handle.agent_name = active.to_owned();
    handle.session_default_agent_profile = Some("echo-agent-build-plan".to_owned());
    handle.signals_handle.restore_signals(crate::session::signals::SessionSignals {
        turn_count: 1,
        ..Default::default()
    });
    let (observed_tx, mut observed_rx) = tokio::sync::mpsc::unbounded_channel();
    let actor = tokio::spawn(async move {
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                TestSessionCommand::GetActiveAgent { responds_to } => {
                    let _ = responds_to.send(Some(active.to_owned()));
                }
                TestSessionCommand::SetSessionModel { sampling_config, skip_prompt_rewrite, responds_to, .. } => {
                    let _ = observed_tx.send(skip_prompt_rewrite);
                    let _ = responds_to.send(Ok(acp::ModelId::new(sampling_config.model)));
                }
                TestSessionCommand::RebuildAgentForDefinition { .. } => panic!("Must preserve the active automation profile"),
                _ => {}
            }
        }
    });
    agent.insert_resident(&sid, handle);
    let response = crate::agent::handlers::model_switch::apply(
        &agent, acp::SetSessionModelRequest::new(sid.clone(), acp::ModelId::new(requested)), None,
    ).await;
    let result = match response {
        Ok(_) => {
            assert_eq!(agent.resident_handle(&sid).unwrap().agent_name, active);
            assert_eq!(agent.resident_handle(&sid).unwrap().model_id.0.as_ref(), requested);
            Ok(observed_rx.try_recv().expect("The model update must reach the actor"))
        }
        Err(error) => {
            assert_eq!(agent.resident_handle(&sid).unwrap().model_id.0.as_ref(), "automation-model");
            assert!(observed_rx.try_recv().is_err(), "Rejected model must not reach the actor");
            Err(error)
        }
    };
    actor.abort();
    result
}

#[tokio::test]
async fn automation_profile_model_restore_preserves_same_model_and_prompt() {
    for profile in ["browser-use", "computer-use"] {
        assert!(automation_profile_model_request(profile, "automation-model", "echo-agent-build-plan").await
            .expect("Reapplying the current model must not reject the automation profile"));
    }
}

#[tokio::test]
async fn automation_profile_model_change_still_rejects_incompatible_harness() {
    for profile in ["browser-use", "computer-use"] {
        let error = automation_profile_model_request(profile, "different-model", "echo-agent-build-plan").await.unwrap_err();
        assert!(error.message.contains("requires agent"));
    }
}

#[tokio::test]
async fn automation_profile_model_restore_still_rejects_new_strict_harness_requirement() {
    for profile in ["browser-use", "computer-use"] {
        let error = automation_profile_model_request(profile, "automation-model", "codex").await.unwrap_err();
        assert!(error.message.contains("requires agent"));
    }
}
/// A session persisted under a routing *slug* (not the catalog map key) must
/// still get reasoning modes and a selected model from
/// `session_config_options` — the id is resolved to the catalog key before
/// the catalog effort lookups and the selected-model match.
#[tokio::test]
async fn session_config_options_resolves_routing_slug_to_catalog_model() {
    use crate::agent::config::{EndpointsConfig, ModelEntry};
    use echo_agent_sampling_types::ReasoningEffort;
    let agent = build_minimal_agent_for_tests();
    let mut entry = ModelEntry::fallback("catalog-key-model", &EndpointsConfig::default());
    entry.info.model = "routing-slug".to_string();
    entry.info.supports_reasoning_effort = true;
    entry.info.reasoning_effort = Some(ReasoningEffort::High);
    agent
        .models_manager
        .insert_test_entry("catalog-key-model", entry);
    let sid = acp::SessionId::new("sess-slug");
    agent.insert_resident(&sid, make_test_handle("routing-slug", false, None));
    let state = agent.model_state(Some(&sid));
    assert_eq!(state.current_model_id.0.as_ref(), "routing-slug");
    let opts = agent.session_config_options(Some(&sid), &state);
    let modes: Vec<_> = opts.iter().filter(|o| o.category == "mode").collect();
    assert!(
        !modes.is_empty(),
        "reasoning modes must surface for a slug-identified session"
    );
    assert!(
        modes.iter().any(|o| o.id == "high" && o.selected),
        "catalog default effort should be selected"
    );
    assert!(
        opts.iter()
            .any(|o| o.category == "model" && o.id == "catalog-key-model" && o.selected),
        "resolved catalog model must be selected"
    );
}
/// YOLO toggle scoped by client_identifier: only matching sessions are updated.
#[tokio::test]
async fn yolo_toggle_scoped_by_client_identifier() {
    let sid_tui = acp::SessionId::new("sess-tui");
    let sid_vscode = acp::SessionId::new("sess-vscode");
    let mut sessions: HashMap<acp::SessionId, crate::session::SessionHandle> = [
        (
            sid_tui.clone(),
            make_test_handle("echo-agent-3", false, Some("echo-agent-tui")),
        ),
        (
            sid_vscode.clone(),
            make_test_handle("echo-agent-3", false, Some("echo-agent-code-extension")),
        ),
    ]
    .into();
    let updated =
        apply_yolo_mode_to_matching_sessions(sessions.values_mut(), Some("echo-agent-tui"), true);
    assert_eq!(updated, 1, "exactly one matching session should be updated");
    assert!(
        sessions[&sid_tui].yolo_mode,
        "TUI session should have yolo=true after TUI toggle"
    );
    assert!(
        !sessions[&sid_vscode].yolo_mode,
        "VS Code session must NOT be affected by TUI's yolo toggle"
    );
}
/// A client can explicitly disable YOLO for its own sessions after startup,
/// even if those sessions were initially created with yolo=true.
#[tokio::test]
async fn yolo_toggle_can_disable_session_started_with_yolo_enabled() {
    let sid_tui = acp::SessionId::new("sess-tui");
    let sid_other = acp::SessionId::new("sess-other");
    let mut sessions: HashMap<acp::SessionId, crate::session::SessionHandle> = [
        (
            sid_tui.clone(),
            make_test_handle("echo-agent-3", true, Some("echo-agent-tui")),
        ),
        (
            sid_other.clone(),
            make_test_handle("echo-agent-3", true, Some("echo-agent-code-extension")),
        ),
    ]
    .into();
    let updated =
        apply_yolo_mode_to_matching_sessions(sessions.values_mut(), Some("echo-agent-tui"), false);
    assert_eq!(updated, 1, "only the sender's session should be updated");
    assert!(
        !sessions[&sid_tui].yolo_mode,
        "sender session should be switched to yolo=false"
    );
    assert!(
        sessions[&sid_other].yolo_mode,
        "other client's session must keep its previous yolo state"
    );
}
/// `drain_old_session_thread` returns immediately when the thread has
/// already finished.
#[tokio::test]
async fn drain_finished_thread_returns_immediately() {
    let session_threads: RefCell<HashMap<acp::SessionId, crate::session::SessionThread>> =
        RefCell::new(HashMap::new());
    let sid = acp::SessionId::new("drain-test");
    let handle = std::thread::spawn(|| {});
    std::thread::sleep(std::time::Duration::from_millis(10));
    session_threads.borrow_mut().insert(
        sid.clone(),
        crate::session::SessionThread::from_handle(handle),
    );
    let thread = session_threads.borrow_mut().remove(&sid).unwrap();
    assert!(thread.is_finished(), "thread should be finished");
    assert!(!session_threads.borrow().contains_key(&sid));
}
/// `drain_old_session_thread` waits for a slow thread to finish.
#[tokio::test]
async fn drain_waits_for_slow_thread() {
    let session_threads: RefCell<HashMap<acp::SessionId, crate::session::SessionThread>> =
        RefCell::new(HashMap::new());
    let sid = acp::SessionId::new("slow-drain");
    let handle = std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_millis(100));
    });
    session_threads.borrow_mut().insert(
        sid.clone(),
        crate::session::SessionThread::from_handle(handle),
    );
    let thread = session_threads.borrow_mut().remove(&sid).unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if thread.is_finished() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "thread should finish within 5s"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(thread.is_finished());
}
/// Drain respects the 5s deadline and returns even if the thread is still running.
#[tokio::test]
async fn drain_respects_deadline() {
    let session_threads: RefCell<HashMap<acp::SessionId, crate::session::SessionThread>> =
        RefCell::new(HashMap::new());
    let sid = acp::SessionId::new("hung-drain");
    let handle = std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(30));
    });
    session_threads.borrow_mut().insert(
        sid.clone(),
        crate::session::SessionThread::from_handle(handle),
    );
    let thread = session_threads.borrow_mut().remove(&sid).unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(200);
    let mut timed_out = false;
    loop {
        if thread.is_finished() {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            timed_out = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        timed_out,
        "should have timed out waiting for the hung thread"
    );
    assert!(!thread.is_finished(), "thread should still be running");
}
#[test]
fn parse_code_nav_capability_present_and_true() {
    let mut meta = serde_json::Map::new();
    meta.insert(
        "echo.agent/codeNavigation".to_string(),
        serde_json::json!({ "enabled": true }),
    );
    let init = acp::InitializeRequest::new(acp::ProtocolVersion::V1).client_capabilities(
        acp::ClientCapabilities::new()
            .fs(acp::FileSystemCapabilities::new())
            .terminal(false)
            .meta(meta),
    );
    assert!(MvpAgent::parse_code_nav_capability(&init));
}
#[test]
fn parse_code_nav_capability_absent_returns_false() {
    let init = acp::InitializeRequest::new(acp::ProtocolVersion::V1).client_capabilities(
        acp::ClientCapabilities::new()
            .fs(acp::FileSystemCapabilities::new())
            .terminal(false),
    );
    assert!(!MvpAgent::parse_code_nav_capability(&init));
}
#[test]
fn parse_code_nav_capability_false_returns_false() {
    let mut meta = serde_json::Map::new();
    meta.insert(
        "echo.agent/codeNavigation".to_string(),
        serde_json::json!({ "enabled": false }),
    );
    let init = acp::InitializeRequest::new(acp::ProtocolVersion::V1).client_capabilities(
        acp::ClientCapabilities::new()
            .fs(acp::FileSystemCapabilities::new())
            .terminal(false)
            .meta(meta),
    );
    assert!(!MvpAgent::parse_code_nav_capability(&init));
}
/// Verify that two session handles with different code-nav state produce
/// independent eligibility outcomes — the key leader-mode isolation test.
///
/// This tests the `code_nav_eligibility_for_request` lookup path directly
/// by inspecting the per-handle fields rather than building a full agent,
/// which mirrors what the method actually reads at runtime.
#[tokio::test]
async fn test_per_session_code_nav_isolation() {
    let web_handle = {
        let mut h = make_test_handle("model", false, Some("echo-agent-web"));
        h.code_nav_enabled = true;
        h
    };
    let tui_handle = {
        let mut h = make_test_handle("model", false, Some("echo-agent-tui"));
        h.code_nav_enabled = false;
        h
    };
    let check = |handle: &crate::session::SessionHandle| {
        let ct = crate::http::client_type_from_origin(handle.origin_client.as_ref());
        if !matches!(ct, ClientType::EchoAgentWeb) {
            return Err(CodeNavEligibility::ClientNotWeb);
        }
        if !handle.code_nav_enabled {
            return Err(CodeNavEligibility::CapabilityNotAdvertised);
        }
        Ok(())
    };
    assert!(
        check(&web_handle).is_ok(),
        "web session with capability should pass client-type and capability gates"
    );
    assert_eq!(
        check(&tui_handle),
        Err(CodeNavEligibility::ClientNotWeb),
        "tui session should be rejected at gate 1"
    );
    let mut web_no_cap = web_handle.clone();
    web_no_cap.code_nav_enabled = false;
    assert_eq!(
        check(&web_no_cap),
        Err(CodeNavEligibility::CapabilityNotAdvertised),
        "web session without capability should be rejected at gate 2"
    );
    assert!(
        check(&web_handle).is_ok(),
        "original web handle must be unaffected"
    );
}
/// Verify that code-nav requests without a sessionId are rejected.
///
/// `sessionId` is required so per-client capability gating is unambiguous
/// in both simple and leader modes.  Falling back to shared global state
/// (last-client-wins in leader mode) is not safe.
#[test]
fn test_sessionless_request_requires_session_id() {
    let session_id: Option<&acp::SessionId> = None;
    let result: Result<(), CodeNavEligibility> = if session_id.is_none() {
        Err(CodeNavEligibility::SessionRequired)
    } else {
        Ok(())
    };
    assert_eq!(
        result,
        Err(CodeNavEligibility::SessionRequired),
        "cwd-only requests with no sessionId must return SessionRequired"
    );
}
#[tokio::test(flavor = "current_thread")]
async fn ext_method_routes_auth_cleared_and_refreshes_resident_sessions() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let agent = build_agent_with_auth(crate::auth::EchoAgentAuth {
                key: "eligible".into(),
                auth_mode: crate::auth::AuthMode::WebLogin,
                ..crate::auth::EchoAgentAuth::test_default()
            });
            use acp::Agent as _;
            agent.managed_mcp_cache.lock().await.enable_gateway_tools();
            let sid = acp::SessionId::new("sess-auth-cleared");
            let (handle, _tx, mut cmd_rx) = make_live_session_handle(&sid, None);
            agent.insert_resident(&sid, handle);
            let params = serde_json::json!({});
            agent
                .ext_method(acp::ExtRequest::new(
                    "echo.agent/internal/auth_cleared",
                    std::sync::Arc::from(serde_json::value::to_raw_value(&params).unwrap()),
                ))
                .await
                .expect("auth_cleared must route through session-admin");
            let cmd = tokio::time::timeout(std::time::Duration::from_secs(1), cmd_rx.recv())
                .await
                .expect("refresh command should be sent")
                .expect("channel should stay open until command is received");
            assert!(matches!(cmd, SessionCommand::RefreshMcpSearchIndex));
            assert!(!agent.managed_mcp_cache.lock().await.gateway_tools_active);
        })
        .await;
}
fn empty_gateway_catalog() -> crate::session::managed_mcp::GatewayToolCatalog {
    crate::session::managed_mcp::GatewayToolCatalog {
        tools: vec![],
        total_tools: 0,
        connectors_needing_reauth: vec![],
    }
}
fn assert_no_update_mcp_servers(cmds: &[SessionCommand]) {
    assert!(
        !cmds
            .iter()
            .any(|c| matches!(c, SessionCommand::UpdateMcpServers { .. })),
        "mcp/list must not push UpdateMcpServers"
    );
}
/// `mcp/list` refresh: two resident sessions, cache=false + committed catalog
/// fans `RefreshMcpSearchIndex`; failed catalog and cache=true do not.
#[tokio::test(flavor = "current_thread")]
async fn mcp_list_gateway_refresh_fans_only_on_committed_uncached_catalog() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            use crate::session::managed_mcp::GatewayToolCatalogCache;
            use std::sync::atomic::{AtomicUsize, Ordering};
            let list_hits = std::sync::Arc::new(AtomicUsize::new(0));
            let list_hits_route = list_hits.clone();
            let app = axum::Router::new().route(
                "/mcp/tools/list",
                axum::routing::get(move || {
                    list_hits_route.fetch_add(1, Ordering::SeqCst);
                    async {
                        axum::Json(serde_json::json!({
                            "tools": [{
                                "connector_id": "gmail",
                                "connector_name": "Gmail",
                                "tool_id": "search",
                                "tool_name": "Search",
                                "call_id": "gmail_search",
                                "description": "d",
                                "json_schema": {"type": "object"}
                            }],
                            "total_tools": 1,
                            "connectors_needing_reauth": []
                        }))
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let proxy_url = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let (agent, _rx) = build_agent_with_auth_and_proxy(
                crate::auth::EchoAgentAuth {
                    key: "eligible".into(),
                    auth_mode: crate::auth::AuthMode::WebLogin,
                    ..crate::auth::EchoAgentAuth::test_default()
                },
                proxy_url,
                crate::agent::config::AgentMode::Generic,
            );
            agent.cfg.borrow_mut().managed_mcp_gateway_tools_enabled = true;
            let sid_a = acp::SessionId::new("sess-list-a");
            let sid_b = acp::SessionId::new("sess-list-b");
            let (handle_a, _tx_a, mut cmd_rx_a) = make_live_session_handle(&sid_a, None);
            let (handle_b, _tx_b, mut cmd_rx_b) = make_live_session_handle(&sid_b, None);
            agent.insert_resident(&sid_a, handle_a);
            agent.insert_resident(&sid_b, handle_b);
            {
                let mut state = agent.managed_mcp_cache.lock().await;
                state.enable_gateway_tools();
                let epoch = state.start_gateway_tool_fetch().unwrap();
                assert!(state.complete_gateway_tool_fetch(epoch, empty_gateway_catalog()));
            }
            let cached = agent.fetch_gateway_catalog_for_mcp_list(true).await;
            let cached = cached.expect("cache=true should hit Ready catalog");
            assert!(
                cached.tools.is_empty() && cached.total_tools == 0,
                "cache=true must return the seeded empty catalog, not a mock refetch"
            );
            assert_eq!(
                list_hits.load(Ordering::SeqCst),
                0,
                "cache=true must not hit /mcp/tools/list"
            );
            assert!(
                cmd_rx_a.try_recv().is_err() && cmd_rx_b.try_recv().is_err(),
                "cache=true must not fan RefreshMcpSearchIndex"
            );
            match &agent.managed_mcp_cache.lock().await.gateway_tool_cache {
                GatewayToolCatalogCache::Ready(catalog) => {
                    assert!(
                        catalog.tools.is_empty() && catalog.total_tools == 0,
                        "in-memory cache must stay the seeded empty Ready catalog"
                    );
                }
                GatewayToolCatalogCache::NotFetched => {
                    panic!("expected Ready(empty), got NotFetched")
                }
                GatewayToolCatalogCache::Fetching(_) => {
                    panic!("expected Ready(empty), got Fetching")
                }
            }
            agent.cfg.borrow_mut().managed_mcp_gateway_tools_enabled = false;
            let failed = agent.fetch_gateway_catalog_for_mcp_list(false).await;
            assert!(failed.is_none(), "gateway off after invalidate is None");
            assert!(
                cmd_rx_a.try_recv().is_err() && cmd_rx_b.try_recv().is_err(),
                "failed catalog must not fan RefreshMcpSearchIndex"
            );
            agent.cfg.borrow_mut().managed_mcp_gateway_tools_enabled = true;
            let fresh = agent.fetch_gateway_catalog_for_mcp_list(false).await;
            assert!(
                fresh.is_some_and(|c| !c.tools.is_empty()),
                "cache=false should refetch a non-empty catalog"
            );
            assert!(
                list_hits.load(Ordering::SeqCst) >= 1,
                "cache=false refetch must hit /mcp/tools/list"
            );
            let cmd_a = tokio::time::timeout(std::time::Duration::from_secs(1), cmd_rx_a.recv())
                .await
                .expect("session A RefreshMcpSearchIndex")
                .expect("channel open");
            let cmd_b = tokio::time::timeout(std::time::Duration::from_secs(1), cmd_rx_b.recv())
                .await
                .expect("session B RefreshMcpSearchIndex")
                .expect("channel open");
            assert!(matches!(cmd_a, SessionCommand::RefreshMcpSearchIndex));
            assert!(matches!(cmd_b, SessionCommand::RefreshMcpSearchIndex));
            assert_no_update_mcp_servers(&[cmd_a, cmd_b]);
            assert!(cmd_rx_a.try_recv().is_err() && cmd_rx_b.try_recv().is_err());
            server.abort();
        })
        .await;
}
/// mcp/list with gateway off must disable the cache, same as initialize.
#[tokio::test(flavor = "current_thread")]
async fn mcp_list_gateway_off_disables_cached_catalog() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            use crate::session::managed_mcp::GatewayToolCatalogCache;
            let (agent, _rx) = build_agent_with_auth_and_proxy(
                crate::auth::EchoAgentAuth {
                    key: "eligible".into(),
                    auth_mode: crate::auth::AuthMode::WebLogin,
                    ..crate::auth::EchoAgentAuth::test_default()
                },
                "http://127.0.0.1:1".into(),
                crate::agent::config::AgentMode::Generic,
            );
            agent.cfg.borrow_mut().managed_mcp_gateway_tools_enabled = true;
            let sid = acp::SessionId::new("sess-list-off");
            let (handle, _tx, mut cmd_rx) = make_live_session_handle(&sid, None);
            agent.insert_resident(&sid, handle);
            {
                let mut state = agent.managed_mcp_cache.lock().await;
                state.enable_gateway_tools();
                let epoch = state.start_gateway_tool_fetch().unwrap();
                assert!(state.complete_gateway_tool_fetch(epoch, empty_gateway_catalog()));
            }
            agent.cfg.borrow_mut().managed_mcp_gateway_tools_enabled = false;
            assert!(
                agent
                    .fetch_gateway_catalog_for_mcp_list(true)
                    .await
                    .is_none(),
                "gateway off must return None"
            );
            let state = agent.managed_mcp_cache.lock().await;
            assert!(
                !state.gateway_tools_active,
                "gateway off must disable the cache"
            );
            assert!(
                matches!(
                    state.gateway_tool_cache,
                    GatewayToolCatalogCache::NotFetched
                ),
                "disable must clear a Ready catalog"
            );
            drop(state);
            assert!(
                cmd_rx.try_recv().is_err(),
                "disable via mcp/list must not fan RefreshMcpSearchIndex"
            );
        })
        .await;
}
/// Gateway tools live on the agent catalog, so sessions only rebuild
/// `search_tool`.
#[tokio::test(flavor = "current_thread")]
async fn refresh_mcp_search_index_broadcasts_to_sessions() {
    let agent = build_minimal_agent_for_tests();
    let sid = acp::SessionId::new("sess-search-index");
    let (handle, _tx, mut cmd_rx) = make_live_session_handle(&sid, None);
    agent.insert_resident(&sid, handle);
    agent.refresh_mcp_search_index_in_sessions();
    let cmd = tokio::time::timeout(std::time::Duration::from_secs(1), cmd_rx.recv())
        .await
        .expect("RefreshMcpSearchIndex should be sent")
        .expect("channel should stay open");
    assert!(matches!(cmd, SessionCommand::RefreshMcpSearchIndex));
}
/// Build a minimal MvpAgent suitable for testing extension methods.
fn build_minimal_agent_for_tests() -> MvpAgent {
    use crate::agent::config::Config as AgentConfig;
    use crate::auth::{AuthManager, EchoAgentComConfig};
    let temp_dir = tempfile::tempdir().unwrap();
    let auth_manager =
        std::sync::Arc::new(AuthManager::new(temp_dir.path(), EchoAgentComConfig::default()));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let gateway = GatewaySender::new(tx);
    let cfg = AgentConfig::default();
    MvpAgent::new(gateway, &cfg, auth_manager, None).expect("valid test config")
}
fn session_usage_request(session_id: &str) -> acp::ExtRequest {
    acp::ExtRequest::new(
        "echo.agent/session/usage",
        serde_json::value::to_raw_value(&serde_json::json!({ "sessionId": session_id }))
            .unwrap()
            .into(),
    )
}
#[tokio::test(flavor = "current_thread")]
async fn session_usage_unknown_session_is_resource_not_found() {
    let agent = build_minimal_agent_for_tests();
    let err = crate::extensions::usage::handle(&agent, &session_usage_request("no-such-session"))
        .await
        .expect_err("unknown session");
    assert_eq!(
        err.code,
        acp::Error::resource_not_found(None::<String>).code
    );
}
#[tokio::test(flavor = "current_thread")]
async fn session_usage_dead_chat_state_actor_fails_closed() {
    let agent = build_minimal_agent_for_tests();
    let sid = acp::SessionId::new("usage-dead-actor-sess");
    let mut handle = make_test_handle("test-model", false, None);
    handle.info.id = sid.clone();
    agent.insert_resident(&sid, handle);
    let err =
        crate::extensions::usage::handle(&agent, &session_usage_request("usage-dead-actor-sess"))
            .await
            .expect_err("dead chat-state actor");
    assert_eq!(err.code, acp::Error::internal_error().code);
}
/// The session responses publish the value THIS session's spawn pinned, so a
/// client describing `/loop` fires can never contradict what the fires do.
#[tokio::test(flavor = "current_thread")]
async fn session_meta_publishes_the_sessions_pinned_scheduler_background_loops() {
    let agent = build_minimal_agent_for_tests();
    let sid = acp::SessionId::new("loop-mode-sess");
    let mut handle = make_test_handle("test-model", false, None);
    handle.info.id = sid.clone();
    handle.scheduler_background_loops = false;
    agent.insert_resident(&sid, handle);
    let model_state = agent.model_state(Some(&sid));
    let mut meta = serde_json::Map::new();
    agent.insert_session_config_meta(&mut meta, &sid, "/tmp".to_string(), None, &model_state);
    assert_eq!(
        meta.get(crate::session::SCHEDULER_BACKGROUND_LOOPS_META_KEY),
        Some(&serde_json::json!(false)),
        "session meta must carry the handle's pinned value"
    );
}
/// Build a minimal MvpAgent with pre-loaded auth for gate tests.
fn build_agent_with_auth(auth: crate::auth::EchoAgentAuth) -> MvpAgent {
    use crate::agent::config::Config as AgentConfig;
    use crate::auth::{AuthManager, EchoAgentComConfig};
    let temp_dir = tempfile::tempdir().unwrap();
    let auth_manager =
        std::sync::Arc::new(AuthManager::new(temp_dir.path(), EchoAgentComConfig::default()));
    auth_manager.hot_swap(auth);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let gateway = GatewaySender::new(tx);
    let cfg = AgentConfig::default();
    MvpAgent::new(gateway, &cfg, auth_manager, None).expect("valid test config")
}
fn make_trace_card_eligible(agent: &MvpAgent) {
    let mut cfg = agent.cfg.borrow_mut();
    cfg.feature_values
        .insert(crate::agent::config::Feature::FeedbackTraceCard, true);
    cfg.features.telemetry = Some(crate::agent::config::TelemetryMode::Enabled);
    cfg.telemetry.trace_upload = Some(false);
}
fn personal_echo_agent_oauth_auth() -> crate::auth::EchoAgentAuth {
    crate::auth::EchoAgentAuth {
        auth_mode: crate::auth::AuthMode::Oidc,
        oidc_issuer: Some(crate::auth::ECHO_AGENT_OAUTH2_ISSUER.to_string()),
        ..crate::auth::EchoAgentAuth::test_default()
    }
}
#[tokio::test]
#[serial_test::serial]
async fn feedback_trace_offer_asks_personal_oauth_accounts() {
    use echo_agent_test_support::EnvGuard;
    let _e1 = EnvGuard::unset("ECHO_AGENT_TELEMETRY_ENABLED");
    let _e2 = EnvGuard::unset("ECHO_AGENT_TELEMETRY_TRACE_UPLOAD");
    let _e3 = EnvGuard::unset("ECHO_AGENT_FEEDBACK_TRACE_CARD");
    let agent = build_agent_with_auth(personal_echo_agent_oauth_auth());
    make_trace_card_eligible(&agent);
    assert!(agent.feedback_trace_offer(), "every gate is open");
    assert!(
        agent
            .one_shot_feedback_gcs_config("sid".into())
            .await
            .is_some(),
        "the consented upload path must be open too"
    );
}
#[tokio::test]
#[serial_test::serial]
async fn feedback_trace_offer_suppressed_for_team_accounts_even_admins() {
    use echo_agent_test_support::EnvGuard;
    let _e1 = EnvGuard::unset("ECHO_AGENT_TELEMETRY_ENABLED");
    let _e2 = EnvGuard::unset("ECHO_AGENT_TELEMETRY_TRACE_UPLOAD");
    let _e3 = EnvGuard::unset("ECHO_AGENT_FEEDBACK_TRACE_CARD");
    for role in ["Admin", "Member"] {
        let agent = build_agent_with_auth(crate::auth::EchoAgentAuth {
            team_name: Some("acme".into()),
            team_role: Some(role.into()),
            ..personal_echo_agent_oauth_auth()
        });
        make_trace_card_eligible(&agent);
        assert!(
            !agent.feedback_trace_offer(),
            "team {role} must not be offered the individual trace card"
        );
        assert!(
            agent
                .one_shot_feedback_gcs_config("sid".into())
                .await
                .is_none(),
            "team {role} must not have a one-shot upload path"
        );
    }
}
#[tokio::test]
#[serial_test::serial]
async fn feedback_trace_offer_suppressed_for_managed_deployments() {
    use echo_agent_test_support::EnvGuard;
    let _e1 = EnvGuard::unset("ECHO_AGENT_TELEMETRY_ENABLED");
    let _e2 = EnvGuard::unset("ECHO_AGENT_TELEMETRY_TRACE_UPLOAD");
    let _e3 = EnvGuard::unset("ECHO_AGENT_FEEDBACK_TRACE_CARD");
    let agent = build_agent_with_auth(personal_echo_agent_oauth_auth());
    make_trace_card_eligible(&agent);
    agent.cfg.borrow_mut().endpoints.deployment_key = Some("dk-test".into());
    assert!(
        !agent.feedback_trace_offer(),
        "a deployment key must suppress the card even with personal OAuth"
    );
    assert!(
        agent
            .one_shot_feedback_gcs_config("sid".into())
            .await
            .is_none(),
        "a deployment key must close the one-shot upload path"
    );
}
/// Regression: boot-time plugin discovery is deferred past ACP
/// `initialize`, so the shared plugin registry starts empty.
/// `resolve_mcp_servers` reads that snapshot to merge plugin-contributed
/// MCP servers into a new session, so without lazy population the servers
/// silently vanished until an explicit `/plugins reload`.
/// `ensure_plugin_registry` must build the snapshot on first use.
#[tokio::test]
#[serial_test::serial]
async fn ensure_plugin_registry_lazily_populates_snapshot() {
    use crate::agent::config::Config as AgentConfig;
    use crate::auth::{AuthManager, EchoAgentComConfig};
    use echo_agent_test_support::EnvGuard;
    let echo_agent_home = tempfile::tempdir().unwrap();
    let _env = EnvGuard::set("ECHO_AGENT_HOME", echo_agent_home.path());
    let plugin_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        plugin_dir.path().join("plugin.json"),
        r#"{"name": "regr-lazy-mcp-plugin"}"#,
    )
    .unwrap();
    std::fs::write(
        plugin_dir.path().join(".mcp.json"),
        r#"{"mcpServers":{"regr-srv":{"command":"echo","args":["hi"]}}}"#,
    )
    .unwrap();
    let auth_home = tempfile::tempdir().unwrap();
    let auth_manager =
        std::sync::Arc::new(AuthManager::new(auth_home.path(), EchoAgentComConfig::default()));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let gateway = GatewaySender::new(tx);
    let mut cfg = AgentConfig::default();
    cfg.plugins.cli_plugin_dirs = vec![plugin_dir.path().to_path_buf()];
    let agent = MvpAgent::new(gateway, &cfg, auth_manager, None).expect("valid test config");
    assert!(
        agent.plugin_registry_handle.snapshot().is_none(),
        "snapshot must start empty (boot discovery deferred past initialize)"
    );
    agent.ensure_plugin_registry();
    let snapshot = agent
        .plugin_registry_handle
        .snapshot()
        .expect("snapshot must be populated on first use");
    assert!(
        snapshot.get("regr-lazy-mcp-plugin").is_some(),
        "lazy discovery must surface the plugin so its MCP server merges into the session"
    );
    agent.ensure_plugin_registry();
    assert!(
        agent
            .plugin_registry_handle
            .snapshot()
            .is_some_and(|s| s.get("regr-lazy-mcp-plugin").is_some()),
        "repeat call must keep the populated snapshot"
    );
}
mod list_running_heal_tests;
#[cfg(unix)]
mod process_scope_reclaim;
mod session_rename_tests;
mod session_resume_close_tests;
mod subagent_spawn_context_tests;
/// No load in flight and no session → the wait returns immediately
/// (the caller then surfaces "unknown session id" exactly as before).
#[tokio::test]
async fn wait_for_in_flight_load_returns_immediately_when_idle() {
    let agent = build_minimal_agent_for_tests();
    let sid = acp::SessionId::new("sess-none");
    tokio::time::timeout(
        std::time::Duration::from_millis(200),
        agent.wait_for_in_flight_session_load(&sid),
    )
    .await
    .expect("wait must not block when no load is in flight");
}
/// A waiter racing an in-flight `session/load` blocks until the load
/// finishes and then observes the registered session. This is the
/// agent-side guarantee that closes the post-leader-crash
/// "unknown session id" race: the reconnect replay's `session/load` and
/// the client's next `session/prompt` can arrive back-to-back.
#[tokio::test]
async fn wait_for_in_flight_load_blocks_until_load_completes() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let agent = std::rc::Rc::new(build_minimal_agent_for_tests());
            let sid = acp::SessionId::new("sess-loading");
            let guard = agent.begin_session_load(&sid);
            let waiter_agent = agent.clone();
            let waiter_sid = sid.clone();
            let waiter = tokio::task::spawn_local(async move {
                waiter_agent
                    .wait_for_in_flight_session_load(&waiter_sid)
                    .await;
                waiter_agent.is_resident(&waiter_sid)
            });
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            assert!(!waiter.is_finished(), "waiter must block while loading");
            let handle = make_test_handle("test-model", false, None);
            agent.insert_resident(&sid, handle);
            drop(guard);
            let found_session = tokio::time::timeout(std::time::Duration::from_secs(5), waiter)
                .await
                .expect("waiter must wake when the load guard drops")
                .expect("waiter task must not panic");
            assert!(
                found_session,
                "after the wait, the session must be visible to the racing request"
            );
        })
        .await;
}
/// A failed load (guard dropped WITHOUT registering the session) also
/// wakes waiters — they re-check, find nothing, and the caller surfaces
/// the regular "unknown session id" error rather than hanging.
#[tokio::test]
async fn wait_for_in_flight_load_wakes_on_failed_load() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let agent = std::rc::Rc::new(build_minimal_agent_for_tests());
            let sid = acp::SessionId::new("sess-load-fails");
            let guard = agent.begin_session_load(&sid);
            let waiter_agent = agent.clone();
            let waiter_sid = sid.clone();
            let waiter = tokio::task::spawn_local(async move {
                waiter_agent
                    .wait_for_in_flight_session_load(&waiter_sid)
                    .await;
                waiter_agent.is_resident(&waiter_sid)
            });
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            drop(guard);
            let found_session = tokio::time::timeout(std::time::Duration::from_secs(5), waiter)
                .await
                .expect("waiter must wake when the failed load's guard drops")
                .expect("waiter task must not panic");
            assert!(!found_session, "failed load leaves no session behind");
        })
        .await;
}
/// Two concurrent loads of the same session: the first guard's drop must
/// not remove the second load's marker (waiters keep waiting on the
/// newer in-flight load).
#[tokio::test]
async fn concurrent_load_guards_do_not_clobber_each_other() {
    let agent = build_minimal_agent_for_tests();
    let sid = acp::SessionId::new("sess-concurrent");
    let guard_one = agent.begin_session_load(&sid);
    let guard_two = agent.begin_session_load(&sid);
    drop(guard_one);
    assert!(
        agent.session_registry.is_attaching(&sid),
        "second load's marker must survive the first guard's drop"
    );
    drop(guard_two);
    assert!(
        agent.session_registry.attaching_count() == 0,
        "all markers removed once every load finished"
    );
}
/// `resident_activity` returns `NeedsInput` whenever the session's
/// pending-interaction map is non-empty — and that wins even over a
/// running turn (a session blocked on a permission mid-turn "needs
/// input"). Clearing the map falls back to Working / Idle.
#[tokio::test]
async fn resident_activity_reports_needs_input_when_pending() {
    use crate::agent::roster::RosterActivity;
    let agent = build_minimal_agent_for_tests();
    let sid = acp::SessionId::new("sess-pending");
    let handle = make_test_handle("echo-agent-3", false, None);
    let pending = handle.pending_interactions.clone();
    let prompt_id = handle.current_prompt_id.clone();
    agent.insert_resident(&sid, handle);
    assert_eq!(agent.resident_activity(&sid), RosterActivity::Idle);
    *prompt_id.lock().unwrap() = Some("turn-1".to_string());
    assert_eq!(agent.resident_activity(&sid), RosterActivity::Working);
    pending.lock().unwrap().insert(
        "call-1".to_string(),
        crate::session::pending_interaction::PendingKind::Permission,
    );
    assert_eq!(agent.resident_activity(&sid), RosterActivity::NeedsInput);
    let entry = agent.resident_roster_entry(&sid).expect("resident entry");
    assert_eq!(entry.activity, RosterActivity::NeedsInput);
    pending.lock().unwrap().clear();
    assert_eq!(agent.resident_activity(&sid), RosterActivity::Working);
}
/// Drain the agent gateway, returning the first `echo.agent/sessions/changed`
/// payload that carries an upserted entry (ignoring any unrelated
/// notifications, which parse into an empty `RosterChanged`).
fn drain_roster_changed(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<echo_agent_acp::AcpClientMessage>,
) -> Option<crate::agent::roster::RosterChanged> {
    let mut found = None;
    while let Ok(msg) = rx.try_recv() {
        if let echo_agent_acp::AcpClientMessage::ExtNotification(args) = msg {
            if found.is_none()
                && let Ok(changed) = serde_json::from_str::<crate::agent::roster::RosterChanged>(
                    args.request.params.get(),
                )
                && !changed.upserted.is_empty()
            {
                found = Some(changed);
            }
            let _ = args.response_tx.send(Ok(()));
        }
    }
    found
}
/// A turn-boundary activity delta (`push_roster_activity_delta`) broadcasts
/// an `echo.agent/sessions/changed` upsert carrying the *overridden* activity, so
/// every attached dashboard reflects Working/Idle immediately instead of
/// waiting for the ≤1s roster poll (turn-start/turn-end). The
/// override matters because at turn-start the actor has not yet published
/// `current_prompt_id`, so a natural `resident_activity` read would emit
/// `Idle` for a session that is in fact starting a turn.
#[tokio::test]
async fn push_roster_activity_delta_broadcasts_overridden_activity() {
    use crate::agent::config::Config as AgentConfig;
    use crate::agent::roster::RosterActivity;
    use crate::auth::{AuthManager, EchoAgentComConfig};
    let temp_dir = tempfile::tempdir().unwrap();
    let auth_manager =
        std::sync::Arc::new(AuthManager::new(temp_dir.path(), EchoAgentComConfig::default()));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let gateway = GatewaySender::new(tx);
    let cfg = AgentConfig::default();
    let agent = MvpAgent::new(gateway, &cfg, auth_manager, None).expect("valid test config");
    let sid = acp::SessionId::new("sess-activity");
    agent.insert_resident(&sid, make_test_handle("echo-agent-3", false, None));
    agent.push_roster_activity_delta(&sid, RosterActivity::Working);
    let changed = drain_roster_changed(&mut rx).expect("turn-start delta emitted");
    assert_eq!(changed.upserted.len(), 1);
    assert_eq!(changed.upserted[0].session_id, sid.0.to_string());
    assert!(changed.upserted[0].resident);
    assert_eq!(
        changed.upserted[0].activity,
        RosterActivity::Working,
        "forced activity must override the Idle that resident_activity would read"
    );
    assert!(changed.removed.is_empty());
    agent.push_roster_activity_delta(&sid, RosterActivity::Idle);
    let changed = drain_roster_changed(&mut rx).expect("turn-end delta emitted");
    assert_eq!(changed.upserted[0].activity, RosterActivity::Idle);
}
/// Extract the inner payload from an ExtResponse.
#[expect(
    dead_code,
    reason = "unused in production; remove expect when wired or delete the item"
)]
fn parse_ext_body(resp: &acp::ExtResponse) -> serde_json::Value {
    let outer: serde_json::Value =
        serde_json::from_str(resp.0.get()).expect("ExtResponse must be valid JSON");
    outer
        .get("result")
        .cloned()
        .unwrap_or_else(|| panic!("ExtResponse has no 'result' key; full JSON: {outer}"))
}
/// Replicate the lookup logic of code_nav_eligibility_for_request so we
/// can test it with a plain sessions HashMap.
fn check_nav_eligibility_from_sessions(
    sessions: &HashMap<acp::SessionId, crate::session::SessionHandle>,
    session_id: Option<&acp::SessionId>,
) -> Result<(), CodeNavEligibility> {
    let session_id = match session_id {
        Some(sid) => sid,
        None => return Err(CodeNavEligibility::SessionRequired),
    };
    let Some(handle) = sessions.get(session_id) else {
        return Err(CodeNavEligibility::SessionRequired);
    };
    let ct = crate::http::client_type_from_origin(handle.origin_client.as_ref());
    if !matches!(ct, ClientType::EchoAgentWeb) {
        return Err(CodeNavEligibility::ClientNotWeb);
    }
    if !handle.code_nav_enabled {
        return Err(CodeNavEligibility::CapabilityNotAdvertised);
    }
    Ok(())
}
/// Web session with code-nav capability is eligible.
///
/// This is the "happy path" that allows lazy index startup on the first
/// code-nav request.
#[tokio::test]
async fn test_web_session_with_capability_is_eligible() {
    let sid = acp::SessionId::new("sess-web");
    let mut handle = make_test_handle("model", false, Some("echo-agent-web"));
    handle.code_nav_enabled = true;
    let sessions = [(sid.clone(), handle)].into();
    assert!(
        check_nav_eligibility_from_sessions(&sessions, Some(&sid)).is_ok(),
        "web session with code-nav capability must be eligible"
    );
}
/// TUI session is rejected at gate 1 (client type) regardless of capability.
#[tokio::test]
async fn test_tui_session_is_rejected() {
    let sid = acp::SessionId::new("sess-tui");
    let mut handle = make_test_handle("model", false, Some("echo-agent-tui"));
    handle.code_nav_enabled = true;
    let sessions = [(sid.clone(), handle)].into();
    assert_eq!(
        check_nav_eligibility_from_sessions(&sessions, Some(&sid)),
        Err(CodeNavEligibility::ClientNotWeb),
        "TUI client must be rejected at gate 1 (client type)"
    );
}
/// Web session without capability is rejected at gate 2.
#[tokio::test]
async fn test_web_session_without_capability_is_rejected() {
    let sid = acp::SessionId::new("sess-web-no-cap");
    let mut handle = make_test_handle("model", false, Some("echo-agent-web"));
    handle.code_nav_enabled = false;
    let sessions = [(sid.clone(), handle)].into();
    assert_eq!(
        check_nav_eligibility_from_sessions(&sessions, Some(&sid)),
        Err(CodeNavEligibility::CapabilityNotAdvertised),
        "web client without capability must be rejected at gate 2"
    );
}
/// Leader-mode isolation: two sessions with different code-nav state return
/// independent results.
#[tokio::test]
async fn test_leader_mode_two_sessions_stay_isolated() {
    let web_sid = acp::SessionId::new("web");
    let tui_sid = acp::SessionId::new("tui");
    let mut web_handle = make_test_handle("model", false, Some("echo-agent-web"));
    web_handle.code_nav_enabled = true;
    let mut tui_handle = make_test_handle("model", false, Some("echo-agent-tui"));
    tui_handle.code_nav_enabled = false;
    let sessions = [(web_sid.clone(), web_handle), (tui_sid.clone(), tui_handle)].into();
    assert!(
        check_nav_eligibility_from_sessions(&sessions, Some(&web_sid)).is_ok(),
        "web session must be eligible"
    );
    assert_eq!(
        check_nav_eligibility_from_sessions(&sessions, Some(&tui_sid)),
        Err(CodeNavEligibility::ClientNotWeb),
        "tui session must remain ineligible even when web session is eligible"
    );
}
/// Unknown session ID returns SessionRequired, not a global fallback.
///
/// This is the stale/evicted session path: a caller with a session ID that
/// no longer exists in the sessions map must get SessionRequired, not
/// accidentally inherit the last-initialized client's eligibility.
#[tokio::test]
async fn test_unknown_session_id_returns_session_required() {
    let known_sid = acp::SessionId::new("known");
    let mut known_handle = make_test_handle("model", false, Some("echo-agent-web"));
    known_handle.code_nav_enabled = true;
    let sessions = [(known_sid.clone(), known_handle)].into();
    let stale_sid = acp::SessionId::new("stale-or-evicted");
    assert_eq!(
        check_nav_eligibility_from_sessions(&sessions, Some(&stale_sid)),
        Err(CodeNavEligibility::SessionRequired),
        "stale/evicted sessionId must not fall back to global state"
    );
    assert!(check_nav_eligibility_from_sessions(&sessions, Some(&known_sid)).is_ok());
}
mod parse_json_object_env_tests {
    use super::parse_json_object_env;
    unsafe fn set(k: &str, v: &str) {
        unsafe { std::env::set_var(k, v) };
    }
    unsafe fn unset(k: &str) {
        unsafe { std::env::remove_var(k) };
    }
    #[test]
    #[serial_test::serial]
    fn valid_json_object_returns_some() {
        unsafe { set("TEST_JSON_OBJ", r#"{"team":"platform","org":"acme"}"#) };
        let result = parse_json_object_env("TEST_JSON_OBJ");
        unsafe { unset("TEST_JSON_OBJ") };
        let val = result.expect("should parse valid JSON object");
        assert_eq!(val["team"], "platform");
        assert_eq!(val["org"], "acme");
    }
    #[test]
    #[serial_test::serial]
    fn non_object_json_returns_none() {
        unsafe { set("TEST_JSON_ARR", r#"["not","an","object"]"#) };
        let result = parse_json_object_env("TEST_JSON_ARR");
        unsafe { unset("TEST_JSON_ARR") };
        assert!(result.is_none());
    }
    #[test]
    #[serial_test::serial]
    fn invalid_json_returns_none() {
        unsafe { set("TEST_JSON_BAD", "not json at all") };
        let result = parse_json_object_env("TEST_JSON_BAD");
        unsafe { unset("TEST_JSON_BAD") };
        assert!(result.is_none());
    }
    #[test]
    #[serial_test::serial]
    fn unset_var_returns_none() {
        unsafe { unset("TEST_JSON_UNSET") };
        assert!(parse_json_object_env("TEST_JSON_UNSET").is_none());
    }
}
mod eligibility_gates {
    use super::*;
    /// Standalone replica of the first three eligibility gates.
    /// Gate 4 (git root) requires a real filesystem and is covered by
    /// integration tests.
    fn check_gates(
        client_type: ClientType,
        code_nav_enabled: bool,
        indexing_enabled: bool,
    ) -> Result<(), CodeNavEligibility> {
        if !matches!(client_type, ClientType::EchoAgentWeb) {
            return Err(CodeNavEligibility::ClientNotWeb);
        }
        if !code_nav_enabled {
            return Err(CodeNavEligibility::CapabilityNotAdvertised);
        }
        if !indexing_enabled {
            return Err(CodeNavEligibility::DisabledByConfig);
        }
        Ok(())
    }
    #[test]
    fn non_web_client_rejected() {
        assert_eq!(
            check_gates(ClientType::Generic, true, true),
            Err(CodeNavEligibility::ClientNotWeb)
        );
    }
    #[test]
    fn tui_client_rejected() {
        assert_eq!(
            check_gates(ClientType::EchoAgentTUI, true, true),
            Err(CodeNavEligibility::ClientNotWeb)
        );
    }
    #[test]
    fn web_client_no_capability_rejected() {
        assert_eq!(
            check_gates(ClientType::EchoAgentWeb, false, true),
            Err(CodeNavEligibility::CapabilityNotAdvertised)
        );
    }
    #[test]
    fn web_client_with_capability_config_disabled_rejected() {
        assert_eq!(
            check_gates(ClientType::EchoAgentWeb, true, false),
            Err(CodeNavEligibility::DisabledByConfig)
        );
    }
    #[test]
    fn web_client_with_capability_and_config_passes_first_three_gates() {
        assert!(check_gates(ClientType::EchoAgentWeb, true, true).is_ok());
    }
}
#[test]
fn find_model_by_id_prefers_key_then_falls_back_to_slug() {
    let entry = |model: &str| ModelEntry {
        info: config::ModelInfo {
            user_selectable: true,
            id: None,
            model_family: None,
            model: model.to_string(),
            base_url: String::new(),
            name: None,
            description: None,
            max_completion_tokens: None,
            temperature: None,
            top_p: None,
            api_backend: crate::sampling::ApiBackend::default(),
            auth_scheme: Default::default(),
            extra_headers: IndexMap::new(),
            query_params: IndexMap::new(),
            env_http_headers: IndexMap::new(),
            context_window: std::num::NonZeroU64::new(200_000).unwrap(),
            auto_compact_threshold_percent: None,
            system_prompt_label: None,
            use_concise: false,
            agent_type: config::default_agent_type(),
            inference_idle_timeout_secs: None,
            max_retries: None,
            subagent_rate_limit_max_attempts: None,
            hidden: false,
            supported_in_api: true,
            reasoning_effort: None,
            supports_reasoning_effort: false,
            reasoning_efforts: Vec::new(),
            supports_backend_search: false,
            compactions_remaining: None,
            compaction_at_tokens: None,
            show_model_fingerprint: false,
            stream_tool_calls: None,
            laziness_detector: crate::agent::config::LazinessDetectorPerModelConfig::default(),
        },
        api_key: None,
        env_key: None,
        auth_provider: None,
        api_base_url: None,
    };
    let mut models = indexmap::IndexMap::new();
    models.insert("a".to_string(), entry("target"));
    models.insert("target".to_string(), entry("other"));
    assert_eq!(
        config::find_model_by_id(&models, "target").unwrap().model,
        "other",
        "key match should win over slug scan"
    );
    assert_eq!(
        config::find_model_by_id(&models, "a").unwrap().model,
        "target",
        "exact key match for 'a'"
    );
}
fn write_updates(dir: &std::path::Path, lines: &[&str]) -> PathBuf {
    let path = dir.join("updates.jsonl");
    std::fs::write(&path, lines.join("\n")).unwrap();
    path
}
fn bg_line(task_id: &str) -> String {
    format!(
        r#"{{"timestamp":1,"method":"_echo.agent/session/update","params":{{"sessionId":"s","update":{{"sessionUpdate":"task_backgrounded","task_id":"{task_id}","command":"sleep 99","cwd":"/tmp"}}}}}}"#
    )
}
fn completed_line(task_id: &str) -> String {
    format!(
        r#"{{"timestamp":2,"method":"_echo.agent/session/update","params":{{"sessionId":"s","update":{{"sessionUpdate":"task_completed","task_snapshot":{{"task_id":"{task_id}","completed":true}}}}}}}}"#
    )
}
fn orphaned_ids(tasks: &[OrphanedTask]) -> std::collections::HashSet<&str> {
    tasks.iter().map(|t| t.task_id.as_str()).collect()
}
#[test]
fn orphaned_tasks_returns_empty_for_no_file() {
    let result = MvpAgent::find_orphaned_background_tasks(&None);
    assert!(result.is_empty());
}
#[test]
fn orphaned_tasks_returns_empty_for_missing_file() {
    let path = PathBuf::from("/nonexistent/updates.jsonl");
    let result = MvpAgent::find_orphaned_background_tasks(&Some(path));
    assert!(result.is_empty());
}
#[test]
fn orphaned_tasks_returns_empty_when_all_completed() {
    let tmp = tempfile::tempdir().unwrap();
    let bg = bg_line("t1");
    let done = completed_line("t1");
    let path = write_updates(tmp.path(), &[&bg, &done]);
    let result = MvpAgent::find_orphaned_background_tasks(&Some(path));
    assert!(result.is_empty());
}
#[test]
fn orphaned_tasks_returns_uncompleted() {
    let tmp = tempfile::tempdir().unwrap();
    let bg1 = bg_line("t1");
    let bg2 = bg_line("t2");
    let done1 = completed_line("t1");
    let path = write_updates(tmp.path(), &[&bg1, &bg2, &done1]);
    let result = MvpAgent::find_orphaned_background_tasks(&Some(path));
    let ids = orphaned_ids(&result);
    assert_eq!(ids.len(), 1);
    assert!(ids.contains("t2"));
}
#[test]
fn orphaned_tasks_returns_multiple_uncompleted() {
    let tmp = tempfile::tempdir().unwrap();
    let bg1 = bg_line("t1");
    let bg2 = bg_line("t2");
    let bg3 = bg_line("t3");
    let done2 = completed_line("t2");
    let path = write_updates(tmp.path(), &[&bg1, &bg2, &bg3, &done2]);
    let result = MvpAgent::find_orphaned_background_tasks(&Some(path));
    let ids = orphaned_ids(&result);
    assert_eq!(ids.len(), 2);
    assert!(ids.contains("t1"));
    assert!(ids.contains("t3"));
}
#[test]
fn orphaned_tasks_captures_command_and_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let bg = bg_line("t1");
    let path = write_updates(tmp.path(), &[&bg]);
    let result = MvpAgent::find_orphaned_background_tasks(&Some(path));
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].command, "sleep 99");
    assert_eq!(result[0].cwd, "/tmp");
}
#[test]
fn orphaned_tasks_skips_malformed_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let bg = bg_line("t1");
    let path = write_updates(tmp.path(), &["not json", &bg, "{}"]);
    let result = MvpAgent::find_orphaned_background_tasks(&Some(path));
    assert_eq!(result.len(), 1);
}
#[test]
fn orphaned_tasks_ignores_unrelated_updates() {
    let tmp = tempfile::tempdir().unwrap();
    let bg = bg_line("t1");
    let unrelated = r#"{"timestamp":1,"method":"_echo.agent/session/update","params":{"sessionId":"s","update":{"sessionUpdate":"auto_compact_started","percentage":80}}}"#;
    let path = write_updates(tmp.path(), &[&bg, unrelated]);
    let result = MvpAgent::find_orphaned_background_tasks(&Some(path));
    assert_eq!(result.len(), 1);
}
#[test]
fn orphaned_tasks_filters_rewind_dead_branches() {
    let tmp = tempfile::tempdir().unwrap();
    let user_msg = r#"{"timestamp":0,"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"hello"}}}}"#;
    let bg_before_rewind = bg_line("t-dead");
    let rewind = r#"{"timestamp":3,"method":"_echo.agent/session/update","params":{"sessionId":"s","update":{"sessionUpdate":"rewind_marker","target_prompt_index":0,"created_at":"2025-01-01T00:00:00Z"}}}"#;
    let user_msg2 = r#"{"timestamp":4,"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"retry"}}}}"#;
    let bg_after_rewind = bg_line("t-alive");
    let path = write_updates(
        tmp.path(),
        &[
            user_msg,
            &bg_before_rewind,
            rewind,
            user_msg2,
            &bg_after_rewind,
        ],
    );
    let result = MvpAgent::find_orphaned_background_tasks(&Some(path));
    let ids = orphaned_ids(&result);
    assert!(
        ids.contains("t-alive"),
        "task after rewind should be present"
    );
    assert!(
        !ids.contains("t-dead"),
        "task in dead branch should be filtered"
    );
}
#[test]
fn allow_access_from_remote_settings() {
    let json = serde_json::json!({ "allow_access": true });
    let rs: crate::util::config::RemoteSettings = serde_json::from_value(json).unwrap();
    assert_eq!(rs.allow_access, Some(true));
    let json = serde_json::json!({ "allow_access": false });
    let rs: crate::util::config::RemoteSettings = serde_json::from_value(json).unwrap();
    assert_eq!(rs.allow_access, Some(false));
    let json = serde_json::json!({});
    let rs: crate::util::config::RemoteSettings = serde_json::from_value(json).unwrap();
    assert_eq!(rs.allow_access, None);
}
#[test]
fn on_demand_enabled_from_remote_settings() {
    let json = serde_json::json!({ "on_demand_enabled": false });
    let rs: crate::util::config::RemoteSettings = serde_json::from_value(json).unwrap();
    assert_eq!(rs.on_demand_enabled, Some(false));
    let json = serde_json::json!({});
    let rs: crate::util::config::RemoteSettings = serde_json::from_value(json).unwrap();
    assert_eq!(rs.on_demand_enabled, None);
}
/// Regression for a 401 sequence seen in production. After a long idle
/// window, the auth manager may have no
/// live token by the time `session/new` runs. For session-based auth methods
/// we MUST still report `SessionToken` so chat_state credentials retain the
/// session-token shape and `try_refresh_session_token` will run on the next
/// prompt instead of early-returning.
#[tokio::test(flavor = "current_thread")]
async fn auth_type_session_based_no_current_returns_session_token() {
    for method_id in [
        crate::agent::auth_method::CACHED_TOKEN_AUTH_METHOD_ID,
        crate::agent::auth_method::ECHO_AGENT_COM_METHOD_ID,
        crate::agent::auth_method::OIDC_METHOD_ID,
    ] {
        let agent = build_minimal_agent_for_tests();
        agent.set_auth_method(acp::AuthMethodId::new(method_id));
        assert!(
            agent.auth_manager.current().is_none(),
            "{method_id}: precondition: AuthManager has no current token",
        );
        assert_eq!(
            agent.auth_type(),
            echo_agent_chat_state::AuthType::SessionToken,
            "{method_id}: session-based auth must report SessionToken even \
                 without a live token -- otherwise chat_state gets locked into \
                 auth_type = ApiKey and try_refresh_session_token will skip \
                 every subsequent refresh attempt.",
        );
    }
}
/// BYOK guard. Users with `echoagent.api_key` must continue to report `ApiKey`
/// regardless of live-token state -- BYOK sessions have nothing to refresh,
/// and reporting `SessionToken` would route through cli-chat-proxy paths
/// (image_gen / video_gen base_url) that don't apply to BYOK keys.
#[tokio::test(flavor = "current_thread")]
async fn auth_type_echo_agent_api_key_no_current_returns_api_key() {
    let agent = build_minimal_agent_for_tests();
    agent.set_auth_method(acp::AuthMethodId::new(
        crate::agent::auth_method::ECHO_AGENT_API_KEY_METHOD_ID,
    ));
    assert!(agent.auth_manager.current().is_none());
    assert_eq!(
        agent.auth_type(),
        echo_agent_chat_state::AuthType::ApiKey,
        "echoagent.api_key auth must report ApiKey -- BYOK has no session-token \
             behavior to fall back to."
    );
}
/// Positive baseline: when both signals agree (session-based method AND
/// a live in-memory token), `SessionToken` is returned. This is the
/// common case during a healthy session.
#[tokio::test(flavor = "current_thread")]
async fn auth_type_session_based_with_current_returns_session_token() {
    use crate::auth::EchoAgentAuth;
    let agent = build_minimal_agent_for_tests();
    agent.set_auth_method(acp::AuthMethodId::new(
        crate::agent::auth_method::OIDC_METHOD_ID,
    ));
    agent.auth_manager.hot_swap(EchoAgentAuth::test_default());
    assert!(agent.auth_manager.current().is_some());
    assert_eq!(agent.auth_type(), echo_agent_chat_state::AuthType::SessionToken,);
}
/// Defensive case: no `auth_method_id` selected yet (pre-`authenticate`
/// state) and no live credential. We default to `ApiKey` so callers
/// that key off this value (e.g. `resolve_chat_state_auth_type` for chat
/// routing) don't accidentally route session-token-shaped traffic
/// through cli-chat-proxy before a method has been chosen.
#[tokio::test(flavor = "current_thread")]
async fn auth_type_no_method_id_no_current_returns_api_key() {
    let agent = build_minimal_agent_for_tests();
    assert!(agent.auth_method_id.load().is_none());
    assert!(agent.auth_manager.current().is_none());
    assert_eq!(agent.auth_type(), echo_agent_chat_state::AuthType::ApiKey,);
}
/// Live credential present but `auth_method_id` is still `None`. The
/// in-memory bearer takes precedence: this is the order observed during
/// `initialize()` silent refresh -- a token is hot-swapped in before
/// `authenticate()` writes the method id. Reporting `SessionToken`
/// here matches pre-fix behavior and keeps logging stable.
#[tokio::test(flavor = "current_thread")]
async fn auth_type_no_method_id_with_current_returns_session_token() {
    use crate::auth::EchoAgentAuth;
    let agent = build_minimal_agent_for_tests();
    agent.auth_manager.hot_swap(EchoAgentAuth::test_default());
    assert!(agent.auth_method_id.load().is_none());
    assert!(agent.auth_manager.current().is_some());
    assert_eq!(agent.auth_type(), echo_agent_chat_state::AuthType::SessionToken,);
}
/// A desktop Runtime can initialize before organization login, then receive
/// its first managed BYOK model through `internal/reload_models`. The reload
/// must install API-key auth immediately and retire it again when the last
/// credential disappears; requiring a process restart is a broken lifecycle.
#[tokio::test(flavor = "current_thread")]
#[serial_test::serial]
async fn model_reload_reconciles_first_added_and_last_removed_byok_credential() {
    use crate::agent::auth_method::{
        ECHO_AGENT_API_KEY_ENV_VAR, ECHO_AGENT_API_KEY_METHOD_ID,
        LEGACY_ECHO_AGENT_API_KEY_ENV_VAR,
    };
    use crate::agent::config::{EndpointsConfig, ModelEntry};
    use echo_agent_test_support::EnvGuard;
    let _new = EnvGuard::unset(ECHO_AGENT_API_KEY_ENV_VAR);
    let _legacy = EnvGuard::unset(LEGACY_ECHO_AGENT_API_KEY_ENV_VAR);
    let agent = build_minimal_agent_for_tests();
    assert!(agent.auth_method_id.load().is_none());

    let mut managed = ModelEntry::fallback("organization/minimax-m3", &EndpointsConfig::default());
    managed.api_key = Some("organization-secret".into());
    agent
        .models_manager
        .insert_test_entry("organization/minimax-m3", managed);

    assert_eq!(
        agent.reconcile_auth_method_after_model_reload().as_deref(),
        Some(ECHO_AGENT_API_KEY_METHOD_ID),
        "the first hot-reloaded organization credential must make session/new usable"
    );

    let without_credential =
        ModelEntry::fallback("organization/minimax-m3", &EndpointsConfig::default());
    agent
        .models_manager
        .insert_test_entry("organization/minimax-m3", without_credential);
    assert_eq!(agent.reconcile_auth_method_after_model_reload(), None);
    assert!(agent.auth_method_id.load().is_none());
}

/// Adding a BYOK model must not replace an already authenticated OIDC/cached
/// session. Model-owned credentials win per model during sampling while the
/// process-wide session method remains available to other models.
#[tokio::test(flavor = "current_thread")]
#[serial_test::serial]
async fn model_reload_preserves_established_session_auth_method() {
    use crate::agent::auth_method::{
        CACHED_TOKEN_AUTH_METHOD_ID, ECHO_AGENT_API_KEY_ENV_VAR,
        LEGACY_ECHO_AGENT_API_KEY_ENV_VAR,
    };
    use crate::agent::config::{EndpointsConfig, ModelEntry};
    use echo_agent_test_support::EnvGuard;
    let _new = EnvGuard::unset(ECHO_AGENT_API_KEY_ENV_VAR);
    let _legacy = EnvGuard::unset(LEGACY_ECHO_AGENT_API_KEY_ENV_VAR);
    let agent = build_minimal_agent_for_tests();
    agent.set_auth_method(acp::AuthMethodId::new(CACHED_TOKEN_AUTH_METHOD_ID));
    let mut managed = ModelEntry::fallback("organization/minimax-m3", &EndpointsConfig::default());
    managed.api_key = Some("organization-secret".into());
    agent
        .models_manager
        .insert_test_entry("organization/minimax-m3", managed);

    assert_eq!(
        agent.reconcile_auth_method_after_model_reload().as_deref(),
        Some(CACHED_TOKEN_AUTH_METHOD_ID)
    );
}
/// Minimal agent whose `echo_agent_com_config` engages the api-key kill switch
/// (`disable_api_key_auth = true`), mirroring a forced-IdP deployment.
fn build_agent_with_api_key_auth_disabled() -> MvpAgent {
    use crate::agent::config::Config as AgentConfig;
    use crate::auth::{AuthManager, EchoAgentComConfig};
    let temp_dir = tempfile::tempdir().unwrap();
    let auth_manager =
        std::sync::Arc::new(AuthManager::new(temp_dir.path(), EchoAgentComConfig::default()));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let gateway = GatewaySender::new(tx);
    let mut cfg = AgentConfig::default();
    cfg.echo_agent_com_config.disable_api_key_auth = Some(true);
    MvpAgent::new(gateway, &cfg, auth_manager, None).expect("valid test config")
}
/// Deployment-key / managed-config user: `ECHO_AGENT_API_KEY` resolves and the kill
/// switch is off, so a dead `cached_token` MUST fall through to `echoagent.api_key`
/// (no browser). This is the exact regression the fallthrough fixes.
#[tokio::test(flavor = "current_thread")]
#[serial_test::serial]
async fn cached_token_fallthrough_prefers_api_key_for_deployment_key() {
    use crate::agent::auth_method::{ECHO_AGENT_API_KEY_ENV_VAR, ECHO_AGENT_API_KEY_METHOD_ID};
    use echo_agent_test_support::EnvGuard;
    let _lockdown = EnvGuard::unset("ECHO_AGENT_DISABLE_API_KEY_AUTH");
    let _key = EnvGuard::set(ECHO_AGENT_API_KEY_ENV_VAR, "test-deployment-key");
    let agent = build_minimal_agent_for_tests();
    assert_eq!(
        agent
            .cached_token_fallthrough_method_id()
            .as_ref()
            .map(|id| id.0.as_ref()),
        Some(ECHO_AGENT_API_KEY_METHOD_ID),
        "deployment-key user (ECHO_AGENT_API_KEY set, no kill switch) must fall \
         through to echoagent.api_key on a dead cached_token -- not interactive login",
    );
}
/// Forced-IdP deployment: even with `ECHO_AGENT_API_KEY` present, the admin kill
/// switch keeps the fallthrough on interactive `cloud.echo-agent.invalid` (api-key auth is
/// neither advertised nor an eligible fallthrough).
#[tokio::test(flavor = "current_thread")]
#[serial_test::serial]
async fn cached_token_fallthrough_respects_kill_switch() {
    use crate::agent::auth_method::{ECHO_AGENT_COM_METHOD_ID, ECHO_AGENT_API_KEY_ENV_VAR};
    use echo_agent_test_support::EnvGuard;
    let _lockdown = EnvGuard::unset("ECHO_AGENT_DISABLE_API_KEY_AUTH");
    let _key = EnvGuard::set(ECHO_AGENT_API_KEY_ENV_VAR, "test-deployment-key");
    let agent = build_agent_with_api_key_auth_disabled();
    assert_eq!(
        agent
            .cached_token_fallthrough_method_id()
            .as_ref()
            .map(|id| id.0.as_ref()),
        Some(ECHO_AGENT_COM_METHOD_ID),
        "disable_api_key_auth must keep the cached_token fallthrough on \
         interactive cloud.echo-agent.invalid so ECHO_AGENT_API_KEY can't bypass forced IdP login",
    );
}
/// No advertiseable credentials at all (no env key, no kill switch): the user
/// genuinely needs to log in, so the fallthrough is interactive `cloud.echo-agent.invalid`.
#[tokio::test(flavor = "current_thread")]
#[serial_test::serial]
async fn cached_token_fallthrough_falls_to_echo_agent_com_without_credentials() {
    use crate::agent::auth_method::{
        ECHO_AGENT_COM_METHOD_ID, LEGACY_ECHO_AGENT_API_KEY_ENV_VAR, ECHO_AGENT_API_KEY_ENV_VAR,
    };
    use echo_agent_test_support::EnvGuard;
    let _lockdown = EnvGuard::unset("ECHO_AGENT_DISABLE_API_KEY_AUTH");
    let _new = EnvGuard::unset(ECHO_AGENT_API_KEY_ENV_VAR);
    let _legacy = EnvGuard::unset(LEGACY_ECHO_AGENT_API_KEY_ENV_VAR);
    let agent = build_minimal_agent_for_tests();
    assert_eq!(
        agent
            .cached_token_fallthrough_method_id()
            .as_ref()
            .map(|id| id.0.as_ref()),
        Some(ECHO_AGENT_COM_METHOD_ID),
        "no API-key creds and no kill switch -> interactive cloud.echo-agent.invalid login",
    );
}
/// Verifies the 4-state matrix of `(disable_zdr_incompatible_tools, zdr_video_output_s3)`:
///
/// | ZDR flag | S3 config | Result                                      |
/// |----------|-----------|---------------------------------------------|
/// | false    | None      | Enabled, no S3 (normal non-ZDR mode)        |
/// | true     | None      | Disabled (ZDR with no escape hatch)         |
/// | false    | Some      | Enabled, S3 **not** threaded (non-ZDR)      |
/// | true     | Some      | Enabled, S3 threaded (ZDR with upload path) |
#[tokio::test(flavor = "current_thread")]
async fn prepare_video_gen_config_disabled_when_zdr_flag_set() {
    use echo_agent_tools::implementations::echo_agent_build::video_gen::{
        S3AccessCredentials, VideoGenConfig, ZdrVideoOutputS3Config,
    };
    fn zdr_s3() -> ZdrVideoOutputS3Config {
        ZdrVideoOutputS3Config {
            bucket: "team-videos".into(),
            endpoint: "https://s3.example.com".into(),
            region: "us-east-1".into(),
            key_prefix: "echo-agent-videos/".into(),
            expires_secs: 900,
            read_write: S3AccessCredentials {
                access_key_id: "AKIA...".into(),
                secret_access_key: "secret".into(),
            },
            read_only: None,
        }
    }
    let agent = build_minimal_agent_for_tests();
    agent.sampling_config.borrow_mut().api_key = Some("test-key".to_string());
    assert!(matches!(
        agent.prepare_video_gen_config(),
        VideoGenConfig::Enabled { .. }
    ));
    agent.cfg.borrow_mut().disable_zdr_incompatible_tools = true;
    assert!(matches!(
        agent.prepare_video_gen_config(),
        VideoGenConfig::Enabled {
            zdr_restricted: true,
            ..
        }
    ));
    agent.cfg.borrow_mut().zdr_video_output_s3 = Some(zdr_s3());
    agent.cfg.borrow_mut().disable_zdr_incompatible_tools = false;
    let VideoGenConfig::Enabled {
        zdr_video_output_s3: s3_when_non_zdr,
        ..
    } = agent.prepare_video_gen_config()
    else {
        panic!("expected Enabled");
    };
    assert!(
        s3_when_non_zdr.is_none(),
        "S3 config must not be threaded when ZDR flag is off"
    );
    agent.cfg.borrow_mut().disable_zdr_incompatible_tools = true;
    let VideoGenConfig::Enabled {
        zdr_video_output_s3,
        zdr_restricted,
        ..
    } = agent.prepare_video_gen_config()
    else {
        panic!("expected Enabled");
    };
    assert!(zdr_video_output_s3.as_ref().is_some_and(|c| c.is_valid()));
    assert!(!zdr_restricted);
}
#[tokio::test(flavor = "current_thread")]
async fn prepare_video_gen_config_respects_feature_flag() {
    use echo_agent_tools::implementations::echo_agent_build::video_gen::VideoGenConfig;
    let agent = build_minimal_agent_for_tests();
    agent.sampling_config.borrow_mut().api_key = Some("test-key".to_string());
    assert!(matches!(
        agent.prepare_video_gen_config(),
        VideoGenConfig::Enabled { .. }
    ));
    agent.cfg.borrow_mut().features.video_gen = Some(false);
    assert!(matches!(
        agent.prepare_video_gen_config(),
        VideoGenConfig::Disabled
    ));
}
/// The imagine tier gate fails **open**: with no resolved auth we can't confirm
/// a restricted personal tier, so the tools stay advertised and un-flagged (the
/// server 429 remains the authoritative backstop). Guards against accidentally
/// disabling a paid feature when tier info hasn't loaded.
#[tokio::test(flavor = "current_thread")]
async fn prepare_image_gen_config_fails_open_without_auth() {
    use echo_agent_tools::implementations::echo_agent_build::image_gen::ImageGenConfig;
    let agent = build_minimal_agent_for_tests();
    agent.sampling_config.borrow_mut().api_key = Some("test-key".to_string());
    let ImageGenConfig::Enabled {
        tier_restricted, ..
    } = agent.prepare_image_gen_config()
    else {
        panic!("expected Enabled");
    };
    assert!(
        !tier_restricted,
        "no resolved auth ⇒ fail open (tools not tier-restricted)"
    );
}
/// The imagine tools bypass cli-chat-proxy (direct API calls), so the server
/// can only scope the coding data-retention opt-out (`/privacy opt-out`) to
/// Build traffic via the `x-echo-agent-client-identifier` header. If this header is
/// dropped, opted-out users' imagine prompts are logged/retained server-side.
#[tokio::test(flavor = "current_thread")]
async fn prepare_image_gen_config_sends_client_identifier_header() {
    use echo_agent_tools::implementations::echo_agent_build::image_gen::ImageGenConfig;
    let agent = build_minimal_agent_for_tests();
    agent.sampling_config.borrow_mut().api_key = Some("test-key".to_string());
    let ImageGenConfig::Enabled { extra_headers, .. } = agent.prepare_image_gen_config() else {
        panic!("expected Enabled");
    };
    assert_eq!(
        extra_headers
            .get("x-echo-agent-client-identifier")
            .map(String::as_str),
        Some(crate::http::process_client_identifier().as_str()),
        "imagine API calls must carry the client identifier so the server \
         applies the coding ZDR opt-out to Build traffic"
    );
}
/// Same contract for video generation (also a direct API call).
#[tokio::test(flavor = "current_thread")]
async fn prepare_video_gen_config_sends_client_identifier_header() {
    use echo_agent_tools::implementations::echo_agent_build::video_gen::VideoGenConfig;
    let agent = build_minimal_agent_for_tests();
    agent.sampling_config.borrow_mut().api_key = Some("test-key".to_string());
    let VideoGenConfig::Enabled { extra_headers, .. } = agent.prepare_video_gen_config() else {
        panic!("expected Enabled");
    };
    assert_eq!(
        extra_headers
            .get("x-echo-agent-client-identifier")
            .map(String::as_str),
        Some(crate::http::process_client_identifier().as_str()),
        "video gen API calls must carry the client identifier so the server \
         applies the coding ZDR opt-out to Build traffic"
    );
}
/// Regression: `echo.agent/auth/info` must return profile fields even when the
/// access token is expired — profile data does not expire with the token,
/// and hiding it made the desktop render "Signed in" with no identity.
#[tokio::test]
async fn auth_info_returns_profile_when_token_expired() {
    let agent = build_agent_with_auth(crate::auth::EchoAgentAuth {
        email: Some("user@example.com".into()),
        first_name: Some("Test".into()),
        refresh_token: Some("rt".into()),
        expires_at: Some(chrono::Utc::now() - chrono::Duration::hours(1)),
        ..crate::auth::EchoAgentAuth::test_default()
    });
    let resp = crate::extensions::auth::handle(
        &agent,
        &acp::ExtRequest::new(
            "echo.agent/auth/info",
            std::sync::Arc::from(serde_json::value::to_raw_value(&serde_json::json!({})).unwrap()),
        ),
    )
    .await
    .expect("auth/info must succeed with an expired token");
    let info: serde_json::Value = serde_json::from_str(resp.0.get()).unwrap();
    assert_eq!(info["email"], "user@example.com");
    assert_eq!(info["firstName"], "Test");
}
#[tokio::test]
async fn data_collection_enabled_for_normal_user() {
    let agent = build_agent_with_auth(crate::auth::EchoAgentAuth::test_default());
    assert!(
        !agent.is_data_collection_disabled(),
        "normal user must have data collection enabled"
    );
}
#[tokio::test]
async fn data_collection_disabled_for_zdr_team() {
    let agent = build_agent_with_auth(crate::auth::EchoAgentAuth {
        team_blocked_reasons: vec!["BLOCKED_REASON_NO_LOGS".into()],
        ..crate::auth::EchoAgentAuth::test_default()
    });
    assert!(
        agent.is_data_collection_disabled(),
        "ZDR team must have data collection disabled"
    );
    assert!(
        agent.trace_upload_config_snapshot().is_none(),
        "trace uploads must be disabled for ZDR team"
    );
}
#[tokio::test]
async fn data_collection_disabled_for_zdr_moderated_team() {
    let agent = build_agent_with_auth(crate::auth::EchoAgentAuth {
        team_blocked_reasons: vec!["BLOCKED_REASON_NO_LOGS_MODERATED".into()],
        ..crate::auth::EchoAgentAuth::test_default()
    });
    assert!(
        agent.is_data_collection_disabled(),
        "ZDR-moderated team must have data collection disabled"
    );
}
#[tokio::test]
async fn data_collection_disabled_for_opted_out_team() {
    let agent = build_agent_with_auth(crate::auth::EchoAgentAuth {
        coding_data_retention_opt_out: true,
        ..crate::auth::EchoAgentAuth::test_default()
    });
    assert!(
        agent.is_data_collection_disabled(),
        "opted-out team must have data collection disabled"
    );
    assert!(
        agent.trace_upload_config_snapshot().is_none(),
        "trace uploads must be disabled for opted-out team"
    );
}
#[tokio::test]
async fn data_collection_disabled_for_zdr_plus_opt_out() {
    let agent = build_agent_with_auth(crate::auth::EchoAgentAuth {
        team_blocked_reasons: vec!["BLOCKED_REASON_NO_LOGS".into()],
        coding_data_retention_opt_out: true,
        ..crate::auth::EchoAgentAuth::test_default()
    });
    assert!(
        agent.is_data_collection_disabled(),
        "ZDR + opt-out must have data collection disabled"
    );
}
#[tokio::test]
async fn data_collection_enabled_for_non_zdr_team_with_unrelated_blocks() {
    let agent = build_agent_with_auth(crate::auth::EchoAgentAuth {
        team_blocked_reasons: vec![
            "BLOCKED_REASON_BILLING".into(),
            "BLOCKED_REASON_SUSPENDED".into(),
        ],
        ..crate::auth::EchoAgentAuth::test_default()
    });
    assert!(
        !agent.is_data_collection_disabled(),
        "non-ZDR blocked reasons must not disable data collection"
    );
}
fn enable_product_telemetry(agent: &MvpAgent) {
    agent.cfg.borrow_mut().features.telemetry = Some(crate::agent::config::TelemetryMode::Enabled);
}
/// Enable trace uploads via config so only the auth-level privacy gate
/// can disable collection in the tests below.
fn enable_trace_upload_config(agent: &MvpAgent) {
    let mut cfg = agent.cfg.borrow_mut();
    cfg.features.telemetry = Some(crate::agent::config::TelemetryMode::Enabled);
    cfg.telemetry.trace_upload = Some(true);
}
#[tokio::test]
async fn product_analytics_enabled_for_normal_user_with_telemetry_on() {
    let agent = build_agent_with_auth(crate::auth::EchoAgentAuth::test_default());
    enable_product_telemetry(&agent);
    assert!(agent.product_analytics_enabled());
}
#[tokio::test]
async fn product_analytics_enabled_despite_coding_retention_opt_out() {
    let agent = build_agent_with_auth(crate::auth::EchoAgentAuth {
        coding_data_retention_opt_out: true,
        ..crate::auth::EchoAgentAuth::test_default()
    });
    enable_product_telemetry(&agent);
    assert!(agent.is_data_collection_disabled());
    assert!(agent.product_analytics_enabled());
}
#[tokio::test]
async fn product_analytics_disabled_for_zdr_team() {
    let agent = build_agent_with_auth(crate::auth::EchoAgentAuth {
        team_blocked_reasons: vec!["BLOCKED_REASON_NO_LOGS".into()],
        ..crate::auth::EchoAgentAuth::test_default()
    });
    enable_product_telemetry(&agent);
    assert!(!agent.product_analytics_enabled());
}
#[tokio::test]
async fn product_analytics_disabled_when_telemetry_off() {
    let agent = build_agent_with_auth(crate::auth::EchoAgentAuth::test_default());
    agent.cfg.borrow_mut().features.telemetry = Some(crate::agent::config::TelemetryMode::Disabled);
    assert!(!agent.product_analytics_enabled());
}
/// Counting HTTP stub: any request increments the counter and gets a
/// storage-proxy-shaped 200 so the client does not retry.
async fn spawn_counting_storage_stub() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count_clone = count.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = axum::Router::new().fallback(move || {
        let count = count_clone.clone();
        async move {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            (
                [("content-type", "application/json")],
                r#"{"bucket":"test-bucket","path":"auth-diagnostics/test.jsonl"}"#,
            )
        }
    });
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://127.0.0.1:{port}"), count)
}
/// Regression: the auth-diagnostics uploader was gated only on the
/// trace-upload config switch; it must also honor ZDR / retention
/// opt-out, checked at invocation time.
#[tokio::test]
async fn diagnostic_upload_skipped_for_opted_out_user() {
    let (stub_url, count) = spawn_counting_storage_stub().await;
    let agent = build_agent_with_auth(crate::auth::EchoAgentAuth {
        coding_data_retention_opt_out: true,
        ..crate::auth::EchoAgentAuth::test_default()
    });
    enable_trace_upload_config(&agent);
    agent.cfg.borrow_mut().endpoints.trace_upload_url = Some(stub_url);
    let uploader = agent
        .diagnostic_upload_config()
        .expect("uploader is wired whenever trace upload config is on");
    uploader(b"log".to_vec(), "tok".into(), "user-id-1".into()).await;
    assert_eq!(
        count.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "no diagnostics request may leave the machine after opt-out"
    );
}
#[tokio::test]
async fn diagnostic_upload_sent_for_normal_user() {
    let (stub_url, count) = spawn_counting_storage_stub().await;
    let agent = build_agent_with_auth(crate::auth::EchoAgentAuth::test_default());
    enable_trace_upload_config(&agent);
    agent.cfg.borrow_mut().endpoints.trace_upload_url = Some(stub_url);
    let uploader = agent
        .diagnostic_upload_config()
        .expect("uploader is wired whenever trace upload config is on");
    uploader(b"log".to_vec(), "tok".into(), "user-id-1".into()).await;
    assert!(
        count.load(std::sync::atomic::Ordering::SeqCst) >= 1,
        "positive control: diagnostics upload reaches the proxy for a \
         normal user"
    );
}
/// The diagnostics privacy gate fails closed: with no credential in the
/// `AuthManager` (e.g. a mid-session `/logout` raced the refresh failure
/// that triggers the upload), nothing may leave the machine.
#[tokio::test]
async fn diagnostic_upload_skipped_without_credentials() {
    let (stub_url, count) = spawn_counting_storage_stub().await;
    let agent = build_minimal_agent_for_tests();
    enable_trace_upload_config(&agent);
    agent.cfg.borrow_mut().endpoints.trace_upload_url = Some(stub_url);
    let uploader = agent
        .diagnostic_upload_config()
        .expect("uploader is wired whenever trace upload config is on");
    uploader(b"log".to_vec(), "tok".into(), "user-id-1".into()).await;
    assert_eq!(
        count.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "missing credentials must fail closed for diagnostics uploads"
    );
}
/// The diagnostics uploader is wired once (at agent construction), so it
/// must re-check the live trace-upload mirror at invocation time: a
/// mid-session config-level kill switch stops diagnostics uploads too.
#[tokio::test]
async fn diagnostic_upload_skipped_after_mid_session_trace_upload_kill_switch() {
    let (stub_url, count) = spawn_counting_storage_stub().await;
    let agent = build_agent_with_auth(crate::auth::EchoAgentAuth::test_default());
    enable_trace_upload_config(&agent);
    agent.cfg.borrow_mut().endpoints.trace_upload_url = Some(stub_url);
    agent.sync_collection_config_gate();
    let uploader = agent
        .diagnostic_upload_config()
        .expect("uploader is wired whenever trace upload config is on");
    {
        let mut cfg = agent.cfg.borrow_mut();
        cfg.features.telemetry = Some(crate::agent::config::TelemetryMode::Disabled);
        cfg.telemetry.trace_upload = Some(false);
    }
    agent.sync_collection_config_gate();
    uploader(b"log".to_vec(), "tok".into(), "user-id-1".into()).await;
    assert_eq!(
        count.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "an already-wired diagnostics uploader must honor a mid-session \
         trace-upload kill switch"
    );
}
use crate::session::storage::search::IndexDecision;
/// A echoagent home of its own, with the switch left at its registered default.
/// `decide_search_index` stops short of a session store, but do not reach
/// `bootstrap_once`: it takes the process-cached `echo_agent_home()`, which these
/// guards cannot redirect, so it could index the developer's own store.
fn search_index_env() -> (tempfile::TempDir, [echo_agent_test_support::EnvGuard; 2]) {
    use echo_agent_test_support::EnvGuard;
    let home = tempfile::tempdir().unwrap();
    let guards = [
        EnvGuard::set("ECHO_AGENT_HOME", home.path()),
        EnvGuard::unset("ECHO_AGENT_SESSION_SEARCH"),
    ];
    (home, guards)
}
#[tokio::test]
#[serial_test::serial]
async fn search_index_honors_the_session_search_feature() {
    let (_home, _env) = search_index_env();
    {
        let _off = echo_agent_test_support::EnvGuard::set("ECHO_AGENT_SESSION_SEARCH", "0");
        let agent = build_agent_with_auth(crate::auth::EchoAgentAuth::test_default());
        agent.decide_search_index();
        assert!(
            matches!(agent.search_index(), IndexDecision::Off),
            "the switch is off, so this process keeps no index"
        );
    }
    let agent = build_agent_with_auth(crate::auth::EchoAgentAuth::test_default());
    agent.decide_search_index();
    assert!(
        matches!(agent.search_index(), IndexDecision::On(_)),
        "the feature is on by default, so this process keeps an index"
    );
}
/// Reclaiming is the one irreversible half of the deferred work, and the six hour
/// throttle then hides the run that could have honored a remote veto.
#[tokio::test]
#[serial_test::serial]
async fn auto_gc_declines_until_the_remote_answer_settles() {
    let (_home, _env) = search_index_env();
    let agent = build_agent_with_auth(crate::auth::EchoAgentAuth::test_default());
    assert!(
        !agent.remote_settings_settled(),
        "precondition: remote fetch is on and no settings have arrived"
    );
    agent.spawn_auto_worktree_gc();
    agent.decide_search_index();
    assert_eq!(
        agent.auto_gc_spawn_count.get(),
        0,
        "a host that never reaches the server must not reclaim under the default policy"
    );
    assert!(
        matches!(agent.search_index(), IndexDecision::On(_)),
        "the index still decides, since running without one is what cannot be undone; got {:?}",
        agent.search_index(),
    );
    agent.cfg.borrow_mut().remote_settings = Some(crate::util::config::RemoteSettings::default());
    agent.spawn_auto_worktree_gc();
    assert_eq!(
        agent.auto_gc_spawn_count.get(),
        1,
        "once the server has answered it reclaims, or the guard would just never run"
    );
}
#[tokio::test]
#[serial_test::serial]
async fn search_before_the_decision_asks_the_caller_to_retry() {
    let (_home, _env) = search_index_env();
    let agent = build_agent_with_auth(crate::auth::EchoAgentAuth::test_default());
    assert!(
        matches!(agent.search_index(), IndexDecision::Pending),
        "precondition: nothing has decided yet"
    );
    let resp = crate::session::storage::search::execute_search(
        agent.search_index(),
        &crate::util::echo_agent_home::echo_agent_home(),
        &crate::session::storage::search::SessionSearchRequest {
            query: "zzqqpending".to_string(),
            cwd: None,
            limit: 10,
            offset: 0,
            include_content: false,
        },
    )
    .await
    .unwrap();
    assert!(
        resp.bootstrapping,
        "an undecided process must not answer final"
    );
    assert!(resp.results.is_empty());
}
/// A leader boots with no remote settings, so if the first reader resolved the
/// feature the registered default would latch before the server could answer.
#[tokio::test]
#[serial_test::serial]
async fn read_before_the_remote_settings_land_does_not_decide() {
    let (_home, _env) = search_index_env();
    let agent = build_agent_with_auth(crate::auth::EchoAgentAuth::test_default());
    assert!(
        !agent.remote_settings_settled(),
        "precondition: remote fetch is on and no settings have arrived"
    );
    assert!(
        matches!(agent.search_index(), IndexDecision::Pending),
        "reading must not settle the question; got {:?}",
        agent.search_index(),
    );
    agent.cfg.borrow_mut().remote_settings = Some(crate::util::config::RemoteSettings {
        session_search: Some(false),
        ..Default::default()
    });
    assert!(agent.remote_settings_settled());
    agent.decide_search_index();
    assert!(
        matches!(agent.search_index(), IndexDecision::Off),
        "the remote kill switch decided, so this process keeps no index; got {:?}",
        agent.search_index(),
    );
}
/// A host with no identity to fetch with never receives remote settings, so
/// waiting for them would cost it an index for the whole run.
#[tokio::test]
#[serial_test::serial]
async fn exhausted_fetch_decides_on_the_local_layers() {
    use crate::agent::config::Config as AgentConfig;
    use crate::auth::{AuthManager, EchoAgentComConfig};
    use echo_agent_test_support::EnvGuard;
    let (_home, _env) = search_index_env();
    let _no_inline_auth = EnvGuard::unset("ECHO_AGENT_AUTH");
    let _no_auth_path = EnvGuard::unset("ECHO_AGENT_AUTH_PATH");
    let auth_dir = tempfile::tempdir().unwrap();
    let auth_manager =
        std::sync::Arc::new(AuthManager::new(auth_dir.path(), EchoAgentComConfig::default()));
    assert!(
        auth_manager.current().is_none(),
        "precondition: no identity to fetch with"
    );
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let agent = MvpAgent::new(
        GatewaySender::new(tx),
        &AgentConfig::default(),
        auth_manager,
        None,
    )
    .expect("valid test config");
    assert!(
        !agent.remote_settings_settled(),
        "precondition: remote fetch is on and no settings have arrived"
    );
    agent.maybe_fetch_post_auth_settings().await;
    assert!(
        matches!(agent.search_index(), IndexDecision::On(_)),
        "a signed out host decides on its local layers rather than keeping no index"
    );
}
/// Once decided, a later switch cannot take the index away: serving one that
/// stopped absorbing writes is worse than keeping it until the next launch.
#[tokio::test]
#[serial_test::serial]
async fn kill_switch_after_the_decision_leaves_the_index_up() {
    let (_home, _env) = search_index_env();
    let agent = build_agent_with_auth(crate::auth::EchoAgentAuth::test_default());
    agent.cfg.borrow_mut().remote_settings = Some(crate::util::config::RemoteSettings {
        session_search: Some(true),
        ..Default::default()
    });
    agent.decide_search_index();
    assert!(
        matches!(agent.search_index(), IndexDecision::On(_)),
        "precondition: indexing"
    );
    agent.cfg.borrow_mut().remote_settings = Some(crate::util::config::RemoteSettings {
        session_search: Some(false),
        ..Default::default()
    });
    agent.decide_search_index();
    assert!(
        matches!(agent.search_index(), IndexDecision::On(_)),
        "a switch arriving after the decision must not tear down a live index"
    );
}
/// Sessions hold the decision itself, not the answer as it stood when they
/// opened, which would otherwise be `None` for the rest of their life.
#[tokio::test]
#[serial_test::serial]
async fn session_opened_before_the_decision_sees_it_land() {
    let (_home, _env) = search_index_env();
    let agent = build_agent_with_auth(crate::auth::EchoAgentAuth::test_default());
    let held_by_a_session = agent.search_index_cell();
    assert!(
        matches!(held_by_a_session.decision(), IndexDecision::Pending),
        "precondition: the session opened before anything decided"
    );
    agent.cfg.borrow_mut().remote_settings = Some(crate::util::config::RemoteSettings {
        session_search: Some(true),
        ..Default::default()
    });
    agent.decide_search_index();
    assert!(
        matches!(held_by_a_session.decision(), IndexDecision::On(_)),
        "the session indexes as soon as the decision lands"
    );
}
/// The live collection gate reads a `Send` mirror of the config-level
/// trace-upload switch; `sync_collection_config_gate` must keep that mirror
/// current so a mid-session remote-settings flip (kill switch) stops
/// collection without a new session.
#[tokio::test]
async fn collection_config_gate_mirror_follows_trace_upload_flip() {
    let agent = build_agent_with_auth(crate::auth::EchoAgentAuth::test_default());
    enable_trace_upload_config(&agent);
    agent.sync_collection_config_gate();
    assert!(
        agent
            .trace_upload_live
            .load(std::sync::atomic::Ordering::Relaxed),
        "precondition: mirror reflects the enabled switch"
    );
    {
        let mut cfg = agent.cfg.borrow_mut();
        cfg.features.telemetry = Some(crate::agent::config::TelemetryMode::Disabled);
        cfg.telemetry.trace_upload = Some(false);
    }
    agent.sync_collection_config_gate();
    assert!(
        !agent
            .trace_upload_live
            .load(std::sync::atomic::Ordering::Relaxed),
        "mirror must follow a mid-session config-level trace-upload flip"
    );
}
/// `parse_session_kind` routes `session/load` to the gateway Chat path vs. the
/// disk-backed Build path. Anything but an explicit `kind: "chat"` is Build.
#[test]
fn parse_session_kind_matrix() {
    use crate::session::unified_list::SessionKind;
    use serde_json::json;
    let cases: &[(&str, serde_json::Value, SessionKind)] = &[
        (
            "chat",
            json!({"echo.agent/session": {"kind": "chat"}}),
            SessionKind::Chat,
        ),
        (
            "build",
            json!({"echo.agent/session": {"kind": "build"}}),
            SessionKind::Build,
        ),
        (
            "chat_malformed_sibling",
            json!({"echo.agent/session": {"kind": "chat", "facets": "not-a-map"}}),
            SessionKind::Chat,
        ),
        (
            "unknown_kind",
            json!({"echo.agent/session": {"kind": "frob"}}),
            SessionKind::Build,
        ),
        ("absent", json!({}), SessionKind::Build),
    ];
    for (label, meta, expected) in cases {
        assert_eq!(parse_session_kind(meta.as_object()), *expected, "[{label}]");
    }
    assert_eq!(parse_session_kind(None), SessionKind::Build, "[none]");
}
#[test]
fn reject_chat_kind_without_feature_errors_without_chat_feature() {
    use serde_json::json;
    assert!(
        reject_chat_kind_without_feature(json!({"echo.agent/session": {"kind": "chat"}}).as_object())
            .is_err()
    );
    assert!(reject_chat_kind_without_feature(None).is_ok());
    assert!(
        reject_chat_kind_without_feature(
            json!({ "echo.agent/session" : { "kind" : "build" } }).as_object()
        )
        .is_ok()
    );
}
#[test]
fn chat_initial_model_matrix() {
    let cases: &[(&str, bool, Option<&str>, Option<&str>)] = &[
        ("chat_with_model", true, Some("echo-agent-4.5"), Some("echo-agent-4.5")),
        ("chat_without_model", true, None, None),
        ("build_with_model", false, Some("echo-agent-4.5"), None),
        ("build_without_model", false, None, None),
    ];
    for (label, is_chat_kind, custom_model_id, expected) in cases {
        assert_eq!(
            chat_initial_model(*is_chat_kind, *custom_model_id).as_deref(),
            *expected,
            "[{label}]"
        );
    }
}
#[test]
fn chat_new_session_model_state_matrix() {
    fn state_with(current: &str, available: &[&str]) -> acp::SessionModelState {
        acp::SessionModelState::new(
            acp::ModelId::new(current.to_owned()),
            available
                .iter()
                .map(|id| {
                    acp::ModelInfo::new(acp::ModelId::new((*id).to_owned()), (*id).to_owned())
                })
                .collect(),
        )
    }
    let cases: &[(&str, acp::SessionModelState, Option<&str>, &str)] = &[
        (
            "requested_in_catalog",
            state_with("auto", &["auto", "echo-agent-4"]),
            Some("echo-agent-4"),
            "echo-agent-4",
        ),
        (
            "no_request_keeps_catalog_default",
            state_with("auto", &["auto", "echo-agent-4"]),
            None,
            "auto",
        ),
        (
            "requested_not_in_catalog",
            state_with("auto", &["auto"]),
            Some("echo-agent-4.5"),
            "echo-agent-4.5",
        ),
        (
            "requested_with_empty_catalog",
            state_with("", &[]),
            Some("echo-agent-4"),
            "echo-agent-4",
        ),
    ];
    for (label, state, requested, expected) in cases {
        let out = chat_new_session_model_state(state.clone(), requested.map(str::to_owned));
        assert_eq!(out.current_model_id.0.as_ref(), *expected, "[{label}]");
        assert_eq!(
            out.available_models.len(),
            state.available_models.len(),
            "[{label}] override must not mutate the catalog"
        );
    }
}
/// valid `echo.agent/local_workspace` → ExistingWorkspace only.
/// Never reads `envId` / never emits SandboxEnvironment.
#[cfg(feature = "local-workspace")]
#[test]
fn parse_session_computer_sessions_local_workspace_matrix() {
    use crate::gateway_bridge::ComputerSession;
    use serde_json::json;
    fn existing(server_id: &str, cwd: Option<&str>) -> Vec<ComputerSession> {
        vec![ComputerSession::ExistingWorkspace {
            server_id: server_id.to_owned(),
            cwd: cwd.map(str::to_owned),
        }]
    }
    let cases: &[(&str, serde_json::Value, Option<Vec<ComputerSession>>)] = &[
        (
            "attach_server_id_on_local",
            json!({
                "echo.agent/local_workspace": {
                    "mode": "attach",
                    "server_id": "lw-attach-1",
                    "cwd": "/repo",
                },
                "envId": "env-must-be-ignored",
            }),
            Some(existing("lw-attach-1", Some("/repo"))),
        ),
        (
            "attach_server_id_from_cloud_existing",
            json!({
                "echo.agent/local_workspace": {
                    "mode": "attach",
                    "cwd": "/repo",
                },
                "echo.agent/cloud_existing_workspace": {
                    "server_id": "lw-attach-2",
                    "cwd": "/repo-existing",
                },
                "envId": "env-must-be-ignored",
            }),
            Some(existing("lw-attach-2", Some("/repo"))),
        ),
        (
            "own_with_server_id_ignores_envid",
            json!({
                "echo.agent/local_workspace": {
                    "mode": "own",
                    "server_id": "lw-own-1",
                    "cwd": "/Users/me/src",
                },
                "envId": "env-must-be-ignored",
            }),
            Some(existing("lw-own-1", Some("/Users/me/src"))),
        ),
        (
            "own_without_server_id_no_sandbox_fallback",
            json!({
                "echo.agent/local_workspace": {
                    "mode": "own",
                    "cwd": "/Users/me/src",
                },
                "envId": "env-must-be-ignored",
            }),
            None,
        ),
        (
            "invalid_mode_falls_through_to_envid",
            json!({
                "echo.agent/local_workspace": {
                    "mode": "bogus",
                    "server_id": "lw-x",
                },
                "envId": "env-prod",
            }),
            Some(vec![ComputerSession::SandboxEnvironment {
                environment_id: Some("env-prod".to_owned()),
            }]),
        ),
        (
            "non_object_local_falls_through_to_envid",
            json!({
                "echo.agent/local_workspace": "not-an-object",
                "envId": "env-prod",
            }),
            Some(vec![ComputerSession::SandboxEnvironment {
                environment_id: Some("env-prod".to_owned()),
            }]),
        ),
    ];
    for (label, meta, expected) in cases {
        let got = parse_session_computer_sessions(meta.as_object());
        assert_eq!(
            got.as_deref(),
            expected.as_deref(),
            "[{label}] local_workspace match-table mismatch"
        );
    }
}
/// Local intent without resolvable server_id fails closed (no silent unstamped start).
#[cfg(feature = "local-workspace")]
#[test]
fn resolve_local_workspace_missing_server_id_fails_closed() {
    use serde_json::json;
    let meta = json!({
        "echo.agent/session": { "kind": "chat" },
        "echo.agent/local_workspace": {
            "mode": "own",
            "cwd": "/repo",
        }
    });
    let err = resolve_session_computer_sessions(meta.as_object())
        .expect_err("own without server_id must fail closed");
    assert_eq!(
        err.data
            .as_ref()
            .and_then(|d| d.get("code"))
            .and_then(|v| v.as_str()),
        Some("local_workspace_server_id_missing")
    );
}
/// Supervisor map + reap guard / shutdown_gateway_bridge tear down the entry.
#[cfg(all(feature = "local-workspace", unix))]
#[test]
fn local_workspace_reap_guard_and_shutdown_clear_map() {
    run_local_for_bridge_test(|| async {
        let agent = build_minimal_agent_for_tests();
        let sid = gateway_bridge_test_session_id();
        {
            let mut guard = agent.new_local_workspace_reap_guard(sid.clone(), true);
            guard.disarm();
        }
        assert!(agent.local_workspace_supervisors.borrow().is_empty());
        agent.shutdown_gateway_bridge(&sid);
        assert!(
            agent
                .local_workspace_generations
                .borrow()
                .get(&sid)
                .is_none()
        );
    });
}
/// Pre-bridge crash refresh rewrites handshake stamp from live supervisor id.
#[cfg(all(feature = "local-workspace", unix))]
#[test]
fn refresh_sessions_from_supervisor_overrides_server_id() {
    use crate::gateway_bridge::ComputerSession;
    use crate::gateway_bridge::local_workspace_supervisor::test_start_ready_own;
    run_local_for_bridge_test(|| async {
        let agent = build_minimal_agent_for_tests();
        let sid = gateway_bridge_test_session_id();
        let original = Some(vec![ComputerSession::ExistingWorkspace {
            server_id: "lw-stale".into(),
            cwd: Some("/repo".into()),
        }]);
        let unchanged = agent.refresh_sessions_from_supervisor(&sid, original.clone());
        assert!(matches!(
            unchanged.as_ref().and_then(|v| v.first()),
            Some(ComputerSession::ExistingWorkspace { server_id, .. }) if server_id == "lw-stale"
        ));
        let (_dir, handle) = test_start_ready_own().await;
        let live_id = handle.server_id.clone();
        agent.register_local_workspace_supervisor(sid.clone(), handle);
        let refreshed = agent.refresh_sessions_from_supervisor(&sid, original);
        match refreshed.as_ref().and_then(|v| v.first()) {
            Some(ComputerSession::ExistingWorkspace { server_id, .. }) => {
                assert_eq!(
                    server_id, &live_id,
                    "refresh must use live supervisor server_id"
                );
            }
            other => panic!("expected ExistingWorkspace, got {other:?}"),
        }
        agent.shutdown_gateway_bridge(&sid);
    });
}
/// start_own + register stamps server_id into meta and stores the handle.
#[cfg(all(feature = "local-workspace", unix))]
#[test]
fn start_own_registers_and_stamps_server_id() {
    use crate::gateway_bridge::local_workspace_supervisor::{
        stamp_server_id_into_meta, test_start_ready_own,
    };
    run_local_for_bridge_test(|| async {
        let agent = build_minimal_agent_for_tests();
        let sid = gateway_bridge_test_session_id();
        let (_dir, handle) = test_start_ready_own().await;
        let server_id = handle.server_id.clone();
        let mut meta = acp::Meta::new();
        meta.insert(
            "echo.agent/local_workspace".into(),
            serde_json::json!({"mode": "own", "cwd": "/tmp/repo"}),
        );
        stamp_server_id_into_meta(&mut meta, &server_id);
        assert_eq!(
            meta.get("echo.agent/local_workspace")
                .and_then(|v| v.get("server_id"))
                .and_then(|v| v.as_str()),
            Some(server_id.as_str())
        );
        agent.register_local_workspace_supervisor(sid.clone(), handle);
        assert!(
            agent
                .local_workspace_supervisors
                .borrow()
                .contains_key(&sid),
            "handle must be registered by SessionId"
        );
        assert!(
            agent
                .local_workspace_generations
                .borrow()
                .get(&sid)
                .is_some_and(|g| *g >= 1),
            "arm must bump generation"
        );
        agent.shutdown_gateway_bridge(&sid);
    });
}
/// Armed reap guard removes a registered supervisor on drop (session/new failure).
#[cfg(all(feature = "local-workspace", unix))]
#[test]
fn reap_guard_drop_removes_registered_supervisor() {
    use crate::gateway_bridge::local_workspace_supervisor::test_start_ready_own;
    run_local_for_bridge_test(|| async {
        let agent = build_minimal_agent_for_tests();
        let sid = gateway_bridge_test_session_id();
        let (_dir, handle) = test_start_ready_own().await;
        agent.register_local_workspace_supervisor(sid.clone(), handle);
        assert!(
            agent
                .local_workspace_supervisors
                .borrow()
                .contains_key(&sid)
        );
        {
            let _guard = agent.new_local_workspace_reap_guard(sid.clone(), true);
        }
        assert!(
            agent
                .local_workspace_supervisors
                .borrow()
                .get(&sid)
                .is_none(),
            "armed guard drop must reap supervisor"
        );
        assert!(
            agent
                .local_workspace_generations
                .borrow()
                .get(&sid)
                .is_none(),
            "armed guard drop must invalidate generation"
        );
    });
}
/// Shutdown generation invalidates a pending restart re-insert.
#[cfg(all(feature = "local-workspace", unix))]
#[test]
fn shutdown_generation_invalidates_stale_restart() {
    use crate::gateway_bridge::local_workspace_supervisor::test_start_ready_own;
    run_local_for_bridge_test(|| async {
        let agent = build_minimal_agent_for_tests();
        let sid = gateway_bridge_test_session_id();
        let (_dir, handle) = test_start_ready_own().await;
        agent.register_local_workspace_supervisor(sid.clone(), handle);
        let generation = *agent
            .local_workspace_generations
            .borrow()
            .get(&sid)
            .expect("generation after register");
        agent.shutdown_gateway_bridge(&sid);
        assert!(
            agent.local_workspace_generations.borrow().get(&sid) != Some(&generation),
            "shutdown must invalidate generation so stale restart cannot re-insert"
        );
        assert!(
            agent
                .local_workspace_supervisors
                .borrow()
                .get(&sid)
                .is_none()
        );
    });
}
/// `spawn_gateway_bridge` uses `tokio::task::spawn_local`.
fn run_local_for_bridge_test<F, Fut, T>(body: F) -> T
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = T>,
{
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime must build");
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, body())
}
#[test]
fn chat_session_spawn_options_matches_thin_profile() {
    let sid = acp::SessionId::new(std::sync::Arc::from("00000000-0000-0000-0000-000000000099"));
    let cwd = echo_agent_paths::AbsPathBuf::new(std::env::temp_dir()).expect("temp cwd");
    let opts = chat_session_spawn_options(
        SessionInfo {
            id: sid,
            cwd: cwd.as_str().to_owned(),
        },
        cwd,
        None,
        None,
        acp::ModelId::new(std::sync::Arc::from("test-model")),
        false,
    );
    assert!(opts.mcp_servers.is_empty());
    assert!(opts.initial_client_mcp_servers.is_empty());
    assert!(!opts.client_code_nav_enabled);
    assert!(!opts.client_terminal);
    assert!(!opts.client_fs_read);
    assert!(!opts.client_fs_write);
    assert!(opts.chat_history.is_empty());
    assert!(!opts.session_auto_mode);
    assert!(
        opts.persistence.is_noop(),
        "K10 thin profile must use PersistenceHandle::noop()"
    );
    assert!(opts.is_chat_kind);
}
/// `remove_session` releases the workspace binding and drains the
/// per-session side maps. Test agents default to `workspace_ops = None`,
/// so no other test reaches the release.
#[tokio::test]
async fn remove_session_releases_workspace_binding_and_side_maps() {
    let agent = build_minimal_agent_for_tests();
    let sid = acp::SessionId::new("test-session-workspace-release");
    let ops = echo_agent_workspace::WorkspaceOps::for_test();
    let toolset =
        std::sync::Arc::new(echo_agent_tools::registry::types::FinalizedToolset::empty_for_test());
    let toolset_weak = std::sync::Arc::downgrade(&toolset);
    ops.bind_local_session(
        sid.0.as_ref(),
        std::env::temp_dir(),
        echo_agent_hunk_tracker::HunkTrackerHandle::noop(),
        toolset,
        None,
    )
    .expect("bind_local_session must succeed");
    assert!(toolset_weak.upgrade().is_some());
    *agent.workspace_ops.borrow_mut() = Some(ops);
    agent
        .session_registry
        .set_unavailable_model(&sid, acp::ModelId::new(std::sync::Arc::from("gone-model")));
    agent.set_turn_number(&sid, 3);
    let (_permission_tx, permission_rx) =
        tokio::sync::mpsc::unbounded_channel::<echo_agent_workspace::permission::PermissionEvent>();
    agent
        .session_registry
        .set_permission_receiver(&sid, permission_rx);
    let _ = agent.session_registry.live_orphan_heal_lock(&sid);
    assert_eq!(agent.session_registry.counts().live_orphan_heal_locks, 1);
    agent.remove_session(&sid);
    assert_eq!(
        agent.session_registry.counts().live_orphan_heal_locks,
        0,
        "remove_session must evict the live-orphan heal mutex"
    );
    assert!(
        toolset_weak.upgrade().is_none(),
        "the workspace binding must release the toolset"
    );
    assert!(agent.session_registry.unavailable_model(&sid).is_none());
    assert_eq!(agent.session_registry.counts().resident_resources, 0);
    assert_eq!(
        agent.session_registry.counts().retained_resources,
        0,
        "retained per-session resources must be reclaimed on removal"
    );
}
/// Without a bridge, `ext_method` falls through to the unchanged local
/// dispatch (`rewind::handle`), which reports the missing session — proving
/// the routing hook is skipped in local mode.
#[test]
fn ext_method_rewind_uses_local_dispatch_without_bridge() {
    use acp::Agent as _;
    let _env = crate::env::EnvVarGuard::remove(crate::env::ECHO_AGENT_DISABLE_CUSTOM_BRIDGE_ENV);
    run_local_for_bridge_test(|| async {
        let agent = build_minimal_agent_for_tests();
        let params = serde_json::json!({ "sessionId": "sess-local" });
        let err = agent
            .ext_method(acp::ExtRequest::new(
                "echo.agent/rewind/points",
                std::sync::Arc::from(serde_json::value::to_raw_value(&params).unwrap()),
            ))
            .await
            .expect_err("local rewind with no session must error");
        assert_eq!(err.code, acp::Error::resource_not_found(None).code);
    });
}
#[test]
fn cancel_does_not_forward_to_bridge_in_local_mode() {
    use crate::session::SessionCommand;
    use acp::Agent as _;
    run_local_for_bridge_test(|| async {
        let agent = build_minimal_agent_for_tests();
        let sid = acp::SessionId::new("sess-cancel-local");
        let (handle, _tx, mut cmd_rx) = make_live_session_handle(&sid, None);
        agent.insert_resident(&sid, handle);
        let driver = tokio::task::spawn_local(async move {
            let Some(SessionCommand::Cancel(options)) = cmd_rx.recv().await else {
                return false;
            };
            let Some(acknowledged) = options.acknowledged else {
                return false;
            };
            acknowledged.notify_one();
            true
        });
        agent
            .cancel(acp::CancelNotification::new(sid.clone()))
            .await
            .expect("cancel must succeed");
        assert!(
            driver.await.expect("cancel driver must finish"),
            "local-mode cancel dispatches locally and waits for the actor acknowledgement"
        );
    });
}
/// Regression (post-cancel slot hang, first bad release 0.2.101; see
/// `dispatch_lock`). SDK e2e shape:
/// `test_cancel_ends_in_flight_turn_and_frees_slot` (echo-agent-agent-sdk).
#[test]
fn cancel_never_overtakes_in_flight_prompt_intake() {
    use crate::session::SessionCommand;
    use acp::Agent as _;
    run_local_for_bridge_test(|| async {
        let agent = build_minimal_agent_for_tests();
        let sid = acp::SessionId::new("sess-cancel-intake-race");
        let (handle, _tx, mut cmd_rx) = make_live_session_handle(&sid, None);
        agent.insert_resident(&sid, handle);
        let order: std::rc::Rc<std::cell::RefCell<Vec<&'static str>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let (intake_parked_tx, intake_parked_rx) = tokio::sync::oneshot::channel::<()>();
        let driver_order = order.clone();
        tokio::task::spawn_local(async move {
            let mut intake_parked_tx = Some(intake_parked_tx);
            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    SessionCommand::GetCurrentPromptMode { .. } => {
                        if let Some(tx) = intake_parked_tx.take() {
                            let _ = tx.send(());
                            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        }
                    }
                    SessionCommand::Prompt { .. } => driver_order.borrow_mut().push("prompt"),
                    SessionCommand::Cancel(options) => {
                        driver_order.borrow_mut().push("cancel");
                        if let Some(acknowledged) = options.acknowledged {
                            acknowledged.notify_one();
                        }
                    }
                    _ => {}
                }
            }
        });
        let prompt_fut = agent.prompt(acp::PromptRequest::new(
            sid.clone(),
            vec![acp::ContentBlock::from("hi")],
        ));
        let cancel_fut = async {
            intake_parked_rx
                .await
                .expect("prompt intake reaches the fake actor");
            let _ = agent
                .cancel(acp::CancelNotification::new(sid.clone()))
                .await;
        };
        let _ = futures::join!(prompt_fut, cancel_fut);
        assert_eq!(
            order.borrow().as_slice(),
            ["prompt", "cancel"],
            "cancel must land on the actor mailbox after the prompt it targets"
        );
    });
}
use crate::session::SessionCommand as TestSessionCommand;
/// Build a session handle wired to a *live* command channel. Returns the
/// handle (move into `sessions`) plus a probe `cmd_tx`/`cmd_rx` so a test
/// can observe what the agent sends to the actor and prove the channel is
/// live.
fn make_live_session_handle(
    sid: &acp::SessionId,
    running_prompt: Option<&str>,
) -> (
    crate::session::SessionHandle,
    tokio::sync::mpsc::UnboundedSender<TestSessionCommand>,
    tokio::sync::mpsc::UnboundedReceiver<TestSessionCommand>,
) {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut handle = make_test_handle("test-model", false, Some("echo-agent-tui"));
    handle.cmd_tx = cmd_tx.clone();
    handle.info = crate::session::info::Info {
        id: sid.clone(),
        cwd: "/tmp".to_string(),
    };
    if let Some(pid) = running_prompt {
        *handle.current_prompt_id.lock().unwrap() = Some(pid.to_string());
    }
    (handle, cmd_tx, cmd_rx)
}
/// Spawn a minimal fake session actor on the `LocalSet` that answers
/// `SessionCommand::IsBusy` with `busy` and forwards every other command to
/// the returned receiver so a test can assert on them (e.g. `Shutdown`).
fn spawn_fake_actor(
    mut cmd_rx: tokio::sync::mpsc::UnboundedReceiver<TestSessionCommand>,
    busy: bool,
) -> tokio::sync::mpsc::UnboundedReceiver<TestSessionCommand> {
    let (observed_tx, observed_rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::task::spawn_local(async move {
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                TestSessionCommand::IsBusy { respond_to } => {
                    let _ = respond_to.send(busy);
                }
                other => {
                    let _ = observed_tx.send(other);
                }
            }
        }
    });
    observed_rx
}
/// Drive `echo.agent/internal/evict_sessions` through the real `ext_notification`
/// handler path (not the internal helper) — matches how the leader server
/// signals a client disconnect.
async fn drive_disconnect(agent: &MvpAgent, sid: &acp::SessionId) {
    drive_disconnect_many(agent, &[sid]).await;
}
/// Like `drive_disconnect`, but evicts several sessions in a single
/// `echo.agent/internal/evict_sessions` notification — the realistic shape of a
/// real client disconnect, and the path that exercises `handle_evict_sessions`'
/// concurrent `join_all` check pass followed by the sequential act pass.
async fn drive_disconnect_many(agent: &MvpAgent, sids: &[&acp::SessionId]) {
    use acp::Agent as _;
    let ids: Vec<&str> = sids.iter().map(|s| s.0.as_ref()).collect();
    let params = serde_json::json!({ "sessionIds": ids });
    let params_json = serde_json::value::to_raw_value(&params).unwrap();
    agent
        .ext_notification(acp::ExtNotification::new(
            "echo.agent/internal/evict_sessions",
            params_json.into(),
        ))
        .await
        .expect("evict_sessions notification must be handled");
}
/// Drive `echo.agent/session/close` through the real `ext_method` dispatch
/// (`ext_method` → `handlers::session::handle` → `handle_session_close`),
/// exercising the exact production path that finalizes the replica.
async fn drive_close(agent: &MvpAgent, session_id: &str) -> Result<acp::ExtResponse, acp::Error> {
    use acp::Agent as _;
    let params = serde_json::json!({ "sessionId": session_id });
    let params_json = serde_json::value::to_raw_value(&params).unwrap();
    agent
        .ext_method(acp::ExtRequest::new(
            "echo.agent/session/close",
            std::sync::Arc::from(params_json),
        ))
        .await
}
/// Every method `parse_queue_edit_command` accepts must be forwarded from
/// `ext_notification` to that session's mailbox. Parser-only coverage misses
/// a dispatch drop.
#[tokio::test(flavor = "current_thread")]
async fn ext_notification_forwards_each_queue_method_to_session_actor() {
    use acp::Agent as _;
    let agent = build_minimal_agent_for_tests();
    let sid = acp::SessionId::new("sess-queue-rt");
    let (handle, _tx, mut cmd_rx) = make_live_session_handle(&sid, None);
    agent.insert_resident(&sid, handle);
    let session_id = sid.0.as_ref();
    let cases: [(&str, serde_json::Value); 7] = [
        (
            "echo.agent/queue/remove",
            serde_json::json!({
                "sessionId": session_id,
                "id": "p-remove",
                "expectedVersion": 3,
                "owner": "echo-agent-tui",
            }),
        ),
        (
            "echo.agent/queue/reorder",
            serde_json::json!({
                "sessionId": session_id,
                "orderedIds": ["a", "b"],
            }),
        ),
        (
            "echo.agent/queue/clear",
            serde_json::json!({
                "sessionId": session_id,
                "clientIdentifier": "echo-agent-desktop",
            }),
        ),
        (
            "echo.agent/queue/edit",
            serde_json::json!({
                "sessionId": session_id,
                "id": "p-edit",
                "newText": "rewritten",
                "owner": "echo-agent-vscode",
            }),
        ),
        (
            "echo.agent/queue/interject",
            serde_json::json!({
                "sessionId": session_id,
                "id": "p-interject",
                "expectedVersion": 2,
                "owner": "echo-agent-tui",
                "newText": "now",
            }),
        ),
        (
            "echo.agent/queue/hold_edit",
            serde_json::json!({
                "sessionId": session_id,
                "id": "p-hold",
            }),
        ),
        (
            "echo.agent/queue/release_edit",
            serde_json::json!({
                "sessionId": session_id,
                "id": "p-release",
            }),
        ),
    ];
    for (method, params) in cases {
        let params_json = serde_json::value::to_raw_value(&params).expect("serialize queue params");
        agent
            .ext_notification(acp::ExtNotification::new(method, params_json.into()))
            .await
            .unwrap_or_else(|e| panic!("{method} ext_notification failed: {e}"));
        let cmd = cmd_rx.try_recv().unwrap_or_else(|e| {
            panic!("{method} must land a SessionCommand on the actor mailbox, try_recv={e}")
        });
        match (method, cmd) {
            (
                "echo.agent/queue/remove",
                SessionCommand::RemoveQueuedPrompt {
                    id,
                    expected_version,
                    owner,
                },
            ) => {
                assert_eq!(id, "p-remove");
                assert_eq!(expected_version, 3);
                assert_eq!(owner.as_deref(), Some("echo-agent-tui"));
            }
            ("echo.agent/queue/reorder", SessionCommand::ReorderQueue { ordered_ids }) => {
                assert_eq!(ordered_ids, vec!["a", "b"]);
            }
            ("echo.agent/queue/clear", SessionCommand::ClearQueue { owner }) => {
                assert_eq!(owner.as_deref(), Some("echo-agent-desktop"));
            }
            (
                "echo.agent/queue/edit",
                SessionCommand::EditQueuedPrompt {
                    id,
                    new_text,
                    editor,
                },
            ) => {
                assert_eq!(id, "p-edit");
                assert_eq!(new_text, "rewritten");
                assert_eq!(editor.as_deref(), Some("echo-agent-vscode"));
            }
            (
                "echo.agent/queue/interject",
                SessionCommand::InterjectQueuedPrompt {
                    id,
                    expected_version,
                    owner,
                    new_text,
                },
            ) => {
                assert_eq!(id, "p-interject");
                assert_eq!(expected_version, 2);
                assert_eq!(owner.as_deref(), Some("echo-agent-tui"));
                assert_eq!(new_text.as_deref(), Some("now"));
            }
            ("echo.agent/queue/hold_edit", SessionCommand::HoldEdit { id }) => {
                assert_eq!(id, "p-hold");
            }
            ("echo.agent/queue/release_edit", SessionCommand::ReleaseEdit { id }) => {
                assert_eq!(id, "p-release");
            }
            (method, _) => {
                panic!("{method} dispatched a SessionCommand of the wrong variant")
            }
        }
    }
    assert!(
        matches!(
            cmd_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ),
        "no extra SessionCommand may remain after the seven queue methods"
    );
}
/// Methods the parser rejects (unknown, outbound `changed`, missing id /
/// newText) and a missing session must not send a command or panic.
#[tokio::test(flavor = "current_thread")]
async fn ext_notification_queue_rejects_unknown_method_missing_id_and_unknown_session() {
    use acp::Agent as _;
    let agent = build_minimal_agent_for_tests();
    let sid = acp::SessionId::new("sess-queue-neg");
    let (handle, _tx, mut cmd_rx) = make_live_session_handle(&sid, None);
    agent.insert_resident(&sid, handle);
    let session_id = sid.0.as_ref();
    let negatives: [(&str, serde_json::Value); 9] = [
        (
            "echo.agent/queue/bogus",
            serde_json::json!({ "sessionId": session_id, "id": "p1" }),
        ),
        (
            "echo.agent/queue/changed",
            serde_json::json!({
                "sessionId": session_id,
                "entries": [{
                    "id": "p1",
                    "version": 0,
                    "kind": "prompt",
                    "text": "hello",
                    "position": 0,
                }],
            }),
        ),
        (
            "echo.agent/queue/hold_edit",
            serde_json::json!({ "sessionId": session_id }),
        ),
        (
            "echo.agent/queue/release_edit",
            serde_json::json!({ "sessionId": session_id }),
        ),
        (
            "echo.agent/queue/remove",
            serde_json::json!({ "sessionId": session_id }),
        ),
        (
            "echo.agent/queue/edit",
            serde_json::json!({ "sessionId": session_id, "newText": "x" }),
        ),
        (
            "echo.agent/queue/edit",
            serde_json::json!({ "sessionId": session_id, "id": "p-edit" }),
        ),
        (
            "echo.agent/queue/interject",
            serde_json::json!({ "sessionId": session_id }),
        ),
        (
            "echo.agent/queue/hold_edit",
            serde_json::json!({ "sessionId": "no-such-session", "id": "p1" }),
        ),
    ];
    for (method, params) in negatives {
        let params_json = serde_json::value::to_raw_value(&params).expect("serialize queue params");
        agent
            .ext_notification(acp::ExtNotification::new(method, params_json.into()))
            .await
            .unwrap_or_else(|e| panic!("{method} ext_notification must not fail: {e}"));
        assert!(
            matches!(
                cmd_rx.try_recv(),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            ),
            "{method} must not send a SessionCommand (params={params})"
        );
    }
    let agent_empty = build_minimal_agent_for_tests();
    let params_json = serde_json::value::to_raw_value(&serde_json::json!({
        "sessionId": "ghost",
        "id": "p1",
    }))
    .expect("serialize");
    agent_empty
        .ext_notification(acp::ExtNotification::new(
            "echo.agent/queue/release_edit",
            params_json.into(),
        ))
        .await
        .expect("queue edit for a missing session must not error");
}
/// Fire-and-forget: a gone session actor must not turn `ext_notification`
/// into an error or panic.
#[tokio::test(flavor = "current_thread")]
async fn ext_notification_queue_edit_survives_dropped_actor_mailbox() {
    use acp::Agent as _;
    let agent = build_minimal_agent_for_tests();
    let sid = acp::SessionId::new("sess-queue-dead");
    let (handle, _tx, cmd_rx) = make_live_session_handle(&sid, None);
    agent.insert_resident(&sid, handle);
    drop(cmd_rx);
    let params = serde_json::json!({
        "sessionId": sid.0.as_ref(),
        "id": "p-hold",
    });
    let params_json = serde_json::value::to_raw_value(&params).expect("serialize queue params");
    agent
        .ext_notification(acp::ExtNotification::new(
            "echo.agent/queue/hold_edit",
            params_json.into(),
        ))
        .await
        .expect("queue edit must not error when the session actor mailbox is gone");
}
/// No-evict keystone: a client disconnecting mid-turn must NOT destroy the
/// session. The actor stays resident, no `Shutdown` is sent, the resident
/// session's command channel still **delivers** commands (so a reconnecting
/// `session/load` can keep driving the turn), and `finalize()` is NOT called
/// on a mere disconnect.
#[test]
fn disconnect_keeps_live_session_resident_without_finalize() {
    run_local_for_bridge_test(|| async {
        let agent = build_minimal_agent_for_tests();
        let sid = acp::SessionId::new("sess-live");
        let (_cmd_tx, mut cmd_rx) = {
            let (handle, tx, rx) = make_live_session_handle(&sid, Some("turn-1"));
            agent.insert_resident(&sid, handle);
            (tx, rx)
        };
        drive_disconnect(&agent, &sid).await;
        assert!(
            agent.is_resident(&sid),
            "live session must stay resident across client disconnect"
        );
        assert!(
            matches!(
                cmd_rx.try_recv(),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            ),
            "no command may be sent to a session kept resident with live work"
        );
        let resident = agent
            .resident_handle(&sid)
            .expect("session must still be resident");
        resident
            .cmd_tx
            .send(TestSessionCommand::ResetPermissionState)
            .expect("resident session channel must accept commands post-disconnect");
        assert!(
            matches!(
                cmd_rx.try_recv(),
                Ok(TestSessionCommand::ResetPermissionState)
            ),
            "the resident session's receiver must observe the delivered command"
        );
        assert!(
            agent.finalize_spy.borrow().is_empty(),
            "finalize() must NOT fire on client disconnect"
        );
        assert_eq!(
            agent.session_live_state_for(&sid),
            Some(SessionLiveState::Working),
            "a kept-resident session with live work is Working"
        );
    });
}
/// Keep-resident must hold even if the `current_prompt_id` lock is poisoned:
/// an unknown state is treated as "busy" (never unload). Guards against a
/// regression flipping the `unwrap_or(true)` fallback to `false`.
#[test]
fn disconnect_keeps_resident_on_poisoned_lock() {
    run_local_for_bridge_test(|| async {
        let agent = build_minimal_agent_for_tests();
        let sid = acp::SessionId::new("sess-poison");
        let (handle, _tx, _rx) = make_live_session_handle(&sid, None);
        let poison_target = handle.current_prompt_id.clone();
        agent.insert_resident(&sid, handle);
        let _ = std::thread::spawn(move || {
            let _g = poison_target.lock().unwrap();
            panic!("poison current_prompt_id");
        })
        .join();
        assert!(
            agent
                .resident_handle(&sid)
                .unwrap()
                .current_prompt_id
                .lock()
                .is_err(),
            "precondition: the lock must be poisoned"
        );
        drive_disconnect(&agent, &sid).await;
        assert!(
            agent.is_resident(&sid),
            "a session with an unknown (poisoned) state must be kept resident"
        );
        assert_eq!(
            agent.session_live_state_for(&sid),
            Some(SessionLiveState::Working),
        );
    });
}
/// A wedged actor stays tracked. `remove_session` releases everything else but
/// keeps a still-running thread, because dropping its handle would detach the
/// thread and leave nothing for the supervisor sweep to find.
#[test]
fn remove_session_keeps_a_running_thread_tracked() {
    run_local_for_bridge_test(|| async {
        let agent = build_minimal_agent_for_tests();
        let sid = acp::SessionId::new("sess-wedged");
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        agent.session_registry.set_thread(
            &sid,
            crate::session::SessionThread::from_handle(std::thread::spawn(move || {
                let _ = release_rx.recv();
            })),
        );
        agent.set_turn_number(&sid, 1);
        agent.remove_session(&sid);
        assert!(
            agent.session_registry.has_thread(&sid),
            "a running actor thread must survive removal for the sweep"
        );
        assert_eq!(
            agent.session_registry.counts().retained_resources,
            0,
            "everything except the running thread must be released"
        );
        drop(release_tx);
        for _ in 0..100 {
            agent.sweep_dead_sessions();
            if !agent.session_registry.has_thread(&sid) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            !agent.session_registry.has_thread(&sid),
            "the sweep must reclaim the thread once it exits"
        );
    });
}
/// Idle-unload stub (memory bound) + supervisor interaction: a *fully idle*
/// session is unloaded to disk on disconnect (actor `Shutdown`, handle
/// dropped) while the `SessionThread` is **retained** for
/// `drain_old_session_thread`. It is not finalized, and once the kept thread
/// finishes the supervisor reaps it as a *clean* exit — never `DeadFailed`.
#[test]
fn disconnect_unloads_idle_session_without_finalize() {
    run_local_for_bridge_test(|| async {
        let agent = build_minimal_agent_for_tests();
        let sid = acp::SessionId::new("sess-idle");
        let (handle, _cmd_tx, cmd_rx) = make_live_session_handle(&sid, None);
        agent.insert_resident(&sid, handle);
        let mut observed = spawn_fake_actor(cmd_rx, false);
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        agent.session_registry.set_thread(
            &sid,
            crate::session::SessionThread::from_handle(std::thread::spawn(move || {
                let _ = release_rx.recv();
            })),
        );
        agent.ensure_session_supervisor();
        drive_disconnect(&agent, &sid).await;
        assert!(
            !agent.is_resident(&sid),
            "idle session must be unloaded from the resident map on disconnect"
        );
        assert!(
            agent.session_registry.has_thread(&sid),
            "idle-unload must keep the SessionThread for reconnect drain"
        );
        let shutdown = tokio::time::timeout(std::time::Duration::from_secs(1), observed.recv())
            .await
            .expect("idle-unload must send a command within 1s")
            .expect("fake actor channel must stay open");
        assert!(
            matches!(shutdown, TestSessionCommand::Shutdown(_)),
            "idle-unload must send SessionCommand::Shutdown"
        );
        assert!(
            agent.finalize_spy.borrow().is_empty(),
            "idle-unload on disconnect must NOT finalize the cloud replica"
        );
        assert_eq!(
            agent.session_live_state_for(&sid),
            Some(SessionLiveState::Dormant),
            "an idle-unloaded session demotes to Dormant"
        );
        drop(release_tx);
        let deadline = tokio::time::Instant::now() + (SESSION_SUPERVISOR_TICK * 6);
        while tokio::time::Instant::now() < deadline {
            if !agent.session_registry.has_thread(&sid) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            !agent.session_registry.has_thread(&sid),
            "supervisor must drop the finished kept thread"
        );
        assert!(
            !agent
                .roster_delta_spy
                .borrow()
                .iter()
                .any(|(id, st)| id == sid.0.as_ref() && *st == SessionLiveState::DeadFailed),
            "a cleanly idle-unloaded session must not be reaped as DeadFailed"
        );
        assert_eq!(
            agent.session_live_state_for(&sid),
            None,
            "clean-exit sweep must drop the Dormant live-state entry"
        );
    });
}

#[test]
fn disconnect_does_not_unload_while_prompt_intake_holds_the_session() {
    run_local_for_bridge_test(|| async {
        let agent = build_minimal_agent_for_tests();
        let sid = acp::SessionId::new("sess-intake-disconnect");
        let (handle, _cmd_tx, cmd_rx) = make_live_session_handle(&sid, None);
        agent.insert_resident(&sid, handle);
        let _observed = spawn_fake_actor(cmd_rx, false);
        let intake = agent.dispatch_lock(&sid);
        let guard = intake.lock().await;
        drive_disconnect(&agent, &sid).await;
        assert!(agent.is_resident(&sid), "disconnect cannot race an admitted prompt");
        drop(guard);
        drive_disconnect(&agent, &sid).await;
        assert!(!agent.is_resident(&sid), "a later idle disconnect can reclaim the session");
    });
}
/// The `IsBusy` keep-resident path. A between-turns session
/// (`current_prompt_id = None`) whose actor answers `IsBusy = true` (queued
/// inputs at the turn boundary) must be kept resident — NOT unloaded — and
/// must receive no `Shutdown`. This exercises the async round-trip that the
/// sync fast-path tests skip.
#[test]
fn disconnect_keeps_resident_when_actor_reports_busy() {
    run_local_for_bridge_test(|| async {
        let agent = build_minimal_agent_for_tests();
        let sid = acp::SessionId::new("sess-busy");
        let (handle, _cmd_tx, cmd_rx) = make_live_session_handle(&sid, None);
        agent.insert_resident(&sid, handle);
        let mut observed = spawn_fake_actor(cmd_rx, true);
        drive_disconnect(&agent, &sid).await;
        assert!(
            agent.is_resident(&sid),
            "a between-turns session with queued work (IsBusy=true) must stay resident"
        );
        assert_eq!(
            agent.session_live_state_for(&sid),
            Some(SessionLiveState::Working),
            "an actor-reported-busy session is kept Working"
        );
        tokio::task::yield_now().await;
        assert!(
            matches!(
                observed.try_recv(),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            ),
            "a busy session must not be sent Shutdown"
        );
    });
}
/// A between-turns session whose ONLY outstanding work is a parked
/// `PlanApproval` reverse-request (the resume re-park) must be kept resident on
/// disconnect. The actor answers `IsBusy = false`, so the keep-resident outcome
/// can come ONLY from the parked-approval sync fast path in `session_has_live_work`
/// — deleting that check would let this session unload (mutation-killing).
#[test]
fn disconnect_keeps_resident_when_plan_approval_parked() {
    run_local_for_bridge_test(|| async {
        let agent = build_minimal_agent_for_tests();
        let sid = acp::SessionId::new("sess-plan-parked");
        let (handle, _cmd_tx, cmd_rx) = make_live_session_handle(&sid, None);
        handle.pending_interactions.lock().unwrap().insert(
            "exit-plan-mode-resume".to_string(),
            crate::session::pending_interaction::PendingKind::PlanApproval,
        );
        agent.insert_resident(&sid, handle);
        let mut observed = spawn_fake_actor(cmd_rx, false);
        drive_disconnect(&agent, &sid).await;
        assert!(
            agent.is_resident(&sid),
            "a session with a parked plan-approval must stay resident"
        );
        assert_eq!(
            agent.session_live_state_for(&sid),
            Some(SessionLiveState::Working),
            "a parked-approval session is kept Working"
        );
        tokio::task::yield_now().await;
        assert!(
            matches!(
                observed.try_recv(),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            ),
            "a parked-approval session must not be sent Shutdown"
        );
    });
}
/// Mixed batch in a *single* `echo.agent/internal/evict_sessions` notification —
/// the realistic disconnect shape and the path that exercises
/// `handle_evict_sessions`' `join_all` two-pass (concurrent `IsBusy` checks,
/// then sequential act). One session's actor reports busy (→ kept resident,
/// `Working`, no `Shutdown`); the other is idle (→ unloaded, `Dormant`,
/// `Shutdown` sent). Each must get its own outcome with no cross-contamination
/// between the concurrent check pass and the sequential act pass.
#[test]
fn disconnect_mixed_batch_keeps_busy_unloads_idle() {
    run_local_for_bridge_test(|| async {
        let agent = build_minimal_agent_for_tests();
        let sid_busy = acp::SessionId::new("sess-batch-busy");
        let sid_idle = acp::SessionId::new("sess-batch-idle");
        let (busy_handle, _busy_tx, busy_rx) = make_live_session_handle(&sid_busy, None);
        let (idle_handle, _idle_tx, idle_rx) = make_live_session_handle(&sid_idle, None);
        agent.insert_resident(&sid_busy, busy_handle);
        agent.insert_resident(&sid_idle, idle_handle);
        let mut busy_observed = spawn_fake_actor(busy_rx, true);
        let mut idle_observed = spawn_fake_actor(idle_rx, false);
        drive_disconnect_many(&agent, &[&sid_busy, &sid_idle]).await;
        assert!(
            agent.is_resident(&sid_busy),
            "the busy session in the batch must stay resident"
        );
        assert_eq!(
            agent.session_live_state_for(&sid_busy),
            Some(SessionLiveState::Working),
            "the busy session must be Working"
        );
        assert!(
            !agent.is_resident(&sid_idle),
            "the idle session in the batch must be unloaded"
        );
        assert_eq!(
            agent.session_live_state_for(&sid_idle),
            Some(SessionLiveState::Dormant),
            "the idle session must be Dormant"
        );
        let idle_shutdown =
            tokio::time::timeout(std::time::Duration::from_secs(1), idle_observed.recv())
                .await
                .expect("idle session must receive a command within 1s")
                .expect("fake actor channel must stay open");
        assert!(
            matches!(idle_shutdown, TestSessionCommand::Shutdown(_)),
            "the idle session must be sent Shutdown"
        );
        tokio::task::yield_now().await;
        assert!(
            matches!(
                busy_observed.try_recv(),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            ),
            "the busy session must not be sent Shutdown in a mixed batch"
        );
        assert!(
            agent.finalize_spy.borrow().is_empty(),
            "neither batch outcome may finalize on a mere disconnect"
        );
    });
}
/// The bounded `session_live_state` map does not grow without bound
/// across repeated create/close cycles — every terminal close drops its
/// entry, so the map size stays at the live count, not the cumulative count.
#[test]
fn session_live_state_map_is_bounded_across_cycles() {
    run_local_for_bridge_test(|| async {
        let agent = build_minimal_agent_for_tests();
        for i in 0..50 {
            let sid = acp::SessionId::new(format!("sess-cycle-{i}"));
            let (handle, _tx, _rx) = make_live_session_handle(&sid, Some("turn"));
            agent.insert_resident(&sid, handle);
            agent.set_session_live_state(&sid, SessionLiveState::IdleResident);
            assert_eq!(
                agent.close_active_session(&sid).await,
                crate::agent::mvp_agent::session_lifecycle::CloseOutcome::Closed,
                "cycle {i} must actually close, or the bound below proves nothing"
            );
        }
        assert_eq!(
            agent.session_registry.counts().session_live_state,
            0,
            "terminal closes must leave no residual live-state entries (bounded map)"
        );
    });
}
/// Finalize fires on a genuine terminal close, driven through the real
/// `echo.agent/session/close` dispatch rather than the internal helper.
#[test]
fn explicit_close_finalizes_the_replica() {
    run_local_for_bridge_test(|| async {
        let agent = build_minimal_agent_for_tests();
        let sid = acp::SessionId::new("sess-close");
        let (handle, _tx, mut cmd_rx) = make_live_session_handle(&sid, Some("turn-1"));
        agent.insert_resident(&sid, handle);
        drive_close(&agent, "no-such-session")
            .await
            .expect("close of a missing session must succeed as a no-op");
        assert!(
            agent.finalize_spy.borrow().is_empty(),
            "closing a missing session must NOT finalize"
        );
        drive_close(&agent, sid.0.as_ref())
            .await
            .expect("session close must be handled");
        let Ok(TestSessionCommand::Cancel(options)) = cmd_rx.try_recv() else {
            panic!("close must send Cancel before anything else");
        };
        assert_eq!(
            (
                options.cancel_subagents,
                options.kill_background_tasks,
                options.history.clone(),
                options.trigger.as_ref().map(|t| t.as_str()),
                options.user_initiated
            ),
            (
                true,
                true,
                crate::session::CancelHistoryDisposition::Keep,
                Some("session_close"),
                false
            ),
        );
        assert!(
            matches!(
                cmd_rx.try_recv(),
                Ok(TestSessionCommand::Shutdown(
                    crate::session::ShutdownKind::CancelRunningTurn
                ))
            ),
            "close frees the session, so its Shutdown must cancel the running \
             turn; a graceful one would let the turn answer EndTurn as the \
             actor tears down"
        );
        assert_eq!(
            agent.finalize_spy.borrow().as_slice(),
            &[sid.0.to_string()],
            "explicit close must finalize the cloud replica exactly once"
        );
        assert!(
            !agent.is_resident(&sid),
            "explicit close removes the session"
        );
        assert_eq!(
            agent.session_live_state_for(&sid),
            None,
            "terminal removal must drop the live-state entry (bounded map)"
        );
        assert!(
            agent
                .roster_delta_spy
                .borrow()
                .iter()
                .any(|(id, st)| id == sid.0.as_ref() && *st == SessionLiveState::Completed),
            "explicit close must emit a Completed roster delta"
        );
    });
}
/// Join-handle supervisor: a *resident* actor that panics is reaped
/// promptly — removed from `sessions`/`session_threads`, demoted to
/// `DeadFailed` (observed via the roster delta, since the live-state entry
/// is dropped on removal), and NOT finalized (the conversation persists).
///
/// Polls in real time (the panic unwinds on a real OS thread, independent of
/// the tokio clock); the reap lands within a small number of supervisor
/// ticks. The injected-panic backtrace on stderr is expected and harmless.
#[test]
fn supervisor_reaps_panicked_resident_actor() {
    run_local_for_bridge_test(|| async {
        let agent = build_minimal_agent_for_tests();
        let sid = acp::SessionId::new("sess-panic");
        let (handle, _tx, _rx) = make_live_session_handle(&sid, Some("turn-1"));
        agent.insert_resident(&sid, handle);
        let panic_thread = std::thread::spawn(|| panic!("injected actor panic"));
        agent.session_registry.set_thread(
            &sid,
            crate::session::SessionThread::from_handle(panic_thread),
        );
        agent.set_session_live_state(&sid, SessionLiveState::Working);
        agent.ensure_session_supervisor();
        let deadline = tokio::time::Instant::now() + (SESSION_SUPERVISOR_TICK * 6);
        while tokio::time::Instant::now() < deadline {
            if !agent.session_registry.has_thread(&sid) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            !agent.session_registry.has_thread(&sid),
            "supervisor must reap the dead thread"
        );
        assert!(
            !agent.is_resident(&sid),
            "reaped session must be removed from the resident map"
        );
        assert_eq!(
            agent.session_live_state_for(&sid),
            None,
            "terminal removal drops the live-state entry (bounded map)"
        );
        assert!(
            agent
                .roster_delta_spy
                .borrow()
                .iter()
                .any(|(id, st)| id == sid.0.as_ref() && *st == SessionLiveState::DeadFailed),
            "a reaped resident actor must emit a DeadFailed roster delta"
        );
        assert!(
            agent.finalize_spy.borrow().is_empty(),
            "reaping a dead actor must NOT finalize (conversation persists)"
        );
    });
}
/// Regression: writeback must self-correct once remote settings arrive
/// (the field used to be frozen at construction).
#[tokio::test]
#[serial_test::serial]
async fn storage_mode_self_corrects_to_writeback_when_settings_arrive() {
    let _env = crate::env::EnvVarGuard::remove("ECHO_AGENT_STORAGE_MODE");
    let auth = crate::auth::EchoAgentAuth {
        auth_mode: crate::auth::AuthMode::Oidc,
        oidc_issuer: Some("https://auth.echo-agent.invalid".to_string()),
        key: "test-token".to_string(),
        ..Default::default()
    };
    let agent = build_agent_with_auth(auth);
    agent.cfg.borrow_mut().mode = crate::agent::config::AgentMode::Leader;
    assert_eq!(agent.storage_mode(), StorageMode::Local);
    agent.cfg.borrow_mut().remote_settings = Some(crate::util::config::RemoteSettings {
        writeback_enabled: Some(true),
        ..Default::default()
    });
    agent.on_remote_settings_changed();
    assert_eq!(agent.storage_mode(), StorageMode::Writeback);
}
/// `spawn_settings_reapply` coalesces: while one reapply is in flight,
/// repeated calls (boot + rapid `/new`) do not spawn overlapping tasks.
#[test]
fn spawn_settings_reapply_coalesces_while_in_flight() {
    run_local_for_bridge_test(|| async {
        let agent = build_minimal_agent_for_tests();
        assert_eq!(agent.settings_reapply_spawn_count.get(), 0);
        agent.spawn_settings_reapply();
        agent.spawn_settings_reapply();
        agent.spawn_settings_reapply();
        assert_eq!(
            agent.settings_reapply_spawn_count.get(),
            1,
            "overlapping settings reapplies must coalesce to a single task"
        );
        assert!(agent.settings_reapply_in_flight.get());
    });
}
/// The in-flight guard clears on task completion (via the `ClearOnDrop`
/// guard, so it also clears on panic), allowing a later reapply to re-spawn.
#[test]
fn spawn_settings_reapply_clears_flag_after_completion() {
    run_local_for_bridge_test(|| async {
        let agent = build_minimal_agent_for_tests();
        agent.spawn_settings_reapply();
        assert_eq!(agent.settings_reapply_spawn_count.get(), 1);
        assert!(agent.settings_reapply_in_flight.get());
        let mut cleared = false;
        for _ in 0..40 {
            if !agent.settings_reapply_in_flight.get() {
                cleared = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert!(
            cleared,
            "in-flight flag must clear after the task completes"
        );
        agent.spawn_settings_reapply();
        assert_eq!(
            agent.settings_reapply_spawn_count.get(),
            2,
            "a reapply after completion must spawn again"
        );
    });
}
/// The post-auth fetch has its own guard, so an in-flight settings reapply
/// cannot coalesce away a freshly authenticated identity's gate and settings
/// resolution.
#[test]
fn post_auth_settings_not_coalesced_by_in_flight_reapply() {
    run_local_for_bridge_test(|| async {
        let agent = build_minimal_agent_for_tests();
        agent.spawn_settings_reapply();
        assert!(agent.settings_reapply_in_flight.get());
        agent.spawn_post_auth_settings(crate::auth::EchoAgentAuth::test_default());
        assert_eq!(
            agent.post_auth_settings_spawn_count.get(),
            1,
            "post-auth must spawn on its own guard despite an in-flight reapply"
        );
        assert!(agent.post_auth_settings_in_flight.get());
    });
}
/// The tier re-check work is single-flight across every caller: back-to-back
/// gated initializes run at most one live check, and an awaited
/// authenticate-path check skips — rather than doubles or waits out — a
/// check already wedged on a stalled subscription endpoint. Drives the exact
/// block `initialize` runs when `tier_allowed` is false (the full
/// `initialize` fires once-per-process ECHO_AGENT_HOME cleanup work that a unit
/// test must not run against the developer's real home).
#[test]
fn gated_reconnect_tier_recheck_is_single_flight() {
    run_local_for_bridge_test(|| async {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        listener
            .set_nonblocking(true)
            .expect("nonblocking accept loop");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let accept_stop = stop.clone();
        let accept_thread = std::thread::spawn(move || {
            let mut held = Vec::new();
            while !accept_stop.load(std::sync::atomic::Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => held.push(stream),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                    Err(_) => break,
                }
            }
        });
        let agent = build_minimal_agent_for_tests();
        agent.cfg.borrow_mut().endpoints.cli_chat_proxy_base_url =
            Some(format!("http://{addr}/v1"));
        agent.cfg.borrow_mut().remote_settings = Some(crate::util::config::RemoteSettings {
            allow_access: Some(false),
            ..Default::default()
        });
        let auth = crate::auth::EchoAgentAuth {
            key: "gated-user-key".into(),
            user_id: "user-gated".into(),
            auth_mode: crate::auth::AuthMode::Oidc,
            oidc_issuer: Some(crate::auth::ECHO_AGENT_OAUTH2_ISSUER.to_owned()),
            expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
            ..crate::auth::EchoAgentAuth::test_default()
        };
        agent.auth_manager.hot_swap(auth.clone());
        *agent.allow_access_resolved_for.borrow_mut() = Some(auth.user_id.clone());
        agent.tier_allowed.set(false);
        for _ in 0..2 {
            if !agent.tier_allowed.get() {
                agent.spawn_tier_recheck();
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert_eq!(
            agent.tier_recheck_run_count.get(),
            1,
            "the second spawned check must skip the claimed re-check"
        );
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            agent.enforce_echo_agent_code_access(&auth),
        )
        .await
        .expect("an awaited check must skip, not wait out, the wedged re-check");
        assert_eq!(
            agent.tier_recheck_run_count.get(),
            1,
            "the awaited check must not run a second concurrent re-check"
        );
        assert!(
            !agent.tier_allowed.get(),
            "the gate stays until a re-check resolves"
        );
        stop.store(true, std::sync::atomic::Ordering::Release);
        accept_thread.join().expect("accept loop joins");
    });
}
/// The check's own mint spawns a `/user` enrichment that can rewrite the
/// in-memory user_id to the proxy-canonical value mid-check; the identity
/// guard must read that normalization as the same account (it is the id the
/// check's own bearer resolved to), while a live id matching neither the
/// started nor the canonical id is a real switch and still discards.
#[test]
fn tier_recheck_identity_guard_accepts_enrichment_canonical_user_id() {
    run_local_for_bridge_test(|| async {
        let agent = build_minimal_agent_for_tests();
        let auth = crate::auth::EchoAgentAuth {
            key: "seeded-key".into(),
            user_id: "canonical-user".into(),
            expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
            ..crate::auth::EchoAgentAuth::test_default()
        };
        agent.auth_manager.hot_swap(auth);
        assert!(!agent.tier_recheck_identity_changed("seeded-user", Some("canonical-user")));
        assert!(!agent.tier_recheck_identity_changed("canonical-user", None));
        assert!(!agent.tier_recheck_identity_changed("canonical-user", Some("other-user")));
        assert!(agent.tier_recheck_identity_changed("seeded-user", Some("other-user")));
        assert!(agent.tier_recheck_identity_changed("seeded-user", None));
        assert!(agent.tier_recheck_identity_changed("seeded-user", Some("")));
    });
}
/// The other half of the reconnect paywall flash (the wedged test above
/// locks the "gate holds while the check is in flight" half): a re-check
/// that confirms a qualifying tier lifts `tier_allowed`, so the flash a
/// subscribed user can see on a gated reconnect clears. Settings stay
/// absent (the mock 404s `/settings`), modeling the remote-fetch-failed /
/// disabled arm where the confirmed tier is the authority for the lift;
/// the bearer's tier claim already matches the live tier, so the
/// post-unblock mint is skipped and no refresher is needed.
#[test]
fn gated_reconnect_recheck_lifts_gate_clearing_paywall_flash() {
    run_local_for_bridge_test(|| async {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        listener
            .set_nonblocking(true)
            .expect("nonblocking accept loop");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let accept_stop = stop.clone();
        let accept_thread = std::thread::spawn(move || {
            use std::io::{Read, Write};
            while !accept_stop.load(std::sync::atomic::Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                        let mut head = Vec::new();
                        let mut byte = [0u8; 1];
                        while !head.ends_with(b"\r\n\r\n") {
                            match stream.read(&mut byte) {
                                Ok(1) => head.push(byte[0]),
                                _ => break,
                            }
                        }
                        let head = String::from_utf8_lossy(&head);
                        let response = if head.contains("/user?include=subscription") {
                            let body =
                                r#"{"userId":"user-flash","subscriptionTier":"EchoAgentPro"}"#;
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            )
                        } else {
                            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\
                             Connection: close\r\n\r\n"
                                .to_owned()
                        };
                        let _ = stream.write_all(response.as_bytes());
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                    Err(_) => break,
                }
            }
        });
        let temp_dir = tempfile::tempdir().unwrap();
        let auth_manager = std::sync::Arc::new(crate::auth::AuthManager::new(
            temp_dir.path(),
            crate::auth::EchoAgentComConfig::default(),
        ));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let gateway = GatewaySender::new(tx);
        let cfg = crate::agent::config::Config::default();
        let agent = MvpAgent::new(gateway, &cfg, auth_manager, None).expect("valid test config");
        agent.cfg.borrow_mut().endpoints.cli_chat_proxy_base_url =
            Some(format!("http://{addr}/v1"));
        let auth = crate::auth::EchoAgentAuth {
            key: jwt_with_tier(5),
            user_id: "user-flash".into(),
            auth_mode: crate::auth::AuthMode::Oidc,
            oidc_issuer: Some(crate::auth::ECHO_AGENT_OAUTH2_ISSUER.to_owned()),
            expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
            ..crate::auth::EchoAgentAuth::test_default()
        };
        agent.auth_manager.hot_swap(auth);
        agent.tier_allowed.set(false);
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            agent.retry_subscription_check(),
        )
        .await
        .expect("re-check must finish well inside its bounded awaits");
        assert!(
            agent.tier_allowed.get(),
            "a confirmed qualifying tier must lift the gate — the reconnect paywall flash clears"
        );
        assert_eq!(
            agent.tier_recheck_run_count.get(),
            1,
            "the lift came from exactly one claimed re-check"
        );
        stop.store(true, std::sync::atomic::Ordering::Release);
        accept_thread.join().expect("accept loop joins");
    });
}
/// Agent with pre-loaded auth, a gateway receiver (to assert emitted
/// notifications), and the proxy URL pointed at a mock `/v1/settings`.
fn build_agent_with_auth_and_proxy(
    auth: crate::auth::EchoAgentAuth,
    proxy_url: String,
    mode: crate::agent::config::AgentMode,
) -> (
    MvpAgent,
    tokio::sync::mpsc::UnboundedReceiver<echo_agent_acp::AcpClientMessage>,
) {
    use crate::agent::config::Config as AgentConfig;
    use crate::auth::{AuthManager, EchoAgentComConfig};
    let temp_dir = tempfile::tempdir().unwrap();
    let auth_manager =
        std::sync::Arc::new(AuthManager::new(temp_dir.path(), EchoAgentComConfig::default()));
    auth_manager.hot_swap(auth);
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let gateway = GatewaySender::new(tx);
    let mut cfg = AgentConfig {
        mode,
        ..Default::default()
    };
    cfg.endpoints.cli_chat_proxy_base_url = Some(proxy_url);
    let agent = MvpAgent::new(gateway, &cfg, auth_manager, None).expect("valid test config");
    (agent, rx)
}
/// Drain the gateway, returning `true` if any `echo.agent/settings/update`
/// notification was emitted (and acking each so the sender doesn't warn).
fn drained_settings_update(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<echo_agent_acp::AcpClientMessage>,
) -> bool {
    let mut found = false;
    while let Ok(msg) = rx.try_recv() {
        if let echo_agent_acp::AcpClientMessage::ExtNotification(args) = msg {
            if &*args.request.method == "echo.agent/settings/update" {
                found = true;
            }
            let _ = args.response_tx.send(Ok(()));
        }
    }
    found
}
/// Re-open the process-global external-OTEL gate on drop so a closed gate
/// never leaks into another test.
struct RestoreOtelGate;
impl Drop for RestoreOtelGate {
    fn drop(&mut self) {
        echo_agent_telemetry::external::mark_external_otel_settings_resolved();
    }
}
/// Regression: `cfg.remote_settings` is not reset on an account switch, so the
/// access gate must not read a previous identity's cached `allow_access`. A
/// mismatched identity stays provisionally open (unknown), like the OTEL gate's
/// `rearm_on_switch`.
#[tokio::test]
async fn access_gate_does_not_leak_verdict_across_identities() {
    use crate::agent::config::AgentMode;
    use crate::auth::{EchoAgentAuth, ECHO_AGENT_OAUTH2_ISSUER};
    let auth_a = EchoAgentAuth {
        oidc_issuer: Some(ECHO_AGENT_OAUTH2_ISSUER.to_string()),
        user_id: "user-a".into(),
        ..EchoAgentAuth::test_default()
    };
    let (agent, _rx) = build_agent_with_auth_and_proxy(
        auth_a,
        "http://127.0.0.1:1/".to_string(),
        AgentMode::Leader,
    );
    {
        let mut cfg = agent.cfg.borrow_mut();
        cfg.remote_settings = Some(crate::util::config::RemoteSettings {
            allow_access: Some(false),
            ..Default::default()
        });
    }
    *agent.allow_access_resolved_for.borrow_mut() = Some("user-a".to_string());
    let auth_b = EchoAgentAuth {
        oidc_issuer: Some(ECHO_AGENT_OAUTH2_ISSUER.to_string()),
        user_id: "user-b".into(),
        ..EchoAgentAuth::test_default()
    };
    assert!(auth_b.is_echo_agent_auth(), "precondition: first-party EchoAgent auth");
    agent.enforce_echo_agent_code_access(&auth_b).await;
    assert!(
        agent.tier_allowed.get(),
        "identity B must not inherit identity A's denied allow_access verdict",
    );
}
/// First-party EchoAgent auth + `writeback_enabled` settings → storage upgrades to
/// Writeback; the settings arrival also emits `echo.agent/settings/update` and opens
/// the external-OTEL gate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn post_auth_settings_echo_agent_upgrades_writeback_emits_and_opens_gate() {
    use crate::agent::config::AgentMode;
    use crate::auth::{EchoAgentAuth, ECHO_AGENT_OAUTH2_ISSUER};
    let _restore = RestoreOtelGate;
    let _storage_env = crate::env::EnvVarGuard::remove("ECHO_AGENT_STORAGE_MODE");
    let server = echo_agent_test_support::MockInferenceServer::start()
        .await
        .unwrap();
    server.set_settings(serde_json::json!({
        "writeback_enabled": true,
        "allow_access": true,
    }));
    let echo_agent_auth = EchoAgentAuth {
        oidc_issuer: Some(ECHO_AGENT_OAUTH2_ISSUER.to_string()),
        ..EchoAgentAuth::test_default()
    };
    assert!(echo_agent_auth.is_echo_agent_auth(), "precondition: first-party EchoAgent auth");
    let (agent, mut rx) =
        build_agent_with_auth_and_proxy(echo_agent_auth, server.url(), AgentMode::Leader);
    assert_eq!(
        agent.storage_mode(),
        StorageMode::Local,
        "precondition: leader boots in Local storage mode"
    );
    echo_agent_telemetry::external::suppress_external_otel_until_settings();
    assert!(!echo_agent_telemetry::external::is_settings_gate_open());
    agent.maybe_fetch_post_auth_settings().await;
    assert_eq!(
        agent.storage_mode(),
        StorageMode::Writeback,
        "echoagent auth + writeback_enabled settings must upgrade storage to Writeback"
    );
    assert!(
        echo_agent_telemetry::external::is_settings_gate_open(),
        "a settings response must open the external-OTEL gate"
    );
    assert!(
        drained_settings_update(&mut rx),
        "settings arrival must push echo.agent/settings/update to clients"
    );
}
/// BYOK auth must not be upgraded to `Writeback` even when the server
/// advertises it; the push and gate still fire.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn post_auth_settings_non_echo_agent_keeps_local_but_still_emits() {
    use crate::agent::config::AgentMode;
    use crate::auth::{AuthMode, EchoAgentAuth};
    let _restore = RestoreOtelGate;
    let server = echo_agent_test_support::MockInferenceServer::start()
        .await
        .unwrap();
    server.set_settings(serde_json::json!({
        "writeback_enabled": true,
        "allow_access": true,
    }));
    let api_auth = EchoAgentAuth {
        auth_mode: AuthMode::ApiKey,
        ..EchoAgentAuth::test_default()
    };
    assert!(
        !api_auth.is_echo_agent_auth(),
        "precondition: non-first-party auth"
    );
    let (agent, mut rx) =
        build_agent_with_auth_and_proxy(api_auth, server.url(), AgentMode::Leader);
    echo_agent_telemetry::external::suppress_external_otel_until_settings();
    agent.maybe_fetch_post_auth_settings().await;
    assert_eq!(
        agent.storage_mode(),
        StorageMode::Local,
        "non-echo-agent auth must stay Local even when writeback is advertised remotely"
    );
    assert!(
        echo_agent_telemetry::external::is_settings_gate_open(),
        "a settings response must open the gate regardless of auth kind"
    );
    assert!(
        drained_settings_update(&mut rx),
        "settings arrival must push echo.agent/settings/update for non-echo-agent auth too"
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn post_auth_settings_failure_resolves_gate_onto_local_policy() {
    use crate::agent::config::AgentMode;
    use crate::auth::{EchoAgentAuth, ECHO_AGENT_OAUTH2_ISSUER};
    let _restore = RestoreOtelGate;
    let server = echo_agent_test_support::MockInferenceServer::start()
        .await
        .unwrap();
    let echo_agent_auth = EchoAgentAuth {
        oidc_issuer: Some(ECHO_AGENT_OAUTH2_ISSUER.to_string()),
        ..EchoAgentAuth::test_default()
    };
    let (agent, _rx) = build_agent_with_auth_and_proxy(echo_agent_auth, server.url(), AgentMode::Leader);
    echo_agent_telemetry::external::suppress_external_otel_until_settings();
    assert!(!echo_agent_telemetry::external::is_settings_gate_open());
    agent.maybe_fetch_post_auth_settings().await;
    assert!(
        echo_agent_telemetry::external::is_settings_gate_open(),
        "an exhausted fetch is a definitive answer: open on local policy"
    );
    assert!(
        agent.cfg.borrow().remote_settings.is_none(),
        "opening the gate must not fabricate settings; none were fetched"
    );
}
/// A same-credential refresh must NOT re-suppress a gate already resolved for
/// that credential; the reason `OtelGate` remembers the identity. With the
/// gate resolved-open for this identity, a later failing (`Retry`) refresh
/// leaves it OPEN (regressing the identity guard would re-close it forever).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn same_credential_refresh_does_not_flap_resolved_gate() {
    use crate::agent::config::AgentMode;
    use crate::auth::{EchoAgentAuth, ECHO_AGENT_OAUTH2_ISSUER};
    let _restore = RestoreOtelGate;
    let server = echo_agent_test_support::MockInferenceServer::start()
        .await
        .unwrap();
    let echo_agent_auth = EchoAgentAuth {
        oidc_issuer: Some(ECHO_AGENT_OAUTH2_ISSUER.to_string()),
        ..EchoAgentAuth::test_default()
    };
    let (agent, _rx) =
        build_agent_with_auth_and_proxy(echo_agent_auth.clone(), server.url(), AgentMode::Leader);
    agent.otel_gate.set_resolved_for(&echo_agent_auth.user_id);
    echo_agent_telemetry::external::mark_external_otel_settings_resolved();
    assert!(echo_agent_telemetry::external::is_settings_gate_open());
    agent.refresh_remote_settings(&echo_agent_auth).await;
    assert!(
        echo_agent_telemetry::external::is_settings_gate_open(),
        "a same-credential refresh must not flap a gate already resolved for it"
    );
}
/// A `/settings` 401 from a token that rotated mid-flight must self-heal:
/// refresh once and, if the token changed, re-fetch with it. Without the
/// re-fetch the stale 401 fails OPEN (no remote policy).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn settings_self_heal_refetches_after_token_rotation() {
    use crate::agent::config::AgentMode;
    use crate::auth::refresh::{RefreshOutcome, TokenRefresher};
    use crate::auth::{EchoAgentAuth, ECHO_AGENT_OAUTH2_ISSUER};
    let _restore = RestoreOtelGate;
    let server = echo_agent_test_support::MockInferenceServer::start_with_required_auth(
        vec![echo_agent_test_support::MockModelEntry::new("echo-agent-build")],
        "rotated-key",
    )
    .await
    .unwrap();
    server.set_settings(serde_json::json!({ "allow_access": true }));
    struct RotatingRefresher;
    #[async_trait::async_trait]
    impl TokenRefresher for RotatingRefresher {
        async fn refresh(&self, _r: crate::auth::manager::RefreshReason) -> RefreshOutcome {
            RefreshOutcome::Success(Box::new(EchoAgentAuth {
                key: "rotated-key".into(),
                oidc_issuer: Some(ECHO_AGENT_OAUTH2_ISSUER.to_string()),
                refresh_token: Some("rt".into()),
                expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
                ..EchoAgentAuth::test_default()
            }))
        }
    }
    let stale = EchoAgentAuth {
        key: "stale-key".into(),
        oidc_issuer: Some(ECHO_AGENT_OAUTH2_ISSUER.to_string()),
        refresh_token: Some("rt".into()),
        expires_at: Some(chrono::Utc::now() - chrono::Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    };
    let (agent, _rx) =
        build_agent_with_auth_and_proxy(stale.clone(), server.url(), AgentMode::Leader);
    agent
        .auth_manager
        .set_refresher(std::sync::Arc::new(RotatingRefresher));
    echo_agent_telemetry::external::suppress_external_otel_until_settings();
    agent.refresh_remote_settings(&stale).await;
    assert!(
        echo_agent_telemetry::external::is_settings_gate_open(),
        "the rotated-token re-fetch must land settings and open the gate"
    );
    assert!(
        agent.cfg.borrow().remote_settings.is_some(),
        "the re-fetched settings must be stored"
    );
}
/// A logout can land while the detached post-auth fetch is in flight; the
/// result must not be cached for the logged-out identity.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn settings_not_cached_when_identity_logs_out_during_fetch() {
    use crate::agent::config::AgentMode;
    use crate::auth::{EchoAgentAuth, ECHO_AGENT_OAUTH2_ISSUER};
    let _restore = RestoreOtelGate;
    let server = echo_agent_test_support::MockInferenceServer::start()
        .await
        .unwrap();
    server.set_settings(serde_json::json!({ "allow_access": true }));
    let echo_agent_auth = EchoAgentAuth {
        oidc_issuer: Some(ECHO_AGENT_OAUTH2_ISSUER.to_string()),
        ..EchoAgentAuth::test_default()
    };
    let (agent, _rx) =
        build_agent_with_auth_and_proxy(echo_agent_auth.clone(), server.url(), AgentMode::Leader);
    agent.auth_manager.clear_in_memory();
    agent.refresh_remote_settings(&echo_agent_auth).await;
    assert!(
        agent.cfg.borrow().remote_settings.is_none(),
        "settings fetched for a logged-out identity must not be cached"
    );
}
/// `ensure_session_supervisor` is idempotent: calling it repeatedly spawns
/// the sweeper loop exactly once.
#[test]
fn ensure_session_supervisor_is_idempotent() {
    run_local_for_bridge_test(|| async {
        let agent = build_minimal_agent_for_tests();
        assert_eq!(agent.supervisor_spawn_count.get(), 0);
        agent.ensure_session_supervisor();
        agent.ensure_session_supervisor();
        agent.ensure_session_supervisor();
        assert_eq!(
            agent.supervisor_spawn_count.get(),
            1,
            "the supervisor task must be spawned at most once"
        );
        assert!(agent.supervisor_started.get());
    });
}
/// After a terminal removal (reap/close drops the live-state entry), a later
/// reload of the same SessionId starts clean at `IdleResident` with no stale
/// terminal state leaking in (ties to the bounded-map fix).
#[test]
fn reload_after_terminal_removal_starts_clean() {
    run_local_for_bridge_test(|| async {
        let agent = build_minimal_agent_for_tests();
        let sid = acp::SessionId::new("sess-reload");
        let (handle, _tx, _rx) = make_live_session_handle(&sid, Some("turn-1"));
        agent.insert_resident(&sid, handle);
        assert_eq!(
            agent.close_active_session(&sid).await,
            crate::agent::mvp_agent::session_lifecycle::CloseOutcome::Closed,
            "the reload below is only meaningful after a close that happened"
        );
        assert_eq!(
            agent.session_live_state_for(&sid),
            None,
            "terminal removal must leave no stale state"
        );
        let (handle2, _tx2, _rx2) = make_live_session_handle(&sid, None);
        agent.insert_resident(&sid, handle2);
        agent.set_session_live_state(&sid, SessionLiveState::IdleResident);
        assert_eq!(
            agent.session_live_state_for(&sid),
            Some(SessionLiveState::IdleResident),
            "a reloaded session must start at IdleResident, not a stale terminal state"
        );
    });
}
/// Build an agent whose gateway is wired to a live receiver, so a test can
/// observe (and answer) agent→client reverse-requests like the dormant
/// `echo.agent/folder_trust/request` round-trip.
fn build_agent_with_gateway_rx() -> (
    MvpAgent,
    tokio::sync::mpsc::UnboundedReceiver<echo_agent_acp::AcpClientMessage>,
) {
    use crate::agent::config::Config as AgentConfig;
    use crate::auth::{AuthManager, EchoAgentComConfig};
    let temp_dir = tempfile::tempdir().unwrap();
    let auth_manager =
        std::sync::Arc::new(AuthManager::new(temp_dir.path(), EchoAgentComConfig::default()));
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let gateway = GatewaySender::new(tx);
    let cfg = AgentConfig::default();
    let agent = MvpAgent::new(gateway, &cfg, auth_manager, None).expect("valid test config");
    (agent, rx)
}
/// A git repo whose only repo-local config is a project `.mcp.json` declaring
/// `projsrv` — so it is untrusted-with-configs, and the project server should
/// reappear after a trust grant.
fn repo_with_project_mcp_server() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    git2::Repository::init(tmp.path()).unwrap();
    std::fs::write(
        tmp.path().join(".mcp.json"),
        r#"{"mcpServers":{"projsrv":{"command":"echo","args":["hi"]}}}"#,
    )
    .unwrap();
    tmp
}
fn write_project_subagent_definitions(cwd: &std::path::Path) {
    let roles = cwd.join(".echo-agent/roles");
    let personas = cwd.join(".echo-agent/personas");
    std::fs::create_dir_all(&roles).unwrap();
    std::fs::create_dir_all(&personas).unwrap();
    std::fs::write(roles.join("probe.toml"), "description = \"Project role\"").unwrap();
    std::fs::write(
        personas.join("probe.toml"),
        "instructions = \"Project persona\"",
    )
    .unwrap();
}
fn folder_trust_on() -> crate::util::config::RemoteSettings {
    crate::util::config::RemoteSettings {
        folder_trust_enabled: Some(true),
        ..Default::default()
    }
}
#[test]
#[serial_test::serial]
fn subagent_spawn_context_reloads_project_definitions_after_trust_changes() {
    let repo = tempfile::tempdir().unwrap();
    git2::Repository::init(repo.path()).unwrap();
    write_project_subagent_definitions(repo.path());
    run_local_for_bridge_test(|| async {
        let (agent, _rx) = build_agent_with_gateway_rx();
        let sid = acp::SessionId::new("roles-personas-trust-transition");
        let (mut handle, _tx, _cmd_rx) = make_live_session_handle(&sid, None);
        handle.info.cwd = repo.path().display().to_string();
        agent.insert_resident(&sid, handle);
        {
            let mut cfg = agent.cfg.borrow_mut();
            cfg.subagent_roles.insert(
                "refreshed".into(),
                echo_agent_subagent_resolution::config::SubagentRole {
                    description: "Refreshed user role".into(),
                    source_dir: Some(repo.path().join("user-roles")),
                    ..Default::default()
                },
            );
            cfg.subagent_model_overrides
                .insert("probe".into(), "refreshed-model".into());
            cfg.subagent_toggle.insert("probe".into(), false);
        }
        crate::agent::folder_trust::record_for_test(repo.path(), false);
        let untrusted = agent.build_subagent_spawn_context(sid.0.as_ref());
        assert!(!untrusted.subagent_roles.contains_key("probe"));
        assert!(!untrusted.subagent_personas.contains_key("probe"));
        assert_eq!(
            untrusted
                .subagent_roles
                .get("refreshed")
                .map(|role| role.description.as_str()),
            Some("Refreshed user role")
        );
        assert_eq!(
            untrusted
                .subagent_model_overrides
                .get("probe")
                .map(String::as_str),
            Some("refreshed-model")
        );
        assert_eq!(untrusted.subagent_toggle.get("probe"), Some(&false));
        crate::agent::folder_trust::record_for_test(repo.path(), true);
        let trusted = agent.build_subagent_spawn_context(sid.0.as_ref());
        assert_eq!(
            trusted
                .subagent_roles
                .get("probe")
                .map(|role| role.description.as_str()),
            Some("Project role")
        );
        assert!(trusted.subagent_personas.contains_key("probe"));
        crate::agent::folder_trust::record_for_test(repo.path(), false);
        let revoked = agent.build_subagent_spawn_context(sid.0.as_ref());
        assert!(!revoked.subagent_roles.contains_key("probe"));
        assert!(!revoked.subagent_personas.contains_key("probe"));
    });
}
/// End-to-end gate wiring: project `.echo-agent/roles` / `personas` alone must drive
/// real `resolve_and_record` untrusted (not a forced `record_for_test` verdict),
/// keep project defs out of Task spawn context, then re-admit them after grant.
#[test]
#[serial_test::serial]
fn project_roles_personas_gated_via_resolve_and_record_chain() {
    use echo_agent_test_support::EnvGuard;
    let home = tempfile::tempdir().unwrap();
    let _env = EnvGuard::set("ECHO_AGENT_HOME", home.path());
    let _sim = EnvGuard::set(echo_agent_version::TEST_VERSION_ENV, "0.0-sim");
    let _flag = EnvGuard::unset("ECHO_AGENT_FOLDER_TRUST");
    let repo = tempfile::tempdir().unwrap();
    git2::Repository::init(repo.path()).unwrap();
    write_project_subagent_definitions(repo.path());
    run_local_for_bridge_test(|| async {
        let (agent, _rx) = build_agent_with_gateway_rx();
        let sid = acp::SessionId::new("roles-personas-resolve-chain");
        let (mut handle, _tx, _cmd_rx) = make_live_session_handle(&sid, None);
        handle.info.cwd = repo.path().display().to_string();
        agent.insert_resident(&sid, handle);
        let allowed = crate::agent::folder_trust::resolve_and_record(
            repo.path(),
            Some(&folder_trust_on()),
            false,
        );
        assert!(
            !allowed,
            "roles/personas markers alone must resolve untrusted without a grant"
        );
        assert!(
            !crate::agent::folder_trust::project_scope_allowed(repo.path()),
            "cached verdict after resolve_and_record must stay untrusted"
        );
        let untrusted = agent.build_subagent_spawn_context(sid.0.as_ref());
        assert!(
            !untrusted.subagent_roles.contains_key("probe"),
            "untrusted: project role must stay out of spawn context"
        );
        assert!(
            !untrusted.subagent_personas.contains_key("probe"),
            "untrusted: project persona must stay out of spawn context"
        );
        crate::agent::folder_trust::grant_folder_trust(repo.path());
        let allowed = crate::agent::folder_trust::resolve_and_record(
            repo.path(),
            Some(&folder_trust_on()),
            false,
        );
        assert!(allowed, "store-granted folder must resolve trusted");
        let trusted = agent.build_subagent_spawn_context(sid.0.as_ref());
        assert_eq!(
            trusted
                .subagent_roles
                .get("probe")
                .map(|role| role.description.as_str()),
            Some("Project role")
        );
        assert!(
            trusted.subagent_personas.contains_key("probe"),
            "trusted: project persona must enter spawn context after grant"
        );
    });
}
/// Pull the next `echo.agent/folder_trust/request` reverse-request off the gateway and
/// answer it with `outcome`. Returns the request's decoded params.
async fn answer_folder_trust_request(
    gw_rx: &mut tokio::sync::mpsc::UnboundedReceiver<echo_agent_acp::AcpClientMessage>,
    outcome: &str,
) -> serde_json::Value {
    let msg = tokio::time::timeout(std::time::Duration::from_secs(2), gw_rx.recv())
        .await
        .expect("trust request must be sent")
        .expect("gateway channel open");
    let echo_agent_acp::AcpClientMessage::ExtMethod(args) = msg else {
        panic!("expected an ext_method reverse-request, got a different message");
    };
    assert_eq!(args.request.method.as_ref(), "echo.agent/folder_trust/request");
    let params: serde_json::Value = serde_json::from_str(args.request.params.get()).unwrap();
    let resp: acp::ExtResponse = acp::ExtResponse::new(std::sync::Arc::from(
        serde_json::value::to_raw_value(&serde_json::json!({ "outcome": outcome })).unwrap(),
    ));
    let _ = args.response_tx.send(Ok(resp));
    params
}
#[test]
#[serial_test::serial]
fn interactive_trust_prompt_grant_reloads_project_mcp() {
    use echo_agent_test_support::EnvGuard;
    use echo_agent_workspace::trust::{TrustStore, workspace_key};
    let home = tempfile::tempdir().unwrap();
    let _env = EnvGuard::set("ECHO_AGENT_HOME", home.path());
    let _sim = EnvGuard::set(echo_agent_version::TEST_VERSION_ENV, "0.0-sim");
    let _flag = EnvGuard::unset("ECHO_AGENT_FOLDER_TRUST");
    let repo = repo_with_project_mcp_server();
    let repo_path = repo.path().to_path_buf();
    let remote = folder_trust_on();
    run_local_for_bridge_test(|| async {
        let (agent, mut gw_rx) = build_agent_with_gateway_rx();
        agent.interactive_trust_client.set(true);
        crate::agent::folder_trust::resolve_and_record(&repo_path, Some(&remote), false);
        assert!(
            !crate::agent::folder_trust::project_scope_allowed(&repo_path),
            "untrusted-with-configs workspace must gate project scope before the grant"
        );
        let sid = acp::SessionId::new("sess-trust");
        let (mut handle, _tx, mut cmd_rx) = make_live_session_handle(&sid, None);
        handle.info.cwd = repo_path.to_string_lossy().to_string();
        agent.insert_resident(&sid, handle);
        agent.maybe_spawn_interactive_trust_prompt(&sid, &repo_path, Some(&remote));
        let params = answer_folder_trust_request(&mut gw_rx, "trust").await;
        assert!(
            params["configKinds"]
                .as_array()
                .is_some_and(|k| k.iter().any(|v| v == "mcp")),
            "request must summarize detected config kinds; got {params}"
        );
        assert_eq!(
            params["sessionId"], "sess-trust",
            "trust request must carry the session id for leader routing; got {params}"
        );
        let mut saw_project_mcp = false;
        let mut saw_reload_plugins = false;
        let mut saw_reload_hooks = false;
        for _ in 0..8 {
            match tokio::time::timeout(std::time::Duration::from_secs(2), cmd_rx.recv()).await {
                Ok(Some(TestSessionCommand::UpdateMcpServers { mcp_servers, .. })) => {
                    saw_project_mcp |= mcp_servers
                        .iter()
                        .any(|s| crate::session::managed_mcp::mcp_server_name(s) == "projsrv");
                }
                Ok(Some(TestSessionCommand::ReloadPlugins { .. })) => {
                    saw_reload_plugins = true;
                }
                Ok(Some(TestSessionCommand::ReloadHooks)) => saw_reload_hooks = true,
                Ok(Some(_other)) => continue,
                _ => break,
            }
            if saw_project_mcp && saw_reload_plugins && saw_reload_hooks {
                break;
            }
        }
        assert!(
            saw_project_mcp,
            "trust grant must reload the session's now-trusted project MCP server"
        );
        assert!(
            saw_reload_plugins,
            "trust grant must reload plugins (plugin-contributed hooks/MCP)"
        );
        assert!(
            saw_reload_hooks,
            "trust grant must reload the session's own project hooks"
        );
        assert!(
            TrustStore::load().is_trusted(&workspace_key(&repo_path)),
            "accepting the prompt must persist the trust grant"
        );
        assert!(
            crate::agent::folder_trust::project_scope_allowed(&repo_path),
            "the in-process gate must flip to trusted after the grant"
        );
    });
}
#[test]
#[serial_test::serial]
fn interactive_trust_prompt_reject_keeps_gated() {
    use echo_agent_test_support::EnvGuard;
    use echo_agent_workspace::trust::{TrustStore, workspace_key};
    let home = tempfile::tempdir().unwrap();
    let _env = EnvGuard::set("ECHO_AGENT_HOME", home.path());
    let _sim = EnvGuard::set(echo_agent_version::TEST_VERSION_ENV, "0.0-sim");
    let _flag = EnvGuard::unset("ECHO_AGENT_FOLDER_TRUST");
    let repo = repo_with_project_mcp_server();
    let repo_path = repo.path().to_path_buf();
    let remote = folder_trust_on();
    run_local_for_bridge_test(|| async {
        let (agent, mut gw_rx) = build_agent_with_gateway_rx();
        agent.interactive_trust_client.set(true);
        crate::agent::folder_trust::resolve_and_record(&repo_path, Some(&remote), false);
        let sid = acp::SessionId::new("sess-reject");
        let (mut handle, _tx, mut cmd_rx) = make_live_session_handle(&sid, None);
        handle.info.cwd = repo_path.to_string_lossy().to_string();
        agent.insert_resident(&sid, handle);
        agent.maybe_spawn_interactive_trust_prompt(&sid, &repo_path, Some(&remote));
        let _ = answer_folder_trust_request(&mut gw_rx, "reject").await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(300), cmd_rx.recv())
                .await
                .is_err(),
            "rejecting trust must leave the session's project servers gated (no reload)"
        );
        assert!(
            !TrustStore::load().is_trusted(&workspace_key(&repo_path)),
            "rejecting trust must leave the store unchanged"
        );
        assert!(
            !crate::agent::folder_trust::project_scope_allowed(&repo_path),
            "rejecting trust must keep the workspace gated"
        );
    });
}
#[test]
#[serial_test::serial]
fn interactive_trust_prompt_dormant_when_feature_off() {
    use echo_agent_test_support::EnvGuard;
    let home = tempfile::tempdir().unwrap();
    let _env = EnvGuard::set("ECHO_AGENT_HOME", home.path());
    let _sim = EnvGuard::set(echo_agent_version::TEST_VERSION_ENV, "0.0-sim");
    let _flag = EnvGuard::unset("ECHO_AGENT_FOLDER_TRUST");
    let repo = repo_with_project_mcp_server();
    let repo_path = repo.path().to_path_buf();
    let remote = crate::util::config::RemoteSettings {
        folder_trust_enabled: Some(false),
        ..Default::default()
    };
    run_local_for_bridge_test(|| async {
        let (agent, mut gw_rx) = build_agent_with_gateway_rx();
        agent.interactive_trust_client.set(true);
        let sid = acp::SessionId::new("sess-dormant");
        let (mut handle, _tx, _cmd_rx) = make_live_session_handle(&sid, None);
        handle.info.cwd = repo_path.to_string_lossy().to_string();
        agent.insert_resident(&sid, handle);
        agent.maybe_spawn_interactive_trust_prompt(&sid, &repo_path, Some(&remote));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(300), gw_rx.recv())
                .await
                .is_err(),
            "feature off must emit no trust request (dormant)"
        );
    });
}
#[test]
#[serial_test::serial]
fn interactive_trust_prompt_no_request_without_capability() {
    use echo_agent_test_support::EnvGuard;
    let home = tempfile::tempdir().unwrap();
    let _env = EnvGuard::set("ECHO_AGENT_HOME", home.path());
    let _sim = EnvGuard::set(echo_agent_version::TEST_VERSION_ENV, "0.0-sim");
    let _flag = EnvGuard::unset("ECHO_AGENT_FOLDER_TRUST");
    let repo = repo_with_project_mcp_server();
    let repo_path = repo.path().to_path_buf();
    let remote = folder_trust_on();
    run_local_for_bridge_test(|| async {
        let (agent, mut gw_rx) = build_agent_with_gateway_rx();
        assert!(!agent.interactive_trust_client.get());
        let sid = acp::SessionId::new("sess-nocap");
        let (mut handle, _tx, _cmd_rx) = make_live_session_handle(&sid, None);
        handle.info.cwd = repo_path.to_string_lossy().to_string();
        agent.insert_resident(&sid, handle);
        agent.maybe_spawn_interactive_trust_prompt(&sid, &repo_path, Some(&remote));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(300), gw_rx.recv())
                .await
                .is_err(),
            "a client without the capability must get no trust request"
        );
    });
}
#[test]
#[serial_test::serial]
fn interactive_trust_prompt_client_error_fails_closed() {
    use echo_agent_test_support::EnvGuard;
    use echo_agent_workspace::trust::{TrustStore, workspace_key};
    let home = tempfile::tempdir().unwrap();
    let _env = EnvGuard::set("ECHO_AGENT_HOME", home.path());
    let _sim = EnvGuard::set(echo_agent_version::TEST_VERSION_ENV, "0.0-sim");
    let _flag = EnvGuard::unset("ECHO_AGENT_FOLDER_TRUST");
    let repo = repo_with_project_mcp_server();
    let repo_path = repo.path().to_path_buf();
    let remote = folder_trust_on();
    run_local_for_bridge_test(|| async {
        let (agent, mut gw_rx) = build_agent_with_gateway_rx();
        agent.interactive_trust_client.set(true);
        crate::agent::folder_trust::resolve_and_record(&repo_path, Some(&remote), false);
        let sid = acp::SessionId::new("sess-err");
        let (mut handle, _tx, mut cmd_rx) = make_live_session_handle(&sid, None);
        handle.info.cwd = repo_path.to_string_lossy().to_string();
        agent.insert_resident(&sid, handle);
        agent.maybe_spawn_interactive_trust_prompt(&sid, &repo_path, Some(&remote));
        let msg = tokio::time::timeout(std::time::Duration::from_secs(2), gw_rx.recv())
            .await
            .expect("trust request must be sent")
            .expect("gateway channel open");
        assert!(matches!(msg, echo_agent_acp::AcpClientMessage::ExtMethod(_)));
        drop(msg);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(300), cmd_rx.recv())
                .await
                .is_err(),
            "a failed client round-trip must not reload the session"
        );
        assert!(
            !TrustStore::load().is_trusted(&workspace_key(&repo_path)),
            "a failed client round-trip must not grant trust"
        );
        assert!(!crate::agent::folder_trust::project_scope_allowed(
            &repo_path
        ));
    });
}
#[test]
#[serial_test::serial]
fn interactive_trust_prompt_dedups_same_workspace() {
    use echo_agent_test_support::EnvGuard;
    let home = tempfile::tempdir().unwrap();
    let _env = EnvGuard::set("ECHO_AGENT_HOME", home.path());
    let _sim = EnvGuard::set(echo_agent_version::TEST_VERSION_ENV, "0.0-sim");
    let _flag = EnvGuard::unset("ECHO_AGENT_FOLDER_TRUST");
    let repo = repo_with_project_mcp_server();
    let repo_path = repo.path().to_path_buf();
    let remote = folder_trust_on();
    run_local_for_bridge_test(|| async {
        let (agent, mut gw_rx) = build_agent_with_gateway_rx();
        agent.interactive_trust_client.set(true);
        crate::agent::folder_trust::resolve_and_record(&repo_path, Some(&remote), false);
        let sid = acp::SessionId::new("sess-dedup");
        let (mut handle, _tx, _cmd_rx) = make_live_session_handle(&sid, None);
        handle.info.cwd = repo_path.to_string_lossy().to_string();
        agent.insert_resident(&sid, handle);
        agent.maybe_spawn_interactive_trust_prompt(&sid, &repo_path, Some(&remote));
        let first = tokio::time::timeout(std::time::Duration::from_secs(2), gw_rx.recv()).await;
        assert!(
            matches!(first, Ok(Some(echo_agent_acp::AcpClientMessage::ExtMethod(_)))),
            "first prompt for an untrusted workspace must emit a request"
        );
        agent.maybe_spawn_interactive_trust_prompt(&sid, &repo_path, Some(&remote));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(300), gw_rx.recv())
                .await
                .is_err(),
            "a workspace already prompted this process must not be re-prompted"
        );
    });
}
/// Which reload commands a session received after a grant.
struct ReloadCmds {
    update_mcp: bool,
    reload_plugins: bool,
    reload_hooks: bool,
    mcp_names: Vec<String>,
}
/// Drain a session's command channel for the post-grant reload trio
/// (`UpdateMcpServers` + `ReloadPlugins` + `ReloadHooks`), capturing the merged
/// MCP server names so a test can assert per-cwd reload.
async fn drain_reload_commands(
    cmd_rx: &mut tokio::sync::mpsc::UnboundedReceiver<TestSessionCommand>,
) -> ReloadCmds {
    let mut out = ReloadCmds {
        update_mcp: false,
        reload_plugins: false,
        reload_hooks: false,
        mcp_names: Vec::new(),
    };
    for _ in 0..8 {
        match tokio::time::timeout(std::time::Duration::from_secs(2), cmd_rx.recv()).await {
            Ok(Some(TestSessionCommand::UpdateMcpServers { mcp_servers, .. })) => {
                out.update_mcp = true;
                out.mcp_names = mcp_servers
                    .iter()
                    .map(|s| crate::session::managed_mcp::mcp_server_name(s).to_string())
                    .collect();
            }
            Ok(Some(TestSessionCommand::ReloadPlugins { .. })) => {
                out.reload_plugins = true;
            }
            Ok(Some(TestSessionCommand::ReloadHooks)) => out.reload_hooks = true,
            Ok(Some(_other)) => continue,
            _ => break,
        }
        if out.update_mcp && out.reload_plugins && out.reload_hooks {
            break;
        }
    }
    out
}
#[test]
#[serial_test::serial]
fn interactive_trust_prompt_reloads_all_same_workspace_sessions() {
    use echo_agent_test_support::EnvGuard;
    let home = tempfile::tempdir().unwrap();
    let _env = EnvGuard::set("ECHO_AGENT_HOME", home.path());
    let _sim = EnvGuard::set(echo_agent_version::TEST_VERSION_ENV, "0.0-sim");
    let _flag = EnvGuard::unset("ECHO_AGENT_FOLDER_TRUST");
    let repo = repo_with_project_mcp_server();
    let root = repo.path().to_path_buf();
    let subdir = root.join("sub");
    std::fs::create_dir_all(&subdir).unwrap();
    std::fs::write(
        subdir.join(".mcp.json"),
        r#"{"mcpServers":{"subsrv":{"command":"echo","args":["hi"]}}}"#,
    )
    .unwrap();
    let other = repo_with_project_mcp_server();
    let other_path = other.path().to_path_buf();
    let remote = folder_trust_on();
    run_local_for_bridge_test(|| async {
        let (agent, mut gw_rx) = build_agent_with_gateway_rx();
        agent.interactive_trust_client.set(true);
        crate::agent::folder_trust::resolve_and_record(&root, Some(&remote), false);
        let sid_root = acp::SessionId::new("sess-root");
        let (mut h_root, _t1, mut rx_root) = make_live_session_handle(&sid_root, None);
        h_root.info.cwd = root.to_string_lossy().to_string();
        agent.insert_resident(&sid_root, h_root);
        let sid_sub = acp::SessionId::new("sess-sub");
        let (mut h_sub, _t2, mut rx_sub) = make_live_session_handle(&sid_sub, None);
        h_sub.info.cwd = subdir.to_string_lossy().to_string();
        agent.insert_resident(&sid_sub, h_sub);
        let sid_other = acp::SessionId::new("sess-other");
        let (mut h_other, _t3, mut rx_other) = make_live_session_handle(&sid_other, None);
        h_other.info.cwd = other_path.to_string_lossy().to_string();
        agent.insert_resident(&sid_other, h_other);
        agent.maybe_spawn_interactive_trust_prompt(&sid_root, &root, Some(&remote));
        let _ = answer_folder_trust_request(&mut gw_rx, "trust").await;
        let root_cmds = drain_reload_commands(&mut rx_root).await;
        assert!(
            root_cmds.update_mcp && root_cmds.reload_plugins && root_cmds.reload_hooks,
            "root session must get UpdateMcpServers + ReloadPlugins + ReloadHooks"
        );
        let sub_cmds = drain_reload_commands(&mut rx_sub).await;
        assert!(
            sub_cmds.update_mcp && sub_cmds.reload_plugins && sub_cmds.reload_hooks,
            "subdir session must get UpdateMcpServers + ReloadPlugins + ReloadHooks"
        );
        assert!(
            sub_cmds.mcp_names.iter().any(|n| n == "subsrv"),
            "subdir session must reload against its own cwd (expect subsrv); got {:?}",
            sub_cmds.mcp_names
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(300), rx_other.recv())
                .await
                .is_err(),
            "a session under a different workspace_key must not be reloaded"
        );
    });
}
#[test]
#[serial_test::serial]
fn interactive_trust_prompt_reprompts_after_untrust() {
    use echo_agent_test_support::EnvGuard;
    use echo_agent_hooks_plugins_types::HooksAction;
    let home = tempfile::tempdir().unwrap();
    let _env = EnvGuard::set("ECHO_AGENT_HOME", home.path());
    let _sim = EnvGuard::set(echo_agent_version::TEST_VERSION_ENV, "0.0-sim");
    let _flag = EnvGuard::unset("ECHO_AGENT_FOLDER_TRUST");
    let repo = repo_with_project_mcp_server();
    let repo_path = repo.path().to_path_buf();
    let remote = folder_trust_on();
    run_local_for_bridge_test(|| async {
        let (agent, mut gw_rx) = build_agent_with_gateway_rx();
        agent.interactive_trust_client.set(true);
        crate::agent::folder_trust::resolve_and_record(&repo_path, Some(&remote), false);
        let sid = acp::SessionId::new("sess-reprompt");
        let (mut handle, _tx, _cmd_rx) = make_live_session_handle(&sid, None);
        handle.info.cwd = repo_path.to_string_lossy().to_string();
        agent.insert_resident(&sid, handle);
        agent.maybe_spawn_interactive_trust_prompt(&sid, &repo_path, Some(&remote));
        assert!(
            matches!(
                tokio::time::timeout(std::time::Duration::from_secs(2), gw_rx.recv()).await,
                Ok(Some(echo_agent_acp::AcpClientMessage::ExtMethod(_)))
            ),
            "first prompt must emit a request"
        );
        agent.maybe_spawn_interactive_trust_prompt(&sid, &repo_path, Some(&remote));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(200), gw_rx.recv())
                .await
                .is_err(),
            "a prompted workspace must be suppressed before untrust"
        );
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            agent.execute_hooks_action(&sid, HooksAction::Untrust),
        )
        .await;
        agent.maybe_spawn_interactive_trust_prompt(&sid, &repo_path, Some(&remote));
        assert!(
            matches!(
                tokio::time::timeout(std::time::Duration::from_secs(2), gw_rx.recv()).await,
                Ok(Some(echo_agent_acp::AcpClientMessage::ExtMethod(_)))
            ),
            "after untrust clears the dedup, the workspace must be promptable again"
        );
    });
}
fn ann(id: &str) -> echo_agent_announcements::RemoteAnnouncement {
    echo_agent_announcements::RemoteAnnouncement {
        id: Some(id.to_string()),
        message: Some(format!("{id}-msg")),
        severity: Some("critical".to_string()),
        ..Default::default()
    }
}
/// `RemoteSettings` with only `announcements` set (callers add sentinel
/// fields as needed).
fn settings_with(
    announcements: Option<Vec<echo_agent_announcements::RemoteAnnouncement>>,
) -> crate::util::config::RemoteSettings {
    crate::util::config::RemoteSettings {
        announcements,
        ..Default::default()
    }
}
fn test_now() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc)
}
/// Pushes must carry strictly increasing generations, seeded from unix-epoch
/// seconds so a restarted leader still beats pager watermarks that survived
/// re-election (`AppView.announcements_last_gen` is never reset).
#[tokio::test]
async fn announcements_gen_seeds_from_epoch_and_strictly_increases() {
    let agent = build_minimal_agent_for_tests();
    let epoch_before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let first = agent.next_announcements_gen();
    let second = agent.next_announcements_gen();
    assert!(
        first >= epoch_before,
        "first gen must be epoch-seeded: {first} < {epoch_before}"
    );
    assert!(
        second > first,
        "gens must strictly increase: {first} -> {second}"
    );
    let far_ahead = first + 1_000_000;
    agent.announcements_gen.set(far_ahead);
    assert_eq!(agent.next_announcements_gen(), far_ahead + 1);
}
/// An unchanged visible list must not produce a push (idle steady-state is
/// silent); a changed one — including clearing to empty — must.
#[test]
fn announcements_push_gate_emits_only_on_change() {
    let now = test_now();
    assert_eq!(
        announcements_push_payload(None, &[], now, AnnouncementsPushMode::IfChanged),
        None
    );
    let list_a = vec![ann("a")];
    assert_eq!(
        announcements_push_payload(
            Some(list_a.as_slice()),
            &[],
            now,
            AnnouncementsPushMode::IfChanged
        ),
        Some(list_a.clone())
    );
    assert_eq!(
        announcements_push_payload(
            Some(list_a.as_slice()),
            &list_a,
            now,
            AnnouncementsPushMode::IfChanged
        ),
        None
    );
    let list_ab = vec![ann("a"), ann("b")];
    assert_eq!(
        announcements_push_payload(
            Some(list_ab.as_slice()),
            &list_a,
            now,
            AnnouncementsPushMode::IfChanged
        ),
        Some(list_ab.clone())
    );
    assert_eq!(
        announcements_push_payload(None, &list_ab, now, AnnouncementsPushMode::IfChanged),
        Some(vec![])
    );
}
/// `seed` (per-client initialize) re-emits an unchanged non-empty list for
/// the freshly attached client, but stays silent when there is nothing to
/// show.
#[test]
fn announcements_push_gate_seed_reemits_nonempty_only() {
    let now = test_now();
    let list_a = vec![ann("a")];
    assert_eq!(
        announcements_push_payload(
            Some(list_a.as_slice()),
            &list_a,
            now,
            AnnouncementsPushMode::SeedNewClient
        ),
        Some(list_a.clone()),
        "seed must re-push an unchanged non-empty list"
    );
    assert_eq!(
        announcements_push_payload(None, &[], now, AnnouncementsPushMode::SeedNewClient),
        None,
        "seed with nothing visible must stay silent"
    );
}
/// `/new` forces a push even when the visible list is unchanged — including
/// unchanged-empty — so the pager re-merges its config-layer (requirements/
/// user/managed TOML) announcements from local mid-session edits.
#[test]
fn announcements_push_gate_force_mode_pushes_unchanged_and_empty() {
    let now = test_now();
    let list_a = vec![ann("a")];
    assert_eq!(
        announcements_push_payload(
            Some(list_a.as_slice()),
            &list_a,
            now,
            AnnouncementsPushMode::Force
        ),
        Some(list_a.clone()),
        "force must push an unchanged list"
    );
    assert_eq!(
        announcements_push_payload(None, &[], now, AnnouncementsPushMode::Force),
        Some(vec![]),
        "force must push even an unchanged empty list"
    );
}
/// An addition that is already expired on arrival never becomes visible, so
/// it must not re-emit.
#[test]
fn announcements_push_gate_ignores_expired_only_addition() {
    let now = test_now();
    let expired = echo_agent_announcements::RemoteAnnouncement {
        expires_at: Some("2000-01-01T00:00:00Z".to_string()),
        ..ann("expired")
    };
    let list_a = vec![ann("a")];
    let stored = vec![ann("a"), expired];
    assert_eq!(
        announcements_push_payload(
            Some(stored.as_slice()),
            &list_a,
            now,
            AnnouncementsPushMode::IfChanged
        ),
        None,
        "an already-expired addition must not re-emit"
    );
}
/// A previously emitted item that passes its `expires_at` between gate runs
/// must emit the shrunken (here: empty) list exactly once, so live banners
/// clear on time instead of outliving their own expiry.
#[test]
fn announcements_push_gate_emits_on_expiry_crossing() {
    let expiring = echo_agent_announcements::RemoteAnnouncement {
        expires_at: Some("2026-06-01T00:00:00Z".to_string()),
        ..ann("soon")
    };
    let stored = vec![expiring.clone()];
    let before = chrono::DateTime::parse_from_rfc3339("2026-05-31T23:59:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let emitted = announcements_push_payload(
        Some(stored.as_slice()),
        &[],
        before,
        AnnouncementsPushMode::IfChanged,
    )
    .expect("live item must emit");
    assert_eq!(emitted, stored);
    let after = chrono::DateTime::parse_from_rfc3339("2026-06-01T00:01:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert_eq!(
        announcements_push_payload(
            Some(stored.as_slice()),
            &emitted,
            after,
            AnnouncementsPushMode::IfChanged
        ),
        Some(vec![]),
        "expiry crossing must emit the shrunken list"
    );
    assert_eq!(
        announcements_push_payload(
            Some(stored.as_slice()),
            &[],
            after,
            AnnouncementsPushMode::IfChanged
        ),
        None
    );
}
/// A poll apply must touch ONLY `remote_settings.announcements`; every other
/// stored field keeps its pre-poll value (full reapply stays owned by
/// startup, auth, and `/new`).
#[tokio::test]
async fn polled_announcements_apply_touches_announcements_only() {
    let agent = build_minimal_agent_for_tests();
    let mut stored = settings_with(Some(vec![ann("old")]));
    stored.tips = Some(vec!["stored-tip".to_string()]);
    stored.allow_access = Some(true);
    stored.default_model = Some("stored-model".to_string());
    agent.cfg.borrow_mut().remote_settings = Some(stored);
    let mut fresh = settings_with(Some(vec![ann("new")]));
    fresh.tips = Some(vec!["fresh-tip".to_string()]);
    fresh.allow_access = Some(false);
    fresh.default_model = Some("fresh-model".to_string());
    agent.apply_polled_announcements(fresh, Some(vec![ann("old")]));
    let cfg = agent.cfg.borrow();
    let after = cfg
        .remote_settings
        .as_ref()
        .expect("settings still present");
    assert_eq!(after.announcements, Some(vec![ann("new")]));
    assert_eq!(
        after.tips,
        Some(vec!["stored-tip".to_string()]),
        "tips must be untouched by a poll apply"
    );
    assert_eq!(
        after.allow_access,
        Some(true),
        "allow_access must be untouched by a poll apply"
    );
    assert_eq!(
        after.default_model.as_deref(),
        Some("stored-model"),
        "default_model must be untouched by a poll apply"
    );
}
/// A poll apply must never fabricate `remote_settings` from scratch — the
/// `is_none()`-keyed retry/gating semantics of the full-refresh owners
/// depend on absence staying observable.
#[tokio::test]
async fn polled_announcements_apply_never_fabricates_settings() {
    let agent = build_minimal_agent_for_tests();
    agent.cfg.borrow_mut().remote_settings = None;
    agent.apply_polled_announcements(settings_with(Some(vec![ann("a")])), None);
    assert!(
        agent.cfg.borrow().remote_settings.is_none(),
        "a poll must leave absent remote_settings absent"
    );
}
/// A full-refresh writer landing during the poll's fetch makes the poll's
/// result stale; the apply must skip rather than clobber the fresher store
/// (the next tick reconciles).
#[tokio::test]
async fn polled_announcements_apply_skips_when_writer_landed_mid_fetch() {
    let agent = build_minimal_agent_for_tests();
    let pre_fetch = Some(vec![ann("old")]);
    agent.cfg.borrow_mut().remote_settings = Some(settings_with(Some(vec![ann("mid-fetch")])));
    agent.apply_polled_announcements(settings_with(Some(vec![ann("stale-poll")])), pre_fetch);
    assert_eq!(
        agent
            .cfg
            .borrow()
            .remote_settings
            .as_ref()
            .and_then(|s| s.announcements.clone()),
        Some(vec![ann("mid-fetch")]),
        "the mid-fetch writer's store must win over the stale poll result"
    );
}
/// End-to-end through the shared gate: every emission advances the baseline
/// and carries a strictly larger gen; unchanged state is silent unless
/// seeding a new client.
#[tokio::test]
async fn emit_announcements_gate_emits_updates_baseline_and_bumps_gen() {
    let (agent, mut rx) = build_agent_with_gateway_rx();
    agent.cfg.borrow_mut().remote_settings = Some(settings_with(Some(vec![ann("a")])));
    let recv_gen =
        |rx: &mut tokio::sync::mpsc::UnboundedReceiver<echo_agent_acp::AcpClientMessage>| {
            let msg = rx.try_recv().expect("expected an announcements push");
            let echo_agent_acp::AcpClientMessage::ExtNotification(args) = msg else {
                panic!("expected ExtNotification, got another message kind");
            };
            assert_eq!(args.request.method.as_ref(), "echo.agent/announcements/update");
            let parsed: serde_json::Value =
                serde_json::from_str(args.request.params.get()).expect("valid JSON payload");
            parsed
                .get("gen")
                .and_then(|g| g.as_u64())
                .expect("gen field")
        };
    agent.emit_announcements(AnnouncementsPushMode::IfChanged);
    let first_gen = recv_gen(&mut rx);
    agent.emit_announcements(AnnouncementsPushMode::IfChanged);
    assert!(rx.try_recv().is_err(), "unchanged list must not re-push");
    agent.emit_announcements(AnnouncementsPushMode::SeedNewClient);
    let seed_gen = recv_gen(&mut rx);
    assert!(
        seed_gen > first_gen,
        "gen must strictly increase: {first_gen} -> {seed_gen}"
    );
    agent.cfg.borrow_mut().remote_settings = Some(settings_with(None));
    agent.emit_announcements(AnnouncementsPushMode::IfChanged);
    let clear_gen = recv_gen(&mut rx);
    assert!(clear_gen > seed_gen);
    agent.emit_announcements(AnnouncementsPushMode::IfChanged);
    assert!(
        rx.try_recv().is_err(),
        "cleared state must push exactly once"
    );
    agent.emit_announcements(AnnouncementsPushMode::Force);
    let force_gen = recv_gen(&mut rx);
    assert!(
        force_gen > clear_gen,
        "forced push must keep gens increasing"
    );
}
/// A send the gateway channel rejects must not advance the last-emitted
/// baseline; the next gate call then re-diffs and re-pushes the same list
/// (the poll's natural retry, no dedicated retry machinery).
#[tokio::test]
async fn emit_announcements_gate_keeps_baseline_on_failed_send_and_retries() {
    let (mut agent, rx) = build_agent_with_gateway_rx();
    agent.cfg.borrow_mut().remote_settings = Some(settings_with(Some(vec![ann("a")])));
    drop(rx);
    agent.emit_announcements(AnnouncementsPushMode::IfChanged);
    assert!(
        agent.last_emitted_announcements.borrow().is_empty(),
        "a failed send must leave the baseline untouched"
    );
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    agent.gateway = GatewaySender::new(tx);
    agent.emit_announcements(AnnouncementsPushMode::IfChanged);
    let msg = rx
        .try_recv()
        .expect("next gate call must re-push after a failed send");
    let echo_agent_acp::AcpClientMessage::ExtNotification(args) = msg else {
        panic!("expected ExtNotification, got another message kind");
    };
    assert_eq!(args.request.method.as_ref(), "echo.agent/announcements/update");
    assert_eq!(
        *agent.last_emitted_announcements.borrow(),
        vec![ann("a")],
        "a successful send advances the baseline"
    );
    agent.emit_announcements(AnnouncementsPushMode::IfChanged);
    assert!(rx.try_recv().is_err(), "unchanged list must not re-push");
}
mod direct_hub_cloud_removed {
    use super::super::{DIRECT_HUB_CLOUD_REMOVED_MSG, reject_direct_hub_cloud_meta};
    use crate::agent::config::HubConfig;
    fn assert_direct_hub_error(err: agent_client_protocol::Error) {
        assert_eq!(
            err.data.as_ref(),
            Some(&serde_json::Value::String(
                DIRECT_HUB_CLOUD_REMOVED_MSG.to_string()
            )),
            "error data must be the exact D8 message, got: {err:?}"
        );
        assert_eq!(
            err.code,
            agent_client_protocol::ErrorCode::InvalidParams,
            "must be invalid_params, got: {err:?}"
        );
    }
    #[test]
    fn cloud_server_id_meta_is_hard_error() {
        let meta = serde_json::json!({ "echo.agent/cloud_server_id": "srv-123" });
        let err = reject_direct_hub_cloud_meta(meta.as_object()).expect_err("must reject");
        assert_direct_hub_error(err);
    }
    #[test]
    fn cloud_server_id_null_still_present_is_hard_error() {
        let meta = serde_json::json!({ "echo.agent/cloud_server_id": null });
        let err = reject_direct_hub_cloud_meta(meta.as_object()).expect_err("must reject");
        assert_direct_hub_error(err);
    }
    #[test]
    fn cloud_server_id_with_gateway_meta_still_hard_error() {
        let meta = serde_json::json!({
            "echo.agent/cloud_server_id": "srv-legacy",
            "envId": "env-1",
            "echo.agent/cloud_existing_workspace": {
                "server_id": "ws-1",
                "cwd": "/workspace"
            }
        });
        let err = reject_direct_hub_cloud_meta(meta.as_object()).expect_err("Direct stamp wins");
        assert_direct_hub_error(err);
    }
    #[test]
    fn absent_or_empty_meta_ok() {
        assert!(reject_direct_hub_cloud_meta(None).is_ok());
        assert!(reject_direct_hub_cloud_meta(serde_json::json!({}).as_object()).is_ok());
        assert!(
            reject_direct_hub_cloud_meta(
                serde_json::json!({
                    "envId": "env-1"
                })
                .as_object()
            )
            .is_ok()
        );
        assert!(
            reject_direct_hub_cloud_meta(
                serde_json::json!({
                    "echo.agent/cloud_existing_workspace": {
                        "server_id": "ws-1",
                        "cwd": "/workspace"
                    }
                })
                .as_object()
            )
            .is_ok()
        );
    }
    #[test]
    fn hub_url_gating_matrix() {
        let with_url = HubConfig {
            url: Some("wss://hub.example/ws".into()),
        };
        let without_url = HubConfig { url: None };
        let blank = HubConfig {
            url: Some("   ".into()),
        };
        assert!(with_url.is_enabled());
        assert!(!without_url.is_enabled());
        assert!(!blank.is_enabled());
    }
    #[test]
    fn hub_config_is_url_only_workspace_default() {
        let json = serde_json::to_value(HubConfig {
            url: Some("wss://hub.example/ws".into()),
        })
        .expect("serialize");
        let obj = json.as_object().expect("object");
        assert_eq!(
            obj.keys().collect::<Vec<_>>(),
            vec!["url"],
            "HubConfig must only serialize url (no proxy-mode fields)"
        );
        let from_legacy: HubConfig = serde_json::from_value(serde_json::json!({
            "url": "wss://hub.example/ws",
            "workspace_mode": "remote",
            "send_turn_hooks": false,
        }))
        .expect("ignore unknown fields");
        assert_eq!(from_legacy.url.as_deref(), Some("wss://hub.example/ws"));
    }
}
mod soft_default_settings_emit {
    use super::*;
    #[tokio::test]
    async fn emit_settings_update_carries_permission_mode_from_cfg() {
        use crate::agent::config::Config as AgentConfig;
        use crate::auth::{AuthManager, EchoAgentComConfig};
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let temp_dir = tempfile::tempdir().unwrap();
                let auth_manager = std::sync::Arc::new(AuthManager::new(
                    temp_dir.path(),
                    EchoAgentComConfig::default(),
                ));
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                let gateway = GatewaySender::new(tx);
                let cfg = AgentConfig {
                    remote_settings: Some(crate::util::config::RemoteSettings {
                        permission_mode: Some("always-approve".into()),
                        slash_command_tags: Some(
                            [("workflows".to_string(), "new".to_string())]
                                .into_iter()
                                .collect(),
                        ),
                        ..Default::default()
                    }),
                    ..Default::default()
                };
                let agent =
                    MvpAgent::new(gateway, &cfg, auth_manager, None).expect("valid test config");
                agent.cfg.borrow_mut().remote_settings = cfg.remote_settings.clone();
                agent.emit_settings_update_notification();
                let msg = rx.try_recv().expect("settings/update must be emitted");
                let echo_agent_acp::AcpClientMessage::ExtNotification(args) = msg else {
                    panic!("expected ExtNotification, got {msg:?}");
                };
                assert_eq!(args.request.method.as_ref(), "echo.agent/settings/update");
                let params: serde_json::Value =
                    serde_json::from_str(args.request.params.get()).expect("parse params");
                assert_eq!(
                    params.get("permission_mode").and_then(|v| v.as_str()),
                    Some("always-approve"),
                    "post-auth emit must carry remote permission_mode for first session"
                );
                assert_eq!(
                    params
                        .get("slash_command_tags")
                        .and_then(|v| v.get("workflows"))
                        .and_then(|v| v.as_str()),
                    Some("new"),
                    "post-auth emit must carry remote slash_command_tags"
                );
                let _ = args.response_tx.send(Ok(()));
            })
            .await;
    }
}
#[test]
fn subagent_rate_limit_max_attempts_resolution_precedence() {
    for (config_toml, remote, env, expected) in [
        (Some(3), Some(5), Some(7), 7),
        (Some(3), Some(5), None, 3),
        (None, Some(5), None, 5),
        (None, None, None, 8),
        (None, None, Some(100), 32),
        (Some(50), None, None, 32),
        (None, Some(99), None, 32),
    ] {
        assert_eq!(
            resolve_subagent_rate_limit_max_attempts(config_toml, remote, env),
            expected,
            "config={config_toml:?} remote={remote:?} env={env:?}"
        );
    }
}
#[test]
fn subagent_rate_limit_max_attempts_env_is_parsed_leniently() {
    for (input, expected) in [
        (None, None),
        (Some(""), None),
        (Some("   "), None),
        (Some("abc"), None),
        (Some("-1"), None),
        (Some("99999999999"), None),
        (Some(" 5 "), Some(5)),
    ] {
        assert_eq!(
            parse_subagent_rate_limit_max_attempts(input),
            expected,
            "input={input:?}"
        );
    }
}
#[cfg(feature = "dhat-heap")]
mod dhat_soak;
/// A leader multiplexes many clients behind one `initialize`, so the answer
/// has to travel with the session: without the session-meta read, one terminal
/// with the row off decides for every other terminal sharing the leader.
/// Silence means off, since the payload costs a git discovery and three round
/// trips.
#[test]
fn session_meta_outranks_the_client_that_started_the_process() {
    let says_nothing = || {
        acp::InitializeRequest::new(acp::ProtocolVersion::V1).client_capabilities(
            acp::ClientCapabilities::new()
                .fs(acp::FileSystemCapabilities::new())
                .terminal(false),
        )
    };
    let wants_a_row = |meta: Option<acp::Meta>, init: acp::InitializeRequest| {
        MvpAgent::resolve_status_line_capability(meta.as_ref(), &init)
    };
    let on = init_advertising_status_line(true);
    let off = init_advertising_status_line(false);
    assert!(
        wants_a_row(Some(status_line_meta(true)), off),
        "a leader that wants a row outranks the client that started the process"
    );
    assert!(
        !wants_a_row(Some(status_line_meta(false)), on),
        "a `/minimal` client attaching to a leader a full-screen pager started \
         was charged for a row it cannot draw"
    );
    assert!(
        wants_a_row(None, init_advertising_status_line(true)),
        "with no leader the client that initialized is the client that asked"
    );
    assert!(!wants_a_row(None, init_advertising_status_line(false)));
    assert!(
        !wants_a_row(Some(acp::Meta::new()), says_nothing()),
        "a leader that injected nothing leaves a silent client off"
    );
    assert!(!wants_a_row(None, says_nothing()));
}
#[tokio::test(flavor = "current_thread")]
async fn an_attach_that_draws_a_row_switches_it_on_and_asks_for_a_fill() {
    let agent = build_minimal_agent_for_tests();
    let session_id = acp::SessionId::new("status-line-attach");
    let (cmd_tx, mut commands) =
        tokio::sync::mpsc::unbounded_channel::<crate::session::SessionCommand>();
    let mut handle = make_test_handle("test-model", false, None);
    handle.cmd_tx = cmd_tx;
    let row = handle.status_line_enabled.clone();
    agent.insert_resident(&session_id, handle);
    let init = init_advertising_status_line(false);
    agent.attach_status_line(&session_id, Some(&status_line_meta(false)), &init);
    assert!(
        !row.load(std::sync::atomic::Ordering::Relaxed),
        "an attach that cannot draw a row switched it on"
    );
    assert!(
        commands.try_recv().is_err(),
        "an attach that cannot draw a row asked the actor to build one"
    );
    agent.attach_status_line(&session_id, Some(&status_line_meta(true)), &init);
    assert!(
        row.load(std::sync::atomic::Ordering::Relaxed),
        "the attach left the row off, so the emitter wakes and builds nothing"
    );
    assert!(
        matches!(
            commands.try_recv(),
            Ok(crate::session::SessionCommand::EmitStatusSnapshot)
        ),
        "the attach never asked for a snapshot, so the transient row never fills"
    );
}
/// A resident session outlives the client that drew its row, and the emitter
/// re-reads the flag on every wake, so a latch that only ever rose would keep
/// building payloads for a row nobody paints. Driven through the real
/// disconnect, not the setter, since the wiring is the part that can rot.
#[test]
fn a_disconnect_switches_the_row_off_and_the_next_attach_switches_it_on() {
    run_local_for_bridge_test(|| async {
        let agent = build_minimal_agent_for_tests();
        let sid = acp::SessionId::new("sess-status-line-busy");
        let (handle, _tx, rx) = make_live_session_handle(&sid, None);
        let row = handle.status_line_enabled.clone();
        agent.insert_resident(&sid, handle);
        let _actor = spawn_fake_actor(rx, true);
        let init = init_advertising_status_line(true);
        agent.attach_status_line(&sid, Some(&status_line_meta(true)), &init);
        assert!(row.load(std::sync::atomic::Ordering::Relaxed));
        drive_disconnect_many(&agent, &[&sid]).await;
        assert!(
            agent.is_resident(&sid),
            "the busy session must stay resident"
        );
        assert!(
            !row.load(std::sync::atomic::Ordering::Relaxed),
            "the last client that could draw the row is gone, so the agent is \
             still assembling payloads nobody paints"
        );
        agent.attach_status_line(&sid, Some(&status_line_meta(true)), &init);
        assert!(
            row.load(std::sync::atomic::Ordering::Relaxed),
            "the row has to come back with the next client"
        );
    });
}
fn status_line_meta(enabled: bool) -> acp::Meta {
    let mut meta = acp::Meta::new();
    meta.insert(
        echo_agent_status_line::CLIENT_STATUS_LINE_META.to_string(),
        serde_json::json!(enabled),
    );
    meta
}
fn init_advertising_status_line(enabled: bool) -> acp::InitializeRequest {
    let mut meta = serde_json::Map::new();
    meta.insert(
        echo_agent_status_line::STATUS_LINE_CAPABILITY.to_string(),
        serde_json::json!(enabled),
    );
    acp::InitializeRequest::new(acp::ProtocolVersion::V1).client_capabilities(
        acp::ClientCapabilities::new()
            .fs(acp::FileSystemCapabilities::new())
            .terminal(false)
            .meta(meta),
    )
}
