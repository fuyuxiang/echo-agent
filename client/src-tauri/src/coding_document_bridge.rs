//! Bounded reverse requests for the editor-owned document transaction boundary.
use serde_json::{json, Value};
use std::{collections::HashMap, sync::OnceLock};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{oneshot, Mutex};

struct Pending {
    tx: oneshot::Sender<Value>,
    session_id: String,
    run_id: String,
    workspace: String,
    tool_kind: String,
    phase: String,
    targets: Vec<String>,
    app: AppHandle,
}
static PENDING: OnceLock<Mutex<HashMap<String, Pending>>> = OnceLock::new();
fn pending() -> &'static Mutex<HashMap<String, Pending>> {
    PENDING.get_or_init(Default::default)
}

pub async fn dispatch(app: AppHandle, request: Value) -> Value {
    let Some(id) = request
        .get("operationId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 256)
        .map(str::to_string)
    else {
        return json!({"handled":true,"error":"Invalid document operation identity"});
    };
    if request.to_string().len() > 3 * 1024 * 1024 {
        return json!({"handled":true,"error":"Document tool request exceeds budget"});
    }
    let Some(session_id) = request.get("sessionId").and_then(Value::as_str) else {
        return json!({"handled":true,"error":"Missing session identity"});
    };
    let Some(run_id) = request.get("runId").and_then(Value::as_str) else {
        return json!({"handled":true,"error":"Missing run identity"});
    };
    let Some(workspace) = request.get("workspace").and_then(Value::as_str) else {
        return json!({"handled":true,"error":"Missing workspace identity"});
    };
    if request.get("phase").and_then(Value::as_str) == Some("after")
        && request.get("externalTool").and_then(Value::as_bool) == Some(true)
    {
        return settle_external_tool(&app, &request, &id, session_id, run_id, workspace).await;
    }
    {
        let authority = runs().lock().await;
        if !authority
            .get(session_id)
            .is_some_and(|run| !run.cancelled && run.run_id == run_id && run.workspace == workspace)
        {
            return json!({"handled":true,"error":"Document run was cancelled, replaced or not registered"});
        }
    }
    if request.get("phase").and_then(Value::as_str) == Some("before")
        && !matches!(
            request.get("toolKind").and_then(Value::as_str),
            Some("read" | "search" | "list_dir")
        )
    {
        if let Err(error) = crate::coding_external_tools::ensure_admission(workspace).await {
            return json!({"handled":true,"error":error});
        }
    }
    let authorized = app
        .state::<crate::commands::AppState>()
        .session_workspace(session_id)
        .and_then(|root| {
            let candidate = app
                .state::<crate::shell_fs::FilesystemAccess>()
                .require_workspace(workspace)?;
            if root.canonicalize().unwrap_or(root) != candidate {
                return Err("Document workspace does not belong to session".into());
            }
            Ok(candidate)
        });
    let root = match authorized {
        Ok(root) => root,
        Err(error) => return json!({"handled":true,"error":error}),
    };
    let task = crate::coding::task::list_tasks(&root)
        .into_iter()
        .filter_map(|summary| crate::coding::task::load(&root, &summary.id))
        .find(|task| task.session_id.as_deref() == Some(session_id));
    let Some(task) = task else {
        return json!({"handled":true,"error":"Document session has no coding task owner"});
    };
    let run = run_id.to_string();
    let checkpoint_root = root.clone();
    let checkpoint_task = task.id.clone();
    let checkpoint_run = run.clone();
    let checkpoint = tokio::task::spawn_blocking(move || {
        crate::coding::changeset::checkpoint_start(
            &checkpoint_root,
            &checkpoint_task,
            &checkpoint_run,
        )
    })
    .await;
    match checkpoint {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => return json!({"handled":true,"error":error}),
        Err(error) => return json!({"handled":true,"error":error.to_string()}),
    }
    let (tx, rx) = oneshot::channel();
    {
        let mut registry = pending().lock().await;
        if registry.len() >= 32 || registry.contains_key(&id) {
            return json!({"handled":true,"error":"Document transaction is already pending or coordinator is busy"});
        }
        registry.insert(
            id.clone(),
            Pending {
                tx,
                session_id: request
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                run_id: request
                    .get("runId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                workspace: request
                    .get("workspace")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                tool_kind: request
                    .get("toolKind")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                phase: request
                    .get("phase")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                targets: if let Some(patch) =
                    request.pointer("/input/patch").and_then(Value::as_str)
                {
                    crate::coding_patch::paths(patch).unwrap_or_default()
                } else {
                    request
                        .pointer("/input/file_path")
                        .or_else(|| request.pointer("/input/target_file"))
                        .or_else(|| request.pointer("/input/filePath"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .into_iter()
                        .collect()
                },
                app: app.clone(),
            },
        );
    }
    let result = if app.emit("agent://document-tool", &request).is_err() {
        json!({"handled":true,"error":"Editor coordinator is unavailable"})
    } else {
        match tokio::time::timeout(std::time::Duration::from_secs(25), rx).await {
            Ok(Ok(value)) => value,
            _ => {
                // The editor may still be awaiting FileService.save. Retire
                // admission now and keep a real cancellation fence until ACK;
                // a later normal CompleteEvent cannot make a new run safe.
                fence_session_run(session_id, Some(run_id)).await;
                let cancel_app = app.clone();
                let cancel_session_id = session_id.to_string();
                let cancel_run_id = run_id.to_string();
                tauri::async_runtime::spawn(async move {
                    let runtime_tx = {
                        cancel_app
                            .state::<crate::commands::AppState>()
                            .tx
                            .lock()
                            .unwrap()
                            .clone()
                    };
                    let stop_runtime = async {
                        if let Some(tx) = runtime_tx {
                            let _ = crate::agent_runtime::cancel(
                                &tx,
                                &cancel_session_id,
                                "stop",
                                Some(&cancel_run_id),
                            )
                            .await;
                        }
                    };
                    let _ = tokio::join!(
                        cancel_session_run(&cancel_session_id, Some(&cancel_run_id)),
                        stop_runtime
                    );
                });
                json!({"handled":true,"error":"Editor coordinator timed out; workspace remains fenced until the editor confirms cancellation. Operation was not retried"})
            }
        }
    };
    pending().lock().await.remove(&id);
    if request.get("externalTool").and_then(Value::as_bool) == Some(true)
        && request.get("phase").and_then(Value::as_str) == Some("before")
        && result.get("handled").and_then(Value::as_bool) == Some(false)
        && result.get("error").is_none()
    {
        // Arm while holding the native grant: cancellation cannot pass between
        // approval and the durable external-execution lease.
        let authority = runs().lock().await;
        if !authority
            .get(session_id)
            .is_some_and(|run| !run.cancelled && run.run_id == run_id && run.workspace == workspace)
        {
            return json!({"handled":true,"error":"Run cancelled before MCP execution admission"});
        }
        if let Err(error) = crate::coding_external_tools::arm(
            workspace,
            session_id,
            run_id,
            &id,
            request
                .get("toolName")
                .and_then(Value::as_str)
                .unwrap_or("use_tool"),
        )
        .await
        {
            drop(authority);
            fence_session_run(session_id, Some(run_id)).await;
            request_native_cancel(app.clone(), session_id.to_string(), run_id.to_string());
            return json!({"handled":true,"error":error});
        }
    }
    if result.get("appliedDocuments").is_some() {
        let documents = result.get("appliedDocuments").and_then(Value::as_array);
        let receipts: Vec<(&str, Option<&str>, Option<&str>)> = if let Some(documents) = documents {
            documents
                .iter()
                .filter_map(|document| {
                    Some((
                        document.get("path")?.as_str()?,
                        Some(document.get("beforeHash")?.as_str()?),
                        Some(document.get("afterHash")?.as_str()?),
                    ))
                })
                .collect()
        } else {
            Vec::new()
        };
        for (path, before_hash, after_hash) in receipts {
            if let Err(error) = crate::coding::changeset::checkpoint_note_operation_applied(
                &root,
                &task.id,
                &run,
                path,
                &id,
                before_hash,
                after_hash,
            ) {
                return json!({"handled":true,"error":format!("文件已修改，但恢复点记录失败：{error}")});
            }
        }
    }
    if request.get("phase").and_then(Value::as_str) == Some("after")
        && !matches!(
            request.get("toolKind").and_then(Value::as_str),
            Some("read" | "search" | "list_dir")
        )
    {
        match crate::coding::changeset::sync_changes(&root, &task.id).await {
            Ok(set) => {
                let _=app.emit("coding://changes-observed",json!({"workspace":workspace,"taskId":task.id,"sessionId":session_id,"runId":run_id,"commandRunId":id,"contentRevision":set.content_revision()}));
            }
            Err(error) => {
                return json!({"handled":true,"error":format!("工具已经结束，既有副作用保留；磁盘变化核对失败，请先检查再决定是否重试：{error}")})
            }
        }
    }
    result
}

fn request_native_cancel(app: AppHandle, session_id: String, run_id: String) {
    tauri::async_runtime::spawn(async move {
        let tx = {
            app.state::<crate::commands::AppState>()
                .tx
                .lock()
                .unwrap()
                .clone()
        };
        let runtime = async {
            if let Some(tx) = tx {
                let _ = crate::agent_runtime::cancel(&tx, &session_id, "stop", Some(&run_id)).await;
            }
        };
        let _ = tokio::join!(cancel_session_run(&session_id, Some(&run_id)), runtime);
    });
}

async fn settle_external_tool(
    app: &AppHandle,
    request: &Value,
    id: &str,
    session_id: &str,
    run_id: &str,
    workspace: &str,
) -> Value {
    let confirmed = request.get("executionSettled").and_then(Value::as_bool) == Some(true);
    match crate::coding_external_tools::validate(workspace, session_id, run_id, id).await {
        Ok(true) => {
            return if confirmed {
                json!({"handled":true,"text":"Exact MCP terminal was already settled; execution was not replayed"})
            } else {
                json!({"handled":true,"error":"A late uncertain result cannot undo an existing protocol terminal"})
            }
        }
        Ok(false) => {}
        Err(error) => return json!({"handled":true,"error":error}),
    }
    if !confirmed {
        if let Err(error) =
            crate::coding_external_tools::settle(workspace, session_id, run_id, id, false).await
        {
            return json!({"handled":true,"error":error});
        }
        fence_session_run(session_id, Some(run_id)).await;
        emit_cancellation_state(app, workspace, session_id, run_id, true);
        request_native_cancel(app.clone(), session_id.to_string(), run_id.to_string());
        return json!({"handled":true,"error":"MCP transport/timeout did not prove remote completion; workspace remains fenced and the call will not be replayed"});
    }
    // This exact existing lease is the sole authority for a late, cancelled
    // run. Do not re-enter editor dispatch or admit another tool. Keep the
    // lease until disk observation and the old checkpoint amendment finish.
    let root = std::path::PathBuf::from(workspace);
    if let Some(task) = crate::coding::task::list_tasks(&root)
        .into_iter()
        .filter_map(|summary| crate::coding::task::load(&root, &summary.id))
        .find(|task| task.session_id.as_deref() == Some(session_id))
    {
        match crate::coding::changeset::sync_changes(&root, &task.id).await {
            Ok(set) => {
                if let Err(error) = crate::coding::changeset::checkpoint_observe_external_terminal(
                    &root, &task.id, run_id,
                ) {
                    return retain_failed_observation(
                        app,
                        workspace,
                        session_id,
                        run_id,
                        id,
                        format!("MCP已结束但旧运行外部变化核对失败，保护状态保留：{error}"),
                    )
                    .await;
                }
                let _=app.emit("coding://changes-observed",json!({"workspace":workspace,"taskId":task.id,"sessionId":session_id,"runId":run_id,"commandRunId":id,"contentRevision":set.content_revision()}));
            }
            Err(error) => {
                return retain_failed_observation(
                    app,
                    workspace,
                    session_id,
                    run_id,
                    id,
                    format!("MCP已返回真实结束回执，副作用保留；磁盘变化核对失败：{error}"),
                )
                .await;
            }
        }
    }
    if let Err(error) =
        crate::coding_external_tools::settle(workspace, session_id, run_id, id, true).await
    {
        return json!({"handled":true,"error":error});
    }
    let released = {
        let mut authority = runs().lock().await;
        let external_pending = crate::coding_external_tools::has_run(session_id, run_id)
            .await
            .unwrap_or(true);
        authority.get_mut(session_id).is_some_and(|run| {
            if run.run_id == run_id
                && run.workspace == workspace
                && run.cancellation_pending
                && run.editor_acknowledged
                && !external_pending
            {
                run.cancellation_pending = false;
                emit_cancellation_state(app, workspace, session_id, run_id, false);
                true
            } else {
                false
            }
        })
    };
    let _ = released;
    json!({"handled":true,"text":"MCP protocol terminal observed; existing disk effects inspected without attributing them to editor operations"})
}

pub(crate) async fn external_recovered(
    app: &AppHandle,
    workspace: &str,
    session: &str,
    run_id: &str,
) {
    let external_pending = crate::coding_external_tools::has_run(session, run_id)
        .await
        .unwrap_or(true);
    let mut authority = runs().lock().await;
    let pending = if let Some(run) = authority
        .get_mut(session)
        .filter(|run| run.run_id == run_id && run.workspace == workspace)
    {
        if !external_pending && run.editor_acknowledged {
            run.cancellation_pending = false;
        }
        run.cancellation_pending || external_pending
    } else {
        external_pending
    };
    emit_cancellation_state(app, workspace, session, run_id, pending);
}

async fn retain_failed_observation(
    app: &AppHandle,
    workspace: &str,
    session: &str,
    run: &str,
    operation: &str,
    error: String,
) -> Value {
    // The RPC really returned; keep the workspace protected, but make the
    // failed local observation recoverable through explicit user inspection.
    let _ = crate::coding_external_tools::settle(workspace, session, run, operation, false).await;
    fence_session_run(session, Some(run)).await;
    emit_cancellation_state(app, workspace, session, run, true);
    request_native_cancel(app.clone(), session.into(), run.into());
    json!({"handled":true,"error":error})
}

#[tauri::command]
pub async fn coding_document_tool_respond(
    operation_id: String,
    result: Value,
) -> Result<(), String> {
    if result.to_string().len() > 3 * 1024 * 1024 {
        return Err("Document response exceeds budget".into());
    }
    let entry = pending()
        .lock()
        .await
        .remove(&operation_id)
        .ok_or("Document request is no longer active")?;
    entry
        .tx
        .send(result)
        .map_err(|_| "Document request was cancelled".into())
}

struct CancelAck {
    notify: oneshot::Sender<()>,
    session_id: String,
    run_id: String,
    workspace: String,
    app: AppHandle,
}
static CANCEL_ACKS: OnceLock<Mutex<HashMap<String, CancelAck>>> = OnceLock::new();
fn cancel_acks() -> &'static Mutex<HashMap<String, CancelAck>> {
    CANCEL_ACKS.get_or_init(Default::default)
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CancellationState {
    workspace: String,
    session_id: String,
    run_id: String,
    pending: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    cancellation_id: Option<String>,
}

fn emit_cancellation_state(
    app: &AppHandle,
    workspace: &str,
    session_id: &str,
    run_id: &str,
    pending: bool,
) {
    let _ = app.emit(
        "agent://document-cancellation-state",
        CancellationState {
            workspace: workspace.into(),
            session_id: session_id.into(),
            run_id: run_id.into(),
            pending,
            cancellation_id: None,
        },
    );
}

#[tauri::command]
pub async fn coding_document_cancellation_state(
    workspace: Option<String>,
) -> Result<Vec<CancellationState>, String> {
    let mut states: Vec<CancellationState> = runs()
        .lock()
        .await
        .iter()
        .filter(|(_, run)| {
            run.cancellation_pending
                && workspace
                    .as_deref()
                    .is_none_or(|workspace| workspace == run.workspace)
        })
        .map(|(session_id, run)| CancellationState {
            workspace: run.workspace.clone(),
            session_id: session_id.clone(),
            run_id: run.run_id.clone(),
            pending: true,
            cancellation_id: Some(format!("cancel:{session_id}:{}", run.run_id)),
        })
        .collect();
    for lease in crate::coding_external_tools::pending(workspace.as_deref()).await? {
        if !states.iter().any(|state| {
            state.workspace == lease.workspace
                && state.session_id == lease.session_id
                && state.run_id == lease.run_id
        }) {
            states.push(CancellationState {
                workspace: lease.workspace,
                session_id: lease.session_id,
                run_id: lease.run_id,
                pending: true,
                cancellation_id: None,
            });
        }
    }
    Ok(states)
}

pub(crate) async fn ensure_workspace_admission(workspace: &str) -> Result<(), String> {
    crate::coding_external_tools::ensure_admission(workspace).await?;
    if runs()
        .lock()
        .await
        .values()
        .any(|run| run.workspace == workspace && run.cancellation_pending)
    {
        return Err("工程仍在停止或外部工具状态未知：Runtime 已取消，但编辑器或外部工具尚未确认结束。不会自动重放调用".into());
    }
    Ok(())
}

async fn await_editor_fence(
    ack: oneshot::Receiver<()>,
    timeout: std::time::Duration,
) -> Result<(), String> {
    match tokio::time::timeout(timeout, ack).await {
        Ok(Ok(())) => Ok(()),
        _ => Err("编辑器尚未确认文档写入停止；已阻止后续运行，请重新连接编辑器后重试停止".into()),
    }
}

async fn mark_session_cancellation(
    session_id: &str,
    expected_run_id: Option<&str>,
) -> Option<RunAuthority> {
    let registered = {
        let mut authority = runs().lock().await;
        authority.get_mut(session_id).and_then(|run| {
            if run.grant.begin_cancellation_if_matching(expected_run_id) {
                emit_cancellation_state(&run.app, &run.workspace, session_id, &run.run_id, true);
                Some(run.clone())
            } else {
                None
            }
        })
    };
    if let Err(error) = crate::coding_external_tools::cancel(session_id, expected_run_id).await {
        tracing::warn!(%error,"external tool cancellation remains unconfirmed");
    }
    registered
}

pub(crate) async fn fence_session_run(session_id: &str, expected_run_id: Option<&str>) {
    let _ = mark_session_cancellation(session_id, expected_run_id).await;
}

pub async fn cancel_session(session_id: &str) -> Result<(), String> {
    cancel_session_run(session_id, None).await
}

pub(crate) async fn cancel_session_run(
    session_id: &str,
    expected_run_id: Option<&str>,
) -> Result<(), String> {
    let registered = mark_session_cancellation(session_id, expected_run_id).await;
    let entries = {
        let mut registry = pending().lock().await;
        let ids: Vec<String> = registry
            .iter()
            .filter(|(_, entry)| {
                entry.session_id == session_id
                    && expected_run_id.is_none_or(|run_id| entry.run_id == run_id)
            })
            .map(|(id, _)| id.clone())
            .collect();
        ids.into_iter()
            .filter_map(|id| registry.remove(&id))
            .collect::<Vec<_>>()
    };
    // Keep run ownership after a tool timeout removes PENDING: its editor may
    // still be processing a versioned write and must acknowledge the fence.
    let mut owners: HashMap<(String, String), AppHandle> = HashMap::new();
    if let Some(run) = registered {
        owners.insert((run.grant.run_id, run.grant.workspace), run.app);
    }
    for entry in &entries {
        owners.insert(
            (entry.run_id.clone(), entry.workspace.clone()),
            entry.app.clone(),
        );
    }
    let mut failure = None;
    for ((run_id, workspace), app) in owners {
        // Stable for this exact run: a retry must not make a legitimate late
        // editor ACK unrecognizable, and it cannot clear a later run's fence.
        let cancellation_id = format!("cancel:{session_id}:{run_id}");
        let (tx, rx) = oneshot::channel();
        cancel_acks().lock().await.insert(
            cancellation_id.clone(),
            CancelAck {
                notify: tx,
                session_id: session_id.into(),
                run_id: run_id.clone(),
                workspace: workspace.clone(),
                app: app.clone(),
            },
        );
        let emitted = app.emit("agent://document-run-cancelled", json!({"cancellationId":cancellation_id,"workspace":workspace,"sessionId":session_id,"runId":run_id}));
        // Wait for native document writes already in flight before acknowledging stop.
        // Unmounted renderers have no live document operation and will time out fail-closed.
        if emitted.is_err()
            || await_editor_fence(rx, std::time::Duration::from_secs(25))
                .await
                .is_err()
        {
            failure = Some(
                "编辑器尚未确认文档写入停止；已阻止后续运行，请重新连接编辑器后重试停止"
                    .to_string(),
            );
        }
        // On timeout retain the ACK identity and native workspace fence. Only
        // a real editor acknowledgement may release them, including a late ACK.
    }
    for entry in entries {
        let _ = entry.tx.send(
            json!({"handled":true,"error":"Run was cancelled before editor transaction completed"}),
        );
    }
    if crate::coding_external_tools::pending(None)
        .await?
        .iter()
        .any(|lease| {
            lease.session_id == session_id && expected_run_id.is_none_or(|run| lease.run_id == run)
        })
    {
        failure = Some(
            "MCP 外部工具尚未返回真实结束回执；工程保持停止未确认，禁止后续写入且不会重放调用"
                .into(),
        );
    }
    failure.map_or(Ok(()), Err)
}

#[tauri::command]
pub async fn coding_document_cancel_ack(cancellation_id: String) -> Result<(), String> {
    let ack = cancel_acks()
        .lock()
        .await
        .remove(&cancellation_id)
        .ok_or("Cancellation is no longer pending")?;
    let (matched, external_pending) = {
        let mut authority = runs().lock().await;
        let external_pending = crate::coding_external_tools::has_run(&ack.session_id, &ack.run_id)
            .await
            .unwrap_or(true);
        let matched = authority.get_mut(&ack.session_id).is_some_and(|run| {
            if run
                .grant
                .acknowledge_if_matching(&ack.workspace, &ack.run_id, external_pending)
            {
                emit_cancellation_state(
                    &ack.app,
                    &ack.workspace,
                    &ack.session_id,
                    &ack.run_id,
                    external_pending,
                );
                true
            } else {
                false
            }
        });
        (matched, external_pending)
    };
    let _ = (matched, external_pending);
    // A timed-out caller has dropped its receiver; the actual ACK is still
    // authoritative and must release the retained fence instead of failing.
    let _ = ack.notify.send(());
    Ok(())
}

#[tauri::command]
pub async fn coding_document_tool_is_active(operation_id: String) -> bool {
    pending().lock().await.contains_key(&operation_id)
}

pub(crate) async fn validate_pending(
    operation_id: &str,
    workspace: &str,
    session_id: &str,
    run_id: &str,
) -> Result<(), String> {
    let registry = pending().lock().await;
    let entry = registry
        .get(operation_id)
        .ok_or("Document operation is no longer active")?;
    if entry.workspace != workspace || entry.session_id != session_id || entry.run_id != run_id {
        return Err("Document operation identity does not match active tool".into());
    }
    Ok(())
}

#[derive(Clone)]
struct RunGrant {
    run_id: String,
    workspace: String,
    cancelled: bool,
    cancellation_pending: bool,
    editor_acknowledged: bool,
}
impl RunGrant {
    fn begin_cancellation_if_matching(&mut self, expected_run_id: Option<&str>) -> bool {
        if expected_run_id.is_some_and(|expected| expected != self.run_id) {
            return false;
        }
        self.cancelled = true;
        if !self.cancellation_pending {
            self.editor_acknowledged = false;
        }
        self.cancellation_pending = true;
        true
    }
    fn acknowledge_if_matching(
        &mut self,
        workspace: &str,
        run_id: &str,
        external_pending: bool,
    ) -> bool {
        if self.workspace != workspace || self.run_id != run_id {
            return false;
        }
        self.editor_acknowledged = true;
        self.cancellation_pending = external_pending;
        true
    }
    fn complete_if_matching(&mut self, run_id: &str) -> bool {
        if self.run_id != run_id {
            return false;
        }
        self.cancelled = true;
        true
    }
}
#[derive(Clone)]
struct RunAuthority {
    grant: RunGrant,
    app: AppHandle,
    used_run_ids: std::collections::HashSet<String>,
}
impl std::ops::Deref for RunAuthority {
    type Target = RunGrant;
    fn deref(&self) -> &RunGrant {
        &self.grant
    }
}
impl std::ops::DerefMut for RunAuthority {
    fn deref_mut(&mut self) -> &mut RunGrant {
        &mut self.grant
    }
}
static RUNS: OnceLock<Mutex<HashMap<String, RunAuthority>>> = OnceLock::new();
fn runs() -> &'static Mutex<HashMap<String, RunAuthority>> {
    RUNS.get_or_init(Default::default)
}
pub(crate) async fn register_run(
    app: AppHandle,
    session_id: &str,
    run_id: &str,
    workspace: &str,
    mode: Option<&str>,
) -> Result<(), String> {
    crate::coding_external_tools::ensure_admission(workspace).await?;
    crate::coding_external_tools::ensure_new_run_identity(session_id, run_id).await?;
    let mut authority = runs().lock().await;
    if authority
        .values()
        .any(|run| run.workspace == workspace && run.cancellation_pending)
    {
        return Err("编辑器取消尚未确认，不能在此工程启动新运行".into());
    }
    if authority
        .get(session_id)
        .is_some_and(|run| run.run_id == run_id)
    {
        return Err("消息运行身份已使用，请使用新的消息身份".into());
    }
    if authority.get(session_id).is_some_and(|run| !run.cancelled) {
        return Err("此会话已有运行，需等待完成或先确认停止后再发送".into());
    }
    let mut used_run_ids = authority
        .get(session_id)
        .map(|run| run.used_run_ids.clone())
        .unwrap_or_default();
    if !used_run_ids.insert(run_id.to_string()) {
        return Err("此消息运行身份已被原生执行层接收过，请使用新的消息身份".into());
    }
    let root = std::path::PathBuf::from(workspace);
    if let Some(task) = crate::coding::task::list_tasks(&root)
        .into_iter()
        .filter_map(|summary| crate::coding::task::load(&root, &summary.id))
        .find(|task| task.session_id.as_deref() == Some(session_id))
    {
        crate::coding::changeset::checkpoint_bind_mode(&root, &task.id, run_id, mode)?;
    }
    authority.insert(
        session_id.into(),
        RunAuthority {
            grant: RunGrant {
                run_id: run_id.into(),
                workspace: workspace.into(),
                cancelled: false,
                cancellation_pending: false,
                editor_acknowledged: false,
            },
            app,
            used_run_ids,
        },
    );
    Ok(())
}

/// A late completion from a superseded turn must not retire its replacement.
/// Normal tools (including bounded children) have settled before CompleteEvent.
pub(crate) async fn complete_run(session_id: &str, run_id: &str) {
    let pending = {
        let mut authority = runs().lock().await;
        let external_pending = crate::coding_external_tools::has_run(session_id, run_id)
            .await
            .unwrap_or(true);
        if external_pending {
            let _ = crate::coding_external_tools::cancel(session_id, Some(run_id)).await;
        }
        authority.get_mut(session_id).and_then(|run| {
            if !run.grant.complete_if_matching(run_id) {
                return None;
            }
            if external_pending {
                run.cancellation_pending = true;
            }
            if run.cancellation_pending {
                emit_cancellation_state(&run.app, &run.workspace, session_id, run_id, true);
            }
            run.cancellation_pending.then(|| run.clone())
        })
    };
    let _ = pending;
}

pub(crate) async fn with_pending_action<T: Send + 'static>(
    operation_id: String,
    workspace: String,
    session_id: String,
    run_id: String,
    action: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let authority = runs().lock().await;
    let run = authority
        .get(&session_id)
        .ok_or("Document run is not registered")?;
    if run.cancelled || run.run_id != run_id || run.workspace != workspace {
        return Err("Document run was cancelled or replaced".into());
    }
    let registry = pending().lock().await;
    let entry = registry
        .get(&operation_id)
        .ok_or("Document operation is no longer active")?;
    if entry.workspace != workspace || entry.session_id != session_id || entry.run_id != run_id {
        return Err("Document operation identity mismatch".into());
    }
    if entry.phase != "before" || !matches!(entry.tool_kind.as_str(), "edit" | "write") {
        return Err("Active tool does not grant document writes".into());
    }
    let editor_leases = editors().lock().await;
    let canonical = std::path::Path::new(&workspace)
        .canonicalize()
        .map_err(|error| error.to_string())?
        .to_string_lossy()
        .into_owned();
    if editor_leases.contains_key(&canonical) {
        return Err("A mounted editor owns this workspace; disk fallback refused".into());
    }
    tokio::task::spawn_blocking(move || {
        let result = action();
        drop(editor_leases);
        drop(registry);
        drop(authority);
        result
    })
    .await
    .map_err(|error| error.to_string())?
}

static EDITORS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
fn editors() -> &'static Mutex<HashMap<String, String>> {
    EDITORS.get_or_init(Default::default)
}
#[tauri::command]
pub async fn coding_document_editor_attach(
    access: tauri::State<'_, crate::shell_fs::FilesystemAccess>,
    root: String,
    instance_id: String,
) -> Result<String, String> {
    if instance_id.is_empty() || instance_id.len() > 256 {
        return Err("Invalid editor instance".into());
    }
    let root = access
        .require_workspace(&root)?
        .to_string_lossy()
        .into_owned();
    editors().lock().await.insert(root.clone(), instance_id);
    Ok(root)
}
#[tauri::command]
pub async fn coding_document_editor_detach(root: String, instance_id: String) {
    let mut entries = editors().lock().await;
    if entries.get(&root) == Some(&instance_id) {
        entries.remove(&root);
    }
}
pub(crate) async fn has_editor(root: &str) -> bool {
    editors().lock().await.contains_key(root)
}

pub(crate) async fn validate_pending_target(
    operation_id: &str,
    workspace: &str,
    session_id: &str,
    run_id: &str,
    path: &str,
    write: bool,
) -> Result<(), String> {
    validate_pending(operation_id, workspace, session_id, run_id).await?;
    let registry = pending().lock().await;
    let entry = registry
        .get(operation_id)
        .ok_or("Document operation is no longer active")?;
    let allowed = if write {
        matches!(entry.tool_kind.as_str(), "edit" | "write")
    } else {
        matches!(entry.tool_kind.as_str(), "read" | "edit" | "write")
    };
    if entry.phase != "before" || !allowed {
        return Err("Active tool does not grant this document capability".into());
    }
    let root = std::path::Path::new(workspace);
    let requested = crate::coding_closed_documents::target_path(root, path)?;
    if !entry.targets.iter().any(|target| {
        crate::coding_closed_documents::target_path(root, target)
            .is_ok_and(|target| target == requested)
    }) {
        return Err("Document target differs from the active tool".into());
    }
    Ok(())
}

#[tauri::command]
pub async fn coding_document_editor_present(
    access: tauri::State<'_, crate::shell_fs::FilesystemAccess>,
    root: String,
) -> Result<bool, String> {
    let root = access
        .require_workspace(&root)?
        .to_string_lossy()
        .into_owned();
    Ok(has_editor(&root).await)
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    #[tokio::test]
    async fn missing_or_late_editor_ack_cannot_confirm_stop() {
        let (tx, rx) = oneshot::channel();
        drop(tx);
        assert!(await_editor_fence(rx, std::time::Duration::from_secs(1))
            .await
            .is_err());
        let (tx, rx) = oneshot::channel();
        assert!(await_editor_fence(rx, std::time::Duration::from_millis(1))
            .await
            .is_err());
        assert!(
            tx.send(()).is_err(),
            "late ACK cannot revive an expired fence"
        );
        let (tx, rx) = oneshot::channel();
        tx.send(()).unwrap();
        assert!(await_editor_fence(rx, std::time::Duration::from_secs(1))
            .await
            .is_ok());
    }
    #[test]
    fn completion_retires_only_the_exact_registered_turn() {
        let mut grant = RunGrant {
            run_id: "replacement".into(),
            workspace: "/workspace".into(),
            cancelled: false,
            cancellation_pending: true,
            editor_acknowledged: false,
        };
        assert!(!grant.complete_if_matching("old-turn"));
        assert!(
            !grant.cancelled,
            "late send-now completion cannot revoke the new grant"
        );
        assert!(grant.complete_if_matching("replacement"));
        assert!(
            grant.cancelled,
            "finished turns cannot admit late dispatches"
        );
        assert!(
            grant.cancellation_pending,
            "Runtime completion cannot fabricate an editor ACK"
        );
        assert!(
            grant.complete_if_matching("replacement"),
            "duplicate completion is idempotent"
        );
    }
    #[test]
    fn editor_ack_does_not_confirm_an_inflight_external_tool_stopped() {
        let mut grant = RunGrant {
            run_id: "mcp-run".into(),
            workspace: "/fixture".into(),
            cancelled: false,
            cancellation_pending: false,
            editor_acknowledged: false,
        };
        assert!(grant.begin_cancellation_if_matching(Some("mcp-run")));
        assert!(grant.acknowledge_if_matching("/fixture", "mcp-run", true));
        assert!(grant.editor_acknowledged && grant.cancellation_pending);
        assert!(grant.complete_if_matching("mcp-run"));
        assert!(
            grant.cancellation_pending,
            "Runtime Complete cannot settle an opaque external call"
        );
        assert!(grant.acknowledge_if_matching("/fixture", "mcp-run", false));
        assert!(!grant.cancellation_pending);
    }
    #[test]
    fn late_ack_releases_only_its_own_workspace_and_run() {
        let mut grant = RunGrant {
            run_id: "active-run".into(),
            workspace: "/workspace".into(),
            cancelled: true,
            cancellation_pending: true,
            editor_acknowledged: false,
        };
        assert!(!grant.begin_cancellation_if_matching(Some("superseded-run")));
        assert!(!grant.acknowledge_if_matching("/other-workspace", "active-run", false));
        assert!(!grant.acknowledge_if_matching("/workspace", "superseded-run", false));
        assert!(grant.cancellation_pending);
        assert!(grant.acknowledge_if_matching("/workspace", "active-run", false));
        assert!(!grant.cancellation_pending);
        assert!(
            grant.cancelled,
            "acknowledgement does not reactivate a cancelled grant"
        );
    }
}
