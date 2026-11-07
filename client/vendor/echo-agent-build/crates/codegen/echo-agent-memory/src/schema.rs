//! SQL schema constants for the memory index.
//!
//! The index uses three tables:
//! - `meta` — key-value metadata (embedding dimensions, schema version)
//! - `chunks` — indexed text chunks with blake3 content hashes
//! - `chunks_fts` — contentless FTS5 virtual table for BM25 keyword search
//!
//! When sqlite-vec is available, a fourth table is created:
//! - `chunks_vec` — vec0 virtual table for KNN vector search

/// Schema version. Bump when making breaking schema changes that require
/// dropping and recreating tables.
pub const SCHEMA_VERSION: u32 = 1;

/// Generate the SQL schema for the memory index.
///
/// `dimensions` controls the embedding vector size for `chunks_vec`.
/// If `vec_available` is false, the `chunks_vec` table is not created.
///
/// Connection pragmas (busy_timeout, journal_mode) are applied on the open
/// path (`echo_agent_sqlite_journal::JournalMode::open`) — the journal mode depends
/// on the database's filesystem.
pub fn schema_sql(dimensions: usize, vec_available: bool) -> String {
    let mut sql = format!(
        r#"
CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS chunks (
    rowid INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT UNIQUE NOT NULL,
    path TEXT NOT NULL,
    start_line INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    text TEXT NOT NULL,
    hash TEXT NOT NULL,
    source TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    access_count INTEGER DEFAULT 0,
    last_accessed INTEGER
);

CREATE INDEX IF NOT EXISTS idx_chunks_path ON chunks(path);
CREATE INDEX IF NOT EXISTS idx_chunks_hash ON chunks(hash);

CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(text, content='');

-- Additive migration: indexed Han bigrams; the original FTS table remains readable.
CREATE VIRTUAL TABLE IF NOT EXISTS chunks_terms USING fts5(text);

INSERT OR IGNORE INTO meta(key, value) VALUES ('reindex_claim', '');
"#
    );

    if vec_available {
        sql.push_str(&format!(
            "\nCREATE VIRTUAL TABLE IF NOT EXISTS chunks_vec USING vec0(\n    \
             chunk_id TEXT PRIMARY KEY,\n    \
             embedding FLOAT[{dimensions}]\n);\n"
        ));
    }

    sql
}

/// SQL to insert or update an embedding dimension record in the meta table.
pub const UPSERT_META_SQL: &str = "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)";

/// SQL to query a meta value by key.
pub const GET_META_SQL: &str = "SELECT value FROM meta WHERE key = ?1";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema_sql_without_vec() {
        let sql = schema_sql(1536, false);
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS chunks"));
        assert!(sql.contains("CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts"));
        assert!(!sql.contains("chunks_vec"));
        // Connection pragmas live on the open path, not in the schema batch.
        assert!(!sql.contains("PRAGMA"));
    }

    #[test]
    fn test_schema_sql_with_vec() {
        let sql = schema_sql(384, true);
        assert!(sql.contains("chunks_vec"));
        assert!(sql.contains("FLOAT[384]"));
    }

    #[test]
    fn test_schema_sql_different_dimensions() {
        let sql = schema_sql(768, true);
        assert!(sql.contains("FLOAT[768]"));
    }
}
