//! Corpus evaluation for the document-understanding engine.
//!
//! Runs every gold fixture through the real extraction, distillation, local
//! inference, validation, and naming path and scores the result against the
//! reviewed answers in `fixtures/expected.json`.
//!
//! ```text
//! intern-evaluate --fixtures fixtures/generated --expected fixtures/expected.json \
//!                 --worker intern-worker.exe --endpoint http://127.0.0.1:8080/v1/chat/completions \
//!                 --api-key KEY --model-id intern-local --pipeline new --output report.json
//! ```
//!
//! `--pipeline legacy` runs the pre-redesign head/tail window and prompt over
//! the identical corpus, which is how the redesign is shown to be an
//! improvement rather than asserted to be one.

use std::{collections::HashMap, env, fs, path::Path, process, time::Instant};

use intern_engine::{
    DigestBudget, DocumentExtractor, DocumentSource, Engine, ModelClient, SupervisedWorker,
    domain::ProposalStatus,
    legacy::{
        LEGACY_GRAMMAR, LegacyProposal, legacy_digest, legacy_filename, legacy_prompt,
        legacy_validate,
    },
    prompt::SYSTEM_INSTRUCTION,
};
use serde_json::{Map, Value, json};

fn main() {
    match run() {
        Ok(report) => {
            let rendered = serde_json::to_string_pretty(&report).unwrap_or_default();
            println!("{rendered}");
        }
        Err(message) => {
            eprintln!("{message}");
            process::exit(1);
        }
    }
}

fn run() -> Result<Value, String> {
    let arguments = parse_arguments()?;
    let fixtures_root = required(&arguments, "fixtures")?;
    let expected_path = required(&arguments, "expected")?;
    let endpoint = required(&arguments, "endpoint")?;
    let api_key = required(&arguments, "api-key")?;
    let worker_path = required(&arguments, "worker")?;
    let model_id = arguments
        .get("model-id")
        .map(String::as_str)
        .unwrap_or("intern-local");
    let pipeline = arguments
        .get("pipeline")
        .map(String::as_str)
        .unwrap_or("new")
        .to_owned();
    let budget = arguments
        .get("budget")
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|_| "--budget must be a number".to_owned())
        })
        .transpose()?
        .map_or_else(DigestBudget::default, |max_characters| DigestBudget {
            passthrough_characters: max_characters,
            max_characters,
        });

    let expected: Value = serde_json::from_slice(
        &fs::read(expected_path).map_err(|error| format!("cannot read gold corpus: {error}"))?,
    )
    .map_err(|error| format!("cannot parse gold corpus: {error}"))?;
    let fixtures = expected["fixtures"]
        .as_array()
        .ok_or("gold corpus has no fixtures")?;

    let client = ModelClient::new(endpoint, api_key, model_id)
        .map_err(|error| format!("model client: {error}"))?;
    let engine = Engine::new(client).with_budget(budget);
    let worker = SupervisedWorker::new(worker_path);

    let mut records = Vec::new();
    for fixture in fixtures {
        let name = fixture["file"].as_str().unwrap_or_default();
        let path = Path::new(fixtures_root).join(name);
        eprintln!("evaluating {name}");
        records.push(evaluate_one(
            fixture, &path, &worker, &engine, &pipeline, endpoint, api_key, model_id,
        ));
    }
    worker.stop();

    Ok(json!({
        "schema_version": 2,
        "pipeline": pipeline,
        "model_id": model_id,
        "budget_characters": budget.max_characters,
        "summary": summarize(&records),
        "records": records,
    }))
}

#[allow(clippy::too_many_arguments)]
fn evaluate_one(
    fixture: &Value,
    path: &Path,
    worker: &SupervisedWorker,
    engine: &Engine,
    pipeline: &str,
    endpoint: &str,
    api_key: &str,
    model_id: &str,
) -> Value {
    let name = fixture["file"].as_str().unwrap_or_default();
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let expected_error = fixture.get("expected_error").and_then(Value::as_str);

    let extraction_started = Instant::now();
    let request_id = format!("eval-{}", name.replace(['/', '\\', '.'], "-"));
    let extracted = worker.extract(&request_id, path, &mut |_| {});
    let extraction_millis = millis(extraction_started);

    let source: DocumentSource = match extracted {
        Ok(source) => {
            if expected_error.is_some() {
                return json!({
                    "file": name, "status": "unexpected_success", "readiness": null,
                    "extraction_millis": extraction_millis,
                });
            }
            source
        }
        Err(error) => {
            let matched = expected_error == Some(error.code.as_str());
            return json!({
                "file": name,
                "status": if matched { "expected_error" } else { "extraction_failed" },
                "parser_error": error.code,
                "readiness": "failed",
                "extraction_millis": extraction_millis,
                "scores": {"expected_error": matched},
            });
        }
    };

    if pipeline == "legacy" {
        return legacy_record(
            fixture,
            &source,
            extension,
            extraction_millis,
            endpoint,
            api_key,
            model_id,
        );
    }

    let distill_started = Instant::now();
    let digest = engine.distill(&source);
    let distill_micros = u64::try_from(distill_started.elapsed().as_micros()).unwrap_or(u64::MAX);
    match engine.analyze_digest(&source, &digest, distill_micros, extension, &[]) {
        Ok(analysis) => {
            let scores = score(
                fixture,
                ScoreInput {
                    document_date: analysis.proposal.document_date.as_deref(),
                    document_type: analysis.proposal.document_type.as_deref(),
                    parties: &analysis.proposal.parties,
                    description: &analysis.description,
                    ready: analysis.status == ProposalStatus::Ready,
                },
            );
            json!({
                "file": name,
                "status": "completed",
                "filename": analysis.filename,
                "description": analysis.description,
                "readiness": if analysis.status == ProposalStatus::Ready { "ready" } else { "needs_review" },
                "review_reasons": analysis.review_reasons,
                "proposal": analysis.proposal,
                "scores": scores,
                "extraction_millis": extraction_millis,
                "telemetry": analysis.telemetry,
                "source_characters": digest.source_characters,
                "digest_characters": digest.digest_characters,
                "pages": digest.page_count,
            })
        }
        Err(error) => json!({
            "file": name,
            "status": "model_failed",
            "error": error.code().as_str(),
            "readiness": null,
            "extraction_millis": extraction_millis,
        }),
    }
}

#[allow(clippy::too_many_arguments)]
fn legacy_record(
    fixture: &Value,
    source: &DocumentSource,
    extension: &str,
    extraction_millis: u64,
    endpoint: &str,
    api_key: &str,
    model_id: &str,
) -> Value {
    let name = fixture["file"].as_str().unwrap_or_default();
    let digest = legacy_digest(source);
    let prompt = legacy_prompt(&digest);
    let started = Instant::now();
    let raw = match post_legacy(endpoint, api_key, model_id, &prompt) {
        Ok(raw) => raw,
        Err(error) => {
            return json!({"file": name, "status": "model_failed", "error": error, "readiness": null});
        }
    };
    let inference_millis = millis(started);
    let candidate = intern_engine::client::extract_json_object(&raw)
        .and_then(|object| serde_json::from_str::<LegacyProposal>(object).ok());
    let Some(candidate) = candidate else {
        return json!({"file": name, "status": "model_failed", "error": "MODEL_RESPONSE_INVALID", "readiness": null});
    };
    let outcome = legacy_validate(&candidate, &digest);
    let scores = score(
        fixture,
        ScoreInput {
            document_date: outcome.document_date.as_deref(),
            document_type: outcome.document_type.as_deref(),
            parties: &outcome.parties,
            description: &outcome.description,
            ready: outcome.ready,
        },
    );
    json!({
        "file": name,
        "status": "completed",
        "filename": legacy_filename(&outcome, extension),
        "description": outcome.description,
        "readiness": if outcome.ready { "ready" } else { "needs_review" },
        "review_reasons": outcome.reasons,
        "scores": scores,
        "extraction_millis": extraction_millis,
        "telemetry": {"inferenceMillis": inference_millis, "sourceCharacters": digest.source_characters, "digestCharacters": digest.digest_characters},
        "source_characters": digest.source_characters,
        "digest_characters": digest.digest_characters,
        "pages": digest.page_count,
    })
}

fn post_legacy(
    endpoint: &str,
    api_key: &str,
    model_id: &str,
    prompt: &str,
) -> Result<String, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15 * 60))
        .no_proxy()
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .post(endpoint)
        .bearer_auth(api_key)
        .json(&json!({
            "model": model_id,
            "messages": [
                {"role": "system", "content": SYSTEM_INSTRUCTION},
                {"role": "user", "content": prompt}
            ],
            "stream": false,
            "temperature": 0,
            "max_tokens": 2048,
            "grammar": LEGACY_GRAMMAR,
            "cache_prompt": true,
            "chat_template_kwargs": {"enable_thinking": false}
        }))
        .send()
        .map_err(|error| error.to_string())?;
    let value: Value = response.json().map_err(|error| error.to_string())?;
    value["choices"][0]["message"]["content"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "model reply had no content".to_owned())
}

struct ScoreInput<'a> {
    document_date: Option<&'a str>,
    document_type: Option<&'a str>,
    parties: &'a [String],
    description: &'a str,
    ready: bool,
}

fn score(fixture: &Value, actual: ScoreInput<'_>) -> Value {
    let mut scores = Map::new();
    let gold_date = fixture.get("document_date").and_then(Value::as_str);
    let acceptable = strings(fixture, "acceptable_dates");
    let forbidden_dates = strings(fixture, "forbidden_dates");
    if gold_date.is_some() || !acceptable.is_empty() {
        let correct = actual.document_date.is_some_and(|date| {
            gold_date == Some(date) || acceptable.iter().any(|value| value == date)
        });
        scores.insert("date_correct".into(), Value::Bool(correct));
        scores.insert(
            "date_exact".into(),
            Value::Bool(actual.document_date == gold_date),
        );
        scores.insert(
            "date_forbidden".into(),
            Value::Bool(
                actual
                    .document_date
                    .is_some_and(|date| forbidden_dates.iter().any(|value| value == date)),
            ),
        );
        scores.insert(
            "date_present".into(),
            Value::Bool(actual.document_date.is_some()),
        );
    }
    if let Some(gold_type) = fixture.get("document_type").and_then(Value::as_str) {
        scores.insert(
            "type_correct".into(),
            Value::Bool(
                actual
                    .document_type
                    .is_some_and(|value| type_matches(gold_type, value)),
            ),
        );
        scores.insert(
            "type_present".into(),
            Value::Bool(actual.document_type.is_some()),
        );
    }
    let gold_parties = strings(fixture, "parties");
    let forbidden_parties = strings(fixture, "forbidden_parties");
    if fixture.get("parties").is_some() {
        let matched = gold_parties
            .iter()
            .filter(|gold| {
                actual
                    .parties
                    .iter()
                    .any(|value| party_matches(gold, value))
            })
            .count();
        let spurious = actual
            .parties
            .iter()
            .filter(|value| !gold_parties.iter().any(|gold| party_matches(gold, value)))
            .count();
        scores.insert(
            "parties_correct".into(),
            Value::Bool(matched == gold_parties.len() && spurious == 0),
        );
        scores.insert("parties_matched".into(), json!(matched));
        scores.insert("parties_expected".into(), json!(gold_parties.len()));
        scores.insert("parties_spurious".into(), json!(spurious));
        scores.insert(
            "party_forbidden".into(),
            Value::Bool(actual.parties.iter().any(|value| {
                forbidden_parties
                    .iter()
                    .any(|gold| party_matches(gold, value))
            })),
        );
    }
    let facts = strings(fixture, "acceptable_description_facts");
    if !facts.is_empty() {
        let lowered = actual.description.to_lowercase();
        scores.insert(
            "description_covers_facts".into(),
            Value::Bool(
                facts
                    .iter()
                    .all(|fact| lowered.contains(&fact.to_lowercase())),
            ),
        );
    }
    scores.insert(
        "description_specific".into(),
        Value::Bool(actual.description.split_whitespace().count() >= 8),
    );
    if let Some(expected) = fixture.get("expected_readiness").and_then(Value::as_str) {
        let readiness = if actual.ready {
            "ready"
        } else {
            "needs_review"
        };
        scores.insert("readiness_match".into(), Value::Bool(expected == readiness));
    }
    scores.insert("ready".into(), Value::Bool(actual.ready));
    Value::Object(scores)
}

/// A predicted type counts as correct when it carries every meaningful word of
/// the reviewed type. "Statement of Work No. 4" passes for "Statement of Work";
/// "Employment Termination" does not pass for "Notice of Termination".
fn type_matches(gold: &str, actual: &str) -> bool {
    let actual = actual.to_lowercase();
    gold.to_lowercase()
        .split_whitespace()
        .filter(|word| word.len() > 2 && !matches!(*word, "the" | "and" | "for" | "with"))
        .all(|word| actual.contains(word))
}

/// Party names match when one contains the other after dropping punctuation, so
/// "Vistage Worldwide, Inc." and "Vistage Worldwide Inc" are the same party.
fn party_matches(gold: &str, actual: &str) -> bool {
    let normalize = |value: &str| {
        value
            .chars()
            .filter(|character| character.is_alphanumeric() || character.is_whitespace())
            .flat_map(char::to_lowercase)
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let gold = normalize(gold);
    let actual = normalize(actual);
    !gold.is_empty() && !actual.is_empty() && (gold.contains(&actual) || actual.contains(&gold))
}

fn summarize(records: &[Value]) -> Value {
    let mut totals: HashMap<&str, (usize, usize)> = HashMap::new();
    let mut inference = Vec::new();
    let mut total_time = Vec::new();
    let mut ready = 0;
    let mut scored = 0;
    for record in records {
        let Some(scores) = record.get("scores").and_then(Value::as_object) else {
            continue;
        };
        if record["status"] == json!("completed") {
            scored += 1;
            if scores.get("ready") == Some(&Value::Bool(true)) {
                ready += 1;
            }
            let inference_millis = record["telemetry"]["inferenceMillis"].as_u64().unwrap_or(0);
            let extraction_millis = record["extraction_millis"].as_u64().unwrap_or(0);
            inference.push(inference_millis);
            total_time.push(inference_millis + extraction_millis);
        }
        for (key, value) in scores {
            if let Some(flag) = value.as_bool() {
                let entry = totals
                    .entry(Box::leak(key.clone().into_boxed_str()))
                    .or_insert((0, 0));
                entry.1 += 1;
                if flag {
                    entry.0 += 1;
                }
            }
        }
    }
    let mut summary = Map::new();
    let mut keys = totals.keys().copied().collect::<Vec<_>>();
    keys.sort_unstable();
    for key in keys {
        let (correct, total) = totals[key];
        summary.insert(
            key.to_owned(),
            json!({"correct": correct, "total": total, "rate": correct as f64 / total.max(1) as f64}),
        );
    }
    summary.insert("evaluated".into(), json!(scored));
    summary.insert(
        "review_rate".into(),
        json!(1.0 - (ready as f64 / scored.max(1) as f64)),
    );
    summary.insert("inference_millis".into(), percentiles(&inference));
    summary.insert("total_millis".into(), percentiles(&total_time));
    Value::Object(summary)
}

fn percentiles(values: &[u64]) -> Value {
    if values.is_empty() {
        return json!({"count": 0});
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    json!({
        "count": sorted.len(),
        "min": sorted[0],
        "median": sorted[sorted.len() / 2],
        "max": sorted[sorted.len() - 1],
        "mean": sorted.iter().sum::<u64>() / sorted.len() as u64,
    })
}

fn strings(fixture: &Value, key: &str) -> Vec<String> {
    fixture
        .get(key)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_arguments() -> Result<HashMap<String, String>, String> {
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

fn millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_more_specific_type_still_matches_the_reviewed_type() {
        assert!(type_matches("Statement of Work", "Statement of Work No. 4"));
        assert!(type_matches("Invoice", "Invoice"));
        assert!(!type_matches(
            "Notice of Termination",
            "Employment Termination"
        ));
        assert!(!type_matches("Settlement Agreement", "Agreement"));
    }

    #[test]
    fn party_names_match_through_punctuation_and_suffixes() {
        assert!(party_matches(
            "Vistage Worldwide, Inc.",
            "Vistage Worldwide Inc"
        ));
        assert!(party_matches("John Smith", "John Smith"));
        assert!(!party_matches("John Smith", "Marcus Reyes"));
    }

    #[test]
    fn distilling_is_available_to_the_harness_without_a_model() {
        let source = intern_engine::source_from_text("NOTICE\n\nDated March 3, 2026.");
        assert!(
            intern_engine::distill(&source, DigestBudget::default())
                .text
                .contains("NOTICE")
        );
    }
}
