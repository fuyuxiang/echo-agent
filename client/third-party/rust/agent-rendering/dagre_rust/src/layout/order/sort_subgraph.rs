use graphlib_rust::Graph;
use ordered_hashmap::OrderedHashMap;

use crate::layout::order::barycenter::{barycenter, Barycenter};
use crate::layout::order::resolve_conflicts::{resolve_conflicts, ResolvedBaryEntry};
use crate::layout::order::sort::sort;
use crate::{GraphConfig, GraphEdge, GraphNode};

#[derive(Debug, Clone, Default)]
pub struct SubgraphResult {
    pub vs: Vec<String>,
    pub barycenter: Option<f32>,
    pub weight: Option<f32>,
}

pub fn sort_subgraph(
    g: &Graph<GraphConfig, GraphNode, GraphEdge>,
    v: &String,
    cg: &Graph<GraphConfig, GraphNode, GraphEdge>,
    bias_right: &bool,
) -> SubgraphResult {
    let mut movable = g.children(v);
    let node = g.node(v);
    let bl = node.and_then(|n| n.border_left_.clone());
    let br = node.and_then(|n| n.border_right_.clone());
    let mut subgraphs: OrderedHashMap<String, SubgraphResult> = OrderedHashMap::new();

    if let (Some(bl_), Some(br_)) = (bl.as_ref(), br.as_ref()) {
        movable = movable
            .into_iter()
            .filter(|w| w != bl_ && w != br_)
            .collect();
    }

    let mut barycenters = barycenter(g, &movable);
    barycenters.iter_mut().for_each(|entry| {
        if !g.children(&entry.v).is_empty() {
            let subgraph_result = sort_subgraph(g, &entry.v, cg, bias_right);
            subgraphs.insert(entry.v.clone(), subgraph_result.clone());
            if subgraph_result.barycenter.is_some() {
                merge_barycenters(entry, &subgraph_result);
            }
        }
    });

    let mut entries = resolve_conflicts(&barycenters, cg);
    expand_subgraphs(&mut entries, &subgraphs);

    let mut result = sort(&entries, bias_right);
    if let (Some(bl_), Some(br_)) = (bl, br) {
        let mut vs: Vec<String> = Vec::with_capacity(result.vs.len() + 2);
        vs.push(bl_.clone());
        vs.extend(result.vs.clone());
        vs.push(br_.clone());
        result.vs = vs;

        let bl_preds = g.predecessors(&bl_).unwrap_or_default();
        if !bl_preds.is_empty() {
            let br_preds = g.predecessors(&br_).unwrap_or_default();
            if !br_preds.is_empty() {
                let bl_pred = g.node(&bl_preds[0]).unwrap();
                let br_pred = g.node(&br_preds[0]).unwrap();
                let bl_pred_order = bl_pred.order.unwrap_or(0) as f32;
                let br_pred_order = br_pred.order.unwrap_or(0) as f32;

                let result_barycenter = result.barycenter.unwrap_or(0.0);
                let result_weight = result.weight.unwrap_or(0.0);
                result.barycenter = Some(
                    (result_barycenter * result_weight + bl_pred_order + br_pred_order)
                        / (result_weight + 2.0),
                );
                result.weight = Some(result_weight + 2.0);
            }
        }
    }

    result
}

fn expand_subgraphs(
    entries: &mut Vec<ResolvedBaryEntry>,
    subgraphs: &OrderedHashMap<String, SubgraphResult>,
) {
    entries.iter_mut().for_each(|entry| {
        let mut vs: Vec<String> = vec![];
        entry.vs.iter().for_each(|v| {
            if let Some(subgraph) = subgraphs.get(v) {
                vs.extend(subgraph.vs.clone());
                return;
            }
            vs.push(v.clone());
        });

        entry.vs = vs;
    });
}

fn merge_barycenters(target: &mut Barycenter, other: &SubgraphResult) {
    let (other_barycenter, other_weight) = match (other.barycenter, other.weight) {
        (Some(barycenter), Some(weight)) => (barycenter, weight),
        _ => return,
    };

    if let (Some(target_barycenter), Some(target_weight)) = (target.barycenter, target.weight) {
        target.barycenter = Some(
            (target_barycenter * target_weight + other_barycenter * other_weight)
                / (target_weight + other_weight),
        );
        target.weight = Some(target_weight + other_weight);
    } else {
        target.barycenter = Some(other_barycenter);
        target.weight = Some(other_weight);
    }
}
