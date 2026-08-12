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
 *
 * `dateCorrect` is a floor on the whole corpus, measured at 76.5%. Every one of
 * the 13 text documents gets its date right; all four misses are scans whose
 * digits OCR corrupts, and each of those is correctly sent to review instead of
 * being named. `dateCorrectWhenNamed` is the gate that expresses the actual
 * promise, and it is absolute.
 */
export const GATES = Object.freeze({
  minimumEvaluated: 8,
  dateCorrect: 0.75,
  dateCorrectWhenNamed: 1,
  typeCorrect: 0.7,
  partiesCorrect: 0.6,
  descriptionSpecific: 0.9,
  maximumForbiddenDateRate: 0,
  maximumForbiddenPartyRate: 0.25,
  maximumReviewRate: 0.5,
});

/**
 * Date accuracy among the documents Intern actually proposed a name for.
 *
 * The corpus deliberately contains scans whose text cannot yield a literal date:
 * the clean-room bitmap font corrupts digits under OCR, so `2024` arrives as
 * `24h24` and evidence validation refuses it. That is the product behaving
 * correctly - it sends the document to review rather than inventing a date - but
 * it puts a hard ceiling on corpus-wide date accuracy, so gating that number at
 * 90% would demand the impossible and tell nobody anything.
 *
 * The guarantee that matters to a user is narrower and stricter: if Intern was
 * confident enough to rename a file, the date on it must be right. That is gated
 * at 100%, with corpus-wide accuracy kept as a regression floor underneath it.
 */
function dateCorrectWhenNamed(records) {
  const named = records.filter((record) => record && record.scores && record.scores.ready === true);
  if (named.length === 0) return { rate: 1, named: 0, wrong: [] };
  const wrong = named.filter((record) => record.scores.date_correct !== true).map((record) => record.file);
  return { rate: (named.length - wrong.length) / named.length, named: named.length, wrong };
}

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

  const named = dateCorrectWhenNamed(report.records);
  if (named.rate < GATES.dateCorrectWhenNamed) {
    failures.push(
      `${named.wrong.length} of ${named.named} documents Intern named carried the wrong date: ${named.wrong.join(', ')}`,
    );
  }

  if (rate(summary, 'date_forbidden') > GATES.maximumForbiddenDateRate) {
    failures.push(`${correct(summary, 'date_forbidden')} documents were filed under a date the corpus marks as wrong`);
  }
  if (rate(summary, 'party_forbidden') > GATES.maximumForbiddenPartyRate) {
    failures.push(`${correct(summary, 'party_forbidden')} documents named a party the corpus marks as not defining`);
  }
  if (summary.review_rate > GATES.maximumReviewRate) {
    failures.push(`review rate ${(summary.review_rate * 100).toFixed(1)}% exceeds the ${(GATES.maximumReviewRate * 100).toFixed(0)}% ceiling`);
  }

  return { accepted: failures.length === 0, failures, summary, named };
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
    named: result.named.named,
    date_correct_when_named: result.named.rate,
  })}\n`);
  if (!result.accepted) process.exit(1);
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) await runCli();
