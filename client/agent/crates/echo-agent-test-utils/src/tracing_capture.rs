//! Test-only tracing capture: count events whose `message` starts with a
//! known prefix.
//!
//! Producers should export the exact log-line prefixes as `pub const`s next
//! to the `tracing::debug!` call sites (e.g. `echo_agent_hunk_tracker`'s
//! `REFRESH_SCAN_LOG_PREFIX`) so tests never duplicate the strings.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Extracts the formatted `message` field of one event.
#[derive(Default)]
struct MessageVisitor(String);

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

/// A `tracing_subscriber::Layer` counting, per registered prefix, the events
/// whose `message` starts with it. Clones share the counts.
#[derive(Clone)]
pub struct MessagePrefixCounter {
    counters: Arc<Vec<(&'static str, AtomicUsize)>>,
}

impl MessagePrefixCounter {
    pub fn new(prefixes: &[&'static str]) -> Self {
        Self {
            counters: Arc::new(prefixes.iter().map(|p| (*p, AtomicUsize::new(0))).collect()),
        }
    }

    /// Events counted so far for `prefix`. Panics on a prefix that was never
    /// registered — that is a bug in the test, not a zero count.
    pub fn count(&self, prefix: &str) -> usize {
        self.counters
            .iter()
            .find(|(p, _)| *p == prefix)
            .unwrap_or_else(|| panic!("prefix not registered with this counter: {prefix:?}"))
            .1
            .load(Ordering::Relaxed)
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for MessagePrefixCounter {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        for (prefix, count) in self.counters.iter() {
            if visitor.0.starts_with(prefix) {
                count.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// Install a **thread-scoped** default subscriber counting `prefixes`; hold
/// the guard for the test's lifetime. Only observes events emitted on the
/// current thread — tasks under test must run on a current-thread runtime.
pub fn install_prefix_counter_thread(
    prefixes: &[&'static str],
) -> (tracing::subscriber::DefaultGuard, MessagePrefixCounter) {
    use tracing_subscriber::layer::SubscriberExt as _;
    let counter = MessagePrefixCounter::new(prefixes);
    let subscriber = tracing_subscriber::registry().with(counter.clone());
    (tracing::subscriber::set_default(subscriber), counter)
}

/// Install the **process-global** subscriber counting `prefixes` — for tests
/// whose subject spawns its own threads/runtimes. Panics if a global
/// subscriber already exists: the test binary must own it.
///
/// `stderr_env_filter` additionally tees formatted logs matching the given
/// `EnvFilter` directive to stderr (local debugging).
pub fn install_prefix_counter_global(
    prefixes: &[&'static str],
    stderr_env_filter: Option<&str>,
) -> MessagePrefixCounter {
    use tracing_subscriber::layer::{Layer as _, SubscriberExt as _};
    use tracing_subscriber::util::SubscriberInitExt as _;
    let counter = MessagePrefixCounter::new(prefixes);
    let fmt = stderr_env_filter.map(|filter| {
        tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .with_filter(tracing_subscriber::EnvFilter::new(filter))
    });
    tracing_subscriber::registry()
        .with(counter.clone())
        .with(fmt)
        .try_init()
        .expect("this test binary must own the global subscriber");
    counter
}
