use super::*;
use crate::flags::ConfigSource;
use std::collections::BTreeMap;
use strum::IntoEnumIterator;

#[test]
fn every_feature_has_one_row_with_its_own_spellings() {
    for feature in Feature::iter() {
        assert_eq!(
            FEATURES.iter().filter(|spec| spec.id == feature).count(),
            1,
            "{feature:?} needs exactly one row in FEATURES",
        );
    }
    assert_eq!(
        Feature::iter().count(),
        FEATURES.len(),
        "the enum and FEATURES disagree: a variant has no row, or a row has no variant",
    );

    for spec in FEATURES {
        assert_eq!(spec.path, format!("features.{}", spec.key));
        assert_eq!(
            FEATURES.iter().filter(|s| s.key == spec.key).count(),
            1,
            "{} is on two rows",
            spec.key,
        );
        assert_eq!(
            FEATURES.iter().filter(|s| s.env == spec.env).count(),
            1,
            "{} is read by two features",
            spec.env,
        );
    }
}

/// Product decisions, stated here rather than read back out of the table.
#[test]
fn registered_settings() {
    let rows: BTreeMap<_, _> = FEATURES
        .iter()
        .map(|spec| (spec.key, (spec.env, spec.default_enabled)))
        .collect();
    assert_eq!(
        rows,
        BTreeMap::from([
            ("session_search", ("ECHO_AGENT_SESSION_SEARCH", true)),
            ("lsp_tools", ("ECHO_AGENT_LSP_TOOLS", false)),
            ("web_fetch", ("ECHO_AGENT_WEB_FETCH", false)),
            ("session_recap", ("ECHO_AGENT_SESSION_RECAP", true)),
            ("ask_user_question", ("ECHO_AGENT_ASK_USER_QUESTION", true)),
            ("voice_mode", ("ECHO_AGENT_VOICE_MODE", true)),
            ("write_file", ("ECHO_AGENT_WRITE_FILE", true)),
            ("feedback", ("ECHO_AGENT_FEEDBACK_ENABLED", true)),
            (
                "feedback_trace_card",
                ("ECHO_AGENT_FEEDBACK_TRACE_CARD", false)
            ),
            ("turn_summary", ("ECHO_AGENT_TURN_SUMMARY", true)),
            ("cancel_rewind", ("ECHO_AGENT_CANCEL_REWIND", true)),
            (
                "compaction_verbatim_input",
                ("ECHO_AGENT_COMPACTION_VERBATIM_INPUT", true),
            ),
            (
                "two_pass_compaction",
                ("ECHO_AGENT_TWO_PASS_COMPACTION", false)
            ),
            ("backend_tools", ("ECHO_AGENT_BACKEND_SEARCH", true)),
            ("auto_wake", ("ECHO_AGENT_AUTO_WAKE", true)),
            (
                "subagent_worktree_snapshot",
                ("ECHO_AGENT_SUBAGENT_WORKTREE_SNAPSHOT", false),
            ),
        ]),
    );
}

/// A row wired to a neighbour's field type-checks, so each case sets one field
/// and a wrong projection reads nothing.
#[test]
fn every_registered_feature_reads_its_own_remote_setting() {
    for spec in FEATURES {
        let value = !spec.default_enabled;
        let mut settings = RemoteSettings::default();
        match spec.id {
            Feature::SessionSearch => settings.session_search = Some(value),
            Feature::LspTools => settings.lsp_tools_enabled = Some(value),
            Feature::WebFetch => settings.web_fetch_enabled = Some(value),
            Feature::SessionRecap => settings.session_recap = Some(value),
            Feature::AskUserQuestion => settings.ask_user_question_enabled = Some(value),
            Feature::VoiceMode => settings.voice_mode_enabled = Some(value),
            Feature::WriteFile => settings.write_file_enabled = Some(value),
            Feature::Feedback => settings.feedback_enabled = Some(value),
            Feature::FeedbackTraceCard => settings.feedback_trace_card_enabled = Some(value),
            Feature::TurnSummary => settings.turn_summary = Some(value),
            Feature::CancelRewind => settings.cancel_rewind_enabled = Some(value),
            Feature::CompactionVerbatimInput => settings.compaction_verbatim_input = Some(value),
            Feature::TwoPassCompaction => settings.two_pass_compaction_enabled = Some(value),
            Feature::AutoWake => settings.auto_wake_enabled = Some(value),
            Feature::SubagentWorktreeSnapshot => {
                settings.subagent_worktree_snapshot_enabled = Some(value)
            }
            // The one row with no remote tier, stated as such rather than as a
            // projection that reads nothing.
            Feature::BackendTools => {
                assert!(spec.remote.is_none(), "{} grew a remote tier", spec.key);
                continue;
            }
        }

        let resolved = spec.id.resolve(FeatureSources {
            remote: spec.id.remote_value(Some(&settings)),
            ..Default::default()
        });
        assert_eq!(
            resolved.source,
            ConfigSource::Remote,
            "{} reads no remote",
            spec.key
        );
        assert_eq!(
            resolved.value, value,
            "{} reads another feature's setting",
            spec.key,
        );
    }
}

#[test]
fn pin_outranks_env_outranks_config_outranks_remote_outranks_default() {
    let pinned = Feature::SessionSearch.resolve(FeatureSources {
        pin: Some(true),
        env: Some(false),
        config: Some(false),
        remote: Some(false),
    });
    assert!(pinned.value);
    assert_eq!(pinned.source, ConfigSource::Requirement);

    let from_env = Feature::SessionSearch.resolve(FeatureSources {
        env: Some(true),
        config: Some(false),
        remote: Some(false),
        ..Default::default()
    });
    assert!(from_env.value);
    assert_eq!(from_env.source, ConfigSource::Env);

    let configured = Feature::SessionSearch.resolve(FeatureSources {
        config: Some(true),
        remote: Some(false),
        ..Default::default()
    });
    assert!(configured.value);
    assert_eq!(configured.source, ConfigSource::Config);

    let remote = Feature::SessionSearch.resolve(FeatureSources {
        remote: Some(false),
        ..Default::default()
    });
    assert!(!remote.value);
    assert_eq!(remote.source, ConfigSource::Remote);

    let fallback = Feature::SessionSearch.resolve(FeatureSources::default());
    assert!(fallback.value);
    assert_eq!(fallback.source, ConfigSource::Default);
}

#[test]
fn off_reason_names_the_setting_that_turned_it_off() {
    let on = Feature::SessionSearch.off_reason(FeatureSources {
        env: Some(true),
        ..Default::default()
    });
    assert_eq!(on, None);

    for (sources, reason) in [
        (
            FeatureSources {
                pin: Some(false),
                ..Default::default()
            },
            "a requirements.toml pin or an MDM policy",
        ),
        (
            FeatureSources {
                env: Some(false),
                ..Default::default()
            },
            "the ECHO_AGENT_SESSION_SEARCH environment variable",
        ),
        (
            FeatureSources {
                config: Some(false),
                ..Default::default()
            },
            "the session_search key in config.toml or managed_config.toml",
        ),
        (
            FeatureSources {
                remote: Some(false),
                ..Default::default()
            },
            "a remote setting",
        ),
    ] {
        assert_eq!(
            Feature::SessionSearch.off_reason(sources).as_deref(),
            Some(reason),
        );
    }

    // `lsp_tools` is the default-off row, so nothing has to set it.
    assert_eq!(
        Feature::LspTools
            .off_reason(FeatureSources::default())
            .as_deref(),
        Some("the default"),
    );
}
