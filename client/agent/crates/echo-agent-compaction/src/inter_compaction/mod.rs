//! Inter-compaction — the chunked summarisation pipeline shared by both
//! `Basic` and `DivideAndConquer` strategies, generic over
//! [`CompactionItemBuilder`](crate::CompactionItemBuilder).
//!
//! Harness wiring (turn selection from the conversation store, raw-request
//! user-query extraction, summary-message assembly, persistence) stays
//! per-harness; the EchoAgent chat host wraps this pipeline.

pub mod compact;
pub mod config;
pub mod observer;

pub use compact::{ChunkedCompactionOutput, sample_compaction_chunked};
pub use config::InterCompactionConfig;
pub use observer::InterCompactionObserver;
