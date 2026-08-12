# Intern

Intern is a local-first Windows 10/11 utility for reviewing safe document rename
and move proposals. Documents, extracted text, OCR output, and model prompts stay
on the device. Intern has no telemetry, cloud fallback, or remote document
processing; the only network activity is an explicit, user-started download of
the pinned local model files.

## Development

```sh
npm ci
npm run dev
```

Use the pinned Node 24.15.0 (`.nvmrc`), Rust 1.88.0, and the committed
`Cargo.lock`. Run the deterministic clean-room
corpus generator and frontend gates with:

```sh
npm run fixtures
npm run check
npx playwright install chromium
npm run test:e2e
```

The browser development and Playwright builds use the real in-memory bridge; no
document content or test data is sent to a service. Fixture contents and gold
fields are documented in `fixtures/README.md` and `fixtures/expected.json`.

Run Rust tests with:

```sh
cargo test --locked --workspace --all-targets
```

## Windows runtime assets and installer

From PowerShell on Windows, fetch the exact llama.cpp b10361, PDFium
chromium/7881, Tesseract 5.5.2, and tessdata assets:

```powershell
cargo build --locked -p intern-worker --release --features windows-native
Copy-Item target/release/intern-worker.exe src-tauri/binaries/intern-worker-x86_64-pc-windows-msvc.exe
./scripts/fetch-windows-assets.ps1
npm run assets:verify -- --require-bundled
./scripts/stage-windows-runtime.ps1 -Destination "$env:TEMP/intern-runtime-stage"
./scripts/smoke-worker.ps1 -WorkerPath "$env:TEMP/intern-runtime-stage/intern-worker.exe" -RuntimeDirectory "$env:TEMP/intern-runtime-stage"
npm run tauri build -- --bundles nsis -- --locked
```

Every downloaded archive or trained-data file is rejected unless both its exact
byte length and committed SHA-256 digest match. The fetcher checks out the exact
vcpkg baseline, stages only required executables/DLLs/trained data and the full
upstream/vcpkg license closure, and records both source and packaged paths plus a
digest for every file in `src-tauri/resources/runtime-assets.json`.
Tauri produces a per-user NSIS installer. To smoke-test a clean installer:

```powershell
./scripts/smoke-installer.ps1 -InstallerPath target/release/bundle/nsis/Intern_0.1.0-alpha.1_x64-setup.exe
```

The installer includes third-party notices and the generated `licenses/`
inventory but never includes `.gguf` model
files. Local model weights are installed separately under the user's local
application data only after explicit setup.

## CI and release

CI runs fixture generation, TypeScript/Vitest/Vite, Playwright, Rust format,
Clippy with warnings denied, workspace tests, native fixture integration, asset
verification, the Windows Tauri build, and the installer smoke test. Runtime and
dependency caches are keyed by lockfiles and the runtime asset manifest.

The release workflow is deliberately limited to the exact annotated testing tag
`v0.1.0-alpha.1`. It requires accepted `docs/qa/model-evaluation.json` evidence,
runs the exact Q4 pair through llama.cpp b10361 in a temporary directory, and
publishes the NSIS executable, `THIRD_PARTY_NOTICES.md`, Microsoft SBOM Tool
4.1.5-generated and validated SPDX 2.2 documents for every actual vcpkg/native
runtime package, and `SHA256SUMS.txt` using the
repository token. It uses the checked-in release notes, refuses lightweight or
unexpected tags, and checks that temporary model files never become release
assets.
