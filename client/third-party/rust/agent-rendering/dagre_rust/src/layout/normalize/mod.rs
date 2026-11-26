use crate::layout::util::add_dummy_node;
use crate::{GraphConfig, GraphEdge, GraphEdgePoint, GraphNode};
use graphlib_rust::{Edge, Graph};
/*
 * Breaks any long edges in the graph into short segments that span 1 layer
 * each. This operation is undoable with the denormalize function.
 *
 * Pre-conditions:
 *
 *    1. The input graph is a DAG.
 *    2. Each node in the graph has a "rank" property.
 *
 * Post-condition:
 *
 *    1. All edges in the graph have a length of 1.
 *    2. Dummy nodes are added where edges have been split into segments.
 *    3. The graph is augmented with a "dummyChains" attribute which contains
 *       the first dummy in each chain of dummy nodes produced.
 */

pub fn run(g: &mut Graph<GraphConfig, GraphNode, GraphEdge>) {
    g.graph_mut().dummy_chains = Some(vec![]);
    let edges = g.edges();
    for edge_obj in edges.into_iter() {
        normalize_edge(g, &edge_obj);
    }
}

fn normalize_edge(g: &mut Graph<GraphConfig, GraphNode, GraphEdge>, e: &Edge) {
    let mut v = e.v.clone();
    let w = e.w.clone();
    let mut v_rank = g
        .node(&v)
        .unwrap_or(&GraphNode::default())
        .rank
        .clone()
        .unwrap_or(0);
    let w_rank = g
        .node(&w)
        .unwrap_or(&GraphNode::default())
        .rank
        .clone()
        .unwrap_or(0);
    // let name = e.name.clone(); // TODO: it was creating error for multi-graph option
    let Some(edge_label) = g.edge_mut_with_obj(&e) else {
        return;
    };
    edge_label.points = Some(vec![]);
    let weight = edge_label.weight.clone();
    let label_rank = edge_label.label_rank.unwrap_or(0);

    if w_rank == v_rank + 1 {
        return ();
    }

    let _edge_label = edge_label.clone();
    g.remove_edge_with_obj(&e);

    let mut i = 0;
    v_rank += 1;
    while v_rank < w_rank {
        let mut attrs = GraphNode::default();
        attrs.edge_label = Some(_edge_label.clone());
        attrs.edge_obj = Some(e.clone());
        attrs.rank = Some(v_rank.clone());
        let mut dummy_type = "edge".to_string();
        if v_rank == label_rank {
            attrs.width = _edge_label.width.clone().unwrap_or(0.0);
            attrs.height = _edge_label.height.clone().unwrap_or(0.0);
            attrs.labelpos = _edge_label.labelpos.clone();
            dummy_type = "edge-label".to_string();
        }
        let dummy = add_dummy_node(g, dummy_type, attrs, "_d".to_string());
        let mut dummy_edge_label = GraphEdge::default();
        dummy_edge_label.weight = weight.clone();
        let _ = g.set_edge(&v, &dummy, Some(dummy_edge_label), None); // remove name from here
        if i == 0 {
            let graph_label = g.graph_mut();
            if graph_label.dummy_chains.is_none() {
                graph_label.dummy_chains = Some(vec![]);
            }
            let dummy_chains = graph_label.dummy_chains.as_mut().unwrap();
            dummy_chains.push(dummy.clone());
        }
        v = dummy.clone();
        i += 1;
        v_rank += 1;
    }

    let mut graph_edge = GraphEdge::default();
    graph_edge.weight = weight;
    let _ = g.set_edge(
        &v,
        &w,
        Some(graph_edge),
        None, // removed name from here
    );
}

pub fn undo(g: &mut Graph<GraphConfig, GraphNode, GraphEdge>) {
    if g.graph().dummy_chains.is_none() {
        return ();
    }
    let dummy_chains = g.graph().dummy_chains.clone().unwrap();
    for v_ in dummy_chains.iter() {
        let node_ = g.node(v_);
        if node_.is_none() {
            continue;
        }
        let mut node = node_.cloned().unwrap();
        let mut orig_label = node.edge_label.clone().unwrap_or(GraphEdge::default());
        let edge_obj = node.edge_obj.unwrap();
        let mut v = v_.clone();
        while node.dummy.is_some() {
            let sucs = g.successors(&v).unwrap_or(vec![]);
            let default_w = "".to_string();
            let w = sucs.first().unwrap_or(&default_w);
            g.remove_node(&v);
            let points = orig_label.points.as_mut().unwrap();
            points.push(GraphEdgePoint {
                x: node.x.clone(),
                y: node.y.clone(),
            });
            if node.dummy.as_ref().unwrap() == "edge-label" {
                orig_label.x = node.x.clone();
                orig_label.y = node.y.clone();
                orig_label.width = Some(node.width.clone());
                orig_label.height = Some(node.height.clone());
            }
            v = w.clone();
            node = g.node(&v).cloned().unwrap();
        }
        let _ = g.set_edge_with_obj(&edge_obj, Some(orig_label));
    }
}
