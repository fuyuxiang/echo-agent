use async_trait::async_trait;

/// Input supplied when the host observes the session settling idle.
pub struct SessionIdleInput;

#[async_trait]
pub trait SessionLifecycleContributor: Send + Sync {
    /// Fired when the session settles idle (no running turn or queued work); the host owns the check.
    async fn on_session_idle(&self, _input: &SessionIdleInput) {}
}
