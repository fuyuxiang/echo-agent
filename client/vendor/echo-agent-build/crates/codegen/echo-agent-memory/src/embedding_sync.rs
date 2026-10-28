//! Shared candidate selection for initial indexing and sync-on-search.
//! Policy skips are normal: they must never poison a batch of allowed text.

use crate::{
    MemoryStorage,
    embedding::{EmbeddingProvider, MemoryPolicyError},
    index::EmbeddingCandidate,
};

#[derive(Default)]
pub(crate) struct EmbeddingSync {
    pub vectors: Vec<(EmbeddingCandidate, Vec<f32>)>,
    pub skipped: usize,
    pub retry_needed: bool,
}

/// A later batch can finish after an earlier candidate was forgotten. Recheck
/// the whole pass while serialized with desktop edits, immediately at commit.
pub(crate) fn commit_embeddings(
    index: &crate::MemoryIndex,
    provider: &dyn EmbeddingProvider,
    progress: EmbeddingSync,
) -> usize {
    let Ok(_mutation) = crate::storage::memory_mutation_lock().lock() else {
        return 0;
    };
    let rules = match index.storage.recall_rules() {
        Ok(rules) => rules,
        Err(MemoryPolicyError::Disabled) => return 0,
        Err(error) => {
            tracing::warn!(target: crate::MEMORY_LOG_TARGET, %error, "cannot commit embeddings without current memory policy");
            return 0;
        }
    };
    let fingerprint = provider.fingerprint();
    let mut count = 0;
    for (candidate, vector) in progress.vectors {
        if !rules.permits_candidate_with_config(
            std::path::Path::new(&candidate.path),
            &candidate.text,
            &candidate.chunk_config,
        ) {
            continue;
        }
        match index.upsert_current_embedding(&candidate, &vector, &fingerprint) {
            Ok(true) => count += 1,
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(target: crate::MEMORY_LOG_TARGET, %error, "failed to commit memory embedding")
            }
        }
    }
    count
}

// Own no SQLite reference across await: the search backend's future must be Send.
pub(crate) async fn embed_candidates(
    storage: &MemoryStorage,
    provider: &dyn EmbeddingProvider,
    candidates: &[EmbeddingCandidate],
) -> EmbeddingSync {
    let mut result = EmbeddingSync::default();
    for batch in candidates.chunks(32) {
        // A correction can land between selection and the provider's preflight.
        // Re-select this batch under fresh rules, without retrying denied text.
        for attempt in 0..3 {
            let rules = match storage.recall_rules() {
                Ok(rules) => rules,
                Err(MemoryPolicyError::Disabled) => return result,
                Err(error) => {
                    tracing::warn!(target: crate::MEMORY_LOG_TARGET, %error, "memory embedding policy unavailable; retaining candidates for retry");
                    result.retry_needed = true;
                    return result;
                }
            };
            let allowed: Vec<_> = batch
                .iter()
                .filter(|c| {
                    rules.permits_candidate_with_config(
                        std::path::Path::new(&c.path),
                        &c.text,
                        &c.chunk_config,
                    )
                })
                .collect();
            if allowed.is_empty() {
                result.skipped += batch.len();
                break;
            }
            let texts: Vec<_> = allowed.iter().map(|c| c.text.as_str()).collect();
            let permit = storage.request_permit_for_candidates(&allowed);
            match tokio::time::timeout(
                crate::EMBEDDING_BATCH_TIMEOUT,
                provider.embed_batch_with_policy(&texts, permit),
            )
            .await
            {
                Ok(Ok(vectors)) => {
                    if let Err(error) = crate::embedding::validate_embedding_batch(
                        &vectors,
                        allowed.len(),
                        provider.dimensions(),
                    ) {
                        tracing::warn!(target: crate::MEMORY_LOG_TARGET, %error, "invalid memory embedding response; retaining candidates for retry");
                        result.retry_needed = true;
                        return result;
                    }
                    // Forgetting/disablement while HTTP is in flight also prevents commit.
                    let current_rules = match storage.recall_rules() {
                        Ok(rules) => rules,
                        Err(MemoryPolicyError::Disabled) => return result,
                        Err(_) => {
                            result.retry_needed = true;
                            return result;
                        }
                    };
                    result.skipped += batch.len() - allowed.len();
                    for (candidate, vector) in allowed.into_iter().zip(vectors) {
                        if current_rules.permits_candidate_with_config(
                            std::path::Path::new(&candidate.path),
                            &candidate.text,
                            &candidate.chunk_config,
                        ) {
                            result.vectors.push((candidate.clone(), vector));
                        } else {
                            result.skipped += 1;
                        }
                    }
                    break;
                }
                Ok(Err(error)) if error.downcast_ref::<MemoryPolicyError>().is_some() => {
                    if !matches!(
                        error.downcast_ref::<MemoryPolicyError>(),
                        Some(MemoryPolicyError::ContentVetoed)
                    ) {
                        result.retry_needed = matches!(
                            error.downcast_ref::<MemoryPolicyError>(),
                            Some(MemoryPolicyError::LedgerUnavailable(_))
                        );
                        tracing::debug!(target: crate::MEMORY_LOG_TARGET, %error, "memory embedding paused by live policy");
                        return result;
                    }
                    if attempt == 2 {
                        result.retry_needed = true;
                        tracing::debug!(target: crate::MEMORY_LOG_TARGET, "memory rules changed repeatedly; retaining current batch for retry");
                    }
                }
                Ok(Err(error)) => {
                    // Service failures stop the pass; continuing would multiply timeout costs.
                    tracing::warn!(target: crate::MEMORY_LOG_TARGET, %error, "memory embedding service failed; retaining remaining candidates for retry");
                    result.retry_needed = true;
                    return result;
                }
                Err(_) => {
                    tracing::warn!(target: crate::MEMORY_LOG_TARGET, "memory embedding timed out; retaining remaining candidates for retry");
                    result.retry_needed = true;
                    return result;
                }
            }
        }
    }
    if result.skipped > 0 {
        tracing::debug!(target: crate::MEMORY_LOG_TARGET, skipped = result.skipped, "memory candidates excluded by privacy policy");
    }
    result
}
