//! Subagent lifecycle soak: churn spawn/run/completion/eviction and assert
//! threads, open files, and heap/RSS reach steady state. A stub `ChildRunner` drives
//! the real coordinator/transport.
//!
//!   SUBAGENT_SOAK_CYCLES=20000 cargo test -p echo-agent-tools \
//!     [--features dhat-heap] --test test_subagent_soak -- --ignored --nocapture

#![cfg(unix)]

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static DHAT_ALLOC: dhat::Alloc = dhat::Alloc;

use std::sync::Arc;
use std::time::Duration;

use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use strum::{EnumCount, IntoEnumIterator};
use tokio_util::sync::CancellationToken;

use echo_agent_test_support::env::env_parse;
use echo_agent_test_support::resources::{ResourceGrowth, ResourceSnapshot};
use echo_agent_tools::implementations::echo_agent_build::task::admission::SubagentLimits;
use echo_agent_tools::implementations::echo_agent_build::task::backend::{
    ChannelBackend, SubagentBackend,
};
use echo_agent_tools::implementations::echo_agent_build::task::coordinator::{
    ChildCompletion, ChildControl, ChildRunOutput, ChildRunRequest, ChildRunner, CoordinatorConfig,
    LocalBoxFuture, MAX_COMPLETED_ENTRIES, StartedChild, SubagentCoordinator, SubagentProgress,
};
use echo_agent_tools::implementations::echo_agent_build::task::types::{
    SubagentDescribeOutcome, SubagentOwner, SubagentRegistryCounts, SubagentRequest,
    SubagentResult, SubagentValidateTypeOutcome,
};

const PARENT_SESSION_ID: &str = "subagent-soak-parent";

#[derive(Clone, Copy, strum::EnumCount, strum::EnumIter)]
enum Metric {
    Rss,
    Threads,
    Fds,
}

impl Metric {
    fn label(self) -> &'static str {
        match self {
            Metric::Rss => "rss",
            Metric::Threads => "threads",
            Metric::Fds => "open_files",
        }
    }

    /// RSS reports raw bytes, so its key names the unit.
    fn summary_key(self) -> &'static str {
        match self {
            Metric::Rss => "rss_bytes",
            Metric::Threads => "threads",
            Metric::Fds => "open_files",
        }
    }

    fn unit(self) -> Option<&'static str> {
        match self {
            Metric::Rss => Some("MiB"),
            Metric::Threads | Metric::Fds => None,
        }
    }

    fn budget(self, bounds: &Bounds) -> f64 {
        match self {
            Metric::Rss => bounds.max_rss_growth_mib as f64,
            Metric::Threads => bounds.max_thread_growth as f64,
            Metric::Fds => bounds.max_open_files_growth as f64,
        }
    }

    /// RSS growth samples are bytes; convert to MiB for the budget comparison.
    fn growth_in_budget_unit(self, raw: usize) -> f64 {
        match self {
            Metric::Rss => bytes_to_mib(raw),
            Metric::Threads | Metric::Fds => raw as f64,
        }
    }

    /// Where a metric must be present and within budget. RSS everywhere;
    /// threads and open files only on Linux — macOS now samples threads too,
    /// but the budgets are tuned against Linux nightlies, so a macOS sample
    /// lands in the summary without being enforced.
    fn budgeted_on_this_platform(self) -> bool {
        match self {
            Metric::Rss => true,
            Metric::Threads | Metric::Fds => cfg!(target_os = "linux"),
        }
    }
}

/// Reads a metric's field from a snapshot or a growth delta so serialization and
/// the gates share one projection instead of repeating it.
trait MetricValue {
    fn value_of(&self, metric: Metric) -> Option<usize>;
}

impl MetricValue for ResourceSnapshot {
    fn value_of(&self, metric: Metric) -> Option<usize> {
        // Destructure so a new resource field is a compile error here, not a
        // silently dropped metric.
        let ResourceSnapshot {
            rss,
            threads,
            open_files,
        } = *self;
        match metric {
            Metric::Rss => rss,
            Metric::Threads => threads,
            Metric::Fds => open_files,
        }
    }
}

impl MetricValue for ResourceGrowth {
    fn value_of(&self, metric: Metric) -> Option<usize> {
        let ResourceGrowth {
            rss,
            threads,
            open_files,
        } = *self;
        match metric {
            Metric::Rss => rss,
            Metric::Threads => threads,
            Metric::Fds => open_files,
        }
    }
}

fn serialize_metrics<T: MetricValue, S: Serializer>(
    value: &T,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let mut map = serializer.serialize_map(Some(Metric::COUNT))?;
    for metric in Metric::iter() {
        map.serialize_entry(metric.summary_key(), &value.value_of(metric))?;
    }
    map.end()
}

fn bytes_to_mib(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

#[cfg_attr(not(feature = "dhat-heap"), allow(dead_code))]
#[derive(Clone, Copy, Serialize)]
struct HeapSample {
    blocks: i64,
    bytes: i64,
}

#[derive(Clone, Copy, Serialize)]
struct HeapMetrics {
    before: HeapSample,
    after: HeapSample,
    blocks_per_cycle: f64,
    bytes_per_cycle: f64,
}

impl HeapMetrics {
    fn new(before: HeapSample, after: HeapSample, cycles: u64) -> Self {
        // `SUBAGENT_SOAK_CYCLES=0` would otherwise divide by zero and feed
        // NaN/inf into the leak gates.
        let cycles = cycles.max(1) as f64;
        Self {
            before,
            after,
            blocks_per_cycle: (after.blocks - before.blocks) as f64 / cycles,
            bytes_per_cycle: (after.bytes - before.bytes) as f64 / cycles,
        }
    }
}

#[derive(Serialize)]
struct Bounds {
    #[serde(rename = "warmup_cycles")]
    warmup: u64,
    #[serde(rename = "measured_cycles")]
    measure: u64,
    concurrency: u64,
    max_thread_growth: u64,
    max_open_files_growth: u64,
    max_rss_growth_mib: u64,
    max_blocks_per_cycle: f64,
    max_bytes_per_cycle: f64,
}

impl Bounds {
    fn from_env() -> Self {
        Self {
            // Default warmup to the completed-entry cap so the ring is saturated
            // and the measured window observes steady-state eviction rather than
            // one-time cache fill.
            warmup: env_parse("SUBAGENT_SOAK_WARMUP", MAX_COMPLETED_ENTRIES as u64),
            measure: env_parse("SUBAGENT_SOAK_CYCLES", 512u64),
            concurrency: env_parse("SUBAGENT_SOAK_CONCURRENCY", 16u64),
            // RSS is looser than threads and open files to absorb allocator noise.
            max_thread_growth: env_parse("SUBAGENT_SOAK_MAX_THREAD_GROWTH", 8u64),
            max_open_files_growth: env_parse("SUBAGENT_SOAK_MAX_OPEN_FILES_GROWTH", 16u64),
            max_rss_growth_mib: env_parse("SUBAGENT_SOAK_MAX_RSS_GROWTH_MIB", 256u64),
            max_blocks_per_cycle: env_parse("SUBAGENT_SOAK_MAX_BLOCKS_PER_CYCLE", 2.0f64),
            max_bytes_per_cycle: env_parse("SUBAGENT_SOAK_MAX_BYTES_PER_CYCLE", 4096.0f64),
        }
    }
}

#[derive(Serialize)]
struct Measurement {
    #[serde(serialize_with = "serialize_metrics")]
    before: ResourceSnapshot,
    #[serde(serialize_with = "serialize_metrics")]
    after: ResourceSnapshot,
    #[serde(serialize_with = "serialize_metrics")]
    growth: ResourceGrowth,
    #[serde(serialize_with = "serialize_counts")]
    counts: SubagentRegistryCounts,
    heap: Option<HeapMetrics>,
    quiesced: bool,
}

fn serialize_counts<S: Serializer>(
    counts: &SubagentRegistryCounts,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    // Exhaustive destructure so a new count field is a compile error here, not a
    // silently dropped summary key.
    let SubagentRegistryCounts {
        pending,
        active,
        completed,
        queued,
    } = counts;
    let entries = [
        ("pending", pending),
        ("active", active),
        ("completed", completed),
        ("queued", queued),
    ];
    let mut map = serializer.serialize_map(Some(entries.len()))?;
    for (key, value) in entries {
        map.serialize_entry(key, value)?;
    }
    map.end()
}

#[derive(Serialize)]
struct Summary<'a> {
    #[serde(flatten)]
    bounds: &'a Bounds,
    #[serde(flatten)]
    measurement: &'a Measurement,
}

fn heap_capture() -> Option<HeapSample> {
    #[cfg(feature = "dhat-heap")]
    {
        let stats = dhat::HeapStats::get();
        Some(HeapSample {
            blocks: stats.curr_blocks as i64,
            bytes: stats.curr_bytes as i64,
        })
    }
    #[cfg(not(feature = "dhat-heap"))]
    {
        None
    }
}

async fn quiesce(backend: &ChannelBackend) -> bool {
    const MAX_POLLS: usize = 200;
    const SLEEP: Duration = Duration::from_millis(5);
    for _ in 0..MAX_POLLS {
        let counts = backend.registry_counts().await;
        if counts.pending == 0 && counts.active == 0 {
            return true;
        }
        tokio::time::sleep(SLEEP).await;
    }
    let counts = backend.registry_counts().await;
    eprintln!(
        "[soak] quiesce budget expired with pending={} active={}; snapshot may be noisy",
        counts.pending, counts.active
    );
    false
}

#[derive(Clone)]
struct SoakControl {
    cancellation: CancellationToken,
}

impl ChildControl for SoakControl {
    type ProgressFuture = std::future::Ready<SubagentProgress>;

    fn progress(&self) -> Self::ProgressFuture {
        std::future::ready(SubagentProgress::default())
    }

    fn cancel(&self) {
        self.cancellation.cancel();
    }
}

struct SoakRunner {
    gate: Arc<tokio::sync::Semaphore>,
}

impl ChildRunner for SoakRunner {
    type Control = SoakControl;
    type CompletionData = ();
    type RunFuture = LocalBoxFuture<ChildRunOutput<()>>;
    type ValidateFuture = LocalBoxFuture<SubagentValidateTypeOutcome>;
    type DescribeFuture = LocalBoxFuture<SubagentDescribeOutcome>;

    fn run(&self, run: ChildRunRequest<Self::Control>) -> Self::RunFuture {
        let gate = self.gate.clone();
        Box::pin(async move {
            let ChildRunRequest {
                request,
                cancellation,
                reporter,
                queued_for: _,
                session_running: _,
            } = run;
            let promoted = reporter
                .started(StartedChild {
                    child_session_id: request.id.clone(),
                    persona: None,
                    resumed_from: request.resume_from.clone(),
                    child_cwd: request.cwd.clone().unwrap_or_default(),
                    worktree_path: None,
                    effective_model_id: "soak-model".to_owned(),
                    definition_background: false,
                    control: SoakControl {
                        cancellation: cancellation.clone(),
                    },
                })
                .await;
            if !promoted || cancellation.is_cancelled() {
                return ChildRunOutput {
                    result: SubagentResult {
                        success: false,
                        cancelled: true,
                        error: Some("cancelled before start".to_owned()),
                        subagent_id: request.id.clone(),
                        child_session_id: request.id,
                        ..Default::default()
                    },
                    completion_data: (),
                    snapshot_ref: None,
                };
            }
            if request.id.starts_with("conc-") {
                // Hold in `active` until the concurrent phase releases the gate.
                let _ = gate.acquire().await;
            }
            ChildRunOutput {
                result: SubagentResult {
                    success: true,
                    output: Arc::from("soak child output"),
                    subagent_id: request.id.clone(),
                    child_session_id: request.id,
                    tool_calls: 1,
                    turns: 1,
                    ..Default::default()
                },
                completion_data: (),
                snapshot_ref: None,
            }
        })
    }

    fn validate_type(&self, _subagent_type: String, _parent: String) -> Self::ValidateFuture {
        Box::pin(std::future::ready(SubagentValidateTypeOutcome::Ok))
    }

    fn describe_type(
        &self,
        _subagent_type: String,
        _harness_agent_type: Option<String>,
        _parent: String,
    ) -> Self::DescribeFuture {
        Box::pin(std::future::ready(SubagentDescribeOutcome::Unavailable))
    }

    fn on_completed(&self, _completion: ChildCompletion<Self::CompletionData>) {}
}

fn soak_request(id: String, background: bool) -> SubagentRequest {
    SubagentRequest {
        id,
        prompt: "soak work".to_owned(),
        description: "soak child".to_owned(),
        subagent_type: "explore".to_owned(),
        parent_session_id: PARENT_SESSION_ID.to_owned(),
        parent_prompt_id: Some("soak-prompt".to_owned()),
        resume_from: None,
        cwd: None,
        runtime_overrides: Default::default(),
        run_in_background: background,
        surface_completion: true,
        await_to_completion: false,
        fork_context: false,
        owner: SubagentOwner::Task,
        cancel_token: CancellationToken::new(),
    }
}

async fn run_cycle(backend: &ChannelBackend, i: u64) {
    let fg = backend
        .spawn(soak_request(format!("fg-{i}"), false))
        .await
        .expect("foreground spawn round-trips through the coordinator");
    assert!(fg.success, "cycle {i}: foreground child must complete");

    let bg_id = format!("bg-{i}");
    let bg = backend
        .spawn(soak_request(bg_id.clone(), true))
        .await
        .expect("background spawn round-trips through the coordinator");
    assert!(bg.success, "cycle {i}: background child must complete");

    let blocking = true;
    let timeout_ms = Some(5_000);
    let snapshot = backend.query(&bg_id, blocking, timeout_ms).await;
    assert!(
        snapshot.is_some(),
        "cycle {i}: completed subagent must be queryable"
    );
}

async fn await_concurrency(backend: &ChannelBackend, n: u64) -> bool {
    const MAX_POLLS: usize = 400;
    const SLEEP: Duration = Duration::from_millis(5);
    for _ in 0..MAX_POLLS {
        if backend.registry_counts().await.active as u64 >= n {
            return true;
        }
        tokio::time::sleep(SLEEP).await;
    }
    false
}

async fn concurrent_phase(backend: &ChannelBackend, gate: &tokio::sync::Semaphore, n: u64) {
    let handles: Vec<_> = (0..n)
        .map(|k| {
            let backend = backend.clone();
            tokio::task::spawn_local(async move {
                backend.spawn(soak_request(format!("conc-{k}"), true)).await
            })
        })
        .collect();

    let reached = await_concurrency(backend, n).await;

    // Release then join before asserting, so no child is left blocked on failure.
    gate.add_permits(n as usize);
    for h in handles {
        let result = h.await.expect("concurrent spawn task");
        assert!(
            result.expect("concurrent spawn round-trips").success,
            "concurrent child must complete"
        );
    }
    assert!(reached, "expected {n} concurrently active children");
}

async fn warmup(backend: &ChannelBackend, cycles: u64) -> bool {
    for i in 0..cycles {
        run_cycle(backend, i).await;
    }
    quiesce(backend).await
}

async fn measure(
    backend: &ChannelBackend,
    bounds: &Bounds,
    baseline_quiesced: bool,
) -> Measurement {
    let heap_before = heap_capture();
    let before = ResourceSnapshot::capture();

    // Continue ids past the warmup window so measured cycles use fresh entries
    // and keep exercising eviction instead of colliding with warmup ids.
    for i in bounds.warmup..(bounds.warmup + bounds.measure) {
        run_cycle(backend, i).await;
    }
    // A baseline that never drained already poisons `before`, so skip the
    // measured-window drain and report the window as not quiesced.
    let quiesced = baseline_quiesced && quiesce(backend).await;

    let heap_after = heap_capture();
    let after = ResourceSnapshot::capture();
    let counts = backend.registry_counts().await;

    Measurement {
        before,
        after,
        growth: after.growth_from(&before),
        counts,
        heap: heap_before
            .zip(heap_after)
            .map(|(before, after)| HeapMetrics::new(before, after, bounds.measure)),
        quiesced,
    }
}

/// Takes `budgeted` as a parameter so both arms are testable on any platform.
/// An unbudgeted metric never fails: missing is fine, and a present value
/// (macOS thread counts) is informational, not measured against a bound
/// tuned for another platform.
fn metric_failure(
    metric: Metric,
    value: Option<usize>,
    budgeted: bool,
    bounds: &Bounds,
) -> Option<String> {
    if !budgeted {
        return None;
    }
    let Some(raw) = value else {
        return Some(format!(
            "{}: growth sample unavailable; the soak cannot bound it",
            metric.label()
        ));
    };
    let growth = metric.growth_in_budget_unit(raw);
    let budget = metric.budget(bounds);
    (growth > budget).then(|| {
        let unit = metric.unit().map(|u| format!(" {u}")).unwrap_or_default();
        format!(
            "{}: grew {growth:.1}{unit} over the soak (bound {budget:.1}{unit})",
            metric.label()
        )
    })
}

fn check_bounds(bounds: &Bounds, m: &Measurement) -> Vec<String> {
    // Drain first: a non-quiesced window has nonzero counts and noisy growth, so
    // report the quiesce failure alone; the gates below only mean anything once
    // drained.
    if !m.quiesced {
        return vec![
            "quiesce budget expired before the measured window drained; soak result is unreliable"
                .to_owned(),
        ];
    }

    let mut failures = Vec::new();
    if m.counts.pending != 0 {
        failures.push(format!(
            "no subagent may remain pending, saw {}",
            m.counts.pending
        ));
    }
    if m.counts.active != 0 {
        failures.push(format!(
            "no subagent may remain active, saw {}",
            m.counts.active
        ));
    }
    if m.counts.completed > MAX_COMPLETED_ENTRIES {
        failures.push(format!(
            "completed retention must stay bounded by its cap, saw {}",
            m.counts.completed
        ));
    }

    for metric in Metric::iter() {
        let budgeted = metric.budgeted_on_this_platform();
        if let Some(f) = metric_failure(metric, m.growth.value_of(metric), budgeted, bounds) {
            failures.push(f);
        }
    }

    if let Some(h) = m.heap {
        let measure = bounds.measure;
        if h.blocks_per_cycle > bounds.max_blocks_per_cycle {
            failures.push(format!(
                "block-count leak: {:.3} blocks/cycle retained ({} over {measure} cycles) \
                 exceeds the {} gate",
                h.blocks_per_cycle,
                h.after.blocks - h.before.blocks,
                bounds.max_blocks_per_cycle
            ));
        }
        if h.bytes_per_cycle > bounds.max_bytes_per_cycle {
            failures.push(format!(
                "byte leak: {:.1} bytes/cycle retained ({} over {measure} cycles) \
                 exceeds the {} gate",
                h.bytes_per_cycle,
                h.after.bytes - h.before.bytes,
                bounds.max_bytes_per_cycle
            ));
        }
    }

    failures
}

fn assert_bounds(bounds: &Bounds, m: &Measurement) {
    let failures = check_bounds(bounds, m);
    assert!(
        failures.is_empty(),
        "subagent soak bounds violated:\n  - {}",
        failures.join("\n  - ")
    );
}

/// Keep this the only test in the binary that creates a `dhat::Profiler`.
#[tokio::test(flavor = "current_thread")]
#[ignore = "subagent soak; run with --ignored (SUBAGENT_SOAK_CYCLES bounds the measured window)"]
async fn subagent_lifecycle_soak_bounds_threads_open_files_and_heap() {
    #[cfg(feature = "dhat-heap")]
    let _profiler = dhat::Profiler::builder().testing().build();

    let bounds = Bounds::from_env();

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let (command_tx, command_rx) = tokio::sync::mpsc::unbounded_channel();
            // The soak measures registry churn, not admission: keep every
            // spawn unthrottled so cycle counts stay resource-bound.
            let config = CoordinatorConfig {
                foreground_budget: Duration::from_secs(600),
                limits: SubagentLimits {
                    max_concurrent: usize::MAX,
                    ..SubagentLimits::default()
                },
                ..CoordinatorConfig::default()
            };
            let gate = Arc::new(tokio::sync::Semaphore::new(0));
            tokio::task::spawn_local(
                SubagentCoordinator::new(command_rx, SoakRunner { gate: gate.clone() }, config)
                    .run(),
            );
            let backend = ChannelBackend::new(command_tx);

            let warmup_quiesced = warmup(&backend, bounds.warmup).await;
            // Drain the concurrent phase into the baseline; a failed drain marks
            // the window unreliable.
            concurrent_phase(&backend, &gate, bounds.concurrency).await;
            let baseline_quiesced = warmup_quiesced && quiesce(&backend).await;
            let measurement = measure(&backend, &bounds, baseline_quiesced).await;

            let summary = Summary {
                bounds: &bounds,
                measurement: &measurement,
            };
            eprintln!(
                "SUBAGENT_SOAK_SUMMARY {}",
                serde_json::to_string(&summary).expect("summary serializes")
            );

            assert_bounds(&bounds, &measurement);
        })
        .await;
}

mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn value_of_reads_the_matching_slot_of_snapshot_and_growth() {
        let snapshot = ResourceSnapshot {
            rss: Some(11),
            threads: Some(22),
            open_files: Some(33),
        };
        assert_eq!(snapshot.value_of(Metric::Rss), Some(11));
        assert_eq!(snapshot.value_of(Metric::Threads), Some(22));
        assert_eq!(snapshot.value_of(Metric::Fds), Some(33));

        let growth = ResourceGrowth {
            rss: Some(1),
            threads: None,
            open_files: Some(3),
        };
        assert_eq!(growth.value_of(Metric::Rss), Some(1));
        assert_eq!(growth.value_of(Metric::Threads), None);
        assert_eq!(growth.value_of(Metric::Fds), Some(3));
    }

    #[test]
    fn serialize_metrics_keys_match_summary_keys_in_order() {
        #[derive(Serialize)]
        struct Wrap(#[serde(serialize_with = "serialize_metrics")] ResourceSnapshot);
        let snapshot = ResourceSnapshot {
            rss: Some(1),
            threads: None,
            open_files: Some(3),
        };
        let json = serde_json::to_string(&Wrap(snapshot)).expect("snapshot serializes");
        assert_eq!(json, r#"{"rss_bytes":1,"threads":null,"open_files":3}"#);
    }

    #[test]
    fn bytes_to_mib_divides_by_1024_squared() {
        assert_eq!(bytes_to_mib(0), 0.0);
        assert_eq!(bytes_to_mib(1024 * 1024), 1.0);
        assert_eq!(bytes_to_mib(3 * 1024 * 1024), 3.0);
    }

    #[test]
    fn growth_in_budget_unit_scales_only_rss() {
        assert_eq!(Metric::Rss.growth_in_budget_unit(2 * 1024 * 1024), 2.0);
        assert_eq!(Metric::Threads.growth_in_budget_unit(7), 7.0);
        assert_eq!(Metric::Fds.growth_in_budget_unit(7), 7.0);
    }

    #[test]
    fn budget_reads_per_metric_bound() {
        let bounds = Bounds {
            warmup: 0,
            measure: 0,
            concurrency: 0,
            max_thread_growth: 3,
            max_open_files_growth: 5,
            max_rss_growth_mib: 7,
            max_blocks_per_cycle: 1.0,
            max_bytes_per_cycle: 2.0,
        };
        assert_eq!(Metric::Rss.budget(&bounds), 7.0);
        assert_eq!(Metric::Threads.budget(&bounds), 3.0);
        assert_eq!(Metric::Fds.budget(&bounds), 5.0);
    }

    #[test]
    fn heap_metrics_clamps_zero_cycles() {
        let before = HeapSample {
            blocks: 10,
            bytes: 100,
        };
        let after = HeapSample {
            blocks: 20,
            bytes: 400,
        };
        let heap = HeapMetrics::new(before, after, 0);
        assert!(heap.blocks_per_cycle.is_finite());
        assert!(heap.bytes_per_cycle.is_finite());
        assert_eq!(heap.blocks_per_cycle, 10.0);
        assert_eq!(heap.bytes_per_cycle, 300.0);
    }

    fn generous_bounds() -> Bounds {
        Bounds {
            warmup: 0,
            measure: 4,
            concurrency: 4,
            max_thread_growth: 100,
            max_open_files_growth: 100,
            max_rss_growth_mib: 100,
            max_blocks_per_cycle: 10.0,
            max_bytes_per_cycle: 10_000.0,
        }
    }

    /// Zero growth that reads as measured, unlike `ResourceGrowth::default()`.
    fn zero_growth() -> ResourceGrowth {
        ResourceGrowth {
            rss: Some(0),
            threads: Some(0),
            open_files: Some(0),
        }
    }

    fn drained(growth: ResourceGrowth, heap: Option<HeapMetrics>) -> Measurement {
        Measurement {
            before: ResourceSnapshot::default(),
            after: ResourceSnapshot::default(),
            growth,
            counts: SubagentRegistryCounts {
                pending: 0,
                active: 0,
                completed: 0,
                queued: 0,
            },
            heap,
            quiesced: true,
        }
    }

    #[test]
    fn check_bounds_passes_a_clean_drained_window() {
        let m = drained(zero_growth(), None);
        assert!(check_bounds(&generous_bounds(), &m).is_empty());
    }

    #[test]
    fn check_bounds_fails_when_an_expected_metric_is_unavailable() {
        let growth = ResourceGrowth {
            rss: None,
            threads: Some(0),
            open_files: Some(0),
        };
        let failures = check_bounds(&generous_bounds(), &drained(growth, None));
        assert!(
            failures
                .iter()
                .any(|f| f.starts_with("rss:") && f.contains("unavailable")),
            "{failures:?}"
        );
    }

    #[test]
    fn metric_failure_covers_unbudgeted_missing_and_budget_arms() {
        let b = generous_bounds();
        assert!(metric_failure(Metric::Threads, None, false, &b).is_none());
        // An unbudgeted metric with a present, over-budget value stays
        // informational (macOS thread counts against Linux-tuned bounds).
        assert!(metric_failure(Metric::Threads, Some(usize::MAX), false, &b).is_none());
        assert!(
            metric_failure(Metric::Rss, None, true, &b)
                .unwrap()
                .contains("unavailable")
        );
        assert!(metric_failure(Metric::Fds, Some(0), true, &b).is_none());
        assert!(
            metric_failure(Metric::Rss, Some(500 * 1024 * 1024), true, &b)
                .unwrap()
                .starts_with("rss:")
        );
    }

    #[test]
    fn check_bounds_reports_non_quiesce_first_and_alone() {
        let mut m = drained(zero_growth(), None);
        m.quiesced = false;
        m.counts.pending = 3;
        let failures = check_bounds(&generous_bounds(), &m);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("quiesce"));
    }

    #[test]
    fn check_bounds_flags_over_budget_growth() {
        let growth = ResourceGrowth {
            rss: Some(200 * 1024 * 1024),
            threads: Some(0),
            open_files: Some(0),
        };
        let failures = check_bounds(&generous_bounds(), &drained(growth, None));
        assert!(
            failures.iter().any(|f| f.starts_with("rss:")),
            "{failures:?}"
        );
    }

    #[test]
    fn check_bounds_treats_the_budget_as_an_inclusive_max() {
        let growth = ResourceGrowth {
            rss: Some(100 * 1024 * 1024),
            threads: Some(100),
            open_files: Some(100),
        };
        assert!(check_bounds(&generous_bounds(), &drained(growth, None)).is_empty());
    }

    #[test]
    fn check_bounds_flags_nonzero_counts_and_heap_leak() {
        let mut m = drained(
            zero_growth(),
            Some(HeapMetrics {
                before: HeapSample {
                    blocks: 0,
                    bytes: 0,
                },
                after: HeapSample {
                    blocks: 0,
                    bytes: 0,
                },
                blocks_per_cycle: 0.0,
                bytes_per_cycle: 1_000_000.0,
            }),
        );
        m.counts.active = 2;
        let failures = check_bounds(&generous_bounds(), &m);
        assert!(
            failures.iter().any(|f| f.contains("active")),
            "{failures:?}"
        );
        assert!(
            failures.iter().any(|f| f.contains("byte leak")),
            "{failures:?}"
        );
    }

    #[test]
    fn check_bounds_flags_pending_while_quiesced() {
        let mut m = drained(zero_growth(), None);
        m.counts.pending = 3;
        let failures = check_bounds(&generous_bounds(), &m);
        assert!(
            failures.iter().any(|f| f.contains("pending")),
            "{failures:?}"
        );
    }

    #[test]
    fn check_bounds_flags_completed_over_cap() {
        let mut m = drained(zero_growth(), None);
        m.counts.completed = MAX_COMPLETED_ENTRIES + 1;
        let failures = check_bounds(&generous_bounds(), &m);
        assert!(
            failures.iter().any(|f| f.contains("completed retention")),
            "{failures:?}"
        );
    }

    /// Thread and open-file budgets only bite on Linux; elsewhere the same
    /// over-budget growth is informational and must not fail the soak.
    #[test]
    fn check_bounds_flags_thread_and_open_files_over_budget_on_linux_only() {
        let growth = ResourceGrowth {
            rss: Some(0),
            threads: Some(200),
            open_files: Some(200),
        };
        let failures = check_bounds(&generous_bounds(), &drained(growth, None));
        if cfg!(target_os = "linux") {
            assert!(
                failures.iter().any(|f| f.starts_with("threads:")),
                "{failures:?}"
            );
            assert!(
                failures.iter().any(|f| f.starts_with("open_files:")),
                "{failures:?}"
            );
        } else {
            assert!(failures.is_empty(), "{failures:?}");
        }
    }

    #[test]
    fn check_bounds_flags_block_count_leak() {
        let m = drained(
            zero_growth(),
            Some(HeapMetrics {
                before: HeapSample {
                    blocks: 0,
                    bytes: 0,
                },
                after: HeapSample {
                    blocks: 0,
                    bytes: 0,
                },
                blocks_per_cycle: 50.0,
                bytes_per_cycle: 0.0,
            }),
        );
        let failures = check_bounds(&generous_bounds(), &m);
        assert!(
            failures.iter().any(|f| f.contains("block-count leak")),
            "{failures:?}"
        );
    }
}
