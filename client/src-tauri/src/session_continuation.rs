//! Desktop continuation: inclusive history cuts and durable, retryable provenance.
use crate::{
    commands::AppState,
    ext::{call_ext, raw_params},
    meta::SessionContinuation,
    sessions::SessionSummary,
};
use echo_agent_runtime::session::{
    fork::ForkSessionRequest,
    info::Info,
    storage::{JsonlStorageAdapter, SessionUpdate, StorageAdapter},
};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use tauri::State;

/// Snapshot creation excludes sends, rewinds and deletion for this session.
/// Ordinary sends share the read side so send-now can still replace a live turn.
fn history_lock(id: &str) -> Arc<tokio::sync::RwLock<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Weak<tokio::sync::RwLock<()>>>>> = OnceLock::new();
    let mut locks = LOCKS.get_or_init(Default::default).lock().unwrap();
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(id).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(tokio::sync::RwLock::new(()));
    locks.insert(id.into(), Arc::downgrade(&lock));
    lock
}

pub(crate) fn history_mutation_guard(
    id: &str,
) -> Result<tokio::sync::OwnedRwLockReadGuard<()>, String> {
    history_lock(id)
        .try_read_owned()
        .map_err(|_| "正在创建接续对话，请稍候再操作原对话".into())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContinuationResult {
    pub session: SessionSummary,
    pub warnings: Vec<String>,
}

fn collect_prompt_ids(value: &serde_json::Value, ids: &mut HashSet<String>) {
    match value {
        serde_json::Value::Object(object) => {
            for (key, value) in object {
                if matches!(key.as_str(), "promptId" | "prompt_id") {
                    if let Some(id) = value
                        .as_str()
                        .filter(|id| !id.is_empty() && id.len() <= 256)
                    {
                        ids.insert(id.into());
                    }
                } else if matches!(key.as_str(), "meta" | "_meta" | "update" | "params") {
                    collect_prompt_ids(value, ids);
                }
            }
        }
        _ => {}
    }
}

fn continuation_title(source: &str, rows: &[SessionSummary]) -> String {
    let base = format!("{} · 新方向", source.chars().take(100).collect::<String>());
    if !rows.iter().any(|row| row.title == base) {
        return base;
    }
    for number in 2..=10_001 {
        let title = format!("{base} {number}");
        if !rows.iter().any(|row| row.title == title) {
            return title;
        }
    }
    format!("{base} {}", chrono::Utc::now().timestamp_millis())
}

#[tauri::command]
pub async fn session_continue(
    state: State<'_, AppState>,
    session_id: String,
    cwd: String,
    target_prompt_index: Option<u32>,
    source_prompt_id: Option<String>,
    request_id: String,
) -> Result<ContinuationResult, String> {
    continue_session(
        &state,
        session_id,
        cwd,
        target_prompt_index,
        source_prompt_id,
        request_id,
    )
    .await
}

async fn continue_session(
    state: &AppState,
    session_id: String,
    cwd: String,
    target_prompt_index: Option<u32>,
    source_prompt_id: Option<String>,
    request_id: String,
) -> Result<ContinuationResult, String> {
    if !uuid::Uuid::parse_str(&request_id).is_ok_and(|id| id.to_string() == request_id)
        || request_id == session_id
        || source_prompt_id
            .as_ref()
            .is_some_and(|id| id.is_empty() || id.len() > 256 || id.chars().any(char::is_control))
    {
        return Err("接续请求标识无效".into());
    }
    let _history_guard = history_lock(&session_id)
        .try_write_owned()
        .map_err(|_| "原对话正在执行或修改历史，请稍后重试")?;
    // Scheduled prompts bypass the interactive send command but hold this fence
    // through completion and permission cleanup. Prevent their next run racing
    // an otherwise idle continuation snapshot.
    let _turn_guard = crate::automations::session_turn_lock(&session_id)
        .try_lock_owned()
        .map_err(|_| "原对话正在执行任务或清理，请稍后重试")?;
    // Serialize with permission/workspace transitions and other continuation requests.
    let _guard = crate::permission_config::permission_transition_lock()
        .lock()
        .await;
    let workspace = state.session_workspace(&session_id)?;
    if std::path::Path::new(&cwd)
        .canonicalize()
        .map_err(|e| e.to_string())?
        != workspace
    {
        return Err("接续请求的工作目录与原会话不一致".into());
    }
    let cwd = workspace.to_string_lossy().into_owned();
    let rows = crate::sessions::list_sessions(&cwd, true);
    let source = rows
        .iter()
        .find(|row| row.session_id == session_id)
        .ok_or("原会话不存在，无法接续")?;
    let previous = crate::meta::read_state()
        .session_continuations
        .get(&request_id)
        .cloned();
    let existing = rows.iter().find(|row| row.session_id == request_id);
    if let Some(existing) = existing {
        if previous.is_none() || existing.parent_session_id.as_deref() != Some(&session_id) {
            return Err("接续目标已被其他会话占用".into());
        }
    }
    if let Some(origin) = &previous {
        if origin.parent_session_id != session_id
            || target_prompt_index.is_some_and(|index| index != origin.prompt_index)
            || origin.prompt_id != source_prompt_id
        {
            return Err("接续重试请求与原请求不一致".into());
        }
    }
    let tx = state
        .tx
        .lock()
        .unwrap()
        .clone()
        .ok_or("agent not initialized")?;
    let points = if existing.is_none() {
        let value: serde_json::Value = call_ext(
            &tx,
            "echo.agent/rewind/points",
            raw_params(&serde_json::json!({"sessionId":session_id})),
        )
        .await
        .map_err(|e| e.to_string())?;
        crate::agent_admin::parse_rewind_points(value)?
    } else {
        Vec::new()
    };
    let origin = if let Some(origin) = previous {
        origin
    } else {
        let index = target_prompt_index
            .or_else(|| points.iter().map(|p| p.prompt_index).max())
            .ok_or("当前会话还没有可接续的完整轮次")?;
        SessionContinuation {
            parent_session_id: session_id.clone(),
            parent_title: source.title.clone(),
            prompt_index: index,
            prompt_id: source_prompt_id,
            inherited_turn_count: points.iter().filter(|p| p.prompt_index <= index).count() as u32,
            temporary_title: continuation_title(&source.title, &rows),
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    };
    if existing.is_none() {
        if !points.iter().any(|p| p.prompt_index == origin.prompt_index) {
            return Err("该历史轮次已变化，请刷新会话后重试".into());
        }
    }
    // Reserve provenance first. A retry repairs the same child even if the copy
    // completed but the response/sidecar copy was interrupted.
    crate::meta::save_continuation(&request_id, origin.clone(), &session_id, &workspace)?;
    if existing.is_none() {
        let request = ForkSessionRequest {
            source_session_id: session_id.clone(),
            source_cwd: cwd.clone(),
            new_cwd: cwd.clone(),
            new_session_id: Some(request_id.clone()),
            target_prompt_index: Some(origin.prompt_index as usize),
            fresh_continuation: true,
            source_prompt_id: origin.prompt_id.clone(),
            ..Default::default()
        };
        let value: serde_json::Value =
            call_ext(&tx, "echo.agent/session/fork", raw_params(&request))
                .await
                .map_err(|e| e.to_string())?;
        let response: echo_agent_runtime::session::fork::ForkSessionResponse =
            serde_json::from_value(value).map_err(|e| e.to_string())?;
        if response.new_session_id != request_id
            || response.parent_session_id != session_id
            || response.new_cwd != cwd
        {
            return Err("接续响应的会话或工作目录与请求不一致，请刷新会话列表".into());
        }
    }
    let mut warnings = Vec::new();
    let info = Info {
        id: agent_client_protocol::SessionId::new(request_id.clone()),
        cwd: cwd.clone(),
    };
    let storage = JsonlStorageAdapter::default();
    match storage.load_session(&info).await {
        Ok(data) => {
            let mut ids = HashSet::new();
            for update in &data.updates {
                let value = match update {
                    SessionUpdate::Acp(value) => serde_json::to_value(value),
                    SessionUpdate::EchoAgent(value) => serde_json::to_value(value),
                };
                if let Ok(value) = value {
                    collect_prompt_ids(&value, &mut ids);
                }
            }
            if let Err(error) =
                crate::knowledge_traces::clone_session_filtered(&session_id, &request_id, &ids)
            {
                warnings.push(format!("接续对话已创建，部分历史知识来源未复制：{error}"));
            }
        }
        Err(error) => warnings.push(format!("接续对话已创建，历史信息暂未载入：{error}")),
    }
    let session = crate::sessions::list_sessions(&cwd, true)
        .into_iter()
        .find(|row| row.session_id == request_id)
        .ok_or("接续已创建，请刷新会话列表重新打开")?;
    Ok(ContinuationResult { session, warnings })
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_runtime::session::fork::source_prompt_matches;

    fn user_update(index: Option<usize>, id: &str) -> SessionUpdate {
        use agent_client_protocol as acp;
        SessionUpdate::Acp(Box::new(
            acp::SessionNotification::new(
                acp::SessionId::new("source"),
                acp::SessionUpdate::UserMessageChunk(
                    acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(
                        "question",
                    )))
                    .meta(index.map(|index| {
                        serde_json::json!({"promptIndex":index})
                            .as_object()
                            .unwrap()
                            .clone()
                    })),
                ),
            )
            .meta(serde_json::json!({"promptId":id}).as_object().cloned()),
        ))
    }

    fn answer_update(id: &str) -> SessionUpdate {
        use agent_client_protocol as acp;
        SessionUpdate::Acp(Box::new(
            acp::SessionNotification::new(
                acp::SessionId::new("source"),
                acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(
                    acp::ContentBlock::Text(acp::TextContent::new("answer")),
                )),
            )
            .meta(serde_json::json!({"promptId":id}).as_object().cloned()),
        ))
    }

    #[test]
    fn source_identity_rejects_replaced_turns_even_when_the_index_is_reused() {
        let updates = vec![
            user_update(Some(0), "p0"),
            answer_update("p0"),
            user_update(Some(1), "old"),
            answer_update("old"),
            SessionUpdate::EchoAgent(Box::new(
                echo_agent_runtime::extensions::notification::SessionNotification {
                    session_id: agent_client_protocol::SessionId::new("source"),
                    update:
                        echo_agent_runtime::extensions::notification::SessionUpdate::RewindMarker {
                            target_prompt_index: 1,
                            created_at: String::new(),
                        },
                    meta: None,
                },
            )),
            user_update(Some(1), "new"),
            answer_update("new"),
        ];
        let matches = |target, id| {
            source_prompt_matches(updates.clone().into_iter().map(Ok), target, id).unwrap()
        };
        assert!(!matches(1, "old"));
        assert!(matches(1, "new"));
        assert!(matches(0, "p0"));
        assert!(!matches(0, "new"));
    }

    #[test]
    fn source_identity_counts_chunked_and_legacy_turns_without_counting_host_inserts() {
        let mut host = user_update(None, "host");
        if let SessionUpdate::Acp(notification) = &mut host {
            if let agent_client_protocol::SessionUpdate::UserMessageChunk(chunk) =
                &mut notification.update
            {
                chunk.meta = serde_json::json!({"hostTurn":true}).as_object().cloned();
            }
        }
        let updates = vec![
            user_update(None, "legacy"),
            answer_update("legacy"),
            user_update(Some(1), "p1"),
            user_update(Some(1), "p1"),
            answer_update("p1"),
            host,
            answer_update("p1"),
            user_update(Some(2), "p2"),
            answer_update("p2"),
        ];
        for (target, id) in [(0, "legacy"), (1, "p1"), (2, "p2")] {
            assert!(
                source_prompt_matches(updates.clone().into_iter().map(Ok), target, id).unwrap()
            );
        }
        assert!(!source_prompt_matches(updates.into_iter().map(Ok), 1, "p2").unwrap());
    }

    #[test]
    fn snapshot_excludes_all_history_mutations_without_blocking_other_sessions() {
        let mutation = history_mutation_guard("lock-source").unwrap();
        assert!(history_lock("lock-source").try_write_owned().is_err());
        drop(mutation);
        let snapshot = history_lock("lock-source").try_write_owned().unwrap();
        assert!(history_mutation_guard("lock-source").is_err());
        assert!(history_mutation_guard("lock-other").is_ok());
        drop(snapshot);
        assert!(history_mutation_guard("lock-source").is_ok());
    }

    #[test]
    fn continuation_keeps_the_latest_target_beyond_the_rewind_picker_limit() {
        let points = (0..2001)
            .map(|index| serde_json::json!({"prompt_index":index}))
            .collect::<Vec<_>>();
        let parsed =
            crate::agent_admin::parse_rewind_points(serde_json::json!({"rewind_points":points}))
                .unwrap();
        assert_eq!(parsed.last().unwrap().prompt_index, 2000);
    }

    #[test]
    fn command_contract_and_lost_response_recovery() {
        let home = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "session_continuation::tests::isolated_command_contract",
                "--exact",
                "--nocapture",
            ])
            .env("ECHO_CONTINUATION_COMMAND_TEST", "1")
            .env("ECHO_AGENT_HOME", home.path())
            .env("GROK_HOME", home.path().join("legacy-empty"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    async fn isolated_command_contract() {
        if std::env::var("ECHO_CONTINUATION_COMMAND_TEST").as_deref() != Ok("1") {
            return;
        }
        use agent_client_protocol as acp;
        use echo_agent_runtime::sampling::ConversationItem;
        use echo_agent_runtime::session::{RewindPointInfo, RewindPointsResponse};
        let workspace = crate::paths::echo_agent_home_dir().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let cwd = workspace.to_string_lossy().into_owned();
        let source = Info {
            id: acp::SessionId::new("original"),
            cwd: cwd.clone(),
        };
        let storage = JsonlStorageAdapter::default();
        storage
            .init_session(&source, acp::ModelId::new("test-model"))
            .await
            .unwrap();
        storage
            .update_session_title(&source, "原对话".into())
            .await
            .unwrap();
        for index in 0..2 {
            let mut user = ConversationItem::user(format!("prompt-{index}"));
            user.set_prompt_index(index);
            storage.append_chat_message(&source, &user).await.unwrap();
            storage
                .append_chat_message(
                    &source,
                    &ConversationItem::assistant(format!("answer-{index}")),
                )
                .await
                .unwrap();
            for update in [
                acp::SessionUpdate::UserMessageChunk(
                    acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(
                        format!("prompt-{index}"),
                    )))
                    .meta(
                        serde_json::json!({"promptIndex":index})
                            .as_object()
                            .cloned(),
                    ),
                ),
                acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(
                    acp::ContentBlock::Text(acp::TextContent::new(format!("answer-{index}"))),
                )),
            ] {
                storage
                    .append_update(
                        &source,
                        &SessionUpdate::Acp(Box::new(
                            acp::SessionNotification::new(source.id.clone(), update).meta(
                                serde_json::json!({"promptId":format!("p{index}")})
                                    .as_object()
                                    .cloned(),
                            ),
                        )),
                    )
                    .await
                    .unwrap();
            }
        }
        let (client, mut agent) = echo_agent_acp::acp_channels();
        let state = AppState::default();
        *state.tx.lock().unwrap() = Some(client.tx.clone());
        state
            .record_session_workspace("original", &workspace)
            .unwrap();
        let forks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = forks.clone();
        let worker = tokio::spawn(async move {
            while let Some(message) = agent.rx.recv().await {
                let echo_agent_acp::AcpAgentMessage::ExtMethod(arguments) = message else {
                    panic!("unexpected ACP request")
                };
                match arguments.request.method.as_ref() {
                    "echo.agent/rewind/points" => {
                        let response = RewindPointsResponse {
                            rewind_points: (0..2)
                                .map(|prompt_index| RewindPointInfo {
                                    prompt_index,
                                    created_at: String::new(),
                                    num_file_snapshots: 0,
                                    has_file_changes: false,
                                    prompt_preview: None,
                                })
                                .collect(),
                        };
                        arguments
                            .response_tx
                            .send(Ok(acp::ExtResponse::new(raw_params(&response))))
                            .unwrap();
                    }
                    "echo.agent/session/fork" => {
                        let request: ForkSessionRequest =
                            serde_json::from_str(arguments.request.params.get()).unwrap();
                        assert!(request.fresh_continuation);
                        assert_eq!(request.target_prompt_index, Some(0));
                        if let Err(error) =
                            echo_agent_runtime::session::fork::fork_session(request, "test", None)
                                .await
                        {
                            arguments
                                .response_tx
                                .send(Err(acp::Error::invalid_request().data(error.to_string())))
                                .unwrap();
                            continue;
                        }
                        observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        // Disk copy succeeded; the caller never received its response.
                        arguments
                            .response_tx
                            .send(Err(acp::Error::internal_error().data("lost response")))
                            .unwrap();
                    }
                    other => panic!("unexpected method {other}"),
                }
            }
        });
        let request = uuid::Uuid::now_v7().to_string();
        let scheduled = crate::automations::session_turn_lock("original")
            .lock_owned()
            .await;
        let busy = continue_session(
            &state,
            "original".into(),
            cwd.clone(),
            Some(0),
            Some("p0".into()),
            request.clone(),
        )
        .await;
        assert!(busy.err().unwrap().contains("正在执行任务或清理"));
        drop(scheduled);
        let stale = uuid::Uuid::now_v7().to_string();
        let stale_result = continue_session(
            &state,
            "original".into(),
            cwd.clone(),
            Some(0),
            Some("p1".into()),
            stale.clone(),
        )
        .await;
        assert!(stale_result
            .err()
            .unwrap()
            .contains("历史轮次已被回溯或替换"));
        assert!(!crate::sessions::list_sessions(&cwd, true)
            .iter()
            .any(|row| row.session_id == stale));
        assert_eq!(forks.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(continue_session(
            &state,
            "original".into(),
            cwd.clone(),
            Some(0),
            Some("p0".into()),
            request.clone()
        )
        .await
        .is_err());
        let result = continue_session(
            &state,
            "original".into(),
            cwd.clone(),
            Some(0),
            Some("p0".into()),
            request.clone(),
        )
        .await
        .unwrap();
        assert_eq!(forks.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(result.session.session_id, request);
        assert_eq!(result.session.continuation.unwrap().inherited_turn_count, 1);
        let child = Info {
            id: acp::SessionId::new(request),
            cwd,
        };
        let data = storage.load_session(&child).await.unwrap();
        let history = serde_json::to_string(&data.chat_history).unwrap();
        assert!(history.contains("answer-0") && !history.contains("answer-1"));
        assert_eq!(
            storage
                .load_session(&source)
                .await
                .unwrap()
                .chat_history
                .len(),
            4
        );
        storage.delete_session(&source).await.unwrap();
        assert_eq!(
            storage
                .load_session(&child)
                .await
                .unwrap()
                .chat_history
                .len(),
            2
        );
        worker.abort();
    }
    #[test]
    fn prompt_provenance_ignores_tool_payload_and_user_text() {
        let value = serde_json::json!({"update":{"_meta":{"promptId":"kept"},"content":{"promptId":"tool-value"},"text":"later"}});
        let mut ids = HashSet::new();
        collect_prompt_ids(&value, &mut ids);
        assert_eq!(ids, HashSet::from(["kept".into()]));
    }
}
