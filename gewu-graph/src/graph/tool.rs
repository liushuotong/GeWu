use async_trait::async_trait;
use gewu_core::traits::Tool;
use gewu_core::types::{ToolContext, ToolResult};
use schemars::schema::RootSchema;
use serde::Serialize;
use std::error::Error;

pub struct GraphTool<S: crate::State> {
    name: String,
    description: String,
    graph: crate::CompiledGraph<S>,
}

impl<S: crate::State> GraphTool<S> {
    pub fn new(name: &str, description: &str, graph: crate::CompiledGraph<S>) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            graph,
        }
    }

    pub fn tool_name(&self) -> &str {
        &self.name
    }

    pub fn tool_description(&self) -> &str {
        &self.description
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

#[async_trait]
impl<S: crate::State + schemars::JsonSchema + Send + Sync + 'static> Tool for GraphTool<S>
where
    S: 'static,
    S::Partial: Serialize,
{
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn schema(&self) -> RootSchema {
        schemars::schema_for!(S)
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, Box<dyn Error + Send + Sync>> {
        let input: S = serde_json::from_value(args)?;
        let result = self.graph.invoke(input).await?;
        Ok(ToolResult::new(&self.name, serde_json::to_string(&result)?))
    }
}
