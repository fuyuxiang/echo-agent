use crate::error::MermaidError;
use crate::theme::MermaidTheme;

use std::collections::BTreeMap;

/// Mermaid 11.12.2 gantt config defaults (from config.schema.yaml)
const BAR_HEIGHT: f64 = 20.0;
const BAR_GAP: f64 = 4.0;
const TOP_PADDING: f64 = 50.0;
const LEFT_PADDING: f64 = 75.0;
const RIGHT_PADDING: f64 = 75.0;
const GRID_LINE_START_PADDING: f64 = 35.0;
const FONT_SIZE: f64 = 11.0;
const SECTION_FONT_SIZE: f64 = 11.0;
const TITLE_TOP_MARGIN: f64 = 25.0;
const BOTTOM_AXIS_HEIGHT: f64 = 50.0;
const RX: f64 = 3.0;
const RY: f64 = 3.0;

/// Default theme colors from Mermaid 11.12.2 theme-default.js
const SECTION_BKG_COLOR: &str = "rgba(102,102,255,0.49)";
const ALT_SECTION_BKG_COLOR: &str = "white";
const TASK_BKG_COLOR: &str = "#8a90dd";
const TASK_BORDER_COLOR: &str = "#534fbc";
const TASK_TEXT_COLOR: &str = "white";
const TASK_TEXT_DARK_COLOR: &str = "black";
const GRID_COLOR: &str = "#333";
const TITLE_COLOR: &str = "#333";
const FONT_FAMILY: &str = "'trebuchet ms', verdana, arial, sans-serif";

pub fn render_gantt_diagram_to_svg(
    mermaid_source: &str,
    _theme: &MermaidTheme,
) -> Result<String, MermaidError> {
    let chart = parse_gantt_diagram(mermaid_source)?;

    // Collect unique categories (section types) in order
    let mut categories: Vec<String> = Vec::new();
    for task in &chart.tasks {
        let cat = task.section.clone().unwrap_or_default();
        if !categories.contains(&cat) {
            categories.push(cat);
        }
    }

    // Category heights (count of tasks per category)
    let mut category_heights: BTreeMap<String, usize> = BTreeMap::new();
    for task in &chart.tasks {
        let cat = task.section.clone().unwrap_or_default();
        *category_heights.entry(cat).or_insert(0) += 1;
    }

    let num_tasks = chart.tasks.len();
    let gap = BAR_HEIGHT + BAR_GAP;
    let h = 2.0 * TOP_PADDING + num_tasks as f64 * gap;

    // Compute time domain
    let mut min_day = i32::MAX;
    let mut max_day = i32::MIN;
    for task in &chart.tasks {
        min_day = min_day.min(task.start_day);
        max_day = max_day.max(task.start_day + task.duration_days);
    }

    let w = 784.0_f64;
    let plot_width = w - LEFT_PADDING - RIGHT_PADDING;

    // Time scale: maps day offset to pixel x
    let span_days = (max_day - min_day).max(1) as f64;
    let px_per_day = plot_width / span_days;

    let mut svg = String::new();
    svg.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {w} {h}\">"
    ));

    // Embedded <style> matching Mermaid 11.12.2 gantt styles
    svg.push_str("<style>");
    svg.push_str(&format!(
        ".section {{ stroke: none; opacity: 0.2; }}\
         .section0 {{ fill: {SECTION_BKG_COLOR}; }}\
         .section1 {{ fill: {ALT_SECTION_BKG_COLOR}; opacity: 0.2; }}\
         .grid .tick line {{ stroke: {GRID_COLOR}; opacity: 0.8; shape-rendering: crispEdges; }}\
         .grid .tick text {{ font-family: {FONT_FAMILY}; fill: #000; font-size: 10px; }}\
         .grid path {{ stroke-width: 0; }}\
         .task {{ stroke-width: 2; }}\
         .task0 {{ fill: {TASK_BKG_COLOR}; stroke: {TASK_BORDER_COLOR}; }}\
         .taskText {{ text-anchor: middle; font-family: {FONT_FAMILY}; }}\
         .taskText0 {{ fill: {TASK_TEXT_COLOR}; }}\
         .taskTextOutsideRight {{ fill: {TASK_TEXT_DARK_COLOR}; text-anchor: start; font-family: {FONT_FAMILY}; }}\
         .taskTextOutsideLeft {{ fill: {TASK_TEXT_DARK_COLOR}; text-anchor: end; }}\
         .titleText {{ text-anchor: middle; font-size: 18px; font-family: {FONT_FAMILY}; fill: {TITLE_COLOR}; }}\
         .sectionTitle {{ text-anchor: start; font-family: {FONT_FAMILY}; font-size: {SECTION_FONT_SIZE}px; }}\
         .sectionTitle0, .sectionTitle1 {{ fill: {TITLE_COLOR}; }}"
    ));
    svg.push_str("</style>");

    // 1. Section background bands
    {
        let mut task_idx = 0;
        for (cat_order, cat) in categories.iter().enumerate() {
            let count = category_heights.get(cat).copied().unwrap_or(0);
            if count == 0 {
                continue;
            }
            let y = task_idx as f64 * gap + TOP_PADDING - 2.0;
            let rect_h = count as f64 * gap;
            let section_class = format!("section section{}", cat_order % 2);
            svg.push_str(&format!(
                "<rect x=\"0\" y=\"{y:.1}\" width=\"{w_rect:.1}\" height=\"{rect_h:.1}\" class=\"{section_class}\"/>",
                w_rect = w - RIGHT_PADDING / 2.0
            ));
            task_idx += count;
        }
    }

    // 2. Grid lines and bottom axis
    {
        let axis_y = h - BOTTOM_AXIS_HEIGHT;
        svg.push_str(&format!(
            "<g class=\"grid\" transform=\"translate({LEFT_PADDING}, {axis_y})\">"
        ));

        let total_days = (max_day - min_day) as usize;
        for d in 0..=total_days {
            let x = d as f64 * px_per_day;
            // Matches D3.js: tickSize(-h + topPadding + gridLineStartPadding)
            let tick_top = -h + TOP_PADDING + GRID_LINE_START_PADDING;
            svg.push_str(&format!(
                "<g class=\"tick\" transform=\"translate({x:.2}, 0)\">\
                 <line y2=\"{tick_top:.1}\"/>\
                 <text dy=\"1em\" text-anchor=\"middle\">{label}</text>\
                 </g>",
                label = day_to_ymd_str(min_day + d as i32)
            ));
        }

        svg.push_str("</g>");
    }

    // 3. Task bars
    for (i, task) in chart.tasks.iter().enumerate() {
        let x = (task.start_day - min_day) as f64 * px_per_day + LEFT_PADDING;
        let bar_w = task.duration_days as f64 * px_per_day;
        let y = i as f64 * gap + TOP_PADDING;

        let sec_num = task
            .section
            .as_ref()
            .and_then(|s| categories.iter().position(|c| c == s))
            .unwrap_or(0)
            % 4;

        svg.push_str(&format!(
            "<rect rx=\"{RX}\" ry=\"{RY}\" x=\"{x:.2}\" y=\"{y:.2}\" width=\"{bar_w:.2}\" height=\"{BAR_HEIGHT}\" \
             class=\"task task{sec_num}\"/>"
        ));
    }

    // 4. Task text (inside bars, or outside if text doesn't fit)
    for (i, task) in chart.tasks.iter().enumerate() {
        let start_x = (task.start_day - min_day) as f64 * px_per_day;
        let end_x = start_x + task.duration_days as f64 * px_per_day;
        let bar_w = end_x - start_x;

        // Estimate text width (Mermaid uses getBBox, we approximate)
        let text_width = task.name.len() as f64 * FONT_SIZE * 0.6;

        let (tx, text_class) = if text_width > bar_w {
            if end_x + text_width + 1.5 * LEFT_PADDING > w - LEFT_PADDING {
                (start_x + LEFT_PADDING - 5.0, "taskTextOutsideLeft")
            } else {
                (end_x + LEFT_PADDING + 5.0, "taskTextOutsideRight")
            }
        } else {
            (bar_w / 2.0 + start_x + LEFT_PADDING, "taskText taskText0")
        };

        let ty = i as f64 * gap + BAR_HEIGHT / 2.0 + (FONT_SIZE / 2.0 - 2.0) + TOP_PADDING;

        svg.push_str(&format!(
            "<text x=\"{tx:.2}\" y=\"{ty:.2}\" font-size=\"{FONT_SIZE}\" class=\"{text_class}\">{}</text>",
            escape_xml(&task.name)
        ));
    }

    // 5. Section labels (vertLabels)
    {
        let ordered_cats: Vec<(String, usize)> = categories
            .iter()
            .map(|c| (c.clone(), category_heights.get(c).copied().unwrap_or(0)))
            .collect();

        let mut prev_total = 0_usize;
        for (i, (cat_name, count)) in ordered_cats.iter().enumerate() {
            if cat_name.is_empty() {
                prev_total += count;
                continue;
            }
            let y = if i > 0 {
                (*count as f64 * gap) / 2.0 + prev_total as f64 * gap + TOP_PADDING
            } else {
                (*count as f64 * gap) / 2.0 + TOP_PADDING
            };

            let sec_num = i % 4;
            svg.push_str(&format!(
                "<text x=\"10\" y=\"{y:.2}\" font-size=\"{SECTION_FONT_SIZE}\" class=\"sectionTitle sectionTitle{sec_num}\">{}</text>",
                escape_xml(cat_name)
            ));
            prev_total += count;
        }
    }

    // 6. Title
    if let Some(title) = &chart.title {
        svg.push_str(&format!(
            "<text x=\"{x:.1}\" y=\"{TITLE_TOP_MARGIN}\" class=\"titleText\">{}</text>",
            escape_xml(title),
            x = w / 2.0
        ));
    }

    svg.push_str("</svg>");
    Ok(svg)
}

#[derive(Debug, Clone)]
struct GanttChart {
    title: Option<String>,
    tasks: Vec<GanttTask>,
}

#[derive(Debug, Clone)]
struct GanttTask {
    section: Option<String>,
    name: String,
    start_day: i32,
    duration_days: i32,
}

fn parse_gantt_diagram(input: &str) -> Result<GanttChart, MermaidError> {
    let lines: Vec<&str> = input.lines().collect();

    let mut i = 0_usize;
    while i < lines.len() {
        let line = lines[i].trim();
        if line.is_empty() || line.starts_with("%%") {
            i += 1;
            continue;
        }

        if line.split_whitespace().next() == Some("gantt") {
            i += 1;
            break;
        }

        return Err(MermaidError::ParseError {
            line: i + 1,
            message: "Expected 'gantt' declaration".to_string(),
        });
    }

    let mut title: Option<String> = None;
    let mut current_section: Option<String> = None;
    let mut tasks: Vec<GanttTask> = Vec::new();
    let mut tasks_by_id: BTreeMap<String, (i32, i32)> = BTreeMap::new();

    while i < lines.len() {
        let raw = lines[i];
        let line = raw.trim();
        let line_no = i + 1;
        i += 1;

        if line.is_empty() || line.starts_with("%%") {
            continue;
        }

        if let Some(rest) = line.strip_prefix("title ") {
            let t = rest.trim();
            if !t.is_empty() {
                title = Some(t.to_string());
            }
            continue;
        }

        if line.starts_with("dateFormat ") {
            continue;
        }

        if let Some(rest) = line.strip_prefix("section ") {
            let name = rest.trim();
            current_section = if name.is_empty() {
                None
            } else {
                Some(name.to_string())
            };
            continue;
        }

        let Some((name_raw, spec_raw)) = line.split_once(':') else {
            return Err(MermaidError::ParseError {
                line: line_no,
                message: format!("Invalid gantt task line: {line}"),
            });
        };

        let name = name_raw.trim();
        let spec_parts: Vec<&str> = spec_raw
            .split(',')
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
            .collect();
        if spec_parts.len() < 3 {
            return Err(MermaidError::ParseError {
                line: line_no,
                message: format!("Invalid gantt task spec: {spec_raw}"),
            });
        }

        let id = spec_parts[0].to_string();
        let start_spec = spec_parts[1];
        let duration_spec = spec_parts[2];

        let duration_days =
            parse_duration_days(duration_spec).map_err(|message| MermaidError::ParseError {
                line: line_no,
                message,
            })?;

        let start_day = if let Some(after) = start_spec.strip_prefix("after ") {
            let ref_id = after.trim();
            let Some((ref_start, ref_dur)) = tasks_by_id.get(ref_id).copied() else {
                return Err(MermaidError::ParseError {
                    line: line_no,
                    message: format!("Unknown gantt dependency id: {ref_id}"),
                });
            };
            ref_start + ref_dur
        } else {
            parse_ymd_to_day(start_spec).map_err(|message| MermaidError::ParseError {
                line: line_no,
                message,
            })?
        };

        let task = GanttTask {
            section: current_section.clone(),
            name: name.to_string(),
            start_day,
            duration_days,
        };

        tasks_by_id.insert(id, (start_day, duration_days));

        tasks.push(task);
    }

    if tasks.is_empty() {
        return Err(MermaidError::ParseError {
            line: 1,
            message: "Gantt diagram requires at least one task".to_string(),
        });
    }

    Ok(GanttChart { title, tasks })
}

fn parse_duration_days(spec: &str) -> Result<i32, String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err("Empty duration".to_string());
    }

    let (num_str, unit) = spec.split_at(spec.len().saturating_sub(1));
    let n: i32 = num_str
        .trim()
        .parse()
        .map_err(|_| format!("Invalid duration: {spec}"))?;
    match unit {
        "d" | "D" => Ok(n),
        "w" | "W" => Ok(n * 7),
        _ => Err(format!("Unsupported duration unit: {spec}")),
    }
}

fn parse_ymd_to_day(s: &str) -> Result<i32, String> {
    let parts: Vec<&str> = s.trim().split('-').collect();
    if parts.len() != 3 {
        return Err(format!("Invalid date: {s}"));
    }

    let y: i32 = parts[0].parse().map_err(|_| format!("Invalid year: {s}"))?;
    let m: i32 = parts[1]
        .parse()
        .map_err(|_| format!("Invalid month: {s}"))?;
    let d: i32 = parts[2].parse().map_err(|_| format!("Invalid day: {s}"))?;

    Ok(days_from_civil(y, m, d))
}

/// Convert a day number back to (year, month, day).
fn day_to_ymd(day_number: i32) -> (i32, i32, i32) {
    let z = day_number + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

fn day_to_ymd_str(day_number: i32) -> String {
    let (y, m, d) = day_to_ymd(day_number);
    format!("{y:04}-{m:02}-{d:02}")
}

fn days_from_civil(y: i32, m: i32, d: i32) -> i32 {
    let y = y - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
