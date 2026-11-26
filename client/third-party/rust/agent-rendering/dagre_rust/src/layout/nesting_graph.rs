use crate::layout::{util, GraphConfig, GraphEdge, GraphNode};
use graphlib_rust::graph::GRAPH_NODE;
use graphlib_rust::Graph;
use ordered_hashmap::OrderedHashMap;

/*
 * A nesting graph creates dummy nodes for the tops and bottoms of subgraphs,
 * adds appropriate edges to ensure that all cluster nodes are placed between
 * these boundries, and ensures that the graph is connected.
 *
 * In addition we ensure, through the use of the minlen property, that nodes
 * and subgraph border nodes to not end up on the same rank.
 *
 * Preconditions:
 *
 *    1. Input graph is a DAG
 *    2. Nodes in the input graph has a minlen attribute
 *
 * Postconditions:
 *
 *    1. Input graph is connected.
 *    2. Dummy nodes are added for the tops and bottoms of subgraphs.
 *    3. The minlen attribute for nodes is adjusted to ensure nodes do not
 *       get placed on the same rank as subgraph border nodes.
 *
 * The nesting graph idea comes from Sander, "Layout of Compound Directed
 * Graphs."
 */
pub fn run(graph: &mut Graph<GraphConfig, GraphNode, GraphEdge>) {
    let graph_node = GraphNode::default();
    let root = util::add_dummy_node(graph, "root".to_string(), graph_node, "_root".to_string());
    let depths = tree_depths(graph);
    let mut height: usize = 0;
    for depth in depths.values() {
        if depth > &height {
            height = depth.to_owned();
        }
    }
    if height > 0 {
        height -= 1;
    }

    let node_sep = (2 * height + 1) as f32;
    graph.graph_mut().nesting_root = Some(root.clone());

    // Multiply minlen by nodeSep to align nodes on non-border ranks.
    let edge_objs = graph.edges();
    for edge_obj in edge_objs.into_iter() {
        let _edge_label = graph.edge_mut_with_obj(&edge_obj);
        if _edge_label.is_none() {
            continue;
        }
        let edge_label = _edge_label.unwrap();
        edge_label.minlen = Some(edge_label.minlen.unwrap_or(1.0) * node_sep);
    }

    // Calculate a weight that is sufficient to keep subgraphs vertically compact
    let weight = sum_weights(graph) + 1.0;

    // Create border nodes and link them up
    let children = graph.children(&GRAPH_NODE.to_string());
    for child_id in children.into_iter() {
        dfs(
            graph, &root, &node_sep, &weight, &height, &depths, &child_id,
        );
    }

    // Save the multiplier for node layers for later removal of empty border
    // layers.
    graph.graph_mut().node_rank_factor = Some(node_sep);
}

fn tree_depths(graph: &Graph<GraphConfig, GraphNode, GraphEdge>) -> OrderedHashMap<String, usize> {
    let mut depths: OrderedHashMap<String, usize> = OrderedHashMap::new();

    fn dfs(
        node_id: String,
        depth: usize,
        depths: &mut OrderedHashMap<String, usize>,
        graph: &Graph<GraphConfig, GraphNode, GraphEdge>,
    ) {
        let children = graph.children(&node_id);
        for child_id in children.iter() {
            // recursion for child node ids
            dfs(child_id.clone(), depth + 1, depths, graph);
        }
        // setting for current node
        depths.insert(node_id.clone(), depth);
    }

    // processing root nodes
    for node_id in graph.children(&GRAPH_NODE.to_string()) {
        dfs(node_id, 1, &mut depths, graph);
    }
    return depths;
}

fn dfs(
    graph: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
    root: &String,
    node_sep: &f32,
    weight: &f32,
    height: &usize,
    depths: &OrderedHashMap<String, usize>,
    node_id: &String,
) {
    let children = graph.children(node_id);
    if children.len() == 0 {
        if node_id != root {
            let mut graph_edge = GraphEdge::default();
            graph_edge.minlen = Some(node_sep.clone());
            graph_edge.weight = Some(0.0);
            let _ = graph.set_edge(&root, &node_id, Some(graph_edge), None);
        }
        return ();
    }

    let top = util::add_border_node(graph, "_bt", None, None);
    let bottom = util::add_border_node(graph, "_bb", None, None);

    let _pt = graph.set_parent(&top, Some(node_id.clone()));
    let _pb = graph.set_parent(&bottom, Some(node_id.clone()));

    let _label = graph.node_mut(node_id);
    if let Some(label) = _label {
        label.border_top = Some(top.clone());
        label.border_bottom = Some(bottom.clone());
    }

    for child_id in children.into_iter() {
        dfs(graph, root, node_sep, weight, height, depths, &child_id);

        let _child_node = graph.node(&child_id);
        if _child_node.is_none() {
            continue;
        }

        let child_node = _child_node.unwrap();
        let border_top = child_node.border_top.clone();
        let border_bottom = child_node.border_bottom.clone();

        let mut child_top = child_id.clone();
        let mut child_bottom = child_id.clone();
        let mut this_weight: f32 = 2.0 * weight.clone();
        let mut minlen: usize = 1;

        if border_top.is_some() {
            child_top = border_top.clone().unwrap();
        }
        if border_bottom.is_some() {
            child_bottom = border_bottom.unwrap();
        }
        if border_top.is_some() {
            this_weight = weight.clone();
        }
        if child_top == child_bottom {
            minlen = height - depths.get(node_id).cloned().unwrap_or(0) + 1;
        }

        let mut _ct_graph_edge = GraphEdge::default();
        _ct_graph_edge.minlen = Some(minlen.clone() as f32);
        _ct_graph_edge.weight = Some(this_weight.clone());
        _ct_graph_edge.nesting_edge = Some(true);
        let _ct = graph.set_edge(&top, &child_top, Some(_ct_graph_edge), None);

        let mut _cb_graph_edge = GraphEdge::default();
        _cb_graph_edge.minlen = Some(minlen.clone() as f32);
        _cb_graph_edge.weight = Some(this_weight.clone());
        _cb_graph_edge.nesting_edge = Some(true);
        let _cb = graph.set_edge(&child_bottom, &bottom, Some(_cb_graph_edge), None);
    }

    if graph.parent(node_id).is_none() {
        let mut graph_edge = GraphEdge::default();
        graph_edge.minlen =
            Some((depths.get(node_id).cloned().unwrap_or(0) + height.clone()) as f32);
        graph_edge.weight = Some(0.0);
        graph_edge.nesting_edge = Some(true);
        let _ = graph.set_edge(&root, &top, Some(graph_edge), None);
    }
}

fn sum_weights(graph: &Graph<GraphConfig, GraphNode, GraphEdge>) -> f32 {
    let mut total_weights: f32 = 0.0;

    for edge in graph.edges() {
        if let Some(edge_label) = graph.edge_with_obj(&edge) {
            if let Some(weight) = edge_label.weight {
                total_weights += weight;
            }
        }
    }

    return total_weights;
}

pub fn cleanup(graph: &mut Graph<GraphConfig, GraphNode, GraphEdge>) {
    let graph_label = graph.graph();
    if graph_label.nesting_root.is_some() {
        graph.remove_node(&graph_label.nesting_root.clone().unwrap());
    }
    graph.graph_mut().nesting_root = None;
    // removing nesting edge
    let edges = graph.edges();
    for edge in edges.into_iter() {
        let _edge_label = graph.edge_with_obj(&edge);
        if let Some(edge_label) = _edge_label {
            if edge_label.nesting_edge.clone().unwrap_or(false) {
                graph.remove_edge_with_obj(&edge);
            }
        }
    }
}
