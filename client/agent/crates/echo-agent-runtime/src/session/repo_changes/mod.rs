//! Serialize local repository changes (local commits + uncommitted worktree/index changes).
//!
//! This module provides high-level async functions for serializing repository changes
//! to a local archive or an in-memory reference structure.
pub use echo_agent_file_utils::BlobCompression;
pub use echo_agent_file_utils::{
    ARCHIVE_SCHEMA_VERSION, ARCHIVE_SCHEMA_VERSION_V3, DEDUP_BLOB_SUBDIR, DEDUP_GCS_PREFIX,
    DEDUP_PATCH_SUBDIR, DedupMetadata, ExcludedContent, FileReference, PatchReference,
    SKIP_DIR_NAMES, TraceExportConfig, UploadMethod, skip_dir_set,
};
