import { fireEvent, render, screen, within } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { App } from '../../App';
import { createInMemoryBridge } from '../../lib/inMemoryBridge';
import type { QueueItem } from '../../types';

const selectRow = (row: HTMLElement) => fireEvent.click(within(row).getByRole('button', { name: /select/i }));

const rescan: QueueItem = {
  id: 'rescan',
  originalFilename: 'Scan 0051.pdf',
  status: 'review',
  proposedFilename: '2024-04-12 Employment Agreement between John Smith and Acme Corporation.pdf',
  confidence: 0.94,
  description: 'Employment agreement between John Smith and Acme Corporation covering duties, salary, and term.',
  evidence: { date: 'signed April 12, 2024', type: 'EMPLOYMENT AGREEMENT', parties: 'John Smith; Acme Corporation' },
  // The desktop bridge turns the NEAR_DUPLICATE code into this sentence; the in-memory bridge passes reasons through as given.
  reason: 'This looks like a document that was filed already. Approve to file it as well, keep the original, or remove it.',
  nearDuplicateOf: '2024-04-12 Employment Agreement between John Smith and Acme Corporation.pdf (filed from Front desk)',
};

describe('a document that repeats a filed one', () => {
  it('says so in words and names the filing it repeats', async () => {
    render(<App bridge={createInMemoryBridge({ items: [rescan] })} />);
    selectRow(await screen.findByRole('row', { name: /Scan 0051.pdf/i }));

    const reason = screen.getByRole('heading', { name: 'Reason for review' }).closest('section')!;
    expect(reason).toHaveTextContent('This looks like a document that was filed already. Approve to file it as well, keep the original, or remove it.');
    expect(within(reason).getByLabelText('Filed already as')).toHaveTextContent('2024-04-12 Employment Agreement between John Smith and Acme Corporation.pdf (filed from Front desk)');
    // The usual choices stay: it is a person's call, not Intern's.
    expect(screen.getByRole('button', { name: /Approve & rename/i })).toBeEnabled();
    expect(screen.getByRole('button', { name: /Keep original/i })).toBeEnabled();
  });
});
