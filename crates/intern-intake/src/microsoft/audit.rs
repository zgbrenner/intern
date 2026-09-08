//! A creator is not necessarily the uploader. Only an actual upload event
//! from the authenticated Microsoft audit API can authorize an intake item.
//! No locally writable sidecar or user-entered name is an authority.
use super::{
    auth::MicrosoftClient,
    proof::{Account, Candidate, is_guid, same_person, text},
};
use chrono::{DateTime, SecondsFormat, Utc};
use reqwest::Url;
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Mutex};

struct Search {
    id: String,
    next_poll: i64,
    expires_at: i64,
    accepted: Option<Account>,
}
#[derive(Default)]
pub struct AuditVerifier {
    searches: Mutex<HashMap<String, Search>>,
}
impl AuditVerifier {
    pub fn clear(&self) {
        if let Ok(mut searches) = self.searches.lock() {
            searches.clear();
        }
    }
    pub fn verify(
        &self,
        client: &MicrosoftClient,
        candidate: &Candidate,
        now: i64,
    ) -> Result<Account, String> {
        let created = DateTime::parse_from_rfc3339(&candidate.created_at)
            .map_err(|_| "Upload time is invalid.")?
            .timestamp();
        if created > now || now - created > 7 * 24 * 3600 {
            return Err("Upload verification currently supports new files from the last seven days. This file remains held.".into());
        }
        if !is_guid(&candidate.list_item_id) {
            return Err("Microsoft did not provide an unambiguous file identity.".into());
        }
        let key = format!(
            "{}|{}|{}|{}",
            candidate.uploader.tenant_id, candidate.list_item_id, candidate.etag, candidate.web_url
        );
        let mut searches = self
            .searches
            .lock()
            .map_err(|_| "Upload verification is unavailable.")?;
        searches.retain(|_, search| search.expires_at > now);
        if let Some(search) = searches.get_mut(&key) {
            if let Some(account) = &search.accepted {
                return Ok(account.clone());
            }
            if now < search.next_poll {
                return Err("Waiting for Microsoft's upload-event verification. Nothing has been processed.".into());
            }
            search.next_poll = now + 30;
            let url = Url::parse(&format!(
                "https://graph.microsoft.com/v1.0/security/auditLog/queries/{}",
                search.id
            ))
            .map_err(|_| "Audit query is invalid.")?;
            let (_, status) = client.metadata(url)?;
            match status["status"].as_str() {
                Some("succeeded") => {}
                Some("running" | "notStarted") => {
                    return Err(
                        "Microsoft is still checking the upload event. The file remains held."
                            .into(),
                    );
                }
                _ => {
                    search.expires_at = now;
                    return Err(
                        "Microsoft could not finish upload verification. The file remains held."
                            .into(),
                    );
                }
            }
            let records_path = format!("/v1.0/security/auditLog/queries/{}/records", search.id);
            let mut next = Some(
                Url::parse(&format!("https://graph.microsoft.com{records_path}"))
                    .map_err(|_| "Audit query is invalid.")?,
            );
            let mut records = Vec::new();
            for _ in 0..4 {
                let Some(url) = next.take() else {
                    break;
                };
                if url.scheme() != "https"
                    || url.host_str() != Some("graph.microsoft.com")
                    || url.path() != records_path
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.fragment().is_some()
                {
                    return Err(
                        "Microsoft returned an unexpected audit page. The file remains held."
                            .into(),
                    );
                }
                let (_, page) = client.metadata(url)?;
                let values = page["value"]
                    .as_array()
                    .ok_or("Microsoft audit records were incomplete.")?;
                records.extend(values.iter().cloned());
                if records.len() > 1024 {
                    return Err(
                        "Upload history is too large to verify safely. The file remains held."
                            .into(),
                    );
                }
                if let Some(link) = page["@odata.nextLink"].as_str() {
                    next = Some(Url::parse(link).map_err(|_| "Microsoft audit page is invalid.")?);
                }
            }
            if next.is_some() {
                return Err(
                    "Microsoft upload history is incomplete. The file remains held.".into(),
                );
            }
            let actor = upload_actor(&records, candidate).map_err(str::to_owned)?;
            search.accepted = Some(actor.clone());
            search.expires_at = now + 60;
            return Ok(actor);
        }
        if searches.len() >= 32 {
            return Err("Other upload checks are still pending. This file remains held until a slot is available.".into());
        }
        let start = DateTime::<Utc>::from_timestamp(created.saturating_sub(5), 0)
            .ok_or("Upload time is invalid.")?
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        let end = DateTime::<Utc>::from_timestamp(now, 0)
            .ok_or("Verification time is invalid.")?
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        let body = json!({"displayName":"Intern upload identity verification","filterStartDateTime":start,"filterEndDateTime":end,"recordTypeFilters":["sharePointFileOperation","oneDrive"],"objectIdFilters":[candidate.web_url]});
        let (_, response) = client.start_audit_query(&body)?;
        let id = text(&response, "/id").map_err(str::to_owned)?;
        if !is_guid(id) {
            return Err("Microsoft returned an invalid audit query identity.".into());
        }
        searches.insert(
            key,
            Search {
                id: id.into(),
                next_poll: now + 30,
                expires_at: now + 600,
                accepted: None,
            },
        );
        Err("Waiting for Microsoft's upload event. The file remains held while audit information becomes available.".into())
    }
}

/// Deliberately conservative: support fresh, unchanged uploads. Moves, copies,
/// overwrites, application actors, conflicting identities, or missing IDs are
/// not inferred from Created By, Modified By, email, or a machine marker.
pub fn upload_actor(records: &[Value], candidate: &Candidate) -> Result<Account, &'static str> {
    let mut upload: Option<&Value> = None;
    for record in records {
        if !text(record, "/organizationId")?.eq_ignore_ascii_case(&candidate.uploader.tenant_id)
            || text(record, "/objectId")? != candidate.web_url
        {
            return Err("Microsoft upload history does not match this tenant and file.");
        }
        match text(record, "/operation")? {
            "FileAccessed"
            | "FileAccessedExtended"
            | "FilePreviewed"
            | "FileDownloaded"
            | "FileSyncDownloadedFull" => continue,
            "FileUploaded" | "FileSyncUploadedFull" => {}
            _ => {
                return Err(
                    "This file has other activity, such as a move, copy, edit, or overwrite. Uploader verification is required.",
                );
            }
        }
        if !matches!(
            record["userType"].as_str(),
            Some("regular" | "admin" | "guest")
        ) {
            return Err("The upload was not attributed to an identifiable person.");
        }
        if !text(record, "/auditData/ListItemUniqueId")?
            .eq_ignore_ascii_case(&candidate.list_item_id)
        {
            return Err("The upload event belongs to a different file revision or identity.");
        }
        let id = text(record, "/userId")?;
        // Some audit records use a UPN instead of a directory GUID. The only
        // permitted UPN link comes from authenticated /me for this very ID;
        // candidate() leaves it empty for everyone else. A conflicting GUID
        // never falls back to an email or display name.
        let actor_matches = if is_guid(id) {
            id.eq_ignore_ascii_case(&candidate.uploader.id)
        } else {
            let upn = &candidate.uploader.user_principal_name;
            !upn.is_empty()
                && upn.contains('@')
                && id.eq_ignore_ascii_case(upn)
                && text(record, "/userPrincipalName")?.eq_ignore_ascii_case(upn)
        };
        if !actor_matches {
            return Err(
                "Microsoft's upload actor and file creator do not establish the same account.",
            );
        }
        let stamp = DateTime::parse_from_rfc3339(text(record, "/createdDateTime")?)
            .map_err(|_| "Upload time is invalid.")?;
        let created = DateTime::parse_from_rfc3339(&candidate.created_at)
            .map_err(|_| "File creation time is invalid.")?;
        if (stamp.timestamp() - created.timestamp()).abs() > 2 {
            return Err("The upload event cannot be bound to this initial file revision.");
        }
        if let Some(previous) = upload {
            if text(previous, "/id")? != text(record, "/id")? {
                return Err("Multiple upload events are ambiguous. The file remains held.");
            }
        }
        text(record, "/id")?;
        upload = Some(record);
    }
    let record = upload.ok_or(
        "Microsoft has not supplied a verifiable upload event. Created By alone is not proof.",
    )?;
    let actor = Account {
        tenant_id: text(record, "/organizationId")?.to_ascii_lowercase(),
        id: candidate.uploader.id.clone(),
        display_name: candidate.uploader.display_name.clone(),
        email: record["userPrincipalName"].as_str().unwrap_or("").into(),
        user_principal_name: candidate.uploader.user_principal_name.clone(),
    };
    if !same_person(&actor, &candidate.uploader) {
        return Err("Upload identity changed. The file remains held.");
    }
    Ok(actor)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn candidate() -> Candidate {
        Candidate {
            item_id: "item".into(),
            etag: "etag".into(),
            uploader: Account {
                tenant_id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".into(),
                id: "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".into(),
                display_name: "Zachary Brenner".into(),
                email: "zack@example.test".into(),
                user_principal_name: "zack@example.test".into(),
            },
            quick_xor: "hash".into(),
            size: 4,
            created_at: "2026-09-08T12:00:00Z".into(),
            web_url: "https://example.sharepoint.com/intake/file.pdf".into(),
            list_item_id: "eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee".into(),
        }
    }
    fn event() -> Value {
        let c = candidate();
        json!({"id":"event-id","organizationId":c.uploader.tenant_id,"objectId":c.web_url,"operation":"FileUploaded","userType":"regular","userId":c.uploader.id,"userPrincipalName":"zack@example.test","createdDateTime":c.created_at,"auditData":{"ListItemUniqueId":c.list_item_id}})
    }
    #[test]
    fn creator_metadata_without_upload_event_never_authorizes() {
        assert!(upload_actor(&[], &candidate()).is_err());
    }
    #[test]
    fn explicit_upload_event_matches_exact_directory_identity() {
        assert!(same_person(
            &upload_actor(&[event()], &candidate()).unwrap(),
            &candidate().uploader
        ));
    }
    #[test]
    fn same_email_or_name_does_not_override_a_different_actor_id() {
        let mut v = event();
        v["userId"] = json!("cccccccc-cccc-cccc-cccc-cccccccccccc");
        assert!(upload_actor(&[v], &candidate()).is_err());
    }
    #[test]
    fn upload_events_missing_any_binding_are_held() {
        for path in [
            "/userId",
            "/organizationId",
            "/objectId",
            "/createdDateTime",
            "/auditData/ListItemUniqueId",
            "/id",
        ] {
            let mut v = event();
            *v.pointer_mut(path).unwrap() = Value::Null;
            assert!(upload_actor(&[v], &candidate()).is_err(), "{path}");
        }
    }
    #[test]
    fn moves_edits_copies_overwrites_and_unknown_events_never_authorize() {
        for op in [
            "FileMoved",
            "FileCopied",
            "FileModified",
            "FileRenamed",
            "FutureOperation",
        ] {
            let mut v = event();
            v["operation"] = json!(op);
            assert!(upload_actor(&[event(), v], &candidate()).is_err());
        }
        let mut second = event();
        second["id"] = json!("second");
        assert!(upload_actor(&[event(), second], &candidate()).is_err());
    }
    #[test]
    fn stale_upload_or_replaced_item_is_held() {
        let mut v = event();
        v["createdDateTime"] = json!("2026-09-07T12:00:00Z");
        assert!(upload_actor(&[v], &candidate()).is_err());
        let mut v = event();
        v["auditData"]["ListItemUniqueId"] = json!("ffffffff-ffff-ffff-ffff-ffffffffffff");
        assert!(upload_actor(&[v], &candidate()).is_err());
    }
    #[test]
    fn system_and_application_actors_are_unknown() {
        for actor in [
            "application",
            "system",
            "servicePrincipal",
            "unknownFutureValue",
        ] {
            let mut v = event();
            v["userType"] = json!(actor);
            assert!(upload_actor(&[v], &candidate()).is_err());
        }
    }
    #[test]
    fn exact_provider_upn_links_an_upload_to_the_authenticated_directory_id() {
        let mut v = event();
        v["userId"] = json!("ZACK@example.test");
        let actor = upload_actor(&[v], &candidate()).unwrap();
        assert!(same_person(&actor, &candidate().uploader));
    }
    #[test]
    fn provider_upn_never_falls_back_to_a_typed_or_display_email() {
        let mut c = candidate();
        c.uploader.user_principal_name.clear();
        let mut v = event();
        v["userId"] = json!(c.uploader.email);
        assert!(upload_actor(&[v], &c).is_err());
    }
    #[test]
    fn conflicting_upn_fields_do_not_authorize() {
        let mut v = event();
        v["userId"] = json!("zack@example.test");
        v["userPrincipalName"] = json!("john@example.test");
        assert!(upload_actor(&[v], &candidate()).is_err());
    }
}
