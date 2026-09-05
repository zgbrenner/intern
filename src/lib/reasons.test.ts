import { describe, expect, it } from 'vitest';
import { humanizeReason } from './reasons';

describe('humanizeReason', () => {
  it('translates a single code into its sentence', () => {
    expect(humanizeReason('SOURCE_LOCKED')).toContain('sync client mid-upload');
  });

  it('translates the pipeline comma-joined list into sentences', () => {
    const result = humanizeReason('TYPE_UNSUPPORTED, LOW_CONFIDENCE, MODEL_REQUESTED_REVIEW');
    expect(result).toBe(
      'The proposed document type does not appear verbatim in the document. '
      + 'The model reported low confidence in its own proposal. '
      + 'The model asked for a person to look at this one.',
    );
  });

  it('tolerates lowercase codes from other serializations', () => {
    expect(humanizeReason('date_unsupported')).toContain('verbatim');
  });

  it('explains a bare duplicate flag once the filed name it referred to is gone', () => {
    expect(humanizeReason('DUPLICATE')).toBe('This document\'s content was filed once already. Retry to process it anyway, or remove it.');
  });

  it('keeps unknown codes and free text verbatim', () => {
    expect(humanizeReason('SOMETHING_NEW')).toBe('SOMETHING_NEW');
    expect(humanizeReason('Duplicate of 2026-01-05 Invoice.pdf')).toBe('Duplicate of 2026-01-05 Invoice.pdf');
  });

  it('never rewrites a list containing an unknown entry, so free-text commas survive', () => {
    const name = 'Duplicate of 2026-04-01 SOW between Ridgeline, LLC and Vistage Worldwide, Inc.pdf';
    expect(humanizeReason(name)).toBe(name);
    expect(humanizeReason('TYPE_UNSUPPORTED, SOMETHING_NEW')).toBe('TYPE_UNSUPPORTED, SOMETHING_NEW');
  });
});
