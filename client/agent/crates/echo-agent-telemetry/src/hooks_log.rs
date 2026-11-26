//! Hooks and plugins tracing target and optional file-based logging layer.
//!
//! A dedicated tracing target for hooks and plugins subsystems with an optional
//! file logger that writes to `~/.echo-agent/logs/hooks.log`.
//!
//! ## When to use
//!
//! Use regular `tracing::info!` / `tracing::debug!` / `tracing::warn!` with
//! targets `echo_agent_hooks` or `echo_agent_core::plugins` at key lifecycle
//! points — discovery, dispatch, execution, errors.
//!
//! ## Enabling
//!
//! ```bash
//! ECHO_AGENT_HOOKS_LOG=1 echoagent              # enable, write to ~/.echo-agent/logs/hooks.log
//! ECHO_AGENT_HOOKS_LOG=/tmp/h.log echoagent     # write to custom path
//! ECHO_AGENT_HOOKS_LOG=0 echoagent              # explicitly disable
//! tail -f ~/.echo-agent/logs/hooks.log     # watch in another terminal
//! ```

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

use echo_agent_config::echo_agent_home;

const ENV_HOOKS_LOG: &str = "ECHO_AGENT_HOOKS_LOG";

static LOG_GUARD: std::sync::OnceLock<Mutex<Option<tracing_appender::non_blocking::WorkerGuard>>> =
    std::sync::OnceLock::new();

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

/// Build the hooks/plugins log layer.
///
/// Writes to `~/.echo-agent/logs/hooks.log` (or custom path via `ECHO_AGENT_HOOKS_LOG`).
/// Filters to hooks (`echo_agent_hooks`) and plugins (`echo_agent_core::plugins`) targets.
/// Set `ECHO_AGENT_HOOKS_LOG=0` to disable, `ECHO_AGENT_HOOKS_LOG=/path` to redirect.
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
            tracing::warn!("[hooks-log] Failed to open {:?}: {}", path, e);
            return None;
        }
    };

    let (non_blocking, guard) = tracing_appender::non_blocking(file);
    let guard_slot = LOG_GUARD.get_or_init(|| Mutex::new(None));
    if let Ok(mut slot) = guard_slot.lock() {
        *slot = Some(guard);
    }

    // Filter for both hooks and plugins targets at debug level
    let filter = tracing_subscriber::filter::EnvFilter::new(
        "echo_agent_hooks=debug,echo_agent_core::plugins=debug",
    );
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_ansi(false)
        .with_thread_ids(true)
        .with_timer(UptimeTimer::new())
        .with_writer(BoxMakeWriter::new(non_blocking))
        .with_filter(filter);

    tracing::info!(
        "[hooks-log] Hooks/plugins logging enabled: {}",
        path.display()
    );
    Some(fmt_layer)
}

fn resolve_log_path() -> Option<PathBuf> {
    let default_path = || echo_agent_home().join("logs").join("hooks.log");
    let raw = match std::env::var(ENV_HOOKS_LOG) {
        Ok(val) => val,
        Err(_) => return None, // opt-in only
    };
    let raw = raw.trim();
    match raw {
        "" | "0" | "false" | "off" | "no" => None,
        "1" | "true" | "on" | "yes" => Some(default_path()),
        other => Some(PathBuf::from(other)),
    }
}
