use crate::error::MermaidError;
use crate::theme::MermaidTheme;

const DEFAULT_ROW_HEIGHT: f64 = 32.0;
const DEFAULT_BIT_WIDTH: f64 = 32.0;
const DEFAULT_BITS_PER_ROW: u32 = 32;
const DEFAULT_SHOW_BITS: bool = true;
const DEFAULT_PADDING_X: f64 = 5.0;
const DEFAULT_PADDING_Y: f64 = 5.0;

pub fn render_packet_diagram_to_svg(
    mermaid_source: &str,
    _theme: &MermaidTheme,
) -> Result<String, MermaidError> {
    let diagram = parse_packet_diagram(mermaid_source)?;

    let padding_y = DEFAULT_PADDING_Y + if DEFAULT_SHOW_BITS { 10.0 } else { 0.0 };
    let total_row_height = DEFAULT_ROW_HEIGHT + padding_y;

    let svg_width = DEFAULT_BIT_WIDTH * (DEFAULT_BITS_PER_ROW as f64) + 2.0;
    let svg_height = total_row_height * ((diagram.rows.len() + 1) as f64)
        - if diagram.title.is_some() {
            0.0
        } else {
            DEFAULT_ROW_HEIGHT
        };

    let mut svg = String::new();

    svg.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {svg_width} {svg_height}\">"
    ));
    svg.push_str(
        "<style>\
.packetByte{font-size:10px;}\
.packetByte.start{fill:black;}\
.packetByte.end{fill:black;}\
.packetLabel{fill:black;font-size:12px;}\
.packetTitle{fill:black;font-size:14px;}\
.packetBlock{stroke:black;stroke-width:1;fill:#efefef;}\
</style>",
    );

    svg.push_str("<g>");
    for (row_idx, row) in diagram.rows.iter().enumerate() {
        let word_y = row_idx as f64 * total_row_height + padding_y;

        for block in row {
            let block_x = 1.0 + (block.start % DEFAULT_BITS_PER_ROW) as f64 * DEFAULT_BIT_WIDTH;
            let width =
                (block.end - block.start + 1) as f64 * DEFAULT_BIT_WIDTH - DEFAULT_PADDING_X;

            svg.push_str(&format!(
                "<rect class=\"packetBlock\" x=\"{block_x}\" y=\"{word_y}\" width=\"{width}\" height=\"{DEFAULT_ROW_HEIGHT}\"/>"
            ));

            let label_x = block_x + width / 2.0;
            let label_y = word_y + DEFAULT_ROW_HEIGHT / 2.0;
            svg.push_str(&format!(
                "<text class=\"packetLabel\" x=\"{label_x}\" y=\"{label_y}\" text-anchor=\"middle\" dominant-baseline=\"middle\">{}</text>",
                escape_xml(&block.label)
            ));

            if DEFAULT_SHOW_BITS {
                let bit_y = word_y - 2.0;
                if block.start == block.end {
                    svg.push_str(&format!(
                        "<text class=\"packetByte start\" x=\"{label_x}\" y=\"{bit_y}\" text-anchor=\"middle\" dominant-baseline=\"auto\">{}</text>",
                        block.start
                    ));
                } else {
                    let end_x = block_x + width;
                    svg.push_str(&format!(
                        "<text class=\"packetByte start\" x=\"{block_x}\" y=\"{bit_y}\" text-anchor=\"start\" dominant-baseline=\"auto\">{}</text>",
                        block.start
                    ));
                    svg.push_str(&format!(
                        "<text class=\"packetByte end\" x=\"{end_x}\" y=\"{bit_y}\" text-anchor=\"end\" dominant-baseline=\"auto\">{}</text>",
                        block.end
                    ));
                }
            }
        }
    }
    svg.push_str("</g>");

    let title_x = svg_width / 2.0;
    let title_y = svg_height - total_row_height / 2.0;
    svg.push_str(&format!(
        "<text class=\"packetTitle\" x=\"{title_x}\" y=\"{title_y}\" text-anchor=\"middle\" dominant-baseline=\"middle\">{}</text>",
        diagram
            .title
            .as_deref()
            .map(escape_xml)
            .unwrap_or_default()
    ));

    svg.push_str("</svg>");

    Ok(svg)
}

#[derive(Debug, Clone)]
struct PacketDiagram {
    title: Option<String>,
    rows: Vec<Vec<PacketBlock>>,
}

#[derive(Debug, Clone)]
struct PacketBlock {
    start: u32,
    end: u32,
    label: String,
}

fn parse_packet_diagram(input: &str) -> Result<PacketDiagram, MermaidError> {
    let lines = input.lines().enumerate();

    let mut found_header = false;
    let mut title: Option<String> = None;
    let mut blocks: Vec<PacketBlock> = Vec::new();

    for (idx, raw) in lines {
        let line_no = idx + 1;
        let line = raw.trim();

        if line.is_empty() || line.starts_with("%%") {
            continue;
        }

        if !found_header {
            if line.split_whitespace().next() != Some("packet-beta") {
                return Err(MermaidError::ParseError {
                    line: line_no,
                    message: "Expected 'packet-beta' declaration".to_string(),
                });
            }
            found_header = true;
            continue;
        }

        if let Some(rest) = line.strip_prefix("title ") {
            let t = rest.trim();
            if !t.is_empty() {
                title = Some(t.to_string());
            }
            continue;
        }

        let Some((range_raw, label_raw)) = line.split_once(':') else {
            return Err(MermaidError::ParseError {
                line: line_no,
                message: format!("Invalid packet block: {line}"),
            });
        };

        let (start, end) = parse_range(range_raw.trim(), line_no)?;
        let label = parse_label(label_raw.trim(), line_no)?;

        blocks.push(PacketBlock { start, end, label });
    }

    if !found_header {
        return Err(MermaidError::ParseError {
            line: 1,
            message: "Expected 'packet-beta' declaration".to_string(),
        });
    }

    if blocks.is_empty() {
        return Err(MermaidError::ParseError {
            line: 1,
            message: "Packet diagram requires at least one block".to_string(),
        });
    }

    ensure_contiguous(&blocks)?;
    let rows = split_into_rows(blocks, DEFAULT_BITS_PER_ROW);

    Ok(PacketDiagram { title, rows })
}

fn parse_range(s: &str, line: usize) -> Result<(u32, u32), MermaidError> {
    let s = s.trim();

    if let Some((start_str, end_str)) = s.split_once('-') {
        let start: u32 = start_str
            .trim()
            .parse()
            .map_err(|_| MermaidError::ParseError {
                line,
                message: format!("Invalid packet start: {start_str}"),
            })?;
        let end: u32 = end_str
            .trim()
            .parse()
            .map_err(|_| MermaidError::ParseError {
                line,
                message: format!("Invalid packet end: {end_str}"),
            })?;

        if end < start {
            return Err(MermaidError::ParseError {
                line,
                message: format!("Packet block {start}-{end} is invalid (end < start)"),
            });
        }

        return Ok((start, end));
    }

    let start: u32 = s.parse().map_err(|_| MermaidError::ParseError {
        line,
        message: format!("Invalid packet bit index: {s}"),
    })?;

    Ok((start, start))
}

fn parse_label(s: &str, line: usize) -> Result<String, MermaidError> {
    let s = s.trim();

    if let Some(stripped) = s.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
        return Ok(stripped.to_string());
    }

    if let Some(stripped) = s.strip_prefix('\'').and_then(|t| t.strip_suffix('\'')) {
        return Ok(stripped.to_string());
    }

    if s.is_empty() {
        return Err(MermaidError::ParseError {
            line,
            message: "Packet block label cannot be empty".to_string(),
        });
    }

    Ok(s.to_string())
}

fn ensure_contiguous(blocks: &[PacketBlock]) -> Result<(), MermaidError> {
    let mut last: Option<u32> = None;

    for block in blocks {
        if let Some(last_bit) = last {
            if block.start != last_bit + 1 {
                return Err(MermaidError::ParseError {
                    line: 1,
                    message: format!(
                        "Packet block {}-{} is not contiguous. It should start from {}.",
                        block.start,
                        block.end,
                        last_bit + 1
                    ),
                });
            }
        }
        last = Some(block.end);
    }

    Ok(())
}

fn split_into_rows(blocks: Vec<PacketBlock>, bits_per_row: u32) -> Vec<Vec<PacketBlock>> {
    let mut rows: Vec<Vec<PacketBlock>> = Vec::new();
    let mut word: Vec<PacketBlock> = Vec::new();
    let mut row = 1_u32;

    for block in blocks {
        let mut cur = block;

        loop {
            let (fitting, remainder) = split_block_at_row_boundary(&cur, row, bits_per_row);
            word.push(fitting);

            if word
                .last()
                .is_some_and(|b| b.end.saturating_add(1) == row.saturating_mul(bits_per_row))
            {
                rows.push(std::mem::take(&mut word));
                row = row.saturating_add(1);
            }

            let Some(next) = remainder else {
                break;
            };

            cur = next;
        }
    }

    if !word.is_empty() {
        rows.push(word);
    }

    rows
}

fn split_block_at_row_boundary(
    block: &PacketBlock,
    row: u32,
    bits_per_row: u32,
) -> (PacketBlock, Option<PacketBlock>) {
    let row_end_exclusive = row.saturating_mul(bits_per_row);

    if block.end.saturating_add(1) <= row_end_exclusive {
        return (block.clone(), None);
    }

    let first = PacketBlock {
        start: block.start,
        end: row_end_exclusive.saturating_sub(1),
        label: block.label.clone(),
    };

    let second = PacketBlock {
        start: row_end_exclusive,
        end: block.end,
        label: block.label.clone(),
    };

    (first, Some(second))
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
