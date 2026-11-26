use crate::error::MermaidError;
use crate::theme::MermaidTheme;

use std::f64::consts::PI;

const WIDTH: f64 = 600.0;
const HEIGHT: f64 = 600.0;
const MARGIN: f64 = 50.0;

const AXIS_SCALE_FACTOR: f64 = 1.0;
const AXIS_LABEL_FACTOR: f64 = 1.05;
const CURVE_TENSION: f64 = 0.17;

const DEFAULT_TICKS: usize = 5;
const DEFAULT_MIN: f64 = 0.0;

const AXIS_COLOR: &str = "#333333";
const GRATICULE_COLOR: &str = "#DEDEDE";
const GRATICULE_OPACITY: f64 = 0.3;

const CURVE_COLOR_0: &str = "hsl(240, 100%, 76.2745098039%)";

pub fn render_radar_diagram_to_svg(
    mermaid_source: &str,
    theme: &MermaidTheme,
) -> Result<String, MermaidError> {
    let diagram = parse_radar(mermaid_source)?;

    if diagram.axes.is_empty() {
        return Err(MermaidError::ParseError {
            line: 1,
            message: "radar diagram requires at least one axis".to_string(),
        });
    }

    let max_value = diagram
        .curves
        .iter()
        .flat_map(|c| c.values.iter().copied())
        .fold(0.0, f64::max)
        .max(1.0);

    let total_width = WIDTH + 2.0 * MARGIN;
    let total_height = HEIGHT + 2.0 * MARGIN;

    let center_x = MARGIN + WIDTH / 2.0;
    let center_y = MARGIN + HEIGHT / 2.0;

    let radius = WIDTH.min(HEIGHT) / 2.0;

    let mut svg = String::new();
    svg.push_str(&format!(
        "<svg aria-roledescription=\"radar\" role=\"graphics-document document\" height=\"{total_height}\" viewBox=\"0 0 {total_width} {total_height}\" xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" width=\"{total_width}\" id=\"my-svg\" style=\"background-color: {};\">",
        theme.background
    ));

    svg.push_str("<style>");
    svg.push_str(&format!(
        "#my-svg .radarAxisLine{{stroke:{AXIS_COLOR};stroke-width:2;}}"
    ));
    svg.push_str(&format!(
        "#my-svg .radarAxisLabel{{dominant-baseline:middle;text-anchor:middle;font-size:12px;color:{AXIS_COLOR};}}"
    ));
    svg.push_str(&format!(
        "#my-svg .radarGraticule{{fill:{GRATICULE_COLOR};fill-opacity:{GRATICULE_OPACITY};stroke:{GRATICULE_COLOR};stroke-width:1;}}"
    ));
    svg.push_str(
        "#my-svg .radarLegendText{text-anchor:start;font-size:12px;dominant-baseline:hanging;}",
    );
    svg.push_str(&format!(
        "#my-svg .radarCurve-0{{color:{CURVE_COLOR_0};fill:{CURVE_COLOR_0};fill-opacity:0.5;stroke:{CURVE_COLOR_0};stroke-width:2;}}"
    ));
    svg.push_str(&format!(
        "#my-svg .radarLegendBox-0{{fill:{CURVE_COLOR_0};fill-opacity:0.5;stroke:{CURVE_COLOR_0};}}"
    ));
    svg.push_str("</style>");

    svg.push_str("<g/>");
    svg.push_str(&format!(
        "<g transform=\"translate({center_x}, {center_y})\">"
    ));

    for i in 0..DEFAULT_TICKS {
        let r = radius * (i as f64 + 1.0) / (DEFAULT_TICKS as f64);
        svg.push_str(&format!("<circle class=\"radarGraticule\" r=\"{r}\"/>"));
    }

    let n_axes = diagram.axes.len();
    for (i, axis_label) in diagram.axes.iter().enumerate() {
        let angle = 2.0 * (i as f64) * PI / (n_axes as f64) - PI / 2.0;
        let x2 = radius * AXIS_SCALE_FACTOR * angle.cos();
        let y2 = radius * AXIS_SCALE_FACTOR * angle.sin();
        svg.push_str(&format!(
            "<line class=\"radarAxisLine\" y2=\"{y2}\" x2=\"{x2}\" y1=\"0\" x1=\"0\"/>"
        ));

        let lx = radius * AXIS_LABEL_FACTOR * angle.cos();
        let ly = radius * AXIS_LABEL_FACTOR * angle.sin();
        svg.push_str(&format!(
            "<text class=\"radarAxisLabel\" y=\"{ly}\" x=\"{lx}\">{}</text>",
            escape_xml(axis_label)
        ));
    }

    for (curve_idx, curve) in diagram.curves.iter().enumerate() {
        if curve.values.len() != n_axes {
            continue;
        }

        let mut points = Vec::with_capacity(n_axes);
        for (i, v) in curve.values.iter().copied().enumerate() {
            let angle = 2.0 * (i as f64) * PI / (n_axes as f64) - PI / 2.0;
            let r = radius * ((v.max(DEFAULT_MIN)).min(max_value) - DEFAULT_MIN)
                / (max_value - DEFAULT_MIN);
            points.push((r * angle.cos(), r * angle.sin()));
        }

        let d = closed_round_curve(&points, CURVE_TENSION);
        svg.push_str(&format!(
            "<path class=\"radarCurve-{curve_idx}\" d=\"{d}\"/>"
        ));

        let legend_x = (WIDTH / 2.0 + MARGIN) * 3.0 / 4.0;
        let legend_y = -(HEIGHT / 2.0 + MARGIN) * 3.0 / 4.0;
        let item_y = legend_y + curve_idx as f64 * 20.0;
        svg.push_str(&format!(
            "<g transform=\"translate({legend_x}, {item_y})\">"
        ));
        svg.push_str(&format!(
            "<rect class=\"radarLegendBox-{curve_idx}\" height=\"12\" width=\"12\"/>"
        ));
        svg.push_str(&format!(
            "<text class=\"radarLegendText\" y=\"0\" x=\"16\">{}</text>",
            escape_xml(&curve.name)
        ));
        svg.push_str("</g>");
    }

    svg.push_str("<text y=\"-350\" x=\"0\" class=\"radarTitle\"/>");
    svg.push_str("</g></svg>");

    Ok(svg)
}

#[derive(Debug, Clone)]
struct RadarDiagram {
    axes: Vec<String>,
    curves: Vec<RadarCurve>,
}

#[derive(Debug, Clone)]
struct RadarCurve {
    name: String,
    values: Vec<f64>,
}

fn parse_radar(input: &str) -> Result<RadarDiagram, MermaidError> {
    let mut found_header = false;

    let mut axes: Vec<String> = Vec::new();
    let mut curves: Vec<RadarCurve> = Vec::new();

    for (idx, raw) in input.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with("%%") {
            continue;
        }

        if !found_header {
            if line.split_whitespace().next() != Some("radar-beta") {
                return Err(MermaidError::ParseError {
                    line: line_no,
                    message: "Expected 'radar-beta' declaration".to_string(),
                });
            }
            found_header = true;
            continue;
        }

        if let Some(rest) = line.strip_prefix("axis ") {
            axes = rest
                .split(',')
                .map(|p| p.trim())
                .filter(|p| !p.is_empty())
                .map(|p| p.to_string())
                .collect();
            continue;
        }

        if let Some(rest) = line.strip_prefix("curve ") {
            let (name, values) = parse_curve(rest.trim(), line_no)?;
            curves.push(RadarCurve { name, values });
            continue;
        }
    }

    if !found_header {
        return Err(MermaidError::ParseError {
            line: 1,
            message: "Expected 'radar-beta' declaration".to_string(),
        });
    }

    Ok(RadarDiagram { axes, curves })
}

fn parse_curve(s: &str, line: usize) -> Result<(String, Vec<f64>), MermaidError> {
    let Some((name, rest)) = s.split_once('{') else {
        return Err(MermaidError::ParseError {
            line,
            message: format!("Invalid curve: {s}"),
        });
    };

    let name = name.trim();
    let inner = rest
        .strip_suffix('}')
        .ok_or_else(|| MermaidError::ParseError {
            line,
            message: format!("Invalid curve: {s}"),
        })?
        .trim();

    let mut values: Vec<f64> = Vec::new();
    for part in inner.split(',') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        let v: f64 = p.parse().map_err(|_| MermaidError::ParseError {
            line,
            message: format!("Invalid curve value: {p}"),
        })?;
        values.push(v);
    }

    Ok((name.to_string(), values))
}

fn closed_round_curve(points: &[(f64, f64)], tension: f64) -> String {
    if points.is_empty() {
        return String::new();
    }

    let n = points.len();
    let mut d = String::new();
    d.push_str(&format!("M{},{}", points[0].0, points[0].1));

    for i in 0..n {
        let p0 = points[(i + n - 1) % n];
        let p1 = points[i];
        let p2 = points[(i + 1) % n];
        let p3 = points[(i + 2) % n];

        let cp1 = (
            p1.0 + (p2.0 - p0.0) * tension,
            p1.1 + (p2.1 - p0.1) * tension,
        );
        let cp2 = (
            p2.0 - (p3.0 - p1.0) * tension,
            p2.1 - (p3.1 - p1.1) * tension,
        );

        d.push_str(&format!(
            " C{},{} {},{} {},{}",
            cp1.0, cp1.1, cp2.0, cp2.1, p2.0, p2.1
        ));
    }

    d.push_str(" Z");
    d
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
