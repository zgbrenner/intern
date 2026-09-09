//! Delegated device authorization. Access tokens are opaque; /me establishes
//! the person. No decoded-but-unverified JWT claims, secret in settings, or
//! arbitrary OAuth endpoint is accepted.
use super::{
    proof::{Account, is_guid, text},
    transport::{MicrosoftTransport, Transport, allowed_endpoint},
};
use crate::coordination::{Clock, SystemClock};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::{Arc, Mutex};

pub const SCOPES: &str = "https://graph.microsoft.com/User.Read https://graph.microsoft.com/Files.SelectedOperations.Selected https://graph.microsoft.com/AuditLogsQuery-SharePoint.Read.All https://graph.microsoft.com/AuditLogsQuery-OneDrive.Read.All offline_access";

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthConfig {
    pub tenant_id: String,
    pub client_id: String,
}
impl AuthConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !is_guid(&self.tenant_id) || !is_guid(&self.client_id) {
            return Err("Enter the organization tenant ID and public application ID supplied by your administrator.".into());
        }
        Ok(())
    }
    fn key(&self) -> String {
        format!(
            "{}:{}",
            self.tenant_id.to_ascii_lowercase(),
            self.client_id.to_ascii_lowercase()
        )
    }
    fn endpoint(&self, operation: &str) -> Result<Url, String> {
        self.validate()?;
        Url::parse(&format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/{operation}",
            self.tenant_id
        ))
        .map_err(|_| "Microsoft sign-in URL is invalid.".into())
    }
}

pub trait TokenStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<String>, String>;
    fn set(&self, key: &str, value: &str) -> Result<(), String>;
    fn delete(&self, key: &str) -> Result<(), String>;
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DevicePrompt {
    pub user_code: String,
    pub verification_uri: String,
    pub interval_seconds: u64,
    pub expires_at: i64,
}
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SignInProgress {
    Pending {
        #[serde(rename = "intervalSeconds")]
        interval_seconds: u64,
    },
    Connected {
        account: Account,
    },
}

// Deliberately no Debug/Serialize on access sessions and device codes.
#[derive(Clone)]
struct Session {
    token: String,
    account: Account,
    expires_at: i64,
}
struct Pending {
    code: String,
    expires_at: i64,
    next_poll: i64,
    interval: u64,
}
#[derive(Deserialize, Serialize)]
struct StoredSession {
    refresh_token: String,
    account: Account,
}
struct State {
    config: AuthConfig,
    session: Option<Session>,
    pending: Option<Pending>,
    retry_at: i64,
    allow_refresh: bool,
}

/// A failed Graph request, and whether it says anything about the connection.
///
/// Only a failure of the connection itself justifies pausing every other
/// file's verification. A 404 for one file the sync client has not finished
/// uploading is a verdict about that file, and holding the whole folder
/// behind it means the slowest document in a share decides how fast every
/// other document is checked.
struct Failure {
    message: String,
    back_off: bool,
}

impl Failure {
    fn connection(message: String) -> Self {
        Self {
            message,
            back_off: true,
        }
    }
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self {
            message,
            back_off: false,
        }
    }
}

impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        Self::from(message.to_string())
    }
}

pub struct MicrosoftClient {
    transport: Arc<dyn Transport>,
    tokens: Arc<dyn TokenStore>,
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
}
impl MicrosoftClient {
    pub fn new(config: AuthConfig, tokens: Arc<dyn TokenStore>) -> Result<Self, String> {
        Ok(Self::with_transport(
            config,
            tokens,
            Arc::new(MicrosoftTransport::new()?),
            Arc::new(SystemClock),
        ))
    }
    pub fn with_transport(
        config: AuthConfig,
        tokens: Arc<dyn TokenStore>,
        transport: Arc<dyn Transport>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            transport,
            tokens,
            clock,
            state: Mutex::new(State {
                config,
                session: None,
                pending: None,
                retry_at: 0,
                allow_refresh: true,
            }),
        }
    }
    pub fn account(&self) -> Option<Account> {
        self.state.try_lock().ok().and_then(|state| {
            state
                .session
                .as_ref()
                .filter(|session| session.expires_at > self.clock.now())
                .map(|session| session.account.clone())
        })
    }
    pub fn begin(&self, config: AuthConfig) -> Result<DevicePrompt, String> {
        config.validate()?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Microsoft sign-in state is unavailable.")?;
        state.session = None;
        state.pending = None;
        state.config = config.clone();
        state.retry_at = 0;
        state.allow_refresh = false;
        let reply = self.transport.request(
            config.endpoint("devicecode")?,
            Some(&[("client_id", &config.client_id), ("scope", SCOPES)]),
            None,
        )?;
        if reply.status != 200 {
            return Err("Microsoft sign-in could not start. Check the application registration, public-client setting, and organization consent.".into());
        }
        let code = text(&reply.body, "/device_code")?.to_owned();
        let user_code = text(&reply.body, "/user_code")?.to_owned();
        let verification_uri = text(&reply.body, "/verification_uri")?;
        // Microsoft supplies this URL, but the UI must not become a phishing redirect.
        if ![
            "https://microsoft.com/devicelogin",
            "https://www.microsoft.com/devicelogin",
        ]
        .contains(&verification_uri)
        {
            return Err("Microsoft returned an unexpected sign-in address.".into());
        }
        let interval = reply.body["interval"].as_u64().unwrap_or(5).clamp(5, 60);
        let lifetime = reply.body["expires_in"]
            .as_i64()
            .filter(|seconds| (1..=1800).contains(seconds))
            .ok_or("Microsoft sign-in expiry is invalid.")?;
        let expires_at = self.clock.now() + lifetime;
        state.pending = Some(Pending {
            code,
            expires_at,
            next_poll: self.clock.now() + interval as i64,
            interval,
        });
        Ok(DevicePrompt {
            user_code,
            verification_uri: verification_uri.to_owned(),
            interval_seconds: interval,
            expires_at,
        })
    }
    pub fn poll(&self) -> Result<SignInProgress, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Microsoft sign-in state is unavailable.")?;
        let config = state.config.clone();
        let pending = state
            .pending
            .as_mut()
            .ok_or("Start Microsoft sign-in again.")?;
        if self.clock.now() >= pending.expires_at {
            state.pending = None;
            return Err("Microsoft sign-in expired. Start again; files remain held.".into());
        }
        if self.clock.now() < pending.next_poll {
            return Ok(SignInProgress::Pending {
                interval_seconds: pending.interval,
            });
        }
        pending.next_poll = self.clock.now() + pending.interval as i64;
        let reply = self.transport.request(
            config.endpoint("token")?,
            Some(&[
                ("client_id", &config.client_id),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", &pending.code),
            ]),
            None,
        )?;
        if reply.status != 200 {
            match reply.body["error"].as_str() {
                Some("authorization_pending") => {
                    return Ok(SignInProgress::Pending {
                        interval_seconds: pending.interval,
                    });
                }
                Some("slow_down") => {
                    pending.interval = (pending.interval + 5).min(60);
                    pending.next_poll = self.clock.now() + pending.interval as i64;
                    return Ok(SignInProgress::Pending {
                        interval_seconds: pending.interval,
                    });
                }
                _ => {
                    state.pending = None;
                    return Err("Microsoft sign-in was declined, expired, or blocked by organization policy. Files remain held.".into());
                }
            }
        }
        state.pending = None;
        let session = self.establish(&config, &reply.body, None)?;
        let account = session.account.clone();
        state.session = Some(session);
        state.allow_refresh = true;
        Ok(SignInProgress::Connected { account })
    }
    pub fn disconnect(&self) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Microsoft sign-in state is unavailable.")?;
        state.pending = None;
        state.session = None;
        state.retry_at = 0;
        state.allow_refresh = false;
        if state.config.validate().is_ok() {
            self.tokens.delete(&state.config.key())?;
        }
        Ok(())
    }
    /// Every metadata request obtains a current delegated session. Refreshing
    /// rechecks /me against the stored tenant-scoped ID before accepting it.
    pub fn metadata(&self, url: Url) -> Result<(Account, Value), String> {
        self.graph_request(Some(url), None)
    }
    pub fn start_audit_query(&self, body: &Value) -> Result<(Account, Value), String> {
        self.graph_request(None, Some(body))
    }
    fn graph_request(
        &self,
        url: Option<Url>,
        body: Option<&Value>,
    ) -> Result<(Account, Value), String> {
        if url
            .as_ref()
            .is_some_and(|url| !allowed_endpoint(url, false, true))
        {
            return Err("Only Microsoft intake metadata may be requested.".into());
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Microsoft connection is unavailable.")?;
        if !state.allow_refresh || state.pending.is_some() {
            return Err(
                "Microsoft is disconnected or sign-in is incomplete. Files remain held.".into(),
            );
        }
        if self.clock.now() < state.retry_at {
            return Err("Microsoft verification is waiting to retry. Files remain held.".into());
        }
        let result = (|| -> Result<(Account, Value), Failure> {
            if state
                .session
                .as_ref()
                .is_none_or(|session| session.expires_at <= self.clock.now() + 30)
            {
                state.session = None;
                state.config.validate()?;
                let raw = self
                    .tokens
                    .get(&state.config.key())?
                    .ok_or("Connect your Microsoft account to verify uploads.")?;
                if raw.len() > 64 * 1024 {
                    return Err("Stored Microsoft credentials are invalid. Sign in again.".into());
                }
                let stored: StoredSession = serde_json::from_str(&raw)
                    .map_err(|_| "Stored Microsoft credentials are invalid. Sign in again.")?;
                if !stored
                    .account
                    .tenant_id
                    .eq_ignore_ascii_case(&state.config.tenant_id)
                {
                    return Err("Microsoft organization changed. Sign in again.".into());
                }
                let reply = self
                    .transport
                    .request(
                        state.config.endpoint("token")?,
                        Some(&[
                            ("client_id", &state.config.client_id),
                            ("grant_type", "refresh_token"),
                            ("refresh_token", &stored.refresh_token),
                            ("scope", SCOPES),
                        ]),
                        None,
                    )
                    .map_err(Failure::connection)?;
                if reply.status != 200 {
                    return Err(Failure::connection("Microsoft sign-in needs attention. Reconnect your account; files remain held.".into()));
                }
                state.session = Some(
                    self.establish(&state.config, &reply.body, Some(&stored))
                        .map_err(Failure::connection)?,
                );
            }
            let session = state
                .session
                .as_ref()
                .ok_or("Connect Microsoft before verifying uploads.")?;
            let reply = if let Some(body) = body {
                self.transport
                    .audit_query(body, &session.token)
                    .map_err(Failure::connection)?
            } else {
                self.transport
                    .request(
                        url.ok_or("Microsoft metadata URL is missing.")?,
                        None,
                        Some(&session.token),
                    )
                    .map_err(Failure::connection)?
            };
            match reply.status {
                200 | 201 => Ok((session.account.clone(),reply.body)),
                401 => { state.session=None; Err("Microsoft sign-in expired. Reconnect; files remain held.".into()) }
                403 => Err("Microsoft denied access to this folder. Ask your administrator to grant the app read access to the selected intake folder.".into()),
                404 => Err("This file is not yet available in the paired Microsoft folder.".into()),
                429 | 503 => { state.retry_at=self.clock.now()+reply.retry_after as i64; Err("Microsoft requested a slower verification rate. Files remain held until retry.".into()) }
                _ => Err("Microsoft could not verify this upload. Files remain held.".into()),
            }
        })();
        result.map_err(|failure| {
            if failure.back_off {
                state.retry_at = state.retry_at.max(self.clock.now() + 10);
            }
            failure.message
        })
    }
    fn establish(
        &self,
        config: &AuthConfig,
        reply: &Value,
        previous: Option<&StoredSession>,
    ) -> Result<Session, String> {
        if reply["token_type"]
            .as_str()
            .is_none_or(|kind| !kind.eq_ignore_ascii_case("Bearer"))
        {
            return Err("Microsoft returned an unusable sign-in token.".into());
        }
        let token = reply["access_token"]
            .as_str()
            .filter(|token| !token.is_empty() && token.len() <= 64 * 1024)
            .ok_or("Microsoft returned no access token.")?;
        let lifetime = reply["expires_in"]
            .as_i64()
            .filter(|seconds| (60..=86400).contains(seconds))
            .ok_or("Microsoft token expiry is invalid.")?;
        let profile = self.transport.request(
            Url::parse(
                "https://graph.microsoft.com/v1.0/me?$select=id,displayName,mail,userPrincipalName",
            )
            .map_err(|_| "Microsoft profile URL is invalid.")?,
            None,
            Some(token),
        )?;
        if profile.status != 200 {
            return Err("Microsoft could not verify the signed-in person.".into());
        }
        let id = text(&profile.body, "/id")?;
        if !is_guid(id) {
            return Err(
                "This connection currently requires a Microsoft work or school account.".into(),
            );
        }
        let account = Account {
            tenant_id: config.tenant_id.to_ascii_lowercase(),
            id: id.to_ascii_lowercase(),
            display_name: text(&profile.body, "/displayName")?.to_owned(),
            user_principal_name: profile.body["userPrincipalName"]
                .as_str()
                .unwrap_or("")
                .to_owned(),
            email: profile.body["mail"]
                .as_str()
                .filter(|email| !email.trim().is_empty())
                .or_else(|| profile.body["userPrincipalName"].as_str())
                .unwrap_or("")
                .to_owned(),
        };
        if previous.is_some_and(|stored| !super::proof::same_person(&stored.account, &account)) {
            return Err("The Microsoft account changed. Sign in deliberately before processing any uploads.".into());
        }
        let refresh_token = reply["refresh_token"]
            .as_str()
            .filter(|token| !token.is_empty() && token.len() <= 64 * 1024)
            .or_else(|| previous.map(|stored| stored.refresh_token.as_str()))
            .ok_or("Microsoft did not grant a persistent sign-in. Files remain held.")?;
        let stored = serde_json::to_string(&StoredSession {
            refresh_token: refresh_token.to_owned(),
            account: account.clone(),
        })
        .map_err(|_| "Microsoft credentials could not be protected.")?;
        self.tokens.set(&config.key(), &stored)?;
        Ok(Session {
            token: token.to_owned(),
            account,
            expires_at: self.clock.now() + lifetime,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::transport::Reply;
    use super::*;
    use serde_json::json;
    use std::{
        collections::{HashMap, VecDeque},
        sync::atomic::{AtomicI64, Ordering},
    };
    #[derive(Default)]
    struct Memory(Mutex<HashMap<String, String>>);
    impl TokenStore for Memory {
        fn get(&self, key: &str) -> Result<Option<String>, String> {
            Ok(self.0.lock().unwrap().get(key).cloned())
        }
        fn set(&self, key: &str, value: &str) -> Result<(), String> {
            self.0.lock().unwrap().insert(key.into(), value.into());
            Ok(())
        }
        fn delete(&self, key: &str) -> Result<(), String> {
            self.0.lock().unwrap().remove(key);
            Ok(())
        }
    }
    struct Time(AtomicI64);
    impl Clock for Time {
        fn now(&self) -> i64 {
            self.0.load(Ordering::SeqCst)
        }
    }
    struct Fake {
        replies: Mutex<VecDeque<Reply>>,
        calls: Mutex<Vec<(String, bool)>>,
    }
    impl Fake {
        fn new(replies: Vec<Reply>) -> Self {
            Self {
                replies: Mutex::new(replies.into()),
                calls: Mutex::new(Vec::new()),
            }
        }
    }
    impl Transport for Fake {
        fn request(
            &self,
            url: Url,
            form: Option<&[(&str, &str)]>,
            bearer: Option<&str>,
        ) -> Result<Reply, String> {
            assert!(allowed_endpoint(&url, form.is_some(), bearer.is_some()));
            self.calls
                .lock()
                .unwrap()
                .push((url.to_string(), bearer.is_some()));
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .ok_or("Unexpected request".into())
        }
        fn audit_query(&self, _body: &Value, _bearer: &str) -> Result<Reply, String> {
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .ok_or("Unexpected audit request".into())
        }
    }
    fn config() -> AuthConfig {
        AuthConfig {
            tenant_id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".into(),
            client_id: "cccccccc-cccc-cccc-cccc-cccccccccccc".into(),
        }
    }
    fn reply(status: u16, body: Value) -> Reply {
        Reply {
            status,
            body,
            retry_after: 60,
        }
    }
    fn device() -> Reply {
        reply(
            200,
            json!({"device_code":"private-device-code","user_code":"ABCD-EFGH","verification_uri":"https://microsoft.com/devicelogin","expires_in":900,"interval":5}),
        )
    }
    fn token() -> Reply {
        reply(
            200,
            json!({"token_type":"Bearer","access_token":"private-access-token","refresh_token":"private-refresh-token","expires_in":3600}),
        )
    }
    fn me() -> Reply {
        reply(
            200,
            json!({"id":"bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb","displayName":"Zachary Brenner","mail":"zack@example.test","userPrincipalName":"zack@example.test"}),
        )
    }
    fn rig(replies: Vec<Reply>) -> (MicrosoftClient, Arc<Fake>, Arc<Memory>, Arc<Time>) {
        let http = Arc::new(Fake::new(replies));
        let store = Arc::new(Memory::default());
        let clock = Arc::new(Time(AtomicI64::new(1000)));
        (
            MicrosoftClient::with_transport(config(), store.clone(), http.clone(), clock.clone()),
            http,
            store,
            clock,
        )
    }
    fn item() -> Url {
        super::super::transport::item_url("drive", "folder", None).unwrap()
    }
    #[test]
    fn construction_and_status_make_no_network_calls() {
        let (client, http, _, _) = rig(vec![]);
        assert!(client.account().is_none());
        assert!(http.calls.lock().unwrap().is_empty());
    }
    #[test]
    fn device_secrets_never_leave_the_backend_prompt() {
        let (client, _, _, _) = rig(vec![device()]);
        let prompt = serde_json::to_string(&client.begin(config()).unwrap()).unwrap();
        assert!(prompt.contains("ABCD-EFGH"));
        assert!(!prompt.contains("private-device-code"));
        assert!(!prompt.contains("private-access-token"));
    }
    #[test]
    fn poll_waits_for_microsoft_interval_and_establishes_identity_via_me() {
        let (client, http, store, time) = rig(vec![device(), token(), me()]);
        client.begin(config()).unwrap();
        assert!(matches!(
            client.poll().unwrap(),
            SignInProgress::Pending { .. }
        ));
        assert_eq!(http.calls.lock().unwrap().len(), 1);
        time.0.store(1005, Ordering::SeqCst);
        let result = client.poll().unwrap();
        assert!(matches!(result, SignInProgress::Connected { .. }));
        let public = serde_json::to_string(&result).unwrap();
        assert!(!public.contains("private-"));
        let stored = store.get(&config().key()).unwrap().unwrap();
        assert!(stored.contains("private-refresh-token"));
        assert!(!stored.contains("private-access-token"));
        assert!(http.calls.lock().unwrap()[2].0.contains("/me?"));
    }
    #[test]
    fn disconnected_account_cannot_be_resurrected_from_a_late_refresh() {
        let (client, http, store, time) = rig(vec![device(), token(), me()]);
        client.begin(config()).unwrap();
        time.0.store(1005, Ordering::SeqCst);
        client.poll().unwrap();
        let old = store.get(&config().key()).unwrap().unwrap();
        client.disconnect().unwrap();
        store.set(&config().key(), &old).unwrap();
        assert!(client.metadata(item()).is_err());
        assert_eq!(http.calls.lock().unwrap().len(), 3);
    }
    #[test]
    fn incomplete_signin_cannot_fall_back_to_previously_stored_credentials() {
        let (client, http, store, _) = rig(vec![device()]);
        store.set(&config().key(), "old-secret").unwrap();
        client.begin(config()).unwrap();
        assert!(client.metadata(item()).is_err());
        assert_eq!(http.calls.lock().unwrap().len(), 1);
    }
    #[test]
    fn expiry_and_slow_down_are_respected() {
        let (client, http, _, time) = rig(vec![device(), reply(400, json!({"error":"slow_down"}))]);
        client.begin(config()).unwrap();
        time.0.store(1005, Ordering::SeqCst);
        assert!(matches!(
            client.poll().unwrap(),
            SignInProgress::Pending {
                interval_seconds: 10
            }
        ));
        time.0.store(1010, Ordering::SeqCst);
        client.poll().unwrap();
        assert_eq!(http.calls.lock().unwrap().len(), 2);
        time.0.store(2000, Ordering::SeqCst);
        assert!(client.poll().is_err());
    }
    #[test]
    fn redirects_and_arbitrary_endpoints_are_never_used_for_authenticated_reads() {
        let (client, http, _, _) = rig(vec![]);
        for url in [
            "https://evil.example/me",
            "http://graph.microsoft.com/v1.0/me",
            "https://graph.microsoft.com/v1.0/drives/drive/items/id/content",
            "https://graph.microsoft.com/v1.0/users",
        ] {
            assert!(client.metadata(Url::parse(url).unwrap()).is_err());
        }
        assert!(http.calls.lock().unwrap().is_empty());
    }
    /// A file the sync client has not finished uploading yet answers 404, and
    /// that is a verdict about one file. Pausing the whole client for it holds
    /// every other file in the folder behind the slowest one.
    #[test]
    fn a_per_file_404_does_not_block_other_requests() {
        let (client, http, _, time) = rig(vec![
            device(),
            token(),
            me(),
            reply(404, json!({})),
            reply(200, json!({"id": "item"})),
        ]);
        client.begin(config()).unwrap();
        time.0.store(1005, Ordering::SeqCst);
        client.poll().unwrap();
        assert!(client.metadata(item()).is_err());
        assert!(
            client.metadata(item()).is_ok(),
            "the next file must still be checked"
        );
        assert_eq!(http.calls.lock().unwrap().len(), 5);
    }

    /// Microsoft being unreachable is not a verdict about any one file, so the
    /// client does back off before trying again.
    #[test]
    fn an_unreachable_microsoft_pauses_verification() {
        let (client, http, _, time) = rig(vec![device(), token(), me()]);
        client.begin(config()).unwrap();
        time.0.store(1005, Ordering::SeqCst);
        client.poll().unwrap();
        assert!(client.metadata(item()).is_err());
        assert!(client.metadata(item()).is_err());
        assert_eq!(
            http.calls.lock().unwrap().len(),
            4,
            "the second attempt must not reach the network"
        );
    }

    #[test]
    fn authentication_failure_does_not_echo_provider_response_or_tokens() {
        let (client, _, _, _) = rig(vec![reply(
            400,
            json!({"error_description":"private-client-secret"}),
        )]);
        let error = client.begin(config()).unwrap_err();
        assert!(!error.contains("private-client-secret"));
    }
}
