//! A hosted model behind an API key, standing in for the local one.
//!
//! Everything about how Intern understands a document is unchanged: the same
//! distillation of the whole file, the same prompt, the same evidence checks
//! on the reply, the same naming. What changes is where the prompt goes. The
//! local server never leaves `127.0.0.1`; this client sends the distilled
//! text of every document to whoever runs the endpoint, and that is the whole
//! reason it is off unless a person turns it on, supplies a key, and is told
//! in Settings what it means.
//!
//! Two wire formats cover nearly every service: Anthropic's Messages API, and
//! the chat-completions shape that OpenAI defined and most other providers
//! and local servers (LM Studio, Ollama, llama.cpp) copy. A request carries
//! only what every server understands - the model, the two messages, and for
//! Anthropic the required output cap - because a sampling knob one provider
//! rejects is a document that never gets filed.

use std::time::Duration;

use reqwest::{StatusCode, Url, blocking::Client};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::client::{
    AttemptError, ChatCompletion, ModelRequest, Proposer, decode, proposal_from_text,
};
use crate::domain::{DocumentAnalysis, ModelProposal};
use crate::engine::Engine;
use crate::error::{EngineError, EngineErrorCode, EngineResult};
use crate::prompt::SYSTEM_INSTRUCTION;
use crate::setup::{semantic_probes, validate_semantic_probe};

/// The Messages API version this client speaks.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";
pub const DEFAULT_ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com/v1";
pub const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
/// The model offered when the Anthropic provider is chosen and none is named.
pub const DEFAULT_ANTHROPIC_MODEL: &str = "claude-opus-5";

/// Room for the reply. On an Anthropic model the cap also covers the thinking
/// that precedes the answer, so it is generous; the answer itself is short.
const MAX_REPLY_TOKENS: u32 = 16_000;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

/// Which wire format the endpoint speaks.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostedProvider {
    /// Anthropic's Messages API.
    #[default]
    Anthropic,
    /// OpenAI's chat completions, and every service or local server that
    /// copies the shape.
    #[serde(rename = "openai_compatible")]
    OpenAiCompatible,
}

impl HostedProvider {
    pub const ALL: [Self; 2] = [Self::Anthropic, Self::OpenAiCompatible];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAiCompatible => "openai_compatible",
        }
    }

    pub const fn default_base_url(self) -> &'static str {
        match self {
            Self::Anthropic => DEFAULT_ANTHROPIC_BASE_URL,
            Self::OpenAiCompatible => DEFAULT_OPENAI_BASE_URL,
        }
    }

    /// The model used when none is named; only Anthropic has a sensible one.
    pub const fn default_model(self) -> &'static str {
        match self {
            Self::Anthropic => DEFAULT_ANTHROPIC_MODEL,
            Self::OpenAiCompatible => "",
        }
    }
}

/// Everything needed to reach one hosted model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostedModelConfig {
    pub provider: HostedProvider,
    /// The API root, `https://api.anthropic.com/v1` or the like. Empty means
    /// the provider's default.
    pub base_url: String,
    /// The model name as the service knows it. Empty means the provider's
    /// default, where there is one.
    pub model: String,
    pub api_key: String,
}

impl HostedModelConfig {
    /// Fills empty fields with the provider's defaults and refuses anything
    /// a request could not be made from.
    pub fn resolved(self) -> EngineResult<Self> {
        let base_url = {
            let trimmed = self.base_url.trim();
            if trimmed.is_empty() {
                self.provider.default_base_url().to_owned()
            } else {
                trimmed.to_owned()
            }
        };
        let model = {
            let trimmed = self.model.trim();
            if trimmed.is_empty() {
                self.provider.default_model().to_owned()
            } else {
                trimmed.to_owned()
            }
        };
        let api_key = self.api_key.trim().to_owned();
        if model.is_empty() {
            return Err(misconfigured("the hosted model has no model name"));
        }
        if api_key.is_empty() {
            return Err(misconfigured("the hosted model has no API key"));
        }
        endpoint_for(self.provider, &base_url)?;
        Ok(Self {
            provider: self.provider,
            base_url,
            model,
            api_key,
        })
    }
}

/// The request endpoint for a provider under an API root: `<root>/messages`
/// for Anthropic, `<root>/chat/completions` for the rest. Only HTTPS is
/// accepted, except plain HTTP to this machine, which is how a local server
/// such as LM Studio or Ollama is reached.
pub fn endpoint_for(provider: HostedProvider, base_url: &str) -> EngineResult<Url> {
    let root = Url::parse(base_url.trim().trim_end_matches('/'))
        .map_err(|_| misconfigured("the hosted model's address is not a URL"))?;
    let local = root.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    });
    match root.scheme() {
        "https" => {}
        "http" if local => {}
        _ => {
            return Err(misconfigured(
                "the hosted model's address must use https, unless it is this machine",
            ));
        }
    }
    if root.cannot_be_a_base() || root.host_str().is_none() {
        return Err(misconfigured("the hosted model's address has no host"));
    }
    let path = match provider {
        HostedProvider::Anthropic => "messages",
        HostedProvider::OpenAiCompatible => "chat/completions",
    };
    let mut endpoint = root;
    let base_path = endpoint.path().trim_end_matches('/').to_owned();
    endpoint.set_path(&format!("{base_path}/{path}"));
    endpoint.set_query(None);
    endpoint.set_fragment(None);
    Ok(endpoint)
}

/// The client for one hosted model.
#[derive(Clone)]
pub struct HostedClient {
    config: HostedModelConfig,
    endpoint: Url,
    http: Client,
}

impl std::fmt::Debug for HostedClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HostedClient")
            .field("provider", &self.config.provider)
            .field("endpoint", &self.endpoint.as_str())
            .field("model", &self.config.model)
            .field("api_key", &"[redacted]")
            .finish()
    }
}

impl HostedClient {
    pub fn new(config: HostedModelConfig) -> EngineResult<Self> {
        let config = config.resolved()?;
        let endpoint = endpoint_for(config.provider, &config.base_url)?;
        // The system proxy is honoured here, unlike for the local server: a
        // machine that reaches the internet through a proxy reaches this
        // endpoint through it too. Redirects are still refused, so a key is
        // only ever sent to the address that was configured.
        let http = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| unreachable_error())?;
        Ok(Self {
            config,
            endpoint,
            http,
        })
    }

    pub fn provider(&self) -> HostedProvider {
        self.config.provider
    }

    pub fn model(&self) -> &str {
        &self.config.model
    }

    pub fn endpoint(&self) -> &Url {
        &self.endpoint
    }

    /// Sends the calibration document the local model is checked with, so a
    /// wrong key, model name, or address is found before a real document is.
    pub fn probe(&self) -> EngineResult<DocumentAnalysis> {
        let engine = Engine::with_proposer(Box::new(self.clone()));
        let mut last = None;
        for probe in semantic_probes()? {
            let analysis = engine.analyze(&probe.document, "pdf", &[])?;
            validate_semantic_probe(&probe, &analysis)?;
            last = Some(analysis);
        }
        last.ok_or_else(|| {
            EngineError::new(
                EngineErrorCode::ModelSelfTestFailed,
                "no calibration document to probe with",
            )
        })
    }

    /// The request body for one document, in the provider's shape.
    pub(crate) fn request_body(&self, request: &ModelRequest) -> Value {
        match self.config.provider {
            HostedProvider::Anthropic => json!({
                "model": self.config.model,
                "max_tokens": MAX_REPLY_TOKENS,
                "system": SYSTEM_INSTRUCTION,
                "messages": [{"role": "user", "content": request.prompt}],
            }),
            HostedProvider::OpenAiCompatible => json!({
                "model": self.config.model,
                "messages": [
                    {"role": "system", "content": SYSTEM_INSTRUCTION},
                    {"role": "user", "content": request.prompt}
                ],
                "stream": false,
            }),
        }
    }

    fn propose_once(&self, request: &ModelRequest) -> Result<ModelProposal, AttemptError> {
        let post = self.http.post(self.endpoint.clone());
        let post = match self.config.provider {
            HostedProvider::Anthropic => post
                .header("x-api-key", &self.config.api_key)
                .header("anthropic-version", ANTHROPIC_VERSION),
            HostedProvider::OpenAiCompatible => post.bearer_auth(&self.config.api_key),
        };
        let response = post
            .json(&self.request_body(request))
            .send()
            .map_err(|_| AttemptError(EngineErrorCode::HostedModelUnreachable))?;
        let status = response.status();
        if !status.is_success() {
            return Err(AttemptError(failure_for_status(status)));
        }
        let bytes = response
            .bytes()
            .map_err(|_| AttemptError(EngineErrorCode::HostedModelUnreachable))?;
        match self.config.provider {
            HostedProvider::Anthropic => decode_anthropic(&bytes),
            HostedProvider::OpenAiCompatible => {
                let completion: ChatCompletion = serde_json::from_slice(&bytes)
                    .map_err(|_| AttemptError(EngineErrorCode::ModelResponseInvalid))?;
                decode(completion)
            }
        }
    }
}

impl Proposer for HostedClient {
    /// One attempt, then one retry when the reply was malformed; a refused
    /// key or an unreachable service is not retried, because the second
    /// answer would be the same and the first is the one to report.
    fn propose(&self, request: &ModelRequest) -> EngineResult<ModelProposal> {
        match self.propose_once(request) {
            Ok(proposal) => Ok(proposal),
            Err(AttemptError(EngineErrorCode::ModelResponseInvalid)) => self
                .propose_once(request)
                .map_err(|AttemptError(code)| hosted_error(code)),
            Err(AttemptError(code)) => Err(hosted_error(code)),
        }
    }
}

/// What an HTTP failure means to the person who has to fix it.
pub(crate) fn failure_for_status(status: StatusCode) -> EngineErrorCode {
    match status.as_u16() {
        401 | 403 => EngineErrorCode::HostedModelUnauthorized,
        429 => EngineErrorCode::HostedModelRateLimited,
        400..=499 => EngineErrorCode::HostedModelRejected,
        _ => EngineErrorCode::HostedModelUnreachable,
    }
}

#[derive(Deserialize)]
struct AnthropicMessage {
    #[serde(default)]
    content: Vec<AnthropicBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct AnthropicBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
}

/// Reads a proposal out of a Messages API reply. Thinking blocks are passed
/// over; only the text blocks carry the answer. A refusal is reported as
/// what it is rather than as a malformed reply, so it is not retried and the
/// document goes to review with the reason shown.
pub(crate) fn decode_anthropic(bytes: &[u8]) -> Result<ModelProposal, AttemptError> {
    let message: AnthropicMessage = serde_json::from_slice(bytes)
        .map_err(|_| AttemptError(EngineErrorCode::ModelResponseInvalid))?;
    match message.stop_reason.as_deref() {
        Some("refusal") => return Err(AttemptError(EngineErrorCode::HostedModelRefused)),
        Some("max_tokens") => return Err(AttemptError(EngineErrorCode::ModelResponseInvalid)),
        _ => {}
    }
    let text = message
        .content
        .into_iter()
        .filter(|block| block.kind == "text")
        .filter_map(|block| block.text)
        .collect::<Vec<_>>()
        .join("");
    if text.trim().is_empty() {
        return Err(AttemptError(EngineErrorCode::ModelResponseInvalid));
    }
    proposal_from_text(&text)
}

const fn hosted_error(code: EngineErrorCode) -> EngineError {
    match code {
        EngineErrorCode::HostedModelUnauthorized => EngineError::new(
            EngineErrorCode::HostedModelUnauthorized,
            "the hosted service rejected the API key",
        ),
        EngineErrorCode::HostedModelRateLimited => EngineError::new(
            EngineErrorCode::HostedModelRateLimited,
            "the hosted service asked for a slower pace",
        ),
        EngineErrorCode::HostedModelRejected => EngineError::new(
            EngineErrorCode::HostedModelRejected,
            "the hosted service rejected the request",
        ),
        EngineErrorCode::HostedModelRefused => EngineError::new(
            EngineErrorCode::HostedModelRefused,
            "the hosted model declined to answer about this document",
        ),
        EngineErrorCode::HostedModelUnreachable => unreachable_error(),
        EngineErrorCode::HostedModelMisconfigured => {
            misconfigured("the hosted model is not configured")
        }
        _ => EngineError::new(
            EngineErrorCode::ModelResponseInvalid,
            "the hosted model returned malformed output twice",
        ),
    }
}

const fn misconfigured(message: &'static str) -> EngineError {
    EngineError::new(EngineErrorCode::HostedModelMisconfigured, message)
}

const fn unreachable_error() -> EngineError {
    EngineError::new(
        EngineErrorCode::HostedModelUnreachable,
        "the hosted service could not be reached",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(provider: HostedProvider, base_url: &str, model: &str) -> HostedModelConfig {
        HostedModelConfig {
            provider,
            base_url: base_url.into(),
            model: model.into(),
            api_key: "sk-test".into(),
        }
    }

    #[test]
    fn empty_fields_take_the_providers_defaults_and_a_missing_key_is_refused() {
        let resolved = config(HostedProvider::Anthropic, "", "")
            .resolved()
            .unwrap();
        assert_eq!(resolved.base_url, DEFAULT_ANTHROPIC_BASE_URL);
        assert_eq!(resolved.model, DEFAULT_ANTHROPIC_MODEL);

        let openai = config(HostedProvider::OpenAiCompatible, "", "gpt-x")
            .resolved()
            .unwrap();
        assert_eq!(openai.base_url, DEFAULT_OPENAI_BASE_URL);

        let no_model = config(HostedProvider::OpenAiCompatible, "", "  ").resolved();
        assert_eq!(
            no_model.unwrap_err().code(),
            EngineErrorCode::HostedModelMisconfigured
        );
        let mut no_key = config(HostedProvider::Anthropic, "", "");
        no_key.api_key = "   ".into();
        assert_eq!(
            no_key.resolved().unwrap_err().code(),
            EngineErrorCode::HostedModelMisconfigured
        );
    }

    #[test]
    fn endpoints_follow_the_provider_and_tolerate_a_trailing_slash() {
        assert_eq!(
            endpoint_for(HostedProvider::Anthropic, "https://api.anthropic.com/v1/")
                .unwrap()
                .as_str(),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            endpoint_for(
                HostedProvider::OpenAiCompatible,
                "https://api.openai.com/v1"
            )
            .unwrap()
            .as_str(),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            endpoint_for(
                HostedProvider::OpenAiCompatible,
                "https://gateway.example.com"
            )
            .unwrap()
            .as_str(),
            "https://gateway.example.com/chat/completions"
        );
    }

    #[test]
    fn plain_http_is_allowed_only_to_this_machine() {
        assert!(
            endpoint_for(
                HostedProvider::OpenAiCompatible,
                "http://localhost:11434/v1"
            )
            .is_ok()
        );
        assert!(endpoint_for(HostedProvider::OpenAiCompatible, "http://127.0.0.1:1234/v1").is_ok());
        assert!(endpoint_for(HostedProvider::OpenAiCompatible, "http://[::1]:1234/v1").is_ok());
        for bad in [
            "http://api.example.com/v1",
            "ftp://api.example.com/v1",
            "not a url",
            "",
        ] {
            assert_eq!(
                endpoint_for(HostedProvider::OpenAiCompatible, bad)
                    .unwrap_err()
                    .code(),
                EngineErrorCode::HostedModelMisconfigured,
                "{bad:?}"
            );
        }
    }

    #[test]
    fn an_anthropic_request_is_a_messages_call_with_no_sampling_knobs() {
        let client = HostedClient::new(config(HostedProvider::Anthropic, "", "")).unwrap();
        let body = client.request_body(&ModelRequest {
            prompt: "File this.".into(),
        });
        assert_eq!(body["model"], json!(DEFAULT_ANTHROPIC_MODEL));
        assert_eq!(body["max_tokens"], json!(MAX_REPLY_TOKENS));
        assert_eq!(body["system"], json!(SYSTEM_INSTRUCTION));
        assert_eq!(body["messages"][0]["role"], json!("user"));
        assert_eq!(body["messages"][0]["content"], json!("File this."));
        // Current Anthropic models reject temperature and top_k outright, and
        // the grammar is the local server's alone.
        for absent in ["temperature", "top_k", "grammar", "thinking", "stream"] {
            assert!(body.get(absent).is_none(), "{absent} must not be sent");
        }
        assert_eq!(
            client.endpoint().as_str(),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn an_openai_compatible_request_is_a_plain_chat_completion() {
        let client = HostedClient::new(config(
            HostedProvider::OpenAiCompatible,
            "http://localhost:1234/v1",
            "local-model",
        ))
        .unwrap();
        let body = client.request_body(&ModelRequest {
            prompt: "File this.".into(),
        });
        assert_eq!(body["model"], json!("local-model"));
        assert_eq!(body["messages"][0]["role"], json!("system"));
        assert_eq!(body["messages"][1]["content"], json!("File this."));
        assert_eq!(body["stream"], json!(false));
        assert!(body.get("temperature").is_none());
        assert!(body.get("max_tokens").is_none(), "left to the server");
        assert_eq!(
            client.endpoint().as_str(),
            "http://localhost:1234/v1/chat/completions"
        );
    }

    #[test]
    fn an_anthropic_reply_is_read_from_its_text_blocks_only() {
        let reply = br#"{"content":[{"type":"thinking","thinking":"..."},{"type":"text","text":"```json\n{\"document_type\":\"Invoice\",\"document_date\":\"2026-03-02\",\"date_role\":\"invoice\",\"parties\":[\"Acme\"],\"party_relation\":\"from\",\"description\":\"An invoice.\",\"confidence\":0.9,\"needs_review\":false}\n```"}],"stop_reason":"end_turn"}"#;
        let proposal = decode_anthropic(reply).unwrap();
        assert_eq!(proposal.document_type.as_deref(), Some("Invoice"));
        assert_eq!(proposal.document_date.as_deref(), Some("2026-03-02"));
        assert_eq!(proposal.parties, vec!["Acme".to_string()]);
    }

    #[test]
    fn a_refusal_and_a_truncated_reply_are_named_not_retried_as_malformed() {
        let refused = br#"{"content":[],"stop_reason":"refusal","stop_details":{"type":"refusal","category":"cyber"}}"#;
        assert_eq!(
            decode_anthropic(refused).unwrap_err().0,
            EngineErrorCode::HostedModelRefused
        );
        let truncated = br#"{"content":[{"type":"text","text":"{\"document_type\":"}],"stop_reason":"max_tokens"}"#;
        assert_eq!(
            decode_anthropic(truncated).unwrap_err().0,
            EngineErrorCode::ModelResponseInvalid
        );
        assert_eq!(
            decode_anthropic(b"not json").unwrap_err().0,
            EngineErrorCode::ModelResponseInvalid
        );
    }

    #[test]
    fn http_statuses_map_to_the_codes_a_person_can_act_on() {
        assert_eq!(
            failure_for_status(StatusCode::UNAUTHORIZED),
            EngineErrorCode::HostedModelUnauthorized
        );
        assert_eq!(
            failure_for_status(StatusCode::FORBIDDEN),
            EngineErrorCode::HostedModelUnauthorized
        );
        assert_eq!(
            failure_for_status(StatusCode::TOO_MANY_REQUESTS),
            EngineErrorCode::HostedModelRateLimited
        );
        assert_eq!(
            failure_for_status(StatusCode::NOT_FOUND),
            EngineErrorCode::HostedModelRejected
        );
        assert_eq!(
            failure_for_status(StatusCode::BAD_REQUEST),
            EngineErrorCode::HostedModelRejected
        );
        assert_eq!(
            failure_for_status(StatusCode::INTERNAL_SERVER_ERROR),
            EngineErrorCode::HostedModelUnreachable
        );
        assert_eq!(
            failure_for_status(StatusCode::from_u16(529).unwrap()),
            EngineErrorCode::HostedModelUnreachable
        );
    }

    #[test]
    fn the_client_never_prints_its_key() {
        let client = HostedClient::new(config(HostedProvider::Anthropic, "", "")).unwrap();
        let debug = format!("{client:?}");
        assert!(!debug.contains("sk-test"));
        assert!(debug.contains("[redacted]"));
    }
}
