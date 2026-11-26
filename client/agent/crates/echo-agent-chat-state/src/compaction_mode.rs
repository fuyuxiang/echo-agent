//! Compaction mode — how much structure the model gets to recover detail the
//! lossy summary dropped. In `echo-agent-chat-state` so flag resolution and the
//! transcript-hint builder share one definition.

use echo_agent_compaction_transcript::CompactionDetail;

/// How compaction exposes pre-compaction history to the model afterwards.
/// `Segments` carries its verbatim detail level inline, since detail is
/// meaningful only there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, strum::Display)]
#[strum(serialize_all = "snake_case")]
pub enum CompactionMode {
    /// Summary only — no pointer back to pre-compaction history. Default.
    #[default]
    Summary,
    /// Summary + pointer to the full raw `updates.jsonl`.
    Transcript,
    /// Summary + a `compaction/` folder of clean per-segment markdown.
    Segments(CompactionDetail),
}

impl CompactionMode {
    /// Parse the mode word (case-insensitive); unknown → `None` so the caller
    /// falls back. `segments` gets the default detail — callers override it via
    /// [`CompactionMode::with_segment_detail`] once detail is resolved.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "summary" => Some(Self::Summary),
            "transcript" => Some(Self::Transcript),
            "segments" => Some(Self::Segments(CompactionDetail::default())),
            _ => None,
        }
    }

    /// Replace the detail level if this is `Segments`, else unchanged. Lets the
    /// resolver attach the separately-resolved `ECHO_AGENT_COMPACTION_DETAIL`.
    pub fn with_segment_detail(self, detail: CompactionDetail) -> Self {
        match self {
            Self::Segments(_) => Self::Segments(detail),
            other => other,
        }
    }

    pub fn segment_detail(self) -> Option<CompactionDetail> {
        match self {
            Self::Segments(d) => Some(d),
            _ => None,
        }
    }

    /// Whether this mode persists the `compaction/` segment store.
    pub fn writes_segments(self) -> bool {
        matches!(self, Self::Segments(_))
    }

    /// Transcript hint for the summary, given the one `location` this mode points
    /// at (raw transcript path or `compaction/` folder). `None` if the mode adds
    /// no pointer (`Summary`) or the location is absent.
    pub fn transcript_hint(self, location: Option<&str>) -> Option<String> {
        use echo_agent_compaction_transcript::INDEX_FILE;
        let loc = location?;
        Some(match self {
            Self::Summary => return None,
            Self::Transcript => format!(
                "\n\nIf you need specific details from before compaction \
                 (like exact code snippets, error messages, or content you \
                 generated), read the full transcript at: {loc}"
            ),
            // Wording mirrors the segment-store continuation note.
            Self::Segments(_) => format!(
                "\n\nFull verbatim rollouts of previous segments are available \
                 at {loc}/segment_*.md.  See {loc}/{INDEX_FILE} for a table of \
                 contents.  Use read_file or grep to recover specific details \
                 (exact code, file paths, tool outputs) if this summary is \
                 insufficient.  Do NOT modify these files."
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `parse` is the public string contract (config + CLI): case-insensitive,
    /// unknown ⇒ `None` so the caller falls back.
    #[test]
    fn parse_maps_names_and_rejects_unknown() {
        assert_eq!(
            CompactionMode::parse("summary"),
            Some(CompactionMode::Summary)
        );
        assert_eq!(
            CompactionMode::parse("transcript"),
            Some(CompactionMode::Transcript)
        );
        // `segments` parses with the default detail; the resolver overrides it.
        assert_eq!(
            CompactionMode::parse("  SEGMENTS "),
            Some(CompactionMode::Segments(CompactionDetail::default()))
        );
        assert_eq!(CompactionMode::parse("nonsense"), None);
        assert_eq!(CompactionMode::default(), CompactionMode::Summary);
    }

    /// Detail is only attached to `Segments`; other modes ignore the override.
    #[test]
    fn with_segment_detail_only_affects_segments() {
        assert_eq!(
            CompactionMode::Segments(CompactionDetail::Verbose)
                .with_segment_detail(CompactionDetail::Minimal),
            CompactionMode::Segments(CompactionDetail::Minimal)
        );
        assert_eq!(
            CompactionMode::Summary.with_segment_detail(CompactionDetail::Minimal),
            CompactionMode::Summary
        );
        assert_eq!(
            CompactionMode::Segments(CompactionDetail::Balanced).segment_detail(),
            Some(CompactionDetail::Balanced)
        );
        assert_eq!(CompactionMode::Transcript.segment_detail(), None);
    }

    /// Contract: no hint for `Summary`, and never point the model at nothing.
    #[test]
    fn transcript_hint_needs_a_location() {
        let segments = CompactionMode::Segments(CompactionDetail::default());
        assert!(
            CompactionMode::Summary
                .transcript_hint(Some("/s/updates.jsonl"))
                .is_none()
        );
        assert!(CompactionMode::Transcript.transcript_hint(None).is_none());
        assert!(segments.transcript_hint(None).is_none());
        assert!(
            CompactionMode::Transcript
                .transcript_hint(Some("/s/updates.jsonl"))
                .unwrap()
                .contains("/s/updates.jsonl")
        );
        assert!(
            segments
                .transcript_hint(Some("/s/compaction"))
                .unwrap()
                .contains("/s/compaction")
        );
    }
}
