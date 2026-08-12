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
