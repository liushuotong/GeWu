pub mod state_trait;
#[cfg(test)]
mod tests;

pub use state_trait::{Append, LastValue, Reducer, State, Sum};
