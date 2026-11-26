//! Per-session pending-interaction registry.
//!
//! Permissions, `ask_user_question`, and plan approval are **blocking ACP
//! reverse-requests**: the agent parks a tool-loop future on an in-memory
//! oneshot and waits for the driver to answer. While such a request is open we
//! record it here, keyed by `tool_call_id` (stable, lives in the transcript →
//! survives reconnect). This registry is the single source of truth for "what
//! is pending right now" and is read by the roster to surface
//! [`crate::agent::roster::RosterActivity::NeedsInput`].
//!
//! Pending interactions are **requests, not notifications** — they are never
//! persisted. We broadcast `pending_interaction` / `interaction_resolved`
//! **fire-and-forget** via the gateway (same idiom as
//! [`crate::session::summary`]); the routing layer fans them to every
//! subscriber because they carry a `sessionId`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use agent_client_protocol as acp;
use echo_agent_acp::AcpAgentGatewaySender as GatewaySender;

use crate::extensions::notification::{
    SessionNotification, SessionUpdate as EchoAgentSessionUpdate,
};

/// Shared per-session map of open reverse-requests, keyed by `tool_call_id`.
///
/// Mirrors the `current_prompt_id` signal on
/// [`crate::session::handle::SessionHandle`]: the same `Arc` is shared between
/// the session actor (which mutates it) and the handle (which the roster reads
/// synchronously).
pub(crate) type PendingInteractions = Arc<Mutex<HashMap<String, PendingKind>>>;

/// Which kind of blocking reverse-request is pending.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingKind {
    /// `request_permission` for a tool action.
    Permission,
    /// `echo.agent/ask_user_question`.
    Question,
    /// `echo.agent/exit_plan_mode` plan approval.
    PlanApproval,
    McpElicitation,
}

/// Whether a blocking plan-approval reverse-request is parked in `pending`.
///
/// The resume re-park issues `echo.agent/exit_plan_mode` from a detached task
/// with no running turn, making it the one parked interaction that also carries a
/// persisted gate (`awaiting_plan_approval`). `session_has_live_work` consults
/// this to keep such a session resident until the decision is answered or a real
/// disconnect `Err`s the reverse-request — otherwise an idle-unload drops the
/// parked future and its guard clears the on-disk gate. Permission/question parks
/// carry no persisted gate, so they are intentionally not counted here. Poisoned
/// lock → recover the map (module idiom) and read it.
pub(crate) fn has_parked_plan_approval(pending: &PendingInteractions) -> bool {
    pending
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .any(|k| *k == PendingKind::PlanApproval)
}

/// Fire-and-forget broadcast of a session notification carrying a `sessionId`
/// (so the routing layer fans it out to every subscriber). Never persisted.
fn broadcast(gateway: &GatewaySender, session_id: &acp::SessionId, update: EchoAgentSessionUpdate) {
    let notification = SessionNotification {
        session_id: session_id.clone(),
        update,
        meta: None,
    };
    if let Ok(params) = serde_json::value::to_raw_value(&notification) {
        gateway.forward_fire_and_forget(acp::ExtNotification::new(
            "echo.agent/session_notification",
            params.into(),
        ));
    }
}

/// RAII guard registering an open reverse-request for the lifetime of the
/// parked oneshot.
///
/// On construction it inserts `(tool_call_id, kind)` into the registry and
/// broadcasts `pending_interaction`. On drop — which happens whether the await
/// returns normally, is cancelled, or errors — it removes the entry and (if it
/// actually removed one) broadcasts `interaction_resolved`. The
/// remove-or-no-op makes resolution **idempotent / first-answer-wins**: a
/// second drop / already-removed key is silent.
pub(crate) struct PendingInteractionGuard {
    pending: PendingInteractions,
    gateway: GatewaySender,
    session_id: acp::SessionId,
    tool_call_id: String,
}

impl PendingInteractionGuard {
    /// Register a pending interaction and broadcast `pending_interaction`.
    pub(crate) fn new(
        pending: PendingInteractions,
        gateway: GatewaySender,
        session_id: acp::SessionId,
        tool_call_id: String,
        kind: PendingKind,
    ) -> Self {
        {
            let mut map = pending.lock().unwrap_or_else(|e| e.into_inner());
            map.insert(tool_call_id.clone(), kind);
        }
        broadcast(
            &gateway,
            &session_id,
            EchoAgentSessionUpdate::PendingInteraction {
                tool_call_id: tool_call_id.clone(),
                kind,
            },
        );
        Self {
            pending,
            gateway,
            session_id,
            tool_call_id,
        }
    }
}

impl Drop for PendingInteractionGuard {
    fn drop(&mut self) {
        let removed = {
            let mut map = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            map.remove(&self.tool_call_id).is_some()
        };
        // First-answer-wins: only announce resolution if this guard actually
        // owned the live entry. An already-resolved id is a silent no-op.
        if removed {
            broadcast(
                &self.gateway,
                &self.session_id,
                EchoAgentSessionUpdate::InteractionResolved {
                    tool_call_id: self.tool_call_id.clone(),
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_registry() -> PendingInteractions {
        Arc::new(Mutex::new(HashMap::new()))
    }

    #[test]
    fn guard_inserts_then_removes() {
        let reg = new_registry();
        // No gateway round-trip is exercised here (broadcast is best-effort and
        // a dead sender simply drops). We only assert the registry mutation.
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let gateway = GatewaySender::new(tx);
        {
            let _g = PendingInteractionGuard::new(
                reg.clone(),
                gateway,
                acp::SessionId::new("sess-1"),
                "call-1".to_string(),
                PendingKind::Permission,
            );
            assert_eq!(reg.lock().unwrap().len(), 1);
            assert_eq!(
                reg.lock().unwrap().get("call-1").copied(),
                Some(PendingKind::Permission)
            );
        }
        assert!(reg.lock().unwrap().is_empty());
    }

    /// `has_parked_plan_approval` counts ONLY a parked plan-approval; other
    /// kinds (permission / question) carry no persisted gate and must not, by
    /// themselves, report the session live.
    #[test]
    fn has_parked_plan_approval_only_counts_plan_approval() {
        let reg = new_registry();
        assert!(!has_parked_plan_approval(&reg));

        reg.lock()
            .unwrap()
            .insert("perm".to_string(), PendingKind::Permission);
        reg.lock()
            .unwrap()
            .insert("q".to_string(), PendingKind::Question);
        assert!(
            !has_parked_plan_approval(&reg),
            "permission/question parks must not count as a parked approval"
        );

        reg.lock()
            .unwrap()
            .insert("plan".to_string(), PendingKind::PlanApproval);
        assert!(has_parked_plan_approval(&reg));

        reg.lock().unwrap().remove("plan");
        assert!(!has_parked_plan_approval(&reg));
    }

    /// A poisoned registry lock must not panic the predicate: it recovers the
    /// inner map (module idiom) and reports the parked approval truthfully.
    #[test]
    fn has_parked_plan_approval_recovers_poisoned_lock() {
        let reg = new_registry();
        reg.lock()
            .unwrap()
            .insert("plan".to_string(), PendingKind::PlanApproval);

        let reg_poison = reg.clone();
        let _ = std::thread::spawn(move || {
            let _g = reg_poison.lock().unwrap();
            panic!("poison pending_interactions");
        })
        .join();
        assert!(
            reg.lock().is_err(),
            "precondition: the lock must be poisoned"
        );

        assert!(
            has_parked_plan_approval(&reg),
            "a poisoned lock must still surface the parked approval, not panic"
        );
    }
}
