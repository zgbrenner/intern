import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { App } from '../../App';
import { createFixtureBatchBridge, createInMemoryBridge } from '../../lib/inMemoryBridge';

describe('queue interactions', () => {
  const selectRow = async (row: HTMLElement) => {
    const select = within(row).getByRole('button', { name: /select/i });
    const filename = select.getAttribute('aria-label')?.replace(/^Select /, '');
    fireEvent.click(select);
    const inspector = await screen.findByRole('complementary', { name: 'Review item' });
    if (filename) await waitFor(() => expect(inspector).toHaveTextContent(filename));
  };
  it('keeps identical bytes from different paths separate and deduplicates only the same path', async () => {
    const bridge = createFixtureBatchBridge();

    await bridge.addFiles([
      { path: 'browser://duplicate-invoice-a.pdf', displayName: 'duplicate-invoice-a.pdf' },
      { path: 'browser://duplicate-invoice-b.pdf', displayName: 'duplicate-invoice-b.pdf' },
      { path: 'browser://unsupported.csv', displayName: 'unsupported.csv' },
      { path: 'browser://~$nda.docx', displayName: '~$nda.docx' },
    ]);

    const items = await bridge.listItems();
    expect(items.find((item) => item.originalFilename === 'duplicate-invoice-a.pdf')).toMatchObject({ status: 'review', proposedFilename: '2025-04-30 Invoice from Nimbus Orchard Supply Co.pdf' });
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
    render(<App bridge={bridge} selection={{ pickFiles: async () => [], pickFolder: async () => undefined, pickExistingModelFiles: async () => undefined, resolveDrop }} />);
    const zone = await screen.findByRole('region', { name: /drag files/i });

    fireEvent.drop(zone);
    expect(await screen.findByRole('complementary', { name: 'Review item' })).toHaveTextContent('duplicate-invoice-a.pdf');
    fireEvent.click(screen.getByRole('button', { name: 'Close review' }));
    fireEvent.drop(zone);

    expect(await screen.findByRole('complementary', { name: 'Review item' })).toHaveTextContent('duplicate-invoice-a.pdf');
    expect(screen.getAllByRole('button', { name: 'Select duplicate-invoice-a.pdf' })).toHaveLength(1);
  });

  it('describes supported formats without adding a nonfunctional keyboard stop', async () => {
    render(<App bridge={createInMemoryBridge()} />);

    const zone = await screen.findByRole('region', { name: /drag files/i });
    expect(zone).toHaveTextContent('Supports PDF, DOCX, TXT, Markdown, PNG, JPEG (JPG), and TIFF');
    expect(zone).not.toHaveAttribute('tabindex');
  });

  it('uses restrained extension-aware document icons', async () => {
    render(<App bridge={createInMemoryBridge()} />);

    expect((await screen.findByRole('row', { name: /Employment Agreement/i })).querySelector('.file-kind--pdf')).toBeInTheDocument();
    expect(screen.getByRole('row', { name: /NDA - Acme Corp/i }).querySelector('.file-kind--document')).toBeInTheDocument();
    expect(screen.getByRole('row', { name: /Q1 Financials/i }).querySelector('.file-kind--pdf')).toBeInTheDocument();
    expect(screen.getByRole('row', { name: /Notes from Call/i }).querySelector('.file-kind--text')).toBeInTheDocument();
    // Every demo row is a format the queue accepts. The spreadsheet icon still
    // exists for a file a user drags in by mistake, but the demo no longer shows
    // an unsupported type sailing through the pipeline.
    expect(screen.queryByRole('row', { name: /\.xlsx/i })).not.toBeInTheDocument();
  });

  it('announces useful queue state changes', async () => {
    render(<App bridge={createInMemoryBridge()} />);

    const status = await screen.findByRole('status', { name: 'Queue status' });
    expect(status).toHaveTextContent('Queue active. 1 processing, 3 ready, 1 needs review, 3 waiting, 1 completed.');
    fireEvent.click(screen.getByRole('button', { name: 'Pause queue' }));

    await waitFor(() => expect(status).toHaveTextContent('Queue paused. 3 ready, 1 needs review, 4 waiting, 1 completed.'));
  });

  it('applies one ready rename from its contextual inspector action', async () => {
    const base = createInMemoryBridge();
    const approve = vi.fn(base.approve);
    render(<App bridge={{ ...base, approve }} />);

    await selectRow(await screen.findByRole('row', { name: /Employment Agreement/i }));
    fireEvent.click(screen.getByRole('button', { name: 'Apply rename' }));

    await waitFor(() => expect(approve).toHaveBeenCalledWith(
      'employment',
      '2024-04-12 Employment Agreement with John Smith.pdf',
      '',
    ));
  });

  it('applies all ready proposals in one deliberate batch action', async () => {
    const base = createInMemoryBridge();
    const approve = vi.fn(base.approve);
    render(<App bridge={{ ...base, approve }} />);

    fireEvent.click(await screen.findByRole('button', { name: 'Apply all ready' }));

    await waitFor(() => expect(approve).toHaveBeenCalledTimes(3));
    expect(approve.mock.calls.map(([id]) => id)).toEqual(['employment', 'nda', 'service']);
  });

  it('reports partial Apply all failures while preserving the failed ready item', async () => {
    const base = createInMemoryBridge();
    const approve = vi.fn(async (id: string, filename: string, description: string) => {
      if (id === 'nda') throw new Error('Destination is locked.');
      await base.approve(id, filename, description);
    });
    render(<App bridge={{ ...base, approve }} />);

    fireEvent.click(await screen.findByRole('button', { name: 'Apply all ready' }));

    expect(await screen.findByRole('status', { name: 'Action error' })).toHaveTextContent('2 renames applied. 1 could not be applied. Destination is locked.');
    expect(screen.getByRole('row', { name: /NDA - Acme Corp/i })).toHaveTextContent('Ready');
    expect(approve).toHaveBeenCalledTimes(3);
  });

  it('shows a command failure and keeps the selected item available to retry', async () => {
    const base = createInMemoryBridge();
    const approve = vi.fn(async () => { throw new Error('Destination is unavailable.'); });
    render(<App bridge={{ ...base, approve }} />);

    await selectRow(await screen.findByRole('row', { name: /Employment Agreement/i }));
    fireEvent.click(screen.getByRole('button', { name: 'Apply rename' }));

    expect(await screen.findByRole('status', { name: 'Action error' })).toHaveTextContent('Destination is unavailable.');
    expect(screen.getByRole('button', { name: 'Apply rename' })).toBeEnabled();
    expect(screen.getByRole('complementary', { name: 'Review item' })).toBeVisible();
  });

  it('disables contextual actions while a bridge command is pending', async () => {
    let finish: (() => void) | undefined;
    const base = createInMemoryBridge();
    const approve = vi.fn(() => new Promise<void>((resolve) => { finish = resolve; }));
    render(<App bridge={{ ...base, approve }} />);

    await selectRow(await screen.findByRole('row', { name: /Employment Agreement/i }));
    const action = screen.getByRole('button', { name: 'Apply rename' });
    fireEvent.click(action);

    expect(action).toBeDisabled();
    finish?.();
    await waitFor(() => expect(screen.queryByRole('button', { name: 'Apply rename' })).not.toBeInTheDocument());
  });

  it('does not close a newer selection when an earlier item action completes', async () => {
    let finish: (() => void) | undefined;
    const base = createInMemoryBridge();
    const approve = vi.fn(() => new Promise<void>((resolve) => { finish = resolve; }));
    render(<App bridge={{ ...base, approve }} />);

    await selectRow(await screen.findByRole('row', { name: /Employment Agreement/i }));
    fireEvent.click(screen.getByRole('button', { name: 'Apply rename' }));
    await selectRow(screen.getByRole('row', { name: /NDA - Acme Corp/i }));
    finish?.();

    await waitFor(() => expect(screen.getByLabelText('Filename')).toHaveValue('2024-03-01 Non-Disclosure Agreement with Acme Corp.docx'));
    expect(screen.getByRole('complementary', { name: 'Review item' })).toBeVisible();
  });

  it('does not close a newer selection when Apply all completes', async () => {
    let finish: (() => void) | undefined;
    const base = createInMemoryBridge();
    const approve = vi.fn(async (id: string, filename: string, description: string) => {
      if (id === 'employment') await new Promise<void>((resolve) => { finish = resolve; });
      await base.approve(id, filename, description);
    });
    render(<App bridge={{ ...base, approve }} />);

    await selectRow(await screen.findByRole('row', { name: /Employment Agreement/i }));
    fireEvent.click(screen.getByRole('button', { name: 'Apply all ready' }));
    await selectRow(screen.getByRole('row', { name: /Lease Agreement - 123 Main St/i }));
    finish?.();

    await waitFor(() => expect(screen.getByLabelText('Filename')).toHaveValue('2023-09-15 Lease Agreement between ABC Properties LLC and TenantCo Inc.pdf'));
    expect(screen.getByRole('complementary', { name: 'Review item' })).toBeVisible();
  });

  it.each([
    ['Cancel processing', 'cancel', { id: 'active', originalFilename: 'active.pdf', status: 'processing' as const }],
    ['Retry item', 'retry', { id: 'failed', originalFilename: 'failed.pdf', status: 'failed' as const }],
    ['Remove item', 'remove', { id: 'failed', originalFilename: 'failed.pdf', status: 'failed' as const }],
  ])('reports a polite visible error when %s fails', async (actionName, method, item) => {
    const command = vi.fn(async () => { throw new Error(`${actionName} failed.`); });
    const base = createInMemoryBridge({ items: [item] });
    const bridge = { ...base, [method]: command };
    render(<App bridge={bridge} />);

    await selectRow(await screen.findByRole('row', { name: new RegExp(item.originalFilename, 'i') }));
    fireEvent.click(screen.getByRole('button', { name: actionName }));

    expect(await screen.findByRole('status', { name: 'Action error' })).toHaveTextContent(`${actionName} failed.`);
    expect(screen.getByRole('button', { name: actionName })).toBeEnabled();
  });

  it('reports Clear history failures without emptying the completed view', async () => {
    const clearHistory = vi.fn(async () => { throw new Error('History is locked.'); });
    render(<App bridge={{ ...createInMemoryBridge(), clearHistory }} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Completed' }));
    fireEvent.click(screen.getByRole('button', { name: 'Clear history' }));

    expect(await screen.findByRole('status', { name: 'Action error' })).toHaveTextContent('History is locked.');
    expect(screen.getByRole('row', { name: /Completed lease.pdf/i })).toBeVisible();
    expect(screen.getByRole('button', { name: 'Clear history' })).toBeEnabled();
  });

  it('offers retry and remove only when a failed item is selected', async () => {
    const retry = vi.fn(async () => undefined);
    const remove = vi.fn(async () => undefined);
    const bridge = { ...createInMemoryBridge({ items: [{ id: 'failed', originalFilename: 'broken.pdf', status: 'failed', reason: 'Extraction failed.' }] }), retry, remove };
    render(<App bridge={bridge} />);

    await selectRow(await screen.findByRole('row', { name: /broken.pdf/i }));
    expect(screen.queryByLabelText('Filename')).not.toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'Failure details' })).toBeVisible();
    expect(screen.getByRole('button', { name: 'Retry item' })).toBeVisible();
    fireEvent.click(screen.getByRole('button', { name: 'Remove item' }));

    await waitFor(() => expect(remove).toHaveBeenCalledWith('failed'));
    expect(retry).not.toHaveBeenCalled();
  });

  it('wires cancellation for the selected active item', async () => {
    const cancel = vi.fn(async () => undefined);
    const bridge = { ...createInMemoryBridge({ items: [{ id: 'active', originalFilename: 'active.pdf', status: 'processing', progress: 25 }] }), cancel };
    render(<App bridge={bridge} />);

    await selectRow(await screen.findByRole('row', { name: /active.pdf/i }));
    expect(screen.queryByLabelText('Filename')).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel processing' }));

    await waitFor(() => expect(cancel).toHaveBeenCalledWith('active'));
  });

  it('does not offer cancellation during the atomic apply stage', async () => {
    const bridge = createInMemoryBridge({ items: [{ id: 'applying', originalFilename: 'applying.pdf', status: 'processing', progress: 90, cancelable: false }] });
    render(<App bridge={bridge} />);

    await selectRow(await screen.findByRole('row', { name: /applying.pdf/i }));

    expect(screen.queryByRole('button', { name: 'Cancel processing' })).not.toBeInTheDocument();
  });

  it('offers Clear history only within a nonempty Completed view', async () => {
    const clearHistory = vi.fn(async () => undefined);
    const bridge = { ...createInMemoryBridge(), clearHistory };
    render(<App bridge={bridge} />);

    expect(screen.queryByRole('button', { name: 'Clear history' })).not.toBeInTheDocument();
    fireEvent.click(await screen.findByRole('button', { name: /^Completed/ }));
    fireEvent.click(screen.getByRole('button', { name: 'Clear history' }));

    await waitFor(() => expect(clearHistory).toHaveBeenCalledOnce());
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

    await selectRow(await screen.findByRole('row', { name: /Lease Agreement - 123 Main St.pdf/i }));

    expect(screen.getByRole('complementary', { name: 'Review item' })).toBeVisible();
    expect(screen.getByLabelText('Filename')).toHaveValue('2023-09-15 Lease Agreement between ABC Properties LLC and TenantCo Inc.pdf');
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
    render(<App bridge={bridge} selection={{ pickFiles, pickFolder: async () => undefined, pickExistingModelFiles: async () => undefined, resolveDrop: async () => ({}) }} />);

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
    render(<App bridge={bridge} selection={{ pickFiles: async () => [], pickFolder: async () => undefined, pickExistingModelFiles: async () => undefined, resolveDrop }} />);

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
    await selectRow(await screen.findByRole('row', { name: /Lease Agreement - 123 Main St.pdf/i }));
    await bridge.keepOriginal('lease');
    fireEvent.click(screen.getByRole('button', { name: 'Pause queue' }));

    expect(await screen.findByRole('button', { name: 'Undo' })).toBeVisible();
    expect(screen.queryByRole('button', { name: /Approve & rename/i })).not.toBeInTheDocument();
  });
});
