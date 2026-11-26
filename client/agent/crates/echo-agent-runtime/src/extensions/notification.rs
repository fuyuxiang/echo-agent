use agent_client_protocol as acp;
use echo_agent_tools::types::TaskSnapshot;

use crate::session::feedback::FeedbackRequest as FeedbackRequestData;

pub use crate::session::goal_tracker::GoalClassifierVerdict;

/// Retained for wire backwards compatibility; always empty in the
/// simplified goal model (no deliverables).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GoalDeliverableInfo {
    pub id: u32,
    pub title: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WorkflowPhaseInfo {
    pub title: String,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WorkflowAgentInfo {
    pub agent_id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub state: String,
    #[serde(default)]
    pub tokens_used: u64,
    #[serde(default)]
    pub duration_ms: u64,
}

/// `_meta` key on rename fan-out (`SessionSummaryGenerated` + ACP
/// `SessionInfoUpdate`). Old clients ignore unknown meta.
pub const TITLE_IS_MANUAL_META_KEY: &str = "echo.agent/titleIsManual";

/// `_meta` object carried on a manual-rename fan-out.
pub fn title_is_manual_meta() -> serde_json::Value {
    serde_json::json!({ TITLE_IS_MANUAL_META_KEY: true })
}

/// `_meta` object carried on `/rename --auto` fan-out. Distinct from
/// *absent* meta (auto title — must not clobber `display_name`).
pub fn title_is_unpinned_meta() -> serde_json::Value {
    serde_json::json!({ TITLE_IS_MANUAL_META_KEY: false })
}

/// EchoAgent-specific session notification (parallel to acp::SessionNotification)
/// This wraps an EchoAgentSessionUpdate with session context for persistence and replay.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionNotification {
    /// The ID of the session this update pertains to.
    pub session_id: acp::SessionId,
    /// The actual update content.
    pub update: SessionUpdate,
    /// Extension point for implementations
    #[serde(skip_serializing_if = "Option::is_none", rename = "_meta")]
    pub meta: Option<serde_json::Value>,
}

/// Wire usage for ACP `_meta.usage` and `TurnCompleted.usage`.
///
/// # Wire contract (ACP vs headless)
///
/// | Surface | `input_tokens` / `inputTokens` | Cost |
/// |---------|--------------------------------|------|
/// | **ACP** (`PromptUsage`) | **Full** prompt sum (includes cache reads) | `costUsdTicks` (1e10 ticks = $1), scrubbed when partial/incomplete |
/// | **Headless** ([`project_result_usage`]) | **Uncached only** (`full − cache_read`) | Float `total_cost_usd` + exact `total_cost_usd_ticks`, only when complete |
/// | ACP `_meta` sibling fields | **Last model call only** (not whole-prompt) | — |
///
/// Trust cost only when present **and** not `usageIsIncomplete` **and** not
/// `costIsPartial`. Absence of cost means untrustworthy or unknown — not free.
///
/// Mixed headless shape is frozen for external-tool compatibility: snake_case
/// on totals (`usage.input_tokens`, `total_cost_usd`) and camelCase under
/// `modelUsage` (`inputTokens`, `costUSD`). Per-model rows are a reduced
/// schema (no reasoning/duration on the wire).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PromptUsage {
    #[serde(flatten)]
    pub totals: PromptUsageModel,
    #[serde(
        default,
        rename = "modelUsage",
        skip_serializing_if = "indexmap::IndexMap::is_empty"
    )]
    pub model_usage: indexmap::IndexMap<String, PromptUsageModel>,
    /// Main-agent loop rounds (same unit as `--max-turns`).
    #[serde(default, rename = "numTurns")]
    pub num_turns: u64,
    /// Bill may under-count (open subagents, usage not applied, or drain timeout).
    #[serde(
        default,
        rename = "usageIsIncomplete",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub usage_is_incomplete: bool,
}

impl PromptUsage {
    /// Project a ledger snapshot for the wire. Returns `Some` whenever
    /// `incomplete` is set — even if `ledger` is `None` — so the flag is never
    /// dropped by omission. Always scrubs untrustworthy costs.
    pub(crate) fn project_from_ledger(
        ledger: Option<&echo_agent_chat_state::UsageLedger>,
        incomplete: bool,
    ) -> Option<Self> {
        let mut usage = match ledger {
            Some(ledger) => {
                let mut usage = Self::from(ledger);
                if incomplete {
                    usage.usage_is_incomplete = true;
                }
                usage
            }
            None if incomplete => Self {
                usage_is_incomplete: true,
                ..Default::default()
            },
            None => return None,
        };
        usage.scrub_untrustworthy_costs();
        Some(usage)
    }

    /// Error-path attach: any open ledger is always incomplete (may under-count
    /// without a freeze drain). `may_undercount` only matters when the ledger is empty.
    pub(crate) fn for_error_path(
        ledger: Option<&echo_agent_chat_state::UsageLedger>,
        may_undercount: bool,
    ) -> Option<Self> {
        match (ledger, may_undercount) {
            (Some(l), _) => Self::project_from_ledger(Some(l), true),
            (None, true) => Self::project_from_ledger(None, true),
            (None, false) => None,
        }
    }

    /// Drop cost ticks when partial or incomplete so all wire surfaces fail closed.
    /// Incomplete bills clear ticks even when `cost_is_partial` is false.
    pub(crate) fn scrub_untrustworthy_costs(&mut self) {
        if !(self.usage_is_incomplete || self.totals.cost_is_partial) {
            return;
        }
        self.totals.cost_usd_ticks = None;
        for m in self.model_usage.values_mut() {
            m.cost_usd_ticks = None;
            if self.totals.cost_is_partial {
                m.cost_is_partial = true;
            }
        }
    }

    fn is_token_empty(&self) -> bool {
        // Exhaustive destructure: a new token field must decide whether it
        // counts as "billed something" here.
        let PromptUsageModel {
            input_tokens,
            output_tokens,
            total_tokens: _, // derived from input + output
            cached_read_tokens,
            cache_creation_tokens, // subset of input_tokens on the wire
            reasoning_tokens: _,   // subset of output_tokens
            model_calls,
            api_duration_ms: _, // timing, not tokens
            cost_usd_ticks: _,  // cost without usage cannot occur
            cost_is_partial: _,
            cost_missing_calls: _,
        } = self.totals;
        model_calls == 0
            && input_tokens == 0
            && output_tokens == 0
            && cached_read_tokens == 0
            && cache_creation_tokens == 0
            && self.model_usage.is_empty()
    }
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptUsageModel {
    /// Full prompt input tokens including cache reads (ACP identity).
    /// Headless projects uncached only — see [`project_result_usage`].
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub cached_read_tokens: u64,
    /// Cache-creation prompt tokens, folded into `input_tokens` on the ACP wire
    /// but projected as a disjoint bucket in the headless shape.
    #[serde(default)]
    pub cache_creation_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
    #[serde(default)]
    pub model_calls: u64,
    #[serde(default)]
    pub api_duration_ms: u64,
    /// Server cost in USD ticks (`USD_TICKS_PER_USD` = 1e10 ticks per $1).
    /// Absent when scrubbed, missing, or zero on the wire. Headless projects
    /// the totals as float `total_cost_usd` (plus exact `total_cost_usd_ticks`)
    /// and per-model rows as float `costUSD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd_ticks: Option<i64>,
    /// Some folded calls lacked cost, so any cost shown is a partial sum.
    /// After a scrub of a partial bill, complete per-model rows are also
    /// stamped `true`: the flag means "do not trust this row's cost", not
    /// "this row's own cost was partial".
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cost_is_partial: bool,
    /// How many calls reported usage but no cost. Internal accounting for
    /// `cost_is_partial` only — never on the public ACP wire.
    #[serde(default, skip_serializing)]
    pub cost_missing_calls: u64,
}

/// One model call's token usage: the four Messages API `message.usage` fields
/// (`input_tokens` is the uncached prompt portion) plus `reasoning_tokens`.
/// Distinct from [`PromptUsageModel`], which sums the whole prompt.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResponseUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
}

impl From<&echo_agent_chat_state::UsageTotals> for PromptUsageModel {
    fn from(t: &echo_agent_chat_state::UsageTotals) -> Self {
        // Exhaustive destructure: a new ledger field cannot silently miss the
        // wire. When one is added here, also extend `project_result_usage`.
        let echo_agent_chat_state::UsageTotals {
            input_tokens,
            output_tokens,
            cached_read_tokens,
            cache_creation_tokens,
            reasoning_tokens,
            model_calls,
            api_duration_ms,
            cost_usd_ticks,
            cost_missing_calls,
        } = *t;
        Self {
            input_tokens,
            output_tokens,
            total_tokens: t.total_tokens(),
            cached_read_tokens,
            cache_creation_tokens,
            reasoning_tokens,
            model_calls,
            api_duration_ms,
            cost_usd_ticks,
            cost_is_partial: t.cost_is_partial(),
            cost_missing_calls,
        }
    }
}

impl From<&echo_agent_chat_state::UsageLedger> for PromptUsage {
    fn from(ledger: &echo_agent_chat_state::UsageLedger) -> Self {
        let mut usage = Self {
            totals: PromptUsageModel::from(&ledger.totals),
            model_usage: ledger
                .by_model
                .iter()
                .map(|(k, v)| (k.clone(), PromptUsageModel::from(v)))
                .collect(),
            num_turns: ledger.main_loop_model_calls,
            usage_is_incomplete: ledger.incomplete,
        };
        usage.scrub_untrustworthy_costs();
        usage
    }
}

/// Server cost scale: 1 USD = 10^10 ticks. ACP exposes ticks; headless converts to float USD.
pub const USD_TICKS_PER_USD: f64 = 1e10;

/// Convert server cost ticks to float USD (headless only).
pub fn ticks_to_usd(ticks: i64) -> f64 {
    ticks as f64 / USD_TICKS_PER_USD
}

/// Full ACP input → headless uncached input (`full − cache_read`).
pub(crate) fn uncached_input_tokens(full_input: u64, cached_read: u64) -> u64 {
    full_input.saturating_sub(cached_read)
}

/// Project usage onto a headless result object.
///
/// - `usage.input_tokens` = uncached (`full − cache_read − cache_creation`), so
///   the three prompt buckets are disjoint; identity
///   `input_tokens + cache_read + cache_creation + output = total_tokens`.
/// - Omits all cost floats when partial or incomplete (absence ≠ free).
/// - Incomplete with no tokens emits only `usage_is_incomplete` (no zero usage object).
/// - `modelUsage` rows are a reduced external-compat schema (camelCase; no reasoning/duration).
pub(crate) fn project_result_usage(result: &mut serde_json::Value, usage: &PromptUsage) {
    if usage.usage_is_incomplete && usage.is_token_empty() {
        result["usage_is_incomplete"] = true.into();
        return;
    }

    // Exhaustive destructure: a new wire field is a compile error until it is
    // either projected or named as deliberately dropped from the headless shape.
    let PromptUsageModel {
        input_tokens,
        output_tokens,
        total_tokens,
        cached_read_tokens,
        cache_creation_tokens,
        reasoning_tokens,
        model_calls: _,     // totals-level; headless carries num_turns instead
        api_duration_ms: _, // dropped: not part of the frozen headless shape
        cost_usd_ticks,
        cost_is_partial,
        cost_missing_calls: _, // internal partiality count; the flag suffices
    } = usage.totals;
    result["usage"] = serde_json::json!({
        "input_tokens": uncached_input_tokens(input_tokens, cached_read_tokens)
            .saturating_sub(cache_creation_tokens),
        "cache_read_input_tokens": cached_read_tokens,
        "cache_creation_input_tokens": cache_creation_tokens,
        "output_tokens": output_tokens,
        "reasoning_tokens": reasoning_tokens,
        "total_tokens": total_tokens,
    });
    result["num_turns"] = usage.num_turns.into();
    if usage.usage_is_incomplete {
        result["usage_is_incomplete"] = true.into();
    }
    let hide_costs = cost_is_partial || usage.usage_is_incomplete;
    if hide_costs {
        if cost_is_partial {
            result["cost_is_partial"] = true.into();
        }
    } else if let Some(ticks) = cost_usd_ticks {
        result["total_cost_usd"] = serde_json::json!(ticks_to_usd(ticks));
        // Exact integer ticks beside the float, under the same trust gate:
        // reconciliation sums ticks exactly, which floats cannot guarantee.
        result["total_cost_usd_ticks"] = serde_json::json!(ticks);
    }
    if !usage.model_usage.is_empty() {
        let mut model_usage = serde_json::Map::new();
        for (name, m) in &usage.model_usage {
            let PromptUsageModel {
                input_tokens,
                output_tokens,
                total_tokens: _, // derivable per row
                cached_read_tokens,
                cache_creation_tokens,
                reasoning_tokens: _, // dropped: reduced per-model schema
                model_calls,
                api_duration_ms: _, // dropped: reduced per-model schema
                cost_usd_ticks,
                cost_is_partial,
                cost_missing_calls: _,
            } = *m;
            let mut entry = serde_json::json!({
                "inputTokens": uncached_input_tokens(input_tokens, cached_read_tokens)
                    .saturating_sub(cache_creation_tokens),
                "outputTokens": output_tokens,
                "cacheReadInputTokens": cached_read_tokens,
                "cacheCreationInputTokens": cache_creation_tokens,
                "modelCalls": model_calls,
            });
            if !hide_costs
                && let Some(ticks) = cost_usd_ticks
                && !cost_is_partial
            {
                entry["costUSD"] = serde_json::json!(ticks_to_usd(ticks));
            }
            model_usage.insert(name.clone(), entry);
        }
        result["modelUsage"] = model_usage.into();
    }
}

/// Fail-closed attach for headless results: parse failure becomes
/// `usage_is_incomplete` (never omit silently — absence must not look free).
pub fn attach_result_usage_fail_closed(result: &mut serde_json::Value, usage: &serde_json::Value) {
    match serde_json::from_value::<PromptUsage>(usage.clone()) {
        Ok(parsed) => project_result_usage(result, &parsed),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "headless: _meta.usage failed to parse; marking usage_is_incomplete"
            );
            result["usage_is_incomplete"] = true.into();
        }
    }
}

/// Status of a single hook run (wire format).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", tag = "status")]
pub enum HookRunStatusDto {
    Success {
        elapsed_ms: u64,
    },
    Skipped,
    Failed {
        error: String,
        elapsed_ms: u64,
        /// Stop-gate block (the hook's decision, not a failure). Rides `failed`
        /// so old pagers keep rendering it. TODO: promote to a dedicated status.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        blocked: bool,
    },
}

/// A single hook run entry (wire format).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HookRunEntryDto {
    pub name: String,
    pub status: HookRunStatusDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

/// Why auto-compaction stopped before completing.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    strum::Display,
    strum::EnumString,
    strum::AsRefStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum AutoCompactCancelReason {
    UserCancelled,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "sessionUpdate")]
pub enum SessionUpdate {
    /// A diff review request containing one or more file diffs for user review.
    DiffReview {
        /// The diff content to be reviewed.
        content: Vec<DiffContent>,
    },
    /// Notification that a retry is in progress due to a transient error.
    RetryState(RetryState),
    /// Auto-compact is starting due to context window threshold
    AutoCompactStarted {
        /// Current token usage
        tokens_used: u64,
        /// Total context window size
        context_window: u64,
        /// Percentage used (e.g., 82)
        percentage: u8,
        /// Reason for compaction
        reason: String,
    },
    /// Auto-compact completed successfully
    AutoCompactCompleted {
        /// Tokens used before compaction. `None` on payloads from older shells.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tokens_before: Option<u64>,
        /// Tokens used after compaction
        tokens_after: u64,
        /// How long the compaction took (milliseconds)
        #[serde(skip_serializing_if = "Option::is_none")]
        elapsed_ms: Option<i64>,
        /// Summary preview (first ~100 chars of summary)
        summary_preview: Option<String>,
    },
    /// Auto-compact failed
    AutoCompactFailed {
        /// Error message
        error: String,
    },
    /// Memory flush is starting before compaction
    MemoryFlushStarted,
    /// Memory flush completed
    MemoryFlushCompleted {
        /// Outcome description
        result: String,
        /// Path to the written memory file (if any)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    /// Local references to the memory sources included in a specific model turn.
    MemoryContextUsed { sources: Vec<String> },
    /// Memory dream consolidation completed
    MemoryDreamCompleted {
        /// Outcome description
        result: String,
        /// Path to the written memory file (if any)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    /// Session-end memory save completed
    MemorySessionSaved {
        /// Path to the written session log
        path: String,
    },
    /// Auto-compact was cancelled (user pressed Ctrl+C)
    AutoCompactCancelled {
        /// Reason for cancellation
        reason: AutoCompactCancelReason,
    },
    /// Auto-continue completed after compaction
    /// This signals the TUI to flush pending agent messages and end the turn
    AutoContinueCompleted {
        /// Total tokens used after auto-continue
        total_tokens: u64,
    },
    /// Request for user feedback based on session heuristics
    FeedbackRequest(FeedbackRequestNotification),
    /// Relay sync status update (connected, disconnected, etc.)
    RelaySyncStatus(RelaySyncStatus),
    /// Auto-recovery is starting after a prompt failure (e.g. remote/workspace recovery)
    AutoRecoveryStarted {
        /// Current recovery attempt number (1-indexed)
        attempt: u32,
        /// Maximum number of recovery attempts allowed
        max_retries: u32,
        /// The error that triggered recovery
        error: String,
        /// Delay in milliseconds before the retry
        delay_ms: u64,
    },
    /// Auto-recovery exhausted all retries and the turn is failing
    AutoRecoveryExhausted {
        /// Total attempts made
        attempts: u32,
        /// The final error message
        error: String,
    },
    /// A hook annotation message for the TUI scrollback.
    /// Rendered inline with the preceding tool call block.
    HookAnnotation {
        /// The hook message to display (e.g., "🪝 Running post_tool_use hooks for `Edit`...")
        message: String,
    },
    /// Structured hook execution data attached to tool call blocks.
    HookExecution {
        /// The hook event name ("pre_tool_use" or "post_tool_use").
        event_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
        /// Keeps a delayed turn-end batch off the wrong turn's marker.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt_id: Option<String>,
        runs: Vec<HookRunEntryDto>,
    },
    /// Hooks registry changed (after reload or trust/untrust).
    /// Sent so the pager modal can auto-refresh if open.
    HooksChanged {
        hooks: Vec<echo_agent_hooks_plugins_types::HookInfo>,
        project_trusted: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        load_errors: Vec<String>,
    },
    /// Plugins registry changed (after reload).
    /// Sent so the pager modal can auto-refresh if open.
    PluginsChanged {
        plugins: Vec<echo_agent_hooks_plugins_types::PluginInfo>,
    },
    /// Marketplace plugin updates were auto-installed on session start.
    /// Sent so desktop/pager can show a notification to the user.
    PluginUpdatesInstalled {
        /// List of (plugin_name, old_version, new_version).
        updates: Vec<(String, String, String)>,
    },
    /// Status snapshot for client status lines. Send-only: never persisted,
    /// since the next emit supersedes it.
    SessionStatus(Box<echo_agent_status_line::StatusLineContext>),
    /// Session summary was generated for a new session.
    /// Sent after the first user prompt when the LLM generates a title.
    SessionSummaryGenerated {
        /// The generated session summary/title
        session_summary: String,
    },
    /// A short "where was I" recap of the session so far.
    ///
    /// Emitted by the `echo.agent/recap` ext method: on demand via the `/recap`
    /// slash command (`auto = false`), or automatically when the user
    /// returns to the terminal after being away (`auto = true`). The pager
    /// renders it as an informational scrollback line; it is never added to
    /// the model conversation.
    SessionRecap {
        /// The one-line recap text (~25–40 words; capped at a generous safety
        /// limit, so a normal recap is shown in full).
        summary: String,
        /// `true` when generated automatically on return-from-away,
        /// `false` for an explicit `/recap`.
        #[serde(default)]
        auto: bool,
    },
    /// A manual `/recap` produced no recap — no assistant turns yet, a failed
    /// prepare/model call, or an empty summary. The pager shows a loading
    /// spinner for `/recap`, so without this signal that spinner would animate
    /// forever; on receipt the pager clears it. Never emitted for an automatic
    /// recap (those show no spinner).
    SessionRecapUnavailable,
    /// Ultra-short summary of the just-finished successful turn, generated at
    /// turn end for the dashboard row's secondary line. Rows show it until
    /// the next successful turn's summary replaces it.
    ///
    /// Transient (never persisted to `updates.jsonl`): the durable copy lives
    /// in `summary.json` and reaches non-attached clients via the roster.
    /// Clients may apply deliveries directly — generation is serialized
    /// shell-side (one in-flight call, aborted by newer turns) and gateway
    /// delivery is ordered, so the latest delivery is the latest summary.
    LastTurnSummary {
        /// One-line fragment (~5–12 words, capped at a safety limit).
        summary: String,
        /// Prompt id of the turn this summary describes (provenance; also
        /// persisted as `Summary::last_turn_summary_prompt_id`).
        #[serde(default)]
        prompt_id: Option<String>,
    },
    /// A compaction checkpoint marker written to `updates.jsonl`.
    ///
    /// This is **persist-only** — it is never sent to the gateway/UI. It records
    /// that a compaction occurred so the replay pipeline can reconstruct the
    /// model's conversation view when rewinding across the compaction boundary.
    ///
    /// The actual compacted conversation is stored in a separate file under
    /// `compaction_checkpoints/{checkpoint_id}.json` to keep `updates.jsonl` lean.
    CompactionCheckpoint(Box<CompactionCheckpointInfo>),
    /// A rewind marker written to `updates.jsonl` when a rewind occurs.
    ///
    /// This is **persist-only** — it is never sent to the gateway/UI. Because
    /// `updates.jsonl` is append-only, rewinding creates a timeline branch.
    /// The marker tells the replay algorithm to discard accumulated state
    /// beyond `target_prompt_index` and continue from that point.
    RewindMarker {
        /// The prompt index being rewound to (0-based).
        target_prompt_index: usize,
        /// When the rewind occurred.
        created_at: String,
    },
    /// Task completed notification
    TaskCompleted {
        task_snapshot: TaskSnapshot,
        /// Advisory: an auto-wake prompt follows this completion. The
        /// first-party TUI no longer consumes it (remaining background work
        /// is surfaced by its persistent "watching" status row); kept for
        /// wire compatibility and other clients. Missing reads as `false`.
        #[serde(default)]
        will_wake: bool,
    },
    /// A subagent session has been spawned.
    ///
    /// Sent on the PARENT session's notification channel so the client
    /// knows this `child_session_id` is a subagent and can route its events.
    /// Emitted BEFORE dispatching `SessionCommand::Prompt` to the child,
    /// preventing a race where child events arrive before the client has
    /// the session ID mapping.
    SubagentSpawned {
        /// Unique subagent identifier (same as child session ID).
        subagent_id: String,
        /// The parent session that spawned this subagent.
        parent_session_id: String,
        /// The parent prompt/turn that spawned this subagent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_prompt_id: Option<String>,
        /// The child session's ACP session ID.
        child_session_id: String,
        /// Agent type used for the subagent ("general-purpose", "explore", "plan", or custom).
        subagent_type: String,
        /// Short human-readable description of the task.
        description: String,
        /// Effective context source after bootstrap: "new" or "resumed".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effective_context_source: Option<String>,
        /// Whether the forked context was normalized into <background_context>.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        context_normalized: bool,
        /// Capability mode applied to this subagent (e.g. "read-only").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        capability_mode: Option<String>,
        /// Named persona applied to this subagent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        persona: Option<String>,
        /// Role that supplied defaults for this subagent (e.g. "researcher").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role: Option<String>,
        /// Effective model ID used by the subagent (may differ from the parent).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// ID of the source subagent this session was resumed from.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resumed_from: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workflow_run_id: Option<String>,
    },
    /// Periodic progress update for a running subagent.
    ///
    /// Sent on the PARENT session's notification channel at a rate-limited
    /// cadence (every ~2s while the subagent is active). Stops automatically
    /// when the subagent completes or is cancelled. The TUI merges these
    /// into the same state path used by ACP poll responses.
    SubagentProgress {
        /// Unique subagent identifier.
        subagent_id: String,
        /// The parent session that owns this subagent.
        parent_session_id: String,
        /// The child session's ACP session ID.
        child_session_id: String,
        /// Elapsed wall-clock time in milliseconds.
        duration_ms: u64,
        /// Number of completed turns so far.
        turn_count: u32,
        /// Total tool calls executed so far.
        tool_call_count: u32,
        /// Current tokens used in the context window.
        tokens_used: u64,
        /// Total context window capacity (tokens).
        context_window_tokens: u64,
        /// Context window usage as a percentage (0-100).
        context_usage_pct: u8,
        /// Distinct tool names called so far.
        tools_used: Vec<String>,
        /// Number of errors encountered so far.
        error_count: u32,
    },
    /// A subagent session has finished (success, failure, or cancellation).
    ///
    /// Sent on the PARENT session's notification channel.
    SubagentFinished {
        /// Unique subagent identifier.
        subagent_id: String,
        /// The child session's ACP session ID.
        child_session_id: String,
        /// Outcome: "completed", "failed", or "cancelled".
        status: String,
        /// Error message if the subagent failed.
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        /// Number of tool calls made by the subagent.
        tool_calls: u32,
        /// Number of conversation turns taken by the subagent.
        turns: u32,
        /// Total wall-clock duration in milliseconds.
        duration_ms: u64,
        /// Total tokens consumed by the subagent's context window.
        #[serde(default)]
        tokens_used: u64,
        /// Final output text from the subagent (if completed).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        /// Advisory: an auto-wake prompt follows this completion. The
        /// first-party TUI no longer consumes it (remaining background work
        /// is surfaced by its persistent "watching" status row); kept for
        /// wire compatibility and other clients. Missing reads as `false`.
        #[serde(default)]
        will_wake: bool,
    },
    /// Task backgrounded notification — a bash command transitioned to background execution.
    /// Sent for both direct `is_background=true` tasks and foreground→background transitions.
    TaskBackgrounded {
        /// The tool_call_id of the bash tool invocation.
        tool_call_id: String,
        /// The background task registry ID.
        task_id: String,
        /// The shell command being executed.
        command: String,
        /// Absolute path of the working directory.
        cwd: String,
        /// Absolute path to the output log file on disk.
        output_file: String,
        /// For monitor tasks: the monitor's human-readable description.
        /// `None` for ordinary backgrounded bash commands. Lets the pager
        /// render monitors with a "Monitor" tag instead of bash-highlighting
        /// the command string.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        monitor_description: Option<String>,
        /// Model-supplied tool `description` for ordinary bash bg tasks
        /// (e.g. "Wait for the server to start"). Prefer over raw `command`
        /// in the pager "Task started" line / tasks pane. `None` when omitted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    ScheduledTaskCreated {
        task_id: String,
        prompt: String,
        human_schedule: String,
        next_fire_at: Option<String>,
    },
    ScheduledTaskFired {
        task_id: String,
        prompt: String,
        human_schedule: String,
        next_fire_at: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_id: Option<String>,
    },
    /// A scheduled task was deleted/cancelled.
    ScheduledTaskDeleted {
        task_id: String,
        /// `Unknown` on rows persisted before the reason field existed.
        #[serde(default)]
        reason: echo_agent_tools::notification::ScheduledTaskRemovedReason,
    },
    /// A monitor event (stdout line from a monitor background process).
    MonitorEvent {
        task_id: String,
        description: String,
        /// Raw event text (NOT XML-wrapped -- for pager stdout display).
        event_text: String,
    },
    /// The session's model was auto-switched because the persisted model
    /// is no longer available for this user.
    ModelAutoSwitched {
        /// The model ID that was persisted in the session but is no longer available.
        previous_model_id: String,
        /// The model ID that was selected as a replacement.
        new_model_id: String,
        /// Human-readable reason for the switch.
        reason: String,
    },
    /// The session's model was switched via `session/setModel`.
    ///
    /// Broadcast to every client subscribed to the session in leader mode so
    /// follower clients (TUI / IDE / web) mirror the change in their local
    /// state — status bar, `/model` dropdown, prompt header, etc. The
    /// originating client also receives this (the leader broadcasts to all
    /// subscribers of the session) but skips applying it because its in-flight
    /// `SetSessionModel` response is the authority for its local state and
    /// drives the single "Switched to X" scrollback entry. Followers gate on
    /// their own `model_switch_pending` flag to distinguish "I'm waiting on
    /// my own switch" from "someone else's switch arrived."
    ModelChanged {
        /// The newly-selected model id (catalog key).
        model_id: String,
        /// Effective reasoning effort, post-resolution. `None` when the model
        /// does not support reasoning effort or no effort override was applied.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_effort: Option<String>,
    },
    /// Streaming chunk of a tool call's arguments.
    ///
    /// Behaves like `acp::SessionUpdate::AgentMessageChunk` /
    /// `AgentThoughtChunk`: flows through the replay buffer, gets merged
    /// with adjacent chunks for the same `tool_call_id`, and is debounced
    /// at the session's buffering interval.
    /// Only persisted as a full `acp::SessionUpdate::ToolCall`.
    ToolCallDeltaChunk {
        /// Stable model-provided id (e.g. `"call_abc"`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_call_id: Option<String>,
        /// Positional index assigned within the assistant tool calls.
        tool_index: u32,
        /// Tool name (e.g. `"search_replace"`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// Raw JSON-fragment string. NOT valid JSON in isolation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arguments_delta: Option<String>,
    },
    /// One or more prompt images were resized to fit within API limits.
    ImageCompressed {
        images: Vec<ImageCompressedEntry>,
        /// Human-readable summary for display.
        message: String,
    },
    /// Prompt images dropped before send (integrity / upscale-cap). The
    /// model is told via a system-reminder; this surfaces them to the UI.
    ImageDropped { notes: Vec<String> },
    /// Memory file listing for the pager's /memory modal.
    MemoryFiles { files: Vec<MemoryFileInfo> },
    WorkflowUpdated {
        run_id: String,
        #[serde(default)]
        revision: u64,
        name: String,
        objective: String,
        status: String,
        #[serde(default)]
        foreground: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        phases: Vec<WorkflowPhaseInfo>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current_phase: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_budget: Option<u64>,
        #[serde(default)]
        agents_used: u64,
        #[serde(default)]
        agents_reserved: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agents_remaining: Option<u64>,
        #[serde(default)]
        agent_usage_incomplete: bool,
        elapsed_ms: u64,
        #[serde(default)]
        active_agents: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current_agent_label: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        agents: Vec<WorkflowAgentInfo>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_event: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_event_detail: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_event_timestamp: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pause_message: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_summary: Option<String>,
    },
    /// Goal mode orchestration progress update.
    ///
    /// Sent on the parent session's notification channel at phase transitions
    /// and rate-limited from the progress handler (max 1/s). Fire-and-forget
    /// to pager — not actionable.
    GoalUpdated {
        goal_id: String,
        objective: String,
        /// `"active"`, `"user_paused"`, `"back_off_paused"`,
        /// `"no_progress_paused"`, `"infra_paused"`, `"blocked"`,
        /// `"budget_limited"`, `"complete"`, `"cleared"`.
        /// Legacy `"doom_loop_paused"` is accepted by pagers as user-paused.
        status: String,
        /// `"idle"`, `"planning"`, `"executing"`
        phase: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        token_budget: Option<i64>,
        #[serde(default)]
        tokens_used: i64,
        elapsed_ms: u64,
        total_deliverables: u32,
        completed_deliverables: u32,
        /// Wire compat: always `None` in the simplified goal model.
        /// Retained for cross-version compatibility with older pagers.
        #[serde(
            rename = "current_deliverable_idx",
            skip_serializing_if = "Option::is_none"
        )]
        current_deliverable_id: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        current_deliverable_title: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        current_subagent_role: Option<String>,
        total_worker_rounds: u32,
        total_verify_rounds: u32,
        #[serde(default)]
        token_baseline: i64,
        #[serde(default)]
        finished_subagent_tokens: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        live_subagent_tokens: Option<u64>,
        /// Per-model marginal-token breakdown `(model_id, tokens)`, sorted
        /// by tokens descending. The producer (`build_goal_updated`) only
        /// populates this when ≥2 distinct models appear; a single-model
        /// goal collapses to the single tokens line, so the field is empty
        /// (and omitted on the wire). The pager re-checks ≥2 as defence in
        /// depth.
        ///
        /// This is a live, active-subagent-window field (it mirrors
        /// `live_subagent_tokens` and is cleared on `SubagentFinished`): the
        /// pager renders it only under the "Active subagent" block. The
        /// producer must therefore keep its populate gate on that same
        /// axis so the wire and render gates stay aligned.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        live_tokens_by_model: Vec<(String, u64)>,
        #[serde(skip_serializing_if = "Option::is_none")]
        live_context_pct: Option<u8>,
        #[serde(skip_serializing_if = "Option::is_none")]
        live_turn_count: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        live_tool_call_count: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        last_event: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        last_event_detail: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        last_event_timestamp: Option<String>,
        /// Wire compat: always empty in the simplified goal model.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        deliverables: Vec<GoalDeliverableInfo>,
        /// Human-readable explanation set when the goal entered a paused
        /// state with a meaningful reason (today only `"blocked"`).
        /// Rendered by the pager under the status row in the goal modal.
        /// Invariant: `Some` iff `status` is a paused-variant string AND
        /// the underlying pause was created via the message-carrying
        /// path. The shell clears this on every transition out of a
        /// paused state (resume / complete / budget_limit); the pager
        /// also gates rendering on `is_paused()` as a defence in depth.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pause_message: Option<String>,
        /// Number of times the goal-achievement classifier has run for
        /// this goal. `None` when no classifier run has occurred yet
        /// (matches the `total_worker_rounds`-style convention of
        /// suppressing the field when the counter is zero so old pagers
        /// don't see a stray zero).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        classifier_runs_attempted: Option<u32>,
        /// Hard cap on classifier runs for this goal. `None` when not
        /// configured.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        classifier_max_runs: Option<u32>,
        /// Last aggregate verdict returned by the verification stage, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_classifier_verdict: Option<GoalClassifierVerdict>,
        /// Filesystem path to the most recent verification-stage details artifact.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_classifier_details_path: Option<String>,
        /// `Some(true)` while a classifier run is in flight. Set only by
        /// the dedicated "verifying" notification path — `build_goal_updated`
        /// always emits `None` because this flag is not persisted state.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        verifying_completion: Option<bool>,
        /// `Some(true)` while the goal planner subagent is running. Set
        /// only by the dedicated "planning" notification path —
        /// `build_goal_updated` always emits `None` because this flag is
        /// not persisted state.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        planning: Option<bool>,
    },
    /// A blocking reverse-request (permission / `ask_user_question` /
    /// plan-approval) is now **pending** on the agent, keyed by `tool_call_id`
    /// Fire-and-forget, **never persisted** — it is a request,
    /// not a notification. Subscribers show ⏳ NeedsInput for this session.
    PendingInteraction {
        tool_call_id: String,
        kind: crate::session::pending_interaction::PendingKind,
    },
    /// A previously-pending reverse-request **resolved** (answered, cancelled,
    /// or errored). Fire-and-forget, **never persisted**. Subscribers clear the
    /// pending ⏳ for this `tool_call_id`.
    InteractionResolved { tool_call_id: String },
    /// The durable, replayable signal that a turn reached its terminal
    /// outcome. Rides the persisted `_echo.agent/session/update` rail (unlike the
    /// fire-and-forget `echo.agent/session/prompt_complete` notification), so a
    /// viewer that re-attaches mid-turn can finalize the turn from replay
    /// instead of staying stuck on "Waiting…".
    TurnCompleted {
        /// Correlation key the re-attaching viewer finalizes the turn on:
        /// the prompt/turn whose terminal outcome this carries.
        prompt_id: String,
        /// Why the turn ended (the model's stop reason, or e.g. "cancelled").
        stop_reason: String,
        /// Final agent result text, when the turn produced one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_result: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<PromptUsage>,
    },
    /// One model response opened (Messages `message_start`), carrying the real
    /// message id, model, and input-side token counts. Rides the buffered chunk
    /// rail so it is ordered AHEAD of this response's agent chunks: headless
    /// partial-mode framing consumes it to emit the real `message_start` id and
    /// input usage instead of a synthesized placeholder / zero-seeded usage.
    /// Messages backend only; other backends never emit it (the reducer keeps
    /// its placeholder fallback there).
    ///
    /// `input_tokens` is the uncached prompt portion; `cache_read_input_tokens`
    /// and `cache_creation_input_tokens` are the separate prompt-side cache
    /// buckets, both known at `message_start` on the Messages backend.
    ResponseStarted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default)]
        input_tokens: u64,
        #[serde(default)]
        cache_read_input_tokens: u64,
        #[serde(default)]
        cache_creation_input_tokens: u64,
    },
    /// This response's reasoning (thinking) block finished; carries its
    /// encrypted signature. Rides the buffered chunk rail so it is ordered right
    /// AFTER this response's thought chunks (and before its text): headless
    /// partial-mode framing consumes it to emit `signature_delta` before the
    /// thinking block's `content_block_stop`, in order. Messages backend only.
    ReasoningCompleted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    /// One completed model response, so headless can emit a Messages API
    /// assistant frame per response. Ordered with the response's chunks; a tool
    /// loop emits several. The durable outcome rides `TurnCompleted`.
    ResponseCompleted {
        /// Provider message id (Messages `message.id`), when reported.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
        /// Verbatim wire stop reason (`end_turn`, `tool_use`, …), when reported.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stop_reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<ResponseUsage>,
        /// Reasoning signature (encrypted content) for this response's thinking.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
        /// The provider's matched stop sequence (Messages API
        /// `message.stop_sequence`), present only when the model stopped on a
        /// configured stop sequence; `None` otherwise. Headless
        /// `streaming-messages-json` stamps it onto the assistant frame.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stop_sequence: Option<String>,
    },
    /// Catch-all for unrecognized session update types.
    /// Allows forward/backward compatibility when variants are added or removed.
    /// All fields from the unrecognized variant are discarded during deserialization.
    #[serde(other)]
    Unknown,
}

/// Metadata for a single memory file, sent to the pager for the memory modal.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct MemoryFileInfo {
    pub path: String,
    /// `"global"`, `"workspace"`, or `"session"`.
    pub source: String,
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_epoch_secs: Option<u64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ImageCompressedEntry {
    pub index: usize,
    pub original_bytes: usize,
    pub compressed_bytes: usize,
    pub original_width: u32,
    pub original_height: u32,
    pub compressed_width: u32,
    pub compressed_height: u32,
}

impl From<&crate::session::image_normalize::ImageCompressionInfo> for ImageCompressedEntry {
    fn from(c: &crate::session::image_normalize::ImageCompressionInfo) -> Self {
        Self {
            index: c.index,
            original_bytes: c.original_bytes,
            compressed_bytes: c.compressed_bytes,
            original_width: c.original_width,
            original_height: c.original_height,
            compressed_width: c.compressed_width,
            compressed_height: c.compressed_height,
        }
    }
}

pub const DISK_FULL_ERROR_TYPE: &str = "disk_full";
pub const DISK_FULL_USER_MESSAGE: &str = "Out of disk space. Free some space and try again.";

/// State of a retry operation or error for visual feedback in the TUI
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum RetryState {
    /// A retry is in progress
    Retrying {
        /// Current retry attempt number (1-indexed)
        attempt: u32,
        /// Maximum number of retries allowed
        max_retries: u32,
        /// Human-readable reason for the retry
        reason: String,
    },
    /// All retries have been exhausted
    Exhausted {
        /// Total number of attempts made
        attempts: u32,
        /// Human-readable reason for the failure
        reason: String,
        /// True when the exhaustion was caused by an HTTP 429 rate limit.
        /// Clients use this to show a user-friendly upgrade message instead
        /// of the raw `reason` string.
        #[serde(default)]
        is_rate_limited: bool,
    },
    /// A non-retryable error occurred (e.g., auth error, invalid params)
    Failed {
        /// Category of the error (e.g., "auth", "invalid_params", "server")
        error_type: String,
        /// Human-readable error message
        message: String,
    },
}

/// Whether a terminal retry failure is a recoverable authentication error
/// (expired/invalid credentials, 401) that the user can fix by signing in
/// again. Drives the actionable re-auth banner.
///
/// `legacy_auth` is intentionally excluded: those failures carry their own
/// detailed migration guidance (`echoagent update` / `echoagent logout` / `echoagent login`)
/// in the message, so we surface that verbatim instead of the generic prompt.
///
/// `auth_transient` is excluded for the opposite reason: the shell emits it
/// only when the failure self-heals (see `AuthManager::requires_manual_reauth`)
/// and the message already says it recovers on its own — no `/login` banner.
pub fn is_reauthable_failure(error_type: Option<&str>, message: &str) -> bool {
    if matches!(error_type, Some("legacy_auth") | Some("auth_transient")) {
        return false;
    }
    error_type == Some("auth") || message.contains("Unauthorized (401)")
}

/// Status updates for relay sync (session sharing) feature.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", tag = "status")]
pub enum RelaySyncStatus {
    /// Successfully connected to relay, session is now shareable
    Connected {
        /// The URL where this session can be viewed
        share_url: String,
    },
    /// Disconnected from relay (will auto-reconnect)
    Disconnected,
    /// Reconnecting to relay
    Reconnecting {
        /// Current attempt number
        attempt: u32,
    },
}

/// A diff content item that serializes compatibly with `acp::ToolCallContent::Diff`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(tag = "type", rename = "diff")]
pub struct DiffContent {
    /// The diff details.
    #[serde(flatten)]
    pub diff: acp::Diff,
}

/// Notification requesting user feedback based on session heuristics.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct FeedbackRequestNotification {
    /// Unique ID for this feedback request
    pub request_id: String,
    /// The tier that triggered this request
    pub tier: String,
    /// Human-readable prompt to show the user
    pub prompt: String,
    /// Whether this is a non-intrusive/dismissible request
    pub dismissible: bool,
    /// Trigger type identifier (e.g., "tier1_engagement", "tier2_complex_recovery")
    pub trigger_type: String,
    /// The specific condition that was met (e.g., "turns >= 10 AND tool_calls >= 5 AND ...")
    pub trigger_condition: String,
    /// Human-readable explanation of what triggered this request with actual values
    pub trigger_reason: String,
    pub stars: bool,
    pub thumbs: bool,
    pub text: bool,
}

impl From<FeedbackRequestData> for FeedbackRequestNotification {
    fn from(data: FeedbackRequestData) -> Self {
        Self {
            request_id: data.request_id,
            tier: format!("{:?}", data.tier).to_lowercase(),
            stars: data.stars,
            thumbs: data.thumbs,
            text: data.text,
            prompt: data.prompt,
            dismissible: data.dismissible,
            trigger_type: data.trigger_type,
            trigger_condition: data.trigger_condition.condition.clone(),
            trigger_reason: data.trigger_condition.trigger_reason(),
        }
    }
}

// ── Compaction checkpoint types ────────────────────────────────────────

/// Metadata stored in `updates.jsonl` as a `CompactionCheckpoint` session update.
///
/// This is a lightweight reference; the full compacted conversation lives in a
/// separate file (`compaction_checkpoints/{checkpoint_id}.json`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct CompactionCheckpointInfo {
    /// Unique checkpoint identifier (UUID).
    pub checkpoint_id: String,
    /// The prompt index at the time compaction completed.
    /// The next real user prompt will receive this index.
    pub prompt_index_at_compaction: usize,
    /// Relative path to the checkpoint file inside the session directory
    /// (e.g., `"compaction_checkpoints/<uuid>.json"`).
    pub checkpoint_file: String,
    /// If this compaction was triggered by auto-compact, contains the
    /// auto-continue prompt that was injected after compaction.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_continue: Option<AutoContinueInfo>,
    /// Schema version for forward compatibility.
    pub schema_version: u32,
    /// ISO 8601 timestamp of when the checkpoint was created.
    pub created_at: String,
}

/// Information about the auto-continue prompt injected after auto-compaction.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct AutoContinueInfo {
    /// The exact text of the auto-continue prompt that was added as a user message.
    pub prompt_text: String,
}

/// The on-disk format for a compaction checkpoint file.
///
/// Stored at `{session_dir}/compaction_checkpoints/{checkpoint_id}.json`.
/// Contains the full compacted conversation history so the replay pipeline can
/// deterministically reconstruct the model's view without re-running compaction.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CompactionCheckpointFile {
    /// Unique checkpoint identifier (matches [`CompactionCheckpointInfo::checkpoint_id`]).
    pub checkpoint_id: String,
    /// The prompt index at the time compaction completed.
    pub prompt_index_at_compaction: usize,
    /// The exact compacted conversation used by the model.
    pub compacted_history: Vec<crate::sampling::ConversationItem>,
    /// Schema version for forward compatibility.
    pub schema_version: u32,
    /// ISO 8601 timestamp of when the checkpoint was created.
    pub created_at: String,
    /// The original User(user_info) text from before compaction.
    /// Used during cross-compaction rewind to restore the correct user_info
    /// that the model originally saw for pre-compaction turns, rather than
    /// the rebuilt user_info from the compacted conversation.
    /// `None` in older checkpoints (schema_version 1 without this field).
    #[serde(default)]
    pub original_user_info: Option<String>,
    /// File paths that were re-read and injected after compaction.
    /// Informational — for debugging and replay understanding.
    #[serde(default)]
    pub reread_file_paths: Vec<String>,
}

/// A compaction segment to persist under `compaction/segment_NNN.md`. Storage
/// assigns the resume-safe index and renders the markdown (it owns the index
/// the header/metadata embed), so the caller supplies the render inputs.
#[derive(Debug, Clone)]
pub struct CompactionSegmentFile {
    pub items: Vec<echo_agent_sampling_types::ConversationItem>,
    /// Curated summary, analysis tags already stripped.
    pub summary: String,
    pub detail: echo_agent_chat_state::CompactionDetail,
    /// ISO-8601, for the segment metadata.
    pub timestamp: String,
}

/// On-disk artifact capturing the exact compaction request sent to the model
/// plus the response (or final error) it produced.
///
/// Stored at `{session_dir}/compaction_requests/{request_id}.json`. Rides on
/// the post-turn session archive to cloud storage, where it can be downloaded for prompt
/// iteration — you get the exact `chat_history` that was sent, the prompt
/// variant, any `/compact <text>` user context, the model used, and the
/// resulting summary (or error). Replay the request locally to A/B test
/// alternate prompt wordings against the same input.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CompactionRequestFile {
    /// Schema version for forward compatibility.
    pub schema_version: u32,
    /// Unique artifact identifier (filename stem).
    /// Note: this is a per-artifact ID, not the model API's `x_echo_agent_req_id`
    /// (which is generated per-attempt inside the sampling layer).
    pub request_id: String,
    /// ISO 8601 timestamp of when the compaction call started.
    pub created_at: String,
    /// What kicked off the compaction: `"manual"` (user ran `/compact`) or `"auto"`.
    pub trigger: String,
    /// Which prompt template was used: `"short"` (concise self-summarization)
    /// or `"detailed"` (10-section structured prompt for echo-agent-build and similar agents).
    pub prompt_variant: String,
    /// The model id that ran the summarization.
    pub model: String,
    /// User-provided context from `/compact <text>`, if any.
    pub user_context: Option<String>,
    /// The full `ConversationItem` list sent to the model, with the
    /// summarization prompt already appended as the final user message.
    /// Replaying this against any model reproduces the exact request.
    pub chat_history: Vec<crate::sampling::ConversationItem>,
    /// Tool definitions attached to the request (same effective set as the
    /// turn loop, for prompt-prefix/KV-cache alignment). Empty for artifacts
    /// written before tools were attached. Backend-hosted tools are not
    /// recorded (not serializable; IC-side concept).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<crate::sampling::ToolSpec>,
    /// Generated summary text, on success. `None` if all retries failed.
    pub summary: Option<String>,
    /// Most recent error message captured during the retry loop, if any.
    ///
    /// `None` on a first-attempt success. May co-occur with `summary` when a
    /// transient failure was eventually retried successfully — the field then
    /// documents the recovered flake and `summary` carries the final result.
    /// On total failure (all retries exhausted, or a deterministic error)
    /// `summary` is `None` and this field carries the final error.
    pub error: Option<String>,
    /// Number of attempts the retry loop made before settling on the final outcome.
    pub attempts: u32,
    /// Per-attempt diagnostics (one per retry-loop iteration), in order —
    /// records each rejected/degraded attempt so retries aren't bumped
    /// invisibly. Empty on artifacts written before schema v2.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attempt_details: Vec<echo_agent_chat_state::compaction_utils::CompactionAttempt>,
}

/// On-disk artifact capturing the exact recap request sent to the model plus
/// the response (or final error) it produced.
///
/// Stored at `{session_dir}/recap_requests/{request_id}.json`. Rides on
/// the post-turn session archive to cloud storage (same path as compaction request
/// artifacts) so recap prompt / model garble can be replayed offline.
/// Recap never mutates the conversation; this file is the only durable
/// record of what was sent for `/recap` or auto return-from-away recap.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RecapRequestFile {
    /// Schema version for forward compatibility.
    pub schema_version: u32,
    /// Unique artifact identifier (filename stem). Distinct from the model
    /// API's `x_echo_agent_req_id` (also recorded below for proxy correlation).
    pub request_id: String,
    /// ISO 8601 timestamp of when the recap model call started.
    pub created_at: String,
    /// What kicked off the recap: `"manual"` (`/recap`) or `"auto"`
    /// (return-from-away).
    pub trigger: String,
    /// The model id used for the recap side-call.
    pub model: String,
    /// Sampling request id sent to the proxy (`echo-agent-recap-{uuid}`).
    pub x_echo_agent_req_id: String,
    /// Sampling conversation id (`recap-{uuid}`).
    pub x_echo_agent_conv_id: String,
    /// Whether the side-call requested reasoning/thinking removal before
    /// budgeting. The over-budget path removes reasoning independently.
    pub strip_reasoning: bool,
    /// Reminder tag used in the recap instruction (`system-reminder` or
    /// the alternate `system_reminder` form).
    pub reminder_tag: String,
    /// The full `ConversationItem` list sent to the model, with the recap
    /// instruction already appended as the final user message. Replaying
    /// this against any model reproduces the exact request.
    pub chat_history: Vec<crate::sampling::ConversationItem>,
    /// Cleaned one-line recap body shown to the user, on success.
    /// `None` if the model call failed or returned empty after cleaning.
    pub summary: Option<String>,
    /// Raw `assistant_text()` from the model before `clean_recap_text`.
    /// Useful for diagnosing garble (tool-call XML / CJK junk) that cleaning
    /// only partially trims. `None` when the call never returned a response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_response: Option<String>,
    /// Error message if preparation or the model call failed, or if the
    /// cleaned summary was empty.
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recap_request_file_roundtrips() {
        let artifact = RecapRequestFile {
            schema_version: 1,
            request_id: "artifact-1".into(),
            created_at: "2026-06-30T00:00:00Z".into(),
            trigger: "auto".into(),
            model: "v9-zingster".into(),
            x_echo_agent_req_id: "echo-agent-recap-abc".into(),
            x_echo_agent_conv_id: "recap-abc".into(),
            strip_reasoning: false,
            reminder_tag: "system-reminder".into(),
            chat_history: vec![],
            summary: Some("We fixed the flaky test in queue_worker.".into()),
            raw_response: Some("We fixed the flaky test in queue_worker.".into()),
            error: None,
        };
        let json = serde_json::to_string(&artifact).unwrap();
        let parsed: RecapRequestFile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.schema_version, 1);
        assert_eq!(parsed.trigger, "auto");
        assert_eq!(parsed.x_echo_agent_req_id, "echo-agent-recap-abc");
        assert_eq!(
            parsed.summary.as_deref(),
            Some("We fixed the flaky test in queue_worker.")
        );
        assert!(parsed.error.is_none());
    }

    #[test]
    fn compaction_request_file_v2_roundtrips_attempt_details() {
        use echo_agent_chat_state::compaction_utils::CompactionAttempt;
        let artifact = CompactionRequestFile {
            schema_version: 2,
            request_id: "req-1".into(),
            created_at: "2026-06-15T00:00:00Z".into(),
            trigger: "auto".into(),
            prompt_variant: "detailed".into(),
            model: "echoagent".into(),
            user_context: None,
            chat_history: vec![],
            tools: vec![],
            summary: Some("the accepted summary".into()),
            error: None,
            attempts: 2,
            attempt_details: vec![
                // A rejected degraded attempt: the loop captures the raw text.
                CompactionAttempt {
                    attempt: 1,
                    outcome: "degenerate".into(),
                    summary_chars: 22,
                    summary: Some("Now I will do X, Y, Z.".into()),
                    error: None,
                },
                // The accepted retry: text lives in the top-level `summary`.
                CompactionAttempt {
                    attempt: 2,
                    outcome: "success".into(),
                    summary_chars: 4096,
                    summary: None,
                    error: None,
                },
            ],
        };
        let json = serde_json::to_string(&artifact).unwrap();
        let parsed: CompactionRequestFile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.schema_version, 2);
        assert_eq!(parsed.attempt_details.len(), 2);
        assert_eq!(parsed.attempt_details[0].outcome, "degenerate");
        assert_eq!(
            parsed.attempt_details[0].summary.as_deref(),
            Some("Now I will do X, Y, Z.")
        );
        assert_eq!(parsed.attempt_details[1].outcome, "success");
        assert_eq!(parsed.attempt_details[1].summary, None);
    }

    #[test]
    fn compaction_request_file_v1_without_attempt_details_defaults_empty() {
        // A pre-schema-v2 artifact carries no `attempt_details` key — it must
        // still deserialize, defaulting the new field to empty.
        let json = serde_json::json!({
            "schema_version": 1,
            "request_id": "req-old",
            "created_at": "2026-06-01T00:00:00Z",
            "trigger": "manual",
            "prompt_variant": "detailed",
            "model": "echoagent",
            "user_context": null,
            "chat_history": [],
            "summary": "ok",
            "error": null,
            "attempts": 1
        });
        let parsed: CompactionRequestFile = serde_json::from_value(json).unwrap();
        assert!(parsed.attempt_details.is_empty());
        assert!(parsed.tools.is_empty());
    }

    #[test]
    fn subagent_progress_serializes_snake_case_tag() {
        let update = SessionUpdate::SubagentProgress {
            subagent_id: "sub-1".into(),
            parent_session_id: "parent-1".into(),
            child_session_id: "child-1".into(),
            duration_ms: 5000,
            turn_count: 3,
            tool_call_count: 12,
            tokens_used: 45_000,
            context_window_tokens: 256_000,
            context_usage_pct: 35,
            tools_used: vec!["bash".into(), "grep".into()],
            error_count: 1,
        };
        let json = serde_json::to_value(&update).unwrap();
        assert_eq!(json["sessionUpdate"], "subagent_progress");
        // Fields serialize as snake_case (Rust field names) — enum
        // `rename_all` only applies to the tag, not struct fields.
        assert_eq!(json["subagent_id"], "sub-1");
        assert_eq!(json["parent_session_id"], "parent-1");
        assert_eq!(json["child_session_id"], "child-1");
        assert_eq!(json["duration_ms"], 5000);
        assert_eq!(json["turn_count"], 3);
        assert_eq!(json["tool_call_count"], 12);
        assert_eq!(json["tokens_used"], 45_000);
        assert_eq!(json["context_window_tokens"], 256_000);
        assert_eq!(json["context_usage_pct"], 35);
        assert_eq!(json["tools_used"], serde_json::json!(["bash", "grep"]));
        assert_eq!(json["error_count"], 1);
    }

    #[test]
    fn subagent_progress_roundtrips_through_json() {
        let update = SessionUpdate::SubagentProgress {
            subagent_id: "sub-rt".into(),
            parent_session_id: "p".into(),
            child_session_id: "c".into(),
            duration_ms: 100,
            turn_count: 1,
            tool_call_count: 2,
            tokens_used: 1000,
            context_window_tokens: 256_000,
            context_usage_pct: 1,
            tools_used: vec![],
            error_count: 0,
        };
        let json_str = serde_json::to_string(&update).unwrap();
        let parsed: SessionUpdate = serde_json::from_str(&json_str).unwrap();
        assert_eq!(update, parsed);
    }

    #[test]
    fn subagent_progress_orders_after_spawned_before_finished() {
        // Verify that SubagentProgress appears between SubagentSpawned
        // and SubagentFinished in the enum definition (important for
        // notification ordering expectations).
        let spawned = serde_json::to_value(SessionUpdate::SubagentSpawned {
            subagent_id: "s".into(),
            parent_session_id: "p".into(),
            parent_prompt_id: None,
            child_session_id: "c".into(),
            subagent_type: "explore".into(),
            description: "d".into(),
            effective_context_source: None,
            context_normalized: false,
            capability_mode: None,
            persona: None,
            role: None,
            model: None,
            resumed_from: None,
            workflow_run_id: None,
        })
        .unwrap();
        let progress = serde_json::to_value(SessionUpdate::SubagentProgress {
            subagent_id: "s".into(),
            parent_session_id: "p".into(),
            child_session_id: "c".into(),
            duration_ms: 100,
            turn_count: 1,
            tool_call_count: 1,
            tokens_used: 100,
            context_window_tokens: 256_000,
            context_usage_pct: 0,
            tools_used: vec![],
            error_count: 0,
        })
        .unwrap();
        let finished = serde_json::to_value(SessionUpdate::SubagentFinished {
            subagent_id: "s".into(),
            child_session_id: "c".into(),
            status: "completed".into(),
            error: None,
            tool_calls: 1,
            turns: 1,
            duration_ms: 200,
            tokens_used: 50_000,
            output: None,
            will_wake: false,
        })
        .unwrap();
        // All three should have distinct tags
        assert_eq!(spawned["sessionUpdate"], "subagent_spawned");
        assert_eq!(progress["sessionUpdate"], "subagent_progress");
        assert_eq!(finished["sessionUpdate"], "subagent_finished");
    }

    #[test]
    fn subagent_finished_without_tokens_used_backward_compat() {
        // Old JSONL entries written before the tokens_used field was added
        // must deserialize successfully with tokens_used defaulting to 0.
        // modelUsage on older wire is ignored (billing is RecordSubagentUsage only).
        let json = r#"{
            "sessionUpdate": "subagent_finished",
            "subagent_id": "sa-old",
            "child_session_id": "cs-old",
            "status": "completed",
            "tool_calls": 3,
            "turns": 1,
            "duration_ms": 5000,
            "modelUsage": { "m": { "inputTokens": 1 } }
        }"#;
        let update: SessionUpdate = serde_json::from_str(json).unwrap();
        match update {
            SessionUpdate::SubagentFinished {
                subagent_id,
                tokens_used,
                output,
                ..
            } => {
                assert_eq!(subagent_id, "sa-old");
                assert_eq!(tokens_used, 0, "missing field must default to 0");
                assert_eq!(output, None);
            }
            other => panic!("expected SubagentFinished, got {other:?}"),
        }
    }

    #[test]
    fn subagent_finished_with_tokens_used_roundtrips() {
        let update = SessionUpdate::SubagentFinished {
            subagent_id: "sa-rt".into(),
            child_session_id: "cs-rt".into(),
            status: "completed".into(),
            error: None,
            tool_calls: 5,
            turns: 2,
            duration_ms: 10_000,
            tokens_used: 75_000,
            output: Some("done".into()),
            will_wake: false,
        };
        let json_str = serde_json::to_string(&update).unwrap();
        let parsed: SessionUpdate = serde_json::from_str(&json_str).unwrap();
        assert_eq!(update, parsed);

        let json = serde_json::to_value(&update).unwrap();
        assert_eq!(json["tokens_used"], 75_000);
    }

    #[test]
    fn unknown_variant_deserializes_from_removed_git_branch_update() {
        let json = r#"{"sessionUpdate": "git_branch_update", "branch": "main"}"#;
        let update: SessionUpdate = serde_json::from_str(json).unwrap();
        assert_eq!(update, SessionUpdate::Unknown);
    }

    #[test]
    fn unknown_variant_deserializes_from_arbitrary_future_variant() {
        let json = r#"{"sessionUpdate": "some_future_feature", "data": 42}"#;
        let update: SessionUpdate = serde_json::from_str(json).unwrap();
        assert_eq!(update, SessionUpdate::Unknown);
    }

    #[test]
    fn unknown_variant_in_session_notification_envelope() {
        let json = r#"{
            "sessionId": "sess-123",
            "update": {"sessionUpdate": "git_branch_update", "branch": "main"}
        }"#;
        let notification: SessionNotification = serde_json::from_str(json).unwrap();
        assert_eq!(notification.session_id.0.as_ref(), "sess-123");
        assert_eq!(notification.update, SessionUpdate::Unknown);
    }

    #[test]
    fn known_variants_still_deserialize_correctly() {
        // MemoryFlushStarted (unit variant)
        let json = r#"{"sessionUpdate": "memory_flush_started"}"#;
        let update: SessionUpdate = serde_json::from_str(json).unwrap();
        assert_eq!(update, SessionUpdate::MemoryFlushStarted);

        // AutoCompactCancelled (strenum reason)
        let json = r#"{"sessionUpdate": "auto_compact_cancelled", "reason": "user_cancelled"}"#;
        let update: SessionUpdate = serde_json::from_str(json).unwrap();
        assert_eq!(
            update,
            SessionUpdate::AutoCompactCancelled {
                reason: AutoCompactCancelReason::UserCancelled,
            }
        );

        // AutoCompactFailed (struct variant)
        let json = r#"{"sessionUpdate": "auto_compact_failed", "error": "oom"}"#;
        let update: SessionUpdate = serde_json::from_str(json).unwrap();
        assert_eq!(
            update,
            SessionUpdate::AutoCompactFailed {
                error: "oom".into()
            }
        );

        // RetryState (newtype variant)
        let json = r#"{"sessionUpdate": "retry_state", "type": "failed", "error_type": "auth", "message": "bad token"}"#;
        let update: SessionUpdate = serde_json::from_str(json).unwrap();
        assert!(matches!(
            update,
            SessionUpdate::RetryState(RetryState::Failed { .. })
        ));
    }

    #[test]
    fn memory_flush_completed_with_path_roundtrips() {
        let update = SessionUpdate::MemoryFlushCompleted {
            result: "written".into(),
            path: Some("/home/user/.echo-agent/memory/ws/sessions/log.md".into()),
        };
        let json_str = serde_json::to_string(&update).unwrap();
        let parsed: SessionUpdate = serde_json::from_str(&json_str).unwrap();
        assert_eq!(update, parsed);
    }

    #[test]
    fn memory_flush_completed_without_path_backward_compat() {
        // Old format without path field should deserialize with path=None
        let json = r#"{"sessionUpdate": "memory_flush_completed", "result": "written"}"#;
        let update: SessionUpdate = serde_json::from_str(json).unwrap();
        assert_eq!(
            update,
            SessionUpdate::MemoryFlushCompleted {
                result: "written".into(),
                path: None,
            }
        );
    }

    #[test]
    fn memory_dream_completed_roundtrips() {
        let update = SessionUpdate::MemoryDreamCompleted {
            result: "written (500 chars)".into(),
            path: Some("/home/user/.echo-agent/memory/ws/MEMORY.md".into()),
        };
        let json_str = serde_json::to_string(&update).unwrap();
        let parsed: SessionUpdate = serde_json::from_str(&json_str).unwrap();
        assert_eq!(update, parsed);
    }

    #[test]
    fn memory_session_saved_roundtrips() {
        let update = SessionUpdate::MemorySessionSaved {
            path: "/home/user/.echo-agent/memory/ws/sessions/2026-01-15-fix-auth-abc12345.md"
                .into(),
        };
        let json_str = serde_json::to_string(&update).unwrap();
        let parsed: SessionUpdate = serde_json::from_str(&json_str).unwrap();
        assert_eq!(update, parsed);
    }

    #[test]
    fn unknown_serializes_with_stable_tag() {
        let json = serde_json::to_value(&SessionUpdate::Unknown).unwrap();
        assert_eq!(json["sessionUpdate"], "unknown");
    }

    #[test]
    fn unknown_roundtrips_through_json() {
        let json_str = serde_json::to_string(&SessionUpdate::Unknown).unwrap();
        let parsed: SessionUpdate = serde_json::from_str(&json_str).unwrap();
        assert_eq!(parsed, SessionUpdate::Unknown);
    }

    #[test]
    fn memory_files_variant_roundtrips_through_json() {
        let update = SessionUpdate::MemoryFiles {
            files: vec![
                MemoryFileInfo {
                    path: "/home/user/.echo-agent/memory/MEMORY.md".into(),
                    source: "global".into(),
                    size_bytes: 1024,
                    modified_epoch_secs: Some(1_700_000_000),
                },
                MemoryFileInfo {
                    path: "/project/.echo-agent/memory/MEMORY.md".into(),
                    source: "workspace".into(),
                    size_bytes: 512,
                    modified_epoch_secs: None,
                },
            ],
        };
        let json_str = serde_json::to_string(&update).unwrap();
        let parsed: SessionUpdate = serde_json::from_str(&json_str).unwrap();
        assert_eq!(update, parsed);

        let json = serde_json::to_value(&update).unwrap();
        assert_eq!(json["sessionUpdate"], "memory_files");
        assert_eq!(json["files"].as_array().unwrap().len(), 2);
        // Populated timestamp serializes as a plain u64
        assert_eq!(json["files"][0]["modified_epoch_secs"], 1_700_000_000_u64);
        // None is omitted entirely
        assert!(json["files"][1].get("modified_epoch_secs").is_none());
    }

    #[test]
    fn memory_files_empty_list_serializes() {
        let update = SessionUpdate::MemoryFiles { files: vec![] };
        let json = serde_json::to_value(&update).unwrap();
        assert_eq!(json["sessionUpdate"], "memory_files");
        assert!(json["files"].as_array().unwrap().is_empty());
        let json_str = serde_json::to_string(&update).unwrap();
        let parsed: SessionUpdate = serde_json::from_str(&json_str).unwrap();
        assert_eq!(update, parsed);
    }

    #[test]
    fn tool_call_delta_chunk_first_event_serializes_with_id_and_name() {
        // First chunk for a tool: carries id+name, no arguments_delta.
        let update = SessionUpdate::ToolCallDeltaChunk {
            tool_call_id: Some("call_abc".into()),
            tool_index: 0,
            name: Some("search_replace".into()),
            arguments_delta: None,
        };
        let json = serde_json::to_value(&update).unwrap();
        assert_eq!(json["sessionUpdate"], "tool_call_delta_chunk");
        assert_eq!(json["tool_call_id"], "call_abc");
        assert_eq!(json["tool_index"], 0);
        assert_eq!(json["name"], "search_replace");
        // None fields are skipped (cleaner wire payload, fewer bytes).
        assert!(json.get("arguments_delta").is_none());
    }

    #[test]
    fn tool_call_delta_chunk_subsequent_event_carries_only_arguments_delta() {
        // Later chunks omit id+name; only the JSON fragment travels.
        let update = SessionUpdate::ToolCallDeltaChunk {
            tool_call_id: None,
            tool_index: 0,
            name: None,
            arguments_delta: Some("{\"file\":\"src/".into()),
        };
        let json = serde_json::to_value(&update).unwrap();
        assert_eq!(json["sessionUpdate"], "tool_call_delta_chunk");
        assert_eq!(json["tool_index"], 0);
        assert_eq!(json["arguments_delta"], "{\"file\":\"src/");
        // Optional fields skipped when None.
        assert!(json.get("tool_call_id").is_none());
        assert!(json.get("name").is_none());
    }

    #[test]
    fn tool_call_delta_chunk_roundtrips_through_json() {
        let cases = vec![
            SessionUpdate::ToolCallDeltaChunk {
                tool_call_id: Some("call_1".into()),
                tool_index: 0,
                name: Some("Bash".into()),
                arguments_delta: None,
            },
            SessionUpdate::ToolCallDeltaChunk {
                tool_call_id: None,
                tool_index: 0,
                name: None,
                arguments_delta: Some("{\"command\":\"ls\"}".into()),
            },
            SessionUpdate::ToolCallDeltaChunk {
                tool_call_id: Some("call_2".into()),
                tool_index: 1,
                name: Some("ReadFile".into()),
                arguments_delta: Some("{\"path\":".into()),
            },
        ];
        for update in cases {
            let s = serde_json::to_string(&update).unwrap();
            let parsed: SessionUpdate = serde_json::from_str(&s).unwrap();
            assert_eq!(update, parsed, "round-trip mismatch for {s}");
        }
    }

    #[test]
    fn tool_call_delta_chunk_unknown_extra_fields_dont_break_deserialization() {
        // Forward-compat: if a future shell adds new optional fields,
        // older deserializers must still accept the message.
        let json = r#"{
            "sessionUpdate": "tool_call_delta_chunk",
            "tool_call_id": "call_x",
            "tool_index": 7,
            "name": "future_tool",
            "arguments_delta": "...",
            "future_field": "ignored"
        }"#;
        let update: SessionUpdate = serde_json::from_str(json).expect("parses with extra field");
        match update {
            SessionUpdate::ToolCallDeltaChunk {
                tool_call_id,
                tool_index,
                name,
                arguments_delta,
            } => {
                assert_eq!(tool_call_id.as_deref(), Some("call_x"));
                assert_eq!(tool_index, 7);
                assert_eq!(name.as_deref(), Some("future_tool"));
                assert_eq!(arguments_delta.as_deref(), Some("..."));
            }
            other => panic!("expected ToolCallDeltaChunk, got {other:?}"),
        }
    }

    /// Helper: `GoalUpdated` with all optional fields populated.
    fn make_goal_updated_full() -> SessionUpdate {
        SessionUpdate::GoalUpdated {
            goal_id: "g-1".into(),
            objective: "Build widget".into(),
            status: "active".into(),
            phase: "executing".into(),
            token_budget: Some(100_000),
            tokens_used: 25_000,
            elapsed_ms: 5000,
            total_deliverables: 3,
            completed_deliverables: 1,
            current_deliverable_id: Some(2),
            current_deliverable_title: Some("Core logic".into()),
            current_subagent_role: Some("worker".into()),
            total_worker_rounds: 4,
            total_verify_rounds: 2,
            live_subagent_tokens: Some(10_000),
            live_tokens_by_model: vec![
                ("echo-agent-4".into(), 6_000),
                ("echo-agent-3".into(), 4_000),
            ],
            live_context_pct: Some(35),
            live_turn_count: Some(3),
            live_tool_call_count: Some(8),
            last_event: Some("worker_completed".into()),
            last_event_detail: Some("Core logic".into()),
            last_event_timestamp: Some("2026-01-01T00:05:00Z".into()),
            token_baseline: 0,
            finished_subagent_tokens: 0,
            deliverables: vec![],
            pause_message: None,
            classifier_runs_attempted: Some(2),
            classifier_max_runs: Some(3),
            last_classifier_verdict: Some(GoalClassifierVerdict::NotAchieved),
            last_classifier_details_path: Some("/tmp/details.md".into()),
            verifying_completion: Some(true),
            planning: Some(true),
        }
    }

    /// Helper: `GoalUpdated` with all optional fields `None` / zeroed.
    fn make_goal_updated_minimal() -> SessionUpdate {
        SessionUpdate::GoalUpdated {
            goal_id: "g-min".into(),
            objective: "Test".into(),
            status: "active".into(),
            phase: "idle".into(),
            token_budget: None,
            tokens_used: 0,
            elapsed_ms: 0,
            total_deliverables: 0,
            completed_deliverables: 0,
            current_deliverable_id: None,
            current_deliverable_title: None,
            current_subagent_role: None,
            total_worker_rounds: 0,
            total_verify_rounds: 0,
            live_subagent_tokens: None,
            live_tokens_by_model: Vec::new(),
            live_context_pct: None,
            live_turn_count: None,
            live_tool_call_count: None,
            last_event: None,
            last_event_detail: None,
            last_event_timestamp: None,
            token_baseline: 0,
            finished_subagent_tokens: 0,
            deliverables: vec![],
            pause_message: None,
            classifier_runs_attempted: None,
            classifier_max_runs: None,
            last_classifier_verdict: None,
            last_classifier_details_path: None,
            verifying_completion: None,
            planning: None,
        }
    }

    #[test]
    fn goal_updated_serializes_snake_case_tag() {
        let update = make_goal_updated_full();
        let json = serde_json::to_value(&update).unwrap();
        assert_eq!(json["sessionUpdate"], "goal_updated");
        assert_eq!(json["goal_id"], "g-1");
        assert_eq!(json["status"], "active");
        assert_eq!(json["phase"], "executing");
        assert_eq!(json["token_budget"], 100_000);
        assert_eq!(json["tokens_used"], 25_000);
        assert_eq!(json["total_deliverables"], 3);
        assert_eq!(json["completed_deliverables"], 1);
        assert_eq!(json["current_deliverable_idx"], 2);
        assert_eq!(json["total_worker_rounds"], 4);
        assert_eq!(json["total_verify_rounds"], 2);
        assert_eq!(json["live_subagent_tokens"], 10_000);
        assert_eq!(json["live_tokens_by_model"][0][0], "echo-agent-4");
        assert_eq!(json["live_tokens_by_model"][0][1], 6_000);
        assert_eq!(json["live_context_pct"], 35);
        assert_eq!(json["last_event"], "worker_completed");
        assert_eq!(json["classifier_runs_attempted"], 2);
        assert_eq!(json["classifier_max_runs"], 3);
        assert_eq!(json["last_classifier_verdict"], "not_achieved");
        assert_eq!(json["last_classifier_details_path"], "/tmp/details.md");
        assert_eq!(json["verifying_completion"], true);
        assert_eq!(json["planning"], true);
    }

    #[test]
    fn goal_updated_roundtrips_through_json() {
        let update = make_goal_updated_minimal();
        let json_str = serde_json::to_string(&update).unwrap();
        let parsed: SessionUpdate = serde_json::from_str(&json_str).unwrap();
        assert_eq!(update, parsed);

        // Also roundtrip the full variant.
        let full = make_goal_updated_full();
        let json_str = serde_json::to_string(&full).unwrap();
        let parsed: SessionUpdate = serde_json::from_str(&json_str).unwrap();
        assert_eq!(full, parsed);
    }

    #[test]
    fn goal_updated_optional_fields_skipped_when_none() {
        let json = serde_json::to_value(make_goal_updated_minimal()).unwrap();
        assert!(json.get("token_budget").is_none());
        assert!(json.get("current_deliverable_idx").is_none());
        assert!(json.get("current_deliverable_title").is_none());
        assert!(json.get("current_subagent_role").is_none());
        assert!(json.get("live_subagent_tokens").is_none());
        assert!(json.get("live_tokens_by_model").is_none());
        assert!(json.get("live_context_pct").is_none());
        assert!(json.get("live_turn_count").is_none());
        assert!(json.get("live_tool_call_count").is_none());
        assert!(json.get("last_event").is_none());
        assert!(json.get("last_event_detail").is_none());
        assert!(json.get("last_event_timestamp").is_none());
        assert!(json.get("classifier_runs_attempted").is_none());
        assert!(json.get("classifier_max_runs").is_none());
        assert!(json.get("last_classifier_verdict").is_none());
        assert!(json.get("last_classifier_details_path").is_none());
        assert!(json.get("verifying_completion").is_none());
        assert!(json.get("planning").is_none());
    }

    #[test]
    fn goal_updated_payload_missing_new_fields_deserializes_to_none() {
        // Wire round-trip: an older shell that predates the
        // classifier / planning fields will omit them all. Each must
        // deserialize to `None` so the pager keeps working.
        let json = r#"{
            "sessionUpdate": "goal_updated",
            "goal_id": "g-old",
            "objective": "test",
            "status": "active",
            "phase": "idle",
            "tokens_used": 0,
            "elapsed_ms": 0,
            "total_deliverables": 0,
            "completed_deliverables": 0,
            "total_worker_rounds": 0,
            "total_verify_rounds": 0
        }"#;
        let update: SessionUpdate = serde_json::from_str(json).unwrap();
        match update {
            SessionUpdate::GoalUpdated {
                classifier_runs_attempted,
                classifier_max_runs,
                last_classifier_verdict,
                last_classifier_details_path,
                verifying_completion,
                planning,
                ..
            } => {
                assert_eq!(classifier_runs_attempted, None);
                assert_eq!(classifier_max_runs, None);
                assert_eq!(last_classifier_verdict, None);
                assert_eq!(last_classifier_details_path, None);
                assert_eq!(verifying_completion, None);
                assert_eq!(planning, None);
            }
            other => panic!("expected GoalUpdated, got {other:?}"),
        }
    }

    #[test]
    fn goal_updated_payload_with_new_fields_round_trips() {
        // Wire round-trip: a payload that carries every classifier
        // field round-trips through serialize → deserialize without
        // mutation.
        let update = make_goal_updated_full();
        let json_str = serde_json::to_string(&update).unwrap();
        let parsed: SessionUpdate = serde_json::from_str(&json_str).unwrap();
        assert_eq!(update, parsed);

        match parsed {
            SessionUpdate::GoalUpdated {
                classifier_runs_attempted,
                classifier_max_runs,
                last_classifier_verdict,
                last_classifier_details_path,
                verifying_completion,
                planning,
                ..
            } => {
                assert_eq!(classifier_runs_attempted, Some(2));
                assert_eq!(classifier_max_runs, Some(3));
                assert_eq!(
                    last_classifier_verdict,
                    Some(GoalClassifierVerdict::NotAchieved)
                );
                assert_eq!(
                    last_classifier_details_path.as_deref(),
                    Some("/tmp/details.md")
                );
                assert_eq!(verifying_completion, Some(true));
                assert_eq!(planning, Some(true));
            }
            other => panic!("expected GoalUpdated, got {other:?}"),
        }
    }

    #[test]
    fn goal_updated_backward_compat_without_optional_fields() {
        let json = r#"{
            "sessionUpdate": "goal_updated",
            "goal_id": "g-bc",
            "objective": "test",
            "status": "active",
            "phase": "idle",
            "tokens_used": 0,
            "elapsed_ms": 0,
            "total_deliverables": 0,
            "completed_deliverables": 0,
            "total_worker_rounds": 0,
            "total_verify_rounds": 0
        }"#;
        let update: SessionUpdate = serde_json::from_str(json).unwrap();
        match update {
            SessionUpdate::GoalUpdated {
                goal_id,
                token_budget,
                current_deliverable_id,
                live_subagent_tokens,
                last_event,
                ..
            } => {
                assert_eq!(goal_id, "g-bc");
                assert_eq!(token_budget, None);
                assert_eq!(current_deliverable_id, None);
                assert_eq!(live_subagent_tokens, None);
                assert_eq!(last_event, None);
            }
            other => panic!("expected GoalUpdated, got {other:?}"),
        }
    }

    // ── ModelChanged (leader-mode multi-client model switch fan-out) ──

    /// Wire format for `ModelChanged` — sanity-check the JSON exactly,
    /// since the pager and any third-party clients consume this on the wire.
    /// Specifically:
    /// - `sessionUpdate` tag is the snake_case variant name.
    /// - Field names use Rust snake_case (struct fields are not subject to
    ///   `rename_all` — that only renames the tag).
    /// - `reasoning_effort` is omitted entirely when `None` (smaller wire +
    ///   distinguishable from explicitly-cleared-by-user, if that ever
    ///   becomes a real distinction).
    #[test]
    fn model_changed_serializes_snake_case_with_optional_effort() {
        let with_effort = SessionUpdate::ModelChanged {
            model_id: "echo-agent-4".into(),
            reasoning_effort: Some("high".into()),
        };
        let json = serde_json::to_value(&with_effort).unwrap();
        assert_eq!(json["sessionUpdate"], "model_changed");
        assert_eq!(json["model_id"], "echo-agent-4");
        assert_eq!(json["reasoning_effort"], "high");

        let without_effort = SessionUpdate::ModelChanged {
            model_id: "echo-agent-3".into(),
            reasoning_effort: None,
        };
        let json = serde_json::to_value(&without_effort).unwrap();
        assert_eq!(json["sessionUpdate"], "model_changed");
        assert_eq!(json["model_id"], "echo-agent-3");
        assert!(
            json.get("reasoning_effort").is_none(),
            "reasoning_effort: None must be skipped on the wire so old pagers \
             and third-party ACP clients see a smaller, no-extra-keys payload"
        );
    }

    /// `ModelChanged` round-trips through JSON: a follower client deserializes
    /// the exact same value the agent serialized. Pins the field order /
    /// case so an accidental rename doesn't silently degrade to `Unknown`
    /// (the `#[serde(other)]` catch-all would swallow that on the pager side
    /// and break multi-client model sync without any test failing).
    #[test]
    fn model_changed_roundtrips_through_json() {
        let original = SessionUpdate::ModelChanged {
            model_id: "echo-agent-4".into(),
            reasoning_effort: Some("medium".into()),
        };
        let json_str = serde_json::to_string(&original).unwrap();
        let parsed: SessionUpdate = serde_json::from_str(&json_str).unwrap();
        assert_eq!(original, parsed);
    }

    /// Wrap `ModelChanged` in the full `SessionNotification` envelope and
    /// confirm the result is what the leader's session-scoped fan-out keys
    /// on: top-level `sessionId` (camelCase from the envelope's
    /// `rename_all`) + nested `update.sessionUpdate == "model_changed"`.
    /// Without the top-level `sessionId`, the leader's `extract_session_id`
    /// returns `None` and the notification falls through to the
    /// last-active-client fallback instead of broadcasting — that would
    /// silently break the entire multi-client sync.
    #[test]
    fn model_changed_envelope_carries_session_id_at_top_level() {
        let notif = SessionNotification {
            session_id: acp::SessionId::new("sess-abc"),
            update: SessionUpdate::ModelChanged {
                model_id: "echo-agent-4".into(),
                reasoning_effort: None,
            },
            meta: None,
        };
        let json = serde_json::to_value(&notif).unwrap();
        assert_eq!(json["sessionId"], "sess-abc");
        assert_eq!(json["update"]["sessionUpdate"], "model_changed");
        assert_eq!(json["update"]["model_id"], "echo-agent-4");
    }

    // ── TurnCompleted (durable, replayable turn-end signal) ──

    #[test]
    fn turn_completed_serializes_snake_case_tag_and_fields() {
        // Mirrors the SubagentProgress convention: `rename_all = "snake_case"`
        // only renames the tag, so struct fields keep their Rust snake_case
        // names on the wire.
        let update = SessionUpdate::TurnCompleted {
            prompt_id: "p-1".into(),
            stop_reason: "end_turn".into(),
            agent_result: Some("done".into()),
            usage: None,
        };
        let json = serde_json::to_value(&update).unwrap();
        assert_eq!(json["sessionUpdate"], "turn_completed");
        assert_eq!(json["prompt_id"], "p-1");
        assert_eq!(json["stop_reason"], "end_turn");
        assert_eq!(json["agent_result"], "done");
    }

    #[test]
    fn turn_completed_optional_fields_skipped_when_none() {
        let update = SessionUpdate::TurnCompleted {
            prompt_id: "p-2".into(),
            stop_reason: "cancelled".into(),
            agent_result: None,
            usage: None,
        };
        let json = serde_json::to_value(&update).unwrap();
        assert_eq!(json["sessionUpdate"], "turn_completed");
        assert!(json.get("agent_result").is_none());
    }

    #[test]
    fn turn_completed_roundtrips_through_json() {
        for update in [
            SessionUpdate::TurnCompleted {
                prompt_id: "p-rt".into(),
                stop_reason: "end_turn".into(),
                agent_result: Some("result text".into()),
                usage: None,
            },
            SessionUpdate::TurnCompleted {
                prompt_id: "p-min".into(),
                stop_reason: "error".into(),
                agent_result: None,
                usage: None,
            },
        ] {
            let json_str = serde_json::to_string(&update).unwrap();
            let parsed: SessionUpdate = serde_json::from_str(&json_str).unwrap();
            assert_eq!(update, parsed);
        }
    }

    #[test]
    fn project_result_hides_costs_when_partial_or_incomplete() {
        let mut model_usage = indexmap::IndexMap::new();
        model_usage.insert(
            "m".into(),
            PromptUsageModel {
                input_tokens: 100,
                cached_read_tokens: 40,
                output_tokens: 10,
                total_tokens: 110,
                model_calls: 4,
                cost_usd_ticks: Some(1_000_000_000),
                ..Default::default()
            },
        );
        let partial = PromptUsage {
            totals: PromptUsageModel {
                input_tokens: 100,
                cached_read_tokens: 40,
                output_tokens: 10,
                total_tokens: 110,
                model_calls: 5,
                cost_usd_ticks: Some(1_000_000_000),
                cost_is_partial: true,
                cost_missing_calls: 1,
                ..Default::default()
            },
            model_usage: model_usage.clone(),
            num_turns: 2,
            usage_is_incomplete: false,
        };
        let mut result = serde_json::json!({});
        project_result_usage(&mut result, &partial);
        assert_eq!(result["usage"]["input_tokens"], 60);
        assert!(result.get("total_cost_usd").is_none());
        assert!(result.get("total_cost_usd_ticks").is_none());
        assert_eq!(result["cost_is_partial"], true);
        assert!(result["modelUsage"]["m"].get("costUSD").is_none());

        let mut incomplete = PromptUsage {
            totals: PromptUsageModel {
                input_tokens: 50,
                output_tokens: 5,
                total_tokens: 55,
                model_calls: 1,
                cost_usd_ticks: Some(5_000_000_000),
                ..Default::default()
            },
            model_usage,
            num_turns: 1,
            usage_is_incomplete: true,
        };
        incomplete.scrub_untrustworthy_costs();
        assert!(incomplete.totals.cost_usd_ticks.is_none());
        let mut result = serde_json::json!({});
        project_result_usage(&mut result, &incomplete);
        assert_eq!(result["usage_is_incomplete"], true);
        assert!(result.get("total_cost_usd").is_none());
        assert!(result.get("total_cost_usd_ticks").is_none());
        assert!(result["modelUsage"]["m"].get("costUSD").is_none());
    }

    #[test]
    fn project_result_incomplete_empty_omits_zero_usage() {
        let usage = PromptUsage::project_from_ledger(None, true).unwrap();
        let mut result = serde_json::json!({});
        project_result_usage(&mut result, &usage);
        assert_eq!(result["usage_is_incomplete"], true);
        assert!(result.get("usage").is_none());
        assert!(result.get("num_turns").is_none());
        assert!(result.get("total_cost_usd").is_none());
    }

    #[test]
    fn attach_result_usage_fail_closed_on_parse_error() {
        let mut result = serde_json::json!({"ok": true});
        attach_result_usage_fail_closed(&mut result, &serde_json::json!("not-an-object"));
        assert_eq!(result["usage_is_incomplete"], true);
        assert!(result.get("usage").is_none());
        assert_eq!(result["ok"], true);
    }

    #[test]
    fn cost_missing_calls_not_on_acp_wire() {
        let model = PromptUsageModel {
            input_tokens: 1,
            cost_missing_calls: 3,
            cost_is_partial: true,
            ..Default::default()
        };
        let v = serde_json::to_value(&model).unwrap();
        assert!(v.get("costMissingCalls").is_none());
        assert_eq!(v["costIsPartial"], true);
    }

    #[test]
    fn scrub_untrustworthy_costs_clears_ticks_when_partial() {
        let mut usage = PromptUsage {
            totals: PromptUsageModel {
                input_tokens: 10,
                output_tokens: 1,
                total_tokens: 11,
                cost_usd_ticks: Some(100),
                cost_is_partial: true,
                cost_missing_calls: 1,
                ..Default::default()
            },
            model_usage: Default::default(),
            num_turns: 1,
            usage_is_incomplete: false,
        };
        usage.scrub_untrustworthy_costs();
        assert!(usage.totals.cost_usd_ticks.is_none());
        assert!(usage.totals.cost_is_partial);
    }

    #[test]
    fn project_result_token_identity_uncached_plus_cache_plus_output() {
        let mut model_usage = indexmap::IndexMap::new();
        model_usage.insert(
            "m".into(),
            PromptUsageModel {
                input_tokens: 100,
                cached_read_tokens: 40,
                output_tokens: 10,
                total_tokens: 110,
                model_calls: 1,
                cost_usd_ticks: Some(2_000_000_000),
                ..Default::default()
            },
        );
        let usage = PromptUsage {
            totals: PromptUsageModel {
                input_tokens: 100,
                cached_read_tokens: 40,
                output_tokens: 10,
                total_tokens: 110,
                model_calls: 1,
                cost_usd_ticks: Some(2_000_000_000),
                ..Default::default()
            },
            model_usage,
            num_turns: 1,
            usage_is_incomplete: false,
        };
        let mut result = serde_json::json!({});
        project_result_usage(&mut result, &usage);
        let uncached = result["usage"]["input_tokens"].as_u64().unwrap();
        let cache = result["usage"]["cache_read_input_tokens"].as_u64().unwrap();
        let output = result["usage"]["output_tokens"].as_u64().unwrap();
        let total = result["usage"]["total_tokens"].as_u64().unwrap();
        assert_eq!(uncached, 60);
        assert_eq!(cache, 40);
        assert_eq!(output, 10);
        assert_eq!(total, uncached + cache + output);
        // ACP serde keeps full input_tokens; headless identity differs.
        let acp = serde_json::to_value(&usage).unwrap();
        assert_eq!(acp["inputTokens"], 100);
        assert_eq!(acp["cachedReadTokens"], 40);
        assert_ne!(acp["inputTokens"], result["usage"]["input_tokens"]);
        assert_eq!(result["modelUsage"]["m"]["inputTokens"], 60);
        assert_eq!(result["modelUsage"]["m"]["cacheReadInputTokens"], 40);
        assert_eq!(result["total_cost_usd"], 0.2);
        // Exact ticks accompany the float for tick-exact reconciliation.
        assert_eq!(result["total_cost_usd_ticks"], 2_000_000_000_i64);
    }

    #[test]
    fn turn_completed_missing_required_fields_fail_to_deserialize() {
        let missing_prompt_id = r#"{"sessionUpdate": "turn_completed", "stop_reason": "end_turn"}"#;
        assert!(serde_json::from_str::<SessionUpdate>(missing_prompt_id).is_err());
        let missing_stop_reason = r#"{"sessionUpdate": "turn_completed", "prompt_id": "p-1"}"#;
        assert!(serde_json::from_str::<SessionUpdate>(missing_stop_reason).is_err());
    }
}
