//! `$PATH`-aware helper for steering messages that suggest shell tools.
//!
//! Hints that recommend concrete binaries (`jq`, `python3`, `sed`, …) must
//! only name tools that actually exist on the tool server, with no
//! "if available" hedge. Consumers call [`QueryTools::detect`] once and build
//! an example clause via [`examples_clause`]; when nothing relevant is
//! installed the clause is empty so the surrounding hint reads cleanly.
//!
//! Shared by the `use_tool` MCP-dump steer and the `search_replace`
//! Unicode-confusable hint.

/// Query tools present on the tool server's `$PATH`, each `Some(name)` when
/// detected; see [`echo_agent_config::shell::is_command_available`].
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct QueryTools {
    /// `jq`, if present.
    pub(crate) jq: Option<&'static str>,
    /// Resolved python interpreter (`python3` preferred), if any.
    pub(crate) python: Option<&'static str>,
    /// `sed`, if present.
    pub(crate) sed: Option<&'static str>,
    /// `cut`, if present.
    pub(crate) cut: Option<&'static str>,
}

impl QueryTools {
    /// Probe `$PATH` for the tools the steer may suggest; resolved once.
    pub(crate) fn detect() -> Self {
        use echo_agent_config::shell::is_command_available;
        use std::sync::OnceLock;
        static DETECTED: OnceLock<QueryTools> = OnceLock::new();
        *DETECTED.get_or_init(|| {
            let present = |name: &'static str| is_command_available(name).then_some(name);
            Self {
                jq: present("jq"),
                python: present("python3").or_else(|| present("python")),
                sed: present("sed"),
                cut: present("cut"),
            }
        })
    }

    /// Backtick-wrapped tools for querying structured JSON, preference order.
    pub(crate) fn json_tools(self) -> Vec<String> {
        Self::wrap([self.jq, self.python])
    }

    /// Backtick-wrapped tools for slicing/searching a long-line text file.
    pub(crate) fn text_tools(self) -> Vec<String> {
        Self::wrap([self.python, self.sed, self.cut])
    }

    /// Backtick-wrapped tools that can script an in-place file edit
    /// (`cut` is excluded: it slices, it does not edit).
    pub(crate) fn edit_tools(self) -> Vec<String> {
        Self::wrap([self.python, self.sed])
    }

    /// Backtick-wrap the tools that are present, dropping absent ones.
    fn wrap(tools: impl IntoIterator<Item = Option<&'static str>>) -> Vec<String> {
        tools
            .into_iter()
            .flatten()
            .map(|t| format!("`{t}`"))
            .collect()
    }
}

/// `" (e.g. `jq` or `python3`)"` for the present tools, or `""` when none were
/// detected — so a steer never names a tool that isn't installed.
pub(crate) fn examples_clause(tools: &[String]) -> String {
    match tools {
        [] => String::new(),
        [a] => format!(" (e.g. {a})"),
        [a, b] => format!(" (e.g. {a} or {b})"),
        [rest @ .., last] => format!(" (e.g. {}, or {last})", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all() -> QueryTools {
        QueryTools {
            jq: Some("jq"),
            python: Some("python3"),
            sed: Some("sed"),
            cut: Some("cut"),
        }
    }

    #[test]
    fn examples_clause_formats_lists() {
        assert_eq!(examples_clause(&[]), "");
        assert_eq!(examples_clause(&["`jq`".into()]), " (e.g. `jq`)");
        assert_eq!(
            examples_clause(&["`jq`".into(), "`python3`".into()]),
            " (e.g. `jq` or `python3`)"
        );
        assert_eq!(
            examples_clause(&["`python3`".into(), "`sed`".into(), "`cut`".into()]),
            " (e.g. `python3`, `sed`, or `cut`)"
        );
    }

    /// Membership and preference order per tool set; absent tools are dropped
    /// (these are the invariants every consumer steer relies on).
    #[test]
    fn tool_sets_membership_and_order() {
        assert_eq!(all().json_tools(), vec!["`jq`", "`python3`"]);
        assert_eq!(all().text_tools(), vec!["`python3`", "`sed`", "`cut`"]);
        assert_eq!(all().edit_tools(), vec!["`python3`", "`sed`"]);

        let partial = QueryTools {
            jq: None,
            python: None,
            sed: Some("sed"),
            cut: Some("cut"),
        };
        assert_eq!(partial.json_tools(), Vec::<String>::new());
        assert_eq!(partial.text_tools(), vec!["`sed`", "`cut`"]);
        assert_eq!(partial.edit_tools(), vec!["`sed`"]);

        let none = QueryTools::default();
        assert!(none.json_tools().is_empty());
        assert!(none.text_tools().is_empty());
        assert!(none.edit_tools().is_empty());
    }

    /// `cut` can slice but not edit in place — it must never be suggested for
    /// editing a file.
    #[test]
    fn edit_tools_exclude_cut() {
        assert_eq!(all().edit_tools(), vec!["`python3`", "`sed`"]);
    }
}
