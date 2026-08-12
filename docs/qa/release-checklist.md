# Intern v0.1.0-alpha.1 release checklist

**Release status: BLOCKED — QA preparation is complete, but required execution evidence is pending.**

This checklist separates evidence produced on the current Linux host from gates that require the pinned Windows release environment. `pass` means the named command ran to completion with exit code 0. `failed` means the command ran and returned a nonzero exit. `pending` means it was not executed in a qualifying environment. A pending or failed required gate blocks release.

## Evidence context

- Source baseline: `48bd32aa9745f1d52454d428b0af1bc94ce1e589` plus the Task 8 working-tree changes described by this document.
- Local runner: Linux x86-64, Node `v24.14.0`, npm `11.9.0`.
- Required release runner: GitHub-hosted `windows-latest`, Node `24.15.0`, Rust `1.88.0`, pinned runtime assets, Q4 and Q8 weights.
- Accepted concept: `docs/design/intern-primary-screen.png`, 1536×1024, SHA-256 `c8cf322da777d77bc490b855fd18c5a70fe24192a343505e677d34d925a30de8`.
- Production prompt SHA-256: `08dc85b2565e7bb49219e41535158c10de168cf5b897129c825035c26ec5072c`.
- Fixture manifest SHA-256: `c9d32ed4f0d4b0a22e7150c550c45454cd981fc38aed0daf70aab5e5696b848a`.
- Gold expectations SHA-256: `912735049332a9c9068dd4620602d2fe631e1ef503c316952095504a4676a0ac`.

## Fresh local evidence

| Status | Command | Exact result | Scope / limitation |
|---|---|---|---|
| pass | `npm run fixtures` | exit 0; generated 13 deterministic gold fixtures | Generator and canonical corpus comparison ran locally. Native parsing did not. |
| pass | `npm run check` | exit 0; TypeScript passed; 13 Vitest files and 53 tests passed; Vite built 1,811 modules | Frontend/unit/config tests and production web bundle only. |
| pass | `npm run assets:verify` | exit 0; 4 pinned downloads verified; 0 bundled runtime files and 0 license files present | Verifies acquisition metadata only on this host. `--require-bundled` is pending Windows staging. |
| pass | `node scripts/validate-model-evaluation.mjs docs/qa/model-evaluation.json --allow-pending` | exit 0; 13 signed records recognized; `release_blocked: true` | Schema/pins/hashes are valid; this is not model acceptance. |
| failed | `node scripts/validate-model-evaluation.mjs docs/qa/model-evaluation.json` | exit 1; `model evaluation is pending; release is blocked` | Expected fail-closed behavior for the unexecuted report. |
| failed | `npm run test:e2e` | exit 1; 3 of 3 tests could not launch Chromium because `chromium_headless_shell-1234` is absent | No browser assertion or interaction body executed. The gate remains pending, not passed. |
| failed | Playwright Chromium installation | exit 1 after repeated 0-byte/truncated downloads with `End of central directory record signature not found` | Prevented local screenshot, accessibility, responsive, and core-path browser evidence. |

## Required release gates

| Gate | Status | Required evidence / producer |
|---|---|---|
| Rust formatting | pending | `cargo fmt --all -- --check` on Rust 1.88.0 |
| Rust lint | pending | `cargo clippy --locked --workspace --all-targets -- -D warnings` |
| Rust workspace tests | pending | `cargo test --locked --workspace --all-targets` |
| Native PDFium/Tesseract fixture integration | pending | Windows `generated_fixtures` test with `windows-native` and the staged worker smoke |
| Browser core interaction path | pending | Playwright mixed-batch add/review/edit/approve/undo test |
| Automated accessibility and 1024-pixel layout | pending | `tests/e2e/qa.spec.ts` in Chromium |
| 1536×1024 implementation capture | pending | Real Playwright screenshot at `docs/qa/latest-implementation.png`; no file is checked in because no qualifying capture was produced |
| Windows Tauri/NSIS build | pending | `npm run tauri build -- --bundles nsis -- --locked` |
| Installer/install/uninstall smoke | pending | `scripts/smoke-installer.ps1`, including signed runtime inventory, installed worker PDF/OCR path, and retained user-data sentinel |
| Corpus evaluation | pending | Every gold fixture through the packaged worker and the shipping distill/prompt/client/validate path, llama.cpp b10361, the exact pinned text model; `scripts/run-model-evaluation.ps1` |
| Model acceptance | pending | `scripts/validate-model-evaluation.mjs`: date, type, party, and description accuracy above their regression floors, zero documents filed under a corpus-marked trap date, and a review rate under the ceiling |
| Rendered fidelity review | pending | Inspect both accepted concept and actual `latest-implementation.png` with `view_image`; code-only inspection is not pixel QA |
| Clean Windows core recovery path | pending | Install/setup; PDF/DOCX/scan/folder; pause/resume; edit without another model call; apply/undo; forced extraction/apply termination; restart; clear history; uninstall |

## Evidence workflow and release boundary

`.github/workflows/qa.yml` is manual, read-only, and cannot create a tag, push a commit, or publish a release. It runs the browser/Rust/native/runtime/installer gates, generates the screenshot and the corpus evaluation, and uploads the checklist, fidelity ledger, evaluation, screenshot, diagnostics, installer, installed-app smoke report, and hash manifest as QA artifacts. Its model step uses `crates/intern-engine/src/bin/intern-evaluate.rs`, which drives the same extraction, distillation, prompt, client, validation, and naming code the application uses — there is no separate evaluation path that could pass while the product fails.

The release workflow does not trust a checked-in completed report or a claimed run ID. Publishing is dispatched deliberately, never triggered by a merge. The dispatched run rebuilds the evaluator and runtime, rescores the whole corpus with the pinned model, recaptures the UI, installs and launches the produced application, and creates a new manifest bound to that commit and workflow run. Publishing is unreachable unless `validate-release-evidence.mjs` accepts all artifact hashes plus model, rendered-fidelity, and installed-core sign-offs. The checked-in fidelity sign-off remains pending, so release is intentionally blocked until the hosted screenshot is inspected.

`Cargo.lock` is committed and every hosted Rust command uses `--locked`. No release tag or publish action is authorized by this checklist while the model evaluation and rendered-fidelity sign-off remain pending.
