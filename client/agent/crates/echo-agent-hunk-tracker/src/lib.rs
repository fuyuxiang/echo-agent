//! echo-agent-hunk-tracker - Track file hunks (diffs) with agent/external attribution.
//!
//! This crate provides:
//! - Actor-based hunk tracking with source attribution (Agent vs External)
//! - Integration with echo-agent-shell sessions
//!
//! ## Actor Pattern
//!
//! The HunkTracker uses an actor pattern with message-passing via channels:
//!
//! ```text
//! ┌────────────────┐                  ┌──────────────────────────────────────┐
//! │  Agent Tool    │ ─── Command ───▶ │        HunkTrackerActor              │
//! │  (search_      │                  │  (runs in dedicated tokio task)      │
//! │   replace)     │                  │                                      │
//! └────────────────┘                  │  State (no locks needed):            │
//!                                     │  - file_states: HashMap              │
//! ┌────────────────┐                  │  - git_dirty_cache: HashSet          │
//! │   fs_notify    │ ─── Command ───▶ │  - mode: TrackingMode                │
//! │   event loop   │                  │                                      │
//! └────────────────┘                  │         │ HunkEvent                  │
//!                                     │         ▼                            │
//! ┌────────────────┐                  │  ┌──────────────────┐               │
//! │  Query (e.g.   │ ── Cmd+Oneshot ─▶│  │ event_tx         │───▶ Client    │
//! │   get_hunks)   │ ◀── Response ────│  └──────────────────┘               │
//! └────────────────┘                  └──────────────────────────────────────┘
//! ```
//!
//! ## Usage
//!
//! ```rust,ignore
//! use echo_agent_hunk_tracker::{HunkTrackerActor, HunkEvent, TrackingMode, HunkAction};
//! use tokio::sync::mpsc;
//!
//! // Create event channel
//! let (event_tx, mut event_rx) = mpsc::unbounded_channel();
//!
//! // Spawn actor and get handle
//! let handle = HunkTrackerActor::spawn(
//!     session_id,
//!     working_dir,
//!     event_tx,
//!     TrackingMode::AllDirty,
//!     cancellation_token,
//! );
//!
//! // Record agent writes
//! handle.record_agent_write(path, content, prompt_index);
//!
//! // Query hunks
//! let hunks = handle.get_all_hunks().await;
//!
//! // Apply actions
//! handle.hunk_action(hunk_id, HunkAction::Accept).await;
//!
//! // Listen for events
//! while let Some(event) = event_rx.recv().await {
//!     match event {
//!         HunkEvent::HunkAdded { path, hunk } => { /* ... */ }
//!         HunkEvent::HunkRemoved { path, hunk_id } => { /* ... */ }
//!         _ => {}
//!     }
//! }
//! ```

pub mod actor;
pub mod commands;
pub mod diff;
pub mod events;
pub mod handle;
pub mod loc;
pub mod types;

// Re-export main types for convenience
pub use actor::{HunkTrackerActor, REFRESH_SCAN_LOG_PREFIX, REFRESH_SKIP_LOG_PREFIX};
pub use events::{HunkEvent, HunkRemovalReason};
pub use handle::HunkTrackerHandle;
pub use loc::{
    AuthorType, EventType, HunkRecord, HunkRecordWriter, JsonlHunkRecordWriter, LocAggregate,
    LocSinkContext, SourceType, run_loc_sink,
};
pub use types::*;
