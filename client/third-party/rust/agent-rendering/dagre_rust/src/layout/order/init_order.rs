use crate::{GraphConfig, GraphEdge, GraphNode};
use graphlib_rust::Graph;
use ordered_hashmap::OrderedHashMap;

/*
 * Assigns an initial order value for each node by performing a DFS search
 * starting from nodes in the first rank. Nodes are assigned an order in their
 * rank as they are first visited.
 *
 * This approach comes from Gansner, et al., "A Technique for Drawing Directed
 * Graphs."
 *
 * Returns a layering matrix with an array per layer and each layer sorted by
 * the order of its nodes.
 */

pub fn init_order(g: &Graph<GraphConfig, GraphNode, GraphEdge>) -> Vec<Vec<String>> {
    let mut visited: OrderedHashMap<String, bool> = OrderedHashMap::new();
    let mut simple_nodes: Vec<String> = g
        .nodes()
        .into_iter()
        .filter(|v| g.children(v).len() == 0)
        .collect();
    let max_rank = simple_nodes
        .iter()
        .map(|v| g.node(v).unwrap().rank.clone().unwrap_or(0))
        .max()
        .unwrap_or(0);
    let mut layers: Vec<Vec<String>> = (0..=max_rank).map(|_| -> Vec<String> { vec![] }).collect();

    fn dfs(
        v: &String,
        g: &Graph<GraphConfig, GraphNode, GraphEdge>,
        visited: &mut OrderedHashMap<String, bool>,
        layers: &mut Vec<Vec<String>>,
    ) {
        if visited.contains_key(v) {
            return ();
        }

        visited.insert(v.clone(), true);
        let node = g.node(v).unwrap();
        let node_rank = node.rank.unwrap_or(0) as usize;
        if layers.get(node_rank.clone()).is_none() {
            layers.insert(node_rank.clone(), vec![]);
        }
        let layer: &mut Vec<String> = layers.get_mut(node_rank.clone()).unwrap();
        layer.push(v.clone());

        let sucs = g.successors(v).unwrap_or(vec![]);
        for sv in sucs.iter() {
            dfs(sv, g, visited, layers)
        }
    }

    simple_nodes.sort_by(|v1, v2| {
        let v1_rank = g.node(v1).unwrap().rank.clone().unwrap_or(0);
        let v2_rank = g.node(v2).unwrap().rank.clone().unwrap_or(0);

        v1_rank.cmp(&v2_rank)
    });

    for v in simple_nodes.iter() {
        dfs(v, g, &mut visited, &mut layers);
    }

    return layers;
}
