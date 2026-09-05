//! The document queue: durable state machine, leases, and safe apply.
//!
//! This is the layer between the engine and whatever is driving it. It owns the
//! order documents are processed in, what happens when one crashes, and how a
//! rename is applied and undone. It has no user interface and no dependency on
//! one, so a desktop app, a CLI, or a watched folder can all sit on top of it.

#![deny(unsafe_code)]

pub mod paths;
pub mod pipeline;
pub mod settings;

pub use pipeline::{
    AnalyzerBoundary, DATE_REQUIRED, DuplicateOracle, FileActions, FiledDocument, FilingSink,
    FilingSinks, KnownFiling, ModelFailure, Pipeline, PipelineError, PipelineEventSink,
    PipelineItem, PipelineProgress, PipelineResult, ProposalRecord, UnfiledDocument,
    WorkerBoundary, WorkerFailure, layout_subfolder, leading_date, proposal_as_applied,
    target_folder,
};
pub use settings::{AppSettings, DestinationLayout, ModelSource, SettingsStore};
