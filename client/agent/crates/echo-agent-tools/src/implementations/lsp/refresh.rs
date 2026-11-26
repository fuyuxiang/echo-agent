//! When the server tells us its answers have changed.
//!
//! A pull-model server answers whatever it knows at the moment it is asked,
//! which after an edit — or during the first load of a large solution — may be
//! nothing yet. The protocol's answer to that is not for the client to guess
//! how long analysis takes, but for the server to say when it has finished:
//! `workspace/diagnostic/refresh` asks the client to re-pull everything, and
//! Roslyn sends it on re-analysis, project load and configuration changes.
//! Roslyn also has its own `workspace/projectInitializationComplete`, which is
//! what other clients use as the "the solution is really open now" signal.
//!
//! Both mean the same thing to us: ask again about everything we have open.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use async_lsp::lsp_types::notification::Notification;

use super::pull::PullDiagnostics;

/// Roslyn's notification that the solution has finished loading.
///
/// Not part of the specification, so it is spelled out here. Roslyn analyzes
/// nothing meaningful until this point, and its answers before it are worth
/// re-asking.
pub enum ProjectInitializationComplete {}

impl Notification for ProjectInitializationComplete {
    /// Roslyn sends `null`, older builds send an empty array; neither carries
    /// anything we need.
    type Params = serde_json::Value;
    const METHOD: &'static str = "workspace/projectInitializationComplete";
}

/// The pull handle, as seen by the router.
///
/// The router is built before the handshake, and the pull handle is only
/// complete after it, so the two meet here. A refresh that arrives before the
/// handshake finishes is dropped, which is right: there is nothing open to
/// re-pull yet, and the first pull of each document is about to happen anyway.
#[derive(Debug, Clone, Default)]
pub struct RefreshTarget {
    pull: Arc<OnceLock<PullDiagnostics>>,
    /// Set when the server has told us its answers are out of date, cleared by
    /// the manager once it has re-opened the questions they answered.
    invalidated: Arc<AtomicBool>,
}

impl RefreshTarget {
    pub fn new() -> Self {
        Self::default()
    }

    /// Hand the router the pull handle, once there is one.
    pub fn publish(&self, pull: PullDiagnostics) {
        if self.pull.set(pull).is_err() {
            tracing::debug!("refresh target already published");
        }
    }

    /// Throw away what the server has told us so far and ask again.
    ///
    /// Forgetting is the point. The server has said its previous answers no
    /// longer describe the code, and an answer that is known to be out of date
    /// is worse than none: presented as current it is a lie, and left in place
    /// it makes the re-pull look like it has already been answered.
    pub fn refresh_all(&self, server_name: &str, reason: &str) {
        let Some(pull) = self.pull.get() else {
            tracing::debug!(server = %server_name, reason, "diagnostics refresh before the handshake finished; nothing open to re-pull");
            return;
        };
        tracing::debug!(server = %server_name, reason, "re-pulling diagnostics for every open document");
        if pull.refresh_all() {
            self.invalidated.store(true, Ordering::Release);
        }
    }

    /// Whether a refresh has happened since this was last asked.
    ///
    /// Consuming: the caller owns re-opening the questions the refresh
    /// invalidated, and dropping the answer drops the refresh with it.
    ///
    /// The re-pull puts the answers back, but only someone waiting on a
    /// document ever gets told about them — so the questions have to be asked
    /// again too, and only the manager keeps those.
    #[must_use]
    pub fn take_invalidated(&self) -> bool {
        self.invalidated.swap(false, Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refresh_before_the_handshake_is_a_no_op() {
        let target = RefreshTarget::new();
        // No panic, no work: there is nothing open to re-pull yet, so there is
        // nothing to re-ask about either.
        target.refresh_all("test", "unit test");
        assert!(!target.take_invalidated());
    }

    #[test]
    fn the_invalidation_is_reported_once() {
        let target = RefreshTarget::new();
        target.invalidated.store(true, Ordering::Release);
        assert!(target.take_invalidated());
        assert!(
            !target.take_invalidated(),
            "a second reader must not re-open questions already re-opened"
        );
    }
}
