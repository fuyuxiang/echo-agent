use std::collections::HashMap;

use crate::layout::rank::feasible_tree::feasible_tree;
use crate::layout::rank::util::{longest_path, slack};
use crate::layout::util;
use crate::{GraphConfig, GraphEdge, GraphNode};
use graphlib_rust::algo::postorder::postorder;
use graphlib_rust::{Edge, Graph};
use ordered_hashmap::OrderedHashMap;

/*
 * The network simplex algorithm assigns ranks to each node in the input graph
 * and iteratively improves the ranking to reduce the length of edges.
 *
 * Preconditions:
 *
 *    1. The input graph must be a DAG.
 *    2. All nodes in the graph must have an object value.
 *    3. All edges in the graph must have "minlen" and "weight" attributes.
 *
 * Postconditions:
 *
 *    1. All nodes in the graph will have an assigned "rank" attribute that has
 *       been optimized by the network simplex algorithm. Ranks start at 0.
 *
 *
 * A rough sketch of the algorithm is as follows:
 *
 *    1. Assign initial ranks to each node. We use the longest path algorithm,
 *       which assigns ranks to the lowest position possible. In general this
 *       leads to very wide bottom ranks and unnecessarily long edges.
 *    2. Construct a feasible tight tree. A tight tree is one such that all
 *       edges in the tree have no slack (difference between length of edge
 *       and minlen for the edge). This by itself greatly improves the assigned
 *       rankings by shorting edges.
 *    3. Iteratively find edges that have negative cut values. Generally a
 *       negative cut value indicates that the edge could be removed and a new
 *       tree edge could be added to produce a more compact graph.
 *
 * Much of the algorithms here are derived from Gansner, et al., "A Technique
 * for Drawing Directed Graphs." The structure of the file roughly follows the
 * structure of the overall algorithm.
 */

pub fn network_simplex(g: &mut Graph<GraphConfig, GraphNode, GraphEdge>) {
    let mut simplified = util::simplify(g);

    longest_path(&mut simplified);

    let mut t: Graph<GraphConfig, GraphNode, GraphEdge> = feasible_tree(&mut simplified);
    init_low_lim_values(&mut t, None);
    init_cut_values(&mut t, &mut simplified);

    while let Some(e) = leave_edge(&t) {
        let Some(f) = enter_edge(&t, &simplified, &e) else {
            break;
        };
        exchange_edges(&mut t, &mut simplified, &e, f);
    }

    for v in g.nodes() {
        let Some(node) = g.node_mut(&v) else {
            continue;
        };
        let Some(simple_node) = simplified.node(&v) else {
            continue;
        };
        node.rank = simple_node.rank;
    }
}

/*
 * Initializes cut values for all edges in the tree.
 */
fn init_cut_values(
    t: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
    g: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
) {
    let node_ids = t.nodes();
    let mut vs = postorder(t, &node_ids);
    vs.pop();
    for node_id in vs {
        assign_cut_value(t, g, &node_id);
    }
}

/*
 * Given the tight tree, its graph, and a child in the graph calculate and
 * return the cut value for the edge between the child and its parent.
 */
fn assign_cut_value(
    t: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
    g: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
    child: &String,
) {
    let cutvalue = calc_cut_value(t, g, child);
    let child_lab_ = t.node_mut(child);
    if let Some(child_lab) = child_lab_ {
        let parent = child_lab.parent.clone().unwrap_or("".to_string());
        let edge_label_ = t.edge_mut(&child, &parent, None);
        if let Some(edge_label) = edge_label_ {
            edge_label.cutvalue = Some(cutvalue);
        }
    }
}

/*
 * Given the tight tree, its graph, and a child in the graph calculate and
 * return the cut value for the edge between the child and its parent.
 */
fn calc_cut_value(
    t: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
    g: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
    child: &String,
) -> f32 {
    // The accumulated cut value for the edge between this node and its parent
    let mut cut_value = 0.0;
    let child_lab_ = t.node_mut(child);
    if let Some(child_lab) = child_lab_ {
        let parent = child_lab.parent.clone().unwrap_or("".to_string());
        // True if the child is on the tail end of the edge in the directed graph
        let mut child_is_tail = true;
        // The graph's view of the tree edge we're inspecting
        let mut graph_edge = g.edge_mut(child, &parent, None);

        if graph_edge.is_none() {
            child_is_tail = false;
            graph_edge = g.edge_mut(&parent, &child, None);
        }

        cut_value = graph_edge
            .cloned()
            .unwrap_or(GraphEdge::default())
            .weight
            .unwrap_or(0.0);
        let edge_objs_ = g.node_edges(child, None);
        if let Some(edge_objs) = edge_objs_ {
            for e in edge_objs {
                let is_out_edge = &e.v == child;
                let other = if is_out_edge {
                    e.w.clone()
                } else {
                    e.v.clone()
                };

                if other != parent {
                    let points_to_head = is_out_edge == child_is_tail;
                    let other_weight = g
                        .edge_with_obj(&e)
                        .unwrap_or(&GraphEdge::default())
                        .weight
                        .unwrap_or(0.0);

                    cut_value += if points_to_head {
                        other_weight
                    } else {
                        -other_weight
                    };

                    if is_tree_edge(t, child, &other) {
                        let out_cut_value = t
                            .edge(&child, &other, None)
                            .unwrap_or(&GraphEdge::default())
                            .cutvalue
                            .unwrap_or(0.0);
                        cut_value += if points_to_head {
                            -out_cut_value
                        } else {
                            out_cut_value
                        }
                    }
                }
            }
        }
    }

    cut_value
}

fn init_low_lim_values(tree: &mut Graph<GraphConfig, GraphNode, GraphEdge>, root_: Option<String>) {
    let mut root = tree.nodes().first().cloned().unwrap_or("".to_string());
    if root_.is_some() {
        root = root_.unwrap();
    }
    let mut visited: OrderedHashMap<String, bool> = OrderedHashMap::new();
    dfs_assign_low_lim(tree, &mut visited, 1, &root, None);
}

fn dfs_assign_low_lim(
    tree: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
    visited: &mut OrderedHashMap<String, bool>,
    next_lim_: usize,
    v: &String,
    parent: Option<&String>,
) -> usize {
    let low = next_lim_.clone();
    let mut next_lim = next_lim_.clone();

    visited.entry(v.clone()).or_insert(true);
    let neighbors_ = tree.neighbors(v);
    if let Some(neighbors) = neighbors_ {
        for w in neighbors.into_iter() {
            if !visited.contains_key(&w) {
                next_lim = dfs_assign_low_lim(tree, visited, next_lim.clone(), &w, Some(v));
            }
        }
    }

    let label_ = tree.node_mut(v);
    if let Some(label) = label_ {
        label.low = Some(low);
        label.lim = Some(next_lim.clone());
        next_lim += 1;

        if parent.is_some() {
            label.parent = Some(parent.cloned().unwrap());
        } else {
            // TODO should be able to remove this when we incrementally update low lim
            label.parent = None;
        }
    }

    next_lim
}

fn leave_edge(tree: &Graph<GraphConfig, GraphNode, GraphEdge>) -> Option<Edge> {
    let edge_objs = tree.edges();
    edge_objs
        .iter()
        .find(|edge_obj| {
            tree.edge_with_obj(edge_obj)
                .unwrap_or(&GraphEdge::default())
                .cutvalue
                .unwrap_or(0.0)
                < 0.0
        })
        .cloned()
}

fn enter_edge(
    t: &Graph<GraphConfig, GraphNode, GraphEdge>,
    g: &Graph<GraphConfig, GraphNode, GraphEdge>,
    edge: &Edge,
) -> Option<Edge> {
    let mut v = edge.v.clone();
    let mut w = edge.w.clone();

    // For the rest of this function we assume that v is the tail and w is the
    // head, so if we don't have this edge in the graph we should flip it to
    // match the correct orientation.
    if !g.has_edge(&v, &w, None) {
        v = edge.w.clone();
        w = edge.v.clone();
    }

    let v_label = t.node(&v).cloned().unwrap_or(GraphNode::default());
    let w_label = t.node(&w).cloned().unwrap_or(GraphNode::default());
    let mut tail_label = &v_label;
    let mut flip = false;

    // If the root is in the tail of the edge then we need to flip the logic that
    // checks for the head and tail nodes in the candidates function below.
    if v_label.lim.clone().unwrap_or(0) > w_label.lim.clone().unwrap_or(0) {
        tail_label = &w_label;
        flip = true;
    }

    let edge_objs = g.edges();
    let candidates = edge_objs.iter().filter(|edge_obj| {
        let v_node = t.node(&edge_obj.v).cloned().unwrap_or(GraphNode::default());
        let w_node = t.node(&edge_obj.w).cloned().unwrap_or(GraphNode::default());
        flip == is_descendant(&v_node, tail_label) && flip != is_descendant(&w_node, tail_label)
    });

    candidates
        .min_by(|e1, e2| slack(g, e1).cmp(&slack(g, e2)))
        .cloned()
}

fn exchange_edges(
    t: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
    g: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
    e: &Edge,
    f: Edge,
) {
    let v = e.v.clone();
    let w = e.w.clone();
    t.remove_edge(&v, &w, None);
    let _ = t.set_edge(&f.v, &f.w, Some(GraphEdge::default()), None);
    init_low_lim_values(t, None);
    init_cut_values(t, g);
    update_ranks(t, g);
}

fn update_ranks(
    t: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
    g: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
) {
    let Some(root) = t.nodes().first().cloned() else {
        return;
    };

    let mut children: HashMap<String, Vec<String>> = HashMap::new();
    for v in t.nodes() {
        let parent = t.node(&v).unwrap_or(&GraphNode::default()).parent.clone();
        let Some(parent) = parent else {
            continue;
        };

        children.entry(parent).or_default().push(v);
    }

    let mut stack: Vec<String> = vec![root.clone()];
    while let Some(parent) = stack.pop() {
        let Some(vs) = children.get(&parent).cloned() else {
            continue;
        };

        for v in vs {
            let mut edge = g.edge(&v, &parent, None);
            let mut flipped = false;
            if edge.is_none() {
                edge = g.edge(&parent, &v, None);
                flipped = true;
            }

            let minlen = edge.unwrap_or(&GraphEdge::default()).minlen.unwrap_or(0.0);
            let parent_rank = g
                .node(&parent)
                .unwrap_or(&GraphNode::default())
                .rank
                .unwrap_or(0);
            if let Some(v_node) = g.node_mut(&v) {
                v_node.rank = Some(
                    parent_rank
                        + if flipped {
                            minlen as i32
                        } else {
                            -(minlen as i32)
                        },
                );
            }

            stack.push(v);
        }
    }
}

/*
 * Returns true if the edge is in the tree.
 */
fn is_tree_edge(tree: &Graph<GraphConfig, GraphNode, GraphEdge>, u: &String, v: &String) -> bool {
    tree.has_edge(&u, &v, None)
}

/*
 * Returns true if the specified node is descendant of the root node per the
 * assigned low and lim attributes in the tree.
 */
fn is_descendant(v_label: &GraphNode, root_label: &GraphNode) -> bool {
    let low = root_label.low.clone().unwrap_or(0);
    let v_lim = v_label.lim.clone().unwrap_or(0);
    let root_lim = root_label.lim.clone().unwrap_or(0);
    low <= v_lim && v_lim <= root_lim
}
