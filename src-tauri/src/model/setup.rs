use std::{path::PathBuf, sync::Mutex};

use base64::Engine as _;
use intern_core::ModelProposal;

use super::{
    ModelError, ModelErrorCode, ModelResult,
    client::{DocumentInput, ImageInput},
    download::{CancellationToken, DiskSpace, SetupProgress, install_selected_file},
    manifest::ModelManifest,
};

const TEXT_MARKER: &str = "Northstar Calibration";
const IMAGE_MARKER: &str = "VISION CALIBRATION 42";
const IMAGE_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAASwAAABICAMAAAB7sTi/AAAABlBMVEX9/f0cHBwvibKYAAAB7UlEQVR42u2XQXbDIAwFpftfuoskBUkfLOMs2vdmNrUdA5+xMaoZAAAAAAAAAAAAAAAAAAAA/EXcPR5/zv3F+3i6ZbSYj2q3ped66+vYfR7L4tnc2gMHqSyPct9WPMzB5nvEtGQsTzc2ZbkaZCXrfqr5tzrIA1ljwOker39lrHI5equyQrM0SGw9d3AzVegpj9GWFR9/nECcVHpR1DqVEeY105UVWmxkdVN9OtKPtW+rRnctKw0yBk+yxBDycafG0l2Z4+L2q1TfkhXeyeWb5SqWdWTN38L7b1Zo3ZMlJ/H+4ZmssfZDhDIpdSWu3VWAtiwTpxeyuql2S+aprFA57GLZtSwPzRulQ3KVBu/Isp6so+JBTiaXDstYfilLSRCy4q4uW/dlpVRlOzhztVkm1pBlX5CVBtu9aE1ZIlUJd+RqI2tVTsTLvpXl8c3p7oZZXa09NtuOTFWL1wNX6629JSvXQfZAVn1qV7K6qYqsU1eh452sUv6NHKVNPVMb52b2Hrf6+jTvpSq9HLqy8uB06VP/sRjNF4nqpt+us2TrKqudSt58aCvNSC4d/e3dfaaUrPxbWR9Trw1Z7VRJVlzcD2SpOmu6nNLKOtRLgfS7sFay5GBinnaa6juyAAAAAAAAAAAAAAAAAAAAAOA/8wMNPQVK/nRabAAAAABJRU5ErkJggg==";

#[derive(Default)]
pub struct SetupOperationGate {
    active: Mutex<Option<CancellationToken>>,
}

impl SetupOperationGate {
    pub fn begin(&self) -> ModelResult<CancellationToken> {
        let mut active = self.active.lock().map_err(|_| setup_busy())?;
        if active.is_some() {
            return Err(setup_busy());
        }
        let cancellation = CancellationToken::new();
        *active = Some(cancellation.clone());
        Ok(cancellation)
    }

    pub fn cancel(&self) -> bool {
        let Ok(active) = self.active.lock() else {
            return false;
        };
        if let Some(cancellation) = active.as_ref() {
            cancellation.cancel();
            true
        } else {
            false
        }
    }

    pub fn finish(&self) {
        if let Ok(mut active) = self.active.lock() {
            active.take();
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExistingModelSelection {
    pub model_path: PathBuf,
    pub projector_path: PathBuf,
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
    let [model, projector] = manifest.files.as_slice() else {
        return Err(ModelError::new(
            ModelErrorCode::ManifestInvalid,
            "model manifest must contain the model and projector",
        ));
    };
    if selection.model_path == selection.projector_path {
        return Err(ModelError::new(
            ModelErrorCode::ModelFileInvalid,
            "model and projector selections must be different files",
        ));
    }
    let total = model
        .size
        .checked_add(projector.size)
        .ok_or_else(|| {
            ModelError::new(
                ModelErrorCode::ManifestInvalid,
                "model manifest size overflow",
            )
        })?;
    let selected = [&selection.model_path, &selection.projector_path];
    let expected = [model, projector];
    let mut completed_before = 0_u64;
    for (selected, expected) in selected.into_iter().zip(expected) {
        if cancellation.is_canceled() {
            return Err(download_canceled());
        }
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

pub struct SemanticProbe {
    pub document: DocumentInput,
    expected_marker: &'static str,
    requires_text_evidence: bool,
}

pub fn semantic_probes() -> ModelResult<Vec<SemanticProbe>> {
    let image = base64::engine::general_purpose::STANDARD
        .decode(IMAGE_PNG_BASE64)
        .map_err(|_| self_test_failed())?;
    Ok(vec![
        SemanticProbe {
            document: DocumentInput {
                text: format!(
                    "CALIBRATION NOTICE\nSubject: {TEXT_MARKER}\nEffective Date: January 2, 2024\nThis notice verifies the local text model path."
                ),
                image: None,
            },
            expected_marker: TEXT_MARKER,
            requires_text_evidence: true,
        },
        SemanticProbe {
            document: DocumentInput {
                text: "Extract the visible document metadata from the attached calibration image."
                    .into(),
                image: Some(ImageInput {
                    media_type: "image/png".into(),
                    bytes: image,
                }),
            },
            expected_marker: IMAGE_MARKER,
            requires_text_evidence: false,
        },
    ])
}

pub fn validate_semantic_probe(
    probe: &SemanticProbe,
    proposal: &ModelProposal,
) -> ModelResult<()> {
    let marker = probe.expected_marker.to_lowercase();
    let facts = proposal
        .document_type
        .iter()
        .chain(proposal.filename_subject.iter())
        .chain(proposal.parties.iter())
        .chain(std::iter::once(&proposal.description))
        .any(|value| value.to_lowercase().contains(&marker));
    let evidence = proposal
        .evidence
        .date
        .iter()
        .chain(proposal.evidence.document_type.iter())
        .chain(proposal.evidence.subject.iter())
        .chain(proposal.evidence.parties.iter())
        .any(|value| value.to_lowercase().contains(&marker));
    if facts && (!probe.requires_text_evidence || evidence) {
        Ok(())
    } else {
        Err(self_test_failed())
    }
}

const fn setup_busy() -> ModelError {
    ModelError::new(ModelErrorCode::SetupBusy, "a model setup operation is already active")
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
