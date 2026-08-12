use std::{fs, path::Path, sync::Mutex};

use intern_app::model::{
    ModelErrorCode,
    download::{CancellationToken, DISK_RESERVE_BYTES, DiskSpace, SetupStage},
    manifest::{ModelFile, ModelManifest},
    setup::{
        ExistingModelSelection, SetupOperationGate, install_existing_model_files, semantic_probes,
        validate_semantic_probe,
    },
};
use intern_core::{DateKind, Evidence, ModelProposal};
use tempfile::tempdir;

#[derive(Clone, Copy)]
struct FixedDisk(u64);

impl DiskSpace for FixedDisk {
    fn available_bytes(&self, _path: &Path) -> intern_app::model::ModelResult<u64> {
        Ok(self.0)
    }
}

fn manifest() -> ModelManifest {
    ModelManifest {
        schema_version: 1,
        model_id: "test-model".into(),
        files: vec![
            ModelFile {
                name: "model.gguf".into(),
                url: "https://example.invalid/model.gguf".into(),
                size: 11,
                sha256: "357e5d6fafa34d27360fec24b4326d3534905e33c6acdee60198fb078b7b79e5".into(),
            },
            ModelFile {
                name: "projector.gguf".into(),
                url: "https://example.invalid/projector.gguf".into(),
                size: 15,
                sha256: "b95a25c1f308da898c582dc7728f9c1157ab1f5b34c36109a5203f0ac71a2f85".into(),
            },
        ],
    }
}

#[test]
fn setup_gate_cancels_the_active_token_and_allows_a_fresh_resume_only_after_finish() {
    let gate = SetupOperationGate::default();
    let first = gate.begin().unwrap();

    let busy = gate.begin().err().expect("second setup must be rejected");
    assert_eq!(busy.code(), ModelErrorCode::SetupBusy);
    assert!(gate.cancel());
    assert!(first.is_canceled());

    gate.finish();
    let resumed = gate.begin().unwrap();
    assert!(!resumed.is_canceled());
}

#[test]
fn existing_pair_install_validates_and_publishes_both_files_with_aggregate_progress() {
    let temp = tempdir().unwrap();
    let model = temp.path().join("selected-model.gguf");
    let projector = temp.path().join("selected-projector.gguf");
    fs::write(&model, b"model-bytes").unwrap();
    fs::write(&projector, b"projector-bytes").unwrap();
    let destination = temp.path().join("installed");
    let progress = Mutex::new(Vec::new());

    let total = install_existing_model_files(
        &manifest(),
        &ExistingModelSelection {
            model_path: model,
            projector_path: projector,
        },
        &destination,
        &FixedDisk(DISK_RESERVE_BYTES + 100),
        &CancellationToken::new(),
        |event| progress.lock().unwrap().push(event),
    )
    .unwrap();

    assert_eq!(total, 26);
    assert_eq!(
        fs::read(destination.join("model.gguf")).unwrap(),
        b"model-bytes"
    );
    assert_eq!(
        fs::read(destination.join("projector.gguf")).unwrap(),
        b"projector-bytes"
    );
    let events = progress.into_inner().unwrap();
    assert_eq!(events.last().unwrap().stage, SetupStage::Complete);
    assert_eq!(events.last().unwrap().completed_bytes, 26);
    assert_eq!(events.last().unwrap().total_bytes, 26);
}

#[test]
fn canceled_existing_pair_can_resume_without_replacing_the_verified_first_file() {
    let temp = tempdir().unwrap();
    let model = temp.path().join("selected-model.gguf");
    let projector = temp.path().join("selected-projector.gguf");
    fs::write(&model, b"model-bytes").unwrap();
    fs::write(&projector, b"projector-bytes").unwrap();
    let destination = temp.path().join("installed");
    let selection = ExistingModelSelection {
        model_path: model,
        projector_path: projector,
    };
    let cancellation = CancellationToken::new();
    let cancel_from_progress = cancellation.clone();

    let error = install_existing_model_files(
        &manifest(),
        &selection,
        &destination,
        &FixedDisk(u64::MAX),
        &cancellation,
        move |event| {
            if event.stage == SetupStage::Complete && event.completed_bytes == 11 {
                cancel_from_progress.cancel();
            }
        },
    )
    .unwrap_err();

    assert_eq!(error.code(), ModelErrorCode::DownloadCanceled);
    assert!(destination.join("model.gguf").exists());
    assert!(!destination.join("projector.gguf").exists());

    install_existing_model_files(
        &manifest(),
        &selection,
        &destination,
        &FixedDisk(u64::MAX),
        &CancellationToken::new(),
        |_| {},
    )
    .unwrap();
    assert!(destination.join("projector.gguf").exists());
}

fn proposal(subject: &str, description: &str, subject_evidence: Option<&str>) -> ModelProposal {
    ModelProposal {
        document_date: Some("2024-01-02".into()),
        date_kind: Some(DateKind::Effective),
        document_type: Some("Calibration Notice".into()),
        filename_subject: Some(subject.into()),
        parties: Vec::new(),
        description: description.into(),
        confidence: 0.99,
        needs_review: false,
        review_reasons: Vec::new(),
        evidence: Evidence {
            date: Some("January 2, 2024".into()),
            document_type: Some("Calibration Notice".into()),
            subject: subject_evidence.map(str::to_owned),
            parties: Vec::new(),
        },
    }
}

#[test]
fn semantic_probes_require_grounded_text_and_visible_image_markers() {
    let probes = semantic_probes().unwrap();
    assert_eq!(probes.len(), 2);
    assert!(probes[0].document.image.is_none());
    assert!(probes[1].document.image.is_some());
    assert!(!probes[1].document.text.contains("VISION CALIBRATION 42"));
    assert!(
        probes[1]
            .document
            .image
            .as_ref()
            .unwrap()
            .bytes
            .starts_with(b"\x89PNG\r\n\x1a\n")
    );

    validate_semantic_probe(
        &probes[0],
        &proposal(
            "Northstar Calibration",
            "A calibration notice for Northstar Calibration.",
            Some("Subject: Northstar Calibration"),
        ),
    )
    .unwrap();
    validate_semantic_probe(
        &probes[1],
        &proposal(
            "VISION CALIBRATION 42",
            "The image displays VISION CALIBRATION 42.",
            None,
        ),
    )
    .unwrap();

    let generic = proposal("Calibration", "A generic calibration notice.", None);
    assert_eq!(
        validate_semantic_probe(&probes[0], &generic)
            .unwrap_err()
            .code(),
        ModelErrorCode::ModelSelfTestFailed
    );
    assert_eq!(
        validate_semantic_probe(&probes[1], &generic)
            .unwrap_err()
            .code(),
        ModelErrorCode::ModelSelfTestFailed
    );
}
