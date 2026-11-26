use crate::{GraphConfig, GraphEdge, GraphNode};
use graphlib_rust::Graph;

#[derive(Debug, Clone)]
pub struct Barycenter {
    pub v: String,
    pub barycenter: Option<f32>,
    pub weight: Option<f32>,
}

pub fn barycenter(
    g: &Graph<GraphConfig, GraphNode, GraphEdge>,
    movable: &Vec<String>,
) -> Vec<Barycenter> {
    movable
        .iter()
        .map(|v| {
            let in_v = g.in_edges(v, None).unwrap_or(vec![]);
            if in_v.len() == 0 {
                return Barycenter {
                    v: v.clone(),
                    barycenter: None,
                    weight: None,
                };
            }

            //( sum, weight )
            let mut sum = 0.0_f64;
            let mut weight = 0.0_f64;
            in_v.iter().for_each(|e| {
                let edge = g.edge_with_obj(&e).unwrap();
                let node_u = g.node(&e.v).unwrap();
                let edge_weight = edge.weight.clone().unwrap_or(0.0) as f64;
                sum += edge_weight * (node_u.order.clone().unwrap_or(0) as f64);
                weight += edge_weight;
            });

            return Barycenter {
                v: v.clone(),
                barycenter: Some((sum / weight) as f32),
                weight: Some(weight as f32),
            };
        })
        .collect()
}
