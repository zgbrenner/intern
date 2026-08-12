# Intern clean-room fixtures

This corpus is generated from scratch for Intern. Every person, organization,
address, identifier, date, and event is fictional. It contains no source,
fixtures, prompts, schemas, or document content from a predecessor project.

Run `npm run fixtures` from the repository root. The command recreates
`fixtures/generated/`, writes a deterministic SHA-256 manifest beside the
artifacts, and checks the generated answers against `expected.json`. Generation
fixes timestamps, ZIP metadata, PDF object order, compression level, and
document metadata so two runs on supported Node versions produce identical
bytes.

## What the corpus covers

**Formats and extraction paths.** Text PDFs, an image-only PDF, a mixed
text/scan PDF, DOCX with header/footer/footnote/table, Markdown, PNG/JPEG/TIFF
document images, a rotated low-resolution scan, encrypted and malformed PDFs, a
100-page boundary document, exact duplicates, an unsupported format, and an
Office lock file. The intentionally invalid fixtures must remain invalid.

**Document understanding.** Seven fixtures exist specifically to test whether
Intern chooses the date it *understood* rather than the date that was easiest to
find:

| Fixture | The trap |
| --- | --- |
| `statement-of-work.pdf` | ~29,000 characters over 14 pages. Its effective date is past the first 14,000 characters and before the last 8,000, so no head/tail window can reach it. Page one carries the master agreement's date and the last page carries the signature date; both are wrong. |
| `termination-notice.pdf` | A notice date, a separate termination-effective date, two response deadlines, the date of the agreement being terminated, and a lawyer copied on it who is not a party. |
| `consulting-amendment.pdf` | Names the original agreement's date nine times. Counting occurrences gets the wrong answer. |
| `vendor-invoice.pdf` | An invoice date and a payment due date, both prominent. |
| `settlement-agreement.pdf` | Boilerplate-heavy. Its effective date differs from its payment date and from the date of the dispute it settles. |
| `order-form.docx` | A subscription start date in a table, a different signature date in prose, an end date, and a master agreement date. |
| `ambiguous-note.pdf` | No document type, no defining date, several names in no clear role. It exists to be sent to review. |

`expected.json` records, for each fixture, the reviewed `document_date`, any
other `acceptable_dates`, and the `forbidden_dates` and `forbidden_parties` that
a careless reading would produce. `intern-evaluate` scores against all of them,
so "picked a date" and "picked the right date" are measured separately.

## What the scanned fixtures can and cannot prove

The image fixtures are drawn with a 5×7 bitmap font defined in the generator, so
the corpus needs no font files and stays byte-identical everywhere. Tesseract
reads that font imperfectly. Measured through the packaged worker with the pinned
runtime:

| Fixture | Mean OCR confidence | Representative misreadings |
| --- | --- | --- |
| `document-image.jpg` | 95.4 | none |
| `document-image.tiff` | 86.3 | `2025` as `26275` |
| `document-image.png` | 82.7 | `PO-310` as `PO-31i`, `2025` as `2625` |
| `scanned-lease.pdf` | 80.4 | `EFFECTIVE` as `EFFECTIWE`, `CEDAR` as `LEDAR`, `2024` as `24h24` |
| `rotated-low-resolution-scan.png` | 76.1 | `DR-771` as `OR-?771`, `COURIERS` as `COURTERS` |
| `mixed-signature.pdf` page 2 | 61.8 | `KITE` as `BRITE`, `SIGNATURE` as `SIGHATURE` |

Digits and narrow glyphs suffer most. So these fixtures prove routing, page
assembly, orientation handling, and that a poor scan is sent to review — which is
what `expected.json` marks them `needs_review` for. They do not prove that Intern
names scanned documents accurately, and no assertion should imply they do.
`scripts/smoke-worker.ps1` therefore asserts the multi-word alphabetic content
that survives OCR rather than dates or identifiers, and asserts a confidence floor
on the rotated fixture, because a page read in the wrong orientation returns a
full page of plausible-looking gibberish that only confidence distinguishes from a
real reading.

The font has no comma glyph, and unmapped characters render as blank space.
Assertions written against the generator's prose rather than its rasterised output
are unsatisfiable by construction.

## Updating the reviewed answers

To intentionally revise them, edit the generator definition, review the new
corpus, and run:

```sh
npm exec --package=node@24.15.0 -- node fixtures/generate-fixtures.mjs --update-gold
```

Do not use this flag in CI.
