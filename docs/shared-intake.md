# Shared intake folders, OneDrive, and SharePoint

Intern can watch an **intake folder**: documents that appear in it are analyzed,
named, and moved to the **destination folder** (the same destination setting the
rest of the app uses). Both folders can live inside a OneDrive or SharePoint
library, and several machines can watch the same intake folder without stepping
on each other. This document explains how, and exactly what that costs in
privacy and in guarantees.

## The integration is the sync client, on purpose

Intern connects to OneDrive and SharePoint through the folders the **Microsoft
sync client** already keeps on your disk — the same OneDrive engine that powers
Files On-Demand. Point the intake or destination at any folder under a sync
root: your personal OneDrive, OneDrive for Business, or a SharePoint document
library synced with **Sync** or **Add shortcut to OneDrive**. Intern detects
sync roots and labels the folder in Settings ("Synced with OneDrive – Contoso",
"Synced with SharePoint – Contoso") so you can see what you picked.

What Intern deliberately does **not** do is talk to Microsoft's servers. There
is no Graph API client, no OAuth token, no upload code. The product's core
promise — document text, extracted pages, OCR output, and model prompts never
leave the machine — survives this feature intact, because the only thing moving
files to the cloud is the sync client you already run, under the account you
already audit. Intern still makes exactly the two network requests it always
made, both user-initiated, neither about your documents.

Files On-Demand is handled: an online-only file is a placeholder on disk, and
Intern queues it like any other document. The read that extraction performs is
what makes the sync client download the content. If the machine is offline and
a placeholder cannot hydrate, the document fails to extract and goes to review
instead of being guessed at — keep intake files available, or the machine
online, and it never comes up.

A failure while the content is still in the cloud is not a verdict on the
document, because nothing ever read it. Intern holds the claim open in that
case rather than closing it, and Settings counts the documents waiting on
their contents. When the bytes arrive the claim is released, the next scan
re-acquires it, and the document goes through the pipeline with something to
read. A second failure with the content local is a real failure and is
recorded as one. A laptop that spends a trip offline therefore returns to a
folder it can still work on, instead of one full of tombstones.

## Watching a folder

Enable **Watch a folder** in Settings and pick the intake folder. The watcher
scans on a short interval — polling, not filesystem events, because sync
clients replay changes in ways that make event streams lie, and a periodic
scan is invisible next to a model that takes seconds per document. A
file is picked up once its size and modification time have held still for a
full scan interval, so a document still being copied or synced in is never
read half-written.

Conflict copies are left alone. When two machines edit the same document
before sync catches up, the sync client keeps both and renames the losing side
after the machine that wrote it — `report-DESKTOP-A1B2C3.pdf` — or spells it
out as `report (Jane's conflicted copy 2026-08-31).pdf`. Neither is a new
document, and filing one would put a second copy of something already filed
into the destination. The spelled-out form is unambiguous; the machine-suffix
form is not, because `Invoice-ACME.pdf` is an ordinary filename, so that
suffix is believed only when it names a machine this folder has actually seen
in its presence records. Skipping a document someone meant to file is the
worse of the two mistakes, so the guess is never made on shape alone. Settings
counts what was skipped; resolve the conflict in the folder and the survivor
is picked up on the next scan.

A watched intake requires a destination folder **outside** the intake folder.
Renaming in place inside a watched folder would make every result reappear as
a new document; Intern refuses the configuration at save time rather than
discovering the loop at run time.

A subfolder the scan cannot list — a folder this account has no permission to
open, or one the sync client removed between the listing and the descent — is
counted in Settings and skipped. Everything else in the folder is still
scanned; one locked folder never stops the rest of a share from being filed.
Only the intake root itself has to be readable.

### Finding the folder

The folder a SharePoint library syncs to lives under the user's profile with
a name nobody chose (`C:\Users\pat\Contoso\Legal - Documents`), and finding
it in a folder dialog was the step people got stuck on. Settings therefore
lists the **synced locations** the sync client has registered on the machine
— every SharePoint library and OneDrive account, with its local folder — and
fills the intake or destination field from one click. A subfolder can still
be typed or browsed afterwards.

## Network shares

Nothing here requires a sync client. A folder on a network share — a UNC path
such as `\\fileserver\legal\intake`, or a mapped drive letter Windows reports
as remote — is recognised and labelled *On a network share* in Settings, and
several machines watching the same share coordinate through exactly the same
`.intern/` claim files. The difference is in the guarantees: a share is one
filesystem, so claim creation is genuinely exclusive there, and the
eventual-consistency caveats below do not apply. What the share does not do is
hydrate anything; every file on it is already local to every machine.

## Several machines, one intake folder

When more than one machine watches the same synced folder, they coordinate
through a `.intern/` directory inside it — small JSON files the sync client
replicates like anything else. No server, no shared database: SQLite's
journaling is not safe over a sync engine, so each machine keeps its own queue
and the shared folder carries only **claims**, **origin markers**, **machine
presence**, and **filed markers**.

### Claims

Before a machine processes a document it writes a claim file named by a key
derived from the document's relative path, size, and modification time. The
claim carries the machine's identity and a **lease**. Creation is exclusive:
whoever creates the claim file first owns the document, and everyone else
skips it. While the document is queued or waiting for review the owner renews
the lease; when it is renamed, kept, or fails, the claim is marked **done**
and stays behind as a tombstone so a machine with a lagging sync view cannot
process it again. Done claims are pruned after 30 days.

A crashed or unplugged machine must not strand its documents, so claims can be
taken over — but only when **both** the lease deadline has passed **and** the
owner's heartbeat has been silent for the full lease period. One clock being
wrong, or one sync being slow, is not enough to steal work from a live
machine. This is the same two-factor liveness rule Intern's local queue uses
between processes.

Sync engines are eventually consistent, so claims are honest about being
best-effort: two machines that race a claim while offline can both think they
won until sync catches up. The claim protocol makes double-processing rare;
what makes it *harmless* is the layer below — every rename re-verifies the
document's content hash and the destination is created atomically, so the
worst a lost race produces is one machine finding the file already moved and
routing to review, never a corrupted or overwritten document.

### The same document twice

A claim identifies an *upload* — a path, a size, a modification time — so the
done tombstone stops a lagging machine from processing that upload again. It
says nothing about the same *content* arriving again: a teammate re-sends
last month's agreement under a new name, and every machine's local queue has
only its own history to check it against. Each machine already refuses to
file content it has filed before; the shared folder extends that memory to
the team.

When a machine files a document out of the intake folder it leaves a **filed
marker** under `.intern/filed/`, named by the document's content hash — the
same SHA-256 the queue verifies every rename against — and carrying the
filename it was filed under, where it was in the folder, and which machine
filed it. A machine that later enqueues the same bytes, under any name and
from any uploader, finds the marker and routes the document to review as a
duplicate: *Duplicate of `2026-03-02 Master Services Agreement between Acme
and Globex.pdf` (filed from Front desk)*. Nothing is analysed and nothing is
renamed unless a person chooses **Retry** to process it anyway. Undoing a
filing removes the marker it left, and only that one — a marker another
machine wrote records a filing this machine did not undo. Markers live for a
year, long enough to outlast the annual re-send of a recurring document, and
only documents that came from the intake folder get one: a document filed
from anywhere else on a machine is nobody else's business.

### Whose documents are they?

Each machine records an **origin marker** for files that first appear locally
on it — a file you dropped into the folder on this machine, as opposed to one
that arrived through sync. By default a machine only processes its own
uploads: documents that arrived from teammates, and documents that were
already in the folder before watching started, are counted in Settings as
held, not claimed.

Turning on **"Also process documents uploaded by others"** lets a machine take
unowned and teammate-uploaded documents too — after a courtesy delay of two
minutes, so the uploader's own machine always gets first claim on its own
work. With every machine opted in, the folder behaves as a shared work queue:
first claim wins, leases keep it fair, takeover keeps it live.

### What the shared folder learns about you

The `.intern/` metadata is visible to everyone who can see the folder. It
contains filenames — including the names documents were filed under, which
carry a date, a type, and the parties — file sizes, content hashes, machine
names, and usernames — never document content, text, or model output. If a
filename is itself sensitive, the folder you are sharing already reveals it;
Intern adds the filed name, which the done tombstone carried already, and
nothing beyond that.

The destination folder can carry a second kind of `.intern/` metadata, opt-in:
**description records**, one small JSON file per filed document with the
one-sentence description and the date, type, and parties behind the name, so
a SharePoint column can be filled from them. Those do state what a document
concerns, in one sentence, to everyone who can see the destination — which is
the point of putting them in a library column, and the reason the setting is
off by default. [`sharepoint-descriptions.md`](sharepoint-descriptions.md)
describes the record and the flow that consumes it.

## Failure behavior, stated plainly

- **Machine dies mid-document**: lease expires, heartbeat goes silent, another
  machine takes over after the full lease period (15 minutes).
- **Document fails analysis**: after the local retry it is marked done-failed
  in the shared folder so machines do not ping-pong a poisoned document.
  Retry it manually from the machine that holds it.
- **Sync client holds the file open**: OneDrive locks a file while it uploads
  it, and Windows refuses the open with a sharing violation. Intern waits the
  lock out — five attempts over about three seconds — rather than reporting an
  intact document as changed. A file still held after that is reported as
  locked, naming what actually happened, and can be retried.
- **Claim lost to a sync conflict**: the loser cancels its local queue item;
  if it had already renamed first, the winner finds the file gone and routes
  to review.
- **Offline placeholder**: extraction fails and the document goes to review
  with the reason shown, but the claim is held rather than tombstoned; it is
  released for a fresh attempt once the content arrives.
- **Sync conflict copy appears**: skipped and counted, never filed.
- **Same content uploaded again**: flagged as a duplicate of the filed name
  before analysis, on whichever machine picks it up; retry processes it anyway.
- **Settings misconfiguration**: refused at save time with a specific error,
  not discovered at run time.
