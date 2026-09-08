//! Identity authorization is a host concern, but no processing path may bypass it.
use crate::pipeline::PipelineResult;
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionStage {
    Enqueue,
    Extract,
    Analyze,
    Apply,
}

pub trait AdmissionGuard: Send + Sync {
    /// None means an ordinary local file. Some is the SHA-256 of the exact
    /// provider-matched bytes; the queue must also bind it to its own hash.
    fn authorize(&self, path: &Path, stage: AdmissionStage) -> PipelineResult<Option<String>>;
    /// Called only after an analysis actually completed, never at selection.
    fn processed(&self, _path: &Path, _hash: &str) {}
}
#[derive(Default)]
pub struct LocalAdmission;
impl AdmissionGuard for LocalAdmission {
    fn authorize(&self, _path: &Path, _stage: AdmissionStage) -> PipelineResult<Option<String>> {
        Ok(None)
    }
}
