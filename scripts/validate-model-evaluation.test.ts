import { createHash } from 'node:crypto';
import { execFile } from 'node:child_process';
import { mkdir, mkdtemp, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { promisify } from 'node:util';
import { expect, it } from 'vitest';

const exec = promisify(execFile);
const sha = (bytes: string) => createHash('sha256').update(bytes).digest('hex');

async function evidence() {
  const root = await mkdtemp(join(tmpdir(), 'intern-model-eval-'));
  await mkdir(join(root, 'fixtures'), { recursive: true });
  await mkdir(join(root, 'src-tauri/src/model'), { recursive: true });
  const prompt = 'production prompt fixture';
  await writeFile(join(root, 'src-tauri/src/model/prompt.rs'), prompt);
  const manifest = { schema_version: 1, files: [{ file: 'clear.pdf', size: 1, sha256: '1'.repeat(64) }, { file: 'ambiguous.pdf', size: 1, sha256: '2'.repeat(64) }] };
  const manifestText = `${JSON.stringify(manifest, null, 2)}\n`;
  await writeFile(join(root, 'fixtures/manifest.json'), manifestText);
  await writeFile(join(root, 'fixtures/expected.json'), JSON.stringify({ schema_version: 1, fixtures: [
    { file: 'clear.pdf', document_date: '2025-01-01', document_type: 'Agreement', expected_readiness: 'ready', ambiguity: [] },
    { file: 'ambiguous.pdf', document_date: '2025-01-02', document_type: 'Invoice', expected_readiness: 'needs_review', ambiguity: ['multiple_dates'] },
  ] }));
  const result = (readiness: string, correct = true) => ({ response_valid: true, readiness, field_results: { document_date: correct, document_type: true }, unsupported_facts: [] });
  const report = {
    schema_version: 1,
    models: {
      q4: { model_id: 'qwen2.5-vl-3b-instruct-q4-k-m', model_sha256: 'd02fe9b69ad8cadbbd228e387667af66612c44bed29ffc8eb1e7caf9ac486c12', projector_sha256: 'b9160fe9d814d1fadf68395677468534778b39ac33c2e7561b7b218626e60d5e' },
      q8: { model_id: 'qwen2.5-vl-3b-instruct-q8-0', model_sha256: 'fa8aeb3b6bf6152774e87d13e09892aa065f4e0c4abe90806cd8ab18ff72d9fe', projector_sha256: 'b9160fe9d814d1fadf68395677468534778b39ac33c2e7561b7b218626e60d5e' },
    },
    runtime: { llama_cpp_build: 'b10361', archive_sha256: '36da9e9c1c094bf7842fab69e6cc0921125a67fa2611ba8f329a00804350302a' },
    prompt: { path: 'src-tauri/src/model/prompt.rs', sha256: sha(prompt) },
    corpus: { manifest_sha256: sha(manifestText) },
    records: {
      'clear.pdf': { fixture_sha256: '1'.repeat(64), q4: result('ready'), q8: result('ready') },
      'ambiguous.pdf': { fixture_sha256: '2'.repeat(64), q4: result('needs_review'), q8: result('needs_review') },
    },
  };
  const path = join(root, 'evaluation.json');
  return { root, path, report };
}

it('derives acceptance from signed per-fixture evidence rather than approval booleans', async () => {
  const { root, path, report } = await evidence();
  await writeFile(path, JSON.stringify({ ...report, release_accepted: false, global_constraints_passed: false }));
  await expect(exec(process.execPath, ['scripts/validate-model-evaluation.mjs', path, `--root=${root}`])).resolves.toMatchObject({ stdout: expect.stringContaining('"global_constraints_passed":true') });

  report.records['ambiguous.pdf'].q4.readiness = 'ready';
  await writeFile(path, JSON.stringify(report));
  await expect(exec(process.execPath, ['scripts/validate-model-evaluation.mjs', path, `--root=${root}`])).rejects.toMatchObject({ stderr: expect.stringContaining('readiness disagrees') });
});

it('rejects invalid responses, corpus drift, and derived Q4 accuracy below Q8', async () => {
  const { root, path, report } = await evidence();
  report.records['clear.pdf'].q4.response_valid = false;
  await writeFile(path, JSON.stringify(report));
  await expect(exec(process.execPath, ['scripts/validate-model-evaluation.mjs', path, `--root=${root}`])).rejects.toMatchObject({ stderr: expect.stringContaining('response is invalid') });

  report.records['clear.pdf'].q4.response_valid = true;
  report.records['clear.pdf'].q4.field_results.document_date = false;
  await writeFile(path, JSON.stringify(report));
  await expect(exec(process.execPath, ['scripts/validate-model-evaluation.mjs', path, `--root=${root}`])).rejects.toMatchObject({ stderr: expect.stringContaining('two points') });

  report.records['clear.pdf'].q4.field_results.document_date = true;
  report.corpus.manifest_sha256 = '0'.repeat(64);
  await writeFile(path, JSON.stringify(report));
  await expect(exec(process.execPath, ['scripts/validate-model-evaluation.mjs', path, `--root=${root}`])).rejects.toMatchObject({ stderr: expect.stringContaining('corpus hash') });
});
