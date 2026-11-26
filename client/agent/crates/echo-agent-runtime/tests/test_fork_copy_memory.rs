//! Bounds peak RSS while forking a synthetic large session, for both the
//! passthrough and prompt-cut copy modes.
//!
//! Run:
//!   cargo test -p echo-agent-runtime --features test-support --test test_fork_copy_memory \
//!       fork_copy_peak_rss_under_budget -- --ignored --nocapture

#![cfg(unix)]

use std::io::BufRead;

use agent_client_protocol as acp;
use echo_agent_runtime::session::info::Info;
use echo_agent_runtime::session::storage::{
    CopySessionOptions, JsonlStorageAdapter, StorageAdapter,
};
use echo_agent_runtime::session::testkit::synth::make_session_with_size;
use echo_agent_test_support::env::env_parse;
use echo_agent_test_support::resources::RssSampler;
use tempfile::TempDir;

const BYTES_PER_MB: f64 = 1024.0 * 1024.0;

/// Count the non-empty lines of `path` without materializing it.
fn count_jsonl_lines(path: &std::path::Path) -> usize {
    let reader = std::io::BufReader::new(std::fs::File::open(path).expect("open updates.jsonl"));
    reader
        .split(b'\n')
        .map(|line| line.expect("read updates.jsonl"))
        .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "peak-memory soak; run with --ignored --nocapture"]
async fn fork_copy_peak_rss_under_budget() {
    let target_mb = env_parse("FORK_COPY_MB", 64u64);
    // Under 1x the file, so even one whole-file materialization fails.
    let budget_mb = env_parse("FORK_COPY_MAX_PEAK_MB", 48u64);

    let root = TempDir::new().unwrap();
    let source = make_session_with_size(root.path(), target_mb * 1024 * 1024).await;
    let adapter = JsonlStorageAdapter::with_root(root.path().to_path_buf());
    let updates_path = adapter
        .updates_file_path(&source)
        .expect("source updates path");
    let on_disk_bytes = std::fs::metadata(&updates_path)
        .expect("stat updates.jsonl")
        .len();
    let source_lines = count_jsonl_lines(&updates_path);

    // A cut past every prompt keeps all lines while exercising the two-pass
    // survivor machinery, so the parity assert below holds for both modes.
    for (mode, target_prompt_index) in [("default", None), ("prompt_cut", Some(source_lines))] {
        let target = Info {
            id: acp::SessionId::new(format!("fork-soak-{mode}")),
            cwd: "/bench/workspace-fork".to_string(),
        };
        let options = CopySessionOptions {
            target_prompt_index,
            ..Default::default()
        };

        let sampler = RssSampler::start();
        let result = adapter
            .copy_session_data(&source, &target, options)
            .await
            .expect("fork copy");
        let outcome = sampler.finish().against_budget(budget_mb);

        // Report before asserting so a regression still emits the summary.
        eprintln!(
            "FORK_COPY_MEMORY_SUMMARY {}",
            serde_json::json!({
                "mode": mode,
                "on_disk_updates_bytes": on_disk_bytes,
                "on_disk_updates_mb": on_disk_bytes as f64 / BYTES_PER_MB,
                "source_lines": source_lines,
                "updates_copied": result.updates_copied,
                "baseline_rss_mb": outcome.baseline_mb(),
                "peak_rss_mb": outcome.peak_rss_mb(),
                "peak_rss_growth_mb": outcome.peak_growth_mb(),
                "budget_mb": outcome.budget_mb,
                "rss_measurable": outcome.measurable(),
                "pass": outcome.pass(),
            })
        );

        // Synth emits no filtered update kinds, so a copy that stays bounded
        // by dropping lines fails this check.
        assert_eq!(
            result.updates_copied, source_lines,
            "{mode}: fork must copy every source update"
        );
        assert!(
            outcome.measurable(),
            "{mode}: RSS sampling unavailable; the soak cannot enforce a bound"
        );
        assert!(
            outcome.within_budget(),
            "{mode}: peak RSS grew {:.1} MB over baseline while forking a {:.1} MB updates file \
             (bound {} MB)",
            outcome.peak_growth_mb(),
            on_disk_bytes as f64 / BYTES_PER_MB,
            outcome.budget_mb,
        );
    }
}
