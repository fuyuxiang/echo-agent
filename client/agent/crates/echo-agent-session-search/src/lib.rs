//! Full-text search over local echoagent sessions, backed by a rebuildable SQLite
//! FTS5 cache under `<echoagent home>/sessions/session_search.sqlite`.
//!
//! Layering, bottom up:
//!
//! - [`fts`] owns the schema, the BM25 query, and the fenced `meta` writes.
//! - `recovery` classifies an unusable database file, quarantines it, and
//!   recreates an empty one; `db` is the open-and-retry plumbing on top.
//! - `doc` turns a session plus its extracted text into an index document.
//! - `bootstrap` runs the cross-process, lease-guarded full reindex.
//! - `manager` debounces per-session upserts and answers queries.
//!
//! The crate never reads the session store directly: a caller supplies a
//! [`SessionSource`] and a [`ContentExtractor`], so the `updates.jsonl` wire
//! format stays owned by the store rather than being duplicated here.

mod bootstrap;
mod db;
mod doc;
pub mod fts;
mod manager;
mod recovery;
mod source;

pub use manager::{
    SearchIndexManager, SearchIndexStatus, SessionSearchRequest, SessionSearchResponse,
    evict_session, execute_search,
};
pub use source::{ContentExtractor, IndexableSession, SessionSource, SessionSourceFactory};
