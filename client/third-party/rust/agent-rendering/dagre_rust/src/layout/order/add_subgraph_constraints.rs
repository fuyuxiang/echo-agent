use crate::{GraphConfig, GraphEdge, GraphNode};
use graphlib_rust::Graph;
use ordered_hashmap::OrderedHashMap;

pub fn add_subgraph_constraints(
    g: &Graph<GraphConfig, GraphNode, GraphEdge>,
    cg: &mut Graph<GraphConfig, GraphNode, GraphEdge>,
    vs: &Vec<String>,
) {
    let mut prev: OrderedHashMap<String, String> = OrderedHashMap::new();
    let mut _root_prev: Option<String> = None;

    vs.iter().for_each(|v| {
        let mut child = g.parent(v).cloned();
        let mut _parent: Option<String> = None;
        let mut _prev_child: Option<String> = None;
        while child.is_some() {
            _parent = g.parent(&child.clone().unwrap()).cloned();
            if _parent.is_some() {
                _prev_child = prev
                    .get(&_parent.clone().unwrap_or("".to_string()))
                    .cloned();
                prev.insert(_parent.clone().unwrap(), child.clone().unwrap());
            } else {
                _prev_child = _root_prev.clone();
                _root_prev = child.clone();
            }

            let prev_child = _prev_child.clone().unwrap_or("".to_string());
            let child_ = child.clone().unwrap_or("".to_string());
            if _prev_child.is_some() && prev_child != child_ {
                let _ = cg.set_edge(&prev_child, &child_, None, None);
                return ();
            }
            child = _parent.clone();
        }
    });
}
