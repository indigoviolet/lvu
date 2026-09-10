//! Bounded, loss-preserving query foundation for lvu.
//!
//! Python is only used when a definition is compiled. Normal batch execution is
//! native Polars, and cached expression JSON is always validated again in Rust.

pub use lvu_core::{
    ExactFieldConstraint, ExactFieldError, ExactScalar, MAX_EXACT_FIELD_BYTES,
    MAX_EXACT_SCALAR_BYTES,
};

pub mod adapter;
pub mod column_stats;
pub mod engine;
pub mod exact_key_filter;
pub mod grouping_flags;
pub mod host;
pub mod parquet;
pub mod regex_enrichment;
pub mod state;
pub mod time_field;
/// Timestamp-ordered union of accepted typed frames (union-views worktree).
/// Kept distinct from any grouping helper: this merges whole frames across
/// views, grouping never does.
pub mod union;
pub mod validate;

pub use adapter::*;
pub use column_stats::*;
pub use engine::*;
pub use exact_key_filter::*;
pub use grouping_flags::*;
pub use host::*;
pub use parquet::*;
pub use regex_enrichment::*;
pub use state::*;
pub use time_field::*;
pub use union::union_sorted_frames;
pub use validate::*;
