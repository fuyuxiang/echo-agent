pub mod bk;

use crate::layout::position::bk::position_x;
use crate::layout::util;
use crate::{GraphConfig, GraphEdge, GraphNode};
use graphlib_rust::Graph;

pub fn position(g: &mut Graph<GraphConfig, GraphNode, GraphEdge>) {
    let mut ncg: Graph<GraphConfig, GraphNode, GraphEdge> = util::as_non_compound_graph(g);

    position_y(&mut ncg);
    position_x(&mut ncg).iter().for_each(|(v, x)| {
        g.node_mut(v).unwrap().x = x.clone();
        g.node_mut(v).unwrap().y = ncg.node(v).unwrap().y;
    });
}

fn position_y(g: &mut Graph<GraphConfig, GraphNode, GraphEdge>) {
    let layering = util::build_layer_matrix(g);
    let rank_sep = g.graph().ranksep.clone().unwrap();
    let mut prev_y = 0.0;
    layering.iter().for_each(|layer| {
        let max_height: f32 = layer
            .iter()
            .map(|v| g.node(v).unwrap().height as i32)
            .max()
            .unwrap_or(0) as f32;

        layer.iter().for_each(|v| {
            let node = g.node_mut(v).unwrap();
            node.y = prev_y + max_height / 2.0;
        });

        prev_y += max_height + rank_sep;
    });
}
