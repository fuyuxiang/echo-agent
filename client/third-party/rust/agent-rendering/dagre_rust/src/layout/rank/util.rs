use crate::{GraphConfig, GraphEdge, GraphNode};
use graphlib_rust::{Edge, Graph};
use ordered_hashmap::OrderedHashMap;

/*
 * Initializes ranks for the input graph using the longest path algorithm. This
 * algorithm scales well and is fast in practice, it yields rather poor
 * solutions. Nodes are pushed to the lowest layer possible, leaving the bottom
 * ranks wide and leaving edges longer than necessary. However, due to its
 * speed, this algorithm is good for getting an initial ranking that can be fed
 * into other algorithms.
 *
 * This algorithm does not normalize layers because it will be used by other
 * algorithms in most cases. If using this algorithm directly, be sure to
 * run normalize at the end.
 *
 * Pre-conditions:
 *
 *    1. Input graph is a DAG.
 *    2. Input graph node labels can be assigned properties.
 *
 * Post-conditions:
 *
 *    1. Each node will be assign an (unnormalized) "rank" property.
 */

pub fn longest_path(g: &mut Graph<GraphConfig, GraphNode, GraphEdge>) {
    let mut visited: OrderedHashMap<String, bool> = OrderedHashMap::new();

    fn dfs(
        v: &String,
        g: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
        visited: &mut OrderedHashMap<String, bool>,
    ) -> i32 {
        let node_label = g.node(v);
        if visited.contains_key(v) {
            return node_label
                .cloned()
                .unwrap_or(GraphNode::default())
                .rank
                .unwrap_or(0);
        }
        visited.insert(v.clone(), true);

        let ranks: Vec<i32> = g
            .out_edges(v, None)
            .unwrap_or(vec![])
            .iter()
            .map(|e| {
                dfs(&e.w, g, visited)
                    - (g.edge_with_obj(&e)
                        .cloned()
                        .unwrap_or(GraphEdge::default())
                        .minlen
                        .unwrap_or(0.0)
                        .round() as i32)
            })
            .collect();
        let rank: i32 = ranks.iter().min().cloned().unwrap_or(0) as i32;
        {
            let _node_label = g.node_mut(v);
            if let Some(node_label) = _node_label {
                node_label.rank = Some(rank.clone());
            }
        }
        return rank;
    }

    for node_id in g.sources().into_iter() {
        dfs(&node_id, g, &mut visited);
    }
}

/*
 * Returns the amount of slack for the given edge. The slack is defined as the
 * difference between the length of the edge and its minimum length.
 */
pub fn slack(g: &Graph<GraphConfig, GraphNode, GraphEdge>, e: &Edge) -> i32 {
    let w_rank = g
        .node(&e.w)
        .cloned()
        .unwrap_or(GraphNode::default())
        .rank
        .unwrap_or(0);
    let v_rank = g
        .node(&e.v)
        .cloned()
        .unwrap_or(GraphNode::default())
        .rank
        .unwrap_or(0);
    let minlen = g
        .edge_with_obj(e)
        .cloned()
        .unwrap_or(GraphEdge::default())
        .minlen
        .unwrap_or(10.0)
        .round() as i32;
    return w_rank - v_rank - minlen;
}
