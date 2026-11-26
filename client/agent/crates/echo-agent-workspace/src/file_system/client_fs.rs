//! Read-only filesystem helpers backing the client-facing
//! `workspace.client_fs_*` RPCs (the cloud.echo-agent.invalid conversation-files UI,
//! tunneled through the server).
//!
//! Deliberately separate from the shell-facing ext ops in
//! [`ext_fs`](super::ext_fs): every path here is client-fs-base-relative
//! (`WorkspaceHandle::client_fs_base`) and resolves through the
//! root-confinement helper, the list walk excludes symlinks that resolve
//! outside the base (and never descends into them), listings paginate
//! with stable post-sort slices, and reads are binary-safe (base64
//! chunks).
//!
//! Wire types live in `echo_agent_workspace_types::rpc::fs` (the
//! `ClientFs*` types), shared with the backend caller.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use echo_agent_workspace_types::rpc::fs::{
    ClientFsListNode as FsListNode, ClientFsListReq as FsListReq, ClientFsListRes as FsListRes,
    ClientFsReadFileReq as FsReadFileReq, ClientFsReadFileRes as FsReadFileRes,
    ClientFsStatReq as FsStatReq, ClientFsStatRes as FsStatRes, FsContentType, FsNodeType,
};

use crate::error::{WorkspaceError, WorkspaceResult};
use crate::handle::{ClientFsBase, WorkspaceHandle};

/// Hard cap on entries collected per list call before sorting (shared
/// across all fs surfaces; see [`super::walk::MAX_LIST_COLLECT`]).
const MAX_LIST_COLLECT: usize = super::walk::MAX_LIST_COLLECT;

/// Server-side cap on `FsListReq::limit`.
const MAX_LIST_LIMIT: u32 = 1000;

/// Server-side cap on a single read's effective byte budget (shared
/// across all fs surfaces; see [`super::walk::MAX_READ_BYTES`]). Only
/// referenced by tests now that the clamp lives in `walk::clamp_read_length`.
#[cfg(test)]
const MAX_READ_BYTES: u64 = super::walk::MAX_READ_BYTES;

/// Bound on memoized hashes; the memo is cleared (not LRU-evicted) when
/// full — entries simply re-hash on next use.
const HASH_MEMO_CAPACITY: usize = 4096;

// =========================================================================
// (path, size, mtime_ms) → hash memo
// =========================================================================

#[derive(Debug, Clone)]
struct MemoEntry {
    size: u64,
    mtime_ms: i64,
    hash: String,
}

/// Memo of full-content SHA-256 digests keyed by absolute path and
/// validated against `(size, mtime_ms)`, so unchanged files hash once
/// instead of on every `client_fs_stat`. The memo only avoids redundant
/// hashing — it never substitutes mtime for content addressing: a
/// `(size, mtime_ms)` mismatch is a miss and the caller re-hashes.
#[derive(Debug, Default)]
pub(crate) struct FileHashMemo {
    entries: parking_lot::Mutex<HashMap<PathBuf, MemoEntry>>,
}

impl FileHashMemo {
    /// Return the memoized hash when `(size, mtime_ms)` still match.
    pub(crate) fn lookup(&self, path: &Path, size: u64, mtime_ms: i64) -> Option<String> {
        let entries = self.entries.lock();
        let entry = entries.get(path)?;
        (entry.size == size && entry.mtime_ms == mtime_ms).then(|| entry.hash.clone())
    }

    /// Record a freshly computed hash, replacing any stale entry for the
    /// same path. Clears the whole memo when inserting a new path would
    /// exceed [`HASH_MEMO_CAPACITY`].
    pub(crate) fn store(&self, path: &Path, size: u64, mtime_ms: i64, hash: String) {
        let mut entries = self.entries.lock();
        if !entries.contains_key(path) && entries.len() >= HASH_MEMO_CAPACITY {
            entries.clear();
        }
        entries.insert(
            path.to_path_buf(),
            MemoEntry {
                size,
                mtime_ms,
                hash,
            },
        );
    }
}

// =========================================================================
// Path resolution
// =========================================================================

/// Resolve a base-relative request path (`""`/`"."` = the base), rejecting
/// `..` and symlink escapes above the caller session's client-fs base.
async fn resolve_with_base(
    ws: &WorkspaceHandle,
    session_id: Option<&str>,
    path: &str,
) -> WorkspaceResult<(PathBuf, ClientFsBase)> {
    let rel = if path.is_empty() { "." } else { path };
    let base = ws.client_fs_base(session_id).await?;
    let abs = WorkspaceHandle::resolve_path_within_root(rel, &base.base, &base.canonical).await?;
    Ok((abs, base))
}

/// [`resolve_with_base`] for callers that don't need the base.
async fn resolve(
    ws: &WorkspaceHandle,
    session_id: Option<&str>,
    path: &str,
) -> WorkspaceResult<PathBuf> {
    resolve_with_base(ws, session_id, path)
        .await
        .map(|(abs, _)| abs)
}

fn system_time_ms(st: std::time::SystemTime) -> i64 {
    match st.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
        Err(e) => -i64::try_from(e.duration().as_millis()).unwrap_or(i64::MAX),
    }
}

// =========================================================================
// list
// =========================================================================

/// List `req.path` with stable pagination: collect the full walk (bounded
/// by [`MAX_LIST_COLLECT`]), sort directories-first / case-insensitive by
/// name, then slice `[offset, offset + limit)`. Symlinks resolving outside
/// the base are excluded from the walk (and never descended into).
pub(crate) async fn list(
    ws: &WorkspaceHandle,
    session_id: Option<&str>,
    req: &FsListReq,
) -> WorkspaceResult<FsListRes> {
    let (abs, base) = resolve_with_base(ws, session_id, &req.path).await?;
    let req = req.clone();
    // The walk does synchronous traversal + metadata syscalls; run it off
    // the async executor (matching the ext_fs ops).
    tokio::task::spawn_blocking(move || {
        list_blocking(&abs, &base.base, &base.canonical, &req, MAX_LIST_COLLECT)
    })
    .await
    .map_err(|e| WorkspaceError::JoinError(e.to_string()))?
}

fn list_blocking(
    abs_dir: &Path,
    base: &Path,
    canonical_base: &Path,
    req: &FsListReq,
    max_collect: usize,
) -> WorkspaceResult<FsListRes> {
    // Base confinement also holds mid-walk: a symlink inside the base
    // pointing outside must not enumerate outside metadata.
    let page = super::walk::list_directory_paged(
        abs_dir,
        super::walk::ListOptions {
            depth: req.depth as usize,
            follow_symlinks: req.follow_symlinks,
            respect_git_ignore: req.respect_git_ignore,
            include_hidden: req.include_hidden,
            include_globs: &req.include_globs,
            exclude_globs: &req.exclude_globs,
            offset: req.offset,
            limit: req.limit.min(MAX_LIST_LIMIT) as usize,
            confine_to_canonical_root: Some(canonical_base.to_path_buf()),
        },
        max_collect,
    );

    let nodes: Vec<FsListNode> = page
        .entries
        .into_iter()
        .map(|e| FsListNode {
            node_type: if e.is_dir {
                FsNodeType::Directory
            } else {
                FsNodeType::File
            },
            size: e.size,
            mtime_ms: e.modified.map(system_time_ms),
            is_symlink: e.is_symlink.then_some(true),
            // Base-relative path (divergent from the shell's absolute path).
            // A walk under a symlinked base yields canonical-spelled
            // entries, so strip either spelling.
            path: e
                .abs_path
                .strip_prefix(base)
                .or_else(|_| e.abs_path.strip_prefix(canonical_base))
                .unwrap_or(&e.abs_path)
                .to_string_lossy()
                .into_owned(),
            name: e.name,
        })
        .collect();

    Ok(FsListRes {
        nodes,
        truncated: page.truncated,
    })
}

// =========================================================================
// stat
// =========================================================================

/// Stat `req.path`: existence, kind, size, mtime, and — for files — a
/// full-content SHA-256 served through the workspace hash memo.
pub(crate) async fn stat(
    ws: &WorkspaceHandle,
    session_id: Option<&str>,
    req: &FsStatReq,
) -> WorkspaceResult<FsStatRes> {
    let abs = resolve(ws, session_id, &req.path).await?;
    let md = match tokio::fs::metadata(&abs).await {
        Ok(md) => md,
        // NotADirectory: a *file* sits mid-path (e.g. `a.txt/sub`) — for an
        // existence probe that is a miss, not an RPC error.
        Err(e)
            if e.kind() == std::io::ErrorKind::NotFound
                || e.kind() == std::io::ErrorKind::NotADirectory =>
        {
            return Ok(FsStatRes {
                exists: false,
                node_type: None,
                size: None,
                mtime_ms: None,
                hash: None,
            });
        }
        Err(e) => {
            return Err(WorkspaceError::HubError(format!(
                "stat failed for {}: {e}",
                req.path
            )));
        }
    };
    let mtime_ms = md.modified().ok().map(system_time_ms);
    if md.is_dir() {
        return Ok(FsStatRes {
            exists: true,
            node_type: Some(FsNodeType::Directory),
            size: None,
            mtime_ms,
            hash: None,
        });
    }
    let size = md.len();
    let memo = &ws.shared.client_fs_hash_memo;
    let hash = match mtime_ms.and_then(|m| memo.lookup(&abs, size, m)) {
        Some(hash) => hash,
        None => {
            let (hash, _, _) = crate::handle::stream_hash_and_range(&abs, 0, 0)
                .await
                .map_err(|e| {
                    WorkspaceError::HubError(format!("hash failed for {}: {e}", req.path))
                })?;
            if let Some(m) = mtime_ms {
                memo.store(&abs, size, m, hash.clone());
            }
            hash
        }
    };
    Ok(FsStatRes {
        exists: true,
        node_type: Some(FsNodeType::File),
        size: Some(size),
        mtime_ms,
        hash: Some(hash),
    })
}

// =========================================================================
// read_file
// =========================================================================

/// Read a byte range of `req.path` (binary-safe, capped at
/// `min(req.max_bytes, MAX_READ_BYTES)`) together with the full-file
/// SHA-256. When the hash is memoized for the current `(size, mtime)`
/// only the requested range is read; otherwise the whole file streams
/// once (via the shared [`crate::handle::stream_hash_and_range`]) to
/// hash it.
pub(crate) async fn read_file(
    ws: &WorkspaceHandle,
    session_id: Option<&str>,
    req: &FsReadFileReq,
) -> WorkspaceResult<FsReadFileRes> {
    let abs = resolve(ws, session_id, &req.path).await?;
    let read_err =
        |e: std::io::Error| WorkspaceError::HubError(format!("read failed for {}: {e}", req.path));
    let md = tokio::fs::metadata(&abs).await.map_err(read_err)?;
    if md.is_dir() {
        return Err(WorkspaceError::HubError(format!(
            "not a file: {}",
            req.path
        )));
    }
    let size = md.len();
    let mtime_ms = md.modified().ok().map(system_time_ms);
    let offset = req.offset.unwrap_or(0);
    // Server-side clamp: a hostile/buggy caller cannot lift the per-chunk
    // budget past MAX_READ_BYTES regardless of `maxBytes`.
    let length = super::walk::clamp_read_length(req.length, req.max_bytes);

    let memo = &ws.shared.client_fs_hash_memo;
    let (hash, chunk, size) = match mtime_ms.and_then(|m| memo.lookup(&abs, size, m)) {
        Some(hash) => {
            let chunk = super::walk::read_range(&abs, offset, length)
                .await
                .map_err(read_err)?;
            (hash, chunk, size)
        }
        None => {
            let (hash, chunk, streamed) =
                crate::handle::stream_hash_and_range(&abs, offset, length)
                    .await
                    .map_err(read_err)?;
            if let Some(m) = mtime_ms {
                memo.store(&abs, streamed, m, hash.clone());
            }
            (hash, chunk, streamed)
        }
    };

    // Shared encoder keeps the paired wire fields coherent with one UTF-8
    // validation pass; `type` is text iff the bytes were valid UTF-8.
    let (payload, is_text) = super::walk::encode_chunk(chunk, req.encoding);
    let (content, content_base64) = match payload {
        super::walk::ChunkPayload::Text(t) => (Some(t), None),
        super::walk::ChunkPayload::Base64(b) => (None, Some(b)),
    };
    let content_type = if is_text {
        FsContentType::Text
    } else {
        FsContentType::Binary
    };
    Ok(FsReadFileRes {
        content,
        content_base64,
        size,
        hash,
        content_type,
    })
}

#[cfg(test)]
mod tests {
    use base64::Engine;
    use echo_agent_workspace_types::rpc::fs::FsReadEncoding;

    use super::*;
    use crate::handle::tests::make_handle;

    fn list_req(path: &str) -> FsListReq {
        FsListReq {
            path: path.to_owned(),
            depth: 1,
            include_hidden: true,
            limit: 1000,
            offset: 0,
            follow_symlinks: true,
            respect_git_ignore: false,
            include_globs: vec![],
            exclude_globs: vec![],
        }
    }

    /// Fixture: root with files `b.txt`, `A.txt`, `c.txt` and dirs
    /// `Zeta`, `alpha`. Expected order: dirs first case-insensitive
    /// (`alpha`, `Zeta`), then files (`A.txt`, `b.txt`, `c.txt`).
    fn populate(root: &Path) {
        std::fs::write(root.join("b.txt"), b"bb").unwrap();
        std::fs::write(root.join("A.txt"), b"a").unwrap();
        std::fs::write(root.join("c.txt"), b"ccc").unwrap();
        std::fs::create_dir(root.join("Zeta")).unwrap();
        std::fs::create_dir(root.join("alpha")).unwrap();
    }

    /// `list_blocking` against `dir` as both walk root and workspace root
    /// (canonicalized for the confinement check, like production).
    fn list_dir(dir: &Path, req: &FsListReq, max_collect: usize) -> FsListRes {
        let canonical = dunce::canonicalize(dir).unwrap();
        list_blocking(dir, dir, &canonical, req, max_collect).unwrap()
    }

    #[test]
    fn list_sorts_dirs_first_case_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        populate(dir.path());
        let res = list_dir(dir.path(), &list_req(""), MAX_LIST_COLLECT);
        let names: Vec<&str> = res.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["alpha", "Zeta", "A.txt", "b.txt", "c.txt"]);
        assert!(!res.truncated);
        assert_eq!(res.nodes[0].node_type, FsNodeType::Directory);
        assert_eq!(res.nodes[2].node_type, FsNodeType::File);
        assert_eq!(res.nodes[2].size, Some(1));
        assert!(res.nodes[2].mtime_ms.is_some());
        // Paths are workspace-root-relative.
        assert_eq!(res.nodes[2].path, "A.txt");
    }

    /// Pagination slices the *sorted* listing, so consecutive pages have
    /// stable boundaries and concatenate to the full listing.
    #[test]
    fn list_paginates_post_sort_with_stable_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        populate(dir.path());
        let full = list_dir(dir.path(), &list_req(""), MAX_LIST_COLLECT);

        let mut paged = Vec::new();
        for page_start in [0u64, 2, 4] {
            let req = FsListReq {
                limit: 2,
                offset: page_start,
                ..list_req("")
            };
            let page = list_dir(dir.path(), &req, MAX_LIST_COLLECT);
            // truncated while more entries remain past this page.
            assert_eq!(page.truncated, page_start + 2 < full.nodes.len() as u64);
            paged.extend(page.nodes);
        }
        assert_eq!(paged, full.nodes);

        // Offset past the end yields an empty, non-truncated page.
        let req = FsListReq {
            offset: 100,
            ..list_req("")
        };
        let page = list_dir(dir.path(), &req, MAX_LIST_COLLECT);
        assert!(page.nodes.is_empty());
        assert!(!page.truncated);
    }

    /// The collection cap marks the result truncated even when the page
    /// itself is not full.
    #[test]
    fn list_collection_cap_truncates() {
        let dir = tempfile::tempdir().unwrap();
        populate(dir.path());
        let res = list_dir(dir.path(), &list_req(""), 2);
        assert_eq!(res.nodes.len(), 2);
        assert!(res.truncated);
    }

    #[test]
    fn list_caps_limit_at_server_max() {
        let dir = tempfile::tempdir().unwrap();
        populate(dir.path());
        let req = FsListReq {
            limit: u32::MAX,
            ..list_req("")
        };
        // Must not panic / overflow; the page is everything (< 1000).
        let res = list_dir(dir.path(), &req, MAX_LIST_COLLECT);
        assert_eq!(res.nodes.len(), 5);
    }

    /// Regression: the list walk must not traverse — or
    /// even surface — in-root symlinks that resolve outside the workspace
    /// root, while symlinks staying inside the root keep working.
    #[test]
    #[cfg(unix)]
    fn list_excludes_symlink_escapes_mid_walk() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), b"secret").unwrap();
        // Escaping symlink: root/escape_link -> <outside>.
        std::os::unix::fs::symlink(outside.path(), root.join("escape_link")).unwrap();
        // In-root symlink: root/good_link -> root/real_dir.
        std::fs::create_dir(root.join("real_dir")).unwrap();
        std::fs::write(root.join("real_dir/inner.txt"), b"inner").unwrap();
        std::os::unix::fs::symlink(root.join("real_dir"), root.join("good_link")).unwrap();

        let req = FsListReq {
            depth: 2,
            follow_symlinks: true,
            ..list_req("")
        };
        let res = list_dir(root, &req, MAX_LIST_COLLECT);
        let paths: Vec<&str> = res.nodes.iter().map(|n| n.path.as_str()).collect();
        assert!(
            !paths.iter().any(|p| p.contains("escape_link")),
            "escaping symlink (and its subtree) must be excluded: {paths:?}"
        );
        assert!(
            !paths.iter().any(|p| p.contains("secret.txt")),
            "outside entries must not be enumerated: {paths:?}"
        );
        // Confinement must not over-filter: in-root symlinks survive,
        // including descent through them.
        assert!(paths.contains(&"good_link"), "{paths:?}");
        assert!(paths.contains(&"good_link/inner.txt"), "{paths:?}");
        assert!(paths.contains(&"real_dir/inner.txt"), "{paths:?}");
        let good = res.nodes.iter().find(|n| n.path == "good_link").unwrap();
        assert_eq!(good.is_symlink, Some(true));
    }

    #[test]
    fn memo_lookup_hits_and_invalidates_on_mismatch() {
        let memo = FileHashMemo::default();
        let path = Path::new("/ws/a.txt");
        memo.store(path, 10, 1000, "h1".into());
        assert_eq!(memo.lookup(path, 10, 1000).as_deref(), Some("h1"));
        // Size change ⇒ miss.
        assert_eq!(memo.lookup(path, 11, 1000), None);
        // Mtime change ⇒ miss.
        assert_eq!(memo.lookup(path, 10, 2000), None);
        // Re-store replaces the stale entry.
        memo.store(path, 11, 2000, "h2".into());
        assert_eq!(memo.lookup(path, 11, 2000).as_deref(), Some("h2"));
        assert_eq!(memo.lookup(path, 10, 1000), None);
    }

    /// `stat` consults the memo (no re-hash for an unchanged file) and
    /// recomputes when `(size, mtime)` no longer match.
    #[tokio::test]
    async fn stat_uses_memo_until_file_changes() {
        let ws = make_handle();
        let root = ws.root_cwd().unwrap();
        std::fs::write(root.join("data.txt"), b"hello world").unwrap();

        let req = FsStatReq {
            path: "data.txt".into(),
        };
        let first = stat(&ws, None, &req).await.unwrap();
        assert!(first.exists);
        assert_eq!(first.node_type, Some(FsNodeType::File));
        assert_eq!(first.size, Some(11));
        let real_hash = first.hash.clone().expect("hash for files");

        // Plant a sentinel hash for the file's current (size, mtime). A
        // second stat must return the sentinel — proof it did not re-hash.
        let abs = root.join("data.txt");
        let md = std::fs::metadata(&abs).unwrap();
        let mtime = system_time_ms(md.modified().unwrap());
        ws.shared
            .client_fs_hash_memo
            .store(&abs, md.len(), mtime, "sentinel".into());
        let memoized = stat(&ws, None, &req).await.unwrap();
        assert_eq!(memoized.hash.as_deref(), Some("sentinel"));

        // A size change invalidates the memo entry and re-hashes.
        std::fs::write(&abs, b"hello brave new world").unwrap();
        let rehashed = stat(&ws, None, &req).await.unwrap();
        let new_hash = rehashed.hash.expect("hash for files");
        assert_ne!(new_hash, "sentinel");
        assert_ne!(new_hash, real_hash);
    }

    /// A path with a *file* as an intermediate component (`ENOTDIR`) is an
    /// existence miss, not an RPC error.
    #[tokio::test]
    async fn stat_enotdir_intermediate_reports_not_exists() {
        let ws = make_handle();
        let root = ws.root_cwd().unwrap();
        std::fs::write(root.join("file.txt"), b"x").unwrap();
        let res = stat(
            &ws,
            None,
            &FsStatReq {
                path: "file.txt/nested".into(),
            },
        )
        .await
        .unwrap();
        assert!(!res.exists);
        assert_eq!(res.node_type, None);
        assert_eq!(res.hash, None);
    }

    #[tokio::test]
    async fn stat_missing_path_reports_not_exists() {
        let ws = make_handle();
        let res = stat(
            &ws,
            None,
            &FsStatReq {
                path: "nope.txt".into(),
            },
        )
        .await
        .unwrap();
        assert!(!res.exists);
        assert_eq!(res.node_type, None);
        assert_eq!(res.hash, None);
    }

    #[tokio::test]
    async fn read_file_chunks_are_binary_safe_and_capped() {
        let ws = make_handle();
        let root = ws.root_cwd().unwrap();
        // Non-UTF-8 payload: every byte value once.
        let payload: Vec<u8> = (0u8..=255).collect();
        std::fs::write(root.join("blob.bin"), &payload).unwrap();

        let req = FsReadFileReq {
            path: "blob.bin".into(),
            // Bytes 200..210 are bare continuation bytes — never valid UTF-8.
            offset: Some(200),
            length: Some(50),
            max_bytes: 10, // cap below the requested length
            encoding: FsReadEncoding::Base64,
        };
        let res = read_file(&ws, None, &req).await.unwrap();
        assert_eq!(res.size, 256);
        assert_eq!(res.content, None);
        assert_eq!(res.content_type, FsContentType::Binary);
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(res.content_base64.unwrap())
            .unwrap();
        assert_eq!(bytes, payload[200..210], "maxBytes caps the chunk");

        // Full-file hash regardless of the requested range.
        use sha2::{Digest, Sha256};
        assert_eq!(res.hash, format!("{:x}", Sha256::digest(&payload)));

        // Memoized second read (range-only fast path) returns the
        // identical chunk + hash.
        let again = read_file(&ws, None, &req).await.unwrap();
        assert_eq!(again.hash, res.hash);
        let again_bytes = base64::engine::general_purpose::STANDARD
            .decode(again.content_base64.unwrap())
            .unwrap();
        assert_eq!(again_bytes, payload[200..210]);
    }

    /// `maxBytes` is server-capped at [`MAX_READ_BYTES`]:
    /// a caller-supplied huge budget cannot make the workspace buffer the
    /// whole file.
    #[tokio::test]
    async fn read_file_server_caps_max_bytes() {
        let ws = make_handle();
        let root = ws.root_cwd().unwrap();
        let payload = vec![0u8; (MAX_READ_BYTES + 100) as usize];
        std::fs::write(root.join("big.bin"), &payload).unwrap();

        let res = read_file(
            &ws,
            None,
            &FsReadFileReq {
                path: "big.bin".into(),
                offset: None,
                length: None,
                max_bytes: u64::MAX,
                encoding: FsReadEncoding::Base64,
            },
        )
        .await
        .unwrap();
        assert_eq!(res.size, payload.len() as u64);
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(res.content_base64.unwrap())
            .unwrap();
        assert_eq!(
            bytes.len() as u64,
            MAX_READ_BYTES,
            "clamped to the server cap"
        );
    }

    #[tokio::test]
    async fn read_file_utf8_default_and_binary_fallback() {
        let ws = make_handle();
        let root = ws.root_cwd().unwrap();
        std::fs::write(root.join("text.txt"), "héllo").unwrap();
        std::fs::write(root.join("bin.dat"), [0xff, 0xfe, 0x00]).unwrap();

        let text = read_file(
            &ws,
            None,
            &FsReadFileReq {
                path: "text.txt".into(),
                offset: None,
                length: None,
                max_bytes: 1_048_576,
                encoding: FsReadEncoding::Utf8,
            },
        )
        .await
        .unwrap();
        assert_eq!(text.content.as_deref(), Some("héllo"));
        assert_eq!(text.content_base64, None);
        assert_eq!(text.content_type, FsContentType::Text);

        // Invalid UTF-8 under the utf8 default degrades to base64.
        let bin = read_file(
            &ws,
            None,
            &FsReadFileReq {
                path: "bin.dat".into(),
                offset: None,
                length: None,
                max_bytes: 1_048_576,
                encoding: FsReadEncoding::Utf8,
            },
        )
        .await
        .unwrap();
        assert_eq!(bin.content, None);
        assert_eq!(bin.content_type, FsContentType::Binary);
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(bin.content_base64.unwrap())
            .unwrap();
        assert_eq!(bytes, [0xff, 0xfe, 0x00]);
    }

    #[tokio::test]
    async fn resolve_rejects_escapes() {
        let ws = make_handle();
        for path in ["/etc/passwd", "../escape.txt"] {
            let err = stat(
                &ws,
                None,
                &FsStatReq {
                    path: path.to_owned(),
                },
            )
            .await
            .expect_err("escape must be rejected");
            assert!(matches!(err, WorkspaceError::HubError(_)), "{err:?}");
        }
    }

    /// An absolute path *inside* the workspace root is accepted and stats
    /// the same file as its root-relative form.
    #[tokio::test]
    async fn resolve_accepts_absolute_within_root() {
        let ws = make_handle();
        let root = ws.root_cwd().unwrap();
        std::fs::write(root.join("data.txt"), b"hello").unwrap();

        let rel = stat(
            &ws,
            None,
            &FsStatReq {
                path: "data.txt".into(),
            },
        )
        .await
        .unwrap();
        assert!(rel.exists);

        let abs_path = root.join("data.txt").to_string_lossy().into_owned();
        let abs = stat(&ws, None, &FsStatReq { path: abs_path })
            .await
            .unwrap();
        assert!(abs.exists);
        assert_eq!(abs.node_type, rel.node_type);
        assert_eq!(abs.size, rel.size);
        assert_eq!(abs.hash, rel.hash);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn resolve_rejects_symlink_escape() {
        let ws = make_handle();
        let root = ws.root_cwd().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), b"secret").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("escape_link")).unwrap();

        let err = read_file(
            &ws,
            None,
            &FsReadFileReq {
                path: "escape_link/secret.txt".into(),
                offset: None,
                length: None,
                max_bytes: 1_048_576,
                encoding: FsReadEncoding::Base64,
            },
        )
        .await
        .expect_err("symlink escape must be rejected");
        assert!(
            err.to_string().contains("symlink escape"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn list_empty_path_lists_root() {
        let ws = make_handle();
        let root = ws.root_cwd().unwrap();
        std::fs::write(root.join("rooted.txt"), b"x").unwrap();
        let res = list(&ws, None, &list_req("")).await.unwrap();
        assert!(res.nodes.iter().any(|n| n.name == "rooted.txt"));
    }

    /// A session cwd that extends the root rebases the client-fs surface:
    /// paths are cwd-relative and root-level files are unreachable.
    #[tokio::test]
    async fn session_cwd_rebases_client_fs_surface() {
        let ws = make_handle();
        let root = ws.root_cwd().unwrap();
        std::fs::create_dir(root.join("artifacts")).unwrap();
        std::fs::write(root.join("rooted.txt"), b"r").unwrap();
        std::fs::write(root.join("artifacts").join("out.txt"), b"out").unwrap();
        ws.create_session_with_cwd("cwd-session", Some(root.join("artifacts")))
            .unwrap();
        let session = Some("cwd-session");

        let res = list(&ws, session, &list_req("")).await.unwrap();
        let names: Vec<&str> = res.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["out.txt"]);
        assert_eq!(res.nodes[0].path, "out.txt");

        let hit = stat(
            &ws,
            session,
            &FsStatReq {
                path: "out.txt".into(),
            },
        )
        .await
        .unwrap();
        assert!(hit.exists);
        assert_eq!(hit.size, Some(3));

        let read = read_file(
            &ws,
            session,
            &FsReadFileReq {
                path: "out.txt".into(),
                offset: None,
                length: None,
                max_bytes: 1_048_576,
                encoding: FsReadEncoding::Utf8,
            },
        )
        .await
        .unwrap();
        assert_eq!(read.content.as_deref(), Some("out"));

        let miss = stat(
            &ws,
            session,
            &FsStatReq {
                path: "rooted.txt".into(),
            },
        )
        .await
        .unwrap();
        assert!(!miss.exists, "root files are not visible under the cwd");
        let err = stat(
            &ws,
            session,
            &FsStatReq {
                path: "../rooted.txt".into(),
            },
        )
        .await
        .expect_err("escape above the session cwd must be rejected");
        assert!(matches!(err, WorkspaceError::HubError(_)), "{err:?}");
    }

    /// Root-cwd and unknown sessions keep the workspace-root base.
    #[tokio::test]
    async fn root_and_unknown_sessions_keep_root_base() {
        let ws = make_handle();
        let root = ws.root_cwd().unwrap();
        std::fs::write(root.join("rooted.txt"), b"x").unwrap();
        for session in [Some("main"), Some("never-bound")] {
            let res = list(&ws, session, &list_req("")).await.unwrap();
            assert!(
                res.nodes.iter().any(|n| n.name == "rooted.txt"),
                "{session:?} must list the workspace root"
            );
        }
    }

    /// Unusable session cwds fall back to the root base instead of failing
    /// every op: a directory missing on disk (e.g. an artifacts mount not
    /// yet established) and a `..`-containing cwd.
    #[tokio::test]
    async fn unusable_session_cwds_fall_back_to_root_base() {
        let ws = make_handle();
        let root = ws.root_cwd().unwrap();
        std::fs::write(root.join("rooted.txt"), b"x").unwrap();
        ws.create_session_with_cwd("missing-dir", Some(root.join("artifacts")))
            .unwrap();
        ws.create_session_with_cwd("dot-dot", Some(root.join("..")))
            .unwrap();
        for session in [Some("missing-dir"), Some("dot-dot")] {
            let res = list(&ws, session, &list_req("")).await.unwrap();
            assert!(
                res.nodes.iter().any(|n| n.name == "rooted.txt"),
                "{session:?} must fall back to the workspace root"
            );
        }
    }

    /// A session cwd whose suffix is a symlink out of the root falls back
    /// to the root base (rebasing there would widen confinement).
    #[tokio::test]
    #[cfg(unix)]
    async fn symlink_escape_session_cwd_falls_back_to_root_base() {
        let ws = make_handle();
        let root = ws.root_cwd().unwrap();
        std::fs::write(root.join("rooted.txt"), b"x").unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("escape_link")).unwrap();
        ws.create_session_with_cwd("escape", Some(root.join("escape_link")))
            .unwrap();
        let res = list(&ws, Some("escape"), &list_req("")).await.unwrap();
        assert!(res.nodes.iter().any(|n| n.name == "rooted.txt"));
    }
}
