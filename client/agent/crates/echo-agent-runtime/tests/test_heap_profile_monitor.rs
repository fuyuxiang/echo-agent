//! Integration tests: jemalloc heap-profile monitor against mock
//! `/v1/settings` + `/v1/storage` (echo-agent-test-support).
//!
//! Fake hooks inject controllable `stats.resident` crossings. Real
//! `gcs::upload_file` proxy uploads hit the mock storage endpoint.
//!
//! ```bash
//! cargo test -p echo-agent-runtime --test test_heap_profile_monitor
//! ```

use std::path::Path;
use std::sync::Arc;
use std::sync::Once;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use echo_agent_runtime::auth::{AuthManager, AuthMode, EchoAgentAuth, EchoAgentComConfig};
use echo_agent_runtime::heap_profile::{
    self, HeapProfileHooks, HeapProfileMonitor, HeapProfileUploadHandles,
    JemallocHeapProfileConfig, JemallocStats, build_upload_handles, is_valid_session_id,
    object_paths, resolve_jemalloc_heap_profile, sanitize_version,
};
use echo_agent_runtime::session::repo_changes::UploadMethod;
use echo_agent_runtime::util::config::RemoteSettings;
use echo_agent_test_support::{EnvGuard, MockInferenceServer};
use echo_agent_version::TEST_VERSION_ENV;

const SID: &str = "11111111-1111-4111-8111-111111111111";
const TEST_VERSION: &str = "9.9.9-heaptest";
const DUMP_PAYLOAD: u64 = 4096;
const AUTH_TOKEN: &str = "heap-profile-test-bearer";
const AUTH_BEARER: &str = "Bearer heap-profile-test-bearer";

static HOOKS_INIT: Once = Once::new();
static FAKE_ALLOCATED: AtomicU64 = AtomicU64::new(1_000);
static FAKE_RESIDENT: AtomicU64 = AtomicU64::new(0);
static FAKE_PROF_ACTIVE: AtomicBool = AtomicBool::new(false);
static FAKE_DUMP_FAIL: AtomicBool = AtomicBool::new(false);
static FAKE_DUMP_SIZE: AtomicU64 = AtomicU64::new(DUMP_PAYLOAD);
static FAKE_DUMP_COUNT: AtomicU64 = AtomicU64::new(0);

fn fake_stats() -> Option<JemallocStats> {
    Some(JemallocStats {
        allocated: FAKE_ALLOCATED.load(Ordering::Relaxed),
        resident: FAKE_RESIDENT.load(Ordering::Relaxed),
    })
}

fn fake_set_prof_active(active: bool) -> bool {
    FAKE_PROF_ACTIVE.store(active, Ordering::Relaxed);
    true
}

fn fake_dump_to_path(path: &Path) -> Result<(), String> {
    FAKE_DUMP_COUNT.fetch_add(1, Ordering::Relaxed);
    if FAKE_DUMP_FAIL.load(Ordering::Relaxed) {
        return Err("fake dump failed".into());
    }
    let n = FAKE_DUMP_SIZE.load(Ordering::Relaxed) as usize;
    std::fs::write(path, vec![0xABu8; n]).map_err(|e| e.to_string())
}

fn ensure_fake_hooks() {
    HOOKS_INIT.call_once(|| {
        heap_profile::install(HeapProfileHooks {
            stats: fake_stats,
            set_prof_active: fake_set_prof_active,
            dump_to_path: fake_dump_to_path,
            prof_available: || true,
        });
    });
    assert!(
        heap_profile::prof_available(),
        "fake hooks must be installed (OnceLock first-wins)"
    );
}

fn reset_fakes() {
    FAKE_ALLOCATED.store(1_000, Ordering::Relaxed);
    FAKE_RESIDENT.store(0, Ordering::Relaxed);
    FAKE_PROF_ACTIVE.store(false, Ordering::Relaxed);
    FAKE_DUMP_FAIL.store(false, Ordering::Relaxed);
    FAKE_DUMP_SIZE.store(DUMP_PAYLOAD, Ordering::Relaxed);
    FAKE_DUMP_COUNT.store(0, Ordering::Relaxed);
}

fn settings(enabled: bool, thresholds: &[u64]) -> RemoteSettings {
    RemoteSettings {
        jemalloc_heap_profile_enabled: Some(enabled),
        jemalloc_heap_profile_thresholds_bytes: Some(thresholds.to_vec()),
        jemalloc_heap_profile_poll_interval_secs: Some(5),
        trace_upload_enabled: Some(true),
        ..Default::default()
    }
}

async fn fetch_settings(server: &MockInferenceServer) -> RemoteSettings {
    let resp = reqwest::get(format!("{}/settings", server.url()))
        .await
        .expect("GET /v1/settings");
    assert_eq!(resp.status(), 200, "settings endpoint must be configured");
    resp.json().await.expect("parse RemoteSettings")
}

fn resolve_from_settings(settings: &RemoteSettings) -> JemallocHeapProfileConfig {
    resolve_jemalloc_heap_profile(
        settings.jemalloc_heap_profile_enabled,
        settings.jemalloc_heap_profile_thresholds_bytes.as_deref(),
        settings.jemalloc_heap_profile_poll_interval_secs,
        false,
        settings.trace_upload_enabled == Some(true),
        heap_profile::prof_available(),
    )
}

fn seed_auth_json(home: &Path, token: &str) {
    let scope = EchoAgentComConfig::default().auth_scope();
    let auth = EchoAgentAuth {
        key: token.to_owned(),
        auth_mode: AuthMode::ApiKey,
        create_time: Utc::now(),
        user_id: "heap-profile-test".into(),
        expires_at: Some(Utc::now() + ChronoDuration::hours(2)),
        ..Default::default()
    };
    let store = serde_json::json!({ scope: auth });
    std::fs::write(
        home.join("auth.json"),
        serde_json::to_vec(&store).expect("serialize auth.json"),
    )
    .expect("write auth.json");
}

fn proxy_handles(server: &MockInferenceServer, auth: Arc<AuthManager>) -> HeapProfileUploadHandles {
    build_upload_handles(
        auth,
        Some("gs://mock-bucket".to_owned()),
        UploadMethod::Proxy {
            proxy_base_url: server.url(),
            user_token: String::new(),
            deployment_key: None,
            alpha_test_key: None,
        },
    )
}

fn assert_jemalloc_object_pair(sid: &str, version: &str, heap: &str, meta: &str) {
    assert!(is_valid_session_id(sid));
    let ver = sanitize_version(version);
    let prefix = format!("{sid}/jemalloc/{sid}-{ver}-");
    assert!(
        heap.starts_with(&prefix) && heap.ends_with(".heap"),
        "heap path {heap:?} must match {prefix}*.heap"
    );
    assert!(
        meta.starts_with(&prefix) && meta.ends_with(".meta.json"),
        "meta path {meta:?} must match {prefix}*.meta.json"
    );
    let heap_base = heap.trim_end_matches(".heap");
    let meta_base = meta.trim_end_matches(".meta.json");
    assert_eq!(heap_base, meta_base, "heap/meta share the same leaf stem");
    let ts = heap_base
        .rsplit('-')
        .next()
        .expect("ts segment")
        .parse::<u64>()
        .expect("ts is u64");
    let (expected_heap, expected_meta) = object_paths(sid, version, ts);
    assert_eq!(heap, expected_heap);
    assert_eq!(meta, expected_meta);
}

fn assert_storage_auth(uploads: &[echo_agent_test_support::mock_server::StorageUpload]) {
    for u in uploads {
        assert_eq!(
            u.authorization.as_deref(),
            Some(AUTH_BEARER),
            "storage upload {:?} missing live AuthManager bearer",
            u.path
        );
    }
}

fn assert_meta_json(body: &[u8], threshold: u64, resident: u64, allocated: u64) {
    let meta: serde_json::Value = serde_json::from_slice(body).expect("meta.json parses");
    assert_eq!(meta["session_id"], SID);
    assert_eq!(meta["binary_version"], TEST_VERSION);
    assert_eq!(meta["threshold_bytes"], threshold);
    assert_eq!(meta["stats_resident"], resident);
    assert_eq!(meta["stats_allocated"], allocated);
    assert_eq!(meta["lg_prof_sample"], heap_profile::LG_PROF_SAMPLE);
    assert_eq!(meta["os"], std::env::consts::OS);
    assert!(meta["ts_unix"].as_u64().is_some());
    assert!(meta["rss_peak_bytes"].as_u64().is_some());
}

/// Busy-wait past the wall-clock second boundary so successive dumps get
/// distinct ts_unix path segments (a fixed short sleep can still finish in
/// the same second). Capped so a stalled/backward clock fails the suite fast.
async fn wait_for_next_unix_second() {
    const CAP: Duration = Duration::from_secs(3);
    let wait = async {
        let start = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_secs();
        loop {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after epoch");
            if now.as_secs() > start {
                return;
            }
            let rem_ms = now.subsec_millis() as u64;
            let wait_ms = 1000u64.saturating_sub(rem_ms).saturating_add(5).max(5);
            tokio::time::sleep(Duration::from_millis(wait_ms)).await;
        }
    };
    tokio::time::timeout(CAP, wait)
        .await
        .expect("wait_for_next_unix_second: wall clock did not advance within 3s");
}

struct Harness {
    server: MockInferenceServer,
    mon: HeapProfileMonitor,
    auth: Arc<AuthManager>,
    _home: tempfile::TempDir,
    _version: EnvGuard,
}

impl Harness {
    async fn start(remote: RemoteSettings) -> Self {
        ensure_fake_hooks();
        reset_fakes();
        let _version = EnvGuard::set(TEST_VERSION_ENV, TEST_VERSION);
        let server = MockInferenceServer::start()
            .await
            .expect("start MockInferenceServer");
        server.set_settings(&remote);
        let fetched = fetch_settings(&server).await;
        let config = resolve_from_settings(&fetched);
        let home = tempfile::TempDir::new().expect("temp home");
        seed_auth_json(home.path(), AUTH_TOKEN);
        let auth = Arc::new(AuthManager::new(home.path(), EchoAgentComConfig::default()));
        let handles = proxy_handles(&server, Arc::clone(&auth));
        let mut mon = HeapProfileMonitor::new();
        mon.reconfigure(config, Some(handles));
        mon.set_session_id(SID.to_owned());
        Self {
            server,
            mon,
            auth,
            _home: home,
            _version,
        }
    }

    async fn reconfigure_from_server(&mut self) {
        let fetched = fetch_settings(&self.server).await;
        let config = resolve_from_settings(&fetched);
        let handles = proxy_handles(&self.server, Arc::clone(&self.auth));
        self.mon.reconfigure(config, Some(handles));
    }
}

#[tokio::test]
#[serial_test::serial(heap_profile_integration)]
async fn mock_settings_enable_threshold_upload_hits_storage_with_object_paths() {
    let mut h = Harness::start(settings(true, &[1_000])).await;
    assert!(h.mon.config().enabled);
    assert!(FAKE_PROF_ACTIVE.load(Ordering::Relaxed));
    assert_eq!(h.server.storage_request_count(), 0);

    FAKE_RESIDENT.store(2_000, Ordering::Relaxed);
    h.mon.poll_tick().await;

    assert!(h.mon.latched().contains(&1_000));
    assert_eq!(FAKE_DUMP_COUNT.load(Ordering::Relaxed), 1);
    assert_eq!(h.server.storage_request_count(), 2);

    let uploads = h.server.storage_uploads();
    assert_eq!(uploads.len(), 2);
    assert_eq!(uploads[0].size, DUMP_PAYLOAD as usize);
    assert_eq!(uploads[0].body.len(), DUMP_PAYLOAD as usize);
    assert!(uploads[0].body.iter().all(|&b| b == 0xAB));
    assert_jemalloc_object_pair(SID, TEST_VERSION, &uploads[0].path, &uploads[1].path);
    assert_storage_auth(&uploads);
    assert_meta_json(&uploads[1].body, 1_000, 2_000, 1_000);

    h.mon.poll_tick().await;
    assert_eq!(FAKE_DUMP_COUNT.load(Ordering::Relaxed), 1);
    assert_eq!(h.server.storage_request_count(), 2);
}

#[tokio::test]
#[serial_test::serial(heap_profile_integration)]
async fn mock_settings_disable_stops_sampling_and_further_dumps() {
    let mut h = Harness::start(settings(true, &[500, 2_000])).await;
    assert!(FAKE_PROF_ACTIVE.load(Ordering::Relaxed));

    FAKE_RESIDENT.store(600, Ordering::Relaxed);
    h.mon.poll_tick().await;
    assert!(h.mon.latched().contains(&500));
    assert_eq!(h.server.storage_uploads().len(), 2);
    let dumps_after_first = FAKE_DUMP_COUNT.load(Ordering::Relaxed);
    assert_eq!(dumps_after_first, 1);

    h.server.set_settings(settings(false, &[500, 2_000]));
    let fetched = fetch_settings(&h.server).await;
    assert_eq!(fetched.jemalloc_heap_profile_enabled, Some(false));
    h.reconfigure_from_server().await;

    assert!(!h.mon.config().enabled);
    assert!(!FAKE_PROF_ACTIVE.load(Ordering::Relaxed));

    FAKE_RESIDENT.store(10_000, Ordering::Relaxed);
    h.mon.poll_tick().await;
    assert_eq!(FAKE_DUMP_COUNT.load(Ordering::Relaxed), dumps_after_first);
    assert_eq!(h.server.storage_uploads().len(), 2);
    assert!(!h.mon.latched().contains(&2_000));
}

#[tokio::test]
#[serial_test::serial(heap_profile_integration)]
async fn mock_settings_off_from_start_never_dumps() {
    let mut h = Harness::start(settings(false, &[100])).await;
    assert!(!h.mon.config().enabled);
    assert!(!FAKE_PROF_ACTIVE.load(Ordering::Relaxed));

    FAKE_RESIDENT.store(1_000_000, Ordering::Relaxed);
    h.mon.poll_tick().await;

    assert!(h.mon.latched().is_empty());
    assert_eq!(FAKE_DUMP_COUNT.load(Ordering::Relaxed), 0);
    assert_eq!(h.server.storage_request_count(), 0);
    assert!(h.server.storage_uploads().is_empty());
}

#[tokio::test]
#[serial_test::serial(heap_profile_integration)]
async fn storage_unauthorized_latches_without_accepted_upload() {
    let mut h = Harness::start(settings(true, &[100])).await;
    h.server.set_storage_unauthorized(true);

    FAKE_RESIDENT.store(200, Ordering::Relaxed);
    h.mon.poll_tick().await;

    assert!(h.mon.latched().contains(&100));
    // Heap upload fails (401); meta is not attempted (no orphan .meta.json).
    assert_eq!(h.server.storage_request_count(), 1);
    assert!(h.server.storage_uploads().is_empty());
    assert_eq!(FAKE_DUMP_COUNT.load(Ordering::Relaxed), 1);
}

#[tokio::test]
#[serial_test::serial(heap_profile_integration)]
async fn empty_thresholds_from_settings_stay_disabled() {
    let mut h = Harness::start(settings(true, &[])).await;
    assert!(!h.mon.config().enabled);
    assert!(!FAKE_PROF_ACTIVE.load(Ordering::Relaxed));

    FAKE_RESIDENT.store(u64::MAX, Ordering::Relaxed);
    h.mon.poll_tick().await;
    assert_eq!(FAKE_DUMP_COUNT.load(Ordering::Relaxed), 0);
    assert_eq!(h.server.storage_request_count(), 0);
}

#[tokio::test]
#[serial_test::serial(heap_profile_integration)]
async fn trace_upload_disabled_keeps_monitor_off() {
    let mut remote = settings(true, &[100]);
    remote.trace_upload_enabled = Some(false);
    let mut h = Harness::start(remote).await;
    assert!(!h.mon.config().enabled);
    assert!(!FAKE_PROF_ACTIVE.load(Ordering::Relaxed));

    FAKE_RESIDENT.store(u64::MAX, Ordering::Relaxed);
    h.mon.poll_tick().await;
    assert_eq!(FAKE_DUMP_COUNT.load(Ordering::Relaxed), 0);
    assert_eq!(h.server.storage_request_count(), 0);
}

#[tokio::test]
#[serial_test::serial(heap_profile_integration)]
async fn multi_threshold_uploads_unique_paths_in_order() {
    let mut h = Harness::start(settings(true, &[100, 200])).await;

    FAKE_RESIDENT.store(500, Ordering::Relaxed);
    h.mon.poll_tick().await;
    assert!(h.mon.latched().contains(&100));
    assert!(!h.mon.latched().contains(&200));
    assert_eq!(h.server.storage_uploads().len(), 2);

    wait_for_next_unix_second().await;
    h.mon.poll_tick().await;
    assert!(h.mon.latched().contains(&200));
    let uploads = h.server.storage_uploads();
    assert_eq!(uploads.len(), 4);

    assert_jemalloc_object_pair(SID, TEST_VERSION, &uploads[0].path, &uploads[1].path);
    assert_jemalloc_object_pair(SID, TEST_VERSION, &uploads[2].path, &uploads[3].path);
    assert_ne!(uploads[0].path, uploads[2].path);
    assert_ne!(uploads[1].path, uploads[3].path);
    assert_storage_auth(&uploads);
    assert_meta_json(&uploads[1].body, 100, 500, 1_000);
    assert_meta_json(&uploads[3].body, 200, 500, 1_000);
}

#[tokio::test]
#[serial_test::serial(heap_profile_integration)]
async fn below_threshold_does_not_touch_storage() {
    let mut h = Harness::start(settings(true, &[10_000])).await;
    assert!(FAKE_PROF_ACTIVE.load(Ordering::Relaxed));

    FAKE_RESIDENT.store(9_999, Ordering::Relaxed);
    h.mon.poll_tick().await;

    assert!(h.mon.latched().is_empty());
    assert_eq!(FAKE_DUMP_COUNT.load(Ordering::Relaxed), 0);
    assert_eq!(h.server.storage_request_count(), 0);
}

#[tokio::test]
#[serial_test::serial(heap_profile_integration)]
async fn exact_threshold_triggers_dump_and_upload() {
    let mut h = Harness::start(settings(true, &[10_000])).await;

    FAKE_RESIDENT.store(10_000, Ordering::Relaxed);
    h.mon.poll_tick().await;

    assert!(h.mon.latched().contains(&10_000));
    assert_eq!(FAKE_DUMP_COUNT.load(Ordering::Relaxed), 1);
    assert_eq!(h.server.storage_request_count(), 2);
    let uploads = h.server.storage_uploads();
    assert_eq!(uploads.len(), 2);
    assert_jemalloc_object_pair(SID, TEST_VERSION, &uploads[0].path, &uploads[1].path);
    assert_storage_auth(&uploads);
    assert_meta_json(&uploads[1].body, 10_000, 10_000, 1_000);
}

#[tokio::test]
#[serial_test::serial(heap_profile_integration)]
async fn dump_failure_latches_without_storage_hit() {
    let mut h = Harness::start(settings(true, &[100])).await;
    FAKE_DUMP_FAIL.store(true, Ordering::Relaxed);
    FAKE_RESIDENT.store(200, Ordering::Relaxed);
    h.mon.poll_tick().await;

    assert!(h.mon.latched().contains(&100));
    assert_eq!(FAKE_DUMP_COUNT.load(Ordering::Relaxed), 1);
    assert_eq!(h.server.storage_request_count(), 0);
    assert!(h.server.storage_uploads().is_empty());
}

#[tokio::test]
#[serial_test::serial(heap_profile_integration)]
async fn re_enable_after_kill_switch_keeps_prior_latches() {
    let mut h = Harness::start(settings(true, &[100, 200])).await;
    FAKE_RESIDENT.store(150, Ordering::Relaxed);
    h.mon.poll_tick().await;
    assert!(h.mon.latched().contains(&100));
    assert_eq!(h.server.storage_uploads().len(), 2);

    h.server.set_settings(settings(false, &[100, 200]));
    h.reconfigure_from_server().await;
    assert!(!h.mon.config().enabled);

    FAKE_RESIDENT.store(500, Ordering::Relaxed);
    h.mon.poll_tick().await;
    assert_eq!(h.server.storage_uploads().len(), 2);

    h.server.set_settings(settings(true, &[100, 200]));
    h.reconfigure_from_server().await;
    assert!(h.mon.config().enabled);
    assert!(FAKE_PROF_ACTIVE.load(Ordering::Relaxed));

    wait_for_next_unix_second().await;
    h.mon.poll_tick().await;
    assert!(h.mon.latched().contains(&100));
    assert!(h.mon.latched().contains(&200));
    assert_eq!(h.server.storage_uploads().len(), 4);
    assert_eq!(FAKE_DUMP_COUNT.load(Ordering::Relaxed), 2);
    let uploads = h.server.storage_uploads();
    assert_ne!(uploads[0].path, uploads[2].path);
    assert_storage_auth(&uploads[2..]);
    assert_meta_json(&uploads[3].body, 200, 500, 1_000);
}
