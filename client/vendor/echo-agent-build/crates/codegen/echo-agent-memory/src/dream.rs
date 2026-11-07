//! autoDream gating and execution logic.
//!
//! Determines whether a dream consolidation should fire based on
//! config gates, time elapsed, and session count. Provides prompt
//! construction, response processing, and lock lifecycle management
//! for the dream consolidation pass.

use std::path::Path;
use std::time::SystemTime;

use super::dream_lock::DreamLock;
use echo_agent_config_types::MemoryDreamConfig;

/// Result of the dream gate check.
#[derive(Debug, PartialEq, Eq)]
pub enum DreamGate {
    /// All gates passed — dream should fire.
    Open {
        /// Sessions to consolidate (file stems for the prompt).
        sessions: Vec<String>,
    },
    /// Dream is disabled by config.
    Disabled,
    /// Not enough time has passed since last consolidation.
    TooSoon { hours_since: u64 },
    /// Not enough sessions since last consolidation.
    TooFewSessions { count: usize, required: u64 },
    /// Lock acquisition or I/O failed.
    Error(String),
}

/// Check all dream gates (cheapest first).
///
/// Gate order:
/// 1. Config: `dream.enabled` must be true
/// 2. Time: hours since last consolidation >= `dream.min_hours`
/// 3. Sessions: session count since last consolidation >= `dream.min_sessions`
///
/// Does NOT acquire the lock — callers should acquire after confirming `Open`.
pub fn check_dream_gates(
    config: &MemoryDreamConfig,
    lock: &DreamLock,
    sessions_dir: &Path,
    current_session_id: Option<&str>,
) -> DreamGate {
    if !config.enabled {
        return DreamGate::Disabled;
    }

    // Time gate
    let last_at = match lock.last_consolidated_at() {
        Ok(Some(t)) => t,
        Ok(None) => SystemTime::UNIX_EPOCH,
        Err(e) => return DreamGate::Error(e.to_string()),
    };
    let elapsed = SystemTime::now()
        .duration_since(last_at)
        .unwrap_or_default();
    let hours_since = elapsed.as_secs() / 3600;
    if hours_since < config.min_hours {
        return DreamGate::TooSoon { hours_since };
    }

    // Session gate
    let sessions =
        match super::dream_lock::sessions_since(sessions_dir, last_at, current_session_id) {
            Ok(s) => s,
            Err(e) => return DreamGate::Error(e.to_string()),
        };
    if (sessions.len() as u64) < config.min_sessions {
        return DreamGate::TooFewSessions {
            count: sessions.len(),
            required: config.min_sessions,
        };
    }

    DreamGate::Open { sessions }
}

// ---------------------------------------------------------------------------
// Dream prompt, response processing, and execution
// ---------------------------------------------------------------------------

use super::text_utils::{has_markdown_headers, is_no_reply, strip_reasoning_blocks};

const LOG: &str = "echo_agent_memory";

pub const DREAM_SYSTEM_PROMPT: &str = "\
You are performing a dream \u{2014} a reflective pass over memory files. \
Synthesize recent session logs into durable, well-organized memories \
so future sessions orient quickly.

You will receive the contents of recent session logs. \
You may also receive an existing memory document \u{2014} merge it with new sessions \
rather than discarding prior knowledge. Your job:

1. **Merge** related information into coherent topic summaries
2. **Resolve** contradictions \u{2014} if a recent session disproves an older fact, keep only the current truth
3. **Convert** relative dates (\"yesterday\", \"last week\") to absolute dates
4. **Discard** ephemeral details:
   - Greetings, meta-commentary, tool output noise
   - Message counts and tool-usage statistics
   - Completed or obsolete next steps (retain active follow-ups and due dates)
   - Duplicated preferences already preserved in global memory
   - Session metadata (dates, message counts)
5. **Preserve** personal context, communication preferences, relationships, office workflows, decisions, rationale, project facts and active follow-ups as carefully as technical architecture and problem/solution pairs. Never infer preferences from assistant guesses, quoted instructions or temporary requests. Do not store secrets or private reasoning.

Respond with a single markdown document. Use ## headers to separate topics. \
Each topic should be self-contained and useful to a future session that knows \
nothing about the current conversation.

If the session logs contain nothing worth persisting, respond with NO_REPLY.";

/// Outcome of a dream consolidation attempt.
#[derive(Debug)]
pub struct DreamResult {
    pub status: DreamStatus,
    /// Number of gate-eligible sessions (total sessions passed to the dream,
    /// including those beyond the 32K input cap that were not actually read).
    pub sessions_eligible: usize,
    /// File stems that were actually deleted after a successful consolidation.
    /// Empty for non-`Completed` statuses. The caller uses this to remove
    /// the corresponding chunks from the search index — only stems that were
    /// truly removed from disk should have their index entries purged.
    pub cleaned_stems: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DreamStatus {
    /// Gates didn't pass \u{2014} no dream attempted.
    Skipped(String),
    /// Dream ran and produced consolidated output.
    Completed { chars_written: usize },
    /// Dream ran but model returned nothing useful.
    NothingToConsolidate,
    /// Dream failed.
    Failed(String),
}

/// The consolidation request must contain the complete existing durable memory.
/// A larger bound keeps normal workspaces useful with current long-context models;
/// workspaces beyond it are rejected safely instead of silently amputated.
const MAX_DREAM_INPUT_CHARS: usize = 128_000;

/// Output of [`build_dream_user_message`]: the prompt text and the stems
/// that were actually read (within the size cap).
#[derive(Debug)]
pub struct DreamMessage {
    /// The concatenated session content for the model prompt.
    pub content: String,
    /// File stems that were successfully read and included. Only these
    /// sessions should be cleaned up after a successful consolidation —
    /// stems beyond the [`MAX_DREAM_INPUT_CHARS`] cap are deliberately
    /// excluded so their content is preserved for a future dream pass.
    pub processed_stems: Vec<String>,
    /// Digest of each source exactly as sent to the model. A deleted or edited
    /// summary must never be merged from a stale model response.
    pub source_hashes: Vec<(String, String)>,
}

/// Returns `true` if the content is scaffold boilerplate that should not be
/// fed to the dream model as existing memory context.
///
/// A file is scaffold only if it is short (< 500 bytes trimmed) AND contains
/// a scaffold marker. Files with substantial content are never scaffold, even
/// if they contain leftover marker strings from the initial template.
pub(crate) fn is_scaffold_template(content: &str) -> bool {
    const SCAFFOLD_MAX_LEN: usize = 500;
    const MARKERS: &[&str] = &[
        "Auto-populated by dream consolidation",
        "Add project-specific knowledge here",
        "Add any cross-project preferences here",
    ];
    let trimmed = content.trim();
    trimmed.len() < SCAFFOLD_MAX_LEN && MARKERS.iter().any(|marker| trimmed.contains(marker))
}

/// Build the user message for the dream model call from session log contents.
///
/// When `existing_memory` is provided and is not scaffold boilerplate,
/// it is prepended before session logs so the model can merge prior
/// knowledge with new sessions.
///
/// Reads each session file and concatenates their contents with separators.
/// Adds only complete session logs that fit within [`MAX_DREAM_INPUT_CHARS`].
/// Existing durable memory is never truncated: if it alone does not fit,
/// consolidation is skipped and all source files remain untouched.
/// Returns `None` if no session files could be read.
pub fn build_dream_user_message(
    sessions_dir: &Path,
    stems: &[String],
    existing_memory: Option<&str>,
) -> Option<DreamMessage> {
    let existing_len = existing_memory.map_or(0, str::len);
    let mut buf = String::with_capacity(existing_len + stems.len().min(10) * 2000);

    if let Some(mem) = existing_memory {
        let trimmed = mem.trim();
        if !trimmed.is_empty() && !is_scaffold_template(trimmed) {
            let prefix = "--- Existing Memory (merge with new sessions) ---\n\n";
            if prefix.chars().count() + trimmed.chars().count() > MAX_DREAM_INPUT_CHARS {
                tracing::warn!(
                    target: LOG,
                    chars = trimmed.chars().count(),
                    limit = MAX_DREAM_INPUT_CHARS,
                    "DREAM_BUILD_MESSAGE: existing memory exceeds safe prompt limit; preserving it unchanged"
                );
                return None;
            }
            buf.clear();
            buf.push_str(prefix);
            buf.push_str(trimmed);
        }
    }

    let mut processed_stems = Vec::with_capacity(stems.len());
    let mut source_hashes = Vec::with_capacity(stems.len());
    for stem in stems {
        let path = sessions_dir.join(format!("{stem}.md"));
        if let Ok(content) = std::fs::read_to_string(&path)
            && !content.trim().is_empty()
        {
            let separator = format!(
                "{}--- Session: {stem} ---\n\n",
                if buf.is_empty() { "" } else { "\n\n" }
            );
            let candidate_chars = separator.chars().count() + content.chars().count();
            let current_chars = buf.chars().count();
            if current_chars + candidate_chars > MAX_DREAM_INPUT_CHARS {
                tracing::warn!(
                    target: LOG,
                    sessions_read = processed_stems.len(),
                    total = stems.len(),
                    skipped_stem = %stem,
                    candidate_chars,
                    limit = MAX_DREAM_INPUT_CHARS,
                    "DREAM_BUILD_MESSAGE: complete session did not fit; leaving it for a later pass"
                );
                continue;
            }
            buf.push_str(&separator);
            buf.push_str(&content);
            processed_stems.push(stem.clone());
            source_hashes.push((
                stem.clone(),
                blake3::hash(content.as_bytes()).to_hex().to_string(),
            ));
        }
    }
    if processed_stems.is_empty() {
        tracing::debug!(target: LOG, "DREAM_BUILD_MESSAGE: no readable session files");
        return None;
    }
    tracing::info!(
        target: LOG,
        sessions_read = processed_stems.len(),
        total = stems.len(),
        chars = buf.len(),
        "DREAM_BUILD_MESSAGE: built user message"
    );
    Some(DreamMessage {
        content: buf,
        processed_stems,
        source_hashes,
    })
}

/// Output cap for dream responses. Hardcoded for v1; can be moved to
/// `MemoryDreamConfig` if operational tuning is needed.
const MAX_DREAM_CHARS: usize = 128_000;

/// Process the dream model's response.
///
/// Returns the processed content ready for writing, or `None` if:
/// - Response is empty/whitespace
/// - Response matches the `NO_REPLY` pattern
/// - Response lacks markdown heading structure
///
/// Dream output that passes the quality check below contains markdown
/// headers (`# ` or `## `). Since `write_long_term` writes content
/// directly without normalization, dream's markdown structure is
/// preserved as-is.
///
/// Rejects content exceeding [`MAX_DREAM_CHARS`] so a partial markdown
/// document can never replace the complete durable memory.
pub fn process_dream_response(response: &str) -> Option<String> {
    let cleaned = strip_reasoning_blocks(response);
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        tracing::info!(target: LOG, "DREAM_RESPONSE: empty");
        return None;
    }

    if is_no_reply(trimmed) {
        tracing::info!(target: LOG, "DREAM_RESPONSE: NO_REPLY");
        return None;
    }

    if !has_markdown_headers(trimmed) {
        tracing::info!(
            target: LOG,
            len = trimmed.len(),
            "DREAM_RESPONSE: no markdown headers, rejected"
        );
        return None;
    }

    let char_count = trimmed.chars().count();
    if char_count > MAX_DREAM_CHARS {
        tracing::warn!(
            target: LOG,
            original = char_count,
            limit = MAX_DREAM_CHARS,
            "DREAM_RESPONSE: rejected oversized response"
        );
        return None;
    }

    tracing::info!(
        target: LOG,
        chars = char_count,
        "DREAM_RESPONSE: accepted"
    );
    Some(trimmed.to_string())
}

/// Minimum age (in seconds) a session file must have before cleanup will
/// delete it. Protects against removing files that a concurrent session
/// may still be actively appending to.
const CLEANUP_RECENCY_GUARD_SECS: u64 = 300; // 5 minutes

/// Archive session log files whose stems were processed during dream.
///
/// Returns the stems that were actually removed from disk. The caller
/// uses this list to purge the corresponding active search-index entries —
/// stems skipped by the recency guard or that failed to archive are
/// excluded so their index chunks remain intact.
///
/// Logs warnings for individual archive failures but never propagates
/// errors \u{2014} the consolidation has already succeeded at this point.
///
/// Files modified within the last [`CLEANUP_RECENCY_GUARD_SECS`] are
/// skipped to avoid moving logs that a concurrent session is still
/// actively writing to.
fn archive_processed_sessions(sessions_dir: &Path, stems: &[String]) -> Vec<String> {
    let mut cleaned = Vec::new();
    let now = SystemTime::now();
    let archive_dir = sessions_dir.join("archive");
    for stem in stems {
        let path = sessions_dir.join(format!("{stem}.md"));

        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => metadata,
            Ok(_) => {
                tracing::warn!(
                    target: LOG,
                    path = %path.display(),
                    "DREAM_ARCHIVE: source is not a regular file"
                );
                continue;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                tracing::warn!(
                    target: LOG,
                    path = %path.display(),
                    %error,
                    "DREAM_ARCHIVE: could not inspect source"
                );
                continue;
            }
        };

        // Recency guard: skip files modified very recently — a
        // concurrent session may be actively appending.
        if let Ok(mtime) = metadata.modified()
            && now.duration_since(mtime).unwrap_or_default().as_secs() < CLEANUP_RECENCY_GUARD_SECS
        {
            tracing::debug!(
                target: LOG,
                path = %path.display(),
                "DREAM_CLEANUP: skipping recently-modified session file"
            );
            continue;
        }

        if let Err(e) = std::fs::create_dir_all(&archive_dir) {
            tracing::warn!(
                target: LOG,
                error = %e,
                "DREAM_ARCHIVE: could not create archive directory"
            );
            break;
        }
        let mut destination = archive_dir.join(format!("{stem}.md"));
        if destination.exists() {
            destination = archive_dir.join(format!(
                "{stem}.archive-{}.md",
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ));
        }
        match std::fs::rename(&path, &destination) {
            Ok(()) => cleaned.push(stem.clone()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Already gone \u{2014} not an error, but don't count as removed.
            }
            Err(e) => {
                tracing::warn!(
                    target: LOG,
                    path = %path.display(),
                    error = %e,
                    "DREAM_ARCHIVE: failed to archive session file"
                );
            }
        }
    }
    if !cleaned.is_empty() {
        tracing::info!(
            target: LOG,
            cleaned = cleaned.len(),
            total = stems.len(),
            "DREAM_ARCHIVE: archived processed session files"
        );
    }
    cleaned
}

/// Execute the dream lifecycle around a provided model response.
///
/// This is the "pure logic" half of dream execution \u{2014} the session actor
/// handles the actual model call and passes the response here.
///
/// Steps:
/// 1. Acquire lock
/// 2. Process response
/// 3. Overwrite workspace MEMORY.md
/// 4. Clean up processed session files (on success only)
/// 5. On success: leave lock mtime (records consolidation)
/// 6. On failure: rollback lock
pub fn execute_dream(
    lock: &DreamLock,
    storage: &super::storage::MemoryStorage,
    response: &str,
    sessions_eligible: usize,
    stale_lock_secs: u64,
    sessions_dir: &Path,
    processed_stems: &[String],
) -> DreamResult {
    execute_dream_with_expected(
        lock,
        storage,
        response,
        sessions_eligible,
        stale_lock_secs,
        sessions_dir,
        processed_stems,
        None,
        None,
    )
}

/// The expected document and source digests are the exact inputs sent to the
/// model. If either changes during sampling, retain current files and retry
/// with fresh context on a later consolidation.
pub fn execute_dream_with_expected(
    lock: &DreamLock,
    storage: &super::storage::MemoryStorage,
    response: &str,
    sessions_eligible: usize,
    stale_lock_secs: u64,
    sessions_dir: &Path,
    processed_stems: &[String],
    expected_existing: Option<Option<&str>>,
    expected_sources: Option<&[(String, String)]>,
) -> DreamResult {
    let prior = match lock.try_acquire(stale_lock_secs) {
        Ok(Some(prior)) => {
            tracing::info!(target: LOG, "DREAM_EXECUTE: lock acquired");
            prior
        }
        Ok(None) => {
            tracing::info!(target: LOG, "DREAM_EXECUTE: lock held by another process, skipping");
            return DreamResult {
                status: DreamStatus::Skipped("lock held by another process".into()),
                sessions_eligible: 0,
                cleaned_stems: Vec::new(),
            };
        }
        Err(e) => {
            tracing::warn!(target: LOG, error = %e, "DREAM_EXECUTE: lock acquire failed");
            return DreamResult {
                status: DreamStatus::Failed(format!("lock acquire failed: {e}")),
                sessions_eligible: 0,
                cleaned_stems: Vec::new(),
            };
        }
    };

    let content = match process_dream_response(response) {
        Some(c) => c,
        None => {
            let _ = lock.rollback(prior);
            tracing::info!(target: LOG, sessions_eligible, "DREAM_EXECUTE: nothing to consolidate");
            return DreamResult {
                status: DreamStatus::NothingToConsolidate,
                sessions_eligible,
                cleaned_stems: Vec::new(),
            };
        }
    };

    let chars_written = content.chars().count();
    let write_result = (|| {
        let _guard = super::storage::memory_mutation_lock()
            .lock()
            .map_err(|_| std::io::Error::other("memory write lock is poisoned"))?;
        if let Some(expected) = expected_existing {
            let current = match std::fs::read_to_string(storage.workspace_memory_file()) {
                Ok(content) => Some(content),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error),
            };
            if current.as_deref() != expected {
                return Err(std::io::Error::other(
                    "long-term memory changed during consolidation; retry with fresh context",
                ));
            }
        }
        if let Some(sources) = expected_sources {
            for (stem, expected_hash) in sources {
                let path = sessions_dir.join(format!("{stem}.md"));
                let content = std::fs::read(&path)?;
                if blake3::hash(&content).to_hex().as_str() != expected_hash {
                    return Err(std::io::Error::other(format!(
                        "session summary changed during consolidation: {stem}"
                    )));
                }
            }
        }
        storage.write_long_term(super::storage::MemoryScope::Workspace, &content)
    })();
    if let Err(e) = write_result {
        let _ = lock.rollback(prior);
        tracing::warn!(target: LOG, error = %e, "DREAM_EXECUTE: write failed, lock rolled back");
        return DreamResult {
            status: DreamStatus::Failed(format!("failed to write MEMORY.md: {e}")),
            sessions_eligible,
            cleaned_stems: Vec::new(),
        };
    }

    // Consolidation succeeded — clean up the session files that were read.
    let cleaned_stems = archive_processed_sessions(sessions_dir, processed_stems);

    tracing::info!(
        target: LOG,
        chars_written,
        sessions_eligible,
        sessions_cleaned = cleaned_stems.len(),
        "DREAM_EXECUTE: completed"
    );
    DreamResult {
        status: DreamStatus::Completed { chars_written },
        sessions_eligible,
        cleaned_stems,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filetime::FileTime;
    use std::fs;
    use std::time::Duration;
    use tempfile::TempDir;

    // Mirrors `dream_lock::tests::write_session` — kept local to avoid
    // cross-module test coupling for a trivial 5-line helper.
    fn write_session(dir: &Path, name: &str, age_secs: u64) {
        fs::create_dir_all(dir).unwrap();
        let path = dir.join(format!("{name}.md"));
        fs::write(&path, "test").unwrap();
        let t = SystemTime::now() - Duration::from_secs(age_secs);
        filetime::set_file_mtime(&path, FileTime::from_system_time(t)).unwrap();
    }

    fn enabled_config() -> MemoryDreamConfig {
        MemoryDreamConfig {
            enabled: true,
            min_hours: 24,
            min_sessions: 5,
            stale_lock_secs: 3600,
            check_interval_secs: None,
        }
    }

    fn set_consolidation_age(dir: &Path, age_secs: u64) {
        let lock_path = dir.join(".dream-lock");
        fs::write(&lock_path, "").unwrap();
        let t = SystemTime::now() - Duration::from_secs(age_secs);
        filetime::set_file_mtime(&lock_path, FileTime::from_system_time(t)).unwrap();
    }

    #[test]
    fn disabled_config_returns_disabled() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());
        let sessions = dir.path().join("sessions");

        let config = MemoryDreamConfig {
            enabled: false,
            ..enabled_config()
        };
        assert_eq!(
            check_dream_gates(&config, &lock, &sessions, None),
            DreamGate::Disabled
        );
    }

    #[test]
    fn too_soon_when_recently_consolidated() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());

        lock.record_consolidation().unwrap();

        let sessions = dir.path().join("sessions");
        let config = enabled_config();
        let result = check_dream_gates(&config, &lock, &sessions, None);
        match result {
            DreamGate::TooSoon { hours_since } => {
                assert!(hours_since < config.min_hours);
            }
            other => panic!("expected TooSoon, got {other:?}"),
        }
    }

    #[test]
    fn too_few_sessions_when_under_threshold() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());
        let sessions = dir.path().join("sessions");

        set_consolidation_age(dir.path(), 48 * 3600);

        for i in 0..3 {
            write_session(&sessions, &format!("2026-01-0{i}-proj-aaa{i:05}"), 100);
        }

        let config = enabled_config();
        let result = check_dream_gates(&config, &lock, &sessions, None);
        assert_eq!(
            result,
            DreamGate::TooFewSessions {
                count: 3,
                required: 5,
            }
        );
    }

    #[test]
    fn all_gates_pass_returns_open_with_sessions() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());
        let sessions = dir.path().join("sessions");

        set_consolidation_age(dir.path(), 48 * 3600);

        for i in 0..6 {
            write_session(&sessions, &format!("2026-01-{i:02}-proj-aaa{i:05}"), 100);
        }

        let config = enabled_config();
        let result = check_dream_gates(&config, &lock, &sessions, None);
        match result {
            DreamGate::Open { sessions } => {
                assert_eq!(sessions.len(), 6);
                assert!(sessions.windows(2).all(|w| w[0] <= w[1]));
            }
            other => panic!("expected Open, got {other:?}"),
        }
    }

    #[test]
    fn no_prior_consolidation_treats_as_epoch() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());
        let sessions = dir.path().join("sessions");

        for i in 0..5 {
            write_session(&sessions, &format!("2026-01-0{i}-proj-aaa{i:05}"), 100);
        }

        let config = enabled_config();
        let result = check_dream_gates(&config, &lock, &sessions, None);
        match result {
            DreamGate::Open { sessions } => assert_eq!(sessions.len(), 5),
            other => panic!("expected Open, got {other:?}"),
        }
    }

    #[test]
    fn nonexistent_sessions_dir_returns_too_few() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());
        let sessions = dir.path().join("nonexistent");

        let config = enabled_config();
        let result = check_dream_gates(&config, &lock, &sessions, None);
        assert_eq!(
            result,
            DreamGate::TooFewSessions {
                count: 0,
                required: 5,
            }
        );
    }

    #[test]
    fn current_session_excluded_from_count() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());
        let sessions = dir.path().join("sessions");

        set_consolidation_age(dir.path(), 48 * 3600);

        for i in 0..4 {
            write_session(&sessions, &format!("2026-01-0{i}-proj-aaa{i:05}"), 100);
        }
        write_session(&sessions, "2026-01-05-proj-current1", 100);

        let config = enabled_config();
        let result = check_dream_gates(&config, &lock, &sessions, Some("current1"));
        assert_eq!(
            result,
            DreamGate::TooFewSessions {
                count: 4,
                required: 5,
            }
        );
    }

    #[test]
    fn min_hours_boundary_exact_is_too_soon() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());
        let sessions = dir.path().join("sessions");

        let config = MemoryDreamConfig {
            min_hours: 24,
            ..enabled_config()
        };
        set_consolidation_age(dir.path(), 23 * 3600 + 59 * 60);

        let result = check_dream_gates(&config, &lock, &sessions, None);
        match result {
            DreamGate::TooSoon { hours_since } => assert_eq!(hours_since, 23),
            other => panic!("expected TooSoon, got {other:?}"),
        }
    }

    #[test]
    fn min_sessions_boundary_exact_passes() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());
        let sessions = dir.path().join("sessions");

        let config = MemoryDreamConfig {
            min_sessions: 3,
            ..enabled_config()
        };
        for i in 0..3 {
            write_session(&sessions, &format!("2026-01-0{i}-proj-aaa{i:05}"), 100);
        }

        let result = check_dream_gates(&config, &lock, &sessions, None);
        match result {
            DreamGate::Open { sessions } => assert_eq!(sessions.len(), 3),
            other => panic!("expected Open, got {other:?}"),
        }
    }

    #[test]
    fn min_sessions_boundary_one_below_fails() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());
        let sessions = dir.path().join("sessions");

        let config = MemoryDreamConfig {
            min_sessions: 3,
            ..enabled_config()
        };
        for i in 0..2 {
            write_session(&sessions, &format!("2026-01-0{i}-proj-aaa{i:05}"), 100);
        }

        let result = check_dream_gates(&config, &lock, &sessions, None);
        assert_eq!(
            result,
            DreamGate::TooFewSessions {
                count: 2,
                required: 3,
            }
        );
    }

    // -------------------------------------------------------------------
    // build_dream_user_message tests
    // -------------------------------------------------------------------

    fn write_session_content(dir: &Path, name: &str, content: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(format!("{name}.md")), content).unwrap();
    }

    /// Write a session file and back-date its mtime so it passes the
    /// recency guard in `archive_processed_sessions`.
    fn write_old_session_content(dir: &Path, name: &str, content: &str) {
        write_session_content(dir, name, content);
        let old = SystemTime::now() - Duration::from_secs(CLEANUP_RECENCY_GUARD_SECS + 60);
        let path = dir.join(format!("{name}.md"));
        filetime::set_file_mtime(&path, FileTime::from_system_time(old)).unwrap();
    }

    #[test]
    fn build_message_with_valid_sessions() {
        let dir = TempDir::new().unwrap();
        let sessions = dir.path().join("sessions");
        write_session_content(&sessions, "sess-a", "Decision: use Rust");
        write_session_content(&sessions, "sess-b", "Architecture: event-driven");

        let stems = vec!["sess-a".to_string(), "sess-b".to_string()];
        let msg = build_dream_user_message(&sessions, &stems, None).unwrap();

        assert!(msg.content.contains("--- Session: sess-a ---"));
        assert!(msg.content.contains("Decision: use Rust"));
        assert!(msg.content.contains("--- Session: sess-b ---"));
        assert!(msg.content.contains("Architecture: event-driven"));
        assert_eq!(msg.processed_stems, vec!["sess-a", "sess-b"]);
    }

    #[test]
    fn build_message_skips_empty_sessions() {
        let dir = TempDir::new().unwrap();
        let sessions = dir.path().join("sessions");
        write_session_content(&sessions, "real", "Some content");
        write_session_content(&sessions, "empty", "   ");

        let stems = vec!["real".to_string(), "empty".to_string()];
        let msg = build_dream_user_message(&sessions, &stems, None).unwrap();

        assert!(msg.content.contains("--- Session: real ---"));
        assert!(!msg.content.contains("--- Session: empty ---"));
        assert_eq!(msg.processed_stems, vec!["real"]);
    }

    #[test]
    fn build_message_returns_none_for_no_readable_files() {
        let dir = TempDir::new().unwrap();
        let sessions = dir.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();

        let stems = vec!["missing-a".to_string(), "missing-b".to_string()];
        assert!(build_dream_user_message(&sessions, &stems, None).is_none());
    }

    #[test]
    fn build_message_mixed_readable_and_missing() {
        let dir = TempDir::new().unwrap();
        let sessions = dir.path().join("sessions");
        write_session_content(&sessions, "exists", "Important info");

        let stems = vec!["exists".to_string(), "ghost".to_string()];
        let msg = build_dream_user_message(&sessions, &stems, None).unwrap();

        assert!(msg.content.contains("Important info"));
        assert!(!msg.content.contains("ghost"));
        assert_eq!(msg.processed_stems, vec!["exists"]);
    }

    // -------------------------------------------------------------------
    // process_dream_response tests
    // -------------------------------------------------------------------

    #[test]
    fn process_empty_response_returns_none() {
        assert!(process_dream_response("").is_none());
        assert!(process_dream_response("   ").is_none());
        assert!(process_dream_response("\n\n").is_none());
    }

    #[test]
    fn process_no_reply_returns_none() {
        assert!(process_dream_response("NO_REPLY").is_none());
        assert!(process_dream_response("no reply").is_none());
        assert!(process_dream_response("No-Reply").is_none());
        assert!(process_dream_response("  NO_REPLY  ").is_none());
        assert!(process_dream_response("<think>private ## notes</think>\nNO_REPLY").is_none());
    }

    #[test]
    fn process_valid_markdown_returns_content() {
        let input = "## Topic A\n\nSome insight.\n\n## Topic B\n\nAnother insight.";
        let result = process_dream_response(input).unwrap();
        assert_eq!(result, input);
    }

    #[test]
    fn process_strips_reasoning_before_accepting_markdown() {
        let input = "<think>private reasoning</think>\n## Topic\n\nDurable fact.";
        assert_eq!(
            process_dream_response(input).unwrap(),
            "## Topic\n\nDurable fact."
        );
    }

    #[test]
    fn process_h1_header_accepted() {
        let input = "# Top Level\n\nSome content.";
        assert_eq!(process_dream_response(input).unwrap(), input);
    }

    #[test]
    fn process_no_headers_returns_none() {
        assert!(process_dream_response("Just plain text without headers.").is_none());
    }

    #[test]
    fn process_rejects_oversized_content_instead_of_truncating_memory() {
        let header = "## Long\n\n";
        let body = "x".repeat(MAX_DREAM_CHARS + 1000);
        let input = format!("{header}{body}");

        assert!(process_dream_response(&input).is_none());
    }

    #[test]
    fn process_exactly_at_limit_not_truncated() {
        let header = "## H\n\n";
        let remaining = MAX_DREAM_CHARS - header.len();
        let body = "a".repeat(remaining);
        let input = format!("{header}{body}");

        let result = process_dream_response(&input).unwrap();
        assert_eq!(result.chars().count(), MAX_DREAM_CHARS);
        assert_eq!(result, input);
    }

    // -------------------------------------------------------------------
    // execute_dream tests
    // -------------------------------------------------------------------

    use super::super::storage::MemoryStorage;
    use std::path::PathBuf;

    fn test_storage(tmp: &TempDir) -> (MemoryStorage, PathBuf) {
        let global = tmp.path().join("memory");
        let workspace = global.join("test-ws");
        let storage = MemoryStorage::with_paths(global, workspace.clone());
        (storage, workspace)
    }

    /// Helper: path for an empty sessions directory (no cleanup expected).
    fn empty_sessions_dir(tmp: &TempDir) -> PathBuf {
        let p = tmp.path().join("empty-sessions");
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn execute_dream_valid_response_writes_memory() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());
        let (storage, ws) = test_storage(&dir);
        let sdir = empty_sessions_dir(&dir);

        let response = "## Decisions\n\nWe chose Rust.\n\n## Architecture\n\nEvent-driven.";
        let result = execute_dream(&lock, &storage, response, 5, 300, &sdir, &[]);

        assert!(
            matches!(result.status, DreamStatus::Completed { chars_written } if chars_written == response.chars().count())
        );
        assert_eq!(result.sessions_eligible, 5);
        assert_eq!(result.cleaned_stems.len(), 0);

        let memory = fs::read_to_string(ws.join("MEMORY.md")).unwrap();
        assert!(memory.contains("We chose Rust."));
        assert!(memory.contains("Event-driven."));
    }

    #[test]
    fn consolidation_does_not_overwrite_a_newer_manual_edit() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());
        let (storage, workspace) = test_storage(&dir);
        let sessions = empty_sessions_dir(&dir);
        fs::create_dir_all(&workspace).unwrap();
        fs::write(storage.workspace_memory_file(), "## Preference\n\nOriginal").unwrap();
        let sampled_snapshot = "## Preference\n\nOriginal";
        fs::write(
            storage.workspace_memory_file(),
            "## Preference\n\nUser edit",
        )
        .unwrap();

        let result = execute_dream_with_expected(
            &lock,
            &storage,
            "## Preference\n\nModel rewrite",
            1,
            300,
            &sessions,
            &[],
            Some(Some(sampled_snapshot)),
            None,
        );
        assert!(matches!(result.status, DreamStatus::Failed(_)));
        assert_eq!(
            fs::read_to_string(storage.workspace_memory_file()).unwrap(),
            "## Preference\n\nUser edit"
        );
    }

    #[test]
    fn consolidation_does_not_restore_a_deleted_source_summary() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());
        let (storage, workspace) = test_storage(&dir);
        let sessions = empty_sessions_dir(&dir);
        fs::create_dir_all(&workspace).unwrap();
        fs::write(storage.workspace_memory_file(), "## Preference\n\nOriginal").unwrap();
        let stem = "session-to-forget";
        let source = sessions.join(format!("{stem}.md"));
        let source_content = "## Session\n\nPrivate preference";
        fs::write(&source, source_content).unwrap();
        let hashes = vec![(
            stem.to_owned(),
            blake3::hash(source_content.as_bytes()).to_hex().to_string(),
        )];
        fs::remove_file(source).unwrap();

        let result = execute_dream_with_expected(
            &lock,
            &storage,
            "## Preference\n\nPrivate preference",
            1,
            300,
            &sessions,
            &[stem.to_owned()],
            Some(Some("## Preference\n\nOriginal")),
            Some(&hashes),
        );
        assert!(matches!(result.status, DreamStatus::Failed(_)));
        assert_eq!(
            fs::read_to_string(storage.workspace_memory_file()).unwrap(),
            "## Preference\n\nOriginal"
        );
    }

    #[test]
    fn execute_dream_empty_response_nothing_to_consolidate() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());
        let (storage, _) = test_storage(&dir);
        let sdir = empty_sessions_dir(&dir);

        let result = execute_dream(&lock, &storage, "", 3, 300, &sdir, &[]);
        assert_eq!(result.status, DreamStatus::NothingToConsolidate);
        assert_eq!(result.sessions_eligible, 3);
        assert_eq!(result.cleaned_stems.len(), 0);
    }

    #[test]
    fn execute_dream_no_reply_nothing_to_consolidate() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());
        let (storage, _) = test_storage(&dir);
        let sdir = empty_sessions_dir(&dir);

        let result = execute_dream(&lock, &storage, "NO_REPLY", 2, 300, &sdir, &[]);
        assert_eq!(result.status, DreamStatus::NothingToConsolidate);
        assert_eq!(result.cleaned_stems.len(), 0);
    }

    #[test]
    fn execute_dream_lock_held_returns_skipped() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());
        let (storage, _) = test_storage(&dir);
        let sdir = empty_sessions_dir(&dir);

        // Acquire the lock (our own PID, non-stale)
        lock.try_acquire(300).unwrap().unwrap();

        let result = execute_dream(&lock, &storage, "## Topic\n\nContent", 5, 300, &sdir, &[]);
        match &result.status {
            DreamStatus::Skipped(reason) => {
                assert!(reason.contains("lock"), "reason: {reason}");
            }
            other => panic!("expected Skipped, got {other:?}"),
        }
        assert_eq!(result.sessions_eligible, 0);
        assert_eq!(result.cleaned_stems.len(), 0);
    }

    #[test]
    fn execute_dream_write_failure_rolls_back_lock() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());
        let sdir = empty_sessions_dir(&dir);

        // Point workspace dir at a path under a regular file, so
        // create_dir_all inside append_to_memory will fail (works
        // even when running as root, unlike chmod-based approaches).
        let blocker = dir.path().join("memory").join("blocker");
        fs::create_dir_all(blocker.parent().unwrap()).unwrap();
        fs::write(&blocker, "I am a file").unwrap();
        let workspace = blocker.join("impossible-subdir");

        let storage = MemoryStorage::with_paths(dir.path().join("memory"), workspace);

        let result = execute_dream(&lock, &storage, "## Topic\n\nContent", 3, 300, &sdir, &[]);

        match &result.status {
            DreamStatus::Failed(reason) => {
                assert!(reason.contains("MEMORY.md"), "reason: {reason}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(result.cleaned_stems.len(), 0);

        // Verify rollback: lock file should be gone (prior was None).
        let lock_path = dir.path().join(".dream-lock");
        assert!(
            !lock_path.exists(),
            "lock file should be deleted after rollback with no prior"
        );
    }

    #[test]
    fn execute_dream_overwrites_existing_memory() {
        let dir = TempDir::new().unwrap();
        let (storage, ws) = test_storage(&dir);
        let sdir = empty_sessions_dir(&dir);

        // Pre-populate MEMORY.md
        fs::create_dir_all(&ws).unwrap();
        fs::write(ws.join("MEMORY.md"), "## Existing\n\nOld content.").unwrap();

        let lock = DreamLock::new(dir.path());
        let response = "## New Topic\n\nFresh insight.";
        let result = execute_dream(&lock, &storage, response, 2, 300, &sdir, &[]);

        assert!(matches!(result.status, DreamStatus::Completed { .. }));

        let memory = fs::read_to_string(ws.join("MEMORY.md")).unwrap();
        // write_long_term overwrites: old content is replaced
        assert!(!memory.contains("Old content."));
        assert_eq!(memory.trim(), response);
    }

    // -------------------------------------------------------------------
    // DREAM_SYSTEM_PROMPT sanity checks
    // -------------------------------------------------------------------

    #[test]
    fn dream_system_prompt_has_required_content() {
        assert!(DREAM_SYSTEM_PROMPT.contains("dream"));
        assert!(DREAM_SYSTEM_PROMPT.contains("NO_REPLY"));
        assert!(DREAM_SYSTEM_PROMPT.contains("## headers"));
        // Core instruction verbs that define dream behavior
        assert!(DREAM_SYSTEM_PROMPT.contains("Merge"));
        assert!(DREAM_SYSTEM_PROMPT.contains("Resolve"));
        assert!(DREAM_SYSTEM_PROMPT.contains("Discard"));
        assert!(DREAM_SYSTEM_PROMPT.contains("Preserve"));
        assert!(DREAM_SYSTEM_PROMPT.contains("Convert"));
    }

    #[test]
    fn build_message_respects_input_size_cap() {
        let dir = TempDir::new().unwrap();
        let sessions = dir.path().join("sessions");
        let big_content = "x".repeat(MAX_DREAM_INPUT_CHARS - 32);
        write_session_content(&sessions, "big", &big_content);
        write_session_content(&sessions, "small", "should not appear");

        let stems = vec!["big".to_string(), "small".to_string()];
        let msg = build_dream_user_message(&sessions, &stems, None).unwrap();

        assert!(msg.content.chars().count() <= MAX_DREAM_INPUT_CHARS);
        assert!(
            !msg.content.contains("should not appear"),
            "second session should be skipped after cap"
        );
        // Only the first stem should be in processed_stems — the second
        // was beyond the cap and must NOT be archived.
        assert_eq!(msg.processed_stems, vec!["big"]);
    }

    #[test]
    fn normalize_memory_content_preserves_headers() {
        let input = "## Topic A\n\nContent A.\n\n## Topic B\n\nContent B.";
        let normalized = super::super::storage::normalize_memory_content(input);
        assert_eq!(normalized, input);
    }

    // -------------------------------------------------------------------
    // Session cleanup tests
    // -------------------------------------------------------------------

    #[test]
    fn cleanup_archives_processed_sessions_on_completed() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());
        let (storage, _ws) = test_storage(&dir);

        // Create session files that will be "processed" (old mtime to
        // pass the recency guard).
        let sessions = dir.path().join("sessions");
        write_old_session_content(&sessions, "sess-a", "Content A");
        write_old_session_content(&sessions, "sess-b", "Content B");

        let processed = vec!["sess-a".to_string(), "sess-b".to_string()];
        let response = "## Consolidated\n\nMerged content.";
        let result = execute_dream(&lock, &storage, response, 2, 300, &sessions, &processed);

        assert!(matches!(result.status, DreamStatus::Completed { .. }));
        assert_eq!(result.cleaned_stems.len(), 2);

        // Both files leave the active directory but remain recoverable.
        assert!(!sessions.join("sess-a.md").exists());
        assert!(!sessions.join("sess-b.md").exists());
        assert!(sessions.join("archive/sess-a.md").exists());
        assert!(sessions.join("archive/sess-b.md").exists());
    }

    #[test]
    fn cleanup_preserves_unprocessed_sessions() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());
        let (storage, _ws) = test_storage(&dir);

        // Create session files — only some will be "processed"
        let sessions = dir.path().join("sessions");
        write_old_session_content(&sessions, "processed", "Content");
        write_old_session_content(&sessions, "unprocessed", "Kept content");

        // Only mark "processed" as having been read
        let processed = vec!["processed".to_string()];
        let response = "## Consolidated\n\nMerged.";
        let result = execute_dream(&lock, &storage, response, 1, 300, &sessions, &processed);

        assert!(matches!(result.status, DreamStatus::Completed { .. }));
        assert_eq!(result.cleaned_stems.len(), 1);

        // Processed file should be gone, unprocessed should remain
        assert!(!sessions.join("processed.md").exists());
        assert!(sessions.join("unprocessed.md").exists());
        assert_eq!(
            fs::read_to_string(sessions.join("unprocessed.md")).unwrap(),
            "Kept content"
        );
    }

    #[test]
    fn cleanup_failure_does_not_affect_completed_status() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());
        let (storage, _ws) = test_storage(&dir);

        let sessions = dir.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();

        // Create a directory where a regular .md file is expected. The
        // archive path must reject it without relying on chmod. Back-date the dir's
        // mtime so it passes the recency guard.
        let bad_path = sessions.join("bad-stem.md");
        fs::create_dir_all(&bad_path).unwrap();
        let old = SystemTime::now() - Duration::from_secs(CLEANUP_RECENCY_GUARD_SECS + 60);
        filetime::set_file_mtime(&bad_path, FileTime::from_system_time(old)).unwrap();

        // Also create a normal file that CAN be cleaned
        write_old_session_content(&sessions, "good-stem", "Content");

        let processed = vec!["bad-stem".to_string(), "good-stem".to_string()];
        let response = "## Consolidated\n\nMerged.";
        let result = execute_dream(&lock, &storage, response, 2, 300, &sessions, &processed);

        // Status must still be Completed despite the cleanup failure
        assert!(matches!(result.status, DreamStatus::Completed { .. }));
        // Only the good-stem file was cleaned; bad-stem failed
        assert_eq!(result.cleaned_stems.len(), 1);
        // The directory-as-file still exists (cleanup failure)
        assert!(sessions.join("bad-stem.md").exists());
        // The normal file was successfully removed
        assert!(!sessions.join("good-stem.md").exists());
    }

    #[test]
    fn cleanup_skips_recently_modified_files() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());
        let (storage, _ws) = test_storage(&dir);

        let sessions = dir.path().join("sessions");
        // This file has the current mtime (within recency guard window)
        write_session_content(&sessions, "recent", "Still being written");
        // This file is old enough to be cleaned
        write_old_session_content(&sessions, "old", "Done");

        let processed = vec!["recent".to_string(), "old".to_string()];
        let response = "## Consolidated\n\nMerged.";
        let result = execute_dream(&lock, &storage, response, 2, 300, &sessions, &processed);

        assert!(matches!(result.status, DreamStatus::Completed { .. }));
        // Only the old file should be cleaned; recent should be skipped.
        // Crucially, cleaned_stems must contain ONLY "old" — if the
        // caller used this list for index cleanup, "recent" must NOT
        // have its index chunks removed since its file is still on disk.
        assert_eq!(result.cleaned_stems, vec!["old"]);
        assert!(
            sessions.join("recent.md").exists(),
            "recently-modified file must be preserved"
        );
        assert!(!sessions.join("old.md").exists());
    }

    #[test]
    fn no_cleanup_on_nothing_to_consolidate() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());
        let (storage, _ws) = test_storage(&dir);

        let sessions = dir.path().join("sessions");
        write_session_content(&sessions, "kept", "Content");

        let processed = vec!["kept".to_string()];
        // NO_REPLY → NothingToConsolidate — should NOT clean up
        let result = execute_dream(&lock, &storage, "NO_REPLY", 1, 300, &sessions, &processed);

        assert_eq!(result.status, DreamStatus::NothingToConsolidate);
        assert_eq!(result.cleaned_stems.len(), 0);
        assert!(
            sessions.join("kept.md").exists(),
            "session file must be preserved on NothingToConsolidate"
        );
    }

    #[test]
    fn no_cleanup_on_failed_status() {
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());

        // Force a write failure with the blocker trick
        let blocker = dir.path().join("memory").join("blocker");
        fs::create_dir_all(blocker.parent().unwrap()).unwrap();
        fs::write(&blocker, "I am a file").unwrap();
        let workspace = blocker.join("impossible-subdir");
        let storage = MemoryStorage::with_paths(dir.path().join("memory"), workspace);

        let sessions = dir.path().join("sessions");
        write_session_content(&sessions, "kept", "Content");

        let processed = vec!["kept".to_string()];
        let result = execute_dream(
            &lock,
            &storage,
            "## Topic\n\nContent",
            1,
            300,
            &sessions,
            &processed,
        );

        assert!(matches!(result.status, DreamStatus::Failed(_)));
        assert_eq!(result.cleaned_stems.len(), 0);
        assert!(
            sessions.join("kept.md").exists(),
            "session file must be preserved on Failed"
        );
    }

    #[test]
    fn build_message_processed_stems_excludes_capped_sessions() {
        let dir = TempDir::new().unwrap();
        let sessions = dir.path().join("sessions");

        // First session fills the buffer past the cap
        let big_content = "x".repeat(MAX_DREAM_INPUT_CHARS - 32);
        write_session_content(&sessions, "first", &big_content);
        // Second session should NOT be processed (over cap)
        write_session_content(&sessions, "second", "small content");
        // Third session should NOT be processed either
        write_session_content(&sessions, "third", "more content");

        let stems = vec![
            "first".to_string(),
            "second".to_string(),
            "third".to_string(),
        ];
        let msg = build_dream_user_message(&sessions, &stems, None).unwrap();

        assert_eq!(
            msg.processed_stems,
            vec!["first"],
            "only the first session should be in processed_stems"
        );
        assert_eq!(msg.processed_stems.len(), 1);
    }

    #[test]
    fn end_to_end_cap_boundary_cleanup() {
        // Integration test: an oversized log is retained while later complete
        // logs are consolidated and moved into the recoverable archive.
        let dir = TempDir::new().unwrap();
        let lock = DreamLock::new(dir.path());
        let (storage, _ws) = test_storage(&dir);

        let sessions = dir.path().join("sessions");

        // The first file cannot fit as a complete unit; the later small files can.
        write_old_session_content(
            &sessions,
            "aaa-first",
            &"a".repeat(MAX_DREAM_INPUT_CHARS + 1),
        );
        write_old_session_content(&sessions, "bbb-second", "small content 2");
        write_old_session_content(&sessions, "ccc-third", "small content 3");
        write_old_session_content(&sessions, "ddd-fourth", "small content 4");
        write_old_session_content(&sessions, "eee-fifth", "small content 5");

        let all_stems: Vec<String> = vec![
            "aaa-first",
            "bbb-second",
            "ccc-third",
            "ddd-fourth",
            "eee-fifth",
        ]
        .into_iter()
        .map(String::from)
        .collect();

        // Phase 1: skip the oversized file without starving later logs.
        let dream_msg = build_dream_user_message(&sessions, &all_stems, None).unwrap();
        assert_eq!(
            dream_msg.processed_stems,
            vec!["bbb-second", "ccc-third", "ddd-fourth", "eee-fifth"]
        );

        // Phase 2: execute_dream with a valid response should clean up
        // only the processed stems.
        let response = "## Consolidated\n\nMerged from 4 sessions.";
        let result = execute_dream(
            &lock,
            &storage,
            response,
            all_stems.len(),
            300,
            &sessions,
            &dream_msg.processed_stems,
        );

        assert!(matches!(result.status, DreamStatus::Completed { .. }));
        assert_eq!(result.cleaned_stems.len(), 4);

        // Oversized source is still active; processed files are archived.
        assert!(sessions.join("aaa-first.md").exists());
        assert!(!sessions.join("bbb-second.md").exists());
        assert!(!sessions.join("ccc-third.md").exists());
        assert!(!sessions.join("ddd-fourth.md").exists());
        assert!(!sessions.join("eee-fifth.md").exists());
        assert!(sessions.join("archive/bbb-second.md").exists());
        assert!(sessions.join("archive/eee-fifth.md").exists());
    }

    #[test]
    fn clean_processed_sessions_handles_already_missing_files() {
        let dir = TempDir::new().unwrap();
        let sessions = dir.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();

        // Only create one of two stems (old mtime to pass recency guard)
        write_old_session_content(&sessions, "exists", "Content");

        // Pass both — "missing" doesn't exist, should not error or count
        let cleaned =
            archive_processed_sessions(&sessions, &["exists".to_string(), "missing".to_string()]);

        assert_eq!(
            cleaned.len(),
            1,
            "only the existing file should count as cleaned"
        );
        assert_eq!(cleaned[0], "exists");
        assert!(!sessions.join("exists.md").exists());
        assert!(sessions.join("archive/exists.md").exists());
    }

    #[test]
    fn archive_collisions_keep_both_versions_and_the_complete_source_identity() {
        let dir = TempDir::new().unwrap();
        let sessions = dir.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        let stem = "2026-11-08-task--sid-opaque";
        for content in ["first snapshot", "second snapshot"] {
            write_old_session_content(&sessions, stem, content);
            assert_eq!(
                archive_processed_sessions(&sessions, &[stem.into()]),
                vec![stem]
            );
        }
        let archived: Vec<_> = fs::read_dir(sessions.join("archive"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(archived.len(), 2);
        let mut contents = Vec::new();
        for path in archived {
            assert!(crate::storage::session_log_path_belongs_to(&path, "opaque"));
            assert!(!crate::storage::session_log_path_belongs_to(
                &path,
                "opaque-neighbor"
            ));
            contents.push(fs::read_to_string(path).unwrap());
        }
        contents.sort();
        assert_eq!(contents, vec!["first snapshot", "second snapshot"]);
    }

    // -------------------------------------------------------------------
    // is_scaffold_template tests
    // -------------------------------------------------------------------

    #[test]
    fn scaffold_detects_old_workspace_template() {
        assert!(is_scaffold_template(
            "# Project Memory\n\n## Project Context\n\n<!-- Add project-specific knowledge here -->\n",
        ));
    }

    #[test]
    fn scaffold_detects_new_workspace_template() {
        assert!(is_scaffold_template(
            "# Project Memory — /test\n\n> Auto-populated by dream consolidation. Edit freely.\n",
        ));
    }

    #[test]
    fn scaffold_detects_global_template() {
        assert!(is_scaffold_template(
            "# Global Memory\n\n## Preferences\n\n<!-- Add any cross-project preferences here -->\n",
        ));
    }

    #[test]
    fn scaffold_rejects_real_content() {
        assert!(!is_scaffold_template(
            "## Decisions\n\nWe chose Rust.\n\n## Architecture\n\nEvent-driven.",
        ));
    }

    #[test]
    fn scaffold_boundary_499_bytes_is_scaffold() {
        let marker = "Add project-specific knowledge here";
        let pad_len = 499 - marker.len();
        let content = format!("{}{}", "x".repeat(pad_len), marker);
        assert_eq!(content.trim().len(), 499);
        assert!(
            is_scaffold_template(&content),
            "499-byte content with marker must be classified as scaffold"
        );
    }

    #[test]
    fn scaffold_boundary_500_bytes_is_not_scaffold() {
        let marker = "Add project-specific knowledge here";
        let pad_len = 500 - marker.len();
        let content = format!("{}{}", "x".repeat(pad_len), marker);
        assert_eq!(content.trim().len(), 500);
        assert!(
            !is_scaffold_template(&content),
            "500-byte content with marker must NOT be classified as scaffold"
        );
    }

    #[test]
    fn scaffold_rejects_large_content_with_leftover_marker() {
        let real_content = "x".repeat(600);
        let content_with_marker = format!(
            "# Project Memory\n\n<!-- Add project-specific knowledge here -->\n\n## Decisions\n\n{}",
            real_content
        );
        assert!(
            !is_scaffold_template(&content_with_marker),
            "large file with leftover scaffold marker must NOT be classified as scaffold"
        );
    }

    #[test]
    fn scaffold_rejects_large_workspace_template_with_real_content() {
        let real_content = "a ".repeat(300);
        let content = format!(
            "# Project Memory — /test\n\n> Auto-populated by dream consolidation. Edit freely.\n\n## Architecture\n\n{}",
            real_content
        );
        assert!(
            !is_scaffold_template(&content),
            "workspace template with substantial appended content must not be scaffold"
        );
    }

    // -------------------------------------------------------------------
    // build_dream_user_message with existing memory tests
    // -------------------------------------------------------------------

    #[test]
    fn build_message_prepends_existing_memory() {
        let dir = TempDir::new().unwrap();
        let sessions = dir.path().join("sessions");
        write_session_content(&sessions, "sess-a", "Decision: use Rust");

        let stems = vec!["sess-a".to_string()];
        let existing = "## Prior\n\nOld knowledge.";
        let msg = build_dream_user_message(&sessions, &stems, Some(existing)).unwrap();

        assert!(msg.content.starts_with("--- Existing Memory"));
        assert!(msg.content.contains("Old knowledge."));
        assert!(msg.content.contains("Decision: use Rust"));
        let mem_pos = msg.content.find("Old knowledge.").unwrap();
        let sess_pos = msg.content.find("Decision: use Rust").unwrap();
        assert!(mem_pos < sess_pos, "existing memory must precede sessions");
    }

    #[test]
    fn build_message_skips_scaffold_existing_memory() {
        let dir = TempDir::new().unwrap();
        let sessions = dir.path().join("sessions");
        write_session_content(&sessions, "sess-a", "Decision: use Rust");

        let stems = vec!["sess-a".to_string()];
        let scaffold = "# Project Memory\n\n<!-- Add project-specific knowledge here -->\n";
        let msg = build_dream_user_message(&sessions, &stems, Some(scaffold)).unwrap();

        assert!(!msg.content.contains("Existing Memory"));
        assert!(msg.content.contains("Decision: use Rust"));
    }

    #[test]
    fn build_message_includes_large_memory_with_scaffold_marker() {
        let dir = TempDir::new().unwrap();
        let sessions = dir.path().join("sessions");
        write_session_content(&sessions, "sess-a", "Decision: use Rust");

        let stems = vec!["sess-a".to_string()];
        let real_content = "x".repeat(600);
        let memory = format!(
            "# Project Memory\n\n<!-- Add project-specific knowledge here -->\n\n## Architecture\n\n{}",
            real_content
        );
        let msg = build_dream_user_message(&sessions, &stems, Some(&memory)).unwrap();

        assert!(
            msg.content.contains("Existing Memory"),
            "large memory with leftover scaffold marker must be included"
        );
        assert!(msg.content.contains(&real_content));
    }

    #[test]
    fn build_message_returns_none_with_existing_memory_but_no_sessions() {
        let dir = TempDir::new().unwrap();
        let sessions = dir.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();

        let stems = vec!["missing".to_string()];
        let existing = "## Prior\n\nOld knowledge.";
        assert!(
            build_dream_user_message(&sessions, &stems, Some(existing)).is_none(),
            "should return None when no sessions are readable, even with existing memory"
        );
    }
}
