use std::collections::HashMap;

use graphlib_rust::Graph;

use crate::{GraphConfig, GraphEdge, GraphNode};

/*
 * A function that takes a layering (an array of layers, each with an array of
 * ordererd nodes) and a graph and returns a weighted crossing count.
 *
 * Pre-conditions:
 *
 *    1. Input graph must be simple (not a multigraph), directed, and include
 *       only simple edges.
 *    2. Edges in the input graph must have assigned weights.
 *
 * Post-conditions:
 *
 *    1. The graph and layering matrix are left unchanged.
 *
 * This algorithm is derived from Barth, et al., "Bilayer Cross Counting."
 */

pub fn cross_count(
    g: &Graph<GraphConfig, GraphNode, GraphEdge>,
    layering: &Vec<Vec<String>>,
) -> f32 {
    let mut cc = 0.0;
    for i in 1..layering.len() {
        cc += two_layer_cross_count(g, &layering[i - 1], &layering[i]);
    }
    cc
}

pub fn two_layer_cross_count(
    g: &Graph<GraphConfig, GraphNode, GraphEdge>,
    north_layer: &Vec<String>,
    south_layer: &Vec<String>,
) -> f32 {
    let mut south_pos: HashMap<String, usize> = HashMap::new();
    for (i, v) in south_layer.iter().enumerate() {
        south_pos.insert(v.clone(), i);
    }

    let mut south_entries: Vec<(usize, f32)> = vec![];
    for v in north_layer {
        let mut out_edges: Vec<(usize, f32)> = g
            .out_edges(v, None)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|e| {
                let pos = south_pos.get(&e.w)?;
                let weight = g
                    .edge_with_obj(&e)
                    .and_then(|edge| edge.weight)
                    .unwrap_or(0.0);
                Some((*pos, weight))
            })
            .collect();

        out_edges.sort_by(|e1, e2| e1.0.cmp(&e2.0));
        south_entries.extend(out_edges);
    }

    let mut first_index: usize = 1;
    while first_index < south_layer.len() {
        first_index <<= 1;
    }
    let tree_size = 2 * first_index - 1;
    first_index -= 1;

    let mut tree: Vec<f32> = vec![0.0; tree_size];

    let mut cc = 0.0;
    for (pos, weight) in south_entries {
        let mut index = pos + first_index;
        tree[index] += weight;

        let mut weight_sum = 0.0;
        while index > 0 {
            if index % 2 != 0 {
                weight_sum += tree[index + 1];
            }
            index = (index - 1) >> 1;
            tree[index] += weight;
        }
        cc += weight * weight_sum;
    }

    cc
}
