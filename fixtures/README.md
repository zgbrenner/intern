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

## Updating the reviewed answers

To intentionally revise them, edit the generator definition, review the new
corpus, and run:

```sh
npm exec --package=node@24.15.0 -- node fixtures/generate-fixtures.mjs --update-gold
```

Do not use this flag in CI.
