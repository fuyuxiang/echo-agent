use crate::{GraphConfig, GraphEdge, GraphNode};
use graphlib_rust::graph::GRAPH_NODE;
use graphlib_rust::Graph;
use ordered_hashmap::OrderedHashMap;

pub fn parent_dummy_chains(g: &mut Graph<GraphConfig, GraphNode, GraphEdge>) {
    let post_order_nums: OrderedHashMap<String, (i32, i32)> = postorder(g);
    let dummy_chains = g.graph().dummy_chains.clone().unwrap_or(vec![]);

    for v_ in dummy_chains.iter() {
        let mut v = v_.clone();
        let mut node = g.node(&v).unwrap();
        let edge_obj = node.edge_obj.clone().unwrap();
        let path_data = find_path(g, &post_order_nums, &edge_obj.v, &edge_obj.w);
        let path = path_data.0;
        let lca = path_data.1;

        let mut path_idx = 0;
        let mut path_v = path.get(path_idx).cloned().unwrap_or(lca.clone());
        let mut ascending = true;

        while v != edge_obj.w {
            node = g.node(&v).unwrap();
            let node_rank = node.rank.unwrap_or(0);

            if ascending {
                loop {
                    path_v = path.get(path_idx).cloned().unwrap_or(lca.clone());
                    if path_v == lca {
                        ascending = false;
                        break;
                    }

                    let Some(path_v_id) = path_v.as_ref() else {
                        break;
                    };
                    let max_rank = g.node(path_v_id).unwrap().max_rank.unwrap_or(0);
                    if max_rank < node_rank {
                        path_idx += 1;
                        continue;
                    }

                    break;
                }
            }

            if !ascending {
                while path_idx < path.len().saturating_sub(1) {
                    let Some(next) = path.get(path_idx + 1).cloned() else {
                        break;
                    };

                    let Some(next_id) = next.as_ref() else {
                        break;
                    };

                    if g.node(next_id).unwrap().min_rank.unwrap_or(0) <= node_rank {
                        path_idx += 1;
                    } else {
                        break;
                    }
                }

                path_v = path.get(path_idx).cloned().unwrap_or(lca.clone());
            }

            let _ = g.set_parent(&v, path_v.clone());

            let Some(next) = g.successors(&v).unwrap_or_default().first().cloned() else {
                break;
            };
            v = next;
        }
    }
}

// Find a path from v to w through the lowest common ancestor (LCA). Return the
// full path and the LCA.
fn find_path(
    g: &Graph<GraphConfig, GraphNode, GraphEdge>,
    post_order_nums: &OrderedHashMap<String, (i32, i32)>,
    v: &String,
    w: &String,
) -> (Vec<Option<String>>, Option<String>) {
    let mut v_path: Vec<Option<String>> = vec![];
    let mut w_path: Vec<Option<String>> = vec![];

    let v_post_order_num = post_order_nums.get(v).cloned().unwrap_or((0, 0));
    let w_post_order_num = post_order_nums.get(w).cloned().unwrap_or((0, 0));
    let low = std::cmp::min(v_post_order_num.0, w_post_order_num.0);
    let lim = std::cmp::max(v_post_order_num.1, w_post_order_num.1);

    let mut parent: Option<String> = Some(v.clone());
    loop {
        parent = parent.and_then(|p| g.parent(&p).cloned());
        v_path.push(parent.clone());

        let Some(parent_id) = parent.as_ref() else {
            break;
        };

        let Some(post_order_num) = post_order_nums.get(parent_id) else {
            break;
        };
        if post_order_num.0 <= low && lim <= post_order_num.1 {
            break;
        }
    }

    let lca = parent.clone();

    parent = Some(w.clone());
    loop {
        parent = parent.and_then(|p| g.parent(&p).cloned());
        if parent == lca {
            break;
        }

        w_path.push(parent.clone());

        if parent.is_none() {
            break;
        }
    }

    w_path.reverse();
    v_path.extend(w_path);
    (v_path, lca)
}

fn postorder(g: &Graph<GraphConfig, GraphNode, GraphEdge>) -> OrderedHashMap<String, (i32, i32)> {
    let mut result: OrderedHashMap<String, (i32, i32)> = OrderedHashMap::new();
    let mut lim = 0;

    fn dfs(
        v: &String,
        g: &Graph<GraphConfig, GraphNode, GraphEdge>,
        lim: &mut i32,
        result: &mut OrderedHashMap<String, (i32, i32)>,
    ) {
        let low = lim.clone();
        g.children(&v).iter().for_each(|v_| {
            dfs(v_, g, lim, result);
        });
        result.insert(v.clone(), (low, lim.clone()));
        *lim += 1;
    }

    g.children(&GRAPH_NODE.to_string()).iter().for_each(|v| {
        dfs(v, g, &mut lim, &mut result);
    });

    return result;
}
