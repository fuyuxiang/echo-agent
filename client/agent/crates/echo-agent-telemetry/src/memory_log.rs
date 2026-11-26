//! Memory system tracing target and optional file-based logging layer.
//!
//! Provides a dedicated tracing target (`echo_agent_memory`) with an optional
//! file logger that writes to `~/.echo-agent/logs/memory.log`.
//!
//! ## When to use
//!
//! Use `tracing::info!(target: memory_log::TARGET, ...)` at memory system
//! lifecycle points — config resolution, storage init, flush, search, etc.
//! These events are always emitted (zero cost when the layer is absent).
//!
//! ## Enabling (debug builds)
//!
//! ```bash
//! # build with memory logging enabled, then:
//! ECHO_AGENT_MEMORY_LOG=0 echoagent                # disable even when enabled
//! tail -f ~/.echo-agent/logs/memory.log      # watch in another terminal
//! ```

/// Tracing target for all memory system operations.
pub const TARGET: &str = "echo_agent_memory";

#[cfg(feature = "memory-log")]
mod inner {
    use std::fmt;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::Instant;

    use tracing::Subscriber;
    use tracing_subscriber::fmt::format::Writer;
    use tracing_subscriber::fmt::time::FormatTime;
    use tracing_subscriber::fmt::writer::BoxMakeWriter;
    use tracing_subscriber::layer::Layer;
    use tracing_subscriber::registry::LookupSpan;

    use super::TARGET;
    use echo_agent_config::echo_agent_home;

    const ENV_MEMORY_LOG: &str = "ECHO_AGENT_MEMORY_LOG";

    static LOG_GUARD: std::sync::OnceLock<
        Mutex<Option<tracing_appender::non_blocking::WorkerGuard>>,
    > = std::sync::OnceLock::new();

    #[derive(Clone)]
    struct UptimeTimer {
        epoch: Instant,
    }

    impl UptimeTimer {
        fn new() -> Self {
            Self {
                epoch: Instant::now(),
            }
        }
    }

    impl FormatTime for UptimeTimer {
        fn format_time(&self, w: &mut Writer<'_>) -> fmt::Result {
            let elapsed = self.epoch.elapsed();
            write!(w, "+{}.{:03}s", elapsed.as_secs(), elapsed.subsec_millis())
        }
    }

    /// Build the memory log layer.
    ///
    /// Writes to `~/.echo-agent/logs/memory.log`. Filters to `echo_agent_memory=trace`.
    /// Set `ECHO_AGENT_MEMORY_LOG=0` to disable, `ECHO_AGENT_MEMORY_LOG=/path` to redirect.
    pub fn layer<S>() -> Option<impl Layer<S>>
    where
        S: Subscriber + for<'span> LookupSpan<'span>,
    {
        let path = resolve_log_path()?;

        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let file = match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!("[memory-log] Failed to open {:?}: {}", path, e);
                return None;
            }
        };

        let (non_blocking, guard) = tracing_appender::non_blocking(file);
        let guard_slot = LOG_GUARD.get_or_init(|| Mutex::new(None));
        if let Ok(mut slot) = guard_slot.lock() {
            *slot = Some(guard);
        }

        let filter = tracing_subscriber::filter::EnvFilter::new(format!("{TARGET}=trace"));
        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_target(true)
            .with_ansi(false)
            .with_thread_ids(true)
            .with_timer(UptimeTimer::new())
            .with_writer(BoxMakeWriter::new(non_blocking))
            .with_filter(filter);

        tracing::info!("[memory-log] Memory logging enabled");
        Some(fmt_layer)
    }

    fn resolve_log_path() -> Option<PathBuf> {
        let default_path = || echo_agent_home().join("logs").join("memory.log");
        let raw = match std::env::var(ENV_MEMORY_LOG) {
            Ok(val) => val,
            Err(_) => return Some(default_path()),
        };
        let raw = raw.trim();
        match raw {
            "" | "0" | "false" | "off" | "no" => None,
            "1" | "true" | "on" | "yes" => Some(default_path()),
            path => Some(PathBuf::from(path)),
        }
    }
}

#[cfg(feature = "memory-log")]
pub use inner::layer;
