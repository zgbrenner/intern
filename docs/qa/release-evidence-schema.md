# Release evidence manifest

`scripts/create-release-evidence.mjs` writes schema version 1 of `release-evidence-manifest.json`. The manifest records the exact 40-character commit, GitHub workflow name, run ID, and attempt that produced it, then records the byte length and SHA-256 of:

- the production model evaluation;
- the 1536×1024 implementation capture;
- the run checklist;
- the rendered-fidelity sign-off;
- the installed-core smoke report;
- the exact NSIS installer; and
- every named test log.

`scripts/validate-release-evidence.mjs` re-hashes every artifact and derives all three sign-off states from source evidence. A publishable manifest requires:

1. a completed, accepted model report produced for the manifest run and commit;
2. an accepted human fidelity record bound to the screenshot SHA-256 and the model report's release-input SHA-256; and
3. an accepted installer report for the same commit and workflow run, with successful native launch, clean shutdown, signed runtime inventory, installed worker path, uninstall, and user-data-retention checks.

The fidelity record uses the release-input digest rather than embedding the commit that contains the sign-off. This avoids a self-referential Git commit while still preventing a sign-off from being reused after product, prompt, fixture, runtime, packaging, or test inputs change. The final manifest separately names the exact release commit.

The read-only QA workflow may create a blocked manifest with `rendered_fidelity: pending`; `--allow-pending` exists only so the complete QA artifact can be uploaded for inspection. The tagged release workflow never uses that flag. It reruns the single pinned Q4_K_M model through the production evaluator, ships the reviewed capture, builds and installs the exact installer, creates a new run-bound manifest, validates it without exceptions, and only then reaches the publishing step.

An earlier revision of this paragraph said the release run "reruns Q4 and Q8" and "recaptures the UI". Neither is true: `src-tauri/resources/model-manifest.json` pins one model and `docs/model-bakeoff.md` records Q8 as rejected, and the release workflow deliberately ships the capture a reviewer inspected rather than generating a new one — see the freshness note in `docs/qa/release-checklist.md`.
