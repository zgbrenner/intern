//! Provider metadata validation. Display names and email addresses never authorize a file.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub tenant_id: String,
    pub id: String,
    pub display_name: String,
    pub email: String,
    /// Only the principal name returned by authenticated /me, never a typed alias.
    #[serde(default)]
    pub user_principal_name: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderBinding {
    pub local_folder: String,
    pub drive_id: String,
    pub folder_id: String,
    pub web_url: String,
    pub tenant_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Candidate {
    pub item_id: String,
    pub etag: String,
    pub uploader: Account,
    pub quick_xor: String,
    pub size: u64,
    pub created_at: String,
    pub web_url: String,
    pub list_item_id: String,
}

/// A missing property, a different tenant, an edit, or a non-user identity is
/// not a weak match. It is no match. This returns a CANDIDATE, not upload proof.
/// Authorization also requires an actual Microsoft audit upload event.
pub fn candidate(
    metadata: &Value,
    account: &Account,
    filename: &str,
) -> Result<Candidate, &'static str> {
    for facet in [
        "deleted",
        "folder",
        "remoteItem",
        "pendingOperations",
        "malware",
    ] {
        if metadata.get(facet).is_some_and(|value| !value.is_null()) {
            return Err("Microsoft reports an unresolved, moved/shared, or unavailable item.");
        }
    }
    let tenant = text(metadata, "/sharepointIds/tenantId")?;
    if !tenant.eq_ignore_ascii_case(&account.tenant_id) {
        return Err("The Microsoft item belongs to a different organization.");
    }
    if text(metadata, "/name")? != filename {
        return Err("The Microsoft filename does not match the local file.");
    }
    let creator = text(metadata, "/createdBy/user/id")?;
    let editor = text(metadata, "/lastModifiedBy/user/id")?;
    if !is_guid(creator) || !creator.eq_ignore_ascii_case(editor) {
        return Err("The creator and latest editor cannot establish one upload identity.");
    }
    let created_at = text(metadata, "/createdDateTime")?;
    let modified_at = text(metadata, "/lastModifiedDateTime")?;
    if created_at != modified_at || !valid_timestamp(created_at) {
        return Err(
            "This item changed after creation; the uploader needs independent verification.",
        );
    }
    let quick_xor = text(metadata, "/file/hashes/quickXorHash")?;
    let size = metadata
        .get("size")
        .and_then(Value::as_u64)
        .filter(|size| *size > 0)
        .ok_or("Microsoft has not provided a settled file size.")?;
    Ok(Candidate {
        item_id: text(metadata, "/id")?.to_owned(),
        etag: text(metadata, "/eTag")?.to_owned(),
        uploader: Account {
            tenant_id: tenant.to_ascii_lowercase(),
            id: creator.to_ascii_lowercase(),
            display_name: metadata
                .pointer("/createdBy/user/displayName")
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .unwrap_or("Microsoft user")
                .to_owned(),
            user_principal_name: if creator.eq_ignore_ascii_case(&account.id) {
                account.user_principal_name.clone()
            } else {
                String::new()
            },
            email: if creator.eq_ignore_ascii_case(&account.id) {
                account.email.clone()
            } else {
                String::new()
            },
        },
        quick_xor: quick_xor.to_owned(),
        size,
        created_at: created_at.to_owned(),
        web_url: text(metadata, "/webUrl")?.to_owned(),
        list_item_id: text(metadata, "/sharepointIds/listItemUniqueId")?.to_owned(),
    })
}

pub fn same_person(left: &Account, right: &Account) -> bool {
    is_guid(&left.id)
        && is_guid(&right.id)
        && is_guid(&left.tenant_id)
        && left.id.eq_ignore_ascii_case(&right.id)
        && left.tenant_id.eq_ignore_ascii_case(&right.tenant_id)
}

pub(crate) fn text<'a>(value: &'a Value, pointer: &str) -> Result<&'a str, &'static str> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty() && text.len() <= 4096)
        .ok_or("Microsoft has not provided complete upload identity metadata.")
}

pub fn is_guid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn valid_timestamp(value: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(value).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn person() -> Account {
        Account {
            tenant_id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".into(),
            id: "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".into(),
            display_name: "Zachary Brenner".into(),
            email: "zack@example.test".into(),
            user_principal_name: "zack@example.test".into(),
        }
    }
    fn metadata() -> Value {
        json!({"id":"document-id","eTag":"revision-1","name":"agreement.pdf","size":12,"webUrl":"https://example.sharepoint.com/Legal/intake/agreement.pdf","sharepointIds":{"tenantId":person().tenant_id,"listItemUniqueId":"eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee"},"createdBy":{"user":{"id":person().id,"displayName":"Zachary Brenner"}},"lastModifiedBy":{"user":{"id":person().id}},"createdDateTime":"2026-09-08T12:00:00Z","lastModifiedDateTime":"2026-09-08T12:00:00Z","file":{"hashes":{"quickXorHash":"AAAAAAAAAAAAAAAAAAAAAAAAAAA="}}})
    }
    #[test]
    fn accepts_complete_provider_identity_for_an_unchanged_initial_item() {
        let proof = candidate(&metadata(), &person(), "agreement.pdf").unwrap();
        assert!(same_person(&proof.uploader, &person()));
    }
    #[test]
    fn missing_creator_is_unknown_not_the_last_editor() {
        let mut v = metadata();
        v.as_object_mut().unwrap().remove("createdBy");
        assert!(candidate(&v, &person(), "agreement.pdf").is_err());
    }
    #[test]
    fn names_and_emails_never_match_different_ids() {
        let mut other = person();
        other.id = "cccccccc-cccc-cccc-cccc-cccccccccccc".into();
        assert!(!same_person(&person(), &other));
    }
    #[test]
    fn a_matching_id_in_another_tenant_is_not_me() {
        let mut other = person();
        other.tenant_id = "dddddddd-dddd-dddd-dddd-dddddddddddd".into();
        assert!(!same_person(&person(), &other));
    }
    #[test]
    fn numeric_sharepoint_lookup_ids_are_not_directory_ids() {
        let mut v = metadata();
        v["createdBy"]["user"]["id"] = json!("17");
        v["lastModifiedBy"]["user"]["id"] = json!("17");
        assert!(candidate(&v, &person(), "agreement.pdf").is_err());
    }
    #[test]
    fn conflicting_editor_or_later_revision_is_held() {
        for pointer in ["/lastModifiedBy/user/id", "/lastModifiedDateTime"] {
            let mut v = metadata();
            *v.pointer_mut(pointer).unwrap() = json!("different");
            assert!(candidate(&v, &person(), "agreement.pdf").is_err());
        }
    }
    #[test]
    fn every_ambiguous_facet_is_held_even_when_empty() {
        for facet in [
            "deleted",
            "folder",
            "remoteItem",
            "pendingOperations",
            "malware",
        ] {
            let mut v = metadata();
            v[facet] = json!({});
            assert!(candidate(&v, &person(), "agreement.pdf").is_err());
        }
    }
    #[test]
    fn missing_or_blank_required_metadata_fails_closed() {
        for pointer in [
            "/id",
            "/eTag",
            "/createdBy/user/id",
            "/lastModifiedBy/user/id",
            "/file/hashes/quickXorHash",
            "/sharepointIds/tenantId",
            "/createdDateTime",
        ] {
            let mut v = metadata();
            *v.pointer_mut(pointer).unwrap() = json!("");
            assert!(
                candidate(&v, &person(), "agreement.pdf").is_err(),
                "{pointer}"
            );
        }
    }
    #[test]
    fn wrong_file_or_tenant_is_held() {
        assert!(candidate(&metadata(), &person(), "other.pdf").is_err());
        let mut v = metadata();
        v["sharepointIds"]["tenantId"] = json!("dddddddd-dddd-dddd-dddd-dddddddddddd");
        assert!(candidate(&v, &person(), "agreement.pdf").is_err());
    }
}
