//! Bounded desktop presentation data for the host's knowledge tools. This is
//! separate from the MCP text budget used in model context; other servers are
//! untouched, and no credentials or organization connection are consulted.

use serde_json::{Map, Value};

const MAX_UI_BYTES: usize = 65_536;
const MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;

pub(crate) fn for_ui(server: &str, tool: &str, text: &str) -> Option<Value> {
    if server != "echoagent_organization_memory"
        || !matches!(
            tool,
            "knowledge_context"
                | "knowledge_ask"
                | "knowledge_search"
                | "local_knowledge_search"
                | "local_knowledge_fetch"
        )
        || text.len() > MAX_INPUT_BYTES
    {
        return None;
    }
    let value: Value = serde_json::from_str(text).ok()?;
    let object = value.as_object()?;
    if serde_json::to_vec(&value).ok()?.len() <= MAX_UI_BYTES {
        return Some(value);
    }

    let mut result = Map::new();
    for key in [
        "qaEventId",
        "qa_event_id",
        "traceId",
        "available",
        "skipped",
        "sufficient",
        "insufficient",
        "confidence",
        "path",
    ] {
        copy_identity(&mut result, object, key);
    }
    copy_text(&mut result, object, "answer", 6_000);
    copy_text(&mut result, object, "text", 12_000);
    if let Some(items) = object.get("missingFacts").and_then(Value::as_array) {
        result.insert(
            "missingFacts".into(),
            Value::Array(
                items
                    .iter()
                    .take(5)
                    .filter_map(Value::as_str)
                    .map(|text| Value::String(clip(text, 600)))
                    .collect(),
            ),
        );
    }
    // Match the renderer's source precedence and visible item counts. Keep
    // document IDs and local paths intact so source actions remain usable.
    for keys in [
        &["evidence", "chunks", "citations"][..],
        &["memories"],
        &["items"],
    ] {
        if let Some((key, items)) = keys.iter().find_map(|key| {
            object
                .get(*key)
                .and_then(Value::as_array)
                .map(|items| (*key, items))
        }) {
            let limit = match key {
                "memories" => 8,
                "items" => 20,
                _ => 12,
            };
            let items = items
                .iter()
                .filter_map(Value::as_object)
                .take(limit)
                .map(source_summary)
                .map(Value::Object)
                .collect();
            result.insert(key.into(), Value::Array(items));
        }
    }
    result.insert("_displayTruncated".into(), Value::Bool(true));
    // Drop whole trailing entries rather than corrupting their source identity.
    while serde_json::to_vec(&result).ok()?.len() > MAX_UI_BYTES {
        let removed = ["memories", "items", "evidence", "chunks", "citations"]
            .iter()
            .any(|key| {
                result
                    .get_mut(*key)
                    .and_then(Value::as_array_mut)
                    .is_some_and(|items| items.pop().is_some())
            });
        if !removed {
            return None;
        }
    }
    Some(Value::Object(result))
}

fn copy_identity(target: &mut Map<String, Value>, source: &Map<String, Value>, key: &str) {
    if let Some(value) = source.get(key) {
        match value {
            Value::Null | Value::Bool(_) | Value::Number(_) => {
                target.insert(key.into(), value.clone());
            }
            Value::String(text) if text.len() <= 16_384 => {
                target.insert(key.into(), value.clone());
            }
            _ => {}
        }
    }
}

fn copy_text(
    target: &mut Map<String, Value>,
    source: &Map<String, Value>,
    key: &str,
    bytes: usize,
) {
    if let Some(text) = source.get(key).and_then(Value::as_str) {
        target.insert(key.into(), Value::String(clip(text, bytes)));
    }
}

fn clip(text: &str, bytes: usize) -> String {
    if text.len() <= bytes {
        return text.to_owned();
    }
    let mut end = bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

fn source_summary(source: &Map<String, Value>) -> Map<String, Value> {
    let mut result = Map::new();
    for key in [
        "id",
        "chunkId",
        "docId",
        "doc_id",
        "path",
        "kind",
        "stale",
        "page",
        "startLine",
        "endLine",
    ] {
        copy_identity(&mut result, source, key);
    }
    for key in ["docTitle", "title", "doc", "heading", "sourceLabel"] {
        copy_text(&mut result, source, key, 512);
    }
    for key in ["text", "quote", "content", "snippet", "rationale"] {
        copy_text(&mut result, source, key, 1_800);
    }
    if let Some(citation) = source.get("citation").and_then(Value::as_object) {
        let mut summary = Map::new();
        for key in ["docId", "doc_id", "page"] {
            copy_identity(&mut summary, citation, key);
        }
        copy_text(&mut summary, citation, "heading", 512);
        result.insert("citation".into(), Value::Object(summary));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    const SERVER: &str = "echoagent_organization_memory";

    #[test]
    fn ordinary_servers_and_unknown_tools_have_no_receipt() {
        assert!(for_ui("other", "knowledge_ask", "{}").is_none());
        assert!(for_ui(SERVER, "unknown_tool", "{}").is_none());
        assert!(for_ui(SERVER, "knowledge_ask", "not json").is_none());
    }

    #[test]
    fn result_above_model_budget_preserves_answer_citations_and_feedback_id() {
        let value = json!({"answer":"答案", "qaEventId":"qa-1", "citations":[{
            "docId":"doc-1", "quote":"中文".repeat(5_000)
        }]});
        let text = value.to_string();
        assert!(text.len() > 20_000);
        assert_eq!(for_ui(SERVER, "knowledge_ask", &text), Some(value));
    }

    #[test]
    fn large_organization_receipt_is_bounded_without_cutting_source_ids() {
        let value = json!({"answer":"长答案".repeat(20_000), "qaEventId":"qa-2",
            "citations":(0..20).map(|i| json!({"docId":format!("doc-{i}"),
                "quote":"依据".repeat(10_000), "citation":{"page":i+1}})).collect::<Vec<_>>()});
        let result = for_ui(SERVER, "knowledge_ask", &value.to_string()).unwrap();
        assert!(serde_json::to_vec(&result).unwrap().len() <= MAX_UI_BYTES);
        assert_eq!(result["qaEventId"], "qa-2");
        assert_eq!(result["citations"][0]["docId"], "doc-0");
        assert_eq!(result["citations"][0]["citation"]["page"], 1);
        assert_eq!(result["_displayTruncated"], true);
    }

    #[test]
    fn personal_receipt_retains_path_and_marks_partial_text_without_login() {
        let path = "/用户/个人知识/项目说明.md";
        let value = json!({"path":path,"text":"原文".repeat(40_000)});
        let result = for_ui(SERVER, "local_knowledge_fetch", &value.to_string()).unwrap();
        assert!(serde_json::to_vec(&result).unwrap().len() <= MAX_UI_BYTES);
        assert_eq!(result["path"], path);
        assert_eq!(result["_displayTruncated"], true);
        assert!(result["text"].as_str().unwrap().ends_with('…'));
    }

    #[test]
    fn oversized_identifiers_are_never_shortened_into_another_source() {
        let value = json!({"qaEventId":"x".repeat(20_000), "citations":[{
            "docId":"x".repeat(20_000), "quote":"依据".repeat(20_000)
        }]});
        let result = for_ui(SERVER, "knowledge_ask", &value.to_string()).unwrap();
        assert!(result.get("qaEventId").is_none());
        assert!(result["citations"][0].get("docId").is_none());
    }
}
