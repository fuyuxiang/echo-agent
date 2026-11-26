//! Shared reasoning-effort gate for the session lifecycle.

use agent_client_protocol as acp;
use echo_agent_sampler::SamplerConfig;
use echo_agent_sampling_types::ReasoningEffort;

use crate::agent::models::ModelsManager;

/// Where a resolved effort applies in the session lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EffortTarget {
    NewSession,
    ModelSwitch,
    SummaryClient,
}

impl EffortTarget {
    fn as_str(self) -> &'static str {
        match self {
            Self::NewSession => "new_session",
            Self::ModelSwitch => "model_switch",
            Self::SummaryClient => "summary",
        }
    }
}

impl ModelsManager {
    /// Gate `effort` against `sampling`'s model and, when supported, assign it.
    pub(crate) fn apply_supported_effort(
        &self,
        sampling: &mut SamplerConfig,
        effort: Option<ReasoningEffort>,
        session_id: &acp::SessionId,
        target: EffortTarget,
    ) {
        let Some(effort) = effort else {
            return;
        };
        let profile = crate::agent::model_capabilities::model_capability_profile(
            &sampling.model,
            &sampling.base_url,
            &sampling.api_backend,
        );
        if !self.model_supports_reasoning_effort(&sampling.model)
            || profile.is_some_and(|profile| !profile.reasoning_efforts.contains(&effort))
        {
            // SummaryClient stays silent on unsupported; the spawn/switch path already warns the same hint.
            if matches!(target, EffortTarget::NewSession | EffortTarget::ModelSwitch) {
                tracing::warn!(
                    session_id = %session_id.0,
                    model = %sampling.model,
                    effort = %effort,
                    "reasoning_effort: model does not support effort; ignoring it"
                );
            }
            return;
        }
        // Same fields at every target; only the level differs. tracing bakes the
        // level into a static callsite, so match a const level per arm.
        macro_rules! log_applied {
            ($level:expr) => {
                tracing::event!(
                    $level,
                    session_id = %session_id.0,
                    model = %sampling.model,
                    effort = %effort,
                    target = %target.as_str(),
                    "reasoning_effort: applied effort"
                )
            };
        }
        match target {
            EffortTarget::NewSession | EffortTarget::ModelSwitch => {
                log_applied!(tracing::Level::INFO)
            }
            EffortTarget::SummaryClient => log_applied!(tracing::Level::DEBUG),
        }
        sampling.reasoning_effort = Some(effort);
    }
}

/// Where a `session/new` reasoning-effort hint is routed. At most one variant
/// carries the hint, so the spawn and switch consumers can never both fire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NewSessionEffort {
    /// Seed the spawned session's sampling config (default-model path).
    Spawn(ReasoningEffort),
    /// Apply after spawn through the model switch (explicit `modelId` path).
    Switch(ReasoningEffort),
    None,
}

/// Resolve the effort hint for `session/new`.
///
/// Precedence: an explicit `_meta.reasoningEffort` wins over the process-wide
/// last-used / `[models].default_reasoning_effort` value
/// ([`ModelsManager::current_reasoning_effort`]). The catalog default is the
/// last resort and is left on the sampling config when this returns `None`.
pub(crate) fn resolve_new_session_effort_hint(
    meta_hint: Option<ReasoningEffort>,
    current: Option<ReasoningEffort>,
) -> Option<ReasoningEffort> {
    meta_hint.or(current)
}

/// Route a `session/new` reasoning-effort hint to exactly one consumer.
///
/// New-session precedence: `_meta.reasoningEffort` wins over last-used /
/// config default, which wins over the model's catalog default. An explicit
/// `modelId` routes the hint to the post-spawn model switch; the default-model
/// path seeds it at spawn.
pub(crate) fn split_new_session_effort(
    resolved_custom_model: Option<&str>,
    hint: Option<ReasoningEffort>,
) -> NewSessionEffort {
    match hint {
        None => NewSessionEffort::None,
        Some(effort) if resolved_custom_model.is_some() => NewSessionEffort::Switch(effort),
        Some(effort) => NewSessionEffort::Spawn(effort),
    }
}
