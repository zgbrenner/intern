import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { App } from '../../App';
import { createInMemoryBridge } from '../../lib/inMemoryBridge';
import type { QueueItem } from '../../types';

const selectRow = (row: HTMLElement) => fireEvent.click(within(row).getByRole('button', { name: /select/i }));

const undated: QueueItem = {
  id: 'resolution',
  originalFilename: 'Scan 0042.pdf',
  status: 'review',
  proposedFilename: 'Board Resolution - Vistage Worldwide, Inc.pdf',
  confidence: 0.81,
  description: 'Board resolution of Vistage Worldwide, Inc. approving the 2024 member-map engagement.',
  evidence: { type: 'BOARD RESOLUTION', parties: 'Vistage Worldwide, Inc.' },
  reason: 'The proposed date does not appear verbatim in the document.',
  suggestedDate: '2024-06-18',
};

describe('the date gate', () => {
  it('refuses to approve a name without a leading date and says what is needed', async () => {
    const baseBridge = createInMemoryBridge();
    const approve = vi.fn(baseBridge.approve);
    render(<App bridge={{ ...baseBridge, approve }} />);
    selectRow(await screen.findByRole('row', { name: /Lease Agreement - 123 Main St.pdf/i }));
    const filename = screen.getByLabelText('Filename');
    fireEvent.change(filename, { target: { value: 'Lease Agreement between ABC Properties LLC and TenantCo Inc.pdf' } });
    // The requirement is shown before anyone clicks.
    expect(screen.getByText(/Every rename needs a date/)).toBeVisible();

    fireEvent.click(screen.getByRole('button', { name: /Approve & rename/i }));

    expect(screen.getByRole('alert')).toHaveTextContent('Start the filename with the document\'s date as YYYY-MM-DD');
    expect(approve).not.toHaveBeenCalled();

    fireEvent.change(filename, { target: { value: '2023-09-15 Lease Agreement between ABC Properties LLC and TenantCo Inc.pdf' } });
    fireEvent.click(screen.getByRole('button', { name: /Approve & rename/i }));
    await waitFor(() => expect(approve).toHaveBeenCalledWith('lease', '2023-09-15 Lease Agreement between ABC Properties LLC and TenantCo Inc.pdf', expect.any(String)));
  });

  it('offers the date the model read but could not verify, and renames with it in one click', async () => {
    const baseBridge = createInMemoryBridge({ items: [undated] });
    const approve = vi.fn(baseBridge.approve);
    render(<App bridge={{ ...baseBridge, approve }} />);
    selectRow(await screen.findByRole('row', { name: /Scan 0042.pdf/i }));

    const suggestion = screen.getByRole('group', { name: 'Suggested date' });
    expect(suggestion).toHaveTextContent('2024-06-18');
    expect(suggestion).toHaveTextContent(/could not find it written in the document/);

    fireEvent.click(within(suggestion).getByRole('button', { name: 'Use date & rename' }));

    await waitFor(() => expect(approve).toHaveBeenCalledWith('resolution', '2024-06-18 Board Resolution - Vistage Worldwide, Inc.pdf', undated.description));
    // The rename went through: the item is settled and the suggestion is gone.
    await waitFor(() => expect(screen.queryByRole('group', { name: 'Suggested date' })).not.toBeInTheDocument());
  });

  it('can put the suggested date into the field to edit before renaming', async () => {
    render(<App bridge={createInMemoryBridge({ items: [undated] })} />);
    selectRow(await screen.findByRole('row', { name: /Scan 0042.pdf/i }));

    fireEvent.click(screen.getByRole('button', { name: 'Use this date' }));

    expect(screen.getByLabelText('Filename')).toHaveValue('2024-06-18 Board Resolution - Vistage Worldwide, Inc.pdf');
    // Once the name carries a date the offer has nothing left to add.
    expect(screen.queryByRole('group', { name: 'Suggested date' })).not.toBeInTheDocument();
    expect(screen.queryByText(/Every rename needs a date/)).not.toBeInTheDocument();
  });
});
