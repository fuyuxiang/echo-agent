//! Memory-system configuration value types, extracted from echo-agent-runtime
//! (config dependency inversion).
//!
//! These are the raw optional settings and resolved leaf value types for
//! `[memory.*]` and the memory-owned `[compaction.*]` tables.

use serde::{Deserialize, Serialize};

/// Raw `[memory]` settings. Absence is preserved for per-field fallback.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemorySettings {
    pub enabled: Option<bool>,
    pub index: Option<MemoryIndexSettings>,
    pub embedding: Option<MemoryEmbeddingSettings>,
    pub search: Option<MemorySearchSettings>,
    pub initial_injection: Option<MemoryInitialInjectionSettings>,
    pub session: Option<MemorySessionSettings>,
    pub watcher: Option<MemoryWatcherSettings>,
    pub gc: Option<MemoryGcSettings>,
    pub dream: Option<MemoryDreamSettings>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryIndexSettings {
    pub max_chunk_chars: Option<usize>,
    pub chunk_overlap_chars: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryEmbeddingSettings {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub dimensions: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemorySearchSettings {
    pub max_results: Option<usize>,
    pub min_score: Option<f32>,
    pub vector_weight: Option<f32>,
    pub text_weight: Option<f32>,
    pub recency_decay: Option<f32>,
    pub temporal_decay: Option<TemporalDecaySettings>,
    pub mmr: Option<MmrSettings>,
    pub source_weights: Option<std::collections::HashMap<String, f32>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TemporalDecaySettings {
    pub enabled: Option<bool>,
    pub half_life_days: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MmrSettings {
    pub enabled: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_clamped_unit_option")]
    pub lambda: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryInitialInjectionSettings {
    pub enabled: Option<bool>,
    pub min_score: Option<f32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemorySessionSettings {
    pub save_on_end: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryWatcherSettings {
    pub enabled: Option<bool>,
    pub stale_claim_secs: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryGcSettings {
    pub max_age_days: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryDreamSettings {
    pub enabled: Option<bool>,
    pub min_hours: Option<u64>,
    pub min_sessions: Option<u64>,
    pub stale_lock_secs: Option<u64>,
    pub check_interval_secs: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryFlushSettings {
    pub enabled: Option<bool>,
    pub soft_threshold_tokens: Option<u64>,
    pub flush_model: Option<String>,
    pub max_flush_write_chars: Option<usize>,
    pub idle_timeout_secs: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_clamped_unit_option")]
    pub semantic_dedup_threshold: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PruningSettings {
    pub enabled: Option<bool>,
    pub keep_last_n_turns: Option<usize>,
    pub soft_trim_threshold: Option<usize>,
    pub soft_trim_head: Option<usize>,
    pub soft_trim_tail: Option<usize>,
    pub hard_clear_age_turns: Option<usize>,
}

/// Index and chunking configuration (`[memory.index]`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct MemoryIndexConfig {
    /// Maximum chunk size in characters (approx tokens × 4).
    pub max_chunk_chars: usize,
    /// Character overlap between consecutive chunks.
    pub chunk_overlap_chars: usize,
}

impl Default for MemoryIndexConfig {
    fn default() -> Self {
        Self {
            max_chunk_chars: 1600,
            chunk_overlap_chars: 64,
        }
    }
}

/// Embedding provider configuration (`[memory.embedding]`).
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct MemoryEmbeddingConfig {
    /// Provider type: `"api"`, `"auto"` or bundled on-device `"local"`.
    /// Missing local model files degrade to FTS without using the network.
    pub provider: String,
    /// Model name for the embedding API. `None` disables vector embeddings.
    pub model: Option<String>,
    /// Embedding vector dimensions.
    pub dimensions: usize,
    /// Host-injected embedding endpoint. This is runtime-only so credentials
    /// and provider routing cannot be overridden through upstream config.
    #[serde(skip)]
    pub endpoint: Option<String>,
    /// Host-injected API key for the embedding endpoint.
    #[serde(skip)]
    pub api_key: Option<String>,
    /// Whether the provider accepts OpenAI's optional `dimensions` request
    /// field. Fixed-dimension providers can require this to be omitted.
    #[serde(skip)]
    pub send_dimensions: bool,
}

impl std::fmt::Debug for MemoryEmbeddingConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MemoryEmbeddingConfig")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("dimensions", &self.dimensions)
            .field("endpoint", &self.endpoint)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("send_dimensions", &self.send_dimensions)
            .finish()
    }
}

impl Default for MemoryEmbeddingConfig {
    fn default() -> Self {
        Self {
            provider: "api".to_string(),
            model: None,
            dimensions: 1024,
            endpoint: None,
            api_key: None,
            send_dimensions: true,
        }
    }
}

/// Optional API-backed relevance re-ranker applied after coarse memory
/// retrieval. Hosts inject this at runtime; it is intentionally not part of
/// the upstream TOML surface.
#[derive(Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct MemoryRerankerConfig {
    pub enabled: bool,
    #[serde(skip)]
    pub endpoint: Option<String>,
    #[serde(skip)]
    pub model: Option<String>,
    #[serde(skip)]
    pub api_key: Option<String>,
}

impl MemoryRerankerConfig {
    pub fn is_configured(&self) -> bool {
        self.enabled
            && self
                .endpoint
                .as_ref()
                .is_some_and(|value| !value.is_empty())
            && self.model.as_ref().is_some_and(|value| !value.is_empty())
    }
}

impl std::fmt::Debug for MemoryRerankerConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MemoryRerankerConfig")
            .field("enabled", &self.enabled)
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

/// Hybrid search scoring configuration (`[memory.search]`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct MemorySearchConfig {
    /// Maximum number of search results to return.
    pub max_results: usize,
    /// Minimum score threshold for inclusion.
    pub min_score: f32,
    /// Weight for vector similarity in hybrid scoring.
    pub vector_weight: f32,
    /// Weight for BM25 text similarity in hybrid scoring.
    pub text_weight: f32,
    /// **Deprecated** — use `temporal_decay` instead.
    ///
    /// Per-day decay factor for recency boosting (0.0–1.0).
    /// When `temporal_decay.enabled` is true, this field is ignored.
    /// When `temporal_decay.enabled` is false and this is set, it is
    /// converted to an approximate half-life for backward compatibility:
    /// `half_life ≈ -1 / log₂(recency_decay)`.
    pub recency_decay: f32,
    /// Temporal decay configuration for time-aware scoring.
    pub temporal_decay: TemporalDecayConfig,
    /// MMR diversity re-ranking configuration (enabled by default).
    pub mmr: MmrConfig,
    /// Optional host-provided model re-ranker. When configured, it replaces
    /// MMR as the final ordering stage.
    #[serde(skip)]
    pub reranker: MemoryRerankerConfig,
    /// Source-type weight multipliers: all default to 1.0.
    pub source_weights: std::collections::HashMap<String, f32>,
}

impl Default for MemorySearchConfig {
    fn default() -> Self {
        let mut source_weights = std::collections::HashMap::new();
        source_weights.insert("workspace".to_string(), 1.0);
        source_weights.insert("session".to_string(), 1.0);
        source_weights.insert("global".to_string(), 1.0);

        Self {
            max_results: 6,
            min_score: 0.45,
            vector_weight: 0.7,
            text_weight: 0.3,
            recency_decay: DEFAULT_RECENCY_DECAY,
            temporal_decay: TemporalDecayConfig::default(),
            mmr: MmrConfig::default(),
            reranker: MemoryRerankerConfig::default(),
            source_weights,
        }
    }
}

/// Temporal decay configuration for time-aware search scoring.
///
/// Controls how memory chunk scores decay over time. Chunks from
/// "evergreen" sources (`global`, `workspace`) are exempt from decay
/// since they contain curated long-term knowledge. Only `session`
/// chunks decay, using an exponential half-life formula:
///
/// ```text
/// decayed_score = base_score × e^(-λ × age_days)
/// where λ = ln(2) / half_life_days
/// ```
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct TemporalDecayConfig {
    /// Whether temporal decay is enabled.
    pub enabled: bool,
    /// Number of days after which a session chunk's score is halved.
    pub half_life_days: f64,
}

impl Default for TemporalDecayConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            half_life_days: 30.0,
        }
    }
}

/// MMR (Maximal Marginal Relevance) diversity re-ranking configuration.
///
/// When enabled, re-ranks search results to penalize redundancy. Uses
/// Jaccard similarity on tokenized snippets to measure inter-result
/// similarity, then greedily selects results that balance relevance
/// with diversity:
///
/// ```text
/// MMR(d) = λ × relevance(d) - (1-λ) × max_similarity(d, selected)
/// ```
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct MmrConfig {
    /// Whether MMR re-ranking is enabled. Default: true.
    pub enabled: bool,
    /// Trade-off between relevance and diversity.
    /// 0.0 = maximum diversity, 1.0 = pure relevance (no re-ranking).
    /// Clamped to [0.0, 1.0] at parse time. Default: 0.7.
    #[serde(deserialize_with = "deserialize_clamped_unit")]
    pub lambda: f64,
}

impl Default for MmrConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            lambda: 0.7,
        }
    }
}

/// Deserialize an `f64` clamped to [0.0, 1.0].
///
/// Used for fields where values outside the unit interval are meaningless
/// (e.g. cosine similarity thresholds, trade-off lambdas).
fn deserialize_clamped_unit<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = f64::deserialize(deserializer)?;
    Ok(v.clamp(0.0, 1.0))
}

/// Like [`deserialize_clamped_unit`] but for `Option<f64>` fields.
fn deserialize_clamped_unit_option<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v: Option<f64> = Option::deserialize(deserializer)?;
    Ok(v.map(|x| x.clamp(0.0, 1.0)))
}

/// Default value for the legacy `recency_decay` field.
pub const DEFAULT_RECENCY_DECAY: f32 = 0.95;

impl MemorySearchConfig {
    /// Resolve the effective half-life for temporal decay.
    ///
    /// Priority order:
    /// 1. `temporal_decay.enabled = true` → use `temporal_decay.half_life_days`
    /// 2. `temporal_decay.enabled = false` AND `recency_decay` differs from
    ///    the default (0.95) → convert the legacy per-day factor to an
    ///    approximate half-life: `half_life ≈ -1.0 / log₂(recency_decay)`.
    ///    This preserves behavior for users who only set `recency_decay`.
    /// 3. Otherwise → `None` (decay fully disabled).
    pub fn effective_half_life_days(&self) -> Option<f64> {
        if self.temporal_decay.enabled {
            if self.temporal_decay.half_life_days <= 0.0 {
                tracing::warn!(
                    half_life_days = self.temporal_decay.half_life_days,
                    "temporal_decay.half_life_days must be positive, disabling decay"
                );
                return None;
            }
            return Some(self.temporal_decay.half_life_days);
        }

        // Legacy backward compat: if the user explicitly set recency_decay
        // to a non-default value, convert it to an approximate half-life.
        if (self.recency_decay - DEFAULT_RECENCY_DECAY).abs() > f32::EPSILON
            && self.recency_decay > 0.0
            && self.recency_decay < 1.0
        {
            let half_life = -1.0 / (self.recency_decay as f64).log2();
            tracing::info!(
                recency_decay = self.recency_decay,
                converted_half_life_days = half_life,
                "converting legacy recency_decay to temporal decay half-life; \
                 consider migrating to [memory.search.temporal_decay]"
            );
            return Some(half_life);
        }

        None
    }
}

/// First-turn memory injection configuration (`[memory.initial_injection]`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct MemoryInitialInjectionConfig {
    /// Whether to search memory and inject a reminder on the first turn.
    pub enabled: bool,
    /// Optional score threshold override for first-turn injection.
    /// When `None`, the first-turn search uses the historical default of `0.0`
    /// (no threshold filtering).
    pub min_score: Option<f32>,
}

impl Default for MemoryInitialInjectionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_score: Some(0.45),
        }
    }
}

/// Session lifecycle configuration (`[memory.session]`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct MemorySessionConfig {
    /// Whether to auto-save a session summary to memory on session end.
    pub save_on_end: bool,
}

impl Default for MemorySessionConfig {
    fn default() -> Self {
        Self { save_on_end: true }
    }
}

/// autoDream consolidation configuration (`[memory.dream]`).
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(default)]
pub struct MemoryDreamConfig {
    /// Whether autoDream background consolidation is enabled.
    pub enabled: bool,
    /// Minimum hours between consolidations.
    pub min_hours: u64,
    /// Minimum sessions since last consolidation to trigger.
    pub min_sessions: u64,
    /// Seconds before a stale dream lock is reclaimed.
    pub stale_lock_secs: u64,
    /// Periodic dream check interval in seconds.
    /// `None` = disabled (dream only at session end or via /dream).
    /// When set, the session actor checks dream gates on this interval.
    pub check_interval_secs: Option<u64>,
}

impl Default for MemoryDreamConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_hours: 24,
            min_sessions: 1,
            stale_lock_secs: 3600,
            check_interval_secs: Some(3600),
        }
    }
}

/// File watcher configuration for detecting external memory edits (`[memory.watcher]`).
///
/// When enabled, watches `~/.echo-agent/memory/` for `.md` file changes (create,
/// modify, delete) and syncs the index on the next `memory_search` call:
/// - Created/modified files are reindexed.
/// - Deleted files have their stale chunks removed from the index.
///
/// Events are coalesced in a lock-free `ArcSwap` set; sync runs at most once
/// per search call when dirty files are present and the claim is acquired.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct MemoryWatcherConfig {
    /// Whether the file watcher is enabled. Default: true (when memory is enabled).
    pub enabled: bool,
    /// Seconds after which a reindex claim is considered stale (crashed agent).
    /// Default: 60.
    pub stale_claim_secs: i64,
}

impl Default for MemoryWatcherConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            stale_claim_secs: 60,
        }
    }
}

/// Garbage collection for orphaned workspace memory directories (`[memory.gc]`).
///
/// On session init, directories under `~/.echo-agent/memory/` are scanned:
/// - `tmp*` dirs: empty ones removed unconditionally, non-empty ones removed
///   after 7 days.
/// - Other workspaces with no session files: removed after `max_age_days`.
/// - Non-empty non-tmp workspaces: never touched.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct MemoryGcConfig {
    pub max_age_days: u64,
}

impl Default for MemoryGcConfig {
    fn default() -> Self {
        Self { max_age_days: 30 }
    }
}

/// Pre-compaction memory flush configuration (`[compaction.memory_flush]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryFlushConfig {
    /// Whether the flush step is enabled before compaction.
    pub enabled: bool,
    /// Token headroom before the compact threshold to trigger flush.
    pub soft_threshold_tokens: u64,
    /// Model to use for the flush turn. `None` = session's primary model.
    pub flush_model: Option<String>,
    /// Max characters the flush response may write to memory.
    pub max_flush_write_chars: usize,
    /// Idle timeout in seconds: when no user message is received for this
    /// duration, a background flush is triggered automatically.
    /// `None` = disabled (flush only before compaction).
    #[serde(default)]
    pub idle_timeout_secs: Option<u64>,
    /// Cosine similarity threshold for semantic dedup of flush content.
    /// When `None`, falls back to the compiled-in default (0.92).
    /// Clamped to [0.0, 1.0] at parse time.
    #[serde(default, deserialize_with = "deserialize_clamped_unit_option")]
    pub semantic_dedup_threshold: Option<f64>,
}

impl Default for MemoryFlushConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            soft_threshold_tokens: 4000,
            flush_model: None,
            max_flush_write_chars: 8000,
            idle_timeout_secs: Some(300),
            semantic_dedup_threshold: None,
        }
    }
}

/// Tool-result pruning configuration (`[compaction.pruning]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PruningConfig {
    /// Whether pruning is enabled.
    pub enabled: bool,
    /// Number of recent turns whose tool results are never pruned.
    pub keep_last_n_turns: usize,
    /// Character threshold above which old tool results are soft-trimmed.
    pub soft_trim_threshold: usize,
    /// Characters to keep from the start of a soft-trimmed result.
    pub soft_trim_head: usize,
    /// Characters to keep from the end of a soft-trimmed result.
    pub soft_trim_tail: usize,
    /// Turn age after which tool results are hard-cleared (replaced with placeholder).
    pub hard_clear_age_turns: usize,
}

impl Default for PruningConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            keep_last_n_turns: 3,
            soft_trim_threshold: 4000,
            soft_trim_head: 1500,
            soft_trim_tail: 1500,
            hard_clear_age_turns: 10,
        }
    }
}

/// Concrete memory configuration used by a running session.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    pub enabled: bool,
    pub index: MemoryIndexConfig,
    pub embedding: MemoryEmbeddingConfig,
    pub search: MemorySearchConfig,
    pub initial_injection: MemoryInitialInjectionConfig,
    pub session: MemorySessionConfig,
    pub watcher: MemoryWatcherConfig,
    pub gc: MemoryGcConfig,
    pub dream: MemoryDreamConfig,
    #[serde(skip)]
    pub flush: MemoryFlushConfig,
    #[serde(skip)]
    pub pruning: PruningConfig,
    #[serde(skip)]
    pub root_dir_override: Option<std::path::PathBuf>,
    #[serde(skip)]
    pub flat_memory_root: bool,
}

impl MemoryConfig {
    /// Legacy compatibility wrapper for callers that pass both memory flags.
    #[doc(hidden)]
    pub fn resolve(
        experimental_memory: bool,
        no_memory: bool,
        config: &toml::Value,
        remote: Option<&crate::RemoteSettings>,
    ) -> Self {
        let memory_enabled_override = if no_memory {
            Some(false)
        } else if experimental_memory {
            Some(true)
        } else {
            None
        };
        Self::resolve_with_override(memory_enabled_override, config, remote)
    }

    /// Compatibility resolver for callers that still hold the effective TOML value.
    #[doc(hidden)]
    pub fn resolve_with_override(
        memory_enabled_override: Option<bool>,
        config: &toml::Value,
        remote: Option<&crate::RemoteSettings>,
    ) -> Self {
        let memory = config
            .get("memory")
            .and_then(|value| value.clone().try_into().ok())
            .unwrap_or_default();
        let compaction = config.get("compaction");
        let flush = compaction
            .and_then(|value| value.get("memory_flush"))
            .and_then(|value| value.clone().try_into().ok())
            .unwrap_or_default();
        let pruning = compaction
            .and_then(|value| value.get("pruning"))
            .and_then(|value| value.clone().try_into().ok())
            .unwrap_or_default();
        Self::resolve_settings(memory_enabled_override, &memory, &flush, &pruning, remote)
    }

    /// Resolve typed effective-TOML settings against remote values and code defaults.
    pub fn resolve_settings(
        memory_enabled_override: Option<bool>,
        memory: &MemorySettings,
        flush: &MemoryFlushSettings,
        pruning: &PruningSettings,
        remote: Option<&crate::RemoteSettings>,
    ) -> Self {
        let defaults = Self::default();
        let index = memory.index.as_ref();
        let embedding = memory.embedding.as_ref();
        let search = memory.search.as_ref();
        let temporal_decay = search.and_then(|settings| settings.temporal_decay.as_ref());
        let mmr = search.and_then(|settings| settings.mmr.as_ref());
        let initial_injection = memory.initial_injection.as_ref();
        let session = memory.session.as_ref();
        let watcher = memory.watcher.as_ref();
        let gc = memory.gc.as_ref();
        let dream = memory.dream.as_ref();

        Self {
            enabled: crate::BoolFlag::env("ECHO_AGENT_MEMORY")
                .cli(memory_enabled_override)
                .config(memory.enabled)
                .feature_flag(remote.and_then(|settings| settings.memory_enabled))
                .default(false)
                .resolve()
                .value,
            index: MemoryIndexConfig {
                max_chunk_chars: index
                    .and_then(|settings| settings.max_chunk_chars)
                    .unwrap_or(defaults.index.max_chunk_chars),
                chunk_overlap_chars: index
                    .and_then(|settings| settings.chunk_overlap_chars)
                    .unwrap_or(defaults.index.chunk_overlap_chars),
            },
            embedding: MemoryEmbeddingConfig {
                provider: embedding
                    .and_then(|settings| settings.provider.clone())
                    .unwrap_or(defaults.embedding.provider),
                model: match embedding.and_then(|settings| settings.model.as_deref()) {
                    Some("") => None,
                    Some(model) => Some(model.to_owned()),
                    None => remote.and_then(|settings| settings.memory_embedding_model.clone()),
                },
                dimensions: embedding
                    .and_then(|settings| settings.dimensions)
                    .or_else(|| {
                        remote
                            .and_then(|settings| settings.memory_embedding_dimensions)
                            .map(|value| value as usize)
                    })
                    .unwrap_or(defaults.embedding.dimensions),
                endpoint: None,
                api_key: None,
                send_dimensions: true,
            },
            search: MemorySearchConfig {
                max_results: search
                    .and_then(|settings| settings.max_results)
                    .or_else(|| {
                        remote
                            .and_then(|settings| settings.memory_search_max_results)
                            .map(|value| value as usize)
                    })
                    .unwrap_or(defaults.search.max_results),
                min_score: search
                    .and_then(|settings| settings.min_score)
                    .or_else(|| remote.and_then(|settings| settings.memory_search_min_score))
                    .unwrap_or(defaults.search.min_score),
                vector_weight: search
                    .and_then(|settings| settings.vector_weight)
                    .unwrap_or(defaults.search.vector_weight),
                text_weight: search
                    .and_then(|settings| settings.text_weight)
                    .unwrap_or(defaults.search.text_weight),
                recency_decay: search
                    .and_then(|settings| settings.recency_decay)
                    .unwrap_or(defaults.search.recency_decay),
                temporal_decay: TemporalDecayConfig {
                    enabled: temporal_decay
                        .and_then(|settings| settings.enabled)
                        .or_else(|| {
                            remote.and_then(|settings| settings.memory_temporal_decay_enabled)
                        })
                        .unwrap_or(defaults.search.temporal_decay.enabled),
                    half_life_days: temporal_decay
                        .and_then(|settings| settings.half_life_days)
                        .or_else(|| {
                            remote
                                .and_then(|settings| settings.memory_temporal_decay_half_life_days)
                        })
                        .unwrap_or(defaults.search.temporal_decay.half_life_days),
                },
                mmr: MmrConfig {
                    enabled: mmr
                        .and_then(|settings| settings.enabled)
                        .or_else(|| remote.and_then(|settings| settings.memory_mmr_enabled))
                        .unwrap_or(defaults.search.mmr.enabled),
                    lambda: mmr
                        .and_then(|settings| settings.lambda)
                        .or_else(|| remote.and_then(|settings| settings.memory_mmr_lambda))
                        .unwrap_or(defaults.search.mmr.lambda)
                        .clamp(0.0, 1.0),
                },
                reranker: MemoryRerankerConfig::default(),
                source_weights: search
                    .and_then(|settings| settings.source_weights.clone())
                    .unwrap_or(defaults.search.source_weights),
            },
            initial_injection: MemoryInitialInjectionConfig {
                enabled: initial_injection
                    .and_then(|settings| settings.enabled)
                    .or_else(|| {
                        remote.and_then(|settings| settings.memory_initial_injection_enabled)
                    })
                    .unwrap_or(defaults.initial_injection.enabled),
                min_score: initial_injection
                    .and_then(|settings| settings.min_score)
                    .or_else(|| {
                        remote.and_then(|settings| settings.memory_initial_injection_min_score)
                    })
                    .or(defaults.initial_injection.min_score),
            },
            session: MemorySessionConfig {
                save_on_end: session
                    .and_then(|settings| settings.save_on_end)
                    .unwrap_or(defaults.session.save_on_end),
            },
            watcher: MemoryWatcherConfig {
                enabled: watcher
                    .and_then(|settings| settings.enabled)
                    .or_else(|| remote.and_then(|settings| settings.memory_watcher_enabled))
                    .unwrap_or(defaults.watcher.enabled),
                stale_claim_secs: watcher
                    .and_then(|settings| settings.stale_claim_secs)
                    .unwrap_or(defaults.watcher.stale_claim_secs),
            },
            gc: MemoryGcConfig {
                max_age_days: gc
                    .and_then(|settings| settings.max_age_days)
                    .unwrap_or(defaults.gc.max_age_days),
            },
            dream: MemoryDreamConfig {
                enabled: dream
                    .and_then(|settings| settings.enabled)
                    .or_else(|| remote.and_then(|settings| settings.dream_enabled))
                    .unwrap_or(defaults.dream.enabled),
                min_hours: dream
                    .and_then(|settings| settings.min_hours)
                    .or_else(|| remote.and_then(|settings| settings.dream_min_hours))
                    .unwrap_or(defaults.dream.min_hours),
                min_sessions: dream
                    .and_then(|settings| settings.min_sessions)
                    .or_else(|| remote.and_then(|settings| settings.dream_min_sessions))
                    .unwrap_or(defaults.dream.min_sessions),
                stale_lock_secs: dream
                    .and_then(|settings| settings.stale_lock_secs)
                    .unwrap_or(defaults.dream.stale_lock_secs),
                check_interval_secs: match dream.and_then(|settings| settings.check_interval_secs) {
                    Some(0) => None,
                    Some(seconds) => Some(seconds),
                    None => match remote.and_then(|settings| settings.dream_check_interval_secs) {
                        Some(0) => None,
                        Some(seconds) => Some(seconds),
                        None => defaults.dream.check_interval_secs,
                    },
                },
            },
            flush: MemoryFlushConfig {
                enabled: flush
                    .enabled
                    .or_else(|| remote.and_then(|settings| settings.flush_enabled))
                    .unwrap_or(defaults.flush.enabled),
                soft_threshold_tokens: flush
                    .soft_threshold_tokens
                    .or_else(|| remote.and_then(|settings| settings.flush_soft_threshold_tokens))
                    .unwrap_or(defaults.flush.soft_threshold_tokens),
                flush_model: match flush.flush_model.as_deref() {
                    Some("") | None => None,
                    Some(model) => Some(model.to_owned()),
                },
                max_flush_write_chars: flush
                    .max_flush_write_chars
                    .unwrap_or(defaults.flush.max_flush_write_chars),
                idle_timeout_secs: match flush.idle_timeout_secs {
                    Some(0) => None,
                    Some(seconds) => Some(seconds),
                    None => match remote.and_then(|settings| settings.flush_idle_timeout_secs) {
                        Some(0) => None,
                        Some(seconds) => Some(seconds),
                        None => defaults.flush.idle_timeout_secs,
                    },
                },
                semantic_dedup_threshold: flush
                    .semantic_dedup_threshold
                    .or_else(|| {
                        remote
                            .and_then(|settings| settings.flush_semantic_dedup_threshold)
                            .map(|value| value.clamp(0.0, 1.0))
                    })
                    .or(defaults.flush.semantic_dedup_threshold),
            },
            pruning: PruningConfig {
                enabled: pruning
                    .enabled
                    .or_else(|| remote.and_then(|settings| settings.pruning_enabled))
                    .unwrap_or(defaults.pruning.enabled),
                keep_last_n_turns: pruning
                    .keep_last_n_turns
                    .or_else(|| {
                        remote
                            .and_then(|settings| settings.pruning_keep_last_n_turns)
                            .map(|value| value as usize)
                    })
                    .unwrap_or(defaults.pruning.keep_last_n_turns),
                soft_trim_threshold: pruning
                    .soft_trim_threshold
                    .or_else(|| {
                        remote
                            .and_then(|settings| settings.pruning_soft_trim_threshold)
                            .map(|value| value as usize)
                    })
                    .unwrap_or(defaults.pruning.soft_trim_threshold),
                soft_trim_head: pruning
                    .soft_trim_head
                    .unwrap_or(defaults.pruning.soft_trim_head),
                soft_trim_tail: pruning
                    .soft_trim_tail
                    .unwrap_or(defaults.pruning.soft_trim_tail),
                hard_clear_age_turns: pruning
                    .hard_clear_age_turns
                    .unwrap_or(defaults.pruning.hard_clear_age_turns),
            },
            root_dir_override: None,
            flat_memory_root: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mmr_lambda_is_clamped_on_deserialize() {
        let m: MmrConfig = serde_json::from_str(r#"{"enabled": true, "lambda": 5.0}"#).unwrap();
        assert_eq!(m.lambda, 1.0);
        let m: MmrConfig = serde_json::from_str(r#"{"enabled": true, "lambda": -3.0}"#).unwrap();
        assert_eq!(m.lambda, 0.0);
    }

    #[test]
    fn flush_semantic_dedup_threshold_clamped_option() {
        let f: MemoryFlushConfig =
            serde_json::from_str(r#"{"semantic_dedup_threshold": 2.0}"#).unwrap();
        assert_eq!(f.semantic_dedup_threshold, Some(1.0));
        let f: MemoryFlushConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(f.semantic_dedup_threshold, None);
    }

    #[test]
    fn effective_half_life_prefers_temporal_decay() {
        let mut s = MemorySearchConfig::default();
        s.temporal_decay.enabled = true;
        s.temporal_decay.half_life_days = 14.0;
        assert_eq!(s.effective_half_life_days(), Some(14.0));
    }

    #[test]
    fn effective_half_life_converts_legacy_recency_decay() {
        let mut s = MemorySearchConfig::default();
        s.temporal_decay.enabled = false;
        s.recency_decay = 0.5; // non-default → converted
        let hl = s.effective_half_life_days().unwrap();
        assert!(
            (hl - 1.0).abs() < 1e-9,
            "0.5 per-day decay ⇒ ~1 day half-life, got {hl}"
        );
    }

    #[test]
    fn effective_half_life_none_when_disabled_and_default_recency() {
        let mut s = MemorySearchConfig::default();
        s.temporal_decay.enabled = false;
        // recency_decay left at default ⇒ no decay
        assert_eq!(s.effective_half_life_days(), None);
    }
}
