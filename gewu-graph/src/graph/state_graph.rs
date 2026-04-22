use crate::graph::constants::{END, START};
use crate::state::State;
use futures::Stream;
use futures::StreamExt;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::marker::Send as StdSend;
use thiserror::Error;
use tokio::sync::mpsc;

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

#[derive(Clone, Debug)]
pub struct GraphSend<S: State>(pub String, pub S);

#[derive(Debug)]
pub enum NodeResult<S: State> {
    Partial(S::Partial),
    Send(Vec<GraphSend<S>>),
    Custom(serde_json::Value),
}

impl<S: State> NodeResult<S> {
    fn map_partial(partial: S::Partial) -> Self {
        NodeResult::Partial(partial)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamMode {
    Values,
    Updates,
    Custom,
    Tasks,
}

#[derive(Debug)]
pub enum StreamOutput<S: State> {
    Value(S),
    Update { node: String, update: S::Partial },
    Custom { node: String, data: serde_json::Value },
    TaskStart { id: String, name: String },
    TaskEnd { id: String, name: String, result: S::Partial },
}

pub struct StreamWriter {
    sender: mpsc::UnboundedSender<serde_json::Value>,
}

impl StreamWriter {
    pub fn write(&self, data: serde_json::Value) {
        let _ = self.sender.send(data);
    }
}

#[derive(Clone)]
pub struct NodeHandler<S: State> {
    name: String,
    handler: Arc<dyn Fn(S) -> Pin<Box<dyn Future<Output = NodeResult<S>> + StdSend>> + Send + Sync>,
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

impl<S: State + 'static> NodeHandler<S> {
    pub fn new<F, Fut>(name: &str, handler: F) -> Self
    where
        F: Fn(S) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = NodeResult<S>> + Send + 'static,
    {
        let handler = Arc::new(handler);
        let wrapper: Arc<dyn Fn(S) -> Pin<Box<dyn Future<Output = NodeResult<S>> + StdSend>> + Send + Sync> =
            Arc::new(move |state: S| {
                let h = handler.clone();
                Box::pin(async move {
                    h(state).await
                }) as Pin<Box<dyn Future<Output = NodeResult<S>> + StdSend>>
            });
        Self {
            name: name.to_string(),
            handler: wrapper,
            subgraph: None,
        }
    }

    pub fn with_subgraph(subgraph: CompiledGraph<S>) -> Self {
        let wrapper: Arc<dyn Fn(S) -> Pin<Box<dyn Future<Output = NodeResult<S>> + StdSend>> + Send + Sync> =
            Arc::new(move |_: S| {
                Box::pin(async move {
                    NodeResult::map_partial(S::Partial::default())
                }) as Pin<Box<dyn Future<Output = NodeResult<S>> + StdSend>>
            });
        Self {
            name: subgraph.entry_point.clone().unwrap_or_default(),
            handler: wrapper,
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

impl<S: State + 'static> Default for StateGraph<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: State + 'static> StateGraph<S> {
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
        Fut: Future<Output = NodeResult<S>> + Send + 'static,
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
    pub fn invoke(&self, input: S) -> Result<S, GraphError> {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(self.ainvoke(input))
    }

    pub async fn ainvoke(&self, input: S) -> Result<S, GraphError> {
        self.ainvoke_internal(input, 0).await
    }

    async fn ainvoke_internal(
        &self,
        input: S,
        depth: usize,
    ) -> Result<S, GraphError> {
        if depth > MAX_SUBGRAPH_DEPTH {
            return Err(GraphError::SubgraphDepthLimitExceeded(MAX_SUBGRAPH_DEPTH));
        }

        let entry = match self.entry_point.clone() {
            Some(e) => e,
            None => return Err(GraphError::NoEntryPoint),
        };

        let mut current_state = input;
        let mut to_execute: Vec<String> = vec![entry];
        let mut sent_to: HashSet<String> = HashSet::new();

        while let Some(node_name) = to_execute.pop() {
            let handler = self
                .nodes
                .get(&node_name)
                .ok_or_else(|| GraphError::NodeNotFound(node_name.clone()))?;

            if let Some(subgraph) = &handler.subgraph {
                let subgraph = subgraph.clone();
                let new_depth = depth + 1;
                current_state = Box::pin(async move {
                    subgraph.ainvoke_internal(current_state, new_depth).await
                }).await?;
            } else {
                let result = (handler.handler)(current_state.clone()).await;
                let is_send = matches!(result, NodeResult::Send(_));
                current_state = match result {
                    NodeResult::Partial(partial) => current_state.merge(partial),
                    NodeResult::Send(sends) => {
                        let sent_names: Vec<String> = sends.iter().map(|s| s.0.clone()).collect();
                        for name in &sent_names {
                            sent_to.insert(name.clone());
                        }
                        current_state = self.process_sends(sends, current_state, depth).await?;
                        current_state
                    }
                    NodeResult::Custom(_) => current_state,
                };
                if is_send {
                    continue;
                }
            }

            let outgoing = self.get_outgoing_edges(&node_name, &current_state);
            for edge in outgoing {
                if edge == END || edge.is_empty() || sent_to.contains(&edge) {
                    continue;
                }
                to_execute.push(edge);
            }

            if self.finish_points.contains(&node_name) && to_execute.is_empty() {
                return Ok(current_state);
            }
        }

        Ok(current_state)
    }

    async fn process_sends(
        &self,
        sends: Vec<GraphSend<S>>,
        mut current_state: S,
        depth: usize,
    ) -> Result<S, GraphError> {
        if sends.is_empty() {
            return Ok(current_state);
        }

        #[derive(Debug)]
        enum SendResult<S: State> {
            Partial(NodeResult<S>),
            Subgraph(Box<CompiledGraph<S>>, S),
        }

        let mut futures: Vec<_> = Vec::new();

        for send in sends {
            let handler = self.nodes.get(&send.0).cloned();
            let state_to_use = send.1;
            
            futures.push(async move {
                match handler {
                    Some(h) if h.subgraph.is_none() => {
                        let result = (h.handler)(state_to_use).await;
                        Ok(SendResult::Partial(result))
                    }
                    Some(h) if h.subgraph.is_some() => {
                        let subgraph = h.subgraph.clone().unwrap();
                        Ok(SendResult::Subgraph(subgraph, state_to_use))
                    }
                    _ => Err(GraphError::NodeNotFound(send.0.clone())),
                }
            });
        }

        let results: Vec<Result<SendResult<S>, GraphError>> =
            futures::future::join_all(futures).await;

        for result in results {
            match result {
                Ok(SendResult::Partial(node_result)) => {
                    match node_result {
                        NodeResult::Partial(partial) => {
                            current_state = current_state.merge(partial);
                        }
                        NodeResult::Send(nested_sends) => {
                            let nested_sends = nested_sends;
                            let this = self;
                            let d = depth + 1;
                            current_state = Box::pin(async move {
                                this.process_sends(nested_sends, current_state, d).await
                            }).await?;
                        }
                        NodeResult::Custom(_) => {}
                    }
                }
                Ok(SendResult::Subgraph(subgraph, state)) => {
                    current_state = Box::pin(async {
                        subgraph.ainvoke_internal(state, depth + 1).await
                    }).await?;
                }
                Err(e) => return Err(e),
            }
        }

        Ok(current_state)
    }

    pub fn stream(&self, input: S) -> impl Stream<Item = Result<StreamOutput<S>, GraphError>> + '_ {
        self.stream_with_modes(input, vec![StreamMode::Values])
    }

    pub fn stream_with_modes(
        &self,
        input: S,
        modes: Vec<StreamMode>,
    ) -> impl Stream<Item = Result<StreamOutput<S>, GraphError>> + '_ {
        async_stream::try_stream! {
            let entry = self.entry_point.clone().ok_or(GraphError::NoEntryPoint)?;

            let mut current_state = input;
            let mut to_execute: Vec<String> = vec![entry];
            let has_values = modes.contains(&StreamMode::Values);
            let has_updates = modes.contains(&StreamMode::Updates);
            let has_custom = modes.contains(&StreamMode::Custom);
            let has_tasks = modes.contains(&StreamMode::Tasks);

            if has_values {
                yield StreamOutput::Value(current_state.clone());
            }

            while let Some(node_name) = to_execute.pop() {
                let handler = self.nodes.get(&node_name)
                    .ok_or_else(|| GraphError::NodeNotFound(node_name.clone()))?;

                let task_id = format!("{}_{}", node_name, uuid::Uuid::new_v4());

                if has_tasks {
                    yield StreamOutput::TaskStart {
                        id: task_id.clone(),
                        name: node_name.clone(),
                    };
                }

                let node_result = if let Some(subgraph) = &handler.subgraph {
                    let new_state = subgraph.ainvoke(current_state.clone()).await?;
                    current_state = new_state;
                    NodeResult::Partial(S::Partial::default())
                } else {
                    (handler.handler)(current_state.clone()).await
                };

                let is_send = matches!(node_result, NodeResult::Send(_));
                match node_result {
                    NodeResult::Partial(partial) => {
                        if has_updates {
                            yield StreamOutput::Update {
                                node: node_name.clone(),
                                update: partial.clone(),
                            };
                        }
                        current_state = current_state.merge(partial);
                    }
                    NodeResult::Send(sends) => {
                        current_state = self.process_sends(sends, current_state, 0).await?;
                    }
                    NodeResult::Custom(data) => {
                        if has_custom {
                            yield StreamOutput::Custom {
                                node: node_name.clone(),
                                data,
                            };
                        }
                    }
                }

                if has_tasks {
                    yield StreamOutput::TaskEnd {
                        id: task_id.clone(),
                        name: node_name.clone(),
                        result: S::Partial::default(),
                    };
                }

                if has_values {
                    yield StreamOutput::Value(current_state.clone());
                }

                if is_send {
                    continue;
                }

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

    pub async fn astream(&self, input: S) -> impl Stream<Item = Result<S, GraphError>> + '_ {
        self.stream(input).filter_map(|r| async move {
            match r {
                Ok(StreamOutput::Value(v)) => Some(Ok(v)),
                Ok(_) => None,
                Err(e) => Some(Err(e)),
            }
        })
    }

    pub async fn invoke_parallel(&self, input: S) -> Result<S, GraphError> {
        let entry = self.entry_point.clone().ok_or(GraphError::NoEntryPoint)?;

        let mut current_state = input;
        let mut finished: HashSet<String> = HashSet::new();
        let mut ready: Vec<String> = vec![entry];

        while !ready.is_empty() {
            let superstep_state = current_state.clone();

            let mut futures: Vec<_> = Vec::new();

            for name in ready.iter() {
                if finished.contains(name) {
                    continue;
                }
                if let Some(handler) = self.nodes.get(name) {
                    let state_clone = superstep_state.clone();
                    let has_subgraph = handler.subgraph.is_some();

                    futures.push(async move {
                        if has_subgraph {
                            panic!("Subgraphs are not supported in invoke_parallel");
                        }
                        let result = (handler.handler)(state_clone).await;
                        (name.clone(), result)
                    });
                }
            }

            if futures.is_empty() {
                break;
            }

            let results: Vec<(String, NodeResult<S>)> =
                futures::future::join_all(futures).await;

            let mut next_ready: Vec<String> = Vec::new();

            for (name, result) in results {
                let is_send = matches!(result, NodeResult::Send(_));
                match result {
                    NodeResult::Partial(partial) => {
                        current_state = current_state.merge(partial);
                    }
                    NodeResult::Send(sends) => {
                        for send in sends {
                            if !finished.contains(&send.0) && !next_ready.contains(&send.0) {
                                next_ready.push(send.0.clone());
                            }
                        }
                    }
                    NodeResult::Custom(_) => {}
                }
                finished.insert(name.clone());

                if is_send {
                    continue;
                }

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
}