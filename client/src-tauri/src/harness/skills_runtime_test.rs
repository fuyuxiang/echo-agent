use super::*;

#[tokio::test]
async fn checked_refresh_waits_for_runtime_ack_and_surfaces_failure_envelopes() {
    let (client, mut agent) = echo_agent_acp::acp_channels();
    let task = tokio::spawn(async move { refresh_skills_with_tx(&client.tx).await });
    let echo_agent_acp::AcpAgentMessage::ExtMethod(arguments) = agent.rx.recv().await.unwrap()
    else {
        panic!("expected checked reload");
    };
    assert_eq!(
        arguments.request.method.as_ref(),
        "echo.agent/internal/reload_skills"
    );
    assert!(
        !task.is_finished(),
        "disk mutation cannot report success before resident actors acknowledge"
    );
    arguments
        .response_tx
        .send(Ok(acp::ExtResponse::new(raw_params(
            &serde_json::json!({"result":{"reloaded":1}}),
        ))))
        .unwrap();
    assert_eq!(task.await.unwrap(), Ok(()));

    let (client, mut agent) = echo_agent_acp::acp_channels();
    let task = tokio::spawn(async move { refresh_skills_with_tx(&client.tx).await });
    let echo_agent_acp::AcpAgentMessage::ExtMethod(arguments) = agent.rx.recv().await.unwrap()
    else {
        panic!("expected checked reload");
    };
    arguments
        .response_tx
        .send(Ok(acp::ExtResponse::new(raw_params(
            &serde_json::json!({"result":null,"error":"resident actor closed"}),
        ))))
        .unwrap();
    let error = task.await.unwrap().unwrap_err();
    assert!(error.contains("已保存") && error.contains("resident actor closed"));
}

/// Explicit no-model Runtime acceptance. The child owns its entire profile,
/// synthetic compatible HOME, project, auth/config and provider. No HOME edit.
#[test]
#[ignore = "spawns a real Runtime; run once through the isolated host harness"]
fn resident_skill_reload_smoke() {
    if std::env::var("ECHO_AGENT_SKILL_RELOAD_CHILD").as_deref() == Ok("1") {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(resident_reload_child());
        return;
    }
    let profile = tempfile::tempdir().unwrap();
    let home = dunce::canonicalize(profile.path()).unwrap();
    std::fs::create_dir_all(home.join("workspace/.git")).unwrap();
    std::fs::create_dir_all(home.join("compat-home/.claude")).unwrap();
    std::fs::write(
        home.join("compat-home/.claude/settings.json"),
        r#"{"env":{"CONTROLLED_RELOAD_MARKER":"owned"}}"#,
    )
    .unwrap();
    std::fs::create_dir_all(home.join("compat-home/.agents/skills/owned-home-marker")).unwrap();
    std::fs::write(home.join("compat-home/.agents/skills/owned-home-marker/SKILL.md"), "---\nname: owned-home-marker\ndescription: Controlled home marker\n---\nSynthetic fixture only.\n").unwrap();
    std::fs::write(
        home.join("config.toml"),
        r#"
[models]
default = "review/mock"
[model_providers.review]
provider_kind = "custom"
base_url = "http://127.0.0.1:9/v1"
api_key = "synthetic-no-network-calls"
api_backend = "chat_completions"
auth_scheme = "bearer"
[model."review/mock"]
model = "mock"
model_provider = "review"
max_completion_tokens = 1024
[marketplace]
official_marketplace_auto_installed = true
[memory]
enabled = false
[features]
remote_fetch = false
"#,
    )
    .unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child.env_clear();
    for name in [
        "PATH",
        "TMPDIR",
        "TMP",
        "TEMP",
        "SYSTEMROOT",
        "WINDIR",
        "LANG",
        "HOME",
    ] {
        if let Some(value) = std::env::var_os(name) {
            child.env(name, value);
        }
    }
    let qualified_name = concat!(module_path!(), "::resident_skill_reload_smoke");
    let (_, test_name) = qualified_name.split_once("::").unwrap();
    child
        .env("ECHO_AGENT_HOME", &home)
        .env("ECHO_AGENT_REVIEW_ISOLATED", "1")
        .env("ECHO_AGENT_TELEMETRY_ENABLED", "false")
        .env("ECHO_AGENT_TELEMETRY_TRACE_UPLOAD", "false")
        .env("ECHO_AGENT_SKILL_RELOAD_CHILD", "1")
        .args(["--ignored", "--exact", test_name, "--nocapture"]);
    let output = child.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("test result: ok. 1 passed;"),
        "isolated Runtime skill reload failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn resident_reload_child() {
    let home = crate::paths::redirected_data_home().expect("controlled data home required");
    assert_eq!(
        std::env::var("ECHO_AGENT_REVIEW_ISOLATED").as_deref(),
        Ok("1")
    );
    let cwd = home.join("workspace");
    let handle = crate::harness::agent_runtime::spawn_agent_runtime(cwd.clone()).unwrap();
    let run = async {
        let init = crate::harness::agent_runtime::initialize(&handle.tx)
            .await
            .unwrap();
        assert!(init.ok);
        let session = crate::harness::agent_runtime::new_session_with_options(
            &handle.tx,
            &cwd,
            Some("review/mock"),
            None,
            None,
        )
        .await
        .unwrap();
        let command_names = || async {
            let value: serde_json::Value = call_ext(
                &handle.tx,
                "echo.agent/commands/list",
                raw_params(&serde_json::json!({"sessionId":session,"cwd":cwd})),
            )
            .await
            .unwrap();
            value["commands"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|row| row["name"].as_str().map(str::to_string))
                .collect::<Vec<_>>()
        };
        assert!(
            command_names()
                .await
                .iter()
                .any(|name| name.trim_start_matches('/') == "owned-home-marker"),
            "resident session must discover controlled compatibility HOME"
        );
        assert!(!command_names()
            .await
            .iter()
            .any(|name| name.trim_start_matches('/') == "reload-marker"));
        let skill = cwd.join(".agents/skills/reload-marker");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "---\nname: reload-marker\ndescription: Controlled reload marker\n---\nUse the synthetic marker.\n").unwrap();
        refresh_skills_with_tx(&handle.tx).await.unwrap();
        assert!(
            command_names()
                .await
                .iter()
                .any(|name| name.trim_start_matches('/') == "reload-marker"),
            "ACK must publish the changed resident command roster"
        );
        for enabled in [false, true] {
            let _: serde_json::Value = call_ext(
                &handle.tx,
                "echo.agent/skills/toggle",
                raw_params(
                    &serde_json::json!({"name":"reload-marker","enabled":enabled,"cwd":cwd}),
                ),
            )
            .await
            .unwrap();
            refresh_skills_with_tx(&handle.tx).await.unwrap();
            assert_eq!(
                command_names()
                    .await
                    .iter()
                    .any(|name| name.trim_start_matches('/') == "reload-marker"),
                enabled
            );
        }
        std::fs::remove_dir_all(&skill).unwrap();
        refresh_skills_with_tx(&handle.tx).await.unwrap();
        assert!(!command_names()
            .await
            .iter()
            .any(|name| name.trim_start_matches('/') == "reload-marker"));
    };
    tokio::time::timeout(std::time::Duration::from_secs(120), run)
        .await
        .expect("Runtime skill reload exceeded bound");
    handle.cancel.cancel();
    if let Some(thread) = handle.thread {
        let _ = tokio::task::spawn_blocking(move || thread.join()).await;
    }
}
