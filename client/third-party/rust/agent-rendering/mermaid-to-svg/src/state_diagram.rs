use crate::ast::{Edge, EdgeStyle, FlowchartGraph, GraphDirection, Node, NodeShape, Statement};
use crate::error::MermaidError;

use std::collections::BTreeMap;

pub fn parse_state_diagram(input: &str) -> Result<FlowchartGraph, MermaidError> {
    let lines: Vec<&str> = input.lines().collect();

    let mut i = 0_usize;
    let mut header: Option<&str> = None;
    while i < lines.len() {
        let line = lines[i].trim();
        if line.is_empty() || line.starts_with("%%") {
            i += 1;
            continue;
        }

        let token = line.split_whitespace().next().unwrap_or("");
        if token == "stateDiagram" || token == "stateDiagram-v2" {
            header = Some(token);
            i += 1;
            break;
        }

        return Err(MermaidError::ParseError {
            line: i + 1,
            message: "Expected 'stateDiagram' or 'stateDiagram-v2' declaration".to_string(),
        });
    }

    if header.is_none() {
        return Err(MermaidError::ParseError {
            line: 1,
            message: "Expected 'stateDiagram' or 'stateDiagram-v2' declaration".to_string(),
        });
    }

    let mut nodes: BTreeMap<String, NodeShape> = BTreeMap::new();
    let mut node_order: Vec<String> = Vec::new();
    let mut edges: Vec<(String, String, Option<String>)> = Vec::new();

    while i < lines.len() {
        let raw = lines[i];
        let line = raw.trim();
        let line_no = i + 1;
        i += 1;

        if line.is_empty() || line.starts_with("%%") {
            continue;
        }

        if let Some(rest) = line.strip_prefix("state ") {
            let rest = rest.trim();
            if rest.is_empty() {
                return Err(MermaidError::ParseError {
                    line: line_no,
                    message: "Expected state name after 'state'".to_string(),
                });
            }

            let name = rest.split_whitespace().next().unwrap_or("").to_string();
            let shape = if rest.contains("<<choice>>") {
                NodeShape::Diamond
            } else if rest.contains("<<fork>>") || rest.contains("<<join>>") {
                NodeShape::ForkJoin
            } else {
                NodeShape::RoundedRectangle
            };

            if !nodes.contains_key(&name) {
                node_order.push(name.clone());
            }
            nodes.insert(name, shape);
            continue;
        }

        if let Some((from_raw, rhs)) = line.split_once("-->") {
            let from_raw = from_raw.trim();
            let rhs = rhs.trim();

            let (to_raw, label) = match rhs.split_once(':') {
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
                None => (rhs, None),
            };

            let from = normalize_state_id(from_raw, true);
            let to = normalize_state_id(to_raw, false);

            ensure_state_node(&mut nodes, &mut node_order, &from);
            ensure_state_node(&mut nodes, &mut node_order, &to);
            edges.push((from, to, label));
            continue;
        }

        return Err(MermaidError::ParseError {
            line: line_no,
            message: format!("Unrecognized stateDiagram line: {line}"),
        });
    }

    let mut statements: Vec<Statement> = Vec::new();
    for id in node_order {
        let Some(shape) = nodes.get(&id) else {
            continue;
        };
        let label = match shape {
            NodeShape::StartState | NodeShape::EndState | NodeShape::ForkJoin => None,
            _ => Some(id.clone()),
        };

        statements.push(Statement::Node(Node {
            id: id.clone(),
            label,
            shape: *shape,
        }));
    }

    for (from, to, label) in edges {
        statements.push(Statement::Edge(Edge {
            from,
            to,
            label,
            style: EdgeStyle::Arrow,
        }));
    }

    Ok(FlowchartGraph {
        direction: GraphDirection::TopToBottom,
        statements,
    })
}

fn normalize_state_id(raw: &str, is_from: bool) -> String {
    let raw = raw.trim();
    if raw == "[*]" {
        if is_from {
            "__start".to_string()
        } else {
            "__end".to_string()
        }
    } else {
        raw.to_string()
    }
}

fn ensure_state_node(
    nodes: &mut BTreeMap<String, NodeShape>,
    node_order: &mut Vec<String>,
    id: &str,
) {
    if nodes.contains_key(id) {
        return;
    }

    node_order.push(id.to_string());
    let shape = match id {
        "__start" => NodeShape::StartState,
        "__end" => NodeShape::EndState,
        _ => NodeShape::RoundedRectangle,
    };
    nodes.insert(id.to_string(), shape);
}
