/*
 * This module provides coordinate assignment based on Brandes and Köpf, "Fast
 * and Simple Horizontal Coordinate Assignment."
 */

use crate::layout::add_border_segments::BorderTypeName;
use crate::layout::util;
use crate::{GraphConfig, GraphEdge, GraphNode};
use graphlib_rust::{Graph, GraphOption};
use ordered_hashmap::OrderedHashMap;
use std::cmp::Ordering;

/*
 * Marks all edges in the graph with a type-1 conflict with the "type1Conflict"
 * property. A type-1 conflict is one where a non-inner segment crosses an
 * inner segment. An inner segment is an edge with both incident nodes marked
 * with the "dummy" property.
 *
 * This algorithm scans layer by layer, starting with the second, for type-1
 * conflicts between the current layer and the previous layer. For each layer
 * it scans the nodes from left to right until it reaches one that is incident
 * on an inner segment. It then scans predecessors to determine if they have
 * edges that cross that inner segment. At the end a final scan is done for all
 * nodes on the current rank to see if they cross the last visited inner
 * segment.
 *
 * This algorithm (safely) assumes that a dummy node will only be incident on a
 * single node in the layers being scanned.
 */
fn find_type_1_conflicts(
    g: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
    layering: &Vec<Vec<String>>,
) -> OrderedHashMap<String, OrderedHashMap<String, bool>> {
    let mut conflicts = OrderedHashMap::new();

    fn visit_layer(
        g: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
        prev_layer: &Vec<String>,
        layer: &Vec<String>,
        conflicts: &mut OrderedHashMap<String, OrderedHashMap<String, bool>>,
    ) {
        // last visited node in the previous layer that is incident on an inner
        // segment.
        let mut k0 = 0;
        // Tracks the last node in this layer scanned for crossings with a type-1
        // segment.
        let mut scan_pos = 0;
        let prev_layer_length = prev_layer.len();
        let last_node = layer.last().unwrap().clone();

        for (i, v) in layer.iter().enumerate() {
            let w = find_other_inner_segment_node(g, v);
            let k1 = if let Some(ref w) = w {
                g.node(w).unwrap().order.unwrap_or(0)
            } else {
                prev_layer_length
            };

            if w.is_some() || *v == last_node {
                for scan_node in layer[scan_pos..=i].iter() {
                    for u in g.predecessors(scan_node).unwrap() {
                        let u_label = g.node(&u).unwrap();
                        let u_pos = u_label.order.unwrap_or(0);
                        if (u_pos < k0 || k1 < u_pos)
                            && !(u_label.dummy.is_some()
                                && g.node(scan_node).unwrap().dummy.is_some())
                        {
                            add_conflict(conflicts, &u, scan_node);
                        }
                    }
                }
                scan_pos = i + 1;
                k0 = k1;
            }
        }
    }

    layering
        .iter()
        .filter(|layer| !layer.is_empty())
        .reduce(|prev_layer, layer| {
            visit_layer(g, &prev_layer, layer, &mut conflicts);

            layer
        });

    conflicts
}

pub fn find_type_2_conflicts(
    g: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
    layering: &Vec<Vec<String>>,
) -> OrderedHashMap<String, OrderedHashMap<String, bool>> {
    let mut conflicts: OrderedHashMap<String, OrderedHashMap<String, bool>> = OrderedHashMap::new();

    fn scan(
        g: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
        south: &Vec<String>,
        south_pos: usize,
        south_end: usize,
        prev_north_border: i32,
        next_north_border: i32,
        conflicts: &mut OrderedHashMap<String, OrderedHashMap<String, bool>>,
    ) {
        for i in south_pos..south_end {
            let v = south.get(i).cloned().unwrap();
            if g.node(&v).is_some() && g.node(&v).unwrap().dummy.is_some() {
                let preds = g.predecessors(&v).unwrap_or(vec![]);
                preds.iter().for_each(|u| {
                    let u_node_ = g.node(u);
                    if let Some(u_node) = u_node_ {
                        let u_node_order = u_node.order.unwrap_or(0) as i32;
                        if u_node.dummy.is_some()
                            && (u_node_order < prev_north_border
                                || u_node_order > next_north_border)
                        {
                            add_conflict(conflicts, u, &v);
                        }
                    }
                });
            }
        }
    }

    fn visit_layer(
        g: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
        north: &Vec<String>,
        south: &Vec<String>,
        conflicts: &mut OrderedHashMap<String, OrderedHashMap<String, bool>>,
    ) {
        let mut prev_north_pos: i32 = -1;
        let mut next_north_pos: i32 = -1;
        let mut south_pos: usize = 0;

        for (south_lookahead, v) in south.iter().enumerate() {
            if let Some(v_node) = g.node(v) {
                if v_node.dummy.as_deref() == Some("border") {
                    let predecessors = g.predecessors(v).unwrap_or(vec![]);
                    if !predecessors.is_empty() {
                        next_north_pos =
                            g.node(&predecessors[0]).unwrap().order.unwrap_or(0) as i32;
                        scan(
                            g,
                            south,
                            south_pos,
                            south_lookahead,
                            prev_north_pos,
                            next_north_pos,
                            conflicts,
                        );
                        south_pos = south_lookahead;
                        prev_north_pos = next_north_pos;
                    }
                }
            }

            scan(
                g,
                south,
                south_pos,
                south.len(),
                next_north_pos,
                north.len() as i32,
                conflicts,
            );
        }
    }

    for i in 1..layering.len() {
        if layering[i - 1].is_empty() || layering[i].is_empty() {
            continue;
        }
        visit_layer(g, &layering[i - 1], &layering[i], &mut conflicts);
    }

    conflicts
}

fn find_other_inner_segment_node(
    g: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
    v: &String,
) -> Option<String> {
    if g.node(v).unwrap().dummy.is_some() {
        let preds = g.predecessors(v).unwrap_or(vec![]);
        return preds
            .iter()
            .find(|u| g.node(u).unwrap().dummy.is_some())
            .cloned();
    }

    None
}

pub fn add_conflict(
    conflicts: &mut OrderedHashMap<String, OrderedHashMap<String, bool>>,
    v_: &String,
    w_: &String,
) {
    let mut v = v_.clone();
    let mut w = w_.clone();
    if v.cmp(&w) == Ordering::Greater {
        let tmp = v;
        v = w;
        w = tmp;
    }

    let _conflicts_v = conflicts.get(&v);
    if _conflicts_v.is_none() {
        conflicts.insert(v.clone(), OrderedHashMap::new());
    }

    let conflicts_v = conflicts.get_mut(&v).unwrap();
    conflicts_v.insert(w.clone(), true);
}

pub fn has_conflict(
    conflicts: &OrderedHashMap<String, OrderedHashMap<String, bool>>,
    v_: &String,
    w_: &String,
) -> bool {
    let mut v = v_;
    let mut w = w_;
    if v_.cmp(w_) == Ordering::Greater {
        let tmp = v;
        v = w;
        w = tmp;
    }

    let empty_hashmap: OrderedHashMap<String, bool> = OrderedHashMap::new();
    conflicts.get(v).unwrap_or(&empty_hashmap).contains_key(w)
}

/*
 * Try to align nodes into vertical "blocks" where possible. This algorithm
 * attempts to align a node with one of its median neighbors. If the edge
 * connecting a neighbor is a type-1 conflict then we ignore that possibility.
 * If a previous node has already formed a block with a node after the node
 * we're trying to form a block with, we also ignore that possibility - our
 * blocks would be split in that scenario.
 */
// root -> 0, align -> 1
pub fn vertical_alignment(
    g: &Graph<GraphConfig, GraphNode, GraphEdge>,
    layering: &Vec<Vec<String>>,
    conflicts: &OrderedHashMap<String, OrderedHashMap<String, bool>>,
    neighbor_fn: Box<dyn Fn(&Graph<GraphConfig, GraphNode, GraphEdge>, &String) -> Vec<String>>,
) -> (OrderedHashMap<String, String>, Vec<String>) {
    let mut root: OrderedHashMap<String, String> = OrderedHashMap::new();
    let mut align: OrderedHashMap<String, String> = OrderedHashMap::new();
    let mut pos: OrderedHashMap<String, usize> = OrderedHashMap::new();

    // We cache the position here based on the layering because the graph and
    // layering may be out of sync. The layering matrix is manipulated to
    // generate different extreme alignments.
    layering.iter().for_each(|layer| {
        layer.iter().enumerate().for_each(|(order, v)| {
            root.insert(v.clone(), v.clone());
            align.insert(v.clone(), v.clone());
            pos.insert(v.clone(), order);
        });
    });

    layering.iter().for_each(|layer| {
        let mut prev_idx: i32 = -1;
        layer.iter().for_each(|v| {
            let mut ws: Vec<String> = neighbor_fn(g, v)
                .into_iter()
                .filter(|w| pos.contains_key(w))
                .collect();
            if ws.len() > 0 {
                ws.sort_by(|w1, w2| pos.get(w1).unwrap().cmp(pos.get(w2).unwrap()));
                let mp = (ws.len() as f32 - 1.0) / 2.0;
                let mut i = mp as usize;
                let il = mp.ceil() as usize;
                while i <= il {
                    let w = ws[i].clone();
                    if align.get(v).unwrap() == v
                        && prev_idx < (pos.get(&w).cloned().unwrap() as i32)
                        && !has_conflict(conflicts, v, &w)
                    {
                        align.insert(w.clone(), v.clone());

                        root.insert(v.clone(), root.get(&w).unwrap().clone());
                        align.insert(v.clone(), root.get(&w).unwrap().clone());

                        prev_idx = pos.get(&w).unwrap().clone() as i32;
                    }

                    i += 1;
                }
            }
        });
    });

    let align_keys: Vec<String> = align.keys().cloned().collect();
    return (root, align_keys);
}

pub fn horizontal_compaction(
    g: &Graph<GraphConfig, GraphNode, GraphEdge>,
    layering: &Vec<Vec<String>>,
    root: &OrderedHashMap<String, String>,
    align: &Vec<String>,
    reverse_sep: bool,
) -> OrderedHashMap<String, f32> {
    // This portion of the algorithm differs from BK due to a number of problems.
    // Instead of their algorithm we construct a new block graph and do two
    // sweeps. The first sweep places blocks with the smallest possible
    // coordinates. The second sweep removes unused space by moving blocks to the
    // greatest coordinates without violating separation.
    let mut xs: OrderedHashMap<String, f32> = OrderedHashMap::new();
    let block_g: Graph<GraphOption, String, f32> =
        build_block_graph(g, layering, root, reverse_sep);
    let border_type = if reverse_sep {
        BorderTypeName::BorderLeft
    } else {
        BorderTypeName::BorderRight
    };

    fn iterate(
        set_xs_func: fn(
            &String,
            &mut OrderedHashMap<String, f32>,
            &Graph<GraphOption, String, f32>,
            &Graph<GraphConfig, GraphNode, GraphEdge>,
            &BorderTypeName,
        ),
        next_nodes_func: Box<dyn Fn(&Graph<GraphOption, String, f32>, &String) -> Vec<String>>,
        block_g: &Graph<GraphOption, String, f32>,
        xs: &mut OrderedHashMap<String, f32>,
        g: &Graph<GraphConfig, GraphNode, GraphEdge>,
        border_type: &BorderTypeName,
    ) {
        let mut stack = block_g.nodes();
        let mut elem = stack.pop();
        let mut visited: OrderedHashMap<String, bool> = OrderedHashMap::new();
        while elem.is_some() {
            let elem_ = elem.unwrap();
            if visited.contains_key(&elem_) {
                set_xs_func(&elem_, xs, block_g, g, border_type);
            } else {
                visited.insert(elem_.clone(), true);
                stack.push(elem_.clone());
                stack.append(&mut (next_nodes_func(block_g, &elem_) as Vec<String>));
            }

            elem = stack.pop();
        }
    }

    // First pass, assign smallest coordinates
    fn pass1(
        elem: &String,
        xs: &mut OrderedHashMap<String, f32>,
        block_g: &Graph<GraphOption, String, f32>,
        _g: &Graph<GraphConfig, GraphNode, GraphEdge>,
        _border_type: &BorderTypeName,
    ) {
        let in_edges = block_g.in_edges(elem, None).unwrap_or(vec![]);
        let val: f32 = in_edges.iter().fold(0.0, |acc, e| {
            let pred_x = xs.get(&e.v).cloned().unwrap_or(0.0);
            let sep = block_g.edge_with_obj(&e).cloned().unwrap_or(0.0);
            let ev = pred_x + sep;
            acc.max(ev)
        });
        xs.insert(elem.clone(), val);
    }

    // Second pass, assign greatest coordinates
    fn pass2(
        elem: &String,
        xs: &mut OrderedHashMap<String, f32>,
        block_g: &Graph<GraphOption, String, f32>,
        g: &Graph<GraphConfig, GraphNode, GraphEdge>,
        border_type: &BorderTypeName,
    ) {
        let out_edges = block_g.out_edges(elem, None).unwrap_or(vec![]);
        let min: f64 = out_edges.iter().fold(f64::INFINITY, |acc, e| {
            let succ_x = xs.get(&e.w).cloned().unwrap_or(0.0);
            let sep = block_g.edge_with_obj(&e).cloned().unwrap_or(0.0);
            let ev = succ_x - sep;

            acc.min(ev as f64)
        });

        if min != f64::INFINITY {
            let Some(node) = g.node(elem) else {
                return;
            };
            if node.border_type.as_ref() != Some(border_type) {
                xs.insert(
                    elem.clone(),
                    xs.get(elem).cloned().unwrap_or(0.0).max(min as f32),
                );
            }
        }
    }

    iterate(
        pass1,
        Box::new(|block_g, v| -> Vec<String> { block_g.predecessors(v).unwrap_or(vec![]) }),
        &block_g,
        &mut xs,
        g,
        &border_type,
    );

    iterate(
        pass2,
        Box::new(|block_g, v| -> Vec<String> { block_g.successors(v).unwrap_or(vec![]) }),
        &block_g,
        &mut xs,
        g,
        &border_type,
    );

    // Assign x coordinates to all nodes
    align.iter().for_each(|v| {
        let Some(root_id) = root.get(v) else {
            return;
        };
        let Some(root_x) = xs.get(root_id).cloned() else {
            return;
        };
        xs.insert(v.clone(), root_x);
    });

    xs
}

pub fn build_block_graph(
    g: &Graph<GraphConfig, GraphNode, GraphEdge>,
    layering: &Vec<Vec<String>>,
    root: &OrderedHashMap<String, String>,
    reverse_sep: bool,
) -> Graph<GraphOption, String, f32> {
    let mut block_graph: Graph<GraphOption, String, f32> = Graph::new(None);
    let graph_label = g.graph();
    let sep_fn: Box<dyn Fn(&Graph<GraphConfig, GraphNode, GraphEdge>, &String, &String) -> f32> =
        sep(
            graph_label.nodesep.as_ref().unwrap(),
            graph_label.edgesep.as_ref().unwrap(),
            &reverse_sep,
        );

    layering.iter().for_each(|layer| {
        let mut u: Option<String> = None;
        layer.iter().for_each(|v| {
            let v_root = root.get(v).unwrap();
            block_graph.set_node(v_root.clone(), None);
            if u.is_some() {
                let u_ = u.as_ref().unwrap();
                let u_root = root.get(u_).unwrap();
                if u_root != v_root {
                    let prev_max = block_graph
                        .edge(&u_root, &v_root, None)
                        .cloned()
                        .unwrap_or(0.0);

                    let _ = block_graph.set_edge(
                        &u_root,
                        &v_root,
                        Some((sep_fn(g, v, u_) as f32).max(prev_max)),
                        None,
                    );
                }
            }
            u = Some(v.clone());
        });
    });

    block_graph
}

/*
 * Returns the alignment that has the smallest width of the given alignments.
 */
pub fn find_smallest_width_alignment<'a>(
    g: &Graph<GraphConfig, GraphNode, GraphEdge>,
    xss: &'a OrderedHashMap<String, OrderedHashMap<String, f32>>,
) -> &'a OrderedHashMap<String, f32> {
    xss.values()
        .min_by(|xs1, xs2| {
            let mut max1 = f64::NEG_INFINITY;
            let mut min1 = f64::INFINITY;

            xs1.iter().for_each(|(v, x)| {
                let half_width = width(g, v) / 2.0;
                max1 = max1.max((*x + half_width) as f64);
                min1 = min1.min((*x - half_width) as f64);
            });

            let r1 = max1 - min1;

            let mut max2 = f64::NEG_INFINITY;
            let mut min2 = f64::INFINITY;

            xs2.iter().for_each(|(v, x)| {
                let half_width = width(g, v) / 2.0;
                max2 = max2.max((*x + half_width) as f64);
                min2 = min2.min((*x - half_width) as f64);
            });

            let r2 = max2 - min2;

            r1.total_cmp(&r2)
        })
        .unwrap()
}

/*
 * Align the coordinates of each of the layout alignments such that
 * left-biased alignments have their minimum coordinate at the same point as
 * the minimum coordinate of the smallest width alignment and right-biased
 * alignments have their maximum coordinate at the same point as the maximum
 * coordinate of the smallest width alignment.
 */
fn align_coordinates(
    xss: &mut OrderedHashMap<String, OrderedHashMap<String, f32>>,
    align_to: &OrderedHashMap<String, f32>,
) {
    let align_to_vals: Vec<f32> = align_to.values().cloned().collect();
    let align_to_min = align_to_vals
        .iter()
        .min_by(|a, b| a.partial_cmp(b).unwrap())
        .unwrap()
        .clone();
    let align_to_max = align_to_vals
        .iter()
        .max_by(|a, b| a.partial_cmp(b).unwrap())
        .unwrap()
        .clone();

    vec!["u", "d"].iter().for_each(|vert| {
        vec!["l", "r"].iter().for_each(|horiz| {
            let alignment = vert.to_string() + horiz;
            let xs = xss.get(&alignment).unwrap();
            if xs == align_to {
                return;
            }

            let xs_vals: Vec<f32> = xs.values().cloned().collect();
            let delta = if *horiz == "l" {
                align_to_min
                    - *xs_vals
                        .iter()
                        .min_by(|a, b| a.partial_cmp(b).unwrap())
                        .unwrap()
            } else {
                align_to_max
                    - *xs_vals
                        .iter()
                        .max_by(|a, b| a.partial_cmp(b).unwrap())
                        .unwrap()
            };

            if delta != 0.0 {
                let _xs = xss.get_mut(&alignment).unwrap();
                _xs.values_mut().for_each(|x| {
                    *x += delta;
                });
            }
        })
    })
}

pub fn balance(
    xss: &OrderedHashMap<String, OrderedHashMap<String, f32>>,
    align: Option<String>,
) -> OrderedHashMap<String, f32> {
    let mut xss_clone = xss.clone();
    if let Some(ul) = xss_clone.get_mut(&"ul".to_string()) {
        let keys: Vec<String> = ul.keys().cloned().collect();
        keys.iter().for_each(|v| {
            if align.is_some() {
                let empty_hash: OrderedHashMap<String, f32> = OrderedHashMap::new();
                let empty_string = "".to_string();
                let _balance = xss
                    .get(align.as_ref().unwrap_or(&empty_string))
                    .unwrap_or(&empty_hash)
                    .get(v)
                    .cloned()
                    .unwrap_or(0.0);
                let item = ul.get_mut(v).unwrap();
                *item = _balance;
            } else {
                let mut xs: Vec<f32> = xss
                    .values()
                    .map(|_xs| _xs.get(v).cloned().unwrap_or(f64::INFINITY as f32))
                    .collect();
                xs.sort_by(|f1, f2| f1.total_cmp(f2));
                let xs1 = xs.get(1).cloned().unwrap_or(0.0);
                let xs2 = xs.get(2).cloned().unwrap_or(0.0);
                let item = ul.get_mut(v).unwrap();
                *item = (xs1 + xs2) / 2.0;
            }
        });
    }

    xss_clone.get_mut(&"ul".to_string()).unwrap().to_owned()
}

pub fn position_x(g: &mut Graph<GraphConfig, GraphNode, GraphEdge>) -> OrderedHashMap<String, f32> {
    let layering = util::build_layer_matrix(g);
    if layering.iter().all(|layer| layer.is_empty()) {
        return OrderedHashMap::new();
    }
    let mut conflicts = find_type_1_conflicts(g, &layering);
    conflicts.extend(find_type_2_conflicts(g, &layering));

    let mut xss: OrderedHashMap<String, OrderedHashMap<String, f32>> = OrderedHashMap::new();
    let mut adjusted_layering: Option<Vec<Vec<String>>> = None;
    vec!["u", "d"].iter().for_each(|vert| {
        adjusted_layering = Some(if vert == &"u" {
            layering.clone()
        } else {
            let mut layering_ = layering.clone();
            layering_.reverse();
            layering_
        });

        vec!["l", "r"].iter().for_each(|horiz| {
            if horiz == &"r" {
                adjusted_layering
                    .as_mut()
                    .unwrap()
                    .iter_mut()
                    .for_each(|inner| inner.reverse());
            }

            let neighbor_fn: Box<
                dyn Fn(&Graph<GraphConfig, GraphNode, GraphEdge>, &String) -> Vec<String>,
            > = if vert == &"u" {
                Box::new(
                    |g: &Graph<GraphConfig, GraphNode, GraphEdge>, v: &String| -> Vec<String> {
                        g.predecessors(v).unwrap_or(vec![])
                    },
                )
            } else {
                Box::new(
                    |g: &Graph<GraphConfig, GraphNode, GraphEdge>, v: &String| -> Vec<String> {
                        g.successors(v).unwrap_or(vec![])
                    },
                )
            };
            let align = vertical_alignment(
                g,
                adjusted_layering.as_ref().unwrap(),
                &conflicts,
                neighbor_fn,
            );
            let mut xs = horizontal_compaction(
                g,
                adjusted_layering.as_ref().unwrap(),
                &align.0,
                &align.1,
                horiz == &"r",
            );
            if horiz == &"r" {
                let mut xs_: OrderedHashMap<String, f32> = OrderedHashMap::new();
                xs.iter().for_each(|(k, v)| {
                    xs_.insert(k.clone(), -v.clone());
                });
                xs = xs_;
            }

            xss.insert(String::from(vert.to_string() + horiz), xs);
        });
    });

    let smallest_width = find_smallest_width_alignment(g, &xss).clone();
    align_coordinates(&mut xss, &smallest_width);
    return balance(&xss, g.graph().align.clone());
}

fn sep(
    node_sep: &f32,
    edge_sep: &f32,
    reverse_sep: &bool,
) -> Box<dyn Fn(&Graph<GraphConfig, GraphNode, GraphEdge>, &String, &String) -> f32> {
    let node_sep_ = node_sep.clone();
    let edge_sep_ = edge_sep.clone();
    let reverse_sep_ = reverse_sep.clone();
    Box::new(
        move |g: &Graph<GraphConfig, GraphNode, GraphEdge>, v: &String, w: &String| -> f32 {
            let v_label = g.node(v).unwrap();
            let w_label = g.node(w).unwrap();
            let mut sum: f32 = 0.0;
            let mut delta: f32 = 0.0;

            sum += v_label.width / 2.0;
            if let Some(v_label_labelpos) = v_label.labelpos.as_ref() {
                if v_label_labelpos == "l" {
                    delta = -v_label.width / 2.0;
                } else if v_label_labelpos == "r" {
                    delta = v_label.width / 2.0;
                }
            }
            if delta != 0.0 {
                sum += if reverse_sep_ { delta } else { -delta }
            }

            sum += if v_label.dummy.is_some() {
                edge_sep_
            } else {
                node_sep_
            } / 2.0;
            sum += if w_label.dummy.is_some() {
                edge_sep_
            } else {
                node_sep_
            } / 2.0;

            sum += w_label.width / 2.0;
            delta = 0.0;
            if let Some(w_label_labelpos) = w_label.labelpos.as_ref() {
                if w_label_labelpos == "l" {
                    delta = w_label.width / 2.0;
                } else if w_label_labelpos == "r" {
                    delta = -w_label.width / 2.0;
                }
            }
            if delta != 0.0 {
                sum += if reverse_sep_ { delta } else { -delta }
            }

            sum
        },
    )
}

fn width(g: &Graph<GraphConfig, GraphNode, GraphEdge>, v: &String) -> f32 {
    g.node(v).unwrap().width
}
