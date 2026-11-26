use crate::error::MermaidError;
use crate::theme::MermaidTheme;

use std::f64::consts::PI;

/// Mermaid 11.12.2 default pie chart colors (from the default theme).
/// pie1 = primaryColor (#ECECFF), pie2 = secondaryColor (#ffffde),
/// pie3–pie12 computed via adjust/darken on primary/secondary/tertiary.
const MERMAID_PIE_COLORS: [&str; 12] = [
    "#ECECFF",                        // pie1  – primaryColor
    "#ffffde",                        // pie2  – secondaryColor
    "hsl(80, 100%, 56.2745098039%)",  // pie3  – adjust(tertiaryColor, l:-40)
    "hsl(240, 60%, 86.2745098039%)",  // pie4  – adjust(primaryColor, l:-10)
    "hsl(120, 100%, 66.2745098039%)", // pie5  – adjust(secondaryColor, l:-30)
    "hsl(80, 100%, 76.2745098039%)",  // pie6  – adjust(tertiaryColor, l:-20)
    "hsl(300, 60%, 76.2745098039%)",  // pie7  – adjust(primaryColor, h:60, l:-20)
    "hsl(180, 60%, 56.2745098039%)",  // pie8  – adjust(primaryColor, h:-60, l:-40)
    "hsl(0, 60%, 56.2745098039%)",    // pie9  – adjust(primaryColor, h:120, l:-40)
    "hsl(300, 60%, 56.2745098039%)",  // pie10 – adjust(primaryColor, h:60, l:-40)
    "hsl(150, 60%, 56.2745098039%)",  // pie11 – adjust(primaryColor, h:-90, l:-40)
    "hsl(0, 60%, 66.2745098039%)",    // pie12 – adjust(primaryColor, h:120, l:-30)
];

// Mermaid 11.12.2 pie chart constants (from pieRenderer.ts and default config).
const PIE_HEIGHT: f64 = 450.0;
const PIE_WIDTH: f64 = 450.0;
const MARGIN: f64 = 40.0;
const RADIUS: f64 = (PIE_WIDTH / 2.0) - MARGIN; // 185
const OUTER_STROKE_WIDTH: f64 = 2.0;
const OUTER_RADIUS: f64 = RADIUS + OUTER_STROKE_WIDTH / 2.0; // 186
const TEXT_POSITION: f64 = 0.75;
const LEGEND_RECT_SIZE: f64 = 18.0;
const LEGEND_SPACING: f64 = 4.0;

const FONT_FAMILY: &str = "trebuchet ms,verdana,arial,sans-serif";

pub fn render_pie_diagram_to_svg(
    mermaid_source: &str,
    _theme: &MermaidTheme,
) -> Result<String, MermaidError> {
    let chart = parse_pie_diagram(mermaid_source)?;

    let total: f64 = chart.slices.iter().map(|s| s.value).sum();
    if total <= 0.0 {
        return Err(MermaidError::ParseError {
            line: 1,
            message: "Pie diagram total must be > 0".to_string(),
        });
    }

    // Filter slices ≥1% and sort descending by value (matches d3.pie() default).
    let mut slices: Vec<&PieSlice> = chart
        .slices
        .iter()
        .filter(|s| s.value / total * 100.0 >= 1.0)
        .collect();
    slices.sort_by(|a, b| b.value.partial_cmp(&a.value).unwrap());

    // All slices for the legend (unfiltered, original order).
    let all_slices: Vec<&PieSlice> = chart.slices.iter().collect();

    // Center of the pie in the translated group coordinate system is (0, 0).
    let cx = PIE_WIDTH / 2.0;
    let cy = PIE_HEIGHT / 2.0;

    // Estimate legend text width (rough: 10px per char at 17px font).
    let longest_label_len = all_slices
        .iter()
        .map(|s| {
            if chart.show_data {
                format!("{} [{}]", s.label, s.value).len()
            } else {
                s.label.len()
            }
        })
        .max()
        .unwrap_or(0);
    let legend_text_width = longest_label_len as f64 * 10.0;
    let total_width = PIE_WIDTH + MARGIN + LEGEND_RECT_SIZE + LEGEND_SPACING + legend_text_width;

    let mut svg = String::new();

    // Mermaid uses a CSS style block for pie chart classes.
    svg.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {total_width:.4} {PIE_HEIGHT}\" \
         style=\"max-width: {total_width:.3}px; background-color: white;\" \
         role=\"graphics-document document\" aria-roledescription=\"pie\">"
    ));

    // Inline CSS matching Mermaid 11.12.2 pieStyles.
    svg.push_str(&format!(
        "<style>\
         .pieCircle{{stroke:black;stroke-width:2px;opacity:0.7;}}\
         .pieOuterCircle{{stroke:black;stroke-width:2px;fill:none;}}\
         .pieTitleText{{text-anchor:middle;font-size:25px;fill:black;font-family:{FONT_FAMILY};}}\
         .slice{{font-family:{FONT_FAMILY};fill:#333;font-size:17px;}}\
         .legend text{{fill:black;font-family:{FONT_FAMILY};font-size:17px;}}\
         </style>"
    ));

    // Group translated to pie center (matches mermaid: translate(pieWidth/2, height/2)).
    svg.push_str(&format!("<g transform=\"translate({cx},{cy})\">"));

    // Outer circle.
    svg.push_str(&format!(
        "<circle cx=\"0\" cy=\"0\" r=\"{OUTER_RADIUS}\" class=\"pieOuterCircle\"/>"
    ));

    // Draw pie slices.
    // d3.pie() default: startAngle=0 (12 o'clock), endAngle=2π, clockwise.
    // In SVG with translate to center, angle 0 points up (-y).
    let mut angle = -PI / 2.0;
    for (idx, slice) in slices.iter().enumerate() {
        let pct = (slice.value / total * 100.0).round() as i64;
        if pct == 0 {
            continue;
        }

        let frac = slice.value / total;
        let sweep = frac * 2.0 * PI;
        let next = angle + sweep;

        let (x0, y0) = polar(0.0, 0.0, RADIUS, angle);
        let (x1, y1) = polar(0.0, 0.0, RADIUS, next);
        let large_arc = if sweep > PI { 1 } else { 0 };

        let fill = MERMAID_PIE_COLORS[idx % MERMAID_PIE_COLORS.len()];

        // Slice path.
        svg.push_str(&format!(
            "<path d=\"M0,0L{x0:.3},{y0:.3}A{RADIUS},{RADIUS},0,{large_arc},1,{x1:.3},{y1:.3}Z\" \
             fill=\"{fill}\" class=\"pieCircle\"/>"
        ));

        // Percentage label inside the slice at textPosition (0.75) of radius.
        let label_r = RADIUS * TEXT_POSITION;
        let mid = angle + sweep / 2.0;
        let (lx, ly) = polar(0.0, 0.0, label_r, mid);
        svg.push_str(&format!(
            "<text transform=\"translate({lx:.3},{ly:.3})\" class=\"slice\" \
             style=\"text-anchor: middle;\">{pct}%</text>"
        ));

        angle = next;
    }

    // Title (positioned above the pie).
    if let Some(title) = &chart.title {
        let title_y = -((PIE_HEIGHT - 50.0) / 2.0);
        svg.push_str(&format!(
            "<text x=\"0\" y=\"{title_y:.0}\" class=\"pieTitleText\">{}</text>",
            escape_xml(title)
        ));
    }

    // Legend (to the right of the pie).
    let legend_h = LEGEND_RECT_SIZE + LEGEND_SPACING;
    let legend_offset = legend_h * all_slices.len() as f64 / 2.0;
    let legend_x = 12.0 * LEGEND_RECT_SIZE; // 216

    // Build a color map that assigns colors to labels in the same order as the
    // sorted/filtered slices (matching d3.scaleOrdinal behavior).
    let mut color_map: Vec<(&str, &str)> = Vec::new();
    for (idx, slice) in slices.iter().enumerate() {
        color_map.push((
            &slice.label,
            MERMAID_PIE_COLORS[idx % MERMAID_PIE_COLORS.len()],
        ));
    }

    for (legend_idx, slice) in all_slices.iter().enumerate() {
        let vert = legend_idx as f64 * legend_h - legend_offset;
        let color = color_map
            .iter()
            .find(|(label, _)| *label == slice.label)
            .map(|(_, c)| *c)
            .unwrap_or(MERMAID_PIE_COLORS[legend_idx % MERMAID_PIE_COLORS.len()]);

        svg.push_str(&format!(
            "<g class=\"legend\" transform=\"translate({legend_x},{vert})\">"
        ));
        svg.push_str(&format!(
            "<rect width=\"{LEGEND_RECT_SIZE}\" height=\"{LEGEND_RECT_SIZE}\" \
             style=\"fill: {color}; stroke: {color};\"/>"
        ));

        let label_text = if chart.show_data {
            format!("{} [{}]", slice.label, slice.value)
        } else {
            slice.label.clone()
        };
        let text_x = LEGEND_RECT_SIZE + LEGEND_SPACING;
        let text_y = LEGEND_RECT_SIZE - LEGEND_SPACING;
        svg.push_str(&format!(
            "<text x=\"{text_x}\" y=\"{text_y}\">{}</text>",
            escape_xml(&label_text)
        ));
        svg.push_str("</g>");
    }

    svg.push_str("</g>"); // close main group
    svg.push_str("</svg>");
    Ok(svg)
}

#[derive(Debug, Clone)]
struct PieChart {
    title: Option<String>,
    show_data: bool,
    slices: Vec<PieSlice>,
}

#[derive(Debug, Clone)]
struct PieSlice {
    label: String,
    value: f64,
}

fn parse_pie_diagram(input: &str) -> Result<PieChart, MermaidError> {
    let lines: Vec<&str> = input.lines().collect();

    let mut i = 0_usize;
    let mut show_data = false;
    while i < lines.len() {
        let line = lines[i].trim();
        if line.is_empty() || line.starts_with("%%") {
            i += 1;
            continue;
        }
        let mut tokens = line.split_whitespace();
        let first = tokens.next().unwrap_or("");
        if first == "pie" {
            show_data = tokens.any(|t| t == "showData");
            i += 1;
            break;
        }
        return Err(MermaidError::ParseError {
            line: i + 1,
            message: "Expected 'pie' declaration".to_string(),
        });
    }

    let mut title: Option<String> = None;
    let mut slices: Vec<PieSlice> = Vec::new();

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

        let Some((label_raw, value_raw)) = line.split_once(':') else {
            return Err(MermaidError::ParseError {
                line: line_no,
                message: format!("Invalid pie slice: {line}"),
            });
        };

        let mut label = label_raw.trim().to_string();
        if let Some(stripped) = label.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
            label = stripped.to_string();
        }

        let value_str = value_raw.trim();
        let value: f64 = value_str.parse().map_err(|_| MermaidError::ParseError {
            line: line_no,
            message: format!("Invalid pie value: {value_str}"),
        })?;

        slices.push(PieSlice { label, value });
    }

    if slices.is_empty() {
        return Err(MermaidError::ParseError {
            line: 1,
            message: "Pie diagram requires at least one slice".to_string(),
        });
    }

    Ok(PieChart {
        title,
        show_data,
        slices,
    })
}

fn polar(cx: f64, cy: f64, r: f64, angle: f64) -> (f64, f64) {
    (cx + r * angle.cos(), cy + r * angle.sin())
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
