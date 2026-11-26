/// Max characters per individual stdout line before truncation.
pub const LINE_TRUNCATION_LIMIT: usize = 500;

/// Max characters per batched event (multiple lines joined).
pub const BATCH_TRUNCATION_LIMIT: usize = 3_000;

/// Raw stdout buffer cap in bytes.
pub const BUFFER_CAP_BYTES: usize = 1_048_576; // 1 MB

/// Debounce window for batching concurrent stdout lines (ms).
pub const DEBOUNCE_MS: u64 = 200;

/// Token bucket capacity.
pub const RATE_LIMIT_CAPACITY: u32 = 10;

/// Token bucket refill interval in milliseconds.
pub const RATE_LIMIT_REFILL_MS: u64 = 2_000;

/// Auto-kill after this many ms of continuous rate-limit violations.
pub const AUTO_KILL_THRESHOLD_MS: u64 = 30_000;

/// Default monitor timeout (non-persistent). 10 hours to avoid short
/// unexpected cutoffs for monitors the model starts without an explicit deadline.
pub const DEFAULT_TIMEOUT_MS: u64 = 36_000_000; // 10 hours

/// Maximum monitor timeout.
pub const MAX_TIMEOUT_MS: u64 = 36_000_000; // 10 hours

/// Max result size for the tool_result response.
pub const MAX_RESULT_SIZE_CHARS: usize = 10_000;

fn default_timeout_ms() -> Option<u64> {
    Some(DEFAULT_TIMEOUT_MS)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct MonitorInput {
    /// Shell command or script. Each stdout line is an event; exit ends the watch.
    #[schemars(
        description = "Shell command or script. Each stdout line is an event; exit ends the watch."
    )]
    pub command: String,

    /// Short human-readable description of what you are monitoring (shown in every notification).
    #[schemars(
        description = "Short human-readable description of what you are monitoring (shown in every notification)."
    )]
    pub description: String,

    /// Kill the monitor after this deadline (ms). Ignored when persistent is true.
    /// Default: 36000000 (10 hr). Max: 36000000 (10 hr).
    #[serde(default = "default_timeout_ms")]
    #[schemars(
        description = "Kill the monitor after this deadline (ms). Default: 36000000 (10 hr). Max: 36000000 (10 hr).",
        default = "default_timeout_ms"
    )]
    pub timeout_ms: Option<u64>,

    /// Run for the lifetime of the session (no timeout). Use for session-length watches.
    /// Stop with kill_command_or_subagent.
    #[serde(
        default,
        deserialize_with = "crate::types::schema::deserialize_lenient_bool"
    )]
    #[schemars(
        description = "Run for the lifetime of the session (no timeout).${%- if tools.by_kind.kill_task_action %} Stop with ${{ tools.by_kind.kill_task_action }}.${%- endif %}"
    )]
    pub persistent: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MonitorOutput {
    /// ID of the background monitor task (used with kill_command_or_subagent to cancel).
    pub task_id: String,
    /// Timeout deadline in milliseconds. 0 when persistent.
    pub timeout_ms: u64,
    /// Whether the monitor runs until kill_command_or_subagent or session end.
    pub persistent: bool,
}

impl echo_agent_tool_runtime::ToolOutput for MonitorOutput {}

#[derive(thiserror::Error, Debug)]
pub enum MonitorError {
    #[error("persistent must be true when timeout_ms exceeds {MAX_TIMEOUT_MS}ms")]
    TimeoutExceedsMax,
}

impl MonitorInput {
    /// Validate input constraints.
    pub fn validate(&self) -> Result<(), MonitorError> {
        let persistent = self.persistent;
        if let Some(timeout) = self.timeout_ms
            && !persistent
            && timeout > MAX_TIMEOUT_MS
        {
            return Err(MonitorError::TimeoutExceedsMax);
        }
        Ok(())
    }

    /// Resolved timeout in milliseconds (0 for persistent / no-deadline monitors).
    pub fn resolved_timeout_ms(&self) -> u64 {
        if self.persistent {
            0
        } else {
            self.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS)
        }
    }
}

// Mid-turn monitor event buffer

/// A monitor event notification to be surfaced as a `<system-reminder>` mid-turn.
#[derive(Debug, Clone)]
pub struct MonitorEventNotification {
    pub task_id: String,
    pub event_text: String,
    /// Session that owns the monitor which produced this event.
    ///
    /// In leader mode every session shares one [`MonitorEventBuffer`], so the
    /// drain sites filter on this to avoid surfacing one session's monitor
    /// events inside another session's turn. `None` for legacy / non-echo-agent-build
    /// backends, which any session drains for backwards compatibility.
    pub owner_session_id: Option<String>,
}

impl MonitorEventNotification {
    /// Whether this buffered event should surface in the session whose owner id
    /// is `my_owner`. Mirrors `task_owned_by_session`: an event surfaces only
    /// when it has no recorded owner (legacy) or its owner matches the draining
    /// session. Foreign events stay buffered for their own session to drain.
    pub fn owned_by_session(&self, my_owner: Option<&str>) -> bool {
        match (my_owner, self.owner_session_id.as_deref()) {
            (Some(me), Some(owner)) => me == owner,
            _ => true,
        }
    }
}

/// Shared buffer for mid-turn monitor event notifications: an [`EventQueue`]
/// of [`MonitorEventNotification`]. Producers `push_capped`; the turn loop
/// drains its session's events via [`drain_owned`].
///
/// [`EventQueue`]: echo_agent_interjection_core::EventQueue
pub type MonitorEventBuffer = echo_agent_interjection_core::EventQueue<MonitorEventNotification>;

crate::register_resource!("echo_agent_build", "MonitorEventBuffer", MonitorEventBuffer);

/// Drain only `my_owner`'s events (the buffer is shared across sessions in
/// leader mode); owner-less legacy events drain anywhere.
pub fn drain_owned(
    buffer: &MonitorEventBuffer,
    my_owner: Option<&str>,
) -> Vec<MonitorEventNotification> {
    buffer.drain_matching(|e| e.owned_by_session(my_owner))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_timeout_is_10h() {
        let input = MonitorInput {
            command: "tail -f log".into(),
            description: "watch log".into(),
            timeout_ms: None,
            persistent: false,
        };
        assert_eq!(input.resolved_timeout_ms(), DEFAULT_TIMEOUT_MS);
        assert!(input.validate().is_ok());
        assert_eq!(DEFAULT_TIMEOUT_MS, 36_000_000);
    }

    #[test]
    fn persistent_has_zero_timeout() {
        let input = MonitorInput {
            command: "tail -f log".into(),
            description: "watch log".into(),
            timeout_ms: None,
            persistent: true,
        };
        assert_eq!(input.resolved_timeout_ms(), 0);
        assert!(input.validate().is_ok());
    }

    #[test]
    fn explicit_timeout_within_max() {
        let input = MonitorInput {
            command: "cmd".into(),
            description: "desc".into(),
            timeout_ms: Some(600_000),
            persistent: false,
        };
        assert_eq!(input.resolved_timeout_ms(), 600_000);
        assert!(input.validate().is_ok());
    }

    #[test]
    fn timeout_exceeds_max_without_persistent_fails() {
        let input = MonitorInput {
            command: "cmd".into(),
            description: "desc".into(),
            timeout_ms: Some(MAX_TIMEOUT_MS + 1),
            persistent: false,
        };
        assert!(input.validate().is_err());
    }

    #[test]
    fn timeout_exceeds_max_with_persistent_ok() {
        let input = MonitorInput {
            command: "cmd".into(),
            description: "desc".into(),
            timeout_ms: Some(MAX_TIMEOUT_MS + 1),
            persistent: true,
        };
        assert!(input.validate().is_ok());
    }
}
