//! Tests for plugin CTA phases, including the end-to-end cta_e2e suite.

use super::*;

#[test]
fn plugin_cta_catalog_loaded_sanitizes_components_at_ingestion() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);

    let mut entry = cta_entry("dirty", "not_installed");
    entry.components = Some(echo_agent_hooks_plugins_types::PluginComponents {
        skills: vec![echo_agent_hooks_plugins_types::ComponentItem {
            name: "evil\u{1b}[31mskill".into(),
            description: Some(format!("\u{7}{}", "d".repeat(300))),
        }],
        ..Default::default()
    });
    let response = echo_agent_hooks_plugins_types::MarketplaceListResponse {
        sources: vec![echo_agent_hooks_plugins_types::MarketplaceScanResult {
            source_name: echo_agent_plugin_marketplace::OFFICIAL_SOURCE_NAME.into(),
            source_kind: "git".into(),
            source_url_or_path: echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL.into(),
            plugins: vec![entry],
            error: None,
        }],
    };

    dispatch(
        Action::TaskComplete(TaskResult::PluginCtaCatalogLoaded {
            agent_id: id,
            result: Ok(response),
        }),
        &mut app,
    );

    let cta = &app.agents[&id].plugin_cta;
    let components = cta.candidates[0].components.as_ref().unwrap();
    assert_eq!(components.skills[0].name, "evil[31mskill");
    let desc = components.skills[0].description.as_deref().unwrap();
    assert_eq!(desc.chars().count(), 120);
    assert!(desc.chars().all(|c| c == 'd'));
}

fn cta_outcome_reload(
    status: echo_agent_hooks_plugins_types::OutcomeStatus,
    message: &str,
) -> echo_agent_hooks_plugins_types::ActionOutcome {
    echo_agent_hooks_plugins_types::ActionOutcome {
        status,
        message: message.into(),
        requires_reload: true,
        requires_restart: false,
    }
}

#[test]
fn plugin_cta_catalog_keeps_official_not_installed_only() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);

    let response = echo_agent_hooks_plugins_types::MarketplaceListResponse {
        sources: vec![
            echo_agent_hooks_plugins_types::MarketplaceScanResult {
                source_name: echo_agent_plugin_marketplace::OFFICIAL_SOURCE_NAME.into(),
                source_kind: "git".into(),
                source_url_or_path: echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL.into(),
                plugins: vec![
                    cta_entry("keep-me", "not_installed"),
                    cta_entry("already-installed", "installed"),
                    cta_entry("has-update", "update_available"),
                ],
                error: None,
            },
            echo_agent_hooks_plugins_types::MarketplaceScanResult {
                source_name: "Third Party".into(),
                source_kind: "git".into(),
                source_url_or_path: "https://github.com/other/repo.git".into(),
                plugins: vec![cta_entry("third-party", "not_installed")],
                error: None,
            },
            echo_agent_hooks_plugins_types::MarketplaceScanResult {
                source_name: "Custom Mirror".into(),
                source_kind: "git".into(),
                source_url_or_path: "git@github.com:fuyuxiang/echo-agent-desktop.git".into(),
                plugins: vec![cta_entry("url-official", "not_installed")],
                error: None,
            },
        ],
    };

    let effects = dispatch(
        Action::TaskComplete(TaskResult::PluginCtaCatalogLoaded {
            agent_id: id,
            result: Ok(response),
        }),
        &mut app,
    );
    assert!(effects.is_empty());

    let cta = &app.agents[&id].plugin_cta;
    let names: Vec<&str> = cta.candidates.iter().map(|p| p.name.as_str()).collect();
    // One source wins: both the first source and "Custom Mirror" are
    // URL-verified official, so the first-registered one supplies the
    // candidates and the install target.
    assert_eq!(names, vec!["keep-me"]);
    assert_eq!(cta.candidates[0].install_status, "not_installed");
    assert_eq!(
        cta.source_url_or_path.as_deref(),
        Some(echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL),
        "without an override the install target stays the official source"
    );
}

#[test]
fn plugin_cta_default_prefers_url_verified_official_over_impostor() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);

    // A first-listed source that merely calls itself "EchoAgent Official" must not
    // become the install root; the URL-verified official source wins even
    // when registered later.
    let response = echo_agent_hooks_plugins_types::MarketplaceListResponse {
        sources: vec![
            echo_agent_hooks_plugins_types::MarketplaceScanResult {
                source_name: echo_agent_plugin_marketplace::OFFICIAL_SOURCE_NAME.into(),
                source_kind: "path".into(),
                source_url_or_path: "/srv/impostor-marketplace".into(),
                plugins: vec![cta_entry("impostor", "not_installed")],
                error: None,
            },
            echo_agent_hooks_plugins_types::MarketplaceScanResult {
                source_name: echo_agent_plugin_marketplace::OFFICIAL_SOURCE_NAME.into(),
                source_kind: "git".into(),
                source_url_or_path: echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL.into(),
                plugins: vec![cta_entry("genuine", "not_installed")],
                error: None,
            },
        ],
    };
    dispatch(
        Action::TaskComplete(TaskResult::PluginCtaCatalogLoaded {
            agent_id: id,
            result: Ok(response),
        }),
        &mut app,
    );

    let cta = &app.agents[&id].plugin_cta;
    let names: Vec<&str> = cta.candidates.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["genuine"]);
    assert_eq!(
        cta.source_url_or_path.as_deref(),
        Some(echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL)
    );
}

#[test]
fn plugin_cta_default_name_only_official_mirror_selected() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);

    // No URL-verified official source in the scan: a mirror registered under
    // the official name (e.g. an on-prem path source) still feeds the CTA.
    let response = echo_agent_hooks_plugins_types::MarketplaceListResponse {
        sources: vec![echo_agent_hooks_plugins_types::MarketplaceScanResult {
            source_name: echo_agent_plugin_marketplace::OFFICIAL_SOURCE_NAME.into(),
            source_kind: "path".into(),
            source_url_or_path: "/srv/onprem-mirror".into(),
            plugins: vec![cta_entry("mirrored", "not_installed")],
            error: None,
        }],
    };
    dispatch(
        Action::TaskComplete(TaskResult::PluginCtaCatalogLoaded {
            agent_id: id,
            result: Ok(response),
        }),
        &mut app,
    );

    let cta = &app.agents[&id].plugin_cta;
    let names: Vec<&str> = cta.candidates.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["mirrored"]);
    assert_eq!(
        cta.source_url_or_path.as_deref(),
        Some("/srv/onprem-mirror")
    );
}

#[test]
fn plugin_cta_marketplace_override_selects_named_source() {
    let mut app = test_app_with_agent();
    app.plugin_cta_marketplace = Some("SpaceX Marketplace".into());
    let id = AgentId(0);

    let response = echo_agent_hooks_plugins_types::MarketplaceListResponse {
        sources: vec![echo_agent_hooks_plugins_types::MarketplaceScanResult {
            source_name: "SpaceX Marketplace".into(),
            source_kind: "path".into(),
            source_url_or_path: "/srv/spacex-marketplace".into(),
            plugins: vec![
                cta_entry("starlink", "not_installed"),
                cta_entry("dragon", "installed"),
            ],
            error: None,
        }],
    };

    dispatch(
        Action::TaskComplete(TaskResult::PluginCtaCatalogLoaded {
            agent_id: id,
            result: Ok(response),
        }),
        &mut app,
    );

    let cta = &app.agents[&id].plugin_cta;
    let names: Vec<&str> = cta.candidates.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["starlink"]);
    assert_eq!(
        cta.source_url_or_path.as_deref(),
        Some("/srv/spacex-marketplace"),
        "named source counts as present"
    );
}

#[test]
fn plugin_cta_marketplace_duplicate_named_sources_first_wins() {
    let mut app = test_app_with_agent();
    app.plugin_cta_marketplace = Some("SpaceX Marketplace".into());
    let id = AgentId(0);

    // Two sources share the override name: candidates and install target must
    // both come from the first, or a later source's candidate would install
    // against the wrong URL.
    let response = echo_agent_hooks_plugins_types::MarketplaceListResponse {
        sources: vec![
            echo_agent_hooks_plugins_types::MarketplaceScanResult {
                source_name: "SpaceX Marketplace".into(),
                source_kind: "path".into(),
                source_url_or_path: "/srv/spacex-marketplace".into(),
                plugins: vec![cta_entry("starlink", "not_installed")],
                error: None,
            },
            echo_agent_hooks_plugins_types::MarketplaceScanResult {
                source_name: "SpaceX Marketplace".into(),
                source_kind: "git".into(),
                source_url_or_path: "https://github.com/impostor/spacex.git".into(),
                plugins: vec![cta_entry("impostor", "not_installed")],
                error: None,
            },
        ],
    };
    dispatch(
        Action::TaskComplete(TaskResult::PluginCtaCatalogLoaded {
            agent_id: id,
            result: Ok(response),
        }),
        &mut app,
    );

    let cta = &app.agents[&id].plugin_cta;
    let names: Vec<&str> = cta.candidates.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["starlink"]);
    assert_eq!(
        cta.source_url_or_path.as_deref(),
        Some("/srv/spacex-marketplace")
    );
}

#[test]
fn plugin_cta_marketplace_override_install_targets_named_source() {
    use crate::app::agent_view::CtaPhase;
    let mut app = test_app_with_agent();
    app.plugin_cta_enabled = true;
    app.plugin_cta_marketplace = Some("SpaceX Marketplace".into());
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.session_id = Some("sess-1".to_string().into());
        // Unique name so a real config's dismissed set can't suppress the match.
        agent.prompt.set_text("try zzspacexcta now");
    }

    let mut entry = cta_entry("zzspacexcta", "not_installed");
    entry.keywords = vec!["zzspacexcta".into()];
    let response = echo_agent_hooks_plugins_types::MarketplaceListResponse {
        sources: vec![echo_agent_hooks_plugins_types::MarketplaceScanResult {
            source_name: "SpaceX Marketplace".into(),
            source_kind: "path".into(),
            source_url_or_path: "/srv/spacex-marketplace".into(),
            plugins: vec![entry],
            error: None,
        }],
    };
    dispatch(
        Action::TaskComplete(TaskResult::PluginCtaCatalogLoaded {
            agent_id: id,
            result: Ok(response),
        }),
        &mut app,
    );

    let agent = app.agents.get_mut(&id).unwrap();
    assert!(matches!(
        &agent.plugin_cta.phase,
        CtaPhase::Matched { name, .. } if name == "zzspacexcta"
    ));
    agent.connect_matched_plugin();
    assert_eq!(agent.pending_effects.len(), 1);
    match &agent.pending_effects[0] {
        Effect::InstallPluginFromCta {
            source_url_or_path,
            plugin_relative_path,
            ..
        } => {
            assert_eq!(source_url_or_path, "/srv/spacex-marketplace");
            assert_eq!(plugin_relative_path, "plugins/zzspacexcta");
        }
        other => panic!("expected InstallPluginFromCta, got {other:?}"),
    }
}

#[test]
fn plugin_cta_marketplace_override_naming_official_selects_it() {
    let mut app = test_app_with_agent();
    app.plugin_cta_marketplace =
        Some(echo_agent_plugin_marketplace::OFFICIAL_SOURCE_NAME.to_string());
    let id = AgentId(0);

    let response = echo_agent_hooks_plugins_types::MarketplaceListResponse {
        sources: vec![echo_agent_hooks_plugins_types::MarketplaceScanResult {
            source_name: echo_agent_plugin_marketplace::OFFICIAL_SOURCE_NAME.into(),
            source_kind: "git".into(),
            source_url_or_path: echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL.into(),
            plugins: vec![cta_entry("official-plugin", "not_installed")],
            error: None,
        }],
    };
    dispatch(
        Action::TaskComplete(TaskResult::PluginCtaCatalogLoaded {
            agent_id: id,
            result: Ok(response),
        }),
        &mut app,
    );

    let cta = &app.agents[&id].plugin_cta;
    assert!(cta.source_url_or_path.is_some());
    let names: Vec<&str> = cta.candidates.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["official-plugin"]);
    assert_eq!(
        cta.source_url_or_path.as_deref(),
        Some(echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL)
    );
}

#[test]
fn plugin_cta_marketplace_override_excludes_official_source() {
    let mut app = test_app_with_agent();
    app.plugin_cta_marketplace = Some("SpaceX Marketplace".into());
    let id = AgentId(0);

    let response = echo_agent_hooks_plugins_types::MarketplaceListResponse {
        sources: vec![
            echo_agent_hooks_plugins_types::MarketplaceScanResult {
                source_name: echo_agent_plugin_marketplace::OFFICIAL_SOURCE_NAME.into(),
                source_kind: "git".into(),
                source_url_or_path: echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL.into(),
                plugins: vec![cta_entry("official-only", "not_installed")],
                error: None,
            },
            echo_agent_hooks_plugins_types::MarketplaceScanResult {
                source_name: "SpaceX Marketplace".into(),
                source_kind: "path".into(),
                source_url_or_path: "/srv/spacex-marketplace".into(),
                plugins: vec![cta_entry("starlink", "not_installed")],
                error: None,
            },
        ],
    };

    dispatch(
        Action::TaskComplete(TaskResult::PluginCtaCatalogLoaded {
            agent_id: id,
            result: Ok(response),
        }),
        &mut app,
    );

    let cta = &app.agents[&id].plugin_cta;
    assert!(cta.source_url_or_path.is_some());
    let names: Vec<&str> = cta.candidates.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["starlink"], "official plugins are excluded");
}

#[test]
fn plugin_cta_marketplace_override_absent_source_hides_cta() {
    use crate::app::agent_view::CtaPhase;
    let mut app = test_app_with_agent();
    app.plugin_cta_enabled = true;
    app.plugin_cta_marketplace = Some("SpaceX Marketplace".into());
    let id = AgentId(0);
    app.agents
        .get_mut(&id)
        .unwrap()
        .prompt
        .set_text("open figma now");

    let mut entry = cta_entry("figma", "not_installed");
    entry.keywords = vec!["figma".into()];
    let response = echo_agent_hooks_plugins_types::MarketplaceListResponse {
        sources: vec![echo_agent_hooks_plugins_types::MarketplaceScanResult {
            source_name: echo_agent_plugin_marketplace::OFFICIAL_SOURCE_NAME.into(),
            source_kind: "git".into(),
            source_url_or_path: echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL.into(),
            plugins: vec![entry],
            error: None,
        }],
    };

    dispatch(
        Action::TaskComplete(TaskResult::PluginCtaCatalogLoaded {
            agent_id: id,
            result: Ok(response),
        }),
        &mut app,
    );

    let cta = &app.agents[&id].plugin_cta;
    assert!(cta.source_url_or_path.is_none());
    assert!(cta.candidates.is_empty());
    assert_eq!(cta.phase, CtaPhase::Hidden);
}

#[test]
fn plugin_cta_catalog_err_preserves_cache() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let cta = &mut app.agents.get_mut(&id).unwrap().plugin_cta;
        cta.source_url_or_path =
            Some(echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL.into());
        cta.candidates = vec![cta_entry("cached", "not_installed")];
    }

    let effects = dispatch(
        Action::TaskComplete(TaskResult::PluginCtaCatalogLoaded {
            agent_id: id,
            result: Err("boom".into()),
        }),
        &mut app,
    );
    assert!(effects.is_empty());

    let cta = &app.agents[&id].plugin_cta;
    assert!(cta.source_url_or_path.is_some());
    assert_eq!(cta.candidates.len(), 1);
}

#[test]
fn plugin_cta_catalog_reload_empty_candidates_preserves_installed_checkmark() {
    use crate::app::agent_view::CtaPhase;
    // The just-installed plugin was the only not-installed official candidate,
    // so the post-settle catalog refresh returns an empty candidate set. The
    // "✓ installed" confirmation must survive (its 4s timer owns the dismiss),
    // not get clobbered to Hidden.
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let cta = &mut app.agents.get_mut(&id).unwrap().plugin_cta;
        cta.source_url_or_path =
            Some(echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL.into());
        cta.candidates = vec![cta_entry("figma", "not_installed")];
        cta.phase = CtaPhase::Installed {
            name: "figma".into(),
        };
    }
    let response = echo_agent_hooks_plugins_types::MarketplaceListResponse {
        sources: vec![echo_agent_hooks_plugins_types::MarketplaceScanResult {
            source_name: echo_agent_plugin_marketplace::OFFICIAL_SOURCE_NAME.into(),
            source_kind: "git".into(),
            source_url_or_path: echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL.into(),
            plugins: vec![cta_entry("figma", "installed")],
            error: None,
        }],
    };
    dispatch(
        Action::TaskComplete(TaskResult::PluginCtaCatalogLoaded {
            agent_id: id,
            result: Ok(response),
        }),
        &mut app,
    );
    let cta = &app.agents[&id].plugin_cta;
    assert!(cta.candidates.is_empty());
    assert_eq!(
        cta.phase,
        CtaPhase::Installed {
            name: "figma".into()
        }
    );
}

#[test]
fn plugin_cta_catalog_load_recomputes_match_for_typed_draft() {
    use crate::app::agent_view::CtaPhase;
    // Redirect config reads to an empty temp home so the catalog-load
    // dismissed-set read is hermetic, not just deterministic.
    {
        use std::sync::OnceLock;
        static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
        HOME.get_or_init(|| {
            let tmp = tempfile::tempdir().expect("tempdir creation");
            unsafe {
                std::env::set_var("ECHO_AGENT_HOME", tmp.path());
            }
            tmp
        });
    }
    // User typed a matching word and the debounce already fired against the
    // (still-empty) catalog -> Hidden. When the async catalog lands, the CTA
    // must surface without waiting for another keystroke. Uses a unique name
    // so the cached dismissed-set read can't suppress it.
    let mut app = test_app_with_agent();
    app.plugin_cta_enabled = true;
    let id = AgentId(0);
    app.agents
        .get_mut(&id)
        .unwrap()
        .prompt
        .set_text("let's try zzctaplugin today");
    let mut entry = cta_entry("zzctaplugin", "not_installed");
    entry.keywords = vec!["zzctaplugin".into()];
    let response = echo_agent_hooks_plugins_types::MarketplaceListResponse {
        sources: vec![echo_agent_hooks_plugins_types::MarketplaceScanResult {
            source_name: echo_agent_plugin_marketplace::OFFICIAL_SOURCE_NAME.into(),
            source_kind: "git".into(),
            source_url_or_path: echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL.into(),
            plugins: vec![entry],
            error: None,
        }],
    };
    dispatch(
        Action::TaskComplete(TaskResult::PluginCtaCatalogLoaded {
            agent_id: id,
            result: Ok(response),
        }),
        &mut app,
    );
    assert!(matches!(
        &app.agents[&id].plugin_cta.phase,
        CtaPhase::Matched { name, .. } if name == "zzctaplugin"
    ));
}

#[test]
fn plugin_cta_phase_hidden_when_feature_disabled() {
    let cands = vec![cta_entry("figma", "not_installed")];
    let phase = plugin_cta_phase_for(false, true, &cands, "open figma now", |_| false);
    assert_eq!(phase, crate::app::agent_view::CtaPhase::Hidden);
}

#[test]
fn plugin_cta_phase_hidden_when_official_source_absent() {
    let cands = vec![cta_entry("figma", "not_installed")];
    let phase = plugin_cta_phase_for(true, false, &cands, "open figma now", |_| false);
    assert_eq!(phase, crate::app::agent_view::CtaPhase::Hidden);
}

#[test]
fn plugin_cta_phase_hidden_when_dismissed() {
    let cands = vec![cta_entry("figma", "not_installed")];
    let phase = plugin_cta_phase_for(true, true, &cands, "open figma now", |id| id == "figma");
    assert_eq!(phase, crate::app::agent_view::CtaPhase::Hidden);
}

#[test]
fn plugin_cta_phase_hidden_when_no_keyword_match() {
    let cands = vec![cta_entry("figma", "not_installed")];
    let phase = plugin_cta_phase_for(true, true, &cands, "hello world", |_| false);
    assert_eq!(phase, crate::app::agent_view::CtaPhase::Hidden);
}

#[test]
fn plugin_cta_phase_matched_when_keyword_hits() {
    let cands = vec![cta_entry("figma", "not_installed")];
    let phase = plugin_cta_phase_for(true, true, &cands, "open figma now", |_| false);
    assert_eq!(
        phase,
        crate::app::agent_view::CtaPhase::Matched {
            plugin_relative_path: "plugins/figma".into(),
            name: "figma".into(),
        }
    );
}

#[test]
fn plugin_cta_phase_matched_on_pasted_domain_url() {
    let mut entry = cta_entry("vc-deploy", "not_installed");
    entry.domains = vec!["vercel.com".into()];
    let cands = vec![entry];
    let phase = plugin_cta_phase_for(true, true, &cands, "see https://vercel.com/dash", |_| false);
    assert_eq!(
        phase,
        crate::app::agent_view::CtaPhase::Matched {
            plugin_relative_path: "plugins/vc-deploy".into(),
            name: "vc-deploy".into(),
        }
    );
}

#[test]
fn plugin_cta_phase_hidden_for_github_homepage_url() {
    let mut entry = cta_entry("vercel", "not_installed");
    entry.homepage = Some("https://github.com/vercel/vercel-plugin".into());
    let cands = vec![entry];
    let phase = plugin_cta_phase_for(
        true,
        true,
        &cands,
        "clone https://github.com/rust-lang/rust",
        |_| false,
    );
    assert_eq!(phase, crate::app::agent_view::CtaPhase::Hidden);
}

#[test]
fn cta_impression_edge_only_on_new_appearance() {
    use crate::app::agent_view::CtaPhase;
    let matched_figma = CtaPhase::Matched {
        plugin_relative_path: "plugins/figma".into(),
        name: "figma".into(),
    };
    let matched_slack = CtaPhase::Matched {
        plugin_relative_path: "plugins/slack".into(),
        name: "slack".into(),
    };
    let installing_figma = CtaPhase::Installing {
        plugin_relative_path: "plugins/figma".into(),
        name: "figma".into(),
    };

    assert_eq!(
        cta_impression_plugin_name(&CtaPhase::Hidden, &matched_figma),
        Some("figma")
    );
    assert_eq!(
        cta_impression_plugin_name(&matched_figma, &matched_figma),
        None
    );
    assert_eq!(
        cta_impression_plugin_name(&matched_figma, &matched_slack),
        Some("slack")
    );
    assert_eq!(
        cta_impression_plugin_name(&matched_figma, &CtaPhase::Hidden),
        None
    );
    assert_eq!(
        cta_impression_plugin_name(&installing_figma, &matched_figma),
        Some("figma")
    );
}

#[test]
fn cta_install_error_category_maps_outcome() {
    use echo_agent_hooks_plugins_types::OutcomeStatus;
    assert_eq!(
        cta_install_error_category(&Ok(cta_outcome(OutcomeStatus::Success, "ok"))),
        None
    );
    assert_eq!(
        cta_install_error_category(&Ok(cta_outcome(OutcomeStatus::ValidationError, "bad"))),
        Some("validation_error".to_string())
    );
    assert_eq!(
        cta_install_error_category(&Ok(cta_outcome(
            OutcomeStatus::ConfirmationRequired,
            "confirm"
        ))),
        Some("confirmation_required".to_string())
    );
    assert_eq!(
        cta_install_error_category(&Ok(cta_outcome(OutcomeStatus::NotFound, "missing"))),
        Some("not_found".to_string())
    );
    assert_eq!(
        cta_install_error_category(&Ok(cta_outcome(OutcomeStatus::InternalError, "boom"))),
        Some("internal_error".to_string())
    );
    assert_eq!(
        cta_install_error_category(&Ok(cta_outcome(OutcomeStatus::Unsupported, "nope"))),
        Some("unsupported".to_string())
    );
    assert_eq!(
        cta_install_error_category(&Err("transport died".to_string())),
        Some("action_failed".to_string())
    );
}

#[test]
fn plugin_cta_debounce_ignores_stale_generation() {
    use crate::app::agent_view::CtaPhase;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let cta = &mut app.agents.get_mut(&id).unwrap().plugin_cta;
        cta.debounce_generation = 5;
        cta.phase = CtaPhase::Hidden;
    }
    let effects = dispatch(
        Action::TaskComplete(TaskResult::PluginCtaDebounceExpired {
            agent_id: id,
            generation: 3,
        }),
        &mut app,
    );
    assert!(effects.is_empty());
    assert_eq!(app.agents[&id].plugin_cta.phase, CtaPhase::Hidden);
}

#[test]
fn plugin_cta_debounce_sets_hidden_when_feature_disabled() {
    use crate::app::agent_view::CtaPhase;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.plugin_cta_enabled = false;
    {
        let cta = &mut app.agents.get_mut(&id).unwrap().plugin_cta;
        cta.source_url_or_path =
            Some(echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL.into());
        cta.candidates = vec![cta_entry("figma", "not_installed")];
        cta.debounce_generation = 1;
        cta.phase = CtaPhase::Matched {
            plugin_relative_path: "plugins/figma".into(),
            name: "figma".into(),
        };
    }
    let effects = dispatch(
        Action::TaskComplete(TaskResult::PluginCtaDebounceExpired {
            agent_id: id,
            generation: 1,
        }),
        &mut app,
    );
    assert!(effects.is_empty());
    assert_eq!(app.agents[&id].plugin_cta.phase, CtaPhase::Hidden);
}

#[test]
fn plugin_cta_debounce_preserves_in_flight_states() {
    use crate::app::agent_view::CtaPhase;
    for phase in [
        CtaPhase::Installing {
            plugin_relative_path: "plugins/figma".into(),
            name: "figma".into(),
        },
        CtaPhase::AwaitingReload {
            name: "figma".into(),
        },
        CtaPhase::AwaitingMcps {
            name: "figma".into(),
        },
        CtaPhase::Error {
            plugin_relative_path: "plugins/figma".into(),
            name: "figma".into(),
            message: "boom".into(),
        },
        // The brief "installed ✓" is owned by its auto-dismiss timer; a
        // keystroke must not recompute it away (or re-offer the plugin).
        CtaPhase::Installed {
            name: "figma".into(),
        },
    ] {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        {
            let cta = &mut app.agents.get_mut(&id).unwrap().plugin_cta;
            cta.source_url_or_path =
                Some(echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL.into());
            cta.candidates = vec![cta_entry("figma", "not_installed")];
            cta.debounce_generation = 1;
            cta.phase = phase.clone();
        }
        let effects = dispatch(
            Action::TaskComplete(TaskResult::PluginCtaDebounceExpired {
                agent_id: id,
                generation: 1,
            }),
            &mut app,
        );
        assert!(effects.is_empty());
        assert_eq!(app.agents[&id].plugin_cta.phase, phase);
    }
}

#[test]
fn cta_install_done_ok_no_reload_enters_awaiting_mcps() {
    use crate::app::agent_view::CtaPhase;
    use echo_agent_hooks_plugins_types::OutcomeStatus;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let cta = &mut app.agents.get_mut(&id).unwrap().plugin_cta;
        cta.phase = CtaPhase::Installing {
            plugin_relative_path: "plugins/figma".into(),
            name: "figma".into(),
        };
        cta.expects_mcp = true;
    }
    let effects = dispatch(
        Action::TaskComplete(TaskResult::CtaPluginInstallDone {
            agent_id: id,
            plugin_name: "figma".into(),
            result: Ok(cta_outcome(OutcomeStatus::Success, "installed")),
        }),
        &mut app,
    );
    assert_eq!(
        app.agents[&id].plugin_cta.phase,
        CtaPhase::AwaitingMcps {
            name: "figma".into()
        }
    );
    assert!(matches!(
        effects.as_slice(),
        [Effect::FetchPluginCtaMcps { plugin_name, .. }] if plugin_name == "figma"
    ));
}

#[test]
fn cta_install_done_ok_requires_reload_enters_awaiting_reload() {
    use crate::app::agent_view::CtaPhase;
    use echo_agent_hooks_plugins_types::OutcomeStatus;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().plugin_cta.phase = CtaPhase::Installing {
        plugin_relative_path: "plugins/figma".into(),
        name: "figma".into(),
    };
    let effects = dispatch(
        Action::TaskComplete(TaskResult::CtaPluginInstallDone {
            agent_id: id,
            plugin_name: "figma".into(),
            result: Ok(cta_outcome_reload(OutcomeStatus::Success, "installed")),
        }),
        &mut app,
    );
    assert_eq!(
        app.agents[&id].plugin_cta.phase,
        CtaPhase::AwaitingReload {
            name: "figma".into()
        }
    );
    assert!(matches!(
        effects.as_slice(),
        [Effect::ReloadPluginsForCta { plugin_name, .. }] if plugin_name == "figma"
    ));
}

#[test]
fn cta_install_done_err_sets_error() {
    use crate::app::agent_view::CtaPhase;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().plugin_cta.phase = CtaPhase::Installing {
        plugin_relative_path: "plugins/figma".into(),
        name: "figma".into(),
    };
    let effects = dispatch(
        Action::TaskComplete(TaskResult::CtaPluginInstallDone {
            agent_id: id,
            plugin_name: "figma".into(),
            result: Err("network down".into()),
        }),
        &mut app,
    );
    assert!(effects.is_empty());
    match &app.agents[&id].plugin_cta.phase {
        CtaPhase::Error {
            plugin_relative_path,
            name,
            message,
        } => {
            assert_eq!(plugin_relative_path.as_str(), "plugins/figma");
            assert_eq!(name.as_str(), "figma");
            assert_eq!(message.as_str(), "network down");
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

#[test]
fn cta_install_done_non_success_sets_error_with_sanitized_message() {
    use crate::app::agent_view::CtaPhase;
    use echo_agent_hooks_plugins_types::OutcomeStatus;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().plugin_cta.phase = CtaPhase::Installing {
        plugin_relative_path: "plugins/figma".into(),
        name: "figma".into(),
    };
    let effects = dispatch(
        Action::TaskComplete(TaskResult::CtaPluginInstallDone {
            agent_id: id,
            plugin_name: "figma".into(),
            result: Ok(cta_outcome(
                OutcomeStatus::InternalError,
                "cli-chat-proxy exploded",
            )),
        }),
        &mut app,
    );
    assert!(effects.is_empty());
    match &app.agents[&id].plugin_cta.phase {
        CtaPhase::Error { message, .. } => {
            assert!(message.contains("server"));
            assert!(!message.contains("cli-chat-proxy"));
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

#[test]
fn cta_install_done_ignored_when_not_installing() {
    use crate::app::agent_view::CtaPhase;
    use echo_agent_hooks_plugins_types::OutcomeStatus;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().plugin_cta.phase = CtaPhase::Matched {
        plugin_relative_path: "plugins/figma".into(),
        name: "figma".into(),
    };
    let effects = dispatch(
        Action::TaskComplete(TaskResult::CtaPluginInstallDone {
            agent_id: id,
            plugin_name: "figma".into(),
            result: Ok(cta_outcome(OutcomeStatus::Success, "installed")),
        }),
        &mut app,
    );
    assert!(effects.is_empty());
    assert_eq!(
        app.agents[&id].plugin_cta.phase,
        CtaPhase::Matched {
            plugin_relative_path: "plugins/figma".into(),
            name: "figma".into(),
        }
    );
}

#[test]
fn cta_install_done_ignored_for_different_plugin() {
    use crate::app::agent_view::CtaPhase;
    use echo_agent_hooks_plugins_types::OutcomeStatus;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().plugin_cta.phase = CtaPhase::Installing {
        plugin_relative_path: "plugins/figma".into(),
        name: "figma".into(),
    };
    let effects = dispatch(
        Action::TaskComplete(TaskResult::CtaPluginInstallDone {
            agent_id: id,
            plugin_name: "slack".into(),
            result: Ok(cta_outcome(OutcomeStatus::Success, "installed")),
        }),
        &mut app,
    );
    assert!(effects.is_empty());
    match &app.agents[&id].plugin_cta.phase {
        CtaPhase::Installing { name, .. } => assert_eq!(name.as_str(), "figma"),
        other => panic!("expected Installing, got {other:?}"),
    }
}

#[test]
fn cta_install_relative_path_prefers_candidate_then_falls_back() {
    // Cached candidate wins, even with a non-standard relative path.
    let mut entry = cta_entry("figma", "not_installed");
    entry.relative_path = "vendor/figma-plugin".into();
    let candidates = vec![entry];
    assert_eq!(
        cta_install_relative_path(&candidates, "figma"),
        "vendor/figma-plugin"
    );
    // Unknown name falls back to the official-source layout.
    assert_eq!(
        cta_install_relative_path(&candidates, "slack"),
        "plugins/slack"
    );
}

#[test]
fn cta_reload_done_ok_enters_awaiting_mcps() {
    use crate::app::agent_view::CtaPhase;
    use echo_agent_hooks_plugins_types::OutcomeStatus;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let cta = &mut app.agents.get_mut(&id).unwrap().plugin_cta;
        cta.phase = CtaPhase::AwaitingReload {
            name: "figma".into(),
        };
        cta.expects_mcp = true;
    }
    let effects = dispatch(
        Action::TaskComplete(TaskResult::CtaPluginReloadDone {
            agent_id: id,
            plugin_name: "figma".into(),
            result: Ok(cta_outcome(OutcomeStatus::Success, "reloaded")),
        }),
        &mut app,
    );
    assert_eq!(
        app.agents[&id].plugin_cta.phase,
        CtaPhase::AwaitingMcps {
            name: "figma".into()
        }
    );
    assert!(matches!(
        effects.as_slice(),
        [Effect::FetchPluginCtaMcps { plugin_name, .. }] if plugin_name == "figma"
    ));
}

#[test]
fn cta_reload_done_non_success_sets_error() {
    use crate::app::agent_view::CtaPhase;
    use echo_agent_hooks_plugins_types::OutcomeStatus;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let cta = &mut app.agents.get_mut(&id).unwrap().plugin_cta;
        cta.phase = CtaPhase::AwaitingReload {
            name: "figma".into(),
        };
        cta.expects_mcp = true;
    }
    // A parsed-but-failed reload outcome must surface the error UI, not
    // advance into MCP polling.
    let effects = dispatch(
        Action::TaskComplete(TaskResult::CtaPluginReloadDone {
            agent_id: id,
            plugin_name: "figma".into(),
            result: Ok(cta_outcome(OutcomeStatus::InternalError, "reload exploded")),
        }),
        &mut app,
    );
    assert!(effects.is_empty());
    match &app.agents[&id].plugin_cta.phase {
        CtaPhase::Error { name, .. } => assert_eq!(name, "figma"),
        other => panic!("expected Error, got {other:?}"),
    }
}

#[test]
fn cta_reload_done_err_sets_error() {
    use crate::app::agent_view::CtaPhase;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().plugin_cta.phase = CtaPhase::AwaitingReload {
        name: "figma".into(),
    };
    let effects = dispatch(
        Action::TaskComplete(TaskResult::CtaPluginReloadDone {
            agent_id: id,
            plugin_name: "figma".into(),
            result: Err("reload boom".into()),
        }),
        &mut app,
    );
    assert!(effects.is_empty());
    match &app.agents[&id].plugin_cta.phase {
        CtaPhase::Error {
            plugin_relative_path,
            name,
            message,
        } => {
            assert_eq!(plugin_relative_path.as_str(), "plugins/figma");
            assert_eq!(name.as_str(), "figma");
            assert_eq!(message.as_str(), "reload boom");
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

#[test]
fn cta_reload_done_ignored_for_stale_phase_or_plugin() {
    use crate::app::agent_view::CtaPhase;
    use echo_agent_hooks_plugins_types::OutcomeStatus;
    // Wrong plugin name.
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().plugin_cta.phase = CtaPhase::AwaitingReload {
        name: "figma".into(),
    };
    let effects = dispatch(
        Action::TaskComplete(TaskResult::CtaPluginReloadDone {
            agent_id: id,
            plugin_name: "slack".into(),
            result: Ok(cta_outcome(OutcomeStatus::Success, "reloaded")),
        }),
        &mut app,
    );
    assert!(effects.is_empty());
    assert_eq!(
        app.agents[&id].plugin_cta.phase,
        CtaPhase::AwaitingReload {
            name: "figma".into()
        }
    );
    // Non-matching phase (AwaitingMcps, not AwaitingReload).
    app.agents.get_mut(&id).unwrap().plugin_cta.phase = CtaPhase::AwaitingMcps {
        name: "figma".into(),
    };
    let effects = dispatch(
        Action::TaskComplete(TaskResult::CtaPluginReloadDone {
            agent_id: id,
            plugin_name: "figma".into(),
            result: Ok(cta_outcome(OutcomeStatus::Success, "reloaded")),
        }),
        &mut app,
    );
    assert!(effects.is_empty());
    assert_eq!(
        app.agents[&id].plugin_cta.phase,
        CtaPhase::AwaitingMcps {
            name: "figma".into()
        }
    );
}

#[test]
fn cta_mcps_loaded_handoff_requires_section_name_parity() {
    use crate::app::agent_view::CtaPhase;
    use crate::views::mcps_modal::McpServerDisplayStatus;
    // Happy path: server "plugin: figma" matches the catalog name "figma" ->
    // handoff fires (modal opens).
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().plugin_cta.phase = CtaPhase::AwaitingMcps {
        name: "figma".into(),
    };
    dispatch(
        Action::TaskComplete(TaskResult::PluginCtaMcpsLoaded {
            agent_id: id,
            plugin_name: "figma".into(),
            result: Ok(vec![cta_mcp_server(
                "figma-srv",
                Some("figma"),
                McpServerDisplayStatus::NeedsAuth,
            )]),
        }),
        &mut app,
    );
    assert_eq!(app.agents[&id].plugin_cta.phase, CtaPhase::Hidden);
    assert!(app.agents[&id].extensions_modal.is_some());

    // Mismatch: needs-auth server is labelled "plugin: figma-connector" while
    // the catalog name is "figma" -> graceful degrade to Installed, no modal.
    let mut app = test_app_with_agent();
    app.agents.get_mut(&id).unwrap().plugin_cta.phase = CtaPhase::AwaitingMcps {
        name: "figma".into(),
    };
    dispatch(
        Action::TaskComplete(TaskResult::PluginCtaMcpsLoaded {
            agent_id: id,
            plugin_name: "figma".into(),
            result: Ok(vec![cta_mcp_server(
                "figma-srv",
                Some("figma-connector"),
                McpServerDisplayStatus::NeedsAuth,
            )]),
        }),
        &mut app,
    );
    assert_eq!(
        app.agents[&id].plugin_cta.phase,
        CtaPhase::Installed {
            name: "figma".into()
        }
    );
    assert!(app.agents[&id].extensions_modal.is_none());
}

#[test]
fn cta_mcps_loaded_initializing_keeps_waiting_and_retries() {
    use crate::app::agent_view::CtaPhase;
    use crate::views::mcps_modal::McpServerDisplayStatus;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let cta = &mut app.agents.get_mut(&id).unwrap().plugin_cta;
        cta.phase = CtaPhase::AwaitingMcps {
            name: "figma".into(),
        };
        cta.expects_mcp = true;
    }
    // Plugin server present but still initializing -> not yet terminal.
    let servers = vec![cta_mcp_server(
        "figma-srv",
        Some("figma"),
        McpServerDisplayStatus::Initializing,
    )];
    let effects = dispatch(
        Action::TaskComplete(TaskResult::PluginCtaMcpsLoaded {
            agent_id: id,
            plugin_name: "figma".into(),
            result: Ok(servers),
        }),
        &mut app,
    );
    // Phase stays AwaitingMcps; a delayed re-probe is queued and the attempt
    // counter advances.
    assert_eq!(
        app.agents[&id].plugin_cta.phase,
        CtaPhase::AwaitingMcps {
            name: "figma".into()
        }
    );
    assert_eq!(app.agents[&id].plugin_cta.mcp_attempt, 1);
    assert!(matches!(
        effects.as_slice(),
        [Effect::RetryPluginCtaMcps { plugin_name, .. }] if plugin_name == "figma"
    ));
    assert!(app.agents[&id].extensions_modal.is_none());
}

#[test]
fn cta_mcps_loaded_unavailable_keeps_waiting() {
    use crate::app::agent_view::CtaPhase;
    use crate::views::mcps_modal::McpServerDisplayStatus;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let cta = &mut app.agents.get_mut(&id).unwrap().plugin_cta;
        cta.phase = CtaPhase::AwaitingMcps {
            name: "figma".into(),
        };
        cta.expects_mcp = true;
    }
    // An OAuth server can briefly surface as Unavailable before flipping to
    // NeedsAuth -- Unavailable is not a final verdict, so keep polling
    // instead of settling early (which would miss the handoff).
    let servers = vec![cta_mcp_server(
        "figma-srv",
        Some("figma"),
        McpServerDisplayStatus::Unavailable,
    )];
    let effects = dispatch(
        Action::TaskComplete(TaskResult::PluginCtaMcpsLoaded {
            agent_id: id,
            plugin_name: "figma".into(),
            result: Ok(servers),
        }),
        &mut app,
    );
    assert_eq!(
        app.agents[&id].plugin_cta.phase,
        CtaPhase::AwaitingMcps {
            name: "figma".into()
        }
    );
    assert_eq!(app.agents[&id].plugin_cta.mcp_attempt, 1);
    assert!(matches!(
        effects.as_slice(),
        [Effect::RetryPluginCtaMcps { .. }]
    ));
}

#[test]
fn cta_mcps_loaded_no_plugin_servers_yet_keeps_waiting() {
    use crate::app::agent_view::CtaPhase;
    use crate::views::mcps_modal::McpServerDisplayStatus;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let cta = &mut app.agents.get_mut(&id).unwrap().plugin_cta;
        cta.phase = CtaPhase::AwaitingMcps {
            name: "figma".into(),
        };
        cta.expects_mcp = true;
    }
    // The plugin's servers haven't appeared yet (only an unrelated server).
    let servers = vec![cta_mcp_server(
        "other-srv",
        Some("slack"),
        McpServerDisplayStatus::Ready,
    )];
    let effects = dispatch(
        Action::TaskComplete(TaskResult::PluginCtaMcpsLoaded {
            agent_id: id,
            plugin_name: "figma".into(),
            result: Ok(servers),
        }),
        &mut app,
    );
    assert_eq!(
        app.agents[&id].plugin_cta.phase,
        CtaPhase::AwaitingMcps {
            name: "figma".into()
        }
    );
    assert!(matches!(
        effects.as_slice(),
        [Effect::RetryPluginCtaMcps { .. }]
    ));
}

#[test]
fn cta_mcps_loaded_absent_servers_settles_without_full_poll() {
    use crate::app::agent_view::CtaPhase;
    use crate::views::mcps_modal::McpServerDisplayStatus;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let cta = &mut app.agents.get_mut(&id).unwrap().plugin_cta;
        cta.phase = CtaPhase::AwaitingMcps {
            name: "superpowers".into(),
        };
        cta.expects_mcp = true;
        // One confirm read already elapsed with no plugin servers.
        cta.mcp_attempt = CTA_MCP_ABSENT_MAX_ATTEMPTS;
    }
    // A skills-only plugin's section stays empty (only unrelated servers).
    let servers = vec![cta_mcp_server(
        "other-srv",
        Some("slack"),
        McpServerDisplayStatus::Ready,
    )];
    let effects = dispatch(
        Action::TaskComplete(TaskResult::PluginCtaMcpsLoaded {
            agent_id: id,
            plugin_name: "superpowers".into(),
            result: Ok(servers),
        }),
        &mut app,
    );
    // Settles well before the full 15-probe budget; no further re-probe.
    assert_eq!(
        app.agents[&id].plugin_cta.phase,
        CtaPhase::Installed {
            name: "superpowers".into()
        }
    );
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::RetryPluginCtaMcps { .. }))
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::DismissCtaInstalled { .. }))
    );
}

#[test]
fn cta_mcps_loaded_empty_list_keeps_waiting_not_absent_settle() {
    use crate::app::agent_view::CtaPhase;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let cta = &mut app.agents.get_mut(&id).unwrap().plugin_cta;
        cta.phase = CtaPhase::AwaitingMcps {
            name: "figma".into(),
        };
        cta.expects_mcp = true;
        cta.mcp_attempt = CTA_MCP_ABSENT_MAX_ATTEMPTS;
    }
    // An entirely empty list means the post-reload config isn't reflected yet
    // (read too early), not that the plugin ships no MCP servers. Keep polling
    // so a slow MCP-bearing plugin's auth handoff isn't skipped.
    let effects = dispatch(
        Action::TaskComplete(TaskResult::PluginCtaMcpsLoaded {
            agent_id: id,
            plugin_name: "figma".into(),
            result: Ok(vec![]),
        }),
        &mut app,
    );
    assert_eq!(
        app.agents[&id].plugin_cta.phase,
        CtaPhase::AwaitingMcps {
            name: "figma".into()
        }
    );
    assert!(matches!(
        effects.as_slice(),
        [Effect::RetryPluginCtaMcps { .. }]
    ));
}

#[test]
fn cta_mcps_loaded_skips_wait_when_not_expecting_mcp() {
    use crate::app::agent_view::CtaPhase;
    use crate::views::mcps_modal::McpServerDisplayStatus;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let cta = &mut app.agents.get_mut(&id).unwrap().plugin_cta;
        cta.phase = CtaPhase::AwaitingMcps {
            name: "figma".into(),
        };
        cta.expects_mcp = false;
    }
    // Even with an initializing server, a skills-only plugin settles at once.
    let effects = dispatch(
        Action::TaskComplete(TaskResult::PluginCtaMcpsLoaded {
            agent_id: id,
            plugin_name: "figma".into(),
            result: Ok(vec![cta_mcp_server(
                "figma-srv",
                Some("figma"),
                McpServerDisplayStatus::Initializing,
            )]),
        }),
        &mut app,
    );
    assert_eq!(
        app.agents[&id].plugin_cta.phase,
        CtaPhase::Installed {
            name: "figma".into()
        }
    );
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::RetryPluginCtaMcps { .. }))
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::DismissCtaInstalled { .. }))
    );
}

#[test]
fn cta_mcps_loaded_times_out_to_installed_after_budget() {
    use crate::app::agent_view::CtaPhase;
    use crate::views::mcps_modal::McpServerDisplayStatus;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let cta = &mut app.agents.get_mut(&id).unwrap().plugin_cta;
        cta.phase = CtaPhase::AwaitingMcps {
            name: "figma".into(),
        };
        cta.expects_mcp = true;
        cta.mcp_attempt = CTA_MCP_POLL_MAX_ATTEMPTS;
    }
    // Still initializing, but the attempt budget is exhausted -> final
    // no-auth verdict: Installed (never loops forever).
    let effects = dispatch(
        Action::TaskComplete(TaskResult::PluginCtaMcpsLoaded {
            agent_id: id,
            plugin_name: "figma".into(),
            result: Ok(vec![cta_mcp_server(
                "figma-srv",
                Some("figma"),
                McpServerDisplayStatus::Initializing,
            )]),
        }),
        &mut app,
    );
    assert_eq!(
        app.agents[&id].plugin_cta.phase,
        CtaPhase::Installed {
            name: "figma".into()
        }
    );
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::RetryPluginCtaMcps { .. }))
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::DismissCtaInstalled { .. }))
    );
}

#[test]
fn cta_installed_dismiss_timeout_hides_when_unchanged() {
    use crate::app::agent_view::CtaPhase;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().plugin_cta.phase = CtaPhase::Installed {
        name: "figma".into(),
    };
    let effects = dispatch(
        Action::TaskComplete(TaskResult::CtaInstalledDismissTimeout {
            agent_id: id,
            plugin_name: "figma".into(),
        }),
        &mut app,
    );
    assert!(effects.is_empty());
    assert_eq!(app.agents[&id].plugin_cta.phase, CtaPhase::Hidden);
}

#[test]
fn cta_installed_dismiss_timeout_ignored_when_phase_moved_on() {
    use crate::app::agent_view::CtaPhase;
    // A keystroke already moved the CTA past the checkmark to a fresh match.
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().plugin_cta.phase = CtaPhase::Matched {
        plugin_relative_path: "plugins/slack".into(),
        name: "slack".into(),
    };
    let effects = dispatch(
        Action::TaskComplete(TaskResult::CtaInstalledDismissTimeout {
            agent_id: id,
            plugin_name: "figma".into(),
        }),
        &mut app,
    );
    assert!(effects.is_empty());
    assert_eq!(
        app.agents[&id].plugin_cta.phase,
        CtaPhase::Matched {
            plugin_relative_path: "plugins/slack".into(),
            name: "slack".into(),
        }
    );
    // Stale dismiss for a different plugin is a no-op too.
    app.agents.get_mut(&id).unwrap().plugin_cta.phase = CtaPhase::Installed {
        name: "figma".into(),
    };
    dispatch(
        Action::TaskComplete(TaskResult::CtaInstalledDismissTimeout {
            agent_id: id,
            plugin_name: "slack".into(),
        }),
        &mut app,
    );
    assert_eq!(
        app.agents[&id].plugin_cta.phase,
        CtaPhase::Installed {
            name: "figma".into()
        }
    );
}

#[test]
fn cta_mcps_loaded_err_sets_error() {
    use crate::app::agent_view::CtaPhase;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().plugin_cta.phase = CtaPhase::AwaitingMcps {
        name: "figma".into(),
    };
    let effects = dispatch(
        Action::TaskComplete(TaskResult::PluginCtaMcpsLoaded {
            agent_id: id,
            plugin_name: "figma".into(),
            result: Err("mcps boom".into()),
        }),
        &mut app,
    );
    assert!(effects.is_empty());
    match &app.agents[&id].plugin_cta.phase {
        CtaPhase::Error {
            plugin_relative_path,
            name,
            message,
        } => {
            assert_eq!(plugin_relative_path.as_str(), "plugins/figma");
            assert_eq!(name.as_str(), "figma");
            assert_eq!(message.as_str(), "mcps boom");
        }
        other => panic!("expected Error, got {other:?}"),
    }
    assert!(app.agents[&id].extensions_modal.is_none());
}

#[test]
fn cta_mcps_loaded_ignored_for_stale_phase_or_plugin() {
    use crate::app::agent_view::CtaPhase;
    use crate::views::mcps_modal::McpServerDisplayStatus;
    // Wrong plugin name.
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().plugin_cta.phase = CtaPhase::AwaitingMcps {
        name: "figma".into(),
    };
    let effects = dispatch(
        Action::TaskComplete(TaskResult::PluginCtaMcpsLoaded {
            agent_id: id,
            plugin_name: "slack".into(),
            result: Ok(vec![cta_mcp_server(
                "x",
                Some("slack"),
                McpServerDisplayStatus::NeedsAuth,
            )]),
        }),
        &mut app,
    );
    assert!(effects.is_empty());
    assert_eq!(
        app.agents[&id].plugin_cta.phase,
        CtaPhase::AwaitingMcps {
            name: "figma".into()
        }
    );
    assert!(app.agents[&id].extensions_modal.is_none());
    // Non-matching phase (Installing, not AwaitingMcps).
    app.agents.get_mut(&id).unwrap().plugin_cta.phase = CtaPhase::Installing {
        plugin_relative_path: "plugins/figma".into(),
        name: "figma".into(),
    };
    let effects = dispatch(
        Action::TaskComplete(TaskResult::PluginCtaMcpsLoaded {
            agent_id: id,
            plugin_name: "figma".into(),
            result: Ok(vec![]),
        }),
        &mut app,
    );
    assert!(effects.is_empty());
    assert!(matches!(
        app.agents[&id].plugin_cta.phase,
        CtaPhase::Installing { .. }
    ));
}

#[allow(clippy::module_inception)]
mod cta_e2e {
    use super::{cta_entry, cta_mcp_server, cta_outcome, cta_outcome_reload, test_app_with_agent};
    use crate::app::actions::{Action, Effect, TaskResult};
    use crate::app::agent::AgentId;
    use crate::app::agent_view::CtaPhase;
    use crate::app::app_view::{AppView, InputOutcome};
    use crate::app::dispatch::cta::{CTA_MCP_POLL_MAX_ATTEMPTS, plugin_cta_phase_for};
    use crate::app::dispatch::dispatch;
    use crate::views::extensions_modal::{ExtensionsTab, TabDataState};
    use crate::views::mcps_modal::{McpSectionId, McpServerDisplayStatus, section_key};
    use echo_agent_hooks_plugins_types::OutcomeStatus;

    const PROMPT: &str = "please open figma now";

    fn figma_candidate() -> echo_agent_hooks_plugins_types::MarketplacePluginEntry {
        let mut entry = cta_entry("figma", "not_installed");
        entry.keywords = vec!["figma".into()];
        // MCP-bearing plugin: install enters AwaitingMcps and polls for auth.
        entry.has_mcp = true;
        entry
    }

    fn left_click(col: u16, row: u16) -> crossterm::event::Event {
        crossterm::event::Event::Mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: col,
            row,
            modifiers: crossterm::event::KeyModifiers::NONE,
        })
    }

    fn isolate_echo_agent_home() {
        use std::sync::OnceLock;
        static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
        HOME.get_or_init(|| {
            let tmp = tempfile::tempdir().expect("tempdir creation");
            unsafe {
                std::env::set_var("ECHO_AGENT_HOME", tmp.path());
            }
            tmp
        });
    }

    fn app_matched() -> AppView {
        isolate_echo_agent_home();
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        app.plugin_cta_enabled = true;
        {
            let cta = &mut app.agents.get_mut(&id).unwrap().plugin_cta;
            cta.source_url_or_path =
                Some(echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL.into());
            cta.candidates = vec![figma_candidate()];
            cta.debounce_generation = 1;
        }
        app.agents.get_mut(&id).unwrap().prompt.set_text(PROMPT);
        let effects = dispatch(
            Action::TaskComplete(TaskResult::PluginCtaDebounceExpired {
                agent_id: id,
                generation: 1,
            }),
            &mut app,
        );
        assert!(effects.is_empty());
        assert_eq!(
            app.agents[&id].plugin_cta.phase,
            CtaPhase::Matched {
                plugin_relative_path: "plugins/figma".into(),
                name: "figma".into(),
            }
        );
        app
    }

    fn connect(app: &mut AppView) -> Vec<Effect> {
        let id = AgentId(0);
        app.agents.get_mut(&id).unwrap().plugin_cta.hit_connect.rect =
            Some(ratatui::layout::Rect::new(2, 3, 9, 1));
        let outcome = app.handle_input(&left_click(3, 3));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(matches!(
            app.agents[&id].plugin_cta.phase,
            CtaPhase::Installing { .. }
        ));
        std::mem::take(&mut app.pending_effects)
    }

    fn app_awaiting_mcps() -> AppView {
        let mut app = app_matched();
        let id = AgentId(0);
        connect(&mut app);
        dispatch(
            Action::TaskComplete(TaskResult::CtaPluginInstallDone {
                agent_id: id,
                plugin_name: "figma".into(),
                result: Ok(cta_outcome(OutcomeStatus::Success, "installed")),
            }),
            &mut app,
        );
        assert_eq!(
            app.agents[&id].plugin_cta.phase,
            CtaPhase::AwaitingMcps {
                name: "figma".into()
            }
        );
        app
    }

    #[test]
    fn happy_path_with_auth_handoff() {
        let mut app = app_matched();
        let id = AgentId(0);

        let effects = connect(&mut app);
        match effects.as_slice() {
            [
                Effect::InstallPluginFromCta {
                    source_url_or_path,
                    plugin_relative_path,
                    ..
                },
            ] => {
                assert_eq!(
                    source_url_or_path,
                    echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL
                );
                assert_eq!(plugin_relative_path, "plugins/figma");
            }
            other => panic!("expected InstallPluginFromCta, got {other:?}"),
        }

        let effects = dispatch(
            Action::TaskComplete(TaskResult::CtaPluginInstallDone {
                agent_id: id,
                plugin_name: "figma".into(),
                result: Ok(cta_outcome_reload(OutcomeStatus::Success, "installed")),
            }),
            &mut app,
        );
        assert_eq!(
            app.agents[&id].plugin_cta.phase,
            CtaPhase::AwaitingReload {
                name: "figma".into()
            }
        );
        assert!(matches!(
            effects.as_slice(),
            [Effect::ReloadPluginsForCta { plugin_name, .. }] if plugin_name == "figma"
        ));

        let effects = dispatch(
            Action::TaskComplete(TaskResult::CtaPluginReloadDone {
                agent_id: id,
                plugin_name: "figma".into(),
                result: Ok(cta_outcome(OutcomeStatus::Success, "reloaded")),
            }),
            &mut app,
        );
        assert_eq!(
            app.agents[&id].plugin_cta.phase,
            CtaPhase::AwaitingMcps {
                name: "figma".into()
            }
        );
        assert!(matches!(
            effects.as_slice(),
            [Effect::FetchPluginCtaMcps { plugin_name, .. }] if plugin_name == "figma"
        ));

        let servers = vec![
            cta_mcp_server(
                "echo_agent_com_managed",
                None,
                McpServerDisplayStatus::Ready,
            ),
            cta_mcp_server("local-srv", None, McpServerDisplayStatus::Ready),
            cta_mcp_server("other-srv", Some("slack"), McpServerDisplayStatus::Ready),
            cta_mcp_server(
                "figma-srv",
                Some("figma"),
                McpServerDisplayStatus::NeedsAuth,
            ),
        ];
        let effects = dispatch(
            Action::TaskComplete(TaskResult::PluginCtaMcpsLoaded {
                agent_id: id,
                plugin_name: "figma".into(),
                result: Ok(servers),
            }),
            &mut app,
        );
        assert_eq!(app.agents[&id].plugin_cta.phase, CtaPhase::Hidden);
        let modal = app.agents[&id]
            .extensions_modal
            .as_ref()
            .expect("extensions modal should be open");
        assert_eq!(modal.active_tab, ExtensionsTab::McpServers);
        match &modal.mcps_data {
            TabDataState::Loaded(servers) => assert_eq!(servers.len(), 4),
            other => panic!("expected mcps_data Loaded, got {other:?}"),
        }
        let collapsed = &modal.mcps_collapsed_sections;
        assert!(collapsed.contains(&section_key(&McpSectionId::Managed)));
        assert!(collapsed.contains(&section_key(&McpSectionId::Local)));
        assert!(collapsed.contains(&section_key(&McpSectionId::Plugin("slack".into()))));
        assert!(!collapsed.contains(&section_key(&McpSectionId::Plugin("figma".into()))));
        assert!(modal.mcps_section_collapse_initialized);
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::FetchPluginCtaCatalog { .. }))
        );
    }

    #[test]
    fn no_reload_path_enters_awaiting_mcps_directly() {
        let mut app = app_matched();
        let id = AgentId(0);
        connect(&mut app);
        let effects = dispatch(
            Action::TaskComplete(TaskResult::CtaPluginInstallDone {
                agent_id: id,
                plugin_name: "figma".into(),
                result: Ok(cta_outcome(OutcomeStatus::Success, "installed")),
            }),
            &mut app,
        );
        assert_eq!(
            app.agents[&id].plugin_cta.phase,
            CtaPhase::AwaitingMcps {
                name: "figma".into()
            }
        );
        assert!(matches!(
            effects.as_slice(),
            [Effect::FetchPluginCtaMcps { plugin_name, .. }] if plugin_name == "figma"
        ));
    }

    #[test]
    fn no_auth_path_settles_installed_without_modal() {
        let mut app = app_awaiting_mcps();
        let id = AgentId(0);
        // All of the plugin's servers are Ready (terminal, no auth) -> settle.
        let effects = dispatch(
            Action::TaskComplete(TaskResult::PluginCtaMcpsLoaded {
                agent_id: id,
                plugin_name: "figma".into(),
                result: Ok(vec![cta_mcp_server(
                    "figma-srv",
                    Some("figma"),
                    McpServerDisplayStatus::Ready,
                )]),
            }),
            &mut app,
        );
        assert_eq!(
            app.agents[&id].plugin_cta.phase,
            CtaPhase::Installed {
                name: "figma".into()
            }
        );
        assert!(app.agents[&id].extensions_modal.is_none());
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::FetchMcpsList { .. }))
        );
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::RetryPluginCtaMcps { .. }))
        );
        // Settling schedules the ✓ auto-dismiss and refreshes the candidate set.
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::DismissCtaInstalled { .. }))
        );
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::FetchPluginCtaCatalog { .. }))
        );
    }

    #[test]
    fn skills_only_install_settles_installed_without_fetch() {
        let mut app = app_matched();
        let id = AgentId(0);
        // Skills-only plugin: clear has_mcp so connect captures expects_mcp=false.
        app.agents.get_mut(&id).unwrap().plugin_cta.candidates[0].has_mcp = false;
        connect(&mut app);
        let effects = dispatch(
            Action::TaskComplete(TaskResult::CtaPluginInstallDone {
                agent_id: id,
                plugin_name: "figma".into(),
                result: Ok(cta_outcome(OutcomeStatus::Success, "installed")),
            }),
            &mut app,
        );
        // No MCP fetch, no "Setting up…" flash: straight to Installed.
        assert_eq!(
            app.agents[&id].plugin_cta.phase,
            CtaPhase::Installed {
                name: "figma".into()
            }
        );
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::FetchPluginCtaMcps { .. }))
        );
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::DismissCtaInstalled { .. }))
        );
        assert!(app.agents[&id].extensions_modal.is_none());
    }

    #[test]
    fn install_error_settles_error() {
        let mut app = app_matched();
        let id = AgentId(0);
        connect(&mut app);
        let effects = dispatch(
            Action::TaskComplete(TaskResult::CtaPluginInstallDone {
                agent_id: id,
                plugin_name: "figma".into(),
                result: Err("install boom".into()),
            }),
            &mut app,
        );
        assert!(effects.is_empty());
        match &app.agents[&id].plugin_cta.phase {
            CtaPhase::Error {
                plugin_relative_path,
                name,
                message,
            } => {
                assert_eq!(plugin_relative_path, "plugins/figma");
                assert_eq!(name, "figma");
                assert_eq!(message, "install boom");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn reload_error_settles_error() {
        let mut app = app_matched();
        let id = AgentId(0);
        connect(&mut app);
        dispatch(
            Action::TaskComplete(TaskResult::CtaPluginInstallDone {
                agent_id: id,
                plugin_name: "figma".into(),
                result: Ok(cta_outcome_reload(OutcomeStatus::Success, "installed")),
            }),
            &mut app,
        );
        let effects = dispatch(
            Action::TaskComplete(TaskResult::CtaPluginReloadDone {
                agent_id: id,
                plugin_name: "figma".into(),
                result: Err("reload boom".into()),
            }),
            &mut app,
        );
        assert!(effects.is_empty());
        match &app.agents[&id].plugin_cta.phase {
            CtaPhase::Error { name, message, .. } => {
                assert_eq!(name, "figma");
                assert_eq!(message, "reload boom");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn mcps_error_settles_error() {
        let mut app = app_awaiting_mcps();
        let id = AgentId(0);
        let effects = dispatch(
            Action::TaskComplete(TaskResult::PluginCtaMcpsLoaded {
                agent_id: id,
                plugin_name: "figma".into(),
                result: Err("mcps boom".into()),
            }),
            &mut app,
        );
        assert!(effects.is_empty());
        match &app.agents[&id].plugin_cta.phase {
            CtaPhase::Error { name, message, .. } => {
                assert_eq!(name, "figma");
                assert_eq!(message, "mcps boom");
            }
            other => panic!("expected Error, got {other:?}"),
        }
        assert!(app.agents[&id].extensions_modal.is_none());
    }

    #[test]
    fn dismissed_plugin_suppressed_on_next_recompute() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        let candidates = vec![figma_candidate()];

        let dismissed = echo_agent_runtime::config::dismissed_plugin_ctas_in_file(&config_path);
        let matched =
            plugin_cta_phase_for(true, true, &candidates, PROMPT, |id| dismissed.contains(id));
        assert_eq!(
            matched,
            CtaPhase::Matched {
                plugin_relative_path: "plugins/figma".into(),
                name: "figma".into(),
            }
        );

        echo_agent_runtime::config::add_dismissed_plugin_cta_to_file("figma", &config_path)
            .expect("persist dismissal");
        let dismissed = echo_agent_runtime::config::dismissed_plugin_ctas_in_file(&config_path);
        assert!(dismissed.contains("figma"));

        let hidden =
            plugin_cta_phase_for(true, true, &candidates, PROMPT, |id| dismissed.contains(id));
        assert_eq!(hidden, CtaPhase::Hidden);
    }

    #[test]
    fn flag_off_or_source_absent_hides_regardless_of_text() {
        for (enabled, source_present) in [(false, true), (true, false)] {
            let mut app = test_app_with_agent();
            let id = AgentId(0);
            app.plugin_cta_enabled = enabled;
            {
                let cta = &mut app.agents.get_mut(&id).unwrap().plugin_cta;
                cta.source_url_or_path = source_present
                    .then(|| echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL.to_string());
                cta.candidates = vec![figma_candidate()];
                cta.debounce_generation = 1;
                cta.phase = CtaPhase::Matched {
                    plugin_relative_path: "plugins/figma".into(),
                    name: "figma".into(),
                };
            }
            app.agents.get_mut(&id).unwrap().prompt.set_text(PROMPT);
            let effects = dispatch(
                Action::TaskComplete(TaskResult::PluginCtaDebounceExpired {
                    agent_id: id,
                    generation: 1,
                }),
                &mut app,
            );
            assert!(effects.is_empty());
            assert_eq!(
                app.agents[&id].plugin_cta.phase,
                CtaPhase::Hidden,
                "enabled={enabled} source_present={source_present}"
            );
        }
    }

    #[test]
    fn plugin_name_parity_match_hands_off() {
        let mut app = app_awaiting_mcps();
        let id = AgentId(0);
        dispatch(
            Action::TaskComplete(TaskResult::PluginCtaMcpsLoaded {
                agent_id: id,
                plugin_name: "figma".into(),
                result: Ok(vec![cta_mcp_server(
                    "figma-srv",
                    Some("figma"),
                    McpServerDisplayStatus::NeedsAuth,
                )]),
            }),
            &mut app,
        );
        assert_eq!(app.agents[&id].plugin_cta.phase, CtaPhase::Hidden);
        assert!(app.agents[&id].extensions_modal.is_some());
    }

    #[test]
    fn plugin_name_parity_mismatch_degrades_to_installed() {
        let mut app = app_awaiting_mcps();
        let id = AgentId(0);
        // A NeedsAuth server whose section plugin-name does not match the CTA
        // name is not a handoff trigger. Under the poll it keeps waiting, so
        // drive it to the attempt budget to force the terminal no-auth verdict.
        app.agents.get_mut(&id).unwrap().plugin_cta.mcp_attempt = CTA_MCP_POLL_MAX_ATTEMPTS;
        let effects = dispatch(
            Action::TaskComplete(TaskResult::PluginCtaMcpsLoaded {
                agent_id: id,
                plugin_name: "figma".into(),
                result: Ok(vec![cta_mcp_server(
                    "figma-srv",
                    Some("figma-connector"),
                    McpServerDisplayStatus::NeedsAuth,
                )]),
            }),
            &mut app,
        );
        assert_eq!(
            app.agents[&id].plugin_cta.phase,
            CtaPhase::Installed {
                name: "figma".into()
            }
        );
        assert!(app.agents[&id].extensions_modal.is_none());
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::RetryPluginCtaMcps { .. }))
        );
    }
}
