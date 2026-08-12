# Model and pipeline bake-off

Every number here was measured on one machine with real local inference over the
whole generated corpus. Nothing is extrapolated from published benchmarks.

**Machine:** AMD Ryzen 7 PRO 8840U, 8 cores / 16 threads, 14.7 GB usable RAM, no
discrete GPU, Windows 11, ordinary background applications running.
**Runtime:** llama.cpp `b10361`, CPU only, 8 threads, 8,192-token context.
**Corpus:** `fixtures/generated`, 20 fixtures. Twelve are text-bearing documents
that reach the model; two are intentionally unreadable (encrypted, malformed);
six require OCR and were not scored on this machine, which has PDFium but no
Tesseract build.

## Raw model speed

`llama-bench`, prompt of 2,048 tokens and 128 generated tokens, 8 threads:

| Model | File size | Prefill (tok/s) | Generation (tok/s) |
| --- | ---: | ---: | ---: |
| **Qwen3.5-2B Q4_K_M** | **1.18 GiB** | **157** | **17.5** |
| Qwen3-1.7B Q4_K_M | 1.03 GiB | 199 | 20.8 |
| Qwen2.5-VL-3B Q4_K_M *(incumbent)* | 1.79 GiB | 75 | 11.3 |
| Qwen3.5-2B Q5_K_M | 1.33 GiB | 78 | 15.9 |
| Qwen3.5-4B Q4_K_M | 2.54 GiB | 48 | 7.3 |

Q5 costs half the prefill speed for no measurable accuracy gain on this corpus
and was dropped. The 4B model is roughly three times slower than the 2B on
prefill and was never a candidate for a laptop that has to stay usable.

## Quality

The bake-off below compares models and pipelines over the same twelve text
documents, which is what makes the rows comparable. The full corpus, including the
six scanned fixtures, is measured separately in
[the section after it](#the-full-corpus). `dates` counts the reviewed answer or a
listed acceptable alternative; `trap dates` counts documents filed under a date the
corpus explicitly marks as wrong (a referenced master agreement's date, a
signature date where an effective date exists, a payment due date).

| Run | Dates | Trap dates | Type | Parties | Description facts | Review rate |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Qwen2.5-VL-3B, **old pipeline** *(what shipped)* | 9/11 | 1 | 3/11 | 8/12 | 8/11 | 25% |
| Qwen3.5-2B, old pipeline | 9/11 | 2 | 4/11 | 7/12 | 6/11 | 42% |
| Qwen2.5-VL-3B, new pipeline | 9/11 | 2 | 9/11 | 7/12 | 8/11 | 42% |
| Qwen3-1.7B, new pipeline | 11/11 | 0 | 9/11 | 7/12 | 9/11 | 33% |
| **Qwen3.5-2B, new pipeline** *(selected)* | **11/11** | **0** | **8/11** | **9/12** | **9/11** | **25%** |

The two comparisons that matter:

* **Same model, old vs new pipeline** isolates the redesign. Document type goes
  from 4/11 to 8/11 and trap dates from 2 to 0. The old pipeline's filenames
  frequently had no type at all (`2025-02-14.pdf`, `2026-12-29 - notice.pdf`)
  because its schema asked for a "subject" and a separate loosely-defined type,
  and its evidence rule discarded whichever the model could not quote exactly.
* **Same pipeline, old vs new model** isolates the model. Dates go from 9/11 with
  2 traps to 11/11 with none, at roughly half the latency and 40% less peak
  memory.

### Before and after

| Document | Old pipeline | New pipeline |
| --- | --- | --- |
| 14-page statement of work | `2026-04-09 - Statement of Work.pdf` *(signature date)* | `2026-04-01 Statement of Work between Ridgeline Cartography LLC and Vistage Worldwide, Inc.pdf` |
| Termination notice | `2026-12-29 - notice.pdf` | `2026-12-29 Notice of Termination - John Smith.pdf` |
| Consulting amendment | `2025-09-14 - amendment - FIRST AMENDMENT TO CONSULTING AGREEMENT.pdf` | `2025-09-14 First Amendment to Consulting Agreement between Vistage Worldwide, Inc and Jane Ellery.pdf` |
| Vendor invoice | `2026-01-05 - invoice.pdf` | `2026-01-05 Invoice between Vistage Worldwide, Inc and Acme Corporation.pdf` |
| Employment agreement | `2025-02-14.pdf` | `2025-02-14 Employment Agreement between Northstar Lantern Works LLC and Mira Vale.pdf` |
| Order form | `2026-01-14 - Order Form.docx` *(signature date)* | `2026-02-01 Order Form between Tessellate Analytics Ltd and Vistage Worldwide, Inc.docx` |
| Ambiguous note | `Document.pdf`, needs review | `Document between Rowan and Priya.pdf`, needs review |

Every filename above is copied from a scored run, including the two that are not
flattering. The vendor invoice reads `between` its two sides where `from Acme
Corporation` would be right, and it leads with the party that was billed; the
direction is under-determined by the document's own layout and the model does not
recover it. An earlier revision of this table showed the correct-looking
`2026-01-05 Invoice from Acme Corporation.pdf`, which the pipeline does not
produce.

### The full corpus

Eighteen documents scored with real inference, including the six scanned fixtures,
which had never been scored at all because the machine had no Tesseract build:

| | Result |
| --- | ---: |
| Date correct, text documents | 13/13 |
| Date correct, whole corpus | 13/17 |
| Date correct where Intern named the file | **9/9** |
| Filed under a corpus-marked trap date | **0** |
| Document type | 13/17 |
| Parties | 13/18 |
| Named a party the corpus marks as not defining | 1/18 |
| Description specific | 18/18 |
| Agreed with the corpus on review-or-name | 16/18 |
| Review rate | 50% |
| Date *role* correct | **6/13** |

Two of these deserve to be read carefully rather than skimmed.

**The four corpus-wide date misses are all scans, and all four are the product
working.** On the lease, the model read `SEPTEMBER 1 24h24` and inferred September
1, 2024 - which is right - and validation threw it out, because "2024" appears
nowhere in the document literally. No date was invented and the file went to
review. Corpus-wide date accuracy therefore has a ceiling set by OCR fidelity, not
by document understanding, which is why the release gate holds the narrower and
absolute promise instead: every file Intern actually renamed carried the right
date.

**The date role is mostly wrong, and it is the one place the model is not
understanding what it reads.** It answers `effective` for every document and has
never once used `notice`, `amendment`, `invoice`, or `issuance`, though all four
are in its grammar. It picks the defining date correctly and cannot say why. The
role never reaches the filename, so no output is wrong because of it, but it was
included as evidence of understanding and it does not currently serve that
purpose. It is now scored on every run rather than being invisible.

The statement of work and the order form are the two documents built to punish a
pipeline that reaches for the easiest date. The old pipeline took the signature
date on both. The new one takes the effective date buried on page five of a
29,000-character contract, and the subscription start date from a table rather
than the "Signed on" line beside it.

## Performance

Measured over the same runs, per document, including extraction:

| | Qwen2.5-VL-3B, old pipeline | Qwen3.5-2B, new pipeline |
| --- | ---: | ---: |
| Median total time | 23.6 s | 12.4-27.7 s |
| Slowest document | 115 s | 51-56 s |
| Peak model process memory | 4,215 MB | 2,470-2,590 MB |
| First-run download | 3.05 GiB *(model + projector)* | 1.19 GiB *(text model only)* |

The new pipeline's spread is wide because it is a laptop measurement, not a
benchmark: repeated runs of the same corpus on the same machine produced medians
of 12.4, 16.6, 19.6, and 27.7 seconds depending on what else was running. The
slowest run was the one taken while other work was competing for the same eight
threads. Quote the range, never one figure.

Extraction is not the cost. A one-page invoice extracts in 13 ms, a 14-page
contract in 33 ms, a 100-page journal in 29 ms. Distillation is 0.3–9 ms. Across
the full 18-document corpus the median extraction is 38 ms. The exception is a
scanned page: OCR is seconds, and the slowest document in the corpus spends 6.6 s
there because a page that will not read confidently is re-read in other
orientations. Essentially all the time is the model:

| Document | Pages | Source chars | Digest chars | Distill | Inference |
| --- | ---: | ---: | ---: | ---: | ---: |
| Vendor invoice | 1 | 641 | 903 | 0.3 ms | 9.7 s |
| 100-page journal | 100 | 9,592 | 103 | 0.5 ms | 9.5 s |
| Settlement agreement | 7 | 14,837 | 12,565 | 5.4 ms | 51.1 s |
| Statement of work | 14 | 29,285 | 13,178 | 9.2 ms | 42.3 s |

Repeated runs of the same corpus on this machine put the median between 12.4 s
and 16.6 s depending on what else is running; treat a single figure as noise.

Short documents are dominated by *generation* — the structured reply is about
240 tokens at 17.5 tokens/second. Long documents add prefill at 157
tokens/second, so the 12,000-character budget costs roughly 19 seconds on the
biggest files. That budget is the tuning lever: shrinking it trades accuracy for
seconds, and 12,000 was where date accuracy stopped improving.

The digest can exceed its budget by one block when the mandatory set alone is
larger, which is why the statement of work reports 13,178 characters. Very short
documents can report a ratio above 1: the page markers and the date index cost
more than the text saved, which is the correct trade when there is nothing to
compress.

## Compression

| Document | Source | Digest | Ratio |
| --- | ---: | ---: | ---: |
| Vendor invoice (1 page) | 641 | 991 | passthrough |
| Settlement agreement (7 pages) | 14,837 | 12,565 | 1.2× |
| Statement of work (14 pages) | 29,285 | 13,178 | 2.2× |
| Project journal (100 pages) | 9,592 | 103 | 93× |

Compression is a consequence of the document, not a configured ratio. The
journal compresses 93× because 100 pages of it differ only by an observation
number; the settlement agreement compresses barely at all because it is already
near the budget.

## What was rejected

* **LLMLingua-2.** A BERT-class encoder plus an ONNX or Python runtime, several
  hundred megabytes, to preprocess for a model that is 1.19 GiB. More decisively,
  it emits a pruned token sequence rather than document text, which destroys
  literal evidence checking and can separate a date from the words that give it
  meaning. See `docs/architecture.md`.
* **Q5_K_M and Q8.** Half the prefill speed for no measured accuracy gain here.
* **Qwen3.5-4B.** Three times slower on prefill; a 29,000-character contract
  would take minutes.
* **A permanently loaded vision projector.** 668 MB resident for a path that a
  text-bearing corpus never takes. It is installed and loaded on demand instead.

## Known misses

Reported rather than tuned away, because twelve documents is a small corpus and
fitting a prompt to it is not the same as being right:

* Two documents (`meeting-minutes.md`, a 100-page project journal) get no
  document type at all and are named `<date> Document.<ext>`. Both are the
  least contract-like fixtures in the corpus. `nda.docx` answers
  `Non-Disclosure Agreement` where the document says `Mutual Non-Disclosure
  Agreement` - correct but less specific than the text supports.
* The vendor invoice reads `between` its two sides rather than `from` the party
  that issued it. The direction is under-determined by the document's own layout.
* The six OCR fixtures are not scored for naming quality. They have since been
  run through the packaged worker against a real Tesseract, and the clean-room
  bitmap font reads at 62-95 mean confidence with digits corrupted often enough
  (`2024` as `24h24`, `2025` as `2625`) that literal-evidence validation cannot
  confirm a date. Every scanned fixture therefore goes to review, which is what
  `expected.json` expects of them and what should happen to a poor scan, but it
  means the corpus does not measure how well Intern names scanned documents.
  `fixtures/README.md` records the measured per-fixture fidelity.
* Running that path for the first time found a real defect rather than a fixture
  problem: Tesseract's orientation detection, which is trained on prose with
  ascenders and descenders, reported a confident but 180-degree-wrong rotation on
  these all-caps forms, and OCR then returned a full page of gibberish with the
  same word count as a real reading. A page that does not read confidently is now
  re-read in the other orientations and the most confident reading wins. On the
  rotated fixture that moved mean confidence from 14-23 to 76 and turned
  unusable output into the document's actual text. Upright pages, which is
  effectively every real document, still cost exactly one OCR pass.
* Those OCR measurements come from UB-Mannheim Tesseract 5.4.0, not the pinned
  vcpkg 5.5.2 that ships; treat the exact confidences as indicative.
