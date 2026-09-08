import { describe, expect, it } from 'vitest';
import { leadingDate, withLeadingDate } from './filenames';

describe('leadingDate', () => {
  it('finds a real date standing at the start of the name', () => {
    expect(leadingDate('2026-03-02 Invoice from Acme.pdf')).toBe('2026-03-02');
    expect(leadingDate('  2026-03-02.pdf')).toBe('2026-03-02');
    expect(leadingDate('2026-03-02-invoice.pdf')).toBe('2026-03-02');
  });

  it('refuses a missing, buried, run-on, or impossible date', () => {
    expect(leadingDate('Invoice from Acme.pdf')).toBeUndefined();
    expect(leadingDate('Invoice 2026-03-02 from Acme.pdf')).toBeUndefined();
    expect(leadingDate('2026-03-021 Invoice.pdf')).toBeUndefined();
    expect(leadingDate('2026-02-30 Invoice.pdf')).toBeUndefined();
    expect(leadingDate('')).toBeUndefined();
  });
});

describe('withLeadingDate', () => {
  it('prepends a date, replacing one already there', () => {
    expect(withLeadingDate('Invoice from Acme.pdf', '2026-03-02')).toBe('2026-03-02 Invoice from Acme.pdf');
    expect(withLeadingDate('2025-01-01 Invoice from Acme.pdf', '2026-03-02')).toBe('2026-03-02 Invoice from Acme.pdf');
    expect(withLeadingDate('2025-01-01 - Invoice.pdf', '2026-03-02')).toBe('2026-03-02 Invoice.pdf');
    expect(withLeadingDate('   ', '2026-03-02')).toBe('2026-03-02');
  });
});
