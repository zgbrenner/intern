import { readFile } from 'node:fs/promises';
import { expect, it } from 'vitest';

it('runs non-publishing browser, Windows, model, and installer QA and uploads evidence', async () => {
  const workflow = await readFile('.github/workflows/qa.yml', 'utf8');
  expect(workflow).toContain('workflow_dispatch:');
  expect(workflow).toContain('contents: read');
  expect(workflow).not.toContain('contents: write');
  expect(workflow).not.toContain('gh release create');
  expect(workflow).not.toContain('git push');
  expect(workflow).toContain('INTERN_QA_CAPTURE: "1"');
  expect(workflow).toContain('docs/qa/latest-implementation.png');
  expect(workflow).toContain('intern-evaluate');
  expect(workflow).toContain('run-model-evaluation.ps1');
  expect(workflow).toContain('smoke-installer.ps1');
  expect(workflow).toContain('validate-model-evaluation.mjs');
  expect(workflow).toContain('create-release-evidence.mjs');
  expect(workflow).toContain('validate-release-evidence.mjs');
  expect(workflow).toContain('docs/qa/release-evidence-manifest.json');
  expect(workflow).toContain('if: always()');
  expect(workflow).toContain('actions/upload-artifact@v4');
});

it('never publishes as a side effect of merging to main', async () => {
  const workflow = await readFile('.github/workflows/release.yml', 'utf8');
  // Releasing is dispatched deliberately; a merge must not tag or publish.
  expect(workflow).toContain('workflow_dispatch:');
  expect(workflow).not.toContain('branches: [main]');
  expect(workflow).toContain('group: intern-v0.1.0-alpha.1-release');
  expect(workflow).toContain('release_target:');
});

it('gates the exact main commit before creating its annotated tag and publishing', async () => {
  const workflow = await readFile('.github/workflows/release.yml', 'utf8');
  expect(workflow).toContain('cargo build --locked -p intern-engine --release --bin intern-evaluate');
  expect(workflow).toContain('run-model-evaluation.ps1');
  expect(workflow).toContain('-OutputPath release\\model-evaluation.json');
  expect(workflow).toContain('create-release-evidence.mjs');
  expect(workflow).toContain('validate-release-evidence.mjs');
  expect(workflow).toContain('git tag -a $Tag $env:GITHUB_SHA');
  expect(workflow).toContain('git push origin "refs/tags/$Tag"');
  expect(workflow.indexOf('validate-release-evidence.mjs')).toBeLessThan(workflow.indexOf('git tag -a $Tag $env:GITHUB_SHA'));
  expect(workflow.indexOf('validate-release-evidence.mjs')).toBeLessThan(workflow.indexOf('gh release create'));
  expect(workflow.indexOf('git tag -a $Tag $env:GITHUB_SHA')).toBeLessThan(workflow.indexOf('gh release create'));
  expect(workflow).toContain('gh release view $Tag --json isDraft');
  expect(workflow).toContain('INTERN_QA_CAPTURE: "1"');
  expect(workflow).toContain('release\\cargo-test.log');
  expect(workflow).toContain('--log=release/cargo-test.log');
  expect(workflow).toContain('Copy-Item docs\\qa\\latest-implementation.png $Release');
  expect(workflow).toContain('--screenshot=release/latest-implementation.png');
  expect(workflow).toContain('--fidelity-signoff=release/rendered-fidelity-signoff.json');
});
