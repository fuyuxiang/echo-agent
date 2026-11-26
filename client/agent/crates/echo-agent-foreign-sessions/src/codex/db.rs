use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, SystemTime};

use rusqlite::{Connection, params};

use super::super::{
    ApprovedRoot, ForeignSessionSummary, ForeignSessionTool, MAX_SESSION_AGE,
    MAX_SESSIONS_PER_TOOL, RecentCandidate, is_within, millis_bounds, normalize_title,
    open_sqlite_transaction, system_time_from_millis,
};
use super::{existing_rollout_path, source_from_persisted, title};

const MAX_DB_CANDIDATES: usize = 200;
pub(super) const MAX_RECENT_DB_CANDIDATES: usize = 8;
const MIN_EPOCH_MILLIS: i64 = 1_577_836_800_000;
const MAX_ID_BYTES: usize = 64;
const MAX_PATH_BYTES: usize = 16 * 1024;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_BRANCH_BYTES: usize = 4 * 1024;

struct DbCandidate {
    id: String,
    rollout_path: String,
    updated_at: i64,
    source: String,
    stored_cwd: String,
    title: String,
    first_user_message: String,
    branch: Option<String>,
}

pub(super) enum RecentDatabaseResult {
    Unusable,
    Incomplete,
    Usable(Option<RecentCandidate>),
}

pub(super) fn scan_database(
    root: &ApprovedRoot,
    db_path: &Path,
    cwd: &Path,
    now: SystemTime,
) -> Option<Vec<ForeignSessionSummary>> {
    let bounds = millis_bounds(now, MAX_SESSION_AGE)?;
    let cwd_string = cwd.to_str()?.to_owned();
    if cwd_string.len() > MAX_PATH_BYTES {
        return None;
    }
    let database = open_sqlite_transaction(root, db_path)?;
    let columns = table_columns(&database, "threads")?;
    let sql = scan_sql(&columns)?;
    let rows = query_candidates(&database, &sql, &cwd_string, bounds)?;
    let mut sessions = Vec::new();
    for row in rows {
        let Some(candidate) =
            qualify_candidate(root, cwd, now, MAX_SESSION_AGE, &row, source_from_persisted)
        else {
            continue;
        };
        let Some(title) = title(&row.title, &row.first_user_message) else {
            continue;
        };
        sessions.push(ForeignSessionSummary {
            tool: ForeignSessionTool::Codex,
            source: candidate.source,
            native_id: candidate.native_id,
            title,
            cwd: Path::new(&row.stored_cwd).to_path_buf(),
            updated_at: candidate.updated_at,
            branch: row.branch.as_deref().and_then(normalize_title),
        });
        if sessions.len() == MAX_SESSIONS_PER_TOOL {
            break;
        }
    }
    Some(sessions)
}

pub(super) fn most_recent_database(
    root: &ApprovedRoot,
    db_path: &Path,
    cwd: &Path,
    now: SystemTime,
    within: Duration,
) -> RecentDatabaseResult {
    let Some(bounds) = millis_bounds(now, within) else {
        return RecentDatabaseResult::Unusable;
    };
    let Some(cwd_string) = cwd.to_str().map(str::to_owned) else {
        return RecentDatabaseResult::Unusable;
    };
    if cwd_string.len() > MAX_PATH_BYTES {
        return RecentDatabaseResult::Unusable;
    }
    let Some(database) = open_sqlite_transaction(root, db_path) else {
        return RecentDatabaseResult::Unusable;
    };
    let Some(columns) = table_columns(&database, "threads") else {
        return RecentDatabaseResult::Unusable;
    };
    let Some(sql) = recent_scan_sql(&columns) else {
        return RecentDatabaseResult::Unusable;
    };
    let Ok(mut rows) = query_recent_candidates(&database, &sql, &cwd_string, bounds) else {
        return RecentDatabaseResult::Incomplete;
    };
    let truncated = rows.len() > MAX_RECENT_DB_CANDIDATES;
    rows.truncate(MAX_RECENT_DB_CANDIDATES);
    let candidate = rows
        .into_iter()
        .find_map(|row| qualify_candidate(root, cwd, now, within, &row, super::source_from_str));
    if candidate.is_none() && truncated {
        RecentDatabaseResult::Incomplete
    } else {
        RecentDatabaseResult::Usable(candidate)
    }
}

fn query_candidates(
    database: &Connection,
    sql: &str,
    cwd: &str,
    (oldest_millis, newest_millis): (i64, i64),
) -> Option<Vec<DbCandidate>> {
    let mut statement = database.prepare(sql).ok()?;
    let rows = statement
        .query_map(
            params![cwd, oldest_millis, newest_millis],
            decode_candidate_row,
        )
        .ok()?;
    Some(rows.flatten().collect())
}

fn query_recent_candidates(
    database: &Connection,
    sql: &str,
    cwd: &str,
    (oldest_millis, newest_millis): (i64, i64),
) -> Result<Vec<DbCandidate>, ()> {
    let mut statement = database.prepare(sql).map_err(|_| ())?;
    let rows = statement
        .query_map(
            params![cwd, oldest_millis, newest_millis],
            decode_candidate_row,
        )
        .map_err(|_| ())?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|_| ())
}

fn decode_candidate_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DbCandidate> {
    Ok(DbCandidate {
        id: row.get(0)?,
        rollout_path: row.get(1)?,
        updated_at: row.get(2)?,
        source: row.get(3)?,
        stored_cwd: row.get(4)?,
        title: row.get(5)?,
        first_user_message: row.get(6)?,
        branch: row.get(7)?,
    })
}

fn qualify_candidate(
    root: &ApprovedRoot,
    cwd: &Path,
    now: SystemTime,
    within: Duration,
    row: &DbCandidate,
    parse_source: impl Fn(&str) -> Option<super::super::ForeignSessionSource>,
) -> Option<RecentCandidate> {
    if uuid::Uuid::try_parse(&row.id).is_err() || Path::new(&row.stored_cwd) != cwd {
        return None;
    }
    let source = parse_source(&row.source)?;
    let updated_at = normalize_updated_at(row.updated_at)?;
    if !is_within(updated_at, now, within)
        || existing_rollout_path(root, &row.rollout_path, &row.id).is_none()
    {
        return None;
    }
    Some(RecentCandidate {
        tool: ForeignSessionTool::Codex,
        source,
        native_id: row.id.clone(),
        updated_at,
    })
}

pub(super) fn normalize_updated_at(value: i64) -> Option<SystemTime> {
    let millis = if value < MIN_EPOCH_MILLIS {
        value.saturating_mul(1_000)
    } else {
        value
    };
    system_time_from_millis(millis)
}

pub(super) fn scan_sql(columns: &HashSet<String>) -> Option<String> {
    scan_sql_with_limit(
        columns,
        MAX_DB_CANDIDATES,
        "('cli', 'vscode', '{\"custom\":\"atlas\"}', '{\"custom\":\"chatgpt\"}')",
    )
}

pub(super) fn recent_scan_sql(columns: &HashSet<String>) -> Option<String> {
    scan_sql_with_limit(columns, MAX_RECENT_DB_CANDIDATES + 1, "('cli', 'vscode')")
}

fn scan_sql_with_limit(
    columns: &HashSet<String>,
    limit: usize,
    allowed_sources: &str,
) -> Option<String> {
    for required in ["id", "rollout_path", "source", "cwd", "archived"] {
        if !columns.contains(required) {
            return None;
        }
    }
    let updated_column = if columns.contains("updated_at_ms") {
        "updated_at_ms"
    } else if columns.contains("updated_at") {
        "updated_at"
    } else {
        return None;
    };
    let title_column = if columns.contains("title") {
        "title"
    } else {
        "''"
    };
    let first_user_message = if columns.contains("first_user_message") {
        "first_user_message"
    } else {
        "''"
    };
    let git_branch = if columns.contains("git_branch") {
        "git_branch"
    } else {
        "NULL"
    };
    let title_projection = format!(
        "CASE WHEN typeof({title_column}) = 'text' \
              AND octet_length({title_column}) <= {MAX_TEXT_BYTES} \
         THEN {title_column} ELSE '' END"
    );
    let first_projection = format!(
        "CASE WHEN typeof({first_user_message}) = 'text' \
              AND octet_length({first_user_message}) <= {MAX_TEXT_BYTES} \
         THEN {first_user_message} ELSE '' END"
    );
    let branch_projection = format!(
        "CASE WHEN typeof({git_branch}) = 'text' \
              AND octet_length({git_branch}) <= {MAX_BRANCH_BYTES} \
         THEN {git_branch} ELSE NULL END"
    );
    Some(format!(
        "SELECT id, rollout_path, {updated_column}, source, cwd, \
         {title_projection}, {first_projection}, {branch_projection} \
         FROM threads \
         WHERE typeof(id) = 'text' \
           AND typeof(rollout_path) = 'text' \
           AND typeof({updated_column}) = 'integer' \
           AND typeof(archived) = 'integer' \
           AND archived = 0 AND cwd = ?1 \
           AND source IN {allowed_sources} \
           AND octet_length(id) <= {MAX_ID_BYTES} \
           AND octet_length(rollout_path) <= {MAX_PATH_BYTES} \
           AND CASE \
               WHEN {updated_column} < {MIN_EPOCH_MILLIS} THEN {updated_column} * 1000 \
               ELSE {updated_column} \
           END BETWEEN ?2 AND ?3 \
         ORDER BY CASE \
             WHEN {updated_column} < {MIN_EPOCH_MILLIS} THEN {updated_column} * 1000 \
             ELSE {updated_column} \
         END DESC, id ASC LIMIT {limit}"
    ))
}

fn table_columns(connection: &Connection, table: &str) -> Option<HashSet<String>> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .ok()?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(1))
        .ok()?;
    let columns = rows.flatten().collect::<HashSet<_>>();
    (!columns.is_empty()).then_some(columns)
}
