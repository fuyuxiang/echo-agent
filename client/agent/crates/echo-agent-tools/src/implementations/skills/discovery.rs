//! SKILL.md filesystem discovery and parsing.
//!
//! Provides `discover_skills_for_paths()` for dynamic mid-session discovery
//! and the shared parsing primitives (`parse_skill_files`, `find_skill_paths`,
//! frontmatter parsing) used by both startup and dynamic discovery.

use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use super::skill::extract_skill_body;
use super::types::{SkillInfo, SkillScope};
use crate::types::compat::CompatConfig;

pub const MAX_DESCRIPTION_LEN: usize = 1024;
pub const MAX_NAME_LEN: usize = 64;
pub const MAX_FRONTMATTER_BYTES: usize = 4096;
pub const MAX_BODY_PEEK_BYTES: usize = 2048;
pub const MAX_SKILL_WALK_DEPTH: usize = 5;

/// Subdirectory names that contain skill definitions.
///
/// `skills` is the standard layout (`.echo-agent/skills/`, `.claude/skills/`,
/// `.cursor/skills/`). The product-specific `skills-cursor/` layout is no
/// longer scanned — it pulled vendor default skills into EchoAgent Build sessions.
const SKILL_SUBDIRS: &[&str] = &["skills"];

/// Cursor ships these default skills in `~/.cursor/skills-cursor/`
/// (per its `.cursor-managed-skills-manifest.json` / `.sync-manifest.json`).
/// They are vendor builtins, not user content, so we drop any skill with one
/// of these names discovered under a `/.cursor/` path segment. The denylist is
/// orthogonal to the per-vendor toggle and always applied.
const CURSOR_DEFAULT_SKILLS: &[&str] = &[
    "babysit",
    "canvas",
    "create-hook",
    "create-rule",
    "create-skill",
    "create-subagent",
    "loop",
    "migrate-to-skills",
    "sdk",
    "shell",
    "split-to-prs",
    "statusline",
    "update-cli-config",
    "update-cursor-settings",
];

/// Vendor ships these default skills in-binary (the on-disk `~/.claude/skills`
/// dir is typically empty, so this is best-effort). Any skill with one of these
/// names discovered under a `/.claude/` path segment is dropped.
const CLAUDE_DEFAULT_SKILLS: &[&str] = &["pdf", "docx", "xlsx", "pptx", "skill-creator"];

/// Return true if `name` is a vendor-shipped default skill discovered under the
/// matching vendor's config dir (`/.cursor/` or `/.claude/`).
///
/// The path check ensures a user's own skill that merely shares a denylisted
/// name (e.g. `~/.echo-agent/skills/shell`) is NOT dropped — only skills physically
/// located under the vendor dir are treated as vendor builtins.
fn is_vendor_default_skill(path: &str, name: &str) -> bool {
    let in_cursor = path.contains("/.cursor/") || path.contains("\\.cursor\\");
    let in_claude = path.contains("/.claude/") || path.contains("\\.claude\\");
    (in_cursor && CURSOR_DEFAULT_SKILLS.contains(&name))
        || (in_claude && CLAUDE_DEFAULT_SKILLS.contains(&name))
}

/// Find SKILL.md files inside `skills/` subdirectories, recursively.
pub fn find_skill_paths(dir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for subdir in SKILL_SUBDIRS {
        let skills_dir = dir.join(subdir);
        if skills_dir.is_dir() {
            walk_for_skill_md(&skills_dir, &mut paths, 0);
        }
    }
    paths
}

/// Find `.md` files inside a `commands/` subdirectory.
pub fn find_command_paths(dir: &Path) -> Vec<PathBuf> {
    scan_md_files(&dir.join("commands"))
}

/// Scan a directory for `.md` files (flat, no recursion).
pub fn scan_md_files(dir: &Path) -> Vec<PathBuf> {
    if !dir.is_dir() {
        return vec![];
    }
    let mut paths = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("md") {
                paths.push(path);
            }
        }
    }
    // Sorted: collision handling is first-seen-wins (see `walk_for_skill_md`).
    paths.sort();
    paths
}

/// Discover all SKILL.md files for a skill directory: a `SKILL.md` at the
/// dir's own root (the dir IS a skill — e.g. a plugin manifest `skills` entry
/// or a config path pointing directly at a skill directory) plus the
/// recursive walk of subdirectories.
///
/// Single source of truth for "what loads from a skill dir" — used by the
/// plugin skill loader, the plugin count/name reporters, and config-path
/// collection so they can never drift apart.
pub fn find_skill_md_paths(dir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let self_skill_md = dir.join("SKILL.md");
    if self_skill_md.is_file() {
        paths.push(self_skill_md);
    }
    walk_for_skill_md(dir, &mut paths, 0);
    paths
}

/// Recursively walk directories looking for SKILL.md files.
///
/// Visits entries in lexicographic order: `read_dir` order is
/// filesystem-dependent, and name-collision handling downstream is
/// first-seen-wins, so an unsorted walk picks a nondeterministic winner.
pub fn walk_for_skill_md(dir: &Path, paths: &mut Vec<PathBuf>, depth: usize) {
    if depth > MAX_SKILL_WALK_DEPTH {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut dirs: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        dirs.sort();
        for path in dirs {
            let skill_md_path = path.join("SKILL.md");
            if skill_md_path.is_file() {
                paths.push(skill_md_path);
            }
            walk_for_skill_md(&path, paths, depth + 1);
        }
    }
}

/// Coerce a scalar YAML value to a trimmed, non-empty string. Numbers and bools
/// are stringified; null, blank, and non-scalars yield `None`.
fn coerce_to_string(value: Option<&serde_yaml::Value>) -> Option<String> {
    use serde_yaml::Value;
    match value? {
        Value::String(s) => {
            let t = s.trim();
            (!t.is_empty()).then(|| t.to_string())
        }
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Parse a boolean frontmatter value: only a YAML `true` or the string `"true"`
/// is true; anything else (including absent) is false. Callers apply any
/// field-specific default for the absent case.
fn parse_boolean_frontmatter(value: Option<&serde_yaml::Value>) -> bool {
    use serde_yaml::Value;
    matches!(value, Some(Value::Bool(true)))
        || matches!(value, Some(Value::String(s)) if s == "true")
}

/// Coerce `allowed-tools`: a comma- or space-delimited string, or a YAML list.
/// Separators inside `()` are kept whole so a spec like `Bash(git diff:*)`
/// survives. A wrong type yields `None`.
fn coerce_tool_list(value: Option<&serde_yaml::Value>) -> Option<Vec<String>> {
    use serde_yaml::Value;
    match value? {
        Value::String(s) => Some(split_top_level(s, '(', ')', true)),
        Value::Sequence(seq) => Some(
            seq.iter()
                .filter_map(|v| v.as_str())
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .collect(),
        ),
        _ => None,
    }
}

/// Split on top-level separators, keeping `open`/`close` groups whole (so
/// `{a,b}` or `Bash(a,b)` stays one item). Always splits on commas; also on
/// whitespace when `split_ws` is set (tool lists). Items are trimmed; empties
/// are dropped.
fn split_top_level(input: &str, open: char, close: char, split_ws: bool) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth = 0i32;
    let mut flush = |current: &mut String| {
        let t = current.trim();
        if !t.is_empty() {
            parts.push(t.to_string());
        }
        current.clear();
    };
    for c in input.chars() {
        if c == open {
            depth += 1;
            current.push(c);
        } else if c == close {
            depth -= 1;
            current.push(c);
        } else if depth <= 0 && (c == ',' || (split_ws && c.is_whitespace())) {
            flush(&mut current);
        } else {
            current.push(c);
        }
    }
    flush(&mut current);
    parts
}

/// Coerce `paths:` into split patterns (not yet normalized — see
/// `normalize_skill_paths`). A string is comma-split outside brace groups, so
/// `{a,b}` stays intact for the gitignore matcher to expand; a YAML list is
/// split per item; a wrong type yields `None`.
fn coerce_path_list(value: Option<&serde_yaml::Value>) -> Option<Vec<String>> {
    use serde_yaml::Value;
    match value? {
        Value::String(s) => Some(split_top_level(s, '{', '}', false)),
        Value::Sequence(seq) => Some(
            seq.iter()
                .filter_map(|v| v.as_str())
                .flat_map(|s| split_top_level(s, '{', '}', false))
                .collect(),
        ),
        _ => None,
    }
}

/// Normalize parsed `paths:` patterns: drop a trailing `/**` (gitignore matches
/// the dir and its contents either way) and treat an all-`**` set as
/// unconditional (`None`).
fn normalize_skill_paths(patterns: Vec<String>) -> Option<Vec<String>> {
    let cleaned: Vec<String> = patterns
        .into_iter()
        .map(|p| p.strip_suffix("/**").map(str::to_string).unwrap_or(p))
        .filter(|p| !p.is_empty())
        .collect();
    if cleaned.is_empty() || cleaned.iter().all(|p| p == "**") {
        None
    } else {
        Some(cleaned)
    }
}

/// Parse the `paths:` field into glob patterns: split (`coerce_path_list`) then
/// normalize (`normalize_skill_paths`). `None` when absent or match-all.
fn parse_skill_paths(value: Option<&serde_yaml::Value>) -> Option<Vec<String>> {
    coerce_path_list(value).and_then(normalize_skill_paths)
}

/// Extract `short-description`, `author`, and the remaining string entries from
/// a `metadata:` mapping. Non-string entries (and a non-mapping value) are
/// skipped.
fn parse_metadata(
    value: Option<&serde_yaml::Value>,
) -> (
    Option<String>,
    Option<String>,
    Option<std::collections::HashMap<String, String>>,
) {
    use serde_yaml::Value;
    let Some(Value::Mapping(map)) = value else {
        return (None, None, None);
    };
    let mut short_description = None;
    let mut author = None;
    let mut rest = std::collections::HashMap::new();
    for (k, v) in map {
        let (Some(key), Some(val)) = (k.as_str(), v.as_str()) else {
            continue;
        };
        match key {
            "short-description" => short_description = Some(val.to_string()),
            "author" => author = Some(val.to_string()),
            _ => {
                rest.insert(key.to_string(), val.to_string());
            }
        }
    }
    (
        short_description,
        author,
        (!rest.is_empty()).then_some(rest),
    )
}

#[derive(Debug)]
pub struct ParsedFrontmatter {
    pub name: String,
    pub description: String,
    pub license: Option<String>,
    pub compatibility: Option<String>,
    pub short_description: Option<String>,
    /// Author extracted from `metadata.author` (promoted for typed access in UIs).
    pub author: Option<String>,
    pub metadata: Option<std::collections::HashMap<String, String>>,
    pub argument_hint: Option<String>,
    pub allowed_tools: Option<Vec<String>>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub user_invocable: bool,
    pub disable_model_invocation: bool,
    pub when_to_use: Option<String>,
    /// True when `description` came from frontmatter (vs derived from the body).
    pub has_user_specified_description: bool,
    /// Glob patterns gating when the skill is surfaced. None = always.
    pub paths: Option<Vec<String>>,
}

#[derive(Debug)]
pub enum SkillParseError {
    NoFrontmatter,
    YamlError(String),
    InvalidName(String),
}

/// Normalize a skill name into a slug: lowercase, map any character that is not
/// `[a-z0-9]` (spaces, underscores, dots, etc.) to a hyphen, collapse
/// consecutive hyphens, and trim leading/trailing hyphens. Keeps names with
/// non-slug characters usable (e.g. `tool-v1.2` → `tool-v1-2`) instead of
/// dropping the skill.
pub fn normalize_skill_name(name: &str) -> String {
    let mut result = String::with_capacity(name.len());
    for c in name.trim().chars() {
        let c = c.to_ascii_lowercase();
        let c = if c.is_ascii_lowercase() || c.is_ascii_digit() {
            c
        } else {
            '-'
        };
        if c == '-' && result.ends_with('-') {
            continue;
        }
        result.push(c);
    }
    result.trim_matches('-').to_string()
}
pub fn is_valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Wrap simple `key: value` values that contain YAML indicator characters in
/// double quotes, so e.g. a description with a colon (`Deploy: prod`) or a
/// `{`-leading value parses. Used only as a retry after the first parse fails.
fn quote_problematic_values(frontmatter: &str) -> String {
    fn needs_quoting(v: &str) -> bool {
        v.contains(|c| {
            matches!(
                c,
                '{' | '}' | '[' | ']' | '*' | '&' | '#' | '!' | '|' | '>' | '%' | '@' | '`'
            )
        }) || v.contains(": ")
    }
    frontmatter
        .lines()
        .map(|line| {
            let Some(colon) = line.find(':') else {
                return line.to_string();
            };
            let key = &line[..colon];
            if key.is_empty()
                || !key
                    .bytes()
                    .all(|b| b.is_ascii_alphabetic() || b == b'_' || b == b'-')
            {
                return line.to_string();
            }
            let after = &line[colon + 1..];
            let value = after.trim_start();
            // Require whitespace after the colon and a non-empty value.
            if value.is_empty() || value.len() == after.len() {
                return line.to_string();
            }
            let value = value.trim_end();
            let already_quoted = (value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\''));
            if already_quoted || !needs_quoting(value) {
                return line.to_string();
            }
            let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
            format!("{key}: \"{escaped}\"")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Frontmatter keys the line-based recovery will salvage. Restricted to the
/// listing-relevant scalar fields so list/map fields (`allowed-tools`, `paths`,
/// `metadata`, …) are never mangled into bogus strings on the recovery path.
const RECOVERABLE_KEYS: &[&str] = &["name", "description", "when-to-use", "when_to_use"];

/// Best-effort recovery of a few top-level scalar fields when YAML parsing fails
/// entirely (e.g. a field mistakenly indented under `description:`, which
/// serde_yaml rejects — otherwise dropping the whole frontmatter, `description`
/// and all). Only unindented lines for a [`RECOVERABLE_KEYS`] key are taken, and
/// the body fallback still runs for anything not recovered. A multi-line value
/// keeps only its first line; duplicate keys resolve first-wins.
fn recover_scalar_fields(yaml: &str) -> std::collections::HashMap<String, serde_yaml::Value> {
    let mut map = std::collections::HashMap::new();
    for line in yaml.lines() {
        if line.is_empty() || line.starts_with([' ', '\t']) {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        if !RECOVERABLE_KEYS.contains(&key) {
            continue;
        }
        let raw = value.trim();
        // Strip one surrounding matched quote pair; an unquoted value also drops a
        // trailing ` # comment`, matching how a real YAML parse would.
        let value = raw
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| raw.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or_else(|| {
                raw.split_once(" #")
                    .map_or(raw, |(before, _)| before.trim_end())
            });
        // A bare block-scalar indicator (`|`, `>`, `|-`, `>2`, …) keeps its content on
        // following indented lines we skip, so treat it as empty and let the body
        // fallback supply the description.
        let block_marker = matches!(value.as_bytes().first(), Some(b'|' | b'>'))
            && value[1..]
                .bytes()
                .all(|b| matches!(b, b'+' | b'-' | b'0'..=b'9'));
        if value.is_empty() || block_marker {
            continue;
        }
        map.entry(key.to_string())
            .or_insert_with(|| serde_yaml::Value::String(value.to_string()));
    }
    map
}

/// Cap a string at `max_len` characters. Uses byte length as a fast pre-check.
fn cap_string(s: String, max_len: usize) -> String {
    if s.len() > max_len {
        s.chars().take(max_len).collect()
    } else {
        s
    }
}

pub fn parse_skill_frontmatter(
    content: &str,
    fallback_name: Option<&str>,
) -> Result<ParsedFrontmatter, SkillParseError> {
    let content = content.trim_start();
    if !content.starts_with("---") {
        return Err(SkillParseError::NoFrontmatter);
    }

    let after_first = content.get(3..).ok_or(SkillParseError::NoFrontmatter)?;
    let closing_idx = after_first
        .find("\n---")
        .ok_or(SkillParseError::NoFrontmatter)?;
    let yaml_content = after_first[..closing_idx].trim();

    // Untyped map coerced per-field so one mistyped field never drops its siblings;
    // the quoting retry recovers value-colon syntax errors; the final line-based
    // recovery salvages top-level scalars when YAML fails outright (rather than
    // dropping the whole frontmatter).
    let frontmatter: std::collections::HashMap<String, serde_yaml::Value> = serde_yaml::from_str(
        yaml_content,
    )
    .or_else(|_| serde_yaml::from_str(&quote_problematic_values(yaml_content)))
    .unwrap_or_else(|err| {
        let recovered = recover_scalar_fields(yaml_content);
        tracing::debug!(
            error = %err,
            recovered = recovered.len(),
            "skill frontmatter failed YAML parse; recovered top-level scalar fields line-by-line"
        );
        recovered
    });

    // Prefer the frontmatter `name`, but fall back to the directory name when it
    // is absent or normalizes to an invalid slug, so one bad `name:` field
    // doesn't drop an otherwise-usable skill.
    let fm_name = coerce_to_string(frontmatter.get("name"));
    if fm_name.is_none() && fallback_name.is_none() {
        return Err(SkillParseError::YamlError(
            "missing 'name' and no directory fallback".to_string(),
        ));
    }
    let name = [fm_name.as_deref(), fallback_name]
        .into_iter()
        .flatten()
        .map(normalize_skill_name)
        .find(|n| is_valid_skill_name(n))
        .ok_or_else(|| {
            SkillParseError::InvalidName(
                fm_name
                    .as_deref()
                    .map(normalize_skill_name)
                    .unwrap_or_default(),
            )
        })?;

    let description_value = frontmatter.get("description");
    let coerced_description = coerce_to_string(description_value);
    if coerced_description.is_none()
        && matches!(
            description_value,
            Some(serde_yaml::Value::Sequence(_) | serde_yaml::Value::Mapping(_))
        )
    {
        tracing::debug!(skill = %name, "skill description is not a scalar; using fallback");
    }
    let has_user_specified_description = coerced_description.is_some();
    let description = coerced_description
        .map(|d| cap_string(d, MAX_DESCRIPTION_LEN))
        .unwrap_or_default();

    let when_to_use = coerce_to_string(
        frontmatter
            .get("when-to-use")
            .or_else(|| frontmatter.get("when_to_use")),
    )
    .map(|w| cap_string(w, MAX_DESCRIPTION_LEN));

    let paths = parse_skill_paths(frontmatter.get("paths"));

    let (short_description, author, metadata) = parse_metadata(frontmatter.get("metadata"));

    Ok(ParsedFrontmatter {
        name,
        description,
        license: coerce_to_string(frontmatter.get("license")),
        compatibility: coerce_to_string(frontmatter.get("compatibility")),
        short_description,
        author,
        metadata,
        argument_hint: coerce_to_string(frontmatter.get("argument-hint")),
        allowed_tools: coerce_tool_list(frontmatter.get("allowed-tools")),
        model: coerce_to_string(frontmatter.get("model")),
        effort: coerce_to_string(frontmatter.get("effort")),
        // Absent `user-invocable` defaults to true; `disable-model-invocation` to false.
        user_invocable: frontmatter
            .get("user-invocable")
            .is_none_or(|v| parse_boolean_frontmatter(Some(v))),
        disable_model_invocation: parse_boolean_frontmatter(
            frontmatter.get("disable-model-invocation"),
        ),
        when_to_use,
        has_user_specified_description,
        paths,
    })
}

pub fn read_frontmatter_only(path: &Path) -> std::io::Result<(String, usize)> {
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut frontmatter = String::new();
    let mut total_bytes = 0usize;
    let mut found_opening = false;
    let mut line_buf = String::new();

    loop {
        line_buf.clear();
        let bytes_read = reader.read_line(&mut line_buf)?;
        if bytes_read == 0 {
            break;
        }
        total_bytes += bytes_read;
        if total_bytes > MAX_FRONTMATTER_BYTES {
            break;
        }

        let trimmed = line_buf.trim();
        if !found_opening {
            if trimmed == "---" {
                found_opening = true;
                frontmatter.push_str(&line_buf);
            } else if !trimmed.is_empty() {
                break;
            }
        } else {
            frontmatter.push_str(&line_buf);
            if trimmed == "---" {
                return Ok((frontmatter, total_bytes));
            }
        }
    }

    Ok((frontmatter, total_bytes))
}

/// First top-level prose paragraph of a markdown body (headings excluded).
pub fn extract_first_paragraph(body: &str) -> Option<String> {
    extract_lead_block(body, false)
}

/// First top-level heading or prose paragraph, in document order.
fn extract_description_from_markdown(body: &str) -> Option<String> {
    extract_lead_block(body, true)
}

/// First top-level prose paragraph (and heading, when `include_headings`) in
/// document order, inline markup flattened; tables, lists, code, blockquotes,
/// and image alt text are skipped. Tables must be enabled or GFM tables parse
/// as paragraphs.
fn extract_lead_block(body: &str, include_headings: bool) -> Option<String> {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;

    let mut skip_depth = 0usize;
    let mut image_depth = 0usize;
    let mut capturing = false;
    let mut buf = String::new();

    for event in Parser::new_ext(body, options) {
        match event {
            Event::Start(Tag::List(_) | Tag::BlockQuote(_)) => skip_depth += 1,
            Event::End(TagEnd::List(_) | TagEnd::BlockQuote(_)) => {
                skip_depth = skip_depth.saturating_sub(1)
            }
            Event::Start(Tag::Paragraph) if skip_depth == 0 => {
                capturing = true;
                buf.clear();
            }
            Event::Start(Tag::Heading { .. }) if include_headings && skip_depth == 0 => {
                capturing = true;
                buf.clear();
            }
            Event::End(TagEnd::Paragraph | TagEnd::Heading(_)) if capturing => {
                let text = buf.split_whitespace().collect::<Vec<_>>().join(" ");
                if !text.is_empty() {
                    return Some(cap_string(text, MAX_DESCRIPTION_LEN));
                }
                capturing = false;
            }
            // Alt text is not prose.
            Event::Start(Tag::Image { .. }) => image_depth += 1,
            Event::End(TagEnd::Image) => image_depth = image_depth.saturating_sub(1),
            Event::Text(t) | Event::Code(t) if capturing && image_depth == 0 => buf.push_str(&t),
            Event::SoftBreak | Event::HardBreak if capturing => buf.push(' '),
            _ => {}
        }
    }
    None
}

/// Parse a list of `(path, scope)` pairs into `SkillInfo` values.
///
/// This is the single chokepoint for all skill parsing (startup, dynamic, and
/// host-driven scans), so the vendor-default denylist is applied here to cover
/// every path. See [`is_vendor_default_skill`].
pub fn parse_skill_files(skill_files: Vec<(PathBuf, SkillScope)>) -> Vec<SkillInfo> {
    let mut skills: Vec<SkillInfo> = skill_files
        .into_iter()
        .filter_map(|(path, scope)| {
            let path_str = path.to_string_lossy().to_string();

            let (content, _) = match read_frontmatter_only(&path) {
                Ok(result) => result,
                Err(err) => {
                    tracing::warn!(%err, "failed to read skill");
                    return None;
                }
            };

            let fallback_name = if path.file_name().is_some_and(|n| n == "SKILL.md") {
                path.parent()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
            } else {
                path.file_stem().and_then(|n| n.to_str())
            };

            let mut parsed = match parse_skill_frontmatter(&content, fallback_name) {
                Ok(parsed) => parsed,
                Err(SkillParseError::NoFrontmatter) => {
                    let name = fallback_name.map(normalize_skill_name);
                    match name {
                        Some(name) if is_valid_skill_name(&name) => ParsedFrontmatter {
                            name,
                            description: String::new(),
                            license: None,
                            compatibility: None,
                            short_description: None,
                            author: None,
                            metadata: None,
                            argument_hint: None,
                            allowed_tools: None,
                            model: None,
                            effort: None,
                            user_invocable: true,
                            disable_model_invocation: false,
                            when_to_use: None,
                            has_user_specified_description: false,
                            paths: None,
                        },
                        _ => return None,
                    }
                }
                Err(SkillParseError::YamlError(msg)) => {
                    tracing::warn!("warning: failed to parse skill frontmatter {path_str}: {msg}, using fallback name");
                    let name = fallback_name.map(normalize_skill_name);
                    match name {
                        Some(name) if is_valid_skill_name(&name) => ParsedFrontmatter {
                            name,
                            description: String::new(),
                            license: None,
                            compatibility: None,
                            short_description: None,
                            author: None,
                            metadata: None,
                            argument_hint: None,
                            allowed_tools: None,
                            model: None,
                            effort: None,
                            user_invocable: true,
                            disable_model_invocation: false,
                            when_to_use: None,
                            has_user_specified_description: false,
                            paths: None,
                        },
                        _ => return None,
                    }
                }
                Err(SkillParseError::InvalidName(name)) => {
                    tracing::warn!(
                        "warning: skill at {path_str} has invalid name \"{name}\": \
                         must be lowercase letters, numbers, and hyphens only (max {MAX_NAME_LEN} chars)"
                    );
                    return None;
                }
            };

            if let Some(expected) = fallback_name
                && expected != parsed.name
            {
                tracing::warn!(
                    path = %path_str,
                    declared_name = %parsed.name,
                    expected_name = %expected,
                    "skill name does not match expected name from path"
                );
            }

            if parsed.description.is_empty() {
                if let Ok(full) = std::fs::read_to_string(&path) {
                    let body = extract_skill_body(&full);
                    let peek = if body.len() > MAX_BODY_PEEK_BYTES {
                        let end = crate::util::floor_char_boundary(&body, MAX_BODY_PEEK_BYTES);
                        &body[..end]
                    } else {
                        &body
                    };
                    // Prefer the first prose paragraph; a leading heading is usually
                    // just the skill title (junk as a description). Fall back to a
                    // heading only when there's no prose, then the name.
                    parsed.description = extract_first_paragraph(peek)
                        .or_else(|| extract_description_from_markdown(peek))
                        .unwrap_or_else(|| parsed.name.clone());
                } else {
                    parsed.description = parsed.name.clone();
                }
            }

            Some(SkillInfo {
                name: parsed.name,
                display_name: parsed.metadata.as_ref()
                    .and_then(|metadata| metadata.get("echoagent-workflow-title"))
                    .filter(|title| !title.chars().any(char::is_control))
                    .map(|title| cap_string(title.clone(), 144)),
                description: parsed.description,
                short_description: parsed.short_description,
                author: parsed.author,
                argument_hint: parsed.argument_hint,
                license: parsed.license,
                compatibility: parsed.compatibility,
                metadata: parsed.metadata,
                path: path_str,
                scope,
                config_source: None,
                plugin_name: None,
                plugin_version: None,
                plugin_root: None,
                plugin_data: None,
                allowed_tools: parsed.allowed_tools,
                model: parsed.model,
                effort: parsed.effort,
                user_invocable: parsed.user_invocable,
                disable_model_invocation: parsed.disable_model_invocation,
                when_to_use: parsed.when_to_use,
                has_user_specified_description: parsed.has_user_specified_description,
                paths: parsed.paths,
                enabled: true,
                body: None,
            })
        })
        .collect();

    // Drop vendor-shipped default skills (vendor builtins) found under
    // a `/.cursor/` or `/.claude/` path. Always applied, independent of the
    // per-vendor toggle, so vendor builtins never leak into EchoAgent Build.
    skills.retain(|s| !is_vendor_default_skill(&s.path, &s.name));

    skills
}

/// Walk upward from accessed file paths toward cwd, discovering skill
/// directories not found at startup.
///
/// For each path in `file_paths`, walks from `dirname(path)` upward toward
/// `cwd` (exclusive). At each directory, checks for `.echo-agent/skills/`,
/// `.agents/skills/`, and (gated on `compat.claude.skills`) `.claude/skills/`.
/// Skips already-checked dirs.
///
/// Skill/command roots are **not** filtered by `.gitignore`. Discovery only
/// visits known config roots (`.echo-agent`, `.agents`, `.claude`, …); those are
/// local harness config (often intentionally gitignored), not tree content.
/// Contrast with AGENTS.md discovery, which still respects gitignore. Use
/// `[skills] ignore` to hide a path. Compat loaders likewise load project
/// `.claude/commands` even when ignored.
///
/// `.cursor/` is intentionally NOT scanned in this dynamic path — it never was
/// historically, and preserving that keeps default behavior byte-for-byte. The
/// `.cursor` skills toggle only governs the startup discovery dir list.
///
/// Returns raw `SkillInfo` without surface-specific filtering.
/// Ordering: deepest-first so deeper local skills take precedence.
pub fn discover_skills_for_paths(
    file_paths: &[&Path],
    cwd: &Path,
    git_root: Option<&Path>,
    already_checked: &mut HashSet<PathBuf>,
    compat: CompatConfig,
) -> Vec<SkillInfo> {
    // `.echo-agent` and `.agents` are always scanned; `.claude` is gated on the
    // claude-vendor skills cell. (`.cursor` is excluded here by design — see fn docs.)
    let mut config_dir_names: Vec<&str> = vec![".echo-agent", ".agents"];
    if compat.claude.skills {
        config_dir_names.push(".claude");
    }

    let mut skill_files: Vec<(PathBuf, SkillScope)> = Vec::new();
    let mut seen_canonical_paths = HashSet::new();

    let cwd_canonical = dunce::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());

    for file_path in file_paths {
        let start_dir = if file_path.is_dir() {
            file_path.to_path_buf()
        } else {
            match file_path.parent() {
                Some(p) => p.to_path_buf(),
                None => continue,
            }
        };

        let mut current = Some(start_dir);
        while let Some(dir) = current {
            let dir_canonical = dunce::canonicalize(&dir).unwrap_or_else(|_| dir.clone());

            if dir_canonical == cwd_canonical {
                break;
            }

            if let Some(root) = git_root {
                let root_canonical =
                    dunce::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
                if !dir_canonical.starts_with(&root_canonical) {
                    break;
                }
            }

            if !already_checked.insert(dir_canonical.clone()) {
                current = dir.parent().map(|p| p.to_path_buf());
                continue;
            }

            for config_dir_name in &config_dir_names {
                let config_dir = dir.join(config_dir_name);
                // Skills before commands: skills win name collisions.
                for path in find_skill_paths(&config_dir)
                    .into_iter()
                    .chain(find_command_paths(&config_dir))
                {
                    let canonical = dunce::canonicalize(&path).unwrap_or_else(|_| path.clone());
                    if seen_canonical_paths.insert(canonical) {
                        skill_files.push((path, SkillScope::Local));
                    }
                }
            }

            current = dir.parent().map(|p| p.to_path_buf());
        }
    }

    let mut skills = parse_skill_files(skill_files);

    skills.sort_by(|a, b| {
        let depth_a = Path::new(&a.path).components().count();
        let depth_b = Path::new(&b.path).components().count();
        depth_b.cmp(&depth_a)
    });

    skills
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_one(dir_name: &str, content: &str) -> SkillInfo {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join(dir_name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), content).unwrap();
        let mut skills = parse_skill_files(vec![(skill_dir.join("SKILL.md"), SkillScope::Local)]);
        skills.pop().unwrap()
    }

    #[test]
    fn fallback_skips_structural_body_uses_name() {
        // Description-less skills must not flatten a table or metadata list into
        // the listing; both fall back to the skill name.
        let table = parse_one(
            "t",
            "---\nname: t\n---\n\n| Chunk | Lines |\n|---|---|\n| a.md | 62 |\n",
        );
        assert_eq!(table.description, "t");
        assert!(!table.has_user_specified_description);

        let list = parse_one(
            "l",
            "---\nname: l\n---\n\n- **Authors:** Unknown\n- **Topics:** Handling\n",
        );
        assert_eq!(list.description, "l");
    }

    #[test]
    fn fallback_prefers_prose_paragraph_over_heading() {
        // A leading H1 is usually just the skill title (redundant with the name,
        // no triggers) — junk as a description — so the first prose paragraph wins
        // even when a heading precedes it.
        let skill = parse_one(
            "h",
            "---\nname: h\n---\n\n# Title\n\nDoes a real thing.\n\n## Section\n",
        );
        assert_eq!(skill.description, "Does a real thing.");
        assert!(!skill.has_user_specified_description);

        // …and the first prose paragraph when no heading precedes it.
        let skill = parse_one("p", "---\nname: p\n---\n\nDoes a real thing.\n\n# Title\n");
        assert_eq!(skill.description, "Does a real thing.");

        // Heading-only body (no prose) still falls back to the heading.
        let skill = parse_one("o", "---\nname: o\n---\n\n# Only A Title\n");
        assert_eq!(skill.description, "Only A Title");
    }

    #[test]
    fn recovers_frontmatter_description_when_a_field_is_accidentally_indented() {
        // Real-world bug (cursorbench): a field accidentally indented under
        // `description:` makes the whole frontmatter invalid YAML (a scanner
        // error). The parser must still recover the frontmatter `description`
        // rather than silently dropping the entire frontmatter and rendering a
        // junk body-derived description in the skill listing.
        let skill = parse_one(
            "cb",
            concat!(
                "---\n",
                "name: cb\n",
                "description: Go from an EAPI deployment name to CursorBench metrics.\n",
                "  Use when: \"cursorbench\", \"compute cursorbench\"\n",
                "---\n",
                "\n",
                "# CursorBench: EAPI deployment to metrics\n",
                "\n",
                "The flow body.\n",
            ),
        );
        assert_eq!(
            skill.description,
            "Go from an EAPI deployment name to CursorBench metrics."
        );
        assert!(skill.has_user_specified_description);
    }

    #[test]
    fn recovery_skips_bare_block_scalar_marker() {
        // On the recovery path, a `description:` line that is
        // only a block-scalar marker (`|` / `>`) must not become the description —
        // otherwise it suppresses the body fallback and the listing shows "|".
        let skill = parse_one(
            "bs",
            concat!(
                "---\n",
                "name: bs\n",
                "description: |\n",
                "  Real block description.\n",
                "tags: a\n",
                "  nested: b\n", // indented under a scalar -> YAML fails -> recovery path
                "---\n",
                "\n",
                "Body paragraph wins.\n",
            ),
        );
        assert_eq!(skill.description, "Body paragraph wins.");
        assert!(!skill.has_user_specified_description);
    }

    #[test]
    fn recovery_ignores_non_scalar_keys_like_allowed_tools() {
        // Recovery is limited to known scalar keys, so a list
        // field like `allowed-tools` on the recovery path is never salvaged as a
        // mangled string (e.g. ["[Bash", "Edit]"]).
        let skill = parse_one(
            "at",
            concat!(
                "---\n",
                "name: at\n",
                "description: Real desc.\n",
                "allowed-tools: [Bash, Edit]\n",
                "  bad: indent\n", // forces YAML failure -> recovery path
                "---\n\nBody.\n",
            ),
        );
        assert_eq!(skill.description, "Real desc.");
        assert!(skill.allowed_tools.is_none()); // not mangled from the broken list
    }

    #[test]
    fn recovery_strips_inline_comment_from_unquoted_value() {
        // An unquoted value drops its inline `# comment`,
        // matching a real YAML parse.
        let skill = parse_one(
            "ic",
            concat!(
                "---\n",
                "name: ic\n",
                "description: Does X # internal note\n",
                "  Use when: y\n", // indented -> YAML fails -> recovery path
                "---\n\nBody.\n",
            ),
        );
        assert_eq!(skill.description, "Does X");
        assert!(skill.has_user_specified_description);
    }

    #[test]
    fn fallback_skips_leading_image_and_blockquote() {
        let body = "![CI](https://img/badge.svg)\n\n> Note: deprecated.\n\nFormats staged files.\n";
        let skill = parse_one("x", &format!("---\nname: x\n---\n\n{body}"));
        assert_eq!(skill.description, "Formats staged files.");
    }

    #[test]
    fn frontmatter_description_sets_user_specified_flag() {
        let skill = parse_one("d", "---\nname: d\ndescription: A real one\n---\n\nbody\n");
        assert_eq!(skill.description, "A real one");
        assert!(skill.has_user_specified_description);
    }

    #[test]
    fn paths_frontmatter_parsed_and_normalized() {
        let skill = parse_one(
            "g",
            "---\nname: g\ndescription: x\npaths: src/**, docs\n---\n",
        );
        assert_eq!(
            skill.paths,
            Some(vec!["src".to_string(), "docs".to_string()])
        );
    }

    #[test]
    fn paths_with_space_not_split_on_whitespace() {
        let skill = parse_one(
            "g",
            "---\nname: g\ndescription: x\npaths: \"my dir/**\"\n---\n",
        );
        assert_eq!(skill.paths, Some(vec!["my dir".to_string()]));
    }

    #[test]
    fn paths_split_preserves_brace_groups() {
        // Commas inside `{...}` are not split points; the brace pattern is kept
        // whole for the matcher to expand.
        let skill = parse_one(
            "g",
            "---\nname: g\ndescription: x\npaths: a/{b,c}/{d,e}, docs\n---\n",
        );
        assert_eq!(
            skill.paths,
            Some(vec!["a/{b,c}/{d,e}".to_string(), "docs".to_string()])
        );
    }

    #[test]
    fn edge_frontmatter_parses_field_by_field() {
        // A value colon (YAML syntax error) and non-string scalars survive via
        // the quoting retry plus per-field coercion.
        let skill = parse_one(
            "d",
            "---\nname: d\ndescription: Deploy: push to prod\nwhen-to-use: trig\nuser-invocable: yes\nallowed-tools: bash, grep\neffort: 5\n---\n",
        );
        assert_eq!(skill.description, "Deploy: push to prod");
        assert_eq!(skill.when_to_use.as_deref(), Some("trig"));
        assert!(!skill.user_invocable); // only literal `true` is true; `yes` → false
        assert_eq!(
            skill.allowed_tools,
            Some(vec!["bash".into(), "grep".into()])
        );
        assert_eq!(skill.effort.as_deref(), Some("5"));
    }

    #[test]
    fn bool_fields_only_literal_true_is_true() {
        // Only a YAML `true` / `"true"` is true; everything else is false.
        let explicit = parse_one(
            "d",
            "---\nname: d\ndescription: x\nuser-invocable: false\ndisable-model-invocation: true\n---\n",
        );
        assert!(!explicit.user_invocable);
        assert!(explicit.disable_model_invocation);

        // Non-`true` tokens (`yes`, numbers) are not truthy.
        let yes = parse_one(
            "e",
            "---\nname: e\ndescription: x\nuser-invocable: yes\ndisable-model-invocation: 1\n---\n",
        );
        assert!(!yes.user_invocable);
        assert!(!yes.disable_model_invocation);

        // Absent → field default (user-invocable true, disable false).
        let absent = parse_one("g", "---\nname: g\ndescription: x\n---\n");
        assert!(absent.user_invocable);
        assert!(!absent.disable_model_invocation);
    }

    #[test]
    fn allowed_tools_keep_specs_with_spaces_and_inner_commas() {
        let skill = parse_one(
            "d",
            "---\nname: d\ndescription: x\nallowed-tools: Read, Bash(git log --format=%h,%s)\n---\n",
        );
        assert_eq!(
            skill.allowed_tools,
            Some(vec!["Read".into(), "Bash(git log --format=%h,%s)".into()])
        );
    }

    #[test]
    fn non_scalar_description_falls_back_to_name() {
        // A YAML list description is non-scalar → rejected → name fallback.
        let skill = parse_one("d", "---\nname: d\ndescription:\n  - a\n  - b\n---\n");
        assert_eq!(skill.description, "d");
        assert!(!skill.has_user_specified_description);
    }

    #[test]
    fn normalize_skill_paths_strips_and_drops_match_all() {
        assert_eq!(
            normalize_skill_paths(vec!["src/**".into(), "docs".into()]),
            Some(vec!["src".into(), "docs".into()])
        );
        assert_eq!(normalize_skill_paths(vec!["**".into()]), None);
        assert_eq!(normalize_skill_paths(Vec::new()), None);
    }

    #[test]
    fn when_to_use_kebab_case() {
        let content = "\
---
name: deploy
description: Deploy to production
when-to-use: User says deploy, push to prod, ship it
---
";
        let parsed = parse_skill_frontmatter(content, None).unwrap();
        assert_eq!(
            parsed.when_to_use.as_deref(),
            Some("User says deploy, push to prod, ship it")
        );
        assert_eq!(parsed.description, "Deploy to production");
    }

    #[test]
    fn when_to_use_snake_case_alias() {
        let content = "\
---
name: deploy
description: Deploy to staging
when_to_use: User says deploy or ship it
---
";
        let parsed = parse_skill_frontmatter(content, None).unwrap();
        assert_eq!(
            parsed.when_to_use.as_deref(),
            Some("User says deploy or ship it")
        );
    }

    #[test]
    fn when_to_use_absent_is_none() {
        let content = "\
---
name: commit
description: Create a git commit
---
";
        let parsed = parse_skill_frontmatter(content, None).unwrap();
        assert!(parsed.when_to_use.is_none());
    }

    #[test]
    fn when_to_use_capped_at_max_len() {
        let long_value = "a".repeat(MAX_DESCRIPTION_LEN + 500);
        let content = format!(
            "---\nname: deploy\ndescription: Deploy\nwhen-to-use: {}\n---\n",
            long_value
        );
        let parsed = parse_skill_frontmatter(&content, None).unwrap();
        let wtu = parsed.when_to_use.unwrap();
        assert_eq!(wtu.len(), MAX_DESCRIPTION_LEN);
    }

    #[test]
    fn when_to_use_capped_multibyte() {
        // 'é' is 2 bytes in UTF-8; cap is by char count, not byte count
        let long_value = "é".repeat(MAX_DESCRIPTION_LEN + 100);
        let content = format!(
            "---\nname: deploy\ndescription: Deploy\nwhen-to-use: {}\n---\n",
            long_value
        );
        let parsed = parse_skill_frontmatter(&content, None).unwrap();
        let wtu = parsed.when_to_use.unwrap();
        assert_eq!(wtu.chars().count(), MAX_DESCRIPTION_LEN);
        assert!(wtu.len() > MAX_DESCRIPTION_LEN); // byte length exceeds char-count cap
    }

    #[test]
    fn when_to_use_empty_string_normalized_to_none() {
        let content = "\
---
name: deploy
description: Deploy to prod
when-to-use: \"\"
---
";
        let parsed = parse_skill_frontmatter(content, None).unwrap();
        assert!(parsed.when_to_use.is_none());
    }

    #[test]
    fn when_to_use_coexists_with_other_fields() {
        let content = "\
---
name: review
description: Code review tool
when-to-use: User says review my code
allowed-tools: grep, read
argument-hint: PR number
model: test-model
---
";
        let parsed = parse_skill_frontmatter(content, None).unwrap();
        assert_eq!(
            parsed.when_to_use.as_deref(),
            Some("User says review my code")
        );
        assert_eq!(parsed.argument_hint.as_deref(), Some("PR number"));
        assert_eq!(parsed.model.as_deref(), Some("test-model"));
        assert_eq!(
            parsed.allowed_tools.as_deref(),
            Some(["grep".to_string(), "read".to_string()].as_slice())
        );
    }

    #[test]
    fn when_to_use_propagates_through_parse_skill_files() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("skills").join("deploy");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: deploy\ndescription: Deploy to prod\nwhen-to-use: User says deploy or ship it\n---\nBody text",
        )
        .unwrap();

        let skills = parse_skill_files(vec![(skill_dir.join("SKILL.md"), SkillScope::Local)]);
        assert_eq!(skills.len(), 1);
        assert_eq!(
            skills[0].when_to_use.as_deref(),
            Some("User says deploy or ship it")
        );
    }

    #[test]
    fn when_to_use_none_for_no_frontmatter_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("skills").join("simple");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "# Simple Skill\n\nJust a body, no frontmatter.",
        )
        .unwrap();

        let skills = parse_skill_files(vec![(skill_dir.join("SKILL.md"), SkillScope::Repo)]);
        assert_eq!(skills.len(), 1);
        assert!(skills[0].when_to_use.is_none());
    }

    #[test]
    fn normalize_underscores_to_hyphens() {
        assert_eq!(
            normalize_skill_name("narrate_crash_video"),
            "narrate-crash-video"
        );
        assert_eq!(normalize_skill_name("my_skill_name"), "my-skill-name");
        assert_eq!(normalize_skill_name("no__double"), "no-double");
        assert_eq!(normalize_skill_name("_leading"), "leading");
        assert_eq!(normalize_skill_name("trailing_"), "trailing");
        // Dots and other non-slug chars become hyphens rather than dropping the skill.
        assert_eq!(normalize_skill_name("tool-v1.2"), "tool-v1-2");
    }

    #[test]
    fn invalid_frontmatter_name_falls_back_to_dir_name() {
        // A bad `name:` (normalizes to empty) must not drop a skill whose
        // directory name is a valid slug; the dir name is used, fields kept.
        let skill = parse_one("validdir", "---\nname: 日本語\ndescription: x\n---\n");
        assert_eq!(skill.name, "validdir");
        assert_eq!(skill.description, "x");
    }

    #[test]
    fn dotted_dir_name_is_kept_not_dropped() {
        // A directory name with a `.` normalizes to a valid slug and loads
        // (slash-invocable), instead of being rejected as an invalid name.
        let skill = parse_one("tool-v1.2", "no frontmatter, just body\n");
        assert_eq!(skill.name, "tool-v1-2");
    }

    #[test]
    fn underscore_name_in_frontmatter_normalizes_and_loads() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("skills").join("narrate-crash-video");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: narrate_crash_video\ndescription: Analyze crash video\n---\nBody",
        )
        .unwrap();

        let skills = parse_skill_files(vec![(skill_dir.join("SKILL.md"), SkillScope::User)]);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "narrate-crash-video");
    }

    // ── skills-cursor removal ──────────────────────────────

    #[test]
    fn skill_subdirs_no_longer_includes_skills_cursor() {
        assert_eq!(SKILL_SUBDIRS, &["skills"]);
        assert!(!SKILL_SUBDIRS.contains(&"skills-cursor"));
    }

    #[test]
    fn find_skill_paths_ignores_skills_cursor_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let cursor_dir = tmp.path().join(".cursor");
        // Cursor product layout: scanned no longer.
        let legacy = cursor_dir.join("skills-cursor").join("babysit");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("SKILL.md"), "---\nname: babysit\n---\n").unwrap();
        // Standard layout: still scanned.
        let standard = cursor_dir.join("skills").join("mine");
        std::fs::create_dir_all(&standard).unwrap();
        std::fs::write(standard.join("SKILL.md"), "---\nname: mine\n---\n").unwrap();

        let paths = find_skill_paths(&cursor_dir);
        let strs: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        assert!(
            strs.iter().any(|p| p.contains("skills/mine")),
            "standard skills/ layout must still be found: {strs:?}"
        );
        assert!(
            !strs.iter().any(|p| p.contains("skills-cursor")),
            "skills-cursor layout must no longer be scanned: {strs:?}"
        );
    }

    // ── vendor-default skill denylist ──────────────────────

    #[test]
    fn is_vendor_default_skill_matches_builtins_under_vendor_dir() {
        assert!(is_vendor_default_skill(
            "/home/u/.cursor/skills/shell/SKILL.md",
            "shell"
        ));
        assert!(is_vendor_default_skill(
            "/home/u/.cursor/skills/create-rule/SKILL.md",
            "create-rule"
        ));
    }

    #[test]
    fn is_vendor_default_skill_matches_claude_builtins_under_claude_dir() {
        assert!(is_vendor_default_skill(
            "/home/u/.claude/skills/pdf/SKILL.md",
            "pdf"
        ));
    }

    #[test]
    fn is_vendor_default_skill_spares_user_skill_outside_vendor_dir() {
        // A user's own "shell" skill in ~/.echo-agent is NOT a vendor builtin.
        assert!(!is_vendor_default_skill(
            "/home/u/.echo-agent/skills/shell/SKILL.md",
            "shell"
        ));
    }

    #[test]
    fn is_vendor_default_skill_spares_non_denylisted_name_under_vendor_dir() {
        assert!(!is_vendor_default_skill(
            "/home/u/.cursor/skills/my-cursor-skill/SKILL.md",
            "my-cursor-skill"
        ));
    }

    #[test]
    fn is_vendor_default_skill_does_not_cross_vendors() {
        // "shell" is a cursor-vendor builtin, not a claude-vendor one — under .claude it stays.
        assert!(!is_vendor_default_skill(
            "/home/u/.claude/skills/shell/SKILL.md",
            "shell"
        ));
    }

    #[test]
    fn parse_skill_files_drops_denylisted_cursor_skill() {
        let tmp = tempfile::tempdir().unwrap();
        // Denylisted name under a /.cursor/ path → dropped.
        let cursor_shell = tmp.path().join(".cursor").join("skills").join("shell");
        std::fs::create_dir_all(&cursor_shell).unwrap();
        std::fs::write(
            cursor_shell.join("SKILL.md"),
            "---\nname: shell\ndescription: cursor builtin\n---\n",
        )
        .unwrap();
        // Same name under /.echo-agent/ → kept (user content).
        let echo_agent_shell = tmp.path().join(".echo-agent").join("skills").join("shell");
        std::fs::create_dir_all(&echo_agent_shell).unwrap();
        std::fs::write(
            echo_agent_shell.join("SKILL.md"),
            "---\nname: shell\ndescription: user content\n---\n",
        )
        .unwrap();

        let skills = parse_skill_files(vec![
            (cursor_shell.join("SKILL.md"), SkillScope::User),
            (echo_agent_shell.join("SKILL.md"), SkillScope::User),
        ]);
        assert_eq!(skills.len(), 1, "cursor builtin must be dropped");
        assert!(skills[0].path.contains("/.echo-agent/"));
    }

    #[test]
    fn parse_skill_files_keeps_non_denylisted_cursor_skill() {
        let tmp = tempfile::tempdir().unwrap();
        let cursor_skill = tmp
            .path()
            .join(".cursor")
            .join("skills")
            .join("my-cursor-skill");
        std::fs::create_dir_all(&cursor_skill).unwrap();
        std::fs::write(
            cursor_skill.join("SKILL.md"),
            "---\nname: my-cursor-skill\ndescription: user content\n---\n",
        )
        .unwrap();

        let skills = parse_skill_files(vec![(cursor_skill.join("SKILL.md"), SkillScope::User)]);
        assert_eq!(skills.len(), 1, "user cursor skill must be kept");
    }

    // ── discover_skills_for_paths vendor gating ────────────

    #[test]
    fn discover_skills_for_paths_gates_claude_dir() {
        use crate::types::compat::CompatConfig;

        let tmp = tempfile::tempdir().unwrap();
        // `discover_skills_for_paths` takes `git_root` explicitly and only uses
        // it as a path boundary, so no real git repo is needed here.
        let repo = dunce::canonicalize(tmp.path()).unwrap();
        let sub = repo.join("sub");
        std::fs::create_dir_all(&sub).unwrap();

        // A .claude skill and a .echo-agent skill in an intermediate dir.
        let claude_skill = sub.join(".claude").join("skills").join("claude-dyn");
        std::fs::create_dir_all(&claude_skill).unwrap();
        std::fs::write(
            claude_skill.join("SKILL.md"),
            "---\nname: claude-dyn\n---\n",
        )
        .unwrap();
        let echo_agent_skill = sub
            .join(".echo-agent")
            .join("skills")
            .join("echo-agent-dyn");
        std::fs::create_dir_all(&echo_agent_skill).unwrap();
        std::fs::write(
            echo_agent_skill.join("SKILL.md"),
            "---\nname: echo-agent-dyn\n---\n",
        )
        .unwrap();

        let file = sub.join("file.rs");
        std::fs::write(&file, "fn main() {}").unwrap();

        // claude.skills ON → both discovered.
        let mut checked = HashSet::new();
        let on = discover_skills_for_paths(
            &[file.as_path()],
            &repo,
            Some(repo.as_path()),
            &mut checked,
            CompatConfig::default(),
        );
        let names_on: Vec<&str> = on.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names_on.contains(&"echo-agent-dyn"),
            "echo-agent-dyn missing: {names_on:?}"
        );
        assert!(
            names_on.contains(&"claude-dyn"),
            "claude-dyn should be found when claude.skills on: {names_on:?}"
        );

        // claude.skills OFF → only echo-agent-dyn discovered.
        let mut compat_off = CompatConfig::default();
        compat_off.claude.skills = false;
        let mut checked2 = HashSet::new();
        let off = discover_skills_for_paths(
            &[file.as_path()],
            &repo,
            Some(repo.as_path()),
            &mut checked2,
            compat_off,
        );
        let names_off: Vec<&str> = off.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names_off.contains(&"echo-agent-dyn"),
            "echo-agent-dyn missing: {names_off:?}"
        );
        assert!(
            !names_off.contains(&"claude-dyn"),
            "claude-dyn must be gated off: {names_off:?}"
        );
    }

    #[test]
    fn walk_for_skill_md_visits_dirs_in_lexicographic_order() {
        let tmp = tempfile::tempdir().unwrap();
        // Created out of order; readdir order is fs-dependent.
        for dir in ["zeta", "alpha", "mid"] {
            let d = tmp.path().join(dir);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("SKILL.md"), "---\nname: x\n---\n").unwrap();
        }
        let nested = tmp.path().join("alpha").join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("SKILL.md"), "---\nname: y\n---\n").unwrap();

        let paths = find_skill_md_paths(tmp.path());
        let rel: Vec<String> = paths
            .iter()
            .map(|p| {
                p.strip_prefix(tmp.path())
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        assert_eq!(
            rel,
            [
                "alpha/SKILL.md",
                "alpha/nested/SKILL.md",
                "mid/SKILL.md",
                "zeta/SKILL.md"
            ]
        );
    }
}
