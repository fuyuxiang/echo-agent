//! The registry of boolean `[features]` keys.
//!
//! One row per feature spells it on every surface that can set it, and
//! [`Feature::resolve`] is the only place precedence lives. A key needing
//! different precedence keeps its own resolver, as `remote_fetch` does.

use crate::{
    RemoteSettings,
    flags::{BoolFlag, ConfigSource, Resolved},
};
use echo_agent_config::env_bool;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, strum::EnumIter)]
pub enum Feature {
    /// The SQLite session-search index.
    SessionSearch,
    /// Language-server-backed navigation tools.
    LspTools,
    /// The `web_fetch` tool.
    WebFetch,
    /// `/recap` and the automatic return-from-away recap.
    SessionRecap,
    /// The `ask_user_question` tool.
    AskUserQuestion,
    /// Voice dictation (speech to text).
    VoiceMode,
    /// The `write_file` tool.
    WriteFile,
    /// Heuristic feedback popups and the `/feedback` command.
    Feedback,
    /// The `/feedback` trace-consent card (the trace-upload opt-in offer).
    FeedbackTraceCard,
    /// The per-turn summary on the agent dashboard.
    TurnSummary,
    /// Ctrl+C before a turn's first activity restores the prompt.
    CancelRewind,
    /// Summarize the verbatim conversation rather than a shortened copy of it.
    CompactionVerbatimInput,
    /// Summarize the earlier part of a long conversation in the background, before compaction.
    TwoPassCompaction,
    /// Server-side execution of `web_search` and `x_search`.
    BackendTools,
    /// Continue the conversation as soon as a background task or subagent finishes.
    AutoWake,
    /// Save a finished subagent's working copy into the repo as a git ref, restored on resume.
    SubagentWorktreeSnapshot,
}

/// How one feature is written on each surface it can be set from.
#[derive(Debug, Clone, Copy)]
pub struct FeatureSpec {
    pub id: Feature,
    pub key: &'static str,
    pub path: &'static str,
    pub env: &'static str,
    pub default_enabled: bool,
    /// `None` where the key has no remote tier, so adding one is a deliberate edit.
    pub remote: Option<fn(&RemoteSettings) -> Option<bool>>,
    // No managed tier: `config` is the loader's merge, where a user's
    // config.toml already beats managed_config.toml.
}

/// What each tier had to say about one feature.
#[derive(Debug, Clone, Copy, Default)]
pub struct FeatureSources {
    pub pin: Option<bool>,
    pub env: Option<bool>,
    pub config: Option<bool>,
    /// Already projected, by [`Feature::remote_value`].
    pub remote: Option<bool>,
}

impl FeatureSources {
    /// The environment tier read from the process; every other tier unset.
    pub fn from_process_env(feature: Feature) -> Self {
        Self {
            env: env_bool(feature.env()),
            ..Self::default()
        }
    }
}

pub const FEATURES: &[FeatureSpec] = &[
    FeatureSpec {
        id: Feature::SessionSearch,
        key: "session_search",
        path: "features.session_search",
        env: "ECHO_AGENT_SESSION_SEARCH",
        default_enabled: true,
        remote: Some(|settings| settings.session_search),
    },
    FeatureSpec {
        id: Feature::LspTools,
        key: "lsp_tools",
        path: "features.lsp_tools",
        env: "ECHO_AGENT_LSP_TOOLS",
        default_enabled: false,
        remote: Some(|settings| settings.lsp_tools_enabled),
    },
    FeatureSpec {
        id: Feature::WebFetch,
        key: "web_fetch",
        path: "features.web_fetch",
        env: "ECHO_AGENT_WEB_FETCH",
        default_enabled: false,
        remote: Some(|settings| settings.web_fetch_enabled),
    },
    FeatureSpec {
        id: Feature::SessionRecap,
        key: "session_recap",
        path: "features.session_recap",
        env: "ECHO_AGENT_SESSION_RECAP",
        default_enabled: true,
        remote: Some(|settings| settings.session_recap),
    },
    FeatureSpec {
        id: Feature::AskUserQuestion,
        key: "ask_user_question",
        path: "features.ask_user_question",
        env: "ECHO_AGENT_ASK_USER_QUESTION",
        default_enabled: true,
        remote: Some(|settings| settings.ask_user_question_enabled),
    },
    FeatureSpec {
        id: Feature::VoiceMode,
        key: "voice_mode",
        path: "features.voice_mode",
        env: "ECHO_AGENT_VOICE_MODE",
        default_enabled: true,
        remote: Some(|settings| settings.voice_mode_enabled),
    },
    FeatureSpec {
        id: Feature::WriteFile,
        key: "write_file",
        path: "features.write_file",
        env: "ECHO_AGENT_WRITE_FILE",
        default_enabled: true,
        remote: Some(|settings| settings.write_file_enabled),
    },
    FeatureSpec {
        id: Feature::Feedback,
        key: "feedback",
        path: "features.feedback",
        env: "ECHO_AGENT_FEEDBACK_ENABLED",
        default_enabled: true,
        remote: Some(|settings| settings.feedback_enabled),
    },
    FeatureSpec {
        id: Feature::FeedbackTraceCard,
        key: "feedback_trace_card",
        path: "features.feedback_trace_card",
        env: "ECHO_AGENT_FEEDBACK_TRACE_CARD",
        default_enabled: false,
        remote: Some(|settings| settings.feedback_trace_card_enabled),
    },
    FeatureSpec {
        id: Feature::TurnSummary,
        key: "turn_summary",
        path: "features.turn_summary",
        env: "ECHO_AGENT_TURN_SUMMARY",
        default_enabled: true,
        remote: Some(|settings| settings.turn_summary),
    },
    FeatureSpec {
        id: Feature::CancelRewind,
        key: "cancel_rewind",
        path: "features.cancel_rewind",
        env: "ECHO_AGENT_CANCEL_REWIND",
        default_enabled: true,
        remote: Some(|settings| settings.cancel_rewind_enabled),
    },
    FeatureSpec {
        id: Feature::CompactionVerbatimInput,
        key: "compaction_verbatim_input",
        path: "features.compaction_verbatim_input",
        env: "ECHO_AGENT_COMPACTION_VERBATIM_INPUT",
        default_enabled: true,
        remote: Some(|settings| settings.compaction_verbatim_input),
    },
    FeatureSpec {
        id: Feature::TwoPassCompaction,
        key: "two_pass_compaction",
        path: "features.two_pass_compaction",
        env: "ECHO_AGENT_TWO_PASS_COMPACTION",
        default_enabled: false,
        remote: Some(|settings| settings.two_pass_compaction_enabled),
    },
    FeatureSpec {
        id: Feature::BackendTools,
        key: "backend_tools",
        path: "features.backend_tools",
        // The variable predates the key and is not spelled after it.
        env: "ECHO_AGENT_BACKEND_SEARCH",
        default_enabled: true,
        remote: None,
    },
    FeatureSpec {
        id: Feature::AutoWake,
        key: "auto_wake",
        path: "features.auto_wake",
        env: "ECHO_AGENT_AUTO_WAKE",
        default_enabled: true,
        remote: Some(|settings| settings.auto_wake_enabled),
    },
    FeatureSpec {
        id: Feature::SubagentWorktreeSnapshot,
        key: "subagent_worktree_snapshot",
        path: "features.subagent_worktree_snapshot",
        env: "ECHO_AGENT_SUBAGENT_WORKTREE_SNAPSHOT",
        default_enabled: false,
        remote: Some(|settings| settings.subagent_worktree_snapshot_enabled),
    },
];

impl Feature {
    fn spec(self) -> &'static FeatureSpec {
        FEATURES
            .iter()
            .find(|spec| spec.id == self)
            .unwrap_or_else(|| unreachable!("missing FeatureSpec for {self:?}"))
    }

    pub fn key(self) -> &'static str {
        self.spec().key
    }

    pub fn env(self) -> &'static str {
        self.spec().env
    }

    pub fn path(self) -> &'static str {
        self.spec().path
    }

    /// The one place a `RemoteSettings` is read for a feature.
    pub fn remote_value(self, settings: Option<&RemoteSettings>) -> Option<bool> {
        let read = self.spec().remote?;
        read(settings?)
    }

    /// Reads like `off (a requirements.toml pin)`. Resolves rather than taking a
    /// resolution, so a reason cannot name another feature's tiers.
    pub fn off_reason(self, sources: FeatureSources) -> Option<String> {
        let resolved = self.resolve(sources);
        if resolved.value {
            return None;
        }
        let spec = self.spec();
        Some(match resolved.source {
            ConfigSource::Requirement => "a requirements.toml pin or an MDM policy".to_owned(),
            ConfigSource::Env => format!("the {} environment variable", spec.env),
            // The tier is the merged document, but only a file can be opened.
            ConfigSource::Config
            // Not reachable from `resolve`, which reports the tier as `Config`.
            // They answer rather than panic for a caller that resolves otherwise.
            | ConfigSource::UserConfig
            | ConfigSource::ManagedConfig
            | ConfigSource::SystemManagedConfig
            | ConfigSource::EnvOverlay => {
                format!("the {} key in config.toml or managed_config.toml", spec.key)
            }
            ConfigSource::Remote => "a remote setting".to_owned(),
            ConfigSource::Default => "the default".to_owned(),
            // Not reachable either: no registered key has a flag to name.
            ConfigSource::Cli => "a command line override".to_owned(),
        })
    }

    /// Pin, then environment, then config, then remote, then the default.
    pub fn resolve(self, sources: FeatureSources) -> Resolved<bool> {
        let spec = self.spec();
        BoolFlag::env_value(sources.env)
            .requirement(sources.pin)
            .config(sources.config)
            .feature_flag(sources.remote)
            .default(spec.default_enabled)
            .resolve()
    }
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
