//! Bounded text extraction for modern ZIP/XML document formats.

use std::collections::HashMap;
use std::io::{Cursor, Read};

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use zip::ZipArchive;

const MAX_XML_ENTRY_BYTES: u64 = 64 * 1024 * 1024;
const MAX_EXTRACTED_TEXT_BYTES: usize = 16 * 1024 * 1024;
const MAX_XLSX_COLUMNS: usize = 16_384;
const MAX_XLSX_ROWS: u32 = 1_048_576;

fn archive<'a>(bytes: &'a [u8], label: &str) -> Result<ZipArchive<Cursor<&'a [u8]>>, String> {
    ZipArchive::new(Cursor::new(bytes)).map_err(|error| format!("Failed to open {label}: {error}"))
}

fn read_entry(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    name: &str,
) -> Result<Option<String>, String> {
    let file = match archive.by_name(name) {
        Ok(file) => file,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(error) => return Err(format!("Failed to open {name}: {error}")),
    };
    if file.size() > MAX_XML_ENTRY_BYTES {
        return Err(format!("{name} exceeds the decompressed size limit"));
    }
    let mut content = String::new();
    file.take(MAX_XML_ENTRY_BYTES + 1)
        .read_to_string(&mut content)
        .map_err(|error| format!("Failed to read {name}: {error}"))?;
    if content.len() as u64 > MAX_XML_ENTRY_BYTES {
        return Err(format!("{name} exceeds the decompressed size limit"));
    }
    Ok(Some(content))
}

fn push_text(output: &mut String, event: &quick_xml::events::BytesText<'_>) -> Result<(), String> {
    let text = event.xml10_content().map_err(|error| error.to_string())?;
    bounded_push(output, &text)
}

fn push_reference(
    output: &mut String,
    event: &quick_xml::events::BytesRef<'_>,
) -> Result<(), String> {
    if let Some(ch) = event
        .resolve_char_ref()
        .map_err(|error| error.to_string())?
    {
        bounded_push(output, &ch.to_string())?;
    } else {
        let name = event.decode().map_err(|error| error.to_string())?;
        if let Some(resolved) = quick_xml::escape::resolve_predefined_entity(&name) {
            bounded_push(output, resolved)?;
        }
    }
    Ok(())
}

fn bounded_push(output: &mut String, value: &str) -> Result<(), String> {
    if output.len().saturating_add(value.len()) > MAX_EXTRACTED_TEXT_BYTES {
        return Err("Extracted document text exceeds the 16MB limit".to_string());
    }
    output.push_str(value);
    Ok(())
}

fn attribute(reader: &Reader<&[u8]>, start: &BytesStart<'_>, name: &[u8]) -> Option<String> {
    start.attributes().flatten().find_map(|attribute| {
        (attribute.key.local_name().as_ref() == name)
            .then(|| {
                attribute
                    .decoded_and_normalized_value(
                        quick_xml::XmlVersion::Implicit1_0,
                        reader.decoder(),
                    )
                    .ok()
            })
            .flatten()
            .map(|value| value.into_owned())
    })
}

pub(crate) fn extract_docx_text(bytes: &[u8]) -> Result<String, String> {
    let mut zip = archive(bytes, "DOCX archive")?;
    let xml = read_entry(&mut zip, "word/document.xml")?
        .ok_or_else(|| "DOCX has no word/document.xml".to_string())?;
    let mut reader = Reader::from_str(&xml);
    let mut output = String::new();
    let mut in_text = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) if event.local_name().as_ref() == b"t" => in_text = true,
            Ok(Event::Empty(event)) if event.local_name().as_ref() == b"tab" => {
                bounded_push(&mut output, "\t")?;
            }
            Ok(Event::Empty(event)) if matches!(event.local_name().as_ref(), b"br" | b"cr") => {
                bounded_push(&mut output, "\n")?;
            }
            Ok(Event::Text(event)) if in_text => push_text(&mut output, &event)?,
            Ok(Event::GeneralRef(event)) if in_text => push_reference(&mut output, &event)?,
            Ok(Event::End(event)) => match event.local_name().as_ref() {
                b"t" => in_text = false,
                b"p" if !output.ends_with('\n') => bounded_push(&mut output, "\n")?,
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(error) => return Err(format!("Failed to parse DOCX XML: {error}")),
            _ => {}
        }
    }
    let text = output.trim().to_string();
    if text.is_empty() {
        Err("DOCX contains no readable text".to_string())
    } else {
        Ok(text)
    }
}

fn shared_strings(zip: &mut ZipArchive<Cursor<&[u8]>>) -> Result<Vec<String>, String> {
    let Some(xml) = read_entry(zip, "xl/sharedStrings.xml")? else {
        return Ok(Vec::new());
    };
    let mut reader = Reader::from_str(&xml);
    let mut strings = Vec::new();
    let mut current = String::new();
    let mut in_item = false;
    let mut in_text = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) if event.local_name().as_ref() == b"si" => {
                current.clear();
                in_item = true;
            }
            Ok(Event::Start(event)) if event.local_name().as_ref() == b"t" && in_item => {
                in_text = true;
            }
            Ok(Event::Text(event)) if in_text => push_text(&mut current, &event)?,
            Ok(Event::GeneralRef(event)) if in_text => push_reference(&mut current, &event)?,
            Ok(Event::End(event)) if event.local_name().as_ref() == b"t" => in_text = false,
            Ok(Event::End(event)) if event.local_name().as_ref() == b"si" => {
                strings.push(std::mem::take(&mut current));
                in_item = false;
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(format!("Failed to parse XLSX shared strings: {error}")),
            _ => {}
        }
    }
    Ok(strings)
}

fn extract_sheet(xml: &str, shared: &[String]) -> Result<String, String> {
    let mut output = String::new();
    visit_sheet_rows(xml, shared, |_, row| {
        bounded_push(&mut output, &row.join("\t"))?;
        bounded_push(&mut output, "\n")
    })?;
    Ok(output.trim_end_matches('\n').to_string())
}

fn visit_sheet_rows(
    xml: &str,
    shared: &[String],
    mut emit: impl FnMut(u32, &[String]) -> Result<(), String>,
) -> Result<(), String> {
    let mut reader = Reader::from_str(xml);
    let mut row_index = 0;
    let mut row: Vec<String> = Vec::new();
    let mut row_text_bytes = 0;
    let mut cell = String::new();
    let mut cell_type = String::new();
    let mut in_cell = false;
    let mut in_value = false;
    let mut cell_column = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) if event.local_name().as_ref() == b"row" => {
                row_index = attribute(&reader, &event, b"r")
                    .and_then(|n| n.parse::<u32>().ok())
                    .unwrap_or(row_index + 1);
                if row_index == 0 || row_index > MAX_XLSX_ROWS {
                    return Err("Invalid XLSX row index".into());
                }
                row.clear();
                row_text_bytes = 0;
            }
            Ok(Event::Start(event)) if event.local_name().as_ref() == b"c" => {
                in_cell = true;
                cell.clear();
                cell_type = attribute(&reader, &event, b"t").unwrap_or_default();
                cell_column = xlsx_cell_column(&reader, &event)?;
            }
            Ok(Event::Empty(event)) if event.local_name().as_ref() == b"c" => {
                let column = xlsx_cell_column(&reader, &event)?;
                push_xlsx_cell(
                    &mut row,
                    &mut row_text_bytes,
                    column,
                    "",
                    MAX_EXTRACTED_TEXT_BYTES,
                )?;
            }
            Ok(Event::Start(event))
                if in_cell && matches!(event.local_name().as_ref(), b"v" | b"t") =>
            {
                in_value = true;
            }
            Ok(Event::Text(event)) if in_value => push_text(&mut cell, &event)?,
            Ok(Event::GeneralRef(event)) if in_value => push_reference(&mut cell, &event)?,
            Ok(Event::End(event)) if matches!(event.local_name().as_ref(), b"v" | b"t") => {
                in_value = false;
            }
            Ok(Event::End(event)) if event.local_name().as_ref() == b"c" => {
                let value = if cell_type == "s" {
                    cell.parse::<usize>()
                        .ok()
                        .and_then(|index| shared.get(index))
                        .map(String::as_str)
                        .unwrap_or(&cell)
                } else {
                    &cell
                };
                push_xlsx_cell(
                    &mut row,
                    &mut row_text_bytes,
                    cell_column,
                    value,
                    MAX_EXTRACTED_TEXT_BYTES,
                )?;
                in_cell = false;
            }
            Ok(Event::End(event)) if event.local_name().as_ref() == b"row" => {
                emit(row_index, &row)?;
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(format!("Failed to parse XLSX worksheet: {error}")),
            _ => {}
        }
    }
    Ok(())
}

/// Cell-aware index. Long code cells retain their exact worksheet/cell locator
/// across every bounded piece instead of becoming an unbounded TSV row.
pub(crate) fn extract_xlsx_records(
    bytes: &[u8],
) -> Result<Vec<super::document::DocumentRecord>, String> {
    let mut zip = archive(bytes, "XLSX archive")?;
    let shared = shared_strings(&mut zip)?;
    let sheets = xlsx_sheets(&mut zip)?;
    if sheets.is_empty() {
        return Err("XLSX contains no worksheets".into());
    }
    let mut records = Vec::new();
    let mut total_bytes = 0usize;
    for (label, name) in sheets {
        let xml = read_entry(&mut zip, &name)?.ok_or_else(|| format!("Missing {name}"))?;
        let before = records.len();
        visit_sheet_rows(&xml, &shared, |row_index, row| {
            for (column, value) in row.iter().enumerate() {
                if value.is_empty() {
                    continue;
                }
                total_bytes = total_bytes.saturating_add(value.len());
                if total_bytes > 128 * 1024 * 1024 {
                    return Err("Workbook exceeds the 128 MiB indexed text limit".into());
                }
                let mut n = column + 1;
                let mut letters = Vec::new();
                while n > 0 {
                    n -= 1;
                    letters.push((b'A' + (n % 26) as u8) as char);
                    n /= 26;
                }
                let col: String = letters.into_iter().rev().collect();
                super::document::push_record(
                    &mut records,
                    &label,
                    &format!("'{}'!{col}{row_index}", label.replace('\'', "''")),
                    value,
                )?;
            }
            Ok(())
        })?;
        if records.len() == before {
            super::document::push_record(&mut records, &label, &label, "[Empty worksheet]")?;
        }
    }
    Ok(records)
}

fn xlsx_column_index(reference: &str) -> Option<usize> {
    let mut column = 0_usize;
    let mut letters = 0_usize;
    for byte in reference.bytes() {
        if !byte.is_ascii_alphabetic() {
            break;
        }
        column = column
            .checked_mul(26)?
            .checked_add((byte.to_ascii_uppercase() - b'A' + 1) as usize)?;
        if column > MAX_XLSX_COLUMNS {
            return None;
        }
        letters += 1;
    }
    let row = reference.get(letters..)?;
    if row.is_empty() || !row.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let row: u32 = row.parse().ok()?;
    (letters > 0 && column > 0 && row > 0 && row <= MAX_XLSX_ROWS).then(|| column - 1)
}

fn xlsx_cell_column(
    reader: &Reader<&[u8]>,
    cell: &BytesStart<'_>,
) -> Result<Option<usize>, String> {
    attribute(reader, cell, b"r")
        .map(|reference| {
            xlsx_column_index(&reference)
                .ok_or_else(|| format!("Invalid XLSX cell reference: {reference}"))
        })
        .transpose()
}

fn push_xlsx_cell(
    row: &mut Vec<String>,
    row_text_bytes: &mut usize,
    column: Option<usize>,
    value: &str,
    remaining_bytes: usize,
) -> Result<(), String> {
    let column = column.unwrap_or(row.len());
    if column >= MAX_XLSX_COLUMNS {
        return Err("XLSX row exceeds the 16384-column limit".into());
    }
    let previous_bytes = row.get(column).map_or(0, String::len);
    let next_bytes = row_text_bytes
        .saturating_sub(previous_bytes)
        .saturating_add(value.len());
    let row_length = row.len().max(column + 1);
    // Include tabs and the terminating newline. Check a shared-string expansion
    // before cloning it, rather than allocating the whole row and then joining.
    if next_bytes.saturating_add(row_length) > remaining_bytes {
        return Err("Extracted document text exceeds the 16MB limit".into());
    }
    row.resize_with(row_length, String::new);
    row[column] = value.to_owned();
    *row_text_bytes = next_bytes;
    Ok(())
}

fn xlsx_part_target(target: &str) -> Result<String, String> {
    let mut parts = if target.starts_with('/') {
        vec![]
    } else {
        vec!["xl"]
    };
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop().ok_or("Invalid XLSX worksheet target")?;
            }
            _ => parts.push(part),
        }
    }
    Ok(parts.join("/"))
}

fn xlsx_sheets(zip: &mut ZipArchive<Cursor<&[u8]>>) -> Result<Vec<(String, String)>, String> {
    let Some(workbook) = read_entry(zip, "xl/workbook.xml")? else {
        // Older minimal archives lack workbook metadata. Keep their existing
        // numeric labels while real workbooks retain names and user ordering.
        let mut numbers: Vec<u32> = zip
            .file_names()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("Invalid archive entry name: {error}"))?
            .into_iter()
            .filter_map(|name| {
                name.strip_prefix("xl/worksheets/sheet")?
                    .strip_suffix(".xml")?
                    .parse()
                    .ok()
            })
            .collect();
        numbers.sort_unstable();
        return Ok(numbers
            .into_iter()
            .map(|number| {
                (
                    format!("Sheet {number}"),
                    format!("xl/worksheets/sheet{number}.xml"),
                )
            })
            .collect());
    };
    let relationships = read_entry(zip, "xl/_rels/workbook.xml.rels")?
        .ok_or("XLSX workbook has no worksheet relationships")?;
    let mut targets = HashMap::new();
    let mut reader = Reader::from_str(&relationships);
    loop {
        match reader.read_event() {
            Ok(Event::Start(event) | Event::Empty(event))
                if event.local_name().as_ref() == b"Relationship" =>
            {
                let id = attribute(&reader, &event, b"Id").ok_or("XLSX relationship has no ID")?;
                let target = attribute(&reader, &event, b"Target")
                    .ok_or("XLSX relationship has no target")?;
                let external =
                    attribute(&reader, &event, b"TargetMode").as_deref() == Some("External");
                let worksheet = attribute(&reader, &event, b"Type")
                    .is_none_or(|kind| kind.ends_with("/worksheet"));
                if targets.insert(id, (target, external, worksheet)).is_some() {
                    return Err("XLSX workbook has duplicate relationship IDs".into());
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(format!("Failed to parse XLSX relationships: {error}")),
            _ => {}
        }
    }
    let mut sheets = Vec::new();
    let mut reader = Reader::from_str(&workbook);
    loop {
        match reader.read_event() {
            Ok(Event::Start(event) | Event::Empty(event))
                if event.local_name().as_ref() == b"sheet" =>
            {
                let name =
                    attribute(&reader, &event, b"name").ok_or("XLSX worksheet has no name")?;
                let id = attribute(&reader, &event, b"id")
                    .ok_or("XLSX worksheet has no relationship ID")?;
                let (target, external, worksheet) = targets
                    .get(&id)
                    .ok_or("XLSX worksheet relationship is missing")?;
                if *external {
                    return Err("External XLSX worksheets are not supported".into());
                }
                if *worksheet {
                    sheets.push((name, xlsx_part_target(target)?));
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(format!("Failed to parse XLSX workbook: {error}")),
            _ => {}
        }
    }
    Ok(sheets)
}

pub(crate) fn extract_xlsx_text(bytes: &[u8]) -> Result<String, String> {
    let mut zip = archive(bytes, "XLSX archive")?;
    let shared = shared_strings(&mut zip)?;
    let sheets = xlsx_sheets(&mut zip)?;
    if sheets.is_empty() {
        return Err("XLSX contains no worksheets".to_string());
    }
    let mut output = String::new();
    for (label, name) in sheets {
        let xml = read_entry(&mut zip, &name)?.ok_or_else(|| format!("Missing {name}"))?;
        let sheet = extract_sheet(&xml, &shared)?;
        if !output.is_empty() {
            bounded_push(&mut output, "\n\n")?;
        }
        bounded_push(&mut output, &format!("--- {label} ---\n"))?;
        bounded_push(&mut output, &sheet)?;
    }
    Ok(output)
}

#[derive(Default)]
struct OdfTableText {
    text: String,
    pending_empty_rows: usize,
    row: Option<OdfRowText>,
}

struct OdfRowText {
    text: String,
    pending_empty_columns: usize,
    has_content: bool,
    repeat: usize,
    cell: Option<OdfCellText>,
}

struct OdfCellText {
    text: String,
    repeat: usize,
}

fn odf_repeat(
    reader: &Reader<&[u8]>,
    event: &BytesStart<'_>,
    name: &[u8],
) -> Result<usize, String> {
    let mut value = None;
    for attribute in event.attributes() {
        let attribute =
            attribute.map_err(|error| format!("Invalid OpenDocument attribute: {error}"))?;
        if attribute.key.local_name().as_ref() == name {
            if value.is_some() {
                return Err("OpenDocument has duplicate repeat attributes".into());
            }
            value = Some(
                attribute
                    .decoded_and_normalized_value(
                        quick_xml::XmlVersion::Implicit1_0,
                        reader.decoder(),
                    )
                    .map_err(|error| format!("Invalid OpenDocument repeat attribute: {error}"))?
                    .into_owned(),
            );
        }
    }
    let Some(value) = value else {
        return Ok(1);
    };
    value
        .trim()
        .parse::<usize>()
        .ok()
        .filter(|count| *count > 0)
        .ok_or_else(|| {
            format!(
                "Invalid OpenDocument {}: {value}",
                String::from_utf8_lossy(name)
            )
        })
}

fn odf_checked_growth(used: usize, addition: Option<usize>) -> Result<usize, String> {
    addition
        .and_then(|addition| used.checked_add(addition))
        .filter(|total| *total <= MAX_EXTRACTED_TEXT_BYTES)
        .ok_or_else(|| "Extracted document text exceeds the 16MB limit".to_string())
}

fn odf_push(output: &mut String, value: &str, used: &mut usize) -> Result<(), String> {
    let next = odf_checked_growth(*used, Some(value.len()))?;
    output.push_str(value);
    *used = next;
    Ok(())
}

fn odf_push_repeated(
    output: &mut String,
    value: &str,
    count: usize,
    used: &mut usize,
) -> Result<(), String> {
    if value.is_empty() || count == 0 {
        return Ok(());
    }
    let next = odf_checked_growth(*used, value.len().checked_mul(count))?;
    output.reserve(next - *used);
    for _ in 0..count {
        output.push_str(value);
    }
    *used = next;
    Ok(())
}

fn odf_sink<'a>(output: &'a mut String, tables: &'a mut [OdfTableText]) -> &'a mut String {
    if let Some(table) = tables.last_mut() {
        if let Some(cell) = table.row.as_mut().and_then(|row| row.cell.as_mut()) {
            return &mut cell.text;
        }
        &mut table.text
    } else {
        output
    }
}

fn odf_row_mut(tables: &mut [OdfTableText]) -> Result<&mut OdfRowText, String> {
    tables
        .last_mut()
        .and_then(|table| table.row.as_mut())
        .ok_or_else(|| "OpenDocument table cell has no table row".to_string())
}

fn odf_begin_row(
    reader: &Reader<&[u8]>,
    event: &BytesStart<'_>,
    tables: &mut [OdfTableText],
    used: &mut usize,
    empty: bool,
) -> Result<(), String> {
    let repeat = odf_repeat(reader, event, b"number-rows-repeated")?;
    let table = tables
        .last_mut()
        .ok_or("OpenDocument table row has no table")?;
    if table.row.is_some() {
        return Err("OpenDocument table has overlapping rows".to_string());
    }
    table.row = Some(OdfRowText {
        text: String::new(),
        pending_empty_columns: 0,
        has_content: false,
        repeat,
        cell: None,
    });
    if empty {
        odf_finish_row(tables, used)?;
    }
    Ok(())
}

fn odf_begin_cell(
    reader: &Reader<&[u8]>,
    event: &BytesStart<'_>,
    tables: &mut [OdfTableText],
    used: &mut usize,
    empty: bool,
) -> Result<(), String> {
    let repeat = odf_repeat(reader, event, b"number-columns-repeated")?;
    let row = odf_row_mut(tables)?;
    if row.cell.is_some() {
        return Err("OpenDocument table row has overlapping cells".to_string());
    }
    row.cell = Some(OdfCellText {
        text: String::new(),
        repeat,
    });
    if empty {
        odf_finish_cell(tables, used)?;
    }
    Ok(())
}

fn odf_finish_cell(tables: &mut [OdfTableText], used: &mut usize) -> Result<(), String> {
    let row = odf_row_mut(tables)?;
    let cell = row
        .cell
        .take()
        .ok_or("OpenDocument table cell is not open")?;
    *used -= cell.text.len();
    // Trim this cell's paragraph separator, never the preceding row's tabs.
    let text = cell.text.trim_end_matches(' ');
    if text.is_empty() {
        row.pending_empty_columns = row.pending_empty_columns.saturating_add(cell.repeat);
        return Ok(());
    }
    let separators = row
        .pending_empty_columns
        .checked_add(usize::from(row.has_content));
    let addition = text
        .len()
        .checked_mul(cell.repeat)
        .and_then(|bytes| bytes.checked_add(cell.repeat - 1))
        .and_then(|bytes| separators.and_then(|separators| bytes.checked_add(separators)));
    odf_checked_growth(*used, addition)?;
    odf_push_repeated(&mut row.text, "\t", separators.unwrap(), used)?;
    for index in 0..cell.repeat {
        if index > 0 {
            odf_push(&mut row.text, "\t", used)?;
        }
        odf_push(&mut row.text, text, used)?;
    }
    row.pending_empty_columns = 0;
    row.has_content = true;
    Ok(())
}

fn odf_finish_row(tables: &mut [OdfTableText], used: &mut usize) -> Result<(), String> {
    let table = tables
        .last_mut()
        .ok_or("OpenDocument table row has no table")?;
    let row = table
        .row
        .take()
        .ok_or("OpenDocument table row is not open")?;
    if row.cell.is_some() {
        return Err("OpenDocument table row has an unfinished cell".to_string());
    }
    *used -= row.text.len();
    if !row.has_content {
        // Empty sheet tails can represent millions of rows without allocating.
        table.pending_empty_rows = table.pending_empty_rows.saturating_add(row.repeat);
        return Ok(());
    }
    let separators = table
        .pending_empty_rows
        .checked_add(usize::from(!table.text.is_empty()));
    let addition = row
        .text
        .len()
        .checked_mul(row.repeat)
        .and_then(|bytes| bytes.checked_add(row.repeat - 1))
        .and_then(|bytes| separators.and_then(|separators| bytes.checked_add(separators)));
    odf_checked_growth(*used, addition)?;
    odf_push_repeated(&mut table.text, "\n", separators.unwrap(), used)?;
    for index in 0..row.repeat {
        if index > 0 {
            odf_push(&mut table.text, "\n", used)?;
        }
        odf_push(&mut table.text, &row.text, used)?;
    }
    table.pending_empty_rows = 0;
    Ok(())
}

fn odf_finish_table(
    output: &mut String,
    tables: &mut Vec<OdfTableText>,
    used: &mut usize,
) -> Result<(), String> {
    let table = tables.pop().ok_or("OpenDocument table is not open")?;
    if table.row.is_some() {
        return Err("OpenDocument table has an unfinished row".to_string());
    }
    *used -= table.text.len();
    if table.text.is_empty() {
        return Ok(());
    }
    let sink = odf_sink(output, tables);
    while sink.ends_with(' ') {
        sink.pop();
        *used -= 1;
    }
    if !sink.is_empty() && !sink.ends_with('\n') {
        odf_push(sink, "\n", used)?;
    }
    odf_push(sink, &table.text, used)?;
    if !sink.ends_with('\n') {
        odf_push(sink, "\n", used)?;
    }
    Ok(())
}

fn extract_paragraph_xml(xml: &str, label: &str) -> Result<String, String> {
    let mut reader = Reader::from_str(xml);
    let mut output = String::new();
    let mut tables = Vec::<OdfTableText>::new();
    let mut used = 0_usize;
    let mut paragraph_depth = 0_u32;
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) if matches!(event.local_name().as_ref(), b"p" | b"h") => {
                paragraph_depth = paragraph_depth.saturating_add(1);
            }
            Ok(Event::Start(event)) if event.local_name().as_ref() == b"table" => {
                tables.push(OdfTableText::default());
            }
            Ok(Event::Start(event)) if event.local_name().as_ref() == b"table-row" => {
                odf_begin_row(&reader, &event, &mut tables, &mut used, false)?
            }
            Ok(Event::Empty(event)) if event.local_name().as_ref() == b"table-row" => {
                odf_begin_row(&reader, &event, &mut tables, &mut used, true)?
            }
            Ok(Event::Start(event))
                if matches!(
                    event.local_name().as_ref(),
                    b"table-cell" | b"covered-table-cell"
                ) =>
            {
                odf_begin_cell(&reader, &event, &mut tables, &mut used, false)?
            }
            Ok(Event::Empty(event))
                if matches!(
                    event.local_name().as_ref(),
                    b"table-cell" | b"covered-table-cell"
                ) =>
            {
                odf_begin_cell(&reader, &event, &mut tables, &mut used, true)?
            }
            Ok(Event::Start(event) | Event::Empty(event))
                if event.local_name().as_ref() == b"s" && paragraph_depth > 0 =>
            {
                let count = odf_repeat(&reader, &event, b"c")?;
                odf_push_repeated(odf_sink(&mut output, &mut tables), " ", count, &mut used)?;
            }
            Ok(Event::Start(event) | Event::Empty(event))
                if event.local_name().as_ref() == b"tab" && paragraph_depth > 0 =>
            {
                odf_push(odf_sink(&mut output, &mut tables), "\t", &mut used)?;
            }
            Ok(Event::Start(event) | Event::Empty(event))
                if event.local_name().as_ref() == b"line-break" && paragraph_depth > 0 =>
            {
                odf_push(odf_sink(&mut output, &mut tables), "\n", &mut used)?;
            }
            Ok(Event::Text(event)) if paragraph_depth > 0 => {
                let text = event.xml10_content().map_err(|error| error.to_string())?;
                odf_push(odf_sink(&mut output, &mut tables), &text, &mut used)?;
            }
            Ok(Event::GeneralRef(event)) if paragraph_depth > 0 => {
                let mut text = String::new();
                push_reference(&mut text, &event)?;
                odf_push(odf_sink(&mut output, &mut tables), &text, &mut used)?;
            }
            Ok(Event::End(event)) if matches!(event.local_name().as_ref(), b"p" | b"h") => {
                paragraph_depth = paragraph_depth.saturating_sub(1);
                if paragraph_depth == 0 {
                    let in_cell = tables
                        .last()
                        .and_then(|table| table.row.as_ref())
                        .and_then(|row| row.cell.as_ref())
                        .is_some();
                    let sink = odf_sink(&mut output, &mut tables);
                    if in_cell {
                        if !sink.ends_with([' ', '\t', '\n']) {
                            odf_push(sink, " ", &mut used)?;
                        }
                    } else if !sink.ends_with('\n') {
                        odf_push(sink, "\n", &mut used)?;
                    }
                }
            }
            Ok(Event::End(event))
                if matches!(
                    event.local_name().as_ref(),
                    b"table-cell" | b"covered-table-cell"
                ) =>
            {
                odf_finish_cell(&mut tables, &mut used)?
            }
            Ok(Event::End(event)) if event.local_name().as_ref() == b"table-row" => {
                odf_finish_row(&mut tables, &mut used)?
            }
            Ok(Event::End(event)) if event.local_name().as_ref() == b"table" => {
                odf_finish_table(&mut output, &mut tables, &mut used)?
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(format!("Failed to parse {label}: {error}")),
            _ => {}
        }
    }
    if !tables.is_empty() {
        return Err(format!("Failed to parse {label}: unfinished table"));
    }
    if output.trim().is_empty() {
        Ok(String::new())
    } else {
        Ok(output.trim_matches('\n').to_string())
    }
}

pub(crate) fn extract_open_document_text(bytes: &[u8]) -> Result<String, String> {
    let mut zip = archive(bytes, "OpenDocument archive")?;
    let xml = read_entry(&mut zip, "content.xml")?
        .ok_or_else(|| "OpenDocument has no content.xml".to_string())?;
    let text = extract_paragraph_xml(&xml, "OpenDocument XML")?;
    if text.is_empty() {
        Err("OpenDocument contains no readable text".to_string())
    } else {
        Ok(text)
    }
}

fn epub_attribute(
    reader: &Reader<&[u8]>,
    start: &BytesStart<'_>,
    name: &[u8],
) -> Result<String, String> {
    let mut value = None;
    for attribute in start.attributes() {
        let attribute = attribute.map_err(|error| format!("Invalid EPUB attribute: {error}"))?;
        if attribute.key.as_ref() == name {
            if value.is_some() {
                return Err("EPUB metadata has duplicate attributes".into());
            }
            value = Some(
                attribute
                    .decoded_and_normalized_value(
                        quick_xml::XmlVersion::Implicit1_0,
                        reader.decoder(),
                    )
                    .map_err(|error| format!("Invalid EPUB attribute: {error}"))?
                    .into_owned(),
            );
        }
    }
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("EPUB metadata has no {}", String::from_utf8_lossy(name)))
}

fn epub_metadata_elements(
    xml: &str,
    root: &[u8],
    mut visit: impl FnMut(&Reader<&[u8]>, &BytesStart<'_>, &[Vec<u8>]) -> Result<(), String>,
) -> Result<(), String> {
    let mut reader = Reader::from_str(xml);
    let mut parents = Vec::new();
    let mut saw_root = false;
    loop {
        match reader.read_event() {
            Ok(event @ (Event::Start(_) | Event::Empty(_))) => {
                let is_start = matches!(event, Event::Start(_));
                let (Event::Start(start) | Event::Empty(start)) = event else {
                    unreachable!();
                };
                if parents.is_empty() {
                    if saw_root || start.local_name().as_ref() != root {
                        return Err("EPUB metadata has an invalid root element".into());
                    }
                    saw_root = true;
                }
                visit(&reader, &start, &parents)?;
                if is_start {
                    parents.push(start.local_name().as_ref().to_vec());
                }
            }
            Ok(Event::End(_)) => {
                parents
                    .pop()
                    .ok_or("EPUB metadata has an unexpected closing element")?;
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(format!("Failed to parse EPUB metadata: {error}")),
            _ => {}
        }
    }
    if !saw_root || !parents.is_empty() {
        return Err("EPUB metadata is incomplete".into());
    }
    Ok(())
}

/// Resolve only an archive-local path, not an OS path or a network URL.
/// Decode each URL escape once; an encoded separator cannot create a new
/// directory, while encoded Unicode/space and literal '+' keep their names.
fn epub_part_target(base_dir: &str, reference: &str) -> Result<String, String> {
    let raw_path = reference.split('#').next().unwrap_or_default();
    if raw_path.is_empty() || raw_path.contains('?') {
        return Err("Invalid EPUB resource path".into());
    }
    let mut decoded = Vec::with_capacity(raw_path.len());
    let mut bytes = raw_path.as_bytes().iter().copied();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let high = bytes.next().and_then(|byte| (byte as char).to_digit(16));
            let low = bytes.next().and_then(|byte| (byte as char).to_digit(16));
            let (Some(high), Some(low)) = (high, low) else {
                return Err("Invalid EPUB URL escape".into());
            };
            let byte = (high * 16 + low) as u8;
            if matches!(byte, b'/' | b'\\' | 0) {
                return Err("Invalid EPUB encoded path separator".into());
            }
            decoded.push(byte);
        } else {
            decoded.push(byte);
        }
    }
    let path = String::from_utf8(decoded).map_err(|_| "EPUB path is not valid UTF-8")?;
    if path.starts_with('/') || path.contains(['\\', ':']) || path.chars().any(char::is_control) {
        return Err("EPUB resource must be an archive-local relative path".into());
    }
    let mut parts: Vec<&str> = base_dir
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts
                    .pop()
                    .ok_or("EPUB resource path escapes the archive")?;
            }
            _ => parts.push(part),
        }
    }
    if parts.is_empty()
        || path.ends_with('/')
        || matches!(path.rsplit('/').next(), Some("." | ".."))
    {
        return Err("EPUB resource path does not name a file".into());
    }
    Ok(parts.join("/"))
}

fn epub_spine_entries(zip: &mut ZipArchive<Cursor<&[u8]>>) -> Result<Vec<String>, String> {
    let Some(container) = read_entry(zip, "META-INF/container.xml")? else {
        if zip
            .file_names()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("Invalid archive entry name: {error}"))?
            .into_iter()
            .any(|name| name.to_ascii_lowercase().ends_with(".opf"))
        {
            return Err("EPUB package metadata has no META-INF/container.xml".into());
        }
        // Retain the old behavior only for minimal archives without metadata.
        let mut entries: Vec<String> = zip
            .file_names()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("Invalid archive entry name: {error}"))?
            .into_iter()
            .filter(|name| {
                let lower = name.to_ascii_lowercase();
                lower.ends_with(".xhtml") || lower.ends_with(".html") || lower.ends_with(".htm")
            })
            .map(|name| name.into_owned())
            .collect();
        entries.sort();
        return Ok(entries);
    };
    let mut package_path = None;
    epub_metadata_elements(&container, b"container", |reader, start, parents| {
        if parents.len() == 2
            && parents[1] == b"rootfiles"
            && start.local_name().as_ref() == b"rootfile"
        {
            let media_type = epub_attribute(reader, start, b"media-type")?;
            let path = epub_attribute(reader, start, b"full-path")?;
            if package_path.is_none() && media_type == "application/oebps-package+xml" {
                if path.contains('#') {
                    return Err("EPUB package rootfile cannot have a fragment".into());
                }
                package_path = Some(epub_part_target("", &path)?);
            }
        }
        Ok(())
    })?;
    let package_path = package_path.ok_or("EPUB container has no package rootfile")?;
    let package = read_entry(zip, &package_path)?
        .ok_or_else(|| format!("EPUB package is missing: {package_path}"))?;
    let mut manifest = HashMap::new();
    let mut spine = Vec::new();
    let mut manifest_count = 0;
    let mut spine_count = 0;
    epub_metadata_elements(&package, b"package", |reader, start, parents| {
        let name = start.local_name();
        if parents.len() == 1 {
            if name.as_ref() == b"manifest" {
                manifest_count += 1;
            } else if name.as_ref() == b"spine" {
                spine_count += 1;
            }
        } else if parents.len() == 2 && parents[1] == b"manifest" && name.as_ref() == b"item" {
            let id = epub_attribute(reader, start, b"id")?;
            let href = epub_attribute(reader, start, b"href")?;
            let media_type = epub_attribute(reader, start, b"media-type")?;
            let fallback = attribute(reader, start, b"fallback");
            if manifest.insert(id, (href, media_type, fallback)).is_some() {
                return Err("EPUB manifest has duplicate item IDs".into());
            }
        } else if parents.len() == 2 && parents[1] == b"spine" && name.as_ref() == b"itemref" {
            spine.push(epub_attribute(reader, start, b"idref")?);
        }
        Ok(())
    })?;
    if manifest_count != 1 || spine_count != 1 || spine.is_empty() {
        return Err("EPUB package has no unique manifest and non-empty spine".into());
    }
    let base_dir = package_path.rsplit_once('/').map_or("", |(dir, _)| dir);
    let mut seen = std::collections::HashSet::new();
    let mut entries = Vec::with_capacity(spine.len());
    for id in spine {
        if !seen.insert(id.clone()) {
            return Err("EPUB spine has duplicate item references".into());
        }
        let mut current_id = id.as_str();
        let mut fallback_chain = std::collections::HashSet::new();
        loop {
            if !fallback_chain.insert(current_id) {
                return Err("EPUB spine has a cyclic content fallback".into());
            }
            let (href, media_type, fallback) = manifest.get(current_id).ok_or_else(|| {
                format!("EPUB spine references a missing manifest ID: {current_id}")
            })?;
            let path = epub_part_target(base_dir, href)?;
            // Even a foreign resource with a readable fallback must identify
            // an actual local entry; never hide missing/external spine data.
            zip.by_name(&path).map_err(|error| {
                format!("EPUB spine resource is missing or unreadable: {path}: {error}")
            })?;
            if matches!(
                media_type.as_str(),
                "application/xhtml+xml" | "text/html" | "image/svg+xml"
            ) {
                entries.push(path);
                break;
            }
            current_id = fallback
                .as_deref()
                .ok_or_else(|| format!("Unsupported EPUB spine content type: {media_type}"))?;
        }
    }
    Ok(entries)
}

pub(crate) fn extract_epub_text(bytes: &[u8]) -> Result<String, String> {
    let mut zip = archive(bytes, "EPUB archive")?;
    let entries = epub_spine_entries(&mut zip)?;
    if entries.is_empty() {
        return Err("EPUB contains no HTML content".to_string());
    }
    let mut output = String::new();
    for name in entries {
        let xml = read_entry(&mut zip, &name)?
            .ok_or_else(|| format!("EPUB spine content is missing: {name}"))?;
        let chapter = extract_paragraph_xml(&xml, &name)?;
        if chapter.is_empty() {
            continue;
        }
        if !output.is_empty() {
            bounded_push(&mut output, "\n\n")?;
        }
        bounded_push(&mut output, &format!("--- {name} ---\n"))?;
        bounded_push(&mut output, &chapter)?;
    }
    if output.is_empty() {
        Err("EPUB contains no readable text".to_string())
    } else {
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    fn build_zip(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for (name, content) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(content.as_bytes()).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn extracts_docx_paragraphs_tabs_and_breaks() {
        let bytes = build_zip(&[(
            "word/document.xml",
            r#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:t>Hello</w:t></w:r><w:tab/><w:r><w:t>world</w:t></w:r></w:p><w:p><w:r><w:t>Next</w:t></w:r><w:br/><w:r><w:t>line</w:t></w:r></w:p></w:body></w:document>"#,
        )]);
        assert_eq!(
            extract_docx_text(&bytes).unwrap(),
            "Hello\tworld\nNext\nline"
        );
    }

    #[test]
    fn extracts_xlsx_shared_and_numeric_cells() {
        let bytes = build_zip(&[
            (
                "xl/sharedStrings.xml",
                r#"<sst><si><t>Name</t></si><si><t>Alice</t></si></sst>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row><c t="s"><v>0</v></c><c><v>42</v></c></row><row><c t="s"><v>1</v></c></row></sheetData></worksheet>"#,
            ),
        ]);
        assert_eq!(
            extract_xlsx_text(&bytes).unwrap(),
            "--- Sheet 1 ---\nName\t42\nAlice"
        );
    }

    #[test]
    fn xlsx_coordinates_are_bounded_before_sparse_padding() {
        assert_eq!(xlsx_column_index("XFD1048576"), Some(16_383));
        for invalid in ["XFE1", "ZZZZZZZZZZ1", "A1048577", "A0", "A", "1"] {
            assert_eq!(xlsx_column_index(invalid), None);
        }
        assert!(extract_sheet(r#"<worksheet><sheetData><row><c r="ZZZZZZZZZZ1"><v>1</v></c></row></sheetData></worksheet>"#, &[]).is_err());
    }

    #[test]
    fn xlsx_checks_shared_string_expansion_before_cloning_the_row() {
        let mut row = Vec::new();
        let mut bytes = 0;
        push_xlsx_cell(&mut row, &mut bytes, Some(0), "existing", 16).unwrap();
        let oversized = "x".repeat(1024);
        assert!(push_xlsx_cell(&mut row, &mut bytes, Some(1), &oversized, 16).is_err());
        assert_eq!(row, ["existing"]);
        assert_eq!(bytes, 8);
        let shared = vec!["x".repeat(MAX_EXTRACTED_TEXT_BYTES / 2)];
        let xml = r#"<worksheet><sheetData><row><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>0</v></c></row></sheetData></worksheet>"#;
        assert!(extract_sheet(xml, &shared).is_err());
    }

    #[test]
    fn xlsx_cells_retain_their_columns_when_xml_cells_are_out_of_order() {
        let xml = r#"<worksheet><sheetData><row><c r="C1"><v>3</v></c><c r="A1"><v>1</v></c><c r="B1"><v>2</v></c></row></sheetData></worksheet>"#;
        assert_eq!(extract_sheet(xml, &[]).unwrap(), "1\t2\t3");
        let sparse =
            r#"<worksheet><sheetData><row><c r="C1"><v>3</v></c></row></sheetData></worksheet>"#;
        assert_eq!(extract_sheet(sparse, &[]).unwrap(), "\t\t3");
    }

    #[test]
    fn xlsx_uses_workbook_names_and_tab_order_with_internal_relationships() {
        let bytes = build_zip(&[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="二季度 &amp; 收入" sheetId="2" r:id="rId2"/><sheet name="一季度收入" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Target="worksheets/sheet1.xml" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"/><Relationship Id="rId2" Target="/xl/worksheets/reordered.xml" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row><c r="A1"><v>100</v></c></row></sheetData></worksheet>"#,
            ),
            (
                "xl/worksheets/reordered.xml",
                r#"<worksheet><sheetData><row><c r="A1"><v>200</v></c></row></sheetData></worksheet>"#,
            ),
        ]);
        let text = extract_xlsx_text(&bytes).unwrap();
        assert!(text.starts_with("--- 二季度 & 收入 ---\n200"));
        assert!(text.contains("--- 一季度收入 ---\n100"));
        assert!(text.find("二季度").unwrap() < text.find("一季度").unwrap());
    }

    #[test]
    fn xlsx_rejects_missing_or_external_worksheet_relationships() {
        for relationships in [
            r#"<Relationships/>"#,
            r#"<Relationships><Relationship Id="rId1" Target="https://external.invalid/sheet.xml" TargetMode="External"/></Relationships>"#,
        ] {
            let bytes = build_zip(&[
                (
                    "xl/workbook.xml",
                    r#"<workbook xmlns:r="r"><sheets><sheet name="收入" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
                ),
                ("xl/_rels/workbook.xml.rels", relationships),
            ]);
            assert!(extract_xlsx_text(&bytes).is_err());
        }
    }

    #[test]
    fn preserves_sparse_xlsx_columns() {
        let bytes = build_zip(&[(
            "xl/worksheets/sheet1.xml",
            r#"<worksheet><sheetData><row><c r="A1"><v>left</v></c><c r="C1"><v>right</v></c></row></sheetData></worksheet>"#,
        )]);
        assert_eq!(
            extract_xlsx_text(&bytes).unwrap(),
            "--- Sheet 1 ---\nleft\t\tright"
        );
    }

    #[test]
    fn extracts_open_document_and_epub_paragraphs() {
        let odt = build_zip(&[(
            "content.xml",
            r#"<office:document xmlns:office="o" xmlns:text="t"><text:p>Hello <text:span>ODT</text:span></text:p></office:document>"#,
        )]);
        assert_eq!(extract_open_document_text(&odt).unwrap(), "Hello ODT");
        let epub = build_zip(&[(
            "OEBPS/chapter1.xhtml",
            "<html><body><p>Hello EPUB</p></body></html>",
        )]);
        assert!(extract_epub_text(&epub).unwrap().contains("Hello EPUB"));
    }

    const EPUB_CONTAINER: &str = r#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OPS/book.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#;

    #[test]
    fn epub_reads_spine_order_without_non_spine_navigation() {
        let bytes = build_zip(&[
            ("META-INF/container.xml", EPUB_CONTAINER),
            (
                "OPS/book.opf",
                r#"<package xmlns="http://www.idpf.org/2007/opf"><metadata><other:item xmlns:other="urn:extra" id="first" href="missing.xhtml" media-type="application/xhtml+xml"/></metadata><manifest><item id="second" href="a-second.xhtml" media-type="application/xhtml+xml"/><item id="nav" href="000-nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="audio" href="https://external.invalid/audio.mp3" media-type="audio/mpeg"/><item id="first" href="z-first.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="first"/><itemref idref="second"/></spine></package>"#,
            ),
            (
                "OPS/a-second.xhtml",
                "<html><body><p>Chapter Two</p></body></html>",
            ),
            (
                "OPS/000-nav.xhtml",
                "<html><body><p>Navigation Only</p></body></html>",
            ),
            (
                "OPS/z-first.xhtml",
                "<html><body><p>Chapter One</p></body></html>",
            ),
            (
                "OPS/unlisted.xhtml",
                "<html><body><p>Unlisted Content</p></body></html>",
            ),
        ]);
        assert_eq!(
            extract_epub_text(&bytes).unwrap(),
            "--- OPS/z-first.xhtml ---\nChapter One\n\n--- OPS/a-second.xhtml ---\nChapter Two"
        );
    }

    #[test]
    fn epub_resolves_encoded_unicode_spaces_and_package_relative_paths() {
        let bytes = build_zip(&[
            (
                "META-INF/container.xml",
                r#"<container><rootfiles><rootfile full-path="%E5%87%BA%E7%89%88%E7%89%A9/metadata/book%20package.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
            ),
            (
                "出版物/metadata/book package.opf",
                r#"<package><manifest><item id="chapter" href="../%E7%AB%A0%E8%8A%82%20%E4%B8%80+#start" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chapter"/></spine></package>"#,
            ),
            (
                "出版物/章节 一+",
                "<html><body><p>中文章节 &amp; 正文</p></body></html>",
            ),
        ]);
        assert_eq!(
            extract_epub_text(&bytes).unwrap(),
            "--- 出版物/章节 一+ ---\n中文章节 & 正文"
        );
    }

    #[test]
    fn epub_keeps_local_content_fallbacks_and_svg_covers_in_spine_order() {
        let bytes = build_zip(&[
            ("META-INF/container.xml", EPUB_CONTAINER),
            (
                "OPS/book.opf",
                r#"<package><manifest><item id="svg" href="cover.svg" media-type="image/svg+xml"/><item id="image" href="cover.png" media-type="image/png" fallback="html"/><item id="html" href="title.xhtml" media-type="application/xhtml+xml"/><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="svg"/><itemref idref="image"/><itemref idref="chapter"/></spine></package>"#,
            ),
            (
                "OPS/cover.svg",
                "<svg><rect width=\"100\" height=\"100\"/></svg>",
            ),
            ("OPS/cover.png", "binary content is not parsed as XML"),
            ("OPS/title.xhtml", "<html><p>Title Fallback</p></html>"),
            ("OPS/chapter.xhtml", "<html><p>Main Chapter</p></html>"),
        ]);
        assert_eq!(
            extract_epub_text(&bytes).unwrap(),
            "--- OPS/title.xhtml ---\nTitle Fallback\n\n--- OPS/chapter.xhtml ---\nMain Chapter"
        );
    }

    #[test]
    fn epub_rejects_missing_duplicate_and_unresolved_spine_metadata() {
        for package in [
            r#"<package><manifest/><spine><itemref idref="unknown"/></spine></package>"#,
            r#"<package><manifest/><spine><itemref/></spine></package>"#,
            r#"<package><manifest><item id="chapter" href="missing.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chapter"/></spine></package>"#,
            r#"<package><manifest><item id="chapter" href="fallback.xhtml" media-type="application/xhtml+xml"/><item id="chapter" href="fallback.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chapter"/></spine></package>"#,
            r#"<package><manifest><item id="chapter" href="fallback.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chapter"/><itemref idref="chapter"/></spine></package>"#,
            r#"<package><manifest/><spine/></package>"#,
            r#"<package><manifest/><spine></package>"#,
            r#"<package><manifest><item id="chapter" href="fallback.xhtml" media-type="image/png" fallback="missing"/></manifest><spine><itemref idref="chapter"/></spine></package>"#,
            r#"<package><manifest><item id="chapter" href="fallback.xhtml" media-type="image/png" fallback="chapter"/></manifest><spine><itemref idref="chapter"/></spine></package>"#,
        ] {
            let bytes = build_zip(&[
                ("META-INF/container.xml", EPUB_CONTAINER),
                ("OPS/book.opf", package),
                (
                    "OPS/fallback.xhtml",
                    "<html><p>Must not hide broken metadata</p></html>",
                ),
            ]);
            assert!(extract_epub_text(&bytes).is_err(), "package={package}");
        }
    }

    #[test]
    fn epub_rejects_external_and_invalid_spine_paths() {
        for href in [
            "https://external.invalid/chapter.xhtml",
            "file:///etc/passwd",
            "data:text/html,not-a-chapter",
            "//external.invalid/chapter.xhtml",
            "/outside.xhtml",
            "../../outside.xhtml",
            "%2e%2e/%2e%2e/outside.xhtml",
            "dir%2Fchapter.xhtml",
            "dir%5Cchapter.xhtml",
            "chapter%00.xhtml",
            "chapter%GG.xhtml",
            "%FF.xhtml",
            "chapter.xhtml?remote=true",
            "..",
        ] {
            let package = format!(
                r#"<package><manifest><item id="chapter" href="{href}" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chapter"/></spine></package>"#
            );
            let bytes = build_zip(&[
                ("META-INF/container.xml", EPUB_CONTAINER),
                ("OPS/book.opf", &package),
                (
                    "OPS/fallback.xhtml",
                    "<html><p>Must not hide an invalid path</p></html>",
                ),
            ]);
            assert!(extract_epub_text(&bytes).is_err(), "href={href}");
        }
    }

    #[test]
    fn epub_rejects_missing_or_invalid_package_roots_without_fallback() {
        for container in [
            "<container><rootfiles/></container>",
            r#"<container><rootfiles><rootfile full-path="missing.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
            r#"<container><rootfiles><rootfile full-path="../book.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
            r#"<container><rootfiles><rootfile full-path="https://external.invalid/book.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
            r#"<container><rootfiles><rootfile full-path="book.opf#fragment" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
        ] {
            let bytes = build_zip(&[
                ("META-INF/container.xml", container),
                (
                    "OPS/fallback.xhtml",
                    "<html><p>Must not hide missing package</p></html>",
                ),
            ]);
            assert!(extract_epub_text(&bytes).is_err(), "container={container}");
        }
        let bytes = build_zip(&[
            ("OPS/book.opf", "<package/>"),
            (
                "OPS/fallback.xhtml",
                "<html><p>Missing container is not legacy</p></html>",
            ),
        ]);
        assert!(extract_epub_text(&bytes).is_err());
    }

    #[test]
    fn preserves_open_document_table_rows_columns_and_spaces() {
        let ods = build_zip(&[(
            "content.xml",
            r#"<office:document xmlns:office="o" xmlns:text="t" xmlns:table="tb"><table:table><table:table-row><table:table-cell><text:p>First<text:s text:c="2"/>Name</text:p></table:table-cell><table:table-cell><text:p>Score</text:p></table:table-cell></table:table-row><table:table-row><table:table-cell><text:p>Alice</text:p></table:table-cell><table:table-cell><text:p>42</text:p></table:table-cell></table:table-row></table:table></office:document>"#,
        )]);
        assert_eq!(
            extract_open_document_text(&ods).unwrap(),
            "First  Name\tScore\nAlice\t42"
        );
    }
}

#[cfg(test)]
mod odf_table_tests {
    use std::io::Write as _;

    use super::*;

    fn ods(content: &str) -> Vec<u8> {
        let xml = format!(
            r#"<office:document xmlns:office="o" xmlns:text="t" xmlns:table="tb"><office:body>{content}</office:body></office:document>"#
        );
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file("content.xml", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(xml.as_bytes()).unwrap();
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn preserves_empty_odf_columns_between_values_and_before_the_first_value() {
        let bytes = ods(
            r#"<table:table><table:table-row><table:table-cell/><table:table-cell><text:p>A</text:p></table:table-cell><table:table-cell/><table:table-cell><text:p/></table:table-cell><table:covered-table-cell/><table:table-cell><text:p>B</text:p></table:table-cell></table:table-row></table:table>"#,
        );
        assert_eq!(extract_open_document_text(&bytes).unwrap(), "\tA\t\t\t\tB");
    }

    #[test]
    fn preserves_repeated_nonempty_odf_columns_and_rows_in_document_order() {
        let bytes = ods(
            r#"<table:table><table:table-row table:number-rows-repeated="2"><table:table-cell table:number-columns-repeated="3"><text:p>A <text:span>span</text:span></text:p></table:table-cell><table:table-cell><text:p>B</text:p></table:table-cell></table:table-row></table:table>"#,
        );
        assert_eq!(
            extract_open_document_text(&bytes).unwrap(),
            "A span\tA span\tA span\tB\nA span\tA span\tA span\tB"
        );
    }

    #[test]
    fn preserves_repeated_empty_odf_rows_and_columns_when_followed_by_content() {
        let bytes = ods(
            r#"<table:table><table:table-row><table:table-cell><text:p>A</text:p></table:table-cell><table:table-cell table:number-columns-repeated="2"/><table:table-cell><text:p>B</text:p></table:table-cell></table:table-row><table:table-row table:number-rows-repeated="2"><table:table-cell/></table:table-row><table:table-row><table:table-cell><text:p>C</text:p></table:table-cell></table:table-row></table:table>"#,
        );
        assert_eq!(
            extract_open_document_text(&bytes).unwrap(),
            "A\t\t\tB\n\n\nC"
        );
    }

    #[test]
    fn rejects_odf_nonempty_repeat_expansion_before_allocating_past_the_budget() {
        for (row_repeat, column_repeat) in [
            (1, MAX_EXTRACTED_TEXT_BYTES),
            (MAX_EXTRACTED_TEXT_BYTES, 1),
            (1, usize::MAX),
            (usize::MAX, 1),
        ] {
            let content = format!(
                r#"<table:table><table:table-row table:number-rows-repeated="{row_repeat}"><table:table-cell table:number-columns-repeated="{column_repeat}"><text:p>AB</text:p></table:table-cell></table:table-row></table:table>"#
            );
            assert!(
                extract_open_document_text(&ods(&content))
                    .unwrap_err()
                    .contains("16MB")
            );
        }
    }

    #[test]
    fn rejects_huge_odf_empty_repeats_only_when_later_content_requires_expansion() {
        for content in [
            format!(
                r#"<table:table><table:table-row><table:table-cell><text:p>A</text:p></table:table-cell><table:table-cell table:number-columns-repeated="{}"/><table:table-cell><text:p>B</text:p></table:table-cell></table:table-row></table:table>"#,
                usize::MAX
            ),
            format!(
                r#"<table:table><table:table-row><table:table-cell><text:p>A</text:p></table:table-cell></table:table-row><table:table-row table:number-rows-repeated="{}"/><table:table-row><table:table-cell><text:p>B</text:p></table:table-cell></table:table-row></table:table>"#,
                usize::MAX
            ),
        ] {
            assert!(
                extract_open_document_text(&ods(&content))
                    .unwrap_err()
                    .contains("16MB")
            );
        }
    }

    #[test]
    fn collapses_large_empty_odf_sheet_tails_without_expanding_them() {
        let bytes = ods(
            r#"<table:table><table:table-row><table:table-cell><text:p>Keep</text:p></table:table-cell><table:table-cell table:number-columns-repeated="16383"/></table:table-row><table:table-row table:number-rows-repeated="1048575"><table:table-cell table:number-columns-repeated="16384"/></table:table-row></table:table>"#,
        );
        assert_eq!(extract_open_document_text(&bytes).unwrap(), "Keep");
    }

    #[test]
    fn nested_odf_tables_preserve_paragraph_span_and_explicit_space_content() {
        let bytes = ods(
            r#"<text:p>Outside <text:span>span</text:span></text:p><table:table><table:table-row><table:table-cell><text:p>Before <text:span>span</text:span></text:p><table:table><table:table-row><table:table-cell><text:p>Inner<text:s text:c="2"/>value</text:p></table:table-cell></table:table-row></table:table><text:p>After</text:p></table:table-cell><table:table-cell><text:p>Right</text:p></table:table-cell></table:table-row></table:table><text:p>End</text:p>"#,
        );
        assert_eq!(
            extract_open_document_text(&bytes).unwrap(),
            "Outside span\nBefore span\nInner  value\nAfter\tRight\nEnd"
        );
    }

    #[test]
    fn rejects_invalid_odf_repeat_counts_instead_of_silently_using_one() {
        for count in [
            "0",
            "-1",
            "many",
            "&unknown;",
            "999999999999999999999999999999999999",
        ] {
            let content = format!(
                r#"<table:table><table:table-row><table:table-cell table:number-columns-repeated="{count}"><text:p>A</text:p></table:table-cell></table:table-row></table:table>"#
            );
            assert!(
                extract_open_document_text(&ods(&content))
                    .unwrap_err()
                    .contains("Invalid OpenDocument")
            );
        }
    }
}
