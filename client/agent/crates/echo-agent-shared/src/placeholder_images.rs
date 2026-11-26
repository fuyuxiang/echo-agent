//! Shared helper for resolving `[Image #N: <path>]` placeholders into
//! image bytes.
//!
//! Both the TUI ([`echo_agent_pager::prompt_images`]) and the server-side
//! ingestion path ([`crate::session::acp_session`]) need to recover image
//! bytes when a placeholder lacks an attached `PastedImage` /
//! `ContentBlock::Image` — e.g. a paste from a previous session's
//! prompt, a session reload, or a synthetic re-render. The two sides
//! share one canonical loader so the validation rules (extension
//! allowlist, prefix allowlist, size cap, image-decoder validation,
//! aggregate-bytes cap) cannot drift.
//!
//! ## Threat model
//!
//! The placeholder path is **user-controlled** but the user does not
//! explicitly opt in to reading any arbitrary file — they paste a chat
//! transcript fragment and the agent may resurrect it across sessions.
//! To stop the placeholder mechanism from becoming a generic file
//! exfiltration sink, the loader is intentionally conservative:
//!
//! * Canonicalises every candidate path (resolves `..` and symlinks).
//! * Asserts the canonical target lives under an explicit prefix
//!   allowlist (workspace cwd + a small set of common user-image
//!   directories under `$HOME`; never the whole `$HOME`). See
//!   [`default_allowed_prefixes`].
//! * Asserts the extension is in [`ALLOWED_IMAGE_EXTENSIONS`].
//! * Routes the bytes through the `image` crate's full header parser
//!   ([`image::ImageReader::with_guessed_format`] +
//!   `into_dimensions`) so magic-byte forgery — a PNG-prefix file
//!   followed by arbitrary content — is rejected. See
//!   [`PlaceholderLoadError::NotAnImage`].
//! * Rejects any canonical path containing a known sensitive-bundle
//!   subtree (`.photoslibrary/`, `.musiclibrary/`, etc.) even when the
//!   parent prefix is in the allowlist.
//! * Enforces a per-image byte cap, a per-prompt placeholder count
//!   cap, and a per-prompt aggregate-bytes cap so a single prompt
//!   cannot trigger huge sequential syscall chains or memory spikes.
//!
//! Wire format: `[Image #<n>: <absolute_path>]` — the producer is
//! [`echo_agent_pager::prompt_images::display_text`]. The shape of this
//! placeholder is part of the chat-history contract — do NOT change
//! it. The regex requires the literal `": "` separator that the
//! producer always emits; see [`extract_placeholders`].
//!
//! ## `file://` URI convention
//!
//! Both the TUI's `prompt_images::build_content_blocks_with_workspace`
//! and the server-side recovery in [`recover_orphan_placeholders`]
//! emit `file://{canonical.display()}` URIs **without** percent-encoding
//! the path. This deviates from RFC 3986 (a path with spaces should be
//! `%20`-encoded) but it is internally consistent across producer and
//! consumer: [`canonical_from_file_uri`] parses inbound URIs using both
//! the relaxed unencoded form **and** percent-decoded form so dedup
//! works against either convention. Do **not** add percent-encoding on
//! one side without also doing it on the other — past-issue: producer
//! / consumer asymmetry breaks dedup.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

/// Maximum size of a single placeholder-loaded image. Matches the TUI's
/// `MAX_SEND_BYTES` (50 MB) so a path that loads on the TUI side cannot
/// be silently rejected by the server-side fallback.
pub const MAX_PLACEHOLDER_IMAGE_BYTES: usize = 50_000_000;

/// Maximum number of placeholders the loader will process per call.
/// Caps **on-disk loads** at 16 per prompt; the regex scan itself is
/// unbounded but linear in input length and short-circuits via
/// [`Iterator::take`] before `filter_map` runs.
pub const MAX_PLACEHOLDERS_PER_PROMPT: usize = 16;

/// Per-prompt aggregate-bytes cap across recovered placeholder images.
/// Prevents 16 × 50 MB worst-case RSS spikes on memory-constrained
/// runners.
pub const MAX_PLACEHOLDER_AGGREGATE_BYTES: usize = 200 * 1024 * 1024;

/// `_meta` key under which an attached image's `[Image #N]` display number
/// is recorded on its ACP image block, so the server can resolve
/// `[Image #N]` tokens to the right attachment by number rather than list
/// position (the two diverge — see `AttachedImages` in `echo-agent-tools`).
pub const IMAGE_DISPLAY_NUMBER_META_KEY: &str = "echoagent.dev/imageDisplayNumber";

/// Build an ACP image-block `_meta` value carrying `display_number` under
/// [`IMAGE_DISPLAY_NUMBER_META_KEY`].
pub fn display_number_meta(display_number: usize) -> agent_client_protocol::Meta {
    let mut meta = agent_client_protocol::Meta::new();
    meta.insert(
        IMAGE_DISPLAY_NUMBER_META_KEY.to_owned(),
        serde_json::json!(display_number),
    );
    meta
}

/// Read the `[Image #N]` display number recorded in an image block's
/// `_meta`, if present.
pub fn display_number_from_meta(meta: Option<&agent_client_protocol::Meta>) -> Option<usize> {
    meta?
        .get(IMAGE_DISPLAY_NUMBER_META_KEY)?
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
}

/// Build the per-turn `[Image #N]` → reference registry (see
/// [`AttachedImages`](echo_agent_tools::types::resources::AttachedImages))
/// from the user's inline attached images.
///
/// The display number comes from each block's `_meta` (set by the TUI),
/// falling back to 1-based position for callers that don't record it. The
/// reference is one `image_edit`'s resolver can read directly: the bare
/// durable path (from the `file://` URI) when present, else a
/// `data:<mime>;base64,<data>` URL.
pub fn attached_image_references(
    images: &[agent_client_protocol::ImageContent],
) -> Vec<(usize, String)> {
    images
        .iter()
        .enumerate()
        .map(|(idx, image)| {
            let display_number = display_number_from_meta(image.meta.as_ref()).unwrap_or(idx + 1);
            let reference =
                if let Some(path) = image.uri.as_deref().and_then(|u| u.strip_prefix("file://")) {
                    path.to_owned()
                } else {
                    format!("data:{};base64,{}", image.mime_type, image.data)
                };
            (display_number, reference)
        })
        .collect()
}

/// File extensions accepted by the placeholder loader.
///
/// SVG is intentionally **not** in this list: SVG is XML text with no
/// reliable magic-byte signature, and adding it would expand the attack
/// surface (script tags, XXE) without a corresponding image-decoder
/// validation pass. Any future SVG support must be gated by a script
/// attack-surface review.
pub const ALLOWED_IMAGE_EXTENSIONS: &[&str] =
    &["png", "jpg", "jpeg", "gif", "webp", "bmp", "tiff", "tif"];

/// Substrings that, if present anywhere in a canonical path, deny the
/// load even when the parent prefix is in the allowlist. Covers macOS
/// bundle subtrees the user did not explicitly opt in to sharing
/// (`~/Pictures/X.photoslibrary/originals/...`), Trash, and Keychain
/// bundles.
///
/// **Platform contract.** Each needle uses forward-slash separators
/// and is matched case-sensitively against the canonical path. The
/// enforcement site
/// ([`load_canonical_placeholder_image`]) normalises `\` → `/` before
/// the substring check, so Windows paths are covered. macOS HFS+
/// volumes (case-insensitive by default) and case-sensitive APFS both
/// hit the case-sensitive match — every entry in this list is a
/// system-emitted name and is case-stable in practice. If a future
/// entry depends on user-typed casing, add a `to_ascii_lowercase` step
/// at both sites.
pub const DENY_PATH_CONTAINS: &[&str] = &[
    ".photoslibrary/",
    ".musiclibrary/",
    ".imovielibrary/",
    "/.Trash/",
    "/Library/Keychains/",
    "/Library/Containers/",
    "/.ssh/",
    "/.aws/",
    "/.gnupg/",
];

/// Compiled regex matching the TUI placeholder format
/// `[Image #<digits>: <path>]`.
///
/// * Producer emits exactly `": "` (colon, single space) as the
///   separator — see [`echo_agent_pager::prompt_images::display_text`].
///   The regex requires the same; a path token like `[Image #5:foo]`
///   does **not** match.
/// * The path capture excludes `]`, `\n`, and `\r` so the match
///   terminates cleanly at the placeholder boundary even on
///   Windows-style line endings or path strings containing other
///   bracket forms.
/// * Path captures may contain spaces (typical macOS paths in
///   `~/My Pictures`).
static IMAGE_PLACEHOLDER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[Image #(\d+): ([^\]\r\n]+?)\]").expect("placeholder regex is valid")
});

/// One occurrence of `[Image #N: <path>]` in arbitrary text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaceholderMatch {
    /// The `N` in `[Image #N: …]` (1-based per the TUI's convention).
    pub display_number: usize,
    /// The raw path string as it appeared in the text (no canonicalisation).
    pub path: String,
    /// Byte offsets into the source text covered by the full
    /// `[Image #N: …]` match — `text[start..end]` is the placeholder.
    pub span: (usize, usize),
}

/// Scan `text` for every well-formed placeholder.
///
/// Malformed forms (`[Image #3]`, `[Image #5: ]`, truncated, etc.) are
/// skipped without failing the whole scan. The regex iterator is
/// short-circuited via [`Iterator::take`] **before** `filter_map`, so a
/// pathological prompt with 100 000 placeholders does not consume
/// 100 000 captures — at most [`MAX_PLACEHOLDERS_PER_PROMPT`] are
/// inspected. Trade-off: a prompt with one invalid placeholder among
/// 16 valid ones may yield 15 results.
pub fn extract_placeholders(text: &str) -> Vec<PlaceholderMatch> {
    IMAGE_PLACEHOLDER_RE
        .captures_iter(text)
        .take(MAX_PLACEHOLDERS_PER_PROMPT)
        .filter_map(|cap| {
            let whole = cap.get(0)?;
            let n = cap.get(1)?.as_str().parse::<usize>().ok()?;
            let path = cap.get(2)?.as_str().trim();
            if path.is_empty() {
                return None;
            }
            Some(PlaceholderMatch {
                display_number: n,
                path: path.to_owned(),
                span: (whole.start(), whole.end()),
            })
        })
        .collect()
}

/// Rewrites every `[Image #N: <path>]` placeholder in `text` to the
/// shorter `[Image #N]` form, dropping the path component.
///
/// Run **after** the orphan-recovery pipeline has finished extracting
/// paths it needs to load. Once the image is attached inline the path
/// is redundant *and harmful*: the model treats it as a hint and may
/// call the `Read` tool on the path even though the bytes are already
/// in context. The bracketed anchor `[Image #N]` is preserved so the
/// model can still tell where in the prose the image was referenced.
///
/// Takes `String` by value so the common no-placeholder case returns
/// the input unchanged with zero allocations.
///
/// The scan is bounded by [`MAX_PLACEHOLDERS_PER_PROMPT`]; any extra
/// placeholders past the cap are left in their original form.
pub fn strip_paths_from_image_placeholders(text: String) -> String {
    use std::fmt::Write as _;
    // Fast path: probe with `is_match` (no `Captures` allocation) and
    // return the owned input unchanged when there is nothing to do.
    if !IMAGE_PLACEHOLDER_RE.is_match(&text) {
        return text;
    }
    let mut out = String::with_capacity(text.len());
    let mut last = 0usize;
    for cap in IMAGE_PLACEHOLDER_RE
        .captures_iter(&text)
        .take(MAX_PLACEHOLDERS_PER_PROMPT)
    {
        // Group 0 is the full match and group 1 is `(\d+)` — both are
        // structurally guaranteed by the regex.
        let whole = cap.get(0).expect("regex match always has group 0");
        let n = cap.get(1).expect("regex always has group 1").as_str();
        out.push_str(&text[last..whole.start()]);
        // `write!` to a String is infallible.
        let _ = write!(out, "[Image #{n}]");
        last = whole.end();
    }
    out.push_str(&text[last..]);
    out
}

/// Loaded image bytes plus a sniffed MIME type.
#[derive(Debug, Clone)]
pub struct LoadedPlaceholderImage {
    /// Raw image bytes read from disk. Ownership transferred to the
    /// caller so it can be base64-encoded or moved into a
    /// `ContentBlock::Image` without an intermediate clone.
    pub data: Vec<u8>,
    /// MIME type derived from the `image` crate's full header parser
    /// ([`image::ImageReader::with_guessed_format`] +
    /// `into_dimensions`). Always one of `image/png`, `image/jpeg`,
    /// `image/gif`, `image/webp`, `image/bmp`, `image/tiff`.
    pub mime_type: String,
}

/// Why a placeholder load was rejected.
///
/// The error variants are deliberately coarse-grained: an attacker with
/// log access should not be able to probe the filesystem by reading
/// distinct error messages. In particular, no variant carries the raw
/// `io::Error` string; an `io::ErrorKind` is retained where useful but
/// renderer-side messages stay generic.
#[derive(Debug, thiserror::Error)]
pub enum PlaceholderLoadError {
    /// Resolved path is outside every entry in the prefix allowlist
    /// (or matches a [`DENY_PATH_CONTAINS`] entry inside an allowed
    /// prefix).
    ///
    /// Returned **before** any further I/O so a path like `/etc/passwd`
    /// returns this variant rather than leaking that `/etc/passwd`
    /// exists.
    #[error("path is outside allowed prefixes")]
    OutsideAllowedPrefixes,
    /// Path could not be canonicalised (missing, permission denied,
    /// etc.). We never include the raw filesystem error to avoid log
    /// probing.
    #[error("path does not resolve")]
    CanonicalizeFailed,
    /// Extension is not in [`ALLOWED_IMAGE_EXTENSIONS`].
    #[error("unsupported image extension")]
    UnsupportedExtension,
    /// Resolved path is not a regular file (e.g. directory, FIFO).
    #[error("path is not a regular file")]
    NotAFile,
    /// `std::fs::read` failed after canonicalisation. The variant
    /// retains only the `io::ErrorKind`, not the verbose message.
    #[error("read failed: {0:?}")]
    ReadFailed(std::io::ErrorKind),
    /// File exceeds the configured per-image byte cap.
    #[error("file is {actual} bytes, exceeds {limit}-byte cap")]
    TooLarge { actual: usize, limit: usize },
    /// Bytes do not decode as a supported image. Routed through
    /// [`image::ImageReader::with_guessed_format`] +
    /// `into_dimensions`, so a file with PNG magic bytes followed by
    /// arbitrary content is rejected here.
    #[error("bytes do not decode as a supported image")]
    NotAnImage,
}

/// Build the canonical prefix allowlist with the workspace cwd plus a
/// small, intentional set of common user-image directories under
/// `$HOME`.
///
/// The list is canonicalised up-front so prefix checks against
/// canonical resolved paths work. Non-canonical paths are **never**
/// appended — if `dunce::canonicalize(workspace_cwd)` fails (transient
/// permission, missing dir), the workspace prefix is dropped entirely.
///
/// `$HOME` itself is **not** an allowed prefix: that would let
/// arbitrary placeholder paths under `~/.ssh`, `~/.aws`, `~/.config`,
/// etc. be exfil-able. Instead, only the typical user-paste image
/// directories are added.
pub fn default_allowed_prefixes(workspace_cwd: &Path) -> Vec<PathBuf> {
    default_allowed_prefixes_with_home(workspace_cwd, dirs::home_dir())
}

/// Test-injectable variant of [`default_allowed_prefixes`]. Production
/// code should call [`default_allowed_prefixes`]; tests pass an
/// explicit `home` so they don't depend on the ambient `$HOME`.
///
/// The returned `Vec` is canonical and deduplicated; **ordering is
/// alphabetical by OS path, not insertion order**. The prefix check in
/// [`load_placeholder_image`] uses `starts_with`, so order is
/// functionally irrelevant.
pub fn default_allowed_prefixes_with_home(
    workspace_cwd: &Path,
    home: Option<PathBuf>,
) -> Vec<PathBuf> {
    let mut prefixes: Vec<PathBuf> = Vec::new();
    match dunce::canonicalize(workspace_cwd) {
        Ok(canon) => prefixes.push(canon),
        Err(e) => {
            tracing::warn!(
                workspace_cwd = ?workspace_cwd,
                error_kind = ?e.kind(),
                "placeholder_images: workspace cwd does not canonicalize; dropping from allowlist",
            );
        }
    }
    if let Some(home) = home {
        for sub in HOME_IMAGE_SUBDIRS {
            if let Ok(canon) = dunce::canonicalize(home.join(sub)) {
                prefixes.push(canon);
            }
        }
    }
    prefixes.sort();
    prefixes.dedup();
    prefixes
}

/// Subdirectories under `$HOME` that are part of the default allowlist.
///
/// Chosen to match the directories users actually paste images from in
/// practice. Sensitive subtrees (`~/.ssh`, `~/.aws`, `~/.config`,
/// `~/.gnupg`, `~/Library/Keychains`) are intentionally excluded — they
/// are never added to the prefix list, and any path resolving into
/// [`DENY_PATH_CONTAINS`] is rejected even from inside an allowed
/// prefix.
pub const HOME_IMAGE_SUBDIRS: &[&str] = &[
    "Downloads",
    "Desktop",
    "Pictures",
    "Documents",
    "Screenshots",
];

/// Resolve and validate `path_str`, then read the file.
///
/// Validation order is **prefix-first** by design: an out-of-allowlist
/// path returns [`PlaceholderLoadError::OutsideAllowedPrefixes`]
/// regardless of whether the file exists, the extension is recognised,
/// or the bytes look like an image. This prevents log-level
/// information disclosure (an attacker with read access to telemetry
/// can no longer distinguish "file exists but is outside scope" from
/// "file does not exist").
///
/// `allowed_prefixes` should already be canonical (see
/// [`default_allowed_prefixes`]). The function does not canonicalise
/// `allowed_prefixes` again — the caller pays that cost once.
///
/// Symlinks: this loader follows symlinks (via `canonicalize`), then
/// checks the **resolved** path against the prefix allowlist. That is
/// strictly stronger than the legacy
/// [`echo_agent_pager::prompt_images::read_image_at_path`], which has no
/// prefix allowlist at all. The new rule applies to both
/// `[Image #N: <path>]` placeholder recovery callers — the
/// server-side `handle_prompt` fallback and the TUI orphan-placeholder
/// fallback. The legacy user-initiated drag/paste path in
/// `read_image_at_path` is intentionally outside this allowlist (the
/// user explicitly chose those files via the OS file picker).
pub fn load_placeholder_image(
    path_str: &str,
    allowed_prefixes: &[PathBuf],
) -> Result<LoadedPlaceholderImage, PlaceholderLoadError> {
    load_placeholder_image_with_cap(path_str, allowed_prefixes, MAX_PLACEHOLDER_IMAGE_BYTES)
}

/// Variant of [`load_placeholder_image`] that takes an explicit byte
/// cap. Used by tests so they can exercise the
/// [`PlaceholderLoadError::TooLarge`] path with a tiny cap and a small
/// file rather than synthesising a 50 MB blob.
pub fn load_placeholder_image_with_cap(
    path_str: &str,
    allowed_prefixes: &[PathBuf],
    max_bytes: usize,
) -> Result<LoadedPlaceholderImage, PlaceholderLoadError> {
    let canonical = dunce::canonicalize(Path::new(path_str))
        .map_err(|_| PlaceholderLoadError::CanonicalizeFailed)?;
    load_canonical_placeholder_image(&canonical, allowed_prefixes, max_bytes)
}

/// Variant of [`load_placeholder_image`] for callers that have already
/// canonicalised the path (e.g. [`recover_orphan_placeholders`], which
/// canonicalises once for dedup). Saves one `canonicalize` syscall per
/// successful load.
pub fn load_canonical_placeholder_image(
    canonical: &Path,
    allowed_prefixes: &[PathBuf],
    max_bytes: usize,
) -> Result<LoadedPlaceholderImage, PlaceholderLoadError> {
    // Prefix check first — out-of-scope paths return a single,
    // file-system-independent variant.
    if !allowed_prefixes.iter().any(|p| canonical.starts_with(p)) {
        return Err(PlaceholderLoadError::OutsideAllowedPrefixes);
    }
    // Deny-list pass: even inside an allowed prefix, certain subtrees
    // (macOS bundle internals, `.Trash`, secret stores) are off-limits.
    // Normalise `\` → `/` so Windows paths hit the same forward-slash
    // needles as Unix paths — see the `DENY_PATH_CONTAINS` doc-comment
    // for the platform contract.
    let canonical_str = canonical.to_string_lossy().replace('\\', "/");
    if DENY_PATH_CONTAINS
        .iter()
        .any(|needle| canonical_str.contains(needle))
    {
        return Err(PlaceholderLoadError::OutsideAllowedPrefixes);
    }

    let ext = canonical
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .ok_or(PlaceholderLoadError::UnsupportedExtension)?;
    if !ALLOWED_IMAGE_EXTENSIONS.contains(&ext.as_str()) {
        return Err(PlaceholderLoadError::UnsupportedExtension);
    }

    let metadata = canonical
        .metadata()
        .map_err(|e| PlaceholderLoadError::ReadFailed(e.kind()))?;
    if !metadata.is_file() {
        return Err(PlaceholderLoadError::NotAFile);
    }
    let size = metadata.len() as usize;
    if size > max_bytes {
        return Err(PlaceholderLoadError::TooLarge {
            actual: size,
            limit: max_bytes,
        });
    }

    let data = std::fs::read(canonical).map_err(|e| PlaceholderLoadError::ReadFailed(e.kind()))?;
    // Re-check after read: a sparse/grown file may exceed the cap
    // even when the metadata snapshot was under it.
    if data.len() > max_bytes {
        return Err(PlaceholderLoadError::TooLarge {
            actual: data.len(),
            limit: max_bytes,
        });
    }

    // Image-decoder validation: routed through the `image` crate's
    // header parser so a file with PNG magic bytes followed by arbitrary
    // content (e.g. a private key) is rejected. `into_dimensions` reads
    // the header (cheap) but not the pixel payload (expensive), so a
    // truncated/garbled image fails fast.
    let mime_type = decode_image_mime(&data).ok_or(PlaceholderLoadError::NotAnImage)?;

    Ok(LoadedPlaceholderImage {
        data,
        mime_type: mime_type.to_owned(),
    })
}

/// Header-only validation via the shared image_validate helper.
/// Returns the matching MIME type, or `None` if the bytes fail validation.
fn decode_image_mime(data: &[u8]) -> Option<&'static str> {
    echo_agent_tools::util::image_validate::validate_image_bytes_with(data, false)
        .ok()
        .map(|(_, _, mime)| mime)
}

/// Recover orphan `[Image #N: <path>]` placeholders embedded in the
/// user query text by loading the referenced files from disk.
///
/// Production wrapper over [`recover_orphan_placeholders_with_prefixes`]
/// that derives the prefix allowlist from `workspace_cwd` via
/// [`default_allowed_prefixes`].
///
/// **Note for integration tests.** This wrapper reads the ambient
/// process `$HOME` via `dirs::home_dir()` to construct
/// [`HOME_IMAGE_SUBDIRS`] prefixes. An end-to-end test driving
/// `handle_prompt` therefore inherits the test runner's `$HOME` and
/// any subdirectories it materialises (`~/Downloads`, etc.) into the
/// allowlist. For hermetic test isolation, call
/// [`recover_orphan_placeholders_with_prefixes`] directly with an
/// explicit prefix list — see the unit tests of this module for the
/// pattern.
pub fn recover_orphan_placeholders(
    query: &str,
    raw_images: &mut Vec<agent_client_protocol::ImageContent>,
    workspace_cwd: &Path,
) -> usize {
    let allowed = default_allowed_prefixes(workspace_cwd);
    recover_orphan_placeholders_with_prefixes(query, raw_images, &allowed)
}

/// Inject-friendly variant of [`recover_orphan_placeholders`].
///
/// "Orphan" = a placeholder whose canonical path is **not** already
/// present in `raw_images` (i.e. the TUI did not send a matching
/// `ContentBlock::Image`).
///
/// Dedup is performed against the **canonical** form of each existing
/// `raw_images[i].uri` so a TUI-attached non-canonical `file://`
/// URI (e.g. `file:///tmp/foo.png` when the canonical path is
/// `/private/tmp/foo.png`) still matches the placeholder's canonical
/// form. Percent-encoded forms are also handled (see
/// [`canonical_from_file_uri`]).
///
/// Enforces [`MAX_PLACEHOLDER_AGGREGATE_BYTES`] across the recovered
/// payloads: once the running total would exceed the cap, the
/// remainder of the placeholders are skipped with a `warn` log.
///
/// Returns the number of recovered images so the caller can log a
/// summary.
pub fn recover_orphan_placeholders_with_prefixes(
    query: &str,
    raw_images: &mut Vec<agent_client_protocol::ImageContent>,
    allowed_prefixes: &[PathBuf],
) -> usize {
    recover_orphan_placeholders_with_prefixes_and_caps(
        query,
        raw_images,
        allowed_prefixes,
        MAX_PLACEHOLDER_IMAGE_BYTES,
        MAX_PLACEHOLDER_AGGREGATE_BYTES,
    )
}

/// Test-injectable variant of
/// [`recover_orphan_placeholders_with_prefixes`].
///
/// Production code calls the cap-defaulting wrapper; tests use this
/// form to exercise the aggregate cap with small synthetic values
/// (real 200 MB tests would burn disk/CPU per run).
///
/// Aggregate-cap semantics: the loop reads the next image, then checks
/// `aggregate_bytes + image.len() > aggregate_max`. The first image
/// that pushes the running total **strictly above** the cap is
/// dropped and the loop `break`s; earlier images already in
/// `raw_images` stay. The cap is therefore an **inclusive** upper
/// bound on the aggregate — a running total exactly equal to the cap
/// is admitted, the next byte trips the break. See test
/// `recover_orphan_placeholders_aggregate_cap_inclusive_boundary`.
pub fn recover_orphan_placeholders_with_prefixes_and_caps(
    query: &str,
    raw_images: &mut Vec<agent_client_protocol::ImageContent>,
    allowed_prefixes: &[PathBuf],
    per_image_max: usize,
    aggregate_max: usize,
) -> usize {
    use base64::Engine as _;

    let placeholders = extract_placeholders(query);
    if placeholders.is_empty() {
        return 0;
    }

    // Pre-compute canonical paths of the already-attached images so the
    // dedup is robust against non-canonical `file://` URIs from the
    // TUI side as well as percent-encoded URIs from non-TUI clients.
    let attached_canonical: std::collections::HashSet<PathBuf> = raw_images
        .iter()
        .filter_map(|img| img.uri.as_deref().and_then(canonical_from_file_uri))
        .collect();

    let mut recovered: usize = 0;
    let mut aggregate_bytes: usize = 0;
    for ph in placeholders {
        let canonical = match dunce::canonicalize(Path::new(&ph.path)) {
            Ok(c) => c,
            Err(_) => {
                tracing::warn!(
                    path = ?ph.path,
                    "placeholder_images: orphan path does not resolve; leaving placeholder text intact",
                );
                continue;
            }
        };
        if attached_canonical.contains(&canonical) {
            continue;
        }
        match load_canonical_placeholder_image(&canonical, allowed_prefixes, per_image_max) {
            Ok(loaded) => {
                let next_total = aggregate_bytes.saturating_add(loaded.data.len());
                if next_total > aggregate_max {
                    tracing::warn!(
                        path = ?ph.path,
                        aggregate_bytes,
                        per_image_bytes = loaded.data.len(),
                        cap = aggregate_max,
                        "placeholder_images: aggregate-bytes cap reached; skipping remaining orphan placeholders",
                    );
                    break;
                }
                aggregate_bytes = next_total;
                let data = base64::engine::general_purpose::STANDARD.encode(&loaded.data);
                raw_images.push(
                    agent_client_protocol::ImageContent::new(data, loaded.mime_type)
                        .uri(format!("file://{}", canonical.display()))
                        // Record the real `[Image #N]` number so it resolves by
                        // number, matching the TUI-attached images (which set it
                        // too) and avoiding position-based collisions.
                        .meta(display_number_meta(ph.display_number)),
                );
                recovered += 1;
            }
            Err(e) => {
                tracing::warn!(
                    path = ?ph.path,
                    error = %e,
                    "placeholder_images: orphan placeholder failed to load",
                );
            }
        }
    }
    recovered
}

/// Parse a `file://...` URI into a canonical `PathBuf`, accepting both
/// the relaxed unencoded form emitted by the TUI / server (see the
/// `file://` URI convention note in the module header) and the
/// percent-encoded RFC 3986 form. Returns `None` if the URI does not
/// start with `file://`; otherwise returns the canonicalised path
/// (falling back to the raw path when canonicalisation fails so the
/// caller can still compare against attached URIs).
pub fn canonical_from_file_uri(uri: &str) -> Option<PathBuf> {
    let raw_path_str = uri.strip_prefix("file://")?;
    // Try percent-decoding first; fall back to the literal form. Both
    // are valid per the module's `file://` URI convention.
    let decoded: std::borrow::Cow<'_, str> = match urlencoding::decode(raw_path_str) {
        Ok(c) => c,
        Err(_) => std::borrow::Cow::Borrowed(raw_path_str),
    };
    let raw = Path::new(decoded.as_ref());
    Some(dunce::canonicalize(raw).unwrap_or_else(|_| raw.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Minimal valid 1x1 PNG (real format, decodable by `image`).
    const PNG_BYTES: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    fn write_png(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(PNG_BYTES).unwrap();
        path
    }

    // ----- strip_paths_from_image_placeholders ---------------------------

    #[test]
    fn strip_paths_drops_path_keeps_anchor() {
        // The whole point: the model should see the bracketed anchor
        // `[Image #N]` but not the path that would tempt a `Read`.
        assert_eq!(
            strip_paths_from_image_placeholders(
                "what is that?[Image #1: /Users/me/Desktop/x.png] thanks".to_owned()
            ),
            "what is that?[Image #1] thanks"
        );
    }

    #[test]
    fn strip_paths_handles_multiple_placeholders_and_spaces_in_paths() {
        assert_eq!(
            strip_paths_from_image_placeholders(
                "[Image #1: /tmp/a.png] mid [Image #2: /home/u/My Pictures/b.jpg] tail".to_owned()
            ),
            "[Image #1] mid [Image #2] tail"
        );
    }

    #[test]
    fn strip_paths_returns_input_unchanged_when_no_placeholders() {
        // Fast-path: the helper takes `String` by value and the
        // no-match branch returns it verbatim — no allocation. The
        // identity here pins the contract (input string ⇔ output
        // string) byte-for-byte.
        let text = "no placeholder here, just prose";
        assert_eq!(strip_paths_from_image_placeholders(text.to_owned()), text);
    }

    #[test]
    fn strip_paths_preserves_surrounding_whitespace_and_unicode() {
        assert_eq!(
            strip_paths_from_image_placeholders(
                "café \u{202f}[Image #4: /Users/me/Desktop/Screenshot 2026-05-22 at 16.01.21.png] ok"
                    .to_owned()
            ),
            "café \u{202f}[Image #4] ok"
        );
    }

    #[test]
    fn strip_paths_ignores_malformed_placeholders() {
        // None of these match the regex (the last is unterminated, the
        // middle two have empty paths). The leading `[Image #1]` is
        // already in the short form, so the output is bit-identical
        // to the input.
        let text = "[Image #1] [Image #2:] [Image #3: ] [Image #4: /ok.png";
        assert_eq!(strip_paths_from_image_placeholders(text.to_owned()), text);
    }

    // ----- extract_placeholders ------------------------------------------

    #[test]
    fn extract_placeholders_basic() {
        let text = "look at [Image #1: /tmp/a.png] and [Image #2: /home/user/b.jpg]";
        let matches = extract_placeholders(text);
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].display_number, 1);
        assert_eq!(matches[0].path, "/tmp/a.png");
        assert_eq!(matches[1].display_number, 2);
        assert_eq!(matches[1].path, "/home/user/b.jpg");
    }

    #[test]
    fn extract_placeholders_with_spaces_in_path() {
        let text = "[Image #3: /Users/me/My Pictures/cat.png]";
        let matches = extract_placeholders(text);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].path, "/Users/me/My Pictures/cat.png");
    }

    #[test]
    fn extract_placeholders_rejects_partial_and_invalid_forms() {
        assert!(extract_placeholders("[Image #3] only").is_empty());
        assert!(extract_placeholders("[Image #4: ]").is_empty());
        assert!(extract_placeholders("[Image #5: /tmp/x.png").is_empty());
        assert!(extract_placeholders("[image #1: /tmp/x.png]").is_empty());
        assert!(extract_placeholders("[Image #: /tmp/x.png]").is_empty());
        // Missing space after colon: producer always emits ": " so the
        // shorthand form is intentionally rejected. Pinned here.
        assert!(extract_placeholders("[Image #5:foo.png]").is_empty());
    }

    #[test]
    fn extract_placeholders_inside_larger_text_returns_spans() {
        let text = "before [Image #7: /tmp/a.png] after";
        let matches = extract_placeholders(text);
        assert_eq!(matches.len(), 1);
        let (start, end) = matches[0].span;
        assert_eq!(&text[start..end], "[Image #7: /tmp/a.png]");
    }

    #[test]
    fn extract_placeholders_unicode_path() {
        let text = "[Image #9: /tmp/café.png]";
        let matches = extract_placeholders(text);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].path, "/tmp/café.png");
    }

    #[test]
    fn extract_placeholders_large_display_number() {
        let text = "[Image #99999: /tmp/a.png]";
        let matches = extract_placeholders(text);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].display_number, 99999);
    }

    #[test]
    fn extract_placeholders_display_number_zero_is_kept() {
        let text = "[Image #0: /tmp/a.png]";
        let matches = extract_placeholders(text);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].display_number, 0);
    }

    #[test]
    fn extract_placeholders_multiple_on_same_line() {
        let text = "[Image #1: /tmp/a.png] [Image #2: /tmp/b.png] [Image #3: /tmp/c.png]";
        let matches = extract_placeholders(text);
        assert_eq!(matches.len(), 3);
        assert_eq!(matches[0].display_number, 1);
        assert_eq!(matches[1].display_number, 2);
        assert_eq!(matches[2].display_number, 3);
    }

    #[test]
    fn extract_placeholders_at_text_boundaries() {
        let matches = extract_placeholders("[Image #1: /tmp/a.png] tail");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].span.0, 0);
        let text2 = "head [Image #2: /tmp/b.png]";
        let matches2 = extract_placeholders(text2);
        assert_eq!(matches2.len(), 1);
        assert_eq!(matches2[0].span.1, text2.len());
    }

    #[test]
    fn extract_placeholders_rejects_carriage_return_in_path() {
        let text = "[Image #1: /tmp/a\r.png]";
        assert!(extract_placeholders(text).is_empty());
    }

    #[test]
    fn extract_placeholders_caps_at_max_per_prompt() {
        let chunk = "[Image #1: /tmp/a.png] ".repeat(MAX_PLACEHOLDERS_PER_PROMPT + 5);
        let matches = extract_placeholders(&chunk);
        assert_eq!(matches.len(), MAX_PLACEHOLDERS_PER_PROMPT);
    }

    #[test]
    fn extract_placeholders_first_nested_bracket_terminates() {
        // Pinning behaviour: `]` inside the path closes the match
        // early. Result: the first segment is captured as the path,
        // the rest of the text is left alone. Also pin the span so a
        // future regex revision that consumes nested brackets is
        // caught.
        let text = "[Image #1: /tmp/[odd].png]";
        let matches = extract_placeholders(text);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].path, "/tmp/[odd");
        let (start, end) = matches[0].span;
        assert_eq!(&text[start..end], "[Image #1: /tmp/[odd]");
    }

    // ----- load_placeholder_image ----------------------------------------

    #[test]
    fn load_placeholder_image_happy_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_png(dir.path(), "ok.png");
        let canon = dunce::canonicalize(dir.path()).unwrap();
        let loaded =
            load_placeholder_image(path.to_str().unwrap(), std::slice::from_ref(&canon)).unwrap();
        assert_eq!(loaded.mime_type, "image/png");
        assert_eq!(loaded.data, PNG_BYTES);
    }

    #[test]
    fn load_placeholder_image_rejects_missing_path() {
        let dir = tempfile::tempdir().unwrap();
        let canon = dunce::canonicalize(dir.path()).unwrap();
        let missing = dir.path().join("does-not-exist.png");
        let err = load_placeholder_image(missing.to_str().unwrap(), std::slice::from_ref(&canon))
            .unwrap_err();
        assert!(matches!(err, PlaceholderLoadError::CanonicalizeFailed));
    }

    #[test]
    fn load_placeholder_image_rejects_unsupported_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, b"hello").unwrap();
        let canon = dunce::canonicalize(dir.path()).unwrap();
        let err = load_placeholder_image(path.to_str().unwrap(), std::slice::from_ref(&canon))
            .unwrap_err();
        assert!(matches!(err, PlaceholderLoadError::UnsupportedExtension));
    }

    #[test]
    fn load_placeholder_image_rejects_outside_allowed_prefixes() {
        let real_dir = tempfile::tempdir().unwrap();
        let png = write_png(real_dir.path(), "real.png");
        let other_dir = tempfile::tempdir().unwrap();
        let other_canon = dunce::canonicalize(other_dir.path()).unwrap();
        let err = load_placeholder_image(png.to_str().unwrap(), std::slice::from_ref(&other_canon))
            .unwrap_err();
        assert!(matches!(err, PlaceholderLoadError::OutsideAllowedPrefixes));
    }

    #[test]
    fn load_placeholder_image_rejects_not_an_image_with_image_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fake.png");
        std::fs::write(&path, b"not actually a png").unwrap();
        let canon = dunce::canonicalize(dir.path()).unwrap();
        let err = load_placeholder_image(path.to_str().unwrap(), std::slice::from_ref(&canon))
            .unwrap_err();
        assert!(matches!(err, PlaceholderLoadError::NotAnImage));
    }

    /// Defence-in-depth regression for the exfil chain: a file whose
    /// first 8 bytes are PNG magic but whose tail is arbitrary content
    /// (e.g. a private key) is **rejected** by the loader. Earlier
    /// rounds pinned the weaker `image::guess_format`-only behaviour
    /// which accepted such forgeries; the full
    /// `with_guessed_format` + `into_dimensions` check closes that
    /// gap.
    #[test]
    fn load_placeholder_image_rejects_png_magic_forgery_with_garbage_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("forged.png");
        let mut bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        bytes.extend(b"PRIVATE KEY DATA - not a real PNG");
        std::fs::write(&path, &bytes).unwrap();
        let canon = dunce::canonicalize(dir.path()).unwrap();
        let err = load_placeholder_image(path.to_str().unwrap(), std::slice::from_ref(&canon))
            .unwrap_err();
        assert!(
            matches!(err, PlaceholderLoadError::NotAnImage),
            "expected NotAnImage, got: {err:?}"
        );
    }

    #[test]
    fn load_placeholder_image_rejects_directory_as_not_a_file() {
        let allowed = tempfile::tempdir().unwrap();
        let dir_path = allowed.path().join("looks-like.png");
        std::fs::create_dir(&dir_path).unwrap();
        let canon = dunce::canonicalize(allowed.path()).unwrap();
        let err = load_placeholder_image(dir_path.to_str().unwrap(), std::slice::from_ref(&canon))
            .unwrap_err();
        assert!(matches!(err, PlaceholderLoadError::NotAFile));
    }

    #[cfg(unix)]
    #[test]
    fn load_placeholder_image_reports_read_failure_on_unreadable_file() {
        // SAFETY: getuid is always safe; we just don't want to import libc.
        let euid = unsafe { libc::geteuid() };
        if euid == 0 {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("locked.png");
        std::fs::write(&path, PNG_BYTES).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let canon = dunce::canonicalize(dir.path()).unwrap();
        let err = load_placeholder_image(path.to_str().unwrap(), std::slice::from_ref(&canon))
            .unwrap_err();
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        assert!(
            matches!(err, PlaceholderLoadError::ReadFailed(_)),
            "expected ReadFailed, got: {err:?}"
        );
    }

    #[test]
    fn load_placeholder_image_rejects_oversize_via_injectable_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_png(dir.path(), "small-but-over-cap.png");
        let canon = dunce::canonicalize(dir.path()).unwrap();
        let err = load_placeholder_image_with_cap(
            path.to_str().unwrap(),
            std::slice::from_ref(&canon),
            32,
        )
        .unwrap_err();
        match err {
            PlaceholderLoadError::TooLarge { actual, limit } => {
                assert_eq!(actual, PNG_BYTES.len());
                assert_eq!(limit, 32);
            }
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn load_placeholder_image_rejects_symlink_escape() {
        let allowed_dir = tempfile::tempdir().unwrap();
        let outside_dir = tempfile::tempdir().unwrap();
        let target = write_png(outside_dir.path(), "secret.png");
        let link = allowed_dir.path().join("link.png");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let allowed_canon = dunce::canonicalize(allowed_dir.path()).unwrap();
        let err =
            load_placeholder_image(link.to_str().unwrap(), std::slice::from_ref(&allowed_canon))
                .unwrap_err();
        assert!(
            matches!(err, PlaceholderLoadError::OutsideAllowedPrefixes),
            "symlink escape must canonicalize then trip the prefix check, got: {err:?}",
        );
    }

    #[test]
    fn load_placeholder_image_rejects_path_traversal_via_dotdot() {
        let allowed_dir = tempfile::tempdir().unwrap();
        let outside_dir = tempfile::tempdir().unwrap();
        let unique = format!("ph-traversal-{}.png", uuid::Uuid::new_v4());
        let outside = outside_dir.path().join(&unique);
        std::fs::write(&outside, PNG_BYTES).unwrap();

        let traversal = allowed_dir
            .path()
            .join("..")
            .join(outside_dir.path().file_name().unwrap())
            .join(&unique);
        let allowed_canon = dunce::canonicalize(allowed_dir.path()).unwrap();
        let err = load_placeholder_image(
            traversal.to_str().unwrap(),
            std::slice::from_ref(&allowed_canon),
        )
        .unwrap_err();
        assert!(matches!(err, PlaceholderLoadError::OutsideAllowedPrefixes));
    }

    #[test]
    fn load_placeholder_image_rejects_deny_listed_subtree() {
        // Construct a workspace whose canonical contains
        // `/.photoslibrary/` so the path is inside the allowlist but
        // hits the deny-list.
        let root = tempfile::tempdir().unwrap();
        let bundle = root
            .path()
            .join("Photos Library.photoslibrary")
            .join("originals");
        std::fs::create_dir_all(&bundle).unwrap();
        let png = write_png(&bundle, "hash.png");
        let allowed_canon = dunce::canonicalize(root.path()).unwrap();
        let err =
            load_placeholder_image(png.to_str().unwrap(), std::slice::from_ref(&allowed_canon))
                .unwrap_err();
        assert!(
            matches!(err, PlaceholderLoadError::OutsideAllowedPrefixes),
            "deny-listed subtree must be rejected even inside allowlist, got: {err:?}"
        );
    }

    // ----- default_allowed_prefixes / _with_home --------------------------

    #[test]
    fn default_allowed_prefixes_with_home_includes_workspace_and_every_subdir() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        // Materialise every entry in HOME_IMAGE_SUBDIRS so the list
        // drift is caught at test time, not at runtime.
        let mut expected_subdir_canons: Vec<PathBuf> = Vec::new();
        for sub in HOME_IMAGE_SUBDIRS {
            let p = home.path().join(sub);
            std::fs::create_dir(&p).unwrap();
            expected_subdir_canons.push(dunce::canonicalize(&p).unwrap());
        }

        let prefixes =
            default_allowed_prefixes_with_home(dir.path(), Some(home.path().to_path_buf()));
        let dir_canon = dunce::canonicalize(dir.path()).unwrap();
        assert!(prefixes.contains(&dir_canon));
        for canon in &expected_subdir_canons {
            assert!(
                prefixes.contains(canon),
                "missing prefix for {canon:?} in {prefixes:?}"
            );
        }
        // $HOME itself is NOT in the list.
        assert!(!prefixes.contains(&dunce::canonicalize(home.path()).unwrap()));
        // At least workspace + each subdir, sorted+deduped. Uses `>=`
        // not `==` so the test stays green if `$TMPDIR` happens to
        // resolve inside one of the home subdirs (e.g. CI runners that
        // set `TMPDIR=$HOME/Downloads/ci`) — in that case the
        // workspace canonical would equal one of the subdir canonicals
        // and the dedup pass collapses them.
        assert!(
            prefixes.len() >= HOME_IMAGE_SUBDIRS.len(),
            "expected at least {} prefixes, got {prefixes:?}",
            HOME_IMAGE_SUBDIRS.len()
        );
    }

    #[test]
    fn default_allowed_prefixes_with_home_unset_returns_workspace_only() {
        let dir = tempfile::tempdir().unwrap();
        let prefixes = default_allowed_prefixes_with_home(dir.path(), None);
        let dir_canon = dunce::canonicalize(dir.path()).unwrap();
        assert_eq!(prefixes, vec![dir_canon]);
    }

    #[test]
    fn default_allowed_prefixes_dedups_workspace_equal_home_subdir() {
        let home = tempfile::tempdir().unwrap();
        let downloads = home.path().join("Downloads");
        std::fs::create_dir(&downloads).unwrap();
        let prefixes =
            default_allowed_prefixes_with_home(&downloads, Some(home.path().to_path_buf()));
        let dl_canon = dunce::canonicalize(&downloads).unwrap();
        let count = prefixes.iter().filter(|p| **p == dl_canon).count();
        assert_eq!(
            count, 1,
            "duplicate canonical prefix not deduped: {prefixes:?}"
        );
    }

    #[test]
    fn default_allowed_prefixes_drops_non_canonical_workspace() {
        let prefixes = default_allowed_prefixes_with_home(
            Path::new("/nonexistent/abs/path/we/never/created"),
            None,
        );
        assert!(
            prefixes.is_empty(),
            "non-canonical workspace must not be added to the allowlist: {prefixes:?}"
        );
    }

    // ----- canonical_from_file_uri ----------------------------------------

    #[test]
    fn canonical_from_file_uri_rejects_non_file_scheme() {
        assert!(canonical_from_file_uri("https://example.com/x.png").is_none());
        assert!(canonical_from_file_uri("/raw/path").is_none());
    }

    #[test]
    fn canonical_from_file_uri_handles_percent_encoded_path() {
        let dir = tempfile::tempdir().unwrap();
        let with_space = dir.path().join("My Pictures");
        std::fs::create_dir(&with_space).unwrap();
        let png = write_png(&with_space, "cat.png");
        let canon = dunce::canonicalize(&png).unwrap();
        // RFC 3986 form: spaces percent-encoded.
        let raw = format!("file://{}", png.display());
        let encoded = raw.replace(' ', "%20");
        let parsed = canonical_from_file_uri(&encoded).unwrap();
        assert_eq!(parsed, canon);
    }

    // ----- recover_orphan_placeholders (hermetic, no ambient $HOME) -------

    /// Build a non-empty ACP `ImageContent` so a future dedup change
    /// that short-circuits on `data.is_empty()` cannot silently pass
    /// these tests.
    fn make_acp_image(uri: &str) -> agent_client_protocol::ImageContent {
        agent_client_protocol::ImageContent::new("AAAA", "image/png").uri(Some(uri.to_string()))
    }

    #[test]
    fn recover_orphan_placeholders_loads_orphan() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_png(dir.path(), "rec.png");
        let canon = dunce::canonicalize(&path).unwrap();
        let query = format!("look at [Image #1: {}]", canon.display());
        let mut raw: Vec<agent_client_protocol::ImageContent> = Vec::new();
        let allowed = vec![dunce::canonicalize(dir.path()).unwrap()];
        let n = recover_orphan_placeholders_with_prefixes(&query, &mut raw, &allowed);
        assert_eq!(n, 1);
        assert_eq!(raw.len(), 1);
        assert_eq!(raw[0].mime_type, "image/png");
        assert!(!raw[0].data.is_empty());
        assert!(raw[0].uri.as_deref().unwrap().starts_with("file://"));
        // The recovered image carries its real `[Image #N]` number so
        // `image_edit` can resolve the token to it by number.
        assert_eq!(display_number_from_meta(raw[0].meta.as_ref()), Some(1));
    }

    #[test]
    fn recover_orphan_placeholders_dedupes_against_canonical_uri() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_png(dir.path(), "dup.png");
        let canon = dunce::canonicalize(&path).unwrap();
        let attached_uri = format!("file://{}", canon.display());
        let mut raw = vec![make_acp_image(&attached_uri)];
        let query = format!("see [Image #1: {}]", canon.display());
        let allowed = vec![dunce::canonicalize(dir.path()).unwrap()];
        let n = recover_orphan_placeholders_with_prefixes(&query, &mut raw, &allowed);
        assert_eq!(n, 0, "canonical-canonical dedup must skip the load");
        assert_eq!(raw.len(), 1);
        // The original entry must be untouched, not silently
        // overwritten by a duplicate load.
        assert_eq!(raw[0].data, "AAAA");
    }

    #[cfg(unix)]
    #[test]
    fn recover_orphan_placeholders_dedupes_against_non_canonical_tui_uri() {
        let dir = tempfile::tempdir().unwrap();
        let outside_root = tempfile::tempdir().unwrap();
        let real_target = write_png(outside_root.path(), "data.png");
        let link = dir.path().join("link.png");
        std::os::unix::fs::symlink(&real_target, &link).unwrap();

        let attached_uri = format!("file://{}", link.display()); // non-canonical
        let mut raw = vec![make_acp_image(&attached_uri)];
        let canonical_placeholder = dunce::canonicalize(&real_target).unwrap();
        let query = format!("[Image #1: {}]", canonical_placeholder.display());
        let allowed = vec![dunce::canonicalize(outside_root.path()).unwrap()];
        let n = recover_orphan_placeholders_with_prefixes(&query, &mut raw, &allowed);
        assert_eq!(
            n, 0,
            "non-canonical TUI URI must still dedup against canonical placeholder path"
        );
        assert_eq!(raw.len(), 1);
        assert_eq!(raw[0].data, "AAAA");
    }

    #[test]
    fn recover_orphan_placeholders_dedupes_against_percent_encoded_uri() {
        let dir = tempfile::tempdir().unwrap();
        let space_dir = dir.path().join("My Pictures");
        std::fs::create_dir(&space_dir).unwrap();
        let png = write_png(&space_dir, "cat.png");
        let canon = dunce::canonicalize(&png).unwrap();
        // Attached image URI uses RFC 3986 percent-encoded form.
        let raw_form = format!("file://{}", canon.display());
        let encoded = raw_form.replace(' ', "%20");
        let mut raw = vec![make_acp_image(&encoded)];
        let query = format!("[Image #1: {}]", canon.display());
        let allowed = vec![dunce::canonicalize(dir.path()).unwrap()];
        let n = recover_orphan_placeholders_with_prefixes(&query, &mut raw, &allowed);
        assert_eq!(
            n, 0,
            "percent-encoded `file://` URI must dedup against canonical placeholder"
        );
        assert_eq!(raw.len(), 1);
        assert_eq!(raw[0].data, "AAAA");
    }

    /// Pin the inverse direction. The placeholder text wire
    /// format is the unencoded path produced by
    /// `echo_agent_pager::prompt_images::display_text`. A
    /// percent-encoded path *inside the placeholder text* is **not**
    /// supported — `extract_placeholders` captures the raw `%20`
    /// substring and `canonicalize` rejects the synthetic name. The
    /// orphan loader logs a warn and skips the placeholder. The
    /// already-attached canonical URI in `raw_images` stays
    /// untouched.
    #[test]
    fn recover_orphan_placeholders_percent_encoded_path_in_placeholder_is_not_supported() {
        let dir = tempfile::tempdir().unwrap();
        let space_dir = dir.path().join("My Pictures");
        std::fs::create_dir(&space_dir).unwrap();
        let png = write_png(&space_dir, "cat.png");
        let canon = dunce::canonicalize(&png).unwrap();
        let attached_uri = format!("file://{}", canon.display());
        let mut raw = vec![make_acp_image(&attached_uri)];
        // Percent-encoded path inside the placeholder text — not the
        // documented wire format.
        let encoded_in_text = format!("{}", canon.display()).replace(' ', "%20");
        let query = format!("[Image #1: {}]", encoded_in_text);
        let allowed = vec![dunce::canonicalize(dir.path()).unwrap()];
        let n = recover_orphan_placeholders_with_prefixes(&query, &mut raw, &allowed);
        assert_eq!(n, 0);
        assert_eq!(raw.len(), 1, "attached URI must remain intact");
        assert_eq!(raw[0].data, "AAAA");
    }

    #[test]
    fn recover_orphan_placeholders_zero_placeholders_short_circuits() {
        let dir = tempfile::tempdir().unwrap();
        let mut raw: Vec<agent_client_protocol::ImageContent> = Vec::new();
        let allowed = vec![dunce::canonicalize(dir.path()).unwrap()];
        let n = recover_orphan_placeholders_with_prefixes("just a message", &mut raw, &allowed);
        assert_eq!(n, 0);
        assert!(raw.is_empty());
    }

    #[test]
    fn recover_orphan_placeholders_failed_load_leaves_raw_images_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.png");
        let mut raw: Vec<agent_client_protocol::ImageContent> = Vec::new();
        let query = format!("[Image #1: {}]", missing.display());
        let allowed = vec![dunce::canonicalize(dir.path()).unwrap()];
        let n = recover_orphan_placeholders_with_prefixes(&query, &mut raw, &allowed);
        assert_eq!(n, 0);
        assert!(raw.is_empty());
    }

    #[test]
    fn recover_orphan_placeholders_outside_allowlist_rejected() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let png = write_png(outside.path(), "secret.png");
        let canon = dunce::canonicalize(&png).unwrap();
        let mut raw: Vec<agent_client_protocol::ImageContent> = Vec::new();
        let query = format!("[Image #1: {}]", canon.display());
        // Allowlist is workspace only — the placeholder canon is
        // outside it.
        let allowed = vec![dunce::canonicalize(workspace.path()).unwrap()];
        let n = recover_orphan_placeholders_with_prefixes(&query, &mut raw, &allowed);
        assert_eq!(n, 0);
        assert!(raw.is_empty());
    }

    // ----- Aggregate cap -------------------------------------------------

    /// Two placeholders, aggregate cap below the cumulative byte
    /// total of both. The first image fits; the second pushes the
    /// running total over and the loop breaks. Pin: `n == 1`, second
    /// placeholder was NOT loaded into `raw`.
    #[test]
    fn recover_orphan_placeholders_aggregate_cap_breaks_loop() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = write_png(dir.path(), "a.png");
        let p2 = write_png(dir.path(), "b.png");
        let c1 = dunce::canonicalize(&p1).unwrap();
        let c2 = dunce::canonicalize(&p2).unwrap();
        let query = format!("[Image #1: {}] [Image #2: {}]", c1.display(), c2.display());
        let mut raw: Vec<agent_client_protocol::ImageContent> = Vec::new();
        let allowed = vec![dunce::canonicalize(dir.path()).unwrap()];
        // Per-image cap permissive; aggregate cap admits exactly one
        // image (PNG_BYTES is 67 bytes; cap at 100 lets one through,
        // blocks the second).
        let n = recover_orphan_placeholders_with_prefixes_and_caps(
            &query, &mut raw, &allowed, 1_000, 100,
        );
        assert_eq!(n, 1, "aggregate cap must allow exactly one image");
        assert_eq!(raw.len(), 1);
        // Order matters — the first placeholder's canonical URI is
        // the one that landed in `raw`.
        let attached_uri = raw[0].uri.as_deref().unwrap();
        assert!(
            attached_uri.contains("a.png"),
            "expected the first placeholder to be the one kept, got: {attached_uri}"
        );
    }

    /// Pin the aggregate-cap boundary semantics. The cap is
    /// **inclusive** — `aggregate + image.len() > cap` triggers the
    /// break. A single image exactly equal to the cap is admitted.
    #[test]
    fn recover_orphan_placeholders_aggregate_cap_inclusive_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_png(dir.path(), "one.png");
        let c = dunce::canonicalize(&p).unwrap();
        let query = format!("[Image #1: {}]", c.display());
        let mut raw: Vec<agent_client_protocol::ImageContent> = Vec::new();
        let allowed = vec![dunce::canonicalize(dir.path()).unwrap()];
        // Cap == image size: the `>` comparison admits this image.
        let n = recover_orphan_placeholders_with_prefixes_and_caps(
            &query,
            &mut raw,
            &allowed,
            1_000,
            PNG_BYTES.len(),
        );
        assert_eq!(n, 1);
        assert_eq!(raw.len(), 1);
    }

    /// The reject side of the inclusive-boundary contract.
    /// `cap == image_size - 1` is the smallest cap that rejects this
    /// image. Two-sided boundary pinning locks down the inclusive vs
    /// exclusive contract.
    #[test]
    fn recover_orphan_placeholders_aggregate_cap_inclusive_boundary_rejects_at_one_below() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_png(dir.path(), "one.png");
        let c = dunce::canonicalize(&p).unwrap();
        let query = format!("[Image #1: {}]", c.display());
        let mut raw: Vec<agent_client_protocol::ImageContent> = Vec::new();
        let allowed = vec![dunce::canonicalize(dir.path()).unwrap()];
        let n = recover_orphan_placeholders_with_prefixes_and_caps(
            &query,
            &mut raw,
            &allowed,
            1_000,
            PNG_BYTES.len() - 1,
        );
        assert_eq!(n, 0, "cap one below image size must reject");
        assert!(raw.is_empty());
    }

    // ----- DENY_PATH_CONTAINS --------------------------------------------

    /// Every entry of `DENY_PATH_CONTAINS` produces an
    /// `OutsideAllowedPrefixes` rejection. Loops the constant so a
    /// future PR that deletes a security-critical entry (e.g.
    /// `/.ssh/`) ships a failing test.
    #[test]
    fn load_placeholder_image_rejects_every_deny_list_entry() {
        // Build a canonical that contains each needle by constructing
        // a directory tree under a tempdir. Each needle is wrapped in
        // a leading `<allowed>` and a trailing `<file>.png`. Forward
        // slashes only — the enforcement site normalises Windows
        // backslashes to forward slashes before the substring check.
        for needle in DENY_PATH_CONTAINS {
            let root = tempfile::tempdir().unwrap();
            let trimmed = needle.trim_matches('/');
            // Materialise the directory tree implied by the needle so
            // canonicalize succeeds.
            let mut current = root.path().to_path_buf();
            for segment in trimmed.split('/') {
                current = current.join(segment);
                std::fs::create_dir(&current).unwrap();
            }
            let png = write_png(&current, "x.png");
            let canon = dunce::canonicalize(&png).unwrap();
            // Allowlist is the root — without the deny-list, this
            // path would be accepted.
            let allowed = vec![dunce::canonicalize(root.path()).unwrap()];
            let err = load_placeholder_image(canon.to_str().unwrap(), &allowed).unwrap_err();
            assert!(
                matches!(err, PlaceholderLoadError::OutsideAllowedPrefixes),
                "needle {needle:?} did not produce OutsideAllowedPrefixes; got {err:?}"
            );
        }

        // Positive control. A benign path inside an allowed
        // prefix containing **none** of the deny needles must still
        // load. Without this, a future regression that rejects every
        // path would pass the loop above and ship.
        let root = tempfile::tempdir().unwrap();
        let png = write_png(root.path(), "picture.png");
        let canon = dunce::canonicalize(&png).unwrap();
        let allowed = vec![dunce::canonicalize(root.path()).unwrap()];
        let loaded =
            load_placeholder_image(canon.to_str().unwrap(), &allowed).unwrap_or_else(|e| {
                panic!("positive control: benign path inside allowed prefix must load, got: {e:?}")
            });
        // Tighten the positive control — a regression that
        // returned an empty `LoadedPlaceholderImage` would pass a
        // bare `is_ok()` assertion. Pin the mime type and round-trip
        // the bytes against the on-disk PNG.
        assert_eq!(loaded.mime_type, "image/png");
        assert_eq!(loaded.data, PNG_BYTES);
    }

    #[test]
    fn attached_image_references_prefers_file_path_over_data() {
        // `[Image #N]` resolution should hand `image_edit` a bare on-disk
        // path (from the durable `file://` URI) so it reads the session
        // copy rather than re-decoding a large base64 blob.
        let img = agent_client_protocol::ImageContent::new("AAAA", "image/png")
            .uri(Some(
                "file:///Users/me/.echo-agent/sessions/s/images/image-1.png".into(),
            ))
            .meta(display_number_meta(1));
        let refs = attached_image_references(std::slice::from_ref(&img));
        assert_eq!(
            refs,
            vec![(
                1,
                "/Users/me/.echo-agent/sessions/s/images/image-1.png".to_string()
            )]
        );
    }

    #[test]
    fn attached_image_references_falls_back_to_data_url() {
        // No durable URI (e.g. persistence failed): keep the inline bytes
        // as a data URL so the token still resolves to the right image.
        let img = agent_client_protocol::ImageContent::new("BBBB", "image/jpeg")
            .meta(display_number_meta(2));
        let refs = attached_image_references(std::slice::from_ref(&img));
        assert_eq!(refs, vec![(2, "data:image/jpeg;base64,BBBB".to_string())]);
    }

    #[test]
    fn attached_image_references_keys_by_meta_number_not_position() {
        // Non-contiguous numbers (`#1`, `#3`) survive a mid-compose chip
        // removal; the registry must key on the recorded number, not the
        // list position.
        let mk = |data: &str, n: usize| {
            agent_client_protocol::ImageContent::new(data, "image/png").meta(display_number_meta(n))
        };
        let refs = attached_image_references(&[mk("first", 1), mk("third", 3)]);
        assert_eq!(
            refs,
            vec![
                (1, "data:image/png;base64,first".to_string()),
                (3, "data:image/png;base64,third".to_string()),
            ]
        );
    }

    #[test]
    fn attached_image_references_falls_back_to_position_without_meta() {
        // Older client / non-TUI caller with no recorded number: fall back
        // to 1-based position so the common contiguous case still resolves.
        let mk = |data: &str| agent_client_protocol::ImageContent::new(data, "image/png");
        let refs = attached_image_references(&[mk("first"), mk("second")]);
        assert_eq!(
            refs,
            vec![
                (1, "data:image/png;base64,first".to_string()),
                (2, "data:image/png;base64,second".to_string()),
            ]
        );
    }
}
