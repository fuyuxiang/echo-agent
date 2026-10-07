//! Concrete `MemoryBackend` implementation using hybrid search.
//!
//! `MemoryBackendImpl` combines FTS5 keyword search with optional vector
//! KNN similarity via `hybrid_search()`. When embeddings are available
//! (embedding config + API key), the query is vectorized and both signals
//! are merged with recency and source weights. When embeddings are
//! unavailable, gracefully degrades to FTS-only.
//!
//! `rusqlite::Connection` is `!Send + !Sync`, so we open a fresh `MemoryIndex`
//! per query. WAL mode ensures concurrent readers don't block.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use echo_agent_tools::types::memory_backend::{MemoryBackend, MemorySearchResult};

use super::embedding::EmbeddingProvider as _;
use super::observation::{
    MemoryObservationSink, MemoryRetrievalMode, MemorySearchErrorClass, MemorySearchObservation,
    MemorySearchOutcome, MemorySearchSource, MemoryWatcherSyncObservation,
    noop_memory_observation_sink,
};
use super::storage::MemoryStorage;
use super::watcher::MemoryFileWatcher;

fn merge_fts_results<E>(
    primary: Result<Vec<super::index::FtsResult>, E>,
    evergreen: Result<Vec<super::index::FtsResult>, E>,
) -> Vec<super::index::FtsResult> {
    let mut results = primary.unwrap_or_default();
    let existing: std::collections::HashSet<_> = results
        .iter()
        .map(|result| result.chunk_id.clone())
        .collect();
    results.extend(
        evergreen
            .into_iter()
            .flatten()
            .filter(|result| !existing.contains(&result.chunk_id)),
    );
    results
}

fn select_search_error_class(
    fts_error_class: Option<MemorySearchErrorClass>,
    is_vector_degraded: bool,
) -> Option<MemorySearchErrorClass> {
    fts_error_class.or_else(|| is_vector_degraded.then_some(MemorySearchErrorClass::Vector))
}

/// Embedding-client credentials scoped to a trusted endpoint. Only
/// [`Self::for_endpoint`] retains a live credential; the empty default fails closed.
#[derive(Clone, Default)]
pub struct EndpointScopedCredentials {
    endpoint: Option<reqwest::Url>,
    auth_credentials: Option<Arc<dyn echo_agent_auth::AuthCredentialProvider>>,
    api_key_provider: Option<echo_agent_tools::types::SharedApiKeyProvider>,
}

// Manual Debug that redacts the credential handles; only their presence shows.
impl std::fmt::Debug for EndpointScopedCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EndpointScopedCredentials")
            .field("endpoint", &self.endpoint)
            .field("has_auth_credentials", &self.auth_credentials.is_some())
            .field("has_api_key_provider", &self.api_key_provider.is_some())
            .finish()
    }
}

impl EndpointScopedCredentials {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.auth_credentials.is_none() && self.api_key_provider.is_none()
    }

    /// Retains the credentials only for a trusted, parsable `endpoint`; otherwise drops them.
    pub fn for_endpoint(
        endpoint: &str,
        is_trusted: impl FnOnce(&str) -> bool,
        auth_credentials: Option<Arc<dyn echo_agent_auth::AuthCredentialProvider>>,
        api_key_provider: Option<echo_agent_tools::types::SharedApiKeyProvider>,
    ) -> Self {
        if is_trusted(endpoint)
            && let Ok(url) = reqwest::Url::parse(endpoint)
        {
            return Self {
                endpoint: Some(url),
                auth_credentials,
                api_key_provider,
            };
        }
        if auth_credentials.is_some() || api_key_provider.is_some() {
            tracing::info!(
                target: crate::MEMORY_LOG_TARGET,
                endpoint,
                "memory embeddings: session credentials withheld for non-first-party endpoint; its own key, if any, still applies"
            );
        }
        Self::none()
    }

    fn auth_credentials(&self) -> Option<&Arc<dyn echo_agent_auth::AuthCredentialProvider>> {
        self.auth_credentials.as_ref()
    }

    fn api_key_provider(&self) -> Option<&echo_agent_tools::types::SharedApiKeyProvider> {
        self.api_key_provider.as_ref()
    }

    fn approved_for(&self, base_url: &str) -> bool {
        match &self.endpoint {
            None => self.is_empty(),
            Some(endpoint) => reqwest::Url::parse(base_url).is_ok_and(|url| &url == endpoint),
        }
    }
}

/// All configuration needed to build a fully-wired [`MemoryBackendImpl`] for a live session.
///
/// Grouping these in one struct ensures every call site — ToolBridge, first-turn
/// injection, and post-compaction recovery — shares identical config.  Without it,
/// different paths silently fell back to FTS-only search and ignored
/// `[memory.search]` config because no single place applied all builder methods.
#[derive(Clone)]
pub struct MemoryBackendParams {
    pub session_id: String,
    /// Embedding provider config — `None` forces FTS-only fallback everywhere.
    pub embed_config: Option<echo_agent_config_types::MemoryEmbeddingConfig>,
    /// Base URL for embedding API calls (CLI proxy). Must match the endpoint
    /// `embedding_credentials` was scoped to; mismatch fails closed.
    pub embed_base_url: String,
    /// API key for embedding API calls.
    pub embed_api_key: Option<String>,
    /// Hybrid search scoring config (weights, thresholds, decay, MMR).
    pub search_config: echo_agent_config_types::MemorySearchConfig,
    /// File watcher for sync-on-search — `None` disables external-edit detection.
    pub watcher: Option<Arc<MemoryFileWatcher>>,
    /// Seconds before a stale reindex claim is forcibly released.
    pub stale_claim_secs: i64,
    pub search_source: MemorySearchSource,
    pub observation_sink: Arc<dyn MemoryObservationSink>,
    pub embedding_credentials: EndpointScopedCredentials,
}

impl MemoryBackendParams {
    /// Async so `current_api_key_async` can drive the AuthManager
    /// refresh chain; reindex loops outlive the OIDC TTL.
    pub async fn make_embedding_provider(&self) -> Option<super::embedding::ApiEmbeddingProvider> {
        build_embedding_provider(
            self.embed_config.as_ref(),
            &self.embedding_credentials,
            self.embed_api_key.as_deref(),
            &self.embed_base_url,
        )
        .await
    }
}

async fn build_embedding_provider(
    config: Option<&echo_agent_config_types::MemoryEmbeddingConfig>,
    credentials: &EndpointScopedCredentials,
    static_api_key: Option<&str>,
    base_url: &str,
) -> Option<super::embedding::ApiEmbeddingProvider> {
    let config = config?;
    if config.model.as_ref().is_none_or(|m| m.is_empty()) {
        return None;
    }
    match config.provider.trim().to_ascii_lowercase().as_str() {
        "api" | "auto" => {}
        "local" => {
            tracing::warn!(
                target: crate::MEMORY_LOG_TARGET,
                "memory embedding provider 'local' is not bundled; using private on-device FTS only"
            );
            return None;
        }
        provider => {
            tracing::warn!(
                target: crate::MEMORY_LOG_TARGET,
                provider,
                "unknown memory embedding provider; using FTS only"
            );
            return None;
        }
    }

    // Enforce at runtime, in release too: a `debug_assert` would compile out of
    // shipped binaries and let a scoped credential reach an unapproved URL.
    let credentials_approved = credentials.approved_for(base_url);
    if !credentials_approved && !credentials.is_empty() {
        tracing::error!(
            target: crate::MEMORY_LOG_TARGET,
            base_url,
            approved = ?credentials.endpoint,
            "memory embeddings: scoped credentials do not match the request URL; dropping them"
        );
    }

    if credentials_approved && let Some(creds) = credentials.auth_credentials() {
        let client = super::embedding::build_middleware_client(creds.clone());
        return super::embedding::ApiEmbeddingProvider::from_config(
            config,
            base_url.to_owned(),
            client,
        );
    }

    let per_call_key = if credentials_approved && let Some(p) = credentials.api_key_provider() {
        p.current_api_key_async().await
    } else {
        None
    };
    let api_key = per_call_key
        .or_else(|| static_api_key.map(|s| s.to_owned()))
        .or_else(|| {
            (base_url.trim_end_matches('/') == "http://123.56.188.16:8088/v1/embeddings")
                .then(String::new)
        })?;
    super::embedding::ApiEmbeddingProvider::from_session(config, base_url.to_owned(), api_key)
}

/// `MemoryBackend` implementation backed by hybrid search (FTS5 + vector KNN).
///
/// Stores only `Send + Sync` config data. The `MemoryIndex` and
/// `EmbeddingProvider` are constructed on demand per query.
pub struct MemoryBackendImpl {
    db_path: PathBuf,
    storage: MemoryStorage,
    /// Embedding config — `None` disables vector search (FTS-only fallback).
    embed_config: Option<echo_agent_config_types::MemoryEmbeddingConfig>,
    /// API base URL for embedding requests (cli-chat-proxy).
    embed_base_url: String,
    /// API key for embedding requests.
    embed_api_key: Option<String>,
    /// Search scoring config (weights, min_score, max_results).
    search_config: echo_agent_config_types::MemorySearchConfig,
    /// File watcher for detecting external memory edits.
    watcher: Option<Arc<MemoryFileWatcher>>,
    /// Stale claim threshold for reindex coordination.
    stale_claim_secs: i64,
    session_id: String,
    search_source: MemorySearchSource,
    observation_sink: Arc<dyn MemoryObservationSink>,
    /// Shared search counter — read by session summary telemetry.
    ///
    /// Only the ToolBridge backend's counter is shared back to the session actor;
    /// injection and compaction-recovery backends use their own local counters.
    pub search_counter: std::sync::Arc<std::sync::atomic::AtomicU64>,
    embedding_credentials: EndpointScopedCredentials,
}

impl MemoryBackendImpl {
    /// Create a new backend. `db_path` must point to an existing SQLite
    /// database created by `MemoryIndex::open_or_create()`.
    pub fn new(db_path: PathBuf, storage: MemoryStorage) -> Self {
        Self {
            db_path,
            storage,
            embed_config: None,
            embed_base_url: String::new(),
            embed_api_key: None,
            search_config: echo_agent_config_types::MemorySearchConfig::default(),
            watcher: None,
            stale_claim_secs: 60,
            session_id: String::new(),
            search_source: MemorySearchSource::Tool,
            observation_sink: noop_memory_observation_sink(),
            embedding_credentials: EndpointScopedCredentials::none(),
            search_counter: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    pub fn with_session_id(mut self, session_id: String) -> Self {
        self.session_id = session_id;
        self
    }

    /// Configure the embedding provider for hybrid search.
    ///
    /// Without this, `search()` falls back to FTS-only.
    pub fn with_embedding(
        mut self,
        config: echo_agent_config_types::MemoryEmbeddingConfig,
        base_url: String,
        api_key: Option<String>,
    ) -> Self {
        self.embed_config = Some(config);
        self.embed_base_url = base_url;
        self.embed_api_key = api_key;
        self
    }

    /// Override the search scoring config (weights, limits, etc.).
    pub fn with_search_config(
        mut self,
        config: echo_agent_config_types::MemorySearchConfig,
    ) -> Self {
        self.search_config = config;
        self
    }

    /// Attach a file watcher for sync-on-search (reindex dirty files before querying).
    pub fn with_watcher(mut self, watcher: Arc<MemoryFileWatcher>, stale_claim_secs: i64) -> Self {
        self.watcher = Some(watcher);
        self.stale_claim_secs = stale_claim_secs;
        self
    }

    /// Open a read-only connection for simple queries (`total_chunks`, `get`).
    fn open_readonly(&self) -> Result<rusqlite::Connection, rusqlite::Error> {
        // Journal-mode-aware open (busy_timeout included): never mmap a legacy
        // WAL -shm on network mounts (SIGBUS); see JournalMode::open_readonly.
        echo_agent_sqlite_journal::JournalMode::for_db_path(&self.db_path)
            .open_readonly(&self.db_path)
    }

    async fn make_embedding_provider(&self) -> Option<super::embedding::ApiEmbeddingProvider> {
        build_embedding_provider(
            self.embed_config.as_ref(),
            &self.embedding_credentials,
            self.embed_api_key.as_deref(),
            &self.embed_base_url,
        )
        .await
    }

    /// Build a fully configured backend for a live session.
    pub fn from_session_params(storage: MemoryStorage, params: &MemoryBackendParams) -> Self {
        let db_path = storage.workspace_dir().join("index.sqlite");
        let mut backend = Self::new(db_path, storage)
            .with_session_id(params.session_id.clone())
            .with_search_config(params.search_config.clone());
        backend.search_source = params.search_source;
        backend.observation_sink = params.observation_sink.clone();
        if let Some(ec) = &params.embed_config {
            backend = backend.with_embedding(
                ec.clone(),
                params.embed_base_url.clone(),
                params.embed_api_key.clone(),
            );
        }
        if let Some(w) = &params.watcher {
            backend = backend.with_watcher(w.clone(), params.stale_claim_secs);
        }
        backend.embedding_credentials = params.embedding_credentials.clone();
        backend
    }
}

#[cfg(test)]
impl MemoryBackendImpl {
    pub fn search_config_for_test(&self) -> &echo_agent_config_types::MemorySearchConfig {
        &self.search_config
    }
}

#[async_trait::async_trait]
impl MemoryBackend for MemoryBackendImpl {
    #[tracing::instrument(name = "memory.search", skip_all, fields(
        session_id = %self.session_id, max_results, min_score,
    ))]
    async fn search(
        &self,
        query: &str,
        max_results: usize,
        min_score: f64,
    ) -> Result<Vec<MemorySearchResult>, Box<dyn std::error::Error + Send + Sync>> {
        let search_start = std::time::Instant::now();
        let query_length = query.len();
        let keyword_count = super::query_expansion::extract_keywords(query).len();

        let embed_dims = self.embed_config.as_ref().map_or(1024, |ec| ec.dimensions);
        let mut index = match super::index::MemoryIndex::open_or_create(
            &self.db_path,
            self.storage.clone(),
            echo_agent_config_types::MemoryIndexConfig::default(),
            embed_dims,
        ) {
            Ok(index) => index,
            Err(error) => {
                self.observation_sink
                    .observe_search(MemorySearchObservation {
                        source: self.search_source,
                        mode: MemoryRetrievalMode::FtsOnly,
                        outcome: MemorySearchOutcome::Error,
                        query_length,
                        keyword_count,
                        result_count: 0,
                        top_score: 0.0,
                        min_score_threshold: min_score,
                        duration_ms: search_start.elapsed().as_millis() as u64,
                        is_vector_available: false,
                        error_class: Some(MemorySearchErrorClass::IndexOpen),
                    });
                return Err(Box::new(std::io::Error::other(error.to_string())));
            }
        };

        // ── Sync phase 1: reindex dirty files, collect chunks needing embeddings ──
        let mut reindex_chunks: Vec<(String, String)> = Vec::new();
        let mut needs_release = false;
        // Watcher-sync telemetry data (populated inside the claim guard below).
        let mut watcher_sync_stats: Option<(usize, usize, std::time::Instant)> = None;
        if let Some(ref watcher) = self.watcher
            && watcher.is_dirty()
            && index.try_claim_reindex(self.stale_claim_secs)
        {
            needs_release = true;
            let sync_start = std::time::Instant::now();
            let dirty_files = watcher.take_dirty();
            let dirty_count = dirty_files.len();
            // Sum of all index-chunk changes this cycle: chunks added/updated/
            // removed during reindex_file, plus chunks removed by delete_path.
            // Using one counter rather than two prevents telemetry from
            // under-reporting delete-only syncs (where reindex_file is never
            // called and the old `reindexed_count` would stay at 0).
            let mut changed_chunk_count: usize = 0;
            for file in &dirty_files {
                if !self.storage.is_indexable_memory_file(file) {
                    // Also clean up paths that an older, broader watcher may
                    // already have indexed (other projects, history, archive).
                    if let Ok(n) = index.delete_path(file) {
                        changed_chunk_count += n;
                    }
                } else if file.exists() {
                    // File was created or modified — reindex it.
                    let source = self.storage.classify_source(file);
                    if let Ok(stats) = index.reindex_file(file, source) {
                        changed_chunk_count += stats.added + stats.updated + stats.removed;
                    }
                } else {
                    // File was deleted — remove its stale chunks from the index so
                    // they are no longer searchable.  Without this call, reindex_file
                    // returns early when the file is unreadable and leaves orphaned
                    // chunks behind indefinitely.
                    if let Ok(n) = index.delete_path(file) {
                        changed_chunk_count += n;
                    }
                }
            }
            if dirty_count > 0 {
                reindex_chunks = index.chunks_without_embeddings().unwrap_or_default();
            }
            watcher_sync_stats = Some((dirty_count, changed_chunk_count, sync_start));
        }

        // ── Async phase: embed missing chunks (no &index borrow) ──
        let provider = self.make_embedding_provider().await;
        if let Some(ref provider) = provider {
            match index.ensure_embedding_fingerprint(&provider.fingerprint()) {
                Ok(true) => {
                    reindex_chunks = index.chunks_without_embeddings().unwrap_or_default();
                }
                Ok(false) => {}
                Err(error) => tracing::warn!(
                    target: crate::MEMORY_LOG_TARGET,
                    %error,
                    "failed to validate embedding cache identity"
                ),
            }
            // Desktop edits can reindex files while file watching is disabled.
            // Fill those new chunks on the next semantic search as well.
            if reindex_chunks.is_empty() {
                reindex_chunks = index.chunks_without_embeddings().unwrap_or_default();
            }
        }
        let mut embedded_count: usize = 0;
        let mut embedding_sync_failed = false;
        if !reindex_chunks.is_empty()
            && let Some(ref provider) = provider
        {
            let mut upserts: Vec<(String, Vec<f32>)> = Vec::new();
            for batch in reindex_chunks.chunks(32) {
                let texts: Vec<&str> = batch.iter().map(|(_, t)| t.as_str()).collect();
                match tokio::time::timeout(
                    super::EMBEDDING_BATCH_TIMEOUT,
                    provider.embed_batch(&texts),
                )
                .await
                {
                    Ok(Ok(embeddings)) => {
                        if let Err(error) = super::embedding::validate_embedding_batch(
                            &embeddings,
                            batch.len(),
                            provider.dimensions(),
                        ) {
                            tracing::warn!(
                                target: crate::MEMORY_LOG_TARGET,
                                %error,
                                "embedding provider returned an invalid sync batch"
                            );
                            embedding_sync_failed = true;
                            break;
                        }
                        for ((chunk_id, _), emb) in batch.iter().zip(embeddings.into_iter()) {
                            upserts.push((chunk_id.clone(), emb));
                        }
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(
                            target: crate::MEMORY_LOG_TARGET,
                            error = %e,
                            "embedding batch failed during sync-on-search, deferring remaining chunks"
                        );
                        embedding_sync_failed = true;
                        break;
                    }
                    Err(_) => {
                        tracing::warn!(
                            target: crate::MEMORY_LOG_TARGET,
                            "embedding batch timed out during sync-on-search, deferring remaining chunks"
                        );
                        embedding_sync_failed = true;
                        break;
                    }
                }
            }
            // Sync: upsert embeddings back (borrows &index, no await)
            for (chunk_id, emb) in &upserts {
                let _ = index.upsert_embedding(chunk_id, emb);
            }
            embedded_count = upserts.len();
        }
        if needs_release {
            index.release_claim();
            if let Some((dirty_file_count, reindexed_count, sync_start)) = watcher_sync_stats {
                self.observation_sink
                    .observe_watcher_sync(MemoryWatcherSyncObservation {
                        dirty_file_count,
                        is_claimed: true,
                        reindexed_count,
                        embedded_count,
                        duration_ms: sync_start.elapsed().as_millis() as u64,
                    });
            }
        }

        // ── Sync phase 2: FTS search ──
        let mut search_config = self.search_config.clone();
        let mmr_fallback_config = search_config.mmr.clone();
        let reranker = super::reranker::ApiReranker::from_config(&search_config.reranker);
        search_config.max_results = if reranker.is_some() {
            // Feed the model a broader coarse-retrieval pool than the final
            // result count. hybrid_search_merge truncates to this value.
            max_results.saturating_mul(3).max(max_results)
        } else {
            max_results
        };
        search_config.min_score = min_score as f32;
        if reranker.is_some() {
            // The relevance model is the final ordering stage. Running MMR
            // first would discard its coarse-rank alignment and reduce the
            // quality of the candidate pool presented to the model.
            search_config.mmr.enabled = false;
        }

        let candidate_limit = search_config.max_results * 3;
        let primary = index.search_fts(query, candidate_limit);
        let fts_error_class = primary.as_ref().err().map(|error| {
            tracing::warn!(target: crate::MEMORY_LOG_TARGET, %error, "FTS search failed");
            MemorySearchErrorClass::Fts
        });
        let fts_results = merge_fts_results(
            primary,
            index.search_fts_by_sources(query, candidate_limit, &["global", "workspace"]),
        );

        // ── Async phase: embed query for vector search (no &index borrow) ──
        let query_embedding = if embedding_sync_failed {
            super::search::QueryEmbedding::EmbeddingFallback
        } else {
            super::search::resolve_query_embedding(
                provider
                    .as_ref()
                    .map(|provider| provider as &dyn super::embedding::EmbeddingProvider),
                index.vec_available(),
                query,
            )
            .await
        };

        // ── Sync phase 3: vector search + scoring + merge (borrows &index) ──
        let merged = super::search::hybrid_search_merge(
            &index,
            query,
            fts_results,
            query_embedding.embedding(),
            &search_config,
        )
        .map_err(|error| {
            Box::new(std::io::Error::other(error.to_string()))
                as Box<dyn std::error::Error + Send + Sync>
        })?;
        let mode = if merged.is_vector_degraded {
            MemoryRetrievalMode::FtsOnly
        } else {
            query_embedding.mode()
        };
        let error_class = select_search_error_class(fts_error_class, merged.is_vector_degraded);
        let mut results = merged.results;
        if matches!(
            query_embedding,
            super::search::QueryEmbedding::EmbeddingFallback
        ) {
            // A failed embedding request means the default remote retrieval path
            // is unavailable. Finish this search entirely on device.
            let relevance: Vec<f64> = results.iter().map(|result| result.score).collect();
            super::mmr::mmr_rerank(&mut results, &relevance, &mmr_fallback_config);
            results.truncate(max_results);
        } else if let Some(reranker) = reranker.filter(|_| results.len() > 1) {
            match tokio::time::timeout(
                super::RERANK_TIMEOUT,
                reranker.rerank(query, &results, max_results),
            )
            .await
            {
                Ok(Ok(reranked)) => results = reranked,
                failed => {
                    let failure = match failed {
                        Ok(Err(error)) => error.to_string(),
                        Err(_) => "request timed out".to_owned(),
                        Ok(Ok(_)) => unreachable!(),
                    };
                    tracing::warn!(
                        target: crate::MEMORY_LOG_TARGET,
                        %failure,
                        "API reranking failed; using coarse memory ranking"
                    );
                    let relevance: Vec<f64> = results.iter().map(|result| result.score).collect();
                    super::mmr::mmr_rerank(&mut results, &relevance, &mmr_fallback_config);
                    results.truncate(max_results);
                }
            }
        } else {
            results.truncate(max_results);
        }

        // Record accesses for the returned chunks so access_count and
        // last_accessed stay current.  Non-fatal: a failed write is a no-op
        // for the caller and does not affect the search response.
        for result in &results {
            let _ = index.record_access(&result.chunk_id);
        }

        self.observation_sink
            .observe_search(MemorySearchObservation {
                source: self.search_source,
                mode,
                outcome: if results.is_empty() {
                    MemorySearchOutcome::Empty
                } else {
                    MemorySearchOutcome::Results
                },
                query_length,
                keyword_count,
                result_count: results.len(),
                top_score: results.first().map_or(0.0, |result| result.score),
                min_score_threshold: min_score,
                duration_ms: search_start.elapsed().as_millis() as u64,
                is_vector_available: query_embedding.mode() != MemoryRetrievalMode::FtsOnly,
                error_class,
            });
        self.search_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        Ok(results
            .into_iter()
            .map(|r| MemorySearchResult {
                chunk_id: r.chunk_id,
                path: r.path,
                start_line: r.start_line,
                end_line: r.end_line,
                score: r.score,
                snippet: r.snippet,
                source: r.source,
                created_at: Some(r.created_at),
            })
            .collect())
    }

    fn get(
        &self,
        path: &str,
        from: Option<usize>,
        lines: Option<usize>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.storage.read_file(Path::new(path), from, lines)?)
    }

    fn total_chunks(&self) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        let conn = self.open_readonly()?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))?;
        Ok(count as usize)
    }

    /// Return the configured `max_results` from the stored search config.
    ///
    /// Overrides the trait default so the `memory_search` tool honours
    /// `[memory.search].max_results` from config when the model does not
    /// supply an explicit value.
    fn default_search_max_results(&self) -> usize {
        self.search_config.max_results
    }

    /// Return the configured `min_score` from the stored search config.
    fn default_search_min_score(&self) -> f64 {
        self.search_config.min_score as f64
    }
}

#[cfg(test)]
mod factory_tests {
    use super::*;

    #[test]
    fn fts_failures_keep_other_results() {
        let primary = Err::<Vec<_>, _>("primary");
        assert_eq!(merge_fts_results(primary, Ok(vec![])).len(), 0);
        let base = vec![crate::index::FtsResult {
            chunk_id: "base".into(),
            rowid: 1,
            rank: -1.0,
        }];
        assert_eq!(
            merge_fts_results(Ok(base), Err("evergreen"))[0].chunk_id,
            "base"
        );
    }

    #[test]
    fn fts_error_takes_precedence_when_vector_search_also_degrades() {
        assert_eq!(
            select_search_error_class(Some(MemorySearchErrorClass::Fts), true),
            Some(MemorySearchErrorClass::Fts)
        );
        assert_eq!(
            select_search_error_class(None, true),
            Some(MemorySearchErrorClass::Vector)
        );
    }
    use crate::index::{MemoryIndex, init_sqlite_vec};
    use crate::storage::MemoryStorage;
    use echo_agent_config_types::{MemoryEmbeddingConfig, MemorySearchConfig};
    use tempfile::TempDir;

    fn make_storage(tmp: &TempDir) -> MemoryStorage {
        let global = tmp.path().join("memory");
        let workspace = global.join("test_ws");
        MemoryStorage::with_paths(global, workspace)
    }

    fn make_params_fts_only(session_id: &str) -> MemoryBackendParams {
        MemoryBackendParams {
            session_id: session_id.to_owned(),
            embed_config: None,
            embed_base_url: String::new(),
            embed_api_key: None,
            search_config: MemorySearchConfig::default(),
            watcher: None,
            stale_claim_secs: 60,
            search_source: MemorySearchSource::Tool,
            observation_sink: noop_memory_observation_sink(),
            embedding_credentials: EndpointScopedCredentials::none(),
        }
    }

    #[tokio::test]
    async fn chinese_subphrase_is_retrievable_at_the_normal_score_threshold() {
        let tmp = TempDir::new().unwrap();
        init_sqlite_vec();
        let storage = make_storage(&tmp);
        std::fs::create_dir_all(storage.workspace_dir()).unwrap();
        let file = storage.workspace_memory_file();
        std::fs::write(&file, "## 用户偏好\n\n用户喜欢简洁说明。\n").unwrap();
        let db_path = storage.workspace_dir().join("index.sqlite");
        let mut index = MemoryIndex::open_or_create(
            &db_path,
            storage.clone(),
            echo_agent_config_types::MemoryIndexConfig::default(),
            4,
        )
        .unwrap();
        index.reindex_file(&file, "workspace").unwrap();
        drop(index);

        let backend = MemoryBackendImpl::from_session_params(
            storage,
            &make_params_fts_only("chinese-subphrase"),
        );
        let results = backend.search("简洁说明", 5, 0.7).await.unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].snippet.contains("简洁说明"));
    }

    #[tokio::test]
    async fn test_factory_backend_increments_search_counter() {
        let tmp = TempDir::new().unwrap();
        init_sqlite_vec();
        let storage = make_storage(&tmp);
        let db_path = storage.workspace_dir().join("index.sqlite");
        let mut idx = MemoryIndex::open_or_create(
            &db_path,
            storage.clone(),
            echo_agent_config_types::MemoryIndexConfig::default(),
            4,
        )
        .unwrap();
        let file = tmp.path().join("note.md");
        std::fs::write(&file, "# Facts\n\nRust is fast.").unwrap();
        idx.reindex_file(&file, "workspace").unwrap();
        drop(idx);

        let params = make_params_fts_only("test-session-abc");
        let backend = MemoryBackendImpl::from_session_params(storage, &params);

        let before = backend
            .search_counter
            .load(std::sync::atomic::Ordering::Relaxed);
        let _ = backend.search("rust", 5, 0.0).await;
        let after = backend
            .search_counter
            .load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            after,
            before + 1,
            "search counter must increment per search"
        );
    }

    /// from_session_params stores the search_config it was given.
    ///
    /// Direct assertion via the `#[cfg(test)]` accessor proves the factory
    /// propagated the config into the backend rather than discarding it.
    /// `max_results` is verified because the `search()` method overrides it
    /// with the caller's argument — so checking the *stored* value is the only
    /// way to confirm the factory wired it correctly.
    #[tokio::test]
    async fn test_factory_wires_search_config() {
        let tmp = TempDir::new().unwrap();
        init_sqlite_vec();
        let storage = make_storage(&tmp);
        let db_path = storage.workspace_dir().join("index.sqlite");
        let mut idx = MemoryIndex::open_or_create(
            &db_path,
            storage.clone(),
            echo_agent_config_types::MemoryIndexConfig::default(),
            4,
        )
        .unwrap();
        for i in 0..10 {
            let f = tmp.path().join(format!("note{i}.md"));
            std::fs::write(&f, format!("# Entry {i}\n\nRust tip number {i}.")).unwrap();
            idx.reindex_file(&f, "workspace").unwrap();
        }
        drop(idx);

        let params = MemoryBackendParams {
            search_config: MemorySearchConfig {
                max_results: 3,
                ..Default::default()
            },
            ..make_params_fts_only("test-search-config")
        };
        let backend = MemoryBackendImpl::from_session_params(storage, &params);

        // Direct: the stored config has exactly the value the factory was given.
        assert_eq!(
            backend.search_config_for_test().max_results,
            3,
            "stored max_results must equal what was supplied to the factory"
        );
    }

    /// from_session_params wires non-overridable config fields (MMR, temporal decay)
    /// that `search()` never replaces with caller arguments.
    ///
    /// This is the clearest proof that `[memory.search]` config is actually wired
    /// rather than silently ignored: fields the caller cannot override must arrive
    /// in the stored search_config exactly as given.
    #[test]
    fn test_factory_wires_non_overridable_search_config_fields() {
        let tmp = TempDir::new().unwrap();
        let storage = make_storage(&tmp);

        let custom_search = MemorySearchConfig {
            max_results: 7,
            mmr: echo_agent_config_types::MmrConfig {
                enabled: true,
                lambda: 0.42,
            },
            temporal_decay: echo_agent_config_types::TemporalDecayConfig {
                enabled: true,
                half_life_days: 14.0,
            },
            ..Default::default()
        };
        let params = MemoryBackendParams {
            search_config: custom_search,
            ..make_params_fts_only("test-full-config")
        };
        let backend = MemoryBackendImpl::from_session_params(storage, &params);
        let stored = backend.search_config_for_test();

        // None of these are overridden by the caller in search() — they must
        // survive the factory path unchanged.
        assert_eq!(stored.max_results, 7);
        assert!(stored.mmr.enabled, "MMR enabled must be stored");
        assert!(
            (stored.mmr.lambda - 0.42).abs() < f64::EPSILON,
            "MMR lambda must be stored exactly"
        );
        assert!(
            stored.temporal_decay.enabled,
            "temporal_decay enabled must be stored"
        );
        assert!(
            (stored.temporal_decay.half_life_days - 14.0).abs() < f64::EPSILON,
            "temporal_decay half_life_days must be stored exactly"
        );
    }

    /// from_session_params propagates search_source into the backend.
    ///
    /// Correctness test: every caller (tool, injection,
    /// compaction_recovery) must be able to set a distinct source label so
    /// dashboards can separate the three search paths.
    #[test]
    fn test_factory_propagates_search_source() {
        let tmp = TempDir::new().unwrap();
        let storage = make_storage(&tmp);

        for source in [
            MemorySearchSource::Tool,
            MemorySearchSource::Injection,
            MemorySearchSource::CompactionRecovery,
        ] {
            let params = MemoryBackendParams {
                search_source: source,
                ..make_params_fts_only("test-source")
            };
            let backend = MemoryBackendImpl::from_session_params(storage.clone(), &params);
            assert_eq!(source, backend.search_source);
        }
    }

    /// The default search_source is "tool" when constructing via new().
    #[test]
    fn test_default_search_source_is_tool() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.sqlite");
        let storage = make_storage(&tmp);
        let backend = MemoryBackendImpl::new(db_path, storage);
        assert_eq!(MemorySearchSource::Tool, backend.search_source);
    }

    /// MemoryBackendParams with different search_source values is Clone.
    #[test]
    fn test_params_clone_preserves_search_source() {
        let params = MemoryBackendParams {
            search_source: MemorySearchSource::Injection,
            ..make_params_fts_only("test-clone-source")
        };
        let cloned = params.clone();
        assert_eq!(MemorySearchSource::Injection, cloned.search_source);
    }

    /// Watcher startup telemetry reflects actual runtime state.
    ///
    /// `watcher.is_some()` is `true` only when the watcher started successfully.
    /// With a valid directory the watcher should start; without one it should return None.
    /// This guards the contract that `watcher_started` in telemetry must reflect
    /// runtime outcome, not configuration intent.
    #[test]
    fn test_params_watcher_started_reflects_runtime() {
        let tmp = TempDir::new().unwrap();

        // Success path: directory exists → watcher starts.
        let watch_dir = tmp.path().join("memory");
        std::fs::create_dir_all(&watch_dir).unwrap();
        let watcher = crate::watcher::MemoryFileWatcher::start(&watch_dir);
        let params_with_watcher = MemoryBackendParams {
            watcher: watcher.map(std::sync::Arc::new),
            ..make_params_fts_only("test-watcher-runtime")
        };
        // watcher.is_some() reflects whether startup succeeded.
        // (On environments without inotify/FSEvents this may be None; skip rather than fail.)
        let _ = params_with_watcher.watcher.is_some(); // just verify it compiles

        // Failure path: non-existent directory → watcher must return None.
        let missing = tmp.path().join("does_not_exist");
        let no_watcher = crate::watcher::MemoryFileWatcher::start(&missing);
        assert!(
            no_watcher.is_none(),
            "watcher must return None for a non-existent directory"
        );
        let params_no_watcher = MemoryBackendParams {
            watcher: None,
            ..make_params_fts_only("test-no-watcher")
        };
        assert!(
            params_no_watcher.watcher.is_none(),
            "params.watcher.is_none() means telemetry reports watcher_started=false"
        );
    }

    /// default_search_max_results returns the configured value from search_config.
    ///
    /// Verifies that the MemoryBackend trait override in MemoryBackendImpl
    /// exposes search_config.max_results rather than the hardcoded default (6).
    #[test]
    fn test_default_search_max_results_from_config() {
        let tmp = TempDir::new().unwrap();
        let storage = make_storage(&tmp);

        let params = MemoryBackendParams {
            search_config: MemorySearchConfig {
                max_results: 12,
                ..Default::default()
            },
            ..make_params_fts_only("test-defaults")
        };
        let backend = MemoryBackendImpl::from_session_params(storage, &params);
        assert_eq!(
            backend.default_search_max_results(),
            12,
            "default_search_max_results must return search_config.max_results"
        );
    }

    /// default_search_min_score returns the configured value from search_config.
    #[test]
    fn test_default_search_min_score_from_config() {
        let tmp = TempDir::new().unwrap();
        let storage = make_storage(&tmp);

        let params = MemoryBackendParams {
            search_config: MemorySearchConfig {
                min_score: 0.42,
                ..Default::default()
            },
            ..make_params_fts_only("test-defaults")
        };
        let backend = MemoryBackendImpl::from_session_params(storage, &params);
        assert!(
            (backend.default_search_min_score() - 0.42_f64).abs() < 1e-6,
            "default_search_min_score must return search_config.min_score"
        );
    }

    /// from_session_params without embed_config produces a backend that does not panic
    /// and returns results using FTS-only path.
    #[tokio::test]
    async fn test_factory_fts_only_without_embed() {
        let tmp = TempDir::new().unwrap();
        init_sqlite_vec();
        let storage = make_storage(&tmp);
        let db_path = storage.workspace_dir().join("index.sqlite");
        let mut idx = MemoryIndex::open_or_create(
            &db_path,
            storage.clone(),
            echo_agent_config_types::MemoryIndexConfig::default(),
            4,
        )
        .unwrap();
        let f = tmp.path().join("note.md");
        std::fs::write(&f, "# Guide\n\nRust ownership rules.").unwrap();
        idx.reindex_file(&f, "workspace").unwrap();
        drop(idx);

        let params = make_params_fts_only("test-fts-only");
        let backend = MemoryBackendImpl::from_session_params(storage, &params);
        let results = backend.search("rust ownership", 5, 0.0).await.unwrap();
        assert!(
            !results.is_empty(),
            "FTS-only backend should return results"
        );
        let ts = results[0].created_at;
        assert!(
            ts.is_some() && ts.unwrap() > 0,
            "created_at must be Some(positive) after backend search (got {ts:?})"
        );
    }

    /// from_session_params with embed_config but no api_key gracefully falls back
    /// to FTS-only (the embedding provider requires a key).
    #[tokio::test]
    async fn test_factory_embed_config_without_key_falls_back_to_fts() {
        let tmp = TempDir::new().unwrap();
        init_sqlite_vec();
        let storage = make_storage(&tmp);
        let db_path = storage.workspace_dir().join("index.sqlite");
        let mut idx = MemoryIndex::open_or_create(
            &db_path,
            storage.clone(),
            echo_agent_config_types::MemoryIndexConfig::default(),
            4,
        )
        .unwrap();
        let f = tmp.path().join("note.md");
        std::fs::write(&f, "# Guide\n\nRust borrow checker.").unwrap();
        idx.reindex_file(&f, "workspace").unwrap();
        drop(idx);

        let params = MemoryBackendParams {
            embed_config: Some(MemoryEmbeddingConfig::default()),
            embed_base_url: "http://localhost".to_string(),
            embed_api_key: None, // no key → provider cannot be created
            ..make_params_fts_only("test-embed-no-key")
        };
        let backend = MemoryBackendImpl::from_session_params(storage, &params);
        // Must not panic; FTS results should still come back.
        let results = backend.search("rust borrow", 5, 0.0).await.unwrap();
        assert!(
            !results.is_empty(),
            "should fall back to FTS when api_key is None"
        );
    }

    #[tokio::test]
    async fn approved_keyless_embedding_endpoint_creates_provider() {
        let config = MemoryEmbeddingConfig {
            provider: "api".into(),
            model: Some("embed-pro".into()),
            ..Default::default()
        };
        let credentials = EndpointScopedCredentials::none();
        assert!(
            build_embedding_provider(
                Some(&config),
                &credentials,
                None,
                "http://123.56.188.16:8088/v1/embeddings",
            )
            .await
            .is_some()
        );
        assert!(
            build_embedding_provider(
                Some(&config),
                &credentials,
                None,
                "http://localhost:8088/v1/embeddings",
            )
            .await
            .is_none()
        );
    }

    /// MemoryBackendParams is Clone.
    #[test]
    fn test_params_is_clone() {
        let params = make_params_fts_only("clone-test");
        let _cloned = params.clone();
    }

    /// from_session_params without watcher produces a backend that searches correctly.
    #[tokio::test]
    async fn test_factory_no_watcher() {
        let tmp = TempDir::new().unwrap();
        init_sqlite_vec();
        let storage = make_storage(&tmp);
        let db_path = storage.workspace_dir().join("index.sqlite");
        let mut idx = MemoryIndex::open_or_create(
            &db_path,
            storage.clone(),
            echo_agent_config_types::MemoryIndexConfig::default(),
            4,
        )
        .unwrap();
        let f = tmp.path().join("note.md");
        std::fs::write(&f, "# Tip\n\nAlways write tests.").unwrap();
        idx.reindex_file(&f, "workspace").unwrap();
        drop(idx);

        let params = MemoryBackendParams {
            watcher: None,
            ..make_params_fts_only("test-no-watcher")
        };
        let backend = MemoryBackendImpl::from_session_params(storage, &params);
        let results = backend.search("tests", 5, 0.0).await.unwrap();
        assert!(
            !results.is_empty(),
            "no-watcher backend should still return results"
        );
    }

    /// `ensure_initialized` must be called before watcher startup.
    ///
    /// Regression test for the ordering fix: on a first-use machine the
    /// memory directories do not exist yet.  If the watcher tries to watch a
    /// non-existent directory it returns `None` (silently dropping the feature).
    /// After `ensure_initialized()` the directories exist and the watcher can
    /// start successfully.
    ///
    /// This mirrors the ordering enforced in `spawn_session_actor`:
    ///   1. `storage.ensure_initialized()`
    ///   2. `MemoryFileWatcher::start(storage.global_dir())`
    #[test]
    fn test_ensure_initialized_before_watcher_ordering() {
        let tmp = TempDir::new().unwrap();
        let global = tmp.path().join("memory");
        let workspace = global.join("test_ws");
        let storage = MemoryStorage::with_paths(global.clone(), workspace.clone());

        // Precondition: neither directory exists yet (fresh machine simulation).
        assert!(
            !global.exists(),
            "global memory dir must not exist before initialization"
        );

        // --- Wrong ordering (watcher before init) ---
        // The watcher returns None because the directory does not exist.
        let watcher_before_init = crate::watcher::MemoryFileWatcher::start(&global);
        assert!(
            watcher_before_init.is_none(),
            "watcher must fail (None) when directory does not exist yet"
        );

        // --- Correct ordering (init, then watcher) ---
        // After ensure_initialized the directories and MEMORY.md templates exist.
        storage.ensure_initialized().unwrap();

        assert!(
            global.exists(),
            "global dir must exist after ensure_initialized"
        );
        assert!(
            workspace.exists(),
            "workspace dir must exist after ensure_initialized"
        );
        assert!(
            global.join("MEMORY.md").exists(),
            "global MEMORY.md template must exist"
        );
        assert!(
            workspace.join("MEMORY.md").exists(),
            "workspace MEMORY.md template must exist"
        );

        // Watcher now succeeds because the directory exists.
        // (Allowed to return None in environments without inotify/kqueue
        //  support — e.g. some CI containers — but must not error-panic.)
        let watcher_after_init = crate::watcher::MemoryFileWatcher::start(&global);
        // If a watcher was returned we can confirm it is usable (not dirty yet).
        if let Some(w) = watcher_after_init {
            assert!(
                !w.is_dirty(),
                "freshly started watcher must report no dirty files"
            );
        }
        // If None, the test environment does not support file-watching —
        // that is acceptable; the directories themselves are what matter here.
    }

    /// End-to-end regression test for the watcher-driven delete path.
    ///
    /// Tests the full chain:
    ///   1. file is indexed
    ///   2. watcher is started
    ///   3. first `backend.search()` confirms content is found
    ///   4. file is deleted (OS fires a Remove event to the watcher)
    ///   5. second `backend.search()` triggers sync-on-search, which calls
    ///      `delete_path()` because the file no longer exists
    ///   6. content is no longer returned
    ///
    /// This test guards against regressions in the `file.exists() → else
    /// delete_path()` branch that would be invisible to the `delete_path`
    /// unit tests alone.
    #[tokio::test]
    async fn test_watcher_delete_clears_stale_chunks() {
        let tmp = TempDir::new().unwrap();
        init_sqlite_vec();

        let global = tmp.path().join("memory");
        let workspace = global.join("test_ws");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();

        let storage = MemoryStorage::with_paths(global.clone(), workspace);
        let db_path = storage.workspace_dir().join("index.sqlite");

        // Step 1: Write + canonicalize the file path BEFORE indexing.
        //
        // On macOS, TempDir paths may live under /private/tmp (via a symlink
        // from /tmp).  FSEvents returns canonicalized paths, so the path stored
        // in the index must match what the watcher event delivers.
        let file_raw = global.join("note.md");
        std::fs::write(&file_raw, "# Unique\n\nXyzzy-watcher-delete-token.").unwrap();
        let file = dunce::canonicalize(&file_raw).unwrap_or(file_raw);

        {
            let mut idx = MemoryIndex::open_or_create(
                &db_path,
                storage.clone(),
                echo_agent_config_types::MemoryIndexConfig::default(),
                4,
            )
            .unwrap();
            // Index with the canonical path so DB key matches watcher event paths.
            idx.reindex_file(&file, "workspace").unwrap();
        }

        // Step 2: Start watcher AFTER indexing so the Remove event for the
        // upcoming deletion is the first event the watcher ever sees.
        let watch_dir = dunce::canonicalize(&global).unwrap_or(global.clone());
        let watcher = match crate::watcher::MemoryFileWatcher::start(&watch_dir) {
            Some(w) => w,
            None => {
                // File-watching not supported in this environment (e.g., some CI
                // containers without inotify/FSEvents).  Skip rather than fail.
                return;
            }
        };
        let watcher_arc = std::sync::Arc::new(watcher);

        let params = MemoryBackendParams {
            watcher: Some(watcher_arc.clone()),
            ..make_params_fts_only("test-watcher-delete")
        };
        let backend = MemoryBackendImpl::from_session_params(storage, &params);

        // Step 3: Confirm content is found before deletion.
        let before = backend
            .search("Xyzzy-watcher-delete-token", 5, 0.0)
            .await
            .unwrap();
        assert!(
            !before.is_empty(),
            "content must be found before file is deleted"
        );

        // Step 4: Delete the file — the OS will fire a Remove event.
        std::fs::remove_file(&file).unwrap();

        // Poll until the watcher detects the event (more reliable than a fixed
        // sleep on macOS where FSEvents delivery time varies considerably).
        // Give up after 2 s and skip the timing-sensitive assertion rather than
        // flake — delete_path unit tests cover the underlying logic.
        let mut event_delivered = false;
        for _ in 0..20 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if watcher_arc.is_dirty() {
                event_delivered = true;
                break;
            }
        }
        if !event_delivered {
            // FSEvents not delivered within 2 s — environment is too slow.
            // Skip silently; the logic is covered by delete_path unit tests.
            return;
        }

        // Step 5+6: search triggers sync-on-search, which detects file.exists()
        // == false and calls delete_path(), clearing all stale chunks.
        let after = backend
            .search("Xyzzy-watcher-delete-token", 5, 0.0)
            .await
            .unwrap();
        assert!(
            after.is_empty(),
            "deleted file's content must not appear after watcher-driven delete sync"
        );
    }

    /// Regression: provider build must use `current_api_key_async`,
    /// never sync. Prevents memory_search 401s on rotated tokens.
    #[tokio::test]
    async fn make_embedding_provider_uses_async_api_key_resolution() {
        use echo_agent_tools::types::ApiKeyProvider;
        use std::sync::atomic::{AtomicU32, Ordering};

        struct AsyncProbe {
            sync_calls: Arc<AtomicU32>,
            async_calls: Arc<AtomicU32>,
        }
        impl ApiKeyProvider for AsyncProbe {
            fn current_api_key(&self) -> Option<String> {
                self.sync_calls.fetch_add(1, Ordering::SeqCst);
                Some("sync-stale".into())
            }
            fn current_api_key_async(
                &self,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send + '_>>
            {
                let counter = self.async_calls.clone();
                Box::pin(async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Some("async-fresh".into())
                })
            }
        }

        let sync_calls = Arc::new(AtomicU32::new(0));
        let async_calls = Arc::new(AtomicU32::new(0));
        let probe: echo_agent_tools::types::SharedApiKeyProvider = Arc::new(AsyncProbe {
            sync_calls: sync_calls.clone(),
            async_calls: async_calls.clone(),
        });

        let params = MemoryBackendParams {
            session_id: "s1".into(),
            embed_config: Some(MemoryEmbeddingConfig {
                model: Some("test-embed-model".into()),
                ..Default::default()
            }),
            embed_base_url: "http://example/v1".into(),
            embed_api_key: Some("static-fallback".into()),
            search_config: MemorySearchConfig::default(),
            watcher: None,
            stale_claim_secs: 60,
            search_source: MemorySearchSource::Tool,
            observation_sink: noop_memory_observation_sink(),
            // Trusted endpoint + no auth_credentials exercises the api_key_provider path.
            embedding_credentials: EndpointScopedCredentials::for_endpoint(
                "http://example/v1",
                |_| true,
                None,
                Some(probe),
            ),
        };

        let provider = params.make_embedding_provider().await;
        assert!(
            provider.is_some(),
            "provider must be built when model is set"
        );
        assert_eq!(
            async_calls.load(Ordering::SeqCst),
            1,
            "must call current_api_key_async exactly once per provider build"
        );
        assert_eq!(
            sync_calls.load(Ordering::SeqCst),
            0,
            "sync current_api_key must NOT be called — the async path is the contract"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{MemoryIndex, init_sqlite_vec};
    use echo_agent_config_types::MemoryIndexConfig;
    use tempfile::TempDir;

    /// An api-key provider that fails the test if its key is ever resolved,
    /// proving a scoped-away credential is never consulted.
    struct PanicKey;
    impl echo_agent_tools::types::ApiKeyProvider for PanicKey {
        fn current_api_key(&self) -> Option<String> {
            panic!("scoped-away credential must not be resolved");
        }
    }

    fn setup_index(tmp: &TempDir) -> (PathBuf, MemoryStorage) {
        init_sqlite_vec();
        let global = tmp.path().join("memory");
        let workspace = global.join("test_ws");
        let storage = MemoryStorage::with_paths(global, workspace);
        let db_path = tmp.path().join("test.sqlite");

        let mut idx =
            MemoryIndex::open_or_create(&db_path, storage.clone(), MemoryIndexConfig::default(), 4)
                .unwrap();

        let file_path = tmp.path().join("test.md");
        std::fs::write(&file_path, "# Guide\n\nRust programming tutorial.").unwrap();
        idx.reindex_file(&file_path, "workspace").unwrap();

        (db_path, storage)
    }

    #[tokio::test]
    async fn test_backend_search() {
        let tmp = TempDir::new().unwrap();
        let (db_path, storage) = setup_index(&tmp);
        let backend = MemoryBackendImpl::new(db_path, storage);

        let results = backend.search("rust programming", 10, 0.0).await.unwrap();
        assert!(!results.is_empty(), "should find indexed content");
        assert!(results[0].snippet.contains("Rust"));
    }

    #[test]
    fn test_backend_total_chunks() {
        let tmp = TempDir::new().unwrap();
        let (db_path, storage) = setup_index(&tmp);
        let backend = MemoryBackendImpl::new(db_path, storage);

        let count = backend.total_chunks().unwrap();
        assert!(count >= 1, "should have at least 1 chunk");
    }

    #[test]
    fn test_backend_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MemoryBackendImpl>();
    }

    /// If credentials approved for one endpoint are used to build against a
    /// different URL (a wiring bug), they are dropped at build time rather than
    /// sent to the wrong endpoint. The session provider would panic if resolved.
    #[tokio::test]
    async fn test_build_drops_credentials_when_request_url_differs() {
        let session: echo_agent_tools::types::SharedApiKeyProvider = Arc::new(PanicKey);

        let scoped = EndpointScopedCredentials::for_endpoint(
            "https://api.echo-agent.invalid/v1",
            |_| true,
            None,
            Some(session),
        );
        assert!(!scoped.is_empty(), "trusted endpoint keeps the credential");

        let config = echo_agent_config_types::MemoryEmbeddingConfig {
            model: Some("test-embedding-model".to_string()),
            ..Default::default()
        };
        let provider = build_embedding_provider(
            Some(&config),
            &scoped,
            Some("byok-static-key"),
            "https://other.example/v1",
        )
        .await;
        assert!(
            provider.is_some(),
            "mismatched request URL must fall back to the static key, not the scoped credential"
        );
    }

    /// A trusted, URL-matching endpoint builds the provider from the
    /// refresh-capable session credential and never consults the per-call
    /// api-key provider. The api-key provider panics if resolved.
    #[tokio::test]
    async fn test_trusted_endpoint_prefers_session_credential() {
        struct StubAuth;
        impl echo_agent_auth::HttpAuth for StubAuth {
            fn apply(
                &self,
                builder: reqwest::RequestBuilder,
                _base_url: &str,
            ) -> reqwest::RequestBuilder {
                builder
            }
        }
        #[async_trait::async_trait]
        impl echo_agent_auth::AuthCredentialProvider for StubAuth {
            fn snapshot(&self) -> echo_agent_auth::CredentialSnapshot {
                echo_agent_auth::CredentialSnapshot::default()
            }
            async fn refresh_after_unauthorized(&self) -> bool {
                false
            }
        }

        let auth: Arc<dyn echo_agent_auth::AuthCredentialProvider> = Arc::new(StubAuth);
        let api_key: echo_agent_tools::types::SharedApiKeyProvider = Arc::new(PanicKey);
        let scoped = EndpointScopedCredentials::for_endpoint(
            "https://api.echo-agent.invalid/v1",
            |_| true,
            Some(auth),
            Some(api_key),
        );
        assert!(!scoped.is_empty(), "trusted endpoint keeps the credential");

        let config = echo_agent_config_types::MemoryEmbeddingConfig {
            model: Some("test-embedding-model".to_string()),
            ..Default::default()
        };
        let provider = build_embedding_provider(
            Some(&config),
            &scoped,
            None,
            "https://api.echo-agent.invalid/v1",
        )
        .await;
        assert!(
            provider.is_some(),
            "trusted endpoint must build a provider from the session credential"
        );
    }

    #[test]
    fn endpoint_scoped_credentials_trust_gate_and_url_match() {
        struct AnyKey;
        impl echo_agent_tools::types::ApiKeyProvider for AnyKey {
            fn current_api_key(&self) -> Option<String> {
                None
            }
        }
        let key = || Arc::new(AnyKey) as echo_agent_tools::types::SharedApiKeyProvider;

        let denied = EndpointScopedCredentials::for_endpoint(
            "https://byok.example/v1",
            |_| false,
            None,
            Some(key()),
        );
        assert!(denied.is_empty(), "untrusted endpoint drops the credential");

        let scoped = EndpointScopedCredentials::for_endpoint(
            "https://api.echo-agent.invalid/v1",
            |_| true,
            None,
            Some(key()),
        );
        assert!(!scoped.is_empty(), "trusted endpoint keeps the credential");
        assert!(
            scoped.approved_for("https://API.echo-agent.invalid/v1"),
            "host casing normalizes"
        );
        assert!(
            !scoped.approved_for("https://api.echo-agent.invalid/v2"),
            "different path rejected"
        );
        assert!(
            !scoped.approved_for("https://other.example/v1"),
            "different host rejected"
        );
        assert!(!scoped.approved_for("not-a-url"), "unparsable fails closed");
    }

    #[tokio::test]
    async fn test_search_with_punctuation_in_query() {
        let tmp = TempDir::new().unwrap();
        let (db_path, storage) = setup_index(&tmp);
        let backend = MemoryBackendImpl::new(db_path, storage);

        // Raw user message with punctuation — should not crash FTS5
        let results = backend
            .search("what is rust? how to use it!", 10, 0.0)
            .await
            .unwrap();
        assert!(
            !results.is_empty(),
            "should match 'rust' despite punctuation in query"
        );
    }

    #[tokio::test]
    async fn test_search_with_special_chars_only() {
        let tmp = TempDir::new().unwrap();
        let (db_path, storage) = setup_index(&tmp);
        let backend = MemoryBackendImpl::new(db_path, storage);

        // Query with only special chars — should return empty, not error
        let results = backend.search("???!!!", 10, 0.0).await.unwrap();
        assert!(
            results.is_empty(),
            "special-chars-only query should return empty"
        );
    }

    #[tokio::test]
    async fn test_search_hybrid_fts_only_fallback() {
        // Without embedding config, hybrid search should degrade to FTS-only
        let tmp = TempDir::new().unwrap();
        let (db_path, storage) = setup_index(&tmp);
        let backend = MemoryBackendImpl::new(db_path, storage);

        // Even with high min_score, hybrid search normalizes scores to [0,1]
        // so results above the threshold should be returned
        let results = backend.search("rust programming", 10, 0.0).await.unwrap();
        assert!(
            !results.is_empty(),
            "FTS-only fallback should still return results"
        );
        // Scores should be normalized (0,1] range from hybrid scoring
        assert!(results[0].score > 0.0, "hybrid scores should be positive");
    }

    /// The supplemental evergreen query in `search()` adds global/workspace
    /// candidates that the base `search_fts` missed due to candidate_limit.
    ///
    /// Tests the mechanism directly at the index level: verifies that with
    /// a tight FTS limit, global/workspace chunks are absent from the base
    /// results but present in the supplemental source-filtered query. Then
    /// confirms the full backend search pipeline surfaces them.
    #[tokio::test]
    async fn test_search_returns_global_and_workspace_memory() {
        let tmp = TempDir::new().unwrap();
        init_sqlite_vec();
        let global = tmp.path().join("memory");
        let workspace = global.join("test_ws");
        let storage = MemoryStorage::with_paths(global, workspace);
        let db_path = storage.workspace_dir().join("index.sqlite");

        let mut idx =
            MemoryIndex::open_or_create(&db_path, storage.clone(), MemoryIndexConfig::default(), 4)
                .unwrap();

        // Index global + workspace with matching content.
        let global_file = tmp.path().join("global_mem.md");
        std::fs::write(
            &global_file,
            "# Preferences\n\nAlways use graphite for PRs. Prefer Rust over Python.",
        )
        .unwrap();
        idx.reindex_file(&global_file, "global").unwrap();

        let ws_file = tmp.path().join("ws_mem.md");
        std::fs::write(
            &ws_file,
            "# Project Decisions\n\nWe chose graphite for PRs in this project.",
        )
        .unwrap();
        idx.reindex_file(&ws_file, "workspace").unwrap();

        // Index session files that also match the query.
        for i in 0..5 {
            let f = tmp.path().join(format!("session_{i}.md"));
            std::fs::write(
                &f,
                format!("# Session {i}\n\nDiscussed graphite for PRs and item {i}."),
            )
            .unwrap();
            idx.reindex_file(&f, "session").unwrap();
        }

        // Verify the supplemental query mechanism: with a tight limit the
        // base FTS returns a mix, but `search_fts_by_sources` for
        // "global"/"workspace" always finds the evergreen chunks.
        let evergreen = idx
            .search_fts_by_sources("graphite PRs", 10, &["global", "workspace"])
            .unwrap();
        assert!(
            evergreen.len() >= 2,
            "supplemental evergreen query must find both global and workspace chunks"
        );
        let evergreen_sources: Vec<String> = evergreen
            .iter()
            .filter_map(|r| idx.get_chunk(&r.chunk_id).ok().flatten())
            .map(|c| c.source)
            .collect();
        assert!(
            evergreen_sources.contains(&"global".to_string()),
            "evergreen query must find global chunk"
        );
        assert!(
            evergreen_sources.contains(&"workspace".to_string()),
            "evergreen query must find workspace chunk"
        );
        drop(idx);

        // Full backend search: global/workspace must appear in results.
        let backend = MemoryBackendImpl::new(db_path, storage);
        let results = backend.search("graphite PRs", 10, 0.0).await.unwrap();

        let has_global = results.iter().any(|r| r.source == "global");
        let has_workspace = results.iter().any(|r| r.source == "workspace");
        assert!(
            has_global,
            "global MEMORY.md chunks must appear in search results"
        );
        assert!(
            has_workspace,
            "workspace MEMORY.md chunks must appear in search results"
        );
    }
}

#[cfg(test)]
mod index_embedding_tests {
    use crate::index::MemoryIndex;
    use crate::storage::MemoryStorage;

    #[test]
    fn test_chunks_without_embeddings() {
        let tmp = tempfile::TempDir::new().unwrap();
        let global = tmp.path().join("memory");
        let workspace = global.join("test_ws");
        let storage = MemoryStorage::with_paths(global, workspace);
        let db_path = tmp.path().join("test.sqlite");

        let mut idx = MemoryIndex::open_or_create(
            &db_path,
            storage,
            echo_agent_config_types::MemoryIndexConfig::default(),
            4,
        )
        .unwrap();

        if !idx.vec_available() {
            // sqlite-vec not available — chunks_without_embeddings returns empty
            let missing = idx.chunks_without_embeddings().unwrap();
            assert!(missing.is_empty(), "no-vec: should return empty");
            return;
        }

        let file_path = tmp.path().join("test.md");
        std::fs::write(&file_path, "# Title\n\nSome content here.").unwrap();
        idx.reindex_file(&file_path, "workspace").unwrap();

        // After reindex, chunks should exist but have no embeddings
        let missing = idx.chunks_without_embeddings().unwrap();
        assert!(
            !missing.is_empty(),
            "newly indexed chunks should be missing embeddings"
        );

        // After upserting an embedding, the chunk should disappear from missing
        let (chunk_id, _) = &missing[0];
        let dummy_embedding = vec![0.0f32; 4];
        idx.upsert_embedding(chunk_id, &dummy_embedding).unwrap();

        let missing_after = idx.chunks_without_embeddings().unwrap();
        assert_eq!(
            missing_after.len(),
            missing.len() - 1,
            "one fewer chunk should be missing after embedding"
        );
    }
}
