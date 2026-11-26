use std::collections::HashMap;

use crate::ast::{EdgeStyle, FlowchartGraph, GraphDirection, NodeShape, Statement};

#[derive(Debug, Clone)]
pub struct FlowDb {
    pub direction: GraphDirection,
    pub vertices: HashMap<String, FlowVertex>,
    pub vertex_order: Vec<String>,
    pub edges: Vec<FlowEdge>,
    pub subgraphs: Vec<FlowSubgraph>,
    pub node_to_subgraph: HashMap<String, String>,
    pub node_styles: HashMap<String, Vec<(String, String)>>,
}

#[derive(Debug, Clone)]
pub struct FlowVertex {
    pub id: String,
    pub label: String,
    pub shape: NodeShape,
}

#[derive(Debug, Clone)]
pub struct FlowEdge {
    pub start: String,
    pub end: String,
    pub label: Option<String>,
    pub style: EdgeStyle,
}

#[derive(Debug, Clone)]
pub struct FlowSubgraph {
    pub id: String,
    pub title: Option<String>,
    pub parent_id: Option<String>,
}

pub fn from_flowchart_graph(graph: &FlowchartGraph) -> FlowDb {
    let mut db = FlowDb {
        direction: graph.direction,
        vertices: HashMap::new(),
        vertex_order: Vec::new(),
        edges: Vec::new(),
        subgraphs: Vec::new(),
        node_to_subgraph: HashMap::new(),
        node_styles: HashMap::new(),
    };

    collect_statements(&mut db, &graph.statements, None);
    db
}

fn collect_statements(db: &mut FlowDb, statements: &[Statement], current_subgraph: Option<&str>) {
    for stmt in statements {
        match stmt {
            Statement::Node(node) => {
                ensure_vertex(db, &node.id, node.label.as_deref(), node.shape);
                maybe_assign_to_subgraph(db, &node.id, current_subgraph);
            }
            Statement::Edge(edge) => {
                ensure_vertex(db, &edge.from, None, NodeShape::Rectangle);
                ensure_vertex(db, &edge.to, None, NodeShape::Rectangle);
                maybe_assign_to_subgraph(db, &edge.from, current_subgraph);
                maybe_assign_to_subgraph(db, &edge.to, current_subgraph);

                db.edges.push(FlowEdge {
                    start: edge.from.clone(),
                    end: edge.to.clone(),
                    label: edge.label.clone(),
                    style: edge.style,
                });
            }
            Statement::Subgraph(subgraph) => {
                collect_statements(db, &subgraph.statements, Some(&subgraph.id));

                db.subgraphs.push(FlowSubgraph {
                    id: subgraph.id.clone(),
                    title: subgraph.title.clone().or_else(|| Some(subgraph.id.clone())),
                    parent_id: current_subgraph.map(|s| s.to_string()),
                });
            }
            Statement::Style(style) => {
                ensure_vertex(db, &style.node_id, None, NodeShape::Rectangle);
                maybe_assign_to_subgraph(db, &style.node_id, current_subgraph);
                db.node_styles
                    .entry(style.node_id.clone())
                    .or_default()
                    .extend(style.properties.iter().cloned());
            }
        }
    }
}

fn ensure_vertex(db: &mut FlowDb, id: &str, label: Option<&str>, shape: NodeShape) {
    if is_subgraph_id(db, id) {
        return;
    }
    let id = id.to_string();

    match db.vertices.get_mut(&id) {
        Some(v) => {
            if let Some(label) = label {
                v.label = label.to_string();
                v.shape = shape;
            }
        }
        None => {
            let label = label.unwrap_or(id.as_str()).to_string();
            db.vertex_order.push(id.clone());
            db.vertices
                .insert(id.clone(), FlowVertex { id, label, shape });
        }
    }
}

fn maybe_assign_to_subgraph(db: &mut FlowDb, node_id: &str, current_subgraph: Option<&str>) {
    if is_subgraph_id(db, node_id) {
        return;
    }
    let Some(subgraph_id) = current_subgraph else {
        return;
    };

    db.node_to_subgraph
        .entry(node_id.to_string())
        .or_insert_with(|| subgraph_id.to_string());
}

fn is_subgraph_id(db: &FlowDb, id: &str) -> bool {
    db.subgraphs.iter().any(|subgraph| subgraph.id == id)
}
