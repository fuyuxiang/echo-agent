//! PDF text extraction and page rendering shared by read tools.

use std::fmt::Write as _;

use base64::Engine as _;
use base64::engine::general_purpose;

use crate::types::output::{FileContent, PdfPageImage, PdfPageImages, ReadFileOutput};

use super::metadata::{bytes_to_metadata, is_pdf_magic};

pub const MAX_PDF_BYTES: usize = 50 * 1024 * 1024;
const PDF_AUTO_READ_THRESHOLD: usize = 10;
const PDF_RENDER_DPI: u32 = 150;
const PDF_RENDER_JPEG_QUALITY: u8 = 85;
pub const PDF_PROCESS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Maximum pages per read_file call when using explicit `pages` param.
pub const PDF_MAX_PAGES_PER_READ: usize = 20;

/// Shared async wrapper for document extraction (PDF, PPTX, etc.).
pub async fn run_document_extraction<F>(
    file_bytes: Vec<u8>,
    path: &std::path::Path,
    format_label: &str,
    max_bytes: usize,
    timeout: std::time::Duration,
    extract_fn: F,
) -> Result<ReadFileOutput, echo_agent_tool_runtime::ToolError>
where
    F: FnOnce(Vec<u8>) -> Result<ReadFileOutput, String> + Send + 'static,
{
    if file_bytes.len() > max_bytes {
        return Ok(ReadFileOutput::FileReadError(format!(
            "{format_label} file is {:.1} MB, exceeds the {:.0} MB limit.",
            file_bytes.len() as f64 / 1_048_576.0,
            max_bytes as f64 / 1_048_576.0,
        )));
    }

    tracing::info!(
        size_bytes = file_bytes.len(),
        format_label,
        "processing document"
    );

    let result = tokio::time::timeout(
        timeout,
        tokio::task::spawn_blocking(move || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| extract_fn(file_bytes)))
        }),
    )
    .await;

    match result {
        Ok(Ok(Ok(Ok(mut output)))) => {
            if let ReadFileOutput::FileContent(ref mut fc) = output {
                fc.absolute_path = path.to_path_buf();
            }
            Ok(output)
        }
        Err(_elapsed) => Ok(ReadFileOutput::FileReadError(format!(
            "{format_label} processing timed out after {}s: {}",
            timeout.as_secs(),
            path.display()
        ))),
        Ok(Ok(Ok(Err(e)))) => Ok(ReadFileOutput::FileReadError(e)),
        Ok(Ok(Err(_panic))) => Ok(ReadFileOutput::FileReadError(format!(
            "{format_label} processing failed (internal error): {}",
            path.display()
        ))),
        Ok(Err(e)) => Ok(ReadFileOutput::FileReadError(format!(
            "{format_label} processing failed: {}",
            e
        ))),
    }
}

pub(crate) async fn handle_pdf(
    file_bytes: Vec<u8>,
    path: &std::path::Path,
    pages: Option<String>,
    format: Option<&str>,
) -> Result<ReadFileOutput, echo_agent_tool_runtime::ToolError> {
    let extract_text = match format {
        None | Some("image") => false,
        Some("text") => true,
        Some(other) => {
            return Ok(ReadFileOutput::FileReadError(format!(
                "Invalid format '{}'. Supported values: 'image' (default), 'text'.",
                other
            )));
        }
    };

    run_document_extraction(
        file_bytes,
        path,
        "PDF",
        MAX_PDF_BYTES,
        PDF_PROCESS_TIMEOUT,
        move |bytes| {
            if extract_text {
                extract_pdf_text(bytes, pages.as_deref())
            } else {
                let file_size = bytes.len();
                render_pdf_pages(bytes, pages.as_deref(), file_size)
            }
        },
    )
    .await
}

/// Parse a page range specification into sorted, deduplicated 0-based page indices.
pub fn parse_page_range(spec: &str, page_count: usize) -> Result<Vec<usize>, String> {
    let mut pages = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((start, end)) = part.split_once('-') {
            let start: usize = start
                .trim()
                .parse()
                .map_err(|_| format!("invalid page number: '{}'", start.trim()))?;
            let end = if end.trim().is_empty() {
                page_count
            } else {
                end.trim()
                    .parse()
                    .map_err(|_| format!("invalid page number: '{}'", end.trim()))?
            };
            if start < 1 || start > page_count {
                return Err(format!(
                    "page {} out of range (document has {} pages)",
                    start, page_count
                ));
            }
            if start > end {
                return Err(format!(
                    "invalid page range: {}-{} (start must be ≤ end)",
                    start, end
                ));
            }
            let end = end.min(page_count);
            for p in start..=end {
                pages.push(p - 1);
            }
        } else {
            let p: usize = part
                .parse()
                .map_err(|_| format!("invalid page number: '{}'", part))?;
            if p < 1 || p > page_count {
                return Err(format!(
                    "page {} out of range (document has {} pages)",
                    p, page_count
                ));
            }
            pages.push(p - 1);
        }
    }
    pages.sort_unstable();
    pages.dedup();
    if pages.len() > PDF_MAX_PAGES_PER_READ {
        return Err(format!(
            "requested {} pages, maximum is {} per call",
            pages.len(),
            PDF_MAX_PAGES_PER_READ
        ));
    }
    if pages.is_empty() {
        return Err("no pages specified".to_string());
    }
    Ok(pages)
}

fn open_pdf_document(bytes: Vec<u8>) -> Result<(pdf_oxide::PdfDocument, usize), String> {
    let doc = pdf_oxide::PdfDocument::from_bytes(bytes)
        .map_err(|e| format!("Failed to open PDF: {e}"))?;

    let page_count = doc
        .page_count()
        .map_err(|e| format!("Failed to read PDF page count: {e}"))?;

    if page_count == 0 {
        return Err("PDF has no pages".to_string());
    }

    Ok((doc, page_count))
}

fn open_pdf_and_resolve_pages(
    bytes: Vec<u8>,
    pages_spec: Option<&str>,
) -> Result<(pdf_oxide::PdfDocument, usize, Vec<usize>), String> {
    let (doc, page_count) = open_pdf_document(bytes)?;

    let page_indices = match pages_spec {
        Some(spec) => parse_page_range(spec, page_count)?,
        None => {
            if page_count > PDF_AUTO_READ_THRESHOLD {
                return Err(format!(
                    "PDF has {} pages which exceeds the {} page auto-read limit. \
                     Use the `pages` parameter to specify which pages to read \
                     (e.g. pages=\"1-5\"). Maximum {} pages per call.",
                    page_count, PDF_AUTO_READ_THRESHOLD, PDF_MAX_PAGES_PER_READ
                ));
            }
            (0..page_count).collect()
        }
    };

    Ok((doc, page_count, page_indices))
}

pub(crate) fn render_pdf_pages(
    bytes: Vec<u8>,
    pages_spec: Option<&str>,
    file_size: usize,
) -> Result<ReadFileOutput, String> {
    let (doc, page_count, page_indices) = open_pdf_and_resolve_pages(bytes, pages_spec)?;

    let opts = pdf_oxide::rendering::RenderOptions::with_dpi(PDF_RENDER_DPI)
        .as_jpeg(PDF_RENDER_JPEG_QUALITY);

    let mut page_images = Vec::with_capacity(page_indices.len());
    for &page_idx in &page_indices {
        let image = pdf_oxide::rendering::render_page(&doc, page_idx, &opts)
            .map_err(|e| format!("Failed to render page {}: {e}", page_idx + 1))?;

        let b64 = general_purpose::STANDARD.encode(&image.data);
        page_images.push(PdfPageImage {
            data: b64,
            mime_type: "image/jpeg".to_string(),
            page_number: page_idx + 1,
        });
    }

    Ok(ReadFileOutput::PdfPageImages(PdfPageImages {
        document_progress: None,
        pages: page_images,
        total_pages: page_count,
        file_size,
    }))
}

pub fn raw_text_to_file_content(text: String) -> ReadFileOutput {
    let total_lines = text.matches('\n').count() + 1;
    let mut content = String::new();
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            content.push('\n');
        }
        write!(&mut content, "{}\u{2192}{line}", i + 1).ok();
    }

    ReadFileOutput::FileContent(FileContent {
        document_progress: None,
        content,
        content_concise: None,
        absolute_path: std::path::PathBuf::new(),
        offset: None,
        limit: None,
        raw_output: text,
        total_lines,
        extracted_images: Vec::new(),
    })
}

enum PageTextStyle {
    EchoAgentBuild,
    Cursor { total_pages: usize },
}

fn append_page_body(text: &mut String, doc: &pdf_oxide::PdfDocument, page_idx: usize) {
    match doc.extract_text(page_idx) {
        Ok(page_text) => text.push_str(&page_text),
        Err(e) => {
            writeln!(
                text,
                "[Failed to extract text from page {}: {e}]",
                page_idx + 1
            )
            .ok();
        }
    }
}

fn extract_page_texts(
    doc: &pdf_oxide::PdfDocument,
    page_indices: &[usize],
    style: PageTextStyle,
) -> Result<String, String> {
    let mut text = String::new();
    for (i, &page_idx) in page_indices.iter().enumerate() {
        if i > 0 {
            text.push('\n');
        }
        match style {
            PageTextStyle::EchoAgentBuild => {
                writeln!(&mut text, "--- Page {} ---", page_idx + 1).ok();
            }
            PageTextStyle::Cursor { .. } => {}
        }
        append_page_body(&mut text, doc, page_idx);
        if let PageTextStyle::Cursor { total_pages } = style {
            text.push_str("\n\n");
            let _ = writeln!(&mut text, "-- {} of {} --", page_idx + 1, total_pages);
            if i + 1 < page_indices.len() {
                text.push('\n');
            }
        }
    }
    Ok(text)
}

fn extract_pdf_plain_text(bytes: Vec<u8>, style: PageTextStyle) -> Result<String, String> {
    let (doc, page_count) = open_pdf_document(bytes)?;
    let page_indices: Vec<usize> = (0..page_count).collect();
    let style = match style {
        PageTextStyle::EchoAgentBuild => PageTextStyle::EchoAgentBuild,
        PageTextStyle::Cursor { .. } => PageTextStyle::Cursor {
            total_pages: page_count,
        },
    };
    extract_page_texts(&doc, &page_indices, style)
}

/// Extract plain text from all PDF pages (no auto-read page limit).
#[cfg(test)]
pub(crate) fn extract_pdf_plain_text_all(bytes: Vec<u8>) -> Result<String, String> {
    extract_pdf_plain_text(bytes, PageTextStyle::EchoAgentBuild)
}

/// Plain text from all PDF pages in the `Read` format.
pub fn extract_pdf_plain_text_cursor(bytes: Vec<u8>) -> Result<String, String> {
    extract_pdf_plain_text(bytes, PageTextStyle::Cursor { total_pages: 0 })
}

pub(crate) fn extract_pdf_records(
    bytes: &[u8],
) -> Result<Vec<super::document::DocumentRecord>, String> {
    let (doc, page_count) = open_pdf_document(bytes.to_vec())?;
    let mut records = Vec::new();
    let mut total_bytes = 0usize;
    for page in 0..page_count {
        let text = doc
            .extract_text(page)
            .map_err(|e| format!("Failed to extract PDF page {}: {e}", page + 1))?;
        total_bytes = total_bytes.saturating_add(text.len());
        if total_bytes > 128 * 1024 * 1024 {
            return Err("PDF exceeds the 128 MiB indexed text limit".into());
        }
        let label = format!("Page {}", page + 1);
        if text.trim().is_empty() {
            super::document::push_record(
                &mut records,
                &label,
                &label,
                "[No extractable text. Inspect this page visually with read_file format=image and pages set to its page number.]",
            )?;
            records.last_mut().unwrap().requires_visual = true;
            continue;
        }
        for (line, text) in text.split('\n').enumerate() {
            super::document::push_record(
                &mut records,
                &label,
                &format!("{label}, line {}", line + 1),
                text,
            )?;
        }
    }
    Ok(records)
}

pub(crate) fn extract_pdf_text(
    bytes: Vec<u8>,
    pages_spec: Option<&str>,
) -> Result<ReadFileOutput, String> {
    let (doc, _page_count, page_indices) = open_pdf_and_resolve_pages(bytes, pages_spec)?;
    let text = extract_page_texts(&doc, &page_indices, PageTextStyle::EchoAgentBuild)?;
    Ok(raw_text_to_file_content(text))
}

/// Three-tier PDF detection: infer metadata, magic bytes, or extension.
pub fn is_pdf_file(file_bytes: &[u8], extension: &str) -> bool {
    bytes_to_metadata(file_bytes).is_ok_and(|m| m.is_pdf())
        || is_pdf_magic(file_bytes)
        || extension == "pdf"
}

/// Minimal multi-page PDF fixture for unit tests.
pub fn make_test_pdf(page_texts: &[&str]) -> Vec<u8> {
    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n");

    let mut offsets = Vec::new();

    offsets.push(pdf.len());
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let page_count = page_texts.len();
    let kids: Vec<String> = (0..page_count)
        .map(|i| format!("{} 0 R", 3 + i * 3))
        .collect();
    offsets.push(pdf.len());
    let pages_obj = format!(
        "2 0 obj\n<< /Type /Pages /Kids [{}] /Count {} >>\nendobj\n",
        kids.join(" "),
        page_count
    );
    pdf.extend_from_slice(pages_obj.as_bytes());

    for (i, text) in page_texts.iter().enumerate() {
        let page_obj = 3 + i * 3;
        let content_obj = 4 + i * 3;
        let font_obj = 5 + i * 3;

        let stream_content = format!("BT /F1 12 Tf 72 720 Td ({text}) Tj ET");
        let stream_len = stream_content.len();

        offsets.push(pdf.len());
        pdf.extend_from_slice(
            format!(
                "{page_obj} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                 /Contents {content_obj} 0 R /Resources << /Font << /F1 {font_obj} 0 R >> >> >>\nendobj\n"
            )
            .as_bytes(),
        );

        offsets.push(pdf.len());
        pdf.extend_from_slice(
            format!(
                "{content_obj} 0 obj\n<< /Length {stream_len} >>\nstream\n{stream_content}\nendstream\nendobj\n"
            )
            .as_bytes(),
        );

        offsets.push(pdf.len());
        pdf.extend_from_slice(
            format!(
                "{font_obj} 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n"
            )
            .as_bytes(),
        );
    }

    let xref_offset = pdf.len();
    let total_objects = 2 + page_count * 3 + 1;
    pdf.extend_from_slice(format!("xref\n0 {total_objects}\n").as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        pdf.extend_from_slice(format!("{:010} 00000 n \n", offset).as_bytes());
    }

    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            total_objects, xref_offset
        )
        .as_bytes(),
    );

    pdf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_pdf_plain_text_all_reads_every_page() {
        let pdf_bytes = make_test_pdf(&["Alpha", "Beta"]);
        let text = extract_pdf_plain_text_all(pdf_bytes).unwrap();
        assert!(text.contains("--- Page 1 ---"));
        assert!(text.contains("--- Page 2 ---"));
        assert!(text.contains("Alpha"));
        assert!(text.contains("Beta"));
    }

    #[test]
    fn extract_pdf_plain_text_cursor_uses_page_of_markers() {
        let pdf_bytes = make_test_pdf(&["Alpha", "Beta"]);
        let text = extract_pdf_plain_text_cursor(pdf_bytes).unwrap();
        assert!(text.contains("Alpha"));
        assert!(text.contains("Beta"));
        assert!(text.contains("-- 1 of 2 --"));
        assert!(text.contains("-- 2 of 2 --"));
        assert!(!text.contains("--- Page"));
    }

    #[test]
    fn extract_pdf_text_returns_file_content() {
        let pdf_bytes = make_test_pdf(&["Hello World"]);
        let result = extract_pdf_text(pdf_bytes, None).unwrap();
        match result {
            ReadFileOutput::FileContent(fc) => {
                assert!(fc.raw_output.contains("Hello World"));
                assert!(fc.raw_output.contains("--- Page 1 ---"));
                assert!(fc.content.contains('\u{2192}'));
            }
            other => panic!("Expected FileContent, got {other:?}"),
        }
    }

    #[test]
    fn extract_pdf_text_multi_page() {
        let pdf_bytes = make_test_pdf(&["Page One", "Page Two"]);
        let result = extract_pdf_text(pdf_bytes, None).unwrap();
        match result {
            ReadFileOutput::FileContent(fc) => {
                assert!(fc.raw_output.contains("--- Page 1 ---"));
                assert!(fc.raw_output.contains("--- Page 2 ---"));
                assert!(fc.raw_output.contains("Page One"));
                assert!(fc.raw_output.contains("Page Two"));
            }
            other => panic!("Expected FileContent, got {other:?}"),
        }
    }

    #[test]
    fn extract_pdf_text_with_page_spec() {
        let pdf_bytes = make_test_pdf(&["First", "Second", "Third"]);
        let result = extract_pdf_text(pdf_bytes, Some("2")).unwrap();
        match result {
            ReadFileOutput::FileContent(fc) => {
                assert!(fc.raw_output.contains("--- Page 2 ---"));
                assert!(fc.raw_output.contains("Second"));
                assert!(!fc.raw_output.contains("--- Page 1 ---"));
                assert!(!fc.raw_output.contains("--- Page 3 ---"));
            }
            other => panic!("Expected FileContent, got {other:?}"),
        }
    }

    #[test]
    fn extract_pdf_text_invalid_pdf() {
        let err = extract_pdf_text(b"not a pdf".to_vec(), None).unwrap_err();
        assert!(err.contains("Failed to open PDF"), "got: {err}");
    }

    #[tokio::test]
    async fn handle_pdf_format_text() {
        let pdf_bytes = make_test_pdf(&["Test Content"]);
        let tmp = tempfile::TempDir::new().unwrap();
        let pdf_path = tmp.path().join("test.pdf");
        std::fs::write(&pdf_path, &pdf_bytes).unwrap();
        let result = handle_pdf(pdf_bytes, &pdf_path, None, Some("text"))
            .await
            .unwrap();
        match result {
            ReadFileOutput::FileContent(fc) => {
                assert!(fc.raw_output.contains("Test Content"));
                assert_eq!(fc.absolute_path, pdf_path);
            }
            other => panic!("Expected FileContent for format='text', got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handle_pdf_format_image() {
        let pdf_bytes = make_test_pdf(&["Some Text"]);
        let path = std::path::Path::new("/tmp/test.pdf");
        let result = handle_pdf(pdf_bytes, path, None, Some("image"))
            .await
            .unwrap();
        assert!(matches!(result, ReadFileOutput::PdfPageImages(_)));
    }

    #[test]
    fn render_pdf_pages_rejects_invalid_pdf() {
        let err = render_pdf_pages(b"not a pdf".to_vec(), None, 10).unwrap_err();
        assert!(err.contains("Failed to open PDF"), "got: {err}");
    }

    #[test]
    fn parse_page_range_single_page() {
        assert_eq!(parse_page_range("3", 10).unwrap(), vec![2]);
    }

    #[test]
    fn parse_page_range_rejects_too_many_pages() {
        let err = parse_page_range("1-21", 30).unwrap_err();
        assert!(err.contains("maximum is"), "got: {err}");
    }
}
