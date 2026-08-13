# Intern v0.1.0-alpha.2 release checklist

**Release status: awaiting the dispatched release run.** The rendered-fidelity sign-off is recorded and accepted in `docs/qa/rendered-fidelity-signoff.json` (reviewed 2026-08-12), so that gate no longer blocks. What remains is the evidence only the pinned Windows release runner can produce, which the release workflow produces and validates itself before it can publish.

This checklist separates evidence produced on the current Linux host from gates that require the pinned Windows release environment. `pass` means the named command ran to completion with exit code 0. `failed` means the command ran and returned a nonzero exit. `pending` means it was not executed in a qualifying environment. A pending or failed required gate blocks release.

## Evidence context

- Source baseline: current `main`. The pipeline redesign replaced the extraction
  window, prompt, validation, and naming code this document originally described,
  and grew the corpus from 13 fixtures to 20.
- Required release runner: GitHub-hosted `windows-latest`, Node `24.15.0`, Rust
  `1.88.0`, pinned runtime assets, and the single pinned Q4_K_M text model named by
  `src-tauri/resources/model-manifest.json`.
- Accepted concept: `docs/design/intern-primary-screen.png`, 1536×1024, SHA-256 `c8cf322da777d77bc490b855fd18c5a70fe24192a343505e677d34d925a30de8`.
- Fixture manifest SHA-256: `5c56ebb466b3bd43e920d2a56b572f98b4ef3cfc1e7605b84d9730d3b67e332a`.
- Gold expectations SHA-256: `7b26b2715fbb312230c8ec8cab928bc4bbf7d566ec97870ea98853896a67675a`.

An earlier revision of this document also pinned a "production prompt SHA-256". No
script produced or checked that value, and the prompt has since been rewritten, so
it is removed rather than left to look verified: the prompt lives in
`crates/intern-engine/src/prompt.rs` under version control.

## Fresh local evidence

Runner: Windows 11 Pro 26100, Node `24.15.0`, Rust `1.88.0` on the
`x86_64-pc-windows-gnu` toolchain (this machine has no MSVC toolchain, so
`intern-app` is compile-checked rather than linked here; CI covers MSVC).

| Status | Command | Exact result | Scope / limitation |
|---|---|---|---|
| pass | `node fixtures/generate-fixtures.mjs --update-gold` | exit 0; 20 deterministic gold fixtures | Byte-identical under pinned Node 24.15.0. |
| pass | `npm run check` | exit 0; TypeScript passed; 15 Vitest files and 97 tests passed | Includes the canonical corpus comparison and the PowerShell parse check. |
| pass | `cargo fmt --all -- --check` | exit 0 | — |
| pass | `cargo clippy --locked -p intern-worker --all-targets --features windows-native -- -D warnings` | exit 0 | Lints the PDFium/Tesseract paths the default feature set never compiles. |
| pass | `cargo test --locked -p intern-engine -p intern-core -p intern-queue -p intern-worker --all-targets` | exit 0; all suites passed | Excludes `intern-app`, which cannot link without MSVC. |
| pass | `cargo test -p intern-worker --features windows-native --test generated_fixtures --test native_assets` | exit 0; 4 + 3 tests passed | Real pinned PDFium chromium/7881, verified by size and SHA-256. |
| pass | `scripts/smoke-worker.ps1` | exit 0; hello, native PDF, OCR, DOCX, image, and both invalid fixtures | Real PDFium and real Tesseract. **Local Tesseract is UB-Mannheim 5.4.0, not the pinned vcpkg 5.5.2**, so OCR text may differ slightly on the release runner. |

CI on `windows-latest` with the MSVC toolchain has separately passed `cargo fmt`,
`cargo clippy --workspace`, `cargo test --locked --workspace --all-targets`, the
pinned asset fetch and `--require-bundled` verification, and the native-asset
fixture integration. The most recent run failed only on a transient upstream `503`
while vcpkg fetched libjpeg-turbo sources; the fetch script now retries that.

## Required release gates

| Gate | Status | Required evidence / producer |
|---|---|---|
| Rust formatting | pass | `cargo fmt --all -- --check` on Rust 1.88.0, locally and on CI |
| Rust lint | pass | `cargo clippy --locked --workspace --all-targets -- -D warnings` on CI, plus the `windows-native` feature locally and on CI |
| Rust workspace tests | pass | `cargo test --locked --workspace --all-targets` on CI with MSVC |
| Native PDFium/Tesseract fixture integration | pass | Windows `generated_fixtures` with `windows-native` on CI; `scripts/smoke-worker.ps1` against real PDFium and Tesseract locally, pending its first CI execution |
| Browser core interaction path | pending | Playwright mixed-batch add/review/edit/approve/undo test |
| Automated accessibility and 1024-pixel layout | pending | `tests/e2e/qa.spec.ts` in Chromium |
| 1536×1024 implementation capture | pass | Real Playwright capture committed at `docs/qa/latest-implementation.png`, SHA-256 `aedb8798c512332d5a79e8194ae3075234eaa01a3a3b8e15c5c66911fd6a1b5c`. Regenerating it from the current tree reproduces those exact bytes, so the reviewed image is the shipping UI |
| Windows Tauri/NSIS build | pending | `npm run tauri build -- --bundles nsis -- --locked` |
| Installer/install/uninstall smoke | pending | `scripts/smoke-installer.ps1`, including signed runtime inventory, installed worker PDF/OCR path, and retained user-data sentinel |
| Corpus evaluation | pending on the release runner | Every gold fixture through the packaged worker and the shipping distill/prompt/client/validate path, llama.cpp b10361, the exact pinned text model; `scripts/run-model-evaluation.ps1`. Run locally on Windows 11 with the size-and-digest-verified pinned model: 18 documents scored, including the six OCR fixtures for the first time. Not the pinned runner, so it does not satisfy this gate. |
| Model acceptance | pending on the release runner | `scripts/validate-model-evaluation.mjs`: date, type, party, and description accuracy above their regression floors, every document Intern named carrying the right date, zero documents filed under a corpus-marked trap date, and a review rate under the ceiling. The local run above returns `accepted`. |
| Rendered fidelity review | accepted | `docs/qa/rendered-fidelity-signoff.json`, reviewed 2026-08-12 against the committed capture. Scope is recorded in the sign-off's own notes: the implementation capture was inspected, not pixel-diffed against the concept art |
| Clean Windows core recovery path | pending | Install/setup; PDF/DOCX/scan/folder; pause/resume; edit without another model call; apply/undo; forced extraction/apply termination; restart; clear history; uninstall |

## Evidence workflow and release boundary

`.github/workflows/qa.yml` is manual, read-only, and cannot create a tag, push a commit, or publish a release. It runs the browser/Rust/native/runtime/installer gates, generates the screenshot and the corpus evaluation, and uploads the checklist, fidelity ledger, evaluation, screenshot, diagnostics, installer, installed-app smoke report, and hash manifest as QA artifacts. Its model step uses `crates/intern-engine/src/bin/intern-evaluate.rs`, which drives the same extraction, distillation, prompt, client, validation, and naming code the application uses — there is no separate evaluation path that could pass while the product fails.

The release workflow does not trust a checked-in completed report or a claimed run ID. Publishing is dispatched deliberately, never triggered by a merge. The dispatched run rebuilds the evaluator and runtime, rescores the whole corpus with the pinned model, installs and launches the produced application, and creates a new manifest bound to that commit and workflow run. Publishing is unreachable unless `validate-release-evidence.mjs` accepts all artifact hashes plus model, rendered-fidelity, and installed-core sign-offs.

The release run ships the reviewed capture rather than taking a new one. Requiring a fresh screenshot to match a digest a reviewer recorded in advance is unsatisfiable, and it means the image inspected is not the image published. Freshness is enforced instead by `release_inputs_sha256`, which `scripts/hash-release-inputs.mjs` computes from the commit's own tree, excluding `docs/qa/`: change any committed application code and the sign-off stops matching the model report, forcing a fresh review.

That digest is read from `git ls-tree`, not from the files on disk, and the difference is load-bearing. An earlier version hashed working-tree bytes, which made this gate unsatisfiable: `scripts/fetch-windows-assets.ps1` rewrites the tracked `src-tauri/resources/runtime-assets.json` in place with the vcpkg package ownership and digests it resolves while building Tesseract — 2,098 bytes as committed, 25,615 bytes on the runner — and the digest is computed after that step. The release runner and a clean checkout of the identical commit therefore disagreed, and every sign-off was rejected. Reading the commit's tree makes the digest immune to build-time file changes and to checkout line-ending settings, while still moving the instant any committed byte changes.

`Cargo.lock` is committed and every hosted Rust command uses `--locked`. The fidelity sign-off is accepted; no release tag or publish action is authorized by this checklist until the dispatched run's own model evaluation and installed-core evidence are also accepted.
