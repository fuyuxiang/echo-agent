//! Backend trait abstracting how subagent operations are dispatched.
//!
//! `SubagentBackend` decouples the tool implementations (`TaskTool`,
//! `TaskOutputTool`, `KillTaskTool`) from the transport mechanism used to
//! communicate with the subagent coordinator.
//!
//! All hosts use [`ChannelBackend`].
//! The receiver is owned by the shared single-writer coordinator actor; only
//! the child runner plugged into that actor differs by host.

use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};

use super::types::{
    SpawnedSubagentRef, SubagentCancelOutcome, SubagentCancelRequest, SubagentCancelTarget,
    SubagentDescribeOutcome, SubagentDescribeRequest, SubagentEvent, SubagentInspectRequest,
    SubagentInspection, SubagentListRunningRequest, SubagentQueryRequest, SubagentRegistryCounts,
    SubagentRegistryCountsRequest, SubagentRequest, SubagentResult, SubagentSnapshot,
    SubagentSpawnRequest, SubagentSpawnedRefsRequest, SubagentValidateTypeOutcome,
    SubagentValidateTypeRequest,
};
use crate::register_resource;
use echo_agent_tool_runtime::ToolError;

/// Abstraction over the mechanism used to spawn, query, and cancel subagents.
///
/// Injected into `Resources` as [`SubagentBackendResource`] so that
/// `TaskTool`, `TaskOutputTool`, and `KillTaskTool` can operate
/// identically regardless of the underlying transport.
#[async_trait::async_trait]
pub trait SubagentBackend: Send + Sync + 'static {
    /// Spawn a subagent and await its result.
    ///
    /// For blocking mode the caller awaits the returned future directly.
    /// For background mode the caller spawns a tokio task around this call
    /// and drops the receiver immediately.
    async fn spawn(&self, request: SubagentRequest) -> Result<SubagentResult, ToolError>;

    /// Query the current state of a subagent by ID.
    ///
    /// When `block` is true the backend waits (up to `timeout_ms`) for the
    /// subagent to reach a terminal state before responding.
    async fn query(
        &self,
        id: &str,
        block: bool,
        timeout_ms: Option<u64>,
    ) -> Option<SubagentSnapshot>;

    /// Request cancellation of a subagent by ID.
    async fn cancel(&self, id: &str) -> SubagentCancelOutcome;

    /// Validate a subagent type synchronously before spawning.
    /// Returns `ValidationUnavailable` on channel close / responder drop / timeout.
    async fn validate_type(
        &self,
        subagent_type: &str,
        parent_session_id: &str,
    ) -> SubagentValidateTypeOutcome;

    /// Describe a subagent type's resolved toolset (tool names + capability
    /// flags) before spawning. Read-only: builds the agent definition and
    /// applies the same parent-dependent toolset re-selection a spawn would,
    /// then reports the result without starting a child session.
    ///
    /// Returns [`SubagentDescribeOutcome::Unavailable`] on channel close /
    /// responder drop / timeout (modeled exactly on [`Self::validate_type`]).
    ///
    /// `harness_agent_type` is the `/goal`-only harness override (see
    /// [`super::types::SubagentRuntimeOverrides::harness_agent_type`]); the
    /// coordinator resolves the toolset for `(subagent_type,
    /// harness_agent_type)`. `None` (every non-goal caller) defers the flavor
    /// to the parent agent.
    async fn describe_subagent_type(
        &self,
        subagent_type: &str,
        harness_agent_type: Option<&str>,
        parent_session_id: &str,
    ) -> SubagentDescribeOutcome;
}

/// Resource wrapper injected into every session's `Resources`.
///
/// Wraps an `Arc<dyn SubagentBackend>` so the backend can be shared across
/// concurrent tool invocations within the same session.
#[derive(Clone)]
pub struct SubagentBackendResource(pub Arc<dyn SubagentBackend>);

impl SubagentBackendResource {
    pub fn backend(&self) -> &dyn SubagentBackend {
        self.0.as_ref()
    }
}

impl std::fmt::Debug for SubagentBackendResource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubagentBackendResource").finish()
    }
}

register_resource!(
    "echo_agent_build",
    "SubagentBackendResource",
    SubagentBackendResource
);

/// In-process channel-based backend for the local host shell.
///
/// Wraps a single `mpsc::UnboundedSender<SubagentEvent>` that carries
/// spawn, query, and cancel messages to the coordinator. The oneshot for
/// `spawn` is created inside the backend so callers never manage it.
#[derive(Clone)]
pub struct ChannelBackend {
    tx: mpsc::UnboundedSender<SubagentEvent>,
    parent_session_id: Option<Arc<str>>,
}

impl ChannelBackend {
    pub fn new(tx: mpsc::UnboundedSender<SubagentEvent>) -> Self {
        Self {
            tx,
            parent_session_id: None,
        }
    }

    /// Bind model-facing operations to one parent session.
    pub fn for_session(
        tx: mpsc::UnboundedSender<SubagentEvent>,
        parent_session_id: impl Into<Arc<str>>,
    ) -> Self {
        Self {
            tx,
            parent_session_id: Some(parent_session_id.into()),
        }
    }

    fn parent_session_id(&self) -> Option<String> {
        self.parent_session_id.as_deref().map(str::to_owned)
    }

    pub fn sender(&self) -> mpsc::UnboundedSender<SubagentEvent> {
        self.tx.clone()
    }

    pub fn into_resource(self) -> SubagentBackendResource {
        SubagentBackendResource(Arc::new(self))
    }

    pub async fn cancel_parent_prompt(&self, parent_prompt_id: &str) -> SubagentCancelOutcome {
        let (respond_to, response_rx) = oneshot::channel();
        if self
            .tx
            .send(SubagentEvent::Cancel(SubagentCancelRequest {
                parent_session_id: self.parent_session_id(),
                target: SubagentCancelTarget::ParentPromptId(parent_prompt_id.to_owned()),
                respond_to,
            }))
            .is_err()
        {
            return SubagentCancelOutcome::NotFound;
        }
        response_rx.await.unwrap_or(SubagentCancelOutcome::NotFound)
    }

    /// User Stop: cancel all non-workflow children for this parent session.
    ///
    /// Requires [`Self::for_session`]; unbound backends return `NotFound` and
    /// do not broadcast a wildcard cancel.
    pub async fn cancel_parent_session(&self) -> SubagentCancelOutcome {
        let (respond_to, response_rx) = oneshot::channel();
        if !self.request_cancel_parent_session(respond_to) {
            return SubagentCancelOutcome::NotFound;
        }
        response_rx.await.unwrap_or(SubagentCancelOutcome::NotFound)
    }

    /// Fire-and-forget ParentSession cancel used by the shell Stop path.
    /// Returns false when the backend is unbound or the channel is closed.
    pub fn request_cancel_parent_session(
        &self,
        respond_to: oneshot::Sender<SubagentCancelOutcome>,
    ) -> bool {
        let Some(parent_session_id) = self.parent_session_id() else {
            return false;
        };
        self.tx
            .send(SubagentEvent::Cancel(SubagentCancelRequest {
                parent_session_id: Some(parent_session_id),
                target: SubagentCancelTarget::ParentSession,
                respond_to,
            }))
            .is_ok()
    }

    /// Re-open Task spawns after a prior ParentSession stop (start of next turn).
    pub fn open_spawn_admission(&self) -> bool {
        let Some(parent_session_id) = self.parent_session_id() else {
            return false;
        };
        self.tx
            .send(SubagentEvent::OpenSpawnAdmission { parent_session_id })
            .is_ok()
    }

    /// Delete-path teardown: cancel `parent_session_id`'s children and wait, up
    /// to `budget`, for the coordinator to drain them. Owns the event shape and
    /// the wait policy so the host does not rebuild them. Best-effort: on a
    /// closed channel or an elapsed budget it logs and returns (the coordinator
    /// keeps admission closed until its own backstop deadline).
    pub async fn teardown_session_and_drain(
        &self,
        parent_session_id: &str,
        budget: std::time::Duration,
    ) {
        let (respond_to, response_rx) = oneshot::channel();
        if self
            .tx
            .send(SubagentEvent::TeardownSession {
                parent_session_id: parent_session_id.to_owned(),
                respond_to: Some(respond_to),
            })
            .is_err()
        {
            return;
        }
        // Err = budget elapsed with children still finishing; Ok = drained or
        // the responder was dropped.
        if tokio::time::timeout(budget, response_rx).await.is_err() {
            tracing::warn!(
                parent_session_id,
                budget_ms = budget.as_millis() as u64,
                "subagents still running after session-delete drain budget"
            );
        }
    }

    pub async fn inspect(&self, id: &str) -> Option<SubagentInspection> {
        let (respond_to, response_rx) = oneshot::channel();
        self.tx
            .send(SubagentEvent::Inspect(SubagentInspectRequest {
                subagent_id: id.to_owned(),
                parent_session_id: self.parent_session_id(),
                respond_to,
            }))
            .ok()?;
        response_rx.await.ok().flatten()
    }

    pub async fn list_running(&self, parent_session_id: &str) -> Vec<SubagentInspection> {
        let (respond_to, response_rx) = oneshot::channel();
        if self
            .tx
            .send(SubagentEvent::ListRunning(SubagentListRunningRequest {
                parent_session_id: parent_session_id.to_owned(),
                respond_to,
            }))
            .is_err()
        {
            return Vec::new();
        }
        response_rx.await.unwrap_or_default()
    }

    pub async fn spawned_refs_for_prompt(
        &self,
        parent_session_id: &str,
        prompt_id: &str,
    ) -> Vec<SpawnedSubagentRef> {
        let (respond_to, response_rx) = oneshot::channel();
        if self
            .tx
            .send(SubagentEvent::SpawnedRefs(SubagentSpawnedRefsRequest {
                parent_session_id: self
                    .parent_session_id
                    .as_deref()
                    .unwrap_or(parent_session_id)
                    .to_owned(),
                prompt_id: prompt_id.to_owned(),
                respond_to,
            }))
            .is_err()
        {
            return Vec::new();
        }
        response_rx.await.unwrap_or_default()
    }

    pub async fn registry_counts(&self) -> SubagentRegistryCounts {
        let (respond_to, response_rx) = oneshot::channel();
        if self
            .tx
            .send(SubagentEvent::RegistryCounts(
                SubagentRegistryCountsRequest { respond_to },
            ))
            .is_err()
        {
            return SubagentRegistryCounts::default();
        }
        response_rx.await.unwrap_or_default()
    }

    /// Spawn while holding the host's interruptible foreground-wait token.
    pub async fn spawn_with_foreground_wait(
        &self,
        request: SubagentRequest,
        wait: Option<&super::types::SubagentForegroundWait>,
    ) -> Result<SubagentResult, ToolError> {
        let _wait = wait.map(super::types::SubagentForegroundWait::enter);
        self.spawn(request).await
    }
}

struct CancelResultReceiverOnDrop {
    cancel_token: tokio_util::sync::CancellationToken,
    armed: bool,
}

impl Drop for CancelResultReceiverOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.cancel_token.cancel();
        }
    }
}

#[async_trait::async_trait]
impl SubagentBackend for ChannelBackend {
    async fn spawn(&self, mut request: SubagentRequest) -> Result<SubagentResult, ToolError> {
        if let Some(parent_session_id) = self.parent_session_id.as_deref() {
            request.parent_session_id = parent_session_id.to_owned();
        }
        let (respond_to, response_rx) = oneshot::channel();
        let cancel_on_receiver_drop = request.owner.is_workflow();
        let cancel_token = request.cancel_token.clone();
        self.tx
            .send(SubagentEvent::Spawn(SubagentSpawnRequest {
                request: Box::new(request),
                result_tx: respond_to,
            }))
            .map_err(|_| {
                ToolError::custom(
                    "channel_closed",
                    "Subagent coordinator channel closed — cannot spawn subagent",
                )
            })?;

        let mut receiver_guard = cancel_on_receiver_drop.then(|| CancelResultReceiverOnDrop {
            cancel_token: cancel_token.clone(),
            armed: true,
        });
        let result = response_rx.await;
        if result.is_ok() {
            if let Some(guard) = receiver_guard.as_mut() {
                guard.armed = false;
            }
        } else if cancel_on_receiver_drop {
            cancel_token.cancel();
        }
        result.map_err(|_| {
            ToolError::custom(
                "channel_closed",
                "Subagent result channel dropped — child session may have crashed",
            )
        })
    }

    async fn query(
        &self,
        id: &str,
        block: bool,
        timeout_ms: Option<u64>,
    ) -> Option<SubagentSnapshot> {
        let (respond_to, response_rx) = oneshot::channel();
        let sent = self.tx.send(SubagentEvent::Query(SubagentQueryRequest {
            subagent_id: id.to_string(),
            parent_session_id: self.parent_session_id(),
            block,
            timeout_ms,
            respond_to,
        }));
        if sent.is_err() {
            return None;
        }
        response_rx.await.ok().flatten()
    }

    async fn cancel(&self, id: &str) -> SubagentCancelOutcome {
        let (respond_to, response_rx) = oneshot::channel();
        let sent = self.tx.send(SubagentEvent::Cancel(SubagentCancelRequest {
            parent_session_id: self.parent_session_id(),
            target: SubagentCancelTarget::SubagentId(id.to_string()),
            respond_to,
        }));
        if sent.is_err() {
            return SubagentCancelOutcome::NotFound;
        }
        response_rx.await.unwrap_or(SubagentCancelOutcome::NotFound)
    }

    async fn validate_type(
        &self,
        subagent_type: &str,
        parent_session_id: &str,
    ) -> SubagentValidateTypeOutcome {
        let parent_session_id = self
            .parent_session_id
            .as_deref()
            .unwrap_or(parent_session_id);
        let (respond_to, response_rx) = oneshot::channel();
        if self
            .tx
            .send(SubagentEvent::ValidateType(SubagentValidateTypeRequest {
                subagent_type: subagent_type.to_string(),
                parent_session_id: parent_session_id.to_string(),
                respond_to,
            }))
            .is_err()
        {
            tracing::warn!(
                subagent_type,
                "coordinator validation channel closed, treating as ValidationUnavailable",
            );
            return SubagentValidateTypeOutcome::ValidationUnavailable;
        }
        let timeout = validate_type_timeout();
        match tokio::time::timeout(timeout, response_rx).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => {
                tracing::warn!(
                    subagent_type,
                    "coordinator validation responder dropped, treating as ValidationUnavailable",
                );
                SubagentValidateTypeOutcome::ValidationUnavailable
            }
            Err(_) => {
                tracing::warn!(
                    subagent_type,
                    timeout_ms = timeout.as_millis() as u64,
                    "coordinator validation timed out, treating as ValidationUnavailable",
                );
                SubagentValidateTypeOutcome::ValidationUnavailable
            }
        }
    }

    async fn describe_subagent_type(
        &self,
        subagent_type: &str,
        harness_agent_type: Option<&str>,
        parent_session_id: &str,
    ) -> SubagentDescribeOutcome {
        let parent_session_id = self
            .parent_session_id
            .as_deref()
            .unwrap_or(parent_session_id);
        let (respond_to, response_rx) = oneshot::channel();
        if self
            .tx
            .send(SubagentEvent::DescribeType(SubagentDescribeRequest {
                subagent_type: subagent_type.to_string(),
                harness_agent_type: harness_agent_type.map(str::to_string),
                parent_session_id: parent_session_id.to_string(),
                respond_to,
            }))
            .is_err()
        {
            tracing::warn!(
                subagent_type,
                "coordinator describe channel closed, treating as Unavailable",
            );
            return SubagentDescribeOutcome::Unavailable;
        }
        let timeout = validate_type_timeout();
        match tokio::time::timeout(timeout, response_rx).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => {
                tracing::warn!(
                    subagent_type,
                    "coordinator describe responder dropped, treating as Unavailable",
                );
                SubagentDescribeOutcome::Unavailable
            }
            Err(_) => {
                tracing::warn!(
                    subagent_type,
                    timeout_ms = timeout.as_millis() as u64,
                    "coordinator describe timed out, treating as Unavailable",
                );
                SubagentDescribeOutcome::Unavailable
            }
        }
    }
}

/// Default `validate_type` timeout. Override via [`VALIDATE_TYPE_TIMEOUT_ENV_VAR`].
pub const VALIDATE_TYPE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Env-var override for [`VALIDATE_TYPE_TIMEOUT`] (positive milliseconds).
pub const VALIDATE_TYPE_TIMEOUT_ENV_VAR: &str = "ECHO_AGENT_VALIDATE_TYPE_TIMEOUT_MS";

/// Validation timeout, honoring the env-var override.
pub fn validate_type_timeout() -> std::time::Duration {
    let value = std::env::var(VALIDATE_TYPE_TIMEOUT_ENV_VAR).ok();
    parse_timeout_ms(value.as_deref())
        .map(std::time::Duration::from_millis)
        .unwrap_or(VALIDATE_TYPE_TIMEOUT)
}

/// Parse a positive `u64` millisecond value; `None` for unset, invalid, or zero.
pub(crate) fn parse_timeout_ms(value: Option<&str>) -> Option<u64> {
    value.and_then(crate::util::env::parse_positive)
}

/// Resolve a `Duration` from a positive-millisecond env override, falling back
/// to `default` when the var is unset / non-numeric / zero.
pub fn env_duration_or(env_var: &str, default: std::time::Duration) -> std::time::Duration {
    parse_timeout_ms(std::env::var(env_var).ok().as_deref())
        .map(std::time::Duration::from_millis)
        .unwrap_or(default)
}

#[cfg(test)]
#[path = "backend_tests.rs"]
mod tests;
