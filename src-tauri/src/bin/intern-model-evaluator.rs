use std::{collections::HashMap, env, fs, path::Path, process, time::Instant};

use intern_app::{
    model::client::{DocumentInput, ModelClient, document_input_sha256},
    pipeline::WorkerBoundary,
    worker::SupervisedWorker,
};
use intern_core::{
    ModelProposal, ProposalStatus, ValidatedProposal, build_document_packet, validate_proposal,
};
use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

fn main() {
    match evaluate() {
        Ok(record) => println!("{record}"),
        Err(error) => {
            eprintln!("{error}");
            process::exit(1);
        }
    }
}

fn evaluate() -> Result<Value, String> {
    let arguments = arguments()?;
    let endpoint = required(&arguments, "endpoint")?;
    let api_key = required(&arguments, "api-key")?;
    let worker_path = required(&arguments, "worker")?;
    let fixture_path = required(&arguments, "fixture")?;
    let fixture_name = required(&arguments, "fixture-name")?;
    let expected_path = required(&arguments, "expected")?;

    let expected: Value = serde_json::from_slice(
        &fs::read(expected_path).map_err(|error| format!("cannot read gold corpus: {error}"))?,
    )
    .map_err(|error| format!("cannot parse gold corpus: {error}"))?;
    let fixture = expected["fixtures"]
        .as_array()
        .and_then(|fixtures| {
            fixtures
                .iter()
                .find(|fixture| fixture["file"].as_str() == Some(fixture_name))
        })
        .ok_or_else(|| format!("gold fixture is missing: {fixture_name}"))?;

    let started = Instant::now();
    let extraction_started = Instant::now();
    let worker = SupervisedWorker::new(worker_path);
    let parsed = worker.parse(
        &format!("qa-{}", fixture_name.replace(['/', '\\'], "-")),
        Path::new(fixture_path),
        &mut |_| {},
    );
    let extraction_ms = elapsed_ms(extraction_started);
    worker.stop();

    let parsed = match parsed {
        Ok(parsed) => {
            if fixture.get("expected_error").is_some() {
                return Ok(json!({
                    "status": "failed",
                    "model_invoked": false,
                    "response_valid": null,
                    "parser_error": null,
                    "model_error": null,
                    "readiness": null,
                    "input_packet_sha256": null,
                    "proposal_sha256": null,
                    "validation_sha256": null,
                    "proposal": null,
                    "validated_proposal": null,
                    "field_results": {"expected_error": false},
                    "unsupported_facts": [],
                    "timings_ms": {"extraction": extraction_ms, "inference": null, "total": elapsed_ms(started)},
                    "peak_rss_bytes": null
                }));
            }
            parsed
        }
        Err(error) => {
            let expected_error = fixture.get("expected_error").and_then(Value::as_str);
            let parser_error = error.code;
            let expected_error_matched = expected_error == Some(parser_error.as_str());
            return Ok(json!({
                "status": if expected_error_matched { "completed" } else { "failed" },
                "model_invoked": false,
                "response_valid": null,
                "parser_error": parser_error,
                "model_error": null,
                "readiness": "failed",
                "input_packet_sha256": null,
                "proposal_sha256": null,
                "validation_sha256": null,
                "proposal": null,
                "validated_proposal": null,
                "field_results": {"expected_error": expected_error_matched},
                "unsupported_facts": [],
                "timings_ms": {"extraction": extraction_ms, "inference": null, "total": elapsed_ms(started)},
                "peak_rss_bytes": null
            }));
        }
    };

    let packet = build_document_packet(parsed.extracted, parsed.image.is_some());
    let document = DocumentInput {
        text: packet.text.clone(),
        image: parsed.image,
    };
    let input_packet_sha256 = document_input_sha256(&document);
    let client = ModelClient::new(endpoint, api_key.to_owned())
        .map_err(|error| format!("cannot initialize production model client: {error}"))?;
    let inference_started = Instant::now();
    let proposal = match client.propose(&document) {
        Ok(proposal) => proposal,
        Err(error) => {
            return Ok(json!({
                "status": "failed",
                "model_invoked": true,
                "response_valid": false,
                "parser_error": null,
                "model_error": error.code().as_str(),
                "readiness": null,
                "input_packet_sha256": input_packet_sha256,
                "proposal_sha256": null,
                "validation_sha256": null,
                "proposal": null,
                "validated_proposal": null,
                "field_results": null,
                "unsupported_facts": [],
                "timings_ms": {"extraction": extraction_ms, "inference": elapsed_ms(inference_started), "total": elapsed_ms(started)},
                "peak_rss_bytes": null
            }));
        }
    };
    let inference_ms = elapsed_ms(inference_started);
    let outcome = validate_proposal(proposal.clone(), &packet);
    let readiness = match outcome.status {
        ProposalStatus::Ready => "ready",
        ProposalStatus::NeedsReview => "needs_review",
    };
    let proposal_json = serde_json::to_string(&proposal)
        .map_err(|error| format!("cannot serialize production proposal: {error}"))?;
    let proposal_sha256 = digest(proposal_json.as_bytes());
    let field_results = field_results(fixture, &outcome.proposal);
    let unsupported_facts = unsupported_facts(fixture, &proposal);
    let validation_sha256 = digest(
        &serde_json::to_vec(&ValidationBinding {
            input_packet_sha256: &input_packet_sha256,
            proposal_sha256: &proposal_sha256,
            validated_proposal: &outcome.proposal,
            readiness,
        })
        .map_err(|error| format!("cannot serialize validation evidence: {error}"))?,
    );

    Ok(json!({
        "status": "completed",
        "model_invoked": true,
        "response_valid": true,
        "parser_error": null,
        "model_error": null,
        "readiness": readiness,
        "input_packet_sha256": input_packet_sha256,
        "proposal_sha256": proposal_sha256,
        "validation_sha256": validation_sha256,
        "proposal": proposal,
        "validated_proposal": outcome.proposal,
        "field_results": field_results,
        "unsupported_facts": unsupported_facts,
        "timings_ms": {"extraction": extraction_ms, "inference": inference_ms, "total": elapsed_ms(started)},
        "peak_rss_bytes": null
    }))
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

fn field_results(fixture: &Value, proposal: &intern_core::ValidatedProposal) -> Value {
    let mut results = Map::new();
    for field in ["document_date", "document_type", "subject", "parties"] {
        let Some(expected) = fixture.get(field) else {
            continue;
        };
        let correct = match field {
            "document_date" => expected.as_str() == proposal.document_date.as_deref(),
            "document_type" => expected.as_str() == proposal.document_type.as_deref(),
            "subject" => expected.as_str() == proposal.filename_subject.as_deref(),
            "parties" => same_strings(expected, &proposal.parties),
            _ => false,
        };
        results.insert(field.to_owned(), Value::Bool(correct));
    }
    if let Some(facts) = fixture
        .get("acceptable_description_facts")
        .and_then(Value::as_array)
    {
        let description = proposal.description.to_lowercase();
        let supported = !facts.is_empty()
            && facts
                .iter()
                .filter_map(Value::as_str)
                .all(|fact| description.contains(&fact.to_lowercase()));
        results.insert("description".to_owned(), Value::Bool(supported));
    }
    Value::Object(results)
}

#[derive(Serialize)]
struct ValidationBinding<'a> {
    input_packet_sha256: &'a str,
    proposal_sha256: &'a str,
    validated_proposal: &'a ValidatedProposal,
    readiness: &'a str,
}

fn unsupported_facts(fixture: &Value, proposal: &ModelProposal) -> Value {
    let mut facts = Vec::new();
    if let Some(date) = proposal.document_date.as_deref() {
        if fixture.get("document_date").and_then(Value::as_str) != Some(date) {
            facts.push(json!({"field": "document_date", "value": date}));
        }
    }
    let supported_parties = fixture
        .get("parties")
        .and_then(Value::as_array)
        .map(|parties| parties.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    for party in &proposal.parties {
        if !supported_parties.contains(&party.as_str()) {
            facts.push(json!({"field": "parties", "value": party}));
        }
    }
    Value::Array(facts)
}

fn same_strings(expected: &Value, actual: &[String]) -> bool {
    let Some(expected) = expected.as_array() else {
        return false;
    };
    let mut expected = expected
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    let mut actual = actual.iter().map(String::as_str).collect::<Vec<_>>();
    expected.sort_unstable();
    actual.sort_unstable();
    expected == actual
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parties_are_compared_without_order_but_without_fuzzy_matching() {
        assert!(same_strings(
            &json!(["Acme", "Mira"]),
            &["Mira".into(), "Acme".into()]
        ));
        assert!(!same_strings(&json!(["Acme"]), &["Acme Corp".into()]));
    }

    #[test]
    fn packet_hash_includes_the_production_vision_input() {
        let image = intern_app::model::client::ImageInput {
            media_type: "image/png".into(),
            bytes: vec![1, 2, 3],
        };
        assert_ne!(
            document_input_sha256(&DocumentInput {
                text: "text".into(),
                image: None,
            }),
            document_input_sha256(&DocumentInput {
                text: "text".into(),
                image: Some(image),
            })
        );
    }
}
