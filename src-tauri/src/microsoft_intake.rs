//! Microsoft account connection, upload authorization, and attribution for the
//! desktop host. Credentials never cross IPC or enter the shared folder.
use crate::secrets::{KeyringStore, SecretStore};
use intern_intake::microsoft::{
    Account, AuthConfig, DevicePrompt, FolderBinding, MicrosoftClient, SignInProgress, TokenStore,
    audit::AuditVerifier,
    hashing::verified_local_hash,
    proof::{candidate, same_person},
    transport::item_url,
};
use intern_intake::{classify, detect_cloud_roots, relative_to_root};
use intern_queue::{
    AdmissionGuard, AdmissionStage, AppSettings, FiledDocument, FilingSink, PipelineError,
    PipelineResult, SettingsStore,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use tauri::State;

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublicConfig {
    #[serde(default)]
    auth: AuthConfig,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    bindings: Vec<FolderBinding>,
    #[serde(default)]
    protected_roots: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attribution {
    pub path: String,
    pub filename: String,
    pub state: String,
    pub reason: String,
    pub uploader: Option<Account>,
    pub processed_by: Option<Account>,
    pub filed_as: Option<String>,
    pub checked_at: i64,
    #[serde(skip)]
    source_hash: Option<String>,
    #[serde(skip)]
    authorized_processor: Option<Account>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MicrosoftStatus {
    pub connected: bool,
    pub account: Option<Account>,
    pub tenant_id: String,
    pub client_id: String,
    pub binding: Option<FolderBinding>,
    pub documents: Vec<Attribution>,
    pub error: Option<String>,
}
struct CredentialStore;
impl TokenStore for CredentialStore {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        KeyringStore
            .get(&format!("microsoft-intake:{key}"))
            .map_err(|_| "The operating system could not read Microsoft credentials.".into())
    }
    fn set(&self, key: &str, value: &str) -> Result<(), String> {
        KeyringStore.set(&format!("microsoft-intake:{key}"),value).map_err(|_| "Microsoft credentials could not be protected by the operating system. Sign-in was not saved.".into())
    }
    fn delete(&self, key: &str) -> Result<(), String> {
        KeyringStore.delete(&format!("microsoft-intake:{key}")).map_err(|_| "Microsoft was disconnected, but the old credential could not be deleted from the operating system store.".into())
    }
}

pub struct MicrosoftIntake {
    settings: SettingsStore,
    data: PathBuf,
    config: Mutex<PublicConfig>,
    config_error: Option<String>,
    client: Option<MicrosoftClient>,
    audit: AuditVerifier,
    generation: AtomicU64,
    documents: Mutex<BTreeMap<String, Attribution>>,
}
impl MicrosoftIntake {
    pub fn new(settings: SettingsStore, data: PathBuf) -> Self {
        let loaded = read_config(&data.join("microsoft-intake.json"));
        let (config, mut config_error) = match loaded {
            Ok(value) => (value, None),
            Err(error) => (PublicConfig::default(), Some(error)),
        };
        let client = match MicrosoftClient::new(config.auth.clone(), Arc::new(CredentialStore)) {
            Ok(client) => Some(client),
            Err(error) => {
                config_error = Some(error);
                None
            }
        };
        let documents = fs::read(data.join("intake-attribution.json"))
            .ok()
            .filter(|bytes| bytes.len() <= 1024 * 1024)
            .and_then(|bytes| serde_json::from_slice::<Vec<Attribution>>(&bytes).ok())
            .unwrap_or_default()
            .into_iter()
            .take(256)
            .map(|record| (record.path.clone(), record))
            .collect();
        Self {
            settings,
            data,
            config: Mutex::new(config),
            config_error,
            client,
            audit: AuditVerifier::default(),
            generation: AtomicU64::new(0),
            documents: Mutex::new(documents),
        }
    }
    fn client(&self) -> Result<&MicrosoftClient, String> {
        self.client
            .as_ref()
            .ok_or("Microsoft connection is unavailable.".into())
    }
    pub fn status(&self) -> MicrosoftStatus {
        let config = self
            .config
            .lock()
            .map(|value| value.clone())
            .unwrap_or_default();
        let settings = self.settings.load().unwrap_or_default();
        let account = if config.enabled {
            self.client.as_ref().and_then(MicrosoftClient::account)
        } else {
            None
        };
        let binding = config
            .bindings
            .iter()
            .find(|binding| same_path(&binding.local_folder, &settings.intake_folder))
            .cloned();
        let documents = self
            .documents
            .lock()
            .map(|rows| rows.values().rev().take(100).cloned().collect())
            .unwrap_or_default();
        MicrosoftStatus {
            connected: config.enabled,
            account,
            tenant_id: config.auth.tenant_id,
            client_id: config.auth.client_id,
            binding,
            documents,
            error: self.config_error.clone(),
        }
    }
    fn save(&self, config: &PublicConfig) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(config)
            .map_err(|_| "Microsoft configuration could not be encoded.")?;
        atomic_write(&self.data.join("microsoft-intake.json"), &bytes)
    }
    /// Remember strict intake roots before saving settings. Disabling watching
    /// or choosing a new root cannot bypass checks on already queued documents.
    pub fn protect_settings(&self, settings: &AppSettings) -> Result<(), String> {
        if let Some(error) = &self.config_error {
            return Err(error.clone());
        }
        if settings.intake_local_only
            && classify(Path::new(&settings.intake_folder), &detect_cloud_roots()).is_some()
        {
            return Err("Local-only intake cannot be used for OneDrive, SharePoint, or a network share. Microsoft upload verification is required.".into());
        }
        self.generation.fetch_add(1, Ordering::SeqCst);
        if settings.intake_folder.trim().is_empty() || settings.intake_local_only {
            return Ok(());
        }
        let mut config = self
            .config
            .lock()
            .map_err(|_| "Microsoft configuration is unavailable.")?;
        if !config
            .protected_roots
            .iter()
            .any(|root| same_path(root, &settings.intake_folder))
        {
            let mut next = config.clone();
            next.protected_roots.push(settings.intake_folder.clone());
            self.save(&next)?;
            *config = next;
        }
        Ok(())
    }
    pub fn begin(&self, auth: AuthConfig, acknowledge: bool) -> Result<DevicePrompt, String> {
        if !acknowledge {
            return Err("Confirm the Microsoft permissions notice before connecting. Audit read access is broader than the selected folder.".into());
        }
        auth.validate()?;
        if let Some(error) = &self.config_error {
            return Err(error.clone());
        }
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.audit.clear();
        {
            let mut config = self
                .config
                .lock()
                .map_err(|_| "Microsoft configuration is unavailable.")?;
            let mut next = config.clone();
            next.auth = auth.clone();
            next.enabled = false;
            self.save(&next)?;
            *config = next;
        }
        self.client()?.disconnect()?;
        self.client()?.begin(auth)
    }
    pub fn poll(&self) -> Result<SignInProgress, String> {
        let epoch = self.generation.load(Ordering::SeqCst);
        let result = self.client()?.poll()?;
        if matches!(result, SignInProgress::Connected { .. }) {
            let mut config = self
                .config
                .lock()
                .map_err(|_| "Microsoft configuration is unavailable.")?;
            if self.generation.load(Ordering::SeqCst) != epoch {
                return Err("Microsoft sign-in changed or was canceled. Files remain held.".into());
            }
            let mut next = config.clone();
            next.enabled = true;
            self.save(&next)?;
            *config = next;
        }
        Ok(result)
    }
    pub fn disconnect(&self) -> Result<(), String> {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.audit.clear();
        // Disable in memory first, even if disk or credential deletion fails.
        let persist = {
            let mut config = self
                .config
                .lock()
                .map_err(|_| "Microsoft configuration is unavailable.")?;
            config.enabled = false;
            self.save(&config)
        };
        let credentials = self.client()?.disconnect();
        persist?;
        credentials
    }
    pub fn bind(&self, drive: &str, folder: &str) -> Result<FolderBinding, String> {
        let epoch = self.generation.load(Ordering::SeqCst);
        let settings = self.settings.load().map_err(|error| error.to_string())?;
        if settings.intake_folder.trim().is_empty() || settings.intake_local_only {
            return Err("Save a Microsoft intake folder in Settings before pairing it.".into());
        }
        if !self
            .config
            .lock()
            .map_err(|_| "Microsoft configuration is unavailable.")?
            .enabled
        {
            return Err("Connect Microsoft before pairing a folder.".into());
        }
        let local = Path::new(&settings.intake_folder)
            .canonicalize()
            .map_err(|_| "The saved intake folder could not be found.")?;
        if !local.is_dir() {
            return Err("The saved intake path is not a folder.".into());
        }
        let (account, remote) = self.client()?.metadata(item_url(drive, folder, None)?)?;
        if !remote["folder"].is_object()
            || remote
                .get("remoteItem")
                .is_some_and(|value| !value.is_null())
            || remote["id"].as_str() != Some(folder)
            || remote
                .pointer("/sharepointIds/tenantId")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|tenant| !tenant.eq_ignore_ascii_case(&account.tenant_id))
        {
            return Err("Microsoft did not confirm a work/school folder in the connected organization. Personal accounts and shortcut items are not supported.".into());
        }
        let web_url = remote["webUrl"]
            .as_str()
            .filter(|url| valid_sharepoint_url(url))
            .ok_or("Microsoft did not return a supported folder address.")?
            .to_owned();
        let binding = FolderBinding {
            local_folder: local.to_string_lossy().into_owned(),
            drive_id: drive.into(),
            folder_id: folder.into(),
            web_url,
            tenant_id: account.tenant_id,
        };
        let mut config = self
            .config
            .lock()
            .map_err(|_| "Microsoft configuration is unavailable.")?;
        if self.generation.load(Ordering::SeqCst) != epoch || !config.enabled {
            return Err("Microsoft connection changed while the folder was being paired.".into());
        }
        let current = self.settings.load().map_err(|error| error.to_string())?;
        if current.intake_folder != settings.intake_folder || current.intake_local_only {
            return Err("The saved intake folder changed. Pair it again.".into());
        }
        let mut next = config.clone();
        next.bindings
            .retain(|entry| !same_path(&entry.local_folder, &binding.local_folder));
        next.bindings.push(binding.clone());
        if !next
            .protected_roots
            .iter()
            .any(|root| same_path(root, &binding.local_folder))
        {
            next.protected_roots.push(binding.local_folder.clone());
        }
        self.save(&next)?;
        *config = next;
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.audit.clear();
        Ok(binding)
    }
    fn scope(&self, path: &Path, settings: &AppSettings) -> Result<Option<FolderBinding>, String> {
        if let Some(error) = &self.config_error {
            return Err(error.clone());
        }
        let config = self
            .config
            .lock()
            .map_err(|_| "Microsoft configuration is unavailable.")?;
        let mut protected = config.protected_roots.iter().any(|root| within(path, root));
        protected |= !settings.intake_local_only
            && !settings.intake_folder.trim().is_empty()
            && within(path, &settings.intake_folder);
        if !protected {
            return Ok(None);
        }
        if !config.enabled {
            return Err(
                "Microsoft is disconnected. Unverified uploads are never processed.".into(),
            );
        }
        let binding=config.bindings.iter().filter(|binding|within(path,&binding.local_folder)).max_by_key(|binding|binding.local_folder.len()).cloned()
            .ok_or("Pair the saved intake folder with its Microsoft drive and folder IDs. Unverified uploads remain held.")?;
        Ok(Some(binding))
    }
    fn verify(
        &self,
        path: &Path,
        settings: &AppSettings,
    ) -> Result<Option<(String, Account, Account)>, String> {
        let epoch = self.generation.load(Ordering::SeqCst);
        let Some(binding) = self.scope(path, settings)? else {
            return Ok(None);
        };
        let relative = relative_to_root(path, Path::new(&binding.local_folder))
            .ok_or("The file is not inside the paired intake folder.")?;
        let leaf = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("The filename cannot be verified.")?;
        let url = item_url(&binding.drive_id, &binding.folder_id, Some(&relative))?;
        let client = self.client()?;
        let (account, first) = client.metadata(url.clone())?;
        if !account.tenant_id.eq_ignore_ascii_case(&binding.tenant_id) {
            return Err(
                "The connected account belongs to a different organization than this folder."
                    .into(),
            );
        }
        let document = candidate(&first, &account, leaf).map_err(str::to_owned)?;
        if !valid_sharepoint_url(&document.web_url) {
            return Err("The Microsoft file address is unsupported.".into());
        }
        let uploader = self
            .audit
            .verify(client, &document, crate::intake::now_unix())?;
        if !same_person(&account, &uploader) && !settings.process_others_uploads {
            self.note(path,"other",Some(uploader),None,None,"Uploaded by another Microsoft account. This installation processes only your uploads.");
            return Err(
                "UPLOADER_OTHER: this verified upload belongs to a different Microsoft account."
                    .into(),
            );
        }
        // The first content read happens only after the actual upload actor is
        // established. The checksum binds local sync bytes, not account identity.
        let hash=verified_local_hash(path,document.size,&document.quick_xor).map_err(|_| "The local file does not yet match Microsoft's file revision. It remains held while sync finishes.")?;
        let (current, second) = client.metadata(url)?;
        let again = candidate(&second, &current, leaf).map_err(str::to_owned)?;
        if document != again
            || !same_person(&account, &current)
            || self.generation.load(Ordering::SeqCst) != epoch
        {
            return Err("The file, Microsoft identity, or connection changed during verification. It remains held.".into());
        }
        let latest = self.settings.load().map_err(|error| error.to_string())?;
        if latest.intake_folder != settings.intake_folder
            || latest.process_others_uploads != settings.process_others_uploads
            || latest.intake_local_only != settings.intake_local_only
        {
            return Err("Intake scope changed during verification. The file remains held.".into());
        }
        Ok(Some((hash, uploader, account)))
    }
    fn note(
        &self,
        path: &Path,
        state: &str,
        uploader: Option<Account>,
        processor: Option<Account>,
        hash: Option<String>,
        reason: &str,
    ) {
        if let Ok(mut rows) = self.documents.lock() {
            let key = path.to_string_lossy().into_owned();
            let prior = rows.get(&key).cloned();
            let same = prior
                .as_ref()
                .is_some_and(|record| record.source_hash == hash && hash.is_some());
            let record = Attribution {
                path: key.clone(),
                filename: path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                state: state.into(),
                reason: reason.into(),
                uploader,
                processed_by: if same {
                    prior
                        .as_ref()
                        .and_then(|record| record.processed_by.clone())
                } else {
                    None
                },
                filed_as: if same {
                    prior.as_ref().and_then(|record| record.filed_as.clone())
                } else {
                    None
                },
                checked_at: crate::intake::now_unix(),
                source_hash: hash,
                authorized_processor: processor,
            };
            rows.insert(key, record);
            while rows.len() > 256 {
                if let Some(oldest) = rows
                    .iter()
                    .min_by_key(|(_, record)| record.checked_at)
                    .map(|(key, _)| key.clone())
                {
                    rows.remove(&oldest);
                }
            }
        }
    }
    fn persist_attribution(&self) {
        if let Ok(rows) = self.documents.lock()
            && let Ok(bytes) = serde_json::to_vec_pretty(&rows.values().collect::<Vec<_>>())
        {
            let _ = atomic_write(&self.data.join("intake-attribution.json"), &bytes);
        }
    }
}
impl AdmissionGuard for MicrosoftIntake {
    fn authorize(&self, path: &Path, _stage: AdmissionStage) -> PipelineResult<Option<String>> {
        let settings = self.settings.load()?;
        match self.verify(path, &settings) {
            Ok(None) => Ok(None),
            Ok(Some((hash, uploader, processor))) => {
                self.note(
                    path,
                    "verified",
                    Some(uploader),
                    Some(processor),
                    Some(hash.clone()),
                    "Uploader verified against Microsoft upload activity.",
                );
                Ok(Some(hash))
            }
            Err(message) => {
                if !message.starts_with("UPLOADER_OTHER:") {
                    self.note(path, "unknown", None, None, None, &message);
                }
                Err(PipelineError::new(
                    if message.starts_with("UPLOADER_OTHER:") {
                        "UPLOADER_OTHER"
                    } else {
                        "UPLOADER_UNVERIFIED"
                    },
                    message,
                ))
            }
        }
    }
    fn processed(&self, path: &Path, hash: &str) {
        if let Ok(mut rows) = self.documents.lock()
            && let Some(record) = rows.get_mut(&path.to_string_lossy().into_owned())
            && record.source_hash.as_deref() == Some(hash)
        {
            record.processed_by = record.authorized_processor.clone();
            record.state = "processed".into();
            record.reason = "Analysis completed on this installation.".into();
        }
        self.persist_attribution();
    }
}
impl FilingSink for MicrosoftIntake {
    fn unfiled(&self, document: &intern_queue::UnfiledDocument) {
        if let Ok(mut rows) = self.documents.lock()
            && let Some(record) = rows.get_mut(&document.source_path.to_string_lossy().into_owned())
        {
            record.filed_as = None;
            record.state = "processed".into();
            record.reason = "Filing was undone; the original file was restored.".into();
        }
        self.persist_attribution();
    }
    fn filed(&self, document: &FiledDocument) {
        if let Ok(mut rows) = self.documents.lock()
            && let Some(record) = rows.get_mut(&document.source_path.to_string_lossy().into_owned())
            && record.source_hash.as_deref() == Some(&document.source_hash)
        {
            record.filed_as = document
                .destination
                .file_name()
                .map(|name| name.to_string_lossy().into_owned());
            record.state = "filed".into();
            record.reason = "The verified document was filed.".into();
        }
        self.persist_attribution();
    }
}
fn within(path: &Path, root: &str) -> bool {
    !root.trim().is_empty()
        && (same_path(&path.to_string_lossy(), root)
            || relative_to_root(path, Path::new(root)).is_some())
}
fn same_path(left: &str, right: &str) -> bool {
    left.replace('\\', "/")
        .trim_end_matches('/')
        .eq_ignore_ascii_case(right.replace('\\', "/").trim_end_matches('/'))
}
fn valid_sharepoint_url(value: &str) -> bool {
    reqwest_url(value)
}
fn reqwest_url(value: &str) -> bool {
    // Parsing is kept in the intake crate so this host does not gain a second
    // direct HTTP dependency. URLs are provider data, never authorization keys.
    intern_intake::microsoft::transport::sharepoint_url(value)
}
fn read_config(path: &Path) -> Result<PublicConfig, String> {
    match fs::read(path){Ok(bytes) if bytes.len()<=64*1024=>serde_json::from_slice(&bytes).map_err(|_| "Microsoft intake configuration is invalid. Processing is held until it is repaired.".into()),Ok(_)=>Err("Microsoft intake configuration is too large. Processing is held.".into()),Err(error) if error.kind()==std::io::ErrorKind::NotFound=>Ok(PublicConfig::default()),Err(_)=>Err("Microsoft intake configuration is unreadable. Processing is held.".into())}
}
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, bytes).map_err(|_| "Microsoft intake state could not be saved.")?;
    fs::rename(temporary, path).map_err(|_| "Microsoft intake state could not be saved.".into())
}

#[tauri::command]
pub fn microsoft_intake_status(state: State<'_, Arc<MicrosoftIntake>>) -> MicrosoftStatus {
    state.status()
}
#[tauri::command]
pub async fn microsoft_sign_in_start(
    config: AuthConfig,
    acknowledge_audit_access: bool,
    state: State<'_, Arc<MicrosoftIntake>>,
) -> Result<DevicePrompt, String> {
    let manager = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || manager.begin(config, acknowledge_audit_access))
        .await
        .map_err(|_| "Microsoft sign-in task could not finish.")?
}
#[tauri::command]
pub async fn microsoft_sign_in_poll(
    state: State<'_, Arc<MicrosoftIntake>>,
) -> Result<SignInProgress, String> {
    let manager = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || manager.poll())
        .await
        .map_err(|_| "Microsoft sign-in task could not finish.")?
}
#[tauri::command]
pub async fn microsoft_disconnect(state: State<'_, Arc<MicrosoftIntake>>) -> Result<(), String> {
    let manager = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || manager.disconnect())
        .await
        .map_err(|_| "Microsoft disconnect task could not finish.")?
}
#[tauri::command]
pub async fn microsoft_bind_intake(
    drive_id: String,
    folder_id: String,
    state: State<'_, Arc<MicrosoftIntake>>,
) -> Result<FolderBinding, String> {
    let manager = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || manager.bind(&drive_id, &folder_id))
        .await
        .map_err(|_| "Microsoft folder pairing could not finish.")?
}
#[tauri::command]
pub fn microsoft_open_sign_in(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    app.opener().open_url("https://microsoft.com/devicelogin",None::<&str>).map_err(|_| "Microsoft sign-in could not be opened. Type https://microsoft.com/devicelogin in your browser.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn path_scope_is_component_based_not_a_prefix() {
        assert!(within(Path::new("C:/Intake/one.pdf"), "C:/Intake"));
        assert!(!within(Path::new("C:/IntakeOther/one.pdf"), "C:/Intake"));
    }
    #[test]
    fn malformed_config_is_not_treated_as_disconnected_defaults() {
        let path =
            std::env::temp_dir().join(format!("intern-invalid-config-{}.json", std::process::id()));
        fs::write(&path, b"not json").unwrap();
        assert!(read_config(&path).is_err());
        let _ = fs::remove_file(path);
    }
}
