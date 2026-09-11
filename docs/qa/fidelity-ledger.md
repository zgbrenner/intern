# Intern alpha.9 rendered-fidelity ledger

**Rendered sign-off status: accepted for alpha.9.**
`docs/qa/rendered-fidelity-signoff.json` is bound to release-input digest
`c4db219b6db91a98eab8f9fae4e359860825a0f7dacf86772f1c44cb5d5471fd` and to the
1536x1024 capture `docs/qa/latest-implementation.png`, SHA-256
`ee46abcfe76433b6ed25b6f7a53c80056bf36918b8b44009ae8f5511c54167a4`, taken by
Whole-product QA evidence run 34538797800 at commit
`a9a96a482b099c4b848a00df8ee66bb4013caf18`.

## alpha.9 record

Run 34538797800 passed every gate on Windows: frontend checks, the browser QA
suite including its contrast assertions, `cargo fmt --check`, workspace clippy
with warnings denied, the Rust workspace tests, native fixture parsing with the
pinned assets, the verified pinned runtime, an NSIS build, installer and
uninstall smoke with user data retained, and an accepted whole-corpus
evaluation with real inference. That evaluation reproduces alpha.8's scores
exactly - date role 14/14, type 18/18, description specific 19/19, parties
16/19, review rate 42.1%, nothing filed under a date the corpus marks as a trap
- which is the point: this release changes no naming behaviour, and the numbers
show it did not.

The capture is materially unchanged from alpha.8's, deliberately. This
release's one visible change is that a window of 1100 pixels or less no longer
opens inside a modal review drawer over an inert queue, and this frame is 1536
wide, where the inspector has always been a side panel. The narrow behaviour is
held by the 1024-pixel browser gate and by a unit test rather than by this
picture; the frame was re-taken because the release inputs changed, not because
it was expected to look different.

Everything else in the release sits beneath the window and cannot be
photographed: a shared folder that recovers when a teammate's machine crashes
mid-document, a private-local-intake setting that is re-derived per folder
rather than trusted for ever, coordination files that survive an antivirus or
sync-client hold, description records found whichever way Windows spells the
folder, and a settings file that reports what it could not read instead of
silently starting with defaults.

The capture was inspected against the accepted concept. Core hierarchy and
interaction emphasis align: sidebar counts, queue table and review drawer read
as three distinct planes, and the header states the privacy posture beside the
brand tag. Date-first proposed filenames, right-aligned confidence and
em-dashes for absent values stay consistent down the column. Ready, Needs
review, Processing and Waiting are each distinguishable by icon as well as
colour. Evidence stays attributed under DATE, TYPE and PARTIES headings with
each party on its own line, the approve and keep actions sit in a pinned bar at
the foot of the panel, and the selected row carries an accent bar as well as a
tint. No clipping, collisions, illegible copy, excessive density or ambiguous
focus were observed.

This is not a pixel-equality assertion: the reference and implementation can
differ where supported behaviour requires it. The native title bar, the
1024-pixel layout, hover states and motion remain covered by the hosted
automated browser and installed-app gates rather than by a claim that one frame
captures every state.

### Reviewer

Reviewed by the maintainer, Zachary Brenner, who inspected the capture named
above and accepted it. Claude Opus 5 (Claude Code) inspected it first and wrote
this record and the sign-off at the maintainer's direction, the same standing
under which the alpha.6 and alpha.8 records were written.

### Freshness boundary

The sign-off is accepted only for the digest and screenshot named above.
`scripts/hash-release-inputs.mjs` derives that digest from the committed non-QA
release inputs; any relevant source change invalidates this record and requires
a new capture and review. Exact-main validation and the deliberately dispatched
release workflow must still reproduce and accept their own evidence before a tag
or publication is allowed.

## Superseded alpha.6 record

The alpha.6 sign-off was reviewed on 2026-09-02 and bound to the non-QA
release-input digest
`fe016f3944a190dd932d001367fdcc44a360b5cb6fd2030d6fe233e290a1c05c` at commit
`9e91511c12ad9a3e727e3fa56e15b7197de191d2`. Eight pull requests and the alpha.7
version bump have changed the non-QA release inputs since, so that digest no
longer describes what would ship.

## Evidence inspected

- Accepted concept: `docs/design/intern-primary-screen.png`, 1536×1024,
  SHA-256 `c8cf322da777d77bc490b855fd18c5a70fe24192a343505e677d34d925a30de8`.
- Fresh implementation capture: hosted Whole-product QA evidence run
  `33572364279`, Windows/X64, commit
  `9e91511c12ad9a3e727e3fa56e15b7197de191d2`. The capture is 1536×1024 and
  SHA-256 `1bc7bb1707c743fc87106eb3a2a914d234128cefdc20e92a77c88fa7a6b7fb76`.
- The capture differs substantially from alpha.5's: alpha.6 rebuilds the review
  panel. Evidence is now attributed quotations under labelled DATE, TYPE, and
  PARTIES headings, each party on its own line so it can be checked separately;
  the model's confidence appears in the panel at all, with an exact percentage
  beside a proportional track; the approve and keep actions sit in a pinned bar
  at the foot of the panel; the selected row carries an accent bar as well as a
  tint, so it is no longer confusable with hover; and a help affordance sits
  beside the settings gear. The run's automated contrast assertions passed.
- That run completed every substantive automated gate: 19 Vitest files / 146
  tests, four Playwright tests, `cargo fmt --check`, `cargo clippy -D
  warnings`, workspace Rust tests, native fixture tests, verified pinned
  Windows assets, an NSIS build, installed-app smoke, and an accepted
  whole-corpus evaluation with real inference - dates 76.5%, types 88.2%,
  every gate above its floor and zero documents filed under a forbidden
  date, with the new reference-date guard converging the hardware-dependent
  model picks. Its final evidence binding correctly failed closed because the
  prior sign-off was bound to the alpha.5
  `f338fbbe5532f18175dff9d95b2d8fe8f225dba30c2f2da775239b4ca7ef0b89` digest;
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
differ where alpha.6's supported behavior requires it. The native title bar,
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
