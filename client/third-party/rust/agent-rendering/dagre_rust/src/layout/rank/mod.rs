pub mod feasible_tree;
pub mod network_simplex;
pub mod util;

use crate::layout::rank::feasible_tree::feasible_tree;
use crate::layout::rank::network_simplex::network_simplex;
use crate::layout::rank::util::longest_path;
use crate::{GraphConfig, GraphEdge, GraphNode};
use graphlib_rust::Graph;

/*
 * Assigns a rank to each node in the input graph that respects the "minlen"
 * constraint specified on edges between nodes.
 *
 * This basic structure is derived from Gansner, et al., "A Technique for
 * Drawing Directed Graphs."
 *
 * Pre-conditions:
 *
 *    1. Graph must be a connected DAG
 *    2. Graph nodes must be objects
 *    3. Graph edges must have "weight" and "minlen" attributes
 *
 * Post-conditions:
 *
 *    1. Graph nodes will have a "rank" attribute based on the results of the
 *       algorithm. Ranks can start at any index (including negative), we'll
 *       fix them up later.
 */

pub fn rank(g: &mut Graph<GraphConfig, GraphNode, GraphEdge>) {
    let _ranker = g.graph().ranker.clone();
    match _ranker {
        Some(ranker) => {
            let ranker_str = &*ranker;
            if ranker_str == "network-simplex" {
                network_simplex(g);
            } else if ranker_str == "tight-tree" {
                tight_tree_ranker(g);
            } else if ranker_str == "longest-path" {
                longest_path(g);
            }
        }
        _ => {
            network_simplex(g);
        }
    }
}

fn tight_tree_ranker(g: &mut Graph<GraphConfig, GraphNode, GraphEdge>) {
    longest_path(g);
    feasible_tree(g);
}
