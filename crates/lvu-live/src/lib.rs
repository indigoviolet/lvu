//! Nonblocking journal-to-TUI paging for live lvu sources.

mod index;
mod provider;

pub use provider::{
    AdapterError, AdapterStats, EventTimeRecognition, IndexState, LiveConfig, LiveRowProvider,
    SourceViewStatus, ViewStatus, display_projection, recognize_event_time,
};
