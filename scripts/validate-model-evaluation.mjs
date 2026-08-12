import { readFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';

/**
 * Gates a corpus evaluation report produced by `intern-evaluate`.
 *
 * The bar is set on what a user actually sees: did the filename carry the date
 * the document is defined by, did it name the document specifically, did it
 * name the right parties, and did it stay off the dates and names the corpus
 * marks as traps. Latency and peak memory are recorded but never gate, because
 * they belong to the machine that ran the evaluation.
 */
/**
 * These are regression floors, not targets. Each sits at or just below what the
 * recorded run in `docs/qa/model-evaluation.json` actually achieved, so the gate
 * fails when the system gets worse rather than decorating it with a number it
 * has never met. Raise a floor when a change raises the measurement.
 *
 * `maximumForbiddenDateRate` is the sharp one: filing a document under a date
 * the corpus marks as wrong is the failure this product exists to avoid, and the
 * measured rate is zero.
 */
export const GATES = Object.freeze({
  minimumEvaluated: 8,
  dateCorrect: 0.9,
  typeCorrect: 0.7,
  partiesCorrect: 0.6,
  descriptionSpecific: 0.9,
  maximumForbiddenDateRate: 0,
  maximumForbiddenPartyRate: 0.25,
  maximumReviewRate: 0.5,
});

function requireValue(condition, message) {
  if (!condition) throw new Error(message);
}

function rate(summary, key) {
  const entry = summary[key];
  requireValue(entry && typeof entry.rate === 'number', `model evaluation summary is missing ${key}`);
  return entry.rate;
}

function correct(summary, key) {
  return summary[key] ? summary[key].correct : 0;
}

export function validateEvaluation(report) {
  requireValue(report && typeof report === 'object', 'model evaluation report must be an object');
  requireValue(report.schema_version === 2, 'model evaluation schema_version must be 2');
  requireValue(report.pipeline === 'new', 'the release gate only accepts the shipping pipeline');
  requireValue(typeof report.model_id === 'string' && report.model_id.length > 0, 'model evaluation must record the served model id');
  requireValue(Array.isArray(report.records) && report.records.length > 0, 'model evaluation must contain records');

  const summary = report.summary;
  requireValue(summary && typeof summary === 'object', 'model evaluation must contain a summary');
  requireValue(
    summary.evaluated >= GATES.minimumEvaluated,
    `model evaluation must score at least ${GATES.minimumEvaluated} documents, scored ${summary.evaluated}`,
  );

  for (const record of report.records) {
    requireValue(typeof record.file === 'string' && record.file.length > 0, 'every record must name its fixture');
    requireValue(typeof record.status === 'string', `record for ${record.file} has no status`);
    if (record.status !== 'completed') continue;
    requireValue(typeof record.filename === 'string' && record.filename.length > 0, `record for ${record.file} has no proposed filename`);
    requireValue(!record.filename.includes('/') && !record.filename.includes('\\'), `record for ${record.file} proposed a path, not a filename`);
  }

  const failures = [];
  const check = (key, minimum) => {
    const value = rate(summary, key);
    if (value < minimum) failures.push(`${key} ${(value * 100).toFixed(1)}% is below the ${(minimum * 100).toFixed(0)}% gate`);
  };
  check('date_correct', GATES.dateCorrect);
  check('type_correct', GATES.typeCorrect);
  check('parties_correct', GATES.partiesCorrect);
  check('description_specific', GATES.descriptionSpecific);

  if (rate(summary, 'date_forbidden') > GATES.maximumForbiddenDateRate) {
    failures.push(`${correct(summary, 'date_forbidden')} documents were filed under a date the corpus marks as wrong`);
  }
  if (rate(summary, 'party_forbidden') > GATES.maximumForbiddenPartyRate) {
    failures.push(`${correct(summary, 'party_forbidden')} documents named a party the corpus marks as not defining`);
  }
  if (summary.review_rate > GATES.maximumReviewRate) {
    failures.push(`review rate ${(summary.review_rate * 100).toFixed(1)}% exceeds the ${(GATES.maximumReviewRate * 100).toFixed(0)}% ceiling`);
  }

  return { accepted: failures.length === 0, failures, summary };
}

async function runCli() {
  const [path] = process.argv.slice(2);
  if (!path) throw new Error('usage: validate-model-evaluation.mjs <report.json>');
  const report = JSON.parse(await readFile(path, 'utf8'));
  const result = validateEvaluation(report);
  process.stdout.write(`${JSON.stringify({
    status: result.accepted ? 'accepted' : 'blocked',
    failures: result.failures,
    evaluated: result.summary.evaluated,
  })}\n`);
  if (!result.accepted) process.exit(1);
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) await runCli();
