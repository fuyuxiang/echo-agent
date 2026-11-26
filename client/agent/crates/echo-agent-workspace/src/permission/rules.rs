//! Native permission rule-string DSL and permission-mode vocabulary.

use std::str::FromStr;

use crate::permission::types::{PatternMode, PermissionRule, PromptPolicy, RuleAction, ToolFilter};

/// Recognized `permissions.defaultMode` values.
///
/// Unknown strings fail `FromStr` and are treated as [`Self::Default`] at the
/// call site (fail-safe) while still claiming the settings scope so a
/// typo in a more-specific file blocks a looser parent mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DefaultPermissionMode {
    Default,
    AcceptEdits,
    Plan,
    /// Classifier-based auto mode. Accepted from settings; seeds the manager's
    /// auto flag (no separate `disableAutoMode` gate yet — intentional).
    Auto,
    DontAsk,
    BypassPermissions,
}

impl FromStr for DefaultPermissionMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "default" => Ok(Self::Default),
            "acceptEdits" => Ok(Self::AcceptEdits),
            "plan" => Ok(Self::Plan),
            "auto" => Ok(Self::Auto),
            "dontAsk" => Ok(Self::DontAsk),
            "bypassPermissions" => Ok(Self::BypassPermissions),
            other => Err(other.to_string()),
        }
    }
}

impl DefaultPermissionMode {
    pub(crate) fn effects(self) -> DefaultModeEffects {
        match self {
            Self::AcceptEdits => DefaultModeEffects {
                accept_edits: true,
                ..Default::default()
            },
            Self::BypassPermissions => DefaultModeEffects {
                bypass_permissions: true,
                ..Default::default()
            },
            Self::Default | Self::Plan => DefaultModeEffects::default(),
            Self::DontAsk => DefaultModeEffects {
                prompt_policy: PromptPolicy::Deny,
                ..Default::default()
            },
            Self::Auto => DefaultModeEffects {
                prompt_policy: PromptPolicy::Auto,
                ..Default::default()
            },
        }
    }
}

/// Effects of a `defaultMode` on rules + prompt policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct DefaultModeEffects {
    pub(crate) prompt_policy: PromptPolicy,
    pub(crate) accept_edits: bool,
    pub(crate) bypass_permissions: bool,
}

// ═════════════════════════════════════════════════════════════════════════════
// Error Type
// ═════════════════════════════════════════════════════════════════════════════

/// Errors from parsing a permission rule string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleParseError {
    /// Tool prefix is recognized but not supported (e.g., "EnterWorktree", "NotebookEdit", "NotebookRead").
    UnsupportedToolPrefix { prefix: String },
    /// Tool prefix is unrecognized.
    UnknownToolPrefix { prefix: String },
    /// Rule string is malformed (e.g., missing closing paren).
    MalformedRule { detail: String },
}

impl std::fmt::Display for RuleParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuleParseError::UnsupportedToolPrefix { prefix } => {
                write!(f, "unsupported tool prefix: {}", prefix)
            }
            RuleParseError::UnknownToolPrefix { prefix } => {
                write!(f, "unknown tool prefix: {}", prefix)
            }
            RuleParseError::MalformedRule { detail } => {
                write!(f, "malformed rule: {}", detail)
            }
        }
    }
}

impl std::error::Error for RuleParseError {}

// ═════════════════════════════════════════════════════════════════════════════
// Rule Parser
// ═════════════════════════════════════════════════════════════════════════════

/// Parse a permission rule string into a native `PermissionRule`.
///
/// Supported tool prefixes:
///   - `Bash(...)` -> `ToolFilter::Bash`
///   - `Read(...)` -> `ToolFilter::Read`
///   - `Edit(...)` / `Write(...)` -> `ToolFilter::Edit`
///   - `MCPTool(...)` -> `ToolFilter::Mcp`
///   - `Grep(...)` / `Glob(...)` -> `ToolFilter::Grep`
///   - `WebFetch(...)` -> `ToolFilter::WebFetch`
///   - `WebSearch(...)` -> `ToolFilter::WebSearch`
///   - No prefix / bare pattern -> `ToolFilter::Any`
///
/// `WebFetch` patterns support a `domain:` prefix (e.g., `WebFetch(domain:example.com)`)
/// which sets `PatternMode::Domain` for host-level matching instead of glob.
///
/// Explicitly unsupported (returns `Err`; the rule is skipped):
///   - `EnterWorktree(...)`
///   - `NotebookEdit` / `NotebookEdit(...)`
///   - `NotebookRead` / `NotebookRead(...)`
///   - Any unrecognized tool prefix
///
/// Pattern semantics:
///   - Supports `*` as prefix/suffix/middle wildcard
///   - Supports `**` for recursive path matching (zero or more segments)
///   - Bash: a trailing `:*` is a prefix idiom — `Bash(cmd:*)` → prefix `cmd`
///
/// Bare tool names (no parentheses) are recognized and treated as wildcard
/// rules for that tool type:
///   - `"Bash"` → `{ Allow, Bash, None }` (matches all bash commands)
///   - `"Edit"` → `{ Allow, Edit, None }` (matches all edit operations)
///
/// Bare-name MCP rules in the `mcp__…` spelling used by `.claude/settings.json`
/// map onto `ToolFilter::Mcp` patterns over EchoAgent's qualified `<server>__<tool>`
/// names (no `mcp__` prefix):
///   - `"mcp__*"` → `{ Mcp, None }` (every MCP tool)
///   - `"mcp__github"` → `{ Mcp, "github__*" }` (every tool on that server)
///   - `"mcp__github__get_issue"` → `{ Mcp, "github__get_issue" }` (exact tool)
///   - `"mcp__github__*"` → `{ Mcp, "github__*" }` (wildcard form)
///
/// Examples:
///   - `Ok`: `"Bash(npm run build)"` → `{ Allow, Bash, "npm run build" }`
///   - `Ok`: `"Read(src/*.rs)"` → `{ Allow, Read, "src/*.rs" }`
///   - `Ok`: `"Read(**/src/**)"` → `{ Allow, Read, "**/src/**" }`
///   - `Ok`: `"Edit(src/**/*.rs)"` → `{ Allow, Edit, "src/**/*.rs" }`
///   - `Ok`: `"Bash"` → `{ Allow, Bash, None }` (bare tool name)
///   - `Err`: `"EnterWorktree(*)"` → `UnsupportedToolPrefix`
///   - `Err`: `"NotebookEdit"` / `"NotebookRead"` → `UnsupportedToolPrefix`
pub fn parse_permission_rule(
    rule: &str,
    action: RuleAction,
) -> Result<PermissionRule, RuleParseError> {
    let rule = rule.trim();

    // Try to extract tool prefix: "ToolName(" ... ")"
    // Use escape-aware parsing to handle \( and \) in content.
    if let Some(open_paren) = find_first_unescaped(rule, b'(') {
        let prefix = &rule[..open_paren];
        let prefix_trimmed = prefix.trim();

        // Find last unescaped closing paren
        let content_and_close = &rule[open_paren + 1..];
        let close_paren = find_last_unescaped(content_and_close, b')').ok_or_else(|| {
            RuleParseError::MalformedRule {
                detail: "missing closing parenthesis".to_string(),
            }
        })?;

        let raw_content = content_and_close[..close_paren].trim();
        // Empty content or standalone wildcard = tool-wide rule.
        let pattern = if raw_content.is_empty() || raw_content == "*" {
            String::new()
        } else {
            unescape_rule_content(raw_content)
        };

        let tool = match tool_name_to_filter(prefix_trimmed) {
            Some(f) => f,
            None if matches!(
                prefix_trimmed,
                "EnterWorktree" | "NotebookEdit" | "NotebookRead"
            ) =>
            {
                return Err(RuleParseError::UnsupportedToolPrefix {
                    prefix: prefix_trimmed.to_string(),
                });
            }
            None => {
                return Err(RuleParseError::UnknownToolPrefix {
                    prefix: prefix_trimmed.to_string(),
                });
            }
        };

        // `Bash(cmd:*)` means "commands starting with cmd"; as a glob it matches nothing.
        let pattern = if tool == ToolFilter::Bash {
            strip_bash_colon_wildcard(pattern)
        } else {
            pattern
        };

        let (pattern, pattern_mode) = strip_domain_prefix(pattern);

        let pattern_opt = if pattern.is_empty() {
            None
        } else {
            Some(pattern)
        };

        Ok(PermissionRule {
            action,
            tool,
            pattern: pattern_opt,
            pattern_mode,
        })
    } else {
        if matches!(rule, "EnterWorktree" | "NotebookEdit" | "NotebookRead") {
            return Err(RuleParseError::UnsupportedToolPrefix {
                prefix: rule.to_string(),
            });
        }

        if let Some(tool) = tool_name_to_filter(rule) {
            return Ok(PermissionRule {
                action,
                tool,
                pattern: None,
                pattern_mode: PatternMode::Glob,
            });
        }

        // `mcp__<server>[__<tool>]` rule (the `.claude/settings.json`
        // spelling). EchoAgent qualifies MCP tools as `<server>__<tool>` with no
        // `mcp__` prefix, so strip it and rewrite into a glob over the
        // qualified name; the literal rule string would otherwise fall through
        // to `ToolFilter::Any` and match nothing. A degenerate bare `mcp__`
        // keeps the old fall-through.
        if let Some(rest) = rule.strip_prefix("mcp__")
            && !rest.is_empty()
        {
            let pattern = if rest == "*" {
                // Matches every MCP tool, so a tool-wide rule (no pattern).
                None
            } else if rest.contains("__") {
                // Already `<server>__<tool>` (or `<server>__*`): the EchoAgent
                // qualified name verbatim. Server names may contain single
                // underscores, but `__` only ever separates server from tool.
                Some(rest.to_string())
            } else {
                // Server-only rule: cover every tool on that server.
                Some(format!("{rest}__*"))
            };
            return Ok(PermissionRule {
                action,
                tool: ToolFilter::Mcp,
                pattern,
                pattern_mode: PatternMode::Glob,
            });
        }

        let pattern_opt = if rule.is_empty() {
            None
        } else {
            Some(rule.to_string())
        };

        Ok(PermissionRule {
            action,
            tool: ToolFilter::Any,
            pattern: pattern_opt,
            pattern_mode: PatternMode::Glob,
        })
    }
}

/// Map a tool name to the native `ToolFilter`.
///
/// Recognized tool-filter names. Returns `None` for unrecognized names.
pub(crate) fn tool_name_to_filter(name: &str) -> Option<ToolFilter> {
    match name {
        "Bash" => Some(ToolFilter::Bash),
        "Read" => Some(ToolFilter::Read),
        "Edit" | "Write" => Some(ToolFilter::Edit),
        "MCPTool" => Some(ToolFilter::Mcp),
        "Grep" | "Glob" => Some(ToolFilter::Grep),
        "WebFetch" => Some(ToolFilter::WebFetch),
        "WebSearch" => Some(ToolFilter::WebSearch),
        _ => None,
    }
}

/// True if the byte at `pos` is NOT preceded by an odd number of backslashes.
pub(crate) fn is_unescaped(bytes: &[u8], pos: usize) -> bool {
    let mut backslashes = 0usize;
    let mut j = pos;
    while j > 0 && bytes[j - 1] == b'\\' {
        backslashes += 1;
        j -= 1;
    }
    backslashes.is_multiple_of(2)
}

pub(crate) fn find_first_unescaped(s: &str, target: u8) -> Option<usize> {
    let bytes = s.as_bytes();
    bytes
        .iter()
        .enumerate()
        .find(|&(i, &b)| b == target && is_unescaped(bytes, i))
        .map(|(i, _)| i)
}

pub(crate) fn find_last_unescaped(s: &str, target: u8) -> Option<usize> {
    let bytes = s.as_bytes();
    (0..bytes.len())
        .rev()
        .find(|&i| bytes[i] == target && is_unescaped(bytes, i))
}

/// Unescape rule content: `\(` → `(`, `\)` → `)`, `\\` → `\`.
pub(crate) fn unescape_rule_content(s: &str) -> String {
    if !s.contains('\\') {
        return s.to_owned();
    }
    // Order matters: unescape parens before backslashes (reverse of escaping).
    s.replace("\\(", "(")
        .replace("\\)", ")")
        .replace("\\\\", "\\")
}

pub(crate) fn strip_domain_prefix(pattern: String) -> (String, PatternMode) {
    match pattern.strip_prefix("domain:") {
        Some(domain) => (domain.to_string(), PatternMode::Domain),
        None => (pattern, PatternMode::Glob),
    }
}

/// Bash `cmd:*` prefix idiom → bare prefix; only the trailing `:*` counts.
/// Deliberately raw-prefix — a superset of a word-boundary `:*` (stricter for deny/ask,
/// wider for allow), matching the evaluator's single prefix regime for every Bash literal.
pub(crate) fn strip_bash_colon_wildcard(pattern: String) -> String {
    match pattern.strip_suffix(":*") {
        Some(prefix) => prefix.to_string(),
        None => pattern,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `mcp__…` rule spellings (the `.claude/settings.json` form) map onto
    /// `ToolFilter::Mcp` globs over EchoAgent's qualified `<server>__<tool>` names.
    #[test]
    fn parse_claude_mcp_rule_forms() {
        for (rule_str, expected_pattern) in [
            ("mcp__github", Some("github__*")),
            ("mcp__github__get_issue", Some("github__get_issue")),
            ("mcp__github__*", Some("github__*")),
            ("mcp__*", None),
        ] {
            let rule = parse_permission_rule(rule_str, RuleAction::Deny).unwrap();
            assert_eq!(rule.action, RuleAction::Deny, "{rule_str}");
            assert_eq!(rule.tool, ToolFilter::Mcp, "{rule_str}");
            assert_eq!(rule.pattern.as_deref(), expected_pattern, "{rule_str}");
            assert_eq!(rule.pattern_mode, PatternMode::Glob, "{rule_str}");
        }
    }
}
