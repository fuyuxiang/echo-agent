//! Wall time of child `updates.jsonl` lookup — the live-`SubagentSpawned` path.
//!
//! `Relocation` (pre-fix) walks every cwd bucket and `lstat`s each `summary.json`
//! after a hinted miss. `HintedOnly` (post-fix) returns on that miss.
//!
//! Default fixture: 180 encoded cwds × 20 sessions = 3,600 summaries (same
//! order as a fat local `~/.echo-agent/sessions`). Override with env:
//!
//! ```text
//! cargo bench -p echo-agent-runtime --bench child_replay_lookup
//! CHILD_REPLAY_LOOKUP_CWDS=3000 CHILD_REPLAY_LOOKUP_PER_CWD=3 cargo bench ...
//! # read-only against a real store (no writes):
//! CHILD_REPLAY_LOOKUP_HOME=$HOME/.echo-agent cargo bench -p echo-agent-runtime --bench child_replay_lookup
//! ```

use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::Duration;

use criterion::{
    BenchmarkId, Criterion, SamplingMode, Throughput, criterion_group, criterion_main,
};
use echo_agent_config::encode_cwd_dirname;
use echo_agent_runtime::session::storage::{
    ReplayEmission, ReplayLookupFallback, ReplayPathHint, stream_replay_updates_at_hinted,
};
use tempfile::TempDir;

const DEFAULT_CWDS: usize = 180;
const DEFAULT_PER_CWD: usize = 20;
const DRAIN_N: usize = 8;
const SAMPLE_SIZE: usize = 10;

struct Fixture {
    _keep: Option<TempDir>,
    home: PathBuf,
    parent_cwd: PathBuf,
    hit_id: String,
    miss_id: String,
    session_count: usize,
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(default)
}

fn lookup(home: &Path, session_id: &str, parent_cwd: &Path, fallback: ReplayLookupFallback) {
    let emission = stream_replay_updates_at_hinted(
        session_id,
        home,
        ReplayPathHint {
            parent_cwd: Some(parent_cwd),
            child_cwd: None,
            fallback,
        },
        |_| {},
    )
    .expect("lookup");
    let _ = black_box(emission);
}

fn synth_fixture() -> Fixture {
    let cwds = env_usize("CHILD_REPLAY_LOOKUP_CWDS", DEFAULT_CWDS);
    let per_cwd = env_usize("CHILD_REPLAY_LOOKUP_PER_CWD", DEFAULT_PER_CWD);
    let root = TempDir::new().expect("tempdir");
    let sessions = root.path().join("sessions");
    fs::create_dir(&sessions).expect("sessions root");

    let parent_cwd = PathBuf::from("/bench/parent");
    let parent_dir = sessions.join(encode_cwd_dirname(&parent_cwd.to_string_lossy()));
    fs::create_dir(&parent_dir).expect("parent cwd dir");

    let hit_id = "bench-hit-child".to_string();
    let hit_dir = parent_dir.join(&hit_id);
    fs::create_dir(&hit_dir).expect("hit session");
    fs::write(hit_dir.join("summary.json"), "{}").expect("hit summary");
    fs::write(hit_dir.join("updates.jsonl"), "{}\n").expect("hit updates");

    let mut session_count = 1usize;
    for cwd_i in 0..cwds {
        let cwd = format!("/bench/ws/{cwd_i:04}");
        let encoded = encode_cwd_dirname(&cwd);
        let cwd_dir = sessions.join(encoded);
        fs::create_dir(&cwd_dir).expect("cwd dir");
        for sess_i in 0..per_cwd {
            let id = format!("bench-session-{cwd_i:04}-{sess_i:02}");
            let dir = cwd_dir.join(&id);
            fs::create_dir(&dir).expect("session dir");
            fs::write(dir.join("summary.json"), "{}").expect("summary");
            session_count += 1;
        }
    }

    Fixture {
        home: root.path().to_path_buf(),
        parent_cwd,
        hit_id,
        miss_id: "bench-missing-child".to_string(),
        session_count,
        _keep: Some(root),
    }
}

fn real_home_fixture(home: PathBuf) -> Fixture {
    let sessions = home.join("sessions");
    let session_count = fs::read_dir(&sessions)
        .map(|cwds| {
            cwds.filter_map(Result::ok)
                .filter(|e| e.path().is_dir())
                .map(|cwd| {
                    fs::read_dir(cwd.path())
                        .map(|ents| {
                            ents.filter_map(Result::ok)
                                .filter(|e| e.path().is_dir())
                                .count()
                        })
                        .unwrap_or(0)
                })
                .sum()
        })
        .unwrap_or(0);
    Fixture {
        _keep: None,
        home,
        parent_cwd: PathBuf::from("/tmp"),
        hit_id: String::new(),
        miss_id: "bench-missing-child".to_string(),
        session_count,
    }
}

fn build_fixture() -> Fixture {
    if let Some(home) = std::env::var_os("CHILD_REPLAY_LOOKUP_HOME") {
        return real_home_fixture(PathBuf::from(home));
    }
    synth_fixture()
}

fn bench_child_replay_lookup(c: &mut Criterion) {
    let fixture = build_fixture();
    eprintln!(
        "child_replay_lookup fixture: sessions={} home={}",
        fixture.session_count,
        fixture.home.display()
    );

    let mut group = c.benchmark_group("child_replay_lookup");
    group
        .sampling_mode(SamplingMode::Flat)
        .sample_size(SAMPLE_SIZE)
        .measurement_time(Duration::from_secs(20))
        .throughput(Throughput::Elements(1));

    group.bench_function(
        BenchmarkId::new("relocation_miss", fixture.session_count),
        |b| {
            b.iter(|| {
                lookup(
                    &fixture.home,
                    &fixture.miss_id,
                    &fixture.parent_cwd,
                    ReplayLookupFallback::Relocation,
                );
            });
        },
    );
    group.bench_function(
        BenchmarkId::new("hinted_only_miss", fixture.session_count),
        |b| {
            b.iter(|| {
                lookup(
                    &fixture.home,
                    &fixture.miss_id,
                    &fixture.parent_cwd,
                    ReplayLookupFallback::HintedOnly,
                );
            });
        },
    );
    if !fixture.hit_id.is_empty() {
        group.bench_function(BenchmarkId::new("hinted_hit", fixture.session_count), |b| {
            b.iter(|| {
                let emission = stream_replay_updates_at_hinted(
                    &fixture.hit_id,
                    &fixture.home,
                    ReplayPathHint {
                        parent_cwd: Some(&fixture.parent_cwd),
                        child_cwd: None,
                        fallback: ReplayLookupFallback::HintedOnly,
                    },
                    |_| {},
                )
                .expect("hit");
                assert_eq!(emission, ReplayEmission::Empty);
                let _ = black_box(emission);
            });
        });
    }

    group.throughput(Throughput::Elements(DRAIN_N as u64));
    group.bench_function(
        BenchmarkId::new("n8_relocation_miss", fixture.session_count),
        |b| {
            b.iter(|| {
                for _ in 0..DRAIN_N {
                    lookup(
                        &fixture.home,
                        &fixture.miss_id,
                        &fixture.parent_cwd,
                        ReplayLookupFallback::Relocation,
                    );
                }
            });
        },
    );
    group.bench_function(
        BenchmarkId::new("n8_hinted_only_miss", fixture.session_count),
        |b| {
            b.iter(|| {
                for _ in 0..DRAIN_N {
                    lookup(
                        &fixture.home,
                        &fixture.miss_id,
                        &fixture.parent_cwd,
                        ReplayLookupFallback::HintedOnly,
                    );
                }
            });
        },
    );
    group.finish();
}

criterion_group!(benches, bench_child_replay_lookup);
criterion_main!(benches);
