//! Intern's document-understanding engine.
//!
//! One local inference per document, over a distillation of the *whole*
//! document, producing a filename, a one-sentence description, and the verbatim
//! evidence behind every fact.
//!
//! Nothing in this crate talks to anything but `127.0.0.1`. Document text never
//! leaves the machine.
//!
//! ```no_run
//! use intern_engine::{Engine, ModelClient, DocumentSource, SourcePage, PageOrigin};
//!
//! let client = ModelClient::new("http://127.0.0.1:8080/v1/chat/completions", "key", "intern-local")?;
//! let engine = Engine::new(client);
//! let source = DocumentSource::from_pages(vec![SourcePage::new(1, "NOTICE OF TERMINATION", PageOrigin::Native)]);
//! let analysis = engine.analyze(&source, "pdf", &[])?;
//! println!("{} - {}", analysis.filename, analysis.description);
//! # Ok::<(), intern_engine::EngineError>(())
//! ```

#![deny(unsafe_code)]

pub mod client;
pub mod distill;
pub mod domain;
pub mod download;
pub mod engine;
pub mod error;
pub mod evidence;
pub mod legacy;
pub mod manifest;
pub mod naming;
pub mod prompt;
pub mod server;
pub mod setup;
pub mod text;
pub mod validate;
pub mod worker;

pub use client::{ModelClient, ModelRequest};
pub use distill::{DigestBudget, DocumentDigest, distill, source_from_text};
pub use domain::*;
pub use engine::Engine;
pub use error::{EngineError, EngineErrorCode, EngineResult};
pub use manifest::{ModelFile, ModelManifest, ModelRole};
pub use naming::compose_filename;
pub use server::{LlamaServer, ServerOptions};
pub use validate::validate;
pub use worker::{
    DocumentExtractor, ExtractFailure, ExtractProgress, SupervisedWorker, prepare_worker_temp_root,
};

/// Semantic version of the engine's input/output contract.
///
/// Callers that persist [`DocumentAnalysis`] should record this alongside it.
pub const ENGINE_CONTRACT_VERSION: u32 = 1;
