# Intern v0.1.0-alpha.8 release checklist

**Release status: blocked pending exact-main validation and the deliberately
dispatched release workflow.** Whole-product QA has run and the rendered
fidelity sign-off is accepted, so those two gates are no longer
**pending/blocked**; everything the release job owns still is. Nothing in this
checklist authorizes a tag or publication.

## Hosted QA artifacts and accepted alpha.8 evidence

- Workflow: Whole-product QA evidence, run `34420596844`, attempt `1`.
- Commit: `622ddec2d6342adb31250804805d6dcd9ca46fca`; runner: `Windows/X64`.
- Execution result: every step passed, including the real-inference corpus
  evaluation, which `validate-model-evaluation.mjs` accepted.
- Release-input digest:
  `dea704a9a8020e176d4b9bef9182a4ae4dc5278726fea332e0b668e2be99f9e1`.
- Capture: `docs/qa/latest-implementation.png`, 1536x1024, SHA-256
  `ccf5e44f2294e0e5d8b7ad0c673eb91d146c6e0470d66f849c80868c8470eb18`.
- Fidelity reviewer: the maintainer, Zachary Brenner, accepted the capture; the
  record is in `rendered-fidelity-signoff.json`, bound to the digest above, and
  the reasoning is in `fidelity-ledger.md`.

| Gate | Status | Hosted run artifact or post-run evidence |
|---|---|---|
| Frontend unit, lint, and build check | accepted | `npm run check` exit 0; TypeScript, Vitest, and the Vite production build. |
| Browser core interaction, accessibility, and 1024-pixel layout | accepted | `npm run test:e2e` exit 0; 4 Playwright tests and the 1536x1024 capture. |
| Rendered fidelity review | accepted | The capture above, reviewed and bound to the alpha.8 release-input digest. |
| Rust formatting and workspace lint | accepted | `cargo fmt --all -- --check` and `cargo clippy --locked --workspace --all-targets -- -D warnings`, both exit 0. |
| Rust workspace tests | accepted | `cargo test --locked --workspace --all-targets` exit 0; 527 tests. |
| Pinned runtime assets and native fixtures | accepted | `npm run assets:verify -- --require-bundled` exit 0; 51 runtime files, 23 license files; 4 native fixture tests. |
| Windows Tauri/NSIS build | accepted | `npm run tauri build -- --bundles nsis -- --locked` exit 0; one installer. |
| Installer and installed-core smoke | accepted | `scripts/smoke-installer.ps1` exit 0; install, packaged worker PDF/OCR, uninstall, user data retained. |
| Corpus evaluation and model acceptance | accepted | 19 documents scored on the pinned model, peak model RSS 2055 MB; every accuracy gate above its floor and no document filed under a forbidden date. |
| Exact-main validation | pending/blocked | The release workflow must verify its dispatch target is the exact current `main` commit. |
| Deliberately dispatched release workflow | pending/blocked | Rebuild, updater signature verification, checksums/SBOM/evidence acceptance, provenance, annotated tag, and publication remain release-job gates. |

## Release boundary

The QA workflow has read-only repository permissions and cannot tag, push, or
publish. The release workflow independently checks the exact main commit and
recreates its release evidence. It fails closed unless the model evaluation,
the fidelity sign-off, the installed-core smoke, updater verification,
checksums, SPDX SBOMs, and the evidence manifest are accepted before
provenance, annotated tag creation, and GitHub release publication.

The release ships the reviewed capture rather than generating a new one after
review. Freshness is supplied by the committed non-QA `release_inputs_sha256`;
changing a relevant release input requires a new capture and sign-off.
