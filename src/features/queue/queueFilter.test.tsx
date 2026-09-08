import { fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { App } from '../../App';
import { createInMemoryBridge } from '../../lib/inMemoryBridge';
import type { QueueItem } from '../../types';

describe('queue filter', () => {
  it('narrows the view by original name, proposed name, or description, and Escape clears it', async () => {
    render(<App bridge={createInMemoryBridge()} />);
    await screen.findByRole('row', { name: /Lease Agreement - 123 Main St.pdf/i });
    expect(screen.getByText('8 items')).toBeVisible();
    const filter = screen.getByRole('searchbox', { name: 'Filter queue' });

    fireEvent.change(filter, { target: { value: '  LEASE ' } });
    expect(screen.getByRole('row', { name: /Lease Agreement - 123 Main St.pdf/i })).toBeVisible();
    expect(screen.queryByRole('row', { name: /Invoice INV-1001.pdf/i })).not.toBeInTheDocument();
    expect(screen.getByText('1 of 8 items')).toBeVisible();

    // A word from the description, which is not in either filename.
    fireEvent.change(filter, { target: { value: 'landlord' } });
    expect(screen.getByRole('row', { name: /Lease Agreement - 123 Main St.pdf/i })).toBeVisible();
    expect(screen.getByText('1 of 8 items')).toBeVisible();

    // A word from a proposed name only.
    fireEvent.change(filter, { target: { value: 'non-disclosure' } });
    expect(screen.getByRole('row', { name: /NDA - Acme Corp.docx/i })).toBeVisible();

    fireEvent.change(filter, { target: { value: 'nothing like this' } });
    expect(screen.getByRole('status', { name: '' })).toHaveTextContent('No items match “nothing like this”.');
    expect(screen.getByText('0 of 8 items')).toBeVisible();

    fireEvent.keyDown(filter, { key: 'Escape' });
    expect(filter).toHaveValue('');
    expect(screen.getByText('8 items')).toBeVisible();
    expect(screen.getByRole('row', { name: /Invoice INV-1001.pdf/i })).toBeVisible();
  });

  it('stays out of the way of a short queue', async () => {
    const items: QueueItem[] = [
      { id: 'a', originalFilename: 'a.pdf', status: 'ready', proposedFilename: '2024-01-01 A.pdf', confidence: 0.9 },
      { id: 'b', originalFilename: 'b.pdf', status: 'waiting' },
    ];
    render(<App bridge={createInMemoryBridge({ items })} />);
    await screen.findByRole('row', { name: /a.pdf/i });

    expect(screen.queryByRole('searchbox', { name: 'Filter queue' })).not.toBeInTheDocument();
    expect(screen.getByText('2 items')).toBeVisible();
  });
});
