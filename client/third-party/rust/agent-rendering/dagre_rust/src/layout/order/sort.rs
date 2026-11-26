use std::cmp::Ordering;

use crate::layout::order::resolve_conflicts::ResolvedBaryEntry;
use crate::layout::order::sort_subgraph::SubgraphResult;
use crate::layout::util;
use crate::layout::util::PartitionResponse;

pub fn sort(entries: &Vec<ResolvedBaryEntry>, bias_right: &bool) -> SubgraphResult {
    let parts: PartitionResponse<ResolvedBaryEntry> = util::partition(
        entries,
        Box::new(|val: &ResolvedBaryEntry| -> bool { val.barycenter.is_some() }),
    );
    let mut sortable = parts.lhs.clone();
    let mut unsortable = parts.rhs.clone();

    sortable.sort_by(|e1, e2| compare_with_bias(e1, e2, bias_right));
    unsortable.sort_by(|e1, e2| e2.i.cmp(&e1.i));

    let mut vs: Vec<Vec<String>> = vec![];
    let mut sum = 0.0;
    let mut weight = 0.0;
    let mut vs_index: usize = 0;

    vs_index = consume_unsortable(&mut vs, &mut unsortable, vs_index);
    sortable.iter().for_each(|entry| {
        vs_index += entry.vs.len();
        vs.push(entry.vs.clone());
        let entry_weight = entry.weight.unwrap_or(0.0);
        sum += entry.barycenter.unwrap_or(0.0) * entry_weight;
        weight += entry_weight;
        vs_index = consume_unsortable(&mut vs, &mut unsortable, vs_index);
    });

    let mut result = SubgraphResult::default();
    result.vs = vs.into_iter().flatten().collect();
    if weight != 0.0 {
        result.barycenter = Some(sum / weight);
        result.weight = Some(weight);
    }

    result
}

fn consume_unsortable(
    vs: &mut Vec<Vec<String>>,
    unsortable: &mut Vec<ResolvedBaryEntry>,
    mut index: usize,
) -> usize {
    loop {
        let last = match unsortable.last() {
            Some(last) => last,
            None => return index,
        };

        if last.i > index {
            return index;
        }

        let last = unsortable.pop().unwrap();
        vs.push(last.vs);
        index += 1;
    }
}

fn compare_with_bias(
    entry_v: &ResolvedBaryEntry,
    entry_w: &ResolvedBaryEntry,
    bias: &bool,
) -> Ordering {
    let barycenter_v = entry_v.barycenter.unwrap_or(0.0);
    let barycenter_w = entry_w.barycenter.unwrap_or(0.0);
    if barycenter_v < barycenter_w {
        return Ordering::Less;
    } else if barycenter_v > barycenter_w {
        return Ordering::Greater;
    }

    if !bias {
        entry_v.i.cmp(&entry_w.i)
    } else {
        entry_w.i.cmp(&entry_v.i)
    }
}
