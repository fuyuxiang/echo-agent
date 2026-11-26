use crate::{GraphConfig, GraphEdge, GraphNode};
use graphlib_rust::Graph;

pub fn adjust(g: &mut Graph<GraphConfig, GraphNode, GraphEdge>) {
    let rank_dir = g.graph().rankdir.clone().unwrap();
    if &rank_dir == "lr" || &rank_dir == "rl" {
        swap_width_height(g);
    }
}

pub fn undo(g: &mut Graph<GraphConfig, GraphNode, GraphEdge>) {
    let rank_dir = g.graph().rankdir.clone().unwrap();
    if &rank_dir == "bt" || &rank_dir == "rl" {
        reverse_y(g);
    }

    if &rank_dir == "lr" || &rank_dir == "rl" {
        swap_x_y(g);
        swap_width_height(g);
    }
}

fn swap_width_height(g: &mut Graph<GraphConfig, GraphNode, GraphEdge>) {
    let nodes = g.nodes();
    // =swapWidthHeightOne
    nodes.iter().for_each(|v| {
        let node = g.node_mut(v).unwrap();
        let w = node.width.clone();
        node.width = node.height;
        node.height = w;
    });
    let edges = g.edges();
    // =swapWidthHeightOne
    edges.iter().for_each(|e| {
        let edge_label = g.edge_mut_with_obj(&e).unwrap();
        let w = edge_label.width.clone();
        edge_label.width = edge_label.height;
        edge_label.height = w;
    });
}

fn reverse_y(g: &mut Graph<GraphConfig, GraphNode, GraphEdge>) {
    let nodes = g.nodes();
    nodes.iter().for_each(|v| {
        // =reverseYOne
        let node = g.node_mut(v).unwrap();
        node.y = -node.y;
    });

    let edges = g.edges();
    edges.iter().for_each(|e| {
        // =reverseYOne
        let edge_label = g.edge_mut_with_obj(&e).unwrap();
        let mut points = edge_label.points.clone().unwrap_or(vec![]);
        points.iter_mut().for_each(|point| {
            point.y = -point.y;
        });
        edge_label.points = Some(points);
        edge_label.y = -edge_label.y;
    });
}

fn swap_x_y(g: &mut Graph<GraphConfig, GraphNode, GraphEdge>) {
    let nodes = g.nodes();
    nodes.iter().for_each(|v| {
        // =swapXYOne
        let node = g.node_mut(v).unwrap();
        let x = node.x.clone();
        node.x = node.y;
        node.y = x;
    });

    let edges = g.edges();
    edges.iter().for_each(|e| {
        // =swapXYOne
        let edge_label = g.edge_mut_with_obj(&e).unwrap();
        let mut points = edge_label.points.clone().unwrap_or(vec![]);
        points.iter_mut().for_each(|point| {
            let x = point.x.clone();
            point.x = point.y;
            point.y = x;
        });
        edge_label.points = Some(points);

        let x = edge_label.x.clone();
        edge_label.x = edge_label.y;
        edge_label.y = x;
    });
}
