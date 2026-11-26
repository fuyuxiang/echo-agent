//! End-to-end measurement of why resuming a large session is slow: the time
//! spent before the client can render anything.
//!
//! The pager resumes via `session/load` and blocks on the response. The shell
//! answers by (1) `load_light` (chat history; rewind points now load lazily) and
//! (2) `replay_session_updates`, which reads `updates.jsonl`, filters it, typed
//! parses every line, and forwards each as a `session/update`. All of that
//! happens while the client waits; both tests drive the real production code.
//!
//! * [`phase_breakdown_real_functions`] drives the exact load-path functions
//!   (`load_session_without_updates`, `load_updates_for_replay_at`) and attributes
//!   wall-clock to rewind load, chat+summary load, and updates read+parse+filter,
//!   then prints a per-`sessionUpdate`-kind byte breakdown of `updates.jsonl`.
//! * [`full_session_load_e2e`] stands up a real `MvpAgent` over in-process ACP
//!   pipes (via [`load_session_via_agent`]); times `session/load` end-to-end,
//!   counts replayed notifications, and dumps the shell's own per-phase
//!   `instrumentation_timer!` events.
//!
//! Session data (both tests): a synthetic session from the shared
//! [`synth`](echo_agent_runtime::session::testkit::synth) generator (redundant
//! `available_commands_update` + big rewind snapshots; size knobs via
//! `ECHO_AGENT_PERF_*`), or a real session dir via `ECHO_AGENT_PERF_SESSION_SRC=<session-dir>`.
//!
//! Run (needs the `test-support` feature; on by default under Bazel):
//!   cargo test -p echo-agent-runtime --features test-support --test session_load_perf -- --nocapture
//!   cargo test -p echo-agent-runtime --features test-support --test session_load_perf full_session_load_e2e -- --ignored --nocapture

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use agent_client_protocol::{self as acp};

#[allow(dead_code)]
#[path = "acp_harness/mod.rs"]
mod acp_harness;
#[path = "perf_harness/mod.rs"]
mod perf_harness;
use perf_harness::{PerfRecorder, SharedRecorded};
use tempfile::TempDir;

use echo_agent_runtime::session::info::Info;
use echo_agent_runtime::session::storage::{
    JsonlStorageAdapter, StorageAdapter, load_updates_for_replay_at,
};
use echo_agent_runtime::session::testkit::e2e::load_session_via_agent;
use echo_agent_runtime::session::testkit::synth::{self, SessionSpec};

// ───────────────────────── session spec ─────────────────────────

/// Perf-tool defaults over the shared [`SessionSpec`], tuned to the pathological
/// real session; scale/override via `ECHO_AGENT_PERF_*` (e.g. `ECHO_AGENT_PERF_TURNS`,
/// `ECHO_AGENT_PERF_SCALE`), or point `ECHO_AGENT_PERF_SESSION_SRC` at a real session dir.
fn perf_spec() -> SessionSpec {
    SessionSpec::from_env_prefixed(
        "ECHO_AGENT_PERF",
        SessionSpec {
            turns: 80,
            rewind_points: 60,
            files_per_rewind: 40,
            file_content_len: 8000,
            ..SessionSpec::default()
        },
    )
}

// ───────────────────────── session setup ─────────────────────────

/// Recursively copy a directory tree (real-session overlay only).
fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap().flatten() {
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            std::fs::copy(&from, &to).unwrap();
        }
    }
}

/// Prepare a session on disk under `root` for working dir `cwd`. With
/// `ECHO_AGENT_PERF_SESSION_SRC` set, copy a real session over a registered stub
/// (keeping our `summary.json`); otherwise synthesize one via
/// [`synth::prepare_session`].
async fn prepare_session(root: &Path, cwd: &Path, spec: &SessionSpec) -> (Info, PathBuf) {
    let Ok(src) = std::env::var("ECHO_AGENT_PERF_SESSION_SRC") else {
        return synth::prepare_session(root, cwd, spec).await;
    };

    let adapter = JsonlStorageAdapter::with_root(root.to_path_buf());
    let id = uuid::Uuid::new_v4().to_string();
    let info = Info {
        id: synth::sid(&id),
        cwd: cwd.to_string_lossy().to_string(),
    };
    adapter
        .init_session(&info, acp::ModelId::new("test-model"))
        .await
        .expect("init_session");
    let dir = synth::locate_session_dir(root, &id);
    for name in ["updates.jsonl", "rewind_points.jsonl", "chat_history.jsonl"] {
        let from = Path::new(&src).join(name);
        if from.exists() {
            std::fs::copy(&from, dir.join(name)).unwrap();
        }
    }
    let ckpt = Path::new(&src).join("compaction_checkpoints");
    if ckpt.is_dir() {
        copy_tree(&ckpt, &dir.join("compaction_checkpoints"));
    }
    eprintln!("[perf] using REAL session copied from {src}");
    (info, dir)
}

/// Re-create the rewind file after the isolation step deletes it (synthetic
/// case). For a real session copy we cannot regenerate; leave it absent.
fn generate_or_restore_rewind(path: &Path, spec: &SessionSpec) {
    if std::env::var("ECHO_AGENT_PERF_SESSION_SRC").is_ok() {
        return;
    }
    synth::write_rewind_jsonl(path, spec);
}

// ───────────────────────── updates.jsonl stats ─────────────────────────

/// Per-kind statistics for the generated/loaded updates file.
#[derive(Default)]
struct KindStats {
    count: BTreeMap<String, u64>,
    bytes: BTreeMap<String, u64>,
}

fn file_size_mb(path: &Path) -> f64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) as f64 / 1e6
}

/// `(len, content_hash)` fingerprint of a file, for asserting it is byte-for-byte
/// unchanged across an operation (zero-data-loss guard). Missing file → `(0, 0)`.
fn file_fingerprint(path: &Path) -> (u64, u64) {
    use std::hash::{Hash, Hasher};
    let Ok(bytes) = std::fs::read(path) else {
        return (0, 0);
    };
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    (bytes.len() as u64, hasher.finish())
}

/// Per-`sessionUpdate`-kind byte + count breakdown of an `updates.jsonl`.
fn updates_kind_breakdown(path: &Path) -> KindStats {
    let mut stats = KindStats::default();
    let Ok(contents) = std::fs::read_to_string(path) else {
        return stats;
    };
    for line in contents.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let len = line.len() as u64 + 1;
        let kind = serde_json::from_str::<serde_json::Value>(line)
            .ok()
            .and_then(|v| {
                v.get("params")
                    .and_then(|p| p.get("update"))
                    .and_then(|u| u.get("sessionUpdate"))
                    .and_then(|s| s.as_str())
                    .map(String::from)
            })
            .unwrap_or_else(|| "<unparsed>".to_string());
        *stats.count.entry(kind.clone()).or_default() += 1;
        *stats.bytes.entry(kind).or_default() += len;
    }
    stats
}

fn print_kind_breakdown(label: &str, stats: &KindStats) {
    let total: u64 = stats.bytes.values().sum();
    eprintln!(
        "\n[perf] {label}: updates.jsonl composition ({:.1} MB total):",
        total as f64 / 1e6
    );
    eprintln!(
        "  {:<32} {:>8} {:>10} {:>7}",
        "sessionUpdate kind", "count", "MB", "%"
    );
    let mut rows: Vec<(&String, &u64)> = stats.bytes.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1));
    for (kind, bytes) in rows {
        let count = stats.count.get(kind).copied().unwrap_or(0);
        let pct = if total > 0 {
            *bytes as f64 / total as f64 * 100.0
        } else {
            0.0
        };
        eprintln!(
            "  {:<32} {:>8} {:>10.1} {:>6.1}%",
            kind,
            count,
            *bytes as f64 / 1e6,
            pct
        );
    }
}

// ───────────────────────── TEST 1: phase breakdown ─────────────────────────

/// Attribute the pre-render load cost to its real phases using the exact
/// production functions, isolating rewind-point load from everything else.
///
/// `#[ignore]`: this is a measurement tool (generates tens of MB, ~3 s), and its
/// only correctness assertion is covered by the unit tests. Run explicitly with
/// `--ignored` (optionally `ECHO_AGENT_PERF_SESSION_SRC=...`) to get the numbers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "perf measurement tool; run with --ignored"]
async fn phase_breakdown_real_functions() {
    let root = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let spec = perf_spec();

    let (info, dir) = prepare_session(root.path(), cwd.path(), &spec).await;

    let updates_path = dir.join("updates.jsonl");
    let rewind_path = dir.join("rewind_points.jsonl");
    eprintln!(
        "\n[perf] session dir: {}\n[perf]   updates.jsonl       = {:.1} MB\n[perf]   rewind_points.jsonl = {:.1} MB",
        dir.display(),
        file_size_mb(&updates_path),
        file_size_mb(&rewind_path),
    );

    let adapter = JsonlStorageAdapter::with_root(root.path().to_path_buf());

    // Phase A: load_light core (summary + chat_history), what mvp_agent's
    // `load_light` blocks on before replay.
    let t = Instant::now();
    let light = adapter
        .load_session_without_updates(&info)
        .await
        .expect("load_session_without_updates");
    let full_load_light = t.elapsed();
    drop(light);

    // Lazy rewind path: the deferred cost moved here. The picker only needs
    // a cheap metadata scan; an actual rewind triggers the full content load.
    // Both read the same file that `load_light` no longer touches.
    use echo_agent_workspace::session::file_state::FileStateTracker;
    let t = Instant::now();
    let lazy_metas = FileStateTracker::with_lazy_source(rewind_path.clone())
        .get_rewind_point_metas()
        .await;
    let lazy_metas_scan = t.elapsed();
    let t = Instant::now();
    let lazy_points = FileStateTracker::with_lazy_source(rewind_path.clone())
        .get_rewind_points()
        .await;
    let lazy_full_load = t.elapsed();
    let num_rewind = lazy_points.len();
    assert_eq!(
        lazy_metas.len(),
        num_rewind,
        "picker metadata scan must see every rewind point"
    );

    // Phase A': isolate rewind cost by deleting the rewind file and re-measuring.
    // The delta is the rewind-point deserialization (full file-content snapshots).
    std::fs::remove_file(&rewind_path).ok();
    let t = Instant::now();
    let _light2 = adapter
        .load_session_without_updates(&info)
        .await
        .expect("load_session_without_updates (no rewind)");
    let load_light_no_rewind = t.elapsed();
    // restore for downstream/manual reruns
    generate_or_restore_rewind(&rewind_path, &spec);

    let rewind_cost = full_load_light.saturating_sub(load_light_no_rewind);

    // Phase B: updates replay parse. The typed `load_updates_for_replay_at`
    // reads the whole file, typed-parses every line, and applies rewind
    // filtering; production now streams via `stream_replay_updates_at`, so this
    // measures the materialize-all parse cost.
    let t = Instant::now();
    let replayed = load_updates_for_replay_at(info.id.0.as_ref(), root.path())
        .expect("load_updates_for_replay_at")
        .unwrap_or_default();
    let updates_parse = t.elapsed();

    let stats = updates_kind_breakdown(&updates_path);
    print_kind_breakdown("phase_breakdown", &stats);

    eprintln!("\n[perf] ===== PRE-RENDER LOAD PHASE BREAKDOWN (real production fns) =====");
    eprintln!("  rewind_points (on disk)      : {num_rewind}");
    eprintln!("  rewind_points loaded in load : 0 (deferred → lazy)");
    eprintln!("  updates replayed (acp)       : {}", replayed.len());
    eprintln!("  ----------------------------------------------------------------");
    eprintln!(
        "  load_light (summary+chat)        : {:>8.1} ms",
        full_load_light.as_secs_f64() * 1e3
    );
    eprintln!(
        "    └─ rewind in load_light (now)  : {:>8.1} ms",
        rewind_cost.as_secs_f64() * 1e3
    );
    eprintln!(
        "    └─ summary + chat only         : {:>8.1} ms",
        load_light_no_rewind.as_secs_f64() * 1e3
    );
    eprintln!(
        "  lazy rewind: picker metas scan   : {:>8.1} ms (on /rewind open)",
        lazy_metas_scan.as_secs_f64() * 1e3
    );
    eprintln!(
        "  lazy rewind: full content load   : {:>8.1} ms (on rewind execute)",
        lazy_full_load.as_secs_f64() * 1e3
    );
    eprintln!(
        "  updates read+parse+filter        : {:>8.1} ms",
        updates_parse.as_secs_f64() * 1e3
    );
    eprintln!("  ----------------------------------------------------------------");
    eprintln!(
        "  TOTAL pre-render parse work      : {:>8.1} ms",
        (full_load_light + updates_parse).as_secs_f64() * 1e3
    );
    eprintln!("================================================================\n");

    assert!(!stats.bytes.is_empty(), "expected a non-empty updates file");
}

// ───────────────────────── TEST 2: true e2e ─────────────────────────

/// Replay stats derived from the shared recorder's raw callback log.
fn load_counters(rec: &SharedRecorded) -> (u64, u64, Option<Instant>, Option<Instant>) {
    let rec = rec.borrow();
    let count = rec.session_updates.len() as u64;
    let acu = rec
        .session_updates
        .iter()
        .filter(|(_, kind)| kind == "available_commands_update")
        .count() as u64;
    let first = rec.session_updates.first().map(|(at, _)| *at);
    let last = rec.session_updates.last().map(|(at, _)| *at);
    (count, acu, first, last)
}

/// Parse the production instrumentation JSON log into `(name -> elapsed_ms)`.
fn parse_instrumentation_log(path: &Path) -> Vec<(String, f64)> {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in contents.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let fields = v.get("fields").unwrap_or(&v);
        if fields.get("event").and_then(|e| e.as_str()) != Some("timing") {
            continue;
        }
        let Some(name) = fields.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        let us = fields
            .get("elapsed_us")
            .and_then(|u| u.as_u64())
            .or_else(|| {
                fields
                    .get("elapsed_ms")
                    .and_then(|m| m.as_u64())
                    .map(|m| m * 1000)
            })
            .unwrap_or(0);
        out.push((name.to_string(), us as f64 / 1000.0));
    }
    out
}

/// True end-to-end: real `MvpAgent` over real ACP pipes. Times `session/load`
/// (what the pager blocks on), counts replayed notifications, and prints the
/// shell's own per-phase instrumentation.
///
/// `#[ignore]` by default because it stands up the full agent; run with
/// `--ignored --nocapture`.
#[tokio::test(flavor = "current_thread")]
#[ignore = "heavy: builds a full MvpAgent and replays a large session; run with --ignored"]
async fn full_session_load_e2e() {
    echo_agent_extra_ca::ensure_default_crypto_provider();

    let server = echo_agent_test_support::MockInferenceServer::start()
        .await
        .unwrap();

    let echo_agent_home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let spec = perf_spec();
    let instr_log = echo_agent_home.path().join("instr.jsonl");

    // SAFETY: single-threaded current-thread runtime; set before any agent code
    // reads these process-globals (echo_agent_home()/instrumentation mode are OnceLock).
    unsafe {
        std::env::set_var("ECHO_AGENT_HOME", echo_agent_home.path());
        std::env::set_var("ECHO_AGENT_INSTRUMENTATION", "log");
        std::env::set_var("ECHO_AGENT_INSTRUMENTATION_LOG", &instr_log);
        std::env::set_var("ECHO_AGENT_CLI_CHAT_PROXY_BASE_URL", server.url());
        std::env::set_var("ECHO_AGENT_API_BASE_URL", server.url());
        std::env::set_var("ECHO_AGENT_API_KEY", "test-key-for-ci");
        std::env::set_var("ECHO_AGENT_TELEMETRY_ENABLED", "false");
        std::env::set_var("ECHO_AGENT_FEEDBACK_ENABLED", "false");
        std::env::set_var("ECHO_AGENT_TRACE_UPLOAD", "false");
    }

    // Install the production instrumentation layer so `instrumentation_timer!`
    // events are written to our temp log file.
    use tracing_subscriber::Registry;
    use tracing_subscriber::prelude::*;
    let _ = tracing_subscriber::registry()
        .with(echo_agent_runtime::instrumentation::layer::<Registry>())
        .try_init();

    let (info, dir) = prepare_session(echo_agent_home.path(), cwd.path(), &spec).await;
    let updates_path = dir.join("updates.jsonl");
    let rewind_path = dir.join("rewind_points.jsonl");
    eprintln!(
        "\n[perf] e2e session: updates={:.1} MB rewind={:.1} MB",
        file_size_mb(&updates_path),
        file_size_mb(&rewind_path)
    );
    let stats = updates_kind_breakdown(&updates_path);
    print_kind_breakdown("e2e", &stats);

    // Zero-data-loss guard: a pure load must never rewrite rewind_points.jsonl
    // (it is read lazily, never on the load path). Captured here, asserted after.
    let rewind_path_guard = rewind_path.clone();
    let rewind_fp_before = file_fingerprint(&rewind_path_guard);

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let (client, rec) = PerfRecorder::new();
            let loaded = load_session_via_agent(
                client,
                "perf-test",
                info.id.clone(),
                cwd.path().to_path_buf(),
            )
            .await;
            let load_started = loaded.load_started;
            let load_elapsed = loaded.load_elapsed;
            // Keep the connection alive so the post-load re-advertise still arrives.
            let _client_conn = loaded.client_conn;

            // Snapshot replay results immediately, before the post-load
            // AdvertiseCommands re-advertise can arrive, so `acu_replayed` counts
            // the ACUs forwarded during history replay (the skip count).
            let (replay_count, acu_replayed, first_at, last_at) = load_counters(&rec);
            let ttfn = first_at
                .map(|t| t.duration_since(load_started).as_secs_f64() * 1e3)
                .unwrap_or(0.0);
            let ttln = last_at
                .map(|t| t.duration_since(load_started).as_secs_f64() * 1e3)
                .unwrap_or(0.0);

            // The post-load `AdvertiseCommands` re-advertise (the safety basis for
            // dropping historical ACUs on replay) must reach the client. It's
            // enqueued at the end of `load_session` and forwarded async, so poll.
            // Replay forwards 0 ACUs, so any received ACU is the re-advertise.
            let readvertised = tokio::time::timeout(Duration::from_secs(10), async {
                while load_counters(&rec).1 == 0 {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .is_ok();

            // Flush the instrumentation writer and read the per-phase log.
            let _ = echo_agent_runtime::instrumentation::finalize();
            std::thread::sleep(Duration::from_millis(150));
            let mut phases = parse_instrumentation_log(&instr_log);
            phases.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

            // Replay-skip guard: the historical available_commands_update copies
            // (3197 in the pathological real session, hundreds synthetic) must
            // NOT be replayed.
            let acu_persisted = stats.count.get("available_commands_update").copied().unwrap_or(0);

            eprintln!("\n[perf] ===== END-TO-END session/load (what the pager waits on) =====");
            eprintln!("  total session/load round-trip : {:>9.1} ms", load_elapsed.as_secs_f64() * 1e3);
            eprintln!("  notifications replayed         : {:>9}", replay_count);
            eprintln!("  available_commands_update      : {acu_replayed:>9} replayed / {acu_persisted} on disk");
            eprintln!("  post-load re-advertise reached : {readvertised:>9}");
            eprintln!("  time-to-first notification     : {ttfn:>9.1} ms");
            eprintln!("  time-to-last notification      : {ttln:>9.1} ms");
            eprintln!("  ---- shell-side per-phase instrumentation (elapsed) ----");
            if phases.is_empty() {
                eprintln!("  (no instrumentation events captured)");
            } else {
                for (name, ms) in &phases {
                    eprintln!("  {name:<40} {ms:>9.1} ms");
                }
            }
            eprintln!("================================================================\n");

            assert!(replay_count > 0, "expected replayed notifications during load");
            // The lazy rewind file must be byte-for-byte unchanged by a load.
            assert_eq!(
                file_fingerprint(&rewind_path_guard),
                rewind_fp_before,
                "rewind_points.jsonl must be unchanged after a load (zero data loss)"
            );
            // The thousands of persisted ACUs must be skipped on replay...
            assert!(
                acu_persisted > 100,
                "fixture should have many persisted ACUs to exercise the skip"
            );
            assert!(
                acu_replayed < 100,
                "historical available_commands_update must be skipped on replay \
                 (replayed {acu_replayed} of {acu_persisted} persisted)"
            );
            // ...but the catalog IS re-advertised to the client after load.
            assert!(
                readvertised,
                "post-load available_commands_update re-advertise must reach the client"
            );
        })
        .await;
}
