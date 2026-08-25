# Intern v0.1.0-alpha.3 release checklist

**Release status: blocked pending exact-main validation and the deliberately
dispatched release workflow.** Hosted QA run `32901120674` completed every
substantive automated gate, then correctly concluded failed at evidence binding
because its prior accepted alpha.3 sign-off was bound to stale digest
`d4e1290146109cba58733c4f9d30c22802125211b1407df36b5eb282603cf0f8`. The fresh alpha.3
fidelity inspection accepted afterward from that run's uploaded capture, model,
and installed-smoke artifacts; neither that run nor this checklist authorizes a
tag or publication.

Historical record: before the fresh hosted capture was reviewed, rendered
fidelity was **pending/blocked** for alpha.3 because the prior sign-off could
not bind the signature-fix release inputs.

## Hosted QA artifacts and post-run accepted alpha.3 evidence

- Workflow: Whole-product QA evidence, run `32901120674`, attempt `1`.
- Commit: `a86e81b3d13a38d49bfb4691e56b7372570dbaad`; runner: Windows/X64.
- Execution result: every substantive automated gate below passed; the final
  evidence-binding step failed closed on the prior stale-digest fidelity sign-off.
- Release-input digest:
  `0ff02d813957e9cabdcf1707cbefe0ef536c0a0885699584fe4e9d2bbe2f191c`.
- Capture: `docs/qa/latest-implementation.png`, 1536×1024, SHA-256
  `9f5b22cd5bc6d4a59c66b14d1e842f3e781e8d3eaa194035846c12c2ffddac23`.
- Post-run fidelity reviewer: Codex primary with Sol Advisor read-only review,
  recorded at `2026-08-25T21:52:04Z` in `rendered-fidelity-signoff.json`.

| Gate | Status | Hosted run 32901120674 artifact or post-run evidence |
|---|---|---|
| Frontend unit, lint, and build check | pass | `npm run check`: 18 Vitest files / 141 tests; TypeScript and Vite build passed. |
| Browser core interaction, accessibility, and 1024-pixel layout | pass | `npm run test:e2e`: 4 Playwright tests; 1536×1024 capture written. |
| Rendered fidelity review | accepted post-run | Fresh capture, model report, and installed-smoke artifact from the hosted run were reviewed after its stale-digest sign-off binding failure; the replacement alpha.3 sign-off is bound to the digest above. |
| Rust formatting and workspace lint | pass | `cargo fmt --all -- --check`; `cargo clippy --locked --workspace --all-targets -- -D warnings`. |
| Rust workspace tests | pass | `cargo test --locked --workspace --all-targets`: 273 tests. |
| Pinned runtime assets and native fixtures | pass | `npm run assets:verify -- --require-bundled`: 51 runtime files and 23 license files; 4 native fixture tests. |
| Windows Tauri/NSIS build | pass | `npm run tauri build -- --bundles nsis -- --locked`: one NSIS installer. |
| Installer and installed-core smoke | pass | App launch, clean shutdown, runtime inventory, installed worker core path, uninstall, and retained user data all accepted. |
| Corpus evaluation and model acceptance | pass | 18 documents scored with `Qwen3.5-2B-Q4_K_M.gguf`; `validate-model-evaluation.mjs` accepted the evaluation. |
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
