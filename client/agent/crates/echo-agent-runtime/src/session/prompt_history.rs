//! Per-working-directory prompt history for fast reverse search.
//!
//! Stores prompts in a separate JSONL file per CWD for instant loading,
//! independent of session storage. Each file is capped at 10,000 entries.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;

const MAX_PROMPT_HISTORY_ENTRIES: usize = 10_000;
const PROMPT_HISTORY_FILE: &str = "prompt_history.jsonl";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptEntry {
    pub timestamp: DateTime<Utc>,
    pub session_id: String,
    pub prompt: String,
    /// Whether this prompt was a direct bash command (vs. an AI prompt).
    /// Defaults to `false` for backward compatibility — entries written before
    /// this field existed are treated as non-bash. Shell history files
    /// (`~/.bash_history` etc.) compensate for this gap.
    #[serde(default)]
    pub is_bash: bool,
}

/// Get the path to the prompt history file for a given CWD
pub(crate) fn prompt_history_path(cwd: &str) -> PathBuf {
    crate::util::echo_agent_home::sessions_cwd_dir(cwd).join(PROMPT_HISTORY_FILE)
}

/// Append a prompt to the history file (synchronous, fast append-only).
/// Creates parent directories if they don't exist.
pub(crate) fn append_prompt(cwd: &str, entry: &PromptEntry) -> io::Result<()> {
    let path = prompt_history_path(cwd);
    crate::util::echo_agent_home::ensure_sessions_cwd_dir(cwd)?;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;

    let mut line =
        serde_json::to_vec(entry).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    line.push(b'\n');
    file.write_all(&line)?;
    Ok(())
}

/// Load prompts for a given CWD.
/// Returns prompts in reverse chronological order (most recent first).
pub(crate) fn load_prompts(cwd: &str) -> io::Result<Vec<String>> {
    load_prompts_filtered(cwd, |_| true)
}

/// Load prompts for a given CWD, restricted to a single session id.
/// Returns prompts in reverse chronological order (most recent first), matching
/// `load_prompts` ordering — used by the pager's up-arrow / Ctrl+R history
/// overlay when it wants only the current session's prompts.
pub(crate) fn load_prompts_for_session(cwd: &str, session_id: &str) -> io::Result<Vec<String>> {
    load_prompts_filtered(cwd, |e| e.session_id == session_id)
}

/// Truncate the history file to MAX_PROMPT_HISTORY_ENTRIES if it exceeds the limit.
/// Uses atomic rename for safety.
pub(crate) fn truncate_if_needed(cwd: &str) -> io::Result<()> {
    let path = prompt_history_path(cwd);
    if !path.exists() {
        return Ok(());
    }

    let file = std::fs::File::open(&path)?;
    let reader = BufReader::new(file);

    let entries: Vec<PromptEntry> = reader
        .lines()
        .map_while(Result::ok)
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(&line).ok())
        .collect();

    if entries.len() <= MAX_PROMPT_HISTORY_ENTRIES {
        return Ok(());
    }

    // Keep the most recent entries
    let to_keep = &entries[entries.len() - MAX_PROMPT_HISTORY_ENTRIES..];

    // Write to a temp file first, then rename (atomic)
    let temp_path = path.with_extension("jsonl.tmp");
    {
        let mut file = std::fs::File::create(&temp_path)?;
        for entry in to_keep {
            let mut line = serde_json::to_vec(entry)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            line.push(b'\n');
            file.write_all(&line)?;
        }
    }

    std::fs::rename(temp_path, path)?;
    Ok(())
}

/// Async wrapper for append_prompt (fire-and-forget via spawn_blocking)
pub(crate) async fn append_prompt_async(cwd: String, entry: PromptEntry) {
    let _ = tokio::task::spawn_blocking(move || {
        if let Err(e) = append_prompt(&cwd, &entry) {
            tracing::warn!(?e, "failed to append prompt to history");
        }
    })
    .await;
}

/// Load only bash-command prompts for a given CWD.
/// Returns prompts in reverse chronological order (most recent first).
pub(crate) fn load_bash_prompts(cwd: &str) -> io::Result<Vec<String>> {
    load_prompts_filtered(cwd, |e| e.is_bash)
}

/// Shared implementation for loading prompts with an optional filter predicate.
fn load_prompts_filtered(
    cwd: &str,
    filter: impl Fn(&PromptEntry) -> bool,
) -> io::Result<Vec<String>> {
    let path = prompt_history_path(cwd);
    if !path.exists() {
        return Ok(Vec::new());
    }

    let file = std::fs::File::open(&path)?;
    let reader = BufReader::new(file);

    let mut prompts = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<PromptEntry>(&line)
            && filter(&entry)
        {
            prompts.push(entry.prompt);
        }
    }

    prompts.dedup();
    prompts.reverse();

    Ok(prompts)
}

/// Async wrapper for load_prompts
pub(crate) async fn load_prompts_async(cwd: String) -> io::Result<Vec<String>> {
    tokio::task::spawn_blocking(move || load_prompts(&cwd))
        .await
        .map_err(io::Error::other)?
}

/// Async wrapper for load_prompts_for_session
pub(crate) async fn load_prompts_for_session_async(
    cwd: String,
    session_id: String,
) -> io::Result<Vec<String>> {
    tokio::task::spawn_blocking(move || load_prompts_for_session(&cwd, &session_id))
        .await
        .map_err(io::Error::other)?
}

/// Async wrapper for load_bash_prompts
pub(crate) async fn load_bash_prompts_async(cwd: String) -> io::Result<Vec<String>> {
    tokio::task::spawn_blocking(move || load_bash_prompts(&cwd))
        .await
        .map_err(io::Error::other)?
}

/// Async wrapper for truncate_if_needed (background maintenance)
pub(crate) async fn truncate_if_needed_async(cwd: String) {
    let _ = tokio::task::spawn_blocking(move || {
        if let Err(e) = truncate_if_needed(&cwd) {
            tracing::warn!(?e, "failed to truncate prompt history");
        }
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_cwd() -> (TempDir, String) {
        let tmp = TempDir::new().unwrap();
        // Use a fake CWD path for testing
        let cwd = tmp
            .path()
            .join("test_project")
            .to_string_lossy()
            .to_string();
        (tmp, cwd)
    }

    #[test]
    fn test_append_and_load() {
        let (_tmp, cwd) = test_cwd();

        let entry1 = PromptEntry {
            timestamp: Utc::now(),
            session_id: "s1".into(),
            prompt: "first prompt".into(),
            is_bash: false,
        };
        let entry2 = PromptEntry {
            timestamp: Utc::now(),
            session_id: "s1".into(),
            prompt: "second prompt".into(),
            is_bash: false,
        };

        append_prompt(&cwd, &entry1).unwrap();
        append_prompt(&cwd, &entry2).unwrap();

        let prompts = load_prompts(&cwd).unwrap();
        assert_eq!(prompts.len(), 2);
        // Most recent first
        assert_eq!(prompts[0], "second prompt");
        assert_eq!(prompts[1], "first prompt");
    }

    #[test]
    fn test_deduplication() {
        let (_tmp, cwd) = test_cwd();

        // Add consecutive identical prompts
        for _ in 0..3 {
            let entry = PromptEntry {
                timestamp: Utc::now(),
                session_id: "s1".into(),
                prompt: "same prompt".into(),
                is_bash: false,
            };
            append_prompt(&cwd, &entry).unwrap();
        }

        let prompts = load_prompts(&cwd).unwrap();
        // Should be deduplicated to 1
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0], "same prompt");
    }

    #[test]
    fn test_empty_file() {
        let (_tmp, cwd) = test_cwd();
        let prompts = load_prompts(&cwd).unwrap();
        assert!(prompts.is_empty());
    }

    #[test]
    fn test_path_encoding() {
        // CWD with special characters
        let cwd = "/path/with spaces/and@special#chars";
        let path = prompt_history_path(cwd);
        assert!(path.to_string_lossy().contains("prompt_history.jsonl"));
        // The path should be encoded
        assert!(!path.to_string_lossy().contains(" "));
    }

    #[test]
    fn test_load_bash_prompts_filters_correctly() {
        let (_tmp, cwd) = test_cwd();

        let bash_entry = PromptEntry {
            timestamp: Utc::now(),
            session_id: "s1".into(),
            prompt: "git status".into(),
            is_bash: true,
        };
        let ai_entry = PromptEntry {
            timestamp: Utc::now(),
            session_id: "s1".into(),
            prompt: "explain this code".into(),
            is_bash: false,
        };
        let bash_entry2 = PromptEntry {
            timestamp: Utc::now(),
            session_id: "s1".into(),
            prompt: "ls -la".into(),
            is_bash: true,
        };

        append_prompt(&cwd, &bash_entry).unwrap();
        append_prompt(&cwd, &ai_entry).unwrap();
        append_prompt(&cwd, &bash_entry2).unwrap();

        let bash_prompts = load_bash_prompts(&cwd).unwrap();
        assert_eq!(bash_prompts.len(), 2);
        assert_eq!(bash_prompts[0], "ls -la");
        assert_eq!(bash_prompts[1], "git status");

        // load_prompts still returns all
        let all_prompts = load_prompts(&cwd).unwrap();
        assert_eq!(all_prompts.len(), 3);
    }

    #[test]
    fn test_backward_compat_missing_is_bash() {
        let (_tmp, cwd) = test_cwd();
        let path = prompt_history_path(&cwd);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        // Write an entry WITHOUT the is_bash field (simulating old format)
        let old_json =
            r#"{"timestamp":"2024-01-01T00:00:00Z","session_id":"s1","prompt":"old command"}"#;
        std::fs::write(&path, format!("{old_json}\n")).unwrap();

        // Should deserialize fine with is_bash defaulting to false
        let all = load_prompts(&cwd).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0], "old command");

        // Should NOT appear in bash-filtered results
        let bash = load_bash_prompts(&cwd).unwrap();
        assert!(bash.is_empty());
    }

    #[test]
    fn test_load_bash_prompts_deduplicates() {
        let (_tmp, cwd) = test_cwd();

        for _ in 0..3 {
            let entry = PromptEntry {
                timestamp: Utc::now(),
                session_id: "s1".into(),
                prompt: "git status".into(),
                is_bash: true,
            };
            append_prompt(&cwd, &entry).unwrap();
        }

        let bash = load_bash_prompts(&cwd).unwrap();
        assert_eq!(bash.len(), 1);
        assert_eq!(bash[0], "git status");
    }

    #[test]
    fn test_load_bash_prompts_empty_file() {
        let (_tmp, cwd) = test_cwd();
        let bash = load_bash_prompts(&cwd).unwrap();
        assert!(bash.is_empty());
    }

    #[test]
    fn test_load_prompts_for_session_filters_by_session_id() {
        let (_tmp, cwd) = test_cwd();

        let mk = |session_id: &str, prompt: &str| PromptEntry {
            timestamp: Utc::now(),
            session_id: session_id.into(),
            prompt: prompt.into(),
            is_bash: false,
        };

        // Interleave prompts from two sessions in the shared per-CWD file.
        append_prompt(&cwd, &mk("s1", "s1 first")).unwrap();
        append_prompt(&cwd, &mk("s2", "s2 first")).unwrap();
        append_prompt(&cwd, &mk("s1", "s1 second")).unwrap();
        append_prompt(&cwd, &mk("s2", "s2 second")).unwrap();

        // Only s1's prompts, most-recent-first (same ordering as load_prompts).
        let s1 = load_prompts_for_session(&cwd, "s1").unwrap();
        assert_eq!(s1, vec!["s1 second".to_string(), "s1 first".to_string()]);

        let s2 = load_prompts_for_session(&cwd, "s2").unwrap();
        assert_eq!(s2, vec!["s2 second".to_string(), "s2 first".to_string()]);

        // Unknown session id yields nothing.
        assert!(load_prompts_for_session(&cwd, "nope").unwrap().is_empty());

        // The unfiltered load still returns everything.
        assert_eq!(load_prompts(&cwd).unwrap().len(), 4);
    }

    #[tokio::test]
    async fn test_append_prompt_async_round_trips_for_session() {
        let (_tmp, cwd) = test_cwd();

        let entry = PromptEntry {
            timestamp: Utc::now(),
            session_id: "s1".into(),
            prompt: "durable prompt".into(),
            is_bash: false,
        };

        // Mirrors the submit path: awaiting the wrapper makes the append immediately loadable.
        append_prompt_async(cwd.clone(), entry).await;

        let prompts = load_prompts_for_session_async(cwd, "s1".into())
            .await
            .unwrap();
        assert_eq!(prompts, vec!["durable prompt".to_string()]);
    }
}
