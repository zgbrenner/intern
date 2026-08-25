import { createHash } from 'node:crypto';
import { execFile } from 'node:child_process';
import { mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { promisify } from 'node:util';
import { expect, it } from 'vitest';

const exec = promisify(execFile);
const sha = (value: string) => createHash('sha256').update(value).digest('hex');

async function evidenceFixture() {
  const root = await mkdtemp(join(tmpdir(), 'intern-release-evidence-'));
  await mkdir(join(root, 'docs/qa/logs'), { recursive: true });
  await mkdir(join(root, 'release'), { recursive: true });
  const commit = 'a'.repeat(40);
  const workflow = 'Release v0.1.0-alpha.3';
  const runId = '123456';
  const screenshot = 'rendered screenshot';
  const files = {
    model: 'docs/qa/model-evaluation.json',
    screenshot: 'docs/qa/latest-implementation.png',
    checklist: 'docs/qa/release-checklist.md',
    fidelity: 'docs/qa/rendered-fidelity-signoff.json',
    installed: 'docs/qa/installed-core-smoke.json',
    installer: 'release/Intern.exe',
    latest: 'release/latest.json',
    runtime: 'release/runtime-assets.json',
    notices: 'release/THIRD_PARTY_NOTICES.md',
    checksum: 'release/SHA256SUMS.txt',
    applicationSbom: 'release/Intern-v0.1.0-alpha.3.spdx.json',
    runtimeSbom: 'release/Intern-v0.1.0-alpha.3-runtime-tesseract.spdx.json',
    cargoLog: 'release/cargo-test.log',
    modelLog: 'release/model-evaluation.log',
    installerLog: 'release/installer-smoke.log',
  };
  await writeFile(join(root, files.model), JSON.stringify({
    schema_version: 2,
    pipeline: 'new',
    status: 'completed',
    commit,
    release_inputs_sha256: 'c'.repeat(64),
    runner: { os: 'Windows', arch: 'X64', ci_run_id: runId },
    acceptance: { status: 'accepted', failures: [] },
  }));
  await writeFile(join(root, files.screenshot), screenshot);
  await writeFile(join(root, files.checklist), '# accepted release checklist\n');
  await writeFile(join(root, files.fidelity), JSON.stringify({
    schema_version: 1,
    status: 'accepted',
    release_inputs_sha256: 'c'.repeat(64),
    screenshot_path: files.screenshot,
    screenshot_sha256: sha(screenshot),
    reviewer: 'release reviewer',
    reviewed_at: '2026-08-11T12:00:00.000Z',
    notes: 'No critical or important fidelity differences remain.',
  }));
  await writeFile(join(root, files.installed), JSON.stringify({
    schema_version: 1,
    status: 'accepted',
    commit,
    workflow,
    run_id: runId,
    run_attempt: '2',
    installer_sha256: sha('installer bytes'),
    checks: {
      app_launched: true,
      clean_shutdown: true,
      runtime_inventory_verified: true,
      installed_worker_core_path: true,
      uninstall_succeeded: true,
      user_data_retained: true,
    },
  }));
  await writeFile(join(root, files.installer), 'installer bytes');
  await writeFile(join(root, files.latest), JSON.stringify({ version: '0.1.0-alpha.3' }));
  await writeFile(join(root, files.runtime), JSON.stringify({ schema_version: 1, packages: [{ name: 'Intern', version: '0.1.0-alpha.3' }] }));
  await writeFile(join(root, files.notices), 'Intern 0.1.0-alpha.3 notices');
  await writeFile(join(root, files.applicationSbom), JSON.stringify({
    spdxVersion: 'SPDX-2.2', SPDXID: 'SPDXRef-DOCUMENT', name: 'Intern 0.1.0-alpha.3',
    documentNamespace: 'https://example.test/intern-alpha.3', packages: [{ SPDXID: 'SPDXRef-Package', name: 'Intern', versionInfo: '0.1.0-alpha.3' }],
  }));
  // Runtime SBOMs intentionally carry their component's pinned version, not the
  // application release version. Filename classification makes this distinction
  // explicit and leaves both documents evidence-bound.
  await writeFile(join(root, files.runtimeSbom), JSON.stringify({
    spdxVersion: 'SPDX-2.2', SPDXID: 'SPDXRef-DOCUMENT', name: 'Tesseract 5.5.2',
    documentNamespace: 'https://example.test/tesseract-5.5.2', packages: [{ SPDXID: 'SPDXRef-Package', name: 'Tesseract', versionInfo: '5.5.2' }],
  }));
  await writeFile(join(root, files.cargoLog), 'cargo test passed');
  await writeFile(join(root, files.modelLog), 'model evaluation passed');
  await writeFile(join(root, files.installerLog), 'installer smoke passed');
  const checksumFiles = [files.installer, files.latest, files.runtime, files.notices, files.applicationSbom, files.runtimeSbom, files.cargoLog, files.modelLog, files.installerLog];
  const sums = (await Promise.all(checksumFiles.map(async (path) => `${sha(await readFile(join(root, path)))}  ${path.split('/').at(-1)}`)))
    .sort().join('\n');
  await writeFile(join(root, files.checksum), `${sums}\n`);
  return { root, commit, workflow, runId, files };
}

async function checksumContents(
  fixture: Awaited<ReturnType<typeof evidenceFixture>>,
  { omit, replace }: { omit?: string; replace?: [string, string] } = {},
) {
  const { files, root } = fixture;
  const checksumFiles = [files.installer, files.latest, files.runtime, files.notices, files.applicationSbom, files.runtimeSbom, files.cargoLog, files.modelLog, files.installerLog]
    .filter((path) => path !== omit);
  const sums = await Promise.all(checksumFiles.map(async (path) => {
    const hash = replace?.[0] === path ? replace[1] : sha(await readFile(join(root, path)));
    return `${hash}  ${path.split('/').at(-1)}`;
  }));
  return `${sums.sort().join('\n')}\n`;
}

async function createManifest(fixture: Awaited<ReturnType<typeof evidenceFixture>>) {
  const output = join(fixture.root, 'release/release-evidence-manifest.json');
  await exec(process.execPath, [
    'scripts/create-release-evidence.mjs',
    `--root=${fixture.root}`,
    `--output=${output}`,
    `--commit=${fixture.commit}`,
    `--workflow=${fixture.workflow}`,
    `--run-id=${fixture.runId}`,
    '--run-attempt=2',
    `--model-evaluation=${fixture.files.model}`,
    `--screenshot=${fixture.files.screenshot}`,
    `--checklist=${fixture.files.checklist}`,
    `--fidelity-signoff=${fixture.files.fidelity}`,
    `--installed-core-smoke=${fixture.files.installed}`,
    `--installer=${fixture.files.installer}`,
    `--latest-json=${fixture.files.latest}`,
    `--runtime-assets=${fixture.files.runtime}`,
    `--notices=${fixture.files.notices}`,
    `--checksum=${fixture.files.checksum}`,
    `--sbom=${fixture.files.applicationSbom}`,
    `--sbom=${fixture.files.runtimeSbom}`,
    `--log=${fixture.files.cargoLog}`,
    `--log=${fixture.files.modelLog}`,
    `--log=${fixture.files.installerLog}`,
  ]);
  return output;
}

it('creates and validates a manifest bound to all release evidence and the exact run', async () => {
  const fixture = await evidenceFixture();
  const manifestPath = await createManifest(fixture);
  const manifest = JSON.parse(await readFile(manifestPath, 'utf8'));

  expect(manifest.subject).toEqual({
    commit: fixture.commit,
    workflow: fixture.workflow,
    run_id: fixture.runId,
    run_attempt: '2',
  });
  expect(manifest.artifacts.logs).toHaveLength(3);
  expect(manifest.distribution.sboms).toHaveLength(2);
  expect(manifest.signoffs).toEqual({
    model_evaluation: 'accepted',
    rendered_fidelity: 'accepted',
    installed_core_path: 'accepted',
  });
  await expect(exec(process.execPath, [
    'scripts/validate-release-evidence.mjs',
    manifestPath,
    `--root=${fixture.root}`,
    `--commit=${fixture.commit}`,
    `--workflow=${fixture.workflow}`,
    `--run-id=${fixture.runId}`,
    '--run-attempt=2',
  ])).resolves.toMatchObject({ stdout: expect.stringContaining('"status":"accepted"') });
});

it('requires the current prerelease workflow contract and rejects regex-like runtime filename near misses', async () => {
  const fixture = await evidenceFixture();
  const manifestPath = await createManifest(fixture);
  const manifest = JSON.parse(await readFile(manifestPath, 'utf8'));

  for (const workflow of ['Release build v0.1.0-alpha.3', 'Release v0.1.0', 'Release v0.1.0-alpha.2']) {
    manifest.subject.workflow = workflow;
    await writeFile(manifestPath, JSON.stringify(manifest));
    await expect(exec(process.execPath, [
      'scripts/validate-release-evidence.mjs', manifestPath, `--root=${fixture.root}`,
      `--commit=${fixture.commit}`, `--workflow=${workflow}`, `--run-id=${fixture.runId}`, '--run-attempt=2',
    ])).rejects.toMatchObject({ stderr: expect.stringContaining('workflow must be exactly') });
  }

  manifest.subject.workflow = fixture.workflow;
  const runtime = manifest.distribution.sboms.find((sbom: { path: string }) => sbom.path === fixture.files.runtimeSbom);
  const nearMiss = 'release/Intern-v0x1y0-alpha.3-runtime-tesseract.spdx.json';
  await writeFile(join(fixture.root, nearMiss), await readFile(join(fixture.root, fixture.files.runtimeSbom)));
  runtime.path = nearMiss;
  await writeFile(manifestPath, JSON.stringify(manifest));
  await expect(exec(process.execPath, [
    'scripts/validate-release-evidence.mjs', manifestPath, `--root=${fixture.root}`,
    `--commit=${fixture.commit}`, `--workflow=${fixture.workflow}`, `--run-id=${fixture.runId}`, '--run-attempt=2',
  ])).rejects.toMatchObject({ stderr: expect.stringContaining('SBOM filename does not match') });
});

it('rejects release evidence without exactly one filename-identified application SBOM', async () => {
  const fixture = await evidenceFixture();
  const manifestPath = await createManifest(fixture);
  const manifest = JSON.parse(await readFile(manifestPath, 'utf8'));
  const allSboms = manifest.distribution.sboms;
  manifest.distribution.sboms = allSboms.filter((sbom: { path: string }) => sbom.path === fixture.files.runtimeSbom);
  await writeFile(manifestPath, JSON.stringify(manifest));
  await expect(exec(process.execPath, [
    'scripts/validate-release-evidence.mjs', manifestPath, `--root=${fixture.root}`,
    `--commit=${fixture.commit}`, `--workflow=${fixture.workflow}`, `--run-id=${fixture.runId}`, '--run-attempt=2',
  ])).rejects.toMatchObject({ stderr: expect.stringContaining('exactly one application SBOM') });

  const application = allSboms.find((sbom: { path: string }) => sbom.path === fixture.files.applicationSbom);
  const runtime = allSboms.find((sbom: { path: string }) => sbom.path === fixture.files.runtimeSbom);
  manifest.distribution.sboms = [application, application, runtime];
  await writeFile(manifestPath, JSON.stringify(manifest));
  await expect(exec(process.execPath, [
    'scripts/validate-release-evidence.mjs', manifestPath, `--root=${fixture.root}`,
    `--commit=${fixture.commit}`, `--workflow=${fixture.workflow}`, `--run-id=${fixture.runId}`, '--run-attempt=2',
  ])).rejects.toMatchObject({ stderr: expect.stringContaining('exactly one application SBOM') });
});

it('fails closed when an artifact is changed after the manifest is written', async () => {
  const fixture = await evidenceFixture();
  const manifestPath = await createManifest(fixture);
  await writeFile(join(fixture.root, fixture.files.installer), 'tampered installer');
  await expect(exec(process.execPath, [
    'scripts/validate-release-evidence.mjs', manifestPath, `--root=${fixture.root}`,
    `--commit=${fixture.commit}`, `--workflow=${fixture.workflow}`, `--run-id=${fixture.runId}`, '--run-attempt=2',
  ])).rejects.toMatchObject({ stderr: expect.stringContaining('hash') });
});

it('rejects malformed, duplicate, missing, and mismatched checksum coverage before release evidence is accepted', async () => {
  const fixture = await evidenceFixture();
  await writeFile(join(fixture.root, fixture.files.checksum), `${'0'.repeat(64)}  ../escape.exe\n`);
  let manifestPath = await createManifest(fixture);
  await expect(exec(process.execPath, [
    'scripts/validate-release-evidence.mjs', manifestPath, `--root=${fixture.root}`,
    `--commit=${fixture.commit}`, `--workflow=${fixture.workflow}`, `--run-id=${fixture.runId}`, '--run-attempt=2',
  ])).rejects.toMatchObject({ stderr: expect.stringContaining('malformed SHA256SUMS') });

  const installerHash = sha(await readFile(join(fixture.root, fixture.files.installer)));
  await writeFile(join(fixture.root, fixture.files.checksum), `${installerHash}  Intern.exe\n${installerHash}  Intern.exe\n`);
  manifestPath = await createManifest(fixture);
  await expect(exec(process.execPath, [
    'scripts/validate-release-evidence.mjs', manifestPath, `--root=${fixture.root}`,
    `--commit=${fixture.commit}`, `--workflow=${fixture.workflow}`, `--run-id=${fixture.runId}`, '--run-attempt=2',
  ])).rejects.toMatchObject({ stderr: expect.stringContaining('duplicate SHA256SUMS') });

  await writeFile(
    join(fixture.root, fixture.files.checksum),
    await checksumContents(fixture, { omit: fixture.files.runtimeSbom }),
  );
  manifestPath = await createManifest(fixture);
  await expect(exec(process.execPath, [
    'scripts/validate-release-evidence.mjs', manifestPath, `--root=${fixture.root}`,
    `--commit=${fixture.commit}`, `--workflow=${fixture.workflow}`, `--run-id=${fixture.runId}`, '--run-attempt=2',
  ])).rejects.toMatchObject({ stderr: expect.stringContaining('SHA256SUMS must cover every distributable release file exactly once') });

  await writeFile(
    join(fixture.root, fixture.files.checksum),
    await checksumContents(fixture, { replace: [fixture.files.installer, '0'.repeat(64)] }),
  );
  manifestPath = await createManifest(fixture);
  await expect(exec(process.execPath, [
    'scripts/validate-release-evidence.mjs', manifestPath, `--root=${fixture.root}`,
    `--commit=${fixture.commit}`, `--workflow=${fixture.workflow}`, `--run-id=${fixture.runId}`, '--run-attempt=2',
  ])).rejects.toMatchObject({ stderr: expect.stringContaining('SHA256SUMS hash does not match Intern.exe') });
});

it('fails closed unless rendered fidelity and the installed core path are accepted', async () => {
  const fixture = await evidenceFixture();
  const fidelity = JSON.parse(await readFile(join(fixture.root, fixture.files.fidelity), 'utf8'));
  fidelity.status = 'pending';
  fidelity.reviewer = null;
  fidelity.reviewed_at = null;
  await writeFile(join(fixture.root, fixture.files.fidelity), JSON.stringify(fidelity));
  const manifestPath = await createManifest(fixture);
  await expect(exec(process.execPath, [
    'scripts/validate-release-evidence.mjs', manifestPath, `--root=${fixture.root}`,
    `--commit=${fixture.commit}`, `--workflow=${fixture.workflow}`, `--run-id=${fixture.runId}`, '--run-attempt=2',
  ])).rejects.toMatchObject({ stderr: expect.stringContaining('rendered fidelity') });

  fidelity.status = 'accepted';
  fidelity.reviewer = 'release reviewer';
  fidelity.reviewed_at = '2026-08-11T12:00:00.000Z';
  await writeFile(join(fixture.root, fixture.files.fidelity), JSON.stringify(fidelity));
  const installed = JSON.parse(await readFile(join(fixture.root, fixture.files.installed), 'utf8'));
  installed.status = 'rejected';
  installed.checks.clean_shutdown = false;
  await writeFile(join(fixture.root, fixture.files.installed), JSON.stringify(installed));
  await createManifest(fixture);
  await expect(exec(process.execPath, [
    'scripts/validate-release-evidence.mjs', manifestPath, `--root=${fixture.root}`,
    `--commit=${fixture.commit}`, `--workflow=${fixture.workflow}`, `--run-id=${fixture.runId}`, '--run-attempt=2',
  ])).rejects.toMatchObject({ stderr: expect.stringContaining('installed core path') });
});

it('rejects copied evidence that names a different commit or workflow run', async () => {
  const fixture = await evidenceFixture();
  const manifestPath = await createManifest(fixture);
  await expect(exec(process.execPath, [
    'scripts/validate-release-evidence.mjs', manifestPath, `--root=${fixture.root}`,
    `--commit=${'b'.repeat(40)}`, `--workflow=${fixture.workflow}`, `--run-id=${fixture.runId}`, '--run-attempt=2',
  ])).rejects.toMatchObject({ stderr: expect.stringContaining('commit') });
});

it('does not accept invented installed-core check names', async () => {
  const fixture = await evidenceFixture();
  const installed = JSON.parse(await readFile(join(fixture.root, fixture.files.installed), 'utf8'));
  installed.checks = { a: true, b: true, c: true, d: true, e: true, f: true };
  await writeFile(join(fixture.root, fixture.files.installed), JSON.stringify(installed));
  const manifestPath = await createManifest(fixture);
  await expect(exec(process.execPath, [
    'scripts/validate-release-evidence.mjs', manifestPath, `--root=${fixture.root}`,
    `--commit=${fixture.commit}`, `--workflow=${fixture.workflow}`, `--run-id=${fixture.runId}`, '--run-attempt=2',
  ])).rejects.toMatchObject({ stderr: expect.stringContaining('installed core path') });
});

it('allow-pending permits only an unreviewed screenshot, never a rejected sign-off', async () => {
  const fixture = await evidenceFixture();
  const fidelity = JSON.parse(await readFile(join(fixture.root, fixture.files.fidelity), 'utf8'));
  fidelity.status = 'rejected';
  await writeFile(join(fixture.root, fixture.files.fidelity), JSON.stringify(fidelity));
  const manifestPath = await createManifest(fixture);
  await expect(exec(process.execPath, [
    'scripts/validate-release-evidence.mjs', manifestPath, '--allow-pending', `--root=${fixture.root}`,
    `--commit=${fixture.commit}`, `--workflow=${fixture.workflow}`, `--run-id=${fixture.runId}`, '--run-attempt=2',
  ])).rejects.toMatchObject({ stderr: expect.stringContaining('rendered fidelity') });
});

it('binds a recaptured release screenshot by digest even when its packaged path changes', async () => {
  const fixture = await evidenceFixture();
  const fidelity = JSON.parse(await readFile(join(fixture.root, fixture.files.fidelity), 'utf8'));
  fidelity.screenshot_path = 'qa-artifact/latest-implementation.png';
  await writeFile(join(fixture.root, fixture.files.fidelity), JSON.stringify(fidelity));
  const manifestPath = await createManifest(fixture);
  await expect(exec(process.execPath, [
    'scripts/validate-release-evidence.mjs', manifestPath, `--root=${fixture.root}`,
    `--commit=${fixture.commit}`, `--workflow=${fixture.workflow}`, `--run-id=${fixture.runId}`, '--run-attempt=2',
  ])).resolves.toMatchObject({ stdout: expect.stringContaining('"status":"accepted"') });
});
