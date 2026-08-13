import { execFile } from 'node:child_process';
import { mkdtemp, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { promisify } from 'node:util';
import { describe, expect, it } from 'vitest';
import { GATES, validateEvaluation } from './validate-model-evaluation.mjs';

const exec = promisify(execFile);

function metric(rate: number, correct = Math.round(rate * 10), total = 10) {
  return { correct, total, rate };
}

function report(overrides: Record<string, unknown> = {}, summaryOverrides: Record<string, unknown> = {}) {
  return {
    schema_version: 2,
    pipeline: 'new',
    model_id: 'intern-local',
    budget_characters: 12_000,
    summary: {
      evaluated: 12,
      review_rate: 0.25,
      date_correct: metric(1),
      type_correct: metric(1),
      parties_correct: metric(0.7, 7),
      description_specific: metric(1),
      date_forbidden: { correct: 0, total: 12, rate: 0 },
      party_forbidden: { correct: 0, total: 12, rate: 0 },
      inference_millis: { count: 12, min: 900, median: 4_100, max: 21_000, mean: 6_000 },
      total_millis: { count: 12, min: 950, median: 4_300, max: 22_000, mean: 6_400 },
      ...summaryOverrides,
    },
    records: [
      { file: 'vendor-invoice.pdf', status: 'completed', filename: '2026-01-05 Invoice from Acme Corporation.pdf' },
      { file: 'encrypted.pdf', status: 'expected_error' },
    ],
    ...overrides,
  };
}

describe('model evaluation gate', () => {
  it('accepts a run that files documents correctly', () => {
    const result = validateEvaluation(report());
    expect(result.accepted).toBe(true);
    expect(result.failures).toEqual([]);
  });

  it('rejects a run where a document it chose to name carried the wrong date', () => {
    const result = validateEvaluation(
      report({
        records: [
          { file: 'statement-of-work.pdf', status: 'completed', filename: '2026-04-01 Statement of Work.pdf', scores: { ready: true, date_correct: false } },
          { file: 'termination-notice.pdf', status: 'completed', filename: '2026-12-29 Notice of Termination.pdf', scores: { ready: true, date_correct: true } },
        ],
      }),
    );
    expect(result.accepted).toBe(false);
    expect(result.failures.join(' ')).toContain('statement-of-work.pdf');
  });

  it('does not hold a document it sent to review to the named-date guarantee', () => {
    // A scan whose digits OCR corrupts cannot produce a literal date. Sending it
    // to review is the correct outcome, not a gate failure.
    const result = validateEvaluation(
      report({
        records: [
          { file: 'scanned-lease.pdf', status: 'completed', filename: 'Lease Agreement with Orion Glass Studio Inc.pdf', scores: { ready: false, date_correct: false } },
          { file: 'statement-of-work.pdf', status: 'completed', filename: '2026-04-01 Statement of Work.pdf', scores: { ready: true, date_correct: true } },
        ],
      }),
    );
    expect(result.accepted).toBe(true);
    expect(result.named).toEqual({ rate: 1, named: 1, wrong: [] });
  });

  it('rejects a run that picks even one date the corpus marks as a trap', () => {
    const result = validateEvaluation(report({}, { date_forbidden: { correct: 1, total: 12, rate: 1 / 12 } }));
    expect(result.accepted).toBe(false);
    expect(result.failures.join(' ')).toContain('date the corpus marks as wrong');
  });

  it('rejects a run that names too many parties the corpus marks as not defining', () => {
    const result = validateEvaluation(report({}, { party_forbidden: { correct: 5, total: 12, rate: 5 / 12 } }));
    expect(result.accepted).toBe(false);
    expect(result.failures.join(' ')).toContain('not defining');
  });

  it('accepts the party misses the recorded run actually has', () => {
    const result = validateEvaluation(report({}, { party_forbidden: { correct: 2, total: 12, rate: 2 / 12 } }));
    expect(result.accepted).toBe(true);
  });

  it('rejects a run whose date accuracy falls under the gate', () => {
    const result = validateEvaluation(report({}, { date_correct: metric(0.5, 5) }));
    expect(result.accepted).toBe(false);
    expect(result.failures.join(' ')).toContain('date_correct');
  });

  it('rejects a run that sends most documents to review', () => {
    const result = validateEvaluation(report({}, { review_rate: 0.8 }));
    expect(result.accepted).toBe(false);
    expect(result.failures.join(' ')).toContain('review rate');
  });

  // The ceiling is the only non-correctness gate here, and it blocked a release
  // once for the pipeline behaving correctly: on the release runner the model
  // claimed a fee the document could not support, evidence validation refused
  // it, that document went to review, and the rate moved from 50% to 55.6%.
  // llama.cpp is not bit-identical across machines, so one document of drift on
  // an 18-document corpus is 5.6 points. Both ends are pinned here so the
  // ceiling cannot return to zero tolerance, or drift upward to where it would
  // stop catching a real collapse.
  it('tolerates one document of machine drift but not a collapse into review', () => {
    const drifted = validateEvaluation(report({}, { review_rate: 10 / 18 }));
    expect(drifted.accepted).toBe(true);

    const collapsed = validateEvaluation(report({}, { review_rate: 12 / 18 }));
    expect(collapsed.accepted).toBe(false);
    expect(collapsed.failures.join(' ')).toContain('review rate');
  });

  it('keeps every correctness gate where it was when the review ceiling moved', () => {
    // Raising a coverage ceiling must never become a way to relax what protects
    // a filename. Changing one of these is a decision to make on purpose.
    expect(GATES.dateCorrectWhenNamed).toBe(1);
    expect(GATES.maximumForbiddenDateRate).toBe(0);
    expect(GATES.dateCorrect).toBe(0.75);
    expect(GATES.typeCorrect).toBe(0.7);
    expect(GATES.partiesCorrect).toBe(0.6);
    expect(GATES.descriptionSpecific).toBe(0.9);
  });

  it('refuses evidence produced by the superseded pipeline', () => {
    expect(() => validateEvaluation(report({ pipeline: 'legacy' }))).toThrow(/shipping pipeline/);
  });

  it('refuses a proposal that is a path rather than a filename', () => {
    expect(() =>
      validateEvaluation(
        report({
          records: [{ file: 'a.pdf', status: 'completed', filename: 'sub\\dir\\a.pdf' }],
        }),
      ),
    ).toThrow(/path, not a filename/);
  });

  it('refuses a corpus too small to mean anything', () => {
    expect(() => validateEvaluation(report({}, { evaluated: GATES.minimumEvaluated - 1 }))).toThrow(/at least/);
  });

  it('exits non-zero from the command line when the gate fails', async () => {
    const directory = await mkdtemp(join(tmpdir(), 'intern-evaluation-'));
    const path = join(directory, 'report.json');
    await writeFile(path, JSON.stringify(report({}, { review_rate: 0.9 })));
    await expect(exec(process.execPath, ['scripts/validate-model-evaluation.mjs', path])).rejects.toThrow();
  });
});
