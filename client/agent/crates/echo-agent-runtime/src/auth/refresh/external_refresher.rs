use std::sync::Arc;

use crate::auth::error::RefreshTokenFailedReason;
use crate::auth::manager::RefreshReason;

use super::ExternalCommandRunner;
use super::RefreshOutcome;
use super::TokenRefresher;

/// Refreshes by re-running the operator's external auth binary via the async
/// external-command runner. Returns data only; mutation lives in
/// `refresh_chain` (honors the [`TokenRefresher`] no-mutation contract).
pub(crate) struct ExternalBinaryRefresher {
    runner: Arc<dyn ExternalCommandRunner>,
    command: String,
}

impl ExternalBinaryRefresher {
    pub(crate) fn new(runner: Arc<dyn ExternalCommandRunner>, command: String) -> Self {
        Self { runner, command }
    }

    /// A failed or timed-out binary run is a single-strike permanent failure;
    /// the reason is non-sticky so a flaky or briefly slow binary still
    /// recovers without the user.
    fn record_failure(&self, message: &str) -> RefreshOutcome {
        tracing::warn!(%message, "auth: external binary refresh failed permanently");
        // No token key in the binary flow; the caller scopes the verdict.
        RefreshOutcome::permanent(RefreshTokenFailedReason::ProviderInteractiveRequired, None)
    }
}

#[async_trait::async_trait]
impl TokenRefresher for ExternalBinaryRefresher {
    async fn refresh(&self, reason: RefreshReason) -> RefreshOutcome {
        tracing::debug!(?reason, "auth: external binary refresh starting");
        match self.runner.run_external_command(&self.command).await {
            Some(auth) => {
                crate::unified_log::info("auth: external binary refresh succeeded", None, None);
                RefreshOutcome::success(auth)
            }
            None => {
                crate::unified_log::warn(
                    "auth: external binary refresh returned no token",
                    None,
                    None,
                );
                self.record_failure("external binary returned no token")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::EchoAgentAuth;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Runner that yields scripted results in order, then `None`.
    struct FakeRunner {
        results: Mutex<Vec<Option<EchoAgentAuth>>>,
        calls: AtomicU32,
    }
    impl FakeRunner {
        fn new(results: Vec<Option<EchoAgentAuth>>) -> Self {
            Self {
                results: Mutex::new(results),
                calls: AtomicU32::new(0),
            }
        }
        fn calls(&self) -> u32 {
            self.calls.load(Ordering::SeqCst)
        }
    }
    #[async_trait::async_trait]
    impl ExternalCommandRunner for FakeRunner {
        async fn run_external_command(&self, _command: &str) -> Option<EchoAgentAuth> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut results = self.results.lock().unwrap();
            if results.is_empty() {
                return None;
            }
            results.remove(0)
        }
    }

    /// A failed binary run must stay NON-sticky: it has to age out via the
    /// TTL, never lock an external-binary user out forever.
    #[tokio::test]
    async fn external_binary_failure_is_single_strike_non_sticky_permanent() {
        let runner = Arc::new(FakeRunner::new(vec![None]));
        let refresher = ExternalBinaryRefresher::new(runner.clone(), "auth-binary".into());
        match refresher.refresh(RefreshReason::ServerRejected).await {
            RefreshOutcome::PermanentFailure { error, .. } => {
                assert_eq!(
                    error.reason,
                    RefreshTokenFailedReason::ProviderInteractiveRequired
                );
                assert!(
                    !error.reason.is_sticky(),
                    "external-binary failure must age out, not strand the user forever",
                );
            }
            other => panic!("a failed binary run must be a permanent Other failure, got {other:?}"),
        }
        assert_eq!(runner.calls(), 1, "the single run gets the whole 7s budget");
    }

    #[tokio::test]
    async fn external_binary_success_returns_fresh_token() {
        let token = EchoAgentAuth {
            key: "ext-fresh".into(),
            ..EchoAgentAuth::test_default()
        };
        let runner = Arc::new(FakeRunner::new(vec![Some(token)]));
        let refresher = ExternalBinaryRefresher::new(runner.clone(), "auth-binary".into());
        match refresher.refresh(RefreshReason::ServerRejected).await {
            RefreshOutcome::Success(auth) => assert_eq!(auth.key, "ext-fresh"),
            other => panic!("a successful binary run must return Success, got {other:?}"),
        }
        assert_eq!(
            runner.calls(),
            1,
            "a success must run the binary exactly once"
        );
    }
}
