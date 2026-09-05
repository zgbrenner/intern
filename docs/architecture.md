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
workbook cannot flood distillation. `.eml` emails emit a fixed-order header
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
from somewhere else in the page.

The party clause is composed from a validated relation and validated names, not
from free text, so every name in a filename has been found in the document.
Names longer than 120 characters shed the second party, then the party clause,
then truncate the type — detail is lost from the least identifying end first.
Windows-hostile characters, reserved device names, trailing dots and spaces, and
bidirectional control characters are removed; the original extension is always
preserved; collisions get a ` (2)` suffix.

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
adding a caller, not touching the engine.

The watched intake folder — including shared OneDrive/SharePoint intake
folders, network shares, and the multi-machine claim protocol behind them —
lives in `intern-intake` and is documented in
[`shared-intake.md`](shared-intake.md). It sits entirely on the queue side of
this boundary: it decides *which* documents enter the local queue and records
what happened to them, and knows nothing about models.

The queue reports every completed rename to a *filing sink*, and the desktop
app's sink writes the description records that let a SharePoint column carry
the sentence — see [`sharepoint-descriptions.md`](sharepoint-descriptions.md).
The sink hears about a rename only after it has succeeded and cannot undo it;
a record that fails to write is reported in Settings, and the rename stands.
