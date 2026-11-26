//! Per-event process resource snapshot; each CPU share covers the interval since the previous derived window.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CpuWindow {
    pub window_ms: u64,
    pub share_percent: f64,
    pub child_share_percent: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProcessMetrics {
    pub cpu: Option<CpuWindow>,
    pub cpu_time_ms: Option<u64>,
    pub child_cpu_time_ms: Option<u64>,
    pub cpu_user_ms: Option<u64>,
    pub cpu_system_ms: Option<u64>,
    pub rss_bytes: Option<u64>,
    pub footprint_bytes: Option<u64>,
    pub memory_limit_bytes: Option<u64>,
    pub cpu_cores: Option<u64>,
    /// Excludes time suspended after launch.
    pub uptime_secs: u64,
}

struct CpuBaseline {
    cpu_time: Duration,
    child_cpu_time: Option<Duration>,
    taken_at: Instant,
}

struct FirstSnapshot {
    at: Instant,
    process_age_secs: u64,
}

static FIRST_SNAPSHOT: OnceLock<FirstSnapshot> = OnceLock::new();
static CPU_BASELINE: parking_lot::Mutex<Option<CpuBaseline>> = parking_lot::Mutex::new(None);
static CPU_CORES: OnceLock<Option<u64>> = OnceLock::new();

struct CpuSample {
    now: Instant,
    cpu: echo_agent_tty_utils::ProcessCpu,
    window: Option<CpuWindow>,
}

const MIN_CPU_WINDOW: Duration = Duration::from_millis(1);

#[doc(hidden)]
pub fn snapshot() -> ProcessMetrics {
    // Clock and CPU read inside the lock: concurrent snapshots must
    // partition time.
    let sample = {
        let mut baseline = CPU_BASELINE.lock();
        let now = Instant::now();
        let cpu = echo_agent_tty_utils::sample_process_cpu();

        let mut window = None;
        // Advance only when a window is derived: this series' emitted
        // windows partition its time exactly.
        match (&*baseline, cpu.self_time) {
            (Some(prev), Some(cpu_time)) => {
                let elapsed = now.saturating_duration_since(prev.taken_at);
                if elapsed >= MIN_CPU_WINDOW {
                    window = Some(CpuWindow {
                        window_ms: duration_ms(elapsed),
                        share_percent: share_percent(
                            cpu_time.saturating_sub(prev.cpu_time),
                            elapsed,
                        ),
                        child_share_percent: match (prev.child_cpu_time, cpu.children_time) {
                            (Some(prev_child), Some(child)) => {
                                Some(share_percent(child.saturating_sub(prev_child), elapsed))
                            }
                            _ => None,
                        },
                    });
                    *baseline = Some(CpuBaseline {
                        cpu_time,
                        child_cpu_time: cpu.children_time,
                        taken_at: now,
                    });
                }
            }
            (None, Some(cpu_time)) => {
                *baseline = Some(CpuBaseline {
                    cpu_time,
                    child_cpu_time: cpu.children_time,
                    taken_at: now,
                });
            }
            _ => {}
        }
        CpuSample { now, cpu, window }
    };

    if sample.cpu.self_time.is_none() {
        log_read_failure_once(&CPU_READ_FAILURE, "cpu");
    }

    let first = FIRST_SNAPSHOT.get_or_init(|| FirstSnapshot {
        at: sample.now,
        process_age_secs: echo_agent_tty_utils::process_start_time()
            .and_then(|start| std::time::SystemTime::now().duration_since(start).ok())
            .map_or(0, |age| age.as_secs()),
    });
    let uptime_secs =
        first.process_age_secs + sample.now.saturating_duration_since(first.at).as_secs();

    let memory = echo_agent_tty_utils::sample_process_memory();
    if memory.rss_bytes.is_none() {
        // macOS memory reads fail via mach codes; the errno may be stale.
        log_read_failure_once(&MEMORY_READ_FAILURE, "memory");
    }
    ProcessMetrics {
        cpu: sample.window,
        cpu_time_ms: sample.cpu.self_time.map(duration_ms),
        child_cpu_time_ms: sample.cpu.children_time.map(duration_ms),
        cpu_user_ms: sample.cpu.self_user_time.map(duration_ms),
        cpu_system_ms: sample.cpu.self_system_time.map(duration_ms),
        rss_bytes: memory.rss_bytes,
        footprint_bytes: memory.footprint_bytes,
        memory_limit_bytes: echo_agent_tty_utils::process_memory_limit(),
        cpu_cores: *CPU_CORES.get_or_init(|| {
            std::thread::available_parallelism()
                .ok()
                .map(|n| usize::from(n) as u64)
        }),
        uptime_secs,
    }
}

static CPU_READ_FAILURE: std::sync::Once = std::sync::Once::new();
static MEMORY_READ_FAILURE: std::sync::Once = std::sync::Once::new();

fn log_read_failure_once(logged: &'static std::sync::Once, reading: &'static str) {
    logged.call_once(|| {
        tracing::debug!(
            reading,
            errno = %std::io::Error::last_os_error(),
            "process resource reading unavailable"
        );
    });
}

/// Deliberately unclamped: a multi-threaded burst exceeds 100.
fn share_percent(cpu_delta: Duration, elapsed: Duration) -> f64 {
    cpu_delta.as_secs_f64() / elapsed.as_secs_f64() * 100.0
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "process_metrics_tests.rs"]
mod tests;
