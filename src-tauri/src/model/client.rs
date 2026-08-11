use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use intern_core::{DateKind, Evidence, ModelProposal};
use reqwest::{Url, blocking::Client};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{
    ModelError, ModelErrorCode, ModelResult,
    prompt::{MODEL_GBNF, SYSTEM_INSTRUCTION, build_prompt},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImageInput {
    pub media_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentInput {
    pub text: String,
    pub image: Option<ImageInput>,
}

pub fn document_input_sha256(document: &DocumentInput) -> String {
    let mut hasher = Sha256::new();
    hasher.update(document.text.as_bytes());
    if let Some(image) = &document.image {
        hasher.update([0]);
        hasher.update(image.media_type.as_bytes());
        hasher.update([0]);
        hasher.update(&image.bytes);
    }
    format!("{:x}", hasher.finalize())
}

pub struct ModelClient {
    endpoint: Url,
    api_key: String,
    http: Client,
}

impl ModelClient {
    pub fn new(endpoint: &str, api_key: impl Into<String>) -> ModelResult<Self> {
        let endpoint = Url::parse(endpoint).map_err(|_| request_failed())?;
        let loopback = endpoint
            .host_str()
            .and_then(|host| host.parse::<std::net::IpAddr>().ok())
            .is_some_and(|address| address.is_loopback());
        if endpoint.scheme() != "http" || !loopback {
            return Err(ModelError::new(
                ModelErrorCode::ModelRequestFailed,
                "model endpoint must be local HTTP",
            ));
        }
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15 * 60))
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| request_failed())?;
        Ok(Self {
            endpoint,
            api_key: api_key.into(),
            http,
        })
    }

    pub fn propose(&self, document: &DocumentInput) -> ModelResult<ModelProposal> {
        for attempt in 0..=1 {
            match self.propose_once(document) {
                Ok(proposal) => return Ok(proposal),
                Err(AttemptError::Retryable) if attempt == 0 => continue,
                Err(AttemptError::Retryable) => {
                    return Err(ModelError::new(
                        ModelErrorCode::ModelResponseInvalid,
                        "local model returned malformed or interrupted output twice",
                    ));
                }
                Err(AttemptError::Fatal(error)) => return Err(error),
            }
        }
        unreachable!("the retry loop has exactly two attempts")
    }

    fn propose_once(&self, document: &DocumentInput) -> Result<ModelProposal, AttemptError> {
        let request = completion_request(document);
        let response = self
            .http
            .post(self.endpoint.clone())
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()
            .map_err(|_| AttemptError::Retryable)?;
        if !response.status().is_success() {
            return Err(AttemptError::Fatal(request_failed()));
        }
        let completion: ChatCompletion = response.json().map_err(|_| AttemptError::Retryable)?;
        decode_completion(completion)
    }
}

fn completion_request(document: &DocumentInput) -> Value {
    let prompt = build_prompt(&document.text);
    let content = if let Some(image) = &document.image {
        json!([
            {"type": "text", "text": prompt},
            {
                "type": "image_url",
                "image_url": {"url": format!("data:{};base64,{}", image.media_type, STANDARD.encode(&image.bytes))}
            }
        ])
    } else {
        json!(prompt)
    };
    json!({
        "model": "local-qwen2.5-vl-3b-instruct",
        "messages": [
            {"role": "system", "content": SYSTEM_INSTRUCTION},
            {"role": "user", "content": content}
        ],
        "stream": false,
        "temperature": 0,
        "max_tokens": 2048,
        "grammar": MODEL_GBNF
    })
}

fn decode_completion(completion: ChatCompletion) -> Result<ModelProposal, AttemptError> {
    if completion.object != "chat.completion" || completion.choices.len() != 1 {
        return Err(AttemptError::Retryable);
    }
    let choice = completion
        .choices
        .into_iter()
        .next()
        .ok_or(AttemptError::Retryable)?;
    if choice.finish_reason.as_deref() != Some("stop") || choice.message.role != "assistant" {
        return Err(AttemptError::Retryable);
    }
    let content = choice
        .message
        .content
        .into_text()
        .ok_or(AttemptError::Retryable)?;
    let json = extract_json_object(&content).ok_or(AttemptError::Retryable)?;
    let proposal: WireProposal = serde_json::from_str(json).map_err(|_| AttemptError::Retryable)?;
    proposal.into_domain().ok_or(AttemptError::Retryable)
}

fn extract_json_object(content: &str) -> Option<&str> {
    let trimmed = content.trim();
    if let Some(fenced) = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```JSON"))
    {
        return fenced.strip_suffix("```").map(str::trim);
    }
    if let Some(fenced) = trimmed.strip_prefix("```") {
        return fenced.strip_suffix("```").map(str::trim);
    }
    (trimmed.starts_with('{') && trimmed.ends_with('}')).then_some(trimmed)
}

#[derive(Deserialize)]
struct ChatCompletion {
    #[allow(dead_code)]
    id: String,
    object: String,
    #[allow(dead_code)]
    created: u64,
    #[allow(dead_code)]
    model: String,
    choices: Vec<Choice>,
    #[allow(dead_code)]
    usage: Usage,
    #[allow(dead_code)]
    system_fingerprint: Option<String>,
    #[allow(dead_code)]
    timings: Option<Value>,
}

#[derive(Deserialize)]
struct Choice {
    #[allow(dead_code)]
    index: u32,
    message: AssistantMessage,
    #[allow(dead_code)]
    logprobs: Option<Value>,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct AssistantMessage {
    role: String,
    content: AssistantContent,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AssistantContent {
    Text(String),
    Parts(Vec<AssistantContentPart>),
}

impl AssistantContent {
    fn into_text(self) -> Option<String> {
        match self {
            Self::Text(text) => Some(text),
            Self::Parts(parts) => {
                let mut text = String::new();
                for part in parts {
                    if part.kind == "text" {
                        text.push_str(&part.text?);
                    }
                }
                (!text.is_empty()).then_some(text)
            }
        }
    }
}

#[derive(Deserialize)]
struct AssistantContentPart {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

#[derive(Deserialize)]
struct Usage {
    #[allow(dead_code)]
    prompt_tokens: u64,
    #[allow(dead_code)]
    completion_tokens: u64,
    #[allow(dead_code)]
    total_tokens: u64,
    #[allow(dead_code)]
    prompt_tokens_details: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireProposal {
    document_date: Option<String>,
    date_kind: Option<DateKind>,
    document_type: Option<String>,
    filename_subject: Option<String>,
    parties: Vec<String>,
    description: String,
    confidence: f32,
    needs_review: bool,
    review_reasons: Vec<String>,
    date_evidence: Option<String>,
    type_evidence: Option<String>,
    subject_evidence: Option<String>,
    party_evidence: Vec<String>,
}

impl WireProposal {
    fn into_domain(self) -> Option<ModelProposal> {
        if !self.confidence.is_finite()
            || !(0.0..=1.0).contains(&self.confidence)
            || self.parties.len() > 8
            || self.review_reasons.len() > 8
            || self.party_evidence.len() > 8
            || self
                .document_date
                .as_deref()
                .is_some_and(|date| !date.trim().is_empty() && !is_iso_date_shape(date))
        {
            return None;
        }

        Some(ModelProposal {
            document_date: self.document_date,
            date_kind: self.date_kind,
            document_type: self.document_type,
            filename_subject: self.filename_subject,
            parties: self.parties,
            description: self.description,
            confidence: self.confidence,
            needs_review: self.needs_review,
            review_reasons: self.review_reasons,
            evidence: Evidence {
                date: self.date_evidence,
                document_type: self.type_evidence,
                subject: self.subject_evidence,
                parties: self.party_evidence,
            },
        })
    }
}

fn is_iso_date_shape(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
}

enum AttemptError {
    Retryable,
    Fatal(ModelError),
}

const fn request_failed() -> ModelError {
    ModelError::new(
        ModelErrorCode::ModelRequestFailed,
        "local model request failed",
    )
}
