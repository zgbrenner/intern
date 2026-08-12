import { readFile } from 'node:fs/promises';
import { expect, it } from 'vitest';

it('runs both pinned models through the production evaluator and llama.cpp settings', async () => {
  const script = await readFile('scripts/run-model-evaluation.ps1', 'utf8');
  expect(script).toContain('Qwen2.5-VL-3B-Instruct-Q4_K_M.gguf');
  expect(script).toContain('Qwen2.5-VL-3B-Instruct-Q8_0.gguf');
  expect(script).toContain('d02fe9b69ad8cadbbd228e387667af66612c44bed29ffc8eb1e7caf9ac486c12');
  expect(script).toContain('fa8aeb3b6bf6152774e87d13e09892aa065f4e0c4abe90806cd8ab18ff72d9fe');
  expect(script).toContain('intern-model-evaluator.exe');
  for (const argument of ['--host', '--api-key', '--parallel', '--ctx-size', '--n-gpu-layers']) {
    expect(script).toContain(`"${argument}"`);
  }
  expect(script).toContain('PeakWorkingSet64');
  expect(script).toContain('docs/qa/model-evaluation.json');
});

it('the evaluator uses the production worker, prompt client, packet builder, and validator', async () => {
  const source = await readFile('src-tauri/src/bin/intern-model-evaluator.rs', 'utf8');
  expect(source).toContain('SupervisedWorker');
  expect(source).toContain('ModelClient');
  expect(source).toContain('build_document_packet');
  expect(source).toContain('validate_proposal');
  expect(source).not.toContain('build_prompt(');
  expect(source).toContain('.all(');
});

it('replays checked-in evidence through production extraction and validation before release', async () => {
  const verifier = await readFile('src-tauri/src/bin/intern-model-evidence-validator.rs', 'utf8');
  expect(verifier).toContain('SupervisedWorker');
  expect(verifier).toContain('build_document_packet');
  expect(verifier).toContain('validate_proposal');
  expect(verifier).toContain('document_input_sha256');

  const release = await readFile('.github/workflows/release.yml', 'utf8');
  expect(release).toContain('intern-model-evidence-validator');
  expect(release).toContain('validate-model-evaluation.mjs');
  expect(release.indexOf('intern-model-evidence-validator')).toBeLessThan(release.lastIndexOf('validate-model-evaluation.mjs'));

  const smoke = await readFile('scripts/smoke-q4-runtime.ps1', 'utf8');
  expect(smoke).toContain('qwen2.5-vl-3b-instruct-q4-k-m');
  expect(smoke).toContain('qwen2.5-vl-3b-instruct-q8-0');
  expect(smoke).toContain('$SelectedModel.name');
});
