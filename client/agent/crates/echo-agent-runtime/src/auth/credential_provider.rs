use crate::auth::AuthManager;
use crate::util::echo_agent_auth_credentials::EchoAgentAuthCredentials;
use echo_agent_auth::{
    AuthCredentialProvider, CredentialSnapshot, HttpAuth, StaticAuthCredentialProvider,
};
use reqwest::RequestBuilder;
use std::sync::Arc;
/// `api_key.id` for the active credential: hash the stable API key, never the
/// OIDC bearer (which rotates). `None` for non-API-key auth.
fn api_key_id_for(auth: Option<&crate::auth::EchoAgentAuth>) -> Option<String> {
    auth.filter(|a| matches!(a.auth_mode, crate::auth::AuthMode::ApiKey))
        .map(|a| echo_agent_telemetry::config::deployment_id_from_key(&a.key))
}
/// Sampler [`BearerResolver`](echo_agent_sampler::BearerResolver) over a live
/// [`AuthManager`]: wire-valid only — never stamps a hard-expired access
/// token (the client auth contract). Shared by the session sampler and
/// subagent configs so the contract can't drift between them.
pub(crate) struct WireValidBearerResolver(pub(crate) Arc<AuthManager>);
impl WireValidBearerResolver {
    /// The one constructor both the session sampler and subagent configs use,
    /// so the wire-valid contract cannot drift between the call sites.
    pub(crate) fn shared(
        auth_manager: Arc<AuthManager>,
    ) -> echo_agent_sampler::SharedBearerResolver {
        Arc::new(Self(auth_manager))
    }
}
impl std::fmt::Debug for WireValidBearerResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WireValidBearerResolver").finish()
    }
}
impl echo_agent_sampler::BearerResolver for WireValidBearerResolver {
    fn current_bearer(&self) -> Option<String> {
        self.0.current_wire_valid().map(|a| a.key)
    }
}
/// Production impl: wraps the live `AuthManager`. 401 recovery
/// delegates to `AuthManager::unauthorized_recovery`.
pub(crate) struct ShellAuthCredentialProvider {
    auth_manager: Arc<AuthManager>,
    static_credentials: EchoAgentAuthCredentials,
}
impl ShellAuthCredentialProvider {
    pub(crate) fn new(
        auth_manager: Arc<AuthManager>,
        deployment_key: Option<String>,
        alpha_test_key: Option<String>,
    ) -> Self {
        let mut static_credentials = EchoAgentAuthCredentials::new(None);
        static_credentials.deployment_key = deployment_key;
        static_credentials.alpha_test_key = alpha_test_key;
        Self {
            auth_manager,
            static_credentials,
        }
    }
}
impl std::fmt::Debug for ShellAuthCredentialProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellAuthCredentialProvider")
            .field("auth_manager", &"<configured>")
            .finish()
    }
}
impl HttpAuth for ShellAuthCredentialProvider {
    fn apply(&self, builder: RequestBuilder, base_url: &str) -> RequestBuilder {
        let mut creds = self.static_credentials.clone();
        if creds.deployment_key.is_none()
            && let Some(auth) = self.auth_manager.current_wire_valid()
        {
            creds.user_token = Some(auth.key);
        }
        creds.apply(builder, base_url)
    }
}
#[async_trait::async_trait]
impl AuthCredentialProvider for ShellAuthCredentialProvider {
    fn snapshot(&self) -> CredentialSnapshot {
        if let Some(ref dk) = self.static_credentials.deployment_key {
            return CredentialSnapshot {
                token: Some(dk.clone()),
                deployment_id: crate::managed_config::resolve_deployment_id(Some(dk)),
                ..Default::default()
            };
        }
        let identity = self.auth_manager.current_or_expired();
        let user_id = identity.as_ref().map(|a| a.user_id.clone());
        let team_id = identity.as_ref().and_then(|a| a.team_id.clone());
        let organization_id = identity.as_ref().and_then(|a| a.organization_id.clone());
        let api_key_id = api_key_id_for(identity.as_ref());
        let token = self.auth_manager.current_wire_valid().map(|a| a.key);
        CredentialSnapshot {
            token,
            user_id,
            team_id,
            deployment_id: None,
            api_key_id,
            organization_id,
        }
    }
    async fn refresh_after_unauthorized(&self) -> bool {
        if self.static_credentials.deployment_key.is_some() {
            return false;
        }
        self.auth_manager
            .try_recover_unauthorized(crate::auth::recovery::RecoverySource::Background)
            .await
    }
    fn needs_token_auth_header(&self) -> bool {
        self.static_credentials.deployment_key.is_none()
    }
}
/// Resolves the embedding credentials for `embed_base_url`, attaching the EchoAgent
/// session credential only to EchoAgent-operated endpoints over `https`.
pub(crate) fn embedding_session_credentials(
    embed_base_url: &str,
    auth_manager: Option<&Arc<AuthManager>>,
    api_key_provider: Option<echo_agent_tools::types::SharedApiKeyProvider>,
) -> echo_agent_memory::EndpointScopedCredentials {
    let auth_credentials = auth_manager.map(|am| {
        Arc::new(ShellAuthCredentialProvider::new(am.clone(), None, None))
            as Arc<dyn AuthCredentialProvider>
    });
    echo_agent_memory::EndpointScopedCredentials::for_endpoint(
        embed_base_url,
        crate::util::is_echo_agent_api_bearer_url,
        auth_credentials,
        api_key_provider,
    )
}
/// Build a `StorageClient` for proxy uploads (including the high-volume
/// `batch_upload` used for repo context / `repo_changes_dedup`).
///
/// ### Client Identity (Important)
///
/// Every call site **must** pass the correct `client_identifier` so that
/// the API proxy (and telemetry / metrics backends) can properly attribute requests:
///
/// - `"echo-agent-shell"`   — classic EchoAgent CLI / TUI (echo-agent-runtime)
/// - `"echo-agent-pager"`   — new EchoAgent Pager / TUI (echo-agent-pager)
/// - `"echo-agent-desktop"` — EchoAgent Desktop app
/// - `"echo-agent-extension"` — VS Code / browser extension
///
/// The factory forwards both:
/// - `x-echo-agent-client-version`   (e.g. "0.1.210-alpha.5 (279ffacddb)")
/// - `x-echo-agent-client-identifier`
///
/// to the underlying `echo-agent-file-utils::StorageClient` via
/// `.with_client_identity(...)`. This is what powers the improved error
/// logging on the request-attribution path.
///
/// - When `auth_manager` is `Some`, uses the live `ShellAuthCredentialProvider`
///   (OIDC refresh, proactive refresh, 401 recovery via `unauthorized_recovery`).
/// - When `auth_manager` is `None`, falls back to a static token:
///   - `user_token` (normal OIDC users via com scope) → sends Bearer + `X-Echo-Agent-Token-Auth`
///   - `deployment_key` (enterprise) → sends bare Bearer
///   - the optional extra access key is added only for matching hosts when
///     the non-production feature is enabled.
///
/// `user_token` is only for AuthManager-less one-shots; live paths (incl. pager
/// restore) pass the AuthManager plus an optional deployment key.
pub fn build_storage_client_for_proxy(
    proxy_base_url: &str,
    deployment_key: Option<String>,
    alpha_test_key: Option<String>,
    auth_manager: Option<Arc<AuthManager>>,
    user_token: Option<String>,
    session_id: Option<String>,
    client_identifier: &str,
) -> echo_agent_file_utils::storage_client::StorageClient {
    let http_client = crate::http::shared_upload_client();
    if let Some(am) = auth_manager {
        let provider: Arc<dyn AuthCredentialProvider> = Arc::new(ShellAuthCredentialProvider::new(
            am.clone(),
            deployment_key,
            alpha_test_key,
        ));
        let bridge: Arc<dyn echo_agent_file_utils::storage_client::Auth401AttributionCallback> =
            Arc::new(StorageClientAttributionBridge::new(am, session_id));
        echo_agent_file_utils::storage_client::StorageClient::with_provider(
            proxy_base_url,
            http_client,
            provider,
        )
        .with_client_identity(echo_agent_version::VERSION, client_identifier)
        .with_client_mode(crate::http::process_client_mode())
        .with_attribution(bridge)
    } else {
        let mut creds = EchoAgentAuthCredentials::new(user_token);
        creds.deployment_key = deployment_key;
        creds.alpha_test_key = alpha_test_key;
        let wire_bearer = creds
            .deployment_key
            .clone()
            .or_else(|| creds.user_token.clone());
        let provider: Arc<dyn AuthCredentialProvider> = Arc::new(
            StaticAuthCredentialProvider::new(Box::new(creds), wire_bearer),
        );
        echo_agent_file_utils::storage_client::StorageClient::with_provider(
            proxy_base_url,
            http_client,
            provider,
        )
        .with_client_identity(echo_agent_version::VERSION, client_identifier)
        .with_client_mode(crate::http::process_client_mode())
    }
}
/// Bridge that lets `StorageClient` (which lives in echo-agent-file-utils)
/// emit shell's 401-attribution event without the data-collector crate
/// having a direct dependency on shell. Holds a reference to the live
/// `AuthManager` so attribution events carry the correct user_id.
pub(crate) struct StorageClientAttributionBridge {
    auth_manager: Arc<AuthManager>,
    session_id: Option<String>,
}
impl std::fmt::Debug for StorageClientAttributionBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StorageClientAttributionBridge")
            .finish_non_exhaustive()
    }
}
impl StorageClientAttributionBridge {
    pub(crate) fn new(auth_manager: Arc<AuthManager>, session_id: Option<String>) -> Self {
        Self {
            auth_manager,
            session_id,
        }
    }
}
impl echo_agent_file_utils::storage_client::Auth401AttributionCallback
    for StorageClientAttributionBridge
{
    fn record_401(&self, operation: &str, sent_bearer_prefix: Option<&str>) {
        crate::auth::attribution::record_consumer_401(
            self.auth_manager.as_ref(),
            self.session_id.as_deref(),
            crate::auth::attribution::ConsumerKind::StorageClient,
            operation,
            sent_bearer_prefix,
        );
    }
}
/// Credential provider for the OTel layer's `RefreshableSpanExporter`.
///
/// Starts with a bootstrap `AuthManager` (disk-read-only, no refresher)
/// and can be upgraded to the agent's live `Arc<AuthManager>` once the
/// agent is initialized. After upgrade:
///
/// - `snapshot()` reads from the live manager's in-memory cache (kept
///   hot by the proactive refresh task) instead of re-reading disk.
/// - `refresh_after_unauthorized()` routes through
///   `unauthorized_recovery` for active OIDC/external-binary refresh.
///
/// Before upgrade, the bootstrap manager provides disk-read-only
/// behavior (equivalent to the pre-consolidation OTel path).
pub(crate) struct OtelAuthCredentialProvider {
    /// Bootstrap manager used before the live one is available.
    bootstrap: Arc<AuthManager>,
    /// Swapped to the agent's live `AuthManager` via `set_live()`.
    /// `None` means still in bootstrap mode.
    live: arc_swap::ArcSwap<Option<Arc<AuthManager>>>,
    /// Enterprise deployment key. Takes precedence over OIDC in `snapshot_inner`.
    deployment_key: arc_swap::ArcSwap<Option<String>>,
}
impl OtelAuthCredentialProvider {
    fn new(bootstrap: Arc<AuthManager>) -> Self {
        Self {
            bootstrap,
            live: arc_swap::ArcSwap::from_pointee(None),
            deployment_key: arc_swap::ArcSwap::from_pointee(None),
        }
    }
    /// Upgrade to the agent's live `AuthManager`. After this call,
    /// `snapshot()` reads from the live manager (proactive refresh
    /// keeps it hot) and `refresh_after_unauthorized()` drives the
    /// full recovery state machine.
    pub(crate) fn set_live(&self, auth_manager: Arc<AuthManager>) {
        self.live.store(Arc::new(Some(auth_manager)));
    }
    pub(crate) fn set_deployment_key(&self, key: String) {
        self.deployment_key.store(Arc::new(Some(key)));
    }
    /// Single-load snapshot of the live/bootstrap state.
    fn load_state(&self) -> (Arc<AuthManager>, bool) {
        let guard = self.live.load();
        match guard.as_ref() {
            Some(am) => (am.clone(), true),
            None => (self.bootstrap.clone(), false),
        }
    }
}
impl std::fmt::Debug for OtelAuthCredentialProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (_, is_live) = self.load_state();
        f.debug_struct("OtelAuthCredentialProvider")
            .field("mode", &if is_live { "live" } else { "bootstrap" })
            .finish()
    }
}
impl HttpAuth for OtelAuthCredentialProvider {
    fn apply(&self, builder: RequestBuilder, base_url: &str) -> RequestBuilder {
        let snapshot = self.snapshot_inner();
        let mut creds = EchoAgentAuthCredentials::new(None);
        if self.deployment_key.load().is_some() {
            creds.deployment_key = snapshot.token;
        } else {
            creds.user_token = snapshot.token;
        }
        creds.apply(builder, base_url)
    }
}
impl OtelAuthCredentialProvider {
    fn snapshot_inner(&self) -> CredentialSnapshot {
        if let Some(ref dk) = **self.deployment_key.load() {
            return CredentialSnapshot {
                token: Some(dk.clone()),
                deployment_id: crate::managed_config::resolve_deployment_id(Some(dk)),
                ..Default::default()
            };
        }
        let (am, is_live) = self.load_state();
        if !is_live {
            am.force_reload_from_disk();
        }
        let auth = am.current_or_expired();
        let user_id = auth.as_ref().map(|a| a.user_id.clone());
        let team_id = auth.as_ref().and_then(|a| a.team_id.clone());
        let organization_id = auth.as_ref().and_then(|a| a.organization_id.clone());
        let api_key_id = api_key_id_for(auth.as_ref());
        let token = auth.map(|a| a.key);
        CredentialSnapshot {
            token,
            user_id,
            team_id,
            deployment_id: None,
            api_key_id,
            organization_id,
        }
    }
}
#[async_trait::async_trait]
impl AuthCredentialProvider for OtelAuthCredentialProvider {
    fn snapshot(&self) -> CredentialSnapshot {
        self.snapshot_inner()
    }
    fn has_usable_credential(&self) -> bool {
        if self.deployment_key.load().is_some() {
            return true;
        }
        self.load_state().0.has_usable_token()
    }
    async fn refresh_after_unauthorized(&self) -> bool {
        let (am, is_live) = self.load_state();
        if !is_live {
            return false;
        }
        am.try_recover_unauthorized(crate::auth::recovery::RecoverySource::Background)
            .await
    }
    fn needs_token_auth_header(&self) -> bool {
        self.deployment_key.load().is_none()
    }
}
/// Process-wide OTel credential provider handle.
///
/// This is one of two acceptable process-wide statics in this crate
/// (the other is `TRACER_PROVIDER` in `otel_layer.rs`). It's set
/// once at tracing init (before any `AuthManager` exists) and holds
/// a bootstrap-mode provider. The `ArcSwap` *inside* the provider
/// handles the runtime auth state swap — the `OnceLock` itself is
/// never re-written.
///
/// Why not thread it? `build_default_otel_layer_config` is called
/// from 15+ `init_tracing*` sites across 3 binaries. Threading a
/// handle through all of them to the agent init site (where the
/// live `AuthManager` is constructed) would touch ~20 files for a
/// single-purpose plumbing change. The `OnceLock` holds a
/// container, not auth state; the auth state lives behind `ArcSwap`
/// and changes at runtime.
static OTEL_PROVIDER: std::sync::OnceLock<Arc<OtelAuthCredentialProvider>> =
    std::sync::OnceLock::new();
/// Upgrade the OTel credential provider to use the agent's live
/// `AuthManager`. Call this once after the main `AuthManager` is
/// constructed and has its refresher configured. No-ops if the OTel
/// layer was never initialized (e.g. `InstrumentationMode::Disabled`).
pub(crate) fn wire_otel_auth_manager(auth_manager: Arc<AuthManager>) {
    if let Some(provider) = OTEL_PROVIDER.get() {
        provider.set_live(auth_manager);
        tracing::debug!("otel: upgraded credential provider to live AuthManager");
    }
    sync_external_otel_identity();
}
/// Push the current identity *attributes* (never the token) to the external
/// OTEL stream. Reads the same `CredentialSnapshot` the internal layer
/// stamps per-export, so both pipelines attribute identically. No-op when
/// the OTel provider was never initialized or the external stream is
/// dormant.
pub(crate) fn sync_external_otel_identity() {
    if let Some(provider) = OTEL_PROVIDER.get() {
        let snapshot = provider.snapshot();
        echo_agent_telemetry::external::set_identity(
            echo_agent_telemetry::external::IdentityAttrs::from_snapshot(&snapshot),
        );
    }
}
/// No-ops if the OTel layer was never initialized.
pub(crate) fn wire_otel_deployment_key(key: String) {
    if let Some(provider) = OTEL_PROVIDER.get() {
        provider.set_deployment_key(key);
        tracing::debug!("otel: set deployment key on credential provider");
        sync_external_otel_identity();
    }
}
/// Bootstrap helper: build the full [`OtelLayerConfig`] that both
/// `echo-agent-pager` and `echo-agent-tui` need at tracing init time.
///
/// The credential provider starts in bootstrap mode (disk-read-only).
/// Call [`wire_otel_auth_manager`] after agent init to upgrade to the
/// live `AuthManager` with active refresh.
pub fn build_default_otel_layer_config() -> echo_agent_telemetry::otel_layer::OtelLayerConfig {
    let endpoints = crate::agent::config::EndpointsConfig::default();
    let echo_agent_com_config = crate::auth::EchoAgentComConfig::default();
    let exporter = echo_agent_telemetry::otel_layer::OtelExporterConfig {
        traces_url: endpoints.resolve_otlp_traces_endpoint(),
        extra_headers: endpoints.resolve_otlp_headers(),
        export_interval: endpoints.resolve_otlp_export_interval(),
        timeout: endpoints.resolve_otlp_timeout(),
        enabled: endpoints.resolve_traces_export_enabled()
            && !crate::agent::config::is_telemetry_explicitly_disabled_sync(),
    };
    let token_header_value = echo_agent_com_config.token_header.clone();
    let echo_agent_home = crate::util::echo_agent_home::echo_agent_home();
    let bootstrap = Arc::new(AuthManager::new(&echo_agent_home, echo_agent_com_config));
    let provider = Arc::new(OtelAuthCredentialProvider::new(bootstrap));
    let _ = OTEL_PROVIDER.set(provider.clone());
    echo_agent_telemetry::otel_layer::OtelLayerConfig {
        credentials: provider as Arc<dyn AuthCredentialProvider>,
        token_header_value,
        alpha_test_key: None,
        exporter,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::EchoAgentAuth;
    use crate::auth::EchoAgentComConfig;
    use crate::auth::manager::AuthManager;
    use chrono::{Duration as ChronoDuration, Utc};
    use echo_agent_auth::AuthCredentialProvider;
    use std::sync::Mutex;
    /// Serializes tests that pin `ECHO_AGENT_AUTH_EARLY_INVALIDATION_SECS`, since
    /// env vars are process-global and parallel tests would race.
    static EARLY_INVALIDATION_LOCK: Mutex<()> = Mutex::new(());
    /// RAII guard: pins `ECHO_AGENT_AUTH_EARLY_INVALIDATION_SECS` to the production
    /// default (300s) while held, restoring the previous value on drop.
    /// Acquires `EARLY_INVALIDATION_LOCK` so concurrent test runners can't
    /// observe a half-mutated env.
    struct EarlyInvalidationGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        previous: Option<String>,
    }
    impl EarlyInvalidationGuard {
        fn pin_to_default() -> Self {
            let lock = EARLY_INVALIDATION_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let previous = std::env::var("ECHO_AGENT_AUTH_EARLY_INVALIDATION_SECS").ok();
            unsafe { std::env::set_var("ECHO_AGENT_AUTH_EARLY_INVALIDATION_SECS", "300") };
            Self {
                _lock: lock,
                previous,
            }
        }
    }
    impl Drop for EarlyInvalidationGuard {
        fn drop(&mut self) {
            unsafe {
                match self.previous.take() {
                    Some(prev) => {
                        std::env::set_var("ECHO_AGENT_AUTH_EARLY_INVALIDATION_SECS", prev)
                    }
                    None => std::env::remove_var("ECHO_AGENT_AUTH_EARLY_INVALIDATION_SECS"),
                }
            }
        }
    }
    fn make_auth(key: &str, expires_in: ChronoDuration) -> EchoAgentAuth {
        EchoAgentAuth {
            key: key.to_string(),
            user_id: "test-user".to_string(),
            create_time: Utc::now(),
            expires_at: Some(Utc::now() + expires_in),
            ..EchoAgentAuth::test_default()
        }
    }
    /// Build an `AuthManager` rooted at `dir`. Caller keeps `dir` alive for
    /// the duration of the test so the `TempDir` `Drop` actually cleans up.
    fn make_manager(dir: &tempfile::TempDir, initial: Option<EchoAgentAuth>) -> Arc<AuthManager> {
        let mgr = AuthManager::new(dir.path(), EchoAgentComConfig::default());
        if let Some(auth) = initial {
            mgr.hot_swap(auth);
        }
        Arc::new(mgr)
    }
    /// The shell half of the subagent-401 contract (the sampler half is
    /// pinned in echo-agent-sampler's resolver tests): over a real
    /// `AuthManager`, the resolver returns `None` when hard-expired
    /// (fail-closed), the token inside the early-invalidation buffer
    /// (still proxy-accepted), and the fresh token after a rotation --
    /// same resolver, no client rebuild.
    #[test]
    fn wire_valid_resolver_tracks_manager_across_expiry_and_refresh() {
        use echo_agent_sampler::BearerResolver;
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(
            &dir,
            Some(make_auth("hard-expired-token", ChronoDuration::hours(-1))),
        );
        let resolver = WireValidBearerResolver(mgr.clone());
        assert_eq!(
            resolver.current_bearer(),
            None,
            "a hard-expired token must never ride the wire"
        );
        mgr.hot_swap(make_auth("buffer-window-token", ChronoDuration::minutes(4)));
        assert_eq!(
            resolver.current_bearer().as_deref(),
            Some("buffer-window-token"),
            "inside the early-invalidation buffer the token is still wire-valid"
        );
        mgr.hot_swap(make_auth("fresh-token", ChronoDuration::hours(1)));
        assert_eq!(
            resolver.current_bearer().as_deref(),
            Some("fresh-token"),
            "the same resolver must serve the rotated token without a rebuild"
        );
    }
    /// `apply()` and `snapshot()` agree (snapshot==wire invariant) when the
    /// in-memory token is fresh.
    #[test]
    fn apply_and_snapshot_agree_on_live_token() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(
            &dir,
            Some(make_auth("live-token", ChronoDuration::hours(1))),
        );
        let provider = ShellAuthCredentialProvider::new(mgr, None, None);
        let snap = provider.snapshot();
        assert_eq!(snap.token.as_deref(), Some("live-token"));
        assert_eq!(snap.user_id.as_deref(), Some("test-user"));
    }
    /// During the 5-minute pre-refresh buffer window, `auth_manager.current()`
    /// returns `None` (the token is treated as expired-soon for refresh
    /// scheduling), but the token is still valid at the proxy. The provider
    /// must fall back to `expired_auth()` so the in-memory token gets sent
    /// instead of nothing -- which is the fix for the bulk of the
    /// `POST /v1/storage` 401s observed in production.
    #[test]
    fn falls_back_to_expired_auth_during_buffer_window() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(
            &dir,
            Some(make_auth("buffer-token", ChronoDuration::minutes(4))),
        );
        assert!(mgr.current().is_none(), "buffer-window precondition");
        assert!(mgr.expired_auth().is_some(), "buffer-window precondition");
        let provider = ShellAuthCredentialProvider::new(mgr, None, None);
        let snap = provider.snapshot();
        assert_eq!(
            snap.token.as_deref(),
            Some("buffer-token"),
            "snapshot should fall back to expired_auth instead of None"
        );
        assert_eq!(snap.user_id.as_deref(), Some("test-user"));
    }
    /// When `auth_manager` has nothing at all (no in-memory auth, expired
    /// or otherwise), `snapshot()` returns `None` for the user-token branch.
    /// `apply()` would then send no Authorization header.
    #[test]
    fn no_token_when_auth_manager_is_empty() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(&dir, None);
        let provider = ShellAuthCredentialProvider::new(mgr, None, None);
        let snap = provider.snapshot();
        assert!(
            snap.token.is_none(),
            "snapshot should be None when manager has no auth"
        );
        assert!(snap.user_id.is_none());
    }
    /// 401 recovery routes through `unauthorized_recovery` (pre-fix
    /// it no-oped because the refresher arg was hardcoded `None`).
    #[tokio::test]
    async fn refresh_after_unauthorized_drives_recovery_state_machine() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = Arc::new(AuthManager::new(
            dir.path(),
            crate::auth::EchoAgentComConfig::default(),
        ));
        mgr.hot_swap(EchoAgentAuth {
            key: "stale".into(),
            auth_mode: crate::auth::AuthMode::Oidc,
            create_time: chrono::Utc::now() - ChronoDuration::hours(2),
            user_id: "u".into(),
            refresh_token: Some("rt-stale".into()),
            expires_at: Some(chrono::Utc::now() - ChronoDuration::hours(1)),
            ..EchoAgentAuth::test_default()
        });
        struct OkRefresher {
            calls: Arc<std::sync::atomic::AtomicU32>,
        }
        #[async_trait::async_trait]
        impl crate::auth::refresh::TokenRefresher for OkRefresher {
            async fn refresh(
                &self,
                _r: crate::auth::manager::RefreshReason,
            ) -> crate::auth::refresh::RefreshOutcome {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                crate::auth::refresh::RefreshOutcome::Success(Box::new(EchoAgentAuth {
                    key: "fresh".into(),
                    auth_mode: crate::auth::AuthMode::Oidc,
                    create_time: chrono::Utc::now(),
                    user_id: "u".into(),
                    refresh_token: Some("rt-new".into()),
                    expires_at: Some(chrono::Utc::now() + ChronoDuration::hours(1)),
                    ..EchoAgentAuth::test_default()
                }))
            }
        }
        let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        mgr.set_refresher(Arc::new(OkRefresher {
            calls: calls.clone(),
        }));
        let provider = ShellAuthCredentialProvider::new(mgr.clone(), None, None);
        assert!(provider.refresh_after_unauthorized().await);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(mgr.current().unwrap().key, "fresh");
        assert_eq!(
            provider.snapshot().token.as_deref(),
            Some("fresh"),
            "snapshot must reflect refreshed token for subsequent apply() calls"
        );
    }
    #[test]
    fn embedding_session_credentials_scopes_to_first_party() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(
            &dir,
            Some(make_auth(
                "echo-agent-session-token",
                ChronoDuration::hours(1),
            )),
        );
        let api_key_provider: echo_agent_tools::types::SharedApiKeyProvider =
            Arc::new(crate::auth::manager::SharedAuthKeyProvider(mgr.clone()));
        for denied in [
            "https://byok.attacker.example/v1",
            "http://api.echo-agent.invalid/v1",
        ] {
            let resolved =
                embedding_session_credentials(denied, Some(&mgr), Some(api_key_provider.clone()));
            assert!(
                resolved.is_empty(),
                "session credentials must not reach {denied}"
            );
        }
        let resolved = embedding_session_credentials(
            "https://api.echo-agent.invalid/v1",
            Some(&mgr),
            Some(api_key_provider),
        );
        assert!(!resolved.is_empty());
    }
    /// Deployment-key path has no recovery (operator owns the bearer).
    #[tokio::test]
    async fn refresh_after_unauthorized_is_noop_for_deployment_key() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(&dir, None);
        let provider =
            ShellAuthCredentialProvider::new(mgr, Some("deployment-key".to_string()), None);
        assert!(!provider.refresh_after_unauthorized().await);
    }
    #[test]
    fn snapshot_populates_tenant_id_per_auth_mode() {
        use echo_agent_telemetry::config::deployment_id_from_key;
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let dep = ShellAuthCredentialProvider::new(
            make_manager(&dir, None),
            Some("echo-agent-token-EX".into()),
            None,
        )
        .snapshot();
        assert_eq!(
            dep.deployment_id.as_deref(),
            Some(deployment_id_from_key("echo-agent-token-EX").as_str())
        );
        assert!(dep.api_key_id.is_none());
        let api_auth = EchoAgentAuth {
            key: "sk-apikey-xyz".into(),
            auth_mode: crate::auth::AuthMode::ApiKey,
            expires_at: Some(Utc::now() + ChronoDuration::hours(1)),
            ..EchoAgentAuth::test_default()
        };
        let api = ShellAuthCredentialProvider::new(make_manager(&dir, Some(api_auth)), None, None)
            .snapshot();
        assert_eq!(
            api.api_key_id.as_deref(),
            Some(deployment_id_from_key("sk-apikey-xyz").as_str())
        );
        assert!(api.deployment_id.is_none());
        let oidc = ShellAuthCredentialProvider::new(
            make_manager(
                &dir,
                Some(make_auth("oidc-token", ChronoDuration::hours(1))),
            ),
            None,
            None,
        )
        .snapshot();
        assert!(oidc.deployment_id.is_none() && oidc.api_key_id.is_none());
    }
    /// Bootstrap mode: `snapshot()` re-reads disk so sibling-rotated
    /// tokens are picked up without a live AuthManager.
    #[test]
    fn otel_bootstrap_snapshot_picks_up_disk_writes() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let scope = crate::auth::EchoAgentComConfig::default().auth_scope();
        let auth_path = dir.path().join("auth.json");
        let mgr = make_manager(
            &dir,
            Some(make_auth("initial-token", ChronoDuration::hours(1))),
        );
        let mut store = crate::auth::read_auth_json(&auth_path).unwrap_or_default();
        store.insert(
            scope.clone(),
            make_auth("initial-token", ChronoDuration::hours(1)),
        );
        crate::auth::storage::write_auth_json(&auth_path, &store).unwrap();
        let provider = OtelAuthCredentialProvider::new(mgr);
        assert_eq!(provider.snapshot().token.as_deref(), Some("initial-token"));
        let mut store = crate::auth::read_auth_json(&auth_path).unwrap();
        store.insert(scope, make_auth("rotated-token", ChronoDuration::hours(1)));
        crate::auth::storage::write_auth_json(&auth_path, &store).unwrap();
        assert_eq!(
            provider.snapshot().token.as_deref(),
            Some("rotated-token"),
            "must pick up sibling-rotated tokens from disk"
        );
    }
    /// After `set_live()`, `snapshot()` reads from the live manager's
    /// in-memory cache (no disk re-read) and `refresh_after_unauthorized()`
    /// drives the recovery state machine.
    #[tokio::test]
    async fn otel_live_mode_uses_shared_auth_manager() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let bootstrap_dir = tempfile::tempdir().unwrap();
        let bootstrap_mgr = make_manager(&bootstrap_dir, None);
        let provider = OtelAuthCredentialProvider::new(bootstrap_mgr);
        assert!(provider.snapshot().token.is_none());
        assert!(!provider.refresh_after_unauthorized().await);
        let live_dir = tempfile::tempdir().unwrap();
        let live_mgr = make_manager(
            &live_dir,
            Some(make_auth("live-token", ChronoDuration::hours(1))),
        );
        provider.set_live(live_mgr.clone());
        assert_eq!(
            provider.snapshot().token.as_deref(),
            Some("live-token"),
            "must read from live AuthManager after set_live()"
        );
        live_mgr.hot_swap(make_auth("rotated-live", ChronoDuration::hours(1)));
        assert_eq!(
            provider.snapshot().token.as_deref(),
            Some("rotated-live"),
            "must see rotated token from live manager"
        );
    }
    /// `refresh_after_unauthorized` drives recovery when live.
    #[tokio::test]
    async fn otel_live_refresh_after_unauthorized_drives_recovery() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let bootstrap_dir = tempfile::tempdir().unwrap();
        let bootstrap_mgr = make_manager(&bootstrap_dir, None);
        let provider = OtelAuthCredentialProvider::new(bootstrap_mgr);
        let live_dir = tempfile::tempdir().unwrap();
        let live_mgr = Arc::new(AuthManager::new(
            live_dir.path(),
            crate::auth::EchoAgentComConfig::default(),
        ));
        live_mgr.hot_swap(EchoAgentAuth {
            key: "stale".into(),
            auth_mode: crate::auth::AuthMode::Oidc,
            create_time: chrono::Utc::now() - ChronoDuration::hours(2),
            user_id: "u".into(),
            refresh_token: Some("rt-stale".into()),
            expires_at: Some(chrono::Utc::now() - ChronoDuration::hours(1)),
            ..EchoAgentAuth::test_default()
        });
        struct OkRefresher;
        #[async_trait::async_trait]
        impl crate::auth::refresh::TokenRefresher for OkRefresher {
            async fn refresh(
                &self,
                _r: crate::auth::manager::RefreshReason,
            ) -> crate::auth::refresh::RefreshOutcome {
                crate::auth::refresh::RefreshOutcome::Success(Box::new(EchoAgentAuth {
                    key: "refreshed".into(),
                    auth_mode: crate::auth::AuthMode::Oidc,
                    create_time: chrono::Utc::now(),
                    user_id: "u".into(),
                    refresh_token: Some("rt-new".into()),
                    expires_at: Some(chrono::Utc::now() + ChronoDuration::hours(1)),
                    ..EchoAgentAuth::test_default()
                }))
            }
        }
        live_mgr.set_refresher(Arc::new(OkRefresher));
        provider.set_live(live_mgr.clone());
        assert!(
            provider.refresh_after_unauthorized().await,
            "live mode must drive recovery"
        );
        assert_eq!(live_mgr.current().unwrap().key, "refreshed");
    }
    #[test]
    fn otel_deployment_key_sent_when_no_oidc_token() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(&dir, None);
        let provider = OtelAuthCredentialProvider::new(mgr);
        provider.set_deployment_key("enterprise-key".to_string());
        let snap = provider.snapshot();
        assert_eq!(
            snap.token.as_deref(),
            Some("enterprise-key"),
            "deployment key must be sent when no OIDC token exists"
        );
    }
    #[test]
    fn otel_deployment_key_wins_over_oidc_token() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(
            &dir,
            Some(make_auth("oidc-token", ChronoDuration::hours(1))),
        );
        let provider = OtelAuthCredentialProvider::new(mgr);
        provider.set_deployment_key("deployment-key-123".to_string());
        let snap = provider.snapshot();
        assert_eq!(
            snap.token.as_deref(),
            Some("deployment-key-123"),
            "deployment key must win over OIDC token"
        );
        assert!(snap.user_id.is_none());
    }
    #[test]
    fn has_usable_credential_reflects_auth_state() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(&dir, Some(make_auth("live", ChronoDuration::hours(1))));
        let provider = OtelAuthCredentialProvider::new(mgr);
        assert!(
            provider.has_usable_credential(),
            "valid unexpired token is usable"
        );
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(&dir, Some(make_auth("stale", ChronoDuration::hours(-1))));
        let provider = OtelAuthCredentialProvider::new(mgr);
        assert!(
            !provider.has_usable_credential(),
            "expired token is not usable"
        );
        let dir = tempfile::tempdir().unwrap();
        let provider = OtelAuthCredentialProvider::new(make_manager(&dir, None));
        assert!(
            !provider.has_usable_credential(),
            "absent token is not usable"
        );
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(&dir, Some(make_auth("live", ChronoDuration::hours(1))));
        mgr.record_permanent_failure(
            "live".into(),
            crate::auth::error::RefreshTokenFailedReason::RefreshTokenRejected.into(),
        );
        let provider = OtelAuthCredentialProvider::new(mgr);
        assert!(
            provider.has_usable_credential(),
            "a wire-valid token stays usable despite a permanent refresh verdict"
        );
        let dir = tempfile::tempdir().unwrap();
        let provider = OtelAuthCredentialProvider::new(make_manager(&dir, None));
        provider.set_deployment_key("enterprise-key".to_string());
        assert!(
            provider.has_usable_credential(),
            "static deployment key is always usable"
        );
    }
    /// A token inside the early-invalidation buffer is still accepted by the
    /// proxy (the buffer is a client-side pre-refresh margin, not a wire
    /// expiry), and the sender puts it on the wire via `current_or_expired()`.
    /// The export gate must therefore keep it usable even though `current()`
    /// reports `None`. Regression for the buffer-window export drop.
    #[test]
    fn has_usable_credential_true_inside_early_invalidation_buffer() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(
            &dir,
            Some(make_auth("buffered", ChronoDuration::minutes(4))),
        );
        assert!(
            mgr.current().is_none(),
            "4-min token sits inside the pinned 5-min buffer, so current() is None"
        );
        let provider = OtelAuthCredentialProvider::new(mgr);
        assert!(
            provider.has_usable_credential(),
            "a buffer-window token is still wire-valid, so the gate keeps it usable"
        );
    }
    /// A configured `deployment_key` always wins over the AuthManager-resolved
    /// user token, matching the precedence in `EchoAgentAuthCredentials::apply`.
    /// The snapshot must report the deployment key (not the user token) so
    /// the 401-attribution prefix matches the wire bytes.
    #[test]
    fn deployment_key_wins_over_resolved_user_token() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(
            &dir,
            Some(make_auth("user-token", ChronoDuration::hours(1))),
        );
        let provider =
            ShellAuthCredentialProvider::new(mgr, Some("deployment-key-12345".to_string()), None);
        let snap = provider.snapshot();
        assert_eq!(snap.token.as_deref(), Some("deployment-key-12345"));
        assert!(snap.user_id.is_none());
    }
}
