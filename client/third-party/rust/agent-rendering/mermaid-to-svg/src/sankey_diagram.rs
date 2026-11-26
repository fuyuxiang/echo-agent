use crate::error::MermaidError;
use crate::theme::MermaidTheme;

use std::collections::{BTreeMap, BTreeSet, HashMap};

const WIDTH: f64 = 600.0;
const HEIGHT: f64 = 400.0;
const NODE_WIDTH: f64 = 10.0;
const NODE_PADDING: f64 = 25.0;
const LABEL_OFFSET: f64 = 6.0;

const NODE_COLORS: [&str; 10] = [
    "#4e79a7", "#f28e2c", "#e15759", "#76b7b2", "#59a14f", "#edc948", "#b07aa1", "#9c755f",
    "#bab0ab", "#ff9da7",
];

pub fn render_sankey_diagram_to_svg(
    mermaid_source: &str,
    theme: &MermaidTheme,
) -> Result<String, MermaidError> {
    let diagram = parse_sankey(mermaid_source)?;

    let layout = compute_layout(&diagram);

    let mut svg = String::new();
    svg.push_str(&format!(
        "<svg aria-roledescription=\"sankey\" role=\"graphics-document document\" viewBox=\"0 0 {WIDTH} {HEIGHT}\" style=\"max-width: {WIDTH}px; background-color: {};\" xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" width=\"100%\" id=\"my-svg\">",
        theme.background
    ));

    svg.push_str("<g/>");

    svg.push_str(&format!(
        "<rect x=\"0\" y=\"0\" width=\"{WIDTH}\" height=\"{HEIGHT}\" fill=\"{}\"/>",
        theme.background
    ));

    svg.push_str("<defs>");
    for (idx, link) in layout.links.iter().enumerate() {
        let grad_id = format!("linearGradient-{idx}");
        svg.push_str(&format!(
            "<linearGradient id=\"{grad_id}\" gradientUnits=\"userSpaceOnUse\" x1=\"{x1}\" x2=\"{x2}\">",
            x1 = link.x0,
            x2 = link.x1,
        ));
        svg.push_str(&format!(
            "<stop offset=\"0%\" stop-color=\"{}\"/>",
            escape_xml(&link.source_color)
        ));
        svg.push_str(&format!(
            "<stop offset=\"100%\" stop-color=\"{}\"/>",
            escape_xml(&link.target_color)
        ));
        svg.push_str("</linearGradient>");
    }
    svg.push_str("</defs>");

    svg.push_str("<g class=\"nodes\">");
    for node in &layout.nodes {
        svg.push_str(&format!(
            "<g class=\"node\" id=\"{}\" transform=\"translate({},{})\" x=\"{}\" y=\"{}\">",
            escape_xml(&node.dom_id),
            node.x,
            node.y,
            node.x,
            node.y
        ));
        svg.push_str(&format!(
            "<rect height=\"{}\" width=\"{}\" fill=\"{}\"/>",
            node.height,
            NODE_WIDTH,
            escape_xml(&node.color)
        ));
        svg.push_str("</g>");
    }
    svg.push_str("</g>");

    svg.push_str("<g class=\"node-labels\" font-size=\"14\">");
    for node in &layout.nodes {
        let center_y = node.y + node.height / 2.0;
        if node.depth == layout.max_depth {
            let x = node.x - LABEL_OFFSET;
            svg.push_str(&format!(
                "<text x=\"{x}\" y=\"{center_y}\" dy=\"0em\" text-anchor=\"end\">{}</text>",
                escape_xml(&node.display_label)
            ));
        } else {
            let x = node.x + NODE_WIDTH + LABEL_OFFSET;
            svg.push_str(&format!(
                "<text x=\"{x}\" y=\"{center_y}\" dy=\"0em\" text-anchor=\"start\">{}</text>",
                escape_xml(&node.display_label)
            ));
        }
    }
    svg.push_str("</g>");

    svg.push_str("<g class=\"links\" fill=\"none\" stroke-opacity=\"0.5\">");
    for (idx, link) in layout.links.iter().enumerate() {
        let grad_id = format!("linearGradient-{idx}");
        let mx = (link.x0 + link.x1) / 2.0;
        let d = format!(
            "M{sx},{sy}C{mx},{sy},{mx},{ty},{tx},{ty}",
            sx = link.x0,
            sy = link.y0,
            mx = mx,
            tx = link.x1,
            ty = link.y1,
        );
        svg.push_str("<g class=\"link\" style=\"mix-blend-mode: multiply;\">");
        svg.push_str(&format!(
            "<path d=\"{d}\" stroke=\"url(#{grad_id})\" stroke-width=\"{}\"/>",
            link.thickness
        ));
        svg.push_str("</g>");
    }
    svg.push_str("</g>");

    svg.push_str("</svg>");
    Ok(svg)
}

#[derive(Debug, Clone)]
struct SankeyDiagram {
    links: Vec<SankeyLink>,
    node_order: Vec<String>,
}

#[derive(Debug, Clone)]
struct SankeyLink {
    source: String,
    target: String,
    value: f64,
}

#[derive(Debug, Clone)]
struct SankeyNodeLayout {
    name: String,
    display_label: String,
    dom_id: String,
    depth: usize,
    x: f64,
    y: f64,
    height: f64,
    color: String,
}

#[derive(Debug, Clone)]
struct SankeyLinkLayout {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    thickness: f64,
    source_color: String,
    target_color: String,
}

#[derive(Debug, Clone)]
struct SankeyLayout {
    nodes: Vec<SankeyNodeLayout>,
    links: Vec<SankeyLinkLayout>,
    max_depth: usize,
}

fn parse_sankey(input: &str) -> Result<SankeyDiagram, MermaidError> {
    let mut found_header = false;
    let mut links: Vec<SankeyLink> = Vec::new();
    let mut node_seen: BTreeSet<String> = BTreeSet::new();
    let mut node_order: Vec<String> = Vec::new();

    for (idx, raw) in input.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with("%%") {
            continue;
        }

        if !found_header {
            if line.split_whitespace().next() != Some("sankey-beta") {
                return Err(MermaidError::ParseError {
                    line: line_no,
                    message: "Expected 'sankey-beta' declaration".to_string(),
                });
            }
            found_header = true;
            continue;
        }

        let parts: Vec<&str> = line.split(',').map(|p| p.trim()).collect();
        if parts.len() != 3 {
            return Err(MermaidError::ParseError {
                line: line_no,
                message: format!("Invalid sankey link: {line}"),
            });
        }

        let source = parts[0].to_string();
        let target = parts[1].to_string();
        let value: f64 = parts[2].parse().map_err(|_| MermaidError::ParseError {
            line: line_no,
            message: format!("Invalid sankey value: {}", parts[2]),
        })?;

        if node_seen.insert(source.clone()) {
            node_order.push(source.clone());
        }
        if node_seen.insert(target.clone()) {
            node_order.push(target.clone());
        }

        links.push(SankeyLink {
            source,
            target,
            value,
        });
    }

    if !found_header {
        return Err(MermaidError::ParseError {
            line: 1,
            message: "Expected 'sankey-beta' declaration".to_string(),
        });
    }

    if links.is_empty() {
        return Err(MermaidError::ParseError {
            line: 1,
            message: "sankey diagram requires at least one link".to_string(),
        });
    }

    Ok(SankeyDiagram { links, node_order })
}

fn compute_layout(diagram: &SankeyDiagram) -> SankeyLayout {
    let mut in_sum: HashMap<&str, f64> = HashMap::new();
    let mut out_sum: HashMap<&str, f64> = HashMap::new();

    for link in &diagram.links {
        *out_sum.entry(link.source.as_str()).or_insert(0.0) += link.value;
        *in_sum.entry(link.target.as_str()).or_insert(0.0) += link.value;
    }

    let mut value_by_node: HashMap<&str, f64> = HashMap::new();
    for node in &diagram.node_order {
        let v_in = *in_sum.get(node.as_str()).unwrap_or(&0.0);
        let v_out = *out_sum.get(node.as_str()).unwrap_or(&0.0);
        value_by_node.insert(node.as_str(), v_in.max(v_out));
    }

    let mut preds: HashMap<&str, Vec<&str>> = HashMap::new();
    for link in &diagram.links {
        preds
            .entry(link.target.as_str())
            .or_default()
            .push(link.source.as_str());
        preds.entry(link.source.as_str()).or_default();
    }

    let mut depth: HashMap<&str, usize> = HashMap::new();
    for node in &diagram.node_order {
        depth.insert(node.as_str(), 0);
    }

    let mut changed = true;
    for _ in 0..diagram.node_order.len().saturating_mul(2) {
        if !changed {
            break;
        }
        changed = false;
        for node in &diagram.node_order {
            let node = node.as_str();
            let p = preds.get(node).map(|v| v.as_slice()).unwrap_or(&[]);
            let mut d = 0_usize;
            for &pred in p {
                d = d.max(depth.get(pred).copied().unwrap_or(0).saturating_add(1));
            }
            if depth.get(node).copied().unwrap_or(0) != d {
                depth.insert(node, d);
                changed = true;
            }
        }
    }

    let max_depth = depth.values().copied().max().unwrap_or(0);
    let layers = max_depth.max(1) + 1;

    let mut nodes_by_depth: BTreeMap<usize, Vec<&str>> = BTreeMap::new();
    for node in &diagram.node_order {
        let d = depth.get(node.as_str()).copied().unwrap_or(0);
        nodes_by_depth.entry(d).or_default().push(node.as_str());
    }

    let mut ky = f64::INFINITY;
    for nodes in nodes_by_depth.values() {
        let sum: f64 = nodes
            .iter()
            .map(|n| value_by_node.get(n).copied().unwrap_or(0.0))
            .sum();
        if sum <= 0.0 {
            continue;
        }
        let n = nodes.len() as f64;
        let available = HEIGHT - (n - 1.0).max(0.0) * NODE_PADDING;
        ky = ky.min(available / sum);
    }
    if !ky.is_finite() {
        ky = 1.0;
    }

    let mut node_layout: HashMap<&str, SankeyNodeLayout> = HashMap::new();

    for (d, nodes) in &nodes_by_depth {
        let sum: f64 = nodes
            .iter()
            .map(|n| value_by_node.get(n).copied().unwrap_or(0.0))
            .sum();
        let used = sum * ky + (nodes.len().saturating_sub(1) as f64) * NODE_PADDING;
        let mut y = (HEIGHT - used) / 2.0;

        let x = if layers <= 1 {
            0.0
        } else {
            (WIDTH - NODE_WIDTH) * (*d as f64) / ((layers - 1) as f64)
        };

        for &name in nodes {
            let v = value_by_node.get(name).copied().unwrap_or(0.0);
            let h = v * ky;

            let global_idx = diagram
                .node_order
                .iter()
                .position(|n| n == name)
                .unwrap_or(0);
            let dom_id = format!("node-{}", global_idx + 1);
            let color = NODE_COLORS
                .get(global_idx)
                .copied()
                .unwrap_or(NODE_COLORS[0])
                .to_string();

            node_layout.insert(
                name,
                SankeyNodeLayout {
                    name: name.to_string(),
                    display_label: format_sankey_node_label(name, v),
                    dom_id,
                    depth: *d,
                    x,
                    y,
                    height: h,
                    color,
                },
            );
            y += h + NODE_PADDING;
        }
    }

    let mut out_offset: HashMap<&str, f64> = HashMap::new();
    let mut in_offset: HashMap<&str, f64> = HashMap::new();

    for node in &diagram.node_order {
        out_offset.insert(node.as_str(), 0.0);
        in_offset.insert(node.as_str(), 0.0);
    }

    let mut link_layouts = Vec::new();
    for link in &diagram.links {
        let Some(source_node) = node_layout.get(link.source.as_str()) else {
            continue;
        };
        let Some(target_node) = node_layout.get(link.target.as_str()) else {
            continue;
        };

        let thickness = link.value * ky;

        let so = *out_offset.get(link.source.as_str()).unwrap_or(&0.0);
        let ti = *in_offset.get(link.target.as_str()).unwrap_or(&0.0);

        let y0 = source_node.y + so + thickness / 2.0;
        let y1 = target_node.y + ti + thickness / 2.0;

        out_offset.insert(link.source.as_str(), so + thickness);
        in_offset.insert(link.target.as_str(), ti + thickness);

        let x0 = source_node.x + NODE_WIDTH;
        let x1 = target_node.x;

        link_layouts.push(SankeyLinkLayout {
            x0,
            y0,
            x1,
            y1,
            thickness,
            source_color: source_node.color.clone(),
            target_color: target_node.color.clone(),
        });
    }

    let mut nodes_vec: Vec<SankeyNodeLayout> = diagram
        .node_order
        .iter()
        .filter_map(|n| node_layout.get(n.as_str()).cloned())
        .collect();

    nodes_vec.sort_by(|a, b| {
        a.depth
            .cmp(&b.depth)
            .then_with(|| a.y.total_cmp(&b.y))
            .then_with(|| a.name.cmp(&b.name))
    });

    SankeyLayout {
        nodes: nodes_vec,
        links: link_layouts,
        max_depth,
    }
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn format_sankey_node_label(name: &str, value: f64) -> String {
    if value.fract().abs() < f64::EPSILON {
        format!("{name} {}", value as i64)
    } else {
        format!("{name} {value}")
    }
}
