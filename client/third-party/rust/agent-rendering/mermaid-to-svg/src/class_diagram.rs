use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::error::MermaidError;
use crate::text_wrap::{line_width, DEFAULT_CHAR_WIDTH};
use crate::theme::MermaidTheme;
use dagre_rust::layout::layout as dagre_layout;
use dagre_rust::{GraphConfig, GraphEdge, GraphNode};
use graphlib_rust::Graph;

// --- Constants matching Mermaid 11.12.2 class diagram styles ---
// Ported from: classBox.ts (PADDING = config.class.padding ?? 12, GAP = PADDING)
// and shapeUtil.ts (textHelper positions sections with GAP*2 and GAP*4 spacing)

const PADDING: f64 = 12.0; // outer padding around the box content
const GAP: f64 = 12.0; // inter-section gap (same as PADDING in mermaid)
const TEXT_PADDING: f64 = 3.0; // per-line text padding (non-HTML mode: 3px)
const LINE_HEIGHT: f64 = 24.0;
const TITLE_FONT_SIZE: f64 = 18.0;
const MEMBER_FONT_SIZE: f64 = 14.0;
const NODE_FILL: &str = "#ECECFF";
const NODE_STROKE: &str = "#9370DB";
const NODE_STROKE_WIDTH: f64 = 1.0; // mermaid CSS: stroke-width: 1px
const EDGE_COLOR: &str = "#333333";
const TEXT_COLOR: &str = "#333";
const GRAPH_MARGIN: f64 = 8.0;
const NODE_SEP: f64 = 50.0;
const RANK_SEP: f64 = 50.0;
// Per-marker offsets: how far the marker body extends from refX.
// extensionStart: refX=18, tip at x=1 → extends 17px backward from path point.
// dependencyEnd: refX=13, tip at x=18 → extends 5px forward past path point.
const EXTENSION_MARKER_OFFSET: f64 = 17.0;
const DEPENDENCY_MARKER_OFFSET: f64 = 6.0;
const COMPOSITION_MARKER_OFFSET: f64 = 17.0;
const AGGREGATION_MARKER_OFFSET: f64 = 17.0;

pub fn render_class_diagram_to_svg(
    mermaid_source: &str,
    theme: &MermaidTheme,
) -> Result<String, MermaidError> {
    let diagram = parse_class_diagram(mermaid_source)?;
    let layout = compute_layout(&diagram);
    Ok(render_svg(&layout, theme))
}

// --- AST types ---

#[derive(Debug, Clone, Copy, PartialEq)]
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
struct ClassDiagram {
    direction: Direction,
    classes: BTreeMap<String, ClassInfo>,
    relationships: Vec<Relationship>,
}

#[derive(Debug, Clone)]
struct ClassInfo {
    name: String,
    stereotype: Option<String>,
    attributes: Vec<String>,
    methods: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum RelationType {
    Extension,   // <|-- (solid line, open triangle) - inheritance
    Composition, // *-- (solid line, filled diamond)
    Aggregation, // o-- (solid line, open diamond)
    Dependency,  // --> (solid line, filled arrowhead)
    Realization, // <|.. (dashed line, open triangle) - implements
    DashedDep,   // ..> (dashed line, filled arrowhead)
    Association, // -- (solid line, no arrowhead)
    DashedAssoc, // .. (dashed line, no arrowhead)
}

impl RelationType {
    fn is_dashed(self) -> bool {
        matches!(
            self,
            RelationType::Realization | RelationType::DashedDep | RelationType::DashedAssoc
        )
    }

    fn start_marker(self) -> Option<&'static str> {
        match self {
            RelationType::Extension => Some("extensionStart"),
            RelationType::Realization => Some("extensionStart"),
            RelationType::Composition => Some("compositionStart"),
            RelationType::Aggregation => Some("aggregationStart"),
            _ => None,
        }
    }

    fn end_marker(self) -> Option<&'static str> {
        match self {
            RelationType::Dependency => Some("dependencyEnd"),
            RelationType::DashedDep => Some("dependencyEnd"),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
struct Relationship {
    from: String,
    to: String,
    label: Option<String>,
    rel_type: RelationType,
}

// --- Layout types ---

#[derive(Debug, Clone)]
struct ClassNodeLayout {
    id: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    info: ClassInfo,
    title_y: f64,
    annotation_y: Option<f64>,
    attr_divider_y: f64,
    method_divider_y: f64,
    attr_start_y: f64,
    method_start_y: f64,
}

#[derive(Debug, Clone)]
struct EdgeLayout {
    rel_type: RelationType,
    label: Option<String>,
    points: Vec<(f64, f64)>,
    label_pos: Option<(f64, f64)>,
    label_width: f64,
    // Source/target node center and half-dimensions for edge clipping
    src_cx: f64,
    src_cy: f64,
    src_hw: f64,
    src_hh: f64,
    tgt_cx: f64,
    tgt_cy: f64,
    tgt_hw: f64,
    tgt_hh: f64,
}

#[derive(Debug, Clone)]
struct DiagramLayout {
    nodes: Vec<ClassNodeLayout>,
    edges: Vec<EdgeLayout>,
    width: f64,
    height: f64,
}

// --- Parser ---

fn parse_class_diagram(input: &str) -> Result<ClassDiagram, MermaidError> {
    let lines: Vec<&str> = input.lines().collect();

    let mut i = 0_usize;
    while i < lines.len() {
        let line = lines[i].trim();
        if line.is_empty() || line.starts_with("%%") {
            i += 1;
            continue;
        }
        if line.split_whitespace().next() == Some("classDiagram") {
            i += 1;
            break;
        }
        return Err(MermaidError::ParseError {
            line: i + 1,
            message: "Expected 'classDiagram' declaration".to_string(),
        });
    }

    let mut direction = Direction::Tb;
    let mut classes: BTreeMap<String, ClassInfo> = BTreeMap::new();
    let mut referenced: BTreeSet<String> = BTreeSet::new();
    let mut relationships: Vec<Relationship> = Vec::new();

    while i < lines.len() {
        let raw = lines[i];
        let line = raw.trim();
        let line_no = i + 1;
        i += 1;

        if line.is_empty() || line.starts_with("%%") {
            continue;
        }

        if let Some(rest) = line.strip_prefix("direction ") {
            let dir = rest.trim();
            direction = parse_direction(dir)
                .map_err(|_| MermaidError::InvalidDirection(dir.to_string()))?;
            continue;
        }

        if let Some(rest) = line.strip_prefix("class ") {
            let rest = rest.trim();
            if let Some(name_raw) = rest.strip_suffix('{') {
                let name = name_raw.trim();
                if name.is_empty() {
                    return Err(MermaidError::ParseError {
                        line: line_no,
                        message: "Expected class name before '{'".to_string(),
                    });
                }

                let mut members: Vec<String> = Vec::new();
                let mut stereotype: Option<String> = None;
                while i < lines.len() {
                    let member_raw = lines[i];
                    let member = member_raw.trim();
                    i += 1;

                    if member.is_empty() || member.starts_with("%%") {
                        continue;
                    }
                    if member == "}" {
                        break;
                    }
                    // Check for stereotypes like <<interface>>
                    if member.starts_with("<<") && member.ends_with(">>") {
                        stereotype = Some(member.to_string());
                        continue;
                    }
                    members.push(member.to_string());
                }

                let info = classes
                    .entry(name.to_string())
                    .or_insert_with(|| ClassInfo {
                        name: name.to_string(),
                        stereotype: None,
                        attributes: Vec::new(),
                        methods: Vec::new(),
                    });
                info.stereotype = stereotype;
                classify_members(&mut info.attributes, &mut info.methods, &members);
                continue;
            }

            if rest.is_empty() {
                return Err(MermaidError::ParseError {
                    line: line_no,
                    message: "Expected class name".to_string(),
                });
            }

            classes
                .entry(rest.to_string())
                .or_insert_with(|| ClassInfo {
                    name: rest.to_string(),
                    stereotype: None,
                    attributes: Vec::new(),
                    methods: Vec::new(),
                });
            continue;
        }

        let (head, label) = match line.split_once(':') {
            Some((a, b)) => {
                let label = b.trim();
                (
                    a.trim(),
                    if label.is_empty() {
                        None
                    } else {
                        Some(label.to_string())
                    },
                )
            }
            None => (line, None),
        };

        let parts: Vec<&str> = head.split_whitespace().collect();

        let op_pos = parts
            .iter()
            .position(|p| is_relationship_operator(p))
            .filter(|&pos| pos >= 1 && pos + 1 < parts.len());
        if let Some(pos) = op_pos {
            let quoted = |s: &str| s.len() >= 2 && s.starts_with('"') && s.ends_with('"');
            let unquote = |s: &str| s.trim_matches('"').to_string();
            let (from_idx, card_from) = if quoted(parts[pos - 1]) {
                if pos < 2 {
                    return Err(MermaidError::ParseError {
                        line: line_no,
                        message: format!("Unrecognized classDiagram line: {line}"),
                    });
                }
                (pos - 2, Some(unquote(parts[pos - 1])))
            } else {
                (pos - 1, None)
            };
            let (to_idx, card_to) = if quoted(parts[pos + 1]) {
                if pos + 2 >= parts.len() {
                    return Err(MermaidError::ParseError {
                        line: line_no,
                        message: format!("Unrecognized classDiagram line: {line}"),
                    });
                }
                (pos + 2, Some(unquote(parts[pos + 1])))
            } else {
                (pos + 1, None)
            };
            if from_idx != 0 || to_idx + 1 != parts.len() {
                return Err(MermaidError::ParseError {
                    line: line_no,
                    message: format!("Unrecognized classDiagram line: {line}"),
                });
            }
            let from = parts[from_idx].to_string();
            let to = parts[to_idx].to_string();
            let rel_type = classify_relationship(parts[pos]);

            let label = {
                let mut pieces: Vec<String> = Vec::new();
                if let Some(c) = card_from {
                    pieces.push(c);
                }
                if let Some(l) = label {
                    pieces.push(l);
                }
                if let Some(c) = card_to {
                    pieces.push(c);
                }
                if pieces.is_empty() {
                    None
                } else {
                    Some(pieces.join(" "))
                }
            };

            referenced.insert(from.clone());
            referenced.insert(to.clone());

            relationships.push(Relationship {
                from,
                to,
                label,
                rel_type,
            });
            continue;
        }

        if parts.len() == 1 {
            if let Some(member) = label {
                let info = classes
                    .entry(parts[0].to_string())
                    .or_insert_with(|| ClassInfo {
                        name: parts[0].to_string(),
                        stereotype: None,
                        attributes: Vec::new(),
                        methods: Vec::new(),
                    });
                let mut attrs = Vec::new();
                let mut methods = Vec::new();
                classify_members(&mut attrs, &mut methods, &[member]);
                info.attributes.extend(attrs);
                info.methods.extend(methods);
                continue;
            }
        }

        return Err(MermaidError::ParseError {
            line: line_no,
            message: format!("Unrecognized classDiagram line: {line}"),
        });
    }

    for id in referenced {
        classes.entry(id.clone()).or_insert_with(|| ClassInfo {
            name: id,
            stereotype: None,
            attributes: Vec::new(),
            methods: Vec::new(),
        });
    }

    Ok(ClassDiagram {
        direction,
        classes,
        relationships,
    })
}

fn parse_direction(dir: &str) -> Result<Direction, ()> {
    match dir.to_uppercase().as_str() {
        "TD" | "TB" => Ok(Direction::Tb),
        "BT" => Ok(Direction::Bt),
        "LR" => Ok(Direction::Lr),
        "RL" => Ok(Direction::Rl),
        _ => Err(()),
    }
}

fn classify_members(attrs: &mut Vec<String>, methods: &mut Vec<String>, members: &[String]) {
    for m in members {
        if m.contains('(') {
            methods.push(m.clone());
        } else {
            attrs.push(m.clone());
        }
    }
}

fn is_relationship_operator(op: &str) -> bool {
    matches!(
        op,
        "<|--"
            | "<|.."
            | "..|>"
            | "--|>"
            | "--"
            | "-->"
            | "<--"
            | ".."
            | "..>"
            | "<.."
            | "*--"
            | "o--"
            | "--*"
            | "--o"
            | "*.."
            | "o.."
            | "..*"
            | "..o"
    )
}

fn classify_relationship(op: &str) -> RelationType {
    match op {
        "<|--" | "--|>" => RelationType::Extension,
        "<|.." | "..|>" => RelationType::Realization,
        "*--" | "--*" => RelationType::Composition,
        "*.." | "..*" => RelationType::Composition,
        "o--" | "--o" => RelationType::Aggregation,
        "o.." | "..o" => RelationType::Aggregation,
        "-->" | "<--" => RelationType::Dependency,
        "..>" | "<.." => RelationType::DashedDep,
        ".." => RelationType::DashedAssoc,
        "--" => RelationType::Association,
        _ => RelationType::Association,
    }
}

// --- Layout computation ---

fn compute_class_box_size(info: &ClassInfo) -> (f64, f64, f64, f64, f64, f64, f64, Option<f64>) {
    // Returns (width, height, title_y, attr_divider_y, method_divider_y,
    //          attr_start_y, method_start_y, annotation_y)
    //
    // Ported from Mermaid 11.12.2 shapeUtil.ts (textHelper) + classBox.ts:
    //   - textHelper accumulates group positions with GAP*2 / GAP*4 spacing
    //   - classBox adds PADDING on all sides of the bbox

    let mut max_text_width: f64 = 0.0;

    // Title (class name) — uses larger font
    let title_width =
        line_width(&info.name, DEFAULT_CHAR_WIDTH) * (TITLE_FONT_SIZE / MEMBER_FONT_SIZE);
    max_text_width = max_text_width.max(title_width);

    // Stereotype annotation
    let annotation_height = if info.stereotype.is_some() {
        LINE_HEIGHT
    } else {
        0.0
    };
    if let Some(ref s) = info.stereotype {
        let guillemet_text = stereotype_to_guillemet(s);
        let w = line_width(&guillemet_text, DEFAULT_CHAR_WIDTH);
        max_text_width = max_text_width.max(w);
    }

    for attr in &info.attributes {
        let w = line_width(attr, DEFAULT_CHAR_WIDTH);
        max_text_width = max_text_width.max(w);
    }

    for method in &info.methods {
        let w = line_width(method, DEFAULT_CHAR_WIDTH);
        max_text_width = max_text_width.max(w);
    }

    // Width: max text width + 2 * PADDING (outer padding on each side)
    let box_width = max_text_width + PADDING * 2.0;

    // Compute internal content heights (matching textHelper group positioning).
    // Members are positioned at: annotationH + labelH + GAP*2
    // Methods are positioned at: annotationH + labelH + (membersH ? membersH + GAP*4 : GAP*2)
    let label_height = LINE_HEIGHT; // title text bounding box
    let members_height = if info.attributes.is_empty() {
        GAP / 2.0 // mermaid: membersGroupHeight = GAP/2 when empty
    } else {
        info.attributes.len() as f64 * (LINE_HEIGHT + TEXT_PADDING)
    };
    let methods_height = if info.methods.is_empty() {
        GAP / 2.0
    } else {
        info.methods.len() as f64 * (LINE_HEIGHT + TEXT_PADDING)
    };

    // Total bbox height from textHelper (before classBox adds PADDING)
    let methods_y = annotation_height
        + label_height
        + if !info.attributes.is_empty() {
            members_height + GAP * 4.0
        } else {
            GAP * 2.0
        };
    let content_height = methods_y + methods_height;

    // classBox adds: GAP when members=0 && methods=0, GAP*2 when members>0 && methods=0
    let extra_h = if info.attributes.is_empty() && info.methods.is_empty() {
        GAP
    } else if !info.attributes.is_empty() && info.methods.is_empty() {
        GAP * 2.0
    } else {
        0.0
    };

    // Final box height: content + extra + 2 * PADDING (outer padding top/bottom)
    let box_height = content_height + extra_h + PADDING * 2.0;
    let half_h = box_height / 2.0;

    // Position elements relative to box center (matching classBox's translate logic).
    // In classBox, the text groups start at y = -h/2 + PADDING (top of content area)
    let content_top = -half_h + PADDING;

    let annotation_y = if info.stereotype.is_some() {
        Some(content_top + LINE_HEIGHT / 2.0)
    } else {
        None
    };

    let title_y = content_top + annotation_height + label_height / 2.0;

    // Divider positions ported from classBox.ts lines 44377-44397:
    //   First divider at:  annotH + labelH + (-h/2) + PADDING  (in box-local coords)
    //   Second divider at: annotH + labelH + membersH + (-h/2) + GAP*2 + PADDING
    // Since content_top = -half_h + PADDING, divider1 = content_top + annotH + labelH.
    let attr_divider_y = content_top + annotation_height + label_height;

    // Second divider: members_height + GAP*2 below first divider
    let method_divider_y = attr_divider_y + members_height + GAP * 2.0;

    let attr_start_y = attr_divider_y + GAP + LINE_HEIGHT / 2.0;
    let method_start_y = method_divider_y + GAP + LINE_HEIGHT / 2.0;

    (
        box_width,
        box_height,
        title_y,
        attr_divider_y,
        method_divider_y,
        attr_start_y,
        method_start_y,
        annotation_y,
    )
}

fn compute_layout(diagram: &ClassDiagram) -> DiagramLayout {
    type ClassBoxMetrics = (f64, f64, f64, f64, f64, f64, f64, Option<f64>);
    let mut node_sizes: BTreeMap<String, ClassBoxMetrics> = BTreeMap::new();
    for (id, info) in &diagram.classes {
        let size = compute_class_box_size(info);
        node_sizes.insert(id.clone(), size);
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

    for (id, (w, h, ..)) in &node_sizes {
        g.set_node(
            id.clone(),
            Some(GraphNode {
                width: *w as f32,
                height: *h as f32,
                ..Default::default()
            }),
        );
    }

    let mut edge_keys: Vec<(String, String, usize)> = Vec::new();
    for (idx, rel) in diagram.relationships.iter().enumerate() {
        let mut edge_label = GraphEdge::default();
        if let Some(ref text) = rel.label {
            let label_w = line_width(text, DEFAULT_CHAR_WIDTH);
            edge_label.width = Some(label_w as f32);
            edge_label.height = Some(LINE_HEIGHT as f32);
            edge_label.labelpos = Some("c".to_string());
        }
        let _ = g.set_edge(&rel.from, &rel.to, Some(edge_label), None);
        edge_keys.push((rel.from.clone(), rel.to.clone(), idx));
    }

    dagre_layout(&mut g);

    let mut node_positions: HashMap<String, (f64, f64)> = HashMap::new();
    for node_id in g.nodes() {
        if let Some(node) = g.node(&node_id) {
            node_positions.insert(node_id, (node.x as f64, node.y as f64));
        }
    }

    let mut nodes: Vec<ClassNodeLayout> = Vec::new();
    for (id, info) in &diagram.classes {
        let (w, h, title_y, attr_div_y, meth_div_y, attr_start_y, meth_start_y, annotation_y) =
            node_sizes[id];
        let (cx, cy) = node_positions.get(id).copied().unwrap_or((0.0, 0.0));
        nodes.push(ClassNodeLayout {
            id: id.clone(),
            x: cx,
            y: cy,
            width: w,
            height: h,
            info: info.clone(),
            title_y,
            annotation_y,
            attr_divider_y: attr_div_y,
            method_divider_y: meth_div_y,
            attr_start_y,
            method_start_y: meth_start_y,
        });
    }

    let mut edges: Vec<EdgeLayout> = Vec::new();
    for (from, to, idx) in &edge_keys {
        let Some(edge) = g.edge(from, to, None) else {
            continue;
        };
        let points: Vec<(f64, f64)> = edge
            .points
            .as_ref()
            .map(|pts| pts.iter().map(|p| (p.x as f64, p.y as f64)).collect())
            .unwrap_or_default();

        let rel = &diagram.relationships[*idx];
        let label_pos = if rel.label.is_some() {
            Some((edge.x as f64, edge.y as f64))
        } else {
            None
        };
        let label_width = rel
            .label
            .as_ref()
            .map(|t| line_width(t, DEFAULT_CHAR_WIDTH))
            .unwrap_or(0.0);

        // Look up source/target node geometry for edge clipping
        let (src_cx, src_cy) = node_positions.get(from).copied().unwrap_or((0.0, 0.0));
        let (src_w, src_h, ..) = node_sizes.get(from).copied().unwrap_or_default();
        let (tgt_cx, tgt_cy) = node_positions.get(to).copied().unwrap_or((0.0, 0.0));
        let (tgt_w, tgt_h, ..) = node_sizes.get(to).copied().unwrap_or_default();

        edges.push(EdgeLayout {
            rel_type: rel.rel_type,
            label: rel.label.clone(),
            points,
            label_pos,
            label_width,
            src_cx,
            src_cy,
            src_hw: src_w / 2.0,
            src_hh: src_h / 2.0,
            tgt_cx,
            tgt_cy,
            tgt_hw: tgt_w / 2.0,
            tgt_hh: tgt_h / 2.0,
        });
    }

    let mut min_x = f64::MAX;
    let mut max_x = f64::MIN;
    let mut min_y = f64::MAX;
    let mut max_y = f64::MIN;

    for n in &nodes {
        min_x = min_x.min(n.x - n.width / 2.0);
        max_x = max_x.max(n.x + n.width / 2.0);
        min_y = min_y.min(n.y - n.height / 2.0);
        max_y = max_y.max(n.y + n.height / 2.0);
    }

    let width = (max_x - min_x) + GRAPH_MARGIN * 2.0;
    let height = (max_y - min_y) + GRAPH_MARGIN * 2.0;

    DiagramLayout {
        nodes,
        edges,
        width: width.max(100.0),
        height: height.max(100.0),
    }
}

// --- SVG rendering ---

fn render_svg(layout: &DiagramLayout, theme: &MermaidTheme) -> String {
    let node_fill = &theme.node_fill;
    let node_stroke = &theme.node_stroke;
    let edge_color = &theme.edge_color;
    let text_color = &theme.text_color;

    let fill = if node_fill.is_empty() {
        NODE_FILL
    } else {
        node_fill
    };
    let stroke = if node_stroke.is_empty() {
        NODE_STROKE
    } else {
        node_stroke
    };
    let edge_col = if edge_color.is_empty() {
        EDGE_COLOR
    } else {
        edge_color
    };
    let txt_col = if text_color.is_empty() {
        TEXT_COLOR
    } else {
        text_color
    };

    let w = layout.width;
    let h = layout.height;

    let mut svg = String::new();
    svg.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" class=\"classDiagram\" \
         viewBox=\"0 0 {w:.0} {h:.0}\" \
         role=\"graphics-document document\" aria-roledescription=\"class\">\n"
    ));

    svg.push_str(&format!(
        "<style>\n\
         svg {{ font-family: \"trebuchet ms\", verdana, arial, sans-serif; font-size: 16px; fill: {txt_col}; }}\n\
         .classTitle {{ font-weight: bolder; font-size: {TITLE_FONT_SIZE}px; }}\n\
         .classMember {{ font-size: {MEMBER_FONT_SIZE}px; }}\n\
         .classAnnotation {{ font-size: {MEMBER_FONT_SIZE}px; }}\n\
         </style>\n"
    ));

    svg.push_str(&render_marker_defs(fill, edge_col));

    for node in &layout.nodes {
        render_class_node(&mut svg, node, fill, stroke, txt_col);
    }

    for edge in &layout.edges {
        render_edge(&mut svg, edge, edge_col, fill, txt_col);
    }

    svg.push_str("</svg>\n");
    svg
}

fn render_marker_defs(fill: &str, edge_color: &str) -> String {
    let mut defs = String::new();
    defs.push_str("<defs>\n");

    // Extension markers (open/hollow triangle) — for inheritance
    defs.push_str(&format!(
        "  <marker id=\"extensionStart\" class=\"marker extension\" refX=\"18\" refY=\"7\" \
         markerWidth=\"190\" markerHeight=\"240\" orient=\"auto\">\
         <path d=\"M 1,7 L18,13 V 1 Z\" fill=\"transparent\" stroke=\"{edge_color}\" stroke-width=\"1\"/></marker>\n"
    ));
    defs.push_str(&format!(
        "  <marker id=\"extensionEnd\" class=\"marker extension\" refX=\"1\" refY=\"7\" \
         markerWidth=\"20\" markerHeight=\"28\" orient=\"auto\">\
         <path d=\"M 1,1 V 13 L18,7 Z\" fill=\"transparent\" stroke=\"{edge_color}\" stroke-width=\"1\"/></marker>\n"
    ));

    // Dependency markers (filled arrowhead)
    defs.push_str(&format!(
        "  <marker id=\"dependencyEnd\" class=\"marker dependency\" refX=\"13\" refY=\"7\" \
         markerWidth=\"20\" markerHeight=\"28\" orient=\"auto\">\
         <path d=\"M 18,7 L9,13 L14,7 L9,1 Z\" fill=\"{edge_color}\" stroke=\"{edge_color}\" stroke-width=\"1\"/></marker>\n"
    ));
    defs.push_str(&format!(
        "  <marker id=\"dependencyStart\" class=\"marker dependency\" refX=\"6\" refY=\"7\" \
         markerWidth=\"190\" markerHeight=\"240\" orient=\"auto\">\
         <path d=\"M 5,7 L9,13 L1,7 L9,1 Z\" fill=\"{edge_color}\" stroke=\"{edge_color}\" stroke-width=\"1\"/></marker>\n"
    ));

    // Composition markers (filled diamond)
    defs.push_str(&format!(
        "  <marker id=\"compositionStart\" class=\"marker composition\" refX=\"18\" refY=\"7\" \
         markerWidth=\"190\" markerHeight=\"240\" orient=\"auto\">\
         <path d=\"M 18,7 L9,13 L1,7 L9,1 Z\" fill=\"{edge_color}\" stroke=\"{edge_color}\" stroke-width=\"1\"/></marker>\n"
    ));
    defs.push_str(&format!(
        "  <marker id=\"compositionEnd\" class=\"marker composition\" refX=\"1\" refY=\"7\" \
         markerWidth=\"20\" markerHeight=\"28\" orient=\"auto\">\
         <path d=\"M 18,7 L9,13 L1,7 L9,1 Z\" fill=\"{edge_color}\" stroke=\"{edge_color}\" stroke-width=\"1\"/></marker>\n"
    ));

    // Aggregation markers (open diamond)
    defs.push_str(&format!(
        "  <marker id=\"aggregationStart\" class=\"marker aggregation\" refX=\"18\" refY=\"7\" \
         markerWidth=\"190\" markerHeight=\"240\" orient=\"auto\">\
         <path d=\"M 18,7 L9,13 L1,7 L9,1 Z\" fill=\"transparent\" stroke=\"{edge_color}\" stroke-width=\"1\"/></marker>\n"
    ));
    defs.push_str(&format!(
        "  <marker id=\"aggregationEnd\" class=\"marker aggregation\" refX=\"1\" refY=\"7\" \
         markerWidth=\"20\" markerHeight=\"28\" orient=\"auto\">\
         <path d=\"M 18,7 L9,13 L1,7 L9,1 Z\" fill=\"transparent\" stroke=\"{edge_color}\" stroke-width=\"1\"/></marker>\n"
    ));

    // Lollipop markers (circle)
    defs.push_str(&format!(
        "  <marker id=\"lollipopStart\" class=\"marker lollipop\" refX=\"13\" refY=\"7\" \
         markerWidth=\"190\" markerHeight=\"240\" orient=\"auto\">\
         <circle cx=\"7\" cy=\"7\" r=\"6\" fill=\"{fill}\" stroke=\"{edge_color}\" stroke-width=\"1\"/></marker>\n"
    ));

    defs.push_str("</defs>\n");
    defs
}

fn render_class_node(
    svg: &mut String,
    node: &ClassNodeLayout,
    fill: &str,
    stroke: &str,
    text_color: &str,
) {
    let w = node.width;
    let h = node.height;

    svg.push_str(&format!(
        "<g class=\"node default\" id=\"classId-{id}\" transform=\"translate({cx},{cy})\">\n",
        id = xml_escape(&node.id),
        cx = node.x,
        cy = node.y
    ));

    let hw = w / 2.0;
    let hh = h / 2.0;

    // Background fill rect
    svg.push_str(&format!(
        "  <rect x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" \
         fill=\"{fill}\" stroke=\"none\"/>\n",
        x = -hw,
        y = -hh,
    ));

    // Border
    svg.push_str(&format!(
        "  <rect x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" \
         fill=\"none\" stroke=\"{stroke}\" stroke-width=\"{NODE_STROKE_WIDTH}\"/>\n",
        x = -hw,
        y = -hh,
    ));

    // Stereotype annotation (if present)
    if let (Some(annotation_y), Some(ref stereo)) = (node.annotation_y, &node.info.stereotype) {
        let guillemet = stereotype_to_guillemet(stereo);
        svg.push_str(&format!(
            "  <text x=\"0\" y=\"{y}\" text-anchor=\"middle\" dominant-baseline=\"central\" \
             class=\"classAnnotation\" fill=\"{text_color}\">{text}</text>\n",
            y = annotation_y,
            text = xml_escape(&guillemet),
        ));
    }

    // Class name (bold, centered)
    svg.push_str(&format!(
        "  <text x=\"0\" y=\"{y}\" text-anchor=\"middle\" dominant-baseline=\"central\" \
         class=\"classTitle\" fill=\"{text_color}\">{text}</text>\n",
        y = node.title_y,
        text = xml_escape(&node.info.name),
    ));

    // Attribute divider line
    svg.push_str(&format!(
        "  <line x1=\"{x1}\" y1=\"{y}\" x2=\"{x2}\" y2=\"{y}\" \
         stroke=\"{stroke}\" stroke-width=\"{NODE_STROKE_WIDTH}\"/>\n",
        x1 = -hw,
        x2 = hw,
        y = node.attr_divider_y,
    ));

    // Attributes (left-aligned)
    for (idx, attr) in node.info.attributes.iter().enumerate() {
        let ay = node.attr_start_y + idx as f64 * (LINE_HEIGHT + TEXT_PADDING);
        svg.push_str(&format!(
            "  <text x=\"{x}\" y=\"{y}\" text-anchor=\"start\" dominant-baseline=\"central\" \
             class=\"classMember\" fill=\"{text_color}\">{text}</text>\n",
            x = -hw + PADDING,
            y = ay,
            text = xml_escape(attr),
        ));
    }

    // Method divider line
    svg.push_str(&format!(
        "  <line x1=\"{x1}\" y1=\"{y}\" x2=\"{x2}\" y2=\"{y}\" \
         stroke=\"{stroke}\" stroke-width=\"{NODE_STROKE_WIDTH}\"/>\n",
        x1 = -hw,
        x2 = hw,
        y = node.method_divider_y,
    ));

    // Methods (left-aligned)
    for (idx, method) in node.info.methods.iter().enumerate() {
        let my = node.method_start_y + idx as f64 * (LINE_HEIGHT + TEXT_PADDING);
        svg.push_str(&format!(
            "  <text x=\"{x}\" y=\"{y}\" text-anchor=\"start\" dominant-baseline=\"central\" \
             class=\"classMember\" fill=\"{text_color}\">{text}</text>\n",
            x = -hw + PADDING,
            y = my,
            text = xml_escape(method),
        ));
    }

    svg.push_str("</g>\n");
}

/// Compute where a ray from `inside` toward `outside` intersects a rectangle
/// centered at (`cx`, `cy`) with given half-width and half-height.
/// Returns the intersection point on the rectangle boundary.
fn intersect_rect(cx: f64, cy: f64, hw: f64, hh: f64, target_x: f64, target_y: f64) -> (f64, f64) {
    let dx = target_x - cx;
    let dy = target_y - cy;
    if dx.abs() < 1e-6 && dy.abs() < 1e-6 {
        return (cx, cy - hh);
    }
    // Determine which edge the ray hits first
    let t_right = if dx > 0.0 { hw / dx } else { f64::INFINITY };
    let t_left = if dx < 0.0 { -hw / dx } else { f64::INFINITY };
    let t_bottom = if dy > 0.0 { hh / dy } else { f64::INFINITY };
    let t_top = if dy < 0.0 { -hh / dy } else { f64::INFINITY };
    let t = t_right.min(t_left).min(t_bottom).min(t_top).max(0.0);
    (cx + dx * t, cy + dy * t)
}

/// Clip edge start/end to node rectangle boundaries, then offset for markers.
/// dagre_rust may return edge points that don't land exactly on node borders,
/// so we compute the intersection ourselves (matching mermaid.js behavior).
#[allow(clippy::too_many_arguments)]
fn clip_and_offset_edge(
    points: &[(f64, f64)],
    rel_type: RelationType,
    src_cx: f64,
    src_cy: f64,
    src_hw: f64,
    src_hh: f64,
    tgt_cx: f64,
    tgt_cy: f64,
    tgt_hw: f64,
    tgt_hh: f64,
) -> Vec<(f64, f64)> {
    if points.len() < 2 {
        return points.to_vec();
    }
    let mut out = points.to_vec();

    // Clip start point to source node boundary
    let next = out[1];
    let border_start = intersect_rect(src_cx, src_cy, src_hw, src_hh, next.0, next.1);
    out[0] = border_start;

    // Clip end point to target node boundary
    let n = out.len();
    let prev = out[n - 2];
    let border_end = intersect_rect(tgt_cx, tgt_cy, tgt_hw, tgt_hh, prev.0, prev.1);
    out[n - 1] = border_end;

    // Offset start for start markers (pull point away from source toward target).
    // Use the direction from the clipped border point toward the last edge point
    // to avoid short-segment capping issues with intermediate dagre points.
    if rel_type.start_marker().is_some() {
        let offset = start_marker_offset(rel_type);
        let (x0, y0) = out[0];
        let last = out[out.len() - 1];
        let dx = last.0 - x0;
        let dy = last.1 - y0;
        let len = (dx * dx + dy * dy).sqrt();
        if len > 0.0 {
            out[0] = (x0 + dx / len * offset, y0 + dy / len * offset);
        }
    }

    // Offset end for end markers (pull point away from target toward source).
    if rel_type.end_marker().is_some() {
        let offset = end_marker_offset(rel_type);
        let n = out.len();
        let (xn, yn) = out[n - 1];
        let first = out[0];
        let dx = first.0 - xn;
        let dy = first.1 - yn;
        let len = (dx * dx + dy * dy).sqrt();
        if len > 0.0 {
            out[n - 1] = (xn + dx / len * offset, yn + dy / len * offset);
        }
    }

    out
}

fn start_marker_offset(rel_type: RelationType) -> f64 {
    match rel_type {
        RelationType::Extension | RelationType::Realization => EXTENSION_MARKER_OFFSET,
        RelationType::Composition => COMPOSITION_MARKER_OFFSET,
        RelationType::Aggregation => AGGREGATION_MARKER_OFFSET,
        _ => 0.0,
    }
}

fn end_marker_offset(rel_type: RelationType) -> f64 {
    match rel_type {
        RelationType::Dependency | RelationType::DashedDep => DEPENDENCY_MARKER_OFFSET,
        _ => 0.0,
    }
}

fn render_edge(
    svg: &mut String,
    edge: &EdgeLayout,
    edge_color: &str,
    fill: &str,
    text_color: &str,
) {
    if edge.points.is_empty() {
        return;
    }

    let dash = if edge.rel_type.is_dashed() {
        " stroke-dasharray=\"3\""
    } else {
        ""
    };

    // Clip edge endpoints to source/target node boundaries, then offset for markers.
    let points = clip_and_offset_edge(
        &edge.points,
        edge.rel_type,
        edge.src_cx,
        edge.src_cy,
        edge.src_hw,
        edge.src_hh,
        edge.tgt_cx,
        edge.tgt_cy,
        edge.tgt_hw,
        edge.tgt_hh,
    );

    let mut path = String::new();
    for (i, (x, y)) in points.iter().enumerate() {
        if i == 0 {
            path.push_str(&format!("M{x:.1},{y:.1}"));
        } else {
            path.push_str(&format!("L{x:.1},{y:.1}"));
        }
    }

    let mut marker_start = String::new();
    let mut marker_end = String::new();
    if let Some(m) = edge.rel_type.start_marker() {
        marker_start = format!(" marker-start=\"url(#{m})\"");
    }
    if let Some(m) = edge.rel_type.end_marker() {
        marker_end = format!(" marker-end=\"url(#{m})\"");
    }

    svg.push_str(&format!(
        "<path d=\"{path}\" class=\"relation\" \
         stroke=\"{edge_color}\" stroke-width=\"1\" fill=\"none\"{dash}{marker_start}{marker_end}/>\n"
    ));

    // Edge label
    if let (Some(ref text), Some((lx, ly))) = (&edge.label, edge.label_pos) {
        let lw = edge.label_width;
        let lh = LINE_HEIGHT;
        svg.push_str(&format!(
            "<rect x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" \
             fill=\"{fill}\" stroke=\"none\" opacity=\"0.75\"/>\n",
            x = lx - lw / 2.0 - 4.0,
            y = ly - lh / 2.0,
            w = lw + 8.0,
            h = lh,
        ));
        svg.push_str(&format!(
            "<text x=\"{x}\" y=\"{y}\" text-anchor=\"middle\" dominant-baseline=\"central\" \
             fill=\"{text_color}\">{text}</text>\n",
            x = lx,
            y = ly,
            text = xml_escape(text),
        ));
    }
}

fn stereotype_to_guillemet(s: &str) -> String {
    let inner = s
        .strip_prefix("<<")
        .and_then(|s| s.strip_suffix(">>"))
        .unwrap_or(s);
    format!("\u{00AB}{inner}\u{00BB}")
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
