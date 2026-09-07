//! Durable source runtime: one acquisition and journal writer per source.

mod catalog;
mod cursor;
pub mod history;
mod manager;
mod writer;

pub use history::{SourceHistory, read_history};
pub use manager::{
    AbortReport, RuntimeConfig, RuntimeError, RuntimeState, SourceHandle, SourceManager,
    SourceProgress, StartIntent, StopReport,
};
