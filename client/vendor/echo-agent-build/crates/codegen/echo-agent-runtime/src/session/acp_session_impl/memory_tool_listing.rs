use echo_agent_memory::records;

// Kept separate from the async tool context so privacy, filtering and pagination
// can be verified against exactly the response returned to the model.
pub(super) fn model_memory_list_response(
    records: Vec<records::MemoryRecord>,
    query: &str,
    include_inactive: bool,
    offset: usize,
    limit: Option<usize>,
) -> serde_json::Value {
    let query = query.to_lowercase();
    let items: Vec<_> = records
        .into_iter()
        .filter(|r| {
            r.status != "forgotten"
                && (include_inactive || records::active(r))
                && (query.is_empty()
                    || format!("{} {}", r.key, r.content)
                        .to_lowercase()
                        .contains(&query))
        })
        .collect();
    let total = items.len();
    let limit = limit.unwrap_or(15).clamp(1, 25);
    let next = offset.saturating_add(limit);
    // The management UI retains retired records for user-visible audit. The
    // model tool must never put forgotten keys/bodies/evidence back in context.
    // Excluding before pagination also keeps counts/cursors from revealing them.
    serde_json::json!({
        "recordType": "structured_memory",
        "contentComplete": true,
        "records": items.into_iter().skip(offset).take(limit).collect::<Vec<_>>(),
        "total": total,
        "nextOffset": if next < total { Some(next) } else { None },
        "noReliableMemory": total == 0,
        "guidance": if total == 0 { Some(
            "No matching recallable memory. Forgotten memories are excluded even with include_inactive. Do not recover them from old revisions, conversation history, or internal memory files. If there is no other reliable evidence, tell the user you do not know."
        ) } else { Some(
            "Each listed structured record already contains its complete content and evidence; answer directly when it supports the requested fact. Record IDs are opaque identifiers, not file paths. Do not pass record IDs to memory_get: that tool only reads file paths returned by memory_search. No follow-up file read is needed to validate a structured record. Use a record only when its exact subject and requested fact match the user's question. A nonempty catalog is not evidence for an unrelated or similarly named project. If the listed records do not support the requested fact, stop and say you do not know in the requested format. Do not broaden into unrelated memories, internal files, or unrequested conversation history."
        ) },
    })
}

#[cfg(test)]
mod memory_listing_privacy_tests {
    use super::*;

    fn record(id: &str, key: &str, status: &str) -> records::MemoryRecord {
        records::MemoryRecord {
            id: id.into(),
            scope: "global".into(),
            kind: "user".into(),
            key: key.into(),
            content: format!("{key}_BODY"),
            status: status.into(),
            pinned: true,
            origin: "manual".into(),
            confidence: 1.0,
            sources: vec![format!("{key}_SOURCE")],
            evidence: format!("{key}_EVIDENCE"),
            created_at: 1,
            updated_at: 1,
            valid_until: None,
            revision: 1,
            observed_at: 1,
        }
    }
    #[test]
    fn forgotten_payload_never_reaches_model_even_with_include_inactive() {
        let response = model_memory_list_response(
            vec![
                record("live", "CURRENT", "active"),
                record("forgotten", "FORGOTTEN_CANARY", "forgotten"),
            ],
            "",
            true,
            0,
            None,
        );
        assert!(
            !response.to_string().contains("FORGOTTEN_CANARY"),
            "Forgotten key, body, evidence and sources must stay out of model context"
        );
        assert_eq!(response["total"], 1);
        assert_eq!(response["records"][0]["id"], "live");
    }

    #[test]
    fn forgotten_filter_runs_before_matching_counts_and_pagination() {
        let response = model_memory_list_response(
            vec![
                record("live", "ALPHA_CURRENT", "active"),
                record("forgotten", "ALPHA_FORGOTTEN", "forgotten"),
                record("completed", "ALPHA_DONE", "completed"),
            ],
            "alpha",
            true,
            1,
            Some(1),
        );
        assert_eq!(response["total"], 2);
        assert_eq!(response["records"][0]["id"], "completed");
        assert!(response["nextOffset"].is_null());
    }

    #[test]
    fn ordinary_recall_does_not_list_completed_or_forgotten() {
        let response = model_memory_list_response(
            vec![
                record("live", "CURRENT", "active"),
                record("forgotten", "FORGOTTEN", "forgotten"),
                record("completed", "DONE", "completed"),
            ],
            "",
            false,
            0,
            None,
        );
        assert_eq!(response["total"], 1);
        assert_eq!(response["records"][0]["id"], "live");
    }

    #[test]
    fn a_nonempty_catalog_is_not_evidence_for_a_different_subject() {
        let response = model_memory_list_response(
            vec![record("live", "OTHER_PROJECT", "active")],
            "",
            false,
            0,
            None,
        );
        assert_eq!(response["total"], 1);
        assert!(response["guidance"]
            .as_str()
            .unwrap_or_default()
            .contains("exact subject"));
    }

    #[test]
    fn structured_records_are_complete_and_not_memory_file_paths() {
        let response = model_memory_list_response(
            vec![record("opaque-record-id", "EXACT_FACT", "active")],
            "",
            false,
            0,
            None,
        );
        assert_eq!(response["recordType"], "structured_memory");
        assert_eq!(response["contentComplete"], true);
        let guidance = response["guidance"].as_str().unwrap_or_default();
        assert!(guidance.contains("not file paths"));
        assert!(guidance.contains("Do not pass record IDs to memory_get"));
        assert!(guidance.contains("answer directly"));
        assert_eq!(response["records"][0]["content"], "EXACT_FACT_BODY");
    }

    #[test]
    fn exhausted_recall_explicitly_guides_answer_without_internal_file_recovery() {
        let response = model_memory_list_response(
            vec![record("forgotten", "FORGOTTEN_CANARY", "forgotten")],
            "unavailable",
            true,
            0,
            None,
        );
        assert_eq!(response["total"], 0);
        assert_eq!(response["noReliableMemory"], true);
        assert!(response["guidance"]
            .as_str()
            .unwrap_or_default()
            .contains("Do not"));
        assert!(!response.to_string().contains("FORGOTTEN_CANARY"));
    }
}
