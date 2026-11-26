//! Tests for [`crate::circuit_breaker_observer::TracingObserver`].
//!
//! Snapshot the exact `tracing::Event` field set for each transition
//! so a future rename or field-set change can't break analytics queries
//! silently.

use super::*;
use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;
use tracing::Level;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

#[derive(Debug, Clone)]
struct CapturedEvent {
    level: Level,
    target: String,
    message: String,
    breaker: Option<String>,
    /// All non-message field names present on the event. Pins the
    /// structured field set that analytics queries consume.
    field_names: BTreeSet<String>,
    /// All non-message field values rendered as strings. Lets a test
    /// assert e.g. `reason=="trip"` without re-emitting the event.
    field_values: HashMap<String, String>,
}

#[derive(Default)]
struct CapturingLayer {
    events: Arc<Mutex<Vec<CapturedEvent>>>,
}

struct CapturingVisitor {
    message: Option<String>,
    breaker: Option<String>,
    field_names: BTreeSet<String>,
    field_values: HashMap<String, String>,
}

impl CapturingVisitor {
    fn new() -> Self {
        Self {
            message: None,
            breaker: None,
            field_names: BTreeSet::new(),
            field_values: HashMap::new(),
        }
    }
}

impl Visit for CapturingVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let name = field.name();
        let rendered = format!("{value:?}");
        let stripped = rendered.trim_matches('"').to_string();
        if name == "message" {
            self.message = Some(stripped);
            return;
        }
        self.field_names.insert(name.to_string());
        self.field_values.insert(name.to_string(), stripped.clone());
        if name == "breaker" {
            self.breaker = Some(stripped);
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        let name = field.name();
        if name == "message" {
            self.message = Some(value.to_string());
            return;
        }
        self.field_names.insert(name.to_string());
        self.field_values
            .insert(name.to_string(), value.to_string());
        if name == "breaker" {
            self.breaker = Some(value.to_string());
        }
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        let name = field.name();
        self.field_names.insert(name.to_string());
        self.field_values
            .insert(name.to_string(), value.to_string());
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        let name = field.name();
        self.field_names.insert(name.to_string());
        self.field_values
            .insert(name.to_string(), value.to_string());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        let name = field.name();
        self.field_names.insert(name.to_string());
        self.field_values
            .insert(name.to_string(), value.to_string());
    }
}

impl<S: tracing::Subscriber> Layer<S> for CapturingLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = CapturingVisitor::new();
        event.record(&mut visitor);
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(CapturedEvent {
                level: *event.metadata().level(),
                target: event.metadata().target().to_string(),
                message: visitor.message.unwrap_or_default(),
                breaker: visitor.breaker,
                field_names: visitor.field_names,
                field_values: visitor.field_values,
            });
    }
}

fn run_with_capture<F: FnOnce()>(f: F) -> Vec<CapturedEvent> {
    let events: Arc<Mutex<Vec<CapturedEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let layer = CapturingLayer {
        events: events.clone(),
    };
    let subscriber = Registry::default().with(layer.with_filter(
        tracing_subscriber::filter::Targets::new().with_target(
            "circuit_breaker",
            tracing::level_filters::LevelFilter::TRACE,
        ),
    ));
    tracing::subscriber::with_default(subscriber, f);
    let guard = events.lock().unwrap_or_else(|e| e.into_inner());
    guard.clone()
}

#[test]
fn closed_to_open_emits_warn_opened() {
    let events = run_with_capture(|| {
        let obs = TracingObserver::new("storage_breaker");
        obs.on_state_change(BreakerState::Closed, BreakerState::Open, "trip");
    });
    let openings: Vec<_> = events
        .iter()
        .filter(|e| e.message == "circuit breaker opened")
        .collect();
    assert_eq!(openings.len(), 1);
    assert_eq!(openings[0].level, Level::WARN);
    assert_eq!(openings[0].target, "circuit_breaker");
    assert_eq!(openings[0].breaker.as_deref(), Some("storage_breaker"));
}

#[test]
fn open_to_half_open_emits_debug_half_open_not_info_closed() {
    let events = run_with_capture(|| {
        let obs = TracingObserver::new("storage_breaker");
        obs.on_state_change(BreakerState::Open, BreakerState::HalfOpen, "open_elapsed");
    });
    let half_open: Vec<_> = events
        .iter()
        .filter(|e| e.message == "circuit breaker half-open")
        .collect();
    let closed: Vec<_> = events
        .iter()
        .filter(|e| e.message == "circuit breaker closed")
        .collect();
    assert_eq!(
        half_open.len(),
        1,
        "Open -> HalfOpen must emit debug 'half-open'"
    );
    assert_eq!(half_open[0].level, Level::DEBUG);
    assert!(closed.is_empty(), "Open -> HalfOpen must NOT emit 'closed'");
}

#[test]
fn half_open_to_closed_emits_info_closed() {
    let events = run_with_capture(|| {
        let obs = TracingObserver::new("storage_breaker");
        obs.on_state_change(
            BreakerState::HalfOpen,
            BreakerState::Closed,
            "probe_success",
        );
    });
    let closed: Vec<_> = events
        .iter()
        .filter(|e| e.message == "circuit breaker closed")
        .collect();
    assert_eq!(
        closed.len(),
        1,
        "HalfOpen -> Closed must emit info 'closed'"
    );
    assert_eq!(closed[0].level, Level::INFO);
    assert_eq!(closed[0].target, "circuit_breaker");
    assert_eq!(closed[0].breaker.as_deref(), Some("storage_breaker"));
}

#[test]
fn half_open_to_open_on_probe_failure_emits_warn_opened() {
    let events = run_with_capture(|| {
        let obs = TracingObserver::new("storage_breaker");
        obs.on_state_change(BreakerState::HalfOpen, BreakerState::Open, "probe_failure");
    });
    let openings: Vec<_> = events
        .iter()
        .filter(|e| e.message == "circuit breaker opened")
        .collect();
    assert_eq!(openings.len(), 1);
    assert_eq!(openings[0].level, Level::WARN);
}

#[test]
fn on_outcome_failure_emits_trace_event() {
    let events = run_with_capture(|| {
        let obs = TracingObserver::new("storage_breaker");
        obs.on_outcome(Outcome::Failure, BreakerState::Closed);
    });
    let failures: Vec<_> = events
        .iter()
        .filter(|e| e.message == "circuit breaker outcome failure")
        .collect();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].level, Level::TRACE);
    assert_eq!(failures[0].breaker.as_deref(), Some("storage_breaker"));
}

#[test]
fn on_outcome_success_is_silent() {
    let events = run_with_capture(|| {
        let obs = TracingObserver::new("storage_breaker");
        obs.on_outcome(Outcome::Success, BreakerState::Closed);
    });
    assert!(events.is_empty(), "successes must not emit events");
}

/// Snapshot the full structured field set on a Closed->Open transition.
/// Analytics queries depend on `target=circuit_breaker` + the named
/// fields `{breaker, old, new, reason}`. A rename of `?old` -> `?prev`
/// (or a dropped `reason` field) at the emit site must fail this test
/// before silently breaking analytics dashboards.
#[test]
fn opened_event_field_set_is_stable() {
    let events = run_with_capture(|| {
        let obs = TracingObserver::new("storage_breaker");
        obs.on_state_change(BreakerState::Closed, BreakerState::Open, "trip");
    });
    let evt = events
        .iter()
        .find(|e| e.message == "circuit breaker opened")
        .expect("must emit one 'opened' event");
    assert_eq!(evt.level, Level::WARN);
    assert_eq!(evt.target, "circuit_breaker");
    assert_eq!(evt.breaker.as_deref(), Some("storage_breaker"));

    // Pin the exact field-name set analytics queries consume.
    let expected: BTreeSet<String> = ["breaker", "old", "new", "reason"]
        .into_iter()
        .map(String::from)
        .collect();
    assert_eq!(
        evt.field_names, expected,
        "opened event field-name set drifted: {:?}",
        evt.field_names
    );

    // Pin the concrete reason string so a typo in `"trip"` fails here
    // rather than in analytics.
    assert_eq!(
        evt.field_values.get("reason").map(String::as_str),
        Some("trip")
    );
    // Old/new render as `Debug` of `BreakerState`.
    assert_eq!(
        evt.field_values.get("old").map(String::as_str),
        Some("Closed")
    );
    assert_eq!(
        evt.field_values.get("new").map(String::as_str),
        Some("Open")
    );
}
