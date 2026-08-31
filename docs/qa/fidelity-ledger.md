# Intern alpha.4 rendered-fidelity ledger

**Rendered sign-off status: accepted.** The accepted record is
`docs/qa/rendered-fidelity-signoff.json`, reviewed on 2026-08-31 and bound to
the final non-QA release-input digest
`1e6af646a00b376080c99e7179659c6d3505a9128739f733232ae192c0010d0e`.

## Evidence inspected

- Accepted concept: `docs/design/intern-primary-screen.png`, 1536×1024,
  SHA-256 `c8cf322da777d77bc490b855fd18c5a70fe24192a343505e677d34d925a30de8`.
- Fresh implementation capture: hosted Whole-product QA evidence run
  `33439159119`, Windows/X64, commit
  `cd9fb6c6b38020544da8475ea981bba3586aaa38`. The committed
  `docs/qa/latest-implementation.png` is 1536×1024 and SHA-256
  `9f5b22cd5bc6d4a59c66b14d1e842f3e781e8d3eaa194035846c12c2ffddac23`.
- That capture is byte-identical to the one alpha.3 was signed against. This
  is expected rather than a stale artifact: `INTERN_QA_CAPTURE` was set, the
  capturing Playwright test ran and passed in this run, and nothing in alpha.4
  changes the primary screen. The two intake lines this release adds are
  conditional and live in the Settings dialog, which this frame does not show.
- That run completed every substantive automated gate: 18 Vitest files / 141
  tests, four Playwright tests, `cargo fmt --check`, `cargo clippy -D
  warnings`, workspace Rust tests, native fixture tests, verified pinned
  Windows assets, an NSIS build, installed-app smoke, and a whole-corpus
  evaluation with real inference. Its final evidence binding correctly failed
  closed because the prior sign-off was bound to the alpha.3
  `34b2039eca40ca9157369e81b5f0acd8a5b2dba096a52d219a1e42db0e060e25` digest;
  this post-run review supplies the accepted replacement record.

## Review conclusion

The fresh capture was inspected against the accepted concept. Core hierarchy
and interaction emphasis align: sidebar counts, queue table, and review drawer
read as three distinct planes, and the header states the privacy posture in the
chrome. Date-first proposed filenames, right-aligned confidence, and em-dashes
for absent values stay consistent down the column. Ready, Needs review,
Processing, and Waiting are each distinguishable by icon as well as colour, and
this run's automated contrast assertions for the review and waiting statuses
passed at 4.5:1.

No clipping, collisions, illegible copy, excessive density, or ambiguous
focus/selection were observed. Focus is a ring on the filename input and
selection is a tint on the row the drawer describes, so neither is in doubt.
The filename field's right-edge truncation is an input scrolled to offset zero,
which the capture test arranges deliberately so the head of the proposed name
is visible rather than its tail.

This is not a pixel-equality assertion: the reference and implementation can
differ where alpha.4's supported behavior requires it. The native title bar,
1024-pixel layout, hover states, and motion remain supported by the hosted
automated browser and installed-app gates rather than an assertion that the
single 1536×1024 frame captures every state.

## Reviewer

Reviewed by Claude Opus 5 (Claude Code) at the maintainer's direction, who
inspected the capture named above. The reviewer field in the sign-off record
names the same, so a later reader can tell who looked and with what standing.

## Freshness boundary and residuals

The sign-off is accepted only for the digest above and its exact screenshot.
`scripts/hash-release-inputs.mjs` derives that digest from the committed
non-QA release inputs; a relevant source change invalidates the sign-off and
requires a new QA capture and review. The remaining release work is not a
fidelity discrepancy: exact-main validation and the deliberately dispatched
release workflow must still reproduce and accept their own evidence before a
tag or publication is allowed.
