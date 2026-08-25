# Intern alpha.3 rendered-fidelity ledger

**Rendered sign-off status: accepted.** The accepted record is
`docs/qa/rendered-fidelity-signoff.json`, reviewed on 2026-08-25 and bound to
the final non-QA release-input digest
`0ff02d813957e9cabdcf1707cbefe0ef536c0a0885699584fe4e9d2bbe2f191c`.

## Evidence inspected

- Accepted concept: `docs/design/intern-primary-screen.png`, 1536×1024,
  SHA-256 `c8cf322da777d77bc490b855fd18c5a70fe24192a343505e677d34d925a30de8`.
- Fresh implementation capture: hosted Whole-product QA evidence run
  `32901120674`, Windows/X64, commit `a86e81b3d13a38d49bfb4691e56b7372570dbaad`.
  The committed `docs/qa/latest-implementation.png` is exactly 1536×1024 and
  SHA-256 `9f5b22cd5bc6d4a59c66b14d1e842f3e781e8d3eaa194035846c12c2ffddac23`.
- That run completed its substantive automated gates: 18 Vitest files / 141
  tests, four Playwright tests, workspace Rust tests, native fixture tests, an
  NSIS build, installed-app smoke, and accepted model evaluation. Its final
  evidence binding correctly failed closed because the prior sign-off was bound
  to the old `d4e1290146109cba58733c4f9d30c22802125211b1407df36b5eb282603cf0f8`
  digest; this post-run review supplies the accepted replacement record.

## Review conclusion

The fresh capture was inspected against the accepted concept. Core hierarchy and
interaction emphasis align. No clipping, collisions, illegible copy, excessive
density, or ambiguous focus/selection were observed. Expanded formats,
date-first filenames, bulk actions, pause control, and changed counts are
acceptable alpha.3 evolution.

This is not a pixel-equality assertion: the reference and implementation can
differ where alpha.3's supported behavior requires it. The native title bar,
1024-pixel layout, hover states, and motion remain supported by the hosted
automated browser and installed-app gates rather than an assertion that the
single 1536×1024 frame captures every state.

## Freshness boundary and residuals

The sign-off is accepted only for the digest above and its exact screenshot.
`scripts/hash-release-inputs.mjs` derives that digest from the committed
non-QA release inputs; a relevant source change invalidates the sign-off and
requires a new QA capture and review. The remaining release work is not a
fidelity discrepancy: exact-main validation and the deliberately dispatched
release workflow must still reproduce and accept their own evidence before a
tag or publication is allowed.
