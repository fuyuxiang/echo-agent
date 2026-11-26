use crate::layout::util::add_dummy_node;
use crate::{GraphConfig, GraphEdge, GraphNode};
use graphlib_rust::graph::GRAPH_NODE;
use graphlib_rust::Graph;
use ordered_hashmap::OrderedHashMap;

#[derive(Debug, Clone, PartialEq)]
pub enum BorderTypeName {
    BorderLeft,
    BorderRight,
}

pub fn add_border_segments(g: &mut Graph<GraphConfig, GraphNode, GraphEdge>) {
    fn dfs(v: &String, g: &mut Graph<GraphConfig, GraphNode, GraphEdge>) {
        let children = g.children(v);
        if children.len() > 0 {
            for cv in children.iter() {
                dfs(cv, g);
            }
        }
        let node = g.node_mut(v).unwrap();
        if node.min_rank.is_some() {
            node.border_left = Some(OrderedHashMap::new());
            node.border_right = Some(OrderedHashMap::new());
            let mut rank = node.min_rank.clone().unwrap_or(0);
            let max_rank = node.max_rank.clone().unwrap_or(0) + 1;
            while rank < max_rank {
                add_border_node(g, BorderTypeName::BorderLeft, "_bl", v, &rank);
                add_border_node(g, BorderTypeName::BorderRight, "_br", v, &rank);
                rank += 1;
            }
        }
    }

    let children = g.children(&GRAPH_NODE.to_string());
    for v in children.iter() {
        dfs(v, g);
    }
}

fn add_border_node(
    g: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
    prop: BorderTypeName,
    prefix: &str,
    sg: &String,
    rank: &i32,
) {
    let mut label = GraphNode::default();
    label.rank = Some(rank.clone());
    label.border_type = Some(prop.clone());

    let curr = add_dummy_node(g, "border".to_string(), label, prefix.to_string());

    let sg_node = g.node_mut(sg).unwrap();
    let mut border = sg_node.border_left.as_mut().unwrap();
    match prop {
        BorderTypeName::BorderRight => {
            border = sg_node.border_right.as_mut().unwrap();
        }
        _ => (),
    }
    border.insert(rank.clone(), curr.clone());

    let prev = border.get(&(rank - 1));
    if prev.is_some() {
        let prev_v = prev.cloned().unwrap();
        let mut graph_edge = GraphEdge::default();
        graph_edge.weight = Some(1.0);
        let _ = g.set_edge(&prev_v, &curr, Some(graph_edge), None);
    }

    let _ = g.set_parent(&curr, Some(sg.clone()));
}
