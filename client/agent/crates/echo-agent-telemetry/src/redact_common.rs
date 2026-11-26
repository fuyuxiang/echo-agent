//! Redaction helpers shared by the **internal** OTLP span pipeline
//! ([`crate::otel_layer`]) and the **external** customer-collector pipeline
//! ([`crate::external`]).
//!
//! Both pipelines are authoritative privacy chokepoints (see the crate
//! `AGENTS.md`); these helpers are the string-level scrubbing primitives they
//! share. Changes here affect every byte that leaves the process on either
//! pipeline.

use std::borrow::Cow;

/// Secret-shape then user-path scrub. Returns `Some` only when the input
/// changed (owned, so callers can overwrite in place).
pub(crate) fn redact_owned(input: &str) -> Option<String> {
    let secrets = echo_agent_secrets::redact_secrets(input);
    match echo_agent_secrets::redact_user_paths(secrets.as_ref()) {
        Cow::Owned(paths) => Some(paths),
        Cow::Borrowed(_) => match secrets {
            Cow::Owned(s) => Some(s),
            Cow::Borrowed(_) => None,
        },
    }
}

/// Scrub a string, returning the (possibly unchanged) owned value.
pub(crate) fn redact_to_owned(input: &str) -> String {
    redact_owned(input).unwrap_or_else(|| input.to_owned())
}

/// Reduce a URL to `scheme://host[:port]` — its path/query can carry user
/// content. Unparseable values are returned unchanged (callers pass the result
/// through the secret scrubber).
pub(crate) fn url_origin(value: &str) -> Cow<'_, str> {
    if let Ok(url) = url::Url::parse(value)
        && let Some(host) = url.host_str()
    {
        let origin = match url.port() {
            Some(port) => format!("{}://{}:{}", url.scheme(), host, port),
            None => format!("{}://{}", url.scheme(), host),
        };
        return Cow::Owned(origin);
    }
    Cow::Borrowed(value)
}

/// Reduce any embedded `http(s)://…` URLs in free-form text (e.g. transport
/// error strings) to their origins, then secret-scrub. Path/query can carry
/// tokens or user content and must not reach logs.
pub(crate) fn redact_urls_in_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while !rest.is_empty() {
        let https = rest.find("https://");
        let http = rest.find("http://");
        let start = match (https, http) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        let Some(start) = start else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..start]);
        let url_rest = &rest[start..];
        let end = url_rest
            .char_indices()
            .find(|&(_, c)| {
                c.is_whitespace() || matches!(c, ')' | ']' | '"' | '\'' | ',' | ';' | '>')
            })
            .map(|(i, _)| i)
            .unwrap_or(url_rest.len());
        let url = &url_rest[..end];
        out.push_str(url_origin(url).as_ref());
        rest = &url_rest[end..];
    }
    redact_to_owned(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_owned_scrubs_secret_shapes() {
        let out = redact_owned("key sk-CANARYabcdefghij1234567890 end")
            .expect("secret must trigger a rewrite");
        assert!(!out.contains("CANARY"));
    }

    #[test]
    fn redact_owned_returns_none_when_clean() {
        assert_eq!(redact_owned("no secrets here"), None);
    }

    #[test]
    fn url_origin_drops_path_and_query() {
        let origin = url_origin("https://collector.corp.example:4318/v1/logs?token=CANARY");
        assert_eq!(origin, "https://collector.corp.example:4318");
    }

    #[test]
    fn url_origin_passes_unparseable_through() {
        assert_eq!(url_origin("not a url"), "not a url");
    }

    #[test]
    fn redact_urls_in_text_reduces_embedded_urls() {
        let err = "error sending request for url (https://collector.corp.example:4318/v1/logs?token=CANARY): connection reset";
        let out = redact_urls_in_text(err);
        assert!(out.contains("https://collector.corp.example:4318"));
        assert!(!out.contains("/v1/logs"));
        assert!(!out.contains("CANARY"));
    }
}
