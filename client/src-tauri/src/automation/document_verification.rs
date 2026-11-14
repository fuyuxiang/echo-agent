//! Read-only, bounded outcome checks for Office packages and searchable PDF text.
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::{Cursor, Read},
    path::Path,
};
pub fn verify(
    scope: Option<&str>,
    args: &Value,
    locations: &super::artifact::Locations,
    extract_pdf: &dyn Fn(Vec<u8>) -> Result<String, String>,
) -> Result<Value, String> {
    let path = args["path"].as_str().ok_or("path_required")?;
    let target = Path::new(path);
    if !target.is_absolute() || !super::artifact::path_in_scope(scope, target, locations) {
        return Err("scope_required: document path must be explicit in the user's task".into());
    }
    let metadata = std::fs::symlink_metadata(target).map_err(|e| e.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 32 * 1024 * 1024
    {
        return Err("verification_limit: regular document at most 32 MiB required".into());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(target).map_err(|e| e.to_string())?;
    let mut bytes = vec![];
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("unsupported_target".into());
    }
    file.take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 32 * 1024 * 1024 {
        return Err("verification_limit".into());
    }
    let extension = target
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    let mut parts = HashMap::new();
    let text = if extension == "pdf" {
        extract_pdf(bytes).map_err(|e| format!("pdf_text_unavailable: {e}"))?
    } else if ["docx", "xlsx", "pptx"].contains(&extension.as_str()) {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
            .map_err(|e| format!("invalid_office_document: {e}"))?;
        if archive.len() > 4000 {
            return Err("verification_limit: too many package parts".into());
        }
        let mut total = 0u64;
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
            let name = entry.name().map_err(|e| e.to_string())?.into_owned();
            if !(name.ends_with(".xml") || name.ends_with(".rels"))
                || !(name.starts_with("word/")
                    || name.starts_with("xl/")
                    || name.starts_with("ppt/"))
            {
                continue;
            }
            total += entry.size();
            if entry.size() > 4 * 1024 * 1024 || total > 16 * 1024 * 1024 {
                return Err("verification_limit: Office XML too large".into());
            }
            let mut data = String::new();
            entry
                .by_ref()
                .take(4 * 1024 * 1024 + 1)
                .read_to_string(&mut data)
                .map_err(|e| e.to_string())?;
            if data.len() > 4 * 1024 * 1024 {
                return Err("verification_limit".into());
            }
            parts.insert(name, data);
        }
        crate::knowledge_documents::extract(&extension, parts.clone().into_iter().collect())?
    } else {
        return Err("unsupported_format: use exact text verification for plain-text files".into());
    };
    let contains = args["containsText"].as_array().cloned().unwrap_or_default();
    let expected_cells = args["cells"].as_array().cloned().unwrap_or_default();
    if contains.is_empty() && expected_cells.is_empty() {
        return Err(
            "verification_requirements_required: provide text fragments or exact spreadsheet cells"
                .into(),
        );
    }
    if contains.len() > 64 || expected_cells.len() > 128 {
        return Err("verification_limit: too many requirements".into());
    }
    let mut checks = vec![];
    for fragment in contains {
        let fragment = fragment
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 64000)
            .ok_or("invalid_text_requirement")?;
        checks.push(json!({"kind":"text_contains","matches":text.contains(fragment)}));
    }
    if !expected_cells.is_empty() {
        if extension != "xlsx" {
            return Err("cells_require_xlsx".into());
        }
        for cell in expected_cells {
            checks.push(json!({"kind":"cell","sheet":cell["sheet"],"cell":cell["cell"],"matches":xlsx_cell(&parts,&cell)?}));
        }
    }
    let after = std::fs::metadata(target).map_err(|e| e.to_string())?;
    let stable = after.len() == metadata.len() && after.modified().ok() == metadata.modified().ok();
    let verified = stable && checks.iter().all(|c| c["matches"] == true);
    Ok(
        json!({"path":path,"format":extension,"verified":verified,"status":if verified{"verified_document_content"}else if !stable{"file_changed_during_readback"}else{"document_mismatch"},"checks":checks,"coverage":"requested extracted text and exact cached spreadsheet cell values; layout and formula recalculation are not verified"}),
    )
}
fn attrs(event: &quick_xml::events::BytesStart<'_>) -> HashMap<String, String> {
    event
        .attributes()
        .flatten()
        .map(|a| {
            (
                String::from_utf8_lossy(a.key.local_name().as_ref()).into_owned(),
                String::from_utf8_lossy(&a.value).into_owned(),
            )
        })
        .collect()
}
fn entity(value: &quick_xml::events::BytesRef<'_>) -> Result<String, String> {
    let raw = value.decode().map_err(|e| e.to_string())?;
    quick_xml::escape::unescape(&format!("&{raw};"))
        .map(|s| s.into_owned())
        .map_err(|e| e.to_string())
}
fn xml_text(xml: &str) -> Result<Vec<String>, String> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut values = vec![];
    let mut value = String::new();
    let mut in_item = false;
    let mut reading = false;
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Start(e) if e.local_name().as_ref() == b"si" => {
                in_item = true;
                value.clear();
            }
            Event::Start(e) if in_item && e.local_name().as_ref() == b"t" => reading = true,
            Event::End(e) if e.local_name().as_ref() == b"t" => reading = false,
            Event::GeneralRef(r) if in_item && reading => value.push_str(&entity(&r)?),
            Event::Text(t) if in_item && reading => value.push_str(
                &quick_xml::escape::unescape(&t.decode().map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?,
            ),
            Event::End(e) if e.local_name().as_ref() == b"si" => {
                values.push(value.clone());
                in_item = false;
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(values)
}
fn xlsx_cell(parts: &HashMap<String, String>, expected: &Value) -> Result<bool, String> {
    use quick_xml::events::Event;
    let sheet = expected["sheet"].as_str().ok_or("sheet_required")?;
    let address = expected["cell"].as_str().ok_or("cell_required")?;
    let wanted = expected["value"]
        .as_str()
        .ok_or("cell_value_must_be_text")?;
    let mut reader =
        quick_xml::Reader::from_str(parts.get("xl/workbook.xml").ok_or("workbook_missing")?);
    let mut relation = None;
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Empty(e) | Event::Start(e) if e.local_name().as_ref() == b"sheet" => {
                let a = attrs(&e);
                if a.get("name").is_some_and(|s| {
                    quick_xml::escape::unescape(s)
                        .map(|s| s == sheet)
                        .unwrap_or(false)
                }) {
                    relation = a.get("id").cloned();
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let Some(relation) = relation else {
        return Ok(false);
    };
    let mut reader = quick_xml::Reader::from_str(
        parts
            .get("xl/_rels/workbook.xml.rels")
            .ok_or("workbook_relationships_missing")?,
    );
    let mut target = None;
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Empty(e) | Event::Start(e) if e.local_name().as_ref() == b"Relationship" => {
                let a = attrs(&e);
                if a.get("Id") == Some(&relation) {
                    target = a.get("Target").cloned();
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let Some(target) = target else {
        return Ok(false);
    };
    let target = if target.starts_with('/') {
        target.trim_start_matches('/').to_owned()
    } else {
        format!("xl/{target}")
    };
    let Some(xml) = parts.get(&target) else {
        return Ok(false);
    };
    let shared = xml_text(
        parts
            .get("xl/sharedStrings.xml")
            .map(String::as_str)
            .unwrap_or(""),
    )?;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut found = false;
    let mut kind = String::new();
    let mut value = String::new();
    let mut reading = false;
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Start(e) if e.local_name().as_ref() == b"c" => {
                let a = attrs(&e);
                found = a.get("r").is_some_and(|r| r == address);
                kind = a.get("t").cloned().unwrap_or_default();
                value.clear();
            }
            Event::Start(e)
                if found
                    && [b"v".as_slice(), b"t".as_slice()].contains(&e.local_name().as_ref()) =>
            {
                reading = true
            }
            Event::Text(t) if found && reading => value.push_str(
                &quick_xml::escape::unescape(&t.decode().map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?,
            ),
            Event::GeneralRef(r) if found && reading => value.push_str(&entity(&r)?),
            Event::End(e)
                if [b"v".as_slice(), b"t".as_slice()].contains(&e.local_name().as_ref()) =>
            {
                reading = false
            }
            Event::End(e) if found && e.local_name().as_ref() == b"c" => {
                let actual = if kind == "s" {
                    value
                        .parse::<usize>()
                        .ok()
                        .and_then(|i| shared.get(i))
                        .cloned()
                        .ok_or("invalid_shared_string")?
                } else {
                    value
                };
                return Ok(actual == wanted);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    struct Fixture(std::path::PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    fn package(extension: &str, parts: &[(&str, &str)]) -> Fixture {
        let path = std::env::temp_dir().join(format!(
            "echo-outcome-{}.{}",
            uuid::Uuid::now_v7(),
            extension
        ));
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        for (name, content) in parts {
            zip.start_file(
                *name,
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        Fixture(path)
    }
    #[test]
    fn verifies_actual_word_content_and_rejects_wrong_scope_or_text() {
        let fixture=package("docx",&[("word/document.xml","<w:document xmlns:w='word'><w:body><w:p><w:r><w:t>中文成果</w:t></w:r></w:p></w:body></w:document>")]);
        let path = fixture.0.to_str().unwrap();
        let args = json!({"path":path,"containsText":["中文成果"]});
        let locations = super::super::artifact::Locations::default();
        assert_eq!(
            verify(Some(path), &args, &locations, &|_| Err("unused".into())).unwrap()["verified"],
            true
        );
        assert!(verify(Some("other"), &args, &locations, &|_| Err("unused".into())).is_err());
        assert_eq!(
            verify(
                Some(path),
                &json!({"path":path,"containsText":["缺失内容"]}),
                &locations,
                &|_| Err("unused".into())
            )
            .unwrap()["verified"],
            false
        );
    }
    #[test]
    fn verifies_exact_sheet_cell_and_shared_unicode_entities() {
        let fixture=package("xlsx",&[
            ("xl/workbook.xml","<workbook xmlns:r='rel'><sheets><sheet name='客户' sheetId='1' r:id='rId1'/></sheets></workbook>"),
            ("xl/_rels/workbook.xml.rels","<Relationships><Relationship Id='rId1' Target='worksheets/sheet1.xml'/></Relationships>"),
            ("xl/sharedStrings.xml","<sst><si>\n<t>甲&amp;乙</t>\n</si></sst>"),
            ("xl/worksheets/sheet1.xml","<worksheet><sheetData><row r='1'><c r='A1' t='s'><v>0</v></c><c r='B1'><v>42</v></c></row></sheetData></worksheet>")]);
        let path = fixture.0.to_str().unwrap();
        let locations = super::super::artifact::Locations::default();
        let args = json!({"path":path,"cells":[{"sheet":"客户","cell":"A1","value":"甲&乙"},{"sheet":"客户","cell":"B1","value":"42"}]});
        assert_eq!(
            verify(Some(path), &args, &locations, &|_| Err("unused".into())).unwrap()["verified"],
            true
        );
        let mut wrong = args;
        wrong["cells"][0]["sheet"] = json!("其他表");
        assert_eq!(
            verify(Some(path), &wrong, &locations, &|_| Err("unused".into())).unwrap()["verified"],
            false
        );
    }
}
