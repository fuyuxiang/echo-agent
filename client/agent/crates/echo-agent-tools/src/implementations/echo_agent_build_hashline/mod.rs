//! `EchoAgentBuildHashline` namespace — hashline-anchored read/edit/search tools.
//!
//! This module provides the anchor engine used by the hashline toolset:
//! - [`AnchorScheme`] trait and implementations (Candidates A, B, C)
//! - Anchor parsing, rendering, and validation
//! - Bounded recovery helpers for shifted/stale anchors
//!
//! The hashline tools themselves (`hashline_read`, `hashline_edit`,
//! `hashline_grep`) build on the reusable core this module provides (also
//! used by the benchmark harness).

pub mod anchor;
pub mod benchmark;
pub mod config;
pub mod edit;
pub mod grep;
pub mod mutate;
pub mod read_file;
pub mod scheme;

pub use config::HashlineSchemeParams;
pub use edit::HashlineEditTool;
pub use grep::HashlineGrepTool;
pub use read_file::HashlineReadTool;
