use crate::error::MermaidError;
use crate::theme::MermaidTheme;

// --- Mermaid 11.12.2 journey config defaults ---
const DIAGRAM_MARGIN_X: f64 = 50.0;
const DIAGRAM_MARGIN_Y: f64 = 10.0;
const LEFT_MARGIN: f64 = 150.0;
const TASK_WIDTH: f64 = 150.0;
const TASK_HEIGHT: f64 = 50.0;
const TASK_MARGIN: f64 = 50.0;
const SECTION_Y: f64 = 50.0;
const FACE_RADIUS: f64 = 15.0;
const ACTOR_CIRCLE_R: f64 = 7.0;
const MAX_FACE_Y: f64 = 300.0;
const FACE_Y_PER_SCORE: f64 = 30.0;
const TASK_LINE_BOTTOM: f64 = MAX_FACE_Y + 5.0 * FACE_Y_PER_SCORE; // 450
const ARROW_Y_MULTIPLIER: f64 = 4.0; // conf.height * 4 = 200
const FONT_FAMILY: &str = "'trebuchet ms', verdana, arial, sans-serif";
const TASK_FONT_SIZE: f64 = 14.0;
const TASK_FONT_FAMILY: &str = "'Open Sans', sans-serif";
const TITLE_FONT_SIZE: &str = "4ex";

// CSS section/task fill colors from Mermaid default theme
const SECTION_FILLS: &[&str] = &[
    "#ECECFF",
    "#ffffde",
    "hsl(304, 100%, 96.2745098039%)",
    "hsl(124, 100%, 93.5294117647%)",
    "hsl(176, 100%, 96.2745098039%)",
    "hsl(-4, 100%, 93.5294117647%)",
    "hsl(8, 100%, 96.2745098039%)",
    "hsl(188, 100%, 93.5294117647%)",
];

// SVG fill attributes for section rects (from sectionFills config)
const SECTION_SVG_FILLS: &[&str] = &[
    "#191970", "#8B008B", "#4B0082", "#2F4F4F", "#800000", "#8B4513", "#00008B",
];

const ACTOR_COLOURS: &[&str] = &[
    "#8FBC8F", "#7CFC00", "#00FFFF", "#20B2AA", "#B0E0E6", "#FFFFE0",
];

pub fn render_journey_diagram_to_svg(
    mermaid_source: &str,
    _theme: &MermaidTheme,
) -> Result<String, MermaidError> {
    let journey = parse_journey_diagram(mermaid_source)?;

    // Collect unique actors in order of first appearance
    let mut actors: Vec<String> = Vec::new();
    for row in &journey.rows {
        if let JourneyRow::Task(task) = row {
            for actor in &task.actors {
                if !actors.contains(actor) {
                    actors.push(actor.clone());
                }
            }
        }
    }

    // In mermaid.js: leftMargin = conf.leftMargin + maxWidth
    // maxWidth is from actor legend text measurement; for simple cases it's 0
    let left_margin = LEFT_MARGIN;

    // Flatten tasks with their section assignments
    let mut flat_tasks: Vec<FlatTask> = Vec::new();
    let mut current_section: Option<String> = None;
    for row in &journey.rows {
        match row {
            JourneyRow::Section(name) => {
                current_section = Some(name.clone());
            }
            JourneyRow::Task(task) => {
                flat_tasks.push(FlatTask {
                    name: task.name.clone(),
                    score: task.score,
                    actors: task.actors.clone(),
                    section: current_section.clone().unwrap_or_default(),
                });
            }
        }
    }

    let num_tasks = flat_tasks.len();
    if num_tasks == 0 {
        return Err(MermaidError::ParseError {
            line: 1,
            message: "Journey requires at least one task".to_string(),
        });
    }

    // Compute section info
    let mut sections: Vec<SectionInfo> = Vec::new();
    {
        let mut last_section = String::new();
        let mut section_idx: usize = 0;
        for (i, task) in flat_tasks.iter().enumerate() {
            if task.section != last_section {
                // Count tasks in this section
                let count = flat_tasks[i..]
                    .iter()
                    .take_while(|t| t.section == task.section)
                    .count();
                let section_num = section_idx % SECTION_SVG_FILLS.len();
                sections.push(SectionInfo {
                    name: task.section.clone(),
                    first_task_idx: i,
                    task_count: count,
                    section_num,
                });
                last_section = task.section.clone();
                section_idx += 1;
            }
        }
    }

    // Compute task positions: task.x = i * taskMargin + i * width + leftMargin
    let task_positions: Vec<f64> = (0..num_tasks)
        .map(|i| i as f64 * TASK_MARGIN + i as f64 * TASK_WIDTH + left_margin)
        .collect();

    // Section vertical height for task y position
    let section_v_height = TASK_HEIGHT * 2.0 + DIAGRAM_MARGIN_Y; // 110
    let task_y = section_v_height; // 110 (0 + sectionVHeight)

    // Arrow y: conf.height * 4 = 50 * 4 = 200
    let arrow_y = TASK_HEIGHT * ARROW_Y_MULTIPLIER;

    // Compute overall dimensions using mermaid.js bounds logic:
    // bounds.insert(task.x, task.y, task.x + task.width + taskMargin, 300+5*30)
    // where task.width = diagramMarginX (50), NOT the visual TASK_WIDTH (150)
    let last_task_x = task_positions.last().copied().unwrap_or(left_margin);
    let bounds_stopx = last_task_x + DIAGRAM_MARGIN_X + TASK_MARGIN; // 50 + 50 = 100

    let width = left_margin + bounds_stopx + 2.0 * DIAGRAM_MARGIN_X;
    // height = stopy - starty + 2 * diagramMarginY; starty=0, stopy=450
    let height = TASK_LINE_BOTTOM + 2.0 * DIAGRAM_MARGIN_Y;

    let has_title = journey.title.is_some();
    let extra_vert_for_title = if has_title { 70.0 } else { 0.0 };
    let viewbox_height = height + extra_vert_for_title;
    let svg_height = height + extra_vert_for_title + 25.0;

    // Arrow endpoint: width - leftMargin - 4
    let arrow_x2 = width - left_margin - 4.0;

    let mut svg = String::new();

    // SVG header
    svg.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" \
         xmlns:xlink=\"http://www.w3.org/1999/xlink\" \
         style=\"max-width: {width:.0}px;\" \
         width=\"100%\" \
         viewBox=\"0 -25 {width:.0} {vh:.0}\" \
         preserveAspectRatio=\"xMinYMin meet\" \
         height=\"{sh:.0}\" \
         role=\"graphics-document document\" \
         aria-roledescription=\"journey\">",
        vh = viewbox_height,
        sh = svg_height,
    ));

    // CSS styles matching mermaid.js default theme
    svg.push_str("<style>");
    svg.push_str(&format!(
        "svg {{font-family:{FONT_FAMILY};font-size:16px;fill:#333;}}"
    ));
    svg.push_str(".mouth{stroke:#666;}");
    svg.push_str("line{stroke:#333;}");
    svg.push_str(&format!(".legend{{fill:#333;font-family:{FONT_FAMILY};}}"));
    svg.push_str(".label text{fill:#333;}");
    svg.push_str(".face{fill:#FFF8DC;stroke:#999;}");

    // Section/task type fills
    for (i, fill) in SECTION_FILLS.iter().enumerate() {
        svg.push_str(&format!(".task-type-{i},.section-type-{i}{{fill:{fill};}}"));
    }

    // Actor colors
    for (i, color) in ACTOR_COLOURS.iter().enumerate() {
        svg.push_str(&format!(".actor-{i}{{fill:{color};}}"));
    }

    svg.push_str("</style>");

    // Arrowhead marker definition
    svg.push_str("<defs><marker id=\"arrowhead\" refX=\"5\" refY=\"2\" markerWidth=\"6\" markerHeight=\"4\" orient=\"auto\"><path d=\"M 0,0 V 4 L6,2 Z\"/></marker></defs>");

    // Actor legend
    let mut actor_y = 60.0_f64;
    for (pos, actor) in actors.iter().enumerate() {
        let color = ACTOR_COLOURS[pos % ACTOR_COLOURS.len()];
        svg.push_str(&format!(
            "<circle cx=\"20\" cy=\"{actor_y:.0}\" class=\"actor-{pos}\" fill=\"{color}\" stroke=\"#000\" r=\"{ACTOR_CIRCLE_R}\"/>"
        ));
        svg.push_str(&format!(
            "<text x=\"40\" y=\"{ty:.0}\" class=\"legend\"><tspan x=\"50\">{}</tspan></text>",
            escape_xml(actor),
            ty = actor_y + 7.0,
        ));
        actor_y += 20.0;
    }

    // Draw sections
    for section in &sections {
        let section_x = task_positions[section.first_task_idx];
        let section_width = TASK_WIDTH * section.task_count as f64
            + DIAGRAM_MARGIN_X * (section.task_count as f64 - 1.0);
        let fill = SECTION_SVG_FILLS[section.section_num % SECTION_SVG_FILLS.len()];
        let num = section.section_num;

        svg.push_str("<g>");
        svg.push_str(&format!(
            "<rect x=\"{section_x:.0}\" y=\"{SECTION_Y:.0}\" fill=\"{fill}\" stroke=\"#666\" \
             width=\"{section_width:.0}\" height=\"{TASK_HEIGHT:.0}\" rx=\"3\" ry=\"3\" \
             class=\"journey-section section-type-{num}\"/>"
        ));

        // Section label using foreignObject with tspan fallback
        let center_x = section_x + section_width / 2.0;
        let center_y = SECTION_Y + TASK_HEIGHT / 2.0;
        svg.push_str(&format!(
            "<switch>\
             <foreignObject x=\"{section_x:.0}\" y=\"{SECTION_Y:.0}\" width=\"{section_width:.0}\" height=\"{TASK_HEIGHT:.0}\" \
             requiredExtensions=\"http://www.w3.org/1999/xhtml\">\
             <div class=\"journey-section section-type-{num}\" xmlns=\"http://www.w3.org/1999/xhtml\" \
             style=\"display: table; height: 100%; width: 100%;\">\
             <div class=\"label\" style=\"display: table-cell; text-align: center; vertical-align: middle;\">\
             {}</div></div></foreignObject>\
             <text x=\"{center_x:.0}\" y=\"{center_y:.0}\" dominant-baseline=\"central\" \
             alignment-baseline=\"central\" class=\"journey-section\" \
             style=\"text-anchor: middle; font-size: {TASK_FONT_SIZE}px; font-family: {TASK_FONT_FAMILY};\">\
             <tspan x=\"{center_x:.0}\" dy=\"0\">{}</tspan></text></switch>",
            escape_xml(&section.name),
            escape_xml(&section.name),
        ));
        svg.push_str("</g>");
    }

    // Draw tasks
    let mut current_section_num = 0_usize;
    let mut current_section_name = String::new();
    let mut section_idx = 0_usize;
    for (i, task) in flat_tasks.iter().enumerate() {
        let task_counter = i;
        // Track section
        if task.section != current_section_name {
            if section_idx < sections.len() {
                current_section_num = sections[section_idx].section_num;
                section_idx += 1;
            }
            current_section_name = task.section.clone();
        }

        let tx = task_positions[i];
        let center = tx + TASK_WIDTH / 2.0;
        let fill = SECTION_SVG_FILLS[current_section_num % SECTION_SVG_FILLS.len()];
        let num = current_section_num;

        svg.push_str("<g>");

        // Dashed task line
        svg.push_str(&format!(
            "<line id=\"task{task_counter}\" x1=\"{center:.0}\" y1=\"{task_y:.0}\" \
             x2=\"{center:.0}\" y2=\"{TASK_LINE_BOTTOM:.0}\" class=\"task-line\" \
             stroke-width=\"1px\" stroke-dasharray=\"4 2\" stroke=\"#666\"/>"
        ));

        // Face icon
        let face_cy = MAX_FACE_Y + (5.0 - task.score as f64) * FACE_Y_PER_SCORE;
        draw_face(&mut svg, center, face_cy, task.score);

        // Task rectangle
        svg.push_str(&format!(
            "<rect x=\"{tx:.0}\" y=\"{task_y:.0}\" fill=\"{fill}\" stroke=\"#666\" \
             width=\"{TASK_WIDTH:.0}\" height=\"{TASK_HEIGHT:.0}\" rx=\"3\" ry=\"3\" \
             class=\"task task-type-{num}\"/>"
        ));

        // Actor circles on the task
        let mut x_pos = tx + 14.0;
        for actor_name in &task.actors {
            if let Some(pos) = actors.iter().position(|a| a == actor_name) {
                let color = ACTOR_COLOURS[pos % ACTOR_COLOURS.len()];
                svg.push_str(&format!(
                    "<circle cx=\"{x_pos:.0}\" cy=\"{task_y:.0}\" class=\"actor-{pos}\" \
                     fill=\"{color}\" stroke=\"#000\" r=\"{ACTOR_CIRCLE_R}\"><title>{}</title></circle>",
                    escape_xml(actor_name)
                ));
                x_pos += 10.0;
            }
        }

        // Task label using foreignObject with tspan fallback
        let task_center_x = tx + TASK_WIDTH / 2.0;
        let task_center_y = task_y + TASK_HEIGHT / 2.0;
        svg.push_str(&format!(
            "<switch>\
             <foreignObject x=\"{tx:.0}\" y=\"{task_y:.0}\" width=\"{TASK_WIDTH:.0}\" height=\"{TASK_HEIGHT:.0}\" \
             requiredExtensions=\"http://www.w3.org/1999/xhtml\">\
             <div class=\"task\" xmlns=\"http://www.w3.org/1999/xhtml\" \
             style=\"display: table; height: 100%; width: 100%;\">\
             <div class=\"label\" style=\"display: table-cell; text-align: center; vertical-align: middle;\">\
             {}</div></div></foreignObject>\
             <text x=\"{task_center_x:.0}\" y=\"{task_center_y:.0}\" dominant-baseline=\"central\" \
             alignment-baseline=\"central\" class=\"task\" \
             style=\"text-anchor: middle; font-size: {TASK_FONT_SIZE}px; font-family: {TASK_FONT_FAMILY};\">\
             <tspan x=\"{task_center_x:.0}\" dy=\"0\">{}</tspan></text></switch>",
            escape_xml(&task.name),
            escape_xml(&task.name),
        ));

        svg.push_str("</g>");
    }

    // Title
    if let Some(title) = &journey.title {
        svg.push_str(&format!(
            "<text x=\"{left_margin:.0}\" font-size=\"{TITLE_FONT_SIZE}\" \
             font-weight=\"bold\" y=\"25\" fill=\"#333\" \
             font-family=\"{FONT_FAMILY}\">{}</text>",
            escape_xml(title),
        ));
    }

    // Horizontal arrow
    svg.push_str(&format!(
        "<line x1=\"{left_margin:.0}\" y1=\"{arrow_y:.0}\" x2=\"{arrow_x2:.0}\" y2=\"{arrow_y:.0}\" \
         stroke-width=\"4\" stroke=\"black\" marker-end=\"url(#arrowhead)\"/>"
    ));

    svg.push_str("</svg>");
    Ok(svg)
}

fn draw_face(svg: &mut String, cx: f64, cy: f64, score: i32) {
    // Face circle
    svg.push_str(&format!(
        "<circle cx=\"{cx:.0}\" cy=\"{cy:.0}\" class=\"face\" r=\"{FACE_RADIUS}\" \
         stroke-width=\"2\" overflow=\"visible\"/>"
    ));

    svg.push_str("<g>");

    // Eyes
    let eye_y = cy - FACE_RADIUS / 3.0;
    let left_eye_x = cx - FACE_RADIUS / 3.0;
    let right_eye_x = cx + FACE_RADIUS / 3.0;
    svg.push_str(&format!(
        "<circle cx=\"{left_eye_x:.0}\" cy=\"{eye_y:.0}\" r=\"1.5\" stroke-width=\"2\" fill=\"#666\" stroke=\"#666\"/>"
    ));
    svg.push_str(&format!(
        "<circle cx=\"{right_eye_x:.0}\" cy=\"{eye_y:.0}\" r=\"1.5\" stroke-width=\"2\" fill=\"#666\" stroke=\"#666\"/>"
    ));

    // Mouth based on score
    if score > 3 {
        // Happy: smile arc
        let inner_r = FACE_RADIUS / 2.0;
        let outer_r = FACE_RADIUS / 2.2;
        let arc_path = generate_smile_arc(inner_r, outer_r);
        svg.push_str(&format!(
            "<path class=\"mouth\" d=\"{arc_path}\" transform=\"translate({cx:.0},{ty:.0})\"/>",
            ty = cy + 2.0,
        ));
    } else if score < 3 {
        // Sad: frown arc
        let inner_r = FACE_RADIUS / 2.0;
        let outer_r = FACE_RADIUS / 2.2;
        let arc_path = generate_sad_arc(inner_r, outer_r);
        svg.push_str(&format!(
            "<path class=\"mouth\" d=\"{arc_path}\" transform=\"translate({cx:.0},{ty:.0})\"/>",
            ty = cy + 7.0,
        ));
    } else {
        // Neutral: straight line
        svg.push_str(&format!(
            "<line class=\"mouth\" stroke=\"#666\" x1=\"{x1:.0}\" y1=\"{y1:.0}\" \
             x2=\"{x2:.0}\" y2=\"{y1:.0}\" stroke-width=\"1px\"/>",
            x1 = cx - 5.0,
            y1 = cy + 7.0,
            x2 = cx + 5.0,
        ));
    }

    svg.push_str("</g>");
}

/// Generate the smile arc path matching d3.arc with
/// startAngle=PI/2, endAngle=3*PI/2, innerRadius=r/2, outerRadius=r/2.2
fn generate_smile_arc(inner_r: f64, outer_r: f64) -> String {
    // The exact path from the reference SVG is:
    // M7.5,0A7.5,7.5,0,1,1,-7.5,0L-6.818,0A6.818,6.818,0,1,0,6.818,0Z
    format!(
        "M{or},0A{or},{or},0,1,1,-{or},0L-{ir},0A{ir},{ir},0,1,0,{ir},0Z",
        or = format_num(outer_r),
        ir = format_num(inner_r),
    )
}

/// Generate the sad arc path matching d3.arc with
/// startAngle=3*PI/2, endAngle=5*PI/2, innerRadius=r/2, outerRadius=r/2.2
fn generate_sad_arc(inner_r: f64, outer_r: f64) -> String {
    format!(
        "M-{or},0A{or},{or},0,1,1,{or},0L{ir},0A{ir},{ir},0,1,0,-{ir},0Z",
        or = format_num(outer_r),
        ir = format_num(inner_r),
    )
}

fn format_num(n: f64) -> String {
    let s = format!("{n:.3}");
    if s.contains('.') {
        let trimmed = s.trim_end_matches('0').trim_end_matches('.');
        trimmed.to_string()
    } else {
        s
    }
}

#[derive(Debug, Clone)]
struct JourneyDiagram {
    title: Option<String>,
    rows: Vec<JourneyRow>,
}

#[derive(Debug, Clone)]
enum JourneyRow {
    Section(String),
    Task(JourneyTask),
}

#[derive(Debug, Clone)]
struct JourneyTask {
    name: String,
    score: i32,
    actors: Vec<String>,
}

#[derive(Debug, Clone)]
struct FlatTask {
    name: String,
    score: i32,
    actors: Vec<String>,
    section: String,
}

#[derive(Debug)]
struct SectionInfo {
    name: String,
    first_task_idx: usize,
    task_count: usize,
    section_num: usize,
}

fn parse_journey_diagram(input: &str) -> Result<JourneyDiagram, MermaidError> {
    let lines: Vec<&str> = input.lines().collect();

    let mut i = 0_usize;
    while i < lines.len() {
        let line = lines[i].trim();
        if line.is_empty() || line.starts_with("%%") {
            i += 1;
            continue;
        }

        if line.split_whitespace().next() == Some("journey") {
            i += 1;
            break;
        }

        return Err(MermaidError::ParseError {
            line: i + 1,
            message: "Expected 'journey' declaration".to_string(),
        });
    }

    let mut title: Option<String> = None;
    let mut rows: Vec<JourneyRow> = Vec::new();

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

        if let Some(rest) = line.strip_prefix("section ") {
            let name = rest.trim();
            if !name.is_empty() {
                rows.push(JourneyRow::Section(name.to_string()));
            }
            continue;
        }

        let parts: Vec<&str> = line
            .split(':')
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
            .collect();
        if parts.len() < 2 {
            return Err(MermaidError::ParseError {
                line: line_no,
                message: format!("Invalid journey task line: {line}"),
            });
        }

        let name = parts[0].to_string();
        let score: i32 = parts[1].parse().map_err(|_| MermaidError::ParseError {
            line: line_no,
            message: format!("Invalid journey score: {line}"),
        })?;

        let actors: Vec<String> = if parts.len() >= 3 {
            // Actors field may contain comma-separated names (e.g., "Alice, Bob")
            // Join remaining parts (in case actor names contain colons) then split by comma
            parts[2..]
                .join(": ")
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        } else {
            Vec::new()
        };

        rows.push(JourneyRow::Task(JourneyTask {
            name,
            score,
            actors,
        }));
    }

    if rows.is_empty() {
        return Err(MermaidError::ParseError {
            line: 1,
            message: "Journey requires at least one section/task".to_string(),
        });
    }

    Ok(JourneyDiagram { title, rows })
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
