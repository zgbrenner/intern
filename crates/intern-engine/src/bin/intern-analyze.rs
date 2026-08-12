//! Headless entry point to the document-understanding engine.
//!
//! ```text
//! intern-analyze --file contract.pdf --worker intern-worker.exe \
//!                --endpoint http://127.0.0.1:8080/v1/chat/completions \
//!                --api-key KEY --model-id intern-local
//! ```
//!
//! Prints one JSON object: the proposed filename, the description, the
//! evidence, the review reasons, and local timings. This is the same code path
//! the desktop app uses, so a watched folder, a script, or a future connector
//! gets identical results without linking the UI.
//!
//! `--pipeline legacy` runs the pre-redesign head/tail window and prompt
//! instead, which is how the two are compared on one corpus.

use std::{collections::HashMap, env, fs, path::Path, process, time::Instant};

use intern_engine::{
    DigestBudget, DocumentExtractor, DocumentSource, Engine, ModelClient, ModelRequest,
    SupervisedWorker,
    distill::distill,
    domain::{AnalysisTelemetry, PageOrigin, SourcePage},
    engine::finish,
    legacy::{
        LEGACY_GRAMMAR, LegacyProposal, legacy_digest, legacy_filename, legacy_prompt,
        legacy_validate,
    },
    prompt::SYSTEM_INSTRUCTION,
    validate,
};
use serde_json::{Value, json};

fn main() {
    match run() {
        Ok(value) => println!("{value}"),
        Err(message) => {
            eprintln!("{message}");
            process::exit(1);
        }
    }
}

fn run() -> Result<Value, String> {
    let arguments = parse_arguments()?;
    let path = required(&arguments, "file")?;
    let extension = Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_owned();

    let extraction_started = Instant::now();
    let source = load_source(&arguments, path)?;
    let extraction_millis = millis(extraction_started);

    let budget = arguments
        .get("budget")
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|_| "--budget must be a character count".to_owned())
        })
        .transpose()?
        .map_or_else(DigestBudget::default, |max_characters| DigestBudget {
            passthrough_characters: max_characters,
            max_characters,
        });

    if arguments.contains_key("distill-only") {
        let digest = distill(&source, budget);
        return Ok(json!({
            "mode": "distill-only",
            "pages": digest.page_count,
            "sourceCharacters": digest.source_characters,
            "digestCharacters": digest.digest_characters,
            "compressionRatio": digest.compression_ratio(),
            "compressed": digest.compressed,
            "outline": digest.outline,
            "digest": digest.text,
        }));
    }

    let endpoint = required(&arguments, "endpoint")?;
    let api_key = required(&arguments, "api-key")?;
    let model_id = arguments
        .get("model-id")
        .map(String::as_str)
        .unwrap_or("intern-local");

    match arguments.get("pipeline").map(String::as_str) {
        Some("legacy") => run_legacy(
            &source,
            &extension,
            endpoint,
            api_key,
            model_id,
            extraction_millis,
        ),
        Some("new") | None => run_current(
            &source,
            &extension,
            endpoint,
            api_key,
            model_id,
            budget,
            extraction_millis,
        ),
        Some(other) => Err(format!("unknown --pipeline {other}")),
    }
}

fn run_current(
    source: &DocumentSource,
    extension: &str,
    endpoint: &str,
    api_key: &str,
    model_id: &str,
    budget: DigestBudget,
    extraction_millis: u64,
) -> Result<Value, String> {
    let client = ModelClient::new(endpoint, api_key, model_id)
        .map_err(|error| format!("model client: {error}"))?;
    let engine = Engine::new(client).with_budget(budget);
    let distill_started = Instant::now();
    let digest = engine.distill(source);
    let distill_micros = u64::try_from(distill_started.elapsed().as_micros()).unwrap_or(u64::MAX);
    let request = ModelRequest::from_digest(&digest, None);
    match engine.analyze_digest(source, &digest, distill_micros, extension, &[]) {
        Ok(analysis) => Ok(json!({
            "pipeline": "new",
            "ok": true,
            "filename": analysis.filename,
            "description": analysis.description,
            "status": analysis.status,
            "reviewReasons": analysis.review_reasons,
            "proposal": analysis.proposal,
            "telemetry": analysis.telemetry,
            "extractionMillis": extraction_millis,
            "requestSha256": request.sha256(),
            "promptCharacters": request.prompt.chars().count(),
        })),
        Err(error) => Ok(json!({
            "pipeline": "new",
            "ok": false,
            "error": error.code().as_str(),
            "extractionMillis": extraction_millis,
        })),
    }
}

fn run_legacy(
    source: &DocumentSource,
    extension: &str,
    endpoint: &str,
    api_key: &str,
    model_id: &str,
    extraction_millis: u64,
) -> Result<Value, String> {
    let digest = legacy_digest(source);
    let prompt = legacy_prompt(&digest);
    let started = Instant::now();
    let raw = post_legacy(endpoint, api_key, model_id, &prompt)?;
    let inference_millis = millis(started);
    let Some(object) = intern_engine::client::extract_json_object(&raw) else {
        return Ok(json!({
            "pipeline": "legacy",
            "ok": false,
            "error": "MODEL_RESPONSE_INVALID",
            "extractionMillis": extraction_millis,
        }));
    };
    let Ok(candidate) = serde_json::from_str::<LegacyProposal>(object) else {
        return Ok(json!({
            "pipeline": "legacy",
            "ok": false,
            "error": "MODEL_RESPONSE_INVALID",
            "extractionMillis": extraction_millis,
        }));
    };
    let outcome = legacy_validate(&candidate, &digest);
    Ok(json!({
        "pipeline": "legacy",
        "ok": true,
        "filename": legacy_filename(&outcome, extension),
        "description": outcome.description,
        "status": if outcome.ready { "ready" } else { "needs_review" },
        "reviewReasons": outcome.reasons,
        "proposal": {
            "documentType": outcome.document_type,
            "documentDate": outcome.document_date,
            "filenameSubject": outcome.filename_subject,
            "parties": outcome.parties,
            "confidence": candidate.confidence,
        },
        "telemetry": {
            "sourceCharacters": digest.source_characters,
            "digestCharacters": digest.digest_characters,
            "compressionRatio": digest.compression_ratio(),
            "inferenceMillis": inference_millis,
        },
        "extractionMillis": extraction_millis,
        "promptCharacters": prompt.chars().count(),
    }))
}

fn post_legacy(
    endpoint: &str,
    api_key: &str,
    model_id: &str,
    prompt: &str,
) -> Result<String, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10 * 60))
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

fn load_source(arguments: &HashMap<String, String>, path: &str) -> Result<DocumentSource, String> {
    if let Some(worker) = arguments.get("worker") {
        let worker = SupervisedWorker::new(worker);
        let request_id = format!("cli-{}", path.replace(['/', '\\', ':'], "-"));
        let source = worker
            .extract(&request_id, Path::new(path), &mut |_| {})
            .map_err(|error| format!("extraction failed: {}", error.code));
        worker.stop();
        return source;
    }
    let text = fs::read_to_string(path).map_err(|error| format!("cannot read {path}: {error}"))?;
    Ok(DocumentSource::from_pages(vec![SourcePage::new(
        1,
        text,
        PageOrigin::PlainText,
    )]))
}

/// Re-validates an already-collected proposal without a model. Used by the
/// scoring harness to replay stored replies.
#[allow(dead_code)]
fn replay(
    source: &DocumentSource,
    proposal: intern_engine::ModelProposal,
    extension: &str,
) -> intern_engine::DocumentAnalysis {
    let digest = distill(source, DigestBudget::default());
    let outcome = validate(proposal, &digest);
    finish(
        outcome,
        &digest,
        extension,
        &[],
        AnalysisTelemetry::default(),
    )
}

fn parse_arguments() -> Result<HashMap<String, String>, String> {
    let mut values = HashMap::new();
    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        let key = argument
            .strip_prefix("--")
            .ok_or_else(|| format!("unexpected argument: {argument}"))?;
        if key == "distill-only" {
            values.insert(key.to_owned(), String::new());
            continue;
        }
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
