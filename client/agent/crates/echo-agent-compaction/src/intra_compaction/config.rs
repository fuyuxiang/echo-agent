//! Configuration for intra-compaction.

use serde::{Deserialize, Serialize};

/// Which targets intra-compaction may compact.
///
/// - `FullReplace` (default): echo-agent-build's full-replace strategy — summarize
///   the *whole* conversation (prior history + accumulated steps) and rebuild
///   context from scratch as `[system] + [summary]`. Drives the shared
///   `code_compaction` summarizer directly; no tail is kept.
/// - `StepsOnly`: only compact accumulated step turns within the current
///   agent loop (keeps the recent tail).
/// - `HistoryOnly`: only compact prior conversation history; leave the
///   current loop's accumulated step turns alone.
/// - `HistoryThenSteps`: compact history first, then — only if the
///   accumulated step turns still account for a large enough share of the
///   prompt (controlled by [`IntraCompactionConfig::steps_trigger_ratio`]) —
///   also compact the current loop's steps.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntraCompactionMode {
    #[default]
    FullReplace,
    StepsOnly,
    HistoryOnly,
    HistoryThenSteps,
}

/// Which *summarization algorithm* intra-compaction uses to turn the selected
/// turns into the replacement summary. Orthogonal to [`IntraCompactionMode`]
/// (which picks *what* to compact); this picks *how* the summary is produced.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntraSummarizer {
    /// New (default): the shared summarization core — `build_summary_prompt`
    /// + degenerate-reject + `format_compact_summary` cleaning.
    #[default]
    Shared,
    /// Previous intra algorithm: per-target prompt (`format_compaction_prompt`
    /// / history dev+user prompts), no cleaning. Kept for switchability.
    Legacy,
}

/// Intra-compaction configuration for an agent's sample loop.
///
/// This is the intra-compaction analog of
/// [`InterCompactionConfig`](crate::inter_compaction::InterCompactionConfig).
/// The structural difference is *where the config lives*:
/// - inter-compaction runs as a singleton between-turn service, so it has one
///   global config resolved from service YAML.
/// - intra-compaction runs **per-agent** inside the harness sampler loop, so
///   this struct is embedded directly in each agent's spec. Defaults come from
///   the [`Default`] impl below; an agent can optionally override them under
///   `agents.<name>.intra_compaction` in agent config YAML (none set today).
///   There is no standalone service config for it.
///
/// When `enabled = false` (default), no intra-compaction runs for that agent.
///
/// Uses **percentage** thresholds (borrowed from echo-agent-shell's
/// `CompactionPolicy`) for portability across models with different context
/// windows.
///
/// The fields are split into two groups: a **common** block that every mode
/// stores (enablement, trigger gate fields, reduction guards, the compaction
/// LLM call, audit) and a **mode-specific** block whose fields are each read by
/// only a subset of modes (see the per-field `[...]` tags). In particular,
/// `FullReplace` — the default — ignores `min_steps_before_compact` at trigger
/// time (token threshold only, matching echo-agent-build) and also ignores
/// `summarizer`, `target_threshold_percent`, `steps_trigger_ratio`, and
/// `user_message_truncate_chars`. The field remains on this config for all
/// modes (YAML / remote agent config / defaults); only enforcement is mode-dependent.
///
/// **Unset / blank → default.** Every field has a default value (the [`Default`]
/// impl below). Leaving a field unset — absent in YAML, or blank in an agent
/// config editor — keeps that default; each field's doc states its default
/// inline as `Default: …`. Note that remote agent-config protos may only
/// surface a *subset* of these fields (`enabled`, `mode`,
/// `trigger_threshold_percent`, `target_threshold_percent`,
/// `min_steps_before_compact` [ignored by FullReplace], `steps_trigger_ratio`
/// [HistoryThenSteps], `compaction_model_name`); the remaining fields are
/// never sent remotely and therefore always take the defaults here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IntraCompactionConfig {
    // ───────────────────────────── Common (all modes) ─────────────────────────────
    // Present on every config path regardless of `mode`. Some trigger fields
    // are ignored under FullReplace (see per-field docs).

    // -- Enablement & strategy selection --
    /// Enable intra-compaction between steps. Default: `false` (disabled).
    pub enabled: bool,

    /// Which targets intra-compaction may compact. See [`IntraCompactionMode`].
    /// Default: `FullReplace`.
    pub mode: IntraCompactionMode,

    // -- Trigger gating: when a compaction pass fires (see `should_compact`) --
    /// Context window usage percentage (0-100) that triggers compaction.
    /// Compared against: `last_prompt_tokens / context_length.max_len`.
    /// Default: `85`.
    pub trigger_threshold_percent: u8,

    /// Minimum number of completed steps before compaction can trigger.
    /// Default: `3`. Always stored on [`IntraCompactionConfig`]; agent YAML
    /// may set it for any mode.
    ///
    /// **Enforcement:** applied for `StepsOnly` / `HistoryOnly` /
    /// `HistoryThenSteps`. **Ignored** when [`mode`](Self::mode) is
    /// [`IntraCompactionMode::FullReplace`] (token threshold alone, same idea
    /// as echo-agent-build full-replace auto-compact). Worthless early passes are
    /// still limited by [`min_compactable_tokens`](Self::min_compactable_tokens)
    /// / reduction guards after a trigger.
    pub min_steps_before_compact: u32,

    // -- Reduction guards: whether a produced summary is worth keeping --
    /// Minimum tokens that must be reducible before compaction is worth
    /// running. Below this, the LLM overhead outweighs the savings.
    /// Default: `5000`.
    pub min_compactable_tokens: u32,

    /// Discard the compaction if it didn't shrink tokens below this ratio.
    /// Matches inter-compaction's `0.8` (= 20% minimum reduction) guard.
    /// Default: `0.8`.
    pub max_reduction_ratio: f64,

    // -- Compaction LLM call (sampling) --
    /// Compaction model name. Blank/`None` → [`DEFAULT_COMPACTION_MODEL_NAME`].
    /// Prefer [`Self::effective_compaction_model_name`].
    pub compaction_model_name: Option<String>,

    /// End-to-end timeout for the compaction LLM call.
    /// `120` by default, aligned with the inter-compaction service default.
    /// Default: `120`.
    pub sampling_timeout_secs: u64,

    /// Max attempts for the compaction LLM call (effective value is `max(1)`).
    /// This is the *total* number of tries, not retries-on-top: `2` (default)
    /// = first try + one retry on a transient failure (timeout / empty / stream
    /// / start), with `retry_delay_secs` between tries. Matches the
    /// inter-compaction service default. Default: `2`.
    pub max_attempts: u32,
    /// Delay between retries. Default: `3`.
    pub retry_delay_secs: u64,

    // -- Audit --
    /// Version string for the compaction (e.g. `"intra-v1"`).
    /// Recorded in audit logs. Default: `"intra-v1"`.
    pub compaction_version: String,

    // ───────────────────────────── Mode-specific ─────────────────────────────
    // Each field below is read by only a subset of modes; the other modes
    // ignore it entirely. The bracketed `[...]` tag on each doc names the modes
    // that consume it.

    // -- Partial modes only: StepsOnly / HistoryOnly / HistoryThenSteps.
    //    FullReplace ignores both `summarizer` and `target_threshold_percent` —
    //    it always uses the shared summarizer and replaces the whole
    //    conversation, so it keeps no tail and never reads a target threshold. --
    /// [StepsOnly / HistoryOnly / HistoryThenSteps] Which summarization
    /// algorithm to use. See [`IntraSummarizer`]. Default: [`IntraSummarizer::Shared`].
    ///
    /// Ignored by `FullReplace`, which *is* the shared `code_compaction` path
    /// and always summarizes via `Shared` regardless of this value. (Not
    /// always exposed by remote agent-config protos — defaults apply there.)
    pub summarizer: IntraSummarizer,

    /// [StepsOnly / HistoryOnly / HistoryThenSteps] Target usage percentage
    /// after compaction. The compactor keeps enough recent turns to bring usage
    /// below this. Default: `50`.
    ///
    /// Only used by the partial modes for tail-keep selection; `FullReplace`
    /// replaces everything and never reads it.
    pub target_threshold_percent: u8,

    // -- HistoryThenSteps only --
    /// [HistoryThenSteps mode] Only compact accumulated step turns when their
    /// token count exceeds this fraction of the history token count.
    ///
    /// Rationale: when both history and current steps are large, compacting
    /// history first usually buys enough budget. Compacting recent steps
    /// loses fine-grained context (tool results, code snippets, recent
    /// errors) and should only happen when steps themselves are large
    /// relative to history.
    ///
    /// At `0.0`, steps are always compacted (after history). At very large
    /// values, steps compaction is effectively disabled in
    /// `HistoryThenSteps` mode. Default: `0.3`.
    pub steps_trigger_ratio: f64,

    // -- History target only: HistoryOnly + HistoryThenSteps' history pass.
    //    Ignored by FullReplace and StepsOnly (neither emits a user-queries
    //    preamble). --
    /// [HistoryOnly / HistoryThenSteps] Character threshold above which an
    /// original user message gets middle-truncated when included in the
    /// `<echo_agent_user_queries>` preamble prepended to the history compaction
    /// summary. Mirrors the inter-compaction Basic threshold. Has no
    /// effect for `Steps` target — steps compaction has no user-queries
    /// preamble. Default: `3000`. (Not always exposed by remote agent-config
    /// protos — defaults apply there.)
    pub user_message_truncate_chars: u32,
}

/// Code-level default compaction model name (last resort).
///
/// Override order: agent field (non-blank) → service YAML (inter) /
/// agent config → this constant. See crate-level docs on
/// [`crate::DEFAULT_COMPACTION_MODEL_NAME`].
pub const DEFAULT_COMPACTION_MODEL_NAME: &str = "echo-agent-4.20";

impl IntraCompactionConfig {
    /// Agent field; blank/`None` → [`DEFAULT_COMPACTION_MODEL_NAME`].
    pub fn effective_compaction_model_name(&self) -> &str {
        self.compaction_model_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_COMPACTION_MODEL_NAME)
    }
}

impl Default for IntraCompactionConfig {
    fn default() -> Self {
        // These are the unset/blank defaults: the value each field takes when it
        // is absent in YAML or left blank in an agent config editor.
        Self {
            // Common (all modes; min_steps stored always, enforced except FullReplace)
            enabled: false,
            mode: IntraCompactionMode::default(),
            trigger_threshold_percent: 85,
            min_steps_before_compact: 3,
            min_compactable_tokens: 5_000,
            max_reduction_ratio: 0.8,
            compaction_model_name: Some(DEFAULT_COMPACTION_MODEL_NAME.to_string()),
            sampling_timeout_secs: 120,
            max_attempts: 2,
            retry_delay_secs: 3,
            compaction_version: "intra-v1".to_string(),
            // Mode-specific
            summarizer: IntraSummarizer::default(),
            target_threshold_percent: 50,
            steps_trigger_ratio: 0.3,
            user_message_truncate_chars: 3_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_disabled() {
        let p = IntraCompactionConfig::default();
        assert!(!p.enabled);
        assert_eq!(p.mode, IntraCompactionMode::FullReplace);
        assert_eq!(p.summarizer, IntraSummarizer::Shared);
        assert_eq!(p.trigger_threshold_percent, 85);
        assert_eq!(p.target_threshold_percent, 50);
        assert_eq!(
            p.compaction_model_name.as_deref(),
            Some(DEFAULT_COMPACTION_MODEL_NAME)
        );
        assert_eq!(
            p.effective_compaction_model_name(),
            DEFAULT_COMPACTION_MODEL_NAME
        );
        assert_eq!(p.max_attempts, 2);
        assert_eq!(p.retry_delay_secs, 3);
        assert!((p.steps_trigger_ratio - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn blank_or_none_compaction_model_name_uses_default() {
        let none = IntraCompactionConfig {
            compaction_model_name: None,
            ..Default::default()
        };
        assert_eq!(
            none.effective_compaction_model_name(),
            DEFAULT_COMPACTION_MODEL_NAME
        );
        let empty = IntraCompactionConfig {
            compaction_model_name: Some(String::new()),
            ..Default::default()
        };
        assert_eq!(
            empty.effective_compaction_model_name(),
            DEFAULT_COMPACTION_MODEL_NAME
        );
        let ws = IntraCompactionConfig {
            compaction_model_name: Some("  ".into()),
            ..Default::default()
        };
        assert_eq!(
            ws.effective_compaction_model_name(),
            DEFAULT_COMPACTION_MODEL_NAME
        );
        let custom = IntraCompactionConfig {
            compaction_model_name: Some("custom-model".into()),
            ..Default::default()
        };
        assert_eq!(custom.effective_compaction_model_name(), "custom-model");
    }

    #[test]
    fn default_mode_is_full_replace() {
        assert_eq!(
            IntraCompactionMode::default(),
            IntraCompactionMode::FullReplace
        );
    }

    #[test]
    fn mode_serde_round_trip() {
        for (mode, s) in [
            (IntraCompactionMode::FullReplace, "\"full_replace\""),
            (IntraCompactionMode::StepsOnly, "\"steps_only\""),
            (IntraCompactionMode::HistoryOnly, "\"history_only\""),
            (
                IntraCompactionMode::HistoryThenSteps,
                "\"history_then_steps\"",
            ),
        ] {
            let json = serde_json::to_string(&mode).unwrap();
            assert_eq!(json, s);
            let back: IntraCompactionMode = serde_json::from_str(s).unwrap();
            assert_eq!(back, mode);
        }
    }

    #[test]
    fn summarizer_defaults_to_shared() {
        assert_eq!(IntraSummarizer::default(), IntraSummarizer::Shared);
        assert_eq!(
            IntraCompactionConfig::default().summarizer,
            IntraSummarizer::Shared
        );
    }

    #[test]
    fn summarizer_serde_round_trip() {
        for (s, json) in [
            (IntraSummarizer::Shared, "\"shared\""),
            (IntraSummarizer::Legacy, "\"legacy\""),
        ] {
            assert_eq!(serde_json::to_string(&s).unwrap(), json);
            let back: IntraSummarizer = serde_json::from_str(json).unwrap();
            assert_eq!(back, s);
        }
    }

    #[test]
    fn json_round_trip_with_serde_default() {
        // Partial JSON — `#[serde(default)]` fills missing fields.
        let json = r#"{
            "enabled": true,
            "trigger_threshold_percent": 80
        }"#;
        let p: IntraCompactionConfig = serde_json::from_str(json).unwrap();
        assert!(p.enabled);
        assert_eq!(p.trigger_threshold_percent, 80);
        // Defaults preserved.
        assert_eq!(p.target_threshold_percent, 50);
        assert_eq!(p.compaction_version, "intra-v1");
    }
}
