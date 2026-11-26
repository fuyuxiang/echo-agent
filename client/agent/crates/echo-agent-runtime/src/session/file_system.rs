use std::path::{Path, PathBuf};

use anyhow::Result;
use base64::Engine;
use chrono::Utc;
use echo_agent_workspace::file_system::{self as wfs, FsReadEncoding};
use serde::Serialize;

#[derive(Clone, Debug)]
pub struct FsListParams {
    pub path: String,
    pub depth: usize,
    pub limit: usize,
    /// Pagination offset applied after the dirs-first sort (default 0).
    pub offset: u64,
    // WalkBuilder options
    pub include_hidden: bool,
    pub follow_symlinks: bool,
    pub respect_git_ignore: bool,
    pub include_globs: Vec<String>,
    pub exclude_globs: Vec<String>,
}

pub const ERROR_CODE_FILE_SIZE_EXCEEDED: &str = "FILE_SIZE_EXCEEDED";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FileSizeExceededError {
    pub path: String,
    pub size_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_lines: Option<u64>,
}

impl FileSizeExceededError {
    pub(crate) fn message(&self) -> String {
        let mut reasons = Vec::new();
        if let Some(limit) = self.limit_bytes {
            reasons.push(format!("{} bytes > {} byte limit", self.size_bytes, limit));
        }
        if let (Some(line_count), Some(limit)) = (self.line_count, self.limit_lines) {
            reasons.push(format!("{} lines > {} line limit", line_count, limit));
        }
        format!("File exceeds size limits: {}", reasons.join(", "))
    }
}

impl<T: Serialize> From<FileSizeExceededError> for super::result::ExtMethodResult<T> {
    fn from(err: FileSizeExceededError) -> Self {
        Self {
            result: None,
            error: serde_json::to_value(super::result::ExtMethodError::with_data(
                ERROR_CODE_FILE_SIZE_EXCEEDED,
                err.message(),
                err,
            ))
            .ok(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FsListNode {
    pub name: String,
    pub path: String,
    #[serde(rename = "type")]
    pub node_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_symlink: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FsListData {
    pub nodes: Vec<FsListNode>,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FsExistsData {
    pub exists: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FsReadFileData {
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_base64: Option<String>,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_count: Option<u64>,
    #[serde(rename = "type")]
    pub content_type: String,
}

pub async fn list(
    abs_path: &Path,
    params: &FsListParams,
    confine_to_canonical_root: Option<PathBuf>,
) -> Result<FsListData> {
    // Confined to the canonical root when set (escaping symlinks not enumerated);
    // `None` (the default) walks unconfined.
    let page = wfs::list_directory_paged(
        abs_path,
        wfs::ListOptions {
            depth: params.depth,
            follow_symlinks: params.follow_symlinks,
            respect_git_ignore: params.respect_git_ignore,
            include_hidden: params.include_hidden,
            include_globs: &params.include_globs,
            exclude_globs: &params.exclude_globs,
            offset: params.offset,
            limit: params.limit,
            confine_to_canonical_root,
        },
        wfs::MAX_LIST_COLLECT,
    );

    let nodes: Vec<FsListNode> = page
        .entries
        .into_iter()
        .map(|e| FsListNode {
            node_type: if e.is_dir { "directory" } else { "file" }.to_string(),
            size: e.size,
            modified_at: e.modified.map(|st| {
                let dt: chrono::DateTime<Utc> = st.into();
                dt.to_rfc3339()
            }),
            is_symlink: e.is_symlink.then_some(true),
            path: e.abs_path.to_string_lossy().into_owned(),
            name: e.name,
        })
        .collect();

    Ok(FsListData {
        nodes,
        truncated: page.truncated,
    })
}

pub async fn exists(abs_path: &Path) -> Result<FsExistsData> {
    let exists = tokio::fs::try_exists(abs_path).await.unwrap_or(false);
    Ok(FsExistsData { exists })
}

pub async fn read_file(abs_path: &Path) -> Result<FsReadFileData> {
    let bytes = tokio::fs::read(abs_path).await?;
    Ok(build_file_entry(&bytes))
}

/// Binary-safe ranged read: returns the chunk `[offset, offset + min(length,
/// max_bytes, cap))` with the full file `size`. `lineCount` is omitted and
/// the MIME `type` is a coarse text/binary tag (mid-file chunks defeat
/// magic-byte sniffing).
pub(crate) async fn read_file_ranged(
    abs_path: &Path,
    offset: u64,
    length: u64,
    max_bytes: u64,
    encoding: FsReadEncoding,
) -> Result<FsReadFileData> {
    let md = tokio::fs::metadata(abs_path).await?;
    if md.is_dir() {
        anyhow::bail!("not a file: {}", abs_path.display());
    }
    // Best-effort snapshot: a concurrent truncate/grow between here and
    // read_range can make `size` inconsistent with the returned chunk.
    let size = md.len();
    let length = wfs::clamp_read_length(Some(length), max_bytes);
    let chunk = wfs::read_range(abs_path, offset, length).await?;
    let (payload, is_text) = wfs::encode_chunk(chunk, encoding);
    let (content, content_base64) = match payload {
        wfs::ChunkPayload::Text(t) => (t, None),
        wfs::ChunkPayload::Base64(b) => (String::new(), Some(b)),
    };
    Ok(FsReadFileData {
        content,
        content_base64,
        size,
        line_count: None,
        content_type: if is_text {
            "text/plain".to_string()
        } else {
            "application/octet-stream".to_string()
        },
    })
}

pub(crate) fn check_file_size_limits(
    data: &FsReadFileData,
    path: &str,
    max_bytes: Option<usize>,
    max_lines: Option<usize>,
) -> Result<(), FileSizeExceededError> {
    let exceeds_bytes = max_bytes.is_some_and(|limit| data.size > limit as u64);
    let exceeds_lines =
        max_lines.is_some_and(|limit| data.line_count.is_some_and(|lc| lc > limit as u64));

    if exceeds_bytes || exceeds_lines {
        return Err(FileSizeExceededError {
            path: path.to_string(),
            size_bytes: data.size,
            line_count: data.line_count,
            limit_bytes: if exceeds_bytes {
                max_bytes.map(|l| l as u64)
            } else {
                None
            },
            limit_lines: if exceeds_lines {
                max_lines.map(|l| l as u64)
            } else {
                None
            },
        });
    }

    Ok(())
}

pub async fn write_file(abs_path: &Path, content: &str, create_dirs: bool) -> Result<()> {
    let abs_path = abs_path.to_path_buf();
    let content = content.to_string();

    tokio::task::spawn_blocking(move || {
        if create_dirs && let Some(parent) = abs_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&abs_path, content.as_bytes())
    })
    .await??;

    Ok(())
}

pub(crate) fn build_file_entry(bytes: &[u8]) -> FsReadFileData {
    let size = bytes.len() as u64;
    let inferred = infer::get(bytes).map(|t| t.mime_type().to_string());
    match String::from_utf8(bytes.to_vec()) {
        Ok(text) => FsReadFileData {
            line_count: Some(text.lines().count() as u64),
            content: text,
            content_base64: None,
            size,
            content_type: inferred.unwrap_or_else(|| "text/plain".to_string()),
        },
        Err(_) => FsReadFileData {
            content: String::new(),
            content_base64: Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
            size,
            line_count: None,
            content_type: inferred.unwrap_or_else(|| "application/octet-stream".to_string()),
        },
    }
}

pub async fn delete_file(abs_path: &Path) -> Result<()> {
    tokio::fs::remove_file(abs_path).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(limit: usize, offset: u64) -> FsListParams {
        FsListParams {
            path: String::new(),
            depth: 1,
            limit,
            offset,
            include_hidden: true,
            follow_symlinks: true,
            respect_git_ignore: false,
            include_globs: Vec::new(),
            exclude_globs: Vec::new(),
        }
    }

    #[tokio::test]
    async fn list_paginates_with_stable_offset() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for n in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(root.join(n), b"x").unwrap();
        }
        std::fs::create_dir(root.join("zd")).unwrap();
        let names = |d: FsListData| d.nodes.into_iter().map(|n| n.name).collect::<Vec<_>>();

        // Dir sorts ahead of files: [zd, a.txt | b.txt, c.txt].
        let p0 = list(root, &params(2, 0), Some(root.to_path_buf()))
            .await
            .unwrap();
        assert!(p0.truncated);
        assert_eq!(names(p0), vec!["zd", "a.txt"]);

        let p1 = list(root, &params(2, 2), Some(root.to_path_buf()))
            .await
            .unwrap();
        assert!(!p1.truncated);
        assert_eq!(names(p1), vec!["b.txt", "c.txt"]);
    }

    #[tokio::test]
    async fn read_file_ranged_is_binary_safe_and_keeps_full_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.bin");
        std::fs::write(&path, b"hello world").unwrap();

        // UTF-8 chunk → `content`; `size` stays the full file size.
        let d = read_file_ranged(&path, 0, 5, 1 << 20, FsReadEncoding::Utf8)
            .await
            .unwrap();
        assert_eq!(d.content, "hello");
        assert!(d.content_base64.is_none());
        assert_eq!(d.size, "hello world".len() as u64);
        assert!(d.line_count.is_none());

        // base64 of a mid-file chunk.
        let d = read_file_ranged(&path, 6, 5, 1 << 20, FsReadEncoding::Base64)
            .await
            .unwrap();
        assert!(d.content.is_empty());
        assert_eq!(
            d.content_base64,
            Some(base64::engine::general_purpose::STANDARD.encode(b"world")),
        );

        // Non-UTF-8 bytes fall back to base64 + octet-stream.
        std::fs::write(&path, [0xff_u8, 0x00]).unwrap();
        let d = read_file_ranged(&path, 0, 10, 1 << 20, FsReadEncoding::Utf8)
            .await
            .unwrap();
        assert!(d.content.is_empty());
        assert_eq!(d.content_type, "application/octet-stream");
    }

    #[tokio::test]
    async fn read_file_ranged_clamps_to_server_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.txt");
        std::fs::write(&path, vec![b'a'; 64]).unwrap();
        // length far over the file; clamped to file end, full size echoed.
        let d = read_file_ranged(&path, 0, u64::MAX, 1 << 20, FsReadEncoding::Utf8)
            .await
            .unwrap();
        assert_eq!(d.size, 64);
        assert_eq!(d.content.len(), 64);
    }
}
