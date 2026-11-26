use ordered_hashmap::OrderedHashMap;
use std::collections::HashSet;
use std::error::Error;
use std::fmt::Debug;
use std::hash::Hash;

pub const DEFAULT_EDGE_NAME: &str = "\x00";
pub const GRAPH_NODE: &str = "\x00";
pub const EDGE_KEY_DELIM: &str = "\x01";

#[derive(Debug, Clone)]
pub struct Edge {
    pub v: String,
    pub w: String,
    pub name: Option<String>,
}

#[derive(Default)]
pub struct GraphOption {
    pub directed: Option<bool>,
    pub multigraph: Option<bool>,
    pub compound: Option<bool>,
}

pub enum DefaultNodeLabel<N> {
    Val(Option<N>),
    Func(Box<dyn Fn(String) -> Option<N>>),
}

pub enum DefaultEdgeLabel<E> {
    Val(Option<E>),
    Func(Box<dyn Fn(String) -> Option<E>>),
}

pub enum EdgeOrString<E> {
    Edge(E),
    String(String),
}

pub enum EdgeLabelOrString<E> {
    EdgeLabel(E),
    String(String),
}

pub struct Graph<GL, N, E>
where
    GL: Default,
{
    // GraphLabel Type, Node Type, Node Index Type, Edge Type, Edge Index Type
    _is_directed: bool,
    _is_multigraph: bool,
    _is_compound: bool,

    // Label for the graph itself
    _label: GL,

    // Defaults to be set when creating a new node
    _default_node_label_fn: DefaultNodeLabel<N>,

    // Defaults to be set when creating a new edge
    _default_edge_label_fn: DefaultEdgeLabel<E>,

    // v -> label
    _nodes: OrderedHashMap<String, N>,

    // v -> e -> edgeObj
    _in: OrderedHashMap<String, OrderedHashMap<String, Edge>>,

    // u -> v -> Number
    _preds: OrderedHashMap<String, OrderedHashMap<String, usize>>,

    // v -> e -> edgeObj
    _out: OrderedHashMap<String, OrderedHashMap<String, Edge>>,

    // v -> w -> Number
    _sucs: OrderedHashMap<String, OrderedHashMap<String, usize>>,

    // e -> edgeObj
    _edge_objs: OrderedHashMap<String, Edge>,

    // e -> label
    _edge_labels: OrderedHashMap<String, E>,

    /* Number of nodes in the graph. Should only be changed by the implementation. */
    _node_count: usize,

    /* Number of edges in the graph. Should only be changed by the implementation. */
    _edge_count: usize,

    // v -> w
    _parent: OrderedHashMap<String, String>,

    // v -> w -> boolean
    _children: OrderedHashMap<String, OrderedHashMap<String, bool>>,
}

impl<GL: Default, N, E> Default for Graph<GL, N, E> {
    fn default() -> Self {
        Self {
            _is_directed: true,
            _is_multigraph: false,
            _is_compound: false,
            _label: GL::default(),
            _default_node_label_fn: DefaultNodeLabel::Val(None),
            _default_edge_label_fn: DefaultEdgeLabel::Val(None),
            _nodes: OrderedHashMap::new(),
            _in: OrderedHashMap::new(),
            _preds: OrderedHashMap::new(),
            _out: OrderedHashMap::new(),
            _sucs: OrderedHashMap::new(),
            _edge_objs: OrderedHashMap::new(),
            _edge_labels: OrderedHashMap::new(),
            _node_count: 0,
            _edge_count: 0,
            _parent: OrderedHashMap::new(),
            _children: OrderedHashMap::new(),
        }
    }
}

impl<GL: Default, N: Default + Clone + Debug, E: Default + Clone + Debug> Graph<GL, N, E> {
    pub fn new(opts: Option<GraphOption>) -> Self {
        let mut graph = Self::default();

        if let Some(_opts) = opts {
            if _opts.directed.is_some() {
                graph._is_directed = _opts.directed.unwrap();
            } else {
                graph._is_directed = true;
            }

            if _opts.multigraph.is_some() {
                graph._is_multigraph = _opts.multigraph.unwrap();
            } else {
                graph._is_multigraph = false;
            }

            if _opts.multigraph.is_some() {
                graph._is_multigraph = _opts.multigraph.unwrap();
            } else {
                graph._is_multigraph = false;
            }

            if _opts.compound.is_some() {
                graph._is_compound = _opts.compound.unwrap();
            } else {
                graph._is_compound = false;
            }
        }

        if graph._is_compound {
            // v -> parent
            graph._parent = OrderedHashMap::new();

            graph._children = OrderedHashMap::new();
            graph
                ._children
                .insert(GRAPH_NODE.clone().to_string(), OrderedHashMap::new());
        }

        graph
    }

    /* === Graph functions ========= */

    /**
     * Whether graph was created with 'directed' flag set to true or not.
     */
    pub fn is_directed(&self) -> bool {
        return self._is_directed;
    }

    /**
     * Whether graph was created with 'multigraph' flag set to true or not.
     */
    pub fn is_multigraph(&self) -> bool {
        return self._is_multigraph;
    }

    /**
     * Whether graph was created with 'compound' flag set to true or not.
     */
    pub fn is_compound(&self) -> bool {
        return self._is_compound;
    }

    /**
     * Sets the label of the graph.
     */
    pub fn set_graph(&mut self, label: GL) -> &mut Self {
        self._label = label;
        return self;
    }

    /**
     * Gets the graph label.
     */
    pub fn graph(&self) -> &GL {
        return &self._label;
    }

    /**
     * Gets the graph label.
     */
    pub fn graph_mut(&mut self) -> &mut GL {
        return &mut self._label;
    }

    /* === Node functions ========== */

    /**
     * Sets the default node label. If newDefault is a function, it will be
     * invoked ach time when setting a label for a node. Otherwise, this label
     * will be assigned as default label in case if no label was specified while
     * setting a node.
     * Complexity: O(1).
     */
    pub fn set_default_node_label(&mut self, new_default: DefaultNodeLabel<N>) -> &mut Self {
        self._default_node_label_fn = new_default;
        return self;
    }

    pub fn default_node_label(&self, node_id: String) -> N {
        let mut _node_label: Option<N> = None;
        match &self._default_node_label_fn {
            DefaultNodeLabel::Func(node_label_fn) => {
                _node_label = node_label_fn(node_id.clone());
            }
            DefaultNodeLabel::Val(node_label_) => {
                if node_label_.is_some() {
                    _node_label = Some(node_label_.clone().unwrap());
                } else {
                    _node_label = Some(N::default());
                }
            }
        }
        return _node_label.unwrap();
    }

    /**
     * Gets the number of nodes in the graph.
     * Complexity: O(1).
     */
    pub fn node_count(&self) -> usize {
        return self._node_count;
    }

    /**
     * Gets all nodes of the graph. Note, the in case of compound graph subnodes are
     * not included in list.
     * Complexity: O(1).
     */
    pub fn nodes(&self) -> Vec<String> {
        return self._nodes.keys().cloned().collect();
    }

    /**
     * Gets list of nodes without in-edges.
     * Complexity: O(|V|).
     */
    pub fn sources(&self) -> Vec<String> {
        return self
            .nodes()
            .iter()
            .filter(|n| {
                if let Some(in_edges) = self._in.get(&n.to_owned().clone()) {
                    return in_edges.len() == 0;
                }
                return true;
            })
            .map(|node_id| node_id.clone())
            .collect();
    }

    /**
     * Gets list of nodes without out-edges.
     * Complexity: O(|V|).
     */
    pub fn sinks(&self) -> Vec<String> {
        return self
            .nodes()
            .iter()
            .filter(|n| {
                if let Some(out_edges) = self._out.get(&n.to_owned().clone()) {
                    return out_edges.len() == 0;
                }
                return true;
            })
            .map(|node_id| node_id.clone())
            .collect();
    }

    /**
     * Invokes setNode method for each node in names list.
     * Complexity: O(|names|).
     */
    pub fn set_nodes(&mut self, node_ids: Vec<String>, value: Option<N>) -> &mut Self {
        node_ids.iter().for_each(|node_id| {
            self.set_node(node_id.to_owned(), value.clone());
        });
        return self;
    }

    /**
     * Creates or updates the value for the node v in the graph. If label is supplied
     * it is set as the value for the node. If label is not supplied and the node was
     * created by this call then the default node label will be assigned.
     * Complexity: O(1).
     */
    pub fn set_node(&mut self, v: String, value: Option<N>) -> &mut Self {
        if self._nodes.get(&v).is_some() {
            if value.is_some() {
                self._nodes.insert(v, value.unwrap());
            }
            return self;
        }

        if value.is_some() {
            self._nodes.insert(v.clone(), value.unwrap());
        } else {
            self._nodes
                .insert(v.clone(), self.default_node_label(v.clone()));
        }

        if self._is_compound {
            let _graph_node = GRAPH_NODE.clone().to_string();
            self._parent.insert(v.clone(), _graph_node.clone());
            self._children.insert(v.clone(), OrderedHashMap::new());
            self._children
                .entry(_graph_node.clone())
                .or_insert(OrderedHashMap::new())
                .entry(v.clone())
                .or_insert(true);
        }

        self._in.insert(v.clone(), OrderedHashMap::new());
        self._preds.insert(v.clone(), OrderedHashMap::new());
        self._out.insert(v.clone(), OrderedHashMap::new());
        self._sucs.insert(v.clone(), OrderedHashMap::new());
        self._node_count += 1;

        return self;
    }

    /**
     * Gets the label of node with specified name.
     * Complexity: O(|V|).
     */
    pub fn node(&self, v: &String) -> Option<&N> {
        return self._nodes.get(v);
    }

    /**
     * Gets the label of node with specified name.
     * Complexity: O(|V|).
     */
    pub fn node_mut(&mut self, v: &String) -> Option<&mut N> {
        return self._nodes.get_mut(v);
    }

    /**
     * Detects whether graph has a node with specified name or not.
     */
    pub fn has_node(&self, v: &String) -> bool {
        return self._nodes.contains_key(v);
    }

    /**
     * Remove the node with the name from the graph or do nothing if the node is not in
     * the graph. If the node was removed this function also removes any incident
     * edges.
     * Complexity: O(1).
     */
    pub fn remove_node(&mut self, v: &String) -> &mut Self {
        if self._nodes.contains_key(v) {
            self._nodes.remove(v);

            if self._is_compound {
                self._remove_from_parents_child_list(v);
                if self._parent.contains_key(v) {
                    self._parent.remove(v);
                }
                self.children(v).iter().for_each(|child_id| {
                    // TODO: exception handling
                    let _ = self.set_parent(child_id, None);
                });
                self._children.remove(v);
            }
            // removing in edges
            if let Some(in_edges) = self._in.get(v) {
                let edge_ids: Vec<String> = in_edges.keys().cloned().collect();
                edge_ids.iter().for_each(|edge_id| {
                    if let Some(edge) = self._edge_objs.get(edge_id) {
                        self.remove_edge_with_obj(&edge.to_owned());
                    }
                });
                self._in.remove(v);
            }

            self._preds.remove(v);

            // removing out edges
            if let Some(out_edges) = self._out.get(v) {
                let edge_ids: Vec<String> = out_edges.keys().cloned().collect();
                edge_ids.iter().for_each(|edge_id| {
                    if let Some(edge) = self._edge_objs.get(edge_id) {
                        self.remove_edge_with_obj(&edge.to_owned());
                    }
                });
                self._out.remove(v);
            }
            self._sucs.remove(v);
            self._node_count -= 1;
        }
        return self;
    }

    /**
     * Sets node p as a parent for node v if it is defined, or removes the
     * parent for v if p is undefined. Method throws an exception in case of
     * invoking it in context of noncompound graph.
     * Average-case complexity: O(1).
     */
    pub fn set_parent(
        &mut self,
        v: &String,
        parent: Option<String>,
    ) -> Result<&mut Self, Box<dyn Error>> {
        if !self._is_compound {
            return Err("Cannot set parent in a non-compound graph".into());
        }

        let mut _parent: String = "".to_string();

        if parent.is_none() {
            _parent = GRAPH_NODE.to_string();
        } else {
            _parent = parent.unwrap().clone();
            let mut ancestor = _parent.clone();
            while let Some(new_ancestor) = self.parent(&ancestor) {
                if &new_ancestor == &v {
                    return Err(format!(
                        "Setting {} as parent of {} would create a cycle",
                        _parent.clone(),
                        v
                    )
                    .into());
                }
                ancestor = new_ancestor.clone();
            }

            self.set_node(_parent.clone(), None);
        }

        self.set_node(v.clone(), None);
        self._remove_from_parents_child_list(v);
        self._parent.insert(v.clone(), _parent.clone());
        self._children
            .entry(_parent.clone())
            .or_insert_with(OrderedHashMap::new)
            .insert(v.clone(), true);
        Ok(self)
    }

    pub fn _remove_from_parents_child_list(&mut self, v: &String) {
        if let Some(parent) = self._parent.get(v) {
            if let Some(children) = self._children.get_mut(parent) {
                children.remove(v);
            }
        }
    }

    /**
     * Gets parent node for node v.
     * Complexity: O(1).
     */
    pub fn parent(&self, v: &String) -> Option<&String> {
        if self._is_compound {
            if let Some(parent) = self._parent.get(v) {
                if parent != GRAPH_NODE {
                    return Some(parent);
                }
            }
        }

        None
    }

    /**
     * Gets list of direct children of node v.
     * Complexity: O(1).
     */
    pub fn children(&self, v: &String) -> Vec<String> {
        if self._is_compound {
            if let Some(children) = self._children.get(v) {
                return children.keys().cloned().collect();
            }
        } else if v == GRAPH_NODE {
            return self._nodes.keys().cloned().collect();
        } else if self.has_node(&v) {
            return vec![];
        }
        vec![]
    }

    /**
     * Return all nodes that are predecessors of the specified node or undefined if node v is not in
     * the graph. Behavior is undefined for undirected graphs - use neighbors instead.
     * Complexity: O(|V|).
     */
    pub fn predecessors(&self, v: &String) -> Option<Vec<String>> {
        if let Some(preds) = self._preds.get(v) {
            return Some(preds.keys().cloned().collect());
        }

        None
    }

    /**
     * Return all nodes that are successors of the specified node or undefined if node v is not in
     * the graph. Behavior is undefined for undirected graphs - use neighbors instead.
     * Complexity: O(|V|).
     */
    pub fn successors(&self, v: &String) -> Option<Vec<String>> {
        if let Some(sucs) = self._sucs.get(v) {
            return Some(sucs.keys().cloned().collect());
        }

        None
    }

    /**
     * Return all nodes that are predecessors or successors of the specified node or undefined if
     * node v is not in the graph.
     * Complexity: O(|V|).
     */
    pub fn neighbors(&self, v: &String) -> Option<Vec<String>> {
        if let Some(preds) = self.predecessors(v) {
            let mut union: HashSet<String> = HashSet::new();
            preds.into_iter().for_each(|pred| {
                union.insert(pred);
            });
            if let Some(sucs) = self.successors(v) {
                sucs.into_iter().for_each(|successor| {
                    union.insert(successor);
                });
            }

            return Some(union.into_iter().collect());
        }

        None
    }

    pub fn is_leaf(&self, v: &String) -> bool {
        let mut _neighbors: Option<Vec<String>> = None;
        if self.is_directed() {
            _neighbors = self.successors(v);
        } else {
            _neighbors = self.neighbors(v);
        }

        if _neighbors.is_none() || _neighbors.unwrap().len() == 0 {
            return true;
        }

        false
    }

    /**
     * Creates new graph with nodes filtered via filter. Edges incident to rejected node
     * are also removed. In case of compound graph, if parent is rejected by filter,
     * than all its children are rejected too.
     * Average-case complexity: O(|E|+|V|).
     */
    pub fn filter_nodes<F>(&self, filter: F) -> Self
    where
        F: Fn(&String) -> bool,
    {
        let mut copy: Graph<GL, N, E> = Graph::new(Some(GraphOption {
            directed: Some(self._is_directed.clone()),
            multigraph: Some(self._is_multigraph.clone()),
            compound: Some(self._is_compound.clone()),
        }));

        for (v, value) in self._nodes.iter() {
            if filter(v) {
                copy.set_node(v.clone(), Some(value.clone()));
            }
        }

        for e_v in self._edge_objs.values() {
            if copy._nodes.contains_key(&e_v.v) && copy._nodes.contains_key(&e_v.w) {
                if let Some(edge_label) = self.edge_with_obj(e_v) {
                    let _ = copy.set_edge_with_obj(e_v, Some(edge_label.to_owned()));
                }
            }
        }

        let mut parents: OrderedHashMap<String, String> = OrderedHashMap::new();

        if self._is_compound {
            let node_ids: Vec<String> = copy._nodes.keys().cloned().into_iter().collect();
            for v in node_ids {
                let parent = find_parent(&v, &mut parents, &mut copy, self);
                let _ = copy.set_parent(&v, parent);
            }
        }

        copy
    }

    /* === Edge functions ========== */

    /**
     * Sets the default edge label or factory function. This label will be
     * assigned as default label in case if no label was specified while setting
     * an edge or this function will be invoked each time when setting an edge
     * with no label specified and returned value * will be used as a label for edge.
     * Complexity: O(1).
     */
    pub fn set_default_edge_label(&mut self, new_default: DefaultEdgeLabel<E>) -> &mut Self {
        self._default_edge_label_fn = new_default;
        return self;
    }

    pub fn default_edge_label(&self, edge_id: String) -> E {
        let mut _edge_label: Option<E> = None;
        match &self._default_edge_label_fn {
            DefaultEdgeLabel::Func(edge_label_fn) => {
                _edge_label = edge_label_fn(edge_id.clone());
            }
            DefaultEdgeLabel::Val(edge_label_) => {
                if edge_label_.is_some() {
                    _edge_label = Some(edge_label_.clone().unwrap());
                } else {
                    _edge_label = Some(E::default());
                }
            }
        }
        return _edge_label.unwrap();
    }

    /**
     * Gets the number of edges in the graph.
     * Complexity: O(1).
     */
    pub fn edge_count(&self) -> usize {
        return self._edge_count.clone();
    }

    /**
     * Gets edges of the graph. In case of compound graph subgraphs are not considered.
     * Complexity: O(|E|).
     */
    pub fn edges(&self) -> Vec<Edge> {
        return self._edge_objs.values().cloned().collect();
    }

    /**
     * Establish an edges path over the nodes in nodes list. If some edge is already
     * exists, it will update its label, otherwise it will create an edge between pair
     * of nodes with label provided or default label if no label provided.
     * Complexity: O(|nodes|).
     */
    pub fn set_path(&mut self, vs: &Vec<String>, value: Option<E>) {
        vs.iter().reduce(|v1, v2| {
            let _ = self.set_edge(v1, v2, value.clone(), None);
            v2
        });
    }

    /**
     * Creates or updates the label for the edge (v, w) with the optionally supplied
     * name. If label is supplied it is set as the value for the edge. If label is not
     * supplied and the edge was created by this call then the default edge label will
     * be assigned. The name parameter is only useful with multigraphs.
     */
    pub fn set_edge(
        &mut self,
        v: &String,
        w: &String,
        edge_label: Option<E>,
        name: Option<String>,
    ) -> Result<&mut Self, Box<dyn Error>> {
        let e = edge_args_to_id(&self._is_directed, v, w, &name);
        if self._edge_labels.contains_key(&e) {
            if edge_label.is_some() {
                self._edge_labels.insert(e.clone(), edge_label.unwrap());
            }
            return Ok(self);
        }

        if name.is_some() && !self._is_multigraph {
            return Err("Cannot set a named edge when isMultigraph = false".into());
        }

        // It didn't exist, so we need to create it.
        // First ensure the nodes exist.
        self.set_node(v.clone(), None);
        self.set_node(w.clone(), None);

        if edge_label.is_some() {
            self._edge_labels
                .insert(e.clone(), edge_label.clone().unwrap());
        } else {
            self._edge_labels
                .insert(e.clone(), self.default_edge_label(e.clone()));
        }

        let edge_obj = edge_args_to_obj(&self.is_directed(), v, w, &name);
        // Ensure we add undirected edges in a consistent way.
        self._edge_objs.insert(e.clone(), edge_obj.clone());
        if let Some(preds) = self._preds.get_mut(w) {
            increment_or_init_entry(preds, v);
        }
        if let Some(sucs) = self._sucs.get_mut(v) {
            increment_or_init_entry(sucs, w);
        }

        let in_edges = self
            ._in
            .entry(w.clone())
            .or_insert_with(OrderedHashMap::new);
        in_edges.insert(e.clone(), edge_obj.clone());

        let out_edges = self
            ._out
            .entry(v.clone())
            .or_insert_with(OrderedHashMap::new);
        out_edges.insert(e.clone(), edge_obj.clone());

        self._edge_count += 1;
        return Ok(self);
    }

    pub fn set_edge_with_obj(
        &mut self,
        e: &Edge,
        edge_label: Option<E>,
    ) -> Result<&mut Self, Box<dyn Error>> {
        self.set_edge(&e.v, &e.w, edge_label, None)
    }

    /**
     * Gets the label for the specified edge.
     * Complexity: O(1).
     */
    pub fn edge(&self, v: &String, w: &String, name: Option<String>) -> Option<&E> {
        let e = edge_args_to_id(&self._is_directed, v, w, &name);
        return self._edge_labels.get(&e);
    }

    /**
     * Gets the label for the specified edge.
     * Complexity: O(1).
     */
    pub fn edge_with_obj(&self, edge: &Edge) -> Option<&E> {
        let e = edge_obj_to_id(&self._is_directed, edge);
        return self._edge_labels.get(&e);
    }

    /**
     * Gets the label for the specified edge.
     * Complexity: O(1).
     */
    pub fn edge_mut(&mut self, v: &String, w: &String, name: Option<String>) -> Option<&mut E> {
        let e = edge_args_to_id(&self._is_directed, v, w, &name);
        return self._edge_labels.get_mut(&e);
    }

    /**
     * Gets the label for the specified edge.
     * Complexity: O(1).
     */
    pub fn edge_mut_with_obj(&mut self, edge: &Edge) -> Option<&mut E> {
        let e = edge_obj_to_id(&self._is_directed, edge);
        return self._edge_labels.get_mut(&e);
    }

    /**
     * Detects whether the graph contains specified edge or not. No subgraphs are considered.
     * Complexity: O(1).
     */
    pub fn has_edge(&self, v: &String, w: &String, name: Option<String>) -> bool {
        let e = edge_args_to_id(&self._is_directed, v, w, &name);
        self._edge_labels.contains_key(&e)
    }

    pub fn has_edge_with_obj(&self, edge: &Edge) -> bool {
        let e = edge_obj_to_id(&self._is_directed, edge);
        self._edge_labels.contains_key(&e)
    }

    /**
     * Removes the specified edge from the graph. No subgraphs are considered.
     * Complexity: O(1).
     */
    pub fn remove_edge(&mut self, v: &String, w: &String, name: Option<String>) -> &mut Self {
        let e: String = edge_args_to_id(&self._is_directed, v, w, &name);
        if let Some(edge) = self._edge_objs.get_mut(&e) {
            let v = edge.v.clone();
            let w = edge.w.clone();
            self._edge_labels.remove(&e);
            self._edge_objs.remove(&e);
            if self._preds.contains_key(&w) {
                decrement_or_remove_entry(self._preds.get_mut(&w).unwrap(), &v);
            }
            if self._sucs.contains_key(&v) {
                decrement_or_remove_entry(self._sucs.get_mut(&v).unwrap(), &w);
            }

            if self._in.contains_key(&w) {
                self._in.get_mut(&w).unwrap().remove(&e);
            }

            if self._out.contains_key(&v) {
                self._out.get_mut(&v).unwrap().remove(&e);
            }
            self._edge_count -= 1;
        }

        return self;
    }

    /**
     * Removes the specified edge from the graph. No subgraphs are considered.
     * Complexity: O(1).
     */
    pub fn remove_edge_with_obj(&mut self, e: &Edge) -> &mut Self {
        self.remove_edge(&e.v, &e.w, None);
        return self;
    }

    /**
     * Return all edges that point to the node v. Optionally filters those edges down to just those
     * coming from node u. Behavior is undefined for undirected graphs - use nodeEdges instead.
     * Complexity: O(|E|).
     */
    pub fn in_edges(&self, v: &String, u: Option<String>) -> Option<Vec<Edge>> {
        if let Some(in_edges) = self._in.get(v) {
            let mut _in_edges: Vec<Edge> = in_edges.values().cloned().collect();
            if u.is_none() {
                return Some(_in_edges.clone());
            }
            let _u = u.unwrap();
            return Some(_in_edges.into_iter().filter(|edge| edge.v == _u).collect());
        }

        None
    }

    /**
     * Return all edges that are pointed at by node v. Optionally filters those edges down to just
     * those point to w. Behavior is undefined for undirected graphs - use nodeEdges instead.
     * Complexity: O(|E|).
     */
    pub fn out_edges(&self, v: &String, w: Option<String>) -> Option<Vec<Edge>> {
        if let Some(out_edges) = self._out.get(v) {
            let mut _out_edges: Vec<Edge> = out_edges.values().cloned().collect();
            if w.is_none() {
                return Some(_out_edges.clone());
            }

            let _w = w.unwrap();
            return Some(_out_edges.into_iter().filter(|edge| edge.w == _w).collect());
        }

        None
    }

    /**
     * Returns all edges to or from node v regardless of direction. Optionally filters those edges
     * down to just those between nodes v and w regardless of direction.
     * Complexity: O(|E|).
     */
    pub fn node_edges(&self, v: &String, w: Option<String>) -> Option<Vec<Edge>> {
        let _in_edges = self.in_edges(v, w.clone());
        if let Some(mut in_edges) = _in_edges {
            let _out_edges = self.out_edges(v, w.clone());
            if let Some(out_edges) = _out_edges {
                in_edges.append(out_edges.clone().as_mut());
            }
            return Some(in_edges);
        }

        None
    }
}

fn increment_or_init_entry<K: Hash + Eq + Clone>(map: &mut OrderedHashMap<K, usize>, k: &K) {
    if let Some(e) = map.get_mut(&k) {
        *e += 1;
    } else {
        map.insert(k.clone(), 1);
    }
}

fn decrement_or_remove_entry<K: Hash + Eq + Clone>(map: &mut OrderedHashMap<K, usize>, k: &K) {
    if let Some(value) = map.get_mut(k) {
        *value -= 1;
        if *value <= 0 {
            map.remove(k);
        }
    }
}

fn edge_args_to_id(is_directed: &bool, v_: &String, w_: &String, name: &Option<String>) -> String {
    let mut v: &str = &*v_;
    let mut w: &str = &*w_;
    if !is_directed.to_owned() && v > w {
        let tmp = v;
        v = w;
        w = tmp;
    }

    if name.is_some() {
        return v.to_owned() + EDGE_KEY_DELIM + w + EDGE_KEY_DELIM + &*name.clone().unwrap();
    }
    return v.to_owned() + EDGE_KEY_DELIM + w + EDGE_KEY_DELIM + DEFAULT_EDGE_NAME;
}

fn edge_args_to_obj(is_directed: &bool, v_: &String, w_: &String, name: &Option<String>) -> Edge {
    let mut v: &str = &*v_;
    let mut w: &str = &*w_;
    if !is_directed.to_owned() && v > w {
        let tmp = v;
        v = w;
        w = tmp;
    }

    return Edge {
        v: v.to_string(),
        w: w.to_string(),
        name: name.clone(),
    };
}

fn edge_obj_to_id(is_directed: &bool, edge: &Edge) -> String {
    return edge_args_to_id(is_directed, &edge.v, &edge.w, &edge.name);
}

fn find_parent<GL: Default, N: Default + Clone + Debug, E: Default + Clone + Debug>(
    v: &String,
    parents: &mut OrderedHashMap<String, String>,
    copy: &mut Graph<GL, N, E>,
    graph: &Graph<GL, N, E>,
) -> Option<String> {
    let parent = graph.parent(v);
    if parent.is_none() || copy._nodes.contains_key(&parent.unwrap().clone()) {
        if !parent.is_none() {
            parents.insert(v.clone(), parent.unwrap().clone());
            return parent.cloned();
        }
        None
    } else if let Some(parent_value) = parents.get(&parent.unwrap().clone()) {
        Some(parent_value.clone())
    } else {
        if parent.is_some() {
            find_parent(parent.as_ref().unwrap(), parents, copy, graph)
        } else {
            None
        }
    }
}
