use crate::layout::util::unique_id;
use crate::layout::{GraphConfig, GraphEdge, GraphNode};
use graphlib_rust::{Edge, Graph};
use ordered_hashmap::OrderedHashMap;

pub fn run(graph: &mut Graph<GraphConfig, GraphNode, GraphEdge>) {
    let mut fas: Option<Vec<Edge>> = None;
    let graph_config = graph.graph();
    if graph_config.acyclicer.is_some()
        && graph_config.acyclicer.clone().unwrap() == "greedy".to_string()
    {
        // TODO: need to implement this algorithm
        println!("greedy_fas");
        // greedyFAS
    } else {
        fas = Some(dfs_fas(graph));
        // println!("dfs_fas");
    }

    let _fas = fas.unwrap_or(vec![]);
    for edge in _fas {
        let _edge_label = graph.edge_with_obj(&edge);
        if _edge_label.is_none() {
            continue;
        }
        let mut edge_label = _edge_label.cloned().unwrap();
        graph.remove_edge_with_obj(&edge);
        edge_label.forward_name = edge.name.clone();
        edge_label.reversed = Some(true);
        let _ = graph.set_edge(
            &edge.w,
            &edge.v,
            Some(edge_label),
            Some(format!("rev{}", unique_id())),
        );
    }
}

fn dfs_fas(graph: &mut Graph<GraphConfig, GraphNode, GraphEdge>) -> Vec<Edge> {
    let mut fas: Vec<Edge> = vec![];
    let mut stack: OrderedHashMap<String, bool> = OrderedHashMap::new();
    let mut visited: OrderedHashMap<String, bool> = OrderedHashMap::new();

    fn dfs(
        node_id: String,
        stack: &mut OrderedHashMap<String, bool>,
        visited: &mut OrderedHashMap<String, bool>,
        graph: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
        fas: &mut Vec<Edge>,
    ) {
        if visited.contains_key(&node_id) {
            return ();
        }

        visited.insert(node_id.clone(), true);
        stack.insert(node_id.clone(), true);
        let out_edges = graph.out_edges(&node_id, None).unwrap_or(vec![]);
        for edge in out_edges.into_iter() {
            if stack.contains_key(&edge.w) {
                fas.push(edge.clone());
            } else {
                dfs(edge.w.clone(), stack, visited, graph, fas);
            }
        }
        stack.remove(&node_id);
    }

    for node_id in graph.nodes() {
        dfs(node_id, &mut stack, &mut visited, graph, &mut fas);
    }
    return fas;
}

pub fn undo(g: &mut Graph<GraphConfig, GraphNode, GraphEdge>) {
    for e in g.edges() {
        let Some(edge) = g.edge_mut_with_obj(&e) else {
            continue;
        };
        if edge.reversed.clone().unwrap_or(false) {
            let forward_name = edge.forward_name.clone();
            let mut label = edge.clone();
            label.reversed = None;
            label.forward_name = None;
            let _ = g.set_edge(&e.w, &e.v, Some(label), forward_name);
        }
    }
}
