//! Turning a session record plus its extracted text into an index document.

use std::path::Path;

use crate::fts::{SessionDoc, SessionSearchIndex};
use crate::source::IndexableSession;

pub(crate) fn should_skip_session(updates_path: &Path, max_size: u64) -> bool {
    match std::fs::metadata(updates_path) {
        Ok(meta) => meta.len() > max_size,
        Err(_) => false,
    }
}

#[derive(Debug)]
pub(crate) enum UpsertOutcome {
    Indexed {
        bytes_read: u64,
    },
    /// Content hash matched the existing index entry.
    Unchanged {
        bytes_read: u64,
    },
    /// Nothing to write: the store exposes no updates file.
    NoContent,
}

/// Upsert `doc` unless the stored content hash already matches.
pub(crate) fn upsert_unless_unchanged(
    index: &SessionSearchIndex,
    doc: &SessionDoc,
    bytes_read: u64,
) -> Result<UpsertOutcome, rusqlite::Error> {
    if let Ok(Some(existing_hash)) = index.get_content_hash(&doc.session_id)
        && existing_hash == doc.content_hash
    {
        return Ok(UpsertOutcome::Unchanged { bytes_read });
    }
    index.upsert_doc(doc)?;
    Ok(UpsertOutcome::Indexed { bytes_read })
}

/// The title is hashed alongside the content so a rename is not deduped away
/// by an unchanged transcript.
pub(crate) fn build_session_doc(session: &IndexableSession, content: String) -> SessionDoc {
    let title = session.title.clone();

    let mut hasher = blake3::Hasher::new();
    hasher.update(title.as_bytes());
    hasher.update(b"\0");
    hasher.update(content.as_bytes());
    let content_hash = hasher.finalize().to_hex().to_string();

    SessionDoc {
        session_id: session.session_id.clone(),
        cwd: session.cwd.clone(),
        updated_at_unix: session.updated_at_unix,
        title,
        content,
        content_hash,
    }
}

#[cfg(test)]
#[path = "doc_tests.rs"]
mod tests;
