//! Types for sandbox events, metrics, and profile configuration.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

/// A recorded sandbox event for telemetry and debugging.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxEvent {
    pub timestamp: DateTime<Utc>,
    pub event_type: SandboxEventType,
    pub profile: String,

    // Context fields — present on ProfileApplied/ApplyFailed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enforced: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restrict_network: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_write_paths: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_only_paths: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deny_paths: Option<Vec<String>>,

    // Violation/error fields
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl SandboxEvent {
    fn base(event_type: SandboxEventType, profile: &str) -> Self {
        Self {
            timestamp: Utc::now(),
            event_type,
            profile: profile.to_string(),
            workspace: None,
            platform: None,
            enforced: None,
            restrict_network: None,
            read_write_paths: None,
            read_only_paths: None,
            deny_paths: None,
            operation: None,
            target: None,
            command: None,
            tool_call_id: None,
            error: None,
        }
    }

    /// Create a "profile applied" event with full context.
    pub fn profile_applied(
        profile: &str,
        workspace: &std::path::Path,
        resolved: &crate::profiles::SandboxProfile,
    ) -> Self {
        let platform = if cfg!(target_os = "linux") {
            "linux/landlock"
        } else if cfg!(target_os = "macos") {
            "macos/seatbelt"
        } else {
            "unknown"
        };

        let mut event = Self::base(SandboxEventType::ProfileApplied, profile);
        event.workspace = Some(workspace.display().to_string());
        event.platform = Some(platform.to_string());
        event.enforced = Some(true);
        event.restrict_network = Some(resolved.restrict_network);
        event.read_write_paths = Some(
            resolved
                .read_write
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
        );
        if !resolved.read_only.is_empty() {
            event.read_only_paths = Some(
                resolved
                    .read_only
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect(),
            );
        }
        if !resolved.deny.is_empty() {
            event.deny_paths = Some(
                resolved
                    .deny
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect(),
            );
        }
        event
    }

    /// Create an "apply failed" event with context.
    pub fn apply_failed(
        profile: &str,
        workspace: &std::path::Path,
        error: &dyn std::fmt::Display,
    ) -> Self {
        let platform = if cfg!(target_os = "linux") {
            "linux/landlock"
        } else if cfg!(target_os = "macos") {
            "macos/seatbelt"
        } else {
            "unknown"
        };

        let mut event = Self::base(SandboxEventType::ApplyFailed, profile);
        event.workspace = Some(workspace.display().to_string());
        event.platform = Some(platform.to_string());
        event.enforced = Some(false);
        event.error = Some(error.to_string());
        event
    }

    /// Create a filesystem violation event.
    pub fn fs_violation(profile: &str, target: &str, operation: &str) -> Self {
        let mut event = Self::base(SandboxEventType::FsViolation, profile);
        event.operation = Some(operation.to_string());
        event.target = Some(target.to_string());
        event
    }

    /// Create a network violation event.
    pub fn net_violation(profile: &str, target: &str) -> Self {
        let mut event = Self::base(SandboxEventType::NetViolation, profile);
        event.operation = Some("connect".to_string());
        event.target = Some(target.to_string());
        event
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SandboxEventType {
    ProfileApplied,
    ApplyFailed,
    FsViolation,
    NetViolation,
    BypassGranted,
    BypassDenied,
}

/// Counters for sandbox activity, used for telemetry dashboards.
#[derive(Debug, Default)]
pub struct SandboxMetrics {
    pub fs_violations: AtomicU64,
    pub net_violations: AtomicU64,
    pub bypasses_granted: AtomicU64,
    pub bypasses_denied: AtomicU64,
}

impl SandboxMetrics {
    pub fn inc_fs_violation(&self) {
        self.fs_violations.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_net_violation(&self) {
        self.net_violations.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_bypass_granted(&self) {
        self.bypasses_granted.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_bypass_denied(&self) {
        self.bypasses_denied.fetch_add(1, Ordering::Relaxed);
    }

    pub fn fs_violation_count(&self) -> u64 {
        self.fs_violations.load(Ordering::Relaxed)
    }

    pub fn net_violation_count(&self) -> u64 {
        self.net_violations.load(Ordering::Relaxed)
    }
}
