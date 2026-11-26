/// Terminal in-process error from [`crate::FsEventSource::start`]. Not
/// `Serialize`: never crosses the workspace transport boundary.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FsNotifyError {
    #[error("failed to start watcher")]
    WatcherStart(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("watcher initialization timed out")]
    Timeout,

    #[error("FsEventSource::start called outside a tokio runtime")]
    NoRuntime,
}
