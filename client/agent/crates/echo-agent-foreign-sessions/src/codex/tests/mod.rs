use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use filetime::FileTime;
use rusqlite::{Connection, params, types::Value as SqlValue};
use serde_json::json;
use tempfile::TempDir;

use super::*;
use crate::{millis_from_system_time, system_time_from_millis};

mod db;
mod files;

struct DbRow<'a> {
    id: uuid::Uuid,
    rollout_path: &'a Path,
    updated_at_ms: i64,
    source: &'a str,
    cwd: &'a Path,
    title: &'a str,
    first_user_message: &'a str,
    archived: bool,
}

fn create_db(path: &Path, rows: &[DbRow<'_>]) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE threads (
                id TEXT,
                rollout_path TEXT,
                updated_at_ms INTEGER,
                source TEXT,
                cwd TEXT,
                title TEXT,
                first_user_message TEXT,
                archived INTEGER,
                git_branch TEXT
            );
            CREATE INDEX threads_archived_cwd_updated \
            ON threads(archived, cwd, updated_at_ms DESC);",
        )
        .unwrap();
    for row in rows {
        connection
            .execute(
                "INSERT INTO threads VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL)",
                params![
                    row.id.to_string(),
                    row.rollout_path.display().to_string(),
                    row.updated_at_ms,
                    row.source,
                    row.cwd.display().to_string(),
                    row.title,
                    row.first_user_message,
                    row.archived,
                ],
            )
            .unwrap();
    }
}

fn touch(path: &Path, time: SystemTime) {
    filetime::set_file_mtime(path, FileTime::from_system_time(time)).unwrap();
}

fn session_day(root: &Path, now: SystemTime) -> PathBuf {
    use chrono::{DateTime, Datelike, Utc};

    let date = DateTime::<Utc>::from(now).date_naive();
    root.join("sessions")
        .join(format!("{:04}", date.year()))
        .join(format!("{:02}", date.month()))
        .join(format!("{:02}", date.day()))
}

fn rollout_path(dir: &Path, id: uuid::Uuid) -> PathBuf {
    dir.join(format!("rollout-2027-01-15T12-00-00-{id}.jsonl"))
}

fn write_recent_rollout(
    root: &Path,
    cwd: &Path,
    now: SystemTime,
    id: uuid::Uuid,
    source: serde_json::Value,
) -> PathBuf {
    let day = session_day(root, now);
    fs::create_dir_all(&day).unwrap();
    let path = rollout_path(&day, id);
    fs::write(
        &path,
        json!({
            "type": "session_meta",
            "payload": {
                "id": id,
                "cwd": cwd.display().to_string(),
                "source": source,
            }
        })
        .to_string(),
    )
    .unwrap();
    touch(&path, now);
    path
}

fn oversized_text(limit: usize, utf8: bool) -> String {
    if utf8 {
        "é".repeat(limit / 2 + 1)
    } else {
        "x".repeat(limit + 1)
    }
}
