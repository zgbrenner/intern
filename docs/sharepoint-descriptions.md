# Descriptions in a SharePoint column

Intern produces two things about every document: a filename and a
one-sentence description. The filename travels with the file. The sentence
used to stay in the local queue, where nobody else could see it.

A SharePoint document library has a natural home for that sentence — a
column — and the sync client that carries the file can carry a small record
beside it. This document describes the record Intern writes, the flow that
copies it into a column, and what the design deliberately does not do.

## What Intern does

With **Write a description record for each filed document** turned on in
Settings (under *Descriptions for SharePoint*), every rename Intern completes
into the destination folder also writes one JSON file under

```text
<destination>\.intern\descriptions\<key>.json
```

`<destination>` is the destination folder chosen in Settings. The record
carries the document's new filename, where it is, the description, and the
facts behind the name. Nothing in a record is document text: not a page, not
an excerpt, not the evidence quotations. If the destination is a SharePoint
library synced with the OneDrive client, the sync client uploads the record
the same way it uploads the renamed document, and a flow in the library can
read it.

The setting needs a destination folder, because that is where the records
live; Settings refuses to save it without one. Undoing a rename removes the
record for the path the document vacated. Re-filing a document at the same
path overwrites its record, so a document has at most one record at a time.
Documents renamed in place (no destination configured) get no record.

Intern itself makes no network request for any of this. The two requests
described in the README — the one-off model download and the update check —
remain the only ones.

## The record

```json
{
  "version": 1,
  "key": "5d3f0c0e8b7a4f1e9c2d6a8b1f3e7d9c",
  "filename": "2026-04-01 Statement of Work between Ridgeline Cartography LLC and Vistage Worldwide, Inc.pdf",
  "path": "Contracts/2026/2026-04-01 Statement of Work between Ridgeline Cartography LLC and Vistage Worldwide, Inc.pdf",
  "libraryPath": "Contracts/2026/2026-04-01 Statement of Work between Ridgeline Cartography LLC and Vistage Worldwide, Inc.pdf",
  "library": "Vistage Worldwide",
  "provider": "sharepoint",
  "description": "Statement of work between Ridgeline Cartography LLC and Vistage Worldwide, Inc. for the 2026 member-map engagement, at a fixed fee of $248,000.",
  "documentDate": "2026-04-01",
  "documentType": "Statement of Work",
  "parties": ["Ridgeline Cartography LLC", "Vistage Worldwide, Inc."],
  "confidence": 0.93,
  "originalFilename": "scan0012.pdf",
  "filedAt": 1775001600,
  "machineId": "0123456789abcdef0123456789abcdef",
  "machineName": "Front desk",
  "userName": "pat"
}
```

| Field | Meaning |
| --- | --- |
| `version` | Record format version. This document describes version 1. |
| `key` | The record's own name (`<key>.json`): the first 32 hex characters of the SHA-256 of `path`, lowercased and `/`-separated, so machines that spell a path differently agree on the record. |
| `filename` | The document's filename after filing. |
| `path` | The document's path relative to the destination folder, `/`-separated. |
| `libraryPath` | The document's path relative to the SharePoint library (or OneDrive) the destination is synced from. Present only when the destination lies inside a sync root the OneDrive client has registered on the machine that filed it. |
| `library` | The sync root's display name: the tenant for a SharePoint library, the account for a OneDrive. Present with `libraryPath`. |
| `provider` | `sharepoint`, `onedrive_business`, `onedrive_personal`, or `network_share`. Present with `libraryPath`. |
| `description` | The sentence that was applied — Intern's, or the reviewer's edit of it. |
| `documentDate`, `documentType`, `parties`, `confidence` | The validated facts behind the filename, as the review inspector showed them. Any of the first three may be `null` or empty when the document went through review without one. |
| `originalFilename` | The name the document arrived with. |
| `filedAt` | Unix seconds when the rename completed. |
| `machineId`, `machineName`, `userName` | Which machine and account filed it, as in the shared-intake claim files. |

Absent optional fields are omitted, not written as `null`. The folder also
contains a `README.txt` explaining itself to whoever finds it.

## Filling the column with Power Automate

The recipe below fills a column called **Description** in the library. It
runs under the SharePoint connection of whoever creates the flow, so that
person needs edit rights on the library. Adjust the names to match yours.

1. **Add the column.** In the library, choose *Add column → Text* (or
   *Multiple lines of text*, if descriptions are long), name it
   `Description`, and save. Note the column's internal name if you later
   need it in an expression: for a column created with this name it is
   `Description`, but a library that already had a hidden field with that
   name gets `Description0`. The internal name appears in the column's
   settings URL after `Field=`.
2. **Create the flow.** In Power Automate, create an automated cloud flow
   with the SharePoint trigger **When a file is created (properties only)**.
   Site Address: your site. Library Name: the library. Folder: the records
   folder, which is `<destination>/.intern/descriptions` written as a
   library-relative path — for a destination of `Contracts` inside the
   library, `/Contracts/.intern/descriptions`.
3. **Read the record.** Add **Get file content** (SharePoint), with
   Identifier set to the trigger's *Identifier*. Then add **Parse JSON** on
   its *File Content* with this schema:

   ```json
   {
     "type": "object",
     "properties": {
       "version": { "type": "integer" },
       "filename": { "type": "string" },
       "path": { "type": "string" },
       "libraryPath": { "type": "string" },
       "description": { "type": "string" },
       "documentDate": { "type": ["string", "null"] },
       "documentType": { "type": ["string", "null"] },
       "parties": { "type": "array", "items": { "type": "string" } }
     },
     "required": ["version", "filename", "path", "description"]
   }
   ```

4. **Find the document.** Add **Get file metadata using path**. File Path is
   the document's library-relative path. When `libraryPath` is present, it
   is exactly that: `concat('/', body('Parse_JSON')?['libraryPath'])`. When
   it is not (the destination was not inside a registered sync root), prefix
   the destination's own library-relative folder yourself:
   `concat('/Contracts/', body('Parse_JSON')?['path'])`.
5. **Write the column.** Add **Update file properties**. Library Name: the
   library. Id: the *ItemId* from the previous step. Description: the
   `description` from Parse JSON.
6. **Guard against records that arrive before their documents.** The sync
   client uploads the document and its record independently, and the record
   can land first. Put steps 4 and 5 inside a **Do until** loop (or add a
   **Delay** of a minute before step 4 and configure the flow's *Retry policy*
   on step 4 to retry on failure). A record for a document the flow never
   finds is a document that was undone before the flow ran; the record is
   removed by the same undo, and the flow can simply end.
7. **Test it.** File one document with Intern and watch the flow run. The
   column fills in within a minute or two of the sync client uploading the
   record.

A **Trigger condition** on the trigger keeps the flow from running on the
folder's `README.txt`:

```text
@endsWith(triggerOutputs()?['body/{FilenameWithExtension}'], '.json')
```

### If the flow is not an option

The rename history's CSV export (Completed → History → Export CSV) has a
`description` column beside the original and new paths. Opened in Excel, the
sentences can be copied into the library in **Edit in grid view**, one column
at a time. It is manual, but it needs no flow and no admin.

## Network shares and other destinations

A destination on a network share — a UNC path like `\\fileserver\legal`, or a
mapped drive letter — gets the same records under
`<destination>\.intern\descriptions`, with `libraryPath` absent. Settings
labels such a folder *On a network share*, and a watched intake folder on a
share coordinates several machines through the same `.intern` claim files it
uses under OneDrive. Any script or indexer that can read the share can read
the records.

## What this design does not do, and why

**It does not call Microsoft Graph or the SharePoint REST API.** Doing so
would give Intern a sign-in, a token to store, an app registration for an
administrator to consent to, and a third network connection to audit — for a
product whose central promise is that it makes exactly two. The record
approach keeps that promise intact: the only thing moving anything to the
cloud is the sync client already installed and already trusted.

If a direct writer is ever wanted, the shape is known: an Entra app
registration with delegated `Sites.ReadWrite.All` (or `Files.ReadWrite.All`
for OneDrive), a device-code sign-in, a `PATCH` to
`/drives/{drive}/items/{item}/listItem/fields` per filed document, and a way
to resolve a local path to a drive item — either the library URL entered in
Settings plus `/shares/{encoded url}/driveItem`, or the sync client's own
`libraryScope`/`libraryFolder` lines in its settings files. Each of those is
a trust decision for the people who run the tenant, not a default for a tool
that reads private documents. It would be built as an explicit opt-in, and
documented as the third request.

**It does not modify the documents.** Office files can carry custom
properties that SharePoint promotes into columns on upload, and PDFs can
carry XMP metadata; writing the description into the file itself would put
the sentence somewhere every viewer shows it. But it would also change the
file's bytes after Intern hashed and verified them, which is exactly what the
rename's verification exists to notice, and PDFs — most of what Intern files
— get no column from it anyway. Records beside the file keep the file
untouched.
