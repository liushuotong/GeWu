//! GeWu Evolve - Self-critique, evolution, and tool creation for AI agents.
//!
//! This crate provides capabilities for AI agents to:
//! - Analyze and learn from failures (self-critique)
//! - Retry tasks with improved strategies
//! - Create and save new tools
//! - Execute Python code in a sandboxed environment
//!
//! # Modules
//!
//! - [`critique`] - Self-critique analysis for error understanding
//! - [`sandbox`] - Python code execution sandbox
//! - [`skill_library`] - Tool persistence and dynamic loading
//! - [`tool_builder`] - LLM-based tool generation
//! - [`graph`] - Evolution state machine for iterative improvement
//!
//! # Usage
//!
//! ```ignore
//! use gewu_evolve::{SelfCritique, PythonSandbox, SkillLibrary};
//!
//! // Analyze a failure
//! let critique = SelfCritique::analyze(task, error, llm).await?;
//!
//! // Execute Python code safely
//! let result = sandbox.execute(code, input).await?;
//!
//! // Create and save a new tool
//! let skill = ToolBuilder::generate(description, llm).await?;
//! library.save_skill(&skill).await?;
//! ```

pub mod config;
pub mod critique;
pub mod graph;
pub mod sandbox;
pub mod skill_library;
pub mod tool_builder;

pub use config::Config;
pub use critique::{CritiqueResult, SelfCritique};
pub use sandbox::{PythonResult, PythonSandbox};
pub use skill_library::{Skill, SkillImplementation, SkillLibrary, SkillMetadata};
pub use tool_builder::{GeneratedTool, ToolBuilder, ValidationResult};
