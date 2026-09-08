# Microsoft Uploader Identity Implementation Plan

**Goal:** enforce the approved fail-closed shared-intake policy using Microsoft identity rather than machine arrival order.

**Architecture:** a pure intake ownership seam and a queue admission seam are wired to a separate Microsoft connector in the desktop host. Auth/metadata never enter the inference engine. The frontend presents backend-owned account and hold state.

**Tech Stack:** existing Rust workspace, reqwest, serde, SHA-256, OS keyring, React/TypeScript and Vitest. Reuse locked dependencies.

**Spec:** ../specs/2026-09-08-microsoft-uploader.md

## Global constraints

Unknown uploader never enters processing. User-entered names/emails never authorize a document. Only provider-verified directory IDs or a /me-bound exact UPN are accepted. No document text is sent to Microsoft by this connector. Secrets never enter settings, shared files, UI responses, logs or committed fixtures. Do not merge main or weaken release checks.

## Tasks

- [ ] Add regression tests that show the unsafe first-arrival assumption and missing identity setup.
- [ ] Introduce ownership decisions before watcher claims and admission checks before queue hashing, extraction, inference and apply. Recheck legacy queued work and manual intake drops.
- [ ] Implement Microsoft device sign-in with selected-folder read and explicit workload audit read, OS-protected refresh token storage, remote folder pairing and bounded metadata and upload-event audit requests. Deny every unresolved or ambiguous response.
- [ ] Bind permitted metadata to local bytes, preserve uploader/processor attribution, and expose counts/reasons through desktop commands.
- [ ] Add tested Settings UX with account identity, local/remote folder confirmation and explicit held states. Update privacy and setup documentation.
- [ ] Run native/unit/frontend/build/browser checks, inspect changes, and open a stacked draft PR with precise verification limits.

Tests are added before their implementation. Existing CI and a temporary isolated verification workspace provide native checks; no production Microsoft credentials are used in tests.
