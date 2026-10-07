//! Hybrid search combining FTS5 BM25 + sqlite-vec KNN + temporal decay + source weighting + MMR.
//!
//! The search pipeline:
//! 1. FTS5 keyword search (always available)
//! 2. Vector KNN search (when sqlite-vec + embeddings are available)
//! 3. Merge results by chunk_id, normalize scores to [0,1]
//! 4. Skip content-free chunks: empty/boilerplate templates (the
//!    auto-generated `MEMORY.md` stub) never appear in results / injection
//! 5. Apply temporal decay: evergreen sources (global, workspace) are exempt;
//!    session chunks decay with exponential half-life:
//!    `decayed = base × e^(-λ × age_days)` where `λ = ln(2) / half_life_days`
//! 6. Apply source weights + access-frequency boost, filter by `min_score`,
//!    rank on the unclamped score, then clamp the stored display score to [0,1]
//! 7. MMR diversity re-ranking (opt-in, penalizes redundant results)
//! 8. Limit to `max_results`
//!
//! Graceful degradation: if vector search is unavailable, falls back to FTS-only
//! with `text_weight = 1.0`.

use std::collections::HashMap;

use super::embedding::EmbeddingProvider;
use super::index::MemoryIndex;
use super::observation::MemoryRetrievalMode;
use echo_agent_config_types::MemorySearchConfig;

pub(super) enum QueryEmbedding {
    FtsOnly,
    Hybrid(Vec<f32>),
    EmbeddingFallback,
}
impl QueryEmbedding {
    pub fn embedding(&self) -> Option<&[f32]> {
        if let Self::Hybrid(value) = self {
            Some(value)
        } else {
            None
        }
    }

    pub fn mode(&self) -> MemoryRetrievalMode {
        match self {
            Self::FtsOnly => MemoryRetrievalMode::FtsOnly,
            Self::Hybrid(_) => MemoryRetrievalMode::Hybrid,
            Self::EmbeddingFallback => MemoryRetrievalMode::EmbeddingFallback,
        }
    }
}

pub(super) async fn resolve_query_embedding(
    provider: Option<&dyn EmbeddingProvider>,
    is_vector_index_available: bool,
    query: &str,
) -> QueryEmbedding {
    let Some(provider) = provider.filter(|_| is_vector_index_available) else {
        return QueryEmbedding::FtsOnly;
    };
    match tokio::time::timeout(
        super::QUERY_EMBEDDING_TIMEOUT,
        provider.embed_batch(&[query]),
    )
    .await
    {
        Ok(Ok(embeddings)) => embeddings
            .into_iter()
            .next()
            .map_or(QueryEmbedding::EmbeddingFallback, QueryEmbedding::Hybrid),
        Ok(Err(error)) => {
            tracing::warn!(target: crate::MEMORY_LOG_TARGET, %error, "embedding query failed");
            QueryEmbedding::EmbeddingFallback
        }
        Err(_) => {
            tracing::warn!(target: crate::MEMORY_LOG_TARGET, "embedding query timed out");
            QueryEmbedding::EmbeddingFallback
        }
    }
}

/// A search result with merged scoring from FTS and vector search.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub chunk_id: String,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub score: f64,
    pub snippet: String,
    pub source: String,
    pub created_at: i64,
}

pub(super) struct SearchMerge {
    pub results: Vec<SearchResult>,
    pub is_vector_degraded: bool,
}

/// Returns `true` for sources that contain curated long-term knowledge
/// and should not be penalized by temporal decay.
///
/// Evergreen: `"global"` (MEMORY.md), `"workspace"` (project MEMORY.md).
/// Decaying: `"session"` (auto-generated session logs).
fn is_evergreen_source(source: &str) -> bool {
    matches!(source, "global" | "workspace")
}

/// Returns `true` when a chunk is an empty/boilerplate template (e.g. the
/// auto-generated `MEMORY.md` stub) and should be filtered out.
///
/// True when the chunk is structurally empty, or matches a known scaffold
/// template via [`super::dream::is_scaffold_template`]. The marker branch is
/// scoped to evergreen sources, where scaffold templates live, so a session
/// chunk that merely quotes a marker phrase is kept.
fn is_content_free(text: &str, source: &str) -> bool {
    is_structurally_empty(text)
        || (is_evergreen_source(source) && super::dream::is_scaffold_template(text))
}

/// Returns `true` when `text` has no substantive content after stripping ATX
/// headings, HTML comments, and whitespace. Blockquotes are NOT stripped —
/// they are real user content.
fn is_structurally_empty(text: &str) -> bool {
    // Fast path: no comment marker means no multi-line span to strip.
    if !text.contains("<!--") {
        return lines_are_scaffolding(text);
    }

    // Strip HTML comments (which may span lines) before the line scan.
    let mut without_comments = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        match rest[start + "<!--".len()..].find("-->") {
            Some(end) => {
                without_comments.push_str(&rest[..start]);
                let after = start + "<!--".len() + end + "-->".len();
                rest = &rest[after..];
            }
            None => {
                // Unterminated comment: keep the remainder as literal text so a
                // comment split across a chunk boundary can't drop real content.
                without_comments.push_str(rest);
                rest = "";
                break;
            }
        }
    }
    without_comments.push_str(rest);

    lines_are_scaffolding(&without_comments)
}

/// Returns `true` when every non-blank line is an ATX heading (per
/// [`super::chunker::header_level`]). Any other non-blank line is content.
fn lines_are_scaffolding(text: &str) -> bool {
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if super::chunker::header_level(line).is_some() {
            continue;
        }
        return false;
    }
    true
}

/// Compute the temporal decay multiplier for a chunk.
///
/// - Evergreen sources → `1.0` (no decay).
/// - Session sources → exponential decay: `e^(-λ × age_days)` where
///   `λ = ln(2) / half_life_days`. Score halves every `half_life_days`.
/// - `half_life = None` → decay disabled, returns `1.0` for all sources.
fn temporal_decay_multiplier(
    source: &str,
    created_at: i64,
    now_secs: i64,
    half_life_days: Option<f64>,
) -> f64 {
    let Some(half_life) = half_life_days else {
        return 1.0;
    };
    if is_evergreen_source(source) {
        return 1.0;
    }
    if half_life <= 0.0 {
        return 1.0;
    }
    // No upper clamp on age: with exponential decay a 2-year-old chunk at
    // 30-day half-life scores ~6e-8, well below any reasonable min_score.
    let age_days = ((now_secs - created_at.max(0)) as f64 / 86400.0).max(0.0);
    let lambda = f64::ln(2.0) / half_life;
    (-lambda * age_days).exp()
}

/// Run a hybrid search across the memory index.
///
/// Combines FTS5 keyword search with optional vector KNN similarity.
/// Falls back to FTS-only when vector search is unavailable.
///
/// Structured so that `&MemoryIndex` is never held across `.await` points,
/// allowing the caller's future to be `Send` even though `MemoryIndex` is `!Sync`.
#[tracing::instrument(name = "memory.hybrid_search", skip_all, fields(
    max_results = config.max_results,
))]
pub async fn hybrid_search(
    index: &MemoryIndex,
    embedding_provider: Option<&dyn EmbeddingProvider>,
    query: &str,
    config: &MemorySearchConfig,
) -> Result<Vec<SearchResult>, Box<dyn std::error::Error>> {
    if let Some(provider) = embedding_provider {
        let fingerprint = provider.fingerprint();
        if index.ensure_embedding_fingerprint(&fingerprint)? {
            // Rebuild immediately so a model switch degrades for at most this
            // operation rather than leaving the vector index empty indefinitely.
            super::embed_missing_chunks(index, provider).await;
        }
    }
    let candidate_limit = config.max_results * 3;

    // Phase 1 (sync): FTS search + supplemental evergreen query so
    // global/workspace chunks aren't crowded out by session volume.
    let mut fts_results = index.search_fts(query, candidate_limit).unwrap_or_default();
    let evergreen = index
        .search_fts_by_sources(query, candidate_limit, &["global", "workspace"])
        .unwrap_or_default();
    let existing: std::collections::HashSet<String> =
        fts_results.iter().map(|r| r.chunk_id.clone()).collect();
    for r in evergreen {
        if !existing.contains(&r.chunk_id) {
            fts_results.push(r);
        }
    }
    // Phase 2 (async): embed query — no &index borrow here
    let query_embedding =
        resolve_query_embedding(embedding_provider, index.vec_available(), query).await;

    // Phase 3 (sync): vector search + scoring + merge
    Ok(hybrid_search_merge(
        index,
        query,
        fts_results,
        query_embedding.embedding(),
        config,
    )?
    .results)
}

/// Synchronous merge phase: vector search (if embedding provided), score
/// normalization, temporal decay, source weighting, MMR, and truncation.
pub(super) fn hybrid_search_merge(
    index: &MemoryIndex,
    query: &str,
    fts_results: Vec<super::index::FtsResult>,
    query_embedding: Option<&[f32]>,
    config: &MemorySearchConfig,
) -> Result<SearchMerge, Box<dyn std::error::Error>> {
    let candidate_limit = config.max_results * 3;

    let (vec_results, is_vector_degraded) = if let Some(embedding) = query_embedding {
        match index.vector_search(embedding, candidate_limit) {
            Ok(results) => (results, false),
            Err(error) => {
                tracing::warn!(target: crate::MEMORY_LOG_TARGET, %error, "vector search failed");
                (vec![], true)
            }
        }
    } else {
        (vec![], false)
    };

    // Normalize and merge scores.
    //
    // Per-chunk scoring strategy:
    //   - Chunks with BOTH FTS and vector matches: weighted combination
    //     (text_weight × fts_score + vector_weight × vec_score)
    //   - Chunks with ONLY FTS matches: score = fts_score (full weight, not
    //     penalized to text_weight just because other chunks have vectors)
    //   - Chunks with ONLY vector matches: score = vector_weight × vec_score
    //
    // This ensures FTS-only chunks (e.g., global MEMORY.md with no embedding
    // match) can still score high enough to pass min_score.
    let mut fts_scores: HashMap<String, f64> = HashMap::new();
    let mut vec_scores: HashMap<String, f64> = HashMap::new();

    // Normalize FTS BM25 scores to [0,1] (BM25 scores are negative in FTS5,
    // more negative = better match)
    if !fts_results.is_empty() {
        let min_rank = fts_results
            .iter()
            .map(|r| r.rank)
            .fold(f64::INFINITY, f64::min);
        let max_rank = fts_results
            .iter()
            .map(|r| r.rank)
            .fold(f64::NEG_INFINITY, f64::max);
        // Relative rank alone is unsafe: a lone weak OR-term match otherwise
        // becomes a misleading 1.0. Blend it with absolute query-term coverage.
        let range = (max_rank - min_rank).max(f64::EPSILON);

        for r in &fts_results {
            // FTS5 rank: more negative = better. Normalize so best = 1.0
            let relative = 1.0 - (r.rank - min_rank) / range;
            let coverage = index
                .get_chunk(&r.chunk_id)
                .ok()
                .flatten()
                .map(|chunk| lexical_query_coverage(query, &chunk.text))
                .unwrap_or(0.0);
            let normalized = relative * (0.25 + 0.75 * coverage);
            fts_scores.insert(r.chunk_id.clone(), normalized);
        }
    }

    // Normalize vector distances to [0,1] similarity using absolute scale.
    //
    // For normalized embeddings, L2 distance ranges from 0 (identical) to 2
    // (opposite). Using `similarity = 1.0 - distance / 2.0` maps this to
    // [0, 1] on an absolute scale, avoiding the compression problem where
    // relative normalization (`1 - d/max_d`) collapses all scores to near-zero
    // when candidates cluster in a narrow distance band (common for
    // high-dimensional embeddings due to concentration of measure).
    //
    // The constant `2.0` is the theoretical maximum L2 distance between two
    // unit-norm vectors: ||u - v||₂ = sqrt(2 - 2·cos(θ)) ≤ sqrt(4) = 2.
    const MAX_L2_DISTANCE: f64 = 2.0;
    for (chunk_id, distance) in &vec_results {
        let similarity = (1.0 - (*distance as f64 / MAX_L2_DISTANCE)).clamp(0.0, 1.0);
        vec_scores.insert(chunk_id.clone(), similarity);
    }

    // Merge per-chunk scores: use max(fts_only, hybrid) so FTS-only chunks
    // are never penalized by the existence of unrelated vector results.
    let mut scores: HashMap<String, f64> = HashMap::new();
    let text_weight = config.text_weight as f64;
    let vector_weight = config.vector_weight as f64;

    // Collect all unique chunk IDs across both result sets.
    let all_chunk_ids: std::collections::HashSet<&String> =
        fts_scores.keys().chain(vec_scores.keys()).collect();

    for chunk_id in all_chunk_ids {
        let fts = fts_scores.get(chunk_id).copied().unwrap_or(0.0);
        let vec = vec_scores.get(chunk_id).copied().unwrap_or(0.0);

        let score = if fts > 0.0 && vec > 0.0 {
            // Both signals available: weighted combination, but never worse
            // than the FTS score alone (since text_weight < 1.0 would otherwise
            // penalize a strong keyword match).
            let hybrid = text_weight * fts + vector_weight * vec;
            hybrid.max(fts)
        } else if fts > 0.0 {
            // FTS-only: full FTS score (not penalized to text_weight)
            fts
        } else {
            // Vector-only: weighted vector score
            vector_weight * vec
        };

        scores.insert(chunk_id.clone(), score);
    }

    // Apply temporal decay and source weights, build results
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let half_life = config.effective_half_life_days();

    // Pairs the unclamped ranking score with each result (see the raw_score /
    // display_score split below).
    let mut ranked: Vec<(f64, SearchResult)> = Vec::new();

    for (chunk_id, base_score) in &scores {
        let Some(chunk) = index.get_chunk(chunk_id).ok().flatten() else {
            continue;
        };

        // Filter at search time (not index time) so already-indexed stubs are
        // excluded without requiring a reindex.
        if is_content_free(&chunk.text, &chunk.source) {
            continue;
        }

        let decay_multiplier =
            temporal_decay_multiplier(&chunk.source, chunk.created_at, now_secs, half_life);

        let source_weight = config
            .source_weights
            .get(&chunk.source)
            .copied()
            .unwrap_or(1.0) as f64;

        // Access-frequency boost: chunks retrieved before score slightly higher.
        //
        // Uses ln(1 + access_count) so:
        // - 0 accesses → boost = 1.0 (no penalty)
        // - 1 access   → boost ≈ 1.035
        // - 10 accesses → boost ≈ 1.120
        // - 100 accesses → boost ≈ 1.230
        //
        // The 0.05 scale factor keeps the boost modest so retrieval relevance
        // (BM25 / vector similarity) remains the primary ranking signal.
        let access_boost = 1.0 + (chunk.access_count as f64).ln_1p() * 0.05;
        // access_boost is an unbounded multiplier (> 1.0), so the product can
        // exceed 1.0 for top evergreen chunks. Rank on the unclamped raw_score
        // (so the boost still orders chunks that would otherwise both clamp to
        // 1.0), but store the clamped display_score so it reads as a [0,1]
        // similarity. Gating on display_score keeps the threshold and the
        // stored value in agreement.
        let raw_score = base_score * decay_multiplier * source_weight * access_boost;
        let display_score = raw_score.clamp(0.0, 1.0);

        if display_score >= config.min_score as f64 {
            ranked.push((
                raw_score,
                SearchResult {
                    chunk_id: chunk_id.clone(),
                    path: chunk.path.clone(),
                    start_line: chunk.start_line,
                    end_line: chunk.end_line,
                    score: display_score,
                    snippet: chunk.text.clone(),
                    source: chunk.source.clone(),
                    created_at: chunk.created_at,
                },
            ));
        }
    }

    ranked.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    // Split into aligned `relevance` (unclamped) + `results` (clamped) vectors
    // so MMR can rank on relevance. Only build `relevance` when MMR is enabled;
    // otherwise `mmr_rerank` early-returns before reading it.
    let mmr_enabled = config.mmr.enabled;
    let mut relevance: Vec<f64> = if mmr_enabled {
        Vec::with_capacity(ranked.len())
    } else {
        Vec::new()
    };
    let mut results: Vec<SearchResult> = Vec::with_capacity(ranked.len());
    for (raw_score, result) in ranked {
        if mmr_enabled {
            relevance.push(raw_score);
        }
        results.push(result);
    }

    // MMR diversity re-ranking (opt-in, applied before truncation)
    super::mmr::mmr_rerank(&mut results, &relevance, &config.mmr);

    results.truncate(config.max_results);

    Ok(SearchMerge {
        results,
        is_vector_degraded,
    })
}

/// Fraction of meaningful query terms present in a candidate. CJK phrases are
/// decomposed into character bigrams so partial Chinese matches do not receive
/// the same confidence as full-phrase matches.
fn lexical_query_coverage(query: &str, text: &str) -> f64 {
    let mut terms = Vec::new();
    for keyword in super::query_expansion::extract_keywords(query) {
        let chars: Vec<char> = keyword.chars().collect();
        if chars.len() > 2 && chars.iter().any(|character| is_cjk(*character)) {
            terms.extend(chars.windows(2).map(|pair| pair.iter().collect::<String>()));
        } else {
            terms.push(keyword);
        }
    }
    if terms.is_empty() {
        return 1.0;
    }
    let haystack = text.to_lowercase();
    let matches = terms
        .iter()
        .filter(|term| haystack.contains(term.as_str()))
        .count();
    matches as f64 / terms.len() as f64
}

fn is_cjk(character: char) -> bool {
    matches!(
        character,
        '\u{3400}'..='\u{4DBF}' | '\u{4E00}'..='\u{9FFF}' | '\u{F900}'..='\u{FAFF}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedding::MockEmbeddingProvider;
    use crate::index::{MemoryIndex, init_sqlite_vec};
    use crate::storage::MemoryStorage;
    use echo_agent_config_types::{MemoryIndexConfig, MemorySearchConfig};
    use tempfile::TempDir;

    struct FailingEmbeddingProvider;

    #[async_trait::async_trait]
    impl EmbeddingProvider for FailingEmbeddingProvider {
        async fn embed_batch(
            &self,
            _texts: &[&str],
        ) -> Result<Vec<Vec<f32>>, Box<dyn std::error::Error>> {
            Err("query embedding failed".into())
        }

        fn model_name(&self) -> &str {
            "failing"
        }

        fn dimensions(&self) -> usize {
            4
        }
    }

    fn test_index(tmp: &TempDir) -> MemoryIndex {
        init_sqlite_vec();
        let global = tmp.path().join("memory");
        let workspace = global.join("test_ws");
        let storage = MemoryStorage::with_paths(global, workspace);
        let db_path = tmp.path().join("test.sqlite");
        MemoryIndex::open_or_create(&db_path, storage, MemoryIndexConfig::default(), 4).unwrap()
    }

    #[test]
    fn vector_failure_falls_back_to_fts() {
        let tmp = TempDir::new().unwrap();
        let mut index = test_index(&tmp);
        let file = tmp.path().join("test.md");
        std::fs::write(&file, "# Guide\n\nRust programming.").unwrap();
        index.reindex_file(&file, "workspace").unwrap();
        index.db().execute("DROP TABLE chunks_vec", []).unwrap();
        let config = MemorySearchConfig {
            min_score: 0.0,
            ..Default::default()
        };
        let merged = hybrid_search_merge(
            &index,
            "rust",
            index.search_fts("rust", 3).unwrap(),
            Some(&[0.0; 4]),
            &config,
        )
        .unwrap();
        assert!(merged.is_vector_degraded && !merged.results.is_empty());
    }

    #[tokio::test]
    async fn test_hybrid_search_fts_only() {
        let tmp = TempDir::new().unwrap();
        let mut idx = test_index(&tmp);

        let file_path = tmp.path().join("test.md");
        std::fs::write(&file_path, "# Guide\n\nRust programming language tutorial.").unwrap();
        idx.reindex_file(&file_path, "workspace").unwrap();

        let config = MemorySearchConfig::default();
        let results = hybrid_search(&idx, None, "rust programming", &config)
            .await
            .unwrap();

        assert!(!results.is_empty(), "should find results via FTS");
        assert!(results[0].snippet.contains("Rust"));
        assert!(
            results[0].created_at > 0,
            "created_at must propagate from ChunkRecord (got {})",
            results[0].created_at,
        );
    }

    #[tokio::test]
    async fn test_hybrid_search_no_match() {
        let tmp = TempDir::new().unwrap();
        let mut idx = test_index(&tmp);

        let file_path = tmp.path().join("test.md");
        std::fs::write(&file_path, "# Guide\n\nPython tutorial.").unwrap();
        idx.reindex_file(&file_path, "workspace").unwrap();

        let config = MemorySearchConfig::default();
        let results = hybrid_search(&idx, None, "haskell monads", &config)
            .await
            .unwrap();

        assert!(results.is_empty(), "should not find unrelated content");
    }

    #[tokio::test]
    async fn test_hybrid_search_respects_max_results() {
        let tmp = TempDir::new().unwrap();
        let mut idx = test_index(&tmp);

        // Create multiple matching files
        for i in 0..10 {
            let file_path = tmp.path().join(format!("test_{i}.md"));
            std::fs::write(&file_path, format!("# Doc {i}\n\nRust content {i}.")).unwrap();
            idx.reindex_file(&file_path, "workspace").unwrap();
        }

        let config = MemorySearchConfig {
            max_results: 3,
            min_score: 0.0, // accept all
            ..Default::default()
        };

        let results = hybrid_search(&idx, None, "rust content", &config)
            .await
            .unwrap();

        assert!(
            results.len() <= 3,
            "should respect max_results, got {}",
            results.len()
        );
    }

    #[tokio::test]
    async fn test_hybrid_search_empty_index() {
        let tmp = TempDir::new().unwrap();
        let idx = test_index(&tmp);

        let config = MemorySearchConfig::default();
        let results = hybrid_search(&idx, None, "anything", &config)
            .await
            .unwrap();

        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_hybrid_search_source_weights() {
        let tmp = TempDir::new().unwrap();
        let mut idx = test_index(&tmp);

        let ws_file = tmp.path().join("ws.md");
        std::fs::write(&ws_file, "# WS\n\nRust workspace content.").unwrap();
        idx.reindex_file(&ws_file, "workspace").unwrap();

        let gl_file = tmp.path().join("gl.md");
        std::fs::write(&gl_file, "# GL\n\nRust global content.").unwrap();
        idx.reindex_file(&gl_file, "global").unwrap();

        let config = MemorySearchConfig {
            min_score: 0.0,
            ..Default::default()
        };

        let results = hybrid_search(&idx, None, "rust content", &config)
            .await
            .unwrap();

        // Both should be found; workspace should score higher due to source_weight
        if results.len() >= 2 {
            let ws_result = results.iter().find(|r| r.source == "workspace");
            let gl_result = results.iter().find(|r| r.source == "global");
            if let (Some(ws), Some(gl)) = (ws_result, gl_result) {
                assert!(
                    (ws.score - gl.score).abs() < 0.01,
                    "workspace and global should score equally (both weight=1.0)"
                );
            }
        }
    }

    #[tokio::test]
    async fn query_embedding_failure_uses_embedding_fallback_mode() {
        let resolved = resolve_query_embedding(
            Some(&FailingEmbeddingProvider),
            true,
            "private query content",
        )
        .await;

        assert_eq!(MemoryRetrievalMode::EmbeddingFallback, resolved.mode());
        assert!(resolved.embedding().is_none());
    }

    #[tokio::test]
    async fn test_hybrid_search_with_vector_and_fts() {
        let tmp = TempDir::new().unwrap();
        let mut idx = test_index(&tmp);
        let mock = MockEmbeddingProvider { dimensions: 4 };

        // Index a file and embed its chunks
        let file_path = tmp.path().join("test.md");
        std::fs::write(&file_path, "# Guide\n\nRust programming language tutorial.").unwrap();
        idx.reindex_file(&file_path, "workspace").unwrap();

        // Embed the chunk
        let path_str = file_path.to_string_lossy().to_string();
        let chunk_id = format!("{path_str}:0");
        let chunk = idx.get_chunk(&chunk_id).unwrap().unwrap();
        let embeddings = mock.embed_batch(&[&chunk.text]).await.unwrap();
        idx.upsert_embedding(&chunk_id, &embeddings[0]).unwrap();

        // Search — should use both FTS and vector paths
        let config = MemorySearchConfig {
            min_score: 0.0,
            ..Default::default()
        };

        let results = hybrid_search(
            &idx,
            Some(&mock as &dyn EmbeddingProvider),
            "rust programming",
            &config,
        )
        .await
        .unwrap();

        assert!(!results.is_empty(), "hybrid search should find results");
        assert!(results[0].snippet.contains("Rust"));
        // With both FTS and vector results, score should combine both weights
        assert!(results[0].score > 0.0, "score should be positive");
    }

    // -----------------------------------------------------------------------
    // Temporal decay unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_is_evergreen_source() {
        assert!(is_evergreen_source("global"));
        assert!(is_evergreen_source("workspace"));
        assert!(!is_evergreen_source("session"));
        assert!(!is_evergreen_source("unknown"));
        assert!(!is_evergreen_source(""));
    }

    #[test]
    fn test_decay_disabled_returns_one() {
        assert_eq!(
            temporal_decay_multiplier("session", 0, 86400 * 90, None),
            1.0
        );
        assert_eq!(
            temporal_decay_multiplier("global", 0, 86400 * 90, None),
            1.0
        );
    }

    #[test]
    fn test_evergreen_sources_never_decay() {
        let now = 86400 * 365; // 1 year
        let created = 0; // created at epoch
        let half_life = Some(30.0);

        assert_eq!(
            temporal_decay_multiplier("global", created, now, half_life),
            1.0
        );
        assert_eq!(
            temporal_decay_multiplier("workspace", created, now, half_life),
            1.0
        );
    }

    #[test]
    fn test_session_chunks_decay_with_half_life() {
        let half_life = Some(30.0);
        let now = 86400 * 30; // 30 days after epoch
        let created = 0;

        let multiplier = temporal_decay_multiplier("session", created, now, half_life);
        // After exactly one half-life, multiplier should be ~0.5
        assert!(
            (multiplier - 0.5).abs() < 0.01,
            "30-day-old session chunk with 30-day half-life should score ~0.5, got {multiplier}"
        );
    }

    #[test]
    fn test_decay_at_two_half_lives() {
        let half_life = Some(30.0);
        let now = 86400 * 60; // 60 days
        let created = 0;

        let multiplier = temporal_decay_multiplier("session", created, now, half_life);
        assert!(
            (multiplier - 0.25).abs() < 0.01,
            "60-day-old session chunk should score ~0.25, got {multiplier}"
        );
    }

    #[test]
    fn test_fresh_session_chunk_no_decay() {
        let half_life = Some(30.0);
        let now = 1_000_000;
        let created = now; // just created

        let multiplier = temporal_decay_multiplier("session", created, now, half_life);
        assert!(
            (multiplier - 1.0).abs() < f64::EPSILON,
            "brand-new session chunk should have multiplier ~1.0, got {multiplier}"
        );
    }

    #[test]
    fn test_zero_half_life_returns_one() {
        let multiplier = temporal_decay_multiplier("session", 0, 86400 * 30, Some(0.0));
        assert_eq!(multiplier, 1.0, "zero half-life should disable decay");
    }

    #[test]
    fn test_negative_half_life_returns_one() {
        let multiplier = temporal_decay_multiplier("session", 0, 86400 * 30, Some(-5.0));
        assert_eq!(multiplier, 1.0, "negative half-life should disable decay");
    }

    #[test]
    fn test_future_created_at_no_negative_age() {
        let now = 1_000_000;
        let created = now + 86400; // 1 day in the future (clock skew)
        let half_life = Some(30.0);

        let multiplier = temporal_decay_multiplier("session", created, now, half_life);
        assert!(
            (multiplier - 1.0).abs() < f64::EPSILON,
            "future created_at should clamp age to 0, got {multiplier}"
        );
    }

    #[tokio::test]
    async fn test_hybrid_search_old_session_ranks_below_evergreen() {
        let tmp = TempDir::new().unwrap();
        let mut idx = test_index(&tmp);

        // Index a workspace file (evergreen) and a session file (decays)
        let ws_file = tmp.path().join("ws.md");
        std::fs::write(&ws_file, "# WS\n\nRust workspace content about memory.").unwrap();
        idx.reindex_file(&ws_file, "workspace").unwrap();

        let sess_file = tmp.path().join("sess.md");
        std::fs::write(&sess_file, "# Sess\n\nRust session content about memory.").unwrap();
        idx.reindex_file(&sess_file, "session").unwrap();

        // Backdate the session chunk's created_at by 60 days (2 half-lives)
        let sixty_days_ago = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            - 86400 * 60;
        let sess_path = sess_file.to_string_lossy().to_string();
        idx.db()
            .execute(
                "UPDATE chunks SET created_at = ?1 WHERE path = ?2",
                rusqlite::params![sixty_days_ago, sess_path],
            )
            .unwrap();

        // Equal source weights to isolate temporal decay
        let mut source_weights = std::collections::HashMap::new();
        source_weights.insert("workspace".to_string(), 1.0);
        source_weights.insert("session".to_string(), 1.0);

        let config = MemorySearchConfig {
            min_score: 0.0,
            source_weights,
            ..Default::default()
        };

        let results = hybrid_search(&idx, None, "rust memory", &config)
            .await
            .unwrap();

        assert!(results.len() >= 2, "should find both chunks");
        let ws = results.iter().find(|r| r.source == "workspace").unwrap();
        let sess = results.iter().find(|r| r.source == "session").unwrap();

        // Workspace (evergreen) should rank above the 60-day-old session chunk.
        // At 2 half-lives the session chunk decays to ~0.25× its base score,
        // while the workspace chunk stays at 1.0×.
        assert!(
            ws.score > sess.score,
            "evergreen workspace ({:.4}) should outscore 60-day-old session ({:.4})",
            ws.score,
            sess.score,
        );
    }

    // -----------------------------------------------------------------------
    // PR-8: access-frequency boost tests
    // -----------------------------------------------------------------------

    /// A chunk with access_count > 0 scores higher than an identical chunk
    /// with access_count = 0, all else equal.
    #[tokio::test]
    async fn test_access_boost_raises_frequently_accessed_chunks() {
        let tmp = TempDir::new().unwrap();
        let mut idx = test_index(&tmp);

        // Two files with nearly identical content; chunk B is accessed once.
        let fa = tmp.path().join("chunk_a.md");
        let fb = tmp.path().join("chunk_b.md");
        std::fs::write(&fa, "# Rust\n\nRust ownership model explained.").unwrap();
        std::fs::write(&fb, "# Rust\n\nRust ownership model explained.").unwrap();
        idx.reindex_file(&fa, "workspace").unwrap();
        idx.reindex_file(&fb, "workspace").unwrap();

        // Record one access for chunk B.
        let chunk_b_id = format!("{}:0", fb.to_string_lossy());
        idx.record_access(&chunk_b_id).unwrap();

        // Use the DEFAULT config (all source_weights = 1.0). Both chunks
        // normalize to base_score = 1.0 as the top FTS matches, so their
        // display scores both clamp to 1.0 — but ranking is performed on the
        // UNCLAMPED score, so the access boost still orders the accessed chunk
        // first. This exercises the common default-config path where the clamp
        // would otherwise make the boost inert.
        let config = MemorySearchConfig::default();
        let results = hybrid_search_merge(
            &idx,
            "rust ownership",
            idx.search_fts("rust ownership", 10).unwrap(),
            None,
            &config,
        )
        .unwrap()
        .results;

        // Both chunks must be returned (no vacuous "inconclusive → pass" path).
        let pos_a = results
            .iter()
            .position(|r| r.path == fa.to_string_lossy().as_ref())
            .expect("chunk A must be returned");
        let pos_b = results
            .iter()
            .position(|r| r.path == fb.to_string_lossy().as_ref())
            .expect("chunk B must be returned");

        // The accessed chunk (B) must rank ahead of the unaccessed chunk (A),
        // even though both display scores clamp to 1.0 under default weights.
        assert!(
            pos_b < pos_a,
            "accessed chunk (rank {pos_b}) should rank ahead of unaccessed (rank {pos_a})",
        );
        // Pin the premise that makes rank-on-unclamped necessary: BOTH display
        // scores are exactly 1.0 (the collision the split resolves). The rank
        // ordering above therefore can only come from the unclamped score.
        assert!(
            (results[pos_a].score - 1.0).abs() < 1e-9,
            "unaccessed display score ({:.6}) must clamp to exactly 1.0",
            results[pos_a].score,
        );
        assert!(
            (results[pos_b].score - 1.0).abs() < 1e-9,
            "accessed display score ({:.6}) must clamp to exactly 1.0",
            results[pos_b].score,
        );
    }

    /// Covers the MMR-enabled handoff through `hybrid_search_merge` — the
    /// construction and alignment of the `relevance`/`results` vectors — which
    /// no other search test exercises (MMR is off by default).
    ///
    /// This is NOT the raw-vs-clamped regression guard: `results` enters MMR
    /// pre-sorted by `raw_score`, so the boosted chunk would stay first even on
    /// a buggy `.score` read. That guarantee lives in the unit test
    /// `test_mmr_ranks_on_relevance_not_clamped_score`.
    #[tokio::test]
    async fn test_hybrid_search_merge_with_mmr_enabled() {
        let tmp = TempDir::new().unwrap();
        let mut idx = test_index(&tmp);

        // Two identical (redundant) chunks + one diverse chunk, all matching.
        let fa = tmp.path().join("a.md");
        let fb = tmp.path().join("b.md");
        let fc = tmp.path().join("c.md");
        std::fs::write(&fa, "# Rust\n\nRust ownership model explained.").unwrap();
        std::fs::write(&fb, "# Rust\n\nRust ownership model explained.").unwrap();
        std::fs::write(&fc, "# Borrow\n\nRust borrowing and lifetimes guide.").unwrap();
        idx.reindex_file(&fa, "workspace").unwrap();
        idx.reindex_file(&fb, "workspace").unwrap();
        idx.reindex_file(&fc, "workspace").unwrap();

        // Boost chunk B so its unclamped relevance exceeds chunk A's.
        let chunk_b_id = format!("{}:0", fb.to_string_lossy());
        idx.record_access(&chunk_b_id).unwrap();

        // Enable MMR (relevance-leaning lambda) to drive the aligned handoff.
        let mut config = MemorySearchConfig {
            min_score: 0.0,
            ..Default::default()
        };
        config.mmr.enabled = true;
        config.mmr.lambda = 0.7;

        let results = hybrid_search_merge(
            &idx,
            "rust ownership",
            idx.search_fts("rust ownership", 10).unwrap(),
            None,
            &config,
        )
        .unwrap()
        .results;

        assert!(
            !results.is_empty(),
            "MMR-enabled search must return results"
        );
        let pos_a = results
            .iter()
            .position(|r| r.path == fa.to_string_lossy().as_ref())
            .expect("chunk A must be returned");
        let pos_b = results
            .iter()
            .position(|r| r.path == fb.to_string_lossy().as_ref())
            .expect("chunk B must be returned");

        // Through the MMR handoff, the access-boosted chunk (B) ranks ahead of
        // its identical twin (A) because MMR's relevance term reads the
        // unclamped `relevance` slice (both share a clamped display score of 1.0).
        assert!(
            pos_b < pos_a,
            "boosted chunk (rank {pos_b}) should rank ahead of its twin (rank {pos_a}) with MMR on",
        );
    }

    /// access_boost never penalises zero-access chunks (boost = 1.0 for access_count = 0).
    #[test]
    fn test_access_boost_zero_access_is_neutral() {
        let boost = 1.0 + (0_f64).ln_1p() * 0.05;
        assert!(
            (boost - 1.0).abs() < f64::EPSILON,
            "zero accesses must yield a boost factor of exactly 1.0"
        );
    }

    // -----------------------------------------------------------------------
    // PR: scoring normalization fix tests
    // -----------------------------------------------------------------------

    /// FTS-only results (no vector search) should score well above a
    /// reasonable min_score threshold (e.g., 0.3). Before the fix,
    /// FTS-only chunks in hybrid mode had their scores capped at
    /// text_weight (0.3), making them impossible to retrieve at default
    /// min_score = 0.35.
    #[tokio::test]
    async fn test_fts_only_scores_above_reasonable_threshold() {
        let tmp = TempDir::new().unwrap();
        let mut idx = test_index(&tmp);

        let file_path = tmp.path().join("test.md");
        std::fs::write(
            &file_path,
            "# Rust Guide\n\nRust programming language ownership and borrowing tutorial.",
        )
        .unwrap();
        idx.reindex_file(&file_path, "workspace").unwrap();

        let config = MemorySearchConfig {
            min_score: 0.3,
            ..Default::default()
        };

        let results = hybrid_search(&idx, None, "rust programming", &config)
            .await
            .unwrap();

        assert!(
            !results.is_empty(),
            "FTS-only results must pass min_score=0.3 threshold"
        );
        assert!(
            results[0].score > 0.3,
            "FTS-only score ({:.4}) must exceed 0.3",
            results[0].score,
        );
    }

    #[tokio::test]
    async fn lone_partial_match_does_not_receive_full_confidence() {
        let tmp = TempDir::new().unwrap();
        let mut idx = test_index(&tmp);
        let file_path = tmp.path().join("partial.md");
        std::fs::write(&file_path, "# Rust\n\nRust ownership notes.").unwrap();
        idx.reindex_file(&file_path, "workspace").unwrap();
        let config = MemorySearchConfig {
            min_score: 0.5,
            ..Default::default()
        };

        let results = hybrid_search(
            &idx,
            None,
            "rust kubernetes postgres accessibility",
            &config,
        )
        .await
        .unwrap();

        assert!(
            results.is_empty(),
            "one weak OR-term match must not look exact"
        );
    }

    #[test]
    fn chinese_coverage_uses_character_bigrams() {
        let coverage = lexical_query_coverage("数据库连接错误", "处理数据库连接超时");
        assert!(coverage > 0.3 && coverage < 1.0, "coverage was {coverage}");
        assert_eq!(lexical_query_coverage("数据库连接", "数据库连接"), 1.0);
    }

    /// Global MEMORY.md chunks (source_weight = 0.7) should still be
    /// retrievable with a reasonable threshold. Before the fix, global
    /// chunks were capped at text_weight × source_weight = 0.21, making
    /// them invisible at any threshold above 0.2.
    #[tokio::test]
    async fn test_global_source_scores_above_min_threshold() {
        let tmp = TempDir::new().unwrap();
        let mut idx = test_index(&tmp);

        let file_path = tmp.path().join("global.md");
        std::fs::write(
            &file_path,
            "# Project Conventions\n\nAlways use graphite for PRs. Never commit without review.",
        )
        .unwrap();
        idx.reindex_file(&file_path, "global").unwrap();

        let config = MemorySearchConfig {
            min_score: 0.25,
            ..Default::default()
        };

        let results = hybrid_search(&idx, None, "graphite PRs review", &config)
            .await
            .unwrap();

        assert!(
            !results.is_empty(),
            "global source results must pass min_score=0.25 threshold"
        );
        assert!(
            results[0].score > 0.25,
            "global chunk score ({:.4}) must exceed 0.25",
            results[0].score,
        );
    }

    /// When vector results exist for some chunks but not others, FTS-only
    /// chunks should NOT be penalized. Their FTS score should remain at
    /// full weight (1.0 × normalized), not capped at text_weight.
    #[tokio::test]
    async fn test_fts_only_chunks_not_penalized_by_vec_existence() {
        let tmp = TempDir::new().unwrap();
        let mut idx = test_index(&tmp);
        let mock = MockEmbeddingProvider { dimensions: 4 };

        // File A: has both FTS and vector embedding
        let file_a = tmp.path().join("embedded.md");
        std::fs::write(
            &file_a,
            "# Rust\n\nRust programming language ownership tutorial.",
        )
        .unwrap();
        idx.reindex_file(&file_a, "workspace").unwrap();

        // Embed chunk A
        let path_a = file_a.to_string_lossy().to_string();
        let chunk_a_id = format!("{path_a}:0");
        let chunk_a = idx.get_chunk(&chunk_a_id).unwrap().unwrap();
        let embeddings = mock.embed_batch(&[&chunk_a.text]).await.unwrap();
        idx.upsert_embedding(&chunk_a_id, &embeddings[0]).unwrap();

        // File B: FTS only (no embedding)
        let file_b = tmp.path().join("unembedded.md");
        std::fs::write(
            &file_b,
            "# Rust\n\nRust programming language borrowing tutorial.",
        )
        .unwrap();
        idx.reindex_file(&file_b, "workspace").unwrap();

        let config = MemorySearchConfig {
            min_score: 0.0,
            ..Default::default()
        };

        // Use mock provider so hybrid path runs vector search
        let results = hybrid_search(
            &idx,
            Some(&mock as &dyn EmbeddingProvider),
            "rust programming",
            &config,
        )
        .await
        .unwrap();

        // Both chunks must be found
        let result_b = results
            .iter()
            .find(|r| r.path == file_b.to_string_lossy().as_ref());

        assert!(
            result_b.is_some(),
            "FTS-only chunk must appear in results even when other chunks have vectors"
        );
        let score_b = result_b.unwrap().score;
        assert!(
            score_b > 0.3,
            "FTS-only chunk score ({:.4}) must not be penalized below 0.3 by vec existence",
            score_b,
        );
    }

    /// Vector normalization should use absolute L2 distance scale (max = 2.0)
    /// instead of relative normalization. This ensures that even when all
    /// candidates have similar distances, vector scores still contribute
    /// meaningfully.
    ///
    /// Note: `dimensions: 4` is chosen deliberately. The mock provider
    /// (blake3 bytes / 255.0) does NOT produce unit-norm vectors. At low
    /// dimensions the L2 distances stay within `MAX_L2_DISTANCE = 2.0`, so
    /// the absolute normalization works. At production dimensions (1024),
    /// mock distances could exceed 2.0 and clamp to 0 — use real embeddings
    /// or normalize the mock output for high-dimensional tests.
    #[tokio::test]
    async fn test_vector_absolute_normalization() {
        let tmp = TempDir::new().unwrap();
        let mut idx = test_index(&tmp);
        let mock = MockEmbeddingProvider { dimensions: 4 };

        let file = tmp.path().join("test.md");
        std::fs::write(&file, "# Test\n\nContent for vector search test.").unwrap();
        idx.reindex_file(&file, "workspace").unwrap();

        let path_str = file.to_string_lossy().to_string();
        let chunk_id = format!("{path_str}:0");

        // Use the mock to get a consistent embedding
        let embedding = mock.embed_batch(&["test"]).await.unwrap();
        idx.upsert_embedding(&chunk_id, &embedding[0]).unwrap();

        // Search with vector — the mock returns deterministic embeddings
        let fts_results = idx.search_fts("content test", 10).unwrap_or_default();
        let query_embedding = mock.embed_batch(&["content test"]).await.unwrap();

        let config = MemorySearchConfig {
            min_score: 0.0,
            ..Default::default()
        };

        let results = hybrid_search_merge(
            &idx,
            "rust programming",
            fts_results,
            Some(&query_embedding[0]),
            &config,
        )
        .unwrap()
        .results;

        assert!(!results.is_empty(), "should find at least one result");
        // With absolute normalization, the combined score should be
        // substantially above zero (mock embeddings produce deterministic
        // but varying values).
        assert!(
            results[0].score > 0.1,
            "hybrid score ({:.4}) should be meaningful with absolute normalization",
            results[0].score,
        );
    }

    // -----------------------------------------------------------------------
    // Empty-template filter + score clamp tests
    // -----------------------------------------------------------------------

    /// The auto-generated global MEMORY.md stub, written verbatim by
    /// `MemoryStorage::ensure_initialized` (storage.rs), including the trailing
    /// newline. Kept in sync with that source.
    const GLOBAL_STUB: &str = "# Global Memory\n\
         \n\
         > This file is automatically managed by EchoAgent's memory system.\n\
         > You can also edit it manually — changes will be indexed on next session.\n\
         \n\
         ## Preferences\n\
         \n\
         <!-- Add any cross-project preferences here -->\n";

    /// The shorter workspace stub variant from `dream.rs`.
    const WORKSPACE_STUB: &str =
        "# Project Memory — /test\n\n> Auto-populated by dream consolidation. Edit freely.\n";

    #[test]
    fn test_is_content_free_global_stub() {
        // Caught via the marker-based scaffold predicate (it has blockquote
        // disclaimer lines, so it is NOT structurally empty) — only on
        // evergreen sources, where the stubs live.
        assert!(
            is_content_free(GLOBAL_STUB, "global"),
            "the unedited global MEMORY.md stub must be content-free"
        );
        assert!(
            is_content_free(WORKSPACE_STUB, "workspace"),
            "the auto-generated workspace stub must be content-free"
        );
    }

    #[test]
    fn test_is_content_free_scaffolding_only() {
        // The structural branch applies to ALL sources — use "session" here.
        assert!(
            is_content_free("# Heading\n## Subheading", "session"),
            "headings only"
        );
        assert!(
            is_content_free("<!-- just a comment -->", "session"),
            "comment only (single line)"
        );
        assert!(
            is_content_free("<!--\nmulti\nline\ncomment\n-->", "session"),
            "comment only (multi-line)"
        );
        assert!(is_content_free("", "session"), "empty string");
        assert!(is_content_free("   \n\t\n  ", "session"), "whitespace only");
        assert!(
            is_content_free("# Heading\n\n<!-- a comment -->\n\n## Another", "session"),
            "headings + comments only"
        );
        assert!(
            is_content_free("   # Indented Heading", "session"),
            "indented ATX heading is still a heading"
        );
    }

    #[test]
    fn test_is_content_free_real_content() {
        // Use "global" (evergreen) so both filter branches are active; real
        // content must survive regardless.
        assert!(
            !is_content_free("## Preferences\n\n- Use tabs", "global"),
            "heading with a following bullet has real content"
        );
        assert!(
            !is_content_free("Use C# for this", "global"),
            "a `#` mid-line is real content, not a heading"
        );
        assert!(
            !is_content_free("#hashtag not a heading", "global"),
            "`#` with no following space is not an ATX heading (per header_level)"
        );
        assert!(
            !is_content_free("# Title\n\nSome actual prose here.", "global"),
            "prose after a heading is real content"
        );
        assert!(
            !is_content_free("<!-- comment -->\nactual content", "global"),
            "content after a comment counts"
        );
        assert!(
            !is_content_free("- a\n- b", "global"),
            "list-only chunk is content"
        );
        assert!(
            !is_content_free("Title\n=====", "global"),
            "setext heading underline counts as content"
        );
        assert!(
            !is_content_free("```\nlet x = 1;\n```", "global"),
            "code-fence chunk is content"
        );
    }

    /// The scaffold-marker branch is scoped to evergreen sources: a short
    /// non-evergreen chunk that merely quotes a marker phrase must be kept,
    /// while the same text on an evergreen source is filtered.
    #[test]
    fn test_is_content_free_marker_branch_scoped_to_evergreen() {
        // A short session note that happens to quote a scaffold marker phrase.
        let quotes_marker =
            "Reminder: the template says \"Add any cross-project preferences here\".";
        assert!(
            !is_content_free(quotes_marker, "session"),
            "non-evergreen chunk quoting a marker phrase must NOT be filtered"
        );
        assert!(
            is_content_free(quotes_marker, "global"),
            "the same short text on an evergreen source is treated as scaffold"
        );
    }

    /// Blockquotes are real user content and must NOT be filtered (the user's
    /// spec listed only headings/comments/whitespace as scaffolding).
    #[test]
    fn test_is_content_free_preserves_blockquotes() {
        assert!(
            !is_content_free("> a quote\n> another quote", "global"),
            "blockquote-only user notes must be preserved"
        );
        assert!(
            !is_content_free(
                "## Important\n> Always run migrations before deploy",
                "global"
            ),
            "heading + blockquote with real guidance must be preserved"
        );
    }

    /// An unterminated `<!--` keeps the remainder as literal text, so a chunk
    /// with real content around it is not classified content-free.
    #[test]
    fn test_is_content_free_unterminated_comment_keeps_content() {
        assert!(
            !is_content_free("real text\n<!-- unterminated", "global"),
            "content before an unterminated comment must be kept"
        );
        assert!(
            !is_content_free("<!-- unterminated comment, no closer\nmore text", "global"),
            "content after an unterminated comment must be kept"
        );
    }

    /// An essentially-empty boilerplate chunk must be excluded from results
    /// even though it matches FTS, while a real-content chunk in the same
    /// scenario IS returned (proving the FTS pipeline is non-empty and the
    /// filter — not an empty result set — is what removes the stub).
    #[tokio::test]
    async fn test_content_free_chunk_excluded_from_search() {
        let tmp = TempDir::new().unwrap();
        let mut idx = test_index(&tmp);

        let stub_path = tmp.path().join("stub.md");
        std::fs::write(&stub_path, GLOBAL_STUB).unwrap();
        idx.reindex_file(&stub_path, "global").unwrap();

        // A real-content global file that also matches the query.
        let real_path = tmp.path().join("real.md");
        std::fs::write(
            &real_path,
            "# Conventions\n\nProject preferences: always use graphite for PRs. \
             Architecture is event-driven.",
        )
        .unwrap();
        idx.reindex_file(&real_path, "global").unwrap();

        // Precondition: the stub IS a raw FTS candidate for this query (the
        // term "preferences" appears in it). This proves the filter — not a
        // non-match — is what removes it from the final results below.
        let fts_candidates = idx
            .search_fts("project conventions preferences architecture", 10)
            .unwrap();
        assert!(
            fts_candidates
                .iter()
                .any(|r| r.chunk_id.starts_with(stub_path.to_string_lossy().as_ref())),
            "stub must be a raw FTS candidate before filtering"
        );

        let config = MemorySearchConfig {
            min_score: 0.0, // accept all by score, so only the filter can exclude
            ..Default::default()
        };

        let results = hybrid_search(
            &idx,
            None,
            "project conventions preferences architecture",
            &config,
        )
        .await
        .unwrap();

        assert!(
            !results.is_empty(),
            "real-content chunk must keep the result set non-empty"
        );
        assert!(
            results
                .iter()
                .any(|r| r.path == real_path.to_string_lossy().as_ref()),
            "real-content global file must be returned"
        );
        assert!(
            results
                .iter()
                .all(|r| r.path != stub_path.to_string_lossy().as_ref()),
            "content-free global stub must be excluded from results",
        );
    }

    /// The display score must clamp to exactly 1.0 when the access boost pushes
    /// the unclamped product above 1.0 — while the unclamped product (used for
    /// ranking) is genuinely > 1.0 (precondition, asserted explicitly so the
    /// test can't silently go vacuous).
    #[tokio::test]
    async fn test_final_score_clamped_to_one() {
        // Precondition: the boost at 100 accesses really does exceed 1.0.
        let boost_at_100 = 1.0 + (100_f64).ln_1p() * 0.05;
        assert!(
            boost_at_100 > 1.0,
            "test precondition: access boost at 100 accesses ({boost_at_100:.4}) must exceed 1.0"
        );

        let tmp = TempDir::new().unwrap();
        let mut idx = test_index(&tmp);

        let file_path = tmp.path().join("test.md");
        std::fs::write(
            &file_path,
            "# Rust\n\nRust ownership and borrowing tutorial.",
        )
        .unwrap();
        idx.reindex_file(&file_path, "workspace").unwrap();

        // Drive access_count high so the unbounded access_boost exceeds 1.0.
        let chunk_id = format!("{}:0", file_path.to_string_lossy());
        for _ in 0..100 {
            idx.record_access(&chunk_id).unwrap();
        }

        let config = MemorySearchConfig {
            min_score: 0.0,
            ..Default::default()
        };

        let results = hybrid_search_merge(
            &idx,
            "rust ownership",
            idx.search_fts("rust ownership", 10).unwrap(),
            None,
            &config,
        )
        .unwrap()
        .results;

        assert!(
            !results.is_empty(),
            "should find the frequently-accessed chunk"
        );
        // The top chunk is a top FTS match (base 1.0) × workspace weight (1.0)
        // × boost (>1.0) → unclamped > 1.0 → display score clamped to exactly 1.0.
        assert!(
            (results[0].score - 1.0).abs() < 1e-9,
            "display score ({:.6}) must clamp to exactly 1.0",
            results[0].score,
        );
        for r in &results {
            assert!(
                r.score <= 1.0,
                "score ({:.4}) must be clamped to <= 1.0 despite access boost",
                r.score,
            );
        }
    }
}
