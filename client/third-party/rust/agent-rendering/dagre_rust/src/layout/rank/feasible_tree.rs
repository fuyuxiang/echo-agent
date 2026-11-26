use crate::layout::rank::util::slack;
use crate::{GraphConfig, GraphEdge, GraphNode};
use graphlib_rust::{Edge, Graph, GraphOption};

/*
 * Constructs a spanning tree with tight edges and adjusted the input node's
 * ranks to achieve this. A tight edge is one that is has a length that matches
 * its "minlen" attribute.
 *
 * The basic structure for this function is derived from Gansner, et al., "A
 * Technique for Drawing Directed Graphs."
 *
 * Pre-conditions:
 *
 *    1. Graph must be a DAG.
 *    2. Graph must be connected.
 *    3. Graph must have at least one node.
 *    5. Graph nodes must have been previously assigned a "rank" property that
 *       respects the "minlen" property of incident edges.
 *    6. Graph edges must have a "minlen" property.
 *
 * Post-conditions:
 *
 *    - Graph nodes will have their rank adjusted to ensure that all edges are
 *      tight.
 *
 * Returns a tree (undirected graph) that is constructed using only "tight"
 * edges.
 */

pub fn feasible_tree(
    g: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
) -> Graph<GraphConfig, GraphNode, GraphEdge> {
    let mut t: Graph<GraphConfig, GraphNode, GraphEdge> = Graph::new(Some(GraphOption {
        directed: Some(false),
        multigraph: Some(false),
        compound: Some(false),
    }));

    // Choose arbitrary node from which to start our tree
    let start = g.nodes().first().cloned().unwrap_or("".to_string());
    let size = g.node_count();
    t.set_node(start, Some(GraphNode::default()));

    while tight_tree(&mut t, g) < size {
        if let Some(edge) = find_min_stack_edge(&t, g) {
            let delta = if t.has_node(&edge.v) {
                slack(g, &edge)
            } else {
                -1 * slack(g, &edge)
            };
            shift_ranks(&t, g, delta);
        }
    }

    t
}

/*
 * Finds a maximal tree of tight edges and returns the number of nodes in the
 * tree.
 */
fn tight_tree(
    t: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
    g: &Graph<GraphConfig, GraphNode, GraphEdge>,
) -> usize {
    fn dfs(
        v: &String,
        t: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
        g: &Graph<GraphConfig, GraphNode, GraphEdge>,
    ) {
        let node_edges = g.node_edges(v, None).unwrap_or(vec![]);
        for node_edge in node_edges {
            let edge_v = node_edge.v.clone();
            let mut _w: Option<&String> = None;
            if v == &edge_v {
                _w = Some(&node_edge.w);
            } else {
                _w = Some(&edge_v);
            }
            let w = _w.unwrap().clone();
            if !t.has_node(&w) && slack(g, &node_edge) == 0 {
                t.set_node(w.clone(), Some(GraphNode::default()));
                let _ = t.set_edge(&v, &w, Some(GraphEdge::default()), None);
                dfs(&w, t, g);
            }
        }
    }

    let nodes = t.nodes();
    for node_id in nodes.into_iter() {
        dfs(&node_id, t, g);
    }
    return t.node_count();
}

/*
 * Finds the edge with the smallest slack that is incident on tree and returns
 * it.
 */
fn find_min_stack_edge(
    t: &Graph<GraphConfig, GraphNode, GraphEdge>,
    g: &Graph<GraphConfig, GraphNode, GraphEdge>,
) -> Option<Edge> {
    let edges = g.edges();
    let result = edges
        .iter()
        .map(|e| {
            let mut e_: Option<i32> = None;
            if t.has_node(&e.v) != t.has_node(&e.w) {
                e_ = Some(slack(g, e));
            }
            (e, e_)
        })
        .filter(|(_, e_)| e_.is_some())
        .min_by(|(_, e1_), (_, e2_)| e1_.unwrap().cmp(&e2_.unwrap()));

    if result.is_some() {
        Some(result.unwrap().0.clone())
    } else {
        None
    }
}

fn shift_ranks(
    t: &Graph<GraphConfig, GraphNode, GraphEdge>,
    g: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
    delta: i32,
) {
    let nodes = t.nodes();
    for node_id in nodes.into_iter() {
        let node_ = g.node_mut(&node_id);
        if let Some(node) = node_ {
            node.rank = Some(node.rank.unwrap_or(0) + delta);
        }
    }
}
