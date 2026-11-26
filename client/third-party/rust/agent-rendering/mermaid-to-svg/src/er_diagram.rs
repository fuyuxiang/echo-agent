use std::collections::{BTreeMap, BTreeSet};

use crate::error::MermaidError;
use crate::text_wrap::{line_width, DEFAULT_CHAR_WIDTH};
use crate::theme::MermaidTheme;
use dagre_rust::layout::layout as dagre_layout;
use dagre_rust::{GraphConfig, GraphEdge, GraphNode};
use graphlib_rust::Graph;

// --- Constants matching mermaid.js ER renderer defaults ---
const PADDING: f64 = 10.0;
const TEXT_PADDING: f64 = 6.0;
const NODE_SEP: f64 = 140.0;
const RANK_SEP: f64 = 80.0;
const GRAPH_MARGIN: f64 = 8.0;
const LINE_HEIGHT: f64 = 36.75;
const MIN_ENTITY_WIDTH: f64 = 100.0;
const FONT_SIZE: f64 = 16.0;
const COLUMN_TEXT_PADDING: f64 = 8.0;

// --- ER-specific AST ---

#[derive(Debug, Clone, PartialEq)]
enum Cardinality {
    ZeroOrOne,
    ZeroOrMore,
    OneOrMore,
    OnlyOne,
}

impl Cardinality {
    fn marker_name(&self) -> &'static str {
        match self {
            Cardinality::ZeroOrOne => "zeroOrOne",
            Cardinality::ZeroOrMore => "zeroOrMore",
            Cardinality::OneOrMore => "oneOrMore",
            Cardinality::OnlyOne => "onlyOne",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Identification {
    Identifying,
    NonIdentifying,
}

#[derive(Debug, Clone)]
struct RelSpec {
    card_a: Cardinality,
    card_b: Cardinality,
    rel_type: Identification,
}

#[derive(Debug, Clone)]
struct Attribute {
    attr_type: String,
    name: String,
}

#[derive(Debug, Clone)]
struct Entity {
    id: String,
    attributes: Vec<Attribute>,
}

#[derive(Debug, Clone)]
struct Relationship {
    entity_a: String,
    entity_b: String,
    role: String,
    rel_spec: RelSpec,
}

#[derive(Debug, Clone)]
struct ErDiagram {
    entities: BTreeMap<String, Entity>,
    relationships: Vec<Relationship>,
}

// --- Layout types ---

#[derive(Debug, Clone)]
struct EntityLayout {
    id: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    header_height: f64,
    max_type_width: f64,
    attributes: Vec<Attribute>,
    row_heights: Vec<f64>,
}

#[derive(Debug, Clone)]
struct EdgeLayout {
    #[allow(dead_code)]
    from: String,
    #[allow(dead_code)]
    to: String,
    role: String,
    rel_spec: RelSpec,
    points: Vec<(f64, f64)>,
    label_pos: Option<(f64, f64)>,
    label_width: f64,
    label_height: f64,
}

#[derive(Debug, Clone)]
struct DiagramLayout {
    entities: BTreeMap<String, EntityLayout>,
    edges: Vec<EdgeLayout>,
    width: f64,
    height: f64,
}

// --- Public entry point ---

pub fn render_er_diagram_to_svg(
    mermaid_source: &str,
    theme: &MermaidTheme,
) -> Result<String, MermaidError> {
    let diagram = parse_er_diagram(mermaid_source)?;
    let layout = compute_layout(&diagram);
    Ok(render_svg(&layout, theme))
}

// --- Parser ---

fn parse_er_diagram(input: &str) -> Result<ErDiagram, MermaidError> {
    let lines: Vec<&str> = input.lines().collect();
    let mut i = 0_usize;

    // Find header
    while i < lines.len() {
        let line = lines[i].trim();
        if line.is_empty() || line.starts_with("%%") {
            i += 1;
            continue;
        }
        if line.split_whitespace().next() == Some("erDiagram") {
            i += 1;
            break;
        }
        return Err(MermaidError::ParseError {
            line: i + 1,
            message: "Expected 'erDiagram' declaration".to_string(),
        });
    }

    let mut entities: BTreeMap<String, Entity> = BTreeMap::new();
    let mut referenced_entities: BTreeSet<String> = BTreeSet::new();
    let mut relationships: Vec<Relationship> = Vec::new();

    while i < lines.len() {
        let raw = lines[i];
        let line = raw.trim();
        let line_no = i + 1;
        i += 1;

        if line.is_empty() || line.starts_with("%%") {
            continue;
        }

        // Entity block: `ENTITY_NAME {`
        if let Some(name_raw) = line.strip_suffix('{') {
            let name = name_raw.trim();
            if name.is_empty() {
                return Err(MermaidError::ParseError {
                    line: line_no,
                    message: "Expected entity name before '{'".to_string(),
                });
            }

            let mut attrs: Vec<Attribute> = Vec::new();
            while i < lines.len() {
                let attr_raw = lines[i];
                let attr_line = attr_raw.trim();
                i += 1;

                if attr_line.is_empty() || attr_line.starts_with("%%") {
                    continue;
                }
                if attr_line == "}" {
                    break;
                }
                // Parse "type name" pairs
                let parts: Vec<&str> = attr_line.splitn(2, char::is_whitespace).collect();
                if parts.len() >= 2 {
                    attrs.push(Attribute {
                        attr_type: parts[0].to_string(),
                        name: parts[1].trim().to_string(),
                    });
                } else if !parts.is_empty() {
                    attrs.push(Attribute {
                        attr_type: parts[0].to_string(),
                        name: String::new(),
                    });
                }
            }

            entities.insert(
                name.to_string(),
                Entity {
                    id: name.to_string(),
                    attributes: attrs,
                },
            );
            continue;
        }

        if line == "}" {
            continue;
        }

        // Relationship: `ENTITY_A ||--o{ ENTITY_B : label`
        if line.contains("--") || line.contains("..") {
            if let Some(rel) = parse_relationship(line, line_no)? {
                referenced_entities.insert(rel.entity_a.clone());
                referenced_entities.insert(rel.entity_b.clone());
                relationships.push(rel);
                continue;
            }
        }

        return Err(MermaidError::ParseError {
            line: line_no,
            message: format!("Unrecognized erDiagram line: {line}"),
        });
    }

    // Ensure all referenced entities exist
    for id in referenced_entities {
        entities.entry(id.clone()).or_insert_with(|| Entity {
            id,
            attributes: Vec::new(),
        });
    }

    Ok(ErDiagram {
        entities,
        relationships,
    })
}

fn parse_relationship(line: &str, line_no: usize) -> Result<Option<Relationship>, MermaidError> {
    // Split on `:` to get role label
    let (lhs, role) = match line.split_once(':') {
        Some((a, b)) => {
            let label = b.trim();
            (
                a.trim(),
                if label.is_empty() {
                    String::new()
                } else {
                    label.to_string()
                },
            )
        }
        None => (line.trim(), String::new()),
    };

    let parts: Vec<&str> = lhs.split_whitespace().collect();
    if parts.len() < 3 {
        return Err(MermaidError::ParseError {
            line: line_no,
            message: format!("Invalid erDiagram relationship: {line}"),
        });
    }

    let entity_a = parts[0].to_string();
    let rel_str = parts[1];
    let entity_b = parts[2].to_string();

    let rel_spec = parse_rel_spec(rel_str, line_no)?;

    Ok(Some(Relationship {
        entity_a,
        entity_b,
        role,
        rel_spec,
    }))
}

fn parse_rel_spec(s: &str, line_no: usize) -> Result<RelSpec, MermaidError> {
    // Format: cardA--cardB or cardA..cardB
    // Cards: || (only one), |o or o| (zero or one), }| or |{ (one or more),
    //        }o or o{ (zero or more)
    // `--` = IDENTIFYING, `..` = NON_IDENTIFYING

    let (left_part, rel_type, right_part) = if let Some(idx) = s.find("--") {
        (&s[..idx], Identification::Identifying, &s[idx + 2..])
    } else if let Some(idx) = s.find("..") {
        (&s[..idx], Identification::NonIdentifying, &s[idx + 2..])
    } else {
        return Err(MermaidError::ParseError {
            line: line_no,
            message: format!("Invalid relationship spec: {s}"),
        });
    };

    let card_a = parse_cardinality(left_part, line_no)?;
    let card_b = parse_cardinality(right_part, line_no)?;

    Ok(RelSpec {
        card_a,
        card_b,
        rel_type,
    })
}

fn parse_cardinality(s: &str, line_no: usize) -> Result<Cardinality, MermaidError> {
    match s {
        "||" => Ok(Cardinality::OnlyOne),
        "|o" | "o|" => Ok(Cardinality::ZeroOrOne),
        "|{" | "}|" => Ok(Cardinality::OneOrMore),
        "o{" | "}o" => Ok(Cardinality::ZeroOrMore),
        _ => Err(MermaidError::ParseError {
            line: line_no,
            message: format!("Invalid cardinality: {s}"),
        }),
    }
}

// --- Layout computation using dagre ---

fn compute_entity_metrics(entity: &Entity) -> (f64, f64, f64, f64, Vec<f64>) {
    // Returns: (total_width, total_height, header_height, max_type_width, row_heights)
    let header_text = &entity.id;
    let header_width = line_width(header_text, DEFAULT_CHAR_WIDTH);
    let header_height = LINE_HEIGHT + TEXT_PADDING;

    if entity.attributes.is_empty() {
        let width = (header_width + PADDING * 2.0).max(MIN_ENTITY_WIDTH);
        let height = header_height + PADDING;
        return (width, height, header_height, 0.0, Vec::new());
    }

    let mut max_type_width: f64 = 0.0;
    let mut max_name_width: f64 = 0.0;
    let mut row_heights = Vec::new();

    for attr in &entity.attributes {
        let type_w = line_width(&attr.attr_type, DEFAULT_CHAR_WIDTH);
        let name_w = line_width(&attr.name, DEFAULT_CHAR_WIDTH);
        max_type_width = max_type_width.max(type_w + COLUMN_TEXT_PADDING * 2.0);
        max_name_width = max_name_width.max(name_w + COLUMN_TEXT_PADDING * 2.0);
        row_heights.push(LINE_HEIGHT + TEXT_PADDING);
    }

    let attr_total_width = max_type_width + max_name_width;
    let content_width = attr_total_width.max(header_width + PADDING * 2.0);
    let total_width = content_width.max(MIN_ENTITY_WIDTH);

    let total_attr_height: f64 = row_heights.iter().sum();
    let total_height = header_height + total_attr_height;

    (
        total_width,
        total_height,
        header_height,
        max_type_width,
        row_heights,
    )
}

fn compute_layout(diagram: &ErDiagram) -> DiagramLayout {
    let mut entity_metrics: BTreeMap<String, (f64, f64, f64, f64, Vec<f64>)> = BTreeMap::new();
    for (id, entity) in &diagram.entities {
        entity_metrics.insert(id.clone(), compute_entity_metrics(entity));
    }

    type DagreGraph = Graph<GraphConfig, GraphNode, GraphEdge>;
    let mut g: DagreGraph = Graph::new(Some(graphlib_rust::GraphOption {
        directed: Some(true),
        multigraph: Some(true),
        compound: Some(false),
    }));

    g.set_graph(GraphConfig {
        rankdir: Some("tb".to_string()),
        nodesep: Some(NODE_SEP as f32),
        ranksep: Some(RANK_SEP as f32),
        edgesep: Some(20.0),
        marginx: Some(GRAPH_MARGIN as f32),
        marginy: Some(GRAPH_MARGIN as f32),
        ..Default::default()
    });

    for (id, (w, h, _, _, _)) in &entity_metrics {
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
    for rel in &diagram.relationships {
        let label_text = &rel.role;
        let label_width = if label_text.is_empty() {
            0.0
        } else {
            line_width(label_text, DEFAULT_CHAR_WIDTH)
        };
        let label_height = if label_text.is_empty() {
            0.0
        } else {
            LINE_HEIGHT
        };

        let edge_label = GraphEdge {
            labelpos: Some("c".to_string()),
            width: Some(label_width as f32),
            height: Some(label_height as f32),
            ..Default::default()
        };

        let _ = g.set_edge(&rel.entity_a, &rel.entity_b, Some(edge_label), None);
        edge_keys.push((rel.entity_a.clone(), rel.entity_b.clone()));
    }

    dagre_layout(&mut g);

    let mut positions: BTreeMap<String, (f64, f64)> = BTreeMap::new();
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

        let rel = &diagram.relationships[idx];
        let label_width = edge.width.unwrap_or(0.0) as f64;
        let label_height = edge.height.unwrap_or(0.0) as f64;

        edges.push(EdgeLayout {
            from: from.clone(),
            to: to.clone(),
            role: rel.role.clone(),
            rel_spec: rel.rel_spec.clone(),
            points,
            label_pos,
            label_width,
            label_height,
        });
    }

    let mut entity_layouts: BTreeMap<String, EntityLayout> = BTreeMap::new();
    for (id, (x, y)) in &positions {
        let Some((w, h, header_h, max_type_w, row_heights)) = entity_metrics.get(id).cloned()
        else {
            continue;
        };
        let entity = diagram.entities.get(id).cloned().unwrap_or(Entity {
            id: id.clone(),
            attributes: Vec::new(),
        });
        entity_layouts.insert(
            id.clone(),
            EntityLayout {
                id: id.clone(),
                x: *x,
                y: *y,
                width: w,
                height: h,
                header_height: header_h,
                max_type_width: max_type_w,
                attributes: entity.attributes,
                row_heights,
            },
        );
    }

    // Compute bounds and normalize
    let (min_x, min_y, max_x, max_y) = compute_bounds(&entity_layouts, &edges);
    let dx = GRAPH_MARGIN - min_x;
    let dy = GRAPH_MARGIN - min_y;

    for entity in entity_layouts.values_mut() {
        entity.x += dx;
        entity.y += dy;
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
        entities: entity_layouts,
        edges,
        width,
        height,
    }
}

fn compute_bounds(
    entities: &BTreeMap<String, EntityLayout>,
    edges: &[EdgeLayout],
) -> (f64, f64, f64, f64) {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;

    for entity in entities.values() {
        let left = entity.x - entity.width / 2.0;
        let right = entity.x + entity.width / 2.0;
        let top = entity.y - entity.height / 2.0;
        let bottom = entity.y + entity.height / 2.0;
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

// --- SVG Rendering ---

fn render_svg(layout: &DiagramLayout, theme: &MermaidTheme) -> String {
    let mut svg = String::new();

    svg.push_str(&format!(
        "<svg aria-roledescription=\"er\" role=\"graphics-document document\" \
         viewBox=\"0 0 {w} {h}\" style=\"max-width: {w}px; background-color: {bg};\" \
         class=\"erDiagram\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" \
         xmlns=\"http://www.w3.org/2000/svg\" width=\"100%\" id=\"my-svg\">",
        bg = theme.background,
        w = layout.width,
        h = layout.height
    ));

    // CSS styles matching mermaid.js ER theme
    svg.push_str(&format!(
        "<style>\
         #my-svg {{font-family:\"trebuchet ms\",verdana,arial,sans-serif;font-size:{font_size}px;fill:{text};}}\
         #my-svg .entityBox {{fill:{node_fill};stroke:{node_stroke};}}\
         #my-svg .relationshipLine {{stroke:{edge};stroke-width:1;fill:none;}}\
         #my-svg .marker {{fill:none !important;stroke:{edge} !important;stroke-width:1;}}\
         #my-svg .edgeLabel .label {{fill:{node_stroke};font-size:14px;}}
         #my-svg .label {{font-family:\"trebuchet ms\",verdana,arial,sans-serif;color:{text};}}\
         #my-svg .label text, #my-svg span {{fill:{text};color:{text};}}\
         #my-svg .node rect, #my-svg .node circle, #my-svg .node ellipse, #my-svg .node polygon {{fill:{node_fill};stroke:{node_stroke};stroke-width:1px;}}\
         #my-svg .divider {{stroke:{node_stroke};stroke-width:1;}}\
         </style>",
        font_size = FONT_SIZE,
        text = theme.text_color,
        node_fill = theme.node_fill,
        node_stroke = theme.node_stroke,
        edge = theme.edge_color,
    ));

    // ER-specific SVG marker definitions
    svg.push_str("<defs>");
    render_er_markers(&mut svg, theme);
    svg.push_str("</defs>");

    svg.push_str("<g>");

    // Edges (paths)
    svg.push_str("<g class=\"edgePaths\">");
    for edge in &layout.edges {
        render_edge_path(&mut svg, edge);
    }
    svg.push_str("</g>");

    // Edge labels
    svg.push_str("<g class=\"edgeLabels\">");
    for edge in &layout.edges {
        render_edge_label(&mut svg, edge, theme);
    }
    svg.push_str("</g>");

    // Entity nodes
    svg.push_str("<g class=\"nodes\">");
    for entity in layout.entities.values() {
        render_entity_node(&mut svg, entity, theme);
    }
    svg.push_str("</g>");

    svg.push_str("</g></svg>");

    svg
}

fn render_er_markers(svg: &mut String, theme: &MermaidTheme) {
    let edge_color = &theme.edge_color;

    // onlyOne markers: two perpendicular bars
    svg.push_str(&format!(
        "<marker id=\"my-svg_er-onlyOneStart\" class=\"marker onlyOne\" \
         refX=\"0\" refY=\"9\" markerWidth=\"18\" markerHeight=\"18\" orient=\"auto\">\
         <path d=\"M9,0 L9,18 M15,0 L15,18\" stroke=\"{edge_color}\" fill=\"none\"/>\
         </marker>"
    ));
    svg.push_str(&format!(
        "<marker id=\"my-svg_er-onlyOneEnd\" class=\"marker onlyOne\" \
         refX=\"18\" refY=\"9\" markerWidth=\"18\" markerHeight=\"18\" orient=\"auto\">\
         <path d=\"M3,0 L3,18 M9,0 L9,18\" stroke=\"{edge_color}\" fill=\"none\"/>\
         </marker>"
    ));

    // zeroOrOne markers: circle + perpendicular bar
    svg.push_str(&format!(
        "<marker id=\"my-svg_er-zeroOrOneStart\" class=\"marker zeroOrOne\" \
         refX=\"0\" refY=\"9\" markerWidth=\"30\" markerHeight=\"18\" orient=\"auto\">\
         <circle fill=\"white\" cx=\"21\" cy=\"9\" r=\"6\" stroke=\"{edge_color}\"/>\
         <path d=\"M9,0 L9,18\" stroke=\"{edge_color}\" fill=\"none\"/>\
         </marker>"
    ));
    svg.push_str(&format!(
        "<marker id=\"my-svg_er-zeroOrOneEnd\" class=\"marker zeroOrOne\" \
         refX=\"30\" refY=\"9\" markerWidth=\"30\" markerHeight=\"18\" orient=\"auto\">\
         <circle fill=\"white\" cx=\"9\" cy=\"9\" r=\"6\" stroke=\"{edge_color}\"/>\
         <path d=\"M21,0 L21,18\" stroke=\"{edge_color}\" fill=\"none\"/>\
         </marker>"
    ));

    // oneOrMore markers: crow's foot + perpendicular bar
    svg.push_str(&format!(
        "<marker id=\"my-svg_er-oneOrMoreStart\" class=\"marker oneOrMore\" \
         refX=\"18\" refY=\"18\" markerWidth=\"45\" markerHeight=\"36\" orient=\"auto\">\
         <path d=\"M0,18 Q 18,0 36,18 Q 18,36 0,18 M42,9 L42,27\" stroke=\"{edge_color}\" fill=\"none\"/>\
         </marker>"
    ));
    svg.push_str(&format!(
        "<marker id=\"my-svg_er-oneOrMoreEnd\" class=\"marker oneOrMore\" \
         refX=\"27\" refY=\"18\" markerWidth=\"45\" markerHeight=\"36\" orient=\"auto\">\
         <path d=\"M3,9 L3,27 M9,18 Q27,0 45,18 Q27,36 9,18\" stroke=\"{edge_color}\" fill=\"none\"/>\
         </marker>"
    ));

    // zeroOrMore markers: circle + crow's foot
    svg.push_str(&format!(
        "<marker id=\"my-svg_er-zeroOrMoreStart\" class=\"marker zeroOrMore\" \
         refX=\"18\" refY=\"18\" markerWidth=\"57\" markerHeight=\"36\" orient=\"auto\">\
         <circle fill=\"white\" cx=\"48\" cy=\"18\" r=\"6\" stroke=\"{edge_color}\"/>\
         <path d=\"M0,18 Q18,0 36,18 Q18,36 0,18\" stroke=\"{edge_color}\" fill=\"none\"/>\
         </marker>"
    ));
    svg.push_str(&format!(
        "<marker id=\"my-svg_er-zeroOrMoreEnd\" class=\"marker zeroOrMore\" \
         refX=\"39\" refY=\"18\" markerWidth=\"57\" markerHeight=\"36\" orient=\"auto\">\
         <circle fill=\"white\" cx=\"9\" cy=\"18\" r=\"6\" stroke=\"{edge_color}\"/>\
         <path d=\"M21,18 Q39,0 57,18 Q39,36 21,18\" stroke=\"{edge_color}\" fill=\"none\"/>\
         </marker>"
    ));
}

fn render_edge_path(svg: &mut String, edge: &EdgeLayout) {
    let d = points_to_path_d(&edge.points);
    let dash = if edge.rel_spec.rel_type == Identification::NonIdentifying {
        " stroke-dasharray=\"8,8\""
    } else {
        ""
    };

    // In mermaid.js ER: arrowTypeStart = cardA, arrowTypeEnd = cardB
    // card_a is the cardinality at entity_a's side, card_b is at entity_b's side.
    // marker-start decorates the start of the path (entity_a), marker-end the end (entity_b).
    let marker_start_name = edge.rel_spec.card_a.marker_name();
    let marker_end_name = edge.rel_spec.card_b.marker_name();

    svg.push_str(&format!(
        "<path class=\"edge-thickness-normal relationshipLine\" \
         d=\"{d}\" \
         marker-start=\"url(#my-svg_er-{marker_start_name}Start)\" \
         marker-end=\"url(#my-svg_er-{marker_end_name}End)\" \
         style=\"fill:none;\"{dash}/>",
    ));
}

fn render_edge_label(svg: &mut String, edge: &EdgeLayout, theme: &MermaidTheme) {
    if edge.role.is_empty() {
        return;
    }

    let Some((x, y)) = edge.label_pos else {
        return;
    };

    svg.push_str(&format!(
        "<g transform=\"translate({x}, {y})\" class=\"edgeLabel\">"
    ));
    svg.push_str(&format!(
        "<text x=\"0\" y=\"0\" text-anchor=\"middle\" dominant-baseline=\"middle\" \
         fill=\"{color}\" style=\"font-size:14px\">{label}</text>",
        color = theme.text_color,
        label = escape_xml(&edge.role)
    ));
    svg.push_str("</g>");
}

fn render_entity_node(svg: &mut String, entity: &EntityLayout, theme: &MermaidTheme) {
    let x_offset = -entity.width / 2.0;
    let y_offset = -entity.height / 2.0;

    svg.push_str(&format!(
        "<g transform=\"translate({x},{y})\" id=\"entity-{id}\" class=\"node default\">",
        x = entity.x,
        y = entity.y,
        id = escape_xml(&entity.id)
    ));

    // Outer rectangle (entity box)
    svg.push_str(&format!(
        "<rect x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" class=\"entityBox\" rx=\"0\" ry=\"0\"/>",
        x = x_offset,
        y = y_offset,
        w = entity.width,
        h = entity.height
    ));

    // Header text (entity name)
    let header_text_y = y_offset + entity.header_height / 2.0;
    svg.push_str(&format!(
        "<text x=\"0\" y=\"{y}\" text-anchor=\"middle\" dominant-baseline=\"middle\" \
         fill=\"{color}\" class=\"er entityLabel\">{label}</text>",
        y = header_text_y,
        color = theme.text_color,
        label = escape_xml(&entity.id)
    ));

    if !entity.attributes.is_empty() {
        // Horizontal divider between header and attributes
        let divider_y = y_offset + entity.header_height;
        svg.push_str(&format!(
            "<line x1=\"{x1}\" y1=\"{y}\" x2=\"{x2}\" y2=\"{y}\" class=\"divider\"/>",
            x1 = x_offset,
            x2 = x_offset + entity.width,
            y = divider_y
        ));

        let col_divider_x = x_offset + entity.max_type_width;
        let attr_start_y = y_offset + entity.header_height;
        let attr_end_y = y_offset + entity.height;

        // Attribute rows
        let mut current_y = attr_start_y;

        // Compute alternating row fill colors matching mermaid.js erBox behaviour.
        // Light themes keep the upstream values: rowOdd = lighten(primary, 75)
        // ≈ #ffffff, rowEven = slightly lighter than node_fill. Dark themes
        // derive both stripes from the background instead (as theme-dark does),
        // so attribute text keeps its contrast instead of white-on-white.
        let (row_odd_fill, row_even_fill) = if is_dark_hex(&theme.background) {
            (
                lighten_hex(&theme.background, 0.08),
                lighten_hex(&theme.background, 0.16),
            )
        } else {
            (String::from("#ffffff"), lighten_hex(&theme.node_fill, 0.25))
        };

        for (i, attr) in entity.attributes.iter().enumerate() {
            let row_h = entity
                .row_heights
                .get(i)
                .copied()
                .unwrap_or(LINE_HEIGHT + TEXT_PADDING);
            let text_y = current_y + row_h / 2.0;

            // Alternating row background rect (zebra striping)
            // Mermaid uses contentRowIndex = i + 1; isEven when contentRowIndex % 2 == 0 && i > 0
            let is_even = (i + 1) % 2 == 0 && i > 0;
            let row_fill = if is_even {
                row_even_fill.as_str()
            } else {
                row_odd_fill.as_str()
            };
            svg.push_str(&format!(
                "<rect x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" \
                 style=\"fill:{fill};stroke:{stroke}\" class=\"er attributeBox{parity}\"/>",
                x = x_offset,
                y = current_y,
                w = entity.width,
                h = row_h,
                fill = row_fill,
                stroke = theme.node_stroke,
                parity = if is_even { "Even" } else { "Odd" },
            ));

            // Horizontal line between attribute rows (faint separator)
            if i > 0 {
                svg.push_str(&format!(
                    "<line x1=\"{x1}\" y1=\"{y}\" x2=\"{x2}\" y2=\"{y}\" class=\"divider\" style=\"stroke-opacity:0.3\"/>",
                    x1 = x_offset,
                    x2 = x_offset + entity.width,
                    y = current_y
                ));
            }

            // Type text (left column)
            let type_text_x = x_offset + COLUMN_TEXT_PADDING;
            svg.push_str(&format!(
                "<text x=\"{x}\" y=\"{y}\" text-anchor=\"start\" dominant-baseline=\"middle\" \
                 fill=\"{color}\" class=\"er entityLabel\">{text}</text>",
                x = type_text_x,
                y = text_y,
                color = theme.text_color,
                text = escape_xml(&attr.attr_type)
            ));

            // Name text (right column)
            let name_text_x = col_divider_x + COLUMN_TEXT_PADDING;
            svg.push_str(&format!(
                "<text x=\"{x}\" y=\"{y}\" text-anchor=\"start\" dominant-baseline=\"middle\" \
                 fill=\"{color}\" class=\"er entityLabel\">{text}</text>",
                x = name_text_x,
                y = text_y,
                color = theme.text_color,
                text = escape_xml(&attr.name)
            ));

            current_y += row_h;
        }

        // Vertical divider between type and name columns.
        // Drawn after the row background rects so it is not covered by their fill.
        svg.push_str(&format!(
            "<line x1=\"{x}\" y1=\"{y1}\" x2=\"{x}\" y2=\"{y2}\" class=\"divider\"/>",
            x = col_divider_x,
            y1 = attr_start_y,
            y2 = attr_end_y
        ));
    }

    svg.push_str("</g>");
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

/// Lighten a hex color by blending it toward white.
/// `amount` is 0.0 (no change) to 1.0 (white).
fn is_dark_hex(hex: &str) -> bool {
    let hex = hex.trim_start_matches('#');
    if hex.len() < 6 {
        return false;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(255) as f64;
    let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(255) as f64;
    let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(255) as f64;
    0.2126 * r + 0.7152 * g + 0.0722 * b < 128.0
}

fn lighten_hex(hex: &str, amount: f64) -> String {
    let hex = hex.trim_start_matches('#');
    if hex.len() < 6 {
        return format!("#{hex}");
    }
    let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
    let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
    let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
    let lr = r as f64 + (255.0 - r as f64) * amount;
    let lg = g as f64 + (255.0 - g as f64) * amount;
    let lb = b as f64 + (255.0 - b as f64) * amount;
    format!("#{:02X}{:02X}{:02X}", lr as u8, lg as u8, lb as u8)
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
