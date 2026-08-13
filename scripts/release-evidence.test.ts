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
  const workflow = 'Release v0.1.0-alpha.2';
  const runId = '123456';
  const screenshot = 'rendered screenshot';
  const files = {
    model: 'docs/qa/model-evaluation.json',
    screenshot: 'docs/qa/latest-implementation.png',
    checklist: 'docs/qa/release-checklist.md',
    fidelity: 'docs/qa/rendered-fidelity-signoff.json',
    installed: 'docs/qa/installed-core-smoke.json',
    installer: 'release/Intern.exe',
    log: 'docs/qa/logs/installer-smoke.log',
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
  await writeFile(join(root, files.log), 'installer smoke passed');
  return { root, commit, workflow, runId, files };
}

async function createManifest(fixture: Awaited<ReturnType<typeof evidenceFixture>>) {
  const output = join(fixture.root, 'release/evidence-manifest.json');
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
    `--log=${fixture.files.log}`,
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
  expect(manifest.artifacts.logs).toHaveLength(1);
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

it('fails closed when an artifact is changed after the manifest is written', async () => {
  const fixture = await evidenceFixture();
  const manifestPath = await createManifest(fixture);
  await writeFile(join(fixture.root, fixture.files.installer), 'tampered installer');
  await expect(exec(process.execPath, [
    'scripts/validate-release-evidence.mjs', manifestPath, `--root=${fixture.root}`,
    `--commit=${fixture.commit}`, `--workflow=${fixture.workflow}`, `--run-id=${fixture.runId}`, '--run-attempt=2',
  ])).rejects.toMatchObject({ stderr: expect.stringContaining('hash') });
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
