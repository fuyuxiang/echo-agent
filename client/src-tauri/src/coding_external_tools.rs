//! Durable identity-only leases for opaque MCP effects. A local cancellation
//! does not prove that a remote server stopped; only its terminal response does.
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf, sync::OnceLock};
use tokio::sync::Mutex;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Lease {
    pub workspace: String,
    pub session_id: String,
    pub run_id: String,
    pub operation_id: String,
    pub tool_name: String,
    pub cancellation_requested: bool,
    pub unknown: bool,
    pub terminal: bool,
    #[serde(skip_deserializing)]
    pub local_inflight: bool,
}

struct Store {
    path: PathBuf,
    leases: HashMap<String, Lease>,
}
impl Store {
    fn open(path: PathBuf) -> Result<Self, String> {
        let mut leases = if path.exists() {
            let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() > 2 * 1024 * 1024
            {
                return Err("外部工具执行记录无效；工程保持保护状态".into());
            }
            let records: Vec<Lease> =
                serde_json::from_slice(&std::fs::read(&path).map_err(|error| error.to_string())?)
                    .map_err(|_| "外部工具执行记录损坏；工程保持保护状态")?;
            if records.len() > 512 {
                return Err("外部工具执行记录超过安全上限".into());
            }
            records
                .into_iter()
                .map(|mut lease| {
                    // A restarted process no longer owns the actual RPC future.
                    if !lease.terminal {
                        lease.unknown = true;
                        lease.cancellation_requested = true;
                    }
                    (lease.operation_id.clone(), lease)
                })
                .collect()
        } else {
            HashMap::new()
        };
        let store = Self {
            path,
            leases: std::mem::take(&mut leases),
        };
        if !store.leases.is_empty() {
            store.persist()?;
        }
        Ok(store)
    }
    fn persist(&self) -> Result<(), String> {
        let records = self.leases.values().cloned().collect::<Vec<_>>();
        crate::coding::store::write_json(&self.path, &records)
    }
    fn arm(&mut self, lease: Lease) -> Result<(), String> {
        if self.leases.len() >= 512 {
            // Completed older runs have no native execution grant. Keep every
            // active-run tombstone and all unresolved identities.
            let session = lease.session_id.clone();
            let run = lease.run_id.clone();
            self.leases
                .retain(|_, old| !old.terminal || (old.session_id == session && old.run_id == run));
        }
        if self.leases.values().filter(|lease| !lease.terminal).count() >= 64
            || self.leases.len() >= 512
            || self.leases.contains_key(&lease.operation_id)
        {
            return Err("外部工具执行已登记或协调器繁忙；没有重放调用".into());
        }
        let operation = lease.operation_id.clone();
        self.leases.insert(operation.clone(), lease);
        // Keep the in-memory unknown fence even on persistence failure.
        if let Err(error) = self.persist() {
            if let Some(lease) = self.leases.get_mut(&operation) {
                lease.unknown = true;
                lease.cancellation_requested = true;
                lease.local_inflight = false;
            }
            return Err(error);
        }
        Ok(())
    }
    fn settle(
        &mut self,
        workspace: &str,
        session: &str,
        run: &str,
        operation: &str,
        confirmed: bool,
    ) -> Result<(), String> {
        let lease = self
            .leases
            .get_mut(operation)
            .ok_or("没有与此结束回执匹配的外部工具执行")?;
        if lease.workspace != workspace || lease.session_id != session || lease.run_id != run {
            return Err("外部工具结束回执归属不匹配".into());
        }
        if lease.terminal {
            return if confirmed {
                Ok(())
            } else {
                Err("已结束的外部工具不能被迟到的未知状态重新激活".into())
            };
        }
        let before = lease.clone();
        lease.local_inflight = false;
        if confirmed {
            lease.terminal = true;
            lease.unknown = false;
            lease.cancellation_requested = false;
        } else {
            lease.unknown = true;
            lease.cancellation_requested = true;
        }
        if let Err(error) = self.persist() {
            // Preserve a fail-closed identity in memory even if the atomic
            // metadata replacement failed after the RPC returned.
            let mut before = before;
            before.unknown = true;
            before.cancellation_requested = true;
            before.local_inflight = false;
            self.leases.insert(operation.into(), before);
            return Err(error);
        }
        Ok(())
    }
    fn validate(
        &self,
        workspace: &str,
        session: &str,
        run: &str,
        operation: &str,
    ) -> Result<bool, String> {
        let lease = self
            .leases
            .get(operation)
            .ok_or("没有与此结束回执匹配的外部工具执行")?;
        if lease.workspace != workspace || lease.session_id != session || lease.run_id != run {
            return Err("外部工具结束回执归属不匹配".into());
        }
        Ok(lease.terminal)
    }
    fn ensure_admission(&self, workspace: &str) -> Result<(), String> {
        if self
            .leases
            .values()
            .any(|lease| !lease.terminal && lease.workspace == workspace)
        {
            return Err("外部工具仍在执行或停止状态未知；工程禁止新运行和写入，直到真实工具结束回执确认。不会自动重放调用".into());
        }
        Ok(())
    }
    fn recover(&mut self, workspace: &str, operation: &str, note: &str) -> Result<Lease, String> {
        let lease = self
            .leases
            .get(operation)
            .ok_or("外部调用记录不存在，请刷新")?;
        if lease.workspace != workspace
            || lease.terminal
            || (!lease.unknown && !lease.cancellation_requested)
        {
            return Err("该外部调用不处于待核验状态".into());
        }
        if lease.local_inflight {
            return Err(
                "本地仍持有实际调用，请等待返回或结束连接后再核验；不会提前放行写入".into(),
            );
        }
        let note = note.trim();
        if !(8..=2000).contains(&note.chars().count()) {
            return Err("请填写 8～2000 字的核验依据，包括工具实际结束或停止的情况".into());
        }
        let before = lease.clone();
        crate::coding::store::append_jsonl(
            &self.path.with_extension("recoveries.jsonl"),
            &serde_json::json!({"workspace":workspace,"operationId":operation,"sessionId":lease.session_id,
                "runId":lease.run_id,"toolName":lease.tool_name,"note":note,"confirmedAt":chrono::Utc::now().to_rfc3339()}),
        )?;
        let lease = self.leases.get_mut(operation).unwrap();
        lease.terminal = true;
        lease.unknown = false;
        lease.cancellation_requested = false;
        if let Err(error) = self.persist() {
            self.leases.insert(operation.into(), before);
            return Err(error);
        }
        Ok(self.leases[operation].clone())
    }
    fn pending(&self, workspace: Option<&str>) -> Vec<Lease> {
        self.leases
            .values()
            .filter(|lease| {
                !lease.terminal
                    && workspace.is_none_or(|root| lease.workspace == root)
                    && (lease.cancellation_requested || lease.unknown)
            })
            .cloned()
            .collect()
    }
}

static STORE: OnceLock<Mutex<Option<Store>>> = OnceLock::new();
async fn store() -> Result<tokio::sync::MutexGuard<'static, Option<Store>>, String> {
    let mut guard = STORE.get_or_init(Default::default).lock().await;
    if guard.is_none() {
        *guard = Some(Store::open(
            crate::paths::echo_agent_home_dir().join("coding-external-tool-leases.json"),
        )?);
    }
    Ok(guard)
}
pub(crate) async fn arm(
    workspace: &str,
    session: &str,
    run: &str,
    operation: &str,
    tool: &str,
) -> Result<(), String> {
    store().await?.as_mut().unwrap().arm(Lease {
        workspace: workspace.into(),
        session_id: session.into(),
        run_id: run.into(),
        operation_id: operation.into(),
        tool_name: tool.into(),
        cancellation_requested: false,
        unknown: false,
        terminal: false,
        local_inflight: true,
    })
}
pub(crate) async fn settle(
    workspace: &str,
    session: &str,
    run: &str,
    operation: &str,
    confirmed: bool,
) -> Result<(), String> {
    store()
        .await?
        .as_mut()
        .unwrap()
        .settle(workspace, session, run, operation, confirmed)
}
pub(crate) async fn cancel(session: &str, expected_run: Option<&str>) -> Result<(), String> {
    let mut guard = store().await?;
    let store = guard.as_mut().unwrap();
    for lease in store.leases.values_mut().filter(|lease| {
        !lease.terminal
            && lease.session_id == session
            && expected_run.is_none_or(|run| lease.run_id == run)
    }) {
        lease.cancellation_requested = true;
    }
    store.persist()
}
pub(crate) async fn has_run(session: &str, run: &str) -> Result<bool, String> {
    Ok(store()
        .await?
        .as_ref()
        .unwrap()
        .leases
        .values()
        .any(|lease| !lease.terminal && lease.session_id == session && lease.run_id == run))
}
pub(crate) async fn ensure_admission(workspace: &str) -> Result<(), String> {
    store().await?.as_ref().unwrap().ensure_admission(workspace)
}
pub(crate) async fn validate(
    workspace: &str,
    session: &str,
    run: &str,
    operation: &str,
) -> Result<bool, String> {
    store()
        .await?
        .as_ref()
        .unwrap()
        .validate(workspace, session, run, operation)
}
pub(crate) async fn ensure_new_run_identity(session: &str, run: &str) -> Result<(), String> {
    if store()
        .await?
        .as_ref()
        .unwrap()
        .leases
        .values()
        .any(|lease| lease.session_id == session && lease.run_id == run)
    {
        return Err("消息运行身份已使用，请创建新的消息身份；不会重放旧工具".into());
    }
    Ok(())
}

pub(crate) async fn pending(workspace: Option<&str>) -> Result<Vec<Lease>, String> {
    Ok(store().await?.as_ref().unwrap().pending(workspace))
}

#[tauri::command]
pub async fn coding_external_tools_pending(
    access: tauri::State<'_, crate::shell_fs::FilesystemAccess>,
    root: String,
) -> Result<Vec<Lease>, String> {
    let root = access.require_workspace(&root)?;
    pending(Some(&root.to_string_lossy())).await
}

#[tauri::command]
pub async fn coding_external_tool_recover(
    app: tauri::AppHandle,
    access: tauri::State<'_, crate::shell_fs::FilesystemAccess>,
    root: String,
    operation_id: String,
    note: String,
    confirmed_stopped: bool,
) -> Result<(), String> {
    if !confirmed_stopped {
        return Err("请先核实外部工具已结束或停止".into());
    }
    let root = access.require_workspace(&root)?;
    let pending_lease = pending(Some(&root.to_string_lossy()))
        .await?
        .into_iter()
        .find(|lease| lease.operation_id == operation_id)
        .ok_or("外部调用已结束或记录已变化，请刷新")?;
    if pending_lease.local_inflight {
        return Err("本地仍持有实际调用，请等待返回后再核验".into());
    }
    // Keep admission fenced until current disk effects and the original run's
    // external observations are refreshed. Manual recovery never attributes
    // opaque effects to versioned editor operations.
    if let Some(task) = crate::coding::task::list_tasks(&root)
        .into_iter()
        .filter_map(|summary| crate::coding::task::load(&root, &summary.id))
        .find(|task| task.session_id.as_deref() == Some(&pending_lease.session_id))
    {
        let changes = crate::coding::changeset::sync_changes(&root, &task.id).await?;
        crate::coding::changeset::checkpoint_observe_external_terminal(
            &root,
            &task.id,
            &pending_lease.run_id,
        )?;
        use tauri::Emitter;
        let _ = app.emit("coding://changes-observed", serde_json::json!({
            "workspace":pending_lease.workspace,"taskId":task.id,"sessionId":pending_lease.session_id,
            "runId":pending_lease.run_id,"commandRunId":operation_id,"contentRevision":changes.content_revision()
        }));
    }
    let lease =
        store()
            .await?
            .as_mut()
            .unwrap()
            .recover(&root.to_string_lossy(), &operation_id, &note)?;
    crate::coding_document_bridge::external_recovered(
        &app,
        &lease.workspace,
        &lease.session_id,
        &lease.run_id,
    )
    .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn lease() -> Lease {
        Lease {
            workspace: "/private/fixture".into(),
            session_id: "session".into(),
            run_id: "run".into(),
            operation_id: "run:child:call".into(),
            tool_name: "use_tool".into(),
            cancellation_requested: false,
            unknown: false,
            terminal: false,
            local_inflight: true,
        }
    }
    #[test]
    fn cancellation_and_restart_never_invent_external_quiescence() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("metadata.json");
        let mut store = Store::open(path.clone()).unwrap();
        store.arm(lease()).unwrap();
        assert!(store.ensure_admission("/private/fixture").is_err());
        assert!(store.ensure_admission("/unrelated").is_ok());
        assert!(
            store.pending(None).is_empty(),
            "a normally running RPC is not a stopped run"
        );
        store
            .leases
            .get_mut("run:child:call")
            .unwrap()
            .cancellation_requested = true;
        store.persist().unwrap();
        assert_eq!(
            store.pending(None).len(),
            1,
            "editor ACK alone cannot remove this lease"
        );
        assert!(store
            .settle("/other", "session", "run", "run:child:call", true)
            .is_err());
        store
            .settle(
                "/private/fixture",
                "session",
                "run",
                "run:child:call",
                false,
            )
            .unwrap();
        let mut recovered = Store::open(path).unwrap();
        assert!(
            recovered.pending(None)[0].unknown,
            "a restarted process cannot prove the old RPC stopped"
        );
        recovered
            .settle(
                "/private/fixture",
                "session",
                "run",
                "run:child:call",
                false,
            )
            .unwrap();
        assert_eq!(recovered.pending(None).len(), 1);
        assert!(
            recovered.ensure_admission("/private/fixture").is_err(),
            "restart/editor ACK cannot release unknown external effects"
        );
        let metadata = std::fs::read_to_string(&recovered.path).unwrap();
        assert!(
            !metadata.contains("toolInput")
                && !metadata.contains("apiKey")
                && !metadata.contains("code")
        );
    }
    #[test]
    fn only_exact_actual_terminal_releases_the_persisted_lease() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("metadata.json");
        let mut store = Store::open(path.clone()).unwrap();
        store.arm(lease()).unwrap();
        assert!(store
            .settle(
                "/private/fixture",
                "session",
                "old-run",
                "run:child:call",
                true
            )
            .is_err());
        store
            .settle("/private/fixture", "session", "run", "run:child:call", true)
            .unwrap();
        assert!(
            store.arm(lease()).is_err(),
            "a durable operation tombstone prevents duplicate MCP execution"
        );
        assert!(
            store
                .settle("/private/fixture", "session", "run", "run:child:call", true)
                .is_ok(),
            "matching terminal is idempotent"
        );
        assert!(store.ensure_admission("/private/fixture").is_ok());
        assert!(Store::open(path).unwrap().pending(None).is_empty());
    }
    #[test]
    fn unknown_calls_can_be_recovered_with_an_audit_but_live_calls_cannot() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("leases.json");
        let mut live = Store::open(path.clone()).unwrap();
        live.arm(lease()).unwrap();
        live.leases
            .get_mut("run:child:call")
            .unwrap()
            .cancellation_requested = true;
        assert!(live
            .recover(
                "/private/fixture",
                "run:child:call",
                "Verified remote execution has finished"
            )
            .is_err());
        live.persist().unwrap();
        let mut restarted = Store::open(path.clone()).unwrap();
        assert!(!restarted.pending(None)[0].local_inflight);
        assert!(restarted
            .recover(
                "/other",
                "run:child:call",
                "Verified remote execution has finished"
            )
            .is_err());
        assert!(restarted
            .recover("/private/fixture", "run:child:call", "short")
            .is_err());
        restarted
            .recover(
                "/private/fixture",
                "run:child:call",
                "Verified remote execution has finished",
            )
            .unwrap();
        assert!(restarted.ensure_admission("/private/fixture").is_ok());
        assert!(Store::open(path.clone()).unwrap().pending(None).is_empty());
        assert!(
            std::fs::read_to_string(path.with_extension("recoveries.jsonl"))
                .unwrap()
                .contains("Verified remote execution has finished")
        );
    }
}
