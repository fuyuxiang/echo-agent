//! Spill-to-disk upload queue for cloud storage trace artifacts.
//!
//! Decouples data capture (inline, synchronous) from network upload (background, async).
//! Artifacts are written to temp files on disk at capture time, then uploaded by a
//! background worker with retries and error budget. This prevents data loss when
//! uploads fail transiently (429 rate limits, proxy restarts, network blips).
//!
//! The worker processes up to `max_concurrent` items in parallel using a semaphore.
//! Each item is spawned as an independent tokio task with its own retry loop.
//! The circuit breaker pauses dispatch (not in-flight tasks) when too many failures
//! accumulate without any successes.
use crate::gcs::{StorageConfig, upload_bytes, upload_file, upload_stream};
use crate::storage_client::{Auth401AttributionCallback, HttpUploadError};
use crate::{BlobCompression, TraceExportConfig, UploadMethod};
use anyhow::Context;
use async_compression::tokio::bufread::ZstdEncoder;
use echo_agent_auth::AuthCredentialProvider;
use echo_agent_circuit_breaker::{Disposition, RetryPolicy};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::Poll;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, ReadBuf};
use tokio::sync::{Notify, mpsc, oneshot};
use tracing::Instrument;
/// Resolves current upload credentials at upload time, plus optional
/// hooks the queue worker uses to wire refresh-aware credentials and
/// `auth_401_attribution` emission into the per-upload `StorageClient`.
///
/// The agent implements this by delegating to its AuthManager, ensuring fresh
/// tokens even when items have been queued for minutes. This avoids stale-token
/// failures on retried items whose original credentials may have expired.
///
/// `proxy_attribution`, `proxy_credentials`, and `proxy_http_client` mirror
/// the same-named methods on [`StorageConfig`]. They default to `None` so existing
/// implementors (tests, no-auth direct-mode resolvers) keep compiling without
/// changes; the queue worker calls them on every dispatch and stitches the
/// returned `Option`s onto the resolved [`TraceExportConfig`] before handing
/// it to the upload helpers.
pub trait TraceExportSource: Send + Sync {
    fn resolve(&self) -> TraceExportConfig;
    /// Async variant. Override to drive auth refresh; default delegates to sync.
    fn resolve_async(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = TraceExportConfig> + Send + '_>> {
        Box::pin(std::future::ready(self.resolve()))
    }
    /// 401-attribution callback for the per-upload `StorageClient`. Default
    /// `None` keeps the pre-existing behavior (no attribution events).
    fn proxy_attribution(&self) -> Option<Arc<dyn Auth401AttributionCallback>> {
        None
    }
    /// Refresh-aware credential provider for the per-upload `StorageClient`.
    /// Default `None` keeps the pre-existing behavior (the static `user_token`
    /// snapshot baked into the resolved `TraceExportConfig` is used).
    fn proxy_credentials(&self) -> Option<Arc<dyn AuthCredentialProvider>> {
        None
    }
    /// Tuned `reqwest::Client` for the per-upload `StorageClient`. Default
    /// `None` falls back to `reqwest::Client::new()` inside the helpers.
    fn proxy_http_client(&self) -> Option<reqwest::Client> {
        None
    }
    /// Park-on-401 recovery signal: a future resolving `true` iff credentials
    /// changed within `timeout`. `failed_bearer` is the token the rejected
    /// attempt used — implementations must resolve `true` immediately when
    /// the current credential already differs, or a rotation landing between
    /// wait slices is missed and retry stalls until the probe interval.
    /// `None` (the default) means no recovery is possible — static creds,
    /// S3/direct mode, or IdP-confirmed permanent failure — and the worker
    /// drops the auth-failed item immediately instead of parking it.
    fn wait_for_auth_recovery(
        &self,
        failed_bearer: Option<&str>,
        timeout: Duration,
    ) -> Option<std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + '_>>> {
        let _ = (failed_bearer, timeout);
        None
    }
    /// Whether the resolver holds a credential worth a real wire attempt — an
    /// unexpired token (in memory or on disk), or a static key. Default `true`
    /// always probes.
    fn has_usable_credential(&self) -> bool {
        true
    }
}
/// Worker-side wrapper that bundles a resolved `TraceExportConfig` with the
/// optional attribution / credentials / http_client provided by the
/// `TraceExportSource`. Constructed once per dispatch attempt so a token
/// rotation between attempts is reflected on the next try.
struct ResolvedStorageConfig {
    config: TraceExportConfig,
    attribution: Option<Arc<dyn Auth401AttributionCallback>>,
    credentials: Option<Arc<dyn AuthCredentialProvider>>,
    http_client: Option<reqwest::Client>,
}
impl ResolvedStorageConfig {
    /// Resolve config with fresh auth via `resolve_async`.
    async fn from_resolver_async(resolver: &Arc<dyn TraceExportSource>) -> Self {
        Self {
            config: resolver.resolve_async().await,
            attribution: resolver.proxy_attribution(),
            credentials: resolver.proxy_credentials(),
            http_client: resolver.proxy_http_client(),
        }
    }
    /// Bearer this resolved config puts on the wire — `snapshot()` mirrors
    /// `HttpAuth::apply` for provider-backed configs; the static fallback
    /// mirrors `EchoAgentAuthCredentials::apply` precedence (deployment key wins).
    fn wire_bearer(&self) -> Option<String> {
        if let Some(ref creds) = self.credentials {
            return creds.snapshot().token;
        }
        match self.config.upload_method() {
            UploadMethod::Proxy {
                user_token,
                deployment_key,
                ..
            } => deployment_key
                .clone()
                .or_else(|| (!user_token.is_empty()).then(|| user_token.clone())),
            _ => None,
        }
    }
}
impl StorageConfig for ResolvedStorageConfig {
    fn bucket_url(&self) -> &str {
        self.config.bucket_url()
    }
    fn upload_method(&self) -> &UploadMethod {
        self.config.upload_method()
    }
    fn proxy_attribution(&self) -> Option<Arc<dyn Auth401AttributionCallback>> {
        self.attribution.clone()
    }
    fn proxy_credentials(&self) -> Option<Arc<dyn AuthCredentialProvider>> {
        self.credentials.clone()
    }
    fn proxy_http_client(&self) -> Option<reqwest::Client> {
        self.http_client.clone()
    }
}
/// Default max age for upload queue items (2 hours).
///
/// Used by both the retry policy (`max_age`) and the startup orphan cleanup
/// (`cleanup_orphaned_uploads`). Kept as a constant so the two stay in sync —
/// if the cleanup threshold is shorter than the retry max_age, a process restart
/// can delete temp files that the previous worker was still trying to upload.
pub const DEFAULT_MAX_AGE: Duration = Duration::from_secs(2 * 60 * 60);
/// Retry policy for individual queue items.
#[derive(Clone, Debug)]
pub struct UploadRetryPolicy {
    /// Max attempts per item before giving up.
    pub max_attempts: u32,
    /// Initial backoff delay.
    pub initial_delay: Duration,
    /// Max backoff delay.
    pub max_delay: Duration,
    /// Backoff multiplier.
    pub multiplier: f64,
    /// Max age — items older than this are dropped to prevent unbounded growth.
    pub max_age: Duration,
    /// Minimum wall time between wire probe attempts while parked for auth
    /// recovery — the fallback for 401s that heal server-side without a
    /// client credential rotation. Env override:
    /// `ECHO_AGENT_UPLOAD_QUEUE_AUTH_PROBE_SECS`.
    pub auth_park_probe_interval: Duration,
}
pub const DEFAULT_AUTH_PARK_PROBE_INTERVAL: Duration = Duration::from_secs(300);
/// Smallest probe interval a `ECHO_AGENT_UPLOAD_QUEUE_AUTH_PROBE_SECS` override may
/// set. Probes can't fire faster than `AUTH_PARK_WAIT_INTERVAL` regardless, so
/// this exists mainly to reject the degenerate `0` (whole-second granularity
/// means a non-zero value already floors at one second).
const MIN_AUTH_PARK_PROBE_INTERVAL: Duration = Duration::from_secs(1);
/// Resolve a `ECHO_AGENT_UPLOAD_QUEUE_AUTH_PROBE_SECS` override (seconds) into a probe
/// interval. `0` is rejected (`None`) so a misconfiguration can't turn every
/// parked upload into a per-wait-slice retry storm; other values are floored at
/// [`MIN_AUTH_PARK_PROBE_INTERVAL`].
fn auth_park_probe_override(secs: u64) -> Option<Duration> {
    (secs > 0).then(|| Duration::from_secs(secs).max(MIN_AUTH_PARK_PROBE_INTERVAL))
}
impl Default for UploadRetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 10,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(120),
            multiplier: 2.0,
            max_age: DEFAULT_MAX_AGE,
            auth_park_probe_interval: DEFAULT_AUTH_PARK_PROBE_INTERVAL,
        }
    }
}
impl UploadRetryPolicy {
    fn backoff_delay(&self, attempt: u32) -> Duration {
        let base_ms = self.initial_delay.as_millis() as f64 * self.multiplier.powi(attempt as i32);
        let capped_ms = base_ms.min(self.max_delay.as_millis() as f64);
        Duration::from_millis(capped_ms as u64)
    }
}
/// Default disk budget for the upload queue temp directory.
const DEFAULT_MAX_QUEUE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// Bounded channel capacity — if full, enqueue falls back to inline upload.
const CHANNEL_CAPACITY: usize = 256;
/// Circuit breaker: pause after this many consecutive failures.
const CIRCUIT_BREAKER_THRESHOLD: u32 = 20;
/// Circuit breaker cooldown period.
const CIRCUIT_BREAKER_COOLDOWN: Duration = Duration::from_secs(60);
/// Default max concurrent uploads in the background worker.
const DEFAULT_MAX_CONCURRENT: usize = 8;
/// Total in-flight byte budget for inline-fallback uploads. Bounds resident
/// memory when uploads pile up under throttling (429s) on a multi-GB dataset.
/// 256 MiB balances upload parallelism against a hard memory lid.
const MAX_INLINE_FALLBACK_INFLIGHT_BYTES: u64 = 256 * 1024 * 1024;
/// Bytes per inline-fallback semaphore permit — see [`inline_fallback_permits`].
const INLINE_FALLBACK_PERMIT_BYTES: u64 = 1024 * 1024;
/// Total permits held by the inline-fallback semaphore (= 256).
const INLINE_FALLBACK_TOTAL_PERMITS: u32 =
    (MAX_INLINE_FALLBACK_INFLIGHT_BYTES / INLINE_FALLBACK_PERMIT_BYTES) as u32;
/// Map an upload size to inline-fallback permits: 1 MiB units rounded up, floor
/// of 1, clamped to the total. The clamp keeps a multi-GB file from requesting
/// more permits than the semaphore holds (which would deadlock `acquire_many`)
/// or overflowing `u32`.
fn inline_fallback_permits(size_bytes: u64) -> u32 {
    let units = size_bytes.div_ceil(INLINE_FALLBACK_PERMIT_BYTES);
    units.clamp(1, INLINE_FALLBACK_TOTAL_PERMITS as u64) as u32
}
/// A queue-owned temp file the worker uploads then deletes. Both variants are
/// owned (the queue never holds a caller's working-tree path); they differ only
/// in disk-budget accounting.
enum UploadSource {
    /// A temp file whose real disk cost equals its size (in-memory artifacts
    /// written to disk, or files copied into the queue dir).
    OwnedTemp(PathBuf),
    /// A reflink/CoW (or real-copy fallback) snapshot of a working-tree file,
    /// taken at enqueue (see `enqueue_file_reference`). `disk_bytes` is its REAL
    /// disk cost — 0 for a reflink (CoW shares blocks), the file size for a copy
    /// — used for budget accounting instead of the (large) logical size.
    OwnedSnapshot { path: PathBuf, disk_bytes: u64 },
}
impl UploadSource {
    /// Filesystem path of the artifact bytes.
    fn path(&self) -> &Path {
        match self {
            UploadSource::OwnedTemp(p) | UploadSource::OwnedSnapshot { path: p, .. } => p,
        }
    }
    /// Real disk bytes this item contributes to the queue budget (0 for a
    /// reflink snapshot, which shares blocks with the source until modified).
    fn disk_bytes(&self, fallback_size: u64) -> u64 {
        match self {
            UploadSource::OwnedTemp(_) => fallback_size,
            UploadSource::OwnedSnapshot { disk_bytes, .. } => *disk_bytes,
        }
    }
}
/// Schema version stamped on every [`QueueItemSidecar`]; bumped only on
/// breaking manifest-shape changes.
pub const QUEUE_ITEM_SIDECAR_SCHEMA_VERSION: u32 = 1;
/// Sidecar manifest written as `<temp>.meta.json` next to a queue temp file by
/// [`UploadQueue::enqueue_bytes_blocking`] (the fire-and-forget paths write the
/// temp file alone). It carries everything a fresh process needs to re-enqueue
/// the upload after a restart — the temp-file name alone is lossy (truncated
/// `session_id`, no GCS path). Read by `echo_agent_workspace::recovery`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct QueueItemSidecar {
    /// Manifest schema version (see [`QUEUE_ITEM_SIDECAR_SCHEMA_VERSION`]).
    #[serde(default = "default_sidecar_schema_version")]
    pub schema_version: u32,
    /// Session that produced the artifact.
    pub session_id: String,
    /// Turn the artifact belongs to.
    pub turn_number: u64,
    /// Destination object path in cloud storage.
    pub gcs_path: String,
    /// MIME type for the upload.
    pub content_type: String,
    pub artifact_name: String,
    /// RFC3339 timestamp of when the item was first enqueued.
    pub enqueued_at: String,
    /// Hex SHA-256 of the temp-file contents, verified at recovery time so a
    /// corrupt temp file is dropped instead of re-uploaded.
    pub sha256: String,
}
fn default_sidecar_schema_version() -> u32 {
    QUEUE_ITEM_SIDECAR_SCHEMA_VERSION
}
/// A pending upload in the spill-to-disk queue.
struct UploadQueueItem {
    /// Source of the artifact bytes and whether the queue owns the file.
    source: UploadSource,
    /// Recovery sidecar path (set only by `enqueue_bytes_blocking`); deleted
    /// with the temp file on every terminal outcome.
    sidecar_path: Option<PathBuf>,
    /// Destination path in cloud storage (e.g., "{session_id}/turn_0/metadata.json").
    gcs_path: String,
    /// Parent span captured at enqueue time so the upload links back to the caller's trace.
    parent_span: tracing::Span,
    /// MIME type for the upload.
    content_type: String,
    /// Human-readable label for logging.
    artifact_name: String,
    /// Number of upload attempts so far.
    attempts: u32,
    /// When this item was first enqueued.
    enqueued_at: Instant,
    /// Optional completion signal for callers that need to block until done.
    completion_tx: Option<oneshot::Sender<anyhow::Result<UploadCompletion>>>,
    /// EchoAgent client version string, stamped on the `gcs_queue_upload` tracing span.
    /// Copied from `UploadQueue::client_version` at enqueue time.
    client_version: Option<String>,
    /// When true, the upload worker compresses the file with zstd before uploading.
    compress: bool,
    /// Un-marks this item's `gcs_path` from the in-flight set on drop (any
    /// terminal outcome). `None` when not dedup-tracked; held only for its `Drop`.
    _in_flight: Option<InFlightGuard>,
}
/// Completion info returned by the upload worker after a successful upload.
#[derive(Debug)]
pub struct UploadCompletion {
    pub gcs_url: String,
    pub compression: BlobCompression,
    pub original_size: u64,
    pub stored_size: u64,
}
/// Result of enqueueing a file with optional compression.
pub struct EnqueueResult {
    pub completion_rx: oneshot::Receiver<anyhow::Result<UploadCompletion>>,
    pub original_size: u64,
}
/// Shared statistics for monitoring and disk budget enforcement.
pub struct UploadQueueStats {
    /// Items counted from enqueue acceptance until upload completion; includes
    /// the [`inflight`](Self::inflight) subset.
    pub pending: AtomicU64,
    /// Total bytes of pending temp files on disk.
    pub pending_bytes: AtomicU64,
    /// Pending items actively uploading right now (a subset of `pending`).
    pub inflight: AtomicU64,
    /// Cumulative items enqueued for background upload.
    pub enqueued: AtomicU64,
    /// Cumulative enqueue attempts dropped because an identical `gcs_path` was
    /// already in flight (local content dedup).
    pub deduplicated: AtomicU64,
    /// Cumulative successful uploads.
    pub uploaded: AtomicU64,
    /// Cumulative failed uploads (exhausted budget, includes expired items).
    pub failed: AtomicU64,
    /// Circuit breaker activations (cumulative count of trips).
    pub circuit_breaker_trips: AtomicU64,
    /// `true` while the breaker is currently paused; cleared after the
    /// cooldown. Distinct from the cumulative `circuit_breaker_trips`.
    pub circuit_breaker_active: AtomicBool,
    /// Times enqueue fell back to inline (queue full or disk budget exceeded).
    pub enqueue_fallbacks: AtomicU64,
    /// Temp files we couldn't remove (non-`NotFound`). Bumped by `try_remove_temp`.
    pub leaked_temp_files: AtomicU64,
    /// Reference uploads skipped because the source was missing or its content
    /// no longer matched `expected_sha256` (corruption guard). Non-fatal.
    pub reference_stale: AtomicU64,
    /// Items that entered the parked-for-auth state. An item parks at most once.
    pub auth_parked: AtomicU64,
    /// Orphan-sweep deletions of a lone queue file (temp without sidecar or
    /// vice versa). Surfaced as `cleanup_orphan_mismatched_total`; only bumped
    /// by [`UploadQueue::cleanup_orphans`], not the legacy free function.
    pub cleanup_orphan_mismatched: AtomicU64,
    /// Optional listener pinged on each pending-count transition so a status
    /// publisher can republish immediately. Wired via `set_transition_notify`.
    transition_notify: OnceLock<Arc<Notify>>,
    /// Internal listener for [`UploadQueue::wait_idle`]. Separate from the
    /// single-slot `transition_notify` so idle-waiters never compete with the
    /// external status publisher for the one wiring.
    idle_notify: Notify,
}
impl Default for UploadQueueStats {
    fn default() -> Self {
        Self::new()
    }
}
impl UploadQueueStats {
    pub fn new() -> Self {
        Self {
            pending: AtomicU64::new(0),
            pending_bytes: AtomicU64::new(0),
            inflight: AtomicU64::new(0),
            enqueued: AtomicU64::new(0),
            deduplicated: AtomicU64::new(0),
            uploaded: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            circuit_breaker_trips: AtomicU64::new(0),
            circuit_breaker_active: AtomicBool::new(false),
            enqueue_fallbacks: AtomicU64::new(0),
            leaked_temp_files: AtomicU64::new(0),
            reference_stale: AtomicU64::new(0),
            auth_parked: AtomicU64::new(0),
            cleanup_orphan_mismatched: AtomicU64::new(0),
            transition_notify: OnceLock::new(),
            idle_notify: Notify::new(),
        }
    }
    /// Wire an external transition listener. Set once; a second call is a
    /// no-op (the first notifier wins).
    pub fn set_transition_notify(&self, notify: Arc<Notify>) {
        let _ = self.transition_notify.set(notify);
    }
    /// Wake the wired transition listener, if any, and any idle-waiters.
    fn notify_transition(&self) {
        if let Some(notify) = self.transition_notify.get() {
            notify.notify_waiters();
        }
        self.idle_notify.notify_waiters();
    }
}
/// Remove `path`; on non-`NotFound` failure, warn and bump `leaked_temp_files`
/// (when `stats` is `Some`). `stats` is optional for callers without a live
/// queue handle (e.g. the startup sweep).
pub fn try_remove_temp(path: &Path, stats: Option<&UploadQueueStats>) {
    if let Err(e) = std::fs::remove_file(path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(
            path = %path.display(),
            error = %e,
            "Failed to remove upload-queue temp file; leaked"
        );
        if let Some(s) = stats {
            s.leaked_temp_files.fetch_add(1, Ordering::Relaxed);
        }
    }
}
/// Delete the queue-owned temp file backing `source`. Both variants are
/// queue-owned (a working-tree source is snapshotted at enqueue, never enqueued
/// directly), so this always removes the file.
fn remove_owned_source(source: &UploadSource, stats: Option<&UploadQueueStats>) {
    try_remove_temp(source.path(), stats);
}
/// Delete a queue item's temp file and sidecar (if any) as a pair on every
/// terminal outcome, so a done item never leaves a `.meta.json` for the
/// restart-recovery scanner to re-process.
fn remove_item_files(item: &UploadQueueItem, stats: Option<&UploadQueueStats>) {
    remove_owned_source(&item.source, stats);
    if let Some(sidecar) = &item.sidecar_path {
        try_remove_temp(sidecar, stats);
    }
}
/// Shutdown state for the background worker, taken by `drain()`.
struct DrainState {
    shutdown_tx: oneshot::Sender<()>,
    worker_handle: tokio::task::JoinHandle<()>,
}
/// Handle for submitting artifacts to the background upload queue.
///
/// Clone-able — share across the agent struct and upload call sites.
/// The background worker is spawned once at creation time and runs until
/// the sender side is dropped (or `drain()` is called on shutdown).
#[derive(Clone)]
pub struct UploadQueue {
    tx: mpsc::Sender<UploadQueueItem>,
    queue_dir: PathBuf,
    resolver: Arc<dyn TraceExportSource>,
    stats: Arc<UploadQueueStats>,
    max_queue_bytes: u64,
    /// EchoAgent client version string stamped on every `gcs_queue_upload` tracing span.
    /// Enables per-version breakdown of upload failures in analytics dashboards.
    pub client_version: Option<String>,
    drain_state: Arc<Mutex<Option<DrainState>>>,
    /// Byte-budget semaphore for inline-fallback uploads (disk budget exhausted /
    /// channel full); each upload acquires [`inline_fallback_permits`] for its
    /// size. Bounds memory + concurrency for the path-streaming variants, and
    /// concurrency only for the bytes variant (`spawn_inline_upload`).
    inline_fallback_semaphore: Arc<tokio::sync::Semaphore>,
    /// Destinations currently queued or uploading, so a duplicate enqueue is
    /// dropped before it spills a second copy to disk.
    uploads_in_flight: Arc<Mutex<HashSet<String>>>,
}
/// Marks one `gcs_path` as in flight; un-marks it from
/// [`UploadQueue::uploads_in_flight`] on drop.
struct InFlightGuard {
    gcs_path: String,
    in_flight: Arc<Mutex<HashSet<String>>>,
}
impl Drop for InFlightGuard {
    fn drop(&mut self) {
        let mut set = match self.in_flight.lock() {
            Ok(set) => set,
            Err(poisoned) => poisoned.into_inner(),
        };
        set.remove(&self.gcs_path);
    }
}
/// Only objects named by their content hash (`sha256_<hex>`) are safe to dedup on
/// path: a stable path with mutable content would drop a changed re-upload.
fn is_content_addressed(gcs_path: &str) -> bool {
    gcs_path
        .rsplit('/')
        .next()
        .is_some_and(|object| object.starts_with("sha256_"))
}
/// Marker error for [`UploadQueue::enqueue_blocking`] when the worker is shut
/// down (channel closed, or worker aborted before sending a completion).
/// Downcastable so callers can distinguish "queue unavailable" (retry another
/// way) from a genuine upload failure (already retried by the worker).
#[derive(Debug)]
pub struct QueueClosed;
impl std::fmt::Display for QueueClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("upload queue worker is shut down")
    }
}
impl std::error::Error for QueueClosed {}
/// Structured outcome of [`UploadQueue::enqueue_bytes_blocking`].
///
/// Distinguishes the three terminal states of an enqueue attempt so callers
/// can report a truthful per-artifact status without inspecting queue
/// internals. The value is returned once the worker has accepted the item
/// (durably on disk) or a fallback / failure has been decided — it does NOT
/// reflect cloud-upload completion. Use [`UploadQueue::enqueue_blocking`] when
/// you need to await the upload itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnqueueOutcome {
    /// Bytes were written to `upload_queue/` as a `.tmp` file AND accepted by
    /// the background worker channel. The worker owns the cloud upload and its
    /// retry policy from here on.
    Enqueued,
    /// The disk budget was exceeded or the worker channel was full, so an
    /// inline fallback upload was spawned (bounded by the inline-fallback
    /// byte-budget semaphore). The bytes are not on the queue's disk spill but
    /// an upload is in flight.
    FellBackToInline,
    /// The temp file could not be written, or the worker is shut down. The
    /// artifact was not handed off anywhere; the caller should log and skip.
    Failed { reason: String },
    /// An identical `gcs_path` was already in flight, so this enqueue was skipped.
    Deduplicated,
    /// The caller skipped enqueue on purpose (e.g. collect deadline). Not a
    /// queue failure; after-turn reduction must not treat this as `Failed`.
    Skipped { reason: String },
}
/// Internal outcome of [`UploadQueue::enqueue_core`], the shared body behind
/// [`UploadQueue::enqueue`] and [`UploadQueue::enqueue_bytes_blocking`].
///
/// The core performs all the common bookkeeping (temp-file write, disk-budget
/// check, item construction, stats, `try_send`) and the inline fallback for the
/// over-budget / channel-full branches. The *closed-channel* branch is the one
/// place the two public methods diverge, so the core stops there and lets each
/// caller decide (`enqueue` inline-falls-back; `enqueue_bytes_blocking` reports
/// `Failed`).
enum EnqueueAttempt {
    /// The temp file could not be written; nothing was enqueued.
    WriteError(anyhow::Error),
    /// An identical `gcs_path` is already queued/uploading; nothing was written.
    Deduplicated,
    /// Item written and accepted by the worker channel.
    Sent,
    /// Over disk budget or channel full: temp removed / pending rolled back,
    /// `enqueue_fallbacks` bumped, and an inline fallback upload already spawned.
    InlineFallback,
    /// Worker channel is closed (shut down): temp removed and `pending` /
    /// `pending_bytes` rolled back, but NO fallback spawned and
    /// `enqueue_fallbacks` NOT bumped — the caller owns that decision.
    ChannelClosed,
}
impl UploadQueue {
    /// Create the queue, initialize the temp directory, and spawn the background worker.
    pub fn spawn(
        echo_agent_home: &Path,
        resolver: Arc<dyn TraceExportSource>,
        retry_policy: UploadRetryPolicy,
    ) -> Self {
        Self::spawn_with_concurrency(
            echo_agent_home,
            resolver,
            retry_policy,
            DEFAULT_MAX_CONCURRENT,
        )
    }
    /// Create the queue with explicit concurrency limit for the background worker.
    pub fn spawn_with_concurrency(
        echo_agent_home: &Path,
        resolver: Arc<dyn TraceExportSource>,
        mut retry_policy: UploadRetryPolicy,
        max_concurrent: usize,
    ) -> Self {
        let queue_dir = echo_agent_home.join("upload_queue");
        if let Err(e) = std::fs::create_dir_all(&queue_dir) {
            tracing::warn!(error = %e, "Failed to create upload queue dir");
        }
        if let Some(raw_secs) = std::env::var("ECHO_AGENT_UPLOAD_QUEUE_AUTH_PROBE_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
        {
            match auth_park_probe_override(raw_secs) {
                Some(interval) => retry_policy.auth_park_probe_interval = interval,
                None => {
                    tracing::warn!(
                        "Ignoring ECHO_AGENT_UPLOAD_QUEUE_AUTH_PROBE_SECS={raw_secs}: a zero probe \
                     interval would re-attempt every parked upload on every wait slice. \
                     Keeping the {}s default.",
                        DEFAULT_AUTH_PARK_PROBE_INTERVAL.as_secs(),
                    )
                }
            }
        }
        let max_queue_bytes = std::env::var("ECHO_AGENT_UPLOAD_QUEUE_MAX_BYTES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_MAX_QUEUE_BYTES);
        let stats = Arc::new(UploadQueueStats::new());
        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let worker_resolver = resolver.clone();
        let worker_stats = stats.clone();
        let worker_handle = tokio::spawn(upload_worker(
            rx,
            shutdown_rx,
            worker_resolver,
            retry_policy,
            worker_stats,
            max_concurrent,
        ));
        let drain_state = Arc::new(Mutex::new(Some(DrainState {
            shutdown_tx,
            worker_handle,
        })));
        Self {
            tx,
            queue_dir,
            resolver,
            stats,
            max_queue_bytes,
            client_version: None,
            drain_state,
            inline_fallback_semaphore: Arc::new(tokio::sync::Semaphore::new(
                INLINE_FALLBACK_TOTAL_PERMITS as usize,
            )),
            uploads_in_flight: Arc::new(Mutex::new(HashSet::new())),
        }
    }
    /// Mark `gcs_path` as in flight; the guard un-marks it on drop, or `None` if
    /// an identical upload is already in flight (skip it). Only the queued path is
    /// deduped; the inline fallback frees the guard on return.
    fn mark_in_flight(&self, gcs_path: &str) -> Option<InFlightGuard> {
        let mut set = match self.uploads_in_flight.lock() {
            Ok(set) => set,
            Err(poisoned) => poisoned.into_inner(),
        };
        if set.insert(gcs_path.to_string()) {
            Some(InFlightGuard {
                gcs_path: gcs_path.to_string(),
                in_flight: self.uploads_in_flight.clone(),
            })
        } else {
            self.stats.deduplicated.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(
                gcs_path,
                "upload queue: skipping duplicate in-flight upload"
            );
            None
        }
    }
    /// Set the echoagent client version to stamp on every `gcs_queue_upload` span.
    pub fn with_client_version(mut self, version: impl Into<String>) -> Self {
        self.client_version = Some(version.into());
        self
    }
    /// Override the temp-dir disk budget. Test seam to force the over-budget
    /// inline-fallback path without mutating the process-global env var.
    pub fn with_max_queue_bytes(mut self, max_bytes: u64) -> Self {
        self.max_queue_bytes = max_bytes;
        self
    }
    /// Enqueue bytes for background upload. Writes to temp file, returns immediately.
    ///
    /// Falls back to inline upload (current behavior) if the queue channel is full
    /// or the disk budget is exceeded.
    pub async fn enqueue(
        &self,
        content: &[u8],
        gcs_path: &str,
        content_type: &str,
        artifact_name: &str,
        session_id: &str,
        turn_number: u64,
    ) -> anyhow::Result<()> {
        match self.enqueue_core(
            content,
            gcs_path,
            content_type,
            artifact_name,
            session_id,
            turn_number,
            false,
        ) {
            EnqueueAttempt::WriteError(e) => Err(e),
            EnqueueAttempt::Sent
            | EnqueueAttempt::InlineFallback
            | EnqueueAttempt::Deduplicated => Ok(()),
            EnqueueAttempt::ChannelClosed => {
                tracing::debug!("Upload queue closed, falling back to inline upload");
                self.stats.enqueue_fallbacks.fetch_add(1, Ordering::Relaxed);
                self.spawn_inline_upload(content, gcs_path, content_type);
                Ok(())
            }
        }
    }
    /// Enqueue bytes for background upload, reporting a structured
    /// [`EnqueueOutcome`] instead of `Result<()>`.
    ///
    /// Mirrors [`Self::enqueue`] — same temp-file write, over-budget check and
    /// channel handling — but maps each terminal branch to a distinct
    /// [`EnqueueOutcome`] so callers can surface a truthful per-artifact
    /// status. Returns once the worker has accepted the item (durably on disk);
    /// it does NOT block on the cloud upload. Use [`Self::enqueue_blocking`] for
    /// the await-upload-completion contract.
    ///
    /// The one behavioural difference from [`Self::enqueue`]: a *closed* worker
    /// channel maps to [`EnqueueOutcome::Failed`] (no inline fallback) because a
    /// shut-down worker means the artifact is lost. A *full* channel still falls
    /// back to inline upload ([`EnqueueOutcome::FellBackToInline`]), exactly as
    /// [`Self::enqueue`] does.
    pub async fn enqueue_bytes_blocking(
        &self,
        content: &[u8],
        gcs_path: &str,
        content_type: &str,
        artifact_name: &str,
        session_id: &str,
        turn_number: u64,
    ) -> EnqueueOutcome {
        match self.enqueue_core(
            content,
            gcs_path,
            content_type,
            artifact_name,
            session_id,
            turn_number,
            true,
        ) {
            EnqueueAttempt::WriteError(e) => EnqueueOutcome::Failed {
                reason: e.to_string(),
            },
            EnqueueAttempt::Deduplicated => EnqueueOutcome::Deduplicated,
            EnqueueAttempt::Sent => EnqueueOutcome::Enqueued,
            EnqueueAttempt::InlineFallback => EnqueueOutcome::FellBackToInline,
            EnqueueAttempt::ChannelClosed => {
                tracing::debug!("Upload queue closed; enqueue_bytes_blocking reporting Failed");
                EnqueueOutcome::Failed {
                    reason: "upload queue worker is shut down".to_string(),
                }
            }
        }
    }
    /// Re-enqueue an existing on-disk pair (temp + sidecar) left by a prior
    /// process life, without rewriting either file. Used by startup recovery.
    ///
    /// Reusing the original pair keeps the sidecar's `enqueued_at` anchored to
    /// the first spill, so repeated restarts cannot slide the recovery max-age
    /// window indefinitely (a fresh pair per boot would reset the clock each
    /// time). The worker owns the pair from `Enqueued` onward and deletes both
    /// files on every terminal outcome, exactly as for a normal enqueue.
    ///
    /// On `Failed` (worker shut down, channel full, or over the disk budget)
    /// the pair is left untouched so a later startup can retry; no inline
    /// fallback is attempted — recovery runs pre-hub-connect where blocking on
    /// cloud I/O would delay registration.
    pub fn enqueue_recovered(
        &self,
        temp_path: &Path,
        sidecar_path: &Path,
        sidecar: &QueueItemSidecar,
    ) -> EnqueueOutcome {
        let size = file_size(temp_path);
        if self.over_disk_budget(size) {
            return EnqueueOutcome::Failed {
                reason: "over disk budget".to_string(),
            };
        }
        let item = UploadQueueItem {
            source: UploadSource::OwnedTemp(temp_path.to_path_buf()),
            gcs_path: sidecar.gcs_path.clone(),
            content_type: sidecar.content_type.clone(),
            artifact_name: sidecar.artifact_name.clone(),
            attempts: 0,
            enqueued_at: Instant::now(),
            sidecar_path: Some(sidecar_path.to_path_buf()),
            completion_tx: None,
            client_version: self.client_version.clone(),
            compress: false,
            parent_span: tracing::Span::current(),
            _in_flight: None,
        };
        self.stats.pending.fetch_add(1, Ordering::Relaxed);
        self.stats.pending_bytes.fetch_add(size, Ordering::Relaxed);
        self.stats.enqueued.fetch_add(1, Ordering::Relaxed);
        match self.tx.try_send(item) {
            Ok(()) => EnqueueOutcome::Enqueued,
            Err(e) => {
                self.stats.pending.fetch_sub(1, Ordering::Relaxed);
                self.stats.pending_bytes.fetch_sub(size, Ordering::Relaxed);
                self.stats.enqueued.fetch_sub(1, Ordering::Relaxed);
                self.stats.notify_transition();
                let reason = match e {
                    mpsc::error::TrySendError::Closed(_) => "upload queue worker is shut down",
                    mpsc::error::TrySendError::Full(_) => "upload queue channel full",
                };
                EnqueueOutcome::Failed {
                    reason: reason.to_string(),
                }
            }
        }
    }
    /// Shared body behind [`Self::enqueue`] and [`Self::enqueue_bytes_blocking`].
    ///
    /// Writes the temp file, checks the disk budget, builds the queue item, and
    /// `try_send`s it — performing all stats bookkeeping and the inline fallback
    /// for the over-budget / channel-full branches. The closed-channel branch is
    /// left to the caller (the two methods diverge only there), so its
    /// `enqueue_fallbacks`/inline decision is NOT taken here. See
    /// [`EnqueueAttempt`].
    ///
    /// When `write_sidecar` is true, a [`QueueItemSidecar`] is written next to
    /// the temp file — but only after the disk-budget gate passes, so the
    /// over-budget fallback never pays for a sidecar it would immediately
    /// delete. All cleanup branches remove temp and sidecar together,
    /// preserving the pair invariant.
    fn enqueue_core(
        &self,
        content: &[u8],
        gcs_path: &str,
        content_type: &str,
        artifact_name: &str,
        session_id: &str,
        turn_number: u64,
        write_sidecar: bool,
    ) -> EnqueueAttempt {
        let in_flight = if is_content_addressed(gcs_path) {
            match self.mark_in_flight(gcs_path) {
                Some(guard) => Some(guard),
                None => return EnqueueAttempt::Deduplicated,
            }
        } else {
            None
        };
        let temp_path = match self.write_temp_file(content, artifact_name, session_id, turn_number)
        {
            Ok(p) => p,
            Err(e) => return EnqueueAttempt::WriteError(e),
        };
        let size = content.len() as u64;
        if self.over_disk_budget(size) {
            try_remove_temp(&temp_path, Some(&self.stats));
            self.stats.enqueue_fallbacks.fetch_add(1, Ordering::Relaxed);
            self.spawn_inline_upload(content, gcs_path, content_type);
            return EnqueueAttempt::InlineFallback;
        }
        let sidecar_path = if write_sidecar {
            match self.write_sidecar_file(
                &temp_path,
                content,
                gcs_path,
                content_type,
                artifact_name,
                session_id,
                turn_number,
            ) {
                Ok(p) => Some(p),
                Err(e) => {
                    try_remove_temp(&temp_path, Some(&self.stats));
                    return EnqueueAttempt::WriteError(e);
                }
            }
        } else {
            None
        };
        let item = UploadQueueItem {
            source: UploadSource::OwnedTemp(temp_path),
            gcs_path: gcs_path.to_string(),
            content_type: content_type.to_string(),
            artifact_name: artifact_name.to_string(),
            attempts: 0,
            enqueued_at: Instant::now(),
            sidecar_path,
            completion_tx: None,
            client_version: self.client_version.clone(),
            compress: false,
            parent_span: tracing::Span::current(),
            _in_flight: in_flight,
        };
        self.stats.pending.fetch_add(1, Ordering::Relaxed);
        self.stats.pending_bytes.fetch_add(size, Ordering::Relaxed);
        self.stats.enqueued.fetch_add(1, Ordering::Relaxed);
        match self.tx.try_send(item) {
            Ok(()) => {
                self.stats.notify_transition();
                EnqueueAttempt::Sent
            }
            Err(e) => {
                let closed = matches!(&e, mpsc::error::TrySendError::Closed(_));
                let rejected = e.into_inner();
                remove_item_files(&rejected, Some(&self.stats));
                self.stats.pending.fetch_sub(1, Ordering::Relaxed);
                self.stats.pending_bytes.fetch_sub(size, Ordering::Relaxed);
                self.stats.enqueued.fetch_sub(1, Ordering::Relaxed);
                self.stats.notify_transition();
                if closed {
                    EnqueueAttempt::ChannelClosed
                } else {
                    self.stats.enqueue_fallbacks.fetch_add(1, Ordering::Relaxed);
                    self.spawn_inline_upload(content, gcs_path, content_type);
                    EnqueueAttempt::InlineFallback
                }
            }
        }
    }
    /// Enqueue bytes and block until upload completes. Returns the upload URL on success.
    ///
    /// Used for `block_for_upload` mode where the caller must await completion
    /// (e.g., metadata.json enrichment on the proxy). Writes the recovery
    /// sidecar like [`Self::enqueue_bytes_blocking`], so an item outliving the
    /// waiter (cancelled confirmation, process exit mid-retry) spills as a
    /// pair the next run re-enqueues.
    pub async fn enqueue_blocking(
        &self,
        content: &[u8],
        gcs_path: &str,
        content_type: &str,
        artifact_name: &str,
        session_id: &str,
        turn_number: u64,
    ) -> anyhow::Result<String> {
        let temp_path = self.write_temp_file(content, artifact_name, session_id, turn_number)?;
        let sidecar_path = match self.write_sidecar_file(
            &temp_path,
            content,
            gcs_path,
            content_type,
            artifact_name,
            session_id,
            turn_number,
        ) {
            Ok(p) => p,
            Err(e) => {
                try_remove_temp(&temp_path, Some(&self.stats));
                return Err(e);
            }
        };
        let size = content.len() as u64;
        let (tx, rx) = oneshot::channel();
        let item = UploadQueueItem {
            source: UploadSource::OwnedTemp(temp_path),
            gcs_path: gcs_path.to_string(),
            content_type: content_type.to_string(),
            artifact_name: artifact_name.to_string(),
            attempts: 0,
            enqueued_at: Instant::now(),
            sidecar_path: Some(sidecar_path),
            completion_tx: Some(tx),
            client_version: self.client_version.clone(),
            compress: false,
            parent_span: tracing::Span::current(),
            _in_flight: None,
        };
        self.stats.pending.fetch_add(1, Ordering::Relaxed);
        self.stats.pending_bytes.fetch_add(size, Ordering::Relaxed);
        self.stats.enqueued.fetch_add(1, Ordering::Relaxed);
        match self.tx.try_send(item) {
            Ok(()) => self.stats.notify_transition(),
            Err(e) => {
                let closed = matches!(&e, mpsc::error::TrySendError::Closed(_));
                let rejected = e.into_inner();
                self.stats.pending.fetch_sub(1, Ordering::Relaxed);
                self.stats.pending_bytes.fetch_sub(size, Ordering::Relaxed);
                self.stats.enqueued.fetch_sub(1, Ordering::Relaxed);
                self.stats.notify_transition();
                if closed {
                    remove_item_files(&rejected, Some(&self.stats));
                    return Err(anyhow::Error::new(QueueClosed).context("upload queue closed"));
                }
                if let Some(sidecar) = &rejected.sidecar_path {
                    try_remove_temp(sidecar, Some(&self.stats));
                }
                self.stats.enqueue_fallbacks.fetch_add(1, Ordering::Relaxed);
                self.spawn_inline_upload_owned_snapshot(
                    rejected.source.path().to_path_buf(),
                    gcs_path.to_string(),
                    content_type.to_string(),
                    size,
                    rejected.completion_tx,
                );
            }
        }
        rx.await
            .map_err(|_| {
                anyhow::Error::new(QueueClosed).context("worker dropped completion channel")
            })?
            .map(|c| c.gcs_url)
    }
    /// Enqueue a file for background upload.
    ///
    /// Copies the source file to the queue directory (reflink on APFS/btrfs).
    pub async fn enqueue_file(
        &self,
        source_path: &Path,
        gcs_path: &str,
        content_type: &str,
        artifact_name: &str,
        session_id: &str,
        turn_number: u64,
    ) -> anyhow::Result<()> {
        let in_flight = if is_content_addressed(gcs_path) {
            match self.mark_in_flight(gcs_path) {
                Some(guard) => Some(guard),
                None => return Ok(()),
            }
        } else {
            None
        };
        let size = std::fs::metadata(source_path)
            .with_context(|| format!("Failed to stat {} for upload queue", source_path.display()))?
            .len();
        if self.over_disk_budget(size) {
            self.stats.enqueue_fallbacks.fetch_add(1, Ordering::Relaxed);
            self.spawn_inline_upload_from_path(
                source_path.to_path_buf(),
                gcs_path.to_string(),
                content_type.to_string(),
                size,
            );
            return Ok(());
        }
        let dest_name = temp_file_name(artifact_name, session_id, turn_number);
        let dest_path = self.queue_dir.join(dest_name);
        std::fs::copy(source_path, &dest_path)
            .with_context(|| format!("Failed to copy {} to queue", source_path.display()))?;
        let item = UploadQueueItem {
            source: UploadSource::OwnedTemp(dest_path),
            gcs_path: gcs_path.to_string(),
            content_type: content_type.to_string(),
            artifact_name: artifact_name.to_string(),
            attempts: 0,
            enqueued_at: Instant::now(),
            sidecar_path: None,
            completion_tx: None,
            client_version: self.client_version.clone(),
            compress: false,
            parent_span: tracing::Span::current(),
            _in_flight: in_flight,
        };
        self.stats.pending.fetch_add(1, Ordering::Relaxed);
        self.stats.pending_bytes.fetch_add(size, Ordering::Relaxed);
        self.stats.enqueued.fetch_add(1, Ordering::Relaxed);
        if let Err(e) = self.tx.try_send(item) {
            if matches!(&e, mpsc::error::TrySendError::Closed(_)) {
                tracing::debug!("Upload queue closed, falling back to inline upload");
            }
            let rejected = e.into_inner();
            remove_owned_source(&rejected.source, Some(&self.stats));
            self.stats.pending.fetch_sub(1, Ordering::Relaxed);
            self.stats.pending_bytes.fetch_sub(size, Ordering::Relaxed);
            self.stats.notify_transition();
            self.stats.enqueue_fallbacks.fetch_add(1, Ordering::Relaxed);
            self.spawn_inline_upload_from_path(
                source_path.to_path_buf(),
                gcs_path.to_string(),
                content_type.to_string(),
                size,
            );
            Ok(())
        } else {
            Ok(())
        }
    }
    /// Enqueue a file for upload, optionally zstd-compressed at upload time
    /// (only when `compress = true` and file >= 128 bytes). On budget-gate
    /// fallback the upload goes inline uncompressed regardless of `compress`.
    pub async fn enqueue_file_blocking(
        &self,
        source_path: &Path,
        gcs_path: &str,
        content_type: &str,
        artifact_name: &str,
        session_id: &str,
        turn_number: u64,
        compress: bool,
    ) -> anyhow::Result<EnqueueResult> {
        let source_size = file_size(source_path);
        let in_flight = if is_content_addressed(gcs_path) {
            match self.mark_in_flight(gcs_path) {
                Some(guard) => Some(guard),
                None => {
                    let (tx, rx) = oneshot::channel();
                    let _ = tx.send(Err(anyhow::anyhow!(
                        "deduplicated: identical gcs_path already in flight"
                    )));
                    return Ok(EnqueueResult {
                        completion_rx: rx,
                        original_size: source_size,
                    });
                }
            }
        } else {
            None
        };
        if self.over_disk_budget(source_size) {
            self.stats.enqueue_fallbacks.fetch_add(1, Ordering::Relaxed);
            let (tx, rx) = oneshot::channel();
            self.spawn_inline_upload_blocking(
                source_path.to_path_buf(),
                gcs_path.to_string(),
                content_type.to_string(),
                source_size,
                tx,
            );
            return Ok(EnqueueResult {
                completion_rx: rx,
                original_size: source_size,
            });
        }
        let dest_name = temp_file_name(artifact_name, session_id, turn_number);
        let dest_path = self.queue_dir.join(&dest_name);
        move_or_copy_to_queue(source_path, &dest_path, &self.queue_dir, &self.stats)?;
        let original_size = file_size(&dest_path);
        let (tx, rx) = oneshot::channel();
        let item = UploadQueueItem {
            source: UploadSource::OwnedTemp(dest_path),
            gcs_path: gcs_path.to_string(),
            content_type: content_type.to_string(),
            artifact_name: artifact_name.to_string(),
            attempts: 0,
            enqueued_at: Instant::now(),
            sidecar_path: None,
            completion_tx: Some(tx),
            client_version: self.client_version.clone(),
            compress,
            parent_span: tracing::Span::current(),
            _in_flight: in_flight,
        };
        self.stats.pending.fetch_add(1, Ordering::Relaxed);
        self.stats
            .pending_bytes
            .fetch_add(original_size, Ordering::Relaxed);
        self.stats.enqueued.fetch_add(1, Ordering::Relaxed);
        if let Err(e) = self.tx.send(item).await {
            let rejected = e.0;
            remove_owned_source(&rejected.source, Some(&self.stats));
            self.stats.pending.fetch_sub(1, Ordering::Relaxed);
            self.stats
                .pending_bytes
                .fetch_sub(original_size, Ordering::Relaxed);
            self.stats.notify_transition();
            return Err(anyhow::anyhow!("Upload queue closed"));
        }
        Ok(EnqueueResult {
            completion_rx: rx,
            original_size,
        })
    }
    /// Enqueue a working-tree file by taking an immutable reflink/CoW snapshot of
    /// it into the queue dir, verifying that snapshot against `expected_sha256`,
    /// then uploading the snapshot (never the live source).
    ///
    /// Snapshotting at enqueue closes the verify-then-upload corruption window:
    /// verify and upload operate on the SAME bytes, so a later mutation of the
    /// working-tree file cannot poison the content-addressed object.
    ///
    /// Reflink-vs-copy disk budgeting is handled at the `snapshot_route` gate
    /// below. A stale snapshot (source changed since the manifest hash) is
    /// discarded and the completion resolves to a non-fatal `Err`. Mirrors
    /// `enqueue_file`'s channel-full/closed fallback. Returns an [`EnqueueResult`].
    pub async fn enqueue_file_reference(
        &self,
        source_path: &Path,
        expected_sha256: &str,
        gcs_path: &str,
        content_type: &str,
        artifact_name: &str,
        session_id: &str,
        turn_number: u64,
    ) -> anyhow::Result<EnqueueResult> {
        let original_size = std::fs::metadata(source_path)
            .with_context(|| {
                format!(
                    "Failed to stat {} for upload queue snapshot",
                    source_path.display()
                )
            })?
            .len();
        let (tx, rx) = oneshot::channel();
        let in_flight = if is_content_addressed(gcs_path) {
            match self.mark_in_flight(gcs_path) {
                Some(guard) => Some(guard),
                None => {
                    let _ = tx.send(Err(anyhow::anyhow!(
                        "deduplicated: identical gcs_path already in flight"
                    )));
                    return Ok(EnqueueResult {
                        completion_rx: rx,
                        original_size,
                    });
                }
            }
        } else {
            None
        };
        let snapshot = self
            .queue_dir
            .join(temp_file_name(artifact_name, session_id, turn_number));
        let disk_bytes = match reflink_copy::reflink_or_copy(source_path, &snapshot) {
            Ok(copied) => copied.unwrap_or(0),
            Err(e) => {
                return Err(anyhow::Error::new(e).context(format!(
                    "Failed to snapshot {} into upload queue",
                    source_path.display()
                )));
            }
        };
        match check_snapshot(&snapshot, expected_sha256) {
            SnapshotCheck::Match => {}
            SnapshotCheck::Stale => {
                try_remove_temp(&snapshot, Some(&self.stats));
                self.stats.reference_stale.fetch_add(1, Ordering::Relaxed);
                let _ = tx.send(Err(anyhow::anyhow!(
                    "reference snapshot did not match expected sha256; upload skipped"
                )));
                return Ok(EnqueueResult {
                    completion_rx: rx,
                    original_size,
                });
            }
            SnapshotCheck::Io(e) => {
                try_remove_temp(&snapshot, Some(&self.stats));
                self.stats.failed.fetch_add(1, Ordering::Relaxed);
                let _ = tx.send(Err(e));
                return Ok(EnqueueResult {
                    completion_rx: rx,
                    original_size,
                });
            }
        }
        tracing::debug!(
            session_id,
            turn_number,
            gcs_path,
            size_bytes = original_size,
            disk_bytes,
            reflinked = disk_bytes == 0,
            "Enqueueing reference snapshot upload"
        );
        if snapshot_route(disk_bytes, self.over_disk_budget(disk_bytes))
            == SnapshotRoute::InlineFallback
        {
            self.stats.enqueue_fallbacks.fetch_add(1, Ordering::Relaxed);
            self.spawn_inline_upload_owned_snapshot(
                snapshot,
                gcs_path.to_string(),
                content_type.to_string(),
                original_size,
                Some(tx),
            );
            return Ok(EnqueueResult {
                completion_rx: rx,
                original_size,
            });
        }
        let item = UploadQueueItem {
            source: UploadSource::OwnedSnapshot {
                path: snapshot,
                disk_bytes,
            },
            gcs_path: gcs_path.to_string(),
            content_type: content_type.to_string(),
            artifact_name: artifact_name.to_string(),
            attempts: 0,
            enqueued_at: Instant::now(),
            sidecar_path: None,
            completion_tx: Some(tx),
            client_version: self.client_version.clone(),
            compress: false,
            parent_span: tracing::Span::current(),
            _in_flight: in_flight,
        };
        self.stats.pending.fetch_add(1, Ordering::Relaxed);
        self.stats
            .pending_bytes
            .fetch_add(disk_bytes, Ordering::Relaxed);
        self.stats.enqueued.fetch_add(1, Ordering::Relaxed);
        if let Err(e) = self.tx.try_send(item) {
            if matches!(&e, mpsc::error::TrySendError::Closed(_)) {
                tracing::debug!("Upload queue closed, falling back to inline snapshot upload");
            }
            let rejected = e.into_inner();
            self.stats.pending.fetch_sub(1, Ordering::Relaxed);
            self.stats
                .pending_bytes
                .fetch_sub(disk_bytes, Ordering::Relaxed);
            self.stats.notify_transition();
            self.stats.enqueue_fallbacks.fetch_add(1, Ordering::Relaxed);
            self.spawn_inline_upload_owned_snapshot(
                rejected.source.path().to_path_buf(),
                gcs_path.to_string(),
                content_type.to_string(),
                original_size,
                rejected.completion_tx,
            );
        }
        Ok(EnqueueResult {
            completion_rx: rx,
            original_size,
        })
    }
    /// Bounded, NON-terminal flush: wait until every queued item has reached a
    /// terminal outcome (`pending == 0`) or `timeout` elapses, and return the
    /// remaining pending count (0 = flushed). Unlike [`Self::drain`] the
    /// worker keeps running either way, so later enqueues proceed normally —
    /// this is the per-turn flush; `drain` is for process shutdown.
    ///
    /// `pending == 0` means every accepted item settled (uploaded, or dropped
    /// by retry/terminal policy); it does not cover inline-fallback tasks,
    /// which leave `pending` at spawn.
    pub async fn wait_idle(&self, timeout: Duration) -> usize {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let notified = self.stats.idle_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let pending = self.stats.pending.load(Ordering::Relaxed) as usize;
            if pending == 0 {
                return 0;
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return pending;
            }
            let slice = deadline.min(now + Duration::from_millis(250));
            tokio::select! {
                _ = notified => {}
                _ = tokio::time::sleep_until(slice) => {}
            }
        }
    }
    /// Drain remaining items with a deadline. Called on graceful shutdown.
    ///
    /// Signals the worker to stop accepting new items, process all remaining
    /// channel items, and wait for in-flight uploads to complete.
    /// Returns 0 on success, or the pending count if the deadline is exceeded.
    /// On timeout the worker task is aborted, which also aborts any still-running
    /// upload tasks (they live in the worker's `JoinSet`); their artifacts stay
    /// on disk for next-session orphan recovery.
    /// Double drain is a no-op (returns 0).
    pub async fn drain(&self, deadline: Duration) -> usize {
        let span = tracing::info_span!(
            "upload_queue.drain",
            deadline_secs = deadline.as_secs(),
            remaining = tracing::field::Empty,
            outcome = tracing::field::Empty,
        );
        async {
            let current_span = tracing::Span::current();
            let state = self
                .drain_state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            let Some(state) = state else {
                current_span.record("outcome", "noop");
                current_span.record("remaining", 0usize);
                return 0;
            };
            let _ = state.shutdown_tx.send(());
            let handle = state.worker_handle;
            tokio::pin!(handle);
            match tokio::time::timeout(deadline, &mut handle).await {
                Ok(Ok(())) => {
                    current_span.record("outcome", "completed");
                    current_span.record("remaining", 0usize);
                    0
                }
                Ok(Err(e)) => {
                    let remaining = self.stats.pending.load(Ordering::Relaxed) as usize;
                    current_span.record("outcome", "panicked");
                    current_span.record("remaining", remaining);
                    tracing::warn!(error = %e, "Upload queue worker panicked during drain");
                    remaining
                }
                Err(_) => {
                    let remaining = self.stats.pending.load(Ordering::Relaxed) as usize;
                    current_span.record("outcome", "timed_out");
                    current_span.record("remaining", remaining);
                    tracing::debug!("Upload queue drain timed out");
                    handle.abort();
                    remaining
                }
            }
        }
        .instrument(span)
        .await
    }
    /// Current queue statistics.
    pub fn stats(&self) -> &UploadQueueStats {
        &self.stats
    }
    /// Get a shared reference to the stats Arc for cross-component sharing.
    ///
    /// Used to pass the stats to the feedback manager's periodic signal sync,
    /// which snapshots upload queue metrics into the session signals.
    pub fn stats_arc(&self) -> Arc<UploadQueueStats> {
        self.stats.clone()
    }
    /// Clean up orphaned entries from previous sessions.
    ///
    /// Called at startup to remove files and directories older than `max_age`
    /// that were left behind by crashes or ungraceful shutdowns. Deleted lone
    /// queue files (temp without sidecar, or vice versa) are counted in
    /// `cleanup_orphan_mismatched`.
    pub fn cleanup_orphans(&self, max_age: Duration) {
        cleanup_queue_dir(&self.queue_dir, max_age, Some(&self.stats));
    }
    fn write_temp_file(
        &self,
        content: &[u8],
        artifact_name: &str,
        session_id: &str,
        turn_number: u64,
    ) -> anyhow::Result<PathBuf> {
        let name = temp_file_name(artifact_name, session_id, turn_number);
        let path = self.queue_dir.join(name);
        std::fs::write(&path, content)
            .with_context(|| format!("Failed to write temp file {}", path.display()))?;
        Ok(path)
    }
    /// Write the [`QueueItemSidecar`] manifest for `temp_path` atomically
    /// (write `<final>.tmp` → fsync → rename). Only the manifest is written
    /// atomically — the temp file itself is a plain write; that asymmetry is
    /// fine because recovery re-hashes the temp bytes and drops the pair on a
    /// `sha256` mismatch, so a torn temp is detected rather than re-uploaded.
    fn write_sidecar_file(
        &self,
        temp_path: &Path,
        content: &[u8],
        gcs_path: &str,
        content_type: &str,
        artifact_name: &str,
        session_id: &str,
        turn_number: u64,
    ) -> anyhow::Result<PathBuf> {
        let sidecar = QueueItemSidecar {
            schema_version: QUEUE_ITEM_SIDECAR_SCHEMA_VERSION,
            session_id: session_id.to_string(),
            turn_number,
            gcs_path: gcs_path.to_string(),
            content_type: content_type.to_string(),
            artifact_name: artifact_name.to_string(),
            enqueued_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            sha256: crate::sha256_hex(content),
        };
        let json =
            serde_json::to_vec_pretty(&sidecar).context("serialize queue item sidecar manifest")?;
        let final_path = sidecar_path_for(temp_path);
        write_atomic(&final_path, &json)?;
        Ok(final_path)
    }
    fn over_disk_budget(&self, additional_bytes: u64) -> bool {
        self.stats.pending_bytes.load(Ordering::Relaxed) + additional_bytes > self.max_queue_bytes
    }
    /// Inline-upload fallback for `enqueue_file_blocking` when over the disk
    /// budget. Streams from `source_path` via `upload_file` and resolves the
    /// caller's `oneshot`. Always uncompressed. Streaming from disk means the
    /// byte semaphore bounds both resident memory and upload concurrency.
    fn spawn_inline_upload_blocking(
        &self,
        source_path: PathBuf,
        gcs_path: String,
        content_type: String,
        original_size: u64,
        completion_tx: oneshot::Sender<anyhow::Result<UploadCompletion>>,
    ) {
        use tracing::Instrument;
        let resolver = self.resolver.clone();
        let semaphore = self.inline_fallback_semaphore.clone();
        let permits = inline_fallback_permits(original_size);
        let parent_span = tracing::Span::current();
        tokio::spawn(
            async move {
                let _permit = semaphore
                    .acquire_many_owned(permits)
                    .await
                    .map_err(|e| {
                        tracing::warn!(error = %e, "inline-fallback semaphore closed; proceeding ungated")
                    })
                    .ok();
                let wrapped = ResolvedStorageConfig::from_resolver_async(&resolver)
                    .await;
                let result = match upload_file(
                        &wrapped,
                        &gcs_path,
                        &source_path,
                        &content_type,
                    )
                    .await
                {
                    Ok(url) => {
                        Ok(UploadCompletion {
                            gcs_url: url,
                            compression: BlobCompression::None,
                            original_size,
                            stored_size: original_size,
                        })
                    }
                    Err(e) => {
                        tracing::warn!(gcs_path, error = %e, "Inline blocking upload failed");
                        Err(e)
                    }
                };
                let _ = completion_tx.send(result);
            }
                .instrument(parent_span),
        );
    }
    /// Inline fallback for `enqueue_file_reference` when the channel is full /
    /// closed or an over-budget copy-fallback snapshot must not accumulate in the
    /// queue. Streams the queue-OWNED snapshot via `upload_file` (bounded by the
    /// byte-budget semaphore), resolves `completion_tx`, and ALWAYS deletes the
    /// snapshot afterward.
    fn spawn_inline_upload_owned_snapshot(
        &self,
        snapshot: PathBuf,
        gcs_path: String,
        content_type: String,
        original_size: u64,
        completion_tx: Option<oneshot::Sender<anyhow::Result<UploadCompletion>>>,
    ) {
        use tracing::Instrument;
        let resolver = self.resolver.clone();
        let semaphore = self.inline_fallback_semaphore.clone();
        let stats = self.stats.clone();
        let permits = inline_fallback_permits(original_size);
        let parent_span = tracing::Span::current();
        tokio::spawn(
            async move {
                let _permit = semaphore
                    .acquire_many_owned(permits)
                    .await
                    .map_err(|e| {
                        tracing::warn!(error = %e, "inline-fallback semaphore closed; proceeding ungated")
                    })
                    .ok();
                let wrapped = ResolvedStorageConfig::from_resolver_async(&resolver)
                    .await;
                let result = match upload_file(
                        &wrapped,
                        &gcs_path,
                        &snapshot,
                        &content_type,
                    )
                    .await
                {
                    Ok(url) => {
                        Ok(UploadCompletion {
                            gcs_url: url,
                            compression: BlobCompression::None,
                            original_size,
                            stored_size: original_size,
                        })
                    }
                    Err(e) => {
                        tracing::warn!(gcs_path, error = %e, "Inline snapshot fallback upload failed");
                        Err(e)
                    }
                };
                try_remove_temp(&snapshot, Some(&stats));
                if let Some(tx) = completion_tx {
                    let _ = tx.send(result);
                }
            }
                .instrument(parent_span),
        );
    }
    /// Fire-and-forget inline fallback for `enqueue_file` (over-budget /
    /// channel-full), streaming from `source_path` via `upload_file` (multipart
    /// for large files) rather than reading the file into memory. Streaming from
    /// disk means the byte semaphore bounds both resident memory and concurrency.
    fn spawn_inline_upload_from_path(
        &self,
        source_path: PathBuf,
        gcs_path: String,
        content_type: String,
        size: u64,
    ) {
        use tracing::Instrument;
        let resolver = self.resolver.clone();
        let semaphore = self.inline_fallback_semaphore.clone();
        let permits = inline_fallback_permits(size);
        let parent_span = tracing::Span::current();
        tokio::spawn(
            async move {
                let _permit = semaphore
                    .acquire_many_owned(permits)
                    .await
                    .map_err(|e| {
                        tracing::warn!(error = %e, "inline-fallback semaphore closed; proceeding ungated")
                    })
                    .ok();
                let wrapped = ResolvedStorageConfig::from_resolver_async(&resolver)
                    .await;
                if let Err(e) = upload_file(
                        &wrapped,
                        &gcs_path,
                        &source_path,
                        &content_type,
                    )
                    .await
                {
                    tracing::warn!(gcs_path, error = %e, "Inline fallback upload failed");
                }
            }
                .instrument(parent_span),
        );
    }
    /// Fire-and-forget inline fallback for the bytes-based `enqueue`
    /// (over-budget / channel-full). The owned `Vec` must be allocated before the
    /// spawn (the borrow can't cross it), so the semaphore bounds only upload
    /// concurrency, not memory — acceptable because this path carries only small
    /// in-memory artifacts; multi-GB files use the path-streaming variants above.
    fn spawn_inline_upload(&self, content: &[u8], gcs_path: &str, content_type: &str) {
        use tracing::Instrument;
        let resolver = self.resolver.clone();
        let semaphore = self.inline_fallback_semaphore.clone();
        let permits = inline_fallback_permits(content.len() as u64);
        let content = content.to_vec();
        let gcs_path = gcs_path.to_string();
        let content_type = content_type.to_string();
        let parent_span = tracing::Span::current();
        tokio::spawn(
            async move {
                let _permit = semaphore
                    .acquire_many_owned(permits)
                    .await
                    .map_err(|e| {
                        tracing::warn!(error = %e, "inline-fallback semaphore closed; proceeding ungated")
                    })
                    .ok();
                let wrapped = ResolvedStorageConfig::from_resolver_async(&resolver)
                    .await;
                if let Err(e) = upload_bytes(
                        &wrapped,
                        &gcs_path,
                        &content,
                        &content_type,
                    )
                    .await
                {
                    tracing::warn!(gcs_path, error = %e, "Inline fallback upload failed");
                }
            }
                .instrument(parent_span),
        );
    }
}
/// A worker concurrency slot paired with its semaphore so a parked item can
/// release the slot (parking does zero wire I/O) and re-acquire it before
/// resuming. Without release, `max_concurrent` parked items would pin every
/// slot for up to `max_age` — collapsing throughput and stalling drain, since
/// the dispatch loop blocks on `acquire_owned()` and stops polling the
/// shutdown signal.
struct ConcurrencyPermit {
    semaphore: Arc<tokio::sync::Semaphore>,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
}
impl ConcurrencyPermit {
    /// Drop the held slot (no-op if already released).
    fn release(&mut self) {
        self.permit = None;
    }
    /// Re-acquire a slot, awaiting if all are currently taken (no-op if already
    /// held).
    async fn reacquire(&mut self) {
        if self.permit.is_none() {
            self.permit = Some(
                self.semaphore
                    .clone()
                    .acquire_owned()
                    .await
                    .expect("semaphore closed unexpectedly"),
            );
        }
    }
}
/// Acquire a semaphore permit and spawn the upload task for a single queue item.
async fn dispatch_item(
    item: UploadQueueItem,
    semaphore: &Arc<tokio::sync::Semaphore>,
    resolver: &Arc<dyn TraceExportSource>,
    retry_policy: &UploadRetryPolicy,
    stats: &Arc<UploadQueueStats>,
    consecutive_failures: &Arc<AtomicU32>,
    draining: &Arc<std::sync::atomic::AtomicBool>,
    tasks: &mut tokio::task::JoinSet<()>,
) {
    let permit = semaphore
        .clone()
        .acquire_owned()
        .await
        .expect("semaphore closed unexpectedly");
    let concurrency = ConcurrencyPermit {
        semaphore: semaphore.clone(),
        permit: Some(permit),
    };
    let resolver = resolver.clone();
    let retry_policy = retry_policy.clone();
    let stats = stats.clone();
    let consecutive_failures = consecutive_failures.clone();
    let draining = draining.clone();
    let span = tracing::info_span!(
        parent: item.parent_span.clone(),
        "gcs_queue_upload",
        artifact = %item.artifact_name,
        gcs_path = %item.gcs_path,
        client_version = %item.client_version.as_deref().unwrap_or("unknown"),
    );
    tasks.spawn(
        async move {
            process_item(
                item,
                &resolver,
                &retry_policy,
                &stats,
                &consecutive_failures,
                &draining,
                Some(concurrency),
            )
            .await;
        }
        .instrument(span),
    );
}
/// Hold the circuit breaker open for one [`CIRCUIT_BREAKER_COOLDOWN`] period,
/// returning `true` if a shutdown interrupted it. Sets `circuit_breaker_active`
/// on entry and always clears it before returning (even on shutdown, so the
/// gauge never stays stuck `true` while draining).
async fn circuit_breaker_cooldown(
    stats: &Arc<UploadQueueStats>,
    mut shutdown_rx: Pin<&mut oneshot::Receiver<()>>,
) -> bool {
    stats.circuit_breaker_active.store(true, Ordering::Relaxed);
    stats.notify_transition();
    let interrupted = tokio::select! {
        _ = tokio::time::sleep(CIRCUIT_BREAKER_COOLDOWN) => false,
        _ = shutdown_rx.as_mut() => {
            tracing::debug!("upload_queue.shutdown_signal");
            true
        }
    };
    stats.circuit_breaker_active.store(false, Ordering::Relaxed);
    stats.notify_transition();
    interrupted
}
/// Concurrent background worker that processes the upload queue.
///
/// Dispatches up to `max_concurrent` items in parallel using a semaphore.
/// Each item is processed in its own spawned task with an independent retry loop.
/// The circuit breaker pauses the dispatch loop (preventing new tasks from starting)
/// while in-flight tasks continue to completion.
///
/// The worker exits when either:
/// - The channel is closed (all senders dropped)
/// - A shutdown signal is received via `shutdown_rx` (from `drain()`)
///
/// On shutdown signal, the worker closes the receiver, drains all remaining
/// buffered items (bypassing the circuit breaker), and waits for all in-flight
/// tasks to complete via semaphore.
async fn upload_worker(
    mut rx: mpsc::Receiver<UploadQueueItem>,
    shutdown_rx: oneshot::Receiver<()>,
    resolver: Arc<dyn TraceExportSource>,
    retry_policy: UploadRetryPolicy,
    stats: Arc<UploadQueueStats>,
    max_concurrent: usize,
) {
    let semaphore = Arc::new(tokio::sync::Semaphore::new(max_concurrent));
    let consecutive_failures = Arc::new(AtomicU32::new(0));
    let draining_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut tasks: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
    tokio::pin!(shutdown_rx);
    let draining = loop {
        if consecutive_failures.load(Ordering::Relaxed) >= CIRCUIT_BREAKER_THRESHOLD {
            tracing::warn!(
                failures = consecutive_failures.load(Ordering::Relaxed),
                "Upload queue circuit breaker tripped, pausing dispatch"
            );
            stats.circuit_breaker_trips.fetch_add(1, Ordering::Relaxed);
            if circuit_breaker_cooldown(&stats, shutdown_rx.as_mut()).await {
                break true;
            }
            consecutive_failures.store(0, Ordering::Relaxed);
        }
        tokio::select! {
            item = rx.recv() => {
                match item {
                    Some(item) => {
                        dispatch_item(
                            item, &semaphore, &resolver, &retry_policy,
                            &stats, &consecutive_failures, &draining_flag, &mut tasks,
                        ).await;
                        // Reap finished tasks so the JoinSet doesn't grow
                        // unbounded over the worker's lifetime.
                        while tasks.try_join_next().is_some() {}
                    }
                    None => break false,
                }
            }
            _ = &mut shutdown_rx => {
                tracing::debug!("upload_queue.shutdown_signal");
                break true;
            }
        }
    };
    draining_flag.store(true, Ordering::Relaxed);
    if draining {
        rx.close();
        while let Some(item) = rx.recv().await {
            dispatch_item(
                item,
                &semaphore,
                &resolver,
                &retry_policy,
                &stats,
                &consecutive_failures,
                &draining_flag,
                &mut tasks,
            )
            .await;
        }
    }
    while tasks.join_next().await.is_some() {}
    tracing::debug!("Upload queue worker exiting (all tasks drained)");
}
/// Minimum file size to attempt compression.
const COMPRESS_MIN_BYTES: u64 = 128;
/// Wraps an `AsyncRead` and counts total bytes read through it.
struct CountingReader<R> {
    inner: R,
    bytes_read: Arc<AtomicU64>,
}
impl<R: AsyncRead + Unpin> AsyncRead for CountingReader<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let result = Pin::new(&mut this.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &result {
            let n = buf.filled().len() - before;
            this.bytes_read.fetch_add(n as u64, Ordering::Relaxed);
        }
        result
    }
}
/// Outcome of verifying a freshly-taken snapshot against `expected_sha256`.
enum SnapshotCheck {
    /// Content matches — safe to upload.
    Match,
    /// Hash mismatch or the snapshot vanished (NotFound): the source changed
    /// between the manifest hash and enqueue. Skip as stale.
    Stale,
    /// A transient read error while hashing our own fresh snapshot — a hard
    /// (non-stale) failure; must NOT be attributed to `reference_stale`.
    Io(anyhow::Error),
}
/// Where a verified reference snapshot should go.
#[derive(Debug, PartialEq, Eq)]
enum SnapshotRoute {
    /// Enqueue normally (a reflink, or a copy that fits the disk budget).
    Queue,
    /// Over-budget real copy — upload inline (bounded) instead of letting it
    /// accumulate in the queue.
    InlineFallback,
}
/// Reflink snapshots (`disk_bytes == 0`, ~0 real disk) always queue; only a real
/// copy that would exceed the budget routes to the bounded inline fallback.
fn snapshot_route(disk_bytes: u64, over_budget: bool) -> SnapshotRoute {
    if disk_bytes > 0 && over_budget {
        SnapshotRoute::InlineFallback
    } else {
        SnapshotRoute::Queue
    }
}
/// Verify the (immutable) snapshot at `path`. Streamed in 8 KB chunks via the
/// shared `sha256_hex_from_file` — never a whole-file read, so multi-GB
/// snapshots stay off the heap. Distinguishes a genuine mismatch/missing
/// (→ `Stale`) from a transient read error (→ `Io`).
fn check_snapshot(path: &Path, expected_sha256: &str) -> SnapshotCheck {
    match crate::sha256_hex_from_file(path, None) {
        Ok(actual) if actual == expected_sha256 => SnapshotCheck::Match,
        Ok(_) => SnapshotCheck::Stale,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => SnapshotCheck::Stale,
        Err(e) => SnapshotCheck::Io(
            anyhow::Error::new(e).context(format!("Failed to hash snapshot {}", path.display())),
        ),
    }
}
/// Settle an item leaving the queue: drop `inflight` FIRST (so it never
/// exceeds `pending`), then `pending`/`pending_bytes`, then notify.
fn settle_pending(stats: &UploadQueueStats, accounted_bytes: u64) {
    stats.inflight.fetch_sub(1, Ordering::Relaxed);
    stats.pending.fetch_sub(1, Ordering::Relaxed);
    stats
        .pending_bytes
        .fetch_sub(accounted_bytes, Ordering::Relaxed);
    stats.notify_transition();
}
/// Process a single upload queue item: age check, upload with retries, optional streaming compression.
async fn process_item(
    mut item: UploadQueueItem,
    resolver: &Arc<dyn TraceExportSource>,
    retry_policy: &UploadRetryPolicy,
    stats: &Arc<UploadQueueStats>,
    consecutive_failures: &Arc<AtomicU32>,
    draining: &Arc<std::sync::atomic::AtomicBool>,
    mut permit: Option<ConcurrencyPermit>,
) {
    let size = file_size(item.source.path());
    let accounted_bytes = item.source.disk_bytes(size);
    stats.inflight.fetch_add(1, Ordering::Relaxed);
    stats.notify_transition();
    if item.enqueued_at.elapsed() > retry_policy.max_age {
        tracing::warn!(
            age_secs = item.enqueued_at.elapsed().as_secs(),
            outcome = "expired",
            "Dropping expired upload queue item"
        );
        remove_item_files(&item, Some(stats));
        stats.failed.fetch_add(1, Ordering::Relaxed);
        settle_pending(stats, accounted_bytes);
        notify_completion(&mut item, Err(anyhow::anyhow!("expired")));
        return;
    }
    let result = upload_with_retries(
        &mut item,
        resolver,
        retry_policy,
        size,
        stats,
        draining,
        permit.as_mut(),
    )
    .await;
    match result {
        Ok((url, compression, stored_size)) => {
            let compressed = matches!(compression, BlobCompression::Zstd);
            tracing::info!(
                attempts = item.attempts,
                size_bytes = size,
                compressed,
                outcome = "success",
                "GCS queue upload completed"
            );
            consecutive_failures.store(0, Ordering::Relaxed);
            remove_item_files(&item, Some(stats));
            stats.uploaded.fetch_add(1, Ordering::Relaxed);
            notify_completion(
                &mut item,
                Ok(UploadCompletion {
                    gcs_url: url,
                    compression,
                    original_size: size,
                    stored_size,
                }),
            );
        }
        Err(e) => {
            let terminal = matches!(upload_disposition(&e), Disposition::Terminal);
            if !terminal {
                consecutive_failures.fetch_add(1, Ordering::Relaxed);
            }
            tracing::warn!(
                attempts = item.attempts,
                size_bytes = size,
                outcome = if terminal { "dropped" } else { "exhausted" },
                error = ?e,
                "Upload queue item failed permanently"
            );
            remove_item_files(&item, Some(stats));
            stats.failed.fetch_add(1, Ordering::Relaxed);
            notify_completion(&mut item, Err(e));
        }
    }
    settle_pending(stats, accounted_bytes);
}
/// Shared status-code classifier for the storage upload queue.
const STORAGE_RETRY_POLICY: RetryPolicy = RetryPolicy::client_storage();
/// Returns `true` if the error indicates an HTTP 401 or 403 response.
///
/// These auth errors will never succeed with the same request — retrying
/// wastes time and generates log noise. This is the direct-mode (`gcloud-storage`)
/// string fallback: direct-mode errors are unstructured anyhow messages, so we
/// scrape for 401/403. Proxy-mode errors carry a structured `HttpUploadError`
/// and are classified by status code in `upload_disposition`.
fn is_non_retryable_error(error: &anyhow::Error) -> bool {
    let msg = format!("{:#}", error);
    msg.contains("HTTP 401")
        || msg.contains("HTTP 403")
        || msg.contains("401 Unauthorized")
        || msg.contains("403 Forbidden")
}
/// Disposition for a failed storage upload. Proxy-mode errors carry a
/// structured `HttpUploadError` and are classified by the shared
/// `RetryPolicy`; direct-mode (gcloud) errors are unstructured strings, so
/// 401/403 are detected by message scraping as a safety net.
fn upload_disposition(error: &anyhow::Error) -> Disposition {
    if let Some(http) = error.downcast_ref::<HttpUploadError>() {
        return STORAGE_RETRY_POLICY
            .classify(http.status_code)
            .unwrap_or(Disposition::Retryable);
    }
    if is_non_retryable_error(error) {
        return Disposition::AuthRefresh;
    }
    Disposition::Retryable
}
/// Park-loop iteration granularity: bounds how long a parked task takes to
/// notice `draining` / `max_age`.
const AUTH_PARK_WAIT_INTERVAL: Duration = Duration::from_secs(5);
/// Upload with retries, exponential backoff, and credential refresh.
///
/// On each attempt, resolves fresh credentials from the resolver and uploads the
/// queue-owned temp/snapshot via `upload_file` (which streams from disk on every
/// backend and keeps the multipart / signed-URL path for large files), or, for
/// compressible owned temps, streams through a zstd encoder. Snapshots are
/// immutable and already verified at enqueue, so the worker just uploads them.
///
/// On a terminal status (400/403/404, origin-TLS 525/526), aborts immediately.
/// On 401, re-resolves credentials and
/// retries once; if the retry also 401s, the item parks until auth recovers
/// (releasing its concurrency permit while parked) rather than dropping.
async fn upload_with_retries(
    item: &mut UploadQueueItem,
    resolver: &Arc<dyn TraceExportSource>,
    policy: &UploadRetryPolicy,
    original_size: u64,
    stats: &Arc<UploadQueueStats>,
    draining: &Arc<std::sync::atomic::AtomicBool>,
    mut permit: Option<&mut ConcurrencyPermit>,
) -> anyhow::Result<(String, BlobCompression, u64)> {
    let should_compress = item.compress && original_size >= COMPRESS_MIN_BYTES;
    let mut auth_retried = false;
    let mut parked = false;
    loop {
        item.attempts += 1;
        let wrapped = ResolvedStorageConfig::from_resolver_async(resolver).await;
        let last_wire_attempt = Instant::now();
        let attempt_bearer = wrapped.wire_bearer();
        let result = if should_compress {
            stream_compress_upload(&wrapped, &item.gcs_path, item.source.path()).await
        } else {
            upload_file(
                &wrapped,
                &item.gcs_path,
                item.source.path(),
                &item.content_type,
            )
            .await
            .map(|url| (url, BlobCompression::None, original_size))
        };
        match result {
            Ok(r) => {
                tracing::debug!(attempt = item.attempts, "Upload queue item succeeded");
                return Ok(r);
            }
            Err(e) => match upload_disposition(&e) {
                Disposition::Terminal => {
                    tracing::warn!(
                        attempt = item.attempts,
                        error = ?e,
                        "Storage upload failed with a terminal status; dropping artifact"
                    );
                    return Err(e);
                }
                Disposition::AuthRefresh => {
                    if !auth_retried {
                        tracing::info!(
                            attempt = item.attempts,
                            error = ?e,
                            "Auth error, re-resolving credentials for one retry"
                        );
                        auth_retried = true;
                        continue;
                    }
                    let failed_bearer = attempt_bearer;
                    if let Some(p) = permit.as_deref_mut() {
                        p.release();
                    }
                    let mut wake = false;
                    loop {
                        if draining.load(Ordering::Relaxed) {
                            tracing::warn!(
                                attempt = item.attempts,
                                parked,
                                "Auth error persists and queue is draining, aborting"
                            );
                            return Err(e);
                        }
                        if wake {
                            if item.enqueued_at.elapsed() >= policy.max_age {
                                tracing::warn!(
                                    attempt = item.attempts,
                                    age_secs = item.enqueued_at.elapsed().as_secs(),
                                    "Parked item exceeded max_age waiting for auth recovery, aborting"
                                );
                                return Err(e);
                            }
                            break;
                        }
                        let Some(wait) = resolver.wait_for_auth_recovery(
                            failed_bearer.as_deref(),
                            AUTH_PARK_WAIT_INTERVAL,
                        ) else {
                            tracing::warn!(
                                attempt = item.attempts,
                                parked,
                                error = ?e,
                                "Auth error persists after credential refresh, aborting"
                            );
                            return Err(e);
                        };
                        if !parked {
                            parked = true;
                            stats.auth_parked.fetch_add(1, Ordering::Relaxed);
                            tracing::warn!(
                                attempt = item.attempts,
                                gcs_path = %item.gcs_path,
                                "401 persists after credential refresh; parking item until auth recovers"
                            );
                            notify_completion(
                                item,
                                Err(anyhow::anyhow!(
                                    "upload parked: credentials rejected (HTTP 401); \
                                     retrying in background until auth recovers"
                                )),
                            );
                        }
                        if item.enqueued_at.elapsed() >= policy.max_age {
                            tracing::warn!(
                                attempt = item.attempts,
                                age_secs = item.enqueued_at.elapsed().as_secs(),
                                "Parked item exceeded max_age waiting for auth recovery, aborting"
                            );
                            return Err(e);
                        }
                        wake = wait.await
                            || (last_wire_attempt.elapsed() >= policy.auth_park_probe_interval
                                && resolver.has_usable_credential());
                    }
                    if let Some(p) = permit.as_deref_mut() {
                        p.reacquire().await;
                    }
                    auth_retried = false;
                    continue;
                }
                Disposition::Retryable => {
                    if item.attempts >= policy.max_attempts {
                        return Err(e);
                    }
                    let delay = policy.backoff_delay(item.attempts - 1);
                    tracing::debug!(
                        attempt = item.attempts,
                        delay_ms = delay.as_millis() as u64,
                        error = ?e,
                        "Upload queue item failed, retrying"
                    );
                    tokio::time::sleep(delay).await;
                }
            },
        }
    }
}
/// Open file, wrap in streaming zstd encoder with byte counter, upload to cloud storage.
async fn stream_compress_upload<C: StorageConfig>(
    config: &C,
    gcs_path: &str,
    file_path: &Path,
) -> anyhow::Result<(String, BlobCompression, u64)> {
    let file = tokio::fs::File::open(file_path)
        .await
        .with_context(|| format!("Failed to open {} for compression", file_path.display()))?;
    let reader = tokio::io::BufReader::new(file);
    let encoder = ZstdEncoder::new(reader);
    let bytes_written = Arc::new(AtomicU64::new(0));
    let counting = CountingReader {
        inner: encoder,
        bytes_read: bytes_written.clone(),
    };
    let url = upload_stream(config, gcs_path, counting, "application/zstd").await?;
    Ok((
        url,
        BlobCompression::Zstd,
        bytes_written.load(Ordering::Relaxed),
    ))
}
/// Send completion signal if a block_for_upload caller is waiting.
fn notify_completion(item: &mut UploadQueueItem, result: anyhow::Result<UploadCompletion>) {
    if let Some(tx) = item.completion_tx.take() {
        let _ = tx.send(result);
    }
}
/// Generate a unique temp file name for a queued artifact.
///
/// Includes a random suffix to avoid collisions when multiple blobs with the
/// same SHA256 prefix are enqueued within the same millisecond.
fn temp_file_name(artifact_name: &str, session_id: &str, turn_number: u64) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let short_id = if session_id.len() > 8 {
        &session_id[session_id.len() - 8..]
    } else {
        session_id
    };
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        "{}_turn{}_{}_{}_{}",
        short_id, turn_number, artifact_name, ts, seq
    )
}
/// Filename suffix of a [`QueueItemSidecar`] manifest (`<temp>.meta.json`).
pub const SIDECAR_SUFFIX: &str = ".meta.json";
/// Sidecar manifest path for a queue temp file: `<temp>.meta.json`. The suffix
/// is appended (not an extension swap) because temp file names already contain
/// dots that `Path::with_extension` would mangle.
pub fn sidecar_path_for(temp_path: &Path) -> PathBuf {
    let mut name = temp_path.as_os_str().to_owned();
    name.push(SIDECAR_SUFFIX);
    PathBuf::from(name)
}
/// Inverse of [`sidecar_path_for`]: the temp file a sidecar describes, or
/// `None` if `sidecar` does not carry the [`SIDECAR_SUFFIX`].
pub fn temp_path_for_sidecar(sidecar: &Path) -> Option<PathBuf> {
    let name = sidecar.file_name()?.to_str()?;
    let stem = name.strip_suffix(SIDECAR_SUFFIX)?;
    Some(sidecar.with_file_name(stem))
}
/// Write `bytes` to `path` atomically: write to `<path>.tmp`, fsync, then
/// rename over `path`. A crash mid-write leaves at most a `<path>.tmp` partial
/// (swept by the orphan janitor), never a torn `path`.
fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp_path = PathBuf::from(tmp_name);
    {
        let mut file = std::fs::File::create(&tmp_path)
            .with_context(|| format!("Failed to create {}", tmp_path.display()))?;
        file.write_all(bytes)
            .with_context(|| format!("Failed to write {}", tmp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("Failed to fsync {}", tmp_path.display()))?;
    }
    std::fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "Failed to rename {} -> {}",
            tmp_path.display(),
            path.display()
        )
    })?;
    Ok(())
}
/// Get file size, returning 0 if the file doesn't exist.
fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}
fn copy_to_queue(source: &Path, dest: &Path) -> anyhow::Result<()> {
    std::fs::copy(source, dest)
        .with_context(|| format!("Failed to copy {} to queue", source.display()))?;
    Ok(())
}
/// Cheap rename if both paths are in `same_dir_hint`; else copy. On rename
/// failure in the same-dir case, copies then removes source via `try_remove_temp`.
fn move_or_copy_to_queue(
    source: &Path,
    dest: &Path,
    same_dir_hint: &Path,
    stats: &UploadQueueStats,
) -> anyhow::Result<()> {
    move_or_copy_to_queue_with(
        source,
        dest,
        same_dir_hint,
        stats,
        |s, d| std::fs::rename(s, d),
        copy_to_queue,
    )
}
/// Test harness for `move_or_copy_to_queue` with injectable rename/copy fns.
fn move_or_copy_to_queue_with(
    source: &Path,
    dest: &Path,
    same_dir_hint: &Path,
    stats: &UploadQueueStats,
    rename_fn: impl Fn(&Path, &Path) -> std::io::Result<()>,
    copy_fn: impl Fn(&Path, &Path) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    if source.parent() == Some(same_dir_hint) {
        match rename_fn(source, dest) {
            Ok(()) => return Ok(()),
            Err(e) => {
                tracing::warn!(
                    source = %source.display(),
                    error = %e,
                    "rename within queue_dir failed; falling back to copy + remove"
                );
                copy_fn(source, dest)?;
                try_remove_temp(source, Some(stats));
                return Ok(());
            }
        }
    }
    copy_fn(source, dest)
}
static LAST_ORPHANS_CLEANED: AtomicU64 = AtomicU64::new(0);
/// Number of orphaned entries cleaned by the last `cleanup_orphaned_uploads` call.
pub fn last_orphans_cleaned() -> u64 {
    LAST_ORPHANS_CLEANED.load(Ordering::Relaxed)
}
/// Clean up orphaned upload queue entries from previous sessions.
///
/// Called at agent startup to remove files and directories older than `max_age`
/// that were left behind by crashes or ungraceful shutdowns. Returns the number
/// of entries removed.
pub fn cleanup_orphaned_uploads(echo_agent_home: &Path, max_age: Duration) -> u64 {
    let cleaned = cleanup_queue_dir(&echo_agent_home.join("upload_queue"), max_age, None);
    LAST_ORPHANS_CLEANED.store(cleaned, Ordering::Relaxed);
    cleaned
}
/// Sweep entries older than `max_age`. `scratch/` is treated specially:
/// recurse one level so per-session subdirs are aged independently (its own
/// mtime stays fresh as new sessions land). `scratch/` itself is preserved.
///
/// When `stats` is `Some`, each deleted lone queue file (temp without sidecar
/// or vice versa) bumps `cleanup_orphan_mismatched`. Pairing is decided against
/// a name snapshot taken before any deletion, so the count is independent of
/// visit order.
fn cleanup_queue_dir(queue_dir: &Path, max_age: Duration, stats: Option<&UploadQueueStats>) -> u64 {
    let entries: Vec<std::fs::DirEntry> = match std::fs::read_dir(queue_dir) {
        Ok(e) => e.flatten().collect(),
        Err(_) => return 0,
    };
    let all_names: HashSet<std::ffi::OsString> = entries.iter().map(|e| e.file_name()).collect();
    let mut cleaned = 0u64;
    let mut cleaned_bytes = 0u64;
    for entry in &entries {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let path = entry.path();
        let name = entry.file_name();
        let is_scratch_root = metadata.is_dir() && name == "scratch";
        if is_scratch_root {
            let (sub_cleaned, sub_bytes) = cleanup_scratch_subdirs(&path, max_age);
            cleaned += sub_cleaned;
            cleaned_bytes += sub_bytes;
            continue;
        }
        let age = pair_age(&path, &name, &all_names).unwrap_or_else(|| {
            metadata
                .modified()
                .ok()
                .and_then(|m| m.elapsed().ok())
                .unwrap_or(Duration::MAX)
        });
        if age <= max_age {
            continue;
        }
        if metadata.is_dir() {
            let size = dir_size(&path).unwrap_or(0);
            if std::fs::remove_dir_all(&path).is_ok() {
                cleaned += 1;
                cleaned_bytes += size;
            }
        } else if std::fs::remove_file(&path).is_ok() {
            cleaned += 1;
            cleaned_bytes += metadata.len();
            if let Some(stats) = stats
                && is_mismatched_queue_file(&name, &all_names)
            {
                stats
                    .cleanup_orphan_mismatched
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    if cleaned > 0 {
        tracing::info!(
            cleaned,
            cleaned_bytes,
            dir = %queue_dir.display(),
            "Cleaned up orphaned upload queue entries from previous session"
        );
    }
    cleaned
}
/// True when `name` is a queue file whose temp↔sidecar partner is absent from
/// `all_names`.
/// Age of a queue file derived from its (or its companion's) sidecar
/// `enqueued_at`, or `None` when the file has no parseable sidecar — the
/// caller then falls back to mtime. Future-dated timestamps (clock skew) map
/// to `Duration::ZERO` so skew never expires live data.
fn pair_age(
    path: &Path,
    name: &std::ffi::OsStr,
    all_names: &HashSet<std::ffi::OsString>,
) -> Option<Duration> {
    let name_str = name.to_string_lossy();
    let sidecar_path = if name_str.ends_with(SIDECAR_SUFFIX) {
        path.to_path_buf()
    } else {
        let companion = format!("{name_str}{SIDECAR_SUFFIX}");
        if !all_names.contains(std::ffi::OsStr::new(companion.as_str())) {
            return None;
        }
        sidecar_path_for(path)
    };
    let bytes = std::fs::read(&sidecar_path).ok()?;
    let sidecar: QueueItemSidecar = serde_json::from_slice(&bytes).ok()?;
    let dt = chrono::DateTime::parse_from_rfc3339(&sidecar.enqueued_at).ok()?;
    let enqueued: std::time::SystemTime = dt.with_timezone(&chrono::Utc).into();
    Some(
        std::time::SystemTime::now()
            .duration_since(enqueued)
            .unwrap_or(Duration::ZERO),
    )
}
fn is_mismatched_queue_file(
    name: &std::ffi::OsStr,
    all_names: &HashSet<std::ffi::OsString>,
) -> bool {
    let name_str = name.to_string_lossy();
    if let Some(stem) = name_str.strip_suffix(SIDECAR_SUFFIX) {
        !all_names.contains(std::ffi::OsStr::new(stem))
    } else {
        let sidecar = format!("{name_str}{SIDECAR_SUFFIX}");
        !all_names.contains(std::ffi::OsStr::new(sidecar.as_str()))
    }
}
/// Reap `scratch/<sid>/` subdirs older than `max_age`. Returns
/// `(removed_count, removed_bytes)`.
///
/// Assumes `scratch/<sid>/` is flat: a nested layer would mask in-use
/// directories from the mtime check. Generalise to recursive probing when
/// that assumption changes.
fn cleanup_scratch_subdirs(scratch_dir: &Path, max_age: Duration) -> (u64, u64) {
    let entries = match std::fs::read_dir(scratch_dir) {
        Ok(e) => e,
        Err(_) => return (0, 0),
    };
    let mut cleaned = 0u64;
    let mut cleaned_bytes = 0u64;
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let age = metadata
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .unwrap_or(Duration::MAX);
        if age <= max_age {
            continue;
        }
        let path = entry.path();
        if metadata.is_dir() {
            let size = dir_size(&path).unwrap_or(0);
            if std::fs::remove_dir_all(&path).is_ok() {
                cleaned += 1;
                cleaned_bytes += size;
            }
        } else if std::fs::remove_file(&path).is_ok() {
            cleaned += 1;
            cleaned_bytes += metadata.len();
        }
    }
    (cleaned, cleaned_bytes)
}
/// Recursively compute the total size of a directory tree.
fn dir_size(path: &Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_dir() {
            total += dir_size(&entry.path())?;
        } else {
            total += meta.len();
        }
    }
    Ok(total)
}
#[cfg(test)]
#[path = "queue_tests.rs"]
mod tests;
