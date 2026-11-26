//! Plugin install call-to-action phase tracking helpers and constants.

use super::transcript::extensions_modal_tab_fetches;
use crate::app::actions::Effect;
use crate::app::agent::AgentId;
use crate::app::app_view::AppView;
use agent_client_protocol as acp;
use echo_agent_telemetry::session_ctx::log_event;

/// Max post-install MCP-list re-probes while waiting for a just-installed
/// plugin's MCP servers to reach a terminal state. Probes are ~1s apart
/// (`Effect::RetryPluginCtaMcps`), so the budget bounds the wait at ~15s before
/// a final no-auth verdict is forced.
pub(super) const CTA_MCP_POLL_MAX_ATTEMPTS: u32 = 15;

/// Re-probes tolerated while the just-installed plugin shows *no* MCP servers
/// at all. A plugin's servers are config-loaded during the awaited reload that
/// precedes the first read, so an empty plugin section means it ships none
/// (skills-only): settle quickly instead of polling the full budget (and paying
/// the per-read managed-config fetch each time).
pub(super) const CTA_MCP_ABSENT_MAX_ATTEMPTS: u32 = 1;

/// Settle the CTA into its brief "installed" confirmation: schedule the auto-
/// dismiss timer and, when a session exists, refresh the not-installed candidate
/// set so the just-installed plugin drops out of CTA matching.
pub(super) fn cta_settle_installed(
    cta: &mut crate::app::agent_view::PluginCtaState,
    agent_id: AgentId,
    name: String,
    session_id: Option<acp::SessionId>,
) -> Vec<Effect> {
    cta.phase = crate::app::agent_view::CtaPhase::Installed { name: name.clone() };
    let mut effects = vec![Effect::DismissCtaInstalled {
        agent_id,
        plugin_name: name,
    }];
    if let Some(session_id) = session_id {
        effects.push(Effect::FetchPluginCtaCatalog {
            agent_id,
            session_id,
        });
    }
    effects
}

/// Not-installed CTA candidates from the catalog scan, plus the selected
/// source's URL/path — the install target the shell resolves sources by;
/// `None` means no CTA source was present. One source wins so the candidates
/// and the install target always come from it. With `cta_marketplace` set
/// (the `[marketplace].plugin_cta_marketplace` override), the first source
/// whose name exactly equals it wins — the EchoAgent Official source is excluded
/// unless it is the named one. Unset (the default) is two-tier: a URL-verified
/// official source beats any name-only "EchoAgent Official" match regardless of
/// order (anti-spoofing: the scanned URL is the install root), then a
/// name-only match keeps mirrors registered under the official name working;
/// first registered wins within a tier.
pub(super) fn plugin_cta_candidates(
    response: echo_agent_hooks_plugins_types::MarketplaceListResponse,
    cta_marketplace: Option<&str>,
) -> (
    Vec<echo_agent_hooks_plugins_types::MarketplacePluginEntry>,
    Option<String>,
) {
    let mut sources = response.sources;
    let winner = match cta_marketplace {
        Some(name) => sources.iter().position(|s| s.source_name == name),
        None => sources
            .iter()
            .position(|s| {
                echo_agent_plugin_marketplace::is_official_source_url(&s.source_url_or_path)
            })
            .or_else(|| {
                sources.iter().position(|s| {
                    s.source_name == echo_agent_plugin_marketplace::OFFICIAL_SOURCE_NAME
                })
            }),
    };
    let Some(idx) = winner else {
        return (Vec::new(), None);
    };
    let source = sources.swap_remove(idx);
    let candidates = source
        .plugins
        .into_iter()
        .filter(|plugin| plugin.install_status == "not_installed")
        .collect();
    (candidates, Some(source.source_url_or_path))
}

/// Resolve the marketplace-relative path for a CTA plugin by name, used to
/// rebuild a retryable `CtaPhase::Error` after a post-install hop fails. Prefers
/// the still-cached candidate entry; the `plugins/{name}` fallback assumes the
/// conventional marketplace layout (a guess for a configured override source).
pub(super) fn cta_install_relative_path(
    candidates: &[echo_agent_hooks_plugins_types::MarketplacePluginEntry],
    name: &str,
) -> String {
    candidates
        .iter()
        .find(|c| c.name == name)
        .map(|c| c.relative_path.clone())
        .unwrap_or_else(|| format!("plugins/{name}"))
}

pub(super) fn cta_install_error_category(
    result: &Result<echo_agent_hooks_plugins_types::ActionOutcome, String>,
) -> Option<String> {
    use echo_agent_hooks_plugins_types::OutcomeStatus;
    match result {
        Ok(outcome) => match outcome.status {
            OutcomeStatus::Success => None,
            OutcomeStatus::ValidationError => Some("validation_error".to_string()),
            OutcomeStatus::ConfirmationRequired => Some("confirmation_required".to_string()),
            OutcomeStatus::NotFound => Some("not_found".to_string()),
            OutcomeStatus::InternalError => Some("internal_error".to_string()),
            OutcomeStatus::Unsupported => Some("unsupported".to_string()),
        },
        Err(_) => Some("action_failed".to_string()),
    }
}

/// Recompute the plugin-CTA phase from the current prompt draft.
///
/// Gating order: feature flag + CTA source present (official, or the
/// configured `plugin_cta_marketplace`), keyword match, then per-plugin
/// dismissal (`is_dismissed` injects the config lookup so the matcher logic
/// stays unit-testable).
pub(super) fn plugin_cta_phase_for(
    enabled: bool,
    cta_source_present: bool,
    candidates: &[echo_agent_hooks_plugins_types::MarketplacePluginEntry],
    prompt_text: &str,
    is_dismissed: impl Fn(&str) -> bool,
) -> crate::app::agent_view::CtaPhase {
    use crate::app::agent_view::CtaPhase;
    use echo_agent_plugin_marketplace::matcher::{KeywordCandidate, match_plugin_keyword};

    if !(enabled && cta_source_present) {
        return CtaPhase::Hidden;
    }
    let cands: Vec<KeywordCandidate<'_>> = candidates
        .iter()
        .map(|entry| KeywordCandidate {
            name: entry.name.as_str(),
            domains: &entry.domains,
            keywords: &entry.keywords,
        })
        .collect();
    let Some(idx) = match_plugin_keyword(prompt_text, &cands) else {
        return CtaPhase::Hidden;
    };
    let entry = &candidates[idx];
    if is_dismissed(&entry.name) {
        return CtaPhase::Hidden;
    }
    CtaPhase::Matched {
        plugin_relative_path: entry.relative_path.clone(),
        name: entry.name.clone(),
    }
}

pub(super) fn cta_impression_plugin_name<'a>(
    prev: &crate::app::agent_view::CtaPhase,
    next: &'a crate::app::agent_view::CtaPhase,
) -> Option<&'a str> {
    use crate::app::agent_view::CtaPhase;
    let CtaPhase::Matched { name, .. } = next else {
        return None;
    };
    match prev {
        CtaPhase::Matched {
            name: prev_name, ..
        } if prev_name == name => None,
        _ => Some(name.as_str()),
    }
}

// TaskResult handlers.

pub(super) fn handle_cta_plugin_install_done(
    app: &mut AppView,
    agent_id: AgentId,
    plugin_name: String,
    result: Result<echo_agent_hooks_plugins_types::ActionOutcome, String>,
) -> Vec<Effect> {
    use crate::app::agent_view::CtaPhase;
    let Some(agent) = app.agents.get_mut(&agent_id) else {
        return vec![];
    };
    let CtaPhase::Installing {
        plugin_relative_path,
        name,
    } = &agent.plugin_cta.phase
    else {
        return vec![];
    };
    let installing_leaf = plugin_relative_path
        .rsplit('/')
        .next()
        .unwrap_or(plugin_relative_path.as_str());
    if installing_leaf != plugin_name {
        return vec![];
    }
    let plugin_relative_path = plugin_relative_path.clone();
    let name = name.clone();
    let session_id = agent.session.session_id.clone();
    let error_category = cta_install_error_category(&result);
    log_event(echo_agent_telemetry::events::PluginCtaInstalled {
        plugin_name: name.clone(),
        success: error_category.is_none(),
        error_category,
    });
    // Ok(requires_reload) on success; Err(message) otherwise.
    let install_result = match result {
        Ok(outcome) if outcome.status == echo_agent_hooks_plugins_types::OutcomeStatus::Success => {
            Ok(outcome.requires_reload)
        }
        Ok(outcome) => Err(crate::app::effects::sanitize_user_error(&outcome.message)),
        Err(e) => Err(e),
    };
    match (install_result, session_id) {
        (Ok(true), Some(session_id)) => {
            agent.plugin_cta.phase = CtaPhase::AwaitingReload { name: name.clone() };
            vec![Effect::ReloadPluginsForCta {
                agent_id,
                session_id,
                plugin_name: name,
            }]
        }
        (Ok(false), Some(session_id)) => {
            if agent.plugin_cta.expects_mcp {
                agent.plugin_cta.phase = CtaPhase::AwaitingMcps { name: name.clone() };
                agent.plugin_cta.mcp_attempt = 0;
                vec![Effect::FetchPluginCtaMcps {
                    agent_id,
                    session_id,
                    plugin_name: name,
                }]
            } else {
                // Skills-only plugin: no MCP servers to wait on, so skip
                // the fetch/"Setting up…" flash and settle immediately.
                cta_settle_installed(&mut agent.plugin_cta, agent_id, name, Some(session_id))
            }
        }
        // Install succeeded but the session vanished; nothing left to
        // chain, so show the brief installed confirmation.
        (Ok(_), None) => cta_settle_installed(&mut agent.plugin_cta, agent_id, name, None),
        (Err(message), _) => {
            agent.plugin_cta.phase = CtaPhase::Error {
                plugin_relative_path,
                name,
                message,
            };
            vec![]
        }
    }
}

pub(super) fn handle_cta_plugin_reload_done(
    app: &mut AppView,
    agent_id: AgentId,
    plugin_name: String,
    result: Result<echo_agent_hooks_plugins_types::ActionOutcome, String>,
) -> Vec<Effect> {
    use crate::app::agent_view::CtaPhase;
    let Some(agent) = app.agents.get_mut(&agent_id) else {
        return vec![];
    };
    // Stale guard: only act on the reload we are currently awaiting for
    // this plugin.
    let CtaPhase::AwaitingReload { name } = &agent.plugin_cta.phase else {
        return vec![];
    };
    if *name != plugin_name {
        return vec![];
    }
    let name = name.clone();
    let session_id = agent.session.session_id.clone();
    // Mirror the install handler: a non-Success outcome is a failure, not
    // a reason to advance into the post-install pipeline.
    let reload_result = match result {
        Ok(outcome) if outcome.status == echo_agent_hooks_plugins_types::OutcomeStatus::Success => {
            Ok(())
        }
        Ok(outcome) => Err(crate::app::effects::sanitize_user_error(&outcome.message)),
        Err(e) => Err(e),
    };
    match reload_result {
        Ok(()) => match session_id {
            Some(session_id) if agent.plugin_cta.expects_mcp => {
                agent.plugin_cta.phase = CtaPhase::AwaitingMcps { name: name.clone() };
                agent.plugin_cta.mcp_attempt = 0;
                vec![Effect::FetchPluginCtaMcps {
                    agent_id,
                    session_id,
                    plugin_name: name,
                }]
            }
            // Skills-only plugin (or no session): settle immediately
            // without an MCP fetch.
            session_id => cta_settle_installed(&mut agent.plugin_cta, agent_id, name, session_id),
        },
        Err(message) => {
            let plugin_relative_path =
                cta_install_relative_path(&agent.plugin_cta.candidates, &name);
            agent.plugin_cta.phase = CtaPhase::Error {
                plugin_relative_path,
                name,
                message,
            };
            vec![]
        }
    }
}

pub(super) fn handle_plugin_cta_mcps_loaded(
    app: &mut AppView,
    agent_id: AgentId,
    plugin_name: String,
    result: Result<Vec<crate::views::mcps_modal::McpServerInfo>, String>,
) -> Vec<Effect> {
    use crate::app::agent_view::CtaPhase;
    use crate::views::extensions_modal::{
        ExtensionsModalState, ExtensionsTab, TabDataState, seed_mcps_section_collapse_for_cta,
    };
    use crate::views::mcps_modal::{McpSectionId, McpServerDisplayStatus, section_for};
    let Some(agent) = app.agents.get_mut(&agent_id) else {
        return vec![];
    };
    // Stale guard: only act on the read we are currently awaiting for
    // this plugin.
    let CtaPhase::AwaitingMcps { name } = &agent.plugin_cta.phase else {
        return vec![];
    };
    if *name != plugin_name {
        return vec![];
    }
    let name = name.clone();
    let session_id = agent.session.session_id.clone();
    match result {
        Ok(servers) => {
            // MCP servers re-initialize progressively after install, so a
            // single early read can miss OAuth servers that only reach
            // NeedsAuth seconds later. Decide now only on a terminal
            // verdict; otherwise keep polling until the plugin's servers
            // settle or the attempt budget runs out.
            let section = McpSectionId::Plugin(name.clone());
            let needs_auth = servers.iter().any(|s| {
                s.status == McpServerDisplayStatus::NeedsAuth && section_for(s) == section
            });
            let any_plugin_server = servers.iter().any(|s| section_for(s) == section);
            // Settle (no auth) only on a clean verdict: every plugin
            // server is Ready. While any is still Initializing or
            // Unavailable the verdict isn't final -- an OAuth server can
            // briefly surface as Unavailable before it flips to NeedsAuth
            // -- so keep polling. needs_auth is handled above.
            let all_ready = servers
                .iter()
                .filter(|s| section_for(s) == section)
                .all(|s| s.status == McpServerDisplayStatus::Ready);
            let settled = any_plugin_server && all_ready;
            let timed_out = agent.plugin_cta.mcp_attempt >= CTA_MCP_POLL_MAX_ATTEMPTS;
            // Skills-only plugins show an empty plugin section even though
            // the rest of the MCP list is populated (all plugin configs
            // load together during the awaited reload that precedes this
            // read). Requiring a non-empty list ensures a read-too-early
            // result (no servers at all) keeps polling rather than being
            // mistaken for skills-only and skipping a slow MCP-bearing
            // plugin's auth handoff; an all-empty list falls through to the
            // attempt-budget timeout.
            let absent_settle = !any_plugin_server
                && !servers.is_empty()
                && agent.plugin_cta.mcp_attempt >= CTA_MCP_ABSENT_MAX_ATTEMPTS;
            let mut effects = Vec::new();
            if needs_auth {
                // Hand off into the Extensions modal on the MCP Servers tab
                // with only the new plugin's section expanded. Seed the MCP
                // data from the read we already have (no flash) and emit the
                // same tab fetch-set as a manual open so no other tab is left
                // stuck Loading; the modal then owns the auth UX.
                let mut modal = ExtensionsModalState::new(ExtensionsTab::McpServers);
                modal.session_team_id = app.team_id.clone();
                seed_mcps_section_collapse_for_cta(
                    &mut modal.mcps_collapsed_sections,
                    &mut modal.mcps_section_collapse_initialized,
                    &servers,
                    &name,
                );
                modal.mcps_data = TabDataState::Loaded(servers);
                agent.agents_modal = None;
                agent.extensions_modal = Some(modal);
                log_event(echo_agent_telemetry::events::ExtensionsModalOpened {
                    trigger: echo_agent_telemetry::events::ExtensionsModalTrigger::AuthHandoff,
                    tab: ExtensionsTab::McpServers.telemetry_tab(),
                });
                agent.plugin_cta.phase = CtaPhase::Hidden;
                if let Some(session_id) = session_id.clone() {
                    if let Some(modal) = agent.extensions_modal.as_mut() {
                        effects.extend(extensions_modal_tab_fetches(modal, agent_id, session_id));
                    }
                } else {
                    agent.pending_extensions_fetch = true;
                }
                if let Some(session_id) = session_id {
                    effects.push(Effect::FetchPluginCtaCatalog {
                        agent_id,
                        session_id,
                    });
                }
            } else if !agent.plugin_cta.expects_mcp || settled || timed_out || absent_settle {
                // Skills-only plugin, all-Ready read, no servers at all, or
                // budget exhausted: show the brief installed confirmation.
                effects.extend(cta_settle_installed(
                    &mut agent.plugin_cta,
                    agent_id,
                    name,
                    session_id,
                ));
            } else if let Some(session_id) = session_id {
                // MCP init hasn't settled and a verdict is still possible;
                // re-probe after a short delay and stay in AwaitingMcps.
                agent.plugin_cta.mcp_attempt += 1;
                effects.push(Effect::RetryPluginCtaMcps {
                    agent_id,
                    session_id,
                    plugin_name: name,
                });
            } else {
                // No session left to re-probe with; settle.
                effects.extend(cta_settle_installed(
                    &mut agent.plugin_cta,
                    agent_id,
                    name,
                    None,
                ));
            }
            effects
        }
        Err(message) => {
            let plugin_relative_path =
                cta_install_relative_path(&agent.plugin_cta.candidates, &name);
            agent.plugin_cta.phase = CtaPhase::Error {
                plugin_relative_path,
                name,
                message,
            };
            vec![]
        }
    }
}

pub(super) fn handle_plugin_cta_catalog_loaded(
    app: &mut AppView,
    agent_id: AgentId,
    result: Result<echo_agent_hooks_plugins_types::MarketplaceListResponse, String>,
) -> Vec<Effect> {
    use crate::app::agent_view::CtaPhase;
    match result {
        Ok(mut response) => {
            response.sanitize();
            let enabled = app.plugin_cta_enabled;
            let cta_marketplace = app.plugin_cta_marketplace.as_deref();
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                let (candidates, source_url_or_path) =
                    plugin_cta_candidates(response, cta_marketplace);
                agent.plugin_cta.candidates = candidates;
                agent.plugin_cta.source_url_or_path = source_url_or_path;
                // Cache the dismissed set once here so the matched-debounce
                // recompute never reads config.toml from the UI thread.
                // Only needed when enabled (the matcher short-circuits to
                // Hidden before consulting it otherwise).
                if enabled {
                    agent.plugin_cta.dismissed =
                        echo_agent_runtime::config::dismissed_plugin_ctas();
                }
                // Recompute the matcher-driven phase now that the catalog
                // landed: a type-and-pause before the async catalog arrived
                // (common at startup) should surface the CTA without another
                // keystroke. This also subsumes the empty-candidate retract.
                // Only touch matcher-driven phases; Installing/AwaitingReload/
                // AwaitingMcps/Installed/Error own their own lifecycle.
                if matches!(
                    agent.plugin_cta.phase,
                    CtaPhase::Hidden | CtaPhase::Matched { .. }
                ) {
                    let prompt_text = agent.prompt.text().to_string();
                    let new_phase = plugin_cta_phase_for(
                        enabled,
                        agent.plugin_cta.source_url_or_path.is_some(),
                        &agent.plugin_cta.candidates,
                        &prompt_text,
                        |name| agent.plugin_cta.dismissed.contains(name),
                    );
                    if let Some(plugin_name) =
                        cta_impression_plugin_name(&agent.plugin_cta.phase, &new_phase)
                    {
                        log_event(echo_agent_telemetry::events::PluginCtaImpression {
                            plugin_name: plugin_name.to_string(),
                        });
                    }
                    if matches!(new_phase, CtaPhase::Hidden) {
                        agent.plugin_cta.hit_connect.clear();
                        agent.plugin_cta.hit_dismiss.clear();
                    }
                    agent.plugin_cta.phase = new_phase;
                }
            }
        }
        Err(e) => {
            tracing::warn!(agent = ?agent_id, error = %e, "couldn't load plugin CTA catalog");
        }
    }
    vec![]
}

pub(super) fn handle_plugin_cta_debounce_expired(
    app: &mut AppView,
    agent_id: AgentId,
    generation: u64,
) -> Vec<Effect> {
    use crate::app::agent_view::CtaPhase;
    let enabled = app.plugin_cta_enabled;
    let Some(agent) = app.agents.get_mut(&agent_id) else {
        return vec![];
    };
    if generation != agent.plugin_cta.debounce_generation {
        return vec![];
    }
    // Preserve in-flight/actionable install states across keystrokes so
    // the eventual install/reload/mcps result is never swallowed by the
    // stale guard. `Installed` is included too: its ✓ confirmation is
    // owned by the auto-dismiss timer, and recomputing during the window
    // could re-offer the just-installed plugin off the stale candidate
    // set before the catalog refresh lands.
    if matches!(
        agent.plugin_cta.phase,
        CtaPhase::Installing { .. }
            | CtaPhase::AwaitingReload { .. }
            | CtaPhase::AwaitingMcps { .. }
            | CtaPhase::Installed { .. }
            | CtaPhase::Error { .. }
    ) {
        return vec![];
    }
    let prompt_text = agent.prompt.text().to_string();
    let new_phase = plugin_cta_phase_for(
        enabled,
        agent.plugin_cta.source_url_or_path.is_some(),
        &agent.plugin_cta.candidates,
        &prompt_text,
        |name| agent.plugin_cta.dismissed.contains(name),
    );
    if let Some(plugin_name) = cta_impression_plugin_name(&agent.plugin_cta.phase, &new_phase) {
        log_event(echo_agent_telemetry::events::PluginCtaImpression {
            plugin_name: plugin_name.to_string(),
        });
    }
    agent.plugin_cta.phase = new_phase;
    vec![]
}
