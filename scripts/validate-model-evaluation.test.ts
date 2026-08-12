import { createHash } from 'node:crypto';
import { execFile } from 'node:child_process';
import { mkdir, mkdtemp, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { promisify } from 'node:util';
import { expect, it } from 'vitest';

const exec = promisify(execFile);
const sha = (bytes: string) => createHash('sha256').update(bytes).digest('hex');

function proposal(date: string, type: string) {
  return {
    document_date: date,
    date_kind: 'issued',
    document_type: type,
    filename_subject: null,
    parties: [],
    description: `${type} issued on ${date}.`,
    confidence: 0.92,
    needs_review: false,
    review_reasons: [],
    date_evidence: date,
    type_evidence: type,
    subject_evidence: null,
    party_evidence: [],
  };
}

function completedResult(readiness: string, date: string, type: string) {
  const raw = proposal(date, type);
  const validated = {
    document_date: raw.document_date,
    date_kind: raw.date_kind,
    document_type: raw.document_type,
    filename_subject: raw.filename_subject,
    parties: raw.parties,
    description: raw.description,
    confidence: raw.confidence,
    date_evidence: raw.date_evidence,
    type_evidence: raw.type_evidence,
    subject_evidence: raw.subject_evidence,
    party_evidence: raw.party_evidence,
  };
  const proposalSha256 = sha(JSON.stringify(raw));
  return {
    status: 'completed',
    model_invoked: true,
    response_valid: true,
    parser_error: null,
    model_error: null,
    readiness,
    input_packet_sha256: 'a'.repeat(64),
    proposal_sha256: proposalSha256,
    validation_sha256: sha(JSON.stringify({ input_packet_sha256: 'a'.repeat(64), proposal_sha256: proposalSha256, validated_proposal: validated, readiness })),
    proposal: raw,
    validated_proposal: validated,
    field_results: { document_date: true, document_type: true, description: true },
    unsupported_facts: [],
    timings_ms: { extraction: 10, inference: 20, total: 30 },
    peak_rss_bytes: 1024,
  };
}

function pendingResult() {
  return {
    status: 'pending',
    model_invoked: null,
    response_valid: null,
    parser_error: null,
    model_error: null,
    readiness: null,
    input_packet_sha256: null,
    proposal_sha256: null,
    validation_sha256: null,
    proposal: null,
    validated_proposal: null,
    field_results: null,
    unsupported_facts: [],
    timings_ms: { extraction: null, inference: null, total: null },
    peak_rss_bytes: null,
  };
}

async function evidence(status: 'completed' | 'pending' = 'completed') {
  const root = await mkdtemp(join(tmpdir(), 'intern-model-eval-'));
  await mkdir(join(root, 'fixtures'), { recursive: true });
  await mkdir(join(root, 'src-tauri/src/model'), { recursive: true });
  await mkdir(join(root, 'src-tauri/resources'), { recursive: true });
  const prompt = 'production prompt fixture';
  await writeFile(join(root, 'src-tauri/src/model/prompt.rs'), prompt);
  await writeFile(join(root, 'src-tauri/resources/model-manifest.json'), JSON.stringify({
    schema_version: 1,
    model_id: 'qwen2.5-vl-3b-instruct-q4-k-m',
    files: [
      { name: 'Qwen2.5-VL-3B-Instruct-Q4_K_M.gguf', url: 'https://huggingface.co/ggml-org/Qwen2.5-VL-3B-Instruct-GGUF/resolve/main/Qwen2.5-VL-3B-Instruct-Q4_K_M.gguf', size: 1929901056, sha256: 'd02fe9b69ad8cadbbd228e387667af66612c44bed29ffc8eb1e7caf9ac486c12' },
      { name: 'mmproj-Qwen2.5-VL-3B-Instruct-f16.gguf', url: 'https://huggingface.co/ggml-org/Qwen2.5-VL-3B-Instruct-GGUF/resolve/main/mmproj-Qwen2.5-VL-3B-Instruct-f16.gguf', size: 1338428128, sha256: 'b9160fe9d814d1fadf68395677468534778b39ac33c2e7561b7b218626e60d5e' },
    ],
  }));
  const manifest = { schema_version: 1, files: [{ file: 'clear.pdf', size: 1, sha256: '1'.repeat(64) }, { file: 'ambiguous.pdf', size: 1, sha256: '2'.repeat(64) }] };
  const manifestText = `${JSON.stringify(manifest, null, 2)}\n`;
  const expected = { schema_version: 1, fixtures: [
    { file: 'clear.pdf', document_date: '2025-01-01', document_type: 'Agreement', expected_readiness: 'ready', ambiguity: [], acceptable_description_facts: ['Agreement', '2025-01-01'] },
    { file: 'ambiguous.pdf', document_date: '2025-01-02', document_type: 'Invoice', expected_readiness: 'needs_review', ambiguity: ['multiple_dates'], acceptable_description_facts: ['Invoice', '2025-01-02'] },
  ] };
  const expectedText = JSON.stringify(expected);
  await writeFile(join(root, 'fixtures/manifest.json'), manifestText);
  await writeFile(join(root, 'fixtures/expected.json'), expectedText);
  await writeFile(join(root, 'source.txt'), 'evaluated source');
  await exec('git', ['init'], { cwd: root });
  await exec('git', ['config', 'user.email', 'qa@example.invalid'], { cwd: root });
  await exec('git', ['config', 'user.name', 'QA Test'], { cwd: root });
  await exec('git', ['add', '.'], { cwd: root });
  await exec('git', ['commit', '-m', 'fixture source'], { cwd: root });
  const commit = (await exec('git', ['rev-parse', 'HEAD'], { cwd: root })).stdout.trim();
  const releaseInputsSha256 = (await exec(process.execPath, ['scripts/hash-release-inputs.mjs', `--root=${root}`])).stdout.trim();
  const result = status === 'completed' ? completedResult : pendingResult;
  const report = {
    schema_version: 2,
    status,
    selected_model: status === 'completed' ? 'q4' : null,
    generated_at: status === 'completed' ? '2026-08-11T12:00:00.000Z' : null,
    commit: status === 'completed' ? commit : null,
    release_inputs_sha256: status === 'completed' ? releaseInputsSha256 : null,
    runner: status === 'completed' ? { os: 'Windows', arch: 'X64', ci_run_id: '1234' } : null,
    models: {
      q4: { model_id: 'qwen2.5-vl-3b-instruct-q4-k-m', filename: 'Qwen2.5-VL-3B-Instruct-Q4_K_M.gguf', size: 1929901056, model_sha256: 'd02fe9b69ad8cadbbd228e387667af66612c44bed29ffc8eb1e7caf9ac486c12', projector_sha256: 'b9160fe9d814d1fadf68395677468534778b39ac33c2e7561b7b218626e60d5e' },
      q8: { model_id: 'qwen2.5-vl-3b-instruct-q8-0', filename: 'Qwen2.5-VL-3B-Instruct-Q8_0.gguf', size: 3285474304, model_sha256: 'fa8aeb3b6bf6152774e87d13e09892aa065f4e0c4abe90806cd8ab18ff72d9fe', projector_sha256: 'b9160fe9d814d1fadf68395677468534778b39ac33c2e7561b7b218626e60d5e' },
    },
    runtime: { llama_cpp_build: 'b10361', archive_sha256: '36da9e9c1c094bf7842fab69e6cc0921125a67fa2611ba8f329a00804350302a' },
    prompt: { path: 'src-tauri/src/model/prompt.rs', sha256: sha(prompt) },
    corpus: { manifest_path: 'fixtures/manifest.json', manifest_sha256: sha(manifestText), expected_path: 'fixtures/expected.json', expected_sha256: sha(expectedText) },
    records: {
      'clear.pdf': { fixture_sha256: '1'.repeat(64), q4: result('ready', '2025-01-01', 'Agreement'), q8: result('ready', '2025-01-01', 'Agreement') },
      'ambiguous.pdf': { fixture_sha256: '2'.repeat(64), q4: result('needs_review', '2025-01-02', 'Invoice'), q8: result('needs_review', '2025-01-02', 'Invoice') },
    },
    summary: status === 'completed' ? {
      eligible_fixtures: 2,
      q4_response_validity: 1,
      q8_response_validity: 1,
      q4_field_accuracy: 1,
      q8_field_accuracy: 1,
      q4_unsupported_ready_dates: 0,
      q4_unsupported_ready_parties: 0,
      q8_unsupported_ready_dates: 0,
      q8_unsupported_ready_parties: 0,
      ambiguous_fixtures_in_review: 2,
      ambiguous_fixtures_total: 2,
      q4_readiness_accuracy: 1,
      q8_readiness_accuracy: 1,
    } : null,
    acceptance: status === 'completed' ? { status: 'accepted', reasons: [] } : { status: 'pending', reasons: ['Q4 and Q8 production evaluation has not run.'] },
  };
  const path = join(root, 'evaluation.json');
  return { root, path, report };
}

it('accepts only complete signed per-fixture production evidence', async () => {
  const { root, path, report } = await evidence();
  await writeFile(path, JSON.stringify(report));
  await expect(exec(process.execPath, ['scripts/validate-model-evaluation.mjs', path, `--root=${root}`])).resolves.toMatchObject({ stdout: expect.stringContaining('"global_constraints_passed":true') });

  report.records['clear.pdf'].q4.field_results.document_date = false;
  await writeFile(path, JSON.stringify(report));
  await expect(exec(process.execPath, ['scripts/validate-model-evaluation.mjs', path, `--root=${root}`])).rejects.toMatchObject({ stderr: expect.stringContaining('field result') });
});

it('rejects completed evidence after a release input changes', async () => {
  const { root, path, report } = await evidence();
  await writeFile(path, JSON.stringify(report));
  await writeFile(join(root, 'source.txt'), 'changed after evaluation');
  await expect(exec(process.execPath, ['scripts/validate-model-evaluation.mjs', path, `--root=${root}`])).rejects.toMatchObject({ stderr: expect.stringContaining('release inputs') });
});

it('requires every accepted description fact for description field credit', async () => {
  const { root, path, report } = await evidence();
  const q4 = report.records['clear.pdf'].q4;
  q4.proposal.description = 'Agreement with fabricated context.';
  q4.validated_proposal.description = q4.proposal.description;
  q4.proposal_sha256 = sha(JSON.stringify(q4.proposal));
  q4.validation_sha256 = sha(JSON.stringify({ input_packet_sha256: q4.input_packet_sha256, proposal_sha256: q4.proposal_sha256, validated_proposal: q4.validated_proposal, readiness: q4.readiness }));
  await writeFile(path, JSON.stringify(report));
  await expect(exec(process.execPath, ['scripts/validate-model-evaluation.mjs', path, `--root=${root}`])).rejects.toMatchObject({ stderr: expect.stringContaining('field result description') });
});

it('accepts the pinned Q8 manifest when Q4 misses the readiness and accuracy gate', async () => {
  const { root, path, report } = await evidence();
  const q4 = report.records['clear.pdf'].q4;
  q4.proposal.document_date = '2024-12-31';
  q4.proposal.date_evidence = '2024-12-31';
  q4.validated_proposal.document_date = null;
  q4.validated_proposal.date_kind = null;
  q4.validated_proposal.date_evidence = '2024-12-31';
  q4.field_results.document_date = false;
  q4.readiness = 'needs_review';
  q4.unsupported_facts = [{ field: 'document_date', value: '2024-12-31' }];
  q4.proposal_sha256 = sha(JSON.stringify(q4.proposal));
  q4.validation_sha256 = sha(JSON.stringify({ input_packet_sha256: q4.input_packet_sha256, proposal_sha256: q4.proposal_sha256, validated_proposal: q4.validated_proposal, readiness: q4.readiness }));
  report.selected_model = 'q8';
  report.summary.q4_field_accuracy = 5 / 6;
  report.summary.q4_readiness_accuracy = 0.5;
  await writeFile(join(root, 'src-tauri/resources/model-manifest.json'), JSON.stringify({
    schema_version: 1,
    model_id: 'qwen2.5-vl-3b-instruct-q8-0',
    files: [
      { name: 'Qwen2.5-VL-3B-Instruct-Q8_0.gguf', url: 'https://huggingface.co/ggml-org/Qwen2.5-VL-3B-Instruct-GGUF/resolve/main/Qwen2.5-VL-3B-Instruct-Q8_0.gguf', size: 3285474304, sha256: 'fa8aeb3b6bf6152774e87d13e09892aa065f4e0c4abe90806cd8ab18ff72d9fe' },
      { name: 'mmproj-Qwen2.5-VL-3B-Instruct-f16.gguf', url: 'https://huggingface.co/ggml-org/Qwen2.5-VL-3B-Instruct-GGUF/resolve/main/mmproj-Qwen2.5-VL-3B-Instruct-f16.gguf', size: 1338428128, sha256: 'b9160fe9d814d1fadf68395677468534778b39ac33c2e7561b7b218626e60d5e' },
    ],
  }));
  await exec('git', ['add', '.'], { cwd: root });
  await exec('git', ['commit', '-m', 'select q8'], { cwd: root });
  report.commit = (await exec('git', ['rev-parse', 'HEAD'], { cwd: root })).stdout.trim();
  report.release_inputs_sha256 = (await exec(process.execPath, ['scripts/hash-release-inputs.mjs', `--root=${root}`])).stdout.trim();
  await writeFile(path, JSON.stringify(report));
  await expect(exec(process.execPath, ['scripts/validate-model-evaluation.mjs', path, `--root=${root}`])).resolves.toMatchObject({ stdout: expect.stringContaining('"global_constraints_passed":true') });
}, 15_000);

it('recognizes pending evidence for QA but blocks release validation', async () => {
  const { root, path, report } = await evidence('pending');
  await writeFile(path, JSON.stringify(report));
  await expect(exec(process.execPath, ['scripts/validate-model-evaluation.mjs', '--allow-pending', path, `--root=${root}`])).resolves.toMatchObject({ stdout: expect.stringContaining('"status":"pending"') });
  await expect(exec(process.execPath, ['scripts/validate-model-evaluation.mjs', path, `--root=${root}`])).rejects.toMatchObject({ stderr: expect.stringContaining('pending') });
});

it('rejects invalid responses, corpus drift, incomplete timing, and unknown schema fields', async () => {
  const { root, path, report } = await evidence();
  report.records['clear.pdf'].q4.response_valid = false;
  await writeFile(path, JSON.stringify(report));
  await expect(exec(process.execPath, ['scripts/validate-model-evaluation.mjs', path, `--root=${root}`])).rejects.toMatchObject({ stderr: expect.stringContaining('response') });

  report.records['clear.pdf'].q4.response_valid = true;
  report.corpus.manifest_sha256 = '0'.repeat(64);
  await writeFile(path, JSON.stringify(report));
  await expect(exec(process.execPath, ['scripts/validate-model-evaluation.mjs', path, `--root=${root}`])).rejects.toMatchObject({ stderr: expect.stringContaining('corpus') });

  report.corpus.manifest_sha256 = sha(`${JSON.stringify({ schema_version: 1, files: [{ file: 'clear.pdf', size: 1, sha256: '1'.repeat(64) }, { file: 'ambiguous.pdf', size: 1, sha256: '2'.repeat(64) }] }, null, 2)}\n`);
  report.records['clear.pdf'].q4.timings_ms.inference = null;
  await writeFile(path, JSON.stringify(report));
  await expect(exec(process.execPath, ['scripts/validate-model-evaluation.mjs', path, `--root=${root}`])).rejects.toMatchObject({ stderr: expect.stringContaining('timing') });

  report.records['clear.pdf'].q4.timings_ms.inference = 20;
  (report.records['clear.pdf'].q4 as typeof report.records['clear.pdf']['q4'] & { invented?: boolean }).invented = true;
  await writeFile(path, JSON.stringify(report));
  await expect(exec(process.execPath, ['scripts/validate-model-evaluation.mjs', path, `--root=${root}`])).rejects.toMatchObject({ stderr: expect.stringContaining('unexpected field') });
});
