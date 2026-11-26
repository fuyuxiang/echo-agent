//! Foundation escaped-slash normalization for inbound ACP stdin lines — the
//! crate's second `agent-client-protocol` v0.6 wire workaround, alongside
//! [`LineBufferedRead`](crate::LineBufferedRead).
//!
//! [`spawn_stdin_line_reader`](crate::spawn_stdin_line_reader) feeds every
//! line through [`normalize_json_line`]; unhooking that one call site removes
//! the workaround once the upstream envelope parses `method` as an
//! owned/`Cow` string.
//!
//! Scope: only process-stdin ingress is normalized. Clients that connect
//! directly to the leader socket bypass this module — fine today, those are
//! first-party clients whose encoders never emit `\/`.
//!
//! Downstream dependency: the leader bridge's replay sniff
//! (`echo-agent-pager-bin/src/main.rs`, the `trimmed.contains("\"session/new\"")`
//! checks) matches escaped Foundation input only because this normalization
//! runs upstream of it.

/// Foundation (Xcode) escapes `/` as `\/` by default, and the pinned
/// `agent-client-protocol` 0.6 envelope parses `method` as a borrowed `&str`
/// ([`RawIncomingMessage`](agent_client_protocol::RawIncomingMessage)), so any
/// escape inside `method` fails the whole envelope parse and the line is
/// silently dropped. Re-serializing through `serde_json` — which never emits
/// `\/` — makes the method borrowable again.
///
/// Rewrites touch only lines the crate would otherwise drop: the two-byte `\/`
/// scan is a cheap prefilter (serde_json / `JSON.stringify` never emit it),
/// and a line that then parses as the real pinned envelope — e.g. a
/// clean-method prompt whose params contain `s/\//_/g` — passes through
/// byte-identical, so healthy clients are untouched by construction. Tradeoff:
/// a hypothetical `\u002F`-escaped method is not normalized (no known encoder
/// emits that, and `\u` can't be the prefilter — JS legitimately emits
/// `\u2028` and surrogate pairs in text).
///
/// Any line that fails both parses passes through byte-identical —
/// deliberately: the acp crate keeps ownership of garbage handling.
pub(crate) fn normalize_json_line(line: Vec<u8>) -> Vec<u8> {
    if !line.windows(2).any(|w| w == br"\/") {
        return line;
    }
    // Same type + bytes the acp crate will parse (trailing terminator is JSON
    // whitespace): if it accepts the line, forward it byte-identical.
    if serde_json::from_slice::<agent_client_protocol::RawIncomingMessage>(&line).is_ok() {
        return line;
    }
    let body_len = line
        .iter()
        .rposition(|&b| b != b'\n' && b != b'\r')
        .map_or(0, |pos| pos + 1);
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&line[..body_len]) else {
        // Exactly the line class the acp 0.6 envelope will then drop silently.
        tracing::debug!(
            len = line.len(),
            "unparseable escaped-slash stdin line passed through; acp may drop it"
        );
        return line;
    };
    let Ok(mut normalized) = serde_json::to_vec(&value) else {
        return line;
    };
    normalized.extend_from_slice(&line[body_len..]);
    tracing::debug!(
        len = line.len(),
        normalized_len = normalized.len(),
        "normalized escaped-slash line for the acp 0.6 envelope"
    );
    normalized
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::RawIncomingMessage;

    use super::*;

    #[test]
    fn escaped_slash_method_accepted_by_upstream_cow_envelope() {
        // acp 0.10.4+ parses `method` as Cow<str>, so Foundation-style
        // `session\/prompt` is accepted without our re-serialization rewrite.
        let raw =
            br#"{"jsonrpc":"2.0","id":"5DE7EA60-0B0C-4A43-9650-2B72CDF6A44B","method":"session\/prompt","params":{}}"#;
        let mut line = raw.to_vec();
        line.push(b'\n');
        assert!(serde_json::from_slice::<RawIncomingMessage>(raw).is_ok());

        let normalized = normalize_json_line(line.clone());
        // Early-return path: envelope-acceptable lines pass through byte-identical.
        assert_eq!(normalized, line);
    }

    #[test]
    fn invalid_json_with_escaped_slash_passes_through_byte_identical() {
        let line = b"not json \\/ at all\n".to_vec();
        assert_eq!(normalize_json_line(line.clone()), line);
    }

    #[test]
    fn line_without_backslash_passes_through_untouched() {
        let expected = br#"{"jsonrpc":"2.0","id":1,"method":"session/new","params":{}}"#.to_vec();
        let line = expected.clone();
        let ptr = line.as_ptr();

        let normalized = normalize_json_line(line);

        assert_eq!(normalized, expected);
        // Same allocation: the fast path never parsed or re-serialized.
        assert_eq!(normalized.as_ptr(), ptr);
    }

    #[test]
    fn string_escapes_without_escaped_slash_pass_through_untouched() {
        let raw =
            br#"{"jsonrpc":"2.0","id":1,"method":"session/prompt","params":{"text":"a\nb \"q\" c\\d"}}"#;
        let expected = raw.to_vec();
        let line = expected.clone();
        let ptr = line.as_ptr();

        let normalized = normalize_json_line(line);

        assert_eq!(normalized, expected);
        // Same allocation: `\n`/`\"`/`\\` escapes alone never trip the rewrite.
        assert_eq!(normalized.as_ptr(), ptr);
    }

    #[test]
    fn escaped_slash_in_params_with_clean_method_passes_through_byte_identical() {
        // serde_json wire form of a prompt containing `s/\//_/g`: the `\\/`
        // bytes trip the `\/` prefilter, but the envelope parses the line.
        let raw =
            br#"{"jsonrpc":"2.0","id":1,"method":"session/prompt","params":{"text":"s/\\//_/g"}}"#;
        assert!(raw.windows(2).any(|w| w == br"\/"));
        assert!(serde_json::from_slice::<RawIncomingMessage>(raw).is_ok());
        let expected = raw.to_vec();
        let line = expected.clone();
        let ptr = line.as_ptr();

        let normalized = normalize_json_line(line);

        assert_eq!(normalized, expected);
        // Same allocation: envelope-acceptable lines are never re-serialized.
        assert_eq!(normalized.as_ptr(), ptr);
    }

    #[test]
    fn params_string_escapes_keep_their_semantics() {
        let raw =
            br#"{"jsonrpc":"2.0","id":1,"method":"session\/prompt","params":{"text":"a\/b\nc \"q\" d\\e"}}"#;
        let mut line = raw.to_vec();
        line.push(b'\n');

        let normalized = normalize_json_line(line);

        let value: serde_json::Value = serde_json::from_slice(&normalized).unwrap();
        assert_eq!(value["method"], "session/prompt");
        assert_eq!(value["params"]["text"], "a/b\nc \"q\" d\\e");
    }

    #[test]
    fn crlf_terminator_is_preserved() {
        let mut line = br#"{"id":2,"method":"session\/new"}"#.to_vec();
        line.extend_from_slice(b"\r\n");

        let normalized = normalize_json_line(line);

        assert!(normalized.ends_with(b"\r\n"));
        let value: serde_json::Value =
            serde_json::from_slice(&normalized[..normalized.len() - 2]).unwrap();
        assert_eq!(value["method"], "session/new");
    }

    #[test]
    fn final_line_without_newline_gains_no_newline() {
        let line = br#"{"id":3,"method":"session\/new"}"#.to_vec();

        let normalized = normalize_json_line(line);

        assert_ne!(normalized.last(), Some(&b'\n'));
        let value: serde_json::Value = serde_json::from_slice(&normalized).unwrap();
        assert_eq!(value["method"], "session/new");
    }
}
