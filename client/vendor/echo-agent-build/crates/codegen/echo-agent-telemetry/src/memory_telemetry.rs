//! Memory subsystem telemetry. Routes through `log_event` (product tier,
//! `Enabled` mode only). No PII or user content -- only counts, scores,
//! durations, and config values.

use serde::Serialize;

#[derive(Debug, Default, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemorySearchSource {
    #[default]
    Tool,
    Injection,
    CompactionRecovery,
}

#[derive(Debug, Default, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemorySearchMode {
    #[default]
    FtsOnly,
    Hybrid,
    EmbeddingFallback,
}

#[derive(Debug, Default, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemorySearchOutcome {
    #[default]
    Results,
    Empty,
    Error,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemorySearchErrorClass {
    IndexOpen,
    Fts,
    Vector,
}

#[derive(Debug, Default, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryInjectionOutcome {
    #[default]
    Results,
    Empty,
    Error,
    Skipped,
}

#[derive(Default, Serialize)]
pub struct MemorySessionInit {
    pub session_id: String,
    pub memory_enabled: bool,
    pub watcher_config_enabled: bool,
    pub watcher_started: bool,
    pub temporal_decay_enabled: bool,
    pub mmr_enabled: bool,
    pub mmr_lambda: f64,
    pub half_life_days: f64,
    pub embedding_dimensions: usize,
    pub total_chunks: usize,
    pub total_files: usize,
    pub has_global_memory_md: bool,
    pub has_workspace_memory_md: bool,
}

#[derive(Default, Serialize)]
pub struct MemorySearch {
    pub session_id: String,
    pub source: MemorySearchSource,
    #[serde(rename = "search_mode")]
    pub mode: MemorySearchMode,
    pub outcome: MemorySearchOutcome,
    pub query_length: usize,
    pub keyword_count: usize,
    pub result_count: usize,
    pub top_score: f64,
    pub min_score_threshold: f64,
    pub duration_ms: u64,
    pub vec_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_class: Option<MemorySearchErrorClass>,
}

#[derive(Default, Serialize)]
pub struct MemoryFlushStart {
    pub session_id: String,
    pub trigger: String,
    pub conversation_len: usize,
    pub user_message_count: usize,
}

#[derive(Default, Serialize)]
pub struct MemoryFlushComplete {
    pub session_id: String,
    pub trigger: String,
    pub outcome: String,
    pub duration_ms: u64,
    pub response_length: usize,
    pub accepted_length: usize,
    pub was_truncated: bool,
}

#[derive(Default, Serialize)]
pub struct MemoryInjection {
    pub session_id: String,
    pub outcome: MemoryInjectionOutcome,
    pub was_greeting_fallback: bool,
    pub result_count: usize,
    pub total_snippet_chars: usize,
    pub top_score: f64,
    pub configured_min_score: f64,
    pub injection_duration_ms: u64,
}

#[derive(Default, Serialize)]
pub struct MemoryReindex {
    pub session_id: String,
    pub source: String,
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub embedded: usize,
    pub duration_ms: u64,
    pub trigger: String,
}

#[derive(Default, Serialize)]
pub struct MemoryWatcherSync {
    pub session_id: String,
    pub dirty_file_count: usize,
    pub claimed: bool,
    pub reindexed_count: usize,
    pub embedded_count: usize,
    pub duration_ms: u64,
}

#[derive(Default, Serialize)]
pub struct MemorySessionSummary {
    pub session_id: String,
    pub memory_enabled: bool,
    pub session_duration_secs: u64,
    pub flush_count: u64,
    pub flush_success_count: u64,
    pub flush_error_count: u64,
    pub tool_search_count: u64,
    pub injection_count: u64,
    pub recovery_search_count: u64,
    pub chunks_added_this_session: usize,
    pub session_end_result: String,
    pub dream_count: u64,
    pub dream_success_count: u64,
    pub dream_error_count: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_events_have_no_content_bearing_fields() {
        let search_event = MemorySearch {
            source: MemorySearchSource::CompactionRecovery,
            mode: MemorySearchMode::EmbeddingFallback,
            outcome: MemorySearchOutcome::Error,
            error_class: Some(MemorySearchErrorClass::Vector),
            ..Default::default()
        };
        let search = serde_json::to_value(search_event).unwrap();
        assert_eq!(search["source"], "compaction_recovery");
        assert_eq!(search["search_mode"], "embedding_fallback");
        assert_eq!(search["outcome"], "error");
        assert_eq!(
            serde_json::to_value(MemoryInjectionOutcome::Skipped).unwrap(),
            "skipped"
        );
        let events = [
            serde_json::to_value(MemorySessionInit::default()).unwrap(),
            search,
            serde_json::to_value(MemoryFlushStart::default()).unwrap(),
            serde_json::to_value(MemoryFlushComplete::default()).unwrap(),
            serde_json::to_value(MemoryInjection::default()).unwrap(),
            serde_json::to_value(MemoryReindex::default()).unwrap(),
            serde_json::to_value(MemoryWatcherSync::default()).unwrap(),
            serde_json::to_value(MemorySessionSummary::default()).unwrap(),
        ];
        const STRING_KEYS: &[&str] = &[
            "session_id",
            "source",
            "search_mode",
            "outcome",
            "error_class",
            "trigger",
            "session_end_result",
        ];
        for event in events {
            for (key, value) in event.as_object().unwrap() {
                let is_aggregate_field =
                    key.ends_with("_length") || key.ends_with("_count") || key.ends_with("_chars");
                let content_key = !is_aggregate_field
                    && ["query", "snippet", "path", "content", "message"]
                        .iter()
                        .any(|word| key.contains(word));
                assert!(
                    !value.is_object()
                        && !value.is_array()
                        && (!value.is_string() || STRING_KEYS.contains(&key.as_str()))
                        && !content_key,
                    "unsafe memory field {key}"
                );
            }
        }
    }
}
