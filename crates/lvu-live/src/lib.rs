//! Nonblocking journal-to-TUI paging for live lvu sources.

mod index;
mod provider;
pub mod time;

pub use provider::{
    AdapterError, AdapterStats, DerivedArtifactIdentity, DerivedArtifactStatus,
    EventTimeRecognition, IndexState, LiveConfig, LiveRowProvider, MAX_EVENT_TIME_RECORD_BYTES,
    SourceViewStatus, StorageBudget, ViewStatus, display_projection, display_timestamp,
    recognize_event_time, recognize_event_time_with,
};
pub use time::{
    CandidateSummary, EpochUnit, MAX_RECOGNITION_RECORD_BYTES, RecognitionOptions,
    RecognitionReport, TimeFieldRef, TimeFieldSelection, TimeFormat, TimeInterpretation,
    TimeOutcome, TimeReading, ZoneAssumption, recognize_record, recognize_sample,
};
