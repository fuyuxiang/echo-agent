//! Shared utilities used by both `echo-agent-runtime` and its downstream clients
//! (e.g. `echo-agent-pager-render`). This crate sits upstream of `echo-agent-runtime`
//! so it must never depend on it.

pub mod clipboard;
pub mod placeholder_images;
pub mod session;
pub mod stderr;
pub mod ui_config;
