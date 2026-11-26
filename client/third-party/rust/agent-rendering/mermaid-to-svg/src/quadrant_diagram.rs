use crate::error::MermaidError;
use crate::theme::MermaidTheme;

use std::collections::BTreeMap;

/// Quadrant chart theme colors, derived from the mermaid.js default theme.
/// Mermaid uses `primaryColor = "#ECECFF"` and derives fills by adjusting RGB channels.
struct QuadrantTheme<'a> {
    quadrant1_fill: &'a str,
    quadrant2_fill: &'a str,
    quadrant3_fill: &'a str,
    quadrant4_fill: &'a str,
    border_stroke: &'a str,
    title_fill: &'a str,
    axis_text_fill: &'a str,
    point_fill: &'a str,
    point_text_fill: &'a str,
    quadrant_text_fill: &'a str,
}

fn quadrant_theme_for(theme: &MermaidTheme) -> QuadrantTheme<'static> {
    let is_dark = theme.background.starts_with("#1") || theme.background.starts_with("#0");
    if is_dark {
        QuadrantTheme {
            quadrant1_fill: "#1f2020",
            quadrant2_fill: "#242525",
            quadrant3_fill: "#292a2a",
            quadrant4_fill: "#2e2f2f",
            border_stroke: "#e0dfdf",
            title_fill: "#ccc",
            axis_text_fill: "#ccc",
            point_fill: "#ccc",
            point_text_fill: "#ccc",
            quadrant_text_fill: "#ccc",
        }
    } else {
        // Default (light) theme: primaryColor = "#ECECFF"
        // quadrant fills = primaryColor + adjust({r: N, g: N, b: N}) for N in 0,5,10,15
        // border = mkBorder("#ECECFF", false) = adjust("#ECECFF", {s:-40, l:-10}) = #C7C7F1
        // point fill = darken("#ECECFF") ≈ #333333 (text color in practice)
        QuadrantTheme {
            quadrant1_fill: "#ECECFF",
            quadrant2_fill: "#F1F1FF",
            quadrant3_fill: "#F6F6FF",
            quadrant4_fill: "#FBFBFF",
            border_stroke: "#C7C7F1",
            title_fill: "#333333",
            axis_text_fill: "#333333",
            point_fill: "#333333",
            point_text_fill: "#333333",
            quadrant_text_fill: "#333333",
        }
    }
}

/// Mermaid.js default config values for quadrant charts.
const CHART_WIDTH: f64 = 500.0;
const CHART_HEIGHT: f64 = 500.0;
const TITLE_FONT_SIZE: f64 = 20.0;
const TITLE_PADDING: f64 = 10.0;
const QUADRANT_PADDING: f64 = 5.0;
const X_AXIS_LABEL_PADDING: f64 = 5.0;
const Y_AXIS_LABEL_PADDING: f64 = 5.0;
const X_AXIS_LABEL_FONT_SIZE: f64 = 16.0;
const Y_AXIS_LABEL_FONT_SIZE: f64 = 16.0;
const QUADRANT_LABEL_FONT_SIZE: f64 = 16.0;
const QUADRANT_TEXT_TOP_PADDING: f64 = 5.0;
const POINT_TEXT_PADDING: f64 = 5.0;
const POINT_LABEL_FONT_SIZE: f64 = 12.0;
const POINT_RADIUS: f64 = 5.0;
const INTERNAL_BORDER_STROKE_WIDTH: f64 = 1.0;
const EXTERNAL_BORDER_STROKE_WIDTH: f64 = 2.0;

const FONT_FAMILY: &str = "trebuchet ms,verdana,arial,sans-serif";

pub fn render_quadrant_chart_to_svg(
    mermaid_source: &str,
    theme: &MermaidTheme,
) -> Result<String, MermaidError> {
    let chart = parse_quadrant_chart(mermaid_source)?;
    let qt = quadrant_theme_for(theme);

    let has_points = !chart.points.is_empty();
    let show_title = chart.title.is_some();
    let show_x_axis = chart.x_axis.is_some();
    let show_y_axis = chart.y_axis.is_some();

    // x-axis goes to bottom when points exist, top otherwise
    let x_axis_bottom = has_points;

    // Space calculations (matches mermaid.js QuadrantBuilder.calculateSpace)
    let x_axis_space = if show_x_axis {
        X_AXIS_LABEL_PADDING * 2.0 + X_AXIS_LABEL_FONT_SIZE
    } else {
        0.0
    };
    let y_axis_space_left = if show_y_axis {
        Y_AXIS_LABEL_PADDING * 2.0 + Y_AXIS_LABEL_FONT_SIZE
    } else {
        0.0
    };
    let title_space_top = if show_title {
        TITLE_FONT_SIZE + TITLE_PADDING * 2.0
    } else {
        0.0
    };

    let x_axis_top = if !x_axis_bottom { x_axis_space } else { 0.0 };
    let x_axis_bot = if x_axis_bottom { x_axis_space } else { 0.0 };

    let quadrant_left = QUADRANT_PADDING + y_axis_space_left;
    let quadrant_top = QUADRANT_PADDING + x_axis_top + title_space_top;
    let quadrant_width = CHART_WIDTH - QUADRANT_PADDING * 2.0 - y_axis_space_left;
    let quadrant_height =
        CHART_HEIGHT - QUADRANT_PADDING * 2.0 - x_axis_top - x_axis_bot - title_space_top;
    let half_w = quadrant_width / 2.0;
    let half_h = quadrant_height / 2.0;
    let half_ext = EXTERNAL_BORDER_STROKE_WIDTH / 2.0;

    let mut svg = String::new();
    svg.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {CHART_WIDTH} {CHART_HEIGHT}\">"
    ));
    svg.push_str(&format!(
        "<rect x=\"0\" y=\"0\" width=\"{CHART_WIDTH}\" height=\"{CHART_HEIGHT}\" fill=\"{}\"/>",
        theme.background
    ));

    // --- Quadrant fill rects (draw FIRST, behind everything else) ---
    let q1_text = chart.quadrants.get(&1).cloned().unwrap_or_default();
    let q2_text = chart.quadrants.get(&2).cloned().unwrap_or_default();
    let q3_text = chart.quadrants.get(&3).cloned().unwrap_or_default();
    let q4_text = chart.quadrants.get(&4).cloned().unwrap_or_default();

    // Mermaid quadrant layout:
    // Q1 = top-right, Q2 = top-left, Q3 = bottom-left, Q4 = bottom-right
    let quadrant_rects: [(f64, f64, &str); 4] = [
        (quadrant_left + half_w, quadrant_top, qt.quadrant1_fill), // Q1 top-right
        (quadrant_left, quadrant_top, qt.quadrant2_fill),          // Q2 top-left
        (quadrant_left, quadrant_top + half_h, qt.quadrant3_fill), // Q3 bottom-left
        (
            quadrant_left + half_w,
            quadrant_top + half_h,
            qt.quadrant4_fill,
        ), // Q4 bottom-right
    ];
    for (rx, ry, fill) in &quadrant_rects {
        svg.push_str(&format!(
            "<rect x=\"{rx:.1}\" y=\"{ry:.1}\" width=\"{half_w:.1}\" height=\"{half_h:.1}\" fill=\"{fill}\"/>"
        ));
    }

    // --- Quadrant labels ---
    let quadrant_labels: [(&str, f64, f64); 4] = [
        (
            &q1_text,
            quadrant_left + half_w + half_w / 2.0,
            quadrant_top,
        ),
        (&q2_text, quadrant_left + half_w / 2.0, quadrant_top),
        (
            &q3_text,
            quadrant_left + half_w / 2.0,
            quadrant_top + half_h,
        ),
        (
            &q4_text,
            quadrant_left + half_w + half_w / 2.0,
            quadrant_top + half_h,
        ),
    ];
    for (text, tx, ty_base) in &quadrant_labels {
        if text.is_empty() {
            continue;
        }
        // When points exist, labels go to top of quadrant; otherwise center
        let ty = if has_points {
            ty_base + QUADRANT_TEXT_TOP_PADDING
        } else {
            ty_base + half_h / 2.0
        };
        let dominant_baseline = if has_points { "hanging" } else { "middle" };
        svg.push_str(&format!(
            "<text x=\"{tx:.1}\" y=\"{ty:.1}\" text-anchor=\"middle\" dominant-baseline=\"{dominant_baseline}\" \
             font-family=\"{FONT_FAMILY}\" font-size=\"{QUADRANT_LABEL_FONT_SIZE}\" \
             fill=\"{fill}\">{}</text>",
            escape_xml(text),
            fill = qt.quadrant_text_fill
        ));
    }

    // --- Border lines (external + internal, all solid) ---
    // External border: 4 lines forming the outer rectangle
    let ext_lines: [(f64, f64, f64, f64); 4] = [
        // top
        (
            quadrant_left - half_ext,
            quadrant_top,
            quadrant_left + quadrant_width + half_ext,
            quadrant_top,
        ),
        // right
        (
            quadrant_left + quadrant_width,
            quadrant_top + half_ext,
            quadrant_left + quadrant_width,
            quadrant_top + quadrant_height - half_ext,
        ),
        // bottom
        (
            quadrant_left - half_ext,
            quadrant_top + quadrant_height,
            quadrant_left + quadrant_width + half_ext,
            quadrant_top + quadrant_height,
        ),
        // left
        (
            quadrant_left,
            quadrant_top + half_ext,
            quadrant_left,
            quadrant_top + quadrant_height - half_ext,
        ),
    ];
    for (x1, y1, x2, y2) in &ext_lines {
        svg.push_str(&format!(
            "<line x1=\"{x1:.1}\" y1=\"{y1:.1}\" x2=\"{x2:.1}\" y2=\"{y2:.1}\" \
             stroke=\"{stroke}\" stroke-width=\"{EXTERNAL_BORDER_STROKE_WIDTH}\"/>",
            stroke = qt.border_stroke
        ));
    }

    // Internal dividers (solid lines, no dash)
    // Vertical
    svg.push_str(&format!(
        "<line x1=\"{x:.1}\" y1=\"{y1:.1}\" x2=\"{x:.1}\" y2=\"{y2:.1}\" \
         stroke=\"{stroke}\" stroke-width=\"{INTERNAL_BORDER_STROKE_WIDTH}\"/>",
        x = quadrant_left + half_w,
        y1 = quadrant_top + half_ext,
        y2 = quadrant_top + quadrant_height - half_ext,
        stroke = qt.border_stroke
    ));
    // Horizontal
    svg.push_str(&format!(
        "<line x1=\"{x1:.1}\" y1=\"{y:.1}\" x2=\"{x2:.1}\" y2=\"{y:.1}\" \
         stroke=\"{stroke}\" stroke-width=\"{INTERNAL_BORDER_STROKE_WIDTH}\"/>",
        x1 = quadrant_left + half_ext,
        y = quadrant_top + half_h,
        x2 = quadrant_left + quadrant_width - half_ext,
        stroke = qt.border_stroke
    ));

    // --- Axis labels ---
    let draw_x_labels_in_middle = chart
        .x_axis
        .as_ref()
        .is_some_and(|(_, high)| !high.is_empty());
    let draw_y_labels_in_middle = chart
        .y_axis
        .as_ref()
        .is_some_and(|(_, high)| !high.is_empty());

    if let Some((low, high)) = &chart.x_axis {
        let x_axis_y = if x_axis_bottom {
            X_AXIS_LABEL_PADDING + quadrant_top + quadrant_height + QUADRANT_PADDING
        } else {
            X_AXIS_LABEL_PADDING + title_space_top
        };

        let low_x = quadrant_left
            + if draw_x_labels_in_middle {
                half_w / 2.0
            } else {
                0.0
            };
        let text_anchor_low = if draw_x_labels_in_middle {
            "middle"
        } else {
            "start"
        };
        svg.push_str(&format!(
            "<text x=\"{low_x:.1}\" y=\"{x_axis_y:.1}\" text-anchor=\"{text_anchor_low}\" dominant-baseline=\"hanging\" \
             font-family=\"{FONT_FAMILY}\" font-size=\"{X_AXIS_LABEL_FONT_SIZE}\" \
             fill=\"{fill}\">{}</text>",
            escape_xml(low),
            fill = qt.axis_text_fill
        ));

        if !high.is_empty() {
            let high_x = quadrant_left
                + half_w
                + if draw_x_labels_in_middle {
                    half_w / 2.0
                } else {
                    0.0
                };
            svg.push_str(&format!(
                "<text x=\"{high_x:.1}\" y=\"{x_axis_y:.1}\" text-anchor=\"middle\" dominant-baseline=\"hanging\" \
                 font-family=\"{FONT_FAMILY}\" font-size=\"{X_AXIS_LABEL_FONT_SIZE}\" \
                 fill=\"{fill}\">{}</text>",
                escape_xml(high),
                fill = qt.axis_text_fill
            ));
        }
    }

    if let Some((low, high)) = &chart.y_axis {
        let y_axis_x = Y_AXIS_LABEL_PADDING;

        // Bottom label (low value) — rotated -90° at (y_axis_x, low_y)
        let low_y = quadrant_top + quadrant_height
            - if draw_y_labels_in_middle {
                half_h / 2.0
            } else {
                0.0
            };
        svg.push_str(&format!(
            "<text x=\"0\" y=\"0\" text-anchor=\"middle\" dominant-baseline=\"hanging\" \
             font-family=\"{FONT_FAMILY}\" font-size=\"{Y_AXIS_LABEL_FONT_SIZE}\" \
             fill=\"{fill}\" transform=\"translate({y_axis_x:.1}, {low_y:.1}) rotate(-90)\">{}</text>",
            escape_xml(low),
            fill = qt.axis_text_fill
        ));

        // Top label (high value) — rotated -90° at (y_axis_x, high_y)
        if !high.is_empty() {
            let high_y = quadrant_top + half_h
                - if draw_y_labels_in_middle {
                    half_h / 2.0
                } else {
                    0.0
                };
            svg.push_str(&format!(
                "<text x=\"0\" y=\"0\" text-anchor=\"middle\" dominant-baseline=\"hanging\" \
                 font-family=\"{FONT_FAMILY}\" font-size=\"{Y_AXIS_LABEL_FONT_SIZE}\" \
                 fill=\"{fill}\" transform=\"translate({y_axis_x:.1}, {high_y:.1}) rotate(-90)\">{}</text>",
                escape_xml(high),
                fill = qt.axis_text_fill
            ));
        }
    }

    // --- Title ---
    if let Some(title) = &chart.title {
        svg.push_str(&format!(
            "<text x=\"{x:.1}\" y=\"{y:.1}\" text-anchor=\"middle\" dominant-baseline=\"hanging\" \
             font-family=\"{FONT_FAMILY}\" font-size=\"{TITLE_FONT_SIZE}\" \
             fill=\"{fill}\">{}</text>",
            escape_xml(title),
            x = CHART_WIDTH / 2.0,
            y = TITLE_PADDING,
            fill = qt.title_fill
        ));
    }

    // --- Data points ---
    for point in &chart.points {
        let px = quadrant_left + point.x.clamp(0.0, 1.0) * quadrant_width;
        let py = quadrant_top + (1.0 - point.y.clamp(0.0, 1.0)) * quadrant_height;

        svg.push_str(&format!(
            "<circle cx=\"{px:.1}\" cy=\"{py:.1}\" r=\"{POINT_RADIUS}\" \
             fill=\"{fill}\" stroke=\"{fill}\" stroke-width=\"0\"/>",
            fill = qt.point_fill
        ));

        svg.push_str(&format!(
            "<text x=\"0\" y=\"0\" text-anchor=\"middle\" dominant-baseline=\"hanging\" \
             font-family=\"{FONT_FAMILY}\" font-size=\"{POINT_LABEL_FONT_SIZE}\" \
             fill=\"{fill}\" transform=\"translate({px:.1}, {ty:.1})\">{}</text>",
            escape_xml(&point.label),
            fill = qt.point_text_fill,
            ty = py + POINT_TEXT_PADDING
        ));
    }

    svg.push_str("</svg>");
    Ok(svg)
}

#[derive(Debug, Clone)]
struct QuadrantChart {
    title: Option<String>,
    x_axis: Option<(String, String)>,
    y_axis: Option<(String, String)>,
    quadrants: BTreeMap<i32, String>,
    points: Vec<QuadrantPoint>,
}

#[derive(Debug, Clone)]
struct QuadrantPoint {
    label: String,
    x: f64,
    y: f64,
}

fn parse_quadrant_chart(input: &str) -> Result<QuadrantChart, MermaidError> {
    let lines: Vec<&str> = input.lines().collect();

    let mut i = 0_usize;
    while i < lines.len() {
        let line = lines[i].trim();
        if line.is_empty() || line.starts_with("%%") {
            i += 1;
            continue;
        }

        if line.split_whitespace().next() == Some("quadrantChart") {
            i += 1;
            break;
        }

        return Err(MermaidError::ParseError {
            line: i + 1,
            message: "Expected 'quadrantChart' declaration".to_string(),
        });
    }

    let mut title: Option<String> = None;
    let mut x_axis: Option<(String, String)> = None;
    let mut y_axis: Option<(String, String)> = None;
    let mut quadrants: BTreeMap<i32, String> = BTreeMap::new();
    let mut points: Vec<QuadrantPoint> = Vec::new();

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

        if let Some(rest) = line.strip_prefix("x-axis ") {
            x_axis = Some(parse_axis(rest.trim(), line_no)?);
            continue;
        }

        if let Some(rest) = line.strip_prefix("y-axis ") {
            y_axis = Some(parse_axis(rest.trim(), line_no)?);
            continue;
        }

        if let Some(rest) = line.strip_prefix("quadrant-") {
            let Some((n_str, label)) = rest.split_once(' ') else {
                return Err(MermaidError::ParseError {
                    line: line_no,
                    message: format!("Invalid quadrant label: {line}"),
                });
            };
            let n: i32 = n_str.trim().parse().map_err(|_| MermaidError::ParseError {
                line: line_no,
                message: format!("Invalid quadrant label: {line}"),
            })?;
            quadrants.insert(n, label.trim().to_string());
            continue;
        }

        let Some((label_raw, coords_raw)) = line.split_once(':') else {
            return Err(MermaidError::ParseError {
                line: line_no,
                message: format!("Invalid quadrant point: {line}"),
            });
        };

        let mut label = label_raw.trim().to_string();
        if let Some(stripped) = label.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
            label = stripped.to_string();
        }

        let coords = coords_raw.trim();
        let coords = coords
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .ok_or_else(|| MermaidError::ParseError {
                line: line_no,
                message: format!("Invalid quadrant point: {line}"),
            })?;

        let parts: Vec<&str> = coords.split(',').map(|p| p.trim()).collect();
        if parts.len() != 2 {
            return Err(MermaidError::ParseError {
                line: line_no,
                message: format!("Invalid quadrant point: {line}"),
            });
        }

        let x: f64 = parts[0].parse().map_err(|_| MermaidError::ParseError {
            line: line_no,
            message: format!("Invalid quadrant point: {line}"),
        })?;
        let y: f64 = parts[1].parse().map_err(|_| MermaidError::ParseError {
            line: line_no,
            message: format!("Invalid quadrant point: {line}"),
        })?;

        points.push(QuadrantPoint { label, x, y });
    }

    if points.is_empty() {
        return Err(MermaidError::ParseError {
            line: 1,
            message: "Quadrant chart requires at least one point".to_string(),
        });
    }

    Ok(QuadrantChart {
        title,
        x_axis,
        y_axis,
        quadrants,
        points,
    })
}

fn parse_axis(s: &str, line_no: usize) -> Result<(String, String), MermaidError> {
    let Some((a, b)) = s.split_once("-->") else {
        return Err(MermaidError::ParseError {
            line: line_no,
            message: format!("Invalid axis: {s}"),
        });
    };
    Ok((a.trim().to_string(), b.trim().to_string()))
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
