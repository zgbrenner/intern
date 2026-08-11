use std::{collections::HashMap, env, fs, path::Path, process};

use intern_app::{
    model::client::{DocumentInput, document_input_sha256},
    pipeline::WorkerBoundary,
    worker::SupervisedWorker,
};
use intern_core::{
    ModelProposal, ProposalStatus, ValidatedProposal, build_document_packet, validate_proposal,
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn main() {
    match verify() {
        Ok(count) => println!("{}", json!({"status": "verified", "records": count})),
        Err(error) => {
            eprintln!("{error}");
            process::exit(1);
        }
    }
}

fn verify() -> Result<usize, String> {
    let arguments = arguments()?;
    let worker_path = required(&arguments, "worker")?;
    let fixture_directory = Path::new(required(&arguments, "fixtures")?);
    let report_path = required(&arguments, "report")?;
    let report: Value = serde_json::from_slice(
        &fs::read(report_path).map_err(|error| format!("cannot read model evidence: {error}"))?,
    )
    .map_err(|error| format!("cannot parse model evidence: {error}"))?;
    if report["status"] != "completed" {
        return Err("production evidence replay requires a completed report".into());
    }
    let records = report["records"]
        .as_object()
        .ok_or("model evidence records are missing")?;

    for (fixture_name, record) in records {
        if Path::new(fixture_name).is_absolute()
            || Path::new(fixture_name)
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(format!("unsafe fixture evidence path: {fixture_name}"));
        }
        let fixture_path = fixture_directory.join(fixture_name);
        let worker = SupervisedWorker::new(worker_path);
        let parsed = worker.parse(
            &format!("evidence-{}", fixture_name.replace(['/', '\\'], "-")),
            &fixture_path,
            &mut |_| {},
        );
        worker.stop();

        match parsed {
            Ok(parsed) => {
                let image_included = parsed.image.is_some();
                let packet = build_document_packet(parsed.extracted, image_included);
                let document = DocumentInput {
                    text: packet.text.clone(),
                    image: parsed.image,
                };
                let input_digest = document_input_sha256(&document);
                for variant in ["q4", "q8"] {
                    verify_model_result(
                        fixture_name,
                        variant,
                        &record[variant],
                        &packet,
                        &input_digest,
                    )?;
                }
            }
            Err(error) => {
                for variant in ["q4", "q8"] {
                    let result = &record[variant];
                    if result["model_invoked"] != false
                        || result["parser_error"].as_str() != Some(error.code.as_str())
                        || !result["proposal"].is_null()
                        || !result["validated_proposal"].is_null()
                    {
                        return Err(format!(
                            "{variant} parser evidence does not replay for {fixture_name}"
                        ));
                    }
                }
            }
        }
    }
    Ok(records.len())
}

fn verify_model_result(
    fixture_name: &str,
    variant: &str,
    result: &Value,
    packet: &intern_core::DocumentPacket,
    input_digest: &str,
) -> Result<(), String> {
    if result["model_invoked"] != true
        || result["input_packet_sha256"].as_str() != Some(input_digest)
    {
        return Err(format!(
            "{variant} production input evidence does not replay for {fixture_name}"
        ));
    }
    let proposal: ModelProposal = serde_json::from_value(result["proposal"].clone()).map_err(
        |error| format!("{variant} raw proposal is invalid for {fixture_name}: {error}"),
    )?;
    let outcome = validate_proposal(proposal, packet);
    let readiness = match outcome.status {
        ProposalStatus::Ready => "ready",
        ProposalStatus::NeedsReview => "needs_review",
    };
    let validated = serde_json::to_value(&outcome.proposal)
        .map_err(|error| format!("cannot serialize replayed production proposal: {error}"))?;
    if result["validated_proposal"] != validated || result["readiness"] != readiness {
        return Err(format!(
            "{variant} production validation outcome does not replay for {fixture_name}"
        ));
    }
    let proposal_sha256 = result["proposal_sha256"]
        .as_str()
        .ok_or_else(|| format!("{variant} proposal hash is missing for {fixture_name}"))?;
    let expected_validation_sha256 = digest(
        &serde_json::to_vec(&ValidationBinding {
            input_packet_sha256: input_digest,
            proposal_sha256,
            validated_proposal: &outcome.proposal,
            readiness,
        })
        .map_err(|error| format!("cannot serialize replayed validation evidence: {error}"))?,
    );
    if result["validation_sha256"].as_str() != Some(expected_validation_sha256.as_str()) {
        return Err(format!(
            "{variant} production validation hash does not replay for {fixture_name}"
        ));
    }
    Ok(())
}

#[derive(Serialize)]
struct ValidationBinding<'a> {
    input_packet_sha256: &'a str,
    proposal_sha256: &'a str,
    validated_proposal: &'a ValidatedProposal,
    readiness: &'a str,
}

fn arguments() -> Result<HashMap<String, String>, String> {
    let mut values = HashMap::new();
    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        let key = argument
            .strip_prefix("--")
            .ok_or_else(|| format!("unexpected argument: {argument}"))?;
        let value = arguments
            .next()
            .ok_or_else(|| format!("missing value for --{key}"))?;
        values.insert(key.to_owned(), value);
    }
    Ok(values)
}

fn required<'a>(arguments: &'a HashMap<String, String>, key: &str) -> Result<&'a str, String> {
    arguments
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| format!("missing --{key}"))
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
