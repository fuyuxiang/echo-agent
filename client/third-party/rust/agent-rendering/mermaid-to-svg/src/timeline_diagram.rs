use crate::error::MermaidError;
use crate::theme::MermaidTheme;

// --- Mermaid 11.12.2 timeline layout constants ---
const LEFT_MARGIN: f64 = 50.0;
const INITIAL_MASTER_X: f64 = 50.0 + LEFT_MARGIN; // 100
const INITIAL_MASTER_Y: f64 = 50.0;
const NODE_BASE_WIDTH: f64 = 150.0;
const NODE_PADDING: f64 = 20.0;
const NODE_WIDTH: f64 = NODE_BASE_WIDTH + 2.0 * NODE_PADDING; // 190
const NODE_STEP: f64 = 200.0;
const FONT_SIZE: f64 = 16.0;
const EVENT_VERTICAL_GAP: f64 = 100.0;
const DASHED_LINE_EXTENSION: f64 = 100.0;
const NODE_CORNER_RADIUS: f64 = 5.0;
const MAX_SECTIONS: usize = 12;
const ARROW_STROKE_WIDTH: f64 = 4.0;
const CONNECTOR_STROKE_WIDTH: f64 = 2.0;
const NODE_LINE_STROKE_WIDTH: f64 = 3.0;

const FONT_FAMILY: &str = r#""trebuchet ms", verdana, arial, sans-serif"#;
const TASK_FONT_SIZE: f64 = 14.0;
const TASK_FONT_FAMILY: &str = "'Open Sans', sans-serif";

// Approximate character width for text measurement at 16px
const CHAR_WIDTH: f64 = 9.0;

// Mermaid 11.12.2 default theme cScale colors (after darken by 10)
const CSCALE_FILLS: &[&str] = &[
    "#BABAFF", // cScale0: periwinkle (primaryColor #ECECFF)
    "#FFFFAC", // cScale1: yellow (secondaryColor #ffffde)
    "#E8FFB9", // cScale2: lime green (tertiaryColor)
    "#D4BAFF", // cScale3
    "#FFBAFF", // cScale4
    "#FFBADC", // cScale5
    "#BAFFBA", // cScale6
    "#BAFFDC", // cScale7
    "#BAFFFF", // cScale8
    "#BABAFF", // cScale9
    "#DCBAFF", // cScale10
    "#FFBAEF", // cScale11
];

// cScaleInv = hue-shifted by 180° from cScale (for node bottom line stroke)
const CSCALE_INV: &[&str] = &[
    "#FFFFAC", "#BABAFF", "#FFB9E8", "#BAFFD4", "#BAFF9A", "#BAFFDC", "#FFBA9A", "#FFBADC",
    "#FFBABA", "#FFFFBA", "#BAFFBA", "#BAFFE0",
];

// cScaleLabel text colors (cScaleLabel0 and cScaleLabel3 = white, rest = black)
const CSCALE_LABEL: &[&str] = &[
    "#ffffff", "#000000", "#000000", "#ffffff", "#000000", "#000000", "#000000", "#000000",
    "#000000", "#000000", "#000000", "#000000",
];

pub fn render_timeline_diagram_to_svg(
    mermaid_source: &str,
    _theme: &MermaidTheme,
) -> Result<String, MermaidError> {
    let timeline = parse_timeline_diagram(mermaid_source)?;

    let has_sections = !timeline.sections.is_empty();
    let tasks = &timeline.tasks;
    if tasks.is_empty() {
        return Err(MermaidError::ParseError {
            line: 1,
            message: "Timeline requires at least one entry".to_string(),
        });
    }

    // --- Compute layout metrics ---
    let mut max_section_height = 0.0_f64;
    if has_sections {
        for section_name in &timeline.sections {
            let h = estimate_node_height(section_name, NODE_PADDING, 0.0);
            max_section_height = max_section_height.max(h + 20.0);
        }
    }

    let mut max_task_height = 0.0_f64;
    let mut max_event_line_length = 0.0_f64;

    for task in tasks {
        let h = estimate_node_height(&task.period, NODE_PADDING, 0.0);
        max_task_height = max_task_height.max(h + 20.0);

        let mut event_line_len = 0.0_f64;
        for event in &task.events {
            event_line_len += estimate_node_height(event, NODE_PADDING, 50.0);
        }
        if task.events.len() > 1 {
            event_line_len += (task.events.len() - 1) as f64 * 10.0;
        }
        max_event_line_length = max_event_line_length.max(event_line_len);
    }

    // --- Build SVG body ---
    let mut svg = String::with_capacity(4096);
    let mut content_right = 0.0_f64;
    let mut content_bottom = 0.0_f64;

    // CSS styles matching mermaid.js timeline default theme
    let mut css = String::new();
    css.push_str(&format!(
        "svg{{font-family:{FONT_FAMILY};font-size:{FONT_SIZE}px;fill:#333;}}"
    ));
    for i in 0..MAX_SECTIONS {
        let si = i as isize - 1;
        let fill = CSCALE_FILLS[i % CSCALE_FILLS.len()];
        let label = CSCALE_LABEL[i % CSCALE_LABEL.len()];
        let inv = CSCALE_INV[i % CSCALE_INV.len()];
        css.push_str(&format!(
            ".section-{si} rect,.section-{si} path,.section-{si} circle{{fill:{fill};}}"
        ));
        css.push_str(&format!(".section-{si} text{{fill:{label};}}"));
        css.push_str(&format!(
            ".section-{si} line{{stroke:{inv};stroke-width:{NODE_LINE_STROKE_WIDTH};}}"
        ));
    }
    css.push_str(".eventWrapper{filter:brightness(120%);}");
    css.push_str(".lineWrapper line{stroke:black;}");

    let defs = "<defs><marker id=\"arrowhead\" refX=\"5\" refY=\"2\" markerWidth=\"6\" \
                markerHeight=\"4\" orient=\"auto\"><path d=\"M 0,0 V 4 L6,2 Z\"/></marker></defs>";

    let mut body = String::new();

    // --- Draw tasks and events ---
    let mut master_x = INITIAL_MASTER_X;
    let master_y = INITIAL_MASTER_Y;
    let section_begin_y = INITIAL_MASTER_Y;
    let mut section_number: usize = 0;

    if has_sections {
        for section_name in &timeline.sections {
            let tasks_for_section: Vec<&TimelineTask> = tasks
                .iter()
                .filter(|t| t.section.as_deref() == Some(section_name.as_str()))
                .collect();

            let section_width = 200.0 * (tasks_for_section.len().max(1)) as f64 - 50.0;
            let section_idx = section_number % MAX_SECTIONS;
            let section_css_idx = section_idx as isize - 1;

            body.push_str(&format!(
                "<g class=\"timeline-node section-{section_css_idx}\" \
                 transform=\"translate({master_x},{section_begin_y})\">"
            ));
            render_node_background(&mut body, section_width, max_section_height);
            render_node_text(&mut body, section_name, section_width);
            body.push_str("</g>");

            let task_y = section_begin_y + max_section_height + 50.0;

            if !tasks_for_section.is_empty() {
                render_tasks(
                    &mut body,
                    &tasks_for_section,
                    section_number,
                    &mut master_x,
                    task_y,
                    max_task_height,
                    max_event_line_length,
                    &mut content_right,
                    &mut content_bottom,
                    false,
                );
            }

            master_x += 200.0 * (tasks_for_section.len().max(1)) as f64;
            section_number += 1;
        }
    } else {
        let task_refs: Vec<&TimelineTask> = tasks.iter().collect();
        render_tasks(
            &mut body,
            &task_refs,
            section_number,
            &mut master_x,
            master_y,
            max_task_height,
            max_event_line_length,
            &mut content_right,
            &mut content_bottom,
            true,
        );
    }

    // --- Horizontal arrow ---
    let depth_y = if has_sections {
        max_section_height + max_task_height + 150.0
    } else {
        max_task_height + 100.0
    };

    // In mermaid.js, box.width is computed from SVG bounding box BEFORE arrow/title
    let nodes_box_width = content_right;

    let arrow_x1 = LEFT_MARGIN;
    let arrow_x2 = nodes_box_width + 3.0 * LEFT_MARGIN;
    content_right = content_right.max(arrow_x2 + 10.0);

    body.push_str(&format!(
        "<g class=\"lineWrapper\"><line x1=\"{arrow_x1:.1}\" y1=\"{depth_y:.1}\" \
         x2=\"{arrow_x2:.1}\" y2=\"{depth_y:.1}\" \
         stroke-width=\"{ARROW_STROKE_WIDTH}\" stroke=\"black\" \
         marker-end=\"url(#arrowhead)\"/></g>"
    ));

    // --- Title ---
    // Position uses node bounding box width (before arrow), matching mermaid.js
    let title_content = if let Some(title) = &timeline.title {
        let title_x = nodes_box_width / 2.0 - LEFT_MARGIN;
        // Estimate title width to expand viewBox if needed
        let approx_title_width = title.len() as f64 * 24.0; // ~24px/char at 4ex
        content_right = content_right.max(title_x + approx_title_width + 20.0);
        format!(
            "<text x=\"{title_x:.1}\" y=\"20\" font-size=\"4ex\" \
             font-weight=\"bold\" fill=\"#333\">{}</text>",
            escape_xml(title)
        )
    } else {
        String::new()
    };

    content_bottom = content_bottom.max(depth_y + 20.0);

    // --- Assemble final SVG ---
    let vb_padding = 50.0;
    let vb_width = content_right + vb_padding;
    let vb_height = content_bottom + vb_padding;

    svg.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" \
         xmlns:xlink=\"http://www.w3.org/1999/xlink\" \
         style=\"max-width: {vb_width:.0}px;\" \
         width=\"100%\" \
         viewBox=\"0 -25 {vb_width:.0} {vh:.0}\" \
         preserveAspectRatio=\"xMinYMin meet\" \
         height=\"{sh:.0}\" \
         role=\"graphics-document document\" \
         aria-roledescription=\"timeline\">",
        vh = vb_height + 25.0,
        sh = vb_height + 50.0,
    ));
    svg.push_str(&format!("<style>{css}</style>"));
    svg.push_str(defs);
    svg.push_str(&title_content);
    svg.push_str(&body);
    svg.push_str("</svg>");

    Ok(svg)
}

#[allow(clippy::too_many_arguments)]
fn render_tasks(
    body: &mut String,
    tasks: &[&TimelineTask],
    initial_section_color: usize,
    master_x: &mut f64,
    master_y: f64,
    max_task_height: f64,
    max_event_line_length: f64,
    content_right: &mut f64,
    content_bottom: &mut f64,
    is_multicolor: bool,
) {
    let mut section_color = initial_section_color;

    for task in tasks {
        let section_idx = section_color % MAX_SECTIONS;
        let section_css_idx = section_idx as isize - 1;

        // Draw task (period) node
        body.push_str(&format!(
            "<g class=\"taskWrapper\"><g class=\"timeline-node section-{section_css_idx}\" \
             transform=\"translate({mx},{my})\">",
            mx = *master_x,
            my = master_y,
        ));
        render_node_background(body, NODE_WIDTH, max_task_height);
        render_node_text(body, &task.period, NODE_WIDTH);
        body.push_str("</g></g>");

        *content_right = (*content_right).max(*master_x + NODE_WIDTH);

        // Draw events below
        if !task.events.is_empty() {
            let mut event_y = master_y + EVENT_VERTICAL_GAP + EVENT_VERTICAL_GAP;

            for event in &task.events {
                let event_height = estimate_event_height(event);

                body.push_str(&format!(
                    "<g class=\"eventWrapper\"><g class=\"timeline-node section-{section_css_idx}\" \
                     transform=\"translate({mx},{ey})\">",
                    mx = *master_x,
                    ey = event_y,
                ));
                render_node_background(body, NODE_WIDTH, event_height);
                render_node_text(body, event, NODE_WIDTH);
                body.push_str("</g></g>");

                event_y += event_height + 10.0;
            }

            // Dashed vertical connector line with arrowhead
            let line_x = *master_x + NODE_WIDTH / 2.0;
            let line_y1 = master_y + max_task_height;
            let line_y2 = master_y
                + max_task_height
                + EVENT_VERTICAL_GAP
                + max_event_line_length
                + DASHED_LINE_EXTENSION;

            body.push_str(&format!(
                "<g class=\"lineWrapper\"><line x1=\"{line_x:.1}\" y1=\"{line_y1:.1}\" \
                 x2=\"{line_x:.1}\" y2=\"{line_y2:.1}\" \
                 stroke-width=\"{CONNECTOR_STROKE_WIDTH}\" stroke=\"black\" \
                 marker-end=\"url(#arrowhead)\" stroke-dasharray=\"5,5\"/></g>"
            ));

            *content_bottom = (*content_bottom).max(line_y2 + 10.0);
        }

        *master_x += NODE_STEP;
        if is_multicolor {
            section_color += 1;
        }
    }
}

fn estimate_text_height(text: &str) -> f64 {
    let text_width = text.len() as f64 * CHAR_WIDTH;
    let num_lines = (text_width / NODE_BASE_WIDTH).ceil().max(1.0);
    num_lines * FONT_SIZE * 1.2
}

fn estimate_node_height(text: &str, padding: f64, max_height: f64) -> f64 {
    let text_h = estimate_text_height(text);
    let h = text_h + FONT_SIZE * 1.1 * 0.5 + padding;
    h.max(max_height)
}

fn estimate_event_height(text: &str) -> f64 {
    let text_h = estimate_text_height(text);
    let h = text_h + FONT_SIZE * 1.1 * 0.5 + NODE_PADDING;
    h.max(50.0)
}

/// Render the node background shape: rounded top corners, flat bottom with a line.
/// Matches mermaid.js `defaultBkg` function.
fn render_node_background(svg: &mut String, width: f64, height: f64) {
    let rd = NODE_CORNER_RADIUS;
    svg.push_str(&format!(
        "<g><path class=\"node-bkg\" d=\"M0 {h_rd:.1} v{up:.1} q0,-{rd} {rd},-{rd} \
         h{across:.1} q{rd},0 {rd},{rd} v{down:.1} H0 Z\"/>",
        h_rd = height - rd,
        up = -(height - 2.0 * rd),
        rd = rd,
        across = width - 2.0 * rd,
        down = height - rd,
    ));
    svg.push_str(&format!(
        "<line x1=\"0\" y1=\"{height:.1}\" x2=\"{width:.1}\" y2=\"{height:.1}\"/>"
    ));
    svg.push_str("</g>");
}

/// Render centered text inside a node.
fn render_node_text(svg: &mut String, text: &str, width: f64) {
    let x = width / 2.0;
    svg.push_str(&format!(
        "<g transform=\"translate({x:.1},{ty:.1})\">\
         <text x=\"0\" y=\"0\" dy=\"1em\" \
         alignment-baseline=\"middle\" dominant-baseline=\"middle\" \
         text-anchor=\"middle\" \
         style=\"font-size:{TASK_FONT_SIZE}px;font-family:{TASK_FONT_FAMILY};\">\
         {}</text></g>",
        escape_xml(text),
        ty = NODE_PADDING / 2.0,
    ));
}

// --- Data model ---

#[derive(Debug, Clone)]
struct TimelineDiagram {
    title: Option<String>,
    sections: Vec<String>,
    tasks: Vec<TimelineTask>,
}

#[derive(Debug, Clone)]
struct TimelineTask {
    period: String,
    events: Vec<String>,
    section: Option<String>,
}

// --- Parser ---

fn parse_timeline_diagram(input: &str) -> Result<TimelineDiagram, MermaidError> {
    let lines: Vec<&str> = input.lines().collect();

    let mut i = 0_usize;
    while i < lines.len() {
        let line = lines[i].trim();
        if line.is_empty() || line.starts_with("%%") {
            i += 1;
            continue;
        }

        if line.split_whitespace().next() == Some("timeline") {
            i += 1;
            break;
        }

        return Err(MermaidError::ParseError {
            line: i + 1,
            message: "Expected 'timeline' declaration".to_string(),
        });
    }

    let mut title: Option<String> = None;
    let mut sections: Vec<String> = Vec::new();
    let mut tasks: Vec<TimelineTask> = Vec::new();
    let mut current_section: Option<String> = None;

    while i < lines.len() {
        let raw = lines[i];
        let line = raw.trim();
        i += 1;

        if line.is_empty() || line.starts_with("%%") || line.starts_with('#') {
            continue;
        }

        // Title directive
        if let Some(rest) = line.strip_prefix("title ") {
            let t = rest.trim();
            if !t.is_empty() {
                title = Some(t.to_string());
            }
            continue;
        }

        // Section directive
        if let Some(rest) = line.strip_prefix("section ") {
            let s = rest.trim();
            if !s.is_empty() {
                current_section = Some(s.to_string());
                if !sections.contains(&s.to_string()) {
                    sections.push(s.to_string());
                }
            }
            continue;
        }

        // Event line (starts with ": " — additional event for the previous task)
        if let Some(event_text) = line.strip_prefix(": ") {
            let event_text = event_text.trim();
            if !event_text.is_empty() {
                if let Some(last_task) = tasks.last_mut() {
                    last_task.events.push(event_text.to_string());
                }
            }
            continue;
        }

        // Period with optional event: "period : event" or just "period"
        if let Some((period, event)) = line.split_once(':') {
            let period = period.trim();
            let event = event.trim();
            if !period.is_empty() {
                let events = if event.is_empty() {
                    vec![]
                } else {
                    vec![event.to_string()]
                };
                tasks.push(TimelineTask {
                    period: period.to_string(),
                    events,
                    section: current_section.clone(),
                });
                continue;
            }
        }

        // Period without event (bare text line)
        if !line.is_empty() {
            tasks.push(TimelineTask {
                period: line.to_string(),
                events: vec![],
                section: current_section.clone(),
            });
        }
    }

    Ok(TimelineDiagram {
        title,
        sections,
        tasks,
    })
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
