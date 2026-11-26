use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::Value;

use super::{
    ApprovedRoot, ForeignSessionSource, ForeignSessionSummary, ForeignSessionTool, MAX_SESSION_AGE,
    MAX_SESSIONS_PER_TOOL, RecentCandidate, RecentProbe, approved_root_for_recent,
    finish_tool_scan, is_within, normalize_title, retain_top_k_by,
};

mod projects;

const READ_CHUNK: usize = 64 * 1024;
const MAX_HEAD: usize = 4 * 1024 * 1024;
const MAX_CONTENT_READS: usize = 128;
const MAX_RECENT_CONTENT_READS: usize = 16;
const MAX_RECENT_DIRECTORY_ENTRIES: usize = 64;

struct Candidate {
    root: ApprovedRoot,
    path: PathBuf,
    session_id: String,
    modified: SystemTime,
    size: u64,
}

pub(super) fn scan(cwd: &Path, now: SystemTime) -> Vec<ForeignSessionSummary> {
    let Some(config_dir) = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".claude")))
    else {
        return Vec::new();
    };
    scan_in_config_dir(&config_dir, cwd, now)
}

pub(super) fn most_recent(
    cwd: &Path,
    now: SystemTime,
    within: Duration,
) -> RecentProbe<RecentCandidate> {
    let Some(config_dir) = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".claude")))
    else {
        return RecentProbe::Complete(None);
    };
    most_recent_in_config_dir(&config_dir, cwd, now, within)
}

fn scan_in_config_dir(
    config_dir: &Path,
    cwd: &Path,
    now: SystemTime,
) -> Vec<ForeignSessionSummary> {
    let Some(root) = ApprovedRoot::new(config_dir) else {
        return Vec::new();
    };
    let project_dirs = projects::scoped_project_dirs(root.path(), cwd);
    scan_project_dirs(&root, &project_dirs, cwd, now)
}

fn most_recent_in_config_dir(
    config_dir: &Path,
    cwd: &Path,
    now: SystemTime,
    within: Duration,
) -> RecentProbe<RecentCandidate> {
    let root = match approved_root_for_recent(config_dir) {
        Ok(Some(root)) => root,
        Ok(None) => return RecentProbe::Complete(None),
        Err(()) => return RecentProbe::Incomplete,
    };
    let Some(project_dir) = projects::project_dir_path(root.path(), cwd) else {
        return RecentProbe::Complete(None);
    };
    match std::fs::symlink_metadata(&project_dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return RecentProbe::Complete(None);
        }
        Err(_) => return RecentProbe::Incomplete,
        Ok(metadata) if project_directory_is_safe(&metadata) => {}
        Ok(_) => return RecentProbe::Incomplete,
    }

    #[cfg(unix)]
    let collected = root
        .subroot(&project_dir)
        .and_then(|project_root| collect_recent_candidates(&project_root, now, within));
    #[cfg(windows)]
    let collected = collect_recent_candidates_windows(&root, &project_dir, now, within);
    #[cfg(not(any(unix, windows)))]
    let collected = None;

    let Some((candidates, truncated)) = collected else {
        return RecentProbe::Incomplete;
    };
    finish_recent_candidates(candidates, truncated, cwd)
}

fn finish_recent_candidates(
    candidates: Vec<Candidate>,
    truncated: bool,
    cwd: &Path,
) -> RecentProbe<RecentCandidate> {
    let candidate = candidates.into_iter().find_map(|candidate| {
        qualify_candidate(&candidate, cwd)?;
        Some(RecentCandidate {
            tool: ForeignSessionTool::Claude,
            source: ForeignSessionSource::ClaudeCode,
            native_id: candidate.session_id,
            updated_at: candidate.modified,
        })
    });
    if candidate.is_none() && truncated {
        RecentProbe::Incomplete
    } else {
        RecentProbe::Complete(candidate)
    }
}

fn project_directory_is_safe(metadata: &std::fs::Metadata) -> bool {
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return false;
        }
    }
    true
}

fn collect_recent_candidates(
    project_root: &ApprovedRoot,
    now: SystemTime,
    within: Duration,
) -> Option<(Vec<Candidate>, bool)> {
    let mut candidates = Vec::with_capacity(MAX_RECENT_CONTENT_READS);
    let mut qualifying = 0;
    let outcome = project_root.for_each_entry_bounded(MAX_RECENT_DIRECTORY_ENTRIES, |name| {
        let path = project_root.join(&name);
        let Some(session_id) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".jsonl"))
            .filter(|id| uuid::Uuid::try_parse(id).is_ok())
        else {
            return;
        };
        let Some((path, metadata)) = project_root.resolve_regular_file(&path) else {
            return;
        };
        if metadata.len() == 0 {
            return;
        }
        let Ok(modified) = metadata.modified() else {
            return;
        };
        if is_within(modified, now, within) {
            qualifying += 1;
            retain_top_k_by(
                &mut candidates,
                Candidate {
                    root: project_root.clone(),
                    path,
                    session_id: session_id.to_owned(),
                    modified,
                    size: metadata.len(),
                },
                MAX_RECENT_CONTENT_READS,
                candidate_order,
            );
        }
    });
    outcome
        .complete
        .then_some((candidates, qualifying > MAX_RECENT_CONTENT_READS))
}

#[cfg(windows)]
fn collect_recent_candidates_windows(
    config_root: &ApprovedRoot,
    project_dir: &Path,
    now: SystemTime,
    within: Duration,
) -> Option<(Vec<Candidate>, bool)> {
    let canonical_project = dunce::canonicalize(project_dir).ok()?;
    if canonical_project.as_path() != project_dir
        || !canonical_project.starts_with(config_root.path())
    {
        return None;
    }
    let project_root = ApprovedRoot::new(&canonical_project)?;
    if project_root.path() != canonical_project.as_path() {
        return None;
    }
    let mut entries = std::fs::read_dir(&canonical_project).ok()?;
    let mut candidates = Vec::with_capacity(MAX_RECENT_CONTENT_READS);
    let mut qualifying = 0;
    for _ in 0..MAX_RECENT_DIRECTORY_ENTRIES {
        let Some(entry) = entries.next() else {
            return Some((candidates, qualifying > MAX_RECENT_CONTENT_READS));
        };
        let entry = entry.ok()?;
        let path = entry.path();
        let Some(session_id) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".jsonl"))
            .filter(|id| uuid::Uuid::try_parse(id).is_ok())
        else {
            continue;
        };
        let opened = project_root.open_regular_file(&path)?;
        if opened.metadata.len() == 0 {
            continue;
        }
        let modified = opened.metadata.modified().ok()?;
        if is_within(modified, now, within) {
            qualifying += 1;
            retain_top_k_by(
                &mut candidates,
                Candidate {
                    root: project_root.clone(),
                    path: opened.path,
                    session_id: session_id.to_owned(),
                    modified,
                    size: opened.metadata.len(),
                },
                MAX_RECENT_CONTENT_READS,
                candidate_order,
            );
        }
    }
    match entries.next() {
        None => Some((candidates, qualifying > MAX_RECENT_CONTENT_READS)),
        Some(_) => None,
    }
}

fn scan_project_dirs(
    root: &ApprovedRoot,
    project_dirs: &[PathBuf],
    cwd: &Path,
    now: SystemTime,
) -> Vec<ForeignSessionSummary> {
    let candidates =
        collect_candidates(root, project_dirs, now, MAX_SESSION_AGE, MAX_CONTENT_READS);
    let mut accepted_ids = HashSet::new();
    let mut sessions = Vec::new();
    for candidate in candidates {
        if accepted_ids.contains(&candidate.session_id) {
            continue;
        }
        let Some(session) = read_candidate(candidate, cwd) else {
            continue;
        };
        accepted_ids.insert(session.native_id.clone());
        sessions.push(session);
        if sessions.len() == MAX_SESSIONS_PER_TOOL {
            break;
        }
    }
    finish_tool_scan(sessions)
}

fn collect_candidates(
    root: &ApprovedRoot,
    project_dirs: &[PathBuf],
    now: SystemTime,
    within: Duration,
    limit: usize,
) -> Vec<Candidate> {
    let mut candidates = Vec::with_capacity(limit);
    for project_dir in project_dirs.iter().take(projects::MAX_PROJECT_DIRS) {
        let Some(project_root) = root.subroot(project_dir) else {
            continue;
        };
        let mut project_candidates = Vec::with_capacity(limit);
        // Enumerate every direct entry in these already-scoped directories so
        // filesystem order cannot decide which sessions receive the read budget.
        let complete = project_root.for_each_entry(|name| {
            let path = project_root.join(&name);
            let Some(session_id) = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(".jsonl"))
                .filter(|id| uuid::Uuid::try_parse(id).is_ok())
            else {
                return;
            };
            let Some((path, metadata)) = project_root.resolve_regular_file(&path) else {
                return;
            };
            if !metadata.is_file() || metadata.len() == 0 {
                return;
            }
            let Ok(modified) = metadata.modified() else {
                return;
            };
            if !is_within(modified, now, within) {
                return;
            }
            retain_top_k_by(
                &mut project_candidates,
                Candidate {
                    root: project_root.clone(),
                    path,
                    session_id: session_id.to_owned(),
                    modified,
                    size: metadata.len(),
                },
                limit,
                candidate_order,
            );
        });
        if !complete {
            continue;
        }
        for candidate in project_candidates {
            retain_top_k_by(&mut candidates, candidate, limit, candidate_order);
        }
    }
    candidates
}

fn candidate_order(a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
    b.modified
        .cmp(&a.modified)
        .then_with(|| a.session_id.cmp(&b.session_id))
        .then_with(|| a.path.cmp(&b.path))
}

fn read_candidate(candidate: Candidate, requested_cwd: &Path) -> Option<ForeignSessionSummary> {
    let (head, stored_cwd) = qualify_candidate(&candidate, requested_cwd)?;
    let tail = read_tail(&candidate.root, &candidate.path, candidate.size)?;
    let first_prompt = first_prompt(&head);
    let title = [
        last_string(&tail, "customTitle").or_else(|| last_string(&head, "customTitle")),
        last_string(&tail, "aiTitle").or_else(|| last_string(&head, "aiTitle")),
        last_string(&tail, "lastPrompt").or_else(|| last_string(&head, "lastPrompt")),
        last_string(&tail, "summary").or_else(|| last_string(&head, "summary")),
        first_prompt,
    ]
    .into_iter()
    .flatten()
    .find_map(|value| normalize_title(&value))?;
    let branch = last_string(&tail, "gitBranch")
        .or_else(|| last_string(&head, "gitBranch"))
        .and_then(|value| normalize_title(&value));

    Some(ForeignSessionSummary {
        tool: ForeignSessionTool::Claude,
        source: ForeignSessionSource::ClaudeCode,
        native_id: candidate.session_id,
        title,
        cwd: PathBuf::from(stored_cwd),
        updated_at: candidate.modified,
        branch,
    })
}

fn qualify_candidate(candidate: &Candidate, requested_cwd: &Path) -> Option<(String, String)> {
    let (head, stored_cwd) = read_head_for_cwd(&candidate.root, &candidate.path, candidate.size)?;
    let first_line = head.lines().next().unwrap_or(&head);
    if first_line.contains("\"isSidechain\":true") || first_line.contains("\"isSidechain\": true") {
        return None;
    }
    let stored_cwd = stored_cwd?;
    if Path::new(&stored_cwd) != requested_cwd {
        return None;
    }
    Some((head, stored_cwd))
}

fn read_head_for_cwd(
    root: &ApprovedRoot,
    path: &Path,
    size: u64,
) -> Option<(String, Option<String>)> {
    let max = usize::try_from(size.min(MAX_HEAD as u64)).ok()?;
    let mut limit = READ_CHUNK.min(max);
    loop {
        let head = read_prefix(root, path, limit)?;
        let cwd = first_string(&head, "cwd");
        if cwd.is_some() || limit >= max {
            return Some((head, cwd));
        }
        limit = limit.saturating_mul(4).min(max);
    }
}

fn read_prefix(root: &ApprovedRoot, path: &Path, limit: usize) -> Option<String> {
    let file = root.open_regular_file(path)?.file;
    let mut bytes = Vec::with_capacity(limit);
    file.take(limit as u64).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn read_tail(root: &ApprovedRoot, path: &Path, size: u64) -> Option<String> {
    let len = size.min(READ_CHUNK as u64);
    let mut file = root.open_regular_file(path)?.file;
    file.seek(SeekFrom::Start(size.saturating_sub(len))).ok()?;
    let mut bytes = Vec::with_capacity(len as usize);
    file.take(len).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn first_string(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| string_field(line, key))
}

fn last_string(text: &str, key: &str) -> Option<String> {
    text.lines().rev().find_map(|line| string_field(line, key))
}

fn string_field(line: &str, key: &str) -> Option<String> {
    let value: Value = serde_json::from_str(line).ok()?;
    value.get(key)?.as_str().map(str::to_owned)
}

fn first_prompt(head: &str) -> Option<String> {
    let mut command_fallback = None;
    for line in head.lines() {
        if line.contains("\"tool_result\"") {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if entry.get("type").and_then(Value::as_str) != Some("user")
            || entry.get("isMeta").and_then(Value::as_bool) == Some(true)
            || entry.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
        {
            continue;
        }
        let Some(content) = entry.pointer("/message/content") else {
            continue;
        };
        let texts = match content {
            Value::String(text) => vec![text.as_str()],
            Value::Array(blocks) => blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect(),
            _ => Vec::new(),
        };
        for text in texts {
            let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
            if normalized.is_empty() {
                continue;
            }
            if let Some(command) = between(&normalized, "<command-name>", "</command-name>") {
                command_fallback.get_or_insert_with(|| command.to_owned());
                continue;
            }
            if let Some(command) = between(&normalized, "<bash-input>", "</bash-input>") {
                return normalize_title(&format!("! {}", command.trim()));
            }
            if is_generated_prompt(&normalized) {
                continue;
            }
            return normalize_title(&normalized);
        }
    }
    command_fallback.and_then(|value| normalize_title(&value))
}

fn between<'a>(value: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let start = value.find(start)? + start.len();
    let end = value[start..].find(end)? + start;
    Some(&value[start..end])
}

fn is_generated_prompt(value: &str) -> bool {
    if value.starts_with("[Request interrupted by user") {
        return true;
    }
    let value = value.trim_start();
    value
        .strip_prefix('<')
        .and_then(|rest| rest.chars().next())
        .is_some_and(|first| first.is_ascii_lowercase())
}

#[cfg(all(test, unix))]
mod tests;
#[cfg(all(test, windows))]
mod windows_tests;
