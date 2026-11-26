#![allow(
    unused_imports,
    unused_variables,
    unused_mut,
    unreachable_code,
    dead_code
)]
//! Bounded, metadata-only listing of foreign coding-agent sessions.
//! Foreign SQLite stores are opened only when `echo_agent_sqlite_journal::JournalMode`
//! selects local WAL. The direct read-only/query-only transaction makes no
//! logical writes, though WAL coordination may update shared-memory read marks.
//! Network filesystems fail soft before SQLite open, conversion, or writes.
use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
mod capability;
mod claude;
mod codex;
use capability::{ApprovedRoot, open_sqlite_transaction};
pub const MAX_SESSIONS_PER_TOOL: usize = 50;
pub const MAX_SESSION_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
pub const MAX_TITLE_CHARS: usize = 200;
const MAX_FUTURE_SKEW: Duration = Duration::from_secs(5 * 60);
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ForeignSessionTool {
    Claude,
    Codex,
    Cursor,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ForeignSessionSource {
    ClaudeCode,
    CodexCli,
    CodexVsCode,
    CodexAtlas,
    CodexChatGpt,
    CursorDesktop,
    CursorCli,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignSessionSummary {
    pub tool: ForeignSessionTool,
    pub source: ForeignSessionSource,
    pub native_id: String,
    pub title: String,
    pub cwd: PathBuf,
    pub updated_at: SystemTime,
    pub branch: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentForeignSession {
    pub tool: ForeignSessionTool,
    pub native_id: String,
    pub age: Duration,
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct RecentCandidate {
    tool: ForeignSessionTool,
    source: ForeignSessionSource,
    native_id: String,
    updated_at: SystemTime,
}
#[derive(Debug, Clone, PartialEq, Eq)]
enum RecentProbe<T> {
    Complete(Option<T>),
    Incomplete,
}
fn approved_root_for_recent(path: &Path) -> Result<Option<ApprovedRoot>, ()> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
        Ok(_) => ApprovedRoot::new(path).map(Some).ok_or(()),
    }
}
#[cfg(test)]
impl<T> RecentProbe<T> {
    fn unwrap(self) -> T {
        match self {
            Self::Complete(Some(value)) => value,
            Self::Complete(None) => {
                panic!("called RecentProbe::unwrap on complete-empty probe")
            }
            Self::Incomplete => panic!("called RecentProbe::unwrap on incomplete probe"),
        }
    }
    fn is_none(&self) -> bool {
        matches!(self, Self::Complete(None))
    }
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EnabledForeignSessionSources {
    pub claude: bool,
    pub codex: bool,
    pub cursor: bool,
}
pub fn scan_foreign_sessions(
    cwd: &Path,
    enabled: EnabledForeignSessionSources,
) -> Vec<ForeignSessionSummary> {
    let scan_cursor = |_: &Path, _: SystemTime| Vec::new();
    scan_with(cwd, enabled, claude::scan, codex::scan, scan_cursor)
}
pub fn most_recent_foreign_session(
    cwd: &Path,
    enabled: EnabledForeignSessionSources,
    within: Duration,
) -> Option<RecentForeignSession> {
    let recent_cursor = |_: &Path, _: SystemTime, _: Duration| RecentProbe::Complete(None);
    match most_recent_with(
        cwd,
        enabled,
        within,
        SystemTime::now(),
        claude::most_recent,
        codex::most_recent,
        recent_cursor,
    ) {
        RecentProbe::Complete(session) => session,
        RecentProbe::Incomplete => None,
    }
}
fn most_recent_with<Claude, Codex, Cursor>(
    cwd: &Path,
    enabled: EnabledForeignSessionSources,
    within: Duration,
    now: SystemTime,
    recent_claude: Claude,
    recent_codex: Codex,
    recent_cursor: Cursor,
) -> RecentProbe<RecentForeignSession>
where
    Claude: FnOnce(&Path, SystemTime, Duration) -> RecentProbe<RecentCandidate>,
    Codex: FnOnce(&Path, SystemTime, Duration) -> RecentProbe<RecentCandidate>,
    Cursor: FnOnce(&Path, SystemTime, Duration) -> RecentProbe<RecentCandidate>,
{
    if !enabled.claude && !enabled.codex && !enabled.cursor {
        return RecentProbe::Complete(None);
    }
    let Ok(cwd) = dunce::canonicalize(cwd) else {
        return RecentProbe::Complete(None);
    };
    let mut candidates = Vec::with_capacity(3);
    if enabled.claude {
        match recent_claude(&cwd, now, within) {
            RecentProbe::Complete(candidate) => candidates.extend(candidate),
            RecentProbe::Incomplete => return RecentProbe::Incomplete,
        }
    }
    if enabled.codex {
        match recent_codex(&cwd, now, within) {
            RecentProbe::Complete(candidate) => candidates.extend(candidate),
            RecentProbe::Incomplete => return RecentProbe::Incomplete,
        }
    }
    if enabled.cursor {
        match recent_cursor(&cwd, now, within) {
            RecentProbe::Complete(candidate) => candidates.extend(candidate),
            RecentProbe::Incomplete => return RecentProbe::Incomplete,
        }
    }
    let winner = candidates
        .into_iter()
        .filter(|candidate| is_within(candidate.updated_at, now, within))
        .min_by(recent_candidate_order);
    RecentProbe::Complete(winner.map(|winner| {
        RecentForeignSession {
            tool: winner.tool,
            native_id: winner.native_id,
            age: now
                .duration_since(winner.updated_at)
                .unwrap_or(Duration::ZERO),
        }
    }))
}
fn recent_candidate_order(a: &RecentCandidate, b: &RecentCandidate) -> Ordering {
    b.updated_at
        .cmp(&a.updated_at)
        .then_with(|| a.tool.cmp(&b.tool))
        .then_with(|| a.native_id.cmp(&b.native_id))
        .then_with(|| a.source.cmp(&b.source))
}
fn scan_with<Claude, Codex, Cursor>(
    cwd: &Path,
    enabled: EnabledForeignSessionSources,
    mut scan_claude: Claude,
    mut scan_codex: Codex,
    mut scan_cursor: Cursor,
) -> Vec<ForeignSessionSummary>
where
    Claude: FnMut(&Path, SystemTime) -> Vec<ForeignSessionSummary>,
    Codex: FnMut(&Path, SystemTime) -> Vec<ForeignSessionSummary>,
    Cursor: FnMut(&Path, SystemTime) -> Vec<ForeignSessionSummary>,
{
    if !enabled.claude && !enabled.codex && !enabled.cursor {
        return Vec::new();
    }
    let canonical_cwd = dunce::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let mut cwd_spellings = vec![canonical_cwd];
    if cwd_spellings[0].as_path() != cwd {
        cwd_spellings.push(cwd.to_path_buf());
    }
    let now = SystemTime::now();
    let mut sessions = Vec::new();
    if enabled.claude {
        let mut tool_sessions = Vec::new();
        for cwd in &cwd_spellings {
            tool_sessions.extend(scan_claude(cwd, now));
        }
        sessions.extend(finish_tool_scan(tool_sessions));
    }
    if enabled.codex {
        let mut tool_sessions = Vec::new();
        for cwd in &cwd_spellings {
            tool_sessions.extend(scan_codex(cwd, now));
        }
        sessions.extend(finish_tool_scan(tool_sessions));
    }
    if enabled.cursor {
        let mut tool_sessions = Vec::new();
        for cwd in &cwd_spellings {
            tool_sessions.extend(scan_cursor(cwd, now));
        }
        sessions.extend(finish_tool_scan(tool_sessions));
    }
    sort_sessions(&mut sessions);
    sessions
}
fn sort_sessions(sessions: &mut [ForeignSessionSummary]) {
    sessions.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.tool.cmp(&b.tool))
            .then_with(|| a.native_id.cmp(&b.native_id))
            .then_with(|| a.source.cmp(&b.source))
            .then_with(|| a.title.cmp(&b.title))
            .then_with(|| a.cwd.cmp(&b.cwd))
    });
}
pub(crate) fn finish_tool_scan(
    mut sessions: Vec<ForeignSessionSummary>,
) -> Vec<ForeignSessionSummary> {
    sort_sessions(&mut sessions);
    let mut seen = HashSet::new();
    sessions.retain(|session| seen.insert(session.native_id.clone()));
    sessions.truncate(MAX_SESSIONS_PER_TOOL);
    sessions
}
pub(crate) fn retain_top_k_by<T>(
    candidates: &mut Vec<T>,
    candidate: T,
    limit: usize,
    compare: impl Fn(&T, &T) -> Ordering,
) {
    if limit == 0 {
        return;
    }
    if candidates.len() == limit {
        if !candidates
            .last()
            .is_some_and(|worst| compare(worst, &candidate).is_gt())
        {
            return;
        }
        candidates.pop();
    }
    let index = candidates
        .binary_search_by(|existing| compare(existing, &candidate))
        .unwrap_or_else(|index| index);
    candidates.insert(index, candidate);
}
pub(crate) fn is_within(updated_at: SystemTime, now: SystemTime, within: Duration) -> bool {
    match now.duration_since(updated_at) {
        Ok(age) => age <= within,
        Err(future) => future.duration() <= MAX_FUTURE_SKEW,
    }
}
pub(crate) fn system_time_from_millis(millis: i64) -> Option<SystemTime> {
    let millis = u64::try_from(millis).ok()?;
    UNIX_EPOCH.checked_add(Duration::from_millis(millis))
}
pub(crate) fn millis_from_system_time(time: SystemTime) -> Option<i64> {
    let millis = time.duration_since(UNIX_EPOCH).ok()?.as_millis();
    i64::try_from(millis).ok()
}
pub(crate) fn millis_bounds(now: SystemTime, within: Duration) -> Option<(i64, i64)> {
    Some((
        millis_from_system_time(now.checked_sub(within)?)?,
        millis_from_system_time(now.checked_add(MAX_FUTURE_SKEW)?)?,
    ))
}
pub(crate) fn normalize_title(value: &str) -> Option<String> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    let mut chars = normalized.chars();
    let prefix: String = chars.by_ref().take(MAX_TITLE_CHARS).collect();
    if chars.next().is_none() {
        Some(prefix)
    } else {
        let mut truncated: String = prefix
            .chars()
            .take(MAX_TITLE_CHARS.saturating_sub(1))
            .collect();
        truncated.push('…');
        Some(truncated)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    fn summary(id: &str, updated_at: SystemTime) -> ForeignSessionSummary {
        ForeignSessionSummary {
            tool: ForeignSessionTool::Claude,
            source: ForeignSessionSource::ClaudeCode,
            native_id: id.to_owned(),
            title: id.to_owned(),
            cwd: PathBuf::from("/repo"),
            updated_at,
            branch: None,
        }
    }
    fn recent_candidate(
        tool: ForeignSessionTool,
        source: ForeignSessionSource,
        id: &str,
        updated_at: SystemTime,
    ) -> RecentCandidate {
        RecentCandidate {
            tool,
            source,
            native_id: id.to_owned(),
            updated_at,
        }
    }
    fn complete_candidate(
        tool: ForeignSessionTool,
        source: ForeignSessionSource,
        id: &str,
        updated_at: SystemTime,
    ) -> RecentProbe<RecentCandidate> {
        RecentProbe::Complete(Some(recent_candidate(tool, source, id, updated_at)))
    }
    #[test]
    fn recent_winner_is_newest_across_tools_with_deterministic_ties() {
        let now = UNIX_EPOCH + Duration::from_secs(10_000);
        let within = Duration::from_secs(600);
        let root = tempfile::tempdir().unwrap();
        let cwd = dunce::canonicalize(root.path()).unwrap();
        let enabled = EnabledForeignSessionSources {
            claude: true,
            codex: true,
            cursor: true,
        };
        let winner = most_recent_with(
            &cwd,
            enabled,
            within,
            now,
            |_, _, _| {
                complete_candidate(
                    ForeignSessionTool::Claude,
                    ForeignSessionSource::ClaudeCode,
                    "claude",
                    now - Duration::from_secs(3),
                )
            },
            |_, _, _| {
                complete_candidate(
                    ForeignSessionTool::Codex,
                    ForeignSessionSource::CodexCli,
                    "codex",
                    now - Duration::from_secs(1),
                )
            },
            |_, _, _| {
                complete_candidate(
                    ForeignSessionTool::Cursor,
                    ForeignSessionSource::CursorDesktop,
                    "cursor",
                    now - Duration::from_secs(2),
                )
            },
        )
        .unwrap();
        assert_eq!(winner.tool, ForeignSessionTool::Codex);
        assert_eq!(winner.native_id, "codex");
        let tied = most_recent_with(
            &cwd,
            enabled,
            within,
            now,
            |_, _, _| {
                complete_candidate(
                    ForeignSessionTool::Claude,
                    ForeignSessionSource::ClaudeCode,
                    "claude",
                    now,
                )
            },
            |_, _, _| {
                complete_candidate(
                    ForeignSessionTool::Codex,
                    ForeignSessionSource::CodexCli,
                    "codex",
                    now,
                )
            },
            |_, _, _| {
                complete_candidate(
                    ForeignSessionTool::Cursor,
                    ForeignSessionSource::CursorDesktop,
                    "cursor",
                    now,
                )
            },
        )
        .unwrap();
        assert_eq!(tied.tool, ForeignSessionTool::Claude);
        assert_eq!(tied.native_id, "claude");
    }
    #[test]
    fn recent_window_includes_cutoff_and_clamps_safe_future_age() {
        let now = UNIX_EPOCH + Duration::from_secs(10_000);
        let within = Duration::from_secs(600);
        let root = tempfile::tempdir().unwrap();
        let cwd = dunce::canonicalize(root.path()).unwrap();
        let claude_only = EnabledForeignSessionSources {
            claude: true,
            ..Default::default()
        };
        let at_cutoff = most_recent_with(
            &cwd,
            claude_only,
            within,
            now,
            |_, _, _| {
                complete_candidate(
                    ForeignSessionTool::Claude,
                    ForeignSessionSource::ClaudeCode,
                    "cutoff",
                    now - within,
                )
            },
            |_, _, _| -> RecentProbe<RecentCandidate> { panic!("disabled codex store touched") },
            |_, _, _| -> RecentProbe<RecentCandidate> { panic!("disabled cursor store touched") },
        )
        .unwrap();
        assert_eq!(at_cutoff.age, within);
        let future = most_recent_with(
            &cwd,
            claude_only,
            within,
            now,
            |_, _, _| {
                complete_candidate(
                    ForeignSessionTool::Claude,
                    ForeignSessionSource::ClaudeCode,
                    "future",
                    now + MAX_FUTURE_SKEW,
                )
            },
            |_, _, _| -> RecentProbe<RecentCandidate> { panic!("disabled codex store touched") },
            |_, _, _| -> RecentProbe<RecentCandidate> { panic!("disabled cursor store touched") },
        )
        .unwrap();
        assert_eq!(future.age, Duration::ZERO);
        assert!(
            most_recent_with(
                &cwd,
                claude_only,
                within,
                now,
                |_, _, _| {
                    complete_candidate(
                        ForeignSessionTool::Claude,
                        ForeignSessionSource::ClaudeCode,
                        "too-far-future",
                        now + MAX_FUTURE_SKEW + Duration::from_secs(1),
                    )
                },
                |_, _, _| -> RecentProbe<RecentCandidate> {
                    panic!("disabled codex store touched")
                },
                |_, _, _| -> RecentProbe<RecentCandidate> {
                    panic!("disabled cursor store touched")
                },
            )
            .is_none()
        );
    }
    #[test]
    fn recent_scan_never_touches_disabled_tool_stores() {
        let now = UNIX_EPOCH + Duration::from_secs(10_000);
        let root = tempfile::tempdir().unwrap();
        let cwd = dunce::canonicalize(root.path()).unwrap();
        let calls = Cell::new((0, 0, 0));
        let found = most_recent_with(
            &cwd,
            EnabledForeignSessionSources {
                codex: true,
                ..Default::default()
            },
            Duration::from_secs(600),
            now,
            |_, _, _| {
                let (_, codex, cursor) = calls.get();
                calls.set((1, codex, cursor));
                RecentProbe::Complete(None)
            },
            |_, _, _| {
                let (claude, _, cursor) = calls.get();
                calls.set((claude, 1, cursor));
                complete_candidate(
                    ForeignSessionTool::Codex,
                    ForeignSessionSource::CodexCli,
                    "codex",
                    now,
                )
            },
            |_, _, _| {
                let (claude, codex, _) = calls.get();
                calls.set((claude, codex, 1));
                RecentProbe::Complete(None)
            },
        );
        assert_eq!(calls.get(), (0, 1, 0));
        assert_eq!(found.unwrap().tool, ForeignSessionTool::Codex);
    }
    #[test]
    fn incomplete_enabled_tool_suppresses_cross_tool_winner() {
        let now = UNIX_EPOCH + Duration::from_secs(10_000);
        let root = tempfile::tempdir().unwrap();
        let cwd = dunce::canonicalize(root.path()).unwrap();
        let result = most_recent_with(
            &cwd,
            EnabledForeignSessionSources {
                claude: true,
                codex: true,
                cursor: true,
            },
            Duration::from_secs(600),
            now,
            |_, _, _| RecentProbe::<RecentCandidate>::Incomplete,
            |_, _, _| {
                complete_candidate(
                    ForeignSessionTool::Codex,
                    ForeignSessionSource::CodexCli,
                    "codex",
                    now,
                )
            },
            |_, _, _| RecentProbe::Complete(None),
        );
        assert_eq!(result, RecentProbe::Incomplete);
    }
    #[test]
    fn recent_scan_normalizes_cwd_before_store_access() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("repo");
        let child = cwd.join("child");
        std::fs::create_dir_all(&child).unwrap();
        let expected = dunce::canonicalize(&cwd).unwrap();
        let spelled = child.join("..");
        let found = most_recent_with(
            &spelled,
            EnabledForeignSessionSources {
                codex: true,
                ..Default::default()
            },
            Duration::from_secs(600),
            SystemTime::now(),
            |_, _, _| -> RecentProbe<RecentCandidate> { panic!("disabled claude store touched") },
            |received, _, _| {
                assert_eq!(received, expected);
                RecentProbe::Complete(None)
            },
            |_, _, _| -> RecentProbe<RecentCandidate> { panic!("disabled cursor store touched") },
        );
        assert!(found.is_none());
    }
    #[test]
    fn recent_scan_canonicalization_failure_never_invokes_stores() {
        let calls = Cell::new(0);
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing");
        let found = most_recent_with(
            &missing,
            EnabledForeignSessionSources {
                claude: true,
                codex: true,
                cursor: true,
            },
            Duration::from_secs(600),
            SystemTime::now(),
            |_, _, _| {
                calls.set(calls.get() + 1);
                RecentProbe::Complete(None)
            },
            |_, _, _| {
                calls.set(calls.get() + 1);
                RecentProbe::Complete(None)
            },
            |_, _, _| {
                calls.set(calls.get() + 1);
                RecentProbe::Complete(None)
            },
        );
        assert!(found.is_none());
        assert_eq!(calls.get(), 0);
    }
    #[test]
    fn disabled_sources_do_not_invoke_scanners() {
        let sessions = scan_with(
            Path::new("/repo"),
            EnabledForeignSessionSources::default(),
            |_, _| panic!("claude scanner called"),
            |_, _| panic!("codex scanner called"),
            |_, _| panic!("cursor scanner called"),
        );
        assert!(sessions.is_empty());
    }
    #[test]
    fn only_enabled_sources_are_invoked() {
        let claude_calls = Cell::new(0);
        let codex_calls = Cell::new(0);
        let cursor_calls = Cell::new(0);
        scan_with(
            Path::new("/repo"),
            EnabledForeignSessionSources {
                codex: true,
                ..Default::default()
            },
            |_, _| {
                claude_calls.set(claude_calls.get() + 1);
                Vec::new()
            },
            |_, _| {
                codex_calls.set(codex_calls.get() + 1);
                Vec::new()
            },
            |_, _| {
                cursor_calls.set(cursor_calls.get() + 1);
                Vec::new()
            },
        );
        assert_eq!(
            (claude_calls.get(), codex_calls.get(), cursor_calls.get()),
            (0, 1, 0)
        );
    }
    #[test]
    fn finish_scan_deduplicates_sorts_and_caps() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let mut sessions = (0..55)
            .map(|i| summary(&format!("{i:02}"), now + Duration::from_secs(i)))
            .collect::<Vec<_>>();
        sessions.push(summary("54", now + Duration::from_secs(500)));
        let sessions = finish_tool_scan(sessions);
        assert_eq!(sessions.len(), MAX_SESSIONS_PER_TOOL);
        assert_eq!(sessions[0].native_id, "54");
        assert!(
            sessions
                .windows(2)
                .all(|pair| pair[0].updated_at >= pair[1].updated_at)
        );
    }
    #[test]
    fn top_k_helper_uses_comparator_ties() {
        let mut candidates = Vec::with_capacity(3);
        for candidate in [(1, "z"), (3, "c"), (3, "a"), (2, "b"), (4, "d"), (3, "b")] {
            retain_top_k_by(&mut candidates, candidate, 3, |a, b| {
                b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1))
            });
        }
        assert_eq!(candidates, vec![(4, "d"), (3, "a"), (3, "b")]);
    }
    #[test]
    fn title_truncation_is_utf8_safe_and_bounded() {
        let title = normalize_title(&"é".repeat(250)).unwrap();
        assert_eq!(title.chars().count(), MAX_TITLE_CHARS);
        assert!(title.ends_with('…'));
    }
    #[test]
    fn recency_allows_thirty_days_and_only_small_future_skew() {
        let now = UNIX_EPOCH + Duration::from_secs(4_000_000);
        assert!(is_within(now - MAX_SESSION_AGE, now, MAX_SESSION_AGE));
        assert!(!is_within(
            now - MAX_SESSION_AGE - Duration::from_secs(1),
            now,
            MAX_SESSION_AGE,
        ));
        assert!(is_within(now + MAX_FUTURE_SKEW, now, MAX_SESSION_AGE));
        assert!(!is_within(
            now + MAX_FUTURE_SKEW + Duration::from_secs(1),
            now,
            MAX_SESSION_AGE,
        ));
    }
    #[test]
    fn enabled_scanners_receive_canonical_and_supplied_cwd_spellings() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("repo");
        let child = cwd.join("child");
        std::fs::create_dir_all(&child).unwrap();
        let expected = dunce::canonicalize(&cwd).unwrap();
        let spelled = child.join("..");
        let received = RefCell::new(Vec::new());
        scan_with(
            &spelled,
            EnabledForeignSessionSources {
                codex: true,
                ..Default::default()
            },
            |_, _| panic!("claude scanner called"),
            |cwd, _| {
                received.borrow_mut().push(cwd.to_path_buf());
                Vec::new()
            },
            |_, _| panic!("cursor scanner called"),
        );
        assert_eq!(
            received.into_inner(),
            vec![expected.clone(), spelled.clone()]
        );
        #[cfg(unix)]
        {
            let link = root.path().join("linked-repo");
            std::os::unix::fs::symlink(&cwd, &link).unwrap();
            let received = RefCell::new(Vec::new());
            scan_with(
                &link,
                EnabledForeignSessionSources {
                    cursor: true,
                    ..Default::default()
                },
                |_, _| panic!("claude scanner called"),
                |_, _| panic!("codex scanner called"),
                |cwd, _| {
                    received.borrow_mut().push(cwd.to_path_buf());
                    Vec::new()
                },
            );
            assert_eq!(received.into_inner(), vec![expected, link]);
        }
    }
    #[cfg(windows)]
    #[test]
    fn normalized_cwd_uses_ordinary_windows_spelling() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        scan_with(
            &cwd,
            EnabledForeignSessionSources {
                codex: true,
                ..Default::default()
            },
            |_, _| panic!("claude scanner called"),
            |received, _| {
                assert_eq!(received, dunce::canonicalize(&cwd).unwrap());
                assert!(!received.to_string_lossy().starts_with(r"\\?\"));
                Vec::new()
            },
            |_, _| panic!("cursor scanner called"),
        );
    }
}
