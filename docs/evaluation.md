# Measuring accuracy: the corpus, the recording, and the baseline

Every claim this project makes about accuracy is a number from
`intern-evaluate` over the clean-room corpus in `fixtures/`. This page is how
those numbers are produced, and how CI keeps them from quietly getting worse.

## Three ways to run the evaluator

```text
intern-evaluate --fixtures fixtures/generated --expected fixtures/expected.json ...
```

| Mode | Needs | Takes | Use it for |
| --- | --- | --- | --- |
| **Live** (`--worker`, `--endpoint`, `--api-key`) | the parser worker, its runtime, and a running `llama-server` | most of an hour on a laptop CPU | the truth: what the model actually says |
| **Record** (live, plus `--record PATH`) | the same | the same | keeping what a live run saw, for replay |
| **Replay** (`--replay PATH`) | nothing but the repository | seconds | scoring an engine change without a model |

A recording (`fixtures/corpus-recording.json`) holds, per fixture, the text
the worker extracted and the reply the model gave, keyed by the SHA-256 of
the exact prompt that reply answers. Replay re-runs everything *after* the
model - distillation, validation, evidence checks, date-role inference, naming
- from that text and that reply, so any change to those stages is scored the
way a live run would score it.

## What replay can and cannot tell you

Replay is honest about its own limits. Before scoring a fixture it rebuilds
the prompt from the recorded text and compares the hash with the one recorded:

* **The hashes match.** The model was asked exactly this. The reply is real
  and the score is the score a live run would give.
* **They differ** because the prompt wording or the distillation changed.
  The recorded reply answers a question the engine no longer asks, and
  scoring it would measure nothing. The fixture is reported as
  `stale_prompt`, the run exits 2, and the fix is to re-record. Pass
  `--allow-stale` to score anyway while iterating locally; the records are
  marked `"stale": true` and CI never uses this flag.
* **The fixture bytes changed** (the generator was edited without
  re-recording): `stale_fixture`, same treatment.

So: a change to `validate.rs`, `evidence.rs`, `infer.rs`, `naming.rs`, or
`distill.rs`'s selection heuristics is measured for free. A change to
`prompt.rs`, or to what the digest contains, costs one live recording on a
machine with the runtime. That cost is the point. A prompt change nobody has
run the model over is a prompt change nobody has measured.

## The baseline gate

`fixtures/corpus-baseline.json` is the per-fixture score sheet the committed
recording earns. CI replays the corpus on every push and compares:

* any score that was right and is now wrong is a **regression**, and the job
  fails (exit 2);
* a trap score (`date_forbidden`, `party_forbidden`) that was clear and now
  fires is a regression too;
* a fixture whose status changed - it scored before and is now stale or
  unrecorded - is a regression;
* a score that was wrong and is now right is an **improvement**, reported in
  the log and never required.

`ready` on its own is not compared: readiness is a routing decision, and
`readiness_match` already scores whether it was the right one.

To accept a new state of the world - a better engine, or a deliberately
different trade - run replay with `--write-baseline fixtures/corpus-baseline.json`
and commit the result with the change that earned it. Reviewers can see, in
the diff of that file, exactly which fixtures changed and in which direction.

## Recording on Windows

The shipped runtime is Windows-only (PDFium, Tesseract, and llama.cpp are
pinned there by `src-tauri/resources/runtime-assets.json`), so the recording of
record is made on Windows:

```powershell
./scripts/fetch-windows-assets.ps1 -CacheDirectory $env:TEMP\intern-assets
cargo build --locked -p intern-worker --release --features windows-native
Copy-Item target\release\intern-worker.exe src-tauri\binaries\intern-worker-x86_64-pc-windows-msvc.exe
./scripts/stage-windows-runtime.ps1 -Destination C:\intern-stage
npm run fixtures
./scripts/record-corpus.ps1 -RuntimeDirectory C:\intern-stage -ModelPath "$env:LOCALAPPDATA\Intern\models\Qwen3.5-2B-Q4_K_M.gguf"
```

The script refuses a model whose SHA-256 is not the one `model-manifest.json`
pins, starts `llama-server` with the flags the app itself uses (one slot, CPU,
8,192-token context, the model's own chat template), records, writes the
baseline, and stops the server. Commit `fixtures/corpus-recording.json`,
`fixtures/corpus-baseline.json`, and the report's summary in
`docs/model-bakeoff.md`.

## Recording elsewhere

The engine and the worker are portable; only the packaged runtime is not. The
committed recording was made on Linux with the same pinned model, the same
llama.cpp release built from source, PDFium `chromium/7881` for Linux, the
same pinned `tessdata_fast` files, and the distribution's Tesseract 5.3.4
rather than the vcpkg 5.5.2 the installer ships; the `note` field of the
recording says so. OCR output can differ by a character between Tesseract
builds, which is why the OCR fixtures are marked `needs_review` in the gold
corpus and scored on routing rather than on the digits they misread. A
recording made on the packaged Windows runtime supersedes it; the workflow is
the same, with `INTERN_RUNTIME_DIR` pointing at a directory holding
`libpdfium.so`, a `tesseract.exe` symlink to the Tesseract binary, and
`tessdata/`.

## Reading a report

`--output report.json` writes the full report; without it, the report goes to
standard output. `summary` carries every boolean score as
`{correct, total, rate}`, the count of records by status, the review rate,
and inference and total-time percentiles (zero in replay, where nothing is
inferred). `records` carries one entry per fixture with the composed
filename, the description, the validated proposal, the review reasons, and
the scores. In replay every record says `"replayed": true`.

The scores are the ones `fixtures/README.md` describes: `date_correct` counts
the reviewed date or a listed acceptable one; `date_forbidden` counts a date
the corpus marks as a trap; `type_correct` accepts any answer carrying every
meaningful word of the reviewed type; `parties_correct` needs every reviewed
party and no spurious one; `description_covers_facts` needs every listed fact
in the sentence; `readiness_match` compares the routing decision with the
reviewed one.
