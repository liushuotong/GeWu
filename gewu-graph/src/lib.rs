pub mod channels;
pub mod graph;
pub mod state;
#[cfg(test)]
mod tests;

pub use channels::{AnyValueChannel, BaseChannel, BinaryOperatorChannel, EphemeralValueChannel, LastValueChannel, NamedBarrierChannel, TopicChannel};
pub use graph::{CompiledGraph, GraphError, GraphSend, GraphTool, NodeResult, StateGraph, StreamMode, StreamOutput, StreamWriter};
pub use state::{Append, LastValue, Reducer, State, Sum};

pub const START: &str = "__start__";
pub const END: &str = "__end__";