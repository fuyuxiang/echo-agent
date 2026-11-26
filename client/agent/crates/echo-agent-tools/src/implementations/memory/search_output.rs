use crate::types::memory_backend::{format_staleness_note, MemorySearchResult};

// The tool and host integration test share this exact model-facing formatter.
pub(super) fn format_memory_search_results(results: &[MemorySearchResult]) -> String {
    if results.is_empty() {
        return serde_json::json!({
            "searchComplete": true,
            "results": [],
            "noReliableMemory": true,
            "maxFocusedReformulations": 1,
            "internalFileRecoveryAllowed": false,
            "unrequestedHistoryRecoveryAllowed": false,
            "guidance": "The memory search succeeded but found no reliable evidence for this query. For a narrow single-fact recall, make at most one focused reformulation if genuinely needed, then stop and say you do not know in the user's requested format. Do not keep broadening queries, substitute a similarly named subject, list unrelated memories, or inspect internal memory files. Do not switch to conversation_history unless the user explicitly requested historical conversations. A miss is not a storage failure and is not permission to recover forgotten facts."
        }).to_string();
    }
    let mut output = format!("Found {} memory result(s):\n", results.len());
    for (i, r) in results.iter().enumerate() {
        let staleness = format_staleness_note(&r.source, r.created_at);
        output.push_str(&format!(
            "\n### Result {} (score: {:.2}, source: {})\n**File:** {} (lines {}-{})\n{}```\n{}\n```\n",
            i + 1, r.score, r.source, r.path, r.start_line, r.end_line, staleness, r.snippet,
        ));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_success_is_terminal_recall_data_with_a_bounded_followup_policy() {
        let text = format_memory_search_results(&[]);
        let result: serde_json::Value = serde_json::from_str(&text)
            .expect("a successful miss must be distinguishable from a backend error");
        assert_eq!(result["searchComplete"], true);
        assert_eq!(result["noReliableMemory"], true);
        assert_eq!(result["results"].as_array().unwrap().len(), 0);
        assert_eq!(result["maxFocusedReformulations"], 1);
        assert_eq!(result["internalFileRecoveryAllowed"], false);
        assert_eq!(result["unrequestedHistoryRecoveryAllowed"], false);
    }

    #[test]
    fn a_real_hit_retains_its_source_and_exact_evidence() {
        let result = MemorySearchResult {
            chunk_id: "memory.md:4".into(),
            path: "memory.md".into(),
            start_line: 4,
            end_line: 5,
            score: 0.9,
            snippet: "ACTIVE_FACT 中文 😀".into(),
            source: "global".into(),
            created_at: None,
        };
        let text = format_memory_search_results(&[result]);
        assert!(text.contains("Found 1 memory result(s)"));
        assert!(text.contains("**File:** memory.md (lines 4-5)"));
        assert!(text.contains("ACTIVE_FACT 中文 😀"));
        assert!(!text.contains("noReliableMemory"));
    }
}
