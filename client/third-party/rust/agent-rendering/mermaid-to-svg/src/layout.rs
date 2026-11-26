use std::collections::{HashMap, HashSet};

use crate::ast::{EdgeStyle, FlowchartGraph, GraphDirection, Node, NodeShape, Statement};
use crate::config::RenderConfig;
use crate::text_wrap::{
    measure_wrapped_lines_with_font_size, scale_char_width, wrap_text_lines, DEFAULT_CHAR_WIDTH,
    DEFAULT_FONT_SIZE, DEFAULT_WRAP_WIDTH,
};
use dagre_rust::layout::layout as dagre_layout;
use dagre_rust::{GraphConfig, GraphEdge, GraphNode};
use graphlib_rust::Graph;

const FLOWCHART_PADDING: f64 = 15.0;
const EDGE_LABEL_PADDING: f64 = 2.0;
const RANK_SEP: f64 = 50.0;
const NODE_SEP: f64 = 50.0;
const MARGIN: f64 = 8.0;
const SUBGRAPH_PADDING: f64 = 8.0;
const SUBGRAPH_TITLE_HEIGHT: f64 = 24.0;
const SUBGRAPH_GAP: f64 = 25.0;
const MIN_NODE_WIDTH: f64 = 0.0;
const MIN_NODE_HEIGHT: f64 = 0.0;
#[allow(dead_code)]
const EDGE_LABEL_GAP: f64 = 24.0;
const STATE_CHAR_WIDTH: f64 = 6.7;
const STATE_NODE_WIDTH_PADDING: f64 = 6.0;
const STATE_NODE_HEIGHT_PADDING: f64 = 16.0;
const STATE_NODE_MIN_HEIGHT: f64 = 40.0;
const STATE_DIAMOND_PADDING: f64 = 18.0;
const STATE_FORK_WIDTH: f64 = 70.0;
const STATE_FORK_HEIGHT: f64 = 7.0;

#[derive(Debug, Clone)]
pub struct LayoutNode {
    pub id: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub shape: NodeShape,
    pub label: String,
    pub fill_color: Option<String>,
    pub stroke_color: Option<String>,
}

fn graph_contains_state_shapes(statements: &[Statement]) -> bool {
    for statement in statements {
        match statement {
            Statement::Node(node) => {
                if matches!(
                    node.shape,
                    NodeShape::StartState | NodeShape::EndState | NodeShape::ForkJoin
                ) {
                    return true;
                }
            }
            Statement::Subgraph(subgraph) => {
                if graph_contains_state_shapes(&subgraph.statements) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct LayoutEdge {
    pub from: String,
    pub to: String,
    pub label: Option<String>,
    pub style: EdgeStyle,
    pub points: Vec<(f64, f64)>,
    pub label_pos: Option<(f64, f64)>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct LayoutSubgraph {
    pub id: String,
    pub title: Option<String>,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug)]
pub struct LayoutResult {
    pub nodes: HashMap<String, LayoutNode>,
    pub edges: Vec<LayoutEdge>,
    pub subgraphs: Vec<LayoutSubgraph>,
    pub width: f64,
    pub height: f64,
}

pub fn compute_layout(graph: &FlowchartGraph) -> LayoutResult {
    let mut layout = LayoutEngine::new(graph);
    layout.compute_with_dagre(true)
}
pub fn compute_layout_with_config(graph: &FlowchartGraph, config: &RenderConfig) -> LayoutResult {
    let mut layout =
        LayoutEngine::new_with_options(graph, FlowchartLayoutOptions::from_render_config(config));
    layout.compute_with_dagre(true)
}

#[allow(dead_code)]
pub fn compute_layout_no_subgraph_centering(graph: &FlowchartGraph) -> LayoutResult {
    let mut layout = LayoutEngine::new(graph);
    layout.compute_with_dagre(false)
}

#[allow(dead_code)]
pub fn compute_layout_no_subgraph_centering_with_config(
    graph: &FlowchartGraph,
    config: &RenderConfig,
) -> LayoutResult {
    let mut layout =
        LayoutEngine::new_with_options(graph, FlowchartLayoutOptions::from_render_config(config));
    layout.compute_with_dagre(false)
}

struct SubgraphInfo {
    id: String,
    title: Option<String>,
    parent_subgraph_id: Option<String>,
}

struct LayoutEngine<'a> {
    graph: &'a FlowchartGraph,
    options: FlowchartLayoutOptions,
    is_state_diagram: bool,
    nodes: HashMap<String, NodeInfo>,
    edges: Vec<EdgeInfo>,
    subgraphs: Vec<SubgraphInfo>,
    adjacency: HashMap<String, Vec<String>>,
    reverse_adjacency: HashMap<String, Vec<String>>,
    next_node_order: usize,
    node_to_subgraph: HashMap<String, String>,
    node_styles: HashMap<String, Vec<(String, String)>>,
}

#[derive(Clone)]
#[allow(dead_code)]
struct NodeInfo {
    id: String,
    label: String,
    shape: NodeShape,
    width: f64,
    height: f64,
    rank: i32,
    order: usize,
}

#[derive(Clone)]
struct EdgeInfo {
    from: String,
    to: String,
    label: Option<String>,
    style: EdgeStyle,
}

#[allow(dead_code)]
struct ClusterAnalysis {
    subgraph_nodes: HashMap<String, HashSet<String>>,
    external_edges: HashMap<String, bool>,
}

type DagreGraph = Graph<GraphConfig, GraphNode, GraphEdge>;
type EdgeMap = HashMap<usize, (String, String)>;
type PositionMap = HashMap<String, (f64, f64)>;
type EdgePointMap = HashMap<usize, Vec<(f64, f64)>>;
type EdgeLabelPosMap = HashMap<usize, (f64, f64)>;
#[derive(Debug, Clone, Copy)]
struct FlowchartLayoutOptions {
    node_spacing: f64,
    rank_spacing: f64,
    padding: f64,
    wrapping_width: f64,
    font_size: f64,
}

impl Default for FlowchartLayoutOptions {
    fn default() -> Self {
        Self {
            node_spacing: NODE_SEP,
            rank_spacing: RANK_SEP,
            padding: FLOWCHART_PADDING,
            wrapping_width: DEFAULT_WRAP_WIDTH,
            font_size: DEFAULT_FONT_SIZE,
        }
    }
}

impl FlowchartLayoutOptions {
    fn from_render_config(config: &RenderConfig) -> Self {
        let default = Self::default();
        Self {
            node_spacing: config
                .flowchart
                .node_spacing
                .map(f64::from)
                .unwrap_or(default.node_spacing),
            rank_spacing: config
                .flowchart
                .rank_spacing
                .map(f64::from)
                .unwrap_or(default.rank_spacing),
            padding: config
                .flowchart
                .padding
                .map(f64::from)
                .unwrap_or(default.padding),
            wrapping_width: config
                .flowchart
                .wrapping_width
                .map(f64::from)
                .unwrap_or(default.wrapping_width),
            font_size: config.font_size_px().unwrap_or(default.font_size),
        }
    }
}

impl<'a> LayoutEngine<'a> {
    fn new(graph: &'a FlowchartGraph) -> Self {
        Self::new_with_options(graph, FlowchartLayoutOptions::default())
    }

    fn new_with_options(graph: &'a FlowchartGraph, options: FlowchartLayoutOptions) -> Self {
        let mut engine = LayoutEngine {
            graph,
            options,
            is_state_diagram: graph_contains_state_shapes(&graph.statements),
            nodes: HashMap::new(),
            edges: Vec::new(),
            subgraphs: Vec::new(),
            adjacency: HashMap::new(),
            reverse_adjacency: HashMap::new(),
            next_node_order: 0,
            node_to_subgraph: HashMap::new(),
            node_styles: HashMap::new(),
        };
        engine.collect_nodes_and_edges(&graph.statements, None);
        engine
    }

    fn compute_with_dagre(&mut self, center_subgraph_nodes: bool) -> LayoutResult {
        let (node_sep, rank_sep) = self.compute_spacing();
        let rank_dir = match self.graph.direction {
            GraphDirection::TopToBottom => "tb",
            GraphDirection::BottomToTop => "bt",
            GraphDirection::LeftToRight => "lr",
            GraphDirection::RightToLeft => "rl",
        };

        let cluster_analysis = self.analyze_clusters();
        let back_edges = self.detect_back_edges();
        let (mut dagre_graph, edge_map) =
            self.build_dagre_graph(rank_dir, node_sep, rank_sep, &cluster_analysis, &back_edges);
        dagre_layout(&mut dagre_graph);
        let (mut positions, edge_points, edge_label_positions) =
            self.extract_layout_from_dagre(&dagre_graph, &edge_map);
        if self.is_state_diagram {
            self.snap_state_ranks(&mut positions, &back_edges);
            self.align_state_terminal_singletons(&mut positions, &back_edges);
        }
        let is_vertical = matches!(
            self.graph.direction,
            GraphDirection::TopToBottom | GraphDirection::BottomToTop
        );
        if center_subgraph_nodes {
            self.center_nodes_in_subgraphs(&mut positions, is_vertical);
        }
        let (width, height) = self.compute_bounds(&positions);
        let layout_nodes: HashMap<String, LayoutNode> = positions
            .into_iter()
            .filter_map(|(id, (x, y))| {
                let info = self.nodes.get(&id)?;
                let (fill_color, stroke_color) = self.get_node_colors(&id);
                Some((
                    id.clone(),
                    LayoutNode {
                        id,
                        x,
                        y,
                        width: info.width,
                        height: info.height,
                        shape: info.shape,
                        label: info.label.clone(),
                        fill_color,
                        stroke_color,
                    },
                ))
            })
            .collect();
        let layout_subgraphs: Vec<LayoutSubgraph> =
            self.compute_subgraph_bounds(&layout_nodes, SUBGRAPH_PADDING);
        let layout_edges: Vec<LayoutEdge> = self
            .edges
            .iter()
            .enumerate()
            .filter_map(|(idx, e)| {
                let from_node =
                    self.layout_endpoint_node(&layout_nodes, &layout_subgraphs, &e.from)?;
                let to_node = self.layout_endpoint_node(&layout_nodes, &layout_subgraphs, &e.to)?;
                let is_back_edge = self.is_back_edge(&from_node, &to_node);
                let is_vertical = matches!(
                    self.graph.direction,
                    GraphDirection::TopToBottom | GraphDirection::BottomToTop
                );

                let dagre_points = edge_points.get(&idx).cloned().unwrap_or_else(|| {
                    self.compute_edge_points_with_obstacles(&from_node, &to_node, &layout_nodes)
                });

                let mut points = if is_back_edge {
                    self.compute_back_edge_points(&from_node, &to_node, is_vertical, &layout_nodes)
                } else {
                    self.straighten_if_aligned(
                        &dagre_points,
                        &from_node,
                        &to_node,
                        is_vertical,
                        &layout_nodes,
                    )
                };
                // Cluster-target edges: dagre routes to an interior member node, so
                // the polyline tail dives inside the cluster rect and the clip then
                // curls it back to the boundary. Drop the interior points first so
                // the edge approaches the cluster boundary monotonically from outside.
                if !is_back_edge {
                    let from_is_cluster = self.is_subgraph_id(&e.from);
                    let to_is_cluster = self.is_subgraph_id(&e.to);
                    if from_is_cluster || to_is_cluster {
                        Self::trim_cluster_interior_points(
                            &mut points,
                            &from_node,
                            &to_node,
                            from_is_cluster,
                            to_is_cluster,
                        );
                    }
                }
                self.clip_edge_to_boundaries(&mut points, &from_node, &to_node);

                let label_pos = e.label.as_ref().and_then(|label| {
                    if label.trim().is_empty() {
                        None
                    } else if is_back_edge {
                        Some(Self::edge_label_midpoint(&points))
                    } else {
                        let midpoint = Self::edge_label_midpoint(&points);
                        edge_label_positions
                            .get(&idx)
                            .copied()
                            .filter(|(x, y)| {
                                let mut min_x = f64::INFINITY;
                                let mut max_x = f64::NEG_INFINITY;
                                let mut min_y = f64::INFINITY;
                                let mut max_y = f64::NEG_INFINITY;
                                for &(px, py) in &points {
                                    min_x = min_x.min(px);
                                    max_x = max_x.max(px);
                                    min_y = min_y.min(py);
                                    max_y = max_y.max(py);
                                }
                                *x >= min_x - 8.0
                                    && *x <= max_x + 8.0
                                    && *y >= min_y - 8.0
                                    && *y <= max_y + 8.0
                            })
                            .or(Some(midpoint))
                    }
                });

                Some(LayoutEdge {
                    from: e.from.clone(),
                    to: e.to.clone(),
                    label: e.label.clone(),
                    style: e.style,
                    points,
                    label_pos,
                })
            })
            .collect();
        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        for sg in &layout_subgraphs {
            min_x = min_x.min(sg.x);
            min_y = min_y.min(sg.y);
        }
        for node in layout_nodes.values() {
            min_x = min_x.min(node.x - node.width / 2.0);
            min_y = min_y.min(node.y - node.height / 2.0);
        }
        for edge in &layout_edges {
            if let Some((label_x, label_y, label_w, label_h)) = self.edge_label_bounds(edge) {
                min_x = min_x.min(label_x - label_w / 2.0);
                min_y = min_y.min(label_y - label_h / 2.0);
            }
        }
        let x_shift = if min_x < MARGIN { MARGIN - min_x } else { 0.0 };
        let y_shift = if min_y < MARGIN { MARGIN - min_y } else { 0.0 };
        let layout_nodes: HashMap<String, LayoutNode> = layout_nodes
            .into_iter()
            .map(|(id, mut node)| {
                node.x += x_shift;
                node.y += y_shift;
                (id, node)
            })
            .collect();
        let layout_edges: Vec<LayoutEdge> = layout_edges
            .into_iter()
            .map(|mut edge| {
                for point in &mut edge.points {
                    point.0 += x_shift;
                    point.1 += y_shift;
                }
                if let Some((x, y)) = edge.label_pos.as_mut() {
                    *x += x_shift;
                    *y += y_shift;
                }
                edge
            })
            .collect();
        let layout_subgraphs: Vec<LayoutSubgraph> = layout_subgraphs
            .into_iter()
            .map(|mut sg| {
                sg.x += x_shift;
                sg.y += y_shift;
                sg
            })
            .collect();
        let mut final_width = width + x_shift;
        let mut final_height = height + y_shift;
        for sg in &layout_subgraphs {
            final_width = final_width.max(sg.x + sg.width + MARGIN);
            final_height = final_height.max(sg.y + sg.height + MARGIN);
        }
        for edge in &layout_edges {
            for &(px, py) in &edge.points {
                final_width = final_width.max(px + MARGIN);
                final_height = final_height.max(py + MARGIN);
            }
            if let Some((label_x, label_y, label_w, label_h)) = self.edge_label_bounds(edge) {
                final_width = final_width.max(label_x + label_w / 2.0 + MARGIN);
                final_height = final_height.max(label_y + label_h / 2.0 + MARGIN);
            }
        }
        LayoutResult {
            nodes: layout_nodes,
            edges: layout_edges,
            subgraphs: layout_subgraphs,
            width: final_width,
            height: final_height,
        }
    }

    fn compute_spacing(&self) -> (f64, f64) {
        (self.options.node_spacing, self.options.rank_spacing)
    }

    fn subgraph_ids_in_mermaid_order(&self) -> Vec<String> {
        if self.subgraphs.is_empty() {
            return Vec::new();
        }

        let mut children_by_parent: HashMap<String, Vec<String>> = HashMap::new();
        let mut roots: Vec<String> = Vec::new();

        for sg in &self.subgraphs {
            let parent_key = sg.parent_subgraph_id.clone().unwrap_or_default();
            children_by_parent
                .entry(parent_key)
                .or_default()
                .push(sg.id.clone());

            if sg.parent_subgraph_id.is_none() {
                roots.push(sg.id.clone());
            }
        }

        fn dfs(
            id: &str,
            children_by_parent: &HashMap<String, Vec<String>>,
            visited: &mut HashSet<String>,
            out: &mut Vec<String>,
        ) {
            if !visited.insert(id.to_string()) {
                return;
            }

            if let Some(children) = children_by_parent.get(id) {
                for child in children {
                    dfs(child, children_by_parent, visited, out);
                }
            }

            out.push(id.to_string());
        }

        let mut post_order: Vec<String> = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        for root in roots {
            dfs(&root, &children_by_parent, &mut visited, &mut post_order);
        }

        post_order.reverse();
        post_order
    }

    fn center_nodes_in_subgraphs(&self, positions: &mut PositionMap, is_vertical: bool) {
        let connected_groups = self.find_connected_subgraph_groups();
        for group in connected_groups {
            // Compute the overall group center from all nodes.
            let all_nodes_in_group: Vec<String> = group
                .iter()
                .flat_map(|sg_id| {
                    self.node_to_subgraph
                        .iter()
                        .filter(|(_, id)| *id == sg_id)
                        .map(|(node_id, _)| node_id.clone())
                        .collect::<Vec<_>>()
                })
                .collect();
            if all_nodes_in_group.is_empty() {
                continue;
            }
            let all_coords: Vec<f64> = all_nodes_in_group
                .iter()
                .filter_map(|id| positions.get(id))
                .map(|(x, y)| if is_vertical { *x } else { *y })
                .collect();
            if all_coords.is_empty() {
                continue;
            }
            let group_avg = all_coords.iter().sum::<f64>() / all_coords.len() as f64;

            // Shift each subgraph's nodes so the subgraph center matches the
            // group center, preserving relative offsets within each subgraph.
            for sg_id in &group {
                let sg_nodes: Vec<String> = self
                    .node_to_subgraph
                    .iter()
                    .filter(|(_, id)| *id == sg_id)
                    .map(|(node_id, _)| node_id.clone())
                    .collect();
                if sg_nodes.is_empty() {
                    continue;
                }
                let sg_coords: Vec<f64> = sg_nodes
                    .iter()
                    .filter_map(|id| positions.get(id))
                    .map(|(x, y)| if is_vertical { *x } else { *y })
                    .collect();
                if sg_coords.is_empty() {
                    continue;
                }
                let sg_avg = sg_coords.iter().sum::<f64>() / sg_coords.len() as f64;
                let shift = group_avg - sg_avg;
                for node_id in &sg_nodes {
                    if let Some((x, y)) = positions.get_mut(node_id) {
                        if is_vertical {
                            *x += shift;
                        } else {
                            *y += shift;
                        }
                    }
                }
            }
        }
    }

    fn find_connected_subgraph_groups(&self) -> Vec<HashSet<String>> {
        if self.subgraphs.is_empty() {
            return vec![];
        }
        let mut subgraph_connections: HashMap<String, HashSet<String>> = HashMap::new();
        for sg in &self.subgraphs {
            subgraph_connections.insert(sg.id.clone(), HashSet::new());
        }
        for edge in &self.edges {
            let from_sg = self.node_to_subgraph.get(&edge.from);
            let to_sg = self.node_to_subgraph.get(&edge.to);
            if let (Some(from_sg), Some(to_sg)) = (from_sg, to_sg) {
                if from_sg != to_sg {
                    subgraph_connections
                        .entry(from_sg.clone())
                        .or_default()
                        .insert(to_sg.clone());
                    subgraph_connections
                        .entry(to_sg.clone())
                        .or_default()
                        .insert(from_sg.clone());
                }
            }
        }
        let mut visited: HashSet<String> = HashSet::new();
        let mut groups: Vec<HashSet<String>> = vec![];
        for sg in &self.subgraphs {
            if visited.contains(&sg.id) {
                continue;
            }
            let mut group: HashSet<String> = HashSet::new();
            let mut stack = vec![sg.id.clone()];
            while let Some(current) = stack.pop() {
                if group.insert(current.clone()) {
                    visited.insert(current.clone());
                    if let Some(connected) = subgraph_connections.get(&current) {
                        for neighbor in connected {
                            if !group.contains(neighbor) {
                                stack.push(neighbor.clone());
                            }
                        }
                    }
                }
            }
            if group.len() > 1 {
                groups.push(group);
            }
        }
        groups
    }

    fn analyze_clusters(&self) -> ClusterAnalysis {
        let mut subgraph_nodes: HashMap<String, HashSet<String>> = HashMap::new();
        for sg in &self.subgraphs {
            let nodes: HashSet<String> = self
                .node_to_subgraph
                .iter()
                .filter(|(_, sg_id)| *sg_id == &sg.id)
                .map(|(node_id, _)| node_id.clone())
                .collect();
            subgraph_nodes.insert(sg.id.clone(), nodes);
        }
        let mut external_edges: HashMap<String, bool> = HashMap::new();
        for sg in &self.subgraphs {
            let nodes_in_sg: &HashSet<String> = &subgraph_nodes[&sg.id];
            let has_external = self.edges.iter().any(|e| {
                let from_in = nodes_in_sg.contains(&e.from);
                let to_in = nodes_in_sg.contains(&e.to);
                from_in != to_in
            });
            external_edges.insert(sg.id.clone(), has_external);
        }
        ClusterAnalysis {
            subgraph_nodes,
            external_edges,
        }
    }

    fn compute_subgraph_bounds(
        &self,
        layout_nodes: &HashMap<String, LayoutNode>,
        padding: f64,
    ) -> Vec<LayoutSubgraph> {
        // Process subgraphs bottom-up: children before parents.  This way a
        // parent's bounds can expand to encompass its children's padded rects.
        let ordered_ids = self.subgraph_ids_bottom_up();

        // Maps sg_id -> (min_x, min_y, max_x, max_y) of the final padded rect.
        let mut rect_map: HashMap<String, (f64, f64, f64, f64)> = HashMap::new();

        for sg_id in &ordered_ids {
            let sg = self.subgraphs.iter().find(|s| s.id == *sg_id).unwrap();
            // Collect only directly-owned nodes (not from child subgraphs).
            let direct_nodes: Vec<String> = self
                .node_to_subgraph
                .iter()
                .filter(|(_, id)| *id == sg_id)
                .map(|(node_id, _)| node_id.clone())
                .collect();

            let mut min_x = f64::INFINITY;
            let mut min_y = f64::INFINITY;
            let mut max_x = f64::NEG_INFINITY;
            let mut max_y = f64::NEG_INFINITY;

            for node_id in &direct_nodes {
                if let Some(node) = layout_nodes.get(node_id) {
                    min_x = min_x.min(node.x - node.width / 2.0);
                    max_x = max_x.max(node.x + node.width / 2.0);
                    min_y = min_y.min(node.y - node.height / 2.0);
                    max_y = max_y.max(node.y + node.height / 2.0);
                }
            }

            // Expand to encompass already-computed child subgraph rects.
            for child_sg in &self.subgraphs {
                if child_sg.parent_subgraph_id.as_deref() == Some(sg_id) {
                    if let Some(&(cx, cy, cx2, cy2)) = rect_map.get(&child_sg.id) {
                        min_x = min_x.min(cx);
                        min_y = min_y.min(cy);
                        max_x = max_x.max(cx2);
                        max_y = max_y.max(cy2);
                    }
                }
            }

            if min_x.is_infinite() {
                continue;
            }

            let title_padding = sg
                .title
                .as_deref()
                .map(|title| self.subgraph_title_height(title))
                .unwrap_or(0.0);
            let rx = min_x - padding;
            let ry = min_y - padding - title_padding;
            let rw = (max_x - min_x) + padding * 2.0;
            let rh = (max_y - min_y) + padding * 2.0 + title_padding;
            rect_map.insert(sg_id.clone(), (rx, ry, rx + rw, ry + rh));
        }

        self.subgraphs
            .iter()
            .filter_map(|sg| {
                rect_map
                    .get(&sg.id)
                    .map(|&(rx, ry, rx2, ry2)| LayoutSubgraph {
                        id: sg.id.clone(),
                        title: sg.title.clone(),
                        x: rx,
                        y: ry,
                        width: rx2 - rx,
                        height: ry2 - ry,
                    })
            })
            .collect()
    }

    fn subgraph_title_height(&self, title: &str) -> f64 {
        let char_width = scale_char_width(DEFAULT_CHAR_WIDTH, self.options.font_size);
        let lines = wrap_text_lines(title, self.options.wrapping_width, char_width);
        let (_, text_height) =
            measure_wrapped_lines_with_font_size(&lines, char_width, self.options.font_size);
        text_height.max(SUBGRAPH_TITLE_HEIGHT)
    }

    /// Returns subgraph IDs in bottom-up order (leaf subgraphs first).
    fn subgraph_ids_bottom_up(&self) -> Vec<String> {
        let mut children_by_parent: HashMap<String, Vec<String>> = HashMap::new();
        let mut roots: Vec<String> = Vec::new();
        for sg in &self.subgraphs {
            let parent_key = sg.parent_subgraph_id.clone().unwrap_or_default();
            children_by_parent
                .entry(parent_key)
                .or_default()
                .push(sg.id.clone());
            if sg.parent_subgraph_id.is_none() {
                roots.push(sg.id.clone());
            }
        }
        fn dfs_post(
            id: &str,
            children_by_parent: &HashMap<String, Vec<String>>,
            out: &mut Vec<String>,
        ) {
            if let Some(children) = children_by_parent.get(id) {
                for child in children {
                    dfs_post(child, children_by_parent, out);
                }
            }
            out.push(id.to_string());
        }
        let mut result = Vec::new();
        for root in &roots {
            dfs_post(root, &children_by_parent, &mut result);
        }
        result
    }

    #[allow(dead_code)]
    fn extract_subgraph_layouts(&self, g: &DagreGraph) -> Vec<LayoutSubgraph> {
        self.subgraphs
            .iter()
            .filter_map(|sg| {
                let node = g.node(&sg.id)?;
                let width = node.width as f64;
                let height = node.height as f64;
                if width <= 0.0 || height <= 0.0 {
                    return None;
                }
                Some(LayoutSubgraph {
                    id: sg.id.clone(),
                    title: sg.title.clone(),
                    x: node.x as f64 - width / 2.0,
                    y: node.y as f64 - height / 2.0,
                    width,
                    height,
                })
            })
            .collect()
    }

    #[allow(dead_code)]
    fn rotate_layout(&self, positions: &mut PositionMap, edge_points: &mut EdgePointMap) {
        for (x, y) in positions.values_mut() {
            std::mem::swap(x, y);
        }
        for pts in edge_points.values_mut() {
            for (x, y) in pts.iter_mut() {
                std::mem::swap(x, y);
            }
        }
    }

    fn build_dagre_graph(
        &self,
        rank_dir: &str,
        node_sep: f64,
        rank_sep: f64,
        _cluster_analysis: &ClusterAnalysis,
        back_edges: &HashSet<(String, String)>,
    ) -> (DagreGraph, EdgeMap) {
        let mut g: DagreGraph = Graph::new(Some(graphlib_rust::GraphOption {
            directed: Some(true),
            multigraph: Some(true),
            compound: Some(true),
        }));
        g.set_graph(GraphConfig {
            rankdir: Some(rank_dir.to_string()),
            nodesep: Some(node_sep as f32),
            ranksep: Some(rank_sep as f32),
            edgesep: Some(20.0),
            marginx: Some(MARGIN as f32),
            marginy: Some(MARGIN as f32),
            ranker: if self.is_state_diagram {
                Some("longest-path".to_string())
            } else {
                None
            },
            ..Default::default()
        });

        for sg_id in self.subgraph_ids_in_mermaid_order() {
            g.set_node(
                sg_id.clone(),
                Some(GraphNode {
                    width: 0.0,
                    height: 0.0,
                    padding: Some(SUBGRAPH_PADDING as f32),
                    ..Default::default()
                }),
            );
        }

        let mut sorted_nodes: Vec<_> = self.nodes.iter().collect();
        sorted_nodes.sort_by_key(|(_, info)| info.order);
        for (id, info) in &sorted_nodes {
            g.set_node(
                (*id).clone(),
                Some(GraphNode {
                    width: info.width as f32,
                    height: info.height as f32,
                    ..Default::default()
                }),
            );
        }

        for (node_id, sg_id) in &self.node_to_subgraph {
            let _ = g.set_parent(node_id, Some(sg_id.clone()));
        }

        for sg in &self.subgraphs {
            if let Some(ref parent_id) = sg.parent_subgraph_id {
                let _ = g.set_parent(&sg.id, Some(parent_id.clone()));
            }
        }

        let mut edge_map: EdgeMap = HashMap::new();
        for (idx, e) in self.edges.iter().enumerate() {
            if back_edges.contains(&(e.from.clone(), e.to.clone())) {
                continue;
            }
            let dagre_from = self
                .dagre_edge_endpoint(&e.from, true)
                .unwrap_or_else(|| e.from.clone());
            let dagre_to = self
                .dagre_edge_endpoint(&e.to, false)
                .unwrap_or_else(|| e.to.clone());
            if dagre_from == dagre_to {
                continue;
            }
            let mut edge_label = GraphEdge {
                labelpos: Some("c".to_string()),
                ..Default::default()
            };
            if let Some(label) = &e.label {
                if let Some((width, height)) = self.edge_label_dimensions(label) {
                    edge_label.width = Some(width as f32);
                    edge_label.height = Some(height as f32);
                }
            }

            let _ = g.set_edge(&dagre_from, &dagre_to, Some(edge_label), None);
            edge_map.insert(idx, (dagre_from, dagre_to));
        }
        (g, edge_map)
    }

    fn extract_layout_from_dagre(
        &self,
        g: &DagreGraph,
        edge_map: &EdgeMap,
    ) -> (PositionMap, EdgePointMap, EdgeLabelPosMap) {
        let mut positions: HashMap<String, (f64, f64)> = HashMap::new();
        for node_id in g.nodes() {
            if let Some(node) = g.node(&node_id) {
                positions.insert(node_id, (node.x as f64, node.y as f64));
            }
        }
        let mut edge_points: HashMap<usize, Vec<(f64, f64)>> = HashMap::new();
        let mut edge_label_positions: EdgeLabelPosMap = HashMap::new();
        for (idx, (from, to)) in edge_map {
            if let Some(edge) = g.edge(from, to, None) {
                if let Some(ref points) = edge.points {
                    let pts: Vec<(f64, f64)> =
                        points.iter().map(|p| (p.x as f64, p.y as f64)).collect();
                    edge_points.insert(*idx, pts);
                }
                if edge.width.unwrap_or(0.0) > 0.0 || edge.height.unwrap_or(0.0) > 0.0 {
                    edge_label_positions.insert(*idx, (edge.x as f64, edge.y as f64));
                }
            }
        }
        (positions, edge_points, edge_label_positions)
    }

    fn snap_state_ranks(
        &self,
        positions: &mut PositionMap,
        back_edges: &HashSet<(String, String)>,
    ) {
        let ranks = self.longest_path_ranks_without_back_edges(back_edges);
        if ranks.is_empty() {
            return;
        }

        let mut level_positions: Vec<f64> = positions.values().map(|(_, y)| *y).collect();
        level_positions.sort_by(|a, b| a.total_cmp(b));
        level_positions.dedup_by(|a, b| (*a - *b).abs() < 0.5);

        let max_rank = ranks.values().copied().max().unwrap_or(0) as usize;
        if level_positions.len() <= max_rank {
            return;
        }

        for (node_id, rank) in ranks {
            if let Some((_, y)) = positions.get_mut(&node_id) {
                *y = level_positions[rank as usize];
            }
        }
    }

    fn align_state_terminal_singletons(
        &self,
        positions: &mut PositionMap,
        back_edges: &HashSet<(String, String)>,
    ) {
        let ranks = self.longest_path_ranks_without_back_edges(back_edges);
        if ranks.is_empty() {
            return;
        }

        let mut nodes_by_rank: HashMap<i32, Vec<&str>> = HashMap::new();
        for (node_id, rank) in &ranks {
            nodes_by_rank
                .entry(*rank)
                .or_default()
                .push(node_id.as_str());
        }

        for (node_id, rank) in &ranks {
            if *rank <= 0 {
                continue;
            }

            let Some(rank_nodes) = nodes_by_rank.get(rank) else {
                continue;
            };
            if rank_nodes.len() != 1 {
                continue;
            }

            let has_forward_outgoing = self.edges.iter().any(|edge| {
                edge.from == *node_id && !back_edges.contains(&(edge.from.clone(), edge.to.clone()))
            });
            if has_forward_outgoing {
                continue;
            }

            let mut predecessor_xs: Vec<f64> = self
                .edges
                .iter()
                .filter(|edge| {
                    edge.to == *node_id
                        && !back_edges.contains(&(edge.from.clone(), edge.to.clone()))
                        && matches!(ranks.get(&edge.from), Some(pred_rank) if *pred_rank < *rank)
                })
                .filter_map(|edge| positions.get(&edge.from).map(|(x, _)| *x))
                .collect();
            if predecessor_xs.len() < 2 {
                continue;
            }

            predecessor_xs.sort_by(|a, b| a.total_cmp(b));
            if let Some((x, _)) = positions.get_mut(node_id) {
                *x = *predecessor_xs.last().unwrap_or(x);
            }
        }
    }

    fn longest_path_ranks_without_back_edges(
        &self,
        back_edges: &HashSet<(String, String)>,
    ) -> HashMap<String, i32> {
        let mut indegree: HashMap<String, usize> =
            self.nodes.keys().cloned().map(|id| (id, 0)).collect();

        for edge in &self.edges {
            if back_edges.contains(&(edge.from.clone(), edge.to.clone())) {
                continue;
            }
            if let Some(count) = indegree.get_mut(&edge.to) {
                *count += 1;
            }
        }

        let mut ready: Vec<String> = indegree
            .iter()
            .filter(|(_, count)| **count == 0)
            .map(|(id, _)| id.clone())
            .collect();
        ready.sort_by_key(|id| {
            self.nodes
                .get(id)
                .map(|node| node.order)
                .unwrap_or(usize::MAX)
        });

        let mut order: Vec<String> = Vec::new();
        let mut indegree_mut = indegree;
        while let Some(node_id) = ready.first().cloned() {
            ready.remove(0);
            order.push(node_id.clone());
            for edge in &self.edges {
                if edge.from != node_id
                    || back_edges.contains(&(edge.from.clone(), edge.to.clone()))
                {
                    continue;
                }
                if let Some(count) = indegree_mut.get_mut(&edge.to) {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        ready.push(edge.to.clone());
                    }
                }
            }
            ready.sort_by_key(|id| {
                self.nodes
                    .get(id)
                    .map(|node| node.order)
                    .unwrap_or(usize::MAX)
            });
        }

        let mut ranks: HashMap<String, i32> =
            self.nodes.keys().cloned().map(|id| (id, 0)).collect();

        for node_id in order {
            let base_rank = *ranks.get(&node_id).unwrap_or(&0);
            for edge in &self.edges {
                if edge.from != node_id
                    || back_edges.contains(&(edge.from.clone(), edge.to.clone()))
                {
                    continue;
                }
                if let Some(rank) = ranks.get_mut(&edge.to) {
                    *rank = (*rank).max(base_rank + 1);
                }
            }
        }

        ranks
    }

    #[allow(dead_code)]
    fn apply_flip(
        &self,
        positions: &mut HashMap<String, (f64, f64)>,
        edge_points: &mut HashMap<usize, Vec<(f64, f64)>>,
        width: f64,
        height: f64,
        flip_x: bool,
        flip_y: bool,
    ) {
        for (x, y) in positions.values_mut() {
            if flip_x {
                *x = MARGIN + (width - 2.0 * MARGIN) - (*x - MARGIN);
            }
            if flip_y {
                *y = MARGIN + (height - 2.0 * MARGIN) - (*y - MARGIN);
            }
        }
        for pts in edge_points.values_mut() {
            for (x, y) in pts.iter_mut() {
                if flip_x {
                    *x = MARGIN + (width - 2.0 * MARGIN) - (*x - MARGIN);
                }
                if flip_y {
                    *y = MARGIN + (height - 2.0 * MARGIN) - (*y - MARGIN);
                }
            }
        }
    }

    fn collect_nodes_and_edges(
        &mut self,
        statements: &[Statement],
        current_subgraph_id: Option<String>,
    ) {
        for stmt in statements {
            match stmt {
                Statement::Node(node) => {
                    if self.is_subgraph_id(&node.id) {
                        continue;
                    }
                    self.add_node(node);
                    if let Some(ref sg_id) = current_subgraph_id {
                        if !self.node_to_subgraph.contains_key(&node.id) {
                            self.node_to_subgraph.insert(node.id.clone(), sg_id.clone());
                        }
                    }
                }
                Statement::Edge(edge) => {
                    self.ensure_node_exists(&edge.from);
                    self.ensure_node_exists(&edge.to);

                    if let Some(ref sg_id) = current_subgraph_id {
                        if !self.is_subgraph_id(&edge.from)
                            && !self.node_to_subgraph.contains_key(&edge.from)
                        {
                            self.node_to_subgraph
                                .insert(edge.from.clone(), sg_id.clone());
                        }
                        if !self.is_subgraph_id(&edge.to)
                            && !self.node_to_subgraph.contains_key(&edge.to)
                        {
                            self.node_to_subgraph.insert(edge.to.clone(), sg_id.clone());
                        }
                    }

                    self.adjacency
                        .entry(edge.from.clone())
                        .or_default()
                        .push(edge.to.clone());
                    self.reverse_adjacency
                        .entry(edge.to.clone())
                        .or_default()
                        .push(edge.from.clone());

                    self.edges.push(EdgeInfo {
                        from: edge.from.clone(),
                        to: edge.to.clone(),
                        label: edge.label.clone(),
                        style: edge.style,
                    });
                }
                Statement::Subgraph(subgraph) => {
                    let sg_info = SubgraphInfo {
                        id: subgraph.id.clone(),
                        title: subgraph.title.clone().or_else(|| Some(subgraph.id.clone())),
                        parent_subgraph_id: current_subgraph_id.clone(),
                    };
                    self.subgraphs.push(sg_info);
                    self.collect_nodes_and_edges(&subgraph.statements, Some(subgraph.id.clone()));
                }
                Statement::Style(style) => {
                    self.node_styles
                        .insert(style.node_id.clone(), style.properties.clone());
                }
            }
        }
    }

    fn add_node(&mut self, node: &Node) {
        if self.is_subgraph_id(&node.id) {
            return;
        }
        if self.nodes.contains_key(&node.id) && node.label.is_none() {
            return;
        }

        let label = node.label.as_deref().unwrap_or(&node.id).to_string();
        let (width, height) = self.measure_node(&label, node.shape);

        let order = self
            .nodes
            .get(&node.id)
            .map(|n| n.order)
            .unwrap_or_else(|| {
                let order = self.next_node_order;
                self.next_node_order += 1;
                order
            });

        self.nodes.insert(
            node.id.clone(),
            NodeInfo {
                id: node.id.clone(),
                label,
                shape: node.shape,
                width,
                height,
                rank: 0,
                order,
            },
        );
    }

    fn ensure_node_exists(&mut self, id: &str) {
        if self.is_subgraph_id(id) {
            return;
        }
        if !self.nodes.contains_key(id) {
            let (width, height) = self.measure_node(id, NodeShape::Rectangle);
            let order = self.next_node_order;
            self.next_node_order += 1;
            self.nodes.insert(
                id.to_string(),
                NodeInfo {
                    id: id.to_string(),
                    label: id.to_string(),
                    shape: NodeShape::Rectangle,
                    width,
                    height,
                    rank: 0,
                    order,
                },
            );
        }
    }

    fn is_subgraph_id(&self, id: &str) -> bool {
        self.subgraphs.iter().any(|subgraph| subgraph.id == id)
    }

    fn dagre_edge_endpoint(&self, id: &str, is_source: bool) -> Option<String> {
        if !self.is_subgraph_id(id) {
            return Some(id.to_string());
        }

        if is_source {
            self.subgraph_exit_node_id(id)
        } else {
            self.subgraph_entry_node_id(id)
        }
    }

    fn subgraph_entry_node_id(&self, subgraph_id: &str) -> Option<String> {
        let node_ids = self.nodes_in_subgraph_by_order(subgraph_id);
        if node_ids.is_empty() {
            return None;
        }

        node_ids
            .iter()
            .find(|node_id| {
                !self
                    .edges
                    .iter()
                    .any(|edge| node_ids.contains(&edge.from) && edge.to == **node_id)
            })
            .cloned()
            .or_else(|| node_ids.first().cloned())
    }

    fn subgraph_exit_node_id(&self, subgraph_id: &str) -> Option<String> {
        let node_ids = self.nodes_in_subgraph_by_order(subgraph_id);
        if node_ids.is_empty() {
            return None;
        }

        node_ids
            .iter()
            .rev()
            .find(|node_id| {
                !self
                    .edges
                    .iter()
                    .any(|edge| edge.from == **node_id && node_ids.contains(&edge.to))
            })
            .cloned()
            .or_else(|| node_ids.last().cloned())
    }

    fn nodes_in_subgraph_by_order(&self, subgraph_id: &str) -> Vec<String> {
        let mut node_ids: Vec<String> = self
            .node_to_subgraph
            .iter()
            .filter(|(_, sg_id)| sg_id.as_str() == subgraph_id)
            .map(|(node_id, _)| node_id.clone())
            .collect();
        node_ids.sort_by_key(|node_id| {
            self.nodes
                .get(node_id)
                .map(|node| node.order)
                .unwrap_or(usize::MAX)
        });
        node_ids
    }

    fn layout_endpoint_node(
        &self,
        layout_nodes: &HashMap<String, LayoutNode>,
        layout_subgraphs: &[LayoutSubgraph],
        id: &str,
    ) -> Option<LayoutNode> {
        if let Some(node) = layout_nodes.get(id) {
            return Some(node.clone());
        }

        layout_subgraphs
            .iter()
            .find(|subgraph| subgraph.id == id)
            .map(|subgraph| LayoutNode {
                id: subgraph.id.clone(),
                x: subgraph.x + subgraph.width / 2.0,
                y: subgraph.y + subgraph.height / 2.0,
                width: subgraph.width,
                height: subgraph.height,
                shape: NodeShape::Rectangle,
                label: subgraph
                    .title
                    .clone()
                    .unwrap_or_else(|| subgraph.id.clone()),
                fill_color: None,
                stroke_color: None,
            })
    }

    /// Mirrors mermaid.js shape sizing for flowcharts.
    /// Sources: packages/mermaid/src/rendering-util/rendering-elements/shapes/*.ts (question, hexagon, stadium, cylinder, subroutine, rect_left_inv_arrow, circle).
    fn measure_node(&self, label: &str, shape: NodeShape) -> (f64, f64) {
        let char_width = if self.is_state_diagram {
            scale_char_width(STATE_CHAR_WIDTH, self.options.font_size)
        } else {
            scale_char_width(DEFAULT_CHAR_WIDTH, self.options.font_size)
        };
        let lines = wrap_text_lines(label, self.options.wrapping_width, char_width);
        let (text_width, text_height) =
            measure_wrapped_lines_with_font_size(&lines, char_width, self.options.font_size);
        let padding = self.options.padding;
        if self.is_state_diagram {
            match shape {
                NodeShape::RoundedRectangle => {
                    let width = (text_width + STATE_NODE_WIDTH_PADDING).max(32.0);
                    let height =
                        (text_height + STATE_NODE_HEIGHT_PADDING).max(STATE_NODE_MIN_HEIGHT);
                    return (width, height);
                }
                NodeShape::Diamond => {
                    let size = (text_width + STATE_DIAMOND_PADDING)
                        .max(text_height + STATE_DIAMOND_PADDING)
                        .max(STATE_NODE_MIN_HEIGHT);
                    return (size, size);
                }
                NodeShape::StartState => return (14.0, 14.0),
                NodeShape::EndState => return (14.0, 14.0),
                NodeShape::ForkJoin => return (STATE_FORK_WIDTH, STATE_FORK_HEIGHT),
                _ => {}
            }
        }

        match shape {
            NodeShape::Rectangle => (text_width + padding * 4.0, text_height + padding * 2.0),
            NodeShape::RoundedRectangle => {
                (text_width + padding * 2.0, text_height + padding * 2.0)
            }
            NodeShape::Subroutine => {
                let w = text_width + padding;
                let h = text_height + padding;
                (w + 16.0, h)
            }
            NodeShape::Asymmetric => {
                let w = text_width + padding;
                let h = text_height + padding;
                (w + h / 4.0, h)
            }
            NodeShape::Hexagon => {
                let h = text_height + padding;
                let w = text_width + padding * 2.5;
                (w * 7.0 / 6.0, h)
            }
            NodeShape::Diamond => {
                let w = text_width + padding;
                let h = text_height + padding;
                let s = w + h;
                (s, s)
            }
            NodeShape::Circle => {
                let diameter = text_width + padding;
                (diameter, diameter)
            }
            NodeShape::StartState => (14.0, 14.0),
            NodeShape::EndState => (20.0, 20.0),
            NodeShape::ForkJoin => (70.0, 10.0),
            NodeShape::Stadium => {
                let h = text_height + padding;
                let w = text_width + h / 4.0 + padding;
                (w, h)
            }
            NodeShape::Cylinder => {
                let w = text_width + padding;
                let rx = w / 2.0;
                let ry = rx / (2.5 + w / 50.0);
                let h = text_height + ry + padding;
                (w, h + 2.0 * ry)
            }
        }
    }

    /// Mirrors mermaid.js edge label sizing with SVG background padding.
    /// Sources: packages/mermaid/src/rendering-util/createText.ts (padding=2) and rendering-elements/edges.js.
    fn edge_label_dimensions(&self, label: &str) -> Option<(f64, f64)> {
        if label.trim().is_empty() {
            return None;
        }
        let char_width = if self.is_state_diagram {
            scale_char_width(STATE_CHAR_WIDTH, self.options.font_size)
        } else {
            scale_char_width(DEFAULT_CHAR_WIDTH, self.options.font_size)
        };
        let lines = wrap_text_lines(label, self.options.wrapping_width, char_width);
        if lines.is_empty() {
            return None;
        }
        let (text_width, text_height) =
            measure_wrapped_lines_with_font_size(&lines, char_width, self.options.font_size);
        let width = text_width + EDGE_LABEL_PADDING * 2.0;
        let height = text_height + EDGE_LABEL_PADDING * 2.0;
        Some((width, height))
    }

    fn get_node_colors(&self, node_id: &str) -> (Option<String>, Option<String>) {
        self.node_styles
            .get(node_id)
            .map(|props| {
                let fill = props
                    .iter()
                    .find(|(k, _)| k == "fill")
                    .map(|(_, v)| v.clone());
                let stroke = props
                    .iter()
                    .find(|(k, _)| k == "stroke")
                    .map(|(_, v)| v.clone());
                (fill, stroke)
            })
            .unwrap_or((None, None))
    }

    #[allow(dead_code)]
    fn compute(&mut self) -> LayoutResult {
        self.assign_ranks();

        let ranks = self.group_by_rank();
        let mut positions = self.compute_positions(&ranks);
        self.separate_subgraphs(&mut positions);
        self.shift_external_nodes(&mut positions);

        let (width, height) = self.compute_bounds(&positions);

        let layout_nodes: HashMap<String, LayoutNode> = positions
            .into_iter()
            .map(|(id, (x, y))| {
                let info = &self.nodes[&id];
                let (fill_color, stroke_color) = self.get_node_colors(&id);
                (
                    id.clone(),
                    LayoutNode {
                        id,
                        x,
                        y,
                        width: info.width,
                        height: info.height,
                        shape: info.shape,
                        label: info.label.clone(),
                        fill_color,
                        stroke_color,
                    },
                )
            })
            .collect();

        let layout_edges: Vec<LayoutEdge> = self
            .edges
            .iter()
            .map(|e| {
                let from_node = &layout_nodes[&e.from];
                let to_node = &layout_nodes[&e.to];
                let points =
                    self.compute_edge_points_with_obstacles(from_node, to_node, &layout_nodes);
                LayoutEdge {
                    from: e.from.clone(),
                    to: e.to.clone(),
                    label: e.label.clone(),
                    style: e.style,
                    points,
                    label_pos: None,
                }
            })
            .collect();

        let subgraph_padding = 20.0;
        let layout_subgraphs: Vec<LayoutSubgraph> =
            self.compute_subgraph_bounds(&layout_nodes, subgraph_padding);

        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        for sg in &layout_subgraphs {
            min_x = min_x.min(sg.x);
            min_y = min_y.min(sg.y);
        }
        for node in layout_nodes.values() {
            min_x = min_x.min(node.x - node.width / 2.0);
            min_y = min_y.min(node.y - node.height / 2.0);
        }

        let x_shift = if min_x < MARGIN { MARGIN - min_x } else { 0.0 };
        let y_shift = if min_y < MARGIN { MARGIN - min_y } else { 0.0 };

        let layout_nodes: HashMap<String, LayoutNode> = layout_nodes
            .into_iter()
            .map(|(id, mut node)| {
                node.x += x_shift;
                node.y += y_shift;
                (id, node)
            })
            .collect();

        let layout_edges: Vec<LayoutEdge> = layout_edges
            .into_iter()
            .map(|mut edge| {
                for point in &mut edge.points {
                    point.0 += x_shift;
                    point.1 += y_shift;
                }
                edge
            })
            .collect();

        let layout_subgraphs: Vec<LayoutSubgraph> = layout_subgraphs
            .into_iter()
            .map(|mut sg| {
                sg.x += x_shift;
                sg.y += y_shift;
                sg
            })
            .collect();

        let mut final_width = width + x_shift;
        let mut final_height = height + y_shift;
        for sg in &layout_subgraphs {
            final_width = final_width.max(sg.x + sg.width + MARGIN);
            final_height = final_height.max(sg.y + sg.height + MARGIN);
        }
        for edge in &layout_edges {
            for &(px, py) in &edge.points {
                final_width = final_width.max(px + MARGIN);
                final_height = final_height.max(py + MARGIN);
            }
        }

        LayoutResult {
            nodes: layout_nodes,
            edges: layout_edges,
            subgraphs: layout_subgraphs,
            width: final_width,
            height: final_height,
        }
    }

    #[allow(dead_code)]
    fn assign_ranks(&mut self) {
        let back_edges = self.detect_back_edges();

        for node in self.nodes.values_mut() {
            node.rank = -1;
        }

        let mut roots: Vec<String> = self
            .nodes
            .keys()
            .filter(|id| {
                let has_non_back_incoming = self
                    .reverse_adjacency
                    .get(*id)
                    .map(|preds| {
                        preds
                            .iter()
                            .any(|pred| !back_edges.contains(&(pred.clone(), (*id).clone())))
                    })
                    .unwrap_or(false);
                !has_non_back_incoming
            })
            .cloned()
            .collect();

        if roots.is_empty() && !self.edges.is_empty() {
            roots.push(self.edges[0].from.clone());
        } else if roots.is_empty() && !self.nodes.is_empty() {
            if let Some(first) = self.nodes.keys().next().cloned() {
                roots.push(first);
            }
        }

        for root in &roots {
            if let Some(node) = self.nodes.get_mut(root) {
                node.rank = 0;
            }
        }

        let mut changed = true;
        let max_iterations = self.nodes.len() * self.edges.len().max(1);
        let mut iterations = 0;
        while changed && iterations < max_iterations {
            changed = false;
            iterations += 1;
            for edge in &self.edges.clone() {
                if back_edges.contains(&(edge.from.clone(), edge.to.clone())) {
                    continue;
                }

                let from_rank = self.nodes.get(&edge.from).map(|n| n.rank).unwrap_or(-1);
                if from_rank < 0 {
                    continue;
                }

                let candidate_rank = from_rank + 1;
                let current_rank = self.nodes.get(&edge.to).map(|n| n.rank).unwrap_or(-1);

                if candidate_rank > current_rank {
                    if let Some(to_node) = self.nodes.get_mut(&edge.to) {
                        to_node.rank = candidate_rank;
                        changed = true;
                    }
                }
            }
        }

        for node in self.nodes.values_mut() {
            if node.rank < 0 {
                node.rank = 0;
            }
        }
    }

    #[allow(dead_code)]
    fn detect_back_edges(&self) -> std::collections::HashSet<(String, String)> {
        use std::collections::HashSet;

        let mut back_edges = HashSet::new();
        let mut visited = HashSet::new();
        let mut in_stack = HashSet::new();

        let mut start_nodes: Vec<String> = self
            .nodes
            .keys()
            .filter(|id| !self.reverse_adjacency.contains_key(*id))
            .cloned()
            .collect();

        if start_nodes.is_empty() && !self.edges.is_empty() {
            start_nodes.push(self.edges[0].from.clone());
        } else if start_nodes.is_empty() && !self.nodes.is_empty() {
            let mut node_list: Vec<_> = self.nodes.keys().cloned().collect();
            node_list.sort();
            if let Some(first) = node_list.first() {
                start_nodes.push(first.clone());
            }
        }

        for start in start_nodes {
            self.dfs_detect_back_edges(&start, &mut visited, &mut in_stack, &mut back_edges);
        }

        let mut remaining: Vec<_> = self
            .nodes
            .keys()
            .filter(|id| !visited.contains(*id))
            .cloned()
            .collect();
        remaining.sort();

        for node_id in remaining {
            if !visited.contains(&node_id) {
                self.dfs_detect_back_edges(&node_id, &mut visited, &mut in_stack, &mut back_edges);
            }
        }

        back_edges
    }

    #[allow(dead_code)]
    fn dfs_detect_back_edges(
        &self,
        node: &str,
        visited: &mut std::collections::HashSet<String>,
        in_stack: &mut std::collections::HashSet<String>,
        back_edges: &mut std::collections::HashSet<(String, String)>,
    ) {
        if visited.contains(node) {
            return;
        }

        visited.insert(node.to_string());
        in_stack.insert(node.to_string());

        if let Some(neighbors) = self.adjacency.get(node) {
            for neighbor in neighbors {
                if in_stack.contains(neighbor) {
                    back_edges.insert((node.to_string(), neighbor.clone()));
                } else if !visited.contains(neighbor) {
                    self.dfs_detect_back_edges(neighbor, visited, in_stack, back_edges);
                }
            }
        }

        in_stack.remove(node);
    }

    #[allow(dead_code)]
    fn group_by_rank(&self) -> Vec<Vec<String>> {
        let max_rank = self.nodes.values().map(|n| n.rank).max().unwrap_or(0);
        let mut ranks: Vec<Vec<String>> = vec![Vec::new(); (max_rank + 1) as usize];

        for node in self.nodes.values() {
            ranks[node.rank as usize].push(node.id.clone());
        }

        for rank in &mut ranks {
            rank.sort_by_key(|id| self.nodes[id].order);
        }

        ranks
    }

    #[allow(dead_code)]
    fn compute_positions(&self, ranks: &[Vec<String>]) -> HashMap<String, (f64, f64)> {
        if ranks.is_empty() {
            return HashMap::new();
        }

        if matches!(
            self.graph.direction,
            GraphDirection::LeftToRight | GraphDirection::RightToLeft
        ) {
            return self.compute_positions_horizontal(ranks);
        }

        self.compute_positions_vertical(ranks)
    }

    #[allow(dead_code)]
    fn compute_positions_vertical(&self, ranks: &[Vec<String>]) -> HashMap<String, (f64, f64)> {
        let mut positions: HashMap<String, (f64, f64)> = HashMap::new();

        let rank_main_sizes: Vec<f64> = ranks
            .iter()
            .map(|rank_nodes| {
                rank_nodes
                    .iter()
                    .map(|node_id| {
                        let node = &self.nodes[node_id];
                        node.height
                    })
                    .fold(0.0, f64::max)
            })
            .collect();

        let mut rank_gap_extras: Vec<f64> = vec![0.0; ranks.len().saturating_sub(1)];
        for edge in &self.edges {
            let Some(label) = edge.label.as_deref() else {
                continue;
            };
            if label.is_empty() {
                continue;
            }

            let from_rank = self.nodes.get(&edge.from).map(|n| n.rank).unwrap_or(0);
            let to_rank = self.nodes.get(&edge.to).map(|n| n.rank).unwrap_or(0);

            if from_rank < 0 || to_rank < 0 {
                continue;
            }

            if from_rank >= to_rank {
                continue;
            }

            let start = from_rank as usize;
            let end = to_rank as usize;
            for gap_idx in start..end.min(rank_gap_extras.len()) {
                rank_gap_extras[gap_idx] = rank_gap_extras[gap_idx].max(EDGE_LABEL_GAP);
            }
        }

        let mut rank_main_positions: Vec<f64> = Vec::with_capacity(ranks.len());
        let mut current = MARGIN + rank_main_sizes.first().copied().unwrap_or(0.0) / 2.0;
        rank_main_positions.push(current);
        for i in 1..ranks.len() {
            let gap = RANK_SEP + rank_gap_extras.get(i - 1).copied().unwrap_or(0.0);
            current += rank_main_sizes[i - 1] / 2.0 + gap + rank_main_sizes[i] / 2.0;
            rank_main_positions.push(current);
        }

        for (rank_idx, rank_nodes) in ranks.iter().enumerate() {
            let mut rank_widths: Vec<f64> = Vec::new();

            for node_id in rank_nodes {
                let node = &self.nodes[node_id];
                rank_widths.push(node.width);
            }

            let total_cross_size: f64 =
                rank_widths.iter().sum::<f64>() + (rank_nodes.len() as f64 - 1.0) * NODE_SEP;

            let cross_offset = MARGIN - total_cross_size / 2.0;
            let rank_main = rank_main_positions[rank_idx];

            for (i, node_id) in rank_nodes.iter().enumerate() {
                let node = &self.nodes[node_id];

                let x = cross_offset
                    + rank_widths[..i].iter().sum::<f64>()
                    + i as f64 * NODE_SEP
                    + node.width / 2.0;

                positions.insert(node_id.clone(), (x, rank_main));
            }
        }

        self.normalize_positions(&mut positions);
        positions
    }

    #[allow(dead_code)]
    fn compute_positions_horizontal(&self, ranks: &[Vec<String>]) -> HashMap<String, (f64, f64)> {
        let mut positions: HashMap<String, (f64, f64)> = HashMap::new();

        let rank_main_sizes: Vec<f64> = ranks
            .iter()
            .map(|rank_nodes| {
                rank_nodes
                    .iter()
                    .map(|node_id| {
                        let node = &self.nodes[node_id];
                        node.width
                    })
                    .fold(0.0, f64::max)
            })
            .collect();

        let mut rank_gap_extras: Vec<f64> = vec![0.0; ranks.len().saturating_sub(1)];
        for edge in &self.edges {
            let Some(label) = edge.label.as_deref() else {
                continue;
            };
            if label.is_empty() {
                continue;
            }

            let from_rank = self.nodes.get(&edge.from).map(|n| n.rank).unwrap_or(0);
            let to_rank = self.nodes.get(&edge.to).map(|n| n.rank).unwrap_or(0);

            if from_rank < 0 || to_rank < 0 {
                continue;
            }

            if from_rank >= to_rank {
                continue;
            }

            let start = from_rank as usize;
            let end = to_rank as usize;
            for gap_idx in start..end.min(rank_gap_extras.len()) {
                rank_gap_extras[gap_idx] = rank_gap_extras[gap_idx].max(EDGE_LABEL_GAP);
            }
        }

        let mut rank_main_positions: Vec<f64> = Vec::with_capacity(ranks.len());
        let mut current = MARGIN + rank_main_sizes.first().copied().unwrap_or(0.0) / 2.0;
        rank_main_positions.push(current);
        for i in 1..ranks.len() {
            let gap = RANK_SEP + rank_gap_extras.get(i - 1).copied().unwrap_or(0.0);
            current += rank_main_sizes[i - 1] / 2.0 + gap + rank_main_sizes[i] / 2.0;
            rank_main_positions.push(current);
        }

        let max_cross = self.nodes.values().map(|n| n.height).fold(0.0, f64::max);
        let lane_spacing = max_cross + NODE_SEP;

        let mut rank_nodes: Vec<Vec<String>> = ranks.to_vec();
        let mut rank_map: HashMap<String, usize> = HashMap::new();
        for (ri, nodes) in rank_nodes.iter().enumerate() {
            for id in nodes {
                rank_map.insert(id.clone(), ri);
            }
        }

        let mut working_edges: Vec<(String, String)> = Vec::new();
        let mut dummy_counter: usize = 0;
        for edge in &self.edges {
            let from_rank = *rank_map.get(&edge.from).unwrap_or(&0);
            let to_rank = *rank_map.get(&edge.to).unwrap_or(&0);
            let diff = to_rank as i32 - from_rank as i32;
            if diff.abs() <= 1 {
                working_edges.push((edge.from.clone(), edge.to.clone()));
                continue;
            }
            let step: i32 = if diff > 0 { 1 } else { -1 };
            let mut current = edge.from.clone();
            let mut next_rank = from_rank as i32 + step;
            while next_rank != to_rank as i32 {
                let dummy_id = format!("__dummy_{}__", dummy_counter);
                dummy_counter += 1;
                rank_nodes[next_rank as usize].push(dummy_id.clone());
                working_edges.push((current.clone(), dummy_id.clone()));
                current = dummy_id;
                next_rank += step;
            }
            working_edges.push((current, edge.to.clone()));
        }

        for (ri, nodes) in rank_nodes.iter().enumerate() {
            for id in nodes {
                rank_map.insert(id.clone(), ri);
            }
        }

        let mut order_map: HashMap<String, usize> = HashMap::new();
        for nodes in rank_nodes.iter() {
            for (idx, id) in nodes.iter().enumerate() {
                order_map.insert(id.clone(), idx);
            }
        }

        for sweep in 0..4 {
            let left_to_right = sweep % 2 == 0;
            let rank_iter: Box<dyn Iterator<Item = usize>> = if left_to_right {
                Box::new(1..rank_nodes.len())
            } else {
                Box::new((0..rank_nodes.len() - 1).rev())
            };
            for r in rank_iter {
                let reference_rank = if left_to_right { r - 1 } else { r + 1 };
                let mut barycenters: Vec<(f64, usize, String)> = Vec::new();
                for id in rank_nodes[r].iter() {
                    let mut neighbors: Vec<usize> = Vec::new();
                    for (u, v) in working_edges.iter() {
                        let ru = *rank_map.get(u).unwrap_or(&0);
                        let rv = *rank_map.get(v).unwrap_or(&0);
                        if left_to_right {
                            if rv == r && ru == reference_rank && v == id {
                                neighbors.push(*order_map.get(u).unwrap_or(&0));
                            }
                        } else if ru == r && rv == reference_rank && u == id {
                            neighbors.push(*order_map.get(v).unwrap_or(&0));
                        }
                    }
                    let bc = if neighbors.is_empty() {
                        *order_map.get(id).unwrap_or(&0) as f64
                    } else {
                        neighbors.iter().copied().map(|v| v as f64).sum::<f64>()
                            / neighbors.len() as f64
                    };
                    let original = *order_map.get(id).unwrap_or(&0);
                    barycenters.push((bc, original, id.clone()));
                }
                barycenters.sort_by(|a, b| {
                    a.0.partial_cmp(&b.0)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| a.1.cmp(&b.1))
                });
                rank_nodes[r] = barycenters.iter().map(|(_, _, id)| id.clone()).collect();
                for (idx, id) in rank_nodes[r].iter().enumerate() {
                    order_map.insert(id.clone(), idx);
                }
            }
        }

        let mut y_local: HashMap<String, f64> = HashMap::new();
        for nodes in rank_nodes.iter() {
            for (idx, id) in nodes.iter().enumerate() {
                y_local.insert(id.clone(), idx as f64 * lane_spacing);
            }
        }

        let rank_count = ranks.len();
        let mut laplacian = vec![vec![0.0f64; rank_count]; rank_count];
        let mut b = vec![0.0f64; rank_count];
        for (u, v) in working_edges.iter() {
            let ru = *rank_map.get(u).unwrap_or(&0);
            let rv = *rank_map.get(v).unwrap_or(&0);
            let yu = *y_local.get(u).unwrap_or(&0.0);
            let yv = *y_local.get(v).unwrap_or(&0.0);
            laplacian[ru][ru] += 1.0;
            laplacian[ru][rv] -= 1.0;
            laplacian[rv][rv] += 1.0;
            laplacian[rv][ru] -= 1.0;
            let diff = yu - yv;
            b[ru] -= diff;
            b[rv] += diff;
        }

        let offsets = self.solve_rank_offsets(laplacian, b);

        for (id, node) in self.nodes.iter() {
            let r = node.rank as usize;
            let x = rank_main_positions[r];
            let y = y_local.get(id).copied().unwrap_or(0.0) + offsets[r];
            positions.insert(id.clone(), (x, y));
        }

        self.normalize_positions(&mut positions);
        positions
    }

    #[allow(dead_code, clippy::needless_range_loop)]
    fn solve_rank_offsets(&self, laplacian: Vec<Vec<f64>>, b: Vec<f64>) -> Vec<f64> {
        let n = laplacian.len();
        if n == 0 {
            return Vec::new();
        }
        if n == 1 {
            return vec![0.0];
        }

        let mut a = vec![vec![0.0f64; n - 1]; n - 1];
        let mut rhs = vec![0.0f64; n - 1];
        for i in 1..n {
            for j in 1..n {
                a[i - 1][j - 1] = laplacian[i][j];
            }
            rhs[i - 1] = b[i];
        }

        for k in 0..n - 1 {
            let pivot = a[k][k];
            if pivot.abs() < 1e-9 {
                continue;
            }
            for val in a[k].iter_mut().skip(k) {
                *val /= pivot;
            }
            rhs[k] /= pivot;
            for i in 0..n - 1 {
                if i == k {
                    continue;
                }
                let factor = a[i][k];
                for j in k..n - 1 {
                    a[i][j] -= factor * a[k][j];
                }
                rhs[i] -= factor * rhs[k];
            }
        }

        let mut offsets = vec![0.0f64; n];
        offsets[1..n].copy_from_slice(&rhs[..(n - 1)]);
        offsets
    }

    #[allow(dead_code)]
    fn compute_max_node_dimensions(&self) -> (f64, f64) {
        let max_w = self.nodes.values().map(|n| n.width).fold(0.0, f64::max);
        let max_h = self.nodes.values().map(|n| n.height).fold(0.0, f64::max);
        (max_w.max(MIN_NODE_WIDTH), max_h.max(MIN_NODE_HEIGHT))
    }

    #[allow(dead_code)]
    fn normalize_positions_and_edges(
        &self,
        positions: &mut HashMap<String, (f64, f64)>,
        edge_points: &mut EdgePointMap,
    ) {
        if positions.is_empty() {
            return;
        }

        let min_x = positions
            .values()
            .map(|(x, _)| *x)
            .fold(f64::INFINITY, f64::min);
        let min_y = positions
            .values()
            .map(|(_, y)| *y)
            .fold(f64::INFINITY, f64::min);

        let max_width = self.nodes.values().map(|n| n.width).fold(0.0, f64::max);
        let max_height = self.nodes.values().map(|n| n.height).fold(0.0, f64::max);

        let x_offset = -min_x + MARGIN + max_width / 2.0;
        let y_offset = -min_y + MARGIN + max_height / 2.0;

        for (x, y) in positions.values_mut() {
            *x += x_offset;
            *y += y_offset;
        }

        for pts in edge_points.values_mut() {
            for (x, y) in pts.iter_mut() {
                *x += x_offset;
                *y += y_offset;
            }
        }
    }

    #[allow(dead_code)]
    fn normalize_positions(&self, positions: &mut HashMap<String, (f64, f64)>) {
        if positions.is_empty() {
            return;
        }

        let min_x = positions
            .values()
            .map(|(x, _)| *x)
            .fold(f64::INFINITY, f64::min);
        let min_y = positions
            .values()
            .map(|(_, y)| *y)
            .fold(f64::INFINITY, f64::min);

        let max_width = self.nodes.values().map(|n| n.width).fold(0.0, f64::max);
        let max_height = self.nodes.values().map(|n| n.height).fold(0.0, f64::max);

        for (_id, (x, y)) in positions.iter_mut() {
            *x = *x - min_x + MARGIN + max_width / 2.0;
            *y = *y - min_y + MARGIN + max_height / 2.0;
        }
    }

    #[allow(dead_code)]
    fn separate_subgraphs(&self, positions: &mut HashMap<String, (f64, f64)>) {
        let top_level_subgraphs: Vec<&SubgraphInfo> = self
            .subgraphs
            .iter()
            .filter(|sg| sg.parent_subgraph_id.is_none())
            .collect();

        if top_level_subgraphs.len() < 2 {
            return;
        }

        let is_vertical = matches!(
            self.graph.direction,
            GraphDirection::TopToBottom | GraphDirection::BottomToTop
        );

        let subgraph_padding = 20.0;
        let title_height = 25.0;

        let subgraph_bounds: Vec<(usize, f64, f64, f64, f64, Vec<String>)> = top_level_subgraphs
            .iter()
            .enumerate()
            .filter_map(|(idx, sg)| {
                let node_ids: Vec<String> = self.get_all_nodes_in_subgraph(&sg.id);
                if node_ids.is_empty() {
                    return None;
                }
                let mut min_x = f64::INFINITY;
                let mut min_y = f64::INFINITY;
                let mut max_x = f64::NEG_INFINITY;
                let mut max_y = f64::NEG_INFINITY;
                for node_id in &node_ids {
                    if let (Some(node_info), Some(&(px, py))) =
                        (self.nodes.get(node_id), positions.get(node_id))
                    {
                        min_x = min_x.min(px - node_info.width / 2.0);
                        max_x = max_x.max(px + node_info.width / 2.0);
                        min_y = min_y.min(py - node_info.height / 2.0);
                        max_y = max_y.max(py + node_info.height / 2.0);
                    }
                }
                if min_x.is_infinite() {
                    return None;
                }
                let sg_min_x = min_x - subgraph_padding;
                let sg_min_y = min_y - subgraph_padding - title_height;
                let sg_max_x = max_x + subgraph_padding;
                let sg_max_y = max_y + subgraph_padding;
                Some((idx, sg_min_x, sg_min_y, sg_max_x, sg_max_y, node_ids))
            })
            .collect();

        let mut sorted_bounds = subgraph_bounds.clone();
        if is_vertical {
            sorted_bounds
                .sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));
        } else {
            sorted_bounds
                .sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        }

        let mut cumulative_y_shift = 0.0;
        let mut x_shifts: HashMap<usize, f64> = HashMap::new();

        for i in 1..sorted_bounds.len() {
            let prev = &sorted_bounds[i - 1];
            let curr = &sorted_bounds[i];

            let prev_min_y = prev.2 + cumulative_y_shift;
            let prev_max_y = prev.4 + cumulative_y_shift;
            let curr_min_y = curr.2;
            let curr_max_y = curr.4;

            let y_overlap_amount =
                (prev_max_y.min(curr_max_y) - prev_min_y.max(curr_min_y)).max(0.0);
            let prev_height = prev_max_y - prev_min_y;
            let curr_height = curr_max_y - curr_min_y;
            let min_height = prev_height.min(curr_height);
            let y_overlap_ratio = if min_height > 0.0 {
                y_overlap_amount / min_height
            } else {
                0.0
            };

            if is_vertical && y_overlap_ratio > 0.5 {
                let prev_x_shift = *x_shifts.get(&(i - 1)).unwrap_or(&0.0);
                let prev_right = prev.3 + prev_x_shift;
                let curr_left = curr.1;
                let x_overlap = (prev_right + SUBGRAPH_GAP) - curr_left;

                if x_overlap > 0.0 {
                    x_shifts.insert(i, x_overlap);
                    for node_id in &sorted_bounds[i].5 {
                        if let Some((x, _)) = positions.get_mut(node_id) {
                            *x += x_overlap;
                        }
                    }
                }
            } else {
                let overlap = if is_vertical {
                    let prev_bottom = prev.4 + cumulative_y_shift;
                    let curr_top = curr.2;
                    (prev_bottom + SUBGRAPH_GAP) - curr_top
                } else {
                    let prev_x_shift = *x_shifts.get(&(i - 1)).unwrap_or(&0.0);
                    let prev_right = prev.3 + prev_x_shift;
                    let curr_left = curr.1;
                    (prev_right + SUBGRAPH_GAP) - curr_left
                };

                if overlap > 0.0 {
                    if is_vertical {
                        cumulative_y_shift += overlap;
                    } else {
                        x_shifts.insert(i, overlap);
                    }
                    for node_id in &sorted_bounds[i].5 {
                        if let Some((x, y)) = positions.get_mut(node_id) {
                            if is_vertical {
                                *y += overlap;
                            } else {
                                *x += overlap;
                            }
                        }
                    }
                }
            }
        }
    }

    fn get_all_nodes_in_subgraph(&self, sg_id: &str) -> Vec<String> {
        let mut nodes: Vec<String> = self
            .node_to_subgraph
            .iter()
            .filter(|(_, id)| *id == sg_id)
            .map(|(node_id, _)| node_id.clone())
            .collect();

        for child_sg in &self.subgraphs {
            if child_sg.parent_subgraph_id.as_deref() == Some(sg_id) {
                nodes.extend(self.get_all_nodes_in_subgraph(&child_sg.id));
            }
        }

        nodes
    }

    #[allow(dead_code)]
    fn center_align_subgraphs(
        &self,
        positions: &mut HashMap<String, (f64, f64)>,
        edge_points: &mut HashMap<usize, Vec<(f64, f64)>>,
    ) {
        let top_level_subgraphs: Vec<&SubgraphInfo> = self
            .subgraphs
            .iter()
            .filter(|sg| sg.parent_subgraph_id.is_none())
            .collect();

        if top_level_subgraphs.len() < 2 {
            return;
        }

        let is_vertical = matches!(
            self.graph.direction,
            GraphDirection::TopToBottom | GraphDirection::BottomToTop
        );

        if !is_vertical {
            return;
        }

        let mut global_min_x = f64::INFINITY;
        let mut global_max_x = f64::NEG_INFINITY;
        for sg in &top_level_subgraphs {
            let node_ids = self.get_all_nodes_in_subgraph(&sg.id);
            for node_id in &node_ids {
                if let (Some(node_info), Some(&(px, _))) =
                    (self.nodes.get(node_id), positions.get(node_id))
                {
                    global_min_x = global_min_x.min(px - node_info.width / 2.0);
                    global_max_x = global_max_x.max(px + node_info.width / 2.0);
                }
            }
        }

        if global_min_x.is_infinite() {
            return;
        }

        let global_center_x = (global_min_x + global_max_x) / 2.0;

        let mut subgraph_shifts: HashMap<String, f64> = HashMap::new();

        for sg in &top_level_subgraphs {
            let node_ids = self.get_all_nodes_in_subgraph(&sg.id);
            if node_ids.is_empty() {
                continue;
            }

            let mut sg_min_x = f64::INFINITY;
            let mut sg_max_x = f64::NEG_INFINITY;
            for node_id in &node_ids {
                if let (Some(node_info), Some(&(px, _))) =
                    (self.nodes.get(node_id), positions.get(node_id))
                {
                    sg_min_x = sg_min_x.min(px - node_info.width / 2.0);
                    sg_max_x = sg_max_x.max(px + node_info.width / 2.0);
                }
            }

            if sg_min_x.is_infinite() {
                continue;
            }

            let sg_center_x = (sg_min_x + sg_max_x) / 2.0;
            let shift = global_center_x - sg_center_x;

            if shift.abs() > 0.1 {
                for node_id in &node_ids {
                    subgraph_shifts.insert(node_id.clone(), shift);
                    if let Some((x, _)) = positions.get_mut(node_id) {
                        *x += shift;
                    }
                }
            }
        }

        let mut edges_to_remove = Vec::new();
        for (idx, e) in self.edges.iter().enumerate() {
            let from_shift = subgraph_shifts.get(&e.from).copied();
            let to_shift = subgraph_shifts.get(&e.to).copied();

            if from_shift.is_none() && to_shift.is_none() {
                continue;
            }

            if from_shift == to_shift {
                if let Some(points) = edge_points.get_mut(&idx) {
                    let shift = from_shift.unwrap_or(0.0);
                    for (x, _) in points.iter_mut() {
                        *x += shift;
                    }
                }
            } else {
                edges_to_remove.push(idx);
            }
        }
        for idx in edges_to_remove {
            edge_points.remove(&idx);
        }
    }

    #[allow(dead_code)]
    fn align_nodes_within_subgraphs(
        &self,
        positions: &mut HashMap<String, (f64, f64)>,
        edge_points: &mut HashMap<usize, Vec<(f64, f64)>>,
    ) {
        let is_vertical = matches!(
            self.graph.direction,
            GraphDirection::TopToBottom | GraphDirection::BottomToTop
        );

        if !is_vertical {
            return;
        }

        for sg in &self.subgraphs {
            let node_ids: Vec<String> = self
                .node_to_subgraph
                .iter()
                .filter(|(_, sg_id)| *sg_id == &sg.id)
                .map(|(node_id, _)| node_id.clone())
                .collect();

            if node_ids.len() < 2 {
                continue;
            }

            let internal_edges: Vec<(&String, &String)> = self
                .edges
                .iter()
                .filter(|e| node_ids.contains(&e.from) && node_ids.contains(&e.to))
                .map(|e| (&e.from, &e.to))
                .collect();

            if internal_edges.is_empty() {
                continue;
            }

            let mut chains: Vec<Vec<String>> = Vec::new();
            let mut assigned: HashSet<String> = HashSet::new();

            for start_node in &node_ids {
                if assigned.contains(start_node) {
                    continue;
                }

                let mut chain = vec![start_node.clone()];
                assigned.insert(start_node.clone());

                let mut current = start_node.clone();
                loop {
                    let next = internal_edges
                        .iter()
                        .find(|(from, to)| {
                            (*from == &current && !assigned.contains(*to))
                                || (*to == &current && !assigned.contains(*from))
                        })
                        .map(|(from, to)| {
                            if *from == &current {
                                (*to).clone()
                            } else {
                                (*from).clone()
                            }
                        });

                    match next {
                        Some(next_node) => {
                            chain.push(next_node.clone());
                            assigned.insert(next_node.clone());
                            current = next_node;
                        }
                        None => break,
                    }
                }

                if chain.len() >= 2 {
                    chains.push(chain);
                }
            }

            for chain in &chains {
                let mut sum_x = 0.0;
                let mut count = 0;
                for node_id in chain {
                    if let Some(&(x, _)) = positions.get(node_id) {
                        sum_x += x;
                        count += 1;
                    }
                }

                if count > 0 {
                    let avg_x = sum_x / count as f64;
                    for node_id in chain {
                        if let Some((x, _)) = positions.get_mut(node_id) {
                            *x = avg_x;
                        }
                    }
                }
            }
        }

        for (idx, e) in self.edges.iter().enumerate() {
            let from_sg = self.node_to_subgraph.get(&e.from);
            let to_sg = self.node_to_subgraph.get(&e.to);

            if from_sg.is_some() && from_sg == to_sg {
                edge_points.remove(&idx);
            }
        }
    }

    #[allow(dead_code)]
    fn shift_external_nodes(&self, positions: &mut HashMap<String, (f64, f64)>) {
        if self.subgraphs.is_empty() {
            return;
        }

        let subgraph_padding = 20.0;
        let title_height = 25.0;
        let gap = 30.0;

        let top_level_subgraphs: Vec<&SubgraphInfo> = self
            .subgraphs
            .iter()
            .filter(|sg| sg.parent_subgraph_id.is_none())
            .collect();

        if top_level_subgraphs.is_empty() {
            return;
        }

        let mut combined_min_y = f64::INFINITY;
        let mut combined_max_y = f64::NEG_INFINITY;

        for sg in &top_level_subgraphs {
            let node_ids = self.get_all_nodes_in_subgraph(&sg.id);
            for node_id in &node_ids {
                if let (Some(node_info), Some(&(_, py))) =
                    (self.nodes.get(node_id), positions.get(node_id))
                {
                    combined_min_y = combined_min_y.min(py - node_info.height / 2.0);
                    combined_max_y = combined_max_y.max(py + node_info.height / 2.0);
                }
            }
        }

        if combined_min_y.is_infinite() {
            return;
        }

        let sg_visual_top = combined_min_y - subgraph_padding - title_height;
        let sg_visual_bottom = combined_max_y + subgraph_padding;

        let external_nodes: Vec<String> = self
            .nodes
            .keys()
            .filter(|id| !self.node_to_subgraph.contains_key(*id))
            .cloned()
            .collect();

        for node_id in &external_nodes {
            if let (Some(node_info), Some((_, py))) =
                (self.nodes.get(node_id), positions.get_mut(node_id))
            {
                let node_top = *py - node_info.height / 2.0;
                let node_bottom = *py + node_info.height / 2.0;

                if node_top < sg_visual_top && node_bottom > sg_visual_top - gap {
                    *py = sg_visual_top - gap - node_info.height / 2.0;
                } else if node_bottom > sg_visual_bottom && node_top < sg_visual_bottom + gap {
                    *py = sg_visual_bottom + gap + node_info.height / 2.0;
                } else if node_top >= sg_visual_top && node_bottom <= sg_visual_bottom {
                    if *py < (sg_visual_top + sg_visual_bottom) / 2.0 {
                        *py = sg_visual_top - gap - node_info.height / 2.0;
                    } else {
                        *py = sg_visual_bottom + gap + node_info.height / 2.0;
                    }
                }
            }
        }
    }

    fn compute_bounds(&self, positions: &HashMap<String, (f64, f64)>) -> (f64, f64) {
        if positions.is_empty() {
            return (200.0, 200.0);
        }

        let mut max_x: f64 = 0.0;
        let mut max_y: f64 = 0.0;

        for (id, (x, y)) in positions {
            let Some(node) = self.nodes.get(id) else {
                continue;
            };
            max_x = max_x.max(x + node.width / 2.0);
            max_y = max_y.max(y + node.height / 2.0);
        }

        (max_x + MARGIN, max_y + MARGIN)
    }

    fn edge_label_bounds(&self, edge: &LayoutEdge) -> Option<(f64, f64, f64, f64)> {
        let label = edge.label.as_ref()?;
        let (width, height) = self.edge_label_dimensions(label)?;
        let (x, y) = edge
            .label_pos
            .unwrap_or_else(|| Self::edge_label_midpoint(&edge.points));
        Some((x, y, width, height))
    }

    fn edge_label_midpoint(points: &[(f64, f64)]) -> (f64, f64) {
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

    fn is_back_edge(&self, from: &LayoutNode, to: &LayoutNode) -> bool {
        let dx = to.x - from.x;
        let dy = to.y - from.y;

        let is_vertical = matches!(
            self.graph.direction,
            GraphDirection::TopToBottom | GraphDirection::BottomToTop
        );

        if is_vertical {
            match self.graph.direction {
                GraphDirection::TopToBottom => dy < -10.0,
                GraphDirection::BottomToTop => dy > 10.0,
                _ => false,
            }
        } else {
            match self.graph.direction {
                GraphDirection::LeftToRight => dx < -10.0,
                GraphDirection::RightToLeft => dx > 10.0,
                _ => false,
            }
        }
    }

    fn compute_edge_points_with_obstacles(
        &self,
        from: &LayoutNode,
        to: &LayoutNode,
        all_nodes: &HashMap<String, LayoutNode>,
    ) -> Vec<(f64, f64)> {
        let dx = to.x - from.x;
        let dy = to.y - from.y;

        let is_vertical = matches!(
            self.graph.direction,
            GraphDirection::TopToBottom | GraphDirection::BottomToTop
        );

        let is_back_edge = if is_vertical {
            match self.graph.direction {
                GraphDirection::TopToBottom => dy < -10.0,
                GraphDirection::BottomToTop => dy > 10.0,
                _ => false,
            }
        } else {
            match self.graph.direction {
                GraphDirection::LeftToRight => dx < -10.0,
                GraphDirection::RightToLeft => dx > 10.0,
                _ => false,
            }
        };

        if is_back_edge {
            return self.compute_back_edge_points_simple(from, to, is_vertical);
        }

        let obstacles: Vec<&LayoutNode> = all_nodes
            .values()
            .filter(|n| n.id != from.id && n.id != to.id)
            .collect();

        if is_vertical {
            self.compute_vertical_edge_with_obstacles(from, to, &obstacles)
        } else {
            self.compute_horizontal_edge_with_obstacles(from, to, &obstacles)
        }
    }

    fn compute_horizontal_edge_with_obstacles(
        &self,
        from: &LayoutNode,
        to: &LayoutNode,
        obstacles: &[&LayoutNode],
    ) -> Vec<(f64, f64)> {
        let travel_right = to.x > from.x;
        let from_x = from.x + (from.width / 2.0) * if travel_right { 1.0 } else { -1.0 };
        let to_x = to.x + (to.width / 2.0) * if travel_right { -1.0 } else { 1.0 };

        let min_x = from_x.min(to_x);
        let max_x = from_x.max(to_x);
        let min_y = from.y.min(to.y);
        let max_y = from.y.max(to.y);

        let blocking_obstacle = obstacles.iter().find(|obs| {
            let obs_left = obs.x - obs.width / 2.0 - 10.0;
            let obs_right = obs.x + obs.width / 2.0 + 10.0;
            let obs_top = obs.y - obs.height / 2.0 - 10.0;
            let obs_bottom = obs.y + obs.height / 2.0 + 10.0;

            obs_left < max_x && obs_right > min_x && obs_top < max_y && obs_bottom > min_y
        });

        if let Some(obs) = blocking_obstacle {
            let obs_top = obs.y - obs.height / 2.0;
            let obs_bottom = obs.y + obs.height / 2.0;

            let route_above = (from.y - obs_top).abs() < (from.y - obs_bottom).abs();
            let route_y = if route_above {
                obs_top - 30.0
            } else {
                obs_bottom + 30.0
            };

            let start = self.connection_point_towards(from, from_x, route_y);
            let corner1 = (from_x, route_y);
            let corner2 = (to_x, route_y);
            let end = self.connection_point_towards(to, to_x, route_y);

            vec![start, corner1, corner2, end]
        } else {
            let mid_x = (from_x + to_x) / 2.0;
            let mid_y = to.y;
            let start = self.connection_point_towards(from, mid_x, mid_y);
            let end = self.connection_point_towards(to, mid_x, mid_y);
            let mut points = vec![start, (mid_x, mid_y), end];
            points.dedup();
            points
        }
    }

    fn compute_vertical_edge_with_obstacles(
        &self,
        from: &LayoutNode,
        to: &LayoutNode,
        obstacles: &[&LayoutNode],
    ) -> Vec<(f64, f64)> {
        let travel_down = to.y > from.y;
        let from_y = from.y + (from.height / 2.0) * if travel_down { 1.0 } else { -1.0 };
        let to_y = to.y + (to.height / 2.0) * if travel_down { -1.0 } else { 1.0 };

        let min_x = from.x.min(to.x);
        let max_x = from.x.max(to.x);
        let min_y = from_y.min(to_y);
        let max_y = from_y.max(to_y);

        let blocking_obstacle = obstacles.iter().find(|obs| {
            let obs_left = obs.x - obs.width / 2.0 - 10.0;
            let obs_right = obs.x + obs.width / 2.0 + 10.0;
            let obs_top = obs.y - obs.height / 2.0 - 10.0;
            let obs_bottom = obs.y + obs.height / 2.0 + 10.0;

            obs_left < max_x && obs_right > min_x && obs_top < max_y && obs_bottom > min_y
        });

        if let Some(obs) = blocking_obstacle {
            let obs_left = obs.x - obs.width / 2.0;
            let obs_right = obs.x + obs.width / 2.0;

            let route_left = (from.x - obs_left).abs() < (from.x - obs_right).abs();
            let route_x = if route_left {
                obs_left - 30.0
            } else {
                obs_right + 30.0
            };

            let start = self.connection_point_towards(from, route_x, from_y);
            let corner1 = (route_x, from_y);
            let corner2 = (route_x, to_y);
            let end = self.connection_point_towards(to, route_x, to_y);

            vec![start, corner1, corner2, end]
        } else {
            let mid_y = (from_y + to_y) / 2.0;
            let mid_x = to.x;
            let start = self.connection_point_towards(from, mid_x, mid_y);
            let end = self.connection_point_towards(to, mid_x, mid_y);
            let mut points = vec![start, (mid_x, mid_y), end];
            points.dedup();
            points
        }
    }

    fn compute_back_edge_points_simple(
        &self,
        from: &LayoutNode,
        to: &LayoutNode,
        is_vertical: bool,
    ) -> Vec<(f64, f64)> {
        let offset = 60.0;

        if is_vertical {
            let side_x = from.x.max(to.x) + from.width.max(to.width) / 2.0 + offset;
            let start_y = from.y;
            let end_y = to.y;

            let start = (from.x + from.width / 2.0, start_y);
            let end = (to.x + to.width / 2.0, end_y);

            Self::build_smooth_u_path(start, end, side_x, true)
        } else {
            let below_y = from.y.max(to.y) + from.height.max(to.height) / 2.0 + offset;
            let start_x = from.x;
            let end_x = to.x;

            let start = (start_x, from.y + from.height / 2.0);
            let end = (end_x, to.y + to.height / 2.0);

            Self::build_smooth_u_path(start, end, below_y, false)
        }
    }

    fn compute_back_edge_points(
        &self,
        from: &LayoutNode,
        to: &LayoutNode,
        is_vertical: bool,
        all_nodes: &HashMap<String, LayoutNode>,
    ) -> Vec<(f64, f64)> {
        let margin = 30.0;

        if is_vertical {
            let max_right = (from.x + from.width / 2.0).max(to.x + to.width / 2.0);
            let min_left = (from.x - from.width / 2.0).min(to.x - to.width / 2.0);
            let center_x = (from.x + to.x) / 2.0;
            let side_x = if from.x >= to.x {
                max_right + margin
            } else {
                min_left - margin
            };
            let start_y = from.y;
            let end_y = to.y;

            let start = if side_x > center_x {
                (from.x + from.width / 2.0, start_y)
            } else {
                (from.x - from.width / 2.0, start_y)
            };
            let end = if side_x > center_x {
                (to.x + to.width / 2.0, end_y)
            } else {
                (to.x - to.width / 2.0, end_y)
            };

            Self::build_smooth_u_path(start, end, side_x, true)
        } else {
            // Compute both above and below routing options, pick the closer one.
            let min_x = from.x.min(to.x);
            let max_x = from.x.max(to.x);
            let mut max_bottom = from.y + from.height / 2.0;
            max_bottom = max_bottom.max(to.y + to.height / 2.0);
            let mut min_top = from.y - from.height / 2.0;
            min_top = min_top.min(to.y - to.height / 2.0);
            for node in all_nodes.values() {
                let node_left = node.x - node.width / 2.0;
                let node_right = node.x + node.width / 2.0;
                if node_right >= min_x - margin && node_left <= max_x + margin {
                    max_bottom = max_bottom.max(node.y + node.height / 2.0);
                    min_top = min_top.min(node.y - node.height / 2.0);
                }
            }
            let below_y = max_bottom + margin;
            let above_y = min_top - margin;
            let center_y = (from.y + to.y) / 2.0;
            let route_y = if (below_y - center_y).abs() <= (above_y - center_y).abs() {
                below_y
            } else {
                above_y
            };
            let start_x = from.x;
            let end_x = to.x;

            let start = if route_y > center_y {
                (start_x, from.y + from.height / 2.0)
            } else {
                (start_x, from.y - from.height / 2.0)
            };
            let end = if route_y > center_y {
                (end_x, to.y + to.height / 2.0)
            } else {
                (end_x, to.y - to.height / 2.0)
            };

            Self::build_smooth_u_path(start, end, route_y, false)
        }
    }

    fn straighten_if_aligned(
        &self,
        dagre_points: &[(f64, f64)],
        from: &LayoutNode,
        to: &LayoutNode,
        is_vertical: bool,
        all_nodes: &HashMap<String, LayoutNode>,
    ) -> Vec<(f64, f64)> {
        let tolerance = 15.0;
        let are_aligned = if is_vertical {
            (from.x - to.x).abs() < tolerance
        } else {
            (from.y - to.y).abs() < tolerance
        };
        if are_aligned && dagre_points.len() >= 2 {
            let start = dagre_points.first().copied().unwrap_or((from.x, from.y));
            let end = dagre_points.last().copied().unwrap_or((to.x, to.y));
            let candidate = if is_vertical {
                let avg_x = (from.x + to.x) / 2.0;
                vec![(avg_x, start.1), (avg_x, end.1)]
            } else {
                let avg_y = (from.y + to.y) / 2.0;
                vec![(start.0, avg_y), (end.0, avg_y)]
            };
            // Check if straightening would cause the edge to cross through any node
            if self.edge_crosses_any_node(&candidate, from, to, all_nodes) {
                dagre_points.to_vec()
            } else {
                candidate
            }
        } else {
            dagre_points.to_vec()
        }
    }

    /// Returns true if the edge path (approximated as a straight line between
    /// first and last points) would cross through any node other than from/to.
    fn edge_crosses_any_node(
        &self,
        points: &[(f64, f64)],
        from: &LayoutNode,
        to: &LayoutNode,
        all_nodes: &HashMap<String, LayoutNode>,
    ) -> bool {
        if points.len() < 2 {
            return false;
        }
        let (x1, y1) = points[0];
        let (x2, y2) = points[points.len() - 1];
        let margin = 5.0;
        for node in all_nodes.values() {
            if node.id == from.id || node.id == to.id {
                continue;
            }
            let left = node.x - node.width / 2.0 - margin;
            let right = node.x + node.width / 2.0 + margin;
            let top = node.y - node.height / 2.0 - margin;
            let bottom = node.y + node.height / 2.0 + margin;
            if Self::line_intersects_rect((x1, y1), (x2, y2), (left, top), (right, bottom)) {
                return true;
            }
        }
        false
    }

    /// Tests if a line segment intersects an axis-aligned rectangle.
    fn line_intersects_rect(
        p1: (f64, f64),
        p2: (f64, f64),
        rect_min: (f64, f64),
        rect_max: (f64, f64),
    ) -> bool {
        let (x1, y1) = p1;
        let (x2, y2) = p2;
        let (left, top) = rect_min;
        let (right, bottom) = rect_max;
        // If both endpoints are on the same side of any edge, no intersection
        if (x1 < left && x2 < left)
            || (x1 > right && x2 > right)
            || (y1 < top && y2 < top)
            || (y1 > bottom && y2 > bottom)
        {
            return false;
        }
        // If either endpoint is inside the rect, intersection
        if x1 >= left && x1 <= right && y1 >= top && y1 <= bottom {
            return true;
        }
        if x2 >= left && x2 <= right && y2 >= top && y2 <= bottom {
            return true;
        }
        // Check intersection with each rect edge
        let dx = x2 - x1;
        let dy = y2 - y1;
        let edges: [(f64, f64, f64, f64); 4] = [
            (left, top, left, bottom),     // left edge
            (right, top, right, bottom),   // right edge
            (left, top, right, top),       // top edge
            (left, bottom, right, bottom), // bottom edge
        ];
        for (ex1, ey1, ex2, ey2) in edges {
            let edx = ex2 - ex1;
            let edy = ey2 - ey1;
            let denom = dx * edy - dy * edx;
            if denom.abs() < 1e-10 {
                continue; // parallel
            }
            let t = ((ex1 - x1) * edy - (ey1 - y1) * edx) / denom;
            let u = ((ex1 - x1) * dy - (ey1 - y1) * dx) / denom;
            if (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u) {
                return true;
            }
        }
        false
    }

    fn build_smooth_u_path(
        start: (f64, f64),
        end: (f64, f64),
        route_coord: f64,
        is_vertical: bool,
    ) -> Vec<(f64, f64)> {
        if is_vertical {
            let side_x = route_coord;
            let total_height = (start.1 - end.1).abs();
            let curve_fraction = 0.3;
            let curve_height = total_height * curve_fraction;
            let mid_y = (start.1 + end.1) / 2.0;
            let top_curve_end_y = start.1 - curve_height;
            let bottom_curve_start_y = end.1 + curve_height;
            vec![
                start,
                (start.0, start.1 - curve_height * 0.33),
                (side_x, top_curve_end_y + curve_height * 0.33),
                (side_x, top_curve_end_y),
                (side_x, mid_y),
                (side_x, bottom_curve_start_y),
                (side_x, bottom_curve_start_y - curve_height * 0.33),
                (end.0, end.1 + curve_height * 0.33),
                end,
            ]
        } else {
            let below_y = route_coord;
            let total_width = (start.0 - end.0).abs();
            let curve_fraction = 0.3;
            let curve_width = total_width * curve_fraction;
            let mid_x = (start.0 + end.0) / 2.0;
            let left_curve_end_x = start.0 - curve_width;
            let right_curve_start_x = end.0 + curve_width;
            vec![
                start,
                (start.0 - curve_width * 0.33, start.1),
                (left_curve_end_x + curve_width * 0.33, below_y),
                (left_curve_end_x, below_y),
                (mid_x, below_y),
                (right_curve_start_x, below_y),
                (right_curve_start_x - curve_width * 0.33, below_y),
                (end.0 + curve_width * 0.33, end.1),
                end,
            ]
        }
    }

    /// Drops route points that fall inside a cluster endpoint's rect so the edge
    /// approaches the cluster boundary from outside, instead of diving toward the
    /// interior member node dagre routed to (which the clip then curled back). One
    /// transition point is kept on each trimmed side for `clip_edge_to_boundaries`
    /// to place on the boundary; the polyline is never reduced below two points.
    fn trim_cluster_interior_points(
        points: &mut Vec<(f64, f64)>,
        from_node: &LayoutNode,
        to_node: &LayoutNode,
        from_is_cluster: bool,
        to_is_cluster: bool,
    ) {
        let inside = |node: &LayoutNode, (px, py): (f64, f64)| {
            let hw = node.width / 2.0;
            let hh = node.height / 2.0;
            px > node.x - hw && px < node.x + hw && py > node.y - hh && py < node.y + hh
        };

        if to_is_cluster && points.len() > 2 {
            if let Some(last_out) = points.iter().rposition(|&p| !inside(to_node, p)) {
                // Keep through the last outside point plus one interior transition.
                // last_out >= 0 and len > 2, so keep >= 2 (never empties the line).
                let keep = (last_out + 2).min(points.len());
                points.truncate(keep);
            }
        }

        if from_is_cluster && points.len() > 2 {
            if let Some(first_out) = points.iter().position(|&p| !inside(from_node, p)) {
                // Drop leading interior points, keeping one transition before it.
                // first_out <= len-1, so at least two points always remain.
                let drop = first_out.saturating_sub(1);
                if drop > 0 {
                    points.drain(0..drop);
                }
            }
        }
    }

    fn clip_edge_to_boundaries(
        &self,
        points: &mut [(f64, f64)],
        from_node: &LayoutNode,
        to_node: &LayoutNode,
    ) {
        if points.len() < 2 {
            return;
        }
        let second_point = points.get(1).copied().unwrap_or(points[0]);
        let new_start = self.connection_point_on_node(from_node, second_point.0, second_point.1);
        points[0] = new_start;
        self.clip_edge_end_only(points, to_node);
    }

    fn clip_edge_end_only(&self, points: &mut [(f64, f64)], to_node: &LayoutNode) {
        if points.len() < 2 {
            return;
        }
        let len = points.len();
        let second_last = points
            .get(len.saturating_sub(2))
            .copied()
            .unwrap_or(points[len - 1]);
        let new_end = self.connection_point_on_node(to_node, second_last.0, second_last.1);
        points[len - 1] = new_end;
    }

    fn connection_point_on_node(&self, node: &LayoutNode, from_x: f64, from_y: f64) -> (f64, f64) {
        let dx = node.x - from_x;
        let dy = node.y - from_y;

        match node.shape {
            NodeShape::Circle | NodeShape::StartState | NodeShape::EndState => {
                let r = node.width.min(node.height) / 2.0;
                let len = (dx * dx + dy * dy).sqrt();
                if len == 0.0 {
                    return (node.x, node.y - r);
                }
                (node.x - r * dx / len, node.y - r * dy / len)
            }
            NodeShape::Diamond => {
                let hw = node.width / 2.0;
                let hh = node.height / 2.0;
                let denom = dx.abs() / hw + dy.abs() / hh;
                if denom == 0.0 {
                    return (node.x, node.y - hh);
                }
                let t = 1.0 / denom;
                (node.x - dx * t, node.y - dy * t)
            }
            _ => {
                let hw = node.width / 2.0;
                let hh = node.height / 2.0;
                let denom_x = if hw > 0.0 { dx.abs() / hw } else { 0.0 };
                let denom_y = if hh > 0.0 { dy.abs() / hh } else { 0.0 };
                let denom = denom_x.max(denom_y);
                if denom == 0.0 {
                    return (node.x, node.y - hh);
                }
                let t = 1.0 / denom;
                (node.x - dx * t, node.y - dy * t)
            }
        }
    }

    fn connection_point_towards(
        &self,
        node: &LayoutNode,
        target_x: f64,
        target_y: f64,
    ) -> (f64, f64) {
        let dx = target_x - node.x;
        let dy = target_y - node.y;

        match node.shape {
            NodeShape::Circle | NodeShape::StartState | NodeShape::EndState => {
                let r = node.width.min(node.height) / 2.0;
                let len = (dx * dx + dy * dy).sqrt();
                if len == 0.0 {
                    return (node.x + r, node.y);
                }

                (node.x + r * dx / len, node.y + r * dy / len)
            }
            NodeShape::Diamond => {
                let hw = node.width / 2.0;
                let hh = node.height / 2.0;

                let denom = dx.abs() / hw + dy.abs() / hh;
                if denom == 0.0 {
                    return (node.x + hw, node.y);
                }

                let t = 1.0 / denom;
                (node.x + dx * t, node.y + dy * t)
            }
            _ => {
                let hw = node.width / 2.0;
                let hh = node.height / 2.0;

                let denom_x = if hw > 0.0 { dx.abs() / hw } else { 0.0 };
                let denom_y = if hh > 0.0 { dy.abs() / hh } else { 0.0 };
                let denom = denom_x.max(denom_y);
                if denom == 0.0 {
                    return (node.x + hw, node.y);
                }

                let t = 1.0 / denom;
                (node.x + dx * t, node.y + dy * t)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect_node(x: f64, y: f64, w: f64, h: f64) -> LayoutNode {
        LayoutNode {
            id: String::new(),
            x,
            y,
            width: w,
            height: h,
            shape: NodeShape::Rectangle,
            label: String::new(),
            fill_color: None,
            stroke_color: None,
        }
    }

    // Cluster rect centered (100,100), 100x100 => interior is (50,150)x(50,150).
    fn cluster() -> LayoutNode {
        rect_node(100.0, 100.0, 100.0, 100.0)
    }

    fn outside_node() -> LayoutNode {
        rect_node(0.0, 0.0, 10.0, 10.0)
    }

    #[test]
    fn trim_drops_interior_points_to_cluster_keeping_one_transition() {
        // Two trailing points are inside the destination cluster rect.
        let mut points = vec![(0.0, 0.0), (40.0, 40.0), (100.0, 100.0), (110.0, 110.0)];
        LayoutEngine::trim_cluster_interior_points(
            &mut points,
            &outside_node(),
            &cluster(),
            false,
            true,
        );
        // They collapse to a single interior transition point for the clip.
        assert_eq!(points, vec![(0.0, 0.0), (40.0, 40.0), (100.0, 100.0)]);
    }

    #[test]
    fn trim_never_drops_below_two_points() {
        // Only the first point is outside; everything else is interior.
        let mut points = vec![(0.0, 0.0), (100.0, 100.0), (110.0, 110.0), (120.0, 120.0)];
        LayoutEngine::trim_cluster_interior_points(
            &mut points,
            &outside_node(),
            &cluster(),
            false,
            true,
        );
        assert_eq!(points, vec![(0.0, 0.0), (100.0, 100.0)]);
    }

    #[test]
    fn trim_is_noop_when_all_points_outside_cluster() {
        // A monotonic approach that never enters the rect is left untouched.
        let original = vec![(0.0, 0.0), (20.0, 20.0), (40.0, 40.0)];
        let mut points = original.clone();
        LayoutEngine::trim_cluster_interior_points(
            &mut points,
            &outside_node(),
            &cluster(),
            false,
            true,
        );
        assert_eq!(points, original);
    }

    #[test]
    fn trim_drops_interior_points_from_cluster_keeping_one_transition() {
        // Source is the cluster: leading interior points collapse to one.
        let mut points = vec![(110.0, 110.0), (100.0, 100.0), (40.0, 40.0), (0.0, 0.0)];
        LayoutEngine::trim_cluster_interior_points(
            &mut points,
            &cluster(),
            &outside_node(),
            true,
            false,
        );
        assert_eq!(points, vec![(100.0, 100.0), (40.0, 40.0), (0.0, 0.0)]);
    }

    #[test]
    fn trim_both_endpoints_cluster_trims_each_end_keeping_transition() {
        // Source cluster centered (100,100); destination cluster centered
        // (400,400). The route enters the source and the destination rects.
        let source = cluster();
        let dest = rect_node(400.0, 400.0, 100.0, 100.0);
        let mut points = vec![
            (110.0, 110.0), // inside source
            (130.0, 130.0), // inside source
            (200.0, 200.0), // outside both
            (300.0, 300.0), // outside both
            (380.0, 380.0), // inside dest
            (400.0, 400.0), // inside dest
        ];
        LayoutEngine::trim_cluster_interior_points(&mut points, &source, &dest, true, true);
        // Both ends are trimmed to a single interior transition; >= 2 points.
        assert_eq!(
            points,
            vec![
                (130.0, 130.0),
                (200.0, 200.0),
                (300.0, 300.0),
                (380.0, 380.0)
            ]
        );
    }

    #[test]
    fn trim_treats_on_boundary_point_as_outside() {
        // (150,100) lies exactly on the cluster's right edge (x = 100 + 100/2);
        // the strictly-inside predicate treats it as outside, so it is retained.
        let mut points = vec![(0.0, 0.0), (150.0, 100.0), (110.0, 110.0)];
        LayoutEngine::trim_cluster_interior_points(
            &mut points,
            &outside_node(),
            &cluster(),
            false,
            true,
        );
        assert_eq!(points, vec![(0.0, 0.0), (150.0, 100.0), (110.0, 110.0)]);
    }
}
