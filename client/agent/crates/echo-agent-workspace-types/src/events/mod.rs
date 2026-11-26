//! Pub/sub event types and topic-filter sets.
//!
//! Only the wire types live here; the broadcast channel
//! and `EventStream` wrapper are runtime concerns.
//!
//! There is **no** `SessionEvent` enum. The EventBus only
//! carries [`WorkspaceEvent`] -- session-scoped state (prompt
//! boundaries, tool-call lifecycle, plan-mode transitions, subagent
//! lifecycle, compaction, memory flushes, ...) is sampler-caused and
//! is reported back to the sampler via the originating call's return
//! value or stream chunks. The sampler owns that state and forwards
//! to its UI channel as needed.

pub mod lag;
pub mod workspace;

pub use lag::EventLag;
pub use workspace::{WorkspaceEvent, WorkspaceTopic, WorkspaceTopicSet};
