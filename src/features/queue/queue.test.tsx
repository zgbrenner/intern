import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { App } from '../../App';
import { createFixtureBatchBridge, createInMemoryBridge } from '../../lib/inMemoryBridge';

describe('queue interactions', () => {
  const selectRow = (row: HTMLElement) => fireEvent.click(within(row).getByRole('button', { name: /select/i }));
  it('keeps identical bytes from different paths separate and deduplicates only the same path', async () => {
    const bridge = createFixtureBatchBridge();

    await bridge.addFiles([
      { path: 'browser://duplicate-invoice-a.pdf', displayName: 'duplicate-invoice-a.pdf' },
      { path: 'browser://duplicate-invoice-b.pdf', displayName: 'duplicate-invoice-b.pdf' },
      { path: 'browser://unsupported.csv', displayName: 'unsupported.csv' },
      { path: 'browser://~$nda.docx', displayName: '~$nda.docx' },
    ]);

    const items = await bridge.listItems();
    expect(items.find((item) => item.originalFilename === 'duplicate-invoice-a.pdf')).toMatchObject({ status: 'review', proposedFilename: '2025-04-30 - Invoice - INV-2048.pdf' });
    expect(items.find((item) => item.originalFilename === 'duplicate-invoice-b.pdf')).toMatchObject({ status: 'review', reason: expect.stringMatching(/different path.*separate/i) });
    expect(items.find((item) => item.originalFilename === 'duplicate-invoice-b.pdf')?.id).not.toBe(items.find((item) => item.originalFilename === 'duplicate-invoice-a.pdf')?.id);
    expect(items.find((item) => item.originalFilename === 'unsupported.csv')).toMatchObject({ status: 'failed', reason: expect.stringMatching(/unsupported.*skipped/i) });
    expect(items.find((item) => item.originalFilename === '~$nda.docx')).toMatchObject({ status: 'failed', reason: expect.stringMatching(/lock file.*skipped/i) });

    await bridge.addFiles([{ path: 'browser://duplicate-invoice-a.pdf', displayName: 'duplicate-invoice-a.pdf' }]);
    expect(await bridge.listItems()).toHaveLength(4);
  });

  it('focuses the existing result when the same unchanged path is dropped again', async () => {
    const bridge = createFixtureBatchBridge();
    const file = { path: 'browser://duplicate-invoice-a.pdf', displayName: 'duplicate-invoice-a.pdf' };
    const resolveDrop = vi.fn(async () => ({ files: [file] }));
    render(<App bridge={bridge} selection={{ pickFiles: async () => [], pickFolder: async () => undefined, resolveDrop }} />);
    const zone = await screen.findByRole('region', { name: /drag files/i });

    fireEvent.drop(zone);
    expect(await screen.findByRole('complementary', { name: 'Review item' })).toHaveTextContent('duplicate-invoice-a.pdf');
    fireEvent.click(screen.getByRole('button', { name: 'Close review' }));
    fireEvent.drop(zone);

    expect(await screen.findByRole('complementary', { name: 'Review item' })).toHaveTextContent('duplicate-invoice-a.pdf');
    expect(screen.getAllByRole('button', { name: 'Select duplicate-invoice-a.pdf' })).toHaveLength(1);
  });
  it('shows em dashes for a waiting row proposal and confidence', async () => {
    const bridge = createInMemoryBridge({
      items: [{ id: 'waiting', originalFilename: 'Invoice INV-1001.pdf', status: 'waiting' }],
    });

    render(<App bridge={bridge} />);

    const row = await screen.findByRole('row', { name: /Invoice INV-1001.pdf/i });
    expect(within(row).getAllByText('—')).toHaveLength(2);
  });

  it('opens the review inspector after selecting a review item', async () => {
    const bridge = createInMemoryBridge();
    render(<App bridge={bridge} />);

    selectRow(await screen.findByRole('row', { name: /Lease Agreement - 123 Main St.pdf/i }));

    expect(screen.getByRole('complementary', { name: 'Review item' })).toBeVisible();
    expect(screen.getByLabelText('Filename')).toHaveValue('Lease Agreement - 123 Main St - 2023-09-15.pdf');
  });

  it('filters the table from Queue to Completed navigation', async () => {
    const bridge = createInMemoryBridge();
    render(<App bridge={bridge} />);

    fireEvent.click(await screen.findByRole('button', { name: /^Completed/ }));

    expect(screen.getByRole('row', { name: /Completed lease.pdf/i })).toBeVisible();
    expect(screen.queryByRole('row', { name: /Lease Agreement - 123 Main St.pdf/i })).not.toBeInTheDocument();
  });

  it('passes exact serializable path selections from the injected picker to the bridge', async () => {
    const baseBridge = createInMemoryBridge({ items: [] });
    const addFiles = vi.fn(baseBridge.addFiles);
    const bridge = { ...baseBridge, addFiles };
    const first = { path: 'C:/Inbox/Alpha.pdf', displayName: 'Alpha.pdf' };
    const second = { path: 'C:/Inbox/Beta.txt', displayName: 'Beta.txt' };
    const pickFiles = vi.fn(async () => [first, second]);
    render(<App bridge={bridge} selection={{ pickFiles, pickFolder: async () => undefined, resolveDrop: async () => ({}) }} />);

    fireEvent.click(await screen.findByRole('button', { name: 'Add files' }));

    await waitFor(() => expect(pickFiles).toHaveBeenCalledOnce());
    await waitFor(() => expect(addFiles).toHaveBeenCalledWith([first, second]));
    expect(await screen.findByRole('row', { name: /Alpha.pdf/i })).toBeVisible();
    expect(screen.getByRole('row', { name: /Beta.txt/i })).toBeVisible();
  });

  it('preserves folder identity for a dropped directory instead of fabricating a PDF row', async () => {
    const baseBridge = createInMemoryBridge({ items: [] });
    const addFolder = vi.fn(baseBridge.addFolder);
    const bridge = { ...baseBridge, addFolder };
    const folder = { path: 'C:/Inbox/Contracts', displayName: 'Contracts', files: [] };
    const resolveDrop = vi.fn(async () => ({ folder }));
    render(<App bridge={bridge} selection={{ pickFiles: async () => [], pickFolder: async () => undefined, resolveDrop }} />);

    fireEvent.drop(await screen.findByLabelText('Drag files or folders here to add to the queue'), {
      dataTransfer: { files: [], items: [{ kind: 'file', getAsFileSystemHandle: async () => ({ kind: 'directory', name: 'Contracts' }) }] },
    });

    await waitFor(() => expect(resolveDrop).toHaveBeenCalledOnce());
    await waitFor(() => expect(addFolder).toHaveBeenCalledWith(folder));
    expect(await screen.findByRole('row', { name: /Contracts\//i })).toBeVisible();
    expect(screen.queryByRole('row', { name: /Contracts folder.pdf/i })).not.toBeInTheDocument();
  });

  it('uses the refreshed selected item rather than stale inspector data', async () => {
    const bridge = createInMemoryBridge();
    render(<App bridge={bridge} />);
    selectRow(await screen.findByRole('row', { name: /Lease Agreement - 123 Main St.pdf/i }));
    await bridge.keepOriginal('lease');
    fireEvent.click(screen.getByRole('button', { name: 'Pause queue' }));

    expect(await screen.findByRole('button', { name: 'Undo' })).toBeVisible();
    expect(screen.queryByRole('button', { name: /Approve & rename/i })).not.toBeInTheDocument();
  });
});
