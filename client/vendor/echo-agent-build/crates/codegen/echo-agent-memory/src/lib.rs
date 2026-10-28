//! Memory system for cross-session knowledge persistence.
//!
//! This crate provides a markdown-based memory storage layer that allows
//! EchoAgent to persist important information across sessions. Memory files are
//! stored under `~/.echo-agent/memory/` with workspace-scoped subdirectories
//! keyed by a blake3 hash of the workspace path.
//!
//! ## Data Layout
//!
//! ```text
//! ~/.echo-agent/memory/
//!   ├── MEMORY.md                         # Global curated knowledge
//!   └── {workspace_hash}/                 # Per-workspace (blake3(cwd)[..16])
//!       ├── MEMORY.md                     # Project-level curated knowledge
//!       └── sessions/
//!           └── YYYY-MM-DD-{slug}-{sid8}.md  # Session logs
//! ```
//!
//! ## Feature Flag
//!
//! Memory is enabled through `ECHO_AGENT_MEMORY`, `[memory] enabled`, or remote settings.
//! When disabled, this crate is not initialized by the host.

pub mod archive;
pub mod backend;
pub mod chunker;
pub mod dream;
pub mod dream_lock;
pub mod embedding;
mod embedding_sync;
pub mod flush;
pub mod index;
pub mod maintenance;
pub mod mmr;
pub mod observation;
pub mod query_expansion;
pub mod records;
pub mod reranker;
pub mod schema;
pub mod search;
pub mod storage;
pub mod text_utils;
pub mod watcher;

pub use backend::{EndpointScopedCredentials, MemoryBackendImpl, MemoryBackendParams};
pub use index::{MemoryIndex, init_sqlite_vec};
pub use maintenance::EmbeddingMaintenance;
pub use observation::*;
pub use storage::{MemoryScope, MemoryStorage};

pub(crate) const MEMORY_LOG_TARGET: &str = "echo_agent_memory";
pub(crate) const EMBEDDING_BATCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);
pub(crate) const QUERY_EMBEDDING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
pub(crate) const RERANK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Embed all chunks that don't have embeddings yet.
///
/// Queries the index for unembedded chunks, batches them through the
/// embedding provider, and upserts the results. Logs progress.
///
/// This is the async glue between the sync `MemoryIndex` and the async
/// `EmbeddingProvider`. Call after reindex, flush writes, or session-end writes.
pub async fn embed_missing_chunks(
    index: &MemoryIndex,
    provider: &dyn embedding::EmbeddingProvider,
) -> usize {
    if let Err(error) = index.ensure_embedding_fingerprint(&provider.fingerprint()) {
        tracing::warn!(
            target: MEMORY_LOG_TARGET,
            %error,
            "failed to validate embedding cache identity"
        );
        return 0;
    }
    let chunks = match index.embedding_candidates() {
        Ok(c) if c.is_empty() => return 0,
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                target: MEMORY_LOG_TARGET,
                error = %e,
                "failed to query chunks without embeddings"
            );
            return 0;
        }
    };

    let total = chunks.len();
    let progress = embedding_sync::embed_candidates(&index.storage, provider, &chunks).await;
    let embedded = embedding_sync::commit_embeddings(index, provider, progress);

    if embedded > 0 {
        tracing::info!(
            target: MEMORY_LOG_TARGET,
            embedded,
            total,
            "embedded missing chunks"
        );
    }
    embedded
}
