# Intern clean-room fixtures

This corpus is generated from scratch for Intern. Every person, organization,
address, identifier, date, and event is fictional. It contains no source,
fixtures, prompts, schemas, or document content from a predecessor project.

Run `npm run fixtures` from the repository root. The command recreates
`fixtures/generated/`, writes a deterministic SHA-256 manifest beside the
artifacts, and checks the generated gold fields against `expected.json`.
Generation fixes timestamps, ZIP metadata, PDF object order, compression level,
and document metadata so two runs on supported Node versions produce identical
bytes.

The corpus covers text and image-only PDFs, a mixed text/scan PDF, a DOCX with a
header/footer/footnote/table, ambiguous dates, Markdown minutes, a rotated scan,
encrypted and malformed PDFs, a 100-page boundary document, PNG/JPEG/TIFF
document images, exact duplicates, an unsupported format, and an Office lock
file. The intentionally invalid fixtures must remain invalid.

To intentionally revise the reviewed gold fields, edit the generator definition,
review the new corpus, and run:

```sh
npm exec --package=node@24.15.0 -- node fixtures/generate-fixtures.mjs --update-gold
```

Do not use this flag in CI.
