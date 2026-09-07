//! Bounded, loss-preserving query foundation for lvu.
//!
//! Python is only used when a definition is compiled. Normal batch execution is
//! native Polars, and cached expression JSON is always validated again in Rust.

pub use lvu_core::{
    ExactFieldConstraint, ExactFieldError, ExactScalar, MAX_EXACT_FIELD_BYTES,
    MAX_EXACT_SCALAR_BYTES,
};

pub mod adapter;
pub mod engine;
pub mod host;
pub mod parquet;
pub mod regex_enrichment;
pub mod state;
pub mod time_field;
pub mod validate;

pub use adapter::*;
pub use engine::*;
pub use host::*;
pub use parquet::*;
pub use regex_enrichment::*;
pub use state::*;
pub use time_field::*;
pub use validate::*;
