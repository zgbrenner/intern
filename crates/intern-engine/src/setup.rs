use std::{path::PathBuf, sync::Mutex};

use crate::domain::{DocumentAnalysis, DocumentSource, PageOrigin, SourcePage};

use crate::download::{CancellationToken, DiskSpace, SetupProgress, install_selected_file};
use crate::error::{
    EngineError as ModelError, EngineErrorCode as ModelErrorCode, EngineResult as ModelResult,
};
use crate::manifest::ModelManifest;
use crate::manifest::ModelRole;

/// A phrase that appears nowhere except the calibration document, so a model
/// that echoes it has actually read the text it was given.
const TEXT_MARKER: &str = "Northstar Calibration";

#[derive(Default)]
pub struct SetupOperationGate {
    active: Mutex<Option<Operation>>,
}

struct Operation {
    cancellation: CancellationToken,
    /// The work has produced its result and cancelling can no longer change
    /// it. The gate is still held, so no second operation may start until the
    /// outcome has been published.
    settled: bool,
}

impl SetupOperationGate {
    pub fn begin(&self) -> ModelResult<CancellationToken> {
        let mut active = self.active.lock().map_err(|_| setup_busy())?;
        if active.is_some() {
            return Err(setup_busy());
        }
        let cancellation = CancellationToken::new();
        *active = Some(Operation {
            cancellation: cancellation.clone(),
            settled: false,
        });
        Ok(cancellation)
    }

    /// Cancels the operation in flight, and says whether there was one to
    /// cancel. An operation that has already settled is not one: the model it
    /// installed is started and verified, and stopping it now would leave the
    /// interface saying it is ready with nothing running behind it.
    pub fn cancel(&self) -> bool {
        let Ok(active) = self.active.lock() else {
            return false;
        };
        match active.as_ref() {
            Some(operation) if !operation.settled => {
                operation.cancellation.cancel();
                true
            }
            _ => false,
        }
    }

    /// Records that the work is over, before its outcome is published.
    pub fn settle(&self) {
        if let Ok(mut active) = self.active.lock()
            && let Some(operation) = active.as_mut()
        {
            operation.settled = true;
        }
    }

    pub fn finish(&self) {
        if let Ok(mut active) = self.active.lock() {
            active.take();
        }
    }
}

/// A model file the user already has on disk, offered instead of downloading.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExistingModelSelection {
    pub model_path: PathBuf,
}

pub fn install_existing_model_files<D, F>(
    manifest: &ModelManifest,
    selection: &ExistingModelSelection,
    destination_directory: &std::path::Path,
    disk: &D,
    cancellation: &CancellationToken,
    mut progress: F,
) -> ModelResult<u64>
where
    D: DiskSpace,
    F: FnMut(SetupProgress),
{
    let total = manifest.total_bytes();
    let mut completed_before = 0_u64;
    for expected in &manifest.files {
        if cancellation.is_canceled() {
            return Err(download_canceled());
        }
        let selected = match expected.role {
            ModelRole::Model => &selection.model_path,
        };
        let offset = completed_before;
        install_selected_file(
            selected,
            expected,
            destination_directory,
            disk,
            cancellation,
            |event| {
                progress(SetupProgress {
                    stage: event.stage,
                    completed_bytes: offset + event.completed_bytes,
                    total_bytes: total,
                });
            },
        )?;
        completed_before += expected.size;
    }
    Ok(total)
}

/// A document the installed model must be able to file correctly before Intern
/// will use it.
///
/// The probe exercises the shipping path end to end - distillation, the real
/// prompt, the grammar, evidence validation, and naming - so a model that
/// installs but cannot actually do the job is caught at setup rather than on
/// the user's first document.
pub struct SemanticProbe {
    pub document: DocumentSource,
    expected_marker: &'static str,
}

pub fn semantic_probes() -> ModelResult<Vec<SemanticProbe>> {
    Ok(vec![SemanticProbe {
        document: DocumentSource::from_pages(vec![SourcePage::new(
            1,
            format!(
                "NOTICE OF CALIBRATION\n\nDate of this Notice: January 2, 2024\n\n\
                 To: {TEXT_MARKER} Holdings LLC\n\n\
                 This notice confirms that the local text model path is working and that \
                 evidence quoted back from this document can be checked against it.\n"
            ),
            PageOrigin::PlainText,
        )]),
        expected_marker: TEXT_MARKER,
    }])
}

pub fn validate_semantic_probe(
    probe: &SemanticProbe,
    analysis: &DocumentAnalysis,
) -> ModelResult<()> {
    let marker = probe.expected_marker.to_lowercase();
    let read_the_document = analysis
        .proposal
        .parties
        .iter()
        .chain(std::iter::once(&analysis.description))
        .chain(analysis.proposal.evidence.parties.iter())
        .any(|value| value.to_lowercase().contains(&marker));
    let found_the_date = analysis.proposal.document_date.as_deref() == Some("2024-01-02");
    if read_the_document && found_the_date {
        Ok(())
    } else {
        Err(self_test_failed())
    }
}

const fn setup_busy() -> ModelError {
    ModelError::new(
        ModelErrorCode::SetupBusy,
        "a model setup operation is already active",
    )
}

const fn download_canceled() -> ModelError {
    ModelError::new(ModelErrorCode::DownloadCanceled, "model setup was canceled")
}

const fn self_test_failed() -> ModelError {
    ModelError::new(
        ModelErrorCode::ModelSelfTestFailed,
        "local model semantic self-test failed",
    )
}
