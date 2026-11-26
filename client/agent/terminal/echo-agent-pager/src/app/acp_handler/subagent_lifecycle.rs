//! Deduplication for persisted subagent spawn/finish notifications.
//!
//! Lifecycle transitions are unique per child but are not delivery-ordered
//! with other EchoAgent updates, or necessarily with each other. Classification,
//! delivery, deferred-finish buffering, and re-dispatch all live here.

use super::*;
use crate::app::agent_view::DeferredSubagentFinish;
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub(super) const MAX_DEFERRED_SUBAGENT_FINISHES: usize = 256;
pub(super) const DEFERRED_FINISH_TTL: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SubagentLifecycle {
    Spawned,
    Finished,
}

impl SubagentLifecycle {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Spawned => "spawned",
            Self::Finished => "finished",
        }
    }
}

impl std::fmt::Display for SubagentLifecycle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Who asked the lifecycle rail to consider this update.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LifecycleOrigin {
    Stream,
    Reconciliation,
}

pub(super) struct SubagentLifecycleUpdate<'a> {
    pub(super) child_session_id: &'a str,
    pub(super) transition: SubagentLifecycle,
    pub(super) origin: LifecycleOrigin,
}

pub(super) fn classify_subagent_lifecycle(
    update: &EchoAgentSessionUpdate,
    origin: LifecycleOrigin,
) -> Option<SubagentLifecycleUpdate<'_>> {
    match update {
        EchoAgentSessionUpdate::SubagentSpawned {
            child_session_id, ..
        } => Some(SubagentLifecycleUpdate {
            child_session_id,
            transition: SubagentLifecycle::Spawned,
            origin,
        }),
        EchoAgentSessionUpdate::SubagentFinished {
            child_session_id, ..
        } => Some(SubagentLifecycleUpdate {
            child_session_id,
            transition: SubagentLifecycle::Finished,
            origin,
        }),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LifecycleDelivery {
    Apply,
    DropDuplicate,
    AwaitSpawn,
}

pub(super) fn decide_subagent_lifecycle_delivery(
    subagent_sessions: &std::collections::HashMap<String, SubagentInfo>,
    scrollback: &crate::scrollback::state::ScrollbackState,
    child_session_id: &str,
    transition: SubagentLifecycle,
    is_replay: bool,
    origin: LifecycleOrigin,
) -> LifecycleDelivery {
    let Some(info) = subagent_sessions.get(child_session_id) else {
        return match transition {
            SubagentLifecycle::Spawned => LifecycleDelivery::Apply,
            SubagentLifecycle::Finished => LifecycleDelivery::AwaitSpawn,
        };
    };
    // Workflow children deliberately have no per-child parent row. Their
    // retained domain state is therefore the spawn source of truth across
    // replay, while standalone replay may rebuild a row discarded with a
    // failed/superseded reload. Live duplicate spawns must never replace
    // retained child state merely because the row is temporarily absent.
    let child_row_still_rendered = info.workflow_run_id.is_some()
        || info
            .scrollback_entry_id
            .is_some_and(|id| scrollback.get_by_id(id).is_some());

    match (transition, origin) {
        (SubagentLifecycle::Spawned, _) if !is_replay || child_row_still_rendered => {
            LifecycleDelivery::DropDuplicate
        }
        (SubagentLifecycle::Spawned, _) => LifecycleDelivery::Apply,
        (SubagentLifecycle::Finished, LifecycleOrigin::Reconciliation) => LifecycleDelivery::Apply,
        (SubagentLifecycle::Finished, LifecycleOrigin::Stream) if info.finished => {
            LifecycleDelivery::DropDuplicate
        }
        (SubagentLifecycle::Finished, LifecycleOrigin::Stream) => LifecycleDelivery::Apply,
    }
}

/// Classify, decide, and (when needed) buffer a lifecycle update.
///
/// `Apply` means the handler should keep going. The other two outcomes
/// have already been logged and must stop the handler.
pub(super) fn gate_subagent_lifecycle(
    subagent_sessions: &std::collections::HashMap<String, SubagentInfo>,
    scrollback: &crate::scrollback::state::ScrollbackState,
    deferred: &mut HashMap<String, DeferredSubagentFinish>,
    lifecycle: &SubagentLifecycleUpdate<'_>,
    is_replay: bool,
    parent_session_id: &str,
    event_id: Option<&str>,
    notification: &SessionNotification,
    now: Instant,
) -> LifecycleDelivery {
    let delivery = decide_subagent_lifecycle_delivery(
        subagent_sessions,
        scrollback,
        lifecycle.child_session_id,
        lifecycle.transition,
        is_replay,
        lifecycle.origin,
    );
    match delivery {
        LifecycleDelivery::Apply => LifecycleDelivery::Apply,
        LifecycleDelivery::DropDuplicate => {
            tracing::debug!(
                session_id = parent_session_id,
                child_session_id = lifecycle.child_session_id,
                transition = %lifecycle.transition,
                event_id,
                "echo.agent/session lifecycle update DROPPED as a duplicate"
            );
            LifecycleDelivery::DropDuplicate
        }
        LifecycleDelivery::AwaitSpawn => defer_subagent_finish(
            deferred,
            lifecycle.child_session_id,
            notification.clone(),
            parent_session_id,
            event_id,
            now,
        ),
    }
}

pub(super) fn defer_subagent_finish(
    deferred: &mut HashMap<String, DeferredSubagentFinish>,
    child_session_id: &str,
    mut notification: SessionNotification,
    parent_session_id: &str,
    event_id: Option<&str>,
    now: Instant,
) -> LifecycleDelivery {
    prune_deferred_subagent_finishes(deferred, now);
    if !deferred.contains_key(child_session_id) && deferred.len() >= MAX_DEFERRED_SUBAGENT_FINISHES
    {
        evict_oldest_deferred_finish(deferred);
    }
    strip_deferred_finish_output(&mut notification);
    deferred
        .entry(child_session_id.to_owned())
        .or_insert(DeferredSubagentFinish {
            notification,
            inserted_at: now,
        });
    tracing::debug!(
        session_id = parent_session_id,
        child_session_id,
        transition = %SubagentLifecycle::Finished,
        event_id,
        "echo.agent/session lifecycle update DEFERRED until spawn"
    );
    LifecycleDelivery::AwaitSpawn
}

/// Take a deferred finish, enforcing the TTL at observe time so an entry is
/// not applied after expiry merely because nothing else deferred in between.
pub(super) fn take_deferred_subagent_finish(
    deferred: &mut HashMap<String, DeferredSubagentFinish>,
    child_session_id: &str,
    now: Instant,
) -> Option<SessionNotification> {
    let entry = deferred.remove(child_session_id)?;
    if now.saturating_duration_since(entry.inserted_at) >= DEFERRED_FINISH_TTL {
        tracing::debug!(child_session_id, "deferred subagent finish expired on take");
        return None;
    }
    Some(entry.notification)
}

pub(super) fn redispatched_subagent_finish(
    payload: SessionNotification,
) -> Option<acp::ExtNotification> {
    serde_json::value::to_raw_value(&payload)
        .ok()
        .map(|params| acp::ExtNotification::new("echo.agent/session/update", params.into()))
}

fn strip_deferred_finish_output(notification: &mut SessionNotification) {
    if let EchoAgentSessionUpdate::SubagentFinished { output, .. } = &mut notification.update {
        *output = None;
    }
}

fn prune_deferred_subagent_finishes(
    deferred: &mut HashMap<String, DeferredSubagentFinish>,
    now: Instant,
) {
    deferred.retain(|child_session_id, entry| {
        let keep = now.saturating_duration_since(entry.inserted_at) < DEFERRED_FINISH_TTL;
        if !keep {
            tracing::debug!(
                child_session_id = %child_session_id,
                "deferred subagent finish expired"
            );
        }
        keep
    });
}

fn evict_oldest_deferred_finish(deferred: &mut HashMap<String, DeferredSubagentFinish>) {
    let oldest = deferred
        .iter()
        .min_by_key(|(_, entry)| entry.inserted_at)
        .map(|(child, _)| child.clone());
    if let Some(child) = oldest {
        deferred.remove(&child);
        tracing::debug!(
            child_session_id = %child,
            "deferred subagent finish evicted (capacity)"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scrollback::state::ScrollbackState;
    use agent_client_protocol as acp;

    fn finish_notification(child: &str, output: Option<String>) -> SessionNotification {
        SessionNotification {
            session_id: acp::SessionId::new("sess-parent"),
            update: EchoAgentSessionUpdate::SubagentFinished {
                subagent_id: child.into(),
                child_session_id: child.into(),
                status: "completed".into(),
                error: None,
                tool_calls: 0,
                turns: 0,
                duration_ms: 1,
                tokens_used: 0,
                output,
                will_wake: false,
            },
            meta: None,
        }
    }

    #[test]
    fn reconciliation_applies_already_finished_child() {
        let mut sessions = std::collections::HashMap::new();
        sessions.insert(
            "child-1".to_string(),
            crate::app::agent_view::test_fixtures::running_subagent_info("child-1"),
        );
        sessions.get_mut("child-1").unwrap().finished = true;
        let scrollback = ScrollbackState::new();

        assert_eq!(
            decide_subagent_lifecycle_delivery(
                &sessions,
                &scrollback,
                "child-1",
                SubagentLifecycle::Finished,
                false,
                LifecycleOrigin::Stream,
            ),
            LifecycleDelivery::DropDuplicate
        );
        assert_eq!(
            decide_subagent_lifecycle_delivery(
                &sessions,
                &scrollback,
                "child-1",
                SubagentLifecycle::Finished,
                false,
                LifecycleOrigin::Reconciliation,
            ),
            LifecycleDelivery::Apply
        );
    }

    #[test]
    fn deferred_finish_strips_output_and_evicts_oldest() {
        let mut deferred = HashMap::new();
        let t0 = Instant::now();
        for i in 0..MAX_DEFERRED_SUBAGENT_FINISHES {
            let child = format!("child-{i}");
            defer_subagent_finish(
                &mut deferred,
                &child,
                finish_notification(&child, Some("keep-out".into())),
                "sess-parent",
                None,
                t0 + Duration::from_millis(i as u64),
            );
        }
        assert_eq!(deferred.len(), MAX_DEFERRED_SUBAGENT_FINISHES);
        assert!(
            deferred
                .values()
                .all(|entry| match &entry.notification.update {
                    EchoAgentSessionUpdate::SubagentFinished { output, .. } => output.is_none(),
                    _ => false,
                })
        );

        defer_subagent_finish(
            &mut deferred,
            "child-newest",
            finish_notification("child-newest", Some("drop-me".into())),
            "sess-parent",
            None,
            t0 + Duration::from_secs(1),
        );
        assert_eq!(deferred.len(), MAX_DEFERRED_SUBAGENT_FINISHES);
        assert!(!deferred.contains_key("child-0"));
        assert!(deferred.contains_key("child-newest"));
        assert!(deferred.contains_key(&format!("child-{}", MAX_DEFERRED_SUBAGENT_FINISHES - 1)));
    }

    #[test]
    fn deferred_finish_expires_stale_entries() {
        let mut deferred = HashMap::new();
        let t0 = Instant::now();
        defer_subagent_finish(
            &mut deferred,
            "child-stale",
            finish_notification("child-stale", Some("old".into())),
            "sess-parent",
            None,
            t0,
        );
        defer_subagent_finish(
            &mut deferred,
            "child-fresh",
            finish_notification("child-fresh", None),
            "sess-parent",
            None,
            t0 + DEFERRED_FINISH_TTL + Duration::from_secs(1),
        );
        assert!(!deferred.contains_key("child-stale"));
        assert!(deferred.contains_key("child-fresh"));
    }

    #[test]
    fn take_deferred_finish_enforces_ttl() {
        let mut deferred = HashMap::new();
        let t0 = Instant::now();
        defer_subagent_finish(
            &mut deferred,
            "child-stale",
            finish_notification("child-stale", None),
            "sess-parent",
            None,
            t0,
        );
        assert!(
            take_deferred_subagent_finish(
                &mut deferred,
                "child-stale",
                t0 + DEFERRED_FINISH_TTL + Duration::from_secs(1),
            )
            .is_none(),
            "expired deferred finish must not apply on take"
        );
        assert!(
            !deferred.contains_key("child-stale"),
            "expired entry must be removed from the map on take"
        );

        defer_subagent_finish(
            &mut deferred,
            "child-fresh",
            finish_notification("child-fresh", None),
            "sess-parent",
            None,
            t0,
        );
        assert!(
            take_deferred_subagent_finish(
                &mut deferred,
                "child-fresh",
                t0 + Duration::from_secs(1)
            )
            .is_some(),
            "fresh deferred finish must still apply"
        );
        assert!(deferred.is_empty());
    }
}
