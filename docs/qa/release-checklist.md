# Intern v0.1.0-alpha.6 release checklist

**Release status: blocked pending whole-product QA, rendered fidelity sign-off,
exact-main validation, and the deliberately dispatched release workflow.** No
hosted QA run has been recorded for alpha.6 yet, so every gate below is
**pending/blocked** until a hosted run produces artifacts bound to the alpha.6
release inputs. Nothing in this checklist authorizes a tag or publication.

## Hosted QA artifacts and accepted alpha.6 evidence

- Workflow: Whole-product QA evidence, run `pending`, attempt `pending`.
- Commit: `pending`; runner: `pending`.
- Execution result: pending — no alpha.6 hosted QA run has been executed.
- Release-input digest: `pending`.
- Capture: `docs/qa/latest-implementation.png`, dimensions and SHA-256 pending a
  fresh alpha.6 capture.
- Fidelity reviewer: pending; a fresh sign-off must be recorded in
  `rendered-fidelity-signoff.json` and bound to the alpha.6 release-input digest.

| Gate | Status | Hosted run artifact or post-run evidence |
|---|---|---|
| Frontend unit, lint, and build check | pending | `npm run check` has not been recorded for alpha.6. |
| Browser core interaction, accessibility, and 1024-pixel layout | pending | `npm run test:e2e` capture has not been recorded for alpha.6. |
| Rendered fidelity review | pending/blocked | A fresh alpha.6 capture must be reviewed and its sign-off bound to the alpha.6 release-input digest. |
| Rust formatting and workspace lint | pending | `cargo fmt --all -- --check`; `cargo clippy --locked --workspace --all-targets -- -D warnings`. |
| Rust workspace tests | pending | `cargo test --locked --workspace --all-targets`. |
| Pinned runtime assets and native fixtures | pending | `npm run assets:verify -- --require-bundled`. |
| Windows Tauri/NSIS build | pending | `npm run tauri build -- --bundles nsis -- --locked`. |
| Installer and installed-core smoke | pending | App launch, clean shutdown, runtime inventory, installed worker core path, uninstall, and retained user data. |
| Corpus evaluation and model acceptance | pending | `validate-model-evaluation.mjs` must accept a fresh alpha.6 evaluation. |
| Exact-main validation | pending | The release workflow must verify its dispatch target is the exact current `main` commit. |
| Deliberately dispatched release workflow | pending | Rebuild, updater signature verification, checksums/SBOM/evidence acceptance, provenance, annotated tag, and publication remain release-job gates. |

## Release boundary

The QA workflow has read-only repository permissions and cannot tag, push, or
publish. The release workflow independently checks the exact main commit and
recreates its release evidence. It fails closed unless the model evaluation,
fresh fidelity sign-off, installed-core smoke, updater verification, checksums,
SPDX SBOMs, and evidence manifest are accepted before provenance, annotated tag
creation, and GitHub release publication.

The release ships the reviewed capture rather than generating a new one after
review. Freshness is supplied by the committed non-QA
`release_inputs_sha256`; changing a relevant release input requires a new
capture and sign-off.
