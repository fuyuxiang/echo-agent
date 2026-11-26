use crate::Graph;
use std::collections::HashMap;
use std::error::Error;
use std::fmt::Debug;

/*
 * A helper that preforms a pre- or post-order traversal on the input graph
 * and returns the nodes in the order they were visited. If the graph is
 * undirected then this algorithm will navigate using neighbors. If the graph
 * is directed then this algorithm will navigate using successors.
 *
 * If the order is not "post", it will be treated as "pre".
 */
pub fn dfs<GL: Default, N: Default + Clone + Debug, E: Default + Clone + Debug>(
    g: &mut Graph<GL, N, E>,
    vs: &Vec<String>,
    order: &str,
) -> Result<Vec<String>, Box<dyn Error>> {
    let navigation: Box<dyn Fn(&String, &Graph<GL, N, E>) -> Vec<String>> =
        Box::new(move |v: &String, g: &Graph<GL, N, E>| {
            if g.is_directed() {
                g.successors(&v).unwrap_or(vec![])
            } else {
                g.neighbors(&v).unwrap_or(vec![])
            }
        });

    let order_func = match order {
        "post" => post_order_dfs,
        _ => pre_order_dfs,
    };

    let mut acc: Vec<String> = vec![];
    let mut visited: HashMap<String, bool> = HashMap::new();
    for v in vs.iter() {
        if !g.has_node(v) {
            return Err(format!("Graph does not have node: {}", v).into());
        }

        order_func(v, &navigation, &mut visited, &mut acc, g);
    }

    Ok(acc)
}

fn post_order_dfs<GL: Default, N: Default + Clone + Debug, E: Default + Clone + Debug>(
    v: &String,
    navigation: &Box<dyn Fn(&String, &Graph<GL, N, E>) -> Vec<String>>,
    visited: &mut HashMap<String, bool>,
    acc: &mut Vec<String>,
    g: &Graph<GL, N, E>,
) {
    let mut stack: Vec<(String, bool)> = vec![(v.clone(), false)];
    while stack.len() > 0 {
        let curr_ = stack.pop();
        if curr_.is_none() {
            continue;
        }
        let curr = curr_.unwrap();
        if curr.1 {
            acc.push(curr.0.clone());
        } else {
            if !visited.contains_key(&curr.0) {
                visited.insert(curr.0.clone(), true);
                stack.push((curr.0.clone(), true));

                // TODO: for_each_right implement in future
                let _navigation_nodes: Vec<String> = navigation(&curr.0, g);
                let mut idx = _navigation_nodes.len();
                while idx > 0 {
                    let nav_node = _navigation_nodes.get(idx - 1).cloned().unwrap();
                    stack.push((nav_node, false));
                    idx -= 1;
                }
            }
        }
    }
}

fn pre_order_dfs<GL: Default, N: Default + Clone, E: Default + Clone>(
    v: &String,
    navigation: &Box<dyn Fn(&String, &Graph<GL, N, E>) -> Vec<String>>,
    visited: &mut HashMap<String, bool>,
    acc: &mut Vec<String>,
    g: &Graph<GL, N, E>,
) {
    let mut stack: Vec<String> = vec![v.clone()];
    while stack.len() > 0 {
        let curr_ = stack.pop();
        if curr_.is_none() {
            continue;
        }
        let curr = curr_.unwrap();
        if !visited.contains_key(&curr) {
            visited.insert(curr.clone(), true);
            acc.push(curr.clone());

            // TODO: for_each_right implement in future
            let _navigation_nodes: Vec<String> = navigation(&curr, g);
            let mut idx = _navigation_nodes.len() as i32;
            while idx >= 0 {
                stack.push(String::from(_navigation_nodes.get(idx as usize).unwrap()));
                idx -= 1;
            }
        }
    }
}
