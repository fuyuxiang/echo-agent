//! Source mapping for rendered markdown back to original source.
//!
//! Used for copy-paste operations: when the user selects rendered text,
//! we can look up the corresponding original markdown source.

use std::ops::Range;

/// Maps rendered byte positions back to source byte positions.
///
/// Direction: rendered (new) → source (old)
#[derive(Debug, Clone, Default)]
pub struct SourceMap {
    /// Segments: (rendered_range, source_range)
    segments: Vec<(Range<usize>, Range<usize>)>,
}

impl SourceMap {
    /// Create an empty source map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a mapping from rendered position to source position.
    ///
    /// The rendered text and source text must have the same length.
    pub fn add(&mut self, rendered_start: usize, source_range: Range<usize>) {
        let len = source_range.end - source_range.start;
        if len > 0 {
            self.segments
                .push((rendered_start..rendered_start + len, source_range));
        }
    }

    /// Given a rendered byte range, return the corresponding source range.
    ///
    /// Returns None if the range doesn't map cleanly (e.g., spans multiple
    /// non-contiguous source regions).
    pub fn to_source(&self, rendered: Range<usize>) -> Option<Range<usize>> {
        let mut source_start = None;
        let mut source_end = None;

        for (r_range, s_range) in &self.segments {
            if r_range.end <= rendered.start || r_range.start >= rendered.end {
                continue;
            }

            let overlap_start = rendered.start.max(r_range.start);
            let overlap_end = rendered.end.min(r_range.end);
            let offset_start = overlap_start - r_range.start;
            let offset_end = overlap_end - r_range.start;

            let s_start = s_range.start + offset_start;
            let s_end = s_range.start + offset_end;

            source_start = Some(source_start.map_or(s_start, |v: usize| v.min(s_start)));
            source_end = Some(source_end.map_or(s_end, |v: usize| v.max(s_end)));
        }

        match (source_start, source_end) {
            (Some(s), Some(e)) => Some(s..e),
            _ => None,
        }
    }

    /// Check if the source map is empty.
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Get the number of segments.
    pub fn len(&self) -> usize {
        self.segments.len()
    }

    /// Extend with entries from another source map, applying offsets.
    ///
    /// Used when combining frozen content with newly rendered tail content.
    pub fn extend_with_offsets(
        &mut self,
        other: &Self,
        rendered_offset: usize,
        source_offset: usize,
    ) {
        for (r_range, s_range) in &other.segments {
            self.segments.push((
                (r_range.start + rendered_offset)..(r_range.end + rendered_offset),
                (s_range.start + source_offset)..(s_range.end + source_offset),
            ));
        }
    }

    /// Get read-only access to segments for inspection.
    pub fn segments(&self) -> &[(Range<usize>, Range<usize>)] {
        &self.segments
    }

    /// Truncate to keep only the first `n` segments.
    pub fn truncate(&mut self, n: usize) {
        self.segments.truncate(n);
    }

    /// Clear all segments.
    pub fn clear(&mut self) {
        self.segments.clear();
    }
}

// ## Restoring Byte-Level Source Maps (if ever needed)
//
// The ratatui rendering path currently only tracks line-level source mapping
// (`line_source_map`), which is sufficient for copy/selection operations.
// Byte-level `SourceMap` was removed for simplicity and ~6% speedup.
//
// To restore byte-level source maps:
//
// 1. Add field to MarkdownRenderOutput and MarkdownRenderView:
//    ```
//    pub source_map: SourceMap,
//    ```
//
// 2. In render_ratatui(), add tracking variables:
//    ```
//    let mut source_map = SourceMap::new();
//    let mut rendered_offset: usize = 0;
//    ```
//
// 3. For each text segment emitted, record the mapping:
//    ```
//    source_map.add(rendered_offset, source_start..source_end);
//    rendered_offset += emitted_text.len();
//    ```
//
// 4. In streaming.rs, update FrozenState to track:
//    ```
//    source_map_len: usize,
//    rendered_bytes: usize,
//    ```
//
// 5. Use SourceMap::extend_with_offsets() to merge tail source maps.
//
// See git history for the removed implementation.
