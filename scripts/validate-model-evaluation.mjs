import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { isAbsolute, relative, resolve } from 'node:path';
import { releaseInputsDigest, requireAncestor } from './hash-release-inputs.mjs';

const arguments_ = process.argv.slice(2);
const rootArgument = arguments_.find((argument) => argument.startsWith('--root='));
const repositoryRoot = resolve(rootArgument?.slice('--root='.length) ?? '.');
const allowPending = arguments_.includes('--allow-pending');
const evaluationArgument = arguments_.find((argument) => !argument.startsWith('--'));
const evaluationPath = resolve(evaluationArgument ?? 'docs/qa/model-evaluation.json');
const report = JSON.parse(await readFile(evaluationPath, 'utf8'));

function requireValue(condition, message) {
  if (!condition) throw new Error(message);
}

function exactKeys(value, expected, label) {
  requireValue(value && typeof value === 'object' && !Array.isArray(value), `${label} must be an object`);
  const actual = Object.keys(value).sort();
  const wanted = [...expected].sort();
  requireValue(JSON.stringify(actual) === JSON.stringify(wanted), `${label} has an unexpected field or omits a required field`);
}

function digest(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

function isSha256(value) {
  return typeof value === 'string' && /^[a-f0-9]{64}$/.test(value);
}

function safeRepositoryPath(path) {
  requireValue(typeof path === 'string' && path.length > 0 && !isAbsolute(path) && !path.includes('\\') && !path.includes(':') && !path.split('/').includes('..'), `unsafe evidence path: ${path}`);
  const absolute = resolve(repositoryRoot, path);
  requireValue(!relative(repositoryRoot, absolute).startsWith('..'), `evidence path escapes repository: ${path}`);
  return absolute;
}

function sameValue(left, right) {
  if (Array.isArray(left) && Array.isArray(right)) {
    return JSON.stringify([...left].sort()) === JSON.stringify([...right].sort());
  }
  return left === right;
}

function validatedDerivesFromRaw(raw, validated) {
  const retained = (value, candidate) => value === null || value === candidate;
  const retainedParties = validated.parties.every((party) => raw.parties.includes(party));
  const retainedDescription = raw.description.trim().startsWith(validated.description);
  return retained(validated.document_date, raw.document_date)
    && retained(validated.date_kind, raw.date_kind)
    && retained(validated.document_type, raw.document_type)
    && retained(validated.filename_subject, raw.filename_subject)
    && retainedParties
    && retainedDescription
    && validated.confidence === raw.confidence
    && validated.date_evidence === raw.date_evidence
    && validated.type_evidence === raw.type_evidence
    && validated.subject_evidence === raw.subject_evidence
    && sameValue(validated.party_evidence, raw.party_evidence);
}

const resultKeys = [
  'status', 'model_invoked', 'response_valid', 'parser_error', 'model_error',
  'readiness', 'input_packet_sha256', 'proposal_sha256', 'proposal',
  'validation_sha256', 'validated_proposal', 'field_results', 'unsupported_facts', 'timings_ms',
  'peak_rss_bytes',
];
const proposalKeys = [
  'document_date', 'date_kind', 'document_type', 'filename_subject', 'parties',
  'description', 'confidence', 'needs_review', 'review_reasons', 'date_evidence',
  'type_evidence', 'subject_evidence', 'party_evidence',
];
const validatedProposalKeys = proposalKeys.filter((key) => !['needs_review', 'review_reasons'].includes(key));
const summaryKeys = [
  'eligible_fixtures', 'q4_response_validity', 'q8_response_validity',
  'q4_field_accuracy', 'q8_field_accuracy', 'q4_unsupported_ready_dates',
  'q4_unsupported_ready_parties', 'q8_unsupported_ready_dates',
  'q8_unsupported_ready_parties', 'ambiguous_fixtures_in_review',
  'ambiguous_fixtures_total', 'q4_readiness_accuracy', 'q8_readiness_accuracy',
];

const exactModels = {
  q4: {
    model_id: 'qwen2.5-vl-3b-instruct-q4-k-m',
    filename: 'Qwen2.5-VL-3B-Instruct-Q4_K_M.gguf',
    size: 1_929_901_056,
    model_sha256: 'd02fe9b69ad8cadbbd228e387667af66612c44bed29ffc8eb1e7caf9ac486c12',
    projector_sha256: 'b9160fe9d814d1fadf68395677468534778b39ac33c2e7561b7b218626e60d5e',
  },
  q8: {
    model_id: 'qwen2.5-vl-3b-instruct-q8-0',
    filename: 'Qwen2.5-VL-3B-Instruct-Q8_0.gguf',
    size: 3_285_474_304,
    model_sha256: 'fa8aeb3b6bf6152774e87d13e09892aa065f4e0c4abe90806cd8ab18ff72d9fe',
    projector_sha256: 'b9160fe9d814d1fadf68395677468534778b39ac33c2e7561b7b218626e60d5e',
  },
};

exactKeys(report, ['schema_version', 'status', 'selected_model', 'generated_at', 'commit', 'release_inputs_sha256', 'runner', 'models', 'runtime', 'prompt', 'corpus', 'records', 'summary', 'acceptance'], 'model evaluation');
requireValue(report.schema_version === 2, 'model evaluation schema_version must be 2');
requireValue(['pending', 'completed', 'failed'].includes(report.status), 'model evaluation status is invalid');

exactKeys(report.models, ['q4', 'q8'], 'models');
for (const [variant, expectedModel] of Object.entries(exactModels)) {
  exactKeys(report.models[variant], Object.keys(expectedModel), `${variant} model`);
  for (const [field, value] of Object.entries(expectedModel)) {
    requireValue(report.models[variant][field] === value, `${variant} ${field} does not match the approved pin`);
  }
}
exactKeys(report.runtime, ['llama_cpp_build', 'archive_sha256'], 'runtime');
requireValue(report.runtime.llama_cpp_build === 'b10361', 'model evaluation must use pinned llama.cpp b10361');
requireValue(report.runtime.archive_sha256 === '36da9e9c1c094bf7842fab69e6cc0921125a67fa2611ba8f329a00804350302a', 'model evaluation runtime archive hash changed');

exactKeys(report.prompt, ['path', 'sha256'], 'prompt');
requireValue(report.prompt.path === 'src-tauri/src/model/prompt.rs', 'evaluation must identify the production prompt source');
const promptBytes = await readFile(safeRepositoryPath(report.prompt.path));
requireValue(report.prompt.sha256 === digest(promptBytes), 'recorded production prompt hash does not match source');

exactKeys(report.corpus, ['manifest_path', 'manifest_sha256', 'expected_path', 'expected_sha256'], 'corpus');
requireValue(report.corpus.manifest_path === 'fixtures/manifest.json', 'evaluation must identify fixtures/manifest.json');
requireValue(report.corpus.expected_path === 'fixtures/expected.json', 'evaluation must identify fixtures/expected.json');
const manifestBytes = await readFile(safeRepositoryPath(report.corpus.manifest_path));
const expectedBytes = await readFile(safeRepositoryPath(report.corpus.expected_path));
requireValue(report.corpus.manifest_sha256 === digest(manifestBytes), 'recorded corpus manifest hash does not match fixtures/manifest.json');
requireValue(report.corpus.expected_sha256 === digest(expectedBytes), 'recorded corpus gold hash does not match fixtures/expected.json');
const manifest = JSON.parse(manifestBytes);
const expected = JSON.parse(expectedBytes);
const manifestByFile = new Map(manifest.files.map((file) => [file.file, file]));
const expectedFiles = expected.fixtures.map((fixture) => fixture.file).sort();
requireValue(JSON.stringify(Object.keys(report.records ?? {}).sort()) === JSON.stringify(expectedFiles), 'evaluation records must exactly cover every gold fixture');

exactKeys(report.acceptance, ['status', 'reasons'], 'acceptance');
requireValue(Array.isArray(report.acceptance.reasons) && report.acceptance.reasons.every((reason) => typeof reason === 'string' && reason.length > 0), 'acceptance reasons must be non-empty strings when present');

for (const fixture of expected.fixtures) {
  const record = report.records[fixture.file];
  exactKeys(record, ['fixture_sha256', 'q4', 'q8'], `${fixture.file} record`);
  const signed = manifestByFile.get(fixture.file);
  requireValue(signed && record.fixture_sha256 === signed.sha256, `${fixture.file} is not keyed to its signed corpus digest`);
  for (const variant of ['q4', 'q8']) {
    exactKeys(record[variant], resultKeys, `${variant} result for ${fixture.file}`);
    exactKeys(record[variant].timings_ms, ['extraction', 'inference', 'total'], `${variant} timing for ${fixture.file}`);
    requireValue(Array.isArray(record[variant].unsupported_facts), `${variant} unsupported facts must be an array for ${fixture.file}`);
  }
}

if (report.status !== 'completed') {
  requireValue(report.selected_model === null && report.generated_at === null && report.commit === null && report.release_inputs_sha256 === null && report.runner === null && report.summary === null, `${report.status} evidence must not claim execution metadata or summary results`);
  requireValue(report.acceptance.status === (report.status === 'pending' ? 'pending' : 'rejected'), `${report.status} evidence has an invalid acceptance status`);
  requireValue(report.acceptance.reasons.length > 0, `${report.status} evidence must explain why release is blocked`);
  for (const fixture of expected.fixtures) {
    for (const variant of ['q4', 'q8']) {
      const result = report.records[fixture.file][variant];
      requireValue(result.status === report.status || (report.status === 'failed' && result.status === 'pending'), `${variant} result status is inconsistent for ${fixture.file}`);
      requireValue(result.model_invoked === null && result.response_valid === null && result.parser_error === null && result.model_error === null && result.readiness === null, `${variant} ${report.status} result claims an execution outcome for ${fixture.file}`);
      requireValue(result.input_packet_sha256 === null && result.proposal_sha256 === null && result.validation_sha256 === null && result.proposal === null && result.validated_proposal === null && result.field_results === null, `${variant} ${report.status} result contains unexecuted proposal evidence for ${fixture.file}`);
      requireValue(result.unsupported_facts.length === 0 && Object.values(result.timings_ms).every((value) => value === null) && result.peak_rss_bytes === null, `${variant} ${report.status} result contains unexecuted metrics for ${fixture.file}`);
    }
  }
  if (!allowPending) throw new Error(`model evaluation is ${report.status}; release is blocked`);
  process.stdout.write(`${JSON.stringify({ status: report.status, release_blocked: true, corpus_records: expectedFiles.length })}\n`);
  process.exit(0);
}

requireValue(typeof report.generated_at === 'string' && Number.isFinite(Date.parse(report.generated_at)), 'completed evaluation generated_at is invalid');
requireValue(typeof report.commit === 'string' && /^[a-f0-9]{40}$/.test(report.commit), 'completed evaluation commit is invalid');
requireValue(isSha256(report.release_inputs_sha256), 'completed evaluation release input hash is invalid');
try {
  requireAncestor(report.commit, repositoryRoot);
} catch {
  throw new Error('evaluated commit is not an ancestor of the release source');
}
requireValue(report.release_inputs_sha256 === releaseInputsDigest(repositoryRoot), 'release inputs changed after model evaluation');
exactKeys(report.runner, ['os', 'arch', 'ci_run_id'], 'runner');
requireValue(/^windows$/i.test(report.runner.os), 'completed model evaluation must run on Windows');
requireValue(typeof report.runner.arch === 'string' && report.runner.arch.length > 0, 'completed model evaluation runner architecture is missing');
requireValue(typeof report.runner.ci_run_id === 'string' && report.runner.ci_run_id.length > 0, 'completed model evaluation CI run id is missing');
exactKeys(report.summary, summaryKeys, 'summary');

const metrics = {
  q4: { correct: 0, total: 0, valid: 0, readiness: 0, unsupportedReadyDates: 0, unsupportedReadyParties: 0 },
  q8: { correct: 0, total: 0, valid: 0, readiness: 0, unsupportedReadyDates: 0, unsupportedReadyParties: 0 },
};
let eligibleFixtures = 0;
let ambiguousResultsInReview = 0;
let ambiguousResultsTotal = 0;

for (const fixture of expected.fixtures) {
  const acceptedFields = ['document_date', 'document_type', 'subject', 'parties'].filter((field) => Object.hasOwn(fixture, field));
  if (Array.isArray(fixture.acceptable_description_facts)) acceptedFields.push('description');
  if (!fixture.expected_error) eligibleFixtures += 1;
  for (const variant of ['q4', 'q8']) {
    const result = report.records[fixture.file][variant];
    requireValue(result.status === 'completed', `${variant} evaluation did not complete for ${fixture.file}`);
    requireValue(Number.isSafeInteger(result.timings_ms.extraction) && result.timings_ms.extraction >= 0, `${variant} extraction timing is missing for ${fixture.file}`);
    requireValue(Number.isSafeInteger(result.timings_ms.total) && result.timings_ms.total >= result.timings_ms.extraction, `${variant} total timing is missing for ${fixture.file}`);
    requireValue(Number.isSafeInteger(result.peak_rss_bytes) && result.peak_rss_bytes > 0, `${variant} peak RSS is missing for ${fixture.file}`);

    if (fixture.expected_error) {
      requireValue(result.model_invoked === false && result.response_valid === null, `${variant} must not invoke the model after the expected parser error for ${fixture.file}`);
      requireValue(result.parser_error === fixture.expected_error && result.model_error === null, `${variant} parser error disagrees with gold for ${fixture.file}`);
      requireValue(result.readiness === 'failed', `${variant} readiness disagrees with parser failure gold for ${fixture.file}`);
      requireValue(result.input_packet_sha256 === null && result.proposal_sha256 === null && result.validation_sha256 === null && result.proposal === null && result.validated_proposal === null, `${variant} parser failure includes proposal evidence for ${fixture.file}`);
      requireValue(result.timings_ms.inference === null, `${variant} parser failure includes model timing for ${fixture.file}`);
      exactKeys(result.field_results, ['expected_error'], `${variant} field results for ${fixture.file}`);
      requireValue(result.field_results.expected_error === true && result.unsupported_facts.length === 0, `${variant} parser failure does not match gold for ${fixture.file}`);
      continue;
    }

    requireValue(result.model_invoked === true && result.response_valid === true, `${variant} response is invalid for ${fixture.file}`);
    requireValue(result.parser_error === null && result.model_error === null, `${variant} model path recorded an error for ${fixture.file}`);
    if (result.readiness === fixture.expected_readiness) metrics[variant].readiness += 1;
    if (variant === 'q8') requireValue(result.readiness === fixture.expected_readiness, `q8 readiness disagrees with gold for ${fixture.file}`);
    if (fixture.ambiguity.length > 0) {
      ambiguousResultsTotal += 1;
      if (result.readiness === 'needs_review') ambiguousResultsInReview += 1;
    }
    requireValue(isSha256(result.input_packet_sha256), `${variant} input packet hash is missing for ${fixture.file}`);
    requireValue(isSha256(result.proposal_sha256), `${variant} proposal hash is missing for ${fixture.file}`);
    requireValue(isSha256(result.validation_sha256), `${variant} validation hash is missing for ${fixture.file}`);
    exactKeys(result.proposal, proposalKeys, `${variant} proposal for ${fixture.file}`);
    exactKeys(result.validated_proposal, validatedProposalKeys, `${variant} validated proposal for ${fixture.file}`);
    requireValue(validatedDerivesFromRaw(result.proposal, result.validated_proposal), `${variant} validated proposal is not a removal-only production validation result for ${fixture.file}`);
    requireValue(result.proposal_sha256 === digest(JSON.stringify(result.proposal)), `${variant} proposal hash does not match the recorded proposal for ${fixture.file}`);
    requireValue(result.validation_sha256 === digest(JSON.stringify({ input_packet_sha256: result.input_packet_sha256, proposal_sha256: result.proposal_sha256, validated_proposal: result.validated_proposal, readiness: result.readiness })), `${variant} validation evidence is not bound to its input and raw proposal for ${fixture.file}`);
    requireValue(Number.isSafeInteger(result.timings_ms.inference) && result.timings_ms.inference >= 0, `${variant} inference timing is missing for ${fixture.file}`);

    exactKeys(result.field_results, acceptedFields, `${variant} field results for ${fixture.file}`);
    const actualFields = {
      document_date: result.validated_proposal.document_date,
      document_type: result.validated_proposal.document_type,
      subject: result.validated_proposal.filename_subject,
      parties: result.validated_proposal.parties,
      description: result.validated_proposal.description,
    };
    for (const field of acceptedFields) {
      const correct = field === 'description'
        ? fixture.acceptable_description_facts.length > 0
          && fixture.acceptable_description_facts.every((fact) => actualFields.description.toLocaleLowerCase('en-US').includes(fact.toLocaleLowerCase('en-US')))
        : sameValue(actualFields[field], fixture[field]);
      requireValue(result.field_results[field] === correct, `${variant} field result ${field} is not derived from proposal evidence for ${fixture.file}`);
      metrics[variant].total += 1;
      if (correct) metrics[variant].correct += 1;
    }

    const unsupported = [];
    if (result.proposal.document_date !== null && result.proposal.document_date !== fixture.document_date) unsupported.push({ field: 'document_date', value: result.proposal.document_date });
    const supportedParties = new Set(fixture.parties ?? []);
    for (const party of result.proposal.parties) {
      if (!supportedParties.has(party)) unsupported.push({ field: 'parties', value: party });
    }
    requireValue(JSON.stringify(result.unsupported_facts) === JSON.stringify(unsupported), `${variant} unsupported facts are not derived from proposal evidence for ${fixture.file}`);
    metrics[variant].valid += 1;
    if (result.readiness === 'ready') {
      metrics[variant].unsupportedReadyDates += unsupported.filter((fact) => fact.field === 'document_date').length;
      metrics[variant].unsupportedReadyParties += unsupported.filter((fact) => fact.field === 'parties').length;
    }
  }
}

const q4Accuracy = metrics.q4.correct / metrics.q4.total;
const q8Accuracy = metrics.q8.correct / metrics.q8.total;
const derivedSummary = {
  eligible_fixtures: eligibleFixtures,
  q4_response_validity: metrics.q4.valid / eligibleFixtures,
  q8_response_validity: metrics.q8.valid / eligibleFixtures,
  q4_field_accuracy: q4Accuracy,
  q8_field_accuracy: q8Accuracy,
  q4_unsupported_ready_dates: metrics.q4.unsupportedReadyDates,
  q4_unsupported_ready_parties: metrics.q4.unsupportedReadyParties,
  q8_unsupported_ready_dates: metrics.q8.unsupportedReadyDates,
  q8_unsupported_ready_parties: metrics.q8.unsupportedReadyParties,
  ambiguous_fixtures_in_review: ambiguousResultsInReview,
  ambiguous_fixtures_total: ambiguousResultsTotal,
  q4_readiness_accuracy: metrics.q4.readiness / eligibleFixtures,
  q8_readiness_accuracy: metrics.q8.readiness / eligibleFixtures,
};
requireValue(JSON.stringify(report.summary) === JSON.stringify(derivedSummary), 'summary is not derived from per-fixture evidence');

requireValue(derivedSummary.q4_response_validity === 1 && derivedSummary.q8_response_validity === 1, 'response validity must be 100% for Q4 and Q8');
requireValue(derivedSummary.q8_readiness_accuracy === 1, 'Q8 readiness must match every gold fixture');
requireValue(ambiguousResultsInReview === ambiguousResultsTotal, 'an intentionally ambiguous fixture did not route to Needs Review');
requireValue(metrics.q8.unsupportedReadyDates === 0 && metrics.q8.unsupportedReadyParties === 0, 'Q8 Ready results contain unsupported dates or parties');
const q4Qualifies = metrics.q4.unsupportedReadyDates === 0
  && metrics.q4.unsupportedReadyParties === 0
  && q4Accuracy >= q8Accuracy - 0.02
  && derivedSummary.q4_readiness_accuracy === 1;
const selectedModel = q4Qualifies ? 'q4' : 'q8';
requireValue(report.selected_model === selectedModel, `selected model must be ${selectedModel} from the derived Q4/Q8 gate`);

const embeddedManifest = JSON.parse(await readFile(resolve(repositoryRoot, 'src-tauri/resources/model-manifest.json'), 'utf8'));
const selected = exactModels[selectedModel];
const selectedFilename = selected.filename;
const selectedUrl = `https://huggingface.co/ggml-org/Qwen2.5-VL-3B-Instruct-GGUF/resolve/main/${selectedFilename}`;
requireValue(embeddedManifest.schema_version === 1 && embeddedManifest.model_id === selected.model_id && embeddedManifest.files?.length === 2, `embedded model manifest does not select accepted ${selectedModel}`);
requireValue(embeddedManifest.files[0]?.name === selectedFilename && embeddedManifest.files[0]?.url === selectedUrl && embeddedManifest.files[0]?.size === selected.size && embeddedManifest.files[0]?.sha256 === selected.model_sha256, `embedded model manifest does not contain accepted ${selectedModel} pin`);
requireValue(embeddedManifest.files[1]?.name === 'mmproj-Qwen2.5-VL-3B-Instruct-f16.gguf' && embeddedManifest.files[1]?.size === 1_338_428_128 && embeddedManifest.files[1]?.sha256 === selected.projector_sha256, 'embedded model manifest does not contain the accepted projector pin');
requireValue(report.acceptance.status === 'accepted' && report.acceptance.reasons.length === 0, 'acceptance must be derived as accepted only after every model gate passes');

process.stdout.write(`${JSON.stringify({
  status: 'completed',
  global_constraints_passed: true,
  corpus_records: expectedFiles.length,
  ...derivedSummary,
})}\n`);
