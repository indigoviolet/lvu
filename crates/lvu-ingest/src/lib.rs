//! Durable source runtime: one acquisition and journal writer per source.

mod catalog;
mod cursor;
mod manager;
mod writer;

pub use manager::{
    AbortReport, RuntimeConfig, RuntimeError, RuntimeState, SourceHandle, SourceManager,
    SourceProgress, StopReport,
};
