use crate::error::MermaidError;
use crate::theme::MermaidTheme;

/// Node type in the mindmap, following Mermaid 11.12.2 nodeType enum.
#[derive(Debug, Clone, Copy, PartialEq)]
enum MindmapNodeType {
    Default,     // no-border — "rounded" shape
    Rect,        // [text]
    RoundedRect, // (text)
    Circle,      // ((text))
    #[allow(dead_code)]
    Cloud, // )text(
    Bang,        // ))text((
    Hexagon,     // {{text}}
}

/// A node in the mindmap tree.
#[derive(Debug, Clone)]
struct MindmapNode {
    id: String,
    label: String,
    node_type: MindmapNodeType,
    children: Vec<MindmapNode>,
    section: Option<usize>,
}

/// Colors for a section (branch) of the mindmap.
struct SectionColors {
    fill: &'static str,
    text: &'static str,
    edge: &'static str,
}

/// Root fill: hsl(240, 100%, 46.27%) from reference SVG.
const ROOT_FILL: &str = "hsl(240, 100%, 46.27%)";
const ROOT_TEXT: &str = "#ffffff";

/// Mermaid 11.12.2 default theme section colors, extracted from reference SVG.
/// Each section-N fill is hsl(H, 100%, ~73-76%).
const SECTION_COLORS: &[SectionColors] = &[
    // Section 0: hsl(60, 100%, 73.53%) — yellow
    SectionColors {
        fill: "hsl(60, 100%, 73.53%)",
        text: "black",
        edge: "hsl(60, 100%, 73.53%)",
    },
    // Section 1: hsl(80, 100%, 76.27%) — yellow-green
    SectionColors {
        fill: "hsl(80, 100%, 76.27%)",
        text: "black",
        edge: "hsl(80, 100%, 76.27%)",
    },
    // Section 2: hsl(270, 100%, 76.27%) — purple
    SectionColors {
        fill: "hsl(270, 100%, 76.27%)",
        text: "#ffffff",
        edge: "hsl(270, 100%, 76.27%)",
    },
    // Section 3: hsl(300, 100%, 76.27%) — magenta
    SectionColors {
        fill: "hsl(300, 100%, 76.27%)",
        text: "black",
        edge: "hsl(300, 100%, 76.27%)",
    },
    // Section 4: hsl(330, 100%, 76.27%) — pink
    SectionColors {
        fill: "hsl(330, 100%, 76.27%)",
        text: "black",
        edge: "hsl(330, 100%, 76.27%)",
    },
    // Section 5: hsl(0, 100%, 76.27%) — red
    SectionColors {
        fill: "hsl(0, 100%, 76.27%)",
        text: "black",
        edge: "hsl(0, 100%, 76.27%)",
    },
    // Section 6: hsl(30, 100%, 76.27%) — orange
    SectionColors {
        fill: "hsl(30, 100%, 76.27%)",
        text: "black",
        edge: "hsl(30, 100%, 76.27%)",
    },
    // Section 7: hsl(90, 100%, 76.27%) — lime
    SectionColors {
        fill: "hsl(90, 100%, 76.27%)",
        text: "black",
        edge: "hsl(90, 100%, 76.27%)",
    },
];

fn section_color(section: usize) -> &'static SectionColors {
    &SECTION_COLORS[section % SECTION_COLORS.len()]
}

/// Layout result for a placed node.
#[derive(Debug, Clone)]
struct PlacedNode {
    #[allow(dead_code)]
    id: String,
    label: String,
    node_type: MindmapNodeType,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    section: Option<usize>,
    is_root: bool,
}

/// Layout result for an edge.
#[derive(Debug, Clone)]
struct PlacedEdge {
    from_x: f64,
    from_y: f64,
    to_x: f64,
    to_y: f64,
    section: Option<usize>,
    depth: usize,
}

// --- Parsing ---

fn parse_mindmap_tree(input: &str) -> Result<MindmapNode, MermaidError> {
    let lines: Vec<&str> = input.lines().collect();
    let mut i = 0_usize;

    // Find "mindmap" declaration
    while i < lines.len() {
        let line = lines[i].trim();
        if line.is_empty() || line.starts_with("%%") {
            i += 1;
            continue;
        }
        if line.split_whitespace().next() == Some("mindmap") {
            i += 1;
            break;
        }
        return Err(MermaidError::ParseError {
            line: i + 1,
            message: "Expected 'mindmap' declaration".to_string(),
        });
    }

    let mut stack: Vec<(usize, MindmapNode)> = Vec::new();
    let mut next_id = 0_usize;

    while i < lines.len() {
        let raw = lines[i];
        i += 1;

        if raw.trim().is_empty() || raw.trim_start().starts_with("%%") {
            continue;
        }

        let indent = raw.chars().take_while(|c| *c == ' ' || *c == '\t').count();
        let text = raw.trim();

        // Skip decoration lines like ::icon(...)
        if text.starts_with("::") {
            continue;
        }

        let (label, node_type) = extract_label_and_type(text);
        let node_id = format!("n{next_id}");
        next_id += 1;

        let is_root = stack.is_empty();

        // Pop stack entries with indent >= current
        while let Some((d, _)) = stack.last() {
            if *d >= indent {
                let (_, child) = stack.pop().unwrap();
                if let Some((_, parent)) = stack.last_mut() {
                    parent.children.push(child);
                } else {
                    // This was the root — re-push it
                    stack.push((indent, child));
                    break;
                }
            } else {
                break;
            }
        }

        let node = MindmapNode {
            id: node_id,
            label,
            node_type: if is_root && node_type == MindmapNodeType::Default {
                MindmapNodeType::Circle
            } else {
                node_type
            },
            children: Vec::new(),
            section: None,
        };

        stack.push((indent, node));
    }

    // Collapse stack to get root
    while stack.len() > 1 {
        let (_, child) = stack.pop().unwrap();
        if let Some((_, parent)) = stack.last_mut() {
            parent.children.push(child);
        }
    }

    stack.pop().map(|(_, n)| n).ok_or(MermaidError::ParseError {
        line: 0,
        message: "No nodes found in mindmap".to_string(),
    })
}

fn extract_label_and_type(text: &str) -> (String, MindmapNodeType) {
    let t = text.trim();

    // (( ... )) → Circle
    if let Some(start) = t.find("((") {
        if t.ends_with("))") && start + 2 < t.len().saturating_sub(2) {
            let label = t[start + 2..t.len() - 2].trim().to_string();
            return (label, MindmapNodeType::Circle);
        }
    }

    // {{ ... }} → Hexagon
    if let Some(start) = t.find("{{") {
        if t.ends_with("}}") && start + 2 < t.len().saturating_sub(2) {
            let label = t[start + 2..t.len() - 2].trim().to_string();
            return (label, MindmapNodeType::Hexagon);
        }
    }

    // )) ... (( → Bang
    if let Some(start) = t.find("))") {
        if t.ends_with("((") && start + 2 < t.len().saturating_sub(2) {
            let label = t[start + 2..t.len() - 2].trim().to_string();
            return (label, MindmapNodeType::Bang);
        }
    }

    // [ ... ] → Rect
    if let Some(start) = t.find('[') {
        if t.ends_with(']') && start + 1 < t.len().saturating_sub(1) {
            let label = t[start + 1..t.len() - 1].trim().to_string();
            return (label, MindmapNodeType::Rect);
        }
    }

    // ( ... ) → RoundedRect
    if let Some(start) = t.find('(') {
        if t.ends_with(')') && start + 1 < t.len().saturating_sub(1) {
            let label = t[start + 1..t.len() - 1].trim().to_string();
            return (label, MindmapNodeType::RoundedRect);
        }
    }

    // Default (no delimiter)
    (t.to_string(), MindmapNodeType::Default)
}

// --- Section assignment ---

fn assign_sections(node: &mut MindmapNode, section: Option<usize>) {
    node.section = section;
    for (i, child) in node.children.iter_mut().enumerate() {
        let child_section = if section.is_none() {
            // Direct children of root get their own section number
            Some(i)
        } else {
            section
        };
        assign_sections(child, child_section);
    }
}

// --- Node sizing ---

const FONT_SIZE: f64 = 16.0;
const NODE_PADDING: f64 = 15.0;
const ROOT_PADDING: f64 = 20.0;

/// Estimate text width using display-width units × average character width.
/// We use this instead of `text_wrap::line_width` because in sandbox/CI
/// environments the font measurer may return 0 (no real fonts loaded).
fn estimate_text_width(text: &str) -> f64 {
    // Average character width for 16px Trebuchet MS is roughly 8.5px
    let avg_char_width = 8.5;
    crate::text_wrap::display_width_units(text) * avg_char_width
}

fn measure_node(node: &MindmapNode) -> (f64, f64) {
    let text_width = estimate_text_width(&node.label);
    let text_height = FONT_SIZE;

    match node.node_type {
        MindmapNodeType::Circle => {
            let diameter = (text_width.max(text_height) + ROOT_PADDING * 2.0).max(60.0);
            (diameter, diameter)
        }
        MindmapNodeType::Rect | MindmapNodeType::RoundedRect | MindmapNodeType::Default => {
            let w = text_width + NODE_PADDING * 2.0;
            let h = text_height + NODE_PADDING * 2.0;
            (w.max(40.0), h.max(36.0))
        }
        MindmapNodeType::Hexagon => {
            let w = text_width + NODE_PADDING * 3.0;
            let h = text_height + NODE_PADDING * 2.0;
            (w.max(50.0), h.max(40.0))
        }
        MindmapNodeType::Cloud | MindmapNodeType::Bang => {
            let w = text_width + NODE_PADDING * 2.5;
            let h = text_height + NODE_PADDING * 2.5;
            (w.max(50.0), h.max(40.0))
        }
    }
}

// --- Layout ---

/// Simple radial mindmap layout.
/// Root is placed at center. Children of root are distributed radially.
/// Deeper nodes extend outward from their parent.
fn layout_mindmap(root: &MindmapNode) -> (Vec<PlacedNode>, Vec<PlacedEdge>) {
    let mut placed_nodes = Vec::new();
    let mut placed_edges = Vec::new();

    let (root_w, root_h) = measure_node(root);

    // Place root at origin (will be shifted later)
    placed_nodes.push(PlacedNode {
        id: root.id.clone(),
        label: root.label.clone(),
        node_type: root.node_type,
        x: 0.0,
        y: 0.0,
        width: root_w,
        height: root_h,
        section: root.section,
        is_root: true,
    });

    let n_children = root.children.len();
    if n_children == 0 {
        return (placed_nodes, placed_edges);
    }

    // Calculate total subtree "weight" for each branch
    let weights: Vec<f64> = root.children.iter().map(subtree_weight).collect();
    let total_weight: f64 = weights.iter().sum();

    // Distribute branches around the root
    let start_angle: f64 = -std::f64::consts::FRAC_PI_2; // top
    let mut current_angle = start_angle;

    let base_radius = 120.0 + (n_children as f64) * 20.0;

    for (i, child) in root.children.iter().enumerate() {
        let weight_fraction = weights[i] / total_weight;
        let sweep = std::f64::consts::TAU * weight_fraction;
        let mid_angle = current_angle + sweep / 2.0;

        layout_subtree(
            child,
            0.0,
            0.0,
            mid_angle,
            base_radius,
            1,
            &mut placed_nodes,
            &mut placed_edges,
        );

        current_angle += sweep;
    }

    // Normalize positions
    let padding = 20.0;
    let min_x = placed_nodes
        .iter()
        .map(|n| n.x - n.width / 2.0)
        .fold(f64::INFINITY, f64::min);
    let min_y = placed_nodes
        .iter()
        .map(|n| n.y - n.height / 2.0)
        .fold(f64::INFINITY, f64::min);

    let shift_x = -min_x + padding;
    let shift_y = -min_y + padding;

    for node in &mut placed_nodes {
        node.x += shift_x;
        node.y += shift_y;
    }

    for edge in &mut placed_edges {
        edge.from_x += shift_x;
        edge.from_y += shift_y;
        edge.to_x += shift_x;
        edge.to_y += shift_y;
    }

    (placed_nodes, placed_edges)
}

fn subtree_weight(node: &MindmapNode) -> f64 {
    if node.children.is_empty() {
        return 1.0;
    }
    let child_weight: f64 = node.children.iter().map(subtree_weight).sum();
    child_weight.max(1.0)
}

#[allow(clippy::too_many_arguments)]
fn layout_subtree(
    node: &MindmapNode,
    parent_x: f64,
    parent_y: f64,
    angle: f64,
    radius: f64,
    depth: usize,
    placed_nodes: &mut Vec<PlacedNode>,
    placed_edges: &mut Vec<PlacedEdge>,
) {
    let (node_w, node_h) = measure_node(node);

    let x = parent_x + angle.cos() * radius;
    let y = parent_y + angle.sin() * radius;

    placed_nodes.push(PlacedNode {
        id: node.id.clone(),
        label: node.label.clone(),
        node_type: node.node_type,
        x,
        y,
        width: node_w,
        height: node_h,
        section: node.section,
        is_root: false,
    });

    placed_edges.push(PlacedEdge {
        from_x: parent_x,
        from_y: parent_y,
        to_x: x,
        to_y: y,
        section: node.section,
        depth,
    });

    let n_children = node.children.len();
    if n_children == 0 {
        return;
    }

    let weights: Vec<f64> = node.children.iter().map(subtree_weight).collect();
    let total_weight: f64 = weights.iter().sum();

    // Fan out children around the parent→child direction
    let fan_spread = std::f64::consts::FRAC_PI_2.min(0.8 * (n_children as f64).sqrt());
    let child_radius = 100.0 + (depth as f64) * 10.0;

    let mut current_angle = angle - fan_spread / 2.0;

    for (i, child) in node.children.iter().enumerate() {
        let weight_fraction = weights[i] / total_weight;
        let sweep = fan_spread * weight_fraction;
        let mid_angle = current_angle + sweep / 2.0;

        layout_subtree(
            child,
            x,
            y,
            mid_angle,
            child_radius,
            depth + 1,
            placed_nodes,
            placed_edges,
        );

        current_angle += sweep;
    }
}

// --- SVG Rendering ---

pub fn render_mindmap_to_svg(input: &str, _theme: &MermaidTheme) -> Result<String, MermaidError> {
    let mut root = parse_mindmap_tree(input)?;
    assign_sections(&mut root, None);

    let (placed_nodes, placed_edges) = layout_mindmap(&root);

    let max_x = placed_nodes
        .iter()
        .map(|n| n.x + n.width / 2.0)
        .fold(0.0_f64, f64::max);
    let max_y = placed_nodes
        .iter()
        .map(|n| n.y + n.height / 2.0)
        .fold(0.0_f64, f64::max);

    let padding = 20.0;
    let svg_width = max_x + padding;
    let svg_height = max_y + padding;

    let mut svg = String::new();

    svg.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" \
         width=\"{}\" height=\"{}\" \
         viewBox=\"0 0 {} {}\" \
         aria-roledescription=\"mindmap\" \
         role=\"graphics-document document\" \
         style=\"max-width: 100%;\">",
        svg_width, svg_height, svg_width, svg_height,
    ));

    // Render edges first (behind nodes)
    for edge in &placed_edges {
        render_edge(&mut svg, edge);
    }

    // Render nodes
    for node in &placed_nodes {
        render_node(&mut svg, node);
    }

    svg.push_str("</svg>");

    Ok(svg)
}

fn render_edge(svg: &mut String, edge: &PlacedEdge) {
    let color = match edge.section {
        Some(s) => section_color(s).edge,
        None => "#333333",
    };

    // Stroke width based on depth: 17 - 3*depth, minimum 2
    let stroke_width = (17.0 - 3.0 * edge.depth as f64).max(2.0);

    // Curved edge using quadratic bezier
    let mx = (edge.from_x + edge.to_x) / 2.0;
    let my = (edge.from_y + edge.to_y) / 2.0;

    svg.push_str(&format!(
        "<path d=\"M {:.1},{:.1} Q {:.1},{:.1} {:.1},{:.1}\" \
         fill=\"none\" stroke=\"{}\" stroke-width=\"{:.1}\" \
         stroke-linecap=\"round\" />",
        edge.from_x, edge.from_y, mx, my, edge.to_x, edge.to_y, color, stroke_width,
    ));
}

fn render_node(svg: &mut String, node: &PlacedNode) {
    let (fill, text_color) = if node.is_root {
        (ROOT_FILL.to_string(), ROOT_TEXT.to_string())
    } else {
        match node.section {
            Some(s) => {
                let sc = section_color(s);
                (sc.fill.to_string(), sc.text.to_string())
            }
            None => ("#ECECFF".to_string(), "#333333".to_string()),
        }
    };

    let cx = node.x;
    let cy = node.y;

    match node.node_type {
        MindmapNodeType::Circle => {
            let r = node.width / 2.0;
            svg.push_str(&format!(
                "<circle cx=\"{:.1}\" cy=\"{:.1}\" r=\"{:.1}\" \
                 fill=\"{}\" stroke=\"none\" />",
                cx, cy, r, fill,
            ));
        }
        MindmapNodeType::Rect => {
            let x = cx - node.width / 2.0;
            let y = cy - node.height / 2.0;
            svg.push_str(&format!(
                "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" \
                 rx=\"0\" ry=\"0\" fill=\"{}\" stroke=\"none\" />",
                x, y, node.width, node.height, fill,
            ));
        }
        MindmapNodeType::RoundedRect | MindmapNodeType::Default => {
            let x = cx - node.width / 2.0;
            let y = cy - node.height / 2.0;
            // Corner radius = 5 matching Mermaid reference SVG path data
            svg.push_str(&format!(
                "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" \
                 rx=\"5\" ry=\"5\" fill=\"{}\" stroke=\"none\" />",
                x, y, node.width, node.height, fill,
            ));
        }
        MindmapNodeType::Hexagon => {
            let x = cx - node.width / 2.0;
            let y = cy - node.height / 2.0;
            let inset = node.height / 4.0;
            let points = format!(
                "{:.1},{:.1} {:.1},{:.1} {:.1},{:.1} {:.1},{:.1} {:.1},{:.1} {:.1},{:.1}",
                x + inset,
                y,
                x + node.width - inset,
                y,
                x + node.width,
                cy,
                x + node.width - inset,
                y + node.height,
                x + inset,
                y + node.height,
                x,
                cy,
            );
            svg.push_str(&format!(
                "<polygon points=\"{}\" fill=\"{}\" stroke=\"none\" />",
                points, fill,
            ));
        }
        MindmapNodeType::Cloud | MindmapNodeType::Bang => {
            let rx = node.width / 2.0;
            let ry = node.height / 2.0;
            svg.push_str(&format!(
                "<ellipse cx=\"{:.1}\" cy=\"{:.1}\" rx=\"{:.1}\" ry=\"{:.1}\" \
                 fill=\"{}\" stroke=\"none\" />",
                cx, cy, rx, ry, fill,
            ));
        }
    }

    // Render underline decoration (non-root nodes get a colored line below)
    if !node.is_root {
        match node.node_type {
            MindmapNodeType::Circle => {}
            _ => {
                let x1 = cx - node.width / 2.0;
                let x2 = cx + node.width / 2.0;
                let line_y = cy + node.height / 2.0 + 5.0;
                // Underline uses the complementary/inverted hue color from CSS
                // (section-N line stroke in reference is the hue+180 version)
                svg.push_str(&format!(
                    "<line x1=\"{:.1}\" y1=\"{:.1}\" x2=\"{:.1}\" y2=\"{:.1}\" \
                     stroke=\"{}\" stroke-width=\"3\" />",
                    x1, line_y, x2, line_y, fill,
                ));
            }
        }
    }

    // Render text
    svg.push_str(&format!(
        "<text x=\"{:.1}\" y=\"{:.1}\" \
         text-anchor=\"middle\" dominant-baseline=\"central\" \
         font-family=\"'trebuchet ms', verdana, arial, sans-serif\" \
         font-size=\"{}\" fill=\"{}\">{}</text>",
        cx,
        cy,
        FONT_SIZE,
        text_color,
        html_escape(&node.label),
    ));
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
