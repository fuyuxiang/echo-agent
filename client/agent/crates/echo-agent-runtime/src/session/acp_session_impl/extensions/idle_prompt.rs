//! Debounced `idle_prompt` notification extension.

use std::rc::Rc;
use std::time::Duration;

use echo_agent_lifecycle::LocalExtensionRegistryBuilder;
use echo_agent_lifecycle::{LocalSessionLifecycleContributor, LocalTurnLifecycleContributor};
use echo_agent_lifecycle::{
    SessionIdleInput, TurnAbortInput, TurnDoneInput, TurnErrorInput, TurnStartInput,
};

use super::super::*;
use super::{NotificationEvent, NotificationEventSink};

/// Default `idle_prompt` debounce (60s of user inactivity).
const DEFAULT_IDLE_NOTIFICATION_DELAY: Duration = Duration::from_secs(60);

/// Debounce between the session settling idle and the `idle_prompt` notification, so it fires only on sustained inactivity.
/// `ECHO_AGENT_IDLE_NOTIFICATION_DELAY_MS` overrides it (used by E2E tests).
fn idle_notification_delay() -> Duration {
    resolve_idle_notification_delay(std::env::var("ECHO_AGENT_IDLE_NOTIFICATION_DELAY_MS").ok())
}

/// Split from [`idle_notification_delay`] so the env parsing is testable without touching the process env.
fn resolve_idle_notification_delay(raw: Option<String>) -> Duration {
    raw.and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_IDLE_NOTIFICATION_DELAY)
}

/// Fires the `idle_prompt` notification hook once the session stays idle for the delay. Synthetic turns (auto-wake, drain, cron) only defer an
/// earned ping: they cancel the timer like any turn start, and their own idle settle re-arms it.
/// Covered by the headless E2E via `ECHO_AGENT_IDLE_NOTIFICATION_DELAY_MS`.
struct IdlePromptExtension {
    notification_event_sink: Rc<dyn NotificationEventSink>,
    timer: TaskSlot<()>,
    /// Never cleared. A turn start cancels the armed timer; clearing this there too would let a
    /// bash-mode turn, which starts but never ends, swallow the ping the previous turn earned.
    has_ever_ended_a_turn: std::cell::Cell<bool>,
}

#[async_trait::async_trait(?Send)]
impl LocalTurnLifecycleContributor for IdlePromptExtension {
    async fn on_turn_start(&self, _input: &TurnStartInput) {
        self.timer.cancel();
    }

    async fn on_turn_done(&self, _input: &TurnDoneInput) {
        self.has_ever_ended_a_turn.set(true);
    }

    async fn on_turn_abort(&self, _input: &TurnAbortInput) {
        self.has_ever_ended_a_turn.set(true);
    }

    async fn on_turn_error(&self, _input: &TurnErrorInput<'_>) {
        self.has_ever_ended_a_turn.set(true);
    }
}

#[async_trait::async_trait(?Send)]
impl LocalSessionLifecycleContributor for IdlePromptExtension {
    async fn on_session_idle(&self, _input: &SessionIdleInput) {
        if !self.has_ever_ended_a_turn.get() {
            return;
        }
        let notification_event_sink = Rc::clone(&self.notification_event_sink);
        let delay = idle_notification_delay();
        let handle = tokio::task::spawn_local(async move {
            tokio::time::sleep(delay).await;
            notification_event_sink.emit(NotificationEvent {
                notification_type: "idle_prompt",
                message: Some("Waiting for your next prompt".into()),
                title: None,
                level: Some("info".into()),
            });
        });
        self.timer.arm(handle);
    }
}

pub(super) fn install(
    builder: &mut LocalExtensionRegistryBuilder,
    notification_event_sink: Rc<dyn NotificationEventSink>,
) {
    let extension = Rc::new(IdlePromptExtension {
        notification_event_sink,
        timer: TaskSlot::new(),
        has_ever_ended_a_turn: std::cell::Cell::new(false),
    });
    builder.turn_lifecycle_contributor(extension.clone());
    builder.session_lifecycle_contributor(extension);
}

#[cfg(test)]
mod idle_notification_delay_tests {
    use super::{DEFAULT_IDLE_NOTIFICATION_DELAY, resolve_idle_notification_delay};
    use std::time::Duration;

    /// Missing env var → 60s default.
    #[test]
    fn defaults_to_claude_code_threshold() {
        assert_eq!(
            resolve_idle_notification_delay(None),
            Duration::from_secs(60)
        );
        assert_eq!(
            resolve_idle_notification_delay(None),
            DEFAULT_IDLE_NOTIFICATION_DELAY
        );
    }

    /// Pins the public `ECHO_AGENT_IDLE_NOTIFICATION_DELAY_MS` contract: a valid override is interpreted as milliseconds (the E2E seam depends on this).
    #[test]
    fn env_override_parses_millis() {
        assert_eq!(
            resolve_idle_notification_delay(Some("250".into())),
            Duration::from_millis(250)
        );
    }

    /// A malformed override falls back to the default instead of panicking.
    #[test]
    fn invalid_override_falls_back_to_default() {
        assert_eq!(
            resolve_idle_notification_delay(Some("not-a-number".into())),
            DEFAULT_IDLE_NOTIFICATION_DELAY
        );
    }
}

#[cfg(test)]
mod idle_after_interrupt_tests {
    use super::*;
    use echo_agent_lifecycle::TurnAbortReason;
    use std::cell::RefCell;

    #[derive(Default)]
    struct RecordingSink {
        emitted: RefCell<Vec<&'static str>>,
    }

    impl NotificationEventSink for RecordingSink {
        fn emit(&self, event: NotificationEvent) {
            self.emitted.borrow_mut().push(event.notification_type);
        }
    }

    fn extension(sink: &Rc<RecordingSink>) -> Rc<IdlePromptExtension> {
        Rc::new(IdlePromptExtension {
            notification_event_sink: Rc::clone(sink) as Rc<dyn NotificationEventSink>,
            timer: TaskSlot::new(),
            has_ever_ended_a_turn: std::cell::Cell::new(false),
        })
    }

    async fn settle() {
        tokio::time::sleep(idle_notification_delay() + Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
    }

    /// Regression: an interrupt used to leave a host stuck on "working". Every way a turn can end
    /// arms the ping, so dropping any one of the three callbacks fails this.
    #[tokio::test(start_paused = true)]
    async fn any_turn_ending_earns_a_ping() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                for end in ["abort", "done", "error"] {
                    let sink = Rc::new(RecordingSink::default());
                    let ext = extension(&sink);
                    match end {
                        "abort" => {
                            ext.on_turn_abort(&TurnAbortInput::new(TurnAbortReason::Interrupted))
                                .await;
                        }
                        "done" => ext.on_turn_done(&TurnDoneInput).await,
                        _ => ext.on_turn_error(&TurnErrorInput { message: "boom" }).await,
                    }
                    ext.on_session_idle(&SessionIdleInput).await;
                    settle().await;
                    assert_eq!(sink.emitted.borrow().as_slice(), ["idle_prompt"], "{end}");
                }
            })
            .await;
    }

    /// A bash-mode turn cancels the armed timer without ever ending a turn. Clearing the flag on
    /// turn start would swallow the ping the turn before it earned.
    #[tokio::test(start_paused = true)]
    async fn a_turn_that_never_ends_keeps_the_earned_ping() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let sink = Rc::new(RecordingSink::default());
                let ext = extension(&sink);
                ext.on_turn_done(&TurnDoneInput).await;
                ext.on_session_idle(&SessionIdleInput).await;
                ext.on_turn_start(&TurnStartInput::new(false)).await;
                ext.on_session_idle(&SessionIdleInput).await;
                settle().await;
                assert_eq!(sink.emitted.borrow().as_slice(), ["idle_prompt"]);
            })
            .await;
    }

    #[tokio::test(start_paused = true)]
    async fn session_with_no_turn_stays_quiet() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let sink = Rc::new(RecordingSink::default());
                let ext = extension(&sink);
                ext.on_session_idle(&SessionIdleInput).await;
                settle().await;
                assert!(sink.emitted.borrow().is_empty());
            })
            .await;
    }
}
