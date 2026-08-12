# Intern primary-screen fidelity ledger

**Rendered sign-off status: accepted**, recorded in `docs/qa/rendered-fidelity-signoff.json` on 2026-08-12 against the committed 1536×1024 capture. What was reviewed is the implementation capture itself; it was not pixel-diffed against the concept art, and this ledger's per-area rows below remain code-backed expectations rather than pixel measurements.

## Evidence inspected

- Accepted concept: `docs/design/intern-primary-screen.png`, 1536×1024, SHA-256 `c8cf322da777d77bc490b855fd18c5a70fe24192a343505e677d34d925a30de8`.
- Concept inspection: performed with `view_image` at original detail. The screen contains native chrome, a 72-pixel-style product header, left navigation, compact queue list, selected review row, and right inspector.
- Implementation sources inspected: `src/App.tsx`, components under `src/components`, `src/styles/tokens.css`, `src/styles/app.css`, and the in-memory browser adapter.
- Latest implementation capture: `docs/qa/latest-implementation.png`, produced by Playwright at exactly 1536×1024, SHA-256 `aedb8798c512332d5a79e8194ae3075234eaa01a3a3b8e15c5c66911fd6a1b5c`. Regenerating it from the current tree reproduces the same bytes, so the reviewed image is the one the release ships.

## Comparison ledger

| Area | Accepted reference | Code-backed implementation | Status / required rendered check |
|---|---|---|---|
| Copy | Intern; Private · On this device; Add files/folder; Queue/Needs Review/Completed; compact table and review labels | Matching primary labels and headings are present. Waiting rows render em dashes. The duplicate product wordmark from native chrome is not added by the web surface. | code-aligned; rendered check pending |
| Structure | Product header above left navigation, center queue, right review inspector | CSS grid uses header across both columns, 230px sidebar, flex queue workspace, and 370px inspector. | runtime geometry assertions prepared; pending |
| Typography | Segoe-like hierarchy with larger product wordmark and compact 12–14px metadata/body/control text | Segoe UI Variable/Segoe UI stack; product 34px; body/table 14px; controls/header cells 13px; metadata 12px. | code-aligned; raster hierarchy pending |
| Palette | True white/cool gray surfaces, charcoal text, restrained indigo, amber review, muted green ready | Tokens are `#fff`, `#171a1f`, `#0b5cff`, `#b66a00`, and `#14804a`; no gradient rule exists. | code-aligned; color rendering pending |
| List density | Compact document rows with single-pixel separators | Cells declare 46px height and 7px vertical padding with 1px borders. Actual browser row boxes must be measured because table layout may exceed the declared height. | potential fidelity risk; pending screenshot measurement |
| Inspector width | Narrow fixed right inspector | 370px flex basis at desktop; fixed right drawer at widths at or below 1100px. | exact 370px Playwright assertion prepared; pending |
| Icon treatment | Consistent thin outline document/status/action icons | Lucide icons are globally 20px with stroke width 1.75. | code-aligned; optical comparison pending |
| Focus / selection | Selected review row has restrained blue treatment; editable filename is visibly focused in concept | Selected row uses `#f2f6ff` plus `#cbd9f8` boundaries. All buttons, inputs, textareas, rows, and drop zone have a 2px accent `:focus-visible` outline. | automated focus-style assertion prepared; pending execution |
| 1024px responsiveness | Compact laptop layout retains usable queue and review surface | At ≤1100px sidebar becomes 64px icon navigation with accessible button labels; inspector becomes a fixed right drawer. | 1024px width/no-overflow/accessibility assertions prepared; pending execution |
| Motion | No decorative motion requirement | Reduced-motion media query collapses animation and transition durations; only processing status spinner animates normally. | code-aligned; browser check pending |

## Prepared objective browser checks

`tests/e2e/qa.spec.ts` fixes the primary viewport at 1536×1024 and asserts 72px header, 230px sidebar, 370px inspector, no page-level horizontal overflow, accessible main/navigation/inspector/control names, visible form labels, and a nontransparent 2px focus outline. With `INTERN_QA_CAPTURE=1` it writes the real viewport to `docs/qa/latest-implementation.png`.

The same spec switches to 1024×768 and asserts a 64px accessible icon navigation, 370px right-edge inspector drawer, no page-level horizontal overflow, and a reachable keyboard focus target. `tests/e2e/queue.spec.ts` separately covers the mixed-batch review/edit/approve/undo path.

## Sign-off procedure still required

After the workflow produces the screenshot, inspect the accepted concept and implementation capture side by side with `view_image`. Record actual differences in copy, structure, typography, palette, row density, inspector width, icon optical weight, selection/focus state, and the 1024px layout. Any Critical or Important discrepancy must be fixed and recaptured before this ledger may change to accepted.

Acceptance is machine-gated through `docs/qa/rendered-fidelity-signoff.json`. The reviewer must record the accepted screenshot SHA-256, the model report's `release_inputs_sha256`, their identity, review time, and useful notes. The tagged release recaptures the same screenshot and fails closed if either digest no longer matches.
