use crate::error::MermaidError;
use crate::theme::MermaidTheme;

const CHART_WIDTH: f64 = 700.0;
const CHART_HEIGHT: f64 = 500.0;

const CHART_TITLE_FONT_SIZE: f64 = 20.0;
const CHART_TITLE_PADDING: f64 = 10.0;

const AXIS_LABEL_FONT_SIZE: f64 = 14.0;
const AXIS_LABEL_PADDING: f64 = 5.0;

const AXIS_TITLE_FONT_SIZE: f64 = 16.0;
const AXIS_TITLE_PADDING: f64 = 5.0;

const AXIS_TICK_LENGTH: f64 = 5.0;
const AXIS_TICK_WIDTH: f64 = 2.0;

const AXIS_LINE_WIDTH: f64 = 2.0;

const DEFAULT_TICK_COUNT: usize = 10;

/// Floor for the auto-shrunk categorical x-axis label font (this port has no
/// label rotation, so a busy axis shrinks-to-fit down to here, then overflows).
const MIN_X_LABEL_FONT_SIZE: f64 = 8.0;

const PLOT_RIGHT_MARGIN: f64 = 12.0;

/// Per-series colors (Tableau 10), cycled by series index. Mid-tone hues stay
/// legible on both the light and dark surfaces this engine renders onto.
const SERIES_PALETTE: [&str; 10] = [
    "#4e79a7", "#f28e2b", "#e15759", "#76b7b2", "#59a14f", "#edc948", "#b07aa1", "#ff9da7",
    "#9c755f", "#bab0ac",
];

pub fn render_xychart_diagram_to_svg(
    mermaid_source: &str,
    theme: &MermaidTheme,
) -> Result<String, MermaidError> {
    let chart = parse_xychart(mermaid_source)?;

    // Theme text color (not a fixed near-black) so axes stay visible on dark.
    let axis_color = theme.text_color.as_str();

    let y_ticks = d3_ticks(chart.y_min, chart.y_max, DEFAULT_TICK_COUNT);
    let y_tick_labels: Vec<String> = y_ticks.iter().map(|v| format_tick(*v)).collect();
    let label_text_height = approx_text_height(AXIS_LABEL_FONT_SIZE);
    let y_label_max_width = y_tick_labels
        .iter()
        .map(|s| approx_text_width(s, AXIS_LABEL_FONT_SIZE))
        .fold(0.0, f64::max);

    let title_height = if chart.title.is_empty() {
        0.0
    } else {
        approx_text_height(CHART_TITLE_FONT_SIZE) + 2.0 * CHART_TITLE_PADDING
    };
    let y_title_width = if chart.y_title.is_empty() {
        0.0
    } else {
        approx_text_height(AXIS_TITLE_FONT_SIZE) + 2.0 * AXIS_TITLE_PADDING
    };
    let x_title_height = if chart.x_title.is_empty() {
        0.0
    } else {
        approx_text_height(AXIS_TITLE_FONT_SIZE) + 2.0 * AXIS_TITLE_PADDING
    };

    let left_axis_width =
        AXIS_LINE_WIDTH + AXIS_TICK_LENGTH + (y_label_max_width + 2.0 * AXIS_LABEL_PADDING);
    let plot_x = y_title_width + left_axis_width;
    let plot_y = title_height;
    let plot_w = (CHART_WIDTH - plot_x - PLOT_RIGHT_MARGIN).max(1.0);

    let point_count = chart.series.iter().map(Vec::len).max().unwrap_or(0);
    let x = layout_x_axis(&chart.x_axis, plot_x, plot_w, point_count);

    // Bottom band depends on the resolved (possibly shrunk) x-label font.
    let x_label_height = approx_text_height(x.label_font);
    let bottom_axis_height = AXIS_LINE_WIDTH
        + AXIS_TICK_LENGTH
        + (x_label_height + 2.0 * AXIS_LABEL_PADDING)
        + x_title_height;
    let plot_h = (CHART_HEIGHT - plot_y - bottom_axis_height).max(1.0);

    let y_outer_padding = (label_text_height / 2.0).min(0.2 * plot_h);
    let y_top = plot_y + y_outer_padding;
    let y_bottom = plot_y + plot_h - y_outer_padding;

    let y_at = |v: f64| scale_linear(v, chart.y_min, chart.y_max, y_bottom, y_top);

    let mut svg = String::new();

    svg.push_str(&format!(
        "<svg aria-roledescription=\"xychart\" role=\"graphics-document document\" viewBox=\"0 0 {CHART_WIDTH} {CHART_HEIGHT}\" style=\"max-width: {CHART_WIDTH}px; background-color: {};\" xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" width=\"100%\" id=\"my-svg\">",
        theme.background
    ));

    svg.push_str("<g/><g class=\"main\">");
    svg.push_str(&format!(
        "<rect fill=\"{}\" class=\"background\" height=\"{CHART_HEIGHT}\" width=\"{CHART_WIDTH}\"/>",
        theme.background
    ));

    if !chart.title.is_empty() {
        let title_y = title_height / 2.0;
        let title_x = CHART_WIDTH / 2.0;
        svg.push_str("<g class=\"chart-title\">");
        svg.push_str(&format!(
            "<text transform=\"translate({title_x}, {title_y}) rotate(0)\" text-anchor=\"middle\" dominant-baseline=\"middle\" font-size=\"{CHART_TITLE_FONT_SIZE}\" fill=\"{axis_color}\" y=\"0\" x=\"0\">{}</text>",
            escape_xml(&chart.title)
        ));
        svg.push_str("</g>");
    }

    svg.push_str("<g class=\"plot\">");
    for (idx, values) in chart.series.iter().enumerate() {
        let points: Vec<(f64, f64)> = values
            .iter()
            .enumerate()
            .map(|(i, v)| (x.series_point_x(i), y_at(*v)))
            .collect();
        if points.is_empty() {
            continue;
        }
        let d = points_to_path_d(&points);
        let stroke = SERIES_PALETTE[idx % SERIES_PALETTE.len()];
        svg.push_str(&format!("<g class=\"line-plot-{idx}\">"));
        svg.push_str(&format!(
            "<path stroke-width=\"2\" stroke=\"{stroke}\" fill=\"none\" d=\"{d}\"/>",
        ));
        svg.push_str("</g>");
    }
    svg.push_str("</g>");

    let bottom_axis_y = plot_y + plot_h;
    svg.push_str("<g class=\"bottom-axis\">");
    svg.push_str("<g class=\"axis-line\">");
    svg.push_str(&format!(
        "<path stroke-width=\"{AXIS_LINE_WIDTH}\" stroke=\"{axis_color}\" fill=\"none\" d=\"M {plot_x},{y} L {x_end},{y}\"/>",
        y = bottom_axis_y + AXIS_LINE_WIDTH / 2.0,
        x_end = plot_x + plot_w,
    ));
    svg.push_str("</g>");

    svg.push_str("<g class=\"label\">");
    let x_label_y = bottom_axis_y + AXIS_LABEL_PADDING + AXIS_TICK_LENGTH + AXIS_LINE_WIDTH;
    for (pos, label) in x.tick_positions.iter().zip(x.tick_labels.iter()) {
        svg.push_str(&format!(
            "<text transform=\"translate({pos}, {x_label_y}) rotate(0)\" text-anchor=\"middle\" dominant-baseline=\"text-before-edge\" font-size=\"{font}\" fill=\"{axis_color}\" y=\"0\" x=\"0\">{}</text>",
            escape_xml(label),
            font = x.label_font,
        ));
    }
    svg.push_str("</g>");

    svg.push_str("<g class=\"ticks\">");
    let tick_y0 = bottom_axis_y + AXIS_LINE_WIDTH;
    let tick_y1 = tick_y0 + AXIS_TICK_LENGTH;
    for pos in &x.tick_positions {
        svg.push_str(&format!(
            "<path stroke-width=\"{AXIS_TICK_WIDTH}\" stroke=\"{axis_color}\" fill=\"none\" d=\"M {pos},{tick_y0} L {pos},{tick_y1}\"/>",
        ));
    }
    svg.push_str("</g>");
    svg.push_str("</g>");

    svg.push_str("<g class=\"left-axis\">");
    svg.push_str("<g class=\"axisl-line\">");
    let axis_x = plot_x - AXIS_LINE_WIDTH / 2.0;
    svg.push_str(&format!(
        "<path stroke-width=\"{AXIS_LINE_WIDTH}\" stroke=\"{axis_color}\" fill=\"none\" d=\"M {axis_x},{plot_y} L {axis_x},{y1}\"/>",
        y1 = plot_y + plot_h,
    ));
    svg.push_str("</g>");

    svg.push_str("<g class=\"label\">");
    let y_label_x = plot_x - AXIS_LABEL_PADDING - AXIS_TICK_LENGTH - AXIS_LINE_WIDTH;
    for (tick_value, tick_label) in y_ticks.iter().zip(y_tick_labels.iter()) {
        let y = y_at(*tick_value);
        svg.push_str(&format!(
            "<text transform=\"translate({y_label_x}, {y}) rotate(0)\" text-anchor=\"end\" dominant-baseline=\"middle\" font-size=\"{AXIS_LABEL_FONT_SIZE}\" fill=\"{axis_color}\" y=\"0\" x=\"0\">{}</text>",
            escape_xml(tick_label)
        ));
    }
    svg.push_str("</g>");

    svg.push_str("<g class=\"ticks\">");
    let tick_x0 = plot_x - AXIS_LINE_WIDTH;
    let tick_x1 = tick_x0 - AXIS_TICK_LENGTH;
    for tick_value in &y_ticks {
        let y = y_at(*tick_value);
        svg.push_str(&format!(
            "<path stroke-width=\"{AXIS_TICK_WIDTH}\" stroke=\"{axis_color}\" fill=\"none\" d=\"M {tick_x0},{y} L {tick_x1},{y}\"/>",
        ));
    }
    svg.push_str("</g>");
    svg.push_str("</g>");

    if !chart.x_title.is_empty() {
        let tx = plot_x + plot_w / 2.0;
        let ty = CHART_HEIGHT - x_title_height / 2.0;
        svg.push_str("<g class=\"x-axis-title\">");
        svg.push_str(&format!(
            "<text transform=\"translate({tx}, {ty}) rotate(0)\" text-anchor=\"middle\" dominant-baseline=\"middle\" font-size=\"{AXIS_TITLE_FONT_SIZE}\" fill=\"{axis_color}\" y=\"0\" x=\"0\">{}</text>",
            escape_xml(&chart.x_title)
        ));
        svg.push_str("</g>");
    }
    if !chart.y_title.is_empty() {
        let tx = y_title_width / 2.0;
        let ty = plot_y + plot_h / 2.0;
        svg.push_str("<g class=\"y-axis-title\">");
        svg.push_str(&format!(
            "<text transform=\"translate({tx}, {ty}) rotate(-90)\" text-anchor=\"middle\" dominant-baseline=\"middle\" font-size=\"{AXIS_TITLE_FONT_SIZE}\" fill=\"{axis_color}\" y=\"0\" x=\"0\">{}</text>",
            escape_xml(&chart.y_title)
        ));
        svg.push_str("</g>");
    }

    svg.push_str("</g><g class=\"mermaid-tmp-group\"/></svg>");

    Ok(svg)
}

/// Either evenly-spaced named categories (`x-axis [a, b]`) or a numeric range.
#[derive(Debug, Clone, PartialEq)]
enum XAxis {
    Numeric { min: f64, max: f64 },
    Category(Vec<String>),
}

#[derive(Debug, Clone)]
struct XyChart {
    title: String,
    x_title: String,
    y_title: String,
    x_axis: XAxis,
    y_min: f64,
    y_max: f64,
    series: Vec<Vec<f64>>,
}

fn parse_xychart(input: &str) -> Result<XyChart, MermaidError> {
    let mut found_header = false;

    let mut title = String::new();
    let mut x_title = String::new();
    let mut y_title = String::new();
    let mut x_axis: Option<XAxis> = None;
    let mut y_range: Option<(f64, f64)> = None;
    let mut series: Vec<Vec<f64>> = Vec::new();

    for (idx, raw) in input.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with("%%") {
            continue;
        }

        if !found_header {
            if line.split_whitespace().next() != Some("xychart-beta") {
                return Err(MermaidError::ParseError {
                    line: line_no,
                    message: "Expected 'xychart-beta' declaration".to_string(),
                });
            }
            found_header = true;
            continue;
        }

        if let Some(rest) = line.strip_prefix("title ") {
            title = unquote(rest);
            continue;
        }

        if let Some(rest) = line.strip_prefix("x-axis ") {
            let (label, axis) = parse_x_axis(rest.trim(), line_no)?;
            x_title = label;
            x_axis = Some(axis);
            continue;
        }

        if let Some(rest) = line.strip_prefix("y-axis ") {
            let (label, range) = parse_y_axis(rest.trim(), line_no)?;
            y_title = label;
            if let Some(range) = range {
                y_range = Some(range);
            }
            continue;
        }

        if let Some(values) = parse_series_line(line, line_no)? {
            series.push(values);
            continue;
        }
        // Unknown lines (e.g. an unsupported `bar` series) are ignored.
    }

    if !found_header {
        return Err(MermaidError::ParseError {
            line: 1,
            message: "Expected 'xychart-beta' declaration".to_string(),
        });
    }

    if series.iter().all(|values| values.is_empty()) {
        return Err(MermaidError::ParseError {
            line: 1,
            message: "xychart requires at least one plot".to_string(),
        });
    }

    let x_axis = x_axis.unwrap_or(XAxis::Numeric { min: 0.0, max: 0.0 });
    let (y_min, y_max) = y_range.unwrap_or_else(|| auto_y_range(&series));

    Ok(XyChart {
        title,
        x_title,
        y_title,
        x_axis,
        y_min,
        y_max,
        series,
    })
}

/// Parse an `x-axis` body: optional title plus a category list or numeric range.
fn parse_x_axis(rest: &str, line: usize) -> Result<(String, XAxis), MermaidError> {
    if let Some(open) = rest.find('[') {
        let title = unquote(rest[..open].trim());
        let close =
            rest.rfind(']')
                .filter(|c| *c > open)
                .ok_or_else(|| MermaidError::ParseError {
                    line,
                    message: format!("Invalid x-axis categories: {rest}"),
                })?;
        let categories = parse_category_list(&rest[open + 1..close]);
        Ok((title, XAxis::Category(categories)))
    } else if rest.contains("-->") {
        let (title, min, max) = parse_labeled_range(rest, line)?;
        Ok((title, XAxis::Numeric { min, max }))
    } else {
        Ok((unquote(rest), XAxis::Category(Vec::new())))
    }
}

/// Parse a `y-axis` body; a title without a range auto-ranges from the data.
fn parse_y_axis(rest: &str, line: usize) -> Result<(String, Option<(f64, f64)>), MermaidError> {
    if rest.contains("-->") {
        let (title, min, max) = parse_labeled_range(rest, line)?;
        Ok((title, Some((min, max))))
    } else {
        Ok((unquote(rest), None))
    }
}

/// Parse a `[title] min --> max` body; the title is everything left of `min`.
fn parse_labeled_range(s: &str, line: usize) -> Result<(String, f64, f64), MermaidError> {
    let (left, right) = s
        .split_once("-->")
        .ok_or_else(|| MermaidError::ParseError {
            line,
            message: format!("Invalid axis range: {s}"),
        })?;

    let max: f64 = right.trim().parse().map_err(|_| MermaidError::ParseError {
        line,
        message: format!("Invalid axis max: {}", right.trim()),
    })?;

    let left = left.trim();
    let (title, min_str) = match left.rsplit_once(char::is_whitespace) {
        Some((title, min)) => (title.trim(), min.trim()),
        None => ("", left),
    };
    let min: f64 = min_str.parse().map_err(|_| MermaidError::ParseError {
        line,
        message: format!("Invalid axis min: {min_str}"),
    })?;

    Ok((unquote(title), min, max))
}

/// Parse a `line [..]` series; non-`line` declarations return `None`.
fn parse_series_line(line: &str, line_no: usize) -> Result<Option<Vec<f64>>, MermaidError> {
    let Some(rest) = strip_keyword(line, "line") else {
        return Ok(None);
    };
    let values = parse_bracketed_number_list(rest.trim(), line_no)?;
    Ok(Some(values))
}

/// Strip `keyword` only when it stands alone (end / whitespace / `[` follows),
/// so `line` matches but `linear` does not.
fn strip_keyword<'a>(line: &'a str, keyword: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(keyword)?;
    match rest.chars().next() {
        None => Some(rest),
        Some(c) if c.is_whitespace() || c == '[' => Some(rest),
        _ => None,
    }
}

/// Split on top-level (quote-aware) commas, then unquote/trim each entry.
fn parse_category_list(inner: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for c in inner.chars() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
                current.push(c);
            }
            None => match c {
                '"' | '\'' => {
                    quote = Some(c);
                    current.push(c);
                }
                ',' => out.push(std::mem::take(&mut current)),
                _ => current.push(c),
            },
        }
    }
    out.push(current);

    out.into_iter()
        .map(|s| unquote(s.trim()))
        .filter(|s| !s.is_empty())
        .collect()
}

/// Strip one pair of matching surrounding quotes (`"…"` or `'…'`).
fn unquote(s: &str) -> String {
    let t = s.trim();
    let bytes = t.as_bytes();
    if t.len() >= 2 {
        let first = bytes[0];
        let last = bytes[t.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return t[1..t.len() - 1].to_string();
        }
    }
    t.to_string()
}

fn auto_y_range(series: &[Vec<f64>]) -> (f64, f64) {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for values in series {
        for &v in values {
            min = min.min(v);
            max = max.max(v);
        }
    }
    if !min.is_finite() || !max.is_finite() {
        return (0.0, 0.0);
    }
    if (max - min).abs() < f64::EPSILON {
        return (min - 1.0, max + 1.0);
    }
    (min, max)
}

struct XAxisLayout {
    tick_positions: Vec<f64>,
    tick_labels: Vec<String>,
    label_font: f64,
    /// Longest series length, shared by every series so the same index maps to
    /// the same x across series (overlaid lines stay on one x domain).
    point_count: usize,
    geom: XGeom,
}

enum XGeom {
    /// Band scale: points/ticks at band centers.
    Category { plot_left: f64, band_w: f64 },
    /// Linear scale: points evenly distributed across `[x0, x1]`.
    Numeric { x0: f64, x1: f64 },
}

impl XAxisLayout {
    fn series_point_x(&self, i: usize) -> f64 {
        match self.geom {
            XGeom::Category { plot_left, band_w } => plot_left + (i as f64 + 0.5) * band_w,
            XGeom::Numeric { x0, x1 } => {
                if self.point_count <= 1 {
                    x0
                } else {
                    x0 + (i as f64) / ((self.point_count - 1) as f64) * (x1 - x0)
                }
            }
        }
    }
}

fn layout_x_axis(x_axis: &XAxis, plot_x: f64, plot_w: f64, point_count: usize) -> XAxisLayout {
    match x_axis {
        XAxis::Category(categories) => {
            // Size bands to whichever is larger so every series point lands in a
            // band (a series longer than the category list still stays on-plot).
            let n = categories.len().max(point_count).max(1);
            let band_w = plot_w / n as f64;
            // Shrink the font so the widest category fits its band (to a floor).
            let widest_units = categories
                .iter()
                .map(|c| crate::text_wrap::display_width_units(c))
                .fold(0.0, f64::max);
            let label_font = if widest_units > 0.0 {
                let fit = (band_w * 0.95) / (widest_units * 0.525);
                AXIS_LABEL_FONT_SIZE.min(fit).max(MIN_X_LABEL_FONT_SIZE)
            } else {
                AXIS_LABEL_FONT_SIZE
            };
            let tick_positions = (0..categories.len())
                .map(|i| plot_x + (i as f64 + 0.5) * band_w)
                .collect();
            XAxisLayout {
                tick_positions,
                tick_labels: categories.clone(),
                label_font,
                point_count,
                geom: XGeom::Category {
                    plot_left: plot_x,
                    band_w,
                },
            }
        }
        XAxis::Numeric { min, max } => {
            let ticks = d3_ticks(*min, *max, DEFAULT_TICK_COUNT);
            let labels: Vec<String> = ticks.iter().map(|v| format_tick(*v)).collect();
            let label_max_width = labels
                .iter()
                .map(|s| approx_text_width(s, AXIS_LABEL_FONT_SIZE))
                .fold(0.0, f64::max);
            let outer = (label_max_width / 2.0).min(0.2 * plot_w);
            let x0 = plot_x + outer;
            let x1 = plot_x + plot_w - outer;
            let tick_positions = ticks
                .iter()
                .map(|v| scale_linear(*v, *min, *max, x0, x1))
                .collect();
            XAxisLayout {
                tick_positions,
                tick_labels: labels,
                label_font: AXIS_LABEL_FONT_SIZE,
                point_count,
                geom: XGeom::Numeric { x0, x1 },
            }
        }
    }
}

fn parse_bracketed_number_list(s: &str, line: usize) -> Result<Vec<f64>, MermaidError> {
    let Some(start) = s.find('[') else {
        return Err(MermaidError::ParseError {
            line,
            message: format!("Invalid plot data: {s}"),
        });
    };
    let Some(end) = s.rfind(']') else {
        return Err(MermaidError::ParseError {
            line,
            message: format!("Invalid plot data: {s}"),
        });
    };

    let inner = &s[start + 1..end];
    let mut out = Vec::new();

    for part in inner.split(',') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        let v: f64 = p.parse().map_err(|_| MermaidError::ParseError {
            line,
            message: format!("Invalid plot value: {p}"),
        })?;
        out.push(v);
    }

    Ok(out)
}

fn points_to_path_d(points: &[(f64, f64)]) -> String {
    if points.is_empty() {
        return String::new();
    }

    let mut d = String::new();
    if let Some((x, y)) = points.first().copied() {
        d.push_str(&format!("M{x},{y}"));
    }

    for &(x, y) in points.iter().skip(1) {
        d.push_str(&format!("L{x},{y}"));
    }

    d
}

fn scale_linear(
    value: f64,
    domain_min: f64,
    domain_max: f64,
    range_min: f64,
    range_max: f64,
) -> f64 {
    if (domain_max - domain_min).abs() < f64::EPSILON {
        return range_min;
    }

    let t = (value - domain_min) / (domain_max - domain_min);
    range_min + t * (range_max - range_min)
}

fn d3_ticks(start: f64, stop: f64, count: usize) -> Vec<f64> {
    if count == 0 {
        return Vec::new();
    }
    if !start.is_finite() || !stop.is_finite() {
        return Vec::new();
    }
    if start == stop {
        return vec![start];
    }

    let reverse = stop < start;
    let (a, b) = if reverse {
        (stop, start)
    } else {
        (start, stop)
    };

    let step = tick_step(a, b, count as f64);
    if !step.is_finite() || step == 0.0 {
        return Vec::new();
    }

    let start0 = (a / step).ceil();
    let stop0 = (b / step).floor();

    let n = (stop0 - start0 + 1.0).max(0.0) as i64;
    let mut ticks = Vec::with_capacity(n as usize);

    for i in 0..n {
        ticks.push((start0 + i as f64) * step);
    }

    if reverse {
        ticks.reverse();
    }

    ticks
}

fn tick_step(start: f64, stop: f64, count: f64) -> f64 {
    let step0 = (stop - start).abs() / count.max(1.0);
    let step1 = 10.0_f64.powf(step0.log10().floor());
    let error = step0 / step1;

    let e10 = 50.0_f64.sqrt();
    let e5 = 10.0_f64.sqrt();
    let e2 = 2.0_f64.sqrt();

    let step = if error >= e10 {
        step1 * 10.0
    } else if error >= e5 {
        step1 * 5.0
    } else if error >= e2 {
        step1 * 2.0
    } else {
        step1
    };

    if stop < start {
        -step
    } else {
        step
    }
}

fn format_tick(value: f64) -> String {
    let rounded = value.round();
    if (value - rounded).abs() < 1e-9 {
        return format!("{:.0}", rounded);
    }

    let s = format!("{value:.6}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn approx_text_width(text: &str, font_size: f64) -> f64 {
    let n = crate::text_wrap::display_width_units(text);
    n * font_size * 0.525
}

fn approx_text_height(font_size: f64) -> f64 {
    (font_size * 1.15).round()
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Categorical x-axis (no `-->`), labeled+ranged y-axis, two `line` series:
    // the case the numeric-only parser rejected ("opening image ... fails").
    const SAMPLE: &str = r#"xychart-beta
    title "Weekly active users by region"
    x-axis ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]
    y-axis "% of users" 0 --> 40
    line [20.3, 22.6, 24.2, 24.3, 26.2, 27.2, 32.4, 31.9, 31.4, 31.1, 33.6, 34.3]
    line [3.2, 6.3, 10.0, 9.4, 11.1, 10.7, 15.3, 13.4, 13.5, 12.5, 15.4, 15.8]"#;

    #[test]
    fn parses_categorical_axis_labels_and_two_series() {
        let chart = parse_xychart(SAMPLE).expect("must parse");
        assert_eq!(chart.title, "Weekly active users by region");
        assert_eq!(chart.y_title, "% of users");
        assert!(chart.x_title.is_empty());
        assert_eq!((chart.y_min, chart.y_max), (0.0, 40.0));
        match &chart.x_axis {
            XAxis::Category(cats) => {
                assert_eq!(cats.len(), 12);
                assert_eq!(cats[0], "Jan");
                assert_eq!(cats[11], "Dec");
            }
            other => panic!("expected categorical x-axis, got {other:?}"),
        }
        assert_eq!(chart.series.len(), 2);
        assert_eq!(chart.series[0].len(), 12);
        assert_eq!(chart.series[1].len(), 12);
        assert_eq!(chart.series[1][0], 3.2);
    }

    #[test]
    fn categorical_x_axis_with_two_lines_renders() {
        let svg = render_xychart_diagram_to_svg(SAMPLE, &MermaidTheme::light())
            .expect("categorical xychart with two line series must render");
        assert!(svg.contains("<svg"));
        assert!(svg.contains("</svg>"));
        assert!(svg.contains(">Jan<"));
        assert!(svg.contains(">Dec<"));
        assert!(svg.contains(">% of users<"));
        assert_eq!(svg.matches("class=\"line-plot-").count(), 2);
        assert!(svg.contains(SERIES_PALETTE[0]));
        assert!(svg.contains(SERIES_PALETTE[1]));
        assert!(svg.contains("Weekly active users by region"));
        assert!(!svg.contains("&quot;")); // title/label quotes stripped
    }

    #[test]
    fn numeric_x_axis_range_still_renders() {
        let src = "xychart-beta\n    title Demo\n    x-axis 0 --> 10\n    y-axis 0 --> 100\n    line [5, 10, 20, 40]";
        let svg = render_xychart_diagram_to_svg(src, &MermaidTheme::light())
            .expect("numeric x-axis must still render");
        assert!(svg.contains("<svg"));
        assert!(svg.contains("Demo"));
        assert_eq!(svg.matches("class=\"line-plot-").count(), 1);
    }

    #[test]
    fn theme_text_color_drives_axis_and_labels() {
        let svg = render_xychart_diagram_to_svg(SAMPLE, &MermaidTheme::dark())
            .expect("dark theme must render");
        assert!(svg.contains(&format!("fill=\"{}\"", MermaidTheme::dark().text_color)));
    }

    #[test]
    fn y_axis_label_only_auto_ranges_from_data() {
        let src = "xychart-beta\n    x-axis [a, b, c]\n    y-axis \"score\"\n    line [10, 20, 30]";
        let chart = parse_xychart(src).expect("must parse");
        assert_eq!(chart.y_title, "score");
        assert_eq!((chart.y_min, chart.y_max), (10.0, 30.0));
    }

    #[test]
    fn missing_plot_is_rejected() {
        let src = "xychart-beta\n    x-axis [a, b]\n    y-axis 0 --> 10";
        assert!(render_xychart_diagram_to_svg(src, &MermaidTheme::light()).is_err());
    }

    #[test]
    fn non_xychart_source_is_rejected() {
        assert!(parse_xychart("flowchart TD\n    A --> B").is_err());
    }

    #[test]
    fn empty_line_series_is_rejected() {
        // `line []` declares a series with no values: still no plottable data.
        assert!(parse_xychart("xychart-beta\n    x-axis [a, b]\n    line []").is_err());
    }

    #[test]
    fn categorical_points_stay_on_plot() {
        let (plot_x, plot_w) = (60.0, 600.0);
        // A series longer than the category list, and an empty category list:
        // every point must still land within [plot_x, plot_x + plot_w].
        for axis in [
            XAxis::Category(vec!["a".to_string(), "b".to_string()]),
            XAxis::Category(Vec::new()),
        ] {
            let layout = layout_x_axis(&axis, plot_x, plot_w, 4);
            for i in 0..4 {
                let x = layout.series_point_x(i);
                assert!(
                    (plot_x..=plot_x + plot_w).contains(&x),
                    "point {i} at {x} escaped the plot for {axis:?}"
                );
            }
        }
    }

    #[test]
    fn numeric_single_point_sits_at_left_edge() {
        let (plot_x, plot_w) = (60.0, 600.0);
        let layout = layout_x_axis(
            &XAxis::Numeric {
                min: 0.0,
                max: 10.0,
            },
            plot_x,
            plot_w,
            1,
        );
        let XGeom::Numeric { x0, .. } = layout.geom else {
            panic!("expected numeric geometry");
        };
        assert_eq!(layout.series_point_x(0), x0);
    }

    #[test]
    fn numeric_series_share_x_domain_across_lengths() {
        // Built with the longest series' length (4); `series_point_x` ignores any
        // individual series length, so every series maps index -> x identically.
        let (plot_x, plot_w) = (60.0, 600.0);
        let layout = layout_x_axis(
            &XAxis::Numeric {
                min: 0.0,
                max: 10.0,
            },
            plot_x,
            plot_w,
            4,
        );
        let XGeom::Numeric { x0, x1 } = layout.geom else {
            panic!("expected numeric geometry");
        };
        // Spacing uses the shared count (4 -> denominator 3), not a per-series one.
        assert_eq!(layout.series_point_x(0), x0);
        assert_eq!(layout.series_point_x(3), x1);
        assert!((layout.series_point_x(1) - (x0 + (x1 - x0) / 3.0)).abs() < 1e-9);
    }
}
