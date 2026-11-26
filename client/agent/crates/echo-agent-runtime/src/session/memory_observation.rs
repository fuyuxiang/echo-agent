use echo_agent_memory::{
    MemoryObservationSink, MemoryRetrievalMode, MemorySearchErrorClass, MemorySearchObservation,
    MemorySearchSource, MemoryWatcherSyncObservation,
};
use echo_agent_telemetry::memory_telemetry::{
    MemoryInjection, MemoryInjectionOutcome, MemorySearch,
    MemorySearchErrorClass as TelemetryErrorClass, MemorySearchMode as TelemetryMode,
    MemorySearchOutcome as TelemetryOutcome, MemorySearchSource as TelemetrySource,
    MemoryWatcherSync,
};

pub(crate) struct TelemetryMemoryObservationSink {
    pub(crate) session_id: String,
}

#[derive(Default)]
pub(crate) struct MemoryInjectionMetrics {
    pub(crate) is_greeting_fallback: bool,
    pub(crate) result_count: usize,
    pub(crate) total_snippet_chars: usize,
    pub(crate) top_score: f64,
    pub(crate) configured_min_score: f64,
    pub(crate) duration_ms: u64,
}

pub(crate) fn log_memory_injection(
    session_id: String,
    outcome: MemoryInjectionOutcome,
    metrics: MemoryInjectionMetrics,
) {
    echo_agent_telemetry::session_ctx::log_event(MemoryInjection {
        session_id,
        outcome,
        was_greeting_fallback: metrics.is_greeting_fallback,
        result_count: metrics.result_count,
        total_snippet_chars: metrics.total_snippet_chars,
        top_score: metrics.top_score,
        configured_min_score: metrics.configured_min_score,
        injection_duration_ms: metrics.duration_ms,
    });
}

impl MemoryObservationSink for TelemetryMemoryObservationSink {
    fn observe_search(&self, observation: MemorySearchObservation) {
        echo_agent_telemetry::session_ctx::log_event(MemorySearch {
            session_id: self.session_id.clone(),
            source: match observation.source {
                MemorySearchSource::Tool => TelemetrySource::Tool,
                MemorySearchSource::Injection => TelemetrySource::Injection,
                MemorySearchSource::CompactionRecovery => TelemetrySource::CompactionRecovery,
            },
            mode: match observation.mode {
                MemoryRetrievalMode::FtsOnly => TelemetryMode::FtsOnly,
                MemoryRetrievalMode::Hybrid => TelemetryMode::Hybrid,
                MemoryRetrievalMode::EmbeddingFallback => TelemetryMode::EmbeddingFallback,
            },
            outcome: match observation.outcome {
                echo_agent_memory::MemorySearchOutcome::Results => TelemetryOutcome::Results,
                echo_agent_memory::MemorySearchOutcome::Empty => TelemetryOutcome::Empty,
                echo_agent_memory::MemorySearchOutcome::Error => TelemetryOutcome::Error,
            },
            query_length: observation.query_length,
            keyword_count: observation.keyword_count,
            result_count: observation.result_count,
            top_score: observation.top_score,
            min_score_threshold: observation.min_score_threshold,
            duration_ms: observation.duration_ms,
            vec_available: observation.is_vector_available,
            error_class: observation
                .error_class
                .map(|error_class| match error_class {
                    MemorySearchErrorClass::IndexOpen => TelemetryErrorClass::IndexOpen,
                    MemorySearchErrorClass::Fts => TelemetryErrorClass::Fts,
                    MemorySearchErrorClass::Vector => TelemetryErrorClass::Vector,
                }),
        });
    }

    fn observe_watcher_sync(&self, observation: MemoryWatcherSyncObservation) {
        echo_agent_telemetry::session_ctx::log_event(MemoryWatcherSync {
            session_id: self.session_id.clone(),
            dirty_file_count: observation.dirty_file_count,
            claimed: observation.is_claimed,
            reindexed_count: observation.reindexed_count,
            embedded_count: observation.embedded_count,
            duration_ms: observation.duration_ms,
        });
    }
}
