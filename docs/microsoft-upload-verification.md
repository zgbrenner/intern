# Microsoft upload verification (pilot)

## What this build enforces

For a protected intake folder, an unverified uploader is never permission to
process. Verification runs before a watcher claim, queue fingerprinting,
extraction, inference and rename application. Manual imports and retries from
that folder use the same backend gate. Revocation moves pending work to review
without calling the extractor, model or file apply operation. Files outside
protected intake folders retain the ordinary local manual workflow.

Microsoft sign-in establishes an organization ID and directory account ID.
Display names and email aliases typed by a user cannot authorize documents.
An audit record containing a UPN rather than a GUID is accepted only when it
exactly matches the principal name returned by authenticated `/me` for the same
creator directory ID. A conflicting GUID never falls back to a matching name
or email. UPN-only identities for other users remain unverified.

`createdBy` alone is not upload proof: it identifies the original creator, not
necessarily the person who moved, copied or overwrote a file. The connector
also requires an actual `FileUploaded` or `FileSyncUploadedFull` event from
Microsoft's audit API, matching tenant, item unique ID, path and creation time.
This initial implementation supports **new, unchanged work/school uploads from
the last seven days**. Creator/editor IDs and service creation/modification
timestamps must agree. Copies, moves, edits, multiple distinct upload events,
missing hashes, remote shortcut items, app/system actors and ambiguous data
remain held. There is no claim of universal uploader detection.

Only after the uploader is established does Intern read local bytes to compare
size and QuickXorHash with Microsoft, compute a local SHA-256 and re-read the
metadata to check the same item and ETag. QuickXorHash is a synchronization
checksum, not a cryptographic signature. SHA-256 binds the checked local bytes
to the queue's existing content checks. This is a routing safeguard for a
cooperative shared workflow, not a replacement for Microsoft permissions,
filesystem ACLs or a tamper-proof audit product.

## Administrator setup

This feature is **not yet a zero-configuration consumer sign-in**. It needs an
organization-owned Microsoft Entra public-client application and a pilot tenant.
No application ID, tenant consent or real Microsoft account is bundled.

1. Register a work/school, tenant-specific public-client application. Enable
   device-code/public-client authentication according to your organization's
   Conditional Access policy. No client secret belongs in the desktop app.
2. Configure the following **delegated** Graph scopes and obtain the necessary
   administrator consent: `User.Read`, `Files.SelectedOperations.Selected`,
   `AuditLogsQuery-SharePoint.Read.All`, `AuditLogsQuery-OneDrive.Read.All` and
   `offline_access`.
3. Grant the application `read` access to the particular intake folder using
   Microsoft's selected-permissions resource grant. Consent alone does not
   grant access to a file or folder. Confirm the signed-in user has appropriate
   access, auditing is available/enabled, and applicable audit role/licensing
   requirements are satisfied. Do not solve a denial by granting blanket
   document access without reviewing the permission change.
4. Provide the tenant ID, public application ID, Microsoft drive ID and intake
   folder item ID to the pilot administrator. The current UI accepts these IDs;
   automatic site/library discovery and organization enrollment are not shipped.
5. In Intern, save the synced intake and destination folders. Under **Microsoft
   upload verification**, read and acknowledge the permissions notice, connect
   your own Microsoft account, and complete device authorization at Microsoft's
   fixed sign-in page. The UI shows Microsoft's name/email and the underlying
   tenant/account IDs, not an editable ownership label.
6. Enter the supplied drive/folder IDs and select **Verify folder pairing**.
   Confirm the Microsoft folder URL corresponds to the saved local folder.
   Unsaved folder edits cannot be paired. The connector checks remote item type
   and tenant; per-file relative path, metadata and checksum checks validate the
   mapping during use. This is not automatic OneDrive root identity discovery.

**Permissions are broader than metadata alone.** Selected-folder `read` access
can permit file contents even though this connector never calls `/content`,
download, preview or upload endpoints. The two audit scopes cover their
workloads, not just the chosen folder. Intern submits searches restricted to
an exact item URL and a bounded creation-time range, but those filters do not
reduce the scope of the underlying permission. An administrator must approve
that tradeoff. Microsoft sign-in does not change the separately selected local
or hosted inference model.

## User behavior

The default is **only my verified uploads**. The existing team-worker option
allows other **verified** uploaders, never unknown ones. Identity is evaluated
before local document content is read by Intern. A held file stays in intake;
no identity guess, machine-arrival fallback or process-anyway button exists.

Settings shows recent checks, the verified uploader and the account that
actually completed analysis. A separate filed name records a completed rename;
undo retracts that filed state. These are local attribution records, capped at
256 entries (the UI displays at most 100), not a tenant-wide audit dashboard.
Previously protected roots remain protected after watching is disabled or the
current folder changes. An ordinary private/local-only watcher must be chosen
explicitly; detected synced or network roots cannot select it. A previously
protected folder does not become unprotected merely by changing that checkbox.

Disconnect disables authorization immediately even if credential deletion
fails. Unknown/missing identity, expired sessions, rejected permissions,
malformed responses, offline sync, throttling or a changed revision hold files.
Holding and revoking are not the same thing. A file whose uploader cannot be
established right now — Microsoft unreachable, throttled, or the audit event
not delivered yet — is never admitted, but a document already claimed and
queued keeps its claim and its place in the queue, because every pipeline
stage authorizes again before it acts. Only a verdict — disconnection, a
changed account, or an upload that no longer verifies — cancels work in
flight.
The existing `.intern` machine claims still coordinate processing but never
prove a Microsoft uploader. Their sync-based leases are best-effort, not
an exactly-once guarantee across offline machines.

## Network, latency and stored information

With this feature connected, Intern contacts `login.microsoftonline.com` for
sign-in/token refresh and `graph.microsoft.com` for account, file metadata and
audit queries. Searches send item paths and time ranges, not document text.
They create audit-search jobs in Microsoft; this is not a wholly offline mode.
Tokens stay in the OS credential store, never UI responses, shared sidecars or
settings. Public IDs/bindings and bounded attribution history stay in the
app's local data directory. The normal OneDrive sync client still transfers
files; a hosted model, if separately enabled, still receives distilled text.

Audit evidence is not immediate. Microsoft documents typical core-service
event availability of **60 to 90 minutes**, without a guaranteed delivery time.
Intern leaves a document held until evidence arrives. Searches are bounded to
32 pending entries, polled no faster than 30 seconds, recreated after ten
minutes if unresolved, and accepted only with complete pagination (at most
four pages/1,024 records). Accepted event evidence is bound to
tenant/item/ETag/path and kept in memory for a day, so a document that takes
longer than a poll interval to extract, analyse and file is not thrown back
into review by its own later authorization checks; a changed revision has a
different binding and needs its own evidence, and disconnecting or re-pairing
discards all of it. Metadata and local-byte checks still run at each
authorization boundary. HTTP requests are bounded to ten
seconds and 256 KiB; retries respect throttling. Oversized results remain held.
The seven-day admission window is this pilot's policy, not a claim about
Graph audit retention limits.

## Verification before deployment

Synthetic tests cover identity mismatch, UPN binding, unavailable evidence,
ambiguous events, file changes, queue bypass attempts, OAuth state, URL
confinement and UI recovery. They do not establish behavior in your tenant.

Before enabling on real documents, an administrator should test a new upload
from account A and account B, unknown/system/move/overwrite cases, disconnection,
Conditional Access denial, delayed audit delivery and two machines using the
same intake. Confirm only A's verified file is analyzed by A, that no unknown
file is read by the worker/model, and that uploader and actual processor are
shown accurately. Test local/cloud path mapping, hash availability, Windows
Credential Manager persistence, restart, installer and undo behavior. This
build remains a pilot until those checks and the normal release gates pass.

## Primary references

- [DriveItem creator/editor semantics](https://learn.microsoft.com/en-us/graph/api/resources/driveitem?view=graph-rest-1.0)
- [Selected permissions and resource grants](https://learn.microsoft.com/en-us/graph/permissions-selected-overview)
- [Device authorization](https://learn.microsoft.com/en-us/entra/identity-platform/v2-oauth2-device-code)
- [Create auditLogQuery and permissions](https://learn.microsoft.com/en-us/graph/api/security-auditcoreroot-post-auditlogqueries?view=graph-rest-1.0)
- [Audit record identity and operation fields](https://learn.microsoft.com/en-us/graph/api/resources/security-auditlogrecord?view=graph-rest-1.0)
- [Audit delivery timing](https://learn.microsoft.com/en-us/office/office-365-management-api/troubleshooting-the-office-365-management-activity-api)
- [QuickXorHash algorithm](https://learn.microsoft.com/en-us/onedrive/developer/code-snippets/quickxorhash?view=odsp-graph-online)
