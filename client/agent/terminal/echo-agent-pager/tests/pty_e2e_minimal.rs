//! PTY coverage for experimental minimal mode and its native-scrollback path.
//!
//! All cases are ignored for ordinary Cargo runs; Bazel opts in and caps this
//! process-heavy family at four concurrent libtest workers.

// Shared support intentionally serves all PTY family crates.
#[allow(dead_code, unused_imports)]
#[path = "pty_e2e/common.rs"]
mod common;

#[path = "pty_e2e/ansi_scrollback_content_integrity.rs"]
mod ansi_scrollback_content_integrity;
#[path = "pty_e2e/minimal/mod.rs"]
mod minimal;
