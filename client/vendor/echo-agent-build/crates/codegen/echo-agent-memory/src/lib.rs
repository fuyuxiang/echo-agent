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
pub mod flush;
pub mod index;
pub mod mmr;
pub mod observation;
pub mod query_expansion;
pub mod reranker;
pub mod schema;
pub mod search;
pub mod storage;
pub mod text_utils;
pub mod watcher;

pub use backend::{EndpointScopedCredentials, MemoryBackendImpl, MemoryBackendParams};
pub use index::{MemoryIndex, init_sqlite_vec};
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
    let chunks = match index.chunks_without_embeddings() {
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
    let mut embedded = 0;

    // Batch in groups of 32 (provider's typical max batch size)
    for batch in chunks.chunks(32) {
        let texts: Vec<&str> = batch.iter().map(|(_, text)| text.as_str()).collect();
        match tokio::time::timeout(EMBEDDING_BATCH_TIMEOUT, provider.embed_batch(&texts)).await {
            Ok(Ok(embeddings)) => {
                if let Err(error) = embedding::validate_embedding_batch(
                    &embeddings,
                    batch.len(),
                    provider.dimensions(),
                ) {
                    tracing::warn!(
                        target: MEMORY_LOG_TARGET,
                        %error,
                        "embedding provider returned an invalid batch"
                    );
                    break;
                }
                for ((chunk_id, _), embedding) in batch.iter().zip(embeddings.iter()) {
                    if let Err(e) = index.upsert_embedding(chunk_id, embedding) {
                        tracing::warn!(
                            target: MEMORY_LOG_TARGET,
                            chunk_id,
                            error = %e,
                            "failed to upsert embedding"
                        );
                    } else {
                        embedded += 1;
                    }
                }
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    target: MEMORY_LOG_TARGET,
                    error = %e,
                    batch_size = texts.len(),
                    "embedding batch failed, deferring remaining chunks"
                );
                break;
            }
            Err(_) => {
                tracing::warn!(
                    target: MEMORY_LOG_TARGET,
                    batch_size = texts.len(),
                    "embedding batch timed out, deferring remaining chunks"
                );
                break;
            }
        }
    }

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
