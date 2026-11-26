//! Shared per-role prompt tool-name rendering for the `/goal` harness.
//!
//! Each `/goal` role (planner, strategist, skeptics/verifier) can run on a
//! different toolset, so its prompt must name that role's OWN tools. This
//! module owns the cross-role machinery that does the naming —
//! [`RoleToolNames`] (built from a `describe_subagent_type` summary or the
//! parent tool bridge), the `name_override` sanitizer, and the
//! `{TOOLSET_TOOLS}` enumerator — so it does not couple a role-specific module
//! (e.g. `goal_planner`) to its siblings.
//!
//! Note: the per-spawn model override [`RoleSpawnOverride`] and the
//! spawn-and-retry-once wrapper live in
//! [`goal_planner`](crate::session::goal_planner); they are a separate concern
//! (model selection / fail-open) from prompt rendering and are not moved here.

use echo_agent_tools::types::tool::ToolKind;

/// Resolved client-facing tool names for a role's prompt placeholders.
///
/// Built parent-side from the role's resolved toolset, with one literal
/// role draws its names from the `describe_subagent_type` summary (so a
/// `name_override` is reflected); an inherit / fail-open role draws them from
/// the parent tool bridge. `{TOOLSET_TOOLS}` enumerates the role's toolset for
/// the explicit path and is empty on the inherit path (so the inherit/default
/// render omits the block). [`Self::apply`] substitutes every placeholder
/// shared across the three role templates.
///
/// Every resolved name is run through [`sanitized_tool_name`] before it is
/// stored, so a `name_override` (registry-validated for uniqueness only, not
/// content) can never splice control chars / markdown / `{…}` tokens into the
/// LLM prompt — an unsafe name falls back to the literal default.
#[derive(Debug, Clone)]
pub(crate) struct RoleToolNames {
    /// `{READ_TOOL}` — `ToolKind::Read`.
    pub read: String,
    /// `{LIST_TOOL}` — `ToolKind::ListDir`.
    pub list: String,
    /// `{SEARCH_TOOL}` — `ToolKind::Search` (grep maps here).
    pub search: String,
    /// `{WRITE_TOOL}` — `ToolKind::Write`, falling back to `ToolKind::Edit`
    /// (the default echo-agent-build host's `search_replace` mutator) when `Write`
    /// is absent from the describe summary.
    pub write: String,
    /// `{EXECUTE_TOOL}` — `ToolKind::Execute` (terminal/bash maps here).
    pub execute: String,
    /// `{WEB_SEARCH_TOOL}` — `ToolKind::WebSearch` (planner template only).
    pub web_search: String,
    /// `{WEB_FETCH_TOOL}` — `ToolKind::WebFetch` (planner template only).
    pub web_fetch: String,
    /// `{TOOLSET_TOOLS}` block (verifier-only placeholder; the planner and
    /// strategist templates do not reference it). Empty on the inherit path.
    pub toolset_tools: String,
}

impl RoleToolNames {
    const READ_FALLBACK: &'static str = "read_file";
    const LIST_FALLBACK: &'static str = "list_dir";
    const SEARCH_FALLBACK: &'static str = "grep";
    const WRITE_FALLBACK: &'static str = "write";
    const EXECUTE_FALLBACK: &'static str = "run_terminal_command";
    const WEB_SEARCH_FALLBACK: &'static str = "web_search";
    const WEB_FETCH_FALLBACK: &'static str = "web_fetch";

    /// Single fallback+sanitize applier shared by every constructor (no
    /// per-path duplication): each name is sanitized, and any `None` or unsafe
    /// name becomes that placeholder's literal default.
    fn from_parts(
        read: Option<String>,
        list: Option<String>,
        search: Option<String>,
        write: Option<String>,
        execute: Option<String>,
        web_search: Option<String>,
        web_fetch: Option<String>,
        toolset_tools: String,
    ) -> Self {
        Self {
            read: sanitized_or_default(read, Self::READ_FALLBACK),
            list: sanitized_or_default(list, Self::LIST_FALLBACK),
            search: sanitized_or_default(search, Self::SEARCH_FALLBACK),
            write: sanitized_or_default(write, Self::WRITE_FALLBACK),
            execute: sanitized_or_default(execute, Self::EXECUTE_FALLBACK),
            web_search: sanitized_or_default(web_search, Self::WEB_SEARCH_FALLBACK),
            web_fetch: sanitized_or_default(web_fetch, Self::WEB_FETCH_FALLBACK),
            toolset_tools,
        }
    }

    /// All-fallback names with an empty `{TOOLSET_TOOLS}` block. Used as the
    /// per-index default when no assignment exists, and by tests.
    pub(crate) fn inherit_defaults() -> Self {
        Self::from_parts(None, None, None, None, None, None, None, String::new())
    }

    /// Inherit / fail-open path: the role runs on the parent toolset, so the
    /// names come from the parent tool bridge (already resolved by the caller).
    /// `{WRITE_TOOL}` falls back to the parent `Edit` tool name when the bridge
    /// has no `Write` — mirroring [`Self::from_summary`], so the inherit / retry
    /// render names the same mutator the subagent actually exposes (e.g.
    /// `search_replace` on the default echo-agent-build host) instead of the literal
    /// `write` default. No `{TOOLSET_TOOLS}` enumeration on the inherit path.
    pub(crate) fn from_parent(
        read: Option<String>,
        list: Option<String>,
        search: Option<String>,
        write: Option<String>,
        edit: Option<String>,
        execute: Option<String>,
        web_search: Option<String>,
        web_fetch: Option<String>,
    ) -> Self {
        Self::from_parts(
            read,
            list,
            search,
            first_safe_tool_name(write, edit),
            execute,
            web_search,
            web_fetch,
            String::new(),
        )
    }

    /// Explicit-pair path: names come from the role's `describe_subagent_type`
    /// summary (the `name_override`-aware client name per kind), and
    /// `{TOOLSET_TOOLS}` enumerates the toolset.
    pub(crate) fn from_summary(
        summary: &echo_agent_tools::implementations::echo_agent_build::task::types::SubagentTypeSummary,
    ) -> Self {
        let get = |kind: ToolKind| summary.tool_names.get(&kind).cloned();
        Self::from_parts(
            get(ToolKind::Read),
            get(ToolKind::ListDir),
            get(ToolKind::Search),
            // The default echo-agent-build host's pre-spawn describe probe exposes only
            // `Edit` (`search_replace`) as the file mutator — the injection-only
            // `write`/`Write` tool is absent there. Without this fallback
            // `{WRITE_TOOL}` would render the literal `write` default instead of
            // the toolset's actual mutator. Picking the first SAFE candidate (not
            // the first present one) keeps a present-but-unsafe `Write` name from
            // shadowing a usable `Edit` name.
            first_safe_tool_name(get(ToolKind::Write), get(ToolKind::Edit)),
            get(ToolKind::Execute),
            get(ToolKind::WebSearch),
            get(ToolKind::WebFetch),
            enumerate_toolset_tools(&summary.tool_names),
        )
    }

    /// Substitute the role-prompt tool placeholders in `template` in a SINGLE
    /// left-to-right pass.
    ///
    /// Invariant: each known `{TOKEN}` is resolved exactly once,
    /// straight from `self` — a substituted value is never re-scanned, so a
    /// resolved name can never be re-expanded into another placeholder
    /// (order-independence). Unknown `{…}` tokens (e.g. the render-time
    /// `{KIND_LENS}` / `{SCRATCH}` placeholders resolved elsewhere) are passed
    /// through untouched. Combined with [`sanitized_tool_name`] (resolved names
    /// contain no `{`/`}`), this is doubly safe. Replacing a placeholder a
    /// template does not contain is a no-op, so all three role templates share
    /// one call even though each names only the subset it uses.
    pub(crate) fn apply(&self, template: &str) -> String {
        let resolve = |token: &str| -> Option<&str> {
            Some(match token {
                "READ_TOOL" => self.read.as_str(),
                "LIST_TOOL" => self.list.as_str(),
                "SEARCH_TOOL" => self.search.as_str(),
                "WRITE_TOOL" => self.write.as_str(),
                "EXECUTE_TOOL" => self.execute.as_str(),
                "WEB_SEARCH_TOOL" => self.web_search.as_str(),
                "WEB_FETCH_TOOL" => self.web_fetch.as_str(),
                "TOOLSET_TOOLS" => self.toolset_tools.as_str(),
                _ => return None,
            })
        };
        let mut out = String::with_capacity(template.len() + 64);
        let mut rest = template;
        while let Some(open) = rest.find('{') {
            out.push_str(&rest[..open]);
            let after = &rest[open + 1..];
            if let Some(close) = after.find('}')
                && let Some(value) = resolve(&after[..close])
            {
                out.push_str(value);
                rest = &after[close + 1..];
                continue;
            }
            // Not a known token (or no closing brace): emit the literal `{`
            // and keep scanning after it, leaving foreign placeholders intact.
            out.push('{');
            rest = after;
        }
        out.push_str(rest);
        out
    }
}

/// `true` when `name` is safe to splice verbatim into an LLM prompt: a
/// non-empty, conservative tool-id charset (ASCII alphanumerics plus `_`, `-`,
/// `.`). This rejects newlines, control chars, backticks, `{`/`}`, spaces, and
/// markdown — a `name_override` is registry-validated for uniqueness only, so
/// its content is untrusted prompt-driving text.
fn is_safe_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// The sanitized client name, or `None` when it is absent/unsafe. Single
/// source of truth for the safe-charset gate (used by both [`RoleToolNames`]
/// fields and [`enumerate_toolset_tools`]).
fn sanitized_tool_name(name: Option<String>) -> Option<String> {
    name.filter(|n| is_safe_tool_name(n))
}

/// The sanitized client name, or `fallback` when it is absent/unsafe.
fn sanitized_or_default(name: Option<String>, fallback: &str) -> String {
    sanitized_tool_name(name).unwrap_or_else(|| fallback.to_string())
}

/// The first of `primary`/`fallback` that passes the safe-charset gate, else
/// `None`. The safety filter runs BEFORE the fallback choice so a present-but-
/// unsafe `primary` (e.g. a bad `Write` `name_override`) cannot shadow a usable
/// `fallback` (`Edit` ⇒ `search_replace`); the `{WRITE_TOOL}` resolution in
/// both [`RoleToolNames::from_summary`] and [`RoleToolNames::from_parent`] uses
/// this so the two renders stay consistent.
fn first_safe_tool_name(primary: Option<String>, fallback: Option<String>) -> Option<String> {
    sanitized_tool_name(primary).or_else(|| sanitized_tool_name(fallback))
}

/// Render the verifier-only `{TOOLSET_TOOLS}` block: a paragraph listing the
/// summary's client-facing names — **one name per classified `ToolKind`**
/// (unclassified tools are absent from `tool_names` and so omitted). Names are
/// sanitized via [`is_safe_tool_name`]; unsafe ones are dropped. Sorted and
/// deduped; empty when nothing safe remains, so the inherit/default render
/// omits the block entirely.
fn enumerate_toolset_tools(tool_names: &std::collections::HashMap<ToolKind, String>) -> String {
    let mut names: Vec<&str> = tool_names
        .values()
        .map(String::as_str)
        .filter(|n| is_safe_tool_name(n))
        .collect();
    if names.is_empty() {
        return String::new();
    }
    names.sort_unstable();
    names.dedup();
    let mut out = String::from("\n\nTools available to you for this review: ");
    for (i, name) in names.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push('`');
        out.push_str(name);
        out.push('`');
    }
    out.push('.');
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use echo_agent_tools::implementations::echo_agent_build::task::types::SubagentTypeSummary;

    /// Build a `SubagentTypeSummary` from `(ToolKind, name)` pairs for the
    /// per-agent_type rendering tests. Shared with the planner / classifier /
    /// strategist render tests.
    pub(crate) fn summary_with(pairs: &[(ToolKind, &str)]) -> SubagentTypeSummary {
        let mut tool_names = std::collections::HashMap::new();
        for (kind, name) in pairs {
            tool_names.insert(*kind, (*name).to_string());
        }
        SubagentTypeSummary {
            can_read: tool_names.contains_key(&ToolKind::Read),
            can_search: tool_names.contains_key(&ToolKind::Search),
            can_execute: tool_names.contains_key(&ToolKind::Execute),
            tool_names,
        }
    }

    /// Shared guard: a regex scan that fails on ANY surviving `{*_TOOL}` /
    /// `{TOOLSET_TOOLS}` token (robust to new tool placeholders, unlike a fixed
    /// marker list). Scoped to the tool-placeholder family so it does NOT
    /// false-positive on render-time placeholders resolved elsewhere
    /// (`{KIND_LENS}`, `{SCRATCH}`, `{PLAN_FILE}`, …) when called on a template
    /// rendered with only `tool_names` applied.
    pub(crate) fn assert_no_tool_placeholders(rendered: &str) {
        let re = regex::Regex::new(r"\{[A-Z_]+_TOOL\}|\{TOOLSET_TOOLS\}").unwrap();
        if let Some(m) = re.find(rendered) {
            panic!(
                "unresolved tool placeholder {:?} in rendered prompt",
                m.as_str(),
            );
        }
    }

    #[test]
    fn inherit_defaults_are_the_literal_fallback_names() {
        let tn = RoleToolNames::inherit_defaults();
        assert_eq!(tn.read, "read_file");
        assert_eq!(tn.list, "list_dir");
        assert_eq!(tn.search, "grep");
        assert_eq!(tn.write, "write");
        assert_eq!(tn.execute, "run_terminal_command");
        assert_eq!(tn.web_search, "web_search");
        assert_eq!(tn.web_fetch, "web_fetch");
        assert_eq!(tn.toolset_tools, "", "inherit path omits the toolset block");
    }

    #[test]
    fn from_summary_uses_echo_agent_build_names() {
        // A echo-agent-build toolset: client names match the literal defaults.
        let tn = RoleToolNames::from_summary(&summary_with(&[
            (ToolKind::Read, "read_file"),
            (ToolKind::ListDir, "list_dir"),
            (ToolKind::Search, "grep"),
            (ToolKind::Write, "write"),
            (ToolKind::Execute, "run_terminal_command"),
        ]));
        assert_eq!(tn.read, "read_file");
        assert_eq!(tn.search, "grep");
        assert_eq!(tn.execute, "run_terminal_command");
    }

    #[test]
    fn from_summary_reflects_cursor_name_overrides() {
        // A toolset with `name_override`d tools renders those names.
        let tn = RoleToolNames::from_summary(&summary_with(&[
            (ToolKind::Read, "cursor_read"),
            (ToolKind::ListDir, "cursor_ls"),
            (ToolKind::Search, "cursor_grep"),
            (ToolKind::Write, "cursor_write"),
            (ToolKind::Execute, "cursor_shell"),
        ]));
        assert_eq!(tn.read, "cursor_read");
        assert_eq!(tn.list, "cursor_ls");
        assert_eq!(tn.search, "cursor_grep");
        assert_eq!(tn.write, "cursor_write");
        assert_eq!(tn.execute, "cursor_shell");
    }

    #[test]
    fn from_summary_resolves_cursor_web_search_name() {
        // The alternate planner toolset exposes WebSearch/WebFetch under
        // the client names "WebSearch"/"WebFetch"; the planner prompt must name
        // THOSE, not the stock fallback — the case that left the alternate
        // planner blind to the web tool and over-scoping from memory alone.
        let tn = RoleToolNames::from_summary(&summary_with(&[
            (ToolKind::WebSearch, "WebSearch"),
            (ToolKind::WebFetch, "WebFetch"),
        ]));
        assert_eq!(tn.web_search, "WebSearch");
        assert_eq!(tn.web_fetch, "WebFetch");
    }

    #[test]
    fn from_parent_maps_web_search_and_fetch_to_distinct_fields() {
        // Distinct values catch a web_search/web_fetch field swap that the
        // all-None fallback cases cannot.
        let tn = RoleToolNames::from_parent(
            None,
            None,
            None,
            None,
            None,
            None,
            Some("WS".into()),
            Some("WF".into()),
        );
        assert_eq!(tn.web_search, "WS");
        assert_eq!(tn.web_fetch, "WF");
    }

    #[test]
    fn web_tools_fall_back_when_absent_from_the_toolset() {
        // A summary / parent bridge without WebSearch/WebFetch ⇒ both resolve to
        // the stock client names, so the planner prompt still names a real tool
        // on the default echo-agent-build host (the stock `web_search`/`web_fetch`).
        let summary = RoleToolNames::from_summary(&summary_with(&[(ToolKind::Read, "rd")]));
        assert_eq!(summary.web_search, "web_search");
        assert_eq!(summary.web_fetch, "web_fetch");
        let parent = RoleToolNames::from_parent(None, None, None, None, None, None, None, None);
        assert_eq!(parent.web_search, "web_search");
        assert_eq!(parent.web_fetch, "web_fetch");
    }

    #[test]
    fn from_summary_write_falls_back_to_edit_on_default_echo_agent_build_host() {
        // Default echo-agent-build host: the pre-spawn describe probe exposes only
        // `Edit` (`search_replace`); `Write` is injection-only and absent. The
        // planner gate accepts this toolset, so `{WRITE_TOOL}` must name the
        // real mutator (`search_replace`), not the literal `write` default.
        let tn = RoleToolNames::from_summary(&summary_with(&[
            (ToolKind::Read, "read_file"),
            (ToolKind::Search, "grep"),
            (ToolKind::Edit, "search_replace"),
        ]));
        assert_eq!(tn.write, "search_replace");
    }

    #[test]
    fn from_summary_write_prefers_write_over_edit_when_both_present() {
        // A toolset exposing both (e.g. the implementer): the explicit
        // `Write` name wins; `Edit` is only the fallback.
        let tn = RoleToolNames::from_summary(&summary_with(&[
            (ToolKind::Write, "cursor_write"),
            (ToolKind::Edit, "cursor_edit"),
        ]));
        assert_eq!(tn.write, "cursor_write");
    }

    #[test]
    fn from_parent_write_falls_back_to_edit_when_bridge_has_no_write() {
        // Default echo-agent-build parent bridge: no `Write`, only `Edit`
        // (`search_replace`). The inherit / fail-open render must name the
        // real mutator, not the literal `write` default — matching
        // `from_summary`'s primary render.
        let tn = RoleToolNames::from_parent(
            Some("read_file".into()),
            Some("list_dir".into()),
            Some("grep".into()),
            None,
            Some("search_replace".into()),
            Some("run_terminal_command".into()),
            None,
            None,
        );
        assert_eq!(tn.write, "search_replace");
    }

    #[test]
    fn from_parent_write_prefers_write_over_edit_when_both_present() {
        // A parent bridge exposing both keeps `Write`; `Edit` is only the
        // fallback.
        let tn = RoleToolNames::from_parent(
            None,
            None,
            None,
            Some("write".into()),
            Some("search_replace".into()),
            None,
            None,
            None,
        );
        assert_eq!(tn.write, "write");
    }

    #[test]
    fn from_parent_write_falls_back_to_default_when_neither_present() {
        // Neither `Write` nor `Edit` on the bridge ⇒ the literal `write`
        // default (unchanged behavior for hosts with no mutator probe).
        let tn = RoleToolNames::from_parent(None, None, None, None, None, None, None, None);
        assert_eq!(tn.write, "write");
    }

    #[test]
    fn from_summary_unsafe_write_falls_through_to_safe_edit() {
        // A present-but-UNSAFE `Write` `name_override` (contains a space) must
        // not shadow a usable `Edit` name: the resolver picks the first SAFE
        // candidate, so `{WRITE_TOOL}` renders the sanitized `search_replace`,
        // not the literal `write` default.
        let tn = RoleToolNames::from_summary(&summary_with(&[
            (ToolKind::Read, "read_file"),
            (ToolKind::Write, "bad write"),
            (ToolKind::Edit, "search_replace"),
        ]));
        assert_eq!(tn.write, "search_replace");
    }

    #[test]
    fn from_parent_unsafe_write_falls_through_to_safe_edit() {
        // Same first-safe-candidate rule on the inherit / parent-bridge path.
        let tn = RoleToolNames::from_parent(
            Some("read_file".into()),
            None,
            None,
            Some("bad write".into()),
            Some("search_replace".into()),
            None,
            None,
            None,
        );
        assert_eq!(tn.write, "search_replace");
    }

    #[test]
    fn write_resolution_uses_literal_default_when_both_candidates_unsafe() {
        // Unsafe `Write` AND unsafe `Edit` ⇒ neither is usable, so both
        // constructors fall back to the literal `write` default.
        let summary = RoleToolNames::from_summary(&summary_with(&[
            (ToolKind::Write, "bad write"),
            (ToolKind::Edit, "bad`edit"),
        ]));
        assert_eq!(summary.write, "write");
        let parent = RoleToolNames::from_parent(
            None,
            None,
            None,
            Some("bad write".into()),
            Some("bad`edit".into()),
            None,
            None,
            None,
        );
        assert_eq!(parent.write, "write");
    }

    #[test]
    fn parent_and_summary_renders_agree_on_default_echo_agent_build_mutator() {
        // The explicit-pair `primary` (from_summary) and inherit/fail-open
        // `fallback` (from_parent) renders must name the SAME mutator on the
        // default echo-agent-build host (Edit-only toolset), so a fail-open retry
        // can never disagree with the first attempt's `{WRITE_TOOL}`.
        let primary = RoleToolNames::from_summary(&summary_with(&[
            (ToolKind::Read, "read_file"),
            (ToolKind::Search, "grep"),
            (ToolKind::Edit, "search_replace"),
        ]));
        let fallback = RoleToolNames::from_parent(
            Some("read_file".into()),
            Some("list_dir".into()),
            Some("grep".into()),
            None,
            Some("search_replace".into()),
            Some("run_terminal_command".into()),
            None,
            None,
        );
        assert_eq!(primary.write, "search_replace");
        assert_eq!(primary.write, fallback.write);
    }

    #[test]
    fn from_summary_falls_back_for_kinds_absent_from_the_toolset() {
        // Only Read present ⇒ the other placeholders take their literal
        // fallbacks (the malformed/partial-summary edge case).
        let tn = RoleToolNames::from_summary(&summary_with(&[(ToolKind::Read, "rd")]));
        assert_eq!(tn.read, "rd");
        assert_eq!(tn.list, "list_dir");
        assert_eq!(tn.search, "grep");
        assert_eq!(tn.write, "write");
        assert_eq!(tn.execute, "run_terminal_command");
    }

    #[test]
    fn toolset_tools_block_enumerates_when_present_and_omits_when_empty() {
        let present = RoleToolNames::from_summary(&summary_with(&[
            (ToolKind::Read, "read_file"),
            (ToolKind::Search, "grep"),
        ]));
        assert!(present.toolset_tools.contains("`grep`"));
        assert!(present.toolset_tools.contains("`read_file`"));
        // Sorted enumeration: grep < read_file.
        assert!(
            present.toolset_tools.find("`grep`").unwrap()
                < present.toolset_tools.find("`read_file`").unwrap()
        );

        let empty = RoleToolNames::from_summary(&SubagentTypeSummary::default());
        assert_eq!(empty.toolset_tools, "", "empty toolset ⇒ no block");
    }

    #[test]
    fn apply_substitutes_every_placeholder_and_leaves_none() {
        let tn = RoleToolNames::from_summary(&summary_with(&[
            (ToolKind::Read, "rd"),
            (ToolKind::ListDir, "ls"),
            (ToolKind::Search, "gr"),
            (ToolKind::Write, "wr"),
            (ToolKind::Execute, "ex"),
            (ToolKind::WebSearch, "ws"),
            (ToolKind::WebFetch, "wf"),
        ]));
        let template = "{READ_TOOL} {LIST_TOOL} {SEARCH_TOOL} {WRITE_TOOL} {EXECUTE_TOOL} \
             {WEB_SEARCH_TOOL} {WEB_FETCH_TOOL}{TOOLSET_TOOLS}";
        let out = tn.apply(template);
        assert!(out.starts_with("rd ls gr wr ex ws wf"));
        assert!(out.contains("Tools available to you for this review:"));
        assert_no_tool_placeholders(&out);
    }

    #[test]
    fn apply_leaves_foreign_placeholders_intact() {
        // Render-time placeholders resolved elsewhere (e.g. `{KIND_LENS}`,
        // `{SCRATCH}`, `{PLAN_FILE}`) and an unknown token must pass through.
        let out = RoleToolNames::inherit_defaults()
            .apply("{READ_TOOL} {KIND_LENS} {SCRATCH} {PLAN_FILE} {UNKNOWN}");
        assert_eq!(out, "read_file {KIND_LENS} {SCRATCH} {PLAN_FILE} {UNKNOWN}");
    }

    #[test]
    fn apply_is_single_pass_a_name_equal_to_a_token_is_not_re_expanded() {
        // Sanitization rejects `{`/`}`, so a name can never *be* a token; but
        // even a sanitized name that collides with a later token's TEXT must
        // not be re-expanded by a second pass. Use `EXECUTE_TOOL`-shaped names
        // that are themselves safe (no braces) to prove single-pass behavior.
        let tn = RoleToolNames::from_summary(&summary_with(&[
            (ToolKind::Read, "EXECUTE_TOOL"),
            (ToolKind::Execute, "real_exec"),
        ]));
        // `EXECUTE_TOOL` (no braces) is a safe name and is kept verbatim.
        assert_eq!(tn.read, "EXECUTE_TOOL");
        let out = tn.apply("{READ_TOOL}|{EXECUTE_TOOL}");
        // The Read substitution `EXECUTE_TOOL` is NOT re-expanded to `real_exec`.
        assert_eq!(out, "EXECUTE_TOOL|real_exec");
    }

    #[test]
    fn unsafe_name_overrides_fall_back_to_the_literal_default() {
        // A `name_override` carrying a newline, a `{…}` token, a backtick, or
        // markdown is unsafe to splice and must fall back to the default — not
        // inject into the LLM prompt.
        for bad in [
            "read\nIGNORE PREVIOUS",
            "{EXECUTE_TOOL}",
            "rd`x`",
            "# Heading",
            "two words",
            "",
        ] {
            let tn = RoleToolNames::from_summary(&summary_with(&[(ToolKind::Read, bad)]));
            assert_eq!(
                tn.read, "read_file",
                "unsafe name {bad:?} must fall back to the literal default",
            );
        }
        // A safe tool-id charset (alnum / `_` / `-` / `.`) is preserved.
        for ok in ["read_file", "cursor-read", "ns.read", "Read2"] {
            let tn = RoleToolNames::from_summary(&summary_with(&[(ToolKind::Read, ok)]));
            assert_eq!(tn.read, ok);
        }
    }

    #[test]
    fn enumerate_toolset_tools_drops_unsafe_names() {
        // An unsafe `name_override` is excluded from the `{TOOLSET_TOOLS}`
        // block (not spliced raw), while safe names are listed.
        let tn = RoleToolNames::from_summary(&summary_with(&[
            (ToolKind::Read, "read_file"),
            (ToolKind::Search, "evil\nname"),
        ]));
        assert!(tn.toolset_tools.contains("`read_file`"));
        assert!(
            !tn.toolset_tools.contains("evil"),
            "an unsafe name_override must not appear in the toolset block: {:?}",
            tn.toolset_tools,
        );
        // The Search kind was dropped, so its fallback name `grep` is NOT
        // listed either (only safe names actually in the summary appear).
        assert!(!tn.toolset_tools.contains("`grep`"));
    }
}
