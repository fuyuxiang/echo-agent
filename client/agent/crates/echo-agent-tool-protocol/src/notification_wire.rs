//! Adjacent-tagged notification wire wrapper with a forward-compat
//! `Custom` shape.
//!
//! Adjacent tagging (`#[serde(tag = "shape", content = "value")]`) was
//! chosen over `#[serde(untagged)]` to eliminate the spoofing risk where
//! a `Custom` payload could silently match a known PascalCase variant.
//! The collision check ([`check_custom_kind`]) runs at
//! notification-emit time, not at registration time.
//!
//! Wire shape:
//!
//! ```jsonc
//! { "shape": "known",  "value": { "type": "BashOutputChunk", ... } }
//! { "shape": "custom", "value": { "kind": "my_tool.progress", "payload": ... } }
//! ```

use serde::{Deserialize, Serialize};

/// Adjacent-tagged notification wire wrapper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "shape", content = "value", rename_all = "snake_case")]
pub enum WireToolNotification {
    Known(serde_json::Value),
    Custom(WireCustomNotification),
}

/// Free-form notification payload for kinds the computer hub does not
/// recognise. The `kind` MUST NOT collide with a known PascalCase variant
/// (see [`check_custom_kind`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireCustomNotification {
    pub kind: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("custom notification kind {kind:?} collides with a known variant")]
pub struct KnownVariantCollision {
    pub kind: String,
}

/// PascalCase variant names of known notification types.
///
/// Source of truth lives upstream; keep this list in sync. The audit
/// test in `tests/notification_collision.rs` round-trips a representative
/// of every variant and asserts its `type` discriminator appears here, so
/// upstream additions cause a test failure rather than silent drift.
pub const KNOWN_NOTIFICATION_KINDS: &[&str] = &[
    "BashOutputChunk",
    "BashExecutionComplete",
    "BashExecutionTimeout",
    "BashExecutionBackgrounded",
    "BashExecutionFailed",
    "FileWritten",
    "TaskCompleted",
    "PlanModeEntered",
    "PlanModeExited",
    "UserQuestionAsked",
    "LspServerStarting",
    "LspServerReady",
    "LspServerCrashed",
    "LspServerRetrying",
    "LspServerFailed",
    "ScheduledTaskFired",
    "ScheduledTaskRemoved",
    "ScheduledTaskCreated",
    "MonitorEvent",
];

pub const fn known_notification_kinds() -> &'static [&'static str] {
    KNOWN_NOTIFICATION_KINDS
}

/// Reject custom notification kinds whose name shadows a known PascalCase
/// variant. Runs at notification-emit time; an empty `kind` is accepted
/// here (the producer is responsible for validating that the field is
/// non-empty).
pub fn check_custom_kind(kind: &str) -> Result<(), KnownVariantCollision> {
    if KNOWN_NOTIFICATION_KINDS.contains(&kind) {
        Err(KnownVariantCollision {
            kind: kind.to_owned(),
        })
    } else {
        Ok(())
    }
}
