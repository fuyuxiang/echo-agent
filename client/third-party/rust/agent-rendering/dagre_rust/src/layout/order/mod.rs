pub mod add_subgraph_constraints;
pub mod barycenter;
pub mod build_layer_graph;
pub mod cross_count;
pub mod init_order;
pub mod resolve_conflicts;
pub mod sort;
pub mod sort_subgraph;

use crate::layout::order::add_subgraph_constraints::add_subgraph_constraints;
use crate::layout::order::build_layer_graph::{build_layer_graph, GraphRelationship};
use crate::layout::order::cross_count::cross_count;
use crate::layout::order::init_order::init_order;
use crate::layout::order::sort_subgraph::sort_subgraph;
use crate::layout::util;
use crate::{GraphConfig, GraphEdge, GraphNode};
use graphlib_rust::graph::GRAPH_NODE;
use graphlib_rust::Graph;

/*
 * Applies heuristics to minimize edge crossings in the graph and sets the best
 * order solution as an order attribute on each node.
 *
 * Pre-conditions:
 *
 *    1. Graph must be DAG
 *    2. Graph nodes must be objects with a "rank" attribute
 *    3. Graph edges must have the "weight" attribute
 *
 * Post-conditions:
 *
 *    1. Graph nodes will have an "order" attribute based on the results of the
 *       algorithm.
 */

pub fn order(g: &mut Graph<GraphConfig, GraphNode, GraphEdge>) {
    let max_rank = util::max_rank(g);
    let down_layer_ranks: Vec<i32> = (1..=max_rank).collect();
    let up_layer_ranks: Vec<i32> = (0..max_rank).rev().collect();

    let mut layering = init_order(g);
    assign_order(g, &layering);

    // Start with the init ordering as the best candidate.
    // The original dagre.js code starts with bestCC = Infinity, which means
    // the first sweep's result always replaces it. However, if the init
    // ordering already has minimal crossings, using it as the starting point
    // ensures the sweep loop doesn't accidentally select a different ordering
    // with the same crossing count but worse visual layout (e.g., a mirrored
    // ordering produced by a biased sweep).
    let init_cc = cross_count(g, &layering) as f64;
    let mut best_cc = init_cc;
    let mut best: Vec<Vec<String>> = layering.clone();

    let mut i = 0;
    let mut last_best = 0;
    while last_best < 4 {
        if i % 2 != 0 {
            sweep_layer_graphs(g, &down_layer_ranks, GraphRelationship::InEdges, i % 4 >= 2);
        } else {
            sweep_layer_graphs(g, &up_layer_ranks, GraphRelationship::OutEdges, i % 4 >= 2);
        }

        layering = util::build_layer_matrix(g);
        let cc = cross_count(g, &layering) as f64;
        if cc < best_cc {
            last_best = 0;
            best = layering.clone();
            best_cc = cc;
        }

        last_best += 1;
        i += 1;
    }

    assign_order(g, &best);
}

fn sweep_layer_graphs(
    g: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
    ranks: &Vec<i32>,
    relationship: GraphRelationship,
    bias_right: bool,
) {
    let mut cg: Graph<GraphConfig, GraphNode, GraphEdge> = Graph::new(None);

    ranks.iter().for_each(|rank| {
        let lg = build_layer_graph(g, rank, relationship);
        let root = lg.graph().root.clone().unwrap_or(GRAPH_NODE.to_string());
        let sorted = sort_subgraph(&lg, &root, &cg, &bias_right);
        sorted.vs.iter().enumerate().for_each(|(i, v)| {
            if let Some(node) = g.node_mut(v) {
                node.order = Some(i);
            }
        });
        add_subgraph_constraints(&lg, &mut cg, &sorted.vs);
    });
}

fn assign_order(g: &mut Graph<GraphConfig, GraphNode, GraphEdge>, layering: &Vec<Vec<String>>) {
    for layer in layering {
        for (i, v) in layer.iter().enumerate() {
            let node_label = g.node_mut(v).unwrap();
            node_label.order = Some(i);
        }
    }
}
