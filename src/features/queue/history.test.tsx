import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { App } from '../../App';
import { createInMemoryBridge } from '../../lib/inMemoryBridge';

describe('rename history', () => {
  const openHistory = async () => {
    fireEvent.click(await screen.findByRole('button', { name: /^Completed/ }));
    fireEvent.click(screen.getByRole('button', { name: 'History' }));
    return await screen.findByRole('dialog', { name: 'Rename history' });
  };

  it('opens the history dialog from the Completed view and lists seeded operations newest first', async () => {
    render(<App bridge={createInMemoryBridge()} />);

    const dialog = await openHistory();

    const rows = within(dialog).getAllByRole('row').slice(1); // Skip the header row.
    expect(rows).toHaveLength(5);
    expect(rows[0]).toHaveTextContent('Undone');
    expect(rows[0]).toHaveTextContent('Board Meeting Minutes - May 7, 2024.docx');
    expect(rows[1]).toHaveTextContent('Renamed');
    expect(rows[1]).toHaveTextContent('2024-05-07 Board Meeting Minutes.docx');
    // Leaves are shown; the full path stays reachable through the title attr.
    const original = within(rows[4]).getByTitle('C:\\Drop\\NDA - Acme Corp.docx');
    expect(original).toHaveTextContent('NDA - Acme Corp.docx');
    expect(within(rows[4]).getByTitle('C:\\Filed\\2024-03-01 Non-Disclosure Agreement with Acme Corp.docx')).toBeVisible();
  });

  it('offers no History button outside a nonempty Completed view', async () => {
    render(<App bridge={createInMemoryBridge()} />);

    await screen.findByRole('button', { name: 'Apply all ready' });
    expect(screen.queryByRole('button', { name: 'History' })).not.toBeInTheDocument();
  });

  it('exports the history CSV and reports how many operations were written', async () => {
    const bridge = createInMemoryBridge();
    const historyExport = vi.fn(bridge.historyExport);
    const pickHistoryExportPath = vi.fn(async () => 'C:\\Exports\\intern-history.csv');
    render(<App
      bridge={{ ...bridge, historyExport }}
      selection={{ pickFiles: async () => [], pickFolder: async () => undefined, pickExistingModelFiles: async () => undefined, resolveDrop: async () => ({}), pickHistoryExportPath }}
    />);
    const dialog = await openHistory();

    fireEvent.click(within(dialog).getByRole('button', { name: 'Export CSV…' }));

    await waitFor(() => expect(within(dialog).getByRole('status', { name: 'Export status' })).toHaveTextContent('Exported 5 operations.'));
    expect(pickHistoryExportPath).toHaveBeenCalledOnce();
    expect(historyExport).toHaveBeenCalledWith('C:\\Exports\\intern-history.csv');
  });

  it('reports nothing when the save dialog is canceled and shows export failures readably', async () => {
    const bridge = createInMemoryBridge();
    const historyExport = vi.fn(async () => { throw { code: 'HISTORY_EXPORT_FAILED', message: 'The export folder does not exist.' }; });
    let path: string | undefined;
    render(<App
      bridge={{ ...bridge, historyExport }}
      selection={{ pickFiles: async () => [], pickFolder: async () => undefined, pickExistingModelFiles: async () => undefined, resolveDrop: async () => ({}), pickHistoryExportPath: async () => path }}
    />);
    const dialog = await openHistory();
    const exportButton = within(dialog).getByRole('button', { name: 'Export CSV…' });

    fireEvent.click(exportButton);
    await waitFor(() => expect(exportButton).toBeEnabled());
    expect(historyExport).not.toHaveBeenCalled();
    expect(within(dialog).queryByRole('status', { name: 'Export status' })).not.toBeInTheDocument();

    path = 'C:\\Exports\\intern-history.csv';
    fireEvent.click(exportButton);
    expect(await within(dialog).findByRole('alert')).toHaveTextContent('The export folder does not exist.');
  });

  it('shows the empty state once the history has been cleared', async () => {
    const bridge = createInMemoryBridge();
    render(<App bridge={bridge} />);
    const dialog = await openHistory();
    fireEvent.click(within(dialog).getByRole('button', { name: 'Close' }));
    fireEvent.click(screen.getByRole('button', { name: 'Clear history' }));
    await waitFor(() => expect(screen.getByRole('status', { name: 'Action status' })).toHaveTextContent('History cleared.'));

    // Clearing removed the completed rows, so reopen from a reseeded queue but
    // the same (now empty) history.
    expect(await bridge.historyList()).toHaveLength(0);
  });

  it('closes on Escape and returns focus to the History trigger', async () => {
    render(<App bridge={createInMemoryBridge()} />);
    const dialog = await openHistory();

    fireEvent.keyDown(document, { key: 'Escape' });

    await waitFor(() => expect(screen.queryByRole('dialog', { name: 'Rename history' })).not.toBeInTheDocument());
    expect(screen.getByRole('button', { name: 'History' })).toHaveFocus();
    expect(dialog).not.toBeInTheDocument();
  });
});
