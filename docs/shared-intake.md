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

## Several machines, one intake folder

When more than one machine watches the same synced folder, they coordinate
through a `.intern/` directory inside it — small JSON files the sync client
replicates like anything else. No server, no shared database: SQLite's
journaling is not safe over a sync engine, so each machine keeps its own queue
and the shared folder carries only **claims**, **origin markers**, and
**machine presence**.

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
contains filenames, file sizes, machine names, and usernames — never document
content, text, or model output. If a filename is itself sensitive, the folder
you are sharing already reveals it; Intern adds nothing beyond that.

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
- **Settings misconfiguration**: refused at save time with a specific error,
  not discovered at run time.
