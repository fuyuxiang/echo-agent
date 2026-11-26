//! Backend-agnostic trait for memory search and retrieval.
//!
//! `MemoryBackend` is defined in `echo-agent-tools` to keep the tool crate
//! backend-agnostic. The concrete implementation (`MemoryIndex`) lives in
//! `echo-agent-runtime`.
//!
//! All methods are `&self` (read-only). Write operations (record_access,
//! memory flush writes) go through the session actor directly.

/// Tracing target for memory system events.
///
/// Use `tracing::info!(target: MEMORY_LOG_TARGET, ...)` in `echo-agent-tools`.
/// Mirrors `echo_agent_runtime::session::memory_log::TARGET`.
pub const MEMORY_LOG_TARGET: &str = "echo_agent_memory";

/// Staleness threshold (days): show a note suggesting verification.
const STALE_NOTE_DAYS: f64 = 1.0;
/// Staleness threshold (days): show a strong stale warning.
const VERY_STALE_DAYS: f64 = 7.0;

/// Format a staleness warning for a memory search result.
///
/// Session-scoped chunks older than [`STALE_NOTE_DAYS`] get a note;
/// those older than [`VERY_STALE_DAYS`] get a stronger warning.
/// Global and workspace entries are curated/evergreen — no warning emitted.
/// Returns an empty string for fresh, evergreen, or unknown-age results.
pub fn format_staleness_note(source: &str, created_at: Option<i64>) -> String {
    if matches!(source, "global" | "workspace") {
        return String::new();
    }
    let Some(created) = created_at else {
        return String::new();
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let age_days = ((now - created) as f64 / 86400.0).max(0.0);

    if age_days > VERY_STALE_DAYS {
        format!(
            "**Stale ({}):** Verify current state before relying on this.\n",
            format_age(age_days)
        )
    } else if age_days > STALE_NOTE_DAYS {
        format!(
            "**Note ({}):** Verify this is still current.\n",
            format_age(age_days)
        )
    } else {
        String::new()
    }
}

fn format_age(days: f64) -> String {
    if days < 1.0 {
        let hours = (days * 24.0).round() as u64;
        format!("{hours}h ago")
    } else if days < 7.0 {
        let d = days.floor() as u64;
        if d == 1 {
            "1 day ago".to_string()
        } else {
            format!("{d} days ago")
        }
    } else {
        let weeks = (days / 7.0).round() as u64;
        if weeks == 1 {
            "1 week ago".to_string()
        } else {
            format!("{weeks} weeks ago")
        }
    }
}

/// A single search result from memory.
#[derive(Debug, Clone)]
pub struct MemorySearchResult {
    /// Unique chunk identifier (e.g., `"/path/to/file.md:0"`).
    pub chunk_id: String,
    /// Source file path.
    pub path: String,
    /// 0-based start line in the source file.
    pub start_line: usize,
    /// 0-based end line (exclusive) in the source file.
    pub end_line: usize,
    /// Relevance score (higher = more relevant).
    pub score: f64,
    /// Text snippet from the chunk.
    pub snippet: String,
    /// Source scope: `"global"`, `"workspace"`, or `"session"`.
    pub source: String,
    /// Unix timestamp (seconds) when the chunk was created.
    /// `None` for backends that don't track creation time.
    pub created_at: Option<i64>,
}

/// Backend-agnostic interface for memory queries.
///
/// Implementations must be `Send + Sync` to be stored in `Arc<dyn MemoryBackend>`
/// on `SessionContext`. All methods are `&self` — no mutation through the trait.
///
/// `search` is async because hybrid search may need to call an embedding API
/// to vectorize the query for KNN lookup.
#[async_trait::async_trait]
pub trait MemoryBackend: Send + Sync {
    /// Search memory for chunks matching a query string.
    ///
    /// Returns up to `max_results` results with score >= `min_score`.
    /// Uses hybrid search (FTS5 + vector KNN) when embeddings are available,
    /// falling back to FTS-only otherwise.
    async fn search(
        &self,
        query: &str,
        max_results: usize,
        min_score: f64,
    ) -> Result<Vec<MemorySearchResult>, Box<dyn std::error::Error + Send + Sync>>;

    /// Read a memory file by path, optionally returning a range of lines.
    fn get(
        &self,
        path: &str,
        from: Option<usize>,
        lines: Option<usize>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>>;

    /// Return the total number of indexed chunks.
    fn total_chunks(&self) -> Result<usize, Box<dyn std::error::Error + Send + Sync>>;

    /// Return the configured default for `max_results` in search queries.
    ///
    /// When the `memory_search` tool caller does not supply an explicit value,
    /// using this instead of a hardcoded fallback ensures that
    /// `[memory.search].max_results` config is honoured at the tool boundary.
    ///
    /// The default implementation returns `6`, matching the previous hardcoded
    /// value, so existing backends without a custom config behave identically.
    fn default_search_max_results(&self) -> usize {
        6
    }

    /// Return the configured default for `min_score` in search queries.
    ///
    /// When the caller does not supply an explicit threshold, using this instead
    /// of a hardcoded `0.0` ensures `[memory.search].min_score` config is
    /// honoured at the tool boundary.
    ///
    /// The default implementation returns `0.0` (accept all results), matching
    /// the previous hardcoded value.
    fn default_search_min_score(&self) -> f64 {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now_secs() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    #[test]
    fn staleness_global_is_evergreen() {
        let ts = Some(now_secs() - 86400 * 30);
        assert!(format_staleness_note("global", ts).is_empty());
    }

    #[test]
    fn staleness_workspace_is_evergreen() {
        let ts = Some(now_secs() - 86400 * 30);
        assert!(format_staleness_note("workspace", ts).is_empty());
    }

    #[test]
    fn staleness_none_created_at_is_empty() {
        assert!(format_staleness_note("session", None).is_empty());
    }

    #[test]
    fn staleness_fresh_is_empty() {
        let ts = Some(now_secs() - 3600); // 1 hour ago
        assert!(format_staleness_note("session", ts).is_empty());
    }

    #[test]
    fn staleness_note_for_moderately_old() {
        let ts = Some(now_secs() - 86400 * 2); // 2 days ago
        let note = format_staleness_note("session", ts);
        assert!(note.starts_with("**Note ("), "expected Note, got: {note}");
        assert!(
            note.contains("2 days ago"),
            "expected '2 days ago', got: {note}"
        );
    }

    #[test]
    fn staleness_warning_for_very_old() {
        let ts = Some(now_secs() - 86400 * 10); // 10 days ago
        let note = format_staleness_note("session", ts);
        assert!(note.starts_with("**Stale ("), "expected Stale, got: {note}");
        assert!(note.contains("week"), "expected weeks unit, got: {note}");
    }

    #[test]
    fn format_age_hours() {
        assert_eq!(format_age(0.5), "12h ago");
    }

    #[test]
    fn format_age_one_day() {
        assert_eq!(format_age(1.0), "1 day ago");
    }

    #[test]
    fn format_age_several_days() {
        assert_eq!(format_age(3.0), "3 days ago");
    }

    #[test]
    fn format_age_one_week() {
        assert_eq!(format_age(7.0), "1 week ago");
    }

    #[test]
    fn format_age_weeks() {
        assert_eq!(format_age(14.0), "2 weeks ago");
    }

    #[test]
    fn staleness_note_at_exact_one_day() {
        let ts = Some(now_secs() - 86400);
        let note = format_staleness_note("session", ts);
        assert!(
            note.is_empty(),
            "exactly 1 day should be fresh (> not >=), got: {note}"
        );
    }

    #[test]
    fn staleness_fresh_just_under_one_day() {
        // 0.99 days = 85536 seconds
        let ts = Some(now_secs() - 85536);
        assert!(
            format_staleness_note("session", ts).is_empty(),
            "0.99 days should be fresh"
        );
    }

    #[test]
    fn staleness_stale_at_exact_seven_days() {
        let ts = Some(now_secs() - 86400 * 7);
        let note = format_staleness_note("session", ts);
        assert!(
            note.starts_with("**Note ("),
            "exactly 7 days should trigger Note (> not >=), got: {note}"
        );
    }

    #[test]
    fn staleness_note_just_under_seven_days() {
        // 6.99 days = 603936 seconds
        let ts = Some(now_secs() - 603936);
        let note = format_staleness_note("session", ts);
        assert!(
            note.starts_with("**Note ("),
            "6.99 days should be Note not Stale, got: {note}"
        );
    }

    #[test]
    fn staleness_future_created_at_is_empty() {
        let ts = Some(now_secs() + 3600);
        assert!(
            format_staleness_note("session", ts).is_empty(),
            "future timestamp should produce no warning"
        );
    }
}
