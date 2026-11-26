pub(crate) mod environment;
use crate::telemetry::dc_log;
use echo_agent_auth::{AuthCredentialProvider, CredentialSnapshot};
use echo_agent_computer_hub_sdk::auth::{AuthCredential, AuthProvider};
use echo_agent_file_utils::gcs::StorageConfig;
use echo_agent_file_utils::queue::{EnqueueOutcome, TraceExportSource, UploadQueue};
use echo_agent_file_utils::storage_client::Auth401AttributionCallback;
use echo_agent_file_utils::{TraceExportConfig, UploadMethod};
use environment::WorkspaceIdentity;
use prometheus::{IntCounterVec, IntGauge, register_int_counter_vec, register_int_gauge};
use std::sync::Arc;
use std::sync::LazyLock;
/// `…_pending_bytes` is the series the mandatory queue-memory alert fires on.
static UPLOAD_QUEUE_PENDING_BYTES: LazyLock<IntGauge> = LazyLock::new(|| {
    register_int_gauge!(
        "echo_agent_workspace_upload_queue_pending_bytes",
        "Bytes spilled to the upload queue and not yet uploaded"
    )
    .unwrap()
});
static UPLOAD_QUEUE_PENDING: LazyLock<IntGauge> = LazyLock::new(|| {
    register_int_gauge!(
        "echo_agent_workspace_upload_queue_pending",
        "Items in the upload queue not yet uploaded"
    )
    .unwrap()
});
/// Per-phase terminal upload outcome: `succeeded` (bytes accepted, not
/// GCS-confirmed) / `failed` / `skipped`.
static UPLOAD_OUTCOME_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        "echo_agent_workspace_upload_outcome_total",
        "Workspace upload terminal outcomes, by phase and outcome",
        &["phase", "outcome"]
    )
    .unwrap()
});
/// Per-phase upload failures, by error category
/// (`archive_failed` / `enqueue_failed` / `upload_failed`).
static UPLOAD_FAILED_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        "echo_agent_workspace_upload_failed_total",
        "Workspace upload failures, by phase and error category",
        &["phase", "error_category"]
    )
    .unwrap()
});
/// Per-phase deliberate upload skips (policy / missing-config only). Failure
/// declines are counted as failures, not here.
static UPLOAD_SKIPPED_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        "echo_agent_workspace_upload_skipped_total",
        "Workspace uploads deliberately skipped, by phase and skip reason",
        &["phase", "skip_reason"]
    )
    .unwrap()
});
/// Record a terminal upload outcome; call sites pair it with the matching
/// [`record_upload_failed`] / [`record_upload_skipped`] when one applies.
pub(crate) fn record_upload_outcome(phase: &str, outcome: &str) {
    UPLOAD_OUTCOME_TOTAL
        .with_label_values(&[phase, outcome])
        .inc();
}
/// Record an upload failure, by error category.
pub(crate) fn record_upload_failed(phase: &str, error_category: &str) {
    UPLOAD_FAILED_TOTAL
        .with_label_values(&[phase, error_category])
        .inc();
}
/// Record a deliberate upload skip, by skip reason.
pub(crate) fn record_upload_skipped(phase: &str, skip_reason: &str) {
    UPLOAD_SKIPPED_TOTAL
        .with_label_values(&[phase, skip_reason])
        .inc();
}
/// Zero-init this module's metric families. See [`crate::init_metrics`].
pub(crate) fn init_metrics() {
    UPLOAD_QUEUE_PENDING_BYTES.set(UPLOAD_QUEUE_PENDING_BYTES.get());
    UPLOAD_QUEUE_PENDING.set(UPLOAD_QUEUE_PENDING.get());
    for outcome in ["succeeded", "failed", "skipped"] {
        UPLOAD_OUTCOME_TOTAL
            .with_label_values(&["tool_state", outcome])
            .inc_by(0);
    }
    UPLOAD_FAILED_TOTAL
        .with_label_values(&["tool_state", "enqueue_failed"])
        .inc_by(0);
    for reason in ["no_upload_queue", "no_session"] {
        UPLOAD_SKIPPED_TOTAL
            .with_label_values(&["tool_state", reason])
            .inc_by(0);
    }
    for outcome in ["succeeded", "failed"] {
        UPLOAD_OUTCOME_TOTAL
            .with_label_values(&["workspace_environment", outcome])
            .inc_by(0);
    }
    UPLOAD_FAILED_TOTAL
        .with_label_values(&["workspace_environment", "enqueue_failed"])
        .inc_by(0);
}
/// Spawn a detached sampler that mirrors the queue's pending/pending-bytes
/// stats into the Prometheus gauges every `interval`, and emits a matching
/// queue-aggregate telemetry snapshot so queue pressure is visible in the
/// same log stream as upload outcomes.
pub(crate) fn spawn_queue_stats_sampler(
    queue: Arc<UploadQueue>,
    interval: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    let stats = queue.stats_arc();
    let sample_period_secs = interval.as_secs();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let pending_bytes = stats
                .pending_bytes
                .load(std::sync::atomic::Ordering::Relaxed);
            let pending = stats.pending.load(std::sync::atomic::Ordering::Relaxed);
            UPLOAD_QUEUE_PENDING_BYTES.set(pending_bytes as i64);
            UPLOAD_QUEUE_PENDING.set(pending as i64);
            dc_log!(
                info,
                pending,
                pending_bytes,
                sample_period_secs,
                "workspace: upload queue pending stats"
            );
        }
    })
}
/// Wraps the server [`AuthProvider`] as an [`AuthCredentialProvider`] +
/// [`HttpAuth`] so the `StorageClient` can authenticate requests.
struct HubAuthCredentialProvider {
    auth: Arc<dyn AuthProvider>,
    /// Resolved workspace owner so `snapshot` can attribute uploads (and 401s)
    /// to the real `user_id`/`team_id`.
    identity: WorkspaceIdentity,
}
impl echo_agent_auth::visibility::HttpAuth for HubAuthCredentialProvider {
    fn apply(&self, builder: reqwest::RequestBuilder, _base_url: &str) -> reqwest::RequestBuilder {
        let cred = self.auth.current();
        match &cred {
            AuthCredential::Bearer { token, .. } => {
                builder.header("Authorization", format!("Bearer {token}"))
            }
            AuthCredential::Headers { headers, .. } => {
                let mut b = builder;
                for (name, value) in headers {
                    b = b.header(name.as_str(), value.as_str());
                }
                b
            }
        }
    }
}
#[async_trait::async_trait]
impl AuthCredentialProvider for HubAuthCredentialProvider {
    fn snapshot(&self) -> CredentialSnapshot {
        let user_id = self.identity.user_id_opt();
        let team_id = self.identity.team_id();
        let cred = self.auth.current();
        match &cred {
            AuthCredential::Bearer { token } => CredentialSnapshot {
                token: Some(token.clone()),
                user_id,
                team_id,
                ..Default::default()
            },
            AuthCredential::Headers { .. } => CredentialSnapshot {
                user_id,
                team_id,
                ..Default::default()
            },
        }
    }
    async fn refresh_after_unauthorized(&self) -> bool {
        false
    }
}
/// [`StorageConfig`] implementation that proxies uploads through the
/// configured proxy endpoint using the connection's auth credentials.
pub(crate) struct ProxyStorageConfig {
    method: UploadMethod,
    credentials: Arc<dyn AuthCredentialProvider>,
}
impl ProxyStorageConfig {
    pub(crate) fn new(
        auth: Arc<dyn AuthProvider>,
        api_base_url: String,
        identity: WorkspaceIdentity,
    ) -> Self {
        let credentials: Arc<dyn AuthCredentialProvider> =
            Arc::new(HubAuthCredentialProvider { auth, identity });
        let method = UploadMethod::Proxy {
            proxy_base_url: api_base_url,
            user_token: "workspace-upload".to_string(),
            deployment_key: None,
            alpha_test_key: None,
        };
        Self {
            method,
            credentials,
        }
    }
}
impl StorageConfig for ProxyStorageConfig {
    fn bucket_url(&self) -> &str {
        "gs://placeholder"
    }
    fn upload_method(&self) -> &UploadMethod {
        &self.method
    }
    fn proxy_credentials(&self) -> Option<Arc<dyn AuthCredentialProvider>> {
        Some(self.credentials.clone())
    }
}
/// Adapts the workspace's [`ProxyStorageConfig`] to the upload queue's
/// [`TraceExportSource`] contract so [`UploadQueue`] can resolve fresh proxy
/// credentials on every upload attempt.
///
/// `resolve` builds a [`TraceExportConfig`] from the proxy config's
/// `bucket_url` + `upload_method`; the auth / attribution / http-client hooks
/// delegate straight through to the wrapped [`ProxyStorageConfig`] (whose
/// `proxy_credentials` is a [`HubAuthCredentialProvider`] over the server's
/// `AuthProvider`).
pub(crate) struct WorkspaceTraceExportSource {
    proxy_storage_config: Arc<ProxyStorageConfig>,
}
impl WorkspaceTraceExportSource {
    pub(crate) fn new(proxy_storage_config: Arc<ProxyStorageConfig>) -> Self {
        Self {
            proxy_storage_config,
        }
    }
}
impl TraceExportSource for WorkspaceTraceExportSource {
    fn resolve(&self) -> TraceExportConfig {
        TraceExportConfig {
            bucket_url: Some(self.proxy_storage_config.bucket_url().to_string()),
            service_account_key: None,
            upload_method: self.proxy_storage_config.upload_method().clone(),
            prefix_dir: None,
            gcs_prefix: None,
            absolute_paths: false,
            archive_name_override: None,
        }
    }
    fn proxy_attribution(&self) -> Option<Arc<dyn Auth401AttributionCallback>> {
        self.proxy_storage_config.proxy_attribution()
    }
    fn proxy_credentials(&self) -> Option<Arc<dyn AuthCredentialProvider>> {
        self.proxy_storage_config.proxy_credentials()
    }
    fn proxy_http_client(&self) -> Option<reqwest::Client> {
        self.proxy_storage_config.proxy_http_client()
    }
}
/// Enqueue the flushed tool-state bytes at
/// `"{session_id}/turn_{turn_number}/tool_state.json"`. The local spill file is
/// named `resources_state.json`, but the durable artifact is always
/// `tool_state.json` to match the environment naming scheme.
/// `Enqueued`/`FellBackToInline` are success; `Failed` is an error.
pub(crate) async fn upload_tool_state_queued(
    state_bytes: Vec<u8>,
    session_id: String,
    turn_number: u64,
    upload_queue: Arc<UploadQueue>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let object_path = format!("{session_id}/turn_{turn_number}/tool_state.json");
    let bytes_len = state_bytes.len();
    match upload_queue
        .enqueue_bytes_blocking(
            &state_bytes,
            &object_path,
            "application/json",
            "tool_state",
            &session_id,
            turn_number,
        )
        .await
    {
        EnqueueOutcome::Enqueued => {
            dc_log!(
                info,
                session_id = %session_id,
                turn_number,
                bytes = bytes_len,
                "workspace: tool_state upload enqueued"
            );
            record_upload_outcome("tool_state", "succeeded");
            Ok(())
        }
        EnqueueOutcome::FellBackToInline => {
            dc_log!(
                info,
                session_id = %session_id,
                turn_number,
                bytes = bytes_len,
                "workspace: tool_state upload fell back to inline"
            );
            record_upload_outcome("tool_state", "succeeded");
            Ok(())
        }
        EnqueueOutcome::Deduplicated => {
            dc_log!(
                info,
                session_id = %session_id,
                turn_number,
                "workspace: tool_state upload deduplicated, identical upload already in flight"
            );
            record_upload_outcome("tool_state", "succeeded");
            Ok(())
        }
        EnqueueOutcome::Skipped { reason } => {
            dc_log!(
                info,
                session_id = %session_id,
                turn_number,
                skip_reason = reason.as_str(),
                "workspace: tool_state upload skipped"
            );
            record_upload_skipped("tool_state", &reason);
            record_upload_outcome("tool_state", "skipped");
            Ok(())
        }
        EnqueueOutcome::Failed { reason } => Err(reason.into()),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_computer_hub_sdk::auth::AuthCredential;
    fn proxy_config() -> Arc<ProxyStorageConfig> {
        proxy_config_with_identity(WorkspaceIdentity::default())
    }
    fn proxy_config_with_identity(identity: WorkspaceIdentity) -> Arc<ProxyStorageConfig> {
        let auth: Arc<dyn AuthProvider> = Arc::new(AuthCredential::bearer("test-token"));
        Arc::new(ProxyStorageConfig::new(
            auth,
            "https://proxy.example/v1".to_string(),
            identity,
        ))
    }
    /// A Team principal's snapshot must carry the real `user_id` and the
    /// `team_id` (from `principal_id`).
    #[test]
    fn snapshot_carries_team_identity() {
        let identity = WorkspaceIdentity::new(
            "user-team-1",
            Some("Team".to_string()),
            Some("team-9".to_string()),
        );
        let cfg = proxy_config_with_identity(identity);
        let snap = cfg
            .proxy_credentials()
            .expect("proxy_credentials must be Some")
            .snapshot();
        assert_eq!(snap.token.as_deref(), Some("test-token"));
        assert_eq!(snap.user_id.as_deref(), Some("user-team-1"));
        assert_eq!(snap.team_id.as_deref(), Some("team-9"));
    }
    /// A User principal's snapshot carries `user_id` but never a `team_id`,
    /// even though the same code path runs.
    #[test]
    fn snapshot_user_identity_has_no_team_id() {
        let identity = WorkspaceIdentity::new("user-solo", Some("User".to_string()), None);
        let cfg = proxy_config_with_identity(identity);
        let snap = cfg
            .proxy_credentials()
            .expect("proxy_credentials must be Some")
            .snapshot();
        assert_eq!(snap.user_id.as_deref(), Some("user-solo"));
        assert_eq!(snap.team_id, None);
    }
    /// With no resolved identity (headless / local-dev), `user_id` and
    /// `team_id` are `None` but the live bearer token still flows.
    #[test]
    fn snapshot_default_identity_omits_user_and_team() {
        let snap = proxy_config()
            .proxy_credentials()
            .expect("proxy_credentials must be Some")
            .snapshot();
        assert_eq!(snap.token.as_deref(), Some("test-token"));
        assert_eq!(snap.user_id, None);
        assert_eq!(snap.team_id, None);
    }
    /// The `Headers` credential arm must surface the real `user_id` / `team_id`
    /// too. It has no bearer token, so `token` stays `None`.
    #[test]
    fn snapshot_headers_credential_carries_identity() {
        let identity = WorkspaceIdentity::new(
            "user-headers",
            Some("Team".to_string()),
            Some("team-h".to_string()),
        );
        let auth: Arc<dyn AuthProvider> =
            Arc::new(AuthCredential::headers([("x-api-key", "secret")]).expect("headers cred"));
        let provider = HubAuthCredentialProvider { auth, identity };
        let snap = provider.snapshot();
        assert_eq!(snap.token, None, "Headers arm carries no bearer token");
        assert_eq!(snap.user_id.as_deref(), Some("user-headers"));
        assert_eq!(snap.team_id.as_deref(), Some("team-h"));
    }
    /// `WorkspaceTraceExportSource` must delegate all four `TraceExportSource`
    /// hooks to the wrapped `ProxyStorageConfig`.
    #[tokio::test]
    async fn workspace_trace_export_source_delegates_all_methods() {
        let source = WorkspaceTraceExportSource::new(proxy_config());
        let cfg = source.resolve();
        assert_eq!(cfg.bucket_url.as_deref(), Some("gs://placeholder"));
        assert!(
            matches!(
                &cfg.upload_method,
                UploadMethod::Proxy { proxy_base_url, .. } if proxy_base_url == "https://proxy.example/v1"
            ),
            "resolve() must carry the proxy upload method + base url"
        );
        let cfg_async = source.resolve_async().await;
        assert_eq!(cfg_async.bucket_url.as_deref(), Some("gs://placeholder"));
        assert!(
            source.proxy_credentials().is_some(),
            "proxy_credentials() must delegate the server credential provider"
        );
        assert!(source.proxy_attribution().is_none());
        assert!(source.proxy_http_client().is_none());
    }
    /// The credential the queue resolves must be the server-backed provider whose
    /// snapshot carries the live bearer token (not the placeholder baked into
    /// `UploadMethod::Proxy`).
    #[test]
    fn workspace_trace_export_source_credentials_snapshot_live_token() {
        let source = WorkspaceTraceExportSource::new(proxy_config());
        let creds = source
            .proxy_credentials()
            .expect("proxy_credentials must be Some");
        assert_eq!(creds.snapshot().token.as_deref(), Some("test-token"));
    }
    use std::path::Path;
    use tempfile::TempDir;
    /// Spawn a real [`UploadQueue`] spilling under `home`. The proxy points at a
    /// dead local port so any background cloud upload fails fast without DNS —
    /// the tests only assert the *enqueue* side (`stats().enqueued`), never the
    /// upload itself.
    fn test_queue(home: &Path) -> Arc<UploadQueue> {
        let auth: Arc<dyn AuthProvider> = Arc::new(AuthCredential::bearer("test-token"));
        let proxy = Arc::new(ProxyStorageConfig::new(
            auth,
            "http://127.0.0.1:1/v1".to_string(),
            WorkspaceIdentity::default(),
        ));
        let source: Arc<dyn TraceExportSource> = Arc::new(WorkspaceTraceExportSource::new(proxy));
        Arc::new(UploadQueue::spawn(
            home,
            source,
            echo_agent_file_utils::queue::UploadRetryPolicy::default(),
        ))
    }
    /// Pins the tool-state path contract: bytes enqueued at exactly
    /// `{session_id}/turn_{N}/tool_state.json` with JSON content-type and the
    /// `tool_state` artifact name (asserted via queue stat + sidecar manifest).
    #[tokio::test]
    async fn tool_state_enqueues_at_session_turn_gcs_path() {
        use echo_agent_file_utils::queue::{
            QueueItemSidecar, SIDECAR_SUFFIX, UploadQueue, UploadRetryPolicy,
        };
        let home = tempfile::TempDir::new().unwrap();
        let source: Arc<dyn TraceExportSource> =
            Arc::new(WorkspaceTraceExportSource::new(proxy_config()));
        let policy = UploadRetryPolicy {
            initial_delay: std::time::Duration::from_secs(3600),
            ..UploadRetryPolicy::default()
        };
        let queue = Arc::new(UploadQueue::spawn(home.path(), source, policy));
        let enqueued_before = queue
            .stats()
            .enqueued
            .load(std::sync::atomic::Ordering::Relaxed);
        upload_tool_state_queued(
            br#"{"state":{}}"#.to_vec(),
            "sess-XYZ".to_string(),
            7,
            queue.clone(),
        )
        .await
        .expect("tool_state enqueue should succeed");
        assert_eq!(
            queue
                .stats()
                .enqueued
                .load(std::sync::atomic::Ordering::Relaxed),
            enqueued_before + 1,
            "the tool_state item must enter the queue"
        );
        let queue_dir = home.path().join("upload_queue");
        let sidecar_path = std::fs::read_dir(&queue_dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(SIDECAR_SUFFIX))
            })
            .expect("a sidecar manifest must exist after enqueue");
        let sidecar: QueueItemSidecar =
            serde_json::from_slice(&std::fs::read(&sidecar_path).unwrap()).unwrap();
        assert_eq!(sidecar.gcs_path, "sess-XYZ/turn_7/tool_state.json");
        assert_eq!(sidecar.artifact_name, "tool_state");
        assert_eq!(sidecar.content_type, "application/json");
        assert_eq!(sidecar.session_id, "sess-XYZ");
        assert_eq!(sidecar.turn_number, 7);
    }
    /// The closed field vocabulary; only fields in this set are emitted
    /// (never the free-form `reason`/`error`/`*_path`).
    const APPROVED_DC_FIELDS: &[&str] = &[
        "session_id",
        "turn_number",
        "phase",
        "bytes",
        "file_count",
        "pending",
        "pending_bytes",
        "sample_period_secs",
        "error_category",
        "outcome",
        "skip_reason",
        "drain_reason",
        "grace_ms",
        "active_at_start",
        "pending_at_start",
        "producers_at_start",
    ];
    #[derive(Clone)]
    struct CapturedEvent {
        level: tracing::Level,
        target: String,
        message: String,
        fields: Vec<String>,
    }
    #[derive(Default)]
    struct FieldVisitor {
        message: String,
        fields: Vec<String>,
    }
    impl tracing::field::Visit for FieldVisitor {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.message = format!("{value:?}");
            } else {
                self.fields.push(field.name().to_string());
            }
        }
    }
    #[derive(Clone)]
    struct CaptureLayer {
        events: Arc<std::sync::Mutex<Vec<CapturedEvent>>>,
    }
    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CaptureLayer {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut v = FieldVisitor::default();
            event.record(&mut v);
            let meta = event.metadata();
            self.events.lock().unwrap().push(CapturedEvent {
                level: *meta.level(),
                target: meta.target().to_string(),
                message: v.message,
                fields: v.fields,
            });
        }
    }
    /// Run `f` with a thread-local capturing subscriber; returns only the events
    /// on the `workspace::telemetry` target.
    fn capture_dc(f: impl FnOnce()) -> Vec<CapturedEvent> {
        use tracing_subscriber::layer::SubscriberExt;
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let layer = CaptureLayer {
            events: events.clone(),
        };
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, f);
        let out = events.lock().unwrap().clone();
        out.into_iter()
            .filter(|e| e.target == crate::telemetry::TELEMETRY_TARGET)
            .collect()
    }
    /// `dc_log!` pins the target, honors the level, keeps the message a verbatim
    /// literal, and only ever carries the approved field vocabulary.
    #[test]
    fn dc_log_pins_target_level_and_vocabulary() {
        let events = capture_dc(|| {
            dc_log!(
                info,
                session_id = %"s",
                turn_number = 1u64,
                bytes = 5usize,
                "constant info message"
            );
            dc_log!(
                warn,
                session_id = %"s",
                outcome = "skipped",
                skip_reason = "no_upload_queue",
                "constant warn message"
            );
        });
        assert_eq!(events.len(), 2, "both events land on the target");
        assert!(
            events
                .iter()
                .all(|e| e.target == crate::telemetry::TELEMETRY_TARGET)
        );
        assert_eq!(events[0].level, tracing::Level::INFO);
        assert_eq!(events[0].message, "constant info message");
        assert_eq!(events[1].level, tracing::Level::WARN);
        for e in &events {
            for f in &e.fields {
                assert!(
                    APPROVED_DC_FIELDS.contains(&f.as_str()),
                    "field {f:?} is not in the approved field vocabulary"
                );
            }
        }
    }
    /// The net-new queue-stats snapshot is INFO, queue-aggregate (no `session_id`),
    /// and carries exactly the queue counters.
    #[tokio::test]
    async fn queue_stats_sampler_emits_info_snapshot() {
        let home = TempDir::new().unwrap();
        let queue = test_queue(home.path());
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        {
            use tracing_subscriber::layer::SubscriberExt;
            let layer = CaptureLayer {
                events: events.clone(),
            };
            let subscriber = tracing_subscriber::registry().with(layer);
            let _guard = tracing::subscriber::set_default(subscriber);
            let handle = spawn_queue_stats_sampler(queue, std::time::Duration::from_millis(20));
            tokio::time::sleep(std::time::Duration::from_millis(60)).await;
            handle.abort();
        }
        let snaps: Vec<_> = events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| {
                e.target == crate::telemetry::TELEMETRY_TARGET
                    && e.message.contains("upload queue pending stats")
            })
            .cloned()
            .collect();
        assert!(
            !snaps.is_empty(),
            "the sampler must emit at least one snapshot"
        );
        let e = &snaps[0];
        assert_eq!(e.level, tracing::Level::INFO);
        let mut fields = e.fields.clone();
        fields.sort();
        assert_eq!(
            fields,
            vec!["pending", "pending_bytes", "sample_period_secs"],
            "queue-aggregate snapshot carries only the queue counters (no session_id)"
        );
    }
}
