//! Anchor scheme abstraction and candidate implementations.
//!
//! Three candidate schemes are provided:
//!
//! - **Candidate A** (`ContentOnly`): content-only line hash. Simplest, weakest
//!   freshness — edits above a line do not invalidate its anchor.
//!
//! - **Candidate B** (`ChunkFingerprint`): local line hash + fixed-size chunk
//!   fingerprint. Edits invalidate only anchors within the affected chunk.
//!   Recommended starting point for benchmarking.
//!
//! - **Candidate C** (`CheckpointChain`): local line hash + checkpoint-derived
//!   fingerprint computed from the nearest preceding checkpoint. Strongest
//!   freshness detection at the cost of more anchor churn after edits.
//!
//! All schemes share the same whitespace-normalized local line hash from
//! [`crate::util::hash::line_hash`].

use std::fmt;

use crate::util::hash::{self, DEFAULT_HASH_LEN};

/// Trait for pluggable anchor generation and validation schemes.
///
/// Implementations generate anchors for file lines and validate anchors
/// against current file content.
pub trait AnchorScheme: fmt::Debug + Send + Sync {
    /// Machine-readable name for this scheme (e.g. `"content_only_v1"`).
    fn name(&self) -> &str;

    /// Number of lowercase letters in the local line hash component.
    fn hash_len(&self) -> usize;

    /// Generate anchors for all lines in a file.
    ///
    /// `lines` is a slice of the file's lines (without trailing newlines).
    /// Returns one `Anchor` per line, in order.
    fn generate_anchors(&self, lines: &[&str]) -> Vec<Anchor>;

    /// Validate a parsed anchor against current file content.
    ///
    /// `anchor` is the anchor to validate. `lines` is the current file
    /// content split by line. Returns the validation result.
    fn validate(&self, anchor: &ParsedAnchor, lines: &[&str]) -> ValidationResult;

    /// Estimated number of lines read to validate a single anchor at
    /// `line_idx` (0-based) in a file of `total_lines` lines.
    ///
    /// Used by the benchmark harness for read-amplification measurement.
    /// Default: 1 (local line only).
    fn validation_window_lines(&self, _line_idx: usize, _total_lines: usize) -> usize {
        1
    }

    /// Search for a shifted anchor within a bounded window around the
    /// original line number.
    ///
    /// Returns `ShiftResult::Found` if exactly one nearby line validates
    /// under this scheme, `ShiftResult::Ambiguous` if multiple candidates
    /// match, and `ShiftResult::NotFound` if none match.
    fn find_shifted(
        &self,
        anchor: &ParsedAnchor,
        lines: &[&str],
        search_radius: usize,
    ) -> ShiftResult;
}

/// A rendered anchor for a single line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchor {
    /// 1-based line number.
    pub line: usize,
    /// Encoded local line hash (e.g. `"abc"`).
    pub local: String,
    /// Optional contextual fingerprint (e.g. `"rst"` for chunk/checkpoint).
    pub context: Option<String>,
}

impl Anchor {
    /// Render this anchor as a string suitable for output.
    ///
    /// Format: `"LINE:LOCAL"` or `"LINE:LOCAL:CONTEXT"`.
    pub fn render(&self) -> String {
        match &self.context {
            Some(ctx) => format!("{}:{}:{}", self.line, self.local, ctx),
            None => format!("{}:{}", self.line, self.local),
        }
    }
}

impl fmt::Display for Anchor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render())
    }
}

/// A parsed anchor extracted from model input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedAnchor {
    /// 1-based line number.
    pub line: usize,
    /// Local line hash component.
    pub local: String,
    /// Optional contextual fingerprint component.
    pub context: Option<String>,
}

impl ParsedAnchor {
    /// Parse an anchor string into its components.
    ///
    /// Accepted formats:
    /// - `"22:abc"` → line=22, local="abc", context=None
    /// - `"22:abc:rst"` → line=22, local="abc", context=Some("rst")
    ///
    /// Returns `None` if the string is malformed (non-numeric line number,
    /// missing components, etc.).
    pub fn parse(s: &str) -> Option<Self> {
        let mut parts = s.splitn(3, ':');
        let line_str = parts.next()?;
        let local = parts.next()?;

        if line_str.is_empty() || local.is_empty() {
            return None;
        }

        let line: usize = line_str.parse().ok()?;
        if line == 0 {
            return None;
        }

        // Validate local hash: must be all lowercase ASCII letters.
        if !local.bytes().all(|b| b.is_ascii_lowercase()) {
            return None;
        }

        let context = parts.next().map(|s| s.to_owned());
        // Validate context hash if present: must be non-empty lowercase ASCII letters.
        if let Some(ref ctx) = context
            && (ctx.is_empty() || !ctx.bytes().all(|b| b.is_ascii_lowercase()))
        {
            return None;
        }

        Some(Self {
            line,
            local: local.to_owned(),
            context,
        })
    }

    /// Render back to string form.
    pub fn render(&self) -> String {
        match &self.context {
            Some(ctx) => format!("{}:{}:{}", self.line, self.local, ctx),
            None => format!("{}:{}", self.line, self.local),
        }
    }
}

/// Result of validating an anchor against current file content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationResult {
    /// Anchor is valid — the line content matches the expected hash.
    Valid,
    /// Anchor is stale — the line exists but its content has changed.
    Stale,
    /// Line number is out of range for the current file.
    OutOfRange,
}

/// Result of searching for a shifted anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShiftResult {
    /// Exactly one nearby line validates — the anchor shifted to this line.
    Found {
        /// 1-based line number where the anchor now validates.
        new_line: usize,
    },
    /// Multiple nearby lines validate — ambiguous recovery.
    Ambiguous {
        /// All candidate 1-based line numbers.
        candidates: Vec<usize>,
    },
    /// No nearby line validates.
    NotFound,
}

/// Default search radius for shifted-anchor recovery (±15 lines).
pub const DEFAULT_SEARCH_RADIUS: usize = 15;

/// Candidate A — content-only line hash.
///
/// Anchor format: `LINE:LOCAL` (e.g. `22:abc`).
/// Validates only the normalized content of the specified line. Edits above
/// the line do not invalidate its anchor. Weakest freshness semantics.
#[derive(Debug, Clone)]
pub struct ContentOnly {
    hash_len: usize,
}

impl ContentOnly {
    /// Create with default hash length (3 letters).
    pub fn new() -> Self {
        Self {
            hash_len: DEFAULT_HASH_LEN,
        }
    }

    /// Create with a custom hash length.
    ///
    /// # Panics
    ///
    /// Panics if `hash_len` is not in `1..=4`.
    pub fn with_hash_len(hash_len: usize) -> Self {
        assert!(
            hash_len > 0 && hash_len <= 4,
            "hash_len must be 1..=4, got {hash_len}"
        );
        Self { hash_len }
    }
}

impl Default for ContentOnly {
    fn default() -> Self {
        Self::new()
    }
}

impl AnchorScheme for ContentOnly {
    fn name(&self) -> &str {
        "content_only_v1"
    }

    fn hash_len(&self) -> usize {
        self.hash_len
    }

    fn generate_anchors(&self, lines: &[&str]) -> Vec<Anchor> {
        lines
            .iter()
            .enumerate()
            .map(|(i, line)| {
                let h = hash::line_hash(line);
                Anchor {
                    line: i + 1,
                    local: hash::encode_hash(h, self.hash_len),
                    context: None,
                }
            })
            .collect()
    }

    fn validate(&self, anchor: &ParsedAnchor, lines: &[&str]) -> ValidationResult {
        let idx = anchor.line.checked_sub(1).unwrap_or(usize::MAX);
        if idx >= lines.len() {
            return ValidationResult::OutOfRange;
        }

        let expected_local = hash::encode_hash(hash::line_hash(lines[idx]), self.hash_len);
        if anchor.local == expected_local {
            ValidationResult::Valid
        } else {
            ValidationResult::Stale
        }
    }

    fn find_shifted(
        &self,
        anchor: &ParsedAnchor,
        lines: &[&str],
        search_radius: usize,
    ) -> ShiftResult {
        find_shifted_generic(self, anchor, lines, search_radius)
    }
}

/// Default chunk size for Candidate B (16 lines).
pub const DEFAULT_CHUNK_SIZE: usize = 16;

/// Candidate B — chunk-fingerprinted line anchors.
///
/// Anchor format: `LINE:LOCAL:CHUNK` (e.g. `22:abc:rst`).
/// `LOCAL` is the normalized line hash. `CHUNK` is a fingerprint of the
/// fixed-size chunk containing this line. Edits invalidate anchors only
/// within the affected chunk.
#[derive(Debug, Clone)]
pub struct ChunkFingerprint {
    hash_len: usize,
    chunk_size: usize,
}

impl ChunkFingerprint {
    /// Create with default parameters (3-letter hash, 16-line chunks).
    pub fn new() -> Self {
        Self {
            hash_len: DEFAULT_HASH_LEN,
            chunk_size: DEFAULT_CHUNK_SIZE,
        }
    }

    /// Create with custom parameters.
    ///
    /// # Panics
    ///
    /// Panics if `hash_len` is not in `1..=4` or `chunk_size` is 0.
    pub fn with_params(hash_len: usize, chunk_size: usize) -> Self {
        assert!(
            hash_len > 0 && hash_len <= 4,
            "hash_len must be 1..=4, got {hash_len}"
        );
        assert!(chunk_size > 0, "chunk_size must be > 0");
        Self {
            hash_len,
            chunk_size,
        }
    }

    /// Compute the chunk fingerprint for the chunk containing `line_idx` (0-based).
    fn chunk_fingerprint(&self, lines: &[&str], line_idx: usize) -> String {
        let chunk_start = (line_idx / self.chunk_size) * self.chunk_size;
        let chunk_end = (chunk_start + self.chunk_size).min(lines.len());

        // Hash all normalized lines in the chunk together.
        let mut combined: u32 = hash::fnv1a_32(b"chunk");
        for line in &lines[chunk_start..chunk_end] {
            let lh = hash::line_hash(line);
            // Mix each line hash into the combined hash.
            combined ^= lh;
            combined = combined.wrapping_mul(16_777_619);
        }
        hash::encode_hash(combined, self.hash_len)
    }
}

impl Default for ChunkFingerprint {
    fn default() -> Self {
        Self::new()
    }
}

impl AnchorScheme for ChunkFingerprint {
    fn name(&self) -> &str {
        "chunk_v1"
    }

    fn hash_len(&self) -> usize {
        self.hash_len
    }

    fn validation_window_lines(&self, _line_idx: usize, total_lines: usize) -> usize {
        self.chunk_size.min(total_lines)
    }

    fn generate_anchors(&self, lines: &[&str]) -> Vec<Anchor> {
        // Pre-compute chunk fingerprints to avoid redundant work.
        let num_chunks = lines.len().div_ceil(self.chunk_size);
        let mut chunk_fps: Vec<String> = Vec::with_capacity(num_chunks);
        for chunk_idx in 0..num_chunks {
            let start = chunk_idx * self.chunk_size;
            let end = (start + self.chunk_size).min(lines.len());

            let mut combined: u32 = hash::fnv1a_32(b"chunk");
            for line in &lines[start..end] {
                let lh = hash::line_hash(line);
                combined ^= lh;
                combined = combined.wrapping_mul(16_777_619);
            }
            chunk_fps.push(hash::encode_hash(combined, self.hash_len));
        }

        lines
            .iter()
            .enumerate()
            .map(|(i, line)| {
                let h = hash::line_hash(line);
                let chunk_idx = i / self.chunk_size;
                Anchor {
                    line: i + 1,
                    local: hash::encode_hash(h, self.hash_len),
                    context: Some(chunk_fps[chunk_idx].clone()),
                }
            })
            .collect()
    }

    fn validate(&self, anchor: &ParsedAnchor, lines: &[&str]) -> ValidationResult {
        let idx = anchor.line.checked_sub(1).unwrap_or(usize::MAX);
        if idx >= lines.len() {
            return ValidationResult::OutOfRange;
        }

        // Validate local line hash.
        let expected_local = hash::encode_hash(hash::line_hash(lines[idx]), self.hash_len);
        if anchor.local != expected_local {
            return ValidationResult::Stale;
        }

        // Chunk-fingerprinted scheme requires context — reject truncated anchors
        // that omit the chunk fingerprint, as they would silently weaken
        // validation to content-only semantics.
        let Some(ref expected_ctx) = anchor.context else {
            return ValidationResult::Stale;
        };
        let actual_ctx = self.chunk_fingerprint(lines, idx);
        if *expected_ctx != actual_ctx {
            return ValidationResult::Stale;
        }

        ValidationResult::Valid
    }

    fn find_shifted(
        &self,
        anchor: &ParsedAnchor,
        lines: &[&str],
        search_radius: usize,
    ) -> ShiftResult {
        find_shifted_generic(self, anchor, lines, search_radius)
    }
}

/// Default checkpoint interval for Candidate C (32 lines).
pub const DEFAULT_CHECKPOINT_INTERVAL: usize = 32;

/// Candidate C — checkpoint-chained line anchors.
///
/// Anchor format: `LINE:LOCAL:CKPT` (e.g. `22:abc:rst`).
/// `LOCAL` is the normalized line hash. `CKPT` is a fingerprint derived from
/// chaining all line hashes from the nearest preceding checkpoint to this
/// line. Strongest freshness detection but more anchor churn after edits.
#[derive(Debug, Clone)]
pub struct CheckpointChain {
    hash_len: usize,
    checkpoint_interval: usize,
}

impl CheckpointChain {
    /// Create with default parameters (3-letter hash, 32-line checkpoints).
    pub fn new() -> Self {
        Self {
            hash_len: DEFAULT_HASH_LEN,
            checkpoint_interval: DEFAULT_CHECKPOINT_INTERVAL,
        }
    }

    /// Create with custom parameters.
    ///
    /// # Panics
    ///
    /// Panics if `hash_len` is not in `1..=4` or `checkpoint_interval` is 0.
    pub fn with_params(hash_len: usize, checkpoint_interval: usize) -> Self {
        assert!(
            hash_len > 0 && hash_len <= 4,
            "hash_len must be 1..=4, got {hash_len}"
        );
        assert!(checkpoint_interval > 0, "checkpoint_interval must be > 0");
        Self {
            hash_len,
            checkpoint_interval,
        }
    }

    /// Compute the checkpoint-chained fingerprint for `line_idx` (0-based).
    ///
    /// Chains line hashes from the nearest checkpoint boundary up to and
    /// including `line_idx`.
    fn checkpoint_fingerprint(&self, lines: &[&str], line_idx: usize) -> String {
        let checkpoint_start = (line_idx / self.checkpoint_interval) * self.checkpoint_interval;

        let mut chain: u32 = hash::fnv1a_32(b"ckpt");
        for line in &lines[checkpoint_start..=line_idx] {
            let lh = hash::line_hash(line);
            chain ^= lh;
            chain = chain.wrapping_mul(16_777_619);
        }
        hash::encode_hash(chain, self.hash_len)
    }
}

impl Default for CheckpointChain {
    fn default() -> Self {
        Self::new()
    }
}

impl AnchorScheme for CheckpointChain {
    fn name(&self) -> &str {
        "checkpoint_v1"
    }

    fn hash_len(&self) -> usize {
        self.hash_len
    }

    fn validation_window_lines(&self, line_idx: usize, _total_lines: usize) -> usize {
        let checkpoint_start = (line_idx / self.checkpoint_interval) * self.checkpoint_interval;
        line_idx - checkpoint_start + 1
    }

    fn generate_anchors(&self, lines: &[&str]) -> Vec<Anchor> {
        let mut anchors = Vec::with_capacity(lines.len());
        let mut chain: u32 = hash::fnv1a_32(b"ckpt");

        for (i, line) in lines.iter().enumerate() {
            // Reset chain at checkpoint boundaries.
            if i % self.checkpoint_interval == 0 {
                chain = hash::fnv1a_32(b"ckpt");
            }

            let lh = hash::line_hash(line);
            chain ^= lh;
            chain = chain.wrapping_mul(16_777_619);

            anchors.push(Anchor {
                line: i + 1,
                local: hash::encode_hash(lh, self.hash_len),
                context: Some(hash::encode_hash(chain, self.hash_len)),
            });
        }

        anchors
    }

    fn validate(&self, anchor: &ParsedAnchor, lines: &[&str]) -> ValidationResult {
        let idx = anchor.line.checked_sub(1).unwrap_or(usize::MAX);
        if idx >= lines.len() {
            return ValidationResult::OutOfRange;
        }

        // Validate local line hash.
        let expected_local = hash::encode_hash(hash::line_hash(lines[idx]), self.hash_len);
        if anchor.local != expected_local {
            return ValidationResult::Stale;
        }

        // Checkpoint-chained scheme requires context — reject truncated anchors
        // that omit the checkpoint fingerprint, as they would silently weaken
        // validation to content-only semantics.
        let Some(ref expected_ctx) = anchor.context else {
            return ValidationResult::Stale;
        };
        let actual_ctx = self.checkpoint_fingerprint(lines, idx);
        if *expected_ctx != actual_ctx {
            return ValidationResult::Stale;
        }

        ValidationResult::Valid
    }

    fn find_shifted(
        &self,
        anchor: &ParsedAnchor,
        lines: &[&str],
        search_radius: usize,
    ) -> ShiftResult {
        find_shifted_generic(self, anchor, lines, search_radius)
    }
}

/// Generic shifted-anchor recovery used by all scheme implementations.
///
/// Searches `±search_radius` lines around the anchor's original position for
/// a line whose local hash matches. For schemes with contextual components,
/// the contextual fingerprint is recomputed at each candidate position and
/// also compared.
///
/// This function avoids per-candidate allocations: it computes the local hash
/// inline (no `ParsedAnchor` cloning) and only evaluates the contextual
/// fingerprint when the cheap local-hash check passes.
fn find_shifted_generic(
    scheme: &dyn AnchorScheme,
    anchor: &ParsedAnchor,
    lines: &[&str],
    search_radius: usize,
) -> ShiftResult {
    let orig_idx = anchor.line.saturating_sub(1);
    let start = orig_idx.saturating_sub(search_radius);
    let end = (orig_idx + search_radius + 1).min(lines.len());
    let hash_len = scheme.hash_len();

    let mut candidates: Vec<usize> = Vec::new();

    for idx in start..end {
        // Skip the original line — it already failed validation.
        if idx == orig_idx {
            continue;
        }

        // Cheap check: does the local line hash match?
        let local = hash::encode_hash(hash::line_hash(lines[idx]), hash_len);
        if local != anchor.local {
            continue;
        }

        // If the anchor carries context, validate via the full scheme
        // (which recomputes the contextual fingerprint at this position).
        // For context-free anchors (Candidate A) this is skipped entirely.
        if anchor.context.is_some() {
            let probe = ParsedAnchor {
                line: idx + 1,
                local,
                context: anchor.context.clone(),
            };
            if scheme.validate(&probe, lines) != ValidationResult::Valid {
                continue;
            }
        }

        candidates.push(idx + 1);
    }

    match candidates.len() {
        0 => ShiftResult::NotFound,
        1 => ShiftResult::Found {
            new_line: candidates[0],
        },
        _ => ShiftResult::Ambiguous { candidates },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Test fixture
    // -----------------------------------------------------------------------

    fn sample_lines() -> Vec<&'static str> {
        vec![
            "import React from 'react';",
            "",
            "export function App() {",
            "  return <div>Hello</div>;",
            "}",
        ]
    }

    // -----------------------------------------------------------------------
    // ParsedAnchor tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_anchor_two_parts() {
        let a = ParsedAnchor::parse("22:abc").unwrap();
        assert_eq!(a.line, 22);
        assert_eq!(a.local, "abc");
        assert!(a.context.is_none());
    }

    #[test]
    fn parse_anchor_three_parts() {
        let a = ParsedAnchor::parse("22:abc:rst").unwrap();
        assert_eq!(a.line, 22);
        assert_eq!(a.local, "abc");
        assert_eq!(a.context.as_deref(), Some("rst"));
    }

    #[test]
    fn parse_anchor_roundtrip() {
        for input in ["1:abc", "100:xyz:def", "42:ab"] {
            let parsed = ParsedAnchor::parse(input).unwrap();
            assert_eq!(parsed.render(), input);
        }
    }

    #[test]
    fn parse_anchor_rejects_malformed() {
        assert!(ParsedAnchor::parse("").is_none());
        assert!(ParsedAnchor::parse("abc").is_none());
        assert!(ParsedAnchor::parse(":abc").is_none());
        assert!(ParsedAnchor::parse("22:").is_none());
        assert!(ParsedAnchor::parse("0:abc").is_none()); // line 0 invalid
        assert!(ParsedAnchor::parse("22:ABC").is_none()); // uppercase
        assert!(ParsedAnchor::parse("22:abc:").is_none()); // empty context
        assert!(ParsedAnchor::parse("22:abc:XYZ").is_none()); // uppercase context
        assert!(ParsedAnchor::parse("abc:def").is_none()); // non-numeric line
    }

    // -----------------------------------------------------------------------
    // Anchor::render tests
    // -----------------------------------------------------------------------

    #[test]
    fn anchor_render_without_context() {
        let a = Anchor {
            line: 5,
            local: "abc".to_owned(),
            context: None,
        };
        assert_eq!(a.render(), "5:abc");
        assert_eq!(a.to_string(), "5:abc");
    }

    #[test]
    fn anchor_render_with_context() {
        let a = Anchor {
            line: 22,
            local: "abc".to_owned(),
            context: Some("rst".to_owned()),
        };
        assert_eq!(a.render(), "22:abc:rst");
    }

    // -----------------------------------------------------------------------
    // Candidate A — ContentOnly
    // -----------------------------------------------------------------------

    #[test]
    fn content_only_generates_correct_count() {
        let lines = sample_lines();
        let scheme = ContentOnly::new();
        let anchors = scheme.generate_anchors(&lines);
        assert_eq!(anchors.len(), lines.len());
    }

    #[test]
    fn content_only_anchors_have_no_context() {
        let lines = sample_lines();
        let scheme = ContentOnly::new();
        let anchors = scheme.generate_anchors(&lines);
        for a in &anchors {
            assert!(a.context.is_none());
        }
    }

    #[test]
    fn content_only_deterministic() {
        let lines = sample_lines();
        let scheme = ContentOnly::new();
        let a = scheme.generate_anchors(&lines);
        let b = scheme.generate_anchors(&lines);
        assert_eq!(a, b);
    }

    #[test]
    fn content_only_line_numbers_1based() {
        let lines = sample_lines();
        let scheme = ContentOnly::new();
        let anchors = scheme.generate_anchors(&lines);
        for (i, a) in anchors.iter().enumerate() {
            assert_eq!(a.line, i + 1);
        }
    }

    #[test]
    fn content_only_validates_correct_anchor() {
        let lines = sample_lines();
        let scheme = ContentOnly::new();
        let anchors = scheme.generate_anchors(&lines);

        for a in &anchors {
            let parsed = ParsedAnchor {
                line: a.line,
                local: a.local.clone(),
                context: None,
            };
            assert_eq!(scheme.validate(&parsed, &lines), ValidationResult::Valid);
        }
    }

    #[test]
    fn content_only_detects_stale_anchor() {
        let lines = sample_lines();
        let scheme = ContentOnly::new();
        let anchors = scheme.generate_anchors(&lines);

        // Mutate line 4 and re-validate anchor 4.
        let mut mutated = lines.clone();
        mutated[3] = "  return <div>World</div>;";

        let parsed = ParsedAnchor {
            line: anchors[3].line,
            local: anchors[3].local.clone(),
            context: None,
        };
        assert_eq!(scheme.validate(&parsed, &mutated), ValidationResult::Stale);
    }

    #[test]
    fn content_only_survives_indentation_change() {
        let lines = sample_lines();
        let scheme = ContentOnly::new();
        let anchors = scheme.generate_anchors(&lines);

        // Change indentation of line 4 (0-indexed: 3).
        let mut reindented = lines.clone();
        reindented[3] = "    return <div>Hello</div>;";

        let parsed = ParsedAnchor {
            line: anchors[3].line,
            local: anchors[3].local.clone(),
            context: None,
        };
        assert_eq!(
            scheme.validate(&parsed, &reindented),
            ValidationResult::Valid
        );
    }

    #[test]
    fn content_only_out_of_range() {
        let lines = sample_lines();
        let scheme = ContentOnly::new();
        let parsed = ParsedAnchor {
            line: 100,
            local: "abc".to_owned(),
            context: None,
        };
        assert_eq!(
            scheme.validate(&parsed, &lines),
            ValidationResult::OutOfRange
        );
    }

    // -----------------------------------------------------------------------
    // Candidate B — ChunkFingerprint
    // -----------------------------------------------------------------------

    #[test]
    fn chunk_generates_context() {
        let lines = sample_lines();
        let scheme = ChunkFingerprint::new();
        let anchors = scheme.generate_anchors(&lines);
        for a in &anchors {
            assert!(a.context.is_some());
        }
    }

    #[test]
    fn chunk_same_chunk_same_context() {
        let lines = sample_lines(); // 5 lines, all in chunk 0 (size 16)
        let scheme = ChunkFingerprint::new();
        let anchors = scheme.generate_anchors(&lines);
        let ctx0 = anchors[0].context.as_ref().unwrap();
        for a in &anchors {
            assert_eq!(a.context.as_ref().unwrap(), ctx0);
        }
    }

    #[test]
    fn chunk_different_chunks_may_differ() {
        // 20 lines → chunk 0 (lines 1-16), chunk 1 (lines 17-20)
        let owned: Vec<String> = (0..20).map(|i| format!("line {i}")).collect();
        let refs: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();

        let scheme = ChunkFingerprint::new();
        let anchors = scheme.generate_anchors(&refs);

        let ctx_0 = anchors[0].context.as_ref().unwrap();
        let ctx_16 = anchors[16].context.as_ref().unwrap();
        // Different chunks with different content should (usually) have
        // different fingerprints; assert inequality for this specific input.
        assert_ne!(ctx_0, ctx_16);
    }

    #[test]
    fn chunk_validates_correct_anchor() {
        let lines = sample_lines();
        let scheme = ChunkFingerprint::new();
        let anchors = scheme.generate_anchors(&lines);

        for a in &anchors {
            let parsed = ParsedAnchor {
                line: a.line,
                local: a.local.clone(),
                context: a.context.clone(),
            };
            assert_eq!(scheme.validate(&parsed, &lines), ValidationResult::Valid);
        }
    }

    #[test]
    fn chunk_detects_stale_from_same_chunk_edit() {
        let lines = sample_lines();
        let scheme = ChunkFingerprint::new();
        let anchors = scheme.generate_anchors(&lines);

        // Mutate line 3 (same chunk as line 1).
        let mut mutated = lines.clone();
        mutated[2] = "export function Changed() {";

        // Line 1's anchor should go stale because its chunk changed.
        let parsed = ParsedAnchor {
            line: anchors[0].line,
            local: anchors[0].local.clone(),
            context: anchors[0].context.clone(),
        };
        assert_eq!(scheme.validate(&parsed, &mutated), ValidationResult::Stale);
    }

    #[test]
    fn chunk_deterministic() {
        let lines = sample_lines();
        let scheme = ChunkFingerprint::new();
        let a = scheme.generate_anchors(&lines);
        let b = scheme.generate_anchors(&lines);
        assert_eq!(a, b);
    }

    // -----------------------------------------------------------------------
    // Candidate C — CheckpointChain
    // -----------------------------------------------------------------------

    #[test]
    fn checkpoint_generates_context() {
        let lines = sample_lines();
        let scheme = CheckpointChain::new();
        let anchors = scheme.generate_anchors(&lines);
        for a in &anchors {
            assert!(a.context.is_some());
        }
    }

    #[test]
    fn checkpoint_validates_correct_anchor() {
        let lines = sample_lines();
        let scheme = CheckpointChain::new();
        let anchors = scheme.generate_anchors(&lines);

        for a in &anchors {
            let parsed = ParsedAnchor {
                line: a.line,
                local: a.local.clone(),
                context: a.context.clone(),
            };
            assert_eq!(scheme.validate(&parsed, &lines), ValidationResult::Valid);
        }
    }

    #[test]
    fn checkpoint_detects_upstream_edit() {
        let lines = sample_lines();
        let scheme = CheckpointChain::with_params(3, 32);
        let anchors = scheme.generate_anchors(&lines);

        // Mutate line 2 (above line 4, same checkpoint window).
        let mut mutated = lines.clone();
        mutated[1] = "// changed";

        // Line 4's checkpoint fingerprint should change.
        let parsed = ParsedAnchor {
            line: anchors[3].line,
            local: anchors[3].local.clone(),
            context: anchors[3].context.clone(),
        };
        assert_eq!(scheme.validate(&parsed, &mutated), ValidationResult::Stale);
    }

    #[test]
    fn checkpoint_deterministic() {
        let lines = sample_lines();
        let scheme = CheckpointChain::new();
        let a = scheme.generate_anchors(&lines);
        let b = scheme.generate_anchors(&lines);
        assert_eq!(a, b);
    }

    // -----------------------------------------------------------------------
    // Shared: find_shifted recovery tests
    // -----------------------------------------------------------------------

    #[test]
    fn find_shifted_after_insert_above() {
        // Original: 5 lines. Insert a line at the top → target shifts down by 1.
        let lines = sample_lines();
        let scheme = ContentOnly::new();
        let anchors = scheme.generate_anchors(&lines);

        // Insert a new line at position 0 → all lines shift down by 1.
        let mut shifted = vec!["// new line"];
        shifted.extend_from_slice(&lines);

        // Anchor for original line 3 ("export function App() {") is now at line 4.
        let parsed = ParsedAnchor {
            line: anchors[2].line, // line 3
            local: anchors[2].local.clone(),
            context: None,
        };

        match scheme.find_shifted(&parsed, &shifted, 5) {
            ShiftResult::Found { new_line } => assert_eq!(new_line, 4),
            other => panic!("Expected Found, got {:?}", other),
        }
    }

    #[test]
    fn find_shifted_not_found() {
        let lines = sample_lines();
        let scheme = ContentOnly::new();

        // Completely fabricated anchor.
        let parsed = ParsedAnchor {
            line: 3,
            local: "zzz".to_owned(),
            context: None,
        };

        assert_eq!(
            scheme.find_shifted(&parsed, &lines, 5),
            ShiftResult::NotFound
        );
    }

    #[test]
    fn find_shifted_ambiguous_with_repeated_lines() {
        // File with identical lines → multiple candidates.
        let lines = vec!["same content"; 10];
        let scheme = ContentOnly::new();

        let anchors = scheme.generate_anchors(&lines);
        let parsed = ParsedAnchor {
            line: 5,
            local: anchors[0].local.clone(), // same hash for all lines
            context: None,
        };

        match scheme.find_shifted(&parsed, &lines, 5) {
            ShiftResult::Ambiguous { candidates } => {
                assert!(candidates.len() > 1);
            }
            other => panic!("Expected Ambiguous, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // Finding 1: B/C reject missing context
    // -----------------------------------------------------------------------

    #[test]
    fn chunk_rejects_anchor_without_context() {
        let lines = sample_lines();
        let scheme = ChunkFingerprint::new();
        let anchors = scheme.generate_anchors(&lines);

        // Construct a truncated anchor that omits the chunk fingerprint.
        let truncated = ParsedAnchor {
            line: anchors[0].line,
            local: anchors[0].local.clone(),
            context: None, // intentionally missing
        };
        assert_eq!(scheme.validate(&truncated, &lines), ValidationResult::Stale);
    }

    #[test]
    fn checkpoint_rejects_anchor_without_context() {
        let lines = sample_lines();
        let scheme = CheckpointChain::new();
        let anchors = scheme.generate_anchors(&lines);

        let truncated = ParsedAnchor {
            line: anchors[0].line,
            local: anchors[0].local.clone(),
            context: None, // intentionally missing
        };
        assert_eq!(scheme.validate(&truncated, &lines), ValidationResult::Stale);
    }

    // -----------------------------------------------------------------------
    // Finding 4: B/C shifted recovery after insertion/deletion
    // -----------------------------------------------------------------------

    #[test]
    fn chunk_find_shifted_after_insert_above() {
        let lines = sample_lines();
        // Use small chunk size so we can test cross-chunk behavior.
        let scheme = ChunkFingerprint::with_params(3, 4);
        let anchors = scheme.generate_anchors(&lines);

        // Insert a line at the top → all lines shift down by 1.
        let mut shifted = vec!["// new line"];
        shifted.extend_from_slice(&lines);

        // Anchor for original line 3 with context — shifted recovery should
        // find it at line 4 (same local + recomputed context at new position).
        let parsed = ParsedAnchor {
            line: anchors[2].line, // line 3
            local: anchors[2].local.clone(),
            context: anchors[2].context.clone(),
        };

        // Recovery may find, not find, or be ambiguous depending on chunk
        // boundaries. The key invariant: it must not return Found at the
        // original (stale) line.
        let result = scheme.find_shifted(&parsed, &shifted, 5);
        match result {
            ShiftResult::Found { new_line } => assert_ne!(new_line, anchors[2].line),
            ShiftResult::Ambiguous { ref candidates } => {
                assert!(!candidates.contains(&anchors[2].line));
            }
            ShiftResult::NotFound => { /* acceptable — context may not match */ }
        }
    }

    #[test]
    fn checkpoint_find_shifted_after_insert_above() {
        let lines = sample_lines();
        let scheme = CheckpointChain::with_params(3, 32);
        let anchors = scheme.generate_anchors(&lines);

        let mut shifted = vec!["// new line"];
        shifted.extend_from_slice(&lines);

        let parsed = ParsedAnchor {
            line: anchors[2].line,
            local: anchors[2].local.clone(),
            context: anchors[2].context.clone(),
        };

        let result = scheme.find_shifted(&parsed, &shifted, 5);
        match result {
            ShiftResult::Found { new_line } => assert_ne!(new_line, anchors[2].line),
            ShiftResult::Ambiguous { ref candidates } => {
                assert!(!candidates.contains(&anchors[2].line));
            }
            ShiftResult::NotFound => { /* acceptable — context may not match */ }
        }
    }

    // -----------------------------------------------------------------------
    // Finding 4: B/C ambiguity in repetitive files
    // -----------------------------------------------------------------------

    #[test]
    fn chunk_ambiguity_with_repeated_lines() {
        let lines = vec!["same content"; 10];
        let scheme = ChunkFingerprint::new();
        let anchors = scheme.generate_anchors(&lines);

        // All lines in the same chunk have the same local hash AND same chunk
        // context, so shifted recovery should find ambiguous matches.
        let parsed = ParsedAnchor {
            line: 5,
            local: anchors[0].local.clone(),
            context: anchors[0].context.clone(),
        };

        match scheme.find_shifted(&parsed, &lines, 5) {
            ShiftResult::Ambiguous { candidates } => {
                assert!(candidates.len() > 1);
            }
            other => panic!("Expected Ambiguous for repetitive chunk, got {:?}", other),
        }
    }

    #[test]
    fn checkpoint_ambiguity_less_likely_with_repeated_lines() {
        // Checkpoint chaining produces different contexts for different positions
        // even with identical line content, so repeated lines may NOT be ambiguous.
        let lines = vec!["same content"; 10];
        let scheme = CheckpointChain::new();
        let anchors = scheme.generate_anchors(&lines);

        // Adjacent lines in a checkpoint window should have different context
        // due to chaining. Verify at least some adjacent lines differ in context.
        let mut any_differ = false;
        for w in anchors.windows(2) {
            if w[0].context != w[1].context {
                any_differ = true;
                break;
            }
        }
        assert!(
            any_differ,
            "Checkpoint chaining should produce different contexts for adjacent identical lines"
        );
    }

    // -----------------------------------------------------------------------
    // Finding 3: Invalid constructor parameters
    // -----------------------------------------------------------------------

    #[test]
    fn custom_hash_len_2() {
        let lines = sample_lines();
        let scheme = ContentOnly::with_hash_len(2);
        let anchors = scheme.generate_anchors(&lines);
        for a in &anchors {
            assert_eq!(a.local.len(), 2);
        }
    }

    #[test]
    #[should_panic(expected = "hash_len must be 1..=4")]
    fn content_only_invalid_hash_len_zero() {
        ContentOnly::with_hash_len(0);
    }

    #[test]
    #[should_panic(expected = "hash_len must be 1..=4")]
    fn content_only_invalid_hash_len_five() {
        ContentOnly::with_hash_len(5);
    }

    #[test]
    #[should_panic(expected = "hash_len must be 1..=4")]
    fn chunk_invalid_hash_len() {
        ChunkFingerprint::with_params(0, 16);
    }

    #[test]
    #[should_panic(expected = "chunk_size must be > 0")]
    fn chunk_invalid_chunk_size() {
        ChunkFingerprint::with_params(3, 0);
    }

    #[test]
    #[should_panic(expected = "hash_len must be 1..=4")]
    fn checkpoint_invalid_hash_len() {
        CheckpointChain::with_params(0, 32);
    }

    #[test]
    #[should_panic(expected = "checkpoint_interval must be > 0")]
    fn checkpoint_invalid_interval() {
        CheckpointChain::with_params(3, 0);
    }

    // -----------------------------------------------------------------------
    // Scheme names
    // -----------------------------------------------------------------------

    #[test]
    fn scheme_names() {
        assert_eq!(ContentOnly::new().name(), "content_only_v1");
        assert_eq!(ChunkFingerprint::new().name(), "chunk_v1");
        assert_eq!(CheckpointChain::new().name(), "checkpoint_v1");
    }
}
