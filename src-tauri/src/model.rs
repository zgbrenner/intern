//! Which model reads documents, and the switch between them.
//!
//! The local model is the product and the default. A hosted model behind an
//! API key is an alternative a person chooses in Settings, knowing that the
//! distilled text of every document will leave the machine for the service
//! they named. Everything on either side of the model is shared: the same
//! distillation goes out, the same evidence checks are applied to what comes
//! back, and the queue never learns which one answered.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

use intern_engine::{
    DocumentAnalysis, DocumentSource, Engine, EngineError, EngineErrorCode, HostedClient,
    HostedModelConfig, HostedProvider, evidence::is_valid_iso_date, hosted::endpoint_for,
};
use intern_queue::{AnalyzerBoundary, AppSettings, ModelFailure, ModelSource, SettingsStore};
use serde::Serialize;

use crate::{
    commands::CommandError,
    secrets::{HOSTED_MODEL_API_KEY, SecretStore, key_hint},
};

/// How long to wait before retrying after a hosted service asked for a
/// slower pace or briefly could not be reached.
const HOSTED_RETRY_DELAY: Duration = Duration::from_secs(8);

/// What Settings shows about the hosted model.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostedModelStatusDto {
    /// Whether a key is in the credential store.
    pub key_stored: bool,
    /// The tail of the stored key, so a person can tell which one it is.
    pub key_hint: Option<String>,
    /// The endpoint the saved settings resolve to, when they resolve.
    pub endpoint: Option<String>,
    /// Each provider's defaults, so the dialog can show what "empty" means.
    pub providers: Vec<ProviderDefaultsDto>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDefaultsDto {
    pub provider: HostedProvider,
    pub base_url: String,
    pub model: String,
}

/// The outcome of a successful test connection: the calibration document
/// went out, and a correct name came back.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostedModelTestDto {
    pub model: String,
    pub endpoint: String,
    pub filename: String,
    pub inference_millis: u64,
}

/// The hosted model as the app holds it: the settings' half of the
/// configuration plus the key from the credential store, and an engine
/// built lazily and kept while the configuration stands.
pub struct HostedModel {
    secrets: Arc<dyn SecretStore>,
    engine: Mutex<Option<(HostedModelConfig, Arc<Engine>)>>,
}

impl HostedModel {
    pub fn new(secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            secrets,
            engine: Mutex::new(None),
        }
    }

    /// The configuration the given settings and the stored key amount to,
    /// or why a request could not be made from them.
    pub fn config(&self, settings: &AppSettings) -> Result<HostedModelConfig, CommandError> {
        let api_key = self.stored_key()?.ok_or_else(|| CommandError {
            code: "HOSTED_MODEL_KEY_MISSING".into(),
            message: "no API key is stored for the hosted model".into(),
        })?;
        Ok(HostedModelConfig {
            provider: settings.hosted_provider,
            base_url: settings.hosted_base_url.clone(),
            model: settings.hosted_model.clone(),
            api_key,
        }
        .resolved()?)
    }

    /// Whether these settings and the stored key are enough to send with.
    pub fn configured(&self, settings: &AppSettings) -> bool {
        self.config(settings).is_ok()
    }

    pub fn status(&self, settings: &AppSettings) -> HostedModelStatusDto {
        let key = self.stored_key().ok().flatten();
        HostedModelStatusDto {
            key_stored: key.is_some(),
            key_hint: key.as_deref().map(key_hint),
            endpoint: self
                .config(settings)
                .ok()
                .and_then(|config| endpoint_for(config.provider, &config.base_url).ok())
                .map(|endpoint| endpoint.to_string()),
            providers: HostedProvider::ALL
                .iter()
                .map(|provider| ProviderDefaultsDto {
                    provider: *provider,
                    base_url: provider.default_base_url().to_owned(),
                    model: provider.default_model().to_owned(),
                })
                .collect(),
        }
    }

    /// Stores a key, replacing any earlier one. The engine built on the old
    /// key is dropped so the next document uses the new one.
    pub fn set_key(&self, key: &str) -> Result<(), CommandError> {
        let trimmed = key.trim();
        if trimmed.is_empty() {
            return Err(CommandError {
                code: "HOSTED_MODEL_KEY_EMPTY".into(),
                message: "the API key is empty".into(),
            });
        }
        self.secrets
            .set(HOSTED_MODEL_API_KEY, trimmed)
            .map_err(store_error)?;
        self.forget_engine();
        Ok(())
    }

    pub fn clear_key(&self) -> Result<(), CommandError> {
        self.secrets
            .delete(HOSTED_MODEL_API_KEY)
            .map_err(store_error)?;
        self.forget_engine();
        Ok(())
    }

    /// Sends the calibration document, the way the local model is checked
    /// at setup, so a wrong key, model, or address is found here rather than
    /// on someone's first real document.
    pub fn test(&self, settings: &AppSettings) -> Result<HostedModelTestDto, CommandError> {
        let client = HostedClient::new(self.config(settings)?)?;
        let analysis = client.probe()?;
        Ok(HostedModelTestDto {
            model: client.model().to_owned(),
            endpoint: client.endpoint().to_string(),
            filename: analysis.filename,
            inference_millis: analysis.telemetry.inference_millis,
        })
    }

    fn analyze(
        &self,
        settings: &AppSettings,
        source: &DocumentSource,
        extension: &str,
        existing_names: &[&str],
    ) -> Result<DocumentAnalysis, ModelFailure> {
        let config = self
            .config(settings)
            .map_err(|error| ModelFailure::fatal(error.code))?;
        let engine = self.engine(config).map_err(failure_for)?;
        engine
            .analyze(source, extension, existing_names)
            .map_err(failure_for)
    }

    fn engine(&self, config: HostedModelConfig) -> Result<Arc<Engine>, EngineError> {
        let mut cached = self
            .engine
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((current, engine)) = cached.as_ref()
            && *current == config
        {
            return Ok(Arc::clone(engine));
        }
        let client = HostedClient::new(config.clone())?;
        let engine = Arc::new(Engine::with_proposer(Box::new(client)));
        *cached = Some((config, Arc::clone(&engine)));
        Ok(engine)
    }

    fn forget_engine(&self) {
        *self
            .engine
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    fn stored_key(&self) -> Result<Option<String>, CommandError> {
        self.secrets.get(HOSTED_MODEL_API_KEY).map_err(store_error)
    }
}

fn store_error(detail: String) -> CommandError {
    CommandError {
        code: "SECRET_STORE_UNAVAILABLE".into(),
        message: format!("the credential store could not be used ({detail})"),
    }
}

/// How a hosted failure reads to the queue: a refused key or a declined
/// document will not change on a retry, so those are fatal; a busy or
/// unreachable service and a malformed reply are worth one more attempt.
pub(crate) fn failure_for(error: EngineError) -> ModelFailure {
    match error.code() {
        EngineErrorCode::HostedModelRateLimited
        | EngineErrorCode::HostedModelUnreachable
        | EngineErrorCode::ModelResponseInvalid => ModelFailure::retryable(error.code().as_str()),
        _ => ModelFailure::fatal(error.code().as_str()),
    }
}

/// The date the model proposed but validation withheld - because the
/// document never states it verbatim - offered to the reviewer to accept
/// with one click. Nothing is offered when a date was accepted, or when the
/// model gave none.
pub(crate) fn suggested_date(analysis: &DocumentAnalysis) -> Option<String> {
    if analysis.proposal.document_date.is_some() {
        return None;
    }
    analysis
        .model_proposal
        .as_ref()?
        .document_date
        .as_deref()
        .map(str::trim)
        .filter(|date| is_valid_iso_date(date))
        .map(str::to_owned)
}

const IDLE: u8 = 0;
const LOCAL: u8 = 1;
const HOSTED: u8 = 2;

/// The queue's model: whichever one the settings name at the moment a
/// document is analysed, so a change in Settings takes effect at the next
/// document with nothing restarted.
pub struct SwitchingModel<L: AnalyzerBoundary> {
    local: Arc<L>,
    hosted: Arc<HostedModel>,
    settings: SettingsStore,
    active: AtomicU8,
}

impl<L: AnalyzerBoundary> SwitchingModel<L> {
    pub fn new(local: Arc<L>, hosted: Arc<HostedModel>, settings: SettingsStore) -> Self {
        Self {
            local,
            hosted,
            settings,
            active: AtomicU8::new(IDLE),
        }
    }
}

impl<L: AnalyzerBoundary> AnalyzerBoundary for SwitchingModel<L> {
    fn analyze(
        &self,
        source: &DocumentSource,
        extension: &str,
        existing_names: &[&str],
    ) -> Result<DocumentAnalysis, ModelFailure> {
        let settings = self
            .settings
            .load()
            .map_err(|error| ModelFailure::fatal(error.code))?;
        let (which, result) = match settings.model_source {
            ModelSource::Local => {
                self.active.store(LOCAL, Ordering::SeqCst);
                (LOCAL, self.local.analyze(source, extension, existing_names))
            }
            ModelSource::Hosted => {
                self.active.store(HOSTED, Ordering::SeqCst);
                (
                    HOSTED,
                    self.hosted
                        .analyze(&settings, source, extension, existing_names),
                )
            }
        };
        let _ = self
            .active
            .compare_exchange(which, IDLE, Ordering::SeqCst, Ordering::SeqCst);
        result
    }

    fn recover(&self, failure: &ModelFailure) -> Result<(), ModelFailure> {
        match failure.code.as_str() {
            "HOSTED_MODEL_RATE_LIMITED" | "HOSTED_MODEL_UNREACHABLE" => {
                std::thread::sleep(HOSTED_RETRY_DELAY);
                Ok(())
            }
            code if code.starts_with("HOSTED_MODEL_") => Ok(()),
            _ => self.local.recover(failure),
        }
    }

    /// A hosted request cannot be interrupted, only outwaited: the reply is
    /// discarded when it arrives. The local server is restarted as before.
    fn cancel(&self) -> Result<(), ModelFailure> {
        match self.active.load(Ordering::SeqCst) {
            HOSTED => Ok(()),
            _ => self.local.cancel(),
        }
    }

    fn shutdown(&self) -> Result<(), ModelFailure> {
        self.local.shutdown()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use intern_engine::{
        AnalysisTelemetry, DateRole, DocumentAnalysis, EngineError, EngineErrorCode, Evidence,
        ModelProposal, PartyRelation, ProposalStatus, ValidatedProposal,
    };
    use intern_queue::{AppSettings, ModelSource};

    use super::{HostedModel, failure_for, suggested_date};
    use crate::secrets::{HOSTED_MODEL_API_KEY, MemoryStore, SecretStore};

    fn analysis(accepted: Option<&str>, proposed: Option<&str>) -> DocumentAnalysis {
        let proposal = ValidatedProposal {
            document_type: Some("Invoice".into()),
            document_date: accepted.map(str::to_owned),
            date_role: accepted.map(|_| DateRole::Invoice),
            parties: vec!["Acme".into()],
            party_relation: PartyRelation::From,
            description: "An invoice from Acme.".into(),
            confidence: 0.8,
            evidence: Evidence::default(),
        };
        DocumentAnalysis {
            filename: "Invoice from Acme.pdf".into(),
            description: proposal.description.clone(),
            status: ProposalStatus::NeedsReview,
            review_reasons: Vec::new(),
            proposal,
            telemetry: AnalysisTelemetry::default(),
            model_proposal: Some(ModelProposal {
                document_type: Some("Invoice".into()),
                document_date: proposed.map(str::to_owned),
                date_role: proposed.map(|_| DateRole::Invoice),
                parties: vec!["Acme".into()],
                party_relation: PartyRelation::From,
                description: "An invoice from Acme.".into(),
                confidence: 0.8,
                needs_review: false,
                evidence: Evidence::default(),
            }),
        }
    }

    #[test]
    fn a_withheld_date_is_offered_and_an_accepted_or_absent_one_is_not() {
        assert_eq!(
            suggested_date(&analysis(None, Some("2026-03-02"))).as_deref(),
            Some("2026-03-02")
        );
        assert_eq!(
            suggested_date(&analysis(Some("2026-03-02"), Some("2026-03-02"))),
            None,
            "already in the filename"
        );
        assert_eq!(suggested_date(&analysis(None, None)), None);
        assert_eq!(
            suggested_date(&analysis(None, Some("2026-02-30"))),
            None,
            "never a day that does not exist"
        );
        let mut legacy = analysis(None, Some("2026-03-02"));
        legacy.model_proposal = None;
        assert_eq!(
            suggested_date(&legacy),
            None,
            "stored before replies were kept"
        );
    }

    #[test]
    fn only_a_busy_service_or_a_malformed_reply_earns_a_retry() {
        for code in [
            EngineErrorCode::HostedModelRateLimited,
            EngineErrorCode::HostedModelUnreachable,
            EngineErrorCode::ModelResponseInvalid,
        ] {
            assert!(
                failure_for(EngineError::new(code, "x")).retryable,
                "{code:?}"
            );
        }
        for code in [
            EngineErrorCode::HostedModelUnauthorized,
            EngineErrorCode::HostedModelMisconfigured,
            EngineErrorCode::HostedModelRejected,
            EngineErrorCode::HostedModelRefused,
        ] {
            let failure = failure_for(EngineError::new(code, "x"));
            assert!(!failure.retryable, "{code:?}");
            assert_eq!(failure.code, code.as_str());
        }
    }

    #[test]
    fn the_hosted_model_is_configured_only_with_a_key_and_a_usable_address() {
        let secrets: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
        let hosted = HostedModel::new(Arc::clone(&secrets));
        let settings = AppSettings {
            model_source: ModelSource::Hosted,
            ..AppSettings::default()
        };

        assert!(!hosted.configured(&settings));
        assert_eq!(
            hosted.config(&settings).unwrap_err().code,
            "HOSTED_MODEL_KEY_MISSING"
        );
        let status = hosted.status(&settings);
        assert!(!status.key_stored);
        assert_eq!(status.endpoint, None);
        assert_eq!(status.providers.len(), 2);

        assert_eq!(
            hosted.set_key("   ").unwrap_err().code,
            "HOSTED_MODEL_KEY_EMPTY"
        );
        hosted.set_key("  sk-ant-api03-example-key-0042  ").unwrap();
        assert_eq!(
            secrets.get(HOSTED_MODEL_API_KEY).unwrap().as_deref(),
            Some("sk-ant-api03-example-key-0042"),
            "stored trimmed, in the credential store, never in settings"
        );
        assert!(hosted.configured(&settings));
        let status = hosted.status(&settings);
        assert!(status.key_stored);
        assert_eq!(status.key_hint.as_deref(), Some("…0042"));
        assert_eq!(
            status.endpoint.as_deref(),
            Some("https://api.anthropic.com/v1/messages")
        );

        let mut plain_http = settings.clone();
        plain_http.hosted_base_url = "http://api.example.com/v1".into();
        assert_eq!(
            hosted.config(&plain_http).unwrap_err().code,
            "HOSTED_MODEL_MISCONFIGURED"
        );

        hosted.clear_key().unwrap();
        assert!(!hosted.configured(&settings));
        assert_eq!(secrets.get(HOSTED_MODEL_API_KEY).unwrap(), None);
    }
}
