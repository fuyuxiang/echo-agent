#![allow(
    unused_imports,
    unused_variables,
    unused_mut,
    unreachable_code,
    dead_code
)]
//! Session-support modules extracted from `echo-agent-runtime`'s `session/` tree
//! (which re-exports them at their original paths) so they build in parallel
//! and stop rebuilding on shell edits.
pub mod managed_mcp;
