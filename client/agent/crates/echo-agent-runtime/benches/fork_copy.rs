//! Throughput benchmark for `StorageAdapter::copy_session_data` over a
//! synthesized production-shaped session; the peak-RSS bound lives in
//! `tests/test_fork_copy_memory.rs`.
//!
//! Run: `cargo bench -p echo-agent-runtime --bench fork_copy`
//! Size override: `FORK_BENCH_MB=64 cargo bench ...` (default 16 MB).

use std::hint::black_box;
use std::time::Duration;

use agent_client_protocol as acp;
use criterion::{
    BenchmarkId, Criterion, SamplingMode, Throughput, criterion_group, criterion_main,
};
use echo_agent_runtime::session::info::Info;
use echo_agent_runtime::session::storage::{
    CopySessionOptions, JsonlStorageAdapter, StorageAdapter,
};
use echo_agent_runtime::session::testkit::synth::make_session_with_size_blocking;
use tempfile::TempDir;

fn bench_fork_copy(c: &mut Criterion) {
    let target_mb: u64 = std::env::var("FORK_BENCH_MB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(16);
    let root = TempDir::new().expect("tempdir");
    let source = make_session_with_size_blocking(root.path(), target_mb * 1024 * 1024);
    let adapter = JsonlStorageAdapter::with_root(root.path().to_path_buf());
    let updates_len = std::fs::metadata(adapter.updates_file_path(&source).expect("updates path"))
        .expect("updates.jsonl")
        .len();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("bench runtime");

    let mut group = c.benchmark_group("fork_copy");
    group
        .sampling_mode(SamplingMode::Flat)
        .sample_size(10)
        .measurement_time(Duration::from_secs(30))
        .throughput(Throughput::Bytes(updates_len));
    group.bench_function(
        BenchmarkId::new("copy_session_data", format!("{target_mb}MB")),
        |b| {
            let mut n = 0usize;
            b.iter(|| {
                n += 1;
                let target = Info {
                    id: acp::SessionId::new(format!("fork-bench-dst-{n}")),
                    cwd: "/bench/workspace-fork".to_string(),
                };
                let result = rt
                    .block_on(adapter.copy_session_data(
                        &source,
                        &target,
                        CopySessionOptions::default(),
                    ))
                    .expect("fork copy");
                // Keep each iteration's output dir from accumulating.
                if let Some(dir) = adapter
                    .updates_file_path(&target)
                    .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
                {
                    std::fs::remove_dir_all(&dir).ok();
                }
                black_box(result)
            });
        },
    );
    group.finish();
}

criterion_group!(benches, bench_fork_copy);
criterion_main!(benches);
