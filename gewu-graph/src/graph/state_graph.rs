use crate::graph::constants::{END, START};
use crate::state::State;
use futures::Stream;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use thiserror::Error;

const MAX_SUBGRAPH_DEPTH: usize = 10;

#[derive(Error, Debug)]
pub enum GraphError {
    #[error("Node `{0}` not found")]
    NodeNotFound(String),
    #[error("No entry point set")]
    NoEntryPoint,
    #[error("Execution error: {0}")]
    ExecutionError(String),
    #[error("Subgraph depth limit ({0}) exceeded")]
    SubgraphDepthLimitExceeded(usize),
}

#[derive(Clone)]
pub struct NodeHandler<S: State> {
    name: String,
    handler: Arc<dyn Fn(S) -> Pin<Box<dyn Future<Output = S::Partial> + Send>> + Send + Sync>,
    subgraph: Option<Box<CompiledGraph<S>>>,
}

impl<S: State + std::fmt::Debug> std::fmt::Debug for NodeHandler<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeHandler")
            .field("name", &self.name)
            .field("has_subgraph", &self.subgraph.is_some())
            .finish()
    }
}

impl<S: State> NodeHandler<S> {
    pub fn new<F, Fut>(name: &str, handler: F) -> Self
    where
        F: Fn(S) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = S::Partial> + Send + 'static,
    {
        Self {
            name: name.to_string(),
            handler: Arc::new(move |state: S| Box::pin(handler(state)) as Pin<Box<dyn Future<Output = S::Partial> + Send>>),
            subgraph: None,
        }
    }

    pub fn with_subgraph(subgraph: CompiledGraph<S>) -> Self {
        Self {
            name: subgraph.entry_point.clone().unwrap_or_default(),
            handler: Arc::new(move |_| Box::pin(async { S::Partial::default() }) as Pin<Box<dyn Future<Output = S::Partial> + Send>>),
            subgraph: Some(Box::new(subgraph)),
        }
    }
}

#[derive(Clone)]
pub struct ConditionalEdge<S: State> {
    path_fn: Arc<dyn Fn(&S) -> String + Send + Sync>,
    path_map: HashMap<String, String>,
}

impl<S: State + std::fmt::Debug> std::fmt::Debug for ConditionalEdge<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConditionalEdge")
            .field("path_map", &self.path_map)
            .finish()
    }
}

#[derive(Debug)]
pub struct StateGraph<S: State> {
    nodes: HashMap<String, NodeHandler<S>>,
    edges: HashSet<(String, String)>,
    conditional_edges: HashMap<String, Vec<ConditionalEdge<S>>>,
    entry_point: Option<String>,
    finish_points: HashSet<String>,
}

impl<S: State> Default for StateGraph<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: State> StateGraph<S> {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            edges: HashSet::new(),
            conditional_edges: HashMap::new(),
            entry_point: None,
            finish_points: HashSet::new(),
        }
    }

    pub fn add_node<F, Fut>(&mut self, name: &str, handler: F) -> &mut Self
    where
        F: Fn(S) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = S::Partial> + Send + 'static,
    {
        self.nodes.insert(name.to_string(), NodeHandler::new(name, handler));
        self
    }

    pub fn add_subgraph_node(&mut self, name: &str, subgraph: CompiledGraph<S>) -> &mut Self {
        let mut handler = NodeHandler::with_subgraph(subgraph);
        handler.name = name.to_string();
        self.nodes.insert(name.to_string(), handler);
        self
    }

    pub fn add_edge(&mut self, start: &str, end: &str) -> &mut Self {
        if start == END {
            panic!("START cannot be END node");
        }
        if end == START {
            panic!("END cannot be START node");
        }
        self.edges.insert((start.to_string(), end.to_string()));
        self
    }

    pub fn add_conditional_edges(
        &mut self,
        source: &str,
        path_fn: impl Fn(&S) -> String + Send + Sync + 'static,
        path_map: HashMap<String, String>,
    ) -> &mut Self {
        let path_fn = Arc::new(path_fn);
        self.conditional_edges
            .entry(source.to_string())
            .or_insert_with(Vec::new)
            .push(ConditionalEdge { path_fn, path_map });
        self
    }

    pub fn set_entry_point(&mut self, name: &str) -> &mut Self {
        self.entry_point = Some(name.to_string());
        self
    }

    pub fn set_finish_point(&mut self, name: &str) -> &mut Self {
        self.finish_points.insert(name.to_string());
        self
    }

    pub fn compile(&self) -> CompiledGraph<S> {
        CompiledGraph {
            nodes: self.nodes.clone(),
            edges: self.edges.clone(),
            conditional_edges: self.conditional_edges.clone(),
            entry_point: self.entry_point.clone(),
            finish_points: self.finish_points.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CompiledGraph<S: State> {
    nodes: HashMap<String, NodeHandler<S>>,
    edges: HashSet<(String, String)>,
    conditional_edges: HashMap<String, Vec<ConditionalEdge<S>>>,
    entry_point: Option<String>,
    finish_points: HashSet<String>,
}

impl<S: State + 'static> CompiledGraph<S> {
    pub async fn invoke(&self, input: S) -> Result<S, GraphError> {
        self.invoke_with_depth(input, 0).await
    }

    fn invoke_with_depth(
        &self,
        input: S,
        depth: usize,
    ) -> Pin<Box<dyn Future<Output = Result<S, GraphError>> + Send>> {
        if depth > MAX_SUBGRAPH_DEPTH {
            return Box::pin(async move { Err(GraphError::SubgraphDepthLimitExceeded(MAX_SUBGRAPH_DEPTH)) });
        }

        let entry = match self.entry_point.clone() {
            Some(e) => e,
            None => return Box::pin(async move { Err(GraphError::NoEntryPoint) }),
        };

        let graph = self.clone();
        Box::pin(async move {
            let mut current_state = input;
            let mut to_execute: Vec<String> = vec![entry];

            while let Some(node_name) = to_execute.pop() {
                let handler = graph
                    .nodes
                    .get(&node_name)
                    .ok_or_else(|| GraphError::NodeNotFound(node_name.clone()))?;

                if let Some(subgraph) = &handler.subgraph {
                    current_state = subgraph.invoke_with_depth(current_state, depth + 1).await?;
                } else {
                    let partial = (handler.handler)(current_state.clone()).await;
                    current_state = current_state.merge(partial);
                }

                let outgoing = graph.get_outgoing_edges(&node_name, &current_state);
                for edge in outgoing {
                    if edge == END || edge.is_empty() {
                        continue;
                    }
                    to_execute.push(edge);
                }

                if graph.finish_points.contains(&node_name) && to_execute.is_empty() {
                    return Ok(current_state);
                }
            }

            Ok(current_state)
        })
    }

    pub fn stream(&self, input: S) -> impl Stream<Item = Result<S, GraphError>> + '_ {
        async_stream::try_stream! {
            let entry = self.entry_point.clone().ok_or(GraphError::NoEntryPoint)?;

            let mut current_state = input;
            let mut to_execute: Vec<String> = vec![entry];

            yield current_state.clone();

            while let Some(node_name) = to_execute.pop() {
                let handler = self.nodes.get(&node_name)
                    .ok_or_else(|| GraphError::NodeNotFound(node_name.clone()))?;

                if let Some(subgraph) = &handler.subgraph {
                    current_state = subgraph.invoke(current_state).await?;
                } else {
                    let partial = (handler.handler)(current_state.clone()).await;
                    current_state = current_state.merge(partial);
                }

                yield current_state.clone();

                let outgoing = self.get_outgoing_edges(&node_name, &current_state);
                for edge in outgoing {
                    if edge == END || edge.is_empty() {
                        continue;
                    }
                    to_execute.push(edge);
                }

                if self.finish_points.contains(&node_name) && to_execute.is_empty() {
                    break;
                }
            }
        }
    }

    pub async fn invoke_parallel(&self, input: S) -> Result<S, GraphError> {
        let entry = self.entry_point.clone().ok_or(GraphError::NoEntryPoint)?;

        let mut current_state = input;
        let mut finished: HashSet<String> = HashSet::new();
        let mut ready: Vec<String> = vec![entry];

        while !ready.is_empty() {
            let superstep_state = current_state.clone();

            let handles: Vec<_> = ready.iter().filter_map(|name| {
                if finished.contains(name) {
                    return None;
                }

                let handler = self.nodes.get(name)?.clone();
                let state_clone = superstep_state.clone();
                let has_subgraph = handler.subgraph.is_some();

                Some(async move {
                    if has_subgraph {
                        panic!("Subgraphs are not supported in invoke_parallel");
                    }
                    let partial = (handler.handler)(state_clone).await;
                    (name.clone(), partial)
                })
            }).collect();

            if handles.is_empty() {
                break;
            }

            let results = futures::future::join_all(handles).await;

            let mut next_ready: Vec<String> = Vec::new();

            for result in results {
                let (name, partial) = result;
                current_state = current_state.merge(partial);
                finished.insert(name.clone());

                let outgoing = self.get_outgoing_edges(&name, &current_state);
                for edge in outgoing {
                    if edge == END || edge.is_empty() {
                        continue;
                    }
                    if !finished.contains(&edge) && !next_ready.contains(&edge) {
                        next_ready.push(edge);
                    }
                }
            }

            ready = next_ready;
        }

        Ok(current_state)
    }

    fn get_outgoing_edges(&self, node_name: &str, state: &S) -> Vec<String> {
        let mut outgoing = Vec::new();

        for (start, end) in &self.edges {
            if start == node_name {
                outgoing.push(end.clone());
            }
        }

        if let Some(conditions) = self.conditional_edges.get(node_name) {
            for cond in conditions {
                let path_result = (cond.path_fn)(state);
                let dest = cond.path_map.get(&path_result).cloned().unwrap_or(path_result);
                outgoing.push(dest);
            }
        }

        outgoing
    }

    pub async fn ainvoke(&self, input: S) -> Result<S, GraphError> {
        self.invoke(input).await
    }
}