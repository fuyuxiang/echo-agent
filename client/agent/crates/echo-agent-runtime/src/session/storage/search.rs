//! Binds the `echo-agent-session-search` index to this crate's JSONL session
//! store. A process that keeps no index holds no manager, so these entry
//! points take the handle rather than reach for a global.

use std::io;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use agent_client_protocol as acp;

use super::StorageAdapter;
use super::jsonl::JsonlStorageAdapter;
use crate::session::info::Info;
use crate::session::persistence::Summary;
use echo_agent_session_search::{IndexableSession, SessionSource};

pub use echo_agent_session_search::{
    SearchIndexManager, SearchIndexStatus, SessionSearchRequest, SessionSearchResponse,
};

/// Private on purpose: [`start_if_enabled`] is the only way to a manager, so the
/// feature cannot be bypassed. One live manager per echoagent home, at most.
fn start_search_index() -> SearchIndexManager {
    SearchIndexManager::start(
        |root| -> Box<dyn SessionSource> {
            Box::new(JsonlSessionSource(JsonlStorageAdapter::with_root(root)))
        },
        super::search_content::collect_all_indexable_content_single_pass,
    )
}

/// The index this process keeps, or the sentence naming what turned it off.
pub enum SearchIndex {
    Started(SearchIndexManager),
    Off { reason: String },
}

impl SearchIndex {
    pub fn index(&self) -> Option<&SearchIndexManager> {
        match self {
            Self::Started(index) => Some(index),
            Self::Off { .. } => None,
        }
    }

    pub fn started(self) -> Option<SearchIndexManager> {
        match self {
            Self::Started(index) => Some(index),
            Self::Off { .. } => None,
        }
    }

    pub fn off_reason(&self) -> Option<&str> {
        match self {
            Self::Started(_) => None,
            Self::Off { reason } => Some(reason),
        }
    }
}

/// The process's one index decision, shared rather than copied, so a session
/// created while the remote settings are in flight reads the answer that lands
/// later. `OnceLock` not `OnceCell`: the persistence actor's clone is `Send`.
#[derive(Clone, Default)]
pub struct SharedSearchIndex(Arc<OnceLock<Option<Arc<SearchIndexManager>>>>);

/// Three states, because collapsing the first two is a bug a reader cannot see:
/// an empty answer from `Pending` is not final, and one from `Off` is.
#[derive(Clone, Copy)]
pub enum IndexDecision<'a> {
    Pending,
    Off,
    On(&'a SearchIndexManager),
}

impl std::fmt::Debug for IndexDecision<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => f.write_str("Pending"),
            Self::Off => f.write_str("Off"),
            Self::On(_) => f.debug_tuple("On").finish_non_exhaustive(),
        }
    }
}

impl<'a> IndexDecision<'a> {
    /// The manager to write through. Treats `Pending` and `Off` alike on purpose:
    /// it skips, and the bootstrap after the decision backfills what was missed.
    /// A read must not, which is why this is not `decision`.
    pub fn writer(self) -> Option<&'a SearchIndexManager> {
        match self {
            Self::On(index) => Some(index),
            Self::Pending | Self::Off => None,
        }
    }

    /// For a caller with no pending window, as a CLI has. Takes the resolution,
    /// not its contents, which would let a pending `writer()` read back as `Off`.
    pub fn settled(index: &'a SearchIndex) -> Self {
        match index {
            SearchIndex::Started(index) => Self::On(index),
            SearchIndex::Off { .. } => Self::Off,
        }
    }
}

impl SharedSearchIndex {
    /// Call at use time, never store: the snapshot is what this type avoids.
    pub fn decision(&self) -> IndexDecision<'_> {
        match self.0.get() {
            None => IndexDecision::Pending,
            Some(None) => IndexDecision::Off,
            Some(Some(index)) => IndexDecision::On(index),
        }
    }

    pub(crate) fn decide(&self, index: impl FnOnce() -> Option<Arc<SearchIndexManager>>) {
        self.0.get_or_init(index);
    }

    /// For a session that must never reach an index, whatever the process decides.
    pub(crate) fn never_indexed() -> Self {
        let cell = OnceLock::new();
        let _ = cell.set(None);
        Self(Arc::new(cell))
    }
}

pub fn start_if_enabled(cfg: &crate::agent::config::Config) -> SearchIndex {
    if let Some(reason) = cfg.feature_off_reason(crate::agent::config::Feature::SessionSearch) {
        tracing::info!(
            reason = %reason,
            "session search index turned off for this process"
        );
        return SearchIndex::Off { reason };
    }
    SearchIndex::Started(start_search_index())
}

/// Projects the JSONL store's `Summary` down to the handful of fields the
/// index reads, so the index never sees the full session record.
struct JsonlSessionSource(JsonlStorageAdapter);

impl JsonlSessionSource {
    fn to_indexable(&self, summary: &Summary) -> IndexableSession {
        IndexableSession {
            session_id: summary.info.id.to_string(),
            cwd: summary.info.cwd.clone(),
            updated_at_unix: summary.updated_at.timestamp(),
            title: summary.display_title().to_owned(),
            updates_path: self.0.updates_file_path(&summary.info),
        }
    }
}

#[async_trait::async_trait]
impl SessionSource for JsonlSessionSource {
    async fn list_sessions(&self) -> io::Result<Vec<IndexableSession>> {
        let summaries = self.0.list_sessions(None).await?;
        Ok(summaries.iter().map(|s| self.to_indexable(s)).collect())
    }

    async fn load_session(
        &self,
        session_id: &str,
        cwd: &str,
    ) -> io::Result<Option<IndexableSession>> {
        let info = Info {
            id: acp::SessionId::new(session_id.to_string()),
            cwd: cwd.to_string(),
        };
        match self.0.load_summary(&info).await {
            Ok(summary) => Ok(Some(self.to_indexable(&summary))),
            // A missing session is a delete, not a failure: the index drops
            // its row. Every other error leaves the row alone.
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
}

pub fn notify_session_updated(index: Option<&SearchIndexManager>, session_id: &str, cwd: &str) {
    let Some(index) = index else {
        return;
    };
    let root = crate::util::echo_agent_home::echo_agent_home();
    index.enqueue(root, session_id.to_string(), cwd.to_string());
}

/// Remove one session from an index built earlier, whether or not this process still indexes.
pub(crate) async fn evict_session(root_dir: &Path, session_id: &str) {
    echo_agent_session_search::evict_session(root_dir, session_id).await;
}

pub async fn execute_search(
    decision: IndexDecision<'_>,
    root_dir: &Path,
    req: &SessionSearchRequest,
) -> io::Result<SessionSearchResponse> {
    let index = match decision {
        IndexDecision::Pending => return Ok(SessionSearchResponse::still_settling()),
        IndexDecision::Off => None,
        IndexDecision::On(index) => Some(index),
    };
    echo_agent_session_search::execute_search(index, root_dir, req).await
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::session::storage::search_content::test_summary;

    fn indexable_of(summary: &Summary) -> IndexableSession {
        JsonlSessionSource(JsonlStorageAdapter::with_root(PathBuf::from(
            "/nonexistent",
        )))
        .to_indexable(summary)
    }

    /// The index stores one title; the store decides which one, and a
    /// generated title outranks the session summary.
    #[test]
    fn indexable_prefers_generated_title() {
        let mut summary = test_summary("s1", "/workspace", "session summary");
        summary.generated_title = Some("Generated Title".to_string());
        assert_eq!(indexable_of(&summary).title, "Generated Title");

        summary.generated_title = Some(String::new());
        assert_eq!(indexable_of(&summary).title, "session summary");
    }

    #[test]
    fn indexable_carries_identity_and_recency() {
        let summary = test_summary("s1", "/workspace", "a title");
        let session = indexable_of(&summary);
        assert_eq!(session.session_id, "s1");
        assert_eq!(session.cwd, "/workspace");
        assert_eq!(session.updated_at_unix, summary.updated_at.timestamp());
    }
}
