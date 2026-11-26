//! Regression guard for the fork/resume replay OOM: a counting allocator checks
//! the streaming load ([`stream_replay_updates_at`]) peaks far below the old
//! parse-all load and forwards identical content.

#![cfg(unix)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use agent_client_protocol as acp;
use tempfile::TempDir;

use echo_agent_runtime::session::storage::{
    ReplayEmission, SessionUpdate, UpdatesIterator, filter_rewind_updates,
    stream_replay_updates_at, strip_context_wrappers,
};
use echo_agent_runtime::session::testkit::synth::{self, SessionSpec};

// `Relaxed` suffices: single-threaded high-water counters read after the
// measured section, with no ordering dependency on other memory.
struct CountingAlloc;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let now = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

fn begin_measure() -> usize {
    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    base
}
fn peak() -> usize {
    PEAK.load(Ordering::Relaxed)
}

/// Heavy ACU catalog, no rewind points: the parity assert covers the
/// typed-to-string swap over this transcript, not rewind-marker filtering
/// (`synth` emits no `rewind_marker` envelopes on the replay path).
fn fork_replay_spec() -> SessionSpec {
    SessionSpec::from_env_prefixed(
        "FORK_REPLAY",
        SessionSpec {
            turns: 60,
            acu_per_turn: 8,
            catalog_commands: 200,
            catalog_desc_len: 48,
            agent_chunks_per_turn: 4,
            agent_chunk_len: 1500,
            rewind_points: 0,
            files_per_rewind: 0,
            file_content_len: 0,
        },
    )
}

fn reference_load_all(updates_path: &Path) -> Vec<acp::SessionUpdate> {
    let iter = UpdatesIterator::open(updates_path)
        .expect("open updates")
        .expect("updates file exists");
    let all: Vec<SessionUpdate> = iter.filter_map(|r| r.ok()).collect();
    let filtered = filter_rewind_updates(all);
    filtered
        .into_iter()
        .filter_map(|u| match u {
            SessionUpdate::Acp(notif) => match notif.update {
                acp::SessionUpdate::AvailableCommandsUpdate(_) => None,
                other => Some(strip_context_wrappers(other)),
            },
            SessionUpdate::EchoAgent(_) => None,
        })
        .collect()
}

fn serialize(u: &acp::SessionUpdate) -> String {
    serde_json::to_string(u).expect("serialize replayed update")
}

/// Streaming holds one `read_to_string` copy (~1x) plus transient per-update
/// structs; the old parse-all path held several multiples. Headroom over 1x.
const MAX_STREAM_PEAK_TO_DISK_RATIO: f64 = 2.0;

// Serial: the process-global counters are only valid when no other test
// allocates concurrently.
#[test]
#[serial_test::serial]
fn fork_replay_stream_is_bounded_and_faithful() {
    let root = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let opts = fork_replay_spec();

    let (id, updates_path) = {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let out = rt.block_on(async {
            let (info, dir) = synth::prepare_session(root.path(), cwd.path(), &opts).await;
            (info.id.0.to_string(), dir.join("updates.jsonl"))
        });
        drop(rt);
        out
    };

    let on_disk = std::fs::metadata(&updates_path).unwrap().len() as usize;
    // The ratio bound is only meaningful once fixed overhead is dwarfed by
    // content; guard against a shrunk spec making the assert trivial.
    assert!(
        on_disk > 256 * 1024,
        "fork-replay fixture must be sizeable to bound the peak ratio, got {on_disk} B"
    );

    let base_old = begin_measure();
    let reference = reference_load_all(&updates_path);
    let old_peak = peak() - base_old;
    let ref_count = reference.len();
    let ref_serialized: Vec<String> = reference.iter().map(serialize).collect();
    drop(reference);

    let mut stream_count = 0usize;
    let base_new = begin_measure();
    let outcome = stream_replay_updates_at(&id, root.path(), |_update| {
        stream_count += 1;
    })
    .expect("stream_replay_updates_at");
    let new_peak = peak() - base_new;

    // Serializing during the measured pass would inflate its peak, so replay a
    // second time purely to collect the parity data.
    let mut streamed_serialized: Vec<String> = Vec::new();
    let _ = stream_replay_updates_at(&id, root.path(), |u| {
        streamed_serialized.push(serialize(&u))
    })
    .expect("stream again");

    let new_ratio = new_peak as f64 / on_disk.max(1) as f64;
    let old_ratio = old_peak as f64 / on_disk.max(1) as f64;
    eprintln!(
        "FORK_REPLAY_MEMORY {}",
        serde_json::json!({
            "on_disk_mb": on_disk as f64 / 1e6,
            "old_parse_all_peak_mb": old_peak as f64 / 1e6,
            "old_ratio": old_ratio,
            "new_stream_peak_mb": new_peak as f64 / 1e6,
            "new_ratio": new_ratio,
            "reduction_x": old_peak as f64 / new_peak.max(1) as f64,
            "count": stream_count,
        })
    );

    assert!(
        outcome == ReplayEmission::Emitted && stream_count > 0,
        "expected a non-empty replay"
    );
    assert_eq!(
        streamed_serialized, ref_serialized,
        "streamed updates must match the typed parse-all path byte-for-byte, in order"
    );
    assert_eq!(
        stream_count, ref_count,
        "streamed update count must equal the typed path count"
    );
    assert!(
        new_peak < old_peak,
        "streaming peak ({new_peak} B) must be below the parse-all peak ({old_peak} B)"
    );
    assert!(
        new_ratio < MAX_STREAM_PEAK_TO_DISK_RATIO,
        "streaming peak {new_ratio:.2}x on-disk must stay near the file size \
         (< {MAX_STREAM_PEAK_TO_DISK_RATIO}x); the whole transcript is no longer \
         materialized as typed structs"
    );
}
