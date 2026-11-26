use async_trait::async_trait;

use crate::send::contributors::session_lifecycle::{SessionIdleInput, SessionLifecycleContributor};

/// `?Send` twin of [`SessionLifecycleContributor`].
#[async_trait(?Send)]
pub trait LocalSessionLifecycleContributor {
    /// Fired when the session settles idle (no running turn or queued work); the host owns the check.
    async fn on_session_idle(&self, _input: &SessionIdleInput) {}
}

/// Send contributors are usable in single-threaded hosts as-is, so shared logic implements
/// [`SessionLifecycleContributor`] once and both hosts can register it.
#[async_trait(?Send)]
impl<T: SessionLifecycleContributor> LocalSessionLifecycleContributor for T {
    async fn on_session_idle(&self, input: &SessionIdleInput) {
        SessionLifecycleContributor::on_session_idle(self, input).await;
    }
}
