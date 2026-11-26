//! The *how* of external emission: content-gate application, secret scrub +
//! truncation, ctx (`session.id`/`turn_number`/`prompt.id`/`event.sequence`)
//! injection, metric-increment conversion, and provider hand-off.
//!
//! The per-event *what* (field → attribute mapping) lives in
//! [`super::schema`], wired via the `telemetry_event!` macro's
//! `external = …` arm.

use opentelemetry::KeyValue;
use opentelemetry::logs::{AnyValue, LogRecord as _, Logger as _, Severity};
use opentelemetry::metrics::{Counter, Histogram, Meter};

use super::ExternalTelemetry;
use super::config::ContentGates;
use super::schema::{
    AttrValue, ExternalKey, ExternalRecord, Gate, METRIC_ERROR_COUNT, METRIC_SESSION_COUNT,
    METRIC_STARTUP_PHASE_DURATION, METRIC_STARTUP_TIMEOUT, METRIC_STARTUP_TOTAL,
    METRIC_TOKEN_USAGE, METRIC_TOOL_DECISION, METRIC_TOOL_USAGE, METRIC_TURN_COUNT,
    MetricIncrement,
};

/// Default OTel buckets end at 10s; startup's failure regime is 10-30s, so
/// those samples need real buckets, not +Inf.
const STARTUP_MS_BOUNDARIES: &[f64] = &[
    50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0, 5000.0, 10000.0, 15000.0, 30000.0, 60000.0, 120000.0,
];

/// Pre-created counters/histograms (schema pinned by test: names, units, attr keys).
pub(crate) struct Instruments {
    session_count: Counter<u64>,
    token_usage: Counter<u64>,
    turn_count: Counter<u64>,
    tool_decision: Counter<u64>,
    tool_usage: Counter<u64>,
    error_count: Counter<u64>,
    startup_timeout: Counter<u64>,
    startup_phase_duration: Histogram<u64>,
    startup_total: Histogram<u64>,
}

impl Instruments {
    pub(crate) fn new(meter: &Meter) -> Self {
        Self {
            session_count: meter
                .u64_counter(METRIC_SESSION_COUNT)
                .with_unit("{session}")
                .build(),
            token_usage: meter
                .u64_counter(METRIC_TOKEN_USAGE)
                .with_unit("{token}")
                .build(),
            turn_count: meter
                .u64_counter(METRIC_TURN_COUNT)
                .with_unit("{turn}")
                .build(),
            tool_decision: meter
                .u64_counter(METRIC_TOOL_DECISION)
                .with_unit("{decision}")
                .build(),
            tool_usage: meter
                .u64_counter(METRIC_TOOL_USAGE)
                .with_unit("{call}")
                .build(),
            error_count: meter
                .u64_counter(METRIC_ERROR_COUNT)
                .with_unit("{error}")
                .build(),
            startup_timeout: meter
                .u64_counter(METRIC_STARTUP_TIMEOUT)
                .with_unit("{timeout}")
                .build(),
            startup_phase_duration: meter
                .u64_histogram(METRIC_STARTUP_PHASE_DURATION)
                .with_unit("ms")
                .with_boundaries(STARTUP_MS_BOUNDARIES.to_vec())
                .build(),
            startup_total: meter
                .u64_histogram(METRIC_STARTUP_TOTAL)
                .with_unit("ms")
                .with_boundaries(STARTUP_MS_BOUNDARIES.to_vec())
                .build(),
        }
    }
}

fn gate_open(gates: ContentGates, gate: Gate) -> bool {
    match gate {
        Gate::UserPrompts => gates.log_user_prompts,
        Gate::ToolDetails => gates.log_tool_details,
    }
}

/// Scrub + truncate one string attribute value. Every string passes the
/// secret/path scrub; the prompt key gets the 60 KB content cap, everything
/// else the standard 512→128 value truncation. Defense-in-depth only — the
/// export-time validators in [`super::redact`] enforce the result.
fn scrub_string(key: ExternalKey, s: String) -> String {
    let scrubbed = crate::redact_common::redact_to_owned(&s);
    match key {
        ExternalKey::Prompt => super::truncate::truncate_content(&scrubbed).unwrap_or(scrubbed),
        _ => super::truncate::truncate_value_owned(scrubbed),
    }
}

fn to_any_value(v: AttrValue) -> AnyValue {
    match v {
        AttrValue::Str(s) => AnyValue::String(s.into()),
        AttrValue::I64(i) => AnyValue::Int(i),
        AttrValue::Bool(b) => AnyValue::Boolean(b),
    }
}

/// Convert one mapped [`ExternalRecord`] into a log record and metric
/// increments. Synchronous and cheap: the `BatchLogProcessor` queues the
/// record; no `tokio::spawn` (contrast with the product-events path).
pub(crate) fn emit_record(ext: &ExternalTelemetry, mut record: ExternalRecord) {
    let gates = *ext.gates.read();

    // Gated attributes: emitted only when the matching gate is on. A gated
    // value sharing a key with a default attr (verbatim vs. sanitized
    // `tool_name`) replaces the default.
    for gated in std::mem::take(&mut record.gated) {
        if !gate_open(gates, gated.gate) {
            continue;
        }
        if let Some(existing) = record.attrs.iter_mut().find(|(k, _)| *k == gated.key) {
            existing.1 = gated.value;
        } else {
            record.attrs.push((gated.key, gated.value));
        }
    }

    // Ambient ctx: a mapping-supplied `session.id` wins; the ctx is a
    // fallback for in-session events (the session-start sites are spawned
    // outside the ctx scope and carry their own ids).
    let ctx = crate::session_ctx::external_ctx_snapshot();
    let mapped_session_id = record
        .attrs
        .iter()
        .find(|(k, _)| *k == ExternalKey::SessionId)
        .and_then(|(_, v)| match v {
            AttrValue::Str(s) => Some(s.clone()),
            _ => None,
        });
    let session_id = mapped_session_id.or_else(|| ctx.as_ref().map(|c| c.session_id.clone()));

    for (key, value) in record.attrs.iter_mut() {
        if let AttrValue::Str(s) = value {
            *value = AttrValue::Str(scrub_string(*key, std::mem::take(s)));
        }
    }

    let identity = ext.identity.read().clone();

    if let (Some(event), Some(logger)) = (record.event, ext.logger.as_ref()) {
        let mut log_record = logger.create_log_record();
        log_record.set_event_name(event.as_str());
        log_record.set_severity_number(Severity::Info);
        let now = std::time::SystemTime::now();
        log_record.set_timestamp(now);
        log_record.set_observed_timestamp(now);
        log_record.add_attribute(
            ExternalKey::EventSequence.as_str(),
            ext.next_sequence() as i64,
        );
        if record
            .attrs
            .iter()
            .all(|(k, _)| *k != ExternalKey::SessionId)
            && let Some(sid) = session_id.as_deref()
        {
            log_record.add_attribute(ExternalKey::SessionId.as_str(), sid.to_owned());
        }
        if let Some(ctx) = ctx.as_ref() {
            if let Some(turn) = ctx.turn_number {
                log_record.add_attribute(ExternalKey::TurnNumber.as_str(), turn as i64);
            }
            // prompt.id: events only, never metrics (unbounded cardinality).
            if let Some(prompt_id) = ctx.prompt_id.as_deref() {
                log_record.add_attribute(ExternalKey::PromptId.as_str(), prompt_id.to_owned());
            }
        }
        for (key, value) in &record.attrs {
            log_record.add_attribute(key.as_str(), to_any_value(value.clone()));
        }
        for (key, value) in [
            (ExternalKey::UserId, identity.user_id.as_deref()),
            (
                ExternalKey::OrganizationId,
                identity.organization_id.as_deref(),
            ),
            (ExternalKey::TeamId, identity.team_id.as_deref()),
            (ExternalKey::DeploymentId, identity.deployment_id.as_deref()),
        ] {
            if let Some(v) = value.filter(|v| !v.is_empty()) {
                log_record.add_attribute(key.as_str(), v.to_owned());
            }
        }
        logger.emit(log_record);
    }

    if let Some(instruments) = ext.instruments.as_ref() {
        for increment in record.metrics {
            add_increment(
                ext,
                instruments,
                increment,
                session_id.as_deref(),
                &identity,
            );
        }
    }
}

fn add_increment(
    ext: &ExternalTelemetry,
    instruments: &Instruments,
    increment: MetricIncrement,
    session_id: Option<&str>,
    identity: &super::IdentityAttrs,
) {
    // Identity/cardinality attrs shared by every instrument. `prompt.id` is
    // deliberately never attached to metrics.
    let mut attrs: Vec<KeyValue> = Vec::with_capacity(8);
    if ext.include_session_id_on_metrics
        && let Some(sid) = session_id.filter(|s| !s.is_empty())
    {
        attrs.push(KeyValue::new("session.id", sid.to_owned()));
    }
    if ext.include_version_on_metrics && !ext.app_version.is_empty() {
        attrs.push(KeyValue::new("app.version", ext.app_version.clone()));
    }
    for (key, value) in [
        ("user.id", identity.user_id.as_deref()),
        ("organization.id", identity.organization_id.as_deref()),
        ("team.id", identity.team_id.as_deref()),
        ("deployment.id", identity.deployment_id.as_deref()),
    ] {
        if let Some(v) = value.filter(|v| !v.is_empty()) {
            attrs.push(KeyValue::new(key, v.to_owned()));
        }
    }

    // `model` is the one non-enum metric attribute value: scrub it at
    // increment time (call-site discipline is never the guarantee on its own
    // — the PR 6 collector fixture pins this with a wire-payload canary).
    let scrub = |s: &str| crate::redact_common::redact_to_owned(s);

    match increment {
        MetricIncrement::SessionCount => {
            instruments.session_count.add(1, &attrs);
        }
        MetricIncrement::TokenUsage {
            token_type,
            model,
            count,
        } => {
            attrs.push(KeyValue::new("type", token_type));
            attrs.push(KeyValue::new("model", scrub(&model)));
            instruments.token_usage.add(count, &attrs);
        }
        MetricIncrement::TurnCount { outcome, model } => {
            attrs.push(KeyValue::new("outcome", outcome));
            attrs.push(KeyValue::new("model", scrub(&model)));
            instruments.turn_count.add(1, &attrs);
        }
        MetricIncrement::ToolDecision {
            tool_name,
            decision,
            access_kind,
            permission_mode,
        } => {
            attrs.push(KeyValue::new("tool_name", scrub(&tool_name)));
            attrs.push(KeyValue::new("decision", decision));
            attrs.push(KeyValue::new("access_kind", access_kind));
            attrs.push(KeyValue::new("permission_mode", permission_mode));
            instruments.tool_decision.add(1, &attrs);
        }
        MetricIncrement::ToolUsage { tool_name, outcome } => {
            attrs.push(KeyValue::new("tool_name", scrub(&tool_name)));
            attrs.push(KeyValue::new("outcome", outcome));
            instruments.tool_usage.add(1, &attrs);
        }
        MetricIncrement::ErrorCount {
            error_category,
            model,
        } => {
            attrs.push(KeyValue::new("error_category", scrub(&error_category)));
            attrs.push(KeyValue::new("model", scrub(&model)));
            instruments.error_count.add(1, &attrs);
        }
        MetricIncrement::StartupTimeout {
            stuck_in,
            auth_mode,
        } => {
            attrs.push(KeyValue::new("stuck_in", scrub(&stuck_in)));
            attrs.push(KeyValue::new("auth_mode", auth_mode));
            instruments.startup_timeout.add(1, &attrs);
        }
        MetricIncrement::StartupPhaseDuration {
            phase,
            duration_ms,
            outcome,
            auth_mode,
        } => {
            attrs.push(KeyValue::new("phase", scrub(&phase)));
            attrs.push(KeyValue::new("outcome", outcome));
            attrs.push(KeyValue::new("auth_mode", auth_mode));
            instruments
                .startup_phase_duration
                .record(duration_ms, &attrs);
        }
        MetricIncrement::StartupTotal {
            duration_ms,
            outcome,
            auth_mode,
        } => {
            attrs.push(KeyValue::new("outcome", outcome));
            attrs.push(KeyValue::new("auth_mode", auth_mode));
            instruments.startup_total.record(duration_ms, &attrs);
        }
    }
}
