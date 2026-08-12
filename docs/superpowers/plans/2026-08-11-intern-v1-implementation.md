# Intern v1 Implementation Plan

> **Superseded.** This records the v0.1 design as built on 2026-08-11. The document-understanding pipeline was redesigned shortly afterwards; see `docs/architecture.md` for what Intern actually does now.


> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Tasks 2, 3, and 4 may run concurrently only in separate git worktrees because their owned paths do not overlap. Every production behavior follows red-green-refactor.

**Goal:** Build, test, package, and release Intern as a local-first Windows utility that sequentially parses documents, obtains one structured Qwen2.5-VL 3B proposal, evidence-gates the result, and safely renames or moves approved files.

**Architecture:** A Tauri 2 shell owns persistence, orchestration, local model lifecycle, and filesystem mutations. A separate Rust parser worker converts documents and performs page-level OCR. A React frontend implements the accepted list-and-inspector design through a typed bridge that works against real Tauri commands and an in-memory test adapter.

**Tech Stack:** Rust 2024 edition with minimum Rust 1.88; Tauri 2.11; React 19.2; TypeScript 7; Vite 8; Vitest 4; SQLite through rusqlite; AnyDoc 0.1.8; PDFium; Tesseract 5; pinned llama.cpp b10361; Qwen2.5-VL-3B-Instruct GGUF.

## Global Constraints

- Windows 10 and 11 x86-64 only for v1.
- Normal processing uses one primary model call per document and at most one retry for malformed or interrupted output.
- The queue processes exactly one document at a time.
- Documents and extracted contents never leave the device; no telemetry or cloud fallback exists.
- Qwen2.5-VL 3B runs through pinned `llama-server.exe`, one slot, 8,192-token context, localhost only, no GPU requirement.
- Q4_K_M ships only if its accepted-field accuracy is within two percentage points of Q8_0 and introduces no unsupported dates or parties among Ready results.
- Ready requires confidence at least 0.86, no model review flag, no field-affecting parser warning, and evidence for every included date, party, and subject.
- Filenames use `YYYY-MM-DD - Document Type - Primary Party or Subject.ext`; unsupported segments are omitted rather than replaced with filler.
- No destination is overwritten. Cross-volume moves copy to a temporary path, verify SHA-256, atomically publish, then delete the source.
- The initial supported formats are PDF, DOCX, TXT, Markdown, PNG, JPEG, and TIFF.
- The accepted UI reference is `docs/design/intern-primary-screen.png` at 1536 by 1024. The implementation must preserve its list, left navigation, right inspector, true-white palette, restrained indigo/amber/green semantics, and density.
- The generated concept's duplicate product wordmark is intentionally removed; Waiting items display no proposal or confidence.
- Configuration, manifests, lockfiles, and generated Tauri scaffolding are the only TDD exceptions. Every function containing product behavior requires a test observed failing first.
- All dependencies are pinned by `package-lock.json` and `Cargo.lock`; runtime archives and model files also have committed sizes and SHA-256 digests.
- Existing BackLog source, tests, fixtures, prompt text, and schemas are not copied.

---

### Task 1: Scaffold the Buildable Workspace

**Files:**
- Create: `.gitignore`
- Create: `package.json`
- Create: `package-lock.json`
- Create: `tsconfig.json`
- Create: `tsconfig.node.json`
- Create: `vite.config.ts`
- Create: `vitest.setup.ts`
- Create: `index.html`
- Create: `src/main.tsx`
- Create: `src/App.tsx`
- Create: `src/App.test.tsx`
- Create: `src/styles/base.css`
- Create: `Cargo.toml`
- Create: `crates/intern-core/Cargo.toml`
- Create: `crates/intern-core/src/lib.rs`
- Create: `crates/intern-worker/Cargo.toml`
- Create: `crates/intern-worker/src/main.rs`
- Create: `src-tauri/Cargo.toml`
- Create: `src-tauri/build.rs`
- Create: `src-tauri/src/main.rs`
- Create: `src-tauri/src/lib.rs`
- Create: `src-tauri/tauri.conf.json`
- Create: `src-tauri/capabilities/default.json`
- Modify: `README.md`

**Interfaces:**
- Produces npm scripts `dev`, `build`, `test`, `test:watch`, `lint`, `tauri`, and `check`.
- Produces a Rust workspace with members `crates/intern-core`, `crates/intern-worker`, and `src-tauri`.
- Produces an empty Tauri command surface. Later tasks add commands without changing the workspace structure.

- [ ] **Step 1: Write the failing frontend boot test**

```tsx
import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { App } from './App';

describe('App', () => {
  it('exposes the Intern application landmark', () => {
    render(<App />);
    expect(screen.getByRole('main', { name: 'Intern' })).toBeInTheDocument();
  });
});
```

- [ ] **Step 2: Add pinned frontend configuration and install dependencies**

Use this package shape and let npm write the exact lockfile:

```json
{
  "name": "intern",
  "private": true,
  "version": "0.1.0-alpha.1",
  "type": "module",
  "scripts": {
    "dev": "vite",
    "build": "tsc -b && vite build",
    "test": "vitest run",
    "test:watch": "vitest",
    "lint": "tsc -b --pretty false",
    "tauri": "tauri",
    "check": "npm run lint && npm test && npm run build"
  },
  "dependencies": {
    "@tauri-apps/api": "2.11.1",
    "lucide-react": "1.31.0",
    "react": "19.2.8",
    "react-dom": "19.2.8"
  },
  "devDependencies": {
    "@tauri-apps/cli": "2.11.4",
    "@testing-library/jest-dom": "7.0.1",
    "@testing-library/react": "16.3.2",
    "@types/react": "19.2.18",
    "@types/react-dom": "19.2.4",
    "@vitejs/plugin-react": "6.0.5",
    "jsdom": "30.0.1",
    "typescript": "7.0.2",
    "vite": "8.2.1",
    "vitest": "4.1.10"
  }
}
```

- [ ] **Step 3: Run the boot test and verify RED**

Run: `npm test -- src/App.test.tsx`

Expected: FAIL because `App` and the configured test environment do not yet exist.

- [ ] **Step 4: Add the minimal React entrypoint and app landmark**

```tsx
export function App() {
  return <main aria-label="Intern" />;
}
```

Configure Vitest for `jsdom`, load `@testing-library/jest-dom/vitest`, and clear rendered DOM after each test.

- [ ] **Step 5: Add the Rust and Tauri scaffold**

The root manifest is:

```toml
[workspace]
resolver = "2"
members = ["crates/intern-core", "crates/intern-worker", "src-tauri"]

[workspace.package]
version = "0.1.0-alpha.1"
edition = "2024"
rust-version = "1.88"
license = "MIT"
```

The Cargo package names are exactly `intern-core`, `intern-worker`, and `intern-app`; `intern-core` is a library crate, `intern-worker` is a binary, and `intern-app` builds the `intern` binary and library from `src-tauri`. Tauri allows only dialog, drag/drop, and explicitly declared Intern commands. Do not grant generic shell or filesystem capabilities to the frontend.

- [ ] **Step 6: Verify the scaffold**

Run: `npm run check`

Expected: frontend test passes, TypeScript exits 0, and Vite emits `dist/`.

Run when Rust is available: `cargo test --workspace --all-targets`

Expected: all three crates compile and the workspace contains zero failing tests.

- [ ] **Step 7: Commit**

```bash
git add .gitignore package.json package-lock.json tsconfig.json tsconfig.node.json vite.config.ts vitest.setup.ts index.html src Cargo.toml crates src-tauri README.md
git commit -m "build: scaffold Intern workspace"
```

---

### Task 2: Implement the Trusted Core, Queue Store, and Safe File Operations

**Files:**
- Create: `crates/intern-core/src/domain.rs`
- Create: `crates/intern-core/src/evidence.rs`
- Create: `crates/intern-core/src/naming.rs`
- Create: `crates/intern-core/src/packet.rs`
- Create: `crates/intern-core/src/validation.rs`
- Create: `crates/intern-core/src/store.rs`
- Create: `crates/intern-core/src/file_ops.rs`
- Create: `crates/intern-core/src/error.rs`
- Create: `crates/intern-core/tests/validation.rs`
- Create: `crates/intern-core/tests/queue_store.rs`
- Create: `crates/intern-core/tests/file_ops.rs`
- Modify: `crates/intern-core/src/lib.rs`
- Modify: `crates/intern-core/Cargo.toml`

**Interfaces:**
- Produces `ModelProposal`, `Evidence`, `ValidatedProposal`, `QueueItem`, `QueueStatus`, and stable `ErrorCode` types serialized in snake_case.
- Produces `validate_proposal(proposal, packet) -> ValidationOutcome`.
- Produces `compose_filename(validated, extension, existing_names) -> ComposedName`.
- Produces `build_document_packet(extracted, image_included) -> DocumentPacket`.
- Produces `QueueStore::open`, `enqueue`, `claim_next`, `transition`, `recover_interrupted`, `list`, and `clear_terminal`.
- Produces `FileApplier::apply` and `FileApplier::undo` behind an injected filesystem boundary.

- [ ] **Step 1: Write validation and naming tests first**

Use literal fixtures that prove these breaks are caught:

```rust
#[test]
fn unsupported_date_is_removed_and_requires_review() {
    let packet = packet("Signed by Acme Corporation.");
    let proposal = proposal_with_date("2024-04-12", "effective April 12, 2024");
    let outcome = validate_proposal(proposal, &packet);
    assert_eq!(outcome.proposal.document_date, None);
    assert_eq!(outcome.status, ProposalStatus::NeedsReview);
    assert!(outcome.reasons.contains(&ReviewReason::EvidenceMissing));
}

#[test]
fn composer_uses_iso_date_type_subject_and_collision_suffix() {
    let name = compose_filename(&validated("2024-04-12", "Employment Agreement", "John Smith"), "PDF", &["2024-04-12 - Employment Agreement - John Smith.pdf"]);
    assert_eq!(name.value, "2024-04-12 - Employment Agreement - John Smith (2).pdf");
}
```

Also cover impossible dates, Windows reserved device names, control and bidi characters, duplicate extensions, trailing periods, 140-character truncation, absent optional segments, multiple-sentence descriptions, unsupported parties, confidence `0.859`, and confidence `0.86`.

- [ ] **Step 2: Run validation tests and verify RED**

Run: `cargo test -p intern-core --test validation`

Expected: FAIL because core types and functions are absent.

- [ ] **Step 3: Implement domain, evidence validation, packet construction, and naming**

Use these public shapes:

```rust
pub struct ModelProposal {
    pub document_date: Option<String>,
    pub date_kind: Option<DateKind>,
    pub document_type: Option<String>,
    pub filename_subject: Option<String>,
    pub parties: Vec<String>,
    pub description: String,
    pub confidence: f32,
    pub needs_review: bool,
    pub review_reasons: Vec<String>,
    #[serde(flatten)]
    pub evidence: Evidence,
}

pub struct Evidence {
    #[serde(rename = "date_evidence")]
    pub date: Option<String>,
    #[serde(rename = "type_evidence")]
    pub document_type: Option<String>,
    #[serde(rename = "subject_evidence")]
    pub subject: Option<String>,
    #[serde(rename = "party_evidence")]
    pub parties: Vec<String>,
}

pub enum QueueStatus {
    Queued, Extracting, Analyzing, Ready, NeedsReview, Failed, Canceled, Applying, Completed
}
```

Evidence matching performs Unicode NFKC normalization, case folding, whitespace collapse, and smart-quote normalization. It never uses fuzzy matching for dates, parties, or subjects. Packet construction uses 22,000 characters for text-only requests and 12,000 with an image, split 14,000/8,000 or 8,000/4,000.

- [ ] **Step 4: Run validation tests and verify GREEN**

Run: `cargo test -p intern-core --test validation`

Expected: PASS with every named edge case covered.

- [ ] **Step 5: Write queue-store tests first**

Test a real temporary SQLite database. Prove duplicate unchanged paths focus one item, identical content at different paths creates two items, one concurrent claimant wins, invalid transitions fail, restart returns extracting/analyzing to queued, applying remains for reconciliation, and automatic retries stop after two processing failures.

- [ ] **Step 6: Run queue tests and verify RED**

Run: `cargo test -p intern-core --test queue_store`

Expected: FAIL because `QueueStore` is absent.

- [ ] **Step 7: Implement SQLite migrations and store operations**

Use WAL, `PRAGMA foreign_keys=ON`, a five-second busy timeout, explicit transactions, compare-and-swap status updates, and tables `queue_items`, `proposals`, `operation_receipts`, and `schema_migrations`. Complete extracted text and rendered pages are never columns.

- [ ] **Step 8: Run queue tests and verify GREEN**

Run: `cargo test -p intern-core --test queue_store`

Expected: PASS, including the concurrent-claim test.

- [ ] **Step 9: Write file-operation tests first**

Use a real temporary filesystem for same-volume rename and an injected copying filesystem for cross-volume boundaries. Prove no overwrite, deterministic suffixes, temporary destination cleanup, hash mismatch retaining the source, verified copy before source deletion, source-delete failure retaining both paths, fingerprint-change rejection, and undo refusal after destination modification.

- [ ] **Step 10: Run file-operation tests and verify RED**

Run: `cargo test -p intern-core --test file_ops`

Expected: FAIL because `FileApplier` is absent.

- [ ] **Step 11: Implement safe apply and undo**

The receipt records source, destination, pre-operation hash, post-operation hash, operation kind, and last durable stage. File operations return stable codes including `FILE_CHANGED`, `DESTINATION_UNAVAILABLE`, `MOVE_VERIFICATION_FAILED`, and `SOURCE_DELETE_FAILED` without embedding document text.

- [ ] **Step 12: Verify and commit**

Run: `cargo test -p intern-core --all-targets`

Expected: all core tests pass.

```bash
git add crates/intern-core
git commit -m "feat: add trusted document processing core"
```

---

### Task 3: Implement the Isolated Parser and OCR Worker

**Files:**
- Create: `crates/intern-worker/src/protocol.rs`
- Create: `crates/intern-worker/src/extract.rs`
- Create: `crates/intern-worker/src/pdf.rs`
- Create: `crates/intern-worker/src/ocr.rs`
- Create: `crates/intern-worker/src/limits.rs`
- Create: `crates/intern-worker/src/temp.rs`
- Create: `crates/intern-worker/tests/protocol.rs`
- Create: `crates/intern-worker/tests/routing.rs`
- Create: `crates/intern-worker/tests/anydoc_docx.rs`
- Modify: `crates/intern-worker/src/main.rs`
- Modify: `crates/intern-worker/Cargo.toml`

**Interfaces:**
- Consumes a JSON Lines request with `protocol_version`, `request_id`, and one of `hello`, `parse`, `cancel`, or `shutdown`.
- Produces JSON Lines `hello`, `progress`, `parsed`, and `error` events.
- Produces `ExtractedDocument { pages, warnings, truncated, optional_image }` without depending on Tauri or SQLite.
- Uses AnyDoc 0.1.8 for DOCX and declarative text formats, PDFium for PDF text/rendering, and a `TesseractOcr` adapter for OCR.

- [ ] **Step 1: Write protocol tests first**

```rust
#[test]
fn hello_reports_exact_protocol_version() {
    let response = handle_line(r#"{"protocol_version":1,"request_id":"r1","command":{"type":"hello"}}"#).unwrap();
    assert_eq!(response, r#"{"protocol_version":1,"request_id":"r1","event":{"type":"hello","worker_version":"0.1.0-alpha.1"}}"#);
}
```

Also prove malformed JSON returns `PARSE_FAILED` without killing the process and that an unsupported protocol version returns a version error.

- [ ] **Step 2: Run protocol tests and verify RED**

Run: `cargo test -p intern-worker --test protocol`

Expected: FAIL because the protocol handler is absent.

- [ ] **Step 3: Implement versioned JSON Lines IPC**

Keep stdout exclusively for protocol JSON. Route structured diagnostics to stderr. Flush every response. The process reads until shutdown or EOF and does not expose a network port.

- [ ] **Step 4: Run protocol tests and verify GREEN**

Run: `cargo test -p intern-worker --test protocol`

Expected: PASS.

- [ ] **Step 5: Write extraction-routing tests first**

Use fake `PdfBackend` and `OcrBackend` implementations to assert observable routing outcomes. Cover native text, fewer than 20 meaningful characters with at least 65 percent image coverage, more than 3 percent replacement glyphs, clean mixed pages, OCR mean confidence below 75 selecting exactly one image, and 25-megapixel render rejection.

- [ ] **Step 6: Run routing tests and verify RED**

Run: `cargo test -p intern-worker --test routing`

Expected: FAIL because routing is absent.

- [ ] **Step 7: Implement extraction and resource limits**

Initial limits are exactly 1 GB source bytes, 500 pages, 1 GB decompressed Office content, 2 GB temporary bytes, 25 megapixels per page, 30 minutes extraction/OCR, and one resident rendered OCR page. Images are auto-rotated, RGB, at most 1,344 pixels on the long edge, and padded to Qwen's 28-pixel grid.

- [ ] **Step 8: Add a real generated DOCX integration fixture in the test**

Construct a minimal DOCX ZIP during the test containing a heading, two paragraphs, and a table. Call AnyDoc and assert literal Markdown containing `# Employment Agreement`, `John Smith`, and `Acme Corporation`. Do not copy a BackLog fixture.

- [ ] **Step 9: Run worker tests and commit**

Run: `cargo test -p intern-worker --all-targets`

Expected: protocol, routing, and AnyDoc integration tests pass. PDFium/Tesseract asset-dependent smoke tests may be feature-gated to Windows but routing logic must run on every platform.

```bash
git add crates/intern-worker
git commit -m "feat: add local document parser worker"
```

---

### Task 4: Implement the Accepted Desktop Interface Against a Typed Bridge

**Files:**
- Create: `src/types.ts`
- Create: `src/lib/bridge.ts`
- Create: `src/lib/inMemoryBridge.ts`
- Create: `src/lib/format.ts`
- Create: `src/components/Icon.tsx`
- Create: `src/components/AppHeader.tsx`
- Create: `src/components/Sidebar.tsx`
- Create: `src/components/DropZone.tsx`
- Create: `src/components/QueueTable.tsx`
- Create: `src/components/StatusCell.tsx`
- Create: `src/components/ReviewInspector.tsx`
- Create: `src/components/SettingsDialog.tsx`
- Create: `src/components/SetupScreen.tsx`
- Create: `src/features/queue/useQueue.ts`
- Create: `src/features/queue/queue.test.tsx`
- Create: `src/features/review/review.test.tsx`
- Create: `src/features/setup/setup.test.tsx`
- Create: `src/styles/tokens.css`
- Create: `src/styles/app.css`
- Modify: `src/App.tsx`
- Modify: `src/App.test.tsx`
- Modify: `src/styles/base.css`

**Interfaces:**
- Consumes a `DesktopBridge` with `listItems`, `addFiles`, `addFolder`, `pauseQueue`, `resumeQueue`, `approve`, `keepOriginal`, `retry`, `remove`, `undo`, `getSettings`, `saveSettings`, `getSetup`, and `startModelDownload`.
- Produces the complete Queue, Needs Review, Completed, review inspector, setup, and compact settings interactions without direct Tauri imports in components.
- The in-memory bridge is a real state adapter used by browser development and tests, not a static screenshot or inert mock.

- [ ] **Step 1: Write queue interaction tests first**

Prove that Waiting rows show em dashes for proposal/confidence, selecting Needs review opens the inspector, navigation filters the list, Add files calls the bridge, and a dropped folder reaches `addFolder`. Assert user-visible state, not mock component existence.

- [ ] **Step 2: Run UI tests and verify RED**

Run: `npm test -- src/features/queue/queue.test.tsx`

Expected: FAIL because the queue UI is absent.

- [ ] **Step 3: Implement the design system and primary screen**

Use these tokens as the extraction baseline from the approved reference:

```css
:root {
  font-family: "Segoe UI Variable", "Segoe UI", system-ui, sans-serif;
  color: #171a1f;
  background: #ffffff;
  --surface: #ffffff;
  --surface-subtle: #f7f9fc;
  --surface-selected: #f2f6ff;
  --border: #d9dee8;
  --border-strong: #c4cad5;
  --text: #171a1f;
  --text-muted: #667085;
  --accent: #0b5cff;
  --accent-hover: #084fdc;
  --ready: #14804a;
  --review: #b66a00;
  --waiting: #747b86;
  --radius-sm: 6px;
  --radius-md: 9px;
  --shadow-inspector: -8px 0 24px rgba(23, 26, 31, 0.06);
}
```

At 1,536 pixels, the sidebar is 230 pixels, inspector 370 pixels, and main list fills the remainder. Use a 72-pixel product header below native window chrome, 46-pixel table rows, 14-pixel body text, 13-pixel controls, 12-pixel metadata, 1-pixel borders, and no gradients. Use consistent Lucide outline icons at 18 or 20 pixels with 1.75-pixel optical weight.

Allowed primary-screen copy is limited to `Intern`, `Private · On this device`, `Add files`, `Add folder`, `Queue`, `Needs Review`, `Completed`, `Drag files or folders here to add to the queue`, `Original filename`, `Status`, `Proposed filename`, `Confidence`, `Review item`, `Filename`, `Description`, `Evidence`, `Date`, `Type`, `Parties`, `Reason for review`, `Approve & rename`, and `Keep original`, plus actual filenames, values, counts, and status labels.

- [ ] **Step 4: Run queue tests and verify GREEN**

Run: `npm test -- src/features/queue/queue.test.tsx`

Expected: PASS.

- [ ] **Step 5: Write review and setup tests first**

Prove editing does not call model processing, approval validates a nonblank filename, Keep original moves the row to Completed, Undo restores it when allowed, setup reports exact bytes, pause/resume updates progress, and setup failure leaves the queue inaccessible without inventing a cloud fallback.

- [ ] **Step 6: Run review/setup tests and verify RED**

Run: `npm test -- src/features/review/review.test.tsx src/features/setup/setup.test.tsx`

Expected: FAIL because those flows are absent.

- [ ] **Step 7: Implement review, settings, setup, responsiveness, and accessibility**

At 1,024 pixels, collapse the inspector into a right drawer over the list and reduce the sidebar to icons with accessible labels. Preserve keyboard focus order, visible focus rings, native form semantics, screen-reader names, `aria-live` progress, and `prefers-reduced-motion`.

- [ ] **Step 8: Verify and commit**

Run: `npm run check`

Expected: all UI tests pass, TypeScript succeeds, and the production bundle builds.

```bash
git add src package.json package-lock.json vite.config.ts
git commit -m "feat: build Intern desktop interface"
```

---

### Task 5: Implement Model Setup, llama.cpp Lifecycle, Prompt, and Structured Client

**Files:**
- Create: `src-tauri/resources/model-manifest.json`
- Create: `src-tauri/src/model/mod.rs`
- Create: `src-tauri/src/model/manifest.rs`
- Create: `src-tauri/src/model/download.rs`
- Create: `src-tauri/src/model/server.rs`
- Create: `src-tauri/src/model/prompt.rs`
- Create: `src-tauri/src/model/client.rs`
- Create: `src-tauri/tests/model_manifest.rs`
- Create: `src-tauri/tests/model_download.rs`
- Create: `src-tauri/tests/model_response.rs`
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/src/lib.rs`

**Interfaces:**
- Consumes `DocumentPacket`-equivalent text and optional image bytes.
- Produces a deserialized `ModelProposal` and typed setup/progress events.
- Owns model installation under LocalAppData, localhost server start/health/stop, a per-process API key, and one retry for malformed or interrupted output.

- [ ] **Step 1: Commit the exact model manifest and write manifest tests first**

```json
{
  "schema_version": 1,
  "model_id": "qwen2.5-vl-3b-instruct-q4-k-m",
  "files": [
    {
      "name": "Qwen2.5-VL-3B-Instruct-Q4_K_M.gguf",
      "url": "https://huggingface.co/ggml-org/Qwen2.5-VL-3B-Instruct-GGUF/resolve/main/Qwen2.5-VL-3B-Instruct-Q4_K_M.gguf",
      "size": 1929901056,
      "sha256": "d02fe9b69ad8cadbbd228e387667af66612c44bed29ffc8eb1e7caf9ac486c12"
    },
    {
      "name": "mmproj-Qwen2.5-VL-3B-Instruct-f16.gguf",
      "url": "https://huggingface.co/ggml-org/Qwen2.5-VL-3B-Instruct-GGUF/resolve/main/mmproj-Qwen2.5-VL-3B-Instruct-f16.gguf",
      "size": 1338428128,
      "sha256": "b9160fe9d814d1fadf68395677468534778b39ac33c2e7561b7b218626e60d5e"
    }
  ]
}
```

Tests reject a changed size, digest, unsafe filename, non-HTTPS URL, and duplicate filename.

- [ ] **Step 2: Run manifest tests and verify RED**

Run: `cargo test -p intern-app --test model_manifest`

Expected: FAIL because manifest parsing is absent.

- [ ] **Step 3: Implement manifest validation and resumable download**

Download to `<name>.partial`, use HTTP Range only when the server confirms it, stream SHA-256, check free disk space for remaining bytes plus 512 MB, and atomically rename only after exact length and digest match. A user-selected existing file follows the same validation.

- [ ] **Step 4: Test the downloader against a local HTTP server**

Cover fresh download, resume after interruption, server ignoring Range, wrong digest, insufficient disk, and cancellation retaining a reusable partial file. Run `cargo test -p intern-app --test model_download` and require PASS.

- [ ] **Step 5: Write server/client response tests first**

Use a local fake HTTP server that mirrors the complete llama.cpp response shape. Prove the process arguments include one slot, 8,192 context, localhost, random port, API key, text model, F16 projector, and no GPU offload. Prove valid JSON decodes, malformed JSON retries once, semantic low confidence does not retry, and a second malformed response returns `MODEL_RESPONSE_INVALID`.

- [ ] **Step 6: Run response tests and verify RED**

Run: `cargo test -p intern-app --test model_response`

Expected: FAIL because the model client is absent.

- [ ] **Step 7: Implement server lifecycle, prompt, GBNF, and client**

The prompt reproduces the approved schema and date priorities, orders Qwen to use `null` rather than inventing facts, and wraps extracted content between explicit untrusted-document delimiters. The grammar permits nullable supported fields, bounded arrays, booleans, and confidence numbers without `$ref`, regex, or external schema resolution. The server is launched without a visible console window on Windows and is terminated on app shutdown.

- [ ] **Step 8: Verify and commit**

Run: `cargo test -p intern-app --test model_manifest --test model_download --test model_response`

Expected: all model setup and client contract tests pass.

```bash
git add src-tauri/resources src-tauri/src/model src-tauri/tests src-tauri/Cargo.toml src-tauri/src/lib.rs
git commit -m "feat: add local Qwen runtime"
```

---

### Task 6: Integrate the Sequential Pipeline and Real Tauri Bridge

**Files:**
- Create: `src-tauri/src/commands.rs`
- Create: `src-tauri/src/pipeline.rs`
- Create: `src-tauri/src/worker.rs`
- Create: `src-tauri/src/settings.rs`
- Create: `src-tauri/src/paths.rs`
- Create: `src-tauri/tests/pipeline.rs`
- Create: `src-tauri/tests/recovery.rs`
- Create: `src/lib/tauriBridge.ts`
- Create: `src/lib/tauriBridge.test.ts`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/main.rs`
- Modify: `src-tauri/capabilities/default.json`
- Modify: `src/App.tsx`

**Interfaces:**
- Consumes parser-worker events, model proposals, `intern-core` validation/store/file operations, and UI commands.
- Produces the exact `DesktopBridge` methods from Task 4 through typed Tauri invokes and queue events.
- Guarantees one active pipeline item and restart reconciliation.

- [ ] **Step 1: Write pipeline tests first**

Use real SQLite and fake worker/model boundaries. Prove a mixed queue stays sequential, Ready and Needs Review are evidence-gated, malformed model output retries once, one failed item does not block the next, pause starts no next item, cancel kills the active request, automatic apply affects only Ready items, and source fingerprint change prevents apply.

- [ ] **Step 2: Run pipeline tests and verify RED**

Run: `cargo test -p intern-app --test pipeline --test recovery`

Expected: FAIL because orchestration is absent.

- [ ] **Step 3: Implement worker supervision and sequential pipeline**

Launch `intern-worker.exe` with hidden Windows console, handshake protocol version 1, stream events, enforce the 30-minute parser and 15-minute model timeouts, restart the worker once after a crash, and fail the same item after a second crash while continuing the queue.

- [ ] **Step 4: Implement narrow Tauri commands and events**

Commands are `queue_list`, `queue_add_files`, `queue_add_folder`, `queue_pause`, `queue_resume`, `queue_cancel`, `queue_retry`, `queue_remove`, `proposal_approve`, `proposal_keep_original`, `operation_undo`, `settings_get`, `settings_save`, `setup_get`, `setup_start`, and `history_clear`. Inputs validate canonical paths and item IDs in Rust. The frontend receives `queue://changed`, `queue://progress`, and `setup://progress` events.

- [ ] **Step 5: Write and run bridge tests**

The TypeScript test supplies a complete fake invoke/listen transport, then asserts command names, payload shapes, unsubscribe behavior, and event-to-state updates. Run `npm test -- src/lib/tauriBridge.test.ts` first for RED, implement `TauriBridge`, then rerun for GREEN.

- [ ] **Step 6: Verify and commit**

Run: `npm run check`

Run when Rust is available: `cargo test -p intern-app --all-targets`

Expected: frontend and app integration tests pass.

```bash
git add src-tauri/src src-tauri/tests src-tauri/capabilities src/lib src/App.tsx
git commit -m "feat: integrate sequential document pipeline"
```

---

### Task 7: Add Clean-Room Fixtures, Windows Assets, CI, and Release Packaging

**Files:**
- Create: `fixtures/README.md`
- Create: `fixtures/generate-fixtures.mjs`
- Create: `fixtures/expected.json`
- Create: `tests/e2e/queue.spec.ts`
- Create: `playwright.config.ts`
- Create: `scripts/fetch-windows-assets.ps1`
- Create: `scripts/verify-assets.mjs`
- Create: `scripts/smoke-installer.ps1`
- Create: `src-tauri/resources/THIRD_PARTY_NOTICES.md`
- Create: `.github/workflows/ci.yml`
- Create: `.github/workflows/release.yml`
- Modify: `package.json`
- Modify: `package-lock.json`
- Modify: `src-tauri/tauri.conf.json`
- Modify: `README.md`

**Interfaces:**
- Produces reproducible PDF, DOCX, scan, image, long-document, and mixed-batch fixtures with no BackLog content.
- Produces pinned Windows sidecars/assets during CI and packages them through Tauri `externalBin` and resources.
- Produces a per-user NSIS installer, checksums, notices, and SBOM on tag `v0.1.0-alpha.1`.

Add `@playwright/test` version `1.62.1` as a development dependency and add scripts `fixtures`, `test:e2e`, and `assets:verify` to `package.json`.

- [ ] **Step 1: Write the fixture generator and expected gold fields**

Generate a text employment agreement PDF, image-only scanned lease, mixed PDF with a scanned signature page, DOCX NDA with table/header/footer/footnote, multi-date invoice, meeting minutes, rotated low-resolution scan, encrypted and malformed PDF, 100-page PDF, PNG/JPEG/TIFF document images, and a mixed folder containing duplicates, unsupported files, and `~$` lock files. Use fictional parties and addresses.

- [ ] **Step 2: Add parser and browser tests around generated artifacts**

Run fixture generation, pass representative files through the real parser worker when Windows assets are present, and assert extracted literal facts. Playwright runs the in-memory bridge build, drops a mixed batch, navigates states, edits a review result, approves it, and verifies Undo.

- [ ] **Step 3: Pin Windows runtime assets**

`scripts/fetch-windows-assets.ps1` downloads and verifies:

- llama.cpp `b10361`, `llama-b10361-bin-win-cpu-x64.zip`, 18,427,695 bytes, SHA-256 `36da9e9c1c094bf7842fab69e6cc0921125a67fa2611ba8f329a00804350302a`;
- PDFium `chromium/7999`, `pdfium-win-x64.tgz`, 3,762,593 bytes, SHA-256 `55329d5cb5de8a379a2fc563106492d7f385a1f795d18970922c71f708f9fbb4`;
- Tesseract 5.5.2 built through vcpkg baseline `644588ca32576d86325fb3fe3b6020042bee61b8` for `x64-windows`; `eng.traineddata` from tessdata_fast commit `87416418657359cb625c412a48b6e1d6d41c29bd`, 4,113,088 bytes, SHA-256 `7d4322bd2a7749724879683fc3912cb542f19906c83bcc1a52132556427170b2`; and `osd.traineddata` from the same commit, 10,562,727 bytes, SHA-256 `9cf5d576fcc47564f11265841e5ca839001e7e6f38ff7f7aacf46d15a96b00ff`. The script records every bundled PE and trained-data digest in `src-tauri/resources/runtime-assets.json`.

The script fails closed on size or digest mismatch and copies only required runtime files into `src-tauri/binaries` or `src-tauri/resources`.

- [ ] **Step 4: Configure CI**

`ci.yml` runs npm check on Ubuntu and Rust fmt, clippy with warnings denied, workspace tests, fixture integration, and Tauri build on `windows-latest`. Cache keys include lockfiles and runtime asset manifest. Upload the installer only from Windows.

- [ ] **Step 5: Configure release workflow**

`release.yml` triggers only on tag `v0.1.0-alpha.1`, repeats all release gates, creates SHA-256 checksums, generates an SBOM, and publishes the NSIS executable and notices with GitHub's repository token. Model files are not release assets.

- [ ] **Step 6: Verify and commit**

Run: `npm run fixtures && npm run check && npm run test:e2e`

Run on Windows CI: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace --all-targets && npm run tauri build`

Expected: fixture, frontend, Rust, E2E, and installer-build gates pass.

```bash
git add fixtures tests playwright.config.ts scripts src-tauri/resources src-tauri/tauri.conf.json .github package.json package-lock.json README.md
git commit -m "build: package and test Windows release"
```

---

### Task 8: Perform Whole-Product QA and Prepare the Testing Release

**Files:**
- Create: `docs/qa/fidelity-ledger.md`
- Create: `docs/qa/release-checklist.md`
- Create: `docs/qa/model-evaluation.json`
- Create: `docs/qa/latest-implementation.png`
- Modify: files identified by functional, model, installer, accessibility, or visual QA

**Interfaces:**
- Consumes the built app, accepted concept, generated fixtures, Q4/Q8 manifests, and Windows installer.
- Produces objective evidence for the release gates and no new product surface.

- [ ] **Step 1: Run the complete automated suite fresh**

Run `npm run check`, Playwright E2E, Rust fmt/clippy/tests, fixture integration, Windows Tauri build, and installer smoke test. Record commands, commit, runner, counts, and exit codes in `release-checklist.md`.

- [ ] **Step 2: Run the real-model evaluation**

Process the gold corpus with Q4_K_M and Q8_0 using the production prompt and llama.cpp build. Record field-level results, unsupported facts, readiness, response validity, peak RSS, and elapsed time. Ship Q4 only when the Global Constraints gate passes; otherwise update the embedded manifest to Q8_0 size 3,285,474,304 and SHA-256 `fa8aeb3b6bf6152774e87d13e09892aa065f4e0c4abe90806cd8ab18ff72d9fe`, then rerun the affected tests.

- [ ] **Step 3: Verify the rendered app against the concept**

Use the built-in browser first; use Playwright only if the browser cannot load the local Vite/Tauri surface. Capture at 1,536 by 1,024 and inspect both `docs/design/intern-primary-screen.png` and the latest capture with `view_image`. The fidelity ledger compares at least copy, structure, typography, palette, list density, inspector width, icon treatment, focus/selection state, and 1,024-pixel responsiveness. Fix every agency-signoff issue.

- [ ] **Step 4: Verify the core interaction path**

On a clean Windows environment, install, complete setup, add PDF/DOCX/scan/mixed folder, pause/resume, inspect Needs Review, edit without a model call, approve, safely apply, undo, force-kill during extraction and applying, restart, clear history, and uninstall without deleting user files.

- [ ] **Step 5: Final review and commit**

Dispatch a whole-branch code review against the design spec and this plan. Fix all Critical and Important findings, run one scoped re-review, then rerun the complete suite.

```bash
git add docs/qa .
git commit -m "test: complete Intern v1 release validation"
```

- [ ] **Step 6: Publish**

Merge the reviewed feature branch into `main`, push `main`, create annotated tag `v0.1.0-alpha.1`, push the tag, wait for the release workflow, inspect the downloadable NSIS asset and checksums, and report any SmartScreen warning caused by an unsigned testing installer.
