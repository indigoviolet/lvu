//! Nonblocking journal-to-TUI paging for live lvu sources.

mod index;
mod provider;

pub use provider::{
    AdapterError, AdapterStats, DerivedArtifactIdentity, DerivedArtifactStatus,
    EventTimeRecognition, IndexState, LiveConfig, LiveRowProvider, SourceViewStatus, StorageBudget,
    ViewStatus, display_projection, recognize_event_time,
};
