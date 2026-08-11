# Intern v1 Design Specification

**Status:** Approved product direction, pending written-spec review

**Date:** 2026-08-11

**Platform:** Windows 10 and 11, x86-64

**Design reference:** [`docs/design/intern-primary-screen.png`](../../design/intern-primary-screen.png)

## Summary

Intern is a small, local-first desktop utility that turns poorly named business and legal documents into consistently named, described files. A user drops files or folders into a persistent queue. Intern processes one document at a time and proposes a date, document type, identifying subject, filename, and one-sentence description. Supported, high-confidence proposals become Ready. Uncertain proposals go to Needs Review. Intern never sends document contents off the device.

The product deliberately excludes BackLog's SharePoint and Power Automate delivery contracts, classifier chains, NER and relationship models, semantic rankers, model escalation, folder-watching appliance behavior, and compliance-reporting surface. Intern has one parser path, one Qwen call per document in the normal case, one validator, one queue, and one safe file-operation path.

## Goals

1. Produce useful filenames and descriptions without inventing dates, parties, or subjects.
2. Run on a normal 16 GB Windows computer without a dedicated GPU.
3. Require no terminal, Python environment, Docker, account, API key, or separately installed model manager.
4. Keep document contents and derived results local.
5. Make uncertain results easy to recognize and correct.
6. Recover cleanly after process, model, parser, or application interruption.
7. Ship as a normal per-user Windows installer and downloadable GitHub release.

## Non-goals

- Document management, search, retention, SharePoint delivery, or cloud synchronization.
- Recursive watched folders or always-running system-tray behavior.
- Clause extraction, legal analysis, compliance classification, or full-document question answering.
- Recreating Word, PDF, or document previews beyond the pages needed for review.
- User-facing model selection, prompt editing, workflow construction, or inference tuning.
- macOS or Linux installers in v1.
- Legacy `.doc` conversion, handwriting transcription, or password recovery for encrypted files.
- An automatic updater in the first testing release.

## Supported inputs

The v1 support contract is:

- PDF, including text PDFs, scanned PDFs, and mixed text/image PDFs
- DOCX
- TXT and Markdown
- PNG, JPEG, and TIFF document images

Folder drops are expanded recursively without following symbolic links or Windows junctions. Hidden files, Office lock files beginning with `~$`, zero-byte files, and unsupported extensions are skipped with a visible explanation. An encrypted or malformed supported file becomes a failed queue item and does not block later files.

## Architecture

### Desktop application

The app uses Tauri 2 with a React, TypeScript, and Vite frontend. The webview renders the interface but receives no generic shell or unrestricted filesystem access. All privileged work is exposed through narrowly scoped Tauri commands.

The Rust application backend owns:

- queue orchestration and cancellation;
- SQLite persistence and migrations;
- model and parser worker lifecycle;
- model download and integrity verification;
- proposal validation and filename composition;
- safe rename, move, collision, and undo operations;
- filesystem event checks and recovery after restart.

### Parser worker

Parsing runs in a separate long-lived Rust process named `intern-worker.exe`. Isolation prevents a native PDF or OCR crash, leak, or timeout from taking down the UI. The worker communicates with the Tauri backend through versioned JSON Lines over standard input and output. Its v1 commands are `hello`, `parse`, `cancel`, and `shutdown`; progress events include document ID, stage, current page, and total pages.

The worker uses:

- a pinned AnyDoc release behind an internal `DocumentExtractor` adapter for DOCX and declarative text formats;
- `pdfium-render` and a pinned bundled `pdfium.dll` for PDF text, page geometry, image coverage, and rendering;
- a pinned Tesseract 5 build with English `tessdata_fast` and orientation data for OCR;
- bounded temporary files under Intern's LocalAppData directory.

AnyDoc is intentionally hidden behind an adapter because its current implementation is new. The adapter's output is Intern's own stable `ExtractedDocument` type, so replacing AnyDoc does not affect the queue, model prompt, or UI.

### Model runtime

Intern bundles a pinned Windows CPU build of `llama-server.exe` from llama.cpp. The initial model is Qwen2.5-VL-3B-Instruct using:

- `Qwen2.5-VL-3B-Instruct-Q4_K_M.gguf` for the language model;
- `mmproj-Qwen2.5-VL-3B-Instruct-f16.gguf` for vision.

The combined immutable download is approximately 3.27 GB. Intern runs one server slot, one document at a time, an 8,192-token context, CPU threads based on physical cores, no GPU offload, and a randomly selected localhost port protected by a per-process API key. The server remains warm while the queue is active and stops when Intern exits.

The release model remains Q4_K_M only if the representative acceptance corpus shows no material loss against Q8_0. A material loss is more than two percentage points in accepted-field accuracy or any increase in unsupported dates or parties among Ready proposals. If Q4_K_M fails that gate, v1 ships Q8_0 with the same F16 vision projector. This is a release decision, not a user-facing setting.

### Persistence

Intern stores queue state in SQLite through `rusqlite`, using WAL mode, a busy timeout, foreign keys, and versioned migrations. The database lives under LocalAppData and contains paths, content fingerprints, proposals, confidence, review reasons, error codes, and file-operation receipts. It does not store complete extracted document text or rendered pages after processing finishes.

Temporary extraction and page images are deleted after the item reaches Ready, Needs Review, Failed, or Completed. A Clear History action removes terminal queue records and cached previews without touching user documents or the installed model.

SQLCipher is not included in v1. The database contains less information than the source files already present on disk, and SQLCipher would add build and key-recovery complexity without protecting the source documents themselves.

## Document processing

### 1. Ingestion

For each supported path, Intern records the canonical path, extension, byte length, modified time, and SHA-256 content hash. The hash is calculated as a streaming operation. The original file is never modified during extraction or inference.

Adding the same unchanged path twice focuses the existing queue item instead of creating a duplicate. Identical content at different paths remains separate because file identity includes both the content hash and normalized canonical path.

### 2. Extraction and OCR routing

PDFium extracts native text and page geometry from every PDF page. A page is routed to OCR when any of the following initial rules is true:

- it has fewer than 20 meaningful native characters and raster images cover at least 65 percent of the page;
- more than 3 percent of extracted characters are replacement or unmapped glyphs;
- the input is a document image rather than a PDF page.

Thresholds are constants covered by tests and may be adjusted only from corpus evidence. Intern OCRs flagged pages sequentially at 300 DPI and holds no more than one rendered OCR page in memory at a time. Clean native text is retained; OCR does not replace trustworthy native text on mixed pages.

Each extracted block records its page number, source (`native` or `ocr`), text, and OCR confidence when applicable. The worker reports parser warnings and whether any page was truncated, skipped, or unreadable.

### 3. Vision decision

Text-based documents use no vision input. Intern includes one rendered page image in the Qwen request only when:

- the input itself is an image;
- a scanned or mixed PDF's mean OCR confidence is below 75; or
- the first substantive page contains a large non-text region and extraction leaves less than 100 meaningful characters.

The selected image is the first page that triggered the rule. It is auto-rotated, converted to RGB, resized without distortion, limited to a 1,344-pixel long edge, and padded to dimensions compatible with Qwen's 28-pixel image grid. Intern never sends more than one image in a v1 model call.

### 4. Document packet

The model receives page-marked text, parser warnings, and the optional page image. A text-only packet contains at most 22,000 Unicode characters: the first 14,000 characters and final 8,000 characters after whitespace normalization. A packet with an image contains at most 12,000 characters: the first 8,000 and final 4,000. If the document fits, Intern includes it in full.

This head-and-tail policy captures titles, parties, operative dates, and signature blocks without adding semantic ranking, NER, or another model call. Truncation is disclosed to Qwen and lowers readiness when the necessary evidence is missing.

### 5. Primary model call

The saved prompt tells Qwen to identify only supported information, prefer `null` over invention, and distinguish a document's operative date from dates merely mentioned in its contents. Date priority is effective date, execution or signature date, issue or filing date, then a clearly labeled document date. An unrelated deadline, historical date, invoice line-item date, or date from quoted material is not a document date.

The request uses a small llama.cpp GBNF grammar rather than a complex schema with references. The JSON response has this shape:

```json
{
  "document_date": "2024-04-12",
  "date_kind": "effective",
  "document_type": "Employment Agreement",
  "filename_subject": "John Smith",
  "parties": ["John Smith", "Acme Corporation"],
  "description": "Employment agreement between Acme Corporation and John Smith governing the terms of Smith's employment.",
  "confidence": 0.94,
  "needs_review": false,
  "review_reasons": [],
  "date_evidence": "effective as of April 12, 2024",
  "type_evidence": "EMPLOYMENT AGREEMENT",
  "subject_evidence": "John Smith",
  "party_evidence": ["John Smith", "Acme Corporation"]
}
```

`document_date`, `date_kind`, `document_type`, `filename_subject`, and each evidence field may be `null` when unsupported. `parties`, `review_reasons`, and `party_evidence` may be empty arrays. The description must be one grammatical sentence of no more than 30 words. Confidence is a number from 0 through 1.

The normal path uses exactly one model request. Intern permits one constrained retry only when the server stops mid-request or the response cannot be parsed against the grammar and local schema. A semantic disagreement or low confidence goes to review rather than triggering a second opinion.

### 6. Validation and filename composition

The validator, not the model, composes the final name. The default convention is:

`YYYY-MM-DD - Document Type - Primary Party or Subject.ext`

Segments without supported values are omitted. Intern never inserts `Unknown`, `Untitled`, or an inferred year. Examples:

- `2024-04-12 - Employment Agreement - John Smith.pdf`
- `2023-09-15 - Commercial Lease - 123 Main St.pdf`
- `Nondisclosure Agreement - Acme Corporation and Blue Sky LLC.docx`

The validator performs these checks:

- the JSON shape and value types are valid;
- every date parses as a real ISO calendar date and its normalized evidence occurs in the document packet;
- each party and filename subject has corresponding evidence present in the packet after Unicode normalization and case folding;
- evidence strings themselves occur in the packet;
- description punctuation produces exactly one sentence and contains no unsupported date or party;
- filename segments contain no Windows-invalid characters, control characters, path separators, bidirectional overrides, reserved device names, trailing spaces, or trailing periods;
- the final filename, including extension and collision suffix, is no longer than 140 Unicode characters;
- the original extension is preserved in lowercase;
- the source file still matches its ingest fingerprint before any file operation.

An unsupported date or identifier is removed from the proposal and produces Needs Review. Intern does not silently keep a field after its evidence fails.

An item is Ready only when all of the following are true:

- model confidence is at least 0.86;
- `needs_review` is false;
- no parser or model warning affects the proposed fields;
- every included date, party, and subject passes evidence validation;
- the description and filename pass local validation;
- the source file remains unchanged.

Everything else becomes Needs Review or Failed with a short, machine-readable reason and plain-language explanation.

## Queue state and recovery

The persisted state machine is:

`queued -> extracting -> analyzing -> ready | needs_review | failed | canceled`, followed by `ready | needs_review -> applying -> completed` after user or automatic approval. Retrying a failed or canceled item returns it to queued.

Only one item may be in extracting, analyzing, or applying at a time. A paused queue finishes the current atomic stage and starts no new item. Canceling an item terminates its current worker request, removes temporary files, and marks it canceled; the user may retry it.

On startup, Intern reconciles interrupted items:

- extracting and analyzing return to queued with their attempt count incremented;
- applying checks the operation receipt and both filesystem paths before deciding whether to complete, roll back, or require review;
- an item stops retrying automatically after two failed processing attempts.

A worker crash restarts the worker once and requeues the active item. A second worker crash on that item marks it Failed and continues the batch.

## Review and file operations

Ready items are not changed automatically by default. The user can select Apply all ready or enable Automatically rename high-confidence files in the compact Settings screen.

The output mode is either:

1. Rename in place, which is the default.
2. Move to one user-selected output folder while preserving no source subfolder hierarchy.

Intern never overwrites an existing destination. It reserves the base filename, then ` (2)`, ` (3)`, and so on. A same-volume rename uses the operating system's atomic rename operation. A cross-volume move writes to a temporary destination, streams and verifies SHA-256, atomically renames the temporary file to its reserved final name, and only then deletes the source. If source deletion fails, Intern keeps both verified files and puts the item in Needs Review with both paths shown.

Completed items retain an Undo action. Undo is available only when the destination still matches the recorded post-operation hash and the original path is free. It uses the same no-overwrite and cross-volume safety rules. Keep original completes an item with a `kept_original` outcome and performs no filesystem operation.

## User interface

Intern is a single-window utility with three destinations:

- Queue
- Needs Review
- Completed

The top bar contains Intern, a `Private · On this device` indicator, Add files, and Add folder. The primary area is a compact document list with original filename, status, proposed filename, and confidence. Unprocessed Waiting rows show no proposed filename or confidence.

Selecting a Ready or Needs Review item opens a narrow inspector containing:

- editable Filename and Description fields;
- Date, Type, Parties, and supporting evidence;
- a short reason for review when applicable;
- Approve & rename, Keep original, Retry, and Remove from queue actions as appropriate.

Correcting a field is a local edit and does not call the model again. A corrected proposal is marked User edited and may be applied immediately after deterministic filename validation.

The app uses true white and cool gray surfaces, charcoal text, restrained indigo for primary actions, amber for review, and muted green for Ready and Completed. It uses a list and inspector, not a dashboard, card grid, chat interface, or analytics surface. The accepted design image controls layout, density, typography relationships, and interaction placement; implementation must remove the duplicate product wordmark visible in the generated window chrome.

The only v1 settings are output mode, output folder, automatic high-confidence rename, and Clear History. Model files and parser diagnostics live behind an About Intern dialog rather than the primary workflow.

### First-run setup

The installer contains Intern, `intern-worker.exe`, llama.cpp, PDFium, Tesseract, English OCR data, and third-party notices. The model weights are not in the installer.

On first run, Intern shows a single setup screen explaining that it will download approximately 3.3 GB of model files and that document contents stay on the device. Setup supports pause, resume through HTTP range requests, retry, disk-space checks, and a Choose existing model files option. Each file is written with a temporary extension and becomes active only after its pinned size and SHA-256 match the versioned model manifest embedded in the installer. Intern then starts llama.cpp and runs a small text and image self-test before opening the queue.

After setup, processing has no network dependency. Intern makes no network request during document processing. The app displays a visible error rather than falling back to a cloud service.

## Privacy and security

- All extraction, OCR, inference, validation, and file changes run locally.
- No telemetry, crash upload, analytics, cloud API, or remote logging is included in v1.
- Network capability is limited to explicit model setup and user-initiated opening of release or help links.
- The llama server binds only to `127.0.0.1`, uses a random high port and per-process API key, and accepts no filesystem media path broader than Intern's temporary directory.
- The webview cannot spawn arbitrary commands or read arbitrary paths.
- Parser limits are 1 GB per source file, 500 pages, 1 GB of decompressed Office XML and assets, 2 GB of temporary disk use per item, 25 megapixels per rendered page, 30 minutes for extraction and OCR, and 15 minutes for inference. Files exceeding a limit fail visibly rather than consuming unbounded resources.
- Temporary files use unique private directories and are removed on normal completion and startup recovery.
- Release artifacts include an SBOM, third-party notices, and model/runtime hashes.

## Error handling

Errors use stable codes and user-facing explanations. The minimum v1 codes are:

- `UNSUPPORTED_FORMAT`
- `FILE_CHANGED`
- `FILE_MISSING`
- `FILE_ENCRYPTED`
- `PARSE_FAILED`
- `OCR_FAILED`
- `RESOURCE_LIMIT`
- `MODEL_NOT_READY`
- `MODEL_TIMEOUT`
- `MODEL_RESPONSE_INVALID`
- `EVIDENCE_MISSING`
- `NAME_INVALID`
- `DESTINATION_UNAVAILABLE`
- `MOVE_VERIFICATION_FAILED`
- `SOURCE_DELETE_FAILED`

Document-derived text is not written to ordinary logs. Developer logs contain document IDs, stages, timings, counts, and error codes. A user may copy a diagnostic summary that excludes extracted text by default.

## Testing and acceptance

Implementation follows test-driven development for queue behavior, validation, persistence, and file operations. The test suite includes:

### Unit tests

- Windows filename sanitization, reserved names, Unicode controls, extensions, length, and collisions
- valid, impossible, ambiguous, unrelated, and conflicting dates
- evidence normalization and rejection of unsupported dates, parties, and subjects
- one-sentence description validation
- queue transitions, claims, pause, cancel, retry limits, and restart reconciliation
- same-volume rename, cross-volume move, hash verification, failure boundaries, and undo
- document packet head/tail construction and vision-routing thresholds

### Parser fixtures

Intern creates its own redistributable fixtures rather than copying BackLog fixtures:

- text PDF employment agreement
- image-only scanned lease
- mixed PDF containing native text and a scanned signature page
- DOCX nondisclosure agreement with tables, header, footer, and footnotes
- invoice with multiple unrelated dates
- meeting minutes
- blank, malformed, encrypted, rotated, low-resolution, and 100-page PDFs
- PNG, JPEG, and multipage TIFF document images
- folder batch containing supported, unsupported, duplicate, and Office lock files

Parser integration tests execute the packaged worker and real PDFium/Tesseract assets, not substitutes.

### Model evaluation

A versioned gold corpus records supported date, type, filename subject, parties, expected readiness, and acceptable description facts. Q4_K_M and Q8_0 run with the production prompt and runtime settings. The release gate requires:

- zero unsupported dates or parties among Ready proposals;
- 100 percent syntactically valid results after the permitted retry;
- no more than two percentage points of accepted-field accuracy loss from Q8_0 if Q4_K_M ships;
- every intentionally ambiguous fixture in Needs Review;
- no model or parser failure blocking the next queue item.

### End-to-end and visual tests

- drag individual files and nested folders into the real Tauri window;
- process a mixed batch sequentially and recover after forced app, worker, and model termination;
- edit a review item, approve it, verify the file operation, and undo it;
- verify keyboard navigation, focus states, screen-reader names, 125 and 150 percent Windows display scaling, and 1,024-pixel-wide laptop layout;
- compare the implemented primary screen directly against the accepted design reference at its native dimensions and record a fidelity ledger;
- run the built NSIS installer on a clean Windows environment, complete model setup, process representative PDF, DOCX, scan, and mixed-batch fixtures, then uninstall without removing user documents.

No release is created unless Rust tests, frontend tests, parser integration tests, the Windows build, installer smoke test, and representative real-model smoke test succeed.

## Packaging and release

GitHub Actions builds Windows x86-64 using pinned Rust, Node, Tauri, llama.cpp, PDFium, Tesseract, and model-manifest versions. It produces:

- a per-user NSIS setup executable;
- SHA-256 checksums;
- an SBOM and third-party notices;
- release notes stating the model download size, supported formats, privacy behavior, and any unsigned-build warning.

The first testing release is `v0.1.0-alpha.1`. It is published from an annotated Git tag after all release gates pass. The model files remain at their official pinned source and are not repackaged in the GitHub release. The installer is unsigned unless a valid Windows Authenticode certificate is available in repository secrets; unsigned testing releases explicitly warn about Windows SmartScreen. Tauri updater support and a stable-release signing policy are deferred until after the first testing cycle.

## Clean-room relationship to BackLog

BackLog is a behavioral reference only. Intern independently reimplements the useful concepts of a persisted queue, deterministic proposal validation, evidence-gated acceptance, safe no-overwrite file operations, operation receipts, and restart recovery. It does not copy BackLog source, tests, fixtures, schemas, prompt text, UI, or delivery contracts.

## v1 boundary

A feature belongs in v1 only if it directly helps a user turn a supported local document into a well-named, accurately described file or prevents that operation from losing or corrupting data. Search, watched folders, custom prompts, custom naming templates, multilingual OCR packs, enhanced Docling parsing, cloud destinations, portable builds, analytics, and automatic updates remain outside v1.
