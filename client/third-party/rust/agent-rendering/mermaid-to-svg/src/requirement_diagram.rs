use std::collections::HashMap;

use crate::error::MermaidError;
use crate::text_wrap::{line_width, DEFAULT_CHAR_WIDTH};
use crate::theme::MermaidTheme;
use dagre_rust::layout::layout as dagre_layout;
use dagre_rust::{GraphConfig, GraphEdge, GraphNode};
use graphlib_rust::Graph;

const BOX_PADDING: f64 = 20.0;
const BOX_GAP: f64 = 20.0;
const LINE_HEIGHT: f64 = 24.0;

const NODE_SEP: f64 = 50.0;
const RANK_SEP: f64 = 50.0;
const GRAPH_MARGIN: f64 = 8.0;

pub fn render_requirement_diagram_to_svg(
    mermaid_source: &str,
    theme: &MermaidTheme,
) -> Result<String, MermaidError> {
    let diagram = parse_requirement_diagram(mermaid_source)?;
    let layout = compute_layout(&diagram);
    Ok(render_svg(&layout, theme))
}

#[derive(Debug, Clone, Copy)]
enum Direction {
    Tb,
    Bt,
    Lr,
    Rl,
}

impl Direction {
    fn as_rankdir(self) -> &'static str {
        match self {
            Direction::Tb => "tb",
            Direction::Bt => "bt",
            Direction::Lr => "lr",
            Direction::Rl => "rl",
        }
    }
}

#[derive(Debug, Clone)]
struct RequirementDiagram {
    direction: Direction,
    nodes: HashMap<String, ReqNode>,
    relations: Vec<Relation>,
}

#[derive(Debug, Clone)]
enum ReqNode {
    Requirement(RequirementNode),
    Element(ElementNode),
}

#[derive(Debug, Clone)]
struct RequirementNode {
    name: String,
    requirement_id: String,
    text: String,
    risk: String,
    verify_method: String,
    req_type: String,
}

#[derive(Debug, Clone)]
struct ElementNode {
    name: String,
    element_type: String,
    doc_ref: String,
}

#[derive(Debug, Clone)]
struct Relation {
    src: String,
    dst: String,
    rel_type: String,
}

#[derive(Debug, Clone)]
struct NodeLayout {
    id: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    labels: Vec<LabelLayout>,
    divider_y: Option<f64>,
}

#[derive(Debug, Clone)]
struct EdgeLayout {
    id: String,
    rel_type: String,
    label: String,
    points: Vec<(f64, f64)>,
    label_pos: Option<(f64, f64)>,
    label_width: f64,
    label_height: f64,
}

#[derive(Debug, Clone)]
struct DiagramLayout {
    nodes: HashMap<String, NodeLayout>,
    edges: Vec<EdgeLayout>,
    width: f64,
    height: f64,
}

#[derive(Debug, Clone)]
struct LabelLayout {
    text: String,
    x: f64,
    y: f64,
    anchor: &'static str,
    bold: bool,
}

fn parse_requirement_diagram(input: &str) -> Result<RequirementDiagram, MermaidError> {
    let mut found_header = false;
    let mut direction = Direction::Tb;

    let mut nodes: HashMap<String, ReqNode> = HashMap::new();
    let mut relations: Vec<Relation> = Vec::new();

    let lines: Vec<&str> = input.lines().collect();
    let mut i = 0_usize;

    while i < lines.len() {
        let raw = lines[i];
        let line_no = i + 1;
        let line = raw.trim();
        i += 1;

        if line.is_empty() || line.starts_with("%%") {
            continue;
        }

        if !found_header {
            if first_diagram_type_token(line) != Some("requirementDiagram") {
                return Err(MermaidError::ParseError {
                    line: line_no,
                    message: "Expected 'requirementDiagram' declaration".to_string(),
                });
            }
            found_header = true;
            continue;
        }

        if let Some(rest) = line.strip_prefix("direction ") {
            direction = parse_direction(rest.trim(), line_no)?;
            continue;
        }

        if let Some((node, new_i)) = try_parse_requirement_or_element(&lines, i - 1)? {
            let id = match &node {
                ReqNode::Requirement(r) => r.name.clone(),
                ReqNode::Element(e) => e.name.clone(),
            };
            nodes.insert(id, node);
            i = new_i;
            continue;
        }

        if let Some(rel) = try_parse_relation(line, line_no)? {
            relations.push(rel);
            continue;
        }

        return Err(MermaidError::ParseError {
            line: line_no,
            message: format!("Unrecognized requirementDiagram line: {line}"),
        });
    }

    if !found_header {
        return Err(MermaidError::ParseError {
            line: 1,
            message: "Expected 'requirementDiagram' declaration".to_string(),
        });
    }

    Ok(RequirementDiagram {
        direction,
        nodes,
        relations,
    })
}

fn first_diagram_type_token(line: &str) -> Option<&str> {
    line.split_whitespace().next()
}

fn parse_direction(s: &str, line: usize) -> Result<Direction, MermaidError> {
    match s.to_uppercase().as_str() {
        "TB" | "TD" => Ok(Direction::Tb),
        "BT" => Ok(Direction::Bt),
        "LR" => Ok(Direction::Lr),
        "RL" => Ok(Direction::Rl),
        _ => Err(MermaidError::ParseError {
            line,
            message: format!("Invalid direction: {s}"),
        }),
    }
}

fn try_parse_requirement_or_element(
    lines: &[&str],
    start_idx: usize,
) -> Result<Option<(ReqNode, usize)>, MermaidError> {
    let line = lines[start_idx].trim();
    if line.is_empty() || line.starts_with("%%") {
        return Ok(None);
    }

    let (kind, name, has_open_brace) = if let Some((kw, rest)) = split_once_ws(line) {
        let kind = kw;
        let (name_raw, tail) = split_once_ws(rest).unwrap_or((rest, ""));
        let name = name_raw.trim();
        let has_open_brace = tail.contains('{') || name.ends_with('{');
        (kind, name.trim_end_matches('{').trim(), has_open_brace)
    } else {
        return Ok(None);
    };

    let kind_lower = kind.to_lowercase();
    if kind_lower != "element"
        && kind_lower != "requirement"
        && kind_lower != "functionalrequirement"
        && kind_lower != "interfacerequirement"
        && kind_lower != "performancerequirement"
        && kind_lower != "physicalrequirement"
        && kind_lower != "designconstraint"
    {
        return Ok(None);
    }

    if name.is_empty() {
        return Err(MermaidError::ParseError {
            line: start_idx + 1,
            message: format!("Expected name after '{kind}'"),
        });
    }

    let mut i = start_idx + 1;
    if !has_open_brace {
        while i < lines.len() {
            let l = lines[i].trim();
            if l.is_empty() || l.starts_with("%%") {
                i += 1;
                continue;
            }
            if l.starts_with('{') {
                i += 1;
                break;
            }
            return Ok(None);
        }
    }

    let mut props: HashMap<String, String> = HashMap::new();
    while i < lines.len() {
        let raw = lines[i];
        let line_no = i + 1;
        let l = raw.trim();
        i += 1;

        if l.is_empty() || l.starts_with("%%") {
            continue;
        }
        if l.starts_with('}') {
            break;
        }

        let Some((k, v)) = l.split_once(':') else {
            return Err(MermaidError::ParseError {
                line: line_no,
                message: format!("Invalid property line: {l}"),
            });
        };
        let key = k.trim().to_string();
        let mut value = v.trim().trim_end_matches(',').trim().to_string();
        value = strip_quotes(&value);
        props.insert(key, value);
    }

    if kind_lower == "element" {
        let node = ReqNode::Element(ElementNode {
            name: name.to_string(),
            element_type: props.get("type").cloned().unwrap_or_else(String::new),
            doc_ref: props
                .get("docref")
                .or_else(|| props.get("docRef"))
                .cloned()
                .unwrap_or_else(String::new),
        });
        return Ok(Some((node, i)));
    }

    let req_type = match kind_lower.as_str() {
        "functionalrequirement" => "Functional Requirement",
        "interfacerequirement" => "Interface Requirement",
        "performancerequirement" => "Performance Requirement",
        "physicalrequirement" => "Physical Requirement",
        "designconstraint" => "Design Constraint",
        _ => "Requirement",
    }
    .to_string();

    let risk = props.get("risk").cloned().unwrap_or_else(String::new);
    let verify_method = props
        .get("verifyMethod")
        .or_else(|| props.get("verifymethod"))
        .cloned()
        .unwrap_or_else(String::new);

    let node = ReqNode::Requirement(RequirementNode {
        name: name.to_string(),
        requirement_id: props.get("id").cloned().unwrap_or_else(String::new),
        text: props.get("text").cloned().unwrap_or_else(String::new),
        risk: normalize_risk(&risk),
        verify_method: normalize_verify_method(&verify_method),
        req_type,
    });

    Ok(Some((node, i)))
}

fn normalize_risk(s: &str) -> String {
    match s.trim().to_lowercase().as_str() {
        "low" => "Low".to_string(),
        "medium" => "Medium".to_string(),
        "high" => "High".to_string(),
        _ => s.trim().to_string(),
    }
}

fn normalize_verify_method(s: &str) -> String {
    match s.trim().to_lowercase().as_str() {
        "analysis" => "Analysis".to_string(),
        "demonstration" => "Demonstration".to_string(),
        "inspection" => "Inspection".to_string(),
        "test" => "Test".to_string(),
        _ => s.trim().to_string(),
    }
}

fn strip_quotes(s: &str) -> String {
    let s = s.trim();
    if let Some(inner) = s.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
        return inner.to_string();
    }
    if let Some(inner) = s.strip_prefix('\'').and_then(|t| t.strip_suffix('\'')) {
        return inner.to_string();
    }
    s.to_string()
}

fn split_once_ws(s: &str) -> Option<(&str, &str)> {
    let mut it = s.splitn(2, char::is_whitespace);
    let a = it.next()?;
    let b = it.next().unwrap_or("");
    Some((a, b.trim()))
}

fn try_parse_relation(line: &str, line_no: usize) -> Result<Option<Relation>, MermaidError> {
    let line = line.trim();
    if line.is_empty() || line.starts_with("%%") {
        return Ok(None);
    }

    if let Some((lhs, rhs)) = line.split_once("->") {
        let dst = rhs.trim();
        let tokens: Vec<&str> = lhs.split_whitespace().filter(|t| *t != "-").collect();
        if tokens.len() < 2 || dst.is_empty() {
            return Err(MermaidError::ParseError {
                line: line_no,
                message: format!("Invalid relationship: {line}"),
            });
        }
        let src = tokens[0].trim();
        let rel = tokens[1].trim();
        if src.is_empty() || rel.is_empty() {
            return Err(MermaidError::ParseError {
                line: line_no,
                message: format!("Invalid relationship: {line}"),
            });
        }
        return Ok(Some(Relation {
            src: src.to_string(),
            dst: dst.to_string(),
            rel_type: rel.to_string(),
        }));
    }

    if let Some((lhs, rhs)) = line.split_once("<-") {
        let dst = lhs.trim();
        let tokens: Vec<&str> = rhs.split_whitespace().filter(|t| *t != "-").collect();
        if tokens.len() < 2 || dst.is_empty() {
            return Err(MermaidError::ParseError {
                line: line_no,
                message: format!("Invalid relationship: {line}"),
            });
        }
        let rel = tokens[0].trim();
        let src = tokens[1].trim();
        if src.is_empty() || rel.is_empty() {
            return Err(MermaidError::ParseError {
                line: line_no,
                message: format!("Invalid relationship: {line}"),
            });
        }
        return Ok(Some(Relation {
            src: src.to_string(),
            dst: dst.to_string(),
            rel_type: rel.to_string(),
        }));
    }

    Ok(None)
}

fn compute_layout(diagram: &RequirementDiagram) -> DiagramLayout {
    let mut node_metrics: HashMap<String, (f64, f64, Vec<LabelLayout>, Option<f64>)> =
        HashMap::new();
    for (id, node) in &diagram.nodes {
        let metrics = compute_requirement_box_layout(node);
        node_metrics.insert(id.clone(), metrics);
    }

    type DagreGraph = Graph<GraphConfig, GraphNode, GraphEdge>;

    let mut g: DagreGraph = Graph::new(Some(graphlib_rust::GraphOption {
        directed: Some(true),
        multigraph: Some(true),
        compound: Some(false),
    }));

    g.set_graph(GraphConfig {
        rankdir: Some(diagram.direction.as_rankdir().to_string()),
        nodesep: Some(NODE_SEP as f32),
        ranksep: Some(RANK_SEP as f32),
        edgesep: Some(20.0),
        marginx: Some(GRAPH_MARGIN as f32),
        marginy: Some(GRAPH_MARGIN as f32),
        ..Default::default()
    });

    for (id, (w, h, _, _)) in &node_metrics {
        g.set_node(
            id.clone(),
            Some(GraphNode {
                width: *w as f32,
                height: *h as f32,
                ..Default::default()
            }),
        );
    }

    let mut edge_keys: Vec<(String, String)> = Vec::new();
    for rel in &diagram.relations {
        let edge_label_text = format!("<<{}>>", rel.rel_type);
        let label_width = line_width(&edge_label_text, DEFAULT_CHAR_WIDTH);
        let label_height = LINE_HEIGHT;

        let edge_label = GraphEdge {
            labelpos: Some("c".to_string()),
            width: Some(label_width as f32),
            height: Some(label_height as f32),
            ..Default::default()
        };

        let _ = g.set_edge(&rel.src, &rel.dst, Some(edge_label), None);
        edge_keys.push((rel.src.clone(), rel.dst.clone()));
    }

    dagre_layout(&mut g);

    let mut positions: HashMap<String, (f64, f64)> = HashMap::new();
    for node_id in g.nodes() {
        if let Some(node) = g.node(&node_id) {
            positions.insert(node_id, (node.x as f64, node.y as f64));
        }
    }

    let mut edges: Vec<EdgeLayout> = Vec::new();
    for (idx, (from, to)) in edge_keys.iter().enumerate() {
        let Some(edge) = g.edge(from, to, None) else {
            continue;
        };

        let points: Vec<(f64, f64)> = edge
            .points
            .as_ref()
            .map(|pts| pts.iter().map(|p| (p.x as f64, p.y as f64)).collect())
            .unwrap_or_default();

        let label_pos = if edge.width.unwrap_or(0.0) > 0.0 || edge.height.unwrap_or(0.0) > 0.0 {
            Some((edge.x as f64, edge.y as f64))
        } else {
            None
        };

        let rel_type = diagram
            .relations
            .get(idx)
            .map(|r| r.rel_type.clone())
            .unwrap_or_default();
        let label = format!("<<{}>>", rel_type);
        let label_width = edge.width.unwrap_or(0.0) as f64;
        let label_height = edge.height.unwrap_or(0.0) as f64;

        edges.push(EdgeLayout {
            id: format!("{from}-{to}-{idx}"),
            rel_type,
            label,
            points,
            label_pos,
            label_width,
            label_height,
        });
    }

    let mut layout_nodes: HashMap<String, NodeLayout> = HashMap::new();
    for (id, (x, y)) in &positions {
        let Some((w, h, labels, divider_y)) = node_metrics.get(id).cloned() else {
            continue;
        };
        layout_nodes.insert(
            id.clone(),
            NodeLayout {
                id: id.clone(),
                x: *x,
                y: *y,
                width: w,
                height: h,
                labels,
                divider_y,
            },
        );
    }

    let (min_x, min_y, max_x, max_y) = compute_bounds(&layout_nodes, &edges);
    let dx = GRAPH_MARGIN - min_x;
    let dy = GRAPH_MARGIN - min_y;

    for node in layout_nodes.values_mut() {
        node.x += dx;
        node.y += dy;
    }
    for edge in &mut edges {
        for p in &mut edge.points {
            p.0 += dx;
            p.1 += dy;
        }
        if let Some((x, y)) = edge.label_pos {
            edge.label_pos = Some((x + dx, y + dy));
        }
    }

    let width = (max_x - min_x) + GRAPH_MARGIN * 2.0;
    let height = (max_y - min_y) + GRAPH_MARGIN * 2.0;

    DiagramLayout {
        nodes: layout_nodes,
        edges,
        width,
        height,
    }
}

fn compute_bounds(
    nodes: &HashMap<String, NodeLayout>,
    edges: &[EdgeLayout],
) -> (f64, f64, f64, f64) {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;

    for node in nodes.values() {
        let left = node.x - node.width / 2.0;
        let right = node.x + node.width / 2.0;
        let top = node.y - node.height / 2.0;
        let bottom = node.y + node.height / 2.0;
        min_x = min_x.min(left);
        min_y = min_y.min(top);
        max_x = max_x.max(right);
        max_y = max_y.max(bottom);
    }

    for edge in edges {
        for (x, y) in &edge.points {
            min_x = min_x.min(*x);
            min_y = min_y.min(*y);
            max_x = max_x.max(*x);
            max_y = max_y.max(*y);
        }
        if let Some((x, y)) = edge.label_pos {
            let left = x - edge.label_width / 2.0;
            let right = x + edge.label_width / 2.0;
            let top = y - edge.label_height / 2.0;
            let bottom = y + edge.label_height / 2.0;
            min_x = min_x.min(left);
            min_y = min_y.min(top);
            max_x = max_x.max(right);
            max_y = max_y.max(bottom);
        }
    }

    if !min_x.is_finite() {
        min_x = 0.0;
        max_x = 0.0;
    }
    if !min_y.is_finite() {
        min_y = 0.0;
        max_y = 0.0;
    }

    (min_x, min_y, max_x, max_y)
}

fn compute_requirement_box_layout(node: &ReqNode) -> (f64, f64, Vec<LabelLayout>, Option<f64>) {
    let (type_line, name_line, body_lines) = match node {
        ReqNode::Requirement(r) => {
            let mut body = Vec::new();
            if !r.requirement_id.is_empty() {
                body.push(format!("ID: {}", r.requirement_id));
            }
            if !r.text.is_empty() {
                body.push(format!("Text: {}", r.text));
            }
            if !r.risk.is_empty() {
                body.push(format!("Risk: {}", r.risk));
            }
            if !r.verify_method.is_empty() {
                body.push(format!("Verification: {}", r.verify_method));
            }
            (format!("<<{}>>", r.req_type), r.name.clone(), body)
        }
        ReqNode::Element(e) => {
            let mut body = Vec::new();
            if !e.element_type.is_empty() {
                body.push(format!("Type: {}", e.element_type));
            }
            if !e.doc_ref.is_empty() {
                body.push(format!("Doc Ref: {}", e.doc_ref));
            }
            ("<<Element>>".to_string(), e.name.clone(), body)
        }
    };

    let type_width = line_width(&type_line, DEFAULT_CHAR_WIDTH);
    let name_width = line_width(&name_line, DEFAULT_CHAR_WIDTH);
    let mut max_width = type_width.max(name_width);

    for line in &body_lines {
        max_width = max_width.max(line_width(line, DEFAULT_CHAR_WIDTH));
    }

    let content_height =
        LINE_HEIGHT + LINE_HEIGHT + BOX_GAP + body_lines.len() as f64 * LINE_HEIGHT;

    let total_width = max_width + BOX_PADDING;
    let total_height = content_height + BOX_PADDING;

    let mut labels: Vec<LabelLayout> = Vec::new();
    labels.push(LabelLayout {
        text: type_line,
        x: 0.0,
        y: 0.0 - content_height / 2.0 + BOX_PADDING / 2.0,
        anchor: "middle",
        bold: false,
    });
    labels.push(LabelLayout {
        text: name_line,
        x: 0.0,
        y: LINE_HEIGHT - content_height / 2.0 + BOX_PADDING / 2.0,
        anchor: "middle",
        bold: true,
    });

    let left_x = -total_width / 2.0 + BOX_PADDING / 2.0;

    let mut y_offset = LINE_HEIGHT + LINE_HEIGHT + BOX_GAP;
    for line in body_lines {
        labels.push(LabelLayout {
            text: line,
            x: left_x,
            y: y_offset - content_height / 2.0 + BOX_PADDING / 2.0,
            anchor: "start",
            bold: false,
        });
        y_offset += LINE_HEIGHT;
    }

    let divider_y = if y_offset > LINE_HEIGHT + LINE_HEIGHT + BOX_GAP {
        Some(-total_height / 2.0 + (LINE_HEIGHT + LINE_HEIGHT + BOX_GAP))
    } else {
        None
    };

    (total_width, total_height, labels, divider_y)
}

fn render_svg(layout: &DiagramLayout, theme: &MermaidTheme) -> String {
    let mut svg = String::new();

    svg.push_str(&format!(
        "<svg aria-roledescription=\"requirement\" role=\"graphics-document document\" viewBox=\"0 0 {w} {h}\" style=\"max-width: {w}px; background-color: {};\" class=\"requirementDiagram\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" xmlns=\"http://www.w3.org/2000/svg\" width=\"100%\" id=\"my-svg\">",
        theme.background,
        w = layout.width,
        h = layout.height
    ));

    svg.push_str(&format!(
        "<style>#my-svg{{font-family:\"trebuchet ms\",verdana,arial,sans-serif;font-size:16px;fill:{};}}#my-svg .relationshipLine{{stroke:{};stroke-width:1;}}#my-svg .node rect{{fill:{};stroke:{};stroke-width:1.3;}}#my-svg .label{{font-family:\"trebuchet ms\",verdana,arial,sans-serif;color:{};}}#my-svg .label text,#my-svg span{{fill:{};color:{};}}#my-svg .labelBkg{{background-color:rgba(232,232,232, 0.8);}}</style>",
        theme.text_color,
        theme.edge_color,
        theme.node_fill,
        theme.node_stroke,
        theme.text_color,
        theme.text_color,
        theme.text_color
    ));

    svg.push_str("<g>");
    svg.push_str(&format!(
        "<defs><marker orient=\"auto\" markerHeight=\"20\" markerWidth=\"20\" refY=\"10\" refX=\"0\" id=\"my-svg_requirement-requirement_containsStart\"><g fill=\"none\" stroke=\"{}\" stroke-width=\"1\"><circle r=\"9\" cy=\"10\" cx=\"10\"/><line y2=\"10\" y1=\"10\" x2=\"19\" x1=\"1\"/><line x2=\"10\" x1=\"10\" y2=\"19\" y1=\"1\"/></g></marker></defs>",
        theme.edge_color
    ));
    svg.push_str(&format!(
        "<defs><marker orient=\"auto\" markerHeight=\"20\" markerWidth=\"20\" refY=\"10\" refX=\"20\" id=\"my-svg_requirement-requirement_arrowEnd\"><path d=\"M0,0 L20,10 M20,10 L0,20\" fill=\"none\" stroke=\"{}\" stroke-width=\"1\"/></marker></defs>",
        theme.edge_color
    ));

    svg.push_str("<g class=\"root\">");
    svg.push_str("<g class=\"clusters\"/>");

    svg.push_str("<g class=\"edgePaths\">");
    for edge in &layout.edges {
        let d = points_to_path_d(&edge.points);
        let is_contains = edge.rel_type == "contains";
        let dash = if is_contains {
            ""
        } else {
            "stroke-dasharray: 10,7;"
        };
        let marker_end = if is_contains {
            ""
        } else {
            " marker-end=\"url(#my-svg_requirement-requirement_arrowEnd)\""
        };
        svg.push_str(&format!(
            "<path{marker_end} style=\"fill:none;{dash}\" class=\"edge-thickness-normal edge-pattern-dashed relationshipLine\" id=\"{}\" d=\"{}\"/>",
            escape_xml(&edge.id),
            d
        ));
    }
    svg.push_str("</g>");

    svg.push_str("<g class=\"edgeLabels\">");
    for edge in &layout.edges {
        let Some((x, y)) = edge.label_pos else {
            continue;
        };

        let x2 = -edge.label_width / 2.0;
        let y2 = -edge.label_height / 2.0;

        svg.push_str(&format!(
            "<g transform=\"translate({x}, {y})\" class=\"edgeLabel\">",
            x = x,
            y = y,
        ));
        svg.push_str(&format!(
            "<rect x=\"{x2}\" y=\"{y2}\" width=\"{w}\" height=\"{h}\" fill=\"#E8E8E8\" fill-opacity=\"0.8\" stroke=\"none\"/>",
            x2 = x2,
            y2 = y2,
            w = edge.label_width,
            h = edge.label_height,
        ));
        svg.push_str(&format!(
            "<text x=\"0\" y=\"0\" text-anchor=\"middle\" dominant-baseline=\"middle\" fill=\"{}\">{}</text>",
            theme.text_color,
            escape_xml(&edge.label)
        ));
        svg.push_str("</g>");
    }
    svg.push_str("</g>");

    svg.push_str("<g class=\"nodes\">");
    let mut node_ids: Vec<&String> = layout.nodes.keys().collect();
    node_ids.sort();
    for node_id in node_ids {
        let Some(node_layout) = layout.nodes.get(node_id) else {
            continue;
        };
        let x2 = -node_layout.width / 2.0;
        let y2 = -node_layout.height / 2.0;

        svg.push_str(&format!(
            "<g transform=\"translate({x},{y})\" id=\"{id}\" class=\"node default\">",
            x = node_layout.x,
            y = node_layout.y,
            id = escape_xml(&node_layout.id)
        ));

        svg.push_str(&format!(
            "<rect x=\"{x2}\" y=\"{y2}\" width=\"{w}\" height=\"{h}\"/>",
            x2 = x2,
            y2 = y2,
            w = node_layout.width,
            h = node_layout.height
        ));

        if let Some(divider_y) = node_layout.divider_y {
            svg.push_str(&format!(
                "<line x1=\"{x1}\" y1=\"{y}\" x2=\"{x2}\" y2=\"{y}\" stroke=\"{}\" stroke-width=\"1.3\"/>",
                theme.node_stroke,
                x1 = x2,
                x2 = x2 + node_layout.width,
                y = divider_y
            ));
        }

        for label in &node_layout.labels {
            let font_weight = if label.bold {
                " font-weight=\"bold\""
            } else {
                ""
            };
            svg.push_str(&format!(
                "<text x=\"{x}\" y=\"{y}\" text-anchor=\"{anchor}\" dominant-baseline=\"middle\" fill=\"{}\"{font_weight}>{}</text>",
                theme.text_color,
                escape_xml(&label.text),
                x = label.x,
                y = label.y,
                anchor = label.anchor,
            ));
        }

        svg.push_str("</g>");
    }
    svg.push_str("</g>");

    svg.push_str("</g></g></svg>");

    svg
}

fn points_to_path_d(points: &[(f64, f64)]) -> String {
    if points.is_empty() {
        return String::new();
    }
    let mut d = String::new();
    let (x0, y0) = points[0];
    d.push_str(&format!("M{x0},{y0}"));
    for (x, y) in &points[1..] {
        d.push_str(&format!("L{x},{y}"));
    }
    d
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
