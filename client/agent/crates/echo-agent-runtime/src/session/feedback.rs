//! Feedback request heuristics for EchoAgent Code sessions.
//!
//! This module implements the feedback request decision logic based on session signals.
//! It uses tiered probability sampling to request feedback at appropriate moments
//! without overwhelming users.

use serde::{Deserialize, Serialize};

use super::signals::SessionSignals;
use crate::util::probabilistic_sample;

// Re-export shared feedback API wire types to avoid duplication
pub use echo_agent_chat_proxy_types::feedback_types::{
    FeedbackHeuristicsConfig, FeedbackMode, TierConfig,
};

/// Feedback request tier with associated probability and criteria.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackTier {
    /// Tier 1: Standard engagement (0.05% sample rate)
    /// Triggered after sustained engagement without issues
    Tier1,
    /// Tier 2: Complex session (0.02% sample rate)
    /// Triggered after complex sessions with some friction
    Tier2,
    /// Tier 3: Recovery/completion (0.01% sample rate)
    /// Triggered after recovery from issues or session end
    Tier3,
}

impl FeedbackTier {
    /// Get the sample rate for this tier (as a fraction, e.g., 0.0005 for 0.05%)
    pub fn sample_rate(&self) -> f64 {
        match self {
            FeedbackTier::Tier1 => 0.0005, // 0.05%
            FeedbackTier::Tier2 => 0.0002, // 0.02%
            FeedbackTier::Tier3 => 0.0001, // 0.01%
        }
    }

    /// Get the trigger type identifier for this tier.
    pub fn trigger_type(&self) -> &'static str {
        match self {
            FeedbackTier::Tier1 => "tier1_engagement",
            FeedbackTier::Tier2 => "tier2_complex_recovery",
            FeedbackTier::Tier3 => "tier3_friction_recovery",
        }
    }
}

/// Describes the specific condition that triggered a feedback request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TriggerCondition {
    /// Tier that was triggered
    pub tier: FeedbackTier,
    /// Specific condition that was met (e.g., "turns >= 10 AND tool_calls >= 5 AND compactions >= 2 AND cancellations == 0")
    pub condition: String,
    /// Actual signal values at trigger time
    pub signal_snapshot: TriggerSignalSnapshot,
}

/// Snapshot of signal values at the time feedback was triggered.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TriggerSignalSnapshot {
    pub turn_count: u32,
    pub tool_calls_count: u32,
    pub compactions_count: u32,
    pub errors_count: u32,
    pub cancellations_count: u32,
    pub has_reverted: bool,
}

impl TriggerCondition {
    /// Create a Tier 1 trigger condition.
    pub fn tier1(signals: &SessionSignals) -> Self {
        Self {
            tier: FeedbackTier::Tier1,
            condition:
                "turns >= 10 AND tool_calls >= 5 AND compactions >= 2 AND cancellations == 0"
                    .to_string(),
            signal_snapshot: TriggerSignalSnapshot::from_signals(signals),
        }
    }

    /// Create a Tier 2 trigger condition.
    pub fn tier2(signals: &SessionSignals) -> Self {
        Self {
            tier: FeedbackTier::Tier2,
            condition: "turns >= 15 AND tool_calls >= 10 AND compactions >= 3 AND errors >= 1"
                .to_string(),
            signal_snapshot: TriggerSignalSnapshot::from_signals(signals),
        }
    }

    /// Create a Tier 3 trigger condition.
    pub fn tier3(signals: &SessionSignals, had_cancellation: bool, had_revert: bool) -> Self {
        let recovery_condition = if had_cancellation && had_revert {
            "(cancellations > 0 OR has_reverted)"
        } else if had_cancellation {
            "cancellations > 0"
        } else {
            "has_reverted"
        };

        Self {
            tier: FeedbackTier::Tier3,
            condition: format!("turns >= 20 AND {}", recovery_condition),
            signal_snapshot: TriggerSignalSnapshot::from_signals(signals),
        }
    }

    /// Get a human-readable trigger reason.
    pub fn trigger_reason(&self) -> String {
        let snapshot = &self.signal_snapshot;
        match self.tier {
            FeedbackTier::Tier1 => format!(
                "Tier 1: Sustained engagement (turns={}, tools={}, compactions={}, no cancellations)",
                snapshot.turn_count, snapshot.tool_calls_count, snapshot.compactions_count
            ),
            FeedbackTier::Tier2 => format!(
                "Tier 2: Complex session with errors (turns={}, tools={}, compactions={}, errors={})",
                snapshot.turn_count,
                snapshot.tool_calls_count,
                snapshot.compactions_count,
                snapshot.errors_count
            ),
            FeedbackTier::Tier3 => format!(
                "Tier 3: Recovery from friction (turns={}, cancellations={}, reverted={})",
                snapshot.turn_count, snapshot.cancellations_count, snapshot.has_reverted
            ),
        }
    }
}

impl TriggerSignalSnapshot {
    pub(crate) fn from_signals(signals: &SessionSignals) -> Self {
        Self {
            turn_count: signals.turn_count,
            tool_calls_count: signals.tool_call_count,
            compactions_count: signals.compaction_count,
            errors_count: signals.error_count,
            cancellations_count: signals.cancellation_count,
            has_reverted: signals.has_reverted,
        }
    }
}

/// Result of evaluating feedback heuristics.
#[derive(Debug, Clone)]
pub struct FeedbackEvaluation {
    /// The trigger condition if criteria were met
    pub trigger_condition: Option<TriggerCondition>,
    /// Whether feedback should actually be requested (after sampling)
    pub should_request: bool,
    /// Human-readable reason for the decision
    pub reason: String,
}

/// Feedback heuristics evaluator.
///
/// Evaluates session signals against tiered criteria to determine
/// when to request user feedback.
#[derive(Debug, Clone)]
pub struct FeedbackHeuristics {
    /// Whether feedback collection is globally enabled
    enabled: bool,

    /// Cooldown period between feedback requests (seconds)
    cooldown_seconds: u64,
    /// Maximum feedback requests per session
    max_requests_per_session: u32,

    /// Tier 1 configuration
    tier1_enabled: bool,
    tier1_sample_rate: f64,
    tier1_min_turns: u32,
    tier1_min_tool_calls: u32,
    tier1_min_compactions: u32,
    tier1_no_cancellations: bool,
    tier1_feedback_mode: FeedbackMode,
    tier1_dismissible: bool,
    tier1_prompt: String,
    tier1_max_triggers: u32,

    /// Tier 2 configuration
    tier2_enabled: bool,
    tier2_sample_rate: f64,
    tier2_min_turns: u32,
    tier2_min_tool_calls: u32,
    tier2_min_compactions: u32,
    tier2_min_errors: u32,
    tier2_feedback_mode: FeedbackMode,
    tier2_dismissible: bool,
    tier2_prompt: String,
    tier2_max_triggers: u32,

    /// Tier 3 configuration
    tier3_enabled: bool,
    tier3_sample_rate: f64,
    tier3_min_turns: u32,
    tier3_requires_cancellation: bool,
    tier3_requires_revert: bool,
    tier3_requires_recovery: bool,
    tier3_feedback_mode: FeedbackMode,
    tier3_dismissible: bool,
    tier3_prompt: String,
    tier3_max_triggers: u32,

    /// Per-tier trigger counts (replaces the old HashSet<FeedbackTier> dedup).
    /// A tier can trigger up to its configured max_triggers times (0 = unlimited).
    trigger_counts: std::collections::HashMap<FeedbackTier, u32>,

    /// Number of feedback requests sent this session
    requests_sent: u32,
    /// Time of the last feedback request (for cooldown tracking)
    last_request_time: Option<std::time::Instant>,
    /// Wall-clock timestamp of the last feedback request (for BQ ingestion)
    last_request_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl Default for FeedbackHeuristics {
    fn default() -> Self {
        Self::new()
    }
}

impl FeedbackHeuristics {
    /// Create a new heuristics evaluator with default thresholds.
    pub fn new() -> Self {
        Self {
            enabled: true,

            // Global limits
            cooldown_seconds: 300, // 5 minutes
            max_requests_per_session: 3,

            // Tier 1: Standard engagement
            tier1_enabled: true,
            tier1_sample_rate: 0.0005, // 0.05%
            tier1_min_turns: 10,
            tier1_min_tool_calls: 5,
            tier1_min_compactions: 2,
            tier1_no_cancellations: true,
            tier1_feedback_mode: FeedbackMode::Thumbs,
            tier1_dismissible: true,
            tier1_prompt:
                "You've been using EchoAgent Code productively! Would you mind sharing quick feedback?"
                    .to_string(),

            // Tier 2: Complex session with friction
            tier2_enabled: true,
            tier2_sample_rate: 0.0002, // 0.02%
            tier2_min_turns: 15,
            tier2_min_tool_calls: 10,
            tier2_min_compactions: 3,
            tier2_min_errors: 1,
            tier2_feedback_mode: FeedbackMode::ThumbsText,
            tier2_dismissible: true,
            tier2_prompt:
                "You've worked through a complex session. Your feedback would help us improve."
                    .to_string(),

            // Tier 3: Recovery or significant milestone
            tier3_enabled: true,
            tier3_sample_rate: 0.0001, // 0.01%
            tier3_min_turns: 20,
            tier3_requires_cancellation: false,
            tier3_requires_revert: false,
            tier3_requires_recovery: true, // requires cancellation OR revert
            tier3_feedback_mode: FeedbackMode::StarsText,
            tier3_dismissible: true,
            tier3_prompt:
                "Thanks for sticking with us through that session. Got a moment to share feedback?"
                    .to_string(),

            tier1_max_triggers: 1,
            tier2_max_triggers: 1,
            tier3_max_triggers: 1,
            trigger_counts: std::collections::HashMap::new(),
            requests_sent: 0,
            last_request_time: None,
            last_request_at: None,
        }
    }

    /// Create a heuristics evaluator from a remote feedback-heuristics config.
    pub fn from_config(config: &FeedbackHeuristicsConfig) -> Self {
        use echo_agent_chat_proxy_types::feedback_types::parse_feedback_mode_str;

        Self {
            enabled: config.enabled,

            // Global limits
            cooldown_seconds: config.cooldown_seconds as u64,
            max_requests_per_session: config.max_requests_per_session as u32,

            // Tier 1
            tier1_enabled: config.tier1_enabled,
            tier1_sample_rate: config.tier1_sample_rate,
            tier1_min_turns: config.tier1_min_turns as u32,
            tier1_min_tool_calls: config.tier1_min_tool_calls as u32,
            tier1_min_compactions: config.tier1_min_compactions as u32,
            tier1_no_cancellations: config.tier1_no_cancellations,
            tier1_feedback_mode: parse_feedback_mode_str(&config.tier1_feedback_mode),
            tier1_dismissible: config.tier1_dismissible,
            tier1_prompt: config.tier1_prompt.clone(),
            tier1_max_triggers: config.tier1_max_triggers as u32,

            // Tier 2
            tier2_enabled: config.tier2_enabled,
            tier2_sample_rate: config.tier2_sample_rate,
            tier2_min_turns: config.tier2_min_turns as u32,
            tier2_min_tool_calls: config.tier2_min_tool_calls as u32,
            tier2_min_compactions: config.tier2_min_compactions as u32,
            tier2_min_errors: config.tier2_min_errors as u32,
            tier2_feedback_mode: parse_feedback_mode_str(&config.tier2_feedback_mode),
            tier2_dismissible: config.tier2_dismissible,
            tier2_prompt: config.tier2_prompt.clone(),
            tier2_max_triggers: config.tier2_max_triggers as u32,

            // Tier 3
            tier3_enabled: config.tier3_enabled,
            tier3_sample_rate: config.tier3_sample_rate,
            tier3_min_turns: config.tier3_min_turns as u32,
            tier3_requires_cancellation: config.tier3_requires_cancellation,
            tier3_requires_revert: config.tier3_requires_revert,
            tier3_requires_recovery: config.tier3_requires_recovery,
            tier3_feedback_mode: parse_feedback_mode_str(&config.tier3_feedback_mode),
            tier3_dismissible: config.tier3_dismissible,
            tier3_prompt: config.tier3_prompt.clone(),
            tier3_max_triggers: config.tier3_max_triggers as u32,

            trigger_counts: std::collections::HashMap::new(),
            requests_sent: 0,
            last_request_time: None,
            last_request_at: None,
        }
    }

    /// Update the heuristics configuration from a loaded config.
    /// Preserves the triggered_tiers state and request tracking.
    pub fn update_config(&mut self, config: &FeedbackHeuristicsConfig) {
        use echo_agent_chat_proxy_types::feedback_types::parse_feedback_mode_str;

        self.enabled = config.enabled;

        // Global limits
        self.cooldown_seconds = config.cooldown_seconds as u64;
        self.max_requests_per_session = config.max_requests_per_session as u32;

        // Tier 1
        self.tier1_enabled = config.tier1_enabled;
        self.tier1_sample_rate = config.tier1_sample_rate;
        self.tier1_min_turns = config.tier1_min_turns as u32;
        self.tier1_min_tool_calls = config.tier1_min_tool_calls as u32;
        self.tier1_min_compactions = config.tier1_min_compactions as u32;
        self.tier1_no_cancellations = config.tier1_no_cancellations;
        self.tier1_feedback_mode = parse_feedback_mode_str(&config.tier1_feedback_mode);
        self.tier1_dismissible = config.tier1_dismissible;
        self.tier1_prompt = config.tier1_prompt.clone();
        self.tier1_max_triggers = config.tier1_max_triggers as u32;

        // Tier 2
        self.tier2_enabled = config.tier2_enabled;
        self.tier2_sample_rate = config.tier2_sample_rate;
        self.tier2_min_turns = config.tier2_min_turns as u32;
        self.tier2_min_tool_calls = config.tier2_min_tool_calls as u32;
        self.tier2_min_compactions = config.tier2_min_compactions as u32;
        self.tier2_min_errors = config.tier2_min_errors as u32;
        self.tier2_feedback_mode = parse_feedback_mode_str(&config.tier2_feedback_mode);
        self.tier2_dismissible = config.tier2_dismissible;
        self.tier2_prompt = config.tier2_prompt.clone();
        self.tier2_max_triggers = config.tier2_max_triggers as u32;

        // Tier 3
        self.tier3_enabled = config.tier3_enabled;
        self.tier3_sample_rate = config.tier3_sample_rate;
        self.tier3_min_turns = config.tier3_min_turns as u32;
        self.tier3_requires_cancellation = config.tier3_requires_cancellation;
        self.tier3_requires_revert = config.tier3_requires_revert;
        self.tier3_requires_recovery = config.tier3_requires_recovery;
        self.tier3_feedback_mode = parse_feedback_mode_str(&config.tier3_feedback_mode);
        self.tier3_dismissible = config.tier3_dismissible;
        self.tier3_prompt = config.tier3_prompt.clone();
        self.tier3_max_triggers = config.tier3_max_triggers as u32;
    }

    /// Check if feedback collection is globally enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Get the sample rate for a tier.
    pub fn sample_rate(&self, tier: FeedbackTier) -> f64 {
        match tier {
            FeedbackTier::Tier1 => self.tier1_sample_rate,
            FeedbackTier::Tier2 => self.tier2_sample_rate,
            FeedbackTier::Tier3 => self.tier3_sample_rate,
        }
    }

    /// Get the feedback mode for a tier.
    pub(crate) fn feedback_mode(&self, tier: FeedbackTier) -> FeedbackMode {
        match tier {
            FeedbackTier::Tier1 => self.tier1_feedback_mode,
            FeedbackTier::Tier2 => self.tier2_feedback_mode,
            FeedbackTier::Tier3 => self.tier3_feedback_mode,
        }
    }

    /// Get whether feedback requests for a tier are dismissible.
    pub fn dismissible(&self, tier: FeedbackTier) -> bool {
        match tier {
            FeedbackTier::Tier1 => self.tier1_dismissible,
            FeedbackTier::Tier2 => self.tier2_dismissible,
            FeedbackTier::Tier3 => self.tier3_dismissible,
        }
    }

    /// Get the prompt text for a tier.
    pub fn prompt(&self, tier: FeedbackTier) -> &str {
        match tier {
            FeedbackTier::Tier1 => &self.tier1_prompt,
            FeedbackTier::Tier2 => &self.tier2_prompt,
            FeedbackTier::Tier3 => &self.tier3_prompt,
        }
    }

    /// Evaluate session signals and determine if feedback should be requested.
    ///
    /// Returns the evaluation result including whether feedback should be requested
    /// and the reason. Uses probabilistic sampling based on tier rates.
    pub fn evaluate(&mut self, signals: &SessionSignals) -> FeedbackEvaluation {
        // Check if feedback is globally enabled
        if !self.enabled {
            return FeedbackEvaluation {
                trigger_condition: None,
                should_request: false,
                reason: "Feedback collection is disabled".to_string(),
            };
        }

        // Check if we've reached the max requests for this session
        if self.requests_sent >= self.max_requests_per_session {
            return FeedbackEvaluation {
                trigger_condition: None,
                should_request: false,
                reason: format!(
                    "Max feedback requests reached ({}/{})",
                    self.requests_sent, self.max_requests_per_session
                ),
            };
        }

        // Check cooldown period
        if let Some(last_time) = self.last_request_time {
            let elapsed = last_time.elapsed();
            let cooldown = std::time::Duration::from_secs(self.cooldown_seconds);
            if elapsed < cooldown {
                let remaining = cooldown - elapsed;
                return FeedbackEvaluation {
                    trigger_condition: None,
                    should_request: false,
                    reason: format!(
                        "In cooldown period ({:.0}s remaining)",
                        remaining.as_secs_f64()
                    ),
                };
            }
        }

        // Check tiers in order of priority (Tier 3 is most specific, check first)
        if let Some(condition) = self.check_tier3(signals) {
            return self.maybe_request(condition);
        }

        if let Some(condition) = self.check_tier2(signals) {
            return self.maybe_request(condition);
        }

        if let Some(condition) = self.check_tier1(signals) {
            return self.maybe_request(condition);
        }

        FeedbackEvaluation {
            trigger_condition: None,
            should_request: false,
            reason: "No feedback tier criteria met".to_string(),
        }
    }

    /// Check if a specific tier should trigger (without sampling).
    /// Used for testing or when sampling is done externally.
    pub fn check_tier(&self, tier: FeedbackTier, signals: &SessionSignals) -> bool {
        match tier {
            FeedbackTier::Tier1 => self.check_tier1(signals).is_some(),
            FeedbackTier::Tier2 => self.check_tier2(signals).is_some(),
            FeedbackTier::Tier3 => self.check_tier3(signals).is_some(),
        }
    }

    /// Mark a tier as triggered (increments trigger count).
    pub fn mark_triggered(&mut self, tier: FeedbackTier) {
        *self.trigger_counts.entry(tier).or_insert(0) += 1;
    }

    /// Reset trigger counts (e.g., for a new session).
    pub fn reset(&mut self) {
        self.trigger_counts.clear();
    }

    fn tier_exhausted(&self, tier: FeedbackTier) -> bool {
        let max = match tier {
            FeedbackTier::Tier1 => self.tier1_max_triggers,
            FeedbackTier::Tier2 => self.tier2_max_triggers,
            FeedbackTier::Tier3 => self.tier3_max_triggers,
        };
        max > 0 && self.trigger_counts.get(&tier).copied().unwrap_or(0) >= max
    }

    fn check_tier1(&self, signals: &SessionSignals) -> Option<TriggerCondition> {
        // Check if tier is enabled
        if !self.tier1_enabled {
            return None;
        }

        if self.tier_exhausted(FeedbackTier::Tier1) {
            return None;
        }

        // Tier 1: Sustained engagement without major issues
        // - At least N turns
        // - At least M tool calls
        // - At least K compactions (shows extended use)
        // - No recent cancellations (if configured)
        let cancellation_check = if self.tier1_no_cancellations {
            signals.cancellation_count == 0
        } else {
            true
        };

        if signals.turn_count >= self.tier1_min_turns
            && signals.tool_call_count >= self.tier1_min_tool_calls
            && signals.compaction_count >= self.tier1_min_compactions
            && cancellation_check
        {
            return Some(TriggerCondition::tier1(signals));
        }

        None
    }

    fn check_tier2(&self, signals: &SessionSignals) -> Option<TriggerCondition> {
        // Check if tier is enabled
        if !self.tier2_enabled {
            return None;
        }

        if self.tier_exhausted(FeedbackTier::Tier2) {
            return None;
        }

        // Tier 2: Complex session with some friction but recovery
        // - More turns than Tier 1
        // - More tool calls
        // - Has encountered errors but continued
        // - Multiple compactions
        if signals.turn_count >= self.tier2_min_turns
            && signals.tool_call_count >= self.tier2_min_tool_calls
            && signals.compaction_count >= self.tier2_min_compactions
            && signals.error_count >= self.tier2_min_errors
        {
            return Some(TriggerCondition::tier2(signals));
        }

        None
    }

    fn check_tier3(&self, signals: &SessionSignals) -> Option<TriggerCondition> {
        // Check if tier is enabled
        if !self.tier3_enabled {
            return None;
        }

        if self.tier_exhausted(FeedbackTier::Tier3) {
            return None;
        }

        // Tier 3: Recovery from significant issues
        // - Extended session
        // - Had cancellations OR reverts (shows friction then recovery)
        // - Still using the session (didn't abandon)
        let had_cancellation = signals.cancellation_count > 0;
        let had_revert = signals.has_reverted;

        // Determine if recovery signal is present based on config
        let has_recovery_signal = if self.tier3_requires_recovery {
            // Any recovery signal (cancellation OR revert) satisfies the requirement
            had_cancellation || had_revert
        } else {
            // Check specific requirements
            let cancellation_ok = !self.tier3_requires_cancellation || had_cancellation;
            let revert_ok = !self.tier3_requires_revert || had_revert;
            cancellation_ok && revert_ok
        };

        if signals.turn_count >= self.tier3_min_turns && has_recovery_signal {
            return Some(TriggerCondition::tier3(
                signals,
                had_cancellation,
                had_revert,
            ));
        }

        None
    }

    fn maybe_request(&mut self, condition: TriggerCondition) -> FeedbackEvaluation {
        let tier = condition.tier;
        // Perform probabilistic sampling using configured sample rate
        let should_sample = probabilistic_sample(self.sample_rate(tier));

        if should_sample {
            *self.trigger_counts.entry(tier).or_insert(0) += 1;
            // Track request for cooldown and max requests limits
            self.requests_sent += 1;
            self.last_request_time = Some(std::time::Instant::now());
            self.last_request_at = Some(chrono::Utc::now());
        }

        let reason = condition.trigger_reason();
        FeedbackEvaluation {
            trigger_condition: Some(condition),
            should_request: should_sample,
            reason: format!(
                "{}. {}",
                reason,
                if should_sample {
                    "Sampled for feedback."
                } else {
                    "Not sampled."
                }
            ),
        }
    }

    /// Number of feedback requests sent this session.
    pub fn requests_sent(&self) -> u32 {
        self.requests_sent
    }

    /// Wall-clock timestamp of the last feedback request sent this session.
    pub(crate) fn last_request_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.last_request_at
    }
}

/// A feedback request to be sent to the client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackRequest {
    /// Unique ID for this feedback request
    pub request_id: String,
    /// The session this request is for
    pub session_id: String,
    /// The tier that triggered this request
    pub tier: FeedbackTier,
    /// What kind of feedback to collect
    pub feedback_mode: FeedbackMode,
    pub stars: bool,
    pub thumbs: bool,
    pub text: bool,
    /// Human-readable prompt to show the user
    pub prompt: String,
    /// Whether this is a non-intrusive/dismissible request
    pub dismissible: bool,
    /// Trigger type identifier (e.g., "tier1_engagement", "tier2_complex_recovery")
    pub trigger_type: String,
    /// The specific condition that triggered this request (includes actual signal values)
    pub trigger_condition: TriggerCondition,
    /// Additional context for the client
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
}

impl FeedbackRequest {
    /// Create a new feedback request with the signals that triggered it.
    pub fn new(session_id: String, trigger_condition: TriggerCondition) -> Self {
        Self::with_mode(
            session_id,
            trigger_condition,
            FeedbackMode::Thumbs,
            true,
            None,
        )
    }

    /// Create a new feedback request with a specific feedback mode.
    pub fn with_mode(
        session_id: String,
        trigger_condition: TriggerCondition,
        feedback_mode: FeedbackMode,
        dismissible: bool,
        prompt_override: Option<&str>,
    ) -> Self {
        let request_id = uuid::Uuid::now_v7().to_string();
        let tier = trigger_condition.tier;
        let prompt = match prompt_override {
            Some(p) if !p.is_empty() => p.to_string(),
            _ => match tier {
                FeedbackTier::Tier1 => {
                    "You've been using EchoAgent Code productively! Would you mind sharing quick feedback?".to_string()
                }
                FeedbackTier::Tier2 => {
                    "You've worked through a complex session. Your feedback would help us improve.".to_string()
                }
                FeedbackTier::Tier3 => {
                    "Thanks for sticking with us through that session. Got a moment to share feedback?".to_string()
                }
            },
        };

        let (stars, thumbs, text) = match feedback_mode {
            FeedbackMode::Stars => (true, false, false),
            FeedbackMode::Thumbs => (false, true, false),
            FeedbackMode::Text => (false, false, true),
            FeedbackMode::StarsText => (true, false, true),
            FeedbackMode::ThumbsText => (false, true, true),
            _ => (false, true, false),
        };

        Self {
            request_id,
            session_id,
            tier,
            feedback_mode,
            prompt,
            dismissible,
            trigger_type: tier.trigger_type().to_string(),
            trigger_condition,
            context: None,
            stars,
            thumbs,
            text,
        }
    }

    /// Add context to the feedback request.
    pub fn with_context(mut self, context: serde_json::Value) -> Self {
        self.context = Some(context);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_signals(
        turns: u32,
        tools: u32,
        compactions: u32,
        errors: u32,
        cancellations: u32,
    ) -> SessionSignals {
        SessionSignals {
            turn_count: turns,
            tool_call_count: tools,
            compaction_count: compactions,
            error_count: errors,
            cancellation_count: cancellations,
            context_window_usage: 50,
            tools_used: vec!["read_file".to_string()],
            ..Default::default()
        }
    }

    #[test]
    fn test_tier1_criteria() {
        let heuristics = FeedbackHeuristics::new();

        // Below threshold - should not trigger
        let signals = make_signals(5, 3, 1, 0, 0);
        assert!(!heuristics.check_tier(FeedbackTier::Tier1, &signals));

        // Meets threshold - should trigger (if no cancellations)
        let signals = make_signals(10, 5, 2, 0, 0);
        assert!(heuristics.check_tier(FeedbackTier::Tier1, &signals));

        // Has cancellations - should not trigger Tier 1
        let signals = make_signals(10, 5, 2, 0, 1);
        assert!(!heuristics.check_tier(FeedbackTier::Tier1, &signals));
    }

    #[test]
    fn test_tier2_criteria() {
        let heuristics = FeedbackHeuristics::new();

        // Below threshold - should not trigger
        let signals = make_signals(10, 5, 2, 0, 0);
        assert!(!heuristics.check_tier(FeedbackTier::Tier2, &signals));

        // Meets threshold - should trigger (requires errors)
        let signals = make_signals(15, 10, 3, 1, 0);
        assert!(heuristics.check_tier(FeedbackTier::Tier2, &signals));

        // No errors - should not trigger
        let signals = make_signals(15, 10, 3, 0, 0);
        assert!(!heuristics.check_tier(FeedbackTier::Tier2, &signals));
    }

    #[test]
    fn test_tier3_criteria() {
        let heuristics = FeedbackHeuristics::new();

        // Below turn threshold - should not trigger
        let signals = make_signals(15, 10, 3, 1, 1);
        assert!(!heuristics.check_tier(FeedbackTier::Tier3, &signals));

        // Meets threshold with cancellation - should trigger
        let signals = make_signals(20, 10, 3, 1, 1);
        assert!(heuristics.check_tier(FeedbackTier::Tier3, &signals));

        // Meets threshold with revert - should trigger
        let mut signals = make_signals(20, 10, 3, 1, 0);
        signals.has_reverted = true;
        assert!(heuristics.check_tier(FeedbackTier::Tier3, &signals));

        // No recovery signal - should not trigger
        let signals = make_signals(20, 10, 3, 1, 0);
        assert!(!heuristics.check_tier(FeedbackTier::Tier3, &signals));
    }

    #[test]
    fn test_tier_deduplication() {
        let mut heuristics = FeedbackHeuristics::new();

        let signals = make_signals(10, 5, 2, 0, 0);

        // First evaluation should find Tier 1
        let eval = heuristics.evaluate(&signals);
        assert!(eval.trigger_condition.is_some());
        assert_eq!(
            eval.trigger_condition.as_ref().unwrap().tier,
            FeedbackTier::Tier1
        );

        // Mark as triggered (simulating that we sent the request)
        heuristics.mark_triggered(FeedbackTier::Tier1);

        // Second evaluation with same signals should not trigger Tier 1 again
        let eval = heuristics.evaluate(&signals);
        assert!(
            eval.trigger_condition.is_none()
                || eval.trigger_condition.as_ref().unwrap().tier != FeedbackTier::Tier1
        );
    }

    #[test]
    fn test_feedback_request_creation() {
        let signals = make_signals(10, 5, 2, 0, 0);
        let condition = TriggerCondition::tier1(&signals);
        let request = FeedbackRequest::new("session-123".to_string(), condition);

        assert!(!request.request_id.is_empty());
        assert_eq!(request.session_id, "session-123");
        assert_eq!(request.tier, FeedbackTier::Tier1);
        assert!(request.dismissible);
        assert!(request.prompt.contains("productively"));
        assert_eq!(request.trigger_type, "tier1_engagement");
        assert_eq!(
            request.trigger_condition.condition,
            "turns >= 10 AND tool_calls >= 5 AND compactions >= 2 AND cancellations == 0"
        );
        assert!(
            request
                .trigger_condition
                .trigger_reason()
                .contains("turns=10")
        );
        assert!(
            request
                .trigger_condition
                .trigger_reason()
                .contains("tools=5")
        );

        // Test Tier2
        let signals2 = make_signals(15, 10, 3, 1, 0);
        let condition2 = TriggerCondition::tier2(&signals2);
        let request2 = FeedbackRequest::new("session-456".to_string(), condition2);
        assert_eq!(request2.trigger_type, "tier2_complex_recovery");
        assert_eq!(
            request2.trigger_condition.condition,
            "turns >= 15 AND tool_calls >= 10 AND compactions >= 3 AND errors >= 1"
        );

        // Test Tier3
        let mut signals3 = make_signals(20, 10, 3, 1, 2);
        signals3.has_reverted = true;
        let condition3 = TriggerCondition::tier3(&signals3, true, true);
        let request3 = FeedbackRequest::new("session-789".to_string(), condition3);
        assert_eq!(request3.trigger_type, "tier3_friction_recovery");
        assert!(request3.trigger_condition.condition.contains("turns >= 20"));
    }

    #[test]
    fn test_sample_rate_values() {
        assert_eq!(FeedbackTier::Tier1.sample_rate(), 0.0005);
        assert_eq!(FeedbackTier::Tier2.sample_rate(), 0.0002);
        assert_eq!(FeedbackTier::Tier3.sample_rate(), 0.0001);
    }

    #[test]
    fn test_feedback_request_non_dismissible() {
        let signals = make_signals(15, 10, 3, 1, 0);
        let condition = TriggerCondition::tier2(&signals);
        let request = FeedbackRequest::with_mode(
            "session-mandatory".to_string(),
            condition,
            FeedbackMode::StarsText,
            false,
            None,
        );
        assert!(!request.dismissible);
        assert_eq!(request.tier, FeedbackTier::Tier2);
    }

    #[test]
    fn test_heuristics_dismissible_per_tier() {
        let mut heuristics = FeedbackHeuristics::new();

        // Defaults are all true
        assert!(heuristics.dismissible(FeedbackTier::Tier1));
        assert!(heuristics.dismissible(FeedbackTier::Tier2));
        assert!(heuristics.dismissible(FeedbackTier::Tier3));

        // Override from config
        let config = FeedbackHeuristicsConfig {
            tier2_dismissible: false,
            tier3_dismissible: false,
            ..FeedbackHeuristicsConfig::default()
        };
        heuristics.update_config(&config);

        assert!(heuristics.dismissible(FeedbackTier::Tier1));
        assert!(!heuristics.dismissible(FeedbackTier::Tier2));
        assert!(!heuristics.dismissible(FeedbackTier::Tier3));
    }

    #[test]
    fn test_tier_repeatable_when_max_triggers_zero() {
        let mut h = FeedbackHeuristics::new();
        h.tier1_max_triggers = 0; // unlimited
        h.tier1_sample_rate = 1.0;
        h.cooldown_seconds = 0;
        h.max_requests_per_session = 100;
        h.tier1_min_turns = 1;
        h.tier1_min_tool_calls = 0;
        h.tier1_min_compactions = 0;
        h.tier1_no_cancellations = false;
        h.tier2_enabled = false;
        h.tier3_enabled = false;

        let signals = make_signals(5, 3, 1, 0, 0);

        // With max_triggers=0 (unlimited), tier should fire on every evaluation
        for i in 0..5 {
            let eval = h.evaluate(&signals);
            assert!(
                eval.should_request,
                "iteration {i}: unlimited tier should keep triggering"
            );
        }
        assert_eq!(
            h.trigger_counts
                .get(&FeedbackTier::Tier1)
                .copied()
                .unwrap_or(0),
            5
        );
    }

    #[test]
    fn test_tier_limited_to_max_triggers() {
        let mut h = FeedbackHeuristics::new();
        h.tier1_max_triggers = 3;
        h.tier1_sample_rate = 1.0;
        h.cooldown_seconds = 0;
        h.max_requests_per_session = 100;
        h.tier1_min_turns = 1;
        h.tier1_min_tool_calls = 0;
        h.tier1_min_compactions = 0;
        h.tier1_no_cancellations = false;
        h.tier2_enabled = false;
        h.tier3_enabled = false;

        let signals = make_signals(5, 3, 1, 0, 0);

        // Should fire exactly 3 times
        for i in 0..3 {
            let eval = h.evaluate(&signals);
            assert!(eval.should_request, "iteration {i}: should trigger");
        }
        // 4th should be blocked
        let eval = h.evaluate(&signals);
        assert!(!eval.should_request, "4th trigger should be blocked");
    }

    #[test]
    fn test_default_max_triggers_preserves_dedup() {
        let mut h = FeedbackHeuristics::new();
        h.tier1_sample_rate = 1.0;
        h.cooldown_seconds = 0;
        h.max_requests_per_session = 100;
        h.tier1_min_turns = 1;
        h.tier1_min_tool_calls = 0;
        h.tier1_min_compactions = 0;
        h.tier1_no_cancellations = false;
        h.tier2_enabled = false;
        h.tier3_enabled = false;
        // max_triggers defaults to 1

        let signals = make_signals(5, 3, 1, 0, 0);

        let eval = h.evaluate(&signals);
        assert!(eval.should_request, "first trigger should fire");

        let eval = h.evaluate(&signals);
        assert!(
            !eval.should_request,
            "second trigger should be blocked (max_triggers=1)"
        );
    }

    #[test]
    fn test_mixed_tier_max_triggers() {
        let mut h = FeedbackHeuristics::new();
        h.tier1_max_triggers = 0; // unlimited
        h.tier2_max_triggers = 2;
        // tier3_max_triggers stays at 1 (default)
        h.tier1_sample_rate = 1.0;
        h.tier2_sample_rate = 1.0;
        h.tier3_sample_rate = 1.0;
        h.cooldown_seconds = 0;
        h.max_requests_per_session = 100;
        // Set thresholds so all tiers can fire
        h.tier1_min_turns = 1;
        h.tier1_min_tool_calls = 0;
        h.tier1_min_compactions = 0;
        h.tier1_no_cancellations = false;
        h.tier2_min_turns = 1;
        h.tier2_min_tool_calls = 0;
        h.tier2_min_compactions = 0;
        h.tier2_min_errors = 0;
        h.tier3_min_turns = 1;
        h.tier3_requires_recovery = false;
        h.tier3_requires_cancellation = false;
        h.tier3_requires_revert = false;

        let signals = make_signals(10, 5, 2, 1, 0);

        // Tier 3 should exhaust after 1 trigger (evaluate checks tiers in 3→2→1 order)
        let eval = h.evaluate(&signals);
        assert!(eval.should_request);
        assert_eq!(
            eval.trigger_condition.as_ref().unwrap().tier,
            FeedbackTier::Tier3
        );

        // Tier 2 should fire next (tier3 exhausted)
        let eval = h.evaluate(&signals);
        assert!(eval.should_request);
        assert_eq!(
            eval.trigger_condition.as_ref().unwrap().tier,
            FeedbackTier::Tier2
        );

        // Tier 2 fires again (max_triggers=2)
        let eval = h.evaluate(&signals);
        assert!(eval.should_request);
        assert_eq!(
            eval.trigger_condition.as_ref().unwrap().tier,
            FeedbackTier::Tier2
        );

        // Tier 2 exhausted → falls through to tier 1 (unlimited)
        let eval = h.evaluate(&signals);
        assert!(eval.should_request);
        assert_eq!(
            eval.trigger_condition.as_ref().unwrap().tier,
            FeedbackTier::Tier1
        );

        // Tier 1 keeps firing
        let eval = h.evaluate(&signals);
        assert!(eval.should_request);
        assert_eq!(
            eval.trigger_condition.as_ref().unwrap().tier,
            FeedbackTier::Tier1
        );
    }
}
