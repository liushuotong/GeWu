//! Experiences tool implementation.
//!
//! This module provides the [`SearchExperiencesTool`] which wraps the
//! [`Experiences`](super::super::traits::Experiences) trait to expose
//! experience search as a callable tool for the agent.

use crate::traits::{Experiences, Tool};
use crate::types::{ExperienceQuery, RecalledExperience, ToolContext, ToolResult};
use async_trait::async_trait;
use schemars::schema::RootSchema;
use schemars::JsonSchema;
use serde::Deserialize;
use std::error::Error;
use std::sync::Arc;

/// Input schema for the search_experiences tool.
#[derive(JsonSchema, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchExperiencesInput {
    /// The query text to search for similar experiences.
    pub query: String,
    /// Maximum number of experiences to return (default: 3).
    #[serde(default)]
    pub limit: Option<usize>,
}

/// A tool that allows the agent to recall past experiences.
///
/// Implements the [`Tool`](super::super::traits::Tool) trait, wrapping an
/// [`Experiences`] instance to provide search functionality as a callable tool.
pub struct SearchExperiencesTool {
    experiences: Arc<dyn Experiences>,
    default_limit: usize,
}

impl SearchExperiencesTool {
    /// Creates a new `SearchExperiencesTool`.
    ///
    /// # Arguments
    ///
    /// * `experiences` - The experiences store to query
    pub fn new(experiences: Arc<dyn Experiences>) -> Self {
        Self {
            experiences,
            default_limit: 3,
        }
    }

    /// Formats recalled experiences as context for the LLM.
    ///
    /// Returns an empty string if no experiences are recalled.
    ///
    /// # Arguments
    ///
    /// * `experiences` - The recalled experiences to format
    ///
    /// # Returns
    ///
    /// A formatted string containing the experience contents
    fn format_experiences_context(experiences: &[RecalledExperience]) -> String {
        if experiences.is_empty() {
            return String::new();
        }

        experiences
            .iter()
            .map(|e| e.experience.content.clone())
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

#[async_trait]
impl Tool for SearchExperiencesTool {
    fn name(&self) -> &str {
        "search_experiences"
    }

    fn description(&self) -> &str {
        "Search past experiences for relevant lessons learned. Returns formatted context from similar experiences."
    }

    fn schema(&self) -> RootSchema {
        schemars::schema_for!(SearchExperiencesInput)
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, Box<dyn Error + Send + Sync>> {
        let input: SearchExperiencesInput = serde_json::from_value(args)?;
        let limit = input.limit.unwrap_or(self.default_limit);

        let query = ExperienceQuery {
            query_text: input.query,
            limit,
        };

        let recalled = self.experiences.recall(&query).await?;

        let output = Self::format_experiences_context(&recalled);

        Ok(ToolResult::new("search_experiences", output))
    }
}