import { confidence } from '../lib/format';
import type { QueueStatus } from '../types';

/**
 * Confidence, kept exact and made comparable.
 *
 * The number is the claim and it never leaves: a person deciding whether to
 * trust a proposed filename is owed the figure, not an adjective. The track
 * beside it exists only so a column of forty rows can be scanned without
 * reading every number, and it carries the same status colour as the text so
 * it adds a second channel rather than a second meaning.
 *
 * An absent confidence stays an em dash rather than an empty track, because
 * "not measured" and "zero" are different things.
 */
export function ConfidenceMeter({ value, status, variant = 'cell' }: { value?: number; status: QueueStatus; variant?: 'cell' | 'panel' }) {
  if (value === undefined) return <>—</>;
  const percent = Math.max(0, Math.min(100, Math.round(value * 100)));
  return <span className={`confidence-meter confidence-meter--${variant}`}>
    <span className="confidence-value">{confidence(value)}</span>
    <span className="meter" aria-hidden="true"><span className="meter-fill" style={{ width: `${percent}%` }} /></span>
  </span>;
}
