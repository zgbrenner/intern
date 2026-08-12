import { readFile } from 'node:fs/promises';
import { expect, it } from 'vitest';

it('scores the corpus with the exact model the app installs, text-only', async () => {
  const [script, manifest] = await Promise.all([
    readFile('scripts/run-model-evaluation.ps1', 'utf8'),
    readFile('src-tauri/resources/model-manifest.json', 'utf8').then(JSON.parse),
  ]);
  // The evaluation must never pin a model of its own; it reads the manifest the
  // application ships, so evidence always describes what users actually run.
  expect(script).toContain('src-tauri/resources/model-manifest.json');
  expect(script).toContain('$_.role -eq "model"');
  expect(script).toContain('$Spec.sha256');
  expect(script).toContain('intern-evaluate.exe');
  expect(script).not.toContain('.gguf"');
  for (const argument of ['--host', '--api-key', '--parallel', '--ctx-size', '--n-gpu-layers', '--no-mmproj']) {
    expect(script).toContain(`"${argument}"`);
  }
  expect(script).toContain('WorkingSet64');
  expect(script).toContain('docs/qa/model-evaluation.json');
  expect(manifest.files.some((file: { role: string }) => file.role === 'model')).toBe(true);
});

it('the evaluator drives the shipping extraction, distillation, and validation path', async () => {
  const source = await readFile('crates/intern-engine/src/bin/intern-evaluate.rs', 'utf8');
  expect(source).toContain('SupervisedWorker');
  expect(source).toContain('Engine::new');
  expect(source).toContain('analyze_digest');
  // Scoring must compare against the reviewed corpus, including the traps.
  expect(source).toContain('forbidden_dates');
  expect(source).toContain('forbidden_parties');
  expect(source).toContain('acceptable_dates');
  // And it must be able to run the superseded pipeline for comparison.
  expect(source).toContain('legacy_digest');
});

it('the release gate only accepts evidence from the shipping pipeline', async () => {
  const [validator, release] = await Promise.all([
    readFile('scripts/validate-model-evaluation.mjs', 'utf8'),
    readFile('.github/workflows/release.yml', 'utf8'),
  ]);
  expect(validator).toContain("report.pipeline === 'new'");
  expect(validator).toContain('date_forbidden');
  expect(release).toContain('intern-evaluate');
  expect(release).toContain('validate-model-evaluation.mjs');
  expect(release.indexOf('run-model-evaluation.ps1')).toBeLessThan(release.indexOf('validate-model-evaluation.mjs'));
});
