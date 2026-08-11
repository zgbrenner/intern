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
  expect(workflow).toContain('intern-model-evaluator');
  expect(workflow).toContain('intern-model-evidence-validator');
  expect(workflow).toContain('run-model-evaluation.ps1');
  expect(workflow).toContain('smoke-installer.ps1');
  expect(workflow).toContain('validate-model-evaluation.mjs');
  expect(workflow).toContain('create-release-evidence.mjs');
  expect(workflow).toContain('validate-release-evidence.mjs');
  expect(workflow).toContain('docs/qa/release-evidence-manifest.json');
  expect(workflow).toContain('if: always()');
  expect(workflow).toContain('actions/upload-artifact@v4');
});

it('reruns production model evidence at the tag and gates publishing on the release-run manifest', async () => {
  const workflow = await readFile('.github/workflows/release.yml', 'utf8');
  expect(workflow).toContain('cargo build --locked -p intern-app --release --bin intern-model-evaluator');
  expect(workflow).toContain('run-model-evaluation.ps1');
  expect(workflow).toContain('-OutputPath release\\model-evaluation.json');
  expect(workflow).toContain('create-release-evidence.mjs');
  expect(workflow).toContain('validate-release-evidence.mjs');
  expect(workflow.indexOf('validate-release-evidence.mjs')).toBeLessThan(workflow.indexOf('gh release create'));
  expect(workflow).toContain('INTERN_QA_CAPTURE: "1"');
  expect(workflow).toContain('release\\cargo-test.log');
  expect(workflow).toContain('--log=release/cargo-test.log');
  expect(workflow).toContain('Copy-Item docs\\qa\\latest-implementation.png $Release');
  expect(workflow).toContain('--screenshot=release/latest-implementation.png');
  expect(workflow).toContain('--fidelity-signoff=release/rendered-fidelity-signoff.json');
});
