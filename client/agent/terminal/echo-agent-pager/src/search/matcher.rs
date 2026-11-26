//! [`TextMatcher`] — a compiled substring/regex query with smart-case matching.

/// How a query string is interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryKind {
    /// Case-insensitive substring (compiled to a regex via `regex::escape`).
    Substring,
    /// User-supplied regex pattern.
    Regex,
}

/// A compiled query that answers whether text matches.
///
/// Both substring and regex queries compile to a `regex::Regex`. Matching is
/// smart-case: case-insensitive unless the query contains an uppercase
/// character (Vim `smartcase` / ripgrep `--smart-case`).
#[derive(Debug, Clone)]
pub struct TextMatcher {
    regex: regex::Regex,
    query: String,
    is_error: bool,
}

impl TextMatcher {
    /// Compile `query` under the given interpretation.
    ///
    /// A regex that fails to compile sets [`is_error`](Self::is_error) and falls
    /// back to a pattern that never matches.
    pub fn new(query: impl Into<String>, kind: QueryKind) -> Self {
        let query = query.into();
        let smart_ci = !query.chars().any(|c| c.is_uppercase());
        let (regex, is_error) = match kind {
            QueryKind::Substring => {
                let escaped = regex::escape(&query);
                let re = regex::RegexBuilder::new(&escaped)
                    .case_insensitive(smart_ci)
                    .build()
                    .unwrap_or_else(|_| regex::Regex::new("(?:)").unwrap());
                (re, false)
            }
            QueryKind::Regex => match regex::RegexBuilder::new(&query)
                .case_insensitive(smart_ci)
                .build()
            {
                Ok(re) => (re, false),
                // `\z.` can never match: an end-of-text anchor followed by a char.
                Err(_) => (regex::Regex::new(r"\z.").unwrap(), true),
            },
        };
        Self {
            regex,
            query,
            is_error,
        }
    }

    /// The raw query string the user typed.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Whether the regex failed to compile (bad user regex).
    pub fn is_error(&self) -> bool {
        self.is_error
    }

    /// The compiled regex, for callers that highlight matches.
    pub fn compiled_regex(&self) -> &regex::Regex {
        &self.regex
    }

    /// Whether `haystack` contains a match.
    pub fn is_match(&self, haystack: &str) -> bool {
        self.regex.is_match(haystack)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substring_is_matched_literally() {
        let m = TextMatcher::new("a.c", QueryKind::Substring);
        assert!(m.is_match("xa.cx"));
        assert!(!m.is_match("abc"));
    }

    #[test]
    fn smart_case_is_insensitive_for_lowercase_query() {
        let m = TextMatcher::new("alpha", QueryKind::Substring);
        assert!(m.is_match("ALPHA"));
        assert!(m.is_match("Alpha"));
    }

    #[test]
    fn smart_case_is_sensitive_when_query_has_uppercase() {
        let m = TextMatcher::new("Alpha", QueryKind::Substring);
        assert!(m.is_match("Alpha"));
        assert!(!m.is_match("alpha"));
    }

    #[test]
    fn smart_case_applies_to_regex_queries() {
        let lower = TextMatcher::new("a.c", QueryKind::Regex);
        assert!(lower.is_match("AXC"));
        let upper = TextMatcher::new("A.C", QueryKind::Regex);
        assert!(!upper.is_match("axc"));
    }

    #[test]
    fn regex_query_matches() {
        let m = TextMatcher::new("^(a|b)$", QueryKind::Regex);
        assert!(!m.is_error());
        assert!(m.is_match("a"));
        assert!(!m.is_match("ab"));
    }

    #[test]
    fn bad_regex_flags_error_and_never_matches() {
        let m = TextMatcher::new("[invalid", QueryKind::Regex);
        assert!(m.is_error());
        assert!(!m.is_match("invalid"));
    }
}
