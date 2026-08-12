use std::{fs, path::Path, sync::Mutex};

use intern_engine::{
    AnalysisTelemetry, DateRole, DigestBudget, DocumentAnalysis, Evidence, ModelProposal,
    PartyRelation, distill, engine::finish, validate,
};
use intern_engine::{
    ModelFile, ModelManifest, ModelRole,
    download::{CancellationToken, DISK_RESERVE_BYTES, DiskSpace, SetupStage},
    error::EngineErrorCode as ModelErrorCode,
    setup::{
        ExistingModelSelection, SetupOperationGate, install_existing_model_files, semantic_probes,
        validate_semantic_probe,
    },
};
use tempfile::tempdir;

#[derive(Clone, Copy)]
struct FixedDisk(u64);

impl DiskSpace for FixedDisk {
    fn available_bytes(&self, _path: &Path) -> intern_engine::EngineResult<u64> {
        Ok(self.0)
    }
}

fn manifest() -> ModelManifest {
    ModelManifest {
        schema_version: 2,
        model_id: "test-model".into(),
        served_model_name: "intern-local".into(),
        files: vec![
            ModelFile {
                name: "model.gguf".into(),
                role: ModelRole::Model,
                url: "https://example.invalid/model.gguf".into(),
                size: 11,
                sha256: "357e5d6fafa34d27360fec24b4326d3534905e33c6acdee60198fb078b7b79e5".into(),
            },
            ModelFile {
                name: "projector.gguf".into(),
                role: ModelRole::Projector,
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

fn analysis(
    probe: &intern_engine::setup::SemanticProbe,
    party: Option<&str>,
    date: Option<&str>,
) -> DocumentAnalysis {
    let digest = distill(&probe.document, DigestBudget::default());
    let proposal = ModelProposal {
        document_type: Some("Notice of Calibration".into()),
        document_date: date.map(str::to_owned),
        date_role: date.map(|_| DateRole::Notice),
        parties: party.map(str::to_owned).into_iter().collect(),
        party_relation: PartyRelation::To,
        description: format!(
            "Notice of calibration confirming that the local text model path is working for {}.",
            party.unwrap_or("this installation")
        ),
        confidence: 0.99,
        needs_review: false,
        evidence: Evidence {
            date: Some("Date of this Notice: January 2, 2024".into()),
            document_type: Some("NOTICE OF CALIBRATION".into()),
            parties: party
                .map(|value| format!("To: {value}"))
                .into_iter()
                .collect(),
        },
    };
    let outcome = validate(proposal, &digest);
    finish(outcome, &digest, "pdf", &[], AnalysisTelemetry::default())
}

#[test]
fn the_self_test_requires_the_model_to_read_the_calibration_document() {
    let probes = semantic_probes().unwrap();
    assert_eq!(probes.len(), 1);
    assert!(probes[0].document.page_image.is_none());

    validate_semantic_probe(
        &probes[0],
        &analysis(
            &probes[0],
            Some("Northstar Calibration Holdings LLC"),
            Some("2024-01-02"),
        ),
    )
    .unwrap();

    // Reading the document but not its defining date is not good enough.
    assert_eq!(
        validate_semantic_probe(
            &probes[0],
            &analysis(&probes[0], Some("Northstar Calibration Holdings LLC"), None)
        )
        .unwrap_err()
        .code(),
        ModelErrorCode::ModelSelfTestFailed
    );
    // Neither is producing a date without ever naming what the document says.
    assert_eq!(
        validate_semantic_probe(&probes[0], &analysis(&probes[0], None, Some("2024-01-02")))
            .unwrap_err()
            .code(),
        ModelErrorCode::ModelSelfTestFailed
    );
}
