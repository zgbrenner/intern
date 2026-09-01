# Intern alpha.5 rendered-fidelity ledger

**Rendered sign-off status: accepted.** The accepted record is
`docs/qa/rendered-fidelity-signoff.json`, reviewed on 2026-09-01 and bound to
the final non-QA release-input digest
`f338fbbe5532f18175dff9d95b2d8fe8f225dba30c2f2da775239b4ca7ef0b89`.

## Evidence inspected

- Accepted concept: `docs/design/intern-primary-screen.png`, 1536×1024,
  SHA-256 `c8cf322da777d77bc490b855fd18c5a70fe24192a343505e677d34d925a30de8`.
- Fresh implementation capture: hosted Whole-product QA evidence run
  `33481594670`, Windows/X64, commit
  `0a62f9b63c73e422263d42df8e778dab878552a8`. The capture is 1536×1024 and
  SHA-256 `5a619cabceb197e39f46624c682d29440c4a604baf8e20eb79c37694306f715e`.
- The capture differs from alpha.4's on purpose: alpha.5 ships the Vistage
  brand treatment, and the frame shows it - the small-caps "for Vistage" tag
  beside the wordmark, the primary action in Vistage navy with white text at
  roughly 12:1, and focus/selection states on the brand blue. The run's
  automated contrast assertions passed, including the "Needs review" amber on
  the retuned selected-row tint.
- That run completed every substantive automated gate: 19 Vitest files / 146
  tests, four Playwright tests, `cargo fmt --check`, `cargo clippy -D
  warnings`, workspace Rust tests, native fixture tests, verified pinned
  Windows assets, an NSIS build, installed-app smoke, and an accepted
  whole-corpus evaluation with real inference - dates 76.5%, types 88.2%,
  every gate above its floor and zero documents filed under a forbidden
  date, with the new reference-date guard converging the hardware-dependent
  model picks. Its final evidence binding correctly failed closed because the
  prior sign-off was bound to the alpha.4
  `1e6af646a00b376080c99e7179659c6d3505a9128739f733232ae192c0010d0e` digest;
  this post-run review supplies the accepted replacement record.

## Review conclusion

The fresh capture was inspected against the accepted concept. Core hierarchy
and interaction emphasis align: sidebar counts, queue table, and review drawer
read as three distinct planes, and the header states the privacy posture in the
chrome beside the new brand tag. Date-first proposed filenames, right-aligned
confidence, and em-dashes for absent values stay consistent down the column.
Ready, Needs review, Processing, and Waiting are each distinguishable by icon
as well as colour, and this run's automated contrast assertions for the review
and waiting statuses passed at 4.5:1 on the brand-tinted selection.

No clipping, collisions, illegible copy, excessive density, or ambiguous
focus/selection were observed. Focus is a ring on the filename input and
selection is a tint on the row the drawer describes, so neither is in doubt.
The filename field's right-edge truncation is an input scrolled to offset zero,
which the capture test arranges deliberately so the head of the proposed name
is visible rather than its tail.

This is not a pixel-equality assertion: the reference and implementation can
differ where alpha.5's supported behavior requires it. The native title bar,
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
