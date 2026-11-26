//! Session recap generation helpers.
//!
//! A *recap* is a short "where was I" summary of the session so far, modelled
//! on common coding-agent `/recap` + automatic session-recap features. Unlike
//! compaction, a recap never mutates the conversation: it is generated from a
//! read-only snapshot and surfaced to the client for display only.
//!
//! Generation reuses the parent session's conversation prefix verbatim (so the
//! provider prompt cache stays warm) and appends a single instruction turn that
//! asks for the recap. The pure helpers here build that request and tidy the
//! model's output; the actual model call lives on the `SessionActor`
//! (`handle_recap`).

use crate::sampling::ConversationItem;
use crate::session::helpers::chat::floor_char_boundary;
use echo_agent_chat_state::{compaction_utils, estimate_conversation_tokens, estimate_item_tokens};

/// Hard cap on the recap text length (characters). Generous headroom: the recap
/// instruction targets ~25–40 words (≈240 chars at the top end), so this only
/// guards against runaway model output and never cuts a normal recap.
const RECAP_MAX_CHARS: usize = 1200;

/// Build the instruction turn appended to the conversation snapshot.
///
/// All recap directions live in this single user message (wrapped in a
/// `<system-reminder>`) rather than a separate system prompt, so the
/// conversation prefix — including the agent's real system prompt at
/// `conversation[0]` — is reused verbatim and the prompt cache stays warm.
///
/// `tag` is the reminder tag for the active harness (`"system-reminder"`, or
/// template-specific tags).
///
/// Body text only — the pager adds `Recap —` on render (manual and auto).
///
/// Keep in sync with the recap prompt eval harness (hillclimb there first).
/// Few-shots must stay synthetic — never embed real eval/session content.
pub(crate) fn recap_instruction(tag: &str) -> String {
    format!(
        "<{tag}>Write ONE sentence recap body for a user returning from idle. \
         Output ONLY the body (the UI adds the \"Recap —\" label). \
         Do NOT call any tools — respond with plain text only.\n\n\
         LANGUAGE: write the body in the language the user's own chat messages \
         are written in (ignore reminder-tagged turns like this one; user \
         instructions such as AGENTS.md may override). Keep code identifiers \
         verbatim.\n\n\
         Lead with agency:\n\
         - \"You asked …\" if the session was mainly questions, walkthroughs, or review with no landed change.\n\
         - \"We <past-tense verb> …\" if the agent implemented, fixed, merged, or changed code/config/docs \
         (e.g. \"We fixed …\", \"We merged …\", \"We wired …\" — not \"We did fix\" / \"We did merge\").\n\
         - If almost nothing happened: \"You had just begun this session.\"\n\n\
         Shape: <lead>: <concrete specifics — crate/file/flag/behavior/endpoint>. ~25–40 words.\n\n\
         Synthetic examples (style only — adapt to THIS session, do not copy):\n\n\
         You asked how retries work in the payment client: exponential backoff in `billing/retry.rs`, max 5 attempts, 429s only.\n\n\
         You asked for a walkthrough of the auth middleware change: warn-only mode in the API layer, no hard fail on missing claims.\n\n\
         We fixed the flaky integration test: race in `queue_worker` shutdown by awaiting the drain channel before exit.\n\n\
         We merged the feature branch: kept the new telemetry hooks, dropped the obsolete feature flag in `config/flags.toml`.\n\n\
         Bad (never):\n\
         - Start with Recap / Session recap / extra labels\n\
         - English recap for a non-English session\n\
         - Quote or restate this reminder or any system prompt\n\
         - Bullets, markdown, code fences, extra sentences\n\
         - Call tools or emit tool/function calls\n\
         - Invent work not reflected in the session</{tag}>"
    )
}

/// Prepare the conversation snapshot for a recap / turn-summary request
/// (same request shape, different instruction).
///
/// 1. Optionally strips reasoning/thinking blocks (`strip_reasoning`).
///    Cache-aligned side-calls pass `false` so the conversation prefix remains
///    byte-identical to the parent turn.
/// 2. Truncates a trailing incomplete assistant/tool-result run — a recap can
///    fire mid-turn, and the Anthropic Messages API rejects `tool_use` ids without a
///    matching `tool_result`.
/// 3. Appends the instruction as a final user turn.
pub(crate) fn build_instruction_items(
    conversation: Vec<ConversationItem>,
    instruction: String,
    strip_reasoning: bool,
) -> Vec<ConversationItem> {
    let mut items = if strip_reasoning {
        echo_agent_chat_state::compaction_utils::strip_reasoning_blocks(conversation)
    } else {
        conversation
    };

    pop_trailing_tool_run(&mut items);

    items.push(ConversationItem::user(instruction));
    items
}

/// Cap on the effective context window for recap budgeting: the verified
/// `max_prompt_length` for current `echo-agent-build` / `echo-agent-4.5` product backends
/// (`500000`). Applied via `min(window, CAP)`, so a smaller real window still
/// wins (e.g. a 256k legacy model or a debug override).
const RECAP_CONTEXT_WINDOW_CAP: u64 = 500_000;

/// Fraction of the (conservative) window a recap may occupy — the DEFAULT
/// auto-compact threshold. Fixed rather than the remote-settings-resolved value (which
/// can exceed 85), so recap stays at least as conservative as the turn path.
const RECAP_BUDGET_THRESHOLD_PERCENT: u64 = 85;

/// Estimator/serialization slack (mirrors memory-flush's soft-threshold pad). The
/// appended instruction is reserved SEPARATELY via `snapshot_budget`, so it is not
/// double-counted here. (`max_prompt_length` is input-length, so output doesn't count.)
const RECAP_BUDGET_HEADROOM_TOKENS: u64 = 4_000;

/// Budget-aware variant of [`build_instruction_items`]. Best-effort: returns a
/// structurally-valid, non-empty request trimmed to the estimated prompt budget
/// (the same bytes/4 estimator compaction triggers on) to prevent
/// `ic_400_prompt_too_long` on long sessions. Not an absolute guarantee — a
/// degenerate tiny window, an oversized retained `System` prefix, or estimator
/// optimism can still exceed the real limit (the 85% + headroom + 500k cap make
/// that unlikely for normal echo-agent-build sessions).
///
/// * Fast path — if the whole snapshot already fits, returns
///   `build_instruction_items(...)` verbatim (keeps the echoagent prefix KV cache
///   warm; honors the caller's `strip_reasoning`).
/// * Over budget — strip reasoning (the prefix cache is lost once we trim),
///   normalize the trailing boundary ([`pop_trailing_tool_run`]),
///   front-trim to fit via `fit_conversation_to_budget` (System kept, most-recent
///   turn truncated in place, never emptied), then append the instruction.
///
/// `context_window` MUST be the window of the model the recap is actually sent to
/// (today the session model).
pub(crate) fn budget_recap_items(
    conversation: Vec<ConversationItem>,
    tag: &str,
    strip_reasoning: bool,
    context_window: u64,
) -> Vec<ConversationItem> {
    budget_instruction_items(
        conversation,
        recap_instruction(tag),
        strip_reasoning,
        context_window,
    )
}

/// Instruction-generic core of [`budget_recap_items`], shared with the
/// turn-summary side-call.
pub(crate) fn budget_instruction_items(
    conversation: Vec<ConversationItem>,
    instruction: String,
    strip_reasoning: bool,
    context_window: u64,
) -> Vec<ConversationItem> {
    let effective_window = context_window.min(RECAP_CONTEXT_WINDOW_CAP);
    let prompt_budget = (effective_window.saturating_mul(RECAP_BUDGET_THRESHOLD_PERCENT) / 100)
        .saturating_sub(RECAP_BUDGET_HEADROOM_TOKENS);

    let instruction_item = ConversationItem::user(instruction.clone());
    let snapshot_budget = prompt_budget.saturating_sub(estimate_item_tokens(&instruction_item));

    // Un-stripped estimate is a safe upper bound (stripping only shrinks); the
    // verbatim path keeps the echoagent prefix cache warm.
    let pre_tokens = estimate_conversation_tokens(&conversation);
    if pre_tokens <= snapshot_budget {
        return build_instruction_items(conversation, instruction, strip_reasoning);
    }

    // Normalize the trailing boundary BEFORE trimming (ordering matters — see doc).
    let mut snapshot =
        compaction_utils::prepare_conversation_for_verbatim_summarization(conversation, true);
    pop_trailing_tool_run(&mut snapshot);
    let mut items = compaction_utils::fit_conversation_to_budget(snapshot, snapshot_budget);
    let post_tokens = estimate_conversation_tokens(&items);
    tracing::debug!(
        context_window,
        effective_window,
        prompt_budget,
        snapshot_budget,
        pre_tokens,
        post_tokens,
        "recap over budget: trimmed conversation to fit"
    );
    items.push(instruction_item);
    items
}

/// Pops a trailing tool run so the appended `User` instruction never follows a `tool_use`/`tool_result`.
/// Trailing `Reasoning` goes too: it precedes its owner, which the pop just removed.
/// Shared by [`build_instruction_items`] and [`budget_recap_items`].
pub(crate) fn pop_trailing_tool_run(items: &mut Vec<ConversationItem>) {
    while let Some(last) = items.last() {
        match last {
            ConversationItem::Assistant(a) if !a.tool_calls.is_empty() => {
                items.pop();
            }
            ConversationItem::ToolResult(_) | ConversationItem::Reasoning(_) => {
                items.pop();
            }
            _ => break,
        }
    }
}

/// Minimum main turns before an automatic return-from-away recap (manual exempt).
pub(crate) const MIN_TURNS_FOR_AUTO_RECAP: usize = 3;

/// Durable auto-recap watermark under `{session_dir}/`. Written only when a
/// recap commits (success or long-tail suppress), never on failure/cancel.
pub(crate) const RECAP_WATERMARK_FILE: &str = "last_recap_main_turn";

/// Real user prompts (`synthetic_reason.is_none()`), not assistant/tool items.
pub(crate) fn main_turn_count(conversation: &[ConversationItem]) -> usize {
    conversation
        .iter()
        .filter(|item| {
            matches!(
                item,
                ConversationItem::User(u) if u.synthetic_reason.is_none()
            )
        })
        .count()
}

/// Load the last committed recap main-turn watermark (`0` if missing/invalid).
pub(crate) fn load_recap_watermark(session_dir: &std::path::Path) -> usize {
    let path = session_dir.join(RECAP_WATERMARK_FILE);
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// Persist the watermark after a successful/suppress commit. Best-effort.
pub(crate) fn save_recap_watermark(session_dir: &std::path::Path, main_turns: usize) {
    if !session_dir.is_dir() {
        return;
    }
    let path = session_dir.join(RECAP_WATERMARK_FILE);
    if let Err(e) = std::fs::write(&path, main_turns.to_string()) {
        tracing::warn!(
            error = %e,
            path = %path.display(),
            "failed to persist recap watermark"
        );
    }
}

/// Manual: any `main_turns > 0`. Auto: new turn since `last`, min turns, idle.
pub(crate) fn recap_gate(
    main_turns: usize,
    last: usize,
    auto: bool,
    idle_ok: bool,
) -> Result<(), &'static str> {
    if main_turns == 0 {
        return Err("no main turns yet");
    }
    if auto {
        if main_turns <= last {
            return Err("no new main turn since last recap");
        }
        if main_turns < MIN_TURNS_FOR_AUTO_RECAP {
            return Err("fewer than min turns for auto recap");
        }
        if !idle_ok {
            return Err("idle threshold not met");
        }
    }
    Ok(())
}

/// Auto recaps longer than this (raw bytes) are saved but not shown.
pub(crate) const RECAP_AUTO_RAW_DISPLAY_MAX: usize = 500;

/// Auto only: long-tail output — persist artifact, do not display.
pub(crate) fn should_suppress_auto_recap_display(raw: &str, summary: &str) -> bool {
    if raw.len() > RECAP_AUTO_RAW_DISPLAY_MAX {
        return true;
    }
    summary.ends_with('\u{2026}') && summary.len() >= RECAP_MAX_CHARS
}

/// Clean the model's raw recap output into a readable one-liner body.
///
/// Normalizes whitespace, strips a stray leading label/quotes if the model
/// added one anyway, and caps length at [`RECAP_MAX_CHARS`] as a safety net
/// against runaway output (the cap is generous, so a normal recap is never
/// cut). Does not prepend `Recap —` — the pager always prefixes with that
/// label on render.
pub(crate) fn clean_recap_text(raw: &str) -> String {
    // Collapse runs of whitespace/newlines into single spaces (one scrollback line).
    let mut out: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");

    // Strip a stray leading label if the model added one anyway.
    for label in [
        "Recap —",
        "Recap—",
        "Recap -",
        "Recap:",
        "recap:",
        "Session recap:",
        "Summary:",
    ] {
        if let Some(rest) = out.strip_prefix(label) {
            out = rest.trim_start().to_string();
            break;
        }
    }

    // Strip symmetric wrapping quotes around the whole string.
    if out.len() >= 2 {
        let bytes = out.as_bytes();
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            out = out[1..out.len() - 1].trim().to_string();
        }
    }

    if out.len() > RECAP_MAX_CHARS {
        let cut = floor_char_boundary(&out, RECAP_MAX_CHARS);
        out.truncate(cut);
        out = out.trim_end().to_string();
        out.push('\u{2026}'); // …
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sampling::ConversationItem;

    #[test]
    fn clean_collapses_whitespace_and_newlines() {
        let raw = "Refactored   the\n\nparser\tand added   tests.";
        assert_eq!(
            clean_recap_text(raw),
            "Refactored the parser and added tests."
        );
    }

    #[test]
    fn clean_strips_leading_label() {
        assert_eq!(
            clean_recap_text("Recap: fixed the auth bug"),
            "fixed the auth bug"
        );
        assert_eq!(
            clean_recap_text("Session recap: wired up the API"),
            "wired up the API"
        );
    }

    #[test]
    fn clean_strips_wrapping_quotes() {
        assert_eq!(clean_recap_text("\"did the thing\""), "did the thing");
        assert_eq!(clean_recap_text("'did the thing'"), "did the thing");
    }

    #[test]
    fn clean_caps_length_on_char_boundary() {
        // Far past the cap → truncated on a char boundary with an ellipsis.
        let long = "word ".repeat(RECAP_MAX_CHARS);
        let out = clean_recap_text(&long);
        assert!(out.len() <= RECAP_MAX_CHARS + 4, "len was {}", out.len());
        assert!(out.ends_with('\u{2026}'));
    }

    #[test]
    fn clean_cap_is_utf8_safe() {
        // 3-byte chars straddling the byte cap must not panic.
        let big = "あ ".repeat(RECAP_MAX_CHARS);
        let out = clean_recap_text(&big);
        assert!(out.ends_with('\u{2026}'));
    }

    #[test]
    fn clean_keeps_normal_recap_in_full() {
        // A normal multi-sentence recap is well under the generous cap, so it is
        // returned verbatim — never cut mid-sentence.
        let recap = "We fixed the flaky integration test by awaiting the drain \
                     channel before exit, added a regression test for the shutdown \
                     path, and updated the runbook with the new sequence.";
        let out = clean_recap_text(recap);
        assert!(out.len() < RECAP_MAX_CHARS);
        assert!(!out.ends_with('\u{2026}'));
        assert!(out.ends_with("with the new sequence."));
    }

    #[test]
    fn build_appends_instruction_user_turn() {
        let conv = vec![
            ConversationItem::system("sys".to_string()),
            ConversationItem::user("hello".to_string()),
            ConversationItem::assistant("hi".to_string()),
        ];
        let items = build_instruction_items(conv, recap_instruction("system-reminder"), true);
        assert!(matches!(items.last(), Some(ConversationItem::User(_))));
        // System prompt prefix is preserved verbatim for cache reuse.
        assert!(matches!(items.first(), Some(ConversationItem::System(_))));
    }

    #[test]
    fn build_truncates_trailing_tool_result() {
        let conv = vec![
            ConversationItem::system("sys".to_string()),
            ConversationItem::user("hello".to_string()),
            ConversationItem::tool_result("call-1".to_string(), "output".to_string()),
        ];
        let items = build_instruction_items(conv, recap_instruction("system-reminder"), false);
        // The dangling ToolResult is dropped; only system + user + instruction remain.
        assert_eq!(items.len(), 3);
        assert!(matches!(items.last(), Some(ConversationItem::User(_))));
        assert!(
            !items
                .iter()
                .any(|i| matches!(i, ConversationItem::ToolResult(_)))
        );
    }

    #[test]
    fn main_turn_count_counts_real_users_only() {
        use echo_agent_sampling_types::{ContentPart, SyntheticReason, ToolCall, UserItem};
        use std::sync::Arc;

        let conv = vec![
            ConversationItem::system("sys".to_string()),
            ConversationItem::user("hi".to_string()),
            ConversationItem::assistant("hello".to_string()),
            ConversationItem::user("again".to_string()),
            ConversationItem::User(UserItem {
                content: vec![ContentPart::Text {
                    text: Arc::from("injected"),
                }],
                synthetic_reason: Some(SyntheticReason::SystemReminder),
                ..Default::default()
            }),
        ];
        assert_eq!(main_turn_count(&conv), 2);
        let empty: Vec<ConversationItem> = vec![ConversationItem::system("sys".to_string())];
        assert_eq!(main_turn_count(&empty), 0);

        let tool_loop = vec![
            ConversationItem::user("fix it".to_string()),
            ConversationItem::assistant_tool_calls(vec![ToolCall {
                id: Arc::from("c1"),
                name: "read_file".into(),
                arguments: Arc::from("{}"),
            }]),
            ConversationItem::tool_result("c1".to_string(), "ok".to_string()),
            ConversationItem::assistant("done".to_string()),
        ];
        assert_eq!(main_turn_count(&tool_loop), 1);
    }

    #[test]
    fn gate_allows_first_recap_when_last_is_zero() {
        assert!(recap_gate(1, 0, false, false).is_ok());
    }

    #[test]
    fn gate_manual_allows_re_recap_on_same_main_turn() {
        assert!(recap_gate(2, 2, false, true).is_ok());
        assert!(recap_gate(3, 3, false, false).is_ok());
    }

    #[test]
    fn gate_auto_denies_second_recap_on_same_main_turn() {
        assert_eq!(
            recap_gate(3, 3, true, true),
            Err("no new main turn since last recap")
        );
    }

    #[test]
    fn gate_allows_after_new_main_turn() {
        assert!(recap_gate(3, 2, false, false).is_ok());
        assert!(recap_gate(3, 2, true, true).is_ok());
    }

    #[test]
    fn gate_auto_requires_min_turns_and_idle() {
        assert_eq!(
            recap_gate(2, 0, true, true),
            Err("fewer than min turns for auto recap")
        );
        assert_eq!(recap_gate(3, 0, true, false), Err("idle threshold not met"));
        assert!(recap_gate(3, 0, true, true).is_ok());
    }

    #[test]
    fn gate_denies_zero_main_turns() {
        assert_eq!(recap_gate(0, 0, false, true), Err("no main turns yet"));
    }

    #[test]
    fn recap_watermark_missing_is_zero() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_recap_watermark(dir.path()), 0);
    }

    #[test]
    fn recap_watermark_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        save_recap_watermark(dir.path(), 7);
        assert_eq!(load_recap_watermark(dir.path()), 7);
        save_recap_watermark(dir.path(), 3);
        assert_eq!(load_recap_watermark(dir.path()), 3);
    }

    #[test]
    fn recap_watermark_invalid_or_empty_is_zero() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(RECAP_WATERMARK_FILE), "not-a-number").unwrap();
        assert_eq!(load_recap_watermark(dir.path()), 0);
        std::fs::write(dir.path().join(RECAP_WATERMARK_FILE), "  \n").unwrap();
        assert_eq!(load_recap_watermark(dir.path()), 0);
    }

    #[test]
    fn recap_watermark_save_skips_missing_dir() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("no-such-session");
        // Must not panic; no file created under a non-existent parent path.
        save_recap_watermark(&missing, 5);
        assert!(!missing.join(RECAP_WATERMARK_FILE).exists());
    }

    #[test]
    fn gate_allows_after_compaction_heal_watermark() {
        assert!(recap_gate(3, 2, false, false).is_ok());
        assert!(recap_gate(3, 3, false, false).is_ok());
        assert_eq!(
            recap_gate(3, 3, true, false),
            Err("no new main turn since last recap")
        );
    }

    #[test]
    fn suppress_auto_long_tail_raw_over_display_max() {
        let normal = "We fixed gitignored project Claude commands loading as slash skills.";
        assert!(!should_suppress_auto_recap_display(
            normal,
            &clean_recap_text(normal)
        ));
        let long_raw = "Creating the PR from the worktree. ".repeat(20);
        assert!(long_raw.len() > RECAP_AUTO_RAW_DISPLAY_MAX);
        assert!(should_suppress_auto_recap_display(
            &long_raw,
            &clean_recap_text(&long_raw)
        ));
    }

    #[test]
    fn suppress_auto_when_clean_hits_hard_cap_ellipsis() {
        let huge = "word ".repeat(RECAP_MAX_CHARS);
        let summary = clean_recap_text(&huge);
        assert!(summary.ends_with('\u{2026}'));
        assert!(should_suppress_auto_recap_display(&huge, &summary));
    }

    #[test]
    fn normal_auto_recap_not_suppressed() {
        let raw = "We fixed the flaky integration test: race in queue_worker shutdown.";
        assert!(raw.len() < RECAP_AUTO_RAW_DISPLAY_MAX);
        assert!(!should_suppress_auto_recap_display(
            raw,
            &clean_recap_text(raw)
        ));
    }

    #[test]
    fn instruction_uses_provided_tag() {
        assert!(recap_instruction("system_reminder").contains("<system_reminder>"));
        assert!(recap_instruction("system-reminder").contains("</system-reminder>"));
    }

    #[test]
    fn instruction_asks_for_one_sentence_body() {
        let text = recap_instruction("system-reminder");
        assert!(text.contains("Output ONLY the body"));
        assert!(text.contains("You asked"));
        assert!(text.contains("We fixed"));
        assert!(text.contains("We merged"));
        assert!(text.contains("billing/retry.rs"));
        assert!(text.contains("queue_worker"));
        assert!(text.contains("We fixed the flaky"));
        assert!(text.contains("We merged the feature"));
        assert!(!text.contains("217584"));
        assert!(!text.contains("lead with \"Recap"));
    }

    #[test]
    fn clean_returns_body_without_recap_prefix() {
        assert_eq!(
            clean_recap_text("Recap: You fixed auth in foo.rs."),
            "You fixed auth in foo.rs."
        );
        assert_eq!(
            clean_recap_text("You fixed auth in foo.rs."),
            "You fixed auth in foo.rs."
        );
    }

    // ---- budget_recap_items ------------------------------------------------

    /// Recompute the prompt budget the helper uses, for end-state assertions.
    fn recap_prompt_budget(context_window: u64) -> u64 {
        (context_window.min(RECAP_CONTEXT_WINDOW_CAP) * RECAP_BUDGET_THRESHOLD_PERCENT / 100)
            .saturating_sub(RECAP_BUDGET_HEADROOM_TOKENS)
    }

    fn mk_reasoning(id: &str) -> ConversationItem {
        use crate::sampling::rs;
        ConversationItem::Reasoning(rs::ReasoningItem {
            id: id.to_string(),
            summary: vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
                text: format!("secret thinking {id}"),
            })],
            content: None,
            encrypted_content: None,
            status: None,
        })
    }

    fn mk_tool_call(id: &str, args: &str) -> echo_agent_sampling_types::ToolCall {
        use std::sync::Arc;
        echo_agent_sampling_types::ToolCall {
            id: Arc::from(id),
            name: "read_file".into(),
            arguments: Arc::from(args),
        }
    }

    #[test]
    fn budget_fast_path_matches_build_instruction_items() {
        // Include a reasoning block so `strip_reasoning=true` actually exercises
        // stripping on the fits path (not just a no-op).
        let conv = vec![
            ConversationItem::system("sys"),
            ConversationItem::user("hello"),
            mk_reasoning("r1"),
            ConversationItem::assistant("hi"),
        ];
        let budgeted = budget_recap_items(conv.clone(), "system-reminder", true, 256_000);
        let built = build_instruction_items(conv, recap_instruction("system-reminder"), true);
        assert_eq!(
            serde_json::to_string(&budgeted).unwrap(),
            serde_json::to_string(&built).unwrap(),
            "under-budget snapshot must match build_instruction_items verbatim"
        );
        assert!(
            !budgeted
                .iter()
                .any(|i| matches!(i, ConversationItem::Reasoning(_))),
            "strip_reasoning=true must strip reasoning on the fast path too"
        );
        assert!(matches!(
            budgeted.first(),
            Some(ConversationItem::System(_))
        ));
        assert!(matches!(budgeted.last(), Some(ConversationItem::User(_))));
    }

    #[test]
    fn budget_over_budget_trims_within_budget() {
        let conv = vec![
            ConversationItem::system("sys"),
            ConversationItem::user("q1"),
            ConversationItem::assistant("a1"),
            ConversationItem::user("d".repeat(40_000)), // ~10k est tokens
            ConversationItem::assistant("a2"),
            ConversationItem::user("recent q"),
        ];
        let snapshot_plus_instruction = conv.len() + 1;
        let out = budget_recap_items(conv, "system-reminder", false, 8_000);
        assert!(
            estimate_conversation_tokens(&out) <= recap_prompt_budget(8_000),
            "trimmed recap must fit the prompt budget"
        );
        assert!(
            out.len() < snapshot_plus_instruction,
            "over-budget output must be strictly smaller than the untrimmed snapshot + instruction"
        );
        assert!(matches!(out.first(), Some(ConversationItem::System(_))));
        assert!(matches!(out.last(), Some(ConversationItem::User(_))));
    }

    #[test]
    fn budget_over_budget_drops_orphan_tool_result_at_front() {
        // The Assistant(tool_use) is heavy and gets excluded by the front-trim;
        // its ToolResult would then be a leading orphan — which must be dropped.
        let conv = vec![
            ConversationItem::system("sys"),
            ConversationItem::assistant_tool_calls(vec![mk_tool_call("c1", &"b".repeat(40_000))]),
            ConversationItem::tool_result("c1", "small result"),
            ConversationItem::user("recent"),
        ];
        let out = budget_recap_items(conv, "system-reminder", false, 8_000);
        let first_non_system = out
            .iter()
            .find(|i| !matches!(i, ConversationItem::System(_)));
        assert!(
            !matches!(first_non_system, Some(ConversationItem::ToolResult(_))),
            "retained tail must not begin with an orphan ToolResult"
        );
    }

    #[test]
    fn budget_over_budget_no_trailing_tool_run_and_keeps_recent_user() {
        // Regression guard locking the "normalize trailing boundary BEFORE
        // fit_conversation_to_budget" ordering. The trailing ToolResult is sized
        // LARGER than the budget on purpose: with the WRONG order (fit-then-pop),
        // `fit` sees the lone giant tool tail, truncates it in place, and drops
        // the most-recent real user turn — so assertion (a) fails. With the
        // correct order (pop-then-fit) the trailing tool run is removed first, so
        // the recent user turn is what survives the front-trim.
        let conv = vec![
            ConversationItem::system("sys"),
            ConversationItem::user("c".repeat(40_000)), // oldest real user, dropped
            ConversationItem::user("what changed in the parser?"), // most-recent real user
            ConversationItem::assistant_tool_calls(vec![mk_tool_call("c9", "{}")]),
            ConversationItem::tool_result("c9", "t".repeat(40_000)), // trailing run, > budget
        ];
        let out = budget_recap_items(conv, "system-reminder", false, 8_000);

        // (b) No trailing tool run before the appended instruction.
        assert!(matches!(out.last(), Some(ConversationItem::User(_))));
        let before = &out[out.len() - 2];
        assert!(
            !matches!(before, ConversationItem::ToolResult(_)),
            "no tool_result immediately before the appended instruction"
        );
        assert!(
            !matches!(before, ConversationItem::Assistant(a) if !a.tool_calls.is_empty()),
            "no dangling assistant tool_use immediately before the appended instruction"
        );
        // (a) The most-recent real user turn survives (FAILS under fit-then-pop,
        // which would instead keep a truncated lone tool tail).
        assert!(
            out.iter().any(|i| matches!(
                i,
                ConversationItem::User(u) if u.content.iter().any(|p| matches!(
                    p,
                    echo_agent_sampling_types::ContentPart::Text { text }
                        if text.contains("what changed in the parser?")
                ))
            )),
            "most-recent real user turn must survive the trim (locks normalize-before-fit)"
        );
    }

    #[test]
    fn budget_giant_single_turn_truncated_in_place() {
        // A single turn larger than the whole budget must be kept, truncated in
        // place — never dropped to an empty request.
        let conv = vec![ConversationItem::user("y".repeat(200_000))];
        let out = budget_recap_items(conv, "system-reminder", false, 8_000);
        assert!(
            out.len() >= 2,
            "giant turn must be truncated in place, not dropped"
        );
        assert!(matches!(out.last(), Some(ConversationItem::User(_))));
        assert!(estimate_conversation_tokens(&out) <= recap_prompt_budget(8_000));
        let serialized = serde_json::to_string(&out).unwrap();
        assert!(
            serialized.contains("truncated"),
            "retained turn must carry the in-place truncation marker"
        );
    }

    #[test]
    fn budget_over_budget_strips_reasoning_even_on_echo_agent() {
        let conv = vec![
            mk_reasoning("r1"),
            ConversationItem::assistant("did stuff"),
            ConversationItem::user("z".repeat(40_000)),
        ];
        // echoagent backend => strip_reasoning=false, but the over-budget branch must
        // strip reasoning anyway (the prefix cache is already lost once trimmed).
        let out = budget_recap_items(conv, "system-reminder", false, 8_000);
        assert!(
            !out.iter()
                .any(|i| matches!(i, ConversationItem::Reasoning(_))),
            "over-budget branch must strip reasoning even when strip_reasoning=false"
        );
    }

    #[test]
    fn budget_fast_path_keeps_reasoning_on_echo_agent() {
        let conv = vec![
            mk_reasoning("r1"),
            ConversationItem::assistant("did stuff"),
            ConversationItem::user("small"),
        ];
        // Fits under a large window on echoagent (strip_reasoning=false) => verbatim,
        // reasoning kept so the prefix KV cache stays warm.
        let out = budget_recap_items(conv, "system-reminder", false, 256_000);
        assert!(
            out.iter()
                .any(|i| matches!(i, ConversationItem::Reasoning(_))),
            "fits path on echoagent must keep reasoning verbatim"
        );
    }

    #[test]
    fn budget_1m_clamps_to_floor_and_256k_shrinks() {
        // One giant user turn larger than any window's budget: fit truncates it in
        // place to exactly snapshot_budget, so the output size is a direct readout
        // of the budget the helper used.
        let giant = || {
            vec![
                ConversationItem::system("sys"),
                ConversationItem::user("x".repeat(2_400_000)),
            ]
        };
        let out_500 = budget_recap_items(giant(), "system-reminder", false, 500_000);
        let out_1m = budget_recap_items(giant(), "system-reminder", false, 1_000_000);
        let out_256 = budget_recap_items(giant(), "system-reminder", false, 256_000);

        // 1M advertises a larger window but clamps to the 500k floor => identical.
        assert_eq!(
            estimate_conversation_tokens(&out_1m),
            estimate_conversation_tokens(&out_500),
            "1M must clamp to the 500k floor (identical budget)"
        );
        assert!(estimate_conversation_tokens(&out_500) <= recap_prompt_budget(500_000));
        // 256k is below the floor => strictly smaller budget and output.
        assert!(
            estimate_conversation_tokens(&out_256) < estimate_conversation_tokens(&out_500),
            "256k must produce a smaller budget than the 500k floor"
        );
        assert!(estimate_conversation_tokens(&out_256) <= recap_prompt_budget(256_000));
    }

    #[test]
    fn pop_trailing_removes_tool_run_keeps_clean_tail() {
        let mut items = vec![
            ConversationItem::user("hi"),
            ConversationItem::assistant_tool_calls(vec![mk_tool_call("c1", "{}")]),
            ConversationItem::tool_result("c1", "out"),
        ];
        pop_trailing_tool_run(&mut items);
        assert_eq!(items.len(), 1);
        assert!(matches!(items[0], ConversationItem::User(_)));

        let mut clean = vec![
            ConversationItem::user("hi"),
            ConversationItem::assistant("done"),
        ];
        pop_trailing_tool_run(&mut clean);
        assert_eq!(clean.len(), 2, "a clean (non-tool) tail is left untouched");
    }

    #[test]
    fn budget_empty_conversation_returns_only_instruction() {
        // The helper is directly reachable (the handler gates `vec![]` upstream);
        // an empty snapshot must return just the appended instruction, no panic.
        let out = budget_recap_items(Vec::new(), "system-reminder", false, 256_000);
        assert_eq!(out.len(), 1, "empty input yields only the instruction turn");
        assert!(matches!(out.last(), Some(ConversationItem::User(_))));
    }

    #[test]
    fn budget_threshold_boundary_selects_fast_vs_over_budget() {
        // Lock the `<=` fits-vs-over-budget comparison. At exactly snapshot_budget
        // the fast path is taken (reasoning kept, `strip_reasoning=false`); one
        // token over takes the over-budget path (reasoning stripped). The
        // reasoning item's presence is the observable branch discriminator.
        let tag = "system-reminder";
        let instruction_tokens =
            estimate_item_tokens(&ConversationItem::user(recap_instruction(tag)));
        let snapshot_budget = recap_prompt_budget(8_000).saturating_sub(instruction_tokens);
        assert!(snapshot_budget > 8, "window must leave room for the probe");

        let reasoning_tokens = estimate_item_tokens(&mk_reasoning("r"));
        let filler_tokens = snapshot_budget - reasoning_tokens;

        // Exactly at budget => fast path (`<=`) keeps reasoning verbatim.
        let at = vec![
            mk_reasoning("r"),
            ConversationItem::user("a".repeat((filler_tokens * 4) as usize)),
        ];
        assert_eq!(estimate_conversation_tokens(&at), snapshot_budget);
        let out_at = budget_recap_items(at, tag, false, 8_000);
        assert!(
            out_at
                .iter()
                .any(|i| matches!(i, ConversationItem::Reasoning(_))),
            "exactly-at-budget must take the fast path (reasoning kept), locking `<=`"
        );

        // One token over => over-budget path strips reasoning.
        let over = vec![
            mk_reasoning("r"),
            ConversationItem::user("a".repeat((filler_tokens * 4 + 4) as usize)),
        ];
        assert!(estimate_conversation_tokens(&over) > snapshot_budget);
        let out_over = budget_recap_items(over, tag, false, 8_000);
        assert!(
            !out_over
                .iter()
                .any(|i| matches!(i, ConversationItem::Reasoning(_))),
            "one token over budget must take the over-budget path (reasoning stripped)"
        );
    }

    #[test]
    fn budget_degenerate_tiny_window_stays_valid_and_nonempty() {
        // Window below the headroom => prompt_budget saturates to 0. The
        // instruction is still appended, so the output necessarily exceeds the
        // computed 0 budget but stays tiny and structurally valid (cannot cause a
        // 400). Asserts graceful degradation — NOT `est <= budget` (documents the
        // informational degenerate-window behavior).
        let conv = vec![
            ConversationItem::system("sys"),
            ConversationItem::user("w".repeat(40_000)),
        ];
        let out = budget_recap_items(conv, "system-reminder", false, 1_000);
        assert!(
            !out.is_empty(),
            "a degenerate tiny window must still return a valid, non-empty request"
        );
        assert!(matches!(out.last(), Some(ConversationItem::User(_))));
    }
}
