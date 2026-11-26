use echo_agent_tools::computer::local::{LocalTerminalBackend, MockFs};
use echo_agent_tools::computer::types::{AsyncFileSystem, TerminalBackend};
use echo_agent_tools::notification::ToolNotificationHandle;
use echo_agent_tools::registry::types::{SessionContext, ToolConfig, ToolServerConfig};

/// A ToolBridge built with a custom FileSystem must route writes through it.
#[tokio::test]
async fn tool_bridge_routes_writes_through_injected_fs() {
    let cwd = std::path::PathBuf::from("/tmp/fs-injection-test-nonexistent");
    let file_path = cwd.join("new.txt");

    let mock_fs = std::sync::Arc::new(MockFs::new());
    let fs: std::sync::Arc<dyn AsyncFileSystem> = mock_fs.clone();
    let terminal: std::sync::Arc<dyn TerminalBackend> =
        std::sync::Arc::new(LocalTerminalBackend::new());

    let builder = crate::tools::bridge::ToolBridge::get_builder();
    let config = ToolServerConfig {
        tools: vec![
            ToolConfig {
                id: "EchoAgentBuild:read_file".into(),
                params: None,
                name_override: None,
                params_name_overrides: None,
                description_override: None,
                behavior_version: None,
                kind: None,
            },
            ToolConfig {
                id: "EchoAgentBuild:search_replace".into(),
                params: Some(
                    serde_json::from_value(serde_json::json!({
                        "skip_read_before_edit": true
                    }))
                    .unwrap(),
                ),
                name_override: None,
                params_name_overrides: None,
                description_override: None,
                behavior_version: None,
                kind: None,
            },
        ],
        behavior_preset: None,
    };
    let ctx = SessionContext {
        backend: terminal,
        fs,
        cwd: cwd.clone(),
        session_folder: std::env::temp_dir().join("echo-agent-test-fs"),
        session_env: std::sync::Arc::new(std::collections::HashMap::new()),
        notification_handle: ToolNotificationHandle::noop(),
        owner_session_id: None,
        subagent: None,
        parent_scheduler_handle: None,
        skills: vec![],
        state_path: std::env::temp_dir().join("echo-agent-test-fs/tool_state.json"),
        memory_backend: None,
        web_search_config: Default::default(),
        web_fetch_config: Default::default(),
        lsp: None,
        image_gen_config: Default::default(),
        video_gen_config: Default::default(),
        app_builder_deployer_config: Default::default(),
        api_key_provider: None,
        auth_provider: None,
        attribution_callback: None,
        system_reminder_tag: echo_agent_tools::reminders::DEFAULT_REMINDER_TAG,
    };
    let bridge = crate::tools::bridge::ToolBridge::finalize_builder(builder, config, ctx)
        .await
        .expect("finalize_builder should succeed");

    // Create a new file via search_replace (old_string="" = new file).
    let result = bridge
        .call(
            "search_replace",
            serde_json::json!({
                "file_path": file_path.to_string_lossy(),
                "old_string": "",
                "new_string": "hello from ACP\n",
            }),
            "test-call-1",
        )
        .await;
    assert!(
        result.is_ok(),
        "search_replace should succeed: {:?}",
        result.err()
    );

    // The write must have landed in MockFs, not on real disk.
    let written = mock_fs
        .get_file(&file_path)
        .await
        .expect("Write went to disk instead of injected FileSystem");
    assert_eq!(String::from_utf8(written).unwrap(), "hello from ACP\n");
}
