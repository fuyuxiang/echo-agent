//! Single-flight guard for interactive login.
//!
//! At most one device-code / loopback wait runs at a time: starting a new
//! attempt (or an explicit `echo.agent/auth/cancel`) cancels the previous one, so
//! remint/retry cannot stack device-code mints.
//!
//! The attempt owns **all** attempt-scoped state — the cancellation token and
//! the code/url channels — so replacing an attempt swaps everything
//! atomically, and a cancelled predecessor that finishes late structurally
//! cannot touch its successor's channels. Generations guard `end()` the same
//! way: a stale finisher must not clear a newer attempt. Client `request_seq`
//! scopes explicit cancels so a delayed `echo.agent/auth/cancel` cannot tear down a
//! successor login.

use std::cell::{Cell, RefCell};
use tokio_util::sync::CancellationToken;

use super::flow::AuthUrlInfo;

/// Channels wired between the ACP ext handlers and one interactive auth flow.
/// `None` for headless attempts (no URL to show, no code to paste).
pub(crate) struct AttemptChannels {
    /// Forwards pasted codes from `echo.agent/auth/submit_code` to the flow.
    code_tx: tokio::sync::mpsc::Sender<String>,
    /// Yields the auth URL to `echo.agent/auth/get_url`. `Option` so
    /// [`AuthSingleFlight::take_url_rx`] can move it out while the attempt
    /// lives on (one-shot read).
    url_rx: Option<tokio::sync::oneshot::Receiver<AuthUrlInfo>>,
}

struct Attempt {
    token: CancellationToken,
    channels: Option<AttemptChannels>,
    /// Pager `request_seq` for this attempt (scopes delayed cancel RPCs).
    client_seq: Option<u64>,
}

/// Why [`AuthSingleFlight::submit_code`] failed.
#[derive(Debug)]
pub(crate) enum SubmitCodeError {
    /// No interactive attempt is waiting for a code (idle or headless).
    NoPendingAttempt,
    /// Channel send failed (attempt channels already closed).
    SendFailed(tokio::sync::mpsc::error::TrySendError<String>),
}

#[derive(Default)]
pub(crate) struct AuthSingleFlight {
    active: RefCell<Option<Attempt>>,
    generation: Cell<u64>,
}

/// RAII end for a [`AuthSingleFlight::begin`] generation: calls [`AuthSingleFlight::end`]
/// on drop so an aborted authenticate future cannot leak attempt state.
pub(crate) struct AuthAttemptGuard<'a> {
    sf: &'a AuthSingleFlight,
    generation: u64,
    ended: Cell<bool>,
}

impl AuthAttemptGuard<'_> {
    /// Explicit end (same as drop). Idempotent.
    pub(crate) fn end(&self) {
        if !self.ended.replace(true) {
            self.sf.end(self.generation);
        }
    }

    #[cfg(test)]
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
}

impl Drop for AuthAttemptGuard<'_> {
    fn drop(&mut self) {
        self.end();
    }
}

impl AuthSingleFlight {
    /// Start a new attempt, cancelling any prior in-flight one. Returns the
    /// new attempt's token and an [`AuthAttemptGuard`] that ends this generation
    /// on drop (pass no separate `end` — the guard is the only closer).
    ///
    /// `client_seq` is the pager auth `request_seq` (when known); used by
    /// [`Self::cancel_for_client_seq`] so a delayed cancel cannot kill a
    /// successor attempt.
    pub(crate) fn begin(
        &self,
        channels: Option<AttemptChannels>,
        client_seq: Option<u64>,
    ) -> (CancellationToken, AuthAttemptGuard<'_>) {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        let token = CancellationToken::new();
        if let Some(prev) = self.active.borrow_mut().replace(Attempt {
            token: token.clone(),
            channels,
            client_seq,
        }) {
            tracing::info!("auth: cancelling prior interactive auth for single-flight");
            prev.token.cancel();
        }
        (
            token,
            AuthAttemptGuard {
                sf: self,
                generation,
                ended: Cell::new(false),
            },
        )
    }

    /// Finish an attempt: drops its token *and channels* only if `generation`
    /// is still the active one (a stale finisher must not clear a newer
    /// attempt's state).
    pub(crate) fn end(&self, generation: u64) {
        if self.generation.get() == generation {
            *self.active.borrow_mut() = None;
        }
    }

    /// Cancel the active attempt, if any. Idempotent. Prefer
    /// [`Self::cancel_for_client_seq`] when the caller has a pager `request_seq`
    /// so a delayed cancel cannot tear down a newer login.
    pub(crate) fn cancel(&self) {
        if let Some(prev) = self.active.borrow_mut().take() {
            tracing::info!("auth: interactive auth cancelled");
            prev.token.cancel();
        }
    }

    /// Cancel only if the active attempt was started for `client_seq`. A stale
    /// cancel (successor already began) is a no-op.
    pub(crate) fn cancel_for_client_seq(&self, client_seq: u64) {
        let mut active = self.active.borrow_mut();
        match active.as_ref() {
            Some(a) if a.client_seq == Some(client_seq) => {
                if let Some(prev) = active.take() {
                    tracing::info!(
                        client_seq,
                        "auth: interactive auth cancelled for client request_seq"
                    );
                    prev.token.cancel();
                }
            }
            Some(a) => {
                tracing::debug!(
                    client_seq,
                    active_client_seq = ?a.client_seq,
                    "auth: ignoring stale cancel for superseded request_seq"
                );
            }
            None => {
                tracing::debug!(
                    client_seq,
                    "auth: cancel_for_client_seq with no active attempt"
                );
            }
        }
    }

    /// Forward a pasted code to the active attempt's flow.
    pub(crate) fn submit_code(&self, code: String) -> Result<(), SubmitCodeError> {
        match self
            .active
            .borrow()
            .as_ref()
            .and_then(|a| a.channels.as_ref())
        {
            Some(ch) => ch
                .code_tx
                .try_send(code)
                .map_err(SubmitCodeError::SendFailed),
            None => Err(SubmitCodeError::NoPendingAttempt),
        }
    }

    /// Take the active attempt's URL receiver (one-shot; subsequent calls
    /// return `None`, as does an idle or headless attempt).
    pub(crate) fn take_url_rx(&self) -> Option<tokio::sync::oneshot::Receiver<AuthUrlInfo>> {
        self.active
            .borrow_mut()
            .as_mut()
            .and_then(|a| a.channels.as_mut().and_then(|ch| ch.url_rx.take()))
    }
}

impl AttemptChannels {
    pub(crate) fn new(
        code_tx: tokio::sync::mpsc::Sender<String>,
        url_rx: tokio::sync::oneshot::Receiver<AuthUrlInfo>,
    ) -> Self {
        Self {
            code_tx,
            url_rx: Some(url_rx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channels() -> (AttemptChannels, tokio::sync::mpsc::Receiver<String>) {
        let (code_tx, code_rx) = tokio::sync::mpsc::channel(1);
        let (_url_tx, url_rx) = tokio::sync::oneshot::channel();
        (AttemptChannels::new(code_tx, url_rx), code_rx)
    }

    #[test]
    fn begin_cancels_prior_attempt() {
        let sf = AuthSingleFlight::default();
        let (first, _g1) = sf.begin(None, None);
        let (second, _g2) = sf.begin(None, None);
        assert!(first.is_cancelled(), "prior attempt must be cancelled");
        assert!(!second.is_cancelled(), "new attempt must be live");
    }

    #[test]
    fn cancel_stops_active_attempt_and_is_idempotent() {
        let sf = AuthSingleFlight::default();
        let (token, _g) = sf.begin(None, None);
        sf.cancel();
        assert!(token.is_cancelled());
        sf.cancel(); // no active attempt — must not panic
    }

    #[test]
    fn stale_end_does_not_clear_newer_attempt() {
        let sf = AuthSingleFlight::default();
        let (_first, first_guard) = sf.begin(None, None);
        let first_gen = first_guard.generation();
        // Keep first_guard alive but end via generation (stale after second begin).
        let (second, _second_guard) = sf.begin(None, None);
        sf.end(first_gen); // stale finisher
        sf.cancel(); // must still cancel the second attempt's token
        assert!(
            second.is_cancelled(),
            "stale end() must not have cleared the active token"
        );
    }

    #[test]
    fn current_end_drops_the_stored_attempt() {
        let sf = AuthSingleFlight::default();
        let (token, guard) = sf.begin(None, None);
        guard.end();
        sf.cancel(); // nothing active — must not cancel the finished attempt
        assert!(!token.is_cancelled());
    }

    /// The race the attempt object exists to prevent: a cancelled
    /// predecessor finishing late must not drop the successor's channels.
    #[test]
    fn stale_end_leaves_successor_channels_intact() {
        let sf = AuthSingleFlight::default();
        let (_first, first_guard) = sf.begin(None, None);
        let first_gen = first_guard.generation();
        let (ch, mut code_rx) = channels();
        let (_second, _g2) = sf.begin(Some(ch), Some(2));

        sf.end(first_gen); // stale finisher (attempt #1's cleanup)

        sf.submit_code("1234".into())
            .expect("successor's code channel must still be wired");
        assert_eq!(code_rx.try_recv().as_deref(), Ok("1234"));
        assert!(
            sf.take_url_rx().is_some(),
            "successor's url receiver must still be present"
        );
    }

    #[test]
    fn submit_code_and_url_rx_absent_when_idle_or_headless() {
        let sf = AuthSingleFlight::default();
        assert!(
            matches!(
                sf.submit_code("x".into()),
                Err(SubmitCodeError::NoPendingAttempt)
            ),
            "idle: no attempt is waiting for a code"
        );
        assert!(sf.take_url_rx().is_none());
        let _g = sf.begin(None, None); // headless attempt: token only
        assert!(
            matches!(
                sf.submit_code("x".into()),
                Err(SubmitCodeError::NoPendingAttempt)
            ),
            "headless: no channels"
        );
        assert!(sf.take_url_rx().is_none());
    }

    #[test]
    fn cancel_for_client_seq_ignores_stale_seq() {
        let sf = AuthSingleFlight::default();
        let (first, _g1) = sf.begin(None, Some(1));
        let (second, _g2) = sf.begin(None, Some(2));
        assert!(first.is_cancelled());
        sf.cancel_for_client_seq(1); // delayed cancel for attempt 1
        assert!(
            !second.is_cancelled(),
            "stale cancel must not tear down the successor"
        );
        sf.cancel_for_client_seq(2);
        assert!(second.is_cancelled());
    }

    #[test]
    fn attempt_guard_ends_on_drop() {
        let sf = AuthSingleFlight::default();
        let (token, guard) = sf.begin(None, Some(7));
        drop(guard);
        sf.cancel(); // nothing active
        assert!(!token.is_cancelled());
        assert!(matches!(
            sf.submit_code("x".into()),
            Err(SubmitCodeError::NoPendingAttempt)
        ));
    }

    /// Headless (and interactive) authenticate `select!`s on this token —
    /// cancel must interrupt a long wait rather than leaving it racing
    /// (logout / unscoped cancel path).
    #[tokio::test]
    async fn cancel_interrupts_waiting_select() {
        let sf = AuthSingleFlight::default();
        let (cancel, _guard) = sf.begin(None, Some(42)); // headless: no channels
        let waiter = tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => "cancelled",
                _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => "timeout",
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        sf.cancel(); // same as handle_logout / unscoped cancel
        assert_eq!(waiter.await.expect("join"), "cancelled");
    }

    /// Logout-style unscoped cancel, then a new begin must not see prior channels.
    #[test]
    fn cancel_then_begin_is_clean_for_successor() {
        let sf = AuthSingleFlight::default();
        let (ch, mut code_rx) = channels();
        let (old, _g) = sf.begin(Some(ch), Some(1));
        sf.cancel();
        assert!(old.is_cancelled());
        let (ch2, mut code_rx2) = channels();
        let (new, _g2) = sf.begin(Some(ch2), Some(2));
        assert!(!new.is_cancelled());
        sf.submit_code("ok".into()).expect("successor wired");
        assert_eq!(code_rx2.try_recv().as_deref(), Ok("ok"));
        assert!(code_rx.try_recv().is_err(), "prior channel must be dead");
    }
}
