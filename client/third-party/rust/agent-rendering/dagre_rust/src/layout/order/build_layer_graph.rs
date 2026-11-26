use crate::layout::util::unique_id;
use crate::{GraphConfig, GraphEdge, GraphNode};
use graphlib_rust::{Edge, Graph, GraphOption};

/*
 * Constructs a graph that can be used to sort a layer of nodes. The graph will
 * contain all base and subgraph nodes from the request layer in their original
 * hierarchy and any edges that are incident on these nodes and are of the type
 * requested by the "relationship" parameter.
 *
 * Nodes from the requested rank that do not have parents are assigned a root
 * node in the output graph, which is set in the root graph attribute. This
 * makes it easy to walk the hierarchy of movable nodes during ordering.
 *
 * Pre-conditions:
 *
 *    1. Input graph is a DAG
 *    2. Base nodes in the input graph have a rank attribute
 *    3. Subgraph nodes in the input graph has minRank and maxRank attributes
 *    4. Edges have an assigned weight
 *
 * Post-conditions:
 *
 *    1. Output graph has all nodes in the movable rank with preserved
 *       hierarchy.
 *    2. Root nodes in the movable layer are made children of the node
 *       indicated by the root attribute of the graph.
 *    3. Non-movable nodes incident on movable nodes, selected by the
 *       relationship parameter, are included in the graph (without hierarchy).
 *    4. Edges incident on movable nodes, selected by the relationship
 *       parameter, are added to the output graph.
 *    5. The weights for copied edges are aggregated as need, since the output
 *       graph is not a multi-graph.
 */

#[derive(Debug, Copy, Clone)]
pub enum GraphRelationship {
    InEdges,
    OutEdges,
}

pub fn build_layer_graph(
    g: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
    rank: &i32,
    relationship: GraphRelationship,
) -> Graph<GraphConfig, GraphNode, GraphEdge> {
    let root = create_root_node(g);
    let mut result: Graph<GraphConfig, GraphNode, GraphEdge> = Graph::new(Some(GraphOption {
        directed: Some(true),
        compound: Some(true),
        multigraph: None,
    }));
    let graph_label = result.graph_mut();
    graph_label.root = Some(root.clone());

    g.nodes().iter().for_each(|v| {
        let node = g.node(v).unwrap();
        let parent = g.parent(v);

        let in_rank = node.rank == Some(*rank);
        let in_subgraph_rank = match (node.min_rank, node.max_rank) {
            (Some(min_rank), Some(max_rank)) => min_rank <= *rank && *rank <= max_rank,
            _ => false,
        };

        let mut _relationship: Vec<Edge> = g.in_edges(v, None).unwrap_or(vec![]);
        match relationship {
            GraphRelationship::OutEdges => {
                _relationship = g.out_edges(v, None).unwrap_or(vec![]);
            }
            _ => (),
        }

        if in_rank || in_subgraph_rank {
            result.set_node(v.clone(), Some(node.clone()));
            if parent.is_some() {
                let _ = result.set_parent(v, parent.cloned());
            } else {
                let _ = result.set_parent(v, Some(root.clone()));
            }

            // This assumes we have only short edges!
            _relationship.iter().for_each(|e| {
                let u = if &e.v == v { e.w.clone() } else { e.v.clone() };
                let edge = result.edge(&u, &v, None);
                let weight = if edge.is_some() {
                    edge.unwrap().weight.clone().unwrap_or(0.0)
                } else {
                    0.0
                };
                let mut edge_label = GraphEdge::default();
                edge_label.weight =
                    Some(g.edge_with_obj(&e).unwrap().weight.clone().unwrap_or(0.0) + weight);
                let _ = result.set_edge(&u, &v, Some(edge_label), None);
            });

            if node.min_rank.is_some() {
                let mut graph_node = GraphNode::default();
                graph_node.border_left_ = node.border_left.as_ref().unwrap().get(rank).cloned();
                graph_node.border_right_ = node.border_right.as_ref().unwrap().get(rank).cloned();
                result.set_node(v.clone(), Some(graph_node));
            }
        }
    });

    result
}

pub fn create_root_node(g: &Graph<GraphConfig, GraphNode, GraphEdge>) -> String {
    let mut v = format!("_root{}", unique_id());
    while g.has_node(&v) {
        v = format!("_root{}", unique_id());
    }

    v
}
