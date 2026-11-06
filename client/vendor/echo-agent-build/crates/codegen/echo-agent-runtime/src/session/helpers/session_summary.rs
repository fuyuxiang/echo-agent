// Modified by EchoAgent for the embedded desktop runtime; see ECHOAGENT_VENDOR.md.
//! Session title generation via a lightweight LLM side call.

use crate::sampling::{Client as OaiCompatClient, ConversationItem, ConversationRequest};
use crate::session::helpers::chat::floor_char_boundary;

/// Upper bound on the user text that feeds title generation; titles only need
/// the opening, and this keeps the request well under the model prompt limit.
const TITLE_SOURCE_MAX_BYTES: usize = 8_000;

/// Real-user turn counts at which the auto title is refreshed from the whole
/// conversation, then frozen. Turn 1's title comes from the fast first-prompt
/// path; refreshing at a couple of early turns lets the title catch up to the
/// real topic without churning enough to make sessions hard to recognize. A
/// manual `/rename` always wins and stops refreshes.
pub(crate) const TITLE_REFRESH_TURNS: [usize; 2] = [3, 6];

/// Number of [`TITLE_REFRESH_TURNS`] checkpoints reached at `turns` real-user
/// turns — i.e. the checkpoint index to advance to, catching up past any
/// checkpoints a burst of turns jumped over. Equal to `TITLE_REFRESH_TURNS.len()`
/// means the title is frozen.
pub(crate) fn checkpoints_reached(turns: usize) -> usize {
    TITLE_REFRESH_TURNS.iter().filter(|&&t| turns >= t).count()
}

/// Hard byte cap guarding runaway title output; the instruction already targets
/// 5-10 words. Applied on a char boundary, so a multibyte title is capped a
/// little shorter — fine for a safety bound.
const TITLE_MAX_BYTES: usize = 80;

/// Desktop-reserved delimiters around an expert persona injected into a model
/// prompt. This context is intentionally invisible to the user and therefore
/// must never participate in title generation or refresh.
const EXPERT_PERSONA_BEGIN: &str = "<!--EXPERT_PERSONA_BEGIN-->";
const EXPERT_PERSONA_END: &str = "<!--EXPERT_PERSONA_END-->";

const INITIAL_TITLE_SYSTEM_PROMPT: &str = r#"Generate a concise, distinctive title for the user's task.

Rules:
- Match the language used by the user. Keep code identifiers unchanged.
- Capture the intended outcome or actual topic, not the conversational phrasing.
- Prefer a concrete action + object + differentiating scope when the request implies an action.
- Remove filler such as "please", "help me", "take a look", "请", "帮我", "看看", and "一下".
- For Chinese, usually use 6-16 Chinese characters. For space-delimited languages, use 4-8 words.
- Do not invent details that are not present in the request.
- Output only the title as plain text: no quotes, label, explanation, markdown, or punctuation at the end.

Examples:
- "任务的标题总感觉不够智能，这块你看看怎么优化下" -> "优化任务标题生成"
- "Can you look into why login redirects forever?" -> "Fix Login Redirect Loop""#;

/// XML-like tags emitted by some reasoning-capable OpenAI-compatible models
/// inside `assistant.content`. They are internal reasoning, never a title.
const INTERNAL_REASONING_TAGS: [&str; 4] = ["think", "thinking", "reasoning", "analysis"];

fn is_tag_boundary(byte: Option<u8>) -> bool {
    byte.is_some_and(|b| b == b'>' || b.is_ascii_whitespace())
}

fn find_reasoning_tag(
    lower: &str,
    tag: &str,
    closing: bool,
    from: usize,
) -> Option<(usize, Option<usize>)> {
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

fn find_first_reasoning_tag(lower: &str) -> Option<(usize, &'static str, Option<usize>)> {
    INTERNAL_REASONING_TAGS
        .into_iter()
        .filter_map(|tag| {
            find_reasoning_tag(lower, tag, false, 0).map(|(start, end)| (start, tag, end))
        })
        .min_by_key(|(start, _, _)| *start)
}

/// Whether a title still contains an opening or closing internal-reasoning tag.
/// Used by persisted-session display as a defense for titles written by older
/// builds before generation-time cleanup existed.
pub(crate) fn contains_internal_reasoning_markup(raw: &str) -> bool {
    let lower = raw.to_ascii_lowercase();
    INTERNAL_REASONING_TAGS.into_iter().any(|tag| {
        find_reasoning_tag(&lower, tag, false, 0).is_some()
            || find_reasoning_tag(&lower, tag, true, 0).is_some()
    })
}

fn strip_internal_reasoning_blocks(raw: &str) -> String {
    let mut output = raw.to_string();
    loop {
        let lower = output.to_ascii_lowercase();
        let Some((start, tag, open_end)) = find_first_reasoning_tag(&lower) else {
            break;
        };
        let Some(open_end) = open_end else {
            output.truncate(start);
            break;
        };
        let lower = output.to_ascii_lowercase();
        let Some((_, Some(close_end))) = find_reasoning_tag(&lower, tag, true, open_end) else {
            // A truncated/unclosed reasoning block is not safe title text.
            // Keep anything before it, but discard the block and its tail.
            output.truncate(start);
            break;
        };
        output.replace_range(start..close_end, "");
    }
    output
}

pub(crate) fn contains_expert_persona_markup(raw: &str) -> bool {
    raw.contains(EXPERT_PERSONA_BEGIN) || raw.contains(EXPERT_PERSONA_END)
}

/// Remove every complete expert-persona block. An unterminated opening marker
/// discards its tail because all following text is ambiguous injected context;
/// a stray closing marker remains detectable and is rejected by title cleanup.
pub(crate) fn strip_expert_persona_blocks(raw: &str) -> String {
    let mut output = raw.to_string();
    loop {
        let Some(start) = output.find(EXPERT_PERSONA_BEGIN) else {
            break;
        };
        let after_open = start + EXPERT_PERSONA_BEGIN.len();
        let Some(relative_end) = output[after_open..].find(EXPERT_PERSONA_END) else {
            output.truncate(start);
            break;
        };
        let end = after_open + relative_end + EXPERT_PERSONA_END.len();
        output.replace_range(start..end, "");
    }
    output.trim().to_string()
}

/// Durable title-refresh checkpoint watermark under `{session_dir}/`: the number
/// of [`TITLE_REFRESH_TURNS`] checkpoints already consumed. Written on every
/// completed attempt (success or failure) so the freeze survives resume,
/// restart, and compaction; only a committed value is persisted, so an aborted
/// refresh still retries. See [`load_title_refresh_watermark`]. Public so the
/// fork/copy path can carry it alongside the inherited title.
pub(crate) const TITLE_REFRESH_WATERMARK_FILE: &str = "title_refresh_idx";

/// Load the persisted checkpoint index, clamped to the number of checkpoints so
/// a stale larger value still means "frozen". `None` when the session has no
/// watermark yet (fresh, pre-feature, or feature-was-off) — the caller decides
/// the starting checkpoint for that case (see [`initial_title_refresh_idx`]).
pub(crate) fn load_title_refresh_watermark(session_dir: &std::path::Path) -> Option<usize> {
    std::fs::read_to_string(session_dir.join(TITLE_REFRESH_WATERMARK_FILE))
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .map(|idx| idx.min(TITLE_REFRESH_TURNS.len()))
}

/// The checkpoint index a session starts at on spawn, given its persisted
/// `watermark` (`None` if unmanaged), whether the feature is `enabled`, and the
/// current real-user-turn count.
///
/// A managed session (has a watermark) uses it — authoritative and durable
/// across compaction. An unmanaged session is *adopted* as open (`0`) only when
/// the feature is enabled and it is brand new (no turns); otherwise it freezes.
/// That freezes pre-feature sessions, sessions created while the feature was
/// off, and anything already past the window, so they are never retitled — with
/// no turn-count guessing that compaction could distort.
pub(crate) fn initial_title_refresh_idx(
    watermark: Option<usize>,
    enabled: bool,
    turns: usize,
) -> usize {
    match watermark {
        Some(idx) => idx,
        None if enabled && turns == 0 => 0,
        None => TITLE_REFRESH_TURNS.len(),
    }
}

/// Persist the checkpoint index after a completed attempt. Best-effort, and
/// written atomically (temp sibling + rename) so a crash mid-write can't leave a
/// partial/empty file that would load as `0` and reopen the refresh window.
pub(crate) fn save_title_refresh_watermark(session_dir: &std::path::Path, idx: usize) {
    if !session_dir.is_dir() {
        return;
    }
    let path = session_dir.join(TITLE_REFRESH_WATERMARK_FILE);
    if let Err(e) = crate::session::storage::write_bytes_atomic(&path, idx.to_string().as_bytes()) {
        tracing::warn!(error = %e, path = %path.display(), "failed to persist title refresh watermark");
    }
}

/// Remove `<system-reminder>…</system-reminder>` blocks from `text` — they are
/// system-injected context (e.g. the `/goal` setup reminder), not the user's
/// words, so they must not drive the session title.
fn strip_system_reminder_blocks(text: &str) -> String {
    const OPEN: &str = "<system-reminder>";
    const CLOSE: &str = "</system-reminder>";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        out.push_str(&rest[..start]);
        let after_open = &rest[start + OPEN.len()..];
        // An unterminated reminder drops the remainder — it is system text.
        let Some(end) = after_open.find(CLOSE) else {
            return out.trim().to_string();
        };
        rest = &after_open[end + CLOSE.len()..];
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// Text the session title is derived from: strip system reminders and skill XML
/// markup, then cap to the first few KB. Stripping runs before the cap so a
/// leading reminder larger than the cap is still removed.
fn title_source_text(user_message: &str) -> String {
    let without_persona = strip_expert_persona_blocks(user_message);
    let without_reminders = strip_system_reminder_blocks(&without_persona);
    let base = &without_reminders;
    let mut display =
        echo_agent_tools::implementations::skills::skill::extract_skill_display_text(base)
            .unwrap_or_else(|| base.to_string());
    display.truncate(floor_char_boundary(&display, TITLE_SOURCE_MAX_BYTES));
    display
}

pub(crate) fn title_fallback_from_user_text(user_message: &str) -> String {
    let text = title_source_text(user_message);
    // Prefer the first meaningful clause. This especially matters for Chinese,
    // where splitting on whitespace alone would preserve the entire prompt.
    let clause = text
        .split(['\n', '。', '！', '？', '；', '，'])
        .map(str::trim)
        .find(|part| !part.is_empty())
        .unwrap_or_default();
    let s = clause
        .split_whitespace()
        .take(10)
        .collect::<Vec<_>>()
        .join(" ");
    let cleaned = clean_title_text(&s);
    if cleaned.is_empty() {
        "New session".to_string()
    } else {
        cleaned
    }
}

/// Generate the initial session title from the first user message, for the fast
/// first-prompt path ([`crate::session::summary::SummaryGenerator`]). The title
/// is later refreshed from the whole conversation at the early checkpoints in
/// [`TITLE_REFRESH_TURNS`], then frozen.
pub async fn generate_session_summary(
    user_message: String,
    client: OaiCompatClient,
    model: &str,
) -> String {
    let clean_message = title_source_text(&user_message);
    if clean_message.trim().is_empty() {
        return title_fallback_from_user_text(&clean_message);
    }
    let request = ConversationRequest::from_items(vec![
        ConversationItem::system(INITIAL_TITLE_SYSTEM_PROMPT),
        ConversationItem::user(format!(
            r#"<user_query>
{}
</user_query>"#,
            clean_message
        )),
    ])
    .with_model(model)
    .with_max_output_tokens(60)
    .with_temperature(0.2);

    match client.conversation_collect(request).await {
        Ok(response) => {
            let title = clean_title_text(&response.assistant_text());
            if !title.is_empty() {
                return title;
            }
            tracing::debug!(
                model = %model,
                "session title generation returned no usable plain-text title"
            );
        }
        Err(e) => {
            tracing::warn!(
                model = %model,
                error = %e,
                "session title generation failed, falling back to truncated user text"
            );
        }
    }
    title_fallback_from_user_text(&clean_message)
}

/// Instruction turn appended to a conversation snapshot to refresh the auto
/// title. Same single-reminder-wrapped-turn design as the recap / turn-summary
/// side-calls so the conversation prefix is reused verbatim and the prompt
/// cache stays warm. The model sees the whole conversation, so the title
/// reflects the real topic rather than a possibly-useless first prompt.
pub(crate) fn title_refresh_instruction(tag: &str) -> String {
    format!(
        "<{tag}>Generate a concise, distinctive title for the WHOLE conversation above, based on \
         what the user is actually trying to accomplish rather than just the first message.\n\n\
         Match the language used by the user and keep code identifiers unchanged. Prefer a concrete \
         action + object + differentiating scope. Remove conversational filler such as \"please\", \
         \"help me\", \"take a look\", \"请\", \"帮我\", \"看看\", and \"一下\". For Chinese, usually \
         use 6-16 Chinese characters; for space-delimited languages, use 4-8 words. Never invent \
         details not established in the conversation. User-role messages wrapped in reminder tags \
         like this one are injected context, not the user.\n\n\
         Output ONLY the title as plain text: no quotes, labels, explanation, markdown, or punctuation \
         at the end. Do NOT call tools.</{tag}>"
    )
}

/// Clean a refreshed title into a one-line string: recap normalization
/// (whitespace collapse, stray label/quote stripping) plus the
/// [`TITLE_MAX_BYTES`] cap.
pub(crate) fn clean_title_text(raw: &str) -> String {
    let without_persona = strip_expert_persona_blocks(raw);
    if contains_expert_persona_markup(&without_persona) {
        return String::new();
    }
    // Strip reasoning before applying the byte cap. Otherwise a long `<think>`
    // prefix is truncated first and the actual title after `</think>` is lost.
    let without_reasoning = strip_internal_reasoning_blocks(&without_persona);
    if contains_internal_reasoning_markup(&without_reasoning) {
        return String::new();
    }
    let mut out = crate::session::helpers::session_recap::clean_recap_text(&without_reasoning);
    if out.len() > TITLE_MAX_BYTES {
        let cut = floor_char_boundary(&out, TITLE_MAX_BYTES);
        out.truncate(cut);
        out = out.trim_end().to_string();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        TITLE_SOURCE_MAX_BYTES, clean_title_text, strip_expert_persona_blocks,
        strip_system_reminder_blocks, title_fallback_from_user_text, title_refresh_instruction,
        title_source_text,
    };

    #[test]
    fn checkpoints_reached_counts_and_catches_up() {
        use super::{TITLE_REFRESH_TURNS, checkpoints_reached};
        assert_eq!(checkpoints_reached(0), 0);
        assert_eq!(checkpoints_reached(2), 0);
        assert_eq!(checkpoints_reached(3), 1);
        assert_eq!(checkpoints_reached(5), 1);
        assert_eq!(checkpoints_reached(6), 2);
        // A burst past the last checkpoint catches up to frozen, no overshoot.
        assert_eq!(checkpoints_reached(50), TITLE_REFRESH_TURNS.len());
    }

    /// The freeze watermark round-trips (durable across a shortened conversation,
    /// e.g. compaction), reads `None` when absent, and clamps a stale-large value.
    #[test]
    fn title_refresh_watermark_round_trips_and_clamps() {
        use super::{
            TITLE_REFRESH_TURNS, TITLE_REFRESH_WATERMARK_FILE, load_title_refresh_watermark,
            save_title_refresh_watermark,
        };
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(
            load_title_refresh_watermark(dir.path()),
            None,
            "missing → None"
        );
        save_title_refresh_watermark(dir.path(), 1);
        assert_eq!(load_title_refresh_watermark(dir.path()), Some(1));
        std::fs::write(dir.path().join(TITLE_REFRESH_WATERMARK_FILE), "99").unwrap();
        assert_eq!(
            load_title_refresh_watermark(dir.path()),
            Some(TITLE_REFRESH_TURNS.len()),
            "stale-large watermark reads as frozen"
        );
    }

    /// A managed session (has a watermark) uses it. An unmanaged session is
    /// adopted open only when the feature is enabled and brand new; otherwise it
    /// freezes (pre-feature, feature-off, or already past the window) — no turn
    /// guessing that compaction could distort.
    #[test]
    fn initial_title_refresh_idx_adopts_only_fresh_enabled_sessions() {
        use super::{TITLE_REFRESH_TURNS, initial_title_refresh_idx};
        let frozen = TITLE_REFRESH_TURNS.len();
        // Managed: watermark is authoritative regardless of enabled/turns.
        assert_eq!(initial_title_refresh_idx(Some(1), true, 9), 1);
        assert_eq!(initial_title_refresh_idx(Some(frozen), true, 0), frozen);
        // Unmanaged + enabled + brand new → adopt open.
        assert_eq!(initial_title_refresh_idx(None, true, 0), 0);
        // Unmanaged but already has turns (pre-feature, even if compacted) → frozen.
        assert_eq!(initial_title_refresh_idx(None, true, 5), frozen);
        // Unmanaged + feature off → frozen even when brand new.
        assert_eq!(initial_title_refresh_idx(None, false, 0), frozen);
    }

    #[test]
    fn title_refresh_instruction_wraps_tag_and_asks_for_whole_conversation() {
        let text = title_refresh_instruction("system-reminder");
        assert!(text.starts_with("<system-reminder>"));
        assert!(text.ends_with("</system-reminder>"));
        assert!(text.contains("WHOLE conversation"));
        assert!(text.contains("6-16 Chinese characters"));
        assert!(text.contains("action + object"));
    }

    #[test]
    fn clean_title_normalizes_and_caps() {
        // Collapses whitespace and strips surrounding quotes.
        assert_eq!(
            clean_title_text("\"Fix the auth  bug\""),
            "Fix the auth bug"
        );
        let capped = clean_title_text(&"word ".repeat(50));
        assert!(capped.len() <= super::TITLE_MAX_BYTES);
    }

    #[test]
    fn clean_title_removes_reasoning_before_applying_cap() {
        let raw = format!(
            "<think>{}</think>\n简短问候与任务确认",
            "private reasoning ".repeat(20)
        );
        assert_eq!(clean_title_text(&raw), "简短问候与任务确认");
    }

    #[test]
    fn clean_title_rejects_unclosed_or_reasoning_only_output() {
        assert!(clean_title_text("<think>private reasoning").is_empty());
        assert!(clean_title_text("<analysis>private</analysis>").is_empty());
    }

    #[test]
    fn clean_title_handles_case_and_tag_attributes() {
        assert_eq!(
            clean_title_text("<THINK data-mode=\"deep\">private</THINK>\nFix login redirect"),
            "Fix login redirect"
        );
    }

    #[test]
    fn title_source_text_caps_oversized_input() {
        let big = "word ".repeat(10_000);
        let out = title_source_text(&big);
        assert!(!out.is_empty() && out.len() <= TITLE_SOURCE_MAX_BYTES);
    }

    #[test]
    fn title_source_text_cap_is_utf8_safe() {
        // 3-byte chars straddle the byte cap; must truncate on a boundary, not panic.
        let big = "あ".repeat(10_000);
        let out = title_source_text(&big);
        assert!(!out.is_empty() && out.len() <= TITLE_SOURCE_MAX_BYTES);
    }

    #[test]
    fn title_source_text_strips_leading_reminder_larger_than_cap() {
        // A leading reminder bigger than the cap must still be stripped, so the
        // title derives from the objective rather than reminder text.
        let reminder = "x".repeat(TITLE_SOURCE_MAX_BYTES * 2);
        let input =
            format!("<system-reminder>\n{reminder}\n</system-reminder>\n\nbuild a mario game");
        let out = title_source_text(&input);
        assert_eq!(out, "build a mario game");
    }

    #[test]
    fn strip_removes_goal_setup_reminder_leaving_objective() {
        let input = "<system-reminder>\nA goal has been set: do stuff\nlots of rules\nStart \
                     now.\n</system-reminder>\n\nbuild a mario platformer game";
        assert_eq!(
            strip_system_reminder_blocks(input),
            "build a mario platformer game"
        );
    }

    #[test]
    fn strip_handles_unterminated_reminder() {
        assert_eq!(
            strip_system_reminder_blocks("<system-reminder>\nrules with no close tag"),
            ""
        );
    }

    #[test]
    fn strip_no_reminder_is_identity() {
        assert_eq!(
            strip_system_reminder_blocks("fix the auth bug"),
            "fix the auth bug"
        );
    }

    /// Regression: a `/goal <objective>` first turn must title off the
    /// objective, not the injected `<system-reminder>` setup block.
    #[test]
    fn fallback_titles_off_goal_objective_not_reminder() {
        let input = "<system-reminder>\nA goal has been set: do stuff\nStart \
                     now.\n</system-reminder>\n\nbuild a mario platformer game in html";
        assert_eq!(
            title_fallback_from_user_text(input),
            "build a mario platformer game in html"
        );
    }

    #[test]
    fn fallback_trims_to_words() {
        assert_eq!(
            title_fallback_from_user_text(
                "one two three four five six seven eight nine ten eleven"
            ),
            "one two three four five six seven eight nine ten"
        );
    }

    #[test]
    fn fallback_uses_first_chinese_clause_and_caps_safely() {
        assert_eq!(
            title_fallback_from_user_text("任务的标题总感觉不够智能，这块你看看怎么优化下"),
            "任务的标题总感觉不够智能"
        );
        let title = title_fallback_from_user_text(&"超长中文任务描述".repeat(20));
        assert!(!title.is_empty() && title.len() <= super::TITLE_MAX_BYTES);
        assert!(title.is_char_boundary(title.len()));
    }

    #[test]
    fn fallback_new_session_when_whitespace_only() {
        assert_eq!(title_fallback_from_user_text("   \n\t"), "New session");
    }

    #[test]
    fn fallback_strips_skill_xml_with_args() {
        let input = "<command-name>implement</command-name>\n\
                      <command-message>/implement</command-message>\n\
                      <command-args>fix the rendering bug</command-args>";
        assert_eq!(
            title_fallback_from_user_text(input),
            "/implement fix the rendering bug",
        );
    }

    #[test]
    fn fallback_strips_skill_xml_no_args() {
        let input = "<command-name>deploy</command-name>\n\
                      <command-message>/deploy</command-message>";
        assert_eq!(title_fallback_from_user_text(input), "/deploy");
    }

    #[test]
    fn expert_persona_never_becomes_a_title_source_or_title() {
        let input = "<!--EXPERT_PERSONA_BEGIN-->\nexpert instructions\n\
                     <!--EXPERT_PERSONA_END-->\n\n帮我写一个关于秋天的文章";
        assert_eq!(
            title_fallback_from_user_text(input),
            "帮我写一个关于秋天的文章"
        );
        assert_eq!(clean_title_text(input), "帮我写一个关于秋天的文章");
        assert_eq!(clean_title_text("<!--EXPERT_PERSONA_BEGIN-->"), "");
        assert_eq!(clean_title_text("<!--EXPERT_PERSONA_END-->"), "");
        assert_eq!(
            strip_expert_persona_blocks(input),
            "帮我写一个关于秋天的文章"
        );
    }

    #[test]
    fn fallback_plain_text_unaffected() {
        assert_eq!(
            title_fallback_from_user_text("fix the auth bug in login.rs"),
            "fix the auth bug in login.rs",
        );
    }
}
