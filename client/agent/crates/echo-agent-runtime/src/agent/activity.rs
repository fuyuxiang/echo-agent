//! Send-safe view of the agent's in-flight work, shared with the leader's
//! auto-update checker and `RelaunchForUpdate` drain (`tokio::spawn` tasks
//! that cannot read the `!Send` `MvpAgent` state on the `LocalSet`).
//!
//! The leader's `agent_busy` flag only counts IPC (Unix-socket) requests;
//! relay (cloud.echo-agent.invalid WebSocket) traffic is bridged straight into the agent's
//! ACP stdin and never sets it, so a relay-driven leader (devbox / remote)
//! always looked idle and got restarted mid-turn on every update —
//! surfacing as "Subagent result channel dropped".
//!
//! [`AgentActivity::is_busy`] derives busyness from agent state regardless
//! of transport, and [`AgentActivity::flush_all_sessions`] lets the shutdown
//! path end session actors gracefully instead of aborting them via
//! `LocalSet` drop.
//!
//! ## Lifecycle: entries expire with their actor, not with agent bookkeeping
//!
//! The agent only ever **registers** sessions (at handle creation). There is
//! deliberately no unregister: an entry is live exactly while its actor
//! holds the command receiver (`!cmd_tx.is_closed()`), and closed entries
//! are purged opportunistically. This sidesteps a whole class of races
//! between `MvpAgent`'s map bookkeeping and actor lifetime — an actor
//! removed from the agent's map but still winding down stays visible to
//! `is_busy`/`flush_all_sessions` until it actually exits, and a session id
//! rebuilt with a fresh actor is just a second (distinct) entry.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::session::pending_interaction::PendingInteractions;
use crate::session::{SessionCommand, SessionHandle, ShutdownKind};

/// How often [`AgentActivity::flush_all_sessions`] re-polls actors that have
/// not yet exited.
const FLUSH_POLL: Duration = Duration::from_millis(50);

/// Default bound on a process-exit session flush ([`AgentActivity::flush_all_sessions`]):
/// leader auto-update shutdown and the in-process agent's `/exit` / headless-quit
/// path both use it, so one wedged actor delays exit by the same amount everywhere.
/// Sessions are normally idle by then and the flush completes in milliseconds.
///
/// Known gap: a `SessionEnd` hook configured with a longer `timeout` than this
/// is still cut off at the grace. Aligning the two needs the hook registry's
/// configured timeouts at flush time, which this layer does not see — tracked as
/// a follow-up rather than hardcoding a larger bound for every exit.
pub const SESSION_FLUSH_GRACE: Duration = Duration::from_secs(10);

/// Per-session slice of state shared with the session actor (the same `Arc`s
/// the actor mutates — see the matching `SessionHandle` fields).
struct SessionActivityEntry {
    id: String,
    cmd_tx: tokio::sync::mpsc::UnboundedSender<SessionCommand>,
    /// `Some` while a turn is running (relay- or IPC-driven alike).
    current_prompt_id: Arc<Mutex<Option<String>>>,
    /// Non-empty while a blocking reverse-request (permission / question /
    /// plan approval) is parked.
    pending_interactions: PendingInteractions,
}

impl SessionActivityEntry {
    /// The actor still holds the command receiver.
    fn is_live(&self) -> bool {
        !self.cmd_tx.is_closed()
    }

    /// A running turn or a parked blocking interaction.
    fn is_busy(&self) -> bool {
        self.current_prompt_id
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
            || !self
                .pending_interactions
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_empty()
    }
}

#[derive(Default)]
struct ActivityInner {
    /// Self-expiring: entries are dead once the actor drops its receiver
    /// (see module docs), and are purged whenever the list is locked.
    sessions: Mutex<Vec<SessionActivityEntry>>,
    /// Kept across a host grace timeout and actor exit: channel closure alone
    /// is never a successful persistence acknowledgement.
    checked_shutdowns: Mutex<Vec<CheckedSessionShutdown>>,
    /// Subagents currently initializing or running; kept in sync by
    /// the shared coordinator's `running_count_changed` callback.
    subagents: Arc<AtomicUsize>,
}

struct CheckedSessionShutdown {
    id: String,
    cmd_tx: tokio::sync::mpsc::UnboundedSender<SessionCommand>,
    completion: tokio::sync::watch::Receiver<Option<Result<(), String>>>,
}

/// Cheap-to-clone, `Send + Sync` handle. See module docs.
#[derive(Clone, Default)]
pub struct AgentActivity {
    inner: Arc<ActivityInner>,
}

impl AgentActivity {
    /// Register a session's shared state at handle-creation time. No
    /// unregister exists — the entry expires when the actor exits.
    pub(crate) fn register_session(&self, id: &str, handle: &SessionHandle) {
        self.lock_live_sessions().push(SessionActivityEntry {
            id: id.to_string(),
            cmd_tx: handle.cmd_tx.clone(),
            current_prompt_id: handle.current_prompt_id.clone(),
            pending_interactions: handle.pending_interactions.clone(),
        });
    }

    /// Shared gauge of initializing + running subagents; updated from the
    /// shared coordinator's lifecycle callback.
    pub(crate) fn subagent_gauge(&self) -> Arc<AtomicUsize> {
        self.inner.subagents.clone()
    }

    /// Whether the agent has live work: a running turn, a parked blocking
    /// interaction, or an initializing/running subagent.
    ///
    /// Known sub-tick window: queued-but-not-started prompts
    /// (`pending_inputs` in the actor) are not mirrored here, so a prompt
    /// submitted exactly at a turn boundary can read as idle (the same
    /// window `session_has_live_work` closes with an actor round-trip,
    /// which a sync `Send` probe cannot do). The flush's quiesce loop
    /// re-snapshots and still ends such an actor via its Shutdown arm.
    pub fn is_busy(&self) -> bool {
        self.inner.subagents.load(Ordering::Relaxed) > 0
            || self.lock_live_sessions().iter().any(|e| e.is_busy())
    }

    /// Number of live registered sessions (diagnostics/tests).
    pub fn session_count(&self) -> usize {
        self.lock_live_sessions().len()
    }

    /// Send [`SessionCommand::Shutdown`] to every live session actor
    /// (replay-buffer flush → hooks → memory save → actor returns) and wait
    /// up to `grace` for the actors to exit, observed via
    /// `cmd_tx.is_closed()`.
    ///
    /// This is a quiesce loop, not a one-shot broadcast: each poll
    /// re-snapshots the registry and signals actors that appeared after the
    /// flush started (deduped by channel identity, so a session id rebuilt
    /// with a fresh actor gets its own signal), all against one deadline —
    /// `grace` bounds the **total** shutdown delay.
    ///
    /// Callers: the leader's auto-update / `RelaunchForUpdate` shutdown, and
    /// the in-process agent worker on `/exit` / headless quit. In the leader
    /// case, call **before** cancelling the root token; in the in-process case,
    /// **after** the cancel that ends the worker's run loop but before its
    /// `LocalSet` drops — either way, session state must be durable before the
    /// drop aborts remaining tasks. Actors that miss the grace are logged and
    /// abandoned.
    pub async fn flush_all_sessions(&self, grace: Duration) {
        let deadline = tokio::time::Instant::now() + grace;
        // Every distinct channel signaled so far (id kept for logging).
        let mut signaled: Vec<(String, tokio::sync::mpsc::UnboundedSender<SessionCommand>)> =
            Vec::new();

        loop {
            let snapshot: Vec<_> = self
                .lock_live_sessions()
                .iter()
                .map(|e| (e.id.clone(), e.cmd_tx.clone()))
                .collect();
            for (id, tx) in snapshot {
                if !signaled.iter().any(|(_, s)| s.same_channel(&tx)) {
                    tracing::info!(session_id = %id, "shutdown: flushing session");
                    let _ = tx.send(SessionCommand::Shutdown(ShutdownKind::Graceful));
                    signaled.push((id, tx));
                }
            }

            if signaled.iter().all(|(_, tx)| tx.is_closed()) {
                return; // nothing to flush, or all actors exited
            }
            if tokio::time::Instant::now() >= deadline {
                for (id, tx) in &signaled {
                    if !tx.is_closed() {
                        tracing::warn!(
                            session_id = %id,
                            "shutdown: session actor did not exit within grace; proceeding"
                        );
                    }
                }
                return;
            }
            tokio::time::sleep(FLUSH_POLL).await;
        }
    }

    /// Checked host shutdown. Unlike the CLI's bounded best-effort flush,
    /// success requires every actor's post-SessionEnd persistence ACK.
    /// Timeout/write failures retain the same ACK receivers for a later retry.
    pub async fn flush_all_sessions_checked(&self, grace: Duration) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + grace;
        loop {
            let snapshot: Vec<_> = self
                .lock_live_sessions()
                .iter()
                .map(|entry| (entry.id.clone(), entry.cmd_tx.clone()))
                .collect();
            {
                let mut checked = self
                    .inner
                    .checked_shutdowns
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                for (id, cmd_tx) in snapshot {
                    if checked
                        .iter()
                        .any(|entry| entry.cmd_tx.same_channel(&cmd_tx))
                    {
                        continue;
                    }
                    let (completion, receiver) = tokio::sync::watch::channel(None);
                    let _ = cmd_tx.send(SessionCommand::ShutdownChecked { completion });
                    checked.push(CheckedSessionShutdown {
                        id,
                        cmd_tx,
                        completion: receiver,
                    });
                }
                let mut pending = Vec::new();
                let mut errors = Vec::new();
                for entry in checked.iter() {
                    match entry.completion.borrow().as_ref() {
                        Some(Ok(())) if entry.cmd_tx.is_closed() => {}
                        Some(Ok(())) => pending.push(entry.id.clone()),
                        Some(Err(error)) => errors.push(format!("{}: {error}", entry.id)),
                        None if entry.completion.has_changed().is_err()
                            || entry.cmd_tx.is_closed() =>
                        {
                            errors.push(format!(
                                "{}: session exited without a final persistence acknowledgement",
                                entry.id
                            ));
                        }
                        None => pending.push(entry.id.clone()),
                    }
                }
                if !errors.is_empty() {
                    return Err(errors.join("; "));
                }
                if pending.is_empty() {
                    checked.clear();
                    return Ok(());
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(format!(
                        "session persistence is still pending: {}",
                        pending.join(", ")
                    ));
                }
            }
            tokio::time::sleep(FLUSH_POLL).await;
        }
    }

    /// Lock the session list, dropping entries whose actor has exited.
    ///
    /// Purging happens only here, so in modes with no periodic reader (no
    /// auto-update checker) a dead entry lingers until the next register —
    /// bounded and tiny (a sender handle + two `Arc`s per entry).
    fn lock_live_sessions(&self) -> std::sync::MutexGuard<'_, Vec<SessionActivityEntry>> {
        let mut guard = self
            .inner
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        guard.retain(SessionActivityEntry::is_live);
        guard
    }

    /// Register a synthetic session from raw parts (no full `SessionHandle`).
    /// Returns the command receiver (the "actor" side) plus the shared
    /// running-turn and pending-interaction slots.
    #[cfg(test)]
    pub(crate) fn register_for_test(
        &self,
        id: &str,
    ) -> (
        tokio::sync::mpsc::UnboundedReceiver<SessionCommand>,
        Arc<Mutex<Option<String>>>,
        PendingInteractions,
    ) {
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let current_prompt_id = Arc::new(Mutex::new(None));
        let pending_interactions: PendingInteractions =
            Arc::new(Mutex::new(std::collections::HashMap::new()));
        self.lock_live_sessions().push(SessionActivityEntry {
            id: id.to_string(),
            cmd_tx,
            current_prompt_id: current_prompt_id.clone(),
            pending_interactions: pending_interactions.clone(),
        });
        (cmd_rx, current_prompt_id, pending_interactions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a registered entry from raw parts without a full SessionHandle.
    fn register_raw(
        activity: &AgentActivity,
        id: &str,
    ) -> (
        tokio::sync::mpsc::UnboundedReceiver<SessionCommand>,
        Arc<Mutex<Option<String>>>,
        PendingInteractions,
    ) {
        activity.register_for_test(id)
    }

    /// Simulated session actor: exits (dropping its receiver) `delay` after
    /// receiving `Shutdown`; resolves to whether Shutdown was received.
    fn spawn_actor(
        mut rx: tokio::sync::mpsc::UnboundedReceiver<SessionCommand>,
        delay: Duration,
    ) -> tokio::task::JoinHandle<bool> {
        tokio::spawn(async move {
            while let Some(cmd) = rx.recv().await {
                if matches!(cmd, SessionCommand::Shutdown(_)) {
                    tokio::time::sleep(delay).await;
                    return true;
                }
            }
            false
        })
    }

    #[test]
    fn idle_by_default() {
        let activity = AgentActivity::default();
        assert!(!activity.is_busy());
    }

    #[tokio::test]
    async fn running_turn_marks_busy() {
        let activity = AgentActivity::default();
        let (_rx, prompt_id, _pending) = register_raw(&activity, "s1");
        assert!(!activity.is_busy());

        *prompt_id.lock().unwrap() = Some("prompt-1".to_string());
        assert!(activity.is_busy());

        *prompt_id.lock().unwrap() = None;
        assert!(!activity.is_busy());
    }

    #[tokio::test]
    async fn pending_interaction_marks_busy() {
        let activity = AgentActivity::default();
        let (_rx, _prompt_id, pending) = register_raw(&activity, "s1");

        pending.lock().unwrap().insert(
            "tc-1".to_string(),
            crate::session::pending_interaction::PendingKind::Permission,
        );
        assert!(activity.is_busy());

        pending.lock().unwrap().clear();
        assert!(!activity.is_busy());
    }

    #[test]
    fn subagent_gauge_marks_busy() {
        let activity = AgentActivity::default();
        let gauge = activity.subagent_gauge();
        assert!(!activity.is_busy());
        gauge.store(1, Ordering::Relaxed);
        assert!(activity.is_busy());
        gauge.store(0, Ordering::Relaxed);
        assert!(!activity.is_busy());
    }

    /// An actor that is still running counts as busy even if the agent has
    /// dropped its handle — liveness comes from the channel, not from agent
    /// bookkeeping. Once the actor exits, the entry expires.
    #[tokio::test]
    async fn live_actor_counts_busy_until_it_exits() {
        let activity = AgentActivity::default();
        let (rx, prompt_id, _pending) = register_raw(&activity, "s1");
        *prompt_id.lock().unwrap() = Some("prompt-1".to_string());
        assert!(activity.is_busy());
        assert_eq!(activity.session_count(), 1);

        // Actor exits (receiver dropped) → entry expires, even though the
        // shared prompt slot still says Some.
        drop(rx);
        assert!(!activity.is_busy());
        assert_eq!(activity.session_count(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn flush_sends_shutdown_and_waits_for_actor_exit() {
        let activity = AgentActivity::default();
        let (rx, _prompt_id, _pending) = register_raw(&activity, "s1");

        // Simulated actor: exits (drops rx) when it receives Shutdown.
        let actor = spawn_actor(rx, Duration::ZERO);

        activity.flush_all_sessions(Duration::from_secs(5)).await;
        assert!(actor.await.unwrap(), "actor should have received Shutdown");
    }

    #[tokio::test(start_paused = true)]
    async fn flush_grace_bounds_total_delay_across_sessions() {
        let activity = AgentActivity::default();
        // One wedged actor (receiver kept open) and one healthy actor.
        let (_wedged_rx, _p1, _i1) = register_raw(&activity, "wedged");
        let (rx, _p2, _i2) = register_raw(&activity, "healthy");
        let actor = spawn_actor(rx, Duration::ZERO);

        // The wedged actor must not consume the healthy actor's budget, and
        // the total wait must be ~one grace period, not one per session.
        let start = tokio::time::Instant::now();
        activity.flush_all_sessions(Duration::from_secs(2)).await;
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_secs(2));
        assert!(
            elapsed < Duration::from_secs(3),
            "grace must be shared, not serial: {elapsed:?}"
        );
        assert!(actor.await.unwrap(), "healthy actor should get Shutdown");
    }

    /// A session id rebuilt with a fresh actor while the old actor is still
    /// winding down: both channels must be signaled and awaited.
    #[tokio::test(start_paused = true)]
    async fn flush_awaits_both_channels_when_id_is_reused() {
        let activity = AgentActivity::default();
        let (old_rx, _p1, _i1) = register_raw(&activity, "s1");
        let (new_rx, _p2, _i2) = register_raw(&activity, "s1");

        let old_actor = spawn_actor(old_rx, Duration::from_millis(500));
        let new_actor = spawn_actor(new_rx, Duration::ZERO);

        activity.flush_all_sessions(Duration::from_secs(5)).await;
        assert!(old_actor.is_finished(), "flush must wait for the old actor");
        assert!(old_actor.await.unwrap());
        assert!(new_actor.await.unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn flush_signals_sessions_that_appear_mid_flush() {
        let activity = AgentActivity::default();
        // Actor 1: holds the flush open for a few polls, then exits.
        let (rx1, _p1, _i1) = register_raw(&activity, "s1");
        let actor1 = spawn_actor(rx1, Duration::from_millis(300));

        // Actor 2 registers AFTER the flush has started (a relay-driven
        // prompt racing the shutdown) — it must still receive Shutdown.
        let activity_late = activity.clone();
        let late = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let (mut rx2, _p2, _i2) = activity_late.register_for_test("s2");
            while let Some(cmd) = rx2.recv().await {
                if matches!(cmd, SessionCommand::Shutdown(_)) {
                    return true;
                }
            }
            false
        });

        activity.flush_all_sessions(Duration::from_secs(5)).await;
        assert!(actor1.await.unwrap());
        assert!(
            late.await.unwrap(),
            "session registered mid-flush must receive Shutdown"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn flush_gives_up_after_grace_when_actor_hangs() {
        let activity = AgentActivity::default();
        // Keep rx alive so the channel never closes (wedged actor).
        let (_rx, _prompt_id, _pending) = register_raw(&activity, "s1");

        let start = tokio::time::Instant::now();
        activity.flush_all_sessions(Duration::from_secs(2)).await;
        assert!(
            start.elapsed() >= Duration::from_secs(2),
            "flush should wait out the grace period"
        );
        // Returned rather than hanging forever — that's the assertion.
    }

    #[tokio::test]
    async fn flush_with_no_sessions_is_noop() {
        let activity = AgentActivity::default();
        activity.flush_all_sessions(Duration::from_secs(1)).await;
    }

    #[tokio::test(start_paused = true)]
    async fn checked_flush_requires_a_save_ack_not_only_actor_exit() {
        let activity = AgentActivity::default();
        let (mut rx, _, _) = register_raw(&activity, "unacknowledged");
        let actor = tokio::spawn(async move {
            let Some(SessionCommand::ShutdownChecked { completion }) = rx.recv().await else {
                panic!("checked shutdown command expected");
            };
            drop(completion);
        });
        let error = activity
            .flush_all_sessions_checked(Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(error.contains("without a final persistence acknowledgement"));
        actor.await.unwrap();
        assert_eq!(activity.session_count(), 0);
        assert!(
            activity
                .flush_all_sessions_checked(Duration::from_secs(1))
                .await
                .is_err(),
            "re-polling an empty live registry must not erase a failed save"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn checked_flush_timeout_retains_the_ack_for_the_next_retry() {
        let activity = AgentActivity::default();
        let (mut rx, _, _) = register_raw(&activity, "slow-save");
        let actor = tokio::spawn(async move {
            let Some(SessionCommand::ShutdownChecked { completion }) = rx.recv().await else {
                panic!("checked shutdown command expected");
            };
            tokio::time::sleep(Duration::from_secs(2)).await;
            completion.send_replace(Some(Ok(())));
        });
        assert!(
            activity
                .flush_all_sessions_checked(Duration::from_millis(100))
                .await
                .is_err()
        );
        actor.await.unwrap();
        assert_eq!(activity.session_count(), 0);
        activity
            .flush_all_sessions_checked(Duration::from_secs(1))
            .await
            .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn checked_flush_propagates_write_failure_and_can_observe_a_save_retry() {
        let activity = AgentActivity::default();
        let (mut rx, _, _) = register_raw(&activity, "disk-full");
        let (retry, retried) = tokio::sync::oneshot::channel();
        let actor = tokio::spawn(async move {
            let Some(SessionCommand::ShutdownChecked { completion }) = rx.recv().await else {
                panic!("checked shutdown command expected");
            };
            completion.send_replace(Some(Err("No space left on device".to_string())));
            retried.await.unwrap();
            completion.send_replace(Some(Ok(())));
        });
        let error = activity
            .flush_all_sessions_checked(Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(error.contains("disk-full: No space left on device"));
        assert_eq!(
            activity.session_count(),
            1,
            "failed final save retains its actor"
        );
        retry.send(()).unwrap();
        actor.await.unwrap();
        activity
            .flush_all_sessions_checked(Duration::from_secs(1))
            .await
            .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn checked_flush_waits_for_actor_exit_even_after_a_successful_save_ack() {
        let activity = AgentActivity::default();
        let (mut rx, _, _) = register_raw(&activity, "finishing");
        let actor = tokio::spawn(async move {
            let Some(SessionCommand::ShutdownChecked { completion }) = rx.recv().await else {
                panic!("checked shutdown command expected");
            };
            completion.send_replace(Some(Ok(())));
            tokio::time::sleep(Duration::from_secs(1)).await;
        });
        let started = tokio::time::Instant::now();
        activity
            .flush_all_sessions_checked(Duration::from_secs(2))
            .await
            .unwrap();
        assert!(started.elapsed() >= Duration::from_secs(1));
        actor.await.unwrap();
    }
}
