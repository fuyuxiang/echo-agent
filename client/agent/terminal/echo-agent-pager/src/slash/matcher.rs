//! Nucleo-based fuzzy matcher for slash command and argument suggestions.
//!
//! Thin
//! wrapper around nucleo's `MultiPattern` that provides ranked results
//! and highlight index extraction.

use nucleo::{
    Config, Matcher, Utf32String,
    pattern::{CaseMatching, MultiPattern, Normalization},
};

/// Fuzzy matcher backed by nucleo.
///
/// Maintains internal state (pattern + matcher) between calls for efficiency.
/// Not thread-safe -- intended for single-threaded use within `SlashController`.
#[derive(Debug)]
pub struct FuzzyMatcher {
    pattern: MultiPattern,
    matcher: Matcher,
}

impl Default for FuzzyMatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl FuzzyMatcher {
    pub fn new() -> Self {
        Self {
            pattern: MultiPattern::new(1),
            matcher: Matcher::new(Config::DEFAULT),
        }
    }

    /// Rank items by fuzzy match score.
    ///
    /// Returns `(index, score)` pairs sorted by descending score, then
    /// ascending key text. At most `limit` results are returned.
    ///
    /// When `query` is empty, returns the first `limit` items with score 0
    /// (insertion order).
    pub fn rank<T, F>(
        &mut self,
        items: &[T],
        query: &str,
        limit: usize,
        mut key_fn: F,
    ) -> Vec<(usize, u32)>
    where
        F: FnMut(&T) -> &str,
    {
        if limit == 0 || items.is_empty() {
            return Vec::new();
        }

        let trimmed = query.trim();
        if trimmed.is_empty() {
            let capped = items.len().min(limit);
            return (0..capped).map(|idx| (idx, 0)).collect();
        }

        self.pattern
            .reparse(0, trimmed, CaseMatching::Smart, Normalization::Smart, false);

        let mut hits: Vec<(usize, u32, String)> = Vec::new();
        for (idx, item) in items.iter().enumerate() {
            let text = key_fn(item);
            if text.is_empty() {
                continue;
            }
            let matcher_text = Utf32String::from(text);
            if let Some(score) = self
                .pattern
                .score(std::slice::from_ref(&matcher_text), &mut self.matcher)
            {
                hits.push((idx, score, text.to_owned()));
            }
        }

        hits.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.2.cmp(&b.2)));
        if hits.len() > limit {
            hits.truncate(limit);
        }
        hits.into_iter()
            .map(|(idx, score, _)| (idx, score))
            .collect()
    }

    /// Extract fuzzy match highlight indices for the most recent pattern.
    ///
    /// Returns character positions in `text` that matched the pattern.
    pub fn indices(&mut self, text: &str) -> Vec<u32> {
        let mut indices = Vec::new();
        if text.is_empty() {
            return indices;
        }
        let s = Utf32String::from(text);
        let pattern = self.pattern.column_pattern(0);
        pattern.indices(s.slice(..), &mut self.matcher, &mut indices);
        indices
    }

    /// Match `query` against display text and return display-relative indices.
    pub fn indices_for(&mut self, query: &str, text: &str) -> Option<Vec<u32>> {
        let query = query.trim();
        if query.is_empty() || text.is_empty() {
            return None;
        }
        self.pattern
            .reparse(0, query, CaseMatching::Smart, Normalization::Smart, false);
        let text = Utf32String::from(text);
        self.pattern
            .score(std::slice::from_ref(&text), &mut self.matcher)?;
        let mut indices = Vec::new();
        self.pattern
            .column_pattern(0)
            .indices(text.slice(..), &mut self.matcher, &mut indices);
        Some(indices)
    }
}

#[cfg(test)]
mod tests {
    use super::FuzzyMatcher;

    #[test]
    fn indices_for_are_relative_to_display() {
        let mut matcher = FuzzyMatcher::new();
        assert_eq!(matcher.indices_for("ssh", "ssh-wrap"), Some(vec![0, 1, 2]));
        assert_eq!(matcher.indices_for("sw", "ssh-wrap"), Some(vec![0, 4]));
        assert_eq!(matcher.indices_for("fix s", "ssh-wrap"), None);
    }

    #[test]
    fn empty_query_yields_insertion_order() {
        let mut matcher = FuzzyMatcher::new();
        let items = ["alpha", "beta", "gamma"];
        let hits = matcher.rank(&items, "", items.len(), |item| item);
        assert_eq!(hits, vec![(0, 0), (1, 0), (2, 0)]);
    }

    #[test]
    fn ranked_results_prioritize_matches() {
        let mut matcher = FuzzyMatcher::new();
        let items = ["model", "help", "history"];
        let hits = matcher.rank(&items, "mod", items.len(), |item| item);
        assert_eq!(hits.first().map(|&(idx, _)| items[idx]), Some("model"));
    }

    #[test]
    fn limit_caps_results() {
        let mut matcher = FuzzyMatcher::new();
        let items = ["aaa", "aab", "aac", "aad", "aae"];
        let hits = matcher.rank(&items, "a", 2, |item| item);
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn empty_items_returns_empty() {
        let mut matcher = FuzzyMatcher::new();
        let items: [&str; 0] = [];
        let hits = matcher.rank(&items, "test", 10, |item| item);
        assert!(hits.is_empty());
    }

    /// Single-letter `/p` ties many `p*` commands at the same nucleo score;
    /// ordering is entirely secondary tiebreaks (display/builtin/MRU/etc.).
    #[test]
    fn query_p_ties_personas_and_pager_headless_at_same_score() {
        let mut matcher = FuzzyMatcher::new();
        let items = ["personas", "pager-headless", "plan", "plugins"];
        let hits = matcher.rank(&items, "p", items.len(), |item| item);
        let score_of = |name: &str| -> Option<u32> {
            hits.iter()
                .find(|&&(idx, _)| items[idx] == name)
                .map(|&(_, s)| s)
        };
        let personas = score_of("personas").expect("personas matches p");
        let pager = score_of("pager-headless").expect("pager-headless matches p");
        assert_eq!(personas, pager, "expected equal fuzzy scores for /p case");
        assert!(personas > 0);
        // Matcher limit=1 secondary sort is ascending key text → pager-headless wins.
        let top1 = matcher.rank(&items, "p", 1, |item| item);
        assert_eq!(items[top1[0].0], "pager-headless");
    }
}
