use crate::ast::{EdgeStyle, NodeShape};

use super::flow_db::{FlowDb, FlowEdge, FlowVertex};

#[derive(Debug, Clone)]
pub struct FlowData {
    pub nodes: Vec<FlowDataNode>,
    pub edges: Vec<FlowDataEdge>,
}

#[derive(Debug, Clone)]
pub struct FlowDataNode {
    pub id: String,
    pub label: String,
    pub shape: NodeShape,
    pub parent_id: Option<String>,
    pub styles: Vec<(String, String)>,
    pub is_group: bool,
}

#[derive(Debug, Clone)]
pub struct FlowDataEdge {
    pub start: String,
    pub end: String,
    pub label: Option<String>,
    pub style: EdgeStyle,
}

pub fn get_data(db: &FlowDb) -> FlowData {
    let mut nodes: Vec<FlowDataNode> = Vec::new();

    for sg in db.subgraphs.iter().rev() {
        nodes.push(FlowDataNode {
            id: sg.id.clone(),
            label: sg.title.clone().unwrap_or_else(|| sg.id.clone()),
            shape: NodeShape::Rectangle,
            parent_id: sg.parent_id.clone(),
            styles: Vec::new(),
            is_group: true,
        });
    }

    for id in &db.vertex_order {
        if let Some(v) = db.vertices.get(id) {
            nodes.push(make_node_data(db, v));
        }
    }

    let edges: Vec<FlowDataEdge> = db.edges.iter().map(make_edge_data).collect();

    FlowData { nodes, edges }
}

fn make_node_data(db: &FlowDb, v: &FlowVertex) -> FlowDataNode {
    let parent_id = db.node_to_subgraph.get(&v.id).cloned();
    let styles = db.node_styles.get(&v.id).cloned().unwrap_or_default();

    FlowDataNode {
        id: v.id.clone(),
        label: v.label.clone(),
        shape: v.shape,
        parent_id,
        styles,
        is_group: false,
    }
}

fn make_edge_data(e: &FlowEdge) -> FlowDataEdge {
    FlowDataEdge {
        start: e.start.clone(),
        end: e.end.clone(),
        label: e.label.clone(),
        style: e.style,
    }
}
