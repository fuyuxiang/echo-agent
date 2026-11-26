//! Markdown-aware semantic chunking.
//!
//! Splits markdown content into chunks suitable for embedding and search.
//! Chunks respect markdown structure (headers, paragraphs, code blocks)
//! and include ancestor headers for self-containment.
//!
//! Token budgets include heading context. Oversized single lines are split at UTF-8 boundaries.

use echo_agent_config_types::MemoryIndexConfig;

/// A chunk of text extracted from a memory file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// The chunk text, including ancestor header context.
    pub text: String,
    /// 0-based start line in the source file.
    pub start_line: usize,
    /// 0-based end line (exclusive) in the source file.
    pub end_line: usize,
}

/// Compute a blake3 hash of the chunk text, returned as a hex string.
pub fn chunk_hash(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

/// Split markdown content into chunks, respecting structure.
///
/// Strategy:
/// 1. Split on `##` headers — each section is a candidate chunk
/// 2. If a section exceeds `max_chunk_chars`, split on paragraph boundaries (`\n\n`)
/// 3. If a paragraph still exceeds `max_chunk_chars`, split on line boundaries
/// 4. Continuation chunks are prefixed with ancestor header context
///
/// When a section is split into multiple sub-chunks, each continuation chunk
/// is prefixed with the last `chunk_overlap_chars` of the previous chunk for
/// embedding continuity, plus ancestor header context.
pub fn chunk_markdown(content: &str, config: &MemoryIndexConfig) -> Vec<Chunk> {
    if content.is_empty() {
        return vec![];
    }

    let max_chars = config.max_chunk_chars.max(1);
    let lines: Vec<&str> = content.lines().collect();

    if lines.is_empty() {
        return vec![];
    }

    // If the entire content fits in one chunk, return it directly.
    if fits(content, max_chars) {
        return vec![Chunk {
            text: content.to_string(),
            start_line: 0,
            end_line: lines.len(),
        }];
    }

    // Split into sections by ## headers
    let sections = split_by_headers(&lines);
    let mut chunks = Vec::new();

    for section in &sections {
        let section_text = section.lines.join("\n");

        if fits(
            &add_header_context(&section.header_context, &section_text),
            max_chars,
        ) {
            chunks.push(Chunk {
                text: add_header_context(&section.header_context, &section_text),
                start_line: section.start_line,
                end_line: section.start_line + section.lines.len(),
            });
        } else {
            // Section too large — split on paragraph boundaries
            let sub_chunks =
                split_section_by_paragraphs(section, max_chars, config.chunk_overlap_chars);
            chunks.extend(sub_chunks);
        }
    }

    chunks
}

/// A section of the document delimited by headers.
struct Section<'a> {
    /// The lines in this section (including the header line itself).
    lines: Vec<&'a str>,
    /// 0-based start line index in the original document.
    start_line: usize,
    /// Ancestor header context (e.g., `"## Architecture > ### Design"`).
    header_context: String,
}

/// Split lines into sections by `##` (or deeper) headers.
fn split_by_headers<'a>(lines: &[&'a str]) -> Vec<Section<'a>> {
    let mut sections: Vec<Section<'a>> = Vec::new();
    let mut current_lines: Vec<&'a str> = Vec::new();
    let mut current_start = 0;
    let mut header_stack: Vec<(usize, String)> = Vec::new(); // (level, text)
    let mut fence: Option<(char, usize)> = None;
    for (i, &line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let marker = trimmed
            .chars()
            .next()
            .filter(|character| matches!(character, '`' | '~'));
        let fence_line = marker.map(|marker| {
            (
                marker,
                trimmed
                    .chars()
                    .take_while(|character| *character == marker)
                    .count(),
            )
        });
        let in_code = fence.is_some();
        if let Some((marker, count)) = fence_line.filter(|(_, count)| *count >= 3) {
            match fence {
                None => fence = Some((marker, count)),
                Some((old, width)) if old == marker && count >= width => fence = None,
                _ => {}
            }
        }
        if let Some(level) = (!in_code && fence.is_none())
            .then(|| header_level(line))
            .flatten()
        {
            // Flush previous section
            if !current_lines.is_empty() {
                sections.push(Section {
                    lines: std::mem::take(&mut current_lines),
                    start_line: current_start,
                    header_context: format_header_context(&header_stack),
                });
            }
            current_start = i;

            // Update header stack: pop headers at same or deeper level
            while header_stack.last().is_some_and(|(l, _)| *l >= level) {
                header_stack.pop();
            }
            header_stack.push((level, line.to_string()));
        }
        current_lines.push(line);
    }

    // Flush final section
    if !current_lines.is_empty() {
        sections.push(Section {
            lines: current_lines,
            start_line: current_start,
            header_context: format_header_context(&header_stack),
        });
    }

    sections
}

/// Split a large section into sub-chunks by paragraph boundaries (`\n\n`).
/// Continuation chunks are prefixed with the last `overlap_chars` of the
/// previous chunk for embedding continuity.
pub const CHUNK_PIPELINE_VERSION: &str = "token-structural-v3";
const TOKEN_BUDGET: usize = 384;
const OVERLAP_TOKENS: usize = 64;

fn fits(text: &str, max_chars: usize) -> bool {
    text.chars().count() <= max_chars && crate::local::token_count(text) <= TOKEN_BUDGET
}

fn split_section_by_paragraphs(
    section: &Section<'_>,
    max_chars: usize,
    overlap_chars: usize,
) -> Vec<Chunk> {
    let text = section.lines.join("\n");
    let boundaries = text
        .char_indices()
        .map(|(offset, _)| offset)
        .chain(std::iter::once(text.len()))
        .collect::<Vec<_>>();
    let own_heading = section
        .lines
        .first()
        .filter(|line| header_level(line).is_some())
        .copied()
        .unwrap_or("");
    let continuation = [section.header_context.as_str(), own_heading]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" > ");
    let mut chunks = Vec::new();
    let mut start = 0;
    while start + 1 < boundaries.len() {
        let context = if start == 0 {
            &section.header_context
        } else {
            &continuation
        };
        let mut context = context
            .chars()
            .take((max_chars / 3).max(1))
            .collect::<String>();
        while !context.is_empty() && crate::local::token_count(&context) > 64 {
            context.pop();
        }
        while !context.is_empty() && context.chars().count() + 14 >= max_chars {
            context.pop();
        }
        let prefix = if context.is_empty() {
            String::new()
        } else {
            format!("[Context: {context}]\n\n")
        };
        let mut lo = start + 1;
        let mut hi = (start + max_chars.max(1)).min(boundaries.len() - 1);
        while lo < hi {
            let mid = (lo + hi + 1) / 2;
            if fits(
                &format!("{prefix}{}", &text[boundaries[start]..boundaries[mid]]),
                max_chars,
            ) {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        let mut end = lo;
        // Prefer sentence/line boundaries near the end without producing tiny chunks.
        if end < boundaries.len() - 1 {
            let floor = start + (end - start) * 2 / 3;
            for candidate in (floor.max(start + 1)..=end).rev() {
                let previous = &text[boundaries[candidate - 1]..boundaries[candidate]];
                if matches!(previous, "\n" | "。" | "！" | "？" | "；") {
                    end = candidate;
                    break;
                }
            }
        }
        let body = &text[boundaries[start]..boundaries[end]];
        if !body.trim().is_empty() {
            let start_line = section.start_line
                + text[..boundaries[start]]
                    .bytes()
                    .filter(|b| *b == b'\n')
                    .count();
            let before_end = &text[..boundaries[end]];
            let end_line = section.start_line
                + before_end.bytes().filter(|b| *b == b'\n').count()
                + usize::from(!before_end.ends_with('\n'));
            chunks.push(Chunk {
                text: format!("{prefix}{}", body.trim()),
                start_line,
                end_line: end_line.max(start_line + 1),
            });
        }
        if end == boundaries.len() - 1 {
            break;
        }
        let overlap = overlap_chars.min((end - start) / 2);
        let mut next = end.saturating_sub(overlap).max(start + 1);
        while next < end
            && crate::local::token_count(&text[boundaries[next]..boundaries[end]]) > OVERLAP_TOKENS
        {
            next += 1;
        }
        start = next;
    }
    chunks
}

/// Detect markdown header level (1 for `#`, 2 for `##`, etc.). Returns `None` if not a header.
pub(crate) fn header_level(line: &str) -> Option<usize> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('#') {
        return None;
    }
    let level = trimmed.chars().take_while(|&c| c == '#').count();
    // Must be followed by a space or end of line to be a valid header
    let rest = &trimmed[level..];
    if rest.is_empty() || rest.starts_with(' ') {
        Some(level)
    } else {
        None
    }
}

/// Format header stack into a context string like `"## Section > ### Subsection"`.
fn format_header_context(stack: &[(usize, String)]) -> String {
    if stack.len() <= 1 {
        return String::new();
    }
    // Skip the last entry (it's the current section's own header)
    stack[..stack.len() - 1]
        .iter()
        .map(|(_, text)| text.trim().to_string())
        .collect::<Vec<_>>()
        .join(" > ")
}

/// Prepend ancestor header context to chunk text (if non-empty).
fn add_header_context(context: &str, text: &str) -> String {
    if context.is_empty() {
        text.to_string()
    } else {
        format!("[Context: {context}]\n\n{text}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> MemoryIndexConfig {
        MemoryIndexConfig::default()
    }

    #[test]
    fn code_comments_do_not_create_document_sections() {
        let lines = [
            "## 配置说明",
            "```bash",
            "# shell comment",
            "echo value",
            "```",
            "正文",
            "### 真正的小节",
            "说明",
        ];
        let sections = split_by_headers(&lines);
        assert_eq!(sections.len(), 2);
        assert!(sections[0].lines.contains(&"# shell comment"));
        assert!(!sections[1].header_context.contains("shell comment"));
    }

    #[test]
    fn chinese_long_lines_are_bounded_and_keep_the_tail() {
        let content = format!("{}最后结论", "中文知识".repeat(1000));
        let chunks = chunk_markdown(&content, &default_config());
        assert!(chunks.len() > 1);
        assert!(
            chunks
                .iter()
                .all(|c| crate::local::token_count(&c.text) <= TOKEN_BUDGET)
        );
        assert!(chunks.last().unwrap().text.contains("最后结论"));
    }

    #[test]
    fn overlapping_chunks_keep_previous_text_and_heading() {
        let content = format!("## 报销制度\n\n{}\n{}", "甲".repeat(200), "乙".repeat(200));
        let chunks = chunk_markdown(
            &content,
            &MemoryIndexConfig {
                max_chunk_chars: 1600,
                chunk_overlap_chars: 64,
            },
        );
        assert!(chunks.len() > 2);
        assert!(
            chunks
                .iter()
                .skip(1)
                .any(|c| c.text.contains("甲") && c.text.contains("乙"))
        );
        assert!(chunks.iter().skip(1).all(|c| c.text.contains("报销制度")));
    }

    #[test]
    fn test_chunk_hash_deterministic() {
        let h1 = chunk_hash("hello world");
        let h2 = chunk_hash("hello world");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64); // blake3 hex = 64 chars
    }

    #[test]
    fn test_chunk_hash_different_inputs() {
        assert_ne!(chunk_hash("hello"), chunk_hash("world"));
    }

    #[test]
    fn test_chunk_empty_content() {
        let chunks = chunk_markdown("", &default_config());
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_chunk_small_content_single_chunk() {
        let content = "# Title\n\nSome text here.";
        let chunks = chunk_markdown(content, &default_config());
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, content);
        assert_eq!(chunks[0].start_line, 0);
        assert_eq!(chunks[0].end_line, 3);
    }

    #[test]
    fn test_chunk_splits_on_headers() {
        let content = "## Section 1\n\nContent for section 1 goes here with enough text to matter.\n\n\
                        ## Section 2\n\nContent for section 2 is also significant enough to be a chunk.";
        let config = MemoryIndexConfig {
            max_chunk_chars: 80,
            chunk_overlap_chars: 0,
        };
        let chunks = chunk_markdown(content, &config);
        assert!(
            chunks.len() >= 2,
            "should split into at least 2 chunks, got {}",
            chunks.len()
        );
        assert!(chunks[0].text.contains("Section 1"));
        assert!(chunks.last().unwrap().text.contains("Section 2"));
    }

    #[test]
    fn test_chunk_header_context_for_subsections() {
        let content = "## Parent\n\nIntro.\n\n### Child\n\nChild content that is long enough to be its own chunk definitely.";
        let config = MemoryIndexConfig {
            max_chunk_chars: 60,
            chunk_overlap_chars: 0,
        };
        let chunks = chunk_markdown(content, &config);
        // The child section chunk should have parent context
        let child_chunk = chunks.iter().find(|c| c.text.contains("Child content"));
        assert!(child_chunk.is_some(), "should have a child chunk");
        assert!(
            child_chunk.unwrap().text.contains("[Context: ## Parent]"),
            "child chunk should have parent header context, got: {}",
            child_chunk.unwrap().text
        );
    }

    #[test]
    fn test_chunk_large_section_splits_on_paragraphs() {
        let para1 = "A".repeat(100);
        let para2 = "B".repeat(100);
        let content = format!("## Big Section\n\n{para1}\n\n{para2}");
        let config = MemoryIndexConfig {
            max_chunk_chars: 150,
            chunk_overlap_chars: 0,
        };
        let chunks = chunk_markdown(&content, &config);
        assert!(
            chunks.len() >= 2,
            "should split large section, got {} chunks",
            chunks.len()
        );
    }

    #[test]
    fn test_header_level_detection() {
        assert_eq!(header_level("# Title"), Some(1));
        assert_eq!(header_level("## Section"), Some(2));
        assert_eq!(header_level("### Subsection"), Some(3));
        assert_eq!(header_level("#hashtag"), None); // no space after #
        assert_eq!(header_level("not a header"), None);
        assert_eq!(header_level(""), None);
        assert_eq!(header_level("##"), Some(2)); // header with no text
    }

    #[test]
    fn test_chunk_line_numbers() {
        let content = "line 0\nline 1\nline 2\nline 3\nline 4";
        let chunks = chunk_markdown(content, &default_config());
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].start_line, 0);
        assert_eq!(chunks[0].end_line, 5);
    }

    #[test]
    fn test_chunk_preserves_code_blocks() {
        let content =
            "## Code\n\n```rust\nfn main() {\n    println!(\"hello\");\n}\n```\n\nSome text.";
        let chunks = chunk_markdown(content, &default_config());
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].text.contains("```rust"));
        assert!(chunks[0].text.contains("fn main()"));
    }
}
