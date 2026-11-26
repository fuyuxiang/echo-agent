//! Documented defaults for known model and endpoint combinations.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use echo_agent_sampling_types::{ApiBackend, ReasoningEffort};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCapabilityProfile {
    model: String,
    endpoints: BTreeMap<String, Vec<String>>,
    pub tools: Option<bool>,
    pub images: Option<bool>,
    pub reasoning_efforts: Vec<ReasoningEffort>,
}

pub fn model_capability_profile(
    model: &str,
    base_url: &str,
    backend: &ApiBackend,
) -> Option<&'static ModelCapabilityProfile> {
    static PROFILES: OnceLock<Vec<ModelCapabilityProfile>> = OnceLock::new();
    let profiles = PROFILES.get_or_init(|| {
        serde_json::from_str(include_str!("model_capability_profiles.json"))
            .expect("bundled model capability profiles must deserialize")
    });
    let endpoint = base_url.trim().trim_end_matches('/');
    let protocol = match backend {
        ApiBackend::ChatCompletions => "chat_completions",
        ApiBackend::Responses => "responses",
        ApiBackend::Messages => "messages",
    };
    profiles.iter().find(|profile| {
        profile.model.eq_ignore_ascii_case(model.trim())
            && profile
                .endpoints
                .get(protocol)
                .is_some_and(|entries| entries.iter().any(|entry| entry == endpoint))
    })
}

/// Capability declarations captured for a desktop session's exact catalog entry.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct LiveModelCapabilities {
    model: String,
    base_url: String,
    backend: ApiBackend,
    pub(crate) tools: Option<bool>,
    pub(crate) supports_effort: bool,
    pub(crate) default_effort: Option<ReasoningEffort>,
    pub(crate) efforts: Vec<ReasoningEffort>,
}

impl From<&super::config::ModelEntry> for LiveModelCapabilities {
    fn from(entry: &super::config::ModelEntry) -> Self {
        let model = entry.info();
        Self {
            model: model.model.clone(),
            base_url: entry
                .api_base_url
                .as_deref()
                .unwrap_or(&model.base_url)
                .to_owned(),
            backend: model.api_backend.clone(),
            tools: model.supports_tool_calls,
            supports_effort: model.supports_reasoning_effort,
            default_effort: model.reasoning_effort,
            efforts: model
                .reasoning_efforts
                .iter()
                .map(|option| option.value)
                .collect(),
        }
    }
}

impl LiveModelCapabilities {
    pub(crate) fn same_route(&self, other: &Self) -> bool {
        self.model == other.model
            && self.base_url == other.base_url
            && self.backend == other.backend
    }

    pub(crate) fn apply_change(
        &self,
        previous: &Self,
        config: &mut echo_agent_sampling_types::SamplingConfig,
    ) {
        config.supports_tool_calls = self.tools;
        let keep_session_effort = previous.supports_effort
            && config.reasoning_effort != previous.default_effort
            && config
                .reasoning_effort
                .is_some_and(|effort| self.efforts.is_empty() || self.efforts.contains(&effort));
        if !self.supports_effort {
            config.reasoning_effort = None;
        } else if !keep_session_effort {
            config.reasoning_effort = self.default_effort;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updates_capabilities_without_overwriting_an_unchanged_session_effort() {
        use echo_agent_sampling_types::SamplingConfig;
        let previous = LiveModelCapabilities {
            model: "model-a".into(),
            base_url: "https://example.test/v1".into(),
            backend: ApiBackend::ChatCompletions,
            tools: Some(true),
            supports_effort: true,
            default_effort: Some(ReasoningEffort::Low),
            efforts: vec![ReasoningEffort::Low, ReasoningEffort::High],
        };
        let mut sampling = SamplingConfig {
            base_url: previous.base_url.clone(),
            model: previous.model.clone(),
            max_completion_tokens: None,
            temperature: None,
            top_p: None,
            api_backend: previous.backend.clone(),
            extra_headers: Default::default(),
            query_params: Default::default(),
            env_http_headers: Default::default(),
            context_window: std::num::NonZeroU64::new(8192).unwrap(),
            reasoning_effort: Some(ReasoningEffort::High),
            stream_tool_calls: None,
            supports_tool_calls: Some(true),
        };
        let mut updated = previous.clone();
        updated.tools = Some(false);
        updated.apply_change(&previous, &mut sampling);
        assert_eq!(sampling.supports_tool_calls, Some(false));
        assert_eq!(sampling.reasoning_effort, Some(ReasoningEffort::High));

        let mut disabled = updated.clone();
        disabled.supports_effort = false;
        disabled.apply_change(&updated, &mut sampling);
        assert_eq!(sampling.reasoning_effort, None);

        disabled.base_url = "https://example.test/new/v1".into();
        assert!(!updated.same_route(&disabled));
    }
}
