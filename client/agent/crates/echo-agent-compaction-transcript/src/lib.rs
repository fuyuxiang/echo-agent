//! Pure rendering of a compacted segment into self-contained markdown, aligned
//! with the Python compaction implementation (`render_segment_to_markdown` /
//! `compute_turn_stats`; INDEX built incrementally via [`INDEX_HEADER`] +
//! [`render_index_row`]). No I/O.
//! Not byte-identical — the data models differ (Python `Turn`/channels vs our
//! [`ConversationItem`]) — but headers, sections, detail levels, and INDEX
//! columns match.

use std::sync::OnceLock;

use echo_agent_sampling_types::ConversationItem;
use regex::Regex;

/// Layout of the per-session segment store — single source of the path
/// convention (writer, index parser, and transcript-hint builder all use these).
pub const COMPACTION_DIR: &str = "compaction";
pub const INDEX_FILE: &str = "INDEX.md";
const SEGMENT_PREFIX: &str = "segment_";

/// Whole-turn-boundary truncation cap for one segment's verbatim section.
const SEGMENT_MAX_BYTES: usize = 512 * 1024;
const TRUNCATION_NOTICE: &str =
    "\n\n[... TRUNCATED at {limit} bytes, {omitted} turns omitted ...]\n";
/// Per-turn text/arg caps for the `balanced` detail level (chars, like the Python implementation).
const BALANCED_TEXT_CHARS: usize = 2000;
const BALANCED_RESPONSE_CHARS: usize = 500;
/// Trailing chars of the last assistant message kept for the stats excerpt.
const LAST_RESPONSE_EXCERPT_CHARS: usize = 500;
/// Approx markdown overhead charged per turn in the verbose-size estimate.
const PER_TURN_OVERHEAD_BYTES: usize = 64;

/// How much per-turn detail lands in the verbatim section. Mirrors the Python
/// `compaction_persist_detail`. `Verbose` is the default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, strum::Display)]
#[strum(serialize_all = "snake_case")]
pub enum CompactionDetail {
    /// Stats + summary only, no verbatim turns.
    None,
    /// One-line tool-call signature per turn.
    Minimal,
    /// Tool calls + truncated responses + full text.
    Balanced,
    /// Full verbatim turns.
    #[default]
    Verbose,
}

impl CompactionDetail {
    /// Case-insensitive; unknown → `None` so the caller falls back to default.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" => Some(Self::None),
            "minimal" => Some(Self::Minimal),
            "balanced" => Some(Self::Balanced),
            "verbose" => Some(Self::Verbose),
            _ => None,
        }
    }
}

/// Role label per item, mapped onto the Python `Turn` role vocabulary
/// (`System`/`Human`/`Assistant`/`Function`). Model-side items with no Python
/// analog (`BackendToolCall`, `Reasoning`) fold into `Assistant`.
fn role_label(item: &ConversationItem) -> &'static str {
    match item {
        ConversationItem::System(_) => "System",
        ConversationItem::User(_) => "Human",
        ConversationItem::Assistant(_) => "Assistant",
        ConversationItem::ToolResult(_) => "Function",
        ConversationItem::BackendToolCall(_) => "Assistant",
        ConversationItem::Reasoning(_) => "Assistant",
    }
}

/// INDEX.md title + table header, written once when the file is created.
pub const INDEX_HEADER: &str = "# Compaction Segment Index\n\n\
     | Segment | File | Turns | Approx bytes | Keywords |\n\
     |---|---|---|---|---|\n";

/// Zero-padded segment number, e.g. `007`. The single source of the pad width.
fn segment_label(index: u64) -> String {
    format!("{index:03}")
}

/// Flat per-segment filename, e.g. `segment_007.md` (matches the Python implementation).
pub fn segment_filename(index: u64) -> String {
    format!("{SEGMENT_PREFIX}{}.md", segment_label(index))
}

/// Parse a segment index out of a `segment_NNN.md` filename, if it matches.
pub fn parse_segment_index(filename: &str) -> Option<u64> {
    filename
        .strip_prefix(SEGMENT_PREFIX)?
        .strip_suffix(".md")?
        .parse()
        .ok()
}

/// A read of the `compaction/` store; `Display` (snake_case) is the telemetry
/// label on `compaction.segment_read`.
#[derive(Debug, PartialEq, Eq, strum::Display)]
#[strum(serialize_all = "snake_case")]
pub enum CompactionArtifact {
    Segment(u64),
    Index,
    Dir,
}

impl CompactionArtifact {
    pub fn segment_index(&self) -> Option<u64> {
        match self {
            Self::Segment(index) => Some(*index),
            _ => None,
        }
    }
}

/// Anchors on the `compaction/` component, not the session dir, so relative
/// reads still match (a same-named file elsewhere is acceptable noise).
pub fn classify_compaction_path(path: &str) -> Option<CompactionArtifact> {
    // Allocation-free: match the `compaction` component directly rather than
    // building `"compaction/"` / `"/compaction"` patterns each call.
    let trimmed = path.trim_end_matches('/');
    if trimmed == COMPACTION_DIR
        || trimmed
            .strip_suffix(COMPACTION_DIR)
            .is_some_and(|prefix| prefix.ends_with('/'))
    {
        return Some(CompactionArtifact::Dir);
    }
    let rest = path
        .rsplit_once(COMPACTION_DIR)
        .and_then(|(_, after)| after.strip_prefix('/'))?;
    if let Some(index) = parse_segment_index(rest) {
        Some(CompactionArtifact::Segment(index))
    } else if rest == INDEX_FILE {
        Some(CompactionArtifact::Index)
    } else {
        None
    }
}

/// Truncate to ≤ `max` chars, appending `marker` if cut (char-based, like
/// the Python `text[:n]`). Char boundaries are respected so we never panic.
fn truncate_chars(s: &str, max: usize, marker: &str) -> String {
    match s.char_indices().nth(max) {
        Some((byte_idx, _)) => format!("{}{marker}", &s[..byte_idx]),
        None => s.to_string(),
    }
}

/// Insert thousands separators (mirrors Python's `{:,}`).
fn with_thousands(n: usize) -> String {
    let digits = n.to_string();
    let bytes = digits.as_bytes();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

/// One JSON tool-arg value rendered for a `- key: value` line.
fn arg_value_plain(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn tool_args(arguments: &str) -> serde_json::Map<String, serde_json::Value> {
    match serde_json::from_str::<serde_json::Value>(arguments) {
        Ok(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    }
}

/// Keys checked (in order) to attribute a tool call to a target file/dir.
const FILE_ARG_KEYS: [&str; 4] = ["target_file", "file_path", "path", "target_directory"];

/// Walk-once statistics for the always-on `## Turn statistics` block.
struct TurnStats {
    turn_count: usize,
    /// Role → count, kept sorted by role name.
    role_counts: Vec<(&'static str, usize)>,
    /// Tool name → count.
    tool_counts: Vec<(String, usize)>,
    unique_files: Vec<String>,
    tool_error_count: usize,
    verbose_byte_estimate: usize,
    last_assistant_excerpt: String,
}

fn compute_turn_stats(items: &[ConversationItem]) -> TurnStats {
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;

    let mut role_counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut tool_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut unique_files: BTreeSet<String> = BTreeSet::new();
    let mut tool_error_count = 0;
    let mut last_assistant = String::new();
    let mut verbose_byte_estimate = 0;

    for item in items {
        *role_counts.entry(role_label(item)).or_insert(0) += 1;
        verbose_byte_estimate += PER_TURN_OVERHEAD_BYTES;

        match item {
            ConversationItem::Assistant(a) => {
                verbose_byte_estimate += a.content.len();
                if !a.content.is_empty() {
                    last_assistant = a.content.to_string();
                }
                for tc in &a.tool_calls {
                    *tool_counts.entry(tc.name.clone()).or_insert(0) += 1;
                    let args = tool_args(&tc.arguments);
                    for key in FILE_ARG_KEYS {
                        if let Some(serde_json::Value::String(v)) = args.get(key)
                            && !v.is_empty()
                        {
                            unique_files.insert(v.clone());
                            break;
                        }
                    }
                    verbose_byte_estimate += args
                        .iter()
                        .map(|(k, v)| 32 + k.len() + arg_value_plain(v).len())
                        .sum::<usize>();
                }
            }
            ConversationItem::ToolResult(t) => {
                verbose_byte_estimate += t.content.len();
                if t.content.starts_with("Error") || t.content.contains("Failed tool validation") {
                    tool_error_count += 1;
                }
            }
            other => verbose_byte_estimate += other.text_content().len(),
        }
    }

    let excerpt = {
        let n = last_assistant.chars().count();
        let tail: String = last_assistant
            .chars()
            .skip(n.saturating_sub(LAST_RESPONSE_EXCERPT_CHARS))
            .collect();
        tail.trim().to_string()
    };

    let mut tool_counts: Vec<(String, usize)> = tool_counts.into_iter().collect();
    // Descending count for at-a-glance scanning; name breaks ties.
    tool_counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    TurnStats {
        turn_count: items.len(),
        role_counts: role_counts.into_iter().collect(),
        tool_counts,
        unique_files: unique_files.into_iter().collect(),
        tool_error_count,
        verbose_byte_estimate,
        last_assistant_excerpt: excerpt,
    }
}

fn render_stats_block(stats: &TurnStats) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("## Turn statistics\n\n");

    let rc = stats
        .role_counts
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(", ");
    let _ = writeln!(out, "- Turns: {} ({rc})", stats.turn_count);

    let tc = if stats.tool_counts.is_empty() {
        "(none)".to_string()
    } else {
        stats
            .tool_counts
            .iter()
            .map(|(name, n)| format!("{name} ({n})"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let _ = writeln!(out, "- Tools used: {tc}");

    let uf = &stats.unique_files;
    let uf_str = if uf.is_empty() {
        "(none)".to_string()
    } else if uf.len() <= 8 {
        uf.join(", ")
    } else {
        format!("{}, ... and {} more", uf[..5].join(", "), uf.len() - 5)
    };
    let _ = writeln!(out, "- Unique target files ({}): {uf_str}", uf.len());
    let _ = writeln!(out, "- Tool errors: {}", stats.tool_error_count);
    let _ = writeln!(
        out,
        "- Verbose-render size estimate: {} B",
        with_thousands(stats.verbose_byte_estimate)
    );
    if !stats.last_assistant_excerpt.is_empty() {
        let oneline = truncate_chars(&stats.last_assistant_excerpt.replace('\n', " "), 300, "");
        let _ = writeln!(out, "- Last assistant response excerpt: \"{oneline}\"");
    }
    out.push('\n');
    out
}

/// One verbatim turn: role header, text, and `[tool_request: …]` arg lines.
fn render_turn_verbose(item: &ConversationItem, index: usize) -> String {
    let mut parts = vec![format!("### Turn {index} ({})", role_label(item))];
    match item {
        ConversationItem::Assistant(a) => {
            if !a.content.is_empty() {
                parts.push(a.content.to_string());
            }
            for tc in &a.tool_calls {
                parts.push(format!("[tool_request: {}]", tc.name));
                for (k, v) in tool_args(&tc.arguments) {
                    parts.push(format!("- {k}: {}", arg_value_plain(&v)));
                }
            }
        }
        ConversationItem::ToolResult(t) => {
            parts.push("[tool_response]".to_string());
            if !t.content.is_empty() {
                parts.push(t.content.to_string());
            }
        }
        other => {
            let txt = other.text_content();
            if !txt.is_empty() {
                parts.push(txt);
            }
        }
    }
    parts.join("\n") + "\n"
}

/// One balanced turn: full text (capped) + truncated tool-call args/responses.
fn render_turn_balanced(item: &ConversationItem, index: usize) -> String {
    let mut parts = vec![format!("### Turn {index} ({})", role_label(item))];
    match item {
        ConversationItem::Assistant(a) => {
            if !a.content.is_empty() {
                parts.push(truncate_chars(
                    &a.content,
                    BALANCED_TEXT_CHARS,
                    "... [truncated]",
                ));
            }
            for tc in &a.tool_calls {
                parts.push(format!("[tool_request: {}]", tc.name));
                for (k, v) in tool_args(&tc.arguments) {
                    let v = truncate_chars(
                        &arg_value_plain(&v),
                        BALANCED_RESPONSE_CHARS,
                        "... [truncated]",
                    );
                    parts.push(format!("- {k}: {v}"));
                }
            }
        }
        ConversationItem::ToolResult(t) => {
            parts.push("[tool_response]".to_string());
            if !t.content.is_empty() {
                parts.push(truncate_chars(
                    &t.content,
                    BALANCED_RESPONSE_CHARS,
                    "... [truncated]",
                ));
            }
        }
        other => {
            let txt = other.text_content();
            if !txt.is_empty() {
                parts.push(txt);
            }
        }
    }
    parts.join("\n") + "\n"
}

/// One-line tool-call signature per turn, no response bodies.
fn render_turn_signature(item: &ConversationItem, index: usize) -> String {
    let role = role_label(item);
    match item {
        ConversationItem::Assistant(a) => {
            let sigs: Vec<String> = a
                .tool_calls
                .iter()
                .map(|tc| {
                    let args = tool_args(&tc.arguments);
                    let key_arg = [
                        "target_file",
                        "file_path",
                        "path",
                        "target_directory",
                        "command",
                        "pattern",
                    ]
                    .iter()
                    .find_map(|k| match args.get(*k) {
                        Some(serde_json::Value::String(v)) if !v.is_empty() => {
                            Some(format!("{k}={:?}", truncate_chars(v, 80, "...")))
                        }
                        _ => None,
                    })
                    .unwrap_or_default();
                    format!("{}({key_arg})", tc.name)
                })
                .collect();
            let sig_str = if sigs.is_empty() {
                "(text only)".to_string()
            } else {
                sigs.join("  ")
            };
            format!("### Turn {index} ({role})  {sig_str}\n")
        }
        ConversationItem::ToolResult(_) => format!("### Turn {index} ({role})  [tool_response]\n"),
        _ => format!("### Turn {index} ({role})\n"),
    }
}

/// Render one segment: header, metadata, stats, curated summary, and (unless
/// `detail == None`) verbatim turns truncated at a whole-turn boundary before
/// [`SEGMENT_MAX_BYTES`]. `summary` must already be cleaned of analysis tags;
/// `items` is the segment view — tool calls + results kept, images/reasoning
/// stripped (see `echo_agent_chat_state::compaction_utils::prepare_conversation_for_segment`).
pub fn render_segment_md(
    items: &[ConversationItem],
    summary: &str,
    index: u64,
    detail: CompactionDetail,
    timestamp: &str,
) -> String {
    let header = format!(
        "# HISTORICAL -- DO NOT EDIT\n\
         # Record of compaction segment {label} (detail={detail}) from this same task.\n\
         # Use read_file or grep to look up details, but do not modify.\n\n",
        label = segment_label(index),
    );
    let metadata = format!(
        "## Segment metadata\n- Index: {label}\n- Turn count: {count}\n- Timestamp: {timestamp}\n\n",
        label = segment_label(index),
        count = items.len(),
    );
    let stats_section = render_stats_block(&compute_turn_stats(items)) + "\n";
    let summary_body = summary.trim();
    let summary_section = format!(
        "## Summary (curated by compaction step)\n\n{}\n\n",
        if summary_body.is_empty() {
            "(empty)"
        } else {
            summary_body
        },
    );

    let preamble_head = format!("{header}{metadata}{stats_section}{summary_section}");
    if detail == CompactionDetail::None {
        return preamble_head;
    }

    let (turns_header, render_turn): (&str, fn(&ConversationItem, usize) -> String) = match detail {
        CompactionDetail::Minimal => ("## Turn signatures\n\n", render_turn_signature),
        CompactionDetail::Balanced => ("## Turns (balanced detail)\n\n", render_turn_balanced),
        CompactionDetail::Verbose => ("## Verbatim turns\n\n", render_turn_verbose),
        CompactionDetail::None => unreachable!("None returns above"),
    };

    let preamble = format!("{preamble_head}{turns_header}");
    // Reserve the preamble, the notice, and slack for its `{limit}`/`{omitted}`
    // substitutions so the rendered doc stays under the cap.
    let budget = SEGMENT_MAX_BYTES
        .saturating_sub(preamble.len() + TRUNCATION_NOTICE.len() + PER_TURN_OVERHEAD_BYTES);

    let mut blocks: Vec<String> = Vec::new();
    let mut used = 0;
    let mut truncated_at: Option<usize> = None;
    for (i, item) in items.iter().enumerate() {
        let block = render_turn(item, i);
        if used + block.len() > budget {
            truncated_at = Some(i);
            break;
        }
        used += block.len();
        blocks.push(block);
    }

    let mut body = blocks.join("\n");
    if let Some(at) = truncated_at {
        let omitted = items.len() - at;
        body.push_str(
            &TRUNCATION_NOTICE
                .replace("{limit}", &SEGMENT_MAX_BYTES.to_string())
                .replace("{omitted}", &omitted.to_string()),
        );
    }
    format!("{preamble}{body}")
}

/// One INDEX.md row (with trailing newline). `keywords` are quoted and
/// comma-joined; the columns match [`INDEX_HEADER`].
pub fn render_index_row(
    index: u64,
    turn_count: usize,
    approx_bytes: usize,
    keywords: &[String],
) -> String {
    let kw = keywords
        .iter()
        .map(|k| format!("\"{k}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "| {label} | {file} | {turn_count} | {approx_bytes} | {kw} |\n",
        label = segment_label(index),
        file = segment_filename(index),
    )
}

static SECTION8_START_RE: OnceLock<Regex> = OnceLock::new();
static SECTION_HEADER_RE: OnceLock<Regex> = OnceLock::new();
static KEYWORD_RE: OnceLock<Regex> = OnceLock::new();

/// Stopwords dropped from INDEX keywords (mirrors the Python implementation).
const KEYWORD_STOPWORDS: [&str; 28] = [
    "section",
    "summary",
    "current",
    "work",
    "errors",
    "analysis",
    "primary",
    "request",
    "intent",
    "technical",
    "concepts",
    "pending",
    "problem",
    "solving",
    "include",
    "outline",
    "describe",
    "specific",
    "messages",
    "feedback",
    "snippet",
    "snippets",
    "session",
    "explicit",
    "thorough",
    "language",
    "important",
    "convention",
];

/// Best-effort INDEX keywords: identifier-shaped tokens from the summary's
/// "8. Current Work" section (falling back to the whole summary), minus
/// stopwords, deduped, capped at 8. Heuristic only — feeds the INDEX table.
pub fn extract_keywords(summary: &str) -> Vec<String> {
    // Rust's regex has no look-ahead, so scope section 8 with two anchored
    // matches: its header, then the next `N. Capital` header (or end of text).
    // `#{0,6}` tolerates our `## 8. Current Work` markdown headers as well as
    // the Python implementation's bare `8. Current Work`.
    let start_re =
        SECTION8_START_RE.get_or_init(|| Regex::new(r"(?m)^#{0,6}\s*8\.\s+Current Work").unwrap());
    let header_re =
        SECTION_HEADER_RE.get_or_init(|| Regex::new(r"(?m)^#{0,6}\s*\d+\.\s+[A-Z]").unwrap());
    let kw_re =
        KEYWORD_RE.get_or_init(|| Regex::new(r"[A-Z][A-Za-z0-9_]{3,}|[a-z][a-z0-9_]{5,}").unwrap());

    let text = match start_re.find(summary) {
        Some(m) => {
            let end = header_re
                .find_at(summary, m.end())
                .map(|h| h.start())
                .unwrap_or(summary.len());
            &summary[m.start()..end]
        }
        None => summary,
    };

    let mut seen: Vec<String> = Vec::new();
    for m in kw_re.find_iter(text) {
        let kw = m.as_str();
        if KEYWORD_STOPWORDS.contains(&kw.to_ascii_lowercase().as_str()) {
            continue;
        }
        if seen.iter().any(|s| s == kw) {
            continue;
        }
        seen.push(kw.to_string());
        if seen.len() >= 8 {
            break;
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> ConversationItem {
        ConversationItem::user(text)
    }

    /// The segment doc carries the Python implementation's skeleton: banner, metadata, stats,
    /// curated summary, and a detail-specific verbatim section.
    #[test]
    fn segment_md_matches_skeleton() {
        let md = render_segment_md(
            &[user("hello world")],
            "Summary: did things.",
            7,
            CompactionDetail::Verbose,
            "2026-01-01T00:00:00Z",
        );
        assert!(md.starts_with("# HISTORICAL -- DO NOT EDIT\n"));
        assert!(
            md.contains("# Record of compaction segment 007 (detail=verbose) from this same task.")
        );
        assert!(md.contains("## Segment metadata\n- Index: 007\n- Turn count: 1\n"));
        assert!(md.contains("## Turn statistics"));
        assert!(md.contains("## Summary (curated by compaction step)\n\nSummary: did things."));
    }

    /// Detail level selects the turns section (and `none` omits it entirely).
    #[test]
    fn detail_levels_select_turns_section() {
        let one = [user("hi")];
        let none = render_segment_md(&one, "s", 0, CompactionDetail::None, "t");
        assert!(!none.contains("## Verbatim turns") && !none.contains("## Turn signatures"));
        assert!(
            render_segment_md(&one, "s", 0, CompactionDetail::Minimal, "t")
                .contains("## Turn signatures")
        );
        assert!(
            render_segment_md(&one, "s", 0, CompactionDetail::Balanced, "t")
                .contains("## Turns (balanced detail)")
        );
        assert!(
            render_segment_md(&one, "s", 0, CompactionDetail::Verbose, "t")
                .contains("## Verbatim turns")
        );
        // Stats + summary survive at every level.
        for d in [
            CompactionDetail::None,
            CompactionDetail::Minimal,
            CompactionDetail::Balanced,
            CompactionDetail::Verbose,
        ] {
            assert!(render_segment_md(&one, "s", 0, d, "t").contains("## Turn statistics"));
        }
    }

    /// Verbatim turns are dropped at a whole-turn boundary once the byte budget
    /// is exceeded, with a notice naming how many turns were omitted.
    #[test]
    fn verbatim_turns_truncate_at_turn_boundary() {
        // Each turn renders ~200 KB, so the 3rd turn blows the 512 KB budget.
        let big = "x".repeat(200 * 1024);
        let items = [user(&big), user(&big), user(&big), user(&big)];
        let md = render_segment_md(&items, "s", 0, CompactionDetail::Verbose, "t");
        assert!(md.contains("### Turn 0 (Human)"));
        assert!(md.contains(&format!("TRUNCATED at {SEGMENT_MAX_BYTES} bytes")));
        assert!(md.contains("turns omitted"));
        // A whole turn was dropped (4 items, not all rendered).
        assert!(md.matches("### Turn ").count() < items.len());
    }

    /// INDEX header + row match the 5-column Python table; keywords are quoted.
    #[test]
    fn index_row_matches_columns() {
        assert!(INDEX_HEADER.starts_with(
            "# Compaction Segment Index\n\n| Segment | File | Turns | Approx bytes | Keywords |"
        ));
        let row = render_index_row(2, 9, 1234, &["Foo".to_string(), "bar_baz".to_string()]);
        assert_eq!(
            row,
            "| 002 | segment_002.md | 9 | 1234 | \"Foo\", \"bar_baz\" |\n"
        );
        assert_eq!(row.matches('\n').count(), 1);
    }

    /// Filename ⇄ index round-trips through the flat `segment_NNN.md` name.
    #[test]
    fn segment_filename_round_trips() {
        assert_eq!(segment_filename(5), "segment_005.md");
        assert_eq!(parse_segment_index("segment_005.md"), Some(5));
        assert_eq!(parse_segment_index("segment_005"), None);
        assert_eq!(parse_segment_index("notes.md"), None);
    }

    /// Store artifacts map to their kind (relative reads included); non-artifacts
    /// — even other files under `compaction/` — don't.
    #[test]
    fn classify_compaction_path_maps_store_artifacts() {
        use CompactionArtifact::*;
        assert_eq!(
            classify_compaction_path("/u/abc/compaction/segment_007.md"),
            Some(Segment(7))
        );
        // Relative read still matches (the substring-anchor behavior).
        assert_eq!(
            classify_compaction_path("compaction/segment_012.md"),
            Some(Segment(12))
        );
        assert_eq!(
            classify_compaction_path("/u/abc/compaction/INDEX.md"),
            Some(Index)
        );
        assert_eq!(classify_compaction_path("/u/abc/compaction"), Some(Dir));
        // Not store artifacts — including other files under `compaction/`.
        assert_eq!(classify_compaction_path("/repo/src/main.rs"), None);
        assert_eq!(classify_compaction_path("compaction/notes.md"), None);
    }

    // --- Parity with the Python implementation's own test vectors (compaction_utils_test.py) ---

    /// Keyword extraction: the Python `TestExtractKeywords` vectors (bare `8.`
    /// headers, stopword filtering, dedup, no-section-8 fallback) plus our
    /// `## 8.` markdown-header tolerance and out-of-section exclusion.
    #[test]
    fn extract_keywords_matches_python_vectors_and_markdown_headers() {
        let kw = extract_keywords(
            "1. Primary Request: ...\n8. Current Work: Just refactored AuthMiddleware in \
             handler.py and updated RedisCache integration.\n9. Next Step: ...\n",
        );
        assert!(kw.iter().any(|k| k == "AuthMiddleware") && kw.iter().any(|k| k == "RedisCache"));
        // No section 8 ⇒ fall back to the whole summary.
        let kw = extract_keywords("Worked on PostgresAdapter and JwtRefresh.");
        assert!(kw.iter().any(|k| k == "PostgresAdapter") && kw.iter().any(|k| k == "JwtRefresh"));
        // All-stopword section ⇒ empty; duplicates collapse to one.
        assert!(
            extract_keywords("8. Current Work: section summary technical concepts.\n").is_empty()
        );
        let kw = extract_keywords("8. Current Work: SameName SameName Other.\n");
        assert_eq!(kw.iter().filter(|k| *k == "SameName").count(), 1);
        // Our `## N.` markdown headers: scope to section 8, exclude outside words.
        let kw = extract_keywords(
            "## 1. Intro\nGenericWord\n\n## 8. Current Work\nEditing CompactionMode here.\n\n\
             ## 9. Next\nUnrelatedThing",
        );
        assert!(kw.iter().any(|k| k == "CompactionMode"));
        assert!(
            !kw.iter()
                .any(|k| k == "GenericWord" || k == "UnrelatedThing")
        );
    }

    /// Mirrors `TestComputeTurnStats::test_basic_counts` — same turns, same
    /// role/tool/file/error stats (roles mapped User→Human, ToolResult→Function).
    #[test]
    fn parity_turn_stats_matches_basic_counts() {
        use echo_agent_sampling_types::{AssistantItem, ToolCall};
        let tc = |name: &str, args: &str| ToolCall {
            id: "t".into(),
            name: name.to_string(),
            arguments: args.into(),
        };
        let items = vec![
            user("Fix the bug"),
            ConversationItem::Assistant(AssistantItem {
                content: "Done".into(),
                tool_calls: vec![
                    tc("read_file", r#"{"target_file":"src/a.py"}"#),
                    tc("read_file", r#"{"target_file":"src/b.py"}"#),
                    tc("grep", r#"{"pattern":"x","path":"src/"}"#),
                ],
                model_id: None,
                model_fingerprint: None,
                reasoning_effort: None,
            }),
            ConversationItem::tool_result("c", "file contents"),
        ];
        let s = compute_turn_stats(&items);
        assert_eq!(s.turn_count, 3);
        assert_eq!(
            s.role_counts,
            vec![("Assistant", 1), ("Function", 1), ("Human", 1)]
        );
        // Descending count, name tie-break.
        assert_eq!(
            s.tool_counts,
            vec![("read_file".to_string(), 2), ("grep".to_string(), 1)]
        );
        assert_eq!(s.unique_files, vec!["src/", "src/a.py", "src/b.py"]);
        assert_eq!(s.tool_error_count, 0);
    }

    /// Mirrors `test_error_counting` + `test_last_assistant_excerpt`.
    #[test]
    fn parity_turn_stats_errors_and_excerpt() {
        let errs = vec![
            ConversationItem::tool_result("a", "Error: not found"),
            ConversationItem::tool_result("c", "Failed tool validation: foo"),
            ConversationItem::tool_result("e", "success"),
        ];
        assert_eq!(compute_turn_stats(&errs).tool_error_count, 2);

        let conv = vec![
            ConversationItem::assistant("early"),
            user("middle"),
            ConversationItem::assistant("the final answer is 42"),
        ];
        assert!(
            compute_turn_stats(&conv)
                .last_assistant_excerpt
                .contains("the final answer is 42")
        );
    }
}
