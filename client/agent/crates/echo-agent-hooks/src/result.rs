use std::time::Duration;

/// The outcome of a blocking (`pre_tool_use`) hook dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookDecision {
    Allow,
    Deny { reason: String, hook_name: String },
}

/// Parsed output of one `Stop`/`SubagentStop` gate hook. The dispatcher
/// aggregates these across hooks; `force_stop` overrides blocks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StopHookOutcome {
    pub block_reason: Option<String>,
    pub additional_context: Option<String>,
    pub force_stop: Option<StopOverride>,
}

/// A `continue: false` force-stop; `reason` is `stopReason`, shown to the user.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StopOverride {
    pub reason: Option<String>,
}

impl StopHookOutcome {
    pub fn is_empty(&self) -> bool {
        self.block_reason.is_none()
            && self.additional_context.is_none()
            && self.force_stop.is_none()
    }
}

/// HTTP execution details for `"http"` hooks, for scrollback enrichment.
#[derive(Debug, Clone)]
pub struct HttpInfo {
    /// Post-expansion target (for SSRF debugging). May contain secrets from
    /// resolved `${VAR}` substitutions, so user-facing display MUST prefer
    /// `raw_url` when present.
    pub url: String,
    /// Pre-expansion source URL as written in the file, safe for display.
    /// `None` when the spec was built without it (fall back to `url`).
    pub raw_url: Option<String>,
    pub status: Option<u16>,
    pub response_preview: Option<String>,
}

/// The outcome of a single hook execution.
#[derive(Debug)]
pub enum HookRunResult {
    Success {
        hook_name: String,
        elapsed: Duration,
        http_info: Option<HttpInfo>,
    },
    Skipped {
        hook_name: String,
    },
    /// Ran and blocked: a stop-gate decision, not a failure (distinct from `Failed`).
    Blocked {
        hook_name: String,
        detail: String,
        elapsed: Duration,
        http_info: Option<HttpInfo>,
    },
    /// Hook failed (timeout, crash, bad output): fail-open.
    Failed {
        hook_name: String,
        error: String,
        elapsed: Duration,
        http_info: Option<HttpInfo>,
    },
}
