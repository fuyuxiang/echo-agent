use crate::error::MermaidError;
use crate::theme::MermaidTheme;
use serde_yaml::{Mapping, Value};

const COLUMN_WIDTH: f64 = 200.0;
const COLUMN_GAP: f64 = 5.0;

const HEADER_HEIGHT: f64 = 25.0;
const BOTTOM_PADDING: f64 = 10.0;

const TASK_WIDTH: f64 = 185.0;
const TASK_HEIGHT: f64 = 44.0;
const TASK_HEIGHT_WITH_ASSIGNED: f64 = 56.0;
const TASK_GAP: f64 = 5.0;
const TASK_TEXT_LINE_HEIGHT: f64 = 24.0;
const TASK_INNER_PADDING: f64 = 10.0;

const DIAGRAM_PADDING: f64 = 10.0;

/// Per the reference SVG, the priority line is inset 2px from the card rect's left edge,
/// and 2px from the top/bottom of the rect.
const PRIORITY_LINE_INSET_X: f64 = 2.0;
const PRIORITY_LINE_INSET_Y: f64 = 2.0;
const PRIORITY_LINE_STROKE_WIDTH: f64 = 4.0;

pub fn render_kanban_diagram_to_svg(
    mermaid_source: &str,
    theme: &MermaidTheme,
) -> Result<String, MermaidError> {
    let board = parse_kanban(mermaid_source)?;

    let columns_count = board.columns.len().max(1) as f64;
    let total_width =
        DIAGRAM_PADDING * 2.0 + columns_count * COLUMN_WIDTH + (columns_count - 1.0) * COLUMN_GAP;

    let mut max_col_height: f64 = 0.0;
    for col in &board.columns {
        let tasks_h = tasks_stack_height(&col.tasks);
        let h = HEADER_HEIGHT + tasks_h + BOTTOM_PADDING;
        max_col_height = max_col_height.max(h);
    }

    let total_height = DIAGRAM_PADDING * 2.0 + max_col_height;

    let mut svg = String::new();

    svg.push_str(&format!(
        "<svg id=\"my-svg\" width=\"100%\" xmlns=\"http://www.w3.org/2000/svg\" \
         xmlns:xlink=\"http://www.w3.org/1999/xlink\" \
         style=\"max-width: {total_width}px; background-color: white;\" \
         viewBox=\"0 0 {total_width} {total_height}\" \
         role=\"graphics-document document\" aria-roledescription=\"kanban\">"
    ));

    // Emit CSS matching Mermaid 11.12.2 reference
    svg.push_str("<style>");
    svg.push_str(&format!(
        "#my-svg{{font-family:\"trebuchet ms\",verdana,arial,sans-serif;font-size:16px;fill:{};}}",
        theme.text_color
    ));
    svg.push_str("#my-svg p{margin:0;}");

    // Section-specific CSS (sections 0-10)
    for i in 0..=10i32 {
        let (fill_hue, fill_sat, fill_light) = section_hsl(i);
        let text_fill = section_text_color(i);
        svg.push_str(&format!(
            "#my-svg .section-{i} rect,#my-svg .section-{i} path,\
             #my-svg .section-{i} circle,#my-svg .section-{i} polygon,\
             #my-svg .section-{i} path\
             {{fill:hsl({fill_hue}, {fill_sat}%, {fill_light}%);\
             stroke:hsl({fill_hue}, {fill_sat}%, {fill_light}%);}}",
        ));
        svg.push_str(&format!("#my-svg .section-{i} text{{fill:{text_fill};}}",));
    }

    // Node styling (matches reference exactly)
    svg.push_str(&format!(
        "#my-svg .node rect,#my-svg .node circle,#my-svg .node ellipse,\
         #my-svg .node polygon,#my-svg .node path\
         {{fill:white;stroke:{};stroke-width:1px;}}",
        theme.node_stroke
    ));
    svg.push_str(&format!(
        "#my-svg .kanban-ticket-link{{fill:white;stroke:{};text-decoration:underline;}}",
        theme.node_stroke
    ));

    // Cluster-label and label styling — makes header text #333 via CSS
    svg.push_str(&format!(
        "#my-svg .cluster-label,#my-svg .label{{color:{};fill:{};}}",
        theme.text_color, theme.text_color
    ));
    svg.push_str(&format!(
        "#my-svg .cluster-label text{{fill:{};font-size:16px;}}",
        theme.text_color
    ));
    svg.push_str(&format!(
        "#my-svg .label text{{fill:{};font-size:16px;}}",
        theme.text_color
    ));

    // Kanban-label class
    svg.push_str(
        "#my-svg .kanban-label{dy:1em;alignment-baseline:middle;\
         text-anchor:middle;dominant-baseline:middle;text-align:center;}",
    );

    svg.push_str("</style>");

    // Background rect
    svg.push_str(&format!(
        "<rect x=\"0\" y=\"0\" width=\"{total_width}\" height=\"{total_height}\" fill=\"white\"/>"
    ));

    // Empty g (matches reference structure)
    svg.push_str("<g/>");

    // === Sections (columns) ===
    svg.push_str("<g class=\"sections\">");
    for (col_idx, col) in board.columns.iter().enumerate() {
        let section_idx = col_idx + 1;
        let col_x = DIAGRAM_PADDING + col_idx as f64 * (COLUMN_WIDTH + COLUMN_GAP);
        let col_y = DIAGRAM_PADDING;

        let tasks_h = tasks_stack_height(&col.tasks);
        let col_h = HEADER_HEIGHT + tasks_h + BOTTOM_PADDING;

        svg.push_str(&format!(
            "<g class=\"cluster undefined section-{section_idx}\" id=\"{}\" data-look=\"classic\">",
            escape_xml(&col.title)
        ));
        // Section rect — no inline fill/stroke; CSS handles it via .section-N
        svg.push_str(&format!(
            "<rect style=\"\" rx=\"5\" ry=\"5\" x=\"{col_x}\" y=\"{col_y}\" \
             width=\"{COLUMN_WIDTH}\" height=\"{col_h}\"/>"
        ));

        // Cluster label using SVG <text> for universal renderer compatibility.
        // Centered horizontally within the column, vertically within the header.
        let label_x = col_x;
        let label_y = col_y;
        let text_x = COLUMN_WIDTH / 2.0;
        let text_y = HEADER_HEIGHT / 2.0;
        svg.push_str(&format!(
            "<g class=\"cluster-label\" transform=\"translate({label_x}, {label_y})\">"
        ));
        svg.push_str(&format!(
            "<text x=\"{text_x}\" y=\"{text_y}\" \
             text-anchor=\"middle\" dominant-baseline=\"central\" \
             font-family=\"'trebuchet ms', verdana, arial, sans-serif\">{}</text>",
            escape_xml(&col.title)
        ));
        svg.push_str("</g>");
        svg.push_str("</g>");
    }
    svg.push_str("</g>");

    // === Items (task cards) ===
    svg.push_str("<g class=\"items\">");
    for (col_idx, col) in board.columns.iter().enumerate() {
        let col_x = DIAGRAM_PADDING + col_idx as f64 * (COLUMN_WIDTH + COLUMN_GAP);
        let col_y = DIAGRAM_PADDING;
        let col_center_x = col_x + COLUMN_WIDTH / 2.0;

        if col.tasks.is_empty() {
            continue;
        }

        // First task center is HEADER_HEIGHT below column top, plus half the task height
        let mut task_center_y = col_y + HEADER_HEIGHT + task_height(&col.tasks[0]) / 2.0;

        for (task_idx, task) in col.tasks.iter().enumerate() {
            if task_idx > 0 {
                let prev_h = task_height(&col.tasks[task_idx - 1]);
                let curr_h = task_height(task);
                task_center_y += prev_h / 2.0 + TASK_GAP + curr_h / 2.0;
            }

            let t_h = task_height(task);
            let half_w = TASK_WIDTH / 2.0;
            let half_h = t_h / 2.0;

            svg.push_str(&format!(
                "<g class=\"node undefined\" id=\"{}\" \
                 transform=\"translate({col_center_x}, {task_center_y})\">",
                escape_xml(&task.label)
            ));

            // Card rect (centered at origin of the transform)
            svg.push_str(&format!(
                "<rect class=\"basic label-container\" style=\"\" rx=\"5\" ry=\"5\" \
                 x=\"{x}\" y=\"{y}\" width=\"{TASK_WIDTH}\" height=\"{t_h}\"/>",
                x = -half_w,
                y = -half_h,
            ));

            // Task label — positioned as foreignObject
            let label_tx = -half_w + TASK_INNER_PADDING;
            let label_ty = if task.assigned.is_some() {
                -half_h + 4.0 // 4px from top when assigned
            } else {
                -(TASK_TEXT_LINE_HEIGHT / 2.0)
            };

            emit_label_text(&mut svg, label_tx, label_ty, &task.label);

            // Assigned/empty placeholders
            match &task.assigned {
                Some(assigned) => {
                    // Empty middle-left placeholder
                    emit_empty_label(&mut svg, label_tx, 0.0);

                    // Assigned name — bottom-right
                    let assigned_w = estimate_text_width(assigned);
                    let assigned_tx = half_w - TASK_INNER_PADDING - assigned_w;
                    emit_label_text(&mut svg, assigned_tx, 0.0, assigned);
                }
                None => {
                    // Two empty label placeholders (matches reference for non-assigned)
                    let ph_y = TASK_TEXT_LINE_HEIGHT / 2.0;
                    emit_empty_label(&mut svg, label_tx, ph_y);

                    let right_tx = half_w - TASK_INNER_PADDING;
                    emit_empty_label(&mut svg, right_tx, ph_y);
                }
            }

            // Priority indicator line
            if let Some(priority_color) = task.priority.as_deref().and_then(color_from_priority) {
                let line_x = -half_w + PRIORITY_LINE_INSET_X;
                let line_y1 = -half_h + PRIORITY_LINE_INSET_Y;
                let line_y2 = half_h - PRIORITY_LINE_INSET_Y;
                svg.push_str(&format!(
                    "<line x1=\"{line_x}\" y1=\"{line_y1}\" x2=\"{line_x}\" y2=\"{line_y2}\" \
                     stroke-width=\"{PRIORITY_LINE_STROKE_WIDTH}\" stroke=\"{priority_color}\"/>"
                ));
            }

            svg.push_str("</g>");
        }
    }
    svg.push_str("</g>");

    svg.push_str("</svg>");

    Ok(svg)
}

/// Emit a native SVG `<text>` label inside `<g class="label">`.
/// Text is left-aligned (text-anchor="start") and vertically centred within
/// one line height so it sits in the same place the old foreignObject did.
fn emit_label_text(svg: &mut String, tx: f64, ty: f64, text: &str) {
    let text_y = TASK_TEXT_LINE_HEIGHT / 2.0;
    svg.push_str(&format!(
        "<g class=\"label\" transform=\"translate({tx}, {ty})\">\
         <text x=\"0\" y=\"{text_y}\" \
         text-anchor=\"start\" dominant-baseline=\"central\" \
         font-family=\"'trebuchet ms', verdana, arial, sans-serif\">\
         {}</text></g>",
        escape_xml(text),
    ));
}

/// Emit an empty placeholder `<g class="label">` (no visible content).
fn emit_empty_label(svg: &mut String, tx: f64, ty: f64) {
    svg.push_str(&format!(
        "<g class=\"label\" transform=\"translate({tx}, {ty})\"/>",
    ));
}

#[derive(Debug, Clone)]
struct KanbanBoard {
    columns: Vec<KanbanColumn>,
}

#[derive(Debug, Clone)]
struct KanbanColumn {
    title: String,
    tasks: Vec<KanbanTask>,
}

#[derive(Debug, Clone)]
struct KanbanTask {
    label: String,
    assigned: Option<String>,
    priority: Option<String>,
    ticket: Option<String>,
}

fn parse_kanban(input: &str) -> Result<KanbanBoard, MermaidError> {
    let lines = input.lines().enumerate();

    let mut found_header = false;
    let mut columns: Vec<KanbanColumn> = Vec::new();
    let mut current_idx: Option<usize> = None;

    for (idx, raw) in lines {
        let line_no = idx + 1;
        let line = raw.trim_end_matches(['\r', '\n']).to_string();
        let trimmed = line.trim();

        if trimmed.is_empty() || trimmed.starts_with("%%") {
            continue;
        }

        if !found_header {
            if trimmed.split_whitespace().next() != Some("kanban") {
                return Err(MermaidError::ParseError {
                    line: line_no,
                    message: "Expected 'kanban' declaration".to_string(),
                });
            }
            found_header = true;
            continue;
        }

        let is_indented = raw.chars().next().is_some_and(|c| c.is_whitespace());
        if !is_indented {
            columns.push(KanbanColumn {
                title: trimmed.to_string(),
                tasks: Vec::new(),
            });
            current_idx = Some(columns.len() - 1);
            continue;
        }

        let Some(cur) = current_idx else {
            return Err(MermaidError::ParseError {
                line: line_no,
                message: "Task found before any kanban column".to_string(),
            });
        };

        let task = parse_kanban_task(trimmed, line_no)?;
        columns[cur].tasks.push(task);
    }

    if !found_header {
        return Err(MermaidError::ParseError {
            line: 1,
            message: "Expected 'kanban' declaration".to_string(),
        });
    }

    Ok(KanbanBoard { columns })
}

fn task_height(task: &KanbanTask) -> f64 {
    if task.assigned.is_some() {
        TASK_HEIGHT_WITH_ASSIGNED
    } else {
        TASK_HEIGHT
    }
}

fn tasks_stack_height(tasks: &[KanbanTask]) -> f64 {
    let mut total = 0.0;
    for (i, task) in tasks.iter().enumerate() {
        if i > 0 {
            total += TASK_GAP;
        }
        total += task_height(task);
    }
    total
}

fn parse_kanban_task(line: &str, line_no: usize) -> Result<KanbanTask, MermaidError> {
    let (label, shape_data) = split_label_and_shape_data(line);

    let mut task = KanbanTask {
        label,
        assigned: None,
        priority: None,
        ticket: None,
    };

    let Some(shape_data) = shape_data else {
        return Ok(task);
    };

    apply_shape_data(&mut task, &shape_data, line_no)?;

    Ok(task)
}

fn split_label_and_shape_data(line: &str) -> (String, Option<String>) {
    let Some(start) = line.find("@{") else {
        return (line.to_string(), None);
    };

    if !line.ends_with('}') {
        return (line.to_string(), None);
    }

    let (label, rest) = line.split_at(start);
    let shape_data = rest.strip_prefix("@{").unwrap_or(rest);
    let shape_data = shape_data.strip_suffix('}').unwrap_or(shape_data);

    (
        label.trim_end().to_string(),
        Some(shape_data.trim().to_string()),
    )
}

fn apply_shape_data(
    task: &mut KanbanTask,
    shape_data: &str,
    line_no: usize,
) -> Result<(), MermaidError> {
    let yaml_data = if shape_data.contains('\n') {
        format!("{shape_data}\n")
    } else {
        format!("{{\n{shape_data}\n}}")
    };

    let doc: Value = serde_yaml::from_str(&yaml_data).map_err(|e| MermaidError::ParseError {
        line: line_no,
        message: format!("Invalid kanban metadata: {e}"),
    })?;

    let Value::Mapping(map) = doc else {
        return Err(MermaidError::ParseError {
            line: line_no,
            message: "Invalid kanban metadata: expected a YAML mapping".to_string(),
        });
    };

    if let Some(label) = yaml_get_string(&map, "label") {
        task.label = label;
    }

    if let Some(assigned) = yaml_get_string(&map, "assigned") {
        task.assigned = Some(assigned);
    }

    if let Some(priority) = yaml_get_string(&map, "priority") {
        task.priority = Some(priority);
    }

    if let Some(ticket) = yaml_get_string(&map, "ticket") {
        task.ticket = Some(ticket);
    }

    Ok(())
}

fn yaml_get_string(map: &Mapping, key: &str) -> Option<String> {
    let value = map.get(Value::String(key.to_string()))?;
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn color_from_priority(priority: &str) -> Option<&'static str> {
    match priority {
        "Very High" => Some("red"),
        "High" => Some("orange"),
        "Medium" => None,
        "Low" => Some("blue"),
        "Very Low" => Some("lightblue"),
        _ => None,
    }
}

/// Returns (hue, saturation, lightness) for a given section index.
/// Matches the Mermaid 11.12.2 CSS section color scheme.
fn section_hsl(idx: i32) -> (i32, f64, f64) {
    let hues: [i32; 12] = [60, 80, 270, 300, 330, 0, 30, 90, 150, 180, 210, 240];
    let wrapped = idx.rem_euclid(12);
    let hue = hues[wrapped as usize];
    if idx == 0 {
        (hue, 100.0, 83.5294117647)
    } else {
        (hue, 100.0, 86.2745098039)
    }
}

/// Returns the text fill color for a given section index.
fn section_text_color(idx: i32) -> &'static str {
    // From the reference CSS: section-2 and section--1 use #ffffff; most others use black.
    // Note: .cluster-label CSS overrides this for header text to #333.
    match idx {
        -1 | 2 => "#ffffff",
        _ => "black",
    }
}

/// Estimate text width in SVG units using a simple char-width heuristic.
/// Uses a slightly wider estimate (9px/char) to prevent clipping in foreignObject,
/// since the inner div's max-width CSS handles the real constraint.
fn estimate_text_width(text: &str) -> f64 {
    let char_width = 9.5;
    text.len() as f64 * char_width
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
