//! Memory tests for the session-load path: prove the resume peek borrows the
//! transcript instead of copying, and bound peak memory with full cleanup.
//! Resuming a large session once OOM-killed the process under a cgroup cap.
//!
//! Run:
//!   cargo test -p echo-agent-runtime --features dhat-heap,test-support --test test_session_load_memory \
//!       session_load_dhat_bounded_and_freed -- --ignored --nocapture

#![cfg(unix)]

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static DHAT_ALLOC: dhat::Alloc = dhat::Alloc;

use echo_agent_runtime::session::storage::{
    JsonlStorageAdapter, StorageAdapter, prepare_replay_lines,
};
use echo_agent_runtime::session::testkit::synth::{self, SessionSpec};
use echo_agent_test_support::env::env_parse;
use pretty_assertions::assert_eq;

#[cfg(feature = "dhat-heap")]
use echo_agent_runtime::session::info::Info;
#[cfg(feature = "dhat-heap")]
use std::path::Path;

use tempfile::TempDir;

const BYTES_PER_MB: f64 = 1024.0 * 1024.0;
#[cfg(feature = "dhat-heap")]
const BYTES_PER_MB_U64: u64 = 1024 * 1024;

fn file_len(path: &std::path::Path) -> u64 {
    // Fail loud: a silent 0 would collapse the ratio budget instead of
    // surfacing a missing or unreadable fixture.
    std::fs::metadata(path).expect("stat updates.jsonl").len()
}

fn memory_spec() -> SessionSpec {
    SessionSpec::from_env_prefixed("SESSION_LOAD", SessionSpec::default())
}

/// Non-ignored zero-copy guard: every replay line must borrow from the
/// transcript, so an owned-copy regression fails here in CI.
#[tokio::test]
async fn prepare_replay_lines_borrows_the_transcript() {
    let spec = SessionSpec {
        turns: 3,
        acu_per_turn: 2,
        catalog_commands: 2,
        catalog_desc_len: 8,
        agent_chunks_per_turn: 2,
        agent_chunk_len: 32,
        rewind_points: 0,
        files_per_rewind: 0,
        file_content_len: 0,
    };
    let root = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let (_info, dir) = synth::prepare_session(root.path(), cwd.path(), &spec).await;
    let transcript =
        std::fs::read_to_string(dir.join("updates.jsonl")).expect("read updates.jsonl");

    let prepared = prepare_replay_lines(&transcript, None);
    assert_eq!(
        prepared.lines.len(),
        synth::expected_replay_lines(&spec),
        "replay line count regressed"
    );

    let start = transcript.as_ptr() as usize;
    let end = start + transcript.len();
    for line in &prepared.lines {
        let line_start = line.as_ptr() as usize;
        assert!(
            line_start >= start && line_start + line.len() <= end,
            "replay line must borrow from the transcript (zero-copy), not own a copy"
        );
    }
}

/// Let ready tasks drain and timer-driven cleanup run before reading heap
/// stats, so a drop's frees show up in the next `curr_bytes`/`curr_blocks`.
#[cfg(feature = "dhat-heap")]
async fn quiesce() {
    const YIELDS: usize = 50;
    const SETTLE: std::time::Duration = std::time::Duration::from_millis(10);
    for _ in 0..YIELDS {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(SETTLE).await;
}

#[cfg(feature = "dhat-heap")]
async fn run_load_cycle(adapter: &JsonlStorageAdapter, info: &Info, updates_path: &Path) -> usize {
    let light = adapter
        .load_session_without_updates(info)
        .await
        .expect("load_session_without_updates");
    let transcript = std::fs::read_to_string(updates_path).expect("read updates.jsonl");
    let prepared = prepare_replay_lines(&transcript, None);
    let replayed_lines = prepared.lines.len();
    drop(prepared);
    drop(transcript);
    drop(light);
    quiesce().await;
    replayed_lines
}

/// Env-derived gates and cycle counts, separated from the measured results. The
/// caller clamps `cycles` to at least one so the per-cycle divisions are safe.
#[cfg(feature = "dhat-heap")]
struct DhatBudget {
    warmup: usize,
    cycles: usize,
    ratio: f64,
    abs_budget_mb: u64,
    max_bytes_per_cycle: i64,
    max_blocks_per_cycle: i64,
}

/// Heap readings captured across the measured window, named so nothing can
/// silently transpose the same-typed counts.
#[cfg(feature = "dhat-heap")]
struct DhatMeasured {
    replayed_lines: usize,
    expected_lines: usize,
    on_disk_bytes: u64,
    peak_over_baseline: u64,
    net_bytes: i64,
    net_blocks: i64,
}

/// The measured window paired with the budget it is judged against; every gate
/// threshold derives from `budget`, so nothing is stored twice.
#[cfg(feature = "dhat-heap")]
struct DhatOutcome<'a> {
    budget: &'a DhatBudget,
    measured: DhatMeasured,
}

#[cfg(feature = "dhat-heap")]
impl DhatOutcome<'_> {
    fn ratio_budget_bytes(&self) -> u64 {
        (self.budget.ratio * self.measured.on_disk_bytes as f64) as u64
    }

    fn abs_budget_bytes(&self) -> u64 {
        self.budget.abs_budget_mb * BYTES_PER_MB_U64
    }

    fn per_cycle_bytes(&self) -> i64 {
        self.measured.net_bytes / self.budget.cycles as i64
    }

    fn per_cycle_blocks(&self) -> i64 {
        self.measured.net_blocks / self.budget.cycles as i64
    }

    fn no_spike(&self) -> bool {
        self.measured.peak_over_baseline < self.ratio_budget_bytes()
            && self.measured.peak_over_baseline < self.abs_budget_bytes()
    }

    fn cleaned_up(&self) -> bool {
        self.per_cycle_bytes() < self.budget.max_bytes_per_cycle
            && self.per_cycle_blocks() < self.budget.max_blocks_per_cycle
    }

    fn pass(&self) -> bool {
        self.no_spike()
            && self.cleaned_up()
            && self.measured.replayed_lines == self.measured.expected_lines
    }
}

#[cfg(feature = "dhat-heap")]
#[test]
fn dhat_outcome_verdict_arithmetic() {
    let budget = DhatBudget {
        warmup: 0,
        cycles: 4,
        ratio: 2.0,
        abs_budget_mb: 1,
        max_bytes_per_cycle: 100,
        max_blocks_per_cycle: 10,
    };

    let ok = DhatOutcome {
        budget: &budget,
        measured: DhatMeasured {
            replayed_lines: 5,
            expected_lines: 5,
            on_disk_bytes: 1024,
            peak_over_baseline: 1000,
            net_bytes: 40,
            net_blocks: 4,
        },
    };
    assert_eq!(ok.per_cycle_bytes(), 10);
    assert_eq!(ok.per_cycle_blocks(), 1);
    assert!(ok.no_spike() && ok.cleaned_up() && ok.pass());

    // Peak over the ratio budget (2x the 1024-byte file) trips no_spike.
    let ratio_spike = DhatOutcome {
        budget: &budget,
        measured: DhatMeasured {
            replayed_lines: 5,
            expected_lines: 5,
            on_disk_bytes: 1024,
            peak_over_baseline: 4096,
            net_bytes: 0,
            net_blocks: 0,
        },
    };
    assert!(!ratio_spike.no_spike() && !ratio_spike.pass());

    // Per-cycle residual over the gate trips cleaned_up.
    let leak = DhatOutcome {
        budget: &budget,
        measured: DhatMeasured {
            replayed_lines: 5,
            expected_lines: 5,
            on_disk_bytes: 1024,
            peak_over_baseline: 1000,
            net_bytes: 4 * 200,
            net_blocks: 4 * 20,
        },
    };
    assert_eq!(leak.per_cycle_bytes(), 200);
    assert!(!leak.cleaned_up() && !leak.pass());

    // Clean gates but a mismatched replay count still fails pass.
    let miscount = DhatOutcome {
        budget: &budget,
        measured: DhatMeasured {
            replayed_lines: 4,
            expected_lines: 5,
            on_disk_bytes: 1024,
            peak_over_baseline: 1000,
            net_bytes: 40,
            net_blocks: 4,
        },
    };
    assert!(miscount.no_spike() && miscount.cleaned_up() && !miscount.pass());

    // A peak under the ratio budget but over the absolute budget trips no_spike
    // via its other arm.
    let abs_budget = DhatBudget {
        ratio: 1.0,
        ..budget
    };
    let abs_spike = DhatOutcome {
        budget: &abs_budget,
        measured: DhatMeasured {
            replayed_lines: 5,
            expected_lines: 5,
            on_disk_bytes: 2 * 1024 * 1024,
            peak_over_baseline: 1024 * 1024 + 1,
            net_bytes: 0,
            net_blocks: 0,
        },
    };
    assert!(!abs_spike.no_spike() && !abs_spike.pass());
}

#[cfg(feature = "dhat-heap")]
fn report_summary(o: &DhatOutcome<'_>) {
    eprintln!(
        "SESSION_LOAD_DHAT_SUMMARY {}",
        serde_json::json!({
            "mode": "dhat-heap",
            "cycles": o.budget.cycles,
            "warmup": o.budget.warmup,
            "replayed_lines": o.measured.replayed_lines,
            "expected_lines": o.measured.expected_lines,
            "on_disk_updates_bytes": o.measured.on_disk_bytes,
            "on_disk_updates_mb": o.measured.on_disk_bytes as f64 / BYTES_PER_MB,
            "peak_over_baseline_bytes": o.measured.peak_over_baseline,
            "peak_over_baseline_mb": o.measured.peak_over_baseline as f64 / BYTES_PER_MB,
            "peak_over_on_disk": o.measured.peak_over_baseline as f64 / o.measured.on_disk_bytes.max(1) as f64,
            "ratio_budget": o.budget.ratio,
            "ratio_budget_mb": o.ratio_budget_bytes() as f64 / BYTES_PER_MB,
            "abs_budget_mb": o.budget.abs_budget_mb,
            "no_spike": o.no_spike(),
            "per_cycle_residual_bytes": o.per_cycle_bytes(),
            "per_cycle_residual_blocks": o.per_cycle_blocks(),
            "max_bytes_per_cycle": o.budget.max_bytes_per_cycle,
            "max_blocks_per_cycle": o.budget.max_blocks_per_cycle,
            "net_window_bytes": o.measured.net_bytes,
            "net_window_blocks": o.measured.net_blocks,
            "cleaned_up": o.cleaned_up(),
            "pass": o.pass(),
        })
    );
}

#[cfg(feature = "dhat-heap")]
fn assert_bounds(o: &DhatOutcome<'_>) {
    assert_eq!(
        o.measured.replayed_lines, o.measured.expected_lines,
        "replayed line count must equal the non-ACU update count"
    );

    assert!(
        o.no_spike(),
        "load peak {:.1} MB over baseline is {:.2}x the {:.1} MB on-disk updates and exceeds a gate \
         (RATIO {}x = {:.1} MB, ABSOLUTE {} MB); load memory is super-linear in session size",
        o.measured.peak_over_baseline as f64 / BYTES_PER_MB,
        o.measured.peak_over_baseline as f64 / o.measured.on_disk_bytes.max(1) as f64,
        o.measured.on_disk_bytes as f64 / BYTES_PER_MB,
        o.budget.ratio,
        o.ratio_budget_bytes() as f64 / BYTES_PER_MB,
        o.budget.abs_budget_mb,
    );

    assert!(
        o.cleaned_up(),
        "leak: {} bytes/cycle and {} blocks/cycle retained over {} load/drop cycles \
         ({} net bytes, {} net blocks) exceed the {}-byte / {}-block gate; load does not free \
         everything",
        o.per_cycle_bytes(),
        o.per_cycle_blocks(),
        o.budget.cycles,
        o.measured.net_bytes,
        o.measured.net_blocks,
        o.budget.max_bytes_per_cycle,
        o.budget.max_blocks_per_cycle,
    );
}

#[cfg(feature = "dhat-heap")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "memory soak; run with --features dhat-heap --ignored --nocapture"]
async fn session_load_dhat_bounded_and_freed() {
    let opts = memory_spec();
    let root = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let (info, dir) = synth::prepare_session(root.path(), cwd.path(), &opts).await;
    let updates_path = dir.join("updates.jsonl");
    let on_disk_bytes = file_len(&updates_path);
    let expected_lines = synth::expected_replay_lines(&opts);

    let budget = DhatBudget {
        warmup: env_parse("SESSION_LOAD_WARMUP", 3usize),
        cycles: env_parse("SESSION_LOAD_CYCLES", 8usize).max(1),
        ratio: env_parse("SESSION_LOAD_HEAP_RATIO", 4.0),
        abs_budget_mb: env_parse("SESSION_LOAD_MAX_PEAK_HEAP_MB", 512u64),
        max_bytes_per_cycle: env_parse("SESSION_LOAD_MAX_BYTES_PER_CYCLE", 1i64 << 20),
        max_blocks_per_cycle: env_parse("SESSION_LOAD_MAX_BLOCKS_PER_CYCLE", 128i64),
    };

    let adapter = JsonlStorageAdapter::with_root(root.path().to_path_buf());

    // Warm up before starting the profiler so its lifetime `max_bytes` covers
    // only the measured window, not a warmup transient.
    for _ in 0..budget.warmup {
        let _ = run_load_cycle(&adapter, &info, &updates_path).await;
    }

    let profiler = dhat::Profiler::builder().testing().build();
    let window_before = dhat::HeapStats::get();
    let mut replayed_lines = 0usize;
    for _ in 0..budget.cycles {
        replayed_lines = run_load_cycle(&adapter, &info, &updates_path).await;
    }
    let window_after = dhat::HeapStats::get();
    drop(profiler);

    // `max_bytes` spans only the measured window, so the peak over its starting
    // baseline is a true load peak rather than a warmup artifact.
    let peak_over_baseline =
        (window_after.max_bytes as u64).saturating_sub(window_before.curr_bytes as u64);

    // Net change across the measured window; goes negative if a cycle frees more
    // than warmup left resident, which still satisfies the leak gate.
    let net_bytes = window_after.curr_bytes as i64 - window_before.curr_bytes as i64;
    let net_blocks = window_after.curr_blocks as i64 - window_before.curr_blocks as i64;

    let outcome = DhatOutcome {
        budget: &budget,
        measured: DhatMeasured {
            replayed_lines,
            expected_lines,
            on_disk_bytes,
            peak_over_baseline,
            net_bytes,
            net_blocks,
        },
    };

    report_summary(&outcome);
    assert_bounds(&outcome);
}

// dhat replaces the global allocator and perturbs RSS, so the RSS-based forms
// only compile without the `dhat-heap` feature.
#[cfg(not(feature = "dhat-heap"))]
mod rss {
    use super::*;
    use pretty_assertions::assert_eq;

    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;

    use agent_client_protocol::{self as acp};

    use echo_agent_test_support::resources::{RssOutcome, RssSampler};

    fn report_summary(mode: &str, counts: serde_json::Value, on_disk_bytes: u64, o: &RssOutcome) {
        let mut summary = serde_json::json!({
            "mode": mode,
            "on_disk_updates_bytes": on_disk_bytes,
            "on_disk_updates_mb": on_disk_bytes as f64 / BYTES_PER_MB,
            "baseline_rss_mb": o.baseline_bytes() as f64 / BYTES_PER_MB,
            "peak_rss_mb": o.peak_rss as f64 / BYTES_PER_MB,
            "peak_rss_growth_mb": o.peak_growth_bytes() as f64 / BYTES_PER_MB,
            "budget_mb": o.budget_mb,
            "rss_measurable": o.measurable(),
            "pass": o.pass(),
        });
        let obj = summary
            .as_object_mut()
            .expect("summary literal is a JSON object");
        let extra = counts.as_object().expect("counts must be a JSON object");
        for (k, v) in extra {
            obj.insert(k.clone(), v.clone());
        }
        eprintln!("SESSION_LOAD_MEMORY_SUMMARY {summary}");
    }

    fn assert_bounds(label: Option<&str>, on_disk_bytes: u64, o: &RssOutcome) {
        let prefix = label.map(|l| format!("{l} ")).unwrap_or_default();
        // This soak exists to enforce a bound, so unmeasurable RSS is a failure,
        // not a silent skip.
        assert!(
            o.measurable(),
            "{prefix}RSS sampling unavailable; the soak cannot enforce a bound"
        );
        assert!(
            o.within_budget(),
            "{prefix}peak RSS grew {:.1} MB over baseline while loading a {:.1} MB updates file \
             (bound {} MB)",
            o.peak_growth_bytes() as f64 / BYTES_PER_MB,
            on_disk_bytes as f64 / BYTES_PER_MB,
            o.budget_mb,
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "peak-memory soak; run with --ignored --nocapture"]
    async fn session_load_peak_rss_under_budget() {
        let opts = memory_spec();
        let root = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let (info, dir) = synth::prepare_session(root.path(), cwd.path(), &opts).await;
        let updates_path = dir.join("updates.jsonl");
        let on_disk_bytes = file_len(&updates_path);
        let expected_lines = synth::expected_replay_lines(&opts);

        let budget_mb = env_parse("SESSION_LOAD_MAX_PEAK_MB", 1024u64);
        let adapter = JsonlStorageAdapter::with_root(root.path().to_path_buf());

        let sampler = RssSampler::start();

        let light = adapter
            .load_session_without_updates(&info)
            .await
            .expect("load_session_without_updates");
        let transcript = std::fs::read_to_string(&updates_path).expect("read updates.jsonl");
        let prepared = prepare_replay_lines(&transcript, None);
        let replayed = prepared.lines.len();

        let outcome = sampler.finish().against_budget(budget_mb);
        drop(prepared);
        drop(transcript);
        drop(light);

        // Report before asserting so a count regression still emits the summary.
        report_summary(
            "rss",
            serde_json::json!({
                "replayed_lines": replayed,
                "expected_lines": expected_lines,
            }),
            on_disk_bytes,
            &outcome,
        );

        assert_eq!(
            replayed, expected_lines,
            "replayed line count must equal the non-ACU update count"
        );
        assert_bounds(None, on_disk_bytes, &outcome);
    }

    struct CountingClient {
        count: Rc<RefCell<u64>>,
    }

    #[async_trait::async_trait(?Send)]
    impl acp::Client for CountingClient {
        async fn request_permission(
            &self,
            args: acp::RequestPermissionRequest,
        ) -> acp::Result<acp::RequestPermissionResponse> {
            let outcome = args
                .options
                .first()
                .map(|o| {
                    acp::RequestPermissionOutcome::Selected(acp::SelectedPermissionOutcome::new(
                        o.option_id.clone(),
                    ))
                })
                .unwrap_or(acp::RequestPermissionOutcome::Cancelled);
            Ok(acp::RequestPermissionResponse::new(outcome))
        }

        async fn session_notification(&self, _args: acp::SessionNotification) -> acp::Result<()> {
            *self.count.borrow_mut() += 1;
            Ok(())
        }
    }

    async fn count_replayed_notifications(session_id: acp::SessionId, cwd: PathBuf) -> u64 {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async move {
                let count = Rc::new(RefCell::new(0u64));
                let client = CountingClient {
                    count: count.clone(),
                };
                let loaded = echo_agent_runtime::session::testkit::e2e::load_session_via_agent(
                    client, "mem-soak", session_id, cwd,
                )
                .await;
                drop(loaded);
                *count.borrow()
            })
            .await
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "heavy: builds a full MvpAgent and loads a large session; run with --ignored --nocapture"]
    async fn session_load_e2e_peak_rss() {
        echo_agent_extra_ca::ensure_default_crypto_provider();

        let server = echo_agent_test_support::MockInferenceServer::start()
            .await
            .unwrap();

        let echo_agent_home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let opts = memory_spec();
        let budget_mb = env_parse("SESSION_LOAD_MAX_PEAK_MB", 1024u64);

        // SAFETY: single-threaded current-thread runtime; set before any agent
        // code reads these process-globals (same pattern as session_load_perf).
        unsafe {
            std::env::set_var("ECHO_AGENT_HOME", echo_agent_home.path());
            std::env::set_var("ECHO_AGENT_CLI_CHAT_PROXY_BASE_URL", server.url());
            std::env::set_var("ECHO_AGENT_API_BASE_URL", server.url());
            std::env::set_var("ECHO_AGENT_API_KEY", "test-key-for-ci");
            std::env::set_var("ECHO_AGENT_TELEMETRY_ENABLED", "false");
            std::env::set_var("ECHO_AGENT_FEEDBACK_ENABLED", "false");
            std::env::set_var("ECHO_AGENT_TRACE_UPLOAD", "false");
        }

        let (info, dir) = synth::prepare_session(echo_agent_home.path(), cwd.path(), &opts).await;
        let on_disk_bytes = file_len(&dir.join("updates.jsonl"));

        let sampler = RssSampler::start();

        let replay_count =
            count_replayed_notifications(info.id.clone(), cwd.path().to_path_buf()).await;

        // The agent load already dropped the loaded state, so the peak here comes
        // from the background sampler; the final read is only a backstop.
        let outcome = sampler.finish().against_budget(budget_mb);
        report_summary(
            "rss-e2e",
            serde_json::json!({ "replayed_notifications": replay_count }),
            on_disk_bytes,
            &outcome,
        );

        // At least one notification per synthesized turn must replay; a near-empty
        // replay that still grew memory would otherwise pass silently.
        assert!(
            replay_count >= opts.turns as u64,
            "replayed {replay_count} notifications, expected at least {} (one per turn)",
            opts.turns,
        );
        assert_bounds(Some("e2e"), on_disk_bytes, &outcome);
    }
}
