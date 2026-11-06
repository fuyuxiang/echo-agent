//! Defensive cleanup for model-generated conversation titles and injected
//! desktop context that must never reach user-facing metadata.
//!
//! Some OpenAI-compatible reasoning models return visible `<think>` (or
//! equivalent) blocks in `assistant.content`. Session titles are user-facing,
//! so those blocks must be removed before a title is emitted or restored.

const INTERNAL_TAGS: [&str; 4] = ["think", "thinking", "reasoning", "analysis"];
const MAX_TITLE_SCALARS: usize = 100;
const EXPERT_PERSONA_BEGIN: &str = "<!--EXPERT_PERSONA_BEGIN-->";
const EXPERT_PERSONA_END: &str = "<!--EXPERT_PERSONA_END-->";

fn is_tag_boundary(byte: Option<u8>) -> bool {
    byte.is_some_and(|b| b == b'>' || b.is_ascii_whitespace())
}

fn find_tag(lower: &str, tag: &str, closing: bool, from: usize) -> Option<(usize, Option<usize>)> {
    let needle = if closing {
        format!("</{tag}")
    } else {
        format!("<{tag}")
    };
    let mut offset = from;
    while let Some(relative) = lower[offset..].find(&needle) {
        let start = offset + relative;
        let after_name = start + needle.len();
        if is_tag_boundary(lower.as_bytes().get(after_name).copied()) {
            let end = lower[after_name..]
                .find('>')
                .map(|relative_end| after_name + relative_end + 1);
            return Some((start, end));
        }
        offset = start + 1;
    }
    None
}

fn find_first_opening_tag(lower: &str) -> Option<(usize, &'static str, Option<usize>)> {
    INTERNAL_TAGS
        .into_iter()
        .filter_map(|tag| find_tag(lower, tag, false, 0).map(|(start, end)| (start, tag, end)))
        .min_by_key(|(start, _, _)| *start)
}

fn contains_internal_markup(raw: &str) -> bool {
    let lower = raw.to_ascii_lowercase();
    INTERNAL_TAGS.into_iter().any(|tag| {
        find_tag(&lower, tag, false, 0).is_some() || find_tag(&lower, tag, true, 0).is_some()
    })
}

fn strip_internal_blocks(raw: &str) -> String {
    let mut output = raw.to_string();
    loop {
        let lower = output.to_ascii_lowercase();
        let Some((start, tag, open_end)) = find_first_opening_tag(&lower) else {
            break;
        };
        let Some(open_end) = open_end else {
            output.truncate(start);
            break;
        };
        let lower = output.to_ascii_lowercase();
        let Some((_, Some(close_end))) = find_tag(&lower, tag, true, open_end) else {
            // An unclosed reasoning block is unsafe. Preserve any title text
            // before it, but discard the block and everything after it.
            output.truncate(start);
            break;
        };
        output.replace_range(start..close_end, "");
    }
    output
}

fn contains_expert_persona_markup(raw: &str) -> bool {
    raw.contains(EXPERT_PERSONA_BEGIN) || raw.contains(EXPERT_PERSONA_END)
}

fn strip_expert_persona_blocks(raw: &str) -> String {
    let mut output = raw.to_string();
    while let Some(start) = output.find(EXPERT_PERSONA_BEGIN) {
        let after_open = start + EXPERT_PERSONA_BEGIN.len();
        let Some(relative_end) = output[after_open..].find(EXPERT_PERSONA_END) else {
            output.truncate(start);
            break;
        };
        let end = after_open + relative_end + EXPERT_PERSONA_END.len();
        output.replace_range(start..end, "");
    }
    output
}

fn strip_system_reminder_blocks(raw: &str) -> String {
    const OPEN: &str = "<system-reminder>";
    const CLOSE: &str = "</system-reminder>";
    let mut output = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find(OPEN) {
        output.push_str(&rest[..start]);
        let after_open = &rest[start + OPEN.len()..];
        let Some(end) = after_open.find(CLOSE) else {
            return output;
        };
        rest = &after_open[end + CLOSE.len()..];
    }
    output.push_str(rest);
    output
}

/// Recover the user-authored portion of a raw prompt persisted by an older
/// desktop build. Reserved injected blocks are removed before any title logic.
fn visible_user_text(raw: &str) -> String {
    let without_persona = strip_expert_persona_blocks(raw);
    if contains_expert_persona_markup(&without_persona) {
        return String::new();
    }
    strip_system_reminder_blocks(&without_persona)
        .trim()
        .to_string()
}

/// Return a safe, single-line automatic title, or `None` when the model only
/// produced reasoning/internal markup. Manual titles deliberately bypass this
/// function so user-authored angle-bracket text remains untouched.
pub(crate) fn clean_auto_title(raw: &str) -> Option<String> {
    let stripped = strip_expert_persona_blocks(&strip_internal_blocks(raw));
    if contains_internal_markup(&stripped) {
        return None;
    }
    if contains_expert_persona_markup(&stripped) {
        return None;
    }

    let mut title = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
    for label in [
        "Session title:",
        "Session Title:",
        "Title:",
        "title:",
        "标题：",
        "标题:",
    ] {
        if let Some(rest) = title.strip_prefix(label) {
            title = rest.trim_start().to_string();
            break;
        }
    }

    if title.len() >= 2 {
        let quoted = (title.starts_with('"') && title.ends_with('"'))
            || (title.starts_with('\'') && title.ends_with('\''))
            || (title.starts_with('“') && title.ends_with('”'));
        if quoted {
            let first = title.chars().next().map(char::len_utf8).unwrap_or(0);
            let last = title.chars().next_back().map(char::len_utf8).unwrap_or(0);
            title = title[first..title.len() - last].trim().to_string();
        }
    }

    if title.is_empty() {
        return None;
    }
    if title == "New session" {
        title = "新会话".to_string();
    }
    if title.chars().count() > MAX_TITLE_SCALARS {
        title = title.chars().take(MAX_TITLE_SCALARS).collect();
    }
    Some(title)
}

/// Produce the same compact fallback shape used for a first prompt. This is
/// used only to recover historical automatic titles that consist of injected
/// expert markup; manual user titles bypass it.
pub(crate) fn fallback_title_from_user_text(raw: &str) -> Option<String> {
    let visible = visible_user_text(raw);
    let clause = visible
        .split(['\n', '。', '！', '？', '；', '，'])
        .map(str::trim)
        .find(|part| !part.is_empty())?;
    let words = clause
        .split_whitespace()
        .take(10)
        .collect::<Vec<_>>()
        .join(" ");
    clean_auto_title(&words)
}

#[cfg(test)]
mod tests {
    use super::{clean_auto_title, fallback_title_from_user_text};

    #[test]
    fn textless_opening_fallback_uses_the_desktop_language() {
        assert_eq!(clean_auto_title("New session").as_deref(), Some("新会话"));
    }

    #[test]
    fn removes_closed_reasoning_before_title() {
        assert_eq!(
            clean_auto_title("<think>The user only greeted twice.</think>\n简短问候与任务确认")
                .as_deref(),
            Some("简短问候与任务确认")
        );
    }

    #[test]
    fn rejects_unclosed_or_reasoning_only_output() {
        assert_eq!(clean_auto_title("<think>internal reasoning"), None);
        assert_eq!(clean_auto_title("<analysis>internal</analysis>"), None);
    }

    #[test]
    fn handles_case_attributes_labels_and_quotes() {
        assert_eq!(
            clean_auto_title(
                "<THINK data-mode=\"deep\">internal</THINK> Title: \"Fix login redirect\""
            )
            .as_deref(),
            Some("Fix login redirect")
        );
    }

    #[test]
    fn rejects_orphan_internal_closing_tag() {
        assert_eq!(clean_auto_title("reasoning</think>Final title"), None);
    }

    #[test]
    fn removes_expert_context_and_rejects_orphan_markers() {
        let raw =
            "<!--EXPERT_PERSONA_BEGIN-->\nexpert\n<!--EXPERT_PERSONA_END-->\n\n帮我写秋天的文章";
        assert_eq!(clean_auto_title(raw).as_deref(), Some("帮我写秋天的文章"));
        assert_eq!(clean_auto_title("<!--EXPERT_PERSONA_BEGIN-->"), None);
        assert_eq!(clean_auto_title("<!--EXPERT_PERSONA_END-->"), None);
        assert_eq!(
            fallback_title_from_user_text(raw).as_deref(),
            Some("帮我写秋天的文章")
        );
    }
}
