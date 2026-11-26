//! Budget-capped skill listing formatter.
//!
//! Converts a slice of `SkillInfo` into a `<system-reminder>` string that fits
//! within a character budget derived from the model's context window.

use std::collections::HashSet;

use crate::implementations::skills::types::{SkillInfo, SkillScope};
use crate::util::truncate_str_with_marker;

/// Default fraction of the context window allocated to the skill listing (50%).
pub(super) const SKILL_BUDGET_CONTEXT_PERCENT: f64 = 0.5;
/// Default character budget when context window is unknown.
/// Derived from percentage to prevent drift: (200k tokens * 4 bytes/token * 50%).
pub(super) const DEFAULT_CHAR_BUDGET: usize =
    (200_000.0 * 4.0 * SKILL_BUDGET_CONTEXT_PERCENT) as usize;
/// Per-entry cap on description + when_to_use combined. Discovery only — the
/// full skill body is loaded on invocation, so the listing stays terse. Split
/// proportionally between the two fields (see `proportional_budgets`).
const MAX_LISTING_COMBINED_BYTES: usize = 400;
/// Minimum description length before falling back to names-only.
const MIN_DESC_LENGTH: usize = 20;

/// Recognized prefixes for trigger-phrase sections in skill descriptions.
/// Stored pre-lowercased to avoid per-call allocations in `extract_trigger_suffix`.
const TRIGGER_PREFIXES: &[&str] = &[
    "use this skill when",
    "use when",
    "auto-invoke when",
    "invoke when",
    "triggers on",
    "trigger on",
    "called when",
    "must trigger when",
    "must invoke when",
    "must be invoked when",
];

/// Default client-facing name for the skill tool when TemplateRenderer
/// has not resolved one. Single source of truth — used by both
/// `format_announcement` callers and test helpers.
pub(super) const DEFAULT_SKILL_TOOL_NAME: &str = "Skill";

fn listing_header(_tool_name: &str) -> String {
    "The following skills are available for use:\n\n".to_string()
}

/// Whether a skill belongs in the model-facing listing. Native, bundled, and
/// repo/user skills always qualify (they carry a body-derived description);
/// plugin skills must have an authored `description` or `when_to_use`.
fn is_listable(s: &SkillInfo) -> bool {
    let is_plugin = s.plugin_name.is_some() || s.scope == SkillScope::Plugin;
    !is_plugin || s.has_user_specified_description || s.when_to_use.is_some()
}

/// A single skill entry to be rendered in a listing announcement.
struct SkillEntry<'a> {
    name: &'a str,
    description: &'a str,
    when_to_use: Option<&'a str>,
    display_path: String,
}

impl<'a> SkillEntry<'a> {
    /// Return the functional description (trigger suffix stripped when `when_to_use` is set).
    ///
    /// When `when_to_use` is present and the description contains a recognized trigger
    /// prefix, returns the portion before the prefix. Otherwise returns the full description.
    /// Call once and pass the result to `format()` and `proportional_budgets()` to avoid
    /// redundant `extract_trigger_suffix` allocations.
    fn func_desc(&self) -> &str {
        if self.when_to_use.is_some() {
            extract_trigger_suffix(self.description).map_or(self.description, |(before, _)| before)
        } else {
            self.description
        }
    }

    /// Render the entry with description and when_to_use truncated to the
    /// budgets from `proportional_budgets`. `func_desc` must be the result of
    /// `self.func_desc()`.
    fn format(&self, func_desc: &str, desc_budget: usize, wtu_budget: usize) -> String {
        let desc = truncate_str_with_marker(func_desc, desc_budget);
        if let Some(wtu) = self.when_to_use {
            let wtu = truncate_str_with_marker(strip_leading_trigger_prefix(wtu), wtu_budget);
            format!(
                "- {}: {}\n  Use when: {}\n  Absolute path: {}",
                self.name, desc, wtu, self.display_path
            )
        } else {
            format!(
                "- {}: {}\n  Absolute path: {}",
                self.name, desc, self.display_path
            )
        }
    }

    /// Split `total` between description and when_to_use proportionally to their
    /// lengths, with a `MIN_DESC_LENGTH` floor for either field when the budget
    /// allows. No when_to_use → the whole budget goes to the description.
    fn proportional_budgets(&self, func_desc_len: usize, total: usize) -> (usize, usize) {
        let Some(wtu) = self.when_to_use else {
            return (total, 0);
        };
        let func_desc_len = func_desc_len.max(1);
        let combined = func_desc_len + wtu.len().max(1);
        let desc_budget = total * func_desc_len / combined;
        let wtu_budget = total.saturating_sub(desc_budget);
        if desc_budget < MIN_DESC_LENGTH && wtu_budget > MIN_DESC_LENGTH {
            (MIN_DESC_LENGTH, total.saturating_sub(MIN_DESC_LENGTH))
        } else if wtu_budget < MIN_DESC_LENGTH && desc_budget > MIN_DESC_LENGTH {
            (total.saturating_sub(MIN_DESC_LENGTH), MIN_DESC_LENGTH)
        } else {
            (desc_budget, wtu_budget)
        }
    }

    /// Render name only (no description or path).
    fn name_only(&self) -> String {
        format!("- {}", self.name)
    }

    /// Byte length of the fixed overhead (name, path, Use when label) excluding content.
    fn overhead(&self) -> usize {
        // "- " + name + ": " + "\n  Absolute path: " + path + "\n"
        let base = "- ".len()
            + self.name.len()
            + ": ".len()
            + "\n  Absolute path: ".len()
            + self.display_path.len()
            + "\n".len();
        if self.when_to_use.is_some() {
            base + "  Use when: ".len() + "\n".len()
        } else {
            base
        }
    }

    // ── Budgeted XML rendering (echoagent build harness) ─────────────

    /// Render as an `<agent_skill>` XML row with description and when_to_use
    /// truncated to their budgets. When `when_to_use` is present it follows the
    /// description after an em-dash: `description — Use when: trigger phrases`.
    fn format_xml(&self, func_desc: &str, desc_budget: usize, wtu_budget: usize) -> String {
        let desc = truncate_str_with_marker(func_desc, desc_budget);
        if let Some(wtu) = self.when_to_use {
            let wtu = truncate_str_with_marker(strip_leading_trigger_prefix(wtu), wtu_budget);
            format!(
                "<agent_skill fullPath=\"{}\">{} \u{2014} Use when: {}</agent_skill>\n",
                xml_attr_escape(&self.display_path),
                xml_text_escape(&desc),
                xml_text_escape(&wtu),
            )
        } else {
            format!(
                "<agent_skill fullPath=\"{}\">{}</agent_skill>\n",
                xml_attr_escape(&self.display_path),
                xml_text_escape(&desc),
            )
        }
    }

    /// Render as a XML row with name only (no description).
    fn name_only_xml(&self) -> String {
        format!(
            "<agent_skill fullPath=\"{}\">{}</agent_skill>\n",
            xml_attr_escape(&self.display_path),
            xml_text_escape(self.name),
        )
    }

    /// Byte length of the fixed XML overhead excluding description body.
    fn overhead_xml(&self) -> usize {
        let base = "<agent_skill fullPath=\"\"></agent_skill>\n".len()
            + xml_attr_escape(&self.display_path).len();
        if self.when_to_use.is_some() {
            base + " \u{2014} Use when: ".len()
        } else {
            base
        }
    }

    // ── XML rendering ────────────────────────────────────────────

    /// Render as an XML `<agent_skill>` row with the full description.
    ///
    /// Rendering behavior:
    /// - Attribute values: only `"` is escaped to `&quot;`
    /// - Body content: no XML escaping (passed through verbatim)
    /// - Empty/whitespace-only description: self-closing `<agent_skill fullPath="..." />`
    fn format_xml_verbatim(&self) -> String {
        if self.description.trim().is_empty() {
            format!(
                "<agent_skill fullPath=\"{}\" />\n",
                jsx_attr_escape(&self.display_path),
            )
        } else if let Some(wtu) = self.when_to_use {
            format!(
                "<agent_skill fullPath=\"{}\">{} Use when: {}</agent_skill>\n",
                jsx_attr_escape(&self.display_path),
                self.description,
                wtu,
            )
        } else {
            format!(
                "<agent_skill fullPath=\"{}\">{}</agent_skill>\n",
                jsx_attr_escape(&self.display_path),
                self.description,
            )
        }
    }
}

/// A budget-aware collection of skill entries ready to render into a listing.
struct SkillListing<'a>(Vec<SkillEntry<'a>>);

impl<'a> SkillListing<'a> {
    /// Render the listing within `budget` bytes, returning `None` if empty.
    ///
    /// Three tiers:
    /// 1. Full descriptions (each capped at `MAX_LISTING_COMBINED_BYTES`) -- if within budget.
    /// 2. Proportionally shortened descriptions -- if descriptions are the bottleneck.
    /// 3. Names-only with overflow indicator -- when even short descriptions don't fit.
    fn render(self, budget: usize, skill_tool_name: &str) -> Option<String> {
        if self.0.is_empty() {
            return None;
        }
        let header = listing_header(skill_tool_name);
        let header_len = header.len();

        // Tier 1: full descriptions.
        let full_listing = self
            .0
            .iter()
            .map(|e| {
                let fd = e.func_desc();
                let (db, wb) = e.proportional_budgets(fd.len().max(1), MAX_LISTING_COMBINED_BYTES);
                e.format(fd, db, wb)
            })
            .collect::<Vec<_>>()
            .join("\n");
        if header_len + full_listing.len() <= budget {
            return Some(format!("{header}{full_listing}"));
        }

        // Tier 2: shortened descriptions with proportional allocation.
        let total_overhead: usize = self.0.iter().map(|e| e.overhead()).sum();
        let available = budget.saturating_sub(header_len + total_overhead);
        let budget_per_entry = available / self.0.len().max(1);

        if budget_per_entry >= MIN_DESC_LENGTH {
            let listing = self
                .0
                .iter()
                .map(|e| {
                    let fd = e.func_desc();
                    let (db, wb) = e.proportional_budgets(fd.len().max(1), budget_per_entry);
                    e.format(fd, db, wb)
                })
                .collect::<Vec<_>>()
                .join("\n");
            return Some(format!("{header}{listing}"));
        }

        // Tier 3: names-only, drop entries that exceed remaining budget.
        Some(format!(
            "{header}{}",
            self.names_only(budget.saturating_sub(header_len))
        ))
    }

    // ── Budgeted XML rendering (echoagent build harness) ─────────────

    /// Render as XML within `budget` bytes using the three-tier strategy:
    /// 1. Full descriptions (each capped at `MAX_LISTING_COMBINED_BYTES`).
    /// 2. Proportionally shortened descriptions.
    /// 3. Names-only with overflow indicator.
    fn render_xml_budgeted(self, budget: usize, overflow_indicator: bool) -> Option<String> {
        if self.0.is_empty() {
            return None;
        }

        // Tier 1: full descriptions.
        let full_listing: String = self
            .0
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let sep = if i > 0 { "\n" } else { "" };
                let fd = e.func_desc();
                let (db, wb) = e.proportional_budgets(fd.len().max(1), MAX_LISTING_COMBINED_BYTES);
                format!("{sep}{}", e.format_xml(fd, db, wb))
            })
            .collect();
        if full_listing.len() <= budget {
            return Some(full_listing);
        }

        // Tier 2: shortened descriptions with proportional allocation.
        let total_overhead: usize = self
            .0
            .iter()
            .enumerate()
            .map(|(i, e)| e.overhead_xml() + if i > 0 { 1 } else { 0 })
            .sum();
        let available = budget.saturating_sub(total_overhead);
        let budget_per_entry = available / self.0.len().max(1);

        if budget_per_entry >= MIN_DESC_LENGTH {
            let listing: String = self
                .0
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    let sep = if i > 0 { "\n" } else { "" };
                    let fd = e.func_desc();
                    let (db, wb) = e.proportional_budgets(fd.len().max(1), budget_per_entry);
                    format!("{sep}{}", e.format_xml(fd, db, wb))
                })
                .collect();
            return Some(listing);
        }

        // Tier 3: names-only, drop entries that exceed remaining budget.
        Some(self.names_only_xml(budget, overflow_indicator))
    }

    fn names_only_xml(&self, budget: usize, overflow_indicator: bool) -> String {
        let mut out = String::new();
        let mut included = 0usize;
        for (i, e) in self.0.iter().enumerate() {
            let sep = if i > 0 { "\n" } else { "" };
            let row = format!("{sep}{}", e.name_only_xml());
            let need_indicator = overflow_indicator && i + 1 < self.0.len();
            let indicator_reserve = if need_indicator { 64 } else { 0 };
            if out.len() + row.len() + indicator_reserve > budget {
                break;
            }
            out.push_str(&row);
            included += 1;
        }
        let remaining = self.0.len() - included;
        if overflow_indicator && remaining > 0 {
            let dirs = collect_source_dirs(&self.0[included..]);
            out.push_str(&format!(
                "<!-- {remaining} more skills available in {} -->\n",
                dirs.join(", ")
            ));
        }
        out
    }

    // ── Vendor-compat XML rendering ──────────────────────────────

    /// Render as verbatim XML, returning `None` if empty.
    ///
    /// All skills are rendered with full descriptions, no budget-based
    /// truncation, no XML entity escaping of body content.
    fn render_xml_verbatim(self) -> Option<String> {
        if self.0.is_empty() {
            return None;
        }

        let listing: String = self
            .0
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let sep = if i > 0 { "\n" } else { "" };
                format!("{sep}{}", e.format_xml_verbatim())
            })
            .collect();
        Some(listing)
    }

    fn names_only(&self, budget: usize) -> String {
        let mut listing = String::new();
        let mut included = 0usize;
        for e in &self.0 {
            let line = e.name_only();
            let needed = line.len() + if listing.is_empty() { 0 } else { 1 };
            if listing.len() + needed > budget {
                break;
            }
            if !listing.is_empty() {
                listing.push('\n');
            }
            listing.push_str(&line);
            included += 1;
        }
        let remaining = self.0.len() - included;
        if remaining > 0 {
            let dirs = collect_source_dirs(&self.0[included..]);
            listing.push_str(&format!(
                "\n... and {remaining} more skills in {}",
                dirs.join(", ")
            ));
        }
        listing
    }
}

/// Extract the skill source directory from a SKILL.md display path.
///
/// `"/path/.echo-agent/skills/my-skill/SKILL.md"` -> `"/path/.echo-agent/skills/"`
fn skill_source_dir(display_path: &str) -> Option<&str> {
    let p = std::path::Path::new(display_path);
    // SKILL.md -> skill-name dir -> skills dir
    let dir = p.parent()?.parent()?;
    dir.to_str()
}

/// Collect unique source directories from a slice of entries.
fn collect_source_dirs<'a>(entries: &'a [SkillEntry<'a>]) -> Vec<&'a str> {
    let mut seen = HashSet::new();
    let mut dirs = Vec::new();
    for e in entries {
        if let Some(d) = skill_source_dir(&e.display_path)
            && seen.insert(d)
        {
            dirs.push(d);
        }
    }
    dirs
}

/// Split a description at the first recognized trigger prefix.
///
/// Returns `(functional_desc, trigger_phrases)` or `None` if no prefix found
/// or if either part would be empty.
fn extract_trigger_suffix(description: &str) -> Option<(&str, &str)> {
    let desc_lower = description.to_ascii_lowercase();
    let mut best_pos: Option<usize> = None;
    for prefix in TRIGGER_PREFIXES {
        if let Some(pos) = desc_lower.find(prefix) {
            match best_pos {
                Some(prev) if pos < prev => best_pos = Some(pos),
                None => best_pos = Some(pos),
                _ => {}
            }
        }
    }
    let pos = best_pos?;

    let before = description[..pos].trim_end();
    let before = before.strip_suffix('.').unwrap_or(before);
    let triggers = &description[pos..];

    if before.is_empty() || triggers.is_empty() {
        return None;
    }
    Some((before, triggers))
}

/// Strip a leading trigger connective (e.g. "Use when", "Triggers on") from a
/// when-to-use string so the rendered `Use when: {wtu}` label is not duplicated.
///
/// Many internal skills embed their triggers in the `description` as
/// "… Use when asked to X"; [`extract_trigger_suffix`] keeps that connective and
/// the renderer prepends its own `Use when:` label, producing
/// "Use when: Use when asked to X". Stripping the connective yields the clean
/// "Use when: asked to X". Returns a sub-slice of `wtu`, or the trimmed input
/// when no known connective leads.
///
/// A prefix only matches at a word boundary: the connective must be followed by
/// end-of-string or a non-alphanumeric char, so "Use whenever …" is left intact
/// rather than mangled into "ever …".
fn strip_leading_trigger_prefix(wtu: &str) -> &str {
    let trimmed = wtu.trim_start();
    let lower = trimmed.to_ascii_lowercase();
    for prefix in TRIGGER_PREFIXES {
        if let Some(rest) = lower.strip_prefix(prefix) {
            // Word-boundary guard: reject matches where the connective runs into
            // a longer word (e.g. "use when" inside "use whenever").
            if rest.starts_with(|c: char| c.is_alphanumeric()) {
                continue;
            }
            // ASCII lowercasing preserves byte length, so the offset computed on
            // the lowercased copy is valid on the original `trimmed` slice.
            let off = trimmed.len() - rest.len();
            let out = trimmed[off..]
                .trim_start_matches(|c: char| c == ':' || c == ',' || c.is_whitespace());
            if !out.is_empty() {
                return out;
            }
        }
    }
    trimmed
}

/// XML-escape a string for use in attribute values. Replaces the five XML
/// metacharacters (`<`, `>`, `&`, `"`, `'`) with their named entities.
/// Used by the budgeted (echoagent build) XML rendering path.
fn xml_attr_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// XML-escape body text. Same as attribute escape minus the quote handling.
/// Used by the budgeted (echoagent build) XML rendering path.
fn xml_text_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Escape a string for use in XML attribute values, matching the behavior
/// of the alternate XML format: only `"` is replaced with `&quot;`.
///
/// This intentionally does NOT escape `<`, `>`, `&`, or `'` to maintain
/// compatibility with the alternate rendering path.
/// Used by the vendor-compat XML rendering path.
fn jsx_attr_escape(s: &str) -> String {
    s.replace('"', "&quot;")
}

/// Build a `SkillEntry` from a `SkillInfo`, optionally extracting trigger
/// phrases from the description when no explicit `when_to_use` is set.
///
/// The full `description` is always preserved on `SkillEntry`; extraction only
/// populates `when_to_use` without modifying the description text.
fn build_skill_entry<'a>(
    s: &'a SkillInfo,
    real_prefix: Option<&str>,
    display_prefix: Option<&str>,
    extract_triggers: bool,
) -> SkillEntry<'a> {
    let wtu = if let Some(ref wtu) = s.when_to_use {
        Some(wtu.as_str())
    } else if extract_triggers {
        extract_trigger_suffix(&s.description).map(|(_, triggers)| triggers)
    } else {
        None
    };
    SkillEntry {
        name: &s.name,
        description: &s.description,
        when_to_use: wtu,
        display_path: match (real_prefix, display_prefix) {
            (Some(real), Some(display)) => s.path.replace(real, display),
            _ => s.path.clone(),
        },
    }
}

/// Rendering mode for [`format_announcement_xml`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XmlRenderMode {
    /// Verbatim rendering: every skill with its full description; no
    /// budget cap, minimal escaping.
    Verbatim,
    /// EchoAgent build harness: budget-capped three-tier rendering with full
    /// XML entity escaping.
    Budgeted {
        /// Character budget for the listing; `None` uses the default.
        budget_chars: Option<usize>,
        /// Append a "more skills available" line when the budget clips
        /// entries.
        overflow_indicator: bool,
    },
}

/// Render a skill listing as `<agent_skill>` XML rows.
///
/// Output is a sequence of `<agent_skill fullPath="...">description</agent_skill>`
/// rows separated by blank lines (no enclosing `<agent_skills>` envelope --
/// the caller wraps it). The skill name is implicit in the parent directory
/// of `fullPath`.
///
/// Filters out skills with `disable_model_invocation` and those already in
/// `announced` (dedup). Returns `None` if no new skills remain.
pub fn format_announcement_xml(
    skills: &[SkillInfo],
    announced: &mut HashSet<String>,
    real_prefix: Option<&str>,
    display_prefix: Option<&str>,
    mode: XmlRenderMode,
) -> Option<String> {
    let verbatim = mode == XmlRenderMode::Verbatim;
    let listing = SkillListing(
        skills
            .iter()
            .filter(|s| {
                s.enabled
                    && !s.disable_model_invocation
                    // Compat mode renders all skills verbatim; only the echoagent
                    // build path drops description-less plugin skills.
                    && (verbatim || is_listable(s))
                    && announced.insert(s.dedup_key())
            })
            .map(|s| build_skill_entry(s, real_prefix, display_prefix, !verbatim))
            .collect(),
    );
    match mode {
        XmlRenderMode::Verbatim => listing.render_xml_verbatim(),
        XmlRenderMode::Budgeted {
            budget_chars,
            overflow_indicator,
        } => listing.render_xml_budgeted(
            budget_chars.unwrap_or(DEFAULT_CHAR_BUDGET),
            overflow_indicator,
        ),
    }
}

/// Build and render a skill listing announcement within the given budget.
///
/// Filters out skills with `disable_model_invocation` and those already
/// in `announced` (dedup). Returns `None` if no new skills remain.
pub(super) fn format_announcement(
    skills: &[SkillInfo],
    announced: &mut HashSet<String>,
    real_prefix: Option<&str>,
    display_prefix: Option<&str>,
    listing_budget_chars: Option<usize>,
    skill_tool_name: &str,
) -> Option<String> {
    let budget = listing_budget_chars.unwrap_or(DEFAULT_CHAR_BUDGET);
    let listing = SkillListing(
        skills
            .iter()
            .filter(|s| {
                s.enabled
                    && !s.disable_model_invocation
                    && is_listable(s)
                    && announced.insert(s.dedup_key())
            })
            .map(|s| build_skill_entry(s, real_prefix, display_prefix, true))
            .collect(),
    );
    listing.render(budget, skill_tool_name)
}

/// Render the standard skill listing for the post-compaction system-reminder.
///
/// Reuses [`format_announcement`] so the post-compaction listing matches the
/// startup `<system-reminder>` byte-for-byte (standard header, `Use when:`
/// triggers, `Absolute path:`) instead of a hand-rolled divergent format.
/// Lists every enabled, model-invocable skill with no carried-over dedup state.
pub fn format_compaction_skill_listing(skills: &[SkillInfo]) -> Option<String> {
    let mut announced = HashSet::new();
    format_announcement(
        skills,
        &mut announced,
        None,
        None,
        None,
        DEFAULT_SKILL_TOOL_NAME,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::implementations::skills::types::SkillInfo;

    fn skill(name: &str, desc: &str) -> SkillInfo {
        SkillInfo {
            name: name.to_owned(),
            description: desc.to_owned(),
            path: format!("/skills/{name}/SKILL.md"),
            ..SkillInfo::default()
        }
    }

    fn announce(skills: &[SkillInfo], budget: usize) -> Option<String> {
        let mut announced = HashSet::new();
        format_announcement(
            skills,
            &mut announced,
            None,
            None,
            Some(budget),
            DEFAULT_SKILL_TOOL_NAME,
        )
    }

    // ── Edge cases: skill count ──────────────────────────────────

    #[test]
    fn no_skills_returns_none() {
        assert!(announce(&[], 8_000).is_none());
    }

    // ── "Use when" label de-duplication ───────────────────────────

    #[test]
    fn use_when_label_not_duplicated_for_embedded_triggers() {
        // Description embeds the trigger connective ("Use when asked to ...").
        let text = announce(
            &[skill("foo", "Do a thing. Use when asked to verify X.")],
            8_000,
        )
        .unwrap();
        assert!(text.contains("Use when: asked to verify X"), "got:\n{text}");
        assert!(
            !text.contains("Use when: Use when"),
            "duplicate label:\n{text}"
        );
    }

    #[test]
    fn use_when_label_not_duplicated_for_when_to_use_field() {
        // Explicit when-to-use frontmatter field that starts with the connective.
        let s = SkillInfo {
            when_to_use: Some("Use when asked to do Y".to_string()),
            ..skill("bar", "A skill.")
        };
        let text = announce(&[s], 8_000).unwrap();
        assert!(text.contains("Use when: asked to do Y"), "got:\n{text}");
        assert!(
            !text.contains("Use when: Use when"),
            "duplicate label:\n{text}"
        );
    }

    #[test]
    fn strip_leading_trigger_prefix_variants() {
        assert_eq!(
            strip_leading_trigger_prefix("Use when asked to X"),
            "asked to X"
        );
        assert_eq!(strip_leading_trigger_prefix("Triggers on Y"), "Y");
        assert_eq!(
            strip_leading_trigger_prefix("Use this skill when editing Z"),
            "editing Z"
        );
        // No known connective -> returned unchanged (trimmed).
        assert_eq!(
            strip_leading_trigger_prefix("when the user asks"),
            "when the user asks"
        );
        // Word boundary: a connective embedded in a longer word must NOT match.
        assert_eq!(
            strip_leading_trigger_prefix("Use whenever you need X"),
            "Use whenever you need X"
        );
        assert_eq!(
            strip_leading_trigger_prefix("Triggers onboarding flow"),
            "Triggers onboarding flow"
        );
        // Punctuation right after the connective is still a valid boundary.
        assert_eq!(strip_leading_trigger_prefix("Use when: doing X"), "doing X");
    }

    // ── Discovery filter (is_listable) ────────────────────────────

    fn plugin_skill(name: &str, desc: &str) -> SkillInfo {
        SkillInfo {
            scope: SkillScope::Plugin,
            plugin_name: Some("p".to_owned()),
            ..skill(name, desc)
        }
    }

    #[test]
    fn native_skill_without_authored_description_shown() {
        // Native/repo/bundled skills are always advertised (they carry a
        // body-derived description); only plugin skills are gated.
        let text = announce(&[skill("local", "derived from body")], 8_000).unwrap();
        assert!(text.contains("local"));
    }

    #[test]
    fn plugin_skill_without_description_or_wtu_hidden() {
        // Plugin skills need an authored description or when_to_use.
        assert!(announce(&[plugin_skill("noise", "derived")], 8_000).is_none());
    }

    #[test]
    fn plugin_skill_with_authored_description_or_when_to_use_shown() {
        let by_desc = SkillInfo {
            has_user_specified_description: true,
            ..plugin_skill("by-desc", "A real skill")
        };
        let by_wtu = SkillInfo {
            when_to_use: Some("use for X".to_owned()),
            ..plugin_skill("by-wtu", "derived")
        };
        let text = announce(&[by_desc, by_wtu], 8_000).unwrap();
        assert!(text.contains("by-desc"));
        assert!(text.contains("by-wtu"));
    }

    #[test]
    fn proportional_budgets_covers_all_branches() {
        fn entry<'a>(desc: &'a str, wtu: Option<&'a str>) -> SkillEntry<'a> {
            SkillEntry {
                name: "s",
                description: desc,
                when_to_use: wtu,
                display_path: "/p/SKILL.md".to_owned(),
            }
        }
        fn split(e: &SkillEntry, total: usize) -> (usize, usize) {
            e.proportional_budgets(e.func_desc().len().max(1), total)
        }

        // No when_to_use → whole budget to the description.
        assert_eq!(split(&entry("some description", None), 100), (100, 0));

        // Tiny desc vs long wtu → desc floored to MIN_DESC_LENGTH; sum preserved.
        let long = "X".repeat(100);
        let (db, wb) = split(&entry("short", Some(&long)), 200);
        assert_eq!((db, db + wb), (MIN_DESC_LENGTH, 200));

        // Long desc vs tiny wtu → wtu floored to MIN_DESC_LENGTH; sum preserved.
        let big = "Z".repeat(100);
        let (db, wb) = split(&entry(&big, Some("x")), 200);
        assert_eq!((wb, db + wb), (MIN_DESC_LENGTH, 200));

        // Equal content → even split, neither floored.
        let half = "Y".repeat(50);
        assert_eq!(split(&entry(&half, Some(&half)), 200), (100, 100));
    }

    #[test]
    fn verbatim_bypasses_is_listable_filter() {
        // echo-agent-build drops a description-less skill; compat mode renders all.
        let skills = [plugin_skill("noise", "derived")];
        let mut a = HashSet::new();
        assert!(
            format_announcement_xml(
                &skills,
                &mut a,
                None,
                None,
                XmlRenderMode::Budgeted {
                    budget_chars: None,
                    overflow_indicator: false
                }
            )
            .is_none()
        );
        let mut a = HashSet::new();
        let text =
            format_announcement_xml(&skills, &mut a, None, None, XmlRenderMode::Verbatim).unwrap();
        assert!(text.contains("noise"));
    }

    /// End-to-end regression for the original bug: a description-less skill whose
    /// body starts with a table/list must not flatten that structure into the
    /// listing. Exercises the real pipeline (`parse_skill_files` → announce).
    #[test]
    fn smoke_table_first_skills_do_not_flatten_into_listing() {
        use crate::implementations::skills::discovery::parse_skill_files;
        use crate::implementations::skills::types::SkillScope;

        let tmp = tempfile::tempdir().unwrap();
        let write = |name: &str, content: String, scope| {
            let dir = tmp.path().join(name);
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("SKILL.md");
            std::fs::write(&path, content).unwrap();
            (path, scope)
        };
        let fm = |name: &str, body: &str| format!("---\nname: {name}\n---\n\n{body}");

        let skills = parse_skill_files(vec![
            // No heading/prose → name fallback (native, shown).
            write(
                "table-only",
                fm("table-only", "| Col | Lines |\n|---|---|\n| a | 1 |\n"),
                SkillScope::Local,
            ),
            // Leading heading → heading (the table is never reached).
            write(
                "heading-table",
                fm(
                    "heading-table",
                    "# Reference Guide\n\n| Col |\n|---|\n| a |\n",
                ),
                SkillScope::Local,
            ),
            // First prose paragraph wins over the leading H1 title; the list is skipped.
            write(
                "heading-list",
                fm(
                    "heading-list",
                    "# Quarterly Report\n\n- **Authors:** Unknown\n\nSummary prose here.\n",
                ),
                SkillScope::Local,
            ),
            // Plugin without an authored description → hidden.
            write(
                "plugin-no-desc",
                fm("plugin-no-desc", "| Col |\n|---|\n| a |\n"),
                SkillScope::Plugin,
            ),
            // Authored description → shown verbatim.
            write(
                "authored",
                "---\nname: authored\ndescription: A real authored description.\n---\n\n# body\n"
                    .to_string(),
                SkillScope::Local,
            ),
        ]);

        let text = announce(&skills, 100_000).expect("listing renders");

        // No structural markdown reaches the listing.
        assert!(!text.contains("| Col"), "table leaked:\n{text}");
        assert!(!text.contains("**Authors:**"), "list leaked:\n{text}");
        // Clean derived descriptions: first prose paragraph wins, else heading,
        // else name; authored shown.
        assert!(text.contains("Reference Guide")); // heading-table: no prose -> heading
        assert!(text.contains("Summary prose here.")); // heading-list: prose beats the H1 title
        assert!(
            !text.contains("Quarterly Report"),
            "H1 title leaked as desc:\n{text}"
        );
        assert!(text.contains("A real authored description."));
        assert!(text.contains("table-only"));
        // Plugin without a description is hidden.
        assert!(
            !text.contains("plugin-no-desc"),
            "plugin without description leaked:\n{text}"
        );
    }

    // ── verbatim mode ─────────────────────────

    /// Regression: the compat body is description-only, with rows separated
    /// by a blank line. The skill name must NOT appear as a prefix in the
    /// body (it's implicit in the parent directory of `fullPath`).
    #[test]
    fn xml_verbatim_row_shape() {
        let skills = vec![
            skill("commit", "Create a git commit."),
            skill("review", "Review pull request."),
        ];
        let mut announced = HashSet::new();
        let text =
            format_announcement_xml(&skills, &mut announced, None, None, XmlRenderMode::Verbatim)
                .expect("non-empty input must render");
        let expected = "<agent_skill fullPath=\"/skills/commit/SKILL.md\">Create a git commit.</agent_skill>\n\
                        \n\
                        <agent_skill fullPath=\"/skills/review/SKILL.md\">Review pull request.</agent_skill>\n";
        assert_eq!(text, expected);
        assert!(!text.contains("commit:"), "no name prefix in body");
        assert!(!text.contains("review:"), "no name prefix in body");
    }

    /// All skills are rendered regardless of count (no budget limiting).
    /// Matches the alternate behavior where all AgentSkill protos are
    /// rendered verbatim.
    #[test]
    fn xml_verbatim_all_skills_rendered_without_budget_limit() {
        let skills: Vec<SkillInfo> = (0..200)
            .map(|i| skill(&format!("skill-{i:03}"), "Short description."))
            .collect();
        let mut announced = HashSet::new();
        let text =
            format_announcement_xml(&skills, &mut announced, None, None, XmlRenderMode::Verbatim)
                .expect("non-empty input must render");
        let row_count = text.matches("<agent_skill fullPath=").count();
        assert_eq!(row_count, 200, "all 200 skills must be rendered");
    }

    /// Skills with `disable_model_invocation = true` must NEVER appear.
    #[test]
    fn xml_verbatim_filters_disable_model_invocation_skills() {
        let skills = vec![
            SkillInfo {
                disable_model_invocation: true,
                ..skill("private-skill", "Should not appear.")
            },
            skill("public-skill", "Should appear."),
        ];
        let mut announced = HashSet::new();
        let text =
            format_announcement_xml(&skills, &mut announced, None, None, XmlRenderMode::Verbatim)
                .unwrap();
        assert!(!text.contains("private-skill"), "private skill leaked");
        assert!(!text.contains("Should not appear."), "private body leaked");
        assert!(text.contains("public-skill"));
    }

    /// Re-announcing the same skill set must yield no new rows.
    #[test]
    fn xml_verbatim_dedups_across_announcements() {
        let skills = [skill("commit", "Commit.")];
        let mut announced = HashSet::new();
        let first =
            format_announcement_xml(&skills, &mut announced, None, None, XmlRenderMode::Verbatim);
        assert!(first.is_some(), "first announcement must render");
        let second =
            format_announcement_xml(&skills, &mut announced, None, None, XmlRenderMode::Verbatim);
        assert!(
            second.is_none(),
            "re-announcement must yield None, got: {second:?}"
        );
    }

    /// Matches the alternate attribute escaping behavior:
    /// - Attribute values: only `"` is escaped to `&quot;`
    /// - Body content: NO escaping (passed through verbatim)
    #[test]
    fn xml_verbatim_attr_escaping_behavior() {
        let skills = vec![SkillInfo {
            name: "unused-by-row-shape".into(),
            description: "desc with & ampersand and <tags>".into(),
            path: "/skills/dir with \"quotes\"/SKILL.md".into(),
            ..SkillInfo::default()
        }];
        let mut announced = HashSet::new();
        let text =
            format_announcement_xml(&skills, &mut announced, None, None, XmlRenderMode::Verbatim)
                .unwrap();
        assert!(
            text.contains("desc with & ampersand and <tags>"),
            "body must pass through verbatim (no XML escaping): {text}"
        );
        assert!(
            text.contains("&quot;quotes&quot;"),
            "attribute \" must be escaped: {text}"
        );
        assert!(!text.contains("&amp;"), "& must NOT be escaped: {text}");
        assert!(!text.contains("&lt;"), "< must NOT be escaped: {text}");
    }

    /// Long descriptions are NOT truncated in vendor-compat mode.
    #[test]
    fn xml_verbatim_long_descriptions_not_truncated() {
        let long_desc = "A".repeat(500);
        let skills = vec![skill("long", &long_desc)];
        let mut announced = HashSet::new();
        let text =
            format_announcement_xml(&skills, &mut announced, None, None, XmlRenderMode::Verbatim)
                .unwrap();
        assert!(
            text.contains(&long_desc),
            "description must not be truncated"
        );
        assert!(!text.contains("…"), "truncation marker must not appear");
    }

    /// Empty descriptions produce self-closing tags in vendor-compat mode.
    #[test]
    fn xml_verbatim_empty_description_self_closes() {
        let skills = vec![skill("empty", "")];
        let mut announced = HashSet::new();
        let text =
            format_announcement_xml(&skills, &mut announced, None, None, XmlRenderMode::Verbatim)
                .unwrap();
        assert!(
            text.contains("<agent_skill fullPath=\"/skills/empty/SKILL.md\" />"),
            "empty description must produce self-closing tag: {text}"
        );
        assert!(
            !text.contains("</agent_skill>"),
            "must not have closing tag: {text}"
        );
    }

    // ── budgeted mode: echoagent build harness (budgeted XML) ───────────

    /// 200 skills must fit within the default budget.
    #[test]
    fn xml_budgeted_two_hundred_skills_fit_within_default_budget() {
        let skills: Vec<SkillInfo> = (0..200)
            .map(|i| skill(&format!("skill-{i:03}"), "Short description."))
            .collect();
        let mut announced = HashSet::new();
        let text = format_announcement_xml(
            &skills,
            &mut announced,
            None,
            None,
            XmlRenderMode::Budgeted {
                budget_chars: Some(DEFAULT_CHAR_BUDGET),
                overflow_indicator: true,
            },
        )
        .expect("non-empty input must render");
        assert!(
            text.len() <= DEFAULT_CHAR_BUDGET + 100,
            "rendered XML exceeded budget: {} > {}",
            text.len(),
            DEFAULT_CHAR_BUDGET + 100,
        );
    }

    /// Tight budget clips skills and shows overflow indicator.
    #[test]
    fn xml_budgeted_overflow_indicator_appears_when_budget_clips() {
        let skills: Vec<SkillInfo> = (0..50)
            .map(|i| skill(&format!("skill-{i:03}"), "A description."))
            .collect();
        let mut announced = HashSet::new();
        let text = format_announcement_xml(
            &skills,
            &mut announced,
            None,
            None,
            XmlRenderMode::Budgeted {
                budget_chars: Some(500),
                overflow_indicator: true,
            },
        )
        .unwrap();
        let row_count = text.matches("<agent_skill fullPath=").count();
        assert!(row_count > 0, "expected at least one row");
        assert!(row_count < 50, "budget should have clipped");
        assert!(
            text.contains("more skills available"),
            "expected overflow indicator: {text}"
        );
    }

    /// Budgeted XML escapes metacharacters in body and attributes.
    #[test]
    fn xml_budgeted_escapes_special_chars() {
        let skills = vec![SkillInfo {
            name: "test".into(),
            description: "desc with & ampersand and <tags>".into(),
            path: "/skills/dir with \"quotes\"/SKILL.md".into(),
            ..SkillInfo::default()
        }];
        let mut announced = HashSet::new();
        let text = format_announcement_xml(
            &skills,
            &mut announced,
            None,
            None,
            XmlRenderMode::Budgeted {
                budget_chars: Some(8_000),
                overflow_indicator: false,
            },
        )
        .unwrap();
        assert!(
            text.contains("&lt;tags&gt;"),
            "body < and > not escaped: {text}"
        );
        assert!(text.contains("&amp;"), "body & not escaped: {text}");
        assert!(
            text.contains("&quot;quotes&quot;"),
            "attribute \" not escaped: {text}"
        );
    }

    /// Budgeted XML tier 2: descriptions shortened to fit budget.
    #[test]
    fn xml_budgeted_tier2_shortens_descriptions() {
        let skills: Vec<SkillInfo> = (0..20)
            .map(|i| skill(&format!("skill-{i:02}"), &"X".repeat(200)))
            .collect();
        let budget = 3_000;
        let mut announced = HashSet::new();
        let text = format_announcement_xml(
            &skills,
            &mut announced,
            None,
            None,
            XmlRenderMode::Budgeted {
                budget_chars: Some(budget),
                overflow_indicator: false,
            },
        )
        .unwrap();
        assert!(text.contains("skill-00"));
        assert!(text.contains("skill-19"));
        assert!(text.len() <= budget + 50);
    }

    /// Budgeted XML tier 3: name-only under extreme budget.
    #[test]
    fn xml_budgeted_tier3_names_only() {
        let skills: Vec<SkillInfo> = (0..50)
            .map(|i| skill(&format!("skill-{i:02}"), &"Y".repeat(300)))
            .collect();
        let mut announced = HashSet::new();
        let text = format_announcement_xml(
            &skills,
            &mut announced,
            None,
            None,
            XmlRenderMode::Budgeted {
                budget_chars: Some(400),
                overflow_indicator: true,
            },
        )
        .unwrap();
        assert!(text.contains(">skill-00<"), "expected name as body: {text}");
        assert!(!text.contains("YYY"), "description should be absent");
    }

    #[test]
    fn single_skill_within_budget() {
        let skills = [skill("commit", "Commit staged changes with a message.")];
        let text = announce(&skills, 8_000).unwrap();
        assert!(text.contains("commit"));
        assert!(text.contains("Commit staged changes"));
        assert!(text.contains("Absolute path: /skills/commit/SKILL.md"));
        assert!(text.starts_with(&listing_header(DEFAULT_SKILL_TOOL_NAME)));
    }

    #[test]
    fn two_skills_both_appear() {
        let skills = [
            skill("commit", "Commit staged changes."),
            skill("review", "Review pull request."),
        ];
        let text = announce(&skills, 8_000).unwrap();
        assert!(text.contains("commit"));
        assert!(text.contains("review"));
    }

    #[test]
    fn two_hundred_skills_fit_within_default_budget() {
        let skills: Vec<SkillInfo> = (0..200)
            .map(|i| skill(&format!("skill-{i:03}"), "Short description."))
            .collect();
        let text = announce(&skills, DEFAULT_CHAR_BUDGET).unwrap();
        assert!(text.len() <= DEFAULT_CHAR_BUDGET + 50); // allow small slack for overflow line
    }

    // ── Tier 1: full descriptions ────────────────────────────────

    #[test]
    fn tier1_full_descriptions_when_within_budget() {
        let desc = "A".repeat(100);
        let skills = [skill("s", &desc)];
        let text = announce(&skills, 8_000).unwrap();
        // Full description should appear (up to MAX_LISTING_COMBINED_BYTES).
        assert!(text.contains(&desc));
    }

    #[test]
    fn tier1_caps_individual_desc_at_max_listing_chars() {
        // Descriptions longer than MAX_LISTING_COMBINED_BYTES are capped even in tier 1.
        let desc = "B".repeat(MAX_LISTING_COMBINED_BYTES + 200);
        let skills = [skill("s", &desc)];
        let text = announce(&skills, 8_000).unwrap();
        let visible_run = "B".repeat(MAX_LISTING_COMBINED_BYTES - "…".len());
        assert!(text.contains(&format!("{visible_run}…")));
        // The original run length must not appear (would mean no truncation marker).
        assert!(!text.contains(&"B".repeat(MAX_LISTING_COMBINED_BYTES - "…".len() + 1)));
    }

    #[test]
    fn tier1_no_marker_when_description_fits_under_cap() {
        // Description shorter than MAX_LISTING_COMBINED_BYTES -> no truncation,
        // no marker.
        let desc = "A".repeat(100);
        let skills = [skill("s", &desc)];
        let text = announce(&skills, 8_000).unwrap();
        assert!(!text.contains("…"));
    }

    #[test]
    fn tier1_marker_appended_when_description_exceeds_cap() {
        // A description over the combined cap forces a tier-1 cut.
        let desc = "C".repeat(MAX_LISTING_COMBINED_BYTES + 100);
        let skills = [skill("s", &desc)];
        let text = announce(&skills, 8_000).unwrap();
        assert!(
            text.contains("…"),
            "marker should be present after truncation"
        );
    }

    // ── Tier 2: shortened descriptions ──────────────────────────

    #[test]
    fn tier2_shortens_descriptions_to_fit_budget() {
        // 20 skills with 200-char descriptions. Budget tight enough to force tier 2.
        let skills: Vec<SkillInfo> = (0..20)
            .map(|i| skill(&format!("s{i}"), &"X".repeat(200)))
            .collect();
        let budget = 3_000;
        let text = announce(&skills, budget).unwrap();
        // All skills still present but descriptions are shorter.
        assert!(text.contains("s0"));
        assert!(text.contains("s19"));
        assert!(text.len() <= budget + 50);
        // Still has descriptions (not names-only).
        assert!(text.contains(": X"));
    }

    #[test]
    fn tier2_marker_appended_when_description_shortened() {
        // Same setup as tier2_shortens_descriptions_to_fit_budget; descriptions
        // are budget-cut, so each one ends with the truncation marker.
        let skills: Vec<SkillInfo> = (0..20)
            .map(|i| skill(&format!("s{i}"), &"X".repeat(200)))
            .collect();
        let budget = 3_000;
        let text = announce(&skills, budget).unwrap();
        assert!(text.contains("…"));
        assert!(text.len() <= budget + 50);
    }

    // ── Tier 3: names-only ───────────────────────────────────────

    #[test]
    fn tier3_names_only_under_extreme_budget() {
        let skills: Vec<SkillInfo> = (0..50)
            .map(|i| skill(&format!("skill-{i}"), &"Y".repeat(300)))
            .collect();
        let text = announce(&skills, 400).unwrap();
        // No descriptions or paths -- names only.
        assert!(!text.contains(": Y"));
        assert!(!text.contains("Absolute path:"));
        assert!(text.len() <= 500); // 400 + slack for overflow line
    }

    #[test]
    fn tier3_overflow_indicator_when_names_dont_fit() {
        // Tiny budget forces dropping entries and showing "... and N more".
        let skills: Vec<SkillInfo> = (0..100)
            .map(|i| skill(&format!("skill-{i:03}"), "desc"))
            .collect();
        let text = announce(&skills, 200).unwrap();
        assert!(text.contains("... and"), "expected overflow indicator");
        assert!(
            text.contains("in /skills"),
            "overflow should reference skill source dir: {text}"
        );
    }

    #[test]
    fn tier3_no_overflow_indicator_when_all_names_fit() {
        let skills = [skill("a", "desc"), skill("b", "desc")];
        // Budget: header + two name-only lines ("- a\n- b") -- use a tight budget
        // that fits names but not full descriptions + paths.
        let text = announce(&skills, 80).unwrap();
        assert!(!text.contains("... and"));
        assert!(text.contains("- a"));
        assert!(text.contains("- b"));
    }

    // ── Custom tool name ─────────────────────────────────────────

    #[test]
    fn header_does_not_reference_skill_tool() {
        let skills = [skill("commit", "Commit staged changes.")];
        let mut announced = HashSet::new();
        let text = format_announcement(
            &skills,
            &mut announced,
            None,
            None,
            Some(8_000),
            "invoke_skill",
        )
        .unwrap();
        assert!(text.starts_with(&listing_header("invoke_skill")));
        // Header should NOT reference any tool name.
        assert!(!text.contains("invoke_skill tool"));
        assert!(!text.contains("Skill tool"));
    }

    // ── extract_trigger_suffix ─────────────────────────────────

    #[test]
    fn extract_no_prefix_returns_none() {
        assert!(extract_trigger_suffix("A plain description with no triggers.").is_none());
    }

    #[test]
    fn extract_single_prefix_mid_description() {
        let (before, triggers) =
            extract_trigger_suffix("Commit staged changes. Use when ready to push.").unwrap();
        assert_eq!(before, "Commit staged changes");
        assert_eq!(triggers, "Use when ready to push.");
    }

    #[test]
    fn extract_multiple_prefixes_earliest_wins() {
        let desc = "Validates configs. Use when editing YAML. Auto-invoke when deploying.";
        let (before, triggers) = extract_trigger_suffix(desc).unwrap();
        assert_eq!(before, "Validates configs");
        assert_eq!(
            triggers,
            "Use when editing YAML. Auto-invoke when deploying."
        );
    }

    #[test]
    fn extract_prefix_at_start_returns_none() {
        assert!(extract_trigger_suffix("Use when the user asks.").is_none());
    }

    #[test]
    fn extract_prefix_is_entire_description_returns_none() {
        assert!(extract_trigger_suffix("Use when").is_none());
    }

    #[test]
    fn extract_no_period_before_prefix() {
        let (before, triggers) = extract_trigger_suffix("Commit changes Use when ready").unwrap();
        assert_eq!(before, "Commit changes");
        assert_eq!(triggers, "Use when ready");
    }

    #[test]
    fn extract_case_variations() {
        let (b1, t1) = extract_trigger_suffix("Do stuff. use when ready.").unwrap();
        assert_eq!(b1, "Do stuff");
        assert_eq!(t1, "use when ready.");

        let (b2, t2) = extract_trigger_suffix("Do stuff. USE WHEN ready.").unwrap();
        assert_eq!(b2, "Do stuff");
        assert_eq!(t2, "USE WHEN ready.");

        let (b3, t3) = extract_trigger_suffix("Do stuff. mUsT invoke WHEN ready.").unwrap();
        assert_eq!(b3, "Do stuff");
        assert_eq!(t3, "mUsT invoke WHEN ready.");
    }

    #[test]
    fn extract_real_world_babysit_monitor() {
        let desc = "Monitor CI jobs, PR status, or processes with active polling \
                    until completion. Use when user says \"babysit my pr\", \
                    \"monitor my CI\", \"keep checking\".";
        let (before, triggers) = extract_trigger_suffix(desc).unwrap();
        assert!(before.starts_with("Monitor CI jobs"));
        assert!(triggers.starts_with("Use when"));
        assert!(triggers.contains("babysit my pr"));
    }

    #[test]
    fn extract_real_world_memorize() {
        let desc = "Persist knowledge to agent skills, AGENTS.md, or CLAUDE.md. \
                    MUST invoke when user says \"memorize this\", \"remember that\".";
        let (before, triggers) = extract_trigger_suffix(desc).unwrap();
        assert_eq!(
            before,
            "Persist knowledge to agent skills, AGENTS.md, or CLAUDE.md"
        );
        assert!(triggers.starts_with("MUST invoke when"));
    }

    #[test]
    fn extract_real_world_hil_feedback() {
        let desc = "Submit feedback on false positives or inaccurate results. \
                    Use when user reports something is \"false positive\" or \"inaccurate\". \
                    Triggers on /hil-feedback.";
        let (before, triggers) = extract_trigger_suffix(desc).unwrap();
        assert_eq!(
            before,
            "Submit feedback on false positives or inaccurate results"
        );
        assert!(triggers.starts_with("Use when"));
        assert!(triggers.contains("Triggers on"));
    }

    // ── Extraction wiring in format_announcement ──────────────

    #[test]
    fn format_announcement_extracts_triggers_renders_use_when() {
        let skills = [skill(
            "memorize",
            "Persist knowledge. Use when user says memorize.",
        )];
        let mut announced = HashSet::new();
        let text = format_announcement(
            &skills,
            &mut announced,
            None,
            None,
            Some(8_000),
            DEFAULT_SKILL_TOOL_NAME,
        )
        .unwrap();
        // Functional description appears (trigger suffix stripped)
        assert!(
            text.contains("memorize: Persist knowledge"),
            "functional desc should appear: {text}"
        );
        // Triggers rendered as separate Use when: line, with the leading
        // connective stripped so the "Use when:" label is not duplicated.
        assert!(
            text.contains("Use when: user says memorize."),
            "trigger line should appear: {text}"
        );
        assert!(
            !text.contains("Use when: Use when"),
            "label must not be duplicated: {text}"
        );
    }

    #[test]
    fn format_announcement_prefers_explicit_when_to_use() {
        let skills = [SkillInfo {
            when_to_use: Some("Explicit trigger.".into()),
            ..skill(
                "s",
                "Full desc. Use when should be ignored in favor of explicit.",
            )
        }];
        let mut announced = HashSet::new();
        let text = format_announcement(
            &skills,
            &mut announced,
            None,
            None,
            Some(8_000),
            DEFAULT_SKILL_TOOL_NAME,
        )
        .unwrap();
        // Explicit when_to_use rendered on Use when: line
        assert!(
            text.contains("Use when: Explicit trigger."),
            "explicit wtu should render: {text}"
        );
        // Functional description without trigger suffix
        assert!(
            text.contains("s: Full desc"),
            "functional desc should appear: {text}"
        );
        // Embedded trigger text should be stripped from desc line
        assert!(
            !text.contains("should be ignored"),
            "embedded trigger text should be stripped: {text}"
        );
    }

    #[test]
    fn format_announcement_no_extraction_when_no_prefix() {
        let skills = [skill("plain", "A plain description with no triggers.")];
        let mut announced = HashSet::new();
        let text = format_announcement(
            &skills,
            &mut announced,
            None,
            None,
            Some(8_000),
            DEFAULT_SKILL_TOOL_NAME,
        )
        .unwrap();
        assert!(
            text.contains("A plain description with no triggers."),
            "full desc should appear: {text}"
        );
    }

    // ── Extraction wiring in format_announcement_xml ─────────

    #[test]
    fn xml_budgeted_extracts_triggers_from_description() {
        let skills = [skill(
            "memorize",
            "Persist knowledge. Use when user says memorize.",
        )];
        let mut announced = HashSet::new();
        let text = format_announcement_xml(
            &skills,
            &mut announced,
            None,
            None,
            XmlRenderMode::Budgeted {
                budget_chars: Some(8_000),
                overflow_indicator: false,
            },
        )
        .unwrap();
        // Functional desc and triggers rendered with em-dash separator
        assert!(
            text.contains("Persist knowledge"),
            "functional desc should appear: {text}"
        );
        assert!(
            text.contains("\u{2014} Use when:"),
            "em-dash separator should appear: {text}"
        );
        // Leading connective stripped so the "Use when:" label is not duplicated.
        assert!(
            text.contains("Use when: user says memorize."),
            "trigger text should appear: {text}"
        );
        assert!(
            !text.contains("Use when: Use when"),
            "label must not be duplicated: {text}"
        );
    }

    #[test]
    fn xml_verbatim_preserves_full_description_no_extraction() {
        let skills = [skill(
            "memorize",
            "Persist knowledge. Use when user says memorize.",
        )];
        let mut announced = HashSet::new();
        let text =
            format_announcement_xml(&skills, &mut announced, None, None, XmlRenderMode::Verbatim)
                .unwrap();
        // vendor-compat must render the full original description verbatim
        assert!(
            text.contains("Persist knowledge. Use when user says memorize."),
            "verbatim mode must preserve full desc: {text}"
        );
    }

    #[test]
    fn xml_verbatim_renders_explicit_when_to_use_in_description() {
        let skills = [SkillInfo {
            when_to_use: Some("Explicit trigger.".into()),
            ..skill("s", "Full desc.")
        }];
        let mut announced = HashSet::new();
        let text =
            format_announcement_xml(&skills, &mut announced, None, None, XmlRenderMode::Verbatim)
                .unwrap();
        // vendor-compat concatenates explicit when_to_use
        assert!(
            text.contains("Full desc. Use when: Explicit trigger."),
            "explicit wtu should be concatenated: {text}"
        );
    }

    // ── Use when: line rendering ──────────────────────────────

    #[test]
    fn format_renders_use_when_line_with_explicit_when_to_use() {
        let skills = [SkillInfo {
            when_to_use: Some("user says memorize".into()),
            ..skill("memorize", "Persist knowledge.")
        }];
        let text = announce(&skills, 8_000).unwrap();
        assert!(
            text.contains("memorize: Persist knowledge."),
            "desc line: {text}"
        );
        assert!(
            text.contains("  Use when: user says memorize"),
            "Use when: line: {text}"
        );
        assert!(
            text.contains("  Absolute path: "),
            "Absolute path line: {text}"
        );
    }

    #[test]
    fn format_no_use_when_line_without_when_to_use() {
        let skills = [skill("commit", "Commit staged changes.")];
        let text = announce(&skills, 8_000).unwrap();
        assert!(
            text.contains("commit: Commit staged changes."),
            "desc: {text}"
        );
        assert!(!text.contains("Use when:"), "no Use when: line: {text}");
    }

    #[test]
    fn overhead_larger_with_when_to_use() {
        let without = SkillEntry {
            name: "s",
            description: "desc",
            when_to_use: None,
            display_path: "/p/SKILL.md".to_owned(),
        };
        let with = SkillEntry {
            name: "s",
            description: "desc",
            when_to_use: Some("trigger"),
            display_path: "/p/SKILL.md".to_owned(),
        };
        let diff = with.overhead() - without.overhead();
        assert_eq!(diff, "  Use when: ".len() + "\n".len());
    }

    #[test]
    fn format_truncates_long_when_to_use() {
        let wtu = "X".repeat(MAX_LISTING_COMBINED_BYTES + 100);
        let skills = [SkillInfo {
            when_to_use: Some(wtu.clone()),
            ..skill("s", "Short desc.")
        }];
        let text = announce(&skills, 8_000).unwrap();
        assert!(text.contains("Use when:"), "Use when: line present: {text}");
        assert!(text.contains("…"), "truncation marker present: {text}");
        assert!(!text.contains(&wtu), "full wtu should be truncated: {text}");
    }

    #[test]
    fn extracted_triggers_render_as_use_when_line() {
        // Description with embedded triggers — extraction populates when_to_use,
        // format() splits and renders functional desc + Use when: line.
        let skills = [skill(
            "memorize",
            "Persist knowledge. Use when user says memorize.",
        )];
        let text = announce(&skills, 8_000).unwrap();
        assert!(
            text.contains("memorize: Persist knowledge"),
            "functional desc: {text}"
        );
        // Leading connective stripped so the "Use when:" label is not duplicated.
        assert!(
            text.contains("Use when: user says memorize."),
            "trigger line: {text}"
        );
        assert!(
            !text.contains("Use when: Use when"),
            "label must not be duplicated: {text}"
        );
        // Full original description should NOT appear as one string
        assert!(
            !text.contains("Persist knowledge. Use when"),
            "description should be split, not full: {text}"
        );
    }

    #[test]
    fn tier2_with_when_to_use_fits_budget() {
        let wtu = "X".repeat(200);
        let skills: Vec<SkillInfo> = (0..20)
            .map(|i| SkillInfo {
                when_to_use: Some(wtu.clone()),
                ..skill(&format!("s{i}"), &"D".repeat(200))
            })
            .collect();
        let budget = 5_000;
        let text = announce(&skills, budget).unwrap();
        assert!(text.contains("s0"));
        assert!(text.contains("s19"));
        assert!(text.contains("Use when:"));
        assert!(text.len() <= budget);
    }

    // ── XML em-dash separator and proportional budgets ──────

    #[test]
    fn xml_budgeted_em_dash_separator_with_explicit_when_to_use() {
        let skills = [SkillInfo {
            when_to_use: Some("user says memorize".into()),
            ..skill("memorize", "Persist knowledge.")
        }];
        let mut announced = HashSet::new();
        let text = format_announcement_xml(
            &skills,
            &mut announced,
            None,
            None,
            XmlRenderMode::Budgeted {
                budget_chars: Some(8_000),
                overflow_indicator: false,
            },
        )
        .unwrap();
        assert!(
            text.contains("Persist knowledge. \u{2014} Use when: user says memorize"),
            "em-dash separator with explicit wtu: {text}"
        );
    }

    #[test]
    fn xml_budgeted_no_em_dash_without_when_to_use() {
        let skills = [skill("commit", "Commit staged changes.")];
        let mut announced = HashSet::new();
        let text = format_announcement_xml(
            &skills,
            &mut announced,
            None,
            None,
            XmlRenderMode::Budgeted {
                budget_chars: Some(8_000),
                overflow_indicator: false,
            },
        )
        .unwrap();
        assert!(
            text.contains("Commit staged changes."),
            "desc should appear: {text}"
        );
        assert!(
            !text.contains("\u{2014} Use when:"),
            "no em-dash without wtu: {text}"
        );
    }

    #[test]
    fn xml_verbatim_concatenates_explicit_when_to_use() {
        let skills = [SkillInfo {
            when_to_use: Some("user says do it".into()),
            ..skill("s", "Does things.")
        }];
        let mut announced = HashSet::new();
        let text =
            format_announcement_xml(&skills, &mut announced, None, None, XmlRenderMode::Verbatim)
                .unwrap();
        assert!(
            text.contains("Does things. Use when: user says do it"),
            "verbatim mode should concatenate with space: {text}"
        );
        assert!(
            !text.contains('\u{2014}'),
            "verbatim mode should not use em-dash: {text}"
        );
    }

    #[test]
    fn xml_verbatim_no_when_to_use_full_description() {
        let skills = [skill("s", "Full description without triggers.")];
        let mut announced = HashSet::new();
        let text =
            format_announcement_xml(&skills, &mut announced, None, None, XmlRenderMode::Verbatim)
                .unwrap();
        assert!(
            text.contains("Full description without triggers."),
            "full desc verbatim: {text}"
        );
        assert!(
            !text.contains("Use when:"),
            "no Use when without wtu: {text}"
        );
    }

    #[test]
    fn xml_overhead_includes_em_dash_separator() {
        let without = SkillEntry {
            name: "s",
            description: "desc",
            when_to_use: None,
            display_path: "/p/SKILL.md".to_owned(),
        };
        let with_wtu = SkillEntry {
            name: "s",
            description: "desc",
            when_to_use: Some("trigger"),
            display_path: "/p/SKILL.md".to_owned(),
        };
        let diff = with_wtu.overhead_xml() - without.overhead_xml();
        assert_eq!(diff, " \u{2014} Use when: ".len());
    }

    #[test]
    fn xml_budgeted_tier2_proportional_with_when_to_use() {
        let wtu = "X".repeat(200);
        let skills: Vec<SkillInfo> = (0..20)
            .map(|i| SkillInfo {
                when_to_use: Some(wtu.clone()),
                ..skill(&format!("s{i:02}"), &"D".repeat(200))
            })
            .collect();
        let budget = 5_000;
        let mut announced = HashSet::new();
        let text = format_announcement_xml(
            &skills,
            &mut announced,
            None,
            None,
            XmlRenderMode::Budgeted {
                budget_chars: Some(budget),
                overflow_indicator: false,
            },
        )
        .unwrap();
        assert!(text.contains("s00"));
        assert!(text.contains("s19"));
        assert!(text.contains("\u{2014} Use when:"));
        assert!(text.len() <= budget + 50);
    }
}
