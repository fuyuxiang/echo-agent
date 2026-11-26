use echo_agent_tools::types::{claude_names_for, echo_agent_names_for};
use regex::Regex;

/// A compiled hook matcher for tool names. The pattern semantics are chosen so that
/// `matcher` entries in hooks migrated from other agent CLIs keep firing unchanged:
///
/// - an empty pattern or `"*"` matches every tool;
/// - a "simple" pattern (only `[A-Za-z0-9_|]`, i.e. a plain name or `|`-list) is an
///   **exact** match against each name (after external→EchoAgent alias expansion), NOT a regex;
/// - anything else is an **unanchored** regex (also tested against the tool's external
///   alias names, so e.g. `^Bash$` matches the EchoAgent tool `run_terminal_command`).
///
/// The simple-vs-regex split is deliberate: it avoids anchoring a `|`-alternation (a
/// naive `^a|b|c$` anchors only the first/last term and silently over-matches). Whitespace
/// is significant (not trimmed): `"  "` is a regex that matches nothing.
#[derive(Debug, Clone)]
pub struct HookMatcher {
    kind: MatcherKind,
}

#[derive(Debug, Clone)]
enum MatcherKind {
    All,
    /// Matches no tool names. Used when a configured matcher fails to compile
    /// after deserialization; fail closed rather than widen to match-all.
    Never,
    Exact(Vec<String>),
    Regex(Regex),
}

impl HookMatcher {
    /// Compile a matcher from a user pattern. Errors only when a regex-form pattern is
    /// itself invalid regex (simple/empty/`*` forms never error).
    pub fn new(pattern: &str) -> Result<Self, regex::Error> {
        let kind = if pattern.is_empty() || pattern == "*" {
            MatcherKind::All
        } else if is_simple_form(pattern) {
            MatcherKind::Exact(exact_names(pattern))
        } else {
            MatcherKind::Regex(Regex::new(pattern)?)
        };
        Ok(Self { kind })
    }

    /// Matcher that never matches. Prefer this over `None` on a [`HookSpec`] when a
    /// pattern was configured but could not be compiled (fail-closed).
    pub(crate) fn never() -> Self {
        Self {
            kind: MatcherKind::Never,
        }
    }

    pub fn is_match(&self, tool_name: &str) -> bool {
        match &self.kind {
            MatcherKind::All => true,
            MatcherKind::Never => false,
            MatcherKind::Exact(names) => names.iter().any(|n| n == tool_name),
            MatcherKind::Regex(regex) => {
                regex.is_match(tool_name)
                    || claude_names_for(tool_name).any(|alias| regex.is_match(alias))
            }
        }
    }
}

/// Shared matcher-application rule: a missing matcher or missing value fires
/// (fail-open); otherwise the compiled matcher decides.
pub fn matcher_allows(matcher: Option<&HookMatcher>, value: Option<&str>) -> bool {
    match (matcher, value) {
        (Some(matcher), Some(value)) => matcher.is_match(value),
        _ => true,
    }
}

/// A pattern is "simple" (exact/`|`-list, not regex) when it contains only
/// ASCII alphanumerics, `_`, and `|`.
fn is_simple_form(pattern: &str) -> bool {
    !pattern.is_empty()
        && pattern
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'|')
}

/// Expand a simple-form pattern into the exact set of names it matches: each `|`-term
/// plus any EchoAgent tool names that term aliases (so `"Bash"` also matches
/// `run_terminal_command`), per the shared external-name to EchoAgent registry in
/// `echo-agent-tools`. Empty terms and duplicates are dropped.
fn exact_names(pattern: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut push = |name: &str| {
        if !name.is_empty() && !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
    };
    for term in pattern.split('|') {
        push(term);
        for echo_agent_name in echo_agent_names_for(term) {
            push(echo_agent_name);
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match() {
        let m = HookMatcher::new("run_terminal_command").unwrap();
        assert!(m.is_match("run_terminal_command"));
        assert!(!m.is_match("run_terminal_command_v2"));
        assert!(!m.is_match("other_tool"));
    }

    #[test]
    fn pipe_list_is_exact_per_term() {
        let m = HookMatcher::new("read_file|list_dir").unwrap();
        assert!(m.is_match("read_file"));
        assert!(m.is_match("list_dir"));
        assert!(!m.is_match("grep"));
        // Regression for the old `^a|b$` anchoring bug: terms must not substring-match.
        assert!(!m.is_match("my_read_file"));
        assert!(!m.is_match("list_dir_v2"));
    }

    #[test]
    fn pipe_skips_empty_terms() {
        // Leading/trailing/double pipes contribute no spurious empty-string match.
        let m = HookMatcher::new("|read_file||grep|").unwrap();
        assert!(m.is_match("read_file"));
        assert!(m.is_match("grep"));
        assert!(!m.is_match(""));
    }

    #[test]
    fn regex_form_is_unanchored() {
        // Contains regex metachars -> regex mode, unanchored.
        let m = HookMatcher::new("run_.*").unwrap();
        assert!(m.is_match("run_terminal_command"));
        assert!(m.is_match("xrun_yyy")); // unanchored: substring match
        assert!(!m.is_match("read_file"));
    }

    #[test]
    fn anchored_regex_respects_user_anchors() {
        let m = HookMatcher::new("^run_.*$").unwrap();
        assert!(m.is_match("run_terminal_command"));
        assert!(!m.is_match("xrun_yyy"));
        assert!(!m.is_match("read_file"));
    }

    #[test]
    fn invalid_regex_errors() {
        assert!(HookMatcher::new("[invalid").is_err());
    }

    #[test]
    fn never_matches_nothing() {
        let m = HookMatcher::never();
        assert!(!m.is_match("read_file"));
        assert!(!m.is_match("run_terminal_command"));
        assert!(!m.is_match(""));
        assert!(!m.is_match("*"));
    }

    #[test]
    fn star_and_empty_match_all() {
        for pat in ["*", ""] {
            let m = HookMatcher::new(pat).unwrap();
            assert!(m.is_match("read_file"), "{pat:?} should match all");
            assert!(m.is_match("anything_at_all"), "{pat:?} should match all");
        }
    }

    #[test]
    fn whitespace_matcher_matches_nothing() {
        // Whitespace is NOT trimmed; `"   "` is a regex that matches no
        // real tool name (NOT match-all, which would turn a deny gate into deny-all).
        let m = HookMatcher::new("   ").unwrap();
        assert!(!m.is_match("read_file"));
        assert!(!m.is_match("run_terminal_command"));
    }

    #[test]
    fn claude_bash_matches_echo_agent_tool() {
        let m = HookMatcher::new("Bash").unwrap();
        assert!(m.is_match("Bash")); // external alias name
        assert!(m.is_match("run_terminal_command")); // EchoAgent name
        assert!(!m.is_match("read_file"));
        // Bug-fix regression: exact, not prefix.
        assert!(!m.is_match("run_terminal_command_v2"));
    }

    #[test]
    fn claude_edit_write_matches_echo_agent_tool_exactly() {
        let m = HookMatcher::new("Edit|Write").unwrap();
        assert!(m.is_match("Edit"));
        assert!(m.is_match("Write"));
        assert!(m.is_match("search_replace")); // EchoAgent equivalent
        assert!(m.is_match("hashline_edit")); // second EchoAgent alias
        assert!(!m.is_match("read_file"));
        // The old anchoring bug matched these; the exact-list mode must not.
        assert!(!m.is_match("Editorial"));
        assert!(!m.is_match("my_search_replace"));
    }

    #[test]
    fn regex_against_claude_alias_matches_echo_agent_tool() {
        // A regex written against an external alias still matches the EchoAgent tool
        // (legacy alias-name expansion).
        let m = HookMatcher::new("^Bash$").unwrap();
        assert!(m.is_match("run_terminal_command"));
        assert!(m.is_match("Bash"));
    }
}
