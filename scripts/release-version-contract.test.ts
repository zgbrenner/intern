import { readFile } from 'node:fs/promises';
import { expect, it } from 'vitest';

const version = '0.1.0-alpha.4';
const tag = `v${version}`;

it('keeps every current alpha.3 release surface synchronized without rewriting historical alpha.2 records', async () => {
  const [packageJson, packageLock, workspace, tauri, workerProtocol, smoke, sbom, assets, notices, readme, checklist, release, ci, notes] = await Promise.all([
    readFile('package.json', 'utf8').then(JSON.parse),
    readFile('package-lock.json', 'utf8').then(JSON.parse),
    readFile('Cargo.toml', 'utf8'),
    readFile('src-tauri/tauri.conf.json', 'utf8').then(JSON.parse),
    readFile('crates/intern-worker/tests/protocol.rs', 'utf8'),
    readFile('scripts/smoke-worker.ps1', 'utf8'),
    readFile('scripts/generate-sbom.ps1', 'utf8'),
    readFile('scripts/fetch-windows-assets.ps1', 'utf8'),
    readFile('src-tauri/resources/THIRD_PARTY_NOTICES.md', 'utf8'),
    readFile('README.md', 'utf8'),
    readFile('docs/qa/release-checklist.md', 'utf8'),
    readFile('.github/workflows/release.yml', 'utf8'),
    readFile('.github/workflows/ci.yml', 'utf8'),
    readFile(`docs/releases/${tag}.md`, 'utf8'),
  ]);

  expect(packageJson.version).toBe(version);
  expect(packageLock.version).toBe(version);
  expect(packageLock.packages[''].version).toBe(version);
  expect(workspace).toContain(`version = "${version}"`);
  expect(tauri.version).toBe(version);
  expect(workerProtocol).toContain(`worker_version":"${version}`);
  expect(smoke).toContain(`worker_version -ne "${version}"`);
  expect(sbom).toContain(`-Version "${version}"`);
  expect(sbom).toContain(`Intern-v${version}.spdx.json`);
  expect(assets).toContain(`version = "${version}"`);
  expect(notices).toContain(`Intern ${version}`);
  expect(readme).toContain(`Intern_${version}_x64-setup.exe`);
  expect(checklist).toContain(`Intern v${version} release checklist`);
  expect(checklist).toContain('pending/blocked');
  expect(notes).toContain(`# Intern ${tag}`);
  expect(notes).toContain('not Authenticode signed');
  expect(release).toContain(`name: Release ${tag}`);
  expect(release).toContain(`RELEASE_TAG: ${tag}`);
  expect(release).toContain(`docs/releases/${tag}.md`);
  expect(release).toContain(`--title 'Intern ${tag}'`);
  expect(release).toContain(`group: intern-${tag}-release`);
  expect(ci).toContain(`intern-${tag}-windows-`);
});

it('pins every release-critical action to an immutable commit and preserves the release gate order', async () => {
  const workflows = await Promise.all(['ci.yml', 'lockfile.yml', 'pages.yml', 'qa.yml', 'release.yml']
    .map((name) => readFile(`.github/workflows/${name}`, 'utf8')));
  for (const workflow of workflows) {
    const uses = workflow.split('\n').filter((line) => line.includes('uses: actions/'));
    expect(uses.length).toBeGreaterThan(0);
    for (const line of uses) expect(line).toMatch(/actions\/[\w/-]+@[a-f0-9]{40}\s+# v\d+/);
  }
  const release = workflows[4];
  const gateMarkers = [
    'cargo run --locked -p intern-release-verifier --',
    'SHA256SUMS.txt',
    'validate-release-evidence.mjs',
    'attest-build-provenance@',
    'git tag -a $Tag $env:GITHUB_SHA',
    'gh release create',
  ];
  const gateIndexes = gateMarkers.map((marker) => release.indexOf(marker));
  for (const index of gateIndexes) expect(index).toBeGreaterThanOrEqual(0);
  for (let index = 0; index < gateIndexes.length - 1; index += 1) {
    expect(gateIndexes[index]).toBeLessThan(gateIndexes[index + 1]);
  }
});
