//! Durable source runtime: one acquisition and journal writer per source.

mod catalog;
mod cursor;
pub mod history;
mod manager;
#[cfg(feature = "test-support")]
pub mod publish_probe;
mod writer;

pub use history::{SourceHistory, read_history};
pub use manager::{
    AbortReport, RuntimeConfig, RuntimeError, RuntimeState, SourceHandle, SourceManager,
    SourceProgress, StartIntent, StopReport,
};
