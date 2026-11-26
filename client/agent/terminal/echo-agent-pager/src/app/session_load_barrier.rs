//! Defers `SessionLoaded` / `SessionLoadFailed` until ACP replay already in
//! the pager queue has been applied — without blocking unrelated JoinSet tasks.
//!
//! This-session [`AcpLoadBacklog::LiveHead`] releases immediately: it means
//! unicast replay has finished. Leader mode buffers live notifications for the
//! loading client until after the `session/load` RESPONSE (`load_live_buffer` in
//! `echo-agent-runtime` leader), so another client's live event cannot land on this
//! pager's socket mid-replay even though agent-side replay now awaits between
//! lines. The client wire order is `[unicast replay] → [load response] →
//! [buffered live]`. Overflow of that buffer forwards live mid-replay and is
//! already warned at the leader; it is not the common path. Direct-spawn has no
//! second client on the same socket.
//!
//! [`AcpLoadBacklog::Unrelated`] still times out after
//! [`SESSION_LOADED_ACP_BARRIER`] of draining: a shared `acp_rx` can show
//! another session's traffic forever, and waiting for `Empty` would stall
//! resume. Remaining this-session `isReplay` behind that head is still applied
//! after dispatch via the post-load late-replay grace on
//! `drop_unexpected_replay`.

use std::time::{Duration, Instant};

use agent_client_protocol as acp;
use echo_agent_acp::AcpClientMessage;
use serde::Deserialize;

use super::actions::TaskResult;
use super::agent::AgentId;
use crate::acp::meta::NotificationMeta;

/// Firehose escape when the ACP head is unrelated to this load.
/// Does not accrue on this-session `ReplayHead` or while input-starved.
pub(super) const SESSION_LOADED_ACP_BARRIER: Duration = Duration::from_secs(2);

/// Head of the pager ACP queue, classified against one deferred load's session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AcpLoadBacklog {
    Empty,
    ReplayHead,
    LiveHead,
    Unrelated,
}

/// Whether the event loop's ACP recv arm can run this iteration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AcpDrainArm {
    InputStarved,
    CanDrain,
}

impl AcpDrainArm {
    pub(super) fn from_input_rx_empty(input_rx_empty: bool) -> Self {
        if input_rx_empty {
            Self::CanDrain
        } else {
            Self::InputStarved
        }
    }
}

/// One barrier observation: ACP peek + whether the ACP arm can drain.
#[derive(Clone, Copy, Debug)]
pub(super) struct SessionLoadAcpTick<'a> {
    pub head: Option<&'a AcpClientMessage>,
    pub drain_arm: AcpDrainArm,
    pub now: Instant,
}

pub(super) fn session_load_agent_id(result: &TaskResult) -> Option<AgentId> {
    match result {
        TaskResult::SessionLoaded { agent_id, .. }
        | TaskResult::SessionLoadFailed { agent_id, .. } => Some(*agent_id),
        _ => None,
    }
}

fn session_load_session_id(result: &TaskResult) -> Option<&acp::SessionId> {
    match result {
        TaskResult::SessionLoaded { session_id, .. }
        | TaskResult::SessionLoadFailed { session_id, .. } => Some(session_id),
        _ => None,
    }
}

/// Classify the ACP lookahead slot (filled by the event loop via `try_recv`).
pub(super) fn acp_load_backlog(
    head: Option<&AcpClientMessage>,
    session_id: &acp::SessionId,
) -> AcpLoadBacklog {
    let Some(msg) = head else {
        return AcpLoadBacklog::Empty;
    };
    match msg {
        AcpClientMessage::SessionNotification(n) => {
            classify_session_meta(&n.request.session_id, n.request.meta.as_ref(), session_id)
        }
        AcpClientMessage::ExtNotification(n) => classify_ext_notification(&n.request, session_id),
        _ => AcpLoadBacklog::Unrelated,
    }
}

fn classify_session_meta(
    msg_session_id: &acp::SessionId,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
    load_session_id: &acp::SessionId,
) -> AcpLoadBacklog {
    if msg_session_id != load_session_id {
        return AcpLoadBacklog::Unrelated;
    }
    if NotificationMeta::from_json(meta).is_replay {
        AcpLoadBacklog::ReplayHead
    } else {
        AcpLoadBacklog::LiveHead
    }
}

fn classify_ext_notification(
    notif: &acp::ExtNotification,
    load_session_id: &acp::SessionId,
) -> AcpLoadBacklog {
    if !crate::acp::is_session_update_ext_method(notif.method.as_ref()) {
        return AcpLoadBacklog::Unrelated;
    }
    #[derive(Deserialize)]
    struct MetaPeek {
        #[serde(default, rename = "isReplay")]
        is_replay: bool,
    }
    #[derive(Deserialize)]
    struct ParamsPeek {
        #[serde(rename = "sessionId")]
        session_id: Option<String>,
        #[serde(default, rename = "_meta")]
        meta: Option<MetaPeek>,
    }
    let Ok(params) = serde_json::from_str::<ParamsPeek>(notif.params.get()) else {
        return AcpLoadBacklog::Unrelated;
    };
    let Some(sid) = params.session_id.as_deref() else {
        return AcpLoadBacklog::Unrelated;
    };
    if sid != load_session_id.0.as_ref() {
        return AcpLoadBacklog::Unrelated;
    }
    if params.meta.is_some_and(|m| m.is_replay) {
        AcpLoadBacklog::ReplayHead
    } else {
        AcpLoadBacklog::LiveHead
    }
}

fn backlog_for_result(result: &TaskResult, acp_head: Option<&AcpClientMessage>) -> AcpLoadBacklog {
    match session_load_session_id(result) {
        Some(sid) => acp_load_backlog(acp_head, sid),
        None => AcpLoadBacklog::Empty,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SessionLoadDeferState {
    pub backlog: AcpLoadBacklog,
    pub drain_arm: AcpDrainArm,
    pub unrelated_drain_elapsed: Duration,
}

/// Whether this JoinSet result should wait for ACP already queued for this
/// agent's in-flight `session/load` replay.
pub(super) fn should_defer_session_load(
    result: &TaskResult,
    agent_loading_replay: bool,
    state: SessionLoadDeferState,
) -> bool {
    if session_load_agent_id(result).is_none() {
        return false;
    }
    if !agent_loading_replay {
        return false;
    }
    match state.backlog {
        AcpLoadBacklog::Empty | AcpLoadBacklog::LiveHead => false,
        AcpLoadBacklog::ReplayHead => true,
        AcpLoadBacklog::Unrelated => match state.drain_arm {
            AcpDrainArm::InputStarved => true,
            AcpDrainArm::CanDrain => state.unrelated_drain_elapsed < SESSION_LOADED_ACP_BARRIER,
        },
    }
}

struct DeferredLoad {
    result: TaskResult,
    unrelated_drain_elapsed: Duration,
    last_unrelated_drain_sample: Option<Instant>,
}

impl DeferredLoad {
    fn observe(&mut self, backlog: AcpLoadBacklog, drain_arm: AcpDrainArm, now: Instant) {
        match backlog {
            AcpLoadBacklog::ReplayHead => {
                self.unrelated_drain_elapsed = Duration::ZERO;
                self.last_unrelated_drain_sample = None;
            }
            AcpLoadBacklog::Unrelated => match drain_arm {
                AcpDrainArm::CanDrain => {
                    if let Some(last) = self.last_unrelated_drain_sample {
                        self.unrelated_drain_elapsed += now.saturating_duration_since(last);
                    }
                    self.last_unrelated_drain_sample = Some(now);
                }
                AcpDrainArm::InputStarved => {
                    self.last_unrelated_drain_sample = None;
                }
            },
            AcpLoadBacklog::Empty | AcpLoadBacklog::LiveHead => {
                self.last_unrelated_drain_sample = None;
            }
        }
    }

    fn defer_state(
        &self,
        backlog: AcpLoadBacklog,
        drain_arm: AcpDrainArm,
    ) -> SessionLoadDeferState {
        SessionLoadDeferState {
            backlog,
            drain_arm,
            unrelated_drain_elapsed: self.unrelated_drain_elapsed,
        }
    }
}

#[derive(Default)]
pub(super) struct SessionLoadBarrier {
    deferred: Vec<DeferredLoad>,
}

impl SessionLoadBarrier {
    pub(super) fn new() -> Self {
        Self {
            deferred: Vec::new(),
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.deferred.is_empty()
    }

    pub(super) fn push_or_dispatch(
        &mut self,
        result: TaskResult,
        agent_loading_replay: bool,
        tick: SessionLoadAcpTick<'_>,
    ) -> Option<TaskResult> {
        let backlog = backlog_for_result(&result, tick.head);
        let mut entry = DeferredLoad {
            result,
            unrelated_drain_elapsed: Duration::ZERO,
            last_unrelated_drain_sample: None,
        };
        entry.observe(backlog, tick.drain_arm, tick.now);
        if should_defer_session_load(
            &entry.result,
            agent_loading_replay,
            entry.defer_state(backlog, tick.drain_arm),
        ) {
            self.deferred.push(entry);
            None
        } else {
            Some(entry.result)
        }
    }

    pub(super) fn next_wakeup(&self) -> Option<Instant> {
        self.deferred
            .iter()
            .filter_map(|d| {
                let last = d.last_unrelated_drain_sample?;
                Some(last + SESSION_LOADED_ACP_BARRIER.saturating_sub(d.unrelated_drain_elapsed))
            })
            .min()
    }

    pub(super) fn take_ready(
        &mut self,
        mut agent_loading_replay: impl FnMut(AgentId) -> bool,
        tick: SessionLoadAcpTick<'_>,
    ) -> Vec<TaskResult> {
        let pending = std::mem::take(&mut self.deferred);
        let mut ready = Vec::new();
        for mut entry in pending {
            let backlog = backlog_for_result(&entry.result, tick.head);
            entry.observe(backlog, tick.drain_arm, tick.now);
            let loading =
                session_load_agent_id(&entry.result).is_some_and(&mut agent_loading_replay);
            if should_defer_session_load(
                &entry.result,
                loading,
                entry.defer_state(backlog, tick.drain_arm),
            ) {
                self.deferred.push(entry);
            } else {
                ready.push(entry.result);
            }
        }
        ready
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use agent_client_protocol as acp;
    use serde_json::json;

    use super::*;
    use crate::app::agent::AgentId;

    fn sid(s: &str) -> acp::SessionId {
        acp::SessionId::new(s)
    }

    fn loaded(id: usize, session: &str) -> TaskResult {
        TaskResult::SessionLoaded {
            agent_id: AgentId(id),
            session_id: sid(session),
            models: None,
            code_restored: false,
            restore_summary: None,
            restore_degree: None,
            running_prompt_id: None,
            scheduler_background_loops: None,
        }
    }

    fn load_failed(id: usize, session: &str) -> TaskResult {
        TaskResult::SessionLoadFailed {
            agent_id: AgentId(id),
            session_id: sid(session),
            error: "x".into(),
        }
    }

    fn other_task() -> TaskResult {
        TaskResult::WorktreeSessionFailed {
            agent_id: AgentId(9),
            error: "nope".into(),
        }
    }

    fn session_notif(session: &str, is_replay: bool) -> AcpClientMessage {
        let mut meta = serde_json::Map::new();
        meta.insert("isReplay".into(), json!(is_replay));
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let request = acp::SessionNotification::new(
            sid(session),
            acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
                acp::TextContent::new("hi"),
            ))),
        )
        .meta(Some(meta));
        AcpClientMessage::SessionNotification(echo_agent_acp::AcpArgs {
            request,
            response_tx: tx,
        })
    }

    fn ext_session_update(session: &str, is_replay: bool) -> AcpClientMessage {
        ext_session_update_raw(json!({
            "sessionId": session,
            "update": { "sessionUpdate": "agent_message_chunk" },
            "_meta": { "isReplay": is_replay },
        }))
    }

    fn ext_session_update_raw(params: serde_json::Value) -> AcpClientMessage {
        ext_notification("echo.agent/session/update", params)
    }

    fn ext_notification(method: &str, params: serde_json::Value) -> AcpClientMessage {
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let raw = serde_json::value::to_raw_value(&params).expect("raw params");
        AcpClientMessage::ExtNotification(echo_agent_acp::AcpArgs {
            request: acp::ExtNotification::new(method, raw.into()),
            response_tx: tx,
        })
    }

    fn request_permission(session: &str) -> AcpClientMessage {
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let request = acp::RequestPermissionRequest::new(
            sid(session),
            acp::ToolCallUpdate::new(
                acp::ToolCallId::new(Arc::from("call-perm-1")),
                acp::ToolCallUpdateFields::default(),
            ),
            vec![acp::PermissionOption::new(
                acp::PermissionOptionId::new(Arc::from("allow-once")),
                "Allow once",
                acp::PermissionOptionKind::AllowOnce,
            )],
        );
        AcpClientMessage::RequestPermission(echo_agent_acp::AcpArgs {
            request,
            response_tx: tx,
        })
    }

    fn tick<'a>(
        head: Option<&'a AcpClientMessage>,
        drain_arm: AcpDrainArm,
        now: Instant,
    ) -> SessionLoadAcpTick<'a> {
        SessionLoadAcpTick {
            head,
            drain_arm,
            now,
        }
    }

    fn draining<'a>(head: Option<&'a AcpClientMessage>, now: Instant) -> SessionLoadAcpTick<'a> {
        tick(head, AcpDrainArm::CanDrain, now)
    }

    fn defer_state(
        backlog: AcpLoadBacklog,
        drain_arm: AcpDrainArm,
        unrelated_drain_elapsed: Duration,
    ) -> SessionLoadDeferState {
        SessionLoadDeferState {
            backlog,
            drain_arm,
            unrelated_drain_elapsed,
        }
    }

    #[test]
    fn predicate_table() {
        let r = loaded(0, "s");
        assert!(
            !should_defer_session_load(
                &r,
                true,
                defer_state(AcpLoadBacklog::Empty, AcpDrainArm::CanDrain, Duration::ZERO)
            ),
            "loading + empty ACP → dispatch"
        );
        assert!(
            !should_defer_session_load(
                &r,
                true,
                defer_state(
                    AcpLoadBacklog::LiveHead,
                    AcpDrainArm::CanDrain,
                    Duration::ZERO
                )
            ),
            "loading + this-session live head → dispatch"
        );
        assert!(
            should_defer_session_load(
                &r,
                true,
                defer_state(
                    AcpLoadBacklog::ReplayHead,
                    AcpDrainArm::CanDrain,
                    SESSION_LOADED_ACP_BARRIER
                )
            ),
            "ReplayHead never times out"
        );
        assert!(
            should_defer_session_load(
                &r,
                true,
                defer_state(
                    AcpLoadBacklog::Unrelated,
                    AcpDrainArm::CanDrain,
                    Duration::ZERO
                )
            ),
            "Unrelated always defers until timeout / Empty / LiveHead"
        );
        assert!(
            should_defer_session_load(
                &r,
                true,
                defer_state(
                    AcpLoadBacklog::Unrelated,
                    AcpDrainArm::InputStarved,
                    SESSION_LOADED_ACP_BARRIER
                )
            ),
            "input-starved Unrelated must not honor elapsed"
        );
        assert!(
            !should_defer_session_load(
                &r,
                true,
                defer_state(
                    AcpLoadBacklog::Unrelated,
                    AcpDrainArm::CanDrain,
                    SESSION_LOADED_ACP_BARRIER
                )
            ),
            "Unrelated + draining firehose timeout → dispatch"
        );
        assert!(
            !should_defer_session_load(
                &r,
                false,
                defer_state(
                    AcpLoadBacklog::ReplayHead,
                    AcpDrainArm::CanDrain,
                    Duration::ZERO
                )
            ),
            "not loading + nonempty ACP → dispatch"
        );
        assert!(!should_defer_session_load(
            &other_task(),
            true,
            defer_state(
                AcpLoadBacklog::ReplayHead,
                AcpDrainArm::CanDrain,
                Duration::ZERO
            )
        ));
        assert!(should_defer_session_load(
            &load_failed(0, "s"),
            true,
            defer_state(
                AcpLoadBacklog::ReplayHead,
                AcpDrainArm::CanDrain,
                Duration::ZERO
            )
        ));
    }

    #[test]
    fn session_loaded_waits_behind_queued_replay() {
        let now = Instant::now();
        let mut barrier = SessionLoadBarrier::new();
        let replay = session_notif("s", true);
        assert!(
            barrier
                .push_or_dispatch(loaded(1, "s"), true, draining(Some(&replay), now))
                .is_none()
        );
        assert!(
            barrier
                .take_ready(|_| true, draining(Some(&replay), now))
                .is_empty(),
            "still waiting while this agent's replay ACP is queued"
        );
        let ready = barrier.take_ready(|_| true, draining(None, now));
        assert_eq!(ready.len(), 1);
    }

    #[test]
    fn this_session_live_head_releases_session_loaded() {
        // LiveHead means unicast replay is done (leader held live until after
        // the load response); remaining this-session live must not block.
        let now = Instant::now();
        let mut barrier = SessionLoadBarrier::new();
        let replay = session_notif("s", true);
        assert!(
            barrier
                .push_or_dispatch(loaded(1, "s"), true, draining(Some(&replay), now))
                .is_none()
        );
        let live = session_notif("s", false);
        let ready = barrier.take_ready(|_| true, draining(Some(&live), now));
        assert_eq!(ready.len(), 1);
    }

    #[test]
    fn other_session_live_head_does_not_release_before_this_replay() {
        let now = Instant::now();
        let mut barrier = SessionLoadBarrier::new();
        let other_live = session_notif("other", false);
        assert!(
            barrier
                .push_or_dispatch(loaded(1, "s"), true, draining(Some(&other_live), now))
                .is_none(),
            "agent B live at head must not release agent A's SessionLoaded yet"
        );
        assert!(
            barrier
                .take_ready(|_| true, draining(Some(&other_live), now))
                .is_empty()
        );
        let a_replay = session_notif("s", true);
        assert!(
            barrier
                .take_ready(|_| true, draining(Some(&a_replay), now))
                .is_empty(),
            "A replay behind a foreign live head still defers once it reaches the peek"
        );
    }

    #[test]
    fn foreign_live_after_this_session_replay_still_defers() {
        let now = Instant::now();
        let mut barrier = SessionLoadBarrier::new();
        let replay = session_notif("s", true);
        assert!(
            barrier
                .push_or_dispatch(loaded(1, "s"), true, draining(Some(&replay), now))
                .is_none()
        );
        assert!(
            barrier
                .take_ready(|_| true, draining(Some(&replay), now))
                .is_empty()
        );
        let foreign = session_notif("other", false);
        assert!(
            barrier
                .take_ready(|_| true, draining(Some(&foreign), now))
                .is_empty(),
            "A-replay then B-live must still defer; remaining A replay may sit behind B"
        );
    }

    #[test]
    fn replay_head_resets_unrelated_drain_clock() {
        let start = Instant::now();
        let mut barrier = SessionLoadBarrier::new();
        let foreign = session_notif("other", false);
        assert!(
            barrier
                .push_or_dispatch(loaded(1, "s"), true, draining(Some(&foreign), start))
                .is_none()
        );
        let almost = start + SESSION_LOADED_ACP_BARRIER - Duration::from_millis(1);
        assert!(
            barrier
                .take_ready(|_| true, draining(Some(&foreign), almost))
                .is_empty()
        );
        let replay = session_notif("s", true);
        let t_replay = almost + Duration::from_millis(1);
        assert!(
            barrier
                .take_ready(|_| true, draining(Some(&replay), t_replay))
                .is_empty()
        );
        assert!(
            barrier.next_wakeup().is_none(),
            "ReplayHead clears the Unrelated timer"
        );
        assert!(
            barrier
                .take_ready(|_| true, draining(Some(&foreign), t_replay))
                .is_empty(),
            "Unrelated clock restarts after ReplayHead; must not inherit prior elapsed"
        );
        assert!(
            barrier
                .take_ready(
                    |_| true,
                    draining(
                        Some(&foreign),
                        t_replay + SESSION_LOADED_ACP_BARRIER - Duration::from_millis(1)
                    )
                )
                .is_empty()
        );
        let ready = barrier.take_ready(
            |_| true,
            draining(
                Some(&foreign),
                t_replay + SESSION_LOADED_ACP_BARRIER + Duration::from_millis(1),
            ),
        );
        assert_eq!(ready.len(), 1);
    }

    #[test]
    fn request_permission_head_does_not_release_immediately() {
        let now = Instant::now();
        let mut barrier = SessionLoadBarrier::new();
        let perm = request_permission("s");
        assert!(
            barrier
                .push_or_dispatch(loaded(1, "s"), true, draining(Some(&perm), now))
                .is_none()
        );
        assert!(
            barrier
                .take_ready(|_| true, draining(Some(&perm), now))
                .is_empty()
        );
    }

    #[test]
    fn other_agent_loading_does_not_block_this_session_loaded() {
        let now = Instant::now();
        let mut barrier = SessionLoadBarrier::new();
        let replay = session_notif("s", true);
        let dispatched =
            barrier.push_or_dispatch(loaded(2, "s"), false, draining(Some(&replay), now));
        assert!(dispatched.is_some());
    }

    #[test]
    fn replay_head_stays_deferred_past_two_second_drain() {
        let start = Instant::now();
        let mut barrier = SessionLoadBarrier::new();
        let replay = session_notif("s", true);
        assert!(
            barrier
                .push_or_dispatch(loaded(1, "s"), true, draining(Some(&replay), start))
                .is_none()
        );
        let later = start + SESSION_LOADED_ACP_BARRIER + Duration::from_secs(1);
        assert!(
            barrier
                .take_ready(|_| true, draining(Some(&replay), later))
                .is_empty(),
            "ReplayHead + CanDrain must not fire a timeout"
        );
        assert!(
            barrier.next_wakeup().is_none(),
            "no timer while head is still this-session replay"
        );
    }

    #[test]
    fn unrelated_firehose_timeout_after_drain() {
        let start = Instant::now();
        let mut barrier = SessionLoadBarrier::new();
        let foreign = session_notif("other", false);
        assert!(
            barrier
                .push_or_dispatch(loaded(1, "s"), true, draining(Some(&foreign), start))
                .is_none()
        );
        assert!(
            barrier
                .take_ready(|_| true, draining(Some(&foreign), start))
                .is_empty()
        );
        let later = start + SESSION_LOADED_ACP_BARRIER + Duration::from_millis(1);
        let ready = barrier.take_ready(|_| true, draining(Some(&foreign), later));
        assert_eq!(ready.len(), 1);
    }

    #[test]
    fn unrelated_timeout_freezes_while_input_starved() {
        let start = Instant::now();
        let mut barrier = SessionLoadBarrier::new();
        let foreign = session_notif("other", false);
        assert!(
            barrier
                .push_or_dispatch(
                    loaded(1, "s"),
                    true,
                    tick(Some(&foreign), AcpDrainArm::InputStarved, start)
                )
                .is_none()
        );
        let later = start + SESSION_LOADED_ACP_BARRIER + Duration::from_secs(1);
        assert!(
            barrier
                .take_ready(
                    |_| true,
                    tick(Some(&foreign), AcpDrainArm::InputStarved, later)
                )
                .is_empty(),
            "input-starved Unrelated must not fire the 2s wall clock"
        );
        assert!(barrier.next_wakeup().is_none());
        assert!(
            barrier
                .take_ready(|_| true, draining(Some(&foreign), later))
                .is_empty(),
            "first CanDrain sample after starve starts the Unrelated clock at zero"
        );
        assert!(
            barrier
                .take_ready(
                    |_| true,
                    draining(
                        Some(&foreign),
                        later + SESSION_LOADED_ACP_BARRIER - Duration::from_millis(1)
                    )
                )
                .is_empty()
        );
        let ready = barrier.take_ready(
            |_| true,
            draining(
                Some(&foreign),
                later + SESSION_LOADED_ACP_BARRIER + Duration::from_millis(1),
            ),
        );
        assert_eq!(ready.len(), 1);
    }

    #[test]
    fn unparseable_ext_meta_fails_closed() {
        let now = Instant::now();
        let mut barrier = SessionLoadBarrier::new();
        let bad = ext_session_update_raw(json!({
            "sessionId": "s",
            "update": { "sessionUpdate": "agent_message_chunk" },
            "_meta": 1,
        }));
        assert_eq!(
            acp_load_backlog(Some(&bad), &sid("s")),
            AcpLoadBacklog::Unrelated
        );
        assert!(
            barrier
                .push_or_dispatch(loaded(1, "s"), true, draining(Some(&bad), now))
                .is_none()
        );
        assert!(
            barrier
                .take_ready(|_| true, draining(Some(&bad), now))
                .is_empty()
        );
    }

    #[test]
    fn acp_load_backlog_classifies_replay_and_live_heads() {
        let load = sid("s");
        assert_eq!(acp_load_backlog(None, &load), AcpLoadBacklog::Empty);
        let replay = session_notif("s", true);
        assert_eq!(
            acp_load_backlog(Some(&replay), &load),
            AcpLoadBacklog::ReplayHead
        );
        let live = session_notif("s", false);
        assert_eq!(
            acp_load_backlog(Some(&live), &load),
            AcpLoadBacklog::LiveHead
        );
        let other_live = session_notif("other", false);
        assert_eq!(
            acp_load_backlog(Some(&other_live), &load),
            AcpLoadBacklog::Unrelated
        );
        let ext_replay = ext_session_update("s", true);
        assert_eq!(
            acp_load_backlog(Some(&ext_replay), &load),
            AcpLoadBacklog::ReplayHead
        );
        let ext_live = ext_session_update("s", false);
        assert_eq!(
            acp_load_backlog(Some(&ext_live), &load),
            AcpLoadBacklog::LiveHead
        );
        let ext_notif_replay = ext_notification(
            "echo.agent/session_notification",
            json!({
                "sessionId": "s",
                "update": { "sessionUpdate": "agent_message_chunk" },
                "_meta": { "isReplay": true },
            }),
        );
        assert_eq!(
            acp_load_backlog(Some(&ext_notif_replay), &load),
            AcpLoadBacklog::ReplayHead
        );
        let other_method = ext_notification(
            "echo.agent/task_completed",
            json!({ "sessionId": "s", "taskId": "t" }),
        );
        assert_eq!(
            acp_load_backlog(Some(&other_method), &load),
            AcpLoadBacklog::Unrelated
        );
        let ext_other = ext_session_update("other", true);
        assert_eq!(
            acp_load_backlog(Some(&ext_other), &load),
            AcpLoadBacklog::Unrelated
        );
        let perm = request_permission("s");
        assert_eq!(
            acp_load_backlog(Some(&perm), &load),
            AcpLoadBacklog::Unrelated
        );
    }
}
