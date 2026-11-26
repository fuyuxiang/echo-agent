use std::collections::{HashMap, HashSet};

use dagre_rust::{GraphConfig, GraphEdge, GraphNode};
use graphlib_rust::{Edge, Graph};

pub type DagreGraph = Graph<GraphConfig, GraphNode, GraphEdge>;

pub struct ExtractedCluster {
    pub graph: DagreGraph,
    pub children: HashMap<String, ExtractedCluster>,
}

#[derive(Debug, Clone)]
struct ClusterDbEntry {
    anchor_id: String,
    external_connections: bool,
}

#[derive(Debug, Default)]
struct AdjustState {
    cluster_db: HashMap<String, ClusterDbEntry>,
    descendants: HashMap<String, HashSet<String>>,
}

pub fn adjust_clusters_and_edges(graph: &mut DagreGraph) -> HashMap<String, ExtractedCluster> {
    let mut state = AdjustState::default();
    let mut parents: HashMap<String, String> = HashMap::new();

    let nodes = graph.nodes();
    for id in &nodes {
        if graph.children(id).is_empty() {
            continue;
        }

        let descendants = extract_descendants(id, graph, &mut parents);
        state.descendants.insert(id.clone(), descendants);

        let anchor_id = find_non_cluster_child(id, graph, id).unwrap_or_else(|| id.clone());
        state.cluster_db.insert(
            id.clone(),
            ClusterDbEntry {
                anchor_id,
                external_connections: false,
            },
        );
    }

    let edges = graph.edges();
    for id in &nodes {
        if graph.children(id).is_empty() {
            continue;
        }

        for edge in &edges {
            let d1 = is_descendant(&edge.v, id, &state);
            let d2 = is_descendant(&edge.w, id, &state);
            if d1 != d2 {
                if let Some(entry) = state.cluster_db.get_mut(id) {
                    entry.external_connections = true;
                }
                break;
            }
        }
    }

    let cluster_ids: Vec<String> = state.cluster_db.keys().cloned().collect();
    for id in cluster_ids {
        let Some(non_cluster_child) = state
            .cluster_db
            .get(&id)
            .map(|entry| entry.anchor_id.clone())
        else {
            continue;
        };

        let Some(parent) = graph.parent(&non_cluster_child) else {
            continue;
        };

        if parent == &id {
            continue;
        }

        let Some(parent_entry) = state.cluster_db.get(parent) else {
            continue;
        };

        if !parent_entry.external_connections {
            if let Some(entry) = state.cluster_db.get_mut(&id) {
                entry.anchor_id = parent.clone();
            }
        }
    }

    let edge_objs = graph.edges();
    for edge_obj in edge_objs {
        if !state.cluster_db.contains_key(&edge_obj.v)
            && !state.cluster_db.contains_key(&edge_obj.w)
        {
            continue;
        }

        let Some(edge_label) = graph.edge_with_obj(&edge_obj).cloned() else {
            continue;
        };

        let v = get_anchor_id(&edge_obj.v, &state);
        let w = get_anchor_id(&edge_obj.w, &state);

        graph.remove_edge_with_obj(&edge_obj);

        if v != edge_obj.v {
            if let Some(parent) = graph.parent(&v) {
                if let Some(entry) = state.cluster_db.get_mut(parent) {
                    entry.external_connections = true;
                }
            }
        }

        if w != edge_obj.w {
            if let Some(parent) = graph.parent(&w) {
                if let Some(entry) = state.cluster_db.get_mut(parent) {
                    entry.external_connections = true;
                }
            }
        }

        let _ = graph.set_edge(&v, &w, Some(edge_label), edge_obj.name.clone());
    }

    extractor(graph, &state, 0)
}

fn extract_descendants(
    id: &String,
    graph: &DagreGraph,
    parents: &mut HashMap<String, String>,
) -> HashSet<String> {
    let children = graph.children(id);
    let mut res: HashSet<String> = children.iter().cloned().collect();
    for child in children {
        parents.insert(child.clone(), id.clone());
        res.extend(extract_descendants(&child, graph, parents));
    }
    res
}

fn is_descendant(id: &String, ancestor_id: &String, state: &AdjustState) -> bool {
    state
        .descendants
        .get(ancestor_id)
        .is_some_and(|desc| desc.contains(id))
}

fn edge_in_cluster(edge: &Edge, cluster_id: &String, state: &AdjustState) -> bool {
    if &edge.v == cluster_id || &edge.w == cluster_id {
        return false;
    }

    let Some(cluster_descendants) = state.descendants.get(cluster_id) else {
        return false;
    };

    cluster_descendants.contains(&edge.v) || cluster_descendants.contains(&edge.w)
}

fn find_common_edges(graph: &DagreGraph, id1: &String, id2: &String) -> Vec<(String, String)> {
    let edges = graph.edges();

    let edges1: Vec<&Edge> = edges
        .iter()
        .filter(|edge| &edge.v == id1 || &edge.w == id1)
        .collect();
    let edges2: Vec<&Edge> = edges
        .iter()
        .filter(|edge| &edge.v == id2 || &edge.w == id2)
        .collect();

    let edges1_prim: Vec<(String, String)> = edges1
        .into_iter()
        .map(|edge| {
            let v = if &edge.v == id1 {
                id2.clone()
            } else {
                edge.v.clone()
            };
            let w = if &edge.w == id1 {
                id1.clone()
            } else {
                edge.w.clone()
            };
            (v, w)
        })
        .collect();

    let edges2_prim: Vec<(String, String)> = edges2
        .into_iter()
        .map(|edge| (edge.v.clone(), edge.w.clone()))
        .collect();

    edges1_prim
        .into_iter()
        .filter(|(v, w)| edges2_prim.iter().any(|(v2, w2)| v == v2 && w == w2))
        .collect()
}

fn find_non_cluster_child(id: &String, graph: &DagreGraph, cluster_id: &String) -> Option<String> {
    let children = graph.children(id);
    if children.is_empty() {
        return Some(id.clone());
    }

    let mut reserve: Option<String> = None;
    for child in children {
        let Some(candidate) = find_non_cluster_child(&child, graph, cluster_id) else {
            continue;
        };
        let candidate_id = candidate.clone();
        let common_edges = find_common_edges(graph, cluster_id, &candidate_id);
        if !common_edges.is_empty() {
            reserve = Some(candidate);
        } else {
            return Some(candidate);
        }
    }

    reserve
}

fn get_anchor_id(id: &String, state: &AdjustState) -> String {
    let Some(entry) = state.cluster_db.get(id) else {
        return id.clone();
    };

    if !entry.external_connections {
        return id.clone();
    }

    entry.anchor_id.clone()
}

fn new_cluster_graph(rankdir: &str) -> DagreGraph {
    let dir = if rankdir == "tb" { "lr" } else { "tb" };

    let mut g: DagreGraph = Graph::new(Some(graphlib_rust::GraphOption {
        directed: Some(true),
        multigraph: Some(true),
        compound: Some(true),
    }));

    g.set_graph(GraphConfig {
        rankdir: Some(dir.to_string()),
        nodesep: Some(50.0),
        ranksep: Some(50.0),
        marginx: Some(8.0),
        marginy: Some(8.0),
        ..Default::default()
    });

    g
}

fn copy(
    cluster_id: &String,
    graph: &mut DagreGraph,
    new_graph: &mut DagreGraph,
    root_id: &String,
    state: &AdjustState,
) {
    let mut nodes = graph.children(cluster_id);
    if cluster_id != root_id {
        nodes.push(cluster_id.clone());
    }

    for node in nodes {
        if !graph.children(&node).is_empty() {
            copy(&node, graph, new_graph, root_id, state);
        } else {
            let Some(data) = graph.node(&node).cloned() else {
                continue;
            };

            new_graph.set_node(node.clone(), Some(data));

            if let Some(parent) = graph.parent(&node) {
                if root_id != parent {
                    let _ = new_graph.set_parent(&node, Some(parent.clone()));
                }
            }

            if cluster_id != root_id && node != *cluster_id {
                let _ = new_graph.set_parent(&node, Some(cluster_id.clone()));
            }

            let edge_objs: Vec<Edge> = graph
                .edges()
                .into_iter()
                .filter(|e| e.v == node || e.w == node)
                .collect();

            for edge_obj in edge_objs {
                let Some(edge_label) = graph.edge_with_obj(&edge_obj).cloned() else {
                    continue;
                };

                if edge_in_cluster(&edge_obj, root_id, state) {
                    let _ = new_graph.set_edge(
                        &edge_obj.v,
                        &edge_obj.w,
                        Some(edge_label),
                        edge_obj.name.clone(),
                    );
                }
            }
        }

        graph.remove_node(&node);
    }
}

fn extractor(
    graph: &mut DagreGraph,
    state: &AdjustState,
    depth: usize,
) -> HashMap<String, ExtractedCluster> {
    if depth > 10 {
        return HashMap::new();
    }

    let nodes = graph.nodes();
    if !nodes.iter().any(|node| !graph.children(node).is_empty()) {
        return HashMap::new();
    }

    let mut extracted: HashMap<String, ExtractedCluster> = HashMap::new();

    let rankdir = graph
        .graph()
        .rankdir
        .clone()
        .unwrap_or_else(|| "tb".to_string());

    for node in nodes {
        if graph.node(&node).is_none() {
            continue;
        }

        if graph.children(&node).is_empty() {
            continue;
        }

        let Some(entry) = state.cluster_db.get(&node) else {
            continue;
        };

        if entry.external_connections {
            continue;
        }

        let mut cluster_graph = new_cluster_graph(&rankdir);
        copy(&node, graph, &mut cluster_graph, &node, state);
        let children = extractor(&mut cluster_graph, state, depth + 1);
        extracted.insert(
            node.clone(),
            ExtractedCluster {
                graph: cluster_graph,
                children,
            },
        );
    }

    extracted
}
