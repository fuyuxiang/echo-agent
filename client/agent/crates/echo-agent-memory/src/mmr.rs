//! Maximal Marginal Relevance (MMR) diversity re-ranking.
//!
//! Without MMR, if a user has multiple memory chunks about the same topic,
//! the top results are nearly identical. MMR penalizes redundancy by
//! greedily selecting results that balance relevance with diversity.
//!
//! **Formula:**
//! ```text
//! MMR(d) = λ × relevance(d) - (1-λ) × max_similarity(d, selected)
//! ```
//!
//! Uses Jaccard similarity on tokenized snippets (no embeddings needed).
//! O(n²) but n is tiny (typically 6–18 candidates after hybrid scoring).

use std::collections::HashSet;

use super::search::SearchResult;
use echo_agent_config_types::MmrConfig;

/// Tokenize text into a set of alphanumeric words for Jaccard comparison.
///
/// Expects **pre-lowered** input — callers should lowercase snippets before
/// calling this. Uses the same splitting strategy as `query_expansion`
/// (split on non-alphanumeric except underscore) for consistency, but without
/// stop word removal — we want full token overlap for similarity measurement.
fn tokenize(text: &str) -> HashSet<&str> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty())
        .collect()
}

/// Jaccard similarity: |A ∩ B| / |A ∪ B|.
fn jaccard_similarity(a: &HashSet<&str>, b: &HashSet<&str>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let intersection = a.intersection(b).count();
    let union = a.len() + b.len() - intersection;
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

/// Re-rank results using Maximal Marginal Relevance.
///
/// Reorders `results` in-place to balance relevance with diversity.
/// No-op when `config.enabled` is false, `lambda` is 1.0, or there
/// are fewer than 2 results.
///
/// `relevance` is the per-result unclamped ranking score, aligned
/// index-for-index with `results` on entry. It is passed separately rather than
/// read from the clamped `SearchResult.score`, which would saturate top chunks
/// to 1.0 and lose the access-frequency boost tiebreak.
pub fn mmr_rerank(results: &mut Vec<SearchResult>, relevance: &[f64], config: &MmrConfig) {
    if !config.enabled || results.len() <= 1 {
        return;
    }
    if config.lambda == 1.0 {
        return;
    }
    assert_eq!(
        relevance.len(),
        results.len(),
        "relevance must be aligned with results"
    );

    // Lowercase snippets once, then tokenize. This ensures "Rust" and "rust"
    // are treated as the same token — casing varies across markdown sources.
    let lowered: Vec<String> = results.iter().map(|r| r.snippet.to_lowercase()).collect();
    let token_cache: Vec<HashSet<&str>> = lowered.iter().map(|s| tokenize(s)).collect();

    let max_score = relevance.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let min_score = relevance.iter().copied().fold(f64::INFINITY, f64::min);
    let range = (max_score - min_score).max(f64::EPSILON);

    let lambda = config.lambda;
    let mut selected: Vec<usize> = Vec::with_capacity(results.len());
    let mut remaining: Vec<usize> = (0..results.len()).collect();

    while !remaining.is_empty() {
        let mut best_pos = 0;
        let mut best_mmr = f64::NEG_INFINITY;

        for (pos, &candidate) in remaining.iter().enumerate() {
            let normalized = (relevance[candidate] - min_score) / range;

            let max_sim = selected
                .iter()
                .map(|&sel| jaccard_similarity(&token_cache[candidate], &token_cache[sel]))
                .fold(0.0_f64, f64::max);

            let mmr_score = lambda * normalized - (1.0 - lambda) * max_sim;

            if mmr_score > best_mmr
                || (mmr_score == best_mmr && relevance[candidate] > relevance[remaining[best_pos]])
            {
                best_mmr = mmr_score;
                best_pos = pos;
            }
        }

        selected.push(remaining.remove(best_pos));
    }

    let reordered: Vec<SearchResult> = selected
        .into_iter()
        .map(|i| std::mem::replace(&mut results[i], placeholder_result()))
        .collect();
    *results = reordered;
    // `results` is now reordered, so the caller's `relevance` slice is stale
    // and must not be read again.
}

/// Placeholder to enable moving results out of the vec without Clone.
fn placeholder_result() -> SearchResult {
    SearchResult {
        chunk_id: String::new(),
        path: String::new(),
        start_line: 0,
        end_line: 0,
        score: 0.0,
        snippet: String::new(),
        source: String::new(),
        created_at: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_result(id: &str, snippet: &str, score: f64) -> SearchResult {
        SearchResult {
            chunk_id: id.to_string(),
            path: format!("{id}.md"),
            start_line: 0,
            end_line: 1,
            score,
            snippet: snippet.to_string(),
            source: "workspace".to_string(),
            created_at: 1_700_000_000,
        }
    }

    fn enabled_config(lambda: f64) -> MmrConfig {
        MmrConfig {
            enabled: true,
            lambda,
        }
    }

    /// Test helper: re-rank using each result's own `score` as its relevance
    /// (mirrors the pre-split behavior the existing assertions were written for).
    fn rerank(results: &mut Vec<SearchResult>, config: &MmrConfig) {
        let relevance: Vec<f64> = results.iter().map(|r| r.score).collect();
        mmr_rerank(results, &relevance, config);
    }

    #[test]
    fn test_disabled_is_noop() {
        let mut results = vec![
            make_result("a", "rust async", 1.0),
            make_result("b", "rust async patterns", 0.9),
        ];
        let original_order: Vec<String> = results.iter().map(|r| r.chunk_id.clone()).collect();
        rerank(&mut results, &MmrConfig::default());
        let after: Vec<String> = results.iter().map(|r| r.chunk_id.clone()).collect();
        assert_eq!(original_order, after);
    }

    #[test]
    fn test_lambda_one_is_noop() {
        let mut results = vec![
            make_result("a", "rust async", 1.0),
            make_result("b", "python sync", 0.5),
        ];
        rerank(&mut results, &enabled_config(1.0));
        assert_eq!(results[0].chunk_id, "a");
        assert_eq!(results[1].chunk_id, "b");
    }

    #[test]
    fn test_single_result_is_noop() {
        let mut results = vec![make_result("a", "rust async", 1.0)];
        rerank(&mut results, &enabled_config(0.7));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].chunk_id, "a");
    }

    /// Regression guard: MMR must rank on `relevance`, not the clamped
    /// `SearchResult.score`. Both results tie at `score == 1.0`; the
    /// higher-relevance result is placed SECOND so a buggy `.score` read would
    /// keep input order and land "low" first.
    #[test]
    fn test_mmr_ranks_on_relevance_not_clamped_score() {
        let mut results = vec![
            make_result("low", "alpha topic one", 1.0),
            make_result("high", "beta subject two", 1.0),
        ];
        let relevance = [1.0, 1.25];
        mmr_rerank(&mut results, &relevance, &enabled_config(0.7));

        assert_eq!(
            results[0].chunk_id, "high",
            "MMR must order by unclamped relevance, not the clamped .score",
        );
        assert_eq!(results[1].chunk_id, "low");
    }

    #[test]
    fn test_diverse_results_promoted() {
        // Three results: two very similar (rust async), one different (python web)
        // With MMR, the diverse result should be promoted over the redundant one
        let mut results = vec![
            make_result("a", "rust async programming patterns", 1.0),
            make_result("b", "rust async programming tutorial", 0.95),
            make_result("c", "python web framework flask", 0.9),
        ];
        rerank(&mut results, &enabled_config(0.5));

        // First should still be "a" (highest relevance)
        assert_eq!(results[0].chunk_id, "a");
        // "c" (diverse) should be promoted above "b" (redundant with "a")
        assert_eq!(
            results[1].chunk_id, "c",
            "diverse result should be promoted over redundant one"
        );
        assert_eq!(results[2].chunk_id, "b");
    }

    #[test]
    fn test_identical_snippets_heavily_penalized() {
        let mut results = vec![
            make_result("a", "exact same content here", 1.0),
            make_result("b", "exact same content here", 0.99),
            make_result("c", "completely different topic", 0.5),
        ];
        rerank(&mut results, &enabled_config(0.5));

        assert_eq!(results[0].chunk_id, "a");
        // "c" should beat "b" because "b" is identical to "a"
        assert_eq!(
            results[1].chunk_id, "c",
            "different result should beat identical duplicate"
        );
    }

    #[test]
    fn test_case_insensitive_similarity() {
        // "Rust Async" and "rust async" should be treated as identical
        // (both lowercased before tokenization). Without lowercasing,
        // these would only have 0.5 Jaccard similarity.
        let mut results = vec![
            make_result("a", "Rust Async Programming", 1.0),
            make_result("b", "rust async programming", 0.95),
            make_result("c", "Python Web Framework", 0.9),
        ];
        rerank(&mut results, &enabled_config(0.5));

        assert_eq!(results[0].chunk_id, "a");
        // "c" (diverse) should beat "b" (same content, different casing)
        assert_eq!(
            results[1].chunk_id, "c",
            "case-only difference should be detected as redundant"
        );
    }

    #[test]
    fn test_preserves_result_count() {
        let mut results = vec![
            make_result("a", "one", 1.0),
            make_result("b", "two", 0.9),
            make_result("c", "three", 0.8),
            make_result("d", "four", 0.7),
        ];
        rerank(&mut results, &enabled_config(0.7));
        assert_eq!(results.len(), 4);
    }

    #[test]
    fn test_scores_and_snippets_preserved() {
        let mut results = vec![
            make_result("a", "rust programming", 1.0),
            make_result("b", "python scripting", 0.5),
        ];
        rerank(&mut results, &enabled_config(0.7));
        // All fields should be intact after re-ranking
        for r in &results {
            assert!(!r.chunk_id.is_empty());
            assert!(!r.snippet.is_empty());
            assert!(r.score > 0.0);
        }
    }

    // -----------------------------------------------------------------------
    // Jaccard similarity unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_jaccard_identical() {
        let a: HashSet<&str> = ["rust", "async"].into();
        let b: HashSet<&str> = ["rust", "async"].into();
        assert!((jaccard_similarity(&a, &b) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_jaccard_disjoint() {
        let a: HashSet<&str> = ["rust", "async"].into();
        let b: HashSet<&str> = ["python", "web"].into();
        assert!((jaccard_similarity(&a, &b)).abs() < f64::EPSILON);
    }

    #[test]
    fn test_jaccard_partial_overlap() {
        let a: HashSet<&str> = ["rust", "async", "programming"].into();
        let b: HashSet<&str> = ["rust", "web", "programming"].into();
        // intersection = {rust, programming} = 2, union = {rust, async, programming, web} = 4
        assert!((jaccard_similarity(&a, &b) - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_jaccard_both_empty() {
        let a: HashSet<&str> = HashSet::new();
        let b: HashSet<&str> = HashSet::new();
        assert!((jaccard_similarity(&a, &b) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_jaccard_one_empty() {
        let a: HashSet<&str> = ["rust"].into();
        let b: HashSet<&str> = HashSet::new();
        assert!((jaccard_similarity(&a, &b)).abs() < f64::EPSILON);
    }

    #[test]
    fn test_tokenize_splits_on_punctuation() {
        let tokens = tokenize("hello, world! rust_code");
        assert!(tokens.contains("hello"));
        assert!(tokens.contains("world"));
        assert!(tokens.contains("rust_code"));
        assert!(!tokens.contains(","));
    }
}
