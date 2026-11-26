use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Datelike, Days, Local, TimeDelta, Utc};
use serde_json::Value;

use super::super::{
    ApprovedRoot, ForeignSessionSource, ForeignSessionSummary, ForeignSessionTool, MAX_SESSION_AGE,
    MAX_SESSIONS_PER_TOOL, RecentCandidate, RecentProbe, is_within, normalize_title,
    retain_top_k_by,
};
use super::{rollout_id, source_from_value, title};

const DAYS_IN_WINDOW: usize = 31;
const MAX_DATE_DIRS: usize = 32;
const MAX_METADATA_READS: usize = 128;
const MAX_RECENT_METADATA_READS: usize = 16;
pub(super) const MAX_RECENT_DIRECTORY_ENTRIES: usize = 64;
const MAX_HEAD_RECORDS: usize = 10;
pub(super) const MAX_HEAD_BYTES: usize = 64 * 1024;
// Bound compressed work separately from the decoded head and decoder window.
pub(super) const MAX_COMPRESSED_HEAD_BYTES: usize = 256 * 1024;
pub(super) const MAX_ZSTD_WINDOW_LOG: u32 = 23;

struct Candidate {
    root: ApprovedRoot,
    path: PathBuf,
    id: String,
    modified: SystemTime,
}

#[derive(Default)]
struct HeadMetadata {
    id: Option<uuid::Uuid>,
    cwd: Option<String>,
    source: Option<ForeignSessionSource>,
    branch: Option<String>,
    first_user_message: Option<String>,
}

pub(super) fn scan_rollouts(
    codex_root: &ApprovedRoot,
    cwd: &Path,
    now: SystemTime,
) -> Vec<ForeignSessionSummary> {
    let Some(root) = codex_root.subroot(Path::new("sessions")) else {
        return Vec::new();
    };
    let candidates = collect_candidates(&root, now, MAX_SESSION_AGE, MAX_METADATA_READS);
    let mut accepted_ids = HashSet::new();
    let mut sessions = Vec::new();
    for candidate in candidates {
        if accepted_ids.contains(&candidate.id) {
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
    sessions
}

pub(super) fn most_recent_rollout(
    codex_root: &ApprovedRoot,
    cwd: &Path,
    now: SystemTime,
    within: Duration,
) -> RecentProbe<RecentCandidate> {
    let sessions_path = codex_root.join("sessions");
    match std::fs::symlink_metadata(&sessions_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return RecentProbe::Complete(None);
        }
        Err(_) => return RecentProbe::Incomplete,
        Ok(_) => {}
    }
    let Some(root) = codex_root.subroot(&sessions_path) else {
        return RecentProbe::Incomplete;
    };
    let Some((candidates, truncated)) = collect_recent_candidates(&root, now, within) else {
        return RecentProbe::Incomplete;
    };
    let candidate = candidates.into_iter().find_map(|candidate| {
        let (_, source, _) = qualify_candidate(&candidate, cwd)?;
        if !matches!(
            source,
            ForeignSessionSource::CodexCli | ForeignSessionSource::CodexVsCode
        ) {
            return None;
        }
        Some(RecentCandidate {
            tool: ForeignSessionTool::Codex,
            source,
            native_id: candidate.id,
            updated_at: candidate.modified,
        })
    });
    if candidate.is_none() && truncated {
        RecentProbe::Incomplete
    } else {
        RecentProbe::Complete(candidate)
    }
}

fn collect_recent_candidates(
    root: &ApprovedRoot,
    now: SystemTime,
    within: Duration,
) -> Option<(Vec<Candidate>, bool)> {
    let mut candidates = Vec::with_capacity(MAX_RECENT_METADATA_READS);
    let mut remaining_entries = MAX_RECENT_DIRECTORY_ENTRIES;
    let mut qualifying = 0;
    let local_offset = DateTime::<Local>::from(now).offset().local_minus_utc();
    for date_dir in recent_date_dirs(root.path(), now, local_offset) {
        let Some(date_root) = root.subroot(&date_dir) else {
            continue;
        };
        let outcome = date_root.for_each_entry_bounded(remaining_entries, |name| {
            let path = date_root.join(&name);
            let Some(id) = rollout_id(&path) else {
                return;
            };
            let Some((path, metadata)) = date_root.resolve_regular_file(&path) else {
                return;
            };
            let Ok(modified) = metadata.modified() else {
                return;
            };
            if is_within(modified, now, within) {
                qualifying += 1;
                retain_top_k_by(
                    &mut candidates,
                    Candidate {
                        root: date_root.clone(),
                        path,
                        id,
                        modified,
                    },
                    MAX_RECENT_METADATA_READS,
                    candidate_order,
                );
            }
        });
        remaining_entries = remaining_entries.saturating_sub(outcome.visited);
        if !outcome.complete {
            return None;
        }
    }
    Some((candidates, qualifying > MAX_RECENT_METADATA_READS))
}

fn collect_candidates(
    root: &ApprovedRoot,
    now: SystemTime,
    within: Duration,
    limit: usize,
) -> Vec<Candidate> {
    let mut candidates = Vec::with_capacity(limit);
    let local_offset = DateTime::<Local>::from(now).offset().local_minus_utc();
    for date_dir in recent_date_dirs(root.path(), now, local_offset) {
        let Some(date_root) = root.subroot(&date_dir) else {
            continue;
        };
        let mut date_candidates = Vec::with_capacity(limit);
        // Enumerate every direct rollout entry in the fixed date window so
        // filesystem order cannot decide which files receive the head-read budget.
        let complete = date_root.for_each_entry(|name| {
            let path = date_root.join(&name);
            let Some(id) = rollout_id(&path) else {
                return;
            };
            let Some((path, metadata)) = date_root.resolve_regular_file(&path) else {
                return;
            };
            let Ok(modified) = metadata.modified() else {
                return;
            };
            if is_within(modified, now, within) {
                retain_top_k_by(
                    &mut date_candidates,
                    Candidate {
                        root: date_root.clone(),
                        path,
                        id,
                        modified,
                    },
                    limit,
                    candidate_order,
                );
            }
        });
        if !complete {
            continue;
        }
        for candidate in date_candidates {
            retain_top_k_by(&mut candidates, candidate, limit, candidate_order);
        }
    }
    candidates
}

fn candidate_order(a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
    b.modified
        .cmp(&a.modified)
        .then_with(|| a.id.cmp(&b.id))
        .then_with(|| a.path.cmp(&b.path))
}

pub(super) fn recent_date_dirs(
    root: &Path,
    now: SystemTime,
    local_offset_seconds: i32,
) -> Vec<PathBuf> {
    let utc = DateTime::<Utc>::from(now);
    let local = utc + TimeDelta::seconds(i64::from(local_offset_seconds));
    let mut dates = [utc.date_naive(), local.date_naive()]
        .into_iter()
        .flat_map(|today| {
            (0..DAYS_IN_WINDOW)
                .filter_map(move |days| today.checked_sub_days(Days::new(days as u64)))
        })
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    dates.sort_by(|a, b| b.cmp(a));
    dates.truncate(MAX_DATE_DIRS);
    dates
        .into_iter()
        .map(|date| {
            root.join(format!("{:04}", date.year()))
                .join(format!("{:02}", date.month()))
                .join(format!("{:02}", date.day()))
        })
        .collect()
}

fn read_candidate(candidate: Candidate, requested_cwd: &Path) -> Option<ForeignSessionSummary> {
    let (
        HeadMetadata {
            branch,
            first_user_message,
            ..
        },
        source,
        stored_cwd,
    ) = qualify_candidate(&candidate, requested_cwd)?;
    let title = title("", first_user_message.as_deref().unwrap_or(""))?;
    Some(ForeignSessionSummary {
        tool: ForeignSessionTool::Codex,
        source,
        native_id: candidate.id,
        title,
        cwd: PathBuf::from(stored_cwd),
        updated_at: candidate.modified,
        branch,
    })
}

fn qualify_candidate(
    candidate: &Candidate,
    requested_cwd: &Path,
) -> Option<(HeadMetadata, ForeignSessionSource, String)> {
    let metadata = read_head(&candidate.root, &candidate.path)?;
    if metadata.id? != uuid::Uuid::try_parse(&candidate.id).ok()? {
        return None;
    }
    let stored_cwd = metadata.cwd.clone()?;
    if Path::new(&stored_cwd) != requested_cwd {
        return None;
    }
    let source = metadata.source?;
    Some((metadata, source, stored_cwd))
}

fn read_head(root: &ApprovedRoot, path: &Path) -> Option<HeadMetadata> {
    let file = root.open_regular_file(path)?.file;
    if path.extension().and_then(|extension| extension.to_str()) == Some("zst") {
        let compressed = file.take(MAX_COMPRESSED_HEAD_BYTES as u64);
        let mut decoder = zstd::Decoder::new(compressed).ok()?;
        decoder.window_log_max(MAX_ZSTD_WINDOW_LOG).ok()?;
        let decoder = decoder.single_frame();
        parse_head(BufReader::new(decoder.take(MAX_HEAD_BYTES as u64)))
    } else {
        parse_head(BufReader::new(file.take(MAX_HEAD_BYTES as u64)))
    }
}

fn parse_head(mut reader: impl BufRead) -> Option<HeadMetadata> {
    let mut line = String::new();
    let mut total = 0;
    let mut metadata = HeadMetadata::default();
    let mut saw_session_meta = false;
    for _ in 0..MAX_HEAD_RECORDS {
        line.clear();
        let read = reader.read_line(&mut line).ok()?;
        if read == 0 {
            break;
        }
        total += read;
        if total > MAX_HEAD_BYTES {
            break;
        }
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let record_type = record.get("type").and_then(Value::as_str);
        let payload = record.get("payload").unwrap_or(&Value::Null);
        if record_type == Some("session_meta") && !saw_session_meta {
            saw_session_meta = true;
            metadata.id = payload
                .get("id")
                .and_then(Value::as_str)
                .and_then(|id| uuid::Uuid::try_parse(id).ok());
            metadata.cwd = payload
                .get("cwd")
                .and_then(Value::as_str)
                .map(str::to_owned);
            metadata.source = payload.get("source").and_then(source_from_value);
            metadata.branch = payload
                .pointer("/git/branch")
                .or_else(|| payload.get("git_branch"))
                .and_then(Value::as_str)
                .and_then(normalize_title);
        }
        if metadata.first_user_message.is_none() {
            metadata.first_user_message = user_message(payload).and_then(normalize_title);
        }
    }
    Some(metadata)
}

fn user_message(payload: &Value) -> Option<&str> {
    if payload.get("type").and_then(Value::as_str) == Some("user_message") {
        return payload.get("message").and_then(Value::as_str);
    }
    if payload.get("type").and_then(Value::as_str) != Some("message")
        || payload.get("role").and_then(Value::as_str) != Some("user")
    {
        return None;
    }
    let text = payload
        .get("content")?
        .as_array()?
        .iter()
        .find_map(|item| match item.get("type").and_then(Value::as_str) {
            Some("input_text" | "text") => item.get("text").and_then(Value::as_str),
            _ => None,
        })?;
    let trimmed = text.trim_start();
    (!trimmed.starts_with("<environment_context>") && !trimmed.starts_with("<user_instructions>"))
        .then_some(text)
}
