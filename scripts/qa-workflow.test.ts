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
  expect(workflow).toContain('if: always()');
  expect(workflow).toContain('actions/upload-artifact@v4');
});
