//! The bearer fragment every 401-attribution site compares. One definition,
//! because the comparison only works if both sides reduce the token alike.

/// Trailing characters of a bearer that may cross an attribution boundary.
/// The tail, not the head: JWTs share a base64 header and EchoAgent keys a prefix.
pub const BEARER_SUFFIX_LEN: usize = 12;

/// Last [`BEARER_SUFFIX_LEN`] characters, or the whole string if shorter.
/// Counts chars, not bytes: slicing at `len - N` panics mid-character, and
/// tokens from `auth.json` or an auth-provider command are not ASCII-safe.
pub fn bearer_suffix(s: &str) -> &str {
    match s.char_indices().rev().nth(BEARER_SUFFIX_LEN - 1) {
        Some((i, _)) => &s[i..],
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_suffix_semantics() {
        for (input, expected) in [
            // The tail, not the head: JWT headers and EchoAgent key prefixes are shared.
            ("eyJ0eXAiOiJh.shared-head.tail-distinct", "ail-distinct"),
            ("echo-agent-key-aaaaaaaaaaadistinct1", "aaadistinct1"),
            // Shorter than the fragment: returned whole.
            ("abc", "abc"),
            ("", ""),
            ("123456789012", "123456789012"),
            // Characters, not bytes. A byte cut at `len - 12` lands
            // mid-character on the first two and would panic.
            ("éabcdefghijk", "éabcdefghijk"),
            ("ééééééééééééé", "éééééééééééé"),
            ("🔑🔑🔑🔑🔑🔑🔑", "🔑🔑🔑🔑🔑🔑🔑"),
        ] {
            assert_eq!(bearer_suffix(input), expected, "input={input:?}");
        }
    }
}
