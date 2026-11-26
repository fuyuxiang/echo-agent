//! Tracing layer for `target: "sampling_log"` → `~/.echo-agent/logs/sampling.jsonl`.
//! Enable with `--log-sampling` or `ECHO_AGENT_LOG_SAMPLING=1`.

use std::sync::Mutex;

use tracing::Subscriber;
use tracing_subscriber::fmt::writer::BoxMakeWriter;
use tracing_subscriber::layer::Layer;
use tracing_subscriber::registry::LookupSpan;

use echo_agent_config::echo_agent_home;

use crate::instrumentation::{NoOpLayer, TargetFilterLayer};

const ENV_VAR: &str = "ECHO_AGENT_LOG_SAMPLING";
const LOG_FILE: &str = "sampling.jsonl";
const TARGET: &str = "sampling_log";

static GUARD: std::sync::OnceLock<Mutex<Option<tracing_appender::non_blocking::WorkerGuard>>> =
    std::sync::OnceLock::new();

pub fn layer<S>() -> Box<dyn Layer<S> + Send + Sync>
where
    S: Subscriber + for<'span> LookupSpan<'span> + Send + Sync + 'static,
{
    if !std::env::var(ENV_VAR).is_ok_and(|v| matches!(v.as_str(), "1" | "true" | "on")) {
        return Box::new(NoOpLayer::new());
    }

    let path = echo_agent_home()
        .join(crate::unified_log::LOG_DIR)
        .join(LOG_FILE);

    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        tracing::warn!("failed to create sampling log dir: {e}");
        return Box::new(NoOpLayer::new());
    }

    if crate::unified_log::file_size(&path) >= crate::unified_log::MAX_SIZE {
        crate::unified_log::trim_file(&path);
    }

    let file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!("failed to open sampling log: {e}");
            return Box::new(NoOpLayer::new());
        }
    };

    let (non_blocking, guard) = tracing_appender::non_blocking(file);
    let guard_slot = GUARD.get_or_init(|| Mutex::new(None));
    if let Ok(mut slot) = guard_slot.lock() {
        *slot = Some(guard);
    }

    let fmt_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_current_span(false) // `spans` array already carries the full ancestor list
        .with_ansi(false)
        .with_timer(tracing_subscriber::fmt::time::UtcTime::rfc_3339())
        .with_target(false)
        .with_writer(BoxMakeWriter::new(non_blocking));

    Box::new(TargetFilterLayer::new(fmt_layer, TARGET))
}
