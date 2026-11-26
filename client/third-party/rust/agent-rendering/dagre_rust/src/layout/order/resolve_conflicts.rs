/*
 * Given a list of entries of the form {v, barycenter, weight} and a
 * constraint graph this function will resolve any conflicts between the
 * constraint graph and the barycenters for the entries. If the barycenters for
 * an entry would violate a constraint in the constraint graph then we coalesce
 * the nodes in the conflict into a new node that respects the contraint and
 * aggregates barycenter and weight information.
 *
 * This implementation is based on the description in Forster, "A Fast and
 * Simple Hueristic for Constrained Two-Level Crossing Reduction," thought it
 * differs in some specific details.
 *
 * Pre-conditions:
 *
 *    1. Each entry has the form {v, barycenter, weight}, or if the node has
 *       no barycenter, then {v}.
 *
 * Returns:
 *
 *    A new list of entries of the form {vs, i, barycenter, weight}. The list
 *    `vs` may either be a singleton or it may be an aggregation of nodes
 *    ordered such that they do not violate constraints from the constraint
 *    graph. The property `i` is the lowest original index of any of the
 *    elements in `vs`.
 */
use std::collections::HashMap;

use graphlib_rust::Graph;

use crate::layout::order::barycenter::Barycenter;
use crate::{GraphConfig, GraphEdge, GraphNode};

#[derive(Debug, Clone)]
pub struct ResolvedBaryEntry {
    pub vs: Vec<String>,
    pub i: usize,
    pub barycenter: Option<f32>,
    pub weight: Option<f32>,
}

#[derive(Debug, Clone)]
struct ConflictEntry {
    indegree: i32,
    ins: Vec<usize>,
    outs: Vec<usize>,
    vs: Vec<String>,
    i: usize,
    barycenter: Option<f32>,
    weight: Option<f32>,
    merged: bool,
}

pub fn resolve_conflicts(
    entries: &Vec<Barycenter>,
    cg: &Graph<GraphConfig, GraphNode, GraphEdge>,
) -> Vec<ResolvedBaryEntry> {
    let mut id_to_idx: HashMap<String, usize> = HashMap::new();
    let mut mapped_entries: Vec<ConflictEntry> = Vec::with_capacity(entries.len());

    for (i, entry) in entries.iter().enumerate() {
        id_to_idx.insert(entry.v.clone(), i);
        mapped_entries.push(ConflictEntry {
            indegree: 0,
            ins: vec![],
            outs: vec![],
            vs: vec![entry.v.clone()],
            i,
            barycenter: entry.barycenter,
            weight: entry.weight,
            merged: false,
        });
    }

    cg.edges().iter().for_each(|e| {
        if let (Some(&v_idx), Some(&w_idx)) = (id_to_idx.get(&e.v), id_to_idx.get(&e.w)) {
            mapped_entries[w_idx].indegree += 1;
            mapped_entries[v_idx].outs.push(w_idx);
        }
    });

    let mut source_set: Vec<usize> = mapped_entries
        .iter()
        .enumerate()
        .filter_map(|(idx, entry)| if entry.indegree == 0 { Some(idx) } else { None })
        .collect();

    let mut entries_order: Vec<usize> = Vec::new();

    while let Some(v_idx) = source_set.pop() {
        entries_order.push(v_idx);

        let ins = mapped_entries[v_idx].ins.clone();
        ins.into_iter().rev().for_each(|u_idx| {
            handle_in(&mut mapped_entries, v_idx, u_idx);
        });

        let outs = mapped_entries[v_idx].outs.clone();
        outs.into_iter().for_each(|w_idx| {
            handle_out(&mut mapped_entries, v_idx, w_idx, &mut source_set);
        });
    }

    entries_order
        .into_iter()
        .filter(|idx| !mapped_entries[*idx].merged)
        .map(|idx| {
            let entry = &mapped_entries[idx];
            ResolvedBaryEntry {
                vs: entry.vs.clone(),
                i: entry.i,
                barycenter: entry.barycenter,
                weight: entry.weight,
            }
        })
        .collect()
}

fn handle_in(entries: &mut [ConflictEntry], v_idx: usize, u_idx: usize) {
    if entries[u_idx].merged {
        return;
    }

    let u_barycenter = entries[u_idx].barycenter;
    let v_barycenter = entries[v_idx].barycenter;
    if u_barycenter.is_none()
        || v_barycenter.is_none()
        || u_barycenter.unwrap() >= v_barycenter.unwrap()
    {
        merge_entries(entries, v_idx, u_idx);
    }
}

fn handle_out(
    entries: &mut [ConflictEntry],
    v_idx: usize,
    w_idx: usize,
    source_set: &mut Vec<usize>,
) {
    entries[w_idx].ins.push(v_idx);
    entries[w_idx].indegree -= 1;
    if entries[w_idx].indegree == 0 {
        source_set.push(w_idx);
    }
}

fn merge_entries(entries: &mut [ConflictEntry], target_idx: usize, source_idx: usize) {
    let mut sum = 0.0;
    let mut weight = 0.0;

    if let (Some(target_barycenter), Some(target_weight)) =
        (entries[target_idx].barycenter, entries[target_idx].weight)
    {
        sum += target_barycenter * target_weight;
        weight += target_weight;
    }

    if let (Some(source_barycenter), Some(source_weight)) =
        (entries[source_idx].barycenter, entries[source_idx].weight)
    {
        sum += source_barycenter * source_weight;
        weight += source_weight;
    }

    let mut vs = entries[source_idx].vs.clone();
    vs.extend(entries[target_idx].vs.clone());

    entries[target_idx].vs = vs;
    entries[target_idx].barycenter = Some(sum / weight);
    entries[target_idx].weight = Some(weight);
    entries[target_idx].i = std::cmp::min(entries[source_idx].i, entries[target_idx].i);

    entries[source_idx].merged = true;
}
