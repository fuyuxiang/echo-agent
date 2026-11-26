//! In-memory cache for self-contained text fetches with TTL expiry and eviction.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::types::output::WebFetchOutput;

#[derive(Clone)]
struct CachedPage {
    output: WebFetchOutput,
    inserted: Instant,
}

pub(crate) struct FetchCache {
    entries: HashMap<String, CachedPage>,
    ttl: Duration,
    max_entries: usize,
}

/// Simple cache that holds N completed fetch requests on a TTL.
impl FetchCache {
    pub(crate) fn new(ttl: Duration, max_entries: usize) -> Self {
        Self {
            entries: HashMap::new(),
            ttl,
            max_entries,
        }
    }

    pub(crate) fn get(&self, url: &str) -> Option<&WebFetchOutput> {
        self.entries.get(url).and_then(|entry| {
            if entry.inserted.elapsed() < self.ttl {
                Some(&entry.output)
            } else {
                None
            }
        })
    }

    /// Cache only inline text; path-bearing outputs must be materialized per call.
    pub(crate) fn insert_text(&mut self, url: String, output: WebFetchOutput, was_truncated: bool) {
        if was_truncated {
            return;
        }
        if self.entries.len() >= self.max_entries {
            // Evict oldest entry.
            let oldest_key = self
                .entries
                .iter()
                .max_by_key(|(_, v)| v.inserted.elapsed())
                .map(|(k, _)| k.clone());
            if let Some(key) = oldest_key {
                self.entries.remove(&key);
            }
        }
        self.entries.insert(
            url,
            CachedPage {
                output,
                inserted: Instant::now(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::output::WebFetchContent;

    fn output(content: &str) -> WebFetchOutput {
        WebFetchOutput::Content(WebFetchContent {
            url: "https://example.com/".to_string(),
            content: content.to_string(),
            content_type: "markdown".to_string(),
            status_code: 200,
            bytes: content.len(),
            source_artifact: None,
            inline_fallback: None,
            output_location: None,
        })
    }

    #[test]
    fn truncated_artifact_output_is_never_cached() {
        let mut cache = FetchCache::new(Duration::from_secs(60), 10);
        let url = "https://example.com/";
        cache.insert_text(url.to_string(), output("/sessions/a/web_fetch/1.md"), true);
        assert!(cache.get(url).is_none());

        cache.insert_text(url.to_string(), output("fully inline"), false);
        assert!(cache.get(url).is_some());
    }
}
