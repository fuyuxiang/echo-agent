//! Tolerant parser for upstream error bodies.

use serde_json::Value;

use crate::error::MAX_USER_ERROR_BODY_CHARS;

/// Everything salvageable from an upstream error body.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ProviderError {
    /// Human-readable reason. Never empty.
    pub message: String,
    pub kind: Option<String>,
    pub code: Option<String>,
    pub param: Option<String>,
    pub wke: Option<String>,
}

impl ProviderError {
    fn from_message(message: impl Into<String>) -> Option<Self> {
        let (message, wke) = split_wke(message.into());
        let message = message.trim().to_owned();
        if message.is_empty() {
            return None;
        }
        Some(Self {
            message,
            wke,
            ..Default::default()
        })
    }

    /// The best available machine-readable tag, preferring the most specific.
    pub fn slug(&self) -> Option<&str> {
        [
            self.wke.as_deref(),
            self.code.as_deref(),
            self.kind.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|s| is_slug_shaped(s))
    }

    /// Whether the message is markup rather than prose.
    pub fn message_is_markup(&self) -> bool {
        let m = self.message.trim();
        let lower = m.to_ascii_lowercase();
        m.starts_with('<')
            || lower.contains("<html")
            || lower.contains("<!doctype")
            || lower.contains("</")
    }

    /// One-line display text.
    pub fn display_message(&self) -> String {
        match self.slug() {
            Some(slug)
                if !message_already_says(&self.message, slug)
                    && !slug.chars().all(|c| c.is_ascii_digit()) =>
            {
                truncate_provider_message(&format!("{slug}: {}", self.message))
            }
            _ => truncate_provider_message(&self.message),
        }
    }
}

fn is_slug_shaped(s: &str) -> bool {
    if s.is_empty() || s.contains(char::is_whitespace) {
        return false;
    }
    !matches!(
        s.to_ascii_lowercase().as_str(),
        "unknown"
            | "unknown_error"
            | "error"
            | "errors"
            | "server_error"
            | "api_error"
            | "internal"
            | "internal_error"
            | "none"
            | "null"
    )
}

fn message_already_says(message: &str, slug: &str) -> bool {
    message
        .to_ascii_lowercase()
        .contains(&slug.to_ascii_lowercase())
}

fn split_wke(message: String) -> (String, Option<String>) {
    const PREFIX: &str = "[WKE=";
    let Some(start) = message.find(PREFIX) else {
        return (message, None);
    };
    let rest = &message[start + PREFIX.len()..];
    let Some(end) = rest.find(']') else {
        return (message, None);
    };
    let code = rest[..end].trim().to_owned();
    if code.is_empty() {
        return (message, None);
    }
    let cleaned = format!("{}{}", &message[..start], &rest[end + 1..]);
    let cleaned = cleaned.trim().trim_end_matches('.').trim().to_owned();
    let cleaned = if cleaned.is_empty() {
        code.clone()
    } else {
        cleaned
    };
    (cleaned, Some(code))
}

fn stringify_code(value: Option<&Value>) -> Option<String> {
    let s = match value? {
        Value::String(s) => s.trim().to_owned(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => return None,
    };
    (!s.is_empty() && s != "null").then_some(s)
}

fn nonempty_str(value: Option<&Value>) -> Option<String> {
    let s = value?.as_str()?.trim();
    (!s.is_empty()).then(|| s.to_owned())
}

fn first_str<'a>(obj: &Value, keys: impl IntoIterator<Item = &'a str>) -> Option<String> {
    keys.into_iter().find_map(|k| nonempty_str(obj.get(k)))
}

/// Parse an upstream error body into a [`ProviderError`]. Returns `None` when
/// the body carries nothing a human could read.
pub fn parse_provider_error(bytes: &[u8]) -> Option<ProviderError> {
    let text = std::str::from_utf8(bytes).ok()?.trim();
    parse_provider_error_str(text)
}

/// [`parse_provider_error`] over a string slice.
pub fn parse_provider_error_str(text: &str) -> Option<ProviderError> {
    let text = text.trim();
    if text.is_empty() || text.starts_with('<') {
        return None;
    }
    let value: Value = serde_json::from_str(text).ok()?;
    let parsed = walk(&value)?;
    Some(unwrap_double_encoding(parsed))
}

fn unwrap_double_encoding(outer: ProviderError) -> ProviderError {
    let Some(inner) = parse_provider_error_str(&outer.message) else {
        return outer;
    };
    if inner.message == outer.message {
        return outer;
    }
    ProviderError {
        message: inner.message,
        kind: inner.kind.or(outer.kind),
        code: inner.code.or(outer.code),
        param: inner.param.or(outer.param),
        wke: inner.wke.or(outer.wke),
    }
}

fn walk(value: &Value) -> Option<ProviderError> {
    match value {
        Value::String(s) => ProviderError::from_message(s.clone()),
        Value::Array(items) => items.iter().find_map(walk),
        Value::Object(_) => walk_object(value),
        _ => None,
    }
}

fn walk_object(obj: &Value) -> Option<ProviderError> {
    match obj.get("error") {
        Some(inner @ Value::Object(_)) => {
            let message = first_str(inner, ["message", "error", "detail", "description"])
                .or_else(|| nonempty_str(inner.get("type")))?;
            let (message, wke) = split_wke(message);
            Some(ProviderError {
                message,
                kind: nonempty_str(inner.get("type")).or_else(|| nonempty_str(obj.get("type"))),
                code: stringify_code(inner.get("code"))
                    .or_else(|| stringify_code(inner.get("error_code")))
                    .or_else(|| stringify_code(obj.get("code"))),
                param: nonempty_str(inner.get("param"))
                    .or_else(|| nonempty_str(inner.get("field"))),
                wke,
            })
        }

        Some(Value::String(s)) => {
            let (message, wke) = split_wke(s.clone());
            let message = message.trim().to_owned();
            if message.is_empty() {
                return None;
            }
            Some(ProviderError {
                message,
                kind: nonempty_str(obj.get("type")),
                code: stringify_code(obj.get("code"))
                    .or_else(|| stringify_code(obj.get("error_code"))),
                param: nonempty_str(obj.get("param")),
                wke,
            })
        }

        Some(other @ Value::Array(_)) => walk(other),

        _ => {
            let message = first_str(obj, ["message", "detail", "msg", "description"])?;
            let (message, wke) = split_wke(message);
            Some(ProviderError {
                message,
                kind: nonempty_str(obj.get("type")),
                code: stringify_code(obj.get("code"))
                    .or_else(|| stringify_code(obj.get("error_code"))),
                param: nonempty_str(obj.get("param")),
                wke,
            })
        }
    }
}

fn truncate_provider_message(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() <= MAX_USER_ERROR_BODY_CHARS {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(MAX_USER_ERROR_BODY_CHARS).collect();
    out.push('\u{2026}');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CORPUS: &[(&str, &str, &str, Option<&str>)] = &[
        (
            "openai_chat",
            r#"{"error":{"message":"Invalid value for 'temperature'","type":"invalid_request_error","param":"temperature","code":null}}"#,
            "Invalid value for 'temperature'",
            Some("invalid_request_error"),
        ),
        (
            "openai_string_code",
            r#"{"error":{"message":"Incorrect API key provided","type":"invalid_request_error","code":"invalid_api_key"}}"#,
            "Incorrect API key provided",
            Some("invalid_api_key"),
        ),
        (
            "type_tagged",
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
            "Overloaded",
            Some("overloaded_error"),
        ),
        (
            "type_tagged_billing",
            r#"{"type":"error","error":{"type":"billing_error","message":"Your credit balance is too low"}}"#,
            "Your credit balance is too low",
            Some("billing_error"),
        ),
        (
            "openrouter_integer_code",
            r#"{"error":{"message":"Provider returned error","code":429}}"#,
            "Provider returned error",
            Some("429"),
        ),
        (
            "google_legacy_integer_code",
            r#"{"error":{"code":400,"message":"API key not valid. Please pass a valid API key.","status":"INVALID_ARGUMENT"}}"#,
            "API key not valid. Please pass a valid API key.",
            Some("400"),
        ),
        (
            "azure_stringified_number_code",
            r#"{"error":{"code":"429","message":"Requests to the ChatCompletions Operation have exceeded the rate limit"}}"#,
            "Requests to the ChatCompletions Operation have exceeded the rate limit",
            Some("429"),
        ),
        (
            "vertex_array_wrapped",
            r#"[{"error":{"code":429,"message":"Quota exceeded for aiplatform.googleapis.com","status":"RESOURCE_EXHAUSTED"}}]"#,
            "Quota exceeded for aiplatform.googleapis.com",
            Some("429"),
        ),
        (
            "bedrock_capitalized_message",
            r#"{"Message":"Too many requests"}"#,
            "",
            None,
        ),
        (
            "bedrock_lowercase_message",
            r#"{"message":"The model is not ready for inference"}"#,
            "The model is not ready for inference",
            None,
        ),
        (
            "echo_agent_flat_sentence_code",
            r#"{"code":"The service is currently unavailable","error":"Service temporarily unavailable."}"#,
            "Service temporarily unavailable.",
            None,
        ),
        (
            "json_error_numeric_code",
            r#"{"code":500,"error":"internal server error"}"#,
            "internal server error",
            Some("500"),
        ),
        (
            "bare_string_body",
            r#""A request may either be streaming or deferred, but not both.""#,
            "A request may either be streaming or deferred, but not both.",
            None,
        ),
        (
            "responses_sse_frame",
            r#"{"type":"error","message":"The model produced an invalid tool call","code":null,"param":null}"#,
            "The model produced an invalid tool call",
            None,
        ),
        (
            "wke_marker",
            r#"{"code":429,"error":"You ran out of credits. [WKE=personal-team-blocked:spending-limit]"}"#,
            "You ran out of credits",
            Some("personal-team-blocked:spending-limit"),
        ),
        (
            "detail_only",
            r#"{"detail":"Not authenticated"}"#,
            "Not authenticated",
            None,
        ),
        (
            "axum_deserialize_failure",
            r#"{"error":"Failed to deserialize the JSON body into the target type: messages[3].content: invalid type: null"}"#,
            "Failed to deserialize the JSON body into the target type: messages[3].content: invalid type: null",
            None,
        ),
    ];

    #[test]
    fn corpus_parses_every_known_shape() {
        for (name, body, expected_message, expected_slug) in CORPUS {
            let parsed = parse_provider_error(body.as_bytes());
            if expected_message.is_empty() {
                assert!(
                    parsed.is_none(),
                    "{name}: expected no parse, got {parsed:?}"
                );
                continue;
            }
            let parsed = parsed.unwrap_or_else(|| panic!("{name}: failed to parse {body}"));
            assert_eq!(&parsed.message, expected_message, "{name}: message");
            assert_eq!(parsed.slug(), *expected_slug, "{name}: slug");
        }
    }

    #[test]
    fn double_encoded_body_is_unwrapped() {
        let body = r#"{"error":"{\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"max_tokens: 200000 > 64000, which is the maximum allowed\"}}"}"#;
        let parsed = parse_provider_error(body.as_bytes()).expect("parses");
        assert_eq!(
            parsed.message,
            "max_tokens: 200000 > 64000, which is the maximum allowed"
        );
        assert_eq!(parsed.slug(), Some("invalid_request_error"));
    }

    #[test]
    fn double_encoded_html_is_not_surfaced() {
        let body = serde_json::json!({
            "error": "<html><body>502 Bad Gateway</body></html>",
        })
        .to_string();
        let parsed = parse_provider_error(body.as_bytes()).expect("parses outer");
        assert!(parsed.slug().is_none());
        assert!(parsed.message_is_markup());
    }

    #[test]
    fn html_and_empty_bodies_are_rejected() {
        assert!(parse_provider_error(b"").is_none());
        assert!(parse_provider_error(b"   ").is_none());
        assert!(parse_provider_error(b"<!DOCTYPE html><html></html>").is_none());
        assert!(parse_provider_error(b"not json at all").is_none());
    }

    #[test]
    fn successful_payloads_are_not_mistaken_for_errors() {
        let chunk = r#"{"id":"abc","object":"chat.completion.chunk","created":0,"model":"echoagent","choices":[]}"#;
        assert!(parse_provider_error(chunk.as_bytes()).is_none());
    }

    #[test]
    fn sentence_shaped_codes_never_become_prefixes() {
        let parsed = parse_provider_error(
            br#"{"code":"Client specified an invalid argument","error":"model 'nope' does not exist"}"#,
        )
        .expect("parses");
        assert_eq!(
            parsed.code.as_deref(),
            Some("Client specified an invalid argument")
        );
        assert_eq!(parsed.slug(), None);
        assert_eq!(parsed.display_message(), "model 'nope' does not exist");
    }

    #[test]
    fn content_free_type_tags_are_not_prefixes() {
        for tag in ["unknown", "server_error", "error", "api_error", "internal"] {
            let body = format!(r#"{{"error":{{"message":"boom","type":"{tag}"}}}}"#);
            let parsed = parse_provider_error(body.as_bytes()).expect("parses");
            assert_eq!(parsed.display_message(), "boom", "tag {tag}");
        }
    }

    #[test]
    fn slug_is_not_repeated_when_the_message_already_says_it() {
        let parsed = parse_provider_error(
            br#"{"error":{"message":"rate_limit_error: slow down","type":"rate_limit_error"}}"#,
        )
        .expect("parses");
        assert_eq!(parsed.display_message(), "rate_limit_error: slow down");
    }

    #[test]
    fn display_message_is_length_capped_on_a_char_boundary() {
        let long = "é".repeat(MAX_USER_ERROR_BODY_CHARS + 50);
        let body = serde_json::json!({ "error": { "message": long } }).to_string();
        let parsed = parse_provider_error(body.as_bytes()).expect("parses");
        let shown = parsed.display_message();
        assert_eq!(shown.chars().count(), MAX_USER_ERROR_BODY_CHARS + 1);
        assert!(shown.ends_with('\u{2026}'));
    }

    #[test]
    fn wke_marker_is_lifted_out_of_the_message() {
        let (msg, wke) = split_wke("User exceeds storage [WKE=file:storage-exhausted]".into());
        assert_eq!(msg, "User exceeds storage");
        assert_eq!(wke.as_deref(), Some("file:storage-exhausted"));

        let (msg, wke) = split_wke("err [WKE=file:too-large".into());
        assert_eq!(msg, "err [WKE=file:too-large");
        assert_eq!(wke, None);

        let (msg, wke) = split_wke("[WKE=foo:bar]".into());
        assert_eq!(msg, "foo:bar");
        assert_eq!(wke.as_deref(), Some("foo:bar"));
    }
}
