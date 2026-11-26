use tokio::sync::{mpsc, oneshot};
use tokio::time::Duration;

use super::{RankedSuggestion, SuggestionSource};
use crate::session::commands::SessionCommand;

const AI_TIMEOUT: Duration = Duration::from_secs(2);
const AI_PRIORITY: i32 = -10;

/// Request AI-powered shell command suggestions via the session actor.
///
/// Sends `SessionCommand::AISuggest` and awaits the response with a 2-second
/// timeout. Returns at most one `RankedSuggestion` with `source: AI` and
/// `priority: -10` (below history/path results).
pub(crate) async fn suggest(
    cmd_tx: &mpsc::UnboundedSender<SessionCommand>,
    prefix: &str,
    cwd: &str,
    model_override: Option<String>,
) -> Vec<RankedSuggestion> {
    if prefix.is_empty() {
        return Vec::new();
    }

    let (tx, rx) = oneshot::channel();
    let cmd = SessionCommand::AISuggest {
        prefix: prefix.to_owned(),
        cwd: cwd.to_owned(),
        model_override,
        respond_to: tx,
    };

    if cmd_tx.send(cmd).is_err() {
        return Vec::new();
    }

    let result = match tokio::time::timeout(AI_TIMEOUT, rx).await {
        Ok(Ok(Some(text))) => text,
        _ => return Vec::new(),
    };

    build_suggestion(prefix, &result)
}

fn build_suggestion(prefix: &str, raw: &str) -> Vec<RankedSuggestion> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == prefix {
        return Vec::new();
    }

    // If the model returned the full command (including prefix), use it as-is.
    // Otherwise concatenate directly — the model output may start with a space
    // or continuation that should be appended verbatim after the prefix.
    let insert_text = if trimmed.starts_with(prefix) {
        trimmed.to_owned()
    } else if raw.starts_with(prefix) {
        raw.trim_end().to_owned()
    } else {
        format!("{prefix}{raw}").trim_end().to_owned()
    };

    vec![RankedSuggestion {
        display: insert_text.clone(),
        description: String::new(),
        insert_text,
        source: SuggestionSource::AI,
        priority: AI_PRIORITY,
        // Whole-line; `handle_suggest` stamps the range (no full text here).
        replace_range: None,
        token_text: None,
        truncated: false,
        is_ghost_candidate: true,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_suggestion_with_prefix_continuation() {
        let result = build_suggestion("git", "git commit --amend");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].insert_text, "git commit --amend");
        assert_eq!(result[0].source, SuggestionSource::AI);
        assert_eq!(result[0].priority, -10);
        assert!(result[0].is_ghost_candidate);
    }

    #[test]
    fn build_suggestion_prepends_prefix_when_missing() {
        let result = build_suggestion("git", " commit --amend");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].insert_text, "git commit --amend");
    }

    #[test]
    fn build_suggestion_exact_match_returns_empty() {
        assert!(build_suggestion("git", "git").is_empty());
    }

    #[test]
    fn build_suggestion_whitespace_only_returns_empty() {
        assert!(build_suggestion("git", "   \n  ").is_empty());
    }

    #[test]
    fn build_suggestion_empty_returns_empty() {
        assert!(build_suggestion("git", "").is_empty());
    }

    #[test]
    fn build_suggestion_no_separator_concatenates_directly() {
        // Model returned a continuation without leading space — result has no separator.
        // This is expected: the model should include the space if one is needed.
        let result = build_suggestion("git", "commit");
        assert_eq!(result[0].insert_text, "gitcommit");
    }

    #[test]
    fn build_suggestion_raw_starts_with_prefix_preserves_internal_whitespace() {
        let result = build_suggestion("git", "git  commit  \n");
        assert_eq!(result[0].insert_text, "git  commit");
    }

    #[test]
    fn build_suggestion_trims_surrounding_whitespace() {
        let result = build_suggestion("git", "  git commit  \n");
        assert_eq!(result[0].insert_text, "git commit");
    }

    #[tokio::test]
    async fn empty_prefix_skips_channel() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let result = suggest(&tx, "", "/tmp", None).await;
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn closed_channel_returns_empty() {
        let (tx, rx) = mpsc::unbounded_channel();
        drop(rx);
        let result = suggest(&tx, "git", "/tmp", None).await;
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn successful_response() {
        let (tx, mut rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            if let Some(SessionCommand::AISuggest { respond_to, .. }) = rx.recv().await {
                let _ = respond_to.send(Some("git commit --amend".into()));
            }
        });

        let result = suggest(&tx, "git", "/tmp", None).await;
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].insert_text, "git commit --amend");
    }

    #[tokio::test]
    async fn none_response_returns_empty() {
        let (tx, mut rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            if let Some(SessionCommand::AISuggest { respond_to, .. }) = rx.recv().await {
                let _ = respond_to.send(None);
            }
        });

        let result = suggest(&tx, "git", "/tmp", None).await;
        assert!(result.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn slow_responder_times_out() {
        let (tx, mut rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            if let Some(SessionCommand::AISuggest { respond_to, .. }) = rx.recv().await {
                // Respond well after the 2-second timeout.
                tokio::time::sleep(Duration::from_secs(10)).await;
                let _ = respond_to.send(Some("git commit --amend".into()));
            }
        });

        let result = suggest(&tx, "git", "/tmp", None).await;
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn sends_correct_fields_to_session() {
        let (tx, mut rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            if let Some(SessionCommand::AISuggest {
                prefix,
                cwd,
                model_override,
                respond_to,
            }) = rx.recv().await
            {
                assert_eq!(prefix, "docker");
                assert_eq!(cwd, "/home/user");
                assert_eq!(model_override.as_deref(), Some("custom-model"));
                let _ = respond_to.send(Some("docker compose up".into()));
            }
        });

        let result = suggest(&tx, "docker", "/home/user", Some("custom-model".into())).await;
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].insert_text, "docker compose up");
    }
}
