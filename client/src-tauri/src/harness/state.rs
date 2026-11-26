//! State shared by desktop IPC, the Runtime bridge and product services.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::agent_runtime::{AgentHandle, InitOutcome};
use super::session_creation::SessionCreations;

#[derive(Debug, Clone, Default)]
pub(super) struct RuntimeModelState {
    pub(super) initialized: bool,
    pub(super) revision: Option<String>,
    pub(super) model_ids: Vec<String>,
    /// Authentication method reconciled from the same Runtime model reload.
    /// A catalog without this acknowledgement is not usable by `session/new`.
    pub(super) auth_method_id: Option<String>,
    pub(super) last_error: Option<String>,
    /// Sender identity that produced this snapshot. A late response from a
    /// retired Runtime must never mark a newly-started Runtime as ready.
    pub(super) sender: Option<echo_agent_acp::AcpAgentTx>,
}

/// Host-owned runtime generation, session admission and acknowledged model state.
#[derive(Default)]
pub struct HarnessState {
    pub handle: Mutex<Option<AgentHandle>>,
    /// Once the dispatcher owns the rx half, only the tx is reachable. We
    /// stash a clone of the tx sender here for commands to use.
    pub tx: Mutex<Option<echo_agent_acp::AcpAgentTx>>,
    pub cwd: Mutex<Option<PathBuf>>,
    /// Background automation scheduler bound to the current agent runtime.
    /// Replacing/aborting it on restart prevents a stale task from retaining a
    /// closed ACP sender forever.
    pub automation_scheduler: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    /// Last model configuration positively acknowledged by the embedded
    /// Runtime. Disk configuration alone must never unlock message sending.
    pub(super) runtime_models: Mutex<RuntimeModelState>,
    /// A synchronous readiness check may be the first observer of an expired
    /// organization-model lease. Remember that disk mutation until an
    /// app-aware caller can start the serialized Runtime hot reload.
    runtime_model_reload_pending: std::sync::atomic::AtomicBool,
    /// Successful ACP initialize response for the installed Runtime. A WebView
    /// reload reuses this generation instead of cancelling unattended work.
    pub(crate) init_outcome: Mutex<Option<InitOutcome>>,
    /// Serialize Runtime model reloads. Organization sync, settings edits and
    /// startup can all request a reload at the same time; overlapping ACP
    /// reloads make it impossible to know which disk revision was applied.
    pub(crate) model_reload_lock: tokio::sync::Mutex<()>,
    /// Serialize `agent_init`. The command is reachable from startup, the retry
    /// button and the settings dialog at once; interleaved runs would let an
    /// earlier call's failure cleanup tear down a later, healthy Runtime.
    pub(crate) init_lock: tokio::sync::Mutex<()>,
    /// Monotonic initialization generation. Cleanup paths and lifecycle events
    /// carry the generation that produced them so a retired Runtime can never
    /// invalidate its replacement.
    pub(crate) init_generation: std::sync::atomic::AtomicU64,
    /// Sessions created by a `new_session` request whose caller had already
    /// timed out. The Runtime completes and persists these regardless, so they
    /// are tracked here and reclaimed instead of leaking as ghost sessions.
    orphaned_sessions: Mutex<Vec<OrphanedSession>>,
    pub(super) session_creations: SessionCreations,
    /// Native-authoritative workspace binding for every session admitted into
    /// the current Runtime generation. Renderer-provided session ids and paths
    /// are never sufficient to authorize attachment reads.
    pub(super) session_workspaces: Mutex<HashMap<String, PathBuf>>,
}

/// A session the Runtime finished creating after its caller stopped waiting.
/// `cwd`, model and permission mode are kept so a retry only adopts a session
/// that matches what it would have asked for.
#[derive(Debug, Clone)]
pub(crate) struct OrphanedSession {
    pub(crate) session_id: String,
    pub(crate) cwd: String,
    pub(crate) model_id: Option<String>,
    pub(crate) permission_mode: String,
    pub(crate) profile_key: Option<String>,
    pub(crate) project_id: Option<String>,
}

impl HarnessState {
    /// Claim the next initialization generation. Every later state mutation from
    /// that `agent_init` call is gated on the claimed value still being current.
    pub(crate) fn begin_init_generation(&self) -> u64 {
        self.init_generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1
    }

    pub(crate) fn is_current_generation(&self, generation: u64) -> bool {
        self.init_generation
            .load(std::sync::atomic::Ordering::SeqCst)
            == generation
    }

    pub(crate) fn is_runtime_current(&self, sender: &echo_agent_acp::AcpAgentTx) -> bool {
        self.tx
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|current| current.same_channel(sender))
    }

    /// Record a session that arrived after its caller stopped waiting. The
    /// Runtime already persisted it, so the id is kept for reclamation.
    pub(crate) fn record_orphaned_session(&self, orphan: OrphanedSession) {
        let mut orphaned = self.orphaned_sessions.lock().unwrap();
        if !orphaned
            .iter()
            .any(|existing| existing.session_id == orphan.session_id)
        {
            orphaned.push(orphan);
        }
    }

    /// Adopt a previously orphaned session that matches the request a caller is
    /// about to make, so a retry after a timeout reuses the session the Runtime
    /// already created instead of leaving it behind and making another.
    pub(crate) fn take_orphaned_session(
        &self,
        cwd: &str,
        model_id: Option<&str>,
        permission_mode: &str,
        profile_key: Option<&str>,
        project_id: Option<&str>,
    ) -> Option<String> {
        let mut orphaned = self.orphaned_sessions.lock().unwrap();
        let index = orphaned.iter().position(|candidate| {
            candidate.cwd == cwd
                && candidate.model_id.as_deref() == model_id
                && candidate.permission_mode == permission_mode
                && candidate.profile_key.as_deref() == profile_key
                && candidate.project_id.as_deref() == project_id
        })?;
        Some(orphaned.remove(index).session_id)
    }

    pub(crate) fn clear_orphaned_sessions(&self) {
        self.orphaned_sessions.lock().unwrap().clear();
        self.session_creations.clear();
    }

    pub(crate) fn record_session_workspace(
        &self,
        session_id: &str,
        cwd: &Path,
    ) -> Result<(), String> {
        self.record_session_workspace_with_project(session_id, cwd, None)
    }

    pub(crate) fn record_session_workspace_with_project(
        &self,
        session_id: &str,
        cwd: &Path,
        project_id: Option<&str>,
    ) -> Result<(), String> {
        crate::meta::bind_session_scope(session_id, cwd, project_id)?;
        self.session_workspaces
            .lock()
            .unwrap()
            .insert(session_id.to_string(), cwd.to_path_buf());
        Ok(())
    }

    pub(crate) fn session_workspace(&self, session_id: &str) -> Result<PathBuf, String> {
        self.session_workspaces
            .lock()
            .unwrap()
            .get(session_id)
            .cloned()
            .ok_or_else(|| "当前会话尚未建立可信工作区绑定，请重新打开会话".to_string())
    }

    pub(crate) fn session_ids(&self) -> Vec<String> {
        self.session_workspaces
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    }

    pub(crate) fn forget_session_workspace(&self, session_id: &str) {
        self.session_workspaces.lock().unwrap().remove(session_id);
    }

    pub(crate) fn clear_session_workspaces(&self) {
        self.session_workspaces.lock().unwrap().clear();
    }

    pub(crate) fn mark_runtime_models_initializing(&self) {
        *self.runtime_models.lock().unwrap() = RuntimeModelState::default();
    }

    pub(super) fn mark_runtime_model_reload_pending(&self) {
        self.mark_runtime_models_initializing();
        self.runtime_model_reload_pending
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub(super) fn take_runtime_model_reload_pending(&self) -> bool {
        self.runtime_model_reload_pending
            .swap(false, std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) fn model_config_is_current(&self) -> bool {
        let revision = crate::providers::model_config_revision();
        let sender = self.tx.lock().unwrap();
        let runtime = self.runtime_models.lock().unwrap();
        runtime.initialized
            && runtime.last_error.is_none()
            && runtime.auth_method_id.is_some()
            && runtime.revision.as_deref() == Some(revision.as_str())
            && sender.as_ref().is_some_and(|sender| {
                runtime
                    .sender
                    .as_ref()
                    .is_some_and(|snapshot| sender.same_channel(snapshot))
            })
    }

    pub(crate) fn mark_runtime_models_initializing_if_current(
        &self,
        sender: &echo_agent_acp::AcpAgentTx,
    ) -> bool {
        let tx = self.tx.lock().unwrap();
        if !tx
            .as_ref()
            .is_some_and(|current| current.same_channel(sender))
        {
            return false;
        }
        *self.runtime_models.lock().unwrap() = RuntimeModelState::default();
        true
    }

    pub(crate) fn mark_runtime_models_synced(
        &self,
        sender: &echo_agent_acp::AcpAgentTx,
        revision: String,
        model_ids: Vec<String>,
        auth_method_id: String,
    ) -> bool {
        let current_sender = self.tx.lock().unwrap();
        if !current_sender
            .as_ref()
            .is_some_and(|current| current.same_channel(sender))
        {
            return false;
        }
        *self.runtime_models.lock().unwrap() = RuntimeModelState {
            initialized: true,
            revision: Some(revision),
            model_ids,
            auth_method_id: Some(auth_method_id),
            last_error: None,
            sender: Some(sender.clone()),
        };
        true
    }

    pub(crate) fn mark_runtime_models_failed(&self, error: impl Into<String>) {
        let mut runtime = self.runtime_models.lock().unwrap();
        runtime.initialized = false;
        runtime.revision = None;
        runtime.model_ids.clear();
        runtime.auth_method_id = None;
        runtime.sender = None;
        runtime.last_error = Some(error.into());
    }

    pub(crate) fn mark_runtime_models_failed_if_current(
        &self,
        sender: &echo_agent_acp::AcpAgentTx,
        error: impl Into<String>,
    ) {
        let current_sender = self.tx.lock().unwrap();
        if current_sender
            .as_ref()
            .is_some_and(|current| current.same_channel(sender))
        {
            self.mark_runtime_models_failed(error);
        }
    }

    /// Invalidate the command-facing runtime only when `tx` is still the
    /// currently installed agent. An older agent can finish joining after a
    /// user has already restarted a replacement; it must not poison the new
    /// runtime's readiness state.
    /// Returns whether the teardown applied. Callers use the result to decide
    /// whether to emit a user-visible "agent died" event: an event from a
    /// retired Runtime would make the frontend mark its healthy replacement
    /// unavailable.
    pub(crate) fn mark_runtime_dead_if_current(
        &self,
        tx: &echo_agent_acp::AcpAgentTx,
        error: impl Into<String>,
    ) -> bool {
        let mut current_tx = self.tx.lock().unwrap();
        let is_current = current_tx
            .as_ref()
            .is_some_and(|current| current.same_channel(tx));
        if !is_current {
            return false;
        }
        current_tx.take();
        // Keep the sender guard through the synchronous state changes so an
        // initialization cannot publish a replacement between identity check
        // and teardown of its handle/readiness.
        if let Some(handle) = self.handle.lock().unwrap().take() {
            handle.cancel.cancel();
        }
        if let Some(scheduler) = self.automation_scheduler.lock().unwrap().take() {
            scheduler.abort();
        }
        crate::automations::clear_runtime_sessions();
        self.clear_orphaned_sessions();
        self.clear_session_workspaces();
        crate::agent_admin::clear_runtime_capabilities();
        self.init_outcome.lock().unwrap().take();
        self.mark_runtime_models_failed(error);
        crate::harness::permission_config::mark_runtime_permission_mode_offline(None);
        true
    }

    /// Remove a dead gateway from readiness immediately, while keeping its
    /// worker handle and LocalSet available for final persistence/retry.
    pub(super) fn quarantine_gateway_if_current(
        &self,
        tx: &echo_agent_acp::AcpAgentTx,
        error: String,
    ) -> bool {
        let mut current_tx = self.tx.lock().unwrap();
        if !current_tx
            .as_ref()
            .is_some_and(|current| current.same_channel(tx))
        {
            return false;
        }
        current_tx.take();
        // Match the same sender throughout quarantine, including its handle
        // and readiness. Publishing another sender must wait for this guard.
        if let Some(handle) = self.handle.lock().unwrap().as_ref() {
            handle.cancel.cancel();
        }
        if let Some(scheduler) = self.automation_scheduler.lock().unwrap().take() {
            scheduler.abort();
        }
        self.init_outcome.lock().unwrap().take();
        self.mark_runtime_models_failed(error);
        crate::harness::permission_config::mark_runtime_permission_mode_offline(None);
        true
    }
}
