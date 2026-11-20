//! Forwards recorded session updates back to a loading client, fitting
//! completion records written before the size limit existed.

use std::collections::VecDeque;
use std::path::PathBuf;

use agent_client_protocol as acp;
use echo_agent_paths::AbsPathBuf;

use super::{MvpAgent, mark_as_replay, stamp_meta_value};
use crate::session::storage::ReplayToolCollapser;

/// Max in-flight `forward_with_completion` receivers during cold resume.
/// Unbounded enqueue + sync pager apply peaks the pager at multi-GB on huge
/// sessions; this keeps ACP apply roughly windowed.
pub(super) const REPLAY_COMPLETION_WINDOW: usize = 64;

type ReplayCompletionRx = tokio::sync::oneshot::Receiver<echo_agent_acp::AcpResult<()>>;

#[derive(Clone, Copy)]
pub(super) enum ReplayOrigin {
    History,
    Live,
    DesktopCatchup,
}

impl ReplayOrigin {
    fn for_cursor(mark_replay: bool, desktop_catchup: bool) -> Self {
        if mark_replay {
            Self::History
        } else if desktop_catchup {
            Self::DesktopCatchup
        } else {
            Self::Live
        }
    }
}

/// Sliding window of replay completion receivers. Awaits the oldest when full
/// so at most [`REPLAY_COMPLETION_WINDOW`] notifications sit un-acked.
pub(super) struct ReplayCompletionDrain {
    pending: VecDeque<ReplayCompletionRx>,
    forwarded: usize,
}

impl ReplayCompletionDrain {
    pub(super) fn new() -> Self {
        Self {
            pending: VecDeque::with_capacity(REPLAY_COMPLETION_WINDOW),
            forwarded: 0,
        }
    }

    pub(super) async fn push(&mut self, rx: ReplayCompletionRx) {
        if self.pending.len() >= REPLAY_COMPLETION_WINDOW
            && let Some(oldest) = self.pending.pop_front()
        {
            let _ = oldest.await;
        }
        self.pending.push_back(rx);
        self.forwarded += 1;
    }

    pub(super) fn forwarded(&self) -> usize {
        self.forwarded
    }

    pub(super) async fn drain_all(mut self) {
        while let Some(rx) = self.pending.pop_front() {
            let _ = rx.await;
        }
    }
}

impl MvpAgent {
    /// Records written before completions were bounded can still be too long
    /// for a client to read. `None` drops one that cannot be shrunk, which
    /// costs a completion event but keeps the connection.
    fn fitted_replay_params(
        params: Box<serde_json::value::RawValue>,
    ) -> Option<Box<serde_json::value::RawValue>> {
        use crate::tools::task_completed_frame::{Refit, refit_recorded};

        match refit_recorded(&params) {
            Refit::Unchanged => Some(params),
            Refit::Fitted(fitted) => Some(fitted.into_inner()),
            Refit::Unfittable => {
                tracing::warn!(
                    bytes = params.get().len(),
                    "replay: dropping a completion too long to send"
                );
                None
            }
        }
    }

    /// Forward one raw JSONL replay line. Returns the completion receiver when
    /// a notification was actually sent.
    ///
    /// Dispatches by on-disk method name:
    /// - ACP updates (`"session/update"`) → typed `SessionNotification` for correct
    ///   TUI dispatch (direct dispatch preserves Rust types, not method strings).
    /// - EchoAgent updates (`"_echo.agent/session/update"`) → `ExtNotification`.
    ///
    /// When `mark_replay` is true, the notification is tagged with
    /// `_meta.isReplay: true` so the client knows it's historical data.
    /// Cursor-based reconnects set this to false for events after the cursor
    /// so the client processes them as live updates.
    pub(super) fn forward_raw_replay_line(
        &self,
        line: &str,
        persist_data: Option<&serde_json::Value>,
        target_client_id: Option<&serde_json::Value>,
        origin: ReplayOrigin,
        collapser: &mut ReplayToolCollapser,
    ) -> Option<ReplayCompletionRx> {
        use crate::session::storage::RawLinePeek;

        let env = match serde_json::from_str::<RawLinePeek<'_>>(line) {
            Ok(e) => e,
            Err(e) => {
                tracing::debug!(?e, "replay: skipping unparseable JSONL line");
                return None;
            }
        };
        // updates.jsonl only persists `_echo.agent/session/update` and `session/update`.
        // Unknown methods fall through to the ACP parse below and are dropped on error.
        let method = env.method.unwrap_or("session/update");
        let Some(raw_params) = env.params else {
            tracing::debug!("replay: skipping JSONL line with no params");
            return None;
        };
        let is_echo_agent = method == "_echo.agent/session/update";

        if is_echo_agent {
            // The fast-path forwards raw params with no `_meta` round-trip, so it
            // can stamp nothing. When a `target_client_id` is present we MUST take
            // the injection path instead, otherwise the replay would lose the
            // target and the leader would broadcast it to every subscriber.
            if target_client_id.is_none() && matches!(origin, ReplayOrigin::Live) {
                if let Ok(owned) =
                    serde_json::value::RawValue::from_string(raw_params.get().to_owned())
                    && let Some(owned) = Self::fitted_replay_params(owned)
                {
                    return Some(
                        self.gateway
                            .forward_with_completion(acp::ExtNotification::new(
                                "echo.agent/session/update",
                                std::sync::Arc::from(owned),
                            )),
                    );
                }
                return None;
            }
            let Ok(mut params) = serde_json::from_str::<serde_json::Value>(raw_params.get()) else {
                tracing::debug!("replay: skipping EchoAgent update with unparseable params");
                return None;
            };
            if let Some(obj) = params.as_object_mut() {
                let meta = obj.entry("_meta").or_insert_with(|| serde_json::json!({}));
                if let Some(m) = meta.as_object_mut() {
                    // `isReplay` only applies to historical replay events, not the
                    // post-cursor live deltas that reach this path when a target is set.
                    if !matches!(origin, ReplayOrigin::Live) {
                        m.insert("isReplay".to_string(), serde_json::json!(true));
                    }
                    if matches!(origin, ReplayOrigin::DesktopCatchup) {
                        m.insert("desktopCatchup".to_string(), serde_json::json!(true));
                    }
                    if let Some(pd) = persist_data {
                        m.insert("echo.agent/persist".to_string(), pd.clone());
                    }
                    if let Some(tid) = target_client_id {
                        m.insert("echo.agent/leaderClientId".to_string(), tid.clone());
                    }
                }
            }
            if let Ok(raw_val) = serde_json::value::to_raw_value(&params)
                && let Some(raw_val) = Self::fitted_replay_params(raw_val)
            {
                return Some(
                    self.gateway
                        .forward_with_completion(acp::ExtNotification::new(
                            "echo.agent/session/update",
                            std::sync::Arc::from(raw_val),
                        )),
                );
            }
            return None;
        }

        let Ok(notification) = serde_json::from_str::<acp::SessionNotification>(raw_params.get())
        else {
            tracing::debug!("replay: skipping ACP update with unparseable params");
            return None;
        };
        let acp::SessionNotification {
            session_id,
            update,
            meta,
            ..
        } = notification;
        let update = collapser.push(update)?;
        let mut notification = acp::SessionNotification::new(session_id, update);
        notification.meta = meta;
        if !matches!(origin, ReplayOrigin::Live) {
            mark_as_replay(&mut notification.meta, persist_data);
        }
        if matches!(origin, ReplayOrigin::DesktopCatchup) {
            stamp_meta_value(
                &mut notification.meta,
                "desktopCatchup",
                &serde_json::json!(true),
            );
        }
        // Stamp the leader unicast target regardless of mark_replay so the
        // leader routes both historical and post-cursor live deltas only to
        // the loading client.
        if let Some(tid) = target_client_id {
            stamp_meta_value(&mut notification.meta, "echo.agent/leaderClientId", tid);
        }
        Some(self.gateway.forward_with_completion(notification))
    }

    /// Replay updates from disk and drain completions.
    /// Returns the usage, resume offset, unfinished children, and whether the cursor matched.
    pub(super) async fn replay_session_updates(
        &self,
        session_id: &acp::SessionId,
        cwd: &AbsPathBuf,
        updates_file_path: &Option<PathBuf>,
        persist_data: Option<&serde_json::Value>,
        target_client_id: Option<&serde_json::Value>,
        cursor: Option<&str>,
        desktop_catchup: bool,
    ) -> Result<(u64, u64, Vec<(String, String)>, bool), acp::Error> {
        let mut replay_timer = crate::instrumentation_timer!("session.load_session_replay");
        replay_timer.with_field("session_id", session_id.0.as_ref());
        replay_timer.with_field("cwd", cwd.as_str());
        replay_timer.with_subphase(echo_agent_telemetry::startup::Subphase::SessionReplay);

        let Some(updates_path) = updates_file_path.as_ref() else {
            tracing::warn!(session_id = %session_id.0, "replay: no updates file path");
            return Ok((0, 0, Vec::new(), false));
        };

        let file_size = std::fs::metadata(updates_path)
            .map(|m| m.len())
            .unwrap_or(0);

        // Inline blocking I/O: spawn_blocking has multi-second latency on LocalSet.
        let raw_contents = match std::fs::read_to_string(updates_path) {
            Ok(s) if !s.is_empty() => s,
            _ => return Ok((0, 0, Vec::new(), false)),
        };
        let end_offset = raw_contents.len() as u64;

        let mut prepared = {
            let _timer = crate::instrumentation_timer!("session.replay.read_and_filter");
            crate::session::storage::prepare_replay_lines(&raw_contents, cursor)
        };
        let unfinished_subagents = std::mem::take(&mut prepared.unfinished_subagents);

        if cursor.is_some() {
            let sending = prepared.lines.len();
            if prepared.mark_replay {
                tracing::warn!(
                    session_id = %session_id.0,
                    "replay: cursor not found, falling back to full replay"
                );
            } else {
                tracing::info!(
                    session_id = %session_id.0,
                    skipped = prepared.total_live - sending,
                    remaining = sending,
                    "replay: cursor found, skipping events"
                );
            }
        }

        let last_tokens = prepared.last_tokens;
        let origin = ReplayOrigin::for_cursor(prepared.mark_replay, desktop_catchup);

        if let Some(max_seq) = prepared.max_event_seq {
            crate::util::event_id::ensure_event_counter_at_least(max_seq + 1);
        }

        let lines_to_send = prepared.lines;
        let updates_count = lines_to_send.len() as u64;
        let mut drain = ReplayCompletionDrain::new();

        {
            let _timer = crate::instrumentation_timer!("session.replay.forward_updates");
            let mut collapser = ReplayToolCollapser::new();
            for line in &lines_to_send {
                if let Some(rx) = self.forward_raw_replay_line(
                    line,
                    persist_data,
                    target_client_id,
                    origin,
                    &mut collapser,
                ) {
                    drain.push(rx).await;
                }
            }
            // Do not flush collapser leftovers: synthesizing a ToolCall here
            // would drop the persisted `_meta.eventId` and duplicate on
            // incremental reconnect. Child stream EOF flush is separate.
        }

        if updates_count > 0 && drain.forwarded() == 0 {
            tracing::warn!(
                updates_count,
                "Replay sent updates but collected 0 completions — \
                 forward_raw_replay_line must use gateway.forward_with_completion(). \
                 See: session/load notification ordering bug."
            );
        }
        {
            let _timer = crate::instrumentation_timer!("session.replay.drain_completions");
            drain.drain_all().await;
        }

        tracing::info!(
            session_id = %session_id.0,
            updates_count,
            end_offset,
            file_size,
            "replay: completed"
        );

        replay_timer.with_field("updates_count", updates_count);

        Ok((
            last_tokens,
            end_offset,
            unfinished_subagents,
            !prepared.mark_replay,
        ))
    }

    /// Enqueue replay notifications for updates appended after `from_offset`.
    /// Returns completion receivers; callers open the gate then drain.
    /// Intentionally sync (not async) so no prompt-task progress before gate flip.
    ///
    /// The delta tail is typically small (appends during the just-finished
    /// replay). Windowing would require `.await` here and would delay the gate
    /// flip; the caller drains the returned receivers before `LoadSessionResponse`.
    ///
    /// When `mark_replay` is false (cursor-based reconnect), delta events are
    /// forwarded without `_meta.isReplay` since they are truly new events the
    /// client has not seen.
    pub(super) fn replay_session_updates_from_offset_enqueue(
        &self,
        session_id: &acp::SessionId,
        updates_file_path: &Option<PathBuf>,
        from_offset: u64,
        persist_data: Option<&serde_json::Value>,
        target_client_id: Option<&serde_json::Value>,
        mark_replay: bool,
        desktop_catchup: bool,
    ) -> Vec<ReplayCompletionRx> {
        use std::io::{Read, Seek, SeekFrom};

        let Some(updates_path) = updates_file_path.as_ref() else {
            return Vec::new();
        };

        let mut file = match std::fs::File::open(updates_path) {
            Ok(f) => f,
            Err(_) => return Vec::new(),
        };
        if file.seek(SeekFrom::Start(from_offset)).is_err() {
            return Vec::new();
        }
        let mut contents = String::new();
        if file.read_to_string(&mut contents).is_err() || contents.is_empty() {
            return Vec::new();
        }

        let live_lines = crate::session::storage::filter_delta_replay_lines(&contents);
        let delta_count = live_lines.len();

        let mut completions = Vec::with_capacity(live_lines.len());
        let mut collapser = ReplayToolCollapser::new();
        for line in &live_lines {
            if let Some(rx) = self.forward_raw_replay_line(
                line,
                persist_data,
                target_client_id,
                ReplayOrigin::for_cursor(mark_replay, desktop_catchup),
                &mut collapser,
            ) {
                completions.push(rx);
            }
        }

        if delta_count > 0 && completions.is_empty() {
            tracing::warn!(
                delta_count,
                "Delta replay sent updates but collected 0 completions — \
                 forward_raw_replay_line must use gateway.forward_with_completion(). \
                 See: session/load notification ordering bug."
            );
        }

        if delta_count > 0 {
            tracing::info!(
                session_id = %session_id.0,
                delta_count,
                from_offset,
                "Delta replay enqueued updates (drain pending)"
            );
        }

        completions
    }
}

#[cfg(test)]
mod drain_tests {
    use super::{REPLAY_COMPLETION_WINDOW, ReplayCompletionDrain};

    #[tokio::test]
    async fn completion_window_drains_all_ready_receivers() {
        assert_eq!(REPLAY_COMPLETION_WINDOW, 64);
        let mut drain = ReplayCompletionDrain::new();
        for _ in 0..5 {
            let (tx, rx) = tokio::sync::oneshot::channel();
            tx.send(Ok(())).unwrap();
            drain.push(rx).await;
        }
        assert_eq!(drain.forwarded(), 5);
        drain.drain_all().await;
    }

    #[tokio::test]
    async fn completion_window_awaits_oldest_before_exceeding_cap() {
        let mut drain = ReplayCompletionDrain::new();
        let mut early_txs = Vec::new();
        for _ in 0..REPLAY_COMPLETION_WINDOW {
            let (tx, rx) = tokio::sync::oneshot::channel();
            early_txs.push(tx);
            drain.push(rx).await;
        }
        let (overflow_tx, overflow_rx) =
            tokio::sync::oneshot::channel::<echo_agent_acp::AcpResult<()>>();
        {
            let push = drain.push(overflow_rx);
            tokio::pin!(push);
            tokio::select! {
                _ = &mut push => panic!("push must wait for the oldest completion when the window is full"),
                _ = tokio::task::yield_now() => {}
            }
            early_txs
                .remove(0)
                .send(Ok(()))
                .expect("oldest receiver still live");
            push.await;
        }
        overflow_tx.send(Ok(())).ok();
        for tx in early_txs {
            let _ = tx.send(Ok(()));
        }
        drain.drain_all().await;
    }

    #[tokio::test]
    async fn drain_all_awaits_fifo_remainder() {
        let mut drain = ReplayCompletionDrain::new();
        let (tx0, rx0) = tokio::sync::oneshot::channel();
        let (tx1, rx1) = tokio::sync::oneshot::channel();
        let (tx2, rx2) = tokio::sync::oneshot::channel();
        drain.push(rx0).await;
        drain.push(rx1).await;
        drain.push(rx2).await;
        let drain_all = drain.drain_all();
        tokio::pin!(drain_all);
        tokio::select! {
            _ = &mut drain_all => panic!("drain_all must wait for index 0 first"),
            _ = tokio::task::yield_now() => {}
        }
        tx2.send(Ok(())).ok();
        tokio::select! {
            _ = &mut drain_all => panic!("index 2 ready must not finish drain before 0"),
            _ = tokio::task::yield_now() => {}
        }
        tx0.send(Ok(())).unwrap();
        tx1.send(Ok(())).unwrap();
        drain_all.await;
    }
}
