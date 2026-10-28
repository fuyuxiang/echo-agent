//! SQLite-backed memory index with FTS5 keyword search and optional sqlite-vec KNN.
//!
//! The index stores chunked text from memory files, with:
//! - A `chunks` table for structured metadata
//! - A contentless FTS5 virtual table for BM25 keyword search
//! - An optional `chunks_vec` vec0 table for vector similarity (when sqlite-vec is available)
//!
//! ## sqlite-vec Initialization
//!
//! Call [`init_sqlite_vec()`] once before creating any `MemoryIndex`.
//! This registers the sqlite-vec extension globally via `std::sync::Once`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Once;

use echo_agent_sqlite_journal::JournalMode;
use rusqlite::{OptionalExtension, params};

use super::chunker::{chunk_hash, chunk_markdown};
use super::schema;
use super::storage::MemoryStorage;
use echo_agent_config_types::MemoryIndexConfig;

static SQLITE_VEC_INIT: Once = Once::new();

/// Register the sqlite-vec extension globally. Must be called before any
/// `MemoryIndex::open_or_create()`. Safe to call multiple times (Once guard).
pub fn init_sqlite_vec() {
    SQLITE_VEC_INIT.call_once(|| {
        // SAFETY: sqlite_vec::sqlite3_vec_init has the C ABI signature expected by
        // sqlite3_auto_extension. The explicit type annotation on transmute makes
        // this compiler-verified — if sqlite-vec changes its init signature, the
        // annotation will cause a compile error instead of silent UB.
        // We pin sqlite-vec to exact version =0.1.7-alpha.2; any bump must re-verify.
        unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                unsafe extern "C" fn(
                    *mut rusqlite::ffi::sqlite3,
                    *mut *mut std::ffi::c_char,
                    *const rusqlite::ffi::sqlite3_api_routines,
                ) -> i32,
            >(
                sqlite_vec::sqlite3_vec_init as *const (),
            )));
        }
    });
}

/// A record from the `chunks` table.
#[derive(Debug, Clone)]
pub struct ChunkRecord {
    pub rowid: i64,
    pub id: String,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub text: String,
    pub hash: String,
    pub source: String,
    pub access_count: i64,
    pub created_at: i64,
}

/// Result of a FTS5 keyword search.
#[derive(Debug, Clone)]
pub struct FtsResult {
    pub chunk_id: String,
    pub rowid: i64,
    pub rank: f64,
}

/// Result of reindexing a file.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReindexResult {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct EmbeddingCandidate {
    pub id: String,
    pub path: String,
    pub text: String,
    pub hash: String,
    pub chunk_config: MemoryIndexConfig,
}

/// SQLite-backed memory index.
pub struct MemoryIndex {
    db: rusqlite::Connection,
    pub(crate) storage: MemoryStorage,
    chunk_config: MemoryIndexConfig,
    /// Whether sqlite-vec loaded successfully (FTS always available).
    vec_available: bool,
    embedding_dimensions: usize,
}

impl MemoryIndex {
    /// Open an existing index without resetting embeddings produced by a
    /// custom model with dimensions different from the desktop default.
    pub fn open_or_create_preserving_dimensions(
        db_path: &Path,
        storage: MemoryStorage,
        config: MemoryIndexConfig,
        default_dimensions: usize,
    ) -> Result<Self, rusqlite::Error> {
        let dimensions = JournalMode::for_db_path(db_path)
            .open_readonly(db_path)
            .ok()
            .and_then(|db| {
                db.query_row(
                    schema::GET_META_SQL,
                    params!["embedding_dimensions"],
                    |row| row.get::<_, String>(0),
                )
                .ok()
            })
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0 && *value <= 8192)
            .unwrap_or(default_dimensions);
        Self::open_or_create(db_path, storage, config, dimensions)
    }

    /// Open or create the index database at `db_path`.
    ///
    /// `dimensions` sets the embedding vector size for the `chunks_vec` table.
    /// If sqlite-vec failed to load (call `init_sqlite_vec()` first), the
    /// index gracefully degrades to FTS-only mode.
    pub fn open_or_create(
        db_path: &Path,
        storage: MemoryStorage,
        config: MemoryIndexConfig,
        dimensions: usize,
    ) -> Result<Self, rusqlite::Error> {
        // Ensure parent directory exists
        if let Some(parent) = db_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        // The mode decision statfs's the parent dir created above.
        Self::open_or_create_with_journal_mode(
            db_path,
            storage,
            config,
            dimensions,
            JournalMode::for_db_path(db_path),
        )
    }

    /// Open with an explicit journal mode — the seam tests use to exercise
    /// the network-filesystem decision on a local disk.
    fn open_or_create_with_journal_mode(
        db_path: &Path,
        storage: MemoryStorage,
        config: MemoryIndexConfig,
        dimensions: usize,
        journal_mode: JournalMode,
    ) -> Result<Self, rusqlite::Error> {
        // busy_timeout + journal pragma live in the helper (see JournalMode::open).
        let db = journal_mode.open(db_path)?;

        // Check if sqlite-vec loaded (graceful fallback if not)
        let vec_available =
            match db.query_row("SELECT vec_version()", [], |r| r.get::<_, String>(0)) {
                Ok(v) => {
                    tracing::info!(version = %v, "sqlite-vec loaded");
                    true
                }
                Err(_) => {
                    static WARNED: std::sync::Once = std::sync::Once::new();
                    WARNED.call_once(|| {
                        tracing::warn!("sqlite-vec not available, falling back to FTS-only search");
                    });
                    false
                }
            };

        // Create schema
        db.execute_batch(&schema::schema_sql(dimensions, vec_available))?;

        // Store/verify embedding dimensions in meta table
        let stored_dims: Option<String> = db
            .query_row(schema::GET_META_SQL, params!["embedding_dimensions"], |r| {
                r.get(0)
            })
            .ok();

        match stored_dims {
            Some(ref s) if s.parse::<usize>().ok() == Some(dimensions) => {
                // Dimensions match — nothing to do
            }
            Some(ref s) => {
                // Dimension mismatch — recreate vec table
                tracing::warn!(
                    stored = %s,
                    requested = dimensions,
                    "embedding dimension mismatch, recreating chunks_vec"
                );
                if vec_available {
                    let _ = db.execute("DROP TABLE IF EXISTS chunks_vec", []);
                    db.execute_batch(&format!(
                        "CREATE VIRTUAL TABLE IF NOT EXISTS chunks_vec USING vec0(\n    \
                         chunk_id TEXT PRIMARY KEY,\n    \
                         embedding FLOAT[{dimensions}]\n);"
                    ))?;
                }
                db.execute(
                    schema::UPSERT_META_SQL,
                    params!["embedding_dimensions", dimensions.to_string()],
                )?;
            }
            None => {
                // First time — store dimensions
                db.execute(
                    schema::UPSERT_META_SQL,
                    params!["embedding_dimensions", dimensions.to_string()],
                )?;
            }
        }

        Ok(Self {
            db,
            storage,
            chunk_config: config,
            vec_available,
            embedding_dimensions: dimensions,
        })
    }

    /// Whether sqlite-vec is available for vector operations.
    pub fn vec_available(&self) -> bool {
        self.vec_available
    }

    /// Embedding dimensions configured for this index.
    pub fn embedding_dimensions(&self) -> usize {
        self.embedding_dimensions
    }

    /// Direct access to the underlying SQLite connection (test-only).
    #[cfg(test)]
    pub(crate) fn db(&self) -> &rusqlite::Connection {
        &self.db
    }

    // -----------------------------------------------------------------------
    // Indexing
    // -----------------------------------------------------------------------

    /// Reindex a single memory file. Compares chunk hashes to avoid redundant work.
    ///
    /// `source` should be `"global"`, `"workspace"`, or `"session"`.
    pub fn reindex_file(
        &mut self,
        path: &Path,
        source: &str,
    ) -> Result<ReindexResult, rusqlite::Error> {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "failed to read file for reindexing");
                return Ok(ReindexResult::default());
            }
        };

        let new_chunks = chunk_markdown(&content, &self.chunk_config);
        let path_str = path.to_string_lossy().to_string();

        // Load existing chunks for this path
        let existing = self.get_chunks_for_path(&path_str)?;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let mut result = ReindexResult::default();
        let mut seen_ids = std::collections::HashSet::new();

        // Wrap all mutations in a transaction so partial failures don't leave
        // the index inconsistent between chunks, FTS, and vec tables.
        let tx = self.db.transaction()?;

        for (i, chunk) in new_chunks.iter().enumerate() {
            let chunk_id = format!("{}:{}", path_str, i);
            let hash = chunk_hash(&chunk.text);
            seen_ids.insert(chunk_id.clone());

            match existing.get(&chunk_id) {
                Some(old) if old.hash == hash => {
                    // Unchanged — skip
                }
                Some(old) => {
                    // Changed: update chunk, delete stale FTS entry, insert new one
                    tx.execute(
                        "UPDATE chunks SET text = ?1, hash = ?2, start_line = ?3, \
                         end_line = ?4, updated_at = ?5 WHERE id = ?6",
                        params![
                            chunk.text,
                            hash,
                            chunk.start_line,
                            chunk.end_line,
                            now,
                            chunk_id
                        ],
                    )?;
                    // Delete old FTS entry and insert new one
                    tx.execute(
                        "INSERT INTO chunks_fts(chunks_fts, rowid, text) VALUES('delete', ?1, ?2)",
                        params![old.rowid, old.text],
                    )?;
                    let new_rowid: Option<i64> = tx
                        .query_row(
                            "SELECT rowid FROM chunks WHERE id = ?1",
                            params![chunk_id],
                            |row| row.get(0),
                        )
                        .ok();
                    if let Some(rid) = new_rowid {
                        tx.execute(
                            "INSERT INTO chunks_fts(rowid, text) VALUES (?1, ?2)",
                            params![rid, chunk.text],
                        )?;
                    }
                    // Delete stale embedding (will be re-embedded later)
                    if self.vec_available {
                        let _ = tx.execute(
                            "DELETE FROM chunks_vec WHERE chunk_id = ?1",
                            params![chunk_id],
                        );
                    }
                    result.updated += 1;
                }
                None => {
                    // New chunk
                    tx.execute(
                        "INSERT INTO chunks (id, path, start_line, end_line, text, hash, source, \
                         created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                        params![
                            chunk_id,
                            path_str,
                            chunk.start_line,
                            chunk.end_line,
                            chunk.text,
                            hash,
                            source,
                            now,
                            now,
                        ],
                    )?;
                    let rowid = tx.last_insert_rowid();
                    tx.execute(
                        "INSERT INTO chunks_fts(rowid, text) VALUES (?1, ?2)",
                        params![rowid, chunk.text],
                    )?;
                    result.added += 1;
                }
            }
        }

        // Delete chunks no longer in file
        for (old_id, old_record) in &existing {
            if !seen_ids.contains(old_id) {
                tx.execute(
                    "INSERT INTO chunks_fts(chunks_fts, rowid, text) VALUES('delete', ?1, ?2)",
                    params![old_record.rowid, old_record.text],
                )?;
                if self.vec_available {
                    let _ = tx.execute(
                        "DELETE FROM chunks_vec WHERE chunk_id = ?1",
                        params![old_id],
                    );
                }
                tx.execute("DELETE FROM chunks WHERE id = ?1", params![old_id])?;
                result.removed += 1;
            }
        }

        tx.commit()?;
        Ok(result)
    }

    // -----------------------------------------------------------------------
    // Search
    // -----------------------------------------------------------------------

    /// FTS5 keyword search. Returns results ranked by BM25 score.
    ///
    /// Applies stop word filtering to improve precision for conversational
    /// queries. When all words are stop words, returns empty results — the
    /// caller (`hybrid_search`) falls back to the vector search path.
    pub fn search_fts_by_sources(
        &self,
        query: &str,
        limit: usize,
        sources: &[&str],
    ) -> Result<Vec<FtsResult>, rusqlite::Error> {
        if sources.is_empty() {
            return Ok(vec![]);
        }
        let keywords = super::query_expansion::extract_keywords(query);
        let fts_query = keywords.join(" OR ");
        if fts_query.is_empty() {
            return self.search_cjk_substrings(query, limit, Some(sources));
        }

        let placeholders: Vec<String> = sources
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 3))
            .collect();
        let sql = format!(
            "SELECT f.rowid, f.rank FROM chunks_fts f \
             JOIN chunks c ON f.rowid = c.rowid \
             WHERE chunks_fts MATCH ?1 AND c.source IN ({}) \
             ORDER BY f.rank LIMIT ?2",
            placeholders.join(", ")
        );
        let mut stmt = self.db.prepare(&sql)?;
        let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        params_vec.push(Box::new(fts_query));
        params_vec.push(Box::new(limit as i64));
        for s in sources {
            params_vec.push(Box::new(s.to_string()));
        }
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params_vec.iter().map(|p| p.as_ref()).collect();

        let rows = stmt
            .query_map(param_refs.as_slice(), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        self.merge_cjk_results(query, limit, Some(sources), rows)
    }

    pub fn search_fts(&self, query: &str, limit: usize) -> Result<Vec<FtsResult>, rusqlite::Error> {
        let keywords = super::query_expansion::extract_keywords(query);
        let fts_query = keywords.join(" OR ");
        if fts_query.is_empty() {
            return self.search_cjk_substrings(query, limit, None);
        }

        let mut stmt = self.db.prepare(
            "SELECT rowid, rank FROM chunks_fts WHERE chunks_fts MATCH ?1 \
             ORDER BY rank LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![fts_query, limit], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        self.merge_cjk_results(query, limit, None, rows)
    }

    /// SQLite's default FTS5 tokenizer treats a continuous Chinese sentence as
    /// one token. Match overlapping Han bigrams against the source chunks so a
    /// shorter Chinese phrase can find a longer saved sentence. The scan uses
    /// at most 16 bigrams and only runs for queries containing Han characters; English still
    /// follows the indexed BM25 path above.
    fn search_cjk_substrings(
        &self,
        query: &str,
        limit: usize,
        sources: Option<&[&str]>,
    ) -> Result<Vec<FtsResult>, rusqlite::Error> {
        let terms = super::query_expansion::extract_cjk_bigrams(query);
        if terms.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let score = vec!["(instr(c.text, ?) > 0)"; terms.len()].join(" + ");
        let source_clause = sources
            .filter(|items| !items.is_empty())
            .map(|items| format!(" AND c.source IN ({})", vec!["?"; items.len()].join(", ")))
            .unwrap_or_default();
        let sql = format!(
            "WITH scored AS (SELECT c.rowid, ({score}) AS hits FROM chunks c WHERE 1=1{source_clause}) \
             SELECT rowid, hits FROM scored WHERE hits > 0 ORDER BY hits DESC, rowid DESC LIMIT ?"
        );
        let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = terms
            .iter()
            .map(|term| Box::new(term.clone()) as Box<dyn rusqlite::types::ToSql>)
            .collect();
        if let Some(sources) = sources {
            args.extend(
                sources
                    .iter()
                    .map(|source| Box::new(source.to_string()) as Box<dyn rusqlite::types::ToSql>),
            );
        }
        args.push(Box::new(limit as i64));
        let refs: Vec<&dyn rusqlite::types::ToSql> = args.iter().map(|arg| arg.as_ref()).collect();
        let mut stmt = self.db.prepare(&sql)?;
        let rows = stmt
            .query_map(refs.as_slice(), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|(rowid, hits)| {
                // Use a bounded synthetic rank; lower (more negative) means better.
                (rowid, -(hits as f64 / terms.len() as f64))
            })
            .collect::<Vec<_>>();
        self.resolve_fts_rowids(rows)
    }

    fn merge_cjk_results(
        &self,
        query: &str,
        limit: usize,
        sources: Option<&[&str]>,
        rows: Vec<(i64, f64)>,
    ) -> Result<Vec<FtsResult>, rusqlite::Error> {
        let mut merged = self.resolve_fts_rowids(rows)?;
        let seen: std::collections::HashSet<i64> =
            merged.iter().map(|result| result.rowid).collect();
        let rank_bounds = merged
            .iter()
            .fold(None, |bounds: Option<(f64, f64)>, result| {
                Some(match bounds {
                    Some((best, worst)) => (best.min(result.rank), worst.max(result.rank)),
                    None => (result.rank, result.rank),
                })
            });
        merged.extend(
            self.search_cjk_substrings(query, limit, sources)?
                .into_iter()
                .filter(|result| !seen.contains(&result.rowid))
                .map(|mut result| {
                    if let Some((best, worst)) = rank_bounds {
                        let coverage = -result.rank;
                        result.rank = best + (1.0 - coverage) * (worst - best);
                    }
                    result
                }),
        );
        merged.sort_by(|left, right| left.rank.total_cmp(&right.rank));
        merged.truncate(limit);
        Ok(merged)
    }

    fn resolve_fts_rowids(&self, rows: Vec<(i64, f64)>) -> Result<Vec<FtsResult>, rusqlite::Error> {
        let mut results = Vec::with_capacity(rows.len());
        for (rowid, rank) in rows {
            if let Ok(chunk_id) = self.db.query_row(
                "SELECT id FROM chunks WHERE rowid = ?1",
                params![rowid],
                |row| row.get::<_, String>(0),
            ) {
                results.push(FtsResult {
                    chunk_id,
                    rowid,
                    rank,
                });
            }
        }
        Ok(results)
    }

    /// Get a chunk by its ID.
    pub fn get_chunk(&self, id: &str) -> Result<Option<ChunkRecord>, rusqlite::Error> {
        let mut stmt = self.db.prepare(
            "SELECT rowid, id, path, start_line, end_line, text, hash, source, access_count, \
             created_at FROM chunks WHERE id = ?1",
        )?;
        let result = stmt
            .query_row(params![id], |row| {
                Ok(ChunkRecord {
                    rowid: row.get(0)?,
                    id: row.get(1)?,
                    path: row.get(2)?,
                    start_line: row.get::<_, i64>(3)? as usize,
                    end_line: row.get::<_, i64>(4)? as usize,
                    text: row.get(5)?,
                    hash: row.get(6)?,
                    source: row.get(7)?,
                    access_count: row.get(8)?,
                    created_at: row.get(9)?,
                })
            })
            .ok();
        Ok(result)
    }

    /// Return the current value of the reindex claim from meta.
    ///
    /// An empty string means no claim is active.  A non-empty claim means
    /// a session currently owns the reindex lock (or a crashed session left
    /// a stale one).  Used by `echoagent memory doctor` to detect stuck states.
    pub fn get_reindex_claim(&self) -> String {
        self.db
            .query_row(
                "SELECT value FROM meta WHERE key = 'reindex_claim'",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap_or_default()
    }

    /// Return all distinct file paths that have at least one indexed chunk.
    ///
    /// Used by `echoagent memory doctor` to detect orphaned chunks (chunks whose
    /// source file has since been deleted).
    pub fn all_indexed_paths(&self) -> Result<Vec<String>, rusqlite::Error> {
        let mut stmt = self
            .db
            .prepare("SELECT DISTINCT path FROM chunks ORDER BY path")?;
        let paths = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(paths)
    }

    /// Record an access to a chunk (increments access_count, updates last_accessed).
    pub fn record_access(&mut self, chunk_id: &str) -> Result<(), rusqlite::Error> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        self.db.execute(
            "UPDATE chunks SET access_count = access_count + 1, last_accessed = ?1 WHERE id = ?2",
            params![now, chunk_id],
        )?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Vector operations (no-op if !vec_available)
    // -----------------------------------------------------------------------

    /// Ensure cached vectors belong to the active embedding space.
    /// Returns true when stale vectors were invalidated. This prevents silent
    /// cross-model similarity corruption when a model or endpoint changes but
    /// happens to use the same dimensions.
    pub fn ensure_embedding_fingerprint(&self, fingerprint: &str) -> Result<bool, rusqlite::Error> {
        if !self.vec_available {
            return Ok(false);
        }
        let stored: Option<String> = self
            .db
            .query_row(
                schema::GET_META_SQL,
                params!["embedding_fingerprint"],
                |row| row.get(0),
            )
            .ok();
        if stored.as_deref() == Some(fingerprint) {
            return Ok(false);
        }

        let tx = self.db.unchecked_transaction()?;
        tx.execute("DELETE FROM chunks_vec", [])?;
        tx.execute(
            schema::UPSERT_META_SQL,
            params!["embedding_fingerprint", fingerprint],
        )?;
        tx.commit()?;
        tracing::info!(
            target: crate::MEMORY_LOG_TARGET,
            had_previous = stored.is_some(),
            "embedding space changed; invalidated cached memory vectors"
        );
        Ok(true)
    }

    /// Return chunks that don't have embeddings yet.
    pub fn chunks_without_embeddings(&self) -> Result<Vec<(String, String)>, rusqlite::Error> {
        Ok(self
            .embedding_candidates()?
            .into_iter()
            .map(|c| (c.id, c.text))
            .collect())
    }

    pub(crate) fn embedding_candidates(&self) -> Result<Vec<EmbeddingCandidate>, rusqlite::Error> {
        if !self.vec_available {
            return Ok(vec![]);
        }
        let mut stmt = self.db.prepare(
            "SELECT c.id, c.path, c.text, c.hash FROM chunks c \
             LEFT JOIN chunks_vec_rowids v ON v.id = c.id \
             WHERE v.id IS NULL ORDER BY c.rowid",
        )?;
        let results = stmt
            .query_map([], |row| {
                Ok(EmbeddingCandidate {
                    id: row.get(0)?,
                    path: row.get(1)?,
                    text: row.get(2)?,
                    hash: row.get(3)?,
                    chunk_config: self.chunk_config.clone(),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(results)
    }

    /// Insert or update an embedding for a chunk.
    pub fn upsert_embedding(
        &self,
        chunk_id: &str,
        embedding: &[f32],
    ) -> Result<(), rusqlite::Error> {
        self.upsert_embedding_inner(chunk_id, embedding, None)
            .map(|_| ())
    }

    pub(crate) fn upsert_current_embedding(
        &self,
        candidate: &EmbeddingCandidate,
        embedding: &[f32],
        fingerprint: &str,
    ) -> Result<bool, rusqlite::Error> {
        self.upsert_embedding_inner(
            &candidate.id,
            embedding,
            Some((&candidate.hash, fingerprint)),
        )
    }

    fn upsert_embedding_inner(
        &self,
        chunk_id: &str,
        embedding: &[f32],
        expected: Option<(&str, &str)>,
    ) -> Result<bool, rusqlite::Error> {
        if !self.vec_available {
            return Ok(false);
        }
        if embedding.len() != self.embedding_dimensions
            || embedding.iter().any(|value| !value.is_finite())
        {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "embedding must contain {} finite dimensions",
                self.embedding_dimensions
            )));
        }
        let embedding_bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
        // vec0 does not implement SQLite's OR REPLACE conflict behavior.
        // Replace within one transaction so a repeated indexing pass updates
        // the vector, and an insert failure cannot remove the old embedding.
        let tx = self.db.unchecked_transaction()?;
        if let Some((hash, fingerprint)) = expected {
            let current: Option<String> = tx
                .query_row(
                    "SELECT hash FROM chunks WHERE id = ?1",
                    params![chunk_id],
                    |row| row.get(0),
                )
                .optional()?;
            let space: Option<String> = tx
                .query_row(
                    schema::GET_META_SQL,
                    params!["embedding_fingerprint"],
                    |row| row.get(0),
                )
                .optional()?;
            if current.as_deref() != Some(hash) || space.as_deref() != Some(fingerprint) {
                return Ok(false);
            }
        }
        tx.execute(
            "DELETE FROM chunks_vec WHERE chunk_id = ?1",
            params![chunk_id],
        )?;
        tx.execute(
            "INSERT INTO chunks_vec(chunk_id, embedding) VALUES (?1, ?2)",
            params![chunk_id, embedding_bytes],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// KNN vector search. Returns (chunk_id, distance) pairs.
    pub fn vector_search(
        &self,
        query_embedding: &[f32],
        k: usize,
    ) -> Result<Vec<(String, f32)>, rusqlite::Error> {
        if !self.vec_available {
            return Ok(vec![]);
        }
        if query_embedding.len() != self.embedding_dimensions
            || query_embedding.iter().any(|value| !value.is_finite())
        {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "query embedding must contain {} finite dimensions",
                self.embedding_dimensions
            )));
        }
        let query_bytes: Vec<u8> = query_embedding
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        let mut stmt = self.db.prepare(
            "SELECT chunk_id, distance FROM chunks_vec \
             WHERE embedding MATCH ?1 AND k = ?2 ORDER BY distance",
        )?;
        let results = stmt
            .query_map(params![query_bytes, k], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, f32>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(results)
    }

    // -----------------------------------------------------------------------
    // Reindex claim coordination (multi-agent)
    // -----------------------------------------------------------------------

    /// Try to claim exclusive reindex rights using the `meta` table.
    ///
    /// Uses an atomic UPDATE: succeeds only if unclaimed (empty) or stale
    /// (older than `stale_threshold_secs`). Returns `true` if claimed.
    /// Under SQLite's serialized writer model, at most one agent wins.
    pub fn try_claim_reindex(&self, stale_threshold_secs: i64) -> bool {
        let pid = std::process::id();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let claim_value = format!("{pid}:{now}");
        let stale_cutoff = now - stale_threshold_secs;

        let rows = self
            .db
            .execute(
                "UPDATE meta SET value = ?1 WHERE key = 'reindex_claim' \
             AND (value = '' OR CAST(SUBSTR(value, INSTR(value, ':') + 1) AS INTEGER) < ?2)",
                params![claim_value, stale_cutoff],
            )
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "reindex claim SQL failed, treating as claim-fail");
                0
            });

        rows == 1
    }

    /// Release the reindex claim. Call after reindex completes.
    pub fn release_claim(&self) {
        let _ = self
            .db
            .execute("UPDATE meta SET value = '' WHERE key = 'reindex_claim'", []);
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Delete all indexed chunks for a given file path.
    ///
    /// Called when the watcher detects a file-removal event.  Without this,
    /// chunks from deleted memory files remain searchable indefinitely.
    ///
    /// Deletes from all three tables (`chunks`, `chunks_fts`, `chunks_vec`) in
    /// a single transaction so the index stays consistent even on partial failure.
    ///
    /// Returns the number of chunks removed, which is 0 when the path was not
    /// previously indexed (idempotent).
    pub fn delete_path(&mut self, path: &Path) -> Result<usize, rusqlite::Error> {
        let path_str = path.to_string_lossy().to_string();
        let existing = self.get_chunks_for_path(&path_str)?;

        if existing.is_empty() {
            return Ok(0);
        }

        let tx = self.db.transaction()?;
        for (chunk_id, record) in &existing {
            // Contentless FTS5 requires the original text for the 'delete' command.
            tx.execute(
                "INSERT INTO chunks_fts(chunks_fts, rowid, text) VALUES('delete', ?1, ?2)",
                params![record.rowid, record.text],
            )?;
            if self.vec_available {
                let _ = tx.execute(
                    "DELETE FROM chunks_vec WHERE chunk_id = ?1",
                    params![chunk_id],
                );
            }
            tx.execute("DELETE FROM chunks WHERE id = ?1", params![chunk_id])?;
        }
        tx.commit()?;

        tracing::debug!(
            path = %path.display(),
            count = existing.len(),
            "memory index: deleted chunks for removed file"
        );
        Ok(existing.len())
    }

    /// Get all existing chunks for a path, keyed by chunk ID.
    fn get_chunks_for_path(
        &self,
        path: &str,
    ) -> Result<HashMap<String, ChunkRecord>, rusqlite::Error> {
        let mut stmt = self.db.prepare(
            "SELECT rowid, id, path, start_line, end_line, text, hash, source, access_count, \
             created_at FROM chunks WHERE path = ?1",
        )?;
        let rows = stmt.query_map(params![path], |row| {
            Ok(ChunkRecord {
                rowid: row.get(0)?,
                id: row.get(1)?,
                path: row.get(2)?,
                start_line: row.get::<_, i64>(3)? as usize,
                end_line: row.get::<_, i64>(4)? as usize,
                text: row.get(5)?,
                hash: row.get(6)?,
                source: row.get(7)?,
                access_count: row.get(8)?,
                created_at: row.get(9)?,
            })
        })?;
        let mut map = HashMap::new();
        for row in rows {
            let record = row?;
            map.insert(record.id.clone(), record);
        }
        Ok(map)
    }

    /// Get the rowid for a chunk by its text ID.
    #[expect(
        dead_code,
        reason = "used by future reindex_all and non-transactional paths"
    )]
    fn get_rowid(&self, id: &str) -> Result<Option<i64>, rusqlite::Error> {
        self.db
            .query_row(
                "SELECT rowid FROM chunks WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .ok()
            .map_or(Ok(None), |v| Ok(Some(v)))
    }

    /// Insert an FTS5 entry for a chunk. Uses contentless FTS5 — manual management.
    #[expect(
        dead_code,
        reason = "used by future reindex_all and non-transactional paths"
    )]
    fn insert_fts_entry(&self, rowid: i64, text: &str) -> Result<(), rusqlite::Error> {
        self.db.execute(
            "INSERT INTO chunks_fts(rowid, text) VALUES (?1, ?2)",
            params![rowid, text],
        )?;
        Ok(())
    }

    /// Delete an FTS5 entry. For contentless FTS5, this requires the original text.
    #[expect(
        dead_code,
        reason = "used by future reindex_all and non-transactional paths"
    )]
    fn delete_fts_entry(&self, rowid: i64, text: &str) -> Result<(), rusqlite::Error> {
        self.db.execute(
            "INSERT INTO chunks_fts(chunks_fts, rowid, text) VALUES('delete', ?1, ?2)",
            params![rowid, text],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_storage(tmp: &TempDir) -> MemoryStorage {
        let global = tmp.path().join("memory");
        let workspace = global.join("test_ws");
        MemoryStorage::with_paths(global, workspace)
    }

    fn test_index(tmp: &TempDir) -> MemoryIndex {
        let db_path = tmp.path().join("test.sqlite");
        let storage = test_storage(tmp);
        MemoryIndex::open_or_create(&db_path, storage, MemoryIndexConfig::default(), 1536).unwrap()
    }

    #[test]
    fn test_open_or_create_new_db() {
        let tmp = TempDir::new().unwrap();
        let idx = test_index(&tmp);
        assert_eq!(idx.embedding_dimensions(), 1536);
    }

    #[test]
    fn embedding_fingerprint_change_invalidates_cached_vectors() {
        init_sqlite_vec();
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("fingerprint.sqlite");
        let mut idx = MemoryIndex::open_or_create(
            &db_path,
            test_storage(&tmp),
            MemoryIndexConfig::default(),
            4,
        )
        .unwrap();
        if !idx.vec_available() {
            return;
        }
        let source = tmp.path().join("source.md");
        std::fs::write(&source, "# Knowledge\n\nDurable vector content.").unwrap();
        idx.reindex_file(&source, "workspace").unwrap();
        let chunk_id = format!("{}:0", source.to_string_lossy());

        assert!(idx.ensure_embedding_fingerprint("model-a:4").unwrap());
        idx.upsert_embedding(&chunk_id, &[0.1, 0.2, 0.3, 0.4])
            .unwrap();
        assert!(!idx.ensure_embedding_fingerprint("model-a:4").unwrap());
        assert!(idx.chunks_without_embeddings().unwrap().is_empty());

        assert!(idx.ensure_embedding_fingerprint("model-b:4").unwrap());
        assert_eq!(idx.chunks_without_embeddings().unwrap().len(), 1);
    }

    #[test]
    fn test_open_or_create_reopens_existing() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.sqlite");
        let storage = test_storage(&tmp);

        // Create
        {
            let _idx = MemoryIndex::open_or_create(
                &db_path,
                storage.clone(),
                MemoryIndexConfig::default(),
                1536,
            )
            .unwrap();
        }

        // Reopen
        let idx =
            MemoryIndex::open_or_create(&db_path, storage, MemoryIndexConfig::default(), 1536)
                .unwrap();
        assert_eq!(idx.embedding_dimensions(), 1536);
    }

    fn journal_mode(idx: &MemoryIndex) -> String {
        idx.db()
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn test_open_or_create_uses_wal_on_local_fs() {
        // Ambient kill-switch would override the decision; skip if set.
        if std::env::var("ECHO_AGENT_SQLITE_JOURNAL_MODE").is_ok() {
            return;
        }
        let tmp = TempDir::new().unwrap();
        assert_eq!(journal_mode(&test_index(&tmp)), "wal");
    }

    #[test]
    fn test_admin_open_preserves_custom_embedding_dimensions() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.sqlite");
        let storage = test_storage(&tmp);
        let original = MemoryIndex::open_or_create(
            &db_path,
            storage.clone(),
            MemoryIndexConfig::default(),
            768,
        )
        .unwrap();
        drop(original);
        let reopened = MemoryIndex::open_or_create_preserving_dimensions(
            &db_path,
            storage,
            MemoryIndexConfig::default(),
            1024,
        )
        .unwrap();
        assert_eq!(reopened.embedding_dimensions(), 768);
    }

    #[test]
    fn test_network_mode_uses_fresh_per_host_truncate_db() {
        // Network mode opens a per-host sibling of the given path (the
        // legacy shared file is left untouched — a live old binary can flip
        // it back to WAL at any time) in rollback-journal mode.
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.sqlite");
        let storage = test_storage(&tmp);

        let idx = MemoryIndex::open_or_create_with_journal_mode(
            &db_path,
            storage,
            MemoryIndexConfig::default(),
            512,
            JournalMode::Truncate,
        )
        .unwrap();
        assert_eq!(journal_mode(&idx), "truncate");
        assert_eq!(idx.embedding_dimensions(), 512);
        drop(idx);

        let eff = JournalMode::Truncate.effective_db_path(&db_path);
        assert_ne!(eff, db_path);
        assert!(eff.exists());
        let base = eff.display().to_string();
        assert!(!std::fs::exists(format!("{base}-wal")).unwrap());
        assert!(!std::fs::exists(format!("{base}-shm")).unwrap());
    }

    #[test]
    fn test_reindex_file_adds_chunks() {
        let tmp = TempDir::new().unwrap();
        let mut idx = test_index(&tmp);

        // Write a test memory file
        let file_path = tmp.path().join("test.md");
        std::fs::write(&file_path, "# Title\n\nSome content here.").unwrap();

        let result = idx.reindex_file(&file_path, "workspace").unwrap();
        assert_eq!(result.added, 1);
        assert_eq!(result.updated, 0);
        assert_eq!(result.removed, 0);
    }

    #[test]
    fn test_chinese_subphrase_finds_longer_memory_sentence() {
        let tmp = TempDir::new().unwrap();
        let mut idx = test_index(&tmp);
        let file = tmp.path().join("memory.md");
        std::fs::write(&file, "## 用户偏好\n\n用户喜欢简洁说明。\n").unwrap();
        idx.reindex_file(&file, "workspace").unwrap();
        let weak_file = tmp.path().join("weak.md");
        std::fs::write(&weak_file, "## 其他偏好\n\n用户喜欢简洁内容。\n").unwrap();
        idx.reindex_file(&weak_file, "workspace").unwrap();

        let matches = idx.search_fts("简洁说明", 10).unwrap();
        assert_eq!(matches.len(), 2);
        assert!(
            idx.get_chunk(&matches[0].chunk_id)
                .unwrap()
                .unwrap()
                .text
                .contains("简洁说明")
        );
        assert_eq!(
            idx.search_fts("简洁说明", 1).unwrap()[0].chunk_id,
            matches[0].chunk_id
        );
        let scoped = idx
            .search_fts_by_sources("简洁说明", 10, &["workspace"])
            .unwrap();
        assert_eq!(scoped.len(), 2);
        assert!(
            idx.search_fts_by_sources("简洁说明", 10, &["global"])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn test_reindex_file_detects_changes() {
        let tmp = TempDir::new().unwrap();
        let mut idx = test_index(&tmp);

        let file_path = tmp.path().join("test.md");
        std::fs::write(&file_path, "# Title\n\nOriginal content.").unwrap();
        let r1 = idx.reindex_file(&file_path, "workspace").unwrap();
        assert_eq!(r1.added, 1);

        // Modify file
        std::fs::write(&file_path, "# Title\n\nUpdated content.").unwrap();
        let r2 = idx.reindex_file(&file_path, "workspace").unwrap();
        assert_eq!(r2.updated, 1);
        assert_eq!(r2.added, 0);
    }

    #[test]
    fn test_reindex_file_removes_deleted_chunks() {
        let tmp = TempDir::new().unwrap();
        let mut idx = test_index(&tmp);

        let file_path = tmp.path().join("test.md");
        // Write file with content that produces 2+ chunks
        let big = format!(
            "## Section 1\n\n{}\n\n## Section 2\n\n{}",
            "A".repeat(2000),
            "B".repeat(2000)
        );
        std::fs::write(&file_path, &big).unwrap();
        let r1 = idx.reindex_file(&file_path, "workspace").unwrap();
        assert!(r1.added >= 2, "should add at least 2 chunks");

        // Shrink to 1 chunk
        std::fs::write(&file_path, "## Only Section\n\nSmall.").unwrap();
        let r2 = idx.reindex_file(&file_path, "workspace").unwrap();
        assert!(r2.removed > 0, "should remove old chunks");
    }

    #[test]
    fn test_reindex_unchanged_is_noop() {
        let tmp = TempDir::new().unwrap();
        let mut idx = test_index(&tmp);

        let file_path = tmp.path().join("test.md");
        std::fs::write(&file_path, "# Static\n\nContent.").unwrap();

        let r1 = idx.reindex_file(&file_path, "workspace").unwrap();
        assert_eq!(r1.added, 1);

        let r2 = idx.reindex_file(&file_path, "workspace").unwrap();
        assert_eq!(r2.added, 0);
        assert_eq!(r2.updated, 0);
        assert_eq!(r2.removed, 0);
    }

    #[test]
    fn test_fts_search() {
        let tmp = TempDir::new().unwrap();
        let mut idx = test_index(&tmp);

        let file_path = tmp.path().join("test.md");
        std::fs::write(&file_path, "# Guide\n\nRust programming language tutorial.").unwrap();
        idx.reindex_file(&file_path, "workspace").unwrap();

        let results = idx.search_fts("rust programming", 10).unwrap();
        assert!(!results.is_empty(), "FTS should find 'rust programming'");
    }

    #[test]
    fn test_fts_search_no_match() {
        let tmp = TempDir::new().unwrap();
        let mut idx = test_index(&tmp);

        let file_path = tmp.path().join("test.md");
        std::fs::write(&file_path, "# Guide\n\nPython tutorial.").unwrap();
        idx.reindex_file(&file_path, "workspace").unwrap();

        let results = idx.search_fts("haskell monads", 10).unwrap();
        assert!(results.is_empty(), "FTS should not find unrelated query");
    }

    #[test]
    fn test_get_chunk_and_record_access() {
        let tmp = TempDir::new().unwrap();
        let mut idx = test_index(&tmp);

        let file_path = tmp.path().join("test.md");
        std::fs::write(&file_path, "# Test\n\nChunk content.").unwrap();
        idx.reindex_file(&file_path, "workspace").unwrap();

        let path_str = file_path.to_string_lossy().to_string();
        let chunk_id = format!("{path_str}:0");

        let chunk = idx.get_chunk(&chunk_id).unwrap();
        assert!(chunk.is_some());
        let chunk = chunk.unwrap();
        assert_eq!(chunk.access_count, 0);

        idx.record_access(&chunk_id).unwrap();
        let chunk = idx.get_chunk(&chunk_id).unwrap().unwrap();
        assert_eq!(chunk.access_count, 1);
    }

    #[test]
    fn test_sqlite_vec_loads_and_reports_version() {
        init_sqlite_vec();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let version: String = conn
            .query_row("SELECT vec_version()", [], |r| r.get(0))
            .unwrap();
        assert!(!version.is_empty(), "sqlite-vec should report a version");
    }

    // -----------------------------------------------------------------------
    // Append-then-reindex regression test
    // -----------------------------------------------------------------------

    /// Simulates the `/memory append` → immediate-reindex flow.
    ///
    /// Previously the TUI's `AppendMemory` action wrote the file and returned
    /// without reindexing.  Appended content was only searchable after a future
    /// watcher-driven sync or the next session startup.  The fix reindexes
    /// immediately after append; this test ensures that regression cannot silently
    /// re-appear.
    #[test]
    fn test_append_then_reindex_is_immediately_searchable() {
        let tmp = TempDir::new().unwrap();
        let mut idx = test_index(&tmp);

        // Simulate MemoryStorage::append_to_memory writing the file.
        let mem_file = tmp.path().join("workspace_memory.md");
        std::fs::write(
            &mem_file,
            "## Rust Tip\n\nAlways prefer references over clones.",
        )
        .unwrap();

        // The content is NOT yet in the index — search returns nothing.
        let pre = idx.search_fts("prefer references over clones", 10).unwrap();
        assert!(
            pre.is_empty(),
            "content must not be searchable before reindex_file is called"
        );

        // Immediate reindex (what reindex_appended_memory in app.rs does).
        idx.reindex_file(&mem_file, "workspace").unwrap();

        // Now the content must be searchable without watcher or restart.
        let post = idx.search_fts("prefer references over clones", 10).unwrap();
        assert!(
            !post.is_empty(),
            "appended content must be immediately searchable after reindex_file"
        );
    }

    // -----------------------------------------------------------------------
    // delete_path tests
    // -----------------------------------------------------------------------

    /// Deleting an indexed file removes all its chunks and they are no longer searchable.
    #[test]
    fn test_delete_path_removes_chunks_and_fts_entries() {
        let tmp = TempDir::new().unwrap();
        let mut idx = test_index(&tmp);

        let file_path = tmp.path().join("removeme.md");
        std::fs::write(&file_path, "# Rust Guide\n\nRust ownership rules.").unwrap();
        idx.reindex_file(&file_path, "workspace").unwrap();

        // Precondition: FTS finds the file.
        let pre = idx.search_fts("rust ownership", 10).unwrap();
        assert!(!pre.is_empty(), "should find content before deletion");

        // Delete the path from the index (simulating a watcher Remove event).
        let removed = idx.delete_path(&file_path).unwrap();
        assert!(removed >= 1, "should report at least 1 chunk removed");

        // Post-condition: FTS no longer returns the deleted content.
        let post = idx.search_fts("rust ownership", 10).unwrap();
        assert!(
            post.is_empty(),
            "deleted file must not be searchable after delete_path"
        );
    }

    /// delete_path is idempotent — calling it twice (or on an unindexed file) is safe.
    #[test]
    fn test_delete_path_idempotent() {
        let tmp = TempDir::new().unwrap();
        let mut idx = test_index(&tmp);

        let file_path = tmp.path().join("idempotent.md");
        std::fs::write(&file_path, "# Entry\n\nSome content.").unwrap();
        idx.reindex_file(&file_path, "workspace").unwrap();

        let first = idx.delete_path(&file_path).unwrap();
        assert!(first >= 1);

        // Calling again on an already-deleted path returns 0 without error.
        let second = idx.delete_path(&file_path).unwrap();
        assert_eq!(second, 0, "second delete_path must be a no-op (idempotent)");
    }

    /// delete_path on a path that was never indexed returns 0.
    #[test]
    fn test_delete_path_never_indexed_returns_zero() {
        let tmp = TempDir::new().unwrap();
        let mut idx = test_index(&tmp);

        let phantom = tmp.path().join("phantom.md");
        let removed = idx.delete_path(&phantom).unwrap();
        assert_eq!(removed, 0, "unindexed path must return 0");
    }

    /// delete_path does not affect chunks from other files.
    #[test]
    fn test_delete_path_does_not_affect_other_files() {
        let tmp = TempDir::new().unwrap();
        let mut idx = test_index(&tmp);

        let keep = tmp.path().join("keep.md");
        let remove = tmp.path().join("remove.md");
        std::fs::write(&keep, "# Keep\n\nPython tutorial.").unwrap();
        std::fs::write(&remove, "# Remove\n\nRust tutorial.").unwrap();

        idx.reindex_file(&keep, "workspace").unwrap();
        idx.reindex_file(&remove, "workspace").unwrap();

        idx.delete_path(&remove).unwrap();

        // Python content from keep.md is still searchable.
        let keep_results = idx.search_fts("python", 10).unwrap();
        assert!(
            !keep_results.is_empty(),
            "chunks from other files must survive delete_path"
        );

        // Rust content from remove.md is gone.
        let remove_results = idx.search_fts("rust", 10).unwrap();
        assert!(
            remove_results.is_empty(),
            "chunks from deleted file must not appear"
        );
    }

    // -----------------------------------------------------------------------
    // access tracking + admin helper tests
    // -----------------------------------------------------------------------

    /// record_access increments access_count and sets last_accessed.
    #[test]
    fn test_record_access_increments_count() {
        let tmp = TempDir::new().unwrap();
        let mut idx = test_index(&tmp);

        let file = tmp.path().join("note.md");
        std::fs::write(&file, "# Guide\n\nRust testing.").unwrap();
        idx.reindex_file(&file, "workspace").unwrap();

        let path_str = file.to_string_lossy().to_string();
        let chunk_id = format!("{path_str}:0");

        // Initial access_count should be 0.
        let chunk_before = idx.get_chunk(&chunk_id).unwrap().unwrap();
        assert_eq!(chunk_before.access_count, 0, "access_count starts at 0");

        // Record two accesses.
        idx.record_access(&chunk_id).unwrap();
        idx.record_access(&chunk_id).unwrap();

        let chunk_after = idx.get_chunk(&chunk_id).unwrap().unwrap();
        assert_eq!(chunk_after.access_count, 2, "access_count must increment");
    }

    /// get_reindex_claim returns empty string when no claim is active.
    #[test]
    fn test_get_reindex_claim_empty_by_default() {
        let tmp = TempDir::new().unwrap();
        let idx = test_index(&tmp);
        assert_eq!(
            idx.get_reindex_claim(),
            "",
            "claim must be empty on fresh index"
        );
    }

    /// all_indexed_paths returns distinct paths for indexed chunks.
    #[test]
    fn test_all_indexed_paths() {
        let tmp = TempDir::new().unwrap();
        let mut idx = test_index(&tmp);

        let f1 = tmp.path().join("a.md");
        let f2 = tmp.path().join("b.md");
        std::fs::write(&f1, "# Alpha\n\nalpha content.").unwrap();
        std::fs::write(&f2, "# Beta\n\nbeta content.").unwrap();
        idx.reindex_file(&f1, "workspace").unwrap();
        idx.reindex_file(&f2, "workspace").unwrap();

        let paths = idx.all_indexed_paths().unwrap();
        let path_strs: Vec<&str> = paths.iter().map(String::as_str).collect();

        assert!(
            path_strs.contains(&f1.to_string_lossy().as_ref()),
            "a.md must be in indexed paths"
        );
        assert!(
            path_strs.contains(&f2.to_string_lossy().as_ref()),
            "b.md must be in indexed paths"
        );
        assert_eq!(
            paths.len(),
            2,
            "exactly two distinct paths should be indexed"
        );
    }

    /// all_indexed_paths returns empty for a fresh empty index.
    #[test]
    fn test_all_indexed_paths_empty_index() {
        let tmp = TempDir::new().unwrap();
        let idx = test_index(&tmp);
        let paths = idx.all_indexed_paths().unwrap();
        assert!(paths.is_empty(), "fresh index has no indexed paths");
    }

    // -----------------------------------------------------------------------
    // reindex maintenance path regression tests
    // -----------------------------------------------------------------------

    /// Regression test for the reindex maintenance flow:
    ///
    /// 1. Index a file.
    /// 2. Delete the file from disk (simulates a user removing a session log).
    /// 3. Run the same orphan-removal logic as `echoagent memory reindex`:
    ///    compare `all_indexed_paths()` against current files and call
    ///    `delete_path()` for paths that no longer exist.
    /// 4. Verify the stale chunks are gone and are no longer searchable.
    ///
    /// This proves that `echoagent memory reindex`'s Phase 1 actually fixes the
    /// state that `echoagent memory doctor` warns about.
    #[test]
    fn test_reindex_maintenance_removes_orphaned_chunks() {
        let tmp = TempDir::new().unwrap();
        let mut idx = test_index(&tmp);

        // Seed the index with a file containing uniquely-identifiable content.
        let file = tmp.path().join("session.md");
        std::fs::write(&file, "# Session\n\nXyzzy-orphan-regression-token.").unwrap();
        idx.reindex_file(&file, "workspace").unwrap();

        // Precondition: the content is searchable.
        let before = idx.search_fts("orphan-regression-token", 10).unwrap();
        assert!(
            !before.is_empty(),
            "should find content before file is deleted"
        );

        // Delete the file — now it is orphaned in the index.
        std::fs::remove_file(&file).unwrap();

        // Simulate `echoagent memory reindex` Phase 1: compare indexed vs current.
        let current: std::collections::BTreeSet<String> = vec![].into_iter().collect(); // empty = no files
        let indexed = idx.all_indexed_paths().unwrap();
        for path in &indexed {
            if !current.contains(path) {
                idx.delete_path(std::path::Path::new(path)).unwrap();
            }
        }

        // Postcondition: stale chunks are gone; search returns nothing.
        let after = idx.search_fts("orphan-regression-token", 10).unwrap();
        assert!(
            after.is_empty(),
            "orphaned chunks must be removed after reindex maintenance"
        );
    }

    /// A fresh (non-stale) reindex claim blocks `try_claim_reindex`.
    ///
    /// Verifies that `echoagent memory reindex` Phase 0 correctly bails when a
    /// live session holds a fresh claim — i.e., the CLI cannot steal a live
    /// session's lock and then mutate the index concurrently.
    #[test]
    fn test_try_claim_reindex_fails_when_fresh_claim_held() {
        let tmp = TempDir::new().unwrap();
        let idx = test_index(&tmp);

        // First caller claims unconditionally (stale threshold = i64::MAX means
        // any pre-existing claim is already "expired").
        let first = idx.try_claim_reindex(i64::MAX);
        assert!(first, "first claim should succeed on fresh index");

        // A second caller with STALE_SECS=60 must fail because the claim was
        // set moments ago and is not yet older than 60 seconds.
        let second = idx.try_claim_reindex(60);
        assert!(
            !second,
            "CLI reindex must not steal a fresh claim held by a live session"
        );

        // After the first owner releases, the CLI can claim successfully.
        idx.release_claim();
        let third = idx.try_claim_reindex(60);
        assert!(
            third,
            "CLI reindex must succeed after the live session releases"
        );
        idx.release_claim();
    }

    /// `echoagent memory reindex` Phase 3 resets the stale reindex claim.
    ///
    /// Verifies that `release_claim()` clears `meta.reindex_claim` so that
    /// `echoagent memory doctor` no longer reports a stale lock after reindex runs.
    #[test]
    fn test_reindex_maintenance_resets_stale_claim() {
        let tmp = TempDir::new().unwrap();
        let idx = test_index(&tmp);

        // Acquire the claim using stale_secs=0 (steals any existing claim)
        // so we start with a known non-empty claim value.
        let claimed = idx.try_claim_reindex(0);
        assert!(claimed, "try_claim_reindex should succeed on a fresh index");
        assert!(
            !idx.get_reindex_claim().is_empty(),
            "claim must be set before Phase 3"
        );

        // Simulate `echoagent memory reindex` Phase 3: release the claim.
        idx.release_claim();

        assert_eq!(
            idx.get_reindex_claim(),
            "",
            "release_claim must reset the claim so doctor reports no stale lock"
        );
    }
}
