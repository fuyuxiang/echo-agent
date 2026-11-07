use agent_client_protocol as acp;
use echo_agent_runtime::sampling::ConversationItem;
use echo_agent_runtime::session::{
    info::Info,
    storage::{CopySessionOptions, JsonlStorageAdapter, SessionUpdate, StorageAdapter},
};
use tempfile::TempDir;
// Exercise the exact production title policy in the host regression suite.
#[path = "../../vendor/echo-agent-build/crates/codegen/echo-agent-runtime/src/session/title_recovery_scope.rs"]
mod title_recovery_scope;
fn chat_user(text: &str, index: usize) -> ConversationItem {
    let mut item = ConversationItem::user(text);
    item.set_prompt_index(index);
    item
}
fn fork_user_chunk(id: &str, text: &str, index: usize) -> SessionUpdate {
    SessionUpdate::Acp(Box::new(acp::SessionNotification::new(
        acp::SessionId::new(id),
        acp::SessionUpdate::UserMessageChunk(
            acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(text))).meta(
                serde_json::json!({"promptIndex":index})
                    .as_object()
                    .cloned(),
            ),
        ),
    )))
}
fn fork_agent_chunk(id: &str, text: &str) -> SessionUpdate {
    SessionUpdate::Acp(Box::new(acp::SessionNotification::new(
        acp::SessionId::new(id),
        acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
            acp::TextContent::new(text),
        ))),
    )))
}
#[tokio::test]
async fn desktop_continuation_cuts_model_history_and_gets_own_title() {
    continuation_scenario(false).await;
}

#[tokio::test]
async fn desktop_continuation_before_compaction_excludes_future_summary() {
    continuation_scenario(true).await;
}

async fn continuation_scenario(compacted: bool) {
    let tmp = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(tmp.path().to_path_buf());
    let source = Info {
        id: acp::SessionId::new("original"),
        cwd: "/workspace".into(),
    };
    let child = Info {
        id: acp::SessionId::new("continued"),
        cwd: "/workspace".into(),
    };
    adapter
        .init_session(&source, acp::ModelId::new("test-model"))
        .await
        .unwrap();
    adapter
        .update_session_title(&source, "用户命名的原对话".into())
        .await
        .unwrap();
    for (index, text) in ["first", "selected", "future-secret"]
        .into_iter()
        .enumerate()
    {
        adapter
            .append_chat_message(&source, &chat_user(text, index))
            .await
            .unwrap();
        adapter
            .append_chat_message(&source, &ConversationItem::assistant(text))
            .await
            .unwrap();
        adapter
            .append_update(&source, &fork_user_chunk("original", text, index))
            .await
            .unwrap();
        adapter
            .append_update(&source, &fork_agent_chunk("original", text))
            .await
            .unwrap();
    }
    if compacted {
        use echo_agent_runtime::extensions::notification::{
            CompactionCheckpointFile, CompactionCheckpointInfo, SessionNotification,
            SessionUpdate as EchoUpdate,
        };
        let checkpoint = CompactionCheckpointInfo {
            checkpoint_id: "after-future".into(),
            prompt_index_at_compaction: 3,
            checkpoint_file: "compaction_checkpoints/after-future.json".into(),
            auto_continue: None,
            schema_version: 1,
            created_at: "2026-11-03T00:00:00Z".into(),
        };
        let dir = tmp
            .path()
            .join("sessions")
            .join(echo_agent_runtime::util::echo_agent_home::encode_cwd_dirname(&source.cwd))
            .join("original");
        std::fs::create_dir_all(dir.join("compaction_checkpoints")).unwrap();
        let history = vec![ConversationItem::user("compacted future-secret summary")];
        let file = CompactionCheckpointFile {
            checkpoint_id: checkpoint.checkpoint_id.clone(),
            prompt_index_at_compaction: 3,
            compacted_history: history.clone(),
            schema_version: 1,
            created_at: checkpoint.created_at.clone(),
            original_user_info: None,
            reread_file_paths: vec![],
        };
        std::fs::write(
            dir.join(&checkpoint.checkpoint_file),
            serde_json::to_vec(&file).unwrap(),
        )
        .unwrap();
        adapter
            .append_update(
                &source,
                &SessionUpdate::EchoAgent(Box::new(SessionNotification {
                    session_id: source.id.clone(),
                    update: EchoUpdate::CompactionCheckpoint(Box::new(checkpoint)),
                    meta: None,
                })),
            )
            .await
            .unwrap();
        let mut compacted = history;
        compacted.push(chat_user("post-compaction future-secret", 3));
        let raw = compacted
            .iter()
            .map(|item| serde_json::to_string(item).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(dir.join("chat_history.jsonl"), raw).unwrap();
    }
    adapter
        .copy_session_data(
            &source,
            &child,
            CopySessionOptions {
                parent_session_id: Some("original".into()),
                target_prompt_index: Some(1),
                fresh_title: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let copied = adapter.load_session(&child).await.unwrap();
    let history = serde_json::to_string(&copied.chat_history).unwrap();
    assert!(history.contains("selected"));
    assert!(!history.contains("future-secret"));
    assert!(!serde_json::to_string(&copied.updates)
        .unwrap()
        .contains("future-secret"));
    assert!(copied.summary.display_title().is_empty());
    assert!(!copied.summary.title_is_manual);
    assert_eq!(copied.summary.title_source_prompt_index, Some(2));
    assert_eq!(copied.summary.inherited_prefix_len, None);
    assert_eq!(
        adapter
            .load_session(&source)
            .await
            .unwrap()
            .summary
            .display_title(),
        "用户命名的原对话"
    );
    assert!(adapter
        .set_generated_title_if_absent(&child, "弹窗新方案".into())
        .await
        .unwrap());
    assert_eq!(
        adapter
            .load_session(&child)
            .await
            .unwrap()
            .summary
            .display_title(),
        "弹窗新方案"
    );
    adapter.delete_session(&source).await.unwrap();
    let restarted = JsonlStorageAdapter::with_root(tmp.path().to_path_buf());
    let restored = restarted.load_session(&child).await.unwrap();
    assert_eq!(
        restored.summary.parent_session_id.as_deref(),
        Some("original")
    );
    assert_eq!(restored.chat_history.len(), copied.chat_history.len());
}

async fn append_acp(adapter: &JsonlStorageAdapter, info: &Info, update: acp::SessionUpdate) {
    adapter
        .append_update(
            info,
            &SessionUpdate::Acp(Box::new(acp::SessionNotification::new(
                info.id.clone(),
                update,
            ))),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn continuation_before_compaction_retains_tools_and_images_and_resets_current_plan() {
    rich_context_scenario(false, true).await;
}

#[tokio::test]
async fn continuation_after_compaction_retains_tools_and_images_and_resets_current_plan() {
    rich_context_scenario(true, true).await;
}

#[tokio::test]
async fn continuation_keeps_reused_tool_ids_in_separate_turns() {
    rich_context_scenario(false, false).await;
}

async fn rich_context_scenario(checkpoint_before_selected_turn: bool, rewound: bool) {
    use base64::Engine;
    use echo_agent_runtime::extensions::notification::{
        CompactionCheckpointFile, CompactionCheckpointInfo, SessionNotification,
        SessionUpdate as EchoUpdate,
    };
    use echo_agent_runtime::sampling::{has_dangling_tool_calls, ContentPart};
    let tmp = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(tmp.path().to_path_buf());
    let source = Info {
        id: acp::SessionId::new("source"),
        cwd: "/workspace".into(),
    };
    let child = Info {
        id: acp::SessionId::new("child"),
        cwd: source.cwd.clone(),
    };
    adapter
        .init_session(&source, acp::ModelId::new("test-model"))
        .await
        .unwrap();
    adapter
        .append_update(&source, &fork_user_chunk("source", "original", 0))
        .await
        .unwrap();
    append_acp(
        &adapter,
        &source,
        acp::SessionUpdate::ToolCall(
            acp::ToolCall::new("read-1", "read_file")
                .status(acp::ToolCallStatus::Completed)
                .raw_output(serde_json::json!({"evidence":"ORIGINAL_TOOL_RESULT"})),
        ),
    )
    .await;
    adapter
        .append_update(&source, &fork_agent_chunk("source", "original answer"))
        .await
        .unwrap();
    let dir = adapter
        .updates_file_path(&source)
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::create_dir_all(dir.join("compaction_checkpoints")).unwrap();
    let checkpoint_index = if checkpoint_before_selected_turn {
        1
    } else {
        3
    };
    let checkpoint = CompactionCheckpointInfo {
        checkpoint_id: "compact".into(),
        prompt_index_at_compaction: checkpoint_index,
        checkpoint_file: "compaction_checkpoints/compact.json".into(),
        auto_continue: None,
        schema_version: 1,
        created_at: "2026-11-03T00:00:00Z".into(),
    };
    let file = CompactionCheckpointFile {
        checkpoint_id: "compact".into(),
        prompt_index_at_compaction: checkpoint_index,
        compacted_history: vec![ConversationItem::user("summary of prior turns")],
        schema_version: 1,
        created_at: checkpoint.created_at.clone(),
        original_user_info: None,
        reread_file_paths: vec![],
    };
    std::fs::write(
        dir.join(&checkpoint.checkpoint_file),
        serde_json::to_vec(&file).unwrap(),
    )
    .unwrap();
    let checkpoint_update = SessionUpdate::EchoAgent(Box::new(SessionNotification {
        session_id: source.id.clone(),
        update: EchoUpdate::CompactionCheckpoint(Box::new(checkpoint)),
        meta: None,
    }));
    if checkpoint_before_selected_turn {
        adapter
            .append_update(&source, &checkpoint_update)
            .await
            .unwrap();
    }
    // Reused turn/tool ids from a discarded branch must never contaminate
    // the selected history, including on either side of a checkpoint.
    if rewound {
        adapter
            .append_update(&source, &fork_user_chunk("source", "discarded-secret", 1))
            .await
            .unwrap();
        append_acp(
            &adapter,
            &source,
            acp::SessionUpdate::ToolCall(
                acp::ToolCall::new("read-1", "read_file")
                    .status(acp::ToolCallStatus::Completed)
                    .raw_output(serde_json::json!({"evidence":"discarded-secret"})),
            ),
        )
        .await;
        adapter
            .append_update(
                &source,
                &SessionUpdate::EchoAgent(Box::new(SessionNotification {
                    session_id: source.id.clone(),
                    update: EchoUpdate::RewindMarker {
                        target_prompt_index: 1,
                        created_at: "2026-11-03T00:00:00Z".into(),
                    },
                    meta: None,
                })),
            )
            .await
            .unwrap();
    }
    adapter
        .append_update(&source, &fork_user_chunk("source", "selected", 1))
        .await
        .unwrap();
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(32, 32, image::Rgba([20, 30, 40, 255]))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let encoded = base64::engine::general_purpose::STANDARD.encode(png.into_inner());
    let image = acp::ContentBlock::Image(
        acp::ImageContent::new(encoded.clone(), "image/png").uri("/original/image.png"),
    );
    append_acp(
        &adapter,
        &source,
        acp::SessionUpdate::UserMessageChunk(
            acp::ContentChunk::new(image.clone())
                .meta(serde_json::json!({"promptIndex":1}).as_object().cloned()),
        ),
    )
    .await;
    let start = acp::SessionNotification::new(
        source.id.clone(),
        acp::SessionUpdate::ToolCall(acp::ToolCall::new("read-1", "读取资料")),
    )
    .meta(
        serde_json::json!({"echo.agent/tool":{"version":1,"name":"read_file"}})
            .as_object()
            .cloned(),
    );
    adapter
        .append_update(&source, &SessionUpdate::Acp(Box::new(start)))
        .await
        .unwrap();
    let result = acp::ToolCallUpdate::new(
        "read-1",
        acp::ToolCallUpdateFields::new()
            .status(acp::ToolCallStatus::Completed)
            .raw_input(serde_json::json!({"path":"data.txt"}))
            .raw_output(serde_json::json!({"evidence":"initial result"}))
            .content(vec![acp::ToolCallContent::Content(acp::Content::new(
                image,
            ))]),
    );
    append_acp(
        &adapter,
        &source,
        acp::SessionUpdate::ToolCallUpdate(result),
    )
    .await;
    // Results can be enriched after a terminal status without repeating it.
    append_acp(
        &adapter,
        &source,
        acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
            "read-1",
            acp::ToolCallUpdateFields::new()
                .raw_output(serde_json::json!({"evidence":"UNIQUE_TOOL_EVIDENCE=42"})),
        )),
    )
    .await;
    adapter
        .append_update(&source, &fork_agent_chunk("source", "selected answer"))
        .await
        .unwrap();
    let plan: acp::SessionUpdate = serde_json::from_value(serde_json::json!({"sessionUpdate":"plan","entries":[{"content":"old plan","priority":"high","status":"in_progress"}]})).unwrap();
    append_acp(&adapter, &source, plan).await;
    adapter
        .append_update(&source, &fork_user_chunk("source", "future-secret", 2))
        .await
        .unwrap();
    adapter
        .append_update(&source, &fork_agent_chunk("source", "future-secret answer"))
        .await
        .unwrap();
    if !checkpoint_before_selected_turn {
        adapter
            .append_update(&source, &checkpoint_update)
            .await
            .unwrap();
    }
    // The live model cache no longer contains the selected tool turn.
    std::fs::write(
        dir.join("chat_history.jsonl"),
        serde_json::to_string(&ConversationItem::user("compacted current history")).unwrap() + "\n",
    )
    .unwrap();
    adapter
        .copy_session_data(
            &source,
            &child,
            CopySessionOptions {
                target_prompt_index: Some(1),
                fresh_title: true,
                copy_plan_state: false,
                copy_plan_mode_state: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let data = adapter.load_session(&child).await.unwrap();
    let serialized = serde_json::to_string(&data.chat_history).unwrap();
    assert!(
        serialized.contains("UNIQUE_TOOL_EVIDENCE=42"),
        "tool evidence must reach the model"
    );
    assert!(!serialized.contains("future-secret"));
    assert!(!serialized.contains("discarded-secret"));
    assert!(!has_dangling_tool_calls(&data.chat_history));
    if !checkpoint_before_selected_turn {
        let first_call = data
            .chat_history
            .iter()
            .find_map(|item| match item {
                ConversationItem::Assistant(assistant) => assistant.tool_calls.first(),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            first_call.arguments.as_ref(),
            "{}",
            "later tool input must not rewrite an earlier turn"
        );
    }
    assert!(data.chat_history.iter().any(|item| matches!(item, ConversationItem::Assistant(assistant) if assistant.tool_calls.iter().any(|call| call.name == "read_file" && call.arguments.contains("data.txt")))));
    assert!(data.chat_history.iter().any(|item| matches!(item, ConversationItem::User(user) if user.content.iter().any(|part| matches!(part, ContentPart::Image {url} if url.starts_with("data:image/png;base64,"))))));
    assert!(data.chat_history.iter().any(|item| matches!(item, ConversationItem::ToolResult(result) if result.images.iter().any(|part| matches!(part, ContentPart::Image {url} if url.starts_with("data:image/png;base64,"))))));
    assert!(data.plan_state.is_none());
    assert!(
        matches!(data.updates.last(), Some(SessionUpdate::Acp(notification)) if matches!(&notification.update, acp::SessionUpdate::Plan(plan) if plan.entries.is_empty()))
    );
    adapter.delete_session(&source).await.unwrap();
    assert!(
        serde_json::to_string(&adapter.load_session(&child).await.unwrap().chat_history)
            .unwrap()
            .contains("UNIQUE_TOOL_EVIDENCE=42")
    );
}
