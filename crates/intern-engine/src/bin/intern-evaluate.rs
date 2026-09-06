//! Corpus evaluation for the document-understanding engine.
//!
//! Runs every gold fixture through the real extraction, distillation,
//! inference, validation, and naming path and scores the result against the
//! reviewed answers in `fixtures/expected.json`.
//!
//! ```text
//! intern-evaluate --fixtures fixtures/generated --expected fixtures/expected.json \
//!                 --worker intern-worker.exe --endpoint http://127.0.0.1:8080/v1/chat/completions \
//!                 --api-key KEY --model-id intern-local --output report.json
//! ```
//!
//! **Record and replay.** A live run is the only way to learn what the model
//! says, and it needs a machine with the worker, the runtime, and the model.
//! `--record PATH` keeps what such a run saw: the extracted text of every
//! fixture and the model's reply to every prompt, keyed by a hash of the
//! prompt. `--replay PATH` then scores the corpus from that file alone - no
//! worker, no model, seconds rather than most of an hour - so a change to
//! distillation, validation, evidence, or naming is measured in CI on every
//! push. A change to the prompt, or to the text the model would have read,
//! changes the hash, and replay refuses the stale reply rather than scoring
//! the wrong model output as if it were the right one: re-record, and commit
//! the recording with the change. `--allow-stale` scores anyway, marked, for
//! local iteration.
//!
//! **Baseline.** `--baseline PATH` compares every fixture's scores with a
//! committed baseline and exits 2 when a reviewed answer that used to be right
//! is now wrong; `--write-baseline PATH` writes the current scores as the new
//! baseline. Improvements are reported, never required.
//!
//! `--pipeline legacy` runs the pre-redesign head/tail window and prompt over
//! the identical corpus, which is how the redesign is shown to be an
//! improvement rather than asserted to be one. Legacy runs are live only.

use std::{
    collections::{BTreeMap, HashMap},
    env, fs,
    path::Path,
    process,
    sync::{Arc, Mutex},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use intern_engine::{
    DigestBudget, DocumentExtractor, DocumentSource, Engine, EngineResult, ModelClient,
    ModelProposal, ModelRequest, PageImage, Proposer, SupervisedWorker,
    distill::DocumentDigest,
    domain::{DocumentAnalysis, ProposalStatus},
    legacy::{
        LEGACY_GRAMMAR, LegacyProposal, legacy_digest, legacy_filename, legacy_prompt,
        legacy_validate,
    },
    prompt::SYSTEM_INSTRUCTION,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

/// Exit status when the corpus scored, but worse than the baseline, or with
/// fixtures replay could not score.
const EXIT_REGRESSED: i32 = 2;

fn main() {
    match run() {
        Ok(exit) => process::exit(exit),
        Err(message) => {
            eprintln!("{message}");
            process::exit(1);
        }
    }
}

fn run() -> Result<i32, String> {
    let arguments = parse_arguments()?;
    let fixtures_root = required(&arguments, "fixtures")?;
    let expected_path = required(&arguments, "expected")?;
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
    let replay_path = arguments.get("replay");
    let record_path = arguments.get("record");
    let allow_stale = arguments.contains_key("allow-stale");
    if replay_path.is_some() && record_path.is_some() {
        return Err("choose --record or --replay, not both".to_owned());
    }
    if record_path.is_some() && pipeline == "legacy" {
        return Err("legacy runs cannot be recorded".to_owned());
    }

    let expected: Value = serde_json::from_slice(
        &fs::read(expected_path).map_err(|error| format!("cannot read gold corpus: {error}"))?,
    )
    .map_err(|error| format!("cannot parse gold corpus: {error}"))?;
    let fixtures = expected["fixtures"]
        .as_array()
        .ok_or("gold corpus has no fixtures")?;

    let (mode, records) = match replay_path {
        Some(path) => {
            let recording = Recording::load(path)?;
            let records = fixtures
                .iter()
                .map(|fixture| {
                    let name = fixture["file"].as_str().unwrap_or_default();
                    replay_one(
                        fixture,
                        &Path::new(fixtures_root).join(name),
                        &recording,
                        budget,
                        allow_stale,
                    )
                })
                .collect::<Vec<_>>();
            ("replay", records)
        }
        None => {
            let endpoint = required(&arguments, "endpoint")?;
            let api_key = required(&arguments, "api-key")?;
            let worker_path = required(&arguments, "worker")?;
            let client = ModelClient::new(endpoint, api_key, model_id)
                .map_err(|error| format!("model client: {error}"))?;
            let recorder = record_path.map(|_| Recorder::new());
            let engine = match &recorder {
                Some(recorder) => Engine::with_proposer(Box::new(recorder.proposer(client))),
                None => Engine::new(client),
            }
            .with_budget(budget);
            let worker = SupervisedWorker::new(worker_path);
            let mut recorded = Vec::new();
            let mut records = Vec::new();
            for fixture in fixtures {
                let name = fixture["file"].as_str().unwrap_or_default();
                let path = Path::new(fixtures_root).join(name);
                eprintln!("evaluating {name}");
                let live = LiveRun {
                    worker: &worker,
                    engine: &engine,
                    pipeline: &pipeline,
                    endpoint,
                    api_key,
                    model_id,
                    recorder: recorder.as_ref(),
                };
                let (record, entry) = evaluate_one(fixture, &path, &live);
                records.push(record);
                recorded.extend(entry);
            }
            worker.stop();
            if let Some(path) = record_path {
                Recording {
                    schema_version: Recording::VERSION,
                    model_id: model_id.to_owned(),
                    budget_characters: budget.max_characters,
                    recorded_at_unix: SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|elapsed| elapsed.as_secs())
                        .unwrap_or(0),
                    note: arguments.get("note").cloned().unwrap_or_default(),
                    fixtures: recorded,
                }
                .save(path)?;
                eprintln!("recorded {} fixtures to {path}", fixtures.len());
            }
            ("live", records)
        }
    };

    let mut exit = 0;
    let unscored = records
        .iter()
        .filter(|record| {
            matches!(
                record["status"].as_str(),
                Some("unrecorded" | "stale_prompt" | "stale_fixture")
            )
        })
        .map(|record| {
            format!(
                "{} ({})",
                record["file"].as_str().unwrap_or_default(),
                record["status"].as_str().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>();
    if !unscored.is_empty() {
        eprintln!(
            "{} fixture(s) could not be scored from the recording: {}",
            unscored.len(),
            unscored.join(", ")
        );
        eprintln!(
            "re-record the corpus (see docs/evaluation.md), or pass --allow-stale to score anyway"
        );
        exit = EXIT_REGRESSED;
    }

    let mut report = Map::new();
    report.insert("schema_version".into(), json!(3));
    report.insert("mode".into(), json!(mode));
    report.insert("pipeline".into(), json!(pipeline));
    report.insert("model_id".into(), json!(model_id));
    report.insert("budget_characters".into(), json!(budget.max_characters));
    report.insert("summary".into(), summarize(&records));

    if let Some(path) = arguments.get("baseline") {
        let baseline: Baseline = serde_json::from_slice(
            &fs::read(path).map_err(|error| format!("cannot read baseline: {error}"))?,
        )
        .map_err(|error| format!("cannot parse baseline: {error}"))?;
        let comparison = compare_with_baseline(&records, &baseline);
        for line in &comparison.regressions {
            eprintln!("regressed: {line}");
        }
        for line in &comparison.improvements {
            eprintln!("improved: {line}");
        }
        eprintln!(
            "baseline: {} regression(s), {} improvement(s), {} new fixture(s)",
            comparison.regressions.len(),
            comparison.improvements.len(),
            comparison.new.len()
        );
        if !comparison.regressions.is_empty() {
            exit = EXIT_REGRESSED;
        }
        report.insert(
            "baseline".into(),
            serde_json::to_value(&comparison).unwrap_or(Value::Null),
        );
    }
    if let Some(path) = arguments.get("write-baseline") {
        let baseline = baseline_from(&records);
        fs::write(
            path,
            serde_json::to_string_pretty(&baseline).unwrap_or_default() + "\n",
        )
        .map_err(|error| format!("cannot write baseline: {error}"))?;
        eprintln!(
            "wrote baseline for {} fixtures to {path}",
            baseline.fixtures.len()
        );
    }
    report.insert("records".into(), Value::Array(records));

    let rendered = serde_json::to_string_pretty(&Value::Object(report)).unwrap_or_default();
    match arguments.get("output") {
        Some(path) => fs::write(path, rendered + "\n")
            .map_err(|error| format!("cannot write report: {error}"))?,
        None => println!("{rendered}"),
    }
    Ok(exit)
}

/// What a live evaluation needs for one fixture.
struct LiveRun<'a> {
    worker: &'a SupervisedWorker,
    engine: &'a Engine,
    pipeline: &'a str,
    endpoint: &'a str,
    api_key: &'a str,
    model_id: &'a str,
    recorder: Option<&'a Recorder>,
}

fn evaluate_one(
    fixture: &Value,
    path: &Path,
    live: &LiveRun<'_>,
) -> (Value, Option<RecordedFixture>) {
    let name = fixture["file"].as_str().unwrap_or_default();
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let expected_error = fixture.get("expected_error").and_then(Value::as_str);
    let fixture_sha256 = fs::read(path).ok().map(|bytes| sha256_hex(&bytes));

    let extraction_started = Instant::now();
    let request_id = format!("eval-{}", name.replace(['/', '\\', '.'], "-"));
    let extracted = live.worker.extract(&request_id, path, &mut |_| {});
    let extraction_millis = millis(extraction_started);

    let source: DocumentSource = match extracted {
        Ok(source) => {
            if expected_error.is_some() {
                return (
                    json!({
                        "file": name, "status": "unexpected_success", "readiness": null,
                        "extraction_millis": extraction_millis,
                    }),
                    None,
                );
            }
            source
        }
        Err(error) => {
            let matched = expected_error == Some(error.code.as_str());
            let record = json!({
                "file": name,
                "status": if matched { "expected_error" } else { "extraction_failed" },
                "parser_error": error.code,
                "readiness": "failed",
                "extraction_millis": extraction_millis,
                "scores": {"expected_error": matched},
            });
            let entry = live.recorder.map(|_| RecordedFixture {
                file: name.to_owned(),
                sha256: fixture_sha256,
                extraction: RecordedExtraction::Failed { code: error.code },
                prompt_sha256: None,
                reply: None,
            });
            return (record, entry);
        }
    };

    if live.pipeline == "legacy" {
        return (
            legacy_record(
                fixture,
                &source,
                extension,
                extraction_millis,
                live.endpoint,
                live.api_key,
                live.model_id,
            ),
            None,
        );
    }

    let distill_started = Instant::now();
    let digest = live.engine.distill(&source);
    let distill_micros = u64::try_from(distill_started.elapsed().as_micros()).unwrap_or(u64::MAX);
    let prompt_sha256 = ModelRequest::from_digest(&digest).sha256();
    let record = match live
        .engine
        .analyze_digest(&source, &digest, distill_micros, extension, &[])
    {
        Ok(analysis) => completed_record(fixture, &analysis, &digest, extraction_millis),
        Err(error) => json!({
            "file": name,
            "status": "model_failed",
            "error": error.code().as_str(),
            "readiness": null,
            "extraction_millis": extraction_millis,
        }),
    };
    let entry = live.recorder.map(|recorder| RecordedFixture {
        file: name.to_owned(),
        sha256: fixture_sha256,
        extraction: RecordedExtraction::Parsed {
            source: without_image_bytes(source),
        },
        reply: recorder.reply_for(&prompt_sha256),
        prompt_sha256: Some(prompt_sha256),
    });
    (record, entry)
}

/// Scores one fixture from the recording alone.
fn replay_one(
    fixture: &Value,
    path: &Path,
    recording: &Recording,
    budget: DigestBudget,
    allow_stale: bool,
) -> Value {
    let name = fixture["file"].as_str().unwrap_or_default();
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let Some(recorded) = recording.fixtures.iter().find(|entry| entry.file == name) else {
        return json!({"file": name, "status": "unrecorded", "readiness": null, "replayed": true});
    };
    // The fixture on disk must be the one the recording was made from; a
    // regenerated corpus with different bytes would be scored against text
    // it no longer contains.
    if let (Some(expected), Ok(bytes)) = (&recorded.sha256, fs::read(path))
        && sha256_hex(&bytes) != *expected
    {
        return json!({"file": name, "status": "stale_fixture", "readiness": null, "replayed": true});
    }
    let expected_error = fixture.get("expected_error").and_then(Value::as_str);
    let source = match &recorded.extraction {
        RecordedExtraction::Failed { code } => {
            let matched = expected_error == Some(code.as_str());
            return json!({
                "file": name,
                "status": if matched { "expected_error" } else { "extraction_failed" },
                "parser_error": code,
                "readiness": "failed",
                "scores": {"expected_error": matched},
                "replayed": true,
            });
        }
        RecordedExtraction::Parsed { source } => {
            if expected_error.is_some() {
                return json!({"file": name, "status": "unexpected_success", "readiness": null, "replayed": true});
            }
            source.clone()
        }
    };
    let digest = intern_engine::distill(&source, budget);
    let prompt_sha256 = ModelRequest::from_digest(&digest).sha256();
    let stale = recorded.prompt_sha256.as_deref() != Some(prompt_sha256.as_str());
    if stale && !allow_stale {
        return json!({
            "file": name,
            "status": "stale_prompt",
            "readiness": null,
            "replayed": true,
            "recorded_prompt_sha256": recorded.prompt_sha256,
            "prompt_sha256": prompt_sha256,
        });
    }
    let proposal = match &recorded.reply {
        Some(RecordedReply::Proposed { proposal }) => proposal.clone(),
        Some(RecordedReply::Failed { code }) => {
            return json!({"file": name, "status": "model_failed", "error": code, "readiness": null, "replayed": true, "stale": stale});
        }
        None => {
            return json!({"file": name, "status": "model_failed", "error": "UNRECORDED", "readiness": null, "replayed": true, "stale": stale});
        }
    };
    let engine = Engine::with_proposer(Box::new(FixedReply(proposal))).with_budget(budget);
    match engine.analyze_digest(&source, &digest, 0, extension, &[]) {
        Ok(analysis) => {
            let mut record = completed_record(fixture, &analysis, &digest, 0);
            if let Some(object) = record.as_object_mut() {
                object.insert("replayed".into(), json!(true));
                object.insert("stale".into(), json!(stale));
            }
            record
        }
        Err(error) => json!({
            "file": name,
            "status": "model_failed",
            "error": error.code().as_str(),
            "readiness": null,
            "replayed": true,
            "stale": stale,
        }),
    }
}

fn completed_record(
    fixture: &Value,
    analysis: &DocumentAnalysis,
    digest: &DocumentDigest,
    extraction_millis: u64,
) -> Value {
    let name = fixture["file"].as_str().unwrap_or_default();
    let scores = score(
        fixture,
        ScoreInput {
            document_date: analysis.proposal.document_date.as_deref(),
            document_type: analysis.proposal.document_type.as_deref(),
            date_role: analysis.proposal.date_role.map(|role| role.as_str()),
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

/// Everything a live run saw that a replay needs: what the worker read from
/// each fixture, and what the model replied to the prompt built from it.
#[derive(Debug, Deserialize, Serialize)]
struct Recording {
    schema_version: u32,
    model_id: String,
    budget_characters: usize,
    recorded_at_unix: u64,
    /// Free text about the machine and runtime the recording was made on.
    #[serde(default)]
    note: String,
    fixtures: Vec<RecordedFixture>,
}

impl Recording {
    const VERSION: u32 = 1;

    fn load(path: &str) -> Result<Self, String> {
        let recording: Self = serde_json::from_slice(
            &fs::read(path).map_err(|error| format!("cannot read recording: {error}"))?,
        )
        .map_err(|error| format!("cannot parse recording: {error}"))?;
        if recording.schema_version != Self::VERSION {
            return Err(format!(
                "recording schema {} is not the supported {}",
                recording.schema_version,
                Self::VERSION
            ));
        }
        Ok(recording)
    }

    fn save(&self, path: &str) -> Result<(), String> {
        fs::write(
            path,
            serde_json::to_string_pretty(self).unwrap_or_default() + "\n",
        )
        .map_err(|error| format!("cannot write recording: {error}"))
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct RecordedFixture {
    file: String,
    /// SHA-256 of the fixture bytes the recording was made from.
    #[serde(default)]
    sha256: Option<String>,
    extraction: RecordedExtraction,
    /// SHA-256 of the prompt the reply answers, so a changed prompt or a
    /// changed distillation is caught rather than scored.
    #[serde(default)]
    prompt_sha256: Option<String>,
    #[serde(default)]
    reply: Option<RecordedReply>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
enum RecordedExtraction {
    Parsed { source: DocumentSource },
    Failed { code: String },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
enum RecordedReply {
    Proposed { proposal: ModelProposal },
    Failed { code: String },
}

/// A rendered page image is a signal that a page could not be read, never an
/// input, so the recording keeps the signal and drops the pixels.
fn without_image_bytes(mut source: DocumentSource) -> DocumentSource {
    source.page_image = source.page_image.map(|image| PageImage {
        bytes: Vec::new(),
        ..image
    });
    source
}

/// Remembers every reply the model gave during a live run, by prompt.
struct Recorder {
    replies: Arc<Mutex<HashMap<String, RecordedReply>>>,
}

impl Recorder {
    fn new() -> Self {
        Self {
            replies: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn proposer(&self, client: ModelClient) -> RecordingProposer {
        RecordingProposer {
            inner: client,
            replies: Arc::clone(&self.replies),
        }
    }

    fn reply_for(&self, prompt_sha256: &str) -> Option<RecordedReply> {
        self.replies
            .lock()
            .ok()
            .and_then(|replies| replies.get(prompt_sha256).cloned())
    }
}

struct RecordingProposer {
    inner: ModelClient,
    replies: Arc<Mutex<HashMap<String, RecordedReply>>>,
}

impl Proposer for RecordingProposer {
    fn propose(&self, request: &ModelRequest) -> EngineResult<ModelProposal> {
        let result = self.inner.propose(request);
        let reply = match &result {
            Ok(proposal) => RecordedReply::Proposed {
                proposal: proposal.clone(),
            },
            Err(error) => RecordedReply::Failed {
                code: error.code().as_str().to_owned(),
            },
        };
        if let Ok(mut replies) = self.replies.lock() {
            replies.insert(request.sha256(), reply);
        }
        result
    }
}

/// The reply a replay hands the engine in the model's place.
struct FixedReply(ModelProposal);

impl Proposer for FixedReply {
    fn propose(&self, _request: &ModelRequest) -> EngineResult<ModelProposal> {
        Ok(self.0.clone())
    }
}

/// The scores a run is held to: one entry per fixture, booleans only.
#[derive(Debug, Default, Deserialize, Serialize)]
struct Baseline {
    schema_version: u32,
    fixtures: BTreeMap<String, BaselineEntry>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct BaselineEntry {
    status: String,
    scores: BTreeMap<String, bool>,
}

#[derive(Debug, Default, Serialize)]
struct Comparison {
    regressions: Vec<String>,
    improvements: Vec<String>,
    new: Vec<String>,
}

fn baseline_from(records: &[Value]) -> Baseline {
    let mut fixtures = BTreeMap::new();
    for record in records {
        let Some(file) = record["file"].as_str() else {
            continue;
        };
        let scores = record["scores"]
            .as_object()
            .map(|scores| {
                scores
                    .iter()
                    .filter_map(|(key, value)| value.as_bool().map(|flag| (key.clone(), flag)))
                    .collect()
            })
            .unwrap_or_default();
        fixtures.insert(
            file.to_owned(),
            BaselineEntry {
                status: record["status"].as_str().unwrap_or_default().to_owned(),
                scores,
            },
        );
    }
    Baseline {
        schema_version: 1,
        fixtures,
    }
}

/// A score is good when it is true, except the trap scores, which count
/// something that must not happen. `ready` on its own is not quality.
fn is_good(key: &str, value: bool) -> bool {
    if key.ends_with("_forbidden") {
        !value
    } else {
        value
    }
}

fn compare_with_baseline(records: &[Value], baseline: &Baseline) -> Comparison {
    let mut comparison = Comparison::default();
    for record in records {
        let Some(file) = record["file"].as_str() else {
            continue;
        };
        let Some(expected) = baseline.fixtures.get(file) else {
            comparison.new.push(file.to_owned());
            continue;
        };
        let status = record["status"].as_str().unwrap_or_default();
        if status != expected.status {
            comparison
                .regressions
                .push(format!("{file}: was {}, now {status}", expected.status));
            continue;
        }
        let scores = record["scores"].as_object();
        for (key, was) in &expected.scores {
            if key == "ready" {
                continue;
            }
            let now = scores
                .and_then(|scores| scores.get(key))
                .and_then(Value::as_bool);
            let was_good = is_good(key, *was);
            let now_good = now.is_some_and(|now| is_good(key, now));
            if was_good && !now_good {
                comparison
                    .regressions
                    .push(format!("{file}: {key} was {was}, now {}", describe(now)));
            } else if !was_good && now_good {
                comparison
                    .improvements
                    .push(format!("{file}: {key} was {was}, now {}", describe(now)));
            }
        }
    }
    comparison
}

fn describe(value: Option<bool>) -> String {
    value.map_or_else(|| "absent".to_owned(), |flag| flag.to_string())
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
            // The old pipeline had no vocabulary for what a date meant; that
            // absence is the point of the comparison, not a gap in the scoring.
            date_role: None,
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
    date_role: Option<&'a str>,
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
        // Picking the right date and knowing why it is the right date are two
        // different things, and the second is what keeps the first from being
        // luck. Scored only where the corpus states a role and a date was
        // produced, because a role attached to no date measures nothing.
        if let Some(gold_role) = fixture
            .get("date_role")
            .and_then(Value::as_str)
            .filter(|role| !role.is_empty())
            && actual.document_date.is_some()
        {
            scores.insert(
                "date_role_correct".into(),
                Value::Bool(actual.date_role == Some(gold_role)),
            );
        }
    }
    if let Some(gold_type) = fixture.get("document_type").and_then(Value::as_str) {
        // A document can have more than one right name. The minutes fixture is
        // titled "Quarterly Operations Review" and never uses the word
        // "minutes", so demanding "Meeting Minutes" would contradict the rule
        // that a document type has to be grounded in the document's own words.
        let acceptable = strings(fixture, "acceptable_types");
        scores.insert(
            "type_correct".into(),
            Value::Bool(actual.document_type.is_some_and(|value| {
                type_matches(gold_type, value)
                    || acceptable.iter().any(|other| type_matches(other, value))
            })),
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
    let mut totals: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut inference = Vec::new();
    let mut total_time = Vec::new();
    let mut ready = 0;
    let mut scored = 0;
    let mut by_status: BTreeMap<String, usize> = BTreeMap::new();
    for record in records {
        *by_status
            .entry(record["status"].as_str().unwrap_or("unknown").to_owned())
            .or_insert(0) += 1;
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
                let entry = totals.entry(key.clone()).or_insert((0, 0));
                entry.1 += 1;
                if flag {
                    entry.0 += 1;
                }
            }
        }
    }
    let mut summary = Map::new();
    for (key, (correct, total)) in totals {
        summary.insert(
            key,
            json!({"correct": correct, "total": total, "rate": correct as f64 / total.max(1) as f64}),
        );
    }
    summary.insert("evaluated".into(), json!(scored));
    summary.insert("statuses".into(), json!(by_status));
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

/// Arguments that take no value.
const FLAGS: &[&str] = &["allow-stale"];

fn parse_arguments() -> Result<HashMap<String, String>, String> {
    let mut values = HashMap::new();
    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        let key = argument
            .strip_prefix("--")
            .ok_or_else(|| format!("unexpected argument: {argument}"))?;
        if FLAGS.contains(&key) {
            values.insert(key.to_owned(), "true".to_owned());
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

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use intern_engine::{DateRole, Evidence, PartyRelation, source_from_text};

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

    fn invoice_source() -> DocumentSource {
        source_from_text(
            "INVOICE\nAcme Corporation, 500 Foundry Road\nInvoice Number: INV-7741\n\
             Invoice Date: January 5, 2026\nPayment Due Date: February 4, 2026\n\
             Bill To: Vistage Worldwide, Inc.\nConsulting services for January, $1,248.00.",
        )
    }

    fn invoice_reply() -> ModelProposal {
        ModelProposal {
            document_type: Some("Invoice".into()),
            document_date: Some("2026-01-05".into()),
            date_role: Some(DateRole::Invoice),
            parties: vec!["Acme Corporation".into()],
            party_relation: PartyRelation::From,
            description: "Invoice INV-7741 from Acme Corporation to Vistage Worldwide, Inc. for January consulting services of $1,248.00."
                .into(),
            confidence: 0.92,
            needs_review: false,
            evidence: Evidence {
                date: Some("Invoice Date: January 5, 2026".into()),
                document_type: Some("INVOICE".into()),
                parties: vec!["Acme Corporation, 500 Foundry Road".into()],
            },
        }
    }

    fn invoice_fixture() -> Value {
        json!({
            "file": "invoice.txt",
            "document_type": "Invoice",
            "document_date": "2026-01-05",
            "forbidden_dates": ["2026-02-04"],
            "date_role": "invoice",
            "parties": ["Acme Corporation"],
            "expected_readiness": "ready",
        })
    }

    fn recording_with(prompt_sha256: Option<String>) -> Recording {
        Recording {
            schema_version: Recording::VERSION,
            model_id: "test".into(),
            budget_characters: DigestBudget::default().max_characters,
            recorded_at_unix: 0,
            note: String::new(),
            fixtures: vec![RecordedFixture {
                file: "invoice.txt".into(),
                sha256: None,
                extraction: RecordedExtraction::Parsed {
                    source: invoice_source(),
                },
                prompt_sha256,
                reply: Some(RecordedReply::Proposed {
                    proposal: invoice_reply(),
                }),
            }],
        }
    }

    /// The whole point of a recording: the engine's own validation and
    /// naming run over the recorded reply, and the score is the score a live
    /// run would give.
    #[test]
    fn a_recorded_reply_is_scored_through_the_real_validation_and_naming() {
        let budget = DigestBudget::default();
        let digest = intern_engine::distill(&invoice_source(), budget);
        let sha = ModelRequest::from_digest(&digest).sha256();
        let record = replay_one(
            &invoice_fixture(),
            Path::new("/nonexistent/invoice.txt"),
            &recording_with(Some(sha)),
            budget,
            false,
        );
        assert_eq!(record["status"], json!("completed"), "{record}");
        assert_eq!(record["replayed"], json!(true));
        assert_eq!(record["stale"], json!(false));
        assert_eq!(
            record["filename"],
            json!("2026-01-05 Invoice from Acme Corporation.txt")
        );
        assert_eq!(record["scores"]["date_correct"], json!(true));
        assert_eq!(record["scores"]["date_forbidden"], json!(false));
        assert_eq!(record["scores"]["parties_correct"], json!(true));
        assert_eq!(record["scores"]["readiness_match"], json!(true));
    }

    /// A reply to a prompt the engine no longer builds says nothing about the
    /// engine as it is now. Refused by default; scored and marked on request.
    #[test]
    fn a_reply_to_a_changed_prompt_is_refused_unless_staleness_is_allowed() {
        let budget = DigestBudget::default();
        let recording = recording_with(Some("0".repeat(64)));
        let refused = replay_one(
            &invoice_fixture(),
            Path::new("/nonexistent/invoice.txt"),
            &recording,
            budget,
            false,
        );
        assert_eq!(refused.get("status"), Some(&json!("stale_prompt")));
        assert!(refused.get("scores").is_none());

        let tolerated = replay_one(
            &invoice_fixture(),
            Path::new("/nonexistent/invoice.txt"),
            &recording,
            budget,
            true,
        );
        assert_eq!(tolerated["status"], json!("completed"));
        assert_eq!(tolerated["stale"], json!(true));

        let missing = replay_one(
            &json!({"file": "never-recorded.pdf"}),
            Path::new("/nonexistent/never-recorded.pdf"),
            &recording,
            budget,
            true,
        );
        assert_eq!(missing["status"], json!("unrecorded"));
    }

    #[test]
    fn the_baseline_reports_a_lost_answer_a_sprung_trap_and_a_gained_answer() {
        let baseline = Baseline {
            schema_version: 1,
            fixtures: BTreeMap::from([
                (
                    "a.pdf".to_owned(),
                    BaselineEntry {
                        status: "completed".into(),
                        scores: BTreeMap::from([
                            ("date_correct".to_owned(), true),
                            ("date_forbidden".to_owned(), false),
                            ("ready".to_owned(), true),
                        ]),
                    },
                ),
                (
                    "b.pdf".to_owned(),
                    BaselineEntry {
                        status: "completed".into(),
                        scores: BTreeMap::from([("type_correct".to_owned(), false)]),
                    },
                ),
                (
                    "c.pdf".to_owned(),
                    BaselineEntry {
                        status: "completed".into(),
                        scores: BTreeMap::new(),
                    },
                ),
            ]),
        };
        let records = vec![
            json!({"file": "a.pdf", "status": "completed", "scores": {"date_correct": false, "date_forbidden": true, "ready": false}}),
            json!({"file": "b.pdf", "status": "completed", "scores": {"type_correct": true}}),
            json!({"file": "c.pdf", "status": "stale_prompt"}),
            json!({"file": "d.pdf", "status": "completed", "scores": {"type_correct": true}}),
        ];
        let comparison = compare_with_baseline(&records, &baseline);
        assert_eq!(
            comparison.regressions,
            vec![
                "a.pdf: date_correct was true, now false",
                "a.pdf: date_forbidden was false, now true",
                "c.pdf: was completed, now stale_prompt",
            ]
        );
        assert_eq!(
            comparison.improvements,
            vec!["b.pdf: type_correct was false, now true"]
        );
        assert_eq!(comparison.new, vec!["d.pdf"]);

        let written = baseline_from(&records);
        assert!(written.fixtures["a.pdf"].scores["date_forbidden"]);
        assert_eq!(written.fixtures["c.pdf"].status, "stale_prompt");
    }
}
