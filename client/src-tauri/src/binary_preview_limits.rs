use std::path::Path;

pub(super) const DEFAULT_BINARY_PREVIEW_BYTES: usize = 1024 * 1024;
pub(super) const PDF_PREVIEW_MAX_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const OOXML_PREVIEW_MAX_BYTES: usize = 8 * 1024 * 1024;

pub(crate) fn is_ooxml(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["docx", "xlsx", "pptx"]
                .iter()
                .any(|kind| extension.eq_ignore_ascii_case(kind))
        })
}

// Production policy shared with direct module boundary tests.
pub(super) fn binary_preview_limit(path: &Path, requested: Option<u64>) -> usize {
    let is_pdf = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"));
    let hard_max = if is_pdf {
        PDF_PREVIEW_MAX_BYTES
    } else if is_ooxml(path) {
        OOXML_PREVIEW_MAX_BYTES
    } else {
        DEFAULT_BINARY_PREVIEW_BYTES
    };
    requested
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(DEFAULT_BINARY_PREVIEW_BYTES)
        .min(hard_max)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_do_not_expand_existing_binary_reads() {
        for path in [
            "image.png",
            "audio.mp3",
            "report.pdf",
            "report.docx",
            "data.bin",
        ] {
            assert_eq!(
                binary_preview_limit(Path::new(path), None),
                DEFAULT_BINARY_PREVIEW_BYTES
            );
        }
    }
    #[test]
    fn an_app_generated_chinese_pdf_fits_the_explicit_pdf_preview_budget() {
        let actual_generated_bytes = 13_834_126;
        assert!(
            binary_preview_limit(
                Path::new("qa_office_f3.pdf"),
                Some(PDF_PREVIEW_MAX_BYTES as u64)
            ) >= actual_generated_bytes
        );
    }
    #[test]
    fn pdf_reads_remain_bounded_and_other_extensions_keep_the_small_limit() {
        assert_eq!(
            binary_preview_limit(Path::new("REPORT.PDF"), Some(u64::MAX)),
            PDF_PREVIEW_MAX_BYTES
        );
        for path in ["image.png", "report.pdf.tmp", "video.mp4", "data.bin"] {
            assert_eq!(
                binary_preview_limit(Path::new(path), Some(u64::MAX)),
                DEFAULT_BINARY_PREVIEW_BYTES
            );
        }
    }
    #[test]
    fn office_previews_allow_only_the_explicit_bounded_budget() {
        for path in ["word.DOCX", "slides.pptx", "table.xlsx"] {
            assert_eq!(
                binary_preview_limit(Path::new(path), Some(u64::MAX)),
                OOXML_PREVIEW_MAX_BYTES
            );
            assert_eq!(binary_preview_limit(Path::new(path), Some(1024)), 1024);
        }
        assert!(!is_ooxml(Path::new("table.xls")));
    }
    #[test]
    fn smaller_requested_limits_and_zero_are_preserved() {
        for path in ["report.pdf", "data.bin"] {
            assert_eq!(binary_preview_limit(Path::new(path), Some(64)), 64);
            assert_eq!(binary_preview_limit(Path::new(path), Some(0)), 0);
        }
    }
}
