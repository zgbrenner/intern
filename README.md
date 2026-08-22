# Intern

Intern reads a document and tells you what to call it.

Every pair below is copied from the scored corpus in
[docs/model-bakeoff.md](docs/model-bakeoff.md) — the left column is what the
previous pipeline produced, the right column is what this one does:

```text
2026-04-09 - Statement of Work.pdf  ->  2026-04-01 Statement of Work between Ridgeline Cartography LLC and Vistage Worldwide, Inc.pdf
2026-12-29 - notice.pdf             ->  2026-12-29 Notice of Termination - John Smith.pdf
2025-02-14.pdf                      ->  2025-02-14 Employment Agreement between Northstar Lantern Works LLC and Mira Vale.pdf
```

The first line is the point of the whole thing: `2026-04-09` was the date the
statement of work was signed, and `2026-04-01` is the date it takes effect. Only
one of those is what the document *is*.

Alongside the name it produces one sentence saying what the document actually
concerns, the verbatim excerpts behind every fact it used, and a confidence. If
anything is unsupported, the document goes to review instead of being renamed.

Everything happens on the machine. Document text, extracted pages, OCR output,
and model prompts never leave it. There is no telemetry, no cloud fallback, and
no remote processing.

Intern makes exactly two network requests, both started by a person pressing a
button, neither carrying anything about your documents:

1. The one-off download of the pinned model file — 1.19 GiB, the text model and
   nothing else.
2. **Check for updates** in Settings, which asks GitHub for the release
   manifest. There is no background poll and no timer. An update is installed
   only if it is signed by this project's key; anything else is refused.

Intern reads documents as text: native PDF text first, OCR when a page has none,
and no vision model. The projector for this model is 668,227,264 bytes — 637 MiB,
a second download larger than half the model itself, before anyone names a
single document, for a path almost nothing takes. A page that neither text
extraction nor OCR can read goes to review rather than being guessed at.

## Install and first run

Windows 10 or 11, x86-64. Download `Intern_0.1.0-alpha.2_x64-setup.exe` from the
[latest release](https://github.com/zgbrenner/intern/releases/latest) and run it.
It installs per-user, so it does not ask for administrator rights, and
uninstalling it leaves your documents and Intern's own data alone.

**The installer is not Authenticode signed**, because this project has no
code-signing certificate. Windows SmartScreen will call it an unrecognized app;
to continue, choose **More info** and then **Run anyway**. Nothing in this
release removes that warning, and you should not take an unrecognized-app
warning lightly — so verify the download instead of trusting it. Every published
installer carries a keyless Sigstore build-provenance attestation naming the
repository, workflow, and commit that produced those exact bytes:

```sh
gh attestation verify Intern_0.1.0-alpha.2_x64-setup.exe --repo zgbrenner/intern
```

`SHA256SUMS.txt` is published beside the installer and catches a corrupted or
truncated download, but it proves nothing about origin: it sits on the same page
as the installer, so whoever could replace one could replace both. The
attestation is the part that establishes where the file came from.

On first launch Intern shows a setup screen and asks to download the one thing
the installer deliberately leaves out: the pinned model file, 1.19 GiB. That
download is resumable, it is the only network request Intern ever makes, and the
file becomes active only after its exact length and SHA-256 match the manifest
built into the installer. If you already have the file, **Choose existing model
files** points Intern at it and skips the download. Nothing else needs
installing — PDF text extraction, OCR, and inference all ship inside the app.

Then drag documents or a folder onto the window. Names Intern can support with
verbatim text from the document appear ready to apply; anything else goes to
review with the reason shown. Nothing on disk is renamed until you approve it,
either one item at a time or with **Apply all ready**, and a rename can be
undone.

## Watched intake folders, OneDrive, and SharePoint

Instead of dragging documents in, Settings can point Intern at an **intake
folder** to watch: documents that appear in it are analyzed and, once approved
(or automatically, if you enable high-confidence renames), moved to the
destination folder under their new name.

Both folders can live inside OneDrive or a SharePoint document library. The
integration is deliberately the **Microsoft sync client**, not a cloud API:
point Intern at any folder the OneDrive engine keeps on disk — your personal
OneDrive, OneDrive for Business, or a SharePoint library synced with **Sync**
or **Add shortcut to OneDrive** — and Intern detects the sync root and labels
the folder in Settings. Files On-Demand placeholders are handled; the sync
client downloads content when Intern reads it. No document text, no OCR
output, and no model prompt goes anywhere new: the two network requests listed
above are still the only ones Intern makes, and the only thing moving files to
the cloud is the sync client you already run.

Several machines can watch the **same** shared intake folder. They coordinate
through small claim files in a `.intern/` directory inside it — leases with
heartbeats, so a document is processed exactly once, a crashed machine's work
is taken over after its lease lapses, and nothing needs a server. By default
each machine only processes documents uploaded from that machine; enabling
**"Also process documents uploaded by others"** turns the folder into a shared
work queue, with a courtesy delay so the uploader's own machine gets first
claim. The design, its guarantees, and its failure behavior are documented in
[`docs/shared-intake.md`](docs/shared-intake.md).

## How a document becomes a filename

```text
document
  -> text/Markdown extraction (native text first; OCR only when there is none)
  -> whole-document distillation (every page read, redundancy removed)
  -> one local inference
  -> evidence validation
  -> filename + description + review decision
```

The distillation stage is the part that matters. Intern does **not** send the
model the first few pages and the last few pages. It reads every block on every
page, scores each one for how much it helps answer "what is this, when does it
take effect, who is it between", and keeps the best blocks **in document order**
under a character budget, marking elisions with `[...]`. A statement of work
whose effective date is on page five reaches the model exactly as well as one
whose date is on page one.

Kept text is verbatim, which is what makes the safety check real: the model has
to quote the document, and Intern checks the quoted facts against the document
before they can rename anything.

Full details, thresholds, and measurements are in
[`docs/architecture.md`](docs/architecture.md).

## The filename

```text
YYYY-MM-DD <what the document is> <the party or parties>.<original extension>
```

The date is the one that *defines* the document, not the first, last, or
easiest one to find: an agreement's effective date, a notice's notice date, an
invoice's invoice date, an amendment's own date rather than the date of the
agreement it amends. Payment due dates, renewal deadlines, and response
deadlines are never used. If no defining date can be established, the document
goes to review rather than getting an invented one.

Names are sanitised for Windows, keep the original extension, shed the least
identifying detail first when they would be too long to scan, and get a numeric
suffix on collision.

## Development

```sh
npm ci
npm run dev
```

Use the pinned Node 24.15.0 (`.nvmrc`), Rust 1.88.0, and the committed
`Cargo.lock`. Run the deterministic clean-room corpus generator and the frontend
gates with:

```sh
npm run fixtures
npm run check
npx playwright install chromium
npm run test:e2e
```

The browser development and Playwright builds use the in-memory bridge; no
document content or test data is sent to a service. Fixture contents and
reviewed answers are documented in `fixtures/README.md` and
`fixtures/expected.json`.

Run Rust tests with:

```sh
cargo test --locked --workspace --all-targets
```

### Crates

| Crate | What it owns |
| --- | --- |
| `intern-engine` | Document understanding: distillation, prompt, local model client and server, evidence validation, filename composition, and model installation. |
| `intern-intake` | Shared intake folders: the multi-machine claim protocol, cloud sync-root detection, and the polling watcher. |
| `intern-queue` | The durable queue: ordering, leases, retries, and the review/apply workflow. |
| `intern-core` | Crash-safe queue storage and journalled file operations. |
| `intern-worker` | The out-of-process parser: PDFium, OCR, and Office extraction. |
| `intern-app` | The Tauri desktop shell. |

The engine is usable without the desktop app:

```sh
intern-analyze --file contract.pdf --worker intern-worker.exe \
  --endpoint http://127.0.0.1:8080/v1/chat/completions --api-key KEY
```

It prints the proposed filename, the description, the evidence, the review
reasons, and local timings as one JSON object. `--distill-only` prints the
digest without running a model. This is the same code path the app uses, so a
watched folder, a script, or a future connector gets identical results.

## Windows runtime assets and installer

From PowerShell on Windows, fetch the exact llama.cpp b10361, PDFium
chromium/7881, Tesseract 5.5.2, and tessdata assets:

```powershell
cargo build --locked -p intern-worker --release --features windows-native
Copy-Item target/release/intern-worker.exe src-tauri/binaries/intern-worker-x86_64-pc-windows-msvc.exe
./scripts/fetch-windows-assets.ps1
npm run assets:verify -- --require-bundled
./scripts/stage-windows-runtime.ps1 -Destination "$env:TEMP/intern-runtime-stage"
./scripts/smoke-worker.ps1 -WorkerPath "$env:TEMP/intern-runtime-stage/intern-worker.exe" -RuntimeDirectory "$env:TEMP/intern-runtime-stage"
npm run tauri build -- --bundles nsis -- --locked
```

Every downloaded archive or trained-data file is rejected unless both its exact
byte length and committed SHA-256 digest match. The fetcher checks out the exact
vcpkg baseline, stages only required executables/DLLs/trained data and the full
upstream/vcpkg license closure, and records both source and packaged paths plus a
digest for every file in `src-tauri/resources/runtime-assets.json`.
Tauri produces a per-user NSIS installer. To smoke-test a clean installer:

```powershell
./scripts/smoke-installer.ps1 -InstallerPath target/release/bundle/nsis/Intern_0.1.0-alpha.2_x64-setup.exe
```

The installer includes third-party notices and the generated `licenses/`
inventory but never includes `.gguf` model files. Local model weights are
installed separately under the user's local application data only after explicit
setup.

## CI and release

CI runs fixture generation, TypeScript/Vitest/Vite, Playwright, Rust format,
Clippy with warnings denied, workspace tests, native fixture integration, asset
verification, the Windows Tauri build, and the installer smoke test. Runtime and
dependency caches are keyed by lockfiles and the runtime asset manifest.

`scripts/run-model-evaluation.ps1` scores the whole gold corpus through the
shipping pipeline with the exact pinned model and real inference, and
`scripts/validate-model-evaluation.mjs` gates on date, type, party, and
description accuracy, on never filing a document under a date the corpus marks
as a trap, and on the review rate. Publishing is a deliberate
`workflow_dispatch` against a chosen main commit, never a side effect of
merging; the release job still refuses any commit but the one it was dispatched
for.
