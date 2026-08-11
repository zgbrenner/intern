import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { isAbsolute, relative, resolve } from 'node:path';

const evaluationPath = resolve(process.argv[2] ?? 'docs/qa/model-evaluation.json');
const rootArgument = process.argv.find((argument) => argument.startsWith('--root='));
const repositoryRoot = resolve(rootArgument?.slice('--root='.length) ?? '.');
const report = JSON.parse(await readFile(evaluationPath, 'utf8'));
function requireValue(condition, message) { if (!condition) throw new Error(message); }
function digest(bytes) { return createHash('sha256').update(bytes).digest('hex'); }
function safeRepositoryPath(path) {
  requireValue(typeof path === 'string' && path.length > 0 && !isAbsolute(path) && !path.includes('\\') && !path.includes(':') && !path.split('/').includes('..'), `unsafe evidence path: ${path}`);
  const absolute = resolve(repositoryRoot, path);
  requireValue(!relative(repositoryRoot, absolute).startsWith('..'), `evidence path escapes repository: ${path}`);
  return absolute;
}

const exactModels = {
  q4: {
    model_id: 'qwen2.5-vl-3b-instruct-q4-k-m',
    model_sha256: 'd02fe9b69ad8cadbbd228e387667af66612c44bed29ffc8eb1e7caf9ac486c12',
    projector_sha256: 'b9160fe9d814d1fadf68395677468534778b39ac33c2e7561b7b218626e60d5e',
  },
  q8: {
    model_id: 'qwen2.5-vl-3b-instruct-q8-0',
    model_sha256: 'fa8aeb3b6bf6152774e87d13e09892aa065f4e0c4abe90806cd8ab18ff72d9fe',
    projector_sha256: 'b9160fe9d814d1fadf68395677468534778b39ac33c2e7561b7b218626e60d5e',
  },
};

requireValue(report.schema_version === 1, 'model evaluation schema_version must be 1');
for (const [variant, expected] of Object.entries(exactModels)) {
  for (const [field, value] of Object.entries(expected)) requireValue(report.models?.[variant]?.[field] === value, `${variant} ${field} does not match the approved pin`);
}
requireValue(report.runtime?.llama_cpp_build === 'b10361', 'model evaluation must use pinned llama.cpp b10361');
requireValue(report.runtime?.archive_sha256 === '36da9e9c1c094bf7842fab69e6cc0921125a67fa2611ba8f329a00804350302a', 'model evaluation runtime archive hash changed');

requireValue(report.prompt?.path === 'src-tauri/src/model/prompt.rs', 'evaluation must identify the production prompt source');
const promptBytes = await readFile(safeRepositoryPath(report.prompt.path));
requireValue(report.prompt.sha256 === digest(promptBytes), 'recorded production prompt hash does not match source');

const manifestBytes = await readFile(resolve(repositoryRoot, 'fixtures/manifest.json'));
requireValue(report.corpus?.manifest_sha256 === digest(manifestBytes), 'recorded corpus hash does not match fixtures/manifest.json');
const manifest = JSON.parse(manifestBytes);
const expected = JSON.parse(await readFile(resolve(repositoryRoot, 'fixtures/expected.json'), 'utf8'));
const manifestByFile = new Map(manifest.files.map((file) => [file.file, file]));
const expectedFiles = expected.fixtures.map((fixture) => fixture.file).sort();
requireValue(JSON.stringify(Object.keys(report.records ?? {}).sort()) === JSON.stringify(expectedFiles), 'evaluation records must exactly cover every gold fixture');

const metrics = { q4: { correct: 0, total: 0, valid: 0, unsupportedReadyDates: 0, unsupportedReadyParties: 0 }, q8: { correct: 0, total: 0, valid: 0, unsupportedReadyDates: 0, unsupportedReadyParties: 0 } };
for (const fixture of expected.fixtures) {
  const record = report.records[fixture.file];
  const signed = manifestByFile.get(fixture.file);
  const acceptedFields = ['document_date', 'document_type', 'subject', 'parties'].filter((field) => Object.hasOwn(fixture, field));
  if (fixture.expected_error) acceptedFields.push('expected_error');
  requireValue(signed && record.fixture_sha256 === signed.sha256, `${fixture.file} is not keyed to its signed corpus digest`);
  for (const variant of ['q4', 'q8']) {
    const result = record[variant];
    requireValue(result?.response_valid === true, `${variant} response is invalid for ${fixture.file}`);
    metrics[variant].valid += 1;
    requireValue(result.readiness === fixture.expected_readiness, `${variant} readiness disagrees with gold for ${fixture.file}`);
    if (fixture.ambiguity.length > 0 && fixture.expected_readiness !== 'failed') {
      requireValue(result.readiness === 'needs_review', `${variant} must route ambiguous fixture ${fixture.file} to Needs Review`);
    }
    requireValue(result.field_results && !Array.isArray(result.field_results), `${variant} field results are missing for ${fixture.file}`);
    requireValue(JSON.stringify(Object.keys(result.field_results).sort()) === JSON.stringify(acceptedFields.sort()), `${variant} field results must exactly cover gold accepted fields for ${fixture.file}`);
    for (const [field, correct] of Object.entries(result.field_results)) {
      requireValue(typeof correct === 'boolean', `${variant} field result ${field} is not boolean for ${fixture.file}`);
      metrics[variant].total += 1;
      if (correct) metrics[variant].correct += 1;
    }
    requireValue(Array.isArray(result.unsupported_facts), `${variant} unsupported facts must be an array for ${fixture.file}`);
    for (const unsupported of result.unsupported_facts) {
      requireValue(typeof unsupported?.field === 'string' && typeof unsupported?.value === 'string', `${variant} unsupported fact evidence is malformed for ${fixture.file}`);
      if (result.readiness === 'ready') {
        if (/date/i.test(unsupported.field)) metrics[variant].unsupportedReadyDates += 1;
        if (/part(y|ies)/i.test(unsupported.field)) metrics[variant].unsupportedReadyParties += 1;
      }
    }
  }
}

const q4Accuracy = metrics.q4.correct / metrics.q4.total;
const q8Accuracy = metrics.q8.correct / metrics.q8.total;
requireValue(metrics.q4.valid === expectedFiles.length && metrics.q8.valid === expectedFiles.length, 'response validity must be 100% for Q4 and Q8');
requireValue(metrics.q4.unsupportedReadyDates === 0 && metrics.q4.unsupportedReadyParties === 0, 'Q4 Ready results contain unsupported dates or parties');
requireValue(q4Accuracy >= q8Accuracy - 0.02, 'derived Q4 field accuracy is more than two points below Q8');

process.stdout.write(`${JSON.stringify({ global_constraints_passed: true, corpus_records: expectedFiles.length, q4_response_validity: 1, q8_response_validity: 1, q4_field_accuracy: q4Accuracy, q8_field_accuracy: q8Accuracy, q4_unsupported_ready_dates: 0, q4_unsupported_ready_parties: 0 })}\n`);
