#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemorySearchSource {
    Tool,
    Injection,
    CompactionRecovery,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryRetrievalMode {
    FtsOnly,
    Hybrid,
    EmbeddingFallback,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemorySearchOutcome {
    Results,
    Empty,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemorySearchErrorClass {
    IndexOpen,
    Fts,
    Vector,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemorySearchObservation {
    pub source: MemorySearchSource,
    pub mode: MemoryRetrievalMode,
    pub outcome: MemorySearchOutcome,
    pub query_length: usize,
    pub keyword_count: usize,
    pub result_count: usize,
    pub top_score: f64,
    pub min_score_threshold: f64,
    pub duration_ms: u64,
    pub is_vector_available: bool,
    pub error_class: Option<MemorySearchErrorClass>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryWatcherSyncObservation {
    pub dirty_file_count: usize,
    pub is_claimed: bool,
    pub reindexed_count: usize,
    pub embedded_count: usize,
    pub duration_ms: u64,
}

pub trait MemoryObservationSink: Send + Sync {
    fn observe_search(&self, observation: MemorySearchObservation);
    fn observe_watcher_sync(&self, observation: MemoryWatcherSyncObservation);
}

impl MemoryObservationSink for () {
    fn observe_search(&self, _: MemorySearchObservation) {}
    fn observe_watcher_sync(&self, _: MemoryWatcherSyncObservation) {}
}

pub fn noop_memory_observation_sink() -> std::sync::Arc<dyn MemoryObservationSink> {
    std::sync::Arc::new(())
}
