use super::*;

// ── Fetch ───────────────────────────────────────────────────────────────────

/// Build the prefetched model map from a flat list of entries.
pub(crate) fn build_prefetched_map(
    models: Vec<config::ModelEntryConfig>,
    api_base_url_override: Option<String>,
) -> IndexMap<String, ModelEntry> {
    let mut map: IndexMap<String, ModelEntry> = IndexMap::with_capacity(models.len());
    for m in models {
        let key = m.id.clone().unwrap_or_else(|| m.model.clone());
        let info = config::ModelInfo::from_config(&m);
        let entry = ModelEntry {
            info,
            api_key: None,
            env_key: None,
            auth_provider: None,
            api_base_url: m.api_base_url.clone().or(api_base_url_override.clone()),
        };
        map.insert(key, entry);
    }
    map
}

/// Fetch remote models. Checks disk cache first; persists after fetch.
pub(crate) fn prefetch_models_blocking(
    endpoints: &config::EndpointsConfig,
    auth: Option<&EchoAgentAuth>,
    fetch_auth: ModelFetchAuth,
) -> Option<IndexMap<String, ModelEntry>> {
    prefetch_models_blocking_gated(
        endpoints,
        auth,
        fetch_auth,
        crate::util::config::resolve_remote_fetch_enabled(),
    )
}

/// Blocking models + `/v1/settings` prefetch pair, shared by the early
pub(crate) fn prefetch_models_and_settings_blocking(
    endpoints: &config::EndpointsConfig,
    auth: Option<&EchoAgentAuth>,
    fetch_auth: ModelFetchAuth,
) -> (
    Option<IndexMap<String, ModelEntry>>,
    Option<crate::util::config::RemoteSettings>,
) {
    let remote_fetch_enabled = crate::util::config::resolve_remote_fetch_enabled();
    let models = prefetch_models_blocking_gated(endpoints, auth, fetch_auth, remote_fetch_enabled);
    let settings = match auth {
        Some(auth) if remote_fetch_enabled => {
            let _timer = crate::instrumentation_timer!("startup.early_settings_fetch");
            crate::remote::fetch_settings_blocking(
                &endpoints.proxy_url(),
                auth,
                endpoints.alpha_test_key.as_deref(),
            )
            .into_option()
        }
        _ => None,
    };
    (models, settings)
}

/// `remote_fetch_enabled` is a parameter so the pair helper above resolves the knob once for both halves.
fn prefetch_models_blocking_gated(
    endpoints: &config::EndpointsConfig,
    auth: Option<&EchoAgentAuth>,
    fetch_auth: ModelFetchAuth,
    remote_fetch_enabled: bool,
) -> Option<IndexMap<String, ModelEntry>> {
    let cache_auth = fetch_auth.cache_auth_method();
    let cache_origin = crate::remote::models_list_url(endpoints, fetch_auth);
    let cache = ModelsCacheManager::new();
    if let Some(cached) = cache.load_fresh(&cache_auth, &cache_origin) {
        return Some(cached.models);
    }

    if !remote_fetch_enabled {
        tracing::info!("models fetch skipped: remote_fetch disabled");
        return None;
    }

    let _timer = crate::instrumentation_timer!("startup.fetch_models_blocking");
    match fetch_models_blocking(endpoints, auth, fetch_auth) {
        Ok(FetchModelsResult { models, etag }) if !models.is_empty() => {
            let api_base_url_override = match fetch_auth {
                ModelFetchAuth::ApiKey => Some(endpoints.echo_agent_api_base_url.clone()),
                _ => None,
            };
            let map = build_prefetched_map(models, api_base_url_override);

            tracing::info!(count = map.len(), etag = ?etag, "Prefetched models");
            cache.persist(&map, etag.as_deref(), cache_auth, &cache_origin);
            Some(map)
        }
        Ok(FetchModelsResult { .. }) => {
            tracing::warn!("Models endpoint returned empty list");
            None
        }
        Err(e) => {
            tracing::warn!("Failed to fetch models: {:?}", e);
            None
        }
    }
}

/// Startup prefetch result: models + remote settings.
pub struct EarlyPrefetchResult {
    pub models: Option<IndexMap<String, ModelEntry>>,
    pub settings: Option<crate::util::config::RemoteSettings>,
}

/// Handle for a startup prefetch thread.
pub type EarlyPrefetchHandle = std::thread::JoinHandle<EarlyPrefetchResult>;

pub(crate) struct PrefetchEnv {
    pub(crate) auth: Option<EchoAgentAuth>,
    pub(crate) endpoints: config::EndpointsConfig,
    pub(crate) model_fetch_auth: ModelFetchAuth,
}

/// Effective startup endpoints, resolved config-aware (not env-only) so the prefetch can't leak the bearer to api.echo-agent.invalid.
fn resolve_startup_endpoints() -> config::EndpointsConfig {
    let mut endpoints = config::EndpointsConfig::from_effective_config();
    if endpoints.deployment_key.is_none() {
        endpoints.deployment_key = crate::managed_config::resolve_deployment_key();
    }
    endpoints
}

/// Decision core of the startup prefetch gate, split from the config loading
pub(crate) fn resolve_prefetch_env_from_parts(
    auth: Option<EchoAgentAuth>,
    endpoints: config::EndpointsConfig,
    remote_fetch_enabled: bool,
) -> Option<PrefetchEnv> {
    if !remote_fetch_enabled {
        tracing::info!("startup model/settings prefetch skipped: remote_fetch disabled");
        return None;
    }

    let model_fetch_auth = ModelFetchAuth::resolve(&endpoints, auth.is_some());

    if auth.is_none()
        && !endpoints.has_custom_endpoint()
        && model_fetch_auth == ModelFetchAuth::Session
    {
        return None;
    }

    Some(PrefetchEnv {
        auth,
        endpoints,
        model_fetch_auth,
    })
}

/// Start model + settings prefetch on a background thread using pre-resolved auth.
pub fn start_early_prefetch_with_auth(auth: Option<EchoAgentAuth>) -> Option<EarlyPrefetchHandle> {
    start_early_prefetch_with_auth_gated(auth, true)
}

/// `sync_managed = false` skips the managed-config sync, so a remote kill-switch
/// can apply on cold start before the fail-closed managed-policy gate without an
/// online sync healing a tampered on-disk policy first.
fn start_early_prefetch_with_auth_gated(
    auth: Option<EchoAgentAuth>,
    sync_managed: bool,
) -> Option<EarlyPrefetchHandle> {
    let _timer = crate::instrumentation_timer!("startup.early_prefetch_launch");
    let endpoints = resolve_startup_endpoints();
    if sync_managed {
        spawn_managed_config_sync_if_stale(&endpoints);
    }
    let env = resolve_prefetch_env_from_parts(
        auth,
        endpoints,
        crate::util::config::resolve_remote_fetch_enabled(),
    )?;
    Some(spawn_prefetch_thread(env))
}

/// Start model + settings prefetch on a background thread.
pub fn start_early_prefetch(
    echo_agent_com_config: Option<EchoAgentComConfig>,
) -> Option<EarlyPrefetchHandle> {
    start_early_prefetch_impl(echo_agent_com_config, true)
}

/// Prefetch models + remote settings only — no managed-config sync. Used before
/// the managed-policy gate (see `start_early_prefetch_with_auth_gated`).
pub fn start_early_prefetch_settings_only(
    echo_agent_com_config: Option<EchoAgentComConfig>,
) -> Option<EarlyPrefetchHandle> {
    start_early_prefetch_impl(echo_agent_com_config, false)
}

fn start_early_prefetch_impl(
    echo_agent_com_config: Option<EchoAgentComConfig>,
    sync_managed: bool,
) -> Option<EarlyPrefetchHandle> {
    let echo_agent_home = crate::util::echo_agent_home::echo_agent_home();
    let auth =
        AuthManager::new(&echo_agent_home, echo_agent_com_config.unwrap_or_default()).current();
    start_early_prefetch_with_auth_gated(auth, sync_managed)
}

fn spawn_prefetch_thread(env: PrefetchEnv) -> EarlyPrefetchHandle {
    std::thread::spawn(move || {
        let mut timer = crate::instrumentation_timer!("startup.early_prefetch");
        let proxy_endpoint = env.endpoints.proxy_url();
        timer.with_field("endpoint", proxy_endpoint.as_str());
        let (models, settings) = prefetch_models_and_settings_blocking(
            &env.endpoints,
            env.auth.as_ref(),
            env.model_fetch_auth,
        );
        EarlyPrefetchResult { models, settings }
    })
}

/// Best-effort, bounded managed-config sync on a detached thread, off the readiness path (syncs at launch; the interval task covers steady state).
fn spawn_managed_config_sync_if_stale(endpoints: &config::EndpointsConfig) {
    let should_sync = (endpoints.deployment_key.is_some()
        || crate::managed_config::has_active_team_auth())
        && crate::config::is_managed_config_stale_for(
            &crate::managed_config::current_serving_identity(),
        )
        && crate::managed_config::is_fetch_enabled();
    if !should_sync {
        return;
    }
    std::thread::spawn(|| {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        crate::managed_config::clear_orphan();
        // tokio timer outside a runtime context panics ("no reactor running").
        let _ = rt.block_on(async {
            tokio::time::timeout(
                crate::http::STARTUP_FETCH_TIMEOUT,
                crate::managed_config::sync(),
            )
            .await
        });
    });
}
