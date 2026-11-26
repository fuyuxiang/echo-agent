pub(crate) const SCHEMA_CONTRACT_RETRIES: u32 = 1;

pub(crate) fn contract_prompt(prompt: &str, schema: &serde_json::Value) -> String {
    format!(
        "{prompt}\n\n<output-contract>\nDo the work above with your tools first. Then end \
         your final message with a single ```json fenced block containing exactly one \
         JSON value that conforms to this JSON Schema (no prose inside the block):\n\
         {schema}\n</output-contract>"
    )
}

const SCHEMA_MAX_BYTES: usize = 256 * 1024;
const CONTRACT_OUTPUT_MAX_BYTES: usize = 2 * 1024 * 1024;
const SCHEMA_REGEX_SIZE_LIMIT: usize = 256 * 1024;
const SCHEMA_REGEX_DFA_SIZE_LIMIT: usize = 2 * 1024 * 1024;

#[derive(Debug)]
struct RejectExternalSchemaRefs;

impl jsonschema::Retrieve for RejectExternalSchemaRefs {
    fn retrieve(
        &self,
        uri: &jsonschema::Uri<String>,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
        Err(format!("external JSON Schema references are disabled: {uri}").into())
    }
}

pub(crate) fn compile_contract_schema(
    schema: &serde_json::Value,
) -> Result<jsonschema::Validator, String> {
    let schema_len = serde_json::to_vec(schema)
        .map_err(|e| format!("output_schema cannot be serialized: {e}"))?
        .len();
    if schema_len > SCHEMA_MAX_BYTES {
        return Err(format!(
            "output_schema is too large ({schema_len} bytes; maximum is {SCHEMA_MAX_BYTES})"
        ));
    }

    jsonschema::options()
        .with_retriever(RejectExternalSchemaRefs)
        .with_pattern_options(
            jsonschema::PatternOptions::regex()
                .size_limit(SCHEMA_REGEX_SIZE_LIMIT)
                .dfa_size_limit(SCHEMA_REGEX_DFA_SIZE_LIMIT),
        )
        .build(schema)
        .map_err(|e| format!("output_schema is not a valid self-contained JSON Schema: {e}"))
}

pub(crate) fn validate_contract_output(
    validator: &jsonschema::Validator,
    final_text: &str,
) -> Result<serde_json::Value, String> {
    if final_text.len() > CONTRACT_OUTPUT_MAX_BYTES {
        return Err(format!(
            "final message exceeds the {CONTRACT_OUTPUT_MAX_BYTES} byte structured-output limit"
        ));
    }
    let text = final_text.trim();
    let mut candidates: Vec<&str> = Vec::new();
    if let Some(start) = text.rfind("```json") {
        let body = &text[start + "```json".len()..];
        if let Some(end) = body.find("```") {
            candidates.push(body[..end].trim());
        }
    }
    candidates.push(text);
    for (open, close) in [('{', '}'), ('[', ']')] {
        if let (Some(s), Some(e)) = (text.find(open), text.rfind(close))
            && s < e
        {
            candidates.push(text[s..=e].trim());
        }
    }
    let mut parse_err = String::new();
    for cand in candidates {
        match serde_json::from_str::<serde_json::Value>(cand) {
            Ok(value) => {
                let verdict = match validator.validate(&value) {
                    Ok(()) => Ok(()),
                    Err(e) => Err(format!("output does not match the required schema: {e}")),
                };
                return verdict.map(|()| value);
            }
            Err(e) => {
                if parse_err.is_empty() {
                    parse_err = e.to_string();
                }
            }
        }
    }
    Err(format!(
        "final message did not contain valid JSON (expected a ```json fenced block): {parse_err}"
    ))
}

#[cfg(test)]
mod contract_tests {
    use super::{compile_contract_schema, validate_contract_output};

    fn v() -> jsonschema::Validator {
        compile_contract_schema(&serde_json::json!({
            "type": "object", "required": ["ok"],
            "properties": { "ok": { "type": "boolean" } }
        }))
        .unwrap()
    }

    #[test]
    fn fenced_json_after_prose_validates() {
        let text = "I scanned all 12 files with grep.\n\n```json\n{\"ok\": true}\n```";
        assert_eq!(
            validate_contract_output(&v(), text).unwrap(),
            serde_json::json!({"ok": true})
        );
    }

    #[test]
    fn bare_json_validates() {
        assert!(validate_contract_output(&v(), "{\"ok\": false}").is_ok());
    }

    #[test]
    fn json_embedded_in_prose_validates() {
        let text = "Here is my result: {\"ok\": true} — done.";
        assert!(validate_contract_output(&v(), text).is_ok());
    }

    #[test]
    fn last_fence_wins() {
        let text = "```json\n{\"wrong\": 1}\n```\ncorrected:\n```json\n{\"ok\": true}\n```";
        assert!(validate_contract_output(&v(), text).is_ok());
    }

    #[test]
    fn schema_violation_reports_schema_error() {
        let err = validate_contract_output(&v(), "{\"ok\": \"yes\"}").unwrap_err();
        assert!(err.contains("does not match the required schema"), "{err}");
    }

    #[test]
    fn no_json_reports_parse_error() {
        let err = validate_contract_output(&v(), "I finished the scan, all clear.").unwrap_err();
        assert!(err.contains("did not contain valid JSON"), "{err}");
    }

    #[test]
    fn external_references_are_rejected() {
        let err = compile_contract_schema(&serde_json::json!({
            "$ref": "https://example.com/schema.json"
        }))
        .unwrap_err();
        assert!(
            err.contains("external JSON Schema references are disabled"),
            "{err}"
        );
    }

    #[test]
    fn unsupported_backtracking_regex_is_rejected() {
        let validator = compile_contract_schema(&serde_json::json!({
            "type": "string",
            "pattern": "^(a+)+$"
        }))
        .expect("nested repetition is supported by the linear regex engine");
        assert!(validator.is_valid(&serde_json::json!("aaaa")));

        let err = compile_contract_schema(&serde_json::json!({
            "type": "string",
            "pattern": "(?=unsafe-lookaround)"
        }))
        .unwrap_err();
        assert!(err.contains("regex"), "{err}");
    }
}
