//! Per-request classification state and auto-denial limits used by the
//! permission-manager actor. Extracted so `manager/mod.rs` keeps the actor
//! loop / `PermissionHandle` surface without absorbing every supporting type.

use crate::permission::auto_mode::{
    BashSecurityAssessment, ClassifierSourceKind, ClassifierVerdict,
};

pub const AUTO_DENY_CONSECUTIVE_LIMIT: u32 = 3;
pub const AUTO_DENY_TOTAL_LIMIT: u32 = 20;

pub(super) const AUTO_DENY_GUIDANCE: &str = "Take a safer approach that stays within what the user asked \
     for; do not retry this exact action or attempt to work around the denial. If no safer \
     alternative exists, ask the user how to proceed.";

/// Consecutive/total auto-denial counters snapshotted for one decision. Kept in
/// its own `Cell` so the finalizer reads the value intended for the event even
/// after the running counters are reset later in the arm.
#[derive(Clone, Copy)]
pub(super) struct DenialCounters {
    pub(super) consecutive: u32,
    pub(super) total: u32,
}

/// Classifier provenance for one request, including the manager-only `NotWired`
/// (Auto classifier route entered but no classifier installed) that the
/// auto_mode [`ClassifierSource`] cannot express. This never reports `heuristic`
/// for a request no classifier actually judged.
#[derive(Clone, Copy)]
pub(super) enum ClassificationSource {
    /// A classifier ran and produced this provenance (llm/heuristic/timeout/…).
    Classifier(crate::permission::auto_mode::ClassifierSource),
    /// Auto route entered but `set_classifier(None)` left no classifier installed.
    NotWired,
}

impl ClassificationSource {
    /// Project to the full-vocabulary owner kind (exhaustive bridge).
    pub(super) const fn kind(self) -> ClassifierSourceKind {
        match self {
            Self::Classifier(source) => source.kind(),
            Self::NotWired => ClassifierSourceKind::NotWired,
        }
    }
}

/// The completed outcome of one classifier route: typed verdict, provenance, and
/// latency. `latency_ms` is `None` for the not-wired case (no classifier ran).
#[derive(Clone, Copy)]
pub(super) struct ClassificationOutcome {
    pub(super) verdict: ClassifierVerdict,
    pub(super) source: ClassificationSource,
    pub(super) latency_ms: Option<u64>,
}

/// The single canonical per-request classification state. Replaces the previous
/// split between a `ClassificationRecord` (assessment + string verdict) and a
/// separate classifier telemetry snapshot (source + latency): one value owns
/// route/attempt, the frozen assessment, and the typed outcome, and the event is
/// projected from it once. This makes impossible combinations unrepresentable.
#[derive(Default)]
pub(super) enum RequestClassification {
    /// The request never entered the Auto classifier route (fast path skipped,
    /// non-Bash, or Auto disabled). Event `security_findings`/verdict/source stay
    /// `None`.
    #[default]
    NotClassified,
    /// Auto fast-path allow: no side query and no assessment. Event reports
    /// `classifier_source = fast_path` with no findings/verdict.
    FastPath,
    /// Entered the classifier route with this frozen assessment (the exact set
    /// handed to the classifier). `outcome` is `None` only when the side query was
    /// abandoned (requester gone mid-classify), so findings survive but there is
    /// no verdict/source/latency.
    Classified {
        assessment: BashSecurityAssessment,
        outcome: Option<ClassificationOutcome>,
    },
}

impl RequestClassification {
    /// Owner-kind `classifier_source`: `FastPath` for the fast path, the outcome
    /// provenance for a completed route, `None` for not-classified or abandoned.
    /// The finalizer emits `.wire_str()`, so the typed owner projection is on the
    /// production path (no raw string literals).
    pub(super) fn classifier_source(&self) -> Option<ClassifierSourceKind> {
        match self {
            Self::NotClassified => None,
            Self::FastPath => Some(ClassifierSourceKind::FastPath),
            Self::Classified { outcome, .. } => outcome.as_ref().map(|o| o.source.kind()),
        }
    }

    /// Typed verdict, only when a classifier produced one.
    pub(super) fn classifier_verdict(&self) -> Option<ClassifierVerdict> {
        match self {
            Self::Classified {
                outcome: Some(o), ..
            } => Some(o.verdict),
            _ => None,
        }
    }

    /// Classifier latency, only for a completed route that recorded one.
    pub(super) fn classifier_latency_ms(&self) -> Option<u64> {
        match self {
            Self::Classified {
                outcome: Some(o), ..
            } => o.latency_ms,
            _ => None,
        }
    }

    /// Ordered finding tokens when the classifier route was entered (`Some([])`
    /// for an empty attempted assessment); `None` when never classified.
    pub(super) fn security_findings_tokens(&self) -> Option<Vec<String>> {
        match self {
            Self::Classified { assessment, .. } => Some(assessment.tokens()),
            _ => None,
        }
    }
}

/// Canonical permission-mode string for the uploaded artifact. Matches
/// `config.ui.permission_mode` (hyphenated) for trace-internal consistency,
/// deliberately diverging from the telemetry enum's underscore product-analytics serde.
pub(super) fn permission_mode_artifact_str(
    mode: echo_agent_telemetry::enums::PermissionMode,
) -> &'static str {
    use echo_agent_telemetry::enums::PermissionMode;
    match mode {
        PermissionMode::AlwaysApprove => "always-approve",
        PermissionMode::Auto => "auto",
        PermissionMode::Ask => "ask",
    }
}
