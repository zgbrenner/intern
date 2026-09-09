# Intern architecture

Intern turns a document into a filename, a one-sentence description, and the
evidence behind both. This document explains how, why the pieces are shaped the
way they are, and what the design costs on an ordinary Windows laptop.

Everything below runs locally. The engine speaks only to `127.0.0.1`.

## The stages

```text
DocumentSource ─▶ distill ─▶ prompt ─▶ one local inference ─▶ validate ─▶ compose name
```

There are five stages and no decision tree. Each has one job:

| Stage | Input | Output | Where |
| --- | --- | --- | --- |
| Extract | a file path | pages of text or Markdown | `intern-worker` (separate process) |
| Distill | pages | a verbatim digest under a character budget | `intern-engine::distill` |
| Prompt | digest | one user turn plus a GBNF grammar | `intern-engine::prompt` |
| Infer | prompt | one JSON reply | `intern-engine::client` + llama.cpp |
| Validate | reply + digest | checked facts and review reasons | `intern-engine::validate` |
| Name | checked facts | a Windows-safe filename | `intern-engine::naming` |

`intern-queue` decides *which* document runs and what happens to the result;
`intern-core` makes the state and the file operations survive a crash. Neither
knows anything about models.

## Extraction

Native text first, always. PDFium supplies each page's text and how much of the
page is covered by images. A page goes to OCR only when it has fewer than 20
meaningful characters under heavy image coverage, or when more than 3% of its
characters came back as replacement glyphs. Office containers go through AnyDoc
to Markdown, which preserves headings and tables; plain text and Markdown are
read directly. Excel workbooks are read sheet-per-page as Markdown tables,
capped at 200 rows by 30 columns per sheet with an elision marker so a large
workbook cannot flood distillation. PowerPoint decks go through the same
Office reader as Word documents, slide by slide in order. `.eml` emails and
Outlook `.msg` messages emit a fixed-order header
block — the `Date:` line verbatim, so the sent date is checkable against the
document like any other fact — followed by the plain-text body and a listing
(never an extraction) of attachments.

Two consequences of "OCR only when necessary" are enforced in code rather than
documented as intent:

* The OCR engine is constructed the first time a page actually needs it. A text
  PDF is never delayed by, and never fails because of, an OCR engine that is
  missing or slow to start.
* PDFium is bound once per process and shared. Binding it per document made
  every PDF after the first one in a queue fail as "native assets missing";
  `one_pdf_backend_parses_every_document_in_a_queue` keeps that fixed.
* A page that does not read confidently is re-read in the other orientations.
  Tesseract's orientation detection is trained on prose with ascenders and
  descenders; on a dense all-caps form it can be confidently 180 degrees wrong,
  and OCR then returns a full page of gibberish with the same word count and
  shape as a real reading. Volume cannot tell those apart, so mean word
  confidence arbitrates: one corpus page scored 23, 14, 14, and 76 across the
  four orientations. A page that reads well the first time — every upright
  document — still costs exactly one pass, so the common case is unchanged and
  only a page already headed for a low-confidence warning pays for the search.

## Distillation

The model has a context window and a CPU budget; a 30,000-character contract
has neither. The old pipeline solved this by sending the first 14,000 characters
and the last 8,000 and discarding the middle. That throws away exactly the part
of a long agreement where its term, its fees, and often its effective date live.

Distillation instead reads the whole document:

1. **Segment.** Pages become blocks: headings, paragraphs, table groups.
   Paragraphs longer than 700 characters are split on sentence boundaries so a
   salient sentence can survive independently of the prose around it.
2. **Collapse running lines.** A short block whose digit-masked shape repeats on
   at least half the pages is a running header or footer; only its first
   appearance is kept.
3. **Score.** Each block is scored on cues that answer the three questions a
   filename needs: date cues and date-role phrases ("effective as of", "date of
   this notice", "invoice date"), party cues ("by and between", corporate
   suffixes, `To:`/`From:`), document-type cues, subject cues, signature cues,
   money, and identifiers. Standard clause bodies — governing law, severability,
   entire agreement, counterparts, and their relatives — are demoted hard.
   Position matters a little: the opening names a document and the closing signs
   it.
4. **Select.** Mandatory blocks first (the opening, anything carrying a date with
   a stated role, anything naming parties and a type, subject lines, signature
   blocks), then the highest-scoring remainder, until the budget is spent.
   A block whose text repeats one already kept (clause number aside) is never
   kept twice, and near-duplicate blocks — the same opening or, for long body
   text, the same closing 80 characters with digits masked — never compete
   for budget with text that appears once.
5. **Emit.** Kept blocks are written back **in document order**, with `[Page N]`
   markers, `[...]` where text was removed, a `SECTIONS:` outline of every
   heading found anywhere in the document, and an index of every sentence that
   carries a date. The date index is what turns "which of these dates defines
   the document" from a scanning problem into a reading problem; adding it took
   the corpus from 9 of 11 dates correct to 11 of 11, and eliminated the last
   two cases of filing a document under a referenced agreement's date.

Three properties are load-bearing and each has a test:

* **Nothing is unreachable.** `a_fact_buried_in_the_middle_of_a_long_document_survives`
  builds an eight-page agreement whose effective date is on page five and
  asserts it is in the digest.
* **Kept text is verbatim.** `distillation_never_invents_text` asserts every
  emitted segment is a substring of the source. This is what makes evidence
  checking meaningful.
* **The digest is deterministic.** The same document always produces the same
  digest, so a re-run is reproducible and a cached prompt prefix stays warm.

### Why not LLMLingua-2

LLMLingua-2 was the obvious candidate and was rejected for two reasons, one
practical and one fatal.

The practical one is deployment cost. It is a BERT-class token classifier: an
ONNX runtime plus a 400 MB–1 GB encoder, or a Python runtime, added to a product
whose entire point is to fit comfortably beside Windows on a 16 GB laptop. That
is a large fraction of the main model's footprint spent on preprocessing.

The fatal one is that it deletes tokens. Its output is a compressed token
sequence, not document text — which means no excerpt the model quotes can be
checked against the original, and Intern's anti-hallucination guarantee
disappears. It also, by construction, breaks the relationships the redesign
exists to preserve: "effective as of" and the date it governs can be separated.

Structure-aware extractive distillation gives the same compression on the
documents that matter, keeps text verbatim, costs no extra download, no extra
process, and no measurable memory, and runs in well under a millisecond. It is
implemented in Rust with no dependencies beyond the standard library.

### Budgets

| Source size | Behaviour |
| --- | --- |
| ≤ 12,000 characters | passed through untouched |
| > 12,000 characters | distilled to ≤ 12,000 characters |

Compression is therefore adaptive by construction: the ratio follows the
document rather than a configured number. A one-page invoice is untouched; a
15,000-character settlement agreement compresses 1.2×; a 29,000-character
statement of work compresses 2.2×; a 100-page journal whose pages differ only by
an observation number compresses 93×, to the four lines that actually differ.

The digest can overshoot the budget by one block when the mandatory set alone is
larger than the budget — dropping evidence to hit a round number would be the
wrong trade.

12,000 characters is roughly 3,000 tokens. It was chosen from measurement, not
taste: prefill on the target machine runs at about 160 tokens/second, so every
1,000 characters of budget costs about 1.5 seconds of wall clock on every
document. A larger budget buys nothing on the corpus and costs seconds per file.

## The prompt and the grammar

One inference per document. The reply is constrained by a GBNF grammar, so
whole classes of mistake are impossible rather than filtered afterwards:

* `document_date` can only be `YYYY-MM-DD`.
* `date_role` has no "due", "deadline", or "renewal" member. The model cannot
  propose a payment due date as the document's date because it has no vocabulary
  for it.
* `parties` is capped at three entries.
* The reply contains no whitespace at all. Pretty-printing costs generated
  tokens, and generation is the slowest thing on a CPU.

The prompt teaches date *meaning* rather than a priority order: an agreement is
defined by its effective date, a notice by its notice date or by the termination
it brings about, an invoice by its invoice date, an amendment by its own date and
never by the date of the agreement it amends. A signature date loses to a stated
effective date.

Hybrid-reasoning models are switched out of thinking mode
(`chat_template_kwargs.enable_thinking = false`). Intern needs a form filled in,
not a chain of thought, and thinking tokens are pure latency here.

## Validation

The goal is calibration, not timidity. A proposal goes to review only when a
*specific* thing is wrong with it.

| Fact | Accepted when |
| --- | --- |
| Date | it is a real calendar date **and** is written, in some ordinary human form, in the document — `April 1, 2026`, `1st April 2026`, `01/04/2026`, `01.04.2026`, `4/1/26`, and their relatives, matched as whole tokens so `12/1/2026` never supports February 1 |
| Type | at least 60% of its significant words appear in the document |
| Party | the name appears in the document, verbatim or with punctuation disregarded (`Vistage Worldwide Inc` for a document that writes `Vistage Worldwide, Inc.`); the words themselves are never loosened |
| Description | one sentence, 6–42 words, and every number and capitalised name in it appears in the document, allowing a possessive, a thousands separator, or a hyphen the sentence added |

The date rule is deliberately about the *date*, not about the model's quoted
line. Small models paraphrase their own quotes — answering
"This Agreement is effective as of February 14, 2025" for a document whose line
reads "Effective date: February 14, 2025". The first version of this validation
gated on the quoted wrapper and threw away correct dates on half the corpus. What
must be true is that the date is really in the document, and that is what is
checked. The model's quoted line is still stored and shown to the reviewer.

Self-reported confidence below 0.60 also routes to review, as does any
fact-affecting parser warning, and any document with no defining date or no
specific type.

Three things are then decided from the document rather than from the model,
because a two-billion-parameter model is good at finding facts and poor at
labelling them consistently:

* **The date's role.** The wording in the ninety-odd characters before the
  validated date decides whether it is an effective, execution, invoice,
  notice, termination, amendment, filing, or issuance date - `Effective as
  of`, `Invoice date`, `Notice is hereby given ... on`, `signed on`. A bare
  `Date:` label defers to the document type (an invoice's bare date is its
  invoice date; an amendment's is its own date). The model's stated role is
  used only when the document's wording says nothing. The wording is read
  across a PDF's line wraps - "is dated" on one line and "as of September
  14, 2025" on the next are one sentence - but a line that ended a sentence
  or a label keeps its cue to itself, so a header's `Date of this Notice:`
  never lends `notice` to the sentence under it.
* **A missing type, or a partial one.** When the model offers no document
  type, or one the document does not support, the document's own title - the
  first outline heading, at most eight words, containing a type noun
  (`agreement`, `invoice`, `minutes`, `NDA` ...) - becomes the type, and the
  document is routed to review with `TYPE_INFERRED` so a person confirms the
  title names the document. A document with no such title still gets no
  type. A supported type the title merely completes - `Journal` under
  "MOONLIT ARCHIVE PROJECT JOURNAL" - is completed from the title, whole,
  without a review flag, provided the extra words are plain: not an exhibit
  label, not a party's name, not the `No` a stripped number leaves behind.
* **Who issued an invoice.** An invoice, receipt, account statement, or
  quote is *from* whoever issued it, not *between* its two sides. When the
  model says `between` for one of those types and the document's own layout
  names a customer (`Bill to`, `Sold to`, `Attn`) or an issuer (`Remit to`,
  `From:`), the relation is repaired to `from` the issuing party. Every cue
  nominates an issuer; one nominee settles it, however many cues agree, and
  two leave the model's answer alone. A statement *of work* is an agreement,
  not a statement, and is never repaired this way.

Each is deterministic, unit-tested against the corpus's own date lines and
titles, and never invents a fact: a role, a type, or a relation is inferred
only from text the validation has already found in the document.

## The filename

```text
YYYY-MM-DD <document type> <relation> <party>[ and <party>].<ext>
```

`<relation>` is one of `between`, `for`, `with`, `from`, `to`, or — when the model
declines to state one — a bare `-`, which keeps a validated party in the name
without asserting a relationship the document never established. Only `between`
takes two names; the others take the first. Real names from the scored corpus:

```text
2026-04-01 Statement of Work between Ridgeline Cartography LLC and Vistage Worldwide, Inc.pdf
2026-12-29 Notice of Termination - John Smith.pdf
2025-04-30 Invoice from Nimbus Orchard Supply Co.pdf
Lease Agreement with ORION GLASS STUDIO INC.pdf
```

Those are outputs, not illustrations. The last one carries no date because the
scan gave up no readable one, so it goes to review rather than borrowing a date
from somewhere else in the page. Nor does it become a rename as it stands:
every applied name must begin with a date (`DATE_REQUIRED`, refused at
approval and again at the apply), so the reviewer types one or accepts the
model's unverified reading. The analysis keeps the model's reply beside the
validated facts for exactly that offer — a date the document never states
verbatim is withheld from the name, not lost — and lists every date the
document does state, so a document the model could not date is dated from
its own page in a click, with the file's last-modified date as the labelled
last resort. Whatever date the applied name carries is the date the queue
files under: the layout's year folder and the description record read it
from the name, never from the fact validation withheld.

The party clause is composed from a validated relation and validated names, not
from free text, so every name in a filename has been found in the document.
Names longer than 120 characters shed the second party, then the party clause,
then truncate the type — detail is lost from the least identifying end first.
Windows-hostile characters, reserved device names, trailing dots and spaces, and
bidirectional control characters are removed; the original extension is always
preserved; collisions get a ` (2)` suffix. The engine checks collisions
against the only folder it knows, the document's own; the queue recomposes the
name against the folder the document is actually going to, so a suffix means
a real collision at the destination and never a phantom one at the source.

Where a document lands is the destination folder plus, optionally, a
subfolder the queue derives from the validated facts: the year, the year and
type, the type, or the first party (`2026/Statement of Work/`). A fact the
layout needs but the document lacks sends it to `Undated` or `Unsorted`, never
the root. Folders are created on first use and removed by the undo that
empties them; the destination itself is never removed. An undo puts the
document back and leaves it waiting for a person, not ready to file: ready is
the state the scheduler files from, and with automatic renaming on the same
name would be applied again within the minute, undoing the undo.

Only one document is worked on at a time, so an approval made while the queue
is busy cannot be applied on the spot. It is remembered on the proposal and
applied by the scheduler between documents, under the name the reviewer typed
- a busy queue is not something wrong with the document, and never sends it to
review.

### House style

The document's words are not always the words a person files under.
"Vistage Worldwide, Inc." is "Vistage" to everyone at Vistage, and a
reviewer who fixes that in every name is teaching something the model cannot
learn and validation must not: validation checks that a name is *in* the
document, and "Vistage" alone would pass that check for the wrong reason.

So house style is a separate, deterministic stage that runs after validation
and before naming. A rule maps a spelling as the document writes it (matched
with case, punctuation, and spacing disregarded, words never loosened) to the
spelling the reviewer wrote, for one party or for the document type. The
queue applies the rules in force to the validated proposal, composes the name
from the result, and records which rules fired beside the proposal. The
engine's analysis is untouched: the evidence panel still shows the document's
words, and the description record and the layout folder follow the styled
proposal, so the name, the folder, and the record agree.

Rules are learned only from edits, and only from edits that respell exactly
one field. The approved name is read with the grammar that composed the
proposed one - type, connecting word, party, `and`, party - after stripping
the extension, the date, and any collision suffix, so a reviewer who typed a
date and shortened a party in one go still teaches the party. An edit that
touches two fields, the connecting word, or a name the engine did not compose
teaches nothing: it is a decision about that document. Reading the grammar
rather than diffing matters, because the smallest edit lies: "Acme and
Vistage" becomes "Acme Corp and Vistage Inc" by inserting text one character
into the connector, and a diff would credit it all to one party.

A rule takes effect on the second identical edit (`EDITS_TO_LEARN`), or at
once when a person says "Use now" in Settings, and every document still
waiting is recomposed under it so the queue shows the change immediately.
Respelling a spelling Intern applied maps back to the document's word - the
person changed their mind about the word, not about Intern - and restoring
the document's own spelling retracts the rule. The whole memory is the list
in Settings; nothing is learned that cannot be seen and forgotten there.

## Model and runtime

| | |
| --- | --- |
| Model | Qwen3.5-2B-Instruct, Q4_K_M GGUF (1.19 GiB) |
| Runtime | llama.cpp `b10361`, CPU only |
| Context | 8,192 tokens |
| Threads | half the logical processors, clamped to 2–12 |
| Vision | none. No projector is pinned, downloaded, or loaded, and the request type has no field for an image |

The model is text-first. Essentially every business document has usable text,
and a vision projector costs hundreds of megabytes for a capability used on a
small minority of files. Intern starts the server with `--no-mmproj` and never
starts it any other way: `LlamaServer::start` has exactly one call site, and it
passes `None` for the projector. A page that neither text extraction nor OCR can
read goes to review.

This paragraph used to describe the runtime reloading once with a projector when
a document arrived with an image and little text. No such path exists — the
manifest pins one file, `ModelRole` has one variant, and the table above already
said so. Both statements could not be true.

Threads are half the logical processors on purpose. llama.cpp scales with
physical cores rather than SMT threads, and taking every core makes the rest of
Windows stutter — the product's premise is that it runs while you work.

### A hosted model

The inference is local by default and the local server is the product. The
same position in the pipeline can be filled by a hosted model behind an API
key: the engine's `Proposer` is the one seam, the local client and the hosted
client both implement it, and the distillation, prompt, validation, and naming
on either side do not know which answered.

The hosted client speaks two wire formats — Anthropic's Messages API, and the
chat-completions shape OpenAI defined and most providers and local servers
copy — and sends only what every server understands: the model, the system
instruction, the prompt, and (for Anthropic, where it is required) an output
cap. No sampling knobs, because a parameter one provider rejects is a document
that never gets filed. What goes out is the distilled digest of the document,
condensed but verbatim; what comes back is read through the same JSON
recovery and the same evidence checks as a local reply. A refusal from the
model is reported as one and sends the document to review, never re-routed
elsewhere; a rejected key or an unreachable service pauses the queue rather
than failing the backlog one item at a time; a busy service earns one retry.

The key is stored in the operating system's credential store under Intern's
name, never in the settings file, and never travels anywhere but the address
that was configured — redirects are refused. Plain HTTP is accepted only to
this machine, so a local server can be used without a certificate and a
remote one cannot be used without one. **Test connection** sends the same
calibration document setup uses to check the local model, so a wrong key,
model name, or address is found before a real document is sent.

### The same document twice

Exact duplicates are a hash comparison before analysis. The duplicates people
make are not exact: a second scan of the same page, a PDF exported twice from
the same message, a copy saved again by a program that rewrote its metadata.
So the engine also fingerprints everything the extractor read - a 64-bit
simhash over five-character shingles of the normalised text, hashed with
FNV-1a spelled out in the crate so the value is identical on every machine
and in every build, because the shared filed index carries it between
teammates. Similar text gives similar bits; a second scan with a handful of
misread characters lands within six bits, and two unrelated documents sit
about thirty-two apart.

The queue holds every analysis against the fingerprints of its own filings
and asks the duplicate oracle about other machines. Closeness alone does not
decide: this month's statement and last month's share almost every word, and
a fingerprint barely sees the date and the figures that differ. So the dates
have to agree - the filed name's leading date against the date the analysis
found or the model read - and without a date on one side only a
near-identical text counts. A match sends the document to review with
`NEAR_DUPLICATE`, named after the filing it repeats and the machine that made
it; it is never filed on its own, and an undo forgets the fingerprint.

## Measuring it

Every stage after the model is deterministic, which is what makes accuracy
measurable without the model. `intern-evaluate` records a live run - the text
the worker extracted from each fixture and the reply the model gave, keyed by
the hash of the prompt - and replays it in seconds: distillation, validation,
inference of roles and types, house style, and naming run for real over the
recorded reply, and the corpus is scored against `fixtures/expected.json`. A
committed baseline turns that into a gate: CI replays on every push and fails
when a reviewed answer that was right is now wrong, and a prompt change makes
the recording stale rather than silently scoring replies to a question the
engine no longer asks. [`evaluation.md`](evaluation.md) has the workflow.

## What it costs

Measured on an AMD Ryzen 7 PRO 8840U with 14.7 GB usable RAM, CPU only, with
ordinary applications running:

| | |
| --- | --- |
| Extraction | 13 ms for a one-page invoice, 33 ms for a 14-page contract; 38 ms median across the corpus |
| Extraction, scanned page | seconds, and up to 6.6 s when a page has to be re-read in other orientations |
| Distillation | 0.3 ms to 9 ms |
| Median document, end to end | 12.4-27.7 s across four runs of the same corpus on the same machine |
| 29,000-character contract | 42 s |
| Peak model process memory | 2,470-2,590 MB |
| First-run download | 1.19 GiB, the text model and nothing else |

Quote the latency as a range. Four runs of the same corpus on this machine gave
medians of 12.4, 16.6, 19.6, and 27.7 seconds depending on what else was
competing for the eight threads, and any single figure from that spread is noise.

Almost all of the time is the model, and on short documents most of that is
*generation*, not reading: the structured reply is about 240 tokens at 17.5
tokens per second. The previous pipeline and model took 23.6 s on the median
document and 115 s on its worst, with 4,215 MB of peak memory.

`docs/qa/model-evaluation.json` records one full-corpus evaluation - all 18
scorable fixtures, real inference, the pinned model verified by size and digest -
bound to the commit and release-input hash that produced it. It comes from a
development laptop, not the pinned release runner, and cannot satisfy a release
gate: the release workflow rescores the corpus itself and
`validate-release-evidence.mjs` requires the evidence to name the live run.
`docs/model-bakeoff.md` has the measurements behind the model and pipeline choice,
including what was rejected and what still misses.

## The boundary

`intern-engine` has one entry point:

```rust
let analysis = engine.analyze(&source, "pdf", &existing_names)?;
```

`DocumentSource` in, `DocumentAnalysis` out — filename, description, status,
review reasons, validated facts with evidence, and local timings.
`ENGINE_CONTRACT_VERSION` versions that shape.

`intern-analyze` is that call as a command-line program. The desktop app, the
CLI, and the watched intake folder are all callers of the same function; none
of them can change how documents are understood. Adding a new host means
adding a caller, not touching the engine. Adding a model means implementing
`Proposer`, which is what the hosted client is.

The watched intake folder — including shared OneDrive/SharePoint intake
folders, network shares, and the multi-machine claim protocol behind them —
lives in `intern-intake` and is documented in
[`shared-intake.md`](shared-intake.md). It sits entirely on the queue side of
this boundary: it decides *which* documents enter the local queue and records
what happened to them, and knows nothing about models.

The queue reports every completed rename to a *filing sink*, and the desktop
app's sinks write the description records that let a SharePoint column carry
the sentence — see [`sharepoint-descriptions.md`](sharepoint-descriptions.md)
— and the filed markers of the shared intake folder. A sink hears about a
rename only after it has succeeded and cannot undo it; a record that fails to
write is reported in Settings, and the rename stands. A rename the applier
had to settle afterwards - an ambiguous failure finished by a reconciliation,
here, on the next retry, or on the next recovery pass - is reported the same
way, because what is reported is read from the queue's own record of the
operation rather than from whichever call happened to finish it.

The mirror image is the *duplicate oracle*: before analysing a document the
queue checks its own history for the same content, then asks the oracle,
which in the desktop app reads the shared folder's filed markers. Either
answer routes the document to review as a duplicate, naming what the content
was filed as and, for a teammate's filing, by which machine. Analysis never
runs on a duplicate unless a person asks for it.
