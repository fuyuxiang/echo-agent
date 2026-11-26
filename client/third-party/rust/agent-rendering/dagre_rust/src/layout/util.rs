use crate::layout::{GraphConfig, GraphEdge, GraphNode};
use crate::GraphEdgePoint;
use graphlib_rust::{Graph, GraphOption};
use ordered_hashmap::OrderedHashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

// VENDORING PATCH: upstream used a `static mut` mutated in an `unsafe` block,
// which is a data race when the engine renders on multiple threads (e.g. the
// parallel `cargo test` suite). An `AtomicUsize` is behaviour-preserving (still
// hands out monotonic unique ids) and removes the only `unsafe` in this crate.
static UNIQUE_STARTER: AtomicUsize = AtomicUsize::new(0);

pub fn unique_id() -> usize {
    UNIQUE_STARTER.fetch_add(1, Ordering::Relaxed) + 1
}

/*
 * Adds a dummy node to the graph and return v.
 */
pub fn add_dummy_node(
    graph: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
    node_type: String,
    data: GraphNode,
    name: String,
) -> String {
    // Generating Random Id
    let mut node_id = format!("{}{}", name, unique_id());
    while graph.has_node(&node_id) {
        node_id = format!("{}{}", name, unique_id());
    }

    // Setting in Graph
    let mut node_data = data.clone();
    node_data.dummy = Some(node_type);
    graph.set_node(node_id.clone(), Some(node_data));
    return node_id;
}

/*
 * Returns a new graph with only simple edges. Handles aggregation of data
 * associated with multi-edges.
 */
pub fn simplify(
    g: &Graph<GraphConfig, GraphNode, GraphEdge>,
) -> Graph<GraphConfig, GraphNode, GraphEdge> {
    let mut simplified: Graph<GraphConfig, GraphNode, GraphEdge> = Graph::new(Some(GraphOption {
        directed: Some(true),
        multigraph: None,
        compound: None,
    }));
    simplified.set_graph(g.graph().clone());

    let nodes = g.nodes();
    let edges = g.edges();
    for node_id in nodes.into_iter() {
        simplified.set_node(node_id.clone(), g.node(&node_id).cloned());
    }
    for edge_obj in edges.into_iter() {
        let edge_label_ = g.edge_with_obj(&edge_obj);

        let mut simple_label = simplified
            .edge(&edge_obj.v, &edge_obj.w, None)
            .cloned()
            .unwrap_or_else(|| {
                let mut edge = GraphEdge::default();
                edge.weight = Some(0.0);
                edge.minlen = Some(1.0);
                edge
            });

        let edge_label = edge_label_.cloned().unwrap_or_else(|| {
            let mut edge = GraphEdge::default();
            edge.weight = Some(0.0);
            edge.minlen = Some(1.0);
            edge
        });

        let minlen = edge_label.minlen.unwrap_or(1.0);
        simple_label.minlen = Some(simple_label.minlen.unwrap_or(1.0).max(minlen));

        let weight = edge_label.weight.unwrap_or(0.0);
        simple_label.weight = Some(simple_label.weight.unwrap_or(0.0) + weight);

        let _ = simplified.set_edge(&edge_obj.v, &edge_obj.w, Some(simple_label), None);
    }

    simplified
}

/*
 * it implement same logic as simplify do but, it uses Ref instead of creating new graph
 */
pub fn simplify_ref(g: &mut Graph<GraphConfig, GraphNode, GraphEdge>) {
    let edges = g.edges();
    for edge_obj in edges.into_iter() {
        let edge_label_ = g.edge_mut_with_obj(&edge_obj);
        if let Some(edge_label) = edge_label_ {
            if edge_label.weight.is_none() {
                edge_label.weight = Some(0.0);
            }
            if edge_label.minlen.is_none() {
                edge_label.minlen = Some(1.0);
            }
        }
    }
}

pub fn as_non_compound_graph(
    g: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
) -> Graph<GraphConfig, GraphNode, GraphEdge> {
    let mut simplified: Graph<GraphConfig, GraphNode, GraphEdge> = Graph::new(Some(GraphOption {
        directed: Some(true),
        multigraph: Some(true),
        compound: Some(false),
    }));
    simplified.set_graph(g.graph().clone());

    let nodes = g.nodes();
    for v in nodes.into_iter() {
        if g.children(&v).len() == 0 {
            simplified.set_node(
                v.clone(),
                Some(g.node(&v).cloned().unwrap_or(GraphNode::default())),
            );
        }
    }
    let edge_objs = g.edges();
    for e in edge_objs.into_iter() {
        let _ = simplified.set_edge_with_obj(&e, g.edge_with_obj(&e).cloned());
    }

    return simplified;
}

pub fn transfer_node_edge_labels(
    source: &Graph<GraphConfig, GraphNode, GraphEdge>,
    destination: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
) {
    let nodes = source.nodes();
    for v in nodes.into_iter() {
        if source.children(&v).len() == 0 {
            destination.set_node(
                v.clone(),
                Some(source.node(&v).cloned().unwrap_or(GraphNode::default())),
            );
        }
    }

    let edge_objs = source.edges();
    for e in edge_objs.into_iter() {
        let _ = destination.set_edge_with_obj(&e, source.edge_with_obj(&e).cloned());
    }
}

pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/*
 * Finds where a line starting at point ({x, y}) would intersect a rectangle
 * ({x, y, width, height}) if it were pointing at the rectangle's center.
 */
pub fn intersect_rect(rect: &Rect, point: &GraphEdgePoint) -> GraphEdgePoint {
    let x = rect.x;
    let y = rect.y;

    // Rectangle intersection algorithm from:
    // http://math.stackexchange.com/questions/108113/find-edge-between-two-boxes
    let dx = point.x - x;
    let dy = point.y - y;
    let w = rect.width / 2.0;
    let h = rect.height / 2.0;

    if dx == 0.0 && dy == 0.0 {
        return GraphEdgePoint { x: x + w, y };
    }

    let (sx, sy) = if (dy.abs() * w) > (dx.abs() * h) {
        // Intersection is top or bottom of rect.
        if dy < 0.0 {
            (-h * dx / dy, -h)
        } else {
            (h * dx / dy, h)
        }
    } else {
        // Intersection is left or right of rect.
        if dx < 0.0 {
            (-w, -w * dy / dx)
        } else {
            (w, w * dy / dx)
        }
    };

    GraphEdgePoint {
        x: x + sx,
        y: y + sy,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(left: f32, right: f32) {
        assert!((left - right).abs() < f32::EPSILON);
    }

    #[test]
    fn intersect_rect_returns_boundary_point_for_center_point() {
        let rect = Rect {
            x: 10.0,
            y: 20.0,
            width: 8.0,
            height: 4.0,
        };
        let point = GraphEdgePoint { x: 10.0, y: 20.0 };

        let intersection = intersect_rect(&rect, &point);

        assert_close(intersection.x, 14.0);
        assert_close(intersection.y, 20.0);
    }
}

/*
 * Given a DAG with each node assigned "rank" and "order" properties, this
 * function will produce a matrix with the ids of each node.
 */
pub fn build_layer_matrix(g: &Graph<GraphConfig, GraphNode, GraphEdge>) -> Vec<Vec<String>> {
    let mut layering: Vec<OrderedHashMap<usize, String>> =
        (0..=max_rank(g)).map(|_| OrderedHashMap::new()).collect();

    g.nodes().iter().for_each(|v| {
        let node = g.node(v).unwrap();
        let Some(rank) = node.rank else {
            return;
        };

        let layer: &mut OrderedHashMap<usize, String> = layering.get_mut(rank as usize).unwrap();
        layer.insert(node.order.unwrap_or(0), v.clone());
    });

    layering
        .into_iter()
        .map(|layer| {
            let mut keys: Vec<usize> = layer.keys().cloned().collect();
            keys.sort();
            keys.iter()
                .map(|key| layer.get(key).cloned().unwrap())
                .collect()
        })
        .collect()
}

/*
 * Adjusts the ranks for all nodes in the graph such that all nodes v have
 * rank(v) >= 0 and at least one node w has rank(w) = 0.
 */
pub fn normalize_ranks(graph: &mut Graph<GraphConfig, GraphNode, GraphEdge>) {
    let node_ids = graph.nodes();
    let node_ranks: Vec<i32> = node_ids
        .iter()
        .map(|v| {
            graph
                .node(v)
                .unwrap_or(&GraphNode::default())
                .rank
                .clone()
                .unwrap_or(0)
        })
        .collect();
    let min = node_ranks.iter().min().cloned().unwrap_or(0);
    node_ids.iter().for_each(|node_id| {
        let node_ = graph.node_mut(node_id);
        if let Some(node) = node_ {
            if node.rank.is_some() {
                node.rank = Some(node.rank.unwrap() - min);
            }
        }
    })
}

pub fn remove_empty_ranks(graph: &mut Graph<GraphConfig, GraphNode, GraphEdge>) {
    let nodes: Vec<String> = graph.nodes();
    if nodes.is_empty() {
        return;
    }

    let node_ranks: Vec<i32> = nodes
        .iter()
        .map(|v| {
            graph
                .node(v)
                .cloned()
                .unwrap_or(GraphNode::default())
                .rank
                .unwrap_or(0)
        })
        .collect();
    let offset: i32 = node_ranks.iter().min().cloned().unwrap_or(0);
    let max_rank: i32 = node_ranks.iter().max().cloned().unwrap_or(0) - offset;

    let mut layers: Vec<Vec<String>> = vec![Vec::new(); (max_rank + 1).max(0) as usize];
    for v in &nodes {
        let rank = graph
            .node(v)
            .unwrap_or(&GraphNode::default())
            .rank
            .unwrap_or(0)
            - offset;
        if rank >= 0 {
            layers[rank as usize].push(v.clone());
        }
    }

    let node_rank_factor = graph.graph().node_rank_factor.clone().unwrap_or(0.0) as i32;
    if node_rank_factor <= 0 {
        return;
    }

    let mut delta = 0;
    for (i, vs) in layers.iter().enumerate() {
        let i = i as i32;
        if vs.is_empty() && i % node_rank_factor != 0 {
            delta -= 1;
        } else if delta != 0 {
            for v in vs {
                if let Some(node) = graph.node_mut(v) {
                    node.rank = Some(node.rank.unwrap_or(0) + delta);
                }
            }
        }
    }
}

pub fn add_border_node(
    graph: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
    prefix: &str,
    rank: Option<&usize>,
    order: Option<&usize>,
) -> String {
    let mut node = GraphNode::default();

    if rank.is_some() {
        node.rank = Some(rank.cloned().unwrap_or(0) as i32);
    }
    if order.is_some() {
        node.order = Some(order.cloned().unwrap_or(0));
    }

    return add_dummy_node(graph, "border".to_string(), node, prefix.to_string());
}

pub fn max_rank(g: &Graph<GraphConfig, GraphNode, GraphEdge>) -> i32 {
    g.nodes()
        .iter()
        .filter_map(|v| g.node(v).and_then(|n| n.rank))
        .max()
        .unwrap_or(0)
}

#[derive(Debug, Clone)]
pub struct PartitionResponse<V> {
    pub lhs: Vec<V>,
    pub rhs: Vec<V>,
}
/*
 * Partition a collection into two groups: `lhs` and `rhs`. If the supplied
 * function returns true for an entry it goes into `lhs`. Otherwise it goes
 * into `rhs.
 */
pub fn partition<V: Clone>(
    collection: &Vec<V>,
    fn_: Box<dyn Fn(&V) -> bool>,
) -> PartitionResponse<V> {
    let mut result: PartitionResponse<V> = PartitionResponse {
        lhs: vec![],
        rhs: vec![],
    };

    collection.iter().for_each(|val| {
        if fn_(val) {
            result.lhs.push(val.clone());
        } else {
            result.rhs.push(val.clone());
        }
    });

    return result;
}
