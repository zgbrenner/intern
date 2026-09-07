# Prior art: what other tools do, and what Intern took from them

A research pass over open-source document organisers, run in September 2026
before round six. The question was not "what is fashionable" but "what has
someone already got right that Intern lacks, and what has someone got wrong
that Intern should keep avoiding". Each item below says which.

## What was looked at

| Project | What it is | What stood out |
| --- | --- | --- |
| [paperless-ngx](https://docs.paperless-ngx.com/usage/) | The self-hosted document archive most people mean by "paperless" | A workflow engine (triggers on consumption, filters on filename and path, actions that assign title, correspondent, type, storage path); matching algorithms per tag and correspondent, including an "auto" classifier trained on the user's own assignments; duplicates by checksum only |
| [paperless-gpt](https://github.com/icereed/paperless-gpt) | LLM front-end for paperless-ngx, in Go | Passes the user's existing tags, correspondents, and document types to the model to choose from; manual review before applying; LLM-vision OCR through hosted or Ollama models; per-prompt templates edited in the UI |
| [AI File Sorter](https://github.com/hyperfield/ai-file-sorter) | Qt desktop organiser with embedded llama.cpp | A "learned-behaviour database" of approved decisions that stabilises later suggestions; category whitelists; dry run; undo the last run; bundled local models with hosted ones as an option |
| [LlamaFS](https://github.com/iyaja/llama-fs) | Self-organising file system demo | A watch mode that observes how the person renames and moves files and completes the same organisation for the rest |
| [Offline AI File Organizer](https://github.com/yousefebrahimi0/Offline-AI-File-Organizer) | Local-LLM batch renamer | Dates appended from modification timestamps; predefined category folders; JSON log for reversal |
| [ocrs](https://github.com/robertknight/ocrs) | Pure-Rust OCR engine | A clean toolchain with less preprocessing than Tesseract; early preview, Latin script only, no published accuracy against Tesseract |
| [msg_parser](https://lib.rs/crates/msg_parser) | Rust parser for Outlook `.msg` | Pure Rust, no unsafe code, bounded reader, resolves MAPI named properties, decompresses RTF bodies |
| Simhash ([Charikar, 2002](https://www.researchgate.net/publication/221615307_Probabilistic_Near-Duplicate_Detection_Using_Simhash); [open implementations](https://github.com/seomoz/simhash-py)) | Near-duplicate detection for text at web scale | Similar texts give similar 64-bit fingerprints; a Hamming distance of a few bits means one document |

## Taken this round

**Near-duplicates by text fingerprint.** Every tool above that handles
duplicates at all handles them by checksum, which catches the same bytes
twice and nothing else. The duplicates people actually make are a second
scan of the same page, a PDF exported twice from the same email, and a copy
saved again by a program that rewrote its metadata - all different bytes.
Intern now fingerprints the extracted text (simhash over character
shingles, a hash spelled out so it is identical on every machine) and holds
a new document against the queue's own filings and the shared filed index.
The twist the literature does not need and a filing tool does: two
documents that share their words are not always one document. This month's
statement and last month's differ in a date and a few figures, which a
fingerprint barely sees, so Intern also compares the dates, and without a
date on one side only a near-identical text counts. A near-duplicate waits
for a person, named after the filing it repeats, and is never filed on its
own. Details in [architecture.md](architecture.md#the-same-document-twice).

**Outlook `.msg`, natively.** Most desktop organisers skip Outlook's own
format, and paperless-ngx needs Apache Tika beside it to read one. Dragging
a message out of Outlook produces `.msg`, not `.eml`, so this was the gap
Outlook users hit first. Intern reads `.msg` through `msg_parser` and renders
it as the same page an `.eml` becomes, so the email's sent date, sender,
subject, body, and attachment names reach the model the same way.

**PowerPoint.** AI File Sorter reads Office decks; Intern's Office parser
already could, and now the queue routes `.pptx`, `.pptm`, and `.ppsx` to it.
A three-slide deck joined the gold corpus so the claim is measured, not
assumed.

## Already in Intern, under another name

* AI File Sorter's learned-behaviour database is Intern's house style: the
  spellings learned from review edits, listed in Settings, applied on the
  second identical edit.
* Its dry run and preview are Intern's review queue; its "undo last run" is
  the per-rename receipt journal, which undoes any rename, not only the last.
* paperless-ngx's "auto" matcher learns from the user's assignments over
  time. Intern learns names rather than tags, deterministically and visibly,
  because a classifier's reasons cannot be shown in a list and forgotten.
* Every one of these projects that runs locally treats it as a feature;
  Intern treats it as the default and the hosted model as the exception.

## Considered and not taken, with reasons

* **Passing the user's vocabulary to the model** (paperless-gpt,
  paperless-ai). Intern's evidence rule already refuses a party or type the
  document does not contain, so the model can only choose among the
  document's own words; a vocabulary would bias that choice toward names the
  user files under. Plausible, but a prompt change, which makes the corpus
  recording stale and has to be re-recorded and measured - and the corpus
  has no history to measure it with. The next candidate once it does.
* **LLM-vision OCR** (paperless-gpt). It sends page images to a model. With
  the local text-only model as the default and a hosted model as an explicit
  opt-in, a vision fallback would either be unavailable by default or move
  images off the machine; neither fits the promise on the setup screen.
* **ocrs** in place of Tesseract. Early preview, Latin only, no accuracy
  numbers against the engine it would replace. Revisit when it publishes
  them; the OCR fixtures in the corpus are the test.
* **Workflow rules per folder** (paperless-ngx). Real value, ranked below
  the items above. The house-style store is the first piece of the machinery
  a rules list would need.
* **Learning folder moves in watch mode** (LlamaFS). Intern learns what
  people call things; learning where they put them needs a layout model
  richer than year, type, and party first.
* **Mail-fetching intake** (paperless-ngx). Network, credentials, and a
  mailbox to read: out of scope for a tool whose point is that nothing
  leaves the machine.
* **Predefined category folders and timestamp dates** (Offline AI File
  Organizer). Intern derives folders from validated facts and dates from the
  document's own wording; a modification timestamp is the labelled last
  resort, never the default.
