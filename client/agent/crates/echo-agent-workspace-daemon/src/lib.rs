//! Process-lifecycle machinery for the workspace-server daemon.
//!
//! Two halves of one concern, both used only by the `echo-agent-workspace-server`
//! binary and never by the `echo-agent-workspace` library:
//!
//! - [`daemonize`] takes the process into its own session (double-fork +
//!   `setsid()` on Unix, stdio redirection on Windows) and holds the
//!   single-instance pidfile lock.
//! - [`preview_supervisor`] runs after that, supervising the in-sandbox
//!   preview-proxy child and scraping its loopback control endpoints.
//!
//! They share the daemon file-open posture (`O_NOFOLLOW` + mode `0600`) for
//! every daemon-owned file, which is why they live together here.
//!
//! This crate deliberately does not depend on `echo-agent-workspace`. The
//! activity scraper reports through the [`preview_supervisor::PreviewActivitySink`]
//! trait, which the binary implements over the workspace `ActivityTracker`.

pub mod daemonize;
pub mod preview_supervisor;
