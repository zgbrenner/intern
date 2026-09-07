import { fireEvent, render, screen, within } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { App } from '../../App';
import { createInMemoryBridge } from '../../lib/inMemoryBridge';
import type { QueueItem } from '../../types';

const selectRow = (row: HTMLElement) => fireEvent.click(within(row).getByRole('button', { name: /select/i }));

const respelled: QueueItem = {
  id: 'sow',
  originalFilename: 'SOW final v3.pdf',
  status: 'ready',
  proposedFilename: '2026-04-01 Statement of Work between Ridgeline Cartography LLC and Vistage.pdf',
  confidence: 0.93,
  description: 'Statement of work between Ridgeline Cartography LLC and Vistage Worldwide, Inc. for the 2026 member-map engagement.',
  evidence: { date: 'effective as of April 1, 2026', type: 'STATEMENT OF WORK', parties: 'Ridgeline Cartography LLC; Vistage Worldwide, Inc.' },
  houseRules: [{ kind: 'party', from: 'Vistage Worldwide, Inc.', to: 'Vistage' }],
};

describe('a name in the reviewer\'s own spelling', () => {
  it('says which spelling was applied while the evidence keeps the document\'s words', async () => {
    render(<App bridge={createInMemoryBridge({ items: [respelled] })} />);
    selectRow(await screen.findByRole('row', { name: /SOW final v3.pdf/i }));

    const note = screen.getByRole('note', { name: 'Learned spellings applied' });
    expect(note).toHaveTextContent('Uses your spelling: Vistage Worldwide, Inc. written as Vistage. Change or forget it under Settings.');
    expect(screen.getByLabelText('Filename')).toHaveValue('2026-04-01 Statement of Work between Ridgeline Cartography LLC and Vistage.pdf');
    const evidence = screen.getByRole('heading', { name: 'Evidence' }).closest('section')!;
    expect(within(evidence).getByText('Vistage Worldwide, Inc.')).toBeVisible();
  });

  it('shows no note for a name in the document\'s own words', async () => {
    render(<App bridge={createInMemoryBridge()} />);
    selectRow(await screen.findByRole('row', { name: /Lease Agreement - 123 Main St.pdf/i }));
    expect(screen.queryByRole('note', { name: 'Learned spellings applied' })).not.toBeInTheDocument();
  });
});
