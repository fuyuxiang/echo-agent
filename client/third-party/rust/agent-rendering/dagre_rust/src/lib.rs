pub mod layout;

use crate::layout::add_border_segments::BorderTypeName;
use graphlib_rust::Edge;
use ordered_hashmap::OrderedHashMap;

#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct GraphNode {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub class: Option<String>,
    pub label: Option<GraphEdge>,
    pub padding: Option<f32>,
    pub padding_x: Option<f32>,
    pub padding_y: Option<f32>,
    pub rx: Option<f32>,
    pub ry: Option<f32>,
    pub shape: Option<String>,
    pub dummy: Option<String>,
    pub rank: Option<i32>,
    pub min_rank: Option<i32>,
    pub max_rank: Option<i32>,
    pub order: Option<usize>,
    pub border_top: Option<String>,
    pub border_bottom: Option<String>,
    pub border_left: Option<OrderedHashMap<i32, String>>,
    pub border_right: Option<OrderedHashMap<i32, String>>,
    pub border_left_: Option<String>,
    pub border_right_: Option<String>,
    pub low: Option<usize>,
    pub lim: Option<usize>,
    pub parent: Option<String>,
    pub e: Option<Edge>,
    pub edge_label: Option<GraphEdge>,
    pub edge_obj: Option<Edge>,
    pub labelpos: Option<String>,
    pub border_type: Option<BorderTypeName>,
    pub self_edges: Vec<(Edge, GraphEdge)>,
}

#[derive(Debug, Clone, Default)]
pub struct GraphEdgePoint {
    pub x: f32,
    pub y: f32,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct GraphEdge {
    pub forward_name: Option<String>,
    pub reversed: Option<bool>,
    pub minlen: Option<f32>,
    pub weight: Option<f32>,
    pub width: Option<f32>,
    pub height: Option<f32>,
    pub label_rank: Option<i32>,
    pub labeloffset: Option<f32>,
    pub labelpos: Option<String>,
    pub nesting_edge: Option<bool>,
    pub cutvalue: Option<f32>,
    pub points: Option<Vec<GraphEdgePoint>>,
    pub x: f32,
    pub y: f32,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct GraphConfig {
    pub width: f32,
    pub height: f32,

    pub nodesep: Option<f32>,      // default 50
    pub edgesep: Option<f32>,      // default 20
    pub ranksep: Option<f32>,      // default 50
    pub marginx: Option<f32>,      // default 0
    pub marginy: Option<f32>,      // default 0
    pub rankdir: Option<String>,   // lr, lr, tb, bt // default tb
    pub acyclicer: Option<String>, // greedy, dfs, unknown-should-still-work
    pub ranker: Option<String>, // "longest-path", "tight-tree", "network-simplex", "unknown-should-still-work"
    pub align: Option<String>,
    pub nesting_root: Option<String>, // id of dummy nesting root
    pub root: Option<String>,
    pub node_rank_factor: Option<f32>, // default 0
    pub dummy_chains: Option<Vec<String>>,
}

impl Default for GraphConfig {
    fn default() -> Self {
        Self {
            width: 0.0,
            height: 0.0,
            nodesep: Some(50.0),
            edgesep: Some(20.0),
            ranksep: Some(50.0),
            marginx: None,
            marginy: None,
            rankdir: Some("tb".to_string()),
            acyclicer: None,
            ranker: None,
            align: None,
            nesting_root: None,
            root: None,
            node_rank_factor: None,
            dummy_chains: None,
        }
    }
}

impl Default for GraphEdge {
    fn default() -> Self {
        Self {
            forward_name: None,
            reversed: None,
            minlen: Some(1.0),
            weight: Some(1.0),
            width: Some(0.0),
            height: Some(0.0),
            label_rank: None,
            labeloffset: Some(0.0),
            labelpos: Some("r".to_string()),
            nesting_edge: None,
            cutvalue: None,
            points: None,
            x: 0.0,
            y: 0.0,
        }
    }
}
