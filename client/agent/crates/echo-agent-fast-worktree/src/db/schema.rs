pub const SCHEMA_VERSION: u32 = 1;

pub const INIT_SQL: &str = r#"
PRAGMA busy_timeout = 5000;

CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS worktrees (
    id TEXT PRIMARY KEY,
    path TEXT UNIQUE NOT NULL,
    source_repo TEXT NOT NULL,
    repo_name TEXT NOT NULL,
    kind TEXT NOT NULL DEFAULT 'session',
    creation_mode TEXT NOT NULL DEFAULT 'linked',
    git_ref TEXT,
    head_commit TEXT,
    session_id TEXT,
    creator_pid INTEGER,
    created_at INTEGER NOT NULL,
    last_accessed_at INTEGER,
    status TEXT NOT NULL DEFAULT 'alive',
    metadata TEXT
);

CREATE INDEX IF NOT EXISTS idx_worktrees_repo ON worktrees(repo_name);
CREATE INDEX IF NOT EXISTS idx_worktrees_status_kind ON worktrees(status, kind);
CREATE INDEX IF NOT EXISTS idx_worktrees_session ON worktrees(session_id);
CREATE INDEX IF NOT EXISTS idx_worktrees_created ON worktrees(created_at);
"#;

pub const UPSERT_META: &str = "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)";
pub const GET_META: &str = "SELECT value FROM meta WHERE key = ?1";
