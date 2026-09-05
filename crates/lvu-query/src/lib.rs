//! Bounded, loss-preserving query foundation for lvu.
//!
//! Python is only used when a definition is compiled. Normal batch execution is
//! native Polars, and cached expression JSON is always validated again in Rust.

pub mod adapter;
pub mod engine;
pub mod host;
pub mod parquet;
pub mod state;
pub mod validate;

pub use adapter::*;
pub use engine::*;
pub use host::*;
pub use parquet::*;
pub use state::*;
pub use validate::*;
