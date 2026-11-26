//! PPTX text extraction shared by read tools.
//!
//! Minimal zip + quick-xml implementation that replaces `omniparse` (which
//! dragged an old `scraper 0.18` / `cssparser 0.31` / `selectors 0.25` /
//! `calamine` line into the shipped pager binary for what is, here, just
//! "unzip and read the DrawingML text runs").
//!
//! Output format is compatible with the previous omniparse-based extraction:
//! `--- Slide N ---` headers, slide body text with one line per paragraph,
//! and a `Speaker Notes:` section when the slide has notes. Two omniparse
//! bugs are deliberately fixed rather than replicated: slides are ordered
//! numerically (slide2 before slide10, not lexicographically), and each
//! slide's notes are matched by the slide's own number instead of its
//! position in the sorted list.

use std::io::{Cursor, Read};

use quick_xml::Reader;
use quick_xml::events::Event;
use zip::ZipArchive;

/// Cap on the decompressed size of any single XML entry we read, guarding
/// against zip bombs (the compressed input is already capped by the caller).
const MAX_XML_ENTRY_BYTES: u64 = 64 * 1024 * 1024;

/// Extract plain text from PPTX bytes.
///
/// Returns the concatenated slide texts, or an error string suitable for
/// `ReadFileOutput::FileReadError`.
pub(crate) fn extract_pptx_text_from_bytes(bytes: &[u8]) -> Result<String, String> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| format!("Failed to open PPTX archive: {e}"))?;

    // Collect slide numbers from `ppt/slides/slideN.xml` entries and sort
    // numerically so slide ordering matches the presentation.
    let mut slide_numbers: Vec<u32> = archive
        .file_names()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("Invalid archive entry name: {error}"))?
        .into_iter()
        .filter_map(|name| {
            name.strip_prefix("ppt/slides/slide")?
                .strip_suffix(".xml")?
                .parse()
                .ok()
        })
        .collect();
    slide_numbers.sort_unstable();

    if slide_numbers.is_empty() {
        return Err("No slides found in PPTX".to_string());
    }

    let mut all_text = String::new();
    for number in slide_numbers {
        let slide_xml = read_entry(&mut archive, &format!("ppt/slides/slide{number}.xml"))?
            .ok_or_else(|| format!("Failed to read slide {number}"))?;
        let slide_text = extract_drawingml_text(&slide_xml)
            .map_err(|e| format!("Error parsing slide {number}: {e}"))?;

        // Notes are optional; a parse failure there shouldn't sink the slide.
        let notes_text = read_entry(
            &mut archive,
            &format!("ppt/notesSlides/notesSlide{number}.xml"),
        )
        .ok()
        .flatten()
        .and_then(|xml| extract_drawingml_text(&xml).ok())
        .unwrap_or_default();

        if !all_text.is_empty() {
            all_text.push_str("\n\n");
        }
        all_text.push_str(&format!("--- Slide {number} ---\n"));
        all_text.push_str(&slide_text);
        if !notes_text.is_empty() {
            all_text.push_str("\n\nSpeaker Notes:\n");
            all_text.push_str(&notes_text);
        }
    }

    Ok(all_text)
}

/// Read a zip entry to a string, `Ok(None)` if the entry does not exist.
fn read_entry(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    name: &str,
) -> Result<Option<String>, String> {
    let file = match archive.by_name(name) {
        Ok(file) => file,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => return Err(format!("Failed to open {name}: {e}")),
    };
    let mut content = String::new();
    file.take(MAX_XML_ENTRY_BYTES)
        .read_to_string(&mut content)
        .map_err(|e| format!("Failed to read {name}: {e}"))?;
    if content.len() as u64 == MAX_XML_ENTRY_BYTES {
        return Err(format!("{name} exceeds the decompressed size limit"));
    }
    Ok(Some(content))
}

/// Extract text from DrawingML: the character content of `<a:t>` runs,
/// concatenated per paragraph, one line per `<a:p>` paragraph.
fn extract_drawingml_text(xml: &str) -> Result<String, String> {
    // No `trim_text`: whitespace inside `<a:t>` runs is significant (runs are
    // frequently split mid-sentence), and text outside runs is already
    // excluded by the `in_text_run` gate below.
    let mut reader = Reader::from_str(xml);

    let mut text = String::new();
    let mut in_text_run = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) if e.local_name().as_ref() == b"t" => in_text_run = true,
            Ok(Event::Text(e)) if in_text_run => {
                let content = e.xml10_content().map_err(|e| e.to_string())?;
                text.push_str(&content);
            }
            // quick-xml ≥0.37 emits `&amp;` / `&#233;` as separate events
            // instead of unescaping them inside `Event::Text`.
            Ok(Event::GeneralRef(e)) if in_text_run => {
                if let Some(ch) = e.resolve_char_ref().map_err(|e| e.to_string())? {
                    text.push(ch);
                } else {
                    let name = e.decode().map_err(|e| e.to_string())?;
                    match quick_xml::escape::resolve_predefined_entity(&name) {
                        Some(resolved) => text.push_str(resolved),
                        // Unknown entity: keep the raw reference visible.
                        None => {
                            text.push('&');
                            text.push_str(&name);
                            text.push(';');
                        }
                    }
                }
            }
            Ok(Event::End(ref e)) => match e.local_name().as_ref() {
                b"t" => in_text_run = false,
                // End of paragraph: line break.
                b"p" if !text.is_empty() && !text.ends_with('\n') => text.push('\n'),
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(e) => return Err(e.to_string()),
            _ => {}
        }
    }
    Ok(text.trim().to_string())
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    /// Build an in-memory PPTX-shaped zip from (entry name, XML) pairs.
    fn build_zip(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for (name, content) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(content.as_bytes()).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn slide_xml(paragraphs: &[&str]) -> String {
        let body: String = paragraphs
            .iter()
            .map(|p| format!("<a:p><a:r><a:t>{p}</a:t></a:r></a:p>"))
            .collect();
        format!(
            r#"<?xml version="1.0"?><p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld><p:spTree><p:sp><p:txBody>{body}</p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#
        )
    }

    #[test]
    fn extracts_slides_in_numeric_order_with_notes() {
        let s1 = slide_xml(&["Title", "Body line"]);
        let s2 = slide_xml(&["Second"]);
        let s10 = slide_xml(&["Tenth"]);
        let notes2 = slide_xml(&["A note"]);
        let bytes = build_zip(&[
            ("[Content_Types].xml", "<Types/>"),
            // Deliberately out of order; slide10 sorts before slide2 lexically.
            ("ppt/slides/slide10.xml", &s10),
            ("ppt/slides/slide1.xml", &s1),
            ("ppt/slides/slide2.xml", &s2),
            ("ppt/notesSlides/notesSlide2.xml", &notes2),
        ]);

        let text = extract_pptx_text_from_bytes(&bytes).unwrap();
        assert_eq!(
            text,
            "--- Slide 1 ---\nTitle\nBody line\n\n--- Slide 2 ---\nSecond\n\nSpeaker Notes:\nA note\n\n--- Slide 10 ---\nTenth"
        );
    }

    #[test]
    fn split_text_runs_concatenate_without_injected_spaces() {
        // PowerPoint frequently splits a word across runs (e.g. spell-check
        // boundaries); the run texts must be joined without separators.
        let slide = r#"<p:sld xmlns:a="a" xmlns:p="p"><a:p><a:r><a:t>Hel</a:t></a:r><a:r><a:t>lo &amp; bye</a:t></a:r></a:p></p:sld>"#;
        let bytes = build_zip(&[("ppt/slides/slide1.xml", slide)]);
        let text = extract_pptx_text_from_bytes(&bytes).unwrap();
        assert_eq!(text, "--- Slide 1 ---\nHello & bye");
    }

    #[test]
    fn empty_text_elements_do_not_leak_surrounding_text() {
        // A self-closing <a:t/> must not flip the in-run flag on (omniparse
        // treated Empty like Start and then captured unrelated text nodes).
        let slide = r#"<p:sld xmlns:a="a" xmlns:p="p"><a:p><a:r><a:t/></a:r>stray<a:r><a:t>kept</a:t></a:r></a:p></p:sld>"#;
        let bytes = build_zip(&[("ppt/slides/slide1.xml", slide)]);
        let text = extract_pptx_text_from_bytes(&bytes).unwrap();
        assert_eq!(text, "--- Slide 1 ---\nkept");
    }

    #[test]
    fn not_a_zip_is_an_error() {
        let err = extract_pptx_text_from_bytes(b"plainly not a zip").unwrap_err();
        assert!(err.contains("Failed to open PPTX archive"), "{err}");
    }

    #[test]
    fn zip_without_slides_is_an_error() {
        let bytes = build_zip(&[("[Content_Types].xml", "<Types/>")]);
        let err = extract_pptx_text_from_bytes(&bytes).unwrap_err();
        assert_eq!(err, "No slides found in PPTX");
    }
}
