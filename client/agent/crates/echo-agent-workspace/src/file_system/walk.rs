//! Shared filesystem core for the fs list/read ops.
//!
//! Owns the `ignore::WalkBuilder` configuration, glob overrides, the
//! dirs-first paginated listing ([`list_directory_paged`]), and the
//! binary-safe ranged-read primitives ([`read_range`], [`encode_chunk`])
//! used by all three fs surfaces — the shell-local
//! `session::file_system`, the shell-facing
//! [`ext_fs`](super::ext_fs) `workspace.fs_*`, and the client-facing
//! [`client_fs`](super::client_fs) `workspace.client_fs_*` — so walk and
//! read fixes apply to every consumer. Each consumer maps the neutral
//! [`ListedEntry`] / [`ChunkPayload`] to its own wire shape (absolute vs
//! root-relative paths, RFC 3339 vs epoch-ms timestamps, MIME vs
//! text/binary type tags).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use base64::Engine;
use echo_agent_workspace_types::rpc::fs::FsReadEncoding;
use ignore::{WalkBuilder, overrides::OverrideBuilder};

/// Hard cap on entries collected per list call before sorting. A
/// pathological directory truncates (`truncated = true`) instead of
/// ballooning memory. Shared by every fs surface.
pub const MAX_LIST_COLLECT: usize = 50_000;

/// Server-side cap on a single ranged read's effective byte budget
/// (`min(length, max_bytes)`). 4 MiB raw (≈ 5.3 MiB base64) stays under
/// the server's 8 MiB frame cap. Shared by every fs read surface.
pub const MAX_READ_BYTES: u64 = 4 * 1024 * 1024;

/// Resolve a ranged read's effective byte budget, shared by every fs read
/// surface so the clamp policy can't drift between them. An absent `length`
/// means "to EOF", but the result is always capped at the caller's
/// `max_bytes` and the hard [`MAX_READ_BYTES`] server limit — so a short
/// read is expected, and callers detect "more data" by comparing the
/// returned bytes (at `offset`) against the file `size`.
pub fn clamp_read_length(length: Option<u64>, max_bytes: u64) -> u64 {
    length
        .unwrap_or(u64::MAX)
        .min(max_bytes)
        .min(MAX_READ_BYTES)
}

/// Walk configuration. Field semantics mirror the `echo.agent/fs/list` request.
pub(super) struct FsWalk<'a> {
    pub depth: usize,
    pub follow_symlinks: bool,
    pub respect_git_ignore: bool,
    pub include_hidden: bool,
    pub include_globs: &'a [String],
    pub exclude_globs: &'a [String],
    /// When set, symlink entries whose canonical target leaves this
    /// canonical root are excluded — and not descended into — so a walk
    /// of a confined tree cannot enumerate paths outside it. `None`
    /// preserves the shell's unconfined semantics.
    pub confine_to_canonical_root: Option<PathBuf>,
}

/// One raw walk entry; `metadata` follows symlinks (like `fs::metadata`).
pub(super) struct RawFsEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_symlink: bool,
    pub metadata: std::fs::Metadata,
}

/// Walk `abs_dir` per `opts`, collecting up to `max_entries` entries (the
/// root itself is skipped; unreadable entries are skipped without counting).
/// Returns `(entries, hit_cap)` where `hit_cap` means the walk stopped at
/// the cap with entries left over.
pub(super) fn walk_fs_entries(
    abs_dir: &Path,
    opts: FsWalk<'_>,
    max_entries: usize,
) -> (Vec<RawFsEntry>, bool) {
    let overrides = build_glob_overrides(abs_dir, opts.include_globs, opts.exclude_globs);
    let mut builder = WalkBuilder::new(abs_dir);
    builder
        .max_depth(Some(opts.depth))
        .follow_links(opts.follow_symlinks)
        .same_file_system(true)
        .standard_filters(true)
        .git_ignore(opts.respect_git_ignore)
        .git_global(opts.respect_git_ignore)
        .git_exclude(opts.respect_git_ignore)
        .hidden(!opts.include_hidden)
        .overrides(overrides);
    if let Some(canonical_root) = opts.confine_to_canonical_root {
        builder.filter_entry(move |dent| symlink_stays_in_root(dent.path(), &canonical_root));
    }

    let mut entries: Vec<RawFsEntry> = Vec::new();
    let mut hit_cap = false;
    for dent in builder.build() {
        let Ok(entry) = dent else { continue };
        if entry.depth() == 0 {
            continue;
        }
        if entries.len() >= max_entries {
            hit_cap = true;
            break;
        }
        let path = entry.path().to_path_buf();
        let is_symlink = std::fs::symlink_metadata(&path)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        let metadata = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        entries.push(RawFsEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            path,
            is_symlink,
            metadata,
        });
    }
    (entries, hit_cap)
}

/// `true` when `path` is not a symlink, or is a symlink whose canonical
/// target stays under `canonical_root`. Unverifiable symlinks (e.g.
/// dangling) are excluded — confinement fails closed.
fn symlink_stays_in_root(path: &Path, canonical_root: &Path) -> bool {
    let is_symlink = std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    if !is_symlink {
        return true;
    }
    dunce::canonicalize(path)
        .map(|c| c.starts_with(canonical_root))
        .unwrap_or(false)
}

pub(super) fn build_glob_overrides(
    base: &Path,
    include: &[String],
    exclude: &[String],
) -> ignore::overrides::Override {
    let mut ob = OverrideBuilder::new(base);
    for pat in include {
        let patt = if pat.starts_with('!') {
            pat.clone()
        } else {
            format!("!{}", pat)
        };
        let _ = ob.add(&patt);
    }
    for pat in exclude {
        let _ = ob.add(pat);
    }
    ob.build().unwrap_or_else(|_| {
        OverrideBuilder::new(base)
            .build()
            .expect("override build fallback")
    })
}

// =========================================================================
// Paginated listing
// =========================================================================

/// One listed node in neutral form (no wire serialization). Consumers map
/// this to their own node shape.
pub struct ListedEntry {
    /// File name (final path component).
    pub name: String,
    /// Absolute path on the workspace/host filesystem.
    pub abs_path: PathBuf,
    /// Whether the entry is a directory.
    pub is_dir: bool,
    /// Whether the entry itself is a symlink.
    pub is_symlink: bool,
    /// Size in bytes (files only).
    pub size: Option<u64>,
    /// Modification time, when readable.
    pub modified: Option<SystemTime>,
}

/// One page of a directory listing.
pub struct ListPage {
    /// The page slice `[offset, offset + limit)` after the dirs-first sort.
    pub entries: Vec<ListedEntry>,
    /// `true` when more entries exist beyond this page, or the collection
    /// cap was hit before the walk finished.
    pub truncated: bool,
}

/// Options for [`list_directory_paged`].
pub struct ListOptions<'a> {
    pub depth: usize,
    pub follow_symlinks: bool,
    pub respect_git_ignore: bool,
    pub include_hidden: bool,
    pub include_globs: &'a [String],
    pub exclude_globs: &'a [String],
    /// Pagination offset applied after the sort.
    pub offset: u64,
    /// Page size (already clamped by the caller as appropriate).
    pub limit: usize,
    /// When set, mid-walk symlink escapes outside this canonical root are
    /// excluded; `None` keeps the shell's unconfined semantics.
    pub confine_to_canonical_root: Option<PathBuf>,
}

/// Whether more entries exist than this page returned. True when entries
/// remain beyond the page (`end < total`) OR the walk hit the collection cap
/// AND the caller has not yet paged past the collected window
/// (`hit_cap && start < total`). Gating the cap term on `start < total` is
/// what makes a `while truncated { offset += limit }` loop terminate: once a
/// client has consumed every collected entry the flag drops to false instead
/// of reporting the (unreachable) over-cap remainder forever.
fn page_truncated(start: usize, end: usize, total: usize, hit_cap: bool) -> bool {
    end < total || (hit_cap && start < total)
}

/// Walk `abs_dir`, sort directories-first / case-insensitive (with exact
/// name as a deterministic tiebreak), then return the stable slice
/// `[offset, offset + limit)`. See [`page_truncated`] for the `truncated`
/// semantics (incomplete listing OR more pages, but terminating).
pub fn list_directory_paged(abs_dir: &Path, opts: ListOptions<'_>, max_collect: usize) -> ListPage {
    let (raw, hit_cap) = walk_fs_entries(
        abs_dir,
        FsWalk {
            depth: opts.depth,
            follow_symlinks: opts.follow_symlinks,
            respect_git_ignore: opts.respect_git_ignore,
            include_hidden: opts.include_hidden,
            include_globs: opts.include_globs,
            exclude_globs: opts.exclude_globs,
            confine_to_canonical_root: opts.confine_to_canonical_root,
        },
        max_collect,
    );

    let mut entries: Vec<ListedEntry> = raw
        .into_iter()
        .map(|e| ListedEntry {
            is_dir: e.metadata.is_dir(),
            size: e.metadata.is_file().then_some(e.metadata.len()),
            modified: e.metadata.modified().ok(),
            is_symlink: e.is_symlink,
            abs_path: e.path,
            name: e.name,
        })
        .collect();

    // Directories first, then case-insensitive by name; exact name as a
    // tiebreak so page boundaries are deterministic.
    entries.sort_by_cached_key(|n| (!n.is_dir, n.name.to_lowercase(), n.name.clone()));

    let total = entries.len();
    let start = usize::try_from(opts.offset)
        .unwrap_or(usize::MAX)
        .min(total);
    let end = start.saturating_add(opts.limit).min(total);
    let truncated = page_truncated(start, end, total, hit_cap);
    entries.truncate(end);
    let page = entries.split_off(start);
    ListPage {
        entries: page,
        truncated,
    }
}

// =========================================================================
// Ranged, binary-safe reads
// =========================================================================

/// A read chunk in the requested transfer encoding.
pub enum ChunkPayload {
    /// Valid UTF-8 text (caller places it in a `content` field).
    Text(String),
    /// Base64 of the raw bytes (caller places it in a `contentBase64`
    /// field). Used when `base64` is requested or the bytes are not UTF-8.
    Base64(String),
}

/// Encode `bytes` per `encoding`, returning the payload and whether the
/// bytes were valid UTF-8 (`is_text`). One UTF-8 validation pass; on the
/// `Utf8` request the error hands the bytes back for base64 fallback.
pub fn encode_chunk(bytes: Vec<u8>, encoding: FsReadEncoding) -> (ChunkPayload, bool) {
    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    match (encoding, String::from_utf8(bytes)) {
        (FsReadEncoding::Utf8, Ok(text)) => (ChunkPayload::Text(text), true),
        (FsReadEncoding::Utf8, Err(e)) => (ChunkPayload::Base64(b64(e.as_bytes())), false),
        (FsReadEncoding::Base64, Ok(text)) => (ChunkPayload::Base64(b64(text.as_bytes())), true),
        (FsReadEncoding::Base64, Err(e)) => (ChunkPayload::Base64(b64(e.as_bytes())), false),
    }
}

/// Read only `[offset, offset + length)` of `abs` (no hashing).
pub async fn read_range(abs: &Path, offset: u64, length: u64) -> std::io::Result<Vec<u8>> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    let mut f = tokio::fs::File::open(abs).await?;
    if offset > 0 {
        f.seek(std::io::SeekFrom::Start(offset)).await?;
    }
    let mut chunk = Vec::new();
    f.take(length).read_to_end(&mut chunk).await?;
    Ok(chunk)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_read_length_caps_at_max_bytes_and_hard_limit() {
        // Absent length -> capped at max_bytes.
        assert_eq!(clamp_read_length(None, 1024), 1024);
        // Explicit length above max_bytes -> max_bytes wins.
        assert_eq!(clamp_read_length(Some(8192), 1024), 1024);
        // Explicit length below max_bytes -> length wins.
        assert_eq!(clamp_read_length(Some(512), 1024), 512);
        // max_bytes above the hard server limit -> hard limit wins.
        assert_eq!(clamp_read_length(None, u64::MAX), MAX_READ_BYTES);
        assert_eq!(clamp_read_length(Some(u64::MAX), u64::MAX), MAX_READ_BYTES);
    }

    #[test]
    fn page_truncated_signals_more_pages_within_collected_set() {
        // 100 collected, no cap, page [0,10) -> more remain.
        assert!(page_truncated(0, 10, 100, false));
        // Last page [90,100) -> nothing remains.
        assert!(!page_truncated(90, 100, 100, false));
    }

    #[test]
    fn page_truncated_terminates_when_paging_past_collection_cap() {
        // Cap hit, total clamped to 50 collected, limit 10.
        // Populated pages stay truncated (listing is incomplete)...
        assert!(page_truncated(0, 10, 50, true));
        assert!(page_truncated(40, 50, 50, true));
        // ...but once the client pages past the collected window the flag
        // drops, so `while truncated { offset += limit }` terminates instead
        // of fetching empty pages forever.
        assert!(!page_truncated(50, 50, 50, true));
        assert!(!page_truncated(60, 50, 50, true));
    }
}
