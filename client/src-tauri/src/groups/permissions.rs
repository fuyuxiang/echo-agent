//! A group owns its approval strategy. Member actors are applications of that
//! setting, never independent sources of authority.
use super::*;
use crate::bridge::Permissions;
use crate::commands::AppState;
use crate::permission_config::{
    self as config, PermissionModeSetResult, PermissionModeStatus, PermissionRuntimeSyncState,
};
use echo_agent_acp::AcpAgentTx;

pub(super) fn effective_mode(group: &Group) -> String {
    let capabilities = config::permission_mode_status(false, None);
    let configured =
        crate::policy::locked_permission_mode().unwrap_or_else(|| group.permission_mode.clone());
    config::effective_permission_mode(
        &configured,
        capabilities.auto_mode_available,
        capabilities.always_approve_available,
    )
}

fn status(state: &AppState, group: &Group) -> PermissionModeStatus {
    let running = state.tx.lock().unwrap().is_some();
    let mut status = config::permission_mode_status(running, None);
    if !status.locked {
        status.configured_permission_mode = group.permission_mode.clone();
        status.permission_mode = config::effective_permission_mode(
            &group.permission_mode,
            status.auto_mode_available,
            status.always_approve_available,
        );
    }
    // The global Runtime default is not evidence that this group's actors
    // have applied their own mode. New/dormant groups apply it on next run.
    status.runtime_sync_state = PermissionRuntimeSyncState::Offline;
    status.runtime_applied_mode = None;
    status.runtime_sync_error = None;
    let sessions = group
        .members
        .iter()
        .filter(|member| !member.removed)
        .flat_map(|member| [&member.session_id, &member.discussion_session_id])
        .flatten()
        .filter(|sid| state.session_workspace(sid).is_ok());
    for sid in sessions {
        let member = config::permission_mode_status(running, Some(sid));
        if member.runtime_sync_state != PermissionRuntimeSyncState::Synced
            || member.runtime_applied_mode.as_deref() != Some(status.permission_mode.as_str())
        {
            status.runtime_sync_state = if running {
                PermissionRuntimeSyncState::Failed
            } else {
                PermissionRuntimeSyncState::Offline
            };
            status.runtime_sync_error =
                running.then(|| "部分成员的权限尚未同步，请重新选择权限模式重试".into());
            return status;
        }
        status.runtime_sync_state = PermissionRuntimeSyncState::Synced;
        status.runtime_applied_mode = Some(status.permission_mode.clone());
    }
    status
}

#[tauri::command]
pub fn group_permission_mode_get(
    state: State<'_, AppState>,
    group_id: String,
) -> Result<PermissionModeStatus, String> {
    Ok(status(&state, &db()?.header(&group_id)?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResidentSession {
    session_id: String,
    resident: bool,
}
#[derive(Deserialize)]
struct Roster {
    sessions: Vec<ResidentSession>,
}

/// Intersect the native resident roster with durable group ownership. Retired
/// contexts are included if still resident; another group's actors never are.
async fn resident_sessions(
    store: &Store,
    group_id: &str,
    tx: &AcpAgentTx,
) -> Result<Vec<String>, String> {
    let owned = store
        .sessions(group_id)?
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    let roster: Roster = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        crate::ext::call_ext(
            tx,
            "echo.agent/sessions/list",
            crate::ext::raw_params(&json!({})),
        ),
    )
    .await
    .map_err(|_| "读取成员权限状态超时，请重试".to_string())?
    .map_err(|error| format!("读取成员权限状态失败：{error}"))?;
    Ok(roster
        .sessions
        .into_iter()
        .filter(|entry| entry.resident && owned.contains(&entry.session_id))
        .map(|entry| entry.session_id)
        .collect())
}

#[derive(Debug)]
pub(super) struct TransitionError {
    pub message: String,
    pub rollback_failed: bool,
}

/// Native acknowledgement precedes persistence. A partial native failure or
/// disk failure restores the previous group mode across the entire target set.
pub(super) async fn transition(
    store: &Store,
    group_id: &str,
    tx: Option<&AcpAgentTx>,
    sessions: &[String],
    mode: &str,
    previous: &str,
) -> Result<(), TransitionError> {
    let apply = async {
        if let Some(tx) = tx {
            config::sync_group_session_permission_modes(tx, sessions, mode).await?;
        }
        store.update(group_id, |group| {
            group.permission_mode = mode.into();
            Ok(())
        })
    }
    .await;
    if let Err(error) = apply {
        let rollback_failed = if let Some(tx) = tx {
            config::sync_group_session_permission_modes(tx, sessions, previous)
                .await
                .is_err()
        } else {
            false
        };
        return Err(TransitionError {
            message: error,
            rollback_failed,
        });
    }
    Ok(())
}

#[tauri::command]
pub async fn group_permission_mode_set(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    permissions: State<'_, Permissions>,
    group_id: String,
    mode: String,
) -> Result<PermissionModeSetResult, String> {
    set_permission_mode(&app, &state, &permissions, &group_id, &mode).await
}

pub(crate) async fn set_permission_mode(
    app: &tauri::AppHandle,
    state: &AppState,
    permissions: &Permissions,
    group_id: &str,
    mode: &str,
) -> Result<PermissionModeSetResult, String> {
    // Same ordering as execution: group dispatch gate, then permission gate.
    let _dispatch = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        runtime::dispatch_lock(group_id).lock_owned(),
    )
    .await
    .map_err(|_| "成员仍在准备执行，请稍后重试切换权限".to_string())?;
    let _transition = config::permission_transition_lock().lock().await;
    let group = db()?.header(group_id)?;
    if group.archived {
        return Err("请先恢复专家团，再调整权限".into());
    }
    // resolve_new_session_permission_mode honors policy for creation. A write
    // must reject a conflicting request instead of reporting a false success.
    if crate::policy::locked_permission_mode().is_some_and(|locked| locked != mode) {
        return Err("权限模式已被组织策略锁定".into());
    }
    let mode = config::resolve_new_session_permission_mode(Some(mode))?;
    let previous = status(state, &group).permission_mode;
    let tx = state.tx.lock().unwrap().clone();
    let sessions = if let Some(tx) = tx.as_ref() {
        resident_sessions(db()?, group_id, tx).await?
    } else {
        vec![]
    };
    for sid in &sessions {
        config::mark_session_permission_mode_syncing(sid, &previous);
    }
    if let Err(error) = transition(db()?, group_id, tx.as_ref(), &sessions, &mode, &previous).await
    {
        if error.rollback_failed {
            if let Some(tx) = tx.as_ref() {
                if state.mark_runtime_dead_if_current(tx, &error.message) {
                    config::mark_runtime_permission_mode_offline(Some(error.message.clone()));
                    let _ = app.emit("agent://agent-died", json!({"reason":error.message}));
                }
            }
            return Err("成员权限同步与回滚未获确认，Agent 已安全停止；原权限配置已保留".into());
        }
        for sid in &sessions {
            config::mark_session_permission_mode_synced(sid, &previous);
        }
        return Err(format!("权限切换失败，已恢复原权限：{}", error.message));
    }
    if tx.as_ref().is_some_and(|tx| !state.is_runtime_current(tx)) {
        changed(group_id);
        return Err("权限设置已保存，但 Agent 已断开；下次执行前将重新同步".into());
    }
    for sid in &sessions {
        config::mark_session_permission_mode_synced(sid, &mode);
        let _ = app.emit(
            "agent://permission-mode",
            config::permission_mode_status(true, Some(sid)),
        );
    }
    changed(group_id);
    let owned = db()?.sessions(group_id)?;
    let remaining_pending = permissions
        .list(None)
        .await
        .iter()
        .filter(|request| owned.contains(&request.session_id))
        .count();
    Ok(PermissionModeSetResult {
        status: status(state, &db()?.header(group_id)?),
        agent_running: tx.is_some(),
        runtime_synced: tx.is_some(),
        resolved_pending: 0,
        remaining_pending,
        resolved_permissions: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_acp::{acp_channels, AcpAgentMessage};
    use std::sync::Arc;

    #[tokio::test]
    async fn offline_selection_is_durable_and_isolated_from_other_groups() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("groups.sqlite");
        let store = Store::open(&path).unwrap();
        let group = super::super::tests::group();
        let other = super::super::tests::group();
        store.create(&group).unwrap();
        store.create(&other).unwrap();
        transition(&store, &group.id, None, &[], "auto", "ask")
            .await
            .unwrap();
        let reloaded = Store::open(&path).unwrap();
        assert_eq!(reloaded.get(&group.id).unwrap().permission_mode, "auto");
        assert_eq!(reloaded.get(&other.id).unwrap().permission_mode, "ask");
    }

    #[tokio::test]
    async fn all_members_acknowledge_before_the_group_selection_is_saved() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&home.path().join("groups.sqlite")).unwrap());
        let group = super::super::tests::group();
        store.create(&group).unwrap();
        let (client, mut agent) = acp_channels();
        let task = tokio::spawn({
            let store = store.clone();
            let id = group.id.clone();
            async move {
                transition(
                    &store,
                    &id,
                    Some(&client.tx),
                    &["coordinator".into(), "expert".into(), "discussion".into()],
                    "always-approve",
                    "ask",
                )
                .await
            }
        });
        let AcpAgentMessage::ExtNotification(args) = agent.rx.recv().await.unwrap() else {
            panic!("expected permission notification");
        };
        let params: Value = serde_json::from_str(args.request.params.get()).unwrap();
        assert_eq!(
            params["sessionIds"],
            json!(["coordinator", "expert", "discussion"])
        );
        assert_eq!(params["permission_mode"], "always-approve");
        assert_eq!(params["promptOnAutoDeny"], true);
        assert_eq!(store.get(&group.id).unwrap().permission_mode, "ask");
        args.response_tx.send(Ok(())).unwrap();
        task.await.unwrap().unwrap();
        assert_eq!(
            store.get(&group.id).unwrap().permission_mode,
            "always-approve"
        );
    }

    #[tokio::test]
    async fn failed_native_sync_rolls_back_every_target_and_preserves_selection() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&home.path().join("groups.sqlite")).unwrap());
        let group = super::super::tests::group();
        store.create(&group).unwrap();
        let (client, mut agent) = acp_channels();
        let task = tokio::spawn({
            let store = store.clone();
            let id = group.id.clone();
            async move {
                transition(
                    &store,
                    &id,
                    Some(&client.tx),
                    &["coordinator".into(), "expert".into()],
                    "auto",
                    "ask",
                )
                .await
            }
        });
        let AcpAgentMessage::ExtNotification(args) = agent.rx.recv().await.unwrap() else {
            panic!()
        };
        args.response_tx
            .send(Err(agent_client_protocol::Error::internal_error()))
            .unwrap();
        let AcpAgentMessage::ExtNotification(args) = agent.rx.recv().await.unwrap() else {
            panic!()
        };
        let params: Value = serde_json::from_str(args.request.params.get()).unwrap();
        assert_eq!(params["permission_mode"], "ask");
        assert_eq!(params["sessionIds"], json!(["coordinator", "expert"]));
        args.response_tx.send(Ok(())).unwrap();
        assert!(!task.await.unwrap().unwrap_err().rollback_failed);
        assert_eq!(store.get(&group.id).unwrap().permission_mode, "ask");
    }

    #[tokio::test]
    async fn failed_persistence_rolls_back_and_reports_an_unacknowledged_rollback() {
        for reject_rollback in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let store = Arc::new(Store::open(&home.path().join("groups.sqlite")).unwrap());
            let (client, mut agent) = acp_channels();
            // No such durable group: simulates a failed save after native ACK.
            let task = tokio::spawn(async move {
                transition(
                    &store,
                    "missing",
                    Some(&client.tx),
                    &["member".into()],
                    "auto",
                    "ask",
                )
                .await
            });
            let AcpAgentMessage::ExtNotification(args) = agent.rx.recv().await.unwrap() else {
                panic!()
            };
            args.response_tx.send(Ok(())).unwrap();
            let AcpAgentMessage::ExtNotification(args) = agent.rx.recv().await.unwrap() else {
                panic!()
            };
            let params: Value = serde_json::from_str(args.request.params.get()).unwrap();
            assert_eq!(params["permission_mode"], "ask");
            args.response_tx
                .send(if reject_rollback {
                    Err(agent_client_protocol::Error::internal_error())
                } else {
                    Ok(())
                })
                .unwrap();
            assert_eq!(
                task.await.unwrap().unwrap_err().rollback_failed,
                reject_rollback
            );
        }
    }

    #[tokio::test]
    async fn roster_filters_dormant_and_foreign_sessions_but_keeps_retired_residents() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&home.path().join("groups.sqlite")).unwrap());
        let group = super::super::tests::group();
        store.create(&group).unwrap();
        for sid in ["active", "retired", "dormant"] {
            store
                .bind_owner(sid, &group.id, &group.coordinator_id)
                .unwrap();
        }
        let (client, mut agent) = acp_channels();
        let task = tokio::spawn({
            let store = store.clone();
            let id = group.id.clone();
            async move { resident_sessions(&store, &id, &client.tx).await }
        });
        let AcpAgentMessage::ExtMethod(args) = agent.rx.recv().await.unwrap() else {
            panic!()
        };
        assert_eq!(args.request.method.as_ref(), "echo.agent/sessions/list");
        args.response_tx
            .send(Ok(agent_client_protocol::ExtResponse::new(
                crate::ext::raw_params(&json!({"result":{"sessions":[
                    {"sessionId":"active","resident":true},{"sessionId":"retired","resident":true},
                    {"sessionId":"dormant","resident":false},{"sessionId":"foreign","resident":true}
                ]}})),
            )))
            .unwrap();
        assert_eq!(task.await.unwrap().unwrap(), vec!["active", "retired"]);
    }
}
