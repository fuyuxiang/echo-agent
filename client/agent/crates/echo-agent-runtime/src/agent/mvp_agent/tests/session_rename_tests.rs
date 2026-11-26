//! `echo.agent/session/rename` ext-handler coverage: resident `ManualTitleRenamed`
//! enqueue, non-resident skip, and control-char stripping at the boundary.

use agent_client_protocol as acp;
use echo_agent_test_support::EnvGuard;

use super::{build_minimal_agent_for_tests, make_test_handle};
use crate::session::info::Info;
use crate::session::persistence::PersistenceMsg;
use crate::session::storage::{JsonlStorageAdapter, StorageAdapter};

struct IsolatedHome {
    _dir: tempfile::TempDir,
    _env: EnvGuard,
}

fn isolate_echo_agent_home() -> IsolatedHome {
    let dir = tempfile::tempdir().unwrap();
    let env = EnvGuard::set("ECHO_AGENT_HOME", dir.path());
    IsolatedHome {
        _dir: dir,
        _env: env,
    }
}

async fn drive_rename(
    agent: &crate::agent::mvp_agent::MvpAgent,
    session_id: &str,
    title: &str,
    cwd: &str,
) -> Result<acp::ExtResponse, acp::Error> {
    use acp::Agent as _;
    let params = serde_json::json!({
        "sessionId": session_id,
        "title": title,
        "cwd": cwd,
    });
    let raw = serde_json::value::to_raw_value(&params).unwrap();
    agent
        .ext_method(acp::ExtRequest::new(
            "echo.agent/session/rename",
            std::sync::Arc::from(raw),
        ))
        .await
}

async fn seed_session(info: &Info) {
    let storage = JsonlStorageAdapter::new();
    storage
        .init_session(info, crate::session::persistence::default_model_id())
        .await
        .unwrap();
}

#[tokio::test]
#[serial_test::serial]
async fn rename_enqueues_manual_title_on_resident_persistence_tx() {
    let _home = isolate_echo_agent_home();
    let cwd = "/tmp/rename-resident";
    let sid = acp::SessionId::new("rename-resident-sid");
    let info = Info {
        id: sid.clone(),
        cwd: cwd.to_owned(),
    };
    seed_session(&info).await;

    let agent = build_minimal_agent_for_tests();
    let (persistence_tx, mut persistence_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut handle = make_test_handle("test-model", false, None);
    handle.persistence_tx = persistence_tx;
    handle.info = info.clone();
    agent.insert_resident(&sid, handle);

    let resp = drive_rename(&agent, sid.0.as_ref(), "Manual via ext", cwd)
        .await
        .expect("rename must succeed");
    let wrapper: serde_json::Value = serde_json::from_str(resp.0.get()).unwrap();
    assert_eq!(wrapper["success"], true, "{wrapper}");

    let msg = persistence_rx
        .try_recv()
        .expect("resident rename must enqueue ManualTitleRenamed");
    assert!(
        matches!(msg, PersistenceMsg::ManualTitleRenamed(ref t) if t == "Manual via ext"),
        "expected ManualTitleRenamed after disk write, got {msg:?}"
    );
    assert!(
        persistence_rx.try_recv().is_err(),
        "rename must enqueue exactly one persistence message"
    );

    let summary = JsonlStorageAdapter::new()
        .load_summary(&info)
        .await
        .unwrap();
    assert_eq!(summary.display_title(), "Manual via ext");
    assert!(summary.title_is_manual);
}

#[tokio::test]
#[serial_test::serial]
async fn rename_non_resident_updates_summary_without_panic() {
    let _home = isolate_echo_agent_home();
    let cwd = "/tmp/rename-dormant";
    let sid = acp::SessionId::new("rename-dormant-sid");
    let info = Info {
        id: sid.clone(),
        cwd: cwd.to_owned(),
    };
    seed_session(&info).await;

    let agent = build_minimal_agent_for_tests();
    let resp = drive_rename(&agent, sid.0.as_ref(), "Dormant title", cwd)
        .await
        .expect("non-resident rename must succeed");
    let wrapper: serde_json::Value = serde_json::from_str(resp.0.get()).unwrap();
    assert_eq!(wrapper["success"], true, "{wrapper}");

    let summary = JsonlStorageAdapter::new()
        .load_summary(&info)
        .await
        .unwrap();
    assert_eq!(summary.display_title(), "Dormant title");
    assert!(summary.title_is_manual);
}

#[tokio::test]
#[serial_test::serial]
async fn rename_strips_ascii_controls_before_persist_and_enqueue() {
    let _home = isolate_echo_agent_home();
    let cwd = "/tmp/rename-sanitize";
    let sid = acp::SessionId::new("rename-sanitize-sid");
    let info = Info {
        id: sid.clone(),
        cwd: cwd.to_owned(),
    };
    seed_session(&info).await;

    let agent = build_minimal_agent_for_tests();
    let (persistence_tx, mut persistence_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut handle = make_test_handle("test-model", false, None);
    handle.persistence_tx = persistence_tx;
    handle.info = info.clone();
    agent.insert_resident(&sid, handle);

    // OSC (`ESC ] … BEL`) + CSI (`ESC [ …`) + BEL: C0/C1 strip; leftover
    // `]0;` / `[31m` payload is expected (payload is not CSI).
    let raw = "Hello\u{1b}[31m\u{1b}]0;evil\u{07}World\u{07}";
    let resp = drive_rename(&agent, sid.0.as_ref(), raw, cwd)
        .await
        .expect("sanitized rename must succeed");
    let wrapper: serde_json::Value = serde_json::from_str(resp.0.get()).unwrap();
    assert_eq!(wrapper["success"], true, "{wrapper}");

    let msg = persistence_rx
        .try_recv()
        .expect("resident rename must enqueue stripped title");
    assert!(
        matches!(msg, PersistenceMsg::ManualTitleRenamed(ref t) if t == "Hello[31m]0;evilWorld"),
        "ext boundary must strip OSC/CSI/BEL before ManualTitleRenamed, got {msg:?}"
    );

    let summary = JsonlStorageAdapter::new()
        .load_summary(&info)
        .await
        .unwrap();
    assert_eq!(summary.display_title(), "Hello[31m]0;evilWorld");
}

#[tokio::test]
#[serial_test::serial]
async fn rename_rejects_title_over_max_scalars() {
    use crate::session::persistence::MAX_TITLE_SCALARS;

    let _home = isolate_echo_agent_home();
    let cwd = "/tmp/rename-too-long";
    let sid = acp::SessionId::new("rename-too-long-sid");
    let info = Info {
        id: sid.clone(),
        cwd: cwd.to_owned(),
    };
    seed_session(&info).await;

    let agent = build_minimal_agent_for_tests();
    let seeded = JsonlStorageAdapter::new()
        .load_summary(&info)
        .await
        .unwrap();
    let before_title = seeded.display_title().to_owned();
    let before_manual = seeded.title_is_manual;

    let too_long: String = "é".repeat(MAX_TITLE_SCALARS + 1);
    let err = drive_rename(&agent, sid.0.as_ref(), &too_long, cwd)
        .await
        .expect_err("101 scalars must be rejected");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("title too long") && msg.contains(&MAX_TITLE_SCALARS.to_string()),
        "expected title-too-long error mentioning {MAX_TITLE_SCALARS}, got {msg}"
    );

    let after_reject = JsonlStorageAdapter::new()
        .load_summary(&info)
        .await
        .unwrap();
    assert_eq!(
        after_reject.display_title(),
        before_title,
        "rejected rename must not persist"
    );
    assert_eq!(after_reject.title_is_manual, before_manual);

    let exact: String = "é".repeat(MAX_TITLE_SCALARS);
    let resp = drive_rename(&agent, sid.0.as_ref(), &exact, cwd)
        .await
        .expect("exactly 100 scalars must be accepted");
    let wrapper: serde_json::Value = serde_json::from_str(resp.0.get()).unwrap();
    assert_eq!(wrapper["success"], true, "{wrapper}");

    let summary = JsonlStorageAdapter::new()
        .load_summary(&info)
        .await
        .unwrap();
    assert_eq!(summary.display_title(), exact);
    assert_eq!(summary.display_title().chars().count(), MAX_TITLE_SCALARS);
}

#[tokio::test]
#[serial_test::serial]
async fn rename_counts_scalars_after_control_strip() {
    use crate::session::persistence::MAX_TITLE_SCALARS;

    let _home = isolate_echo_agent_home();
    let cwd = "/tmp/rename-strip-len";
    let sid = acp::SessionId::new("rename-strip-len-sid");
    let info = Info {
        id: sid.clone(),
        cwd: cwd.to_owned(),
    };
    seed_session(&info).await;

    let agent = build_minimal_agent_for_tests();
    // 100 thumbs + one ESC: after strip this is exactly the cap.
    let raw = format!("\u{1b}{}", "👍".repeat(MAX_TITLE_SCALARS));
    let resp = drive_rename(&agent, sid.0.as_ref(), &raw, cwd)
        .await
        .expect("control-stripped 100 scalars must pass");
    let wrapper: serde_json::Value = serde_json::from_str(resp.0.get()).unwrap();
    assert_eq!(wrapper["success"], true, "{wrapper}");

    let summary = JsonlStorageAdapter::new()
        .load_summary(&info)
        .await
        .unwrap();
    let expected = "👍".repeat(MAX_TITLE_SCALARS);
    assert_eq!(summary.display_title(), expected);
    assert!(
        !summary.display_title().contains('\u{1b}'),
        "stripped title must not retain ESC"
    );
}

#[tokio::test]
#[serial_test::serial]
async fn rename_rejects_overlong_after_control_strip() {
    use crate::session::persistence::MAX_TITLE_SCALARS;

    let _home = isolate_echo_agent_home();
    let cwd = "/tmp/rename-strip-reject";
    let sid = acp::SessionId::new("rename-strip-reject-sid");
    let info = Info {
        id: sid.clone(),
        cwd: cwd.to_owned(),
    };
    seed_session(&info).await;
    let seeded = JsonlStorageAdapter::new()
        .load_summary(&info)
        .await
        .unwrap();
    let before = seeded.display_title().to_owned();

    let agent = build_minimal_agent_for_tests();
    let raw = format!("\u{1b}{}", "👍".repeat(MAX_TITLE_SCALARS + 1));
    let err = drive_rename(&agent, sid.0.as_ref(), &raw, cwd)
        .await
        .expect_err("101 scalars after strip must be rejected");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("title too long"),
        "expected title-too-long, got {msg}"
    );
    let after = JsonlStorageAdapter::new()
        .load_summary(&info)
        .await
        .unwrap();
    assert_eq!(after.display_title(), before);
}

#[tokio::test]
#[serial_test::serial]
async fn rename_rejects_title_over_max_bytes_before_strip() {
    use crate::session::persistence::{MAX_TITLE_BYTES, MAX_TITLE_SCALARS};

    let _home = isolate_echo_agent_home();
    let cwd = "/tmp/rename-max-bytes";
    let sid = acp::SessionId::new("rename-max-bytes-sid");
    let info = Info {
        id: sid.clone(),
        cwd: cwd.to_owned(),
    };
    seed_session(&info).await;
    let seeded = JsonlStorageAdapter::new()
        .load_summary(&info)
        .await
        .unwrap();
    let before = seeded.display_title().to_owned();

    let agent = build_minimal_agent_for_tests();
    // 65 C0 + 100 4-byte scalars = MAX_TITLE_BYTES + 1, but only 100
    // scalars after strip — the scalar cap would accept this. Isolates
    // the byte gate: 64 C0 + 100 thumbs is the slack accept below.
    let too_long = format!("{}{}", "\u{1b}".repeat(65), "👍".repeat(MAX_TITLE_SCALARS));
    assert_eq!(too_long.len(), MAX_TITLE_BYTES + 1);
    assert_eq!(
        too_long.chars().filter(|c| !c.is_ascii_control()).count(),
        MAX_TITLE_SCALARS,
        "post-strip scalar count must sit on the cap so only the byte gate rejects"
    );
    let err = drive_rename(&agent, sid.0.as_ref(), &too_long, cwd)
        .await
        .expect_err("byte ceiling must reject before strip");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("title too large"),
        "expected title-too-large byte-gate, got {msg}"
    );
    let after = JsonlStorageAdapter::new()
        .load_summary(&info)
        .await
        .unwrap();
    assert_eq!(after.display_title(), before);

    // Slack: 64 C0 bytes + 100 4-byte scalars still sit on the ceiling.
    let slack = format!("{}{}", "\u{1b}".repeat(64), "👍".repeat(MAX_TITLE_SCALARS));
    assert_eq!(slack.len(), MAX_TITLE_BYTES);
    let resp = drive_rename(&agent, sid.0.as_ref(), &slack, cwd)
        .await
        .expect("byte slack must still accept a 100-scalar title");
    let wrapper: serde_json::Value = serde_json::from_str(resp.0.get()).unwrap();
    assert_eq!(wrapper["success"], true, "{wrapper}");
    let summary = JsonlStorageAdapter::new()
        .load_summary(&info)
        .await
        .unwrap();
    assert_eq!(summary.display_title(), "👍".repeat(MAX_TITLE_SCALARS));
}

#[tokio::test]
#[serial_test::serial]
async fn rename_fanout_stamps_title_is_manual_meta() {
    use crate::agent::config::Config as AgentConfig;
    use crate::auth::{AuthManager, EchoAgentComConfig};
    use crate::extensions::notification::TITLE_IS_MANUAL_META_KEY;
    use echo_agent_acp::{AcpAgentGatewaySender as GatewaySender, AcpClientMessage};

    let _home = isolate_echo_agent_home();
    let cwd = "/tmp/rename-fanout-meta";
    let sid = acp::SessionId::new("rename-fanout-meta-sid");
    let info = Info {
        id: sid.clone(),
        cwd: cwd.to_owned(),
    };
    seed_session(&info).await;

    let temp_dir = tempfile::tempdir().unwrap();
    let auth_manager =
        std::sync::Arc::new(AuthManager::new(temp_dir.path(), EchoAgentComConfig::default()));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let gateway = GatewaySender::new(tx);
    let agent = crate::agent::mvp_agent::MvpAgent::new(
        gateway,
        &AgentConfig::default(),
        auth_manager,
        None,
    )
    .expect("valid test config");
    let mut handle = make_test_handle("test-model", false, None);
    handle.info = info.clone();
    agent.insert_resident(&sid, handle);

    drive_rename(&agent, sid.0.as_ref(), "Fanout Title", cwd)
        .await
        .expect("rename must succeed");

    let mut saw_manual_meta = false;
    while let Ok(msg) = rx.try_recv() {
        let AcpClientMessage::ExtNotification(args) = msg else {
            continue;
        };
        if args.request.method.as_ref() != "echo.agent/session_notification" {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(args.request.params.get()).unwrap();
        if v["_meta"][TITLE_IS_MANUAL_META_KEY] == true {
            saw_manual_meta = true;
            assert_eq!(v["update"]["session_summary"], "Fanout Title");
        }
    }
    assert!(
        saw_manual_meta,
        "drive_rename must stamp _meta.echo.agent/titleIsManual on SessionSummaryGenerated"
    );
}

async fn drive_reset(
    agent: &crate::agent::mvp_agent::MvpAgent,
    session_id: &str,
    cwd: &str,
    extra: serde_json::Value,
) -> Result<acp::ExtResponse, acp::Error> {
    use acp::Agent as _;
    let mut params = serde_json::json!({
        "sessionId": session_id,
        "title": "",
        "cwd": cwd,
        "resetToAuto": true,
    });
    if let Some(obj) = extra.as_object() {
        params.as_object_mut().unwrap().extend(obj.clone());
    }
    let raw = serde_json::value::to_raw_value(&params).unwrap();
    agent
        .ext_method(acp::ExtRequest::new(
            "echo.agent/session/rename",
            std::sync::Arc::from(raw),
        ))
        .await
}

#[tokio::test]
#[serial_test::serial]
async fn reset_enqueues_reset_title_to_auto_on_resident_persistence_tx() {
    let _home = isolate_echo_agent_home();
    let cwd = "/tmp/reset-resident";
    let sid = acp::SessionId::new("reset-resident-sid");
    let info = Info {
        id: sid.clone(),
        cwd: cwd.to_owned(),
    };
    seed_session(&info).await;
    JsonlStorageAdapter::new()
        .update_session_title(&info, "Manual".into())
        .await
        .unwrap();

    let agent = build_minimal_agent_for_tests();
    let (persistence_tx, mut persistence_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut handle = make_test_handle("test-model", false, None);
    handle.persistence_tx = persistence_tx;
    handle.info = info.clone();
    agent.insert_resident(&sid, handle);

    let resp = drive_reset(&agent, sid.0.as_ref(), cwd, serde_json::json!({}))
        .await
        .expect("reset must succeed");
    let wrapper: serde_json::Value = serde_json::from_str(resp.0.get()).unwrap();
    assert_eq!(wrapper["success"], true, "{wrapper}");

    let msg = persistence_rx
        .try_recv()
        .expect("resident reset must enqueue ResetTitleToAuto");
    assert!(
        matches!(msg, PersistenceMsg::ResetTitleToAuto),
        "expected ResetTitleToAuto after disk write, got {msg:?}"
    );
    assert!(
        persistence_rx.try_recv().is_err(),
        "reset must enqueue exactly one persistence message"
    );

    let summary = JsonlStorageAdapter::new()
        .load_summary(&info)
        .await
        .unwrap();
    assert!(!summary.title_is_manual);
    assert!(summary.display_title().trim().is_empty());
}

#[tokio::test]
#[serial_test::serial]
async fn reset_non_resident_updates_summary_without_panic() {
    let _home = isolate_echo_agent_home();
    let cwd = "/tmp/reset-dormant";
    let sid = acp::SessionId::new("reset-dormant-sid");
    let info = Info {
        id: sid.clone(),
        cwd: cwd.to_owned(),
    };
    seed_session(&info).await;
    JsonlStorageAdapter::new()
        .update_session_title(&info, "Manual".into())
        .await
        .unwrap();

    let agent = build_minimal_agent_for_tests();
    let resp = drive_reset(&agent, sid.0.as_ref(), cwd, serde_json::json!({}))
        .await
        .expect("non-resident reset must succeed");
    let wrapper: serde_json::Value = serde_json::from_str(resp.0.get()).unwrap();
    assert_eq!(wrapper["success"], true, "{wrapper}");

    let summary = JsonlStorageAdapter::new()
        .load_summary(&info)
        .await
        .unwrap();
    assert!(!summary.title_is_manual);
    assert!(summary.display_title().trim().is_empty());
}

#[tokio::test]
#[serial_test::serial]
async fn reset_rejects_nonempty_title() {
    let _home = isolate_echo_agent_home();
    let cwd = "/tmp/reset-nonempty";
    let sid = acp::SessionId::new("reset-nonempty-sid");
    let info = Info {
        id: sid.clone(),
        cwd: cwd.to_owned(),
    };
    seed_session(&info).await;
    JsonlStorageAdapter::new()
        .update_session_title(&info, "Manual".into())
        .await
        .unwrap();

    let agent = build_minimal_agent_for_tests();
    let err = drive_reset(
        &agent,
        sid.0.as_ref(),
        cwd,
        serde_json::json!({ "title": "still a title" }),
    )
    .await
    .expect_err("non-empty title + resetToAuto must be rejected");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("title must be empty when resetToAuto is set"),
        "got {msg}"
    );
    let after = JsonlStorageAdapter::new()
        .load_summary(&info)
        .await
        .unwrap();
    assert_eq!(after.display_title(), "Manual");
    assert!(after.title_is_manual);

    let resp = drive_reset(
        &agent,
        sid.0.as_ref(),
        cwd,
        serde_json::json!({ "title": "   " }),
    )
    .await
    .expect("whitespace-only title is empty after sanitize");
    let wrapper: serde_json::Value = serde_json::from_str(resp.0.get()).unwrap();
    assert_eq!(wrapper["success"], true, "{wrapper}");
}

#[tokio::test]
#[serial_test::serial]
async fn reset_rejects_chat_kind() {
    let _home = isolate_echo_agent_home();
    let cwd = "/tmp/reset-chat";
    let sid = acp::SessionId::new("reset-chat-sid");
    let info = Info {
        id: sid.clone(),
        cwd: cwd.to_owned(),
    };
    seed_session(&info).await;
    JsonlStorageAdapter::new()
        .update_session_title(&info, "Manual".into())
        .await
        .unwrap();

    let agent = build_minimal_agent_for_tests();
    let (persistence_tx, mut persistence_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut handle = make_test_handle("test-model", false, None);
    handle.persistence_tx = persistence_tx;
    handle.info = info.clone();
    agent.insert_resident(&sid, handle);

    let err = drive_reset(
        &agent,
        sid.0.as_ref(),
        cwd,
        serde_json::json!({ "kind": "chat" }),
    )
    .await
    .expect_err("chat resetToAuto must be refused");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("chat conversations have no auto-title to restore"),
        "got {msg}"
    );
    assert!(persistence_rx.try_recv().is_err());
    let after = JsonlStorageAdapter::new()
        .load_summary(&info)
        .await
        .unwrap();
    assert_eq!(after.display_title(), "Manual");
    assert!(after.title_is_manual);
}

#[tokio::test]
#[serial_test::serial]
async fn reset_fanout_stamps_title_is_manual_false() {
    use crate::agent::config::Config as AgentConfig;
    use crate::auth::{AuthManager, EchoAgentComConfig};
    use crate::extensions::notification::TITLE_IS_MANUAL_META_KEY;
    use echo_agent_acp::{AcpAgentGatewaySender as GatewaySender, AcpClientMessage};

    let _home = isolate_echo_agent_home();
    let cwd = "/tmp/reset-fanout";
    let sid = acp::SessionId::new("reset-fanout-sid");
    let info = Info {
        id: sid.clone(),
        cwd: cwd.to_owned(),
    };
    seed_session(&info).await;
    JsonlStorageAdapter::new()
        .update_session_title(&info, "Manual".into())
        .await
        .unwrap();

    let temp_dir = tempfile::tempdir().unwrap();
    let auth_manager =
        std::sync::Arc::new(AuthManager::new(temp_dir.path(), EchoAgentComConfig::default()));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let gateway = GatewaySender::new(tx);
    let agent = crate::agent::mvp_agent::MvpAgent::new(
        gateway,
        &AgentConfig::default(),
        auth_manager,
        None,
    )
    .expect("valid test config");
    let mut handle = make_test_handle("test-model", false, None);
    handle.info = info.clone();
    agent.insert_resident(&sid, handle);

    drive_reset(&agent, sid.0.as_ref(), cwd, serde_json::json!({}))
        .await
        .expect("reset must succeed");

    let mut saw_unpin_ext = false;
    let mut saw_unpin_siu = false;
    let mut saw_manual_true = false;
    while let Ok(msg) = rx.try_recv() {
        match msg {
            AcpClientMessage::ExtNotification(args) => {
                if args.request.method.as_ref() != "echo.agent/session_notification" {
                    continue;
                }
                let v: serde_json::Value = serde_json::from_str(args.request.params.get()).unwrap();
                if v["_meta"][TITLE_IS_MANUAL_META_KEY] == false {
                    saw_unpin_ext = true;
                    assert_eq!(v["update"]["session_summary"], "");
                }
                if v["_meta"][TITLE_IS_MANUAL_META_KEY] == true {
                    saw_manual_true = true;
                }
            }
            AcpClientMessage::SessionNotification(args) => {
                let v = serde_json::to_value(&args.request).unwrap();
                if v["_meta"][TITLE_IS_MANUAL_META_KEY] == false {
                    saw_unpin_siu = true;
                    let title = v
                        .pointer("/update/title")
                        .or_else(|| v.pointer("/update/sessionInfoUpdate/title"));
                    assert!(
                        title.is_none(),
                        "unpinned SessionInfoUpdate must omit title: {v}"
                    );
                }
                if v["_meta"][TITLE_IS_MANUAL_META_KEY] == true {
                    saw_manual_true = true;
                }
            }
            _ => {}
        }
    }
    assert!(
        saw_unpin_ext,
        "reset must stamp _meta.echo.agent/titleIsManual=false on SessionSummaryGenerated"
    );
    assert!(
        saw_unpin_siu,
        "resident reset must also send SessionInfoUpdate with titleIsManual=false"
    );
    assert!(!saw_manual_true, "unpin must not stamp titleIsManual true");
}

#[tokio::test]
#[serial_test::serial]
async fn reset_already_auto_is_idempotent_and_skips_persistence_msg() {
    use crate::agent::config::Config as AgentConfig;
    use crate::auth::{AuthManager, EchoAgentComConfig};
    use crate::extensions::notification::TITLE_IS_MANUAL_META_KEY;
    use echo_agent_acp::{AcpAgentGatewaySender as GatewaySender, AcpClientMessage};

    let _home = isolate_echo_agent_home();
    let cwd = "/tmp/reset-idempotent";
    let sid = acp::SessionId::new("reset-idempotent-sid");
    let info = Info {
        id: sid.clone(),
        cwd: cwd.to_owned(),
    };
    seed_session(&info).await;
    JsonlStorageAdapter::new()
        .set_generated_title_if_absent(&info, "Auto".into())
        .await
        .unwrap();

    let temp_dir = tempfile::tempdir().unwrap();
    let auth_manager =
        std::sync::Arc::new(AuthManager::new(temp_dir.path(), EchoAgentComConfig::default()));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let gateway = GatewaySender::new(tx);
    let agent = crate::agent::mvp_agent::MvpAgent::new(
        gateway,
        &AgentConfig::default(),
        auth_manager,
        None,
    )
    .expect("valid test config");
    let (persistence_tx, mut persistence_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut handle = make_test_handle("test-model", false, None);
    handle.persistence_tx = persistence_tx;
    handle.info = info.clone();
    agent.insert_resident(&sid, handle);

    let resp = drive_reset(&agent, sid.0.as_ref(), cwd, serde_json::json!({}))
        .await
        .expect("no-op reset must succeed");
    let wrapper: serde_json::Value = serde_json::from_str(resp.0.get()).unwrap();
    assert_eq!(wrapper["success"], true, "{wrapper}");
    assert!(
        persistence_rx.try_recv().is_err(),
        "no-op reset must not enqueue ResetTitleToAuto"
    );
    let mut saw_unpin = false;
    while let Ok(msg) = rx.try_recv() {
        match msg {
            AcpClientMessage::ExtNotification(args) => {
                if args.request.method.as_ref() != "echo.agent/session_notification" {
                    continue;
                }
                let v: serde_json::Value = serde_json::from_str(args.request.params.get()).unwrap();
                if v["_meta"][TITLE_IS_MANUAL_META_KEY] == false {
                    saw_unpin = true;
                }
            }
            AcpClientMessage::SessionNotification(args) => {
                let v = serde_json::to_value(&args.request).unwrap();
                if v["_meta"][TITLE_IS_MANUAL_META_KEY] == false {
                    saw_unpin = true;
                }
            }
            _ => {}
        }
    }
    assert!(
        !saw_unpin,
        "already-auto unpin must not fan out titleIsManual=false"
    );
    let summary = JsonlStorageAdapter::new()
        .load_summary(&info)
        .await
        .unwrap();
    assert_eq!(summary.display_title(), "Auto");
    assert!(!summary.title_is_manual);
}
