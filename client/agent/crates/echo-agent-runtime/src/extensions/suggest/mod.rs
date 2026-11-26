mod ai_provider;
mod file_provider;
mod history_provider;
mod path_provider;
mod shell_token;

use agent_client_protocol as acp;
use serde::{Deserialize, Serialize};

use super::{ExtResult, parse_params, to_raw_response};
use crate::agent::MvpAgent;

pub(crate) use file_provider::FilePathProvider;
pub(crate) use history_provider::HistoryProvider;
pub(crate) use path_provider::PathProvider;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SuggestRequest {
    text: String,
    cursor: usize,
    cwd: String,
    limit: usize,
    generation: u64,
    #[serde(default)]
    include_ai: bool,
    #[serde(default)]
    ai_model: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    /// Deterministic Tab mode: run only the token providers (path/file).
    /// A history/AI row would make the set mixed — killing the pager's
    /// insta-accept/LCP semantics — and reparse history per keystroke.
    #[serde(default)]
    token_only: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SuggestResponse {
    ghost: Option<GhostSuggestion>,
    completions: Vec<CompletionItem>,
    generation: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GhostSuggestion {
    full_text: String,
    suffix: String,
    source: String,
}

/// One completion row. Wire-compat contract (leader mode and the cloud
/// bridge mix shell/pager versions):
/// - `insert_text` is ALWAYS a safe whole-line replacement — range-unaware
///   pagers `set_text` it, so it must never be a bare token.
/// - `replace_range` + `token_text` are the additive token-in-place upgrade:
///   byte offsets `[start, end)` into the request `text` and the text that
///   replaces that span. Range-aware pagers use them as an ATOMIC pair —
///   a range without `token_text` (history/AI whole-line rows, where
///   `insert_text` doubles as the span replacement) degrades to the
///   equivalent whole-line accept.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CompletionItem {
    display: String,
    description: String,
    insert_text: String,
    source: String,
    priority: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    replace_range: Option<(usize, usize)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    token_text: Option<String>,
    /// The provider capped its scan/result set: the row set may be
    /// incomplete, so range-aware pagers keep dropdown-only semantics
    /// (absent = `false` for older shells).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    truncated: bool,
}

pub(crate) struct SuggestContext {
    text: String,
    cursor: usize,
    cwd: String,
}

impl SuggestContext {
    fn new(text: String, cursor: usize, cwd: String) -> Self {
        let mut cursor = cursor.min(text.len());
        while cursor > 0 && !text.is_char_boundary(cursor) {
            cursor -= 1;
        }
        Self { text, cursor, cwd }
    }

    pub(crate) fn prefix(&self) -> &str {
        &self.text[..self.cursor]
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RankedSuggestion {
    pub(crate) display: String,
    pub(crate) description: String,
    /// Whole-line replacement (see the [`CompletionItem`] compat contract).
    pub(crate) insert_text: String,
    pub(crate) source: SuggestionSource,
    pub(crate) priority: i32,
    pub(crate) is_ghost_candidate: bool,
    /// Request-text byte range the completion targets (token for path/file,
    /// whole line for history/AI); `None` keeps whole-line-only semantics.
    pub(crate) replace_range: Option<(usize, usize)>,
    /// Replacement for `replace_range` when it differs from `insert_text`.
    pub(crate) token_text: Option<String>,
    /// Provider capped its scan/results — a hidden row could disprove a
    /// sole match or an LCP.
    pub(crate) truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SuggestionSource {
    History,
    Path,
    File,
    AI,
}

impl SuggestionSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::History => "history",
            Self::Path => "path",
            Self::File => "file",
            Self::AI => "ai",
        }
    }
}

impl From<RankedSuggestion> for CompletionItem {
    fn from(s: RankedSuggestion) -> Self {
        Self {
            display: s.display,
            description: s.description,
            insert_text: s.insert_text,
            source: s.source.as_str().to_owned(),
            priority: s.priority,
            replace_range: s.replace_range,
            token_text: s.token_text,
            truncated: s.truncated,
        }
    }
}

/// Mark whole-line suggestions (history/AI carry the full command as
/// `insert_text`) as replacing the entire request text.
fn stamp_whole_line_range(results: &mut [RankedSuggestion], text_len: usize) {
    results
        .iter_mut()
        .for_each(|s| s.replace_range = Some((0, text_len)));
}

/// Convert token-valued suggestions (path/file build `insert_text` as the
/// token replacing `range`) into the wire pair: the token moves to
/// `token_text` and `insert_text` becomes the full line with the token
/// spliced in — the shape range-unaware pagers can safely `set_text`.
fn splice_token_into_line(results: &mut [RankedSuggestion], text: &str, range: (usize, usize)) {
    for s in results {
        let token = std::mem::take(&mut s.insert_text);
        s.insert_text = format!("{}{}{}", &text[..range.0], token, &text[range.1..]);
        s.token_text = Some(token);
    }
}

#[tracing::instrument(skip_all, fields(method = %args.method))]
pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        "echo.agent/suggest" => handle_suggest(agent, args).await,
        "echo.agent/suggestPrompt" => handle_suggest_prompt(agent, args).await,
        _ => Err(acp::Error::method_not_found()),
    }
}

/// Request/response for `echo.agent/suggestPrompt` — predict the user's likely next
/// prompt after a completed turn (tab-autocomplete ghost text).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SuggestPromptRequest {
    /// Client-side generation counter, echoed back so stale responses can be
    /// discarded (the client may have started a newer turn meanwhile).
    generation: u64,
    #[serde(default)]
    session_id: Option<String>,
    /// Client hint for the suggestion model (the pager sends its env
    /// override, or `echo-agent-build-0.1` when its catalog offers it). One tier
    /// of the shell-side resolution in
    /// `prompt_suggest::effective_suggest_model`: env > config.toml > remote
    /// > this hint > `echo-agent-build-0.1` default, catalog-guarded (a
    /// non-sampleable effective model skips the request; the session model
    /// is never used).
    #[serde(default)]
    model: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SuggestPromptResponse {
    suggestion: Option<String>,
    generation: u64,
}

/// Upper bound on the suggestion round-trip. Turn-end prediction is not
/// latency-critical (the user is reading the agent's reply — the idle window
/// after a turn is typically long), but a hung call must not pin the oneshot
/// forever. Reasoning models (e.g. `echo-agent-build`) can take ~30s on a cold
/// cache; a late suggestion is still useful (the pager's generation guard
/// and empty-prompt gating discard it if the user moved on).
const SUGGEST_PROMPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);

async fn handle_suggest_prompt(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let req: SuggestPromptRequest = parse_params(args)?;
    let generation = req.generation;

    let suggestion = match find_session(agent, req.session_id.as_deref()) {
        Some(handle) => {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let cmd = crate::session::commands::SessionCommand::SuggestPrompt {
                model_override: req.model,
                respond_to: tx,
            };
            if handle.cmd_tx.send(cmd).is_err() {
                tracing::debug!("suggestPrompt: session command channel closed");
                None
            } else {
                match tokio::time::timeout(SUGGEST_PROMPT_TIMEOUT, rx).await {
                    Ok(Ok(suggestion)) => suggestion,
                    Ok(Err(_)) => {
                        tracing::debug!("suggestPrompt: responder dropped");
                        None
                    }
                    Err(_) => {
                        tracing::debug!("suggestPrompt: timed out");
                        None
                    }
                }
            }
        }
        None => {
            tracing::debug!(session_id = ?req.session_id, "suggestPrompt: session not found");
            None
        }
    };

    to_raw_response(&SuggestPromptResponse {
        suggestion,
        generation,
    })
}

async fn handle_suggest(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    let req: SuggestRequest = parse_params(args)?;
    let limit = req.limit;
    let generation = req.generation;
    let include_ai = req.include_ai;
    let token_only = req.token_only;
    let SuggestRequest {
        text,
        cursor,
        cwd,
        ai_model,
        session_id,
        ..
    } = req;
    let ctx = SuggestContext::new(text, cursor, cwd);

    let (history_results, path_results, file_results) = tokio::join!(
        async {
            if token_only {
                Vec::new()
            } else {
                HistoryProvider.suggest(&ctx).await
            }
        },
        PathProvider.suggest(&ctx),
        FilePathProvider.suggest(&ctx),
    );

    let mut ai_results =
        if include_ai && !token_only && !should_skip_ai(&history_results, ctx.prefix()) {
            let session = find_session(agent, session_id.as_deref());
            match session {
                Some(handle) => {
                    ai_provider::suggest(&handle.cmd_tx, ctx.prefix(), &ctx.cwd, ai_model).await
                }
                None => Vec::new(),
            }
        } else {
            Vec::new()
        };
    stamp_whole_line_range(&mut ai_results, ctx.text.len());

    let (ghost, completions) = aggregate(
        history_results,
        path_results,
        file_results,
        ai_results,
        ctx.prefix(),
        limit,
    );

    to_raw_response(&SuggestResponse {
        ghost,
        completions,
        generation,
    })
}

fn find_session(
    agent: &MvpAgent,
    session_id: Option<&str>,
) -> Option<crate::session::handle::SessionHandle> {
    if let Some(id) = session_id {
        agent.resident_handle(&acp::SessionId::new(id))
    } else {
        agent
            .resident_ids()
            .into_iter()
            .next()
            .and_then(|id| agent.resident_handle(&id))
    }
}

fn aggregate(
    history: Vec<RankedSuggestion>,
    path: Vec<RankedSuggestion>,
    file: Vec<RankedSuggestion>,
    ai: Vec<RankedSuggestion>,
    prefix: &str,
    limit: usize,
) -> (Option<GhostSuggestion>, Vec<CompletionItem>) {
    let mut all: Vec<RankedSuggestion> = history
        .into_iter()
        .chain(path)
        .chain(file)
        .chain(ai)
        .collect();
    // STABLE sort — load-bearing: providers pre-rank their items and ship
    // them at one shared priority per response (the file provider's fuzzy
    // tier/score/dirs-first order — see its `FILE_CMD_BOOST` doc), relying
    // on equal-priority order surviving to the wire. Do not "optimize"
    // into `sort_unstable_by`.
    all.sort_by(|a, b| b.priority.cmp(&a.priority));

    let ghost = all.iter().find(|s| s.is_ghost_candidate).map(|s| {
        let suffix = s.insert_text.strip_prefix(prefix).unwrap_or(&s.insert_text);
        GhostSuggestion {
            full_text: s.insert_text.clone(),
            suffix: suffix.to_owned(),
            source: s.source.as_str().to_owned(),
        }
    });

    let completions = all
        .into_iter()
        .take(limit)
        .map(CompletionItem::from)
        .collect();

    (ghost, completions)
}

/// Determines whether AI suggestions can be skipped based on history quality.
pub(crate) fn should_skip_ai(history_matches: &[RankedSuggestion], prefix: &str) -> bool {
    if history_matches.is_empty() {
        return false;
    }
    if history_matches[0].priority >= 30 {
        return true;
    }
    !prefix.is_empty() && history_matches.len() >= 3
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranked(
        priority: i32,
        source: SuggestionSource,
        ghost: bool,
        text: &str,
    ) -> RankedSuggestion {
        RankedSuggestion {
            display: text.into(),
            description: String::new(),
            insert_text: text.into(),
            source,
            priority,
            is_ghost_candidate: ghost,
            replace_range: None,
            token_text: None,
            truncated: false,
        }
    }

    // --- aggregate ---

    #[test]
    fn aggregate_sorts_by_descending_priority() {
        let history = vec![
            ranked(10, SuggestionSource::History, true, "git commit"),
            ranked(5, SuggestionSource::History, false, "git checkout"),
        ];
        let path = vec![ranked(0, SuggestionSource::Path, false, "git")];
        let (_, completions) = aggregate(history, path, vec![], vec![], "git", 10);
        assert_eq!(completions[0].priority, 10);
        assert_eq!(completions[1].priority, 5);
        assert_eq!(completions[2].priority, 0);
    }

    #[test]
    fn aggregate_selects_ghost_with_correct_suffix() {
        let history = vec![ranked(10, SuggestionSource::History, true, "git commit")];
        let path = vec![ranked(0, SuggestionSource::Path, false, "git")];
        let (ghost, _) = aggregate(history, path, vec![], vec![], "git", 10);
        let ghost = ghost.unwrap();
        assert_eq!(ghost.full_text, "git commit");
        assert_eq!(ghost.suffix, " commit");
        assert_eq!(ghost.source, "history");
    }

    #[test]
    fn aggregate_no_ghost_without_candidate() {
        let path = vec![
            ranked(0, SuggestionSource::Path, false, "git"),
            ranked(0, SuggestionSource::Path, false, "grep"),
        ];
        let (ghost, _) = aggregate(vec![], path, vec![], vec![], "g", 10);
        assert!(ghost.is_none());
    }

    #[test]
    fn aggregate_respects_limit() {
        let history: Vec<_> = (0..10)
            .map(|i| {
                ranked(
                    10 - i,
                    SuggestionSource::History,
                    i == 0,
                    &format!("cmd_{i}"),
                )
            })
            .collect();
        let (_, completions) = aggregate(history, vec![], vec![], vec![], "cmd", 3);
        assert_eq!(completions.len(), 3);
    }

    #[test]
    fn aggregate_ghost_suffix_for_exact_prefix() {
        let history = vec![ranked(40, SuggestionSource::History, true, "ls")];
        let (ghost, _) = aggregate(history, vec![], vec![], vec![], "ls", 10);
        let ghost = ghost.unwrap();
        assert_eq!(ghost.suffix, "");
    }

    #[test]
    fn aggregate_includes_ai_results() {
        let history = vec![ranked(10, SuggestionSource::History, true, "git commit")];
        let ai = vec![ranked(
            -10,
            SuggestionSource::AI,
            true,
            "git commit --amend",
        )];
        let (_, completions) = aggregate(history, vec![], vec![], ai, "git", 10);
        assert_eq!(completions.len(), 2);
        assert_eq!(completions[0].source, "history");
        assert_eq!(completions[1].source, "ai");
        assert_eq!(completions[1].priority, -10);
    }

    #[test]
    fn aggregate_ai_ghost_used_when_no_history_ghost() {
        let ai = vec![ranked(
            -10,
            SuggestionSource::AI,
            true,
            "git commit --amend",
        )];
        let (ghost, _) = aggregate(vec![], vec![], vec![], ai, "git", 10);
        let ghost = ghost.unwrap();
        assert_eq!(ghost.source, "ai");
        assert_eq!(ghost.suffix, " commit --amend");
    }

    // --- should_skip_ai ---

    #[test]
    fn skip_ai_returns_false_for_empty_history() {
        assert!(!should_skip_ai(&[], "git"));
    }

    #[test]
    fn skip_ai_on_exact_match() {
        let m = vec![ranked(40, SuggestionSource::History, true, "git commit")];
        assert!(should_skip_ai(&m, "git commit"));
    }

    #[test]
    fn skip_ai_prefix_with_enough_matches() {
        let m = vec![
            ranked(10, SuggestionSource::History, true, "git commit"),
            ranked(9, SuggestionSource::History, false, "git checkout"),
            ranked(8, SuggestionSource::History, false, "git cherry-pick"),
        ];
        assert!(should_skip_ai(&m, "git"));
    }

    #[test]
    fn dont_skip_ai_prefix_with_few_matches() {
        let m = vec![ranked(5, SuggestionSource::History, true, "git commit")];
        assert!(!should_skip_ai(&m, "git"));
    }

    #[test]
    fn skip_ai_many_matches_empty_prefix() {
        let m = vec![
            ranked(5, SuggestionSource::History, true, "a"),
            ranked(4, SuggestionSource::History, false, "b"),
            ranked(3, SuggestionSource::History, false, "c"),
        ];
        // empty prefix + 3 matches: !prefix.is_empty() is false, len >= 3 is true → false AND true → false
        assert!(!should_skip_ai(&m, ""));
    }

    #[test]
    fn dont_skip_ai_few_matches_empty_prefix() {
        let m = vec![
            ranked(5, SuggestionSource::History, true, "a"),
            ranked(4, SuggestionSource::History, false, "b"),
        ];
        assert!(!should_skip_ai(&m, ""));
    }

    // --- context ---

    #[test]
    fn context_clamps_cursor_to_len() {
        let ctx = SuggestContext::new("abc".into(), 100, "/tmp".into());
        assert_eq!(ctx.cursor, 3);
        assert_eq!(ctx.prefix(), "abc");
    }

    #[test]
    fn context_adjusts_to_char_boundary() {
        let text = "caf\u{00e9}"; // "cafe" with e-acute (2 bytes for e-acute)
        assert_eq!(text.len(), 5);
        let ctx = SuggestContext::new(text.into(), 4, "/tmp".into()); // middle of 2-byte e-acute
        assert_eq!(ctx.prefix(), "caf");
    }

    #[test]
    fn completion_item_serializes_replace_range_and_token_as_camel_case() {
        let mut s = ranked(10, SuggestionSource::Path, false, "ls | grep");
        s.replace_range = Some((5, 7));
        s.token_text = Some("grep".into());
        let json = serde_json::to_value(CompletionItem::from(s)).unwrap();
        assert_eq!(json["replaceRange"], serde_json::json!([5, 7]));
        assert_eq!(json["tokenText"], "grep");
        // Whole-line compat field for range-unaware pagers.
        assert_eq!(json["insertText"], "ls | grep");
    }

    #[test]
    fn completion_item_omits_absent_replace_range_and_token() {
        let json = serde_json::to_value(CompletionItem::from(ranked(
            0,
            SuggestionSource::Path,
            false,
            "grep",
        )))
        .unwrap();
        assert!(json.get("replaceRange").is_none());
        assert!(json.get("tokenText").is_none());
        // Additive: `truncated` only serializes when set.
        assert!(json.get("truncated").is_none());
    }

    #[test]
    fn completion_item_serializes_truncated_when_set() {
        let mut s = ranked(0, SuggestionSource::File, false, "notes.md");
        s.truncated = true;
        let json = serde_json::to_value(CompletionItem::from(s)).unwrap();
        assert_eq!(json["truncated"], true);
    }

    #[test]
    fn stamp_whole_line_range_covers_full_text() {
        let mut results = vec![
            ranked(10, SuggestionSource::History, true, "git commit"),
            ranked(9, SuggestionSource::History, false, "git checkout"),
        ];
        stamp_whole_line_range(&mut results, 5);
        assert!(results.iter().all(|s| s.replace_range == Some((0, 5))));
        // Whole-line items double as their own span replacement.
        assert!(results.iter().all(|s| s.token_text.is_none()));
    }

    /// Token-valued suggestions become the wire pair: token in `token_text`,
    /// `insert_text` rebuilt as the full line (safe for old pagers).
    #[test]
    fn splice_token_into_line_builds_compat_pair() {
        let mut results = vec![ranked(0, SuggestionSource::Path, false, "grep")];
        splice_token_into_line(&mut results, "ls | gr | wc -l", (5, 7));
        assert_eq!(results[0].insert_text, "ls | grep | wc -l");
        assert_eq!(results[0].token_text.as_deref(), Some("grep"));
    }

    /// Equal-priority items must keep their provider-internal order: the
    /// file provider ships pre-ranked rows (fuzzy tier/score/dirs-first) at
    /// ONE shared priority and its ranking reaches the wire only through
    /// this sort's stability. Deliberately non-alphabetical, larger than
    /// the small-slice insertion-sort threshold, and interleaved with a
    /// second priority class so a `sort_unstable_by` swap turns this red.
    #[test]
    fn aggregate_preserves_provider_order_within_equal_priority() {
        let file: Vec<_> = (0..32)
            .map(|i| {
                ranked(
                    2,
                    SuggestionSource::File,
                    false,
                    &format!("ranked_{:02}", 31 - i),
                )
            })
            .collect();
        let expected: Vec<String> = file.iter().map(|s| s.display.clone()).collect();
        let path: Vec<_> = (0..32)
            .map(|i| ranked(0, SuggestionSource::Path, false, &format!("exe_{i:02}")))
            .collect();

        let (_, completions) = aggregate(vec![], path, file, vec![], "r", 100);
        let file_order: Vec<String> = completions
            .iter()
            .filter(|c| c.source == "file")
            .map(|c| c.display.clone())
            .collect();
        assert_eq!(file_order, expected);
        // The boosted file rows all sort ahead of the priority-0 path rows.
        assert_eq!(
            completions[..32]
                .iter()
                .filter(|c| c.source == "file")
                .count(),
            32
        );
    }

    #[test]
    fn completion_item_preserves_source_string() {
        let item = CompletionItem::from(ranked(10, SuggestionSource::History, true, "cmd"));
        assert_eq!(item.source, "history");
        assert_eq!(item.priority, 10);

        let item = CompletionItem::from(ranked(0, SuggestionSource::Path, false, "cmd"));
        assert_eq!(item.source, "path");

        let item = CompletionItem::from(ranked(5, SuggestionSource::File, false, "cmd"));
        assert_eq!(item.source, "file");
    }

    #[test]
    fn aggregate_includes_file_results() {
        let history = vec![ranked(10, SuggestionSource::History, true, "cat ~/.bashrc")];
        let file = vec![ranked(5, SuggestionSource::File, false, ".bashrc")];
        let (_, completions) = aggregate(history, vec![], file, vec![], "cat", 10);
        assert_eq!(completions.len(), 2);
        assert_eq!(completions[0].priority, 10);
        assert_eq!(completions[1].priority, 5);
        assert_eq!(completions[1].source, "file");
    }
}
