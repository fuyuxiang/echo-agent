//! `grove_wt_create` create-strategy telemetry.
//!
//! Histogram-shaped: every completed create records `(strategy, duration)`.
//! Counters are process-local so tests can assert emission without a Prometheus
//! scrape; production scrapes the matching tracing fields.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static CREATE_NFS: AtomicU64 = AtomicU64::new(0);
static CREATE_GROVE_FUSE: AtomicU64 = AtomicU64::new(0);
static CREATE_GROVE_NFS: AtomicU64 = AtomicU64::new(0);
static CREATE_COPY: AtomicU64 = AtomicU64::new(0);
static CREATE_BTRFS: AtomicU64 = AtomicU64::new(0);
static CREATE_OVERLAY: AtomicU64 = AtomicU64::new(0);
static CREATE_GIT: AtomicU64 = AtomicU64::new(0);
static CREATE_OTHER: AtomicU64 = AtomicU64::new(0);
static LAST_DURATION_NS: AtomicU64 = AtomicU64::new(0);

/// Record one completed worktree create. `strategy` matches the design label
/// set (`nfs` / `copy` / `btrfs` / `overlay`); `git` and `standalone` map to
/// `copy`/`git` as appropriate for the metric family.
pub fn record_grove_wt_create(strategy: &'static str, duration: Duration) {
    let metric_strategy = match strategy {
        "standalone" => "copy",
        other => other,
    };
    let counter = match metric_strategy {
        "nfs" => &CREATE_NFS,
        "grove-fuse" => &CREATE_GROVE_FUSE,
        "grove-nfs" => &CREATE_GROVE_NFS,
        "copy" => &CREATE_COPY,
        "btrfs" => &CREATE_BTRFS,
        "overlay" => &CREATE_OVERLAY,
        "git" => &CREATE_GIT,
        _ => &CREATE_OTHER,
    };
    counter.fetch_add(1, Ordering::Relaxed);
    LAST_DURATION_NS.store(duration.as_nanos() as u64, Ordering::Relaxed);
    tracing::info!(
        metric = "grove_wt_create_duration_seconds",
        strategy = metric_strategy,
        duration_seconds = duration.as_secs_f64(),
        "grove_wt_create"
    );
}

/// Process-local count for `grove_wt_create_duration_seconds{strategy}`.
#[must_use]
pub fn grove_wt_create_count(strategy: &str) -> u64 {
    let c = match strategy {
        "nfs" => &CREATE_NFS,
        "grove-fuse" => &CREATE_GROVE_FUSE,
        "grove-nfs" => &CREATE_GROVE_NFS,
        "copy" => &CREATE_COPY,
        "btrfs" => &CREATE_BTRFS,
        "overlay" => &CREATE_OVERLAY,
        "git" => &CREATE_GIT,
        _ => &CREATE_OTHER,
    };
    c.load(Ordering::Relaxed)
}

#[must_use]
pub fn grove_wt_create_last_duration_ns() -> u64 {
    LAST_DURATION_NS.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_increments_named_strategy_counter() {
        let before = grove_wt_create_count("copy");
        record_grove_wt_create("copy", Duration::from_millis(12));
        assert_eq!(grove_wt_create_count("copy"), before + 1);
        assert!(grove_wt_create_last_duration_ns() >= 12_000_000);
    }

    #[test]
    fn standalone_counts_as_copy_metric_label() {
        let before = grove_wt_create_count("copy");
        record_grove_wt_create("standalone", Duration::from_millis(1));
        assert_eq!(grove_wt_create_count("copy"), before + 1);
    }

    #[test]
    fn grove_fuse_and_grove_nfs_have_named_counters() {
        let fuse_before = grove_wt_create_count("grove-fuse");
        let nfs_before = grove_wt_create_count("grove-nfs");
        let alias_before = grove_wt_create_count("nfs");
        record_grove_wt_create("grove-fuse", Duration::from_millis(1));
        record_grove_wt_create("grove-nfs", Duration::from_millis(1));
        record_grove_wt_create("nfs", Duration::from_millis(1));
        assert_eq!(grove_wt_create_count("grove-fuse"), fuse_before + 1);
        assert_eq!(grove_wt_create_count("grove-nfs"), nfs_before + 1);
        assert_eq!(grove_wt_create_count("nfs"), alias_before + 1);
    }
}
