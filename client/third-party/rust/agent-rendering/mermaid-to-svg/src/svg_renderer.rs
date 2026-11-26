use crate::ast::{EdgeStyle, NodeShape};
use crate::config::RenderConfig;
use crate::layout::{LayoutEdge, LayoutNode, LayoutResult, LayoutSubgraph};
use crate::text_wrap::{
    line_width_words, measure_wrapped_lines_with_font_size, scale_char_width, wrap_text_lines,
    wrapped_text_height_with_font_size, DEFAULT_CHAR_WIDTH, DEFAULT_FONT_SIZE, DEFAULT_LINE_HEIGHT,
    DEFAULT_WRAP_WIDTH,
};
use crate::theme::MermaidTheme;

/// The arrowhead marker has refX="5" with viewBox 0..10 and markerWidth 8.
/// The tip at viewBox x=10 extends (10−5)/10 × 8 = 4 px past the reference point.
/// We shorten each arrowed edge by this amount so the tip lands exactly on the
/// target node border — matching how mermaid.js renders edges.
const EDGE_ARROWHEAD_OFFSET: f64 = 4.0;
const EDGE_ARROWHEAD_OFFSET_THICK: f64 = 5.5; // markerWidth 11 × (10−5)/10

const EDGE_LABEL_CHAR_WIDTH: f64 = DEFAULT_CHAR_WIDTH;
const EDGE_LABEL_PADDING_H: f64 = 2.0;
const EDGE_LABEL_PADDING_V: f64 = 2.0;
const EDGE_LABEL_BG_OPACITY: f64 = 0.8;
const SUBGRAPH_TITLE_TOP_MARGIN: f64 = 0.0;
const STATE_CHAR_WIDTH: f64 = 6.7;
const DEFAULT_FONT_FAMILY: &str = "Trebuchet MS, verdana, arial, sans-serif";

pub fn render(layout: &LayoutResult, theme: &MermaidTheme) -> String {
    render_with_config(layout, theme, &RenderConfig::default())
}

pub fn render_with_config(
    layout: &LayoutResult,
    theme: &MermaidTheme,
    config: &RenderConfig,
) -> String {
    let is_state_diagram = layout.nodes.values().any(|node| {
        matches!(
            node.shape,
            NodeShape::StartState | NodeShape::EndState | NodeShape::ForkJoin
        )
    });
    let mut svg = SvgRenderer::new(
        layout.width,
        layout.height,
        theme,
        is_state_diagram,
        SvgRenderOptions::from_render_config(config),
    );
    svg.render(layout)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EdgeCurve {
    Basis,
    Linear,
}

#[derive(Debug, Clone)]
struct SvgRenderOptions {
    font_family: String,
    font_size: f64,
    wrapping_width: f64,
    edge_curve: EdgeCurve,
}

impl Default for SvgRenderOptions {
    fn default() -> Self {
        Self {
            font_family: DEFAULT_FONT_FAMILY.to_string(),
            font_size: DEFAULT_FONT_SIZE,
            wrapping_width: DEFAULT_WRAP_WIDTH,
            edge_curve: EdgeCurve::Basis,
        }
    }
}

impl SvgRenderOptions {
    fn from_render_config(config: &RenderConfig) -> Self {
        let default = Self::default();
        Self {
            font_family: config.font_family.clone().unwrap_or(default.font_family),
            font_size: config.font_size_px().unwrap_or(default.font_size),
            wrapping_width: config
                .flowchart
                .wrapping_width
                .map(f64::from)
                .unwrap_or(default.wrapping_width),
            edge_curve: config
                .flowchart
                .curve
                .as_deref()
                .map(EdgeCurve::from_mermaid_name)
                .unwrap_or(default.edge_curve),
        }
    }
}

impl EdgeCurve {
    fn from_mermaid_name(name: &str) -> Self {
        if name.eq_ignore_ascii_case("linear") {
            Self::Linear
        } else {
            Self::Basis
        }
    }
}

struct SvgRenderer<'a> {
    width: f64,
    height: f64,
    theme: &'a MermaidTheme,
    is_state_diagram: bool,
    options: SvgRenderOptions,
    output: String,
}

impl<'a> SvgRenderer<'a> {
    fn new(
        width: f64,
        height: f64,
        theme: &'a MermaidTheme,
        is_state_diagram: bool,
        options: SvgRenderOptions,
    ) -> Self {
        Self {
            width,
            height,
            theme,
            is_state_diagram,
            options,
            output: String::new(),
        }
    }

    fn render(&mut self, layout: &LayoutResult) -> String {
        self.write_header();
        self.write_defs();

        for subgraph in &layout.subgraphs {
            self.render_subgraph_background(subgraph);
        }

        for edge in &layout.edges {
            self.render_edge_line(edge);
        }

        let mut nodes: Vec<&LayoutNode> = layout.nodes.values().collect();
        nodes.sort_by(|a, b| a.id.cmp(&b.id));
        for node in nodes {
            self.render_node(node);
        }

        for subgraph in &layout.subgraphs {
            self.render_subgraph_title(subgraph);
        }

        self.render_edge_labels(&layout.edges);

        self.write_footer();
        std::mem::take(&mut self.output)
    }

    /// Matches mermaid's SVG behavior: sizing via setupGraphViewbox.js and background via SVG style
    /// (mermaid-cli src/index.js sets svg.style.backgroundColor), with a background rect for rasterizers.
    fn write_header(&mut self) {
        self.output.push_str(&format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<svg width="{:.0}" height="{:.0}" viewBox="0 0 {:.0} {:.0}" xmlns="http://www.w3.org/2000/svg" style="background-color: {};">
<rect x="0" y="0" width="{:.0}" height="{:.0}" fill="{}" stroke="none"/>
"#,
            self.width,
            self.height,
            self.width,
            self.height,
            self.theme.background,
            self.width,
            self.height,
            self.theme.background
        ));
    }

    fn write_defs(&mut self) {
        self.output.push_str(&format!(
            r#"<defs>
  <marker id="arrowhead" markerWidth="8" markerHeight="8" refX="5" refY="5" orient="auto" markerUnits="userSpaceOnUse" viewBox="0 0 10 10">
    <path d="M 0 0 L 10 5 L 0 10 z" fill="{}" stroke="{}" stroke-width="1"/>
  </marker>
  <marker id="arrowhead-thick" markerWidth="11" markerHeight="11" refX="5" refY="5" orient="auto" markerUnits="userSpaceOnUse" viewBox="0 0 10 10">
    <path d="M 0 0 L 10 5 L 0 10 z" fill="{0}" stroke="{0}" stroke-width="1"/>
  </marker>
</defs>
"#,
            self.theme.edge_color, self.theme.edge_color
        ));
    }

    fn write_footer(&mut self) {
        self.output.push_str("</svg>\n");
    }

    fn render_subgraph_background(&mut self, subgraph: &LayoutSubgraph) {
        self.output.push_str(&format!(
            r#"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" fill="{}" stroke="{}" stroke-width="1"/>\n"#,
            subgraph.x, subgraph.y, subgraph.width, subgraph.height,
            self.theme.subgraph_fill, self.theme.subgraph_stroke
        ));
    }

    fn render_subgraph_title(&mut self, subgraph: &LayoutSubgraph) {
        if let Some(title) = &subgraph.title {
            let char_width = scale_char_width(DEFAULT_CHAR_WIDTH, self.options.font_size);
            let lines = wrap_text_lines(title, self.options.wrapping_width, char_width);
            if lines.is_empty() {
                return;
            }
            let (_, text_height) =
                measure_wrapped_lines_with_font_size(&lines, char_width, self.options.font_size);
            let title_x = subgraph.x + subgraph.width / 2.0;
            let title_y = subgraph.y + SUBGRAPH_TITLE_TOP_MARGIN + text_height / 2.0;
            self.render_text_lines(
                title_x,
                title_y,
                &lines,
                self.options.font_size,
                DEFAULT_LINE_HEIGHT,
                &self.theme.text_color,
            );
        }
    }

    fn render_node(&mut self, node: &LayoutNode) {
        match node.shape {
            NodeShape::Rectangle => self.render_rectangle(node, 0.0),
            NodeShape::RoundedRectangle => self.render_rectangle(node, 5.0),
            NodeShape::Stadium => self.render_rectangle(node, node.height / 2.0),
            NodeShape::Diamond => self.render_diamond(node),
            NodeShape::Circle => self.render_circle(node),
            NodeShape::StartState => self.render_start_state(node),
            NodeShape::EndState => self.render_end_state(node),
            NodeShape::ForkJoin => self.render_fork_join(node),
            NodeShape::Hexagon => self.render_hexagon(node),
            NodeShape::Cylinder => self.render_cylinder(node),
            NodeShape::Subroutine => self.render_subroutine(node),
            NodeShape::Asymmetric => self.render_asymmetric(node),
        }
    }

    fn render_rectangle(&mut self, node: &LayoutNode, rx: f64) {
        let x = node.x - node.width / 2.0;
        let y = node.y - node.height / 2.0;
        let fill = node.fill_color.as_ref().unwrap_or(&self.theme.node_fill);
        let stroke = node
            .stroke_color
            .as_ref()
            .unwrap_or(&self.theme.node_stroke);

        self.output.push_str(&format!(
            r#"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" rx="{:.1}" fill="{}" stroke="{}" stroke-width="1"/>
"#,
            x, y, node.width, node.height, rx, fill, stroke
        ));

        self.render_text(node.x, node.y, &node.label);
    }

    fn render_start_state(&mut self, node: &LayoutNode) {
        let r = node.width.min(node.height) / 2.0;
        self.output.push_str(&format!(
            r#"<circle cx="{:.1}" cy="{:.1}" r="{:.1}" fill="{}" stroke="{}" stroke-width="1.5"/>
"#,
            node.x, node.y, r, self.theme.edge_color, self.theme.edge_color
        ));
    }

    fn render_end_state(&mut self, node: &LayoutNode) {
        let outer_r = node.width.min(node.height) / 2.0;
        let inner_r = (outer_r - 4.0).max(outer_r * 0.55).min(outer_r - 2.0);
        self.output.push_str(&format!(
            r#"<circle cx="{:.1}" cy="{:.1}" r="{:.1}" fill="{}" stroke="{}" stroke-width="1"/>
"#,
            node.x, node.y, outer_r, self.theme.node_stroke, self.theme.background
        ));
        self.output.push_str(&format!(
            r#"<circle cx="{:.1}" cy="{:.1}" r="{:.1}" fill="{}" stroke="none"/>
"#,
            node.x, node.y, inner_r, self.theme.background
        ));
    }

    fn render_fork_join(&mut self, node: &LayoutNode) {
        let x = node.x - node.width / 2.0;
        let y = node.y - node.height / 2.0;
        self.output.push_str(&format!(
            r#"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" rx="1" fill="{}" stroke="{}" stroke-width="1"/>
"#,
            x, y, node.width, node.height, self.theme.edge_color, self.theme.edge_color
        ));
    }

    fn render_diamond(&mut self, node: &LayoutNode) {
        let hw = node.width / 2.0;
        let hh = node.height / 2.0;
        let fill = node.fill_color.as_ref().unwrap_or(&self.theme.node_fill);
        let stroke = node
            .stroke_color
            .as_ref()
            .unwrap_or(&self.theme.node_stroke);

        let points = format!(
            "{:.1},{:.1} {:.1},{:.1} {:.1},{:.1} {:.1},{:.1}",
            node.x,
            node.y - hh,
            node.x + hw,
            node.y,
            node.x,
            node.y + hh,
            node.x - hw,
            node.y
        );

        self.output.push_str(&format!(
            r#"<polygon points="{}" fill="{}" stroke="{}" stroke-width="1"/>
"#,
            points, fill, stroke
        ));

        self.render_text(node.x, node.y, &node.label);
    }

    fn render_circle(&mut self, node: &LayoutNode) {
        let r = node.width.min(node.height) / 2.0;
        let fill = node.fill_color.as_ref().unwrap_or(&self.theme.node_fill);
        let stroke = node
            .stroke_color
            .as_ref()
            .unwrap_or(&self.theme.node_stroke);

        self.output.push_str(&format!(
            r#"<circle cx="{:.1}" cy="{:.1}" r="{:.1}" fill="{}" stroke="{}" stroke-width="1"/>
"#,
            node.x, node.y, r, fill, stroke
        ));

        self.render_text(node.x, node.y, &node.label);
    }

    fn render_hexagon(&mut self, node: &LayoutNode) {
        let hw = node.width / 2.0;
        let hh = node.height / 2.0;
        let inset = node.height / 3.0;
        let fill = node.fill_color.as_ref().unwrap_or(&self.theme.node_fill);
        let stroke = node
            .stroke_color
            .as_ref()
            .unwrap_or(&self.theme.node_stroke);

        let points = format!(
            "{:.1},{:.1} {:.1},{:.1} {:.1},{:.1} {:.1},{:.1} {:.1},{:.1} {:.1},{:.1}",
            node.x - hw + inset,
            node.y - hh,
            node.x + hw - inset,
            node.y - hh,
            node.x + hw,
            node.y,
            node.x + hw - inset,
            node.y + hh,
            node.x - hw + inset,
            node.y + hh,
            node.x - hw,
            node.y
        );

        self.output.push_str(&format!(
            r#"<polygon points="{}" fill="{}" stroke="{}" stroke-width="1"/>
"#,
            points, fill, stroke
        ));

        self.render_text(node.x, node.y, &node.label);
    }

    fn render_cylinder(&mut self, node: &LayoutNode) {
        let hw = node.width / 2.0;
        let hh = node.height / 2.0;
        let ellipse_ry = (hw / 4.0).min(hh / 2.0);
        let fill = node.fill_color.as_ref().unwrap_or(&self.theme.node_fill);
        let stroke = node
            .stroke_color
            .as_ref()
            .unwrap_or(&self.theme.node_stroke);

        let x = node.x - hw;
        let y = node.y - hh;
        let body_top = y + ellipse_ry;
        let body_bottom = node.y + hh - ellipse_ry;

        self.output.push_str(&format!(
            r#"<path d="M {:.1} {:.1} L {:.1} {:.1} A {:.1} {:.1} 0 0 0 {:.1} {:.1} L {:.1} {:.1} A {:.1} {:.1} 0 0 0 {:.1} {:.1} Z" fill="{}" stroke="{}" stroke-width="1"/>
"#,
            x,
            body_top,
            x,
            body_bottom,
            hw,
            ellipse_ry,
            node.x + hw,
            body_bottom,
            node.x + hw,
            body_top,
            hw,
            ellipse_ry,
            x,
            body_top,
            fill,
            stroke
        ));

        self.output.push_str(&format!(
            r#"<ellipse cx="{:.1}" cy="{:.1}" rx="{:.1}" ry="{:.1}" fill="{}" stroke="{}" stroke-width="1"/>
"#,
            node.x, body_top, hw, ellipse_ry, fill, stroke
        ));

        // Center text in the cylinder body (below the top ellipse cap)
        let body_center_y = (body_top + body_bottom) / 2.0;
        self.render_text(node.x, body_center_y, &node.label);
    }

    fn render_subroutine(&mut self, node: &LayoutNode) {
        let x = node.x - node.width / 2.0;
        let y = node.y - node.height / 2.0;
        let bar_inset = 8.0;
        let fill = node.fill_color.as_ref().unwrap_or(&self.theme.node_fill);
        let stroke = node
            .stroke_color
            .as_ref()
            .unwrap_or(&self.theme.node_stroke);

        self.output.push_str(&format!(
            r#"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" fill="{}" stroke="{}" stroke-width="1"/>
"#,
            x, y, node.width, node.height, fill, stroke
        ));

        self.output.push_str(&format!(
            r#"<line x1="{:.1}" y1="{:.1}" x2="{:.1}" y2="{:.1}" stroke="{}" stroke-width="1"/>
"#,
            x + bar_inset,
            y,
            x + bar_inset,
            y + node.height,
            stroke
        ));
        self.output.push_str(&format!(
            r#"<line x1="{:.1}" y1="{:.1}" x2="{:.1}" y2="{:.1}" stroke="{}" stroke-width="1"/>
"#,
            x + node.width - bar_inset,
            y,
            x + node.width - bar_inset,
            y + node.height,
            stroke
        ));

        self.render_text(node.x, node.y, &node.label);
    }

    fn render_asymmetric(&mut self, node: &LayoutNode) {
        let hw = node.width / 2.0;
        let hh = node.height / 2.0;
        let point_offset = hh;
        let fill = node.fill_color.as_ref().unwrap_or(&self.theme.node_fill);
        let stroke = node
            .stroke_color
            .as_ref()
            .unwrap_or(&self.theme.node_stroke);

        // Mermaid's `>text]` flag shape: indent (V-notch) on the LEFT, flat RIGHT.
        let points = format!(
            "{:.1},{:.1} {:.1},{:.1} {:.1},{:.1} {:.1},{:.1} {:.1},{:.1}",
            node.x - hw + point_offset,
            node.y - hh,
            node.x + hw,
            node.y - hh,
            node.x + hw,
            node.y + hh,
            node.x - hw + point_offset,
            node.y + hh,
            node.x - hw,
            node.y,
        );

        self.output.push_str(&format!(
            r#"<polygon points="{}" fill="{}" stroke="{}" stroke-width="1"/>
"#,
            points, fill, stroke
        ));

        self.render_text(node.x + point_offset / 4.0, node.y, &node.label);
    }

    fn render_text(&mut self, x: f64, y: f64, text: &str) {
        let char_width = if self.is_state_diagram {
            scale_char_width(STATE_CHAR_WIDTH, self.options.font_size)
        } else {
            scale_char_width(DEFAULT_CHAR_WIDTH, self.options.font_size)
        };
        let lines = wrap_text_lines(text, self.options.wrapping_width, char_width);
        if lines.is_empty() {
            return;
        }
        self.render_text_lines(
            x,
            y,
            &lines,
            self.options.font_size,
            DEFAULT_LINE_HEIGHT,
            &self.theme.text_color,
        );
    }

    fn render_text_lines(
        &mut self,
        x: f64,
        y: f64,
        lines: &[Vec<String>],
        font_size: f64,
        line_height: f64,
        color: &str,
    ) {
        let line_height_px = font_size * line_height;
        // With dominant-baseline="central", the y attribute positions the vertical
        // center of the text glyph. We distribute n lines evenly around the center y.
        let start_y = y - (lines.len() as f64 - 1.0) * line_height_px / 2.0;

        let font_family = Self::escape_xml(&self.options.font_family);
        self.output.push_str(&format!(
            r#"<text text-anchor="middle" dominant-baseline="central" font-family="{}" font-size="{:.0}" fill="{}">
"#,
            font_family, font_size, color
        ));

        for (i, line) in lines.iter().enumerate() {
            let line_y = start_y + (i as f64 * line_height_px);
            let line_text = line.join(" ");
            self.output.push_str(&format!(
                r#"<tspan x="{:.1}" y="{:.1}">{}</tspan>"#,
                x,
                line_y,
                Self::escape_xml(&line_text)
            ));
            self.output.push('\n');
        }

        self.output.push_str("</text>\n");
    }

    /// Matches Mermaid flowchart edge thickness/pattern defaults
    /// (see packages/mermaid/src/diagrams/flowchart/styles.ts and rendering-elements/edges.js).
    fn render_edge_line(&mut self, edge: &LayoutEdge) {
        if edge.points.len() < 2 {
            return;
        }

        let has_arrow = matches!(
            edge.style,
            EdgeStyle::Arrow | EdgeStyle::DottedArrow | EdgeStyle::ThickArrow
        );
        let is_dotted = matches!(edge.style, EdgeStyle::DottedArrow | EdgeStyle::DottedLine);
        let is_thick = matches!(edge.style, EdgeStyle::ThickArrow | EdgeStyle::ThickLine);

        let marker = match (has_arrow, is_thick) {
            (true, true) => r#" marker-end="url(#arrowhead-thick)""#,
            (true, false) => r#" marker-end="url(#arrowhead)""#,
            _ => "",
        };
        let stroke_width = if is_thick { 3.5 } else { 1.0 };
        let dash_array = if is_dotted {
            // Match mermaid's dotted style: round-capped short dashes for dot look
            r#" stroke-dasharray="3 3""#
        } else {
            ""
        };

        let mut points = edge.points.clone();
        if has_arrow {
            let offset = if is_thick {
                EDGE_ARROWHEAD_OFFSET_THICK
            } else {
                EDGE_ARROWHEAD_OFFSET
            };
            Self::shorten_end_for_marker(&mut points, offset);
        }

        let d = self.edge_path_d(&points);

        self.output.push_str(&format!(
            r#"<path d="{}" fill="none" stroke="{}" stroke-width="{:.1}" stroke-linecap="round" stroke-linejoin="round"{}{}/>
"#,
            d, self.theme.edge_color, stroke_width, dash_array, marker
        ));
    }

    fn shorten_end_for_marker(points: &mut [(f64, f64)], offset: f64) {
        if points.len() < 2 || offset <= 0.0 {
            return;
        }

        let last_idx = points.len() - 1;
        let prev = points[last_idx - 1];
        let last = points[last_idx];

        let dx = last.0 - prev.0;
        let dy = last.1 - prev.1;
        let len = (dx * dx + dy * dy).sqrt();
        if len <= offset {
            return;
        }

        let ux = dx / len;
        let uy = dy / len;
        points[last_idx] = (last.0 - ux * offset, last.1 - uy * offset);
    }

    fn edge_path_d(&self, points: &[(f64, f64)]) -> String {
        match self.options.edge_curve {
            EdgeCurve::Basis => {
                let points = Self::fix_corners(points);
                Self::basis_spline_path_d(&points)
            }
            EdgeCurve::Linear => Self::linear_path_d(points),
        }
    }

    fn linear_path_d(points: &[(f64, f64)]) -> String {
        let Some((first_x, first_y)) = points.first().copied() else {
            return String::new();
        };
        let mut d = format!("M{first_x:.1},{first_y:.1}");
        for (x, y) in points.iter().skip(1) {
            d.push_str(&format!("L{x:.1},{y:.1}"));
        }
        d
    }

    fn basis_spline_path_d(points: &[(f64, f64)]) -> String {
        if points.is_empty() {
            return String::new();
        }

        let mut d = String::new();

        let mut x0 = f64::NAN;
        let mut y0 = f64::NAN;
        let mut x1 = f64::NAN;
        let mut y1 = f64::NAN;
        let mut point_state = 0;

        for &(x, y) in points {
            match point_state {
                0 => {
                    point_state = 1;
                    d.push_str(&format!("M{x:.1},{y:.1}"));
                }
                1 => {
                    point_state = 2;
                }
                2 => {
                    point_state = 3;
                    d.push_str(&format!(
                        "L{:.1},{:.1}",
                        (5.0 * x0 + x1) / 6.0,
                        (5.0 * y0 + y1) / 6.0
                    ));
                    d.push_str(&Self::basis_point(x0, y0, x1, y1, x, y));
                }
                _ => {
                    d.push_str(&Self::basis_point(x0, y0, x1, y1, x, y));
                }
            }

            x0 = x1;
            x1 = x;
            y0 = y1;
            y1 = y;
        }

        match point_state {
            3 => {
                d.push_str(&Self::basis_point(x0, y0, x1, y1, x1, y1));
                d.push_str(&format!("L{x1:.1},{y1:.1}"));
            }
            2 => {
                d.push_str(&format!("L{x1:.1},{y1:.1}"));
            }
            _ => {}
        }

        d
    }

    fn basis_point(x0: f64, y0: f64, x1: f64, y1: f64, x: f64, y: f64) -> String {
        format!(
            "C{:.1},{:.1} {:.1},{:.1} {:.1},{:.1}",
            (2.0 * x0 + x1) / 3.0,
            (2.0 * y0 + y1) / 3.0,
            (x0 + 2.0 * x1) / 3.0,
            (y0 + 2.0 * y1) / 3.0,
            (x0 + 4.0 * x1 + x) / 6.0,
            (y0 + 4.0 * y1 + y) / 6.0,
        )
    }

    fn fix_corners(points: &[(f64, f64)]) -> Vec<(f64, f64)> {
        let corner_positions = Self::corner_positions(points);
        let mut new_points = Vec::new();
        for (idx, point) in points.iter().enumerate() {
            if corner_positions.contains(&idx) {
                let prev_point = points[idx - 1];
                let next_point = points[idx + 1];
                let corner_point = *point;
                let new_prev = Self::find_adjacent_point(prev_point, corner_point, 5.0);
                let new_next = Self::find_adjacent_point(next_point, corner_point, 5.0);
                let x_diff = new_next.0 - new_prev.0;
                let y_diff = new_next.1 - new_prev.1;
                let mut new_corner = corner_point;
                let a = (2.0_f64).sqrt() * 2.0;
                if (next_point.0 - prev_point.0).abs() > 10.0
                    && (next_point.1 - prev_point.1).abs() >= 10.0
                {
                    if (corner_point.0 - new_prev.0).abs() < f64::EPSILON {
                        new_corner = (
                            if x_diff < 0.0 {
                                new_prev.0 - 5.0 + a
                            } else {
                                new_prev.0 + 5.0 - a
                            },
                            if y_diff < 0.0 {
                                new_prev.1 - a
                            } else {
                                new_prev.1 + a
                            },
                        );
                    } else {
                        new_corner = (
                            if x_diff < 0.0 {
                                new_prev.0 - a
                            } else {
                                new_prev.0 + a
                            },
                            if y_diff < 0.0 {
                                new_prev.1 - 5.0 + a
                            } else {
                                new_prev.1 + 5.0 - a
                            },
                        );
                    }
                }
                new_points.push(new_prev);
                new_points.push(new_corner);
                new_points.push(new_next);
            } else {
                new_points.push(*point);
            }
        }
        new_points
    }

    fn corner_positions(points: &[(f64, f64)]) -> Vec<usize> {
        let mut positions = Vec::new();
        if points.len() < 3 {
            return positions;
        }
        for i in 1..points.len() - 1 {
            let prev = points[i - 1];
            let curr = points[i];
            let next = points[i + 1];
            if ((prev.0 - curr.0).abs() < f64::EPSILON
                && (curr.1 - next.1).abs() < f64::EPSILON
                && (curr.0 - next.0).abs() > 5.0
                && (curr.1 - prev.1).abs() > 5.0)
                || ((prev.1 - curr.1).abs() < f64::EPSILON
                    && (curr.0 - next.0).abs() < f64::EPSILON
                    && (curr.0 - prev.0).abs() > 5.0
                    && (curr.1 - next.1).abs() > 5.0)
            {
                positions.push(i);
            }
        }
        positions
    }

    fn find_adjacent_point(a: (f64, f64), b: (f64, f64), distance: f64) -> (f64, f64) {
        let x_diff = b.0 - a.0;
        let y_diff = b.1 - a.1;
        let length = (x_diff * x_diff + y_diff * y_diff).sqrt();
        if length == 0.0 {
            return a;
        }
        let ratio = distance / length;
        (b.0 - ratio * x_diff, b.1 - ratio * y_diff)
    }

    fn render_edge_labels(&mut self, edges: &[LayoutEdge]) {
        struct LabelInfo {
            x: f64,
            y: f64,
            width: f64,
            height: f64,
            lines: Vec<Vec<String>>,
        }

        let mut labels: Vec<LabelInfo> = Vec::new();
        let char_width = if self.is_state_diagram {
            scale_char_width(STATE_CHAR_WIDTH, self.options.font_size)
        } else {
            scale_char_width(EDGE_LABEL_CHAR_WIDTH, self.options.font_size)
        };
        for edge in edges {
            let Some(label) = &edge.label else {
                continue;
            };
            if label.trim().is_empty() || (edge.label_pos.is_none() && edge.points.len() < 2) {
                continue;
            }

            let (label_x, label_y) = if let Some((x, y)) = edge.label_pos {
                if x > 0.0 && y > 0.0 {
                    (x, y)
                } else {
                    let label_points = Self::fix_corners(&edge.points);
                    Self::label_position(&label_points)
                }
            } else {
                let label_points = Self::fix_corners(&edge.points);
                Self::label_position(&label_points)
            };

            let lines = wrap_text_lines(label, self.options.wrapping_width, char_width);
            if lines.is_empty() {
                continue;
            }

            let max_line_width = lines
                .iter()
                .map(|line| line_width_words(line, char_width))
                .fold(0.0, f64::max);
            let total_height =
                wrapped_text_height_with_font_size(lines.len(), self.options.font_size);
            let rect_width = max_line_width + EDGE_LABEL_PADDING_H * 2.0;
            let rect_height = total_height + EDGE_LABEL_PADDING_V * 2.0;

            labels.push(LabelInfo {
                x: label_x,
                y: label_y,
                width: rect_width,
                height: rect_height,
                lines,
            });
        }

        const MIN_SEPARATION: f64 = 8.0;
        const MAX_ITERATIONS: usize = 10;

        for _ in 0..MAX_ITERATIONS {
            let mut any_collision = false;

            for i in 0..labels.len() {
                for j in (i + 1)..labels.len() {
                    let a_left = labels[i].x - labels[i].width / 2.0 - MIN_SEPARATION;
                    let a_right = labels[i].x + labels[i].width / 2.0 + MIN_SEPARATION;
                    let a_top = labels[i].y - labels[i].height / 2.0 - MIN_SEPARATION;
                    let a_bottom = labels[i].y + labels[i].height / 2.0 + MIN_SEPARATION;

                    let b_left = labels[j].x - labels[j].width / 2.0 - MIN_SEPARATION;
                    let b_right = labels[j].x + labels[j].width / 2.0 + MIN_SEPARATION;
                    let b_top = labels[j].y - labels[j].height / 2.0 - MIN_SEPARATION;
                    let b_bottom = labels[j].y + labels[j].height / 2.0 + MIN_SEPARATION;

                    let overlap_x = a_right > b_left && b_right > a_left;
                    let overlap_y = a_bottom > b_top && b_bottom > a_top;

                    if overlap_x && overlap_y {
                        any_collision = true;
                        let dx = labels[j].x - labels[i].x;
                        let dy = labels[j].y - labels[i].y;

                        let overlap_amount_x = (a_right - b_left).min(b_right - a_left);
                        let overlap_amount_y = (a_bottom - b_top).min(b_bottom - a_top);

                        if overlap_amount_x < overlap_amount_y {
                            let shift = overlap_amount_x / 2.0;
                            if dx >= 0.0 {
                                labels[i].x -= shift;
                                labels[j].x += shift;
                            } else {
                                labels[i].x += shift;
                                labels[j].x -= shift;
                            }
                        } else {
                            let shift = overlap_amount_y / 2.0;
                            if dy >= 0.0 {
                                labels[i].y -= shift;
                                labels[j].y += shift;
                            } else {
                                labels[i].y += shift;
                                labels[j].y -= shift;
                            }
                        }
                    }
                }
            }

            if !any_collision {
                break;
            }
        }

        for info in &labels {
            let rect_x = info.x - info.width / 2.0;
            let rect_y = info.y - info.height / 2.0;

            self.output.push_str(&format!(
                r#"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" fill="rgba(232,232,232,{})" rx="2"/>
"#,
                rect_x, rect_y, info.width, info.height, EDGE_LABEL_BG_OPACITY
            ));

            self.render_text_lines(
                info.x,
                info.y,
                &info.lines,
                self.options.font_size,
                DEFAULT_LINE_HEIGHT,
                &self.theme.text_color,
            );
        }
    }

    fn label_position(points: &[(f64, f64)]) -> (f64, f64) {
        if points.len() < 2 {
            return points.first().copied().unwrap_or((0.0, 0.0));
        }

        let mut segment_lengths = Vec::with_capacity(points.len() - 1);
        let mut total_length = 0.0;

        for i in 0..points.len() - 1 {
            let dx = points[i + 1].0 - points[i].0;
            let dy = points[i + 1].1 - points[i].1;
            let len = (dx * dx + dy * dy).sqrt();
            segment_lengths.push(len);
            total_length += len;
        }

        if total_length < 0.001 {
            return points[0];
        }

        let target_distance = total_length * 0.5;
        let mut accumulated = 0.0;

        for (i, &seg_len) in segment_lengths.iter().enumerate() {
            if accumulated + seg_len >= target_distance {
                let remaining = target_distance - accumulated;
                let t = if seg_len > 0.001 {
                    remaining / seg_len
                } else {
                    0.0
                };
                let x = points[i].0 + t * (points[i + 1].0 - points[i].0);
                let y = points[i].1 + t * (points[i + 1].1 - points[i].1);
                return (x, y);
            }
            accumulated += seg_len;
        }

        let last = points.len() - 1;
        (
            (points[0].0 + points[last].0) / 2.0,
            (points[0].1 + points[last].1) / 2.0,
        )
    }

    fn escape_xml(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&#39;")
    }
}
