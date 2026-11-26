use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::Value;

mod db;
mod files;

use super::{
    ApprovedRoot, ForeignSessionSource, ForeignSessionSummary, RecentCandidate, RecentProbe,
    approved_root_for_recent, finish_tool_scan, normalize_title,
};

// Codex is currently on single-digit state generations. Probe a generous,
// deterministic supported range without enumerating unrelated CODEX_HOME files.
const MAX_STATE_DB_GENERATION: u32 = 128;

pub(super) fn scan(cwd: &Path, now: SystemTime) -> Vec<ForeignSessionSummary> {
    let Some(codex_home) = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))
    else {
        return Vec::new();
    };
    scan_in_home(&codex_home, cwd, now)
}

pub(super) fn most_recent(
    cwd: &Path,
    now: SystemTime,
    within: Duration,
) -> RecentProbe<RecentCandidate> {
    let Some(codex_home) = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))
    else {
        return RecentProbe::Complete(None);
    };
    most_recent_in_home(&codex_home, cwd, now, within)
}

fn scan_in_home(codex_home: &Path, cwd: &Path, now: SystemTime) -> Vec<ForeignSessionSummary> {
    let Some(root) = ApprovedRoot::new(codex_home) else {
        return Vec::new();
    };
    let sessions = state_databases(&root)
        .find_map(|path| {
            db::scan_database(&root, &path, cwd, now).filter(|sessions| !sessions.is_empty())
        })
        .unwrap_or_else(|| files::scan_rollouts(&root, cwd, now));
    finish_tool_scan(sessions)
}

fn most_recent_in_home(
    codex_home: &Path,
    cwd: &Path,
    now: SystemTime,
    within: Duration,
) -> RecentProbe<RecentCandidate> {
    let root = match approved_root_for_recent(codex_home) {
        Ok(Some(root)) => root,
        Ok(None) => return RecentProbe::Complete(None),
        Err(()) => return RecentProbe::Incomplete,
    };
    let Some(path) = highest_named_state_database(&root) else {
        return files::most_recent_rollout(&root, cwd, now, within);
    };
    match db::most_recent_database(&root, &path, cwd, now, within) {
        db::RecentDatabaseResult::Usable(candidate) => RecentProbe::Complete(candidate),
        db::RecentDatabaseResult::Incomplete => RecentProbe::Incomplete,
        db::RecentDatabaseResult::Unusable => files::most_recent_rollout(&root, cwd, now, within),
    }
}

fn highest_named_state_database(root: &ApprovedRoot) -> Option<PathBuf> {
    (0..=MAX_STATE_DB_GENERATION).rev().find_map(|generation| {
        let path = root.join(format!("state_{generation}.sqlite"));
        match std::fs::symlink_metadata(&path) {
            Ok(_) => Some(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => Some(path),
        }
    })
}

fn state_databases(root: &ApprovedRoot) -> impl Iterator<Item = PathBuf> + '_ {
    (0..=MAX_STATE_DB_GENERATION)
        .rev()
        .filter_map(move |generation| {
            let path = root.join(format!("state_{generation}.sqlite"));
            root.resolve_regular_file(&path).map(|(path, _)| path)
        })
}

fn source_from_str(source: &str) -> Option<ForeignSessionSource> {
    match source {
        "cli" => Some(ForeignSessionSource::CodexCli),
        "vscode" => Some(ForeignSessionSource::CodexVsCode),
        _ => None,
    }
}

fn source_from_persisted(source: &str) -> Option<ForeignSessionSource> {
    source_from_str(source).or_else(|| {
        let value = serde_json::from_str::<Value>(source).ok()?;
        source_from_value(&value)
    })
}

fn source_from_value(source: &Value) -> Option<ForeignSessionSource> {
    source
        .as_str()
        .and_then(source_from_str)
        .or_else(|| match source.get("custom")?.as_str()? {
            "atlas" => Some(ForeignSessionSource::CodexAtlas),
            "chatgpt" => Some(ForeignSessionSource::CodexChatGpt),
            _ => None,
        })
}

fn existing_rollout_path(root: &ApprovedRoot, value: &str, expected_id: &str) -> Option<PathBuf> {
    let path = PathBuf::from(value);
    if path.components().any(|part| part == Component::ParentDir) {
        return None;
    }
    let path = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    let mut compressed = path.clone().into_os_string();
    compressed.push(".zst");
    let approved_prefixes = ["sessions", "archived_sessions"]
        .into_iter()
        .filter_map(|name| dunce::canonicalize(root.join(name)).ok())
        .filter(|path| path.starts_with(root.path()))
        .collect::<Vec<_>>();
    [path, PathBuf::from(compressed)]
        .into_iter()
        .filter_map(|candidate| root.open_regular_file(&candidate).map(|opened| opened.path))
        .find(|candidate| {
            rollout_id(candidate).as_deref() == Some(expected_id)
                && approved_prefixes
                    .iter()
                    .any(|prefix| candidate.starts_with(prefix))
        })
}

fn rollout_id(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let stem = name
        .strip_suffix(".jsonl.zst")
        .or_else(|| name.strip_suffix(".jsonl"))?;
    let value = stem.strip_prefix("rollout-")?;
    let id_start = value.len().checked_sub(36)?;
    if id_start == 0 || value.as_bytes().get(id_start - 1) != Some(&b'-') {
        return None;
    }
    let timestamp = &value[..id_start - 1];
    if timestamp.len() != 19 {
        return None;
    }
    chrono::NaiveDateTime::parse_from_str(timestamp, "%Y-%m-%dT%H-%M-%S").ok()?;
    let id = &value[id_start..];
    uuid::Uuid::try_parse(id).ok()?;
    Some(id.to_owned())
}

fn title(primary: &str, fallback: &str) -> Option<String> {
    normalize_title(primary).or_else(|| normalize_title(fallback))
}

#[cfg(test)]
mod fixed_path_tests {
    use super::*;

    #[test]
    fn fixed_rollout_qualification_does_not_require_enumeration() {
        let root = tempfile::tempdir().unwrap();
        let sessions = root.path().join("sessions/2027/01/15");
        std::fs::create_dir_all(&sessions).unwrap();
        let id = uuid::Uuid::from_u128(9_000);
        let rollout = sessions.join(format!("rollout-2027-01-15T12-00-00-{id}.jsonl"));
        std::fs::write(&rollout, "").unwrap();
        let approved = ApprovedRoot::new(root.path()).unwrap();
        assert_eq!(
            existing_rollout_path(&approved, &rollout.to_string_lossy(), &id.to_string()),
            Some(dunce::canonicalize(&rollout).unwrap())
        );
    }
}

#[cfg(all(test, unix))]
mod tests;
