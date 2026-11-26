use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, params};

use super::{DbStats, ListFilter, WorktreeKind, WorktreeRecord, WorktreeStatus, now_epoch_secs};

fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorktreeRecord> {
    let kind_str: String = row.get("kind")?;
    let status_str: String = row.get("status")?;
    let path_str: String = row.get("path")?;
    let source_str: String = row.get("source_repo")?;
    let metadata_str: Option<String> = row.get("metadata")?;

    Ok(WorktreeRecord {
        id: row.get("id")?,
        path: path_str.into(),
        source_repo: source_str.into(),
        repo_name: row.get("repo_name")?,
        kind: WorktreeKind::from_str_lossy(&kind_str),
        creation_mode: row.get("creation_mode")?,
        git_ref: row.get("git_ref")?,
        head_commit: row.get("head_commit")?,
        session_id: row.get("session_id")?,
        creator_pid: row.get::<_, Option<i64>>("creator_pid")?.map(|v| v as u32),
        created_at: row.get("created_at")?,
        last_accessed_at: row.get("last_accessed_at")?,
        status: WorktreeStatus::from_str_lossy(&status_str),
        metadata: metadata_str.and_then(|s| serde_json::from_str(&s).ok()),
    })
}

pub fn register(conn: &Connection, record: &WorktreeRecord) -> Result<()> {
    let path_str = record.path.to_string_lossy();
    let source_str = record.source_repo.to_string_lossy();
    let metadata_str = record
        .metadata
        .as_ref()
        .and_then(|v| serde_json::to_string(v).ok());

    conn.execute(
        "INSERT OR REPLACE INTO worktrees \
         (id, path, source_repo, repo_name, kind, creation_mode, git_ref, \
          head_commit, session_id, creator_pid, created_at, last_accessed_at, \
          status, metadata) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            record.id,
            path_str.as_ref(),
            source_str.as_ref(),
            record.repo_name,
            record.kind.as_str(),
            record.creation_mode,
            record.git_ref,
            record.head_commit,
            record.session_id,
            record.creator_pid.map(|p| p as i64),
            record.created_at,
            record.last_accessed_at,
            record.status.as_str(),
            metadata_str,
        ],
    )
    .context("failed to register worktree")?;
    Ok(())
}

pub fn unregister(conn: &Connection, id: &str) -> Result<bool> {
    let affected = conn
        .execute("DELETE FROM worktrees WHERE id = ?1", params![id])
        .context("failed to unregister worktree")?;
    Ok(affected > 0)
}

pub fn unregister_by_path(conn: &Connection, path: &Path) -> Result<bool> {
    let path_str = path.to_string_lossy();
    let affected = conn
        .execute(
            "DELETE FROM worktrees WHERE path = ?1",
            params![path_str.as_ref()],
        )
        .context("failed to unregister worktree by path")?;
    Ok(affected > 0)
}

pub fn mark_dead(conn: &Connection, id: &str) -> Result<bool> {
    let affected = conn
        .execute(
            "UPDATE worktrees SET status = 'dead' WHERE id = ?1",
            params![id],
        )
        .context("failed to mark worktree dead")?;
    Ok(affected > 0)
}

pub fn touch(conn: &Connection, id: &str) -> Result<bool> {
    let now = now_epoch_secs();
    let affected = conn
        .execute(
            "UPDATE worktrees SET last_accessed_at = ?1 WHERE id = ?2",
            params![now, id],
        )
        .context("failed to touch worktree")?;
    Ok(affected > 0)
}

fn get_one(conn: &Connection, sql: &str, param: &str) -> Result<Option<WorktreeRecord>> {
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query_map(params![param], row_to_record)?;
    match rows.next() {
        Some(Ok(record)) => Ok(Some(record)),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

pub fn get_by_id(conn: &Connection, id: &str) -> Result<Option<WorktreeRecord>> {
    get_one(conn, "SELECT * FROM worktrees WHERE id = ?1", id)
}

pub fn get_by_label(conn: &Connection, label: &str) -> Result<Option<WorktreeRecord>> {
    get_one(
        conn,
        "SELECT * FROM worktrees WHERE json_valid(metadata) AND json_extract(metadata, '$.label') = ?1 ORDER BY created_at DESC",
        label,
    )
}

pub fn get_by_path(conn: &Connection, path: &Path) -> Result<Option<WorktreeRecord>> {
    get_one(
        conn,
        "SELECT * FROM worktrees WHERE path = ?1",
        &path.to_string_lossy(),
    )
}

pub fn list(conn: &Connection, filter: &ListFilter) -> Result<Vec<WorktreeRecord>> {
    let mut sql = String::from("SELECT * FROM worktrees WHERE 1=1");
    let mut idx = 0usize;

    let status_str = filter.status.map(|s| s.as_str());
    let kind_str = filter.kind.map(|k| k.as_str());
    let source_repo_str = filter
        .source_repo
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned());

    if !filter.include_dead {
        sql.push_str(" AND status = 'alive'");
    }
    if status_str.is_some() {
        idx += 1;
        sql.push_str(&format!(" AND status = ?{idx}"));
    }
    if kind_str.is_some() {
        idx += 1;
        sql.push_str(&format!(" AND kind = ?{idx}"));
    }
    if filter.repo_name.is_some() {
        idx += 1;
        sql.push_str(&format!(" AND repo_name = ?{idx}"));
    }
    if source_repo_str.is_some() {
        idx += 1;
        sql.push_str(&format!(" AND source_repo = ?{idx}"));
    }
    sql.push_str(" ORDER BY created_at DESC");

    let mut params: Vec<&dyn rusqlite::types::ToSql> = Vec::with_capacity(idx);
    if let Some(ref s) = status_str {
        params.push(s);
    }
    if let Some(ref k) = kind_str {
        params.push(k);
    }
    if let Some(ref r) = filter.repo_name {
        params.push(r);
    }
    if let Some(ref s) = source_repo_str {
        params.push(s);
    }

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params.as_slice(), row_to_record)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

pub fn stats(conn: &Connection) -> Result<DbStats> {
    let total: u64 = conn.query_row("SELECT COUNT(*) FROM worktrees", [], |row| row.get(0))?;
    let alive: u64 = conn.query_row(
        "SELECT COUNT(*) FROM worktrees WHERE status = 'alive'",
        [],
        |row| row.get(0),
    )?;
    let page_count: u64 = conn
        .query_row("PRAGMA page_count", [], |row| row.get(0))
        .unwrap_or(0);
    let page_size: u64 = conn
        .query_row("PRAGMA page_size", [], |row| row.get(0))
        .unwrap_or(0);

    Ok(DbStats {
        total_records: total,
        alive_count: alive,
        dead_count: total.saturating_sub(alive),
        db_file_bytes: page_count * page_size,
    })
}

pub fn sweep_dead(conn: &Connection) -> Result<u64> {
    let alive_paths: Vec<(String, String, String)> = {
        let mut stmt =
            conn.prepare("SELECT id, path, creation_mode FROM worktrees WHERE status = 'alive'")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        rows.filter_map(|r| r.ok()).collect()
    };

    let mut marked = 0u64;
    for (id, path_str, mode) in alive_paths {
        // Grove dests can be a leftover mountpoint dir or a wedged mount.
        // `exists()` follows the mount and can hang; nfs_row_is_dead is
        // the only liveness probe for those rows.
        if crate::worktree::is_grove_strategy(&mode) {
            if crate::nfs::nfs_record_is_dead(Path::new(&path_str), None) {
                conn.execute(
                    "UPDATE worktrees SET status = 'dead' WHERE id = ?1",
                    params![id],
                )?;
                marked += 1;
            }
            continue;
        }
        // Linked/copy rows on a live grove dest: exists() hangs.
        let dest = Path::new(&path_str);
        if crate::nfs::dest_is_nfs_mount(dest)
            || crate::nfs::dest_is_mountpoint(dest)
            || !crate::nfs::dest_is_known_unmounted(dest)
        {
            continue;
        }
        // `exists()` follows the dest. A dangling worktree symlink still
        // occupies the path and must be unlinked by the age pass, not marked
        // dead and forgotten.
        if std::fs::symlink_metadata(dest).is_err() {
            conn.execute(
                "UPDATE worktrees SET status = 'dead' WHERE id = ?1",
                params![id],
            )?;
            marked += 1;
        }
    }
    Ok(marked)
}
