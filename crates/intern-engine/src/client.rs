//! HTTP client for the local llama.cpp server.
//!
//! The endpoint is required to be loopback HTTP with no proxy and no redirects,
//! so a misconfiguration cannot silently send document text off the machine.
//! Sending it off the machine on purpose is [`crate::hosted`]'s job, behind
//! an explicit setting.

use std::time::Duration;

use reqwest::{Url, blocking::Client};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::domain::{DateRole, Evidence, ModelProposal, PartyRelation};
use crate::error::{EngineError, EngineErrorCode, EngineResult};
use crate::evidence::is_valid_iso_date;
use crate::prompt::{RESPONSE_GRAMMAR, SYSTEM_INSTRUCTION, build_prompt};

/// Room for the reply plus the grammar's fixed scaffolding. The model is not
/// writing prose, so this stays small and generation stays fast.
const MAX_REPLY_TOKENS: u32 = 420;

/// What actually gets sent to the model for one document.
///
/// Text only, by construction. Intern reads documents as text and the local
/// server runs without a vision projector, so there is no field here that could
/// ask it for something it cannot do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelRequest {
    pub prompt: String,
}

impl ModelRequest {
    pub fn from_digest(digest: &crate::distill::DocumentDigest) -> Self {
        Self {
            prompt: build_prompt(digest),
        }
    }

    /// A stable identity for this exact input, for evaluation records.
    pub fn sha256(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.prompt.as_bytes());
        format!("{:x}", hasher.finalize())
    }
}

/// Anything that can turn the prompt for one document into a proposal: the
/// local server, or a hosted model standing in for it.
pub trait Proposer: Send + Sync {
    fn propose(&self, request: &ModelRequest) -> EngineResult<ModelProposal>;
}

pub struct ModelClient {
    endpoint: Url,
    api_key: String,
    model_id: String,
    http: Client,
}

impl Proposer for ModelClient {
    fn propose(&self, request: &ModelRequest) -> EngineResult<ModelProposal> {
        ModelClient::propose(self, request)
    }
}

impl ModelClient {
    pub fn new(
        endpoint: &str,
        api_key: impl Into<String>,
        model_id: impl Into<String>,
    ) -> EngineResult<Self> {
        let endpoint = Url::parse(endpoint).map_err(|_| request_failed())?;
        let loopback = endpoint
            .host_str()
            .and_then(|host| host.parse::<std::net::IpAddr>().ok())
            .is_some_and(|address| address.is_loopback());
        if endpoint.scheme() != "http" || !loopback {
            return Err(EngineError::new(
                EngineErrorCode::ModelRequestFailed,
                "model endpoint must be local HTTP",
            ));
        }
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10 * 60))
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| request_failed())?;
        Ok(Self {
            endpoint,
            api_key: api_key.into(),
            model_id: model_id.into(),
            http,
        })
    }

    /// One attempt, then one retry on a malformed reply.
    pub fn propose(&self, request: &ModelRequest) -> EngineResult<ModelProposal> {
        match self.propose_once(request) {
            Ok(proposal) => Ok(proposal),
            Err(_) => self.propose_once(request).map_err(AttemptError::into_error),
        }
    }

    pub(crate) fn propose_once(
        &self,
        request: &ModelRequest,
    ) -> Result<ModelProposal, AttemptError> {
        let response = self
            .http
            .post(self.endpoint.clone())
            .bearer_auth(&self.api_key)
            .json(&self.completion_request(request))
            .send()
            .map_err(|_| AttemptError(EngineErrorCode::ModelRequestFailed))?;
        if !response.status().is_success() {
            return Err(AttemptError(EngineErrorCode::ModelRequestFailed));
        }
        let completion: ChatCompletion = response
            .json()
            .map_err(|_| AttemptError(EngineErrorCode::ModelResponseInvalid))?;
        decode(completion)
    }

    fn completion_request(&self, request: &ModelRequest) -> Value {
        json!({
            "model": self.model_id,
            "messages": [
                {"role": "system", "content": SYSTEM_INSTRUCTION},
                {"role": "user", "content": request.prompt}
            ],
            "stream": false,
            "temperature": 0,
            "top_k": 1,
            "max_tokens": MAX_REPLY_TOKENS,
            "grammar": RESPONSE_GRAMMAR,
            "cache_prompt": true,
            // Hybrid-reasoning models must answer directly: Intern needs a form
            // filled in, not a chain of thought, and thinking tokens are pure
            // latency on a CPU.
            "chat_template_kwargs": {"enable_thinking": false}
        })
    }
}

/// Reads a proposal out of a chat-completion reply: the local server's, or
/// any OpenAI-compatible service's.
pub(crate) fn decode(completion: ChatCompletion) -> Result<ModelProposal, AttemptError> {
    let choice = completion
        .choices
        .into_iter()
        .next()
        .ok_or(AttemptError(EngineErrorCode::ModelResponseInvalid))?;
    if !matches!(choice.finish_reason.as_deref(), Some("stop") | None) {
        return Err(AttemptError(EngineErrorCode::ModelResponseInvalid));
    }
    let content = choice
        .message
        .content
        .into_text()
        .ok_or(AttemptError(EngineErrorCode::ModelResponseInvalid))?;
    proposal_from_text(&content)
}

/// Reads a proposal out of the text a model replied with, fences and
/// chatter tolerated.
pub(crate) fn proposal_from_text(content: &str) -> Result<ModelProposal, AttemptError> {
    let json =
        extract_json_object(content).ok_or(AttemptError(EngineErrorCode::ModelResponseInvalid))?;
    let wire: WireProposal = serde_json::from_str(json)
        .map_err(|_| AttemptError(EngineErrorCode::ModelResponseInvalid))?;
    wire.into_domain()
        .ok_or(AttemptError(EngineErrorCode::ModelResponseInvalid))
}

/// Recovers the JSON object from a reply that may be fenced or prefixed.
pub fn extract_json_object(content: &str) -> Option<&str> {
    let trimmed = content.trim();
    if let Some(fenced) = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```JSON"))
        .or_else(|| trimmed.strip_prefix("```"))
    {
        return fenced.strip_suffix("```").map(str::trim);
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    (end > start).then(|| trimmed[start..=end].trim())
}

#[derive(Deserialize)]
pub(crate) struct ChatCompletion {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: AssistantMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct AssistantMessage {
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
                        text.push_str(part.text.as_deref()?);
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
struct WireProposal {
    #[serde(default)]
    type_evidence: Option<String>,
    #[serde(default)]
    document_type: Option<String>,
    #[serde(default)]
    date_evidence: Option<String>,
    #[serde(default)]
    document_date: Option<String>,
    #[serde(default)]
    date_role: Option<DateRole>,
    #[serde(default)]
    party_evidence: Vec<String>,
    #[serde(default)]
    parties: Vec<String>,
    #[serde(default)]
    party_relation: Option<PartyRelation>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    confidence: f32,
    #[serde(default)]
    needs_review: bool,
}

impl WireProposal {
    fn into_domain(mut self) -> Option<ModelProposal> {
        if !self.confidence.is_finite() || !(0.0..=1.0).contains(&self.confidence) {
            return None;
        }
        if self
            .document_date
            .as_deref()
            .map(str::trim)
            .is_some_and(|date| !date.is_empty() && !is_valid_iso_date(date))
        {
            // A shape-valid but non-calendar date is a hard reply failure; the
            // grammar guarantees the shape, so this is the model inventing a
            // day that does not exist.
            self.document_date = None;
            self.date_role = None;
        }
        self.parties.truncate(3);
        self.party_evidence.truncate(3);
        Some(ModelProposal {
            document_type: self.document_type,
            document_date: self.document_date,
            date_role: self.date_role,
            parties: self.parties,
            party_relation: self.party_relation.unwrap_or(PartyRelation::None),
            description: self.description,
            confidence: self.confidence,
            needs_review: self.needs_review,
            evidence: Evidence {
                date: self.date_evidence,
                document_type: self.type_evidence,
                parties: self.party_evidence,
            },
        })
    }
}

#[derive(Debug)]
pub(crate) struct AttemptError(pub(crate) EngineErrorCode);

impl AttemptError {
    fn into_error(self) -> EngineError {
        match self.0 {
            EngineErrorCode::ModelRequestFailed => request_failed(),
            _ => EngineError::new(
                EngineErrorCode::ModelResponseInvalid,
                "local model returned malformed output twice",
            ),
        }
    }
}

const fn request_failed() -> EngineError {
    EngineError::new(
        EngineErrorCode::ModelRequestFailed,
        "local model request failed",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_non_loopback_endpoint_is_refused() {
        assert!(ModelClient::new("http://example.com/v1/chat/completions", "k", "m").is_err());
        assert!(ModelClient::new("https://127.0.0.1/v1/chat/completions", "k", "m").is_err());
        assert!(ModelClient::new("http://127.0.0.1:8080/v1/chat/completions", "k", "m").is_ok());
    }

    #[test]
    fn json_is_recovered_from_fences_and_from_surrounding_chatter() {
        assert_eq!(
            extract_json_object("```json\n{\"a\":1}\n```"),
            Some("{\"a\":1}")
        );
        assert_eq!(
            extract_json_object("Sure!\n{\"a\":1}\nDone"),
            Some("{\"a\":1}")
        );
        assert_eq!(extract_json_object("no object here"), None);
    }

    #[test]
    fn a_reply_missing_optional_fields_still_decodes() {
        let wire: WireProposal =
            serde_json::from_str(r#"{"description":"A document.","confidence":0.7}"#).unwrap();
        let proposal = wire.into_domain().unwrap();
        assert_eq!(proposal.party_relation, PartyRelation::None);
        assert!(proposal.document_date.is_none());
    }

    #[test]
    fn an_impossible_calendar_date_is_discarded_rather_than_trusted() {
        let wire: WireProposal = serde_json::from_str(
            r#"{"document_date":"2026-02-31","date_role":"effective","description":"x","confidence":0.9}"#,
        )
        .unwrap();
        let proposal = wire.into_domain().unwrap();
        assert!(proposal.document_date.is_none());
        assert!(proposal.date_role.is_none());
    }

    #[test]
    fn the_request_carries_the_grammar_and_no_thinking() {
        let client = ModelClient::new("http://127.0.0.1:9/v1/chat/completions", "k", "m").unwrap();
        let body = client.completion_request(&ModelRequest { prompt: "p".into() });
        assert_eq!(body["grammar"], serde_json::json!(RESPONSE_GRAMMAR));
        assert_eq!(body["temperature"], serde_json::json!(0));
        assert_eq!(
            body["chat_template_kwargs"]["enable_thinking"],
            serde_json::json!(false)
        );
    }

    /// The local server runs without a vision projector. A request that carried
    /// an image would be rejected by it, so the request must always be plain
    /// text - never a multimodal content array.
    #[test]
    fn every_request_is_plain_text() {
        let client = ModelClient::new("http://127.0.0.1:9/v1/chat/completions", "k", "m").unwrap();
        let body = client.completion_request(&ModelRequest { prompt: "p".into() });
        assert!(body["messages"][1]["content"].is_string());
        assert!(!body.to_string().contains("image_url"));
    }

    #[test]
    fn the_request_identity_follows_the_prompt() {
        assert_ne!(
            ModelRequest { prompt: "a".into() }.sha256(),
            ModelRequest { prompt: "b".into() }.sha256()
        );
    }
}
