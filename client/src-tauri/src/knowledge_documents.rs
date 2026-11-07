//! Bounded Office XML to readable, source-attributed Markdown. No model calls.
use quick_xml::{
    Reader,
    events::{BytesStart, Event},
};
use std::collections::{BTreeMap, HashMap};

fn attribute(event: &BytesStart<'_>, name: &[u8], reader: &Reader<&[u8]>) -> Option<String> {
    event
        .attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == name)
        .and_then(|a| {
            a.decoded_and_normalized_value(quick_xml::XmlVersion::Implicit1_0, reader.decoder())
                .ok()
                .map(|s| s.into_owned())
        })
}
fn text(value: &quick_xml::events::BytesText<'_>) -> Result<String, String> {
    let decoded = value.decode().map_err(|e| e.to_string())?;
    quick_xml::escape::unescape(&decoded)
        .map(|s| s.into_owned())
        .map_err(|e| e.to_string())
}
fn cell(value: &str) -> String {
    value.trim().replace('|', "\\|").replace(['\n', '\r'], " ")
}
fn row(output: &mut Vec<String>, cells: &[String], header: bool) {
    output.push(format!(
        "| {} |",
        cells
            .iter()
            .map(|s| cell(s))
            .collect::<Vec<_>>()
            .join(" | ")
    ));
    if header {
        output.push(format!("| {} |", vec!["---"; cells.len()].join(" | ")));
    }
}
fn predefined(reference: &quick_xml::events::BytesRef<'_>) -> String {
    match reference.decode().ok().as_deref() {
        Some("amp") => "&".into(),
        Some("lt") => "<".into(),
        Some("gt") => ">".into(),
        Some("quot") => "\"".into(),
        Some("apos") => "'".into(),
        Some(code) if code.starts_with("#x") => u32::from_str_radix(&code[2..], 16)
            .ok()
            .and_then(char::from_u32)
            .map(String::from)
            .unwrap_or_default(),
        Some(code) if code.starts_with('#') => code[1..]
            .parse::<u32>()
            .ok()
            .and_then(char::from_u32)
            .map(String::from)
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Shared paragraph/table structure used by Word and PowerPoint.
pub fn paragraphs(xml: &str) -> Result<String, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut stack = Vec::<Vec<u8>>::new();
    let mut output = Vec::new();
    let mut paragraph = String::new();
    let mut heading = 0usize;
    let mut in_cell = false;
    let mut cell_text = String::new();
    let mut cells = Vec::new();
    let mut rows = 0usize;
    let mut table = Vec::new();
    loop {
        match reader
            .read_event()
            .map_err(|e| format!("文档 XML 无效：{e}"))?
        {
            Event::Start(event) => {
                let name = event.local_name().as_ref().to_vec();
                match name.as_slice() {
                    b"p" => {
                        paragraph.clear();
                        heading = 0;
                    }
                    b"tc" => {
                        in_cell = true;
                        cell_text.clear();
                    }
                    b"tr" => cells.clear(),
                    b"tbl" => {
                        rows = 0;
                        table.clear();
                    }
                    _ => {}
                }
                stack.push(name);
            }
            Event::Empty(event) => match event.local_name().as_ref() {
                b"pStyle" => {
                    if let Some(style) = attribute(&event, b"val", &reader) {
                        if style.to_lowercase().contains("heading") || style.contains("标题") {
                            heading = style
                                .chars()
                                .find_map(|c| c.to_digit(10))
                                .unwrap_or(1)
                                .clamp(1, 6) as usize;
                        }
                    }
                }
                b"tab" => paragraph.push('\t'),
                b"br" | b"cr" => paragraph.push('\n'),
                _ => {}
            },
            Event::Text(value) if stack.last().is_some_and(|name| name == b"t") => {
                paragraph.push_str(&text(&value)?)
            }
            Event::GeneralRef(value) if stack.last().is_some_and(|name| name == b"t") => {
                paragraph.push_str(&predefined(&value))
            }
            Event::End(event) => {
                match event.local_name().as_ref() {
                    b"p" => {
                        if !paragraph.trim().is_empty() {
                            if in_cell {
                                if !cell_text.is_empty() {
                                    cell_text.push_str("; ");
                                }
                                cell_text.push_str(paragraph.trim());
                            } else if heading > 0 {
                                output.push(format!(
                                    "{} {}",
                                    "#".repeat(heading),
                                    paragraph.trim()
                                ));
                            } else {
                                output.push(paragraph.trim().to_owned());
                            }
                        }
                    }
                    b"tc" => {
                        in_cell = false;
                        cells.push(cell_text.clone());
                    }
                    b"tr" => {
                        if !cells.is_empty() {
                            row(&mut table, &cells, rows == 0);
                            rows += 1;
                        }
                    }
                    b"tbl" if !table.is_empty() => output.push(table.join("\n")),
                    _ => {}
                }
                stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(output.join("\n\n"))
}

fn shared_strings(xml: &str) -> Result<Vec<String>, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut strings = Vec::new();
    let mut current = String::new();
    let mut in_text = false;
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Start(event) => match event.local_name().as_ref() {
                b"si" => current.clear(),
                b"t" => in_text = true,
                _ => {}
            },
            Event::Text(value) if in_text => current.push_str(&text(&value)?),
            Event::GeneralRef(value) if in_text => current.push_str(&predefined(&value)),
            Event::End(event) => match event.local_name().as_ref() {
                b"si" => strings.push(current.clone()),
                b"t" => in_text = false,
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(strings)
}

fn column(reference: &str) -> Result<usize, String> {
    let letters = reference
        .bytes()
        .take_while(u8::is_ascii_alphabetic)
        .count();
    let position = reference.as_bytes()[..letters]
        .iter()
        .try_fold(0usize, |n, c| {
            n.checked_mul(26)?
                .checked_add((c.to_ascii_uppercase() - b'A' + 1) as usize)
        });
    let row = reference[letters..].parse::<usize>().ok();
    if letters == 0 || !row.is_some_and(|row| (1..=1_048_576).contains(&row)) {
        return Err(format!("Excel 单元格地址无效：{reference}"));
    }
    position
        .filter(|n| (1..=16_384).contains(n))
        .map(|n| n - 1)
        .ok_or_else(|| format!("Excel 单元格列超出范围：{reference}"))
}

fn column_name(mut position: usize) -> String {
    let mut name = Vec::new();
    position += 1;
    while position > 0 {
        position -= 1;
        name.push((b'A' + (position % 26) as u8) as char);
        position /= 26;
    }
    name.into_iter().rev().collect()
}

fn sheet(xml: &str, shared: &[String]) -> Result<String, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut rows = Vec::new();
    let mut cells = BTreeMap::<usize, String>::new();
    let mut value = String::new();
    let mut kind = String::new();
    let mut position = 0;
    let mut reading = false;
    let mut row_number = 0usize;
    let mut next_column = 0usize;
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Start(event) => match event.local_name().as_ref() {
                b"row" => {
                    cells.clear();
                    next_column = 0;
                    row_number = attribute(&event, b"r", &reader)
                        .map(|r| r.parse::<usize>().map_err(|_| "Excel 行号无效"))
                        .transpose()?
                        .unwrap_or(row_number + 1);
                    if !(1..=1_048_576).contains(&row_number) {
                        return Err("Excel 行号超出范围".into());
                    }
                }
                b"c" => {
                    value.clear();
                    kind = attribute(&event, b"t", &reader).unwrap_or_default();
                    position = attribute(&event, b"r", &reader)
                        .map(|r| column(&r))
                        .transpose()?
                        .unwrap_or(next_column);
                    if position >= 16_384 {
                        return Err("Excel 单元格列超出范围".into());
                    }
                    next_column = position + 1;
                }
                b"v" | b"t" => reading = true,
                _ => {}
            },
            Event::Text(content) if reading => value.push_str(&text(&content)?),
            Event::GeneralRef(content) if reading => value.push_str(&predefined(&content)),
            Event::End(event) => match event.local_name().as_ref() {
                b"v" | b"t" => reading = false,
                b"c" => {
                    let displayed = if kind == "s" {
                        let index = value
                            .trim()
                            .parse::<usize>()
                            .map_err(|_| "Excel 共享字符串索引无效")?;
                        shared.get(index).ok_or("Excel 共享字符串缺失")?.clone()
                    } else if kind == "b" {
                        if value == "1" {
                            "是".into()
                        } else {
                            "否".into()
                        }
                    } else {
                        value.clone()
                    };
                    if !displayed.trim().is_empty() {
                        if cells.insert(position, displayed).is_some() {
                            return Err("Excel 同一行存在重复单元格地址".into());
                        }
                    }
                }
                b"row" => {
                    if !cells.is_empty() {
                        rows.push((row_number, std::mem::take(&mut cells)));
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    let width = rows
        .iter()
        .filter_map(|(_, cells)| cells.last_key_value().map(|(c, _)| c + 1))
        .max()
        .unwrap_or(0);
    let mut output = String::new();
    for (index, (number, cells)) in rows.into_iter().enumerate() {
        let text = if width <= 128 {
            let mut values = vec![String::new(); width];
            for (position, value) in cells {
                values[position] = value;
            }
            let mut lines = Vec::new();
            row(&mut lines, &values, index == 0);
            lines.join("\n")
        } else {
            // Sparse coordinates keep wide sheets faithful without expanding
            // thousands of empty columns per row or overwriting later values.
            let values = cells
                .into_iter()
                .map(|(position, value)| {
                    format!("- {}{number}：{}", column_name(position), cell(&value))
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!("### 第 {number} 行\n\n{values}")
        };
        if output.len().saturating_add(text.len()).saturating_add(2) > 8 * 1024 * 1024 {
            return Err("Excel 展开内容超过 8MB，请缩小知识源文件范围".into());
        }
        if !output.is_empty() {
            output.push_str(if width <= 128 { "\n" } else { "\n\n" });
        }
        output.push_str(&text);
    }
    Ok(output)
}

fn number(name: &str) -> usize {
    name.rsplit('/')
        .next()
        .unwrap_or(name)
        .chars()
        .filter(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap_or(0)
}

fn relationship_id(event: &BytesStart<'_>, reader: &Reader<&[u8]>) -> Option<String> {
    // A slide has both numeric `id` and relationship `r:id`; never confuse them.
    event
        .attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == b"id" && a.key.as_ref() != b"id")
        .and_then(|a| {
            a.decoded_and_normalized_value(quick_xml::XmlVersion::Implicit1_0, reader.decoder())
                .ok()
                .map(|s| s.into_owned())
        })
}

fn relationship_targets(xml: &str, base: &str) -> Result<HashMap<String, String>, String> {
    let mut reader = Reader::from_str(xml);
    let mut targets = HashMap::new();
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Empty(event) | Event::Start(event)
                if event.local_name().as_ref() == b"Relationship" =>
            {
                if attribute(&event, b"TargetMode", &reader).as_deref() == Some("External") {
                    continue;
                }
                if let (Some(id), Some(target)) = (
                    attribute(&event, b"Id", &reader),
                    attribute(&event, b"Target", &reader),
                ) {
                    let mut segments = if target.starts_with('/') {
                        Vec::new()
                    } else {
                        vec![base]
                    };
                    for segment in target.split('/') {
                        match segment {
                            "" | "." => {}
                            ".." => {
                                if segments.pop().is_none() {
                                    return Err("Office 文档关系路径超出范围".into());
                                }
                            }
                            _ => segments.push(segment),
                        }
                    }
                    if targets.insert(id, segments.join("/")).is_some() {
                        return Err("Office 文档关系标识重复".into());
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(targets)
}

fn presentation(parts: &[(String, String)]) -> Result<String, String> {
    let find = |name: &str| {
        parts
            .iter()
            .find(|(path, _)| path == name)
            .map(|(_, xml)| xml.as_str())
    };
    let xml = find("ppt/presentation.xml").ok_or("PPT 幻灯片顺序信息缺失，无法确认引用页码")?;
    let rels = find("ppt/_rels/presentation.xml.rels").ok_or("PPT 幻灯片关系信息缺失")?;
    let targets = relationship_targets(rels, "ppt")?;
    let mut reader = Reader::from_str(xml);
    let mut slides = Vec::new();
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Empty(event) | Event::Start(event)
                if event.local_name().as_ref() == b"sldId" =>
            {
                let id = relationship_id(&event, &reader).ok_or("PPT 幻灯片关系标识缺失")?;
                let path = targets.get(&id).ok_or("PPT 幻灯片关系无法解析")?;
                if !path.starts_with("ppt/slides/") {
                    return Err("PPT 幻灯片路径无效".into());
                }
                let content = find(path).ok_or("PPT 幻灯片内容缺失，无法生成完整索引")?;
                slides.push(format!(
                    "## 第 {} 页\n\n{}",
                    slides.len() + 1,
                    paragraphs(content)?
                ));
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(slides.join("\n\n"))
}

pub fn extract(extension: &str, mut parts: Vec<(String, String)>) -> Result<String, String> {
    parts.sort_by_key(|(name, _)| number(name));
    match extension {
        "docx" => parts
            .iter()
            .find(|(name, _)| name == "word/document.xml")
            .map(|(_, xml)| paragraphs(xml))
            .unwrap_or_else(|| Err("Word 正文缺失".into())),
        "pptx" => presentation(&parts),
        "xlsx" => {
            let shared = parts
                .iter()
                .find(|(name, _)| name == "xl/sharedStrings.xml")
                .map(|(_, xml)| shared_strings(xml))
                .transpose()?
                .unwrap_or_default();
            let relationships = parts
                .iter()
                .find(|(name, _)| name == "xl/_rels/workbook.xml.rels")
                .map(|(_, xml)| relationship_targets(xml, "xl"))
                .transpose()?
                .unwrap_or_default();
            let mut sheets = Vec::new();
            if let Some((_, xml)) = parts.iter().find(|(name, _)| name == "xl/workbook.xml") {
                let mut reader = Reader::from_str(xml);
                loop {
                    match reader.read_event().map_err(|e| e.to_string())? {
                        Event::Empty(event) | Event::Start(event)
                            if event.local_name().as_ref() == b"sheet" =>
                        {
                            if let (Some(name), Some(id)) = (
                                attribute(&event, b"name", &reader),
                                relationship_id(&event, &reader),
                            ) {
                                if let Some(path) = relationships.get(&id) {
                                    sheets.push((name, path.clone()));
                                }
                            }
                        }
                        Event::Eof => break,
                        _ => {}
                    }
                }
            }
            if sheets.is_empty() {
                sheets = parts
                    .iter()
                    .filter(|(name, _)| name.starts_with("xl/worksheets/sheet"))
                    .map(|(name, _)| (name.clone(), name.clone()))
                    .collect();
            }
            sheets
                .into_iter()
                .map(|(name, path)| {
                    let (_, xml) = parts
                        .iter()
                        .find(|(part, _)| *part == path)
                        .ok_or_else(|| format!("Excel 工作表缺失：{name}"))?;
                    sheet(xml, &shared).map(|text| format!("## 工作表：{name}\n\n{text}"))
                })
                .collect::<Result<Vec<_>, _>>()
                .map(|texts| texts.join("\n\n"))
        }
        _ => Err("不支持的文档格式".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reordered_slides() -> Vec<(String, String)> {
        vec![
            ("ppt/presentation.xml".into(), r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="r2"/><p:sldId id="257" r:id="r1"/></p:sldIdLst></p:presentation>"#.into()),
            ("ppt/_rels/presentation.xml.rels".into(), r#"<Relationships><Relationship Id="r1" Target="slides/slide1.xml"/><Relationship Id="r2" Target="/ppt/slides/slide2.xml"/></Relationships>"#.into()),
            ("ppt/slides/slide1.xml".into(), "<p><t>实际第二页</t></p>".into()),
            ("ppt/slides/slide2.xml".into(), "<p><t>实际第一页</t></p>".into()),
            ("ppt/slides/slide3.xml".into(), "<p><t>已从展示顺序移除的页面</t></p>".into()),
        ]
    }

    #[test]
    fn presentation_pages_follow_relationship_order_and_exclude_unreferenced_slides() {
        let text = extract("pptx", reordered_slides()).unwrap();
        assert_eq!(text, "## 第 1 页\n\n实际第一页\n\n## 第 2 页\n\n实际第二页");
    }

    #[test]
    fn missing_presentation_order_or_slides_cannot_create_incorrect_citations() {
        let mut parts = reordered_slides();
        parts.retain(|(path, _)| path != "ppt/slides/slide2.xml");
        assert!(extract("pptx", parts).unwrap_err().contains("内容缺失"));
        let mut parts = reordered_slides();
        parts.retain(|(path, _)| path != "ppt/presentation.xml");
        assert!(extract("pptx", parts).unwrap_err().contains("顺序信息缺失"));
        let xml = r#"<Relationships><Relationship Id="r1" Target="https://example.invalid/slide.xml" TargetMode="External"/></Relationships>"#;
        assert!(relationship_targets(xml, "ppt").unwrap().is_empty());
        assert!(relationship_targets(r#"<Relationships><Relationship Id="r1" Target="../../outside.xml"/></Relationships>"#, "ppt").is_err());
    }

    #[test]
    fn wide_spreadsheets_preserve_all_values_with_cell_addresses_without_dense_padding() {
        let xml = r#"<worksheet><row r="7"><c r="A7" t="inlineStr"><is><t>项目</t></is></c><c r="AMJ7"><v>1024</v></c><c r="AMK7"><v>1025</v></c><c r="XFD7"><v>16384</v></c></row></worksheet>"#;
        let text = sheet(xml, &[]).unwrap();
        for cell in ["A7：项目", "AMJ7：1024", "AMK7：1025", "XFD7：16384"] {
            assert!(text.contains(cell), "missing {cell}: {text}");
        }
        assert!(
            text.len() < 256,
            "sparse sheets must not expand all empty columns"
        );
    }

    #[test]
    fn invalid_spreadsheet_addresses_are_reported_instead_of_clamped() {
        assert_eq!(column("XFD1048576").unwrap(), 16383);
        for address in ["XFE1", "A0", "A1048577", "1", "A", "A-1"] {
            let xml =
                format!(r#"<worksheet><row><c r="{address}"><v>secret</v></c></row></worksheet>"#);
            assert!(sheet(&xml, &[]).is_err(), "accepted {address}");
        }
    }
    #[test]
    fn spreadsheet_names_follow_relationships_instead_of_filename_order() {
        let parts=vec![
            ("xl/workbook.xml".into(),r#"<workbook xmlns:r="r"><sheets><sheet name="第二季度" r:id="r2"/><sheet name="第一季度" r:id="r1"/></sheets></workbook>"#.into()),
            ("xl/_rels/workbook.xml.rels".into(),r#"<Relationships><Relationship Id="r1" Target="worksheets/sheet1.xml"/><Relationship Id="r2" Target="worksheets/sheet2.xml"/></Relationships>"#.into()),
            ("xl/worksheets/sheet1.xml".into(),"<worksheet><row><c><v>100</v></c></row></worksheet>".into()),
            ("xl/worksheets/sheet2.xml".into(),"<worksheet><row><c><v>200</v></c></row></worksheet>".into()),
        ];
        let result = extract("xlsx", parts).unwrap();
        assert!(result.starts_with("## 工作表：第二季度\n\n| 200 |"));
        assert!(result.contains("## 工作表：第一季度\n\n| 100 |"));
        assert_eq!(
            paragraphs("<p><t>&#x4E2D;&#25991;&amp;</t></p>").unwrap(),
            "中文&"
        );
    }

    #[test]
    fn word_keeps_paragraphs_and_table_conditions() {
        let text=paragraphs(r#"<w:document xmlns:w="w"><w:p><w:r><w:t>费用标准</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t>城市</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>上限</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:p><w:r><w:t>北京</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>500元</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:document>"#).unwrap();
        assert!(text.contains("| 北京 | 500元 |"));
        assert!(text.contains("| 城市 | 上限 |\n| --- | --- |\n| 北京 | 500元 |"));
        assert!(text.starts_with("费用标准\n\n"));
    }
    #[test]
    fn spreadsheet_resolves_strings_and_preserves_empty_columns() {
        let shared = shared_strings("<sst><si><t>项目</t></si><si><t>差旅</t></si></sst>").unwrap();
        let text=sheet(r#"<worksheet><sheetData><row><c r="A1" t="s"><v>0</v></c><c r="C1"><v>100</v></c></row><row><c r="A2" t="s"><v>1</v></c><c r="C2"><v>500</v></c></row></sheetData></worksheet>"#,&shared).unwrap();
        assert!(text.contains("| 差旅 |  | 500 |"));
        assert!(text.contains("| 项目 |  | 100 |"));
    }
}
