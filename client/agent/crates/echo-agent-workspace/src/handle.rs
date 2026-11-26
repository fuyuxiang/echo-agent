//! [`WorkspaceHandle`] -- public handle to a workspace instance.
use echo_agent_hunk_tracker::{HunkTrackerActor, HunkTrackerHandle, TrackingMode};
use echo_agent_tool_protocol::ToolServerStatusPayload;
use echo_agent_tool_protocol::turn_hook::TurnHookOutcome;
use fastrace::future::FutureExt as _;
use fastrace::local::LocalSpan;
use prometheus::{
    Histogram, HistogramVec, IntCounter, IntCounterVec, register_histogram, register_histogram_vec,
    register_int_counter, register_int_counter_vec,
};
use std::path::PathBuf;
use std::sync::Arc;
/// Default SIGTERM drain budget (ms); override via
/// `ECHO_AGENT_WORKSPACE_TERMINATION_GRACE_MS`. 45s fits under the K8s grace period.
const DEFAULT_TERMINATION_GRACE_MS: u64 = 45_000;
/// preStop-hook drain marker; override via `ECHO_AGENT_WORKSPACE_DRAINING_FILE`.
const DEFAULT_DRAINING_FILE: &str = "/tmp/workspace-server.draining";
static DRAIN_STARTED_TOTAL: std::sync::LazyLock<IntCounterVec> = std::sync::LazyLock::new(|| {
    register_int_counter_vec!(
        "echo_agent_workspace_drain_started_total",
        "Graceful drains started, by trigger reason",
        &["reason"]
    )
    .unwrap()
});
static DRAIN_COMPLETED_TOTAL: std::sync::LazyLock<IntCounterVec> = std::sync::LazyLock::new(|| {
    register_int_counter_vec!(
        "echo_agent_workspace_drain_completed_total",
        "Graceful drains completed, by outcome",
        &["outcome"]
    )
    .unwrap()
});
static DRAIN_DURATION: std::sync::LazyLock<Histogram> = std::sync::LazyLock::new(|| {
    register_histogram!(
        "echo_agent_workspace_drain_duration_seconds",
        "Wall-clock duration of a graceful two-phase drain",
        vec![0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0]
    )
    .unwrap()
});
static DRAIN_LOST_ITEMS_TOTAL: std::sync::LazyLock<IntCounter> = std::sync::LazyLock::new(|| {
    register_int_counter!(
        "echo_agent_workspace_drain_lost_items_total",
        "Upload-queue items still pending when a drain deadline was exceeded (expected 0)"
    )
    .unwrap()
});
static PRODUCER_SPAWNED_AFTER_DRAIN_TOTAL: std::sync::LazyLock<IntCounter> =
    std::sync::LazyLock::new(|| {
        register_int_counter!(
            "echo_agent_workspace_producer_spawned_after_drain_total",
            "Artifact producers spawned after a drain started — still tracked, but \
             their artifacts may miss the drain's queue flush (expected 0)"
        )
        .unwrap()
    });
/// Startup stages until hub connected. Labels: stage + outcome (ok/error).
static STARTUP_STAGE_DURATION_SECONDS: std::sync::LazyLock<HistogramVec> =
    std::sync::LazyLock::new(|| {
        register_histogram_vec!(
            "echo_agent_workspace_startup_stage_duration_seconds",
            "Workspace-server startup stage wall time by stage and outcome \
             (ok/error; fat-tail failures are recorded, not only success): \
             startup_recovery, tool_catalog, hub_ws_connect \
             (open_socket+hello through on_connect), connect_hub (catalog+ws), \
             time_to_ready (connect_local_workspace start to hub connect attempt end).",
            &["stage", "outcome"],
            vec![
                0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 30.0,
                60.0,
            ]
        )
        .unwrap()
    });
const STARTUP_STAGE_STARTUP_RECOVERY: &str = "startup_recovery";
const STARTUP_STAGE_TOOL_CATALOG: &str = "tool_catalog";
const STARTUP_STAGE_HUB_WS_CONNECT: &str = "hub_ws_connect";
const STARTUP_STAGE_CONNECT_HUB: &str = "connect_hub";
const STARTUP_STAGE_TIME_TO_READY: &str = "time_to_ready";
const STARTUP_OUTCOME_OK: &str = "ok";
const STARTUP_OUTCOME_ERROR: &str = "error";
fn observe_startup_stage(stage: &str, outcome: &str, secs: f64) {
    STARTUP_STAGE_DURATION_SECONDS
        .with_label_values(&[stage, outcome])
        .observe(secs);
}
/// tool_catalog always; connect_hub error only when catalog fails. Testable.
fn observe_connect_hub_catalog_result(
    catalog_ok: bool,
    tool_catalog_secs: f64,
    connect_hub_secs: f64,
) {
    let outcome = if catalog_ok {
        STARTUP_OUTCOME_OK
    } else {
        STARTUP_OUTCOME_ERROR
    };
    observe_startup_stage(STARTUP_STAGE_TOOL_CATALOG, outcome, tool_catalog_secs);
    if !catalog_ok {
        observe_startup_stage(
            STARTUP_STAGE_CONNECT_HUB,
            STARTUP_OUTCOME_ERROR,
            connect_hub_secs,
        );
    }
}
/// `session.bind` resolutions advertising zero model-facing tools, by reason.
/// At most one reason is counted per zero-tool bind.
static WORKSPACE_BIND_ZERO_TOOLS_TOTAL: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        register_int_counter_vec!(
            "echo_agent_workspace_bind_zero_tools_total",
            "session.bind resolutions advertising zero model-facing tools, by reason",
            &["reason"]
        )
        .unwrap()
    });
/// `session.bind` resolutions that FAILED the bind (the server reports
/// bind-unavailable and the harness re-provisions), by reason. Distinct from
/// [`WORKSPACE_BIND_ZERO_TOOLS_TOTAL`], which counts binds that *completed*
/// while advertising zero model-facing tools.
static WORKSPACE_BIND_FAILED_TOTAL: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        register_int_counter_vec!(
            "echo_agent_workspace_bind_failed_total",
            "session.bind resolutions that failed the bind, by reason",
            &["reason"]
        )
        .unwrap()
    });
/// Pinned tool ids this binary could not serve at `session.bind`.
static WORKSPACE_BIND_UNSERVED_TOOLS_TOTAL: std::sync::LazyLock<IntCounter> =
    std::sync::LazyLock::new(|| {
        register_int_counter!(
            "echo_agent_workspace_bind_unserved_tools_total",
            "Pinned tool ids unknown to this binary at session.bind (reported, not served)"
        )
        .unwrap()
    });
/// Model-facing tools advertised per successful `session.bind` (the RPC infra
/// handler is not counted). Catches silent shrinkage of a session's toolset.
static WORKSPACE_BIND_ADVERTISED_TOOLS: std::sync::LazyLock<Histogram> =
    std::sync::LazyLock::new(|| {
        register_histogram!(
            "echo_agent_workspace_bind_advertised_tools",
            "Model-facing tools advertised per successful session.bind",
            vec![
                0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0, 12.0, 16.0, 20.0, 30.0
            ]
        )
        .unwrap()
    });
/// Tripwire, expected 0 in production. `path="swap"`: a toolset swap found
/// the outgoing toolset's `Terminal` resource pointing at a backend other
/// than the session-owned one — a resolve path bypassed the session-owned
/// backend, and that backend's background tasks die with the old toolset.
/// Non-zero means background tasks were (or are about to be) killed by a
/// toolset swap: page the owning team. (`path="actor"` — actor-loop
/// channel-closure detection — is not emitted yet.)
pub(crate) static WORKSPACE_TERMINAL_BACKEND_ORPHANED_TOTAL: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        register_int_counter_vec!(
            "echo_agent_workspace_terminal_backend_orphaned_total",
            "Terminal backends detected orphaned from their session, by detection path \
             (tripwire, expected 0)",
            &["path"]
        )
        .unwrap()
    });
/// Environment-capture (`workspace_environment.json`) blocking task panics
/// (tripwire, expected 0). A non-zero rate means `WorkspaceEnvironment::capture`
/// is faulting for real sessions and dropping the artifact.
static ENV_CAPTURE_PANIC_TOTAL: std::sync::LazyLock<IntCounter> = std::sync::LazyLock::new(|| {
    register_int_counter!(
        "echo_agent_workspace_env_capture_panic_total",
        "Environment-capture blocking task panics (tripwire, expected 0)"
    )
    .unwrap()
});
use crate::capability::CapabilityMode;
use crate::config::{
    AgentSessionConfig, DEFAULT_EVENT_BUFFER_CAPACITY, HookSourceConfig, WorkspaceConfig,
};
use crate::error::{WorkspaceError, WorkspaceResult};
use crate::session::swap_policy::{
    DeferReason, SessionSnapshot, SwapAction, SwapDecision, SwapPolicy, SwapTrigger,
    record_swap_decision, record_toolset_swap,
};
use crate::session::tool_config::resolve_session_toolset;
use crate::session::{WorkspaceSession, WorkspaceShared};
use crate::telemetry::dc_log;
use crate::workspace_ops::{
    GetFileEntry, GetFileResult, GetFilesRes, PutFileEntry, PutFileResult, PutFilesRes,
};
use echo_agent_diag_server::DiagHandle;
use echo_agent_file_utils::queue::EnqueueOutcome;
use echo_agent_session_events::types::CancellationCategory;
use echo_agent_session_events::{Event, SessionRelationship, TurnOutcomeLabel};
use echo_agent_tool_protocol::turn_hook::{AfterTurnAckPayload, AfterTurnAckStatus};
/// Per-domain checkpoint captures, by domain and turn outcome.
pub(crate) static REWIND_CHECKPOINT_CAPTURE_TOTAL: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        register_int_counter_vec!(
            "echo_agent_workspace_rewind_checkpoint_capture_total",
            "Total rewind-checkpoint domain captures",
            &["domain", "outcome"]
        )
        .unwrap()
    });
/// Checkpoint finalizes, by turn outcome.
pub(crate) static REWIND_CHECKPOINT_FINALIZE_TOTAL: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        register_int_counter_vec!(
            "echo_agent_workspace_rewind_checkpoint_finalize_total",
            "Total rewind-checkpoint finalizes",
            &["outcome"]
        )
        .unwrap()
    });
/// Per-domain restores (the user-initiated `rewind_to` path), by domain and result.
pub(crate) static REWIND_RESTORE_TOTAL: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        register_int_counter_vec!(
            "echo_agent_workspace_rewind_restore_total",
            "Total rewind-checkpoint domain restores",
            &["domain", "result"]
        )
        .unwrap()
    });
/// Duration of per-domain capture operations.
pub(crate) static REWIND_CHECKPOINT_DURATION: std::sync::LazyLock<HistogramVec> =
    std::sync::LazyLock::new(|| {
        register_histogram_vec!(
            "echo_agent_workspace_rewind_checkpoint_duration_seconds",
            "Duration of rewind-checkpoint per-domain capture operations",
            &["domain"],
            vec![0.001, 0.005, 0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0]
        )
        .unwrap()
    });
/// Correctness canary: non-`Completed` `after_turn` boundaries that produced
/// a rewind finalize. Stays 0 unless `workspace_rewind_all_outcomes` is on.
pub(crate) static REWIND_NON_COMPLETED_FINALIZE_TOTAL: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        register_int_counter_vec!(
            "echo_agent_workspace_rewind_non_completed_finalize_total",
            "Non-Completed after_turn boundaries that produced a rewind finalize",
            &["outcome"]
        )
        .unwrap()
    });
/// `domain` label for the rewind metrics. Typed so the closed fs/hunk/git
/// vocabulary can't be mistyped at a call site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RewindDomain {
    Fs,
    Hunk,
    Git,
}
impl RewindDomain {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            RewindDomain::Fs => "fs",
            RewindDomain::Hunk => "hunk",
            RewindDomain::Git => "git",
        }
    }
}
/// Map a turn outcome to a stable, bounded `outcome` metric label. The catch-all
/// keeps label cardinality bounded (`TurnHookOutcome` is `#[non_exhaustive]`).
pub(crate) fn rewind_outcome_label(outcome: TurnHookOutcome) -> &'static str {
    match outcome {
        TurnHookOutcome::Completed => "completed",
        TurnHookOutcome::Cancelled => "cancelled",
        TurnHookOutcome::Error => "error",
        _ => "other",
    }
}
/// Map a restore result to its `result` metric label.
pub(crate) fn rewind_result_label(success: bool) -> &'static str {
    if success { "success" } else { "failure" }
}
/// Record a per-domain checkpoint capture, labeled by turn outcome.
pub(crate) fn record_rewind_capture(domain: RewindDomain, outcome: TurnHookOutcome) {
    REWIND_CHECKPOINT_CAPTURE_TOTAL
        .with_label_values(&[domain.as_str(), rewind_outcome_label(outcome)])
        .inc();
}
/// Observe how long a per-domain capture operation took (seconds).
pub(crate) fn observe_rewind_capture_duration(domain: RewindDomain, seconds: f64) {
    REWIND_CHECKPOINT_DURATION
        .with_label_values(&[domain.as_str()])
        .observe(seconds);
}
/// Record a per-domain restore, labeled by result (success/failure).
pub(crate) fn record_rewind_restore(domain: RewindDomain, success: bool) {
    REWIND_RESTORE_TOTAL
        .with_label_values(&[domain.as_str(), rewind_result_label(success)])
        .inc();
}
/// Record the metrics common to every finalize: FS-domain capture + finalize
/// counter (both by `outcome`) + FS capture duration. Shared by the RPC finalize
/// and the non-`Completed` cross-over so the two paths can't drift.
pub(crate) fn record_fs_finalize(outcome: TurnHookOutcome, fs_capture_seconds: f64) {
    observe_rewind_capture_duration(RewindDomain::Fs, fs_capture_seconds);
    record_rewind_capture(RewindDomain::Fs, outcome);
    REWIND_CHECKPOINT_FINALIZE_TOTAL
        .with_label_values(&[rewind_outcome_label(outcome)])
        .inc();
}
/// Record the correctness canary: a non-`Completed` `after_turn` boundary that
/// produced a finalize.
pub(crate) fn record_non_completed_finalize_canary(outcome: TurnHookOutcome) {
    REWIND_NON_COMPLETED_FINALIZE_TOTAL
        .with_label_values(&[rewind_outcome_label(outcome)])
        .inc();
}
/// Zero-init this module's metric families. See [`crate::init_metrics`].
pub(crate) fn init_metrics() {
    for reason in [DrainReason::Sigterm, DrainReason::Evict] {
        DRAIN_STARTED_TOTAL
            .with_label_values(&[reason.as_str()])
            .inc_by(0);
    }
    for outcome in [
        DrainOutcome::Full,
        DrainOutcome::Partial,
        DrainOutcome::ProducersTimeout,
        DrainOutcome::Timeout,
    ] {
        DRAIN_COMPLETED_TOTAL
            .with_label_values(&[outcome.as_str()])
            .inc_by(0);
    }
    DRAIN_LOST_ITEMS_TOTAL.inc_by(0);
    PRODUCER_SPAWNED_AFTER_DRAIN_TOTAL.inc_by(0);
    WORKSPACE_BIND_UNSERVED_TOOLS_TOTAL.inc_by(0);
    ENV_CAPTURE_PANIC_TOTAL.inc_by(0);
    std::sync::LazyLock::force(&DRAIN_DURATION);
    std::sync::LazyLock::force(&WORKSPACE_BIND_ADVERTISED_TOOLS);
    for stage in [
        STARTUP_STAGE_STARTUP_RECOVERY,
        STARTUP_STAGE_TOOL_CATALOG,
        STARTUP_STAGE_HUB_WS_CONNECT,
        STARTUP_STAGE_CONNECT_HUB,
        STARTUP_STAGE_TIME_TO_READY,
    ] {
        for outcome in [STARTUP_OUTCOME_OK, STARTUP_OUTCOME_ERROR] {
            let _ = STARTUP_STAGE_DURATION_SECONDS.with_label_values(&[stage, outcome]);
        }
    }
    for reason in [
        "workspace_shutdown",
        "session_lookup_failed",
        "session_error",
    ] {
        WORKSPACE_BIND_FAILED_TOTAL
            .with_label_values(&[reason])
            .inc_by(0);
    }
    for reason in ["empty_after_filter", "missing_tool_config"] {
        WORKSPACE_BIND_ZERO_TOOLS_TOTAL
            .with_label_values(&[reason])
            .inc_by(0);
    }
    WORKSPACE_TERMINAL_BACKEND_ORPHANED_TOTAL
        .with_label_values(&["swap"])
        .inc_by(0);
    for domain in [RewindDomain::Fs, RewindDomain::Hunk, RewindDomain::Git] {
        for outcome in ["completed", "cancelled", "error", "other"] {
            REWIND_CHECKPOINT_CAPTURE_TOTAL
                .with_label_values(&[domain.as_str(), outcome])
                .inc_by(0);
        }
        for result in ["success", "failure"] {
            REWIND_RESTORE_TOTAL
                .with_label_values(&[domain.as_str(), result])
                .inc_by(0);
        }
        let _ = REWIND_CHECKPOINT_DURATION.with_label_values(&[domain.as_str()]);
    }
    for outcome in ["completed", "cancelled", "error", "other"] {
        REWIND_CHECKPOINT_FINALIZE_TOTAL
            .with_label_values(&[outcome])
            .inc_by(0);
        REWIND_NON_COMPLETED_FINALIZE_TOTAL
            .with_label_values(&[outcome])
            .inc_by(0);
    }
}
/// Outcome of a hub `session.bind` against an already-existing session
/// (see [`WorkspaceHandle::rebind_existing_hub_session`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RebindOutcome {
    /// Same (or no) explicit toolset — session reused untouched.
    Reused,
    /// Changed explicit toolset — re-resolved and swapped in.
    Reresolved,
    /// Changed explicit toolset, but the re-resolve failed; existing kept.
    ReresolveFailed,
    /// Changed explicit toolset, but the session's toolset is externally
    /// owned (local-bind shape) — nothing was resolved or swapped; the
    /// existing toolset (and fingerprint) kept. Reused-semantics for the
    /// bind reply: advertise the KEPT toolset, drop any unserved set from
    /// the unapplied resolve.
    KeptExternallyOwned,
    /// Changed explicit toolset while the session had tool calls in flight
    /// (`explicit → different-explicit` transition only) — existing kept;
    /// a later rebind with no calls in flight applies the correction.
    ReresolveDeferredInFlight,
}
/// What [`WorkspaceHandle::resolve_and_swap_session_toolset`] actually did —
/// so no caller can mistake a deliberate skip for an installed swap (the
/// skip leaves toolset AND fingerprint untouched).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "a skip means the config was NOT applied; callers must not report success"]
pub(crate) enum SwapOutcome {
    /// Toolset re-resolved and installed; fingerprint updated.
    Swapped,
    /// Identical fingerprint ([`SwapDecision::Reuse`]): the live toolset
    /// already reflects the config, nothing resolved or changed.
    Reused,
    /// Externally-owned (local-bind) toolset: rebuild skipped, nothing
    /// changed. See `toolset_terminal_is_session_owned`.
    SkippedExternallyOwned,
}
/// Public handle to a workspace instance. Owns shared state (sessions,
/// MCP snapshot, tool config, event bus) and session lifecycle.
#[derive(Clone)]
pub struct WorkspaceHandle {
    pub(crate) shared: Arc<WorkspaceShared>,
}
type AcknowledgedNotifyChannel = (
    echo_agent_tools::notification::types::ToolNotificationHandle,
    tokio::sync::mpsc::UnboundedReceiver<
        echo_agent_tools::notification::AcknowledgedToolNotification,
    >,
);
/// Builds with no forwarder. They must not open the channel, because an unread one blocks every delete.
fn acknowledged_notify_channel(_enabled: bool) -> Option<AcknowledgedNotifyChannel> {
    None
}
/// Client-fs resolution base: request paths resolve against `base`,
/// `canonical` is the matching canonicalization-containment boundary.
pub(crate) struct ClientFsBase {
    pub(crate) base: PathBuf,
    pub(crate) canonical: PathBuf,
}
impl WorkspaceHandle {
    /// `None` when not connected. Never hands out an owned
    /// `ToolServer` — a clone-drop begins server teardown.
    pub async fn trace_donation_reporter(
        &self,
        service_name: &str,
    ) -> Option<(
        echo_agent_computer_hub_sdk::HubDonatingReporter,
        echo_agent_computer_hub_sdk::TraceDonationPump,
    )> {
        self.shared
            .hub_handle
            .lock()
            .await
            .as_ref()
            .map(|hub| hub.server.trace_donation_reporter(service_name))
    }
    /// Post-connect entry point for the log export layer, the analogue of
    /// [`Self::trace_donation_reporter`]. Returns `None` when not connected
    /// (the layer stays inert). On
    /// `Some`, yields a [`LogDonationSender`] to swap into the
    /// already-installed inert `DonatingLogLayer` plus a drain handle.
    /// Never hands out an owned `ToolServer` — a clone-drop begins server
    /// teardown.
    ///
    /// [`LogDonationSender`]: echo_agent_computer_hub_sdk::LogDonationSender
    pub async fn log_donation_layer(
        &self,
        service_name: &str,
    ) -> Option<(
        echo_agent_computer_hub_sdk::LogDonationSender,
        echo_agent_computer_hub_sdk::LogDonationPump,
    )> {
        self.shared
            .hub_handle
            .lock()
            .await
            .as_ref()
            .map(|hub| hub.server.log_donation_layer(service_name))
    }
    /// Post-connect entry point for metric export, the analogue of
    /// [`Self::trace_donation_reporter`]. Returns `None` when not connected
    /// (no reporter is spawned). On
    /// `Some`, spawns the periodic Prometheus-registry gather → OTLP →
    /// export pump and yields a drain handle. Never hands out an owned
    /// `ToolServer` — a clone-drop begins server teardown.
    pub async fn metric_donation_reporter(
        &self,
        service_name: &str,
    ) -> Option<echo_agent_computer_hub_sdk::MetricDonationPump> {
        self.shared
            .hub_handle
            .lock()
            .await
            .as_ref()
            .map(|hub| hub.server.metric_donation_reporter(service_name))
    }
    /// Construct a handle with zero sessions.
    ///
    /// Sessions are created explicitly via [`Self::create_session`] or
    /// [`Self::fork_session`]. There is no implicit "main" session —
    /// callers (TUI, workspace-server binary) create their first
    /// session after construction.
    ///
    /// # Panics
    /// Requires a Tokio runtime to be entered (for broadcast channel).
    pub fn new(config: WorkspaceConfig) -> WorkspaceResult<Self> {
        Self::build(
            config,
            ephemeral_workspace_home(),
            None,
            true,
            false,
            events_enabled(),
            rewind_all_outcomes_from_env(),
            tool_defs_enabled(),
            crate::upload::environment::WorkspaceIdentity::default(),
        )
    }
    /// Construct a handle with an explicit `$ECHO_AGENT_WORKSPACE_HOME` and a
    /// pre-spawned [`UploadQueue`](echo_agent_file_utils::queue::UploadQueue).
    ///
    /// [`connect_local_workspace`] calls this so the queue is backed by the
    /// proxy storage config; [`Self::new`] takes the queue-less path for tests
    /// and local mode.
    ///
    /// # Panics
    /// Requires a Tokio runtime to be entered (for broadcast channel).
    pub(crate) fn new_with_data_collection(
        config: WorkspaceConfig,
        workspace_home: std::path::PathBuf,
        upload_queue: Arc<echo_agent_file_utils::queue::UploadQueue>,
        upload_queue_enabled: bool,
        data_collection_disabled: bool,
        identity: crate::upload::environment::WorkspaceIdentity,
    ) -> WorkspaceResult<Self> {
        Self::build(
            config,
            workspace_home,
            Some(upload_queue),
            upload_queue_enabled,
            data_collection_disabled,
            events_enabled(),
            rewind_all_outcomes_from_env(),
            tool_defs_enabled(),
            identity,
        )
    }
    fn build(
        config: WorkspaceConfig,
        workspace_home: std::path::PathBuf,
        upload_queue: Option<Arc<echo_agent_file_utils::queue::UploadQueue>>,
        _upload_queue_enabled: bool,
        data_collection_disabled: bool,
        events_enabled: bool,
        workspace_rewind_all_outcomes: bool,
        tool_defs_enabled: bool,
        identity: crate::upload::environment::WorkspaceIdentity,
    ) -> WorkspaceResult<Self> {
        let sessions = std::collections::HashMap::new();
        let local_registry = echo_agent_computer_hub_sdk::LocalRegistry::new();
        let capacity = if config.event_buffer_capacity == 0 {
            DEFAULT_EVENT_BUFFER_CAPACITY
        } else {
            config.event_buffer_capacity
        };
        let (events, _drop_rx) = tokio::sync::broadcast::channel(capacity);
        let (hook_registry, hook_load_errors) = {
            use echo_agent_hooks::discovery::{HookSource, load_hooks_from_sources};
            fn to_hook_source(s: &HookSourceConfig) -> HookSource<'_> {
                match s {
                    HookSourceConfig::SettingsFile(p) => HookSource::SettingsFile(p.as_path()),
                    HookSourceConfig::Directory(p) => HookSource::Directory(p.as_path()),
                }
            }
            let global_refs: Vec<HookSource<'_>> = config
                .hook_global_sources
                .iter()
                .map(to_hook_source)
                .collect();
            let project_refs: Vec<HookSource<'_>> = config
                .hook_project_sources
                .iter()
                .map(to_hook_source)
                .collect();
            let (registry, errors) = load_hooks_from_sources(&global_refs, &project_refs);
            for err in &errors {
                tracing::warn!(error = %err, "hook discovery error (non-fatal)");
            }
            tracing::info!(
                hook_count = registry.len(),
                error_count = errors.len(),
                "hook discovery complete"
            );
            (registry, errors)
        };
        #[cfg(not(feature = "desktop"))]
        let lsp: Option<Arc<dyn echo_agent_tools::implementations::lsp::LspBackend>> = {
            let sourced =
                echo_agent_tools::implementations::lsp::config::load_servers_with_plugins_sourced(
                    &config.root_cwd,
                    &[],
                    &[],
                    &[],
                    &[],
                );
            let servers =
                echo_agent_tools::implementations::lsp::config::filter_project_lsp_when_untrusted(
                    sourced,
                    config.project_lsp_trusted,
                );
            if servers.is_empty() {
                None
            } else {
                use echo_agent_tools::implementations::lsp::{
                    LspBackend, LspBackendAdapter, LspManager,
                };
                let mgr = Arc::new(tokio::sync::Mutex::new(LspManager::new(
                    servers,
                    config.root_cwd.clone(),
                    true,
                    echo_agent_tools::notification::ToolNotificationHandle::noop(),
                )));
                let adapter = Arc::new(LspBackendAdapter::new(mgr));
                adapter.ensure_started_background();
                Some(adapter)
            }
        };
        #[cfg(feature = "desktop")]
        let lsp = None;
        let session_event_writers: Arc<
            dashmap::DashMap<String, echo_agent_session_events::EventWriter>,
        > = Arc::new(dashmap::DashMap::new());
        let activity_tracker =
            Arc::new(
                crate::activity::ActivityTracker::with_prune_window(
                    config.status_config.session_idle_prune,
                )
                .with_idle_ignores_background(config.status_config.idle_ignores_background)
                .with_preview_activity_window_ms(
                    config.status_config.preview_activity_window.as_millis() as u64,
                )
                .with_rpc_activity_window_ms(
                    config.status_config.rpc_activity_window.as_millis() as u64
                )
                .with_presence_activity_window_ms(
                    config
                        .status_config
                        .effective_presence_activity_window()
                        .as_millis() as u64,
                )
                .with_scheduled_task_keep_awake_window_ms(
                    config.status_config.scheduled_task_keep_awake.as_millis() as u64,
                ),
            );
        activity_tracker.set_event_writers(session_event_writers.clone());
        if let Some(queue) = &upload_queue {
            activity_tracker.set_upload_queue_stats(queue.stats_arc());
            queue
                .stats()
                .set_transition_notify(activity_tracker.notify_handle());
        }
        let producer_tasks = tokio_util::task::TaskTracker::new();
        activity_tracker.set_producer_tasks(producer_tasks.clone());
        let shared = WorkspaceShared {
            default_tool_config: config.default_tool_config,
            require_explicit_toolset: config.require_explicit_toolset,
            confine_fs_to_workspace_root: config.confine_fs_to_workspace_root,
            root_cwd: config.root_cwd.clone(),
            sessions: parking_lot::RwLock::new(sessions),
            session_factory: config.session_factory,
            mcp_tools_snapshot: arc_swap::ArcSwap::new(Arc::new(vec![])),
            events,
            respect_gitignore: config.respect_gitignore,
            memory_config: config.memory_config,
            hook_registry: Arc::new(parking_lot::RwLock::new(hook_registry)),
            hook_load_errors,
            skills_config: config.skills_config,
            plugin_discovery_config: config.plugin_discovery_config,
            hub_handle: tokio::sync::Mutex::new(None),
            hub_tools_snapshot: arc_swap::ArcSwap::new(Arc::new(vec![])),
            hub_config: config.hub_config,
            auth_provider: config.auth_provider,
            activity_notify_handle: arc_swap::ArcSwap::new(Arc::new(None)),
            client_ext_sink: arc_swap::ArcSwap::new(Arc::new(None)),
            local_registry,
            activity_tracker,
            scheduler_poll_started: std::sync::atomic::AtomicBool::new(false),
            status_config: config.status_config,
            server_metadata: config.server_metadata,
            identity,
            fuzzy_searches: Arc::new(tokio::sync::Mutex::new(
                crate::file_system::FuzzySearchManager::new(std::time::Duration::from_secs(300)),
            )),
            lsp,
            codebase_indexes: Arc::new(parking_lot::Mutex::new(
                crate::file_system::CodebaseIndexManager::new(),
            )),
            workspace_rewind_all_outcomes,
            workspace_home,
            upload_queue,
            data_collection_disabled,
            events_enabled,
            tool_defs_enabled,
            tool_defs_last_emit: dashmap::DashMap::new(),
            session_event_writers,
            inflight_enqueues: dashmap::DashMap::new(),
            producer_tasks,
            bind_mount_hook: arc_swap::ArcSwap::from_pointee(
                crate::path_virtualization::BindMountHook::noop(),
            ),
            #[cfg(test)]
            post_resolve_test_hook: parking_lot::Mutex::new(None),
            client_fs_hash_memo: Default::default(),
        };
        Ok(Self {
            shared: Arc::new(shared),
        })
    }
    #[allow(dead_code)]
    pub fn shared(&self) -> &Arc<WorkspaceShared> {
        &self.shared
    }
    pub fn activity_tracker(&self) -> &std::sync::Arc<crate::activity::ActivityTracker> {
        &self.shared.activity_tracker
    }
    /// The [`ToolServer`](echo_agent_computer_hub_sdk::ToolServer) for this
    /// workspace, if a server connection is active.
    ///
    /// Non-blocking: returns `None` both when no server is connected and when the
    /// handle is momentarily locked (e.g. a concurrent connect), so callers
    /// must treat `None` as "no server available right now" and degrade gracefully.
    pub fn hub_server(&self) -> Option<echo_agent_computer_hub_sdk::ToolServer> {
        self.shared.hub_server()
    }
    /// Like [`Self::hub_server`] but awaits the connection lock instead of returning
    /// `None` on contention, so a transient `connect_hub` lock is not mistaken
    /// for "no server". `None` means no server is connected. Use from async callers.
    pub async fn hub_server_blocking(&self) -> Option<echo_agent_computer_hub_sdk::ToolServer> {
        self.shared.hub_server_blocking().await
    }
    /// Get the workspace root directory.
    pub(crate) fn root_cwd(&self) -> crate::error::WorkspaceResult<PathBuf> {
        Ok(self.shared.root_cwd.clone())
    }
    /// Create a new top-level session from the workspace's default config.
    ///
    /// Unlike [`fork_session`](Self::fork_session), this does not inherit
    /// from a parent — it creates a fresh session with
    /// `CapabilityMode::All` and the workspace's `root_cwd`. Both the
    /// TUI and server use this as the primary session creation path.
    ///
    /// Returns the newly created session, or an error if a session with
    /// the given ID already exists.
    pub fn create_session(
        &self,
        session_id: impl Into<String>,
    ) -> WorkspaceResult<Arc<WorkspaceSession>> {
        self.create_session_with_cwd(session_id, None)
    }
    /// Create a session with an optional CWD override, using the workspace
    /// default toolset and `CapabilityMode::All`.
    pub fn create_session_with_cwd(
        &self,
        session_id: impl Into<String>,
        cwd: Option<std::path::PathBuf>,
    ) -> WorkspaceResult<Arc<WorkspaceSession>> {
        self.create_session_with_config(session_id, cwd, None, CapabilityMode::All, None, false)
    }
    /// Create a session with an optional CWD override, per-session toolset, and
    /// capability mode. Bind-time entry point; `tool_config: None` uses the default.
    /// `viewer_ctx` is `None` for sessions that don't go through the server bind path.
    pub fn create_session_with_config(
        &self,
        session_id: impl Into<String>,
        cwd: Option<std::path::PathBuf>,
        tool_config: Option<echo_agent_tools::registry::types::ToolServerConfig>,
        capability: CapabilityMode,
        viewer_ctx: Option<echo_agent_tool_runtime::WorkspaceViewerContext>,
        system_notifications: bool,
    ) -> WorkspaceResult<Arc<WorkspaceSession>> {
        let session_id = session_id.into();
        let session_cwd = cwd.unwrap_or_else(|| self.shared.root_cwd.clone());
        let (hunk_event_tx, _hunk_event_rx) = tokio::sync::mpsc::unbounded_channel();
        let hunk_cancel = tokio_util::sync::CancellationToken::new();
        let hunk_tracker = HunkTrackerActor::spawn(
            session_id.clone(),
            session_cwd.clone(),
            hunk_event_tx,
            TrackingMode::AllDirty,
            hunk_cancel.clone(),
        );
        let result = self.create_session_with_tracker_inner(
            session_id,
            session_cwd,
            hunk_tracker,
            Some(hunk_cancel.clone()),
            tool_config,
            capability,
            viewer_ctx,
            system_notifications,
        );
        if result.is_err() {
            hunk_cancel.cancel();
        }
        result
    }
    /// Create a session that reuses an existing hunk tracker (already rooted at
    /// `cwd`) instead of spawning a new one, so the workspace session and the
    /// agent share a single per-session tracker. `tool_config: None` uses the default.
    pub fn create_session_with_tracker(
        &self,
        session_id: impl Into<String>,
        cwd: std::path::PathBuf,
        hunk_tracker: HunkTrackerHandle,
        tool_config: Option<echo_agent_tools::registry::types::ToolServerConfig>,
        capability: CapabilityMode,
    ) -> WorkspaceResult<Arc<WorkspaceSession>> {
        self.create_session_with_tracker_and_viewer_ctx(
            session_id,
            cwd,
            hunk_tracker,
            tool_config,
            capability,
            None,
            false,
        )
    }
    /// Variant of [`create_session_with_tracker`](Self::create_session_with_tracker)
    /// that carries a session-bind viewer context. The tracker is externally
    /// owned, so the session stores no cancel token for it.
    pub fn create_session_with_tracker_and_viewer_ctx(
        &self,
        session_id: impl Into<String>,
        cwd: std::path::PathBuf,
        hunk_tracker: HunkTrackerHandle,
        tool_config: Option<echo_agent_tools::registry::types::ToolServerConfig>,
        capability: CapabilityMode,
        viewer_ctx: Option<echo_agent_tool_runtime::WorkspaceViewerContext>,
        system_notifications: bool,
    ) -> WorkspaceResult<Arc<WorkspaceSession>> {
        self.create_session_with_tracker_inner(
            session_id,
            cwd,
            hunk_tracker,
            None,
            tool_config,
            capability,
            viewer_ctx,
            system_notifications,
        )
    }
    /// Shared creation body. `hunk_tracker_cancel` is `Some` only for
    /// workspace-spawned trackers, whose actor lifetime the session then
    /// owns; externally owned trackers pass `None`.
    #[allow(clippy::too_many_arguments)]
    fn create_session_with_tracker_inner(
        &self,
        session_id: impl Into<String>,
        cwd: std::path::PathBuf,
        hunk_tracker: HunkTrackerHandle,
        hunk_tracker_cancel: Option<tokio_util::sync::CancellationToken>,
        tool_config: Option<echo_agent_tools::registry::types::ToolServerConfig>,
        capability: CapabilityMode,
        viewer_ctx: Option<echo_agent_tool_runtime::WorkspaceViewerContext>,
        system_notifications: bool,
    ) -> WorkspaceResult<Arc<WorkspaceSession>> {
        let session_id = session_id.into();
        if session_id.is_empty() {
            return Err(WorkspaceError::EmptyAgentId);
        }
        {
            let sessions = self.shared.sessions.read();
            if self.shared.activity_tracker.is_draining() {
                return Err(WorkspaceError::ShuttingDown);
            }
            if sessions.contains_key(&session_id) {
                return Err(WorkspaceError::SessionAlreadyExists(session_id));
            }
        }
        let session_env = Arc::new(std::collections::HashMap::new());
        let config = tool_config.unwrap_or_else(|| self.shared.default_tool_config.clone());
        let mcp_snapshot = self.shared.mcp_tools_snapshot.load_full();
        let hub_snapshot = self.shared.hub_tools_snapshot.load_full();
        let system_notify_channel = acknowledged_notify_channel(system_notifications);
        let system_notify_handle = system_notify_channel.as_ref().map(|(h, _)| h.clone());
        let (effective, toolset, terminal_backend) = {
            let _span = LocalSpan::enter_with_local_parent("tool_server.toolset_resolve")
                .with_property(|| ("session_id", session_id.clone()));
            resolve_session_toolset(
                config,
                capability,
                &mcp_snapshot,
                &hub_snapshot,
                cwd.clone(),
                session_env.clone(),
                &session_id,
                self.shared.session_factory.as_ref(),
                Some(self.shared.local_registry.clone()),
                self.shared.lsp.clone(),
                viewer_ctx.clone(),
                self.shared
                    .compose_session_notification_handle(system_notify_handle),
            )
        }?;
        let session = Arc::new(WorkspaceSession::new(
            session_id.clone(),
            cwd,
            session_env,
            capability,
            0,
            u32::MAX,
            Arc::new(effective),
            toolset,
            terminal_backend,
            hunk_tracker,
            hunk_tracker_cancel,
            viewer_ctx,
            system_notifications,
            system_notify_channel,
        ));
        self.insert_session_guarded(&session)?;
        tracing::info!(session_id = %session.session_id(), "create_session: new session created");
        record_toolset_swap(
            &self.shared.activity_tracker,
            "create",
            session.session_id(),
        );
        Ok(session)
    }
    /// Insert under the write lock the evict drain shares, so a racing insert is
    /// seen by the evict or rejected here; rejection tears down what resolve spawned.
    fn insert_session_guarded(&self, session: &Arc<WorkspaceSession>) -> WorkspaceResult<()> {
        let rejection = {
            let mut sessions = self.shared.sessions.write();
            if self.shared.activity_tracker.is_draining() {
                Some(WorkspaceError::ShuttingDown)
            } else if sessions.contains_key(session.session_id()) {
                Some(WorkspaceError::SessionAlreadyExists(
                    session.session_id().to_owned(),
                ))
            } else {
                sessions.insert(session.session_id().to_owned(), Arc::clone(session));
                None
            }
        };
        if let Some(err) = rejection {
            session.cancel_hunk_tracker();
            session.shutdown_terminal_backend();
            return Err(err);
        }
        Ok(())
    }
    /// Update a session's tool config with auth and serialization; the RPC
    /// handler derives `caller_session_id` from the server-bound envelope.
    /// Swap gating (retryable `TurnActive`, stale heal): [`SwapPolicy::evaluate`].
    pub(crate) async fn update_tool_config(
        &self,
        caller_session_id: &str,
        session_id: &str,
        new_config: echo_agent_tools::registry::types::ToolServerConfig,
    ) -> crate::error::WorkspaceResult<()> {
        let session = self
            .session(session_id)
            .ok_or_else(|| crate::error::WorkspaceError::SessionNotFound(session_id.to_owned()))?;
        if caller_session_id != session_id {
            return Err(crate::error::WorkspaceError::Unauthorized {
                caller: caller_session_id.to_owned(),
                target: session_id.to_owned(),
            });
        }
        match self
            .resolve_and_swap_session_toolset(&session, new_config, SwapTrigger::UpdateRpc)
            .await?
        {
            SwapOutcome::Swapped | SwapOutcome::Reused => Ok(()),
            SwapOutcome::SkippedExternallyOwned => Err(
                crate::error::WorkspaceError::ToolsetExternallyOwned(session_id.to_owned()),
            ),
        }
    }
    /// Re-resolve `new_config` against the session's frozen bind-time inputs
    /// and atomically swap its toolset (`ToolsChanged`). Update-RPC entry:
    /// gated by [`SwapPolicy::evaluate`], twice (entry + post-resolve).
    pub(crate) async fn resolve_and_swap_session_toolset(
        &self,
        session: &Arc<crate::session::WorkspaceSession>,
        new_config: echo_agent_tools::registry::types::ToolServerConfig,
        trigger: SwapTrigger,
    ) -> crate::error::WorkspaceResult<SwapOutcome> {
        let _update_guard = session.update_lock.lock().await;
        let session_id = session.session_id();
        let new_fingerprint = serde_json::to_value(&new_config).ok();
        let snapshot = SessionSnapshot::capture(
            session,
            &self.shared.activity_tracker,
            new_fingerprint.as_ref(),
        )
        .await;
        match SwapPolicy::evaluate(&snapshot, trigger) {
            SwapDecision::Reuse => {
                tracing::debug!(
                    session_id = %session_id,
                    trigger = trigger.metric_label(),
                    "toolset config identical to the stored bind fingerprint — \
                     reused untouched"
                );
                Ok(SwapOutcome::Reused)
            }
            SwapDecision::Skip(reason) => {
                record_swap_decision(
                    &self.shared.activity_tracker,
                    trigger,
                    session_id,
                    SwapAction::Skipped(reason),
                );
                tracing::warn!(
                    session_id = %session_id,
                    trigger = trigger.metric_label(),
                    "toolset swap skipped: toolset terminal backend is externally \
                     owned (local bind)"
                );
                Ok(SwapOutcome::SkippedExternallyOwned)
            }
            SwapDecision::Defer(reason) => {
                record_swap_decision(
                    &self.shared.activity_tracker,
                    trigger,
                    session_id,
                    SwapAction::Deferred(reason),
                );
                tracing::info!(
                    session_id = %session_id,
                    trigger = trigger.metric_label(),
                    "toolset mutation rejected: turn active — retry at the turn boundary"
                );
                Err(crate::error::WorkspaceError::TurnActive(
                    session_id.to_owned(),
                ))
            }
            SwapDecision::Apply => {
                self.resolve_and_swap_session_toolset_locked(
                    session,
                    new_config,
                    new_fingerprint,
                    trigger,
                )
                .await
            }
        }
    }
    /// The [`SwapDecision::Apply`] arm: resolve `new_config` (whose
    /// fingerprint `new_fingerprint` must be) and install it. Callers hold
    /// `update_lock` and evaluated [`SwapPolicy`] to `Apply` under that hold.
    async fn resolve_and_swap_session_toolset_locked(
        &self,
        session: &Arc<crate::session::WorkspaceSession>,
        new_config: echo_agent_tools::registry::types::ToolServerConfig,
        new_fingerprint: Option<serde_json::Value>,
        trigger: SwapTrigger,
    ) -> crate::error::WorkspaceResult<SwapOutcome> {
        let session_id = session.session_id().to_owned();
        let mcp_snapshot = self.shared.mcp_tools_snapshot.load_full();
        let hub_snapshot = self.shared.hub_tools_snapshot.load_full();
        let cwd = session.cwd().to_path_buf();
        let session_env = session.session_env().clone();
        let cap = session.capability_mode();
        let factory = self.shared.session_factory.clone();
        let lr = self.shared.local_registry.clone();
        let lsp = self.shared.lsp.clone();
        let sid = session_id.to_owned();
        let viewer_ctx = session.viewer_ctx().cloned();
        let notification_handle = self
            .shared
            .compose_session_notification_handle(session.system_notify_handle());
        let terminal_backend = session.terminal_backend().clone();
        let resolve_result = tokio::task::spawn_blocking(move || {
            crate::session::tool_config::resolve_session_toolset_rebuild(
                new_config,
                cap,
                &mcp_snapshot,
                &hub_snapshot,
                cwd,
                session_env,
                &sid,
                factory.as_ref(),
                Some(lr),
                lsp,
                viewer_ctx,
                notification_handle,
                terminal_backend,
            )
        })
        .await
        .map_err(|e| crate::error::WorkspaceError::JoinError(e.to_string()))?;
        let (effective, new_toolset) = resolve_result?;
        #[cfg(test)]
        if let Some(hook) = self.shared.post_resolve_test_hook.lock().as_ref() {
            hook();
        }
        if trigger.rechecks_after_resolve() {
            let snapshot = SessionSnapshot::capture(
                session,
                &self.shared.activity_tracker,
                new_fingerprint.as_ref(),
            )
            .await;
            match SwapPolicy::evaluate(&snapshot, trigger) {
                SwapDecision::Apply => {}
                SwapDecision::Reuse => {
                    tracing::debug!(
                        session_id = %session_id,
                        trigger = trigger.metric_label(),
                        "resolved toolset discarded post-resolve: a concurrent \
                         bind installed the identical fingerprint during the \
                         re-resolve"
                    );
                    return Ok(SwapOutcome::Reused);
                }
                SwapDecision::Skip(reason) => {
                    record_swap_decision(
                        &self.shared.activity_tracker,
                        trigger,
                        &session_id,
                        SwapAction::Skipped(reason),
                    );
                    tracing::warn!(
                        session_id = %session_id,
                        trigger = trigger.metric_label(),
                        "toolset swap skipped: toolset terminal backend is externally \
                         owned (local bind)"
                    );
                    return Ok(SwapOutcome::SkippedExternallyOwned);
                }
                SwapDecision::Defer(reason) => {
                    let reason = match reason {
                        DeferReason::TurnActive => DeferReason::TurnActiveLate,
                        other => other,
                    };
                    record_swap_decision(
                        &self.shared.activity_tracker,
                        trigger,
                        &session_id,
                        SwapAction::Deferred(reason),
                    );
                    tracing::info!(
                        session_id = %session_id,
                        trigger = trigger.metric_label(),
                        "toolset mutation rejected post-resolve: a turn started during \
                         the re-resolve — resolved toolset discarded; retry at the \
                         turn boundary"
                    );
                    return Err(crate::error::WorkspaceError::TurnActive(session_id));
                }
            }
        }
        session
            .replace_carrying_browser_service(Arc::new(effective), new_toolset)
            .await;
        session.set_bind_tool_config_fingerprint(new_fingerprint);
        session.clear_stale_resolve();
        record_swap_decision(
            &self.shared.activity_tracker,
            trigger,
            &session_id,
            SwapAction::Applied,
        );
        let _ = self
            .shared
            .events
            .send(echo_agent_workspace_types::WorkspaceEvent::ToolsChanged {
                session_id: session_id.to_owned(),
            });
        Ok(SwapOutcome::Swapped)
    }
    /// Hub `session.bind` against an existing session: reuse, or re-resolve
    /// and swap per the owner-rebind policy rows (incl. the identical stale
    /// heal). `explicit_cfg=None` never overwrites; `None` = session vanished.
    pub(crate) async fn rebind_existing_hub_session(
        &self,
        session_id: &str,
        explicit_cfg: Option<echo_agent_tools::registry::types::ToolServerConfig>,
        bind_fingerprint: Option<serde_json::Value>,
    ) -> Option<(Arc<crate::session::WorkspaceSession>, RebindOutcome)> {
        let session = self.session(session_id)?;
        let Some(cfg) = explicit_cfg else {
            return Some((session, RebindOutcome::Reused));
        };
        let outcome = {
            let _update_guard = session.update_lock.lock().await;
            let snapshot = SessionSnapshot::capture(
                &session,
                &self.shared.activity_tracker,
                bind_fingerprint.as_ref(),
            )
            .await;
            match SwapPolicy::evaluate(&snapshot, SwapTrigger::OwnerRebind) {
                SwapDecision::Reuse => RebindOutcome::Reused,
                SwapDecision::Defer(reason) => {
                    record_swap_decision(
                        &self.shared.activity_tracker,
                        SwapTrigger::OwnerRebind,
                        session_id,
                        SwapAction::Deferred(reason),
                    );
                    tracing::warn!(
                        session_id = %session_id,
                        in_flight = snapshot.in_flight_calls(),
                        "session.bind: rebind swap (changed explicit toolset or stale-heal \
                         re-apply) deferred: tool calls in flight — keeping the existing \
                         toolset"
                    );
                    RebindOutcome::ReresolveDeferredInFlight
                }
                SwapDecision::Skip(reason) => {
                    record_swap_decision(
                        &self.shared.activity_tracker,
                        SwapTrigger::OwnerRebind,
                        session_id,
                        SwapAction::Skipped(reason),
                    );
                    tracing::warn!(
                        session_id = %session_id,
                        "session.bind: rebind carried a changed toolset config, but the \
                         session's toolset is externally owned (local bind) — keeping the \
                         existing toolset; the new config did NOT take effect"
                    );
                    RebindOutcome::KeptExternallyOwned
                }
                SwapDecision::Apply => {
                    match self
                        .resolve_and_swap_session_toolset_locked(
                            &session,
                            cfg,
                            bind_fingerprint,
                            SwapTrigger::OwnerRebind,
                        )
                        .await
                    {
                        Ok(SwapOutcome::Swapped) => {
                            tracing::info!(
                                session_id = %session_id,
                                "session.bind: rebind carried a changed toolset config — re-resolved \
                                 and swapped"
                            );
                            RebindOutcome::Reresolved
                        }
                        Ok(SwapOutcome::Reused) => RebindOutcome::Reused,
                        Ok(SwapOutcome::SkippedExternallyOwned) => {
                            tracing::warn!(
                                session_id = %session_id,
                                "session.bind: rebind carried a changed toolset config, but the \
                                 session's toolset is externally owned (local bind) — keeping the \
                                 existing toolset; the new config did NOT take effect"
                            );
                            RebindOutcome::KeptExternallyOwned
                        }
                        Err(e) => {
                            record_swap_decision(
                                &self.shared.activity_tracker,
                                SwapTrigger::OwnerRebind,
                                session_id,
                                SwapAction::ApplyFailed,
                            );
                            tracing::warn!(
                                session_id = %session_id, error = %e,
                                "session.bind: rebind toolset re-resolve failed — keeping the \
                                 existing toolset"
                            );
                            RebindOutcome::ReresolveFailed
                        }
                    }
                }
            }
        };
        Some((session, outcome))
    }
    pub async fn on_before_turn(
        &self,
        session_id: &str,
        payload: &echo_agent_tool_protocol::turn_hook::BeforeTurnPayload,
    ) {
        self.sync_session_yolo_mode(session_id, payload.yolo_mode);
        let before_handle = self
            .on_turn_boundary(
                session_id,
                crate::session::checkpoint::TurnBoundary::turn_start(payload.turn_number),
            )
            .await;
        tracing::debug!(
            session = %session_id,
            turn = payload.turn_number,
            model = %payload.model_id,
            "workspace: before_turn processed"
        );
        self.shared
            .session_event_writer(session_id)
            .emit(Event::TurnStarted {
                session_id: session_id.to_owned(),
                turn_number: payload.turn_number,
                model_id: payload.model_id.clone(),
                yolo_mode: payload.yolo_mode,
                conversation_message_count: payload.conversation_message_count,
                session_relationship: decode_session_relationship(&payload.session_relationship),
                schema_version: payload.schema_version.clone(),
                redirect_kind: None,
            });
        if let Some(handle) = before_handle {
            self.shared
                .inflight_enqueues
                .insert((session_id.to_owned(), payload.turn_number), handle);
        }
    }
    /// Fire-and-forget `after_turn` hook path (legacy shells / local mode):
    /// turn-end work with detached enqueue handles, no ack. New shells use
    /// the request/response path ([`Self::compute_turn_injections`]) instead.
    pub async fn on_after_turn(
        &self,
        session_id: &str,
        payload: &echo_agent_tool_protocol::turn_hook::AfterTurnPayload,
    ) {
        let _ = self.process_after_turn(session_id, payload).await;
    }
    async fn process_after_turn(
        &self,
        session_id: &str,
        payload: &echo_agent_tool_protocol::turn_hook::AfterTurnPayload,
    ) -> (
        Option<tokio::task::JoinHandle<EnqueueOutcome>>,
        Option<tokio::task::JoinHandle<EnqueueOutcome>>,
    ) {
        let after_handle = self
            .on_turn_boundary(
                session_id,
                crate::session::checkpoint::TurnBoundary::turn_end(
                    payload.turn_number,
                    payload.duration_ms,
                    payload.outcome,
                    payload.written_repo_paths.clone(),
                ),
            )
            .await;
        tracing::debug!(
            session = %session_id,
            turn = payload.turn_number,
            outcome = ?payload.outcome,
            "workspace: after_turn processed"
        );
        self.shared
            .session_event_writer(session_id)
            .emit(Event::TurnEnded {
                outcome: turn_outcome_label(payload.outcome),
                cancellation_category: decode_cancellation_category(
                    payload.cancellation_category.as_deref(),
                ),
                cancellation_context: payload.cancellation_context.clone(),
            });
        self.spawn_tool_state_upload(session_id, payload.turn_number);
        let before_handle = self
            .shared
            .inflight_enqueues
            .remove(&(session_id.to_owned(), payload.turn_number))
            .map(|(_, handle)| handle);
        (before_handle, after_handle)
    }
    /// Answer a request/response `turn_hook` (sampler/shell → workspace).
    ///
    /// Both phases run the same turn-boundary work as their fire-and-forget
    /// hook counterparts (the server-side sampler signals turns ONLY through
    /// this request channel): `Before` drives [`Self::on_before_turn`]
    /// (including the YOLO-state sync) and answers with a no-op reply
    /// (injections are not computed yet); `After` runs the turn-end work,
    /// awaits this turn's enqueue outcomes under [`after_turn_watchdog`]
    /// (which MUST undercut the requester's hook timeout), and returns the
    /// artifact ack on `HookReply::after_turn_ack`.
    ///
    /// Each phase must be signalled through exactly ONE channel per client —
    /// fire-and-forget hook or request — otherwise its work runs twice.
    pub async fn compute_turn_injections(
        &self,
        session_id: &str,
        request: &echo_agent_tool_protocol::turn_hook::TurnHookRequest,
    ) -> echo_agent_tool_protocol::turn_hook::HookReply {
        use echo_agent_tool_protocol::turn_hook::{HookReply, TurnHookRequest};
        match request {
            TurnHookRequest::Before(payload) => {
                self.on_before_turn(session_id, payload).await;
                HookReply::default()
            }
            TurnHookRequest::After(payload) => {
                let (before_handle, after_handle) =
                    self.process_after_turn(session_id, payload).await;
                let no_handle_skip_reason = if self.shared.data_collection_disabled {
                    "data_collection_disabled"
                } else {
                    "no_upload_queue"
                };
                let (status, artifact_count, error_message) = resolve_after_turn_ack(
                    before_handle,
                    after_handle,
                    after_turn_watchdog(),
                    no_handle_skip_reason,
                )
                .await;
                tracing::debug!(
                    session_id = %session_id,
                    turn_number = payload.turn_number,
                    ?status,
                    artifact_count,
                    "after_turn ack returned on hook reply"
                );
                HookReply {
                    after_turn_ack: Some(AfterTurnAckPayload {
                        turn_number: payload.turn_number,
                        status,
                        error_message,
                        artifact_count,
                    }),
                    ..HookReply::default()
                }
            }
            _ => HookReply::default(),
        }
    }
    /// Sync a before-turn hook's YOLO state into the session, emitting
    /// `YoloToggled` on transitions. No-op for unknown sessions.
    fn sync_session_yolo_mode(&self, session_id: &str, yolo_mode: bool) {
        let Some(session) = self.session(session_id) else {
            return;
        };
        let was = session.yolo_mode();
        if was != yolo_mode {
            tracing::info!(
                session = %session_id,
                from = was,
                to = yolo_mode,
                "workspace: yolo_mode changed via before-turn hook"
            );
            session.set_yolo_mode(yolo_mode);
            self.on_yolo_toggled(session_id, yolo_mode);
        }
    }
    /// Spawn an artifact-producer future tracked in the producer `TaskTracker`
    /// so status counts it and the durability idle gate withholds `idle_since_ms`
    /// while it runs; pokes status on start and completion. (The graceful drain
    /// added in the next PR awaits these tasks in phase 1.5 before flushing the
    /// queue — this PR only wires the tracking + idle-withholding.) Spawns after
    /// drain start stay tracked (the idle gate must not go blind) but are warned
    /// + counted as at-risk of missing the queue flush.
    pub(crate) fn spawn_producer<F>(&self, fut: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        if self.shared.activity_tracker.drain_started() {
            tracing::warn!(
                "producer spawned after drain start — artifact may miss the queue flush"
            );
            PRODUCER_SPAWNED_AFTER_DRAIN_TOTAL.inc();
        }
        let activity = self.shared.activity_tracker.clone();
        let tracked = self.shared.producer_tasks.track_future(fut);
        let handle = tokio::spawn(async move {
            let out = tracked.await;
            activity.poke();
            out
        });
        self.shared.activity_tracker.poke();
        handle
    }
    /// Spawn a fire-and-forget per-turn `tool_state.json` snapshot + upload to
    /// `{session_id}/turn_{N}/tool_state.json`. No-op when
    /// `ECHO_AGENT_WORKSPACE_TOOL_STATE_ENABLED` is off, opted out,
    /// there is no upload queue (local/test mode), or the
    /// session is unknown — legacy behavior unchanged.
    fn spawn_tool_state_upload(&self, session_id: &str, turn_number: u64) {
        if !crate::session::tool_config::tool_state_enabled() {
            return;
        }
        if self.shared.data_collection_disabled {
            return;
        }
        let Some(upload_queue) = self.shared.upload_queue.clone() else {
            dc_log!(
                debug,
                session_id = %session_id,
                turn_number,
                phase = "tool_state",
                outcome = "skipped",
                skip_reason = "no_upload_queue",
                "workspace: tool_state upload skipped — no upload queue"
            );
            crate::upload::record_upload_outcome("tool_state", "skipped");
            crate::upload::record_upload_skipped("tool_state", "no_upload_queue");
            return;
        };
        let Some(session) = self.session(session_id) else {
            dc_log!(
                warn,
                session_id = %session_id,
                turn_number,
                phase = "tool_state",
                outcome = "skipped",
                skip_reason = "no_session",
                "workspace: tool_state upload skipped — no bound session"
            );
            crate::upload::record_upload_outcome("tool_state", "skipped");
            crate::upload::record_upload_skipped("tool_state", "no_session");
            return;
        };
        let session_id = session_id.to_owned();
        self.spawn_producer(async move {
            if persist_and_enqueue_tool_state(
                session,
                session_id.clone(),
                turn_number,
                upload_queue,
            )
            .await
            .is_err()
            {
                dc_log!(
                    warn,
                    session_id = %session_id,
                    turn_number,
                    error_category = "enqueue_failed",
                    "workspace: tool_state upload failed"
                );
                crate::upload::record_upload_failed("tool_state", "enqueue_failed");
                crate::upload::record_upload_outcome("tool_state", "failed");
            }
        });
    }
    /// Drain the workspace's upload queue, waiting up to `deadline` for in-flight
    /// uploads to finish. Returns the number of items still pending after the
    /// deadline (0 when no queue is configured). Called from the workspace-server
    /// SIGTERM handler on graceful shutdown.
    pub async fn drain_upload_queue(&self, deadline: std::time::Duration) -> usize {
        match &self.shared.upload_queue {
            Some(queue) => queue.drain(deadline).await,
            None => 0,
        }
    }
    /// Serialize the session's workspace-side toolset to the Chat Completions
    /// tool-definitions shape and enqueue it (fire-and-forget) at the
    /// session-root path `{session_id}/workspace_tool_definitions.json`.
    ///
    /// This is the WORKSPACE-side subset; the shell's `tool_definitions.json`
    /// remains the source of truth for the full set the model sees — consumers
    /// union the two on `session_id`. Ordering is best-effort: the bind
    /// emission bypasses the 5s debounce (so it can't suppress the immediate
    /// post-bind `ToolsChanged` re-emit), and queue dispatch has no per-path
    /// ordering, so a stale baseline-only write may rarely clobber a fresher
    /// baseline+MCP snapshot — accepted as telemetry-only.
    ///
    /// No-op when the `ECHO_AGENT_WORKSPACE_TOOL_DEFS_ENABLED` flag is off, no upload
    /// queue is wired, or the session is unknown.
    pub(crate) fn emit_workspace_tool_definitions(&self, session_id: &str) {
        if !self.shared.tool_defs_enabled {
            return;
        }
        if !is_safe_object_segment(session_id) {
            self.shared.tool_defs_last_emit.remove(session_id);
            tracing::warn!(%session_id, "tool_defs: unsafe session id, skipping");
            return;
        }
        let Some(upload_queue) = self.shared.upload_queue.clone() else {
            return;
        };
        let Some((object_path, bytes)) = self.workspace_tool_definitions_payload(session_id) else {
            if self.session(session_id).is_none() {
                self.shared.tool_defs_last_emit.remove(session_id);
            }
            tracing::debug!(%session_id, "tool_defs: no payload, skipping");
            return;
        };
        let session_id = session_id.to_owned();
        self.spawn_producer(async move {
            let _ = enqueue_workspace_tool_definitions(
                &upload_queue,
                &session_id,
                &object_path,
                &bytes,
            )
            .await;
        });
    }
    /// Build the `(gcs_path, json_bytes)` payload for a session's workspace-side
    /// tool definitions, or `None` for an unknown session. Uses the same
    /// serializer as the shell's `tool_definitions.json`, so the two artifacts
    /// share a byte-identical element shape. Free of flag/queue gating for
    /// direct unit testing.
    fn workspace_tool_definitions_payload(&self, session_id: &str) -> Option<(String, Vec<u8>)> {
        let session = self.session(session_id)?;
        let definitions = session.toolset().tool_definitions();
        let bytes = serde_json::to_vec_pretty(&definitions)
            .inspect_err(|e| {
                tracing::warn!(%session_id, error = %e, "failed to serialize workspace tool definitions");
            })
            .ok()?;
        Some((workspace_tool_definitions_path(session_id), bytes))
    }
    /// Preemption-aware graceful drain: phase 1 waits for tool calls, phase 1.5
    /// for artifact producers, phase 2 flushes the upload queue (budgets per the
    /// `phase*_budget` helpers). Shared by the SIGTERM and server-evict triggers so
    /// they can't diverge.
    ///
    /// The preStop drain marker is (re)written at every phase boundary — not
    /// just once at the start — with the live total of outstanding durability
    /// work: active tool calls + background tasks (phase 1), in-flight artifact
    /// producers that have not yet enqueued (phase 1.5), and queued uploads
    /// (phase 2). This keeps a preStop hook from reading `0` while a tool call
    /// is still running (queue and producers both empty) or while later phases
    /// have yet to flush newly-produced work.
    ///
    /// Returns that same outstanding total after the deadline, so `0` means a
    /// fully clean drain — consistent with the final marker and
    /// [`DrainOutcome::Full`]; a wedged producer or tool call keeps it non-zero.
    pub async fn two_phase_drain(
        &self,
        grace_budget: std::time::Duration,
        reason: DrainReason,
    ) -> usize {
        let tracker = self.shared.activity_tracker.clone();
        let start = std::time::Instant::now();
        tracker.set_draining();
        tracker.poke();
        DRAIN_STARTED_TOTAL
            .with_label_values(&[reason.as_str()])
            .inc();
        let active_at_start = tracker.total_active() as usize;
        let pending_at_start = self.upload_queue_pending();
        let producers_at_start = self.shared.producer_tasks.len();
        let drain_file = draining_file_path();
        write_draining_marker(
            &drain_file,
            active_at_start + producers_at_start + pending_at_start,
        );
        dc_log!(
            info,
            drain_reason = reason.as_str(),
            grace_ms = grace_budget.as_millis() as u64,
            active_at_start,
            pending_at_start,
            producers_at_start,
            "workspace: two-phase drain commencing"
        );
        let phase1 = phase1_budget(grace_budget);
        let tools_idle = tokio::time::timeout(phase1, tracker.wait_until_tools_idle())
            .await
            .is_ok();
        if !tools_idle {
            tracing::warn!(
                active = tracker.total_active(),
                "drain phase 1 deadline exceeded — tool calls still in flight"
            );
        }
        write_draining_marker(&drain_file, self.outstanding_drain_work());
        let producers_done = wait_for_producers_idle(
            &self.shared.producer_tasks,
            phase15_budget(grace_budget.saturating_sub(start.elapsed())),
        )
        .await;
        if !producers_done {
            tracing::warn!(
                producers = self.shared.producer_tasks.len(),
                "drain phase 1.5 deadline exceeded — artifact producers still in flight"
            );
        }
        write_draining_marker(&drain_file, self.outstanding_drain_work());
        let phase2 = grace_budget.saturating_sub(start.elapsed());
        let unfinished = self.drain_upload_queue(phase2).await;
        let producers_unfinished = self.shared.producer_tasks.len();
        let active_unfinished = self.shared.activity_tracker.total_active() as usize;
        let total_unfinished = active_unfinished + producers_unfinished + unfinished;
        let outcome =
            classify_drain_outcome(tools_idle, producers_done, producers_unfinished, unfinished);
        DRAIN_COMPLETED_TOTAL
            .with_label_values(&[outcome.as_str()])
            .inc();
        DRAIN_DURATION.observe(start.elapsed().as_secs_f64());
        if unfinished > 0 {
            DRAIN_LOST_ITEMS_TOTAL.inc_by(unfinished as u64);
        }
        write_draining_marker(&drain_file, total_unfinished);
        if total_unfinished > 0 {
            tracing::warn!(
                reason = reason.as_str(),
                outcome = outcome.as_str(),
                active_unfinished,
                producers_unfinished,
                unfinished,
                total_unfinished,
                duration_ms = start.elapsed().as_millis() as u64,
                "workspace: two-phase drain finished with work still outstanding"
            );
        } else {
            tracing::info!(
                reason = reason.as_str(),
                outcome = outcome.as_str(),
                duration_ms = start.elapsed().as_millis() as u64,
                "workspace: two-phase drain complete"
            );
        }
        total_unfinished
    }
    /// Live pending upload-queue depth (0 when no queue is configured).
    fn upload_queue_pending(&self) -> usize {
        self.shared
            .upload_queue
            .as_ref()
            .map(|q| q.stats().pending.load(std::sync::atomic::Ordering::Relaxed) as usize)
            .unwrap_or(0)
    }
    /// Live total of outstanding durability work the two-phase drain must wait
    /// on: active tool calls + background tasks (phase 1) + in-flight artifact
    /// producers that have not yet enqueued (phase 1.5) + queued uploads
    /// (phase 2). Used to refresh the preStop drain marker at each phase
    /// boundary so it is never `0` while any phase still has work.
    fn outstanding_drain_work(&self) -> usize {
        self.shared.activity_tracker.total_active() as usize
            + self.shared.producer_tasks.len()
            + self.upload_queue_pending()
    }
    /// Bookkeeping for a cancelled in-flight tool call: marks it as
    /// completed in the activity tracker. Does **not** abort execution
    /// of the tool — that requires `CancellationToken` plumbing (future work).
    pub fn cancel_tool_call(&self, session_id: &str, call_id: &str) {
        self.shared.activity_tracker.tool_call_completed(
            call_id,
            Some(session_id),
            echo_agent_session_events::ToolOutcome::Cancelled,
        );
        tracing::info!(%session_id, %call_id, "cancel_tool_call: marked as completed");
    }
    /// Cancel all in-flight tool calls for a session. Called when a
    /// session-wide Cancel hook arrives (no specific `call_id`).
    pub fn cancel_all_tool_calls(&self, session_id: &str) {
        let count = self
            .shared
            .activity_tracker
            .cancel_all_session_calls(session_id);
        tracing::info!(%session_id, count, "cancel_all_tool_calls: marked all as completed");
    }
    /// Clean up workspace state for a session that has ended.
    /// Does **not** drop the session — that is handled by the server's
    /// `unbind_session` lifecycle.
    pub fn on_session_ended(&self, session_id: &str) {
        self.shared.activity_tracker.session_ended(session_id);
        self.shared.session_event_writers.remove(session_id);
        self.shared
            .inflight_enqueues
            .retain(|(sid, _), _| sid != session_id);
        self.shared.tool_defs_last_emit.remove(session_id);
        tracing::info!(%session_id, "session_ended cleanup completed");
    }
    /// Record a YOLO / always-approve mode toggle into the session's
    /// `events.jsonl`. These volatile-config mutations are shell-owned; this is
    /// the workspace-side emission entry point invoked by the server/shell forwarding
    /// layer when it observes a `SetYoloMode` command for a bound session. A no-op
    /// when events recording is disabled.
    pub fn on_yolo_toggled(&self, session_id: &str, enabled: bool) {
        self.shared
            .session_event_writer(session_id)
            .emit(Event::YoloToggled { enabled });
        tracing::debug!(%session_id, enabled, "workspace: yolo toggle recorded");
    }
    /// Record an MCP server enable/disable toggle into the session's
    /// `events.jsonl`. Like [`on_yolo_toggled`](Self::on_yolo_toggled), this is
    /// the workspace-side emission point for a shell-owned mutation; the server/shell
    /// forwarding layer calls it when it observes an MCP toggle for a bound
    /// session. A no-op when events recording is disabled.
    pub fn on_mcp_server_toggled(&self, session_id: &str, server_name: &str, enabled: bool) {
        self.shared
            .session_event_writer(session_id)
            .emit(Event::McpServerToggled {
                server_name: server_name.to_owned(),
                enabled,
            });
        tracing::debug!(%session_id, %server_name, enabled, "workspace: mcp toggle recorded");
    }
    /// Returns a cloned snapshot of the hook registry, disconnected
    /// from the workspace's live state.
    ///
    /// The registry is loaded once at workspace construction from the
    /// global and project sources in `WorkspaceConfig`; mid-session
    /// reloads (e.g. plugin hook appending) mutate the live registry
    /// in place via the `RwLock` on `WorkspaceShared`. The returned
    /// clone is not affected by subsequent mutations.
    pub fn hook_registry(&self) -> echo_agent_hooks::discovery::HookRegistry {
        self.shared.hook_registry.read().clone()
    }
    /// Non-fatal errors from the initial hook discovery pass at
    /// workspace construction time.
    ///
    /// Empty when all hook files parsed cleanly. Not updated on
    /// mid-session hook mutations (e.g. plugin hook appending).
    pub fn hook_load_errors(&self) -> &[echo_agent_hooks::error::HookError] {
        &self.shared.hook_load_errors
    }
    /// Canonicalize the workspace root directory.
    /// Called once per batch and passed to `resolve_service_path` for each file.
    pub(crate) async fn canonical_root(&self) -> WorkspaceResult<PathBuf> {
        Self::canonicalize_root_dir(&self.root_cwd()?).await
    }
    /// Canonicalize a confinement root directory.
    async fn canonicalize_root_dir(root: &std::path::Path) -> WorkspaceResult<PathBuf> {
        #[allow(clippy::disallowed_methods)]
        let canonical = tokio::fs::canonicalize(root).await.map_err(|e| {
            WorkspaceError::HubError(format!("failed to canonicalize workspace root: {e}"))
        })?;
        Ok(dunce::simplified(&canonical).to_path_buf())
    }
    /// Resolve a caller-provided path safely. Accepts a path relative to the
    /// workspace root, or an absolute path that resolves within the root;
    /// either form is confined to the root (paths that escape are rejected).
    /// See [`Self::resolve_path_within_root`] for the confinement contract
    /// and its TOCTOU caveat.
    pub(crate) async fn resolve_service_path(
        &self,
        req_path: &str,
        canonical_root: &std::path::Path,
    ) -> WorkspaceResult<PathBuf> {
        let root = self.root_cwd()?;
        Self::resolve_path_within_root(req_path, &root, canonical_root).await
    }
    /// Resolution base for the client-facing fs ops: the bound session's
    /// cwd when it extends the workspace root by a plain path suffix (e.g.
    /// a bind cwd of `<root>/artifacts`), else the root. A suffix that is
    /// missing on disk, not a directory, non-`Normal` (`..`), or whose
    /// canonicalization leaves the root falls back to the root base rather
    /// than failing every op with a confinement error (the bind cwd is
    /// caller-supplied and the artifacts mount is asynchronous).
    pub(crate) async fn client_fs_base(
        &self,
        session_id: Option<&str>,
    ) -> WorkspaceResult<ClientFsBase> {
        let root = self.root_cwd()?;
        let canonical_root = self.canonical_root().await?;
        let suffix = session_id
            .and_then(|id| self.session(id))
            .and_then(|session| {
                let cwd = session.cwd();
                cwd.strip_prefix(&root)
                    .or_else(|_| cwd.strip_prefix(&canonical_root))
                    .ok()
                    .filter(|s| {
                        !s.as_os_str().is_empty()
                            && s.components()
                                .all(|c| matches!(c, std::path::Component::Normal(_)))
                    })
                    .map(std::path::Path::to_path_buf)
            });
        let root_base = || ClientFsBase {
            base: root.clone(),
            canonical: canonical_root.clone(),
        };
        let Some(suffix) = suffix else {
            return Ok(root_base());
        };
        let base = root.join(&suffix);
        #[allow(clippy::disallowed_methods)]
        let canonical = match tokio::fs::canonicalize(&base).await {
            Ok(c) => dunce::simplified(&c).to_path_buf(),
            Err(error) => {
                tracing::warn!(session_id, base = %base.display(), %error,
                    "client-fs base unusable; falling back to the workspace root");
                return Ok(root_base());
            }
        };
        let is_dir = tokio::fs::metadata(&canonical)
            .await
            .is_ok_and(|m| m.is_dir());
        if !canonical.starts_with(&canonical_root) || !is_dir {
            tracing::warn!(session_id, base = %base.display(), canonical = %canonical.display(),
                "client-fs base leaves the root or is not a directory; falling back to the workspace root");
            return Ok(root_base());
        }
        tracing::info!(session_id, base = %base.display(),
            "client-fs ops rebased to the session cwd");
        Ok(ClientFsBase { base, canonical })
    }
    /// Resolve a caller-provided path against an explicit base, confining
    /// it there with two-layer defense: textual normalization + symlink
    /// containment (see [`Self::confine_to_root`]). Entry point for the
    /// client-facing fs ops, and the core of [`Self::resolve_service_path`].
    ///
    /// # TOCTOU caveat
    /// The symlink check is point-in-time. If a symlink is created between
    /// resolution and I/O, containment is not guaranteed. Defense-in-depth
    /// (e.g., `O_NOFOLLOW`, mount namespaces) would be needed for hostile
    /// workspace environments, which is out of scope for this service-level API.
    pub(crate) async fn resolve_path_within_root(
        req_path: &str,
        root: &std::path::Path,
        canonical_root: &std::path::Path,
    ) -> WorkspaceResult<PathBuf> {
        use std::path::{Component, Path};
        if req_path.is_empty() {
            return Err(WorkspaceError::HubError("empty path not allowed".into()));
        }
        let path = Path::new(req_path);
        let joined = if path.is_absolute() {
            path.to_path_buf()
        } else {
            root.join(path)
        };
        let mut components = Vec::new();
        for component in joined.components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    if !components.is_empty()
                        && !matches!(components.last(), Some(Component::RootDir))
                    {
                        components.pop();
                    }
                }
                c => components.push(c),
            }
        }
        let normalized: PathBuf = components.into_iter().collect();
        if !normalized.starts_with(root) && !normalized.starts_with(canonical_root) {
            return Err(WorkspaceError::HubError(format!(
                "path escapes workspace root: {req_path}"
            )));
        }
        const MAX_SYMLINK_HOPS: usize = 40;
        let mut symlink_hops = 0usize;
        let mut check_path = normalized.clone();
        loop {
            #[allow(clippy::disallowed_methods)]
            match tokio::fs::canonicalize(&check_path).await {
                Ok(canonical) => {
                    let canonical = dunce::simplified(&canonical).to_path_buf();
                    if !canonical.starts_with(canonical_root) {
                        return Err(WorkspaceError::HubError(format!(
                            "path resolves outside workspace root (symlink escape): {req_path}"
                        )));
                    }
                    break;
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::NotFound
                        || e.kind() == std::io::ErrorKind::NotADirectory =>
                {
                    if let Ok(md) = tokio::fs::symlink_metadata(&check_path).await
                        && md.file_type().is_symlink()
                    {
                        if symlink_hops >= MAX_SYMLINK_HOPS {
                            return Err(WorkspaceError::HubError(format!(
                                "path resolves outside workspace root (unresolved symlink chain): {req_path}"
                            )));
                        }
                        let Ok(target) = tokio::fs::read_link(&check_path).await else {
                            return Err(WorkspaceError::HubError(format!(
                                "failed to resolve symlink for containment: {req_path}"
                            )));
                        };
                        symlink_hops += 1;
                        check_path = if target.is_absolute() {
                            target
                        } else {
                            check_path
                                .parent()
                                .map(|p| p.join(&target))
                                .unwrap_or(target)
                        };
                        continue;
                    }
                    match check_path.parent() {
                        Some(parent) if parent != check_path => {
                            check_path = parent.to_path_buf();
                        }
                        _ => {
                            tracing::warn!(
                                "symlink containment: parent chain exhausted without canonicalize for {req_path}"
                            );
                            break;
                        }
                    }
                }
                Err(e) => {
                    return Err(WorkspaceError::HubError(format!(
                        "failed to verify path containment: {e}"
                    )));
                }
            }
        }
        Ok(normalized)
    }
    /// Confine `path` to the workspace root (reject `..`, absolute-outside-root,
    /// symlink escapes) when confinement is enabled. Returns the resolved path and
    /// an optional walk root: `Some(root)` confines a `list`, `None` leaves it
    /// unconfined. Off by default (see
    /// [`WorkspaceConfig::confine_fs_to_workspace_root`](crate::config::WorkspaceConfig::confine_fs_to_workspace_root)):
    /// the absolute `path` is returned as-is, following out-of-root symlinks.
    pub async fn confine_to_workspace_root(
        &self,
        path: &std::path::Path,
    ) -> WorkspaceResult<(PathBuf, Option<PathBuf>)> {
        if !self.shared.confine_fs_to_workspace_root {
            return Ok((path.to_path_buf(), None));
        }
        let path_str = path.to_str().ok_or_else(|| {
            WorkspaceError::HubError(format!("non-UTF-8 path: {}", path.display()))
        })?;
        let canonical_root = self.canonical_root().await?;
        let confined = self.resolve_service_path(path_str, &canonical_root).await?;
        Ok((confined, Some(canonical_root)))
    }
    /// Like [`Self::confine_to_workspace_root`] but against an alternative trusted
    /// root (e.g. a worktree session cwd). Same gate; unconfined by default.
    pub async fn confine_to_root(
        &self,
        path: &std::path::Path,
        root: &std::path::Path,
    ) -> WorkspaceResult<(PathBuf, Option<PathBuf>)> {
        if !self.shared.confine_fs_to_workspace_root {
            return Ok((path.to_path_buf(), None));
        }
        let path_str = path.to_str().ok_or_else(|| {
            WorkspaceError::HubError(format!("non-UTF-8 path: {}", path.display()))
        })?;
        let canonical_root = Self::canonicalize_root_dir(root).await?;
        let confined = Self::resolve_path_within_root(path_str, root, &canonical_root).await?;
        Ok((confined, Some(canonical_root)))
    }
    /// Write files to the workspace filesystem (service-level, no hunk tracking).
    ///
    /// Files are written sequentially. If file N fails, files 1..N-1 are
    /// already on disk and will NOT be rolled back. Callers must inspect
    /// per-file results in the response to detect partial failures.
    pub async fn put_files(
        &self,
        session_id: Option<&str>,
        files: Vec<PutFileEntry>,
    ) -> WorkspaceResult<PutFilesRes> {
        let base = self.client_fs_base(session_id).await?;
        let mut results = Vec::with_capacity(files.len());
        for entry in files {
            let result = self.put_single_file(&entry, &base).await;
            results.push(result);
        }
        Ok(PutFilesRes { results })
    }
    async fn put_single_file(&self, entry: &PutFileEntry, base: &ClientFsBase) -> PutFileResult {
        let resolved =
            match Self::resolve_path_within_root(&entry.path, &base.base, &base.canonical).await {
                Ok(p) => p,
                Err(e) => {
                    return PutFileResult {
                        path: entry.path.clone(),
                        ok: false,
                        error: Some(e.to_string()),
                        hash: None,
                    };
                }
            };
        if entry.create_dirs
            && let Some(parent) = resolved.parent()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            return PutFileResult {
                path: entry.path.clone(),
                ok: false,
                error: Some(format!("failed to create directories: {e}")),
                hash: None,
            };
        }
        let write_result = if entry.append {
            use tokio::io::AsyncWriteExt;
            async {
                let mut f = tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&resolved)
                    .await?;
                f.write_all(entry.content.as_bytes()).await?;
                f.flush().await
            }
            .await
        } else {
            tokio::fs::write(&resolved, entry.content.as_bytes()).await
        };
        match write_result {
            Ok(()) => {
                let hash = sha256_hex(entry.content.as_bytes());
                PutFileResult {
                    path: entry.path.clone(),
                    ok: true,
                    error: None,
                    hash: Some(hash),
                }
            }
            Err(e) => PutFileResult {
                path: entry.path.clone(),
                ok: false,
                error: Some(e.to_string()),
                hash: None,
            },
        }
    }
    /// Read files from the workspace filesystem with optional cache
    /// validation and byte-range support.
    ///
    /// Files are read sequentially. Each result includes:
    /// - `exists`: whether the file exists on disk.
    /// - `content`: file content (full or requested byte range as UTF-8).
    /// - `hash`: SHA-256 hex digest of the **full** file content.
    /// - `matched`: true if `if_none_match` matched the current hash.
    /// - `size`: total file size in bytes.
    pub async fn get_files(
        &self,
        session_id: Option<&str>,
        files: Vec<GetFileEntry>,
    ) -> WorkspaceResult<GetFilesRes> {
        let base = self.client_fs_base(session_id).await?;
        let mut results = Vec::with_capacity(files.len());
        for entry in files {
            let result = self.get_single_file(&entry, &base).await;
            results.push(result);
        }
        Ok(GetFilesRes { results })
    }
    async fn get_single_file(&self, entry: &GetFileEntry, base: &ClientFsBase) -> GetFileResult {
        let resolved =
            match Self::resolve_path_within_root(&entry.path, &base.base, &base.canonical).await {
                Ok(p) => p,
                Err(e) => {
                    return GetFileResult {
                        path: entry.path.clone(),
                        exists: false,
                        content: None,
                        hash: None,
                        matched: false,
                        size: None,
                        error: Some(e.to_string()),
                    };
                }
            };
        let is_chunked = entry.offset.is_some() || entry.length.is_some();
        let metadata = match tokio::fs::metadata(&resolved).await {
            Ok(m) => m,
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound
                    || e.kind() == std::io::ErrorKind::NotADirectory =>
            {
                return GetFileResult {
                    path: entry.path.clone(),
                    exists: false,
                    content: None,
                    hash: None,
                    matched: false,
                    size: None,
                    error: None,
                };
            }
            Err(e) => {
                return GetFileResult {
                    path: entry.path.clone(),
                    exists: true,
                    content: None,
                    hash: None,
                    matched: false,
                    size: None,
                    error: Some(e.to_string()),
                };
            }
        };
        let file_size = metadata.len();
        if is_chunked {
            let req_offset = entry.offset.unwrap_or(0);
            let req_length = entry.length.unwrap_or(file_size.saturating_sub(req_offset));
            let read_result = stream_hash_and_range(&resolved, req_offset, req_length).await;
            match read_result {
                Ok((hash, chunk_bytes, _streamed)) => {
                    if let Some(ref etag) = entry.if_none_match
                        && *etag == hash
                    {
                        return GetFileResult {
                            path: entry.path.clone(),
                            exists: true,
                            content: None,
                            hash: Some(hash),
                            matched: true,
                            size: Some(file_size),
                            error: None,
                        };
                    }
                    match String::from_utf8(chunk_bytes) {
                        Ok(content) => GetFileResult {
                            path: entry.path.clone(),
                            exists: true,
                            content: Some(content),
                            hash: Some(hash),
                            matched: false,
                            size: Some(file_size),
                            error: None,
                        },
                        Err(e) => GetFileResult {
                            path: entry.path.clone(),
                            exists: true,
                            content: None,
                            hash: Some(hash),
                            matched: false,
                            size: Some(file_size),
                            error: Some(format!("not valid UTF-8 in range: {e}")),
                        },
                    }
                }
                Err(e) => GetFileResult {
                    path: entry.path.clone(),
                    exists: true,
                    content: None,
                    hash: None,
                    matched: false,
                    size: Some(file_size),
                    error: Some(e.to_string()),
                },
            }
        } else {
            match tokio::fs::read(&resolved).await {
                Ok(bytes) => {
                    let hash = sha256_hex(&bytes);
                    if let Some(ref etag) = entry.if_none_match
                        && *etag == hash
                    {
                        return GetFileResult {
                            path: entry.path.clone(),
                            exists: true,
                            content: None,
                            hash: Some(hash),
                            matched: true,
                            size: Some(file_size),
                            error: None,
                        };
                    }
                    match String::from_utf8(bytes) {
                        Ok(content) => GetFileResult {
                            path: entry.path.clone(),
                            exists: true,
                            content: Some(content),
                            hash: Some(hash),
                            matched: false,
                            size: Some(file_size),
                            error: None,
                        },
                        Err(e) => GetFileResult {
                            path: entry.path.clone(),
                            exists: true,
                            content: None,
                            hash: Some(hash),
                            matched: false,
                            size: Some(file_size),
                            error: Some(format!("not valid UTF-8: {e}")),
                        },
                    }
                }
                Err(e) => GetFileResult {
                    path: entry.path.clone(),
                    exists: true,
                    content: None,
                    hash: None,
                    matched: false,
                    size: Some(file_size),
                    error: Some(e.to_string()),
                },
            }
        }
    }
    /// Open a fuzzy file search index rooted at the workspace cwd.
    pub async fn fuzzy_open(
        &self,
        root: Option<&std::path::Path>,
        request_id: Option<String>,
        hidden: bool,
        session_id: Option<String>,
        target_client_id: crate::file_system::TargetClientId,
    ) -> String {
        let search_root = root.unwrap_or(&self.shared.root_cwd);
        let mut manager = self.shared.fuzzy_searches.lock().await;
        manager.open(
            search_root,
            request_id,
            hidden,
            session_id,
            target_client_id,
        )
    }
    /// Routing info (session id + target client) stored for a search at open
    /// time, read by the notification driver to address status updates.
    pub async fn fuzzy_routing(
        &self,
        search_id: &str,
    ) -> (Option<String>, crate::file_system::TargetClientId) {
        let manager = self.shared.fuzzy_searches.lock().await;
        (
            manager.get_session_id(search_id),
            manager.get_target_client_id(search_id),
        )
    }
    /// Run one poll tick for an active fuzzy search. Returns the next batch of
    /// results (paths absolutized against the search root) or a signal to keep
    /// polling / stop. Drives the `echo.agent/search/fuzzy/status` notification loop.
    pub async fn fuzzy_poll(
        &self,
        search_id: &str,
        min_generation: usize,
        has_query: bool,
        query_version: usize,
        limit: usize,
    ) -> crate::file_system::FuzzyPollOutcome {
        use crate::file_system::FuzzyPollOutcome;
        let mut manager = self.shared.fuzzy_searches.lock().await;
        if !manager.is_current_query(search_id, query_version) {
            return FuzzyPollOutcome::Stale;
        }
        let root = manager.get_root(search_id);
        match manager.get_results_filtered(search_id, min_generation, has_query) {
            None => {
                if manager.get_results(search_id).is_none() {
                    FuzzyPollOutcome::Closed
                } else {
                    FuzzyPollOutcome::Pending
                }
            }
            Some(mut data) => {
                data.matches.truncate(limit);
                if let Some(root) = &root {
                    for m in &mut data.matches {
                        let path_str = m.path.to_string();
                        if !path_str.starts_with('/') {
                            m.path = root.join(&path_str).to_string_lossy().into_owned().into();
                        }
                    }
                }
                FuzzyPollOutcome::Update(data)
            }
        }
    }
    /// Update the query for an active fuzzy search.
    /// Returns (min_generation, has_query, query_version) if the search exists.
    pub async fn fuzzy_change(
        &self,
        search_id: &str,
        query: &str,
        dirs_only: bool,
    ) -> Option<(usize, bool, usize)> {
        let mut manager = self.shared.fuzzy_searches.lock().await;
        manager.change(search_id, query, dirs_only)
    }
    /// Get fuzzy search results.
    pub async fn fuzzy_get_results(
        &self,
        search_id: &str,
    ) -> Option<crate::file_system::FuzzySearchData> {
        let mut manager = self.shared.fuzzy_searches.lock().await;
        manager.get_results(search_id)
    }
    /// Close a fuzzy search.
    pub async fn fuzzy_close(&self, search_id: &str) -> bool {
        let mut manager = self.shared.fuzzy_searches.lock().await;
        manager.close(search_id)
    }
    /// Install the sink used to deliver workspace-originated ext-notifications
    /// to the client (gateway in local mode, hub in proxy mode).
    pub fn set_client_ext_sink(&self, sink: crate::session::ClientExtSink) {
        self.shared.client_ext_sink.store(Arc::new(Some(sink)));
    }
    /// Whether a client ext-notification sink has been installed.
    pub fn has_client_ext_sink(&self) -> bool {
        self.shared.client_ext_sink.load().is_some()
    }
    /// Deliver an ext-notification to the client via the installed sink.
    /// No-op when no sink is set.
    pub fn emit_client_ext(&self, method: String, params: serde_json::Value) {
        if let Some(sink) = self.shared.client_ext_sink.load_full().as_ref() {
            sink(method, params);
        }
    }
    /// Drive the `echo.agent/search/fuzzy/status` stream for an active search: poll
    /// until done / closed / superseded, emitting each new result batch to the
    /// client through the ext-notification sink. Co-located with the manager so
    /// it polls in-process in both local and proxy mode.
    pub async fn run_fuzzy_notifications(
        &self,
        search_id: String,
        min_generation: usize,
        has_query: bool,
        query_version: usize,
        limit: usize,
    ) {
        use crate::file_system::FuzzyPollOutcome;
        use std::time::Duration;
        use tokio::time::interval;
        let (session_id, target_client_id) = self.fuzzy_routing(&search_id).await;
        let context_id = session_id.unwrap_or_else(|| "agent".to_string());
        let mut poll_interval = interval(Duration::from_millis(25));
        let mut last_generation: Option<usize> = None;
        let max_polls = 400;
        poll_interval.tick().await;
        for _ in 0..max_polls {
            poll_interval.tick().await;
            let data = match self
                .fuzzy_poll(&search_id, min_generation, has_query, query_version, limit)
                .await
            {
                FuzzyPollOutcome::Stale | FuzzyPollOutcome::Closed => break,
                FuzzyPollOutcome::Pending => continue,
                FuzzyPollOutcome::Update(data) => data,
            };
            if last_generation == Some(data.generation) {
                if data.done {
                    break;
                }
                continue;
            }
            last_generation = Some(data.generation);
            let mut params = serde_json::json!({
                "sessionId": context_id.as_str(),
                "searchId": search_id.as_str(),
                "matches": serde_json::to_value(&data.matches).unwrap_or_default(),
                "total": data.total,
                "done": data.done,
                "generation": data.generation,
            });
            if !target_client_id.is_none() {
                params["_meta"] = serde_json::json!({
                    "targetClientId": serde_json::to_value(&target_client_id).unwrap_or_default(),
                });
            }
            self.emit_client_ext("echo.agent/search/fuzzy/status".to_string(), params);
            if data.done {
                break;
            }
        }
    }
    /// Run a content search (ripgrep) and return results.
    /// Run a streaming content (ripgrep) search rooted at `cwd`, emitting each
    /// batch as `echo.agent/search/content/status` via the client sink, and returning
    /// the final result. Co-located with the sink so it streams in both modes.
    pub async fn run_content_search(
        &self,
        cwd: std::path::PathBuf,
        context_id: String,
        params: crate::file_system::ContentSearchParams,
    ) -> crate::error::WorkspaceResult<crate::file_system::ContentSearchData> {
        let handle = self.clone();
        crate::file_system::content_search_streaming(&cwd, &params, move |batch| {
            let params = serde_json::json!({
                "sessionId": context_id.as_str(),
                "files": serde_json::to_value(&batch.files).unwrap_or_default(),
                "totalMatches": batch.total_matches,
                "totalFiles": batch.total_files,
                "done": batch.done,
                "truncated": batch.truncated,
            });
            handle.emit_client_ext("echo.agent/search/content/status".to_string(), params);
        })
        .await
        .map_err(|e| WorkspaceError::HubError(e.to_string()))
    }
    pub fn get_or_create_codebase_index(
        &self,
        cwd: std::path::PathBuf,
    ) -> (Arc<echo_agent_codebase_graph::IndexManagerHandle>, bool) {
        self.shared.codebase_indexes.lock().get_or_create(cwd)
    }
    pub fn get_codebase_index(
        &self,
        cwd: &std::path::Path,
    ) -> Option<Arc<echo_agent_codebase_graph::IndexManagerHandle>> {
        self.shared.codebase_indexes.lock().get(cwd)
    }
    pub fn get_covering_codebase_index(
        &self,
        path: &std::path::Path,
    ) -> Option<Arc<echo_agent_codebase_graph::IndexManagerHandle>> {
        self.shared.codebase_indexes.lock().get_covering(path)
    }
    pub fn ensure_codebase_indexes(&self, roots: &[std::path::PathBuf]) {
        self.shared.codebase_indexes.lock().ensure_all(roots);
    }
    fn spawn_codebase_index_event_forwarder(&self) -> tokio::task::JoinHandle<()> {
        let shared = self.shared.clone();
        let root_cwd = self.shared.root_cwd.clone();
        let index_root =
            crate::session::git::find_git_root_from_path(&root_cwd).unwrap_or(root_cwd.clone());
        tokio::spawn(async move {
            let mut rx = shared.events.subscribe();
            loop {
                match rx.recv().await {
                    Ok(echo_agent_workspace_types::WorkspaceEvent::FsChanged {
                        ref path,
                        kind,
                    }) => {
                        let idx = {
                            let indexes = shared.codebase_indexes.lock();
                            indexes
                                .get_covering(path)
                                .or_else(|| indexes.get(&index_root))
                        };
                        if let Some(idx) = idx {
                            let event =
                                crate::fs_notify::ws_event_to_codebase_graph_event(path, kind);
                            if let Err(e) = idx.send_event(event) {
                                tracing::debug!(error = %e, "codebase graph: fs event forward failed");
                            }
                        }
                    }
                    Ok(echo_agent_workspace_types::WorkspaceEvent::GitHeadChanged { .. }) => {
                        let idx_opt = {
                            let indexes = shared.codebase_indexes.lock();
                            indexes
                                .get_covering(&index_root)
                                .or_else(|| indexes.get(&index_root))
                        };
                        if let Some(idx) = idx_opt {
                            crate::fs_notify::refresh_codebase_graph_after_head_change(
                                &idx,
                                &index_root,
                                &shared.events,
                            )
                            .await;
                        }
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(lagged = n, "codebase index event forwarder lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            tracing::debug!("codebase index event forwarder exited");
        })
    }
    /// Re-emit `workspace_tool_definitions.json` on every `ToolsChanged` event,
    /// debounced per session via [`tool_defs_reemit_gate`] so a cascade of
    /// reclassifications does not churn the file. Returns `None` (no task, no
    /// broadcast subscriber) when the feature flag is off; exits when the
    /// broadcast channel closes. The returned handle is tracked on `HubHandle`
    /// so shutdown aborts it — a reconnect must not stack a second subscriber.
    fn spawn_tool_definitions_event_forwarder(&self) -> Option<tokio::task::JoinHandle<()>> {
        if !self.shared.tool_defs_enabled {
            return None;
        }
        let handle = self.clone();
        Some(tokio::spawn(async move {
            let mut rx = handle.shared.events.subscribe();
            loop {
                match rx.recv().await {
                    Ok(echo_agent_workspace_types::WorkspaceEvent::ToolsChanged { session_id }) => {
                        if tool_defs_reemit_gate(
                            handle.shared.tool_defs_enabled,
                            &handle.shared.tool_defs_last_emit,
                            &session_id,
                            std::time::Instant::now(),
                            TOOL_DEFS_DEBOUNCE,
                        ) {
                            handle.emit_workspace_tool_definitions(&session_id);
                        }
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(lagged = n, "tool definitions event forwarder lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            tracing::debug!("tool definitions event forwarder exited");
        }))
    }
    /// Post-creation session setup (browser service seeding, etc.).
    ///
    /// When the optional browser backend is enabled, seeds a fresh per-session `BrowserService`
    /// into the toolset unless one is already present (idempotent — safe
    /// against double-finalize on concurrent on-demand session creation).
    /// Toolset rebuilds carry the handle forward via
    /// [`WorkspaceSession::replace_carrying_browser_service`](crate::session::WorkspaceSession::replace_carrying_browser_service).
    ///
    /// Holds the session's `update_lock` for the whole read-check-insert so
    /// it cannot interleave with a concurrent toolset rebuild (which swaps
    /// in a fresh `FinalizedToolset` under the same lock) — otherwise the
    /// seed could land in a just-replaced, stale toolset and the live one
    /// would miss the browser service.
    ///
    /// Also the initial `workspace_tool_definitions.json` emission point.
    pub(crate) async fn finalize_session_setup(&self, session: &crate::session::WorkspaceSession) {
        let _update_guard = session.update_lock.lock().await;
        self.emit_workspace_tool_definitions(session.session_id());
        self.maybe_emit_environment(session.session_id(), session.cwd());
    }
    /// Emit `workspace_environment.json` once at session bind. Emission is
    /// unconditional except for the legitimate suppression conditions below:
    /// it is a no-op when opted out or when
    /// there is no upload queue. Runs as a tracked producer task so the bind
    /// path never waits on the enqueue and the drain/idle gating still sees the
    /// in-flight work.
    fn maybe_emit_environment(&self, session_id: &str, cwd: &std::path::Path) {
        if self.shared.data_collection_disabled {
            return;
        }
        let trace_parent = fastrace::collector::SpanContext::current_local_parent();
        let this = self.clone();
        let session_id = session_id.to_owned();
        let cwd = cwd.to_path_buf();
        self.spawn_producer(async move {
            let _ = this
                .emit_environment_artifact(&session_id, &cwd, trace_parent)
                .await;
        });
    }
    /// Build and enqueue the environment artifact at the session-root path.
    /// Flag-independent core (the flag check lives in `maybe_emit_environment`)
    /// so it is unit-testable; returns `None` when there is no upload queue.
    async fn emit_environment_artifact(
        &self,
        session_id: &str,
        cwd: &std::path::Path,
        trace_parent: Option<fastrace::collector::SpanContext>,
    ) -> Option<echo_agent_file_utils::queue::EnqueueOutcome> {
        let upload_queue = self.shared.upload_queue.clone()?;
        if !is_safe_object_segment(session_id) {
            tracing::warn!(%session_id, "environment: unsafe session id, skipping");
            return None;
        }
        let env = {
            let session_id_owned = session_id.to_owned();
            let cwd = cwd.to_path_buf();
            let identity = self.shared.identity().clone();
            let server_id = self.shared.server_id();
            let sandbox_id = self.shared.server_metadata_typed().sandbox_id;
            match tokio::task::spawn_blocking(move || {
                crate::upload::environment::WorkspaceEnvironment::capture(
                    &session_id_owned,
                    &cwd,
                    &identity,
                    server_id,
                    sandbox_id,
                )
            })
            .in_span(
                fastrace::Span::root(
                    "tool_server.session_bind.environment_capture",
                    trace_parent.unwrap_or_else(echo_agent_tracing::local_or_random_span_ctx),
                )
                .with_properties(|| {
                    [
                        ("session_id", session_id.to_owned()),
                        ("force_tracing", "true".to_owned()),
                    ]
                }),
            )
            .await
            {
                Ok(env) => env,
                Err(e) if e.is_cancelled() => {
                    tracing::debug!(%session_id, "environment: capture cancelled during shutdown");
                    return None;
                }
                Err(e) => {
                    dc_log!(
                        warn,
                        session_id = %session_id,
                        "workspace: environment capture panicked"
                    );
                    ENV_CAPTURE_PANIC_TOTAL.inc();
                    tracing::warn!(
                        %session_id,
                        error = %e,
                        "workspace: environment capture task panicked"
                    );
                    return None;
                }
            }
        };
        let bytes = match env.to_json_bytes() {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(
                    session_id = %session_id,
                    error = %e,
                    "workspace: failed to serialize workspace_environment.json"
                );
                return None;
            }
        };
        let gcs_path = format!("{session_id}/workspace_environment.json");
        let outcome = upload_queue
            .enqueue_bytes_blocking(
                &bytes,
                &gcs_path,
                "application/json",
                "workspace_environment",
                session_id,
                0,
            )
            .await;
        match &outcome {
            echo_agent_file_utils::queue::EnqueueOutcome::Failed { reason: _ } => {
                dc_log!(
                    warn,
                    session_id = %session_id,
                    error_category = "enqueue_failed",
                    "workspace: environment artifact enqueue failed"
                );
                crate::upload::record_upload_failed("workspace_environment", "enqueue_failed");
                crate::upload::record_upload_outcome("workspace_environment", "failed");
            }
            _ => {
                dc_log!(
                    info,
                    session_id = %session_id,
                    bytes = bytes.len(),
                    "workspace: environment artifact enqueued"
                );
                crate::upload::record_upload_outcome("workspace_environment", "succeeded");
            }
        }
        Some(outcome)
    }
    /// Start MCP servers for a session and bridge them to the server.
    pub async fn start_session_mcp_servers(
        &self,
        session_id: &str,
        configs: Vec<agent_client_protocol::McpServer>,
    ) -> crate::error::WorkspaceResult<crate::mcp::McpStartResult> {
        use crate::mcp::{
            McpClientTransportAdapter, McpStartFailure, McpStartResult, QualifiedMcpToolHandler,
            make_bridge_config, server_name_from_mcp_error,
        };
        use echo_agent_computer_hub_mcp_adapter::McpBridge;
        use echo_agent_computer_hub_sdk::ToolServerHandler as _;
        use echo_agent_mcp::servers::MCP_TOOL_NAME_DELIMITER;
        use echo_agent_tool_protocol::SessionId;
        let tool_server = {
            let hub_guard = self.shared.hub_handle.lock().await;
            let hub = hub_guard
                .as_ref()
                .ok_or_else(|| WorkspaceError::HubError("no hub connection".into()))?;
            hub.server.clone()
        };
        let session = self
            .session(session_id)
            .ok_or_else(|| WorkspaceError::SessionNotFound(session_id.to_owned()))?;
        let sid = SessionId::new(session_id)
            .map_err(|e| WorkspaceError::HubError(format!("invalid session_id: {e}")))?;
        {
            let mut tool_ids = session.mcp_tool_ids.lock().await;
            for tid in tool_ids.drain(..) {
                let _ = tool_server.unregister_tool_dynamic(&tid, &sid).await;
            }
            let mut existing_bridges = session.mcp_bridges.lock().await;
            existing_bridges.clear();
            let mut state = session.mcp_state.lock().await;
            state.owned_clients.clear();
        }
        let session_id_owned = session_id.to_owned();
        let event_writer = self.shared.session_event_writer(session_id);
        let rt_handle = tokio::runtime::Handle::current();
        let mcp_results: Vec<
            Result<echo_agent_mcp::servers::McpClient, echo_agent_mcp::servers::McpError>,
        > = tokio::task::spawn_blocking(move || {
            use echo_agent_mcp::oauth_config::McpOAuthConfigMap;
            use echo_agent_mcp::servers::{McpClientTimeoutOverrides, McpMetaConfigMap};
            use std::collections::HashMap;
            let overrides_map: HashMap<String, McpClientTimeoutOverrides> = HashMap::new();
            let meta_config_map = McpMetaConfigMap::new();
            let oauth_config_map = McpOAuthConfigMap::new();
            let ctx = echo_agent_mcp::servers::McpSpawnCtx::for_session(
                &session_id_owned,
                &event_writer,
                echo_agent_mcp::servers::OauthInteractivity::Interactive,
                None,
            );
            rt_handle.block_on(echo_agent_mcp::servers::start_mcp_servers(
                configs,
                &overrides_map,
                &meta_config_map,
                &oauth_config_map,
                &ctx,
            ))
        })
        .await
        .map_err(|e| WorkspaceError::JoinError(e.to_string()))?;
        let mcp_state = session.mcp_state.clone();
        let mut started = Vec::new();
        let mut failed = Vec::new();
        let mut bridges = Vec::new();
        let mut registered_tool_ids = Vec::new();
        for result in mcp_results {
            match result {
                Ok(client) => {
                    let server_name = client.server_name().to_owned();
                    let client = Arc::new(client);
                    {
                        let mut state = mcp_state.lock().await;
                        state
                            .owned_clients
                            .insert(server_name.clone(), Arc::clone(&client));
                    }
                    let transport: Arc<dyn echo_agent_computer_hub_mcp_adapter::McpTransport> =
                        Arc::new(McpClientTransportAdapter::new(Arc::clone(&client)));
                    let bridge_config = make_bridge_config(sid.clone(), &server_name);
                    match McpBridge::connect(transport, &bridge_config).await {
                        Ok(handle) => {
                            for handler in handle.bridge.handlers() {
                                let qualified_name = format!(
                                    "{}{}{}",
                                    server_name,
                                    MCP_TOOL_NAME_DELIMITER,
                                    handler.tool_id()
                                );
                                let qualified = match QualifiedMcpToolHandler::try_new(
                                    qualified_name.clone(),
                                    handler.clone(),
                                ) {
                                    Some(h) => Arc::new(h),
                                    None => continue,
                                };
                                if let Err(e) = tool_server
                                    .register_tool_dynamic(qualified, vec![sid.clone()])
                                    .await
                                {
                                    tracing::warn!(
                                        server = %server_name,
                                        tool = %qualified_name,
                                        error = %e,
                                        "failed to register MCP tool on hub"
                                    );
                                } else if let Ok(tid) =
                                    echo_agent_tool_protocol::ToolId::new(&qualified_name)
                                {
                                    registered_tool_ids.push(tid);
                                }
                            }
                            bridges.push(handle);
                            started.push(server_name);
                        }
                        Err(e) => {
                            {
                                let mut state = mcp_state.lock().await;
                                state.owned_clients.remove(&server_name);
                            }
                            tracing::warn!(
                                server = %server_name,
                                error = %e,
                                "McpBridge::connect failed"
                            );
                            failed.push(McpStartFailure {
                                name: server_name,
                                error: e.to_string(),
                            });
                        }
                    }
                }
                Err(e) => {
                    let name = server_name_from_mcp_error(&e).to_owned();
                    tracing::warn!(
                        server = %name,
                        error = %e,
                        "MCP server start failed"
                    );
                    failed.push(McpStartFailure {
                        name,
                        error: e.to_string(),
                    });
                }
            }
        }
        {
            let mut session_bridges = session.mcp_bridges.lock().await;
            session_bridges.extend(bridges);
        }
        {
            let mut ids = session.mcp_tool_ids.lock().await;
            ids.extend(registered_tool_ids);
        }
        tracing::info!(
            session_id = %session_id,
            started = ?started,
            failed_count = failed.len(),
            "session MCP servers initialized"
        );
        if !started.is_empty() {
            let _ =
                self.shared
                    .events
                    .send(echo_agent_workspace_types::WorkspaceEvent::ToolsChanged {
                        session_id: session_id.to_owned(),
                    });
        }
        Ok(McpStartResult { started, failed })
    }
    /// Unregister all MCP tools for a session from the server.
    pub async fn teardown_session_mcp(&self, session_id: &str) {
        let tool_server = {
            let hub_guard = self.shared.hub_handle.lock().await;
            match hub_guard.as_ref() {
                Some(hub) => hub.server.clone(),
                None => return,
            }
        };
        let session = match self.session(session_id) {
            Some(s) => s,
            None => return,
        };
        let sid = match echo_agent_tool_protocol::SessionId::new(session_id) {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut tool_ids = session.mcp_tool_ids.lock().await;
        for tid in tool_ids.drain(..) {
            let _ = tool_server.unregister_tool_dynamic(&tid, &sid).await;
        }
        let mut bridges = session.mcp_bridges.lock().await;
        bridges.clear();
        let mut state = session.mcp_state.lock().await;
        state.owned_clients.clear();
    }
    /// Look up an existing session.
    pub fn session(&self, session_id: &str) -> Option<Arc<WorkspaceSession>> {
        self.shared.sessions.read().get(session_id).cloned()
    }
    /// IDs of all sessions currently bound to this workspace.
    pub fn session_ids(&self) -> Vec<String> {
        self.shared.sessions.read().keys().cloned().collect()
    }
    pub fn session_count(&self) -> usize {
        self.shared.sessions.read().len()
    }
    /// Fork a new subagent session. Clones (not references) the parent's
    /// tool config and env. Enforces capability subset and fork budget.
    ///
    /// Forks go through the same post-creation setup as hub-bound sessions
    /// ([`Self::finalize_session_setup`]): each fork gets its own browser
    /// service rather than sharing the parent's tabs.
    pub async fn fork_session(
        &self,
        config: AgentSessionConfig,
    ) -> WorkspaceResult<Arc<WorkspaceSession>> {
        if config.agent_id.is_empty() {
            return Err(WorkspaceError::EmptyAgentId);
        }
        let parent_id = config.parent_session_id.clone().ok_or_else(|| {
            WorkspaceError::ParentSessionNotFound(
                "fork_session requires an explicit parent_session_id".into(),
            )
        })?;
        let parent = self
            .shared
            .sessions
            .read()
            .get(&parent_id)
            .cloned()
            .ok_or_else(|| WorkspaceError::ParentSessionNotFound(parent_id.clone()))?;
        if !config.capability_mode.is_subset_of(parent.capability_mode) {
            return Err(WorkspaceError::CapabilityWidening {
                parent: parent.capability_mode,
                child: config.capability_mode,
            });
        }
        if parent.fork_budget == 0 {
            return Err(WorkspaceError::MaxDepthExceeded { parent: parent_id });
        }
        let new_depth = parent.depth.saturating_add(1);
        let new_fork_budget = parent.fork_budget.saturating_sub(1).min(config.max_depth);
        let baseline = config
            .tool_config
            .clone()
            .unwrap_or_else(|| (*parent.effective_tool_config()).clone());
        let cwd = config
            .cwd_override
            .clone()
            .unwrap_or_else(|| parent.cwd().to_path_buf());
        let mut env: std::collections::HashMap<String, String> = (**parent.session_env()).clone();
        env.extend(config.extra_env.clone());
        let session_env = Arc::new(env);
        let mcp_snapshot = self.shared.mcp_tools_snapshot.load_full();
        let hub_snapshot = self.shared.hub_tools_snapshot.load_full();
        let inherited_viewer_ctx = parent.viewer_ctx().cloned();
        let (effective, toolset, terminal_backend) = resolve_session_toolset(
            baseline,
            config.capability_mode,
            &mcp_snapshot,
            &hub_snapshot,
            cwd.clone(),
            session_env.clone(),
            &config.agent_id,
            self.shared.session_factory.as_ref(),
            Some(self.shared.local_registry.clone()),
            self.shared.lsp.clone(),
            inherited_viewer_ctx.clone(),
            self.shared.compose_session_notification_handle(None),
        )?;
        let (hunk_event_tx, _hunk_event_rx) = tokio::sync::mpsc::unbounded_channel();
        let hunk_cancel = tokio_util::sync::CancellationToken::new();
        let hunk_tracker = HunkTrackerActor::spawn(
            config.agent_id.clone(),
            cwd.clone(),
            hunk_event_tx,
            TrackingMode::AllDirty,
            hunk_cancel.clone(),
        );
        let session = Arc::new(WorkspaceSession::new(
            config.agent_id.clone(),
            cwd,
            session_env,
            config.capability_mode,
            new_depth,
            new_fork_budget,
            Arc::new(effective),
            toolset,
            terminal_backend,
            hunk_tracker,
            Some(hunk_cancel),
            inherited_viewer_ctx,
            false,
            None,
        ));
        if let Some(mapping) = parent.path_virtualization() {
            session.set_path_virtualization(mapping.clone());
        }
        self.insert_session_guarded(&session)?;
        record_toolset_swap(&self.shared.activity_tracker, "fork", session.session_id());
        self.finalize_session_setup(&session).await;
        Ok(session)
    }
    /// Replace the bind-time mount hook. Default is a no-op until a command
    /// is configured; `on_unbind` must not unmount.
    pub fn set_bind_mount_hook(&self, hook: crate::path_virtualization::BindMountHook) {
        self.shared.bind_mount_hook.store(Arc::new(hook));
    }
    fn bind_lifecycle_ctx<'a>(
        session: &'a crate::session::WorkspaceSession,
        real_root: &'a std::path::Path,
    ) -> crate::path_virtualization::BindLifecycleCtx<'a> {
        crate::path_virtualization::BindLifecycleCtx {
            session_id: session.session_id(),
            real_root,
        }
    }
    async fn invoke_bind_mount_hook(
        &self,
        session: &crate::session::WorkspaceSession,
    ) -> Result<(), crate::path_virtualization::BindMountError> {
        let Some(mapping) = session.path_virtualization() else {
            return Ok(());
        };
        let hook = self.shared.bind_mount_hook.load_full();
        let session_id = session.session_id().to_owned();
        let real_root = mapping.real_root_path();
        const BIND_MOUNT_HOOK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
        match tokio::time::timeout(
            BIND_MOUNT_HOOK_TIMEOUT,
            tokio::task::spawn_blocking(move || {
                hook.on_bind(crate::path_virtualization::BindLifecycleCtx {
                    session_id: &session_id,
                    real_root: &real_root,
                })
            }),
        )
        .await
        {
            Ok(join) => join.unwrap_or_else(|e| {
                Err(crate::path_virtualization::BindMountError(format!(
                    "bind mount hook join: {e}"
                )))
            }),
            Err(_) => Err(crate::path_virtualization::BindMountError(format!(
                "bind mount hook timed out after {}s",
                BIND_MOUNT_HOOK_TIMEOUT.as_secs()
            ))),
        }
    }
    pub(crate) fn invoke_unbind_hook(&self, session: &crate::session::WorkspaceSession) {
        let Some(mapping) = session.path_virtualization() else {
            return;
        };
        let real_root = mapping.real_root_path();
        self.shared
            .bind_mount_hook
            .load()
            .on_unbind(Self::bind_lifecycle_ctx(session, &real_root));
    }
    /// Remove a session.
    pub fn drop_session(&self, caller_session_id: &str, session_id: &str) -> WorkspaceResult<()> {
        if caller_session_id != session_id {
            return Err(WorkspaceError::Unauthorized {
                caller: caller_session_id.to_owned(),
                target: session_id.to_owned(),
            });
        }
        let mut sessions = self.shared.sessions.write();
        let Some(session) = sessions.remove(session_id) else {
            return Err(WorkspaceError::SessionNotFound(session_id.to_owned()));
        };
        drop(sessions);
        self.invoke_unbind_hook(&session);
        session.abort_system_notify_producers();
        session.shutdown_terminal_backend();
        session.shutdown_browser_service();
        session.cancel_hunk_tracker();
        self.shared.tool_defs_last_emit.remove(session_id);
        Ok(())
    }
    /// Re-resolve every session's toolset against `new_snapshot` and
    /// emit one `WorkspaceEvent::ToolsChanged` per session.
    pub fn on_mcp_snapshot_changed(
        &self,
        new_snapshot: Vec<echo_agent_tools::registry::types::ToolConfig>,
    ) -> usize {
        self.shared.mcp_tools_snapshot.store(Arc::new(new_snapshot));
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(
                self.shared
                    .re_resolve_all_sessions("mcp_snapshot_changed", true),
            )
        })
    }
    /// Bulk-replace hub tool configs and re-resolve every session.
    pub fn on_hub_tools_changed(
        &self,
        new_hub_tools: Vec<echo_agent_tools::registry::types::ToolConfig>,
    ) -> usize {
        self.shared
            .hub_tools_snapshot
            .store(Arc::new(new_hub_tools));
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(
                self.shared
                    .re_resolve_all_sessions("hub_tools_changed", true),
            )
        })
    }
    /// Per-`session.bind` handler resolver: resolves the bind metadata into a
    /// session toolset (fail-closed in strict mode) and returns the handlers
    /// plus the bind-report fields. Extracted from `connect_hub` so tests can
    /// drive the full bind path without a hub connection.
    pub(crate) fn session_bind_resolver(
        &self,
        catalog: Arc<Vec<Arc<dyn echo_agent_computer_hub_sdk::ToolServerHandler>>>,
        rpc_tool_id: echo_agent_tool_protocol::ToolId,
    ) -> echo_agent_computer_hub_sdk::SessionHandlerResolver {
        let weak_shared = Arc::downgrade(&self.shared);
        Arc::new(
            move |sid: echo_agent_tool_protocol::SessionId, params: Option<serde_json::Value>| {
                let catalog = catalog.clone();
                let rpc_tool_id = rpc_tool_id.clone();
                let weak_shared = weak_shared.clone();
                let bind_parent = params
                    .as_ref()
                    .and_then(|p| p.pointer("/trace_context"))
                    .and_then(serde_json::Value::as_str)
                    .and_then(fastrace::collector::SpanContext::decode_w3c_traceparent)
                    .unwrap_or_else(echo_agent_tracing::local_or_random_span_ctx);
                let bind_span = fastrace::Span::root("tool_server.session_bind", bind_parent)
                    .with_properties(|| {
                        [
                            ("session_id", sid.to_string()),
                            ("force_tracing", "true".to_owned()),
                        ]
                    });
                Box::pin(
                async move {
                    let Some(shared) = weak_shared.upgrade() else {
                        WORKSPACE_BIND_FAILED_TOTAL
                            .with_label_values(&["workspace_shutdown"])
                            .inc();
                        return Err(
                            echo_agent_tool_runtime::ToolError::service_unavailable(
                                "workspace is shutting down; cannot bind session",
                            ),
                        );
                    };
                    let ws = WorkspaceHandle { shared };
                    let sid_str = sid.to_string();
                    let params = params.unwrap_or(serde_json::Value::Null);
                    let bind_cwd = params
                        .pointer("/cwd")
                        .and_then(serde_json::Value::as_str)
                        .map(std::path::PathBuf::from);
                    let bind_config = params
                        .pointer("/metadata")
                        .map(crate::config::WorkspaceBindConfig::from_metadata)
                        .unwrap_or_default();
                    let path_virt = bind_config
                        .session_root
                        .as_deref()
                        .and_then(
                            crate::path_virtualization::PathVirtualization::try_from_session_root,
                        );
                    if bind_config.session_root.is_some() && path_virt.is_none() {
                        tracing::warn!(
                            session_id = %sid_str,
                            session_root = ?bind_config.session_root,
                            "session.bind: ignoring malformed session_root"
                        );
                    }
                    let bind_cwd = match (bind_cwd, &path_virt) {
                        (Some(cwd), Some(v)) => {
                            Some(
                                std::path::PathBuf::from(
                                    v.to_guest(&cwd.to_string_lossy()).into_owned(),
                                ),
                            )
                        }
                        (None, Some(v)) => Some(v.real_root_path()),
                        (cwd, None) => cwd,
                    };
                    let empty_toolset = || echo_agent_tools::registry::types::ToolServerConfig {
                        tools: vec![],
                        behavior_preset: None,
                    };
                    let mut resolve_zero_reason: Option<&'static str> = None;
                    let mut resolve_error: Option<String> = None;
                    let mut unserved_tool_ids: Vec<String> = Vec::new();
                    let known_ids = ws.shared.session_factory.known_tool_ids();
                    let known_id = |id: &str| known_ids.contains(id);
                    let require_explicit = ws.shared.require_explicit_toolset;
                    let tool_config = match bind_config
                        .resolve(&known_id, require_explicit)
                    {
                        crate::config::ResolvedToolset::Toolset(resolved) => {
                            unserved_tool_ids = resolved.unserved_tool_ids;
                            Some(resolved.toolset)
                        }
                        crate::config::ResolvedToolset::UseDefault => None,
                        crate::config::ResolvedToolset::MissingToolConfig => {
                            if bind_config.rpc_only {
                                tracing::info!(
                                    session_id = %sid_str,
                                    "session.bind: rpc_only bind with no toolset — \
                                     failing closed with an empty toolset"
                                );
                            } else {
                                tracing::warn!(
                                    session_id = %sid_str,
                                    "session.bind: no explicit tool configuration passed and this \
                                     workspace requires one — failing closed with an empty toolset"
                                );
                            }
                            resolve_zero_reason = Some("missing_tool_config");
                            resolve_error = Some(
                                format!(
                                "missing_tool_config: no usable explicit tool configuration \
                                 on session.bind (absent, or dropped as malformed — see \
                                 server logs) and this workspace requires one (presets are \
                                 not supported; server version {})",
                                echo_agent_version::VERSION
                            ),
                            );
                            Some(empty_toolset())
                        }
                        crate::config::ResolvedToolset::InvalidToolConfig(err) => {
                            tracing::warn!(
                                session_id = %sid_str, error = %err,
                                "session.bind: invalid tool config entry — failing closed with an empty toolset"
                            );
                            resolve_zero_reason = Some("invalid_tool_config");
                            resolve_error = Some(
                                format!(
                                "invalid_tool_config: {err} (server version {})",
                                echo_agent_version::VERSION
                            ),
                            );
                            Some(empty_toolset())
                        }
                    };
                    let (explicit_cfg, bind_fingerprint) = match (
                        &tool_config,
                        resolve_zero_reason,
                    ) {
                        (Some(cfg), None) if !cfg.tools.is_empty() => {
                            (Some(cfg.clone()), serde_json::to_value(cfg).ok())
                        }
                        _ => (None, None),
                    };
                    let capability = bind_config
                        .capability_mode
                        .unwrap_or(crate::capability::CapabilityMode::All);
                    let yolo_mode = bind_config.yolo_mode.unwrap_or(false);
                    tracing::info!(
                        session_id = %sid_str,
                        cwd = ?bind_cwd,
                        preset = ?bind_config.preset,
                        capability = ?capability,
                        yolo_mode,
                        "session.bind: resolving workspace session toolset"
                    );
                    let bind_cwd_for_rebind = bind_cwd.clone();
                    let created = {
                        let _span = LocalSpan::enter_with_local_parent(
                                "tool_server.session_bind.create_session",
                            )
                            .with_property(|| ("session_id", sid_str.clone()));
                        ws.create_session_with_config(
                            sid_str.clone(),
                            bind_cwd,
                            tool_config,
                            capability,
                            bind_config.viewer_ctx.clone(),
                            bind_config.system_notifications,
                        )
                    };
                    let session = match created {
                        Ok(session) => {
                            session.set_yolo_mode(yolo_mode);
                            session
                                .set_bind_tool_config_fingerprint_if_unset(
                                    bind_fingerprint.clone(),
                                );
                            if let Some(mapping) = path_virt.clone() {
                                session.set_path_virtualization(mapping);
                            }
                            ws.finalize_session_setup(&session)
                                .in_span(
                                    fastrace::Span::enter_with_local_parent(
                                            "tool_server.session_bind.finalize",
                                        )
                                        .with_property(|| ("session_id", sid_str.clone())),
                                )
                                .await;
                            tracing::info!(
                                session_id = %sid_str,
                                "workspace session created for hub bind"
                            );
                            session
                        }
                        Err(crate::error::WorkspaceError::SessionAlreadyExists(_)) => {
                            if let Some(existing) = ws.session(&sid_str)
                                && let Some(mapping) = path_virt.clone()
                            {
                                if let Some(cwd) = bind_cwd_for_rebind.clone()
                                    && let Err(e) = existing
                                        .set_cwd_for_virtualization(cwd)
                                        .await
                                {
                                    return Err(
                                        echo_agent_tool_runtime::ToolError::service_unavailable(
                                            format!(
                                            "path-virt remount failed for `{sid_str}`: {e}"
                                        ),
                                        ),
                                    );
                                }
                                existing.set_path_virtualization(mapping);
                            }
                            match ws
                                .rebind_existing_hub_session(
                                    &sid_str,
                                    explicit_cfg,
                                    bind_fingerprint,
                                )
                                .await
                            {
                                Some((session, RebindOutcome::Reresolved)) => session,
                                Some((session, _)) => {
                                    unserved_tool_ids.clear();
                                    if resolve_zero_reason != Some("invalid_tool_config")
                                        && !session.effective_tool_config().tools.is_empty()
                                    {
                                        resolve_error = None;
                                        resolve_zero_reason = None;
                                    }
                                    session
                                }
                                None => {
                                    WORKSPACE_BIND_FAILED_TOTAL
                                        .with_label_values(&["session_lookup_failed"])
                                        .inc();
                                    return Err(
                                        echo_agent_tool_runtime::ToolError::service_unavailable(
                                            format!(
                                            "session rebind raced teardown for `{sid_str}`; retry"
                                        ),
                                        ),
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!(
                                session_id = %sid_str, error = %e,
                                "failed to create workspace session for hub bind"
                            );
                            WORKSPACE_BIND_FAILED_TOTAL
                                .with_label_values(&["session_error"])
                                .inc();
                            return Err(
                                echo_agent_tool_runtime::ToolError::service_unavailable(
                                    format!("failed to create workspace session: {e}"),
                                ),
                            );
                        }
                    };
                    if let Some(mapping) = path_virt {
                        if let Some(cwd) = bind_cwd_for_rebind
                            && let Err(e) = session.set_cwd_for_virtualization(cwd).await
                        {
                            return Err(
                                echo_agent_tool_runtime::ToolError::service_unavailable(
                                    format!(
                                "path-virt remount failed for `{sid_str}`: {e}"
                            ),
                                ),
                            );
                        }
                        session.set_path_virtualization(mapping);
                    }
                    if let Err(e) = ws.invoke_bind_mount_hook(&session).await {
                        tracing::error!(
                            session_id = %sid_str,
                            error = %e,
                            "session.bind: bind mount hook failed"
                        );
                        WORKSPACE_BIND_FAILED_TOTAL
                            .with_label_values(&["bind_mount_failed"])
                            .inc();
                        let _ = ws.drop_session(&sid_str, &sid_str);
                        return Err(
                            echo_agent_tool_runtime::ToolError::service_unavailable(
                                format!(
                            "bind mount hook failed: {e}"
                        ),
                            ),
                        );
                    }
                    let mut handlers = {
                        let _span = LocalSpan::enter_with_local_parent(
                                "tool_server.session_bind.handlers",
                            )
                            .with_property(|| ("session_id", sid_str.clone()));
                        build_session_routed_handlers(&session.toolset(), &ws)
                    };
                    let advertised: Vec<String> = handlers
                        .iter()
                        .map(|h| h.tool_id().as_str().to_owned())
                        .collect();
                    WORKSPACE_BIND_ADVERTISED_TOOLS.observe(advertised.len() as f64);
                    if advertised.is_empty() {
                        let reason = resolve_zero_reason.unwrap_or("empty_after_filter");
                        let skip_zero_metric = bind_config.rpc_only
                            && reason == "missing_tool_config";
                        if skip_zero_metric {
                            tracing::info!(
                                session_id = %sid_str,
                                reason,
                                "session.bind: advertising zero model-facing tools (rpc_only)"
                            );
                        } else {
                            tracing::warn!(
                                session_id = %sid_str,
                                "session.bind: advertising zero model-facing tools (RPC handler only)"
                            );
                            WORKSPACE_BIND_ZERO_TOOLS_TOTAL
                                .with_label_values(&[reason])
                                .inc();
                        }
                    }
                    handlers
                        .extend(
                            catalog
                                .iter()
                                .filter(|h| h.tool_id() == rpc_tool_id)
                                .cloned(),
                        );
                    if !unserved_tool_ids.is_empty() {
                        WORKSPACE_BIND_UNSERVED_TOOLS_TOTAL
                            .inc_by(unserved_tool_ids.len() as u64);
                        tracing::warn!(
                            session_id = %sid_str,
                            unserved = ?unserved_tool_ids,
                            "session.bind: serving partial pinned toolset"
                        );
                    }
                    tracing::info!(
                        session_id = %sid_str,
                        advertised = advertised.len(),
                        tools = ?advertised,
                        unserved = ?unserved_tool_ids,
                        "session.bind: advertising finalized session toolset"
                    );
                    Ok(echo_agent_computer_hub_sdk::ResolvedSessionHandlers {
                        handlers,
                        unserved_tool_ids,
                        resolve_error,
                    })
                }
                    .in_span(bind_span),
            )
            },
        )
    }
    /// Connect to the server, start the tool server (provider
    /// direction) and notification listener (consumer direction).
    ///
    /// No-op if no `hub_config` was provided or already connected.
    ///
    /// The tool server exposes the workspace's main session tools so
    /// the server can dispatch `tool_call_request` frames to them. The
    /// notification listener updates `hub_tools_snapshot` and
    /// re-resolves every session's toolset whenever the server announces
    /// tool changes.
    pub async fn connect_hub(&self) -> WorkspaceResult<()> {
        use crate::hub::{HubHandle, HubWsTiming, apply_tools_changed, hub_result};
        tracing::info!("WorkspaceHandle::connect_hub — starting");
        let connect_hub_started = std::time::Instant::now();
        let hub_config = match &self.shared.hub_config {
            Some(c) => {
                let mut cfg = c.clone();
                cfg.activity_tracker = Some(self.shared.activity_tracker.clone());
                cfg
            }
            None => {
                tracing::info!("WorkspaceHandle::connect_hub — no hub config, skipping");
                return Ok(());
            }
        };
        let mut hub_guard = self.shared.hub_handle.lock().await;
        if hub_guard.is_some() {
            return Ok(());
        }
        tracing::info!(url = %hub_config.url, "WorkspaceHandle::connect_hub — connecting to hub");
        let catalog_started = std::time::Instant::now();
        let catalog_result = (|| -> WorkspaceResult<_> {
            let session_env = Arc::new(std::collections::HashMap::new());
            let mcp_snapshot = self.shared.mcp_tools_snapshot.load_full();
            let hub_snapshot = self.shared.hub_tools_snapshot.load_full();
            let (_, template_toolset, _template_backend) = resolve_session_toolset(
                self.shared.default_tool_config.clone(),
                crate::capability::CapabilityMode::All,
                &mcp_snapshot,
                &hub_snapshot,
                self.shared.root_cwd.clone(),
                session_env,
                "__template__",
                self.shared.session_factory.as_ref(),
                Some(self.shared.local_registry.clone()),
                self.shared.lsp.clone(),
                None,
                None,
            )?;
            let mut handlers = build_session_routed_handlers(&template_toolset, self);
            let tool_names: Vec<String> = handlers
                .iter()
                .map(|h| h.tool_id().as_str().to_owned())
                .collect();
            let rpc_handler: Arc<dyn echo_agent_computer_hub_sdk::ToolServerHandler> =
                Arc::new(crate::hub_server::WorkspaceRpcHandler::new(self.clone()));
            let rpc_tool_id = rpc_handler.tool_id();
            handlers.push(rpc_handler);
            tracing::info!(
                tool_count = handlers.len(),
                tools = ?tool_names,
                "Registering server tool catalog on hub"
            );
            Ok((handlers, rpc_tool_id))
        })();
        let tool_catalog_secs = catalog_started.elapsed().as_secs_f64();
        let (template_handlers, rpc_tool_id) = match catalog_result {
            Ok(v) => {
                observe_connect_hub_catalog_result(true, tool_catalog_secs, 0.0);
                v
            }
            Err(e) => {
                observe_connect_hub_catalog_result(
                    false,
                    tool_catalog_secs,
                    connect_hub_started.elapsed().as_secs_f64(),
                );
                return Err(e);
            }
        };
        let catalog: Arc<Vec<Arc<dyn echo_agent_computer_hub_sdk::ToolServerHandler>>> =
            Arc::new(template_handlers.clone());
        let resolver = self.session_bind_resolver(catalog, rpc_tool_id);
        let hub_ws_started = std::time::Instant::now();
        let connect_result = HubHandle::connect(
            &hub_config,
            HubWsTiming::from_status(&self.shared.status_config),
            template_handlers,
            self.shared.server_metadata.clone(),
            Some(resolver),
        )
        .await;
        let hub_ws_connect_secs = hub_ws_started.elapsed().as_secs_f64();
        let connect_hub_secs = connect_hub_started.elapsed().as_secs_f64();
        let connect_outcome = if connect_result.is_ok() {
            STARTUP_OUTCOME_OK
        } else {
            STARTUP_OUTCOME_ERROR
        };
        observe_startup_stage(
            STARTUP_STAGE_HUB_WS_CONNECT,
            connect_outcome,
            hub_ws_connect_secs,
        );
        observe_startup_stage(STARTUP_STAGE_CONNECT_HUB, connect_outcome, connect_hub_secs);
        let mut handle = hub_result(connect_result)?;
        tracing::info!(
            tool_catalog_secs,
            hub_ws_connect_secs,
            connect_hub_secs,
            "WorkspaceHandle::connect_hub — connected, starting server + listeners"
        );
        let (activity_notify_handle, activity_notify_rx) =
            echo_agent_tools::notification::types::ToolNotificationHandle::channel();
        let activity_feed_task = tokio::spawn(run_activity_feed(
            self.shared.activity_tracker.clone(),
            activity_notify_rx,
        ));
        handle.set_activity_feed_task(activity_feed_task);
        self.shared
            .activity_notify_handle
            .store(Arc::new(Some(activity_notify_handle)));
        crate::scheduler_liveness::spawn_scheduler_liveness_poll(&self.shared);
        let server = handle.server.clone();
        let server_task = tokio::spawn(async move {
            if let Err(e) = server.run().await {
                tracing::warn!(error = %e, "hub tool server run loop exited with error");
            }
        });
        handle.set_server_task(server_task);
        let mut notification_rx = handle.server.subscribe_notifications();
        let shared = self.shared.clone();
        let listener_task = tokio::spawn(async move {
            while let Some(notification) = notification_rx.recv().await {
                match notification {
                    echo_agent_computer_hub_sdk::HubNotification::ToolsChanged {
                        added,
                        removed,
                        updated,
                        ..
                    } => {
                        let current = shared.hub_tools_snapshot.load_full();
                        let new_tools = apply_tools_changed(&current, &added, &removed, &updated);
                        shared.hub_tools_snapshot.store(Arc::new(new_tools));
                        shared
                            .re_resolve_all_sessions("hub_notification", true)
                            .await;
                    }
                    other => {
                        tracing::debug!(?other, "hub notification (unhandled type)");
                    }
                }
            }
            tracing::debug!("hub notification listener exited");
        });
        handle.set_notification_task(listener_task);
        let hub_warn_threshold = self.shared.status_config.hub_warn_threshold;
        let hub_backoff_base = self.shared.status_config.hub_backoff_base;
        /// Compute exponential backoff: `base` * 2^min(n, 7).
        fn hub_backoff(base: std::time::Duration, consecutive_errors: u32) -> std::time::Duration {
            base.saturating_mul(2u32.pow(consecutive_errors.min(7)))
        }
        let events_rx = self.shared.events.subscribe();
        let server_for_events = handle.server.clone();
        let event_publisher_task = tokio::spawn(async move {
            let mut rx = events_rx;
            let mut consecutive_errors: u32 = 0;
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        let payload =
                            serde_json::to_value(&event).unwrap_or(serde_json::Value::Null);
                        let frame = echo_agent_tool_protocol::ToolNotificationFrame::custom(
                            echo_agent_tool_protocol::ToolId::new(
                                crate::hub_ids::WORKSPACE_EVENTS_TOOL_ID,
                            )
                            .expect("constant tool id"),
                            "workspace_event",
                            payload,
                        );
                        if let Err(e) = server_for_events.send_notification(frame).await {
                            consecutive_errors += 1;
                            if consecutive_errors <= hub_warn_threshold {
                                tracing::warn!(error = %e, "failed to send workspace event to hub");
                            } else {
                                tracing::debug!(error = %e, consecutive = consecutive_errors, "workspace event send failed (backoff)");
                            }
                            tokio::time::sleep(hub_backoff(hub_backoff_base, consecutive_errors))
                                .await;
                        } else {
                            consecutive_errors = 0;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "workspace event publisher lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            tracing::debug!("workspace event publisher exited");
        });
        handle.set_event_publisher_task(event_publisher_task);
        let tracker_for_status = self.shared.activity_tracker.clone();
        let server_conn = handle.server.connection().clone();
        let heartbeat = self.shared.status_config.heartbeat;
        let keepalive = self.shared.status_config.keepalive;
        let status_publisher_task = tokio::spawn(async move {
            /// Attempt to send a status frame.
            ///
            /// Returns `Some(true)` on success, `Some(false)` on transport
            /// failure (hub unreachable), and `None` when the send was
            /// skipped due to a local error (serialization, id allocation)
            /// that does not indicate a dead connection.
            async fn send_status(
                conn: &echo_agent_computer_hub_sdk::HubConnection,
                payload: ToolServerStatusPayload,
            ) -> Option<bool> {
                let params = match serde_json::to_value(&payload) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to serialize tool server status");
                        return None;
                    }
                };
                let request_id = match conn.try_alloc_request_id() {
                    Ok(id) => id,
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to alloc request id for status");
                        return None;
                    }
                };
                let req = echo_agent_tool_protocol::JsonRpcRequest {
                    jsonrpc: echo_agent_tool_protocol::JsonRpcVersion,
                    id: echo_agent_tool_protocol::JsonRpcId::from_request_id(&request_id),
                    session_id: None,
                    method: echo_agent_tool_protocol::Method::ToolServerStatus
                        .as_wire_str()
                        .to_owned(),
                    params,
                };
                if let Err(e) = conn.call_request(request_id, &req).await {
                    tracing::debug!(error = %e, "tool_server.status send failed");
                    return Some(false);
                }
                Some(true)
            }
            fn dedup_key(p: &ToolServerStatusPayload) -> ToolServerStatusPayload {
                let mut k = p.clone();
                k.uptime_ms = 0;
                k
            }
            let mut last_sent: std::collections::HashMap<Option<String>, ToolServerStatusPayload> =
                std::collections::HashMap::new();
            let mut consecutive_errors: u32 = 0;
            let mut last_successful_send = std::time::Instant::now();
            {
                let payload = tracker_for_status.snapshot();
                if send_status(&server_conn, payload.clone()).await == Some(true) {
                    last_sent.insert(None, payload);
                    last_successful_send = std::time::Instant::now();
                }
            }
            const MIN_REPUBLISH_INTERVAL: std::time::Duration =
                std::time::Duration::from_millis(250);
            let mut last_cycle = tokio::time::Instant::now() - MIN_REPUBLISH_INTERVAL;
            loop {
                tracker_for_status.wait_for_change(heartbeat).await;
                let since_last = last_cycle.elapsed();
                if since_last < MIN_REPUBLISH_INTERVAL {
                    tokio::time::sleep(MIN_REPUBLISH_INTERVAL - since_last).await;
                }
                last_cycle = tokio::time::Instant::now();
                let mut any_attempt = false;
                let mut any_success = false;
                let session_ids = tracker_for_status.known_sessions();
                let mut publish = session_ids.clone();
                for sid in last_sent.keys().filter_map(|k| k.as_ref()) {
                    if !publish.contains(sid) {
                        publish.push(sid.clone());
                    }
                }
                let mut closed: Vec<String> = Vec::new();
                for sid in &publish {
                    let payload = tracker_for_status.snapshot_session(sid);
                    let key = Some(sid.clone());
                    let ended = !session_ids.iter().any(|s| s == sid);
                    if last_sent.get(&key).map(dedup_key) == Some(dedup_key(&payload)) {
                        if ended {
                            closed.push(sid.clone());
                        }
                        continue;
                    }
                    if let Some(ok) = send_status(&server_conn, payload.clone()).await {
                        any_attempt = true;
                        if ok {
                            any_success = true;
                            if ended {
                                closed.push(sid.clone());
                            }
                            last_sent.insert(key, payload);
                            last_successful_send = std::time::Instant::now();
                        }
                    }
                }
                last_sent.retain(|k, _| match k {
                    None => true,
                    Some(sid) => {
                        session_ids.iter().any(|s| s == sid)
                            || (any_success && !closed.contains(sid))
                    }
                });
                let payload = tracker_for_status.snapshot();
                let needs_send = last_sent.get(&None).map(dedup_key) != Some(dedup_key(&payload));
                let force_keepalive =
                    !needs_send && !any_success && last_successful_send.elapsed() >= keepalive;
                if (needs_send || force_keepalive)
                    && let Some(ok) = send_status(&server_conn, payload.clone()).await
                {
                    any_attempt = true;
                    if ok {
                        any_success = true;
                        last_sent.insert(None, payload);
                        last_successful_send = std::time::Instant::now();
                    }
                }
                if any_attempt && !any_success {
                    consecutive_errors += 1;
                    if consecutive_errors <= hub_warn_threshold {
                        tracing::warn!(
                            "status publisher: hub unreachable ({} consecutive failed cycles)",
                            consecutive_errors,
                        );
                    } else {
                        tracing::debug!(
                            consecutive = consecutive_errors,
                            "status publish failed (backoff)"
                        );
                    }
                    tokio::time::sleep(hub_backoff(hub_backoff_base, consecutive_errors)).await;
                } else if any_success {
                    consecutive_errors = 0;
                }
            }
        });
        handle.set_status_publisher_task(status_publisher_task);
        {
            let (ext_tx, mut ext_rx) =
                tokio::sync::mpsc::unbounded_channel::<(String, serde_json::Value)>();
            let server_for_ext = handle.server.clone();
            let ext_task = tokio::spawn(async move {
                while let Some((method, params)) = ext_rx.recv().await {
                    let frame = echo_agent_tool_protocol::ToolNotificationFrame::custom(
                        echo_agent_tool_protocol::ToolId::new(
                            crate::hub_ids::WORKSPACE_CLIENT_EXT_NOTIFICATIONS_TOOL_ID,
                        )
                        .expect("constant tool id"),
                        "client_ext_notification",
                        serde_json::json!({ "method": method, "params": params }),
                    );
                    let _ = server_for_ext.send_notification(frame).await;
                }
            });
            handle.set_client_ext_forwarder_task(ext_task);
            self.set_client_ext_sink(Arc::new(move |method, params| {
                let _ = ext_tx.send((method, params));
            }));
        }
        handle.set_codebase_index_forwarder_task(self.spawn_codebase_index_event_forwarder());
        {
            let ws = self.clone();
            tokio::spawn(async move {
                if let Ok(roots) = crate::workspace_ops::materialized_git_roots(&ws).await {
                    ws.ensure_codebase_indexes(&roots);
                }
            });
        }
        if let Some(task) = self.spawn_tool_definitions_event_forwarder() {
            handle.set_tool_defs_forwarder_task(task);
        }
        *hub_guard = Some(handle);
        Ok(())
    }
    /// Shutdown the server connection, if active.
    pub async fn shutdown_hub(&self) {
        let handle = self.shared.hub_handle.lock().await.take();
        if let Some(h) = handle {
            h.shutdown().await;
        }
    }
}
/// Build one [`SessionRoutedToolHandler`](crate::hub::SessionRoutedToolHandler)
/// per tool in `toolset`, keyed by client (function) name. Shared by the
/// connect-time catalog and the per-`session.bind` resolver so the two
/// construction paths cannot drift.
///
/// `finalize` already rejects duplicate client names, so the `seen` set is
/// defense-in-depth: it guards a regression from ever emitting two handlers
/// with the same `tool_id` (which would duplicate the bind response and
/// silently first-win at dispatch).
fn build_session_routed_handlers(
    toolset: &echo_agent_tools::registry::types::FinalizedToolset,
    ws: &WorkspaceHandle,
) -> Vec<Arc<dyn echo_agent_computer_hub_sdk::ToolServerHandler>> {
    let tool_kinds = toolset.tool_kinds();
    let mut seen = std::collections::HashSet::new();
    let mut handlers = Vec::new();
    for def in toolset.tool_definitions() {
        if !seen.insert(def.function.name.clone()) {
            tracing::warn!(
                tool = %def.function.name,
                "duplicate client name in finalized toolset; skipping"
            );
            continue;
        }
        let mut desc = echo_agent_tool_types::ToolDescription::new(
            def.function.name.clone(),
            def.function.description.clone().unwrap_or_default(),
        );
        desc.arguments_schema = Some(def.function.parameters.clone());
        desc.kind = tool_kinds.get(&def.function.name).cloned();
        match crate::hub::SessionRoutedToolHandler::new(
            def.function.name.clone(),
            desc,
            Some(def.function.parameters.clone()),
            ws.clone(),
        ) {
            Ok(handler) => handlers
                .push(Arc::new(handler) as Arc<dyn echo_agent_computer_hub_sdk::ToolServerHandler>),
            Err(e) => {
                tracing::warn!(
                    tool = %def.function.name,
                    error = %e,
                    "client name is not a valid ToolId; skipping hub registration"
                );
            }
        }
    }
    handlers
}
/// Apply a tool notification to the ActivityTracker background-task count.
/// `started` must precede `completed`, else the unknown `completed` no-ops and
/// strands the count.
pub(crate) fn apply_background_task_notification(
    tracker: &crate::activity::ActivityTracker,
    notification: &echo_agent_tools::notification::types::ToolNotification,
) {
    use echo_agent_tools::notification::types::ToolNotification;
    match notification {
        ToolNotification::BashExecutionBackgrounded(bg) => {
            tracker.background_task_started(&bg.task_id);
        }
        ToolNotification::TaskCompleted(snap) => {
            tracker.background_task_completed(&snap.task_id);
        }
        _ => {}
    }
}
/// Tracker-only drain of the session tool-notification stream — not a network
/// send, so the hibernation decrement isn't delayed by send backoff and
/// notifications aren't misattributed across sessions.
pub(crate) async fn run_activity_feed(
    tracker: Arc<crate::activity::ActivityTracker>,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<
        echo_agent_tools::notification::types::ToolNotification,
    >,
) {
    while let Some(notification) = rx.recv().await {
        apply_background_task_notification(&tracker, &notification);
    }
}
/// Compute SHA-256 hex digest.
fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(data))
}
/// What triggered a [`WorkspaceHandle::two_phase_drain`] — the metric label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainReason {
    /// Process received SIGTERM / Ctrl-C (standalone `workspace_server`).
    Sigterm,
    /// Hub sent `tool_server.evict`.
    Evict,
}
impl DrainReason {
    /// Stable `reason` label for `echo_agent_workspace_drain_started_total`.
    pub fn as_str(self) -> &'static str {
        match self {
            DrainReason::Sigterm => "sigterm",
            DrainReason::Evict => "evict",
        }
    }
}
/// Terminal classification of a two-phase drain — the metric label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainOutcome {
    /// Tools, producers, and the upload queue all finished within budget.
    Full,
    /// Tool calls still in flight at the phase-1 deadline.
    Partial,
    /// Producers still in flight at the phase-1.5 deadline (artifacts never queued).
    ProducersTimeout,
    /// Upload-queue deadline exceeded with items still pending (lost on exit).
    Timeout,
}
impl DrainOutcome {
    /// Stable `outcome` label for `echo_agent_workspace_drain_completed_total`.
    pub fn as_str(self) -> &'static str {
        match self {
            DrainOutcome::Full => "full",
            DrainOutcome::Partial => "partial",
            DrainOutcome::ProducersTimeout => "producers_timeout",
            DrainOutcome::Timeout => "timeout",
        }
    }
}
/// Phase-1 (in-flight tool call) budget: one third of the total grace budget.
/// Phases 1.5 and 2 split the remainder.
fn phase1_budget(grace_budget: std::time::Duration) -> std::time::Duration {
    grace_budget / 3
}
/// Phase-1.5 (artifact producer) budget: half the post-phase-1 remainder, so a
/// wedged producer can't starve the phase-2 flush of already-enqueued items.
fn phase15_budget(remaining: std::time::Duration) -> std::time::Duration {
    remaining / 2
}
/// Poll the producer tracker until it reports zero in-flight tasks or `budget`
/// elapses; `true` = idle reached. Replaces `close()` + `wait()` so the
/// tracker stays open (reusable after a non-terminal drain).
async fn wait_for_producers_idle(
    tracker: &tokio_util::task::TaskTracker,
    budget: std::time::Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + budget;
    while !tracker.is_empty() {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    true
}
/// Classify a drain by the earliest phase that blew its deadline:
/// tools (`Partial`) > producers (`ProducersTimeout`) > queue (`Timeout`) >
/// clean (`Full`). `producers_unfinished` is the final producer count after
/// phase 2 (a producer can be spawned *during* phase 2, after `producers_done`
/// was latched in phase 1.5); it is checked so `Full` and the drain marker
/// agree — `Full` requires that no producer work remains, matching the marker /
/// return total (active tool calls + producers + queue), which is `0` only when
/// `tools_idle`, no producers remain, and the queue is empty.
fn classify_drain_outcome(
    tools_idle: bool,
    producers_done: bool,
    producers_unfinished: usize,
    unfinished: usize,
) -> DrainOutcome {
    if !tools_idle {
        DrainOutcome::Partial
    } else if !producers_done || producers_unfinished > 0 {
        DrainOutcome::ProducersTimeout
    } else if unfinished > 0 {
        DrainOutcome::Timeout
    } else {
        DrainOutcome::Full
    }
}
/// The SIGTERM drain budget from `ECHO_AGENT_WORKSPACE_TERMINATION_GRACE_MS`
/// (default [`DEFAULT_TERMINATION_GRACE_MS`]). The hub-evict path uses the
/// hub-provided `grace_period_ms` instead.
pub fn termination_grace_from_env() -> std::time::Duration {
    grace_budget_from_raw(std::env::var("ECHO_AGENT_WORKSPACE_TERMINATION_GRACE_MS").ok())
}
/// Pure parse of the termination-grace env value: a positive integer ms wins,
/// anything else (absent, unparseable, zero) falls back to the default.
fn grace_budget_from_raw(raw: Option<String>) -> std::time::Duration {
    let ms = raw
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&ms| ms > 0)
        .unwrap_or(DEFAULT_TERMINATION_GRACE_MS);
    std::time::Duration::from_millis(ms)
}
/// Path of the preStop drain marker (`ECHO_AGENT_WORKSPACE_DRAINING_FILE` or
/// [`DEFAULT_DRAINING_FILE`]).
fn draining_file_path() -> std::path::PathBuf {
    std::env::var("ECHO_AGENT_WORKSPACE_DRAINING_FILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from(DEFAULT_DRAINING_FILE))
}
/// Atomically write `outstanding` (total durability work still pending: upload
/// queue depth + in-flight artifact producers) to the drain marker (temp +
/// fsync + rename) so the preStop hook never reads a torn value and never sees
/// `0` while a producer could still enqueue. Best-effort. The temp name is
/// unique (pid + counter) so concurrent evict drains don't race on a fixed
/// `.tmp`.
fn write_draining_marker(path: &std::path::Path, outstanding: usize) {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static TMP_SEQ: AtomicU64 = AtomicU64::new(0);
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(
        ".{}.{}.draining.tmp",
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let tmp = std::path::PathBuf::from(tmp);
    let result = std::fs::File::create(&tmp)
        .and_then(|mut f| {
            f.write_all(outstanding.to_string().as_bytes())?;
            f.sync_all()
        })
        .and_then(|()| std::fs::rename(&tmp, path));
    if let Err(e) = result {
        tracing::warn!(path = %path.display(), error = %e, "failed to write drain marker");
        let _ = std::fs::remove_file(&tmp);
    }
}
/// Stream a file once: SHA-256 over every byte while capturing the
/// `[offset, offset + length)` overlap. Returns
/// `(hash_hex, range_bytes, total_streamed_bytes)`.
///
/// Shared by [`WorkspaceHandle::get_files`]' chunked reads and the
/// `file_system::client_fs` ops so the overlap arithmetic lives in one
/// place.
pub(crate) async fn stream_hash_and_range(
    path: &std::path::Path,
    offset: u64,
    length: u64,
) -> std::io::Result<(String, Vec<u8>, u64)> {
    use sha2::{Digest, Sha256};
    use tokio::io::AsyncReadExt;
    let req_end = offset.saturating_add(length);
    let mut f = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut chunk = Vec::new();
    let mut pos: u64 = 0;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        let start = pos.max(offset);
        let end = (pos + n as u64).min(req_end);
        if start < end {
            let local_start = (start - pos) as usize;
            let local_end = (end - pos) as usize;
            chunk.extend_from_slice(&buf[local_start..local_end]);
        }
        pos += n as u64;
    }
    Ok((format!("{:x}", hasher.finalize()), chunk, pos))
}
/// Create a [`WorkspaceHandle`] and connect it to the hub.
///
/// This is the shared setup used by both the standalone `workspace_server`
/// binary and the TUI's in-process local workspace server. The workspace
/// registers its tools on the server so external clients can reach them.
/// Sessions are bound dynamically by clients calling `bind_server`.
///
/// `confine_fs_to_workspace_root` confines `echo.agent/fs/*` resolution to the root.
/// The standalone workspace server defaults it on (it always backs a remote
/// sandbox; override via `ECHO_AGENT_WORKSPACE_CONFINE_FS_TO_ROOT`); the CLI leader
/// passes `false`.
///
/// Returns the connected handle (caller should keep it alive for the
/// lifetime of the server connection).
pub async fn connect_local_workspace(
    cwd: std::path::PathBuf,
    hub_url: url::Url,
    auth: echo_agent_computer_hub_sdk::SharedAuthProvider,
    metadata: Option<serde_json::Value>,
    server_id: Option<String>,
    alpha_test_key: Option<String>,
    allow_insecure_ws: bool,
    status_config: crate::status_config::StatusConfig,
    upload_queue_enabled: bool,
    project_lsp_trusted: bool,
    diag: Option<DiagHandle>,
    require_explicit_toolset: bool,
    confine_fs_to_workspace_root: bool,
) -> WorkspaceResult<WorkspaceHandle> {
    use crate::session::tool_config::WorkspaceSessionContextFactory;
    let time_to_ready_started = std::time::Instant::now();
    let identity: crate::upload::environment::WorkspaceIdentity =
        auth.identity().map(Into::into).unwrap_or_default();
    let workspace_home = resolve_workspace_home();
    std::fs::create_dir_all(&workspace_home).map_err(|e| {
        WorkspaceError::HubError(format!(
            "failed to create workspace home {}: {e}",
            workspace_home.display()
        ))
    })?;
    let api_base_url = std::env::var("ECHO_AGENT_CLI_CHAT_PROXY_BASE_URL")
        .unwrap_or_else(|_| "https://chat-proxy.echo-agent.invalid/v1".to_string());
    let data_collection_disabled =
        std::env::var("ECHO_AGENT_WORKSPACE_DATA_COLLECTION_DISABLED").as_deref() != Ok("false");
    let mut factory = WorkspaceSessionContextFactory::with_auth(auth.clone(), api_base_url.clone());
    if crate::session::tool_config::tool_state_enabled() {
        factory = factory.with_tool_state_home(workspace_home.clone());
    }
    let hub_cfg = crate::hub::HubConfig {
        url: hub_url,
        auth: auth.clone(),
        activity_tracker: None,
        server_id,
        alpha_test_key,
        allow_insecure_ws,
        diag,
    };
    let tool_config = echo_agent_core::workspace_echo_agent_build_toolset();
    let mut ws_config = WorkspaceConfig::new_for_proxy(
        cwd,
        Arc::new(factory),
        hub_cfg,
        auth.clone(),
        metadata,
        status_config,
        tool_config,
    );
    ws_config.project_lsp_trusted = project_lsp_trusted;
    ws_config.require_explicit_toolset = require_explicit_toolset;
    ws_config.confine_fs_to_workspace_root = confine_fs_to_workspace_root;
    if let Ok(dir) = std::env::var("ECHO_AGENT_WORKSPACE_SERVER_SKILLS_DIR")
        && !dir.is_empty()
    {
        ws_config.skills_config.server_skill_dirs = vec![dir];
    }
    if let Ok(dir) = std::env::var("ECHO_AGENT_WORKSPACE_BUNDLED_SKILLS_DIR")
        && !dir.is_empty()
    {
        let allowlist = std::env::var("ECHO_AGENT_WORKSPACE_BUNDLED_SKILLS_ALLOWLIST").ok();
        ws_config
            .skills_config
            .ignore
            .extend(bundled_allowlist_ignore_dirs(&dir, allowlist.as_deref()));
        ws_config.skills_config.bundled_skill_dirs = vec![dir];
    }
    let proxy_storage = Arc::new(crate::upload::ProxyStorageConfig::new(
        auth.clone(),
        api_base_url.clone(),
        identity.clone(),
    ));
    let trace_source: Arc<dyn echo_agent_file_utils::queue::TraceExportSource> = Arc::new(
        crate::upload::WorkspaceTraceExportSource::new(proxy_storage.clone()),
    );
    let upload_queue = Arc::new(echo_agent_file_utils::queue::UploadQueue::spawn(
        &workspace_home,
        trace_source,
        echo_agent_file_utils::queue::UploadRetryPolicy::default(),
    ));
    {
        let recovery_started = std::time::Instant::now();
        if data_collection_disabled {
            crate::recovery::purge_spilled_items(&workspace_home);
        } else {
            let report =
                crate::recovery::run_startup_recovery(&workspace_home, &upload_queue).await;
            tracing::info!(?report, "workspace startup restart-recovery scan complete");
        }
        observe_startup_stage(
            STARTUP_STAGE_STARTUP_RECOVERY,
            STARTUP_OUTCOME_OK,
            recovery_started.elapsed().as_secs_f64(),
        );
    }
    upload_queue.cleanup_orphans(echo_agent_file_utils::queue::DEFAULT_MAX_AGE);
    crate::upload::spawn_queue_stats_sampler(
        upload_queue.clone(),
        std::time::Duration::from_secs(15),
    );
    if crate::session::tool_config::tool_state_enabled() {
        let home = workspace_home.clone();
        tokio::spawn(async move {
            crate::recovery::cleanup_stale_sessions(
                &home,
                crate::recovery::DEFAULT_SESSION_MAX_AGE,
            )
            .await;
        });
    }
    tokio::task::spawn_blocking(|| {
        crate::worktree::run_auto_gc_best_effort();
    });
    let ws_handle = WorkspaceHandle::new_with_data_collection(
        ws_config,
        workspace_home,
        upload_queue,
        upload_queue_enabled,
        data_collection_disabled,
        identity,
    )
    .map_err(|e| WorkspaceError::HubError(format!("failed to create workspace: {e}")))?;
    let connect_result = ws_handle.connect_hub().await;
    observe_startup_stage(
        STARTUP_STAGE_TIME_TO_READY,
        if connect_result.is_ok() {
            STARTUP_OUTCOME_OK
        } else {
            STARTUP_OUTCOME_ERROR
        },
        time_to_ready_started.elapsed().as_secs_f64(),
    );
    connect_result?;
    Ok(ws_handle)
}
/// Resolve `$ECHO_AGENT_WORKSPACE_HOME` — the workspace-owned on-disk state root.
///
/// Precedence:
/// 1. `$ECHO_AGENT_WORKSPACE_HOME` (operator override).
/// 2. `<echo_agent_home>/workspace`, where `<echo_agent_home>` honours `$ECHO_AGENT_HOME` and
///    otherwise falls back to `~/.echo-agent` (see [`echo_agent_config::echo_agent_home`]).
pub fn resolve_workspace_home() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("ECHO_AGENT_WORKSPACE_HOME")
        && !p.trim().is_empty()
    {
        return std::path::PathBuf::from(p);
    }
    echo_agent_config::echo_agent_home().join("workspace")
}
/// Skill `ignore` entries for the allow-list: subdirs of `dir` not in the
/// comma-separated list (`bundled__` prefix optional). Unreadable `dir` fails
/// closed (ignore `dir` itself).
///
/// Unset and set-but-empty differ: unset means no filtering at all, empty means
/// advertise none. The sandbox service relies on that to forward the tri-state
/// of `AgentSandboxStartRequest.bundled_skills`.
fn bundled_allowlist_ignore_dirs(dir: &str, allowlist: Option<&str>) -> Vec<String> {
    let Some(allowlist) = allowlist else {
        return vec![];
    };
    let allowed: std::collections::HashSet<&str> = allowlist
        .split(',')
        .map(|s| s.trim())
        .map(|s| s.strip_prefix("bundled__").unwrap_or(s))
        .filter(|s| !s.is_empty())
        .collect();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            tracing::warn!(
                dir,
                %err,
                "bundled skills dir unreadable; allow-list ignores the whole dir"
            );
            return vec![dir.to_string()];
        }
    };
    let mut dirs: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let stripped = name.strip_prefix("bundled__").unwrap_or(&name);
            !allowed.contains(stripped)
        })
        .map(|entry| entry.path().to_string_lossy().into_owned())
        .collect();
    dirs.sort();
    dirs
}
/// Whether per-session `events.jsonl` recording is enabled
/// (`ECHO_AGENT_WORKSPACE_EVENTS_ENABLED=true`). Any other value — including unset —
/// keeps the legacy behaviour: [`WorkspaceShared::session_event_writer`] hands
/// back [`EventWriter::noop()`](echo_agent_session_events::EventWriter::noop)
/// and no `events.jsonl` is ever opened.
fn events_enabled() -> bool {
    std::env::var("ECHO_AGENT_WORKSPACE_EVENTS_ENABLED").as_deref() == Ok("true")
}
/// Watchdog for awaiting enqueue outcomes when answering an `After` turn
/// hook. MUST undercut the requester's 10s hook deadline or the reply (and
/// its ack) arrives after the requester gave up. Default 8s; override via
/// `ECHO_AGENT_WORKSPACE_AFTER_TURN_WATCHDOG_MS` (malformed values fall back).
fn after_turn_watchdog() -> std::time::Duration {
    const DEFAULT_MS: u64 = 8_000;
    let ms = std::env::var("ECHO_AGENT_WORKSPACE_AFTER_TURN_WATCHDOG_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MS);
    std::time::Duration::from_millis(ms)
}
/// Whether per-session `workspace_tool_definitions.json` emission is enabled
/// (`ECHO_AGENT_WORKSPACE_TOOL_DEFS_ENABLED=true`; any other value keeps legacy
/// behaviour).
fn tool_defs_enabled() -> bool {
    std::env::var("ECHO_AGENT_WORKSPACE_TOOL_DEFS_ENABLED").as_deref() == Ok("true")
}
/// Debounce window for `ToolsChanged`-driven re-emission: at most one re-emit
/// per session per window.
pub(crate) const TOOL_DEFS_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(5);
/// Session-root GCS object path for a session's workspace-side tool
/// definitions (same cadence convention as `workspace_environment.json`).
fn workspace_tool_definitions_path(session_id: &str) -> String {
    format!("{session_id}/workspace_tool_definitions.json")
}
/// Whether `s` is safe to interpolate as the leading segment of a GCS object
/// key: non-empty, no separators, `..`, or NUL (RPC ids are a trust boundary).
fn is_safe_object_segment(s: &str) -> bool {
    !s.is_empty() && !s.contains('/') && !s.contains('\\') && !s.contains("..") && !s.contains('\0')
}
/// Per-session re-emit gate: `true` (recording `now` as last-emit) only when
/// `enabled` and at least `window` elapsed since the previous re-emit. Disabled
/// records no state, so flipping the flag on later is never pre-empted by
/// suppressed-while-off events; the check-and-set is atomic via the dashmap
/// entry API so concurrent events for one session cannot both pass.
fn tool_defs_reemit_gate(
    enabled: bool,
    last_emit: &dashmap::DashMap<String, std::time::Instant>,
    session_id: &str,
    now: std::time::Instant,
    window: std::time::Duration,
) -> bool {
    if !enabled {
        return false;
    }
    if let Some(prev) = last_emit.get(session_id)
        && now.saturating_duration_since(*prev) < window
    {
        return false;
    }
    use dashmap::mapref::entry::Entry;
    match last_emit.entry(session_id.to_owned()) {
        Entry::Occupied(mut e) => {
            if now.saturating_duration_since(*e.get()) >= window {
                e.insert(now);
                true
            } else {
                false
            }
        }
        Entry::Vacant(e) => {
            e.insert(now);
            true
        }
    }
}
/// Enqueue serialized workspace tool definitions at `object_path`, mapping the
/// outcome to a log line. Shared by `emit_workspace_tool_definitions` (which
/// spawns it) and the unit tests (which await it).
async fn enqueue_workspace_tool_definitions(
    upload_queue: &echo_agent_file_utils::queue::UploadQueue,
    session_id: &str,
    object_path: &str,
    bytes: &[u8],
) -> echo_agent_file_utils::queue::EnqueueOutcome {
    use echo_agent_file_utils::queue::EnqueueOutcome;
    let outcome = upload_queue
        .enqueue_bytes_blocking(
            bytes,
            object_path,
            "application/json",
            "workspace_tool_definitions",
            session_id,
            0,
        )
        .await;
    match &outcome {
        EnqueueOutcome::Enqueued
        | EnqueueOutcome::FellBackToInline
        | EnqueueOutcome::Deduplicated
        | EnqueueOutcome::Skipped { .. } => {
            tracing::info!(
                %session_id,
                object_path = %object_path,
                bytes = bytes.len(),
                outcome = ?outcome,
                "workspace: tool definitions enqueued"
            );
        }
        EnqueueOutcome::Failed { reason } => {
            tracing::warn!(
                %session_id,
                object_path = %object_path,
                error = %reason,
                "workspace: tool definitions enqueue failed"
            );
        }
    }
    outcome
}
/// Single source of truth for mapping a turn-hook outcome to the `events.jsonl`
/// [`TurnOutcomeLabel`]. Kept as one `match` so the two enums cannot drift and
/// the mapping is never duplicated across call sites.
fn turn_outcome_label(
    outcome: echo_agent_tool_protocol::turn_hook::TurnHookOutcome,
) -> TurnOutcomeLabel {
    use echo_agent_tool_protocol::turn_hook::TurnHookOutcome;
    match outcome {
        TurnHookOutcome::Completed => TurnOutcomeLabel::Completed,
        TurnHookOutcome::Cancelled => TurnOutcomeLabel::Cancelled,
        TurnHookOutcome::Error => TurnOutcomeLabel::Error,
        _ => TurnOutcomeLabel::Error,
    }
}
/// Decode the wire `session_relationship` string into the `events.jsonl`
/// enum. Unknown values map to the safe default `Primary`; the snake_case
/// forms are pinned by `session_relationship_wire_forms_round_trip`.
fn decode_session_relationship(s: &str) -> SessionRelationship {
    match s {
        "subagent" => SessionRelationship::Subagent,
        _ => SessionRelationship::Primary,
    }
}
/// Decode the bare snake_case `cancellation_category` string into the
/// `events.jsonl` enum; unrecognised values decode to `None` rather than
/// failing the whole `TurnEnded` emission.
fn decode_cancellation_category(s: Option<&str>) -> Option<CancellationCategory> {
    s.and_then(|s| {
        serde_json::from_value::<CancellationCategory>(serde_json::Value::String(s.to_owned())).ok()
    })
}
/// Await both per-phase enqueue handles and reduce them to the wire ack triple
/// `(status, artifact_count, error_message)`. No handles at all means nothing
/// is on disk → `Skipped` with `no_handle_skip_reason` as the diagnostic.
async fn resolve_after_turn_ack(
    before_handle: Option<tokio::task::JoinHandle<EnqueueOutcome>>,
    after_handle: Option<tokio::task::JoinHandle<EnqueueOutcome>>,
    watchdog: std::time::Duration,
    no_handle_skip_reason: &str,
) -> (AfterTurnAckStatus, u32, Option<String>) {
    if before_handle.is_none() && after_handle.is_none() {
        return (
            AfterTurnAckStatus::Skipped,
            0,
            Some(no_handle_skip_reason.to_owned()),
        );
    }
    let (before, after) = tokio::join!(
        await_enqueue_outcome(before_handle, watchdog, "before_enqueue"),
        await_enqueue_outcome(after_handle, watchdog, "after_enqueue"),
    );
    reduce_enqueue_outcomes(&before, &after)
}
/// Await one enqueue handle under a watchdog, mapping every failure mode
/// (missing handle, join error, timeout) to [`EnqueueOutcome::Failed`]. On
/// timeout the task is detached, not aborted — we only stop blocking the ack.
async fn await_enqueue_outcome(
    handle: Option<tokio::task::JoinHandle<EnqueueOutcome>>,
    watchdog: std::time::Duration,
    phase: &str,
) -> EnqueueOutcome {
    let Some(handle) = handle else {
        return EnqueueOutcome::Failed {
            reason: format!("no inflight enqueue for {phase}"),
        };
    };
    match tokio::time::timeout(watchdog, handle).await {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(join_err)) => EnqueueOutcome::Failed {
            reason: format!("{phase} enqueue task failed to join: {join_err}"),
        },
        Err(_elapsed) => EnqueueOutcome::Failed {
            reason: "watchdog_timeout".to_owned(),
        },
    }
}
/// Reduce the two per-phase [`EnqueueOutcome`]s to the wire ack triple.
/// `artifact_count` counts only durably-spilled phases (`FellBackToInline` is
/// a success for `status` but not durable, so it does not count); any `Failed`
/// wins the `status`, carrying the first failure reason. [`EnqueueOutcome::Skipped`]
/// (e.g. collect deadline) is a non-failure and not a durable enqueue. The
/// no-handle case is handled by [`resolve_after_turn_ack`].
fn reduce_enqueue_outcomes(
    before: &EnqueueOutcome,
    after: &EnqueueOutcome,
) -> (AfterTurnAckStatus, u32, Option<String>) {
    let durable = |o: &EnqueueOutcome| matches!(o, EnqueueOutcome::Enqueued);
    let artifact_count = durable(before) as u32 + durable(after) as u32;
    let first_failure = [before, after].into_iter().find_map(|o| match o {
        EnqueueOutcome::Failed { reason } => Some(reason.clone()),
        EnqueueOutcome::Enqueued
        | EnqueueOutcome::FellBackToInline
        | EnqueueOutcome::Deduplicated
        | EnqueueOutcome::Skipped { .. } => None,
    });
    match first_failure {
        Some(reason) => (AfterTurnAckStatus::Failed, artifact_count, Some(reason)),
        None => (AfterTurnAckStatus::Enqueued, artifact_count, None),
    }
}
/// Per-process ephemeral workspace home for handles constructed without a
/// backing upload queue (tests, local mode). Never the real echoagent home —
/// only [`connect_local_workspace`] resolves `$ECHO_AGENT_WORKSPACE_HOME` — so the
/// queue-less default path can never collide with a real workspace's state dir.
fn ephemeral_workspace_home() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "echo-agent-workspace-ephemeral-{}",
        std::process::id()
    ))
}
/// Resolve `workspace_rewind_all_outcomes` from `ECHO_AGENT_WORKSPACE_REWIND_ALL_OUTCOMES` (default off).
fn rewind_all_outcomes_from_env() -> bool {
    echo_agent_config::env_bool("ECHO_AGENT_WORKSPACE_REWIND_ALL_OUTCOMES").unwrap_or(false)
}
/// Flush the session toolset's `ResourcesPersistence` to disk (a fresh
/// snapshot, waiting for the atomic-rename write to land), then read the bytes
/// back and enqueue them for the given turn. Extracted from
/// `spawn_tool_state_upload` so the path is unit-testable without a live turn.
async fn persist_and_enqueue_tool_state(
    session: Arc<crate::session::WorkspaceSession>,
    session_id: String,
    turn_number: u64,
    upload_queue: Arc<echo_agent_file_utils::queue::UploadQueue>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let toolset = session.toolset();
    let Some(state_path) = toolset
        .save_and_flush_persistence()
        .await
        .map(std::path::Path::to_path_buf)
    else {
        dc_log!(
            debug,
            session_id = %session_id,
            turn_number,
            phase = "tool_state",
            outcome = "skipped",
            skip_reason = "no_state_path",
            "workspace: tool_state upload skipped, session has no state directory"
        );
        crate::upload::record_upload_outcome("tool_state", "skipped");
        crate::upload::record_upload_skipped("tool_state", "no_state_path");
        return Ok(());
    };
    let bytes = tokio::fs::read(&state_path).await.map_err(|e| {
        format!(
            "failed to read flushed tool_state from {}: {e}",
            state_path.display()
        )
    })?;
    crate::upload::upload_tool_state_queued(bytes, session_id, turn_number, upload_queue).await
}
/// `ToolHandle` adapter that delegates to a workspace session's
/// [`FinalizedToolset`]. Used by [`WorkspaceHandle::create_local_harness`]
/// to populate a [`LocalRegistry`] for in-process tool dispatch.
///
/// This is the same dispatch pattern as [`SessionRoutedToolHandler`] in
/// `hub.rs`, but implements `ToolHandle` (for `LocalRegistry`) instead
/// of `ToolServerHandler` (for `ToolServer`).
struct SessionToolHandle {
    tool_id: echo_agent_tool_protocol::ToolId,
    desc: echo_agent_tool_types::ToolDescription,
    workspace: WorkspaceHandle,
    session_id: String,
}
impl SessionToolHandle {
    fn new(
        tool_name: String,
        desc: echo_agent_tool_types::ToolDescription,
        workspace: WorkspaceHandle,
        session_id: String,
    ) -> Result<Self, echo_agent_tool_protocol::IdError> {
        Ok(Self {
            tool_id: echo_agent_tool_protocol::ToolId::new(tool_name)?,
            desc,
            workspace,
            session_id,
        })
    }
    fn name(&self) -> &str {
        self.tool_id.as_str()
    }
}
impl std::fmt::Debug for SessionToolHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionToolHandle")
            .field("tool_name", &self.name())
            .field("session_id", &self.session_id)
            .finish()
    }
}
#[async_trait::async_trait]
impl echo_agent_tool_runtime::ToolDyn for SessionToolHandle {
    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        self.tool_id.clone()
    }
    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        self.desc.clone()
    }
    async fn execute(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        args: serde_json::Value,
    ) -> echo_agent_tool_runtime::ToolStream<echo_agent_tool_runtime::TypedToolOutput> {
        use echo_agent_tool_runtime::{ToolError, ToolErrorKind, ToolStreamItem, terminal_only};
        let session = match self.workspace.session(&self.session_id) {
            Some(s) => s,
            None => {
                return terminal_only(Err(ToolError::new(
                    ToolErrorKind::InvalidArguments,
                    format!("session not bound: {}", self.session_id),
                )));
            }
        };
        let toolset = session.toolset();
        let call_id = ctx.call_id.to_string();
        let tool_id = self.id();
        let tool_name = self.name().to_owned();
        let session_label = self.session_id.clone();
        tracing::debug!(
            tool = %self.name(),
            call_id = %call_id,
            session = %self.session_id,
            "local harness: dispatching tool call"
        );
        let virt = session.path_virtualization().cloned();
        let args = match &virt {
            Some(v) => v.rewrite_json_inbound(args),
            None => args,
        };
        let inner = toolset.call_streaming(self.name(), args, &call_id, None);
        Box::pin(async_stream::stream! {
            use futures::StreamExt;
            let mut inner = inner;
            while let Some(item) = inner.next().await {
                match item {
                    // Rollout gate lives downstream in the sampler.
                    ToolStreamItem::Progress(p) => {
                        let p = match &virt {
                            Some(v) => v.rewrite_progress(p),
                            None => p,
                        };
                        yield ToolStreamItem::Progress(p);
                    }
                    ToolStreamItem::Terminal(Ok(run_result)) => {
                        let output = run_result.into_typed_tool_output(tool_id);
                        let output = match &virt {
                            Some(v) => v.rewrite_typed_output(output),
                            None => output,
                        };
                        yield ToolStreamItem::Terminal(Ok(output));
                        return;
                    }
                    ToolStreamItem::Terminal(Err(e)) => {
                        tracing::error!(
                            tool = %tool_name,
                            session = %session_label,
                            error = %e,
                            "local harness tool call failed"
                        );
                        let detail = match &virt {
                            Some(v) => v.rewrite_error(e).to_string(),
                            None => e.to_string(),
                        };
                        yield ToolStreamItem::Terminal(Err(ToolError::new(
                            ToolErrorKind::TerminalError,
                            detail,
                        )));
                        return;
                    }
                }
            }
            // Defensive fallback: every terminal arm above `return`s, so this
            // is only reached if the inner `call_streaming` stream ended
            // without a terminal. That is unreachable under the
            // `call_streaming` contract (it yields exactly one terminal on
            // every code path), but emit a terminal here anyway so the
            // "exactly one Terminal" invariant is enforced locally rather
            // than merely inherited from the inner layer.
            yield ToolStreamItem::Terminal(Err(ToolError::new(
                ToolErrorKind::TerminalError,
                "tool stream ended without a terminal",
            )));
        })
    }
}
impl WorkspaceHandle {
    /// Create a local-only [`ToolHarness`] backed by this workspace's
    /// session toolset.
    ///
    /// Tools are dispatched in-process via a [`LocalRegistry`] — no hub
    /// connection needed. Each tool is resolved dynamically from the
    /// session's live [`FinalizedToolset`] at call time, so tool config
    /// hot-reloads (via `update_tool_config()`) take effect automatically.
    pub fn create_local_harness(
        &self,
        session_id: &str,
    ) -> WorkspaceResult<echo_agent_computer_hub_sdk::ToolHarness> {
        let session = self
            .session(session_id)
            .ok_or_else(|| WorkspaceError::SessionNotFound(session_id.to_string()))?;
        let toolset = session.toolset();
        let registry = echo_agent_computer_hub_sdk::LocalRegistry::new();
        for def in toolset.tool_definitions() {
            let tool_name = def.function.name.clone();
            let desc = echo_agent_tool_types::ToolDescription::new(
                tool_name.clone(),
                def.function.description.clone().unwrap_or_default(),
            );
            match SessionToolHandle::new(tool_name, desc, self.clone(), session_id.to_string()) {
                Ok(tool) => {
                    registry
                        .register_dyn(Arc::new(tool) as Arc<dyn echo_agent_tool_runtime::ToolDyn>);
                }
                Err(e) => {
                    tracing::warn!(
                        tool = %def.function.name,
                        error = %e,
                        "client name is not a valid ToolId; skipping local-harness registration"
                    );
                }
            }
        }
        let session_id = echo_agent_tool_protocol::SessionId::new(session_id.to_string())
            .map_err(|e| WorkspaceError::HubError(format!("invalid session id: {e}")))?;
        Ok(echo_agent_computer_hub_sdk::ToolHarness::local_only_with(
            registry,
            session_id,
            echo_agent_tool_runtime::TypedExtensions::default(),
        ))
    }
}
impl WorkspaceHandle {
    /// Minimal handle for local mode (no hub). Requires Tokio runtime.
    ///
    /// `identity` is stored for parity with the standalone path; this local
    /// path has no upload queue, so no environment artifact is emitted.
    pub fn new_minimal(
        cwd: std::path::PathBuf,
        identity: crate::upload::environment::WorkspaceIdentity,
        project_lsp_trusted: bool,
    ) -> WorkspaceResult<Self> {
        use crate::session::tool_config::WorkspaceSessionContextFactory;
        let config = WorkspaceConfig {
            root_cwd: cwd,
            default_tool_config: echo_agent_tools::registry::types::ToolServerConfig {
                tools: vec![],
                behavior_preset: None,
            },
            respect_gitignore: false,
            memory_config: None,
            event_buffer_capacity: DEFAULT_EVENT_BUFFER_CAPACITY,
            session_factory: Arc::new(WorkspaceSessionContextFactory::new()),
            hook_global_sources: vec![],
            hook_project_sources: vec![],
            skills_config: Default::default(),
            plugin_discovery_config: Default::default(),
            hub_config: None,
            auth_provider: None,
            server_metadata: None,
            status_config: Default::default(),
            project_lsp_trusted,
            require_explicit_toolset: false,
            confine_fs_to_workspace_root: false,
        };
        Self::build(
            config,
            ephemeral_workspace_home(),
            None,
            true,
            false,
            events_enabled(),
            rewind_all_outcomes_from_env(),
            tool_defs_enabled(),
            identity,
        )
    }
}
#[cfg(any(test, feature = "test-support"))]
impl WorkspaceHandle {
    fn test_config(
        root_cwd: std::path::PathBuf,
        factory: std::sync::Arc<
            crate::session::tool_config::test_support::TestSessionContextFactory,
        >,
    ) -> crate::config::WorkspaceConfig {
        use crate::config::{DEFAULT_EVENT_BUFFER_CAPACITY, WorkspaceConfig};
        use crate::session::tool_config::test_support::baseline_config;
        WorkspaceConfig {
            root_cwd,
            default_tool_config: baseline_config(),
            respect_gitignore: false,
            memory_config: None,
            event_buffer_capacity: DEFAULT_EVENT_BUFFER_CAPACITY,
            session_factory: factory,
            hook_global_sources: vec![],
            hook_project_sources: vec![],
            skills_config: Default::default(),
            plugin_discovery_config: Default::default(),
            hub_config: None,
            auth_provider: None,
            server_metadata: None,
            status_config: Default::default(),
            project_lsp_trusted: true,
            require_explicit_toolset: false,
            confine_fs_to_workspace_root: false,
        }
    }
    /// Test handle backed by a temp dir. Zero sessions; `TempDir` kept alive via `Arc`.
    pub fn for_test() -> Self {
        use crate::session::tool_config::test_support::TestSessionContextFactory;
        let factory = std::sync::Arc::new(TestSessionContextFactory::new());
        let root_cwd = factory.temp.path().to_path_buf();
        Self::new(Self::test_config(root_cwd, factory))
            .expect("test workspace handle construction must succeed")
    }
    /// Like [`Self::for_test`] but rooted at `root` (must exist on disk).
    pub fn for_test_in(root: &std::path::Path) -> Self {
        use crate::session::tool_config::test_support::TestSessionContextFactory;
        let factory = std::sync::Arc::new(TestSessionContextFactory::new());
        Self::new(Self::test_config(root.to_path_buf(), factory))
            .expect("test workspace handle construction must succeed")
    }
}
#[cfg(test)]
#[path = "handle_tests.rs"]
pub(crate) mod tests;
